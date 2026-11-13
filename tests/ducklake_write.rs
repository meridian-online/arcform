//! End to end, through the real `arc` binary: a Protocol whose step publishes a table
//! into a DuckLake with `op: ducklake_write@1`.
//!
//! Without the write credential the step publishes into a local lake beside the
//! Protocol and the run stays green; `arc run --publish` makes the live publish
//! required. No test here reaches a live bucket: the one that sets a credential points
//! it at a closed port, to show the values never reach a report or a log line.
//!
//! Needs the `ducklake` DuckDB extension, which the step installs on first use.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::{arc_run, step_outcome, strip_ansi};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/lake/protocol");

/// The credential variables the fixture names, removed from every run's environment
/// unless a test sets them.
const VARS: [&str; 6] = [
    "ARC_TEST_LAKE_KEY_ID",
    "ARC_TEST_LAKE_SECRET",
    "ARC_TEST_LAKE_ACCOUNT_ID",
    "ARC_TEST_LAKE_ENDPOINT",
    "ARC_TEST_LAKE_REGION",
    "ARC_PUBLISH",
];

/// A copy of the fixture Protocol with `rows` rows in its input, labels prefixed `label`.
fn project(rows: u32, label: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in ["arcform.yaml", "codes.datapackage.json"] {
        std::fs::copy(Path::new(FIXTURE).join(name), dir.path().join(name)).unwrap();
    }
    build(dir.path(), rows, label);
    dir
}

/// Write the step's input: `rows` codes, each labelled `<label> <n>`.
fn build(project: &Path, rows: u32, label: &str) {
    let out = project.join("data/codes.parquet");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT printf('c%03d', range) AS code, '{label} ' || range AS label \
         FROM range({rows}) ORDER BY code) TO '{}' (FORMAT parquet);",
        out.display()
    ))
    .unwrap();
}

/// Replace the fixture's `credential:` block, which ends the manifest.
fn set_credential(project: &Path, block: &str) {
    let path = project.join("arcform.yaml");
    let manifest = std::fs::read_to_string(&path).unwrap();
    let (head, _) = manifest.split_once("      credential:\n").unwrap();
    std::fs::write(&path, format!("{head}{block}")).unwrap();
}

/// One `arc run` with extra arguments and environment, returning (exit, stdout, stderr).
fn arc(project: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arc"));
    cmd.current_dir(project)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .arg("run")
        .args(args);
    for var in VARS {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn arc run");
    (
        out.status.code(),
        strip_ansi(&String::from_utf8_lossy(&out.stdout)),
        strip_ansi(&String::from_utf8_lossy(&out.stderr)),
    )
}

fn contracts(project: &Path) -> BTreeSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(project.join("build/.arcform/runs")) else {
        return BTreeSet::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
}

/// The run record the run just wrote: the one file in `after` not in `before`.
fn new_record(before: &BTreeSet<PathBuf>, after: &BTreeSet<PathBuf>) -> PathBuf {
    let new: Vec<_> = after.difference(before).collect();
    assert_eq!(new.len(), 1, "one run writes one record: {new:?}");
    new[0].clone()
}

/// The `publish` step's entry in a run record.
fn publish_entry(record: &Path) -> serde_json::Value {
    let contract: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
    contract["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "publish")
        .expect("a publish step in the record")
        .clone()
}

/// One `arc run` that must succeed and run the publish, returning its report.
fn publish(project: &Path) -> serde_json::Value {
    let before = contracts(project);
    let stdout = arc_run(project);
    assert_eq!(step_outcome(&stdout, "publish"), "ran");
    publish_entry(&new_record(&before, &contracts(project)))["report"].clone()
}

/// Read the local lake, attached read-only, through a connection of the test's own.
fn lake<T>(project: &Path, read: impl FnOnce(&duckdb::Connection) -> T) -> T {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "LOAD ducklake; ATTACH 'ducklake:{}' AS lake (READ_ONLY);",
        project.join("build/lake/test.ducklake").display()
    ))
    .unwrap();
    read(&conn)
}

