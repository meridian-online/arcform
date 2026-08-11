//! First-class typed operators — the `op:` / `with:` third step arm.
//!
//! An operator is a *packaged, typed* unit of work configured (not scripted) via a
//! `with:` block. Unlike an opaque `command:` shell string, an operator declares its
//! I/O to the **same** [`crate::asset::AssetGraph`] the SQL path uses, so lineage,
//! ordering, and stale-propagation hold at its boundary — a step that isn't in the
//! graph can silently ship a stale artifact on a green run (the exact defect the
//! `edgar_gleif` `package` step patches by hand today).
//!
//! Operators are addressed by `op: <name>@<semver-req>` and resolved from a built-in
//! [`catalog`] — a namespace deliberately *distinct* from the pipeline `registry`.
//! Config is validated by typed deserialization (a `with:`
//! block that doesn't deserialize into the operator's config is a load-time error);
//! JSON-Schema emission for Brightfield authoring forms is a later addition.

use std::collections::HashMap;
// `BTreeMap` — not `HashMap` — wherever the map's iteration order reaches an output:
// the fetch operators' `headers`, and `parquet_export`'s `metadata`, whose order is
// written verbatim into the Parquet footer and so into the file's hash.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_yaml::Value;

use crate::engine::StepOutput;
use crate::error::{Error, Result};

/// Assets an operator step reads and produces — merged into the [`crate::asset::AssetGraph`]
/// exactly as SQL introspection and command `produces`/`depends_on` are.
#[derive(Debug, Clone, Default)]
pub struct OpAssets {
    pub produces: Vec<String>,
    pub reads: Vec<String>,
}

/// Execution context handed to an operator's [`Operator::run`].
pub struct OpContext<'a> {
    /// Manifest directory — resolve relative paths (`dest`, `out`) against this.
    pub dir: &'a Path,
    /// The pipeline's DuckDB file — operators that touch tables open this.
    pub db_path: &'a Path,
    /// `ARC_PARAM_*` environment for the step, passed through to subprocess operators.
    pub env: &'a HashMap<String, String>,
    /// Step timeout, if any — enforced by `run_process` (Inherit mode).
    pub timeout: Option<Duration>,
    /// The shared fetch cache, when the environment gives the run one. Ingress
    /// operators consult it so the same URL is transferred once however many
    /// Protocols want it; `None` is a run with no cache, which every other operator
    /// is anyway.
    pub cache: Option<&'a crate::fetch_cache::FetchCache>,
}

/// A first-class typed operator. Registered in the [`catalog`], addressed by
/// `op: <name>@<semver>`.
pub trait Operator: Sync {
    /// Catalog name — the `<name>` in `op: <name>@<ver>`.
    fn name(&self) -> &'static str;

    /// Operator version. A Protocol pins it with a semver constraint, exactly as
    /// `engine_version` pins the engine — extending local≡cloud parity to every operator.
    fn version(&self) -> semver::Version;

    /// Validate the config and declare the assets this step reads/produces, so lineage
    /// holds at the boundary. Doubles as the config-validation gate (a `with:` that does
    /// not deserialize is an `Err` here, surfaced at manifest load).
    fn assets(&self, with: &Value) -> Result<OpAssets>;

    /// Execute the operator.
    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput>;
}

static PARQUET_EXPORT: ParquetExport = ParquetExport;
#[cfg(feature = "http-fetch")]
static HTTP_FETCH: HttpFetch = HttpFetch;
#[cfg(feature = "http-fetch")]
static HTML_LINK_DISCOVER: HtmlLinkDiscover = HtmlLinkDiscover;
static ARCHIVE_EXTRACT: ArchiveExtract = ArchiveExtract;
static DATAPACKAGE_DESCRIBE: DatapackageDescribe = DatapackageDescribe;
static FINETYPE_VALIDATE: FinetypeValidate = FinetypeValidate;
static SPLINK_RESOLVE: SplinkResolve = SplinkResolve;
static GLEIF_RA_FETCH: GleifRaFetch = GleifRaFetch;
#[cfg(feature = "opendal")]
static OPENDAL_FETCH: OpendalFetch = OpendalFetch;

/// The built-in operator catalog. Two entries are feature-gated: the ureq-backed
/// ingress ops (`http_fetch`, `html_link_discover`) appear only under `http-fetch`
/// (enabled via `cli`; a `default-features = false` library consumer builds without
/// them and without ureq — see Cargo.toml), and `opendal_fetch` only under `opendal`
/// (off by default to keep the single binary lean).
fn catalog() -> Vec<&'static dyn Operator> {
    #[allow(unused_mut)]
    let mut ops: Vec<&'static dyn Operator> = vec![
        &PARQUET_EXPORT,
        &ARCHIVE_EXTRACT,
        &DATAPACKAGE_DESCRIBE,
        &FINETYPE_VALIDATE,
        &SPLINK_RESOLVE,
        &GLEIF_RA_FETCH,
    ];
    #[cfg(feature = "http-fetch")]
    ops.extend([
        &HTTP_FETCH as &'static dyn Operator,
        &HTML_LINK_DISCOVER as &'static dyn Operator,
    ]);
    #[cfg(feature = "opendal")]
    ops.push(&OPENDAL_FETCH);
    ops
}

/// Resolve an `op:` reference (`name` or `name@<semver-req>`) from the catalog,
/// checking the version constraint. Returns a validation error if the operator is
/// unknown or the installed version doesn't satisfy the constraint.
pub fn resolve(op_ref: &str) -> Result<&'static dyn Operator> {
    let (name, req) = match op_ref.split_once('@') {
        Some((n, r)) => (n.trim(), Some(r.trim())),
        None => (op_ref.trim(), None),
    };
    let op = catalog()
        .into_iter()
        .find(|o| o.name() == name)
        .ok_or_else(|| {
            Error::ManifestValidation(format!(
                "unknown operator '{}' (not in the operator catalog)",
                name
            ))
        })?;
    if let Some(req) = req {
        let vreq = semver::VersionReq::parse(req).map_err(|_| {
            Error::ManifestValidation(format!(
                "operator '{}': invalid version constraint '{}' (expected semver, e.g. '1' or '^1.2')",
                name, req
            ))
        })?;
        if !vreq.matches(&op.version()) {
            return Err(Error::ManifestValidation(format!(
                "operator '{}@{}' not satisfied by installed version {}",
                name,
                req,
                op.version()
            )));
        }
    }
    Ok(op)
}

/// Resolve and declare an operator step's assets in one call — used by manifest
/// validation and the asset-graph builder (which have no [`OpContext`]).
pub fn assets_for(op_ref: &str, with: Option<&Value>) -> Result<OpAssets> {
    resolve(op_ref)?.assets(with.unwrap_or(&Value::Null))
}

/// Every operator in the built-in [`catalog`], by name, in catalog order.
///
/// `opendal_fetch` appears only when the `opendal` feature is on (it is only in the
/// catalog then). Used by the `arc mcp` `operator_describe` tool to enumerate the
/// operators an authoring UI can pick from.
#[cfg(feature = "mcp")]
pub(crate) fn catalog_names() -> Vec<&'static str> {
    catalog().into_iter().map(|o| o.name()).collect()
}

/// The JSON Schema (Draft 2020-12) for an operator's `with:` block, or `None` for a
/// name not in the catalog.
///
/// This is the authoring-form emission the module doc anticipates: it describes the
/// shape a `with:` block must take so an editor or the `arc mcp` `operator_describe`
/// tool can build a form or validate a draft. It is a *description*, not the gate —
/// each operator's typed `serde` deserialize (see its `…Config::parse`) remains the
/// load-time validator, and this schema is hand-kept in step with it (the
/// `every_catalog_operator_has_a_with_schema` test fails if a new operator is added
/// without one).
#[cfg(feature = "mcp")]
pub(crate) fn with_schema(op_name: &str) -> Option<serde_json::Value> {
    use serde_json::json;

    // A `with:` schema: an object of the operator's typed fields, closed to unknown
    // keys (every config is `#[serde(deny_unknown_fields)]`).
    fn object(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
            "required": required,
        })
    }
    let str_headers = json!({
        "type": "object",
        "description": "Extra request headers (a default User-Agent is set unless overridden).",
        "additionalProperties": { "type": "string" }
    });

    let schema = match op_name {
        "parquet_export" => object(
            json!({
                "input": { "type": "string", "description": "Source table or view in the pipeline DB to export." },
                "dest": { "type": "string", "description": "Destination Parquet path, relative to the protocol directory." },
                "compression": { "type": "string", "description": "Parquet codec.", "default": "zstd" },
                "row_group_size": { "type": "integer", "minimum": 0, "description": "Optional Parquet row-group size." },
                "order_by": { "type": "string", "description": "Optional ORDER BY clause applied to the export." },
                "metadata": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Key-value metadata stamped into the Parquet footer. Keys and values are strings, written as UTF-8 bytes; read them back with decode(key)/decode(value) over parquet_kv_metadata(), since Parquet stores them untyped and DuckDB returns BLOB. Entries are emitted in sorted key order. Declaring any metadata changes the file's bytes and so its hash; declaring none leaves the output unchanged."
                }
            }),
            &["input", "dest"],
        ),
        "http_fetch" => object(
            json!({
                "url": { "type": "string", "description": "Source URL (http/https)." },
                "out": { "type": "string", "description": "Destination path, relative to the protocol directory. Written atomically." },
                "headers": str_headers,
                "sha256": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$", "description": "Pin the artifact's SHA-256. Pinned bytes are taken from the shared fetch cache without asking the origin, and a transfer that does not match is refused." }
            }),
            &["url", "out"],
        ),
        "opendal_fetch" => object(
            json!({
                "from": { "type": "string", "description": "Scheme-dispatched source: https://host/key, s3://bucket/key, …" },
                "to": { "type": "string", "description": "Destination path, relative to the protocol directory. Written atomically." },
                "headers": str_headers
            }),
            &["from", "to"],
        ),
        "html_link_discover" => object(
            json!({
                "url": { "type": "string", "description": "Index page to fetch (http/https)." },
                "pattern": { "type": "string", "description": "Regex tested (unanchored) against each raw href; matches are kept." },
                "out": { "type": "string", "description": "Destination URL-list path (newline-delimited), relative to the protocol dir." },
                "headers": str_headers
            }),
            &["url", "pattern", "out"],
        ),
        "archive_extract" => object(
            json!({
                "archive": { "type": "string", "description": "Source .zip, relative to the protocol dir." },
                "members": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Explicit member names to extract (exact match). Supply this or `pattern`."
                },
                "pattern": { "type": "string", "description": "Regex tested (unanchored) against each member name; matches are extracted. Supply this or `members`." },
                "dest": { "type": "string", "description": "Destination directory, relative to the protocol dir." }
            }),
            &["archive", "dest"],
        ),
        "datapackage_describe" => object(
            json!({
                "parquet": { "type": "string", "description": "Built Parquet whose columns FineType types." },
                "overrides": { "type": "string", "description": "Curated descriptor sidecar (JSON) overlaid onto FineType's base." },
                "out": { "type": "string", "description": "datapackage.json to write." }
            }),
            &["parquet", "overrides", "out"],
        ),
        "finetype_validate" => object(
            json!({
                "parquet": { "type": "string", "description": "Built Parquet to check." },
                "schema": { "type": "string", "description": "Self-derived JSON-Schema contract to check against." },
                "extension": { "type": "string", "description": "Optional per-step override for the FineType DuckDB extension path (else FINETYPE_DUCKDB_EXT)." }
            }),
            &["parquet", "schema"],
        ),
        "splink_resolve" => object(
            json!({
                "edgar": { "type": "string", "description": "EDGAR/SEC-entity Parquet (the crosswalk's left side)." },
                "gleif": { "type": "string", "description": "GLEIF golden-copy Parquet (the right side)." },
                "out": { "type": "string", "description": "Resolved-crosswalk Parquet to write." },
                "sample": { "type": "integer", "minimum": 0, "description": "Optional GLEIF-row cap for a fast smoke test (omit/0 resolves the full corpus)." }
            }),
            &["edgar", "gleif", "out"],
        ),
        "gleif_ra_fetch" => object(
            json!({
                "ra": { "type": "string", "description": "GLEIF registration-authority id to page (e.g. RA000665)." },
                "out": { "type": "string", "description": "Destination CSV path, relative to the protocol directory." },
                "page_size": { "type": "integer", "minimum": 1, "description": "GLEIF page[size]. Defaults to the script's 200 when omitted." },
                "user_agent": { "type": "string", "description": "User-Agent request header. Defaults to the script's Meridian UA when omitted." }
            }),
            &["ra", "out"],
        ),
        _ => return None,
    };
    // Name the schema after the operator so an authoring form can title it.
    let mut schema = schema;
    if let Some(map) = schema.as_object_mut() {
        map.insert("title".to_string(), json!(op_name));
    }
    Some(schema)
}

#[cfg(all(test, feature = "mcp"))]
mod with_schema_tests {
    use super::*;

    #[test]
    fn every_catalog_operator_has_a_with_schema() {
        for name in catalog_names() {
            let schema = with_schema(name)
                .unwrap_or_else(|| panic!("operator `{name}` is in the catalog but has no `with:` schema — add one in `with_schema`"));
            assert_eq!(
                schema["type"], "object",
                "`{name}` schema must be an object"
            );
            assert!(
                schema["properties"].is_object(),
                "`{name}` schema must carry `properties`"
            );
        }
    }

    #[test]
    fn unknown_operator_has_no_schema() {
        assert!(with_schema("does_not_exist").is_none());
    }

    #[test]
    fn parquet_export_schema_shape() {
        let schema = with_schema("parquet_export").expect("parquet_export has a schema");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["input"].is_object());
        assert!(schema["properties"]["dest"].is_object());
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"input") && required.contains(&"dest"));

        // `metadata` is optional, and an authoring form has to know it is a
        // string→string mapping: the config is `BTreeMap<String, String>`, so a
        // form offering any other value type builds a `with:` block that will not
        // deserialize.
        let metadata = &schema["properties"]["metadata"];
        assert_eq!(metadata["type"], "object", "`metadata` must be an object");
        assert_eq!(
            metadata["additionalProperties"]["type"], "string",
            "`metadata` values must be declared as strings"
        );
        assert!(
            !required.contains(&"metadata"),
            "`metadata` must stay optional — an export declaring none must remain valid"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Subprocess substrate — the shared spawn/wait/error-map for operators that wrap
// an external process: a `uv run` Python script (splink_resolve, gleif_ra_fetch)
// or a CLI (datapackage_describe → finetype). One place for the retry taxonomy so
// each op doesn't hand-roll a Command block with divergent error handling.
// (Wired by those operators in later increments — allow(dead_code) until then.)
// ─────────────────────────────────────────────────────────────────────────────

/// How a subprocess operator handles its child's output.
enum OutputMode {
    /// Inherit the terminal — stream child stdout/stderr live (e.g. `splink_resolve`'s
    /// coverage tables). Honours the step timeout via `wait_with_timeout`.
    Inherit,
    /// Capture stdout+stderr into `StepOutput` — for ops that parse stdout
    /// (`datapackage_describe`) or surface child stderr. `output()` drains the pipes
    /// (no fill-up deadlock); the timeout is not enforced here (parity with the
    /// capturing command path today).
    Capture,
}

/// Spawn `program args…` in `ctx.dir` with `ctx.env`, mapping failures to the engine's
/// retry taxonomy: a **spawn** failure (missing binary — deterministic) is a
/// NON-retryable [`Error::StepExecution`] so a bad binary never burns 3 attempts of a
/// 4 h job; a **non-zero exit** is a retryable [`Error::StepFailed`]; a **deadline** is
/// [`Error::StepTimeout`] (retryable). `name` labels timeout/exec errors (the runner
/// rewrites only `StepFailed.step`).
fn run_process(
    program: &str,
    args: &[String],
    ctx: &OpContext,
    mode: OutputMode,
    name: &str,
) -> Result<StepOutput> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(ctx.dir).envs(ctx.env);

    match mode {
        OutputMode::Inherit => {
            cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
            let mut child = cmd.spawn().map_err(|e| Error::StepExecution {
                step: name.to_string(),
                source: e,
            })?;
            let status = crate::engine::wait_with_timeout(&mut child, ctx.timeout, name)?;
            if !status.success() {
                return Err(Error::StepFailed {
                    step: String::new(), // runner rewrites with the step name
                    code: status.code().unwrap_or(1),
                    stderr: String::new(),
                });
            }
            Ok(StepOutput {
                stderr: String::new(),
                stdout: None,
            })
        }
        OutputMode::Capture => {
            let out = cmd.output().map_err(|e| Error::StepExecution {
                step: name.to_string(),
                source: e,
            })?;
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if !out.status.success() {
                return Err(Error::StepFailed {
                    step: String::new(),
                    code: out.status.code().unwrap_or(1),
                    stderr,
                });
            }
            Ok(StepOutput {
                stderr,
                stdout: Some(String::from_utf8_lossy(&out.stdout).to_string()),
            })
        }
    }
}

/// `["run", "--script", <script>, <extra…>]` — the shared `uv run --script` invocation
/// for the uv-backed Python operators. Factored out so its arg order is unit-testable.
fn uv_run_args(script: &str, extra: &[String]) -> Vec<String> {
    let mut a = vec![
        "run".to_string(),
        "--script".to_string(),
        script.to_string(),
    ];
    a.extend_from_slice(extra);
    a
}

/// Materialize an `include_str!`-embedded operator script to a version-stamped cache
/// under the temp dir (write-if-changed), returning the path to run. Pinning the
/// script into the binary means `op@<ver>` addresses **exact** bytes — a script change
/// is a version bump + rebuild, never a silent edit. This is the reproducibility
/// contract the old "call the script by relative path" step lacked.
fn materialize_frozen_script(name: &str, version: &str, bytes: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("arcform-op-{}-{}", name, version));
    std::fs::create_dir_all(&dir)
        .map_err(|e| fetch_failed(format!("{}: cache dir {}: {}", name, dir.display(), e)))?;
    let path = dir.join(format!("{}.py", name));
    let needs_write = std::fs::read_to_string(&path)
        .map(|s| s != bytes)
        .unwrap_or(true);
    if needs_write {
        std::fs::write(&path, bytes)
            .map_err(|e| fetch_failed(format!("{}: write {}: {}", name, path.display(), e)))?;
    }
    Ok(path)
}

// ─────────────────────────────────────────────────────────────────────────────
// parquet_export (activate) — the terminal Parquet output as a first-class asset.
//
// Replaces the opaque `COPY … (COMPRESSION zstd)` step whose SQL the engine can't
// introspect, so it becomes a graph island that can silently skip and ship a stale
// artifact. By declaring `reads: [input]`, the export sits *in* the graph downstream
// of the table it exports — no manual `depends_on`, no silent stale-ship.
// ─────────────────────────────────────────────────────────────────────────────

struct ParquetExport;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParquetExportConfig {
    /// Source table or view in the pipeline DB to export.
    input: String,
    /// Destination Parquet path, relative to the manifest directory.
    dest: String,
    /// Parquet codec. Defaults to `zstd`.
    #[serde(default = "default_zstd")]
    compression: String,
    /// Optional Parquet row-group size.
    #[serde(default)]
    row_group_size: Option<u64>,
    /// Optional `ORDER BY` clause applied to the export.
    #[serde(default)]
    order_by: Option<String>,
    /// Optional Parquet **key-value metadata**, written into the file's footer.
    ///
    /// Keys and values are both strings and are written as their **UTF-8 bytes**.
    /// Parquet's footer map is untyped bytes, so DuckDB reads them back as `BLOB`:
    /// `SELECT decode(key), decode(value) FROM parquet_kv_metadata('f.parquet')`
    /// recovers the strings. **`value::VARCHAR` does not** — casting a `BLOB` to
    /// `VARCHAR` yields DuckDB's escaped rendering (`"` becomes `\x22`), not the
    /// text that was written. `decode()` is the read-back.
    ///
    /// Deliberately untyped: this operator carries whatever a protocol wants
    /// stamped and takes no view on what the keys mean.
    ///
    /// A `BTreeMap`, so the entries are emitted in sorted key order however the
    /// `with:` block lists them. Parquet stores the map as an ordered list and
    /// DuckDB writes it in the order given, so an unordered map here would move
    /// the output bytes between runs. See [`parquet_export_sql`].
    #[serde(default)]
    metadata: BTreeMap<String, String>,
}

