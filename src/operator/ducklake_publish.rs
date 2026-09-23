//! `ducklake_publish` — the step that ends a Protocol which publishes: register the
//! Parquet the Run built into a DuckLake catalog, as one snapshot.
//!
//! Publishing used to happen outside the Run, in a program that then had to prove
//! from run records that the bytes it uploaded came from a finished Run. As a step,
//! the proof is structural: the step reads the artefact the build step produced, so
//! it cannot run before that step succeeds, and it re-runs only when that artefact
//! was rebuilt. The run contract records what it did (see [`Report`]).
//!
//! # What a publish does
//!
//! 1. Refuses, naming the variables, when a declared `credential:` is not in the
//!    environment — before anything is attached or written.
//! 2. Attaches the catalog in a private in-memory DuckDB, so the pipeline's own
//!    database is never locked or written.
//! 3. Places a byte-for-byte copy of the built file under the catalog's data path,
//!    at a name no earlier registration used, hashing the bytes as they are written.
//! 4. If the last snapshot this operator committed for the table carries the same
//!    digest, and the table has not changed since, deletes the copy and reports that
//!    snapshot: an unchanged build is a **no-op**, never a second snapshot.
//! 5. Otherwise, in ONE transaction: stamps the commit with a marker naming the
//!    digest, creates the table from the file's schema if it does not exist, deletes
//!    every live row, and registers the copy with `ducklake_add_data_files`. The table
//!    then holds exactly the file's rows, as one snapshot, and time travel to any
//!    earlier snapshot still reads that snapshot's bytes.
//!
//! # Why a copy, and why not a re-encode
//!
//! `ducklake_add_data_files` registers a file where it lies; DuckLake reads its
//! footer and records its size, and from then on treats the file as its own and
//! immutable. A build step rewrites its output path in place, so registering the
//! build output directly corrupts the published version the moment the next build
//! runs — measured against DuckDB 1.5.5: after `COPY … TO` rewrote a registered
//! path, time travel to the snapshot that registered it failed with *"Parquet footer
//! length stored in file is not equal to footer length provided"*. And DuckLake's
//! maintenance (`expire_snapshots` then `cleanup_old_files`) may delete a registered
//! file, so the file registered must be one nothing else depends on. Both say the
//! same thing: register a copy. The copy is made with a filesystem copy, never
//! through DuckDB, so the registered bytes are the validated bytes — the report's
//! `sha256` is the digest of the registered object, taken while it was written.
//!
//! # What this version does not do
//!
//! The copy is a local filesystem write, so a catalog whose data path is object
//! storage (`s3://…`) is refused rather than half-supported. Uploading the copy is the
//! next increment, and so is the gate that decides whether a given Run may publish to
//! production: this operator publishes whenever it runs.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use sha2::{Digest, Sha256};

use super::{OpAssets, OpContext, Operator, sql_string_literal};
use crate::engine::StepOutput;
use crate::error::{Error, Result};

pub(super) struct DucklakePublish;

const NAME: &str = "ducklake_publish";

/// The alias the catalog is attached under, inside the operator's own in-memory
/// DuckDB — never visible to the Protocol's SQL.
const LAKE: &str = "arc_publish_lake";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DucklakePublishConfig {
    /// The built Parquet to publish, relative to the manifest directory. Declared as
    /// a read, so the step runs after whatever produces it and re-runs when it is
    /// rebuilt.
    file: String,
    /// The DuckLake catalog: a local catalog file (relative to the manifest
    /// directory), or a DuckLake connection string such as `postgres:dbname=lake`.
    /// A leading `ducklake:` is accepted and ignored.
    catalog: String,
    /// `DATA_PATH` for the attach. Omitted, DuckLake uses the path the catalog
    /// already records, or `<catalog>.files/` for a new file catalog.
    #[serde(default)]
    data_path: Option<String>,
    /// The table to publish into: `table` (schema `main`) or `schema.table`.
    table: String,
    /// What the attach needs to authenticate, if anything.
    #[serde(default)]
    credential: Option<CredentialConfig>,
}

/// A DuckDB secret whose values come from the environment. The manifest names the
/// variables and never holds a value.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialConfig {
    /// The DuckDB secret `TYPE`: `s3`, `r2`, `gcs`, `postgres`, …
    #[serde(rename = "type")]
    kind: String,
    /// Secret parameter → the environment variable holding its value, e.g.
    /// `key_id: R2_ACCESS_KEY_ID`.
    env: BTreeMap<String, String>,
}

fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::ManifestValidation(format!("{NAME}: {msg}"))
}

