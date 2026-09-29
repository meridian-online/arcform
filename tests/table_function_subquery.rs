//! End-to-end: a SQL step that calls a table function on a subquery records the tables
//! the subquery reads, through the real `arc` binary — in the asset graph a run prints,
//! in the run contract, in the warnings, and in whether the step is skipped as fresh
//! when a table its subquery reads changes.
//!
//! `onager_pth_dijkstra((SELECT src, dst, w FROM edges))` used to record the function's
//! name as a table the step reads and no edge from `edges`: an analyst who changed
//! `edges` saw the step skipped as fresh, and its table kept the old rows.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run`. The tests that
//! call an extension's function do not load it, so their step fails in DuckDB: what arc
//! read is a static property of the SQL, printed and recorded before any step runs.

use std::path::{Path, PathBuf};

mod common;
use common::{arc_run, arc_run_raw, step_outcome, strip_ansi};

/// A project whose steps run in the order given, each a `sql:` step over
/// `<name>.sql`, on one database file so a later step can read what an earlier one
/// created.
fn project(steps: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest =
        String::from("name: subquery_call\nengine: duckdb\ndb: subquery_call.duckdb\nsteps:\n");
    for (name, sql) in steps {
        manifest.push_str(&format!("  - name: {name}\n    sql: {name}.sql\n"));
        std::fs::write(dir.path().join(format!("{name}.sql")), sql).unwrap();
    }
    std::fs::write(dir.path().join("arcform.yaml"), manifest).unwrap();
    dir
}

/// A step that makes `edges`, which the calls below read through a subquery.
const MAKE_EDGES: &str = "CREATE OR REPLACE TABLE edges AS \
    SELECT * FROM (VALUES (0, 1, 1.0, 'road'), (1, 2, 2.0, 'rail')) AS t(src, dst, w, kind);\n";

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

/// The names of the contract's assets.
fn asset_names(contract: &serde_json::Value) -> Vec<&str> {
    contract["assets"]
        .as_array()
        .expect("assets array")
        .iter()
        .map(|a| a["name"].as_str().expect("an asset name"))
        .collect()
}

/// The `reads` and `produces` of the one statement of `step`, as the contract records
/// them.
fn statement_of<'a>(contract: &'a serde_json::Value, step: &str) -> (Vec<&'a str>, Vec<&'a str>) {
    let step = contract["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|s| s["name"] == step)
        .unwrap_or_else(|| panic!("no step {step:?} in the contract"));
    let statements = step["sql"]["statements"]
        .as_array()
        .expect("statements array");
    assert_eq!(statements.len(), 1, "one statement in {step}");
    let names = |list: &'a serde_json::Value| -> Vec<&'a str> {
        list.as_array()
            .expect("a list of names")
            .iter()
            .map(|v| v.as_str().expect("a name"))
            .collect()
    };
    (
        names(&statements[0]["reads"]),
        names(&statements[0]["produces"]),
    )
}

/// The printed asset graph's entry for `asset`: its header line and the lines under
/// it, each with its whitespace collapsed to single spaces.
fn graph_entry(stdout: &str, asset: &str) -> Vec<String> {
    let plain = strip_ansi(stdout);
    let header = format!("  {asset} [");
    let mut lines = plain.lines().skip_while(|l| !l.starts_with(&header));
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("the printed graph has no entry for {asset:?}:\n{plain}"));
    let mut entry = vec![squash(first)];
    entry.extend(lines.take_while(|l| l.starts_with("      ")).map(squash));
    entry
}

