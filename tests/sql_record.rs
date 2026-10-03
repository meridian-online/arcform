//! `arc sql record` and the `arc mcp` tool `sql_record`: a SQL statement recorded
//! as a new step of a Protocol, as the operation it is read as or as a SQL step,
//! asked of the real `arc` binary.
//!
//!   1. **an operation** — a statement `arc sql recognise` reads as `filter-rows`
//!      is recorded as the step `arc operation record filter-rows` writes, byte
//!      for byte, and a comment after its condition is in neither file;
//!   2. **a SQL step** — any other statement DuckDB parses is recorded as a model
//!      with no `-- generated:` line; for a `SELECT` the model's first line makes
//!      the step's table and the statement as typed follows it, and a statement
//!      the line cannot go in front of is written as typed;
//!   3. **nothing runs** — recording changes `arcform.yaml`, adds the model and
//!      appends to the folder's log, and changes nothing else in the Protocol's
//!      directory;
//!   4. **run** — `arc run` then makes the table each kind of step makes;
//!   5. **refuse** — text DuckDB cannot parse, more than one statement, a filter of
//!      a table no step makes, a name a step already has, a description that is
//!      empty or spans lines and a statement that opens with the generated line are
//!      each refused with the directory untouched and a message naming the fault;
//!   6. **describe and history** — `--description` is written on the step, and
//!      each recording is a version of the Protocol that names the way it was
//!      reached, and its save's line in the folder's log names the step it
//!      added;
//!   7. **mcp** — `sql_record` writes the bytes the command writes, and is
//!      listed, described and refused as the command is.
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

/// The step that makes `orders`: one row at 100, one either side of it.
const ORDERS_SQL: &str = "\
CREATE OR REPLACE TABLE orders AS
SELECT * FROM (VALUES (1, 50, 'a;b'), (2, 150, 'abc'), (3, 300, 'xyz'), (4, 100, 'a'))
    AS t(id, amount, note);
";

/// The statement read as `filter-rows` on `orders`.
const FILTER: &str = "SELECT * FROM orders WHERE amount > 100";

/// The statement recorded as a SQL step, typed with no `;` and no final newline.
const BY_AMOUNT: &str = "SELECT id, amount FROM orders WHERE amount > 100 ORDER BY amount DESC";

/// What recording [`FILTER`] as `big_orders` appends to `arcform.yaml`.
const APPENDED_BIG_ORDERS: &str = "  - name: big_orders\n    sql: models/02_big_orders.sql\n";

/// What recording [`BY_AMOUNT`] as `by_amount` appends to `arcform.yaml`.
const APPENDED_BY_AMOUNT: &str = "  - name: by_amount\n    sql: models/02_by_amount.sql\n";

/// The model recording [`FILTER`] writes, byte for byte.
const BIG_ORDERS_MODEL: &str = "\
-- generated: filter-rows on orders
CREATE OR REPLACE TABLE \"big_orders\" AS
SELECT *
FROM orders
WHERE amount > 100;
";

