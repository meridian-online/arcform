//! `arc operation record` and the `arc mcp` tool `operation_record`: an operation
//! recorded as a new step of a Protocol from its long name, the table it is
//! applied to, the step's name and its arguments, asked of the real `arc` binary.
//!
//!   1. **record** — `filter-rows` on `orders` writes one generated model whose
//!      first line names the operation and the table and nothing else, and adds
//!      one step to the end of `arcform.yaml`, every other byte kept; nothing else
//!      in the Protocol's directory changes, so nothing ran;
//!   2. **run** — `arc run` then runs the step, and its table holds the rows the
//!      condition keeps and no other;
//!   3. **mcp** — the same request sent to `arc mcp` writes the same bytes,
//!      and the two versions it records name `mcp` where the terminal's name
//!      `terminal`; the way is in the history alone, and no file of the
//!      Protocol names it;
//!   4. **refuse** — no `where`, an argument the operation does not take, an
//!      operation arc does not hold, a condition holding a `;` outside a string or
//!      a comment, and a table no step makes are each refused with the directory
//!      untouched and a message naming the fault; a `;` inside a string or a
//!      comment, and any other condition, is recorded as written.
//!
//! Every recording is a version of the Protocol, so each run points
//! `ARCFORM_HISTORY_DIR` at a directory of its own, outside the Protocol.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A Protocol whose one step makes the table `orders`. The comment and the blank
/// line are there to be kept.
const MANIFEST: &str = "\
# A shop's orders, and what is made from them.
name: shop
engine: duckdb
db: shop.duckdb

steps:
  - name: load_orders
    sql: models/01_orders.sql
";

/// The step that makes `orders`: one row at 100, one either side of it, and a
/// `note` holding a `;` inside a string.
const ORDERS_SQL: &str = "\
CREATE OR REPLACE TABLE orders AS
SELECT * FROM (VALUES (1, 50, 'a;b'), (2, 150, 'abc'), (3, 300, 'xyz'), (4, 100, 'a'))
    AS t(id, amount, note);
";

/// What recording `filter-rows` on `orders` as `big_orders` appends to `arcform.yaml`.
const APPENDED_STEP: &str = "  - name: big_orders\n    sql: models/02_big_orders.sql\n";

/// The model that recording writes, byte for byte.
const BIG_ORDERS_MODEL: &str = "\
-- generated: filter-rows on orders
CREATE OR REPLACE TABLE \"big_orders\" AS
SELECT *
FROM orders
WHERE amount > 100;
";

/// What recording `sort-rows` on `orders` as `by_amount` appends to `arcform.yaml`.
const APPENDED_SORT_STEP: &str = "  - name: by_amount\n    sql: models/02_by_amount.sql\n";

/// The model recording `sort-rows` on `orders` as `by_amount` writes, byte for byte.
const BY_AMOUNT_MODEL: &str = "\
-- generated: sort-rows on orders
CREATE OR REPLACE TABLE \"by_amount\" AS
SELECT *
FROM orders
ORDER BY amount desc;
";

/// A Protocol as it stands before anything is recorded, and a history store of
/// its own beside it, outside the Protocol's directory.
struct Protocol {
    _root: tempfile::TempDir,
    dir: PathBuf,
    history: PathBuf,
}

impl Protocol {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("shop");
        let history = root.path().join("history");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("arcform.yaml"), MANIFEST).unwrap();
        std::fs::write(dir.join("models/01_orders.sql"), ORDERS_SQL).unwrap();
        Protocol {
            _root: root,
            dir,
            history,
        }
    }

    /// Run the real `arc` binary with `args` in the Protocol's directory.
    fn arc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(&self.dir)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .args(args)
            .output()
            .expect("spawn arc")
    }

    /// `arc operation record filter-rows --on orders --name big_orders` with
    /// `where` set to `condition`.
    fn record_filter(&self, condition: &str) -> Output {
        let arg = format!("where={condition}");
        self.arc(&[
            "operation",
            "record",
            "filter-rows",
            "--on",
            "orders",
            "--name",
            "big_orders",
            "--arg",
            &arg,
        ])
    }

    /// `arc operation record sort-rows --on orders --name by_amount` with
    /// `order_by` set to `order`.
    fn record_sort(&self, order: &str) -> Output {
        let arg = format!("order_by={order}");
        self.arc(&[
            "operation",
            "record",
            "sort-rows",
            "--on",
            "orders",
            "--name",
            "by_amount",
            "--arg",
            &arg,
        ])
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    /// The versions `arc history list` prints for this Protocol: each line's
    /// id, kind and the words after its size, which are the way.
    fn versions(&self) -> Vec<(String, String, String)> {
        let out = self.arc(&["history", "list"]);
        ok(&out, "arc history list");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| {
                let (head, way) = line.split_once(" bytes  ")?;
                let mut words = head.split_whitespace();
                Some((
                    words.next()?.to_string(),
                    words.next()?.to_string(),
                    way.to_string(),
                ))
            })
            .collect()
    }

    /// Every file under the Protocol's directory, by path relative to it, with
    /// its bytes.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, files);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    files.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&self.dir, &self.dir, &mut files);
        files
    }
}

