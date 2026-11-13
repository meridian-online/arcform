//! `ducklake_write` — the step that ends a Protocol by publishing its tables into a
//! DuckLake catalogue held in object storage, as one snapshot.
//!
//! DuckLake writes the data: each table is upserted on its key, so a snapshot records
//! only the rows that changed and an unchanged rebuild commits nothing. Each table's
//! Frictionless descriptor goes into the lake's `meta` schema in the same snapshot.
//! The building blocks are in [`crate::lake`]; this module is the step around them.
//!
//! # Live or local
//!
//! The write credential comes from the step's environment: the manifest's `dotenv:`
//! files, then the process environment. With every variable set, the step publishes
//! into the live lake. With any unset, it publishes into a lake of its own at
//! `local_catalog`, whose data sits beside it, and the run stays green, saying which
//! variables the live lake needs. `arc run --publish` (which sets
//! `ARC_PUBLISH=required`) makes a missing credential fail the step instead.
//!
//! A live publish also queues the run's record for upload to `<runs><run_id>.json`
//! (`runs` defaults to `runs/`) once the run ends (see [`crate::lake::queue_run_record_upload`]); the snapshot's
//! commit metadata names that address.
//!
//! No credential value ever reaches a report, a log line or an error: values are
//! only spliced into the DuckDB secret statement, whose own errors are not passed on,
//! and the object-store client, whose errors name only keys.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_yaml::Value;
use sha2::{Digest, Sha256};

use super::{OpAssets, OpContext, Operator, sql_string_literal};
use crate::asset_kind::AssetKind;
use crate::engine::StepOutput;
use crate::error::{Error, Result};
use crate::lake::commit::{CommitOutcome, TableChange, TableInput};
use crate::lake::manifest::{ManifestEntry, update_manifest};
use crate::lake::meta::{META_SCHEMA, table_descriptor};
use crate::lake::publish::{
    ExtraFile, Lake, Published, UploadedObject, Watch, publish, publish_with_landing,
};
use crate::lake::store::{DirStore, ObjectStore, S3Store};
use crate::runner::{PUBLISH_ENV, RUN_ID_ENV};

pub(super) struct DucklakeWrite;

const NAME: &str = "ducklake_write";

/// How many times a publish, or a manifest write, is tried when another writer wins.
const MAX_ATTEMPTS: u32 = 5;

#[derive(Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(deny_unknown_fields)]
pub(super) struct DucklakeWriteConfig {
    /// The bucket's public base address, ending in `/`.
    public_base: String,
    /// The catalogue's key under `public_base`.
    catalog: String,
    /// The data files' key prefix; the persisted data path is `public_base + data_path`.
    data_path: String,
    /// The local fallback catalogue, relative to the Protocol.
    local_catalog: String,
    tables: Vec<TableConfig>,
    #[serde(default)]
    files: Vec<FileConfig>,
    #[serde(default)]
    manifest: Option<String>,
    /// The key prefix, ending in `/`, the run record is uploaded under.
    #[serde(default = "default_runs")]
    runs: String,
    credential: CredentialConfig,
}

fn default_runs() -> String {
    "runs/".into()
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(deny_unknown_fields)]
struct TableConfig {
    /// `table` (schema `main`) or `schema.table`.
    table: String,
    /// The Parquet file whose rows the table takes, relative to the Protocol.
    from: String,
    /// The columns rows are matched on. Omitted: the descriptor's `primaryKey`.
    #[serde(default)]
    key: Option<Vec<String>>,
    /// The table's Frictionless descriptor, relative to the Protocol.
    descriptor: String,
    /// The key the readable download copy of `from` is uploaded to.
    #[serde(default)]
    readable_copy: Option<String>,
    #[serde(default)]
    on_schema_change: OnSchemaChange,
    /// Further keys for the table's entry in the compatible manifest.
    #[serde(default)]
    manifest_extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
#[serde(rename_all = "lowercase")]
enum OnSchemaChange {
    #[default]
    Fail,
    Replace,
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(deny_unknown_fields)]
struct FileConfig {
    /// A file, or a directory ending in `/`, relative to the Protocol.
    from: String,
    /// The key, or for a directory a key prefix ending in `/`.
    to: String,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(Debug))]
#[serde(rename_all = "lowercase")]
enum CredentialKind {
    R2,
    S3,
}

impl CredentialKind {
    /// The URL scheme DuckDB reads and writes this kind of store through, and the
    /// scope of its secret.
    fn scheme(self) -> &'static str {
        match self {
            CredentialKind::R2 => "r2",
            CredentialKind::S3 => "s3",
        }
    }
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(deny_unknown_fields)]
struct CredentialConfig {
    #[serde(rename = "type")]
    kind: CredentialKind,
    bucket: String,
    env: CredentialEnv,
}

/// Each secret parameter's environment variable. The manifest names variables and
/// never holds a value.
#[derive(Deserialize)]
#[cfg_attr(test, derive(Debug))]
#[serde(deny_unknown_fields)]
struct CredentialEnv {
    key_id: String,
    secret: String,
    /// `r2` only.
    #[serde(default)]
    account_id: Option<String>,
    /// `s3` only: the endpoint's URL.
    #[serde(default)]
    endpoint: Option<String>,
    /// `s3` only.
    #[serde(default)]
    region: Option<String>,
}

/// The credential's values, resolved from the environment.
#[cfg_attr(test, derive(Clone))]
struct Resolved {
    kind: CredentialKind,
    bucket: String,
    key_id: String,
    secret: String,
    /// The account id (`r2`) or the endpoint URL (`s3`).
    location: String,
    /// `s3` only.
    region: String,
}

fn invalid(msg: impl std::fmt::Display) -> Error {
    Error::ManifestValidation(format!("{NAME}: {msg}"))
}

/// A run-time failure; the runner writes the step's name into it.
fn failed(msg: impl std::fmt::Display) -> Error {
    Error::StepFailed {
        step: String::new(),
        code: 1,
        stderr: format!("{NAME}: {msg}"),
    }
}

/// A store key: relative, with no `..` part, no backslash and no scheme.
fn check_key(field: &str, key: &str) -> Result<()> {
    let unsafe_key = key.is_empty()
        || key.starts_with('/')
        || key.contains('\\')
        || key.contains("://")
        || key.split('/').any(|part| part == "..");
    if unsafe_key {
        return Err(invalid(format!(
            "`{field}: {key}` must be a relative key, without `..`, under the lake's public base"
        )));
    }
    Ok(())
}

/// https, or plain http to this machine (for tests), ending in `/`.
fn check_public_base(base: &str) -> Result<()> {
    let refuse = || {
        invalid(format!(
            "`public_base: {base}` must be an https address ending in `/` (plain http only \
             for 127.0.0.1 or localhost)"
        ))
    };
    let url = url::Url::parse(base).map_err(|_| refuse())?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost"));
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && local);
    if !scheme_ok
        || !base.ends_with('/')
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(refuse());
    }
    Ok(())
}

/// `schema.table` → its parts; one part is a table in `main`.
fn split_table(table: &str) -> Option<(&str, &str)> {
    let (schema, name) = table.split_once('.').unwrap_or(("main", table));
    let valid = |part: &str| !part.is_empty() && !part.contains('.');
    (valid(schema) && valid(name)).then_some((schema, name))
}

