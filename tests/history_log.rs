//! The log in a Protocol's folder: one line per version local history records,
//! naming the file, the time, the kind, the way, the version's id and the step,
//! asked of the real `arc` binary and of the library calls the app records with.
//!
//!   1. **a recorded step** — `arc operation record` writes a checkpoint line and
//!      a save line naming the step it added, with the times and ids `arc history`
//!      holds; through `arc mcp` the lines name `mcp`;
//!   2. **one log per folder** — a chart file's versions, under `panels/` or
//!      beside the spec, land in the same log as the spec's, each line naming its
//!      file, and a save with no earlier state names no step;
//!   3. **no contents** — no line holds a byte of a file, and a line is as long
//!      for a large file as for a small one;
//!   4. **one JSON object** — every line reads back through a JSON reader with
//!      the keys the README names and no others, a path or step that is not
//!      plain reads back as written, and a folder arc cannot write to costs the
//!      line and not the version;
//!   5. **git** — `git add --all` in a fresh Protocol stages the log beside
//!      `arcform.yaml` and nothing a run records, and a folder that is no
//!      repository gets its log with no `git` to run;
//!   6. **the list** — `arc history list` ends by saying where the log is, for
//!      the spec and for a file under `panels/`;
//!   7. **a plain save's step** — a save of the spec names each step it added,
//!      removed or changed, found by comparing the saved text with the state the
//!      store held before it, and names the file alone when no step changed, when
//!      the file has no steps or when the store holds no state to compare with.
//!
//! The README is read for the keys rather than the keys being copied here, so
//! a README that drifts from what arc writes fails these tests.

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

    /// The log's lines, each read by a JSON reader.
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

/// One line of the log, read back into its keys.
#[derive(Debug, PartialEq)]
struct Line {
    at: String,
    file: String,
    kind: String,
    /// `None` for a version recorded by a handle that names no way.
    interface: Option<String>,
    version: String,
    /// The step the line names, or `None` when no verb named one.
    step: Option<String>,
    /// What was done to `step`, present exactly when `step` is.
    change: Option<String>,
    /// The steps and what was done to each, when a save touched more than one;
    /// present only when `step` is not.
    steps: Option<Vec<(String, String)>>,
}

