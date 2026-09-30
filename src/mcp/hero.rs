//! The MCP tools native to `arc` — the ones that make `arc mcp` more than a
//! FineType proxy.
//!
//! - `protocol_run` runs a Protocol and returns its live **Protocol+Run contract**
//!   (`contract_version` `b4/1`): the run's protocol, engine, params, assets and
//!   per-step outcome, the same JSON `arc run` writes under `build/.arcform/runs/`.
//! - `operator_describe` emits an operator's `with:` JSON Schema (or lists the
//!   catalog) so an agent or authoring UI can build and check a `with:` block.
//! - `operation_describe` lists the SQL operations arc holds, or describes an
//!   operation or an operator by its name: the same JSON `arc operation list --json`
//!   and `arc operation describe` print, both read through the one lookup in
//!   `record`.
//! - `operation_record` records an operation as a new step of a Protocol, through
//!   the same record path `arc operation record` takes, so the same request writes
//!   the same bytes from either.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{ToolDef, ToolOutput, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// protocol_run
// ─────────────────────────────────────────────────────────────────────────────

fn protocol_run(args: &Value) -> ToolResult {
    let dir = match args.get("dir").and_then(Value::as_str) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()
            .map_err(|e| format!("no `dir` given and the current directory is unavailable: {e}"))?,
    };
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let cli_params = parse_params(args.get("params"))?;

    let manifest = crate::manifest::Manifest::load(&dir).map_err(|e| e.to_string())?;
    let db_path = manifest.db_path(&dir);
    let engine = crate::engine::DuckDbEngine;
    let state = crate::state::DuckDbStateBackend::new(&db_path);

    // The contract is written to `<dir>/build/.arcform/runs/<run_id>.json` at run end
    // (on success and failure alike). Snapshot the directory, run, then read the file
    // the run just added.
    let runs = crate::contract::runs_dir(&dir);
    let before = list_contracts(&runs);
    let run_result = crate::runner::run_with_params(&dir, &engine, &state, force, &cli_params);

    match newest_new_contract(&runs, &before) {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading run contract {}: {e}", path.display()))?;
            let contract: Value = serde_json::from_str(&text)
                .map_err(|e| format!("parsing run contract {}: {e}", path.display()))?;
            Ok(ToolOutput::json(contract))
        }
        None => Err(match run_result {
            Err(e) => format!("run failed before a contract was written: {e}"),
            Ok(()) => {
                "the run produced no contract (a protocol with no steps writes none)".to_string()
            }
        }),
    }
}

/// Runtime parameter overrides, accepted as an object `{key: value}` or an array of
/// `"KEY=VALUE"` strings.
fn parse_params(value: Option<&Value>) -> std::result::Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(map) = value.as_object() {
        return Ok(map
            .iter()
            .map(|(key, value)| {
                let value = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (key.clone(), value)
            })
            .collect());
    }
    if let Some(array) = value.as_array() {
        let mut out = Vec::new();
        for item in array {
            let entry = item
                .as_str()
                .ok_or_else(|| "`params` array items must be \"KEY=VALUE\" strings".to_string())?;
            let (key, value) = entry
                .split_once('=')
                .ok_or_else(|| format!("param `{entry}` must be KEY=VALUE"))?;
            out.push((key.trim().to_string(), value.to_string()));
        }
        return Ok(out);
    }
    Err("`params` must be an object {key: value} or an array of \"KEY=VALUE\" strings".to_string())
}

/// The `.json` contract files currently in `runs` (empty if the directory is absent).
fn list_contracts(runs: &Path) -> BTreeSet<PathBuf> {
    let mut set = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(runs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                set.insert(path);
            }
        }
    }
    set
}

