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
//!   5. **help** — `arc history --help` and each verb's help name `--file`;
//!   6. **the way** — each version a verb of the command line writes lists as
//!      `terminal`; a version an earlier arc wrote lists as `not recorded` and
//!      is shown and restored by the id that arc printed;
//!   7. **the bound** — past `HISTORY_MAX_ENTRIES` versions written by the
//!      command line, the list and the key directory hold exactly that many;
//!   8. **a text never written to its file** — an unsaved entry lists under a
//!      kind word of its own that fits the kind column, and restoring it
//!      writes its text to the file.
//!
//! The history is recorded through the library, which is the write path a
//! tool saving a chart file takes, and read back through the binary alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arc::spec::{HISTORY_MAX_ENTRIES, HistoryKind, HistoryWay, LocalHistory, MANIFEST_FILENAME};

const SPEC_V1: &str = "name: fixture\nsteps: []\n";
const SPEC_V2: &str = "name: fixture-renamed\nsteps: []\n";
const SPEC_NOW: &str = "name: fixture-now\nsteps: []\n";
const A_V1: &str = "# chart a\nmark: lineY\n";
const A_V2: &str = "# chart a\nmark: areaY\n";
const A_NOW: &str = "# chart a\nmark: dot\n";
const B_V1: &str = "# chart b\nmark: barY\n";
const B_NOW: &str = "# chart b\nmark: barX\n";
const A_UNSAVED: &str = "# chart a\nmark: tickX\n";

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

/// Run the real `arc` binary with `args` in `cwd`, against the history store
/// at `store` and never the developer's `~/.arcform`.
fn arc(store: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(cwd)
        .env("ARCFORM_HISTORY_DIR", store)
        .env_remove("ARCFORM_VERBOSE")
        .args(args)
        .output()
        .expect("spawn arc")
}

