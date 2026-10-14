//! `arc mcp`'s standard output carries JSON-RPC and nothing else while `protocol_run`
//! runs a Protocol, asked of the real `arc mcp` server over stdio.
//!
//! MCP's stdio transport reads every line the server writes to standard output as a
//! JSON-RPC message. A run writes lines of its own: arc's step lines, the rows a SQL
//! step's `SELECT` prints on the DuckDB command line, and whatever a command step
//! echoes. These tests pin that:
//!
//!   1. **clean** — every line of the server's standard output parses as JSON-RPC,
//!      for a run whose SQL step and command step each print;
//!   2. **told why** — a run that fails before it writes a record answers with an
//!      error ending in the run's last output, bounded in length;
//!   3. **large** — a step that prints megabytes does not stall the server;
//!   4. **force** — `force` re-runs fresh steps only when the call asks for it.

#![cfg(feature = "mcp")]

#[allow(dead_code)]
mod common;

use std::path::Path;

use common::mcp_session;
use serde_json::{Value, json};

/// The Protocol in `tests/fixtures/mcp_stdout`, copied to a fresh directory.
fn copy_fixture(name: &str) -> tempfile::TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let dir = tempfile::tempdir().expect("tempdir");
    copy_dir(&src, dir.path());
    dir
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create a fixture directory");
    for entry in std::fs::read_dir(src).expect("read a fixture directory") {
        let entry = entry.expect("a fixture entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("a file type").is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).expect("copy a fixture file");
        }
    }
}

/// Swap the fixture's command step for one that runs `command`.
fn with_command(dir: &Path, command: &str) {
    let manifest = dir.join("arcform.yaml");
    let text = std::fs::read_to_string(&manifest).expect("read arcform.yaml");
    let text = text.replace(
        "command: echo hello-from-command",
        &format!("command: {}", serde_json::to_string(command).unwrap()),
    );
    std::fs::write(&manifest, text).expect("write arcform.yaml");
}

fn initialize(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp_stdout", "version": "0" },
        },
    })
}

fn tools_call(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

/// Parse every line as JSON-RPC, failing on the first that is not.
fn parse_all(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|line| {
            let v: Value =
                serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON-RPC: {line}"));
            assert_eq!(v["jsonrpc"], "2.0", "not JSON-RPC: {line}");
            v
        })
        .collect()
}

/// The response that answers request `id`.
fn response(lines: &[String], id: u64) -> Value {
    parse_all(lines)
        .into_iter()
        .find(|v| v["id"] == id)
        .unwrap_or_else(|| panic!("no response to request {id} in {lines:?}"))
}

/// One `protocol_run` call on `dir` with `extra` arguments, returning its `result`
/// after checking every line of the session is JSON-RPC.
fn protocol_run(dir: &Path, extra: Value) -> Value {
    let mut arguments = json!({ "dir": dir });
    if let (Some(args), Some(extra)) = (arguments.as_object_mut(), extra.as_object()) {
        args.extend(extra.clone());
    }
    let lines = mcp_session(
        dir,
        &[initialize(1), tools_call(2, "protocol_run", arguments)],
    );
    parse_all(&lines);
    response(&lines, 2)["result"].clone()
}

