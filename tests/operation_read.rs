//! `arc operation read` and the `arc mcp` tool `operation_read`: what a step was
//! recorded as, read back by a program from the record its generated model
//! carries, asked of the real `arc` binary.
//!
//! The model's first line names the operation and its tables for a person, the
//! tables unquoted, so a table whose name holds `, ` reads ambiguously there.
//! The second line is the record, one line of JSON, and these tests hold that a
//! program can read the step back from it:
//!
//!   1. **round trip** — a step recorded from `filter-rows` on a table named
//!      `north, south` reads back as that operation on that one table with its
//!      condition, `as_recorded` true, and reading writes no file;
//!   2. **roles** — a list role and a single role read back by role, a list as a
//!      list and a single role as a name, and an operation that reads no table
//!      reads back with no `on`;
//!   3. **the same bytes** — the record's keys are sorted, so arguments sent in
//!      either order write the same model;
//!   4. **hand edit** — a model edited under the header still answers its record,
//!      with `as_recorded` false, and a record line edited to name another step
//!      or model cannot change which the answer names;
//!   5. **no record** — a hand-written SQL step, a model whose header has no
//!      record under it, and an `op:` step answer `operation` null;
//!   6. **refused** — no step of that name, and a record line that is not a JSON
//!      object naming its operation, are refused with nothing on stdout;
//!   7. **mcp** — `operation_read` answers what the command prints, and its
//!      answer, sent to `operation_record` as given, records the same record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

/// The table the first step makes, named so the header cannot be split on it.
const COMMA_TABLE: &str = "north, south";

/// A Protocol whose two steps make `north, south` and `orders`.
const MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb
steps:
  - name: load_regions
    sql: models/01_regions.sql
  - name: load_orders
    sql: models/02_orders.sql
";

/// A Protocol in a temporary directory, with a history directory of its own.
struct Protocol {
    root: tempfile::TempDir,
}

impl Protocol {
    fn new() -> Protocol {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("shop");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("arcform.yaml"), MANIFEST).unwrap();
        std::fs::write(
            dir.join("models/01_regions.sql"),
            "CREATE OR REPLACE TABLE \"north, south\" AS SELECT 1 AS id, 150 AS amount;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("models/02_orders.sql"),
            "CREATE OR REPLACE TABLE orders AS SELECT 2 AS id, 50 AS amount;\n",
        )
        .unwrap();
        Protocol { root }
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join("shop")
    }

    fn history(&self) -> PathBuf {
        self.root.path().join("history")
    }

    /// Run the real `arc` with `args` and `--dir` the Protocol.
    fn arc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(self.root.path())
            .env("ARCFORM_HISTORY_DIR", self.history())
            .args(args)
            .arg("--dir")
            .arg(self.dir())
            .output()
            .expect("spawn arc")
    }

    /// Run `arc` with `args`, which must exit 0.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.arc(args);
        assert!(
            out.status.success(),
            "arc {args:?} failed (code {:?}):\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("stdout is UTF-8")
    }

    /// `arc operation read <step>`: the one JSON object it printed.
    fn read(&self, step: &str) -> Value {
        let stdout = self.ok(&["operation", "read", step]);
        let answer: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "arc operation read {step} printed other than one JSON document ({e}):\n{stdout}"
            )
        });
        assert!(
            answer.is_object(),
            "arc operation read {step} printed {answer}"
        );
        answer
    }

    /// The bytes of the model at `rel`, relative to the Protocol.
    fn model(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir().join(rel)).expect("the model was written")
    }

    /// Every file under the Protocol's directory and its bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, base: &Path, into: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, base, into);
                } else {
                    let rel = path.strip_prefix(base).unwrap().to_path_buf();
                    into.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut files = BTreeMap::new();
        walk(&self.dir(), &self.dir(), &mut files);
        files
    }
}

#[test]
fn a_filter_of_a_table_named_with_a_comma_reads_back_as_that_one_table() {
    let protocol = Protocol::new();
    protocol.ok(&[
        "operation",
        "record",
        "filter-rows",
        "--on",
        COMMA_TABLE,
        "--name",
        "big",
        "--arg",
        "where=amount > 100",
    ]);
    assert_eq!(
        protocol.model("models/03_big.sql"),
        "-- generated: filter-rows on north, south\n\
         -- operation: {\"arguments\":{\"where\":\"amount > 100\"},\"on\":\"north, south\",\"operation\":\"filter-rows\"}\n\
         CREATE OR REPLACE TABLE \"big\" AS\n\
         SELECT *\n\
         FROM \"north, south\"\n\
         WHERE amount > 100;\n",
        "the model carries the record on its second line"
    );

    let before = protocol.snapshot();
    let answer = protocol.read("big");
    assert_eq!(
        answer,
        json!({
            "step": "big",
            "model": "models/03_big.sql",
            "operation": "filter-rows",
            "on": COMMA_TABLE,
            "arguments": { "where": "amount > 100" },
            "as_recorded": true,
        })
    );
    assert_eq!(protocol.snapshot(), before, "reading a step writes no file");
}

