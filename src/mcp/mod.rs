//! `arc mcp` — a Model Context Protocol server over stdio.
//!
//! This is the entry point that subsumes the FineType MCP server's role: instead of a
//! separate `finetype mcp` process, `arc mcp` federates the `finetype` CLI as a set of
//! tools and adds two of its own — one that runs a Protocol and returns the live
//! Protocol+Run contract, one that emits an operator's `with:` JSON Schema.
//!
//! # Transport
//!
//! MCP's stdio transport is newline-delimited JSON-RPC 2.0: one JSON message per line
//! on stdin, one response line per request on stdout. That is small enough to speak
//! directly with the `serde_json` already in the dependency graph — no async runtime,
//! no MCP SDK — which is why the `mcp` feature adds no dependencies (see `Cargo.toml`).
//!
//! # Methods
//!
//! - `initialize` — handshake; advertises the `tools` capability.
//! - `notifications/*` — accepted and ignored (notifications get no reply).
//! - `ping` — liveness.
//! - `tools/list` — the registered tool set with input schemas.
//! - `tools/call` — invoke a tool by name. A tool that fails returns an
//!   `isError: true` result (per MCP), not a JSON-RPC protocol error.
//!
//! # Tools
//!
//! Five federate the `finetype` CLI behind a minimum-version gate (see [`finetype`]);
//! two are native to `arc` (see [`hero`]).

mod finetype;
mod hero;

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::error::{Error, Result};

/// The MCP protocol revision this server implements.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// Server identity reported in the `initialize` handshake.
const SERVER_NAME: &str = "arc-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const INSTRUCTIONS: &str = "arc — a local-first data-pipeline engine, exposed over MCP.\n\n\
    Tools: infer / profile / taxonomy / validate / generate federate the FineType CLI \
    (semantic type inference over tabular data); protocol_run runs an arc Protocol and \
    returns its live Protocol+Run contract; operator_describe emits an operator's `with:` \
    JSON Schema for authoring.";

// ─────────────────────────────────────────────────────────────────────────────
// Tool result shapes
// ─────────────────────────────────────────────────────────────────────────────

/// A tool's successful output: human-readable text (always) plus, when the tool
/// produced a single JSON document, that value for `structuredContent`.
#[derive(Debug)]
pub(crate) struct ToolOutput {
    pub text: String,
    pub structured: Option<Value>,
}

impl ToolOutput {
    /// A text-only result.
    pub(crate) fn text(text: impl Into<String>) -> Self {
        ToolOutput {
            text: text.into(),
            structured: None,
        }
    }

    /// A structured result: the value pretty-printed as text and carried as
    /// `structuredContent`.
    pub(crate) fn json(value: Value) -> Self {
        ToolOutput {
            text: serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            structured: Some(value),
        }
    }

    /// Federated-CLI output: if the child's stdout is one JSON document, carry it as
    /// structured content too; otherwise (plain text, or newline-delimited JSON) keep
    /// it as text.
    pub(crate) fn from_cli(stdout: String) -> Self {
        match serde_json::from_str::<Value>(stdout.trim()) {
            Ok(value) => ToolOutput {
                text: stdout,
                structured: Some(value),
            },
            Err(_) => ToolOutput::text(stdout),
        }
    }
}

/// The outcome of a tool call: `Ok` output, or an `Err` message surfaced to the caller
/// as an `isError: true` result.
pub(crate) type ToolResult = std::result::Result<ToolOutput, String>;

/// A registered tool: its MCP name, one-line description, input-schema builder, and
/// handler.
pub(crate) struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: fn() -> Value,
    pub handler: fn(&Value) -> ToolResult,
}

/// Every tool this server registers: the five FineType-federating proxies followed by
/// the two native tools.
fn tools() -> Vec<ToolDef> {
    let mut all = finetype::tools();
    all.extend(hero::tools());
    all
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON-RPC dispatch
// ─────────────────────────────────────────────────────────────────────────────

/// Standard JSON-RPC error codes used here.
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INVALID_REQUEST: i64 = -32600;

/// Handle one request `method` with its `params`, producing either a result value or a
/// `(code, message)` JSON-RPC error. Notifications are handled by the caller and never
/// reach here.
fn dispatch(method: &str, params: &Value) -> std::result::Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(initialize_result(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(params),
        other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
    }
}

/// The `initialize` handshake result. Echoes the client's requested `protocolVersion`
/// when present (maximising compatibility), else offers ours.
fn initialize_result(params: &Value) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        "instructions": INSTRUCTIONS,
    })
}

/// The `tools/list` result: every registered tool with its input schema.
fn tools_list() -> Value {
    let list: Vec<Value> = tools()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": (t.input_schema)(),
            })
        })
        .collect();
    json!({ "tools": list })
}

/// The `tools/call` result. Unknown params are a JSON-RPC error; an unknown tool or a
/// failing tool is an `isError: true` result (the MCP convention — a tool error is data
/// the model can react to, not a transport fault).
fn tools_call(params: &Value) -> std::result::Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        (
            INVALID_PARAMS,
            "tools/call: missing tool `name`".to_string(),
        )
    })?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let Some(def) = tools().into_iter().find(|t| t.name == name) else {
        return Ok(error_result(format!("unknown tool: {name}")));
    };
    Ok(call_result((def.handler)(&arguments)))
}