fn parquet_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .map(|e| e.unwrap().path())
        .map(|p| {
            if p.is_dir() {
                parquet_files(&p)
            } else {
                usize::from(p.extension().is_some_and(|x| x == "parquet"))
            }
        })
        .sum()
}

fn strings(conn: &duckdb::Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn without_a_credential_the_run_publishes_locally_and_is_green() {
    let dir = project(5, "first");
    let p = dir.path();

    let before = contracts(p);
    let (code, stdout, stderr) = arc(p, &[], &[]);
    assert_eq!(code, Some(0), "arc run failed:\n{stdout}\n{stderr}");
    let report = publish_entry(&new_record(&before, &contracts(p)))["report"].clone();

    assert_eq!(report["target"], "local", "{report}");
    assert_eq!(
        report["missing_credential"],
        serde_json::json!([
            "ARC_TEST_LAKE_KEY_ID",
            "ARC_TEST_LAKE_SECRET",
            "ARC_TEST_LAKE_ACCOUNT_ID"
        ]),
        "{report}"
    );
    assert!(report.get("run_record").is_none(), "{report}");
    assert_eq!(report["committed"], true, "{report}");
    assert_eq!(report["tables"][0]["table"], "demo.codes", "{report}");
    assert!(
        stderr.contains(
            "ducklake_write: published locally to build/lake/test.ducklake; the live lake \
             needs ARC_TEST_LAKE_KEY_ID, ARC_TEST_LAKE_SECRET, ARC_TEST_LAKE_ACCOUNT_ID set"
        ),
        "{stderr}"
    );

    let codes = lake(p, |c| {
        strings(c, "SELECT code FROM lake.demo.codes ORDER BY code")
    });
    assert_eq!(codes, ["c000", "c001", "c002", "c003", "c004"]);
    let described = lake(p, |c| {
        strings(c, "SELECT table_name FROM lake.meta.descriptors")
    });
    assert_eq!(described, ["demo.codes"]);
}

#[test]
fn with_publish_a_missing_credential_fails_the_step() {
    let dir = project(5, "first");
    let p = dir.path();
    // One variable set, so the refusal names only the two that are not, and holds no
    // value of the one that is.
    let (code, stdout, stderr) = arc(
        p,
        &["--publish"],
        &[("ARC_TEST_LAKE_ACCOUNT_ID", "sentinel-account-value")],
    );
    assert_ne!(
        code,
        Some(0),
        "a required publish without a credential must fail"
    );
    assert!(
        stderr.contains("ARC_TEST_LAKE_KEY_ID") && stderr.contains("ARC_TEST_LAKE_SECRET"),
        "the refusal names the unset variables:\n{stderr}"
    );
    assert!(
        !stderr.contains("ARC_TEST_LAKE_ACCOUNT_ID"),
        "only the unset variables are named:\n{stderr}"
    );
    assert!(
        !stderr.contains("sentinel-account-value") && !stdout.contains("sentinel-account-value"),
        "no value is printed:\n{stdout}\n{stderr}"
    );
    assert!(
        !p.join("build/lake/test.ducklake").exists(),
        "a required publish never falls back to the local lake"
    );
}

#[test]
fn a_local_lake_reads_back() {
    let dir = project(50, "first");
    let p = dir.path();

    let first = publish(p);
    assert_eq!(first["committed"], true, "{first}");
    let first_snapshot = first["snapshot_id"].as_i64().unwrap();

    // A change to every row's label, and one row gone.
    build(p, 49, "second");
    let second = publish(p);
    assert_eq!(second["committed"], true, "{second}");
    let second_snapshot = second["snapshot_id"].as_i64().unwrap();
    assert!(second_snapshot > first_snapshot, "{first}\n{second}");
    assert_eq!(second["tables"][0]["updated"], 49, "{second}");
    assert_eq!(second["tables"][0]["deleted"], 1, "{second}");

    lake(p, |c| {
        let now = strings(c, "SELECT label FROM lake.demo.codes ORDER BY code");
        assert_eq!(now.len(), 49);
        assert!(now.iter().all(|l| l.starts_with("second ")), "{now:?}");

        let then = strings(
            c,
            &format!(
                "SELECT label FROM lake.demo.codes AT (VERSION => {first_snapshot}) ORDER BY code"
            ),
        );
        assert_eq!(then.len(), 50);
        assert!(then.iter().all(|l| l.starts_with("first ")), "{then:?}");
    });
    // 50 rows are past DuckLake's inlining limit: the publishes wrote data files under
    // the local lake's own folder, which is where the reader above found them.
    assert!(
        parquet_files(&p.join("build/lake/test.ducklake.files")) > 0,
        "the publishes wrote data files"
    );
}

#[test]
fn the_report_and_logs_never_contain_a_credential_value() {
    let dir = project(5, "first");
    let p = dir.path();
    set_credential(
        p,
        "      credential:
        type: s3
        bucket: test-bucket
        env:
          key_id: ARC_TEST_LAKE_KEY_ID
          secret: ARC_TEST_LAKE_SECRET
          endpoint: ARC_TEST_LAKE_ENDPOINT
          region: ARC_TEST_LAKE_REGION
",
    );
    // The credential comes from a `.env` file the manifest lists, as an owner's does.
    let manifest = std::fs::read_to_string(p.join("arcform.yaml")).unwrap();
    std::fs::write(
        p.join("arcform.yaml"),
        manifest.replace(
            "db: build/pipeline.db\n",
            "db: build/pipeline.db\ndotenv: [.env]\n",
        ),
    )
    .unwrap();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    std::fs::write(
        p.join(".env"),
        format!(
            "ARC_TEST_LAKE_KEY_ID=sentinel-key-id-4f1c\n\
             ARC_TEST_LAKE_SECRET=sentinel-secret-9b2e\n\
             ARC_TEST_LAKE_ENDPOINT=http://127.0.0.1:{port}\n\
             ARC_TEST_LAKE_REGION=us-east-1\n"
        ),
    )
    .unwrap();

    let before = contracts(p);
    let (code, stdout, stderr) = arc(p, &[], &[]);
    assert_ne!(code, Some(0), "a live publish to a closed port fails");
    assert!(
        stderr.contains("catalog/test.ducklake"),
        "the run went live, and its failure names the catalogue:\n{stderr}"
    );
    let record = std::fs::read_to_string(new_record(&before, &contracts(p))).unwrap();
    for sentinel in ["sentinel-key-id-4f1c", "sentinel-secret-9b2e"] {
        assert!(
            !stderr.contains(sentinel),
            "stderr holds {sentinel}:\n{stderr}"
        );
        assert!(
            !stdout.contains(sentinel),
            "stdout holds {sentinel}:\n{stdout}"
        );
        assert!(
            !record.contains(sentinel),
            "the run record holds {sentinel}:\n{record}"
        );
    }
    assert!(
        !p.join("build/lake/test.ducklake").exists(),
        "a live run never writes the local lake"
    );
}

#[test]
fn publish_after_a_local_run_still_requires_the_credential() {
    let dir = project(5, "first");
    let p = dir.path();
    let (code, stdout, stderr) = arc(p, &[], &[]);
    assert_eq!(code, Some(0), "arc run failed:\n{stdout}\n{stderr}");

    // Nothing changed since, but the local publish is not the live one: the step runs
    // again, and without the credential a required publish fails.
    let (code, stdout, stderr) = arc(p, &["--publish"], &[]);
    assert_ne!(
        code,
        Some(0),
        "a required publish after a local one must not pass as fresh:\n{stdout}\n{stderr}"
    );
    assert_eq!(step_outcome(&stdout, "publish"), "ran");
    assert!(
        stderr.contains("ARC_TEST_LAKE_KEY_ID") && stderr.contains("ARC_TEST_LAKE_SECRET"),
        "{stderr}"
    );
}
