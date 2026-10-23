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
const RANGE: &str = ">=2.0, <3";
const OLDEST: &str = "2.0.0";
const NAME: &str = "engine_contract";
/// The command arc names for installing DuckDB's release build.
const INSTALLER: &str = "curl https://install.duckdb.org | DUCKDB_VERSION=2.0.0 sh";
/// What Homebrew's DuckDB 2.0.0, built without the autocomplete extension, says when it is
/// asked for `sql_tokenize`.
const NO_AUTOCOMPLETE: &str = "Extension Autoloading Error: An error occurred while trying to \
    automatically install the required extension 'autocomplete': Failed to install local \
    extension \"autocomplete\", no access to the file at PATH \
    \"/tmp/build/repository/d54cbbe3df/osx_arm64/autocomplete.duckdb_extension\"";

/// A project directory plus somewhere to put engines and PATH directories.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    /// A one-step project whose step is `sql`, with `engine_version:` when given.
    fn new(engine_version: Option<&str>, sql: &str) -> Self {
        Self::with_steps(engine_version, &[sql])
    }

    /// A project of SQL steps `s1`, `s2`, … in the order given, each running its `sql`.
    fn with_steps(engine_version: Option<&str>, sqls: &[&str]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir_all(project.join("models")).unwrap();
        let constraint = engine_version
            .map(|v| format!("engine_version: \"{v}\"\n"))
            .unwrap_or_default();
        let mut steps = String::new();
        for (i, sql) in sqls.iter().enumerate() {
            let step = format!("s{}", i + 1);
            steps.push_str(&format!("  - name: {step}\n    sql: models/{step}.sql\n"));
            fs::write(project.join(format!("models/{step}.sql")), sql).unwrap();
        }
        fs::write(
            project.join("arcform.yaml"),
            format!("name: {NAME}\n{constraint}steps:\n{steps}"),
        )
        .unwrap();
        Fixture { root }
    }

    fn project(&self) -> PathBuf {
        self.root.path().join("project")
    }

    /// The project's database: it names no `db:`, so arc keeps `<name>.duckdb` in the
    /// one keyed directory of the data folder `arc_run` points it at.
    fn database(&self) -> PathBuf {
        let keys: Vec<PathBuf> = fs::read_dir(self.root.path().join("db"))
            .unwrap()
            .map(|key| key.unwrap().path())
            .collect();
        assert_eq!(keys.len(), 1, "one Protocol, one keyed directory: {keys:?}");
        keys[0].join(format!("{NAME}.duckdb"))
    }

    /// A directory, created empty, to stand in for a search path with no DuckDB on it.
    fn dir(&self, name: &str) -> PathBuf {
        let d = self.root.path().join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake engine at `<dir>/<name>` reporting `version`, logging to `<dir>/<name>.log`.
    fn fake(&self, dir: &Path, name: &str, version: &str) -> FakeEngine {
        self.fake_printing(dir, name, &format!("v{version} (fake) 0000000000"))
    }

    /// A fake engine whose `--version` prints `line` exactly.
    fn fake_printing(&self, dir: &Path, name: &str, line: &str) -> FakeEngine {
        let bin = dir.join(name);
        let log = dir.join(format!("{name}.log"));
        // Builtins only: several tests run arc with a PATH that holds nothing else.
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\n{}echo \"$*\" >> '{}'\nif [ \"$1\" = \"--version\" ]; then echo '{line}'; fi\nexit 0\n",
                tokens_from_real_duckdb(),
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        FakeEngine { bin, log }
    }

    /// A fake engine whose `--version` prints `line` and which, like a DuckDB built without
    /// its autocomplete extension, fails every question arc asks it on standard input
    /// (`-no-init`, as the token questions and the tokenizer probe are asked) with the error
    /// such a build gives for `sql_tokenize`.
    fn fake_without_tokenizer(&self, dir: &Path, name: &str, line: &str) -> FakeEngine {
        let bin = dir.join(name);
        let log = dir.join(format!("{name}.log"));
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$1\" = \"--version\" ]; then echo '{line}'; exit 0; fi\n\
                 if [ \"$1\" = \"-no-init\" ]; then echo 'Error: {}' >&2; exit 1; fi\nexit 0\n",
                log.display(),
                NO_AUTOCOMPLETE.replace('\'', ""),
            ),
        )
        .unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        FakeEngine { bin, log }
    }

    /// `(steps_executed, outcome)` of every row in the run record.
    fn run_records(&self) -> Vec<(i64, String)> {
        let conn = duckdb::Connection::open(self.database()).unwrap();
        let mut stmt = conn
            .prepare("SELECT steps_executed, outcome FROM _arcform_runs")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }
}

