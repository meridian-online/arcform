//! `arc operation record read-file`: a table made from a file, a glob of files
//! or a URL, recorded as a new step of a Protocol, then run by `arc run`, asked
//! of the real `arc` binary.
//!
//! The Protocol has no steps yet, only three CSV files beside its manifest:
//!
//!   1. **file and glob** — a step reading one file and a step reading a glob
//!      of two make tables of their rows, and the model's header names the
//!      operation alone;
//!   2. **missing** — a path that matches nothing records, and `arc run` fails
//!      naming it;
//!   3. **remote** — a URL records with a note on stderr that it is read live,
//!      and the run's asset graph has a `remote` node the step reads;
//!   4. **mcp** — the same URL sent to `arc mcp` answers `notes` with the one
//!      sentence.
//!
//! Nothing touches the network: a Protocol that reads a URL opens with a step
//! that fails, so the run stops before the URL is read and the contract, which
//! the graph is read from, is written either way. Every recording is a version
//! of the Protocol, so each run points `ARCFORM_HISTORY_DIR` at a directory of
//! its own, outside the Protocol.

use std::path::PathBuf;
use std::process::{Command, Output};

/// A Protocol with no steps, as `arc create-protocol` writes one.
const MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb

steps: []
";

/// A Protocol whose one step fails, so a step recorded after it never runs.
const HALTING_MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb

steps:
  - name: halt
    command: \"false\"
";

/// Three orders.
const ORDERS_CSV: &str = "id,amount\n1,5\n2,6\n3,7\n";

/// Two orders of one year, and one of the next, in the same columns.
const ORDERS_2024_CSV: &str = "id,amount\n10,1\n11,2\n";
const ORDERS_2025_CSV: &str = "id,amount\n20,3\n";

/// A URL that is never fetched.
const URL: &str = "https://example.invalid/a.csv";

/// A Protocol as it stands before anything is recorded, and a history store of
/// its own beside it, outside the Protocol's directory.
struct Protocol {
    _root: tempfile::TempDir,
    dir: PathBuf,
    history: PathBuf,
}

impl Protocol {
    fn new(manifest: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("shop");
        let history = root.path().join("history");
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("arcform.yaml"), manifest).unwrap();
        std::fs::write(dir.join("data/orders.csv"), ORDERS_CSV).unwrap();
        std::fs::write(dir.join("data/2024.csv"), ORDERS_2024_CSV).unwrap();
        std::fs::write(dir.join("data/2025.csv"), ORDERS_2025_CSV).unwrap();
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

    /// `arc operation record read-file --name <name> --arg path=<path>`.
    fn record_read(&self, name: &str, path: &str) -> Output {
        let path = format!("path={path}");
        self.arc(&[
            "operation",
            "record",
            "read-file",
            "--name",
            name,
            "--arg",
            &path,
        ])
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
    }

    fn count(&self, table: &str) -> i64 {
        duckdb::Connection::open(self.dir.join("shop.duckdb"))
            .expect("open shop.duckdb")
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap_or_else(|e| panic!("count {table}: {e}"))
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
fn a_file_and_a_glob_each_make_a_table_of_their_rows_in_a_protocol_with_no_steps() {
    let protocol = Protocol::new(MANIFEST);
    let recorded = protocol.record_read("orders", "data/orders.csv");
    ok(&recorded, "arc operation record read-file");
    assert!(
        String::from_utf8_lossy(&recorded.stderr)
            .lines()
            .all(|l| !l.starts_with("note:")),
        "a path inside the folder is noted:\n{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    ok(
        &protocol.record_read("years", "data/20*.csv"),
        "arc operation record read-file of a glob",
    );
    ok(&protocol.arc(&["run"]), "arc run");

    let model = protocol.read("models/01_orders.sql");
    assert_eq!(model.lines().next(), Some("-- generated: read-file"));
    assert!(model.contains("FROM 'data/orders.csv';"), "{model}");
    assert_eq!(protocol.count("orders"), 3);
    assert_eq!(
        protocol.count("years"),
        3,
        "both files of the glob are read"
    );
}

#[test]
fn a_path_that_matches_nothing_records_and_the_run_fails_naming_it() {
    let protocol = Protocol::new(MANIFEST);
    ok(
        &protocol.record_read("missing", "data/missing.csv"),
        "arc operation record read-file of a missing file",
    );
    let run = protocol.arc(&["run"]);
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(!run.status.success(), "the run succeeded:\n{output}");
    assert!(
        output.contains("step 'missing' failed")
            && output.contains("No files found that match the pattern \"data/missing.csv\""),
        "the run did not fail in the `missing` step on DuckDB's own error:\n{output}"
    );
}

#[test]
fn a_url_records_with_a_note_that_it_is_read_live_and_is_a_remote_node_the_step_reads() {
    let protocol = Protocol::new(HALTING_MANIFEST);
    let recorded = protocol.record_read("fetched", URL);
    ok(&recorded, "arc operation record read-file of a URL");
    let stderr = String::from_utf8_lossy(&recorded.stderr);
    let notes: Vec<&str> = stderr.lines().filter(|l| l.starts_with("note: ")).collect();
    assert_eq!(notes.len(), 1, "{stderr}");
    assert!(
        notes[0].contains(&format!("`{URL}`")) && notes[0].contains("live"),
        "{stderr}"
    );

    let run = protocol.arc(&["run"]);
    assert!(
        !run.status.success(),
        "halt must fail so the URL is never read"
    );
    let assets = protocol.assets();
    let remote = assets
        .iter()
        .find(|a| a["name"] == URL)
        .unwrap_or_else(|| panic!("{URL} is no node of the graph: {assets:?}"));
    assert_eq!(remote["kind"], "remote", "{remote}");
    assert!(
        remote["consumed_by"]
            .as_array()
            .is_some_and(|by| by.contains(&serde_json::json!("fetched"))),
        "the URL is not read by the step: {remote}"
    );
}

#[cfg(feature = "mcp")]
#[test]
fn a_url_through_the_tool_answers_the_one_note() {
    use std::io::Write;
    use std::process::Stdio;

    let protocol = Protocol::new(MANIFEST);
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&protocol.dir)
        .env("ARCFORM_HISTORY_DIR", &protocol.history)
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
                    "operation": "read-file",
                    "name": "fetched",
                    "arguments": { "path": URL },
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
    let result = &response["result"];
    assert_eq!(result["isError"], false, "the tool refused: {response}");
    let notes = result["structuredContent"]["notes"]
        .as_array()
        .unwrap_or_else(|| panic!("no notes: {response}"));
    assert_eq!(notes.len(), 1, "{response}");
    let note = notes[0].as_str().expect("a note is a string");
    assert!(
        note.contains(&format!("`{URL}`")) && note.contains("live") && !note.contains('\n'),
        "{note}"
    );
    assert_eq!(
        protocol.read("models/01_fetched.sql").lines().next(),
        Some("-- generated: read-file")
    );
}
