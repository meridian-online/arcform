//! Where a Protocol's working database lives, driven through the real `arc` binary and a
//! real DuckDB.
//!
//! A Protocol whose `arcform.yaml` names no `db:` keeps its database in arc's data
//! folder — `$ARCFORM_DB_DIR`, else `~/.arcform/db` — under a key made from the Protocol
//! directory's canonical path, so the directory holds what the author wrote, what the
//! steps produce and arc's run records, and a copy of it runs where it lands. Every test
//! here points `ARCFORM_DB_DIR` at a directory of its own, so none writes into the
//! developer's `~/.arcform`, and computes the database's path from that definition rather
//! than reading it back from arc.
//!
//! Needs a `duckdb` CLI on PATH, the same requirement as any `arc run` of a SQL step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

// This file reads `arc run`'s output with two of the shared helpers and drives the
// binary itself, so the helpers that spawn `arc run` go unused here.
#[allow(dead_code)]
mod common;
use common::{step_outcome, strip_ansi};

/// A test's own directory, holding its Protocols, arc's data folder and arc's history.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Fixture {
            root: tempfile::tempdir().unwrap(),
        }
    }

    /// The data folder arc is told to keep databases in.
    fn db_root(&self) -> PathBuf {
        self.root.path().join("db")
    }

    /// Write a Protocol at `rel` under the fixture: its manifest and `files`.
    fn protocol(&self, rel: &str, manifest: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = self.root.path().join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("arcform.yaml"), manifest).unwrap();
        for (path, body) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    /// `arc <args>` in `dir`, with the data folder and history under the fixture.
    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_arc"));
        cmd.current_dir(dir)
            .args(args)
            .env("ARCFORM_DB_DIR", self.db_root())
            .env("ARCFORM_HISTORY_DIR", self.root.path().join("history"))
            .env_remove("ARC_DB_PATH");
        cmd
    }

    fn arc(&self, dir: &Path, args: &[&str]) -> Output {
        self.command(dir, args).output().expect("spawn arc")
    }

    /// `arc run` in `dir`, which has to succeed; its stdout.
    fn run_ok(&self, dir: &Path) -> String {
        let out = self.arc(dir, &["run"]);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "arc run in {} failed:\nstdout:\n{stdout}\nstderr:\n{}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }
}

/// The database of the Protocol `name` in `dir` under the data folder `root`: the first
/// sixteen hex digits of the SHA-256 of `dir`'s canonical path, then `<name>.duckdb`.
fn expected_db(root: &Path, dir: &Path, name: &str) -> PathBuf {
    let canonical = dir.canonicalize().unwrap();
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let key: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    root.join(key).join(format!("{name}.duckdb"))
}

/// Every file under `dir` whose name ends `.duckdb` or `.wal`.
fn databases_under(dir: &Path) -> Vec<PathBuf> {
    files_under(dir)
        .into_keys()
        .filter(|p| {
            let name = p.to_string_lossy();
            name.ends_with(".duckdb") || name.ends_with(".wal")
        })
        .collect()
}

/// Every file under `dir`, by its path, with its bytes.
fn files_under(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

/// Copy the directory `from` whole to `to`.
fn copy_dir(from: &Path, to: &Path) {
    for (path, bytes) in files_under(from) {
        let dest = to.join(path.strip_prefix(from).unwrap());
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, bytes).unwrap();
    }
}

