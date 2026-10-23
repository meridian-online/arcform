//! `arc run` says once, on standard error and before the first step, when the Protocol's
//! newest run record names a DuckDB whose major.minor differs from the engine about to run.
//!
//! The engine is a shell script that reports a chosen version and writes a marker to
//! standard error when it runs a step's file, so the order of the notice and the first step
//! is read from the one stream.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const STEP_MARKER: &str = "FAKE-STEP-RAN";
const NOTICE: &str = "this Protocol last ran on DuckDB 1.5.6, and this run uses DuckDB 2.0.0";
const FOUR: &str = "DuckDB 2.0 changes four results without an error";
const DEV: &str = "v2.0.0-dev88614 (Development Version) d54cbbe3df";

fn real_duckdb() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("duckdb"))
                .find(|c| c.is_file())
        })
        .expect("this test hands arc a real DuckDB for token questions, and found none on PATH")
}

/// A Protocol with one SQL step, a fake 2.0.0 engine, and the project's `runs` directory.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir_all(project.join("models")).unwrap();
        fs::write(project.join("models/s1.sql"), "SELECT 42;").unwrap();
        fs::write(
            project.join("arcform.yaml"),
            "name: notice\nsteps:\n  - name: s1\n    sql: models/s1.sql\n",
        )
        .unwrap();
        let bin = root.path().join("engine/duckdb");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"-no-init\" ]; then exec '{}' \"$@\"; fi\n\
                 if [ \"$1\" = \"--version\" ]; then echo 'v2.0.0 (fake) 0000000000'; \
                 else echo {STEP_MARKER} >&2; fi\nexit 0\n",
                real_duckdb().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        Fixture { root }
    }

    fn project(&self) -> PathBuf {
        self.root.path().join("project")
    }

    /// Seed a run record naming `duckdb_cli`.
    fn seed(&self, duckdb_cli: &str) {
        let runs = self.project().join("build/.arcform/runs");
        fs::create_dir_all(&runs).unwrap();
        let record = serde_json::json!({
            "contract_version": "b4/1",
            "run": {
                "run_id": "seed", "attempt_id": "seed",
                "protocol": {"name": "notice", "manifest_sha256": null, "dir": "."},
                "engine": {"arc": "0", "duckdb": null, "duckdb_cli": duckdb_cli,
                           "platform": null, "extensions": null},
                "params": [], "started_at": null, "finished_at": null, "outcome": "success"
            },
            "assets": [], "steps": []
        });
        fs::write(runs.join("seed.json"), record.to_string()).unwrap();
    }

    /// `arc run`, returning (stdout, stderr).
    fn run(&self) -> (String, String) {
        let engine: &Path = &self.root.path().join("engine/duckdb");
        let out = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(self.project())
            .env("ARCFORM_DB_DIR", self.root.path().join("db"))
            .env("ARC_DUCKDB_BIN", engine)
            .env("DUCKDB_AGENT_MODE", "0")
            .arg("run")
            .output()
            .expect("spawn arc run");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(0), "run failed:\n{stderr}");
        (String::from_utf8_lossy(&out.stdout).into_owned(), stderr)
    }
}

#[test]
fn a_1_x_record_and_a_2_0_engine_print_the_notice_on_stderr_before_the_first_step() {
    let fx = Fixture::new();
    fx.seed("v1.5.6");
    let (stdout, stderr) = fx.run();
    let notice = stderr
        .find(NOTICE)
        .unwrap_or_else(|| panic!("no notice:\n{stderr}"));
    let four = stderr
        .find(FOUR)
        .unwrap_or_else(|| panic!("no four changes:\n{stderr}"));
    let step = stderr
        .find(STEP_MARKER)
        .unwrap_or_else(|| panic!("step never ran:\n{stderr}"));
    assert!(notice < four && four < step, "order:\n{stderr}");
    assert_eq!(stderr.matches(NOTICE).count(), 1, "once:\n{stderr}");
    assert!(
        !stdout.contains("last ran on DuckDB") && !stdout.contains(FOUR),
        "{stdout}"
    );
}

#[test]
fn no_record_prints_no_notice() {
    let fx = Fixture::new();
    let (stdout, stderr) = fx.run();
    assert!(!stderr.contains("last ran on DuckDB") && !stdout.contains("last ran on DuckDB"));
}

#[test]
fn a_record_on_a_dev_build_of_the_same_release_prints_no_notice() {
    let fx = Fixture::new();
    fx.seed(DEV);
    let (stdout, stderr) = fx.run();
    assert!(!stderr.contains("last ran on DuckDB") && !stdout.contains("last ran on DuckDB"));
}