/// Wrap a [`ToolResult`] into an MCP `CallToolResult`.
fn call_result(result: ToolResult) -> Value {
    match result {
        Ok(out) => {
            let mut value = json!({
                "content": [ { "type": "text", "text": out.text } ],
                "isError": false,
            });
            if let (Some(structured), Some(map)) = (out.structured, value.as_object_mut()) {
                map.insert("structuredContent".to_string(), structured);
            }
            value
        }
        Err(message) => error_result(message),
    }
}

/// An `isError: true` `CallToolResult` carrying `message`.
fn error_result(message: String) -> Value {
    json!({
        "content": [ { "type": "text", "text": message } ],
        "isError": true,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// stdio loop
// ─────────────────────────────────────────────────────────────────────────────

/// Serve MCP over stdio until stdin closes. One JSON-RPC message per line in, one
/// response line per request out.
pub(crate) fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(Error::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle_line(&line) {
            write_message(&mut out, &response)?;
        }
    }
    Ok(())
}

/// Process one input line, returning the response to write (or `None` for a
/// notification, a parse error on a message with no id, or anything that warrants
/// silence).
fn handle_line(line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        // A malformed message with no recoverable id: reply with a null-id parse error
        // so a client sees the fault rather than a silent hang.
        Err(_) => return Some(error_response(Value::Null, INVALID_REQUEST, "parse error")),
    };

    // Notifications (a request without an `id`) are handled without a reply.
    let id = match message.get("id") {
        Some(id) => id.clone(),
        None => return None,
    };

    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(error_response(id, INVALID_REQUEST, "missing method"));
    };
    // A `notifications/*` method carrying an id is still a notification in spirit; but a
    // client that gave us an id expects nothing back for it, so stay silent.
    if method.starts_with("notifications/") {
        return None;
    }

    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match dispatch(method, &params) {
        Ok(result) => Some(success_response(id, result)),
        Err((code, msg)) => Some(error_response(id, code, &msg)),
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Write one JSON message as a single line, then flush.
fn write_message(out: &mut impl Write, value: &Value) -> Result<()> {
    let line = serde_json::to_string(value).map_err(|e| Error::Io(std::io::Error::other(e)))?;
    out.write_all(line.as_bytes()).map_err(Error::Io)?;
    out.write_all(b"\n").map_err(Error::Io)?;
    out.flush().map_err(Error::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_advertises_tools_and_identity() {
        let result = initialize_result(&json!({ "protocolVersion": "2025-06-18" }));
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(result["serverInfo"]["version"], SERVER_VERSION);
    }

    #[test]
    fn initialize_echoes_client_protocol_version() {
        let result = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn tools_list_registers_all_seven_tools() {
        let listed = tools_list();
        let names: Vec<&str> = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "infer",
            "profile",
            "taxonomy",
            "validate",
            "generate",
            "protocol_run",
            "operator_describe",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool `{expected}` in {names:?}"
            );
        }
        assert_eq!(names.len(), 7, "unexpected tool set: {names:?}");
        // Every tool advertises an object input schema.
        for tool in listed["tools"].as_array().unwrap() {
            assert!(tool["description"].is_string());
            assert_eq!(tool["inputSchema"]["type"], "object", "tool {tool:?}");
        }
    }

    #[test]
    fn tools_call_unknown_tool_is_an_iserror_result_not_a_protocol_error() {
        let result = tools_call(&json!({ "name": "nope", "arguments": {} }))
            .expect("unknown tool is a result, not a protocol error");
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }

    #[test]
    fn tools_call_missing_name_is_a_protocol_error() {
        let err = tools_call(&json!({ "arguments": {} })).unwrap_err();
        assert_eq!(err.0, INVALID_PARAMS);
    }

    #[test]
    fn operator_describe_reachable_through_tools_call() {
        let result = tools_call(&json!({ "name": "operator_describe", "arguments": {} }))
            .expect("operator_describe is registered");
        assert_eq!(result["isError"], false);
        assert!(result["structuredContent"]["operators"].is_array());
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let err = dispatch("frobnicate", &Value::Null).unwrap_err();
        assert_eq!(err.0, METHOD_NOT_FOUND);
    }

    #[test]
    fn notifications_get_no_reply() {
        assert!(handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        // A bare notification (no id) is silent even for a real method.
        assert!(handle_line(r#"{"jsonrpc":"2.0","method":"ping"}"#).is_none());
    }

    #[test]
    fn request_gets_a_reply_with_the_same_id() {
        let response = handle_line(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 7);
        assert!(response["result"].is_object());
    }

    #[test]
    fn parse_error_reports_with_null_id() {
        let response = handle_line("not json").unwrap();
        assert_eq!(response["id"], Value::Null);
        assert_eq!(response["error"]["code"], INVALID_REQUEST);
    }
}
