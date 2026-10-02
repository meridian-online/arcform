//! A file beside `arcform.yaml` that may be a copy a synced drive set aside, asked of
//! the real `arc` binary.
//!
//! When two people save a Protocol shared through Dropbox or OneDrive, the drive keeps
//! one save as `arcform.yaml` and sets the other aside under a name of its own: Dropbox's
//! `arcform (conflicted copy).yaml`, OneDrive's `arcform-LAPTOP.yaml`. arc runs the file
//! it loads by name and reads neither. These tests pin that a run says so:
//!
//!   1. **named** — such a file is named on standard error before a step runs, in one
//!      line that says arc did not read it and that it may be a set-aside copy;
//!   2. **by name** — the rule is the name: it starts with `arcform`, ends with `.yaml`
//!      and is not `arcform.yaml`, so a name neither drive gives is named too;
//!   3. **unchanged** — the run exits, prints and leaves its tables as it does without
//!      the file;
//!   4. **quiet** — a directory with no such file, or only `arcform.yaml.bak` or
//!      `my-arcform.yaml`, raises no line;
//!   5. **an agent is told** — `arc mcp`'s `protocol_run` carries the same warning.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::strip_ansi;

/// The Dropbox name: <https://help.dropbox.com/organize/conflicted-copy>.
const DROPBOX_COPY: &str = "arcform (conflicted copy).yaml";
/// The OneDrive name: <https://learn.microsoft.com/en-us/troubleshoot/sharepoint/sync/troubleshoot-sync-issues>.
const ONEDRIVE_COPY: &str = "arcform-LAPTOP.yaml";
/// A name neither vendor gives, of the same shape.
const OTHER_COPY: &str = "arcform old.yaml";

/// What a step writes to standard error when it runs, so a test can tell whether a
/// line came before it.
const STEP_MARKER: &str = "THE-STEP-RAN";

/// A Protocol with one command step that announces itself on standard error.
const SPEAKING_PROTOCOL: &str = "name: speaking\n\
db: speaking.duckdb\n\
steps:\n  \
- name: speak\n    \
command: \"echo THE-STEP-RAN >&2\"\n";

/// A Protocol whose one SQL step builds a table.
const TABLE_PROTOCOL: &str = "name: tables\n\
db: tables.duckdb\n\
steps:\n  \
- name: build\n    \
sql: build.sql\n";

/// What a drive might set aside beside [`TABLE_PROTOCOL`]: a colleague's save with a
/// second step and a second table, so a run that read it would leave another table.
const TABLE_PROTOCOL_COPY: &str = "name: tables\n\
db: tables.duckdb\n\
steps:\n  \
- name: build\n    \
sql: build.sql\n  \
- name: colleague\n    \
sql: colleague.sql\n";

/// Write `files` into a fresh directory.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write a project file");
    }
    dir
}

/// One `arc run` in `dir`.
fn arc_run(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .arg("run")
        .output()
        .expect("spawn arc run")
}

/// Standard error with its styling dropped.
fn stderr(out: &Output) -> String {
    strip_ansi(&String::from_utf8_lossy(&out.stderr))
}

/// The lines of `stderr` that name a file arc did not read.
fn copy_lines(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| l.contains("did not read"))
        .map(str::to_string)
        .collect()
}

/// The one line a run in `dir`, holding the speaking Protocol and `copy`, writes for
/// `copy`; the run has to succeed, and the line has to come before the step's output.
fn line_for(copy: &str) -> String {
    let dir = project(&[
        ("arcform.yaml", SPEAKING_PROTOCOL),
        (copy, SPEAKING_PROTOCOL),
    ]);
    let out = arc_run(dir.path());
    let err = stderr(&out);
    assert!(out.status.success(), "arc run failed:\n{err}");
    let lines = copy_lines(&err);
    assert_eq!(lines.len(), 1, "one line for `{copy}`, in:\n{err}");
    let line = &lines[0];
    let at_line = err.find(line.as_str()).expect("the line is in stderr");
    let at_step = err
        .find(STEP_MARKER)
        .unwrap_or_else(|| panic!("the step never ran:\n{err}"));
    assert!(
        at_line < at_step,
        "the line for `{copy}` came after the step ran:\n{err}"
    );
    line.clone()
}

