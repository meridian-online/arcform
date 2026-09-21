//! How `arc run` finds the DuckDB it runs, and which DuckDB versions it refuses.
//!
//! `ARC_DUCKDB_BIN` names the engine; unset, arc runs the `duckdb` on PATH. Every test
//! here drives the built `arc` binary with PATH pointed at a directory the test made, so
//! "no duckdb on the search path" and "a different duckdb on the search path" are facts
//! of the test rather than of the machine running it.
//!
//! Most engines here are shell scripts that report a chosen version and append each
//! invocation's arguments to a log beside them, so a test can see which binary each of
//! arc's two calls, the `--version` preflight and the SQL step, actually reached. One
//! test hands arc a real DuckDB and reads back the table its SQL step wrote.
#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN_ENV: &str = "ARC_DUCKDB_BIN";
const ALLOW_ENV: &str = "ARC_ALLOW_UNTESTED_ENGINE";
const RANGE: &str = ">=1.2, <2";
const NAME: &str = "engine_contract";

/// A project directory plus somewhere to put engines and PATH directories.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    /// A one-step project whose step is `sql`, with `engine_version:` when given.
    fn new(engine_version: Option<&str>, sql: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir_all(project.join("models")).unwrap();
        let constraint = engine_version
            .map(|v| format!("engine_version: \"{v}\"\n"))
            .unwrap_or_default();
        fs::write(
            project.join("arcform.yaml"),
            format!("name: {NAME}\n{constraint}steps:\n  - name: s1\n    sql: models/s1.sql\n"),
        )
        .unwrap();
        fs::write(project.join("models/s1.sql"), sql).unwrap();
        Fixture { root }
    }

    fn project(&self) -> PathBuf {
        self.root.path().join("project")
    }

    /// A directory, created empty, to stand in for a search path with no DuckDB on it.
    fn dir(&self, name: &str) -> PathBuf {
        let d = self.root.path().join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake engine at `<dir>/<name>` reporting `version`, logging to `<dir>/<name>.log`.
    fn fake(&self, dir: &Path, name: &str, version: &str) -> FakeEngine {
        let bin = dir.join(name);
        let log = dir.join(format!("{name}.log"));
        // Builtins only: several tests run arc with a PATH that holds nothing else.
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$1\" = \"--version\" ]; then echo 'v{version} (fake) 0000000000'; fi\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        FakeEngine { bin, log }
    }

    /// `(steps_executed, outcome)` of every row in the run record.
    fn run_records(&self) -> Vec<(i64, String)> {
        let conn = duckdb::Connection::open(self.project().join(format!("{NAME}.duckdb"))).unwrap();
        let mut stmt = conn
            .prepare("SELECT steps_executed, outcome FROM _arcform_runs")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }
}

struct FakeEngine {
    bin: PathBuf,
    log: PathBuf,
}

impl FakeEngine {
    /// One line per invocation; empty when the engine was never run.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn version_calls(&self) -> usize {
        self.calls().iter().filter(|c| *c == "--version").count()
    }

    fn sql_calls(&self) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.contains(" -f ") && c.ends_with("s1.sql"))
            .count()
    }
}

/// How `arc run` is started: the search path, and the two variables under test.
struct Run<'a> {
    path: &'a Path,
    told: Option<&'a OsStr>,
    allow_untested: bool,
}

/// `arc run` in `project`, returning (exit code, stderr).
fn arc_run(project: &Path, how: Run) -> (Option<i32>, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arc"));
    cmd.current_dir(project)
        .arg("run")
        .env("PATH", how.path)
        .env_remove(BIN_ENV)
        .env_remove(ALLOW_ENV);
    if let Some(told) = how.told {
        cmd.env(BIN_ENV, told);
    }
    if how.allow_untested {
        cmd.env(ALLOW_ENV, "1");
    }
    let out = cmd.output().expect("spawn arc run");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The `duckdb` on the PATH this test process was started with.
fn real_duckdb() -> PathBuf {
    std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("duckdb"))
                .find(|c| c.is_file())
        })
        .expect(
            "this test hands arc a real DuckDB, and found no `duckdb` on PATH; \
             every SQL-step test in this suite needs one",
        )
}