/// The model recording [`BY_AMOUNT`] as `by_amount` writes, byte for byte.
const BY_AMOUNT_MODEL: &str = "\
CREATE OR REPLACE TABLE \"by_amount\" AS
SELECT id, amount FROM orders WHERE amount > 100 ORDER BY amount DESC
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
        Self::with_manifest(MANIFEST)
    }

    fn with_manifest(manifest: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("shop");
        let history = root.path().join("history");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("arcform.yaml"), manifest).unwrap();
        std::fs::write(dir.join("models/01_orders.sql"), ORDERS_SQL).unwrap();
        Protocol {
            _root: root,
            dir,
            history,
        }
    }

    /// A Protocol whose one step makes the table `order` — a word DuckDB does not
    /// read as a table name unless it is in quotes — holding the rows of `orders`.
    fn making_order() -> Self {
        let protocol = Self::new();
        std::fs::write(
            protocol.dir.join("models/01_orders.sql"),
            ORDERS_SQL.replacen("TABLE orders", "TABLE \"order\"", 1),
        )
        .unwrap();
        protocol
    }

    /// Run the real `arc` binary with `args` in the Protocol's directory.
    fn arc(&self, args: &[&str]) -> Output {
        self.arc_from(&self.dir, args)
    }

    /// Run the real `arc` binary with `args` in `cwd`, with this Protocol's
    /// history store: the directory arc is run from is not this Protocol's, and
    /// a `--dir` names it.
    fn arc_from(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(cwd)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .args(args)
            .output()
            .expect("spawn arc")
    }

    /// `arc sql record <sql> --name <name>`.
    fn record(&self, sql: &str, name: &str) -> Output {
        self.arc(&["sql", "record", sql, "--name", name])
    }

    /// `arc sql record <sql> --name <name> --description <description>`.
    fn record_described(&self, sql: &str, name: &str, description: &str) -> Output {
        self.arc(&[
            "sql",
            "record",
            sql,
            "--name",
            name,
            "--description",
            description,
        ])
    }

    /// `arc operation record filter-rows --on orders --name <name> --arg where=<condition>`.
    fn record_filter_by_name(&self, name: &str, condition: &str) -> Output {
        let arg = format!("where={condition}");
        self.arc(&[
            "operation",
            "record",
            "filter-rows",
            "--on",
            "orders",
            "--name",
            name,
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
    /// its bytes, the folder's log's with each line's time, interface and version
    /// left out.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, files);
                } else {
                    let mut bytes = std::fs::read(&path).unwrap();
                    if path.file_name().is_some_and(|name| name == LOG) {
                        bytes = log_without_times(&bytes);
                    }
                    files.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&self.dir, &self.dir, &mut files);
        files
    }

    /// The rows `sql` selects from the database `arc run` made, as `(id, amount)`.
    fn rows(&self, sql: &str) -> Vec<(i32, i32)> {
        let db = duckdb::Connection::open(self.dir.join("shop.duckdb")).expect("open shop.duckdb");
        let mut statement = db.prepare(sql).unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}

/// The log a Protocol's folder holds, one line per version arc records.
const LOG: &str = "arcform-log.txt";

/// The folder's log with each line's time, interface and version id left out,
/// which differ between two Protocols that record the same versions at other
/// times or through another interface: what is left is each line's file, kind,
/// step and change.
fn log_without_times(bytes: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| {
            let mut object: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(line).unwrap();
            for key in ["at", "interface", "version"] {
                object.remove(key);
            }
            format!("{}\n", serde_json::Value::Object(object))
        })
        .collect::<String>()
        .into_bytes()
}

/// The interface each line of the folder's log in `dir` names, in order.
#[cfg(feature = "mcp")]
fn ways_in_log(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(LOG))
        .unwrap()
        .lines()
        .map(|line| {
            let object: serde_json::Value = serde_json::from_str(line).unwrap();
            object["interface"].as_str().unwrap().to_string()
        })
        .collect()
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

/// The one line `out` printed on stdout.
fn one_line(out: &Output) -> String {
    let stdout = String::from_utf8(out.stdout.clone()).expect("stdout is UTF-8");
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout is one line, not:\n{stdout}"
    );
    stdout
}

// ------------------------------------------------------------ an operation

#[test]
fn a_filter_typed_as_sql_leaves_the_bytes_the_same_filter_recorded_by_name_leaves() {
    let typed = Protocol::new();
    let by_name = Protocol::new();
    let out = typed.record(FILTER, "big_orders");
    ok(&out, "arc sql record");
    ok(
        &by_name.record_filter_by_name("big_orders", "amount > 100"),
        "arc operation record",
    );

    assert_eq!(
        typed.files(),
        by_name.files(),
        "the Protocol the statement was recorded into differs from the one the \
         operation was recorded into"
    );
    assert_eq!(typed.read("models/02_big_orders.sql"), BIG_ORDERS_MODEL);
    assert_eq!(
        typed.read("models/02_big_orders.sql").lines().next(),
        Some("-- generated: filter-rows on orders")
    );
    assert_eq!(
        typed.read("arcform.yaml"),
        format!("{MANIFEST}{APPENDED_BIG_ORDERS}")
    );

    let line = one_line(&out);
    for named in [
        "big_orders",
        "models/02_big_orders.sql",
        "filter-rows",
        "orders",
    ] {
        assert!(
            line.contains(named),
            "the line does not name {named}: {line}"
        );
    }
    assert!(
        !line.contains("SQL step"),
        "a filter is not recorded as a SQL step: {line}"
    );
}

