//! `arc mcp` — a Model Context Protocol server over stdio.
//!
//! This is the entry point that subsumes the FineType MCP server's role: instead of a
//! separate `finetype mcp` process, `arc mcp` federates the `finetype` CLI as a set of
//! tools and adds tools of its own that run, inspect and record Protocols. The tool
//! set is what [`tools_list`] registers, and [`INSTRUCTIONS`] tells a client when to
//! reach for each; neither is repeated here, so this comment cannot fall behind.
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
//! the rest are native to `arc` (see [`hero`]).

mod finetype;
mod hero;

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::error::{Error, Result};

/// The MCP protocol revisions this server implements, newest last.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-11-25"];

/// Negotiate the protocol version: the client's request when this server supports it,
/// else the newest version it does (the client then decides whether to proceed).
fn negotiate(requested: Option<&str>) -> &'static str {
    requested
        .and_then(|r| SUPPORTED_PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .or_else(|| SUPPORTED_PROTOCOL_VERSIONS.last())
        .copied()
        .expect("at least one supported protocol version")
}

/// Server identity reported in the `initialize` handshake.
const SERVER_NAME: &str = "arc-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const INSTRUCTIONS: &str = "Use arc when you analyse data over DuckDB and the work should be kept as a \
    versioned, re-runnable Protocol rather than a one-off query. arc is a local-first \
    data-pipeline engine, exposed over MCP.\n\n\
    The analyst workflow has five stages. Scope: agree the question, the data and the size of \
    the work (scoping-an-analysis). Profile: type the columns and report data-quality findings \
    (profiling-and-typing-data). Build: record each step into the Protocol \
    (building-and-checking-a-protocol). Check: run it and test the tables it makes \
    (building-and-checking-a-protocol). Present: give the answer, how it was derived and what \
    else turned up (presenting-findings). Start with the using-arcform skill where skills are \
    available.\n\n\
    The tools below are listed in the order you use them: Discover serves Scope and Profile, \
    Record and Run serve Build, Check serves Check, and Present serves Present.\n\n\
    Work in this order. Discover: table_describe shows what a table in a Protocol's database \
    holds, sql_query runs SELECTs against it, table_profile types its columns with FineType, \
    operation_describe and operator_describe say what arc's operations and operators take, and \
    sql_recognise says what arc reads a SQL statement as. Record: protocol_create makes a \
    Protocol, then sql_record, operation_record and operator_record add a step each, an \
    operator's `with:` checked against its schema; protocol_edit changes one thing in the spec, \
    and protocol_write_file writes a table schema, a panel or the README. Run: protocol_run \
    runs the Protocol and returns its Protocol+Run contract. Check: table_validate tests a \
    table against a schema file with FineType's DuckDB extension, and panel_check reports a \
    panel that names a table or column the Protocol does not make. Present: report_write writes \
    report.md from your prose and the newest run's contract.\n\n\
    Recording runs nothing and a run runs: it can write tables and files and reach the \
    network. history_list shows the recorded states of the spec, or of one file beside it, and \
    history_restore rolls it back to one, so a change can be undone. protocol_edit, \
    protocol_write_file and report_write replace content, but what they replace stays in \
    history. table_describe, sql_query, table_profile and table_validate never write the \
    database, though the SQL they run may read files and URLs it names. The FineType tools \
    (infer, profile, taxonomy, validate, generate) work on files and change nothing.";

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

/// What a tool tells the client about its effects, emitted as MCP tool annotations.
#[derive(Clone, Copy)]
pub(crate) struct Annotations {
    /// The tool changes nothing in its environment.
    pub read_only: bool,
    /// The tool may interact with an open set of external entities: the network, or
    /// whatever a Protocol's steps reach.
    pub open_world: bool,
    /// The tool may replace or destroy existing state, not only add to it.
    pub destructive: bool,
}

