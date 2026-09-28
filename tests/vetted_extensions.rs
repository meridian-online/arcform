//! `arc run` refuses a Protocol whose SQL installs a DuckDB community extension off the
//! vetted list, before a step runs, and names what it found; `docs/VETTED_EXTENSIONS.md`
//! is the list for a person, and `src/vetted_extensions.json` is the list arc holds.
//!
//! Every Protocol here runs on a fake engine: a shell script that reports a chosen version
//! and appends each call's arguments to a log, so "no step ran" and "the step started" are
//! read off the engine's own record. The check reads SQL text and never asks the engine
//! anything but its version, so a fake one decides nothing the tests read. One test runs
//! the real DuckDB CLI instead, to hold arc's idea of what is a comment or a string to
//! DuckDB's own.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A Protocol in a directory of its own, beside the fake engine that runs it.
struct Protocol {
    root: tempfile::TempDir,
}

/// What one `arc run` did.
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// One line per call the engine received: `--version`, or `<db> -f <sql file>`.
    engine_calls: Vec<String>,
}

impl Outcome {
    /// The SQL files the engine was handed, by file name, in the order it ran them.
    fn started(&self) -> Vec<String> {
        self.engine_calls
            .iter()
            .filter_map(|call| call.split(" -f ").nth(1))
            .map(|sql| sql.rsplit('/').next().unwrap().to_string())
            .collect()
    }

    fn assert_refused(&self, label: &str) {
        assert_eq!(
            self.code,
            Some(1),
            "[{label}] arc run should refuse:\n{}",
            self.stderr
        );
        assert!(
            self.stderr
                .contains("installs a DuckDB extension arc has not vetted"),
            "[{label}] refused for another reason:\n{}",
            self.stderr
        );
        assert_eq!(
            self.engine_calls,
            vec!["--version"],
            "[{label}] no step or hook may reach the engine"
        );
    }

    fn assert_not_refused(&self, label: &str, started: &[&str]) {
        assert!(
            !self.stderr.contains("vetted"),
            "[{label}] arc run should say nothing of the vetted list:\n{}",
            self.stderr
        );
        assert_eq!(
            self.code,
            Some(0),
            "[{label}] arc run should succeed:\n{}",
            self.stderr
        );
        assert_eq!(self.started(), started, "[{label}] steps the engine ran");
    }
}

impl Protocol {
    /// One SQL step per `(name, sql)`, each in `models/<name>.sql`.
    fn steps(steps: &[(&str, &str)]) -> Self {
        let mut yaml = String::from("name: vetted\nsteps:\n");
        for (name, _) in steps {
            yaml.push_str(&format!("  - name: {name}\n    sql: models/{name}.sql\n"));
        }
        let files: Vec<(String, &str)> = steps
            .iter()
            .map(|(name, sql)| (format!("models/{name}.sql"), *sql))
            .collect();
        Self::with(&yaml, &files)
    }

