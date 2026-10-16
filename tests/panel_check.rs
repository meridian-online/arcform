//! `arc panel check` and the `arc mcp` tool `panel_check`: a panel (a Brightfield spec
//! at `panels/<name>.yaml`) that names a table or column the Protocol does not make is
//! reported, and the same findings come back as warnings when a panel is written.
//!
//!   1. **tables** — a mark's `data.from` is a key of the panel's own `data:` or a
//!      table of the database;
//!   2. **columns** — a bare-string channel is a column of that table; an object
//!      channel is left alone;
//!   3. **before the first run** — with no database, the relations the steps produce
//!      stand in for the tables, and columns are not checked;
//!   4. **write** — a panel with a finding is still written, with the finding as a
//!      warning.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[cfg(feature = "mcp")]
use serde_json::{Value, json};

fn panel(from: &str, channels: &str) -> String {
    format!("plot:\n  - mark: barY\n    data: {{ from: {from} }}\n{channels}")
}

/// A Protocol whose database holds `trips(name, count)`.
fn protocol_with_database() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: p\nengine: duckdb\ndb: data.duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch("CREATE TABLE trips (name VARCHAR, count INTEGER);")
        .unwrap();
    dir
}

/// A Protocol not yet run, whose one step makes two tables.
fn protocol_before_first_run() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("models")).unwrap();
    std::fs::write(
        dir.path().join("models/rank.sql"),
        "CREATE OR REPLACE TABLE details AS SELECT 1 AS id;\n\
         CREATE OR REPLACE TABLE ranking AS SELECT 1 AS id, 2 AS score;\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: p\nengine: duckdb\ndb: data.duckdb\nsteps:\n  - name: rank\n    sql: models/rank.sql\nassets: {}\n",
    )
    .unwrap();
    dir
}

fn put_panel(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir.join("panels")).unwrap();
    std::fs::write(dir.join("panels").join(name), text).unwrap();
}

