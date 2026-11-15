//! Running one test: a copy of the Protocol beside it, its manifest rewritten, its fixtures
//! placed, `arc run` on the copy as a child of this process, and the outcome read from the
//! child's exit, its `error:` line and the copy's run record.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_yaml::Value;

use crate::asset::AssetGraph;
use crate::check::{ERROR_FILE_ENV, MEASURE_HISTORY_ENV};
use crate::contract::{CheckSection, Contract};
use crate::engine::Engine;
use crate::manifest::{MANIFEST_FILENAME, Manifest};
use crate::runner::{PUBLISH_ENV, strip_ansi};

use super::ProtocolTest;
use super::fixture::{RoundTrip, place};
use super::plan::{TestPlan, plan_test};

/// How one test came out.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    Passed,
    /// `what` says what happened against what was expected; it may run to several lines.
    Failed {
        what: String,
    },
}

/// One test's name, risk and outcome, and how long it took.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TestResult {
    pub name: String,
    pub risk: String,
    pub outcome: Outcome,
    pub seconds: f64,
}

/// The name a test's inline check `n` (1-based) has as a step in the copy's manifest.
const CHECK_STEP_PREFIX: &str = "arc-test-check-";

/// Where in the copy the child `arc run` writes its error, through [`ERROR_FILE_ENV`].
const ERROR_FILE: &str = ".arc-test/error.txt";

/// The folder in the copy that holds the test's inline checks as files.
const CHECKS_DIR: &str = ".arc-test/checks";

/// The database a copy runs with when the manifest names none, so a test leaves nothing
/// in arc's data folder.
const TEST_DB: &str = "build/arc-test.duckdb";

/// How many of a failing test check's rows the failure shows.
const SAMPLE_LINES: usize = 5;

/// How long to wait, once the child `arc run` has exited, for the rest of what it printed.
const READER_GRACE: Duration = Duration::from_secs(2);

/// Run `test` against the Protocol in `dir`, whose manifest and asset graph are given, and
/// say whether it passed. Never panics on a run that failed early: a refusal, a copy that
/// could not be made and a run with no record are all failing outcomes.
pub(crate) fn run_test(
    dir: &Path,
    manifest: &Manifest,
    graph: &AssetGraph,
    test: &ProtocolTest,
    engine: &dyn Engine,
) -> TestResult {
    let started = Instant::now();
    let outcome = match judge(dir, manifest, graph, test, engine) {
        Ok(()) => Outcome::Passed,
        Err(what) => Outcome::Failed { what },
    };
    TestResult {
        name: test.name.clone(),
        risk: test.risk.clone(),
        outcome,
        seconds: started.elapsed().as_secs_f64(),
    }
}

/// `Ok` when `test` passes, else what went wrong.
fn judge(
    dir: &Path,
    manifest: &Manifest,
    graph: &AssetGraph,
    test: &ProtocolTest,
    engine: &dyn Engine,
) -> std::result::Result<(), String> {
    let plan = plan_test(manifest, graph, dir, test)?;
    let run = run_in_copy(dir, test, &plan, engine, false)?;
    let Some(expect) = &test.expect else {
        if let Some((n, check)) = &run.failed_check {
            return Err(check_failure(test, *n, check));
        }
        return match run.failure {
            Some(message) => Err(format!(
                "expected the run to succeed; it failed with: {message}"
            )),
            None => Ok(()),
        };
    };
    let text = &expect.fails_with;
    match run.failure {
        None => Err(format!(
            "expected the run to fail with \"{text}\"; it succeeded"
        )),
        Some(message) if !message.contains(text.as_str()) => Err(format!(
            "expected the run to fail with \"{text}\"; it failed with: {message}"
        )),
        Some(_) => {
            // The break must be what makes the run fail: the same rewrite with no
            // statements must not fail with the same message.
            if plan.placements.iter().any(|p| p.statements.is_some()) {
                let control = run_in_copy(dir, test, &plan, engine, true)?;
                if control
                    .failure
                    .is_some_and(|message| message.contains(text.as_str()))
                {
                    return Err(format!(
                        "the control run, with the fixtures rewritten and no break, also \
                         failed with \"{text}\", so the failure does not come from the break"
                    ));
                }
            }
            Ok(())
        }
    }
}