impl Line {
    /// Read `line` as one JSON object, refusing a key the README does not name
    /// and a value that is not a string.
    fn read(line: &str) -> Line {
        let value: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{line:?} is not one JSON value: {e}"));
        let object = value
            .as_object()
            .unwrap_or_else(|| panic!("{line:?} is not a JSON object"));
        let named = readme_keys();
        for key in object.keys() {
            assert!(
                named.contains(key),
                "{line:?} holds the key {key:?}, which the README does not name"
            );
        }
        let text = |key: &str| {
            object.get(key).map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("{key} is not a string in {line:?}"))
                    .to_string()
            })
        };
        let need = |key: &str| text(key).unwrap_or_else(|| panic!("{line:?} has no {key}"));
        let steps = object.get("steps").map(|value| {
            value
                .as_array()
                .unwrap_or_else(|| panic!("steps is not a list in {line:?}"))
                .iter()
                .map(|one| {
                    let one = one
                        .as_object()
                        .unwrap_or_else(|| panic!("a step in {line:?} is not an object"));
                    assert_eq!(one.len(), 2, "a step holds a name and a change: {line:?}");
                    let word = |key: &str| {
                        one.get(key)
                            .and_then(|value| value.as_str())
                            .unwrap_or_else(|| panic!("a step has no text {key} in {line:?}"))
                            .to_string()
                    };
                    (word("step"), word("change"))
                })
                .collect::<Vec<_>>()
        });
        let read = Line {
            at: need("at"),
            file: need("file"),
            kind: need("kind"),
            interface: text("interface"),
            version: need("version"),
            step: text("step"),
            change: text("change"),
            steps,
        };
        assert_eq!(
            read.step.is_some(),
            read.change.is_some(),
            "a step and its change come together: {line:?}"
        );
        assert!(
            read.step.is_none() || read.steps.is_none(),
            "one step is `step` and `change`, several are `steps`: {line:?}"
        );
        if let Some(steps) = &read.steps {
            assert!(steps.len() > 1, "`steps` is for several steps: {line:?}");
        }
        read
    }

    /// Every step the line names with what was done to it, whichever way the
    /// line holds them.
    fn named(&self) -> Vec<(&str, &str)> {
        match (&self.step, &self.change, &self.steps) {
            (Some(step), Some(change), _) => vec![(step.as_str(), change.as_str())],
            (_, _, Some(steps)) => steps
                .iter()
                .map(|(step, change)| (step.as_str(), change.as_str()))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Each line of the log the README's `json` blocks show, as written there.
fn readme_objects() -> Vec<String> {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("read the README");
    let mut shown = Vec::new();
    let mut rest = readme.as_str();
    while let Some((_, after)) = rest.split_once("```json\n{\"at\"") {
        let (block, tail) = after.split_once("\n```").expect("the block closes");
        shown.push(format!("{{\"at\"{block}"));
        rest = tail;
    }
    assert!(
        !shown.is_empty(),
        "the README shows a line of the log in a json block"
    );
    shown
}

/// The keys the README names: those of the lines its `json` blocks show, each
/// of which its list of keys names in backticks.
fn readme_keys() -> Vec<String> {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("read the README");
    let mut keys: Vec<String> = Vec::new();
    for shown in readme_objects() {
        let object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&shown).expect("the README's line is a JSON object");
        for key in object.keys() {
            if !keys.contains(key) {
                keys.push(key.clone());
            }
        }
    }
    for key in &keys {
        assert!(
            readme.contains(&format!("- `{key}`")) || readme.contains(&format!(" and `{key}`:")),
            "the README's list of keys does not name {key:?}"
        );
    }
    keys
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
        at: time_of(&history, &protocol.dir, id),
        file: MANIFEST_FILENAME.to_string(),
        kind: kind.clone(),
        interface: Some("terminal".to_string()),
        version: id.clone(),
        step: step.map(str::to_string),
        change: step.map(|_| "added".to_string()),
        steps: None,
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
            .map(|l| (
                l.kind.as_str(),
                l.interface.as_deref(),
                l.step.as_deref(),
                l.change.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            ("checkpoint", Some("mcp"), None, None),
            ("save", Some("mcp"), Some("big_orders"), Some("added"))
        ]
    );
    assert_eq!(
        lines.iter().map(|l| l.version.clone()).collect::<Vec<_>>(),
        versions.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------- one log per folder

#[test]
fn a_chart_files_versions_go_to_the_spec_folders_one_log_and_a_save_with_no_earlier_state_names_no_step()
 {
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
                l.interface.as_deref(),
                l.version.as_str(),
                l.step.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "panels/sales.yaml",
                "save",
                Some("app"),
                chart.id.as_str(),
                None
            ),
            ("trend.yaml", "save", Some("app"), sibling.id.as_str(), None),
            (
                MANIFEST_FILENAME,
                "save",
                Some("app"),
                spec.id.as_str(),
                None
            ),
            (
                "panels/sales.yaml",
                "unsaved",
                Some("app"),
                unsaved.id.as_str(),
                None
            ),
            (
                "trend.yaml",
                "checkpoint",
                Some("app"),
                checkpoint.id.as_str(),
                None
            ),
        ]
    );
    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    assert!(
        log.lines()
            .all(|line| !line.contains("\"step\"") && !line.contains("\"change\"")),
        "a save with no earlier state names no step:\n{log}"
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
            .map(|l| (l.kind.as_str(), l.version.as_str(), l.step.as_deref()))
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
        (lines[1].kind.as_str(), lines[1].version.as_str()),
        ("checkpoint", entries[1].id.as_str()),
        "the restore's checkpoint of the text it replaced has its line"
    );
}

// ------------------------------------------------------- one JSON object