impl Annotations {
    /// A tool that changes nothing and reaches nothing outside the files it is given.
    pub(crate) const READ_ONLY: Annotations = Annotations {
        read_only: true,
        open_world: false,
        destructive: false,
    };
    /// A tool that changes nothing, but runs SQL that may read files and URLs it names:
    /// it reads a Protocol's database without writing it.
    pub(crate) const READS_OPEN: Annotations = Annotations {
        read_only: true,
        open_world: true,
        destructive: false,
    };
    /// A tool that adds to the Protocol's folder, never replaces what is there, and
    /// reaches nothing else.
    pub(crate) const WRITES: Annotations = Annotations {
        read_only: false,
        open_world: false,
        destructive: false,
    };
    /// A tool that changes a file of the Protocol's folder in place, keeping the state
    /// it replaces in history, and reaches nothing else.
    pub(crate) const EDITS: Annotations = Annotations {
        read_only: false,
        open_world: false,
        destructive: true,
    };
    /// A tool that runs a Protocol: it replaces tables, runs arbitrary steps and may
    /// reach the network.
    pub(crate) const RUNS: Annotations = Annotations {
        read_only: false,
        open_world: true,
        destructive: true,
    };
}

/// A registered tool: its MCP name, one-line description, input-schema builder, and
/// handler.
pub(crate) struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: fn() -> Value,
    pub handler: fn(&Value) -> ToolResult,
    pub annotations: Annotations,
}

/// Every tool this server registers: the five FineType-federating proxies followed by
/// the tools native to `arc`.
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

