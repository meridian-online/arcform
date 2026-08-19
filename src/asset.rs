//! Asset graph construction and dependency validation.
//!
//! Builds a graph of data assets from three sources:
//! 1. **Inferred** — sqlparser-rs parses SQL files to discover outputs/inputs
//! 2. **Declared** — command steps' `produces`/`depends_on` fields
//! 3. **Overrides** — the manifest's top-level `assets:` section
//!
//! The merged graph is validated against step declaration order before execution.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::asset_kind::{AssetKind, default_kind_for_declared_name};
use crate::error::{Error, Result};
use crate::introspect;
use crate::manifest::Manifest;

/// The assets associated with a single step.
#[derive(Debug, Clone, Default)]
pub struct StepAssets {
    /// Asset names this step produces (creates/writes/modifies) — lowercased. A
    /// DuckDB table identifier is case-insensitive, so two spellings of the same
    /// table have to land on one graph node; this is the identity every other
    /// consumer (propagation, `all_produced`, the printed graph) matches on.
    pub produces: BTreeSet<String>,
    /// Asset names this step reads data from (external dependencies) — lowercased,
    /// same reasoning as `produces`.
    pub reads: BTreeSet<String>,
    /// CTE names — step-internal assets visible in lineage but not cross-step dependencies.
    pub internal: BTreeSet<String>,
    /// Asset names this step destroys (DROP operations).
    pub destroys: BTreeSet<String>,
    /// For every lowercased name in `produces` or `reads`, every RAW (case-preserved,
    /// exactly as declared) spelling that lowercased to it — almost always exactly
    /// one. More than one means two declarations collapsed onto the same graph node
    /// purely by case, which `AssetGraph::validate_no_case_collisions` refuses at
    /// load for BOTH `produces:` and `depends_on:`. That gate is what a manifest
    /// author sees; it does not make this field a singleton by construction — a
    /// caller reaching this map without having gone through `AssetGraph::build` plus
    /// that validation (or a future insertion site the gate does not yet cover) can
    /// still see 2+ entries here, so `produced_artifact_hash` treats that count, not
    /// this doc comment, as the source of truth and refuses to pick one by iteration
    /// order. This is what `produced_artifact_hash` reads bytes from — the lowercased
    /// name is graph identity, never a filesystem path; no amount of scanning a
    /// directory recovers a real file's case once this is thrown away, so it has to
    /// be carried, not reconstructed.
    pub declared_case: BTreeMap<String, BTreeSet<String>>,
    /// For every lowercased name in `produces` or `reads`, what it actually is —
    /// carried from wherever it was declared (SQL introspection already knows a
    /// `COPY … PARTITION_BY` target is a directory; an operator's typed config
    /// already knows `dest:` is a file; only `produces:`/`depends_on:`/`assets:`
    /// entries with no operator or parser to consult fall back to a guess). Never
    /// reconstructed later from the string or the filesystem — this is what
    /// `produced_artifact_hash` reads to decide HOW to hash a produced asset, not
    /// WHETHER to hash it at all.
    pub declared_kind: BTreeMap<String, AssetKind>,
}

impl StepAssets {
    /// Insert `raw` (a name exactly as declared — a SQL-introspected path, an
    /// operator's config value, an explicit `produces:`/`depends_on:` entry, or an
    /// `assets:` override) into `set`, lowercased for graph identity, while recording
    /// `raw` itself in `declared_case` under that same lowercased key and `kind` in
    /// `declared_kind` under the same key. The one path every insertion into
    /// `produces`/`reads` goes through, so the three can never drift apart.
    ///
    /// `declared_kind` keeps the FIRST kind recorded for a given lowered name within
    /// a step (`or_insert`, not overwrite): Phase 1 (SQL introspection) and Phase 1b
    /// (operator config) carry real, source-derived kind; Phase 2/3's declaration
    /// sites have no parser or operator to consult and fall back to a guess
    /// (`default_kind_for_declared_name`). Running phases in that fixed order means
    /// a later guess can never downgrade an earlier, better-informed answer for the
    /// same step.
    fn record(
        set: &mut BTreeSet<String>,
        declared_case: &mut BTreeMap<String, BTreeSet<String>>,
        declared_kind: &mut BTreeMap<String, AssetKind>,
        raw: &str,
        kind: AssetKind,
    ) {
        let lowered = raw.to_lowercase();
        declared_case
            .entry(lowered.clone())
            .or_default()
            .insert(raw.to_string());
        declared_kind.entry(lowered.clone()).or_insert(kind);
        set.insert(lowered);
    }
}

/// The complete asset graph for a pipeline.
#[derive(Debug)]
pub struct AssetGraph {
    /// Per-step asset information, keyed by step name.
    pub steps: BTreeMap<String, StepAssets>,
    /// Warnings generated during graph construction (e.g. parse failures).
    pub warnings: Vec<String>,
}