    /// `arcform.yaml` holding `yaml`, and each `(path, text)` beside it.
    fn with(yaml: &str, files: &[(String, &str)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir_all(project.join("models")).unwrap();
        fs::write(project.join("arcform.yaml"), yaml).unwrap();
        for (path, text) in files {
            fs::write(project.join(path), text).unwrap();
        }
        Protocol { root }
    }

    fn project(&self) -> PathBuf {
        self.root.path().join("project")
    }

    /// `arc run` on a fake engine that reports `version`.
    fn run(&self, version: &str) -> Outcome {
        let engine = self.root.path().join("duckdb");
        let log = self.root.path().join("duckdb.log");
        let _ = fs::remove_file(&log);
        fs::write(
            &engine,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$1\" = \"--version\" ]; then echo 'v{version} (fake) 0000000000'; fi\nexit 0\n",
                log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(self.project())
            .arg("run")
            .env("ARC_DUCKDB_BIN", &engine)
            .env_remove("ARC_ALLOW_UNTESTED_ENGINE")
            .output()
            .expect("spawn arc run");
        Outcome {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            engine_calls: fs::read_to_string(&log)
                .map(|s| s.lines().map(str::to_string).collect())
                .unwrap_or_default(),
        }
    }
}

/// The version every entry on the list names today, so a run on it draws no warning.
const VETTED_ON: &str = "1.5.5";

// ---- what is refused ----

#[test]
fn an_unvetted_community_install_is_refused_before_a_step_runs() {
    let protocol = Protocol::steps(&[
        ("add_forecast", "INSTALL anofox_forecast FROM community;\n"),
        ("after", "SELECT 1;\n"),
    ]);
    let run = protocol.run(VETTED_ON);
    run.assert_refused("unvetted");
    for needle in [
        "step 'add_forecast'",
        "anofox_forecast",
        "is not on the vetted list",
        "docs/VETTED_EXTENSIONS.md",
    ] {
        assert!(
            run.stderr.contains(needle),
            "the refusal should name {needle:?}:\n{}",
            run.stderr
        );
    }
    assert!(
        !protocol.project().join("build").exists(),
        "a refused Protocol leaves no run record"
    );
}

#[test]
fn any_letter_case_and_force_install_are_refused_as_install_is() {
    for sql in [
        "install anofox_forecast from community;",
        "FORCE INSTALL anofox_forecast FROM community;",
    ] {
        let run = Protocol::steps(&[("s", sql)]).run(VETTED_ON);
        run.assert_refused(sql);
        assert!(
            run.stderr
                .contains("anofox_forecast is not on the vetted list"),
            "[{sql}] {}",
            run.stderr
        );
    }
}

#[test]
fn an_install_from_an_address_or_another_repository_is_refused_by_name() {
    for (sql, named) in [
        (
            "INSTALL mlpack FROM 'https://example.org/ext';",
            "from the address 'https://example.org/ext'",
        ),
        (
            "INSTALL mlpack FROM core_nightly;",
            "from the repository core_nightly",
        ),
    ] {
        let run = Protocol::steps(&[("s", sql)]).run(VETTED_ON);
        run.assert_refused(sql);
        assert!(
            run.stderr.contains(named),
            "[{sql}] should name {named:?}:\n{}",
            run.stderr
        );
    }
}

#[test]
fn a_repository_setting_and_an_install_by_path_are_refused_by_name() {
    for (sql, named) in [
        (
            "SET custom_extension_repository = '/tmp/ext'; INSTALL mlpack;",
            "names the setting custom_extension_repository",
        ),
        (
            "INSTALL '/tmp/ext/mlpack.duckdb_extension';",
            "installs the extension at '/tmp/ext/mlpack.duckdb_extension'",
        ),
    ] {
        let run = Protocol::steps(&[("s", sql)]).run(VETTED_ON);
        run.assert_refused(sql);
        assert!(
            run.stderr.contains(named),
            "[{sql}] should name {named:?}:\n{}",
            run.stderr
        );
    }
}

#[test]
fn a_hook_is_read_as_a_step_is() {
    let protocol = Protocol::with(
        "name: vetted\nsteps:\n  - name: s\n    sql: models/s.sql\nhooks:\n  on_init:\n    name: setup\n    sql: models/setup.sql\n",
        &[
            ("models/s.sql".into(), "SELECT 1;\n"),
            (
                "models/setup.sql".into(),
                "INSTALL anofox_forecast FROM community;\n",
            ),
        ],
    );
    let run = protocol.run(VETTED_ON);
    run.assert_refused("hook");
    assert!(
        run.stderr.contains("hook on_init 'setup'")
            && run
                .stderr
                .contains("anofox_forecast is not on the vetted list"),
        "the refusal should name the hook:\n{}",
        run.stderr
    );
}

#[test]
fn a_step_arc_cannot_parse_is_checked_too() {
    let unparseable = "CREATE TABLE b AS SELECT 1 +;\n";
    // The same step with a vetted install: arc reports it cannot parse the step, and runs it.
    let vetted = Protocol::steps(&[(
        "s",
        &format!("INSTALL mlpack FROM community;\n{unparseable}"),
    )])
    .run(VETTED_ON);
    assert!(
        vetted.stderr.contains("could not parse"),
        "the step should be one arc cannot parse:\n{}",
        vetted.stderr
    );
    vetted.assert_not_refused("vetted, unparseable", &["s.sql"]);

    let run = Protocol::steps(&[(
        "s",
        &format!("INSTALL anofox_forecast FROM community;\n{unparseable}"),
    )])
    .run(VETTED_ON);
    run.assert_refused("unparseable");
    assert!(
        run.stderr.contains("step 's'")
            && run
                .stderr
                .contains("anofox_forecast is not on the vetted list"),
        "{}",
        run.stderr
    );
}

// ---- what is not refused ----

#[test]
fn a_vetted_community_install_starts_its_step() {
    let run =
        Protocol::steps(&[("s", "INSTALL mlpack FROM community; LOAD mlpack;\n")]).run(VETTED_ON);
    run.assert_not_refused("vetted", &["s.sql"]);
}

#[test]
fn an_install_inside_a_comment_or_a_string_is_not_read() {
    for sql in [
        "-- INSTALL anofox_forecast FROM community;\nSELECT 1;",
        "/* INSTALL anofox_forecast FROM community; */ SELECT 1;",
        "SELECT 'INSTALL anofox_forecast FROM community';",
    ] {
        Protocol::steps(&[("s", sql)])
            .run(VETTED_ON)
            .assert_not_refused(sql, &["s.sql"]);
    }
}

#[test]
fn an_install_from_duckdbs_own_repository_and_a_load_are_not_checked() {
    for sql in [
        "INSTALL excel;",
        "INSTALL spatial FROM core;",
        "LOAD mlpack;",
    ] {
        Protocol::steps(&[("s", sql)])
            .run(VETTED_ON)
            .assert_not_refused(sql, &["s.sql"]);
    }
}

#[test]
fn a_vetted_extension_on_a_version_its_entry_does_not_name_warns_once_and_runs() {
    let run = Protocol::steps(&[
        ("a", "INSTALL mlpack FROM community;\n"),
        ("b", "FORCE INSTALL mlpack FROM community; LOAD mlpack;\n"),
    ])
    .run("1.5.4");
    let warnings: Vec<&str> = run
        .stderr
        .lines()
        .filter(|l| l.contains("warning:") && l.contains("mlpack"))
        .collect();
    assert_eq!(warnings.len(), 1, "one warning:\n{}", run.stderr);
    for needle in ["mlpack", "v1.5.4", "v1.5.5"] {
        assert!(
            warnings[0].contains(needle),
            "the warning should name {needle:?}: {}",
            warnings[0]
        );
    }
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.started(), vec!["a.sql", "b.sql"]);
}

/// What `arc run` printed for this Protocol on e2a6e99, the commit this check was added
/// to, byte for byte: recorded by running that commit's binary on the same fake engine.
const BEFORE_STDOUT: &str = "\u{1b}[2m[hook]\u{1b}[0m \u{1b}[1msetup\u{1b}[0m ...\n\
[1/3] \u{1b}[1mload\u{1b}[0m ...\n\
[2/3] \u{1b}[1mtransform\u{1b}[0m ...\n\
[3/3] \u{1b}[1mannounce\u{1b}[0m ...\n\
hi\n\
\n\
Asset graph (2 nodes):\n\
\x20\x20a [table]\n\
\x20\x20\x20\x20\x20\x20produced by  load\n\
\x20\x20\x20\x20\x20\x20feeds        transform\n\
\x20\x20b [table]\n\
\x20\x20\x20\x20\x20\x20produced by  transform\n\
\n\
\u{1b}[32m✓\u{1b}[39m 3/3 steps succeeded.\n\
";

#[test]
fn a_protocol_with_no_install_prints_what_it_printed_before() {
    let protocol = Protocol::with(
        "name: vetted\nsteps:\n  - name: load\n    sql: models/load.sql\n  - name: transform\n    sql: models/transform.sql\n  - name: announce\n    command: echo hi\nhooks:\n  on_init:\n    name: setup\n    sql: models/setup.sql\n",
        &[
            (
                "models/load.sql".into(),
                "CREATE TABLE a AS SELECT 1 AS x;\n",
            ),
            (
                "models/transform.sql".into(),
                "-- a comment\nCREATE TABLE b AS SELECT x FROM a;\nLOAD mlpack;\n",
            ),
            ("models/setup.sql".into(), "SELECT 1;\n"),
        ],
    );
    let run = protocol.run("1.5.4");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stderr, "", "nothing on stderr");
    assert_eq!(run.stdout, BEFORE_STDOUT);
}