/// The `initialize` handshake result. Negotiates the client's requested `protocolVersion`:
/// when this server supports it, else offers the newest it does.
fn initialize_result(params: &Value) -> Value {
    let protocol_version = negotiate(params.get("protocolVersion").and_then(Value::as_str));
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
                "annotations": {
                    "readOnlyHint": t.annotations.read_only,
                    "openWorldHint": t.annotations.open_world,
                    "destructiveHint": t.annotations.destructive,
                },
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
    fn initialize_offers_newest_version_for_an_unsupported_request() {
        let result = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(result["protocolVersion"], "2025-11-25");
    }

    fn tool<'a>(list: &'a Value, name: &str) -> &'a Value {
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
    }

    #[test]
    fn negotiate_returns_supported_request() {
        assert_eq!(negotiate(Some("2025-06-18")), "2025-06-18");
    }

    #[test]
    fn negotiate_unknown_returns_newest() {
        assert_eq!(negotiate(Some("2099-01-01")), "2025-11-25");
    }

    #[test]
    fn negotiate_absent_returns_newest() {
        assert_eq!(negotiate(None), "2025-11-25");
    }

    #[test]
    fn read_only_tools_say_so() {
        let list = tools_list();
        for name in [
            "infer",
            "profile",
            "taxonomy",
            "validate",
            "generate",
            "operator_describe",
            "operation_describe",
            "sql_recognise",
            "panel_check",
            "table_describe",
            "table_profile",
            "table_validate",
            "sql_query",
        ] {
            assert_eq!(
                tool(&list, name)["annotations"]["readOnlyHint"],
                true,
                "{name}"
            );
        }
        // The SQL these run may read the files and URLs it names.
        for name in ["table_describe", "sql_query"] {
            let annotations = &tool(&list, name)["annotations"];
            assert_eq!(annotations["openWorldHint"], true, "{name}");
            assert_eq!(annotations["destructiveHint"], false, "{name}");
        }
        for name in ["operation_record", "sql_record", "protocol_run"] {
            assert_eq!(
                tool(&list, name)["annotations"]["readOnlyHint"],
                false,
                "{name}"
            );
        }
        assert_eq!(
            tool(&list, "protocol_run")["annotations"]["openWorldHint"],
            true
        );
        assert_eq!(tool(&list, "infer")["annotations"]["openWorldHint"], false);
        for name in ["operation_record", "sql_record", "operator_record"] {
            assert_eq!(
                tool(&list, name)["annotations"]["destructiveHint"],
                false,
                "{name}"
            );
        }
        for name in [
            "protocol_run",
            "protocol_edit",
            "history_restore",
            "protocol_write_file",
            "report_write",
        ] {
            assert_eq!(
                tool(&list, name)["annotations"]["destructiveHint"],
                true,
                "{name}"
            );
        }
        assert_eq!(
            tool(&list, "history_list")["annotations"]["readOnlyHint"],
            true
        );
    }

    #[test]
    fn instructions_open_with_when_to_use_arc_and_name_every_tool() {
        assert!(INSTRUCTIONS.starts_with("Use arc when"));
        for t in tools() {
            assert!(
                INSTRUCTIONS.contains(t.name),
                "INSTRUCTIONS omits {}",
                t.name
            );
        }
    }

    #[test]
    fn instructions_outline_the_workflow() {
        let outline = INSTRUCTIONS
            .split("\n\n")
            .find(|p| p.starts_with("The analyst workflow"))
            .expect("INSTRUCTIONS has an outline paragraph");
        let mut at = 0;
        for label in ["Scope:", "Profile:", "Build:", "Check:", "Present:"] {
            let found = outline[at..]
                .find(label)
                .unwrap_or_else(|| panic!("outline omits {label} in order"));
            at += found + label.len();
        }
        assert!(
            outline.contains("using-arcform"),
            "outline names using-arcform"
        );
        let mut skills = vec!["using-arcform"];
        let mut rest = outline;
        while let Some(open) = rest.find('(') {
            let close = rest[open..].find(')').expect("closing parenthesis") + open;
            skills.push(&rest[open + 1..close]);
            rest = &rest[close + 1..];
        }
        assert!(skills.len() >= 5, "outline names its skills: {skills:?}");
        for name in skills {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("skills")
                .join(name)
                .join("SKILL.md");
            assert!(path.is_file(), "no skill file for {name}");
        }
    }

    #[test]
    fn tools_list_registers_every_tool() {
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
            "operation_describe",
            "operation_record",
            "operator_record",
            "sql_recognise",
            "protocol_edit",
            "history_list",
            "history_restore",
            "sql_record",
            "protocol_create",
            "protocol_write_file",
            "report_write",
            "table_describe",
            "table_profile",
            "table_validate",
            "sql_query",
            "panel_check",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool `{expected}` in {names:?}"
            );
        }
        assert_eq!(names.len(), 23, "unexpected tool set: {names:?}");
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
    fn operation_describe_reachable_through_tools_call() {
        let listed = tools_call(&json!({ "name": "operation_describe", "arguments": {} }))
            .expect("operation_describe is registered");
        assert_eq!(listed["isError"], false);
        assert!(listed["structuredContent"]["operations"].is_array());

        let described = tools_call(&json!({
            "name": "operation_describe",
            "arguments": { "operation": "filter-rows" },
        }))
        .expect("operation_describe is registered");
        assert_eq!(described["isError"], false);
        assert_eq!(described["structuredContent"]["long_name"], "filter-rows");
    }

    #[test]
    fn operation_describe_unknown_operation_is_an_iserror_result_naming_it() {
        let result = tools_call(&json!({
            "name": "operation_describe",
            "arguments": { "operation": "nope" },
        }))
        .expect("an unknown operation is a result, not a protocol error");
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("`nope`")
        );
    }

    #[test]
    fn initialize_instructions_name_every_native_tool() {
        let result = initialize_result(&Value::Null);
        let instructions = result["instructions"].as_str().expect("instructions");
        for tool in [
            "protocol_run",
            "operator_describe",
            "operation_describe",
            "operation_record",
            "operator_record",
            "protocol_edit",
            "history_list",
            "history_restore",
            "sql_recognise",
            "sql_record",
        ] {
            assert!(instructions.contains(tool), "instructions omit `{tool}`");
        }
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
