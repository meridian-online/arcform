//! The log in a Protocol's folder: one line per version local history records,
//! naming the file, the time, the kind, the way, the version's id and the step,
//! asked of the real `arc` binary and of the library calls the app records with.
//!
//!   1. **a recorded step** — `arc operation record` writes a checkpoint line and
//!      a save line naming the step it added, with the times and ids `arc history`
//!      holds; through `arc mcp` the lines name `mcp`;
//!   2. **one log per folder** — a chart file's versions, under `panels/` or
//!      beside the spec, land in the same log as the spec's, each line naming its
//!      file, and a plain save names no step;
//!   3. **no contents** — no line holds a byte of a file, and a line is as long
//!      for a large file as for a small one;
//!   4. **one rule** — every line reads back through the pattern the README
//!      states, a path or step that is not plain is quoted, and a folder arc
//!      cannot write to costs the line and not the version;
//!   5. **git** — `git add --all` in a fresh Protocol stages the log beside
//!      `arcform.yaml` and nothing a run records, and a folder that is no
//!      repository gets its log with no `git` to run;
//!   6. **the list** — `arc history list` ends by saying where the log is, for
//!      the spec and for a file under `panels/`.
//!
//! The README is read for the pattern rather than the pattern being copied
//! here, so a README that drifts from what arc writes fails these tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arc::spec::{
    HistoryKind, HistoryWay, LOG_FILENAME, LocalHistory, MANIFEST_FILENAME, RecordedStep, SpecEdit,
    edit_spec_with_history, record_step_with_history,
};

const MANIFEST: &str = "\
name: shop
engine: duckdb
db: shop.duckdb

steps:
  - name: load_orders
    sql: models/01_orders.sql
";

const ORDERS_SQL: &str = "\
CREATE OR REPLACE TABLE orders AS
SELECT * FROM (VALUES (1, 50), (2, 150)) AS t(id, amount);
";

/// A Protocol whose folder holds no log yet, and a history store of its own
/// outside it.
struct Protocol {
    _root: tempfile::TempDir,
    dir: PathBuf,
    store: PathBuf,
}

impl Protocol {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("shop");
        fs::create_dir_all(dir.join("models")).unwrap();
        fs::write(dir.join(MANIFEST_FILENAME), MANIFEST).unwrap();
        fs::write(dir.join("models/01_orders.sql"), ORDERS_SQL).unwrap();
        let store = root.path().join("history");
        Protocol {
            _root: root,
            dir,
            store,
        }
    }

    /// The store as a library caller opens it, reached by `way`.
    fn history(&self, way: HistoryWay) -> LocalHistory {
        LocalHistory::at_root(&self.store).reached_by(way)
    }

    /// `arc` with `args`, run in the Protocol's folder against its store.
    fn arc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(&self.dir)
            .env("ARCFORM_HISTORY_DIR", &self.store)
            .args(args)
            .output()
            .expect("spawn arc")
    }

    fn arc_ok(&self, args: &[&str]) -> String {
        let out = self.arc(args);
        assert!(
            out.status.success(),
            "arc {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// The log's lines, each read by the README's rule.
    fn lines(&self) -> Vec<Line> {
        let log = fs::read_to_string(self.dir.join(LOG_FILENAME)).expect("the folder's log");
        log.lines().map(Line::read).collect()
    }

    /// Every path under the folder whose name is the log's, relative to it.
    fn logs(&self) -> Vec<String> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else if path.file_name().is_some_and(|n| n == LOG_FILENAME) {
                    out.push(path.strip_prefix(root).unwrap().display().to_string());
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.dir, &self.dir, &mut out);
        out
    }
}

/// One line of the log, read back into its parts.
#[derive(Debug, PartialEq)]
struct Line {
    time: String,
    file: String,
    kind: String,
    way: String,
    id: String,
    /// The step the line names, or `None` for `no step named`.
    step: Option<String>,
}