/// One value from the database at `db`, read through the DuckDB CLI.
fn query(db: &Path, sql: &str) -> String {
    let out = Command::new("duckdb")
        .arg(db)
        .args(["-noheader", "-list", "-readonly", "-c", sql])
        .output()
        .expect("spawn duckdb");
    assert!(out.status.success(), "duckdb {}: {:?}", db.display(), out);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `out` has to have succeeded.
fn ok(out: Output, what: &str) -> Output {
    assert!(
        out.status.success(),
        "{what} failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

// A Protocol `arc create-protocol` makes with no `--db`, given a first step with `arc
// edit-protocol` and run with `arc run`, has no file ending `.duckdb` or `.wal` anywhere
// under its directory; its database is in the data folder, keyed to the directory —
// `$ARCFORM_DB_DIR` when it is set, `~/.arcform/db` when it is not; and the run leaves
// the manifest as the authoring verbs wrote it.
#[test]
fn a_protocol_arc_creates_runs_with_its_database_in_the_data_folder_and_none_in_its_directory() {
    for told in [true, false] {
        let fx = Fixture::new();
        let home = fx.root.path().join("home");
        let work = fx.root.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let data = if told {
            fx.db_root()
        } else {
            home.join(".arcform").join("db")
        };
        let arc = |dir: &Path, args: &[&str]| {
            let mut cmd = fx.command(dir, args);
            cmd.env("HOME", &home);
            if !told {
                cmd.env_remove("ARCFORM_DB_DIR");
            }
            ok(cmd.output().expect("spawn arc"), &format!("arc {args:?}"))
        };

        arc(&work, &["create-protocol", "fieldbook"]);
        let dir = work.join("fieldbook");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(
            dir.join("models/load.sql"),
            "CREATE OR REPLACE TABLE field AS SELECT 'sun' AS sky, 4 AS hours;\n",
        )
        .unwrap();
        arc(
            &work,
            &[
                "edit-protocol",
                "--dir",
                "fieldbook",
                "replace",
                "steps",
                "\n  - name: load\n    sql: models/load.sql",
            ],
        );
        let authored = std::fs::read(dir.join("arcform.yaml")).unwrap();
        assert!(
            !String::from_utf8_lossy(&authored)
                .lines()
                .any(|l| l.starts_with("db")),
            "no db line"
        );

        arc(&dir, &["run"]);

        assert_eq!(
            databases_under(&dir),
            Vec::<PathBuf>::new(),
            "told={told}: no database file under the Protocol directory"
        );
        let db = expected_db(&data, &dir, "fieldbook");
        assert!(
            db.is_file(),
            "told={told}: the database at {}",
            db.display()
        );
        assert_eq!(query(&db, "SELECT hours FROM field"), "4");
        assert_eq!(
            std::fs::read(dir.join("arcform.yaml")).unwrap(),
            authored,
            "the run leaves the manifest as it was"
        );
        let other = if told {
            home.join(".arcform")
        } else {
            fx.db_root()
        };
        assert!(
            !other.exists(),
            "told={told}: nothing at {}",
            other.display()
        );
    }
}

/// Three SQL steps with `db` unset: a table, a count of it, and the count written to a
/// file under the Protocol directory.
const TALLY: &str = "name: tally
steps:
  - name: load
    sql: models/load.sql
  - name: count
    sql: models/count.sql
  - name: export
    sql: models/export.sql
";

const TALLY_FILES: &[(&str, &str)] = &[
    (
        "models/load.sql",
        "CREATE OR REPLACE TABLE t AS SELECT * FROM range(3) r(i);\n",
    ),
    (
        "models/count.sql",
        "CREATE OR REPLACE TABLE n AS SELECT count(*) AS c FROM t;\n",
    ),
    ("models/export.sql", "COPY n TO 'n.csv' (HEADER false);\n"),
];

const TALLY_SQL_STEPS: [&str; 3] = ["load", "count", "export"];

// Two Protocols with one `name:` in two directories, each with `db` unset, run to two
// databases; and a second run of one with nothing changed finds its state where the
// first run left it, so every SQL step skips as fresh.
#[test]
fn one_name_in_two_directories_runs_to_two_databases_and_a_second_run_skips_every_sql_step() {
    let fx = Fixture::new();
    let a = fx.protocol("a/tally", TALLY, TALLY_FILES);
    let b = fx.protocol("b/tally", TALLY, TALLY_FILES);

    fx.run_ok(&a);
    fx.run_ok(&b);

    let db_a = expected_db(&fx.db_root(), &a, "tally");
    let db_b = expected_db(&fx.db_root(), &b, "tally");
    assert_ne!(db_a, db_b, "one name in two directories, two keys");
    assert!(db_a.is_file(), "a's database at {}", db_a.display());
    assert!(db_b.is_file(), "b's database at {}", db_b.display());
    for (db, dir) in [(&db_a, &a), (&db_b, &b)] {
        assert_eq!(query(db, "SELECT c FROM n"), "3", "{}", db.display());
        let marker = db.parent().unwrap().join("protocol-path");
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!("{}\n", dir.canonicalize().unwrap().display()),
            "the protocol-path file names the directory"
        );
    }

    let second = fx.run_ok(&a);
    for step in TALLY_SQL_STEPS {
        assert_eq!(
            step_outcome(&second, step),
            "skip: hash_clean",
            "{step} on a second run with nothing changed:\n{second}"
        );
    }
}

// A Protocol directory copied whole after a run runs where it lands: every SQL step runs
// again against a database of the copy's own, its outputs land under the copy, and the
// first location's database is byte for byte as it was.
#[test]
fn a_directory_copied_whole_runs_where_it_lands_and_leaves_the_first_database_as_it_was() {
    let fx = Fixture::new();
    let first = fx.protocol("first/tally", TALLY, TALLY_FILES);
    fx.run_ok(&first);
    let first_db = expected_db(&fx.db_root(), &first, "tally");
    assert!(first_db.is_file(), "{}", first_db.display());
    let first_db_files = files_under(first_db.parent().unwrap());
    let first_files = files_under(&first);

    let copy = fx.root.path().join("elsewhere/tally");
    copy_dir(&first, &copy);
    assert_eq!(
        databases_under(&copy),
        Vec::<PathBuf>::new(),
        "the copy carries no database"
    );
    // So a rewrite of the output under the copy is visible, whatever the clock reads.
    let output = copy.join("n.csv");
    filetime::set_file_mtime(&output, filetime::FileTime::from_unix_time(1, 0)).unwrap();

    let stdout = fx.run_ok(&copy);
    for step in TALLY_SQL_STEPS {
        assert_eq!(step_outcome(&stdout, step), "ran", "{step}:\n{stdout}");
    }
    let copy_db = expected_db(&fx.db_root(), &copy, "tally");
    assert_ne!(copy_db, first_db);
    assert_eq!(query(&copy_db, "SELECT c FROM n"), "3");
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "3\n");
    assert!(
        filetime::FileTime::from_last_modification_time(&std::fs::metadata(&output).unwrap())
            .unix_seconds()
            > 1,
        "export rewrote n.csv under the copy"
    );
    assert_eq!(databases_under(&copy), Vec::<PathBuf>::new());

    assert!(
        files_under(first_db.parent().unwrap()) == first_db_files,
        "the first location's database is byte for byte as it was"
    );
    assert!(
        files_under(&first) == first_files,
        "and so is the first location's directory"
    );
}

// With `db` unset, `arc run` names the database's path once, and a `command:` step and
// every hook read that path from `ARC_DB_PATH`, so a step that runs the DuckDB CLI by
// hand opens the database the SQL steps ran on.
#[test]
fn arc_run_names_the_database_once_and_steps_and_hooks_read_it_from_arc_db_path() {
    let fx = Fixture::new();
    let hook = |name: &str| {
        format!(
            "    name: {name}\n    command: \"printf '%s' \\\"$ARC_DB_PATH\\\" > {name}.txt\"\n"
        )
    };
    let manifest = format!(
        "name: reach
hooks:
  on_init:
{}  on_success:
{}  on_exit:
{}steps:
  - name: load
    sql: models/load.sql
  - name: peek
    command: \"duckdb \\\"$ARC_DB_PATH\\\" -noheader -list -c 'SELECT count(*) FROM t' > peek.txt\"
",
        hook("init"),
        hook("success"),
        hook("exit"),
    );
    let dir = fx.protocol(
        "reach",
        &manifest,
        &[(
            "models/load.sql",
            "CREATE OR REPLACE TABLE t AS SELECT * FROM range(4) r(i);\n",
        )],
    );

    let stdout = strip_ansi(&fx.run_ok(&dir));
    let db = expected_db(&fx.db_root(), &dir, "reach");
    let named: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("database:"))
        .collect();
    assert_eq!(
        named,
        vec![format!("database: {}", db.display())],
        "the database is named once:\n{stdout}"
    );

    assert_eq!(
        std::fs::read_to_string(dir.join("peek.txt"))
            .unwrap()
            .trim(),
        "4",
        "the command step opened the database the SQL step built"
    );
    for name in ["init", "success", "exit"] {
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap(),
            db.display().to_string(),
            "the {name} hook reads the same path"
        );
    }
    assert_eq!(databases_under(&dir), Vec::<PathBuf>::new());
}

