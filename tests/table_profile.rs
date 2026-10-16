//! `arc table profile` and `arc table validate`, and the `arc mcp` tools
//! `table_profile` and `table_validate`, asked of the real `arc` binary:
//!
//!   1. **profile** — a table's columns come back with the types FineType gives
//!      the same rows as a CSV; the `json-schema` format is a schema that can be
//!      written into the Protocol and used; an empty table says so; a missing
//!      table is refused with the tables there are;
//!   2. **validate** — a table checked against a schema in DuckDB with FineType's
//!      extension: its own profile passes, a schema it breaks names the column
//!      and the rejects, a schema outside the Protocol is refused, and a missing
//!      extension names `FINETYPE_DUCKDB_EXT`.
//!
//! The profile tests need `finetype` at its version floor on PATH, and the
//! validate tests need `FINETYPE_DUCKDB_EXT`; each skips with a printed reason
//! when its tool is not there.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
#[cfg(feature = "mcp")]
use serde_json::json;

/// Whether `finetype` at arc's floor is on PATH; prints why when it is not.
fn finetype_present() -> bool {
    let out = match Command::new("finetype").arg("--version").output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("skipping: finetype is not on PATH ({e})");
            return false;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let version = text
        .split_whitespace()
        .next_back()
        .and_then(|v| semver::Version::parse(v).ok());
    match version {
        Some(v) if v >= semver::Version::new(0, 6, 54) => true,
        _ => {
            eprintln!("skipping: finetype is older than 0.6.54 ({})", text.trim());
            false
        }
    }
}

/// Whether the DuckDB extension is configured; prints why when it is not.
fn extension_present() -> bool {
    match std::env::var("FINETYPE_DUCKDB_EXT") {
        Ok(path) if Path::new(&path).exists() => true,
        found => {
            assert!(
                std::env::var_os("ARCFORM_REQUIRE_FINETYPE_EXT").is_none(),
                "ARCFORM_REQUIRE_FINETYPE_EXT is set, so this run promised a finetype \
                 extension, but FINETYPE_DUCKDB_EXT is {found:?}"
            );
            eprintln!("skipping: FINETYPE_DUCKDB_EXT does not name a built extension");
            false
        }
    }
}

struct Protocol {
    _root: tempfile::TempDir,
    dir: PathBuf,
    history: PathBuf,
}

const CONTACTS: &str = "CREATE TABLE contacts (joined VARCHAR, email VARCHAR, status VARCHAR);
    INSERT INTO contacts SELECT
        strftime(DATE '2024-01-01' + CAST(range AS INTEGER), '%Y-%m-%d'),
        'person' || range || '@example.com',
        CASE WHEN range % 2 = 0 THEN 'open' ELSE 'closed' END
      FROM range(40);";

impl Protocol {
    /// A Protocol whose database holds the SQL's tables.
    fn new(sql: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let protocol = Protocol {
            dir: root.path().join("rates"),
            history: root.path().join("history"),
            _root: root,
        };
        let out = protocol.run(protocol._root.path(), &["create-protocol", "rates"], None);
        assert!(out.status.success(), "create-protocol: {}", stderr(&out));
        let manifest = protocol.dir.join("arcform.yaml");
        let mut text = std::fs::read_to_string(&manifest).unwrap();
        text = format!("{text}\ndb: data.duckdb\n");
        std::fs::write(&manifest, text).unwrap();
        let conn = duckdb::Connection::open(protocol.dir.join("data.duckdb")).unwrap();
        conn.execute_batch(sql).unwrap();
        drop(conn);
        protocol
    }

    fn run(&self, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(cwd)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn arc");
        {
            let mut pipe = child.stdin.take().expect("stdin is piped");
            if let Some(text) = stdin {
                pipe.write_all(text.as_bytes()).unwrap();
            }
        }
        child.wait_with_output().expect("wait for arc")
    }

    fn arc(&self, args: &[&str]) -> Output {
        self.run(&self.dir, args, None)
    }

    fn write_schema(&self, path: &str, content: &str) {
        let out = self.run(&self.dir, &["protocol", "write-file", path], Some(content));
        assert!(out.status.success(), "write-file: {}", stderr(&out));
    }

