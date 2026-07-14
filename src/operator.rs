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

use std::collections::HashMap;
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

/// The built-in operator catalog.
fn catalog() -> Vec<&'static dyn Operator> {
    vec![&PARQUET_EXPORT]
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
}