/// The line a fake engine opens with: it hands a question for a SQL file's tokens, which
/// arc asks with `-no-init` first, to [`real_duckdb`] by its path, unlogged, so what the fake logs is
/// still the calls a run makes of its own engine.
fn tokens_from_real_duckdb() -> String {
    format!(
        "if [ \"$1\" = \"-no-init\" ]; then exec '{}' \"$@\"; fi\n",
        real_duckdb().display()
    )
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

    /// The calls that ran the step's file: `<db> -f <file>`, and the question arc asks after
    /// it of the extensions the step loaded.
    fn sql_calls(&self) -> usize {
        self.calls()
            .iter()
            .filter_map(|c| c.split(" -f ").nth(1))
            .filter(|rest| {
                rest.split(' ')
                    .next()
                    .is_some_and(|f| f.ends_with("s1.sql"))
            })
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
    // The project names no `db:`, so its database is in the data folder beside it.
    cmd.current_dir(project)
        .env("ARCFORM_DB_DIR", project.with_file_name("db"))
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

// Told, with no duckdb anywhere on the search path, the run completes and writes
// its run record. The first run, without the variable, proves the search path really is
// empty: it is the situation the variable exists for.
#[test]
fn a_told_engine_runs_with_no_duckdb_on_the_search_path() {
    let fx = Fixture::new(None, "SELECT 42;");
    let empty = fx.dir("empty-path");
    let told = fx.fake(&fx.dir("engine"), "told-duckdb", "2.0.0");

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

// Told a real DuckDB, with no duckdb on the search path: the SQL step's table is in the database afterwards, so the told
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
    let conn = duckdb::Connection::open(fx.database()).unwrap();
    let answer: i64 = conn
        .query_row("SELECT answer FROM told", [], |r| r.get(0))
        .unwrap();
    assert_eq!(answer, 42);
}

// Unset, arc finds `duckdb` on the search path, as a command-line user expects.
#[test]
fn unset_arc_runs_the_duckdb_on_the_search_path() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let duckdb = fx.fake(&on_path, "duckdb", "2.0.0");

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

// A Protocol cannot change which DuckDB runs its SQL step: what arc adds to a step's
// environment is named `ARC_…`, so a parameter called `PATH` reaches the step as
// `ARC_PARAM_PATH`. The step runs the `duckdb` arc's own search path finds, the one arc asked
// for the version and the file's tokens, so the refusals read the file as the DuckDB that
// runs it reads it.
#[test]
fn a_parameter_called_path_does_not_change_which_duckdb_runs_the_step() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let duckdb = fx.fake(&on_path, "duckdb", "2.0.0");
    let elsewhere = fx.dir("elsewhere");
    let other = fx.fake(&elsewhere, "duckdb", "2.0.0");
    let manifest = fx.project().join("arcform.yaml");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(
        &manifest,
        text.replace(
            "steps:\n",
            &format!(
                "params:\n  PATH:\n    default: \"{}\"\nsteps:\n",
                elsewhere.display()
            ),
        ),
    )
    .unwrap();

    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &on_path,
            told: None,
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "the step must run:\n{stderr}");
    assert_eq!(duckdb.sql_calls(), 1, "calls: {:?}", duckdb.calls());
    assert_eq!(other.calls(), Vec::<String>::new());
}

// A told path that is not an executable file refuses the run, naming the variable
// and the path, and the working `duckdb` on the search path is never started.
#[test]
fn a_told_path_that_is_not_an_executable_file_is_refused_and_path_is_not_tried() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let decoy = fx.fake(&on_path, "duckdb", "2.0.0");

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

