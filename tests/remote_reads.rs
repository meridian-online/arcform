//! End to end: a URL a step reads or writes is a `remote` asset in the recorded
//! contract, is never reported as a missing file, and makes the step reading it run
//! every time. Nothing here touches the network: the SQL step that reads a URL sits
//! behind a step that fails, so it is declared and in the graph but never run.

use std::path::Path;
use std::process::{Command, Output};

const SQL_URL: &str = "https://example.invalid/a.csv";
const DEPENDS_URL: &str = "https://example.invalid/b.csv";
const PRODUCES_URL: &str = "s3://example-bucket/out.parquet";

/// The command step both tests are about: it reads and produces a URL, and is gated by
/// a precondition that is never fresh (its file is old), so only the URLs can decide
/// whether it runs. `preface` adds steps before it.
fn project(preface: &str, tail: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let gate = workspace.path().join("gate.txt");
    std::fs::write(&gate, "old").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&gate)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600))
        .unwrap();
    let manifest = format!(
        "name: remote_probe\nengine: duckdb\ndb: probe.db\nsteps:\n{preface}  \
         - name: touch\n    command: \"true\"\n    depends_on: [{DEPENDS_URL}]\n    \
         produces: [{PRODUCES_URL}]\n    preconditions:\n      \
         - modified_after: {{ path: gate.txt, period: 24h }}\n{tail}"
    );
    std::fs::write(workspace.path().join("arcform.yaml"), manifest).unwrap();
    std::fs::write(
        workspace.path().join("load.sql"),
        format!("CREATE OR REPLACE TABLE t AS SELECT * FROM '{SQL_URL}';"),
    )
    .unwrap();
    workspace
}

fn arc(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .args(args)
        .output()
        .expect("spawn arc")
}

#[test]
fn a_url_is_a_remote_node_in_the_recorded_contract() {
    // `halt` fails first, so the run stops before `touch` and `load`; the contract is
    // written either way, and the graph is a property of the parsed manifest and SQL.
    let workspace = project(
        "  - name: halt\n    command: \"false\"\n",
        "  - name: load\n    sql: load.sql\n",
    );
    let run = arc(workspace.path(), &["run"]);
    assert!(!run.status.success(), "halt must fail so load never runs");

    let runs = workspace.path().join("build/.arcform/runs");
    let json = std::fs::read_dir(&runs)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .expect("one contract");
    let contract: serde_json::Value =
        serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
    let assets = contract["assets"].as_array().unwrap();
    for url in [SQL_URL, DEPENDS_URL, PRODUCES_URL] {
        let asset = assets
            .iter()
            .find(|a| a["name"] == url)
            .unwrap_or_else(|| panic!("{url} is a node of the graph: {assets:?}"));
        assert_eq!(asset["kind"], "remote", "{url}");
        assert!(asset["path"].is_null(), "{url} has no path on disk");
        assert!(asset["content_hash"].is_null(), "{url} is never hashed");
    }
}

#[test]
fn a_step_reading_a_url_runs_every_time_without_a_missing_file_warning() {
    let workspace = project("", "");
    let gate = workspace.path().join("gate.txt");
    for round in 1..=3 {
        let run = arc(workspace.path(), &["run"]);
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            !stdout.contains("[skip"),
            "run {round}: the step reads a URL, so it must not skip:\n{stdout}"
        );
        assert!(
            !stderr.contains("example.invalid") && !stderr.contains("example-bucket"),
            "run {round}: no missing-file warning for a URL:\n{stderr}"
        );
        // After the first run has recorded a baseline, make the precondition fresh, so
        // that only the URLs can send the next run back to work.
        std::fs::File::options()
            .write(true)
            .open(&gate)
            .unwrap()
            .set_modified(std::time::SystemTime::now())
            .unwrap();
    }
}
