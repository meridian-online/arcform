//! End-to-end: `check:` and `measure:` files are read as Protocol SQL, refused unless each
//! holds exactly one query, and never skipped as fresh, through the real `arc` binary. Needs
//! a DuckDB 2 `duckdb` CLI on PATH, as any `arc run` with a SQL step does.

#[allow(dead_code)]
mod common;

use std::path::Path;

/// A Protocol in a fresh folder: `manifest` as `arcform.yaml` and each `(path, sql)` file.
fn project(manifest: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("arcform.yaml"), manifest).unwrap();
    for (path, sql) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, sql).unwrap();
    }
    std::fs::create_dir_all(dir.path().join("build")).unwrap();
    dir
}

/// Every run record the folder holds, oldest first.
fn records(dir: &Path) -> Vec<serde_json::Value> {
    let runs = dir.join("build/.arcform/runs");
    let Ok(read) = std::fs::read_dir(&runs) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect()
}

fn step<'a>(record: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    record["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no step {name} in {record}"))
}

const WITH_CHECK: &str = "name: c\ndb: build/c.duckdb\nsteps:\n  - name: t\n    sql: models/t.sql\n  - name: c\n    check: checks/c.sql\n    risk: the analyst reads a wrong table\n";
const MAKES_T: (&str, &str) = (
    "models/t.sql",
    "CREATE OR REPLACE TABLE t AS SELECT 1 AS n;\n",
);

#[test]
fn a_check_file_with_two_statements_is_refused_before_any_step_runs() {
    let dir = project(
        WITH_CHECK,
        &[MAKES_T, ("checks/c.sql", "SELECT 1; SELECT 2;")],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("step 'c'"), "{stderr}");
    assert!(stderr.contains("exactly one query"), "{stderr}");
    assert!(records(dir.path()).is_empty(), "a run left a record");
}

#[test]
fn a_check_file_holding_a_create_is_refused() {
    let dir = project(
        WITH_CHECK,
        &[MAKES_T, ("checks/c.sql", "CREATE TABLE x AS SELECT 1")],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("step 'c'"), "{stderr}");
    assert!(stderr.contains("exactly one query"), "{stderr}");
    assert!(records(dir.path()).is_empty());
}

#[test]
fn a_step_producing_measures_is_refused() {
    let dir = project(
        "name: c\ndb: build/c.duckdb\nsteps:\n  - name: m\n    sql: models/m.sql\n  - name: c\n    check: checks/c.sql\n    risk: the analyst reads a wrong table\n",
        &[
            ("models/m.sql", "CREATE TABLE measures AS SELECT 1;"),
            ("checks/c.sql", "SELECT * FROM measures WHERE false;"),
        ],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("measures"), "{stderr}");
    assert!(stderr.contains("reserves"), "{stderr}");
    assert!(records(dir.path()).is_empty());
}

#[test]
fn a_protocol_without_checks_may_make_a_measures_table() {
    let dir = project(
        "name: c\ndb: build/c.duckdb\nsteps:\n  - name: m\n    sql: models/m.sql\n",
        &[(
            "models/m.sql",
            "CREATE OR REPLACE TABLE measures AS SELECT 1;",
        )],
    );
    common::arc_run(dir.path());
}

#[test]
fn a_pivot_check_file_is_one_query() {
    let dir = project(
        WITH_CHECK,
        &[MAKES_T, ("checks/c.sql", "PIVOT t ON n USING sum(n);\n")],
    );
    common::arc_run(dir.path());
}