#[test]
fn every_line_is_one_json_object_with_the_readme_keys_and_odd_names_read_back_as_written() {
    // The README shows lines holding every key between them, and names each of them.
    let mut keys = readme_keys();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "at",
            "change",
            "file",
            "interface",
            "kind",
            "step",
            "steps",
            "version"
        ]
    );
    for shown in readme_objects() {
        Line::read(&shown);
    }

    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    fs::create_dir_all(protocol.dir.join("panels")).unwrap();
    let odd = protocol.dir.join("panels/q3 \"sales\", by region.yaml");
    let plain = protocol.dir.join("panels/q3_sales-v2.yaml");
    history
        .record_save_for_file(&odd, "mark: bar\n")
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

    // One object on each line of its own, nothing around it.
    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    assert_eq!(log.lines().count(), 4, "{log}");
    for raw in log.lines() {
        assert!(raw.starts_with("{\"at\":") && raw.ends_with('}'), "{raw}");
    }

    let lines = protocol.lines();
    assert_eq!(lines[0].file, "panels/q3 \"sales\", by region.yaml");
    assert_eq!(lines[1].file, "panels/q3_sales-v2.yaml");
    assert_eq!(
        (
            lines[2].kind.as_str(),
            lines[2].step.as_deref(),
            lines[2].change.as_deref()
        ),
        ("checkpoint", None, None)
    );
    assert_eq!(
        (
            lines[3].kind.as_str(),
            lines[3].step.as_deref(),
            lines[3].change.as_deref()
        ),
        ("save", Some("café orders"), Some("added"))
    );
}

#[test]
fn a_version_recorded_by_a_handle_with_no_way_has_no_interface_and_reads_back() {
    let protocol = Protocol::new();
    LocalHistory::at_root(&protocol.store)
        .record_save(&protocol.dir, MANIFEST)
        .unwrap()
        .unwrap();
    let log = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    assert!(!log.contains("interface"), "{log}");
    let lines = protocol.lines();
    assert_eq!(
        (lines[0].kind.as_str(), lines[0].interface.as_deref()),
        ("save", None)
    );
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
            ".gitattributes",
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

// ------------------------------------------------------ a plain save's step

/// A spec holding `steps`, each a name and the SQL file it names.
fn spec_with(steps: &[(&str, &str)]) -> String {
    let mut text = String::from("name: shop\nengine: duckdb\ndb: shop.duckdb\n\nsteps:\n");
    for (name, sql) in steps {
        text.push_str(&format!("  - name: {name}\n    sql: {sql}\n"));
    }
    text
}

/// A save of the spec in `protocol` through the app's handle, which names no
/// step of its own: the entry recorded.
fn save_spec(protocol: &Protocol, text: &str) -> arc::spec::HistoryEntry {
    protocol
        .history(HistoryWay::new("app").unwrap())
        .record_save(&protocol.dir, text)
        .unwrap()
        .expect("a save of a new state is recorded")
}

/// The last line of the log, read back.
fn last_line(protocol: &Protocol) -> Line {
    protocol.lines().pop().expect("the log holds a line")
}

#[test]
fn a_save_names_the_step_it_added_changed_or_removed() {
    let protocol = Protocol::new();
    let two = spec_with(&[("load_orders", "models/01_orders.sql"), ("tally", "a.sql")]);
    save_spec(&protocol, &two);
    assert_eq!(
        last_line(&protocol).named(),
        [] as [(&str, &str); 0],
        "a first save has no earlier state to compare with"
    );

    // One step added to a Protocol holding two.
    let three = spec_with(&[
        ("load_orders", "models/01_orders.sql"),
        ("tally", "a.sql"),
        ("big_orders", "b.sql"),
    ]);
    let saved = save_spec(&protocol, &three);
    let line = last_line(&protocol);
    assert_eq!(line.version, saved.id);
    assert_eq!(line.file, MANIFEST_FILENAME);
    assert_eq!(line.interface.as_deref(), Some("app"));
    assert_eq!(
        (line.step.as_deref(), line.change.as_deref(), &line.steps),
        (Some("big_orders"), Some("added"), &None),
        "one step is `step` and `change`: {line:?}"
    );

    // One step's statement changed.
    let changed = spec_with(&[
        ("load_orders", "models/01_orders.sql"),
        ("tally", "a.sql"),
        ("big_orders", "c.sql"),
    ]);
    save_spec(&protocol, &changed);
    let line = last_line(&protocol);
    assert_eq!(line.named(), [("big_orders", "changed")], "{line:?}");

    // That step removed.
    let removed = spec_with(&[("load_orders", "models/01_orders.sql"), ("tally", "a.sql")]);
    save_spec(&protocol, &removed);
    let line = last_line(&protocol);
    assert_eq!(line.named(), [("big_orders", "removed")], "{line:?}");
    assert_eq!(protocol.lines().len(), 4, "one line for each save");
}

#[test]
fn an_unmerged_save_writes_a_line_of_its_own_and_names_the_step_it_added() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let one = spec_with(&[("load_orders", "models/01_orders.sql")]);
    let two = spec_with(&[("load_orders", "models/01_orders.sql"), ("tally", "a.sql")]);
    let first = history
        .record_save_unmerged(&protocol.dir, &one)
        .unwrap()
        .expect("recorded");
    let second = history
        .record_save_unmerged(&protocol.dir, &two)
        .unwrap()
        .expect("recorded");

    let lines = protocol.lines();
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.kind.as_str(), l.version.as_str()))
            .collect::<Vec<_>>(),
        [("save", first.id.as_str()), ("save", second.id.as_str())],
        "each unmerged save has its own line"
    );
    assert_eq!(
        lines[1].named(),
        [("tally", "added")],
        "the line names the step by comparison, as a save's does: {:?}",
        lines[1]
    );
}

