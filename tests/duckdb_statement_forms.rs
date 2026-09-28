//! DuckDB statement forms arc's SQL introspection now understands: `INSTALL …
//! FROM <repository>` and `FORCE INSTALL`, an empty-string `COPY` option
//! (`QUOTE ''`, `ESCAPE ''`, `DELIMITER ''`), a `PRAGMA` call with several
//! positional and/or named arguments, and `SET VARIABLE <name> = <expr>`.
//!
//! Before the parser fork learned these (see
//! `vendor/sqlparser-0.55.0/MERIDIAN_PATCH.md`, additions 4-7), a step opening
//! with any of them printed `could not parse … — treating as opaque step` on
//! stderr, and the tables it produced or read never reached the asset graph —
//! so a downstream step could run against a table arc still believed was
//! fresh after an upstream rebuild changed it. This file pins both the
//! negative (no opaque warning) and the positive (the assets show up, wired,
//! in the run's own contract) outcome against a real `arc run`, plus a
//! PostgreSQL-dialect check that the new grammars stay DuckDB/Generic-only,
//! and a check that every form this change does not touch keeps parsing.

use std::path::PathBuf;
use std::process::Command;

use sqlparser::ast::{CopyOption, InstallRepository, Statement, Value};
use sqlparser::dialect::{DuckDbDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

/// One project's `arc run`: its directory (kept alive via `_workspace`),
/// captured stdout/stderr, whether the process exited zero, and its parsed
/// run contract.
struct Run {
    _workspace: tempfile::TempDir,
    project: PathBuf,
    stdout: String,
    stderr: String,
    succeeded: bool,
    contract: serde_json::Value,
}

impl Run {
    /// The asset named `name` in this run's contract, or a panic naming what
    /// was actually discovered — never a missing-key panic with no context.
    fn asset(&self, name: &str) -> &serde_json::Value {
        self.contract["assets"]
            .as_array()
            .expect("assets array")
            .iter()
            .find(|a| a["name"] == name)
            .unwrap_or_else(|| panic!("asset {name:?} not discovered:\n{}", self.contract))
    }

    /// The negative outcome each case below guards against: an unparsed
    /// leading statement degrades the whole file to one opaque step.
    fn assert_not_opaque(&self) {
        assert!(
            !self.stderr.contains("could not parse")
                && !self.stderr.contains("treating as opaque step"),
            "must not degrade to an opaque step:\nstderr:\n{}",
            self.stderr
        );
    }

    fn assert_succeeded(&self) {
        assert!(
            self.succeeded,
            "arc run should have succeeded:\nstdout:\n{}\nstderr:\n{}",
            self.stdout, self.stderr
        );
    }
}

/// Writes a two-step project — `load_source` runs `load_source_sql`,
/// `transform` runs `transform_sql` — and returns its `arc run` result.
fn run_project(label: &str, load_source_sql: &str, transform_sql: &str) -> Run {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    let project = workspace.path().join(label);
    std::fs::create_dir_all(project.join("models")).unwrap();
    std::fs::write(
        project.join("arcform.yaml"),
        "name: statement_forms\n\
         engine: duckdb\n\
         db: statement_forms.duckdb\n\
         steps:\n\
         \x20\x20- name: load_source\n\
         \x20\x20\x20\x20sql: models/load_source.sql\n\
         \x20\x20- name: transform\n\
         \x20\x20\x20\x20sql: models/transform.sql\n",
    )
    .unwrap();
    std::fs::write(project.join("models/load_source.sql"), load_source_sql).unwrap();
    std::fs::write(project.join("models/transform.sql"), transform_sql).unwrap();

    let output = Command::new(arc)
        .current_dir(&project)
        .arg("run")
        .output()
        .unwrap_or_else(|e| panic!("[{label}] spawn arc run: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let contract = read_contract(&project);

    Run {
        project,
        _workspace: workspace,
        stdout,
        stderr,
        succeeded: output.status.success(),
        contract,
    }
}

/// The shape most cases below need: `load_source` creates table `a`,
/// `transform` runs `transform_sql`.
fn run_transform(label: &str, transform_sql: &str) -> Run {
    run_project(
        label,
        "CREATE TABLE a AS SELECT * FROM (VALUES (1), (2), (3)) AS t(x);\n",
        transform_sql,
    )
}

/// Read the single run contract under `<project>/build/.arcform/runs/*.json`.
fn read_contract(project: &std::path::Path) -> serde_json::Value {
    let runs = project.join("build/.arcform/runs");
    let mut json_files: Vec<PathBuf> = std::fs::read_dir(&runs)
        .unwrap_or_else(|e| panic!("runs dir {}: {e}", runs.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        json_files.len(),
        1,
        "expected exactly one contract, got {json_files:?}"
    );
    let bytes = std::fs::read(json_files.remove(0)).unwrap();
    serde_json::from_slice(&bytes).expect("contract is valid JSON")
}

/// `first_stmt` followed by `CREATE TABLE b AS SELECT * FROM a;`, asserting
/// `first_stmt` parsed (no opaque warning) and `b`/`a` are wired in the run's
/// own contract — regardless of whether `first_stmt` itself succeeds at
/// runtime, because arc discovers lineage from the parsed SQL, never from what
/// the engine does with it.
fn assert_wired(label: &str, first_stmt: &str) -> Run {
    let run = run_transform(
        label,
        &format!("{first_stmt}\nCREATE TABLE b AS SELECT * FROM a;\n"),
    );
    run.assert_not_opaque();
    let a = run.asset("a");
    let b = run.asset("b");
    assert_eq!(a["produced_by"], "load_source", "[{label}]");
    assert_eq!(b["produced_by"], "transform", "[{label}]");
    let consumed_by: Vec<String> = a["consumed_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        consumed_by.contains(&"transform".to_string()),
        "[{label}] transform should consume 'a', got {consumed_by:?}"
    );
    run
}

// ---- `INSTALL … FROM <repository>` and `FORCE INSTALL` ----

#[test]
fn install_from_community() {
    assert_wired("install_from_community", "INSTALL mlpack FROM community;");
}

#[test]
fn install_from_core() {
    assert_wired("install_from_core", "INSTALL httpfs FROM core;");
}

/// `arc run` refuses a Protocol that installs from an address before a step runs, and
/// `tests/vetted_extensions.rs` holds that refusal. What this holds is that arc parses the
/// statement, address and all, rather than reading the step as opaque.
#[test]
fn install_from_a_url_that_does_not_resolve() {
    match parse_one("INSTALL x FROM 'https://example.org/ext';") {
        Statement::Install {
            extension_name,
            force,
            repository,
        } => {
            assert_eq!(extension_name.value, "x");
            assert!(!force, "INSTALL … FROM '<url>' is not FORCE");
            assert_eq!(
                repository,
                Some(InstallRepository::Url("https://example.org/ext".into())),
                "the address is the repository"
            );
        }
        other => panic!("expected INSTALL, got {other:?}"),
    }
}

#[test]
fn force_install_from_community() {
    assert_wired(
        "force_install_from_community",
        "FORCE INSTALL mlpack FROM community;",
    );
}

#[test]
fn force_install_bare() {
    assert_wired("force_install_bare", "FORCE INSTALL spatial;");
}

// ---- an empty-string COPY option disables it instead of refusing to parse ----

#[test]
fn copy_with_empty_quote_writes_the_file() {
    let run = assert_wired(
        "copy_with_empty_quote",
        "COPY a TO 'out.csv' (HEADER false, QUOTE '');",
    );
    run.assert_succeeded();
    assert!(
        run.project.join("out.csv").is_file(),
        "COPY should have written out.csv"
    );
}

#[test]
fn copy_with_empty_escape_writes_the_file() {
    let run = assert_wired(
        "copy_with_empty_escape",
        "COPY a TO 'out.csv' (HEADER false, ESCAPE '');",
    );
    run.assert_succeeded();
    assert!(
        run.project.join("out.csv").is_file(),
        "COPY should have written out.csv"
    );
}

#[test]
fn copy_with_empty_delimiter_writes_the_file() {
    let run = assert_wired(
        "copy_with_empty_delimiter",
        "COPY a TO 'out.csv' (HEADER false, DELIMITER '');",
    );
    run.assert_succeeded();
    assert!(
        run.project.join("out.csv").is_file(),
        "COPY should have written out.csv"
    );
}

// ---- PRAGMA as a function call — several positional args, and a named one ----

fn assert_pragma_fts_wired(label: &str, trailing_args: &str) {
    let load_source = "CREATE TABLE a AS SELECT * FROM (VALUES (1), (2), (3)) AS t(x);\n\
                        CREATE TABLE docs (order_id INTEGER, body VARCHAR);\n\
                        INSERT INTO docs VALUES (1, 'hello world'), (2, 'goodbye world');\n";
    let transform = format!(
        "PRAGMA create_fts_index('docs', 'order_id', 'body'{trailing_args});\n\
         CREATE TABLE b AS SELECT * FROM a;\n"
    );
    let run = run_project(label, load_source, &transform);
    run.assert_not_opaque();
    run.assert_succeeded();
    assert_eq!(run.asset("b")["produced_by"], "transform", "[{label}]");
}

#[test]
fn pragma_with_three_positional_args() {
    assert_pragma_fts_wired("pragma_with_three_positional_args", "");
}

#[test]
fn pragma_with_a_named_arg() {
    assert_pragma_fts_wired("pragma_with_a_named_arg", ", overwrite = 1");
}

// ---- `SET VARIABLE <name> = <expr>` ----

#[test]
fn set_variable_with_a_typed_value() {
    assert_wired(
        "set_variable_with_a_typed_value",
        "SET VARIABLE cutoff = DATE '2026-01-01';",
    );
}

// ---- a dialect that refused these forms before still refuses them.
// Checked directly against the grammar, since arc's own introspection always
// parses with `DuckDbDialect` and never reaches PostgreSQL's. ----

#[test]
fn postgres_still_refuses_the_new_forms() {
    let cases = [
        ("INSTALL … FROM", "INSTALL mlpack FROM community;"),
        ("FORCE INSTALL", "FORCE INSTALL spatial;"),
        (
            "empty QUOTE",
            "COPY a TO 'out.csv' (HEADER false, QUOTE '');",
        ),
        (
            "PRAGMA, three positional args",
            "PRAGMA create_fts_index('docs', 'order_id', 'body');",
        ),
        (
            "PRAGMA, named arg",
            "PRAGMA create_fts_index('docs', 'order_id', 'body', overwrite = 1);",
        ),
        ("SET VARIABLE", "SET VARIABLE cutoff = 1;"),
    ];
    for (label, sql) in cases {
        assert!(
            Parser::parse_sql(&PostgreSqlDialect {}, sql).is_err(),
            "[{label}] PostgreSQL dialect should still refuse this form: {sql}"
        );
    }
}

// ---- the forms this change does not touch keep parsing exactly as they
// did on f9a5f44. Arc can only observe "parsed, no opaque warning", so each
// case also pins the syntax tree the form lands in — a form that still parsed
// but had migrated into one of the new fields would be invisible to arc and is
// exactly what a backward-compatible extension must not do. ----

/// Parses `sql` with the DuckDB dialect and returns its only statement.
fn parse_one(sql: &str) -> Statement {
    let mut stmts = Parser::parse_sql(&DuckDbDialect {}, sql)
        .unwrap_or_else(|e| panic!("{sql} should parse: {e}"));
    assert_eq!(stmts.len(), 1, "{sql} should be one statement: {stmts:?}");
    stmts.remove(0)
}

#[test]
fn bare_install_unaffected() {
    match parse_one("INSTALL spatial;") {
        Statement::Install {
            extension_name,
            force,
            repository,
        } => {
            assert_eq!(extension_name.value, "spatial");
            assert!(!force, "a bare INSTALL is not FORCE");
            assert_eq!(repository, None, "a bare INSTALL names no repository");
        }
        other => panic!("expected INSTALL, got {other:?}"),
    }
    assert_wired("bare_install_unaffected", "INSTALL spatial;");
}

#[test]
fn copy_with_a_one_character_quote_unaffected() {
    match parse_one("COPY a TO 'out.csv' (HEADER false, QUOTE '\"');") {
        Statement::Copy { options, .. } => assert!(
            options.contains(&CopyOption::Quote(Some('"'))),
            "a one-character QUOTE keeps its character: {options:?}"
        ),
        other => panic!("expected COPY, got {other:?}"),
    }
    let run = assert_wired(
        "copy_with_a_one_character_quote_unaffected",
        "COPY a TO 'out.csv' (HEADER false, QUOTE '\"');",
    );
    run.assert_succeeded();
    assert!(run.project.join("out.csv").is_file());
}

#[test]
fn pragma_single_value_call_unaffected() {
    match parse_one("PRAGMA table_info('orders');") {
        Statement::Pragma {
            value, is_eq, args, ..
        } => {
            assert_eq!(value, Some(Value::SingleQuotedString("orders".into())));
            assert!(!is_eq);
            assert!(
                args.is_empty(),
                "a one-argument PRAGMA stays on `value`, not `args`: {args:?}"
            );
        }
        other => panic!("expected PRAGMA, got {other:?}"),
    }
    assert_eq!(
        parse_one("PRAGMA table_info('orders');").to_string(),
        "PRAGMA table_info('orders')"
    );
}

#[test]
fn pragma_eq_form_unaffected() {
    match parse_one("PRAGMA threads = 4;") {
        Statement::Pragma {
            value, is_eq, args, ..
        } => {
            assert_eq!(value, Some(Value::Number("4".into(), false)));
            assert!(is_eq);
            assert!(
                args.is_empty(),
                "the `= value` form has no `args`: {args:?}"
            );
        }
        other => panic!("expected PRAGMA, got {other:?}"),
    }
    let run = assert_wired("pragma_eq_form_unaffected", "PRAGMA threads = 4;");
    run.assert_succeeded();
}