#[test]
fn a_filter_of_a_table_named_with_a_reserved_word_leaves_the_bytes_the_same_filter_recorded_by_name_leaves()
 {
    let typed = Protocol::making_order();
    let by_name = Protocol::making_order();
    let out = typed.record("SELECT * FROM \"order\" WHERE amount > 100", "big_order");
    ok(&out, "arc sql record");
    ok(
        &by_name.arc(&[
            "operation",
            "record",
            "filter-rows",
            "--on",
            "order",
            "--name",
            "big_order",
            "--arg",
            "where=amount > 100",
        ]),
        "arc operation record",
    );

    assert_eq!(
        typed.files(),
        by_name.files(),
        "the Protocol the statement was recorded into differs from the one the \
         operation was recorded into"
    );
    assert_eq!(
        typed.read("models/02_big_order.sql"),
        "\
-- generated: filter-rows on order
CREATE OR REPLACE TABLE \"big_order\" AS
SELECT *
FROM \"order\"
WHERE amount > 100;
",
        "the table is written in quotes, as DuckDB reads it"
    );
    let line = one_line(&out);
    assert!(
        line.contains("filter-rows") && !line.contains("SQL step"),
        "a filter of a table named with a reserved word is recorded as the operation: {line}"
    );

    ok(&typed.arc(&["run"]), "arc run");
    assert_eq!(
        typed.rows("SELECT id, amount FROM big_order ORDER BY id"),
        [(2, 150), (3, 300)],
        "big_order holds the rows of order whose amount is over 100, and no other"
    );
}

#[test]
fn a_comment_after_the_condition_is_in_neither_file() {
    let by_name = Protocol::new();
    ok(
        &by_name.record_filter_by_name("big_orders", "amount > 100"),
        "arc operation record",
    );
    for statement in [
        "SELECT * FROM orders WHERE amount > 100 -- big",
        "SELECT * FROM orders WHERE amount > 100 /* big */",
        "SELECT * FROM orders WHERE amount > 100; -- big",
    ] {
        let typed = Protocol::new();
        ok(&typed.record(statement, "big_orders"), "arc sql record");
        assert_eq!(
            typed.files(),
            by_name.files(),
            "{statement:?} left other bytes than the filter recorded by name"
        );
        for (path, bytes) in typed.files() {
            assert!(
                !String::from_utf8_lossy(&bytes).contains("big;")
                    && !String::from_utf8_lossy(&bytes).contains("big */"),
                "the comment is in {}",
                path.display()
            );
        }
    }
}

// ------------------------------------------------------------- a SQL step

#[test]
fn a_select_is_recorded_as_a_sql_step_that_makes_its_table() {
    let protocol = Protocol::new();
    let before = protocol.files();
    let out = protocol.record(BY_AMOUNT, "by_amount");
    ok(&out, "arc sql record");

    assert_eq!(protocol.read("models/02_by_amount.sql"), BY_AMOUNT_MODEL);
    assert!(
        !protocol
            .read("models/02_by_amount.sql")
            .contains("-- generated:"),
        "a SQL step's model carries no generated line"
    );
    assert_eq!(
        protocol.read("arcform.yaml"),
        format!("{MANIFEST}{APPENDED_BY_AMOUNT}"),
        "the step is appended and every other byte of arcform.yaml is kept"
    );

    let line = one_line(&out);
    for named in ["by_amount", "models/02_by_amount.sql", "SQL step"] {
        assert!(
            line.contains(named),
            "the line does not name {named}: {line}"
        );
    }
    assert!(
        !line.contains("filter-rows"),
        "a SQL step is not recorded as an operation: {line}"
    );

    // Recording runs nothing: the files the directory gained are the model and the
    // folder's log, no
    // database file is there, and every file it held is as it was, arcform.yaml
    // apart.
    let mut after = protocol.files();
    assert!(
        after
            .keys()
            .all(|path| path.extension().is_none_or(|e| e != "duckdb")),
        "a database file is there: {:?}",
        after.keys().collect::<Vec<_>>()
    );
    let model = after
        .remove(Path::new("models/02_by_amount.sql"))
        .expect("the model was written");
    assert_eq!(model, BY_AMOUNT_MODEL.as_bytes());
    after.remove(Path::new("arcform.yaml"));
    after
        .remove(Path::new(LOG))
        .expect("the log names the versions recorded");
    let mut untouched = before;
    untouched.remove(Path::new("arcform.yaml"));
    assert_eq!(after, untouched, "recording changed a file it did not own");
}