fn ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `out` is a refusal: a non-zero exit, nothing on stdout, and a message on
/// stderr holding each of `named`.
fn refused(out: &Output, named: &[&str]) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected a refusal naming {named:?}; arc exited 0:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stdout.is_empty(),
        "a refusal prints nothing on stdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    for name in named {
        assert!(
            stderr.contains(name),
            "the refusal does not name {name}:\n{stderr}"
        );
    }
}

// ------------------------------------------------------------------ record

#[test]
fn record_writes_one_generated_model_and_appends_one_step() {
    let protocol = Protocol::new();
    let before = protocol.files();

    let out = protocol.record_filter("amount > 100");
    ok(&out, "arc operation record");

    assert_eq!(protocol.read("models/02_big_orders.sql"), BIG_ORDERS_MODEL);
    assert_eq!(
        protocol.read("models/02_big_orders.sql").lines().next(),
        Some("-- generated: filter-rows on orders"),
        "the first line names the operation and the table and nothing else"
    );
    assert_eq!(
        protocol.read("arcform.yaml"),
        format!("{MANIFEST}{APPENDED_STEP}"),
        "the step is appended and every other byte of arcform.yaml is kept"
    );

    // Recording runs nothing: the one file the directory gained is the model, and
    // every file it held is as it was, arcform.yaml apart.
    let mut after = protocol.files();
    let model = after
        .remove(Path::new("models/02_big_orders.sql"))
        .expect("the model was written");
    assert_eq!(model, BIG_ORDERS_MODEL.as_bytes());
    after.remove(Path::new("arcform.yaml"));
    let mut untouched = before;
    untouched.remove(Path::new("arcform.yaml"));
    assert_eq!(
        after.keys().collect::<Vec<_>>(),
        untouched.keys().collect::<Vec<_>>(),
        "recording added or removed a file other than the model"
    );
    assert_eq!(after, untouched, "recording changed a file it did not own");
}

// One recording in a Protocol whose history is empty lists two versions: the
// state it replaced and the state it wrote, each naming the terminal.
#[test]
fn each_recording_is_a_version_of_the_protocol() {
    let protocol = Protocol::new();
    ok(
        &protocol.record_filter("amount > 100"),
        "arc operation record",
    );

    let versions = protocol.versions();
    let seen: Vec<(&str, &str)> = versions
        .iter()
        .map(|(_, kind, way)| (kind.as_str(), way.as_str()))
        .collect();
    assert_eq!(
        seen,
        [("checkpoint", "terminal"), ("save", "terminal")],
        "the state the recording replaced and the state it wrote: {versions:?}"
    );
    assert_eq!(
        protocol.arc(&["history", "show", &versions[0].0]).stdout,
        MANIFEST.as_bytes(),
        "the checkpoint is the Protocol as it was"
    );
}

// --------------------------------------------------------------------- run

