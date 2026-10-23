//! The parity gate: arc's two SQL readers read every SQL file in the repo, and what each
//! statement produces and reads must be the same under both, or the difference must be
//! one this file explains.
//!
//! Every `.sql` file under `tests/fixtures` and `examples` is read, so a new one joins
//! the gate without being listed; a `build` directory is a run's output and is skipped.
//! Each file is read as `arc run` reads a step: the file is the one SQL step of a
//! Protocol whose first step fails, so nothing runs and the run's contract still records,
//! for each statement of the step, what it produces and reads. That contract comes from
//! `extract_per_statement`, once with `ARC_SQL_READER` unset, arc's default reading, and
//! once with `ARC_SQL_READER=sqlparser`. The default reading is DuckDB's, with sqlparser-rs
//! reading each statement DuckDB's reading cannot take, so a statement read by that fallback
//! compares sqlparser-rs with itself; a default run that read a whole file with sqlparser-rs,
//! because no DuckDB 2 answered, fails the gate. Which statements take that fallback is
//! pinned per file in `FALLBACK`, from the reader each contract statement names, so a
//! statement DuckDB's reading stops taking fails the gate too.
//! The recorded steps in `src/duckdb_lineage.rs` are held to the same comparison by the
//! parity tests in `src/introspect.rs`, which read them from recorded answers.
//!
//! The comparison is of names only: the contract records what a statement produces and
//! reads by name, and not the kind of each (a file, a glob pattern, a directory) or the
//! table functions arc could not read. Those are compared, reader against reader, by
//! the parity tests in `src/introspect.rs`, on the recorded steps alone; over the files
//! here they are an accepted limit of this gate.
//!
//! The DuckDB reader asks a DuckDB 2.0 command line, named by `ARC_DUCKDB_RECORD_BIN`, so
//! the test is `#[ignore]`d and an ordinary run skips it; `ci.yml`'s build job runs it on
//! the v2.0.0 release command line with `--include-ignored`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const RECORD_ENV: &str = "ARC_DUCKDB_RECORD_BIN";

/// What the warning says when a command read the graph with sqlparser-rs because no DuckDB
/// 2 answered.
const NO_DUCKDB: &str = "because no DuckDB 2 answered";

/// A statement on which the two readings are expected to differ, and exactly how: the
/// names only DuckDB's reading has and those only sqlparser-rs's has. The statement
/// counts from 1. Any other difference on it fails the test, as does a row that no longer
/// differs.
struct Difference {
    file: &'static str,
    statement: usize,
    duckdb_only_reads: &'static [&'static str],
    sqlparser_only_reads: &'static [&'static str],
    duckdb_only_produces: &'static [&'static str],
    sqlparser_only_produces: &'static [&'static str],
    reason: &'static str,
}

/// DuckDB reads the table an `EXISTS` reads, and sqlparser-rs does not.
const EXISTS: &str = "DuckDB reads the table the `EXISTS (SELECT 1 FROM …)` of the WHERE \
                      reads; sqlparser-rs reads no table inside an EXISTS, so DuckDB is right";

/// DuckDB reads the tables the scalar subqueries in a select list read.
const SCALAR: &str = "DuckDB reads the tables the scalar subqueries `(SELECT … FROM t)` of the \
                      select list read; sqlparser-rs reads no table inside one, so DuckDB is right";

/// A `CREATE TABLE name (columns) AS VALUES …`, which sqlparser-rs cannot parse.
const COLUMNS_AS_VALUES: &str = "sqlparser-rs cannot parse `CREATE TABLE name (columns) AS \
                                 VALUES …`, a column list without types, and reads the \
                                 statement as reading nothing; DuckDB reads the table it makes";

