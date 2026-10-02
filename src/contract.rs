//! The live Protocol+Run contract.
//!
//! `arc run` builds a self-describing asset graph while it executes; historically
//! that graph was discarded when the run returned. This module turns it into a
//! durable artifact: a JSON **contract** describing the run (its protocol, engine,
//! parameters, assets, and steps) plus a per-step **status stream** written live as
//! the run progresses. Together they are the seam a viewer, a dashboard, or a later
//! phase reads — the run stops being a black box that only leaves behind a DuckDB
//! file.
//!
//! What is emitted, per run, under `<dir>/build/.arcform/runs/`:
//!   - `<run_id>.json`  — the full contract (written once, at run end).
//!   - `<run_id>.jsonl` — one line per step as it finishes, then a terminal
//!     `run_complete` line. Append-only; a tailing reader takes the last complete
//!     line per step.
//!
//! The `contract_version` tag (`"b4/1"`) versions the JSON shape. Fields that a
//! later phase will populate (bytes, content hashes, file-path lineage, per-step
//! duration, typed skip reasons, …) are present in the schema but emitted as `null`
//! now, so the shape is stable across phases.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::asset::AssetGraph;
use crate::asset_kind::{AssetKind, default_kind_for_declared_name};
use crate::ingress_meta::FetchMeta;
use crate::manifest::{Manifest, Param, Step};

/// The JSON contract shape version. An opaque tag — bump it when the shape changes.
pub const CONTRACT_VERSION: &str = "b4/1";

// ─────────────────────────────────────────────────────────────────────────────
// Contract shape
// ─────────────────────────────────────────────────────────────────────────────

/// The full per-run contract, serialized to `<run_id>.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Contract {
    /// Opaque schema-shape version tag.
    pub contract_version: String,
    /// The run: protocol, engine, params, timing, outcome.
    pub run: RunInfo,
    /// One entry per real produced/read asset (CTE internals excluded).
    pub assets: Vec<AssetEntry>,
    /// One entry per declared step.
    pub steps: Vec<StepEntry>,
}

/// Run-level metadata.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunInfo {
    pub run_id: String,
    /// The attempt within the run. Equal to `run_id` in this phase (no re-attempts yet).
    pub attempt_id: String,
    pub protocol: ProtocolInfo,
    pub engine: EngineInfo,
    pub params: Vec<ParamEntry>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// `"success"`, `"error"`, or `"partial"`.
    pub outcome: String,
}

/// Which protocol ran, and the exact manifest bytes it ran from.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProtocolInfo {
    pub name: String,
    /// SHA-256 of the `arcform.yaml` bytes — pins the exact manifest this run used.
    pub manifest_sha256: Option<String>,
    pub dir: String,
}

/// The engine versions that executed the run.
///
/// `duckdb_cli`, `platform` and `extensions` are what the DuckDB the SQL steps ran on reports
/// when the run ends, and are `null` for a Protocol with no SQL step or hook, which asks it
/// nothing. They were added under `b4/1` rather than a new tag, and a record written before
/// them reads with each absent.
#[derive(Debug, Serialize, Deserialize)]
pub struct EngineInfo {
    /// `arc` binary version.
    pub arc: String,
    /// The version of the DuckDB library `arc` is linked with (`SELECT version()`), if
    /// reachable. The SQL steps run on the DuckDB CLI, which `duckdb_cli` names.
    pub duckdb: Option<String>,
    /// The version the DuckDB CLI the SQL steps ran on reports (`SELECT version()`), which
    /// differs from `duckdb` when `ARC_DUCKDB_BIN` or the search path gives the steps
    /// another DuckDB.
    pub duckdb_cli: Option<String>,
    /// The platform that DuckDB reports (`PRAGMA platform`): `linux_amd64`.
    pub platform: Option<String>,
    /// Each extension the Protocol's SQL installs by name, from DuckDB's own repository or
    /// `FROM community`, in the order the run first found it; then each other extension DuckDB
    /// reported loaded in the process of a SQL step or hook that passed, one it loaded on its own
    /// included, in the order the run first found it, except one built into DuckDB, whose
    /// `install_path` reads `(BUILT-IN)`.
    pub extensions: Option<Vec<ExtensionEntry>>,
}

/// An extension a run's SQL installs, as the DuckDB the steps ran on reports it when the run
/// ends; or one a SQL step's or hook's process loaded, as that process reported it. Each value
/// is `null` where DuckDB gives none or arc could not ask it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionEntry {
    pub name: String,
    /// The repository DuckDB reports it was installed from (`installed_from`): `core`,
    /// `community`.
    pub repository: Option<String>,
    /// The version DuckDB reports for it (`extension_version`).
    pub version: Option<String>,
    /// The SHA-256 of the file DuckDB names as its `install_path`; `null` when DuckDB names
    /// none or arc cannot read the file it names.
    pub sha256: Option<String>,
}

/// A resolved parameter and where its value came from.
#[derive(Debug, Serialize, Deserialize)]
pub struct ParamEntry {
    pub key: String,
    pub value: String,
    /// `"cli"`, `"env"` (dotenv), or `"default"`.
    pub source: String,
}

