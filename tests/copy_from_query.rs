//! End-to-end: a `COPY (SELECT …) TO 'file'` step records the file it writes and the
//! tables its query reads, through the real `arc` binary — in the asset graph a run
//! prints, in the run contract, and in whether a step that reads the file later is
//! skipped as fresh.
//!
//! Before this, a `COPY <table> TO 'file'` recorded its file and a `COPY (SELECT …)
//! TO 'file'` recorded only the query's tables: an analyst who exported a filtered or
//! aggregated table got a graph with no file in it, and a later step that read the
//! file did not depend on the step that wrote it.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run`.

use std::path::{Path, PathBuf};

mod common;
use common::{arc_run, step_outcome, strip_ansi};

/// A project whose steps run in the order given, each a `sql:` step over
/// `<name>.sql`, on one database file so a later step can read what an earlier one
/// created.
fn project(steps: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest =
        String::from("name: copy_query\nengine: duckdb\ndb: copy_query.duckdb\nsteps:\n");
    for (name, sql) in steps {
        manifest.push_str(&format!("  - name: {name}\n    sql: {name}.sql\n"));
        std::fs::write(dir.path().join(format!("{name}.sql")), sql).unwrap();
    }
    std::fs::write(dir.path().join("arcform.yaml"), manifest).unwrap();
    dir
}

/// A step that creates table `a`, which the exports below read.
const LOAD_A: &str =
    "CREATE TABLE a AS SELECT * FROM (VALUES (1, 'n'), (2, 's'), (3, 'n')) AS t(x, region);\n";

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

/// The contract's asset named `name`, or a panic listing what was found.
fn asset<'a>(contract: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    contract["assets"]
        .as_array()
        .expect("assets array")
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("no asset {name:?} in the contract:\n{}", contract["assets"]))
}

/// The per-statement `produces`/`reads` of `step`, as the contract records them.
fn statements<'a>(contract: &'a serde_json::Value, step: &str) -> &'a Vec<serde_json::Value> {
    contract["steps"]
        .as_array()
        .expect("steps array")
        .iter()
        .find(|s| s["name"] == step)
        .unwrap_or_else(|| panic!("no step {step:?} in the contract"))["sql"]["statements"]
        .as_array()
        .expect("statements array")
}

fn names(list: &serde_json::Value) -> Vec<&str> {
    list.as_array()
        .expect("a list of names")
        .iter()
        .map(|v| v.as_str().expect("a name"))
        .collect()
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

#[test]
fn a_copy_from_a_query_produces_its_file_and_reads_the_querys_table() {
    let dir = project(&[
        ("load", LOAD_A),
        (
            "export",
            "COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER);\n",
        ),
    ]);
    let stdout = arc_run(dir.path());
    assert!(
        dir.path().join("out.csv").is_file(),
        "the COPY must really have written out.csv"
    );

    // The asset graph the run prints.
    let file = graph_entry(&stdout, "out.csv");
    assert_eq!(file[0], "out.csv [file]", "printed entry: {file:?}");
    assert!(
        file.contains(&"produced by export".to_string()),
        "the printed graph does not show export producing out.csv: {file:?}"
    );
    let table = graph_entry(&stdout, "a");
    assert!(
        table.contains(&"feeds export".to_string()),
        "the printed graph does not show a feeding export: {table:?}"
    );

    // The run contract, per asset and per statement.
    let contract = read_contract(dir.path());
    let out = asset(&contract, "out.csv");
    assert_eq!(out["kind"], "file");
    assert_eq!(out["produced_by"], "export");
    assert!(
        names(&asset(&contract, "a")["consumed_by"]).contains(&"export"),
        "the contract does not show export consuming a: {}",
        asset(&contract, "a")
    );
    let export = statements(&contract, "export");
    assert_eq!(export.len(), 1, "one statement in export: {export:?}");
    assert_eq!(names(&export[0]["produces"]), ["out.csv"]);
    assert_eq!(names(&export[0]["reads"]), ["a"]);
}

#[test]
fn a_copy_from_a_query_records_a_directory_where_the_same_options_make_copy_from_a_table_one() {
    let partitioned = "(FORMAT parquet, PARTITION_BY (region))";
    let mut kinds = Vec::new();
    for (label, sql) in [
        (
            "query",
            format!("COPY (SELECT * FROM a) TO 'parts' {partitioned};\n"),
        ),
        ("table", format!("COPY a TO 'parts' {partitioned};\n")),
    ] {
        let dir = project(&[("load", LOAD_A), ("export", &sql)]);
        let stdout = arc_run(dir.path());
        assert!(
            dir.path().join("parts").is_dir(),
            "[{label}] the COPY must really have written a directory at parts"
        );
        let entry = graph_entry(&stdout, "parts");
        assert_eq!(
            entry[0], "parts [directory]",
            "[{label}] printed: {entry:?}"
        );
        let contract = read_contract(dir.path());
        kinds.push(
            asset(&contract, "parts")["kind"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert_eq!(
        kinds,
        ["directory", "directory"],
        "the query form and the table form, in that order, must agree"
    );

    // The control: the same query copy without an option that writes a directory
    // records a file, so the result above is the options' doing and not every COPY's.
    let dir = project(&[
        ("load", LOAD_A),
        (
            "export",
            "COPY (SELECT * FROM a) TO 'one.parquet' (FORMAT parquet);\n",
        ),
    ]);
    arc_run(dir.path());
    assert_eq!(
        asset(&read_contract(dir.path()), "one.parquet")["kind"],
        "file"
    );
}

#[test]
fn a_step_reading_the_file_a_query_copy_wrote_reruns_when_the_copy_changes() {
    let dir = project(&[
        ("export", "COPY (SELECT 1 AS x) TO 'out.csv' (HEADER);\n"),
        (
            "load",
            "CREATE OR REPLACE TABLE r AS SELECT * FROM read_csv('out.csv');\n",
        ),
    ]);

    let first = arc_run(dir.path());
    assert_eq!(step_outcome(&first, "export"), "ran");
    assert_eq!(step_outcome(&first, "load"), "ran");

    // The control: with nothing changed both steps settle to a skip, so the re-run
    // below is what the edit did and not what every run does.
    let second = arc_run(dir.path());
    assert_eq!(step_outcome(&second, "export"), "skip: hash_clean");
    assert_eq!(step_outcome(&second, "load"), "skip: hash_clean");

    // Change the first step's SQL so the file it writes changes.
    std::fs::write(
        dir.path().join("export.sql"),
        "COPY (SELECT 2 AS x) TO 'out.csv' (HEADER);\n",
    )
    .unwrap();
    let third = arc_run(dir.path());
    assert_eq!(step_outcome(&third, "export"), "ran");
    assert_eq!(
        step_outcome(&third, "load"),
        "ran",
        "load reads out.csv, which export rewrote, and was skipped as fresh:\n{}",
        strip_ansi(&third)
    );
}
