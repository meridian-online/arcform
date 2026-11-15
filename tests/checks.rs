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
    // The pivot returns a row, so the check is a warning, which leaves the run green.
    let dir = project(
        &format!("{WITH_CHECK}    severity: warn\n"),
        &[MAKES_T, ("checks/c.sql", "PIVOT t ON n USING sum(n);\n")],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    assert_eq!(step(record, "c")["check"]["rows_returned"], 1);
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
    // arc stands `measures` and `measure_history` up for the check itself.
    let dir = project(
        WITH_CHECK,
        &[
            MAKES_T,
            (
                "checks/c.sql",
                "SELECT m.name FROM measures m JOIN measure_history h USING (name) WHERE m.value < h.value;\n",
            ),
        ],
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

/// Three rows of `(code, parent)`, every one an orphan.
const MAKES_ORPHANS: (&str, &str) = (
    "models/t.sql",
    "CREATE OR REPLACE TABLE t AS SELECT * FROM (VALUES ('11-1', '11-0'), ('11-2', '11-0'), ('11-3', NULL)) v(code, parent);\n",
);
const ORPHANS: (&str, &str) = ("checks/c.sql", "SELECT code, parent FROM t;\n");
const RISK: &str = "the analyst reads a wrong table";

/// `WITH_CHECK` with `extra` added to the check step.
fn with_check(extra: &str) -> String {
    format!("{WITH_CHECK}{extra}")
}

#[test]
fn a_passing_check_records_success_and_no_rows() {
    let dir = project(
        WITH_CHECK,
        &[MAKES_T, ("checks/c.sql", "SELECT * FROM t WHERE n < 0;\n")],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    assert_eq!(record["contract_version"], "b5/1");
    let check = &step(record, "c")["check"];
    assert_eq!(check["name"], "c", "{check}");
    assert_eq!(check["success"], true, "{check}");
    assert_eq!(check["rows_returned"], 0, "{check}");
    assert_eq!(check["sample"], serde_json::json!([]), "{check}");
    assert_eq!(check["sample_columns"], serde_json::json!(["n"]), "{check}");
    assert_eq!(check["severity"], "error", "{check}");
    assert_eq!(check["risk"], RISK, "{check}");
    assert!(check.get("error").is_none(), "{check}");
    assert_eq!(
        record["run"]["checks"],
        serde_json::json!({"passed": 1, "warned": 0, "failed": 0})
    );
    assert!(
        step(record, "t").get("check").is_none(),
        "a sql step has no check section"
    );
}

#[test]
fn a_failed_warn_check_leaves_the_run_green() {
    let dir = project(
        &with_check("    severity: warn\n"),
        &[MAKES_ORPHANS, ORPHANS],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_eq!(code, Some(0), "{stderr}");
    let record = &records(dir.path())[0];
    let entry = step(record, "c");
    assert_eq!(entry["status"]["state"], "success", "{entry}");
    let check = &entry["check"];
    assert_eq!(check["success"], false, "{check}");
    assert_eq!(check["severity"], "warn", "{check}");
    assert_eq!(check["rows_returned"], 3, "{check}");
    assert_eq!(
        check["sample_columns"],
        serde_json::json!(["code", "parent"])
    );
    assert_eq!(
        check["sample"],
        serde_json::json!([["11-1", "11-0"], ["11-2", "11-0"], ["11-3", null]])
    );
    assert_eq!(record["run"]["outcome"], "success");
    assert_eq!(
        record["run"]["checks"],
        serde_json::json!({"passed": 0, "warned": 1, "failed": 0})
    );
    let plain = common::strip_ansi(&stderr);
    let line = plain
        .lines()
        .find(|l| l.starts_with("warning:") && l.contains("check 'c'"))
        .unwrap_or_else(|| panic!("no warning naming the check in:\n{plain}"));
    assert!(line.contains(RISK), "{line}");
    assert!(line.contains("3 rows"), "{line}");
}

#[test]
fn a_failed_error_check_stops_the_run_before_the_next_step() {
    let manifest = with_check("  - name: after\n    command: touch build/after.marker\n");
    let dir = project(&manifest, &[MAKES_ORPHANS, ORPHANS]);
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("check 'c' failed:"), "{stderr}");
    assert!(stderr.contains(RISK), "{stderr}");
    assert!(
        !dir.path().join("build/after.marker").exists(),
        "the step after a failed error check ran"
    );
    let record = &records(dir.path())[0];
    let entry = step(record, "c");
    assert_eq!(entry["status"]["state"], "failed", "{entry}");
    assert_eq!(entry["check"]["success"], false, "{entry}");
    assert_eq!(entry["check"]["rows_returned"], 3, "{entry}");
    assert_eq!(entry["check"]["sample"].as_array().unwrap().len(), 3);
    assert!(entry["check"].get("error").is_none(), "{entry}");
    assert_eq!(record["run"]["checks"]["failed"], 1);
    assert_eq!(record["run"]["outcome"], "partial");
}

#[test]
fn a_check_whose_sql_fails_fails_the_step_even_as_warn() {
    let dir = project(
        &with_check("    severity: warn\n"),
        &[
            MAKES_T,
            ("checks/c.sql", "SELECT 'x'::INTEGER AS n FROM t;\n"),
        ],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0), "{stderr}");
    let record = &records(dir.path())[0];
    let entry = step(record, "c");
    assert_eq!(entry["status"]["state"], "failed", "{entry}");
    let check = &entry["check"];
    assert_eq!(check["success"], false, "{check}");
    assert_eq!(check["rows_returned"], 0, "{check}");
    assert_eq!(check["sample"], serde_json::json!([]), "{check}");
    assert!(
        check["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{check}"
    );
    assert_eq!(
        record["run"]["checks"],
        serde_json::json!({"passed": 0, "warned": 0, "failed": 1})
    );
}

#[test]
fn a_check_with_comments_and_no_semicolon_runs() {
    let dir = project(
        WITH_CHECK,
        &[
            MAKES_T,
            (
                "checks/c.sql",
                "-- leading\nSELECT 1 WHERE false -- trailing",
            ),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    assert_eq!(step(record, "c")["check"]["success"], true);
}

#[test]
fn a_sample_is_capped_and_cells_truncated() {
    let dir = project(
        &with_check("    severity: warn\n"),
        &[
            MAKES_T,
            (
                "checks/c.sql",
                "SELECT repeat('x', 10000) AS s FROM range(50);\n",
            ),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    let check = &step(record, "c")["check"];
    assert_eq!(check["rows_returned"], 50);
    let sample = check["sample"].as_array().unwrap();
    assert_eq!(sample.len(), 20);
    for row in sample {
        let cell = row[0].as_str().unwrap();
        assert_eq!(cell.chars().count(), 201, "{cell}");
        assert!(cell.ends_with('…'), "{cell}");
    }
}

#[test]
fn a_check_can_read_its_parameters() {
    let manifest = format!(
        "{}    severity: warn\nparams:\n  limit:\n    default: \"5\"\n",
        WITH_CHECK
    );
    let dir = project(
        &manifest,
        &[
            MAKES_T,
            ("checks/c.sql", "SELECT getenv('ARC_PARAM_LIMIT') AS v;\n"),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    assert_eq!(
        step(record, "c")["check"]["sample"],
        serde_json::json!([["5"]])
    );
}

#[test]
fn a_failed_error_check_names_itself_to_the_failure_hook() {
    let manifest = with_check(
        "hooks:\n  on_failure:\n    name: tell\n    command: echo \"$ARC_FAILED_STEP $ARC_EXIT_CODE\" > build/failed\n",
    );
    let dir = project(&manifest, &[MAKES_ORPHANS, ORPHANS]);
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_eq!(code, Some(2), "{stderr}");
    let told = std::fs::read_to_string(dir.path().join("build/failed")).unwrap();
    assert_eq!(told.trim(), "c check");
}

#[test]
fn a_check_whose_sql_fails_on_every_retry_records_why() {
    let dir = project(
        &with_check("    retry:\n      max_attempts: 2\n      backoff_sec: 0\n"),
        &[
            MAKES_T,
            (
                "checks/c.sql",
                "-- a header\n-- of two lines\nSELECT n\nFROM t\nWHERE no_such_column = 1;\n",
            ),
        ],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_eq!(code, Some(2), "{stderr}");
    let record = &records(dir.path())[0];
    let entry = step(record, "c");
    assert_eq!(entry["status"]["state"], "failed", "{entry}");
    assert_eq!(entry["attempts"], 2, "{entry}");
    let check = &entry["check"];
    assert_eq!(check["success"], false, "{check}");
    let error = check["error"].as_str().unwrap_or_default();
    assert!(error.contains("no_such_column"), "{check}");
    // DuckDB's line numbers are the check file's own, header comments counted.
    assert!(error.contains("LINE 5:"), "{check}");
    assert_eq!(
        record["run"]["checks"],
        serde_json::json!({"passed": 0, "warned": 0, "failed": 1})
    );
}

/// `t` of three rows, then a measure step `m` reading `measures/m.sql`.
const WITH_MEASURE: &str = "name: c\ndb: build/c.duckdb\nsteps:\n  - name: t\n    sql: models/t.sql\n  - name: m\n    measure: measures/m.sql\n";
const MAKES_THREE: (&str, &str) = (
    "models/t.sql",
    "CREATE OR REPLACE TABLE t AS SELECT * FROM range(3) r(n);\n",
);

/// A check step `name` of severity `warn` reading `checks/<name>.sql`.
fn warn_check(name: &str) -> String {
    format!(
        "  - name: {name}\n    check: checks/{name}.sql\n    risk: the analyst reads a wrong table\n    severity: warn\n"
    )
}

#[test]
fn a_measure_step_records_its_values() {
    let dir = project(
        WITH_MEASURE,
        &[
            MAKES_THREE,
            (
                "measures/m.sql",
                "SELECT count(*) AS n, 0.5 AS share FROM t;\n",
            ),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    let entry = step(record, "m");
    assert_eq!(entry["kind"], "measure", "{entry}");
    assert_eq!(entry["status"]["state"], "success", "{entry}");
    let values = entry["measure"]["values"].as_object().unwrap();
    assert_eq!(values.len(), 2, "{entry}");
    assert_eq!(values["m.n"], 3.0, "{entry}");
    assert_eq!(values["m.share"], 0.5, "{entry}");
}

#[test]
fn a_measure_returning_two_rows_or_text_fails_its_step() {
    for (sql, rule) in [
        ("SELECT n FROM t WHERE n < 2;\n", "exactly one row"),
        ("SELECT 'x' AS s;\n", "numeric"),
    ] {
        let dir = project(WITH_MEASURE, &[MAKES_THREE, ("measures/m.sql", sql)]);
        let (code, _, stderr) = common::arc_run_raw(dir.path());
        assert_ne!(code, Some(0), "{sql}: {stderr}");
        assert!(stderr.contains("step 'm'"), "{sql}: {stderr}");
        assert!(stderr.contains(rule), "{sql}: {stderr}");
        let record = &records(dir.path())[0];
        let entry = step(record, "m");
        assert_eq!(entry["status"]["state"], "failed", "{entry}");
        assert!(entry.get("measure").is_none(), "{entry}");
    }
}

#[test]
fn a_null_measure_is_recorded_as_null() {
    let manifest = format!("{WITH_MEASURE}{}", warn_check("c"));
    let dir = project(
        &manifest,
        &[
            MAKES_THREE,
            ("measures/m.sql", "SELECT NULL::INTEGER AS z;\n"),
            (
                "checks/c.sql",
                "SELECT name FROM measures WHERE name = 'm.z' AND value IS NULL;\n",
            ),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    let entry = step(record, "m");
    let values = entry["measure"]["values"].as_object().unwrap();
    assert!(values.contains_key("m.z"), "{entry}");
    assert!(values["m.z"].is_null(), "{entry}");
    // `measures` holds the null measure, with a NULL value.
    assert_eq!(step(record, "c")["check"]["rows_returned"], 1, "{record}");
}

#[test]
fn a_measure_column_with_a_dot_fails_its_step() {
    let dir = project(
        WITH_MEASURE,
        &[MAKES_THREE, ("measures/m.sql", "SELECT 1 AS \"a.b\";\n")],
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("step 'm'"), "{stderr}");
    assert!(stderr.contains("'a.b'"), "{stderr}");
    let record = &records(dir.path())[0];
    assert_eq!(step(record, "m")["status"]["state"], "failed");
}

#[test]
fn each_produced_table_is_measured_at_its_step() {
    let dir = project(
        WITH_MEASURE,
        &[MAKES_THREE, ("measures/m.sql", "SELECT 1 AS one;\n")],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    let entry = step(record, "t");
    let values = &entry["measure"]["values"];
    assert_eq!(values["table:t.row_count"], 3.0, "{entry}");
    assert!(
        values["table:t.bytes"].as_f64().is_some_and(|b| b > 0.0),
        "{entry}"
    );
    assert!(
        step(record, "m")["measure"]["values"]
            .as_object()
            .unwrap()
            .keys()
            .all(|k| !k.starts_with("table:")),
        "a measure step produces no table"
    );
}

#[test]
fn a_table_named_order_is_measured_at_its_step() {
    let dir = project(
        WITH_MEASURE,
        &[
            (
                "models/t.sql",
                "CREATE OR REPLACE TABLE \"order\" AS SELECT * FROM range(2) r(n);\n",
            ),
            ("measures/m.sql", "SELECT 1 AS one;\n"),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    let entry = step(record, "t");
    assert_eq!(
        entry["measure"]["values"]["table:order.row_count"], 2.0,
        "{entry}"
    );
}

#[test]
fn a_check_reads_this_runs_measures() {
    for (other, passes) in [(3, true), (4, false)] {
        let manifest = format!(
            "name: c\ndb: build/c.duckdb\nsteps:\n  - name: t\n    sql: models/t.sql\n{}",
            warn_check("c")
        );
        let check = format!(
            "SELECT * FROM measures WHERE name = 'table:t.row_count' AND value <> {other};\n"
        );
        let dir = project(&manifest, &[MAKES_THREE, ("checks/c.sql", &check)]);
        common::arc_run(dir.path());
        let record = &records(dir.path())[0];
        let found = &step(record, "c")["check"];
        assert_eq!(found["success"], passes, "<> {other}: {found}");
        assert_eq!(found["rows_returned"], u64::from(!passes), "{found}");
    }
}

#[test]
fn a_check_before_a_measure_does_not_see_it() {
    let manifest = format!(
        "name: c\ndb: build/c.duckdb\nsteps:\n  - name: t\n    sql: models/t.sql\n{}  - name: m\n    measure: measures/m.sql\n{}",
        warn_check("before"),
        warn_check("after")
    );
    let names = "SELECT name FROM measures ORDER BY name;\n";
    let dir = project(
        &manifest,
        &[
            MAKES_THREE,
            ("measures/m.sql", "SELECT 7 AS seven;\n"),
            ("checks/before.sql", names),
            ("checks/after.sql", names),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    assert_eq!(
        step(record, "before")["check"]["sample"],
        serde_json::json!([["table:t.bytes"], ["table:t.row_count"]])
    );
    assert_eq!(
        step(record, "after")["check"]["sample"],
        serde_json::json!([["m.seven"], ["table:t.bytes"], ["table:t.row_count"]])
    );
}

#[test]
fn a_fresh_step_still_has_its_tables_measured() {
    let dir = project(
        WITH_MEASURE,
        &[
            MAKES_THREE,
            ("measures/m.sql", "SELECT count(*) AS n FROM t;\n"),
        ],
    );
    common::arc_run(dir.path());
    common::arc_run(dir.path());
    let records = records(dir.path());
    assert_eq!(records.len(), 2);
    let entry = step(&records[1], "t");
    assert_eq!(entry["status"]["state"], "skipped", "{entry}");
    assert_eq!(
        entry["measure"]["values"]["table:t.row_count"], 3.0,
        "{entry}"
    );
}

#[test]
fn measures_holds_one_row_per_name_the_latest_winning() {
    let manifest = format!(
        "name: c\ndb: build/c.duckdb\nsteps:\n  - name: t\n    sql: models/t.sql\n  - name: more\n    sql: models/more.sql\n{}",
        warn_check("c")
    );
    let dir = project(
        &manifest,
        &[
            MAKES_THREE,
            (
                "models/more.sql",
                "INSERT INTO t SELECT * FROM range(3, 5);\n",
            ),
            (
                "checks/c.sql",
                "SELECT name, count(*) FROM measures WHERE name = 'table:t.row_count' GROUP BY name HAVING count(*) > 1\nUNION ALL SELECT name, 1 FROM measures WHERE name = 'table:t.row_count' AND value <> 5;\n",
            ),
        ],
    );
    common::arc_run(dir.path());
    let record = &records(dir.path())[0];
    let check = &step(record, "c")["check"];
    assert_eq!(check["success"], true, "{check}");
    // Each step's own entry keeps what it measured.
    assert_eq!(
        step(record, "t")["measure"]["values"]["table:t.row_count"],
        3.0
    );
    assert_eq!(
        step(record, "more")["measure"]["values"]["table:t.row_count"],
        5.0
    );
}

#[test]
fn a_comparison_check_sees_the_previous_run() {
    let manifest = "name: c\ndb: build/c.duckdb\nsteps:\n  - name: t\n    sql: models/t.sql\n  - name: drop\n    check: checks/drop.sql\n    risk: a table shrank by half\n    severity: warn\n";
    let compare = "SELECT name FROM measures m JOIN (SELECT name, last(value ORDER BY run_at, run_id) AS prev FROM measure_history GROUP BY name) p USING (name) WHERE m.value < p.prev * 0.5\n";
    let dir = project(
        manifest,
        &[
            (
                "models/t.sql",
                "CREATE OR REPLACE TABLE t AS SELECT * FROM range(4) r(n);\n",
            ),
            ("checks/drop.sql", compare),
        ],
    );
    common::arc_run(dir.path());
    // The sql file changes, so the step runs again and loads one row.
    std::fs::write(
        dir.path().join("models/t.sql"),
        "CREATE OR REPLACE TABLE t AS SELECT * FROM range(1) r(n);\n",
    )
    .unwrap();
    common::arc_run(dir.path());
    let records = records(dir.path());
    assert_eq!(records.len(), 2);
    let first = &step(&records[0], "drop")["check"];
    assert_eq!(
        first["success"], true,
        "an empty history warns nothing: {first}"
    );
    let second = &step(&records[1], "drop")["check"];
    assert_eq!(second["success"], false, "{second}");
    assert_eq!(second["severity"], "warn", "{second}");
    let sample = second["sample"].as_array().unwrap();
    assert!(
        sample.contains(&serde_json::json!(["table:t.row_count"])),
        "{second}"
    );
}

/// The issue's acceptance: a load, a measure, a warn check, a comparison over
/// `measure_history`, an error check and a local `ducklake_write` behind it. Run 1 passes and
/// publishes; run 2 loads data the error check refuses, so nothing after it publishes.
#[test]
fn checks_and_measures_end_to_end() {
    let manifest = "name: e2e\ndb: build/e2e.duckdb\nsteps:\n  - name: load\n    sql: models/load.sql\n  - name: size\n    measure: measures/size.sql\n  - name: labels\n    check: checks/labels.sql\n    risk: a code publishes without a label\n    severity: warn\n  - name: shrank\n    check: checks/shrank.sql\n    risk: a table lost half its rows since the last good run\n    severity: warn\n  - name: unique\n    check: checks/unique.sql\n    risk: a duplicated code publishes two meanings for one key\n  - name: export\n    sql: models/export.sql\n  - name: publish\n    op: ducklake_write@1\n    with:\n      public_base: https://lake.example/\n      catalog: catalog/test.ducklake\n      data_path: lake/\n      local_catalog: build/lake/test.ducklake\n      tables:\n        - table: demo.codes\n          from: data/codes.parquet\n          descriptor: codes.datapackage.json\n      credential:\n        type: r2\n        bucket: test-bucket\n        env:\n          key_id: ARC_TEST_LAKE_KEY_ID\n          secret: ARC_TEST_LAKE_SECRET\n          account_id: ARC_TEST_LAKE_ACCOUNT_ID\n";
    let descriptor = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lake/protocol/codes.datapackage.json"
    ))
    .unwrap();
    let run1_csv: String = std::iter::once("code,label\n".to_string())
        .chain((0..10).map(|n| format!("c{n:03},label {n}\n")))
        .collect();
    let dir = project(
        manifest,
        &[
            ("codes.datapackage.json", &descriptor),
            ("data/codes.csv", &run1_csv),
            (
                "models/load.sql",
                "CREATE OR REPLACE TABLE codes AS SELECT code, label FROM read_csv('data/codes.csv', all_varchar = true);\n",
            ),
            (
                "models/export.sql",
                "COPY (SELECT * FROM codes ORDER BY code) TO 'data/codes.parquet' (FORMAT parquet);\n",
            ),
            (
                "measures/size.sql",
                "SELECT count(*) AS codes FROM codes;\n",
            ),
            (
                "checks/labels.sql",
                "SELECT code, label FROM codes WHERE coalesce(label, '') = '' ORDER BY code;\n",
            ),
            (
                "checks/shrank.sql",
                "SELECT name, m.value, p.prev FROM measures m JOIN (\n  SELECT name, last(value ORDER BY run_at, run_id) AS prev FROM measure_history GROUP BY name\n) p USING (name)\nWHERE m.value < p.prev * 0.5\nORDER BY name;\n",
            ),
            (
                "checks/unique.sql",
                "SELECT code, count(*) AS copies FROM codes GROUP BY code HAVING count(*) > 1 ORDER BY code;\n",
            ),
        ],
    );
    let p = dir.path();

    // Run 1: every check passes and the lake gets its first snapshot.
    let (code, stdout, stderr) = common::arc_run_raw(p);
    assert_eq!(code, Some(0), "run 1:\n{stdout}\n{stderr}");
    let first = records(p).pop().unwrap();
    assert_eq!(first["contract_version"], "b5/1");
    assert_eq!(
        first["run"]["checks"],
        serde_json::json!({"passed": 3, "warned": 0, "failed": 0}),
        "{first}"
    );
    let published = &step(&first, "publish")["report"];
    assert_eq!(published["target"], "local", "{published}");
    assert_eq!(published["committed"], true, "{published}");
    let first_snapshot = published["snapshot_id"].as_i64().unwrap();
    assert_eq!(
        step(&first, "size")["measure"]["values"]["size.codes"],
        10.0
    );

    // Run 2: four rows, one code twice and one label empty.
    std::fs::write(
        p.join("data/codes.csv"),
        "code,label\nc000,label 0\nc001,\nc002,label 2\nc002,label 2 again\n",
    )
    .unwrap();
    let (code, stdout, stderr) = common::arc_run_raw(p);
    assert_eq!(code, Some(2), "run 2:\n{stdout}\n{stderr}");
    let all = records(p);
    assert_eq!(all.len(), 2);
    let second = &all[1];
    assert_eq!(second["run"]["outcome"], "partial", "{second}");
    assert_eq!(
        second["run"]["checks"],
        serde_json::json!({"passed": 0, "warned": 2, "failed": 1}),
        "{second}"
    );

    // The measures: the measure step's own and the load's built-in ones.
    assert_eq!(step(second, "size")["measure"]["values"]["size.codes"], 4.0);
    assert_eq!(
        step(second, "load")["measure"]["values"]["table:codes.row_count"],
        4.0
    );

    // The warn check: its outcome, its rows, and a warning line on stderr.
    let labels = &step(second, "labels")["check"];
    assert_eq!(labels["success"], false, "{labels}");
    assert_eq!(labels["severity"], "warn", "{labels}");
    assert_eq!(step(second, "labels")["status"]["state"], "success");
    assert_eq!(
        labels["sample_columns"],
        serde_json::json!(["code", "label"])
    );
    assert_eq!(labels["sample"], serde_json::json!([["c001", null]]));
    let plain = common::strip_ansi(&stderr);
    assert!(
        plain.lines().any(|l| l.starts_with("warning:")
            && l.contains("check 'labels'")
            && l.contains("a code publishes without a label")),
        "{plain}"
    );

    // The comparison: both counts fell below half of run 1's.
    let shrank = &step(second, "shrank")["check"];
    assert_eq!(shrank["success"], false, "{shrank}");
    assert_eq!(
        shrank["sample"],
        serde_json::json!([
            ["size.codes", "4.0", "10.0"],
            ["table:codes.row_count", "4.0", "10.0"]
        ]),
        "{shrank}"
    );

    // The error check stops the run.
    let unique = step(second, "unique");
    assert_eq!(unique["status"]["state"], "failed", "{unique}");
    assert_eq!(unique["check"]["success"], false, "{unique}");
    assert_eq!(unique["check"]["severity"], "error", "{unique}");
    assert_eq!(unique["check"]["rows_returned"], 1, "{unique}");
    assert_eq!(
        unique["check"]["sample"],
        serde_json::json!([["c002", "2"]])
    );
    assert!(plain.contains("check 'unique' failed:"), "{plain}");

    // Nothing after it ran, and the lake holds only run 1's snapshot.
    for name in ["export", "publish"] {
        let entry = step(second, name);
        assert_eq!(entry["status"]["state"], "skipped", "{entry}");
        assert_eq!(entry["attempts"], 0, "{entry}");
    }
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "LOAD ducklake; ATTACH 'ducklake:{}' AS lake (READ_ONLY);",
        p.join("build/lake/test.ducklake").display()
    ))
    .unwrap();
    let latest: i64 = conn
        .query_row(
            "SELECT max(snapshot_id) FROM ducklake_snapshots('lake')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(latest, first_snapshot);
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM lake.demo.codes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 10);
}

#[test]
fn an_unreadable_measure_history_csv_fails_the_run_before_it_starts() {
    let dir = project(
        WITH_CHECK,
        &[MAKES_T, ("checks/c.sql", "SELECT n FROM t WHERE n < 0;")],
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir.path())
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .env("ARC_TEST_MEASURE_HISTORY", dir.path().join("missing.csv"))
        .arg("run")
        .output()
        .expect("spawn arc run");
    let stderr = common::strip_ansi(&String::from_utf8_lossy(&out.stderr));
    assert_ne!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("ARC_TEST_MEASURE_HISTORY"), "{stderr}");
    assert!(stderr.contains("missing.csv"), "{stderr}");
    let runs = dir.path().join("build/.arcform/runs");
    let left: Vec<_> = std::fs::read_dir(&runs)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "the run left {left:?}");
}
