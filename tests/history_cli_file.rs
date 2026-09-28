//! `arc history` given one file, through the real `arc` binary: a chart file
//! beside a Protocol's spec has versions the command line can list, print and
//! restore, and a verb given no file does what it did before files had a
//! history of their own.
//!
//!   1. **list** — `arc history list --file panels/a.yaml` prints that file's
//!      entries and none of the spec's or another file's;
//!   2. **show** — `arc history show <id> --file panels/a.yaml` prints the
//!      entry's exact text;
//!   3. **restore** — `arc history restore <id> --file panels/a.yaml` writes
//!      that file alone, leaves `arcform.yaml` byte-identical and exits zero;
//!   4. **no file** — `list`, `show` and `restore` without `--file` print and
//!      write what they did before `--file` existed;
//!   5. **help** — `arc history --help` and each verb's help name `--file`.
//!
//! The history is recorded through the library, which is the write path a
//! tool saving a chart file takes, and read back through the binary alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arc::spec::{HistoryKind, LocalHistory, MANIFEST_FILENAME};

const SPEC_V1: &str = "name: fixture\nsteps: []\n";
const SPEC_V2: &str = "name: fixture-renamed\nsteps: []\n";
const SPEC_NOW: &str = "name: fixture-now\nsteps: []\n";
const A_V1: &str = "# chart a\nmark: lineY\n";
const A_V2: &str = "# chart a\nmark: areaY\n";
const A_NOW: &str = "# chart a\nmark: dot\n";
const B_V1: &str = "# chart b\nmark: barY\n";
const B_NOW: &str = "# chart b\nmark: barX\n";

/// A Protocol directory with a spec and two chart files under `panels/`, a
/// history store beside it, and two recorded versions each of the spec and of
/// `panels/a.yaml`, and one of `panels/b.yaml`. Every file on disk holds text
/// no entry recorded, so a restore is visible as a change.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    dir: PathBuf,
    a: PathBuf,
    b: PathBuf,
    history: LocalHistory,
    spec_ids: [String; 2],
    a_ids: [String; 2],
    b_id: String,
}

fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let dir = root.join("protocol");
    fs::create_dir_all(dir.join("panels")).unwrap();
    let a = dir.join("panels").join("a.yaml");
    let b = dir.join("panels").join("b.yaml");
    let history = LocalHistory::at_root(root.join("history"));

    // A save then a checkpoint: a checkpoint is never merged into the save
    // before it, so each file keeps both entries however fast this runs.
    let id = |e: Option<arc::spec::HistoryEntry>| e.expect("recorded").id;
    let spec_ids = [
        id(history.record_save(&dir, SPEC_V1).unwrap()),
        id(history.record_checkpoint(&dir, SPEC_V2).unwrap()),
    ];
    let a_ids = [
        id(history.record_save_for_file(&a, A_V1).unwrap()),
        id(history.record_checkpoint_for_file(&a, A_V2).unwrap()),
    ];
    let b_id = id(history.record_save_for_file(&b, B_V1).unwrap());

    fs::write(dir.join(MANIFEST_FILENAME), SPEC_NOW).unwrap();
    fs::write(&a, A_NOW).unwrap();
    fs::write(&b, B_NOW).unwrap();
    Fixture {
        _tmp: tmp,
        root,
        dir,
        a,
        b,
        history,
        spec_ids,
        a_ids,
        b_id,
    }
}

impl Fixture {
    /// Run the real `arc` binary with `args` in `cwd`, against this fixture's
    /// history store and never the developer's `~/.arcform`.
    fn arc_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(cwd)
            .env("ARCFORM_HISTORY_DIR", self.history.root())
            .env_remove("ARCFORM_VERBOSE")
            .args(args)
            .output()
            .expect("spawn arc")
    }

    /// Run `arc` in the Protocol's directory, demand exit code zero, and
    /// return stdout.
    fn arc_ok(&self, args: &[&str]) -> String {
        let out = self.arc_in(&self.dir, args);
        assert_eq!(
            out.status.code(),
            Some(0),
            "arc {args:?} did not exit zero:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Every recorded id the fixture knows, for telling which of them a
    /// listing printed.
    fn all_ids(&self) -> Vec<&str> {
        self.spec_ids
            .iter()
            .chain(&self.a_ids)
            .chain([&self.b_id])
            .map(String::as_str)
            .collect()
    }
}

/// The ids a listing printed, in order: a line is an entry line when its
/// first field is an id the fixture recorded.
fn listed_ids<'a>(stdout: &str, known: &[&'a str]) -> Vec<&'a str> {
    stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|first| known.iter().find(|id| **id == first).copied())
        .collect()
}