fn default_zstd() -> String {
    "zstd".to_string()
}

impl ParquetExportConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("parquet_export: invalid `with:` config: {}", e))
        })
    }
}

/// A DuckDB single-quoted string literal: wrap in `'` and double any interior `'`.
///
/// That is the whole escape — a plain `'…'` literal in DuckDB processes no backslash
/// escapes (unlike `E'…'`), so a backslash, a newline and a `"` all pass through as
/// themselves. Verified against DuckDB 1.5.4 for `\`, LF and `"`.
fn sql_string_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Build the `COPY … TO … (…)` statement a `parquet_export` step executes.
///
/// Split out of [`Operator::run`] so the statement — and above all its option list,
/// which is where the output bytes are decided — is testable without a DuckDB
/// connection or a written file.
///
/// **Byte-reproducibility.** An export declaring no `metadata` emits no
/// `KV_METADATA` option at all, so its statement is character-identical to the one
/// this operator built before key-value metadata existed and its output bytes do not
/// move. An export that does declare metadata necessarily changes the footer and so
/// the file's hash; the `order_by` clause that makes an export reproducible still
/// does, because the entries are emitted in sorted key order. An empty map takes the
/// no-metadata path for the same reason it must: DuckDB rejects `KV_METADATA {}` as a
/// syntax error.
fn parquet_export_sql(cfg: &ParquetExportConfig, dest: &Path) -> String {
    let order = cfg
        .order_by
        .as_deref()
        .map(|o| format!(" ORDER BY {}", o))
        .unwrap_or_default();
    let mut opts = format!("FORMAT parquet, COMPRESSION {}", cfg.compression);
    if let Some(rg) = cfg.row_group_size {
        opts.push_str(&format!(", ROW_GROUP_SIZE {}", rg));
    }
    if !cfg.metadata.is_empty() {
        let entries: Vec<String> = cfg
            .metadata
            .iter()
            .map(|(k, v)| format!("{}: {}", sql_string_literal(k), sql_string_literal(v)))
            .collect();
        opts.push_str(&format!(", KV_METADATA {{{}}}", entries.join(", ")));
    }
    format!(
        "COPY (SELECT * FROM {input}{order}) TO '{dest}' ({opts});",
        input = cfg.input,
        order = order,
        dest = dest.display(),
        opts = opts,
    )
}

impl Operator for ParquetExport {
    fn name(&self) -> &'static str {
        "parquet_export"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = ParquetExportConfig::parse(with)?;
        Ok(OpAssets {
            reads: vec![cfg.input.to_lowercase()],
            produces: vec![cfg.dest.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = ParquetExportConfig::parse(with)?;
        let dest = ctx.dir.join(&cfg.dest);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let sql = parquet_export_sql(&cfg, &dest);

        let conn = duckdb::Connection::open(ctx.db_path).map_err(|e| Error::StepFailed {
            step: String::new(),
            code: 1,
            stderr: format!("parquet_export: open db {}: {}", ctx.db_path.display(), e),
        })?;
        conn.execute_batch(&sql).map_err(|e| Error::StepFailed {
            step: String::new(),
            code: 1,
            stderr: format!("parquet_export: {}", e),
        })?;

        Ok(StepOutput {
            stderr: String::new(),
            stdout: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// http_fetch (ingress) — the curl/wget of the catalog: an authenticated GET
// streamed atomically to `out`, with a **content-freshness contract**.
//
// Built on **ureq** — already an arcform dependency, blocking, ~0 transitive
// weight, http/https only. Resilience (retry + backoff) comes from the ENGINE's
// step-retry loop, which already wraps every op run via `defaults.retry`. A
// default User-Agent is always set (gov registries — SEC — 403 a missing UA) and
// any `headers` the Protocol supplies are layered on top.
//
// The one thing `command: curl` can't give cleanly, and the reason this operator
// is worth owning: the **freshness contract** ([`crate::ingress_meta`]). Every
// fetch records the remote identity (ETag / Last-Modified / sha256) in a sidecar
// `<out>.arcmeta`; the next run replays it as an `If-None-Match` conditional
// request, so an unchanged 127 MB remote answers `304` and is not re-downloaded.
// The validator recorded is the first hop's where that hop offers one, and the
// last hop's otherwise — see the redirect loop in `run`.
// Paired with the `fresh` precondition (which HEAD-probes the same sidecar), a
// step re-runs — and propagates downstream — only when the remote actually
// changed: content-addressed ingress, not the clock-based mtime `modified_after`.
// This is the workhorse that retires `fetch_edgar`/`fetch_gleif` and the SEC fetch.
//
// REACHABILITY (2026-07-24). This operator, `html_link_discover`, and the `fresh`
// precondition are the crate's only ureq users, and ureq is the only runtime-linked
// consumer of the rustls/ring TLS stack. None of them is reachable through the
// published library surface — `arc::spec` exports the spec loader alone (src/lib.rs);
// the engine, the operator catalog, and this operator are all private. A pipeline is
// only ever run through the `arc` CLI. So a crate linking arc with
// `default-features = false` — brightfield's desktop shell, which wants only the spec
// loader — cannot call this path, yet before this gating still compiled ureq (and
// rustls + ring) into its binary as dead weight. The path is therefore gated behind
// `http-fetch` (Cargo.toml), which `cli` pulls in; a no-CLI consumer drops ureq and
// its whole TLS stack from the build. (DuckDB still links in via the private engine,
// which is a separate, larger surface — out of scope here.)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "http-fetch")]
struct HttpFetch;

#[cfg(feature = "http-fetch")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpFetchConfig {
    /// Source URL (http/https).
    url: String,
    /// Destination path, relative to the manifest directory. Written atomically.
    out: String,
    /// Extra request headers. A default `User-Agent` is set unless overridden here.
    #[serde(default)]
    headers: BTreeMap<String, String>,
    /// The artifact's SHA-256, pinned by the Protocol. Naming the bytes is the one
    /// thing that lets the fetch skip revalidation: a run cannot disagree with an
    /// origin it never asks, and the manifest has said which bytes it wants.
    #[serde(default)]
    sha256: Option<String>,
}

#[cfg(feature = "http-fetch")]
impl HttpFetchConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("http_fetch: invalid `with:` config: {}", e))
        })
    }

    /// The pinned digest, normalised, or a manifest error naming the bad value — a
    /// pin that cannot match anything is a typo, and a typo that reads as "always
    /// re-download" would be the worst of both.
    fn pinned_digest(&self) -> Result<Option<String>> {
        match self.sha256.as_deref() {
            None => Ok(None),
            Some(raw) => crate::fetch_cache::parse_digest(raw)
                .map(Some)
                .ok_or_else(|| {
                    Error::ManifestValidation(format!(
                        "http_fetch: `sha256: {raw}` is not a SHA-256 (expected 64 hex characters)"
                    ))
                }),
        }
    }

    /// Whether the request carries a credential. The shared cache is keyed by URL, so
    /// it cannot tell one credential's bytes from another's — a credentialed fetch
    /// therefore neither reads from it nor writes to it. Same two headers ureq drops
    /// across a redirect, and for the same reason.
    fn is_credentialed(&self) -> bool {
        self.headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("authorization") || k.eq_ignore_ascii_case("cookie"))
    }
}

/// Header carrying the validator of the content a redirect points at, as opposed to
/// the validator of the redirect response itself.
#[cfg(feature = "http-fetch")]
const LINKED_ETAG: &str = "X-Linked-ETag";

/// Redirect hops followed before the fetch gives up. Matches ureq's own default.
#[cfg(feature = "http-fetch")]
const MAX_REDIRECTS: u32 = 5;

/// What a hop of the redirect chain said about the artifact. The fetch keeps the
/// first hop's where that hop offers a validator, because that is the hop `url`
/// addresses; where it offers none, the last hop's, and the conditional request is
/// forwarded down the chain to reach it.
#[cfg(feature = "http-fetch")]
#[derive(Default)]
struct RemoteIdentity {
    etag: Option<String>,
    last_modified: Option<String>,
    content_sha256: Option<String>,
}

#[cfg(feature = "http-fetch")]
impl RemoteIdentity {
    /// True when the hop offered nothing a conditional request could be built from.
    fn offers_nothing(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none() && self.content_sha256.is_none()
    }

    fn read(resp: &ureq::Response) -> Self {
        let linked = resp.header(LINKED_ETAG).map(str::to_string);
        Self {
            content_sha256: linked.as_deref().and_then(sha256_validator),
            // `ETag` is the response's own validator and is preferred where the
            // origin sends one; a redirect that sends only `LINKED_ETAG` has that
            // header as its sole offered validator.
            etag: resp
                .header("ETag")
                .map(str::to_string)
                .or_else(|| linked.clone()),
            last_modified: resp.header("Last-Modified").map(str::to_string),
        }
    }
}

/// The digest inside a validator that is a bare 64-character hex string, unquoted
/// and lowercased. `None` for any other shape.
#[cfg(feature = "http-fetch")]
fn sha256_validator(validator: &str) -> Option<String> {
    let v = validator.trim().trim_start_matches("W/").trim_matches('"');
    (v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).then(|| v.to_ascii_lowercase())
}

/// Resolve a `Location` against the URL that issued it, so a relative redirect
/// target is followed the way ureq would have followed it.
#[cfg(feature = "http-fetch")]
fn join_location(base: &str, location: &str) -> Option<String> {
    Some(url::Url::parse(base).ok()?.join(location).ok()?.to_string())
}

/// The remote is unchanged: the artifact and its sidecar stay as they are.
#[cfg(feature = "http-fetch")]
fn unchanged() -> StepOutput {
    StepOutput {
        stderr: String::new(),
        stdout: None,
    }
}

/// Map any fetch/IO failure to the retryable `StepFailed` so the engine's
/// step-retry loop re-attempts it with backoff.
fn fetch_failed(msg: String) -> Error {
    Error::StepFailed {
        step: String::new(), // runner overwrites with the step name
        code: 1,
        stderr: msg,
    }
}

/// Best-effort wall-clock stamp on a sidecar — audit only, and the one field a
/// cached run cannot reproduce from an uncached one.
#[cfg(feature = "http-fetch")]
fn now_unix() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// A record pairing **this step's** addressing — its `url`, its request headers — with
/// `entry`'s content identity, which is the half that was verified against bytes.
///
/// It is what crosses the boundary in each direction, and the direction is why the
/// split matters.
///
/// Reading, it is the sidecar this Protocol writes for bytes the shared cache
/// supplied, and the equality is the whole of local≡cached parity: the record a cached
/// run leaves behind is the record the transfer would have left, field for field, down
/// to `fetched_unix`, which is a clock and differs between any two runs.
///
/// Writing, `entry` is a `<out>.arcmeta` — plain YAML inside a Protocol directory,
/// which anything that can write a file can author, and `url` is as authored as
/// `sha256` is. Re-addressing it here is what stops a Protocol seeding the shared store
/// for a URL its step never named.
#[cfg(feature = "http-fetch")]
fn adopt(
    cfg: &HttpFetchConfig,
    entry: &crate::ingress_meta::FetchMeta,
) -> crate::ingress_meta::FetchMeta {
    crate::ingress_meta::FetchMeta {
        url: cfg.url.clone(),
        request_headers: cfg.headers.clone(),
        etag: entry.etag.clone(),
        last_modified: entry.last_modified.clone(),
        sha256: entry.sha256.clone(),
        content_sha256: entry.content_sha256.clone(),
        fetched_unix: now_unix(),
    }
}

/// The sidecar for a **pinned** artifact the fetch did not transfer. Where the shared
/// entry is the pinned bytes it also carries the validators, so a later run of the
/// same Protocol *without* the pin still has something to revalidate with; where it
/// is not, the digest is all that is known and the sidecar says so.
#[cfg(feature = "http-fetch")]
fn pinned_meta(
    cfg: &HttpFetchConfig,
    digest: &str,
    entry: Option<&crate::ingress_meta::FetchMeta>,
) -> crate::ingress_meta::FetchMeta {
    match entry.filter(|e| e.sha256.eq_ignore_ascii_case(digest)) {
        Some(e) => adopt(cfg, e),
        None => crate::ingress_meta::FetchMeta {
            url: cfg.url.clone(),
            request_headers: cfg.headers.clone(),
            sha256: digest.to_string(),
            fetched_unix: now_unix(),
            ..Default::default()
        },
    }
}

/// Copy a cache entry into the Protocol and leave the sidecar an uncached fetch would
/// have left. The one line of output is the point of the feature being visible: a run
/// that says nothing is indistinguishable from one that downloaded 127 MB again.
#[cfg(feature = "http-fetch")]
fn serve_from_cache(
    cache: &crate::fetch_cache::FetchCache,
    cfg: &HttpFetchConfig,
    meta: &crate::ingress_meta::FetchMeta,
    out: &Path,
) -> Result<StepOutput> {
    cache.materialise(meta, out).map_err(|e| {
        fetch_failed(format!(
            "http_fetch: {} from the shared cache: {}",
            cfg.url, e
        ))
    })?;
    let _ = crate::ingress_meta::write(out, meta);
    {
        use owo_colors::OwoColorize;
        eprintln!(
            "{} {} from the shared fetch cache — no transfer",
            "cached:".dimmed(),
            cfg.out
        );
    }
    Ok(unchanged())
}

/// The remote is unchanged. Either the Protocol already holds the bytes, in which
/// case the shared cache is seeded from them the first time so the *next* Protocol
/// naming this URL transfers nothing — or the cache holds them and the Protocol does
/// not, in which case they are copied in.
///
/// A `304` with neither is a remote answering "unchanged" about bytes nobody has:
/// nothing was downloaded, so nothing changes here either.
#[cfg(feature = "http-fetch")]
fn keep_or_materialise(
    cfg: &HttpFetchConfig,
    out: &Path,
    prior: Option<&crate::ingress_meta::FetchMeta>,
    shared: Option<&crate::ingress_meta::FetchMeta>,
    cache: Option<&crate::fetch_cache::FetchCache>,
) -> Result<StepOutput> {
    if let Some(p) = prior {
        if let Some(c) = cache
            && !c.holds(&cfg.url)
        {
            // The store files a locator under the record's own `url`, and `p` is this
            // Protocol's `<out>.arcmeta`. `adopt` re-addresses it to the URL the step
            // named, so what a Protocol can seed is bounded by what it fetched — and
            // the guard above then asks about the URL the write targets, so an entry
            // that is already there is not refiled on every run. `store` hashes what it
            // is given and refuses bytes that do not match the key, so a Protocol whose
            // copy has rotted cannot seed the cache with it.
            let _ = c.store(&adopt(cfg, p), out);
        }
        return Ok(unchanged());
    }
    match (cache, shared) {
        (Some(c), Some(entry)) => serve_from_cache(c, cfg, &adopt(cfg, entry), out),
        _ => Ok(unchanged()),
    }
}

