//! `arc report write` and the `arc mcp` tool `report_write`: the agent's prose
//! first, then what arc knows from the run contract, written to `report.md`
//! beside the Protocol and kept in the file's history. Asked of the real `arc`
//! binary.
//!
//!   1. **shape** — the prose, then "How this was derived", "Charts" and "Run";
//!   2. **contract** — row counts, the run and its assets come from the contract;
//!   3. **refuse** — no run yet leaves the folder as it was;
//!   4. **history** — each write is a version of `report.md`;
//!   5. **mcp** — `report_write` writes the bytes the command writes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[cfg(feature = "mcp")]
use serde_json::{Value, json};

struct Protocol {
    _root: tempfile::TempDir,
    dir: PathBuf,
    history: PathBuf,
}

impl Protocol {
    /// A Protocol of two steps that has not run.
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("rates");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(
            dir.join("models/base.sql"),
            "CREATE OR REPLACE TABLE base AS SELECT * FROM range(7) t(id);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("models/top.sql"),
            "CREATE OR REPLACE TABLE top AS SELECT id FROM base WHERE id < 3;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("arcform.yaml"),
            "name: rates\nengine: duckdb\ndb: data.duckdb\nsteps:\n  \
             - name: base\n    sql: models/base.sql\n    description: Seven ids to start from\n  \
             - name: top\n    sql: models/top.sql\nassets: {}\n",
        )
        .unwrap();
        Protocol {
            dir,
            history: root.path().join("history"),
            _root: root,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_arc"));
        command
            .current_dir(&self.dir)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .env("ARCFORM_DB_DIR", self._root.path().join("db"));
        command
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut child = self
            .command()
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
        child.wait_with_output().expect("wait for arc")
    }

    fn arc_run(&self) {
        let out = self.run(&["run"], None);
        assert!(out.status.success(), "arc run: {}", stderr(&out));
    }

    fn write(&self, prose: &str) -> Output {
        self.run(&["report", "write"], Some(prose))
    }

    fn report(&self) -> String {
        std::fs::read_to_string(self.dir.join("report.md")).expect("report.md")
    }

    fn history_lines(&self, file: &str) -> usize {
        let out = self.run(&["history", "list", "--file", file], None);
        assert!(out.status.success(), "history list: {}", stderr(&out));
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|line| line.contains(" bytes  "))
            .count()
    }

    fn tree(&self) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, into: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                into.push(path.strip_prefix(base).unwrap().display().to_string());
                if path.is_dir() && !path.is_symlink() {
                    walk(&path, base, into);
                }
            }
        }
        let mut paths = Vec::new();
        walk(&self.dir, &self.dir, &mut paths);
        paths.sort();
        paths
    }

    /// The one contract file a run left.
    fn contract_path(&self) -> PathBuf {
        let runs = self.dir.join("build/.arcform/runs");
        let mut files: Vec<PathBuf> = std::fs::read_dir(runs)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        assert_eq!(files.len(), 1, "{files:?}");
        files.remove(0)
    }

    #[cfg(feature = "mcp")]
    fn call(&self, arguments: Value) -> Value {
        let mut child = self
            .command()
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn arc mcp");
        {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            let request = json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "report_write", "arguments": arguments },
            });
            writeln!(stdin, "{request}").unwrap();
        }
        let out = child.wait_with_output().expect("wait for arc mcp");
        assert!(out.status.success(), "arc mcp failed: {}", stderr(&out));
        let stdout = String::from_utf8(out.stdout).unwrap();
        let response: Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
        response["result"].clone()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn report_has_prose_then_generated_sections() {
    let protocol = Protocol::new();
    protocol.arc_run();
    std::fs::create_dir(protocol.dir.join("panels")).unwrap();
    std::fs::write(
        protocol.dir.join("panels/top.yaml"),
        "plot:\n  - mark: barY\n    data: { from: top }\n    x: id\n    y: nothing\n",
    )
    .unwrap();
    let out = protocol.write("Three ids sit below the cut.\n\nCaveat: toy data.\n");
    assert!(out.status.success(), "write: {}", stderr(&out));
    let report = protocol.report();
    assert!(
        report.starts_with("Three ids sit below the cut.\n\nCaveat: toy data.\n"),
        "{report}"
    );
    let at = |heading: &str| {
        report
            .find(heading)
            .unwrap_or_else(|| panic!("no {heading} in:\n{report}"))
    };
    assert!(
        at("## How this was derived") < at("## Charts") && at("## Charts") < at("## Run"),
        "{report}"
    );
    assert!(report.contains("panels/top.yaml"), "{report}");
    assert!(
        report.contains("current database, not the run's"),
        "{report}"
    );
    assert!(report.contains("nothing"), "the finding is named: {report}");
    assert!(report.contains("Seven ids to start from"), "{report}");
    assert!(report.contains("success"), "{report}");
}