// The version preflight and the SQL step both reach the told engine, and neither
// reaches the `duckdb` on the search path.
#[test]
fn preflight_and_sql_both_reach_the_told_engine() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let decoy = fx.fake(&on_path, "duckdb", "2.0.0");
    let told = fx.fake(&fx.dir("engine"), "told-duckdb", "2.0.0");

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
    let decoy = fx.fake(&on_path, "told-duckdb", "2.0.0");
    let told = fx.fake(&fx.project(), "told-duckdb", "2.0.0");

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

// With no `engine_version:`, an engine outside the tested range is refused before a
// step runs, and the message names the version, the range and the override.
#[test]
fn an_engine_outside_the_range_is_refused_before_a_step_runs() {
    let (code, stderr, sql) = run_on_version(None, "3.0.0", false);
    assert_eq!(code, Some(1), "3.0.0 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 3.0.0");
    for needle in ["3.0.0", RANGE, ALLOW_ENV] {
        assert!(stderr.contains(needle), "expected '{needle}':\n{stderr}");
    }
}

#[test]
fn a_2_0_0_engine_runs() {
    let (code, stderr, sql) = run_on_version(None, OLDEST, false);
    assert_eq!(code, Some(0), "{OLDEST} must run:\n{stderr}");
    assert_eq!(sql, 1);
    assert!(!stderr.contains("warning:"), "stderr: {stderr}");
    assert!(!stderr.contains(RANGE), "stderr: {stderr}");
}

#[test]
fn a_2_7_3_engine_runs() {
    let (code, stderr, sql) = run_on_version(None, "2.7.3", false);
    assert_eq!(code, Some(0), "2.7.3 must run:\n{stderr}");
    assert_eq!(sql, 1);
    assert!(!stderr.contains("warning:"), "stderr: {stderr}");
}

// A 1.x engine cannot open a database the linked 2.0 library shares with it, so it is
// refused, and the message names the release of arc that is for DuckDB 1.5.
#[test]
fn a_1_5_6_engine_is_refused_as_too_old_naming_0_2_1() {
    let (code, stderr, sql) = run_on_version(None, "1.5.6", false);
    assert_eq!(code, Some(1), "1.5.6 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 1.5.6");
    for needle in ["1.5.6", RANGE, "0.2.1"] {
        assert!(stderr.contains(needle), "expected '{needle}':\n{stderr}");
    }
}

#[test]
fn a_1_5_6_engine_is_refused_even_with_the_override_set() {
    let (code, stderr, sql) = run_on_version(None, "1.5.6", true);
    assert_eq!(code, Some(1), "1.5.6 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 1.5.6");
    for needle in ["1.5.6", RANGE, "0.2.1"] {
        assert!(stderr.contains(needle), "expected '{needle}':\n{stderr}");
    }
}

#[test]
fn a_3_0_0_engine_is_untested_and_the_override_lifts_it() {
    let (code, stderr, sql) = run_on_version(None, "3.0.0", false);
    assert_eq!(code, Some(1), "3.0.0 must be refused:\n{stderr}");
    assert_eq!(sql, 0);
    assert!(stderr.contains("outside the versions"), "stderr: {stderr}");

    let (code, stderr, sql) = run_on_version(None, "3.0.0", true);
    assert_eq!(code, Some(0), "override must run 3.0.0:\n{stderr}");
    assert_eq!(sql, 1);
}

#[test]
fn a_2_0_0_pre_release_is_untested() {
    let (code, stderr, sql) = run_on_version(None, "2.0.0-alpha48730", false);
    assert_eq!(code, Some(1), "a pre-release must be refused:\n{stderr}");
    assert_eq!(sql, 0);
    assert!(stderr.contains("2.0.0-alpha48730"), "stderr: {stderr}");
}

/// What Homebrew's DuckDB 2.0.0, built from the v2.0.0 commit, prints for `--version`.
const HOMEBREW_2_0_0: &str = "v2.0.0-dev88614 (Development Version) d54cbbe3df";

// Homebrew's build of the v2.0.0 commit calls itself a dev build, and is v2.0.0: arc's range
// and a Protocol's `engine_version: ">=1.0"` both take it as that release.
#[test]
fn homebrews_dev_build_of_the_2_0_0_commit_runs_as_2_0_0() {
    let fx = Fixture::new(Some(">=1.0"), "SELECT 42;");
    let empty = fx.dir("empty-path");
    let told = fx.fake_printing(&fx.dir("engine"), "told-duckdb", HOMEBREW_2_0_0);
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "the dev build must run:\n{stderr}");
    assert_eq!(told.sql_calls(), 1);
    assert!(!stderr.contains("warning:"), "stderr: {stderr}");
}

