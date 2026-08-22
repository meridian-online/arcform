//! End-to-end, through the real `arc` binary and a real DuckDB: the shape the
//! published `gleif` protocol ships, and what a warm `arc run` with no `--force`
//! does when the bytes underneath it move.
//!
//! `fetch` is a precondition-gated `command:` step declaring an ordering token
//! (`produces: [src_raw]` — there is no file at `./src_raw`), and `load` reads the
//! bytes it fetched through a glob (`read_csv('build/src/*.csv')`). Neither end used
//! to hash those bytes: `compute_staleness` returned before `is_hash_stale` for a
//! command step, and a glob was classified `Pattern` and excluded from hashing. So
//! after `load` settled, appending a row, truncating the CSV to zero and deleting it
//! outright each left `load [skip: hash_clean]` at exit 0 with the built table still
//! holding the original row — measured on `main` at 238735d with this manifest.
//!
//! Driven through the binary rather than in-process because the thing being pinned is
//! what a Run DOES over several invocations against one state store, and because the
//! built table is the observable an analyst actually gets. `MockEngine` runs no SQL,
//! so in-process it can only assert which steps the engine was handed.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run`.

use std::path::Path;
use std::process::Command;

const MANIFEST: &str = r#"name: glob_read
engine: duckdb
db: build/glob_read.db
steps:
  - name: fetch
    command: "mkdir -p build/src && printf 'lei,name\nA,Alpha\n' > build/src/data.csv"
    produces: [src_raw]
    preconditions:
      - modified_after: { path: build/src, period: 24h }
  - name: load
    sql: models/load.sql
    depends_on: [src_raw]
"#;

const MODEL: &str = "CREATE OR REPLACE TABLE t AS\n  \
                     SELECT * FROM read_csv('build/src/*.csv', header = true, all_varchar = true);\n";

const SEED: &[u8] = b"lei,name\nA,Alpha\n";

/// A project with the CSV already in place, so `fetch`'s `modified_after` precondition
/// on the freshly created `build/src` is FRESH from the first run onwards. That is the
/// point: the fetch must keep skipping throughout, so the only thing that can mark
/// `load` stale is the bytes it reads.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::create_dir_all(dir.path().join("build/src")).unwrap();
    std::fs::write(dir.path().join("arcform.yaml"), MANIFEST).unwrap();
    std::fs::write(dir.path().join("models/load.sql"), MODEL).unwrap();
    std::fs::write(dir.path().join("build/src/data.csv"), SEED).unwrap();
    dir
}

/// Drop ANSI styling so a step line can be read as text. `arc` colours
/// unconditionally, including when its stdout is a pipe.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// What one `arc run` did to `step`: `"ran"`, or the skip reason it reported.
fn step_outcome(stdout: &str, step: &str) -> String {
    let plain = strip_ansi(stdout);
    let line = plain
        .lines()
        .find(|l| l.starts_with('[') && l.contains(&format!("] {step} ")))
        .unwrap_or_else(|| panic!("no step line for '{step}' in:\n{plain}"))
        .to_string();
    match line.split_once("[skip: ") {
        Some((_, rest)) => format!("skip: {}", rest.trim_end_matches(']')),
        None => "ran".to_string(),
    }
}

