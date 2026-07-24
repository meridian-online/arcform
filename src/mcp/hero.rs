//! The two MCP tools native to `arc` — the ones that make `arc mcp` more than a
//! FineType proxy.
//!
//! - `protocol_run` runs a Protocol and returns its live **Protocol+Run contract**
//!   (`contract_version` `b4/1`): the run's protocol, engine, params, assets and
//!   per-step outcome, the same JSON `arc run` writes under `build/.arcform/runs/`.
//! - `operator_describe` emits an operator's `with:` JSON Schema (or lists the
//!   catalog) so an agent or authoring UI can build and check a `with:` block.

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

/// The two native tools.
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
