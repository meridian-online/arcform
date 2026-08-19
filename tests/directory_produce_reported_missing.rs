//! End-to-end: a step that succeeds while a `Directory`-kind asset it declares
//! cannot be read says so, on every run, through the real `arc` binary.
//!
//! This is the one part of the staleness machinery whose only observable is stderr.
//! `produced_artifact_hash` returning `None` already forces the step to re-run — safe,
//! and silent — so the warning is the whole of what makes a run that keeps redoing
//! itself legible rather than merely expensive. Deleting the Directory arm of
//! `missing_declared_produces` changes nothing a step-count assertion can see, which
//! is why these tests spawn the binary and read its stderr instead of calling `run()`.
//!
//! Two ways a produced tree stops being hashable, because `state::hash_directory_
//! contents` gives up at a different `.ok()?` for each and one test cannot reach
//! both: a sub-directory that cannot be listed (the `read_dir` arm) and a single
//! child file whose bytes cannot be read (the `fs::read` arm). Replacing the second
//! with `unwrap_or_default()` leaves the whole suite green without the second test —
//! and a binary built that way settles into `[skip: hash_clean]` over a tree it
//! cannot read.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run`.

use std::path::Path;
use std::process::{Command, Output};

/// The manifest and model. `OVERWRITE_OR_IGNORE` makes DuckDB leave whatever is
/// already inside the partition directory alone, so the run below can keep a file of
/// its own in there across runs; `PARTITION_BY` is what makes `build/parts` a
/// directory rather than a file, read from the statement's own options by SQL
/// introspection.
const MANIFEST: &str =
    "name: parts_demo\nengine: duckdb\nsteps:\n  - name: export\n    sql: models/export.sql\n";
const MODEL: &str = "CREATE OR REPLACE TABLE orders AS SELECT 1 AS year, 'a' AS v;\n\
                     COPY orders TO 'build/parts' (FORMAT parquet, PARTITION_BY (year), OVERWRITE_OR_IGNORE);\n";

/// The substring the warning is built from, in `runner::run`.
const WARNING: &str = "does not appear to have produced";

fn arc_run(project: &Path) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .arg("run")
        .output()
        .expect("spawn arc run");
    assert!(
        out.status.success(),
        "arc run must exit 0 here — the whole point is that a run reporting success \
         is what needs the warning (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A protocol whose one step writes `build/parts` as a partitioned directory.
fn partitioned_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("models")).unwrap();
    std::fs::write(project.path().join("arcform.yaml"), MANIFEST).unwrap();
    std::fs::write(project.path().join("models/export.sql"), MODEL).unwrap();
    // DuckDB creates the partition directory itself but not its parent.
    std::fs::create_dir_all(project.path().join("build")).unwrap();
    project
}

#[test]
fn an_unreadable_produced_directory_is_named_on_every_run_that_succeeds_over_it() {
    let project = partitioned_project();

    // Run 1 — nothing wrong. The negative control: without it, a warning printed
    // unconditionally would satisfy every assertion below.
    let first = arc_run(project.path());
    let parts = project.path().join("build/parts");
    assert!(
        parts.is_dir(),
        "the partitioned COPY must really have written a directory at build/parts"
    );
    assert!(
        !stderr_of(&first).contains(WARNING),
        "run 1 produced its directory and must not be warned about:\n{}",
        stderr_of(&first)
    );

    // Make one subdirectory inside the produced tree unlistable. `arc` keeps writing
    // its own partitions beside it, so the step goes on succeeding while the tree it
    // is answerable for can no longer be hashed.
    let locked = parts.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    set_mode(&locked, 0o000);
    if std::fs::read_dir(&locked).is_ok() {
        // Running as a user that permission bits do not bind (root, or a filesystem
        // with no POSIX modes). Restore and stop rather than assert something this
        // environment cannot express.
        set_mode(&locked, 0o755);
        eprintln!(
            "skipping an_unreadable_produced_directory_is_named_on_every_run_that_succeeds_over_it: \
             mode 000 on {} still lists, so this environment cannot make a child unreadable",
            locked.display()
        );
        return;
    }

    // Runs 2 and 3 — the step succeeds, the run exits 0, and the warning names the
    // directory. Twice, because a warning that fires once and then goes quiet would
    // leave the second run of a perpetual re-run silent.
    for run in 2..=3 {
        let out = arc_run(project.path());
        let err = stderr_of(&out);
        assert!(
            err.contains(WARNING),
            "run {run}: the run exited 0 with build/parts unhashable and said nothing:\n{err}"
        );
        assert!(
            err.contains("build/parts"),
            "run {run}: the warning must name the asset, not just complain:\n{err}"
        );
    }

    // Repair it. The warning must stop, and the step must settle — so the warning
    // above was the damage and not something this manifest always says.
    set_mode(&locked, 0o755);
    std::fs::remove_dir(&locked).unwrap();

    let repaired = arc_run(project.path());
    assert!(
        !stderr_of(&repaired).contains(WARNING),
        "run 4: build/parts is readable again and must not still be warned about:\n{}",
        stderr_of(&repaired)
    );

    let settled = arc_run(project.path());
    let out = String::from_utf8_lossy(&settled.stdout);
    assert!(
        out.contains("skipped"),
        "run 5: an intact produced directory must settle to a skip, or the runs above \
         were re-running for some reason other than the one under test:\n{out}"
    );
}

