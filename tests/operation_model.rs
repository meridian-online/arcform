//! `arcform::spec::operation_model`: the model `arc operation record` would write
//! for a request, given by the library with nothing written and no Protocol read.
//!
//!   1. **the same bytes** — for a request of each shape (one table, a second
//!      table, a list of tables, no table, an integer argument, a list argument,
//!      a description), the model is the file the real `arc operation record`
//!      writes for it in a Protocol, byte for byte, and the notes are the notes it
//!      prints;
//!   2. **the same refusals** — what `arc operation record` refuses before it
//!      looks at the Protocol is refused here with the same message;
//!   3. **no Protocol** — a table no step makes and a name a step already has are
//!      not refused, since there is no Protocol to ask; `arc operation record`
//!      still refuses them.

use std::path::PathBuf;
use std::process::{Command, Output};

use arcform::spec::{OperationModel, OperationRequest, operation_model};
use serde_json::{Map, Value, json};

/// A Protocol whose steps make `orders` and `returns`, for the record side.
const MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb
steps:
  - name: load_orders
    sql: models/01_orders.sql
  - name: load_returns
    sql: models/02_returns.sql
";

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
        std::fs::write(
            dir.join("models/01_orders.sql"),
            "CREATE OR REPLACE TABLE orders AS SELECT 1 AS id, 150 AS amount;\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("models/02_returns.sql"),
            "CREATE OR REPLACE TABLE returns AS SELECT 1 AS id, 150 AS amount;\n",
        )
        .unwrap();
        Protocol {
            _root: root,
            dir,
            history,
        }
    }

    fn arc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(&self.dir)
            .env("ARCFORM_HISTORY_DIR", &self.history)
            .args(args)
            .output()
            .expect("spawn arc")
    }

    /// The bytes of the model the manifest's step `name` cites.
    fn model_of(&self, name: &str) -> String {
        let manifest = arcform::spec::Manifest::load(&self.dir).unwrap();
        let step = manifest.steps.iter().find(|s| s.name == name).unwrap();
        std::fs::read_to_string(self.dir.join(step.sql.as_deref().unwrap())).unwrap()
    }
}

/// One request, as the command line takes it and as the library takes it.
struct Case {
    long_name: &'static str,
    on: Option<&'static str>,
    reads: Value,
    name: &'static str,
    arguments: Value,
    description: Option<&'static str>,
    /// The command line's `--read` and `--arg` flags for the same request.
    flags: &'static [&'static str],
}

impl Case {
    fn model(&self) -> arcform::spec::Result<OperationModel> {
        let reads: Map<String, Value> = self.reads.as_object().unwrap().clone();
        let arguments: Map<String, Value> = self.arguments.as_object().unwrap().clone();
        operation_model(&OperationRequest {
            long_name: self.long_name,
            on: self.on,
            reads: &reads,
            name: self.name,
            arguments: &arguments,
            description: self.description,
        })
    }

    fn record(&self, protocol: &Protocol) -> Output {
        // `--name=…` in one word, so a name opening with `-` is the name's value
        // and not a flag of its own.
        let name = format!("--name={}", self.name);
        let mut args = vec!["operation", "record", self.long_name, &name];
        if let Some(on) = self.on {
            args.extend(["--on", on]);
        }
        if let Some(description) = self.description {
            args.extend(["--description", description]);
        }
        args.extend(self.flags);
        protocol.arc(&args)
    }
}

/// The lines `arc` printed on stderr as `note: …`, without the prefix.
fn notes(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|line| line.strip_prefix("note: ").map(str::to_string))
        .collect()
}