/// `check <n> returned <k> rows: <query>`, then up to [`SAMPLE_LINES`] of its rows, one
/// per line and indented, cells joined by ` | `.
fn check_failure(test: &ProtocolTest, n: usize, check: &CheckSection) -> String {
    let query = test
        .checks
        .get(n - 1)
        .map(|q| q.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let mut what = format!("check {n} returned {} rows: {query}", check.rows_returned);
    for row in check.sample.iter().take(SAMPLE_LINES) {
        let cells: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap_or("null")).collect();
        what.push_str("\n  ");
        what.push_str(&cells.join(" | "));
    }
    what
}

/// What one child run left.
struct ChildOutcome {
    /// The text after `error: ` on its standard error, when it failed.
    failure: Option<String>,
    /// The first of the test's own checks that returned rows, with its number.
    failed_check: Option<(usize, CheckSection)>,
}

/// Make a fresh copy of the Protocol, set it up for `test` as `plan` says, run it, and
/// remove the copy. `control` places each broken fixture through the round trip with no
/// statements, as the control run does. `Err` is a refusal that fails the test.
fn run_in_copy(
    dir: &Path,
    test: &ProtocolTest,
    plan: &TestPlan,
    engine: &dyn Engine,
    control: bool,
) -> std::result::Result<ChildOutcome, String> {
    let dir = std::fs::canonicalize(dir)
        .map_err(|e| format!("the Protocol folder {} cannot be read: {e}", dir.display()))?;
    let copy = ProtocolCopy::make(&dir)?;
    rewrite_manifest(copy.path(), test, plan)?;
    for placement in &plan.placements {
        let round_trip = match (&placement.statements, control) {
            (None, _) => RoundTrip::None,
            (Some(_), false) => RoundTrip::Break,
            (Some(_), true) => RoundTrip::Control,
        };
        place(copy.path(), placement, &dir, engine, round_trip)?;
    }
    let history = test.measure_history.as_ref().map(|h| dir.join(h));
    let child = run_child(copy.path(), test, history.as_deref())?;
    Ok(ChildOutcome {
        failure: child,
        failed_check: failed_test_check(copy.path()),
    })
}

/// A copy of a Protocol folder, removed when dropped, on every return path.
struct ProtocolCopy(PathBuf);

impl ProtocolCopy {
    /// Copy `dir` (absolute) into a new hidden sibling `.<folder>.arc-test-<random>`, so a
    /// path the Protocol reaches with `..` resolves to the same file from the copy. Leaves
    /// out `build/` and the database the manifest's `db:` names, with its `.wal`, so the
    /// copy starts with no state and no earlier run records.
    fn make(dir: &Path) -> std::result::Result<ProtocolCopy, String> {
        let cannot = |e: std::io::Error| {
            format!(
                "a copy of the Protocol could not be made beside {}: {e}",
                dir.display()
            )
        };
        let (Some(parent), Some(folder)) = (dir.parent(), dir.file_name()) else {
            return Err(format!(
                "a copy of the Protocol could not be made: {} has no parent folder",
                dir.display()
            ));
        };
        let folder = folder.to_string_lossy();
        let mut attempt = 0u32;
        let copy = loop {
            let candidate = parent.join(format!(".{folder}.arc-test-{}", random_suffix(attempt)));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break ProtocolCopy(candidate),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 16 => {
                    attempt += 1;
                }
                Err(e) => return Err(cannot(e)),
            }
        };
        let mut skip = vec![PathBuf::from("build")];
        if let Some(db) = Manifest::load(dir).ok().and_then(|m| m.db) {
            let db = clean(Path::new(&db));
            let mut wal = db.clone().into_os_string();
            wal.push(".wal");
            skip.push(PathBuf::from(wal));
            skip.push(db);
        }
        copy_tree(dir, copy.path(), Path::new(""), &skip).map_err(cannot)?;
        Ok(copy)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ProtocolCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A name part no other copy is likely to hold: this process, the clock and `attempt`.
fn random_suffix(attempt: u32) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{:x}{:x}{attempt}", std::process::id(), nanos & 0xffff_ffff)
}