#[test]
fn a_filter_recording_runs_nothing_either() {
    let protocol = Protocol::new();
    let before = protocol.files();
    ok(&protocol.record(FILTER, "big_orders"), "arc sql record");
    let mut after = protocol.files();
    after.remove(Path::new("models/02_big_orders.sql"));
    after.remove(Path::new("arcform.yaml"));
    after
        .remove(Path::new(LOG))
        .expect("the log names the versions recorded");
    let mut untouched = before;
    untouched.remove(Path::new("arcform.yaml"));
    assert_eq!(after, untouched, "recording changed a file it did not own");
}

#[test]
fn a_statement_that_ends_in_a_newline_is_written_with_none_added() {
    let protocol = Protocol::new();
    ok(
        &protocol.record(&format!("{BY_AMOUNT}\n"), "by_amount"),
        "arc sql record",
    );
    assert_eq!(
        protocol.read("models/02_by_amount.sql"),
        BY_AMOUNT_MODEL,
        "one final newline, and not two"
    );
}

#[test]
fn the_step_is_appended_at_the_indentation_of_the_step_before_it() {
    let manifest = "\
name: shop
engine: duckdb
db: shop.duckdb

steps:
    - name: load_orders
      sql: models/01_orders.sql
";
    let protocol = Protocol::with_manifest(manifest);
    ok(&protocol.record(BY_AMOUNT, "by_amount"), "arc sql record");
    assert_eq!(
        protocol.read("arcform.yaml"),
        format!("{manifest}    - name: by_amount\n      sql: models/02_by_amount.sql\n"),
        "the step carries the indentation of the one before it and the bytes before it are kept"
    );
}

#[test]
fn a_statement_the_line_cannot_precede_is_written_as_typed() {
    let protocol = Protocol::new();
    let statement =
        "CREATE OR REPLACE TABLE top_order AS SELECT * FROM orders ORDER BY amount DESC LIMIT 1";
    let out = protocol.record(statement, "top_order");
    ok(&out, "arc sql record");
    assert_eq!(
        protocol.read("models/02_top_order.sql"),
        format!("{statement}\n"),
        "the statement's bytes and one final newline, with no line above it"
    );
    assert!(one_line(&out).contains("SQL step"));
}

// --------------------------------------------------------------------- run

#[test]
fn arc_run_makes_the_table_a_filter_typed_as_sql_makes() {
    let protocol = Protocol::new();
    ok(&protocol.record(FILTER, "big_orders"), "arc sql record");
    ok(&protocol.arc(&["run"]), "arc run");
    assert_eq!(
        protocol.rows("SELECT id, amount FROM big_orders ORDER BY id"),
        [(2, 150), (3, 300)],
        "big_orders holds the rows of orders whose amount is over 100, and no other"
    );
}

#[test]
fn arc_run_makes_the_table_of_a_sql_step() {
    let protocol = Protocol::new();
    ok(&protocol.record(BY_AMOUNT, "by_amount"), "arc record");
    ok(&protocol.arc(&["run"]), "arc run");
    assert_eq!(
        protocol.rows("SELECT id, amount FROM by_amount"),
        [(3, 300), (2, 150)],
        "by_amount holds the id and amount of the rows of orders over 100, in the order asked"
    );
}

#[test]
fn arc_run_makes_the_table_a_statement_written_as_typed_makes_itself() {
    let protocol = Protocol::new();
    ok(
        &protocol.record(
            "CREATE OR REPLACE TABLE top_order AS SELECT * FROM orders ORDER BY amount DESC LIMIT 1",
            "top_order",
        ),
        "arc sql record",
    );
    ok(&protocol.arc(&["run"]), "arc run");
    assert_eq!(
        protocol.rows("SELECT id, amount FROM top_order"),
        [(3, 300)],
        "top_order holds the one row of orders with the greatest amount"
    );
}