impl DucklakeWriteConfig {
    fn parse(with: &Value) -> Result<Self> {
        let cfg: Self = serde_yaml::from_value(with.clone())
            .map_err(|e| invalid(format!("invalid `with:` config: {e}")))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        check_public_base(&self.public_base)?;
        check_key("catalog", &self.catalog)?;
        check_key("data_path", &self.data_path)?;
        check_key("runs", &self.runs)?;
        if !self.runs.ends_with('/') {
            return Err(invalid(format!(
                "`runs: {}` must be a key prefix ending in `/`",
                self.runs
            )));
        }
        if let Some(manifest) = &self.manifest {
            check_key("manifest", manifest)?;
        }
        if Path::new(&self.local_catalog).file_name().is_none() {
            return Err(invalid(format!(
                "`local_catalog: {}` must name a catalogue file",
                self.local_catalog
            )));
        }
        if self.tables.is_empty() {
            return Err(invalid("`tables` names no table to publish"));
        }
        for t in &self.tables {
            let (schema, _) = split_table(&t.table).ok_or_else(|| {
                invalid(format!(
                    "`table: {}` must be `table` or `schema.table`",
                    t.table
                ))
            })?;
            if schema.eq_ignore_ascii_case(META_SCHEMA) {
                return Err(invalid(format!(
                    "`table: {}` is in the schema `{META_SCHEMA}`, which is reserved for the \
                     lake's descriptors",
                    t.table
                )));
            }
            if let Some(copy) = &t.readable_copy {
                check_key("readable_copy", copy)?;
            }
        }
        for f in &self.files {
            check_key("files.to", &f.to)?;
        }
        self.credential.validate()
    }

    /// The key the run record is uploaded to.
    fn record_key(&self, run_id: &str) -> String {
        format!("{}{run_id}.json", self.runs)
    }

    /// The data prefix, ending in `/`.
    fn data_prefix(&self) -> String {
        let mut prefix = self.data_path.clone();
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        prefix
    }

    /// The live lake, in the credential's bucket. The write path's scheme is the
    /// credential's type, since DuckDB applies a secret only within its scope: an
    /// `r2` secret covers `r2://` and an `s3` secret `s3://`.
    fn live_lake(&self, credential: &Resolved) -> Lake {
        let data_prefix = self.data_prefix();
        Lake {
            catalog_key: self.catalog.clone(),
            data_path: format!("{}{data_prefix}", self.public_base),
            write_path: format!(
                "{}://{}/{data_prefix}",
                credential.kind.scheme(),
                credential.bucket
            ),
            data_prefix,
            pinned_prefix: Some(pinned_prefix(&self.catalog)),
        }
    }
}

/// `catalog/open.ducklake` → `catalog/open-`.
fn pinned_prefix(catalog: &str) -> String {
    format!("{}-", catalog.strip_suffix(".ducklake").unwrap_or(catalog))
}

impl CredentialConfig {
    fn validate(&self) -> Result<()> {
        if self.bucket.is_empty() {
            return Err(invalid("`credential.bucket` is empty"));
        }
        let needs: &[(&str, &Option<String>)] = match self.kind {
            CredentialKind::R2 => &[("account_id", &self.env.account_id)],
            CredentialKind::S3 => &[
                ("endpoint", &self.env.endpoint),
                ("region", &self.env.region),
            ],
        };
        for (param, var) in needs {
            if var.as_deref().is_none_or(str::is_empty) {
                return Err(invalid(format!(
                    "`credential.env.{param}` must name a variable for a credential of type `{}`",
                    self.kind.scheme()
                )));
            }
        }
        if self.env.key_id.is_empty() || self.env.secret.is_empty() {
            return Err(invalid("`credential.env` names an empty variable"));
        }
        Ok(())
    }

    /// The variables this credential reads, in manifest order.
    fn variables(&self) -> Vec<&str> {
        let mut vars = vec![self.env.key_id.as_str(), self.env.secret.as_str()];
        match self.kind {
            CredentialKind::R2 => vars.extend(self.env.account_id.as_deref()),
            CredentialKind::S3 => {
                vars.extend(self.env.endpoint.as_deref());
                vars.extend(self.env.region.as_deref());
            }
        }
        vars
    }

    /// Every value, or the names of the variables unset or empty.
    fn resolve(
        &self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> std::result::Result<Resolved, Vec<String>> {
        let vars = self.variables();
        let mut values = Vec::with_capacity(vars.len());
        let mut missing = Vec::new();
        for var in &vars {
            match lookup(var).filter(|v| !v.is_empty()) {
                Some(v) => values.push(v),
                None => missing.push((*var).to_string()),
            }
        }
        if !missing.is_empty() {
            return Err(missing);
        }
        let mut values = values.into_iter();
        let mut next = || values.next().expect("one value per variable");
        let (key_id, secret, location) = (next(), next(), next());
        let region = match self.kind {
            CredentialKind::R2 => "auto".to_string(),
            CredentialKind::S3 => next(),
        };
        Ok(Resolved {
            kind: self.kind,
            bucket: self.bucket.clone(),
            key_id,
            secret,
            location,
            region,
        })
    }
}

impl Resolved {
    fn store(&self) -> Result<S3Store> {
        match self.kind {
            CredentialKind::R2 => {
                S3Store::r2(&self.location, &self.bucket, &self.key_id, &self.secret)
            }
            CredentialKind::S3 => S3Store::new(
                &self.location,
                &self.bucket,
                &self.region,
                &self.key_id,
                &self.secret,
            ),
        }
        .map_err(|e| failed(format!("the object store: {e}")))
    }

    /// The DuckDB secret letting DuckLake read and write the lake's data through
    /// the write path (`r2://` or `s3://`). Temporary: it dies with the commit's in-memory DuckDB.
    fn secret_sql(&self) -> String {
        let common = format!(
            "KEY_ID {}, SECRET {}",
            sql_string_literal(&self.key_id),
            sql_string_literal(&self.secret)
        );
        match self.kind {
            CredentialKind::R2 => format!(
                "CREATE TEMPORARY SECRET arc_ducklake_write (TYPE r2, {common}, ACCOUNT_ID {});",
                sql_string_literal(&self.location)
            ),
            CredentialKind::S3 => {
                let (host, ssl) = match self.location.split_once("://") {
                    Some(("http", rest)) => (rest, false),
                    Some((_, rest)) => (rest, true),
                    None => (self.location.as_str(), true),
                };
                format!(
                    "CREATE TEMPORARY SECRET arc_ducklake_write (TYPE s3, {common}, ENDPOINT {}, \
                     REGION {}, URL_STYLE 'path', USE_SSL {ssl});",
                    sql_string_literal(host.trim_end_matches('/')),
                    sql_string_literal(&self.region)
                )
            }
        }
    }
}

/// One table, read and checked, ready to commit.
struct Prepared {
    input: TableInput,
    /// The input's row count.
    rows: u64,
}

/// A Parquet file's column names and row count.
fn parquet_shape(path: &Path) -> Result<(Vec<String>, u64)> {
    let context = |what: &str| {
        let what = format!("{what} {}", path.display());
        move |e: duckdb::Error| failed(format!("{what}: {e}"))
    };
    let conn = duckdb::Connection::open_in_memory().map_err(context("open duckdb for"))?;
    let file = sql_string_literal(&path.display().to_string());
    let mut stmt = conn
        .prepare(&format!("DESCRIBE SELECT * FROM read_parquet({file})"))
        .map_err(context("read the columns of"))?;
    let columns = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .and_then(|rows| rows.collect::<duckdb::Result<Vec<_>>>())
        .map_err(context("read the columns of"))?;
    let rows: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM read_parquet({file})"),
            [],
            |r| r.get(0),
        )
        .map_err(context("count the rows of"))?;
    Ok((columns, u64::try_from(rows).unwrap_or(0)))
}

