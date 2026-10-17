//! What `arc::spec` gives an application that opens a Protocol written by an agent:
//! the Protocol's database path, the tables of its newest run with their content
//! hashes, the run its report names, and its version log.
//!
//!   1. **database** — arc's data folder without a `db:`, the named file with one;
//!   2. **run tables** — the newest run's tables and hashes, from the contract;
//!   3. **report run** — the run id `report.md` names, and none without a report;
//!   4. **log** — every kind of line read, a line that is not the log's skipped.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use arcform::spec::{LOG_FILENAME, LogLine, protocol_db_path, read_log, report_run, run_tables};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn arc(dir: &Path, history: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_HISTORY_DIR", history)
        .env("ARCFORM_DB_DIR", history.join("db"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc");
    {
        let mut pipe = child.stdin.take().expect("stdin is piped");
        if let Some(text) = stdin {
            pipe.write_all(text.as_bytes()).unwrap();
        }
    }
    let out = child.wait_with_output().expect("wait for arc");
    assert!(
        out.status.success(),
        "arc {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The skills fixture with the aggregate step `presenting-findings` records, run once.
fn ran_protocol() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("protocol");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skills"),
        &dir,
    );
    let history = root.path().join("history");
    arc(
        &dir,
        &history,
        &[
            "sql",
            "record",
            "SELECT category, sum(amount) AS total FROM sales GROUP BY category",
            "--name",
            "spend_by_category",
        ],
        None,
    );
    arc(&dir, &history, &["run"], None);
    (root, dir, history)
}

#[test]
fn db_path_follows_an_explicit_db() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: p\nengine: duckdb\ndb: data/x.duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    assert_eq!(
        protocol_db_path(dir.path()).unwrap(),
        dir.path().join("data/x.duckdb")
    );
}

#[test]
fn run_tables_reads_the_newest_run() {
    let (_root, dir, _history) = ran_protocol();
    let runs = dir.join("build/.arcform/runs");
    let newest = std::fs::read_dir(&runs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .max()
        .unwrap();
    let read = run_tables(&dir, None).unwrap();
    assert_eq!(
        read.run_id,
        newest.file_stem().unwrap().to_string_lossy(),
        "{read:?}"
    );
    let hash = read
        .tables
        .get("spend_by_category")
        .expect("the aggregate table");
    assert!(hash.as_ref().is_some_and(|h| !h.is_empty()), "{read:?}");
    assert_eq!(run_tables(&dir, Some(&read.run_id)).unwrap(), read);
    assert!(run_tables(&dir, Some("no-such-run")).is_err());
}

#[test]
fn report_run_names_the_reports_run() {
    let (_root, dir, history) = ran_protocol();
    assert_eq!(report_run(&dir).unwrap(), None);
    arc(
        &dir,
        &history,
        &["report", "write"],
        Some("Spend is even.\n"),
    );
    let run_id = run_tables(&dir, None).unwrap().run_id;
    assert_eq!(report_run(&dir).unwrap(), Some(run_id));
}

#[test]
fn read_log_parses_every_line_kind() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_log(dir.path()).unwrap().is_empty());
    let lines = [
        r#"{"at":"2026-10-17T01:02:03.004Z","file":"arcform.yaml","kind":"save","interface":"mcp","version":"v1","step":"load","change":"added"}"#,
        r#"{"at":"2026-10-17T01:02:04.004Z","file":"arcform.yaml","kind":"checkpoint","interface":"terminal","version":"v2","steps":[{"step":"a","change":"edited"},{"step":"b","change":"removed"}]}"#,
        r#"{"at":"2026-10-17T01:02:05.004Z","file":"panels/a.yaml","kind":"save","version":"v3"}"#,
        "this is not a log line",
        r#"{"at":"2026-10-17T01:02:06.004Z","file":"report.md","kind":"save","interface":"brightfield","version":"v4"}"#,
    ];
    std::fs::write(dir.path().join(LOG_FILENAME), lines.join("\n") + "\n").unwrap();
    let read = read_log(dir.path()).unwrap();
    let line = |at: &str,
                file: &str,
                kind: &str,
                interface: Option<&str>,
                version: &str,
                steps: &[(&str, &str)]| LogLine {
        at: at.into(),
        file: file.into(),
        kind: kind.into(),
        interface: interface.map(Into::into),
        version: version.into(),
        steps: steps
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
    };
    assert_eq!(
        read,
        vec![
            line(
                "2026-10-17T01:02:03.004Z",
                "arcform.yaml",
                "save",
                Some("mcp"),
                "v1",
                &[("load", "added")]
            ),
            line(
                "2026-10-17T01:02:04.004Z",
                "arcform.yaml",
                "checkpoint",
                Some("terminal"),
                "v2",
                &[("a", "edited"), ("b", "removed")]
            ),
            line(
                "2026-10-17T01:02:05.004Z",
                "panels/a.yaml",
                "save",
                None,
                "v3",
                &[]
            ),
            line(
                "2026-10-17T01:02:06.004Z",
                "report.md",
                "save",
                Some("brightfield"),
                "v4",
                &[]
            ),
        ]
    );
}