#[test]
fn a_save_that_added_one_step_and_changed_another_names_both_on_one_line() {
    let protocol = Protocol::new();
    save_spec(
        &protocol,
        &spec_with(&[("load_orders", "models/01_orders.sql"), ("tally", "a.sql")]),
    );

    save_spec(
        &protocol,
        &spec_with(&[
            ("load_orders", "models/01_orders.sql"),
            ("tally", "changed.sql"),
            ("big_orders", "b.sql"),
        ]),
    );

    let lines = protocol.lines();
    assert_eq!(lines.len(), 2, "one line for the one write: {lines:?}");
    let line = &lines[1];
    assert_eq!(
        (&line.step, &line.change),
        (&None, &None),
        "several steps are `steps`, not `step` and `change`: {line:?}"
    );
    assert_eq!(
        line.named(),
        [("tally", "changed"), ("big_orders", "added")],
        "{line:?}"
    );
    let raw = fs::read_to_string(protocol.dir.join(LOG_FILENAME)).unwrap();
    assert!(
        raw.lines().last().unwrap().contains(
            "\"steps\":[{\"step\":\"tally\",\"change\":\"changed\"},\
             {\"step\":\"big_orders\",\"change\":\"added\"}]"
        ),
        "{raw}"
    );
}

#[test]
fn a_save_of_the_spec_through_its_file_path_names_the_step_as_the_directory_call_does() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let spec = protocol.dir.join(MANIFEST_FILENAME);
    history
        .record_save_for_file(&spec, &spec_with(&[("tally", "a.sql")]))
        .unwrap()
        .expect("recorded");

    let saved = history
        .record_save_for_file(
            &spec,
            &spec_with(&[("tally", "a.sql"), ("big_orders", "b.sql")]),
        )
        .unwrap()
        .expect("recorded");

    let line = last_line(&protocol);
    assert_eq!(line.version, saved.id);
    assert_eq!(line.file, MANIFEST_FILENAME);
    assert_eq!(line.named(), [("big_orders", "added")], "{line:?}");
}

#[test]
fn a_renamed_step_reads_as_one_removed_and_one_added() {
    let protocol = Protocol::new();
    save_spec(&protocol, &spec_with(&[("tally", "a.sql")]));

    save_spec(&protocol, &spec_with(&[("count", "a.sql")]));

    assert_eq!(
        last_line(&protocol).named(),
        [("count", "added"), ("tally", "removed")]
    );
}