impl AssetGraph {
    /// Build the asset graph from a manifest.
    ///
    /// For each step:
    /// 1. If it's a SQL step, parse the SQL file to discover assets
    /// 2. If it has explicit produces/depends_on, add those
    /// 3. Apply any overrides from the top-level assets: section
    ///
    /// `manifest_dir` is the directory containing arcform.yaml, used to
    /// resolve relative SQL file paths.
    pub fn build(manifest: &Manifest, manifest_dir: &Path) -> Self {
        let mut graph = AssetGraph {
            steps: BTreeMap::new(),
            warnings: Vec::new(),
        };

        // Phase 1 & 2: Infer from SQL + merge declared fields.
        for step in &manifest.steps {
            let mut step_assets = StepAssets::default();

            // Phase 1: SQL introspection. Already case-preserved — a path embedded
            // in SQL text (e.g. `read_csv('build/ncen/2026q2/REGISTRANT.tsv')`) is
            // captured verbatim by the parser, never lowercased here, so its graph
            // node IS its real on-disk spelling; `record` still runs so
            // `declared_case` holds a uniform entry regardless of provenance.
            if let Some(ref sql_path) = step.sql {
                let full_path = manifest_dir.join(sql_path);
                match std::fs::read_to_string(&full_path) {
                    Ok(sql_content) => match introspect::extract_assets(&sql_content) {
                        Ok(sql_assets) => {
                            for asset in &sql_assets.outputs {
                                let kind = sql_assets
                                    .kinds
                                    .get(asset)
                                    .copied()
                                    .unwrap_or(AssetKind::Table);
                                StepAssets::record(
                                    &mut step_assets.produces,
                                    &mut step_assets.declared_case,
                                    &mut step_assets.declared_kind,
                                    asset,
                                    kind,
                                );
                            }
                            for asset in &sql_assets.inputs {
                                let kind = sql_assets
                                    .kinds
                                    .get(asset)
                                    .copied()
                                    .unwrap_or(AssetKind::Table);
                                StepAssets::record(
                                    &mut step_assets.reads,
                                    &mut step_assets.declared_case,
                                    &mut step_assets.declared_kind,
                                    asset,
                                    kind,
                                );
                            }
                            step_assets.internal.extend(sql_assets.internal);
                            step_assets.destroys.extend(sql_assets.destroys);
                        }
                        Err(warnings) => {
                            // Warn on parse failure, treat as opaque.
                            for w in warnings {
                                graph.warnings.push(format!(
                                    "could not parse {}: {} — treating as opaque step",
                                    sql_path, w
                                ));
                            }
                        }
                    },
                    Err(e) => {
                        // File read errors are not asset graph errors — the
                        // runner will catch missing files during execution.
                        graph.warnings.push(format!(
                            "could not read {}: {} — treating as opaque step",
                            sql_path, e
                        ));
                    }
                }
            }

            // Phase 1b: Operator asset declaration. An `op:` step declares its
            // reads/produces from its typed config, so lineage + stale-propagation hold
            // at its boundary exactly as SQL introspection does — no manual depends_on.
            if let Some(ref op_ref) = step.op {
                match crate::operator::assets_for(op_ref, step.with.as_ref()) {
                    Ok(op_assets) => {
                        for asset in &op_assets.produces {
                            let kind = op_assets
                                .kinds
                                .get(asset)
                                .copied()
                                .unwrap_or_else(|| default_kind_for_declared_name(asset));
                            StepAssets::record(
                                &mut step_assets.produces,
                                &mut step_assets.declared_case,
                                &mut step_assets.declared_kind,
                                asset,
                                kind,
                            );
                        }
                        for asset in &op_assets.reads {
                            let kind = op_assets
                                .kinds
                                .get(asset)
                                .copied()
                                .unwrap_or_else(|| default_kind_for_declared_name(asset));
                            StepAssets::record(
                                &mut step_assets.reads,
                                &mut step_assets.declared_case,
                                &mut step_assets.declared_kind,
                                asset,
                                kind,
                            );
                        }
                    }
                    Err(e) => {
                        // Manifest validation already rejects bad op steps; this is
                        // defensive (treat as opaque rather than panic).
                        graph.warnings.push(format!(
                            "operator step '{}': {} — treating as opaque",
                            step.name, e
                        ));
                    }
                }
            }

            // Phase 2: Explicit declarations (primarily for command steps). Both
            // sides take the same default — see `default_kind_for_declared_name`
            // for what a separator-free `produces:` token costs and why that cost
            // is the one taken.
            for asset in &step.produces {
                StepAssets::record(
                    &mut step_assets.produces,
                    &mut step_assets.declared_case,
                    &mut step_assets.declared_kind,
                    asset,
                    default_kind_for_declared_name(asset),
                );
            }
            for asset in &step.depends_on {
                StepAssets::record(
                    &mut step_assets.reads,
                    &mut step_assets.declared_case,
                    &mut step_assets.declared_kind,
                    asset,
                    default_kind_for_declared_name(asset),
                );
            }

            graph.steps.insert(step.name.clone(), step_assets);
        }

        // Phase 3: Apply overrides from the top-level assets: section.
        for (asset_name, override_entry) in &manifest.assets {
            if let Some(step_assets) = graph.steps.get_mut(&override_entry.produced_by) {
                // Override: ensure this step produces the asset.
                StepAssets::record(
                    &mut step_assets.produces,
                    &mut step_assets.declared_case,
                    &mut step_assets.declared_kind,
                    asset_name,
                    default_kind_for_declared_name(asset_name),
                );

                // Add override dependencies as reads for the producing step.
                for dep in &override_entry.depends_on {
                    StepAssets::record(
                        &mut step_assets.reads,
                        &mut step_assets.declared_case,
                        &mut step_assets.declared_kind,
                        dep,
                        default_kind_for_declared_name(dep),
                    );
                }
            } else {
                graph.warnings.push(format!(
                    "asset '{}' references step '{}' which does not exist",
                    asset_name, override_entry.produced_by
                ));
            }
        }

        graph
    }

