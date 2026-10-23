//! The parity gate: arc's two SQL readers read every SQL file in the repo, and what each
//! statement produces and reads must be the same under both, or the difference must be
//! one this file explains.
//!
//! Every `.sql` file under `tests/fixtures` and `examples` is read, so a new one joins
//! the gate without being listed; a `build` directory is a run's output and is skipped.
//! Each file is read as `arc run` reads a step: the file is the one SQL step of a
//! Protocol whose first step fails, so nothing runs and the run's contract still records,
//! for each statement of the step, what it produces and reads. That contract comes from
//! `extract_per_statement`, once with `ARC_SQL_READER=duckdb` and once with sqlparser-rs.
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

const DIFFERENCES: &[Difference] = &[
    Difference {
        file: "examples/brewtrend/models/trending.sql",
        statement: 4,
        duckdb_only_reads: &[],
        sqlparser_only_reads: &["categories"],
        duckdb_only_produces: &[],
        sqlparser_only_produces: &[],
        reason: "sqlparser-rs is right here: DuckDB gives no tree for a PIVOT, so the DuckDB \
                 reader reads `CREATE TABLE … AS WITH … PIVOT` as unread and keeps only the \
                 table it creates. The row closes when an unread statement falls back to \
                 sqlparser-rs, at the flip that makes DuckDB's reading the default",
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

/// A file sqlparser-rs cannot parse at all, so there is nothing to compare DuckDB's
/// reading with: that reading is pinned instead, as each statement's produces and reads.
/// The row holds while sqlparser-rs gives no statements for the file and DuckDB's
/// reading is the one pinned; anything else fails the test.
struct Unparsed {
    file: &'static str,
    reason: &'static str,
    duckdb: &'static [(&'static [&'static str], &'static [&'static str])],
}

const NOTHING: (&[&str], &[&str]) = (&[], &[]);

const UNPARSED: &[Unparsed] = &[
    Unparsed {
        file: "examples/code-lists/models/catalog.sql",
        reason: "sqlparser-rs cannot parse the options of `ATTACH 'ducklake:…' AS open \
                 (DATA_PATH …)`",
        duckdb: &[
            NOTHING,
            NOTHING,
            NOTHING,
            NOTHING,
            NOTHING,
            NOTHING,
            NOTHING,
            (&["naics"], &[]),
            (&["icd10cm"], &[]),
            NOTHING,
            NOTHING,
        ],
    },
    Unparsed {
        file: "examples/code-lists/models/load.sql",
        reason: "sqlparser-rs cannot parse `trim(x, chr(13))`, DuckDB's two-argument trim",
        duckdb: &[
            NOTHING,
            NOTHING,
            NOTHING,
            NOTHING,
            (&["naics_raw"], &["data/naics/naics_structure.xlsx"]),
            (&["icd10cm_raw"], &["data/icd10cm/icd10cm_order.txt"]),
        ],
    },
    Unparsed {
        file: "examples/code-lists/models/validate.sql",
        reason: "sqlparser-rs cannot parse the options of `ATTACH 'ducklake:…' AS open \
                 (DATA_PATH …)`",
        duckdb: &[
            NOTHING,
            NOTHING,
            NOTHING,
            NOTHING,
            NOTHING,
            (&["baseline"], &[]),
            (&["actual"], &["dist/icd10cm.parquet", "dist/naics.parquet"]),
            (&["rowcount_check"], &["actual", "baseline"]),
            (
                &["ri_check"],
                &["dist/icd10cm.parquet", "dist/naics.parquet"],
            ),
            (&["golden"], &[]),
            (
                &["golden_check"],
                &["dist/icd10cm.parquet", "dist/naics.parquet", "golden"],
            ),
            (&["catalog_check"], &["actual", "icd10cm", "naics"]),
            (
                &[],
                &[
                    "catalog_check",
                    "golden_check",
                    "ri_check",
                    "rowcount_check",
                ],
            ),
        ],
    },
];

/// One statement as the contract records it: what it produces, and what it reads.
type Reading = (BTreeSet<String>, BTreeSet<String>);

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
/// reader that refuses the step, or cannot parse it, gives no statements.
fn read(file: &Path, reader: Reader, duckdb: &OsString) -> Result<(Vec<Reading>, String), String> {
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
        Reader::DuckDb => {
            command.env("ARC_SQL_READER", "duckdb");
        }
        // With `ARC_SQL_READER` unset, sqlparser-rs reads the step. When DuckDB's reading
        // becomes the default, this sets `ARC_SQL_READER=sqlparser` instead.
        Reader::Sqlparser => {}
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
    Ok((statements, stderr))
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
    for path in &files {
        let file = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let readings = read(path, Reader::DuckDb, &duckdb)
            .and_then(|by_duckdb| Ok((by_duckdb, read(path, Reader::Sqlparser, &duckdb)?)));
        let ((by_duckdb, duckdb_stderr), (by_sqlparser, sqlparser_stderr)) = match readings {
            Ok(both) => both,
            Err(why) => {
                failures.push(format!("{file}: {why}"));
                continue;
            }
        };
        if by_duckdb.is_empty() && by_sqlparser.is_empty() {
            failures.push(format!(
                "{file}: neither reader gave a statement:\n{duckdb_stderr}\n{sqlparser_stderr}"
            ));
            continue;
        }
        // Why a reader gave no statements, when one gave none.
        let silent = |reading: &[Reading], stderr: &str| {
            if reading.is_empty() {
                format!("\n    (gave no statements; arc run printed:\n{stderr})")
            } else {
                String::new()
            }
        };
        if let Some(unparsed) = UNPARSED.iter().find(|u| u.file == file) {
            let pinned: Vec<Reading> = unparsed
                .duckdb
                .iter()
                .map(|(produces, reads)| (set(produces), set(reads)))
                .collect();
            // The row is seen either way: a reading that is not the pinned one fails here.
            seen.insert((file.clone(), 0));
            if !by_sqlparser.is_empty() || by_duckdb != pinned {
                failures.push(format!(
                    "{file} is in UNPARSED ({}), and its readings are not the ones pinned:\n    \
                     DuckDB:     {by_duckdb:?}\n    pinned:     {pinned:?}\n    \
                     sqlparser:  {by_sqlparser:?}{}",
                    unparsed.reason,
                    silent(&by_duckdb, &duckdb_stderr),
                ));
            }
            continue;
        }
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
    let rows = DIFFERENCES
        .iter()
        .map(|row| (row.file, row.statement, row.reason))
        .chain(UNPARSED.iter().map(|row| (row.file, 0, row.reason)));
    for (file, statement, reason) in rows {
        if !seen.contains(&(file.to_string(), statement)) {
            failures.push(format!(
                "{file}, statement {statement} (0: the whole file) is tabled ({reason}), and \
                 the two readers now agree on it; remove its row"
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