#[test]
fn a_save_that_changed_no_step_names_the_file_and_no_step() {
    let protocol = Protocol::new();
    let base = spec_with(&[("load_orders", "models/01_orders.sql"), ("tally", "a.sql")]);
    save_spec(&protocol, &base);

    // A key at the manifest's top.
    let top = base.replace(
        "engine: duckdb",
        "engine: duckdb\nengine_version: \">=1.3\"",
    );
    assert_ne!(top, base);
    let saved = save_spec(&protocol, &top);
    let line = last_line(&protocol);
    assert_eq!(
        (line.file.as_str(), line.version.as_str()),
        (MANIFEST_FILENAME, saved.id.as_str())
    );
    assert_eq!(line.named(), [] as [(&str, &str); 0], "a top key: {line:?}");

    // A comment, above a step and inside one.
    let comment = top.replace(
        "  - name: tally\n    sql: a.sql",
        "  # what the tally counts\n  - name: tally\n    # the file\n    sql: a.sql",
    );
    assert_ne!(comment, top);
    save_spec(&protocol, &comment);
    let line = last_line(&protocol);
    assert_eq!(line.named(), [] as [(&str, &str); 0], "a comment: {line:?}");

    // The order of a step's keys, and of the steps themselves.
    let reordered = "name: shop\nengine: duckdb\nengine_version: \">=1.3\"\ndb: shop.duckdb\n\n\
                     steps:\n  - sql: a.sql\n    name: tally\n  - name: load_orders\n    \
                     sql: models/01_orders.sql\n";
    save_spec(&protocol, reordered);
    let line = last_line(&protocol);
    assert_eq!(line.named(), [] as [(&str, &str); 0], "an order: {line:?}");

    let lines = protocol.lines();
    assert_eq!(lines.len(), 4, "each save has its line: {lines:?}");
    assert!(
        lines.iter().all(|line| line.file == MANIFEST_FILENAME),
        "{lines:?}"
    );
}

#[test]
fn a_save_equal_to_the_last_recorded_state_records_no_version_and_writes_no_line() {
    let protocol = Protocol::new();
    let text = spec_with(&[("tally", "a.sql")]);
    save_spec(&protocol, &text);
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let lines_before = protocol.lines();

    assert!(
        history.record_save(&protocol.dir, &text).unwrap().is_none(),
        "the store records nothing for a state it already holds"
    );

    assert_eq!(protocol.lines(), lines_before, "and the log gains no line");
    assert_eq!(history.entries(&protocol.dir).unwrap().len(), 1);
}

#[test]
fn a_save_of_a_file_that_is_not_the_spec_names_the_file_and_no_step() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    fs::create_dir_all(protocol.dir.join("panels")).unwrap();
    let chart = protocol.dir.join("panels/sales.yaml");
    // The second text holds a `steps:` list that differs from the first's, so
    // only the file's name says it is not a spec.
    let texts = [
        "mark: bar\n",
        "mark: line\n",
        "mark: line\nsteps:\n  - name: a\n",
        "mark: line\nsteps:\n  - name: a\n  - name: b\n",
    ];
    for text in texts {
        history.record_save_for_file(&chart, text).unwrap().unwrap();
    }

    let lines = protocol.lines();
    assert_eq!(lines.len(), texts.len(), "{lines:?}");
    for line in &lines {
        assert_eq!(line.file, "panels/sales.yaml");
        assert_eq!(line.named(), [] as [(&str, &str); 0], "{line:?}");
    }
}

#[test]
fn a_save_whose_last_recorded_state_the_store_does_not_hold_names_the_file_and_is_recorded() {
    // Never on this machine: the Protocol came with its log and the store is empty.
    let protocol = Protocol::new();
    save_spec(&protocol, &spec_with(&[("tally", "a.sql")]));
    assert_eq!(
        last_line(&protocol).named(),
        [] as [(&str, &str); 0],
        "a spec the store has no state of"
    );

    // Pruned: the snapshot the store held is gone.
    let snapshot = |protocol: &Protocol| -> PathBuf {
        let mut found = Vec::new();
        for dir in fs::read_dir(&protocol.store).unwrap().flatten() {
            for file in fs::read_dir(dir.path()).unwrap().flatten() {
                if file.path().extension().is_some_and(|ext| ext == "yaml") {
                    found.push(file.path());
                }
            }
        }
        assert_eq!(found.len(), 1, "one snapshot: {found:?}");
        found.remove(0)
    };
    fs::remove_file(snapshot(&protocol)).unwrap();
    let saved = save_spec(&protocol, &spec_with(&[("tally", "b.sql")]));
    let line = last_line(&protocol);
    assert_eq!(line.version, saved.id);
    assert_eq!(
        line.named(),
        [] as [(&str, &str); 0],
        "a pruned state: {line:?}"
    );
}