#[cfg(feature = "http-fetch")]
impl Operator for HttpFetch {
    fn name(&self) -> &'static str {
        "http_fetch"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = HttpFetchConfig::parse(with)?;
        // A pin that cannot be a digest is a manifest error, and this is the
        // load-time gate — refusing it here means a run never starts on a pin that
        // could not have matched.
        cfg.pinned_digest()?;
        // The network source is not a graph node; only the local artifact is.
        Ok(OpAssets {
            reads: vec![],
            produces: vec![cfg.out.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        use std::io::{Read, Write};

        use sha2::{Digest, Sha256};

        use crate::ingress_meta::{self, DEFAULT_UA, FetchMeta};

        let cfg = HttpFetchConfig::parse(with)?;
        let pin = cfg.pinned_digest()?;
        let out = ctx.dir.join(&cfg.out);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // The shared cache, unless this fetch carries a credential — see
        // `is_credentialed`. Every use below is `Option`-guarded, so a run without one
        // takes exactly the path this operator took before the cache existed.
        let cache = ctx.cache.filter(|_| !cfg.is_credentialed());

        let prior = ingress_meta::read(&out).filter(|_| out.exists());

        // ── Pinned ───────────────────────────────────────────────────────────────
        // A pinned digest names the bytes, and bytes that are already named have
        // nothing to revalidate: the origin cannot tell this Protocol anything it did
        // not already say it wanted. This is the ONLY route that skips the origin, and
        // it is the reason the default route can afford not to.
        if let Some(ref want) = pin {
            if out.exists()
                && crate::fetch_cache::hash_file(&out).is_ok_and(|on_disk| on_disk == *want)
            {
                if let Some(c) = cache
                    && !c.holds(&cfg.url)
                {
                    let _ = c.store(&pinned_meta(&cfg, want, prior.as_ref()), &out);
                }
                return Ok(unchanged());
            }
            if let Some(c) = cache {
                // The URL first, then the digest on its own: an object filed by
                // another URL is still the artifact this manifest asked for, because
                // the manifest asked for it by hash.
                let entry = c.lookup(&cfg.url);
                let hit = entry
                    .as_ref()
                    .is_some_and(|e| e.sha256.eq_ignore_ascii_case(want))
                    || c.pinned_object(want).is_some();
                if hit {
                    let meta = pinned_meta(&cfg, want, entry.as_ref());
                    return serve_from_cache(c, &cfg, &meta, &out);
                }
            }
        }

        // ── Revalidated ──────────────────────────────────────────────────────────
        // What is known about these bytes already: the Protocol's own sidecar, which
        // describes the copy in `out`, or — when it has none — the shared cache's
        // entry for the same URL, verified on the way out of the store. Either way the
        // request below carries a validator, so an unchanged remote answers `304` and
        // the payload is not transferred a second time.
        let shared = match (&prior, cache) {
            (None, Some(c)) => c.lookup(&cfg.url),
            _ => None,
        };
        let known = prior.as_ref().or(shared.as_ref());

        // Redirects are followed here rather than by ureq. ureq follows them itself by
        // default, and by the time it returns, the only headers left on the `Response`
        // are the last hop's — for a resolve URL that redirects to a signed, expiring
        // storage URL, that is the storage object's validator, which the origin does
        // not accept in a later `If-None-Match`.
        let agent = ureq::builder().redirects(0).build();
        let mut url = cfg.url.clone();
        let mut hop = 0u32;
        let mut identity = RemoteIdentity::default();

        let resp = loop {
            // Default UA first, then Protocol overrides (a `User-Agent` key wins).
            let mut req = agent.get(&url).set("User-Agent", DEFAULT_UA);
            for (k, v) in &cfg.headers {
                // ureq drops `Authorization` and `Cookie` when it follows a redirect;
                // following by hand has to drop them too, or a Protocol's credential
                // reaches whichever host the origin points at.
                if hop > 0
                    && (k.eq_ignore_ascii_case("authorization") || k.eq_ignore_ascii_case("cookie"))
                {
                    continue;
                }
                req = req.set(k, v);
            }
            // The freshness contract: where this artifact has been fetched before —
            // by this Protocol, or by any Protocol, into the shared cache — replay the
            // stored ETag / Last-Modified as a conditional request. Sent on every hop,
            // because the stored validator belongs to whichever hop offered one — for
            // a redirect that carries no validator of its own that is the redirect
            // target, which is only asked if the header travels with the fetch. An
            // unchanged remote answers `304` and we keep the bytes.
            if let Some(p) = known {
                if let Some(ref etag) = p.etag {
                    req = req.set("If-None-Match", etag);
                }
                if let Some(ref lm) = p.last_modified {
                    req = req.set("If-Modified-Since", lm);
                }
            }

            let resp = match req.call() {
                Ok(resp) => resp,
                // 304 Not Modified — the remote is byte-unchanged. The Protocol's own
                // copy (and its sidecar) stay as they are, so its content identity —
                // hence downstream staleness — is stable; a Protocol with no copy of
                // its own takes the confirmed bytes from the shared cache.
                Err(ureq::Error::Status(304, _)) => {
                    return keep_or_materialise(&cfg, &out, prior.as_ref(), shared.as_ref(), cache);
                }
                Err(e) => return Err(fetch_failed(format!("http_fetch: GET {}: {}", cfg.url, e))),
            };
            if resp.status() == 304 {
                return keep_or_materialise(&cfg, &out, prior.as_ref(), shared.as_ref(), cache);
            }

            if hop == 0 {
                identity = RemoteIdentity::read(&resp);
                // The origin declared the artifact's content hash in the response head.
                // Where it is the hash of bytes we already hold — in this Protocol or
                // in the shared cache — the body is not read at all, and the
                // declaration is recorded even if the sidecar predates the field, which
                // costs no transfer.
                //
                // The comparison is against `sha256` and nothing else, because `sha256`
                // is the one field of a record that has been hashed against the bytes
                // it describes: `FetchCache::store` refuses bytes that do not match it
                // and `FetchCache::lookup` re-hashes the object before returning it.
                // `content_sha256` is the origin's word carried verbatim from a response
                // head into the record, so a declaration matched against *it* is a
                // record confirming itself — and the case it would decide is the one
                // where the verified hash says these are other bytes.
                if let Some(declared) = identity.content_sha256.as_deref()
                    && let Some(p) = known
                    && p.sha256 == declared
                {
                    if prior.is_some() && p.content_sha256.as_deref() != Some(declared) {
                        let mut refreshed = p.clone();
                        refreshed.content_sha256 = Some(declared.to_string());
                        let _ = ingress_meta::write(&out, &refreshed);
                    }
                    return keep_or_materialise(&cfg, &out, prior.as_ref(), shared.as_ref(), cache);
                }
            }

            let location = resp.header("Location").map(str::to_string);
            match location {
                Some(loc) if (300..400).contains(&resp.status()) => {
                    hop += 1;
                    if hop > MAX_REDIRECTS {
                        return Err(fetch_failed(format!(
                            "http_fetch: GET {}: more than {} redirects",
                            cfg.url, MAX_REDIRECTS
                        )));
                    }
                    url = join_location(&url, &loc).ok_or_else(|| {
                        fetch_failed(format!(
                            "http_fetch: GET {}: unresolvable redirect to {}",
                            cfg.url, loc
                        ))
                    })?;
                }
                _ => break resp,
            }
        };

        // An origin whose 3xx carries no `ETag`, `Last-Modified` or `X-Linked-ETag` —
        // the shape of an http→https upgrade or a release redirect — leaves nothing to
        // record, and a sidecar with no validator forfeits the next `304`. Take the
        // final hop's instead; the conditional above is sent to every hop, so it
        // reaches the hop that issued it.
        if identity.offers_nothing() {
            identity = RemoteIdentity::read(&resp);
        }

        // Stream to a sibling `.part` file, hashing as we go, then atomically rename —
        // a killed run never leaves a truncated artifact that looks complete.
        let tmp = out.with_extension("part");
        let mut reader = resp.into_reader();
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| fetch_failed(format!("http_fetch: create {}: {}", tmp.display(), e)))?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = reader
                .read(&mut buf)
                .map_err(|e| fetch_failed(format!("http_fetch: read body {}: {}", cfg.url, e)))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])
                .map_err(|e| fetch_failed(format!("http_fetch: write {}: {}", tmp.display(), e)))?;
            hasher.update(&buf[..n]);
        }
        let _ = file.sync_all();
        drop(file);
        let digest = format!("{:x}", hasher.finalize());

        // A pin the transfer does not satisfy is refused before the bytes land: the
        // manifest said which artifact it wanted and this is not it. Non-retryable —
        // the same URL will serve the same bytes on the next attempt — and the partial
        // file goes, so nothing downstream can read what was rejected.
        if let Some(ref want) = pin
            && !digest.eq_ignore_ascii_case(want)
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::StepExecution {
                step: "http_fetch".to_string(),
                source: std::io::Error::other(format!(
                    "{} served bytes hashing to {digest}, but the Protocol pinned {want}",
                    cfg.url
                )),
            });
        }

        std::fs::rename(&tmp, &out)
            .map_err(|e| fetch_failed(format!("http_fetch: rename {}: {}", out.display(), e)))?;

        // Persist the content identity for next run's conditional request + the
        // `fresh` precondition's HEAD probe. Best-effort: a failed sidecar write
        // doesn't fail the fetch (it just forfeits the next conditional/skip).
        let meta = FetchMeta {
            url: cfg.url.clone(),
            request_headers: cfg.headers.clone(),
            etag: identity.etag,
            last_modified: identity.last_modified,
            sha256: digest,
            content_sha256: identity.content_sha256,
            fetched_unix: now_unix(),
        };
        let _ = ingress_meta::write(&out, &meta);
        // And file the bytes so the next Protocol to name this URL revalidates rather
        // than transfers. Best-effort for the same reason: a cache that will not take
        // a write costs a future transfer, not this one.
        if let Some(c) = cache {
            let _ = c.store(&meta, &out);
        }

        Ok(StepOutput {
            stderr: String::new(),
            stdout: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// opendal_fetch (ingress) — the same fetch over **Apache OpenDAL**, a single
// operator that reads a scheme-dispatched `from` URL across 40+ backends
// (http/https, s3, gcs, azblob, …) with a built-in `RetryLayer` (per-request
// backoff, *on top of* the engine step-retry).
//
// MEASURED TRADE-OFF (2026-07-14 spike): OpenDAL's Http *builder* exposes only
// basic-auth / bearer — no header setter — but arbitrary request headers (a SEC
// User-Agent) ARE reachable by injecting a reqwest client (`default_headers`)
// through a custom `ReqwestTransport` (wired below; proven on the live SEC
// UA-gated fetch). The cost is +2 deps and ~25 lines of transport plumbing vs
// ureq's one `.set(k, v)`. OpenDAL also buffers the whole object in memory
// (blocking `read`) and needs a tokio runtime, where ureq streams with zero
// runtime. OpenDAL's genuine win is **s3://securelake** and multi-backend
// portability — one operator, many stores. Feature-gated behind `opendal`
// (Cargo.toml) so the default binary stays lean.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "opendal")]
struct OpendalFetch;

#[cfg(feature = "opendal")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpendalFetchConfig {
    /// Scheme-dispatched source: `https://host/key`, `s3://bucket/key`, …
    from: String,
    /// Destination path, relative to the manifest directory. Written atomically.
    to: String,
    /// Extra request headers. Rejected on the http(s) backend (see above).
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

#[cfg(feature = "opendal")]
impl OpendalFetchConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("opendal_fetch: invalid `with:` config: {}", e))
        })
    }
}

#[cfg(feature = "opendal")]
impl Operator for OpendalFetch {
    fn name(&self) -> &'static str {
        "opendal_fetch"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = OpendalFetchConfig::parse(with)?;
        Ok(OpAssets {
            reads: vec![],
            produces: vec![cfg.to.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        use opendal::layers::RetryLayer;
        use opendal::{Operator as DalOperator, services};

        let cfg = OpendalFetchConfig::parse(with)?;
        let out = ctx.dir.join(&cfg.to);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let (scheme, rest) = cfg.from.split_once("://").ok_or_else(|| {
            Error::ManifestValidation(format!(
                "opendal_fetch: `from` must be a scheme URL (https://… or s3://…), got '{}'",
                cfg.from
            ))
        })?;
        // authority = host or bucket; path = the object key relative to backend root.
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));

        let op = match scheme {
            "http" | "https" => {
                let endpoint = format!("{}://{}", scheme, authority);
                let builder = services::Http::default().endpoint(&endpoint);
                let mut dal = DalOperator::new(builder)
                    .map_err(|e| fetch_failed(format!("opendal_fetch: http backend: {}", e)))?;
                // OpenDAL's Http builder has no header setter (only basic-auth /
                // bearer). To carry request headers — e.g. the SEC User-Agent —
                // inject a reqwest client with `default_headers` through a custom
                // ReqwestTransport. More plumbing (+2 deps) than ureq's `.set()`,
                // but it works: proven on the live SEC fetch.
                if !cfg.headers.is_empty() {
                    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
                    let mut hm = HeaderMap::new();
                    for (k, v) in &cfg.headers {
                        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                            Error::ManifestValidation(format!(
                                "opendal_fetch: bad header name '{}': {}",
                                k, e
                            ))
                        })?;
                        let val = HeaderValue::from_str(v).map_err(|e| {
                            Error::ManifestValidation(format!(
                                "opendal_fetch: bad header value for '{}': {}",
                                k, e
                            ))
                        })?;
                        hm.insert(name, val);
                    }
                    let client = reqwest::Client::builder()
                        .default_headers(hm)
                        .build()
                        .map_err(|e| {
                            fetch_failed(format!("opendal_fetch: build http client: {}", e))
                        })?;
                    let transport = opendal_http_transport_reqwest::ReqwestTransport::new(client);
                    dal = dal.with_context(
                        opendal::OperationContext::new()
                            .with_http_transport(opendal::HttpTransporter::new(transport)),
                    );
                }
                dal
            }
            "s3" => {
                // Credentials/region from the standard AWS_* environment — the
                // securelake path. Constructed + retry-wrapped here; exercised
                // when securelake creds are present (not in this spike's run).
                let builder = services::S3::default().bucket(authority);
                DalOperator::new(builder)
                    .map_err(|e| fetch_failed(format!("opendal_fetch: s3 backend: {}", e)))?
            }
            other => {
                return Err(Error::ManifestValidation(format!(
                    "opendal_fetch: unsupported scheme '{}' (http, https, s3)",
                    other
                )));
            }
        };
        let op = op.layer(RetryLayer::new());

        // OpenDAL's blocking bridge requires an entered tokio runtime.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| fetch_failed(format!("opendal_fetch: tokio runtime: {}", e)))?;
        let _guard = rt.enter();
        let bop = opendal::blocking::Operator::new(op)
            .map_err(|e| fetch_failed(format!("opendal_fetch: blocking bridge: {}", e)))?;
        let buf = bop
            .read(path)
            .map_err(|e| fetch_failed(format!("opendal_fetch: read {}: {}", cfg.from, e)))?;

        let tmp = out.with_extension("part");
        std::fs::write(&tmp, buf.to_vec())
            .map_err(|e| fetch_failed(format!("opendal_fetch: write {}: {}", tmp.display(), e)))?;
        std::fs::rename(&tmp, &out)
            .map_err(|e| fetch_failed(format!("opendal_fetch: rename {}: {}", out.display(), e)))?;

        Ok(StepOutput {
            stderr: String::new(),
            stdout: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// datapackage_describe (activate) — emit the Frictionless datapackage.json from the
// built Parquet: finetype types every column (the machine half) overlaid with the
// curated descriptor.overrides.json (the hand half; overrides win, relational keys
// hard-checked against the Parquet).
//
// Wraps an arcform-EMBEDDED copy of describe.py via the uv-run substrate — NOT a
// Rust reimplementation of the JSON merge. That is deliberate: running the identical
// script makes the output byte-identical to the retired scripts/describe.py by
// construction, it needs no serde_json dep (and dodges its float/sort byte-equivalence
// hazards vs Python's json.dump), and the operator becomes reusable by any dataset.
// Python at the edges — the same posture as splink_resolve. describe.py itself has
// no Python deps; it shells the `finetype` CLI (must be on PATH, as today).
//
// Verified byte-identical to the retired script on the live build, save the one field
// `finetype` stamps itself — the per-run `created` timestamp (non-deterministic, and
// pre-existing; the command step had it too). datapackage.json is metadata, not the
// data; the parquet (the byte-equivalence target) is untouched by this op.
// ─────────────────────────────────────────────────────────────────────────────

/// The describe script, pinned into the binary. `@1` == these exact bytes.
const DESCRIBE_PY: &str = include_str!("../operators/datapackage_describe/describe.py");

struct DatapackageDescribe;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DatapackageDescribeConfig {
    /// Built Parquet finetype types (a `reads` asset — keeps `describe` downstream
    /// of the step that produces it). Resolved against ctx.dir.
    parquet: String,
    /// Curated descriptor sidecar (JSON) overlaid onto finetype's base.
    overrides: String,
    /// datapackage.json to write (the `produces` asset). Resolved against ctx.dir.
    out: String,
}

impl DatapackageDescribeConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!(
                "datapackage_describe: invalid `with:` config: {}",
                e
            ))
        })
    }
}

impl Operator for DatapackageDescribe {
    fn name(&self) -> &'static str {
        "datapackage_describe"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = DatapackageDescribeConfig::parse(with)?;
        Ok(OpAssets {
            reads: vec![cfg.parquet.to_lowercase()],
            produces: vec![cfg.out.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = DatapackageDescribeConfig::parse(with)?;
        let parquet = ctx.dir.join(&cfg.parquet);
        let overrides = ctx.dir.join(&cfg.overrides);
        let out = ctx.dir.join(&cfg.out);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let script = materialize_frozen_script("datapackage_describe", "1.0.0", DESCRIBE_PY)?;
        let args = uv_run_args(
            &script.to_string_lossy(),
            &[
                "--parquet".to_string(),
                parquet.display().to_string(),
                "--overrides".to_string(),
                overrides.display().to_string(),
                "--out".to_string(),
                out.display().to_string(),
            ],
        );
        run_process(
            "uv",
            &args,
            ctx,
            OutputMode::Capture,
            "datapackage_describe",
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// finetype_validate (gate) — the fail-closed DRIFT GATE that makes finetype's typed
// schema LOAD-BEARING, not just post-hoc descriptive. Where datapackage_describe
// *describes* the built Parquet, this *enforces* it: it checks every column of the
// terminal Parquet against a self-derived JSON-Schema contract.
//
// IN-ENGINE (no subprocess). The gate opens a throwaway in-memory DuckDB, LOADs the
// finetype DuckDB extension, and runs its `ft_validate(table, schema) → TABLE` macro —
// the model-free (jsonschema-based, no model dir) validator. `ft_validate` melts every
// column to VARCHAR and checks each against its `$.properties.<col>` subschema,
// returning a per-column report {column_name, total, rejects, sample_message}. Any
// rejecting row → the run fails closed, and the offending columns are logged first.
// Running the extension in-process (vs shelling a `finetype` CLI) means the gate needs
// no `finetype`/`duckdb` on PATH — only the built extension artifact, located via the
// `FINETYPE_DUCKDB_EXT` env var (the deployment sets the per-platform build) or a
// per-step `extension:` override.
//
// The contract per dataset is derived from that dataset's OWN produced Parquet, relaxed
// to the string-surviving keywords `ft_validate` enforces once columns are melted to
// VARCHAR — `enum` (closed categorical domains), `pattern`, `minLength`/`maxLength`,
// `const`, `type: string`. Because it is derived from the data, every current row
// validates: the gate is a PASS-THROUGH today and a TRIPWIRE for future Runs (a value
// outside a closed enum, a length past the envelope, a shape break → rejects → the run
// fails). See each dataset's schema.finetype.json `$comment` for what is enforced (and
// what is deliberately NOT: numeric ranges are inert once melted, and NULLs are
// invisible — no not-null enforcement).
//
// CHECK-ONLY is load-bearing for byte-equivalence: the gate READS the Parquet via
// `read_parquet` and never rewrites/reorders/drops rows, so the published bytes are
// unchanged. `allow_unsigned_extensions` MUST be an open-time flag (a runtime `SET`
// fails), so the connection is opened with it via `open_in_memory_with_flags`.
// ─────────────────────────────────────────────────────────────────────────────

struct FinetypeValidate;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinetypeValidateConfig {
    /// Built Parquet to check (a `reads` asset — orders the gate downstream of the
    /// step that produces it, so a rebuilt Parquet re-triggers the gate). Resolved
    /// against ctx.dir.
    parquet: String,
    /// Self-derived JSON-Schema contract to check against (a `reads` asset). Resolved
    /// against ctx.dir.
    schema: String,
    /// OPTIONAL per-step override for the finetype DuckDB extension path. Normally the
    /// path comes from the `FINETYPE_DUCKDB_EXT` env var (the deployment sets the
    /// per-platform build); this field lets a single Protocol pin a specific artifact.
    /// NOT a data asset — deliberately excluded from `assets()`, so manifests that omit
    /// it (all of them) don't churn and lineage is unchanged.
    #[serde(default)]
    extension: Option<String>,
}

impl FinetypeValidateConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("finetype_validate: invalid `with:` config: {}", e))
        })
    }
}

impl Operator for FinetypeValidate {
    fn name(&self) -> &'static str {
        "finetype_validate"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = FinetypeValidateConfig::parse(with)?;
        // A gate produces no artifact (check-only). It READS both the Parquet — which
        // orders it downstream of the export that produces it (so a rebuilt Parquet
        // re-triggers the gate via stale-propagation) — and the schema contract.
        Ok(OpAssets {
            reads: vec![cfg.parquet.to_lowercase(), cfg.schema.to_lowercase()],
            produces: vec![],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = FinetypeValidateConfig::parse(with)?;
        let parquet = ctx.dir.join(&cfg.parquet);
        let schema = ctx.dir.join(&cfg.schema);

        // Locate the finetype DuckDB extension (per-step override, else env var).
        // A missing/unset artifact is a deterministic misconfiguration → non-retryable.
        let ext = finetype_ext_path(cfg.extension.as_deref())?;

        // Open a throwaway in-memory DuckDB with unsigned extensions ALLOWED. This is an
        // OPEN-TIME flag — a runtime `SET allow_unsigned_extensions` is rejected — so it
        // goes through `open_in_memory_with_flags`.
        let config = duckdb::Config::default()
            .allow_unsigned_extensions()
            .map_err(|e| gate_failed(format!("configure duckdb: {e}")))?;
        let conn = duckdb::Connection::open_in_memory_with_flags(config)
            .map_err(|e| gate_failed(format!("open in-memory duckdb: {e}")))?;
        conn.execute_batch(&format!("LOAD '{}';", sql_lit(&ext.display().to_string())))
            .map_err(|e| gate_failed(format!("LOAD finetype extension {}: {e}", ext.display())))?;
        // View the terminal Parquet — read-only, so the published bytes are untouched.
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP VIEW _ftv AS SELECT * FROM read_parquet('{}');",
            sql_lit(&parquet.display().to_string())
        ))
        .map_err(|e| gate_failed(format!("read_parquet {}: {e}", parquet.display())))?;

        // Run the model-free `ft_validate` macro and pull the full per-column report.
        // It auto-detects the `schema` arg as a file path (it does not start with `{`).
        // coalesce guards the nested-column skip rows, which carry NULL total/rejects.
        let report_sql = format!(
            "SELECT column_name, coalesce(total, 0)::BIGINT, coalesce(rejects, 0)::BIGINT, \
             coalesce(sample_message, '') FROM ft_validate('_ftv', '{}') \
             ORDER BY coalesce(rejects, 0) DESC, column_name",
            sql_lit(&schema.display().to_string())
        );
        let mut stmt = conn
            .prepare(&report_sql)
            .map_err(|e| gate_failed(format!("prepare ft_validate: {e}")))?;
        let report = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?, // column_name
                    row.get::<_, i64>(1)?,    // total
                    row.get::<_, i64>(2)?,    // rejects
                    row.get::<_, String>(3)?, // sample_message
                ))
            })
            .map_err(|e| gate_failed(format!("run ft_validate: {e}")))?;

        let mut total_rejects: i64 = 0;
        let mut offenders: Vec<(String, i64, i64, String)> = Vec::new();
        for row in report {
            let (col, total, rejects, msg) =
                row.map_err(|e| gate_failed(format!("read ft_validate report: {e}")))?;
            total_rejects += rejects;
            if rejects > 0 {
                offenders.push((col, total, rejects, msg));
            }
        }

        if total_rejects > 0 {
            // Fail-report UX: surface the offending {column, rejects, sample_message}
            // rows before failing closed — a strict upgrade on the old exit-code signal.
            eprintln!(
                "finetype_validate: {} contract violation(s) in {} vs {} — DRIFT, failing closed:",
                total_rejects, cfg.parquet, cfg.schema
            );
            for (col, total, rejects, msg) in &offenders {
                eprintln!("  ✗ {col}: {rejects}/{total} row(s) reject — {msg}");
            }
            return Err(Error::StepFailed {
                step: String::new(), // runner rewrites with the step name
                code: 1,
                stderr: format!(
                    "finetype_validate: {} column(s) drifted from {} ({} rejecting row(s))",
                    offenders.len(),
                    cfg.schema,
                    total_rejects
                ),
            });
        }

        Ok(StepOutput {
            stderr: String::new(),
            stdout: None,
        })
    }
}