#[test]
fn arc_run_makes_a_table_of_each_form_of_select() {
    // A `;`, a `WITH` and a statement that opens with `FROM`: each is text the
    // line goes in front of, and each runs and makes its table.
    let protocol = Protocol::new();
    let forms = [
        ("ended", "SELECT id, amount FROM orders ORDER BY id;", 4),
        (
            "with_cte",
            "WITH big AS (SELECT * FROM orders WHERE amount > 100) SELECT id, amount FROM big",
            2,
        ),
        ("from_first", "FROM orders SELECT id, amount", 4),
    ];
    for (n, (name, statement, _)) in forms.iter().enumerate() {
        ok(&protocol.record(statement, name), name);
        assert_eq!(
            protocol.read(&format!("models/{:02}_{name}.sql", n + 2)),
            format!("CREATE OR REPLACE TABLE \"{name}\" AS\n{statement}\n"),
            "{name}: the line, then the statement as typed"
        );
    }
    ok(&protocol.arc(&["run"]), "arc run");
    for (name, _, rows) in forms {
        assert_eq!(
            protocol
                .rows(&format!("SELECT id, amount FROM {name}"))
                .len(),
            rows,
            "{name} holds the rows of its statement"
        );
    }
}

// ------------------------------------------------------------------ refuse

#[test]
fn a_request_arc_cannot_record_is_refused_with_the_directory_untouched() {
    for (what, args, named) in [
        (
            "text DuckDB cannot parse",
            vec![
                "sql",
                "record",
                "SELECT * FROM orders WHERE amount >",
                "--name",
                "x",
            ],
            vec!["cannot parse"],
        ),
        (
            "more than one statement",
            vec!["sql", "record", "SELECT 1; SELECT 2", "--name", "x"],
            vec!["more than one statement"],
        ),
        (
            "a filter of a table no step makes",
            vec![
                "sql",
                "record",
                "SELECT * FROM nothere WHERE amount > 100",
                "--name",
                "x",
            ],
            vec!["no step of the protocol makes a table called `nothere`"],
        ),
        (
            "a name a step already has",
            vec!["sql", "record", BY_AMOUNT, "--name", "load_orders"],
            vec!["load_orders"],
        ),
        (
            "a name a step already has, for a filter",
            vec!["sql", "record", FILTER, "--name", "load_orders"],
            vec!["load_orders"],
        ),
        (
            "a name YAML would read as a comment",
            vec!["sql", "record", BY_AMOUNT, "--name", "by # amount"],
            vec!["by # amount", "'#'"],
        ),
        (
            "a name YAML would read as a mapping",
            vec!["sql", "record", BY_AMOUNT, "--name", "by:amount"],
            vec!["by:amount", "':'"],
        ),
        (
            "an empty description",
            vec![
                "sql",
                "record",
                BY_AMOUNT,
                "--name",
                "x",
                "--description",
                "",
            ],
            vec!["description", "is empty"],
        ),
        (
            "a description that spans lines",
            vec![
                "sql",
                "record",
                BY_AMOUNT,
                "--name",
                "x",
                "--description",
                "a\nb",
            ],
            vec!["description", "spans lines"],
        ),
        (
            "a description that spans lines, for a filter",
            vec![
                "sql",
                "record",
                FILTER,
                "--name",
                "x",
                "--description",
                "a\nb",
            ],
            vec!["description", "spans lines"],
        ),
        (
            "a statement that opens with the line that marks a file arc may rewrite",
            vec![
                "sql",
                "record",
                "-- generated: mine\nUPDATE orders SET amount = 0",
                "--name",
                "x",
            ],
            vec!["-- generated:"],
        ),
    ] {
        let protocol = Protocol::new();
        let before = protocol.files();
        refused(&protocol.arc(&args), &named);
        assert_eq!(protocol.files(), before, "{what}: the directory changed");
    }
}

#[test]
fn the_refusal_of_a_filter_of_an_unmade_table_is_the_one_arc_operation_record_gives() {
    let protocol = Protocol::new();
    let by_name = protocol.arc(&[
        "operation",
        "record",
        "filter-rows",
        "--on",
        "nothere",
        "--name",
        "x",
        "--arg",
        "where=amount > 100",
    ]);
    let typed = protocol.record("SELECT * FROM nothere WHERE amount > 100", "x");
    assert!(!typed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&typed.stderr),
        String::from_utf8_lossy(&by_name.stderr)
    );
}

// ----------------------------------------------------------- description

