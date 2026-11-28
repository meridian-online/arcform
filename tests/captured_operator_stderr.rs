//! End-to-end: what a capturing operator's child writes to stderr reaches the person
//! running `arc run` when the step succeeds, not only when it fails.
//!
//! `text_embed` runs its script through `uv` with its output captured, so its stderr is
//! held until the child exits. A fake `uv` first on PATH stands in for the script: it
//! writes the operator's output file, a diagnostic line to stderr, and exits 0. The
//! test reads the real `arc` binary's stderr for that line.
//!
//! Unix only, for the executable shell script. Needs a `duckdb` CLI on PATH, the same
//! requirement as any `arc run`.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const MANIFEST: &str = "name: embed_demo\nengine: duckdb\nsteps:\n  - name: embed\n    op: text_embed@1\n    with:\n      input: build/corpus.parquet\n      text_column: description\n      extension: vendor/subtoken.duckdb_extension\n      out: build/embedded.parquet\n";

const DIAGNOSTIC: &str = "text_embed: 2 of 5 rows embedded as zero vectors";

/// A `uv` that writes the last argument following `--out` and says `DIAGNOSTIC` on stderr.
fn fake_uv(bin: &Path, exit: i32) {
    let script = format!(
        "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = --out ]; then out=\"$2\"; fi\n  shift\ndone\n[ -n \"$out\" ] && : > \"$out\"\necho '{DIAGNOSTIC}' >&2\nexit {exit}\n"
    );
    let uv = bin.join("uv");
    std::fs::write(&uv, script).unwrap();
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn project() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let p = project.path();
    std::fs::write(p.join("arcform.yaml"), MANIFEST).unwrap();
    std::fs::create_dir_all(p.join("build")).unwrap();
    std::fs::write(p.join("build/corpus.parquet"), b"stand-in corpus").unwrap();
    std::fs::create_dir_all(p.join("vendor")).unwrap();
    std::fs::write(p.join("vendor/subtoken.duckdb_extension"), b"stand-in").unwrap();
    std::fs::create_dir_all(p.join("bin")).unwrap();
    project
}

fn arc_run(project: &Path) -> std::process::Output {
    let path = format!(
        "{}:{}",
        project.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .env("PATH", path)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .arg("run")
        .output()
        .expect("spawn arc run")
}

#[test]
fn a_successful_capturing_step_shows_what_its_child_wrote_to_stderr() {
    let project = project();
    fake_uv(&project.path().join("bin"), 0);

    let out = arc_run(project.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the step's child exited 0, so the run must too (code {:?}):\nstdout:\n{}\nstderr:\n{stderr}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
    );
    assert!(
        stderr.contains(DIAGNOSTIC),
        "the child's stderr must reach the terminal on success; got:\n{stderr}"
    );
    assert_eq!(
        stderr.matches(DIAGNOSTIC).count(),
        1,
        "the line is shown once; got:\n{stderr}"
    );
}

#[test]
fn a_failing_capturing_step_still_shows_its_stderr_in_the_error() {
    // The control: the line already reached the user through the failure's error, and
    // passing it on at success must not have moved it from there.
    let project = project();
    std::fs::write(
        project.path().join("arcform.yaml"),
        MANIFEST.replace(
            "steps:",
            "defaults:\n  retry:\n    max_attempts: 1\n    backoff_sec: 0\nsteps:",
        ),
    )
    .unwrap();
    fake_uv(&project.path().join("bin"), 3);

    let out = arc_run(project.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a child exiting 3 fails the run");
    assert!(
        stderr.contains(DIAGNOSTIC),
        "a failure's error carries the child's stderr; got:\n{stderr}"
    );
}