#[test]
fn a_dropbox_conflicted_copy_is_named_on_stderr_before_a_step_runs() {
    let line = line_for(DROPBOX_COPY);
    assert!(line.starts_with("warning: "), "not a warning line: {line}");
    assert!(
        line.contains(&format!("`{DROPBOX_COPY}`")),
        "the line does not name the file: {line}"
    );
    assert!(
        line.contains("arc did not read it"),
        "the line does not say arc did not read it: {line}"
    );
    assert!(
        line.contains("may be a copy a synced drive") && line.contains("set aside"),
        "the line does not say it may be a copy a drive set aside: {line}"
    );
}

#[test]
fn a_onedrive_copy_and_a_name_neither_vendor_gives_raise_the_same_line() {
    let dropbox = line_for(DROPBOX_COPY);
    for copy in [ONEDRIVE_COPY, OTHER_COPY] {
        assert_eq!(
            line_for(copy),
            dropbox.replace(DROPBOX_COPY, copy),
            "`{copy}` does not raise the line `{DROPBOX_COPY}` does"
        );
    }
}

/// Each table in the run's database, by schema and name, with its rows in order. arc's
/// own run state, whose rows carry the run's id and times, is compared by name alone.
fn tables(db: &Path) -> Vec<(String, Vec<String>)> {
    let conn = duckdb::Connection::open(db).expect("open the run's database");
    let mut stmt = conn
        .prepare(
            "SELECT table_schema, table_name FROM information_schema.tables \
             ORDER BY table_schema, table_name",
        )
        .expect("prepare");
    let names: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("list tables")
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|(schema, name)| {
            let rows = if schema == "main" && !name.starts_with("_arc") {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT CAST(t AS VARCHAR) FROM \"{schema}\".\"{name}\" t ORDER BY ALL"
                    ))
                    .expect("prepare a read of a table");
                stmt.query_map([], |r| r.get::<_, String>(0))
                    .expect("read a table")
                    .map(Result::unwrap)
                    .collect()
            } else {
                Vec::new()
            };
            (format!("{schema}.{name}"), rows)
        })
        .collect()
}

#[test]
fn a_run_beside_a_copy_exits_prints_and_builds_as_it_does_without_it() {
    let sql = [
        (
            "build.sql",
            "CREATE OR REPLACE TABLE kept AS SELECT 1 AS id, 'a' AS v;\n",
        ),
        (
            "colleague.sql",
            "CREATE OR REPLACE TABLE theirs AS SELECT 2 AS id;\n",
        ),
    ];
    let mut without: Vec<(&str, &str)> = vec![("arcform.yaml", TABLE_PROTOCOL)];
    without.extend(sql);
    let mut with = without.clone();
    with.push((DROPBOX_COPY, TABLE_PROTOCOL_COPY));

    let plain = project(&without);
    let beside = project(&with);
    let plain_out = arc_run(plain.path());
    let beside_out = arc_run(beside.path());

    assert!(
        plain_out.status.success(),
        "the run without the copy failed:\n{}",
        stderr(&plain_out)
    );
    assert_eq!(
        copy_lines(&stderr(&beside_out)).len(),
        1,
        "the run beside the copy did not name it:\n{}",
        stderr(&beside_out)
    );
    assert_eq!(
        beside_out.status.code(),
        plain_out.status.code(),
        "the copy changed how the run exits"
    );
    assert_eq!(
        String::from_utf8_lossy(&beside_out.stdout),
        String::from_utf8_lossy(&plain_out.stdout),
        "the copy changed what the run writes to standard output"
    );
    let plain_tables = tables(&plain.path().join("tables.duckdb"));
    assert!(
        plain_tables
            .iter()
            .any(|(name, rows)| name == "main.kept" && rows.len() == 1),
        "the run built no `kept` table: {plain_tables:?}"
    );
    assert_eq!(
        tables(&beside.path().join("tables.duckdb")),
        plain_tables,
        "the copy changed the tables the run leaves"
    );
}

#[test]
fn a_failing_run_beside_a_copy_exits_as_it_does_without_it() {
    let failing =
        "name: failing\ndb: failing.duckdb\nsteps:\n  - name: fail\n    command: \"exit 3\"\n";
    let plain = project(&[("arcform.yaml", failing)]);
    let beside = project(&[("arcform.yaml", failing), (DROPBOX_COPY, SPEAKING_PROTOCOL)]);
    let plain_out = arc_run(plain.path());
    let beside_out = arc_run(beside.path());
    assert!(!plain_out.status.success(), "the failing step passed");
    assert_eq!(
        copy_lines(&stderr(&beside_out)).len(),
        1,
        "the failing run beside the copy did not name it:\n{}",
        stderr(&beside_out)
    );
    assert_eq!(
        beside_out.status.code(),
        plain_out.status.code(),
        "the copy changed how a failing run exits"
    );
    assert_eq!(
        String::from_utf8_lossy(&beside_out.stdout),
        String::from_utf8_lossy(&plain_out.stdout),
        "the copy changed what a failing run writes to standard output"
    );
}

