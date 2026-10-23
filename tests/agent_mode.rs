//! arc's own DuckDB processes run out of DuckDB 2.0's agent mode.
//!
//! The 2.0 shell turns on agent mode when the environment says an agent is running it
//! (`AI_AGENT`, `CLAUDECODE`) and its stdout is not a terminal: it prints markdown tables
//! and a banner on stderr, and a failure as JSON. arc starts the shell with
//! `DUCKDB_AGENT_MODE=0`, so a step prints what DuckDB normally prints, and the answers arc
//! asks for are readable. These tests run the built `arc` with an agent's environment.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The `duckdb` on the PATH this test process was started with.
fn real_duckdb() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("duckdb"))
                .find(|c| c.is_file())
        })
        .expect("these tests run a real DuckDB, and found no `duckdb` on PATH")
}

/// A project with one step, `sql`, in a directory of its own.
fn project(sql: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: agent_mode\nengine: duckdb\ndb: agent_mode.duckdb\nsteps:\n  - name: s\n    sql: models/s.sql\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("models/s.sql"), sql).unwrap();
    dir
}

/// `arc run` in `dir`, as an agent runs it, with stdout piped.
fn arc_run_as_agent(dir: &Path, configure: impl FnOnce(&mut Command)) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arc"));
    command
        .current_dir(dir)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .env("AI_AGENT", "x")
        .env("CLAUDECODE", "1")
        .env_remove("DUCKDB_AGENT_MODE")
        .arg("run");
    configure(&mut command);
    command.output().expect("spawn arc run")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_step_run_under_an_agent_prints_duckdbs_normal_output() {
    let dir = project("SELECT 1 AS a;\nCREATE TABLE t AS SELECT 1 AS a;\n");
    let out = arc_run_as_agent(dir.path(), |_| {});
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        !stderr.contains("agent mode"),
        "no agent-mode banner:\n{stderr}"
    );
    assert!(
        !stdout.contains("| a:"),
        "no markdown table with typed headings:\n{stdout}"
    );
}

#[test]
fn a_failing_step_under_an_agent_reports_duckdbs_error_text_not_json() {
    let dir = project("SELECT * FROM missing;\n");
    let out = arc_run_as_agent(dir.path(), |_| {});
    let stderr = text(&out.stderr);
    assert!(!out.status.success(), "the step fails");
    assert!(stderr.contains('s'), "{stderr}");
    assert!(stderr.contains("Catalog Error"), "{stderr}");
    assert!(!stderr.contains("{\"exception_type\""), "{stderr}");
}

#[test]
fn arcs_answers_parse_when_duckdbrc_sets_markdown_mode() {
    let dir = project("SELECT 1 AS a;\n");
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".duckdbrc"),
        ".mode markdown\nSELECT 'hello';\n",
    )
    .unwrap();
    let out = arc_run_as_agent(dir.path(), |c| {
        c.env("HOME", home.path());
    });
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let runs = dir.path().join("build/.arcform/runs");
    let record = std::fs::read_dir(&runs)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .expect("a run record");
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
    assert!(
        record["run"]["engine"]["duckdb_cli"].is_string(),
        "the record names the DuckDB CLI:\n{}",
        record["run"]["engine"]
    );
}

#[test]
fn the_reader_asks_the_program_arc_duckdb_bin_names() {
    let dir = project("CREATE TABLE t AS SELECT 1 AS a;\n");
    let tools = tempfile::tempdir().unwrap();
    let log = tools.path().join("argv.log");
    let wrapper = tools.path().join("wrapped-duckdb");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real_duckdb().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = arc_run_as_agent(dir.path(), |c| {
        c.env("ARC_DUCKDB_BIN", &wrapper)
            .env("ARC_SQL_READER", "duckdb");
    });
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    let calls = std::fs::read_to_string(&log).expect("the wrapper was run");
    assert!(
        calls.lines().any(|l| l.contains("-no-init -json")),
        "the reader's call went through the program ARC_DUCKDB_BIN names:\n{calls}"
    );
}
