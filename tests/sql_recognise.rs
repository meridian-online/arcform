//! `arc sql recognise` and the `arc mcp` tool `sql_recognise`: what arc reads a
//! SQL statement as, asked of the real `arc` binary.
//!
//! A filter typed as SQL and the same filter recorded by name are one operation,
//! and these tests hold that arc can say so, from DuckDB's own parse:
//!
//!   1. **a filter** — `SELECT *` from one table with a `WHERE` and no other
//!      clause is `filter-rows` on that table, its condition as typed, and
//!      asking writes no file;
//!   2. **no operation** — each statement DuckDB parses that is not that shape is
//!      answered with `operation` `null`, the clauses a filter cannot carry among
//!      them;
//!   3. **refused** — text that is not one statement DuckDB parses is refused,
//!      with nothing on stdout;
//!   4. **no executable** — the reading needs no `duckdb` on `PATH`;
//!   5. **help** — `arc --help` and `arc sql recognise --help` say what it is;
//!   6. **mcp** — `sql_recognise` answers what the command prints.
//!
//! Every run happens in an empty directory: the verb reads no protocol.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

/// Run the real `arc` binary with `args` in `dir`.
fn arc_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn arc")
}

/// `arc sql recognise <sql>` in an empty directory: the answer it printed, which
/// must be one JSON object, after a run that must exit 0.
fn recognise(sql: &str) -> Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = arc_in(dir.path(), &["sql", "recognise", sql]);
    assert!(
        out.status.success(),
        "arc sql recognise {sql:?} failed (code {:?}):\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    let answer: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("arc sql recognise {sql:?} printed something other than one JSON document ({e}):\n{stdout}")
    });
    assert!(
        answer.is_object(),
        "arc sql recognise {sql:?} printed {answer}, not an object"
    );
    answer
}

/// The answer for a statement read as `filter-rows` on `on` with `condition`.
fn filter_rows(on: &str, condition: &str) -> Value {
    json!({ "operation": "filter-rows", "on": on, "arguments": { "where": condition } })
}

/// The answer for a statement DuckDB parses that arc reads as no operation.
fn none() -> Value {
    json!({ "operation": null })
}

#[test]
fn a_filter_of_a_table_is_read_as_filter_rows_and_no_file_is_written() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sql = "SELECT * FROM orders WHERE amount > 100";
    let out = arc_in(dir.path(), &["sql", "recognise", sql]);
    assert!(
        out.status.success(),
        "arc sql recognise {sql:?} failed (code {:?}):\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let answer: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    assert_eq!(
        answer,
        filter_rows("orders", "amount > 100"),
        "what {sql:?} is read as"
    );
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read the directory")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert!(
        left.is_empty(),
        "arc sql recognise wrote into the directory it ran in: {left:?}"
    );
}

#[test]
fn the_condition_is_the_text_as_typed() {
    for (sql, on, condition) in [
        // Lower case, parentheses, a string holding a `;`, and the `;` that ends
        // the statement left out of the condition.
        (
            "select * from orders where (amount > 100 OR id = 1) AND note <> 'a;b';",
            "orders",
            "(amount > 100 OR id = 1) AND note <> 'a;b'",
        ),
        // A statement over several lines, the `*` and the condition each on their own.
        (
            "SELECT\n    *\nFROM orders\nWHERE amount > 100\n  AND id <> 7",
            "orders",
            "amount > 100\n  AND id <> 7",
        ),
        // A comment before the statement and after it, and a closing `;` with spaces.
        (
            "-- the big ones\nSELECT * FROM orders WHERE amount > 100 ;  -- done",
            "orders",
            "amount > 100",
        ),
        // A `where`, a `;` and a `--` inside a string, and a table whose name holds `where`.
        (
            "SELECT * FROM somewhere WHERE note = ' WHERE x; -- y'",
            "somewhere",
            "note = ' WHERE x; -- y'",
        ),
        // A subquery with a `WHERE` of its own, inside the condition.
        (
            "SELECT * FROM orders WHERE id IN (SELECT id FROM refunds WHERE amount < 0)",
            "orders",
            "id IN (SELECT id FROM refunds WHERE amount < 0)",
        ),
        // A quoted column, and a table named in double quotes, as DuckDB names it.
        (
            "SELECT * FROM \"Orders\" WHERE \"amount\" > 100",
            "Orders",
            "\"amount\" > 100",
        ),
        // The statement DuckDB reads `FROM … WHERE …` as.
        ("FROM orders WHERE amount > 100", "orders", "amount > 100"),
    ] {
        assert_eq!(
            recognise(sql),
            filter_rows(on, condition),
            "what {sql:?} is read as"
        );
    }
}

