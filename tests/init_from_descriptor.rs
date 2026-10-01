//! End-to-end: `arc init --from-descriptor` generates a runnable Protocol from a
//! Frictionless descriptor, and `arc run` executes the GENERATED manifest.
//!
//! Self-contained: the committed `tests/fixtures/signups.datapackage.json` (and
//! its `signups.duckdb`) were produced by running the shipped `dovetail relate`
//! over `tests/fixtures/build.sql`. This test needs no sibling repo — only the
//! `arc` binary and a `duckdb` CLI on PATH (the same requirement as any `arc run`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Read the single run contract under `<project>/build/.arcform/runs/*.json`.
fn read_contract(project: &Path) -> serde_json::Value {
    let runs = project.join("build/.arcform/runs");
    let mut json_files: Vec<PathBuf> = std::fs::read_dir(&runs)
        .unwrap_or_else(|e| panic!("runs dir {}: {e}", runs.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        json_files.len(),
        1,
        "expected exactly one contract, got {json_files:?}"
    );
    let bytes = std::fs::read(json_files.remove(0)).unwrap();
    serde_json::from_slice(&bytes).expect("contract is valid JSON")
}

#[test]
fn generates_and_runs_a_protocol_from_a_descriptor() {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = fixtures_dir().join("signups.datapackage.json");

    // --- arc init --from-descriptor -----------------------------------------
    let init = Command::new(arc)
        .current_dir(workspace.path())
        .args(["init", "assemble_demo", "--from-descriptor"])
        .arg(&descriptor)
        .output()
        .expect("spawn arc init");
    assert!(
        init.status.success(),
        "arc init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let project = workspace.path().join("assemble_demo");
    assert!(project.join("arcform.yaml").is_file());
    assert!(
        project.join("signups.duckdb").is_file(),
        "database copied in"
    );
    assert!(
        project.join("datapackage.json").is_file(),
        "companion descriptor written"
    );

    // --- generated arcform.yaml: exactly three steps, in order --------------
    let yaml = std::fs::read_to_string(project.join("arcform.yaml")).unwrap();
    let manifest: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let steps = manifest["steps"].as_sequence().expect("steps sequence");
    let step_names: Vec<&str> = steps.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(
        step_names,
        ["load_accounts", "load_logins", "fk_logins_accounts"],
        "generated steps"
    );

    // The fk step fans in from both tables.
    let fk = steps
        .iter()
        .find(|s| s["name"] == "fk_logins_accounts")
        .unwrap();
    let deps: Vec<&str> = fk["depends_on"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        deps.contains(&"logins") && deps.contains(&"accounts"),
        "fk depends_on both tables: {deps:?}"
    );

    // The fk model joins account_id -> id.
    let fk_sql = std::fs::read_to_string(project.join("models/fk_logins_accounts.sql")).unwrap();
    assert!(
        fk_sql.contains(r#"c."account_id" = p."id""#),
        "fk joins account_id -> id: {fk_sql}"
    );

    // --- arc run on the GENERATED project -----------------------------------
    let run = Command::new(arc)
        .current_dir(&project)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .arg("run")
        .output()
        .expect("spawn arc run");
    assert!(
        run.status.success(),
        "arc run failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    // --- the emitted b4/1 contract ------------------------------------------
    let contract = read_contract(&project);
    assert_eq!(contract["contract_version"], "b4/1");
    assert_eq!(contract["run"]["outcome"], "success");

    let assets = contract["assets"].as_array().expect("assets array");
    let find = |name: &str| assets.iter().find(|a| a["name"] == name);

    let accounts = find("accounts").expect("accounts asset");
    let logins = find("logins").expect("logins asset");
    let fk_asset = find("fk_logins_accounts").expect("fk asset");

    // Each table is produced by its load step.
    assert_eq!(accounts["produced_by"], "load_accounts");
    assert_eq!(logins["produced_by"], "load_logins");
    assert_eq!(fk_asset["produced_by"], "fk_logins_accounts");

    // The fk asset consumes BOTH tables: accounts and logins are each consumed by
    // the fk step.
    let consumed_by = |a: &serde_json::Value| -> Vec<String> {
        a["consumed_by"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert!(
        consumed_by(accounts).contains(&"fk_logins_accounts".to_string()),
        "accounts consumed by fk"
    );
    assert!(
        consumed_by(logins).contains(&"fk_logins_accounts".to_string()),
        "logins consumed by fk"
    );
}

/// What `arc init assemble_demo --from-descriptor signups.datapackage.json` wrote into
/// `arcform.yaml` on 4c521e3, byte for byte, with the `db: assemble_demo.duckdb` line taken
/// out: recorded by running that commit's binary. The generated Protocol names no database
/// now, and every other byte is the one that commit wrote.
const GENERATED_BEFORE: &str = r#"# Generated by `arc init --from-descriptor` from signups.datapackage.json.
# The descriptor DESCRIBES the data (discovered tables, semantic types,
# verified foreign keys); this Protocol EXECUTES it. Each load step copies a
# described table out of the attached database; each accepted foreign key
# becomes a validating anti-join that counts orphan rows. Edit freely.
name: assemble_demo
engine: duckdb
engine_version: '>=1.0'
params: {}
dotenv: []
timeout_sec: null
defaults: null
hooks:
  on_init: null
  on_success: null
  on_failure: null
  on_exit: null
steps:
- name: load_accounts
  sql: models/load_accounts.sql
  command: null
  op: null
  with: null
  produces: []
  depends_on: []
  preconditions: []
  output: null
  retry: null
  timeout_sec: null
- name: load_logins
  sql: models/load_logins.sql
  command: null
  op: null
  with: null
  produces: []
  depends_on: []
  preconditions: []
  output: null
  retry: null
  timeout_sec: null
- name: fk_logins_accounts
  sql: models/fk_logins_accounts.sql
  command: null
  op: null
  with: null
  produces: []
  depends_on:
  - logins
  - accounts
  preconditions: []
  output: null
  retry: null
  timeout_sec: null
assets: {}
"#;

/// The manifest `--from-descriptor` generates names no database, its bytes are otherwise
/// what they were, and `arc run` on it builds `<name>.duckdb` in arc's data folder rather
/// than beside the manifest.
#[test]
fn the_generated_manifest_names_no_database_and_run_builds_it_in_arcs_data_folder() {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = fixtures_dir().join("signups.datapackage.json");

    let init = Command::new(arc)
        .current_dir(workspace.path())
        .args(["init", "assemble_demo", "--from-descriptor"])
        .arg(&descriptor)
        .output()
        .expect("spawn arc init");
    assert!(
        init.status.success(),
        "arc init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let project = workspace.path().join("assemble_demo");
    let yaml = std::fs::read_to_string(project.join("arcform.yaml")).unwrap();
    assert!(
        !yaml.lines().any(|l| l.starts_with("db")),
        "no db line:\n{yaml}"
    );
    assert_eq!(yaml, GENERATED_BEFORE, "every other byte is as it was");
    assert!(
        !project.join("assemble_demo.duckdb").exists(),
        "generating a Protocol builds no database of its own name"
    );

    let data = workspace.path().join("db");
    let run = Command::new(arc)
        .current_dir(&project)
        .env("ARCFORM_DB_DIR", &data)
        .arg("run")
        .output()
        .expect("spawn arc run");
    assert!(
        run.status.success(),
        "arc run failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        !project.join("assemble_demo.duckdb").exists(),
        "a manifest with no db builds no <name>.duckdb beside it"
    );
    let built: Vec<_> = std::fs::read_dir(&data)
        .unwrap()
        .map(|key| key.unwrap().path().join("assemble_demo.duckdb"))
        .collect();
    assert!(
        built.len() == 1 && built[0].is_file(),
        "it builds <name>.duckdb in one keyed directory of the data folder: {built:?}"
    );
}

/// `git` in `dir`, with the developer's own configuration out of reach: a global ignore
/// file that names `*.duckdb` would make a database look as though arc's list had kept
/// it out, and a test that passed for that reason would pass whatever arc wrote. The
/// same helper `tests/cli_authoring.rs` uses to ask what `git add --all` stages.
fn git(dir: &Path, args: &[&str]) -> String {
    let home = std::env::temp_dir().join("arc-descriptor-git-home");
    let out = Command::new("git")
        .current_dir(dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("spawn git: these tests read what `git add --all` stages");
    assert!(
        out.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The paths `git add --all` has staged in `dir` (a repository is made there if there
/// is none), sorted.
fn staged_by_add_all(dir: &Path) -> Vec<String> {
    git(dir, &["init", "-q"]);
    git(dir, &["add", "--all"]);
    let listing = git(dir, &["ls-files", "--cached", "-z"]);
    let mut paths: Vec<String> = listing
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    paths.sort();
    paths
}

/// What the generated Protocol holds that git is meant to take: the manifest, the
/// models, the companion descriptor and the list itself.
const SHARED_FILES: [&str; 6] = [
    ".gitignore",
    "arcform.yaml",
    "datapackage.json",
    "models/fk_logins_accounts.sql",
    "models/load_accounts.sql",
    "models/load_logins.sql",
];

/// The lines of an ignore list that name something, with its comments left out.
fn patterns(list: &str) -> Vec<&str> {
    list.lines().filter(|l| !l.starts_with('#')).collect()
}

/// `--from-descriptor` copies the database the descriptor names into the directory and
/// keeps it out of git: after a run, `git add --all` stages the manifest, the models
/// and the descriptor, and the list names the database and its write-ahead log. The
/// lines it prints naming what it made name the list and say the database is kept out.
#[test]
fn the_copied_database_is_kept_out_of_git_and_the_summary_says_so() {
    let workspace = tempfile::tempdir().unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(workspace.path())
        .args(["init", "assemble_demo", "--from-descriptor"])
        .arg(fixtures_dir().join("signups.datapackage.json"))
        .output()
        .expect("spawn arc init");
    assert!(
        init.status.success(),
        "arc init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let project = workspace.path().join("assemble_demo");
    let list = std::fs::read_to_string(project.join(".gitignore")).unwrap();
    assert_eq!(
        patterns(&list),
        ["/build/.arcform/", "/signups.duckdb", "/signups.duckdb.wal"],
        "the list names the run records and the copied database with its log:\n{list}"
    );
    let stdout = String::from_utf8_lossy(&init.stdout);
    let summary = stdout
        .lines()
        .find(|l| l.trim_start().starts_with(".gitignore"))
        .unwrap_or_else(|| panic!("the summary names the list among what it made:\n{stdout}"));
    assert!(
        summary.contains("kept out of git"),
        "the line naming the list says the copied database is kept out of git: {summary}"
    );

    // The control: the same database in a directory with no list is staged. Without it
    // the assertions below could pass for a reason that is not arc's list.
    let control = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixtures_dir().join("signups.duckdb"),
        control.path().join("signups.duckdb"),
    )
    .unwrap();
    assert_eq!(
        staged_by_add_all(control.path()),
        ["signups.duckdb"],
        "this git stages a database nothing ignores"
    );

    let run = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&project)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .arg("run")
        .output()
        .expect("spawn arc run");
    assert!(
        run.status.success(),
        "arc run failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    // The log exists only while DuckDB has writes to replay; put one there so the check
    // is on a file that is.
    std::fs::write(project.join("signups.duckdb.wal"), b"wal").unwrap();
    assert!(project.join("signups.duckdb").is_file());

    assert_eq!(
        staged_by_add_all(&project),
        SHARED_FILES,
        "the Protocol is staged, and no database it copied in nor that database's log is"
    );
}

/// Each database a descriptor names is copied in and kept out of git, not the first
/// alone: two databases give two names in the list and two files git does not stage.
#[test]
fn each_database_a_descriptor_names_is_kept_out_of_git() {
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("source");
    std::fs::create_dir(&source).unwrap();
    for db in ["signups.duckdb", "second logins.duckdb"] {
        std::fs::copy(fixtures_dir().join("signups.duckdb"), source.join(db)).unwrap();
    }
    let descriptor = std::fs::read_to_string(fixtures_dir().join("signups.datapackage.json"))
        .unwrap()
        .replace("signups.duckdb#logins", "second logins.duckdb#logins");
    assert!(descriptor.contains("second logins.duckdb#logins"));
    std::fs::write(source.join("datapackage.json"), descriptor).unwrap();

    let init = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(workspace.path())
        .args(["init", "two_dbs", "--from-descriptor"])
        .arg(source.join("datapackage.json"))
        .output()
        .expect("spawn arc init");
    assert!(
        init.status.success(),
        "arc init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let project = workspace.path().join("two_dbs");
    for db in ["signups.duckdb", "second logins.duckdb"] {
        assert!(project.join(db).is_file(), "{db}: copied in");
        std::fs::write(project.join(format!("{db}.wal")), b"wal").unwrap();
    }
    let list = std::fs::read_to_string(project.join(".gitignore")).unwrap();
    assert_eq!(
        patterns(&list),
        [
            "/build/.arcform/",
            "/signups.duckdb",
            "/signups.duckdb.wal",
            "/second\\ logins.duckdb",
            "/second\\ logins.duckdb.wal"
        ],
        "the list names each copied database with its log:\n{list}"
    );
    assert_eq!(
        staged_by_add_all(&project),
        SHARED_FILES,
        "neither database nor either log is staged"
    );
}

/// A copy of the directory that does not hold the database — a clone, since git does
/// not take it — fails `arc run` with a non-zero exit and a message naming the file.
#[test]
fn a_run_where_the_copied_database_is_missing_fails_naming_the_file() {
    let workspace = tempfile::tempdir().unwrap();
    let init = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(workspace.path())
        .args(["init", "clone_of_demo", "--from-descriptor"])
        .arg(fixtures_dir().join("signups.datapackage.json"))
        .output()
        .expect("spawn arc init");
    assert!(
        init.status.success(),
        "arc init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let project = workspace.path().join("clone_of_demo");
    std::fs::remove_file(project.join("signups.duckdb")).unwrap();

    let run = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&project)
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
        .arg("run")
        .output()
        .expect("spawn arc run");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        matches!(run.status.code(), Some(code) if code != 0),
        "arc run exits non-zero without the database, got {:?}:\n{stderr}",
        run.status.code()
    );
    assert!(
        stderr.contains("signups.duckdb"),
        "the message names the missing file:\n{stderr}"
    );
    assert!(
        !project.join("signups.duckdb").exists(),
        "the run did not make a database in the file's place"
    );
}
