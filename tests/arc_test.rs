//! End-to-end: `arc test` runs each test in `tests.yaml` in a copy of the Protocol, with
//! checked-in fixture files standing in for its fetches, through the real `arc` binary.
//! Needs a DuckDB 2 `duckdb` CLI on PATH, as any `arc run` with a SQL step does.

#[allow(dead_code)]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// The fixture the fetch step's output is replaced with: three codes, each labelled.
const CODES: &str = "code,label\n11,Management\n13,Business\n15,Computing\n";

/// Reads the fetched CSV, then refuses it when its `code` column has gone.
const LOAD: &str = "CREATE OR REPLACE TABLE codes AS FROM read_csv('build/raw/codes.csv', all_varchar = true);\n\
SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'codes' AND column_name = 'code') THEN error('codes header has moved') END AS guard;\n";

/// The same load, with a guard that fails whatever the data.
const LOAD_ALWAYS_FAILS: &str = "CREATE OR REPLACE TABLE codes AS FROM read_csv('build/raw/codes.csv', all_varchar = true);\n\
SELECT error('codes header has moved') AS guard;\n";

/// A Protocol at `root/p/`, beside nothing else: a fetch, a load, a measure, a check, an
/// export and a local publish.
struct Protocol {
    root: tempfile::TempDir,
}

impl Protocol {
    /// Build it with `tests` as `tests.yaml`, the fetch reading `url`, the load step's SQL
    /// `load`, and `params` as the manifest's `params:` block (empty for none).
    fn with(tests: &str, url: &str, load: &str, params: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("p");
        let catalog = root.path().join("lake").join("test.ducklake");
        let manifest = format!(
            "name: codes\n{params}steps:\n  \
             - name: fetch_codes\n    op: http_fetch@1\n    with:\n      url: {url}\n      out: build/raw/codes.csv\n  \
             - name: load\n    sql: models/load.sql\n  \
             - name: size\n    measure: measures/size.sql\n  \
             - name: labelled\n    check: checks/labelled.sql\n    risk: a code publishes without a label\n  \
             - name: export\n    sql: models/export.sql\n  \
             - name: publish\n    op: ducklake_write@1\n    with:\n      public_base: https://lake.example/\n      catalog: catalog/test.ducklake\n      data_path: lake/\n      local_catalog: {}\n      tables:\n        - table: demo.codes\n          from: build/codes.parquet\n          descriptor: codes.datapackage.json\n      credential:\n        type: r2\n        bucket: test-bucket\n        env:\n          key_id: ARC_TEST_LAKE_KEY_ID\n          secret: ARC_TEST_LAKE_SECRET\n          account_id: ARC_TEST_LAKE_ACCOUNT_ID\n",
            catalog.display()
        );
        let files = [
            ("arcform.yaml", manifest.as_str()),
            ("tests.yaml", tests),
            ("testdata/codes.csv", CODES),
            ("models/load.sql", load),
            (
                "measures/size.sql",
                "SELECT count(*) AS codes FROM codes;\n",
            ),
            (
                "checks/labelled.sql",
                "SELECT code FROM codes WHERE label IS NULL OR label = '';\n",
            ),
            (
                "models/export.sql",
                "COPY codes TO 'build/codes.parquet' (FORMAT parquet);\n",
            ),
            (
                "codes.datapackage.json",
                "{\"name\": \"codes\", \"resources\": []}\n",
            ),
        ];
        for (path, text) in files {
            let full = p.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        Protocol { root }
    }

    fn new(tests: &str) -> Self {
        Self::with(tests, "http://127.0.0.1:9/never", LOAD, "")
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("p")
    }

    fn write(&self, path: &str, text: &str) {
        let full = self.dir().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }

    /// `arc test --dir p`: (exit code, stdout with ANSI codes stripped, stderr).
    fn arc_test(&self) -> (Option<i32>, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_arc"))
            .args(["test", "--dir"])
            .arg(self.dir())
            .env("ARCFORM_DB_DIR", self.root.path().join("arc-db"))
            .env_remove("ARC_PUBLISH")
            .output()
            .expect("spawn arc test");
        (
            out.status.code(),
            common::strip_ansi(&String::from_utf8_lossy(&out.stdout)),
            common::strip_ansi(&String::from_utf8_lossy(&out.stderr)),
        )
    }
}

/// What `tests.yaml` holds for one test with the fetch fixtured: `name`, `risk` and the
/// rest of its keys, each line already indented by two spaces.
fn test_yaml(name: &str, rest: &str) -> String {
    format!(
        "- name: {name}\n  risk: the codes load wrong\n  fixtures:\n    fetch_codes: testdata/codes.csv\n{rest}"
    )
}

#[test]
fn a_passing_test_prints_ok_and_exits_0() {
    let p = Protocol::new(&test_yaml("the codes load", ""));
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.lines().any(|l| l == "ok the codes load"), "{stdout}");
}