impl Line {
    /// Read `line` by the pattern the README states, decoding a quoted word as
    /// the README says to.
    fn read(line: &str) -> Line {
        let rule = regex::Regex::new(&readme_rule()).expect("the README's pattern compiles");
        let caps = rule
            .captures(line)
            .unwrap_or_else(|| panic!("the README's pattern does not read {line:?}"));
        let word = |name: &str| caps.name(name).map(|m| unquote(m.as_str()));
        Line {
            time: word("time").unwrap(),
            file: word("file").unwrap(),
            kind: word("kind").unwrap(),
            way: word("way").unwrap(),
            id: word("id").unwrap(),
            step: word("step"),
        }
    }
}

/// A word as the README says to read it: a JSON string decoded, else as written.
fn unquote(word: &str) -> String {
    if word.starts_with('"') {
        serde_json::from_str(word).expect("a quoted word is a JSON string")
    } else {
        word.to_string()
    }
}

/// The one pattern the README's `regex` block states.
fn readme_rule() -> String {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("read the README");
    let (_, after) = readme
        .split_once("```regex\n")
        .expect("the README states the log's rule in a regex block");
    let (rule, _) = after.split_once("\n```").expect("the block closes");
    rule.trim().to_string()
}

/// The time `arc history` holds for `id`, as a line writes it.
fn time_of(history: &LocalHistory, dir: &Path, id: &str) -> String {
    let entry = history
        .entries(dir)
        .unwrap()
        .into_iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("{id} is in the store"));
    humantime::format_rfc3339_millis(entry.at).to_string()
}

/// The ids and kinds `arc history list` prints, oldest first.
fn listed(protocol: &Protocol) -> Vec<(String, String)> {
    protocol
        .arc_ok(&["history", "list"])
        .lines()
        .filter(|line| line.contains(" bytes  "))
        .map(|line| {
            let mut words = line.split_whitespace();
            (
                words.next().unwrap().to_string(),
                words.next().unwrap().to_string(),
            )
        })
        .collect()
}

fn record_big_orders(protocol: &Protocol) {
    protocol.arc_ok(&[
        "operation",
        "record",
        "filter-rows",
        "--on",
        "orders",
        "--name",
        "big_orders",
        "--arg",
        "where=amount > 100",
    ]);
}

// ------------------------------------------------------------ a recorded step

#[test]
fn an_operation_recorded_at_the_terminal_writes_its_checkpoint_and_its_save_naming_the_step() {
    let protocol = Protocol::new();
    assert!(protocol.logs().is_empty(), "the folder starts with no log");

    record_big_orders(&protocol);

    assert_eq!(protocol.logs(), [LOG_FILENAME], "one log, beside the spec");
    let versions = listed(&protocol);
    assert_eq!(
        versions
            .iter()
            .map(|(_, kind)| kind.as_str())
            .collect::<Vec<_>>(),
        ["checkpoint", "save"]
    );
    let history = LocalHistory::at_root(&protocol.store);
    let lines = protocol.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    let expected = |(id, kind): &(String, String), step: Option<&str>| Line {
        time: time_of(&history, &protocol.dir, id),
        file: MANIFEST_FILENAME.to_string(),
        kind: kind.clone(),
        way: "terminal".to_string(),
        id: id.clone(),
        step: step.map(str::to_string),
    };
    assert_eq!(lines[0], expected(&versions[0], None), "the checkpoint");
    assert_eq!(
        lines[1],
        expected(&versions[1], Some("big_orders")),
        "the save names the step it added"
    );
}