/// A data asset — a table (or source, or file) produced and/or consumed by steps.
#[derive(Debug, Serialize, Deserialize)]
pub struct AssetEntry {
    /// Dotted id, e.g. `"table:customers"` / `"file:build/out.parquet"`.
    pub id: String,
    /// `"table"`, `"source"` (read but never produced here), `"file"`, or
    /// `"directory"` (a produced tree — a `COPY … PARTITION_BY` target, or a
    /// pattern-only `archive_extract` destination).
    pub kind: String,
    pub name: String,
    /// Filesystem path for a `file`/`directory` asset (relative to the protocol
    /// dir); `null` for relational (`table`/`source`) assets, whose `name` is the
    /// relation.
    pub path: Option<String>,
    /// Measured byte size: on-disk length for `file` assets, DuckDB `estimated_size` for
    /// relational assets. `null` when the artifact can't be measured (missing/unqueryable).
    pub bytes: Option<u64>,
    /// Live measure: row count for table/source assets, if queryable.
    pub row_count: Option<i64>,
    /// Content hash (sha256 hex — one representation across kinds): the `.arcmeta` fetch
    /// sidecar's sha256 for a fetched file, else the file's bytes hashed; for a relational
    /// asset, a deterministic hash of its rows. `null` when unmeasurable.
    pub content_hash: Option<String>,
    /// The step whose produced-set contains this asset, or `null` for external reads.
    pub produced_by: Option<String>,
    /// Steps whose read-set contains this asset.
    pub consumed_by: Vec<String>,
}

/// The step arm: how the step does its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    Sql,
    Op,
    Command,
}

/// A declared step and how it fared.
#[derive(Debug, Serialize, Deserialize)]
pub struct StepEntry {
    pub name: String,
    pub kind: StepKind,
    /// For `op:` steps: the operator reference resolved from the catalog. `null` otherwise.
    pub op_ref: Option<OpRef>,
    /// For `op:` steps: the resolved `with:` config (secret-looking values redacted). `null` otherwise.
    pub resolved_with: Option<serde_json::Value>,
    /// For `sql:` steps: the model path, text, hash, and per-statement lineage. `null` otherwise.
    pub sql: Option<SqlInfo>,
    pub status: StatusInfo,
    /// Actual attempt count (1 on first-try success, more if retried; 0 if skipped/not reached).
    pub attempts: u32,
    /// Wall-clock duration of the step (all attempts + backoff). `null` when the step was
    /// skipped or never reached.
    pub duration_sec: Option<f64>,
    /// Effective retry policy (step override or manifest default), if any.
    pub retry: Option<RetryInfo>,
    pub timeout_sec: Option<f64>,
    pub io: IoInfo,
    /// Ingress freshness sidecar (`.arcmeta`) for a fetched artifact, if present.
    pub ingress_meta: Option<FetchMeta>,
    /// What an `op:` step reported about THIS run's execution of it — for
    /// `ducklake_publish`, the snapshot it created or found already holding the bytes.
    /// `null` for a step that was skipped, failed or never reached, and for every step
    /// kind that reports nothing, so a value here is always this run's own account.
    /// Added under `b4/1` rather than a new tag: it is one nullable field, and every
    /// reader of the contract ignores fields it does not know.
    pub report: Option<serde_json::Value>,
    pub narrative: Narrative,
}

/// A resolved `op:` reference.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpRef {
    pub name: String,
    /// The `@<semver-req>` constraint the manifest pinned, if any.
    pub constraint: Option<String>,
    /// The catalog operator's version that satisfied the constraint.
    pub version_resolved: String,
}

/// SQL step detail: the model file, its text + hash, and per-statement lineage.
#[derive(Debug, Serialize, Deserialize)]
pub struct SqlInfo {
    pub model_path: String,
    pub sql_text: String,
    pub sql_hash: String,
    pub statements: Vec<StatementInfo>,
}

/// Lineage of a single SQL statement.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatementInfo {
    pub produces: Vec<String>,
    pub reads: Vec<String>,
    /// `[start, end)` byte offsets into the step's `sql_text` — the exact source slice for
    /// this statement, so a viewer can render statements individually. `null` if the
    /// lexical split didn't line up with the parsed statements.
    pub byte_range: Option<[usize; 2]>,
}

/// A step's terminal status.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusInfo {
    /// `"success"`, `"failed"`, or `"skipped"`.
    pub state: String,
    /// For a skipped step, the typed reason it was fresh; `null` for steps that executed.
    pub skip_reason: Option<crate::state::SkipReason>,
}

/// The effective retry policy for a step.
#[derive(Debug, Serialize, Deserialize)]
pub struct RetryInfo {
    pub max_attempts: u32,
    pub backoff_sec: f64,
}

/// Captured stdout/stderr paths — deferred (`null`) in this phase.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IoInfo {
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
}

/// Human-facing framing for a step — deferred (all `null`) in this phase.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Narrative {
    pub label: Option<String>,
    pub stage: Option<String>,
    pub doc: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Live status stream (`<run_id>.jsonl`)
// ─────────────────────────────────────────────────────────────────────────────

/// Append-only per-step status stream. One JSON object per line: a `{step,state,ts}`
/// line as each step finishes, then a terminal `{event:"run_complete",outcome,ts}`
/// line. Best-effort — if the file can't be opened, writes are silently no-ops so a
/// stream-write failure never fails the run.
pub struct RunStream {
    file: Option<std::fs::File>,
}