/// The newest `.json` contract in `runs` that was not in `exclude` — i.e. the one this
/// run just wrote.
fn newest_new_contract(runs: &Path, exclude: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = std::fs::read_dir(runs).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") || exclude.contains(&path) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(best_time, _)| modified >= *best_time)
        {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

fn protocol_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." },
            "force": { "type": "boolean", "description": "Re-run every step, ignoring staleness." },
            "params": {
                "type": "object",
                "description": "Runtime parameter overrides, as { key: value }.",
                "additionalProperties": { "type": "string" }
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// operator_describe
// ─────────────────────────────────────────────────────────────────────────────

fn operator_describe(args: &Value) -> ToolResult {
    match args.get("operator").and_then(Value::as_str) {
        None => {
            let operators = crate::operator::catalog_names();
            Ok(ToolOutput::json(json!({
                "operators": operators,
                "hint": "call operator_describe with { \"operator\": \"<name>\" } for its with: JSON Schema",
            })))
        }
        Some(name) => match crate::operator::with_schema(name) {
            Some(schema) => Ok(ToolOutput::json(schema)),
            None => Err(format!(
                "unknown operator `{name}` — call operator_describe with no arguments to list the catalog"
            )),
        },
    }
}

fn operator_describe_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operator": {
                "type": "string",
                "description": "Operator name (e.g. parquet_export). Omit to list every operator in the catalog."
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// operation_describe
// ─────────────────────────────────────────────────────────────────────────────

/// List the operations arc holds, or describe the operation or the operator named
/// by `operation`.
///
/// The answer is the catalogue's own: the listing is what `arc operation list
/// --json` prints, under one key, and a description is what `arc operation
/// describe` prints, found through the same lookup. Nothing here builds a
/// description of its own. The listing holds operations alone. An `operation` that
/// is not a string is refused rather than read as absent, so a client that sends
/// the wrong type is told so and does not get a listing back.
fn operation_describe(args: &Value) -> ToolResult {
    match args.get("operation") {
        None | Some(Value::Null) => {
            let operations: Vec<Value> = crate::record::operations()
                .iter()
                .map(|op| op.listing())
                .collect();
            Ok(ToolOutput::json(json!({ "operations": operations })))
        }
        Some(Value::String(name)) => match crate::record::describe(name) {
            Some(description) => Ok(ToolOutput::json(description)),
            None => Err(format!(
                "no operation or operator called `{name}` — call operation_describe with no arguments to list the operations arc holds"
            )),
        },
        Some(other) => Err(format!(
            "`operation` must be a string, the long name of an operation, not {other} — call operation_describe with no arguments to list the operations arc holds"
        )),
    }
}

fn operation_describe_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "description": "An operation's long name (e.g. filter-rows) or an operator's name (e.g. splink_resolve). Omit to list every operation arc holds."
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// operation_record
// ─────────────────────────────────────────────────────────────────────────────

/// Record an operation as a new step at the end of a Protocol.
///
/// The request is handed to the record path `arc operation record` takes, which
/// writes the step's SQL from the operation's catalogue entry; nothing here names
/// an operation or an argument. `dir` defaults to `.`, the server's working
/// directory, as `--dir` does. `arguments` is an object, absent when the
/// operation takes none. A refusal is an error result naming what was wrong, with
/// the Protocol's directory untouched.
fn operation_record(args: &Value) -> ToolResult {
    let dir = PathBuf::from(args.get("dir").and_then(Value::as_str).unwrap_or("."));
    let operation = required_string(args, "operation")?;
    let on = required_string(args, "on")?;
    let name = required_string(args, "name")?;
    let arguments = match args.get("arguments") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(other) => {
            return Err(format!(
                "`arguments` must be an object of the operation's arguments, not {other}"
            ));
        }
    };
    let history = crate::history::LocalHistory::open_default().map_err(|e| e.to_string())?;
    let model = crate::record::record_operation(&dir, operation, on, name, &arguments, &history)
        .map_err(|e| e.to_string())?;
    Ok(ToolOutput::json(json!({
        "step": name,
        "model": model.display().to_string(),
    })))
}

/// The string `key` of a tool's arguments, refused by name when absent or not a string.
fn required_string<'a>(args: &'a Value, key: &str) -> std::result::Result<&'a str, String> {
    match args.get(key) {
        Some(Value::String(value)) => Ok(value),
        None | Some(Value::Null) => Err(format!("`{key}` is required")),
        Some(other) => Err(format!("`{key}` must be a string, not {other}")),
    }
}

