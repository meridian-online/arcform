//! End-to-end: `arc run` takes what each SQL step reads and produces from DuckDB's own
//! parse of it, through the real `arc` binary; `ARC_SQL_READER=sqlparser` makes it take
//! them from arc's own parser; a statement DuckDB's reading cannot take is read by arc's
//! own, and one neither can read draws a warning naming the step; and a command that reads
//! the graph with no DuckDB 2 to ask reads it with arc's own parser and says so once.
//!
//! The runs ask the DuckDB 2.0 command line `ARC_DUCKDB_RECORD_BIN` names when it is set,
//! and the `duckdb` on PATH otherwise, as `ci.yml`'s build job and any `arc run` do.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
mod common;
use common::strip_ansi;

const RECORD_ENV: &str = "ARC_DUCKDB_RECORD_BIN";

/// Which reader a run is told to use, and so which DuckDB it runs.
enum Reader {
    /// `ARC_SQL_READER` unset, on [`duckdb`].
    Default,
    /// `ARC_SQL_READER=sqlparser`, on [`duckdb`].
    Sqlparser,
    /// `ARC_SQL_READER` set to `value`, on the `duckdb` found on PATH.
    Told(&'static str),
    /// `ARC_SQL_READER` unset, on the executable at this path.
    Program(PathBuf),
}

/// The DuckDB 2.0 build `ARC_DUCKDB_RECORD_BIN` names, when it is set; else the runs use
/// the `duckdb` on PATH.
fn duckdb() -> Option<OsString> {
    std::env::var_os(RECORD_ENV)
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn printed(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }

    /// How many warnings named an unread table function call.
    fn unread_warnings(&self) -> usize {
        self.stderr.matches("calls the table function").count()
    }
}

/// A project whose steps run in the order given, each a `sql:` step over `<name>.sql`,
/// on one database file.
fn project(steps: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = String::from("name: reader\nengine: duckdb\ndb: reader.duckdb\nsteps:\n");
    for (name, sql) in steps {
        manifest.push_str(&format!("  - name: {name}\n    sql: {name}.sql\n"));
        std::fs::write(dir.path().join(format!("{name}.sql")), sql).unwrap();
    }
    std::fs::write(dir.path().join("arcform.yaml"), manifest).unwrap();
    dir
}

/// One `arc run` of `project` with `reader`. The variables this file sets are cleared
/// first, so the environment the tests run in does not choose for them.
fn arc_run(project: &Path, reader: Reader) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arc"));
    command.current_dir(project).arg("run");
    for var in [
        "ARC_SQL_READER",
        "ARC_DUCKDB_BIN",
        "ARC_ALLOW_UNTESTED_ENGINE",
    ] {
        command.env_remove(var);
    }
    match reader {
        Reader::Default | Reader::Sqlparser => {
            if let Some(bin) = duckdb() {
                command.env("ARC_DUCKDB_BIN", bin);
            }
            if matches!(reader, Reader::Sqlparser) {
                command.env("ARC_SQL_READER", "sqlparser");
            }
        }
        Reader::Told(value) => {
            command.env("ARC_SQL_READER", value);
        }
        Reader::Program(path) => {
            command.env("ARC_DUCKDB_BIN", path);
        }
    }
    let out = command.output().expect("spawn arc run");
    Run {
        code: out.status.code(),
        stdout: strip_ansi(&String::from_utf8_lossy(&out.stdout)),
        stderr: strip_ansi(&String::from_utf8_lossy(&out.stderr)),
    }
}