#[test]
fn a_list_role_reads_back_as_a_list_and_a_single_role_as_a_name() {
    let protocol = Protocol::new();
    protocol.ok(&[
        "operation",
        "record",
        "append-rows",
        "--on",
        "orders",
        "--read",
        &format!("other={COMMA_TABLE}"),
        "--name",
        "all_orders",
    ]);
    assert_eq!(
        protocol.read("all_orders"),
        json!({
            "step": "all_orders",
            "model": "models/03_all_orders.sql",
            "operation": "append-rows",
            "on": "orders",
            "reads": { "other": [COMMA_TABLE] },
            "arguments": {},
            "as_recorded": true,
        })
    );

    protocol.ok(&[
        "operation",
        "record",
        "join-tables",
        "--on",
        "orders",
        "--read",
        &format!("other={COMMA_TABLE}"),
        "--arg",
        "using=id",
        "--name",
        "joined",
    ]);
    assert_eq!(
        protocol.read("joined"),
        json!({
            "step": "joined",
            "model": "models/04_joined.sql",
            "operation": "join-tables",
            "on": "orders",
            "reads": { "other": COMMA_TABLE },
            "arguments": { "using": ["id"] },
            "as_recorded": true,
        })
    );
}

#[test]
fn an_operation_that_reads_no_table_reads_back_with_no_on() {
    let protocol = Protocol::new();
    protocol.ok(&[
        "operation",
        "record",
        "read-file",
        "--name",
        "raw",
        "--arg",
        "path=data/raw.csv",
    ]);
    assert_eq!(
        protocol.read("raw"),
        json!({
            "step": "raw",
            "model": "models/03_raw.sql",
            "operation": "read-file",
            "arguments": { "path": "data/raw.csv" },
            "as_recorded": true,
        })
    );
}

#[test]
fn a_record_line_cannot_say_which_step_or_model_it_is() {
    let protocol = Protocol::new();
    protocol.ok(&[
        "operation",
        "record",
        "filter-rows",
        "--on",
        "orders",
        "--name",
        "big",
        "--arg",
        "where=amount > 100",
    ]);
    let path = protocol.dir().join("models/03_big.sql");
    let model = protocol.model("models/03_big.sql");
    std::fs::write(
        &path,
        model.replace(
            "-- operation: {",
            "-- operation: {\"step\":\"other\",\"model\":\"x.sql\",\"extra\":1,",
        ),
    )
    .unwrap();
    let answer = protocol.read("big");
    assert_eq!(answer["step"], "big", "{answer}");
    assert_eq!(answer["model"], "models/03_big.sql", "{answer}");
    assert!(answer.get("extra").is_none(), "{answer}");
    assert_eq!(answer["as_recorded"], false, "{answer}");
}

#[test]
fn arguments_given_in_either_order_write_the_same_model() {
    let one = Protocol::new();
    let other = Protocol::new();
    let record = |protocol: &Protocol, args: [&str; 2]| {
        protocol.ok(&[
            "operation",
            "record",
            "dedupe-rows",
            "--on",
            "orders",
            "--name",
            "unique_orders",
            "--arg",
            args[0],
            "--arg",
            args[1],
        ]);
    };
    record(&one, ["key=id", "order_by=amount desc"]);
    record(&other, ["order_by=amount desc", "key=id"]);
    let model = one.model("models/03_unique_orders.sql");
    assert_eq!(model, other.model("models/03_unique_orders.sql"));
    assert!(
        model.contains(
            "-- operation: {\"arguments\":{\"key\":\"id\",\"order_by\":\"amount desc\"},"
        ),
        "the record's keys are sorted: {model}"
    );
}

#[test]
fn a_model_edited_under_the_header_answers_its_record_as_not_as_recorded() {
    let protocol = Protocol::new();
    protocol.ok(&[
        "operation",
        "record",
        "filter-rows",
        "--on",
        "orders",
        "--name",
        "big",
        "--arg",
        "where=amount > 100",
    ]);
    let path = protocol.dir().join("models/03_big.sql");
    let model = protocol.model("models/03_big.sql");
    std::fs::write(
        &path,
        model.replace("WHERE amount > 100;", "WHERE amount > 10;"),
    )
    .unwrap();
    let answer = protocol.read("big");
    assert_eq!(answer["operation"], "filter-rows");
    assert_eq!(answer["arguments"], json!({ "where": "amount > 100" }));
    assert_eq!(answer["as_recorded"], false, "{answer}");
}

