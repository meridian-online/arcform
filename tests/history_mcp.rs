//! The `arc mcp` tools `protocol_edit`, `history_list` and `history_restore`,
//! asked of the real `arc` binary and held to what the commands do:
//!
//!   1. **edit** — `protocol_edit` writes the bytes `arc edit-protocol` writes
//!      and returns the edited step list; a refused edit leaves the folder as it
//!      was;
//!   2. **list** — `history_list` returns the entries `arc history list --json`
//!      prints, for the spec or for one file beside it;
//!   3. **restore** — `history_restore` rolls the spec, or one file, back to an
//!      entry and says what it restored.
//!
//! Every write is a version of the Protocol, so each run points
//! `ARCFORM_HISTORY_DIR` at a directory of its own, outside the Protocol.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use arcform::spec::LocalHistory;
use serde_json::{Value, json};

struct Protocol {
    _root: tempfile::TempDir,
    dir: PathBuf,
    history: PathBuf,
}

impl Protocol {
    /// A Protocol made by `arc create-protocol`.
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let protocol = Protocol {
            dir: root.path().join("rates"),
            history: root.path().join("history"),
            _root: root,
        };
        let out = protocol.arc_from(protocol._root.path(), &["create-protocol", "rates"]);
        assert!(out.status.success(), "create-protocol: {}", stderr(&out));
        protocol
    }

    fn arc_from(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(cwd)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .args(args)
            .output()
            .expect("spawn arc")
    }

    fn arc(&self, args: &[&str]) -> Output {
        self.arc_from(&self.dir, args)
    }

    /// A SQL step recorded by the command.
    fn record(&self, name: &str) {
        let out = self.arc(&["sql", "record", "SELECT 1 AS x", "--name", name]);
        assert!(out.status.success(), "sql record: {}", stderr(&out));
    }

    fn manifest(&self) -> Vec<u8> {
        std::fs::read(self.dir.join("arcform.yaml")).unwrap()
    }

    /// One `tools/call` to a fresh `arc mcp` started in the Protocol's directory.
    fn call(&self, tool: &str, arguments: Value) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(&self.dir)
            .env("ARCFORM_HISTORY_DIR", &self.history)
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
                "params": { "name": tool, "arguments": arguments },
            });
            writeln!(stdin, "{request}").unwrap();
        }
        let out = child.wait_with_output().expect("wait for arc mcp");
        assert!(out.status.success(), "arc mcp failed: {}", stderr(&out));
        let stdout = String::from_utf8(out.stdout).unwrap();
        let response: Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
        response["result"].clone()
    }

    /// The JSON a successful tool call returns.
    fn call_ok(&self, tool: &str, arguments: Value) -> Value {
        let result = self.call(tool, arguments);
        assert_ne!(result["isError"], true, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn protocol_edit_matches_cli() {
    let by_command = Protocol::new();
    let by_tool = Protocol::new();
    for protocol in [&by_command, &by_tool] {
        protocol.record("first");
    }
    let out = by_command.arc(&[
        "edit-protocol",
        "add",
        "steps[0]",
        "description",
        "The first step",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));

    let steps = by_tool.call_ok(
        "protocol_edit",
        json!({
            "edit": "add",
            "path": "steps[0]",
            "key": "description",
            "value": "The first step",
        }),
    );
    assert_eq!(by_command.manifest(), by_tool.manifest());
    assert_eq!(steps[0]["name"], "first");
    assert_eq!(steps[0]["description"], "The first step");
}

#[test]
fn protocol_edit_takes_every_edit() {
    let protocol = Protocol::new();
    protocol.record("first");
    protocol.record("second");
    let edits = [
        json!({ "edit": "replace", "path": "name", "value": "renamed" }),
        json!({ "edit": "reorder", "path": "steps", "from": 1, "to": 0 }),
        json!({ "edit": "add", "path": "steps[0]", "key": "description", "value": "Second now" }),
        json!({ "edit": "rewrite", "path": "steps[0].description", "from": "now", "to": "first" }),
        json!({ "edit": "delete", "path": "steps[1]" }),
    ];
    let mut steps = Value::Null;
    for edit in edits {
        steps = protocol.call_ok("protocol_edit", edit);
    }
    let steps = steps.as_array().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["name"], "second");
    assert_eq!(steps[0]["description"], "Second first");
}

#[test]
fn protocol_edit_refuses_as_the_command_does() {
    let protocol = Protocol::new();
    protocol.record("first");
    let before = protocol.manifest();
    for arguments in [
        json!({ "edit": "delete", "path": "steps[7]" }),
        json!({ "edit": "bogus", "path": "name" }),
        json!({ "path": "name", "value": "x" }),
        json!({ "edit": "replace", "path": "name" }),
        json!({ "edit": "reorder", "path": "steps", "from": "a", "to": 0 }),
        json!({ "edit": "replace", "path": "steps[0].name", "value": "" }),
    ] {
        let result = protocol.call("protocol_edit", arguments.clone());
        assert_eq!(result["isError"], true, "{arguments}: {result}");
        assert_eq!(protocol.manifest(), before, "{arguments}");
    }
}

#[test]
fn history_list_lists_after_record() {
    let protocol = Protocol::new();
    protocol.record("first");
    let by_tool = protocol.call_ok("history_list", json!({}));
    let out = protocol.arc(&["history", "list", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let by_command: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(by_tool, by_command);

    let entries = by_tool.as_array().unwrap();
    assert!(
        !entries.is_empty(),
        "recording is a version of the Protocol"
    );
    for entry in entries {
        for key in ["id", "kind", "at", "bytes", "way"] {
            assert!(entry.get(key).is_some(), "{key} in {entry}");
        }
    }
}

#[test]
fn history_list_given_a_dir_that_is_not_the_working_directory() {
    let protocol = Protocol::new();
    protocol.record("first");
    let elsewhere = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(elsewhere.path())
        .env("ARCFORM_HISTORY_DIR", &protocol.history)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.take().unwrap(),
        "{}",
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "history_list", "arguments": { "dir": protocol.dir } } })
    )
    .unwrap();
    let out = child.wait_with_output().unwrap();
    let response: Value =
        serde_json::from_slice(out.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_ne!(response["result"]["isError"], true, "{response}");
}