/// A run-time failure. The runner writes the step's name into it, so every refusal
/// this operator raises names the step without having to know it.
fn failed(msg: impl std::fmt::Display) -> Error {
    Error::StepFailed {
        step: String::new(),
        code: 1,
        stderr: format!("{NAME}: {msg}"),
    }
}

/// An ASCII identifier: what DuckDB accepts unquoted as a secret type or a secret
/// parameter name. Both are spliced into `CREATE SECRET`, so nothing else is let in.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `table` → `("main", "table")`, `schema.table` → `("schema", "table")`; anything
/// else — an empty part, or more than one dot — is `None`.
fn split_table(table: &str) -> Option<(&str, &str)> {
    let (schema, name) = table.split_once('.').unwrap_or(("main", table));
    let valid = |part: &str| !part.is_empty() && !part.contains('.');
    (valid(schema) && valid(name)).then_some((schema, name))
}

/// A DuckDB double-quoted identifier.
fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Whether `s` starts with a `scheme:` — a URL or a DuckLake connection string rather
/// than a filesystem path. Two characters at least, so a Windows drive letter is a
/// path.
fn has_scheme(s: &str) -> bool {
    s.split_once(':')
        .is_some_and(|(scheme, _)| scheme.len() > 1 && is_identifier(scheme))
}

/// A location from the manifest: a relative path resolves against the manifest
/// directory; an absolute path, a URL or a connection string passes through.
fn resolve_location(dir: &Path, location: &str) -> String {
    if has_scheme(location) || Path::new(location).is_absolute() {
        location.to_string()
    } else {
        dir.join(location).display().to_string()
    }
}

impl DucklakePublishConfig {
    fn parse(with: &Value) -> Result<Self> {
        let cfg: Self = serde_yaml::from_value(with.clone())
            .map_err(|e| invalid(format!("invalid `with:` config: {e}")))?;
        if split_table(&cfg.table).is_none() {
            return Err(invalid(format!(
                "`table: {}` must be `table` or `schema.table`",
                cfg.table
            )));
        }
        if let Some(credential) = &cfg.credential {
            credential.validate()?;
        }
        Ok(cfg)
    }

    /// The `ATTACH` statement. The catalog and data path are resolved against the
    /// manifest directory here, because DuckDB would resolve them against whatever
    /// directory `arc` happened to be started from.
    fn attach_sql(&self, dir: &Path) -> String {
        let catalog = self
            .catalog
            .strip_prefix("ducklake:")
            .unwrap_or(&self.catalog);
        let options = self
            .data_path
            .as_deref()
            .map(|p| {
                format!(
                    " (DATA_PATH {})",
                    sql_string_literal(&resolve_location(dir, p))
                )
            })
            .unwrap_or_default();
        format!(
            "ATTACH {} AS {LAKE}{options};",
            sql_string_literal(&format!("ducklake:{}", resolve_location(dir, catalog)))
        )
    }
}

impl CredentialConfig {
    fn validate(&self) -> Result<()> {
        if !is_identifier(&self.kind) {
            return Err(invalid(format!(
                "`credential.type: {}` is not a DuckDB secret type",
                self.kind
            )));
        }
        if self.env.is_empty() {
            return Err(invalid(
                "`credential.env` names no environment variables, so the credential \
                 could never hold anything",
            ));
        }
        if let Some(param) = self.env.keys().find(|p| !is_identifier(p)) {
            return Err(invalid(format!(
                "`credential.env` key `{param}` is not a secret parameter name"
            )));
        }
        Ok(())
    }

    /// Every declared parameter with its value, or — when any is unset or empty —
    /// the names of the variables that are missing, in manifest order.
    fn resolve(
        &self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> std::result::Result<Vec<(String, String)>, Vec<String>> {
        let mut values = Vec::new();
        let mut missing = Vec::new();
        for (param, var) in &self.env {
            match lookup(var).filter(|v| !v.is_empty()) {
                Some(value) => values.push((param.clone(), value)),
                None => missing.push(var.clone()),
            }
        }
        if missing.is_empty() {
            Ok(values)
        } else {
            Err(missing)
        }
    }

    /// `CREATE TEMPORARY SECRET` for the resolved values. Temporary: it lives in the
    /// operator's in-memory DuckDB and dies with it, so a value is never persisted.
    fn create_secret_sql(&self, values: &[(String, String)]) -> String {
        let params: String = values
            .iter()
            .map(|(param, value)| format!(", {param} {}", sql_string_literal(value)))
            .collect();
        format!(
            "CREATE TEMPORARY SECRET arc_ducklake_publish (TYPE {}{params});",
            self.kind
        )
    }
}

/// What a publish records, twice: as the commit's `extra_info` in the catalog, which
/// is how a later publish recognises bytes it already published, and as the step's
/// report in the run contract, with the snapshot id added.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Marker {
    op: String,
    /// `schema.table`.
    table: String,
    /// SHA-256 of the registered object, taken while it was written.
    sha256: String,
    /// The `file:` the bytes came from, as the manifest names it.
    source: String,
    /// The registered object's path.
    data_file: String,
}