#[test]
fn arc_run_runs_the_recorded_step_and_keeps_the_rows_the_condition_holds_for() {
    let protocol = Protocol::new();
    ok(
        &protocol.record_filter("amount > 100"),
        "arc operation record",
    );
    ok(&protocol.arc(&["run"]), "arc run");

    let db = duckdb::Connection::open(protocol.dir.join("shop.duckdb")).expect("open shop.duckdb");
    let ids = |sql: &str| -> Vec<i32> {
        let mut statement = db.prepare(sql).unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(ids("SELECT id FROM orders ORDER BY id"), [1, 2, 3, 4]);
    assert_eq!(
        ids("SELECT id FROM big_orders ORDER BY id"),
        [2, 3],
        "big_orders holds the rows of orders whose amount is over 100, and no other"
    );
}

#[test]
fn record_sort_writes_the_ordered_model_and_appends_one_step() {
    let protocol = Protocol::new();
    let before = protocol.files();

    let out = protocol.record_sort("amount desc");
    ok(&out, "arc operation record sort-rows");

    assert_eq!(protocol.read("models/02_by_amount.sql"), BY_AMOUNT_MODEL);
    assert_eq!(
        protocol.read("models/02_by_amount.sql").lines().next(),
        Some("-- generated: sort-rows on orders"),
        "the first line names the operation and the table and nothing else"
    );
    assert_eq!(
        protocol.read("arcform.yaml"),
        format!("{MANIFEST}{APPENDED_SORT_STEP}"),
        "the step is appended and every other byte of arcform.yaml is kept"
    );

    // Recording runs nothing: the one file the directory gained is the model, and
    // every file it held is as it was, arcform.yaml apart.
    let mut after = protocol.files();
    let model = after
        .remove(Path::new("models/02_by_amount.sql"))
        .expect("the model was written");
    assert_eq!(model, BY_AMOUNT_MODEL.as_bytes());
    after.remove(Path::new("arcform.yaml"));
    let mut untouched = before;
    untouched.remove(Path::new("arcform.yaml"));
    assert_eq!(
        after.keys().collect::<Vec<_>>(),
        untouched.keys().collect::<Vec<_>>(),
        "recording added or removed a file other than the model"
    );
    assert_eq!(after, untouched, "recording changed a file it did not own");
}

#[test]
fn arc_run_puts_the_rows_in_the_order_the_sort_stored() {
    let protocol = Protocol::new();
    ok(
        &protocol.record_sort("amount desc"),
        "arc operation record sort-rows",
    );
    ok(&protocol.arc(&["run"]), "arc run");

    // `orders` holds amounts 50, 150, 300, 100 at ids 1, 2, 3, 4, so descending
    // amount is ids 3, 2, 4, 1. Read with no ORDER BY and `preserve_insertion_order`
    // at its default, so what comes back is the order the step stored.
    let db = duckdb::Connection::open(protocol.dir.join("shop.duckdb")).expect("open shop.duckdb");
    let ids: Vec<i32> = db
        .prepare("SELECT id FROM by_amount")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        ids,
        [3, 2, 4, 1],
        "by_amount holds every row of orders once, in descending order of amount"
    );
}

#[test]
fn a_multi_column_order_is_recorded_as_written() {
    let protocol = Protocol::new();
    ok(
        &protocol.record_sort("region, amount desc"),
        "arc operation record sort-rows",
    );
    let model = protocol.read("models/02_by_amount.sql");
    assert_eq!(
        model.lines().last(),
        Some("ORDER BY region, amount desc;"),
        "the order is recorded as written:\n{model}"
    );
}

