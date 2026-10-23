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
//! The DuckDB reader asks a DuckDB 2.0 command line, named by `ARC_DUCKDB_RECORD_BIN`, so
//! the test is `#[ignore]`d and an ordinary run skips it; `ci.yml`'s build job runs it on
//! the v2.0.0 release command line with `--include-ignored`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

const RECORD_ENV: &str = "ARC_DUCKDB_RECORD_BIN";

/// The differences between the two readings that are expected: the file, the statement
/// (counting from 1, or 0 for every statement of a file sqlparser-rs cannot parse at
/// all), and why. Any other difference fails the test, and so does an entry here that no
/// longer differs.
const EXPECTED: &[(&str, usize, &str)] = &[
    (
        "examples/brewtrend/models/trending.sql",
        4,
        "DuckDB gives no tree for a PIVOT, so the DuckDB reader reads `CREATE TABLE … AS \
         WITH … PIVOT` as unread, keeping only the table it creates; an unread statement \
         is sqlparser-rs's to read once DuckDB's reading is the default",
    ),
    (
        "examples/code-lists/models/catalog.sql",
        0,
        "sqlparser-rs cannot parse the options of `ATTACH 'ducklake:…' AS open (DATA_PATH …)`",
    ),
    (
        "examples/code-lists/models/load.sql",
        0,
        "sqlparser-rs cannot parse `trim(x, chr(13))`, DuckDB's two-argument trim",
    ),
    (
        "examples/code-lists/models/validate.sql",
        0,
        "sqlparser-rs cannot parse the options of `ATTACH 'ducklake:…' AS open (DATA_PATH …)`",
    ),
    (
        "tests/fixtures/open_analytics/edgar_gleif/models/load.sql",
        5,
        "DuckDB reads `gleif` in the `EXISTS (SELECT 1 FROM gleif …)` of the WHERE; \
         sqlparser-rs reads no table inside an EXISTS",
    ),
    (
        "tests/fixtures/open_analytics/edgar_gleif/models/load.sql",
        6,
        "DuckDB reads `gleif` in the `EXISTS (SELECT 1 FROM gleif …)` of the WHERE; \
         sqlparser-rs reads no table inside an EXISTS",
    ),
    (
        "tests/fixtures/open_analytics/edgar_gleif/models/package.sql",
        2,
        "DuckDB reads `gleif` in the `NOT EXISTS (SELECT 1 FROM gleif …)` of the WHERE; \
         sqlparser-rs reads no table inside an EXISTS",
    ),
    (
        "tests/fixtures/open_analytics/edgar_gleif/models/tier.sql",
        3,
        "DuckDB reads `authoritative_d` in the `NOT EXISTS (SELECT 1 FROM authoritative_d \
         …)` of the WHERE; sqlparser-rs reads no table inside an EXISTS",
    ),
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

/// Whether [`EXPECTED`] holds `statement` of `file`.
fn expects(file: &str, statement: usize) -> bool {
    EXPECTED
        .iter()
        .any(|(f, n, _)| *f == file && *n == statement)
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
    let mut expected_seen = BTreeSet::new();
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
        for i in 0..by_duckdb.len().max(by_sqlparser.len()) {
            let (d, s) = (by_duckdb.get(i), by_sqlparser.get(i));
            if d == s {
                continue;
            }
            let statement = i + 1;
            // A whole-file entry holds only while sqlparser-rs reads none of the file.
            let whole_file = by_sqlparser.is_empty() && expects(&file, 0);
            if whole_file || expects(&file, statement) {
                expected_seen.insert((file.clone(), if whole_file { 0 } else { statement }));
                continue;
            }
            failures.push(format!(
                "{file}, statement {statement}:\n    DuckDB:     {}{}\n    sqlparser:  {}{}",
                show(d),
                silent(&by_duckdb, &duckdb_stderr),
                show(s),
                silent(&by_sqlparser, &sqlparser_stderr),
            ));
        }
    }
    for (file, statement, reason) in EXPECTED {
        if !expected_seen.contains(&(file.to_string(), *statement)) {
            failures.push(format!(
                "{file}, statement {statement} is expected to differ ({reason}), and the \
                 two readers now agree on it or it was not read; remove it from EXPECTED"
            ));
        }
    }
    eprintln!("read {} SQL files with both readers", files.len());
    assert!(
        failures.is_empty(),
        "the two readers differ on {} statement(s) the table does not explain:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