#[cfg(feature = "mcp")]
#[test]
fn an_operation_recorded_through_arc_mcp_writes_lines_naming_mcp() {
    use std::io::Write as _;
    use std::process::Stdio;

    let protocol = Protocol::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(&protocol.dir)
        .env("ARCFORM_HISTORY_DIR", &protocol.store)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc mcp");
    {
        let mut stdin = child.stdin.take().unwrap();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "operation_record",
                "arguments": {
                    "operation": "filter-rows",
                    "on": "orders",
                    "name": "big_orders",
                    "arguments": { "where": "amount > 100" },
                },
            },
        });
        writeln!(stdin, "{request}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let response = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !response.contains("\"isError\":true"),
        "arc mcp failed:\n{response}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let versions = listed(&protocol);
    let lines = protocol.lines();
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.kind.as_str(), l.way.as_str(), l.step.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("checkpoint", "mcp", None),
            ("save", "mcp", Some("big_orders"))
        ]
    );
    assert_eq!(
        lines.iter().map(|l| l.id.clone()).collect::<Vec<_>>(),
        versions.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------- one log per folder

#[test]
fn a_chart_files_versions_go_to_the_spec_folders_one_log_and_a_plain_save_names_no_step() {
    let protocol = Protocol::new();
    let app = protocol.history(HistoryWay::new("app").unwrap());
    fs::create_dir_all(protocol.dir.join("panels")).unwrap();
    let nested = protocol.dir.join("panels/sales.yaml");
    let beside = protocol.dir.join("trend.yaml");

    let chart = app
        .record_save_for_file(&nested, "mark: bar\n")
        .unwrap()
        .expect("recorded");
    let sibling = app
        .record_save_for_file(&beside, "mark: line\n")
        .unwrap()
        .expect("recorded");
    let spec = app
        .record_save(&protocol.dir, MANIFEST)
        .unwrap()
        .expect("recorded");
    let unsaved = app
        .record_unsaved_for_file(&nested, "mark: area\n")
        .unwrap()
        .expect("recorded");
    let checkpoint = app
        .record_checkpoint_for_file(&beside, "mark: point\n")
        .unwrap()
        .expect("recorded");

    assert_eq!(protocol.logs(), [LOG_FILENAME], "one log for the folder");
    let lines = protocol.lines();
    let summary: Vec<_> = lines
        .iter()
        .map(|l| {
            (
                l.file.as_str(),
                l.kind.as_str(),
                l.way.as_str(),
                l.id.as_str(),
                l.step.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("panels/sales.yaml", "save", "app", chart.id.as_str(), None),
            ("trend.yaml", "save", "app", sibling.id.as_str(), None),
            (MANIFEST_FILENAME, "save", "app", spec.id.as_str(), None),
            (
                "panels/sales.yaml",
                "unsaved",
                "app",
                unsaved.id.as_str(),
                None
            ),
            (
                "trend.yaml",
                "checkpoint",
                "app",
                checkpoint.id.as_str(),
                None
            ),
        ]
    );
    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    assert!(
        log.lines().all(|line| line.ends_with(", no step named")),
        "a plain save says no step is named:\n{log}"
    );
}

// A spec not yet written in a folder inside another Protocol's is that
// folder's spec: its line goes to that folder's log, not the outer one's.
#[test]
fn an_unwritten_spec_in_a_folder_inside_a_protocol_writes_to_its_own_folders_log() {
    let protocol = Protocol::new();
    let inner = protocol.dir.join("draft");
    fs::create_dir_all(&inner).unwrap();
    protocol
        .history(HistoryWay::new("app").unwrap())
        .record_unsaved_for_file(&inner.join(MANIFEST_FILENAME), "name: draft\n")
        .unwrap()
        .expect("recorded");

    assert_eq!(protocol.logs(), [format!("draft/{LOG_FILENAME}")]);
    let log = fs::read_to_string(inner.join(LOG_FILENAME)).unwrap();
    let line = Line::read(log.trim_end());
    assert_eq!(
        (line.file.as_str(), line.kind.as_str()),
        (MANIFEST_FILENAME, "unsaved")
    );
}

#[test]
fn a_file_in_no_protocols_folder_is_recorded_and_gets_no_line() {
    let root = tempfile::tempdir().unwrap();
    let loose = root.path().join("loose");
    fs::create_dir_all(&loose).unwrap();
    let history = LocalHistory::at_root(root.path().join("history"));
    let entry = history
        .record_save_for_file(&loose.join("notes.yaml"), "a: 1\n")
        .unwrap()
        .expect("recorded");
    assert_eq!(entry.kind, HistoryKind::Save);
    assert!(!loose.join(LOG_FILENAME).exists());
    assert!(!root.path().join(LOG_FILENAME).exists());
}

// ------------------------------------------------------------- no contents

#[test]
fn no_line_holds_contents_and_a_line_is_as_long_for_a_large_file_as_a_small_one() {
    let protocol = Protocol::new();
    let app = protocol.history(HistoryWay::new("app").unwrap());
    let chart = protocol.dir.join("chart.yaml");
    let statement = "title: the quarterly marker q7z-4242 nobody else wrote\n";
    let small = format!("{statement}mark: bar\n");
    let large = format!(
        "{small}{}",
        "# padding to make the file large\n".repeat(4000)
    );

    app.record_save_for_file(&chart, &small).unwrap().unwrap();
    app.record_save_for_file(&chart, &large).unwrap().unwrap();

    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    assert!(!log.contains("q7z-4242"), "a line holds the text:\n{log}");
    assert!(!log.contains("padding"), "a line holds the text:\n{log}");
    let lengths: Vec<usize> = log.lines().map(str::len).collect();
    assert_eq!(lengths.len(), 2, "{log}");
    assert!(large.len() > 100 * small.len());
    assert_eq!(
        lengths[0], lengths[1],
        "the line grew with the file:\n{log}"
    );
}

#[test]
fn an_edit_writes_the_line_of_its_checkpoint_once_the_edit_lands_and_then_its_save() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let rename = SpecEdit::Replace {
        path: vec!["name".into()],
        value: "renamed".to_string(),
    };
    edit_spec_with_history(&protocol.dir, &[rename], &history).unwrap();

    let ids: Vec<String> = history
        .entries(&protocol.dir)
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    let lines = protocol.lines();
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.kind.as_str(), l.id.as_str(), l.step.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("checkpoint", ids[0].as_str(), None),
            ("save", ids[1].as_str(), None)
        ]
    );
}