const DIFFERENCES: &[Difference] = &[
    Difference {
        file: "examples/code-lists/models/load.sql",
        statement: 6,
        duckdb_only_reads: &["data/icd10cm/icd10cm_order.txt"],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &["icd10cm_raw"],
        sqlparser_only_produces: &[],
        reason: "sqlparser-rs cannot parse `trim(x, chr(13))`, DuckDB's two-argument trim, and \
                 reads the statement as reading nothing; DuckDB reads it",
    },
    Difference {
        file: "examples/code-lists/models/validate.sql",
        statement: 6,
        duckdb_only_reads: &[],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &["baseline"],
        sqlparser_only_produces: &[],
        reason: COLUMNS_AS_VALUES,
    },
    Difference {
        file: "examples/code-lists/models/validate.sql",
        statement: 10,
        duckdb_only_reads: &[],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &["golden"],
        sqlparser_only_produces: &[],
        reason: COLUMNS_AS_VALUES,
    },
    Difference {
        file: "examples/code-lists/models/validate.sql",
        statement: 12,
        duckdb_only_reads: &["actual", "icd10cm", "naics"],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &[],
        sqlparser_only_produces: &[],
        reason: SCALAR,
    },
    Difference {
        file: "examples/code-lists/models/validate.sql",
        statement: 13,
        duckdb_only_reads: &[
            "catalog_check",
            "golden_check",
            "ri_check",
            "rowcount_check",
        ],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &[],
        sqlparser_only_produces: &[],
        reason: SCALAR,
    },
    Difference {
        file: "tests/fixtures/open_analytics/edgar_gleif/models/load.sql",
        statement: 5,
        duckdb_only_reads: &["gleif"],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &[],
        sqlparser_only_produces: &[],
        reason: EXISTS,
    },
    Difference {
        file: "tests/fixtures/open_analytics/edgar_gleif/models/load.sql",
        statement: 6,
        duckdb_only_reads: &["gleif"],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &[],
        sqlparser_only_produces: &[],
        reason: EXISTS,
    },
    Difference {
        file: "tests/fixtures/open_analytics/edgar_gleif/models/package.sql",
        statement: 2,
        duckdb_only_reads: &["gleif"],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &[],
        sqlparser_only_produces: &[],
        reason: EXISTS,
    },
    Difference {
        file: "tests/fixtures/open_analytics/edgar_gleif/models/tier.sql",
        statement: 3,
        duckdb_only_reads: &["authoritative_d"],
        sqlparser_only_reads: &[],
        duckdb_only_produces: &[],
        sqlparser_only_produces: &[],
        reason: EXISTS,
    },
];

/// The statements of each file the default reading does not read with DuckDB's parse alone,
/// each with the reader the contract names for it: `sqlparser`, for a statement DuckDB's
/// reading cannot take, or `nothing`, for one neither reader can read. A file not listed
/// has every statement read by DuckDB. Any change fails the test, so a statement DuckDB's
/// reading stops taking, which the comparison above would pass, is caught here.
const FALLBACK: &[(&str, &[(usize, &str)])] = &[
    // `CREATE FUNCTION`, a form the reader does not frame; a `CREATE TABLE … AS` holding a
    // PIVOT DuckDB gives no tree for.
    (
        "examples/brewtrend/models/trending.sql",
        &[(2, "sqlparser"), (4, "sqlparser")],
    ),
    // `ATTACH … (DATA_PATH …)`, which neither reads; two `DROP TABLE`s and two `CALL`s, forms
    // the reader does not frame.
    (
        "examples/code-lists/models/catalog.sql",
        &[
            (5, "nothing"),
            (6, "sqlparser"),
            (7, "sqlparser"),
            (10, "sqlparser"),
            (11, "sqlparser"),
        ],
    ),
    // `ATTACH … (DATA_PATH …, READ_ONLY)`, which neither reads.
    ("examples/code-lists/models/validate.sql", &[(5, "nothing")]),
    // `CREATE MACRO`, a form the reader does not frame.
    (
        "tests/fixtures/open_analytics/edgar_gleif/models/load.sql",
        &[(1, "sqlparser")],
    ),
];

/// One statement as the contract records it: what it produces, and what it reads.
type Reading = (BTreeSet<String>, BTreeSet<String>);

/// A reader's statements, which reader read each of them, and what `arc run` printed on
/// stderr.
type Read = (Vec<Reading>, Vec<String>, String);

#[derive(Clone, Copy, Debug)]
enum Reader {
    DuckDb,
    Sqlparser,
}

fn duckdb() -> OsString {
    std::env::var_os(RECORD_ENV).unwrap_or_else(|| {
        panic!(
            "{RECORD_ENV} must name a DuckDB 2.0 build: this test is #[ignore]d so an \
             ordinary run skips it, and running it with --include-ignored says one is staged"
        )
    })
}