#[test]
fn a_description_is_written_after_sql_for_each_kind_and_the_model_is_as_without_one() {
    let filter = Protocol::new();
    ok(
        &filter.record_described(FILTER, "big_orders", "Keep the orders worth chasing"),
        "arc sql record --description",
    );
    assert_eq!(
        filter.read("arcform.yaml"),
        format!("{MANIFEST}{APPENDED_BIG_ORDERS}    description: Keep the orders worth chasing\n")
    );
    assert_eq!(filter.read("models/02_big_orders.sql"), BIG_ORDERS_MODEL);

    let select = Protocol::new();
    ok(
        &select.record_described(BY_AMOUNT, "by_amount", "Rank the orders worth chasing"),
        "arc sql record --description",
    );
    assert_eq!(
        select.read("arcform.yaml"),
        format!("{MANIFEST}{APPENDED_BY_AMOUNT}    description: Rank the orders worth chasing\n")
    );
    assert_eq!(select.read("models/02_by_amount.sql"), BY_AMOUNT_MODEL);
}

// ----------------------------------------------------------------- history

#[test]
fn each_recording_is_a_version_of_the_protocol() {
    for (statement, name) in [(FILTER, "big_orders"), (BY_AMOUNT, "by_amount")] {
        let protocol = Protocol::new();
        ok(&protocol.record(statement, name), "arc sql record");
        let versions = protocol.versions();
        let seen: Vec<(&str, &str)> = versions
            .iter()
            .map(|(_, kind, way)| (kind.as_str(), way.as_str()))
            .collect();
        assert_eq!(
            seen,
            [("checkpoint", "terminal"), ("save", "terminal")],
            "{name}: the state the recording replaced and the state it wrote: {versions:?}"
        );
        assert_eq!(
            protocol.arc(&["history", "show", &versions[0].0]).stdout,
            MANIFEST.as_bytes(),
            "{name}: the checkpoint is the Protocol as it was"
        );
        // The folder's log names the step the recording added, on the save's
        // line, and no step on the checkpoint's.
        let log = std::fs::read_to_string(protocol.dir.join(LOG)).unwrap();
        let steps: Vec<(String, Option<String>, Option<String>)> = log
            .lines()
            .map(|line| {
                let object: serde_json::Value = serde_json::from_str(line).unwrap();
                let text = |key: &str| object.get(key).and_then(|v| v.as_str()).map(str::to_string);
                (text("kind").unwrap(), text("step"), text("change"))
            })
            .collect();
        assert_eq!(
            steps,
            [
                ("checkpoint".to_string(), None, None),
                (
                    "save".to_string(),
                    Some(name.to_string()),
                    Some("added".to_string())
                ),
            ],
            "{name}: {log}"
        );
    }
}

#[test]
fn a_statement_recorded_with_dir_from_another_directory_leaves_the_bytes_it_leaves_run_inside_it() {
    // Both kinds of step: a filter, recorded as the operation, and a statement
    // recorded as a SQL step. `elsewhere` is a Protocol of its own, so a verb that
    // reads the working directory in place of `--dir` records there and exits 0.
    for (statement, name) in [(FILTER, "big_orders"), (BY_AMOUNT, "by_amount")] {
        let inside = Protocol::new();
        let named = Protocol::new();
        let elsewhere = Protocol::new();
        let elsewhere_before = elsewhere.files();
        ok(&inside.record(statement, name), "arc sql record");

        let out = named.arc_from(
            &elsewhere.dir,
            &[
                "sql",
                "record",
                statement,
                "--name",
                name,
                "--dir",
                named.dir.to_str().unwrap(),
            ],
        );
        ok(&out, "arc sql record --dir");

        assert_eq!(
            named.files(),
            inside.files(),
            "{name}: the Protocol --dir names differs from the one the same \
             statement recorded inside it leaves"
        );
        assert_eq!(
            elsewhere.files(),
            elsewhere_before,
            "{name}: the directory arc was run from changed"
        );
    }
}

// --------------------------------------------------------------------- mcp

#[cfg(feature = "mcp")]
mod mcp {
    use std::io::Write;
    use std::process::Stdio;

    use serde_json::{Value, json};

    use super::*;

