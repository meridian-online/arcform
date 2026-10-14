//! `arc sql query` runs read-only SELECTs against a Protocol's database and prints
//! what they return, without changing it:
//!
//!   1. **rows** — each statement's columns and types, and its rows as arrays in the
//!      order of the columns;
//!   2. **cap** — at most `--limit` rows (100 by default), saying so and how many were
//!      held back;
//!   3. **gate** — a text holding anything but SELECTs is refused whole, before any of
//!      it runs, and the database's bytes are the same afterwards;
//!   4. **backstop** — a side effect smuggled inside a SELECT fails on the read-only
//!      connection;
//!   5. **tool** — `sql_query` returns the verb's JSON and is annotated read-only.

#[allow(dead_code)]
mod common;

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const KEEP: &str = "to keep a query as a step: arc sql record '<statement>' --name <name>";

/// A Protocol whose database `data.duckdb` holds `t` (one INTEGER column `i`, 1..=3)
/// and `many` (150 rows).
fn protocol_with_data() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: p\nengine: duckdb\ndb: data.duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE t (i INTEGER);
         INSERT INTO t VALUES (1), (2), (3);
         CREATE TABLE many AS SELECT range AS n FROM range(150);
         CREATE SEQUENCE seq;
         CREATE TABLE odd_numbers (d DOUBLE, h HUGEINT, m DECIMAL(20,2));
         INSERT INTO odd_numbers VALUES
           ('NaN', 170141183460469231731687303715884105727, 12345678901234567.89),
           ('Infinity', 1, 0.10);",
    )
    .unwrap();
    drop(conn);
    dir
}

fn query(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_HISTORY_DIR", dir.join(".history"))
        .env("ARCFORM_DB_DIR", dir.join(".db"))
        .args(["sql", "query"])
        .args(args)
        .output()
        .expect("spawn arc")
}