/// [`arc`], demanding exit code zero; stdout.
fn arc_ok(store: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = arc(store, cwd, args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "arc {args:?} did not exit zero:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

impl Fixture {
    /// Run the real `arc` binary with `args` in `cwd`, against this fixture's
    /// history store.
    fn arc_in(&self, cwd: &Path, args: &[&str]) -> Output {
        arc(self.history.root(), cwd, args)
    }

    /// Run `arc` in the Protocol's directory, demand exit code zero, and
    /// return stdout.
    fn arc_ok(&self, args: &[&str]) -> String {
        arc_ok(self.history.root(), &self.dir, args)
    }

    /// [`arc_ok`](Self::arc_ok) in `cwd`.
    fn arc_ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        arc_ok(self.history.root(), cwd, args)
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

/// One entry line exactly as `arc history list` prints it: the four columns it
/// has printed since the command existed, then the way.
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
        HistoryKind::Unsaved => "unsaved",
    };
    format!(
        "{}  {:<10}  {}  {:>7} bytes  {}",
        entry.id,
        kind,
        humantime::format_rfc3339_seconds(entry.at),
        entry.bytes,
        entry
            .way
            .as_ref()
            .map_or("not recorded", HistoryWay::as_str)
    )
}

/// Each entry line of a listing as its id, its kind and the words after its
/// size, which are the way.
fn listed(stdout: &str) -> Vec<(String, String, String)> {
    stdout
        .lines()
        .filter_map(|line| {
            let (head, way) = line.split_once(" bytes  ")?;
            let mut words = head.split_whitespace();
            Some((
                words.next()?.to_string(),
                words.next()?.to_string(),
                way.to_string(),
            ))
        })
        .collect()
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
    assert_eq!(lines.len(), 5, "{stdout}");
    assert_eq!(lines[0], entry_line(&fx.history, &fx.dir, &fx.spec_ids[0]));
    assert_eq!(lines[1], entry_line(&fx.history, &fx.dir, &fx.spec_ids[1]));
    assert_eq!(
        lines[2],
        "(2 recorded state(s), newest last — `arc history restore <id>` rolls back)"
    );
    assert!(lines[3].starts_with("policy: "), "{stdout}");
    assert!(lines[4].starts_with("log: "), "{stdout}");

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

// Refusals: an id one history holds is not found in another's, and a path
// that does not exist is refused. Each exits nonzero with the reason on
// stderr, and a refused restore writes nothing.
#[test]
fn what_a_history_does_not_hold_is_refused() {
    let fx = setup();
    let spec_before = fs::read(fx.dir.join(MANIFEST_FILENAME)).unwrap();
    let a_before = fs::read(&fx.a).unwrap();
    let spec_id = fx.spec_ids[0].as_str();
    let a_id = fx.a_ids[0].as_str();

    for args in [
        &["history", "show", spec_id, "--file", "panels/a.yaml"][..],
        &["history", "show", a_id],
        &["history", "restore", spec_id, "--file", "panels/a.yaml"],
        &["history", "restore", a_id],
        &["history", "list", "--file", "missing/a.yaml"],
        &["history", "list", "--dir", "missing"],
    ] {
        let out = fx.arc_in(&fx.dir, args);
        assert_ne!(out.status.code(), Some(0), "arc {args:?} was not refused");
        assert!(!out.stderr.is_empty(), "arc {args:?} gave no reason");
    }
    assert_eq!(
        fs::read(fx.dir.join(MANIFEST_FILENAME)).unwrap(),
        spec_before
    );
    assert_eq!(fs::read(&fx.a).unwrap(), a_before);
}

// A show whose text cannot be written fails rather than exiting as though it
// printed: the pipe's reader is closed before arc writes a byte.
#[test]
fn show_to_a_closed_pipe_is_not_reported_done() {
    let fx = setup();
    for args in [
        &[
            "history",
            "show",
            fx.a_ids[0].as_str(),
            "--file",
            "panels/a.yaml",
        ][..],
        &["history", "show", fx.spec_ids[0].as_str()],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let status = Command::new(env!("CARGO_BIN_EXE_arc"))
            .current_dir(&fx.dir)
            .env("ARCFORM_HISTORY_DIR", fx.history.root())
            .env_remove("ARCFORM_VERBOSE")
            .args(args)
            .stdout(writer)
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn arc");
        assert_ne!(status.code(), Some(0), "arc {args:?} exited zero");
    }
}

// ------------------------------------------------------------------ the way

// Every version a verb of the command line writes names `terminal`:
// `create-protocol`'s first save, `edit-protocol`'s checkpoint and save, and
// the checkpoint `history restore` takes of the state it replaces. The spec is
// changed by hand before each machine write, so each checkpoint is of a state
// no entry holds yet and is not skipped as a duplicate.
#[test]
fn every_version_the_command_line_writes_names_terminal() {
    let fx = setup();
    let proto = fx.root.join("fresh");
    let spec = proto.join(MANIFEST_FILENAME);
    let by_hand = |note: &str| {
        let text = fs::read_to_string(&spec).unwrap();
        fs::write(&spec, format!("{text}# {note}\n")).unwrap();
    };

    fx.arc_ok_in(&fx.root, &["create-protocol", "fresh"]);
    by_hand("changed by hand");
    fx.arc_ok_in(&proto, &["edit-protocol", "replace", "name", "renamed"]);
    let first = listed(&fx.arc_ok_in(&proto, &["history", "list"]))[0]
        .0
        .clone();
    by_hand("changed by hand again");
    fx.arc_ok_in(&proto, &["history", "restore", &first]);

    let entries = listed(&fx.arc_ok_in(&proto, &["history", "list"]));
    let kinds: Vec<&str> = entries.iter().map(|(_, kind, _)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["save", "checkpoint", "save", "checkpoint"],
        "create's save, edit's checkpoint and save, restore's checkpoint: {entries:?}"
    );
    for (id, kind, way) in &entries {
        assert_eq!(way, "terminal", "the {kind} {id}");
    }
}

// A version an earlier arc wrote — `<millis>-<seq>-<kind>.yaml` with nothing
// beside it — lists with `not recorded` where the way would be, and is shown
// and restored by the id that arc printed; an id this arc prints is shown and
// restored alike.
#[test]
fn a_version_an_earlier_arc_wrote_says_not_recorded_and_keeps_its_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("protocol");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(MANIFEST_FILENAME), SPEC_NOW).unwrap();
    let history = LocalHistory::at_root(tmp.path().join("history"))
        .reached_by(HistoryWay::new("app").unwrap());
    history.record_save(&dir, SPEC_V2).unwrap().unwrap();

    // The spec's directory in the store is the one the entry above made, and
    // the earlier arc's two entries go in it by hand, named as it named them.
    let key_dirs: Vec<PathBuf> = fs::read_dir(history.root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(key_dirs.len(), 1, "{key_dirs:?}");
    let older = "name: older\nsteps: []\n";
    fs::write(key_dirs[0].join("1600000000000-000-save.yaml"), SPEC_V1).unwrap();
    fs::write(key_dirs[0].join("1600000000000-001-checkpoint.yaml"), older).unwrap();

    let arc_ok = |args: &[&str]| arc_ok(history.root(), &dir, args);
    let entries = listed(&arc_ok(&["history", "list"]));
    let printed_new = entries[2].0.clone();
    assert_eq!(
        entries,
        [
            (
                "1600000000000-000-save".into(),
                "save".into(),
                "not recorded".into()
            ),
            (
                "1600000000000-001-checkpoint".into(),
                "checkpoint".into(),
                "not recorded".into()
            ),
            (printed_new.clone(), "save".into(), "app".into()),
        ]
    );

    // The earlier arc's ids.
    assert_eq!(
        arc_ok(&["history", "show", "1600000000000-000-save"]),
        SPEC_V1
    );
    assert_eq!(
        arc_ok(&["history", "show", "1600000000000-001-checkpoint"]),
        older
    );
    arc_ok(&["history", "restore", "1600000000000-000-save"]);
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC_V1
    );

    // The ids this arc prints: the caller's save, and the checkpoint the
    // restore above took, which the command line wrote.
    let entries = listed(&arc_ok(&["history", "list"]));
    assert_eq!(entries.len(), 4, "{entries:?}");
    let (printed_checkpoint, kind, way) = entries[3].clone();
    assert_eq!((kind.as_str(), way.as_str()), ("checkpoint", "terminal"));
    assert_eq!(arc_ok(&["history", "show", &printed_new]), SPEC_V2);
    arc_ok(&["history", "restore", &printed_new]);
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC_V2
    );
    assert_eq!(arc_ok(&["history", "show", &printed_checkpoint]), SPEC_NOW);
    arc_ok(&["history", "restore", &printed_checkpoint]);
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC_NOW
    );
}

