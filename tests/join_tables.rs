//! `arc operation record join-tables`: a join of one table to another recorded as
//! a new step of a Protocol, then run by `arc run`, asked of the real `arc`
//! binary.
//!
//! The Protocol makes `orders` and `customers`. Order 3's customer is not among
//! the customers, and customer 30 has no order, so each kind of join keeps or
//! drops a row the others do not:
//!
//!   1. **left** on `using` keeps every order, the key once and first, and
//!      `customers`' other columns prefixed `customers_`, null for order 3;
//!   2. **full** on `using` keeps customer 30 too, with its key set;
//!   3. **condition** keeps both keys, `customers`' prefixed;
//!   4. **semi** and **anti** keep the orders with a customer and the order
//!      without one, in `orders`' columns alone;
//!   5. **header and graph** — the model names both tables, and the run's asset
//!      graph has the step read both;
//!   6. **mcp** — the same request sent to `arc mcp` writes the same bytes.
//!
//! Every recording is a version of the Protocol, so each run points
//! `ARCFORM_HISTORY_DIR` at a directory of its own, outside the Protocol.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A Protocol whose two steps make `orders` and `customers`.
const MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb

steps:
  - name: load_orders
    sql: models/01_orders.sql
  - name: load_customers
    sql: models/02_customers.sql
";

/// Three orders; the third's customer, 99, is not among the customers.
const ORDERS_SQL: &str = "\
CREATE OR REPLACE TABLE orders AS
SELECT * FROM (VALUES (1, 10, 'pen', 5), (2, 20, 'ink', 6), (3, 99, 'cap', 7))
    AS t(id, customer_id, name, amount);
";

