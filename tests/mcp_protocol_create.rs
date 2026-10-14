//! `arc mcp`'s `protocol_create` tool writes what `arc create-protocol` writes, asked of
//! the real binaries:
//!
//!   1. **same files** — `arcform.yaml`, `.gitignore` and `.gitattributes` are byte-equal
//!      to the verb's;
//!   2. **history** — the version it records names `mcp`;
//!   3. **refusal** — a second call on the same directory is an error result and leaves
//!      the spec as it was;
//!   4. **stdout** — every line the server writes is JSON-RPC.

#![cfg(feature = "mcp")]

#[allow(dead_code)]
mod common;

use std::path::Path;
use std::process::Command;

use common::mcp_session_env;
use serde_json::{Value, json};

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": { "name": "mcp_protocol_create", "version": "0" },
        },
    })
}

fn create_call(id: u64, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "protocol_create", "arguments": arguments },
    })
}

/// The tool's responses to `calls` (ids 2..), every stdout line checked as JSON-RPC.
fn call_tool(base: &Path, history: &Path, calls: Vec<Value>) -> Vec<Value> {
    let mut requests = vec![initialize()];
    requests.extend(calls);
    let lines = mcp_session_env(base, &requests, &[("ARCFORM_HISTORY_DIR", history)]);
    lines
        .iter()
        .map(|line| {
            let v: Value =
                serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON-RPC: {line}"));
            assert_eq!(v["jsonrpc"], "2.0", "not JSON-RPC: {line}");
            v
        })
        .filter(|v| v["id"] != 1)
        .collect()
}

fn arc(base: &Path, history: &Path, args: &[&str]) -> std::process::Output {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(base)
        .env("ARCFORM_HISTORY_DIR", history)
        .args(args)
        .output()
        .expect("spawn arc");
    assert!(out.status.success(), "arc {args:?} failed: {out:?}");
    out
}

#[test]
fn protocol_create_writes_the_same_files_as_the_verb() {
    let base = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    let (a, b) = (base.path().join("a"), base.path().join("b"));
    arc(
        base.path(),
        history.path(),
        &["create-protocol", "a", "--name", "p"],
    );
    std::fs::create_dir(&b).unwrap();
    let out = call_tool(
        base.path(),
        history.path(),
        vec![create_call(2, json!({ "dir": b, "name": "p" }))],
    );
    let result = &out[0]["result"];
    assert_ne!(result["isError"], json!(true), "{result}");
    assert_eq!(
        result["structuredContent"]["files_written"],
        json!(["arcform.yaml", ".gitignore", ".gitattributes"])
    );
    assert_eq!(
        result["structuredContent"]["manifest"],
        json!(b.join("arcform.yaml").display().to_string())
    );
    for file in ["arcform.yaml", ".gitignore", ".gitattributes"] {
        assert_eq!(
            std::fs::read(a.join(file)).unwrap(),
            std::fs::read(b.join(file)).unwrap(),
            "{file} differs"
        );
    }
}

#[test]
fn protocol_create_records_history_as_mcp() {
    let base = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    let b = base.path().join("b");
    std::fs::create_dir(&b).unwrap();
    call_tool(
        base.path(),
        history.path(),
        vec![create_call(2, json!({ "dir": b, "name": "p" }))],
    );
    let out = arc(
        base.path(),
        history.path(),
        &["history", "list", "--dir", "b"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mcp"), "no mcp version in:\n{stdout}");
}

#[test]
fn protocol_create_refuses_existing_spec() {
    let base = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    let b = base.path().join("b");
    std::fs::create_dir(&b).unwrap();
    let out = call_tool(
        base.path(),
        history.path(),
        vec![create_call(2, json!({ "dir": b, "name": "p" }))],
    );
    assert_ne!(out[0]["result"]["isError"], json!(true));
    let before = std::fs::read(b.join("arcform.yaml")).unwrap();
    let out = call_tool(
        base.path(),
        history.path(),
        vec![create_call(2, json!({ "dir": b, "name": "other" }))],
    );
    let result = &out[0]["result"];
    assert_eq!(result["isError"], json!(true), "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("already"), "unexpected refusal: {text}");
    assert_eq!(before, std::fs::read(b.join("arcform.yaml")).unwrap());
}

#[test]
fn protocol_create_keeps_stdout_json_rpc() {
    let base = tempfile::tempdir().unwrap();
    let history = tempfile::tempdir().unwrap();
    let b = base.path().join("b");
    std::fs::create_dir(&b).unwrap();
    // call_tool parses every line, so a stray `created ...` line would panic.
    let out = call_tool(
        base.path(),
        history.path(),
        vec![create_call(2, json!({ "dir": b }))],
    );
    assert_eq!(out.len(), 1);
}
