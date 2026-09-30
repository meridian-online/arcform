//! `arc mcp`'s `operation_describe` tool: the operations arc holds, and what one
//! of them or one of arc's operators takes, asked of the real `arc mcp` server
//! over stdio.
//!
//! An agent that reaches arc only over MCP, such as an editor or a hosted
//! assistant with no shell, asks the same two questions `arc operation list` and
//! `arc operation describe` answer in a terminal. These tests put each question to
//! both and compare the answers:
//!
//!   1. **list** — no argument returns the array `arc operation list --json`
//!      prints, under one key, which holds operations alone;
//!   2. **describe** — an operation's long name or an operator's name returns
//!      the object `arc operation describe` prints for it, and `operator_describe`
//!      still returns an operator's `with:` schema whole;
//!   3. **refuse** — an operation arc does not hold is an error result naming it,
//!      and the server answers the request after it;
//!   4. **advertise** — `tools/list` holds the tool, and both `arc mcp --help` and
//!      the `instructions` that `initialize` returns name it beside the tools they
//!      named before.
//!
//! The server is spoken to as MCP's stdio transport is: one JSON-RPC message per
//! line on stdin, one response line per request on stdout. Neither operation verb
//! reads a protocol, so every run happens in an empty directory.

#![cfg(feature = "mcp")]

use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

/// The tools `arc mcp --help` and the server's `instructions` named before
/// `operation_describe` was added; both texts keep naming them.
const NAMED_BEFORE: [&str; 2] = ["protocol_run", "operator_describe"];

/// Run the real `arc` binary with `args`, in a directory that holds no protocol.
fn arc(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir.path())
        .args(args)
        .output()
        .expect("spawn arc")
}

/// Run the binary, demand success, and return its stdout.
fn arc_ok(args: &[&str]) -> String {
    let out = arc(args);
    assert!(
        out.status.success(),
        "arc {args:?} failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout is UTF-8")
}

/// Run the binary and parse its stdout, all of it, as one JSON document.
fn arc_json(args: &[&str]) -> Value {
    let stdout = arc_ok(args);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("arc {args:?} stdout is not JSON ({e}):\n{stdout}"))
}

/// Start `arc mcp`, send each request as one line, close stdin, and return the
/// responses in the order the server wrote them.
fn mcp_session(requests: &[Value]) -> Vec<Value> {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        for request in requests {
            writeln!(stdin, "{request}").expect("write a request");
        }
        // Dropping stdin closes it, which ends the server's read loop.
    }
    let out = child.wait_with_output().expect("wait for arc mcp");
    assert!(
        out.status.success(),
        "arc mcp failed (code {:?}):\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("stdout is UTF-8")
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("response is not JSON ({e}): {line}"))
        })
        .collect()
}

/// A `tools/call` request for `operation_describe` with `arguments`.
fn call_operation_describe(id: u64, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": "operation_describe", "arguments": arguments },
    })
}

/// The one response in `responses` that answers request `id`.
fn response_to(responses: &[Value], id: u64) -> &Value {
    responses
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no response to request {id} in {responses:?}"))
}

#[test]
fn tools_list_holds_operation_describe_with_a_description_and_an_input_schema() {
    let responses = mcp_session(&[json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list",
    })]);
    let tools = response_to(&responses, 1)["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    // Beside the seven it held.
    for held in [
        "infer",
        "profile",
        "taxonomy",
        "validate",
        "generate",
        "protocol_run",
        "operator_describe",
    ] {
        assert!(names.contains(&held), "`{held}` is missing from {names:?}");
    }
    let tool = tools
        .iter()
        .find(|t| t["name"] == "operation_describe")
        .unwrap_or_else(|| panic!("`operation_describe` is missing from {names:?}"));
    assert!(
        tool["description"]
            .as_str()
            .is_some_and(|d| !d.trim().is_empty()),
        "tool: {tool}"
    );
    assert_eq!(tool["inputSchema"]["type"], "object", "tool: {tool}");
    assert_eq!(
        tool["inputSchema"]["properties"]["operation"]["type"], "string",
        "tool: {tool}"
    );
}

#[test]
fn called_with_no_argument_it_returns_what_arc_operation_list_json_prints_under_one_key() {
    let from_cli = arc_json(&["operation", "list", "--json"]);
    let responses = mcp_session(&[call_operation_describe(1, json!({}))]);
    let result = &response_to(&responses, 1)["result"];
    assert_eq!(result["isError"], false, "result: {result}");
    assert_eq!(
        result["structuredContent"],
        json!({ "operations": from_cli }),
        "the tool's listing is not the command line's"
    );
    // The text a client shows is the same document.
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
        .expect("the text is JSON");
    assert_eq!(text, result["structuredContent"]);
    // Each entry carries the summary the command line's listing carries.
    let entries = result["structuredContent"]["operations"]
        .as_array()
        .unwrap();
    assert!(!entries.is_empty(), "the catalogue holds an operation");
    for entry in entries {
        assert!(
            entry["long_name"].as_str().is_some_and(|s| !s.is_empty())
                && entry["summary"].as_str().is_some_and(|s| !s.is_empty()),
            "entry: {entry}"
        );
    }
}

#[test]
fn called_with_filter_rows_it_returns_what_arc_operation_describe_prints() {
    let from_cli = arc_json(&["operation", "describe", "filter-rows"]);
    let responses = mcp_session(&[call_operation_describe(
        1,
        json!({ "operation": "filter-rows" }),
    )]);
    let result = &response_to(&responses, 1)["result"];
    assert_eq!(result["isError"], false, "result: {result}");
    assert_eq!(
        result["structuredContent"], from_cli,
        "the tool's description is not the command line's"
    );
    // A description, not the listing entry: it carries what the operation takes.
    assert_eq!(result["structuredContent"]["long_name"], "filter-rows");
    assert!(result["structuredContent"]["parameters"].is_object());
}

