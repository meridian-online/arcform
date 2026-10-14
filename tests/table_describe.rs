//! `arc table describe` answers what a table in a Protocol's database holds, read from
//! DuckDB itself and without changing the file:
//!
//!   1. **fidelity** — columns, types, row count and null counts equal DuckDB's own;
//!   2. **bounds** — the sample is at most five rows;
//!   3. **refusals** — a missing table, a missing database and a locked database are
//!      named in the error;
//!   4. **read-only** — the database's bytes are the same afterwards;
//!   5. **tool** — `table_describe` returns the verb's JSON and is annotated read-only.

#[allow(dead_code)]
mod common;

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A Protocol directory with `db: data.duckdb`, and no database yet.
fn protocol() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: p\nengine: duckdb\ndb: data.duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    dir
}

/// A Protocol whose database holds `orders`, `Order Lines` and `many`.
fn protocol_with_data() -> tempfile::TempDir {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE orders (id INTEGER, label VARCHAR, price DOUBLE);
         INSERT INTO orders VALUES (1, 'a', 1.5), (2, NULL, 2.5), (3, 'c', NULL);
         CREATE TABLE \"Order Lines\" (\"order\" INTEGER, \"a\"\"b\" VARCHAR);
         INSERT INTO \"Order Lines\" VALUES (1, 'x'), (2, NULL);
         CREATE TABLE many AS SELECT range AS n FROM range(12);",
    )
    .unwrap();
    drop(conn);
    dir
}

fn describe(dir: &Path, table: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_HISTORY_DIR", dir.join(".history"))
        .env("ARCFORM_DB_DIR", dir.join(".db"))
        .args(["table", "describe", table])
        .output()
        .expect("spawn arc")
}

fn describe_json(dir: &Path, table: &str) -> Value {
    let out = describe(dir, table);
    assert!(out.status.success(), "describe failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn sha(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}

#[test]
fn describe_matches_duckdb() {
    let dir = protocol_with_data();
    let got = describe_json(dir.path(), "orders");
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();

    let count: i64 = conn
        .query_row("SELECT count(*) FROM orders", [], |r| r.get(0))
        .unwrap();
    assert_eq!(got["row_count"], count);
    assert_eq!(got["table"], "orders");
    assert!(got["database"].as_str().unwrap().ends_with("data.duckdb"));

    let mut stmt = conn
        .prepare("SELECT column_name, column_type FROM (DESCRIBE orders)")
        .unwrap();
    let expected: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let columns = got["columns"].as_array().unwrap();
    assert_eq!(columns.len(), expected.len());
    for (column, (name, ty)) in columns.iter().zip(&expected) {
        assert_eq!(column["name"], name.as_str());
        assert_eq!(column["type"], ty.as_str());
        let nulls: i64 = conn
            .query_row(
                &format!("SELECT count(*) - count(\"{name}\") FROM orders"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(column["null_count"], nulls, "nulls in {name}");
    }
    assert_eq!(
        got["sample"][0],
        json!({"id": 1, "label": "a", "price": 1.5})
    );
}

#[test]
fn describe_sample_is_at_most_five_rows() {
    let dir = protocol_with_data();
    let got = describe_json(dir.path(), "many");
    assert_eq!(got["row_count"], 12);
    assert_eq!(got["sample"].as_array().unwrap().len(), 5);
}

#[test]
fn describe_missing_table_is_refused() {
    let dir = protocol_with_data();
    let out = describe(dir.path(), "nope");
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("no table or view named nope"), "{err}");
    assert!(err.contains("orders"), "{err}");
}

#[test]
fn describe_missing_database_is_refused() {
    let dir = protocol();
    let out = describe(dir.path(), "orders");
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("run the Protocol (arc run)"), "{err}");
    assert!(!dir.path().join("data.duckdb").exists(), "nothing was made");
}

#[test]
fn describe_leaves_database_bytes_unchanged() {
    let dir = protocol_with_data();
    let db = dir.path().join("data.duckdb");
    let before = sha(&db);
    describe_json(dir.path(), "orders");
    assert_eq!(before, sha(&db));
}

#[test]
fn describe_quotes_identifiers() {
    let dir = protocol_with_data();
    let got = describe_json(dir.path(), "Order Lines");
    assert_eq!(got["row_count"], 2);
    assert_eq!(got["columns"][0]["name"], "order");
    assert_eq!(got["columns"][1]["name"], "a\"b");
    assert_eq!(got["columns"][1]["null_count"], 1);
    assert_eq!(got["sample"][0], json!({"order": 1, "a\"b": "x"}));
}

#[test]
fn describe_while_locked_is_refused_with_message() {
    let dir = protocol_with_data();
    // The writer is another process, since DuckDB's lock is per process.
    let writer = hold_writer(&dir.path().join("data.duckdb"));
    let out = describe(dir.path(), "orders");
    drop(writer);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        stderr(&out).contains("in use by another process"),
        "{}",
        stderr(&out)
    );
}

/// A child process that holds a read-write connection until it is dropped.
struct Writer(std::process::Child);
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Not a test: when `ARC_TEST_HOLD_DB` names a file, this opens it read-write, says so
/// and waits, so the test above has a second process holding the lock. DuckDB's lock
/// is per process, so a connection in the test's own process would prove nothing.
#[test]
fn lock_holder_helper() {
    use std::io::Write;
    let Ok(db) = std::env::var("ARC_TEST_HOLD_DB") else {
        return;
    };
    let _conn = duckdb::Connection::open(db).unwrap();
    println!("ready");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(60));
}

fn hold_writer(db: &Path) -> Writer {
    use std::io::{BufRead, BufReader};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_holder_helper", "--nocapture"])
        .env("ARC_TEST_HOLD_DB", db)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the lock holder");
    let mut lines = BufReader::new(child.stdout.as_mut().unwrap()).lines();
    let ready = lines.any(|l| l.unwrap().trim() == "ready");
    assert!(ready, "the writer must open the database");
    Writer(child)
}

#[cfg(feature = "mcp")]
#[test]
fn table_describe_tool_matches_verb_and_is_read_only() {
    let dir = protocol_with_data();
    let verb = describe_json(dir.path(), "orders");
    let history = dir.path().join(".history");
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "table_describe", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "table_describe", "arguments": {"table": "orders"}}}),
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
        .find(|t| t["name"] == "table_describe")
        .expect("table_describe is listed")
        .clone();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(by_id(3)["result"]["structuredContent"], verb);
}