impl RunStream {
    /// Open (create/append) `<runs_dir>/<run_id>.jsonl`.
    pub fn create(runs_dir: &Path, run_id: &str) -> Self {
        let path = runs_dir.join(format!("{run_id}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        RunStream { file }
    }

    /// Record that `step` finished in `state` (`"success"`/`"failed"`/`"skipped"`).
    pub fn step(&mut self, step: &str, state: &str) {
        self.write_line(&serde_json::json!({
            "step": step,
            "state": state,
            "ts": now_iso(),
        }));
    }

    /// Record the terminal run outcome.
    pub fn complete(&mut self, outcome: &str) {
        self.write_line(&serde_json::json!({
            "event": "run_complete",
            "outcome": outcome,
            "ts": now_iso(),
        }));
    }

    fn write_line(&mut self, value: &serde_json::Value) {
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{value}");
            let _ = file.flush();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Building + writing the contract
// ─────────────────────────────────────────────────────────────────────────────

/// The directory a run's contract + stream are written to: `<dir>/build/.arcform/runs`.
pub fn runs_dir(dir: &Path) -> PathBuf {
    dir.join("build").join(".arcform").join("runs")
}

/// How a single step fared, captured by the run loop for the contract.
#[derive(Debug, Clone)]
pub struct StepOutcome {
    /// Terminal state: `"success"`, `"failed"`, or `"skipped"`.
    pub state: String,
    /// Attempts made (1 on first-try success, more if retried; 0 if skipped/not reached).
    pub attempts: u32,
    /// For a skipped step, why it was fresh; `None` for steps that executed.
    pub skip_reason: Option<crate::state::SkipReason>,
    /// Wall-clock duration of the step; `None` when skipped or never reached.
    pub duration_sec: Option<f64>,
    /// The operator's own report of a successful execution (see
    /// [`crate::engine::StepOutput::report`]); `None` otherwise.
    pub report: Option<serde_json::Value>,
}

/// What a run records of the DuckDB its SQL steps ran on: [`EngineInfo`]'s `duckdb_cli`,
/// `platform` and `extensions`.
#[derive(Debug, Clone, PartialEq)]
pub struct StepsEngine {
    pub duckdb_cli: Option<String>,
    pub platform: Option<String>,
    pub extensions: Vec<ExtensionEntry>,
}

/// What the run records of the DuckDB its SQL steps ran on, from `report`, its answer when
/// asked about `names`: an entry for each name, in order, whatever the answer holds of it;
/// then an entry for each extension in `loaded` that is not one of `names` and whose file
/// DuckDB does not report as [`BUILT_IN`](crate::engine::BUILT_IN), with what the process that
/// loaded it reported. Each entry has the SHA-256 of the file DuckDB names, hashed now.
pub fn steps_engine(
    report: crate::engine::EngineReport,
    names: &[String],
    loaded: &crate::engine::LoadedExtensions,
) -> StepsEngine {
    let built_in = Path::new(crate::engine::BUILT_IN);
    let installed = names.iter().map(|name| {
        let reported = report.extensions.get(name).cloned().unwrap_or_default();
        (name, reported)
    });
    let loaded_only = loaded
        .iter()
        .filter(|(name, reported)| {
            !names.contains(name) && reported.path.as_deref() != Some(built_in)
        })
        .map(|(name, reported)| (name, reported.clone()));
    let extensions = installed
        .chain(loaded_only)
        .map(|(name, reported)| ExtensionEntry {
            name: name.clone(),
            repository: reported.repository,
            version: reported.version,
            sha256: reported
                .path
                .and_then(|path| crate::fetch_cache::hash_file(&path).ok()),
        })
        .collect();
    StepsEngine {
        duckdb_cli: report.version,
        platform: report.platform,
        extensions,
    }
}

/// Inputs to [`build_contract`] — grouped to keep the call site readable.
pub struct ContractInputs<'a> {
    pub manifest: &'a Manifest,
    pub dir: &'a Path,
    pub db_path: &'a Path,
    pub graph: &'a AssetGraph,
    pub run_id: &'a str,
    pub started_at: &'a str,
    pub finished_at: &'a str,
    pub outcome: &'a str,
    pub params: Vec<ParamEntry>,
    /// Per-step terminal outcome, keyed by step name.
    pub step_outcomes: &'a HashMap<String, StepOutcome>,
    /// What the DuckDB the SQL steps ran on reported; `None` for a Protocol with no SQL step
    /// or hook.
    pub steps_engine: Option<StepsEngine>,
}

/// Assemble the full contract from the run's manifest, asset graph, and outcomes.
///
/// Opens a single DuckDB connection to the run's database (best-effort) to read the
/// engine version and measure per-table row counts.
pub fn build_contract(inp: ContractInputs) -> Contract {
    // One connection for both the engine version and every row-count measure.
    let conn = duckdb::Connection::open(inp.db_path).ok();
    let duckdb_version = conn.as_ref().and_then(|c| {
        c.query_row("SELECT version()", [], |r| r.get::<_, String>(0))
            .ok()
    });

    let assets = build_assets(inp.manifest, inp.dir, inp.graph, conn.as_ref());
    let steps = inp
        .manifest
        .steps
        .iter()
        .map(|s| build_step(s, inp.dir, inp.graph, inp.manifest, inp.step_outcomes))
        .collect();

    Contract {
        contract_version: CONTRACT_VERSION.to_string(),
        run: RunInfo {
            run_id: inp.run_id.to_string(),
            attempt_id: inp.run_id.to_string(),
            protocol: ProtocolInfo {
                name: inp.manifest.name.clone(),
                manifest_sha256: manifest_sha256(inp.dir),
                dir: inp.dir.display().to_string(),
            },
            engine: {
                let (duckdb_cli, platform, extensions) = match inp.steps_engine {
                    Some(e) => (e.duckdb_cli, e.platform, Some(e.extensions)),
                    None => (None, None, None),
                };
                EngineInfo {
                    arc: env!("CARGO_PKG_VERSION").to_string(),
                    duckdb: duckdb_version,
                    duckdb_cli,
                    platform,
                    extensions,
                }
            },
            params: inp.params,
            started_at: Some(inp.started_at.to_string()),
            finished_at: Some(inp.finished_at.to_string()),
            outcome: inp.outcome.to_string(),
        },
        assets,
        steps,
    }
}

/// Write the contract to `<runs_dir>/<run_id>.json` (pretty-printed).
pub fn write_contract(runs_dir: &Path, run_id: &str, contract: &Contract) -> std::io::Result<()> {
    let path = runs_dir.join(format!("{run_id}.json"));
    let json = serde_json::to_string_pretty(contract).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

/// Build the deduplicated asset list from the graph, measuring each asset where possible.
///
/// Each asset is measured net-new at run end: `file` assets are stat'd + hashed on disk
/// (reusing the `.arcmeta` sha256 when a fetch left one), relational assets get their
/// DuckDB `estimated_size` + row count + a deterministic row hash. `dir` resolves the
/// relative file paths lineage lifted from the SQL (see [`crate::introspect`]).
fn build_assets(
    manifest: &Manifest,
    dir: &Path,
    graph: &AssetGraph,
    conn: Option<&duckdb::Connection>,
) -> Vec<AssetEntry> {
    // Walk steps in declared order so producer selection is deterministic.
    let mut producer: BTreeMap<String, String> = BTreeMap::new();
    let mut consumers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut universe: BTreeSet<String> = BTreeSet::new();

    for step in &manifest.steps {
        let Some(sa) = graph.steps.get(&step.name) else {
            continue;
        };
        for produced in &sa.produces {
            universe.insert(produced.clone());
            producer
                .entry(produced.clone())
                .or_insert_with(|| step.name.clone());
        }
        for read in &sa.reads {
            universe.insert(read.clone());
            consumers
                .entry(read.clone())
                .or_default()
                .push(step.name.clone());
        }
    }

    universe
        .iter()
        .map(|name| {
            let produced_by = producer.get(name).cloned();
            let consumed_by = consumers.get(name).cloned().unwrap_or_default();
            // The kind carried from wherever this asset was declared — the producing
            // step's own `declared_kind` when there is a producer (the step that
            // wrote it knows best what it wrote), else any step that reads it (an
            // external input still had its kind set at the point SQL introspection
            // or an operator's config declared it), else a name-based guess for the
            // one case nothing in the graph can answer (an asset referenced only via
            // `assets:` with no matching producer or reader — a manifest defect the
            // load-time warning already reports separately).
            let asset_kind = producer
                .get(name)
                .and_then(|step_name| graph.steps.get(step_name))
                .and_then(|sa| sa.declared_kind.get(name))
                .copied()
                .or_else(|| {
                    consumers.get(name).and_then(|readers| {
                        readers.iter().find_map(|step_name| {
                            graph
                                .steps
                                .get(step_name)
                                .and_then(|sa| sa.declared_kind.get(name))
                                .copied()
                        })
                    })
                })
                .unwrap_or_else(|| default_kind_for_declared_name(name));
            let kind = match asset_kind {
                AssetKind::File => "file",
                AssetKind::Directory => "directory",
                // A pattern is never one relation either, but nothing in this
                // report distinguishes it from a table with no producer today —
                // both are unmeasurable through the engine and land as "source".
                AssetKind::Pattern | AssetKind::Table => {
                    if produced_by.is_none() {
                        "source"
                    } else {
                        "table"
                    }
                }
            };
            // Measure the asset net-new. Files and directories are measured on disk;
            // relational assets (table/source) are measured through the engine.
            let (path, bytes, row_count, content_hash) = match kind {
                "file" => {
                    let full = dir.join(name);
                    let (bytes, content_hash) = measure_file(&full);
                    (Some(name.clone()), bytes, None, content_hash)
                }
                "directory" => {
                    let full = dir.join(name);
                    let (bytes, content_hash) = measure_directory(&full);
                    (Some(name.clone()), bytes, None, content_hash)
                }
                _ => {
                    let (bytes, content_hash) =
                        conn.map(|c| measure_table(c, name)).unwrap_or((None, None));
                    let row_count = conn.and_then(|c| table_row_count(c, name));
                    (None, bytes, row_count, content_hash)
                }
            };
            AssetEntry {
                id: format!("{kind}:{name}"),
                kind: kind.to_string(),
                name: name.clone(),
                path,
                bytes,
                row_count,
                content_hash,
                produced_by,
                consumed_by,
            }
        })
        .collect()
}

/// On-disk measure for a `file` asset: byte length + content hash. The hash **reuses the
/// fetch sidecar's sha256** (`<file>.arcmeta`, see [`crate::ingress_meta`]) when present, so
/// a fetched artifact is never re-hashed under a second scheme; otherwise the file's bytes
/// are hashed directly. Both are sha256 hex — the single representation shared with tables.
/// Best-effort: an absent/unreadable file yields `(None, None)`.
fn measure_file(full: &Path) -> (Option<u64>, Option<String>) {
    let bytes = std::fs::metadata(full).ok().map(|m| m.len());
    let content_hash = crate::ingress_meta::read(full)
        .map(|m| m.sha256)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read(full)
                .ok()
                .map(|b| crate::state::content_hash(&b))
        });
    (bytes, content_hash)
}

/// On-disk measure for a `directory` asset: total byte length of every regular file
/// in the tree, plus [`crate::state::hash_directory_contents`] — the same hash
/// `produced_artifact_hash` uses for staleness, so a report and a staleness
/// decision can never quietly disagree about what a directory asset's content
/// identity is. No `.arcmeta` sidecar reuse here (unlike `measure_file`): a sidecar
/// is written per-file by the fetch operators, and a directory asset is a tree of
/// files, not one fetched artifact.
fn measure_directory(full: &Path) -> (Option<u64>, Option<String>) {
    let bytes = walk_file_sizes(full);
    let content_hash = crate::state::hash_directory_contents(full);
    (bytes, content_hash)
}

/// Sum of every regular file's byte length under `dir`, recursively. `None` when
/// the directory cannot be read at all (missing, permission-denied) — mirrors
/// `hash_directory_contents`'s own unreadable signal, so `bytes` and `content_hash`
/// go missing together rather than one reporting a stale partial answer.
fn walk_file_sizes(dir: &Path) -> Option<u64> {
    let mut total: u64 = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let read_dir = std::fs::read_dir(&current).ok()?;
        for entry in read_dir {
            let entry = entry.ok()?;
            let file_type = entry.file_type().ok()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().ok()?.len();
            }
        }
    }
    Some(total)
}