/// One entry line exactly as `arc history list` has printed it since the
/// command existed.
fn entry_line(history: &LocalHistory, dir: &Path, id: &str) -> String {
    let entry = history
        .entries(dir)
        .unwrap()
        .into_iter()
        .find(|e| e.id == id)
        .expect("the id is the spec's");
    let kind = match entry.kind {
        HistoryKind::Save => "save",
        HistoryKind::Checkpoint => "checkpoint",
    };
    format!(
        "{}  {:<10}  {}  {:>7} bytes",
        entry.id,
        kind,
        humantime::format_rfc3339_seconds(entry.at),
        entry.bytes
    )
}

// List: the file's two entries, oldest first, and no entry of the spec's or of
// the other chart's; the restore hint names the file it was listed for.
#[test]
fn list_given_a_file_prints_that_files_entries_and_none_of_the_protocols() {
    let fx = setup();
    let stdout = fx.arc_ok(&["history", "list", "--file", "panels/a.yaml"]);

    assert_eq!(
        listed_ids(&stdout, &fx.all_ids()),
        fx.a_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        "the listing is panels/a.yaml's two entries and nothing else:\n{stdout}"
    );
    for id in fx.spec_ids.iter().chain([&fx.b_id]) {
        assert!(!stdout.contains(id.as_str()), "{id} is not a's:\n{stdout}");
    }
    assert!(
        stdout.contains(
            "(2 recorded state(s), newest last — \
             `arc history restore <id> --file panels/a.yaml` rolls back)"
        ),
        "the restore hint names the file:\n{stdout}"
    );
}

// A file nothing has recorded says so, naming the file rather than the spec.
#[test]
fn list_given_a_file_with_no_history_names_the_file() {
    let fx = setup();
    let stdout = fx.arc_ok(&["history", "list", "--file", "panels/c.yaml"]);
    assert_eq!(
        stdout.lines().next().unwrap(),
        "no local history for ./panels/c.yaml yet — \
         entries are recorded as the file is saved or machine-edited",
    );
}

