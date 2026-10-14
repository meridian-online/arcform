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
    assert_eq!(got["sample"][0], json!([1, "a", 1.5]));
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

/// A view the columns and counts of which read cleanly but whose body fails once the
/// sample query (the only one that says LIMIT) runs it: describe must refuse with that
/// error, not report an empty sample as if the table held no rows.
#[test]
fn describe_refuses_when_the_sample_query_fails() {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE src (a INTEGER);
         INSERT INTO src VALUES (1), (2);
         CREATE VIEW sample_fails AS
           SELECT CASE WHEN current_query() ILIKE '%LIMIT%' THEN error('sample boom') ELSE a END AS a
           FROM src;",
    )
    .unwrap();
    drop(conn);
    let out = describe(dir.path(), "sample_fails");
    assert!(!out.status.success(), "describe succeeded: {out:?}");
    assert!(stderr(&out).contains("sample boom"), "{}", stderr(&out));
}

/// A view whose columns read cleanly but whose body fails once the counting query (the
/// only one that says `count(*) - count(`) runs it: describe must refuse with that
/// error, not report counts it never got.
#[test]
fn describe_refuses_when_the_count_query_fails() {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE src (a INTEGER);
         INSERT INTO src VALUES (1), (2);
         CREATE VIEW count_fails AS
           SELECT CASE WHEN current_query() ILIKE '%count(*) - count(%' THEN error('count boom') ELSE a END AS a
           FROM src;",
    )
    .unwrap();
    drop(conn);
    let out = describe(dir.path(), "count_fails");
    assert!(!out.status.success(), "describe succeeded: {out:?}");
    assert!(stderr(&out).contains("count boom"), "{}", stderr(&out));
}

/// With no `db:`, the database's place comes from the Protocol's name; a name that is
/// not a plain file name is refused with that reason, not read as a missing database.
#[test]
fn describe_refuses_a_name_that_is_not_a_file_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: a/b\nengine: duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    let out = describe(dir.path(), "orders");
    assert!(!out.status.success(), "describe succeeded: {out:?}");
    let err = stderr(&out);
    assert!(!err.contains("no database for this Protocol"), "{err}");
    assert!(err.contains("a/b"), "{err}");
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
    assert_eq!(got["sample"][0], json!([1, "x"]));
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

#[test]
fn describe_a_file_that_is_not_a_database_is_not_called_locked() {
    let dir = protocol();
    // The path holds "lock" and "block", which are not what DuckDB says of a lock.
    let hidden = dir.path().join("blockchain-lock");
    std::fs::create_dir(&hidden).unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: p\nengine: duckdb\ndb: blockchain-lock/data.duckdb\nsteps: []\nassets: {}\n",
    )
    .unwrap();
    let junk: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(hidden.join("data.duckdb"), junk).unwrap();
    let out = describe(dir.path(), "orders");
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(!err.contains("in use by another process"), "{err}");
    assert!(!err.contains("invalid manifest"), "{err}");
}

#[test]
fn describe_names_a_table_in_another_schema() {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE SCHEMA staging; CREATE SCHEMA other;
         CREATE TABLE staging.orders (id INTEGER); INSERT INTO staging.orders VALUES (7);
         CREATE TABLE staging.only_here (id INTEGER);
         CREATE TABLE other.dup (id INTEGER); CREATE TABLE staging.dup (id INTEGER);
         CREATE TABLE main.dup (id INTEGER, extra INTEGER);",
    )
    .unwrap();
    drop(conn);
    for name in ["staging.orders", "orders"] {
        let got = describe_json(dir.path(), name);
        assert_eq!(got["sample"], json!([[7]]), "{name}");
    }
    // A bare name in main wins over the same name elsewhere.
    let dup = describe_json(dir.path(), "dup");
    assert_eq!(dup["columns"].as_array().unwrap().len(), 2);
    let err = stderr(&describe(dir.path(), "nope"));
    assert!(err.contains("staging.orders"), "{err}");
    assert!(err.contains("staging.only_here"), "{err}");
}

#[test]
fn describe_lists_main_tables_by_bare_name() {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE main.orders (id INTEGER);
         CREATE SCHEMA staging; CREATE TABLE staging.only_here (id INTEGER);",
    )
    .unwrap();
    drop(conn);
    let err = stderr(&describe(dir.path(), "nope"));
    assert!(
        err.contains("the tables are: orders, staging.only_here"),
        "{err}"
    );
    assert!(!err.contains("main."), "{err}");
}

#[test]
fn describe_a_bare_name_in_two_schemas_is_refused() {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE SCHEMA a; CREATE SCHEMA b; CREATE TABLE a.t (x INT); CREATE TABLE b.t (x INT);",
    )
    .unwrap();
    drop(conn);
    let out = describe(dir.path(), "t");
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("a.t") && err.contains("b.t"), "{err}");
}