/// The single run contract under `<project>/build/.arcform/runs/`.
fn read_contract(project: &Path) -> serde_json::Value {
    let runs = project.join("build/.arcform/runs");
    let json_files: Vec<PathBuf> = std::fs::read_dir(&runs)
        .unwrap_or_else(|e| panic!("runs dir {}: {e}", runs.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        json_files.len(),
        1,
        "expected exactly one contract, got {json_files:?}"
    );
    serde_json::from_slice(&std::fs::read(&json_files[0]).unwrap()).expect("contract is valid JSON")
}

/// Each statement of `step` in the contract, as (produces, reads).
fn statements(contract: &serde_json::Value, step: &str) -> Vec<(Vec<String>, Vec<String>)> {
    let names = |list: &serde_json::Value| -> Vec<String> {
        list.as_array()
            .expect("a list of names")
            .iter()
            .map(|v| v.as_str().expect("a name").to_string())
            .collect()
    };
    contract["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|s| s["name"] == step)
        .unwrap_or_else(|| panic!("no step {step:?} in the contract"))["sql"]["statements"]
        .as_array()
        .expect("statements array")
        .iter()
        .map(|s| (names(&s["produces"]), names(&s["reads"])))
        .collect()
}

/// The contract's assets, each as its name, kind, producer and consumers, sorted.
fn assets(contract: &serde_json::Value) -> Vec<String> {
    let mut assets: Vec<String> = contract["assets"]
        .as_array()
        .expect("assets array")
        .iter()
        .map(|a| {
            format!(
                "{} [{}] produced_by={} consumed_by={}",
                a["name"], a["kind"], a["produced_by"], a["consumed_by"]
            )
        })
        .collect();
    assets.sort();
    assets
}

/// The printed asset graph's entry for `asset`: its header line and the lines under it,
/// each with its whitespace collapsed to single spaces.
fn graph_entry(stdout: &str, asset: &str) -> Vec<String> {
    let header = format!("  {asset} [");
    let mut lines = stdout.lines().skip_while(|l| !l.starts_with(&header));
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("the printed graph has no entry for {asset:?}:\n{stdout}"));
    let mut entry = vec![squash(first)];
    entry.extend(lines.take_while(|l| l.starts_with("      ")).map(squash));
    entry
}

fn squash(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Creates `t` and `u` for the steps below to read.
const LOAD: &str = "CREATE TABLE t AS SELECT * FROM (VALUES (1, 10, 'p', 2)) AS v(a, k, c, v);\n\
                    CREATE TABLE u AS SELECT * FROM (VALUES (5, 'x')) AS v(k, b);\n";

/// A step arc's own parser cannot parse and DuckDB's can.
const ASOF: &str = "CREATE TABLE r AS SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k);\n";

/// A step both parse and read differently: arc's own parser reads no table inside an
/// `EXISTS`.
const EXISTS: &str = "CREATE TABLE e AS SELECT * FROM t WHERE EXISTS (SELECT 1 FROM u);\n";

/// The warning lines of a run's standard error.
fn warnings(run: &Run) -> Vec<&str> {
    run.stderr
        .lines()
        .filter(|l| l.starts_with("warning:"))
        .collect()
}

/// The warning a command prints when no DuckDB 2 answered for the graph.
const NO_DUCKDB: &str = "because no DuckDB 2 answered";

#[test]
fn with_the_reader_unset_a_step_is_read_from_duckdbs_parse() {
    let dir = project(&[("load", LOAD), ("asof", ASOF)]);
    let run = arc_run(dir.path(), Reader::Default);
    assert_eq!(
        run.code,
        Some(0),
        "arc run must exit 0 on DuckDB 2.0:\n{}",
        run.printed()
    );
    assert_eq!(warnings(&run), [] as [&str; 0], "{}", run.printed());

    let r = graph_entry(&run.stdout, "r");
    assert!(
        r.contains(&"produced by asof".to_string()),
        "the printed graph does not show asof producing r: {r:?}"
    );
    for read in ["t", "u"] {
        let entry = graph_entry(&run.stdout, read);
        assert!(
            entry.contains(&"feeds asof".to_string()),
            "the printed graph does not show {read} feeding asof: {entry:?}"
        );
    }
    assert_eq!(
        statements(&read_contract(dir.path()), "asof"),
        [(
            vec!["r".to_string()],
            vec!["t".to_string(), "u".to_string()]
        )],
        "the run contract's one statement of asof"
    );
}

#[test]
fn sqlparser_forces_arcs_own_reading() {
    let dir = project(&[("load", LOAD), ("exists", EXISTS), ("asof", ASOF)]);
    let by_duckdb = arc_run(dir.path(), Reader::Default);
    assert_eq!(by_duckdb.code, Some(0), "{}", by_duckdb.printed());
    assert_eq!(
        statements(&read_contract(dir.path()), "exists"),
        [(
            vec!["e".to_string()],
            vec!["t".to_string(), "u".to_string()]
        )],
        "DuckDB's parse reads the table inside the EXISTS"
    );

    let dir = project(&[("load", LOAD), ("exists", EXISTS), ("asof", ASOF)]);
    let run = arc_run(dir.path(), Reader::Sqlparser);
    assert_eq!(run.code, Some(0), "arc run must exit 0:\n{}", run.printed());
    let contract = read_contract(dir.path());
    assert_eq!(
        statements(&contract, "exists"),
        [(vec!["e".to_string()], vec!["t".to_string()])],
        "arc's own parser reads no table inside an EXISTS"
    );
    assert_eq!(
        statements(&contract, "asof"),
        [(Vec::new(), Vec::new())],
        "arc's own parser cannot parse the ASOF join, which reads nothing"
    );
    assert_eq!(
        warnings(&run),
        [
            "warning: could not read statement 1 of asof (asof.sql:1) \u{2014} treating it as \
          reading nothing"
        ],
        "the statement arc's own parser cannot read is named"
    );
}

#[test]
fn an_unread_pivot_join_draws_a_warning_naming_the_step() {
    let dir = project(&[
        ("load", LOAD),
        (
            "pivot",
            "-- a pivot of two tables\nCREATE TABLE r AS PIVOT t JOIN u USING (k) ON c USING sum(v);\n",
        ),
    ]);
    let run = arc_run(dir.path(), Reader::Default);
    assert_eq!(run.code, Some(0), "arc run must exit 0:\n{}", run.printed());
    assert_eq!(
        warnings(&run),
        [
            "warning: could not read statement 1 of pivot (pivot.sql:2) \u{2014} treating it as \
          reading nothing"
        ],
        "DuckDB gives the PIVOT no tree and arc's own parser cannot parse it"
    );
    assert_eq!(
        statements(&read_contract(dir.path()), "pivot"),
        [(vec!["r".to_string()], Vec::new())],
        "the statement keeps the table its words make certain, and reads nothing"
    );
}

#[test]
fn a_bare_name_statement_reads_the_table() {
    let dir = project(&[("load", LOAD), ("peek", "t;\n")]);
    let run = arc_run(dir.path(), Reader::Default);
    assert_eq!(run.code, Some(0), "arc run must exit 0:\n{}", run.printed());
    assert_eq!(warnings(&run), [] as [&str; 0], "{}", run.printed());
    assert_eq!(
        statements(&read_contract(dir.path()), "peek"),
        [(Vec::new(), vec!["t".to_string()])],
        "DuckDB 2.0's command line runs `t;` as `SELECT * FROM t`"
    );
    assert!(
        graph_entry(&run.stdout, "t").contains(&"feeds peek".to_string()),
        "the printed graph shows t feeding peek:\n{}",
        run.stdout
    );
}

/// `arc sql record` reads the Protocol's graph to find the step that makes the table a
/// filter is recorded on. With no `duckdb` on PATH it reads the graph with arc's own parser,
/// and says so once, however many SQL steps there are.
#[test]
fn arc_sql_record_without_duckdb_on_path_reads_with_sqlparser_and_warns_once() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("shop");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("arcform.yaml"),
        "name: shop\nengine: duckdb\ndb: shop.duckdb\nsteps:\n  \
         - name: load_orders\n    sql: orders.sql\n  \
         - name: load_customers\n    sql: customers.sql\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("orders.sql"),
        "CREATE TABLE orders AS SELECT * FROM (VALUES (1, 50)) AS t(id, amount);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("customers.sql"),
        "CREATE TABLE customers AS SELECT * FROM (VALUES (1)) AS t(id);\n",
    )
    .unwrap();
    let empty = root.path().join("bin");
    std::fs::create_dir_all(&empty).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&dir)
        .env("PATH", &empty)
        .env("ARCFORM_HISTORY_DIR", root.path().join("history"))
        .env_remove("ARC_SQL_READER")
        .env_remove("ARC_DUCKDB_BIN")
        .args([
            "sql",
            "record",
            "SELECT * FROM orders WHERE amount > 10",
            "--name",
            "big_orders",
        ])
        .output()
        .expect("spawn arc");
    let stderr = strip_ansi(&String::from_utf8_lossy(&out.stderr));
    assert!(
        out.status.success(),
        "arc sql record must succeed: {stderr}"
    );
    assert_eq!(
        stderr.matches(NO_DUCKDB).count(),
        1,
        "one warning says the graph is arc's own reading: {stderr}"
    );
    assert!(
        stderr.contains(
            "warning: the asset graph is arc's own reading of the SQL, because no DuckDB 2 \
             answered (could not ask DuckDB `duckdb` for its parse:"
        ),
        "the warning says why: {stderr}"
    );
    assert!(
        std::fs::read_to_string(dir.join("arcform.yaml"))
            .unwrap()
            .contains("big_orders"),
        "the step was recorded on the table arc's own parser found"
    );
}