#[test]
fn no_line_without_a_copy_nor_for_a_backup_or_a_name_that_does_not_start_with_arcform() {
    for others in [
        &[][..],
        &[("arcform.yaml.bak", SPEAKING_PROTOCOL)][..],
        &[("my-arcform.yaml", SPEAKING_PROTOCOL)][..],
    ] {
        let mut files = vec![("arcform.yaml", SPEAKING_PROTOCOL)];
        files.extend_from_slice(others);
        let dir = project(&files);
        let out = arc_run(dir.path());
        let err = stderr(&out);
        assert!(
            out.status.success(),
            "arc run failed beside {others:?}:\n{err}"
        );
        assert!(
            err.contains(STEP_MARKER),
            "the step never ran beside {others:?}:\n{err}"
        );
        assert_eq!(
            copy_lines(&err),
            Vec::<String>::new(),
            "a line was raised beside {others:?}"
        );
    }
}

#[cfg(feature = "mcp")]
mod mcp {
    use std::io::Write;
    use std::process::Stdio;

    use serde_json::{Value, json};

    use super::*;

    /// Call `protocol_run` on `dir` through the real `arc mcp` server over stdio and
    /// return the `result` it answers with.
    ///
    /// The run's own standard output reaches the server's standard output beside the
    /// response, so the response is the one line that parses as a JSON-RPC message.
    fn protocol_run(dir: &Path) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(dir)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn arc mcp");
        {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            let request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "protocol_run",
                    "arguments": { "dir": dir.to_str().expect("a UTF-8 path") },
                },
            });
            writeln!(stdin, "{request}").expect("write the request");
        }
        let out = child.wait_with_output().expect("wait for arc mcp");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let response: Value = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["id"] == 1)
            .unwrap_or_else(|| panic!("no response to the call in:\n{stdout}"));
        response["result"].clone()
    }

    #[test]
    fn protocol_run_carries_the_warning_arc_run_prints_and_none_without_a_copy() {
        let files = [
            ("arcform.yaml", SPEAKING_PROTOCOL),
            (DROPBOX_COPY, SPEAKING_PROTOCOL),
        ];
        let line = copy_lines(&stderr(&arc_run(project(&files).path())))
            .pop()
            .expect("arc run names the copy");
        let printed = line.strip_prefix("warning: ").expect("a warning line");

        let beside = project(&files);
        let result = protocol_run(beside.path());
        assert_eq!(result["isError"], false, "protocol_run failed: {result}");
        let copies = &result["structuredContent"]["run"]["protocol"]["possible_copies"];
        assert_eq!(
            copies,
            &json!([{ "file": DROPBOX_COPY, "warning": printed }]),
            "protocol_run does not carry the warning arc run prints"
        );

        let plain = project(&[("arcform.yaml", SPEAKING_PROTOCOL)]);
        let result = protocol_run(plain.path());
        assert_eq!(result["isError"], false, "protocol_run failed: {result}");
        assert_eq!(
            result["structuredContent"]["run"]["protocol"]["possible_copies"],
            json!([]),
            "protocol_run names a copy where there is none"
        );
        assert!(
            !result.to_string().contains("did not read"),
            "protocol_run carries a warning where there is no copy: {result}"
        );
    }

    #[test]
    fn protocol_run_names_the_copy_when_the_run_writes_no_record() {
        // A Protocol with no steps runs and writes no record, so the tool answers with
        // an error; the copy beside it is named there instead.
        let empty = "name: empty\nsteps: []\n";
        let beside = project(&[("arcform.yaml", empty), (DROPBOX_COPY, SPEAKING_PROTOCOL)]);
        let result = protocol_run(beside.path());
        assert_eq!(
            result["isError"], true,
            "a run with no record is an error: {result}"
        );
        let text = result["content"][0]["text"]
            .as_str()
            .expect("an error text");
        assert!(
            text.contains(&format!("`{DROPBOX_COPY}`")) && text.contains("did not read it"),
            "the error does not name the copy: {text}"
        );

        let plain = project(&[("arcform.yaml", empty)]);
        let result = protocol_run(plain.path());
        let text = result["content"][0]["text"]
            .as_str()
            .expect("an error text");
        assert!(
            !text.contains("did not read"),
            "the error names a copy where there is none: {text}"
        );
    }
}