#[test]
fn an_unreadable_child_file_inside_a_produced_directory_is_named_on_every_run() {
    let project = partitioned_project();

    // Run 1 — nothing wrong. The negative control, as above.
    let first = arc_run(project.path());
    let parts = project.path().join("build/parts");
    assert!(
        parts.is_dir(),
        "the partitioned COPY must really have written a directory at build/parts"
    );
    assert!(
        !stderr_of(&first).contains(WARNING),
        "run 1 produced its directory and must not be warned about:\n{}",
        stderr_of(&first)
    );

    // One CHILD FILE inside the produced tree, readable by nobody. The directory
    // still lists, every sub-directory still lists, and `entry.file_type()` still
    // answers — the only thing that fails is reading this file's bytes, which is
    // the arm an `unwrap_or_default()` swallows.
    let locked = parts.join("locked.bin");
    std::fs::write(&locked, b"bytes nobody can read").unwrap();
    set_mode(&locked, 0o000);
    if std::fs::read(&locked).is_ok() {
        // Running as a user that permission bits do not bind (root, or a filesystem
        // with no POSIX modes). Restore and stop rather than assert something this
        // environment cannot express.
        set_mode(&locked, 0o644);
        eprintln!(
            "skipping an_unreadable_child_file_inside_a_produced_directory_is_named_on_every_run: \
             mode 000 on {} still reads, so this environment cannot make a child file unreadable",
            locked.display()
        );
        return;
    }
    assert!(
        std::fs::read_dir(&parts).is_ok(),
        "build/parts itself must still LIST — otherwise this test is re-running the \
         unlistable-directory case and not the unreadable-child-file one"
    );

    // Runs 2 and 3 — the step succeeds, the run exits 0, and the warning names the
    // directory. Twice, because a warning that fires once and then goes quiet would
    // leave the second run of a perpetual re-run silent.
    for run in 2..=3 {
        let out = arc_run(project.path());
        let err = stderr_of(&out);
        assert!(
            err.contains(WARNING),
            "run {run}: the run exited 0 with one child of build/parts unreadable and \
             said nothing:\n{err}"
        );
        assert!(
            err.contains("build/parts"),
            "run {run}: the warning must name the asset, not just complain:\n{err}"
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.contains("skipped"),
            "run {run}: a tree with an unreadable child must not settle to a skip:\n{stdout}"
        );
    }

    // Repair it. The warning must stop, and the step must settle — so the warning
    // above was the damage and not something this manifest always says.
    set_mode(&locked, 0o644);
    std::fs::remove_file(&locked).unwrap();

    let repaired = arc_run(project.path());
    assert!(
        !stderr_of(&repaired).contains(WARNING),
        "run 4: every child of build/parts is readable again and must not still be \
         warned about:\n{}",
        stderr_of(&repaired)
    );

    let settled = arc_run(project.path());
    let out = String::from_utf8_lossy(&settled.stdout);
    assert!(
        out.contains("skipped"),
        "run 5: an intact produced directory must settle to a skip, or the runs above \
         were re-running for some reason other than the one under test:\n{out}"
    );
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}