/// Why the record-time check refuses a condition DuckDB cannot parse whole,
/// rather than admitting it as round two did: `arc run` hands a step's file to
/// the `duckdb` CLI's `-f`, which runs each statement it can read before it
/// reaches the one it cannot. So a step whose recorded file held a runnable,
/// `;`-terminated prefix and an unparseable tail — the shape of
/// `where = amount > 100; DROP TABLE orders; zzz` — would run the prefix,
/// dropping a table, and only then fail. This characterises `arc run`; it is not
/// a guard on an arc line, so `a_probe_duckdb_cannot_parse_at_all_is_refused`
/// and the record-time tests below hold the fix. Here a step arc cannot fully
/// parse is run opaque, its file handed to DuckDB whole.
#[test]
fn arc_run_runs_a_valid_prefix_before_an_unparseable_tail() {
    let protocol = Protocol::new();
    std::fs::write(
        protocol.dir.join("models/02_incremental.sql"),
        // Makes `victim`, drops it, records that the drop ran, then meets `zzz`.
        "CREATE OR REPLACE TABLE victim AS SELECT 1;\n\
         DROP TABLE victim;\n\
         CREATE OR REPLACE TABLE the_drop_ran AS SELECT 1;\n\
         zzz\n",
    )
    .unwrap();
    let manifest = protocol.read("arcform.yaml");
    std::fs::write(
        protocol.dir.join("arcform.yaml"),
        format!("{manifest}  - name: incremental\n    sql: models/02_incremental.sql\n"),
    )
    .unwrap();

    let out = protocol.arc(&["run"]);
    assert!(
        !out.status.success(),
        "the run should fail on the unparseable tail:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let db = duckdb::Connection::open(protocol.dir.join("shop.duckdb")).expect("open shop.duckdb");
    let tables: Vec<String> = db
        .prepare("SELECT table_name FROM information_schema.tables")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    // The statement after the DROP ran, so the DROP ran, and `victim` is gone:
    // the CLI executed the whole prefix before the tail errored — the hazard the
    // record-time check now blocks by refusing the condition that writes it.
    assert!(
        tables.iter().any(|t| t == "the_drop_ran"),
        "the statement after the DROP did not run; tables: {tables:?}"
    );
    assert!(
        !tables.iter().any(|t| t == "victim"),
        "the DROP did not run before the tail errored; tables: {tables:?}"
    );
}

// ----------------------------------------------------------------- refuse

#[test]
fn a_request_the_operation_does_not_admit_is_refused_with_the_directory_untouched() {
    let cases: [(&str, &[&str], &[&str]); 4] = [
        (
            "no where",
            &["filter-rows", "--on", "orders", "--name", "big_orders"],
            &["`where`", "`filter-rows`"],
        ),
        (
            "an argument filter-rows does not take",
            &[
                "filter-rows",
                "--on",
                "orders",
                "--name",
                "big_orders",
                "--arg",
                "where=amount > 100",
                "--arg",
                "limit=5",
            ],
            &["`limit`", "`where`"],
        ),
        (
            "an operation arc does not hold",
            &[
                "no-such-operation",
                "--on",
                "orders",
                "--name",
                "big_orders",
                "--arg",
                "where=amount > 100",
            ],
            &["`no-such-operation`", "filter-rows"],
        ),
        (
            "a table no step makes",
            &[
                "filter-rows",
                "--on",
                "customers",
                "--name",
                "big_orders",
                "--arg",
                "where=amount > 100",
            ],
            &["`customers`"],
        ),
    ];
    for (case, args, named) in cases {
        let protocol = Protocol::new();
        let before = protocol.files();
        let mut argv = vec!["operation", "record"];
        argv.extend_from_slice(args);
        let out = protocol.arc(&argv);
        refused(&out, named);
        assert_eq!(protocol.files(), before, "{case}: the directory changed");
    }
}

#[test]
fn the_terminal_refuses_an_argument_it_cannot_read_with_the_directory_untouched() {
    let cases: [(&[&str], &str); 2] = [
        (&["--arg", "where"], "`--arg where` must be KEY=VALUE"),
        (
            &["--arg", "where=amount > 100", "--arg", "where=amount > 200"],
            "`--arg where` is given more than once",
        ),
    ];
    for (args, named) in cases {
        let protocol = Protocol::new();
        let before = protocol.files();
        let mut argv = vec![
            "operation",
            "record",
            "filter-rows",
            "--on",
            "orders",
            "--name",
            "big_orders",
        ];
        argv.extend_from_slice(args);
        refused(&protocol.arc(&argv), &[named]);
        assert_eq!(protocol.files(), before, "{args:?}: the directory changed");
    }
}

#[test]
fn a_condition_holding_a_terminator_is_refused_with_the_directory_untouched() {
    // The plain terminator, and the two forms round one admitted because its
    // hand-written splitter read an `E'...'` backslash escape and a `--` comment
    // ended by a carriage return differently from DuckDB. Each carries a
    // top-level `;` DuckDB acts on, so `arc run` would drop `orders`; each is
    // refused instead, the directory untouched, and the message names the
    // condition. The lower-case `e'...'` is the same string literal.
    for condition in [
        "amount > 100; DROP TABLE orders",
        r"note = E'\'' ; DROP TABLE orders ; --'",
        r"note = e'\'' ; DROP TABLE orders ; --'",
        "amount > 100 --\r; DROP TABLE orders",
    ] {
        let protocol = Protocol::new();
        let before = protocol.files();
        let out = protocol.record_filter(condition);
        refused(&out, &["`where`", "`;`", condition]);
        assert_eq!(
            protocol.files(),
            before,
            "{condition:?}: the directory changed"
        );
    }
}

#[test]
fn a_condition_duckdb_cannot_parse_whole_is_refused_with_the_directory_untouched() {
    // A runnable, `;`-terminated prefix followed by an unparseable tail: a bare
    // word, and a `.` dot-command. DuckDB parses neither probe as a whole, so
    // the record-time check counts each `0`. But `arc run` hands the step's file
    // to the `duckdb` CLI, which runs the DROP before it reaches the tail it
    // cannot read (see `arc_run_runs_a_valid_prefix_before_an_unparseable_tail`),
    // so each is refused at record time instead, the directory untouched, and
    // the message names the argument. Round two recorded both.
    for condition in [
        "amount > 100; DROP TABLE orders; zzz",
        "amount > 100; DROP TABLE orders;\n.print done",
    ] {
        let protocol = Protocol::new();
        let before = protocol.files();
        let out = protocol.record_filter(condition);
        refused(&out, &["`where`", condition]);
        assert_eq!(
            protocol.files(),
            before,
            "{condition:?}: the directory changed"
        );
    }
}

#[test]
fn any_other_condition_is_recorded_as_written() {
    for condition in [
        "note = 'a;b'",                    // a `;` inside a string
        "amount > 100 and note like 'a%'", // a compound condition
        "amount > 100 /* ; */",            // a `;` inside a block comment
        "amount > 100;",                   // a bare trailing `;`: one statement, nothing after
    ] {
        let protocol = Protocol::new();
        ok(&protocol.record_filter(condition), condition);
        let model = protocol.read("models/02_big_orders.sql");
        assert_eq!(
            model.lines().last(),
            Some(format!("WHERE {condition};").as_str()),
            "the condition is recorded as written:\n{model}"
        );
    }
}

#[test]
fn an_order_that_is_not_one_statement_is_refused_with_the_directory_untouched() {
    // A real terminator, a runnable prefix with an unparseable tail (a bare word,
    // and a `.` dot-command), and the two forms a hand-written splitter missed —
    // an `E'...'` escape and a `--` comment ended by a carriage return — each
    // carries a top-level `;` DuckDB acts on under the `ORDER BY` probe, so
    // `arc run` would drop `orders`; each is refused, the directory untouched, and
    // the message names `order_by`. The lower-case `e'...'` is the same literal.
    for order in [
        "amount desc; DROP TABLE orders",
        "amount desc; DROP TABLE orders; zzz",
        "amount desc; DROP TABLE orders;\n.print done",
        r"note = E'\'' ; DROP TABLE orders ; --'",
        r"note = e'\'' ; DROP TABLE orders ; --'",
        "amount desc --\r; DROP TABLE orders",
    ] {
        let protocol = Protocol::new();
        let before = protocol.files();
        let out = protocol.record_sort(order);
        refused(&out, &["`order_by`"]);
        assert_eq!(protocol.files(), before, "{order:?}: the directory changed");
    }
}

#[test]
fn a_sort_missing_its_order_or_given_a_where_is_refused_with_the_directory_untouched() {
    // No `order_by` names the missing required argument; a `where` names the
    // argument sort-rows does not take. Each is refused, the directory untouched.
    let cases: [(&[&str], &str); 2] = [
        (
            &["sort-rows", "--on", "orders", "--name", "by_amount"],
            "`order_by`",
        ),
        (
            &[
                "sort-rows",
                "--on",
                "orders",
                "--name",
                "by_amount",
                "--arg",
                "where=amount > 100",
            ],
            "`where`",
        ),
    ];
    for (args, named) in cases {
        let protocol = Protocol::new();
        let before = protocol.files();
        let mut argv = vec!["operation", "record"];
        argv.extend_from_slice(args);
        refused(&protocol.arc(&argv), &[named]);
        assert_eq!(protocol.files(), before, "{args:?}: the directory changed");
    }
}

// --------------------------------------------------------------------- mcp

#[cfg(feature = "mcp")]
mod mcp {
    use std::io::Write;
    use std::process::Stdio;

    use serde_json::{Value, json};

    use super::*;

    /// Start `arc mcp` in `cwd`, send one `tools/call` of `operation_record` with
    /// `arguments`, and return the call's result.
    fn call_operation_record(cwd: &Path, history: &Path, arguments: Value) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(cwd)
            .env("ARCFORM_HISTORY_DIR", history)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn arc mcp");
        {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            let request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "operation_record", "arguments": arguments },
            });
            writeln!(stdin, "{request}").expect("write the request");
        }
        let out = child.wait_with_output().expect("wait for arc mcp");
        assert!(
            out.status.success(),
            "arc mcp failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
        let response: Value = serde_json::from_str(stdout.lines().next().expect("a response"))
            .expect("the response is JSON");
        response["result"].clone()
    }

    /// The text of a tool result.
    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap_or_default()
    }

    #[test]
    fn the_tool_writes_the_bytes_the_terminal_writes() {
        let terminal = Protocol::new();
        let agent = Protocol::new();
        ok(
            &terminal.record_filter("amount > 100"),
            "arc operation record",
        );

        // Started in the Protocol's directory with no `dir`, as `protocol_run` is.
        let result = call_operation_record(
            &agent.dir,
            &agent.history,
            json!({
                "operation": "filter-rows",
                "on": "orders",
                "name": "big_orders",
                "arguments": { "where": "amount > 100" },
            }),
        );
        assert_ne!(
            result["isError"],
            true,
            "the call failed: {}",
            text(&result)
        );
        assert_eq!(result["structuredContent"]["step"], "big_orders");
        assert_eq!(
            result["structuredContent"]["model"],
            "models/02_big_orders.sql"
        );

        // No file under either Protocol's directory names a way, in its name
        // or in its bytes: the way is kept in the history alone.
        for (protocol, name) in [(&terminal, "terminal"), (&agent, "agent")] {
            for (path, bytes) in protocol.files() {
                let text = String::from_utf8_lossy(&bytes);
                for way in ["terminal", "mcp"] {
                    assert!(
                        !path.to_string_lossy().contains(way) && !text.contains(way),
                        "{name}'s {} names the way `{way}`:\n{text}",
                        path.display()
                    );
                }
            }
        }

        assert_eq!(
            agent.files(),
            terminal.files(),
            "the Protocol the agent recorded into differs from the one the terminal did"
        );
        assert_eq!(agent.read("models/02_big_orders.sql"), BIG_ORDERS_MODEL);

        // The way differs in the history alone: the same versions, the same
        // bytes in each, and a different way beside them.
        let (by_terminal, by_agent) = (terminal.versions(), agent.versions());
        let kinds_and_ways = |versions: &[(String, String, String)]| -> Vec<(String, String)> {
            versions
                .iter()
                .map(|(_, kind, way)| (kind.clone(), way.clone()))
                .collect()
        };
        assert_eq!(
            kinds_and_ways(&by_terminal),
            [
                ("checkpoint".to_string(), "terminal".to_string()),
                ("save".to_string(), "terminal".to_string())
            ]
        );
        assert_eq!(
            kinds_and_ways(&by_agent),
            [
                ("checkpoint".to_string(), "mcp".to_string()),
                ("save".to_string(), "mcp".to_string())
            ]
        );
        for ((terminal_id, ..), (agent_id, ..)) in by_terminal.iter().zip(&by_agent) {
            assert_eq!(
                agent.arc(&["history", "show", agent_id]).stdout,
                terminal.arc(&["history", "show", terminal_id]).stdout,
                "the version {agent_id} holds other bytes than {terminal_id}"
            );
        }
    }

    #[test]
    fn the_tool_records_a_sort_as_the_terminal_does() {
        let terminal = Protocol::new();
        let agent = Protocol::new();
        ok(
            &terminal.record_sort("amount desc"),
            "arc operation record sort-rows",
        );

        let result = call_operation_record(
            &agent.dir,
            &agent.history,
            json!({
                "operation": "sort-rows",
                "on": "orders",
                "name": "by_amount",
                "arguments": { "order_by": "amount desc" },
            }),
        );
        assert_ne!(
            result["isError"],
            true,
            "the call failed: {}",
            text(&result)
        );
        assert_eq!(result["structuredContent"]["step"], "by_amount");
        assert_eq!(
            result["structuredContent"]["model"],
            "models/02_by_amount.sql"
        );

        assert_eq!(
            agent.files(),
            terminal.files(),
            "the Protocol the agent recorded into differs from the one the terminal did"
        );
        assert_eq!(agent.read("models/02_by_amount.sql"), BY_AMOUNT_MODEL);
    }

    #[test]
    fn the_tool_refuses_an_order_the_terminal_refuses_with_the_directory_untouched() {
        for order in [
            "amount desc; DROP TABLE orders",
            "amount desc; DROP TABLE orders; zzz",
            "amount desc; DROP TABLE orders;\n.print done",
            r"note = E'\'' ; DROP TABLE orders ; --'",
            r"note = e'\'' ; DROP TABLE orders ; --'",
            "amount desc --\r; DROP TABLE orders",
        ] {
            let protocol = Protocol::new();
            let before = protocol.files();
            let mut arguments = json!({
                "operation": "sort-rows", "on": "orders", "name": "by_amount",
                "arguments": { "order_by": order },
            });
            arguments["dir"] = json!(protocol.dir.to_str().unwrap());
            let result = call_operation_record(
                protocol.history.parent().unwrap(),
                &protocol.history,
                arguments.clone(),
            );
            assert_eq!(result["isError"], true, "{order:?} was not refused");
            assert!(
                text(&result).contains("`order_by`"),
                "the refusal of {order:?} does not name order_by: {}",
                text(&result)
            );
            assert_eq!(protocol.files(), before, "{order:?}: the directory changed");
        }
    }

    #[test]
    fn the_tool_refuses_what_the_terminal_refuses_with_the_directory_untouched() {
        let cases = [
            (
                json!({ "operation": "filter-rows", "on": "orders", "name": "big_orders" }),
                vec!["`where`"],
            ),
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": "amount > 100", "limit": 5 },
                }),
                vec!["`limit`"],
            ),
            (
                json!({
                    "operation": "no-such-operation", "on": "orders", "name": "big_orders",
                    "arguments": { "where": "amount > 100" },
                }),
                vec!["`no-such-operation`"],
            ),
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": "amount > 100; DROP TABLE orders" },
                }),
                vec!["`;`"],
            ),
            // The two forms round one admitted, refused over MCP as well as from
            // the terminal: an `E'...'` backslash escape and a `--` comment ended
            // by a carriage return. An MCP-only agent is the one operation_record
            // most guards, since it is the tool that writes a Protocol.
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": r"note = E'\'' ; DROP TABLE orders ; --'" },
                }),
                vec!["`;`"],
            ),
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": r"note = e'\'' ; DROP TABLE orders ; --'" },
                }),
                vec!["`;`"],
            ),
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": "amount > 100 --\r; DROP TABLE orders" },
                }),
                vec!["`;`"],
            ),
            // A runnable, `;`-terminated prefix with an unparseable tail: a bare
            // word, and a `.` dot-command. DuckDB parses neither probe whole, so
            // the check counts each 0; `arc run` would still run the DROP. Both
            // are refused over MCP as from the terminal; round two recorded both.
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": "amount > 100; DROP TABLE orders; zzz" },
                }),
                vec!["`where`"],
            ),
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": "amount > 100; DROP TABLE orders;\n.print done" },
                }),
                vec!["`where`"],
            ),
            (
                json!({
                    "operation": "filter-rows", "on": "customers", "name": "big_orders",
                    "arguments": { "where": "amount > 100" },
                }),
                vec!["`customers`"],
            ),
            (
                json!({
                    "operation": "filter-rows", "on": "orders", "name": "big_orders",
                    "arguments": { "where": 100 },
                }),
                vec!["`where`", "string"],
            ),
        ];
        for (mut arguments, named) in cases {
            let protocol = Protocol::new();
            let before = protocol.files();
            // Named by `dir`, from a server started elsewhere.
            arguments["dir"] = json!(protocol.dir.to_str().unwrap());
            let result = call_operation_record(
                protocol.history.parent().unwrap(),
                &protocol.history,
                arguments.clone(),
            );
            assert_eq!(result["isError"], true, "{arguments} was not refused");
            for name in &named {
                assert!(
                    text(&result).contains(name),
                    "the refusal of {arguments} does not name {name}: {}",
                    text(&result)
                );
            }
            assert_eq!(
                protocol.files(),
                before,
                "{arguments}: the directory changed"
            );
        }
    }
}