fn prepare(dir: &Path, t: &TableConfig) -> Result<Prepared> {
    let parquet = dir.join(&t.from);
    if !parquet.is_file() {
        return Err(failed(format!(
            "`{}`, the input of table '{}', does not exist",
            t.from, t.table
        )));
    }
    let descriptor_path = dir.join(&t.descriptor);
    let descriptor: serde_json::Value = std::fs::read(&descriptor_path)
        .map_err(|e| failed(format!("read the descriptor `{}`: {e}", t.descriptor)))
        .and_then(|bytes| {
            serde_json::from_slice(&bytes).map_err(|e| {
                failed(format!(
                    "the descriptor `{}` is not JSON: {e}",
                    t.descriptor
                ))
            })
        })?;
    let (columns, rows) = parquet_shape(&parquet)?;
    let descriptor = table_descriptor(&t.table, &descriptor, &columns)?;
    let key = t.key.clone().unwrap_or_else(|| descriptor.key.clone());
    Ok(Prepared {
        input: TableInput {
            table: t.table.clone(),
            parquet,
            descriptor,
            key,
            replace_on_schema_change: t.on_schema_change == OnSchemaChange::Replace,
        },
        rows,
    })
}

/// A private working folder for one publish, removed when dropped.
struct Work(PathBuf);

impl Work {
    fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("arc-ducklake-write-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path)
            .map_err(|e| failed(format!("create {}: {e}", path.display())))?;
        Ok(Work(path))
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the step reports, in the run record.
#[derive(Serialize)]
struct Report {
    /// `live` or `local`.
    target: &'static str,
    snapshot_id: i64,
    committed: bool,
    attempts: u32,
    tables: Vec<TableChange>,
    uploaded: Vec<UploadedObject>,
    /// Readable copies and extra files not uploaded because the object already held
    /// their bytes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unchanged: Vec<String>,
    /// Readable copies and extra files skipped because a newer snapshot changed their
    /// table.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    superseded: Vec<String>,
    /// The data and delete files this snapshot added, by store key.
    data_files: Vec<String>,
    pinned_catalog: Option<String>,
    /// Live only: where the run's record is uploaded when the run ends.
    #[serde(skip_serializing_if = "Option::is_none")]
    run_record: Option<String>,
    /// Live only, with `manifest:` set: whether the manifest was rewritten.
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_written: Option<bool>,
    /// Local only: the variables the live lake needs.
    #[serde(skip_serializing_if = "Option::is_none")]
    missing_credential: Option<Vec<String>>,
}

impl Report {
    fn new(target: &'static str, lake: &Lake, published: Published) -> Self {
        Report {
            target,
            snapshot_id: published.outcome.snapshot_id,
            committed: published.outcome.committed,
            attempts: published.attempts,
            tables: published.outcome.tables,
            uploaded: published.uploaded,
            unchanged: published.unchanged,
            superseded: published.superseded,
            data_files: published
                .outcome
                .new_files
                .iter()
                .map(|f| format!("{}{f}", lake.data_prefix))
                .collect(),
            pinned_catalog: published.pinned_catalog,
            run_record: None,
            manifest_written: None,
            missing_credential: None,
        }
    }
}

/// Everything one publish needs, whichever lake it goes to.
struct Job<'a> {
    cfg: &'a DucklakeWriteConfig,
    dir: &'a Path,
    prepared: Vec<Prepared>,
    run_id: Option<String>,
}

impl Job<'_> {
    fn inputs(&self) -> Vec<TableInput> {
        self.prepared.iter().map(|p| p.input.clone()).collect()
    }

    fn readable(&self) -> Vec<(PathBuf, String)> {
        self.cfg
            .tables
            .iter()
            .zip(&self.prepared)
            .filter_map(|(t, p)| {
                t.readable_copy
                    .as_ref()
                    .map(|key| (p.input.parquet.clone(), key.clone()))
            })
            .collect()
    }

    fn extra(&self) -> Vec<ExtraFile> {
        self.cfg
            .files
            .iter()
            .map(|f| ExtraFile {
                from: self.dir.join(&f.from),
                to: f.to.clone(),
            })
            .collect()
    }

    fn commit_info(&self, run_record: Option<String>) -> serde_json::Value {
        json!({
            "op": NAME,
            "run_id": self.run_id,
            "run_record": run_record,
            "tables": self.prepared.iter().map(|p| json!({
                "table": p.input.table,
                "rows": p.rows,
            })).collect::<Vec<_>>(),
        })
    }

    fn publish(
        &self,
        store: &dyn ObjectStore,
        lake: &Lake,
        secret_sql: Option<&str>,
        work: &Path,
        run_record: Option<String>,
    ) -> Result<Published> {
        publish(
            store,
            lake,
            secret_sql,
            work,
            &self.inputs(),
            &self.readable(),
            &self.extra(),
            &self.commit_info(run_record),
            MAX_ATTEMPTS,
        )
    }