/// Each step's `status.state`, by name.
fn step_states(result: &Value) -> Vec<(String, String)> {
    result["structuredContent"]["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("no steps in {result}"))
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_string(),
                s["status"]["state"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn every_stdout_line_of_protocol_run_is_json_rpc() {
    let dir = copy_fixture("mcp_stdout");
    let lines = mcp_session(
        dir.path(),
        &[
            initialize(1),
            tools_call(2, "protocol_run", json!({ "dir": dir.path() })),
        ],
    );
    assert!(!lines.is_empty());
    for line in &lines {
        let v: Value =
            serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON-RPC: {line}"));
        assert_eq!(v["jsonrpc"], "2.0");
    }
    let result = &response(&lines, 2)["result"];
    assert_eq!(result["isError"], false, "protocol_run failed: {result}");
    assert_eq!(
        result["structuredContent"]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(result["structuredContent"]["run"]["outcome"], "success");
}

#[test]
fn protocol_run_returns_the_b4_protocol_and_run_contract() {
    let dir = copy_fixture("mcp_stdout");
    let result = protocol_run(dir.path(), json!({}));
    let contract = &result["structuredContent"];
    assert_eq!(
        contract["contract_version"], "b4/1",
        "protocol_run must return the live Protocol+Run contract"
    );
    assert_eq!(contract["run"]["protocol"]["name"], "mcp_stdout");
    assert!(contract["assets"].is_array());
    assert_eq!(
        step_states(&result),
        vec![
            ("t".to_string(), "success".to_string()),
            ("echo_step".to_string(), "success".to_string()),
        ]
    );
}

#[test]
fn protocol_run_failure_carries_bounded_tail() {
    let dir = copy_fixture("mcp_stdout");
    // A failing `on_init` hook stops the run before it writes a record, so the tool's
    // error is the one place the run's output can reach the agent. It prints well over
    // the bound before the marker, so only a tail that keeps the end carries it.
    let manifest = dir.path().join("arcform.yaml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "steps:\n",
            "hooks:\n  on_init:\n    name: setup\n    command: \"yes noise | head -c 20000; echo marker-xyz; exit 3\"\nsteps:\n",
        ),
    )
    .unwrap();
    let result = protocol_run(dir.path(), json!({}));
    assert_eq!(
        result["isError"], true,
        "a run with no record is an error: {result}"
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("\n--- last output ---\n") && text.contains("marker-xyz"),
        "the error does not carry the run's last output: {text}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "the error carries terminal escape sequences: {text:?}"
    );
    assert!(
        text.len() < 6_000,
        "the error is not bounded: {} bytes",
        text.len()
    );
}

#[test]
fn protocol_run_failure_with_a_record_returns_the_record() {
    let dir = copy_fixture("mcp_stdout");
    with_command(dir.path(), "echo marker-xyz; exit 3");
    let result = protocol_run(dir.path(), json!({}));
    // A run that fails after it writes its record answers with the record, as a run
    // that succeeds does: the failure is in the record's outcome and the step's state.
    assert_eq!(result["isError"], false, "protocol_run failed: {result}");
    assert_ne!(result["structuredContent"]["run"]["outcome"], "success");
    assert_eq!(
        step_states(&result),
        vec![
            ("t".to_string(), "success".to_string()),
            ("echo_step".to_string(), "failed".to_string()),
        ]
    );
}

#[test]
fn protocol_run_survives_large_child_output() {
    let dir = copy_fixture("mcp_stdout");
    with_command(dir.path(), "yes line | head -c 5000000");
    let result = protocol_run(dir.path(), json!({}));
    assert_eq!(result["isError"], false, "protocol_run failed: {result}");
    assert_eq!(result["structuredContent"]["run"]["outcome"], "success");
}

#[test]
fn protocol_run_passes_force_only_when_asked() {
    // The SQL step `t` is fresh once it has run; the command step declares no output,
    // so it runs every time and says nothing of `force`.
    let state_of_t = |result: &Value| {
        step_states(result)
            .into_iter()
            .find(|(name, _)| name == "t")
            .map(|(_, state)| state)
            .unwrap_or_else(|| panic!("no step t in {result}"))
    };
    let dir = copy_fixture("mcp_stdout");
    let first = protocol_run(dir.path(), json!({}));
    assert_eq!(state_of_t(&first), "success", "first run: {first}");

    let second = protocol_run(dir.path(), json!({}));
    assert_eq!(
        state_of_t(&second),
        "skipped",
        "a run without force re-ran a fresh step"
    );

    let third = protocol_run(dir.path(), json!({ "force": false }));
    assert_eq!(
        state_of_t(&third),
        "skipped",
        "`force: false` re-ran a fresh step"
    );

    let fourth = protocol_run(dir.path(), json!({ "force": true }));
    assert_eq!(
        state_of_t(&fourth),
        "success",
        "a run with force skipped a step"
    );
}

#[test]
fn protocol_run_refuses_a_dir_or_force_of_the_wrong_type_and_runs_nothing() {
    // A `dir` that is not a string must not fall back to the server's working
    // directory (here the fixture), and a `force` that is not a boolean must not
    // pass as false: each is refused by name, and no run starts.
    let dir = copy_fixture("mcp_stdout");
    for (extra, message) in [
        (json!({ "dir": 7 }), "`dir` must be a string, not 7"),
        (
            json!({ "force": "yes" }),
            "`force` must be a boolean, not \"yes\"",
        ),
    ] {
        let result = protocol_run(dir.path(), extra.clone());
        assert_eq!(result["isError"], true, "{extra} was not refused: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(message), "{extra}: {text}");
        assert!(
            !dir.path().join("build").exists(),
            "{extra}: a refused call ran the Protocol"
        );
    }
}

#[test]
fn protocol_run_passes_each_param_to_the_run() {
    let dir = copy_fixture("mcp_stdout");
    let manifest = dir.path().join("arcform.yaml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace("steps:\n", "params:\n  who:\n    default: nobody\nsteps:\n"),
    )
    .unwrap();
    let result = protocol_run(dir.path(), json!({ "params": { "who": "somebody" } }));
    assert_eq!(result["isError"], false, "protocol_run failed: {result}");
    let params = &result["structuredContent"]["run"]["params"];
    let who = params
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["key"] == "who"))
        .unwrap_or_else(|| panic!("no param `who` in {params}"));
    assert_eq!(
        who["value"], "somebody",
        "the run did not take the param: {who}"
    );
}

#[test]
fn protocol_run_carries_a_param_key_that_starts_with_a_dash() {
    let dir = copy_fixture("mcp_stdout");
    let manifest = dir.path().join("arcform.yaml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace(
            "steps:\n",
            "params:\n  \"-x\":\n    default: nobody\nsteps:\n",
        ),
    )
    .unwrap();
    let result = protocol_run(dir.path(), json!({ "params": { "-x": "1" } }));
    assert_eq!(result["isError"], false, "protocol_run failed: {result}");
    let params = &result["structuredContent"]["run"]["params"];
    let x = params
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["key"] == "-x"))
        .unwrap_or_else(|| panic!("no param `-x` in {params}"));
    assert_eq!(x["value"], "1", "the run did not take the param: {x}");
}