#[test]
fn a_refusal_test_passes_when_the_break_triggers_the_message() {
    let p = Protocol::new(&test_yaml(
        "a moved header is refused",
        "  break:\n    fetch_codes: ALTER TABLE fixture RENAME COLUMN code TO kode\n  expect:\n    fails_with: header has moved\n",
    ));
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.lines().any(|l| l == "ok a moved header is refused"),
        "{stdout}"
    );
}

#[test]
fn a_refusal_test_fails_when_the_run_succeeds() {
    let tests = "- name: a moved header is refused\n  risk: the header moves and labels load into the wrong columns\n  fixtures:\n    fetch_codes: testdata/codes.csv\n  break:\n    fetch_codes: DELETE FROM fixture WHERE code = '11'\n  expect:\n    fails_with: header has moved\n";
    let p = Protocol::new(tests);
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let block = "FAIL a moved header is refused\n  \
                 risk: the header moves and labels load into the wrong columns\n  \
                 expected the run to fail with \"header has moved\"; it succeeded\n";
    assert!(stdout.contains(block), "{stdout}");
}

#[test]
fn the_control_catches_a_message_the_break_did_not_cause() {
    let p = Protocol::with(
        &test_yaml(
            "a moved header is refused",
            "  break:\n    fetch_codes: ALTER TABLE fixture RENAME COLUMN code TO kode\n  expect:\n    fails_with: header has moved\n",
        ),
        "http://127.0.0.1:9/never",
        LOAD_ALWAYS_FAILS,
        "",
    );
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains(
            "  the control run, with the fixtures rewritten and no break, also failed with \
             \"header has moved\", so the failure does not come from the break\n"
        ),
        "{stdout}"
    );
}

#[test]
fn an_inline_check_with_rows_fails_the_test_with_its_sample() {
    let p = Protocol::new(&test_yaml(
        "only management codes",
        "  checks:\n    - SELECT code, label FROM codes WHERE code <> '11' ORDER BY code\n",
    ));
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let block = "FAIL only management codes\n  \
                 risk: the codes load wrong\n  \
                 check 1 returned 2 rows: SELECT code, label FROM codes WHERE code <> '11' ORDER BY code\n    \
                 13 | Business\n    \
                 15 | Computing\n";
    assert!(stdout.contains(block), "{stdout}");
}

#[test]
fn params_reach_the_run() {
    let check = "  checks:\n    - SELECT count(*) AS n FROM codes HAVING count(*) < CAST(getenv('ARC_PARAM_MIN_CODES') AS INTEGER)\n";
    let tests = format!(
        "{}{}",
        test_yaml(
            "two codes are enough",
            &format!("{check}  params:\n    min_codes: \"2\"\n")
        ),
        test_yaml(
            "ten codes are not there",
            &format!("{check}  params:\n    min_codes: 10\n")
        ),
    );
    let p = Protocol::with(
        &tests,
        "http://127.0.0.1:9/never",
        LOAD,
        "params:\n  min_codes:\n    default: \"1\"\n",
    );
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.lines().any(|l| l == "ok two codes are enough"),
        "{stdout}"
    );
    assert!(
        stdout.contains("FAIL ten codes are not there\n"),
        "{stdout}"
    );
    assert!(stdout.contains("  check 1 returned 1 rows: "), "{stdout}");
}