    /// The local lake: a catalogue at `local_catalog`, its data in the folder beside
    /// it, so the contributor can attach it and query what the run built, offline.
    fn local(&self, missing: Vec<String>) -> Result<Report> {
        let catalog = self.dir.join(&self.cfg.local_catalog);
        let name = catalog
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| failed("`local_catalog` must name a catalogue file"))?
            .to_string();
        let folder = catalog.parent().unwrap_or(self.dir);
        std::fs::create_dir_all(folder)
            .map_err(|e| failed(format!("create {}: {e}", folder.display())))?;
        let folder = std::path::absolute(folder)
            .map_err(|e| failed(format!("resolve {}: {e}", folder.display())))?;
        let data_prefix = format!("{name}.files/");
        let data_path = format!("{}/{data_prefix}", folder.display());
        // No reader fetches the local catalogue by a pinned address, so it pins none.
        let lake = Lake {
            catalog_key: name.clone(),
            write_path: data_path.clone(),
            data_path,
            data_prefix,
            pinned_prefix: None,
        };
        let store = DirStore::new(&folder);
        let work = Work::new()?;
        let published = self.publish(&store, &lake, None, &work.0, None)?;
        eprintln!(
            "{NAME}: published locally to {}; the live lake needs {} set",
            self.cfg.local_catalog,
            missing.join(", ")
        );
        let mut report = Report::new("local", &lake, published);
        report.missing_credential = Some(missing);
        Ok(report)
    }

    /// The live lake, with the resolved credential.
    fn live(&self, credential: &Resolved) -> Result<Report> {
        let store: Arc<dyn ObjectStore> = Arc::new(credential.store()?);
        let lake = self.cfg.live_lake(credential);
        self.publish_live(store, &lake, Some(&credential.secret_sql()))
    }

    /// Publish into `lake` in `store`, then keep the manifest. The run record is
    /// queued as soon as the catalogue holds the snapshot, since the snapshot names
    /// it: a failure after that still leaves the record owed.
    fn publish_live(
        &self,
        store: Arc<dyn ObjectStore>,
        lake: &Lake,
        secret_sql: Option<&str>,
    ) -> Result<Report> {
        let run_id = self.run_id.clone().ok_or_else(|| {
            failed(format!(
                "{RUN_ID_ENV} is not set; a live publish runs inside `arc run`, which names \
                 the run its record is uploaded for"
            ))
        })?;
        let record_key = self.cfg.record_key(&run_id);
        let record_url = format!("{}{record_key}", self.cfg.public_base);
        let work = Work::new()?;
        let mut on_landed = |outcome: &CommitOutcome| {
            // An unchanged publish committed nothing, so no snapshot names a record.
            if outcome.committed {
                crate::lake::queue_run_record_upload(
                    Arc::clone(&store),
                    record_key.clone(),
                    outcome.snapshot_id,
                );
            }
        };
        let published = publish_with_landing(
            store.as_ref(),
            lake,
            secret_sql,
            &work.0,
            &self.inputs(),
            &self.readable(),
            &self.extra(),
            &self.commit_info(Some(record_url)),
            MAX_ATTEMPTS,
            &mut on_landed,
        )?;
        let manifest_written = match &self.cfg.manifest {
            Some(key) => {
                Some(self.write_manifest(store.as_ref(), lake, &work.0, &published, key)?)
            }
            None => None,
        };
        let mut report = Report::new("live", lake, published);
        report.run_record = Some(record_key);
        report.manifest_written = manifest_written;
        Ok(report)
    }

    /// Bring each published table's entry in the compatible manifest up to date,
    /// leaving any a newer snapshot owns. An entry whose copy is unchanged keeps the
    /// time and run it was published with (see [`update_manifest`]), so a publish
    /// that changed nothing leaves the manifest as it was.
    fn write_manifest(
        &self,
        store: &dyn ObjectStore,
        lake: &Lake,
        work: &Path,
        published: &Published,
        key: &str,
    ) -> Result<bool> {
        let snapshot = published.outcome.snapshot_id;
        let time = snapshot_time(&work.join("catalog.ducklake"), snapshot)?;
        let run = json!({
            "id": self.run_id,
            "record": format!(
                "{}{}",
                self.cfg.public_base,
                self.cfg.record_key(self.run_id.as_deref().unwrap_or_default())
            ),
        });
        let mut entries: Vec<(String, ManifestEntry)> = Vec::new();
        for (t, p) in self.cfg.tables.iter().zip(&self.prepared) {
            let Some(copy) = &t.readable_copy else {
                continue;
            };
            let (bytes, sha256) = file_digest(&p.input.parquet)?;
            entries.push((
                t.table.clone(),
                ManifestEntry {
                    address: copy.strip_suffix(".parquet").unwrap_or(copy).to_string(),
                    url: format!("{}{copy}", self.cfg.public_base),
                    rows: p.rows,
                    bytes,
                    sha256,
                    published: time.clone(),
                    run: run.clone(),
                    extra: t.manifest_extra.clone(),
                },
            ));
        }
        let pinned = published
            .pinned_catalog
            .as_ref()
            .map(|k| format!("{}{k}", self.cfg.public_base));
        let watch = Watch::new(
            store,
            lake,
            work,
            snapshot,
            published.catalog_etag.clone(),
            published.seen.clone(),
        );
        let current = || -> Result<(Vec<ManifestEntry>, Option<String>)> {
            let live = watch.live_etag()?;
            let mut current = Vec::with_capacity(entries.len());
            for (table, entry) in &entries {
                if !watch.newer_at(live.as_ref(), &[table.as_str()])? {
                    current.push(entry.clone());
                }
            }
            let pinned = pinned
                .clone()
                .filter(|_| live.as_ref() == Some(&published.catalog_etag));
            Ok((current, pinned))
        };
        update_manifest(store, key, &current, MAX_ATTEMPTS)
    }
}

