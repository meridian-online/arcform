//! `arc operator record` and the `arc mcp` tool `operator_record`: an operator
//! step recorded at the end of a Protocol with its `with:` checked against the
//! operator's schema, asked of the real `arc` binary.
//!
//!   1. **record** — the step lands at the end of `steps` with its `with:` as a
//!      block mapping that reads back equal, and is a version of the Protocol;
//!   2. **refuse** — a `with:` the schema does not admit, an operator arc does
//!      not hold and a step name already taken are each refused with the
//!      directory untouched and a message naming the fault;
//!   3. **mcp** — `operator_record` writes the bytes the command writes.
//!
//! Every recording is a version of the Protocol, so each run points
//! `ARCFORM_HISTORY_DIR` at a directory of its own, outside the Protocol.

#[cfg(feature = "mcp")]
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(feature = "mcp")]
use std::process::Stdio;
use std::process::{Command, Output};

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

    fn record(&self, operator: &str, name: &str, with: &Value) -> Output {
        self.arc(&[
            "operator",
            "record",
            operator,
            "--name",
            name,
            "--with",
            &with.to_string(),
        ])
    }

    fn manifest(&self) -> Vec<u8> {
        std::fs::read(self.dir.join("arcform.yaml")).unwrap()
    }

    fn history_lines(&self) -> usize {
        let out = self.arc(&["history", "list"]);
        assert!(out.status.success(), "history list: {}", stderr(&out));
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|line| line.contains(" bytes  "))
            .count()
    }

    /// One `tools/call` of `operator_record` to a fresh `arc mcp` started in the
    /// Protocol's directory.
    #[cfg(feature = "mcp")]
    fn call(&self, arguments: Value) -> Value {
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
                "params": { "name": "operator_record", "arguments": arguments },
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

fn rates_with() -> Value {
    json!({
        "url": "https://example.com/rates.csv",
        "out": "data/rates.csv",
        "headers": { "Accept": "text/csv" }
    })
}

#[test]
fn records_http_fetch_step_at_end() {
    let protocol = Protocol::new();
    let before = protocol.history_lines();
    let with = rates_with();
    let out = protocol.record("http_fetch", "fetch_rates", &with);
    assert!(out.status.success(), "record failed: {}", stderr(&out));

    let manifest: serde_yaml::Value = serde_yaml::from_slice(&protocol.manifest()).unwrap();
    let steps = manifest["steps"].as_sequence().unwrap();
    let last = steps.last().unwrap();
    assert_eq!(last["name"], "fetch_rates");
    assert_eq!(last["op"], "http_fetch");
    assert_eq!(serde_yaml::to_value(&with).unwrap(), last["with"]);
    assert!(last.get("description").is_none());
    assert!(
        protocol.history_lines() > before,
        "the recording is a version of the Protocol"
    );
}

#[test]
fn a_second_step_follows_the_first_and_carries_its_description() {
    let protocol = Protocol::new();
    let first = protocol.record("http_fetch", "fetch_rates", &rates_with());
    assert!(first.status.success(), "{}", stderr(&first));
    let out = protocol.arc(&[
        "operator",
        "record",
        "http_fetch",
        "--name",
        "fetch_more",
        "--with",
        &json!({ "url": "https://example.com/b.csv", "out": "b.csv" }).to_string(),
        "--description",
        "Fetch: the second file",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let manifest: serde_yaml::Value = serde_yaml::from_slice(&protocol.manifest()).unwrap();
    let steps = manifest["steps"].as_sequence().unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["name"], "fetch_rates");
    assert_eq!(steps[1]["name"], "fetch_more");
    assert_eq!(steps[1]["description"], "Fetch: the second file");
}

#[test]
fn refuses_with_that_fails_schema() {
    let protocol = Protocol::new();
    let before = protocol.manifest();
    let out = protocol.record(
        "http_fetch",
        "fetch_rates",
        &json!({ "url": "https://example.com/rates.csv" }),
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("out"),
        "names the field: {}",
        stderr(&out)
    );
    assert_eq!(protocol.manifest(), before);

    for (with, word) in [
        (json!({ "url": 3, "out": "x" }), "url"),
        (json!({ "url": "u", "out": "x", "bogus": 1 }), "bogus"),
        (
            json!({ "url": "u", "out": "x", "headers": { "A": 1 } }),
            "headers",
        ),
    ] {
        let out = protocol.record("http_fetch", "fetch_rates", &with);
        assert!(!out.status.success());
        assert!(stderr(&out).contains(word), "{word}: {}", stderr(&out));
        assert_eq!(protocol.manifest(), before);
    }
}

#[test]
fn refuses_unknown_operator() {
    let protocol = Protocol::new();
    let before = protocol.manifest();
    let out = protocol.record("no_such_operator", "x", &json!({}));
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("call operator_describe with no arguments to list the catalog"),
        "{}",
        stderr(&out)
    );
    assert_eq!(protocol.manifest(), before);
}

#[test]
fn refuses_duplicate_step_name() {
    let protocol = Protocol::new();
    let first = protocol.record("http_fetch", "fetch_rates", &rates_with());
    assert!(first.status.success(), "{}", stderr(&first));
    let before = protocol.manifest();
    let out = protocol.record("http_fetch", "fetch_rates", &rates_with());
    assert!(!out.status.success());
    assert!(stderr(&out).contains("fetch_rates"), "{}", stderr(&out));
    assert_eq!(protocol.manifest(), before);
}

#[test]
fn refuses_a_with_that_is_not_json_or_not_an_object() {
    let protocol = Protocol::new();
    let before = protocol.manifest();
    for with in ["{not json", "[1]"] {
        let out = protocol.arc(&[
            "operator",
            "record",
            "http_fetch",
            "--name",
            "x",
            "--with",
            with,
        ]);
        assert!(!out.status.success(), "{with}");
        assert_eq!(protocol.manifest(), before);
    }
}

#[test]
#[cfg(feature = "mcp")]
fn the_tool_writes_the_bytes_the_command_writes() {
    let by_command = Protocol::new();
    let by_tool = Protocol::new();
    let with = rates_with();
    let out = by_command.arc(&[
        "operator",
        "record",
        "http_fetch",
        "--name",
        "fetch_rates",
        "--with",
        &with.to_string(),
        "--description",
        "Fetch the rates",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let result = by_tool.call(json!({
        "operator": "http_fetch",
        "name": "fetch_rates",
        "with": with,
        "description": "Fetch the rates",
    }));
    assert_ne!(result["isError"], true, "{result}");
    let reported: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        reported,
        json!({ "step": "fetch_rates", "operator": "http_fetch" })
    );

    // The two Protocols were named alike, so their manifests agree whole.
    assert_eq!(by_command.manifest(), by_tool.manifest());
}

#[test]
#[cfg(feature = "mcp")]
fn the_tool_refuses_as_the_command_does() {
    let protocol = Protocol::new();
    let before = protocol.manifest();
    for arguments in [
        json!({ "operator": "http_fetch", "name": "x", "with": { "url": "u" } }),
        json!({ "operator": "nope", "name": "x", "with": {} }),
        json!({ "operator": "http_fetch", "name": "x", "with": "not an object" }),
        json!({ "operator": "http_fetch", "name": "x" }),
    ] {
        let result = protocol.call(arguments.clone());
        assert_eq!(result["isError"], true, "{arguments}: {result}");
        assert_eq!(protocol.manifest(), before, "{arguments}");
    }
    let result = protocol.call(json!({ "operator": "nope", "name": "x", "with": {} }));
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("call operator_describe with no arguments to list the catalog")
    );
}