#[test]
fn a_step_with_no_record_answers_no_operation() {
    let protocol = Protocol::new();
    assert_eq!(
        protocol.read("load_orders"),
        json!({ "step": "load_orders", "operation": null }),
        "a hand-written SQL step"
    );

    // A model an arc that wrote no record left: the header alone.
    std::fs::write(
        protocol.dir().join("models/03_old.sql"),
        "-- generated: filter-rows on orders\nCREATE OR REPLACE TABLE \"old\" AS\nSELECT *\nFROM orders\nWHERE amount > 100;\n",
    )
    .unwrap();
    let mut manifest = MANIFEST.to_string();
    manifest.push_str("  - name: old\n    sql: models/03_old.sql\n");
    std::fs::write(protocol.dir().join("arcform.yaml"), &manifest).unwrap();
    assert_eq!(
        protocol.read("old"),
        json!({ "step": "old", "operation": null }),
        "a model with a header and no record"
    );

    protocol.ok(&[
        "operator",
        "record",
        "http_fetch",
        "--name",
        "fetched",
        "--with",
        r#"{"url": "https://example.com/data.csv", "out": "data/data.csv"}"#,
    ]);
    assert_eq!(
        protocol.read("fetched"),
        json!({ "step": "fetched", "operation": null }),
        "an op: step"
    );
}

#[test]
fn no_such_step_and_an_unreadable_record_are_refused() {
    let protocol = Protocol::new();
    let out = protocol.arc(&["operation", "read", "nope"]);
    assert!(!out.status.success(), "a step that is not there is refused");
    assert!(out.stdout.is_empty(), "nothing on stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no step named `nope`") && stderr.contains("load_orders"),
        "the refusal names the step and the steps there are: {stderr}"
    );

    std::fs::write(
        protocol.dir().join("models/02_orders.sql"),
        "-- generated: filter-rows on x\n-- operation: not json\nCREATE OR REPLACE TABLE orders AS SELECT 1 AS id;\n",
    )
    .unwrap();
    let out = protocol.arc(&["operation", "read", "load_orders"]);
    assert!(!out.status.success(), "an unreadable record is refused");
    assert!(out.stdout.is_empty(), "nothing on stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("-- operation:") && stderr.contains("02_orders.sql"),
        "the refusal names the line and the model: {stderr}"
    );
}

#[test]
fn help_says_what_the_verb_does() {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .args(["operation", "read", "--help"])
        .output()
        .expect("spawn arc");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("as_recorded"), "{help}");
}

/// `operation_read` through `arc mcp`, compared with what `arc operation read`
/// prints.
#[cfg(feature = "mcp")]
mod mcp {
    use std::io::Write;
    use std::process::Stdio;

    use super::*;

    /// Start `arc mcp` for `protocol`, send each request as one line, close stdin,
    /// and return the result of each in order.
    fn mcp_session(protocol: &Protocol, requests: &[Value]) -> Vec<Value> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(protocol.root.path())
            .env("ARCFORM_HISTORY_DIR", protocol.history())
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
            "arc mcp failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("stdout is UTF-8")
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).expect("response is JSON")["result"].clone()
            })
            .collect()
    }

    fn call(id: u64, tool: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        })
    }

    #[test]
    fn the_tool_answers_what_the_command_prints_and_its_answer_records_again() {
        let protocol = Protocol::new();
        protocol.ok(&[
            "operation",
            "record",
            "join-tables",
            "--on",
            "orders",
            "--read",
            &format!("other={COMMA_TABLE}"),
            "--arg",
            "using=id",
            "--name",
            "joined",
        ]);
        let dir = protocol.dir().to_str().unwrap().to_string();
        let results = mcp_session(
            &protocol,
            &[call(
                1,
                "operation_read",
                json!({ "step": "joined", "dir": dir }),
            )],
        );
        assert_eq!(results[0]["isError"], false, "{}", results[0]);
        let answer = results[0]["structuredContent"].clone();
        assert_eq!(answer, protocol.read("joined"));

        let mut request = json!({ "name": "joined_again", "dir": dir });
        for key in ["operation", "on", "reads", "arguments"] {
            if let Some(value) = answer.get(key) {
                request[key] = value.clone();
            }
        }
        let results = mcp_session(&protocol, &[call(1, "operation_record", request)]);
        assert_eq!(results[0]["isError"], false, "{}", results[0]);
        let second_line = |rel: &str| protocol.model(rel).lines().nth(1).unwrap().to_string();
        assert_eq!(
            second_line("models/04_joined_again.sql"),
            second_line("models/03_joined.sql"),
            "the answer, sent to operation_record as given, records the same record"
        );
    }
}
