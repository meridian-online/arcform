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

    /// The stderr lines holding [`UNREAD`]: the warnings about SQL a step builds or writes.
    fn unread_warnings(&self) -> Vec<&str> {
        self.stderr.lines().filter(|l| l.contains(UNREAD)).collect()
    }

    /// Assert the run started `started` without a refusal and printed one warning about SQL a
    /// step builds or writes, and return it.
    fn assert_warned_once(&self, label: &str, started: &[&str]) -> &str {
        self.assert_not_refused(label, started);
        let warnings = self.unread_warnings();
        assert_eq!(warnings.len(), 1, "[{label}] one warning:\n{}", self.stderr);
        warnings[0]
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

/// What each warning about SQL a step builds or writes while it runs holds, and no other
/// message arc prints.
const UNREAD: &str = "does not read the SQL";

/// The page every refusal and warning about an extension points to.
const PAGE: &str = "https://github.com/meridian-online/arcform/blob/main/docs/VETTED_EXTENSIONS.md";

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
fn an_extension_the_registry_serves_and_the_list_leaves_out_is_refused_by_name() {
    let run = Protocol::steps(&[("s", "INSTALL read_stat FROM community;\n")]).run(VETTED_ON);
    run.assert_refused("read_stat");
    assert!(
        run.stderr.contains("read_stat is not on the vetted list"),
        "the refusal should name read_stat:\n{}",
        run.stderr
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
fn a_parser_switch_is_refused_by_name() {
    for (sql, named) in [
        ("CALL enable_peg_parser();", "names enable_peg_parser"),
        (
            "SET allow_parser_override_extension = 'fallback';",
            "names allow_parser_override_extension",
        ),
        (
            "SET allow_parser_override_extension = 'strict';",
            "names allow_parser_override_extension",
        ),
    ] {
        let run = Protocol::steps(&[("s", sql)]).run(VETTED_ON);
        run.assert_refused(sql);
        assert!(
            run.stderr.contains(&format!(
                "models/s.sql, line 1) {named}, which switches DuckDB to a second parser"
            )),
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
fn each_extension_the_proofs_added_starts_its_step() {
    for name in ["finetype", "minijinja", "onager", "splink_udfs"] {
        let sql = format!("INSTALL {name} FROM community; LOAD {name};\n");
        Protocol::steps(&[("s", &sql)])
            .run(VETTED_ON)
            .assert_not_refused(name, &["s.sql"]);
    }
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

// ---- SQL a step builds or writes while it runs ----

#[test]
fn a_call_that_runs_sql_arc_does_not_read_warns_and_runs_the_step() {
    let run = Protocol::steps(&[(
        "s",
        "SELECT 1;\nFROM query('FROM enable_' || 'peg_parser()');\n",
    )])
    .run(VETTED_ON);
    let warning = run.assert_warned_once("assembled", &["s.sql"]);
    for needle in ["step 's'", "models/s.sql", "line 2", "query()", PAGE] {
        assert!(
            warning.contains(needle),
            "the warning should name {needle:?}: {warning}"
        );
    }
    for (sql, named) in [
        ("FROM query(getvariable('q'));", "query() on line 1"),
        (
            "FROM json_execute_serialized_sql(json_serialize_sql('FROM enable_' || 'peg_parser()'));",
            "json_execute_serialized_sql() on line 1",
        ),
        // A call held in the literal another call runs is read as one in the file, and the
        // warning names the line of the call in the file.
        (
            "SET VARIABLE q = 'FROM enable_' || 'peg_parser()'; FROM query('FROM query(getvariable(''q''))');",
            "query() on line 1",
        ),
        (
            "SET VARIABLE q = 'FROM enable_' || 'peg_parser()'; FROM query($$FROM query(getvariable('q'))$$);",
            "query() on line 1",
        ),
        (
            "SELECT 1;\nFROM query('SELECT 1\nUNION ALL\nFROM query(getvariable(''q''))');",
            "query() on line 2",
        ),
    ] {
        let run = Protocol::steps(&[("s", sql)]).run(VETTED_ON);
        let warning = run.assert_warned_once(sql, &["s.sql"]);
        assert!(
            warning.contains(named),
            "[{sql}] the warning should name {named:?}: {warning}"
        );
    }
}

#[test]
fn a_call_on_one_string_and_a_name_that_is_not_a_call_draw_no_warning() {
    for sql in [
        "FROM query('SELECT 42');",
        "FROM QUERY($$SELECT 42$$);",
        "SELECT query FROM log;",
        "CREATE TABLE query (a INT);",
    ] {
        let run = Protocol::steps(&[("s", sql)]).run(VETTED_ON);
        run.assert_not_refused(sql, &["s.sql"]);
        assert_eq!(run.unread_warnings(), Vec::<&str>::new(), "[{sql}]");
    }
}

#[test]
fn importing_a_database_warns_and_runs_the_step() {
    for sql in [
        "IMPORT DATABASE 'imp';",
        "import database 'imp';",
        "PRAGMA import_database('imp');",
    ] {
        let run = Protocol::steps(&[("s", &format!("SELECT 1;\n{sql}\n"))]).run(VETTED_ON);
        let warning = run.assert_warned_once(sql, &["s.sql"]);
        for needle in ["step 's'", "models/s.sql", "IMPORT DATABASE on line 2"] {
            assert!(
                warning.contains(needle),
                "[{sql}] the warning should name {needle:?}: {warning}"
            );
        }
    }
}

#[test]
fn naming_duckdbrc_warns_and_runs_every_step() {
    for sql in [
        "COPY (SELECT 'SELECT 1;') TO '~/.duckdbrc' (HEADER false, QUOTE '');",
        "COPY (SELECT 'SELECT 1;') TO \"~/.duckdbrc\" (HEADER false, QUOTE '');",
        "SELECT '/Users/me/.DuckDBrc';",
    ] {
        let run = Protocol::steps(&[
            ("write", &format!("SELECT 1;\n{sql}\n")),
            ("after", "SELECT 1;\n"),
        ])
        .run(VETTED_ON);
        let warning = run.assert_warned_once(sql, &["write.sql", "after.sql"]);
        for needle in ["step 'write'", "models/write.sql", ".duckdbrc on line 2"] {
            assert!(
                warning.contains(needle),
                "[{sql}] the warning should name {needle:?}: {warning}"
            );
        }
    }
}

#[test]
fn each_step_or_hook_draws_one_warning_naming_each_line() {
    let one_shape = Protocol::steps(&[(
        "s",
        "FROM query(getvariable('q'));\nSELECT 1;\nFROM query(getvariable('r'));\n",
    )])
    .run(VETTED_ON);
    let warning = one_shape.assert_warned_once("one shape", &["s.sql"]);
    assert!(
        warning.contains("query() on line 1, query() on line 3"),
        "{warning}"
    );

    let two_shapes = Protocol::steps(&[(
        "s",
        "IMPORT DATABASE 'imp';\nFROM query(getvariable('q'));\n",
    )])
    .run(VETTED_ON);
    let warning = two_shapes.assert_warned_once("two shapes", &["s.sql"]);
    assert!(
        warning.contains("IMPORT DATABASE on line 1, query() on line 2"),
        "{warning}"
    );

    // Two shapes in one statement, the call on the later line.
    let one_statement = Protocol::steps(&[(
        "s",
        "SELECT '~/.duckdbrc'\nUNION ALL FROM query(getvariable('q'));\n",
    )])
    .run(VETTED_ON);
    let warning = one_statement.assert_warned_once("one statement", &["s.sql"]);
    assert!(
        warning.contains(".duckdbrc on line 1, query() on line 2"),
        "{warning}"
    );

    let two_steps = Protocol::steps(&[
        ("a", "IMPORT DATABASE 'imp';\n"),
        ("b", "FROM query(getvariable('q'));\n"),
    ])
    .run(VETTED_ON);
    two_steps.assert_not_refused("two steps", &["a.sql", "b.sql"]);
    let warnings = two_steps.unread_warnings();
    assert_eq!(warnings.len(), 2, "{}", two_steps.stderr);
    assert!(
        warnings[0].contains("step 'a' (models/a.sql)"),
        "{warnings:?}"
    );
    assert!(
        warnings[1].contains("step 'b' (models/b.sql)"),
        "{warnings:?}"
    );

    let hook = Protocol::with(
        "name: vetted\nsteps:\n  - name: s\n    sql: models/s.sql\nhooks:\n  on_init:\n    name: setup\n    sql: models/setup.sql\n",
        &[
            ("models/s.sql".into(), "SELECT 1;\n"),
            ("models/setup.sql".into(), "IMPORT DATABASE 'imp';\n"),
        ],
    )
    .run(VETTED_ON);
    assert_eq!(hook.code, Some(0), "{}", hook.stderr);
    let warnings = hook.unread_warnings();
    assert_eq!(warnings.len(), 1, "{}", hook.stderr);
    assert!(
        warnings[0].contains("hook on_init 'setup' (models/setup.sql)"),
        "{warnings:?}"
    );
}

#[test]
fn a_shape_inside_a_comment_or_a_string_draws_no_warning() {
    for sql in [
        "-- IMPORT DATABASE 'imp';\nSELECT 1;",
        "/* FROM query(getvariable('q')); */ SELECT 1;",
        "SELECT 'IMPORT DATABASE imp';",
    ] {
        let run = Protocol::steps(&[("s", sql)]).run(VETTED_ON);
        run.assert_not_refused(sql, &["s.sql"]);
        assert_eq!(run.unread_warnings(), Vec::<&str>::new(), "[{sql}]");
    }
}

/// What `arc run` printed on stderr for [`a_refused_protocol_prints_what_it_printed_before`]'s
/// Protocol on 90c8260, byte for byte: recorded by running that commit's binary on the same
/// fake engine.
const REFUSED_BEFORE_STDERR: &str = "error: this Protocol's SQL installs a DuckDB extension arc has not vetted, so no step was run:\n  \
step 's' (models/s.sql, line 2) installs anofox_forecast from the community registry, and anofox_forecast is not on the vetted list\n\
A step or hook may install a community extension on the vetted list, FROM community. The list, and what this check does not read, are at https://github.com/meridian-online/arcform/blob/main/docs/VETTED_EXTENSIONS.md\n";

#[test]
fn a_refused_protocol_prints_what_it_printed_before() {
    let run = Protocol::steps(&[(
        "s",
        "IMPORT DATABASE 'imp';\nINSTALL anofox_forecast FROM community;\n",
    )])
    .run(VETTED_ON);
    run.assert_refused("import and install");
    assert!(run.stderr.contains("anofox_forecast"), "{}", run.stderr);
    assert_eq!(run.stderr, REFUSED_BEFORE_STDERR);
}

/// What `arc run` printed for [`a_protocol_with_none_of_the_shapes_prints_what_it_printed_before`]'s
/// Protocol on 90c8260, byte for byte: recorded by running that commit's binary on the same
/// fake engine. The first warning is lineage's, on `query('SELECT 42')`, whose arguments arc
/// does not read for what the step reads and writes.
const NO_SHAPE_BEFORE_STDOUT: &str = "\u{1b}[2m[hook]\u{1b}[0m \u{1b}[1msetup\u{1b}[0m ...\n\
[1/3] \u{1b}[1mload\u{1b}[0m ...\n\
[2/3] \u{1b}[1mask\u{1b}[0m ...\n\
[3/3] \u{1b}[1mexport\u{1b}[0m ...\n\
\n\
Asset graph (3 nodes):\n\
\x20\x20a [table]\n\
\x20\x20\x20\x20\x20\x20produced by  load\n\
\x20\x20\x20\x20\x20\x20feeds        export\n\
\x20\x20b [table]\n\
\x20\x20\x20\x20\x20\x20produced by  ask\n\
\x20\x20out.csv [file]\n\
\x20\x20\x20\x20\x20\x20produced by  export\n\
\n\
\u{1b}[32m✓\u{1b}[39m 3/3 steps succeeded.\n\
";
const NO_SHAPE_BEFORE_STDERR: &str = "\u{1b}[33mwarning:\u{1b}[39m step 'ask' calls the table function 'query', whose arguments arc does not read, so what the step reads and writes is unknown — `depends_on:` and `produces:` declare what the step reads and writes\n\
\u{1b}[33mwarning:\u{1b}[39m step 'export' succeeded but does not appear to have produced: out.csv — arc will keep re-running this step until its own work (or the manifest's produces:) matches\n";

#[test]
fn a_protocol_with_none_of_the_shapes_prints_what_it_printed_before() {
    let protocol = Protocol::with(
        "name: vetted\nsteps:\n  - name: load\n    sql: models/load.sql\n  - name: ask\n    sql: models/ask.sql\n  - name: export\n    sql: models/export.sql\nhooks:\n  on_init:\n    name: setup\n    sql: models/setup.sql\n",
        &[
            (
                "models/load.sql".into(),
                "CREATE TABLE a AS SELECT 1 AS x;\n",
            ),
            (
                "models/ask.sql".into(),
                "CREATE TABLE b AS FROM query('SELECT 42');\n",
            ),
            ("models/export.sql".into(), "COPY a TO 'out.csv';\n"),
            ("models/setup.sql".into(), "SELECT 1;\n"),
        ],
    );
    let run = protocol.run(VETTED_ON);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, NO_SHAPE_BEFORE_STDOUT);
    assert_eq!(run.stderr, NO_SHAPE_BEFORE_STDERR);
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
    assert_eq!(page.len(), 16, "sixteen rows on the page: {page:?}");
    assert_eq!(held.len(), 16, "sixteen entries arc holds: {held:?}");
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

/// Each entry whose licence is not permissive, as a message naming the extension. A licence
/// written as names joined by ` OR ` is permissive when each name is one of the permissive ones.
fn licence_refusals(entries: &[(&str, &str)]) -> Vec<String> {
    const PERMISSIVE: [&str; 5] = ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC"];
    entries
        .iter()
        .filter(|(_, licence)| !licence.split(" OR ").all(|name| PERMISSIVE.contains(&name)))
        .map(|(name, licence)| {
            format!("{name} states {licence}, which is not a permissive licence")
        })
        .collect()
}

#[test]
fn every_licence_on_the_list_is_permissive() {
    let held = held_entries();
    let entries: Vec<(&str, &str)> = held
        .iter()
        .map(|(name, licence, ..)| (name.as_str(), licence.as_str()))
        .collect();
    assert_eq!(licence_refusals(&entries), Vec::<String>::new());
}

#[test]
fn a_licence_of_names_joined_by_or_is_permissive_when_each_name_is() {
    assert_eq!(
        licence_refusals(&[
            ("onager", "MIT OR Apache-2.0"),
            ("swapped", "Apache-2.0 OR MIT"),
            ("plain", "BSD-3-Clause"),
        ]),
        Vec::<String>::new()
    );
    assert_eq!(
        licence_refusals(&[
            ("source_available", "BSL-1.1"),
            ("one_bad_name", "MIT OR BSL-1.1"),
            ("none_stated", ""),
        ]),
        vec![
            "source_available states BSL-1.1, which is not a permissive licence".to_string(),
            "one_bad_name states MIT OR BSL-1.1, which is not a permissive licence".to_string(),
            "none_stated states , which is not a permissive licence".to_string(),
        ]
    );
}

// ---- arc reads a SQL file as DuckDB does ----

/// Run the DuckDB CLI on `sql` as `arc run` runs a step, `duckdb <db> -f <file>`, with a home
/// directory of its own so no `~/.duckdbrc` is read, and return what it printed.
fn duckdb_runs(scratch: &Path, label: &str, sql: &str) -> (PathBuf, String) {
    let db = scratch.join(format!("{label}.duckdb"));
    let file = scratch.join(format!("{label}.sql"));
    fs::write(&file, sql).unwrap();
    let out = Command::new("duckdb")
        .arg(&db)
        .arg("-f")
        .arg(&file)
        .env("HOME", scratch)
        .output()
        .expect("the DuckDB CLI on PATH");
    let printed = String::from_utf8_lossy(&out.stdout).to_string();
    (db, printed + &String::from_utf8_lossy(&out.stderr))
}

/// The text between `before` and the next `after` in `text`, if `before` is there.
fn between<'a>(text: &'a str, before: &str, after: &str) -> Option<&'a str> {
    let rest = &text[text.find(before)? + before.len()..];
    Some(&rest[..rest.find(after)?])
}

/// Each case in `ran` holds `@@` where a statement goes. DuckDB runs the case with `@@` as a
/// `CREATE TABLE`, and arc reads it with `@@` as an unvetted `INSTALL`: arc has to refuse
/// exactly the cases in which DuckDB ran the statement, for the `INSTALL` or for a switch to
/// DuckDB's second parser before it. The cases hold each rule arc copies from DuckDB's CLI,
/// its parser's pre-pass and its scanner, at least one input each rule changes the reading
/// of, and each form of the switch.
///
/// Each case in `names` holds `@@` where the repository goes. DuckDB installs from a
/// directory that is not there, and its error names the extension it tried to fetch; arc
/// reads the community registry, and its refusal names the extension it read. The two names
/// have to be the same, or both absent.
#[test]
fn arc_refuses_an_install_exactly_where_duckdb_would_run_it() {
    let mut ran: Vec<String> = [
        // Comments and strings.
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
        // A `--` comment ends at a bare `\r`.
        "SELECT 1; -- note\r@@;",
        // The parser's unicode spaces, and two it does not count.
        "\u{feff}@@;",
        "-- setup\n\u{feff}@@;",
        "\u{200b}@@;",
        "\u{a0}@@;",
        "\u{2000}@@;",
        "\u{202f}@@;",
        "\u{205f}@@;",
        "\u{2060}@@;",
        "\u{3000}@@;",
        "\u{200c}@@;",
        "\u{2028}@@;",
        // What the pre-pass reads as a string or a comment, where a unicode space stays.
        "/* it's */ \u{200b}@@;",
        "SELECT 'a'; \u{200b}@@;",
        "SELECT 1 AS \"a\"; \u{200b}@@;",
        "SELECT $$ ' $$; \u{200b}@@;",
        "SELECT $t$ ' $t$; \u{200b}@@;",
        "-- '\n\u{200b}@@;",
        "-- '\r\u{200b}@@;",
        "SELECT 1 AS a$b$; \u{200b}@@;",
        // A `.` or a `#` line where no statement is open is not SQL.
        ".print /*\n@@;\n.print */",
        ".print '\n@@;\n.print '",
        ".print \"\n@@;\n.print \"",
        ".print $$\n@@;\n.print $$",
        "SELECT 1;\n.print /*\n@@;",
        "# '\n@@;\n# '",
        "# /*\n@@;",
        // ... and is SQL inside a statement, after a space, or after a byte-order mark.
        "SELECT 1\n.print /*\n; @@; -- */",
        "SELECT 1;\n .print /*\n@@; -- */",
        "\u{feff}.print /*\n@@; -- */",
        // A line starting with \x03 drops the lines held before it.
        "SELECT '\n\u{3}\n@@;",
        // Lines of spaces and comments hold nothing, and a `.` line after them is not SQL.
        "/* x */\n.print '\n@@;",
        "-- x\n.print '\n@@;",
        "\u{b}\n.print '\n@@;",
        "/* x\n.print */ '\n@@;",
        // Where the CLI ends a batch: a `;` outside what it reads as a string or comment,
        // on the line just read.
        "SELECT ';\n.print'; @@;",
        "SELECT 1 AS \";\n.print\"; @@;",
        "SELECT 1 /* ;\n.print */; @@;",
        "SELECT -- ;\n.5; @@;",
        "SELECT 1; -- note\n.print '\n@@;",
        "SELECT $$;\n.print$$; @@;",
        "SELECT $a1$;\n.print$a1$; @@;",
        "SELECT $\u{e9}$;\n.print$\u{e9}$; @@;",
        "SELECT $1$;\n.print$1$; @@;",
        "SELECT 1;\u{b}\n.print '\n@@;",
        "SELECT 1; /*\n*/\n.print '\n@@;",
        "SELECT 1 /*xx;\n.print */; @@;",
        "SELECT 'x;;\n.print'; @@;",
        "SELECT 1; SELECT 2 AS x$\n.print '\n@@;",
        // The pre-pass's dollar tags: no digit first, and digits after.
        "/* $1$ */ \u{200b}@@;",
        "/* $a1$ ' $a1$ */ \u{200b}@@;",
        // The CLI hands over what it holds at the end of the file, and a last \x03 line
        // drops it.
        "@@",
        "@@\n\u{3}",
        // A NUL joins the next line on, and does not end a statement there.
        "SELECT 1;;\0x\n.print '\n@@;",
        // A batch the CLI ends inside a comment its scanner reads as open.
        ".bail off\nSELECT 1 /* /* */ ;\n@@; -- */",
        // A NUL byte drops the rest of the chunk the CLI read it in.
        "SELECT 1; \0/*\n@@; -- */",
        // After DuckDB's second parser is switched on, a block comment does not nest and a
        // backslash escapes nothing inside E'...'. arc refuses the switch.
        "CALL enable_peg_parser();\n/* /* */ @@; -- */",
        "CALL enable_peg_parser();\nSELECT E'\\' ; @@; --';",
        "FROM enable_peg_parser();\n/* /* */ @@; -- */",
        "SET allow_parser_override_extension = 'fallback';\n/* /* */ @@; -- */",
        "SET allow_parser_override_extension = 'strict';\n/* /* */ @@; -- */",
        "PRAGMA allow_parser_override_extension = 'strict';\n/* /* */ @@; -- */",
        "FROM query('FROM enable_peg_parser()');\n/* /* */ @@; -- */",
        // ... and a switch in a comment switches nothing.
        "-- CALL enable_peg_parser();\n/* /* */ @@; -- */",
        "/* SET allow_parser_override_extension = 'strict'; */\nSELECT E'\\' ; @@; --';",
    ]
    .map(String::from)
    .to_vec();
    // The first chunk ends at byte 99, so `@@` starts the second and is kept.
    ran.push(format!("SELECT 1; \0{}@@;", "x".repeat(88)));
    ran.push(format!("SELECT 1; \0{}@@;", "x".repeat(87)));
    // A line longer than a chunk is still one line, so its `.` at byte 99 is SQL.
    ran.push(format!("SELECT 1;{}.print '\n@@;", " ".repeat(90)));
    let names = [
        "INSTALL anofox_forecast FROM @@;",
        "INSTALL anofox_forecast\r\nFROM @@;",
        "INSTALL anofox_forecast\rFROM @@;",
        "INSTALL\u{c}anofox_forecast FROM @@;",
        "INSTALL\u{a0}anofox_forecast FROM @@;",
        "INSTALL \u{2028}anofox_forecast FROM @@;",
        "INSTALL \"Anofox_Forecast\" FROM @@;",
        // Two strings with a line end between them are one.
        "INSTALL 'anofox_'\n'forecast' FROM @@;",
        "INSTALL 'anofox_'\r'forecast' FROM @@;",
        "INSTALL 'anofox_' -- note\n'forecast' FROM @@;",
        "INSTALL 'anofox_'\n  -- note\n\t'forecast' FROM @@;",
        "INSTALL 'anofox_' 'forecast' FROM @@;",
        "INSTALL 'anofox_'\n/* note */\n'forecast' FROM @@;",
        // The escapes of an E'...' string.
        r"INSTALL E'anofox\x5fforecast' FROM @@;",
        r"INSTALL E'anofox\137forecast' FROM @@;",
        "INSTALL E'anofox_'\n'fore\\x63ast' FROM @@;",
    ];
    let scratch = tempfile::tempdir().unwrap();
    let mut disagreements = Vec::new();
    for (i, case) in ran.iter().enumerate() {
        let (db, _) = duckdb_runs(
            scratch.path(),
            &format!("ran{i}"),
            &case.replace("@@", "CREATE TABLE probe AS SELECT 1"),
        );
        let probe = Command::new("duckdb")
            .arg(&db)
            .args(["-csv", "-noheader", "-c"])
            .arg("SELECT count(*) FROM duckdb_tables() WHERE table_name = 'probe'")
            .env("HOME", scratch.path())
            .output()
            .expect("the DuckDB CLI on PATH");
        let ran = match String::from_utf8_lossy(&probe.stdout).trim() {
            "1" => true,
            "0" => false,
            other => panic!("[{case:?}] DuckDB answered {other:?}"),
        };
        let run = Protocol::steps(&[(
            "s",
            &case.replace("@@", "INSTALL anofox_forecast FROM community"),
        )])
        .run(VETTED_ON);
        let refused = run
            .stderr
            .contains("anofox_forecast is not on the vetted list")
            || run
                .stderr
                .contains("which switches DuckDB to a second parser");
        if ran != refused {
            disagreements.push(format!(
                "{case:?}: DuckDB ran it: {ran}; arc refused: {refused}"
            ));
        }
    }
    let repo = scratch.path().join("no-such-repository");
    for (i, case) in names.iter().enumerate() {
        let (_, printed) = duckdb_runs(
            scratch.path(),
            &format!("name{i}"),
            &case.replace("@@", &format!("'{}'", repo.display())),
        );
        let fetched = between(&printed, "local extension \"", "\"").map(str::to_string);
        let run = Protocol::steps(&[("s", &case.replace("@@", "community"))]).run(VETTED_ON);
        let read =
            between(&run.stderr, ") installs ", " from the community registry").map(str::to_string);
        if fetched != read {
            disagreements.push(format!(
                "{case:?}: DuckDB fetched {fetched:?}; arc refused {read:?}"
            ));
        }
    }
    // SQL a step builds or writes while it runs, which DuckDB runs and arc warns on and does
    // not refuse. Each route is its step files, with what the probe reads after each.
    let serialized = Command::new("duckdb")
        .args(["-init", "/dev/null", "-noheader", "-list", "-c"])
        .arg("SELECT json_serialize_sql('FROM query(''FROM enable_'' || ''peg_parser()'')')")
        .output()
        .expect("the DuckDB CLI on PATH");
    let serialized = String::from_utf8_lossy(&serialized.stdout)
        .trim()
        .replace('\'', "''");
    let routes: Vec<(&str, Vec<String>, &[&str])> = vec![
        (
            "query() on an assembled string",
            vec!["FROM query('FROM enable_' || 'peg_parser()');\n/* /* */ @@; -- */".into()],
            &["1"],
        ),
        (
            "query() serialized as JSON, on an assembled string",
            vec![format!(
                "FROM json_execute_serialized_sql('{serialized}');\n/* /* */ @@; -- */"
            )],
            &["1"],
        ),
        (
            "IMPORT DATABASE of files the step wrote",
            vec!["EXPORT DATABASE 'imp';\nCOPY (SELECT '@@;') TO 'imp/schema.sql' (HEADER false, QUOTE '');\nIMPORT DATABASE 'imp';".into()],
            &["1"],
        ),
        // The second file runs the startup file the first wrote, as the CLI runs it before
        // each file it is given.
        (
            "a written ~/.duckdbrc",
            vec![
                "COPY (SELECT '@@;') TO '~/.duckdbrc' (HEADER false, QUOTE '');".into(),
                "SELECT 1;".into(),
            ],
            &["0", "1"],
        ),
    ];
    for (label, files, probes) in routes {
        // A directory of the route's own is its working directory, so `imp` lands there,
        // and its home, so the `.duckdbrc` it writes is read by no other case. The probe
        // runs with `-init /dev/null`, so it reads what the files did and runs no startup
        // file of its own.
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("route.duckdb");
        let duckdb = |args: &[&std::ffi::OsStr]| {
            Command::new("duckdb")
                .args(args)
                .current_dir(home.path())
                .env("HOME", home.path())
                .output()
                .expect("the DuckDB CLI on PATH")
        };
        let mut read = Vec::new();
        for (i, file) in files.iter().enumerate() {
            let path = home.path().join(format!("s{i}.sql"));
            fs::write(&path, file.replace("@@", "CREATE TABLE probe AS SELECT 1")).unwrap();
            duckdb(&[db.as_os_str(), "-f".as_ref(), path.as_os_str()]);
            let probe = duckdb(&[
                db.as_os_str(),
                "-init".as_ref(),
                "/dev/null".as_ref(),
                "-csv".as_ref(),
                "-noheader".as_ref(),
                "-c".as_ref(),
                "SELECT count(*) FROM duckdb_tables() WHERE table_name = 'probe'".as_ref(),
            ]);
            read.push(String::from_utf8_lossy(&probe.stdout).trim().to_string());
        }
        if read != probes {
            disagreements.push(format!(
                "{label}: DuckDB's probe read {read:?} after each file, and {probes:?} was measured"
            ));
        }
        let steps: Vec<(String, String)> = files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                (
                    format!("s{i}"),
                    file.replace("@@", "INSTALL anofox_forecast FROM community"),
                )
            })
            .collect();
        let steps: Vec<(&str, &str)> = steps
            .iter()
            .map(|(name, sql)| (name.as_str(), sql.as_str()))
            .collect();
        let run = Protocol::steps(&steps).run(VETTED_ON);
        let started: Vec<String> = (0..files.len()).map(|i| format!("s{i}.sql")).collect();
        let warned = run
            .unread_warnings()
            .iter()
            .any(|warning| warning.contains("step 's0' (models/s0.sql)"));
        let refused = run.stderr.contains("arc has not vetted");
        if run.code != Some(0) || run.started() != started || !warned || refused {
            disagreements.push(format!(
                "{label}: DuckDB ran it; arc exited {:?}, started {:?}, warned: {warned}, refused: {refused}\n{}",
                run.code,
                run.started(),
                run.stderr
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "arc and DuckDB disagree on what is SQL:\n{}",
        disagreements.join("\n")
    );
}