#[test]
fn a_check_reads_its_tables_in_the_asset_graph() {
    let dir = project(
        WITH_CHECK,
        &[MAKES_T, ("checks/c.sql", "SELECT * FROM t WHERE n < 0;\n")],
    );
    common::arc_run(dir.path());
    let records = records(dir.path());
    let record = &records[0];
    let check = step(record, "c");
    assert_eq!(check["kind"], "check");
    assert_eq!(check["sql"]["model_path"], "checks/c.sql");
    let statements = check["sql"]["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0]["reads"], serde_json::json!(["t"]));
    let t = record["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "table:t")
        .unwrap_or_else(|| panic!("no asset t in {record}"));
    assert!(
        t["consumed_by"].to_string().contains("\"c\""),
        "consumed_by: {}",
        t["consumed_by"]
    );
}

#[test]
fn a_check_reading_measures_lists_no_measures_asset() {
    // Nothing makes `measures` yet, so a command step stands one up for the check to read.
    let dir = project(
        "name: c\ndb: build/c.duckdb\nsteps:\n  - name: m\n    command: duckdb \"$ARC_DB_PATH\" \"CREATE OR REPLACE TABLE measures AS SELECT 1 AS n\"\n  - name: c\n    check: checks/c.sql\n    risk: the analyst reads a wrong table\n",
        &[("checks/c.sql", "SELECT * FROM measures WHERE n < 0;\n")],
    );
    common::arc_run(dir.path());
    let records = records(dir.path());
    for asset in records[0]["assets"].as_array().unwrap() {
        let id = asset["id"].as_str().unwrap();
        assert!(
            id != "table:measures" && id != "table:measure_history",
            "{id} is listed"
        );
    }
    assert_eq!(step(&records[0], "c")["kind"], "check");
}

#[test]
fn a_protocol_whose_only_sql_is_a_check_gets_the_engine_preflight() {
    let dir = project(
        "name: c\ndb: build/c.duckdb\nsteps:\n  - name: t\n    command: duckdb \"$ARC_DB_PATH\" \"CREATE OR REPLACE TABLE t AS SELECT 1 AS n\"\n  - name: c\n    check: checks/c.sql\n    risk: the analyst reads a wrong table\n",
        &[("checks/c.sql", "SELECT * FROM t WHERE n < 0;\n")],
    );
    let out = common::arc_run(dir.path());
    assert!(!out.contains("Error"), "{out}");
    assert_eq!(records(dir.path()).len(), 1);
}

#[test]
fn a_check_runs_again_on_an_unchanged_second_run() {
    let dir = project(
        WITH_CHECK,
        &[MAKES_T, ("checks/c.sql", "SELECT * FROM t WHERE n < 0;\n")],
    );
    common::arc_run(dir.path());
    common::arc_run(dir.path());
    let records = records(dir.path());
    assert_eq!(records.len(), 2);
    let second = &records[1];
    assert_eq!(step(second, "t")["status"]["state"], "skipped", "{second}");
    assert_ne!(step(second, "c")["status"]["state"], "skipped", "{second}");
}

#[test]
fn the_ordering_rule_refuses_before_any_step_runs() {
    let manifest = "name: c\ndb: build/c.duckdb\nsteps:\n  - name: load\n    sql: models/t.sql\n  - name: export\n    sql: models/export.sql\n  - name: publish\n    op: ducklake_write@1\n    with:\n      public_base: https://lake.example/\n      catalog: catalog/test.ducklake\n      data_path: lake/\n      local_catalog: build/lake/test.ducklake\n      tables:\n        - table: demo.t\n          from: build/t.parquet\n          descriptor: t.datapackage.json\n      credential:\n        type: r2\n        bucket: test-bucket\n        env:\n          key_id: ARC_TEST_LAKE_KEY_ID\n          secret: ARC_TEST_LAKE_SECRET\n          account_id: ARC_TEST_LAKE_ACCOUNT_ID\n  - name: c\n    check: checks/c.sql\n    risk: the analyst reads a wrong table\n";
    let dir = project(
        manifest,
        &[
            MAKES_T,
            (
                "models/export.sql",
                "COPY t TO 'build/t.parquet' (FORMAT parquet);\n",
            ),
            ("checks/c.sql", "SELECT * FROM t WHERE n < 0;\n"),
        ],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("step 'publish' publishes before the error check 'c'"),
        "{stderr}"
    );
    assert!(records(dir.path()).is_empty(), "a run left a record");
}
