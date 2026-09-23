//! End to end, through the real `arc` binary: a Protocol whose last step publishes
//! its build into a local DuckLake catalog with `op: ducklake_publish@1`.
//!
//! The question each test answers is one a publish outside the Run used to have to
//! answer after the fact from run records — did the bytes come from a finished Run,
//! and from the Protocol that claims to build them. Driven through the binary because
//! what is being pinned is what a Run does across several invocations against one
//! state store and one catalog, and because the catalog is what a reader queries.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run`, and the
//! `ducklake` DuckDB extension, which the operator installs on first use.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::{arc_run, step_outcome, strip_ansi};

const MANIFEST: &str = r#"name: lake_publish
engine: duckdb
db: build/pipeline.db
steps:
  - name: build
    sql: models/build.sql
  - name: export
    op: parquet_export@1
    with:
      input: src
      dest: build/out.parquet
      order_by: id
  - name: publish
    op: ducklake_publish@1
    with:
      file: build/out.parquet
      catalog: build/lake.ducklake
      table: t
"#;

const CREDENTIAL: &str = "      credential:
        type: s3
        env:
          key_id: ARC_TEST_PUBLISH_KEY_ID
          secret: ARC_TEST_PUBLISH_SECRET
";

fn project(manifest: &str, rows: u32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::write(dir.path().join("arcform.yaml"), manifest).unwrap();
    set_rows(dir.path(), rows);
    dir
}

/// Rewrite the build model to produce `0..rows` — a rebuild, as far as the Run can tell.
fn set_rows(project: &Path, rows: u32) {
    std::fs::write(
        project.join("models/build.sql"),
        format!("CREATE OR REPLACE TABLE src AS SELECT range AS id FROM range({rows});\n"),
    )
    .unwrap();
}

/// One `arc run` with extra arguments and environment, returning (exit, stdout, stderr).
fn arc(project: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arc"));
    cmd.current_dir(project)
        .arg("run")
        .args(args)
        .env_remove("ARC_TEST_PUBLISH_KEY_ID")
        .env_remove("ARC_TEST_PUBLISH_SECRET");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn arc run");
    (
        out.status.code(),
        strip_ansi(&String::from_utf8_lossy(&out.stdout)),
        strip_ansi(&String::from_utf8_lossy(&out.stderr)),
    )
}

/// Read the catalog through a connection of the test's own.
fn lake<T>(project: &Path, read: impl FnOnce(&duckdb::Connection) -> T) -> T {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let catalog = project.join("build/lake.ducklake");
    conn.execute_batch(&format!(
        "LOAD ducklake; ATTACH 'ducklake:{}' AS lake;",
        catalog.display()
    ))
    .unwrap();
    read(&conn)
}

fn snapshots(project: &Path) -> i64 {
    lake(project, |c| {
        c.query_row("SELECT count(*) FROM ducklake_snapshots('lake')", [], |r| {
            r.get(0)
        })
        .unwrap()
    })
}

fn row_count(project: &Path) -> i64 {
    lake(project, |c| {
        c.query_row("SELECT count(*) FROM lake.t", [], |r| r.get(0))
            .unwrap()
    })
}

