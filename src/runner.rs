use std::path::Path;

use owo_colors::OwoColorize;

use crate::asset::AssetGraph;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::state::{self, StateBackend, StepStatus};

/// Run a pipeline: load manifest, preflight, validate assets, execute steps
/// with staleness-aware selective execution.
pub fn run(dir: &Path, engine: &dyn Engine, state: &dyn StateBackend, force: bool) -> Result<()> {
    let manifest = Manifest::load(dir)?;

    // If there are SQL steps, verify the engine is available and check version.
    if manifest.has_sql_steps() {
        let info = engine.preflight()?;

        // Check engine version constraint if specified.
        if let Some(ref constraint_str) = manifest.engine_version {
            if let Ok(req) = semver::VersionReq::parse(constraint_str) {
                match &info.version {
                    Some(ver) => {
                        if !req.matches(ver) {
                            return Err(Error::VersionMismatch {
                                required: constraint_str.clone(),
                                found: ver.to_string(),
                            });
                        }
                    }
                    None => {
                        // Version unparseable — warn but don't block.
                        eprintln!(
                            "{} could not detect engine version — skipping version check (requires {})",
                            "warning:".yellow(),
                            constraint_str,
                        );
                    }
                }
            }
            // If constraint_str is invalid, manifest validation already caught it.
        }
    }

    if manifest.steps.is_empty() {
        println!("{}", "No steps defined.".dimmed());
        return Ok(());
    }

    // Initialise the state backend (creates tables if needed).
    state.init()?;

    // Start a new run record.
    let run_id = state.start_run()?;

    // Build the asset graph and validate dependency ordering.
    let asset_graph = AssetGraph::build(&manifest, dir);

    // Print any warnings from asset discovery (e.g. unparseable SQL).
    for warning in &asset_graph.warnings {
        eprintln!("{} {}", "warning:".yellow(), warning);
    }

    // If the graph has assets, validate step ordering against dependencies.
    if asset_graph.has_assets() {
        let step_order: Vec<String> = manifest.steps.iter().map(|s| s.name.clone()).collect();
        asset_graph.validate_order(&step_order)?;
    }

    // Determine which steps are stale.
    let stale_steps = compute_staleness(&manifest, dir, state, &asset_graph, force)?;

    let db_path = manifest.db_path(dir);
    let total = manifest.steps.len();
    let mut succeeded = 0;
    let mut executed = 0;
    let mut skipped = 0;

    for (i, step) in manifest.steps.iter().enumerate() {
        let is_stale = stale_steps.contains(&step.name);

        if !is_stale {
            println!(
                "[{}/{}] {} {}",
                i + 1,
                total,
                step.name.bold(),
                "[skip]".dimmed(),
            );
            skipped += 1;
            continue;
        }

        println!(
            "[{}/{}] {} ...",
            i + 1,
            total,
            step.name.bold()
        );

        // Compute the SQL hash for this step (for state recording).
        let sql_hash = if let Some(ref sql) = step.sql {
            let sql_path = dir.join(sql);
            if !sql_path.exists() {
                return Err(Error::SqlFileNotFound {
                    step: step.name.clone(),
                    path: sql_path,
                });
            }
            let content = std::fs::read(&sql_path).map_err(|e| Error::FileRead {
                path: sql_path.clone(),
                source: e,
            })?;
            state::content_hash(&content)
        } else {
            String::new()
        };

        let result = if let Some(ref sql) = step.sql {
            let sql_path = dir.join(sql);
            engine.execute_sql(&db_path, &sql_path)
        } else if let Some(ref command) = step.command {
            engine.execute_command(command)
        } else {
            unreachable!("validation ensures sql or command is present")
        };

        match result {
            Ok(_output) => {
                succeeded += 1;
                executed += 1;
                // Record success.
                let _ = state.record_step(&step.name, &sql_hash, StepStatus::Success);
            }
            Err(Error::StepFailed { code, stderr, .. }) => {
                // Record failure before returning error.
                let _ = state.record_step(&step.name, &sql_hash, StepStatus::Failed);
                let _ = state.finish_run(&run_id, executed, "failed");
                return Err(Error::StepFailed {
                    step: step.name.clone(),
                    code,
                    stderr,
                });
            }
            Err(e) => {
                let _ = state.finish_run(&run_id, executed, "error");
                return Err(e);
            }
        }
    }

    // Finish the run record.
    let _ = state.finish_run(&run_id, executed, "success");

    if skipped > 0 {
        println!(
            "\n{} {}/{} steps succeeded, {} skipped (fresh).",
            "✓".green(),
            succeeded,
            total,
            skipped,
        );
    } else {
        println!(
            "\n{} {}/{} steps succeeded.",
            "✓".green(),
            succeeded,
            total,
        );
    }

    Ok(())
}