fn operation_record_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": { "type": "string", "description": "The operation's long name (e.g. filter-rows), as operation_describe lists them." },
            "on": { "type": "string", "description": "The table the operation is applied to: one a step of the Protocol makes." },
            "name": { "type": "string", "description": "The new step's name, which is also the name of the table it makes." },
            "arguments": { "type": "object", "description": "The operation's arguments, as the `parameters` JSON Schema operation_describe returns for it describes them." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["operation", "on", "name"]
    })
}

/// The tools native to `arc`.
pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "protocol_run",
            description: "Run an arc Protocol and return its live Protocol+Run contract (protocol, engine, params, assets, per-step outcome).",
            input_schema: protocol_run_schema,
            handler: protocol_run,
        },
        ToolDef {
            name: "operator_describe",
            description: "Emit an operator's `with:` JSON Schema for authoring — or list the operator catalog when called with no operator.",
            input_schema: operator_describe_schema,
            handler: operator_describe,
        },
        ToolDef {
            name: "operation_describe",
            description: "List the SQL operations arc holds when called with no operation — or describe one operation or operator (what it does, what its step reads and writes by role, and the JSON Schema of what it takes) by its name.",
            input_schema: operation_describe_schema,
            handler: operation_describe,
        },
        ToolDef {
            name: "operation_record",
            description: "Record an SQL operation as a new step at the end of a Protocol, from its long name, the table it is applied to, the step's name and its arguments. Writes a generated model and the step naming it; runs nothing.",
            input_schema: operation_record_schema,
            handler: operation_record,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_describe_lists_the_catalog() {
        let result = operator_describe(&json!({})).expect("listing succeeds");
        let operators = result.structured.as_ref().unwrap()["operators"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(operators.contains(&"parquet_export".to_string()));
        assert!(operators.contains(&"finetype_validate".to_string()));
    }

    #[test]
    fn operator_describe_emits_a_with_schema() {
        let result =
            operator_describe(&json!({ "operator": "http_fetch" })).expect("describe succeeds");
        let schema = result.structured.expect("a structured schema");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["url"].is_object());
        assert!(schema["properties"]["out"].is_object());
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"url") && required.contains(&"out"));
    }

    #[test]
    fn operator_describe_unknown_operator_errors() {
        let err = operator_describe(&json!({ "operator": "nope" })).unwrap_err();
        assert!(err.contains("unknown operator"), "message: {err}");
    }

    #[test]
    fn operation_describe_lists_each_operation_under_one_key() {
        for args in [json!({}), json!({ "operation": null })] {
            let result = operation_describe(&args).expect("listing succeeds");
            let listed = result.structured.as_ref().expect("a structured listing");
            let object = listed.as_object().expect("the listing is an object");
            assert_eq!(object.len(), 1, "one key only: {listed}");
            let entries = object["operations"]
                .as_array()
                .expect("an array of entries");
            assert!(!entries.is_empty(), "the catalogue holds an operation");
            for entry in entries {
                let keys: Vec<&str> = entry
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert_eq!(keys, ["long_name", "summary"], "entry: {entry}");
            }
            let long_names: Vec<&str> = entries
                .iter()
                .filter_map(|e| e["long_name"].as_str())
                .collect();
            assert!(
                long_names.contains(&"filter-rows"),
                "listed: {long_names:?}"
            );
        }
    }

    #[test]
    fn operation_describe_returns_the_catalogues_description() {
        let result =
            operation_describe(&json!({ "operation": "filter-rows" })).expect("describe succeeds");
        let described = result.structured.expect("a structured description");
        assert_eq!(described["long_name"], "filter-rows");
        assert!(described["reads"].is_array(), "described: {described}");
        assert!(described["writes"].is_array(), "described: {described}");
        assert!(
            described.get("applied_to").is_none(),
            "described: {described}"
        );
        assert_eq!(described["parameters"]["required"], json!(["where"]));
        // The text an MCP client shows is the same document.
        let from_text: Value = serde_json::from_str(&result.text).expect("text is JSON");
        assert_eq!(from_text, described);
    }

    #[test]
    fn operation_describe_unknown_operation_errors_naming_it() {
        let err = operation_describe(&json!({ "operation": "nope" })).unwrap_err();
        assert!(err.contains("`nope`"), "message: {err}");
        assert!(
            err.contains("no operation or operator called"),
            "message: {err}"
        );
    }

    #[test]
    fn operation_describe_refuses_an_operation_that_is_not_a_string() {
        let err = operation_describe(&json!({ "operation": ["filter-rows"] })).unwrap_err();
        assert!(err.contains("must be a string"), "message: {err}");
        assert!(err.contains("[\"filter-rows\"]"), "message: {err}");
    }

    #[test]
    fn operation_record_refuses_a_request_missing_a_key_or_of_the_wrong_shape() {
        let dir = tempfile::tempdir().unwrap();
        let request = json!({
            "dir": dir.path().to_str().unwrap(),
            "operation": "filter-rows",
            "on": "orders",
            "name": "big_orders",
            "arguments": { "where": "amount > 100" },
        });
        let refusals = [
            ("operation", Value::Null, "`operation` is required"),
            ("on", json!(null), "`on` is required"),
            (
                "name",
                json!(["big_orders"]),
                "`name` must be a string, not [\"big_orders\"]",
            ),
            (
                "arguments",
                json!("where=amount > 100"),
                "`arguments` must be an object",
            ),
        ];
        for (key, value, message) in refusals {
            let mut args = request.clone();
            if value.is_null() && key == "operation" {
                args.as_object_mut().unwrap().remove(key);
            } else {
                args[key] = value;
            }
            let err = operation_record(&args).unwrap_err();
            assert!(err.contains(message), "{key}: {err}");
        }
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "a refused request wrote into the directory"
        );
    }

    #[test]
    fn parse_params_accepts_objects_and_arrays() {
        let from_object = parse_params(Some(&json!({ "threshold": "25", "n": 3 }))).unwrap();
        assert!(from_object.contains(&("threshold".to_string(), "25".to_string())));
        assert!(from_object.contains(&("n".to_string(), "3".to_string())));

        let from_array = parse_params(Some(&json!(["date=2026-07-25"]))).unwrap();
        assert_eq!(
            from_array,
            vec![("date".to_string(), "2026-07-25".to_string())]
        );

        assert_eq!(parse_params(None).unwrap(), Vec::new());
        assert!(parse_params(Some(&json!("nope"))).is_err());
    }

    #[test]
    fn protocol_run_returns_the_b4_protocol_and_run_contract() {
        let dir = tempfile::tempdir().unwrap();
        // A minimal Protocol: one no-op command step (executes via `sh -c`), which is
        // enough for the runner to build and write a contract.
        std::fs::write(
            dir.path().join("arcform.yaml"),
            "name: mcp_smoke\nsteps:\n  - name: noop\n    command: \"true\"\n",
        )
        .unwrap();

        let result = protocol_run(&json!({ "dir": dir.path().to_str().unwrap() }))
            .expect("protocol_run succeeds");
        let contract = result.structured.expect("a structured contract");

        assert_eq!(
            contract["contract_version"], "b4/1",
            "protocol_run must return the live Protocol+Run contract"
        );
        assert_eq!(contract["run"]["protocol"]["name"], "mcp_smoke");
        assert_eq!(contract["run"]["outcome"], "success");
        assert!(contract["steps"].is_array());
        assert!(contract["assets"].is_array());
        let steps = contract["steps"].as_array().unwrap();
        assert!(steps.iter().any(|s| s["name"] == "noop"));
    }
}