    /// Every path under the Protocol's folder, so a refusal can be shown to
    /// have left it as it was.
    fn tree(&self) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, into: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                into.push(path.strip_prefix(base).unwrap().display().to_string());
                if path.is_dir() && !path.is_symlink() {
                    walk(&path, base, into);
                }
            }
        }
        let mut all = Vec::new();
        walk(&self.dir, &self.dir, &mut all);
        all.sort();
        all
    }

    /// One `tools/call` to a fresh `arc mcp` started in the Protocol's directory.
    #[cfg(feature = "mcp")]
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
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_json(out: &Output) -> Value {
    assert!(out.status.success(), "failed: {}", stderr(out));
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

/// `column -> type` of a FineType profile.
fn types(profile: &Value) -> Vec<(String, String)> {
    profile["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|c| {
            (
                c["column"].as_str().unwrap().to_string(),
                c["type"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn profiles_table_columns() {
    if !finetype_present() {
        return;
    }
    let protocol = Protocol::new(CONTACTS);
    let got = stdout_json(&protocol.arc(&["table", "profile", "contacts"]));
    assert_eq!(got["table"], "contacts");
    assert_eq!(got["rows_profiled"], 40);

    // The same rows as a CSV, handed to finetype directly.
    let csv = protocol._root.path().join("contacts.csv");
    let conn = duckdb::Connection::open(protocol.dir.join("data.duckdb")).unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT * FROM contacts) TO '{}' (HEADER)",
        csv.display()
    ))
    .unwrap();
    drop(conn);
    let direct = Command::new("finetype")
        .args(["profile", "-o", "json", "-f"])
        .arg(&csv)
        .output()
        .unwrap();
    let direct: Value = serde_json::from_slice(&direct.stdout).unwrap();
    assert_eq!(types(&got), types(&direct));
    let named: Vec<String> = types(&got).into_iter().map(|(c, _)| c).collect();
    assert_eq!(named, ["joined", "email", "status"]);
    assert!(
        types(&got).iter().any(|(_, t)| t.contains("email")),
        "{got}"
    );
    assert!(types(&got).iter().any(|(_, t)| t.contains("date")), "{got}");
}

#[test]
fn json_schema_format_saves_through_write_file() {
    if !finetype_present() {
        return;
    }
    let protocol = Protocol::new(CONTACTS);
    let out = protocol.arc(&["table", "profile", "contacts", "--format", "json-schema"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let schema = String::from_utf8(out.stdout).unwrap();
    // A clean JSON Schema: FineType's own, with an `$id` naming the table.
    let parsed: Value = serde_json::from_str(&schema).unwrap();
    assert_eq!(parsed["$id"], "finetype://contacts", "{schema}");
    assert!(parsed.get("rows_profiled").is_none(), "{schema}");
    assert!(parsed.get("table").is_none(), "{schema}");
    protocol.write_schema("schemas/contacts.json", &schema);

    let csv = protocol._root.path().join("contacts.csv");
    let conn = duckdb::Connection::open(protocol.dir.join("data.duckdb")).unwrap();
    conn.execute_batch(&format!(
        "COPY (SELECT * FROM contacts) TO '{}' (HEADER)",
        csv.display()
    ))
    .unwrap();
    drop(conn);
    let checked = Command::new("finetype")
        .arg("validate")
        .arg(&csv)
        .arg(protocol.dir.join("schemas/contacts.json"))
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "finetype validate: {}{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn empty_table_says_so() {
    let protocol = Protocol::new("CREATE TABLE nothing (a VARCHAR, b INTEGER);");
    let got = stdout_json(&protocol.arc(&["table", "profile", "nothing"]));
    assert_eq!(got["rows_profiled"], 0);
    assert_eq!(got["table"], "nothing");
    assert!(
        got["message"].as_str().unwrap().contains("no rows"),
        "{got}"
    );

    // No schema can come from no rows, and nothing is written.
    let before = protocol.tree();
    let out = protocol.arc(&["table", "profile", "nothing", "--format", "json-schema"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no rows to infer a schema from"),
        "{}",
        stderr(&out)
    );
    assert!(out.stdout.is_empty());
    assert_eq!(protocol.tree(), before);
}

#[test]
fn refuses_missing_table() {
    let protocol = Protocol::new(CONTACTS);
    let before = protocol.tree();
    let out = protocol.arc(&["table", "profile", "nope"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("no table or view named nope"), "{err}");
    assert!(err.contains("contacts"), "{err}");
    assert_eq!(protocol.tree(), before);
}

#[test]
#[cfg(feature = "mcp")]
fn mcp_table_profile_matches_the_command() {
    if !finetype_present() {
        return;
    }
    let protocol = Protocol::new(CONTACTS);
    let cli = stdout_json(&protocol.arc(&["table", "profile", "contacts", "--sample-size", "10"]));
    let result = protocol.call(
        "table_profile",
        json!({ "table": "contacts", "sample_size": 10 }),
    );
    assert_ne!(result["isError"], true, "{result}");
    let mcp: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(mcp["rows_profiled"], 10);
    assert_eq!(types(&mcp), types(&cli));
}

#[test]
fn validate_table_passes_its_own_schema() {
    if !finetype_present() || !extension_present() {
        return;
    }
    let protocol = Protocol::new(CONTACTS);
    let out = protocol.arc(&["table", "profile", "contacts", "--format", "json-schema"]);
    assert!(out.status.success(), "{}", stderr(&out));
    protocol.write_schema(
        "schemas/contacts.json",
        &String::from_utf8(out.stdout).unwrap(),
    );

    let out = protocol.arc(&[
        "table",
        "validate",
        "contacts",
        "--schema",
        "schemas/contacts.json",
    ]);
    let got = stdout_json(&out);
    assert_eq!(got["valid"], true, "{got}");
    assert_eq!(got["table"], "contacts");
    assert_eq!(got["schema"], "schemas/contacts.json");
    assert!(got["columns"].as_array().unwrap().len() >= 3, "{got}");

    #[cfg(feature = "mcp")]
    {
        let result = protocol.call(
            "table_validate",
            json!({ "table": "contacts", "schema": "schemas/contacts.json" }),
        );
        assert_ne!(result["isError"], true, "{result}");
        assert_eq!(result["structuredContent"]["valid"], true, "{result}");
    }
}

#[test]
fn validate_table_reports_rejects() {
    if !extension_present() {
        return;
    }
    let protocol = Protocol::new(CONTACTS);
    protocol.write_schema(
        "schemas/strict.json",
        r#"{"type":"object","properties":{"status":{"type":"string","enum":["open"]}}}"#,
    );
    let out = protocol.arc(&[
        "table",
        "validate",
        "contacts",
        "--schema",
        "schemas/strict.json",
    ]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let got: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(got["valid"], false, "{got}");
    let status = got["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["column_name"] == "status")
        .unwrap_or_else(|| panic!("no status column in {got}"));
    assert_eq!(status["rejects"], 20, "{got}");
    assert_eq!(status["total"], 40, "{got}");
    assert!(!status["sample_message"].as_str().unwrap().is_empty());
}

#[test]
fn validate_refuses_schema_outside_protocol() {
    let protocol = Protocol::new(CONTACTS);
    let outside = protocol._root.path().join("outside.json");
    std::fs::write(&outside, "{}").unwrap();
    let before = protocol.tree();
    for schema in ["../outside.json", outside.to_str().unwrap()] {
        let out = protocol.arc(&["table", "validate", "contacts", "--schema", schema]);
        assert!(!out.status.success(), "{schema}");
        let err = stderr(&out);
        assert!(err.contains("outside the Protocol folder"), "{err}");
    }
    #[cfg(feature = "mcp")]
    {
        let result = protocol.call(
            "table_validate",
            json!({ "table": "contacts", "schema": "../outside.json" }),
        );
        assert_eq!(result["isError"], true, "{result}");
    }
    assert_eq!(protocol.tree(), before);
}

#[test]
fn validate_without_extension_names_the_variable() {
    let protocol = Protocol::new(CONTACTS);
    protocol.write_schema("schemas/any.json", "{}");
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&protocol.dir)
        .env("ARCFORM_HISTORY_DIR", &protocol.history)
        .env_remove("FINETYPE_DUCKDB_EXT")
        .args([
            "table",
            "validate",
            "contacts",
            "--schema",
            "schemas/any.json",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("FINETYPE_DUCKDB_EXT"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn refuses_a_sample_size_of_nothing() {
    let protocol = Protocol::new(CONTACTS);
    let before = protocol.tree();
    let out = protocol.arc(&["table", "profile", "contacts", "--sample-size", "0"]);
    assert!(!out.status.success());
    #[cfg(feature = "mcp")]
    {
        let result = protocol.call(
            "table_profile",
            json!({ "table": "contacts", "sample_size": 0 }),
        );
        assert_eq!(result["isError"], true, "{result}");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("at least 1"),
            "{result}"
        );
    }
    assert_eq!(protocol.tree(), before);
}