/// Determine which steps are stale and need to execute.
///
/// A step is stale if:
/// - `force` is true (all steps run)
/// - It's a command step (always re-runs)
/// - It has no prior state (first run)
/// - Its prior run failed
/// - Its SQL file content hash changed
/// - An upstream step (via asset graph) is stale (downstream propagation)
fn compute_staleness(
    manifest: &Manifest,
    dir: &Path,
    state: &dyn StateBackend,
    asset_graph: &AssetGraph,
    force: bool,
) -> Result<std::collections::HashSet<String>> {
    let mut stale: std::collections::HashSet<String> = std::collections::HashSet::new();

    if force {
        // Force mode: everything is stale.
        for step in &manifest.steps {
            stale.insert(step.name.clone());
        }
        return Ok(stale);
    }

    // Phase 1: Check each step's own staleness.
    for step in &manifest.steps {
        if step.command.is_some() {
            // Command steps always re-run.
            stale.insert(step.name.clone());
            continue;
        }

        let prior = state.get_step_state(&step.name)?;

        match prior {
            None => {
                // Never run before — stale.
                stale.insert(step.name.clone());
            }
            Some(prior_state) => {
                if prior_state.status == StepStatus::Failed {
                    // Previously failed — always stale.
                    stale.insert(step.name.clone());
                    continue;
                }

                // Check SQL content hash.
                if let Some(ref sql) = step.sql {
                    let sql_path = dir.join(sql);
                    if sql_path.exists() {
                        let content = std::fs::read(&sql_path).map_err(|e| Error::FileRead {
                            path: sql_path.clone(),
                            source: e,
                        })?;
                        let current_hash = state::content_hash(&content);
                        if current_hash != prior_state.sql_hash {
                            stale.insert(step.name.clone());
                        }
                    } else {
                        // SQL file doesn't exist — will error during execution.
                        stale.insert(step.name.clone());
                    }
                }
            }
        }
    }

    // Phase 2: Downstream propagation.
    let directly_stale: Vec<String> = stale.iter().cloned().collect();
    let downstream = asset_graph.downstream_steps(&directly_stale);
    for step_name in downstream {
        stale.insert(step_name);
    }

    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mock::{MockCall, MockEngine};
    use crate::state::mock::MockStateBackend;
    use std::fs;

    fn setup_project(dir: &Path, yaml: &str, files: &[(&str, &str)]) {
        fs::write(dir.join("arcform.yaml"), yaml).unwrap();
        for (path, content) in files {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(full, content).unwrap();
        }
    }

    // AC-8: Empty steps list exits successfully.
    #[test]
    fn test_run_empty_steps() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path(), "name: test\nsteps: []\n", &[]);
        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();
        // No preflight called for empty steps.
        assert!(engine.calls.borrow().is_empty());
    }

    // AC-3: Steps execute in declared order against shared database.
    #[test]
    fn test_run_sql_steps_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n  - name: s2\n    sql: models/s2.sql\n  - name: s3\n    sql: models/s3.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "CREATE TABLE t(v TEXT);"),
                ("models/s2.sql", "INSERT INTO t VALUES ('b');"),
                ("models/s3.sql", "INSERT INTO t VALUES ('c');"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 4); // 1 preflight + 3 sql
        assert!(matches!(calls[0], MockCall::Preflight));

        // Verify execution order.
        let sql_calls: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::Sql { sql_content, .. } => Some(sql_content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            sql_calls,
            vec![
                "CREATE TABLE t(v TEXT);",
                "INSERT INTO t VALUES ('b');",
                "INSERT INTO t VALUES ('c');",
            ]
        );
    }

    // AC-9: Command steps execute via sh -c, preflight skipped for command-only.
    #[test]
    fn test_run_command_step() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // No preflight (no sql steps), 1 command.
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0], MockCall::Command { command } if command == "echo hello"));
    }

    // AC-4: Halt on failure — steps after a failed step do not execute.
    #[test]
    fn test_run_halts_on_step2_failure_step3_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n  - name: s2\n    sql: models/s2.sql\n  - name: s3\n    sql: models/s3.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "SELECT 1;"),
                ("models/s2.sql", "INVALID SQL;"),
                ("models/s3.sql", "SELECT 3;"),
            ],
        );

        let engine = MockEngine::new();
        engine.set_fail_on_call(1, 1, "syntax error");
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("s2"), "error should name step 's2': {err_msg}");
    }

    // AC-5: Missing SQL file produces a specific error.
    #[test]
    fn test_run_missing_sql_file() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/missing.sql\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        assert!(err.to_string().contains("sql file not found"));
    }

    // AC-7: SQL files passed to engine byte-identical.
    #[test]
    fn test_sql_content_passed_unmodified() {
        let dir = tempfile::tempdir().unwrap();
        let original_sql = "SELECT 1;\n-- comment with special chars: émojis 🎉\n";
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", original_sql)]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_content = match &calls[1] {
            MockCall::Sql { sql_content, .. } => sql_content.as_str(),
            _ => panic!("expected Sql call"),
        };
        assert_eq!(sql_content, original_sql);
    }

    // AC-6: Preflight failure blocks execution — no steps run.
    #[test]
    fn test_ac06_preflight_failure_blocks_execution() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_preflight_failure();
        let state = MockStateBackend::new();

        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report engine not found: {err}"
        );
    }

    // AC-9: Failing command step exits non-zero and halts pipeline.
    #[test]
    fn test_ac09_command_step_failure_halts_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: fetch\n    command: curl http://example.com\n  - name: transform\n    command: echo done\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "connection refused");
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("fetch"),
            "error should name step 'fetch': {err_msg}"
        );
    }

    // v0.2 AC-06: `arc run` halts with dependency order violation before executing.
    #[test]
    fn test_v02_ac06_dependency_order_blocks_execution() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: summary\n    sql: models/summary.sql\n  - name: load\n    sql: models/load.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                (
                    "models/summary.sql",
                    "CREATE TABLE summary AS SELECT count(*) FROM customers;",
                ),
                ("models/load.sql", "CREATE TABLE customers (id INT);"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("summary"),
            "error should name reader 'summary': {err_msg}"
        );
        assert!(
            err_msg.contains("customers"),
            "error should name asset 'customers': {err_msg}"
        );
    }

    // v0.2 AC-08: v0.1-style manifest (no assets) runs identically.
    #[test]
    fn test_v02_ac08_v1_manifest_runs_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n  - name: done\n    command: echo done\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 2);
    }

    // v0.2 AC-07: Unparseable SQL warns but still executes.
    #[test]
    fn test_v02_ac07_unparseable_sql_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: weird\n    sql: models/weird.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[("models/weird.sql", "THIS IS NOT VALID SQL %%%")],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 2); // preflight + 1 SQL
    }

    // v0.2 AC-09: Multi-step chain with valid ordering succeeds.
    #[test]
    fn test_v02_ac09_valid_chain_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: step-a\n    sql: models/a.sql\n  - name: step-b\n    sql: models/b.sql\n  - name: step-c\n    sql: models/c.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/a.sql", "CREATE TABLE x (id INT);"),
                ("models/b.sql", "CREATE TABLE y AS SELECT * FROM x;"),
                ("models/c.sql", "CREATE TABLE z AS SELECT * FROM y;"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 4); // preflight + 3 SQL
    }

    // ---- v0.3 Staleness Tests ----

    // v0.3 AC-04: Fresh SQL step is skipped on second run.
    #[test]
    fn test_v03_ac04_fresh_step_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        let sql = "CREATE TABLE t(v TEXT);";
        setup_project(dir.path(), yaml, &[("models/s1.sql", sql)]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — step executes.
        run(dir.path(), &engine, &state, false).unwrap();
        let calls_after_first = engine.calls.borrow().len();
        assert_eq!(calls_after_first, 2); // preflight + 1 sql

        // Second run — step should be skipped (hash unchanged).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        // Only preflight, no SQL execution.
        assert_eq!(calls.len(), 1, "fresh step should be skipped on second run");
        assert!(matches!(calls[0], MockCall::Preflight));
    }

    // v0.3 AC-05: Stale SQL step re-runs after edit.
    #[test]
    fn test_v03_ac05_stale_step_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Edit the SQL file.
        fs::write(dir.path().join("models/s1.sql"), "SELECT 2;").unwrap();

        // Second run — step should re-execute (hash changed).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 2, "stale step should re-run: preflight + 1 sql");
    }

    // v0.3 AC-06: Downstream propagation — stale upstream makes dependents stale.
    #[test]
    fn test_v03_ac06_downstream_propagation() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: step-a\n    sql: models/a.sql\n  - name: step-b\n    sql: models/b.sql\n  - name: step-c\n    sql: models/c.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/a.sql", "CREATE TABLE x (id INT);"),
                ("models/b.sql", "CREATE TABLE y AS SELECT * FROM x;"),
                ("models/c.sql", "CREATE TABLE z AS SELECT * FROM y;"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — all execute.
        run(dir.path(), &engine, &state, false).unwrap();

        // Edit only step-a's SQL.
        fs::write(dir.path().join("models/a.sql"), "CREATE TABLE x (id INT, name TEXT);").unwrap();

        // Second run — all three should re-run (a is stale, b and c are downstream).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(sql_calls.len(), 3, "all 3 steps should re-run due to downstream propagation");
    }

    // v0.3 AC-08: Failed step always re-runs.
    #[test]
    fn test_v03_ac08_failed_step_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_fail_on_call(0, 1, "error");
        let state = MockStateBackend::new();

        // First run — fails.
        let _ = run(dir.path(), &engine, &state, false);

        // Verify state records failure.
        let step_state = state.get_step_state("s1").unwrap().unwrap();
        assert_eq!(step_state.status, StepStatus::Failed);

        // Second run — should re-execute (failed = always stale).
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(sql_calls.len(), 1, "failed step should re-run");
    }

    // v0.3 AC-09: Command steps always re-run.
    #[test]
    fn test_v03_ac09_command_always_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: greet\n    command: echo hello\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run — command should still execute.
        drop(engine);
        let engine = MockEngine::new();
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 1, "command step should always re-run");
    }

    // v0.3 AC-10: --force runs all steps regardless of staleness.
    #[test]
    fn test_v03_ac10_force_runs_all() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run.
        run(dir.path(), &engine, &state, false).unwrap();

        // Second run with --force — should execute even though fresh.
        {
            let engine = MockEngine::new();
            run(dir.path(), &engine, &state, true).unwrap();

            let calls = engine.calls.borrow();
            let sql_calls: Vec<_> = calls
                .iter()
                .filter(|c| matches!(c, MockCall::Sql { .. }))
                .collect();
            assert_eq!(sql_calls.len(), 1, "--force should run fresh step");
        }

        // Third run without --force — should skip (--force recorded new state).
        {
            let engine = MockEngine::new();
            run(dir.path(), &engine, &state, false).unwrap();

            let calls = engine.calls.borrow();
            let sql_calls: Vec<_> = calls
                .iter()
                .filter(|c| matches!(c, MockCall::Sql { .. }))
                .collect();
            assert_eq!(sql_calls.len(), 0, "after --force, step should be fresh");
        }
    }

    // v0.3 AC-11: First run treats all steps as stale.
    #[test]
    fn test_v03_ac11_first_run_all_stale() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n  - name: s2\n    sql: models/s2.sql\n";
        setup_project(
            dir.path(),
            yaml,
            &[
                ("models/s1.sql", "SELECT 1;"),
                ("models/s2.sql", "SELECT 2;"),
            ],
        );

        let engine = MockEngine::new();
        let state = MockStateBackend::new();

        // First run — no prior state, all should execute.
        run(dir.path(), &engine, &state, false).unwrap();

        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(sql_calls.len(), 2, "first run should execute all steps");
    }

    // ---- Local-Remote Parity Tests ----

    // lrp ac-05: Version mismatch blocks execution before any step runs.
    #[test]
    fn test_lrp_ac05_version_mismatch_blocks_execution() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine_version: '>=2.0'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        // MockEngine defaults to v2.0.0, set it to 1.3.0 to trigger mismatch.
        engine.set_version(Some(semver::Version::new(1, 3, 0)));
        let state = MockStateBackend::new();

        let result = run(dir.path(), &engine, &state, false);
        assert!(result.is_err(), "should fail due to version mismatch");

        // Verify no steps executed — only preflight was called.
        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 1, "only preflight should be called");
        assert!(matches!(calls[0], MockCall::Preflight));
    }

    // lrp ac-06: Version mismatch error contains both required and found versions.
    #[test]
    fn test_lrp_ac06_error_contains_both_versions() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine_version: '>=2.0'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_version(Some(semver::Version::new(1, 3, 0)));
        let state = MockStateBackend::new();

        let err = run(dir.path(), &engine, &state, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(">=2.0"), "error should contain constraint: {msg}");
        assert!(msg.contains("1.3.0"), "error should contain detected version: {msg}");
    }

    // lrp ac-07: No engine_version skips the version check.
    #[test]
    fn test_lrp_ac07_no_version_constraint_skips_check() {
        let dir = tempfile::tempdir().unwrap();
        // No engine_version in YAML — should skip version check.
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        // Even with a very old version, no constraint means no check.
        engine.set_version(Some(semver::Version::new(0, 1, 0)));
        let state = MockStateBackend::new();

        // Should succeed — no version comparison.
        run(dir.path(), &engine, &state, false).unwrap();
    }

    // lrp ac-05: Version that satisfies constraint passes.
    #[test]
    fn test_lrp_ac05_version_satisfies_constraint() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine_version: '>=1.5'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        engine.set_version(Some(semver::Version::new(1, 5, 2)));
        let state = MockStateBackend::new();

        // Should succeed — 1.5.2 >= 1.5.
        run(dir.path(), &engine, &state, false).unwrap();
    }

    // lrp ac-12: Unparseable version warns but pipeline continues.
    #[test]
    fn test_lrp_ac12_unparseable_version_warns_continues() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine_version: '>=1.5'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        setup_project(dir.path(), yaml, &[("models/s1.sql", "SELECT 1;")]);

        let engine = MockEngine::new();
        // Set version to None (simulating unparseable output).
        engine.set_version(None);
        let state = MockStateBackend::new();

        // Should succeed — unparseable version skips check.
        run(dir.path(), &engine, &state, false).unwrap();

        // Verify step actually executed.
        let calls = engine.calls.borrow();
        let sql_calls: Vec<_> = calls
            .iter()
            .filter(|c| matches!(c, MockCall::Sql { .. }))
            .collect();
        assert_eq!(sql_calls.len(), 1, "step should execute despite unparseable version");
    }
}