/// Three customers; the third, 30, has no order.
const CUSTOMERS_SQL: &str = "\
CREATE OR REPLACE TABLE customers AS
SELECT * FROM (VALUES (10, 'Ann'), (20, 'Bo'), (30, 'Cy'))
    AS t(customer_id, name);
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
        std::fs::write(dir.join("models/02_customers.sql"), CUSTOMERS_SQL).unwrap();
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

    /// `arc operation record join-tables --on orders --read other=customers
    /// --name joined`, with each of `args` as an `--arg`.
    fn record_join(&self, args: &[&str]) -> Output {
        let mut argv = vec![
            "operation",
            "record",
            "join-tables",
            "--on",
            "orders",
            "--read",
            "other=customers",
            "--name",
            "joined",
        ];
        for arg in args {
            argv.extend_from_slice(&["--arg", arg]);
        }
        self.arc(&argv)
    }

    /// Record the join with `args`, then `arc run` the Protocol.
    fn record_and_run(&self, args: &[&str]) {
        ok(&self.record_join(args), "arc operation record join-tables");
        ok(&self.arc(&["run"]), "arc run");
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    fn db(&self) -> duckdb::Connection {
        duckdb::Connection::open(self.dir.join("shop.duckdb")).expect("open shop.duckdb")
    }

    /// The name of each column of `joined`, in the order the table holds them.
    fn columns(&self) -> Vec<String> {
        self.db()
            .prepare(
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_name = 'joined' ORDER BY ordinal_position",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Each row of `query` on the database `arc run` made, as two columns: an
    /// integer that may be null and a string that may be null.
    fn rows(&self, query: &str) -> Vec<(Option<i32>, Option<String>)> {
        self.db()
            .prepare(query)
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The `id` of each row of `joined`, in order.
    fn ids(&self) -> Vec<i32> {
        self.db()
            .prepare("SELECT id FROM joined ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// The `assets` of the one run contract `arc run` wrote.
    fn assets(&self) -> Vec<serde_json::Value> {
        let runs = self.dir.join("build/.arcform/runs");
        let contracts: Vec<PathBuf> = std::fs::read_dir(&runs)
            .unwrap_or_else(|e| panic!("runs dir {}: {e}", runs.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        assert_eq!(contracts.len(), 1, "one run, one contract: {contracts:?}");
        let contract: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&contracts[0]).unwrap()).unwrap();
        contract["assets"].as_array().expect("assets array").clone()
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

#[test]
fn a_left_join_on_using_keeps_every_order_the_key_once_and_the_others_columns_prefixed() {
    let protocol = Protocol::new();
    protocol.record_and_run(&["using=customer_id"]);
    assert_eq!(
        protocol.read("models/03_joined.sql").lines().next(),
        Some("-- generated: join-tables on orders, customers")
    );
    assert_eq!(
        protocol.columns(),
        ["customer_id", "id", "name", "amount", "customers_name"]
    );
    assert_eq!(
        protocol.rows("SELECT id, customers_name FROM joined ORDER BY id"),
        [
            (Some(1), Some("Ann".to_string())),
            (Some(2), Some("Bo".to_string())),
            (Some(3), None),
        ],
        "every order is kept, and order 3's customer is null"
    );
}

#[test]
fn the_join_step_reads_both_tables_in_the_runs_asset_graph() {
    let protocol = Protocol::new();
    protocol.record_and_run(&["using=customer_id"]);
    let assets = protocol.assets();
    for table in ["orders", "customers"] {
        let asset = assets
            .iter()
            .find(|a| a["name"] == table)
            .unwrap_or_else(|| panic!("no asset `{table}`: {assets:?}"));
        assert!(
            asset["consumed_by"]
                .as_array()
                .is_some_and(|by| by.contains(&serde_json::json!("joined"))),
            "`{table}` is not read by the join step: {asset}"
        );
    }
}

#[test]
fn a_full_join_on_using_keeps_the_customer_with_no_order_with_its_key_set() {
    let protocol = Protocol::new();
    protocol.record_and_run(&["kind=full", "using=customer_id"]);
    assert_eq!(
        protocol.rows("SELECT customer_id, customers_name FROM joined WHERE id IS NULL"),
        [(Some(30), Some("Cy".to_string()))]
    );
}

#[test]
fn a_join_on_a_condition_keeps_both_keys_the_others_prefixed() {
    let protocol = Protocol::new();
    protocol.record_and_run(&["condition=orders.customer_id = customers.customer_id"]);
    assert_eq!(
        protocol.columns(),
        [
            "id",
            "customer_id",
            "name",
            "amount",
            "customers_customer_id",
            "customers_name"
        ]
    );
}

#[test]
fn a_semi_join_keeps_the_orders_with_a_customer_and_an_anti_join_the_other() {
    for (kind, ids) in [("kind=semi", vec![1, 2]), ("kind=anti", vec![3])] {
        let protocol = Protocol::new();
        protocol.record_and_run(&[kind, "using=customer_id"]);
        assert_eq!(protocol.ids(), ids, "{kind}");
        assert_eq!(
            protocol.columns(),
            ["id", "customer_id", "name", "amount"],
            "{kind} keeps `orders`' columns alone"
        );
    }
}

#[test]
fn a_join_of_a_table_to_itself_in_another_case_is_refused_with_the_directory_untouched() {
    let protocol = Protocol::new();
    let snapshot = || {
        let mut models: Vec<String> = std::fs::read_dir(protocol.dir.join("models"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        models.sort();
        (protocol.read("arcform.yaml"), models)
    };
    let before = snapshot();
    let out = protocol.arc(&[
        "operation",
        "record",
        "join-tables",
        "--on",
        "orders",
        "--read",
        "other=Orders",
        "--name",
        "joined",
        "--arg",
        "using=customer_id",
    ]);
    assert!(!out.status.success(), "a join of orders to itself recorded");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for named in ["`table`", "`other`", "itself"] {
        assert!(
            stderr.contains(named),
            "the refusal does not name {named}:\n{stderr}"
        );
    }
    assert_eq!(snapshot(), before, "the refusal changed the directory");
}

#[cfg(feature = "mcp")]
#[test]
fn the_tool_writes_the_bytes_the_terminal_writes() {
    use std::io::Write;
    use std::process::Stdio;

    let terminal = Protocol::new();
    let agent = Protocol::new();
    ok(
        &terminal.record_join(&["kind=full", "using=customer_id"]),
        "arc operation record join-tables",
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&agent.dir)
        .env("ARCFORM_HISTORY_DIR", &agent.history)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "operation_record",
                "arguments": {
                    "operation": "join-tables",
                    "on": "orders",
                    "reads": { "other": "customers" },
                    "name": "joined",
                    "arguments": { "kind": "full", "using": ["customer_id"] },
                },
            },
        });
        writeln!(stdin, "{request}").expect("write the request");
    }
    let out = child.wait_with_output().expect("wait for arc mcp");
    ok(&out, "arc mcp");
    let stdout = String::from_utf8(out.stdout).expect("stdout is UTF-8");
    let response: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("a response")).unwrap();
    assert_eq!(
        response["result"]["isError"], false,
        "the tool refused: {response}"
    );

    for file in ["models/03_joined.sql", "arcform.yaml"] {
        assert_eq!(agent.read(file), terminal.read(file), "{file}");
    }
}

#[test]
fn a_right_join_on_using_keeps_the_customer_with_no_order_with_its_key_set() {
    let protocol = Protocol::new();
    protocol.record_and_run(&["kind=right", "using=customer_id"]);
    assert_eq!(
        protocol.columns(),
        ["customer_id", "id", "name", "amount", "customers_name"]
    );
    assert_eq!(
        protocol.rows("SELECT customer_id, customers_name FROM joined WHERE id IS NULL"),
        [(Some(30), Some("Cy".to_string()))],
        "customer 30 is kept with its key set"
    );
    assert_eq!(
        protocol.rows("SELECT customer_id, name FROM joined WHERE id IS NULL"),
        [(Some(30), None)],
        "the order columns are null for it"
    );
    assert_eq!(
        protocol.rows("SELECT count(*)::INTEGER, NULL::VARCHAR FROM joined"),
        [(Some(3), None)],
        "orders 1 and 2 match, and customer 30 has none; order 3 is dropped"
    );
}

#[test]
fn a_cross_join_pairs_every_order_with_every_customer() {
    let protocol = Protocol::new();
    protocol.record_and_run(&["kind=cross"]);
    assert_eq!(
        protocol.columns(),
        [
            "id",
            "customer_id",
            "name",
            "amount",
            "customers_customer_id",
            "customers_name"
        ]
    );
    assert_eq!(
        protocol.rows("SELECT count(*)::INTEGER, NULL::VARCHAR FROM joined"),
        [(Some(9), None)],
        "three orders by three customers"
    );
}