/// A file's size and SHA-256.
fn file_digest(path: &Path) -> Result<(u64, String)> {
    let mut file =
        std::fs::File::open(path).map_err(|e| failed(format!("read {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let bytes = std::io::copy(&mut file, &mut hasher)
        .map_err(|e| failed(format!("read {}: {e}", path.display())))?;
    Ok((bytes, format!("{:x}", hasher.finalize())))
}

/// The commit time of `snapshot`, RFC 3339, read from the catalogue file as a plain
/// DuckDB database (DuckLake keeps its snapshots in `ducklake_snapshot`).
fn snapshot_time(catalog_file: &Path, snapshot: i64) -> Result<String> {
    let context = |e: duckdb::Error| failed(format!("read the snapshot's commit time: {e}"));
    let conn = duckdb::Connection::open_in_memory().map_err(context)?;
    conn.execute_batch(&format!(
        "ATTACH {} AS arc_catalog (READ_ONLY);",
        sql_string_literal(&catalog_file.display().to_string())
    ))
    .map_err(context)?;
    let micros: i64 = conn
        .query_row(
            "SELECT epoch_us(snapshot_time) FROM arc_catalog.ducklake_snapshot WHERE snapshot_id = ?",
            [snapshot],
            |r| r.get(0),
        )
        .map_err(context)?;
    let at = std::time::UNIX_EPOCH
        + std::time::Duration::from_micros(u64::try_from(micros).unwrap_or_default());
    Ok(humantime::format_rfc3339_seconds(at).to_string())
}

impl Operator for DucklakeWrite {
    fn name(&self) -> &'static str {
        NAME
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    /// A publish runs every time: a run that published locally is not one that
    /// published live, `--publish` changes what the step must do, and an unchanged
    /// publish commits nothing while still repairing a manifest left behind.
    fn always_runs(&self) -> bool {
        true
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = DucklakeWriteConfig::parse(with)?;
        let mut assets = OpAssets::default();
        for t in &cfg.tables {
            assets.record_reads(t.from.clone(), AssetKind::File);
            assets.record_reads(t.descriptor.clone(), AssetKind::File);
        }
        for f in &cfg.files {
            let kind = if f.from.ends_with('/') {
                AssetKind::Directory
            } else {
                AssetKind::File
            };
            assets.record_reads(f.from.clone(), kind);
        }
        Ok(assets)
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = DucklakeWriteConfig::parse(with)?;
        // The asset graph reads a directory's kind from its trailing `/`, as it does
        // every asset name's, without asking the filesystem.
        for f in &cfg.files {
            if !f.from.ends_with('/') && ctx.dir.join(&f.from).is_dir() {
                return Err(failed(format!(
                    "`files.from: {0}` is a directory; write it `{0}/` so the run records \
                     it as one",
                    f.from
                )));
            }
        }
        // The manifest's dotenv files, then the process environment. Never `ctx.env`:
        // that is the run's own `ARC_*` variables.
        let lookup = |var: &str| {
            ctx.dotenv
                .get(var)
                .cloned()
                .filter(|v| !v.is_empty())
                .or_else(|| std::env::var(var).ok())
        };
        let credential = cfg.credential.resolve(lookup);
        // Before anything is read: a required publish that cannot go live fails here,
        // naming only the variables.
        if let Err(missing) = &credential
            && ctx.env.get(PUBLISH_ENV).is_some_and(|v| v == "required")
        {
            return Err(failed(format!(
                "--publish requires the live lake, and {} {} unset or empty; set {} and re-run",
                missing.join(", "),
                if missing.len() == 1 { "is" } else { "are" },
                if missing.len() == 1 { "it" } else { "them" },
            )));
        }
        let prepared = cfg
            .tables
            .iter()
            .map(|t| prepare(ctx.dir, t))
            .collect::<Result<Vec<_>>>()?;
        let job = Job {
            cfg: &cfg,
            dir: ctx.dir,
            prepared,
            run_id: ctx.env.get(RUN_ID_ENV).cloned(),
        };
        let report = match credential {
            Ok(credential) => job.live(&credential)?,
            Err(missing) => job.local(missing)?,
        };
        let tables = report
            .tables
            .iter()
            .map(|t| format!("{} +{} ~{} -{}", t.table, t.inserted, t.updated, t.deleted))
            .collect::<Vec<_>>()
            .join(", ");
        if report.committed {
            eprintln!(
                "{NAME}: committed snapshot {} to the {} lake ({tables})",
                report.snapshot_id, report.target
            );
        } else {
            eprintln!(
                "{NAME}: nothing changed; the {} lake stays at snapshot {}",
                report.target, report.snapshot_id
            );
        }
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

    const BASE: &str = "public_base: https://lake.example/
catalog: catalog/test.ducklake
data_path: lake/
local_catalog: build/lake/test.ducklake
tables:
  - table: demo.codes
    from: data/codes.parquet
    descriptor: codes.datapackage.json
    readable_copy: demo/codes.parquet
files:
  - from: build/search/
    to: demo/search/
credential:
  type: r2
  bucket: test-bucket
  env: { key_id: K, secret: S, account_id: A }
";

    fn with(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    fn refusal(yaml: &str) -> String {
        DucklakeWriteConfig::parse(&with(yaml))
            .err()
            .unwrap_or_else(|| panic!("admitted:\n{yaml}"))
            .to_string()
    }

    #[test]
    fn config_refuses_an_unsafe_key_or_base() {
        assert!(DucklakeWriteConfig::parse(&with(BASE)).is_ok());
        for (from, to, names) in [
            (
                "https://lake.example/",
                "http://lake.example/",
                "public_base",
            ),
            (
                "https://lake.example/",
                "https://lake.example",
                "public_base",
            ),
            (
                "https://lake.example/",
                "ftp://lake.example/",
                "public_base",
            ),
            ("catalog/test.ducklake", "/catalog/test.ducklake", "catalog"),
            ("catalog/test.ducklake", "catalog/../x.ducklake", "catalog"),
            ("data_path: lake/", "data_path: ../lake/", "data_path"),
            ("data_path: lake/", "data_path: /lake/", "data_path"),
            ("demo/codes.parquet", "../codes.parquet", "readable_copy"),
            ("to: demo/search/", "to: s3://elsewhere/", "files.to"),
        ] {
            let yaml = BASE.replace(from, to);
            let e = refusal(&yaml);
            assert!(e.contains(names), "{to}: {e}");
        }
        // Plain http is for tests on this machine only.
        for local in ["http://127.0.0.1:8080/", "http://localhost/"] {
            let yaml = BASE.replace("https://lake.example/", local);
            assert!(DucklakeWriteConfig::parse(&with(&yaml)).is_ok(), "{local}");
        }
        // A credential of each type names the variables that type needs.
        let s3 = BASE.replace("type: r2", "type: s3");
        assert!(refusal(&s3).contains("credential.env.endpoint"));
        let s3 = s3.replace("account_id: A", "endpoint: E, region: R");
        assert!(DucklakeWriteConfig::parse(&with(&s3)).is_ok());
    }

    #[test]
    fn runs_defaults_to_the_runs_prefix_and_refuses_an_unsafe_one() {
        let cfg = DucklakeWriteConfig::parse(&with(BASE)).unwrap();
        assert_eq!(cfg.runs, "runs/");
        assert_eq!(cfg.record_key("run-1"), "runs/run-1.json");
        let with_runs = |runs: &str| format!("{BASE}runs: {runs}\n");
        let ok = DucklakeWriteConfig::parse(&with(&with_runs("lake-test/runs/"))).unwrap();
        assert_eq!(ok.record_key("run-1"), "lake-test/runs/run-1.json");
        for bad in [
            "/runs/",
            "../runs/",
            "a/../runs/",
            "s3://elsewhere/",
            "runs",
        ] {
            let e = refusal(&with_runs(bad));
            assert!(e.contains("runs"), "{bad}: {e}");
        }
    }

    /// A lake under a prefix keeps its run records there: the record is queued
    /// under the configured prefix and the snapshot names that address.
    #[test]
    fn a_configured_runs_prefix_holds_the_record_and_the_snapshot_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        let yaml = format!(
            "{}runs: lake-test/runs/\n",
            BASE.replace(
                "files:\n  - from: build/search/\n    to: demo/search/\n",
                ""
            )
        );
        let cfg = DucklakeWriteConfig::parse(&with(&yaml)).unwrap();
        let job = Job {
            cfg: &cfg,
            dir: p,
            prepared: vec![prepare(p, &cfg.tables[0]).unwrap()],
            run_id: Some("run-3".into()),
        };
        let root = p.join("store");
        std::fs::create_dir_all(&root).unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(&root));
        let lake = Lake {
            catalog_key: cfg.catalog.clone(),
            data_path: format!("{}/lake/", root.display()),
            data_prefix: "lake/".into(),
            write_path: format!("{}/lake/", root.display()),
            pinned_prefix: Some(pinned_prefix(&cfg.catalog)),
        };
        let report = job.publish_live(Arc::clone(&store), &lake, None).unwrap();
        assert_eq!(
            report.run_record.as_deref(),
            Some("lake-test/runs/run-3.json")
        );
        let queued = crate::lake::take_run_record_uploads();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].key, "lake-test/runs/run-3.json");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "LOAD ducklake; ATTACH {} AS l (READ_ONLY);",
            sql_string_literal(&format!(
                "ducklake:{}",
                root.join("catalog/test.ducklake").display()
            ))
        ))
        .unwrap();
        let info: String = conn
            .query_row(
                "SELECT commit_extra_info FROM ducklake_snapshots('l') ORDER BY snapshot_id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let info: serde_json::Value = serde_json::from_str(&info).unwrap();
        assert_eq!(
            info["run_record"],
            "https://lake.example/lake-test/runs/run-3.json"
        );
    }

    #[test]
    fn config_refuses_the_meta_schema() {
        for table in ["meta.codes", "META.codes"] {
            let e = refusal(&BASE.replace("demo.codes", table));
            assert!(e.contains("reserved"), "{table}: {e}");
        }
        assert!(refusal(&BASE.replace("demo.codes", "a.b.c")).contains("schema.table"));
    }

    #[test]
    fn assets_read_every_input() {
        let yaml = BASE.replace(
            "files:\n",
            "  - table: demo.more\n    from: data/more.parquet\n    descriptor: more.json\nfiles:\n  - from: build/notes.txt\n    to: demo/notes.txt\n",
        );
        let assets = DucklakeWrite.assets(&with(&yaml)).unwrap();
        assert_eq!(
            assets.reads,
            [
                "data/codes.parquet",
                "codes.datapackage.json",
                "data/more.parquet",
                "more.json",
                "build/notes.txt",
                "build/search/",
            ]
        );
        assert_eq!(assets.kinds["build/search/"], AssetKind::Directory);
        assert_eq!(assets.kinds["build/notes.txt"], AssetKind::File);
        assert_eq!(assets.kinds["data/codes.parquet"], AssetKind::File);
        assert!(assets.produces.is_empty());
    }

    #[test]
    fn the_secret_statement_holds_the_values_and_its_type() {
        let r2 = Resolved {
            kind: CredentialKind::R2,
            bucket: "b".into(),
            key_id: "kid".into(),
            secret: "it's".into(),
            location: "acct".into(),
            region: "auto".into(),
        };
        assert_eq!(
            r2.secret_sql(),
            "CREATE TEMPORARY SECRET arc_ducklake_write (TYPE r2, KEY_ID 'kid', SECRET 'it''s', ACCOUNT_ID 'acct');"
        );
        let s3 = Resolved {
            kind: CredentialKind::S3,
            location: "http://127.0.0.1:9000/".into(),
            region: "us-east-1".into(),
            ..r2
        };
        assert_eq!(
            s3.secret_sql(),
            "CREATE TEMPORARY SECRET arc_ducklake_write (TYPE s3, KEY_ID 'kid', SECRET 'it''s', ENDPOINT '127.0.0.1:9000', REGION 'us-east-1', URL_STYLE 'path', USE_SSL false);"
        );
    }

    /// DuckDB applies a secret only to paths within its scope, and an `r2` secret's
    /// scope is `r2://`: the live lake's write path must fall inside it, or DuckLake
    /// would reach the data unauthenticated.
    #[test]
    fn the_secret_applies_to_the_write_path() {
        let cfg = DucklakeWriteConfig::parse(&with(BASE)).unwrap();
        let r2 = Resolved {
            kind: CredentialKind::R2,
            bucket: "test-bucket".into(),
            key_id: "kid".into(),
            secret: "sec".into(),
            location: "acct".into(),
            region: "auto".into(),
        };
        let s3 = Resolved {
            kind: CredentialKind::S3,
            location: "http://127.0.0.1:9000/".into(),
            region: "us-east-1".into(),
            ..r2.clone()
        };
        for (credential, kind) in [(&r2, "r2"), (&s3, "s3")] {
            let lake = cfg.live_lake(credential);
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch("INSTALL httpfs; LOAD httpfs;").unwrap();
            conn.execute_batch(&credential.secret_sql()).unwrap();
            let file = format!("{}ab/cd.parquet", lake.write_path);
            let names: Vec<String> = conn
                .prepare("SELECT name FROM which_secret(?, ?)")
                .unwrap()
                .query_map([file.as_str(), kind], |r| r.get(0))
                .unwrap()
                .collect::<duckdb::Result<_>>()
                .unwrap();
            assert_eq!(names, ["arc_ducklake_write"], "{kind}: {file}");
            assert!(lake.write_path.ends_with("test-bucket/lake/"), "{kind}");
        }
    }

    /// Three codes at `data/codes.parquet`, with their descriptor beside them.
    fn write_codes(p: &Path) {
        std::fs::create_dir_all(p.join("data")).unwrap();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT printf('c%03d', range) AS code, 'label' AS label FROM range(3)) TO {} (FORMAT parquet);",
            sql_string_literal(&p.join("data/codes.parquet").display().to_string())
        ))
        .unwrap();
        std::fs::write(
            p.join("codes.datapackage.json"),
            r#"{"name":"codes","resources":[{"name":"codes","schema":{"fields":[{"name":"code","type":"string"},{"name":"label","type":"string"}],"primaryKey":["code"]}}]}"#,
        )
        .unwrap();
    }

    /// A store on disk that counts catalogue fetches and records every write.
    struct Counting {
        inner: DirStore,
        gets: std::sync::Mutex<HashMap<String, u32>>,
        puts: std::sync::Mutex<Vec<String>>,
    }

    impl Counting {
        fn new(root: &Path) -> Self {
            Counting {
                inner: DirStore::new(root),
                gets: Default::default(),
                puts: Default::default(),
            }
        }

        fn gets(&self, key: &str) -> u32 {
            self.gets.lock().unwrap().get(key).copied().unwrap_or(0)
        }

        fn puts(&self) -> Vec<String> {
            self.puts.lock().unwrap().clone()
        }
    }

    impl ObjectStore for Counting {
        fn get(
            &self,
            key: &str,
        ) -> std::result::Result<
            Option<(Vec<u8>, crate::lake::store::ETag)>,
            crate::lake::store::StoreError,
        > {
            *self
                .gets
                .lock()
                .unwrap()
                .entry(key.to_string())
                .or_default() += 1;
            self.inner.get(key)
        }
        fn head(
            &self,
            key: &str,
        ) -> std::result::Result<
            Option<crate::lake::store::ObjectHead>,
            crate::lake::store::StoreError,
        > {
            self.inner.head(key)
        }
        fn put(
            &self,
            key: &str,
            bytes: &[u8],
            cond: crate::lake::store::PutCondition,
            cache: Option<&str>,
        ) -> std::result::Result<crate::lake::store::ETag, crate::lake::store::StoreError> {
            self.puts.lock().unwrap().push(key.to_string());
            self.inner.put(key, bytes, cond, cache)
        }
        fn put_file(
            &self,
            key: &str,
            path: &Path,
            cond: crate::lake::store::PutCondition,
            cache: Option<&str>,
        ) -> std::result::Result<crate::lake::store::ETag, crate::lake::store::StoreError> {
            self.puts.lock().unwrap().push(key.to_string());
            self.inner.put_file(key, path, cond, cache)
        }
    }

    /// A store that refuses every upload of one key, as a dropped connection would.
    struct RefusingKey {
        inner: DirStore,
        key: &'static str,
    }

    impl ObjectStore for RefusingKey {
        fn get(
            &self,
            key: &str,
        ) -> std::result::Result<
            Option<(Vec<u8>, crate::lake::store::ETag)>,
            crate::lake::store::StoreError,
        > {
            self.inner.get(key)
        }
        fn head(
            &self,
            key: &str,
        ) -> std::result::Result<
            Option<crate::lake::store::ObjectHead>,
            crate::lake::store::StoreError,
        > {
            self.inner.head(key)
        }
        fn put(
            &self,
            key: &str,
            bytes: &[u8],
            cond: crate::lake::store::PutCondition,
            cache: Option<&str>,
        ) -> std::result::Result<crate::lake::store::ETag, crate::lake::store::StoreError> {
            self.inner.put(key, bytes, cond, cache)
        }
        fn put_file(
            &self,
            key: &str,
            path: &Path,
            cond: crate::lake::store::PutCondition,
            cache: Option<&str>,
        ) -> std::result::Result<crate::lake::store::ETag, crate::lake::store::StoreError> {
            if key == self.key {
                return Err(crate::lake::store::StoreError::Other(
                    "the connection dropped".into(),
                ));
            }
            self.inner.put_file(key, path, cond, cache)
        }
    }

    /// The snapshot names the run's record from the moment the catalogue lands, so a
    /// failure after that (here, the readable copy's upload) still leaves the record
    /// queued: the run fails, and its record is uploaded beside the snapshot.
    #[test]
    fn a_failure_after_the_catalogue_lands_still_queues_the_run_record() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        let yaml = BASE.replace(
            "files:\n  - from: build/search/\n    to: demo/search/\n",
            "",
        );
        let cfg = DucklakeWriteConfig::parse(&with(&yaml)).unwrap();
        let job = Job {
            cfg: &cfg,
            dir: p,
            prepared: vec![prepare(p, &cfg.tables[0]).unwrap()],
            run_id: Some("run-9".into()),
        };
        let root = p.join("store");
        std::fs::create_dir_all(&root).unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(RefusingKey {
            inner: DirStore::new(&root),
            key: "demo/codes.parquet",
        });
        let lake = Lake {
            catalog_key: cfg.catalog.clone(),
            data_path: format!("{}/lake/", root.display()),
            data_prefix: "lake/".into(),
            write_path: format!("{}/lake/", root.display()),
            pinned_prefix: Some(pinned_prefix(&cfg.catalog)),
        };
        let e = job
            .publish_live(Arc::clone(&store), &lake, None)
            .err()
            .expect("the readable copy's upload fails");
        assert!(e.to_string().contains("demo/codes.parquet"), "{e}");
        assert!(
            store.get(&cfg.catalog).unwrap().is_some(),
            "the catalogue landed"
        );
        let queued = crate::lake::take_run_record_uploads();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].key, "runs/run-9.json");
        assert_eq!(queued[0].snapshot_id, 1);
    }

    /// The live path's own work past `publish`, the manifest and the commit metadata,
    /// run against a store on disk: `publish` and the store are the lake module's,
    /// tested there.
    #[test]
    fn a_publish_writes_the_manifest_and_its_snapshot_names_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        let yaml = BASE
            .replace(
                "files:\n  - from: build/search/\n    to: demo/search/\n",
                "manifest: manifest.json\n",
            )
            .replace(
                "    readable_copy: demo/codes.parquet\n",
                "    readable_copy: demo/codes.parquet\n    manifest_extra: { searchRows: 3 }\n",
            );
        let cfg = DucklakeWriteConfig::parse(&with(&yaml)).unwrap();
        let prepared = vec![prepare(p, &cfg.tables[0]).unwrap()];
        let job = Job {
            cfg: &cfg,
            dir: p,
            prepared,
            run_id: Some("run-1".into()),
        };
        let root = p.join("store");
        std::fs::create_dir_all(&root).unwrap();
        let store = DirStore::new(&root);
        let lake = Lake {
            catalog_key: cfg.catalog.clone(),
            data_path: format!("{}/lake/", root.display()),
            data_prefix: "lake/".into(),
            write_path: format!("{}/lake/", root.display()),
            pinned_prefix: Some(pinned_prefix(&cfg.catalog)),
        };
        let work = Work::new().unwrap();
        let published = job
            .publish(
                &store,
                &lake,
                None,
                &work.0,
                Some("https://lake.example/runs/run-1.json".into()),
            )
            .unwrap();
        assert!(published.outcome.committed);
        assert!(
            job.write_manifest(&store, &lake, &work.0, &published, "manifest.json")
                .unwrap()
        );

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
        let entry = &manifest["datasets"]["demo/codes"];
        assert_eq!(entry["url"], "https://lake.example/demo/codes.parquet");
        assert_eq!(entry["rows"], 3);
        assert_eq!(entry["searchRows"], 3);
        assert_eq!(entry["run"]["id"], "run-1");
        assert_eq!(
            entry["run"]["record"],
            "https://lake.example/runs/run-1.json"
        );
        let (bytes, sha256) = file_digest(&p.join("data/codes.parquet")).unwrap();
        assert_eq!(entry["bytes"], bytes);
        assert_eq!(entry["sha256"], sha256);
        let published_at = entry["published"].as_str().unwrap();
        assert!(
            humantime::parse_rfc3339(published_at).is_ok(),
            "{published_at}"
        );
        assert_eq!(
            manifest["catalog"]["url"],
            format!(
                "https://lake.example/{}",
                published.pinned_catalog.as_deref().unwrap()
            )
        );

        // The snapshot names the run and its record.
        let info: String = {
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "LOAD ducklake; ATTACH {} AS l (READ_ONLY);",
                sql_string_literal(&format!(
                    "ducklake:{}",
                    root.join("catalog/test.ducklake").display()
                ))
            ))
            .unwrap();
            conn.query_row(
                "SELECT commit_extra_info FROM ducklake_snapshots('l') ORDER BY snapshot_id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        let info: serde_json::Value = serde_json::from_str(&info).unwrap();
        assert_eq!(
            info,
            json!({
                "op": "ducklake_write",
                "run_id": "run-1",
                "run_record": "https://lake.example/runs/run-1.json",
                "tables": [{ "table": "demo.codes", "rows": 3 }],
            })
        );

        // Published again unchanged, by another run: nothing in the store is
        // rewritten, the manifest still naming the run that committed the snapshot.
        let manifest_before = std::fs::read(root.join("manifest.json")).unwrap();
        let rerun = Job {
            cfg: &cfg,
            dir: p,
            prepared: vec![prepare(p, &cfg.tables[0]).unwrap()],
            run_id: Some("run-2".into()),
        };
        let counting = Counting::new(&root);
        let work = Work::new().unwrap();
        let again = rerun
            .publish(
                &counting,
                &lake,
                None,
                &work.0,
                Some("https://lake.example/runs/run-2.json".into()),
            )
            .unwrap();
        assert!(!again.outcome.committed);
        assert!(again.uploaded.is_empty(), "{:?}", again.uploaded);
        assert_eq!(again.unchanged, ["demo/codes.parquet"]);
        assert!(
            !rerun
                .write_manifest(&counting, &lake, &work.0, &again, "manifest.json")
                .unwrap()
        );
        assert_eq!(counting.puts(), Vec::<String>::new());
        assert_eq!(
            std::fs::read(root.join("manifest.json")).unwrap(),
            manifest_before
        );

        // A copy left stale is still repaired.
        std::fs::write(root.join("demo/codes.parquet"), b"stale").unwrap();
        let work = Work::new().unwrap();
        let repaired = rerun.publish(&store, &lake, None, &work.0, None).unwrap();
        assert_eq!(
            repaired
                .uploaded
                .iter()
                .map(|u| u.key.as_str())
                .collect::<Vec<_>>(),
            ["demo/codes.parquet"]
        );
        assert_eq!(
            std::fs::read(root.join("demo/codes.parquet")).unwrap(),
            std::fs::read(p.join("data/codes.parquet")).unwrap()
        );
    }

    /// Where nothing moves after the catalogue lands, the uploads and the manifest
    /// read the catalogue's version, and fetch none of its bytes.
    #[test]
    fn a_publish_fetches_the_catalogue_once_when_nothing_moves() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        std::fs::copy(p.join("data/codes.parquet"), p.join("data/more.parquet")).unwrap();
        std::fs::copy(
            p.join("codes.datapackage.json"),
            p.join("more.datapackage.json"),
        )
        .unwrap();
        std::fs::create_dir_all(p.join("build/search")).unwrap();
        for part in ["a", "b", "c"] {
            std::fs::write(p.join(format!("build/search/{part}.json")), part).unwrap();
        }
        let yaml = BASE
            .replace(
                "files:\n",
                "  - table: demo.more\n    from: data/more.parquet\n    descriptor: more.datapackage.json\n    readable_copy: demo/more.parquet\nmanifest: manifest.json\nfiles:\n",
            );
        let cfg = DucklakeWriteConfig::parse(&with(&yaml)).unwrap();
        let root = p.join("store");
        std::fs::create_dir_all(&root).unwrap();
        let lake = Lake {
            catalog_key: cfg.catalog.clone(),
            data_path: format!("{}/lake/", root.display()),
            data_prefix: "lake/".into(),
            write_path: format!("{}/lake/", root.display()),
            pinned_prefix: Some(pinned_prefix(&cfg.catalog)),
        };
        for run in ["run-1", "run-2"] {
            let job = Job {
                cfg: &cfg,
                dir: p,
                prepared: cfg.tables.iter().map(|t| prepare(p, t).unwrap()).collect(),
                run_id: Some(run.into()),
            };
            let store = Arc::new(Counting::new(&root));
            let report = job
                .publish_live(Arc::clone(&store) as Arc<dyn ObjectStore>, &lake, None)
                .unwrap();
            assert_eq!(report.manifest_written, Some(run == "run-1"), "{run}");
            // The attempt's own fetch, and none after the catalogue lands.
            assert_eq!(store.gets(&cfg.catalog), 1, "{run}");
        }
    }

    /// Only a publish that committed a snapshot owes a run record: an unchanged
    /// re-run queues nothing, since no snapshot or manifest entry would name it.
    #[test]
    fn an_unchanged_live_publish_queues_no_run_record() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        let yaml = BASE.replace(
            "files:\n  - from: build/search/\n    to: demo/search/\n",
            "",
        );
        let cfg = DucklakeWriteConfig::parse(&with(&yaml)).unwrap();
        let root = p.join("store");
        std::fs::create_dir_all(&root).unwrap();
        let lake = Lake {
            catalog_key: cfg.catalog.clone(),
            data_path: format!("{}/lake/", root.display()),
            data_prefix: "lake/".into(),
            write_path: format!("{}/lake/", root.display()),
            pinned_prefix: Some(pinned_prefix(&cfg.catalog)),
        };
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(&root));
        for (run, committed) in [("run-1", true), ("run-2", false)] {
            let job = Job {
                cfg: &cfg,
                dir: p,
                prepared: cfg.tables.iter().map(|t| prepare(p, t).unwrap()).collect(),
                run_id: Some(run.into()),
            };
            let report = job.publish_live(Arc::clone(&store), &lake, None).unwrap();
            assert_eq!(report.committed, committed, "{run}");
            let queued = crate::lake::take_run_record_uploads();
            assert_eq!(queued.len(), usize::from(committed), "{run}");
        }
    }

    /// A manifest entry names the run that published its copy: an unchanged rerun
    /// of one step, after another step committed to the same catalogue, leaves the
    /// manifest byte for byte as it was.
    #[test]
    fn an_unchanged_rerun_keeps_its_entry_after_another_step_publishes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        std::fs::copy(p.join("data/codes.parquet"), p.join("data/more.parquet")).unwrap();
        std::fs::copy(
            p.join("codes.datapackage.json"),
            p.join("more.datapackage.json"),
        )
        .unwrap();
        let a_yaml = BASE.replace(
            "files:\n  - from: build/search/\n    to: demo/search/\n",
            "manifest: manifest.json\n",
        );
        let b_yaml = a_yaml
            .replace("demo.codes", "demo.more")
            .replace("data/codes.parquet", "data/more.parquet")
            .replace("codes.datapackage.json", "more.datapackage.json")
            .replace("demo/codes.parquet", "demo/more.parquet");
        let a_cfg = DucklakeWriteConfig::parse(&with(&a_yaml)).unwrap();
        let b_cfg = DucklakeWriteConfig::parse(&with(&b_yaml)).unwrap();
        let root = p.join("store");
        std::fs::create_dir_all(&root).unwrap();
        let lake = Lake {
            catalog_key: a_cfg.catalog.clone(),
            data_path: format!("{}/lake/", root.display()),
            data_prefix: "lake/".into(),
            write_path: format!("{}/lake/", root.display()),
            pinned_prefix: Some(pinned_prefix(&a_cfg.catalog)),
        };
        let store: Arc<dyn ObjectStore> = Arc::new(DirStore::new(&root));
        let publish = |cfg: &DucklakeWriteConfig, run: &str| {
            let job = Job {
                cfg,
                dir: p,
                prepared: vec![prepare(p, &cfg.tables[0]).unwrap()],
                run_id: Some(run.into()),
            };
            job.publish_live(Arc::clone(&store), &lake, None).unwrap()
        };
        assert!(publish(&a_cfg, "run-1").committed);
        assert!(publish(&b_cfg, "run-2").committed);
        let before = std::fs::read(root.join("manifest.json")).unwrap();
        let rerun = publish(&a_cfg, "run-3");
        assert!(!rerun.committed);
        assert_eq!(rerun.manifest_written, Some(false));
        assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), before);
        let manifest: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(manifest["datasets"]["demo/codes"]["run"]["id"], "run-1");
        assert_eq!(manifest["datasets"]["demo/more"]["run"]["id"], "run-2");
    }

    /// No reader fetches the local lake's catalogue by a pinned address, so a local
    /// publish pins none.
    #[test]
    fn a_local_publish_pins_no_catalogue_copy() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        let yaml = BASE.replace(
            "files:\n  - from: build/search/\n    to: demo/search/\n",
            "",
        );
        let env = HashMap::new();
        let ctx = OpContext {
            dir: p,
            db_path: p,
            env: &env,
            dotenv: crate::operator::no_dotenv(),
            timeout: None,
            cache: None,
        };
        let out = DucklakeWrite.run(&with(&yaml), &ctx).unwrap();
        let report = out.report.unwrap();
        assert_eq!(report["target"], "local");
        assert_eq!(report["committed"], true);
        assert!(report["pinned_catalog"].is_null(), "{report}");
        let names: Vec<String> = std::fs::read_dir(p.join("build/lake"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".ducklake"))
            .collect();
        assert_eq!(names, ["test.ducklake"]);
    }

    /// A directory's kind is read from its trailing `/`, as every asset name's is, so
    /// a `files.from` naming a directory without one is refused before anything is
    /// published, rather than recorded as a file.
    #[test]
    fn a_files_directory_without_a_trailing_slash_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        write_codes(p);
        std::fs::create_dir_all(p.join("build/search")).unwrap();
        std::fs::write(p.join("build/search/part.json"), b"{}").unwrap();
        let yaml = BASE.replace("from: build/search/", "from: build/search");
        let env = HashMap::new();
        let ctx = OpContext {
            dir: p,
            db_path: p,
            env: &env,
            dotenv: crate::operator::no_dotenv(),
            timeout: None,
            cache: None,
        };
        let e = DucklakeWrite
            .run(&with(&yaml), &ctx)
            .expect_err("refused")
            .to_string();
        assert!(e.contains("`build/search/`"), "{e}");
        assert!(!p.join("build/lake").exists(), "nothing was published");
    }

    #[test]
    fn a_required_publish_without_the_credential_fails_naming_only_variables() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert(PUBLISH_ENV.to_string(), "required".to_string());
        let mut dotenv = HashMap::new();
        dotenv.insert("A".to_string(), "account-value".to_string());
        // A dotenv file cannot make the publish required, nor stand in for arc's flag.
        dotenv.insert(PUBLISH_ENV.to_string(), "optional".to_string());
        let ctx = OpContext {
            dir: dir.path(),
            db_path: dir.path(),
            env: &env,
            dotenv: &dotenv,
            timeout: None,
            cache: None,
        };
        let e = DucklakeWrite
            .run(&with(BASE), &ctx)
            .expect_err("refused")
            .to_string();
        assert!(e.contains("K, S are unset"), "{e}");
        assert!(!e.contains("account-value"), "{e}");
    }
}