/// Engine-side measure for a relational asset: DuckDB `estimated_size` bytes + a
/// deterministic content hash. The hash is `sha256` over the relation's rows rendered as
/// text and **ordered**, so it is independent of physical row order and shares the sha256-hex
/// representation used for files. Best-effort: a non-identifier name or any query failure
/// (missing/unqueryable relation) degrades to `None`, exactly like [`table_row_count`].
fn measure_table(conn: &duckdb::Connection, name: &str) -> (Option<u64>, Option<String>) {
    if !is_simple_ident(name) {
        return (None, None);
    }
    let bytes = conn
        .query_row(
            "SELECT estimated_size FROM duckdb_tables() WHERE table_name = ?",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .filter(|n| *n >= 0)
        .map(|n| n as u64);
    // `CAST(<rel> AS VARCHAR)` renders each row as a struct literal; string_agg with an
    // ORDER BY makes the digest order-independent. sha256() returns lowercase hex.
    let hash_sql = format!(
        "SELECT sha256(coalesce(string_agg(r, '\n' ORDER BY r), '')) \
         FROM (SELECT CAST({name} AS VARCHAR) AS r FROM {name})"
    );
    let content_hash = conn
        .query_row(&hash_sql, [], |r| r.get::<_, String>(0))
        .ok();
    (bytes, content_hash)
}

/// Build a single step entry.
fn build_step(
    step: &Step,
    dir: &Path,
    graph: &AssetGraph,
    manifest: &Manifest,
    outcomes: &HashMap<String, StepOutcome>,
) -> StepEntry {
    let kind = if step.sql.is_some() {
        StepKind::Sql
    } else if step.op.is_some() {
        StepKind::Op
    } else {
        StepKind::Command
    };

    // op_ref: split the `name@constraint` reference and resolve the catalog version.
    let op_ref = step.op.as_ref().map(|reference| {
        let (name, constraint) = match reference.split_once('@') {
            Some((n, c)) => (n.trim().to_string(), Some(c.trim().to_string())),
            None => (reference.trim().to_string(), None),
        };
        let version_resolved = crate::operator::resolve(reference)
            .map(|op| op.version().to_string())
            .unwrap_or_default();
        OpRef {
            name,
            constraint,
            version_resolved,
        }
    });

    // resolved_with: the op's typed config, secret-looking values redacted.
    let resolved_with = if step.op.is_some() {
        match &step.with {
            Some(with) if !with.is_null() => serde_json::to_value(with).ok().map(|mut json| {
                redact_json(&mut json);
                json
            }),
            _ => None,
        }
    } else {
        None
    };

    // sql: keep the model text + hash + per-statement lineage.
    let sql = step.sql.as_ref().map(|model_path| {
        let full = dir.join(model_path);
        let (sql_text, sql_hash, statements) = match std::fs::read(&full) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes).to_string();
                let hash = crate::state::content_hash(&bytes);
                let statements = match crate::introspect::extract_per_statement(&text) {
                    Ok(list) => {
                        // Attach each statement's source byte range. The lexical splitter
                        // and the parser agree on count for well-formed SQL; if they ever
                        // diverge, emit no ranges rather than misalign them.
                        let ranges = crate::introspect::statement_byte_ranges(&text);
                        let aligned = ranges.len() == list.len();
                        list.into_iter()
                            .enumerate()
                            .map(|(i, a)| StatementInfo {
                                produces: a.outputs.into_iter().collect(),
                                reads: a.inputs.into_iter().collect(),
                                byte_range: aligned.then(|| {
                                    let (s, e) = ranges[i];
                                    [s, e]
                                }),
                            })
                            .collect()
                    }
                    Err(_) => Vec::new(),
                };
                (text, hash, statements)
            }
            Err(_) => (String::new(), String::new(), Vec::new()),
        };
        SqlInfo {
            model_path: model_path.clone(),
            sql_text,
            sql_hash,
            statements,
        }
    });

    let outcome = outcomes.get(&step.name).cloned().unwrap_or(StepOutcome {
        state: "skipped".to_string(),
        attempts: 0,
        skip_reason: None,
        duration_sec: None,
        report: None,
    });

    // Effective retry policy: a step override wins, else the manifest default.
    let effective_retry = step
        .retry
        .as_ref()
        .or_else(|| manifest.defaults.as_ref().and_then(|d| d.retry.as_ref()));
    let retry = effective_retry.map(|r| RetryInfo {
        max_attempts: r.max_attempts,
        backoff_sec: r.backoff_sec,
    });

    // ingress_meta: best-effort — a produced artifact with a sibling `.arcmeta`.
    let ingress_meta = graph.steps.get(&step.name).and_then(|sa| {
        sa.produces
            .iter()
            .find_map(|produced| crate::ingress_meta::read(&dir.join(produced)))
    });

    StepEntry {
        name: step.name.clone(),
        kind,
        op_ref,
        resolved_with,
        sql,
        status: StatusInfo {
            state: outcome.state,
            skip_reason: outcome.skip_reason,
        },
        attempts: outcome.attempts,
        duration_sec: outcome.duration_sec,
        retry,
        timeout_sec: step.timeout_sec,
        io: IoInfo::default(),
        ingress_meta,
        report: outcome.report,
        narrative: Narrative::default(),
    }
}

