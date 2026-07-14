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
//! [`catalog`] — a namespace deliberately *distinct* from the pipeline `registry`
//! (DECISIONS §G choice 0011). Config is validated by typed deserialization (a `with:`
//! block that doesn't deserialize into the operator's config is a load-time error);
//! JSON-Schema emission for Brightfield authoring forms is a later addition.
//!
//! Design spec: `bearing/research/arcform-typed-operators.md`.

use std::collections::{BTreeMap, HashMap};
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
static HTTP_FETCH: HttpFetch = HttpFetch;
static DATAPACKAGE_DESCRIBE: DatapackageDescribe = DatapackageDescribe;
#[cfg(feature = "opendal")]
static OPENDAL_FETCH: OpendalFetch = OpendalFetch;

/// The built-in operator catalog. `opendal_fetch` is present only when the
/// `opendal` feature is enabled (see Cargo.toml — off by default to keep the
/// single binary lean).
fn catalog() -> Vec<&'static dyn Operator> {
    #[allow(unused_mut)]
    let mut ops: Vec<&'static dyn Operator> =
        vec![&PARQUET_EXPORT, &HTTP_FETCH, &DATAPACKAGE_DESCRIBE];
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
            Ok(StepOutput { stderr: String::new(), stdout: None })
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
    let mut a = vec!["run".to_string(), "--script".to_string(), script.to_string()];
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
    let needs_write = std::fs::read_to_string(&path).map(|s| s != bytes).unwrap_or(true);
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

        let order = cfg
            .order_by
            .as_deref()
            .map(|o| format!(" ORDER BY {}", o))
            .unwrap_or_default();
        let mut opts = format!("FORMAT parquet, COMPRESSION {}", cfg.compression);
        if let Some(rg) = cfg.row_group_size {
            opts.push_str(&format!(", ROW_GROUP_SIZE {}", rg));
        }
        let sql = format!(
            "COPY (SELECT * FROM {input}{order}) TO '{dest}' ({opts});",
            input = cfg.input,
            order = order,
            dest = dest.display(),
            opts = opts,
        );

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
// Paired with the `fresh` precondition (which HEAD-probes the same sidecar), a
// step re-runs — and propagates downstream — only when the remote actually
// changed: content-addressed ingress, not the clock-based mtime `modified_after`.
// This is the workhorse that retires `fetch_edgar`/`fetch_gleif` and the SEC fetch.
// ─────────────────────────────────────────────────────────────────────────────

struct HttpFetch;

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
}

impl HttpFetchConfig {
    fn parse(with: &Value) -> Result<Self> {
        serde_yaml::from_value(with.clone()).map_err(|e| {
            Error::ManifestValidation(format!("http_fetch: invalid `with:` config: {}", e))
        })
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

impl Operator for HttpFetch {
    fn name(&self) -> &'static str {
        "http_fetch"
    }

    fn version(&self) -> semver::Version {
        semver::Version::new(1, 0, 0)
    }

    fn assets(&self, with: &Value) -> Result<OpAssets> {
        let cfg = HttpFetchConfig::parse(with)?;
        // The network source is not a graph node; only the local artifact is.
        Ok(OpAssets {
            reads: vec![],
            produces: vec![cfg.out.to_lowercase()],
        })
    }

    fn run(&self, with: &Value, ctx: &OpContext) -> Result<StepOutput> {
        use std::io::{Read, Write};

        use sha2::{Digest, Sha256};

        use crate::ingress_meta::{self, FetchMeta, DEFAULT_UA};

        let cfg = HttpFetchConfig::parse(with)?;
        let out = ctx.dir.join(&cfg.out);
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Default UA first, then Protocol overrides (a `User-Agent` key wins).
        let mut req = ureq::get(&cfg.url).set("User-Agent", DEFAULT_UA);
        for (k, v) in &cfg.headers {
            req = req.set(k, v);
        }
        // The freshness contract: if we've fetched this artifact before and it's
        // still on disk, replay the stored ETag / Last-Modified as a conditional
        // request. An unchanged remote answers `304` and we keep the bytes.
        let prior = ingress_meta::read(&out);
        if out.exists() {
            if let Some(ref p) = prior {
                if let Some(ref etag) = p.etag {
                    req = req.set("If-None-Match", etag);
                }
                if let Some(ref lm) = p.last_modified {
                    req = req.set("If-Modified-Since", lm);
                }
            }
        }

        let resp = match req.call() {
            Ok(resp) => resp,
            // 304 Not Modified — the remote is byte-unchanged. Keep the file + sidecar
            // untouched (so its content identity, hence downstream staleness, is stable).
            Err(ureq::Error::Status(304, _)) => {
                return Ok(StepOutput { stderr: String::new(), stdout: None });
            }
            Err(e) => return Err(fetch_failed(format!("http_fetch: GET {}: {}", cfg.url, e))),
        };
        if resp.status() == 304 {
            return Ok(StepOutput { stderr: String::new(), stdout: None });
        }

        // 200: capture the server's content identity before consuming the body.
        let etag = resp.header("ETag").map(str::to_string);
        let last_modified = resp.header("Last-Modified").map(str::to_string);

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
        std::fs::rename(&tmp, &out)
            .map_err(|e| fetch_failed(format!("http_fetch: rename {}: {}", out.display(), e)))?;

        // Persist the content identity for next run's conditional request + the
        // `fresh` precondition's HEAD probe. Best-effort: a failed sidecar write
        // doesn't fail the fetch (it just forfeits the next conditional/skip).
        let fetched_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        let meta = FetchMeta {
            url: cfg.url.clone(),
            request_headers: cfg.headers.clone(),
            etag,
            last_modified,
            sha256: format!("{:x}", hasher.finalize()),
            fetched_unix,
        };
        let _ = ingress_meta::write(&out, &meta);

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
        use opendal::{services, Operator as DalOperator};

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
                    let transport =
                        opendal_http_transport_reqwest::ReqwestTransport::new(client);
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
            Error::ManifestValidation(format!("datapackage_describe: invalid `with:` config: {}", e))
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
        run_process("uv", &args, ctx, OutputMode::Capture, "datapackage_describe")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx<'a>(dir: &'a Path, env: &'a HashMap<String, String>) -> OpContext<'a> {
        OpContext { dir, db_path: dir, env, timeout: None }
    }

    #[test]
    fn uv_run_args_arg_order_is_stable() {
        assert_eq!(
            uv_run_args("scripts/x.py", &["--out".to_string(), "o.parquet".to_string()]),
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
        match run_process("arc-no-such-binary-xyz", &[], &ctx, OutputMode::Inherit, "t") {
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
        let with: Value = serde_yaml::from_str("input: crosswalk_edges\ndest: build/out.parquet").unwrap();
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
    fn http_fetch_declares_only_its_output() {
        // Ingress produces the local artifact and reads no graph node (the
        // network source isn't in the AssetGraph).
        let hf: Value = serde_yaml::from_str("url: https://x/edgar.parquet\nout: build/edgar.parquet").unwrap();
        let a = assets_for("http_fetch", Some(&hf)).unwrap();
        assert_eq!(a.produces, vec!["build/edgar.parquet".to_string()]);
        assert!(a.reads.is_empty());
    }

    #[cfg(feature = "opendal")]
    #[test]
    fn opendal_fetch_declares_only_its_output() {
        let of: Value = serde_yaml::from_str("from: s3://securelake/edgar.parquet\nto: build/edgar.parquet").unwrap();
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
}