#[test]
fn measure_history_from_a_csv_lets_a_comparison_fail() {
    let check = "  checks:\n    - SELECT h.value FROM measure_history h JOIN measures m ON h.name = m.name WHERE m.name = 'size.codes' AND m.value < h.value / 2\n";
    let tests = format!(
        "{}{}",
        test_yaml("no history passes", check),
        test_yaml(
            "a shrunken list fails",
            &format!("{check}  measure_history: testdata/history.csv\n")
        ),
    );
    let p = Protocol::new(&tests);
    p.write(
        "testdata/history.csv",
        "run_id,run_at,name,value\nr1,2026-01-01T00:00:00Z,size.codes,1000\n",
    );
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.lines().any(|l| l == "ok no history passes"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "FAIL a shrunken list fails\n  risk: the codes load wrong\n  check 1 returned 1 rows: "
        ) && stdout.contains("\n    1000.0\n"),
        "{stdout}"
    );
}

#[test]
fn an_ingress_step_without_a_fixture_fails_the_test_and_fetches_nothing() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/never", listener.local_addr().unwrap());
    let tests = "- name: no fixture\n  risk: the test reaches the network\n";
    let p = Protocol::with(tests, &url, LOAD, "");
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains(
            "FAIL no fixture\n  risk: the test reaches the network\n  step 'fetch_codes' fetches data (http_fetch)"
        ),
        "{stdout}"
    );
    assert!(
        listener.accept().is_err(),
        "the test connected to the fetch's address"
    );
}

#[test]
fn the_publish_step_never_runs() {
    let p = Protocol::new(&test_yaml("the codes load", ""));
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let lake = p.root.path().join("lake");
    assert!(!lake.exists(), "the publish step wrote {}", lake.display());
}

#[test]
fn a_run_refused_before_any_step_still_reports() {
    let p = Protocol::with(
        &test_yaml("the codes load", ""),
        "http://127.0.0.1:9/never",
        LOAD,
        "params:\n  release: {}\n",
    );
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(
        stdout.contains(
            "FAIL the codes load\n  risk: the codes load wrong\n  expected the run to succeed; it failed with: missing required parameter 'release'"
        ),
        "{stdout}"
    );
}

/// The entries in `dir` whose names hold `arc-test`.
fn copies_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("arc-test"))
        .collect()
}

#[test]
fn the_copy_is_removed_after_a_passing_and_a_failing_test() {
    let tests = format!(
        "{}{}",
        test_yaml("the codes load", ""),
        test_yaml("a check fails", "  checks:\n    - SELECT code FROM codes\n"),
    );
    let p = Protocol::new(&tests);
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.lines().any(|l| l == "ok the codes load"), "{stdout}");
    assert!(stdout.contains("FAIL a check fails\n"), "{stdout}");
    assert_eq!(copies_in(p.root.path()), Vec::<String>::new());
    assert_eq!(copies_in(&p.dir()), Vec::<String>::new());
    assert!(
        !p.dir().join("build").exists(),
        "the test ran in the Protocol"
    );
}

#[test]
fn a_path_outside_the_protocol_resolves_in_the_copy() {
    let p = Protocol::new(&test_yaml(
        "the shared model runs",
        "  checks:\n    - SELECT * FROM shared_codes WHERE n <> 3\n",
    ));
    let manifest = std::fs::read_to_string(p.dir().join("arcform.yaml")).unwrap();
    let manifest = manifest.replace(
        "  - name: size\n",
        "  - name: shared\n    sql: ../shared/model.sql\n  - name: size\n",
    );
    p.write("arcform.yaml", &manifest);
    let shared = p.root.path().join("shared/model.sql");
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    std::fs::write(
        shared,
        "CREATE OR REPLACE TABLE shared_codes AS SELECT count(*) AS n FROM codes;\n",
    )
    .unwrap();
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.lines().any(|l| l == "ok the shared model runs"),
        "{stdout}"
    );
}

