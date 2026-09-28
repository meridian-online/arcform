//! End-to-end: `ARC_SQL_READER=duckdb` makes `arc run` take what each SQL step reads and
//! produces from DuckDB's own parse of it, through the real `arc` binary, and leaves every
//! other run as it was.
//!
//! The tests that read a step with DuckDB's parse need a DuckDB 2.0 build, named by
//! `ARC_DUCKDB_PREVIEW_BIN`, and are `#[ignore]`d so an ordinary run skips them; `ci.yml`
//! downloads the 2.0 preview and runs them with `--include-ignored`. A 2.0 build is outside
//! the versions arc is tested on, so those runs set `ARC_ALLOW_UNTESTED_ENGINE` as well.
//! The rest need the `duckdb` on PATH, a 1.x release, as any `arc run` does.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
mod common;
use common::strip_ansi;

const PREVIEW_ENV: &str = "ARC_DUCKDB_PREVIEW_BIN";

/// Which reader a run is told to use, and so which DuckDB it runs.
enum Reader {
    /// `ARC_SQL_READER` unset, on the `duckdb` found on PATH.
    Default,
    /// `ARC_SQL_READER=duckdb`, on the 2.0 build `ARC_DUCKDB_PREVIEW_BIN` names.
    DuckDbPreview,
    /// `ARC_SQL_READER` set to `value`, on the `duckdb` found on PATH.
    Told(&'static str),
}

fn preview() -> OsString {
    std::env::var_os(PREVIEW_ENV).unwrap_or_else(|| {
        panic!(
            "{PREVIEW_ENV} must name a DuckDB 2.0 build: this test is #[ignore]d so an \
             ordinary run skips it, and running it with --include-ignored says one is staged"
        )
    })
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
        Reader::Default => {}
        Reader::DuckDbPreview => {
            command
                .env("ARC_SQL_READER", "duckdb")
                .env("ARC_DUCKDB_BIN", preview())
                .env("ARC_ALLOW_UNTESTED_ENGINE", "1");
        }
        Reader::Told(value) => {
            command.env("ARC_SQL_READER", value);
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

#[test]
#[ignore = "needs the DuckDB 2.0 build ARC_DUCKDB_PREVIEW_BIN names; ci.yml's build job \
            downloads the preview and passes --include-ignored"]
fn with_the_reader_set_a_step_is_read_from_duckdbs_parse() {
    let dir = project(&[("load", LOAD), ("asof", ASOF)]);
    let run = arc_run(dir.path(), Reader::DuckDbPreview);
    assert_eq!(
        run.code,
        Some(0),
        "arc run must exit 0 on the preview:\n{}",
        run.printed()
    );
    assert!(
        !run.printed().contains("could not parse"),
        "the step was not read:\n{}",
        run.printed()
    );

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

/// What `arc run` printed for the Protocol above, with `ARC_SQL_READER` unset, on the
/// commit this change is based on, `6550d3c`, on DuckDB v1.5.5: its standard output, and
/// the warnings on its standard error. Neither names the version.
const PRINTED_BEFORE: &str = "[1/2] load ...
[2/2] asof ...

Asset graph (2 nodes):
  t [table, 1 row]
      produced by  load
  u [table, 1 row]
      produced by  load

\u{2713} 2/2 steps succeeded.
";
const WARNED_BEFORE: &[&str] = &["warning: could not parse asof.sql: sql parser error: \
     Expected: JOIN, found: LEFT at Line: 1, Column: 52 \u{2014} treating as opaque step"];

#[test]
fn without_the_reader_set_a_step_is_read_as_before() {
    let dir = project(&[("load", LOAD), ("asof", ASOF)]);
    let run = arc_run(dir.path(), Reader::Default);
    assert_eq!(run.code, Some(0), "arc run must exit 0:\n{}", run.printed());
    assert_eq!(
        run.stdout, PRINTED_BEFORE,
        "the standard output differs from what arc printed before"
    );
    let warnings: Vec<&str> = run
        .stderr
        .lines()
        .filter(|l| l.starts_with("warning:"))
        .collect();
    assert_eq!(
        warnings, WARNED_BEFORE,
        "the warnings differ from what arc printed before"
    );
    assert_eq!(
        statements(&read_contract(dir.path()), "asof"),
        [] as [(Vec<String>, Vec<String>); 0],
        "the run contract holds no statement of a step arc's own parser could not parse"
    );
}

#[test]
#[ignore = "needs the DuckDB 2.0 build ARC_DUCKDB_PREVIEW_BIN names; ci.yml's build job \
            downloads the preview and passes --include-ignored"]
fn table_functions_are_read_as_with_the_reader_unset() {
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
        for reader in [Reader::Default, Reader::DuckDbPreview] {
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
#[ignore = "needs the DuckDB 2.0 build ARC_DUCKDB_PREVIEW_BIN names; ci.yml's build job \
            downloads the preview and passes --include-ignored"]
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
    let run = arc_run(dir.path(), Reader::DuckDbPreview);
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

/// What `duckdb` on PATH reports as its version: `v1.5.5`.
fn version_on_path() -> String {
    let out = Command::new("duckdb")
        .args(["-no-init", "-csv", "-noheader", "-c", "SELECT version()"])
        .output()
        .expect("a duckdb on PATH");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn with_the_reader_set_a_duckdb_before_2_is_refused_before_a_step_runs() {
    let version = version_on_path();
    assert!(
        version.starts_with("v1."),
        "the duckdb on PATH is {version}, and this test needs a 1.x"
    );
    let dir = project(&[("load", LOAD)]);
    let run = arc_run(dir.path(), Reader::Told("duckdb"));
    assert_eq!(run.code, Some(1), "arc run must refuse:\n{}", run.printed());
    assert!(
        run.stderr.contains(&format!("is DuckDB {version}"))
            && run.stderr.contains("needs a DuckDB 2.0 build"),
        "the refusal must name {version} and ask for a 2.0 build:\n{}",
        run.stderr
    );
    assert!(
        !dir.path().join("reader.duckdb").exists() && !run.stdout.contains("] load"),
        "a step ran:\n{}",
        run.printed()
    );
}

#[test]
fn a_reader_arc_does_not_have_is_refused_naming_the_one_it_has() {
    for value in ["sqlparser", "DuckDB", ""] {
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
                && run.stderr.contains("`duckdb`"),
            "ARC_SQL_READER={value:?}: the refusal must name the value and the one it takes:\n{}",
            run.stderr
        );
        assert!(
            !dir.path().join("reader.duckdb").exists(),
            "ARC_SQL_READER={value:?}: a step ran"
        );
    }
}

#[test]
#[ignore = "needs the DuckDB 2.0 build ARC_DUCKDB_PREVIEW_BIN names; ci.yml's build job \
            downloads the preview and passes --include-ignored"]
fn a_pivot_joined_to_another_table_is_not_read_as_reading_its_first() {
    let dir = project(&[
        ("load", LOAD),
        ("pivot", "PIVOT t JOIN u USING (k) ON c USING sum(v);\n"),
    ]);
    let run = arc_run(dir.path(), Reader::DuckDbPreview);
    assert_eq!(run.code, Some(0), "arc run must exit 0:\n{}", run.printed());
    let pivot = statements(&read_contract(dir.path()), "pivot");
    assert_eq!(pivot.len(), 1, "one statement: {pivot:?}");
    assert_ne!(
        pivot[0].1,
        ["t"],
        "the pivot reads the rows of t and u, and the contract holds t as the one it reads"
    );
}