// A relative `--file` is read from `--dir`, not from the working directory.
#[test]
fn a_relative_file_is_read_from_the_protocol_directory() {
    let fx = setup();
    let out = fx.arc_in(
        &fx.root,
        &[
            "history",
            "list",
            "--dir",
            "protocol",
            "--file",
            "panels/a.yaml",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        listed_ids(&stdout, &fx.all_ids()),
        fx.a_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        "{stdout}"
    );
}

// Show: each entry's exact text, and nothing else on stdout.
#[test]
fn show_given_a_file_prints_that_entrys_text() {
    let fx = setup();
    for (id, text) in fx.a_ids.iter().zip([A_V1, A_V2]) {
        let stdout = fx.arc_ok(&["history", "show", id, "--file", "panels/a.yaml"]);
        assert_eq!(stdout, text, "entry {id}");
    }
}

// Restore: the entry's text lands in panels/a.yaml, the spec and the other chart
// are byte-identical, the command exits zero, and the text it replaced is
// checkpointed in a's history rather than the spec's.
#[test]
fn restore_given_a_file_writes_that_file_and_leaves_the_spec_alone() {
    let fx = setup();
    let spec_before = fs::read(fx.dir.join(MANIFEST_FILENAME)).unwrap();
    let b_before = fs::read(&fx.b).unwrap();

    let stdout = fx.arc_ok(&[
        "history",
        "restore",
        &fx.a_ids[0],
        "--file",
        "panels/a.yaml",
    ]);

    assert_eq!(fs::read_to_string(&fx.a).unwrap(), A_V1);
    assert_eq!(
        fs::read(fx.dir.join(MANIFEST_FILENAME)).unwrap(),
        spec_before
    );
    assert_eq!(fs::read(&fx.b).unwrap(), b_before);
    // A chart file is not a spec, so no "does not load as a spec" note.
    assert_eq!(
        stdout,
        format!(
            "restored ./panels/a.yaml to {} — the replaced state was checkpointed first\n",
            fx.a_ids[0]
        )
    );

    let a_entries = fx.history.entries_for_file(&fx.a).unwrap();
    assert_eq!(a_entries.len(), 3, "the replaced text is checkpointed");
    let newest = &a_entries.last().unwrap().id;
    assert_eq!(fx.history.read_for_file(&fx.a, newest).unwrap(), A_NOW);
    assert_eq!(fx.history.entries(&fx.dir).unwrap().len(), 2);
}

// No file: the listing is the spec's entries in the format the
// command has always printed, show prints the spec's text, and restore writes
// the spec, leaves the chart files byte-identical and reports as before.
#[test]
fn with_no_file_the_verbs_print_and_write_what_they_did() {
    let fx = setup();

    let stdout = fx.arc_ok(&["history", "list"]);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{stdout}");
    assert_eq!(lines[0], entry_line(&fx.history, &fx.dir, &fx.spec_ids[0]));
    assert_eq!(lines[1], entry_line(&fx.history, &fx.dir, &fx.spec_ids[1]));
    assert_eq!(
        lines[2],
        "(2 recorded state(s), newest last — `arc history restore <id>` rolls back)"
    );
    assert!(lines[3].starts_with("policy: "), "{stdout}");

    assert_eq!(fx.arc_ok(&["history", "show", &fx.spec_ids[0]]), SPEC_V1);

    let a_before = fs::read(&fx.a).unwrap();
    let b_before = fs::read(&fx.b).unwrap();
    let stdout = fx.arc_ok(&["history", "restore", &fx.spec_ids[0]]);
    assert_eq!(
        stdout,
        format!(
            "restored ./arcform.yaml to {} — the replaced state was checkpointed first\n",
            fx.spec_ids[0]
        )
    );
    assert_eq!(
        fs::read_to_string(fx.dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC_V1
    );
    assert_eq!(fs::read(&fx.a).unwrap(), a_before);
    assert_eq!(fs::read(&fx.b).unwrap(), b_before);
    assert_eq!(fx.history.entries_for_file(&fx.a).unwrap().len(), 2);
}

// No file, no history: a Protocol with no history names the spec, as before.
#[test]
fn with_no_file_and_no_history_the_listing_names_the_spec() {
    let fx = setup();
    let empty = fx.root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join(MANIFEST_FILENAME), SPEC_V1).unwrap();
    let out = fx.arc_in(&empty, &["history", "list"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
        "no local history for ./arcform.yaml yet — \
         entries are recorded as the spec is saved or machine-edited",
    );
}

// The note: a restored spec that does not load is still said out loud,
// with no file given and with the spec given as the file; a chart is not
// asked to load as a spec (the restore test above pins that side).
#[test]
fn a_restored_spec_that_does_not_load_is_noted() {
    let fx = setup();
    let broken = fx
        .history
        .record_checkpoint(&fx.dir, "steps: []\n")
        .unwrap()
        .expect("recorded")
        .id;
    let note = "note: the restored state does not load as a spec";

    let stdout = fx.arc_ok(&["history", "restore", &broken]);
    assert!(stdout.contains(note), "{stdout}");

    fs::write(fx.dir.join(MANIFEST_FILENAME), SPEC_NOW).unwrap();
    let stdout = fx.arc_ok(&["history", "restore", &broken, "--file", "arcform.yaml"]);
    assert!(stdout.contains(note), "{stdout}");
    assert_eq!(
        fs::read_to_string(fx.dir.join(MANIFEST_FILENAME)).unwrap(),
        "steps: []\n"
    );
}

// Help: `arc history --help` names the file argument, and each verb's help
// lists it as an option it takes.
#[test]
fn help_names_the_file_argument() {
    let fx = setup();
    let stdout = fx.arc_ok(&["history", "--help"]);
    assert!(stdout.contains("--file <FILE>"), "{stdout}");
    for verb in ["list", "show", "restore"] {
        let stdout = fx.arc_ok(&["history", verb, "--help"]);
        assert!(
            stdout
                .lines()
                .any(|l| l.trim_start().starts_with("--file <FILE>")),
            "`arc history {verb} --help` lists --file:\n{stdout}"
        );
    }
}