fn contracts(project: &Path) -> BTreeSet<PathBuf> {
    let Ok(entries) = std::fs::read_dir(project.join("build/.arcform/runs")) else {
        return BTreeSet::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
}

/// The `publish` step's entry in the contract the run just written — the one file in
/// `after` that was not in `before`.
fn publish_entry(before: &BTreeSet<PathBuf>, after: &BTreeSet<PathBuf>) -> serde_json::Value {
    let new: Vec<_> = after.difference(before).collect();
    assert_eq!(new.len(), 1, "one run writes one contract: {new:?}");
    let contract: serde_json::Value =
        serde_json::from_slice(&std::fs::read(new[0]).unwrap()).unwrap();
    contract["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "publish")
        .expect("a publish step in the contract")
        .clone()
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

/// The publish runs after the build, is skipped with it, runs again when
/// it rebuilds, and the run contract names the snapshot it committed.
#[test]
fn a_run_publishes_its_build_once_and_again_only_when_it_rebuilds() {
    let dir = project(MANIFEST, 5);
    let p = dir.path();

    let before = contracts(p);
    arc_run(p);
    let entry = publish_entry(&before, &contracts(p));
    assert_eq!(entry["status"]["state"], "success");
    let current: i64 = lake(p, |c| {
        c.query_row(
            "SELECT max(snapshot_id) FROM ducklake_snapshots('lake')",
            [],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert_eq!(entry["report"]["action"], "registered");
    assert_eq!(entry["report"]["snapshot_id"], current);
    assert_eq!(
        entry["report"]["sha256"],
        sha256(&p.join("build/out.parquet"))
    );
    let published = snapshots(p);
    assert_eq!(row_count(p), 5);

    // Nothing rebuilt: the publish is skipped with its input, and commits nothing.
    let before = contracts(p);
    let stdout = arc_run(p);
    assert_eq!(step_outcome(&stdout, "export"), "skip: hash_clean");
    assert_eq!(step_outcome(&stdout, "publish"), "skip: hash_clean");
    let entry = publish_entry(&before, &contracts(p));
    assert_eq!(entry["status"]["state"], "skipped");
    assert!(
        entry["report"].is_null(),
        "a skipped step reports nothing: {entry}"
    );
    assert_eq!(snapshots(p), published);

    // A rebuild marks the publish stale through the file it reads.
    set_rows(p, 7);
    let stdout = arc_run(p);
    assert_eq!(step_outcome(&stdout, "export"), "ran");
    assert_eq!(step_outcome(&stdout, "publish"), "ran");
    assert_eq!(snapshots(p), published + 1);
    assert_eq!(row_count(p), 7);
}

/// A Run whose build fails never reaches the publish, and the catalog gains no
/// snapshot. Starts from a Run that did publish, then breaks the build.
#[test]
fn a_failed_build_never_reaches_the_publish() {
    let dir = project(MANIFEST, 5);
    let p = dir.path();
    arc_run(p);
    let published = snapshots(p);

    std::fs::write(p.join("models/build.sql"), "SELEC broken;\n").unwrap();
    let (code, stdout, stderr) = arc(p, &[], &[]);
    assert_ne!(
        code,
        Some(0),
        "a broken build fails the Run:\n{stdout}\n{stderr}"
    );
    assert!(
        !stdout.contains("] publish ") && !stderr.contains("ducklake_publish:"),
        "the publish step was reached:\n{stdout}\n{stderr}"
    );
    assert_eq!(snapshots(p), published, "the catalog gained a snapshot");
    assert_eq!(row_count(p), 5);
}

/// A declared credential that is not in the environment refuses the publish,
/// naming the step and the variable, and nothing is published. The same Run with the
/// variables set publishes.
#[test]
fn a_publish_without_its_credential_refuses_naming_the_step_and_the_variable() {
    let dir = project(&format!("{MANIFEST}{CREDENTIAL}"), 5);
    let p = dir.path();

    let (code, stdout, stderr) = arc(p, &[], &[("ARC_TEST_PUBLISH_KEY_ID", "id")]);
    assert_ne!(code, Some(0), "{stdout}\n{stderr}");
    assert!(stderr.contains("step 'publish' failed"), "{stderr}");
    assert!(
        stderr.contains("no credential available — `credential.env` names ARC_TEST_PUBLISH_SECRET"),
        "{stderr}"
    );
    assert!(
        !p.join("build/lake.ducklake").exists(),
        "nothing was attached"
    );

    let (code, stdout, stderr) = arc(
        p,
        &[],
        &[
            ("ARC_TEST_PUBLISH_KEY_ID", "id"),
            ("ARC_TEST_PUBLISH_SECRET", "secret"),
        ],
    );
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert_eq!(row_count(p), 5);
}

/// Forcing a Run over an unchanged build re-executes the publish, and the publish
/// is a no-op that reports the snapshot already holding the bytes.
#[test]
fn a_forced_run_over_an_unchanged_build_publishes_nothing_new() {
    let dir = project(MANIFEST, 5);
    let p = dir.path();
    arc_run(p);
    let published = snapshots(p);

    let before = contracts(p);
    let (code, stdout, stderr) = arc(p, &["--force"], &[]);
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert_eq!(step_outcome(&stdout, "publish"), "ran");
    assert!(
        stderr.contains("ducklake_publish: unchanged build/out.parquet into main.t"),
        "{stderr}"
    );
    let entry = publish_entry(&before, &contracts(p));
    assert_eq!(entry["report"]["action"], "unchanged");
    assert_eq!(
        snapshots(p),
        published,
        "a second snapshot of identical bytes"
    );
    assert_eq!(row_count(p), 5);
}
