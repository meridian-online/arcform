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
    run_steps(&[("s", sql)])
}

/// Run several `sql:` steps, in the order given, on a throwaway project — each `(name,
/// text)` pair is a step named `name` whose SQL is `name.sql` — and return (stdout, the
/// run's `assets` array from its one JSON contract).
fn run_steps(steps: &[(&str, &str)]) -> (String, Vec<serde_json::Value>) {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    let mut manifest = String::from("name: lineage_probe\nengine: duckdb\ndb: probe.db\nsteps:\n");
    for (name, sql) in steps {
        manifest.push_str(&format!("  - name: {name}\n    sql: {name}.sql\n"));
        std::fs::write(workspace.path().join(format!("{name}.sql")), sql).unwrap();
    }
    std::fs::write(workspace.path().join("arcform.yaml"), manifest).unwrap();
    let sql = steps
        .iter()
        .map(|(_, sql)| *sql)
        .collect::<Vec<_>>()
        .join("\n");

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

// End to end: a plain table reference still shows up under its own name. (An
// extension table function called with table arguments no longer does — see
// `tests/unread_table_function.rs`.)
#[test]
fn plain_table_is_unaffected_end_to_end() {
    let sql = "CREATE OR REPLACE TABLE c AS SELECT * FROM customers;";
    let (_, assets) = run_step(sql);
    assert!(
        find(&assets, "customers").is_some(),
        "plain table reference unaffected"
    );
}

// A table that carries a row-generator's or a catalog function's name, read with no
// parentheses, is a table: the name only means "no table behind me" when it is called
// (`range(3)`). Each name is made as a table by one step and read by the next, and the
// printed graph and the contract both hold it as a table the second step reads.
#[test]
fn a_table_named_for_a_generator_is_recorded_as_a_table_the_step_reads() {
    for name in [
        "range",
        "generate_series",
        "duckdb_functions",
        "duckdb_tables",
        "duckdb_secrets",
    ] {
        let make = format!("CREATE TABLE {name} AS SELECT 1 AS n;");
        let read = format!("CREATE TABLE r AS SELECT * FROM {name};");
        let (stdout, assets) = run_steps(&[("make", &make), ("read", &read)]);

        let asset = find(&assets, name)
            .unwrap_or_else(|| panic!("{name} should be a recorded asset, got {assets:?}"));
        assert_eq!(asset["kind"], "table", "{name} is a table, got {asset}");
        assert_eq!(asset["produced_by"], "make", "{name}: {asset}");
        assert_eq!(
            asset["consumed_by"],
            serde_json::json!(["read"]),
            "{name} is read by the second step, got {asset}"
        );
        assert!(
            // `[table]` alone when the run could not execute, `[table, 1 row]` when it did.
            stdout.contains(&format!("{name} [table")),
            "printed graph should hold {name} as a table, got:\n{stdout}"
        );
    }
}

// A function named `read_` something that arc does not know as a reader is not a
// reader: its argument is not recorded as a file. arc knows a fixed list of readers
// (`read_parquet`, `read_csv`, `read_xlsx`, …); a name's `read_` prefix is not on it.
#[test]
fn an_unknown_read_function_records_no_file_for_its_argument() {
    let sql = "CREATE TABLE r AS SELECT * FROM read_widget('x.dat');";
    let (_, assets) = run_step(sql);

    assert!(
        find(&assets, "x.dat").is_none(),
        "x.dat is an argument to a function arc does not know as a reader, got {assets:?}"
    );
    assert!(
        assets.iter().all(|a| a["kind"] != "file"),
        "no file should be recorded, got {assets:?}"
    );
    assert!(
        find(&assets, "read_widget").is_none(),
        "no table named read_widget"
    );
    assert_eq!(
        find(&assets, "r").expect("the step's own table")["produced_by"],
        "s",
        "the statement parsed and its lineage was recorded, got {assets:?}"
    );
}