    /// Refuse a manifest where two declarations collapse onto the same graph node
    /// purely because they differ only by case, **and there is no single producer to
    /// arbitrate between them**. `asset.rs`'s lowercasing is what makes that collapse
    /// happen at all (needed so a case-insensitive DuckDB table lands on one node).
    ///
    /// **Exempt: a step's `produces:` and another step's `depends_on:` naming the
    /// same asset with different case.** That is the ordinary shape of a
    /// producer→consumer edge, not a collision — the real edgar_gleif manifest has
    /// exactly this, eight times over (`archive_extract` preserves `REGISTRANT.tsv`'s
    /// real case in each quarter's `produces:`; `load`'s hand-written `depends_on:`
    /// spells all eight lowercase), and refusing it would make the gate reject the
    /// flagship manifest it exists to protect. The exemption is keyed on the ASSET,
    /// not the step: any number of readers, in any steps, may spell it however they
    /// like as long as exactly one producer spelling exists, because
    /// `produced_artifact_hash` never consults a reader's own spelling for an asset
    /// something else produces in the first place (`all_produced` filters it out
    /// before `declared_case` is ever reached for it; propagation carries the
    /// staleness instead) — so a reader's case variance was never going to be read
    /// from disk under its own name regardless of what this gate decided.
    ///
    /// **Refused: two or more DISTINCT producer spellings** (`build/Report.csv` and
    /// `build/report.csv` each declared as `produces:` — probe2's shape) — **or two
    /// or more distinct spellings with no producer at all** (two `depends_on:`
    /// entries and nothing that creates either file — an external input's declared
    /// case is the only identity it has, so disagreement there is exactly as real as
    /// disagreement between producers). This is a manifest defect independent of
    /// anything happening on disk, caught here at load rather than left for a
    /// staleness check to discover only once both files are already written.
    ///
    /// It is a complement to, not a replacement for, `produced_artifact_hash`'s own
    /// read-time refusal to pick a raw spelling by iteration order — that refusal is
    /// the backstop for a `declared_case` entry this gate did not (or could not yet)
    /// inspect; it is not covering a *different kind* of collision. Nothing here
    /// inspects the filesystem, so a coincidental, undeclared file sharing a
    /// case-folded name with something real is not this gate's concern — no
    /// declaration collides with it, and `produced_artifact_hash` never looks it up
    /// at all.
    pub fn validate_no_case_collisions(&self) -> Result<()> {
        // (kind, step, raw), grouped by lowered name — kind labels the error with the
        // manifest key a reader would actually go fix, since a `produces:` and a
        // `depends_on:` entry can collide with each other, not just with their own kind.
        let mut by_lowered: BTreeMap<&str, BTreeSet<(&str, &str, &str)>> = BTreeMap::new();
        for (step_name, assets) in &self.steps {
            for (kind, names) in [
                ("produces:", &assets.produces),
                ("depends_on:", &assets.reads),
            ] {
                for lowered in names {
                    let Some(raws) = assets.declared_case.get(lowered) else {
                        continue;
                    };
                    for raw in raws {
                        by_lowered.entry(lowered.as_str()).or_default().insert((
                            kind,
                            step_name.as_str(),
                            raw.as_str(),
                        ));
                    }
                }
            }
        }

        for (lowered, entries) in &by_lowered {
            // A SINGLE producer spelling makes any number of differently-cased
            // READERS harmless, regardless of which step reads them — this is not a
            // narrower "same step" exemption, because the shape it exists for is
            // cross-step: the real edgar_gleif manifest has `archive_extract`
            // preserve `REGISTRANT.tsv`'s real case in `extract_ncen_2025q3`'s
            // `produces:`, while `load`'s hand-written `depends_on:` spells the same
            // file lowercase eight times over (four quarters × two members). That is
            // one asset referenced twice, not two assets colliding — and
            // `produced_artifact_hash` already treats it that way structurally: a
            // reader's own `reads` entry for an asset something else produces is
            // filtered out by `all_produced` before `declared_case` is ever
            // consulted for it (propagation carries the staleness instead), so the
            // reader's spelling was never going to be read from disk under its own
            // name regardless of what this gate decided.
            let produces_raw: BTreeSet<&str> = entries
                .iter()
                .filter(|(kind, _, _)| *kind == "produces:")
                .map(|(_, _, raw)| *raw)
                .collect();
            if produces_raw.len() == 1 {
                continue;
            }

            // Otherwise: two or more PRODUCER spellings disagree (the genuine
            // collision this gate exists for), or nothing produces this asset at all
            // and two or more READERS disagree with no producer to arbitrate between
            // them — an external input's own case is then the only identity it has,
            // so ambiguity there is exactly as real as ambiguity between producers.
            let distinct_raw: BTreeSet<&str> = entries.iter().map(|(_, _, raw)| *raw).collect();
            if distinct_raw.len() > 1 {
                let detail: Vec<String> = entries
                    .iter()
                    .map(|(kind, step, raw)| format!("'{raw}' ({kind} step '{step}')"))
                    .collect();
                return Err(Error::ManifestValidation(format!(
                    "case collision — {} all collapse to the same asset '{}' by case alone; rename one so the spellings are genuinely distinct, or declare it once",
                    detail.join(", "),
                    lowered
                )));
            }
        }

        Ok(())
    }