#[test]
fn history_restore_undoes_record() {
    let protocol = Protocol::new();
    let earlier = protocol.manifest();
    protocol.record("first");
    assert_ne!(protocol.manifest(), earlier);

    let entries = protocol.call_ok("history_list", json!({}));
    let entry = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            let shown = protocol.arc(&["history", "show", entry["id"].as_str().unwrap()]);
            shown.stdout == earlier
        })
        .expect("an entry holds the text before the record");
    let id = entry["id"].as_str().unwrap();

    let restored = protocol.call_ok("history_restore", json!({ "id": id }));
    assert_eq!(restored["restored"], id);
    // `dir` defaults to the working directory, so the path reads from there.
    assert_eq!(restored["file"], "./arcform.yaml");
    assert_eq!(protocol.manifest(), earlier);
}

#[test]
fn history_restore_for_panel_file() {
    let protocol = Protocol::new();
    let panel = protocol.dir.join("panels").join("x.yaml");
    std::fs::create_dir_all(panel.parent().unwrap()).unwrap();
    let store = LocalHistory::at_root(&protocol.history);
    let id = store
        .record_save_for_file(&panel, "mark: lineY\n")
        .unwrap()
        .expect("recorded")
        .id;
    std::fs::write(&panel, "mark: dot\n").unwrap();
    let spec = protocol.manifest();

    let listed = protocol.call_ok("history_list", json!({ "file": "panels/x.yaml" }));
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], id.as_str());

    let restored = protocol.call_ok(
        "history_restore",
        json!({ "id": id, "file": "panels/x.yaml" }),
    );
    assert_eq!(restored["restored"], id.as_str());
    assert_eq!(restored["file"], "./panels/x.yaml");
    assert_eq!(std::fs::read_to_string(&panel).unwrap(), "mark: lineY\n");
    assert_eq!(protocol.manifest(), spec, "no other file is written");
}

#[test]
fn history_restore_refuses_an_unknown_entry() {
    let protocol = Protocol::new();
    protocol.record("first");
    let before = protocol.manifest();
    for arguments in [json!({ "id": "not-an-id" }), json!({})] {
        let result = protocol.call("history_restore", arguments.clone());
        assert_eq!(result["isError"], true, "{arguments}: {result}");
        assert_eq!(protocol.manifest(), before);
    }
}