#[test]
fn describe_a_column_named_like_the_row_alias_gives_real_rows() {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch("CREATE TABLE odd (t INTEGER, a VARCHAR); INSERT INTO odd VALUES (5, 'z');")
        .unwrap();
    drop(conn);
    assert_eq!(
        describe_json(dir.path(), "odd")["sample"],
        json!([[5, "z"]])
    );
}

#[test]
fn describe_records_no_history() {
    let dir = protocol_with_data();
    describe_json(dir.path(), "orders");
    let history = dir.path().join(".history");
    let entries = std::fs::read_dir(&history).map(|d| d.count()).unwrap_or(0);
    assert_eq!(entries, 0, "describe wrote into the history folder");
}

#[test]
fn describe_a_protocol_with_no_db_key_reads_the_default_location() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: nodb\nengine: duckdb\nsteps:\n  - name: make\n    sql: make.sql\nassets: {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("make.sql"),
        "CREATE TABLE t AS SELECT 1 AS n;\n",
    )
    .unwrap();
    let data = dir.path().join(".db");
    let run = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir.path())
        .env("ARCFORM_HISTORY_DIR", dir.path().join(".history"))
        .env("ARCFORM_DB_DIR", &data)
        .arg("run")
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    let got = describe_json(dir.path(), "t");
    assert_eq!(got["row_count"], 1);
    let database = got["database"].as_str().unwrap();
    let mut made = Vec::new();
    let mut pending = vec![data.clone()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "duckdb") {
                made.push(path.canonicalize().unwrap());
            }
        }
    }
    assert_eq!(made.len(), 1, "arc run made one database: {made:?}");
    assert_eq!(Path::new(database).canonicalize().unwrap(), made[0]);
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

/// A Protocol whose database holds `odd_numbers`: a DOUBLE column with NaN and the
/// infinities, which DuckDB's `to_json` writes as bare words JSON has no place for,
/// and a HUGEINT and a DECIMAL(20,2) wider than a double holds exactly.
fn protocol_with_odd_numbers() -> tempfile::TempDir {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE odd_numbers (d DOUBLE, h HUGEINT, m DECIMAL(20,2), s DECIMAL(9,2));
         INSERT INTO odd_numbers VALUES
           ('NaN', 170141183460469231731687303715884105727, 12345678901234567.89, 1.5),
           ('Infinity', 1, 0.10, 2.25),
           ('-Infinity', -2, -1.00, NULL);",
    )
    .unwrap();
    drop(conn);
    dir
}

#[test]
fn describe_writes_non_finite_doubles_as_strings() {
    let dir = protocol_with_odd_numbers();
    let sample = &describe_json(dir.path(), "odd_numbers")["sample"];
    assert_eq!(sample[0][0], "NaN");
    assert_eq!(sample[1][0], "Infinity");
    assert_eq!(sample[2][0], "-Infinity");
}

#[test]
fn describe_keeps_wide_numbers_exact_as_strings() {
    let dir = protocol_with_odd_numbers();
    let sample = &describe_json(dir.path(), "odd_numbers")["sample"];
    assert_eq!(sample[0][1], "170141183460469231731687303715884105727");
    // A HUGEINT within ±2^53 is a number: a double holds it exactly.
    assert_eq!(sample[1][1], json!(1));
    assert_eq!(sample[2][1], json!(-2));
    assert_eq!(sample[0][2], "12345678901234567.89");
    assert_eq!(sample[1][2], "0.10");
    // A decimal a double holds exactly stays a number.
    assert_eq!(sample[0][3], json!(1.5));
    assert_eq!(sample[2][3], Value::Null);
}

#[test]
fn describe_writes_nested_values_as_duckdb_writes_them() {
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE TABLE nested (ds DOUBLE[], ms DECIMAL(38,2)[]);
         INSERT INTO nested VALUES ([1.5::DOUBLE, 'NaN'::DOUBLE], [1.50, 2.25]);",
    )
    .unwrap();
    drop(conn);
    // A list holding NaN is not JSON, so the whole cell is DuckDB's text; a list of
    // wide decimals is not cast, so its values are JSON numbers.
    assert_eq!(
        describe_json(dir.path(), "nested")["sample"],
        json!([["[1.5,NaN]", [1.5, 2.25]]])
    );
}

#[test]
fn describe_opens_with_extension_install_off() {
    // A view reading the setting on the connection describe opens.
    let dir = protocol();
    let conn = duckdb::Connection::open(dir.path().join("data.duckdb")).unwrap();
    conn.execute_batch(
        "CREATE VIEW ext AS SELECT current_setting('autoinstall_known_extensions') AS v;",
    )
    .unwrap();
    drop(conn);
    assert_eq!(describe_json(dir.path(), "ext")["sample"], json!([[false]]));
}

#[cfg(feature = "mcp")]
#[test]
fn table_describe_tool_without_table_names_the_missing_argument() {
    let dir = protocol_with_data();
    let history = dir.path().join(".history");
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "table_describe", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "table_describe", "arguments": {}}}),
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
    let result = &responses.iter().find(|v| v["id"] == 2).unwrap()["result"];
    assert_eq!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("`table` is required"), "{result}");
}
