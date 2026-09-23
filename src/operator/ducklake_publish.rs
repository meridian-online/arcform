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
use std::io::{Read, Write};
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

/// Copy `from` to a new file `to`, returning the SHA-256 of the bytes written. One
/// read serves both, so the digest is of the copy itself and never of a file read at
/// another moment. `create_new`: a registered file is never overwritten.
fn copy_hashing(from: &Path, to: &Path) -> std::io::Result<String> {
    let mut reader = std::fs::File::open(from)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
    }
    file.sync_all()?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// The directory the copy goes into: `<data path>/<schema>/<table>/`, where DuckLake
/// keeps a table's own files. Refused when the data path is not local, because the
/// copy is a filesystem write.
fn table_dir(data_path: &str, schema: &str, table: &str) -> Result<PathBuf> {
    if has_scheme(data_path) {
        return Err(failed(format!(
            "the catalog's data path `{data_path}` is not a local directory; this version \
             copies the file with a filesystem write, so it publishes only into a catalog \
             whose data path is local"
        )));
    }
    Ok(Path::new(data_path).join(schema).join(table))
}

fn db(context: &str) -> impl Fn(duckdb::Error) -> Error + '_ {
    move |e| failed(format!("{context}: {e}"))
}

/// The last snapshot this operator committed for `table`, with the marker it wrote.
fn last_publish(conn: &duckdb::Connection, table: &str) -> Result<Option<(i64, Marker)>> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT snapshot_id, commit_extra_info FROM ducklake_snapshots('{LAKE}') \
             WHERE commit_extra_info IS NOT NULL ORDER BY snapshot_id DESC"
        ))
        .map_err(db("read the catalog's snapshots"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db("read the catalog's snapshots"))?;
    for row in rows {
        let (snapshot, info) = row.map_err(db("read the catalog's snapshots"))?;
        if let Ok(marker) = serde_json::from_str::<Marker>(&info)
            && marker.op == NAME
            && marker.table == table
        {
            return Ok(Some((snapshot, marker)));
        }
    }
    Ok(None)
}

/// Whether anything changed `schema.table` after `snapshot` — a write from outside
/// arc, for one. `ducklake_table_changes` refuses a range starting past the current
/// snapshot, so "nothing has been committed since" is answered first.
fn changed_since(
    conn: &duckdb::Connection,
    schema: &str,
    table: &str,
    snapshot: i64,
) -> Result<bool> {
    let current: i64 = conn
        .query_row(
            &format!("SELECT id::BIGINT FROM {LAKE}.current_snapshot()"),
            [],
            |row| row.get(0),
        )
        .map_err(db("read the current snapshot"))?;
    if snapshot >= current {
        return Ok(false);
    }
    let changes: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM ducklake_table_changes('{LAKE}', {}, {}, {}, {current})",
                sql_string_literal(schema),
                sql_string_literal(table),
                snapshot + 1,
            ),
            [],
            |row| row.get(0),
        )
        .map_err(db("read the table's changes"))?;
    Ok(changes > 0)
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
    tx.commit().map_err(db("commit the publish"))?;
    conn.query_row(
        &format!(
            "SELECT snapshot_id FROM ducklake_snapshots('{LAKE}') WHERE commit_extra_info = {}",
            sql_string_literal(&info)
        ),
        [],
        |row| row.get(0),
    )
    .map_err(db("find the snapshot the publish committed"))
}

/// Everything after the copy is placed: either an unchanged no-op or a registration.
fn publish_copy(
    conn: &mut duckdb::Connection,
    schema: &str,
    table: &str,
    marker: Marker,
) -> Result<Report> {
    if let Some((snapshot, last)) = last_publish(conn, &marker.table)?
        && last.sha256 == marker.sha256
        && !changed_since(conn, schema, table, snapshot)?
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
        conn.execute_batch("INSTALL ducklake; LOAD ducklake;")
            .map_err(db("load the ducklake extension"))?;
        if let Some(sql) = secret {
            conn.execute_batch(&sql)
                .map_err(db("create the credential's secret"))?;
        }
        conn.execute_batch(&cfg.attach_sql(ctx.dir))
            .map_err(db(&format!("attach the catalog `{}`", cfg.catalog)))?;
        let data_path: String = conn
            .query_row(
                &format!(
                    "SELECT value FROM __ducklake_metadata_{LAKE}.ducklake_metadata \
                     WHERE key = 'data_path'"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(db("read the catalog's data path"))?;

        let dir = table_dir(&data_path, schema, table)?;
        std::fs::create_dir_all(&dir)
            .map_err(|e| failed(format!("create {}: {e}", dir.display())))?;
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
