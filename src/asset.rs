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

use crate::error::{Error, Result};
use crate::introspect;
use crate::manifest::Manifest;

/// The assets associated with a single step.
#[derive(Debug, Clone, Default)]
pub struct StepAssets {
    /// Asset names this step produces (creates/writes/modifies).
    pub produces: BTreeSet<String>,
    /// Asset names this step reads data from (external dependencies).
    pub reads: BTreeSet<String>,
    /// CTE names — step-internal assets visible in lineage but not cross-step dependencies.
    pub internal: BTreeSet<String>,
    /// Asset names this step destroys (DROP operations).
    pub destroys: BTreeSet<String>,
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

            // Phase 1: SQL introspection.
            if let Some(ref sql_path) = step.sql {
                let full_path = manifest_dir.join(sql_path);
                match std::fs::read_to_string(&full_path) {
                    Ok(sql_content) => match introspect::extract_assets(&sql_content) {
                        Ok(sql_assets) => {
                            step_assets.produces.extend(sql_assets.outputs);
                            step_assets.reads.extend(sql_assets.inputs);
                            step_assets.internal.extend(sql_assets.internal);
                            step_assets.destroys.extend(sql_assets.destroys);
                        }
                        Err(warnings) => {
                            // AC-07: Warn on parse failure, treat as opaque.
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

            // Phase 2: Explicit declarations (primarily for command steps).
            for asset in &step.produces {
                step_assets.produces.insert(asset.to_lowercase());
            }
            for asset in &step.depends_on {
                step_assets.reads.insert(asset.to_lowercase());
            }

            graph.steps.insert(step.name.clone(), step_assets);
        }

        // Phase 3: Apply overrides from the top-level assets: section.
        for (asset_name, override_entry) in &manifest.assets {
            let name = asset_name.to_lowercase();
            if let Some(step_assets) = graph.steps.get_mut(&override_entry.produced_by) {
                // Override: ensure this step produces the asset.
                step_assets.produces.insert(name.clone());

                // Add override dependencies as reads for the producing step.
                for dep in &override_entry.depends_on {
                    step_assets.reads.insert(dep.to_lowercase());
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
            steps,
            assets,
        };

        AssetGraph::build(&manifest, dir)
    }

    // AC-01: SQL steps auto-discover produced assets.
    #[test]
    fn test_ac01_sql_discovers_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("load", "models/load.sql")],
            HashMap::new(),
            &[("models/load.sql", "CREATE TABLE customers (id INT, name TEXT);")],
        );

        let step = graph.steps.get("load").unwrap();
        assert!(step.produces.contains("customers"));
        assert!(graph.warnings.is_empty());
    }

    // AC-02: SQL steps auto-discover consumed assets.
    #[test]
    fn test_ac02_sql_discovers_inputs() {
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

    // AC-03: INSERT INTO is recognised as an output.
    #[test]
    fn test_ac03_insert_into_output() {
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

    // AC-04: Command steps with produces/depends_on are included in graph.
    #[test]
    fn test_ac04_command_step_declared_assets() {
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

    // AC-05: Top-level assets: section overrides inferred graph.
    #[test]
    fn test_ac05_override_adds_dependency() {
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

    // AC-06: Dependency order violation is detected.
    #[test]
    fn test_ac06_dependency_order_violation() {
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
        assert!(msg.contains("load-customers"), "should name producer: {msg}");
    }

    // AC-06: Valid order passes validation.
    #[test]
    fn test_ac06_valid_order_passes() {
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

    // AC-07: Unparseable SQL produces a warning, step is opaque.
    #[test]
    fn test_ac07_unparseable_sql_warns() {
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

    // AC-09: Multi-step chain validates correctly.
    #[test]
    fn test_ac09_multi_step_chain() {
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
                (
                    "models/b.sql",
                    "CREATE TABLE y AS SELECT * FROM x;",
                ),
                (
                    "models/c.sql",
                    "CREATE TABLE z AS SELECT * FROM y;",
                ),
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

    // AC-10: Bare command steps are opaque in the graph.
    #[test]
    fn test_ac10_opaque_command_step() {
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

    // AC-08: Empty graph (no assets at all) has_assets returns false.
    #[test]
    fn test_ac08_no_assets_graph() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![cmd_step("greet", "echo hello")],
            HashMap::new(),
            &[],
        );

        assert!(!graph.has_assets(), "bare command step has no assets");
    }

    // v0.3 AC-07: downstream_steps computes transitive downstream.
    #[test]
    fn test_v03_ac07_downstream_steps() {
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
        assert!(downstream.contains(&"step-b".to_string()), "step-b depends on step-a's output");
        assert!(downstream.contains(&"step-c".to_string()), "step-c transitively depends on step-a");
    }

    // v0.3 AC-07: downstream_steps with opaque middle step — chain breaks.
    #[test]
    fn test_v03_ac07_downstream_opaque_breaks_chain() {
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
        assert!(!downstream.contains(&"step-c".to_string()), "opaque middle step breaks propagation");
    }

    // v0.3 AC-10: StepAssets gains internal and destroys, populated from SqlAssets.
    #[test]
    fn test_v03_ac10_step_assets_internal_from_cte() {
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
        assert!(step.internal.contains("recent"), "CTE name should be in step's internal set");
        assert!(step.reads.contains("orders"), "real table should be in reads");
        assert!(!step.reads.contains("recent"), "CTE name should NOT be in reads");
    }

    // v0.3 AC-10: StepAssets destroys populated from DROP TABLE.
    #[test]
    fn test_v03_ac10_step_assets_destroys_from_drop() {
        let dir = tempfile::tempdir().unwrap();
        let graph = build_graph(
            dir.path(),
            vec![sql_step("cleanup", "models/cleanup.sql")],
            HashMap::new(),
            &[("models/cleanup.sql", "DROP TABLE old_data;")],
        );

        let step = graph.steps.get("cleanup").unwrap();
        assert!(step.destroys.contains("old_data"), "dropped table should be in step's destroys set");
    }

    // v0.3 AC-11: validate_order ignores internal — CTE names don't cause false violations.
    #[test]
    fn test_v03_ac11_cte_name_no_false_violation() {
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
}