/// Every `.sql` file under `dir`, skipping `build` directories and hidden ones.
fn sql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name != "build" && !name.starts_with('.') {
                sql_files(&path, out);
            }
        } else if path.extension().is_some_and(|x| x == "sql") {
            out.push(path);
        }
    }
}

/// What `reader` makes of each statement of the SQL file at `file`, from the contract of
/// an `arc run` that stops before the step runs, and what that run printed on stderr. A
/// reader that refuses the step gives no statements; one that cannot read a statement gives
/// it reading nothing.
fn read(file: &Path, reader: Reader, duckdb: &OsString) -> Result<Read, String> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(file, dir.path().join("read.sql")).unwrap();
    std::fs::write(
        dir.path().join("arcform.yaml"),
        "name: parity\nengine: duckdb\ndb: parity.duckdb\nsteps:\n  \
         - name: stop\n    command: exit 1\n  \
         - name: read\n    sql: read.sql\n",
    )
    .unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_arc"));
    command.current_dir(dir.path()).arg("run");
    for var in [
        "ARC_SQL_READER",
        "ARC_DUCKDB_BIN",
        "ARC_ALLOW_UNTESTED_ENGINE",
    ] {
        command.env_remove(var);
    }
    command.env("ARC_DUCKDB_BIN", duckdb);
    match reader {
        // With `ARC_SQL_READER` unset, DuckDB's reading is arc's default.
        Reader::DuckDb => {}
        Reader::Sqlparser => {
            command.env("ARC_SQL_READER", "sqlparser");
        }
    }
    let out = command.output().expect("spawn arc run");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    let runs = dir.path().join("build/.arcform/runs");
    let contract = std::fs::read_dir(&runs)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "json"))
        .ok_or_else(|| format!("arc run wrote no contract:\n{stderr}"))?;
    let contract: serde_json::Value =
        serde_json::from_slice(&std::fs::read(contract).unwrap()).expect("the contract is JSON");
    let step = contract["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|s| s["name"] == "read"))
        .ok_or_else(|| format!("the contract has no step `read`:\n{contract:#}"))?;
    let names = |list: &serde_json::Value| -> BTreeSet<String> {
        list.as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_str().expect("a name").to_string())
            .collect()
    };
    let statements: Vec<Reading> = step["sql"]["statements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| (names(&s["produces"]), names(&s["reads"])))
        .collect();
    let read_by: Vec<String> = step["sql"]["statements"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| s["read_by"].as_str().unwrap_or("absent").to_string())
        .collect();
    Ok((statements, read_by, stderr))
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(ToString::to_string).collect()
}

/// Whether `difference` is exactly the one between `duckdb` and `sqlparser`.
fn is_exactly(difference: &Difference, duckdb: &Reading, sqlparser: &Reading) -> bool {
    let only = |a: &BTreeSet<String>, b: &BTreeSet<String>| -> BTreeSet<String> {
        a.difference(b).cloned().collect()
    };
    let ((d_produces, d_reads), (s_produces, s_reads)) = (duckdb, sqlparser);
    only(d_reads, s_reads) == set(difference.duckdb_only_reads)
        && only(s_reads, d_reads) == set(difference.sqlparser_only_reads)
        && only(d_produces, s_produces) == set(difference.duckdb_only_produces)
        && only(s_produces, d_produces) == set(difference.sqlparser_only_produces)
}

fn show(reading: Option<&Reading>) -> String {
    match reading {
        Some((produces, reads)) => format!("produces {produces:?}, reads {reads:?}"),
        None => "no such statement".to_string(),
    }
}

#[test]
#[ignore = "needs the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build job \
            runs it on the v2.0.0 release CLI with --include-ignored"]
