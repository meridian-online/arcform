//! Publishing a local DuckLake commit to an object store.
//!
//! Each attempt fetches the live catalogue and commits into a local copy of it, with
//! DuckLake reading and writing data files in place through the lake's write path.
//! It then pins an immutable copy of the new catalogue, and only then writes the
//! stable catalogue, conditionally on the ETag it fetched. A failure before that
//! last write leaves the live lake as it was (at worst, orphan data files nothing
//! references). Readable copies and extra files are uploaded only
//! once the catalogue holds this publish's snapshot, and are skipped when a newer
//! snapshot changed their table, so an older publish never overwrites a newer one's.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::commit::{CommitOutcome, TableInput, commit};
use super::meta::{META_SCHEMA, failed};
use super::store::{ETag, ObjectStore, PutCondition, StoreError};
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
    /// `s3://<bucket>/<data_prefix>` live, a local folder otherwise. Ends in `/`.
    pub write_path: String,
    /// The pinned copies' key prefix, e.g. `catalog/open-`.
    pub pinned_prefix: String,
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
    /// Readable copies and extra files skipped because a newer snapshot changed
    /// their table: a newer publish owns them.
    pub superseded: Vec<String>,
    /// The pinned catalogue's key. Always set.
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

/// Upload `bytes`' immutable copy, if absent, and return its key; `None` when the
/// store is throttling, so the attempt starts again.
fn pin(store: &dyn ObjectStore, lake: &Lake, bytes: &[u8]) -> Result<Option<String>> {
    let key = format!(
        "{}{}.ducklake",
        lake.pinned_prefix,
        &sha256_hex(bytes)[..16]
    );
    match store.put(&key, bytes, PutCondition::IfAbsent, Some(PINNED_CACHE)) {
        // Present already: its key is its content's hash, so it holds these bytes.
        Ok(_) | Err(StoreError::PreconditionFailed) => Ok(Some(key)),
        Err(StoreError::Throttled) => Ok(None),
        Err(e) => Err(store_failed("upload the pinned catalogue", &key)(e)),
    }
}

/// What an attempt that landed leaves for the uploads after it.
struct Landed {
    outcome: CommitOutcome,
    pinned: String,
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
                .get(key)
                .map_err(store_failed("fetch the catalogue", key))?
                .map(|(_, etag)| etag);
            if now.as_ref() != Some(&fetched_etag) {
                return Ok(Try::Lost);
            }
            let Some(pinned) = pin(store, lake, &fetched_bytes)? else {
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
            let Some(pinned) = pin(store, lake, &bytes)? else {
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
/// changed `table`'s rows or its descriptor in `meta.descriptors`. Both checks go
/// by name, so DuckLake resolves table ids itself.
pub fn newer_change(
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

/// `newer_change` against catalogue bytes already fetched.
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

/// The condition that makes an upload to `key` land only on the version just read.
fn copy_condition(store: &dyn ObjectStore, key: &str) -> Result<PutCondition> {
    Ok(
        match store
            .etag(key)
            .map_err(store_failed("read the version of", key))?
        {
            Some(etag) => PutCondition::IfMatch(etag),
            None => PutCondition::IfAbsent,
        },
    )
}

/// Whether a snapshot newer than the landed one changed a file's tables, with each
/// table's answer cached per catalogue ETag. A newer change stays newer.
struct Supersede<'a> {
    store: &'a dyn ObjectStore,
    lake: &'a Lake,
    work: &'a Path,
    landed: &'a Landed,
    checked: HashMap<(String, String), bool>,
    newer_tables: HashSet<String>,
}

impl Supersede<'_> {
    fn known_newer(&self, owners: &[&str]) -> bool {
        owners.iter().any(|t| self.newer_tables.contains(*t))
    }

    /// One catalogue fetch, then the cached or computed check for each table.
    fn newer(&mut self, owners: &[&str]) -> Result<bool> {
        if self.known_newer(owners) {
            return Ok(true);
        }
        let key = &self.lake.catalog_key;
        let current = self
            .store
            .get(key)
            .map_err(store_failed("fetch the catalogue", key))?;
        let Some((bytes, etag)) = current.filter(|(_, e)| *e != self.landed.etag) else {
            return Ok(false);
        };
        for table in owners {
            let cache_key = (etag.0.clone(), (*table).to_string());
            let changed = match self.checked.get(&cache_key) {
                Some(&changed) => changed,
                None => {
                    let changed = newer_change_in(
                        self.lake,
                        self.work,
                        &bytes,
                        self.landed.outcome.snapshot_id,
                        table,
                    )?;
                    self.checked.insert(cache_key, changed);
                    changed
                }
            };
            if changed {
                self.newer_tables.insert((*table).to_string());
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
    let mut superseded = Vec::new();
    let mut guard = Supersede {
        store,
        lake,
        work,
        landed: &landed,
        checked: HashMap::new(),
        newer_tables: HashSet::new(),
    };
    for (file, key, owners) in files {
        if guard.known_newer(&owners) {
            superseded.push(key);
            continue;
        }
        // The copy's own version first, then the catalogue: a newer publish uploads
        // only after its catalogue lands, so either the check sees it, or the
        // conditional upload below is refused.
        let mut cond = copy_condition(store, &key)?;
        if guard.newer(&owners)? {
            superseded.push(key);
            continue;
        }
        let mut refusals = 0;
        loop {
            match store.put_file(&key, &file, cond, None) {
                Ok(_) => {
                    uploaded.push(describe(&key, &file)?);
                    break;
                }
                // Written since the check: by a newer publish, or by an older one's
                // late upload. Only a newer snapshot of the table owns the key.
                Err(StoreError::PreconditionFailed) => {
                    if guard.newer(&owners)? {
                        superseded.push(key);
                        break;
                    }
                    refusals += 1;
                    if refusals >= COPY_ATTEMPTS {
                        return Err(failed(format!(
                            "`{key}` kept changing under this publish; gave up after \
                             {refusals} refused uploads"
                        )));
                    }
                    cond = copy_condition(store, &key)?;
                }
                Err(e) => return Err(store_failed("upload", &key)(e)),
            }
        }
    }

    Ok(Published {
        outcome: landed.outcome,
        uploaded,
        superseded,
        pinned_catalog: Some(landed.pinned),
        catalog_etag: landed.etag,
        attempts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lake::commit::TableInput;
    use crate::lake::meta::{TableDescriptor, table_descriptor};
    use crate::lake::store::{DirStore, ETag, ObjectStore, PutCondition, StoreError};
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
                pinned_prefix: "catalog/open-".to_string(),
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

    /// A store that passes through to `inner`, with faults and a hook.
    struct Wrapped<'a> {
        inner: &'a dyn ObjectStore,
        fail_pinned_uploads: bool,
        /// Answer `Throttled` to the next pinned-copy write.
        throttle_next_pin: Mutex<bool>,
        refuse_catalog_writes: bool,
        throttle_catalog_writes: bool,
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
                before_copy_upload: Mutex::new(None),
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

        fn etag(&self, key: &str) -> std::result::Result<Option<ETag>, StoreError> {
            self.inner.etag(key)
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
}
