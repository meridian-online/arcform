//! End-to-end: a table function's lineage as the real `arc` binary reports it, not
//! just as `introspect::extract_assets` computes it in-process.
//!
//! Every SQL text below is a `sql:` step run against the real `arc run`. Several of
//! them fail to execute — `read_xlsx`/`ST_Read` name files that don't exist, and some
//! runners have no network to autoload the extensions those functions need — but
//! lineage recording is a static property of the parsed SQL, independent of whether
//! DuckDB could run it, and the run contract is written either way (see
//! `src/introspect.rs`'s own unit tests for the in-process version of these same
//! claims). Every assertion here is about the printed asset graph and the recorded
//! contract, never about a query's result.

use std::path::PathBuf;
use std::process::Command;

/// Run one `sql:` step's text on a throwaway project and return (stdout, the run's
/// `assets` array from its one JSON contract).
fn run_step(sql: &str) -> (String, Vec<serde_json::Value>) {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("arcform.yaml"),
        "name: lineage_probe\nengine: duckdb\ndb: probe.db\nsteps:\n  - name: s\n    sql: s.sql\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join("s.sql"), sql).unwrap();

    let run = Command::new(arc)
        .current_dir(workspace.path())
        .arg("run")
        .output()
        .expect("spawn arc run");
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();

    let runs = workspace.path().join("build/.arcform/runs");
    let json_files: Vec<PathBuf> = std::fs::read_dir(&runs)
        .unwrap_or_else(|e| panic!("runs dir {}: {e}", runs.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        json_files.len(),
        1,
        "expected exactly one contract for sql {sql:?}, run stdout:\n{stdout}"
    );
    let bytes = std::fs::read(&json_files[0]).unwrap();
    let contract: serde_json::Value =
        serde_json::from_slice(&bytes).expect("contract is valid JSON");
    let assets = contract["assets"].as_array().expect("assets array").clone();
    (stdout, assets)
}

fn find<'a>(assets: &'a [serde_json::Value], name: &str) -> Option<&'a serde_json::Value> {
    assets.iter().find(|a| a["name"] == name)
}

// End to end: read_xlsx lifts the file path in the printed graph and the
// contract, and records no table named `read_xlsx`.
#[test]
fn read_xlsx_lifts_the_path_in_the_printed_graph_and_contract() {
    let sql = "CREATE OR REPLACE TABLE r AS SELECT * FROM read_xlsx('build/budget.xlsx');";
    let (stdout, assets) = run_step(sql);
    assert!(
        stdout.contains("build/budget.xlsx [file]"),
        "printed graph should show the file, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("read_xlsx [") && !stdout.contains("read_xlsx["),
        "printed graph should have no node named read_xlsx, got:\n{stdout}"
    );
    assert_eq!(find(&assets, "build/budget.xlsx").unwrap()["kind"], "file");
    assert!(find(&assets, "read_xlsx").is_none());
}

// End to end: a step reading `range(3)` prints an asset graph with no node named
// `range`.
#[test]
fn range_reads_no_table_in_the_printed_graph_and_contract() {
    let sql = "CREATE OR REPLACE TABLE c AS SELECT * FROM range(3);";
    let (stdout, assets) = run_step(sql);
    assert!(
        !stdout.contains("range ["),
        "printed graph should have no node named range, got:\n{stdout}"
    );
    assert_eq!(
        assets.len(),
        1,
        "only 'c' should be a tracked asset, got {assets:?}"
    );
    assert_eq!(assets[0]["name"], "c");
}

// End to end: glob('pattern') lifts the pattern in the contract, not a table
// named `glob`.
#[test]
fn glob_lifts_the_pattern_in_the_contract_not_a_table_named_glob() {
    let sql = "CREATE OR REPLACE TABLE c AS SELECT * FROM glob('data/*.csv');";
    let (_, assets) = run_step(sql);
    assert!(
        find(&assets, "data/*.csv").is_some(),
        "pattern should be recorded, got {assets:?}"
    );
    assert!(find(&assets, "glob").is_none(), "no table named glob");
}

// End to end: an extension table function taking table arguments, and a plain
// table reference, are unaffected — both still show up under their own name.
#[test]
fn extension_function_and_plain_table_are_unaffected_end_to_end() {
    let sql = r#"CREATE OR REPLACE TABLE model AS SELECT * FROM mlpack_random_forest_train("X", "Y", "params", "model");"#;
    let (_, assets) = run_step(sql);
    assert!(
        find(&assets, "mlpack_random_forest_train").is_some(),
        "extension TVF name should still be recorded, got {assets:?}"
    );

    let sql = "CREATE OR REPLACE TABLE c AS SELECT * FROM customers;";
    let (_, assets) = run_step(sql);
    assert!(
        find(&assets, "customers").is_some(),
        "plain table reference unaffected"
    );
}