// ---- the page and the list ----

fn repo_file(path: &str) -> String {
    let full = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    fs::read_to_string(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()))
}

/// `(name, licence, repository, DuckDB version, what was run)`, one per extension.
type Entry = (String, String, String, String, String);

/// The rows of the table under `## The list` on the page.
fn page_entries() -> Vec<Entry> {
    let page = repo_file("docs/VETTED_EXTENSIONS.md");
    let section = page
        .split("\n## The list\n")
        .nth(1)
        .expect("the page has a `## The list` section");
    let section = section.split("\n## ").next().unwrap();
    section
        .lines()
        .filter(|l| l.starts_with("| `"))
        .map(|l| {
            let cells: Vec<String> = l
                .trim_matches('|')
                .split(" | ")
                .map(|c| c.trim().trim_matches('`').to_string())
                .collect();
            assert_eq!(cells.len(), 5, "a row of five cells: {l}");
            (
                cells[0].clone(),
                cells[1].clone(),
                cells[2].clone(),
                cells[3].clone(),
                cells[4].clone(),
            )
        })
        .collect()
}

/// The entries of the list arc compiles in.
fn held_entries() -> Vec<Entry> {
    let list: serde_json::Value =
        serde_json::from_str(&repo_file("src/vetted_extensions.json")).unwrap();
    let text = |e: &serde_json::Value, k: &str| e[k].as_str().unwrap().to_string();
    list["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let versions: Vec<&str> = e["duckdb_versions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            (
                text(e, "name"),
                text(e, "licence"),
                text(e, "repository"),
                versions.join(", "),
                text(e, "proof"),
            )
        })
        .collect()
}