/// The step's report: what `arc run` records under the step in the run contract.
#[derive(Debug, Serialize)]
struct Report {
    /// `registered` when this run committed a snapshot, `unchanged` when the bytes
    /// were already what the table holds and nothing was committed.
    action: &'static str,
    /// The snapshot holding these bytes — created by this run when `registered`.
    snapshot_id: i64,
    #[serde(flatten)]
    marker: Marker,
}

/// A writer that hashes exactly the bytes its inner writer accepted.
struct Hashing<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf).map(|n| {
            self.hasher.update(&buf[..n]);
            n
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Copy `from` to a new file `to`, returning the SHA-256 of the bytes written. One
/// read serves both, so the digest is of the copy itself and never of a file read at
/// another moment. `create_new`: a registered file is never overwritten.
fn copy_hashing(from: &Path, to: &Path) -> std::io::Result<String> {
    let mut reader = std::fs::File::open(from)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut writer = Hashing {
        inner: file,
        hasher: Sha256::new(),
    };
    std::io::copy(&mut reader, &mut writer)
        .and_then(|_| writer.inner.sync_all())
        .map(|()| format!("{:x}", writer.hasher.finalize()))
}

/// The directory the copy goes into, created: `<data path>/<schema>/<table>/`, where
/// DuckLake keeps a table's own files. Refused when the data path is not local,
/// because the copy is a filesystem write.
fn table_dir(data_path: &str, schema: &str, table: &str) -> Result<PathBuf> {
    if has_scheme(data_path) {
        return Err(failed(format!(
            "the catalog's data path `{data_path}` is not a local directory; this version \
             copies the file with a filesystem write, so it publishes only into a catalog \
             whose data path is local"
        )));
    }
    let dir = Path::new(data_path).join(schema).join(table);
    std::fs::create_dir_all(&dir)
        .map(|()| dir.clone())
        .map_err(|e| failed(format!("create {}: {e}", dir.display())))
}

fn db(context: &str) -> impl Fn(duckdb::Error) -> Error + '_ {
    move |e| failed(format!("{context}: {e}"))
}

/// What the catalog says about earlier publishes into one table. No `Default`: a
/// catalog that could not be read must never stand in for one with nothing in it.
struct Published {
    /// The catalog's current snapshot.
    current: i64,
    /// The last snapshot this operator committed for the table, with its marker.
    last: Option<(i64, Marker)>,
}

/// Read every snapshot once: the newest is the current one, and the newest carrying
/// this operator's marker for `table` is the last publish.
fn published(conn: &duckdb::Connection, table: &str) -> Result<Published> {
    conn.prepare(&format!(
        "SELECT snapshot_id, commit_extra_info FROM ducklake_snapshots('{LAKE}') \
         ORDER BY snapshot_id DESC"
    ))
    .and_then(|mut stmt| {
        stmt.query_map([], |row| {
            row.get::<_, i64>(0)
                .and_then(|id| row.get::<_, Option<String>>(1).map(|info| (id, info)))
        })
        .and_then(|rows| rows.collect::<duckdb::Result<Vec<_>>>())
    })
    .map(|snapshots| Published {
        current: snapshots.first().map_or(0, |(id, _)| *id),
        last: snapshots.into_iter().find_map(|(id, info)| {
            info.and_then(|info| serde_json::from_str::<Marker>(&info).ok())
                .filter(|marker| marker.op == NAME && marker.table == table)
                .map(|marker| (id, marker))
        }),
    })
    .map_err(db("read the catalog's snapshots"))
}

/// Whether `schema.table` still holds what `snapshot` committed. No `Default`, for
/// the reason [`Published`] has none: "could not tell" is not "unchanged".
#[derive(Debug, PartialEq)]
enum Since {
    Unchanged,
    Changed,
}

/// Whether anything changed `schema.table` after `snapshot` — a write or a drop from
/// outside arc. `ducklake_table_changes` refuses a range that starts past the current
/// snapshot, and a table that no longer exists at its end, so both are answered first.
fn since(
    conn: &duckdb::Connection,
    schema: &str,
    table: &str,
    snapshot: i64,
    current: i64,
) -> Result<Since> {
    if snapshot >= current {
        return Ok(Since::Unchanged);
    }
    let (schema, table) = (sql_string_literal(schema), sql_string_literal(table));
    conn.query_row(
        &format!(
            "SELECT count(*) > 0 FROM duckdb_tables() WHERE database_name = '{LAKE}' \
             AND schema_name = {schema} AND table_name = {table}"
        ),
        [],
        |row| row.get::<_, bool>(0),
    )
    .and_then(|exists| {
        if !exists {
            return Ok(Since::Changed);
        }
        conn.query_row(
            &format!(
                "SELECT count(*) FROM ducklake_table_changes('{LAKE}', {schema}, {table}, {}, \
                 {current})",
                snapshot + 1
            ),
            [],
            |row| {
                row.get::<_, i64>(0).map(|changes| {
                    if changes > 0 {
                        Since::Changed
                    } else {
                        Since::Unchanged
                    }
                })
            },
        )
    })
    .map_err(db("read the table's changes since the last publish"))
}

/// Register `marker.data_file` as the table's whole content, in one transaction, and
/// return the snapshot that commit created.
fn register(
    conn: &mut duckdb::Connection,
    schema: &str,
    table: &str,
    marker: &Marker,
) -> Result<i64> {
    let info = serde_json::to_string(marker).expect("a marker serializes");
    let target = format!("{LAKE}.{}.{}", quote_ident(schema), quote_ident(table));
    let file = sql_string_literal(&marker.data_file);
    let tx = conn
        .transaction()
        .map_err(db("begin the publish transaction"))?;
    tx.execute_batch(&format!(
        "CALL {LAKE}.set_commit_message('arcform', {message}, extra_info => {info});
         CREATE SCHEMA IF NOT EXISTS {LAKE}.{schema_ident};
         CREATE TABLE IF NOT EXISTS {target} AS FROM read_parquet({file}) LIMIT 0;
         DELETE FROM {target};
         CALL ducklake_add_data_files('{LAKE}', {table_lit}, {file}, schema => {schema_lit});",
        message = sql_string_literal(&format!("{NAME}: {} -> {}", marker.source, marker.table)),
        info = sql_string_literal(&info),
        schema_ident = quote_ident(schema),
        table_lit = sql_string_literal(table),
        schema_lit = sql_string_literal(schema),
    ))
    .map_err(db(&format!(
        "register {} into {}",
        marker.source, marker.table
    )))?;
    tx.commit()
        .and_then(|()| {
            conn.query_row(
                &format!(
                    "SELECT snapshot_id FROM ducklake_snapshots('{LAKE}') \
                     WHERE commit_extra_info = {}",
                    sql_string_literal(&info)
                ),
                [],
                |row| row.get(0),
            )
        })
        .map_err(db("commit the publish"))
}

/// Everything after the copy is placed: either an unchanged no-op or a registration.
fn publish_copy(
    conn: &mut duckdb::Connection,
    schema: &str,
    table: &str,
    marker: Marker,
) -> Result<Report> {
    let published = published(conn, &marker.table)?;
    if let Some((snapshot, last)) = published.last
        && last.sha256 == marker.sha256
        && since(conn, schema, table, snapshot, published.current)? == Since::Unchanged
    {
        return Ok(Report {
            action: "unchanged",
            snapshot_id: snapshot,
            marker: last,
        });
    }
    let snapshot = register(conn, schema, table, &marker)?;
    Ok(Report {
        action: "registered",
        snapshot_id: snapshot,
        marker,
    })
}

impl Operator for DucklakePublish {
    fn name(&self) -> &'static str {
        NAME
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = DucklakePublishConfig::parse(with)?;
        let mut assets = OpAssets::default();
        assets.record_reads(cfg.file.clone(), crate::asset_kind::AssetKind::File);
        Ok(assets)
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = DucklakePublishConfig::parse(with)?;
        let (schema, table) = split_table(&cfg.table).expect("parse validated the table");

        // The credential is checked before anything is attached or written: a publish
        // that cannot authenticate must fail here, naming what is missing, and not
        // halfway through with the copy placed.
        let secret = match &cfg.credential {
            None => None,
            Some(credential) => {
                let values = credential
                    .resolve(|var| {
                        ctx.env
                            .get(var)
                            .cloned()
                            .or_else(|| std::env::var(var).ok())
                    })
                    .map_err(|missing| {
                        failed(format!(
                            "no credential available — `credential.env` names {} but {} unset \
                             or empty in the environment; set {} and re-run",
                            missing.join(", "),
                            if missing.len() == 1 {
                                "it is"
                            } else {
                                "they are"
                            },
                            if missing.len() == 1 { "it" } else { "them" },
                        ))
                    })?;
                Some(credential.create_secret_sql(&values))
            }
        };

        let mut conn = duckdb::Connection::open_in_memory().map_err(db("open duckdb"))?;
        if let Some(sql) = secret {
            conn.execute_batch(&sql)
                .map_err(db("create the credential's secret"))?;
        }
        conn.execute_batch(&format!(
            "INSTALL ducklake; LOAD ducklake; {}",
            cfg.attach_sql(ctx.dir)
        ))
        .map_err(db(&format!("attach the catalog `{}`", cfg.catalog)))?;
        let dir = conn
            .query_row(
                &format!(
                    "SELECT value FROM __ducklake_metadata_{LAKE}.ducklake_metadata \
                     WHERE key = 'data_path'"
                ),
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(db("read the catalog's data path"))
            .and_then(|data_path| table_dir(&data_path, schema, table))?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let data_file = dir.join(format!("arc-publish-{nonce}.parquet"));
        let source = ctx.dir.join(&cfg.file);
        let sha256 = copy_hashing(&source, &data_file).map_err(|e| {
            let _ = std::fs::remove_file(&data_file);
            failed(format!("copy {} into the catalog: {e}", cfg.file))
        })?;

        let marker = Marker {
            op: NAME.to_string(),
            table: format!("{schema}.{table}"),
            sha256,
            source: cfg.file.clone(),
            data_file: data_file.display().to_string(),
        };
        // The copy is the catalog's only when a snapshot references it; on a no-op or
        // a failure nothing does, so it is removed rather than left for DuckLake's
        // orphan cleanup to find.
        let report = publish_copy(&mut conn, schema, table, marker).inspect_err(|_| {
            let _ = std::fs::remove_file(&data_file);
        })?;
        if report.action == "unchanged" {
            let _ = std::fs::remove_file(&data_file);
        }
        eprintln!(
            "{NAME}: {} {} into {} — snapshot {} (sha256 {})",
            report.action,
            report.marker.source,
            report.marker.table,
            report.snapshot_id,
            report.marker.sha256,
        );
        Ok(StepOutput {
            stderr: String::new(),
            stdout: None,
            report: Some(serde_json::to_value(&report).expect("a report serializes")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const PUBLISH: &str = "file: build/out.parquet\ncatalog: lake.ducklake\ntable: t\n";

    fn write_parquet(path: &Path, select: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY ({select}) TO {} (FORMAT parquet);",
            sql_string_literal(&path.display().to_string())
        ))
        .unwrap();
    }

    fn build(dir: &Path, select: &str) {
        write_parquet(&dir.join("build/out.parquet"), select);
    }

    fn with(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    fn publish_with(
        dir: &Path,
        yaml: &str,
        env: &HashMap<String, String>,
    ) -> Result<serde_json::Value> {
        let ctx = OpContext {
            dir,
            db_path: dir,
            env,
            timeout: None,
            cache: None,
        };
        DucklakePublish
            .run(&with(yaml), &ctx)
            .map(|out| out.report.expect("a publish reports what it did"))
    }

    fn publish(dir: &Path, yaml: &str) -> serde_json::Value {
        publish_with(dir, yaml, &HashMap::new()).unwrap()
    }

    /// Read the catalog through a connection of the test's own, closed again before
    /// the next publish opens it.
    fn lake<T>(dir: &Path, read: impl FnOnce(&duckdb::Connection) -> T) -> T {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "LOAD ducklake; ATTACH {} AS lake;",
            sql_string_literal(&format!("ducklake:{}", dir.join("lake.ducklake").display()))
        ))
        .unwrap();
        read(&conn)
    }

    fn snapshots(dir: &Path) -> i64 {
        lake(dir, |c| {
            c.query_row("SELECT count(*) FROM ducklake_snapshots('lake')", [], |r| {
                r.get(0)
            })
            .unwrap()
        })
    }

    fn ids(conn: &duckdb::Connection, sql: &str) -> Vec<i64> {
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn rows(dir: &Path, table: &str) -> Vec<i64> {
        lake(dir, |c| {
            ids(c, &format!("SELECT id FROM lake.{table} ORDER BY id"))
        })
    }

    /// The copies this operator has placed for `main.t` — every one a snapshot
    /// references, and any it failed to clean up.
    fn copies(dir: &Path) -> Vec<PathBuf> {
        let table_dir = dir.join("lake.ducklake.files/main/t");
        let Ok(entries) = std::fs::read_dir(&table_dir) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
        out.sort();
        out
    }

    fn hash(path: &Path) -> String {
        crate::fetch_cache::hash_file(path).unwrap()
    }

    #[test]
    fn a_publish_registers_a_byte_identical_copy_and_reports_its_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        let report = publish(dir.path(), PUBLISH);

        assert_eq!(report["action"], "registered");
        let current: i64 = lake(dir.path(), |c| {
            c.query_row(
                "SELECT max(snapshot_id) FROM ducklake_snapshots('lake')",
                [],
                |r| r.get(0),
            )
            .unwrap()
        });
        assert_eq!(
            report["snapshot_id"], current,
            "the report names the snapshot the publish created"
        );

        let registered: Vec<String> = lake(dir.path(), |c| {
            let mut stmt = c
                .prepare("SELECT data_file FROM ducklake_list_files('lake', 't')")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        });
        assert_eq!(
            registered,
            vec![report["data_file"].as_str().unwrap().to_string()]
        );
        let built = dir.path().join("build/out.parquet");
        let object = Path::new(&registered[0]);
        assert_ne!(
            object,
            built.as_path(),
            "the build output itself is never registered"
        );
        assert_eq!(
            hash(object),
            hash(&built),
            "the registered object holds the built bytes"
        );
        assert_eq!(report["sha256"], hash(&built));
        assert_eq!(report["table"], "main.t");
        assert_eq!(report["source"], "build/out.parquet");
        assert_eq!(rows(dir.path(), "t"), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn an_unchanged_build_is_a_no_op_that_reports_the_snapshot_holding_it() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        let first = publish(dir.path(), PUBLISH);
        let before = snapshots(dir.path());

        let second = publish(dir.path(), PUBLISH);
        assert_eq!(second["action"], "unchanged");
        assert_eq!(second["snapshot_id"], first["snapshot_id"]);
        assert_eq!(second["data_file"], first["data_file"]);
        assert_eq!(
            snapshots(dir.path()),
            before,
            "an unchanged build commits nothing"
        );
        assert_eq!(
            rows(dir.path(), "t"),
            vec![0, 1, 2, 3, 4],
            "and the rows are not doubled"
        );
        // Compared by name: DuckLake records the data path canonicalised, and a
        // macOS temp directory is reached through a symlink.
        let names: Vec<_> = copies(dir.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                Path::new(first["data_file"].as_str().unwrap())
                    .file_name()
                    .unwrap()
                    .to_owned()
            ],
            "the no-op's own copy is removed, not left for orphan cleanup"
        );
    }

    #[test]
    fn a_publish_to_another_table_in_between_does_not_make_an_unchanged_build_publish_again() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        write_parquet(&dir.path().join("build/other.parquet"), "SELECT 9 AS id");
        let first = publish(dir.path(), PUBLISH);
        publish(
            dir.path(),
            "file: build/other.parquet\ncatalog: lake.ducklake\ntable: other\n",
        );
        let before = snapshots(dir.path());

        let again = publish(dir.path(), PUBLISH);
        assert_eq!(again["action"], "unchanged");
        assert_eq!(again["snapshot_id"], first["snapshot_id"]);
        assert_eq!(snapshots(dir.path()), before);
    }

    #[test]
    fn a_write_from_outside_arc_makes_an_unchanged_build_publish_again() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        let first = publish(dir.path(), PUBLISH);
        lake(dir.path(), |c| {
            c.execute_batch("INSERT INTO lake.t VALUES (99);").unwrap()
        });

        let again = publish(dir.path(), PUBLISH);
        assert_eq!(again["action"], "registered");
        assert!(again["snapshot_id"].as_i64() > first["snapshot_id"].as_i64());
        assert_eq!(
            rows(dir.path(), "t"),
            vec![0, 1, 2, 3, 4],
            "the stray row is replaced"
        );
    }

    #[test]
    fn a_rebuild_replaces_the_rows_and_history_still_reads_the_old_bytes() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        let first = publish(dir.path(), PUBLISH);
        // The build rewrites its output path in place, as `parquet_export` does.
        build(dir.path(), "SELECT range + 100 AS id FROM range(3)");

        let second = publish(dir.path(), PUBLISH);
        assert_eq!(second["action"], "registered");
        assert_ne!(second["sha256"], first["sha256"]);
        assert_eq!(rows(dir.path(), "t"), vec![100, 101, 102]);
        let then = first["snapshot_id"].as_i64().unwrap();
        let old = lake(dir.path(), |c| {
            ids(
                c,
                &format!("SELECT id FROM lake.t AT (VERSION => {then}) ORDER BY id"),
            )
        });
        assert_eq!(
            old,
            vec![0, 1, 2, 3, 4],
            "the first snapshot still reads its own bytes"
        );
        assert_eq!(
            hash(Path::new(first["data_file"].as_str().unwrap())),
            first["sha256"]
        );
    }

    #[test]
    fn a_declared_credential_missing_from_the_environment_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        let yaml = format!(
            "{PUBLISH}credential:\n  type: s3\n  env:\n    key_id: ARC_TEST_DUCKLAKE_KEY_ID_UNSET\n    secret: ARC_TEST_DUCKLAKE_SECRET_UNSET\n"
        );

        let err = publish_with(dir.path(), &yaml, &HashMap::new()).unwrap_err();
        let Error::StepFailed { stderr, .. } = &err else {
            panic!("expected a step failure, got {err:?}")
        };
        assert!(stderr.contains("no credential available"), "{stderr}");
        assert!(
            stderr.contains("ARC_TEST_DUCKLAKE_KEY_ID_UNSET, ARC_TEST_DUCKLAKE_SECRET_UNSET"),
            "{stderr}"
        );
        assert!(
            !dir.path().join("lake.ducklake").exists(),
            "nothing is attached before the refusal"
        );

        // One supplied, one still missing: the refusal names only the one missing.
        let mut env = HashMap::new();
        env.insert(
            "ARC_TEST_DUCKLAKE_KEY_ID_UNSET".to_string(),
            "id".to_string(),
        );
        env.insert("ARC_TEST_DUCKLAKE_SECRET_UNSET".to_string(), String::new());
        let err = publish_with(dir.path(), &yaml, &env)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("names ARC_TEST_DUCKLAKE_SECRET_UNSET but it is unset or empty"),
            "{err}"
        );

        env.insert(
            "ARC_TEST_DUCKLAKE_SECRET_UNSET".to_string(),
            "secret".to_string(),
        );
        let report = publish_with(dir.path(), &yaml, &env).unwrap();
        assert_eq!(report["action"], "registered");
    }

    #[test]
    fn a_credential_the_database_rejects_fails_the_publish() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT 1 AS id");
        let yaml = format!("{PUBLISH}credential:\n  type: s3\n  env:\n    not_a_parameter: V\n");
        let mut env = HashMap::new();
        env.insert("V".to_string(), "x".to_string());
        let err = publish_with(dir.path(), &yaml, &env)
            .unwrap_err()
            .to_string();
        assert!(err.contains("create the credential's secret"), "{err}");
    }

    #[test]
    fn a_failed_registration_leaves_no_copy_and_no_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        lake(dir.path(), |c| {
            c.execute_batch("CREATE TABLE lake.t (name VARCHAR);")
                .unwrap()
        });
        let before = snapshots(dir.path());

        let err = publish_with(dir.path(), PUBLISH, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("register build/out.parquet into main.t"),
            "{err}"
        );
        assert_eq!(snapshots(dir.path()), before);
        assert_eq!(copies(dir.path()), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_missing_build_output_fails_and_leaves_no_copy() {
        let dir = tempfile::tempdir().unwrap();
        let err = publish_with(dir.path(), PUBLISH, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("copy build/out.parquet into the catalog"),
            "{err}"
        );
        assert_eq!(copies(dir.path()), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_data_path_on_object_storage_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT 1 AS id");
        let yaml = format!("{PUBLISH}data_path: s3://bucket/lake/\n");
        let err = publish_with(dir.path(), &yaml, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("`s3://bucket/lake/` is not a local directory"),
            "{err}"
        );
        let local = dir.path().join("files");
        let made = table_dir(&local.display().to_string(), "main", "t").unwrap();
        assert_eq!(made, local.join("main/t"));
        assert!(made.is_dir(), "the table's directory is created");
    }

    #[test]
    fn a_catalog_that_cannot_be_attached_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT 1 AS id");
        let yaml = "file: build/out.parquet\ncatalog: no/such/dir/lake.ducklake\ntable: t\n";
        let err = publish_with(dir.path(), yaml, &HashMap::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("attach the catalog `no/such/dir/lake.ducklake`"),
            "{err}"
        );
    }

    #[test]
    fn a_table_dropped_outside_arc_is_published_again() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT range AS id FROM range(5)");
        publish(dir.path(), PUBLISH);
        lake(dir.path(), |c| {
            c.execute_batch("DROP TABLE lake.t;").unwrap()
        });

        let again = publish(dir.path(), PUBLISH);
        assert_eq!(again["action"], "registered");
        assert_eq!(rows(dir.path(), "t"), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_schema_qualified_table_and_an_explicit_data_path_are_honoured() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), "SELECT 7 AS id");
        let report = publish(
            dir.path(),
            "file: build/out.parquet\ncatalog: ducklake:lake.ducklake\ndata_path: files/\ntable: pub.t\n",
        );
        assert_eq!(report["table"], "pub.t");
        let expected = dir.path().join("files/pub/t");
        assert!(
            Path::new(report["data_file"].as_str().unwrap()).starts_with(&expected),
            "{report}"
        );
        assert_eq!(
            lake(dir.path(), |c| ids(c, "SELECT id FROM lake.pub.t")),
            vec![7]
        );
    }

    #[test]
    fn the_file_is_declared_as_a_read_and_nothing_as_produced() {
        let assets = DucklakePublish.assets(&with(PUBLISH)).unwrap();
        assert_eq!(assets.reads, vec!["build/out.parquet".to_string()]);
        assert!(assets.produces.is_empty());
        assert_eq!(
            assets.kinds.get("build/out.parquet"),
            Some(&crate::asset_kind::AssetKind::File)
        );
    }

    #[test]
    fn a_table_is_bare_or_schema_qualified_and_nothing_else() {
        assert_eq!(split_table("t"), Some(("main", "t")));
        assert_eq!(split_table("s.t"), Some(("s", "t")));
        for bad in ["", "s.", ".t", "a.b.c"] {
            assert_eq!(split_table(bad), None, "{bad:?}");
            let yaml = format!("file: f.parquet\ncatalog: c\ntable: '{bad}'\n");
            let err = DucklakePublishConfig::parse(&with(&yaml))
                .err()
                .unwrap()
                .to_string();
            assert!(
                err.contains("must be `table` or `schema.table`"),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn identifiers_and_schemes_are_told_apart_from_paths() {
        assert!(is_identifier("key_id") && is_identifier("_x") && is_identifier("s3"));
        assert!(!is_identifier("") && !is_identifier("3s") && !is_identifier("a-b"));
        assert!(!is_identifier("a b") && !is_identifier("a'"));
        assert!(has_scheme("postgres:dbname=lake") && has_scheme("s3://bucket/x"));
        assert!(!has_scheme("build/lake.ducklake") && !has_scheme("C:/lake"));
        assert!(!has_scheme(":x") && !has_scheme("a b:c"));
        let dir = Path::new("/p");
        assert_eq!(resolve_location(dir, "lake.ducklake"), "/p/lake.ducklake");
        assert_eq!(
            resolve_location(dir, "/abs/lake.ducklake"),
            "/abs/lake.ducklake"
        );
        assert_eq!(
            resolve_location(dir, "postgres:dbname=lake"),
            "postgres:dbname=lake"
        );
    }

    #[test]
    fn the_attach_resolves_against_the_manifest_directory() {
        let dir = Path::new("/p");
        let parse = |yaml: &str| DucklakePublishConfig::parse(&with(yaml)).unwrap();
        assert_eq!(
            parse(PUBLISH).attach_sql(dir),
            "ATTACH 'ducklake:/p/lake.ducklake' AS arc_publish_lake;"
        );
        assert_eq!(
            parse("file: f\ncatalog: ducklake:postgres:dbname=l\ndata_path: d/\ntable: t\n")
                .attach_sql(dir),
            "ATTACH 'ducklake:postgres:dbname=l' AS arc_publish_lake (DATA_PATH '/p/d/');"
        );
    }

    #[test]
    fn a_credential_names_a_type_and_at_least_one_parameter() {
        let parse = |credential: &str| {
            DucklakePublishConfig::parse(&with(&format!("{PUBLISH}credential:\n{credential}")))
                .err()
                .map(|e| e.to_string())
        };
        let err = parse("  type: \"s3; DROP\"\n  env: { key_id: K }\n").unwrap();
        assert!(err.contains("is not a DuckDB secret type"), "{err}");
        let err = parse("  type: s3\n  env: {}\n").unwrap();
        assert!(err.contains("names no environment variables"), "{err}");
        let err = parse("  type: s3\n  env: { \"key id\": K }\n").unwrap();
        assert!(
            err.contains("key `key id` is not a secret parameter name"),
            "{err}"
        );
        assert_eq!(parse("  type: s3\n  env: { key_id: K }\n"), None);
    }

    #[test]
    fn the_secret_is_temporary_and_holds_the_resolved_values() {
        let credential = CredentialConfig {
            kind: "s3".to_string(),
            env: BTreeMap::from([
                ("key_id".to_string(), "K".to_string()),
                ("secret".to_string(), "S".to_string()),
            ]),
        };
        let values = credential.resolve(|var| Some(format!("{var}'v"))).unwrap();
        assert_eq!(
            credential.create_secret_sql(&values),
            "CREATE TEMPORARY SECRET arc_ducklake_publish (TYPE s3, key_id 'K''v', secret 'S''v');"
        );
        assert_eq!(
            credential.resolve(|var| (var == "S").then(|| "x".to_string())),
            Err(vec!["K".to_string()])
        );
    }
}