/// Resolve the finetype DuckDB extension path for the in-engine gate: a per-step
/// `extension:` override wins, else the `FINETYPE_DUCKDB_EXT` env var. A missing/unset
/// path is a deterministic misconfiguration → NON-retryable [`Error::StepExecution`]
/// (a bad binary must never burn a 4 h job's retries — parity with the old missing
/// `finetype` CLI case).
fn finetype_ext_path(config_ext: Option<&str>) -> Result<PathBuf> {
    let raw = match config_ext {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => std::env::var("FINETYPE_DUCKDB_EXT").map_err(|_| Error::StepExecution {
            step: "finetype_validate".to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "finetype DuckDB extension not configured: set FINETYPE_DUCKDB_EXT to the built \
                 finetype.duckdb_extension (per-platform), or pass `extension:` in the step",
            ),
        })?,
    };
    let path = PathBuf::from(raw);
    if !path.exists() {
        return Err(Error::StepExecution {
            step: "finetype_validate".to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("finetype DuckDB extension not found at {}", path.display()),
            ),
        });
    }
    Ok(path)
}

/// Map an in-engine gate failure (duckdb open / LOAD / query) to the retryable
/// [`Error::StepFailed`], matching the old shell-out's "finetype error exit → retryable"
/// posture (the runner rewrites the empty `step` with the step name).
fn gate_failed(msg: String) -> Error {
    Error::StepFailed {
        step: String::new(),
        code: 1,
        stderr: format!("finetype_validate: {msg}"),
    }
}

/// Escape single quotes for a SQL string literal — used when interpolating filesystem
/// paths into `LOAD`/`read_parquet`/`ft_validate`.
fn sql_lit(s: &str) -> String {
    s.replace('\'', "''")
}

// ─────────────────────────────────────────────────────────────────────────────
// html_link_discover (ingress) — the "list what's there" primitive: GET an index
// page, regex-pluck the hrefs matching `pattern`, absolutise them against the page
// URL, de-dup preserving document order, and emit a newline-delimited URL list as
// a first-class produced asset.
//
// This is the discovery half of the SEC N-CEN pull (scripts/fetch_ncen.sh): the
// SEC "Form N-CEN Data Sets" page lists each quarter's zip as a root-relative href
// (`/files/dera/data/…ncen_2024q1.zip`); this op turns that page into the list of
// absolute zip URLs a downstream fetch fans out over. Same default UA as http_fetch
// (SEC 403s a missing UA); a Protocol `User-Agent` in `headers` overrides it.
//
// NOTE (byte-equivalence): this + archive_extract are unit-verified building blocks;
// they do NOT by themselves retire fetch_ncen.sh end-to-end, because the engine has
// no fan-out step that maps over the discovered-quarters URL list yet. That fan-out
// is separate work; here we produce the complete list.
// ─────────────────────────────────────────────────────────────────────────────

// Gated with `http_fetch` (both are ureq-backed ingress ops); see the reachability
// note beside `http_fetch` above. The pure URL helpers below (`url_origin`,
// `absolutise`, `discover_links`, …) are ungated — they carry no ureq and their
// unit tests run in every build.
#[cfg(feature = "http-fetch")]
struct HtmlLinkDiscover;

#[cfg(feature = "http-fetch")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HtmlLinkDiscoverConfig {
    /// Index page to fetch (http/https).
    url: String,
    /// Regex tested (unanchored) against each raw `href` value; matches are kept.
    pattern: String,
    /// Destination URL-list path (newline-delimited), relative to the manifest dir.
    out: String,
    /// Extra request headers. A default `User-Agent` is set unless overridden here.
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

#[cfg(feature = "http-fetch")]
impl HtmlLinkDiscoverConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("html_link_discover: invalid `with:` config: {}", e))
        })
    }

    /// Compile `pattern`, mapping a bad regex to a load-time validation error.
    fn compiled_pattern(&self) -> Result<regex::Regex> {
        regex::Regex::new(&self.pattern).map_err(|e| {
            Error::ManifestValidation(format!(
                "html_link_discover: invalid `pattern` regex '{}': {}",
                self.pattern, e
            ))
        })
    }
}

/// The origin (`scheme://authority`) of an absolute URL, or `None` if `url` isn't one.
fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    Some(format!("{}://{}", scheme, authority))
}

/// The RFC-3986 directory base of a page URL — the URL up to and including the last
/// `/` of its path (query/fragment stripped) — that a bare relative href resolves
/// against. Falls back to `origin/` when the path has no slash.
fn url_dir_base(page_url: &str) -> Option<String> {
    let origin = url_origin(page_url)?;
    let rest = &page_url[origin.len()..];
    let path = rest.split(['?', '#']).next().unwrap_or("");
    match path.rfind('/') {
        Some(i) => Some(format!("{}{}", origin, &path[..=i])),
        None => Some(format!("{}/", origin)),
    }
}

