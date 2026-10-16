//! The five `arc mcp` tools that federate the `finetype` CLI (`infer`, `profile`,
//! `taxonomy`, `validate`, `generate`), called through `arc mcp` against the real
//! binary with every argument each tool's schema advertises.
//!
//! The unit tests in `src/mcp/finetype.rs` drive a stand-in `finetype` that accepts
//! any arguments, so a flag finetype drops would pass them and fail an agent's call.
//! Here the arguments come from the schemas `tools/list` returns: a property with no
//! value in [`calls`], or an enum value no call uses, fails the test, so a new
//! argument cannot reach a schema untested. finetype then rejects any flag it does
//! not know, and the call comes back `isError: true`.
//!
//! Needs `finetype` at arc's version floor on PATH. It skips with a printed reason
//! when there is none, unless `ARCFORM_REQUIRE_FINETYPE_CLI` is set, as CI sets it
//! after installing a pinned release; then it fails.

#![cfg(feature = "mcp")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

#[allow(dead_code)]
mod common;

const TOOLS: [&str; 5] = ["infer", "profile", "taxonomy", "validate", "generate"];

const CONTACTS: &str = "email,joined\n\
    a@example.com,2024-01-01\n\
    b@example.com,2024-01-02\n\
    c@example.com,2024-01-03\n";

/// Each tool's calls. Between them they set every property the tool advertises and
/// use every value of each enum property; `infer` takes `values` or `value`, so it
/// needs a call for each.
fn calls(csv: &Path, schema: &Path) -> Vec<(&'static str, Value)> {
    let csv = csv.to_str().unwrap();
    let schema = schema.to_str().unwrap();
    vec![
        (
            "infer",
            json!({ "values": ["a@example.com", "b@example.com"], "header": "email" }),
        ),
        (
            "infer",
            json!({ "value": "a@example.com", "header": "email" }),
        ),
        (
            "profile",
            json!({ "file": csv, "format": "json", "sample_size": 10, "delimiter": "," }),
        ),
        (
            "profile",
            json!({ "file": csv, "format": "json-schema", "stats": true }),
        ),
        // `labels` is finetype's own default for its taxonomy, so naming it passes the
        // flag without needing a taxonomy directory of the test's own.
        (
            "taxonomy",
            json!({ "type_key": "identity.person.email", "format": "json", "full": true, "taxonomy_file": "labels" }),
        ),
        (
            "taxonomy",
            json!({ "domain": "identity", "category": "person", "format": "csv" }),
        ),
        ("taxonomy", json!({ "format": "json-schema" })),
        ("taxonomy", json!({ "format": "plain" })),
        (
            "validate",
            json!({ "file": csv, "schema": schema, "format": "json", "strict": true }),
        ),
        (
            "validate",
            json!({ "file": csv, "schema": schema, "format": "plain" }),
        ),
        (
            "generate",
            json!({ "samples": 1, "seed": 7, "localized": true, "taxonomy_file": "labels" }),
        ),
    ]
}

/// Each finetype tool's advertised input schema, from `tools/list`.
fn advertised_schemas(dir: &Path) -> BTreeMap<String, Value> {
    let lines = common::mcp_session(
        dir,
        &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })],
    );
    let response: Value = serde_json::from_str(&lines[0]).unwrap();
    response["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools")
        .iter()
        .filter(|tool| TOOLS.contains(&tool["name"].as_str().unwrap()))
        .map(|tool| {
            (
                tool["name"].as_str().unwrap().to_string(),
                tool["inputSchema"].clone(),
            )
        })
        .collect()
}

#[test]
fn every_advertised_argument_reaches_the_real_finetype() {
    if !common::finetype_cli_present() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("contacts.csv");
    std::fs::write(&csv, CONTACTS).unwrap();
    let schema = dir.path().join("contacts.schema.json");
    let profiled = Command::new("finetype")
        .args(["profile", "--output", "json-schema", "--file"])
        .arg(&csv)
        .output()
        .unwrap();
    assert!(
        profiled.status.success(),
        "{}",
        String::from_utf8_lossy(&profiled.stderr)
    );
    std::fs::write(&schema, &profiled.stdout).unwrap();

    let calls = calls(&csv, &schema);

    // The calls cover each schema, and use nothing a schema does not advertise.
    let schemas = advertised_schemas(dir.path());
    assert_eq!(schemas.keys().map(String::as_str).collect::<Vec<_>>(), {
        let mut names = TOOLS.to_vec();
        names.sort();
        names
    });
    for (tool, schema) in &schemas {
        let properties = schema["properties"].as_object().unwrap();
        let used: Vec<&serde_json::Map<String, Value>> = calls
            .iter()
            .filter(|(name, _)| name == tool)
            .map(|(_, args)| args.as_object().unwrap())
            .collect();
        let advertised: BTreeSet<&String> = properties.keys().collect();
        let set: BTreeSet<&String> = used.iter().flat_map(|args| args.keys()).collect();
        assert_eq!(
            set, advertised,
            "the calls to `{tool}` must set exactly the properties its schema advertises"
        );
        for (property, spec) in properties {
            let Some(values) = spec["enum"].as_array() else {
                continue;
            };
            for value in values {
                assert!(
                    used.iter().any(|args| args.get(property) == Some(value)),
                    "no call to `{tool}` sets `{property}` to {value}"
                );
            }
        }
    }

    // Every call succeeds against the real binary.
    for (tool, args) in &calls {
        let request = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        });
        let lines = common::mcp_session(dir.path(), &[request]);
        let response: Value = serde_json::from_str(&lines[0]).unwrap();
        let result = &response["result"];
        assert_eq!(
            result["isError"], false,
            "`{tool}` with {args} failed: {result}"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        assert!(
            !text.trim().is_empty(),
            "`{tool}` with {args} returned nothing"
        );
    }
}
