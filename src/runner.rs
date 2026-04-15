use std::path::Path;

use owo_colors::OwoColorize;

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::manifest::Manifest;

/// Run a pipeline: load manifest, preflight, execute steps sequentially.
pub fn run(dir: &Path, engine: &dyn Engine) -> Result<()> {
    let manifest = Manifest::load(dir)?;

    // If there are SQL steps, verify the engine is available.
    if manifest.has_sql_steps() {
        engine.preflight()?;
    }

    if manifest.steps.is_empty() {
        println!("{}", "No steps defined.".dimmed());
        return Ok(());
    }

    let db_path = manifest.db_path(dir);
    let total = manifest.steps.len();
    let mut succeeded = 0;

    for (i, step) in manifest.steps.iter().enumerate() {
        println!(
            "[{}/{}] {} ...",
            i + 1,
            total,
            step.name.bold()
        );

        let result = if let Some(ref sql) = step.sql {
            let sql_path = dir.join(sql);
            if !sql_path.exists() {
                return Err(Error::SqlFileNotFound {
                    step: step.name.clone(),
                    path: sql_path,
                });
            }
            engine.execute_sql(&db_path, &sql_path)
        } else if let Some(ref command) = step.command {
            engine.execute_command(command)
        } else {
            unreachable!("validation ensures sql or command is present")
        };

        match result {
            Ok(_output) => {
                succeeded += 1;
            }
            Err(Error::StepFailed { code, stderr, .. }) => {
                return Err(Error::StepFailed {
                    step: step.name.clone(),
                    code,
                    stderr,
                });
            }
            Err(e) => {
                return Err(e);
            }
        }
    }

    println!(
        "\n{} {}/{} steps succeeded.",
        "✓".green(),
        succeeded,
        total,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::mock::{MockCall, MockEngine};
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
        run(dir.path(), &engine).unwrap();
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
        run(dir.path(), &engine).unwrap();

        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 4); // 1 preflight + 3 sql
        assert!(matches!(calls[0], MockCall::Preflight));

        // Verify order and content.
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
        run(dir.path(), &engine).unwrap();

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
        // Fail on the 2nd execution call (0-indexed: call 1 = step 2).
        engine.set_fail_on_call(1, 1, "syntax error");

        let result = run(dir.path(), &engine);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("s2"), "error should name step 's2': {err_msg}");

        let calls = engine.calls.borrow();
        let exec_calls: Vec<_> = calls
            .iter()
            .filter(|c| !matches!(c, MockCall::Preflight))
            .collect();
        // Step 1 ran, step 2 ran (and failed), step 3 never ran.
        assert_eq!(exec_calls.len(), 2, "expected 2 execution calls (s1 + s2), got {}", exec_calls.len());
    }

    // AC-5: Missing SQL file produces a specific error.
    #[test]
    fn test_run_missing_sql_file() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/missing.sql\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        let err = run(dir.path(), &engine).unwrap_err();
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
        run(dir.path(), &engine).unwrap();

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

        let err = run(dir.path(), &engine).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "should report engine not found: {err}"
        );

        // Only preflight was called — no execution calls.
        let calls = engine.calls.borrow();
        assert_eq!(calls.len(), 1, "only preflight should be called");
        assert!(matches!(calls[0], MockCall::Preflight));
    }

    // AC-9: Failing command step exits non-zero and halts pipeline.
    #[test]
    fn test_ac09_command_step_failure_halts_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: fetch\n    command: curl http://example.com\n  - name: transform\n    command: echo done\n";
        setup_project(dir.path(), yaml, &[]);

        let engine = MockEngine::new();
        // Fail on the first command execution.
        engine.set_fail_on_call(0, 1, "connection refused");

        let result = run(dir.path(), &engine);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("fetch"),
            "error should name step 'fetch': {err_msg}"
        );

        // Only 1 command call — second step never ran.
        let calls = engine.calls.borrow();
        let exec_calls: Vec<_> = calls
            .iter()
            .filter(|c| !matches!(c, MockCall::Preflight))
            .collect();
        assert_eq!(
            exec_calls.len(),
            1,
            "only first command should run, got {}",
            exec_calls.len()
        );
    }
}