/// Whether `s` begins with a URL scheme (`http:`, `mailto:`, …) per RFC-3986.
fn href_has_scheme(s: &str) -> bool {
    match s.find(':') {
        None | Some(0) => false,
        Some(i) => {
            let scheme = &s[..i];
            scheme
                .chars()
                .next()
                .map(|c| c.is_ascii_alphabetic())
                .unwrap_or(false)
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
    }
}

/// Absolutise an `href` against the page URL. Handles absolute URLs (kept verbatim),
/// protocol-relative (`//host/…`), root-relative (`/path` → page origin), and
/// path-relative (resolved against the page's directory base). Returns `None` for
/// empty or fragment-only hrefs.
fn absolutise(page_url: &str, href: &str) -> Option<String> {
    let h = href.trim();
    if h.is_empty() || h.starts_with('#') {
        return None;
    }
    if href_has_scheme(h) {
        return Some(h.to_string());
    }
    if let Some(rest) = h.strip_prefix("//") {
        let scheme = page_url.split("://").next()?;
        if scheme.is_empty() || scheme == page_url {
            return None;
        }
        return Some(format!("{}://{}", scheme, rest));
    }
    if h.starts_with('/') {
        return Some(format!("{}{}", url_origin(page_url)?, h));
    }
    Some(format!("{}{}", url_dir_base(page_url)?, h))
}

/// Extract every `href` in `html` whose value matches `pattern`, absolutise it
/// against `page_url`, and return the list de-duplicated in document order. `limit`
/// caps the number of distinct URLs returned (the `head -n N` of fetch_ncen.sh).
fn discover_links(
    html: &str,
    page_url: &str,
    pattern: &regex::Regex,
    limit: Option<usize>,
) -> Vec<String> {
    // Quoted href forms only (`href="…"` / `href='…'`) — unquoted attributes are
    // rare in the gov index pages this targets.
    let href_re = regex::Regex::new(r#"(?i)href\s*=\s*["']([^"']*)["']"#)
        .expect("static href regex is valid");
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in href_re.captures_iter(html) {
        let href = &cap[1];
        if !pattern.is_match(href) {
            continue;
        }
        let Some(abs) = absolutise(page_url, href) else {
            continue;
        };
        if seen.insert(abs.clone()) {
            out.push(abs);
            if let Some(l) = limit
                && out.len() >= l
            {
                break;
            }
        }
    }
    out
}

#[cfg(feature = "http-fetch")]
impl Operator for HtmlLinkDiscover {
    fn name(&self) -> &'static str {
        "html_link_discover"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = HtmlLinkDiscoverConfig::parse(with)?;
        cfg.compiled_pattern()?; // fail fast on a bad regex at manifest load
        // The network source is not a graph node; only the local URL list is.
        Ok(OpAssets {
            reads: vec![],
            produces: vec![cfg.out.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        use crate::ingress_meta::DEFAULT_UA;

        let cfg = HtmlLinkDiscoverConfig::parse(with)?;
        let pattern = cfg.compiled_pattern()?;
        let out = ctx.dir.join(&cfg.out);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Default UA first, then Protocol overrides (a `User-Agent` key wins).
        let mut req = ureq::get(&cfg.url).set("User-Agent", DEFAULT_UA);
        for (k, v) in &cfg.headers {
            req = req.set(k, v);
        }
        let resp = req
            .call()
            .map_err(|e| fetch_failed(format!("html_link_discover: GET {}: {}", cfg.url, e)))?;
        let body = resp
            .into_string()
            .map_err(|e| fetch_failed(format!("html_link_discover: read {}: {}", cfg.url, e)))?;

        let links = discover_links(&body, &cfg.url, &pattern, None);
        let mut payload = String::new();
        for l in &links {
            payload.push_str(l);
            payload.push('\n');
        }

        // Atomic write: sibling `.part`, then rename — a killed run never leaves a
        // half-written list that looks complete.
        let mut tmp_os = out.clone().into_os_string();
        tmp_os.push(".part");
        let tmp = PathBuf::from(tmp_os);
        std::fs::write(&tmp, payload.as_bytes()).map_err(|e| {
            fetch_failed(format!(
                "html_link_discover: write {}: {}",
                tmp.display(),
                e
            ))
        })?;
        std::fs::rename(&tmp, &out).map_err(|e| {
            fetch_failed(format!(
                "html_link_discover: rename {}: {}",
                out.display(),
                e
            ))
        })?;

        Ok(StepOutput {
            stderr: String::new(),
            stdout: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// archive_extract (ingress) — pull named / pattern-matched members out of a `.zip`
// into `dest` as first-class produced assets. The extraction half of the N-CEN
// pull: each quarter's `ncen_YYYYqN.zip` yields REGISTRANT.tsv and
// FUND_REPORTED_INFO.tsv (the CIK↔LEI / SERIES_ID↔LEI carriers).
//
// Three properties `command: unzip` doesn't give cleanly:
//   • zip-slip guard — a member named `../…` or an absolute path is rejected, so a
//     hostile archive can't write outside `dest`.
//   • atomic per-file write — each member lands via a sibling `.part` + rename, so a
//     killed run never leaves a half-written TSV that looks complete.
//   • case-preserving on disk, case-folded in the graph — the file keeps its real
//     name (`REGISTRANT.tsv`, referenced verbatim by downstream SQL) while the
//     AssetGraph node name is lowercased, matching every other operator.
//
// zip dep: default-features OFF + `deflate-flate2` ONLY. The N-CEN zips are DEFLATE-
// compressed, and plain `deflate` is a zip base meta-feature with no inflate backend
// (→ runtime failure); `deflate-flate2` wires flate2 (already an arcform dep).
// Dropping defaults avoids pulling the bzip2/zstd/aes C backends.
// ─────────────────────────────────────────────────────────────────────────────

struct ArchiveExtract;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveExtractConfig {
    /// Source `.zip`, relative to the manifest dir (a `reads` asset).
    archive: String,
    /// Explicit member names to extract (exact match on the zip-internal name).
    #[serde(default)]
    members: Vec<String>,
    /// Regex tested (unanchored) against each member name; matches are extracted.
    #[serde(default)]
    pattern: Option<String>,
    /// Destination directory, relative to the manifest dir. Members are written under
    /// it preserving their real-case relative path.
    dest: String,
}

impl ArchiveExtractConfig {
    fn parse(with: &Value) -> Result<Self> {
        let cfg: Self = serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("archive_extract: invalid `with:` config: {}", e))
        })?;
        if cfg.members.is_empty() && cfg.pattern.is_none() {
            return Err(Error::ManifestValidation(
                "archive_extract: specify at least one of `members` or `pattern`".to_string(),
            ));
        }
        Ok(cfg)
    }

    /// Compile `pattern` (if any), mapping a bad regex to a load-time validation error.
    fn compiled_pattern(&self) -> Result<Option<regex::Regex>> {
        match &self.pattern {
            None => Ok(None),
            Some(p) => regex::Regex::new(p).map(Some).map_err(|e| {
                Error::ManifestValidation(format!(
                    "archive_extract: invalid `pattern` regex '{}': {}",
                    p, e
                ))
            }),
        }
    }
}

/// Whether a zip member is selected: an exact name in `members`, or a `pattern` hit.
fn member_selected(name: &str, members: &[String], pattern: Option<&regex::Regex>) -> bool {
    members.iter().any(|m| m == name) || pattern.map(|re| re.is_match(name)).unwrap_or(false)
}

/// Resolve a zip member name to a SAFE relative path (case preserved). Returns `None`
/// for a zip-slip attempt — an absolute path, a Windows drive, or any `..` component —
/// so nothing can be written outside `dest`.
fn safe_relative(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') || name.starts_with('\\') {
        return None;
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return None; // Windows drive-absolute (C:\…)
    }
    let mut rel = PathBuf::new();
    for comp in name.split(['/', '\\']) {
        match comp {
            "" | "." => continue,
            ".." => return None,
            c => rel.push(c),
        }
    }
    if rel.as_os_str().is_empty() {
        return None;
    }
    Some(rel)
}

/// Extract the selected members of a zip `reader` into `dest`: case-preserving on
/// disk, zip-slip guarded, atomic per-file write. Returns the written paths. Factored
/// out of `run` so the extraction is unit-testable against an in-memory zip.
fn extract_members<R: std::io::Read + std::io::Seek>(
    reader: R,
    dest: &Path,
    members: &[String],
    pattern: Option<&regex::Regex>,
) -> Result<Vec<PathBuf>> {
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| fetch_failed(format!("archive_extract: open zip: {}", e)))?;
    let mut written = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| fetch_failed(format!("archive_extract: read entry {}: {}", i, e)))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if !member_selected(&name, members, pattern) {
            continue;
        }
        let rel = safe_relative(&name).ok_or_else(|| {
            fetch_failed(format!(
                "archive_extract: unsafe member path '{}' (zip-slip rejected)",
                name
            ))
        })?;
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                fetch_failed(format!(
                    "archive_extract: mkdir {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }
        // Atomic per-file write: sibling `<target>.part`, then rename.
        let mut tmp_os = target.clone().into_os_string();
        tmp_os.push(".part");
        let tmp = PathBuf::from(tmp_os);
        let mut f = std::fs::File::create(&tmp).map_err(|e| {
            fetch_failed(format!("archive_extract: create {}: {}", tmp.display(), e))
        })?;
        std::io::copy(&mut entry, &mut f)
            .map_err(|e| fetch_failed(format!("archive_extract: extract '{}': {}", name, e)))?;
        let _ = f.sync_all();
        drop(f);
        std::fs::rename(&tmp, &target).map_err(|e| {
            fetch_failed(format!(
                "archive_extract: rename {}: {}",
                target.display(),
                e
            ))
        })?;
        written.push(target);
    }
    Ok(written)
}

impl Operator for ArchiveExtract {
    fn name(&self) -> &'static str {
        "archive_extract"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = ArchiveExtractConfig::parse(with)?;
        cfg.compiled_pattern()?; // fail fast on a bad regex at manifest load
        // Node names are lowercased (graph convention); the on-disk files keep real
        // case. Explicit `members` give per-file produced nodes; a pattern-only
        // selection isn't known until the archive is opened, so `dest` stands in as
        // the coarse produced node.
        let produces = if cfg.members.is_empty() {
            vec![cfg.dest.to_lowercase()]
        } else {
            let base = cfg.dest.trim_end_matches('/');
            cfg.members
                .iter()
                .map(|m| format!("{}/{}", base, m).to_lowercase())
                .collect()
        };
        Ok(OpAssets {
            reads: vec![cfg.archive.to_lowercase()],
            produces,
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = ArchiveExtractConfig::parse(with)?;
        let pattern = cfg.compiled_pattern()?;
        let archive_path = ctx.dir.join(&cfg.archive);
        let dest = ctx.dir.join(&cfg.dest);
        std::fs::create_dir_all(&dest).map_err(|e| {
            fetch_failed(format!("archive_extract: mkdir {}: {}", dest.display(), e))
        })?;
        let file = std::fs::File::open(&archive_path).map_err(|e| {
            fetch_failed(format!(
                "archive_extract: open {}: {}",
                archive_path.display(),
                e
            ))
        })?;
        extract_members(file, &dest, &cfg.members, pattern.as_ref())?;
        Ok(StepOutput {
            stderr: String::new(),
            stdout: None,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// splink_resolve (transform) — the probabilistic EDGAR↔GLEIF name match, promoted
// from the opaque `command: uv run --script …/resolve.py …` step to a first-class,
// versioned operator. `op: splink_resolve@1` addresses the EXACT frozen script bytes
// embedded below (SEED=42 Fellegi-Sunter model; see resolve.py) — the reproducibility
// contract the "call the script by relative path" step lacked. Its `reads`/`produces`
// put the resolve *in* the asset graph, so a stale input propagates instead of the
// graph-island silent-skip the path-based step risked.
//
// Wraps the arcform-EMBEDDED resolve.py via the uv-run substrate (materialize_frozen_
// script → uv_run_args → run_process), NOT a Rust reimplementation of Splink — Python
// at the edges, same posture as datapackage_describe. Runs in OutputMode::Inherit so
// resolve.py's `[coverage]` / `[hero tickers]` tables stream live and the step timeout
// (the manifest's `timeout_sec`) is enforced by wait_with_timeout.
//
// SCALE — this is the ~4 h full-corpus job (GLEIF ≈ 3.2M rows). The manifest step MUST
// pin `retry.max_attempts: 1`: a non-zero exit or timeout is retryable in the substrate,
// so without a cap the engine would re-attempt a 4 h job up to `defaults.retry`. (A
// spawn failure — bad `uv`/binary — is already NON-retryable, so a typo never burns
// attempts; but a mid-run failure is the case the manifest cap guards.) `--sample` is a
// smoke-test cap on GLEIF rows and is OMITTED for a published run (see the arg builder).
// ─────────────────────────────────────────────────────────────────────────────

/// The resolve script, pinned into the binary. `@1` == these exact bytes.
const RESOLVE_PY: &str = include_str!("../operators/splink_resolve/resolve.py");

struct SplinkResolve;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SplinkResolveConfig {
    /// EDGAR / SEC-entity Parquet (cik, ticker, company_name) — the crosswalk's left
    /// side. A `reads` asset, resolved against ctx.dir.
    edgar: String,
    /// GLEIF golden-copy Parquet (lei, legal_name, jurisdiction, country, reg_status) —
    /// the right side. A `reads` asset, resolved against ctx.dir.
    gleif: String,
    /// Resolved-crosswalk Parquet to write (the `produces` asset). Resolved against ctx.dir.
    out: String,
    /// Optional GLEIF-row cap for a fast smoke test (reservoir sample, SEED=42). OMITTED
    /// from the argv unless `Some(n)` with `n > 0`; a published run leaves it unset (or 0)
    /// and resolves the full corpus.
    #[serde(default)]
    sample: Option<u64>,
}

impl SplinkResolveConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("splink_resolve: invalid `with:` config: {}", e))
        })
    }
}

/// Build resolve.py's argv tail (everything after the script path). `--sample` is emitted
/// ONLY for a positive cap: a published run passes `sample: None` (or `0`) and the full
/// GLEIF corpus is resolved. Factored out so the omit-on-publish rule is unit-testable
/// without spawning `uv`.
fn splink_resolve_args(edgar: &str, gleif: &str, out: &str, sample: Option<u64>) -> Vec<String> {
    let mut a = vec![
        "--edgar".to_string(),
        edgar.to_string(),
        "--gleif".to_string(),
        gleif.to_string(),
        "--out".to_string(),
        out.to_string(),
    ];
    if let Some(n) = sample
        && n > 0
    {
        a.push("--sample".to_string());
        a.push(n.to_string());
    }
    a
}

impl Operator for SplinkResolve {
    fn name(&self) -> &'static str {
        "splink_resolve"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = SplinkResolveConfig::parse(with)?;
        Ok(OpAssets {
            reads: vec![cfg.edgar.to_lowercase(), cfg.gleif.to_lowercase()],
            produces: vec![cfg.out.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = SplinkResolveConfig::parse(with)?;
        let edgar = ctx.dir.join(&cfg.edgar);
        let gleif = ctx.dir.join(&cfg.gleif);
        let out = ctx.dir.join(&cfg.out);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let script = materialize_frozen_script("splink_resolve", "1.0.0", RESOLVE_PY)?;
        let extra = splink_resolve_args(
            &edgar.display().to_string(),
            &gleif.display().to_string(),
            &out.display().to_string(),
            cfg.sample,
        );
        let args = uv_run_args(&script.to_string_lossy(), &extra);
        // Inherit: stream resolve.py's coverage/hero-ticker tables live and honour the
        // step timeout (a 4 h job that overruns is killed, not left hanging).
        run_process("uv", &args, ctx, OutputMode::Inherit, "splink_resolve")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// gleif_ra_fetch (ingress) — page the public GLEIF v1 API for every entity
// registered under one registration authority (RA000665 = the SEC) and emit the
// `lei,registered_as,category` CSV the `load` step consumes.
//
// Retires `edgar_gleif/scripts/fetch_gleif_ra.sh`. That script hand-rolled cursor
// paging in bash (`page[cursor]=*`, follow `.links.next`, terminate on null/empty,
// with a loop guard) because GLEIF hard-caps OFFSET paging at 10,000 records and
// RA000665 has ~27k. The embedded `fetch_gleif_ra.py` does the same with dlt's
// `RESTClient` + `JSONLinkPaginator(next_url_path="links.next")` — STATELESS
// full-refresh (no incremental cursor persisted) — then writes a DETERMINISTIC
// SORTED CSV so the row-set parity gate against the retired script is a clean
// `EXCEPT`. Same uv-run substrate + frozen-script contract as datapackage_describe
// and splink_resolve: `op@1` addresses these exact script bytes.
//
// Ingress, so `reads: []` (the network source is not a graph node) and
// `produces: [out]` — the CSV sits in the AssetGraph downstream of nothing and
// upstream of `load`, exactly where the old `command:` step sat but now typed.
// ─────────────────────────────────────────────────────────────────────────────

/// The GLEIF-fetch script, pinned into the binary. `@1` == these exact bytes.
const GLEIF_RA_FETCH_PY: &str = include_str!("../operators/gleif_ra_fetch/fetch_gleif_ra.py");

struct GleifRaFetch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GleifRaFetchConfig {
    /// GLEIF registration-authority id to page (e.g. `RA000665`, the SEC).
    ra: String,
    /// Destination CSV path, relative to the manifest directory (the `produces`
    /// asset). The script writes it atomically (`.part` + rename).
    out: String,
    /// GLEIF `page[size]`. Defaults to the script's 200 when omitted.
    #[serde(default)]
    page_size: Option<u64>,
    /// User-Agent request header. Defaults to the script's Meridian UA when omitted.
    #[serde(default)]
    user_agent: Option<String>,
}

impl GleifRaFetchConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("gleif_ra_fetch: invalid `with:` config: {}", e))
        })
    }
}

impl Operator for GleifRaFetch {
    fn name(&self) -> &'static str {
        "gleif_ra_fetch"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = GleifRaFetchConfig::parse(with)?;
        // Ingress: the network source is not a graph node; only the local CSV is.
        Ok(OpAssets {
            reads: vec![],
            produces: vec![cfg.out.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        let cfg = GleifRaFetchConfig::parse(with)?;
        let out = ctx.dir.join(&cfg.out);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let script = materialize_frozen_script("gleif_ra_fetch", "1.0.0", GLEIF_RA_FETCH_PY)?;
        let mut extra = vec![
            "--ra".to_string(),
            cfg.ra.clone(),
            "--out".to_string(),
            out.display().to_string(),
        ];
        // `--max-pages` is intentionally NOT surfaced in config — it is a smoke-test
        // lever only. Production always does a full, guarded, unbounded pull.
        if let Some(ps) = cfg.page_size {
            extra.push("--page-size".to_string());
            extra.push(ps.to_string());
        }
        if let Some(ref ua) = cfg.user_agent {
            extra.push("--user-agent".to_string());
            extra.push(ua.clone());
        }
        let args = uv_run_args(&script.to_string_lossy(), &extra);
        run_process("uv", &args, ctx, OutputMode::Capture, "gleif_ra_fetch")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx<'a>(dir: &'a Path, env: &'a HashMap<String, String>) -> OpContext<'a> {
        OpContext {
            dir,
            db_path: dir,
            env,
            timeout: None,
            cache: None,
        }
    }

    #[test]
    fn uv_run_args_arg_order_is_stable() {
        assert_eq!(
            uv_run_args(
                "scripts/x.py",
                &["--out".to_string(), "o.parquet".to_string()]
            ),
            vec!["run", "--script", "scripts/x.py", "--out", "o.parquet"]
        );
    }

    #[test]
    fn run_process_success_and_capture() {
        let dir = std::env::temp_dir();
        let env = HashMap::new();
        let ctx = test_ctx(&dir, &env);
        // Inherit: `true` exits 0 → Ok.
        assert!(run_process("true", &[], &ctx, OutputMode::Inherit, "t").is_ok());
        // Capture: stdout is returned.
        let out = run_process("echo", &["hi".to_string()], &ctx, OutputMode::Capture, "t").unwrap();
        assert!(out.stdout.unwrap().contains("hi"));
    }

    #[test]
    fn run_process_nonzero_exit_is_retryable_stepfailed() {
        let dir = std::env::temp_dir();
        let env = HashMap::new();
        let ctx = test_ctx(&dir, &env);
        // `false` exits 1 → retryable StepFailed (the engine step-retry re-attempts).
        match run_process("false", &[], &ctx, OutputMode::Inherit, "t") {
            Err(Error::StepFailed { code, .. }) => assert_eq!(code, 1),
            other => panic!("expected StepFailed, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn run_process_missing_binary_is_nonretryable_stepexecution() {
        let dir = std::env::temp_dir();
        let env = HashMap::new();
        let ctx = test_ctx(&dir, &env);
        // A missing binary is deterministic → NON-retryable StepExecution, so a 4 h
        // job is never re-attempted 3× for a typo. This is the load-bearing choice.
        match run_process(
            "arc-no-such-binary-xyz",
            &[],
            &ctx,
            OutputMode::Inherit,
            "t",
        ) {
            Err(Error::StepExecution { .. }) => {}
            other => panic!("expected StepExecution, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn resolves_by_name_and_semver() {
        assert!(resolve("parquet_export").is_ok());
        assert!(resolve("parquet_export@1").is_ok());
        assert!(resolve("parquet_export@^1.0").is_ok());
        assert!(resolve("parquet_export@2").is_err()); // version not satisfied
        assert!(resolve("no_such_op").is_err()); // unknown
    }

    #[test]
    fn parquet_export_declares_lineage() {
        let with: Value =
            serde_yaml::from_str("input: crosswalk_edges\ndest: build/out.parquet").unwrap();
        let assets = assets_for("parquet_export", Some(&with)).unwrap();
        assert_eq!(assets.reads, vec!["crosswalk_edges".to_string()]);
        assert_eq!(assets.produces, vec!["build/out.parquet".to_string()]);
    }

    #[test]
    fn parquet_export_rejects_bad_config() {
        // missing required `dest`
        let with: Value = serde_yaml::from_str("input: t").unwrap();
        assert!(assets_for("parquet_export", Some(&with)).is_err());
        // unknown field
        let with: Value = serde_yaml::from_str("input: t\ndest: o.parquet\nbogus: 1").unwrap();
        assert!(assets_for("parquet_export", Some(&with)).is_err());
    }

    // ── parquet_export key-value metadata ────────────────────────────────────
    //
    // The statement tests below build the SQL and never touch a file; the three
    // that follow them write real Parquet and compare bytes. Both halves are
    // needed: the statement tests say what was asked for, and only the byte tests
    // say what landed in the footer.

    /// Config helper — parse a `with:` block into the typed config.
    fn pq_cfg(yaml: &str) -> ParquetExportConfig {
        ParquetExportConfig::parse(&serde_yaml::from_str::<Value>(yaml).unwrap()).unwrap()
    }

    /// An export declaring no metadata emits the statement this operator
    /// built before key-value metadata existed, character for character. Pinned as
    /// a literal rather than rebuilt from the config, so a change to the option
    /// list cannot quietly agree with itself.
    #[test]
    fn parquet_export_sql_without_metadata_is_unchanged() {
        const UNSTAMPED: &str = "COPY (SELECT * FROM t ORDER BY id) TO '/tmp/o.parquet' \
             (FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE 122880);";

        // `metadata:` absent entirely.
        let absent = pq_cfg(
            "input: t\ndest: o.parquet\ncompression: zstd\nrow_group_size: 122880\norder_by: id",
        );
        assert_eq!(
            parquet_export_sql(&absent, Path::new("/tmp/o.parquet")),
            UNSTAMPED,
            "an export with no `metadata:` must emit the pre-change statement"
        );

        // `metadata: {}` declared but empty takes the same path — and must, since
        // DuckDB rejects `KV_METADATA {}` as a syntax error.
        let empty = pq_cfg(
            "input: t\ndest: o.parquet\ncompression: zstd\nrow_group_size: 122880\norder_by: id\nmetadata: {}",
        );
        assert_eq!(
            parquet_export_sql(&empty, Path::new("/tmp/o.parquet")),
            UNSTAMPED,
            "an empty `metadata:` map must not emit a KV_METADATA option"
        );
    }

    /// Declared metadata reaches the statement. This is the test that
    /// reddens on a version that accepts the config and drops it.
    #[test]
    fn parquet_export_sql_stamps_declared_metadata() {
        let cfg = pq_cfg("input: t\ndest: o.parquet\nmetadata:\n  descriptor: '{\"name\":\"x\"}'");
        let sql = parquet_export_sql(&cfg, Path::new("/tmp/o.parquet"));
        assert_eq!(
            sql,
            "COPY (SELECT * FROM t) TO '/tmp/o.parquet' \
             (FORMAT parquet, COMPRESSION zstd, KV_METADATA {'descriptor': '{\"name\":\"x\"}'});",
            "declared metadata must reach the COPY option list"
        );
    }

    /// A value carrying an apostrophe must be escaped by doubling, or the statement
    /// is a syntax error at best and an injection at worst.
    #[test]
    fn parquet_export_sql_escapes_quotes_in_keys_and_values() {
        assert_eq!(sql_string_literal("it's"), "'it''s'");
        // Backslash and double-quote are NOT escapes in a DuckDB `'…'` literal, so
        // they must pass through untouched — escaping them would corrupt JSON.
        assert_eq!(sql_string_literal(r#"{"a":"b\c"}"#), r#"'{"a":"b\c"}'"#);

        let cfg = pq_cfg("input: t\ndest: o.parquet\nmetadata:\n  \"it's\": \"o'clock\"");
        let sql = parquet_export_sql(&cfg, Path::new("/tmp/o.parquet"));
        assert!(
            sql.contains("KV_METADATA {'it''s': 'o''clock'}"),
            "quotes must be doubled in both key and value; got: {sql}"
        );
    }

    /// The entries are emitted in sorted key order however the `with:` block
    /// lists them. DuckDB writes the map into the footer in the order given, so an
    /// unordered map would move the file's bytes between runs and destroy the
    /// reproducibility `order_by` exists to provide.
    #[test]
    fn parquet_export_metadata_emits_in_sorted_key_order() {
        let declared_forwards =
            pq_cfg("input: t\ndest: o.parquet\nmetadata:\n  aaa: '1'\n  zzz: '2'");
        let declared_backwards =
            pq_cfg("input: t\ndest: o.parquet\nmetadata:\n  zzz: '2'\n  aaa: '1'");
        let forwards = parquet_export_sql(&declared_forwards, Path::new("/tmp/o.parquet"));
        let backwards = parquet_export_sql(&declared_backwards, Path::new("/tmp/o.parquet"));
        assert_eq!(
            forwards, backwards,
            "declaration order must not reach the statement"
        );
        assert!(
            forwards.contains("KV_METADATA {'aaa': '1', 'zzz': '2'}"),
            "entries must be sorted by key; got: {forwards}"
        );
    }

    /// The generated statement stays legible to SQL introspection: the vendored
    /// sqlparser fork parses the open-ended `KV_METADATA` option and its struct
    /// literal under the DuckDB dialect. Upstream 0.55.0 rejects the option list
    /// outright, so this pins the fork's reach over the new syntax.
    #[test]
    fn parquet_export_sql_parses_under_the_duckdb_dialect() {
        let cfg = pq_cfg(
            "input: t\ndest: o.parquet\nrow_group_size: 122880\norder_by: id\nmetadata:\n  descriptor: '{\"name\":\"x\"}'",
        );
        let sql = parquet_export_sql(&cfg, Path::new("/tmp/o.parquet"));
        let parsed =
            sqlparser::parser::Parser::parse_sql(&sqlparser::dialect::DuckDbDialect {}, &sql);
        assert!(
            parsed.is_ok(),
            "the stamped COPY must parse under the DuckDB dialect, else introspection \
             degrades the step to an opaque node; got: {parsed:?}\nsql: {sql}"
        );
    }

    /// A pipeline DB with a two-column table, for the byte-level tests below.
    ///
    /// The rows are **stored out of key order** — `(i * 7919) % 1000` is a
    /// permutation of `0..1000`, chosen over `hash(i)` so the scramble is fixed by
    /// arithmetic rather than by a DuckDB internal. That is load-bearing, not
    /// decoration: a fixture written in key order makes `order_by: id` a no-op, and
    /// every claim below about the interaction between stamping and `order_by`
    /// would hold vacuously. `parquet_export_stamping_moves_the_hash_but_only_the_footer`
    /// asserts the scramble is real before it relies on it.
    fn pq_fixture(dir: &Path) -> PathBuf {
        let db = dir.join("pipeline.duckdb");
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE t AS SELECT i AS id, 'name' || i AS name \
             FROM range(1000) AS r(i) ORDER BY (i * 7919) % 1000;",
        )
        .unwrap();
        drop(conn);
        db
    }

    /// Run `parquet_export` with the given `with:` block against `db`, returning the
    /// written file's bytes.
    fn pq_run(dir: &Path, db: &Path, with_yaml: &str) -> Vec<u8> {
        let with: Value = serde_yaml::from_str(with_yaml).unwrap();
        let env = HashMap::new();
        let ctx = OpContext {
            dir,
            db_path: db,
            env: &env,
            timeout: None,
            cache: None,
        };
        ParquetExport.run(&with, &ctx).unwrap();
        let dest: String = serde_yaml::from_value::<ParquetExportConfig>(with)
            .unwrap()
            .dest;
        std::fs::read(dir.join(dest)).unwrap()
    }

    /// The metadata is in the written file's footer, and comes back as the
    /// UTF-8 it went in as. `decode()` is the read-back, not a `VARCHAR` cast: the
    /// footer map is untyped bytes, so DuckDB hands it over as `BLOB` and casting
    /// that to `VARCHAR` yields an escaped rendering rather than the text.
    #[test]
    fn parquet_export_writes_metadata_readable_off_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let db = pq_fixture(dir);

        // A value with a quote, a brace and a non-ASCII character — the shapes a
        // real descriptor carries.
        let descriptor = r#"{"name":"café","note":"it's fine"}"#;
        pq_run(
            dir,
            &db,
            &format!(
                "input: t\ndest: out/stamped.parquet\norder_by: id\nmetadata:\n  descriptor: '{}'\n  licence: CC-BY-4.0",
                descriptor.replace('\'', "''")
            ),
        );

        let conn = duckdb::Connection::open_in_memory().unwrap();
        let path = dir.join("out/stamped.parquet");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT decode(key), decode(value) FROM parquet_kv_metadata('{}') ORDER BY 1",
                path.display()
            ))
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(
            rows,
            vec![
                ("descriptor".to_string(), descriptor.to_string()),
                ("licence".to_string(), "CC-BY-4.0".to_string()),
            ],
            "both entries must be readable off the footer as the UTF-8 that was written"
        );
    }

    /// At the byte level — an export declaring no metadata produces the same
    /// bytes as the statement this operator issued before the change. The reference
    /// is not a checked-in golden but a COPY run through the same library in the
    /// same process: the Parquet footer records the writing DuckDB's version
    /// string, so a golden would pin the library, not this operator.
    #[test]
    fn parquet_export_without_metadata_is_byte_identical_to_the_prior_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let db = pq_fixture(dir);

        // The reference: the exact statement `run` built before `metadata` existed.
        let reference_path = dir.join("reference.parquet");
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT * FROM t ORDER BY id) TO '{}' (FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE 122880);",
            reference_path.display()
        ))
        .unwrap();
        drop(conn);
        let reference = std::fs::read(&reference_path).unwrap();

        let through_operator = pq_run(
            dir,
            &db,
            "input: t\ndest: unstamped.parquet\ncompression: zstd\nrow_group_size: 122880\norder_by: id",
        );
        assert_eq!(
            through_operator, reference,
            "an unstamped export must be byte-identical to the pre-change COPY"
        );

        // And an explicitly empty map is the same file again.
        let empty_map = pq_run(
            dir,
            &db,
            "input: t\ndest: empty.parquet\ncompression: zstd\nrow_group_size: 122880\norder_by: id\nmetadata: {}",
        );
        assert_eq!(
            empty_map, reference,
            "an empty `metadata:` map must not move the output bytes"
        );
    }

    /// What stamping costs, measured rather than assumed.
    ///
    /// Three claims, each checked: a stamped export is reproducible (same config,
    /// same bytes), stamping changes the file and so its hash, and the change is
    /// confined to the footer — every data page is byte-identical, so `order_by`
    /// still buys exactly what it bought before. Any publish step that pins the
    /// hash of a stamped file has to re-pin it once, not on every run.
    #[test]
    fn parquet_export_stamping_moves_the_hash_but_only_the_footer() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let db = pq_fixture(dir);

        let unstamped = "input: t\ndest: u.parquet\norder_by: id\nrow_group_size: 122880";
        let stamped = "input: t\ndest: s.parquet\norder_by: id\nrow_group_size: 122880\nmetadata:\n  descriptor: DESCRIPTOR";
        let stamped_again = "input: t\ndest: s2.parquet\norder_by: id\nrow_group_size: 122880\nmetadata:\n  descriptor: DESCRIPTOR";

        let u = pq_run(dir, &db, unstamped);
        let s = pq_run(dir, &db, stamped);
        let s2 = pq_run(dir, &db, stamped_again);

        // First: `order_by` is load-bearing on this fixture. The rows are stored
        // out of key order, so the same export without the clause is a different
        // file. Without this the footer-confinement assertion below would pass on a
        // fixture where ORDER BY does nothing, and would be measuring nothing.
        let unordered = pq_run(
            dir,
            &db,
            "input: t\ndest: unordered.parquet\nrow_group_size: 122880",
        );
        assert_ne!(
            unordered, u,
            "the fixture must be stored out of key order, or `order_by` proves nothing here"
        );

        // Reproducible: the same stamp twice is the same file.
        assert_eq!(
            s, s2,
            "a stamped export must be reproducible — same config, same bytes"
        );
        // But not the same file as the unstamped one: the hash moves.
        assert_ne!(
            u, s,
            "stamping must change the file, or nothing was written to the footer"
        );

        // And the change is confined to the footer. Every byte up to the first
        // difference is shared, and the difference lands past the last data page —
        // located here as the start of the Parquet footer, which is the final
        // 8 bytes (4-byte footer length + the `PAR1` magic) plus that length.
        let footer_len = u32::from_le_bytes(u[u.len() - 8..u.len() - 4].try_into().unwrap());
        let footer_start = u.len() - 8 - footer_len as usize;
        let common = u.iter().zip(s.iter()).take_while(|(a, b)| a == b).count();
        assert!(
            common >= footer_start,
            "stamping must not touch the data pages: files diverge at byte {common}, \
             but the unstamped footer starts at {footer_start}"
        );
        assert!(
            s.len() > u.len(),
            "the stamped file must be the larger one: {} vs {}",
            s.len(),
            u.len()
        );
    }

    #[test]
    fn datapackage_describe_declares_lineage() {
        let with: Value = serde_yaml::from_str(
            "parquet: build/edgar_gleif.parquet\noverrides: descriptor.overrides.json\nout: datapackage.json",
        )
        .unwrap();
        let a = assets_for("datapackage_describe", Some(&with)).unwrap();
        assert_eq!(a.reads, vec!["build/edgar_gleif.parquet".to_string()]);
        assert_eq!(a.produces, vec!["datapackage.json".to_string()]);
        // unknown field rejected (deny_unknown_fields)
        let bad: Value = serde_yaml::from_str("parquet: p\noverrides: o\nout: d\nx: 1").unwrap();
        assert!(assets_for("datapackage_describe", Some(&bad)).is_err());
    }

    #[test]
    fn splink_resolve_declares_lineage() {
        // reads = [edgar, gleif] (order-preserving), produces = [out].
        let with: Value = serde_yaml::from_str(
            "edgar: build/sec_entities.parquet\ngleif: build/gleif.parquet\nout: build/resolved.parquet",
        )
        .unwrap();
        let a = assets_for("splink_resolve", Some(&with)).unwrap();
        assert_eq!(
            a.reads,
            vec![
                "build/sec_entities.parquet".to_string(),
                "build/gleif.parquet".to_string()
            ]
        );
        assert_eq!(a.produces, vec!["build/resolved.parquet".to_string()]);
        // `sample` is optional — a published run omits it and still validates.
        assert!(assets_for("splink_resolve", Some(&with)).is_ok());
        // unknown field rejected (deny_unknown_fields)
        let bad: Value = serde_yaml::from_str("edgar: e\ngleif: g\nout: o\nbogus: 1").unwrap();
        assert!(assets_for("splink_resolve", Some(&bad)).is_err());
        // missing a required field rejected
        let miss: Value = serde_yaml::from_str("edgar: e\ngleif: g").unwrap();
        assert!(assets_for("splink_resolve", Some(&miss)).is_err());
    }

    #[test]
    fn splink_resolve_resolves_by_semver() {
        assert!(resolve("splink_resolve").is_ok());
        assert!(resolve("splink_resolve@1").is_ok());
        assert!(resolve("splink_resolve@^1.0").is_ok());
        assert!(resolve("splink_resolve@2").is_err()); // version not satisfied
    }

    #[test]
    fn splink_resolve_sample_omitted_for_published_run() {
        // Published run: no sample → the full corpus, no `--sample` flag at all
        // (it MUST be omittable — a `--sample 0` would cap GLEIF to zero rows).
        let full = splink_resolve_args("e.parquet", "g.parquet", "o.parquet", None);
        assert_eq!(
            full,
            vec![
                "--edgar",
                "e.parquet",
                "--gleif",
                "g.parquet",
                "--out",
                "o.parquet"
            ]
        );
        assert!(!full.iter().any(|a| a == "--sample"));
        // `sample: 0` is treated as "full corpus" too → still omitted.
        let zero = splink_resolve_args("e.parquet", "g.parquet", "o.parquet", Some(0));
        assert!(!zero.iter().any(|a| a == "--sample"));
        // A positive smoke-test cap IS emitted, as the trailing `--sample N` pair.
        let sampled = splink_resolve_args("e.parquet", "g.parquet", "o.parquet", Some(20_000));
        assert_eq!(
            &sampled[sampled.len() - 2..],
            &["--sample".to_string(), "20000".to_string()]
        );
        // Full invocation threads through uv_run_args in the documented order.
        let script = "/tmp/arcform-op-splink_resolve-1.0.0/splink_resolve.py";
        let argv = uv_run_args(script, &sampled);
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "--script");
        assert_eq!(argv[2], script);
        assert_eq!(argv[3], "--edgar");
    }

    #[test]
    fn gleif_ra_fetch_is_in_catalog_and_versioned() {
        assert!(resolve("gleif_ra_fetch").is_ok());
        assert!(resolve("gleif_ra_fetch@1").is_ok());
        assert!(resolve("gleif_ra_fetch@^1.0").is_ok());
        assert!(resolve("gleif_ra_fetch@2").is_err()); // version not satisfied
    }

    #[test]
    fn gleif_ra_fetch_declares_only_its_output() {
        // Ingress: produces the local CSV, reads no graph node (the GLEIF API is not
        // in the AssetGraph). `out` is lowercased into the produces asset.
        let with: Value =
            serde_yaml::from_str("ra: RA000665\nout: build/gleif_ra_sec.csv").unwrap();
        let a = assets_for("gleif_ra_fetch", Some(&with)).unwrap();
        assert_eq!(a.produces, vec!["build/gleif_ra_sec.csv".to_string()]);
        assert!(a.reads.is_empty());
    }

    #[test]
    fn gleif_ra_fetch_optional_fields_default() {
        // page_size / user_agent are optional — a minimal ra+out config validates.
        let min: Value = serde_yaml::from_str("ra: RA000665\nout: g.csv").unwrap();
        assert!(assets_for("gleif_ra_fetch", Some(&min)).is_ok());
        // …and both may be supplied.
        let full: Value =
            serde_yaml::from_str("ra: RA000665\nout: g.csv\npage_size: 50\nuser_agent: 'X (y@z)'")
                .unwrap();
        assert!(assets_for("gleif_ra_fetch", Some(&full)).is_ok());
    }

    #[test]
    fn gleif_ra_fetch_rejects_bad_config() {
        // missing required `out`
        let no_out: Value = serde_yaml::from_str("ra: RA000665").unwrap();
        assert!(assets_for("gleif_ra_fetch", Some(&no_out)).is_err());
        // missing required `ra`
        let no_ra: Value = serde_yaml::from_str("out: g.csv").unwrap();
        assert!(assets_for("gleif_ra_fetch", Some(&no_ra)).is_err());
        // unknown field rejected (deny_unknown_fields) — e.g. a stray `max_pages`,
        // which is a smoke-test-only CLI flag and deliberately not a config field.
        let bogus: Value = serde_yaml::from_str("ra: RA000665\nout: g.csv\nmax_pages: 2").unwrap();
        assert!(assets_for("gleif_ra_fetch", Some(&bogus)).is_err());
    }

    // ── finetype_validate ───────────────────────────────────────────────────

    #[test]
    fn finetype_validate_is_in_catalog_and_versioned() {
        assert!(resolve("finetype_validate").is_ok());
        assert!(resolve("finetype_validate@1").is_ok());
        assert!(resolve("finetype_validate@^1.0").is_ok());
        assert!(resolve("finetype_validate@2").is_err()); // version not satisfied
    }

    #[test]
    fn finetype_validate_declares_reads_and_no_produces() {
        // Gate: reads [parquet, schema] (parquet first → downstream of its producer),
        // produces nothing (check-only leaves the terminal Parquet byte-identical).
        let with: Value = serde_yaml::from_str(
            "parquet: build/edgar_gleif.parquet\nschema: schema.finetype.json",
        )
        .unwrap();
        let a = assets_for("finetype_validate", Some(&with)).unwrap();
        assert_eq!(
            a.reads,
            vec![
                "build/edgar_gleif.parquet".to_string(),
                "schema.finetype.json".to_string(),
            ]
        );
        assert!(a.produces.is_empty());
    }

    #[test]
    fn finetype_validate_rejects_bad_config() {
        // missing required `schema`
        let no_schema: Value = serde_yaml::from_str("parquet: p.parquet").unwrap();
        assert!(assets_for("finetype_validate", Some(&no_schema)).is_err());
        // missing required `parquet`
        let no_parquet: Value = serde_yaml::from_str("schema: s.json").unwrap();
        assert!(assets_for("finetype_validate", Some(&no_parquet)).is_err());
        // unknown field rejected (deny_unknown_fields)
        let bogus: Value = serde_yaml::from_str("parquet: p\nschema: s\nbogus: 1").unwrap();
        assert!(assets_for("finetype_validate", Some(&bogus)).is_err());
    }

    /// A minimal Parquet + a JSON Schema it satisfies, then a schema it violates — the
    /// two halves of the in-engine gate. Needs the built finetype DuckDB extension: the
    /// verify harness exports `FINETYPE_DUCKDB_EXT` to point at it; if it is unset or
    /// absent (e.g. CI without the extension) the test SKIPS rather than false-fails.
    #[test]
    fn finetype_validate_passes_clean_and_fails_on_drift() {
        let ext = match std::env::var("FINETYPE_DUCKDB_EXT") {
            Ok(p) if std::path::Path::new(&p).exists() => p,
            _ => {
                eprintln!("skipping finetype_validate: FINETYPE_DUCKDB_EXT unset or missing");
                return;
            }
        };

        let dir = tempfile::tempdir().unwrap();
        // Build a tiny Parquet with one column `code` holding two 2-letter values — the
        // gate reads Parquet via `read_parquet`, so materialise it with DuckDB itself.
        let parquet = dir.path().join("data.parquet");
        {
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "COPY (SELECT 'US' AS code UNION ALL SELECT 'GB') TO '{}' (FORMAT parquet);",
                sql_lit(&parquet.display().to_string())
            ))
            .unwrap();
        }
        // PASS schema: a closed enum containing BOTH values — 0 rejects.
        std::fs::write(
            dir.path().join("pass.json"),
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"code":{"type":"string","minLength":2,"maxLength":2,"enum":["US","GB"]}}}"#,
        )
        .unwrap();
        // DRIFT schema: a closed enum that does NOT contain "GB" — the second row
        // violates it, so ft_validate reports a reject and the gate must fail closed.
        std::fs::write(
            dir.path().join("drift.json"),
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"code":{"type":"string","enum":["US"]}}}"#,
        )
        .unwrap();

        // Wire the extension through the per-step `extension:` config field — the same
        // resolution path the env var feeds, without mutating process-global env.
        let ext_yaml = format!("\nextension: {}", ext);
        let env = HashMap::new();
        let ctx = test_ctx(dir.path(), &env);
        let op = resolve("finetype_validate").unwrap();

        let pass_with: Value = serde_yaml::from_str(&format!(
            "parquet: data.parquet\nschema: pass.json{ext_yaml}"
        ))
        .unwrap();
        match op.run(&pass_with, &ctx) {
            Ok(_) => {} // clean → gate passes
            other => panic!("clean validate should pass, got {:?}", other.map(|_| ())),
        }

        let drift_with: Value = serde_yaml::from_str(&format!(
            "parquet: data.parquet\nschema: drift.json{ext_yaml}"
        ))
        .unwrap();
        match op.run(&drift_with, &ctx) {
            // Fail-closed: a row violates the contract → rejects > 0 → StepFailed.
            Err(Error::StepFailed { .. }) => {}
            other => panic!("drift must fail the gate, got {:?}", other.map(|_| ())),
        }
    }

    #[cfg(feature = "http-fetch")]
    #[test]
    fn http_fetch_declares_only_its_output() {
        // Ingress produces the local artifact and reads no graph node (the
        // network source isn't in the AssetGraph).
        let hf: Value =
            serde_yaml::from_str("url: https://x/edgar.parquet\nout: build/edgar.parquet").unwrap();
        let a = assets_for("http_fetch", Some(&hf)).unwrap();
        assert_eq!(a.produces, vec!["build/edgar.parquet".to_string()]);
        assert!(a.reads.is_empty());
    }

    // ── html_link_discover ──────────────────────────────────────────────────

    #[test]
    fn archive_extract_is_in_catalog() {
        assert!(resolve("archive_extract@^1.0").is_ok());
    }

    #[cfg(feature = "http-fetch")]
    #[test]
    fn html_link_discover_is_in_catalog() {
        assert!(resolve("html_link_discover@1").is_ok());
        assert!(resolve("html_link_discover@2").is_err()); // version not satisfied
    }

    #[cfg(feature = "http-fetch")]
    #[test]
    fn html_link_discover_declares_only_its_output() {
        let with: Value = serde_yaml::from_str(
            "url: https://www.sec.gov/x\npattern: 'ncen.*\\.zip'\nout: build/ncen_urls.txt",
        )
        .unwrap();
        let a = assets_for("html_link_discover", Some(&with)).unwrap();
        assert_eq!(a.produces, vec!["build/ncen_urls.txt".to_string()]);
        assert!(a.reads.is_empty());
    }

    #[cfg(feature = "http-fetch")]
    #[test]
    fn html_link_discover_rejects_bad_config() {
        // missing required `out`
        let bad: Value = serde_yaml::from_str("url: https://x\npattern: 'a'").unwrap();
        assert!(assets_for("html_link_discover", Some(&bad)).is_err());
        // unknown field (deny_unknown_fields)
        let bad: Value =
            serde_yaml::from_str("url: https://x\npattern: 'a'\nout: o\nbogus: 1").unwrap();
        assert!(assets_for("html_link_discover", Some(&bad)).is_err());
        // invalid `pattern` regex is a load-time validation error
        let bad: Value = serde_yaml::from_str("url: https://x\npattern: '('\nout: o").unwrap();
        assert!(assets_for("html_link_discover", Some(&bad)).is_err());
    }

    #[test]
    fn discover_links_extracts_absolutises_dedups_in_order() {
        let page = "https://www.sec.gov/data-research/sec-markets-data/form-n-cen-data-sets";
        let html = r#"
            <a href="/files/dera/data/ncen_2024q4.zip">Q4</a>
            <a href='/files/dera/data/ncen_2024q3.zip'>Q3</a>
            <a href="/about">nope</a>
            <a href="https://cdn.example.com/ncen_2024q2.zip">Q2 abs</a>
            <a href="/files/dera/data/ncen_2024q4.zip">Q4 dup</a>
            <a href="/files/dera/data/other_2024q1.zip">other</a>
        "#;
        let re = regex::Regex::new(r"ncen.*\.zip").unwrap();
        let all = discover_links(html, page, &re, None);
        assert_eq!(
            all,
            vec![
                "https://www.sec.gov/files/dera/data/ncen_2024q4.zip".to_string(),
                "https://www.sec.gov/files/dera/data/ncen_2024q3.zip".to_string(),
                "https://cdn.example.com/ncen_2024q2.zip".to_string(),
            ]
        );
        // `/about` and `other_2024q1.zip` don't match `pattern`; the duplicate Q4 is
        // de-duped, first-seen order preserved.

        // cap: `limit` truncates to the head of the ordered, de-duped list.
        let capped = discover_links(html, page, &re, Some(2));
        assert_eq!(
            capped,
            vec![
                "https://www.sec.gov/files/dera/data/ncen_2024q4.zip".to_string(),
                "https://www.sec.gov/files/dera/data/ncen_2024q3.zip".to_string(),
            ]
        );
    }

    #[test]
    fn absolutise_handles_relative_forms() {
        let page = "https://www.sec.gov/a/b/page.html";
        assert_eq!(
            absolutise(page, "/files/x.zip").as_deref(),
            Some("https://www.sec.gov/files/x.zip")
        );
        // path-relative resolves against the page's directory base.
        assert_eq!(
            absolutise(page, "c.zip").as_deref(),
            Some("https://www.sec.gov/a/b/c.zip")
        );
        // absolute is kept verbatim; protocol-relative borrows the page scheme.
        assert_eq!(
            absolutise(page, "https://cdn/x").as_deref(),
            Some("https://cdn/x")
        );
        assert_eq!(
            absolutise(page, "//cdn.example.com/x").as_deref(),
            Some("https://cdn.example.com/x")
        );
        assert_eq!(absolutise(page, "#frag"), None);
        assert_eq!(absolutise(page, ""), None);
    }

    // ── archive_extract ─────────────────────────────────────────────────────

    #[test]
    fn archive_extract_declares_members_and_reads() {
        let with: Value = serde_yaml::from_str(
            "archive: build/ncen/2024q1.zip\nmembers: [REGISTRANT.tsv, FUND_REPORTED_INFO.tsv]\ndest: build/ncen/2024q1",
        )
        .unwrap();
        let a = assets_for("archive_extract", Some(&with)).unwrap();
        assert_eq!(a.reads, vec!["build/ncen/2024q1.zip".to_string()]);
        // Node names lowercased (graph convention); on-disk case preserved separately.
        assert_eq!(
            a.produces,
            vec![
                "build/ncen/2024q1/registrant.tsv".to_string(),
                "build/ncen/2024q1/fund_reported_info.tsv".to_string(),
            ]
        );
    }

    #[test]
    fn archive_extract_pattern_only_declares_dest_node() {
        let with: Value =
            serde_yaml::from_str("archive: a.zip\npattern: '\\.tsv$'\ndest: build/out").unwrap();
        let a = assets_for("archive_extract", Some(&with)).unwrap();
        assert_eq!(a.reads, vec!["a.zip".to_string()]);
        assert_eq!(a.produces, vec!["build/out".to_string()]);
    }

    #[test]
    fn archive_extract_rejects_bad_config() {
        // neither `members` nor `pattern` — nothing to select
        let bad: Value = serde_yaml::from_str("archive: a.zip\ndest: out").unwrap();
        assert!(assets_for("archive_extract", Some(&bad)).is_err());
        // unknown field (deny_unknown_fields)
        let bad: Value =
            serde_yaml::from_str("archive: a.zip\nmembers: [x]\ndest: out\nbogus: 1").unwrap();
        assert!(assets_for("archive_extract", Some(&bad)).is_err());
        // invalid `pattern` regex
        let bad: Value = serde_yaml::from_str("archive: a.zip\npattern: '('\ndest: out").unwrap();
        assert!(assets_for("archive_extract", Some(&bad)).is_err());
    }

    #[test]
    fn safe_relative_guards_zip_slip_and_preserves_case() {
        // Rejected: traversal, absolute, Windows drive / UNC.
        assert!(safe_relative("../evil.txt").is_none());
        assert!(safe_relative("a/../../b").is_none());
        assert!(safe_relative("/etc/passwd").is_none());
        assert!(safe_relative("C:\\Windows\\x").is_none());
        assert!(safe_relative("\\\\server\\share").is_none());
        // Accepted: real case preserved, `.` components dropped.
        assert_eq!(
            safe_relative("REGISTRANT.tsv"),
            Some(PathBuf::from("REGISTRANT.tsv"))
        );
        assert_eq!(
            safe_relative("Sub/Dir/File.TSV"),
            Some(PathBuf::from("Sub/Dir/File.TSV"))
        );
        assert_eq!(safe_relative("./a/./b"), Some(PathBuf::from("a/b")));
    }

    /// Build a DEFLATE-compressed in-memory zip of `(name, bytes)` members. Exercises
    /// the `deflate-flate2` backend end-to-end (the compression the N-CEN zips use).
    fn build_zip(entries: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
        use std::io::{Cursor, Write};
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default().compression_method(method);
            for (name, bytes) in entries {
                zw.start_file(*name, opts).unwrap();
                zw.write_all(bytes).unwrap();
            }
            zw.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_members_roundtrip_preserves_case_and_selects() {
        let dir = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(
            &[
                ("REGISTRANT.tsv", b"cik\tlei\n1\tABC\n"),
                ("FUND_REPORTED_INFO.tsv", b"series_id\tlei\nS1\tXYZ\n"),
                ("SUBMISSION.tsv", b"unwanted\n"),
            ],
            zip::CompressionMethod::Deflated,
        );
        let members = vec![
            "REGISTRANT.tsv".to_string(),
            "FUND_REPORTED_INFO.tsv".to_string(),
        ];
        let written =
            extract_members(std::io::Cursor::new(&zip_bytes), dir.path(), &members, None).unwrap();
        assert_eq!(written.len(), 2);

        // Real (upper) case preserved on disk; content round-trips through DEFLATE.
        let reg = dir.path().join("REGISTRANT.tsv");
        assert!(
            reg.exists(),
            "REGISTRANT.tsv (real case) should exist on disk"
        );
        assert_eq!(std::fs::read_to_string(&reg).unwrap(), "cik\tlei\n1\tABC\n");
        assert!(dir.path().join("FUND_REPORTED_INFO.tsv").exists());
        // Non-selected member is not extracted; no leftover `.part` temp file.
        assert!(!dir.path().join("SUBMISSION.tsv").exists());
        assert!(!dir.path().join("REGISTRANT.tsv.part").exists());
    }

    #[test]
    fn extract_members_selects_by_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let zip_bytes = build_zip(
            &[
                ("REGISTRANT.tsv", b"a"),
                ("FUND_REPORTED_INFO.tsv", b"b"),
                ("readme.txt", b"c"),
            ],
            zip::CompressionMethod::Deflated,
        );
        let re = regex::Regex::new(r"\.tsv$").unwrap();
        let written =
            extract_members(std::io::Cursor::new(&zip_bytes), dir.path(), &[], Some(&re)).unwrap();
        assert_eq!(written.len(), 2);
        assert!(dir.path().join("REGISTRANT.tsv").exists());
        assert!(!dir.path().join("readme.txt").exists());
    }

    #[test]
    fn extract_members_blocks_zip_slip_write() {
        let dir = tempfile::tempdir().unwrap();
        // A hostile member named `../evil.txt`.
        let zip_bytes = build_zip(&[("../evil.txt", b"pwned")], zip::CompressionMethod::Stored);
        let re = regex::Regex::new("evil").unwrap();
        let res = extract_members(std::io::Cursor::new(&zip_bytes), dir.path(), &[], Some(&re));
        // Invariant: whether the writer stored `../evil.txt` verbatim (→ guard rejects,
        // `Err`) or normalised it (→ lands safely inside `dest`), nothing is ever
        // written to the traversal target outside `dest`.
        let escaped = dir.path().parent().unwrap().join("evil.txt");
        assert!(
            !escaped.exists(),
            "zip-slip must never write outside dest (res_ok={})",
            res.is_ok()
        );
    }

    #[cfg(feature = "opendal")]
    #[test]
    fn opendal_fetch_declares_only_its_output() {
        let of: Value =
            serde_yaml::from_str("from: s3://securelake/edgar.parquet\nto: build/edgar.parquet")
                .unwrap();
        let a = assets_for("opendal_fetch", Some(&of)).unwrap();
        assert_eq!(a.produces, vec!["build/edgar.parquet".to_string()]);
        assert!(a.reads.is_empty());
    }

    #[cfg(feature = "opendal")]
    #[test]
    fn opendal_fetch_accepts_headers() {
        // `headers` is a valid config field on opendal_fetch — it is carried to
        // the http(s) backend via a custom ReqwestTransport (see run()), not
        // rejected. The live transport wiring is proven by the SEC fetch in the
        // spike run; here we just assert the config is accepted + lineage holds.
        let with: Value = serde_yaml::from_str(
            "from: https://www.sec.gov/files/company_tickers.json\nto: build/tickers.json\nheaders:\n  User-Agent: 'Meridian (research@meridian.online)'",
        )
        .unwrap();
        let assets = assets_for("opendal_fetch", Some(&with)).unwrap();
        assert_eq!(assets.produces, vec!["build/tickers.json".to_string()]);
        assert!(assets.reads.is_empty());
    }

    #[cfg(feature = "http-fetch")]
    #[test]
    fn sha256_validator_accepts_only_a_bare_64_hex_digest() {
        let digest = "6b552ea48424648dc86d00df276f93fdfc55e9ad342ce3e4affc23a3a370792b";
        assert_eq!(
            sha256_validator(&format!("\"{}\"", digest)),
            Some(digest.to_string())
        );
        assert_eq!(
            sha256_validator(&format!("W/\"{}\"", digest.to_ascii_uppercase())),
            Some(digest.to_string())
        );
        // A git blob sha1 (40 hex), an opaque storage id, and an md5 are not it.
        assert_eq!(
            sha256_validator("\"9bb295ddab0e05d785b879661af7260fed5140fc\""),
            None
        );
        assert_eq!(sha256_validator("\"storage-object-id\""), None);
        assert_eq!(
            sha256_validator("\"5d41402abc4b2a76b9719d911017c592\""),
            None
        );
    }

    #[cfg(feature = "http-fetch")]
    #[test]
    fn join_location_resolves_absolute_and_relative_targets() {
        let base = "https://example.test/datasets/x/resolve/main/a.parquet";
        assert_eq!(
            join_location(base, "https://cdn.example.test/blob?sig=1").as_deref(),
            Some("https://cdn.example.test/blob?sig=1")
        );
        assert_eq!(
            join_location(base, "/blob").as_deref(),
            Some("https://example.test/blob")
        );
        assert_eq!(
            join_location(base, "b.parquet").as_deref(),
            Some("https://example.test/datasets/x/resolve/main/b.parquet")
        );
        assert_eq!(join_location("not a url", "/blob"), None);
    }

    // ── the redirect fixture ────────────────────────────────────────────────
    //
    // A loopback origin with a dataset host's shape: `/file` answers `302` with a
    // RELATIVE `Location`, and the redirect target `/blob` answers `200` under
    // `STORAGE_VALIDATOR` and `304` to a conditional request carrying it. What the
    // `302` itself carries is per-test, which is the axis the fetch has to get right.
    // It records every request head it receives and every payload byte it writes,
    // which is what the assertions below read.
    #[cfg(feature = "http-fetch")]
    mod origin {
        use std::io::{Read, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::{Arc, Mutex};

        pub const PAYLOAD: &[u8] = b"the artifact bytes, transferred at most once";
        pub const STORAGE_VALIDATOR: &str = "\"storage-object-id\"";

        /// The headers the `302` from `/file` carries, and whether `/file` itself
        /// honours a conditional request.
        #[derive(Default)]
        pub struct Spec {
            /// Served as `X-Linked-ETag` on the redirect when set.
            pub linked: Option<String>,
            /// Served as `Last-Modified` on the redirect when set.
            pub redirect_last_modified: Option<String>,
            /// When true, `/file` answers `304` to an `If-None-Match` matching
            /// `linked`; when false it always redirects.
            pub honour_conditional: bool,
        }

        impl Spec {
            /// The dataset-host shape: a `302` carrying `linked`, answering `304` to
            /// a conditional request that replays it.
            pub fn linked(validator: &str) -> Self {
                Self {
                    linked: Some(validator.to_string()),
                    honour_conditional: true,
                    ..Self::default()
                }
            }

            /// The same `302`, from an origin that ignores conditional requests.
            pub fn linked_unconditional(validator: &str) -> Self {
                Self {
                    linked: Some(validator.to_string()),
                    ..Self::default()
                }
            }
        }

        #[derive(Default)]
        pub struct Log {
            /// Request-target of each request, in arrival order.
            pub paths: Vec<String>,
            /// Full request head of each request, in arrival order.
            pub heads: Vec<String>,
            /// Payload bytes written to clients.
            pub bytes_served: usize,
        }

        pub struct Origin {
            port: u16,
            log: Arc<Mutex<Log>>,
        }

        impl Origin {
            pub fn start(spec: Spec) -> Self {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                let log = Arc::new(Mutex::new(Log::default()));
                let served = Arc::clone(&log);
                std::thread::spawn(move || {
                    for stream in listener.incoming().flatten() {
                        serve(stream, &served, &spec);
                    }
                });
                Self { port, log }
            }

            pub fn url(&self, path: &str) -> String {
                format!("http://127.0.0.1:{}{}", self.port, path)
            }

            pub fn hits(&self, path: &str) -> usize {
                self.log
                    .lock()
                    .unwrap()
                    .paths
                    .iter()
                    .filter(|p| *p == path)
                    .count()
            }

            pub fn bytes_served(&self) -> usize {
                self.log.lock().unwrap().bytes_served
            }

            pub fn heads(&self) -> Vec<String> {
                self.log.lock().unwrap().heads.clone()
            }
        }

        /// True when the request head replays `validator` in `If-None-Match`.
        fn replays(head: &str, validator: &str) -> bool {
            head.to_ascii_lowercase().contains(&format!(
                "if-none-match: {}",
                validator.to_ascii_lowercase()
            ))
        }

        fn serve(mut stream: TcpStream, log: &Arc<Mutex<Log>>, spec: &Spec) {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => return,
                }
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            let origin_304 = spec.honour_conditional
                && spec.linked.as_deref().is_some_and(|v| replays(&head, v));
            let target_304 = replays(&head, STORAGE_VALIDATOR);

            let mut l = log.lock().unwrap();
            l.paths.push(path.clone());
            l.heads.push(head);
            let serve_body = path == "/blob" && !target_304;
            let response = if path == "/blob" {
                if target_304 {
                    "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    l.bytes_served += PAYLOAD.len();
                    format!(
                        "HTTP/1.1 200 OK\r\nETag: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        STORAGE_VALIDATOR,
                        PAYLOAD.len()
                    )
                }
            } else if origin_304 {
                "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".to_string()
            } else {
                let mut r = "HTTP/1.1 302 Found\r\nLocation: /blob\r\n".to_string();
                if let Some(ref v) = spec.linked {
                    r.push_str(&format!("X-Linked-ETag: {}\r\n", v));
                }
                if let Some(ref lm) = spec.redirect_last_modified {
                    r.push_str(&format!("Last-Modified: {}\r\n", lm));
                }
                r.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
                r
            };
            drop(l);
            let _ = stream.write_all(response.as_bytes());
            if serve_body {
                let _ = stream.write_all(PAYLOAD);
            }
            let _ = stream.flush();
        }
    }

    #[cfg(feature = "http-fetch")]
    fn payload_digest() -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(origin::PAYLOAD))
    }

    #[cfg(feature = "http-fetch")]
    fn fetch(dir: &Path, url: &str) -> Result<StepOutput> {
        let env = HashMap::new();
        let with: Value =
            serde_yaml::from_str(&format!("url: {}\nout: build/artifact.bin", url)).unwrap();
        HttpFetch.run(&with, &test_ctx(dir, &env))
    }

    // AC1/AC4: the sidecar records the validator the REDIRECT offered, not the one
    // the redirect target answered under. Reverting to the final hop's `ETag` puts
    // `STORAGE_VALIDATOR` in the sidecar and reddens this.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn fetch_records_the_redirects_validator_not_the_final_hops() {
        let digest = payload_digest();
        let linked = format!("\"{}\"", digest);
        let server = origin::Origin::start(origin::Spec::linked(&linked));
        let dir = tempfile::tempdir().unwrap();

        fetch(dir.path(), &server.url("/file")).unwrap();

        let out = dir.path().join("build/artifact.bin");
        assert_eq!(std::fs::read(&out).unwrap(), origin::PAYLOAD);
        let meta = crate::ingress_meta::read(&out).expect("sidecar written");
        assert_eq!(meta.etag.as_deref(), Some(linked.as_str()));
        assert_ne!(meta.etag.as_deref(), Some(origin::STORAGE_VALIDATOR));
        assert_eq!(meta.content_sha256.as_deref(), Some(digest.as_str()));
    }

    // AC2: a second run against an unchanged remote transfers no payload — the
    // origin's byte counter does not move, and the redirect target is not asked
    // for at all.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn second_fetch_against_an_unchanged_remote_transfers_no_payload() {
        let digest = payload_digest();
        let linked = format!("\"{}\"", digest);
        let server = origin::Origin::start(origin::Spec::linked(&linked));
        let dir = tempfile::tempdir().unwrap();
        let url = server.url("/file");

        fetch(dir.path(), &url).unwrap();
        let after_first = server.bytes_served();
        assert_eq!(after_first, origin::PAYLOAD.len());

        fetch(dir.path(), &url).unwrap();
        assert_eq!(server.bytes_served(), after_first, "second run transferred");
        assert_eq!(server.hits("/blob"), 1, "redirect target re-requested");

        let sent = server.heads();
        let conditional = sent.last().expect("a second request was made");
        assert!(
            conditional
                .to_ascii_lowercase()
                .contains(&format!("if-none-match: {}", linked.to_ascii_lowercase())),
            "{}",
            conditional
        );
    }

    // AC2 for the other redirect shape — a `302` carrying no `ETag`, no
    // `Last-Modified` and no `X-Linked-ETag`, which is what an http→https upgrade or
    // a release redirect sends. Nothing on the first hop is storable, so the second
    // run's `304` can only come from the target's validator, reached by forwarding
    // the conditional. Recording the first hop's alone re-transfers PAYLOAD.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_redirect_carrying_no_validator_records_the_targets() {
        let server = origin::Origin::start(origin::Spec::default());
        let dir = tempfile::tempdir().unwrap();
        let url = server.url("/file");
        let out = dir.path().join("build/artifact.bin");

        fetch(dir.path(), &url).unwrap();
        let after_first = server.bytes_served();
        assert_eq!(after_first, origin::PAYLOAD.len());
        let meta = crate::ingress_meta::read(&out).expect("sidecar written");
        assert_eq!(meta.etag.as_deref(), Some(origin::STORAGE_VALIDATOR));

        fetch(dir.path(), &url).unwrap();
        assert_eq!(server.bytes_served(), after_first, "second run transferred");
        assert_eq!(std::fs::read(&out).unwrap(), origin::PAYLOAD);

        let sent = server.heads();
        let to_target = sent
            .last()
            .expect("the target was asked")
            .to_ascii_lowercase();
        assert!(
            to_target.contains(&format!(
                "if-none-match: {}",
                origin::STORAGE_VALIDATOR.to_ascii_lowercase()
            )),
            "{}",
            to_target
        );
    }

    // The fallback is reached only where the first hop offers nothing: a `302` with a
    // `Last-Modified` and no `ETag` keeps that, and `STORAGE_VALIDATOR` — which the
    // origin would not recognise — stays out of the sidecar.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_redirect_offering_only_last_modified_keeps_it() {
        const WHEN: &str = "Wed, 21 Oct 2026 07:28:00 GMT";
        let server = origin::Origin::start(origin::Spec {
            redirect_last_modified: Some(WHEN.to_string()),
            ..Default::default()
        });
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("build/artifact.bin");

        fetch(dir.path(), &server.url("/file")).unwrap();

        let meta = crate::ingress_meta::read(&out).expect("sidecar written");
        assert_eq!(meta.last_modified.as_deref(), Some(WHEN));
        assert!(meta.etag.is_none(), "{:?}", meta.etag);
    }

    // AC3: where the origin declares a content hash on the redirect, the sidecar
    // records it with no body transferred — here on a sidecar that predates the
    // field, against an origin that ignores conditional requests entirely.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn declared_content_hash_is_recorded_without_downloading_the_body() {
        let digest = payload_digest();
        let server = origin::Origin::start(origin::Spec::linked_unconditional(&format!(
            "\"{}\"",
            digest
        )));
        let dir = tempfile::tempdir().unwrap();
        let url = server.url("/file");
        let out = dir.path().join("build/artifact.bin");

        fetch(dir.path(), &url).unwrap();
        let after_first = server.bytes_served();

        // Roll the sidecar back to the shape a fetch wrote before `content_sha256`
        // existed: the bytes' own hash, and no declaration.
        let mut aged = crate::ingress_meta::read(&out).unwrap();
        aged.content_sha256 = None;
        crate::ingress_meta::write(&out, &aged).unwrap();

        fetch(dir.path(), &url).unwrap();

        assert_eq!(server.bytes_served(), after_first, "second run transferred");
        assert_eq!(server.hits("/blob"), 1, "redirect target re-requested");
        assert_eq!(std::fs::read(&out).unwrap(), origin::PAYLOAD);
        let meta = crate::ingress_meta::read(&out).unwrap();
        assert_eq!(meta.content_sha256.as_deref(), Some(digest.as_str()));
    }

    // The declared hash is a match test, not a blanket skip: a different one is
    // fetched.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_changed_declared_content_hash_transfers_again() {
        let digest = payload_digest();
        let server = origin::Origin::start(origin::Spec::linked_unconditional(&format!(
            "\"{}\"",
            digest
        )));
        let dir = tempfile::tempdir().unwrap();
        let url = server.url("/file");
        let out = dir.path().join("build/artifact.bin");

        fetch(dir.path(), &url).unwrap();
        let mut moved = crate::ingress_meta::read(&out).unwrap();
        moved.sha256 = "0".repeat(64);
        moved.content_sha256 = Some("1".repeat(64));
        crate::ingress_meta::write(&out, &moved).unwrap();

        fetch(dir.path(), &url).unwrap();
        assert_eq!(server.hits("/blob"), 2);
        assert_eq!(server.bytes_served(), origin::PAYLOAD.len() * 2);
    }

    // A credential the Protocol set reaches the origin and stops there — the
    // policy ureq applies when it follows a redirect itself.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn credentials_are_not_forwarded_to_the_redirect_target() {
        let server = origin::Origin::start(origin::Spec::linked_unconditional("\"opaque\""));
        let dir = tempfile::tempdir().unwrap();
        let env = HashMap::new();
        let with: Value = serde_yaml::from_str(&format!(
            "url: {}\nout: build/artifact.bin\nheaders:\n  Authorization: 'Bearer hunter2'\n  Cookie: 'session=abc'",
            server.url("/file")
        ))
        .unwrap();
        HttpFetch.run(&with, &test_ctx(dir.path(), &env)).unwrap();

        let heads = server.heads();
        let to_origin = heads.first().unwrap().to_ascii_lowercase();
        let to_target = heads.last().unwrap().to_ascii_lowercase();
        assert!(to_origin.contains("authorization: bearer hunter2"));
        assert!(to_origin.contains("cookie: session=abc"));
        assert!(!to_target.contains("authorization"), "{}", to_target);
        assert!(!to_target.contains("cookie:"), "{}", to_target);
    }

    // ── the shared fetch cache ──────────────────────────────────────────────
    //
    // The fixture above is a whole origin, so these read the same counters the
    // freshness tests do: `bytes_served` is the number that has to stop growing when a
    // second Protocol wants bytes a first one already has.

    /// An opaque validator — no content hash inside it, so a `304` is the only way the
    /// origin can decline to re-send the payload. That isolates the conditional
    /// request; the declared-hash shortcut has its own test below.
    #[cfg(feature = "http-fetch")]
    const OPAQUE: &str = "\"opaque-v1\"";

    /// A cache under a directory of its own, returned with the directory so the
    /// caller keeps it alive.
    #[cfg(feature = "http-fetch")]
    fn shared_cache() -> (tempfile::TempDir, crate::fetch_cache::FetchCache) {
        let store = tempfile::tempdir().unwrap();
        let cache = crate::fetch_cache::FetchCache::at(store.path().join("cache"));
        (store, cache)
    }

    /// One Protocol's fetch of `url`, with whatever cache the run has and any extra
    /// `with:` lines the test needs.
    #[cfg(feature = "http-fetch")]
    fn fetch_with(
        dir: &Path,
        url: &str,
        cache: Option<&crate::fetch_cache::FetchCache>,
        extra: &str,
    ) -> Result<StepOutput> {
        let env = HashMap::new();
        let with: Value =
            serde_yaml::from_str(&format!("url: {url}\nout: build/artifact.bin\n{extra}")).unwrap();
        HttpFetch.run(
            &with,
            &OpContext {
                dir,
                db_path: dir,
                env: &env,
                timeout: None,
                cache,
            },
        )
    }

    #[cfg(feature = "http-fetch")]
    fn artifact(dir: &Path) -> PathBuf {
        dir.join("build/artifact.bin")
    }

    /// The single object a cache rooted at `root` holds. Reached through the
    /// filesystem rather than through the cache's own accessors, because a test that
    /// corrupts an entry has to reach it the way rot would.
    #[cfg(feature = "http-fetch")]
    fn sole_object(root: &Path) -> PathBuf {
        let mut objects: Vec<_> = std::fs::read_dir(root.join("objects"))
            .expect("the cache filed an object")
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(objects.len(), 1, "{:?}", objects);
        objects.pop().unwrap()
    }

    /// The single locator a cache rooted at `root` holds, and the record inside it.
    /// Read through the filesystem rather than through [`FetchCache::lookup`], because
    /// the question is which URL the store was filed under — not whether a URL the test
    /// already has in hand is present.
    #[cfg(feature = "http-fetch")]
    fn sole_locator(root: &Path) -> (PathBuf, crate::ingress_meta::FetchMeta) {
        let mut locators: Vec<_> = std::fs::read_dir(root.join("urls"))
            .expect("the cache filed a locator")
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(locators.len(), 1, "{:?}", locators);
        let path = locators.pop().unwrap();
        let record = std::fs::read_to_string(&path).unwrap();
        (path, serde_yaml::from_str(&record).unwrap())
    }

    /// A Protocol already holding `bytes`, beside a sidecar describing them as what
    /// `url` served. Every field of a `<out>.arcmeta` is a field of a file inside the
    /// Protocol, so `url` is as authored as `sha256` is.
    #[cfg(feature = "http-fetch")]
    fn protocol_claiming(dir: &Path, url: &str, bytes: &[u8]) {
        let out = artifact(dir);
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, bytes).unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url: url.to_string(),
                etag: Some(OPAQUE.to_string()),
                sha256: crate::state::content_hash(bytes),
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// Two Protocols agree: same artifact bytes, and the same content identity beside
    /// them. `fetched_unix` is a wall clock — no two runs share one — so it is
    /// compared for presence, and every other field for value.
    #[cfg(feature = "http-fetch")]
    fn assert_same_result(left: &Path, right: &Path) {
        let (a, b) = (artifact(left), artifact(right));
        assert_eq!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "artifact bytes differ between {} and {}",
            left.display(),
            right.display()
        );
        let (ma, mb) = (
            crate::ingress_meta::read(&a).expect("sidecar written"),
            crate::ingress_meta::read(&b).expect("sidecar written"),
        );
        assert_eq!(ma.url, mb.url);
        assert_eq!(ma.request_headers, mb.request_headers);
        assert_eq!(ma.etag, mb.etag);
        assert_eq!(ma.last_modified, mb.last_modified);
        assert_eq!(ma.sha256, mb.sha256);
        assert_eq!(ma.content_sha256, mb.content_sha256);
        assert!(ma.fetched_unix.is_some() && mb.fetched_unix.is_some());
    }

    // The whole point, in one test: two Protocols name the same URL and the payload
    // crosses the wire once. The second still asks the origin — it asks with a validator, so
    // the answer is `304` and the bytes come off the shared store.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn two_protocols_fetching_the_same_url_transfer_it_once() {
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let url = server.url("/file");
        let (_store, cache) = shared_cache();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        fetch_with(first.path(), &url, Some(&cache), "").unwrap();
        assert_eq!(
            server.bytes_served(),
            origin::PAYLOAD.len(),
            "the first Protocol transfers the artifact"
        );

        fetch_with(second.path(), &url, Some(&cache), "").unwrap();
        assert_eq!(
            server.bytes_served(),
            origin::PAYLOAD.len(),
            "the second Protocol transferred it again"
        );
        assert_eq!(
            std::fs::read(artifact(second.path())).unwrap(),
            origin::PAYLOAD
        );
        assert_same_result(first.path(), second.path());
    }

    // The same, through the origin that declares a content hash and ignores
    // conditional requests: the declaration is checked against what the shared store
    // holds, so the body is never read even though the response is a `200`.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_declared_content_hash_the_cache_already_holds_transfers_nothing() {
        let digest = payload_digest();
        let server =
            origin::Origin::start(origin::Spec::linked_unconditional(&format!("\"{digest}\"")));
        let url = server.url("/file");
        let (_store, cache) = shared_cache();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        fetch_with(first.path(), &url, Some(&cache), "").unwrap();
        fetch_with(second.path(), &url, Some(&cache), "").unwrap();

        assert_eq!(server.bytes_served(), origin::PAYLOAD.len());
        assert_eq!(server.hits("/blob"), 1, "the payload host was asked twice");
        assert_same_result(first.path(), second.path());
    }

    // Parity, demonstrated: the same two Protocols against the same origin, once with
    // no cache and once with one. The cached pair transfers the payload once and the
    // uncached pair twice — and all four Protocols end holding the same artifact and
    // the same content identity.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_cached_run_and_an_uncached_run_agree() {
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let url = server.url("/file");
        let uncached = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        for dir in &uncached {
            fetch_with(dir.path(), &url, None, "").unwrap();
        }
        let without = server.bytes_served();
        assert_eq!(
            without,
            origin::PAYLOAD.len() * 2,
            "without a cache each Protocol transfers the artifact"
        );

        let (_store, cache) = shared_cache();
        let cached = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        for dir in &cached {
            fetch_with(dir.path(), &url, Some(&cache), "").unwrap();
        }
        assert_eq!(
            server.bytes_served() - without,
            origin::PAYLOAD.len(),
            "with one, the pair transfers it once"
        );

        for dir in [&uncached[1], &cached[0], &cached[1]] {
            assert_same_result(uncached[0].path(), dir.path());
        }
    }

    // The cache does not decide freshness. A hit is put to the origin, which is what
    // keeps a cached run agreeing with an uncached one when the remote has moved —
    // and the exception is a manifest that pinned the hash, which named the bytes and
    // leaves the origin nothing to add.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_cache_hit_is_revalidated_unless_the_manifest_pinned_the_hash() {
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let url = server.url("/file");
        let (_store, cache) = shared_cache();
        let first = tempfile::tempdir().unwrap();
        fetch_with(first.path(), &url, Some(&cache), "").unwrap();
        let asked = server.hits("/file");

        let second = tempfile::tempdir().unwrap();
        fetch_with(second.path(), &url, Some(&cache), "").unwrap();
        assert_eq!(
            server.hits("/file"),
            asked + 1,
            "an unpinned hit must still be revalidated"
        );

        let third = tempfile::tempdir().unwrap();
        fetch_with(
            third.path(),
            &url,
            Some(&cache),
            &format!("sha256: {}", payload_digest()),
        )
        .unwrap();
        assert_eq!(
            server.hits("/file"),
            asked + 1,
            "a pinned hit must not ask the origin"
        );
        assert_eq!(
            std::fs::read(artifact(third.path())).unwrap(),
            origin::PAYLOAD
        );
        assert_same_result(second.path(), third.path());
    }

    // Revalidation is not a formality: against an origin that ignores conditional
    // requests, the cached run downloads exactly as the uncached run does. The store
    // never answers on behalf of an origin that did not confirm it.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn an_origin_that_ignores_conditionals_is_still_the_authority() {
        let server = origin::Origin::start(origin::Spec::linked_unconditional(OPAQUE));
        let url = server.url("/file");
        let (_store, cache) = shared_cache();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        fetch_with(first.path(), &url, Some(&cache), "").unwrap();
        fetch_with(second.path(), &url, Some(&cache), "").unwrap();

        assert_eq!(
            server.bytes_served(),
            origin::PAYLOAD.len() * 2,
            "the origin's answer, not the cache's, decides"
        );
        assert_same_result(first.path(), second.path());
    }

    // An entry is verified on the way out, so bytes that have rotted in the store are
    // refused rather than handed to the next Protocol — which costs that Protocol a
    // transfer, and is the only acceptable price.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_corrupt_entry_is_not_served_to_a_second_consumer() {
        let store = tempfile::tempdir().unwrap();
        let root = store.path().join("cache");
        let cache = crate::fetch_cache::FetchCache::at(root.clone());
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let url = server.url("/file");
        let first = tempfile::tempdir().unwrap();
        fetch_with(first.path(), &url, Some(&cache), "").unwrap();

        // One byte flipped, length unchanged — the mutation a size check would miss.
        let object = sole_object(&root);
        let mut rotted = std::fs::read(&object).unwrap();
        *rotted.last_mut().unwrap() ^= 0b0000_0001;
        std::fs::write(&object, &rotted).unwrap();

        let second = tempfile::tempdir().unwrap();
        fetch_with(second.path(), &url, Some(&cache), "").unwrap();

        assert_eq!(
            std::fs::read(artifact(second.path())).unwrap(),
            origin::PAYLOAD,
            "the second Protocol got the artifact, not the rot"
        );
        assert_eq!(
            server.bytes_served(),
            origin::PAYLOAD.len() * 2,
            "which cost a transfer, as it must"
        );
        // The rot is gone: refused, evicted, and replaced by what the transfer
        // brought back — so the store heals rather than staying poisoned.
        assert_eq!(
            std::fs::read(sole_object(&root)).unwrap(),
            origin::PAYLOAD,
            "the object filed under the key is not the bytes that failed it"
        );
    }

    // A `.arcmeta` is a file inside a Protocol, so its `sha256` is chosen by whoever
    // authored the tree — and that field becomes the shared store's key, which is a
    // path. Run one Protocol holding a poisoned sidecar, then a second naming the same
    // URL, and the second used to unlink whatever the first pointed at: a fetch-only
    // Protocol, exit 0, no message. Nothing outside the store may be touched.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_poisoned_sidecar_cannot_make_a_second_protocol_delete_a_file() {
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let url = server.url("/file");
        let (_store, cache) = shared_cache();

        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("important.txt");
        let contents = b"someone else's work";
        std::fs::write(&victim, contents).unwrap();

        // The first Protocol holds the artifact and a sidecar whose `sha256` is that
        // file's path rather than a digest. `Path::join` replaces the path it is given
        // when the argument is absolute, so this key escapes `<root>/objects` outright.
        let first = tempfile::tempdir().unwrap();
        let out = artifact(first.path());
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, origin::PAYLOAD).unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url: url.clone(),
                etag: Some(OPAQUE.to_string()),
                sha256: victim.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();

        // Its next run revalidates, gets a `304`, keeps its bytes — and offers the
        // sidecar to the shared store, which is where the key gets in.
        fetch_with(first.path(), &url, Some(&cache), "").unwrap();
        assert!(
            !cache.holds(&url),
            "a locator was filed under a key that is not a digest"
        );

        // A second Protocol names the same URL. Its lookup is what used to evict.
        let second = tempfile::tempdir().unwrap();
        fetch_with(second.path(), &url, Some(&cache), "").unwrap();

        assert_eq!(
            std::fs::read(&victim).unwrap(),
            contents,
            "a fetch deleted a file outside the cache root"
        );
        assert_eq!(
            std::fs::read(artifact(second.path())).unwrap(),
            origin::PAYLOAD,
            "and the second Protocol still got its artifact"
        );
    }

    // A transfer that does not match the pin is refused before it lands: the manifest
    // said which artifact it wanted, and this is not it.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_transfer_that_misses_the_pin_is_refused() {
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let dir = tempfile::tempdir().unwrap();
        let wrong = crate::state::content_hash(b"some other artifact entirely");

        let err = fetch_with(
            dir.path(),
            &server.url("/file"),
            None,
            &format!("sha256: {wrong}"),
        )
        .expect_err("a pin the bytes miss must fail the step");
        assert!(
            matches!(err, Error::StepExecution { .. }),
            "and must not be retried: {err:?}"
        );
        assert!(!artifact(dir.path()).exists(), "nothing lands");
        assert!(
            !dir.path().join("build/artifact.part").exists(),
            "and no partial file is left to be read"
        );
    }

    // A pin that cannot be a SHA-256 is a manifest error, caught where every other
    // `with:` mistake is — at load, by `assets`, before a run starts.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_malformed_pin_is_a_load_time_manifest_error() {
        let with: Value = serde_yaml::from_str(
            "url: https://x.invalid/a.parquet\nout: build/a.parquet\nsha256: deadbeef",
        )
        .unwrap();
        let err = assets_for("http_fetch", Some(&with)).expect_err("a bad pin is refused");
        assert!(matches!(err, Error::ManifestValidation(_)), "{err:?}");
        assert!(err.to_string().contains("64 hex characters"), "{err}");
    }

    // Bytes behind a credential belong to the credential, not to the URL, and this
    // store is keyed by URL. So a credentialed fetch neither fills it nor reads it.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_credentialed_fetch_is_not_shared() {
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let url = server.url("/file");
        let (_store, cache) = shared_cache();
        let dir = tempfile::tempdir().unwrap();

        fetch_with(
            dir.path(),
            &url,
            Some(&cache),
            "headers:\n  Authorization: 'Bearer hunter2'\n",
        )
        .unwrap();

        assert_eq!(
            std::fs::read(artifact(dir.path())).unwrap(),
            origin::PAYLOAD
        );
        assert!(
            !cache.holds(&url),
            "a credentialed fetch must not seed the store"
        );
    }

    // A Protocol that already holds the artifact seeds the store from it, so the cache
    // arriving mid-corpus does not cost a re-download of what is already on disk.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn an_existing_artifact_seeds_the_store_on_the_next_run() {
        let server = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let url = server.url("/file");
        let first = tempfile::tempdir().unwrap();

        // Fetched before there was a cache to file it in.
        fetch_with(first.path(), &url, None, "").unwrap();
        let (_store, cache) = shared_cache();
        assert!(!cache.holds(&url));

        // The next run of the same Protocol revalidates, keeps its bytes, and files
        // them.
        fetch_with(first.path(), &url, Some(&cache), "").unwrap();
        assert!(
            cache.holds(&url),
            "the store is seeded from what was already there"
        );

        let second = tempfile::tempdir().unwrap();
        fetch_with(second.path(), &url, Some(&cache), "").unwrap();
        assert_eq!(
            server.bytes_served(),
            origin::PAYLOAD.len(),
            "so a second Protocol transfers nothing"
        );
        assert_same_result(first.path(), second.path());
    }

    // The store files a locator under the record's own `url`, and the record a keeping
    // Protocol offers it is `<out>.arcmeta` — a file inside that Protocol. So a
    // Protocol whose only step fetches its OWN origin could name a third party's URL in
    // the sidecar and have the store file that URL against bytes nobody fetched from
    // it. Exit 0, no warning, and the next Protocol to name the URL for real is served
    // them under a sidecar carrying the genuine URL and the genuine validator.
    //
    // Two loopback origins, because the whole defect is the gap between the URL the
    // step names and the URL the sidecar does.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_sidecar_cannot_seed_the_store_for_a_url_the_step_did_not_name() {
        const SUBSTITUTED: &[u8] = b"bytes the genuine origin never served";

        let hostile_origin = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let genuine_origin = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let hostile_url = hostile_origin.url("/file");
        let genuine_url = genuine_origin.url("/file");

        let store = tempfile::tempdir().unwrap();
        let root = store.path().join("cache");
        let cache = crate::fetch_cache::FetchCache::at(root.clone());

        // One `http_fetch`, of its own origin. It holds its own bytes already, so the
        // conditional request is answered `304` and the keep-and-seed path runs.
        let hostile = tempfile::tempdir().unwrap();
        protocol_claiming(hostile.path(), &genuine_url, SUBSTITUTED);
        fetch_with(hostile.path(), &hostile_url, Some(&cache), "").unwrap();

        assert!(
            !cache.holds(&genuine_url),
            "a Protocol that never fetched that URL filed the store's locator for it"
        );
        assert_eq!(
            sole_locator(&root).1.url,
            hostile_url,
            "the store may only be told about the URL the step named"
        );

        // What a later Protocol gets for the genuine URL, against what it gets with no
        // store at all — the parity a cached run owes an uncached one, which is the
        // acceptance this defect fails as well as the substitution itself.
        let served = tempfile::tempdir().unwrap();
        fetch_with(served.path(), &genuine_url, Some(&cache), "").unwrap();
        assert_eq!(
            std::fs::read(artifact(served.path())).unwrap(),
            origin::PAYLOAD,
            "a later Protocol was served the substituted bytes"
        );
        let uncached = tempfile::tempdir().unwrap();
        fetch_with(uncached.path(), &genuine_url, None, "").unwrap();
        assert_same_result(served.path(), uncached.path());
    }

    // The declared-content-hash shortcut decides on `sha256`, which the store has
    // hashed against the object's bytes, and not on `content_sha256` — a value copied
    // from a response head into a `.arcmeta`, which is plain YAML inside a Protocol
    // directory that anything able to write a file can author.
    //
    // The two differ when the verified hash says the bytes are not the declared
    // artifact, so a shortcut that accepts the claim keeps bytes it has just been told
    // are the wrong ones — here in both directions. A Protocol holding substituted
    // bytes answers the origin's declaration with a copy of the declaration, skips the
    // transfer, keeps the substitution and files it in the shared store for the URL its
    // step named; the next Protocol to name that URL is handed the same match and
    // served the substituted bytes.
    //
    // Neither half needs a `304` or a live validator: the sidecar's `ETag` here is
    // worthless and this origin ignores conditional requests, so the shortcut is the
    // whole of what decides.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_declared_content_hash_is_matched_against_verified_bytes_only() {
        const SUBSTITUTED: &[u8] = b"bytes the genuine origin never served";

        let digest = payload_digest();
        let server =
            origin::Origin::start(origin::Spec::linked_unconditional(&format!("\"{digest}\"")));
        let url = server.url("/file");
        let (_store, cache) = shared_cache();

        // A Protocol whose artifact is not what the origin serves, beside a sidecar
        // declaring the origin's own published content hash for it.
        let seeding = tempfile::tempdir().unwrap();
        let out = artifact(seeding.path());
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, SUBSTITUTED).unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url: url.clone(),
                etag: Some("\"stale-and-worthless\"".to_string()),
                sha256: crate::state::content_hash(SUBSTITUTED),
                content_sha256: Some(digest.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        fetch_with(seeding.path(), &url, Some(&cache), "").unwrap();
        assert_eq!(
            std::fs::read(&out).unwrap(),
            origin::PAYLOAD,
            "the transfer was skipped on a claim the sidecar made about itself"
        );
        assert_eq!(
            cache.lookup(&url).map(|e| e.sha256),
            Some(digest.clone()),
            "and the shared store was seeded with bytes the origin never served"
        );

        // What a second Protocol naming the same URL is served, against what it gets
        // with no store at all.
        let served = tempfile::tempdir().unwrap();
        fetch_with(served.path(), &url, Some(&cache), "").unwrap();
        assert_eq!(
            std::fs::read(artifact(served.path())).unwrap(),
            origin::PAYLOAD,
            "a second Protocol was served the substituted bytes"
        );
        let uncached = tempfile::tempdir().unwrap();
        fetch_with(uncached.path(), &url, None, "").unwrap();
        assert_same_result(served.path(), uncached.path());
    }

    // The guard that decides whether to seed and the write it guards have to ask about
    // the same URL. While the write took the sidecar's `url` and the guard asked about
    // the step's, the guard's answer could never become true — so a Protocol that keeps
    // its bytes re-filed the store on every single run.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn a_seeded_store_is_not_re_filed_on_every_run() {
        const KEPT: &[u8] = b"bytes this Protocol already had";

        let hostile_origin = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let genuine_origin = origin::Origin::start(origin::Spec::linked(OPAQUE));
        let hostile_url = hostile_origin.url("/file");

        let store = tempfile::tempdir().unwrap();
        let root = store.path().join("cache");
        let cache = crate::fetch_cache::FetchCache::at(root.clone());

        let dir = tempfile::tempdir().unwrap();
        protocol_claiming(dir.path(), &genuine_origin.url("/file"), KEPT);
        fetch_with(dir.path(), &hostile_url, Some(&cache), "").unwrap();

        // Stamp the entry the first run filed. A second write rewrites the record
        // wholesale, so the stamp surviving is the proof the store was left alone.
        let (locator, mut filed) = sole_locator(&root);
        filed.etag = Some("\"filed-once\"".to_string());
        std::fs::write(&locator, serde_yaml::to_string(&filed).unwrap()).unwrap();

        fetch_with(dir.path(), &hostile_url, Some(&cache), "").unwrap();
        assert_eq!(
            sole_locator(&root).1.etag,
            filed.etag,
            "the store was filed again by a run that had nothing new to tell it"
        );
    }
}