    /// Validate that the declared step order is consistent with the
    /// dependency graph.
    ///
    /// For each step, check that every asset it reads has been produced
    /// by a step that runs before it in the declared order.
    ///
    /// Returns `Ok(())` if ordering is valid, or the first dependency
    /// violation found.
    pub fn validate_order(&self, step_order: &[String]) -> Result<()> {
        // Build a set of assets produced so far, tracking which step
        // produced each one.
        let mut produced: HashMap<String, String> = HashMap::new();

        for step_name in step_order {
            let Some(step_assets) = self.steps.get(step_name) else {
                continue;
            };

            // Check: does this step read any asset that hasn't been produced yet?
            for read_asset in &step_assets.reads {
                // Skip self-references: a step that reads and writes the same
                // table (e.g. INSERT INTO t SELECT * FROM t) is a self-contained
                // operation, not a cross-step dependency violation.
                if step_assets.produces.contains(read_asset) {
                    continue;
                }

                // Skip assets not in our graph (external tables, CTEs, etc.)
                let is_produced_by_any_step = step_order.iter().any(|s| {
                    self.steps
                        .get(s)
                        .is_some_and(|sa| sa.produces.contains(read_asset))
                });

                if is_produced_by_any_step && !produced.contains_key(read_asset) {
                    // This asset IS produced by a step in the pipeline,
                    // but that step hasn't run yet — ordering violation.
                    let producer = step_order
                        .iter()
                        .find(|s| {
                            self.steps
                                .get(*s)
                                .is_some_and(|sa| sa.produces.contains(read_asset))
                        })
                        .unwrap();

                    return Err(Error::DependencyOrder {
                        reader: step_name.clone(),
                        asset: read_asset.clone(),
                        producer: producer.clone(),
                    });
                }
            }

            // Record all assets this step produces.
            for produced_asset in &step_assets.produces {
                produced.insert(produced_asset.clone(), step_name.clone());
            }
        }

        Ok(())
    }

    /// Compute the transitive set of downstream steps affected when the
    /// given steps are stale. A step is downstream if it reads an asset
    /// produced by a stale step (directly or transitively).
    ///
    /// Returns step names in arbitrary order (callers should not rely on ordering).
    pub fn downstream_steps(&self, stale_steps: &[String]) -> Vec<String> {
        let mut affected: std::collections::HashSet<String> = stale_steps.iter().cloned().collect();
        let mut changed = true;

        // Iterate until no new steps are added (fixed-point).
        while changed {
            changed = false;
            for (step_name, step_assets) in &self.steps {
                if affected.contains(step_name) {
                    continue;
                }
                // Check if this step reads any asset produced by an affected step.
                for read_asset in &step_assets.reads {
                    let produced_by_affected = affected.iter().any(|s| {
                        self.steps
                            .get(s)
                            .is_some_and(|sa| sa.produces.contains(read_asset))
                    });
                    if produced_by_affected {
                        affected.insert(step_name.clone());
                        changed = true;
                        break;
                    }
                }
            }
        }

        // Remove the original stale steps — return only newly-affected downstream steps.
        for s in stale_steps {
            affected.remove(s);
        }

        affected.into_iter().collect()
    }

    /// Check if this graph has any asset information worth validating.
    /// Returns false if no steps have any known assets (pure v0.1 manifest).
    pub fn has_assets(&self) -> bool {
        self.steps
            .values()
            .any(|sa| !sa.produces.is_empty() || !sa.reads.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{AssetOverride, Manifest, Step};
    use std::fs;

    /// Helper: create a SQL step.
    fn sql_step(name: &str, sql: &str) -> Step {
        Step {
            name: name.to_string(),
            sql: Some(sql.to_string()),
            command: None,
            produces: vec![],
            depends_on: vec![],
            preconditions: vec![],
            op: None,
            with: None,
            output: None,
            retry: None,
            timeout_sec: None,
        }
    }

    /// Helper: create a command step with asset declarations.
    fn cmd_step_with_assets(
        name: &str,
        command: &str,
        produces: Vec<&str>,
        depends_on: Vec<&str>,
    ) -> Step {
        Step {
            name: name.to_string(),
            sql: None,
            command: Some(command.to_string()),
            produces: produces.into_iter().map(String::from).collect(),
            depends_on: depends_on.into_iter().map(String::from).collect(),
            preconditions: vec![],
            op: None,
            with: None,
            output: None,
            retry: None,
            timeout_sec: None,
        }
    }

    /// Helper: create a bare command step (no assets).
    fn cmd_step(name: &str, command: &str) -> Step {
        Step {
            name: name.to_string(),
            sql: None,
            command: Some(command.to_string()),
            produces: vec![],
            depends_on: vec![],
            preconditions: vec![],
            op: None,
            with: None,
            output: None,
            retry: None,
            timeout_sec: None,
        }
    }

    /// Helper: create an `op:` step with a typed `with:` config.
    fn op_step(name: &str, op_ref: &str, with: serde_yaml::Value) -> Step {
        Step {
            name: name.to_string(),
            sql: None,
            command: None,
            produces: vec![],
            depends_on: vec![],
            preconditions: vec![],
            op: Some(op_ref.to_string()),
            with: Some(with),
            output: None,
            retry: None,
            timeout_sec: None,
        }
    }

    /// Helper: set up a project directory with SQL files and build the graph.
    fn build_graph(
        dir: &Path,
        steps: Vec<Step>,
        assets: HashMap<String, AssetOverride>,
        sql_files: &[(&str, &str)],
    ) -> AssetGraph {
        for (path, content) in sql_files {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }

        let manifest = Manifest {
            name: "test".to_string(),
            engine: "duckdb".to_string(),
            engine_version: None,
            db: None,
            params: indexmap::IndexMap::new(),
            dotenv: Vec::new(),
            timeout_sec: None,
            defaults: None,
            hooks: crate::manifest::Hooks::default(),
            steps,
            assets,
        };

        AssetGraph::build(&manifest, dir)
    }

    // SQL steps auto-discover produced assets.
    #[test]
    fn test_sql_discovers_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("load", "models/load.sql")],
            HashMap::new(),
            &[(
                "models/load.sql",
                "CREATE TABLE customers (id INT, name TEXT);",
            )],
        );