// AC1: told, with no duckdb anywhere on the search path, the run completes and writes
// its run record. The first run, without the variable, proves the search path really is
// empty: it is the situation the variable exists for.
#[test]
fn a_told_engine_runs_with_no_duckdb_on_the_search_path() {
    let fx = Fixture::new(None, "SELECT 42;");
    let empty = fx.dir("empty-path");
    let told = fx.fake(&fx.dir("engine"), "told-duckdb", "1.5.4");

    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: None,
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(1), "no engine anywhere must refuse:\n{stderr}");
    assert!(
        stderr.contains("engine 'duckdb' not found"),
        "stderr: {stderr}"
    );

    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "told engine must run:\n{stderr}");
    assert_eq!(told.sql_calls(), 1, "calls: {:?}", told.calls());
    assert_eq!(fx.run_records(), vec![(1, "success".to_string())]);
}

// AC1 with a real DuckDB: the SQL step's table is in the database afterwards, so the told
// binary did the work, and the run record says the run succeeded.
#[test]
fn a_told_real_duckdb_writes_the_table_with_no_duckdb_on_the_search_path() {
    let fx = Fixture::new(None, "CREATE TABLE told AS SELECT 42 AS answer;");
    let empty = fx.dir("empty-path");
    let real = real_duckdb();

    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(real.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "told real duckdb must run:\n{stderr}");

    assert_eq!(fx.run_records(), vec![(1, "success".to_string())]);
    let conn = duckdb::Connection::open(fx.project().join(format!("{NAME}.duckdb"))).unwrap();
    let answer: i64 = conn
        .query_row("SELECT answer FROM told", [], |r| r.get(0))
        .unwrap();
    assert_eq!(answer, 42);
}

// AC2: unset, arc finds `duckdb` on the search path, as a command-line user expects.
#[test]
fn unset_arc_runs_the_duckdb_on_the_search_path() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let duckdb = fx.fake(&on_path, "duckdb", "1.5.4");

    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &on_path,
            told: None,
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "duckdb on PATH must run:\n{stderr}");
    assert_eq!(duckdb.version_calls(), 1, "calls: {:?}", duckdb.calls());
    assert_eq!(duckdb.sql_calls(), 1, "calls: {:?}", duckdb.calls());
}

// AC3: a told path that is not an executable file refuses the run, naming the variable
// and the path, and the working `duckdb` on the search path is never started.
#[test]
fn a_told_path_that_is_not_an_executable_file_is_refused_and_path_is_not_tried() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let decoy = fx.fake(&on_path, "duckdb", "1.5.4");

    let not_executable = fx.dir("engine").join("duckdb-no-x");
    fs::write(&not_executable, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&not_executable, fs::Permissions::from_mode(0o644)).unwrap();
    let missing = fx.root.path().join("nowhere/duckdb");
    let directory = fx.dir("a-directory");

    for (told, why) in [
        (missing, "No such file"),
        (not_executable, "not executable"),
        (directory, "not a file"),
        (PathBuf::new(), "is empty"),
    ] {
        let (code, stderr) = arc_run(
            &fx.project(),
            Run {
                path: &on_path,
                told: Some(told.as_os_str()),
                allow_untested: false,
            },
        );
        assert_eq!(code, Some(1), "{told:?} must be refused:\n{stderr}");
        assert!(
            stderr.contains(&format!("{BIN_ENV} names '{}'", told.display())),
            "{told:?}: the message must name the variable and the path:\n{stderr}"
        );
        assert!(
            stderr.contains(why),
            "{told:?}: expected '{why}':\n{stderr}"
        );
        assert!(
            decoy.calls().is_empty(),
            "{told:?}: the search path's duckdb ran: {:?}",
            decoy.calls()
        );
    }
}

// AC4: the version preflight and the SQL step both reach the told engine, and neither
// reaches the `duckdb` on the search path.
#[test]
fn preflight_and_sql_both_reach_the_told_engine() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let decoy = fx.fake(&on_path, "duckdb", "1.5.4");
    let told = fx.fake(&fx.dir("engine"), "told-duckdb", "1.5.4");

    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &on_path,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "told engine must run:\n{stderr}");
    assert_eq!(told.version_calls(), 1, "calls: {:?}", told.calls());
    assert_eq!(told.sql_calls(), 1, "calls: {:?}", told.calls());
    assert!(decoy.calls().is_empty(), "decoy ran: {:?}", decoy.calls());
}