#[test]
fn row_counts_come_from_contract() {
    let protocol = Protocol::new();
    protocol.arc_run();
    let path = protocol.contract_path();
    let mut contract: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for asset in contract["assets"].as_array_mut().unwrap() {
        if asset["name"] == "top" {
            asset["row_count"] = json_number(4242);
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&contract).unwrap()).unwrap();
    // The database is gone: a count that is printed was not read from it.
    std::fs::remove_dir_all(protocol.dir.join("data.duckdb")).ok();
    std::fs::remove_file(protocol.dir.join("data.duckdb")).ok();
    let out = protocol.write("Prose.\n");
    assert!(out.status.success(), "write: {}", stderr(&out));
    let report = protocol.report();
    assert!(report.contains("4242 rows"), "{report}");
    assert!(report.contains("7 rows"), "{report}");
}

fn json_number(n: i64) -> serde_json::Value {
    serde_json::Value::from(n)
}

#[test]
fn refuses_before_any_run() {
    let protocol = Protocol::new();
    let before = protocol.tree();
    let out = protocol.write("Prose.\n");
    assert!(!out.status.success());
    let message = stderr(&out);
    assert!(
        message.contains("arc run") && message.contains("protocol_run"),
        "{message}"
    );
    assert_eq!(protocol.tree(), before);
}

#[test]
fn named_run_used() {
    let protocol = Protocol::new();
    protocol.arc_run();
    let path = protocol.contract_path();
    let mut older: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let original = older["run"]["run_id"].as_str().unwrap().to_string();
    // A later run, copied from the first and marked as failed.
    let mut newer = older.clone();
    newer["run"]["run_id"] = "20990101-000000-later".into();
    newer["run"]["outcome"] = "failed".into();
    older["run"]["outcome"] = "success".into();
    std::fs::write(&path, serde_json::to_string_pretty(&older).unwrap()).unwrap();
    std::fs::write(
        path.with_file_name("20990101-000000-later.json"),
        serde_json::to_string_pretty(&newer).unwrap(),
    )
    .unwrap();

    let out = protocol.write("Newest.\n");
    assert!(out.status.success(), "{}", stderr(&out));
    let report = protocol.report();
    assert!(report.contains("20990101-000000-later"), "{report}");

    let out = protocol.run(&["report", "write", "--run", &original], Some("Named.\n"));
    assert!(out.status.success(), "{}", stderr(&out));
    let report = protocol.report();
    assert!(
        report.contains(&original) && !report.contains("20990101-000000-later"),
        "{report}"
    );

    let out = protocol.run(&["report", "write", "--run", "nope"], Some("x\n"));
    assert!(!out.status.success());
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));
}

#[test]
fn rewrite_is_a_history_version() {
    let protocol = Protocol::new();
    protocol.arc_run();
    assert!(protocol.write("First.\n").status.success());
    assert_eq!(protocol.history_lines("report.md"), 1);
    assert!(protocol.write("Second.\n").status.success());
    assert_eq!(protocol.history_lines("report.md"), 2);
    assert!(protocol.report().starts_with("Second."));
}

