//! End to end: a URL a step reads or writes is a `remote` asset in the printed graph
//! and the recorded contract, is never reported as a missing file, and makes the step
//! reading it run every time. Nothing here touches the network: the SQL step that
//! reads a URL is declared but never run.

use std::path::Path;
use std::process::{Command, Output};

const SQL_URL: &str = "https://example.invalid/a.csv";
const DEPENDS_URL: &str = "https://example.invalid/b.csv";

/// A project whose command step reads a URL, and, with `with_sql`, a SQL step after it
/// that reads another. The SQL step cannot succeed without the network, so only the
/// graph test includes it.
fn project(with_sql: bool) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let mut manifest = format!(
        "name: remote_probe\nengine: duckdb\ndb: probe.db\nsteps:\n  \
         - name: touch\n    command: \"true\"\n    depends_on: [{DEPENDS_URL}]\n"
    );
    if with_sql {
        manifest.push_str("  - name: load\n    sql: load.sql\n");
    }
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
fn a_url_is_a_remote_node_in_the_graph() {
    let workspace = project(true);
    // The SQL step fails without a network, but the contract is written either way and
    // the graph is a static property of the parsed SQL.
    arc(workspace.path(), &["run"]);

    let runs = workspace.path().join("build/.arcform/runs");
    let json = std::fs::read_dir(&runs)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .expect("one contract");
    let contract: serde_json::Value =
        serde_json::from_slice(&std::fs::read(json).unwrap()).unwrap();
    let assets = contract["assets"].as_array().unwrap();
    for url in [SQL_URL, DEPENDS_URL] {
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
    let workspace = project(false);
    for round in 1..=2 {
        let run = arc(workspace.path(), &["run"]);
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(
            !stdout.contains("[skip"),
            "run {round}: the step reads a URL, so it must not skip:\n{stdout}"
        );
        assert!(
            !stderr.contains("example.invalid"),
            "run {round}: no missing-file warning for a URL:\n{stderr}"
        );
    }
}