fn arc(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_HISTORY_DIR", dir.join(".history"))
        .env("ARCFORM_DB_DIR", dir.join(".db"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc");
    {
        let mut pipe = child.stdin.take().unwrap();
        if let Some(text) = stdin {
            pipe.write_all(text.as_bytes()).unwrap();
        }
    }
    child.wait_with_output().expect("wait for arc")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn inline_data_is_not_missing() {
    let dir = protocol_with_database();
    put_panel(
        dir.path(),
        "fruit.yaml",
        "data:\n  fruit:\n    - { name: Apples, count: 30 }\nplot:\n  - mark: barY\n    data: { from: fruit }\n    x: name\n    y: whatever\n",
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_clean_panel_passes() {
    let dir = protocol_with_database();
    put_panel(
        dir.path(),
        "ok.yaml",
        &panel("trips", "    x: name\n    y: count\n    fill: steelblue\n"),
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn missing_table_reported() {
    let dir = protocol_with_database();
    put_panel(dir.path(), "bad.yaml", &panel("rides", "    x: name\n"));
    let out = arc(dir.path(), &["panel", "check"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = stdout(&out);
    assert!(text.contains("panels/bad.yaml"), "{text}");
    assert!(text.contains("rides"), "{text}");
}

#[test]
fn missing_column_reported() {
    let dir = protocol_with_database();
    put_panel(
        dir.path(),
        "bad.yaml",
        &panel("trips", "    x: name\n    y: total\n"),
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = stdout(&out);
    assert!(text.contains("total") && text.contains("trips"), "{text}");
    assert!(!text.contains("column name"), "{text}");
}

#[test]
fn colours_are_not_columns_but_typos_are() {
    let dir = protocol_with_database();
    put_panel(
        dir.path(),
        "colours.yaml",
        &panel(
            "trips",
            "    x: name\n    fill: darkorange\n    stroke: oklch(0.7 0.1 200)\n",
        ),
    );
    put_panel(
        dir.path(),
        "none.yaml",
        &panel("trips", "    x: name\n    fill: none\n    stroke: none\n"),
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert!(out.status.success(), "{out:?}");
    put_panel(
        dir.path(),
        "typo.yaml",
        &panel("trips", "    x: name\n    fill: cuont\n"),
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(stdout(&out).contains("cuont"), "{}", stdout(&out));
}

#[test]
fn a_panel_with_no_plot_is_a_finding() {
    let dir = protocol_with_database();
    put_panel(dir.path(), "empty.yaml", "data:\n  t: []\n");
    let out = arc(dir.path(), &["panel", "check"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(stdout(&out).contains("has no `plot`"), "{}", stdout(&out));
}

#[test]
fn nested_concat_marks_are_checked() {
    let dir = protocol_with_database();
    put_panel(
        dir.path(),
        "concat.yaml",
        "hconcat:\n  - plot:\n      - mark: dot\n        data: { from: rides }\n        x: name\n",
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(stdout(&out).contains("rides"), "{}", stdout(&out));
}

#[test]
fn an_unreadable_panel_is_a_finding_and_the_rest_are_checked() {
    let dir = protocol_with_database();
    put_panel(dir.path(), "a_broken.yaml", "plot: [unclosed\n");
    put_panel(dir.path(), "b_bad.yaml", &panel("rides", "    x: name\n"));
    let out = arc(dir.path(), &["panel", "check"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = stdout(&out);
    assert!(
        text.contains("a_broken.yaml") && text.contains("not valid YAML"),
        "{text}"
    );
    assert!(
        text.contains("b_bad.yaml") && text.contains("rides"),
        "{text}"
    );
}

#[test]
fn expression_channel_not_checked() {
    let dir = protocol_with_database();
    put_panel(
        dir.path(),
        "agg.yaml",
        &panel("trips", "    x: name\n    y: { sum: nothing_here }\n"),
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn a_three_part_name_is_not_checked_before_or_after_a_run() {
    let before = protocol_before_first_run();
    put_panel(
        before.path(),
        "p.yaml",
        &panel("arc.main.nope", "    x: whatever\n"),
    );
    let out = arc(before.path(), &["panel", "check"], None);
    assert!(out.status.success(), "{out:?}");

    let after = protocol_with_database();
    put_panel(
        after.path(),
        "p.yaml",
        &panel("arc.main.trips", "    x: whatever\n"),
    );
    let out = arc(after.path(), &["panel", "check"], None);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn before_first_run_checks_produced_tables() {
    let dir = protocol_before_first_run();
    put_panel(
        dir.path(),
        "second.yaml",
        &panel("ranking", "    x: whatever\n"),
    );
    let out = arc(dir.path(), &["panel", "check"], None);
    assert!(out.status.success(), "{out:?}");
    put_panel(dir.path(), "third.yaml", &panel("nothing", "    x: id\n"));
    let out = arc(dir.path(), &["panel", "check"], None);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = stdout(&out);
    assert!(
        text.contains("third.yaml") && !text.contains("second.yaml"),
        "{text}"
    );
}

#[test]
fn write_file_returns_findings_as_warnings() {
    let dir = protocol_with_database();
    let content = panel("trips", "    x: name\n    y: total\n");
    let out = arc(
        dir.path(),
        &["protocol", "write-file", "panels/new.yaml"],
        Some(&content),
    );
    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("warning:") && stdout(&out).contains("total"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("panels/new.yaml")).unwrap(),
        content
    );
}

#[cfg(feature = "mcp")]
fn call(dir: &Path, name: &str, arguments: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_HISTORY_DIR", dir.join(".history"))
        .env("ARCFORM_DB_DIR", dir.join(".db"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc mcp");
    {
        let mut stdin = child.stdin.take().unwrap();
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(stdin, "{request}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let response: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    response["result"].clone()
}

#[cfg(feature = "mcp")]
#[test]
fn the_tool_reports_what_the_verb_does() {
    let dir = protocol_with_database();
    put_panel(dir.path(), "bad.yaml", &panel("trips", "    y: total\n"));
    let result = call(dir.path(), "panel_check", json!({ "dir": dir.path() }));
    assert_ne!(result["isError"], true, "{result}");
    let panels = &result["structuredContent"]["panels"];
    assert_eq!(panels[0]["panel"], "panels/bad.yaml", "{result}");
    assert!(panels[0]["findings"][0].as_str().unwrap().contains("total"));

    let written = call(
        dir.path(),
        "protocol_write_file",
        json!({ "dir": dir.path(), "path": "panels/w.yaml", "content": panel("trips", "    y: total\n") }),
    );
    assert!(
        written["structuredContent"]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("total")
    );
}