#[test]
fn a_dev_build_of_another_commit_is_still_untested() {
    let fx = Fixture::new(None, "SELECT 42;");
    let empty = fx.dir("empty-path");
    let told = fx.fake_printing(
        &fx.dir("engine"),
        "told-duckdb",
        "v2.0.0-dev88614 (Development Version) 0123456789",
    );
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(
        code,
        Some(1),
        "an unknown dev build must be refused:\n{stderr}"
    );
    assert_eq!(told.sql_calls(), 0);
    assert!(stderr.contains("2.0.0-dev88614"), "stderr: {stderr}");
}

/// A project whose one step installs a vetted community extension, run on a fake engine
/// printing `line`; returns stderr.
fn community_install_stderr(line: &str) -> String {
    let fx = Fixture::new(None, "INSTALL zipfs FROM community;\nSELECT 42;");
    let empty = fx.dir("empty-path");
    let told = fx.fake_printing(&fx.dir("engine"), "told-duckdb", line);
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "the run must proceed:\n{stderr}");
    assert_eq!(told.sql_calls(), 1);
    stderr
}

#[test]
fn a_release_build_of_a_protocol_installing_an_extension_draws_no_dev_build_warning() {
    let stderr = community_install_stderr("v2.0.0 (Cyanoptera) d54cbbe3df");
    assert!(!stderr.contains("community extensions"), "stderr: {stderr}");
}

#[test]
fn a_dev_build_running_a_protocol_that_installs_a_community_extension_warns_before_the_first_step()
{
    let stderr = community_install_stderr(HOMEBREW_2_0_0);
    let warning = stderr
        .lines()
        .find(|l| l.contains("cannot download community extensions"))
        .unwrap_or_else(|| panic!("no dev-build warning:\n{stderr}"));
    for needle in ["v2.0.0-dev88614", INSTALLER] {
        assert!(warning.contains(needle), "expected '{needle}': {warning}");
    }
    assert_eq!(
        stderr
            .matches("cannot download community extensions")
            .count(),
        1,
        "stderr: {stderr}"
    );
}

// A manifest asking for a range that starts below the oldest release still loads, and runs
// on an engine arc accepts; on a 1.x engine, arc's own range refuses it, not the manifest's.
#[test]
fn a_manifest_that_starts_below_the_oldest_release_loads_and_does_not_widen_the_range() {
    let (code, stderr, sql) = run_on_version(Some(">=1.2"), "2.0.0", false);
    assert_eq!(code, Some(0), ">=1.2 on 2.0.0 must run:\n{stderr}");
    assert_eq!(sql, 1);

    let (code, stderr, sql) = run_on_version(Some(">=1.2"), "1.5.6", false);
    assert_eq!(code, Some(1), ">=1.2 on 1.5.6 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 1.5.6");
    assert!(
        stderr.contains("DuckDB 1.5.6 is older than") && stderr.contains(OLDEST),
        "arc's range must be the refusal: {stderr}"
    );
}