/// Resolve each declared parameter's value and its source, mirroring the runner's
/// precedence (CLI > dotenv > default). Secret-looking values are redacted.
pub fn param_entries(
    manifest_params: &IndexMap<String, Param>,
    dotenv_vars: &HashMap<String, String>,
    cli_params: &[(String, String)],
) -> Vec<ParamEntry> {
    let cli: HashMap<&str, &str> = cli_params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let mut out = Vec::new();
    for (name, param) in manifest_params {
        let (value, source) = if let Some(v) = cli.get(name.as_str()) {
            (v.to_string(), "cli")
        } else if let Some(v) = dotenv_vars.get(name) {
            (v.clone(), "env")
        } else if let Some(default) = &param.default {
            (default.clone(), "default")
        } else {
            // Missing required param — resolve_params already errored before this
            // point; skip defensively.
            continue;
        };
        let value = if is_secret_key(name) {
            REDACTED.to_string()
        } else {
            value
        };
        out.push(ParamEntry {
            key: name.clone(),
            value,
            source: source.to_string(),
        });
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Terminal DAG render
// ─────────────────────────────────────────────────────────────────────────────

/// A readable, dependency-free text DAG derived from the contract: assets as nodes
/// (kind + measured row count), with producer and consumer edges.
pub fn render_dag(contract: &Contract) -> String {
    let mut out = String::new();
    let n = contract.assets.len();
    out.push_str(&format!(
        "\nAsset graph ({n} node{}):\n",
        if n == 1 { "" } else { "s" }
    ));

    if contract.assets.is_empty() {
        out.push_str("  (no tracked assets)\n");
        return out;
    }

    for asset in &contract.assets {
        let rows = match asset.row_count {
            Some(count) => format!(", {count} row{}", if count == 1 { "" } else { "s" }),
            None => String::new(),
        };
        out.push_str(&format!("  {} [{}{}]\n", asset.name, asset.kind, rows));
        match &asset.produced_by {
            Some(producer) => out.push_str(&format!("      produced by  {producer}\n")),
            None => out.push_str("      produced by  (external source)\n"),
        }
        if !asset.consumed_by.is_empty() {
            out.push_str(&format!(
                "      feeds        {}\n",
                asset.consumed_by.join(", ")
            ));
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

const REDACTED: &str = "***REDACTED***";

/// Whether a config/param key name looks like it holds a secret.
fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "secret",
        "token",
        "password",
        "passwd",
        "credential",
        "api_key",
        "apikey",
        "access_key",
        "auth",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

/// Recursively replace secret-looking object values with a redaction marker.
fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if is_secret_key(key) {
                    *val = serde_json::Value::String(REDACTED.to_string());
                } else {
                    redact_json(val);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_json),
        _ => {}
    }
}

/// Whether `name` is a bare SQL identifier safe to interpolate into a count query.
fn is_simple_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Count rows in a relational asset. Returns `None` for non-identifier names or when
/// the relation doesn't exist / isn't countable — the guard that keeps a missing
/// table from erroring the run.
fn table_row_count(conn: &duckdb::Connection, name: &str) -> Option<i64> {
    if !is_simple_ident(name) {
        return None;
    }
    let sql = format!("SELECT count(*) FROM {name}");
    conn.query_row(&sql, [], |r| r.get::<_, i64>(0)).ok()
}

/// SHA-256 of the manifest bytes (`arcform.yaml`), pinning the exact manifest run.
fn manifest_sha256(dir: &Path) -> Option<String> {
    std::fs::read(dir.join("arcform.yaml"))
        .ok()
        .map(|bytes| crate::state::content_hash(&bytes))
}

/// Current UTC time as an ISO-8601 second-resolution string (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let tod = secs % 86400;
    let (hours, minutes, seconds) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (year, month, day) = days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since the Unix epoch to (year, month, day). Howard Hinnant's
/// public-domain civil-from-days algorithm (matches the run-id timestamp logic).
fn days_to_date(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `build_assets` classifies by `declared_kind` (carried from the SQL/operator
    // declaration), never by inspecting the name's shape — a File-kind produces
    // entry with no recognized extension, a Directory-kind entry with no operator
    // ever consulting its extension, a Table with a producer, and a Table with no
    // producer (an external "source") each land correctly. This replaced
    // `looks_like_file`'s standalone string tests once that function was removed —
    // the classification now only exists as part of the graph it is carried
    // through, so it is tested the same way.
    #[test]
    fn build_assets_classifies_by_declared_kind_not_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("models")).unwrap();
        std::fs::write(
            dir.path().join("models/load.sql"),
            "CREATE TABLE registry AS SELECT 1;",
        )
        .unwrap();

        let yaml = "name: test
steps:
  - name: load
    sql: models/load.sql
  - name: export
    op: parquet_export
    with:
      input: customers
      dest: build/out.avro
  - name: extract
    op: archive_extract
    with:
      archive: build/in.zip
      pattern: '\\.tsv$'
      dest: build/out
";
        let manifest = Manifest::from_yaml_str(yaml).unwrap();
        let graph = AssetGraph::build(&manifest, dir.path());
        let assets = build_assets(&manifest, dir.path(), &graph, None);
        let by_name: BTreeMap<&str, &AssetEntry> =
            assets.iter().map(|a| (a.name.as_str(), a)).collect();

        // A Table this manifest produces.
        assert_eq!(by_name["registry"].kind, "table");
        assert_eq!(by_name["registry"].produced_by.as_deref(), Some("load"));
        // A Table nothing in this manifest produces — an external source.
        assert_eq!(by_name["customers"].kind, "source");
        assert!(by_name["customers"].produced_by.is_none());
        // A File produced under an extension no allowlist would recognize —
        // `parquet_export`'s `dest:` carries File from its own typed config, not
        // from `.avro` being on a list.
        assert_eq!(by_name["build/out.avro"].kind, "file");
        // A File read with no producer in this graph at all.
        assert_eq!(by_name["build/in.zip"].kind, "file");
        // A Directory — `archive_extract`'s pattern-only `dest:` — never mistaken
        // for a file because it happens to have no extension or because nothing
        // stats the filesystem to check.
        assert_eq!(by_name["build/out"].kind, "directory");
    }

    #[test]
    fn simple_ident_guard() {
        assert!(is_simple_ident("customers"));
        assert!(is_simple_ident("_tmp1"));
        assert!(!is_simple_ident("build/out.parquet"));
        assert!(!is_simple_ident("1abc"));
        assert!(!is_simple_ident("drop table x; --"));
    }

    #[test]
    fn redaction_hits_secret_keys_only() {
        let mut json = serde_json::json!({
            "url": "https://example.com",
            "api_token": "hunter2",
            "headers": { "Authorization": "Bearer abc" },
        });
        redact_json(&mut json);
        assert_eq!(json["url"], serde_json::json!("https://example.com"));
        assert_eq!(json["api_token"], serde_json::json!(REDACTED));
        assert_eq!(
            json["headers"]["Authorization"],
            serde_json::json!(REDACTED)
        );
    }

    #[test]
    fn measure_file_hashes_bytes_when_no_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("out.parquet");
        std::fs::write(&f, b"hello world").unwrap();
        let (bytes, hash) = measure_file(&f);
        assert_eq!(bytes, Some(11), "byte length measured on disk");
        // sha256("hello world") — the file-bytes hash when there is no .arcmeta sidecar.
        assert_eq!(
            hash.as_deref(),
            Some("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"),
        );
    }

    #[test]
    fn measure_file_reuses_arcmeta_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("edgar.parquet");
        std::fs::write(&f, b"the actual downloaded bytes").unwrap();
        // A fetch left a sidecar recording the remote content identity — reuse it
        // verbatim rather than re-hashing the file under a second scheme.
        let meta = FetchMeta {
            url: "https://openlake.meridian.online/edgar.parquet".to_string(),
            sha256: "feedface00000000000000000000000000000000000000000000000000000000".to_string(),
            ..Default::default()
        };
        crate::ingress_meta::write(&f, &meta).unwrap();
        let (bytes, hash) = measure_file(&f);
        assert!(bytes.is_some(), "byte length still measured on disk");
        assert_eq!(
            hash.as_deref(),
            Some(meta.sha256.as_str()),
            "sidecar sha256 reused"
        );
    }

    #[test]
    fn measure_file_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let (bytes, hash) = measure_file(&dir.path().join("nope.parquet"));
        assert!(bytes.is_none() && hash.is_none());
    }

    #[test]
    fn now_iso_is_well_formed() {
        let ts = now_iso();
        // YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "unexpected timestamp: {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn param_entries_track_source_and_redact_secrets() {
        let mut params: IndexMap<String, Param> = IndexMap::new();
        params.insert(
            "date".to_string(),
            Param {
                default: Some("2026-01-01".to_string()),
            },
        );
        params.insert(
            "mode".to_string(),
            Param {
                default: Some("local".to_string()),
            },
        );
        params.insert(
            "api_token".to_string(),
            Param {
                default: Some("sekret".to_string()),
            },
        );

        let mut dotenv = HashMap::new();
        dotenv.insert("mode".to_string(), "dotenv-mode".to_string());

        let cli = vec![("date".to_string(), "2026-07-18".to_string())];

        let entries = param_entries(&params, &dotenv, &cli);
        let by_key: HashMap<&str, &ParamEntry> =
            entries.iter().map(|e| (e.key.as_str(), e)).collect();

        assert_eq!(by_key["date"].source, "cli");
        assert_eq!(by_key["date"].value, "2026-07-18");
        assert_eq!(by_key["mode"].source, "env");
        assert_eq!(by_key["mode"].value, "dotenv-mode");
        assert_eq!(by_key["api_token"].source, "default");
        assert_eq!(by_key["api_token"].value, REDACTED);
    }

    /// A run record as `arc run` wrote it before the record named the DuckDB the SQL steps ran
    /// on: written by the commit before that change, on a fake engine, with the Protocol's
    /// directory replaced.
    const RECORD_BEFORE_STEPS_ENGINE: &str = r#"{
  "contract_version": "b4/1",
  "run": {
    "run_id": "20261001-225417-11c7d1ec",
    "attempt_id": "20261001-225417-11c7d1ec",
    "protocol": {
      "name": "old",
      "manifest_sha256": "9b163d9bd65fce144ee2b33a1beecd27dddf5ef41b1ba67ad52897555c9bd20a",
      "dir": "/home/analyst/old"
    },
    "engine": {
      "arc": "0.1.0",
      "duckdb": "v1.5.4"
    },
    "params": [],
    "started_at": "2026-10-01T22:54:17Z",
    "finished_at": "2026-10-01T22:54:17Z",
    "outcome": "success"
  },
  "assets": [
    {
      "id": "table:a",
      "kind": "table",
      "name": "a",
      "path": null,
      "bytes": null,
      "row_count": null,
      "content_hash": null,
      "produced_by": "load",
      "consumed_by": []
    }
  ],
  "steps": [
    {
      "name": "load",
      "kind": "sql",
      "op_ref": null,
      "resolved_with": null,
      "sql": {
        "model_path": "models/load.sql",
        "sql_text": "INSTALL mlpack FROM community;\nCREATE TABLE a AS SELECT 1 AS x;\n",
        "sql_hash": "4e3de9a8f1cfc449abbbd0844c79f0d44590cc08f392ec0c2e2e2112549e9375",
        "statements": [
          {
            "produces": [],
            "reads": [],
            "byte_range": [
              0,
              30
            ]
          },
          {
            "produces": [
              "a"
            ],
            "reads": [],
            "byte_range": [
              31,
              63
            ]
          }
        ]
      },
      "status": {
        "state": "success",
        "skip_reason": null
      },
      "attempts": 1,
      "duration_sec": 0.040102817,
      "retry": null,
      "timeout_sec": null,
      "io": {
        "stdout_path": null,
        "stderr_path": null
      },
      "ingress_meta": null,
      "report": null,
      "narrative": {
        "label": null,
        "stage": null,
        "doc": null
      }
    }
  ]
}"#;

    #[test]
    fn a_record_written_before_the_steps_engine_fields_reads_with_each_absent() {
        assert_eq!(CONTRACT_VERSION, "b4/1");
        let contract: Contract = serde_json::from_str(RECORD_BEFORE_STEPS_ENGINE)
            .expect("a record in the shape arc wrote before reads into Contract");
        assert_eq!(contract.contract_version, CONTRACT_VERSION);
        let engine = &contract.run.engine;
        assert_eq!(engine.duckdb.as_deref(), Some("v1.5.4"));
        assert_eq!(engine.duckdb_cli, None);
        assert_eq!(engine.platform, None);
        assert_eq!(engine.extensions, None);
    }

    #[test]
    fn the_steps_engine_names_each_extension_asked_about_with_what_duckdb_reported() {
        use crate::engine::{EngineReport, ReportedExtension};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mlpack.duckdb_extension");
        std::fs::write(&file, b"a build").unwrap();
        let hash = crate::fetch_cache::hash_file(&file).unwrap();
        let reported = |path: Option<PathBuf>, repository: &str, version: &str| ReportedExtension {
            path,
            repository: Some(repository.to_string()),
            version: Some(version.to_string()),
        };
        let report = EngineReport {
            version: Some("v1.5.3".into()),
            platform: Some("osx_arm64".into()),
            extensions: [
                ("mlpack", reported(Some(file), "community", "v1.5.5")),
                // A path arc cannot read: one that is not there, and a directory.
                (
                    "httpfs",
                    reported(Some(dir.path().join("gone")), "core", "827222f"),
                ),
                ("json", reported(Some(dir.path().into()), "core", "v1.5.3")),
                // DuckDB names no file.
                ("excel", reported(None, "core", "v1.5.3")),
            ]
            .into_iter()
            .map(|(name, ext)| (name.to_string(), ext))
            .collect(),
        };
        let names: Vec<String> = ["mlpack", "httpfs", "json", "excel", "spatial"]
            .map(String::from)
            .to_vec();
        let entry =
            |name: &str, repository: Option<&str>, version: Option<&str>, sha256: Option<&str>| {
                ExtensionEntry {
                    name: name.to_string(),
                    repository: repository.map(String::from),
                    version: version.map(String::from),
                    sha256: sha256.map(String::from),
                }
            };
        assert_eq!(
            steps_engine(report, &names, &Default::default()),
            StepsEngine {
                duckdb_cli: Some("v1.5.3".into()),
                platform: Some("osx_arm64".into()),
                extensions: vec![
                    entry("mlpack", Some("community"), Some("v1.5.5"), Some(&hash)),
                    entry("httpfs", Some("core"), Some("827222f"), None),
                    entry("json", Some("core"), Some("v1.5.3"), None),
                    entry("excel", Some("core"), Some("v1.5.3"), None),
                    // DuckDB lists nothing for it.
                    entry("spatial", None, None, None),
                ],
            }
        );

        // DuckDB could not be asked.
        assert_eq!(
            steps_engine(EngineReport::default(), &names[..1], &Default::default()),
            StepsEngine {
                duckdb_cli: None,
                platform: None,
                extensions: vec![entry("mlpack", None, None, None)],
            }
        );
    }

    #[test]
    fn the_steps_engine_names_each_extension_a_step_loaded_after_the_ones_the_sql_installs() {
        use crate::engine::{BUILT_IN, EngineReport, LoadedExtensions, ReportedExtension};
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = dir.path().join(format!("{name}.duckdb_extension"));
            std::fs::write(&path, format!("{name}'s build")).unwrap();
            let hash = crate::fetch_cache::hash_file(&path).unwrap();
            (path, hash)
        };
        let (httpfs, httpfs_hash) = file("httpfs");
        let (httpfs_loaded, _) = file("httpfs-loaded");
        let (fts, fts_hash) = file("fts");
        let (json, json_hash) = file("json");
        let reported = |path: Option<PathBuf>, repository: &str, version: &str| ReportedExtension {
            path,
            repository: Some(repository.to_string()),
            version: Some(version.to_string()),
        };
        let report = EngineReport {
            version: Some("v1.5.5".into()),
            platform: Some("linux_amd64".into()),
            extensions: [(
                "httpfs".to_string(),
                reported(Some(httpfs), "core", "v1.5.5"),
            )]
            .into_iter()
            .collect(),
        };
        let mut loaded = LoadedExtensions::default();
        loaded.add(vec![
            // The SQL installs it by name: its entry is the one the run gives when it ends.
            (
                "httpfs".into(),
                reported(Some(httpfs_loaded), "community", "0000000"),
            ),
            ("fts".into(), reported(Some(fts), "core", "v1.5.5")),
            // Built into DuckDB: covered by the version the record gives.
            (
                "parquet".into(),
                reported(Some(PathBuf::from(BUILT_IN)), "", ""),
            ),
            // Built into DuckDB, and loaded from a file an earlier `INSTALL` fetched.
            ("json".into(), reported(Some(json), "core", "v1.5.5")),
            // DuckDB names no file.
            ("excel".into(), reported(None, "core", "v1.5.5")),
        ]);
        let entry =
            |name: &str, repository: Option<&str>, version: Option<&str>, sha256: Option<&str>| {
                ExtensionEntry {
                    name: name.to_string(),
                    repository: repository.map(String::from),
                    version: version.map(String::from),
                    sha256: sha256.map(String::from),
                }
            };
        let names = vec!["httpfs".to_string(), "spatial".to_string()];
        assert_eq!(
            steps_engine(report, &names, &loaded).extensions,
            vec![
                entry("httpfs", Some("core"), Some("v1.5.5"), Some(&httpfs_hash)),
                entry("spatial", None, None, None),
                entry("fts", Some("core"), Some("v1.5.5"), Some(&fts_hash)),
                entry("json", Some("core"), Some("v1.5.5"), Some(&json_hash)),
                entry("excel", Some("core"), Some("v1.5.5"), None),
            ]
        );
    }
}
