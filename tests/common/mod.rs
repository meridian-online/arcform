//! Helpers shared by the integration tests that drive the real `arc` binary.
//!
//! A subdirectory under `tests/`, so cargo compiles it into each test binary rather
//! than building it as a test target of its own.

use std::path::Path;
use std::process::Command;

/// Drop ANSI styling so a step line can be read as text. `arc` colours
/// unconditionally, including when its stdout is a pipe.
pub fn strip_ansi(s: &str) -> String {
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
pub fn step_outcome(stdout: &str, step: &str) -> String {
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
pub fn arc_run_raw(project: &Path) -> (Option<i32>, String, String) {
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

/// One `arc run` that has to succeed.
pub fn arc_run(project: &Path) -> String {
    let (code, stdout, stderr) = arc_run_raw(project);
    assert_eq!(
        code,
        Some(0),
        "arc run must exit 0 here:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    stdout
}