fn both_readers_read_every_sql_file_in_the_repo_alike() {
    let duckdb = duckdb();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sql_files(&root.join("tests/fixtures"), &mut files);
    sql_files(&root.join("examples"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no SQL files found");

    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    // Files the comparison did not reach, whose rows are not called stale.
    let mut skipped = BTreeSet::new();
    for path in &files {
        let file = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let readings = read(path, Reader::DuckDb, &duckdb)
            .and_then(|by_duckdb| Ok((by_duckdb, read(path, Reader::Sqlparser, &duckdb)?)));
        let ((by_duckdb, read_by, duckdb_stderr), (by_sqlparser, _, sqlparser_stderr)) =
            match readings {
                Ok(both) => both,
                Err(why) => {
                    failures.push(format!("{file}: {why}"));
                    skipped.insert(file.clone());
                    continue;
                }
            };
        // A default run that read the file with sqlparser-rs would compare it with itself.
        if duckdb_stderr.contains(NO_DUCKDB) {
            failures.push(format!(
                "{file}: the default run read the file with sqlparser-rs, as no DuckDB 2 \
                 answered:\n{duckdb_stderr}"
            ));
            skipped.insert(file.clone());
            continue;
        }
        if by_duckdb.is_empty() && by_sqlparser.is_empty() {
            failures.push(format!(
                "{file}: neither reader gave a statement:\n{duckdb_stderr}\n{sqlparser_stderr}"
            ));
            skipped.insert(file.clone());
            continue;
        }
        let fell_back: Vec<(usize, String)> = read_by
            .iter()
            .enumerate()
            .filter(|(_, by)| *by != "duckdb")
            .map(|(i, by)| (i + 1, by.clone()))
            .collect();
        let pinned: Vec<(usize, String)> = FALLBACK
            .iter()
            .find(|(f, _)| *f == file)
            .map(|(_, rows)| rows.iter().map(|(n, by)| (*n, by.to_string())).collect())
            .unwrap_or_default();
        if fell_back != pinned {
            failures.push(format!(
                "{file}: the statements the default reading does not read with DuckDB's parse \
                 alone are {fell_back:?}, and FALLBACK pins {pinned:?}"
            ));
        }
        // Why a reader gave no statements, when one gave none.
        let silent = |reading: &[Reading], stderr: &str| {
            if reading.is_empty() {
                format!("\n    (gave no statements; arc run printed:\n{stderr})")
            } else {
                String::new()
            }
        };
        for i in 0..by_duckdb.len().max(by_sqlparser.len()) {
            let (d, s) = (by_duckdb.get(i), by_sqlparser.get(i));
            if d == s {
                continue;
            }
            let statement = i + 1;
            let row = DIFFERENCES
                .iter()
                .find(|row| row.file == file && row.statement == statement);
            if row.is_some() {
                // Seen either way: a difference that is not the row's fails below.
                seen.insert((file.clone(), statement));
            }
            if let (Some(row), Some(d), Some(s)) = (row, d, s)
                && is_exactly(row, d, s)
            {
                continue;
            }
            let tabled = match row {
                Some(row) => format!(
                    "\n    (DIFFERENCES expects DuckDB only to read {:?} and produce {:?}, and \
                     sqlparser-rs only to read {:?} and produce {:?}: {})",
                    row.duckdb_only_reads,
                    row.duckdb_only_produces,
                    row.sqlparser_only_reads,
                    row.sqlparser_only_produces,
                    row.reason
                ),
                None => String::new(),
            };
            failures.push(format!(
                "{file}, statement {statement}:\n    DuckDB:     {}{}\n    sqlparser:  {}{}{tabled}",
                show(d),
                silent(&by_duckdb, &duckdb_stderr),
                show(s),
                silent(&by_sqlparser, &sqlparser_stderr),
            ));
        }
    }
    for (file, _) in FALLBACK {
        if !files
            .iter()
            .any(|path| path.strip_prefix(root).is_ok_and(|p| p == Path::new(file)))
        {
            failures.push(format!(
                "{file} is in FALLBACK and is not in the repo; remove its row"
            ));
        }
    }
    for row in DIFFERENCES {
        if skipped.contains(row.file) {
            continue;
        }
        if !seen.contains(&(row.file.to_string(), row.statement)) {
            failures.push(format!(
                "{}, statement {} is tabled ({}), and the two readers now agree on it; remove \
                 its row",
                row.file, row.statement, row.reason
            ));
        }
    }
    eprintln!("read {} SQL files with both readers", files.len());
    assert!(
        failures.is_empty(),
        "{} difference(s) the tables do not explain:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
