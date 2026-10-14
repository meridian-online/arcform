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

const KEEP: &str =
    "to keep a query as a step: `arc sql record '<statement>' --name <name>` (MCP: sql_record)";

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
           ('Infinity', 1, 0.10);
         CREATE TABLE nested (ds DOUBLE[], ms DECIMAL(38,2)[]);
         INSERT INTO nested VALUES ([1.5::DOUBLE, 'NaN'::DOUBLE], [1.50, 2.25]);",
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
                "only read-only SELECT statements can be queried; refused before anything ran: \
                 a statement is not a SELECT"
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
    // sequence, a write the read-only connection refuses. The failure is the
    // statement's own result, and the command exits 1.
    for (sql, why) in [
        (
            "SELECT * FROM query('CREATE TABLE x(i INT)')",
            "Expected a single SELECT statement",
        ),
        ("SELECT nextval('seq')", "read-only"),
    ] {
        let out = query(dir.path(), &[sql]);
        assert_eq!(out.status.code(), Some(1), "{sql}: {out:?}");
        let got: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
        let result = &got["results"][0];
        assert_eq!(result["statement"], 1);
        let error = result["error"].as_str().expect("an error result");
        assert!(error.contains(why), "{sql}: {error}");
        assert!(
            !stderr(&out).contains("refused before anything ran"),
            "{sql}"
        );
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
fn a_failing_statement_keeps_the_others_results() {
    let dir = protocol_with_data();
    let out = query(dir.path(), &["SELECT 1 AS a; SELECT nextval('seq')"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let got: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    let results = got["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["rows"], json!([[1]]));
    assert!(results[0].get("error").is_none());
    assert_eq!(results[1]["statement"], 2);
    assert!(
        results[1]["error"].as_str().unwrap().contains("read-only"),
        "{}",
        results[1]
    );
    assert!(results[1].get("rows").is_none());
    assert_eq!(got["keep"], KEEP);
}

/// A view that fails only under the query that counts the rows held back (the one
/// reading `count(*) FROM (`): the statement reports that error, not a count of nothing.
#[test]
fn a_failing_count_of_the_rows_held_back_is_reported() {
    let dir = protocol_with_data();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE VIEW count_fails AS
           SELECT n FROM many
           WHERE CASE WHEN current_query() ILIKE '%count(*) FROM (%' THEN error('count boom') ELSE true END;",
    )
    .unwrap();
    drop(conn);
    let out = query(dir.path(), &["--limit", "10", "SELECT n FROM count_fails"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let got: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    let result = &got["results"][0];
    assert!(
        result["error"].as_str().unwrap().contains("count boom"),
        "{result}"
    );
    assert!(result.get("rows_held_back").is_none(), "{result}");
}

#[test]
fn a_statement_after_a_failed_one_still_runs() {
    let dir = protocol_with_data();
    let out = query(
        dir.path(),
        &["SELECT 1 AS a; SELECT nextval('seq'); SELECT 2 AS b"],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let got: Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    let results = got["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["rows"], json!([[1]]));
    assert!(results[1]["error"].as_str().unwrap().contains("read-only"));
    assert_eq!(results[2]["statement"], 3);
    assert_eq!(results[2]["rows"], json!([[2]]));
    assert!(results[2].get("error").is_none());
}

#[test]
fn extensions_are_not_installed_by_a_query() {
    let dir = protocol_with_data();
    let got = query_json(
        dir.path(),
        &["SELECT current_setting('autoinstall_known_extensions') AS v"],
    );
    assert_eq!(got["results"][0]["rows"], json!([[false]]));
}

#[test]
fn a_limit_over_the_bound_is_refused() {
    let dir = protocol_with_data();
    let out = query(dir.path(), &["SELECT 1", "--limit", "100001"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        stderr(&out).contains("the limit is at most 100000 rows"),
        "{}",
        stderr(&out)
    );
    query_json(dir.path(), &["SELECT 1", "--limit", "100000"]);
}

#[test]
fn nested_values_are_written_as_duckdb_writes_them() {
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT ds, ms FROM nested"]);
    // A list holding NaN is not JSON, so the whole cell is DuckDB's text; a list of
    // wide decimals is not cast, so its values are JSON numbers.
    assert_eq!(
        got["results"][0]["rows"],
        json!([["[1.5,NaN]", [1.5, 2.25]]])
    );
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
            ["Infinity", 1, "0.10"],
        ])
    );
}

#[test]
fn query_gives_a_huge_integer_within_two_to_the_53_as_a_number() {
    // sum() of an INTEGER column is a HUGEINT: a small total is a number, not "6".
    let dir = protocol_with_data();
    let got = query_json(dir.path(), &["SELECT sum(i) FROM t"]);
    assert_eq!(got["results"][0]["rows"], json!([[6]]));

    // ±2^53 is the edge: inside it a double holds every integer, so a number; one
    // past it, or the widest values, keep their exact digits as a string.
    let got = query_json(
        dir.path(),
        &[
            "SELECT 9007199254740992::HUGEINT, 9007199254740993::HUGEINT, \
           -9007199254740992::HUGEINT, -9007199254740993::HUGEINT, \
           '-170141183460469231731687303715884105728'::HUGEINT, \
           5::UHUGEINT, 340282366920938463463374607431768211455::UHUGEINT, NULL::HUGEINT",
        ],
    );
    assert_eq!(
        got["results"][0]["rows"],
        json!([[
            9007199254740992_i64,
            "9007199254740993",
            -9007199254740992_i64,
            "-9007199254740993",
            "-170141183460469231731687303715884105728",
            5,
            "340282366920938463463374607431768211455",
            null
        ]])
    );
}

#[test]
fn a_text_that_does_not_parse_is_refused() {
    let dir = protocol_with_data();
    let out = query(dir.path(), &["selec 1"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("refused before anything ran: syntax error at or near"),
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
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
            "name": "sql_query", "arguments": {"sql": "SELECT 1 AS a; SELECT nextval('seq')"}}}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
            "name": "sql_query", "arguments": {"sql": "SELECT 1", "limit": 100001}}}),
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
    assert_eq!(tool["annotations"]["openWorldHint"], true);
    assert_eq!(
        tool["inputSchema"]["properties"]["limit"]["maximum"],
        100000
    );
    assert_eq!(by_id(3)["result"]["structuredContent"], verb);
    // A statement that fails while running is in the result, not a tool error.
    let partial = &by_id(5)["result"];
    assert_eq!(partial["isError"], false, "{partial}");
    let results = &partial["structuredContent"]["results"];
    assert_eq!(results[0]["rows"], json!([[1]]));
    assert!(results[1]["error"].as_str().unwrap().contains("read-only"));
    let over = &by_id(6)["result"];
    assert_eq!(over["isError"], true);
    assert!(
        over["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("the limit is at most 100000 rows"),
        "{over}"
    );
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