#[test]
fn a_statement_that_is_not_a_filter_of_a_table_is_read_as_no_operation() {
    for sql in [
        // A column list, an order, a limit, `DISTINCT` and `EXCLUDE`.
        "SELECT id, amount FROM orders WHERE amount > 100",
        "SELECT * FROM orders WHERE amount > 100 ORDER BY amount DESC",
        "SELECT * FROM orders WHERE amount > 100 LIMIT 5",
        "SELECT DISTINCT * FROM orders WHERE amount > 100",
        "SELECT * EXCLUDE (note) FROM orders WHERE amount > 100",
        // No `WHERE`: the whole table, and a sort alone.
        "SELECT * FROM orders",
        "SELECT * FROM orders ORDER BY amount DESC",
        // An alias, a schema, a file and a `WITH`.
        "SELECT * FROM orders o WHERE o.amount > 100",
        "SELECT * FROM main.orders WHERE amount > 100",
        "SELECT * FROM 'orders.csv' WHERE amount > 100",
        "WITH o AS (SELECT * FROM orders) SELECT * FROM o WHERE amount > 100",
        // A `QUALIFY`, and a statement that is not a `SELECT`.
        "SELECT * FROM orders WHERE amount > 100 QUALIFY row_number() OVER () = 1",
        "CREATE TABLE x AS SELECT 1",
        // A `WINDOW` clause, which DuckDB's tree does not show.
        "SELECT * FROM orders WHERE amount > 100 WINDOW w AS (ORDER BY id)",
        // A table whose recorded step would not parse: `FROM order`.
        "SELECT * FROM \"order\" WHERE amount > 100",
    ] {
        assert_eq!(recognise(sql), none(), "what {sql:?} is read as");
    }
}

#[test]
fn text_that_is_not_one_statement_is_refused() {
    for (sql, reason) in [
        (
            "SELECT * FROM orders WHERE amount >",
            "cannot parse the text as a SQL statement",
        ),
        ("", "cannot parse the text as a SQL statement"),
        (
            "-- a comment alone",
            "cannot parse the text as a SQL statement",
        ),
        ("SELECT 1; SELECT 2", "holds more than one statement"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = arc_in(dir.path(), &["sql", "recognise", sql]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "arc sql recognise {sql:?} exited 0; it is refused because the text {reason}"
        );
        assert!(
            out.stdout.is_empty(),
            "arc sql recognise {sql:?} was refused and still printed:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains(reason),
            "arc sql recognise {sql:?}: stderr does not say the text {reason}:\n{stderr}"
        );
    }
}

#[test]
fn no_duckdb_executable_is_needed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("an empty PATH directory");
    let sql = "SELECT * FROM orders WHERE amount > 100";
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir.path())
        .env("PATH", bin.path())
        .env_remove("ARC_DUCKDB_BIN")
        .args(["sql", "recognise", sql])
        .output()
        .expect("spawn arc");
    assert!(
        out.status.success(),
        "with no `duckdb` on PATH, arc sql recognise {sql:?} failed (code {:?}):\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let answer: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    assert_eq!(
        answer,
        recognise(sql),
        "with no `duckdb` on PATH, what {sql:?} is read as"
    );
}

#[test]
fn help_says_what_the_verb_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let top = String::from_utf8(arc_in(dir.path(), &["--help"]).stdout).expect("UTF-8");
    let listed = top
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with("sql "))
        .unwrap_or_else(|| panic!("`arc --help` does not list `sql`:\n{top}"));
    assert!(
        listed["sql ".len()..].trim().len() > 1,
        "`arc --help` lists `sql` with no line saying what it is: {listed:?}"
    );

    let out = arc_in(dir.path(), &["sql", "recognise", "--help"]);
    assert!(out.status.success(), "arc sql recognise --help failed");
    let help = String::from_utf8(out.stdout).expect("UTF-8");
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    for says in [
        "Print what arc reads a SQL statement as",
        "writes no file",
    ] {
        assert!(
            flat.contains(says),
            "`arc sql recognise --help` does not say {says:?}:\n{help}"
        );
    }
}

