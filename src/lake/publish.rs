//! Publishing a local DuckLake commit to an object store.
//!
//! Each attempt fetches the live catalogue and commits into a local copy of it, with
//! DuckLake reading and writing data files in place through the lake's write path.
//! It then pins an immutable copy of the new catalogue, and only then writes the
//! stable catalogue, conditionally on the ETag it fetched. A failure before that
//! last write leaves the live lake as it was (at worst, orphan data files nothing
//! references). Readable copies and extra files are uploaded only
//! once the catalogue holds this publish's snapshot, and are skipped when a newer
//! snapshot changed their table, so an older publish never overwrites a newer one's,
//! or when the object already holds their bytes, so an unchanged publish writes
//! nothing. The live catalogue is fetched again only when its ETag has moved.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::commit::{CommitOutcome, TableInput, commit};
use super::meta::{META_SCHEMA, failed};
use super::store::{ETag, ObjectHead, ObjectStore, PutCondition, StoreError};
use crate::error::{Error, Result};
use crate::operator::sql_string_literal;

/// The pinned catalogue never changes: its key is its content's hash.
const PINNED_CACHE: &str = "public, max-age=31536000, immutable";
/// The stable catalogue moves with every publish.
const STABLE_CACHE: &str = "public, max-age=60";

/// Where a lake lives in its store.
#[derive(Debug, Clone)]
pub struct Lake {
    /// The stable catalogue's key, e.g. `catalog/open.ducklake`.
    pub catalog_key: String,
    /// The catalogue's persisted data path, the address readers resolve data files
    /// against, e.g. `https://openlake.meridian.online/lake/`.
    pub data_path: String,
    /// The store key prefix matching `data_path`, e.g. `lake/`.
    pub data_prefix: String,
    /// The same objects as `data_path`, addressed so DuckDB can read and write them:
    /// `r2://<bucket>/<data_prefix>` (or `s3://`) live, a local folder otherwise. Ends in `/`.
    pub write_path: String,
    /// The pinned copies' key prefix, e.g. `catalog/open-`; `None` for a lake no
    /// reader fetches by a pinned address (the local lake), which pins none.
    pub pinned_prefix: Option<String>,
}

/// A file or directory to upload beside the step's tables.
#[derive(Debug, Clone)]
pub struct ExtraFile {
    pub from: PathBuf,
    /// The key, or for a directory a key prefix ending in `/`.
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UploadedObject {
    pub key: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct Published {
    pub outcome: CommitOutcome,
    /// The readable copies and extra files this publish uploaded. The data files
    /// DuckLake wrote are in `outcome.new_files`.
    pub uploaded: Vec<UploadedObject>,
    /// Readable copies and extra files not uploaded because the object already
    /// held their bytes.
    pub unchanged: Vec<String>,
    /// Readable copies and extra files skipped because a newer snapshot changed
    /// their table: a newer publish owns them.
    pub superseded: Vec<String>,
    /// The pinned catalogue's key; `None` when the lake pins no copies.
    pub pinned_catalog: Option<String>,
    /// The stable catalogue's ETag as this publish wrote or verified it.
    pub catalog_etag: ETag,
    pub attempts: u32,
}

fn store_failed<'a>(action: &str, key: &'a str) -> impl Fn(StoreError) -> Error + 'a {
    let action = action.to_string();
    move |e| failed(format!("{action} `{key}`: {e}"))
}

fn io_failed(path: &Path) -> impl Fn(io::Error) -> Error + '_ {
    move |e| failed(format!("{}: {e}", path.display()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A file's size and SHA-256, read in chunks.
fn describe(key: &str, path: &Path) -> Result<UploadedObject> {
    let mut file = fs::File::open(path).map_err(io_failed(path))?;
    let mut hasher = Sha256::new();
    let bytes = io::copy(&mut file, &mut hasher).map_err(io_failed(path))?;
    Ok(UploadedObject {
        key: key.to_string(),
        bytes,
        sha256: hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    })
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_failed(path)(e)),
        _ => Ok(()),
    }
}

fn wal_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".wal");
    PathBuf::from(name)
}

/// Write `bytes` (or nothing) to `path` as a catalogue file, with no stale WAL beside it.
fn place_catalogue(path: &Path, bytes: Option<&[u8]>) -> Result<()> {
    remove_if_present(path)?;
    remove_if_present(&wal_of(path))?;
    if let Some(bytes) = bytes {
        fs::write(path, bytes).map_err(io_failed(path))?;
    }
    Ok(())
}

/// A catalogue file's bytes, refusing one whose changes still sit in a WAL.
fn read_catalogue(path: &Path) -> Result<Vec<u8>> {
    if wal_of(path).exists() {
        return Err(failed(format!(
            "the catalogue {} was left with an uncheckpointed WAL",
            path.display()
        )));
    }
    fs::read(path).map_err(io_failed(path))
}

/// How pinning a catalogue went.
enum Pin {
    /// The pinned copy's key, or `None` for a lake that pins none.
    Done(Option<String>),
    /// The store throttled the write: start the attempt again.
    Throttled,
}

/// Upload `bytes`' immutable copy, if absent, and return its key.
fn pin(store: &dyn ObjectStore, lake: &Lake, bytes: &[u8]) -> Result<Pin> {
    let Some(prefix) = &lake.pinned_prefix else {
        return Ok(Pin::Done(None));
    };
    let key = format!("{prefix}{}.ducklake", &sha256_hex(bytes)[..16]);
    // Its key is its content's hash: present already, it holds these bytes, and an
    // unchanged publish sends none of them again.
    let present = store
        .etag(&key)
        .map_err(store_failed("read the version of", &key))?;
    if present.is_some() {
        return Ok(Pin::Done(Some(key)));
    }
    match store.put(&key, bytes, PutCondition::IfAbsent, Some(PINNED_CACHE)) {
        // Present already: its key is its content's hash, so it holds these bytes.
        Ok(_) | Err(StoreError::PreconditionFailed) => Ok(Pin::Done(Some(key))),
        Err(StoreError::Throttled) => Ok(Pin::Throttled),
        Err(e) => Err(store_failed("upload the pinned catalogue", &key)(e)),
    }
}

/// What an attempt that landed leaves for the uploads after it.
struct Landed {
    outcome: CommitOutcome,
    pinned: Option<String>,
    etag: ETag,
}

/// How an attempt ended.
enum Try {
    Landed(Landed),
    /// Another publish moved the catalogue first: start again.
    Lost,
    /// The store throttled a write: start again.
    Throttled,
}