#[test]
fn a_spec_that_does_not_read_as_steps_names_the_file_and_no_step_on_either_side() {
    let protocol = Protocol::new();
    let valid = spec_with(&[("tally", "a.sql")]);
    let unreadable = [
        ("text that is not YAML", "steps: [unclosed\n".to_string()),
        ("a document that is not a mapping", "- tally\n".to_string()),
        (
            "steps that are not a list",
            "name: shop\nsteps: 3\n".to_string(),
        ),
        (
            "a step with no name",
            "name: shop\nsteps:\n  - sql: a.sql\n".to_string(),
        ),
        (
            "a step whose name is not text",
            "name: shop\nsteps:\n  - name: 5\n".to_string(),
        ),
        (
            "a step that is not a mapping",
            "name: shop\nsteps:\n  - tally\n".to_string(),
        ),
        (
            "two steps of one name",
            spec_with(&[("tally", "a.sql"), ("tally", "b.sql")]),
        ),
    ];
    save_spec(&protocol, &valid);
    for (why, text) in &unreadable {
        // Out of a state that reads and into one that does not, and back.
        save_spec(&protocol, text);
        assert_eq!(
            last_line(&protocol).named(),
            [] as [(&str, &str); 0],
            "a save of {why} after a spec that reads"
        );
        save_spec(&protocol, &valid);
        assert_eq!(
            last_line(&protocol).named(),
            [] as [(&str, &str); 0],
            "a save of a spec after {why}"
        );
    }
    assert_eq!(protocol.lines().len(), 2 * unreadable.len() + 1);
}

#[test]
fn a_spec_with_no_steps_key_or_an_empty_one_has_no_steps_to_compare() {
    let protocol = Protocol::new();
    let valid = spec_with(&[("tally", "a.sql")]);
    save_spec(&protocol, &valid);

    save_spec(&protocol, "name: shop\nengine: duckdb\n");
    assert_eq!(
        last_line(&protocol).named(),
        [("tally", "removed")],
        "a spec with no `steps` key has none"
    );

    save_spec(&protocol, &valid);
    save_spec(&protocol, "name: shop\nengine: duckdb\nsteps:\n");
    assert_eq!(
        last_line(&protocol).named(),
        [("tally", "removed")],
        "an empty `steps` has none"
    );

    save_spec(&protocol, &valid);
    assert_eq!(last_line(&protocol).named(), [("tally", "added")]);
}

#[test]
fn a_checkpoint_and_an_unsaved_text_of_the_spec_name_no_step() {
    let protocol = Protocol::new();
    let history = protocol.history(HistoryWay::new("app").unwrap());
    let spec = protocol.dir.join(MANIFEST_FILENAME);
    save_spec(&protocol, &spec_with(&[("tally", "a.sql")]));

    // Each text differs from the newest state in a step, and neither is a save.
    history
        .record_checkpoint(
            &protocol.dir,
            &spec_with(&[("tally", "a.sql"), ("big_orders", "b.sql")]),
        )
        .unwrap()
        .expect("recorded");
    history
        .record_unsaved_for_file(&spec, &spec_with(&[("tally", "changed.sql")]))
        .unwrap()
        .expect("recorded");

    let lines = protocol.lines();
    assert_eq!(
        lines
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        ["save", "checkpoint", "unsaved"]
    );
    for line in &lines {
        assert_eq!(line.named(), [] as [(&str, &str); 0], "{line:?}");
    }
}