// On a real DuckDB, a run whose first SQL step's statement fails exits non-zero, records the
// run as failed, and stops before the second step: arc reads a step's outcome from the
// command line's exit status alone. The DuckDB is the one on PATH, handed to arc by
// `ARC_DUCKDB_BIN`; put another release's directory first on PATH to run this on it.
#[test]
fn a_failed_statement_on_a_real_duckdb_fails_the_run_and_stops_it() {
    let fx = Fixture::with_steps(
        None,
        &[
            "CREATE TABLE first AS SELECT * FROM no_such_table;",
            "CREATE TABLE second AS SELECT 2 AS x;",
        ],
    );
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
    assert_eq!(
        code,
        Some(2),
        "a failed SQL step must fail the run:\n{stderr}"
    );
    assert!(stderr.contains("no_such_table"), "stderr: {stderr}");

    assert_eq!(fx.run_records(), vec![(1, "failed".to_string())]);
    let conn = duckdb::Connection::open(fx.database()).unwrap();
    let second: i64 = conn
        .query_row(
            "SELECT count(*) FROM information_schema.tables WHERE table_name = 'second'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(second, 0, "the second step ran after the first failed");
}

// A manifest's constraint narrows the range and does not replace it. `>=1.2` does not
// switch the ceiling off.
#[test]
fn a_manifest_constraint_narrows_the_range_and_does_not_replace_it() {
    let (code, stderr, sql) = run_on_version(Some(">=1.2"), "3.0.0", false);
    assert_eq!(code, Some(1), ">=1.2 on 3.0.0 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 3.0.0");
    assert!(stderr.contains(RANGE), "stderr: {stderr}");

    let (code, stderr, sql) = run_on_version(Some(">=2.1"), "2.0.0", false);
    assert_eq!(code, Some(1), ">=2.1 on 2.0.0 must be refused:\n{stderr}");
    assert_eq!(sql, 0, "a step ran on 2.0.0");
    assert!(
        stderr.contains("requires >=2.1, found 2.0.0"),
        "stderr: {stderr}"
    );
}

// The override runs an untested engine with a warning naming the version and the
// range, and lifts only arc's range: a manifest's own constraint still refuses.
#[test]
fn the_override_runs_an_untested_engine_with_a_warning_and_keeps_the_manifest_constraint() {
    let (code, stderr, sql) = run_on_version(None, "3.0.0", true);
    assert_eq!(code, Some(0), "override must run 3.0.0:\n{stderr}");
    assert_eq!(sql, 1);
    let warning = stderr
        .lines()
        .find(|l| l.contains("warning:"))
        .unwrap_or_else(|| panic!("no warning printed:\n{stderr}"));
    assert!(
        warning.contains("3.0.0") && warning.contains(RANGE),
        "the warning must name the version and the range: {warning}"
    );

    let (code, stderr, sql) = run_on_version(Some(">=2.1"), "2.0.0", true);
    assert_eq!(code, Some(1), "override must not lift >=2.1:\n{stderr}");
    assert_eq!(sql, 0);
    assert!(
        stderr.contains("requires >=2.1, found 2.0.0"),
        "stderr: {stderr}"
    );

    let (code, stderr, sql) = run_on_version(Some("<3"), "3.0.0", true);
    assert_eq!(code, Some(1), "override must not lift <3:\n{stderr}");
    assert_eq!(sql, 0);
    assert!(
        stderr.contains("requires <3, found 3.0.0"),
        "stderr: {stderr}"
    );
}

/// The refusal of a run on a DuckDB that cannot read SQL into tokens, checked for what it
/// must say and must not.
fn assert_no_tokenizer_refusal(stderr: &str, names: &str) {
    for needle in [
        names,
        "which prints `v2.0.0-dev88614 (Development Version) d54cbbe3df`, cannot read",
        "cannot read SQL into tokens",
        "autocomplete extension",
        "check what each SQL step installs",
        INSTALLER,
        "~/.duckdb/cli/2.0.0",
        "ARC_DUCKDB_BIN=~/.duckdb/cli/2.0.0/duckdb",
        "No step was run.",
    ] {
        assert!(stderr.contains(needle), "expected {needle:?}:\n{stderr}");
    }
    assert!(
        !stderr.contains("could not read the tokens"),
        "the old wording: {stderr}"
    );
    assert_eq!(
        stderr.matches("cannot read SQL into tokens").count(),
        1,
        "one message: {stderr}"
    );
}

// A DuckDB built without its autocomplete extension, as Homebrew's 2.0.0 is, has no
// `sql_tokenize`, and arc reads every SQL step's tokens from it. The run is refused before a
// step runs, naming the DuckDB, why, and DuckDB's own installer as the way out.
#[test]
fn a_duckdb_without_the_tokenizer_is_refused_naming_the_installer() {
    let fx = Fixture::with_steps(None, &["SELECT 42;", "SELECT 43;"]);
    let empty = fx.dir("empty-path");
    let told = fx.fake_without_tokenizer(&fx.dir("engine"), "told-duckdb", HOMEBREW_2_0_0);
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(2), "refused:\n{stderr}");
    assert_eq!(told.sql_calls(), 0, "calls: {:?}", told.calls());
    assert!(
        !told.calls().iter().any(|c| c.contains(" -f ")),
        "a step ran: {:?}",
        told.calls()
    );
    assert_no_tokenizer_refusal(&stderr, &told.bin.display().to_string());
}

