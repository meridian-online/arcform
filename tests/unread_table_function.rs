//! End-to-end: a SQL step that calls a table function whose arguments arc does not read,
//! as the real `arc run` reports it.
//!
//! A table function called with a string or a quoted identifier among its arguments —
//! `mlpack_random_forest_train("X", "Y", "params", "model")` — names tables, files or
//! queries that arc has no signature to interpret. The call records no table under the
//! function's own name, and `arc run` prints one warning that asks the step for
//! `depends_on:` and `produces:`.
//!
//! Lineage and the warning are static properties of the parsed SQL, so the tests that
//! call a function no runner has installed still hold when DuckDB refuses the call: the
//! warning is printed before any step runs and the run contract is written either way.

use std::path::PathBuf;
use std::process::Command;

/// The two things the rule reads off a run: what arc printed to stderr, and the
/// contract's `assets` array.
struct Run {
    stdout: String,
    stderr: String,
    assets: Vec<serde_json::Value>,
}

impl Run {
    /// How many warnings named an unread table function call.
    fn unread_warnings(&self) -> usize {
        self.stderr.matches("calls the table function").count()
    }

    fn asset(&self, name: &str) -> Option<&serde_json::Value> {
        self.assets.iter().find(|a| a["name"] == name)
    }
}

/// Run one `sql:` step's text on a throwaway project. `declarations` is extra step YAML
/// under the step's own keys (`    depends_on: [x]`), or empty.
fn run_step(declarations: &str, sql: &str) -> Run {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("arcform.yaml"),
        format!(
            "name: unread_probe\nengine: duckdb\ndb: probe.db\nsteps:\n  - name: s\n    sql: s.sql\n{declarations}"
        ),
    )
    .unwrap();
    std::fs::write(workspace.path().join("s.sql"), sql).unwrap();

    let run = Command::new(arc)
        .current_dir(workspace.path())
        .arg("run")
        .output()
        .expect("spawn arc run");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

    let runs = workspace.path().join("build/.arcform/runs");
    let json_files: Vec<PathBuf> = std::fs::read_dir(&runs)
        .unwrap_or_else(|e| panic!("runs dir {}: {e}", runs.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        json_files.len(),
        1,
        "expected exactly one contract for sql {sql:?}, run stderr:\n{stderr}"
    );
    let contract: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&json_files[0]).unwrap())
            .expect("contract is valid JSON");
    let assets = contract["assets"].as_array().expect("assets array").clone();
    Run {
        stdout,
        stderr,
        assets,
    }
}

const TRAIN: &str = r#"CREATE TABLE fitted AS SELECT * FROM mlpack_random_forest_train("X", "Y", "params", "model");"#;

// Neither `depends_on:` nor `produces:` declared. The step reads no table named
// for the function, in the printed graph or in the contract, and the run prints one
// warning naming the step and the function and asking for the two declarations.
#[test]
fn undeclared_step_records_no_table_for_the_function_and_warns_once() {
    let run = run_step("", TRAIN);

    assert!(
        run.asset("mlpack_random_forest_train").is_none(),
        "no asset named for the function, got {:?}",
        run.assets
    );
    assert!(
        !run.stdout.contains("mlpack_random_forest_train ["),
        "printed graph should have no node named for the function, got:\n{}",
        run.stdout
    );
    assert_eq!(run.asset("fitted").unwrap()["produced_by"], "s");

    assert_eq!(
        run.unread_warnings(),
        1,
        "exactly one warning, stderr:\n{}",
        run.stderr
    );
    for wanted in [
        "step 's'",
        "'mlpack_random_forest_train'",
        "`depends_on:` and `produces:` declare what the step reads and writes",
    ] {
        assert!(
            run.stderr.contains(wanted),
            "warning should contain {wanted}, stderr:\n{}",
            run.stderr
        );
    }
}

// The same step with `depends_on: [x, y, params]` and `produces: [model]` reads
// those three, produces `model` and `fitted`, and draws no such warning.
#[test]
fn declared_step_reads_and_produces_what_it_declared_and_draws_no_warning() {
    let run = run_step(
        "    depends_on: [x, y, params]\n    produces: [model]\n",
        TRAIN,
    );

    assert_eq!(
        run.unread_warnings(),
        0,
        "no warning when the step declares, stderr:\n{}",
        run.stderr
    );
    for read in ["x", "y", "params"] {
        let asset = run
            .asset(read)
            .unwrap_or_else(|| panic!("{read} should be in the graph, got {:?}", run.assets));
        assert_eq!(asset["consumed_by"], serde_json::json!(["s"]), "{read}");
    }
    for produced in ["model", "fitted"] {
        let asset = run
            .asset(produced)
            .unwrap_or_else(|| panic!("{produced} should be in the graph, got {:?}", run.assets));
        assert_eq!(asset["produced_by"], "s", "{produced}");
    }
    assert!(run.asset("mlpack_random_forest_train").is_none());
}

// A table function with no string and no quoted identifier among its arguments
// draws no such warning. `range(10)` and the table macro call `recent()` are the two
// checked; both run to completion, so the warning's absence is not a run that stopped
// early.
#[test]
fn a_call_with_no_string_or_quoted_identifier_draws_no_warning() {
    let range = run_step("", "CREATE TABLE c AS SELECT * FROM range(10);");
    assert_eq!(range.unread_warnings(), 0, "range(10): {}", range.stderr);
    assert!(range.asset("range").is_none(), "range(10) reads no table");

    let recent = run_step(
        "",
        "CREATE MACRO recent() AS TABLE SELECT 1 AS n;\nCREATE TABLE r AS SELECT * FROM recent();",
    );
    assert_eq!(recent.unread_warnings(), 0, "recent(): {}", recent.stderr);
    assert!(
        recent.asset("recent").is_some(),
        "the table macro keeps recording its own name, got {:?}",
        recent.assets
    );
}