/// Whether the relative `path` stays inside `copy`: not absolute, not reaching above it
/// with `..`, and not through a linked folder that leads out of it. The deepest part of
/// the path that exists (a dangling link counts, and refuses) is resolved, links and all,
/// and must lie under the copy.
fn inside_copy(copy: &Path, path: &Path) -> bool {
    let path = clean(path);
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    let Ok(root) = std::fs::canonicalize(copy) else {
        return false;
    };
    let full = root.join(&path);
    let Some(existing) = full.ancestors().find(|a| a.symlink_metadata().is_ok()) else {
        return false;
    };
    std::fs::canonicalize(existing).is_ok_and(|resolved| resolved.starts_with(&root))
}

/// `path` without its `.` components, so `./data.duckdb` and `data.duckdb` compare equal.
fn clean(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// Copy everything under `from/relative` to `to/relative`, leaving out each path in
/// `skip` (relative to `from`). A symbolic link is copied as a link where the platform
/// has them, so a link to a folder above is not followed.
fn copy_tree(from: &Path, to: &Path, relative: &Path, skip: &[PathBuf]) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from.join(relative))? {
        let entry = entry?;
        let rel = relative.join(entry.file_name());
        if skip.contains(&rel) {
            continue;
        }
        let source = entry.path();
        let dest = to.join(&rel);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir(&dest)?;
            copy_tree(from, to, &rel, skip)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(&source)?, &dest)?;
            #[cfg(not(unix))]
            std::fs::copy(&source, &dest).map(|_| ())?;
        } else {
            std::fs::copy(&source, &dest)?;
        }
    }
    Ok(())
}

/// Rewrite the copy's `arcform.yaml` as a YAML value, so every key it does not touch is
/// kept: take out the steps `plan` removes, append the test's inline checks as `error`
/// check steps carrying its risk, and give it a database of its own unless it names one
/// inside its folder: a `db:` that is absent, null, absolute or reaching above the folder
/// would be a database outside the copy, the user's own or one in arc's data folder.
fn rewrite_manifest(
    copy: &Path,
    test: &ProtocolTest,
    plan: &TestPlan,
) -> std::result::Result<(), String> {
    let cannot = |e: String| format!("the copy's {MANIFEST_FILENAME} could not be rewritten: {e}");
    let path = copy.join(MANIFEST_FILENAME);
    let text = std::fs::read_to_string(&path).map_err(|e| cannot(e.to_string()))?;
    let mut doc: Value = serde_yaml::from_str(&text).map_err(|e| cannot(e.to_string()))?;
    let Some(map) = doc.as_mapping_mut() else {
        return Err(cannot("it is not a mapping".into()));
    };
    let own_db = map
        .get("db")
        .and_then(Value::as_str)
        .is_some_and(|db| !db.trim().is_empty() && inside_copy(copy, Path::new(db)));
    if !own_db {
        map.insert("db".into(), TEST_DB.into());
    }
    let Some(steps) = map.get_mut("steps").and_then(Value::as_sequence_mut) else {
        return Err(cannot("it has no list of steps".into()));
    };
    steps.retain(|step| {
        let name = step.get("name").and_then(Value::as_str);
        !name.is_some_and(|name| plan.removed_steps.iter().any(|r| r == name))
    });
    if !test.checks.is_empty() {
        std::fs::create_dir_all(copy.join(CHECKS_DIR)).map_err(|e| cannot(e.to_string()))?;
    }
    for (i, query) in test.checks.iter().enumerate() {
        let n = i + 1;
        let file = format!("{CHECKS_DIR}/check-{n}.sql");
        std::fs::write(copy.join(&file), query).map_err(|e| cannot(e.to_string()))?;
        let mut step = serde_yaml::Mapping::new();
        step.insert("name".into(), format!("{CHECK_STEP_PREFIX}{n}").into());
        step.insert("check".into(), file.into());
        step.insert("risk".into(), test.risk.clone().into());
        step.insert("severity".into(), "error".into());
        steps.push(Value::Mapping(step));
    }
    let text = serde_yaml::to_string(&doc).map_err(|e| cannot(e.to_string()))?;
    std::fs::write(&path, text).map_err(|e| cannot(e.to_string()))
}