#[test]
fn a_message_in_duckdbs_echo_of_the_sql_does_not_pass_the_test() {
    // The renamed column fails the guard's own binding, and DuckDB's error echoes the
    // guard's SQL, message and all; only the error itself counts.
    let load = "CREATE OR REPLACE TABLE codes AS FROM read_csv('build/raw/codes.csv', all_varchar = true);\n\
SELECT CASE WHEN code IS NULL THEN error('codes header has moved') END FROM codes;\n";
    let p = Protocol::with(
        &test_yaml(
            "a moved header is refused",
            "  break:\n    fetch_codes: ALTER TABLE fixture RENAME COLUMN code TO kode\n  expect:\n    fails_with: header has moved\n",
        ),
        "http://127.0.0.1:9/never",
        load,
        "",
    );
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("FAIL a moved header is refused\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("expected the run to fail with \"header has moved\"; it failed with: "),
        "{stdout}"
    );
    assert!(stdout.contains("Binder Error"), "{stdout}");
    assert!(!stdout.contains("LINE 1:"), "{stdout}");
}

/// `p`'s manifest with `db: <db>` added under its name.
fn with_db(p: &Protocol, db: &str) {
    let manifest = std::fs::read_to_string(p.dir().join("arcform.yaml")).unwrap();
    p.write(
        "arcform.yaml",
        &manifest.replacen("name: codes\n", &format!("name: codes\ndb: {db}\n"), 1),
    );
}

#[test]
fn a_database_outside_the_protocol_is_not_touched() {
    let p = Protocol::new(&test_yaml("the codes load", ""));
    with_db(&p, "../shared.duckdb");
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.lines().any(|l| l == "ok the codes load"), "{stdout}");
    let shared = p.root.path().join("shared.duckdb");
    assert!(!shared.exists(), "the test wrote {}", shared.display());
}

#[test]
fn a_null_database_leaves_nothing_in_arcs_data_folder() {
    let p = Protocol::new(&test_yaml("the codes load", ""));
    with_db(&p, "null");
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let data = p.root.path().join("arc-db");
    let left: Vec<_> = std::fs::read_dir(&data)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "arc's data folder holds {left:?}");
}

#[test]
fn an_error_line_inside_a_steps_failure_does_not_cut_the_message_short() {
    // DuckDB's failure, which arc's error for the step holds whole, has a line of its own
    // that starts `error: `; the message is still all of arc's error.
    let load = "CREATE OR REPLACE TABLE codes AS FROM read_csv('build/raw/codes.csv', all_varchar = true);\n\
SELECT error('codes header has moved' || chr(10) || 'error: aborting') AS guard;\n";
    let p = Protocol::with(
        &test_yaml(
            "a moved header is refused",
            "  expect:\n    fails_with: header has moved\n",
        ),
        "http://127.0.0.1:9/never",
        load,
        "",
    );
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.lines().any(|l| l == "ok a moved header is refused"),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn a_database_reached_through_a_linked_folder_is_not_touched() {
    let elsewhere = tempfile::tempdir().unwrap();
    let p = Protocol::new(&test_yaml("the codes load", ""));
    std::os::unix::fs::symlink(elsewhere.path(), p.dir().join("data")).unwrap();
    with_db(&p, "data/x.duckdb");
    let (code, stdout, stderr) = p.arc_test();
    assert_eq!(code, Some(0), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let target = elsewhere.path().join("x.duckdb");
    assert!(!target.exists(), "the test wrote {}", target.display());
}
