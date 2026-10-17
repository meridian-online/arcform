//! `ARCFORM_HISTORY_WAY` names the interface the command line records its history
//! under, for a caller that runs `arc` as a child process.

use std::process::{Command, Output};

fn record(way: &str) -> (tempfile::TempDir, Output) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("protocol");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("arcform.yaml"),
        "name: p\nengine: duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&dir)
        .env("ARCFORM_HISTORY_DIR", root.path().join("history"))
        .env("ARCFORM_HISTORY_WAY", way)
        .args(["sql", "record", "SELECT 1 AS x", "--name", "t"])
        .output()
        .expect("spawn arc");
    (root, out)
}

#[test]
fn the_env_names_the_cli_interface() {
    let (root, out) = record("brightfield");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = std::fs::read_to_string(root.path().join("protocol/arcform-log.txt")).unwrap();
    assert!(log.contains(r#""interface":"brightfield""#), "{log}");

    let (root, out) = record("");
    assert!(out.status.success());
    let log = std::fs::read_to_string(root.path().join("protocol/arcform-log.txt")).unwrap();
    assert!(log.contains(r#""interface":"terminal""#), "{log}");

    let (_root, out) = record("mcp");
    assert!(!out.status.success());
    let message = String::from_utf8_lossy(&out.stderr);
    assert!(message.contains("ARCFORM_HISTORY_WAY"), "{message}");
}