/// Run `arc run --force` on `copy` from this executable, one `--param` per test parameter,
/// with standard input closed, standard output dropped and standard error read. The child
/// gets `history` as its measure history and never publishes. `Ok(None)` when it
/// succeeded, `Ok(Some(message))` when it failed, `Err` when it could not start.
fn run_child(
    copy: &Path,
    test: &ProtocolTest,
    history: Option<&Path>,
) -> std::result::Result<Option<String>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let cannot = |e: std::io::Error| format!("arc run could not start: {e}");
    let mut command = Command::new(std::env::current_exe().map_err(cannot)?);
    command.args(["run", "--force"]);
    for (key, value) in &test.params {
        command.arg(format!("--param={key}={value}"));
    }
    let error_file = copy.join(ERROR_FILE);
    if let Some(parent) = error_file.parent() {
        std::fs::create_dir_all(parent).map_err(cannot)?;
    }
    let _ = std::fs::remove_file(&error_file);
    command
        .current_dir(copy)
        .env("NO_COLOR", "1")
        .env(ERROR_FILE_ENV, &error_file)
        .env_remove(PUBLISH_ENV)
        .env_remove(MEASURE_HISTORY_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(history) = history {
        command.env(MEASURE_HISTORY_ENV, history);
    }
    let mut child = command.spawn().map_err(cannot)?;

    // A process the run left behind, such as a step's `sleep 30 &`, inherits the pipe and
    // holds it open after `arc run` exits; read on a thread and wait for it a short while.
    let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    if let Some(mut pipe) = child.stderr.take() {
        let stderr = Arc::clone(&stderr);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => stderr
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&chunk[..n]),
                }
            }
            let _ = done_tx.send(());
        });
    }
    let status = child.wait().map_err(cannot)?;
    let _ = done_rx.recv_timeout(READER_GRACE);
    if status.success() {
        return Ok(None);
    }
    // arc wrote its own error to the file; only a child that crashed or was killed did not,
    // and then the error is looked for on its standard error.
    if let Ok(text) = std::fs::read_to_string(&error_file) {
        return Ok(Some(without_duckdb_context(&text)));
    }
    let bytes = stderr.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = strip_ansi(&String::from_utf8_lossy(&bytes));
    Ok(Some(error_message(&text).unwrap_or_else(|| {
        format!("arc run exited with {status} and printed no error")
    })))
}

/// `text` without DuckDB's echo of the failing SQL: each `LINE <n>: …` line and the caret
/// line under it. The echo holds the SQL's own text, an `error('…')` message included, so
/// it is not part of the message a test matches.
fn without_duckdb_context(text: &str) -> String {
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !is_duckdb_context(line))
        .collect();
    kept.join("\n").trim().to_string()
}

/// The error a child that wrote no error file printed: from the last line of standard
/// error that starts with `error: ` to the end, trimmed, with DuckDB's context lines taken
/// out. Only a fallback: a step's own output may hold such a line too.
fn error_message(stderr: &str) -> Option<String> {
    let at = stderr
        .match_indices("error: ")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || stderr[..i].ends_with('\n'))
        .last()?;
    Some(without_duckdb_context(&stderr[at + "error: ".len()..]))
}

/// Whether `line` is DuckDB's echo of the SQL (`LINE 3: SELECT …`) or the caret line
/// pointing into it (only spaces and `^`).
fn is_duckdb_context(line: &str) -> bool {
    let echo = line
        .strip_prefix("LINE ")
        .and_then(|rest| rest.split_once(": "))
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let caret = line.contains('^') && line.chars().all(|c| c == ' ' || c == '^');
    echo || caret
}