#[test]
fn a_restore_writes_the_line_of_the_checkpoint_it_takes() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let earlier = "name: shop\nengine: duckdb\nsteps: []\n";
    let saved = history
        .record_save(&protocol.dir, earlier)
        .unwrap()
        .expect("recorded");

    history.restore(&protocol.dir, &saved.id).unwrap();

    let entries = history.entries(&protocol.dir).unwrap();
    let lines = protocol.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        (lines[1].kind.as_str(), lines[1].id.as_str()),
        ("checkpoint", entries[1].id.as_str()),
        "the restore's checkpoint of the text it replaced has its line"
    );
}

// --------------------------------------------------------------- one rule

#[test]
fn a_path_or_step_that_is_not_plain_is_quoted_and_reads_back_by_the_readme_rule() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    fs::create_dir_all(protocol.dir.join("panels")).unwrap();
    let spaced = protocol.dir.join("panels/q3 sales, by region.yaml");
    let plain = protocol.dir.join("panels/q3_sales-v2.yaml");
    history
        .record_save_for_file(&spaced, "mark: bar\n")
        .unwrap()
        .unwrap();
    history
        .record_save_for_file(&plain, "mark: bar\n")
        .unwrap()
        .unwrap();
    let step = RecordedStep {
        name: "café orders".to_string(),
        sql: "SELECT 1 AS n\n".to_string(),
        provenance: "a test".to_string(),
        description: None,
    };
    record_step_with_history(&protocol.dir, &step, &history).unwrap();

    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    let raw: Vec<&str> = log.lines().collect();
    assert!(
        raw[0].contains(" \"panels/q3 sales, by region.yaml\" save by app,"),
        "{}",
        raw[0]
    );
    assert!(
        raw[1].contains(" panels/q3_sales-v2.yaml save by app,"),
        "{}",
        raw[1]
    );
    assert!(
        raw[3].ends_with(", step \"café orders\" added"),
        "{}",
        raw[3]
    );

    let lines = protocol.lines();
    assert_eq!(lines[0].file, "panels/q3 sales, by region.yaml");
    assert_eq!(lines[1].file, "panels/q3_sales-v2.yaml");
    assert_eq!(
        (lines[2].kind.as_str(), lines[2].step.as_deref()),
        ("checkpoint", None)
    );
    assert_eq!(
        (lines[3].kind.as_str(), lines[3].step.as_deref()),
        ("save", Some("café orders"))
    );
}