        let step = graph.steps.get("load").unwrap();
        assert!(step.produces.contains("customers"));
        assert!(graph.warnings.is_empty());
        // A bare CREATE TABLE target carries Table, not guessed from the name later.
        assert_eq!(step.declared_kind.get("customers"), Some(&AssetKind::Table));
    }

    // SQL introspection's kind survives into `declared_kind` end to end — a
    // `COPY … PARTITION_BY` target is a directory by the time it reaches the graph,
    // not by the time something later stats the filesystem.
    #[test]
    fn test_sql_copy_partition_by_kind_reaches_declared_kind() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("export", "models/export.sql")],
            HashMap::new(),
            &[(
                "models/export.sql",
                "COPY orders TO 'build/orders' (FORMAT parquet, PARTITION_BY (year));",
            )],
        );

        let step = graph.steps.get("export").unwrap();
        assert!(step.produces.contains("build/orders"));
        assert_eq!(
            step.declared_kind.get("build/orders"),
            Some(&AssetKind::Directory)
        );
    }

    // An operator's typed config carries kind into the graph exactly as SQL
    // introspection does — `archive_extract`'s pattern-only `dest:` lands as a
    // Directory, not guessed later from whether the path has a `/` in it.
    #[test]
    fn test_op_declared_kind_reaches_declared_kind() {
        let dir = tempfile::tempdir().unwrap();
        let with: serde_yaml::Value =
            serde_yaml::from_str("archive: build/in.zip\npattern: '\\.tsv$'\ndest: build/out")
                .unwrap();
        let graph = build_graph(
            dir.path(),
            vec![op_step("extract", "archive_extract", with)],
            HashMap::new(),
            &[],
        );

        let step = graph.steps.get("extract").unwrap();
        assert!(step.produces.contains("build/out"));
        assert_eq!(
            step.declared_kind.get("build/out"),
            Some(&AssetKind::Directory)
        );
        assert_eq!(
            step.declared_kind.get("build/in.zip"),
            Some(&AssetKind::File)
        );
    }

    // An explicit `produces:` entry with no operator or parser to consult falls
    // back to `default_kind_for_declared_name`: File, unless it's a glob.
    #[test]
    fn test_explicit_produces_falls_back_to_default_kind() {
        let dir = tempfile::tempdir().unwrap();
        let mut step = cmd_step("build", "make build");
        step.produces = vec!["build/out.bin".to_string(), "build/*.tmp".to_string()];
        let graph = build_graph(dir.path(), vec![step], HashMap::new(), &[]);

        let sa = graph.steps.get("build").unwrap();
        assert_eq!(
            sa.declared_kind.get("build/out.bin"),
            Some(&AssetKind::File)
        );
        assert_eq!(
            sa.declared_kind.get("build/*.tmp"),
            Some(&AssetKind::Pattern)
        );
    }

    // SQL steps auto-discover consumed assets.
    #[test]
    fn test_sql_discovers_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("summary", "models/summary.sql")],
            HashMap::new(),
            &[(
                "models/summary.sql",
                "CREATE TABLE summary AS SELECT count(*) FROM customers JOIN orders ON customers.id = orders.cid;",
            )],
        );

        let step = graph.steps.get("summary").unwrap();
        assert!(step.produces.contains("summary"));
        assert!(step.reads.contains("customers"));
        assert!(step.reads.contains("orders"));
    }

    // INSERT INTO is recognised as an output.
    #[test]
    fn test_insert_into_output() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("append", "models/append.sql")],
            HashMap::new(),
            &[(
                "models/append.sql",
                "INSERT INTO summary SELECT count(*) FROM customers;",
            )],
        );

        let step = graph.steps.get("append").unwrap();
        assert!(step.produces.contains("summary"));
        assert!(step.reads.contains("customers"));
    }

    // Command steps with produces/depends_on are included in graph.
    #[test]
    fn test_command_step_declared_assets() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![cmd_step_with_assets(
                "export",
                "duckdb db -c \"COPY ...\"",
                vec!["customers_csv"],
                vec!["customers"],
            )],
            HashMap::new(),
            &[],
        );

        let step = graph.steps.get("export").unwrap();
        assert!(step.produces.contains("customers_csv"));
        assert!(step.reads.contains("customers"));
    }

    // Top-level assets: section overrides inferred graph.
    #[test]
    fn test_override_adds_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let mut assets = HashMap::new();
        assets.insert(
            "customers".to_string(),
            AssetOverride {
                produced_by: "load".to_string(),
                depends_on: vec!["raw_data".to_string(), "lookups".to_string()],
            },
        );

        let graph = build_graph(
            dir.path(),
            vec![sql_step("load", "models/load.sql")],
            assets,
            &[("models/load.sql", "CREATE TABLE customers (id INT);")],
        );

        let step = graph.steps.get("load").unwrap();
        assert!(step.produces.contains("customers"));
        // Override added these dependencies.
        assert!(step.reads.contains("raw_data"));
        assert!(step.reads.contains("lookups"));
    }

    // Dependency order violation is detected.
    #[test]
    fn test_dependency_order_violation() {
        let dir = tempfile::tempdir().unwrap();
        // Step order: summary runs BEFORE load-customers.
        // summary reads from customers, which load-customers creates.
        let graph = build_graph(
            dir.path(),
            vec![
                sql_step("summary", "models/summary.sql"),
                sql_step("load-customers", "models/load.sql"),
            ],
            HashMap::new(),
            &[
                (
                    "models/summary.sql",
                    "CREATE TABLE summary AS SELECT count(*) FROM customers;",
                ),
                (
                    "models/load.sql",
                    "CREATE TABLE customers (id INT, name TEXT);",
                ),
            ],
        );

        let step_order: Vec<String> = vec!["summary".into(), "load-customers".into()];
        let err = graph.validate_order(&step_order).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("summary"), "should name reader: {msg}");
        assert!(msg.contains("customers"), "should name asset: {msg}");
        assert!(
            msg.contains("load-customers"),
            "should name producer: {msg}"
        );
    }

    // Valid order passes validation.
    #[test]
    fn test_valid_order_passes() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![
                sql_step("load-customers", "models/load.sql"),
                sql_step("summary", "models/summary.sql"),
            ],
            HashMap::new(),
            &[
                (
                    "models/load.sql",
                    "CREATE TABLE customers (id INT, name TEXT);",
                ),
                (
                    "models/summary.sql",
                    "CREATE TABLE summary AS SELECT count(*) FROM customers;",
                ),
            ],
        );

        let step_order: Vec<String> = vec!["load-customers".into(), "summary".into()];
        graph.validate_order(&step_order).unwrap();
    }

    // Unparseable SQL produces a warning, step is opaque.
    #[test]
    fn test_unparseable_sql_warns() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("pivot", "models/pivot.sql")],
            HashMap::new(),
            &[("models/pivot.sql", "THIS IS NOT VALID SQL %%%")],
        );

        let step = graph.steps.get("pivot").unwrap();
        assert!(step.produces.is_empty(), "opaque step has no outputs");
        assert!(step.reads.is_empty(), "opaque step has no inputs");
        assert!(!graph.warnings.is_empty(), "should have a warning");
        assert!(
            graph.warnings[0].contains("could not parse"),
            "warning should mention parse failure: {}",
            graph.warnings[0]
        );
    }

    // Multi-step chain validates correctly.
    #[test]
    fn test_multi_step_chain() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![
                sql_step("step-a", "models/a.sql"),
                sql_step("step-b", "models/b.sql"),
                sql_step("step-c", "models/c.sql"),
            ],
            HashMap::new(),
            &[
                ("models/a.sql", "CREATE TABLE x (id INT);"),
                ("models/b.sql", "CREATE TABLE y AS SELECT * FROM x;"),
                ("models/c.sql", "CREATE TABLE z AS SELECT * FROM y;"),
            ],
        );

        // Correct order: a → b → c.
        let order: Vec<String> = vec!["step-a".into(), "step-b".into(), "step-c".into()];
        graph.validate_order(&order).unwrap();

        // Reversed B and C — C reads y before B produces it.
        let bad_order: Vec<String> = vec!["step-a".into(), "step-c".into(), "step-b".into()];
        let err = graph.validate_order(&bad_order).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("step-c"), "should name reader: {msg}");
        assert!(msg.contains("y"), "should name asset: {msg}");
    }

    // Bare command steps are opaque in the graph.
    #[test]
    fn test_opaque_command_step() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![
                sql_step("load", "models/load.sql"),
                cmd_step("notify", "echo done"),
                sql_step("summary", "models/summary.sql"),
            ],
            HashMap::new(),
            &[
                ("models/load.sql", "CREATE TABLE customers (id INT);"),
                (
                    "models/summary.sql",
                    "CREATE TABLE summary AS SELECT count(*) FROM customers;",
                ),
            ],
        );

        let notify = graph.steps.get("notify").unwrap();
        assert!(notify.produces.is_empty(), "opaque step has no outputs");
        assert!(notify.reads.is_empty(), "opaque step has no inputs");

        // The pipeline should still validate fine — the opaque step
        // doesn't participate in dependency checking.
        let order: Vec<String> = vec!["load".into(), "notify".into(), "summary".into()];
        graph.validate_order(&order).unwrap();
    }

    // Edge case: self-referencing step (reads and writes same table) is not a violation.
    #[test]
    fn test_self_reference_not_a_violation() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("update-summary", "models/update.sql")],
            HashMap::new(),
            &[(
                "models/update.sql",
                "INSERT INTO summary SELECT count(*) FROM summary WHERE date > '2026-01-01';",
            )],
        );

        let step = graph.steps.get("update-summary").unwrap();
        assert!(step.produces.contains("summary"), "should produce summary");
        assert!(step.reads.contains("summary"), "should read summary");

        // Should NOT be flagged as a violation — self-reference is fine.
        let order: Vec<String> = vec!["update-summary".into()];
        graph.validate_order(&order).unwrap();
    }

    // Empty graph (no assets at all) has_assets returns false.
    #[test]
    fn test_no_assets_graph() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![cmd_step("greet", "echo hello")],
            HashMap::new(),
            &[],
        );

        assert!(!graph.has_assets(), "bare command step has no assets");
    }

    // downstream_steps computes transitive downstream.
    #[test]
    fn test_v03_downstream_steps() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![
                sql_step("step-a", "models/a.sql"),
                sql_step("step-b", "models/b.sql"),
                sql_step("step-c", "models/c.sql"),
            ],
            HashMap::new(),
            &[
                ("models/a.sql", "CREATE TABLE x (id INT);"),
                ("models/b.sql", "CREATE TABLE y AS SELECT * FROM x;"),
                ("models/c.sql", "CREATE TABLE z AS SELECT * FROM y;"),
            ],
        );

        let downstream = graph.downstream_steps(&["step-a".into()]);
        assert!(
            downstream.contains(&"step-b".to_string()),
            "step-b depends on step-a's output"
        );
        assert!(
            downstream.contains(&"step-c".to_string()),
            "step-c transitively depends on step-a"
        );
    }

    // downstream_steps with opaque middle step — chain breaks.
    #[test]
    fn test_v03_downstream_opaque_breaks_chain() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![
                sql_step("step-a", "models/a.sql"),
                cmd_step("step-b", "echo transform"),
                sql_step("step-c", "models/c.sql"),
            ],
            HashMap::new(),
            &[
                ("models/a.sql", "CREATE TABLE x (id INT);"),
                ("models/c.sql", "CREATE TABLE z AS SELECT * FROM y;"),
            ],
        );

        // step-b is opaque (no produces/reads), so step-c doesn't transitively
        // depend on step-a through step-b.
        let downstream = graph.downstream_steps(&["step-a".into()]);
        // step-c reads 'y' which is not produced by step-a (step-a produces 'x'),
        // so step-c is NOT downstream of step-a in this graph.
        assert!(
            !downstream.contains(&"step-c".to_string()),
            "opaque middle step breaks propagation"
        );
    }

    // StepAssets gains internal and destroys, populated from SqlAssets.
    #[test]
    fn test_v03_step_assets_internal_from_cte() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("transform", "models/transform.sql")],
            HashMap::new(),
            &[(
                "models/transform.sql",
                "WITH recent AS (SELECT * FROM orders WHERE date > '2026-01-01') SELECT * FROM recent;",
            )],
        );

        let step = graph.steps.get("transform").unwrap();
        assert!(
            step.internal.contains("recent"),
            "CTE name should be in step's internal set"
        );
        assert!(
            step.reads.contains("orders"),
            "real table should be in reads"
        );
        assert!(
            !step.reads.contains("recent"),
            "CTE name should NOT be in reads"
        );
    }

    // StepAssets destroys populated from DROP TABLE.
    #[test]
    fn test_v03_step_assets_destroys_from_drop() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("cleanup", "models/cleanup.sql")],
            HashMap::new(),
            &[("models/cleanup.sql", "DROP TABLE old_data;")],
        );

        let step = graph.steps.get("cleanup").unwrap();
        assert!(
            step.destroys.contains("old_data"),
            "dropped table should be in step's destroys set"
        );
    }

    // validate_order ignores internal — CTE names don't cause false violations.
    #[test]
    fn test_v03_cte_name_no_false_violation() {
        let dir = tempfile::tempdir().unwrap();
        // step-A creates table `recent`, step-B uses WITH recent AS (...) which shadows the name.
        // validate_order should NOT flag this as a dependency violation.
        let graph = build_graph(
            dir.path(),
            vec![
                sql_step("step-b", "models/b.sql"),
                sql_step("step-a", "models/a.sql"),
            ],
            HashMap::new(),
            &[
                (
                    "models/b.sql",
                    "WITH recent AS (SELECT * FROM raw_data) CREATE TABLE summary AS SELECT * FROM recent;",
                ),
                ("models/a.sql", "CREATE TABLE recent (id INT);"),
            ],
        );

        // step-B runs before step-A. step-B's SQL has a CTE named 'recent' —
        // this should NOT trigger a violation even though step-A creates a table named 'recent'.
        let order: Vec<String> = vec!["step-b".into(), "step-a".into()];
        graph.validate_order(&order).unwrap();
    }

    // Round 6, ground 1's second requirement: no test anywhere in this repo loaded a
    // real shipping manifest through the case-collision gate before this one, which
    // is exactly why the gate could refuse one (the real edgar_gleif manifest — see
    // `validate_no_case_collisions`'s doc for the eight-collision shape it used to
    // trip) and nothing here would have gone red. Byte-identical copies of all four
    // manifests `open-analytics` ships, vendored under `tests/fixtures/
    // open_analytics/` (both repos are public) so this is self-contained on CI, which
    // checks out only this repo. A future manifest edit — or a future narrowing of
    // this gate — that starts refusing a shape one of these four actually uses
    // reddens here.
    #[test]
    fn all_open_analytics_manifests_load_and_pass_the_case_collision_gate() {
        // (dataset name, arcform.yaml content, [(relative sql path, sql content)])
        type Fixture<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);
        let fixtures: &[Fixture] = &[
            (
                "gleif",
                include_str!("../tests/fixtures/open_analytics/gleif/arcform.yaml"),
                &[
                    (
                        "models/load.sql",
                        include_str!("../tests/fixtures/open_analytics/gleif/models/load.sql"),
                    ),
                    (
                        "models/package.sql",
                        include_str!("../tests/fixtures/open_analytics/gleif/models/package.sql"),
                    ),
                ],
            ),
            (
                "naics",
                include_str!("../tests/fixtures/open_analytics/naics/arcform.yaml"),
                &[
                    (
                        "models/load.sql",
                        include_str!("../tests/fixtures/open_analytics/naics/models/load.sql"),
                    ),
                    (
                        "models/package.sql",
                        include_str!("../tests/fixtures/open_analytics/naics/models/package.sql"),
                    ),
                ],
            ),
            (
                "edgar",
                include_str!("../tests/fixtures/open_analytics/edgar/arcform.yaml"),
                &[
                    (
                        "models/load.sql",
                        include_str!("../tests/fixtures/open_analytics/edgar/models/load.sql"),
                    ),
                    (
                        "models/package.sql",
                        include_str!("../tests/fixtures/open_analytics/edgar/models/package.sql"),
                    ),
                ],
            ),
            (
                "edgar_gleif",
                include_str!("../tests/fixtures/open_analytics/edgar_gleif/arcform.yaml"),
                &[
                    (
                        "models/sec_entities.sql",
                        include_str!(
                            "../tests/fixtures/open_analytics/edgar_gleif/models/sec_entities.sql"
                        ),
                    ),
                    (
                        "models/load.sql",
                        include_str!(
                            "../tests/fixtures/open_analytics/edgar_gleif/models/load.sql"
                        ),
                    ),
                    (
                        "models/tier.sql",
                        include_str!(
                            "../tests/fixtures/open_analytics/edgar_gleif/models/tier.sql"
                        ),
                    ),
                    (
                        "models/package.sql",
                        include_str!(
                            "../tests/fixtures/open_analytics/edgar_gleif/models/package.sql"
                        ),
                    ),
                ],
            ),
        ];

        for (name, yaml, sql_files) in fixtures {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
            for (path, content) in *sql_files {
                let full = dir.path().join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(full, content).unwrap();
            }

            let manifest = Manifest::load(dir.path())
                .unwrap_or_else(|e| panic!("{name}: manifest failed to load: {e}"));
            let graph = AssetGraph::build(&manifest, dir.path());
            assert!(
                graph.warnings.is_empty(),
                "{name}: unexpected asset-graph warnings (a step likely became opaque \
                 because a referenced SQL file did not vendor cleanly): {:?}",
                graph.warnings
            );
            graph.validate_no_case_collisions().unwrap_or_else(|e| {
                panic!("{name}: case-collision gate refused a real shipping manifest: {e}")
            });
        }
    }

    #[test]
    fn all_open_analytics_manifests_have_not_drifted_from_their_vendored_checksums() {
        let fixtures: &[(&str, &str, &str)] = &[
            (
                "gleif/arcform.yaml",
                include_str!("../tests/fixtures/open_analytics/gleif/arcform.yaml"),
                "b9560912b49511e0f5d113406acf8e298811200ee6f55536149cfd90dfbbdbeb",
            ),
            (
                "gleif/models/load.sql",
                include_str!("../tests/fixtures/open_analytics/gleif/models/load.sql"),
                "2f454ada74311a92c430c0bc355d0db79ff8b62b3bd3af51b8b294757b4450a6",
            ),
            (
                "gleif/models/package.sql",
                include_str!("../tests/fixtures/open_analytics/gleif/models/package.sql"),
                "359276a8bb87c39636ae74703815b00b28e7da2be506c9da1526379beabef4f4",
            ),
            (
                "naics/arcform.yaml",
                include_str!("../tests/fixtures/open_analytics/naics/arcform.yaml"),
                "3a9a3c870ce7e485ecb0d6d94c80e16bb33bc71bfcbeabe78677b34f55caceba",
            ),
            (
                "naics/models/load.sql",
                include_str!("../tests/fixtures/open_analytics/naics/models/load.sql"),
                "a1b258a5d58739d609a871b09eae6d341ba71ae8a40fc370e7df6db7805d6019",
            ),
            (
                "naics/models/package.sql",
                include_str!("../tests/fixtures/open_analytics/naics/models/package.sql"),
                "24181e6342dcd3c571756faad6245953dc3bf47fa644735f981267f480ab987e",
            ),
            (
                "edgar/arcform.yaml",
                include_str!("../tests/fixtures/open_analytics/edgar/arcform.yaml"),
                "3dc51d55313c71df8562808eede1bbc65556b1c88be4a9159f050d02a6411439",
            ),
            (
                "edgar/models/load.sql",
                include_str!("../tests/fixtures/open_analytics/edgar/models/load.sql"),
                "f472f769efb93e815020eb4062f283bb3aaf2f1680d4493c29ff1885f8999fe7",
            ),
            (
                "edgar/models/package.sql",
                include_str!("../tests/fixtures/open_analytics/edgar/models/package.sql"),
                "b5fff7c311ac52e5c74d4602ae230af79c37f15f7d1e6925efac143070069975",
            ),
            (
                "edgar_gleif/arcform.yaml",
                include_str!("../tests/fixtures/open_analytics/edgar_gleif/arcform.yaml"),
                "934e691a91868aad6ced19a66dd66b868fec7cd9759fc51c437f7adde5614c00",
            ),
            (
                "edgar_gleif/models/sec_entities.sql",
                include_str!(
                    "../tests/fixtures/open_analytics/edgar_gleif/models/sec_entities.sql"
                ),
                "296223049072b164383aa6aa8278b1adfb98cb3ac63b178eaf9a5b4fe1f39efe",
            ),
            (
                "edgar_gleif/models/load.sql",
                include_str!("../tests/fixtures/open_analytics/edgar_gleif/models/load.sql"),
                "f7a7281129a753919c64d443dafd95140e98fa34fe2f297b5c090b451ccf1240",
            ),
            (
                "edgar_gleif/models/tier.sql",
                include_str!("../tests/fixtures/open_analytics/edgar_gleif/models/tier.sql"),
                "155534dbe397777c58caef91992dc6aeb0d244d56a37bab9bdea907010db7dda",
            ),
            (
                "edgar_gleif/models/package.sql",
                include_str!("../tests/fixtures/open_analytics/edgar_gleif/models/package.sql"),
                "1806cbe9d1a0af36376b98d599cc73b2fa5008e467a90ee0ea6df18d75143097",
            ),
        ];

        for (path, content, expected_sha256) in fixtures {
            let actual = crate::state::content_hash(content.as_bytes());
            assert_eq!(
                &actual, expected_sha256,
                "{path}: vendored fixture content does not match its recorded SHA-256 — \
                 either the copy was edited directly (revert it and re-sync from \
                 open-analytics deliberately) or this assertion needs updating as part \
                 of a documented re-sync (see SOURCE.md)"
            );
        }
    }
}