/// One fetch, commit and conditional catalogue write.
fn attempt(
    store: &dyn ObjectStore,
    lake: &Lake,
    secret_sql: Option<&str>,
    work: &Path,
    tables: &[TableInput],
    commit_info: &serde_json::Value,
) -> Result<Try> {
    let key = lake.catalog_key.as_str();
    let fetched = store
        .get(key)
        .map_err(store_failed("fetch the catalogue", key))?;
    let catalog_file = work.join("catalog.ducklake");
    place_catalogue(&catalog_file, fetched.as_ref().map(|(b, _)| b.as_slice()))?;
    let outcome = commit(
        &catalog_file,
        &lake.data_path,
        &lake.write_path,
        secret_sql,
        tables,
        commit_info,
    )?;

    match fetched {
        Some((fetched_bytes, fetched_etag)) if !outcome.committed => {
            // Nothing changed. The catalogue stands as fetched, unless another
            // publish moved it meanwhile; pin it so a manifest can name it.
            let now = store
                .etag(key)
                .map_err(store_failed("read the version of", key))?;
            if now.as_ref() != Some(&fetched_etag) {
                return Ok(Try::Lost);
            }
            let Pin::Done(pinned) = pin(store, lake, &fetched_bytes)? else {
                return Ok(Try::Throttled);
            };
            Ok(Try::Landed(Landed {
                outcome,
                pinned,
                etag: fetched_etag,
            }))
        }
        fetched => {
            // Committed, or the catalogue is new. DuckLake wrote the data in place;
            // the pinned copy goes next, then the stable catalogue, conditionally
            // and last.
            let bytes = read_catalogue(&catalog_file)?;
            let Pin::Done(pinned) = pin(store, lake, &bytes)? else {
                return Ok(Try::Throttled);
            };
            let cond = match fetched {
                Some((_, etag)) => PutCondition::IfMatch(etag),
                None => PutCondition::IfAbsent,
            };
            match store.put(key, &bytes, cond, Some(STABLE_CACHE)) {
                Ok(etag) => Ok(Try::Landed(Landed {
                    outcome,
                    pinned,
                    etag,
                })),
                Err(StoreError::PreconditionFailed) => Ok(Try::Lost),
                Err(StoreError::Throttled) => Ok(Try::Throttled),
                Err(e) => Err(store_failed("write the catalogue", key)(e)),
            }
        }
    }
}

/// `schema.table` → its parts; one part is a table in `main`.
fn split_table(table: &str) -> (&str, &str) {
    table.split_once('.').unwrap_or(("main", table))
}

/// Whether a snapshot newer than `since_snapshot` in the store's current catalogue
/// changed `table`'s rows or its descriptor in `meta.descriptors`, fetching the
/// catalogue every time: the tests' reference for what [`Watch`] answers.
#[cfg(test)]
pub(crate) fn newer_change(
    store: &dyn ObjectStore,
    lake: &Lake,
    work: &Path,
    since_snapshot: i64,
    table: &str,
) -> Result<bool> {
    let key = lake.catalog_key.as_str();
    match store
        .get(key)
        .map_err(store_failed("fetch the catalogue", key))?
    {
        Some((bytes, _)) => newer_change_in(lake, work, &bytes, since_snapshot, table),
        None => Ok(false),
    }
}

/// Whether a snapshot newer than `since_snapshot` in the catalogue `bytes` changed
/// `table`'s rows or its descriptor in `meta.descriptors`. Both checks go by name,
/// so DuckLake resolves table ids itself.
fn newer_change_in(
    lake: &Lake,
    work: &Path,
    bytes: &[u8],
    since_snapshot: i64,
    table: &str,
) -> Result<bool> {
    fs::create_dir_all(work).map_err(io_failed(work))?;
    let file = work.join("newer.ducklake");
    place_catalogue(&file, Some(bytes))?;

    let context = |what: &'static str| move |e: duckdb::Error| failed(format!("{what}: {e}"));
    let conn = duckdb::Connection::open_in_memory().map_err(context("open duckdb"))?;
    let mut setup = String::from("INSTALL ducklake; LOAD ducklake;");
    if lake.data_path.starts_with("http://") || lake.data_path.starts_with("https://") {
        setup.push_str(" INSTALL httpfs; LOAD httpfs;");
    }
    setup.push_str(&format!(
        " ATTACH {} AS arc_newer (READ_ONLY);",
        sql_string_literal(&format!("ducklake:{}", file.display()))
    ));
    conn.execute_batch(&setup)
        .map_err(context("attach the fetched catalogue"))?;
    let latest: i64 = conn
        .query_row(
            "SELECT max(snapshot_id) FROM ducklake_snapshots('arc_newer')",
            [],
            |r| r.get(0),
        )
        .map_err(context("read the catalogue's snapshots"))?;
    if latest <= since_snapshot {
        return Ok(false);
    }
    let from = since_snapshot + 1;
    let (schema, name) = split_table(table);
    let exists = |schema: &str, name: &str| -> Result<bool> {
        conn.query_row(
            "SELECT count(*) FROM duckdb_tables() \
             WHERE database_name = 'arc_newer' AND lower(schema_name) = lower(?) \
             AND lower(table_name) = lower(?)",
            [schema, name],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .map_err(context("look up the table"))
    };
    if !exists(schema, name)? {
        // Absent at the newest snapshot: no newer publish owns its copy.
        return Ok(false);
    }
    let rows: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM (SELECT 1 FROM ducklake_table_changes('arc_newer', {}, {}, {from}, {latest}) LIMIT 1)",
                sql_string_literal(schema),
                sql_string_literal(name)
            ),
            [],
            |r| r.get(0),
        )
        .map_err(context("read the table's changes"))?;
    if rows > 0 {
        return Ok(true);
    }
    if !exists(META_SCHEMA, "descriptors")? {
        return Ok(false);
    }
    let described: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM ducklake_table_changes('arc_newer', {}, 'descriptors', {from}, {latest}) \
                 WHERE lower(table_name) = lower(?)",
                sql_string_literal(META_SCHEMA)
            ),
            [format!("{schema}.{name}")],
            |r| r.get(0),
        )
        .map_err(context("read the descriptors' changes"))?;
    Ok(described > 0)
}

