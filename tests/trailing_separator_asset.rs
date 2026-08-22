//! End-to-end, through the real `arc` binary: a hand-declared asset name that ends in
//! a path separator is a directory, and a step that reads or produces one settles.
//!
//! `build/parts/` and `build/parts` name the same place, and only the first spelling
//! says which of the two it is. Before the classifier read that, a trailing separator
//! fell through to `AssetKind::File`, `fs::read` on a directory errored,
//! `produced_artifact_hash` withheld the hash, and the owning step re-ran on EVERY
//! run — for a `depends_on:` entry with nothing at all on stderr, because
//! `missing_declared_produces` reports the `produces:` side only. That is expensive
//! rather than wrong, which is exactly why nothing caught it: the run kept exiting 0.
//!
//! Driven here rather than against the classifier's return value because a step that
//! never settles is a fact about a sequence of runs, and because the silent half has
//! no observable but the absence of a line on stderr.
//!
//! Both sides are declared BY HAND. SQL introspection already types a
//! `COPY … PARTITION_BY` destination as a directory from the COPY options, and
//! `declared_kind` keeps the first kind recorded for a name — so a manifest whose
//! SQL also writes the directory is typed by the parser and says nothing about the
//! classifier. These two names appear in no SQL.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run`.

use std::path::Path;

mod common;
use common::{arc_run, arc_run_raw, step_outcome};

const MANIFEST: &str = r#"name: trailing_separator
engine: duckdb
db: build/ts.db
steps:
  - name: emit
    sql: models/emit.sql
    produces: [build/parts_out/]
  - name: consume
    sql: models/consume.sql
    depends_on: [build/parts_in/]
"#;

const EMIT_SQL: &str = "CREATE OR REPLACE TABLE emitted AS SELECT 1 AS v;\n";
const CONSUME_SQL: &str = "CREATE OR REPLACE TABLE consumed AS SELECT 2 AS v;\n";

/// A project with both directories already populated. `arc` runs the SQL, which
/// creates tables and writes neither directory — they stand in for what a real
/// partitioned write and a real upstream tool would have left there, which is what
/// makes this a test of the classifier rather than of DuckDB.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("models")).unwrap();
    std::fs::create_dir_all(p.join("build/parts_out")).unwrap();
    std::fs::create_dir_all(p.join("build/parts_in")).unwrap();
    std::fs::write(p.join("arcform.yaml"), MANIFEST).unwrap();
    std::fs::write(p.join("models/emit.sql"), EMIT_SQL).unwrap();
    std::fs::write(p.join("models/consume.sql"), CONSUME_SQL).unwrap();
    std::fs::write(p.join("build/parts_out/year=2024.parquet"), b"out-one").unwrap();
    std::fs::write(p.join("build/parts_in/year=2024.parquet"), b"in-one").unwrap();
    dir
}

fn outcomes(p: &Path) -> (String, String) {
    let stdout = arc_run(p);
    (
        step_outcome(&stdout, "emit"),
        step_outcome(&stdout, "consume"),
    )
}

#[test]
fn a_step_owning_a_name_that_ends_in_a_separator_settles_and_stays_settled() {
    let dir = project();
    let p = dir.path();

    assert_eq!(outcomes(p), ("ran".into(), "ran".into()), "run 1");

    // Four warm runs, not one. A step that re-ran once and then went quiet, and a
    // step that never settles at all, are the two things being told apart, and only
    // a sequence tells them apart.
    for run in 2..=5 {
        assert_eq!(
            outcomes(p),
            ("skip: hash_clean".into(), "skip: hash_clean".into()),
            "run {run}: `build/parts_out/` and `build/parts_in/` are directories that \
             are there and unchanged, so both steps must skip — reading a trailing \
             separator as a file name makes `fs::read` error here and re-runs both \
             on every run, at exit 0, forever"
        );
    }

    // And nothing is said about it, on the run that would say it. The `produces:`
    // side is the half that CAN speak; the `depends_on:` side has no observable but
    // this absence.
    let (_, _, stderr) = arc_run_raw(p);
    assert!(
        !stderr.contains("does not appear to have produced"),
        "a produced directory that is there must not be reported missing:\n{stderr}"
    );
}

#[test]
fn a_name_that_ends_in_a_separator_is_still_hashed_by_its_contents() {
    // The control that stops the test above being satisfied by a name dropped out of
    // the staleness question altogether. Settling is only the right answer if the
    // bytes are still being watched.
    let dir = project();
    let p = dir.path();

    arc_run(p);
    assert_eq!(
        outcomes(p),
        ("skip: hash_clean".into(), "skip: hash_clean".into()),
        "settled"
    );

    // A file inside the READ directory moves only the reader.
    std::fs::write(p.join("build/parts_in/year=2025.parquet"), b"in-two").unwrap();
    assert_eq!(
        outcomes(p),
        ("skip: hash_clean".into(), "ran".into()),
        "a file arriving in build/parts_in/ must re-run the step that declares it and \
         only that step"
    );
    assert_eq!(
        outcomes(p),
        ("skip: hash_clean".into(), "skip: hash_clean".into()),
        "and it settles again"
    );

    // A file inside the PRODUCED directory moves only the producer.
    std::fs::write(p.join("build/parts_out/year=2024.parquet"), b"rewritten").unwrap();
    assert_eq!(
        outcomes(p),
        ("ran".into(), "skip: hash_clean".into()),
        "rewriting a file in build/parts_out/ must re-run the step that produced it"
    );
    assert_eq!(
        outcomes(p),
        ("skip: hash_clean".into(), "skip: hash_clean".into()),
        "and it settles again"
    );

    // Emptying a directory but leaving it standing is the case a presence check
    // cannot see, and it has to move too.
    std::fs::remove_file(p.join("build/parts_in/year=2024.parquet")).unwrap();
    std::fs::remove_file(p.join("build/parts_in/year=2025.parquet")).unwrap();
    assert!(
        p.join("build/parts_in").is_dir(),
        "the directory itself survives"
    );
    assert_eq!(
        outcomes(p),
        ("skip: hash_clean".into(), "ran".into()),
        "an emptied but surviving directory is a change to what the step reads"
    );
}