// The bound, through the command line: every `edit-protocol` writes its
// versions as `terminal`, and past `HISTORY_MAX_ENTRIES` of them `arc history
// list` prints exactly that many and the store's key directory holds exactly
// that many entry files, the first version `create-protocol` saved gone from
// both. The spec is changed by hand before each edit so that edit's checkpoint
// is of a state no entry holds and is recorded rather than skipped as a
// duplicate, and the save after it follows a checkpoint and so does not merge:
// each edit writes two versions, and the loop writes more than the bound.
#[test]
fn the_bound_holds_for_the_versions_the_command_line_writes() {
    const EDITS: usize = HISTORY_MAX_ENTRIES / 2 + 5;
    const {
        assert!(
            1 + 2 * EDITS > HISTORY_MAX_ENTRIES,
            "the loop below must write more versions than the bound keeps"
        )
    };

    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("history");
    let proto = tmp.path().join("fresh");
    let spec = proto.join(MANIFEST_FILENAME);

    arc_ok(&store, tmp.path(), &["create-protocol", "fresh"]);
    let first = listed(&arc_ok(&store, &proto, &["history", "list"]));
    let [(first_id, _, _)] = first.as_slice() else {
        panic!("create-protocol saves one version: {first:?}");
    };
    for n in 0..EDITS {
        let text = fs::read_to_string(&spec).unwrap();
        fs::write(&spec, format!("{text}# changed by hand {n}\n")).unwrap();
        arc_ok(
            &store,
            &proto,
            &["edit-protocol", "replace", "name", &format!("renamed{n}")],
        );
    }

    let entries = listed(&arc_ok(&store, &proto, &["history", "list"]));
    assert_eq!(entries.len(), HISTORY_MAX_ENTRIES, "{entries:?}");
    assert!(
        entries.iter().all(|(_, _, way)| way == "terminal"),
        "{entries:?}"
    );
    assert!(
        entries.iter().all(|(id, _, _)| id != first_id),
        "the oldest version was pruned: {entries:?}"
    );

    let key_dirs: Vec<PathBuf> = fs::read_dir(&store)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [key_dir] = key_dirs.as_slice() else {
        panic!("one key directory, found {key_dirs:?}");
    };
    let mut on_disk: Vec<String> = fs::read_dir(key_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".yaml"))
        .collect();
    on_disk.sort();
    let mut listed_files: Vec<String> = entries
        .iter()
        .map(|(id, _, _)| format!("{id}.terminal.yaml"))
        .collect();
    listed_files.sort();
    assert_eq!(
        on_disk, listed_files,
        "the key directory holds one file for each version listed and no other"
    );
}