/// `sql_recognise` asked of the real `arc mcp` server over stdio, its answers
/// compared with what `arc sql recognise` prints.
#[cfg(feature = "mcp")]
mod mcp {
    use std::io::Write;
    use std::process::{Command, Stdio};

    use serde_json::{Value, json};

    /// Start `arc mcp` in an empty directory, send each request as one line,
    /// close stdin, and return the responses in the order the server wrote them.
    fn mcp_session(requests: &[Value]) -> Vec<Value> {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(dir.path())
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn arc mcp");
        {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            for request in requests {
                writeln!(stdin, "{request}").expect("write a request");
            }
        }
        let out = child.wait_with_output().expect("wait for arc mcp");
        assert!(
            out.status.success(),
            "arc mcp failed (code {:?}):\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("stdout is UTF-8")
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("response is not JSON ({e}): {line}"))
            })
            .collect()
    }

    /// The result of request `id` in `responses`.
    fn result_of(responses: &[Value], id: u64) -> &Value {
        &responses
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no response to request {id} in {responses:?}"))["result"]
    }

    /// A `tools/call` request for `sql_recognise` with `arguments`.
    fn call(id: u64, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": "sql_recognise", "arguments": arguments },
        })
    }

    #[test]
    fn the_tool_is_listed_with_a_schema_requiring_one_string() {
        let responses = mcp_session(&[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })]);
        let tools = result_of(&responses, 1)["tools"]
            .as_array()
            .expect("tools/list returns an array");
        let tool = tools
            .iter()
            .find(|t| t["name"] == "sql_recognise")
            .unwrap_or_else(|| panic!("tools/list holds no `sql_recognise`: {tools:?}"));
        assert!(
            tool["description"]
                .as_str()
                .is_some_and(|d| !d.trim().is_empty()),
            "`sql_recognise` has no description: {tool}"
        );
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "the input schema: {schema}");
        assert_eq!(
            schema["required"],
            json!(["sql"]),
            "the input schema requires `sql` alone: {schema}"
        );
        let properties = schema["properties"].as_object().expect("properties");
        assert_eq!(
            properties.keys().collect::<Vec<_>>(),
            ["sql"],
            "the input schema takes `sql` alone: {schema}"
        );
        assert_eq!(
            properties["sql"]["type"], "string",
            "`sql` is a string: {schema}"
        );
    }

    #[test]
    fn the_tool_answers_what_the_command_prints() {
        let statements = [
            "SELECT * FROM orders WHERE amount > 100",
            "SELECT id, amount FROM orders WHERE amount > 100",
        ];
        let requests: Vec<Value> = statements
            .iter()
            .zip(1..)
            .map(|(sql, id)| call(id, json!({ "sql": sql })))
            .collect();
        let responses = mcp_session(&requests);
        for (sql, id) in statements.iter().zip(1..) {
            let result = result_of(&responses, id);
            assert_eq!(result["isError"], false, "sql_recognise {sql:?}: {result}");
            assert_eq!(
                result["structuredContent"],
                super::recognise(sql),
                "sql_recognise {sql:?} answers what `arc sql recognise` prints"
            );
        }
    }

    #[test]
    fn more_than_one_statement_and_no_statement_are_error_results() {
        let responses = mcp_session(&[
            call(1, json!({ "sql": "SELECT 1; SELECT 2" })),
            call(2, json!({})),
        ]);
        for (id, says) in [
            (1, "holds more than one statement"),
            (2, "`sql` is required"),
        ] {
            let result = result_of(&responses, id);
            assert_eq!(result["isError"], true, "request {id}: {result}");
            let text = result["content"][0]["text"].as_str().unwrap_or_default();
            assert!(
                text.contains(says),
                "request {id}'s error does not say {says:?}: {text}"
            );
        }
    }

    #[test]
    fn mcp_help_and_the_instructions_name_the_tool() {
        let dir = tempfile::tempdir().expect("tempdir");
        let help = super::arc_in(dir.path(), &["mcp", "--help"]);
        let help = String::from_utf8(help.stdout).expect("UTF-8");
        assert!(
            help.contains("sql_recognise"),
            "`arc mcp --help` does not name `sql_recognise`:\n{help}"
        );
        let responses = mcp_session(&[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" },
        })]);
        let instructions = result_of(&responses, 1)["instructions"]
            .as_str()
            .expect("initialize returns instructions");
        assert!(
            instructions.contains("sql_recognise"),
            "the instructions `initialize` returns do not name `sql_recognise`:\n{instructions}"
        );
    }
}
