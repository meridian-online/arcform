//! `arc operation record append-rows`: the rows of one or more tables appended
//! to a table's, recorded as a new step of a Protocol, then run by `arc run`,
//! asked of the real `arc` binary.
//!
//! The Protocol makes `orders_2024(id, amount)` and `orders_2025(id, amount,
//! channel)`, with one row in both:
//!
//!   1. **union** — the row count is the sum, `channel` is null for 2024's
//!      rows, and the duplicate is kept twice;
//!   2. **header and graph** — the model names both tables, and the run's asset
//!      graph has the step read both;
//!   3. **mcp** — a union of three tables sent to `arc mcp` writes its members
//!      in the order given.
//!
//! Every recording is a version of the Protocol, so each run points
//! `ARCFORM_HISTORY_DIR` at a directory of its own, outside the Protocol.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A Protocol whose steps make the three tables.
const MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb

steps:
  - name: load_2024
    sql: models/01_orders_2024.sql
  - name: load_2025
    sql: models/02_orders_2025.sql
  - name: load_2026
    sql: models/03_orders_2026.sql
";

/// Two orders; order 2 is also in 2025.
const ORDERS_2024_SQL: &str = "\
CREATE OR REPLACE TABLE orders_2024 AS
SELECT * FROM (VALUES (1, 5), (2, 6)) AS t(id, amount);
";

/// Two orders, with a column 2024 lacks; order 2 repeats 2024's.
const ORDERS_2025_SQL: &str = "\
CREATE OR REPLACE TABLE orders_2025 AS
SELECT * FROM (VALUES (2, 6, 'web'), (3, 7, 'shop')) AS t(id, amount, channel);
";

/// One order.
const ORDERS_2026_SQL: &str = "\
CREATE OR REPLACE TABLE orders_2026 AS
SELECT * FROM (VALUES (4, 8)) AS t(id, amount);
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
        std::fs::write(dir.join("models/01_orders_2024.sql"), ORDERS_2024_SQL).unwrap();
        std::fs::write(dir.join("models/02_orders_2025.sql"), ORDERS_2025_SQL).unwrap();
        std::fs::write(dir.join("models/03_orders_2026.sql"), ORDERS_2026_SQL).unwrap();
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

    /// `arc operation record append-rows --on orders_2024 --read other=<each of
    /// others> --name unioned`.
    fn record_union(&self, others: &[&str]) -> Output {
        let mut argv = vec![
            "operation",
            "record",
            "append-rows",
            "--on",
            "orders_2024",
            "--name",
            "unioned",
        ];
        let reads: Vec<String> = others.iter().map(|o| format!("other={o}")).collect();
        for read in &reads {
            argv.extend_from_slice(&["--read", read]);
        }
        self.arc(&argv)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    fn db(&self) -> duckdb::Connection {
        duckdb::Connection::open(self.dir.join("shop.duckdb")).expect("open shop.duckdb")
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
fn a_union_keeps_every_row_of_both_tables_nulls_the_missing_column_and_keeps_a_duplicate_twice() {
    let protocol = Protocol::new();
    ok(
        &protocol.record_union(&["orders_2025"]),
        "arc operation record append-rows",
    );
    ok(&protocol.arc(&["run"]), "arc run");
    assert_eq!(
        protocol.read("models/04_unioned.sql").lines().next(),
        Some("-- generated: append-rows on orders_2024, orders_2025")
    );
    let rows: Vec<(i32, i32, Option<String>)> = protocol
        .db()
        .prepare("SELECT id, amount, channel FROM unioned ORDER BY id, channel NULLS FIRST")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        [
            (1, 5, None),
            (2, 6, None),
            (2, 6, Some("web".to_string())),
            (3, 7, Some("shop".to_string())),
        ],
        "the count is the sum, 2024's rows have no channel and the duplicate is kept"
    );
}

#[test]
fn the_union_step_reads_both_tables_in_the_runs_asset_graph() {
    let protocol = Protocol::new();
    ok(
        &protocol.record_union(&["orders_2025"]),
        "arc operation record append-rows",
    );
    ok(&protocol.arc(&["run"]), "arc run");
    let assets = protocol.assets();
    for table in ["orders_2024", "orders_2025"] {
        let asset = assets
            .iter()
            .find(|a| a["name"] == table)
            .unwrap_or_else(|| panic!("no asset `{table}`: {assets:?}"));
        assert!(
            asset["consumed_by"]
                .as_array()
                .is_some_and(|by| by.contains(&serde_json::json!("unioned"))),
            "`{table}` is not read by the union step: {asset}"
        );
    }
}

#[cfg(feature = "mcp")]
#[test]
fn a_union_of_three_tables_through_the_tool_writes_the_members_in_order() {
    use std::io::Write;
    use std::process::Stdio;

    let terminal = Protocol::new();
    let agent = Protocol::new();
    ok(
        &terminal.record_union(&["orders_2026", "orders_2025"]),
        "arc operation record append-rows",
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
                    "operation": "append-rows",
                    "on": "orders_2024",
                    "reads": { "other": ["orders_2026", "orders_2025"] },
                    "name": "unioned",
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

    let model = agent.read("models/04_unioned.sql");
    let at = |table: &str| model.find(&format!("SELECT * FROM {table}")).unwrap();
    assert!(
        at("orders_2024") < at("orders_2026") && at("orders_2026") < at("orders_2025"),
        "the members are not in the order given:\n{model}"
    );
    for file in ["models/04_unioned.sql", "arcform.yaml"] {
        assert_eq!(agent.read(file), terminal.read(file), "{file}");
    }
}