// A relative value resolves against the working directory. Handed to the OS as a bare
// file name it would be a search-path lookup, and would start the decoy of the same name.
#[test]
fn a_relative_told_path_resolves_against_the_working_directory_not_path() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let decoy = fx.fake(&on_path, "told-duckdb", "1.5.4");
    let told = fx.fake(&fx.project(), "told-duckdb", "1.5.4");

    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &on_path,
            told: Some(OsStr::new("told-duckdb")),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "relative told engine must run:\n{stderr}");
    assert_eq!(told.version_calls(), 1, "calls: {:?}", told.calls());
    assert_eq!(told.sql_calls(), 1, "calls: {:?}", told.calls());
    assert!(decoy.calls().is_empty(), "decoy ran: {:?}", decoy.calls());
}

/// Run the fixture once on a fake engine reporting `version`, returning the exit code,
/// stderr and how many times the SQL step reached the engine.
fn run_on_version(
    engine_version: Option<&str>,
    version: &str,
    allow: bool,
) -> (Option<i32>, String, usize) {
    let fx = Fixture::new(engine_version, "SELECT 42;");
    let empty = fx.dir("empty-path");
    let told = fx.fake(&fx.dir("engine"), "told-duckdb", version);
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: allow,
        },
    );
    (code, stderr, told.sql_calls())
}

// AC5: with no `engine_version:`, an engine outside the tested range is refused before a
// step runs, and the message names the version, the range and the override.
#[test]
fn an_engine_outside_the_range_is_refused_before_a_step_runs() {
    let (code, stderr, sql) = run_on_version(None, "2.0.0", false);
    assert_eq!(code, Some(1), "2.0.0 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 2.0.0");
    for needle in ["2.0.0", RANGE, ALLOW_ENV] {
        assert!(stderr.contains(needle), "expected '{needle}':\n{stderr}");
    }

    let (code, stderr, sql) = run_on_version(None, "1.5.4", false);
    assert_eq!(code, Some(0), "1.5.4 must run:\n{stderr}");
    assert_eq!(sql, 1);
}

// AC6: a manifest's constraint narrows the range and does not replace it. `>=1.2`, what
// every manifest that states one says, does not switch the ceiling off.
#[test]
fn a_manifest_constraint_narrows_the_range_and_does_not_replace_it() {
    let (code, stderr, sql) = run_on_version(Some(">=1.2"), "2.0.0", false);
    assert_eq!(code, Some(1), ">=1.2 on 2.0.0 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 2.0.0");
    assert!(stderr.contains(RANGE), "stderr: {stderr}");

    let (code, stderr, sql) = run_on_version(Some(">=1.6"), "1.5.4", false);
    assert_eq!(code, Some(1), ">=1.6 on 1.5.4 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 1.5.4");
    assert!(
        stderr.contains("requires >=1.6, found 1.5.4"),
        "stderr: {stderr}"
    );
}

// AC7: the override runs an untested engine with a warning naming the version and the
// range, and lifts only arc's range: a manifest's own constraint still refuses.
#[test]
fn the_override_runs_an_untested_engine_with_a_warning_and_keeps_the_manifest_constraint() {
    let (code, stderr, sql) = run_on_version(None, "2.0.0", true);
    assert_eq!(code, Some(0), "override must run 2.0.0:\n{stderr}");
    assert_eq!(sql, 1);
    let warning = stderr
        .lines()
        .find(|l| l.contains("warning:"))
        .unwrap_or_else(|| panic!("no warning printed:\n{stderr}"));
    assert!(
        warning.contains("2.0.0") && warning.contains(RANGE),
        "the warning must name the version and the range: {warning}"
    );

    let (code, stderr, sql) = run_on_version(Some(">=1.6"), "1.5.4", true);
    assert_eq!(code, Some(1), "override must not lift >=1.6:\n{stderr}");
    assert_eq!(sql, 0);
    assert!(
        stderr.contains("requires >=1.6, found 1.5.4"),
        "stderr: {stderr}"
    );

    let (code, stderr, sql) = run_on_version(Some("<2"), "2.0.0", true);
    assert_eq!(code, Some(1), "override must not lift <2:\n{stderr}");
    assert_eq!(sql, 0);
    assert!(
        stderr.contains("requires <2, found 2.0.0"),
        "stderr: {stderr}"
    );
}