    /// Start `arc mcp` in `cwd`, send `request`, and return its one response.
    fn ask(cwd: &Path, history: &Path, request: &Value) -> Value {
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

    /// One `tools/call` of `sql_record` with `arguments`, started in `protocol`'s
    /// directory with no `dir`, as `protocol_run` is.
    fn call(protocol: &Protocol, arguments: Value) -> Value {
        call_from(&protocol.dir, protocol, arguments)
    }

    /// [`call`] with the server started in `cwd`, which is not `protocol`'s
    /// directory when `arguments` carries a `dir` naming it.
    fn call_from(cwd: &Path, protocol: &Protocol, arguments: Value) -> Value {
        ask(
            cwd,
            &protocol.history,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "sql_record", "arguments": arguments },
            }),
        )
    }

    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap_or_default()
    }

    #[test]
    fn the_tool_is_listed_with_the_input_schema_the_command_takes() {
        let dir = tempfile::tempdir().unwrap();
        let result = ask(
            dir.path(),
            dir.path(),
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        );
        let tools = result["tools"].as_array().expect("a list of tools");
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == "sql_record")
            .expect("sql_record is listed");
        let schema = &tool["inputSchema"];
        let mut required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| key.as_str().unwrap())
            .collect();
        required.sort_unstable();
        assert_eq!(required, ["name", "sql"]);
        for key in ["sql", "name", "description", "dir"] {
            assert_eq!(schema["properties"][key]["type"], "string", "{key}");
        }
    }

    #[test]
    fn the_help_and_the_instructions_name_the_tool() {
        let help = Command::new(env!("CARGO_BIN_EXE_arc"))
            .args(["mcp", "--help"])
            .output()
            .expect("spawn arc mcp --help");
        ok(&help, "arc mcp --help");
        assert!(
            String::from_utf8_lossy(&help.stdout).contains("sql_record"),
            "arc mcp --help does not name sql_record"
        );

        let dir = tempfile::tempdir().unwrap();
        let result = ask(
            dir.path(),
            dir.path(),
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        );
        assert!(
            result["instructions"]
                .as_str()
                .unwrap_or_default()
                .contains("sql_record"),
            "the instructions initialize returns do not name sql_record"
        );
    }

    #[test]
    fn the_tool_writes_the_bytes_the_command_writes_and_names_what_it_recorded() {
        for (statement, name, operation, on) in [
            (FILTER, "big_orders", json!("filter-rows"), json!("orders")),
            (BY_AMOUNT, "by_amount", Value::Null, Value::Null),
        ] {
            let terminal = Protocol::new();
            let agent = Protocol::new();
            ok(&terminal.record(statement, name), "arc sql record");

            let result = call(&agent, json!({ "sql": statement, "name": name }));
            assert_ne!(result["isError"], true, "{name}: {}", text(&result));
            let recorded = &result["structuredContent"];
            assert_eq!(recorded["step"], name);
            assert_eq!(recorded["model"], format!("models/02_{name}.sql"));
            assert_eq!(recorded["operation"], operation, "{name}");
            assert_eq!(recorded["on"], on, "{name}");

            // The spec and the model are the same bytes either way, and the
            // folder's logs differ in the way alone.
            let (mut by_agent, mut by_terminal) = (agent.files(), terminal.files());
            let agent_log = by_agent.remove(Path::new(LOG)).expect("the agent's log");
            let terminal_log = by_terminal
                .remove(Path::new(LOG))
                .expect("the terminal's log");
            assert_eq!(
                by_agent, by_terminal,
                "{name}: the Protocol the agent recorded into differs from the terminal's"
            );
            assert_eq!(
                agent_log, terminal_log,
                "{name}: the two logs differ in more than the way"
            );
            assert_eq!(ways_in_log(&agent.dir), ["mcp", "mcp"], "{name}");
            assert_eq!(
                ways_in_log(&terminal.dir),
                ["terminal", "terminal"],
                "{name}"
            );

            // The way differs in the history as in the log.
            let ways = |protocol: &Protocol| -> Vec<(String, String)> {
                protocol
                    .versions()
                    .into_iter()
                    .map(|(_, kind, way)| (kind, way))
                    .collect()
            };
            assert_eq!(
                ways(&terminal),
                [
                    ("checkpoint".to_string(), "terminal".to_string()),
                    ("save".to_string(), "terminal".to_string())
                ]
            );
            assert_eq!(
                ways(&agent),
                [
                    ("checkpoint".to_string(), "mcp".to_string()),
                    ("save".to_string(), "mcp".to_string())
                ],
                "{name}: each version the tool wrote names mcp"
            );
        }
    }

    #[test]
    fn the_tool_writes_the_description_the_command_writes() {
        let terminal = Protocol::new();
        let agent = Protocol::new();
        ok(
            &terminal.record_described(BY_AMOUNT, "by_amount", "Rank the big orders"),
            "arc sql record --description",
        );
        let result = call(
            &agent,
            json!({ "sql": BY_AMOUNT, "name": "by_amount", "description": "Rank the big orders" }),
        );
        assert_ne!(result["isError"], true, "{}", text(&result));
        assert_eq!(agent.files(), terminal.files());
    }

    #[test]
    fn the_tool_records_into_the_directory_dir_names_from_a_server_started_elsewhere() {
        // `elsewhere` is a Protocol of its own, so a tool that reads the working
        // directory in place of `dir` records there and reports success.
        for (statement, name) in [(FILTER, "big_orders"), (BY_AMOUNT, "by_amount")] {
            let inside = Protocol::new();
            let named = Protocol::new();
            let elsewhere = Protocol::new();
            let elsewhere_before = elsewhere.files();
            let started_inside = call(&inside, json!({ "sql": statement, "name": name }));
            assert_ne!(
                started_inside["isError"],
                true,
                "{name}: {}",
                text(&started_inside)
            );

            let result = call_from(
                &elsewhere.dir,
                &named,
                json!({ "sql": statement, "name": name, "dir": named.dir.to_str().unwrap() }),
            );
            assert_ne!(result["isError"], true, "{name}: {}", text(&result));

            assert_eq!(
                named.files(),
                inside.files(),
                "{name}: the Protocol `dir` names differs from the one the same call \
                 leaves when the server is started inside it"
            );
            assert_eq!(
                elsewhere.files(),
                elsewhere_before,
                "{name}: the server's own directory changed"
            );
        }
    }

    #[test]
    fn the_tool_takes_a_null_description_as_none() {
        let terminal = Protocol::new();
        ok(&terminal.record(BY_AMOUNT, "by_amount"), "arc sql record");
        for arguments in [
            json!({ "sql": BY_AMOUNT, "name": "by_amount", "description": null }),
            json!({ "sql": BY_AMOUNT, "name": "by_amount" }),
        ] {
            let agent = Protocol::new();
            let result = call(&agent, arguments);
            assert_ne!(result["isError"], true, "{}", text(&result));
            assert_eq!(agent.files(), terminal.files());
        }
    }

    #[test]
    fn the_tool_refuses_what_the_command_refuses_with_the_directory_untouched() {
        for (what, arguments, named) in [
            (
                "text DuckDB cannot parse",
                json!({ "sql": "SELECT * FROM orders WHERE amount >", "name": "x" }),
                "cannot parse",
            ),
            (
                "more than one statement",
                json!({ "sql": "SELECT 1; SELECT 2", "name": "x" }),
                "more than one statement",
            ),
            (
                "a filter of a table no step makes",
                json!({ "sql": "SELECT * FROM nothere WHERE amount > 100", "name": "x" }),
                "`nothere`",
            ),
            (
                "a name a step already has",
                json!({ "sql": BY_AMOUNT, "name": "load_orders" }),
                "load_orders",
            ),
            (
                "a description that spans lines",
                json!({ "sql": BY_AMOUNT, "name": "x", "description": "a\nb" }),
                "spans lines",
            ),
            ("no name", json!({ "sql": BY_AMOUNT }), "`name` is required"),
            ("no statement", json!({ "name": "x" }), "`sql` is required"),
            (
                "a description that is not a string",
                json!({ "sql": BY_AMOUNT, "name": "x", "description": 4 }),
                "`description` must be a string",
            ),
        ] {
            let protocol = Protocol::new();
            let before = protocol.files();
            let result = call(&protocol, arguments);
            assert_eq!(result["isError"], true, "{what}: {}", text(&result));
            assert!(
                text(&result).contains(named),
                "{what}: the error does not name {named}: {}",
                text(&result)
            );
            assert_eq!(protocol.files(), before, "{what}: the directory changed");
        }
    }
}