/// The first of the test's own checks the copy's run record shows returning rows, with
/// its number. `None` when the run left no record, as a run refused before any step does.
fn failed_test_check(copy: &Path) -> Option<(usize, CheckSection)> {
    let path = crate::contract::newest_contract(&crate::contract::runs_dir(copy))?;
    let record: Contract = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    record.steps.into_iter().find_map(|step| {
        let n = step.name.strip_prefix(CHECK_STEP_PREFIX)?.parse().ok()?;
        let check = step.check?;
        (!check.success && check.error.is_none() && check.rows_returned > 0).then_some((n, check))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_message_is_arcs_last_error_without_duckdbs_context() {
        let stderr = "error: retrying\nwarning: a thing\nnot an error: here\n\
Binder Error: Referenced column \"code\" not found\n\
LINE 1: SELECT CASE WHEN code IS NULL THEN error('codes header has moved') END FROM codes;\n\
                         ^\n\
error: step 'load' failed (exit code 1):\n\
Binder Error: Referenced column \"code\" not found\n\
LINE 1: SELECT CASE WHEN code IS NULL THEN error('codes header has moved') END FROM codes;\n\
                         ^\n";
        assert_eq!(
            error_message(stderr).as_deref(),
            Some(
                "step 'load' failed (exit code 1):\nBinder Error: Referenced column \"code\" not found"
            )
        );
        assert_eq!(error_message("nothing went wrong\n"), None);
    }

    #[test]
    fn the_copy_leaves_out_build_and_the_database() {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().join("p");
        for (path, text) in [
            (
                "arcform.yaml",
                "name: t\ndb: ./data.duckdb\nsteps:\n  - name: s\n    command: \"true\"\n",
            ),
            ("data.duckdb", "db"),
            ("data.duckdb.wal", "wal"),
            ("build/.arcform/runs/r.json", "{}"),
            ("models/m.sql", "SELECT 1;"),
        ] {
            let full = p.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
        let p = std::fs::canonicalize(&p).unwrap();
        let copy = ProtocolCopy::make(&p).unwrap();
        let path = copy.path().to_path_buf();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".p.arc-test-"), "{name}");
        assert_eq!(path.parent(), p.parent());
        assert!(path.join("models/m.sql").is_file());
        assert!(path.join("arcform.yaml").is_file());
        assert!(!path.join("build").exists());
        assert!(!path.join("data.duckdb").exists());
        assert!(!path.join("data.duckdb.wal").exists());
        drop(copy);
        assert!(!path.exists(), "the copy outlived its guard");
    }

    #[test]
    fn only_a_database_inside_the_folder_is_kept() {
        let plan = TestPlan {
            removed_steps: Vec::new(),
            placements: Vec::new(),
        };
        let test = ProtocolTest {
            name: "n".into(),
            risk: "r".into(),
            fixtures: Default::default(),
            breaks: Default::default(),
            expect: None,
            checks: Vec::new(),
            params: Default::default(),
            measure_history: None,
        };
        for (db, kept) in [
            ("data.duckdb", "data.duckdb"),
            ("./build/x.duckdb", "./build/x.duckdb"),
            ("null", TEST_DB),
            ("\"\"", TEST_DB),
            ("/var/x.duckdb", TEST_DB),
            ("../shared.duckdb", TEST_DB),
            ("data/../../x.duckdb", TEST_DB),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join(MANIFEST_FILENAME),
                format!("name: t\ndb: {db}\nsteps:\n  - name: s\n    command: \"true\"\n"),
            )
            .unwrap();
            rewrite_manifest(dir.path(), &test, &plan).unwrap();
            let manifest = Manifest::load(dir.path()).unwrap();
            assert_eq!(manifest.db.as_deref(), Some(kept), "db: {db}");
        }
    }

    #[test]
    fn the_manifest_rewrite_keeps_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MANIFEST_FILENAME),
            "name: t\ncheck_samples: false\nsteps:\n  - name: fetch\n    command: \"true\"\n  - name: load\n    command: \"true\"\n",
        )
        .unwrap();
        let test = ProtocolTest {
            name: "n".into(),
            risk: "a wrong table".into(),
            fixtures: Default::default(),
            breaks: Default::default(),
            expect: None,
            checks: vec!["SELECT 1".into()],
            params: Default::default(),
            measure_history: None,
        };
        let plan = TestPlan {
            removed_steps: vec!["fetch".into()],
            placements: Vec::new(),
        };
        rewrite_manifest(dir.path(), &test, &plan).unwrap();
        let manifest = Manifest::load(dir.path()).unwrap();
        assert_eq!(manifest.db.as_deref(), Some(TEST_DB));
        assert!(!manifest.check_samples);
        let names: Vec<&str> = manifest.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["load", "arc-test-check-1"]);
        let check = &manifest.steps[1];
        assert_eq!(check.check.as_deref(), Some(".arc-test/checks/check-1.sql"));
        assert_eq!(check.risk.as_deref(), Some("a wrong table"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".arc-test/checks/check-1.sql")).unwrap(),
            "SELECT 1"
        );
    }
}