// An unsaved text: `arc history list --file` prints it under a kind word that
// is neither of the other two, inside the ten-wide kind column every entry
// line shares, and `arc history restore` writes its text to the file alone.
#[test]
fn an_unsaved_text_lists_under_a_word_of_its_own_and_restores_to_its_file() {
    let fx = setup();
    let unsaved = fx
        .history
        .record_unsaved_for_file(&fx.a, A_UNSAVED)
        .unwrap()
        .expect("recorded");
    assert_eq!(fs::read_to_string(&fx.a).unwrap(), A_NOW, "recording wrote");

    let stdout = fx.arc_ok(&["history", "list", "--file", "panels/a.yaml"]);
    let kinds: Vec<(String, String)> = listed(&stdout)
        .into_iter()
        .map(|(id, kind, _)| (id, kind))
        .collect();
    assert_eq!(
        kinds.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        [
            fx.a_ids[0].as_str(),
            fx.a_ids[1].as_str(),
            unsaved.id.as_str()
        ],
        "{stdout}"
    );
    let word = kinds[2].1.as_str();
    assert!(
        word != "save" && word != "checkpoint" && word.len() <= 10,
        "the unsaved entry's kind word is `{word}`:\n{stdout}"
    );
    assert_eq!(
        kinds.iter().map(|(_, k)| k.as_str()).collect::<Vec<_>>(),
        ["save", "checkpoint", word],
        "{stdout}"
    );

    // Each entry line is its id, two spaces, the kind padded to ten, two
    // spaces and the time: a kind word wider than the column, or of more than
    // one word, moves the time off its column.
    for line in stdout.lines().filter(|l| l.contains(" bytes  ")) {
        let (_, rest) = line.split_once("  ").expect("an id then two spaces");
        let (column, after) = rest.split_at(10);
        assert!(
            !column.trim_end().contains(' ')
                && after.starts_with("  ")
                && after[2..].starts_with(|c: char| c.is_ascii_digit()),
            "the kind column does not stand in `{line}`"
        );
    }

    fx.arc_ok(&["history", "restore", &unsaved.id, "--file", "panels/a.yaml"]);
    assert_eq!(fs::read_to_string(&fx.a).unwrap(), A_UNSAVED);
    assert_eq!(fs::read_to_string(&fx.b).unwrap(), B_NOW);
    assert_eq!(
        fs::read_to_string(fx.dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC_NOW
    );
}