// `examples/almanac` names no `db:`. Run from a copy, it produces the report it produced
// before its database moved, and nothing ending `.duckdb` or `.wal` is written under it.
#[test]
fn almanac_runs_to_its_report_with_no_database_in_its_directory() {
    let fx = Fixture::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/almanac");
    let dir = fx.root.path().join("almanac");
    for name in ["arcform.yaml", ".gitignore", "README.md"] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(source.join(name), dir.join(name)).unwrap();
    }

    let stdout = strip_ansi(&fx.run_ok(&dir));
    // The outputs `arc run` wrote for this example at 4c521e3, before its database moved.
    assert_eq!(
        std::fs::read_to_string(dir.join("data/report.csv")).unwrap(),
        "1,5.1\n2,5.4\n3,5.9\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("data/rows.txt")).unwrap(),
        "3\n"
    );
    assert_eq!(databases_under(&dir), Vec::<PathBuf>::new(), "{stdout}");
    assert!(expected_db(&fx.db_root(), &dir, "almanac").is_file());
}

// A run refused before its first step writes nothing in arc's data folder and runs
// nothing: a `name:` that would put the database outside its keyed directory, and a
// `protocol-path` file arc cannot write, each refuse the run with what to look at.
#[test]
fn a_run_arc_cannot_place_in_the_data_folder_is_refused_before_any_step() {
    let fx = Fixture::new();
    let step = "steps:\n  - name: mark\n    command: \"touch ran\"\n";

    let escaping = fx.protocol("escaping", &format!("name: ../shared\n{step}"), &[]);
    let out = fx.arc(&escaping, &["run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a name that leaves its keyed directory"
    );
    assert!(stderr.contains("'../shared'"), "{stderr}");
    assert!(!escaping.join("ran").exists(), "no step ran");
    assert!(!fx.db_root().exists(), "nothing in the data folder");

    let blocked = fx.protocol("blocked", &format!("name: blocked\n{step}"), &[]);
    let marker = expected_db(&fx.db_root(), &blocked, "blocked")
        .parent()
        .unwrap()
        .join("protocol-path");
    std::fs::create_dir_all(&marker).unwrap();
    let out = fx.arc(&blocked, &["run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a protocol-path arc cannot write");
    assert!(stderr.contains(&marker.display().to_string()), "{stderr}");
    assert!(!blocked.join("ran").exists(), "no step ran");
}