#[test]
fn the_page_and_the_list_arc_holds_are_the_same_list() {
    let page = page_entries();
    let held = held_entries();
    assert_eq!(page.len(), 12, "twelve rows on the page: {page:?}");
    assert_eq!(held.len(), 12, "twelve entries arc holds: {held:?}");
    let page: BTreeSet<Entry> = page.into_iter().collect();
    let held: BTreeSet<Entry> = held.into_iter().collect();
    assert_eq!(
        page.difference(&held).collect::<Vec<_>>(),
        Vec::<&Entry>::new(),
        "on the page and not in src/vetted_extensions.json"
    );
    assert_eq!(
        held.difference(&page).collect::<Vec<_>>(),
        Vec::<&Entry>::new(),
        "in src/vetted_extensions.json and not on the page"
    );
}

#[test]
fn every_extension_on_the_page_is_one_arc_lets_a_step_install() {
    let sql: String = page_entries()
        .iter()
        .map(|(name, ..)| format!("INSTALL {name} FROM community;\n"))
        .collect();
    Protocol::steps(&[("s", &sql)])
        .run(VETTED_ON)
        .assert_not_refused("every page row", &["s.sql"]);
}

#[test]
fn every_licence_on_the_list_is_permissive() {
    const PERMISSIVE: [&str; 5] = ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC"];
    for (name, licence, ..) in held_entries() {
        assert!(
            PERMISSIVE.contains(&licence.as_str()),
            "{name} states {licence}, which is not a permissive licence"
        );
    }
}

// ---- arc reads comments and strings as DuckDB does ----

/// Each case holds `@@` where a statement goes. DuckDB runs the case with `@@` as a
/// `CREATE TABLE`, and arc reads it with `@@` as an unvetted `INSTALL`: arc has to refuse
/// exactly the cases in which DuckDB ran the statement.
#[test]
fn arc_refuses_an_install_exactly_where_duckdb_would_run_it() {
    let cases = [
        "-- @@;\nSELECT 1;",
        "/* @@; */ SELECT 1;",
        "/* /* */ @@; */ SELECT 1;",
        "/* /* */ */ @@;",
        "SELECT '@@;';",
        "SELECT 'it''s'; @@;",
        r"SELECT E'a\'; @@; --';",
        r"SELECT e'a\'; @@; --';",
        r"SELECT 'a\'; @@;",
        "SELECT $$ @@; $$;",
        "SELECT $t$ $$ @@; $t$;",
        "SELECT 1 AS \"x;@@\";",
        "SELECT 1; -- note\n@@;",
        "SELECT 1 /* x */; @@;",
    ];
    let scratch = tempfile::tempdir().unwrap();
    let mut disagreements = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let db = scratch.path().join(format!("case{i}.duckdb"));
        let sql = scratch.path().join(format!("case{i}.sql"));
        fs::write(&sql, case.replace("@@", "CREATE TABLE probe AS SELECT 1")).unwrap();
        let duckdb = |args: &[&std::ffi::OsStr]| {
            Command::new("duckdb")
                .args(args)
                .env("HOME", scratch.path())
                .output()
                .expect("the DuckDB CLI on PATH")
        };
        duckdb(&[db.as_os_str(), "-f".as_ref(), sql.as_os_str()]);
        let probe = duckdb(&[
            db.as_os_str(),
            "-csv".as_ref(),
            "-noheader".as_ref(),
            "-c".as_ref(),
            "SELECT count(*) FROM duckdb_tables() WHERE table_name = 'probe'".as_ref(),
        ]);
        let ran = match String::from_utf8_lossy(&probe.stdout).trim() {
            "1" => true,
            "0" => false,
            other => panic!("[{case}] DuckDB answered {other:?}"),
        };
        let run = Protocol::steps(&[(
            "s",
            &case.replace("@@", "INSTALL anofox_forecast FROM community"),
        )])
        .run(VETTED_ON);
        let refused = run
            .stderr
            .contains("anofox_forecast is not on the vetted list");
        if ran != refused {
            disagreements.push(format!(
                "{case:?}: DuckDB ran it: {ran}; arc refused: {refused}"
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "arc and DuckDB disagree on what is SQL:\n{}",
        disagreements.join("\n")
    );
}