#[test]
fn a_report_never_written_through_arc_is_kept_before_replacement() {
    let protocol = Protocol::new();
    protocol.arc_run();
    std::fs::write(protocol.dir.join("report.md"), "Written by hand.\n").unwrap();
    assert!(protocol.write("Through arc.\n").status.success());
    assert_eq!(protocol.history_lines("report.md"), 2);
}

#[cfg(feature = "mcp")]
#[test]
fn mcp_writes_the_same_bytes_as_the_command() {
    let protocol = Protocol::new();
    protocol.arc_run();
    assert!(protocol.write("Same prose.\n").status.success());
    let by_command = protocol.report();
    let result = protocol.call(json!({ "prose": "Same prose.\n" }));
    assert_ne!(result["isError"], true, "{result}");
    assert_eq!(result["structuredContent"]["path"], "report.md");
    assert_eq!(protocol.report(), by_command);

    let fresh = Protocol::new();
    let result = fresh.call(json!({ "prose": "x" }));
    assert_eq!(result["isError"], true, "{result}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("protocol_run")
    );
}

#[test]
fn a_run_id_that_leaves_the_runs_folder_is_refused() {
    let protocol = Protocol::new();
    protocol.arc_run();
    let before = protocol.tree();
    for id in ["../x", "a/b", ".."] {
        let out = protocol.run(&["report", "write", "--run", id], Some("x\n"));
        assert!(!out.status.success(), "{id}");
        assert_eq!(protocol.tree(), before, "{id}");
    }
}

#[test]
fn newest_run_is_the_greatest_id_not_the_latest_file() {
    let protocol = Protocol::new();
    protocol.arc_run();
    let path = protocol.contract_path();
    let mut older: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    older["run"]["run_id"] = "19990101-000000-old".into();
    // Written last, so the latest file, but the smaller id.
    std::fs::write(
        path.with_file_name("19990101-000000-old.json"),
        serde_json::to_string(&older).unwrap(),
    )
    .unwrap();
    assert!(protocol.write("x\n").status.success());
    assert!(!protocol.report().contains("19990101-000000-old"));
}

#[test]
fn a_locked_database_does_not_stop_the_report() {
    let protocol = Protocol::new();
    protocol.arc_run();
    std::fs::create_dir(protocol.dir.join("panels")).unwrap();
    std::fs::write(
        protocol.dir.join("panels/top.yaml"),
        "title: Top\nplot:\n  - mark: barY\n    data: { from: top }\n    x: id\n",
    )
    .unwrap();
    let db = std::fs::read_dir(&protocol.dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .chain(
            std::fs::read_dir(protocol._root.path().join("db"))
                .into_iter()
                .flatten()
                .map(|e| e.unwrap().path()),
        )
        .find(|p| p.extension().is_some_and(|e| e == "duckdb"));
    let db = db.expect("the run left a database");
    let _held = duckdb::Connection::open(db).unwrap();
    let out = protocol.write("Prose.\n");
    assert!(out.status.success(), "{}", stderr(&out));
    let report = protocol.report();
    assert!(report.contains("Panels not checked"), "{report}");
    assert!(!report.contains("no database yet"), "{report}");
    assert!(report.contains("## Run"), "{report}");
}

#[test]
fn panels_without_a_database_still_write() {
    let protocol = Protocol::new();
    protocol.arc_run();
    std::fs::create_dir(protocol.dir.join("panels")).unwrap();
    std::fs::write(
        protocol.dir.join("panels/top.yaml"),
        "title: >\n  A long\n  title\nplot:\n  - mark: barY\n    data: { from: top }\n",
    )
    .unwrap();
    let _ = std::fs::remove_file(protocol.dir.join("data.duckdb"));
    let _ = std::fs::remove_dir_all(protocol._root.path().join("db"));
    let out = protocol.write("Prose.\n");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        protocol
            .report()
            .contains("`panels/top.yaml`: A long title")
    );
}