/// A stand-in `duckdb` that reports `version` and answers nothing else, so the test needs
/// no particular DuckDB on PATH.
fn fake_duckdb(dir: &Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("fake-duckdb");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{version} (fake) 0000000000'; fi\nexit 0\n"),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn arc_run_without_a_duckdb_2_is_refused_before_a_step_runs() {
    // A 1.x command line is refused by its version; a 2.0 one that answers nothing, by the
    // first question arc asks of it, for the step's tokens.
    for (version, says) in [
        ("v1.5.6", vec!["DuckDB 1.5.6", ">=2.0, <3"]),
        ("v2.0.0", vec!["could not read the tokens", "no version"]),
    ] {
        let engines = tempfile::tempdir().unwrap();
        let bin = fake_duckdb(engines.path(), version);
        let dir = project(&[("load", LOAD)]);
        let run = arc_run(dir.path(), Reader::Program(bin));
        assert!(
            run.code.is_some_and(|code| code != 0),
            "{version}: arc run must refuse:\n{}",
            run.printed()
        );
        for word in says {
            assert!(
                run.stderr.contains(word),
                "{version}: the refusal must name {word:?}:\n{}",
                run.stderr
            );
        }
        assert!(
            !dir.path().join("reader.duckdb").exists() && !run.stdout.contains("] load"),
            "{version}: a step ran:\n{}",
            run.printed()
        );
    }
}