// The `duckdb` on PATH is named by the file the search path finds.
#[test]
fn a_duckdb_on_path_without_the_tokenizer_is_refused_naming_where_it_is() {
    let fx = Fixture::new(None, "SELECT 42;");
    let on_path = fx.dir("path");
    let engine = fx.fake_without_tokenizer(&on_path, "duckdb", HOMEBREW_2_0_0);
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &on_path,
            told: None,
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(2), "refused:\n{stderr}");
    assert_eq!(engine.sql_calls(), 0, "calls: {:?}", engine.calls());
    assert_no_tokenizer_refusal(&stderr, &engine.bin.display().to_string());
}

// A Protocol whose SQL holds nothing to tokenise before the run, such as a step whose file
// an earlier step writes, is still checked before its first step: the probe asks DuckDB for
// the tokens of `SELECT 1`.
#[test]
fn a_protocol_with_no_sql_to_read_yet_is_still_refused_before_a_step_runs() {
    let fx = Fixture::new(None, "");
    let empty = fx.dir("empty-path");
    let told = fx.fake_without_tokenizer(&fx.dir("engine"), "told-duckdb", HOMEBREW_2_0_0);
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(2), "refused:\n{stderr}");
    assert_eq!(told.sql_calls(), 0, "calls: {:?}", told.calls());
    assert_no_tokenizer_refusal(&stderr, &told.bin.display().to_string());
}

// On a DuckDB whose tokenizer answers, the same empty file runs: the probe passes.
#[test]
fn a_protocol_with_no_sql_to_read_yet_runs_on_a_duckdb_that_tokenises() {
    let fx = Fixture::new(None, "");
    let empty = fx.dir("empty-path");
    let told = fx.fake(&fx.dir("engine"), "told-duckdb", "2.0.0");
    let (code, stderr) = arc_run(
        &fx.project(),
        Run {
            path: &empty,
            told: Some(told.bin.as_os_str()),
            allow_untested: false,
        },
    );
    assert_eq!(code, Some(0), "runs:\n{stderr}");
    assert_eq!(told.sql_calls(), 1, "calls: {:?}", told.calls());
    assert!(!stderr.contains("cannot read SQL into tokens"), "{stderr}");
}

// Outside a run, a command that reads the asset graph reads it with arc's own parser when
// DuckDB gives no tokens, and its one warning names the missing tokenizer and the fix.
#[test]
fn a_graph_only_command_on_a_duckdb_without_the_tokenizer_warns_naming_the_fix() {
    let fx = Fixture::new(
        None,
        "CREATE TABLE orders AS SELECT * FROM (VALUES (1, 50)) AS t(id, amount);\n",
    );
    let empty = fx.dir("empty-path");
    let told = fx.fake_without_tokenizer(&fx.dir("engine"), "told-duckdb", HOMEBREW_2_0_0);
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(fx.project())
        .env("PATH", &empty)
        .env("ARCFORM_DB_DIR", fx.project().with_file_name("db"))
        .env("ARCFORM_HISTORY_DIR", fx.dir("history"))
        .env(BIN_ENV, &told.bin)
        .env_remove("ARC_SQL_READER")
        .env("NO_COLOR", "1")
        .args([
            "sql",
            "record",
            "SELECT * FROM orders WHERE amount > 10",
            "--name",
            "big_orders",
        ])
        .output()
        .expect("spawn arc");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "arc sql record succeeds: {stderr}");
    let warnings: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("cannot read SQL into tokens"))
        .collect();
    assert_eq!(warnings.len(), 1, "one warning: {stderr}");
    for needle in [
        "warning:",
        told.bin.display().to_string().as_str(),
        "v2.0.0-dev88614",
        "autocomplete extension",
        "arc's own reading",
        INSTALLER,
        "ARC_DUCKDB_BIN=~/.duckdb/cli/2.0.0/duckdb",
    ] {
        assert!(
            warnings[0].contains(needle),
            "expected {needle:?}: {}",
            warnings[0]
        );
    }
    assert!(!stderr.contains("no DuckDB 2 answered"), "{stderr}");
}