/// One `arc run`, returning (exit code, stdout, stderr).
fn arc_run_raw(project: &Path) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .arg("run")
        .output()
        .expect("spawn arc run");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn arc_run(project: &Path) -> String {
    let (code, stdout, stderr) = arc_run_raw(project);
    assert_eq!(
        code,
        Some(0),
        "arc run must exit 0 here:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}

/// The rows the built table actually holds, read back through the same DuckDB the
/// run used. This is the analyst-visible half: a step that skipped when it should not
/// have leaves the table disagreeing with the file on disk.
fn table_rows(project: &Path) -> usize {
    let out = Command::new("duckdb")
        .arg(project.join("build/glob_read.db"))
        .args([
            "-noheader",
            "-list",
            "-readonly",
            "-c",
            "SELECT count(*) FROM t",
        ])
        .output()
        .expect("spawn duckdb");
    assert!(out.status.success(), "duckdb: {:?}", out);
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("a row count")
}

#[test]
fn a_change_to_what_a_command_step_fetched_makes_the_glob_reader_stale() {
    let dir = project();
    let p = dir.path();
    let csv = p.join("build/src/data.csv");

    // Settle. Three runs, not two: a step that re-ran once and then went quiet would
    // pass a two-run check, and every assertion below is about the difference between
    // a settled run and a mutated one.
    arc_run(p);
    for run in 2..=3 {
        let out = arc_run(p);
        assert_eq!(step_outcome(&out, "load"), "skip: hash_clean", "run {run}");
        assert_eq!(
            step_outcome(&out, "fetch"),
            "skip: precondition_modified_after",
            "run {run}"
        );
    }
    assert_eq!(table_rows(p), 1, "the settled table holds the seeded row");

    // APPEND. This is the case that produced wrong data rather than merely a skipped
    // step: the CSV held two rows and the built table held one.
    let mut appended = SEED.to_vec();
    appended.extend_from_slice(b"B,Beta\n");
    std::fs::write(&csv, &appended).unwrap();
    let out = arc_run(p);
    assert_eq!(
        step_outcome(&out, "load"),
        "ran",
        "append: load must re-run"
    );
    assert_eq!(
        step_outcome(&out, "fetch"),
        "skip: precondition_modified_after",
        "append: the fetch's precondition is untouched, and re-fetching is the \
         expensive answer this must not give"
    );
    assert_eq!(
        table_rows(p),
        2,
        "append: the table must hold what the CSV holds"
    );
    assert_eq!(
        step_outcome(&arc_run(p), "load"),
        "skip: hash_clean",
        "append: and settle again rather than re-running forever"
    );

    // TRUNCATE to zero bytes. `fs::read` succeeds on an empty file, so this is the
    // case a missing-file check cannot see and only a content hash catches.
    std::fs::write(&csv, b"").unwrap();
    assert_eq!(
        step_outcome(&arc_run(p), "load"),
        "ran",
        "truncate: load must re-run"
    );

    // Restore and settle, so the delete below starts from a clean skip.
    std::fs::write(&csv, SEED).unwrap();
    arc_run(p);
    assert_eq!(step_outcome(&arc_run(p), "load"), "skip: hash_clean");

    // DELETE outright. The match set loses its only member, which moves the digest,
    // so `load` re-runs — and re-running is when DuckDB says the source is gone. A
    // silent `[skip: hash_clean]` at exit 0 becomes a named failure at a non-zero
    // one, which is the whole difference between the two behaviours.
    std::fs::remove_file(&csv).unwrap();
    let (code, stdout, stderr) = arc_run_raw(p);
    assert_eq!(
        step_outcome(&stdout, "load"),
        "ran",
        "delete: load must re-run"
    );
    assert_ne!(code, Some(0), "delete: the run must not report success");
    assert!(
        stderr.contains("build/src/*.csv"),
        "delete: and it must name what it could not read:\n{stderr}"
    );
}

#[test]
fn an_unrelated_file_beside_the_matched_one_does_not_drag_the_reader_stale() {
    // The control for the test above, and the one that refuses a hash of the whole
    // build directory: only what the pattern MATCHES may move the digest. Without
    // this, `a_change_to_what_a_command_step_fetched...` is equally satisfied by a
    // mechanism that marks the reader stale on any write anywhere.
    let dir = project();
    let p = dir.path();

    arc_run(p);
    assert_eq!(step_outcome(&arc_run(p), "load"), "skip: hash_clean");

    std::fs::write(p.join("build/src/README.txt"), b"not a csv").unwrap();
    assert_eq!(
        step_outcome(&arc_run(p), "load"),
        "skip: hash_clean",
        "README.txt does not match build/src/*.csv, so writing it is not a change to \
         what `load` reads"
    );

    std::fs::write(
        p.join("build/src/README.txt"),
        b"rewritten, still not a csv",
    )
    .unwrap();
    assert_eq!(
        step_outcome(&arc_run(p), "load"),
        "skip: hash_clean",
        "and rewriting it is not one either"
    );
}