#[test]
fn a_version_recorded_by_a_handle_with_no_way_says_so_and_reads_back() {
    let protocol = Protocol::new();
    LocalHistory::at_root(&protocol.store)
        .record_save(&protocol.dir, MANIFEST)
        .unwrap()
        .unwrap();
    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    assert!(
        log.contains(" save by a way not recorded, version "),
        "{log}"
    );
    assert_eq!(protocol.lines()[0].way, "a way not recorded");
}

#[test]
fn a_log_whose_last_line_lacks_its_newline_gets_one_before_the_next_line() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let kept = "a line a person wrote by hand, with no newline";
    fs::write(protocol.dir.join(LOG_FILENAME), kept).unwrap();

    history
        .record_save(&protocol.dir, MANIFEST)
        .unwrap()
        .unwrap();

    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log:?}");
    assert_eq!(lines[0], kept);
    assert_eq!(Line::read(lines[1]).kind, "save");
    assert!(log.ends_with('\n'));
}

#[cfg(unix)]
#[test]
fn a_folder_arc_cannot_write_to_costs_the_line_and_not_the_version() {
    use std::os::unix::fs::PermissionsExt;

    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let mode = |m: u32| fs::set_permissions(&protocol.dir, fs::Permissions::from_mode(m));
    mode(0o555).unwrap();
    let probe = fs::write(protocol.dir.join("probe"), b"");
    let recorded = history.record_save(&protocol.dir, "name: shop\n");
    let entries = history.entries(&protocol.dir);
    mode(0o755).unwrap();

    assert!(
        probe.is_err(),
        "the folder took a write at mode 0555, so this run cannot show a line arc \
         cannot write; run the test as a user that file modes bind"
    );
    let entry = recorded
        .expect("a line arc cannot write does not refuse the version")
        .expect("recorded");
    assert_eq!(entries.unwrap(), [entry]);
    assert!(!protocol.dir.join(LOG_FILENAME).exists());
}

// --------------------------------------------------------------------- git