#[test]
fn protocol_run_answers_when_a_step_leaves_a_process_holding_its_output() {
    let dir = copy_fixture("mcp_stdout");
    // The background `sleep` inherits the run's standard output and standard error and
    // keeps both open for 30 seconds after `arc run` has exited.
    with_command(dir.path(), "sleep 30 &");
    let started = std::time::Instant::now();
    let result = protocol_run(dir.path(), json!({}));
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "protocol_run waited {elapsed:?} on a process the run left behind"
    );
    assert_eq!(result["isError"], false, "protocol_run failed: {result}");
    assert_eq!(result["structuredContent"]["run"]["outcome"], "success");
}

#[test]
fn protocol_run_refuses_a_param_key_the_command_line_cannot_carry() {
    let dir = copy_fixture("mcp_stdout");
    for (key, why) in [("a=b", "`=`"), ("", "empty")] {
        let result = protocol_run(dir.path(), json!({ "params": { key: "c" } }));
        assert_eq!(
            result["isError"], true,
            "a key {why} was accepted: {result}"
        );
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains(&format!("`{key}`")),
            "the error does not name the key {key:?}: {text}"
        );
    }
    assert!(
        !dir.path().join("build/.arcform/runs").exists(),
        "a refused call ran the Protocol"
    );
}
