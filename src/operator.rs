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
use std::path::Path;
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
    /// `ARC_PARAM_*` environment for the step. Read by ingress/model operators (B2).
    #[allow(dead_code)]
    pub env: &'a HashMap<String, String>,
    /// Step timeout, if any. Read by ingress/model operators (B2).
    #[allow(dead_code)]
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
#[cfg(feature = "opendal")]
static OPENDAL_FETCH: OpendalFetch = OpendalFetch;

/// The built-in operator catalog. `opendal_fetch` is present only when the
/// `opendal` feature is enabled (see Cargo.toml — off by default to keep the
/// single binary lean).
fn catalog() -> Vec<&'static dyn Operator> {
    #[allow(unused_mut)]
    let mut ops: Vec<&'static dyn Operator> = vec![&PARQUET_EXPORT, &HTTP_FETCH];
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
// http_fetch (ingress) — the curl/wget of the catalog: a plain authenticated
// GET streamed atomically to `out`.
//
// Built on **ureq** — already an arcform dependency, blocking, ~0 transitive
// weight, http/https only. The operator is a straight-line GET: resilience
// (retry + backoff) comes from the ENGINE's step-retry loop, which already
// wraps every op run via `defaults.retry` — a transient failure returns the
// retryable `StepFailed` and the runner re-attempts with backoff. A default
// User-Agent is always set (gov registries — SEC — 403 a missing UA) and any
// `headers` the Protocol supplies are layered on top. This is the workhorse
// that retires `fetch_edgar`/`fetch_gleif` and the header-gated SEC fetch.
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

/// Default UA — an unset User-Agent is a 403 at the SEC and several registries.
const DEFAULT_UA: &str = "arcform-http_fetch/1 (+https://meridian.online)";

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
        let resp = req
            .call()
            .map_err(|e| fetch_failed(format!("http_fetch: GET {}: {}", cfg.url, e)))?;

        // Stream to a sibling `.part` file, then atomically rename into place —
        // a killed run never leaves a truncated artifact that looks complete.
        let tmp = out.with_extension("part");
        let mut reader = resp.into_reader();
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| fetch_failed(format!("http_fetch: create {}: {}", tmp.display(), e)))?;
        std::io::copy(&mut reader, &mut file)
            .map_err(|e| fetch_failed(format!("http_fetch: write {}: {}", tmp.display(), e)))?;
        let _ = file.sync_all();
        drop(file);
        std::fs::rename(&tmp, &out)
            .map_err(|e| fetch_failed(format!("http_fetch: rename {}: {}", out.display(), e)))?;

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

#[cfg(test)]
mod tests {
    use super::*;

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