/// The files under `extra.from` and the keys they go to.
fn expand(extra: &ExtraFile) -> Result<Vec<(PathBuf, String)>> {
    if !extra.from.is_dir() {
        let key = if extra.to.ends_with('/') {
            let name = extra
                .from
                .file_name()
                .ok_or_else(|| failed(format!("{} has no file name", extra.from.display())))?;
            format!("{}{}", extra.to, name.to_string_lossy())
        } else {
            extra.to.clone()
        };
        return Ok(vec![(extra.from.clone(), key)]);
    }
    let prefix = if extra.to.ends_with('/') {
        extra.to.clone()
    } else {
        format!("{}/", extra.to)
    };
    let mut out = Vec::new();
    let mut stack = vec![extra.from.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(io_failed(&dir))? {
            let path = entry.map_err(io_failed(&dir))?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(&extra.from)
                .map_err(|_| failed(format!("{} escaped its folder", path.display())))?;
            let parts: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push((path.clone(), format!("{prefix}{}", parts.join("/"))));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// How many times a readable copy's conditional upload may be refused, by writes
/// that no newer snapshot accounts for, before the publish fails.
const COPY_ATTEMPTS: u32 = 3;

/// The condition that makes an upload land only on the version `head` describes.
fn copy_condition(head: Option<&ObjectHead>) -> PutCondition {
    match head {
        Some(h) => PutCondition::IfMatch(h.etag.clone()),
        None => PutCondition::IfAbsent,
    }
}

/// Watches the live catalogue after a publish lands, answering whether a snapshot
/// newer than the landed one changed a table. Each question costs one HEAD; the
/// catalogue is fetched only when its ETag has moved from the landed one, once per
/// ETag, and each table's answer is cached per ETag. A table found changed stays
/// changed. Shared by the uploads and the manifest, so a publish where nothing
/// moves fetches no catalogue after landing.
pub struct Watch<'a> {
    store: &'a dyn ObjectStore,
    lake: &'a Lake,
    work: &'a Path,
    snapshot: i64,
    landed: ETag,
    seen: RefCell<Seen>,
}

#[derive(Default)]
struct Seen {
    /// The newest catalogue fetched, and its ETag.
    fetched: Option<(ETag, Vec<u8>)>,
    checked: HashMap<(String, String), bool>,
    newer_tables: HashSet<String>,
}

impl<'a> Watch<'a> {
    /// A watch for the publish that landed `snapshot` with the catalogue at `landed`.
    pub fn new(
        store: &'a dyn ObjectStore,
        lake: &'a Lake,
        work: &'a Path,
        snapshot: i64,
        landed: ETag,
    ) -> Self {
        Watch {
            store,
            lake,
            work,
            snapshot,
            landed,
            seen: RefCell::new(Seen::default()),
        }
    }

    /// The live catalogue's ETag, by HEAD.
    pub fn live_etag(&self) -> Result<Option<ETag>> {
        let key = self.lake.catalog_key.as_str();
        self.store
            .etag(key)
            .map_err(store_failed("read the version of", key))
    }

    /// Whether any of `tables` is already known to have a newer change.
    fn known_newer(&self, tables: &[&str]) -> bool {
        let seen = self.seen.borrow();
        tables.iter().any(|t| seen.newer_tables.contains(*t))
    }

    /// Whether a snapshot newer than the landed one changed any of `tables`, asking
    /// the store for the catalogue's version now.
    pub fn newer(&self, tables: &[&str]) -> Result<bool> {
        if self.known_newer(tables) {
            return Ok(true);
        }
        let live = self.live_etag()?;
        self.newer_at(live.as_ref(), tables)
    }

    /// [`Self::newer`], for a catalogue version `live` the caller has just read.
    pub fn newer_at(&self, live: Option<&ETag>, tables: &[&str]) -> Result<bool> {
        if self.known_newer(tables) {
            return Ok(true);
        }
        let Some(live) = live.filter(|e| **e != self.landed) else {
            return Ok(false);
        };
        let mut seen = self.seen.borrow_mut();
        if seen.fetched.as_ref().is_none_or(|(e, _)| e != live) {
            let key = self.lake.catalog_key.as_str();
            let fetched = self
                .store
                .get(key)
                .map_err(store_failed("fetch the catalogue", key))?;
            match fetched {
                // Moved again since the HEAD: the newer version answers just as well.
                Some((bytes, etag)) if etag != self.landed => seen.fetched = Some((etag, bytes)),
                _ => return Ok(false),
            }
        }
        let Seen {
            fetched,
            checked,
            newer_tables,
        } = &mut *seen;
        let (etag, bytes) = fetched.as_ref().expect("fetched above");
        for table in tables {
            let cache_key = (etag.0.clone(), (*table).to_string());
            let changed = match checked.get(&cache_key) {
                Some(&changed) => changed,
                None => {
                    let changed =
                        newer_change_in(self.lake, self.work, bytes, self.snapshot, table)?;
                    checked.insert(cache_key, changed);
                    changed
                }
            };
            if changed {
                newer_tables.insert((*table).to_string());
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Publish `tables` into the lake in `store` as one snapshot (`secret_sql` lets
/// DuckDB reach `lake.write_path`, and never appears in an error), then upload each
/// readable copy (`file`, `key`) and extra file, retrying the whole sequence when
/// another publish wins the catalogue write, up to `max_attempts` times.
///
/// A readable copy belongs to the table whose `parquet` is its file; one that
/// matches no table, like an extra file, belongs to every table in the step. Either
/// is skipped, and named in `superseded`, when a snapshot newer than this publish's
/// changed a table it belongs to.
#[expect(
    clippy::too_many_arguments,
    reason = "the publish signature the step calls"
)]
pub fn publish(
    store: &dyn ObjectStore,
    lake: &Lake,
    secret_sql: Option<&str>,
    work: &Path,
    tables: &[TableInput],
    readable: &[(PathBuf, String)],
    extra: &[ExtraFile],
    commit_info: &serde_json::Value,
    max_attempts: u32,
) -> Result<Published> {
    publish_with_landing(
        store,
        lake,
        secret_sql,
        work,
        tables,
        readable,
        extra,
        commit_info,
        max_attempts,
        &mut |_| {},
    )
}

/// [`publish`], calling `on_landed` with the commit's outcome as soon as the
/// catalogue holds it, before the uploads after it: whatever fails later, the
/// snapshot is in the lake, and the caller may owe something it names.
#[expect(
    clippy::too_many_arguments,
    reason = "the publish signature the step calls"
)]
pub fn publish_with_landing(
    store: &dyn ObjectStore,
    lake: &Lake,
    secret_sql: Option<&str>,
    work: &Path,
    tables: &[TableInput],
    readable: &[(PathBuf, String)],
    extra: &[ExtraFile],
    commit_info: &serde_json::Value,
    max_attempts: u32,
    on_landed: &mut dyn FnMut(&CommitOutcome),
) -> Result<Published> {
    fs::create_dir_all(work).map_err(io_failed(work))?;
    let max_attempts = max_attempts.max(1);
    let mut attempts = 0;
    let mut throttled = false;
    let landed = loop {
        attempts += 1;
        match attempt(store, lake, secret_sql, work, tables, commit_info)? {
            Try::Landed(landed) => break landed,
            Try::Lost => {}
            Try::Throttled => throttled = true,
        }
        if attempts >= max_attempts {
            let why = if throttled {
                "another publish won, or the store throttled"
            } else {
                "another publish won each time"
            };
            return Err(failed(format!(
                "could not write the catalogue `{}` after {attempts} attempts ({why})",
                lake.catalog_key
            )));
        }
        std::thread::sleep(Duration::from_millis(250 * u64::from(attempts)));
    };
    on_landed(&landed.outcome);

    let every_table: Vec<&str> = tables.iter().map(|t| t.table.as_str()).collect();
    let mut files: Vec<(PathBuf, String, Vec<&str>)> = Vec::new();
    for (file, key) in readable {
        let owners: Vec<&str> = tables
            .iter()
            .filter(|t| t.parquet == *file)
            .map(|t| t.table.as_str())
            .collect();
        let owners = if owners.is_empty() {
            every_table.clone()
        } else {
            owners
        };
        files.push((file.clone(), key.clone(), owners));
    }
    for e in extra {
        for (file, key) in expand(e)? {
            files.push((file, key, every_table.clone()));
        }
    }

    let mut uploaded = Vec::new();
    let mut unchanged = Vec::new();
    let mut superseded = Vec::new();
    let guard = Watch::new(
        store,
        lake,
        work,
        landed.outcome.snapshot_id,
        landed.etag.clone(),
    );
    for (file, key, owners) in files {
        if guard.known_newer(&owners) {
            superseded.push(key);
            continue;
        }
        let local = describe(&key, &file)?;
        // Every try reads the copy's own version first, then the catalogue: a newer
        // publish uploads only after its catalogue lands, so either the check sees
        // it, or the conditional upload is refused. A refusal may also be an older
        // publish's late upload, which no newer snapshot accounts for: try again.
        // An object already holding these bytes needs no upload at all.
        let mut refusals = 0;
        loop {
            let head = store
                .head(&key)
                .map_err(store_failed("read the version of", &key))?;
            if head
                .as_ref()
                .and_then(|h| h.sha256.as_deref())
                .is_some_and(|sha| sha == local.sha256)
            {
                unchanged.push(key);
                break;
            }
            let cond = copy_condition(head.as_ref());
            if guard.newer(&owners)? {
                superseded.push(key);
                break;
            }
            match store.put_file(&key, &file, cond, None) {
                Ok(_) => {
                    uploaded.push(local);
                    break;
                }
                Err(StoreError::PreconditionFailed) => {
                    refusals += 1;
                    if refusals < COPY_ATTEMPTS {
                        continue;
                    }
                    if guard.newer(&owners)? {
                        superseded.push(key);
                        break;
                    }
                    return Err(failed(format!(
                        "`{key}` kept changing under this publish; gave up after \
                         {refusals} refused uploads"
                    )));
                }
                Err(e) => return Err(store_failed("upload", &key)(e)),
            }
        }
    }

    Ok(Published {
        outcome: landed.outcome,
        uploaded,
        unchanged,
        superseded,
        pinned_catalog: landed.pinned,
        catalog_etag: landed.etag,
        attempts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lake::commit::TableInput;
    use crate::lake::meta::{TableDescriptor, table_descriptor};
    use crate::lake::store::{DirStore, ETag, ObjectHead, ObjectStore, PutCondition, StoreError};
    use crate::lake::test_server::TestServer;
    use crate::operator::sql_string_literal;
    use serde_json::json;
    use sha2::Digest;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Barrier, Mutex};
    use tempfile::TempDir;

    const CATALOG: &str = "catalog/open.ducklake";

    struct Fixture {
        dir: TempDir,
        store: DirStore,
        server: TestServer,
        lake: Lake,
        works: AtomicU32,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let root = dir.path().join("store");
            std::fs::create_dir_all(&root).unwrap();
            let server = TestServer::serve(&root);
            let lake = Lake {
                catalog_key: CATALOG.to_string(),
                data_path: server.url("lake/"),
                data_prefix: "lake/".to_string(),
                write_path: format!("{}/", root.join("lake").display()),
                pinned_prefix: Some("catalog/open-".to_string()),
            };
            Fixture {
                store: DirStore::new(&root),
                dir,
                server,
                lake,
                works: AtomicU32::new(0),
            }
        }

        /// A fresh work folder for one publish.
        fn work(&self) -> PathBuf {
            let n = self.works.fetch_add(1, Ordering::SeqCst);
            self.dir.path().join(format!("work-{n}"))
        }

        fn parquet(&self, name: &str, select: &str) -> PathBuf {
            let path = self.dir.path().join("in").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "COPY ({select}) TO {} (FORMAT parquet);",
                sql_string_literal(&path.display().to_string())
            ))
            .unwrap();
            path
        }

        fn publish_via(
            &self,
            store: &dyn ObjectStore,
            tables: &[TableInput],
            readable: &[(PathBuf, String)],
            extra: &[ExtraFile],
        ) -> crate::error::Result<Published> {
            publish(
                store,
                &self.lake,
                None,
                &self.work(),
                tables,
                readable,
                extra,
                &json!({"run": "test"}),
                3,
            )
        }

        fn publish(
            &self,
            tables: &[TableInput],
            readable: &[(PathBuf, String)],
        ) -> crate::error::Result<Published> {
            self.publish_via(&self.store, tables, readable, &[])
        }

        fn object(&self, key: &str) -> Option<(Vec<u8>, ETag)> {
            self.store.get(key).unwrap()
        }

        fn keys_under(&self, prefix: &str) -> Vec<String> {
            let root = self.dir.path().join("store");
            let mut out = Vec::new();
            let mut stack = vec![root.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        let key = p
                            .strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        if key.starts_with(prefix)
                            && !key.ends_with(".lock")
                            && !key.ends_with(".tmp")
                        {
                            out.push(key);
                        }
                    }
                }
            }
            out.sort();
            out
        }

        /// Attach the published catalogue over http, as a reader does.
        fn http<T>(&self, f: impl FnOnce(&duckdb::Connection) -> T) -> T {
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "INSTALL httpfs; LOAD httpfs; LOAD ducklake; \
                 ATTACH {} AS lake (READ_ONLY, DATA_PATH {});",
                sql_string_literal(&format!("ducklake:{}", self.server.url(CATALOG))),
                sql_string_literal(&self.server.url("lake/")),
            ))
            .unwrap();
            f(&conn)
        }

        fn http_count(&self, sql: &str) -> i64 {
            self.http(|c| c.query_row(sql, [], |r| r.get(0)).unwrap())
        }
    }

    fn descriptor_with(table: &str, about: &str) -> TableDescriptor {
        let package = json!({
            "name": table,
            "description": about,
            "resources": [{
                "name": table,
                "format": "parquet",
                "schema": {
                    "fields": [
                        {"name": "id", "type": "integer"},
                        {"name": "v", "type": "string"},
                    ],
                    "primaryKey": ["id"],
                },
            }],
        });
        table_descriptor(table, &package, &["id".to_string(), "v".to_string()]).unwrap()
    }

    fn input(table: &str, parquet: &Path) -> TableInput {
        input_with(table, parquet, &format!("all about {table}"))
    }

    fn input_with(table: &str, parquet: &Path, about: &str) -> TableInput {
        let descriptor = descriptor_with(table, about);
        TableInput {
            table: table.to_string(),
            parquet: parquet.to_path_buf(),
            key: descriptor.key.clone(),
            descriptor,
            replace_on_schema_change: false,
        }
    }

    /// `n` rows `id 0..n`, `v = '<tag><id>'`: above ten rows, DuckLake writes a
    /// data file instead of inlining them.
    fn rows(n: u32, tag: &str) -> String {
        format!("SELECT range AS id, '{tag}' || range AS v FROM range({n})")
    }

    type Hook<'a> = Box<dyn FnOnce() + Send + 'a>;

    /// A store that passes through to `inner`, with faults and a hook.
    struct Wrapped<'a> {
        inner: &'a dyn ObjectStore,
        fail_pinned_uploads: bool,
        /// Answer `Throttled` to the next pinned-copy write.
        throttle_next_pin: Mutex<bool>,
        refuse_catalog_writes: bool,
        throttle_catalog_writes: bool,
        /// Answer `PreconditionFailed` to every readable or extra upload.
        refuse_copy_uploads: bool,
        /// Run once, just before the version lookup of a readable or extra file
        /// numbered `.0` (from 1). The catalogue's own lookups are not counted.
        before_etag: Mutex<(u32, Option<Hook<'a>>)>,
        etag_calls: AtomicU32,
        /// Run once, just before the first readable or extra upload.
        before_copy_upload: Mutex<Option<Box<dyn FnOnce() + Send + 'a>>>,
        /// Waited on before the first catalogue write.
        barrier: Option<&'a Barrier>,
        waited: Mutex<bool>,
        /// Run once, just after the first successful catalogue write.
        after_catalog_write: Mutex<Option<Box<dyn FnOnce() + Send + 'a>>>,
    }

    impl<'a> Wrapped<'a> {
        fn new(inner: &'a dyn ObjectStore) -> Self {
            Wrapped {
                inner,
                fail_pinned_uploads: false,
                throttle_next_pin: Mutex::new(false),
                refuse_catalog_writes: false,
                throttle_catalog_writes: false,
                refuse_copy_uploads: false,
                before_copy_upload: Mutex::new(None),
                before_etag: Mutex::new((0, None)),
                etag_calls: AtomicU32::new(0),
                barrier: None,
                waited: Mutex::new(false),
                after_catalog_write: Mutex::new(None),
            }
        }

        fn after_catalog_write(self, hook: impl FnOnce() + Send + 'a) -> Self {
            *self.after_catalog_write.lock().unwrap() = Some(Box::new(hook));
            self
        }
    }

    impl ObjectStore for Wrapped<'_> {
        fn get(&self, key: &str) -> std::result::Result<Option<(Vec<u8>, ETag)>, StoreError> {
            self.inner.get(key)
        }

        fn head(&self, key: &str) -> std::result::Result<Option<ObjectHead>, StoreError> {
            if key != CATALOG {
                let n = self.etag_calls.fetch_add(1, Ordering::SeqCst) + 1;
                let hook = {
                    let mut slot = self.before_etag.lock().unwrap();
                    if slot.0 == n { slot.1.take() } else { None }
                };
                if let Some(hook) = hook {
                    hook();
                }
            }
            self.inner.head(key)
        }

        fn put(
            &self,
            key: &str,
            bytes: &[u8],
            cond: PutCondition,
            cache_control: Option<&str>,
        ) -> std::result::Result<ETag, StoreError> {
            if key.starts_with("catalog/open-") {
                if self.fail_pinned_uploads {
                    return Err(StoreError::Other("upload refused".into()));
                }
                let mut throttle = self.throttle_next_pin.lock().unwrap();
                if *throttle {
                    *throttle = false;
                    return Err(StoreError::Throttled);
                }
            }
            if key == CATALOG {
                if self.throttle_catalog_writes {
                    return Err(StoreError::Throttled);
                }
                if self.refuse_catalog_writes {
                    return Err(StoreError::PreconditionFailed);
                }
                if let Some(b) = self.barrier {
                    let mut waited = self.waited.lock().unwrap();
                    if !*waited {
                        *waited = true;
                        drop(waited);
                        b.wait();
                    }
                }
                let out = self.inner.put(key, bytes, cond, cache_control)?;
                if let Some(hook) = self.after_catalog_write.lock().unwrap().take() {
                    hook();
                }
                return Ok(out);
            }
            self.inner.put(key, bytes, cond, cache_control)
        }

        fn put_file(
            &self,
            key: &str,
            path: &Path,
            cond: PutCondition,
            cache_control: Option<&str>,
        ) -> std::result::Result<ETag, StoreError> {
            let hook = self.before_copy_upload.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
            if self.refuse_copy_uploads {
                return Err(StoreError::PreconditionFailed);
            }
            self.inner.put_file(key, path, cond, cache_control)
        }
    }

    #[test]
    fn a_publish_is_readable_over_http() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let b = fx.parquet("b.parquet", &rows(3, "b"));
        let out = fx
            .publish(
                &[input("s.a", &a), input("s.b", &b)],
                &[(a.clone(), "ns/a.parquet".to_string())],
            )
            .unwrap();
        assert!(out.outcome.committed);
        assert_eq!(out.attempts, 1);
        assert!(!out.outcome.new_files.is_empty(), "twenty rows make a file");
        for f in &out.outcome.new_files {
            assert!(
                fx.object(&format!("{}{f}", fx.lake.data_prefix)).is_some(),
                "{f} in the store"
            );
        }
        let readable = out
            .uploaded
            .iter()
            .find(|u| u.key == "ns/a.parquet")
            .unwrap();
        let bytes = std::fs::read(&a).unwrap();
        assert_eq!(readable.bytes, bytes.len() as u64);
        assert_eq!(readable.sha256.len(), 64);
        assert_eq!(fx.object("ns/a.parquet").unwrap().0, bytes);
        assert_eq!(fx.object(CATALOG).unwrap().1, out.catalog_etag);

        assert_eq!(fx.http_count("SELECT count(*) FROM lake.s.a"), 20);
        assert_eq!(fx.http_count("SELECT count(*) FROM lake.s.b"), 3);
        assert_eq!(
            fx.http_count("SELECT count(*) FROM lake.s.a WHERE v = 'a7'"),
            1
        );
        assert_eq!(
            fx.http_count("SELECT count(*) FROM lake.meta.descriptors"),
            2
        );
    }

    #[test]
    fn a_second_publish_time_travels() {
        let fx = Fixture::new();
        let v1 = fx.parquet("v1.parquet", &rows(20, "old"));
        let first = fx.publish(&[input("s.a", &v1)], &[]).unwrap();
        let v2 = fx.parquet("v2.parquet", &rows(30, "new"));
        let second = fx.publish(&[input("s.a", &v2)], &[]).unwrap();
        assert!(second.outcome.snapshot_id > first.outcome.snapshot_id);
        assert_eq!(fx.http_count("SELECT count(*) FROM lake.s.a"), 30);
        assert_eq!(
            fx.http_count("SELECT count(*) FROM lake.s.a WHERE v LIKE 'new%'"),
            30
        );
        let then = first.outcome.snapshot_id;
        assert_eq!(
            fx.http_count(&format!(
                "SELECT count(*) FROM lake.s.a AT (VERSION => {then}) WHERE v LIKE 'old%'"
            )),
            20
        );
    }

    #[test]
    fn an_unchanged_publish_writes_no_catalogue() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let first = fx.publish(&[input("s.a", &a)], &[]).unwrap();
        let (_, etag) = fx.object(CATALOG).unwrap();
        let pinned = fx.keys_under("catalog/");
        let again = fx.publish(&[input("s.a", &a)], &[]).unwrap();
        assert!(!again.outcome.committed);
        assert_eq!(again.outcome.snapshot_id, first.outcome.snapshot_id);
        assert_eq!(fx.object(CATALOG).unwrap().1, etag);
        assert_eq!(again.catalog_etag, etag);
        assert_eq!(fx.keys_under("catalog/"), pinned, "no pinned copy added");
        assert_eq!(again.pinned_catalog, first.pinned_catalog);
        assert!(again.uploaded.is_empty());
    }

    #[test]
    fn a_lost_race_retries_and_both_publishes_land() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let b = fx.parquet("b.parquet", &rows(20, "b"));
        let barrier = Barrier::new(2);
        let (ra, rb) = std::thread::scope(|s| {
            let run = |table: &'static str, file: &PathBuf| {
                let mut store = Wrapped::new(&fx.store);
                store.barrier = Some(&barrier);
                fx.publish_via(&store, &[input(table, file)], &[], &[])
            };
            let ha = s.spawn(move || run("s.a", &a));
            let hb = s.spawn(move || run("s.b", &b));
            (ha.join().unwrap(), hb.join().unwrap())
        });
        let (ra, rb) = (ra.unwrap(), rb.unwrap());
        assert!(
            ra.attempts >= 2 || rb.attempts >= 2,
            "{} {}",
            ra.attempts,
            rb.attempts
        );
        assert_eq!(fx.http_count("SELECT count(*) FROM lake.s.a"), 20);
        assert_eq!(fx.http_count("SELECT count(*) FROM lake.s.b"), 20);
        assert_eq!(
            fx.http_count("SELECT count(*) FROM lake.meta.descriptors"),
            2
        );
    }

    #[test]
    fn a_refused_upload_leaves_the_live_lake_as_it_was() {
        let fx = Fixture::new();
        let v1 = fx.parquet("v1.parquet", &rows(20, "old"));
        fx.publish(
            &[input("s.a", &v1)],
            &[(v1.clone(), "ns/a.parquet".to_string())],
        )
        .unwrap();
        let catalogue = fx.object(CATALOG).unwrap();
        let copy = fx.object("ns/a.parquet").unwrap().0;

        let v2 = fx.parquet("v2.parquet", &rows(40, "new"));
        let mut store = Wrapped::new(&fx.store);
        store.fail_pinned_uploads = true;
        let err = fx
            .publish_via(
                &store,
                &[input("s.a", &v2)],
                &[(v2.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap_err();
        assert!(err.to_string().contains("upload refused"), "{err}");
        assert_eq!(fx.object(CATALOG).unwrap(), catalogue);
        assert_eq!(fx.object("ns/a.parquet").unwrap().0, copy);
        assert_eq!(
            fx.http_count("SELECT count(*) FROM lake.s.a WHERE v LIKE 'old%'"),
            20
        );
    }

    #[test]
    fn a_commit_to_another_table_does_not_stop_this_publishs_readable_copy() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let b = fx.parquet("b.parquet", &rows(20, "b"));
        let store = Wrapped::new(&fx.store).after_catalog_write(|| {
            fx.publish(&[input("t.b", &b)], &[]).unwrap();
        });
        let out = fx
            .publish_via(
                &store,
                &[input("s.a", &a)],
                &[(a.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap();
        assert_ne!(
            fx.object(CATALOG).unwrap().1,
            out.catalog_etag,
            "the other publish landed in between"
        );
        assert!(out.superseded.is_empty(), "{:?}", out.superseded);
        assert_eq!(
            fx.object("ns/a.parquet").unwrap().0,
            std::fs::read(&a).unwrap()
        );
        assert!(
            !newer_change(
                &fx.store,
                &fx.lake,
                &fx.work(),
                out.outcome.snapshot_id,
                "s.a"
            )
            .unwrap()
        );
    }

    #[test]
    fn a_newer_descriptor_only_change_supersedes_the_readable_copy() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let a_newer = fx.parquet("a-newer.parquet", &rows(20, "a"));
        let store = Wrapped::new(&fx.store).after_catalog_write(|| {
            let newer = fx
                .publish(&[input_with("s.a", &a_newer, "a better description")], &[])
                .unwrap();
            assert!(newer.outcome.committed);
        });
        let out = fx
            .publish_via(
                &store,
                &[input("s.a", &a)],
                &[(a.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap();
        assert_eq!(out.superseded, ["ns/a.parquet"]);
        assert!(fx.object("ns/a.parquet").is_none());
        assert!(
            newer_change(
                &fx.store,
                &fx.lake,
                &fx.work(),
                out.outcome.snapshot_id,
                "s.a"
            )
            .unwrap()
        );
    }

    #[test]
    fn a_superseded_publish_does_not_overwrite_the_readable_copy() {
        let fx = Fixture::new();
        let v1 = fx.parquet("v1.parquet", &rows(20, "old"));
        let v2 = fx.parquet("v2.parquet", &rows(25, "new"));
        let store = Wrapped::new(&fx.store).after_catalog_write(|| {
            let newer = fx
                .publish(
                    &[input("s.a", &v2)],
                    &[(v2.clone(), "ns/a.parquet".to_string())],
                )
                .unwrap();
            assert!(newer.superseded.is_empty());
        });
        let out = fx
            .publish_via(
                &store,
                &[input("s.a", &v1)],
                &[(v1.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap();
        assert_eq!(out.superseded, ["ns/a.parquet"]);
        assert_eq!(
            fx.object("ns/a.parquet").unwrap().0,
            std::fs::read(&v2).unwrap()
        );
    }

    #[test]
    fn a_publish_that_loses_every_race_leaves_the_readable_copy_alone() {
        let fx = Fixture::new();
        let v1 = fx.parquet("v1.parquet", &rows(20, "old"));
        fx.publish(
            &[input("s.a", &v1)],
            &[(v1.clone(), "ns/a.parquet".to_string())],
        )
        .unwrap();
        let catalogue = fx.object(CATALOG).unwrap();
        let copy = fx.object("ns/a.parquet").unwrap().0;

        let v2 = fx.parquet("v2.parquet", &rows(20, "new"));
        let mut store = Wrapped::new(&fx.store);
        store.refuse_catalog_writes = true;
        let err = fx
            .publish_via(
                &store,
                &[input("s.a", &v2)],
                &[(v2.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains(CATALOG), "{err}");
        assert!(err.contains('3'), "names the attempt count: {err}");
        assert!(err.contains("another publish won"), "{err}");
        assert!(!err.contains("throttl"), "{err}");
        assert_eq!(fx.object(CATALOG).unwrap(), catalogue);
        assert_eq!(fx.object("ns/a.parquet").unwrap().0, copy);
    }

    #[test]
    fn the_pinned_copy_is_byte_identical_to_the_catalogue() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let out = fx.publish(&[input("s.a", &a)], &[]).unwrap();
        let pinned = out.pinned_catalog.clone().unwrap();
        let (stable, _) = fx.object(CATALOG).unwrap();
        let (copy, _) = fx.object(&pinned).unwrap();
        assert_eq!(copy, stable);
        let sha: String = sha2::Sha256::digest(&stable)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(pinned, format!("catalog/open-{}.ducklake", &sha[..16]));
    }

    #[test]
    fn extra_files_upload_under_their_prefix() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(3, "a"));
        let parts = fx.dir.path().join("parts");
        std::fs::create_dir_all(parts.join("deep")).unwrap();
        std::fs::write(parts.join("one.txt"), b"one").unwrap();
        std::fs::write(parts.join("deep").join("two.txt"), b"two").unwrap();
        let single = fx.dir.path().join("single.json");
        std::fs::write(&single, b"{}").unwrap();
        let out = fx
            .publish_via(
                &fx.store,
                &[input("s.a", &a)],
                &[],
                &[
                    ExtraFile {
                        from: parts,
                        to: "ns/search/".to_string(),
                    },
                    ExtraFile {
                        from: single,
                        to: "ns/single.json".to_string(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(fx.object("ns/search/one.txt").unwrap().0, b"one");
        assert_eq!(fx.object("ns/search/deep/two.txt").unwrap().0, b"two");
        assert_eq!(fx.object("ns/single.json").unwrap().0, b"{}");
        let keys: Vec<&str> = out.uploaded.iter().map(|u| u.key.as_str()).collect();
        for k in [
            "ns/search/one.txt",
            "ns/search/deep/two.txt",
            "ns/single.json",
        ] {
            assert!(keys.contains(&k), "{keys:?}");
        }
    }

    #[test]
    fn a_copy_written_between_the_check_and_the_upload_is_not_overwritten() {
        let fx = Fixture::new();
        let v1 = fx.parquet("v1.parquet", &rows(20, "old"));
        let v2 = fx.parquet("v2.parquet", &rows(25, "new"));
        let store = Wrapped::new(&fx.store);
        *store.before_copy_upload.lock().unwrap() = Some(Box::new(|| {
            let newer = fx
                .publish(
                    &[input("s.a", &v2)],
                    &[(v2.clone(), "ns/a.parquet".to_string())],
                )
                .unwrap();
            assert!(newer.superseded.is_empty());
        }));
        let out = fx
            .publish_via(
                &store,
                &[input("s.a", &v1)],
                &[(v1.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap();
        assert_eq!(out.superseded, ["ns/a.parquet"]);
        assert_eq!(
            fx.object("ns/a.parquet").unwrap().0,
            std::fs::read(&v2).unwrap()
        );
    }

    #[test]
    fn a_throttled_pinned_copy_write_retries() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let store = Wrapped::new(&fx.store);
        *store.throttle_next_pin.lock().unwrap() = true;
        let out = fx
            .publish_via(&store, &[input("s.a", &a)], &[], &[])
            .unwrap();
        assert_eq!(out.attempts, 2);
        assert_eq!(fx.http_count("SELECT count(*) FROM lake.s.a"), 20);
    }

    #[test]
    fn running_out_of_attempts_while_throttled_names_throttling() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(3, "a"));
        let mut store = Wrapped::new(&fx.store);
        store.throttle_catalog_writes = true;
        let err = fx
            .publish_via(&store, &[input("s.a", &a)], &[], &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains(CATALOG) && err.contains("throttled"), "{err}");
    }

    #[test]
    fn an_older_publishs_late_copy_is_replaced_by_the_newer_one() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let store = Wrapped::new(&fx.store);
        *store.before_copy_upload.lock().unwrap() = Some(Box::new(|| {
            // An older publish's slow upload lands; no newer catalogue does.
            fx.store
                .put("ns/a.parquet", b"older bytes", PutCondition::Any, None)
                .unwrap();
        }));
        let out = fx
            .publish_via(
                &store,
                &[input("s.a", &a)],
                &[(a.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap();
        assert!(out.superseded.is_empty(), "{:?}", out.superseded);
        assert_eq!(
            fx.object("ns/a.parquet").unwrap().0,
            std::fs::read(&a).unwrap()
        );
    }

    #[test]
    fn a_newer_publish_during_the_retry_is_not_overwritten() {
        let fx = Fixture::new();
        let v1 = fx.parquet("v1.parquet", &rows(20, "old"));
        let v2 = fx.parquet("v2.parquet", &rows(25, "new"));
        let store = Wrapped::new(&fx.store);
        // The first upload is refused: an older publish's late copy lands.
        *store.before_copy_upload.lock().unwrap() = Some(Box::new(|| {
            fx.store
                .put("ns/a.parquet", b"older bytes", PutCondition::Any, None)
                .unwrap();
        }));
        // Then, as the retry reads the copy's version, a newer publish lands.
        *store.before_etag.lock().unwrap() = (
            2,
            Some(Box::new(|| {
                let newer = fx
                    .publish(
                        &[input("s.a", &v2)],
                        &[(v2.clone(), "ns/a.parquet".to_string())],
                    )
                    .unwrap();
                assert!(newer.superseded.is_empty());
            })),
        );
        let out = fx
            .publish_via(
                &store,
                &[input("s.a", &v1)],
                &[(v1.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap();
        assert_eq!(out.superseded, ["ns/a.parquet"]);
        assert_eq!(
            fx.object("ns/a.parquet").unwrap().0,
            std::fs::read(&v2).unwrap()
        );
    }

    #[test]
    fn a_copy_refused_every_time_fails_naming_the_key() {
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(3, "a"));
        let mut store = Wrapped::new(&fx.store);
        store.refuse_copy_uploads = true;
        let err = fx
            .publish_via(
                &store,
                &[input("s.a", &a)],
                &[(a.clone(), "ns/a.parquet".to_string())],
                &[],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("ns/a.parquet") && err.contains('3'), "{err}");
    }

    /// The entry a caller (the operator) would build for a table's readable copy.
    fn manifest_entry(
        address: &str,
        url: &str,
        bytes: &[u8],
    ) -> crate::lake::manifest::ManifestEntry {
        crate::lake::manifest::ManifestEntry {
            address: address.to_string(),
            url: url.to_string(),
            rows: 20,
            bytes: bytes.len() as u64,
            sha256: sha256_hex(bytes),
            published: "2026-11-13T00:00:00Z".to_string(),
            run: json!({"id": "r", "record": "runs/r.json"}),
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn a_newer_publish_of_one_table_leaves_only_its_entry_alone() {
        use crate::lake::manifest::update_manifest;
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let b1 = fx.parquet("b1.parquet", &rows(20, "b"));
        let b2 = fx.parquet("b2.parquet", &rows(25, "newer"));
        let first = fx
            .publish(&[input("s.a", &a), input("s.b", &b1)], &[])
            .unwrap();
        let snapshot = first.outcome.snapshot_id;
        // A newer publish changes table B only.
        fx.publish(&[input("s.b", &b2)], &[]).unwrap();
        let mut kept = Vec::new();
        for (table, address, file) in [("s.a", "ns/a", &a), ("s.b", "ns/b", &b1)] {
            let changed = newer_change(&fx.store, &fx.lake, &fx.work(), snapshot, table).unwrap();
            if !changed {
                kept.push(manifest_entry(address, "u", &std::fs::read(file).unwrap()));
            }
        }
        assert_eq!(
            kept.iter().map(|e| e.address.as_str()).collect::<Vec<_>>(),
            ["ns/a"]
        );
        assert!(
            update_manifest(&fx.store, "manifest.json", &|| Ok((kept.clone(), None)), 3).unwrap()
        );
        let doc: serde_json::Value =
            serde_json::from_slice(&fx.object("manifest.json").unwrap().0).unwrap();
        assert!(doc["datasets"]["ns/a"].is_object());
        assert!(doc["datasets"].get("ns/b").is_none());
    }

    #[test]
    fn an_unchanged_publish_repairs_a_stale_manifest_entry() {
        use crate::lake::manifest::update_manifest;
        let fx = Fixture::new();
        let a = fx.parquet("a.parquet", &rows(20, "a"));
        let bytes = std::fs::read(&a).unwrap();
        let entry = manifest_entry("ns/a", "u", &bytes);
        let first = fx.publish(&[input("s.a", &a)], &[]).unwrap();
        let pinned = first.pinned_catalog.clone().unwrap();
        update_manifest(
            &fx.store,
            "manifest.json",
            &|| Ok((vec![entry.clone()], Some(pinned.clone()))),
            3,
        )
        .unwrap();

        // An interrupted publish left the manifest behind.
        let mut stale = entry.clone();
        stale.sha256 = "0".repeat(64);
        assert!(
            update_manifest(
                &fx.store,
                "manifest.json",
                &|| Ok((
                    vec![stale.clone()],
                    Some("catalog/open-old.ducklake".to_string())
                )),
                3,
            )
            .unwrap()
        );
        let doc = manifest_doc(&fx);
        assert_eq!(doc["datasets"]["ns/a"]["sha256"], "0".repeat(64));
        assert_eq!(doc["catalog"]["url"], "catalog/open-old.ducklake");

        // The next publish changes nothing, yet the caller's filtered write repairs both.
        let again = fx.publish(&[input("s.a", &a)], &[]).unwrap();
        assert!(!again.outcome.committed);
        assert_eq!(
            fx.store.etag(CATALOG).unwrap(),
            Some(again.catalog_etag.clone())
        );
        assert!(
            !newer_change(
                &fx.store,
                &fx.lake,
                &fx.work(),
                again.outcome.snapshot_id,
                "s.a"
            )
            .unwrap()
        );
        let pinned = again.pinned_catalog.clone().unwrap();
        assert!(
            update_manifest(
                &fx.store,
                "manifest.json",
                &|| Ok((vec![entry.clone()], Some(pinned.clone()))),
                3
            )
            .unwrap()
        );
        let doc = manifest_doc(&fx);
        assert_eq!(doc["datasets"]["ns/a"]["sha256"], entry.sha256);
        assert_eq!(doc["catalog"]["url"], pinned);
    }

    fn manifest_doc(fx: &Fixture) -> serde_json::Value {
        serde_json::from_slice(&fx.object("manifest.json").unwrap().0).unwrap()
    }

    /// What the operator computes for one table: its entry only if no newer snapshot
    /// changed the table, and the pinned address only while the live catalogue is
    /// still the one this publish wrote.
    fn filtered(
        fx: &Fixture,
        published: &Published,
        table: &str,
        address: &str,
        file: &Path,
    ) -> crate::error::Result<(Vec<crate::lake::manifest::ManifestEntry>, Option<String>)> {
        let mut entries = Vec::new();
        if !newer_change(
            &fx.store,
            &fx.lake,
            &fx.work(),
            published.outcome.snapshot_id,
            table,
        )? {
            entries.push(manifest_entry(address, "u", &std::fs::read(file).unwrap()));
        }
        let pinned = if fx.store.etag(CATALOG).unwrap() == Some(published.catalog_etag.clone()) {
            published.pinned_catalog.clone()
        } else {
            None
        };
        Ok((entries, pinned))
    }

    #[test]
    fn a_refused_manifest_write_does_not_reapply_a_stale_entry_over_a_newer_publish() {
        use crate::lake::manifest::{BeforeFirstPut, update_manifest};
        let fx = Fixture::new();
        let a1 = fx.parquet("a1.parquet", &rows(20, "one"));
        let a2 = fx.parquet("a2.parquet", &rows(22, "two"));
        let a3 = fx.parquet("a3.parquet", &rows(25, "three"));
        let first = fx.publish(&[input("s.a", &a1)], &[]).unwrap();
        let (e, p) = filtered(&fx, &first, "s.a", "ns/a", &a1).unwrap();
        update_manifest(
            &fx.store,
            "manifest.json",
            &|| Ok((e.clone(), p.clone())),
            3,
        )
        .unwrap();

        // An older publish (2) reads the manifest; before its write, a newer publish
        // (3) of the same table lands and writes its own entry.
        let second = fx.publish(&[input("s.a", &a2)], &[]).unwrap();
        let store = BeforeFirstPut::new(&fx.store, || {
            let third = fx.publish(&[input("s.a", &a3)], &[]).unwrap();
            let (e, p) = filtered(&fx, &third, "s.a", "ns/a", &a3).unwrap();
            update_manifest(
                &fx.store,
                "manifest.json",
                &|| Ok((e.clone(), p.clone())),
                3,
            )
            .unwrap();
        });
        update_manifest(
            &store,
            "manifest.json",
            &|| filtered(&fx, &second, "s.a", "ns/a", &a2),
            3,
        )
        .unwrap();

        let doc = manifest_doc(&fx);
        assert_eq!(
            doc["datasets"]["ns/a"]["sha256"],
            sha256_hex(&std::fs::read(&a3).unwrap())
        );
        let live_pin = fx.store.etag(CATALOG).unwrap();
        assert!(live_pin.is_some());
        assert_ne!(
            doc["catalog"]["url"].as_str().unwrap(),
            second.pinned_catalog.as_deref().unwrap()
        );
    }
}
