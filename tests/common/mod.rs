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
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
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

/// Start `arc mcp` in `dir`, write each request on its own line, close stdin, and
/// return every line the server wrote to standard output, unfiltered, so a caller can
/// check that each one is a JSON-RPC message.
///
/// Only the tests built with the `mcp` feature call it, and the others include this
/// module too.
#[allow(dead_code)]
pub fn mcp_session(dir: &Path, requests: &[serde_json::Value]) -> Vec<String> {
    mcp_session_env(dir, requests, &[])
}

/// [`mcp_session`], with `env` set on the spawned server.
#[allow(dead_code)]
pub fn mcp_session_env(
    dir: &Path,
    requests: &[serde_json::Value],
    env: &[(&str, &Path)],
) -> Vec<String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .envs(env.iter().copied())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        for request in requests {
            writeln!(stdin, "{request}").expect("write a request");
        }
        // Dropping stdin closes it, which ends the server's read loop.
    }
    let out = child.wait_with_output().expect("wait for arc mcp");
    assert!(
        out.status.success(),
        "arc mcp failed (code {:?}):\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The `finetype` binary arc runs: `$FINETYPE_BIN` if set, else `finetype` on PATH.
#[allow(dead_code)]
pub fn finetype_bin() -> String {
    std::env::var("FINETYPE_BIN").unwrap_or_else(|_| "finetype".into())
}

/// arc's minimum finetype version, read from `MIN_VERSION` in `src/mcp/finetype.rs`.
#[allow(dead_code)]
pub fn finetype_floor() -> semver::Version {
    let src =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/finetype.rs"))
            .unwrap();
    let c = regex::Regex::new(r"MIN_VERSION: \(u64, u64, u64\) = \((\d+), (\d+), (\d+)\)")
        .unwrap()
        .captures(&src)
        .expect("MIN_VERSION in src/mcp/finetype.rs");
    semver::Version::new(
        c[1].parse().unwrap(),
        c[2].parse().unwrap(),
        c[3].parse().unwrap(),
    )
}

/// Why the finetype arc would run cannot be used, or None if it is at the floor.
#[allow(dead_code)]
pub fn finetype_unavailable() -> Option<String> {
    let bin = finetype_bin();
    let out = match Command::new(&bin).arg("--version").output() {
        Ok(o) => o,
        Err(_) => return Some(format!("`{bin}` is not installed")),
    };
    // clap prints the version to stdout; fall back to stderr as `detect_version` does.
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).into_owned()
    } else {
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let token = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().next_back());
    let Some(found) = token.and_then(|t| semver::Version::parse(t.trim_start_matches('v')).ok())
    else {
        return Some(format!(
            "could not parse a finetype version from `{}`",
            text.trim()
        ));
    };
    let floor = finetype_floor();
    if found >= floor {
        None
    } else {
        Some(format!(
            "`{bin}` is {found}, older than arc's floor {floor}"
        ))
    }
}

/// Whether the finetype arc would run is at its floor, printing why when it is not.
///
/// A test that needs the real CLI returns early when this is false. With
/// `ARCFORM_REQUIRE_FINETYPE_CLI` set, as CI sets it after installing a pinned
/// release, a missing or too-old binary panics instead, so those tests cannot pass
/// by skipping.
#[allow(dead_code)]
pub fn finetype_cli_present() -> bool {
    let Some(why) = finetype_unavailable() else {
        return true;
    };
    assert!(
        std::env::var_os("ARCFORM_REQUIRE_FINETYPE_CLI").is_none(),
        "ARCFORM_REQUIRE_FINETYPE_CLI is set, so this run promised the finetype CLI, but {why}"
    );
    eprintln!("skipping: {why}");
    false
}