fn query_json(dir: &Path, args: &[&str]) -> Value {
    let out = query(dir, args);
    assert!(out.status.success(), "query failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn sha(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}

#[test]
fn select_returns_rows_as_json() {
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT i FROM t ORDER BY i"]);
    assert_eq!(
        got,
        json!({
            "results": [{
                "statement": 1,
                "columns": [{"name": "i", "type": "INTEGER"}],
                "rows": [[1], [2], [3]],
                "row_count": 3,
                "capped": false,
                "rows_held_back": null,
            }],
            "keep": KEEP,
        })
    );
}

#[test]
fn query_over_cap_is_capped_and_says_so() {
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT n FROM many ORDER BY n"]);
    let result = &got["results"][0];
    assert_eq!(result["rows"].as_array().unwrap().len(), 100);
    assert_eq!(result["rows"][99], json!([99]));
    assert_eq!(result["row_count"], 100);
    assert_eq!(result["capped"], true);
    assert_eq!(result["rows_held_back"], 50);
}

#[test]
fn limit_flag_changes_cap() {
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT n FROM many", "--limit", "10"]);
    let result = &got["results"][0];
    assert_eq!(result["rows"].as_array().unwrap().len(), 10);
    assert_eq!(result["capped"], true);
    assert_eq!(result["rows_held_back"], 140);

    let got = query_json(dir.path(), &["SELECT i FROM t", "--limit", "3"]);
    assert_eq!(got["results"][0]["capped"], false);
    assert_eq!(got["results"][0]["rows_held_back"], Value::Null);
}

#[test]
fn writes_are_refused_and_bytes_unchanged() {
    let dir = protocol_with_data();
    let db = dir.path().join("data.duckdb");
    let before = sha(&db);
    for sql in [
        "INSERT INTO t VALUES (9)",
        "CREATE TABLE x(i INT)",
        "COPY t TO 'out.csv'",
        "ATTACH 'other.duckdb'",
    ] {
        let out = query(dir.path(), &[sql]);
        assert!(!out.status.success(), "{sql} was not refused");
        let err = stderr(&out);
        assert!(
            err.contains(
                "only read-only SELECT statements can be queried; refused before anything ran: "
            ),
            "{sql}: {err}"
        );
        assert!(out.stdout.is_empty(), "{sql}: printed {out:?}");
        assert_eq!(before, sha(&db), "{sql} changed the database");
        assert!(!dir.path().join("out.csv").exists(), "{sql} wrote out.csv");
        assert!(
            !dir.path().join("other.duckdb").exists(),
            "{sql} made other.duckdb"
        );
    }
}

#[test]
fn mixed_batch_is_refused_whole() {
    let dir = protocol_with_data();
    let db = dir.path().join("data.duckdb");
    let before = sha(&db);
    let out = query(dir.path(), &["SELECT 1; INSERT INTO t VALUES (9)"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("refused before anything ran"),
        "{}",
        stderr(&out)
    );
    assert!(out.stdout.is_empty(), "the SELECT ran: {out:?}");
    assert_eq!(before, sha(&db));
}

#[test]
fn several_selects_return_one_result_each() {
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT 1 AS a; SELECT 2 AS b"]);
    let results = got["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["statement"], 1);
    assert_eq!(
        results[0]["columns"],
        json!([{"name": "a", "type": "INTEGER"}])
    );
    assert_eq!(results[0]["rows"], json!([[1]]));
    assert_eq!(results[1]["statement"], 2);
    assert_eq!(
        results[1]["columns"],
        json!([{"name": "b", "type": "INTEGER"}])
    );
    assert_eq!(results[1]["rows"], json!([[2]]));
}

#[test]
fn query_side_effect_inside_select_is_stopped() {
    let dir = protocol_with_data();
    let db = dir.path().join("data.duckdb");
    let before = sha(&db);
    // Each of these is a SELECT, so the gate lets it through; what stops it is
    // DuckDB: `query()` takes a SELECT alone, and `nextval` would advance a
    // sequence, a write the read-only connection refuses.
    for (sql, why) in [
        (
            "SELECT * FROM query('CREATE TABLE x(i INT)')",
            "Expected a single SELECT statement",
        ),
        ("SELECT nextval('seq')", "read-only"),
    ] {
        let out = query(dir.path(), &[sql]);
        assert!(!out.status.success(), "{sql}: {out:?}");
        assert!(out.stdout.is_empty(), "{sql}: {out:?}");
        let err = stderr(&out);
        assert!(err.contains(why), "{sql}: {err}");
        assert!(!err.contains("refused before anything ran"), "{sql}: {err}");
        assert_eq!(before, sha(&db), "{sql} changed the database");
    }
    let conn = duckdb::Connection::open(&db).unwrap();
    let made: i64 = conn
        .query_row(
            "SELECT count(*) FROM information_schema.tables WHERE table_name = 'x'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(made, 0);
    let next: i64 = conn
        .query_row("SELECT nextval('seq')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(next, 1, "the sequence had advanced");
}

#[test]
fn columns_named_alike_or_like_an_alias_keep_their_values() {
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT 1 AS a, 2 AS a, 3 AS q, 4 AS t"]);
    let result = &got["results"][0];
    assert_eq!(result["rows"], json!([[1, 2, 3, 4]]));
    let names: Vec<&str> = result["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "a", "q", "t"]);
}

#[test]
fn query_writes_non_finite_and_wide_numbers_as_strings() {
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT d, h, m FROM odd_numbers"]);
    assert_eq!(
        got["results"][0]["rows"],
        json!([
            [
                "NaN",
                "170141183460469231731687303715884105727",
                "12345678901234567.89"
            ],
            ["Infinity", "1", "0.10"],
        ])
    );
}

#[test]
fn a_text_that_does_not_parse_is_refused() {
    let dir = protocol_with_data();
    let out = query(dir.path(), &["selec 1"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("refused before anything ran"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn query_records_no_history() {
    let dir = protocol_with_data();
    query_json(dir.path(), &["SELECT 1"]);
    let history = dir.path().join(".history");
    let entries = std::fs::read_dir(&history).map(|d| d.count()).unwrap_or(0);
    assert_eq!(entries, 0, "query wrote into the history folder");
}

#[cfg(feature = "mcp")]
#[test]
fn sql_query_tool_matches_verb_and_is_read_only() {
    let dir = protocol_with_data();
    let sql = "SELECT i FROM t ORDER BY i; SELECT n FROM many";
    let verb = query_json(dir.path(), &[sql, "--limit", "5"]);
    let history = dir.path().join(".history");
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "sql_query", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "sql_query", "arguments": {"sql": sql, "limit": 5}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
            "name": "sql_query", "arguments": {"sql": "DROP TABLE t"}}}),
    ];
    let lines = common::mcp_session_env(
        dir.path(),
        &requests,
        &[("ARCFORM_HISTORY_DIR", history.as_path())],
    );
    let responses: Vec<Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let by_id = |id: u64| responses.iter().find(|v| v["id"] == id).unwrap();
    let tool = by_id(2)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "sql_query")
        .expect("sql_query is listed")
        .clone();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(by_id(3)["result"]["structuredContent"], verb);
    let refused = &by_id(4)["result"];
    assert_eq!(refused["isError"], true);
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("refused before anything ran"),
        "{refused}"
    );
}