/// `git` in `dir` with the developer's own configuration out of reach, so a global
/// ignore file cannot decide what is staged.
fn git(dir: &Path, args: &[&str]) -> String {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new("git")
        .current_dir(dir)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `arc` in `cwd` with `store` as its history and `path` as its `PATH`.
fn arc_in(cwd: &Path, store: &Path, path: Option<&Path>, args: &[&str]) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arc"));
    cmd.current_dir(cwd)
        .env("ARCFORM_HISTORY_DIR", store)
        .env("ARCFORM_DB_DIR", store.join("db"))
        .args(args);
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    let out = cmd.output().expect("spawn arc");
    assert!(
        out.status.success(),
        "arc {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const MODEL_STEP: &str = "\n  - name: generate\n    command: \"mkdir -p models && echo 'SELECT 1 AS n' > models/gen.sql\"";

#[test]
fn git_add_all_stages_the_log_beside_the_spec_and_nothing_a_run_records() {
    let base = tempfile::tempdir().unwrap();
    let store = base.path().join("history");
    arc_in(base.path(), &store, None, &["init", "fieldbook"]);
    let proto = base.path().join("fieldbook");
    assert!(
        !proto.join(LOG_FILENAME).exists(),
        "init records no version"
    );

    let edit = [
        "edit-protocol",
        "--dir",
        "fieldbook",
        "replace",
        "steps",
        MODEL_STEP,
    ];
    arc_in(base.path(), &store, None, &edit);
    arc_in(&proto, &store, None, &["run"]);
    assert!(
        fs::read_dir(proto.join("build/.arcform/runs")).is_ok_and(|mut r| r.next().is_some()),
        "the run recorded something for the staging below to leave out"
    );

    git(&proto, &["init", "-q"]);
    git(&proto, &["add", "--all"]);
    let staged = git(&proto, &["ls-files", "--cached"]);
    assert_eq!(
        staged.lines().collect::<Vec<_>>(),
        [
            ".gitignore",
            LOG_FILENAME,
            MANIFEST_FILENAME,
            "models/gen.sql"
        ],
        "the log is staged beside the spec, and no run record is"
    );
}

#[test]
fn a_folder_that_is_no_repository_gets_its_log_with_no_git_to_run() {
    let base = tempfile::tempdir().unwrap();
    let store = base.path().join("history");
    let empty_path = base.path().join("no-bin");
    fs::create_dir_all(&empty_path).unwrap();
    arc_in(
        base.path(),
        &store,
        Some(&empty_path),
        &["create-protocol", "loose"],
    );
    let proto = base.path().join("loose");
    let edit = [
        "edit-protocol",
        "--dir",
        "loose",
        "replace",
        "steps",
        MODEL_STEP,
    ];
    arc_in(base.path(), &store, Some(&empty_path), &edit);

    assert!(!proto.join(".git").exists() && !base.path().join(".git").exists());
    let log = fs::read_to_string(proto.join(LOG_FILENAME)).unwrap();
    let kinds: Vec<String> = log.lines().map(|l| Line::read(l).kind).collect();
    // The edit's checkpoint is the state `create-protocol` saved, which the store
    // already holds, so it is not recorded again and gets no line.
    assert_eq!(kinds, ["save", "save"], "{log}");
}

// ---------------------------------------------------------------- the list

#[test]
fn history_list_ends_saying_where_the_folders_log_is_for_the_spec_and_a_chart_file() {
    let protocol = Protocol::new();
    let canonical_log = protocol.dir.canonicalize().unwrap().join(LOG_FILENAME);
    let place = |log: &Path, written: &str| {
        format!(
            "log: one line per version, naming its file and no contents, at {}{written} — \
             inside the protocol, so it goes with the folder and `git add` stages it",
            log.display()
        )
    };

    let before = protocol.arc_ok(&["history", "list"]);
    assert_eq!(
        before.lines().last(),
        Some(place(&canonical_log, " (no line written yet)").as_str()),
        "{before}"
    );

    fs::create_dir_all(protocol.dir.join("panels")).unwrap();
    record_big_orders(&protocol);
    for args in [
        &["history", "list"][..],
        &["history", "list", "--file", "panels/sales.yaml"][..],
    ] {
        let listed = protocol.arc_ok(args);
        let lines: Vec<&str> = listed.lines().collect();
        assert_eq!(
            lines.last(),
            Some(&place(&canonical_log, "").as_str()),
            "{args:?}:\n{listed}"
        );
        let policy = lines[lines.len() - 2];
        assert!(
            policy.starts_with("policy: keeps the last 50 snapshots per file")
                && policy.contains("the snapshots are stored outside the protocol at ")
                && policy.ends_with(" and never promoted to git"),
            "{args:?}: {policy}"
        );
    }
}

#[test]
fn history_list_says_a_file_in_no_protocols_folder_gets_no_line() {
    let root = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(root.path())
        .env("ARCFORM_HISTORY_DIR", root.path().join("history"))
        .args(["history", "list", "--file", "notes.yaml"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout.lines().last().unwrap_or_default();
    assert!(
        last.starts_with("log: none — no folder at or above ")
            && last.ends_with(" holds arcform.yaml, so no line names its versions"),
        "{stdout}"
    );
}