fn squash(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The lines of `stderr` that are warnings.
fn warnings(stderr: &str) -> Vec<String> {
    strip_ansi(stderr)
        .lines()
        .filter(|l| l.contains("warning:"))
        .map(str::to_string)
        .collect()
}

// The step's DuckDB error names the function, because the extension is not loaded; a
// warning must not.
#[test]
fn a_function_called_on_a_subquery_reads_the_subquerys_table_and_not_its_own_name() {
    let dir = project(&[
        ("e", MAKE_EDGES),
        (
            "g",
            "CREATE TABLE paths AS SELECT * FROM onager_pth_dijkstra((SELECT src, dst, w FROM edges));\n",
        ),
    ]);
    let (_, stdout, stderr) = arc_run_raw(dir.path());

    // The asset graph the run prints.
    let edges = graph_entry(&stdout, "edges");
    assert!(
        edges.contains(&"feeds g".to_string()),
        "the printed graph does not show edges feeding g: {edges:?}"
    );
    assert!(
        !strip_ansi(&stdout).contains("onager_pth_dijkstra ["),
        "the printed graph has a node named for the function:\n{}",
        strip_ansi(&stdout)
    );

    // The run contract, per asset and per statement.
    let contract = read_contract(dir.path());
    assert!(
        !asset_names(&contract).contains(&"onager_pth_dijkstra"),
        "the contract has an asset named for the function: {:?}",
        asset_names(&contract)
    );
    let (reads, produces) = statement_of(&contract, "g");
    assert_eq!(reads, ["edges"], "what the step reads");
    assert_eq!(produces, ["paths"], "what the step produces");

    for warning in warnings(&stderr) {
        assert!(
            !warning.contains("onager_pth_dijkstra"),
            "a warning names the function: {warning}"
        );
    }
}

// A string inside the subquery is the subquery's own: it names nothing the call reads.
#[test]
fn a_string_inside_the_subquery_draws_no_warning() {
    let dir = project(&[
        ("e", MAKE_EDGES),
        (
            "g",
            "CREATE TABLE r AS SELECT * FROM summary((SELECT src, dst FROM edges WHERE kind = 'road'));\n",
        ),
    ]);
    let (code, stdout, stderr) = arc_run_raw(dir.path());
    assert_eq!(code, Some(0), "summary is DuckDB's own:\n{stderr}");

    assert!(
        graph_entry(&stdout, "edges").contains(&"feeds g".to_string()),
        "the printed graph does not show edges feeding g:\n{}",
        strip_ansi(&stdout)
    );
    let contract = read_contract(dir.path());
    assert!(
        !asset_names(&contract).contains(&"summary"),
        "the contract has an asset named for the function: {:?}",
        asset_names(&contract)
    );
    assert_eq!(statement_of(&contract, "g").0, ["edges"]);
    assert!(
        !stderr.contains("calls the table function"),
        "the run warned that the call is unread:\n{stderr}"
    );
}

// A string beside the subquery names something arc does not read, so the call keeps the
// warning, and the subquery's table is still read.
#[test]
fn a_string_beside_the_subquery_keeps_the_warning() {
    let dir = project(&[
        (
            "e",
            "CREATE OR REPLACE TABLE train AS SELECT 1 AS x, 2 AS y;\n",
        ),
        (
            "g",
            "CREATE TABLE m AS SELECT * FROM my_ext_fit((SELECT x, y FROM train), 'model');\n",
        ),
    ]);
    let (_, stdout, stderr) = arc_run_raw(dir.path());

    let contract = read_contract(dir.path());
    assert!(
        !asset_names(&contract).contains(&"my_ext_fit"),
        "the contract has an asset named for the function: {:?}",
        asset_names(&contract)
    );
    assert_eq!(statement_of(&contract, "g").0, ["train"]);
    assert!(
        graph_entry(&stdout, "train").contains(&"feeds g".to_string()),
        "the printed graph does not show train feeding g:\n{}",
        strip_ansi(&stdout)
    );
    let unread = stderr.matches("calls the table function").count();
    assert_eq!(unread, 1, "one warning expected, stderr:\n{stderr}");
    assert!(
        stderr.contains("step 'g' calls the table function 'my_ext_fit'"),
        "the warning names the step and the function, stderr:\n{stderr}"
    );
}

// Before, `s` kept its old rows: the step read `summary`, not `edges`, so a change to
// `edges` left it looking fresh.
#[test]
fn a_step_calling_a_function_on_a_subquery_reruns_when_the_table_it_reads_changes() {
    let two_rows = "CREATE OR REPLACE TABLE edges AS \
        SELECT * FROM (VALUES (0, 1), (1, 2)) AS t(src, dst);\n";
    let three_rows = "CREATE OR REPLACE TABLE edges AS \
        SELECT * FROM (VALUES (0, 1), (1, 2), (2, 3)) AS t(src, dst);\n";
    let dir = project(&[
        ("e", two_rows),
        (
            "g",
            "CREATE OR REPLACE TABLE s AS SELECT * FROM summary((SELECT src, dst FROM edges));\n",
        ),
    ]);
    let rows_of_s = |dir: &Path| -> i64 {
        let conn = duckdb::Connection::open(dir.join("subquery_call.duckdb")).unwrap();
        conn.query_row("SELECT count(*)::BIGINT FROM s", [], |row| row.get(0))
            .unwrap()
    };

    let first = arc_run(dir.path());
    assert_eq!(step_outcome(&first, "e"), "ran");
    assert_eq!(step_outcome(&first, "g"), "ran");
    assert_eq!(
        rows_of_s(dir.path()),
        2,
        "s holds a row for each of edges' two"
    );

    // The control: with nothing changed both steps settle to a skip, so the re-run
    // below is what the edit did and not what every run does.
    let second = arc_run(dir.path());
    assert_eq!(step_outcome(&second, "e"), "skip: hash_clean");
    assert_eq!(step_outcome(&second, "g"), "skip: hash_clean");

    // Add a row to `edges`.
    std::fs::write(dir.path().join("e.sql"), three_rows).unwrap();
    let third = arc_run(dir.path());
    assert_eq!(step_outcome(&third, "e"), "ran");
    assert_eq!(
        step_outcome(&third, "g"),
        "ran",
        "g reads edges through a subquery, which e rewrote, and was skipped as fresh:\n{}",
        strip_ansi(&third)
    );
    assert_eq!(
        rows_of_s(dir.path()),
        3,
        "s holds a row for each of the three rows edges now has"
    );
}