/// `arc`'s refusal on stderr, without its `error: ` prefix.
fn refusal(out: &Output) -> String {
    assert!(!out.status.success(), "arc did not refuse");
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr
        .trim_end()
        .strip_prefix("error: ")
        .unwrap_or_else(|| panic!("no `error: ` on stderr:\n{stderr}"))
        .to_string()
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            long_name: "filter-rows",
            on: Some("orders"),
            reads: json!({}),
            name: "big_orders",
            arguments: json!({ "where": "amount > 100" }),
            description: Some("Keep the orders worth chasing: over 100"),
            flags: &["--arg", "where=amount > 100"],
        },
        Case {
            long_name: "limit-rows",
            on: Some("orders"),
            reads: json!({}),
            name: "top_orders",
            arguments: json!({ "count": 2, "order_by": "amount desc" }),
            description: None,
            flags: &["--arg", "order_by=amount desc", "--arg", "count=2"],
        },
        Case {
            long_name: "join-tables",
            on: None,
            reads: json!({ "table": "orders", "other": "returns" }),
            name: "orders_returned",
            arguments: json!({ "using": ["id", "amount"] }),
            description: None,
            flags: &[
                "--read",
                "table=orders",
                "--read",
                "other=returns",
                "--arg",
                "using=id, amount",
            ],
        },
        Case {
            long_name: "append-rows",
            on: Some("orders"),
            reads: json!({ "other": ["returns"] }),
            name: "everything",
            arguments: json!({}),
            description: None,
            flags: &["--read", "other=returns"],
        },
        Case {
            long_name: "read-file",
            on: None,
            reads: json!({}),
            name: "outside",
            arguments: json!({ "path": "/tmp/elsewhere.csv" }),
            description: None,
            flags: &["--arg", "path=/tmp/elsewhere.csv"],
        },
    ]
}

#[test]
fn the_model_is_the_file_arc_operation_record_writes_with_its_notes() {
    let mut noted = false;
    for case in cases() {
        let protocol = Protocol::new();
        let model = case
            .model()
            .unwrap_or_else(|e| panic!("{}: {e}", case.long_name));
        let out = case.record(&protocol);
        assert!(
            out.status.success(),
            "{}: arc refused:\n{}",
            case.long_name,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            model.model,
            protocol.model_of(case.name),
            "{}: the model is not what arc recorded",
            case.long_name
        );
        assert_eq!(model.notes, notes(&out), "{}: the notes", case.long_name);
        noted |= !model.notes.is_empty();
    }
    assert!(noted, "no case carried a note, so the notes went unchecked");
}

#[test]
fn what_arc_operation_record_refuses_before_the_protocol_is_refused_in_its_words() {
    let base = || Case {
        long_name: "filter-rows",
        on: Some("orders"),
        reads: json!({}),
        name: "big_orders",
        arguments: json!({ "where": "amount > 100" }),
        description: None,
        flags: &["--arg", "where=amount > 100"],
    };
    let refused = [
        Case {
            long_name: "filter-everything",
            ..base()
        },
        Case {
            arguments: json!({}),
            flags: &[],
            ..base()
        },
        Case {
            arguments: json!({ "where": "amount > 100; DROP TABLE orders" }),
            flags: &["--arg", "where=amount > 100; DROP TABLE orders"],
            ..base()
        },
        Case {
            name: "big:orders",
            ..base()
        },
        Case {
            name: "-big",
            ..base()
        },
        Case {
            description: Some(" "),
            ..base()
        },
        Case {
            long_name: "join-tables",
            on: None,
            reads: json!({ "table": "orders", "other": "orders" }),
            arguments: json!({ "using": ["id"] }),
            flags: &[
                "--read",
                "table=orders",
                "--read",
                "other=orders",
                "--arg",
                "using=id",
            ],
            ..base()
        },
    ];
    for case in refused {
        let protocol = Protocol::new();
        let Err(error) = case.model() else {
            panic!("{} {:?}: not refused", case.long_name, case.name);
        };
        assert_eq!(
            error.to_string(),
            refusal(&case.record(&protocol)),
            "{} {:?}",
            case.long_name,
            case.name
        );
    }
}

#[test]
fn a_table_no_step_makes_and_a_name_a_step_has_are_left_to_the_record_path() {
    let protocol = Protocol::new();
    for case in [
        Case {
            long_name: "filter-rows",
            on: Some("nowhere"),
            reads: json!({}),
            name: "kept",
            arguments: json!({ "where": "true" }),
            description: None,
            flags: &["--arg", "where=true"],
        },
        Case {
            long_name: "filter-rows",
            on: Some("orders"),
            reads: json!({}),
            name: "load_orders",
            arguments: json!({ "where": "true" }),
            description: None,
            flags: &["--arg", "where=true"],
        },
    ] {
        assert!(case.model().is_ok(), "{:?} {:?}", case.on, case.name);
        refusal(&case.record(&protocol));
    }
    // Neither refusal recorded anything.
    assert_eq!(
        std::fs::read_to_string(protocol.dir.join("arcform.yaml")).unwrap(),
        MANIFEST
    );
}
