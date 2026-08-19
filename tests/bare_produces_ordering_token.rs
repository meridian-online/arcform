//! End-to-end: a `produces:` name with no file behind it re-runs its step on every
//! run and says why, through the real `arc` binary.
//!
//! This is the cost of treating a separator-free `produces:` token as a path, and it
//! is the trade taken over the alternative — reading such a name as a table
//! identifier and dropping it out of the staleness path, which lets a step whose
//! declared artifact was truncated or deleted report `[skip: hash_clean]` at exit 0.
//! The re-run half is pinned in-process by
//! `runner::tests::test_bare_produces_ordering_token_reruns_rather_than_settling`;
//! the warning half has no observable but stderr, which is why this test spawns the
//! binary.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run`.

use std::path::Path;
use std::process::{Command, Output};

/// A step whose SQL creates a table and whose `produces:` names an ordering token —
/// the shape `examples/code-lists/arcform.yaml` ships as `produces: [raw_tables]`.
const TOKEN_MANIFEST: &str = "name: token_demo\nengine: duckdb\nsteps:\n  - name: load\n    sql: models/load.sql\n    produces: [raw_tables]\n";

/// The same step, declaring a `produces:` the SQL really writes. The negative
/// control: without it, a warning printed unconditionally and a step that never
/// settles for some unrelated reason would both satisfy the assertions below.
const FILE_MANIFEST: &str = "name: file_demo\nengine: duckdb\nsteps:\n  - name: load\n    sql: models/load.sql\n    produces: [build/loaded.csv]\n";

const TOKEN_MODEL: &str = "CREATE OR REPLACE TABLE naics_raw AS SELECT 1 AS code;\n";
const FILE_MODEL: &str = "CREATE OR REPLACE TABLE naics_raw AS SELECT 1 AS code;\n\
                          COPY naics_raw TO 'build/loaded.csv' (FORMAT csv);\n";

/// The substring the warning is built from, in `runner::run`.
const WARNING: &str = "does not appear to have produced";

fn project(manifest: &str, model: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("models")).unwrap();
    std::fs::create_dir_all(dir.path().join("build")).unwrap();
    std::fs::write(dir.path().join("arcform.yaml"), manifest).unwrap();
    std::fs::write(dir.path().join("models/load.sql"), model).unwrap();
    dir
}

fn arc_run(project: &Path) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .arg("run")
        .output()
        .expect("spawn arc run");
    assert!(
        out.status.success(),
        "arc run must exit 0 here — a run that reports success while redoing itself \
         is exactly what needs the warning (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_ordering_token_in_produces_reruns_its_step_and_names_it_on_every_run() {
    let dir = project(TOKEN_MANIFEST, TOKEN_MODEL);

    // Three runs, not two: a step that re-ran once and then went quiet, or a warning
    // that fired once and then stopped, would both pass a two-run check.
    for run in 1..=3 {
        let out = arc_run(dir.path());
        let err = stderr_of(&out);
        assert!(
            err.contains(WARNING),
            "run {run}: exited 0 with nothing at ./raw_tables and said nothing:\n{err}"
        );
        assert!(
            err.contains("raw_tables"),
            "run {run}: the warning must name the asset, not just complain:\n{err}"
        );
        let stdout = stdout_of(&out);
        assert!(
            !stdout.contains("skipped"),
            "run {run}: the step must actually re-run — a skip here is the silent \
             certification this trade exists to refuse:\n{stdout}"
        );
    }

    // The control: the same step declaring a `produces:` its own SQL writes settles
    // to a skip and is never warned about, so the runs above are this configuration
    // and not every configuration.
    let control = project(FILE_MANIFEST, FILE_MODEL);
    let first = arc_run(control.path());
    assert!(
        control.path().join("build/loaded.csv").is_file(),
        "the control's COPY must really have written build/loaded.csv"
    );
    assert!(
        !stderr_of(&first).contains(WARNING),
        "control run 1 produced its file and must not be warned about:\n{}",
        stderr_of(&first)
    );
    let second = arc_run(control.path());
    assert!(
        stdout_of(&second).contains("skipped"),
        "control run 2: a declared produces: that is on disk must settle to a skip:\n{}",
        stdout_of(&second)
    );
    assert!(
        !stderr_of(&second).contains(WARNING),
        "control run 2 must stay quiet:\n{}",
        stderr_of(&second)
    );
}