#[test]
fn called_with_sort_rows_it_returns_what_arc_operation_describe_prints() {
    let from_cli = arc_json(&["operation", "describe", "sort-rows"]);
    let responses = mcp_session(&[call_operation_describe(
        1,
        json!({ "operation": "sort-rows" }),
    )]);
    let result = &response_to(&responses, 1)["result"];
    assert_eq!(result["isError"], false, "result: {result}");
    assert_eq!(
        result["structuredContent"], from_cli,
        "the tool's description is not the command line's"
    );
    // A description of the sort, carrying its `order_by` order annotation.
    assert_eq!(result["structuredContent"]["long_name"], "sort-rows");
    assert_eq!(
        result["structuredContent"]["parameters"]["properties"]["order_by"]["x-kind"],
        "order"
    );
}

/// The operators this build holds, as `operator_describe` lists them.
fn operator_names() -> Vec<String> {
    let responses = mcp_session(&[json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "operator_describe", "arguments": {} },
    })]);
    let listed = &response_to(&responses, 1)["result"]["structuredContent"]["operators"];
    let names: Vec<String> = listed
        .as_array()
        .unwrap_or_else(|| panic!("operator_describe lists no operators: {listed}"))
        .iter()
        .map(|n| n.as_str().expect("an operator name").to_string())
        .collect();
    assert!(
        names.len() >= 12,
        "the build holds the twelve operators `cli` brings, and lists {names:?}"
    );
    names
}

#[test]
fn called_with_each_operator_and_operation_it_returns_what_arc_operation_describe_prints() {
    let names: Vec<String> = operator_names()
        .into_iter()
        .chain(["filter-rows".to_string(), "sort-rows".to_string()])
        .collect();
    let requests: Vec<Value> = names
        .iter()
        .zip(1..)
        .map(|(name, id)| call_operation_describe(id, json!({ "operation": name })))
        .collect();
    let responses = mcp_session(&requests);
    for (name, id) in names.iter().zip(1..) {
        let from_cli = arc_json(&["operation", "describe", name]);
        let result = &response_to(&responses, id)["result"];
        assert_eq!(result["isError"], false, "`{name}`: {result}");
        assert_eq!(
            result["structuredContent"], from_cli,
            "`{name}`: the tool's description is not the command line's"
        );
    }
}

#[test]
fn called_with_no_argument_it_lists_no_operator() {
    let operators = operator_names();
    let responses = mcp_session(&[call_operation_describe(1, json!({}))]);
    let listed = &response_to(&responses, 1)["result"]["structuredContent"]["operations"];
    let names: Vec<&str> = listed
        .as_array()
        .unwrap_or_else(|| panic!("no listing: {listed}"))
        .iter()
        .filter_map(|e| e["long_name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["filter-rows", "sort-rows"],
        "the listing holds the operations alone, not the operators {operators:?}"
    );
}

#[test]
fn operator_describe_still_returns_the_with_schema_whole_its_roles_included() {
    let responses = mcp_session(&[json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "operator_describe", "arguments": { "operator": "splink_resolve" } },
    })]);
    let schema = &response_to(&responses, 1)["result"]["structuredContent"];
    assert_eq!(
        schema["required"],
        json!(["edgar", "gleif", "out"]),
        "operator_describe is the form a `with:` block is written from, roles and all: {schema}"
    );
    for key in ["edgar", "gleif", "out", "sample"] {
        assert!(
            schema["properties"].get(key).is_some(),
            "operator_describe dropped `{key}` from splink_resolve's schema: {schema}"
        );
    }
    assert!(
        schema.get("reads").is_none() && schema.get("writes").is_none(),
        "operator_describe returns the schema, not the description: {schema}"
    );
}

#[test]
fn an_operation_arc_does_not_hold_is_an_error_result_naming_it_and_the_server_keeps_answering() {
    let responses = mcp_session(&[
        call_operation_describe(1, json!({ "operation": "no-such-operation" })),
        call_operation_describe(2, json!({ "operation": "filter-rows" })),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }),
    ]);

    let refused = response_to(&responses, 1);
    assert!(
        refused.get("error").is_none(),
        "a refusal is a result, not a protocol error: {refused}"
    );
    assert_eq!(refused["result"]["isError"], true, "refused: {refused}");
    let text = refused["result"]["content"][0]["text"]
        .as_str()
        .expect("the error has text");
    assert!(text.contains("no-such-operation"), "text: {text}");

    // The requests after it were answered, the first of them with a description.
    let described = &response_to(&responses, 2)["result"];
    assert_eq!(described["isError"], false, "described: {described}");
    assert_eq!(described["structuredContent"]["long_name"], "filter-rows");
    assert_eq!(response_to(&responses, 3)["result"], json!({}));
}

#[test]
fn help_and_instructions_each_name_operation_describe_beside_the_tools_they_named() {
    let help = arc_ok(&["mcp", "--help"]);
    let responses = mcp_session(&[json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18" },
    })]);
    let instructions = response_to(&responses, 1)["result"]["instructions"]
        .as_str()
        .expect("initialize returns instructions");
    for (what, text) in [
        ("arc mcp --help", help.as_str()),
        ("instructions", instructions),
    ] {
        for tool in NAMED_BEFORE.into_iter().chain(["operation_describe"]) {
            assert!(
                text.contains(tool),
                "{what} does not name `{tool}`:\n{text}"
            );
        }
    }
}