#[test]
fn table_functions_are_read_alike_by_either_reader() {
    let steps = [
        (
            "xlsx",
            "CREATE TABLE b AS SELECT * FROM read_xlsx('build/budget.xlsx');\n",
        ),
        ("range", "CREATE TABLE g AS SELECT * FROM range(10);\n"),
        (
            "mlpack",
            "CREATE TABLE m AS SELECT * FROM \
             mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\");\n",
        ),
    ];
    for (step, sql) in steps {
        let mut read = Vec::new();
        for reader in [Reader::Sqlparser, Reader::Default] {
            let dir = project(&[(step, sql)]);
            let run = arc_run(dir.path(), reader);
            let contract = read_contract(dir.path());
            read.push((
                statements(&contract, step),
                assets(&contract),
                run.unread_warnings(),
            ));
        }
        assert_eq!(
            read[1], read[0],
            "{step}: DuckDB's parse (right) against arc's own (left)"
        );
        let (statements, _, warnings) = &read[1];
        match step {
            "xlsx" => assert_eq!(
                statements[0].1,
                ["build/budget.xlsx"],
                "read_xlsx records its file"
            ),
            "range" => assert_eq!(statements[0].1, [] as [&str; 0], "range reads no table"),
            _ => assert_eq!(
                *warnings, 1,
                "one warning asks for depends_on: and produces:"
            ),
        }
    }
}

#[test]
fn a_copy_from_a_query_records_its_file_and_the_querys_table() {
    let dir = project(&[
        (
            "load",
            "CREATE TABLE a AS SELECT * FROM (VALUES (1), (2)) AS v(x);\n",
        ),
        (
            "export",
            "COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER);\n",
        ),
    ]);
    let run = arc_run(dir.path(), Reader::Default);
    assert_eq!(run.code, Some(0), "arc run must exit 0:\n{}", run.printed());
    assert!(
        dir.path().join("out.csv").is_file(),
        "the COPY wrote out.csv"
    );
    assert_eq!(
        statements(&read_contract(dir.path()), "export"),
        [(vec!["out.csv".to_string()], vec!["a".to_string()])],
        "the run contract's one statement of export"
    );
    let file = graph_entry(&run.stdout, "out.csv");
    assert_eq!(file[0], "out.csv [file]", "printed entry: {file:?}");
    assert!(
        file.contains(&"produced by export".to_string()),
        "the printed graph does not show export producing out.csv: {file:?}"
    );
}

#[test]
fn a_reader_arc_does_not_have_is_refused_naming_the_ones_it_has() {
    for value in ["DuckDB", "sqlparser-rs", ""] {
        let dir = project(&[("load", LOAD)]);
        let run = arc_run(dir.path(), Reader::Told(value));
        assert_eq!(
            run.code,
            Some(1),
            "ARC_SQL_READER={value:?}: arc run must refuse:\n{}",
            run.printed()
        );
        assert!(
            run.stderr.contains(&format!("ARC_SQL_READER is '{value}'"))
                && run.stderr.contains("`duckdb`")
                && run.stderr.contains("`sqlparser`"),
            "ARC_SQL_READER={value:?}: the refusal must name the value and the two it takes:\n{}",
            run.stderr
        );
        assert!(
            !dir.path().join("reader.duckdb").exists(),
            "ARC_SQL_READER={value:?}: a step ran"
        );
    }
}

#[test]
fn a_pivot_joined_to_another_table_is_not_read_as_reading_its_first() {
    let dir = project(&[
        ("load", LOAD),
        ("pivot", "PIVOT t JOIN u USING (k) ON c USING sum(v);\n"),
    ]);
    let run = arc_run(dir.path(), Reader::Default);
    assert_eq!(run.code, Some(0), "arc run must exit 0:\n{}", run.printed());
    let pivot = statements(&read_contract(dir.path()), "pivot");
    assert_eq!(pivot.len(), 1, "one statement: {pivot:?}");
    assert_ne!(
        pivot[0].1,
        ["t"],
        "the pivot reads the rows of t and u, and the contract holds t as the one it reads"
    );
}
