//! Local history kept per file, exercised through the published surface the
//! way a tool that saves a chart file beside a Protocol's spec would use it:
//!
//!   1. **one file, one list** — two chart files in one protocol directory
//!      list apart, and neither lists the spec's entries nor the spec theirs;
//!   2. **a restore writes one file** — restoring a chart writes that chart,
//!      leaves the spec and the other chart byte-identical, and checkpoints
//!      the text it replaced in that chart's own history;
//!   3. **the spec's key is unchanged** — a history the directory calls
//!      recorded, laid out as the store laid it out before the file calls
//!      existed, is read by the file calls given the directory's
//!      `arcform.yaml`;
//!   4. **the bound is per file** — one file reaching the bound prunes its own
//!      oldest entry and none of another file's, whether or not the entries'
//!      file names carry a way;
//!   5. **a text never written to its file** — recorded as an unsaved entry,
//!      it lists as a kind of its own and reads back byte for byte, leaves the
//!      file as it was, never merges with a save either side of it, counts
//!      toward the bound, and sits beside a file's saves and checkpoints
//!      without changing how they list or restore.
//!
//! Nothing here needs the `arc` binary, so the file runs with the `cli`
//! feature off: `cargo test --no-default-features --test history_per_file`.

use std::fs;
use std::path::{Path, PathBuf};

use arc::spec::{
    Error, HISTORY_MAX_ENTRIES, HISTORY_MERGE_WINDOW, HistoryEntry, HistoryKind, HistoryWay,
    LocalHistory, MANIFEST_FILENAME, Result,
};
use sha2::{Digest, Sha256};

const SPEC: &str = "name: fixture\nsteps: []\n";
const CHART_A: &str = "# chart a\nmark: lineY\n";
const CHART_B: &str = "# chart b\nmark: barY\n";

/// A protocol directory holding the spec and two chart files under
/// `panels/`, and a history store beside it — never inside it.
struct Fixture {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    a: PathBuf,
    b: PathBuf,
    history: LocalHistory,
}

fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("protocol");
    fs::create_dir_all(dir.join("panels")).unwrap();
    fs::write(dir.join(MANIFEST_FILENAME), SPEC).unwrap();
    let a = dir.join("panels").join("a.yaml");
    let b = dir.join("panels").join("b.yaml");
    fs::write(&a, CHART_A).unwrap();
    fs::write(&b, CHART_B).unwrap();
    let history = LocalHistory::at_root(tmp.path().join("history"));
    Fixture {
        _tmp: tmp,
        dir,
        a,
        b,
        history,
    }
}

/// The text of every entry `file` lists, oldest first.
fn texts_for_file(history: &LocalHistory, file: &Path) -> Vec<String> {
    history
        .entries_for_file(file)
        .unwrap()
        .iter()
        .map(|e| history.read_for_file(file, &e.id).unwrap())
        .collect()
}

/// The text of every entry the directory calls list for the spec in `dir`.
fn texts_for_dir(history: &LocalHistory, dir: &Path) -> Vec<String> {
    history
        .entries(dir)
        .unwrap()
        .iter()
        .map(|e| history.read(dir, &e.id).unwrap())
        .collect()
}

#[test]
fn two_chart_files_in_one_protocol_list_apart() {
    let f = setup();
    f.history
        .record_checkpoint_for_file(&f.a, CHART_A)
        .unwrap()
        .expect("recorded");
    f.history
        .record_checkpoint_for_file(&f.b, CHART_B)
        .unwrap()
        .expect("recorded");

    assert_eq!(texts_for_file(&f.history, &f.a), vec![CHART_A]);
    assert_eq!(texts_for_file(&f.history, &f.b), vec![CHART_B]);
}

#[test]
fn the_protocols_history_and_a_charts_history_do_not_see_each_other() {
    let f = setup();
    f.history
        .record_checkpoint(&f.dir, SPEC)
        .unwrap()
        .expect("recorded");
    f.history
        .record_checkpoint_for_file(&f.a, CHART_A)
        .unwrap()
        .expect("recorded");
    f.history
        .record_save_for_file(&f.b, CHART_B)
        .unwrap()
        .expect("recorded");

    // The directory's list is the spec's checkpoint and nothing else.
    assert_eq!(texts_for_dir(&f.history, &f.dir), vec![SPEC]);
    // Neither chart lists the spec's checkpoint.
    assert_eq!(texts_for_file(&f.history, &f.a), vec![CHART_A]);
    assert_eq!(texts_for_file(&f.history, &f.b), vec![CHART_B]);
}

#[test]
fn restoring_a_chart_writes_that_chart_alone_and_can_be_undone() {
    let f = setup();
    let first = f
        .history
        .record_save_for_file(&f.a, CHART_A)
        .unwrap()
        .expect("recorded");
    let edited = "# chart a\nmark: areaY\n";
    fs::write(&f.a, edited).unwrap();
    let spec_before = fs::read(f.dir.join(MANIFEST_FILENAME)).unwrap();
    let b_before = fs::read(&f.b).unwrap();

    let restored = f.history.restore_for_file(&f.a, &first.id).unwrap();

    assert_eq!(restored, CHART_A);
    assert_eq!(fs::read_to_string(&f.a).unwrap(), CHART_A);
    assert_eq!(
        fs::read(f.dir.join(MANIFEST_FILENAME)).unwrap(),
        spec_before
    );
    assert_eq!(fs::read(&f.b).unwrap(), b_before);

    // The text the restore replaced is the newest entry of the chart's own
    // history, as a checkpoint, and the spec's history gained nothing.
    let entries = f.history.entries_for_file(&f.a).unwrap();
    let replaced = entries.last().expect("the replaced text is listed");
    assert_eq!(replaced.kind, HistoryKind::Checkpoint);
    assert_eq!(f.history.read_for_file(&f.a, &replaced.id).unwrap(), edited);
    assert!(f.history.entries(&f.dir).unwrap().is_empty());

    // So the restore is itself undone by restoring that entry.
    f.history.restore_for_file(&f.a, &replaced.id).unwrap();
    assert_eq!(fs::read_to_string(&f.a).unwrap(), edited);
}

#[test]
fn a_protocols_history_recorded_before_the_file_calls_is_read_through_them() {
    let f = setup();
    // The store as the directory calls laid it out before a file had a key of
    // its own: the directory named by the first eight bytes of the SHA-256 of
    // the canonical protocol directory joined with `arcform.yaml`, in hex,
    // holding `<millis>-<seq>-<kind>.yaml` snapshots. Written by hand, so the
    // file calls must find a layout this change did not produce.
    let spec_path = f.dir.canonicalize().unwrap().join(MANIFEST_FILENAME);
    let digest = Sha256::digest(spec_path.as_os_str().as_encoded_bytes());
    let key: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let key_dir = f.history.root().join(key);
    fs::create_dir_all(&key_dir).unwrap();
    fs::write(key_dir.join("1700000000000-000-save.yaml"), SPEC).unwrap();
    fs::write(
        key_dir.join("1700000001000-000-checkpoint.yaml"),
        "name: older\n",
    )
    .unwrap();

    let manifest = f.dir.join(MANIFEST_FILENAME);
    let by_dir = f.history.entries(&f.dir).unwrap();
    assert_eq!(by_dir.len(), 2, "the directory call reads the old layout");
    assert_eq!(f.history.entries_for_file(&manifest).unwrap(), by_dir);
    assert_eq!(
        f.history
            .read_for_file(&manifest, "1700000001000-000-checkpoint")
            .unwrap(),
        "name: older\n"
    );

    // A path that reaches the spec another way is the same file.
    let roundabout = f.dir.join("panels").join("..").join(MANIFEST_FILENAME);
    assert_eq!(f.history.entries_for_file(&roundabout).unwrap(), by_dir);

    // And a save through the file call lands in the directory's history.
    f.history
        .record_save_for_file(&manifest, "name: newer\n")
        .unwrap()
        .expect("recorded");
    assert_eq!(
        texts_for_dir(&f.history, &f.dir).last().map(String::as_str),
        Some("name: newer\n")
    );
}

#[test]
fn the_bound_holds_per_file() {
    let f = setup();
    for i in 0..HISTORY_MAX_ENTRIES {
        f.history
            .record_checkpoint_for_file(&f.a, &format!("a: {i}\n"))
            .unwrap()
            .expect("recorded");
    }
    for i in 0..2 {
        f.history
            .record_checkpoint_for_file(&f.b, &format!("b: {i}\n"))
            .unwrap()
            .expect("recorded");
    }
    assert_eq!(
        f.history.entries_for_file(&f.a).unwrap().len(),
        HISTORY_MAX_ENTRIES
    );

    f.history
        .record_checkpoint_for_file(&f.a, "a: one more\n")
        .unwrap()
        .expect("recorded");

    let a = texts_for_file(&f.history, &f.a);
    assert_eq!(a.len(), HISTORY_MAX_ENTRIES);
    assert_eq!(
        a.first().map(String::as_str),
        Some("a: 1\n"),
        "oldest pruned"
    );
    assert_eq!(a.last().map(String::as_str), Some("a: one more\n"));
    assert_eq!(texts_for_file(&f.history, &f.b), vec!["b: 0\n", "b: 1\n"]);
}

/// The entry files in `file`'s key directory, sorted by name: every `.yaml`
/// file there, and not the `spec-path` marker beside them. The key directory
/// is the one in `history`'s store whose `spec-path` names `file`.
fn entry_files_for_file(history: &LocalHistory, file: &Path) -> Vec<String> {
    let canonical = fs::canonicalize(file).unwrap();
    let key_dirs: Vec<PathBuf> = fs::read_dir(history.root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|dir| {
            fs::read_to_string(dir.join("spec-path"))
                .is_ok_and(|named| named.trim_end() == canonical.display().to_string())
        })
        .collect();
    let [key_dir] = key_dirs.as_slice() else {
        panic!("one key directory for {file:?}, found {key_dirs:?}");
    };
    let mut names: Vec<String> = fs::read_dir(key_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".yaml"))
        .collect();
    names.sort();
    names
}

// The bound per file, for a store whose entries name a way: one file recorded
// `HISTORY_MAX_ENTRIES + 10` times through a handle that names a way lists and
// holds on disk exactly the newest `HISTORY_MAX_ENTRIES`, and another file's two
// entries are neither listed nor removed with them. Prune removes an entry by the
// name its way is part of.
#[test]
fn the_bound_holds_per_file_for_entries_that_name_a_way() {
    let ways = [
        HistoryWay::TERMINAL,
        HistoryWay::MCP,
        HistoryWay::new("app").unwrap(),
    ];
    for way in ways {
        let f = setup();
        let history = f.history.clone().reached_by(way.clone());
        let recorded: Vec<String> = (0..HISTORY_MAX_ENTRIES + 10)
            .map(|i| {
                history
                    .record_checkpoint_for_file(&f.a, &format!("a: {i}\n"))
                    .unwrap()
                    .expect("recorded")
                    .id
            })
            .collect();
        for i in 0..2 {
            history
                .record_checkpoint_for_file(&f.b, &format!("b: {i}\n"))
                .unwrap()
                .expect("recorded");
        }
        let kept = &recorded[10..];

        let listed: Vec<String> = history
            .entries_for_file(&f.a)
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(
            listed,
            kept,
            "{}: the list holds the newest {HISTORY_MAX_ENTRIES}, oldest first",
            way.as_str()
        );
        let mut on_disk: Vec<String> = kept
            .iter()
            .map(|id| format!("{id}.{}.yaml", way.as_str()))
            .collect();
        on_disk.sort();
        assert_eq!(
            entry_files_for_file(&history, &f.a),
            on_disk,
            "{}: the key directory holds exactly the kept entries' files, and none of the pruned",
            way.as_str()
        );
        assert_eq!(
            entry_files_for_file(&history, &f.b).len(),
            2,
            "{}: the other file's entries are untouched",
            way.as_str()
        );
    }
}

#[test]
fn a_path_naming_a_directory_or_no_file_is_refused_by_every_file_call() {
    let f = setup();
    let id = "1700000000000-000-save";
    // The empty path is the one a directory check alone would let through:
    // it names no file, and it is not a directory either.
    for path in [
        f.dir.clone(),
        f.dir.join("panels").join(".."),
        PathBuf::new(),
    ] {
        let refusals = [
            (
                "record_save_for_file",
                f.history.record_save_for_file(&path, SPEC).map(drop),
            ),
            (
                "record_save_unmerged_for_file",
                f.history
                    .record_save_unmerged_for_file(&path, SPEC)
                    .map(drop),
            ),
            (
                "record_checkpoint_for_file",
                f.history.record_checkpoint_for_file(&path, SPEC).map(drop),
            ),
            (
                "record_unsaved_for_file",
                f.history.record_unsaved_for_file(&path, SPEC).map(drop),
            ),
            (
                "entries_for_file",
                f.history.entries_for_file(&path).map(drop),
            ),
            (
                "read_for_file",
                f.history.read_for_file(&path, id).map(drop),
            ),
            (
                "restore_for_file",
                f.history.restore_for_file(&path, id).map(drop),
            ),
        ];
        for (call, result) in refusals {
            let refused = result.expect_err(call);
            assert!(
                matches!(&refused, Error::FileRead { path: p, .. } if *p == path),
                "{call}({path:?}): {refused}"
            );
        }
    }
    // Refused before anything is recorded: the store was never created.
    assert!(!f.history.root().exists());
}

#[test]
fn a_file_whose_directory_does_not_exist_is_refused_naming_the_directory() {
    let f = setup();
    let missing = f.dir.join("missing");
    let refused = f
        .history
        .record_save_for_file(&missing.join("c.yaml"), CHART_A)
        .unwrap_err();
    assert!(
        matches!(&refused, Error::FileRead { path, .. } if *path == missing),
        "{refused}"
    );
    assert!(!f.history.root().exists());
}

#[test]
fn rapid_saves_of_one_chart_merge_into_one_entry() {
    let f = setup();
    f.history.record_save_for_file(&f.a, "mark: dot\n").unwrap();
    f.history.record_save_for_file(&f.a, CHART_A).unwrap();
    assert_eq!(texts_for_file(&f.history, &f.a), vec![CHART_A]);
}

#[test]
fn a_deleted_chart_is_restored_from_its_own_history() {
    let f = setup();
    let saved = f
        .history
        .record_save_for_file(&f.a, CHART_A)
        .unwrap()
        .expect("recorded");
    fs::remove_file(&f.a).unwrap();

    assert_eq!(
        f.history.restore_for_file(&f.a, &saved.id).unwrap(),
        CHART_A
    );
    assert_eq!(fs::read_to_string(&f.a).unwrap(), CHART_A);
    // Nothing stood at the path, so there was nothing to checkpoint.
    assert_eq!(texts_for_file(&f.history, &f.a), vec![CHART_A]);
}

#[test]
fn a_restore_over_a_chart_that_is_not_text_is_refused_and_the_bytes_kept() {
    let f = setup();
    let saved = f
        .history
        .record_save_for_file(&f.a, CHART_A)
        .unwrap()
        .expect("recorded");
    let not_text = [0xff, 0xfe, b'\n'];
    fs::write(&f.a, not_text).unwrap();

    let refused = f.history.restore_for_file(&f.a, &saved.id).unwrap_err();
    assert!(
        matches!(&refused, Error::FileRead { path, .. } if *path == f.a),
        "{refused}"
    );
    assert_eq!(fs::read(&f.a).unwrap(), not_text);
}

#[test]
fn a_restore_whose_checkpoint_cannot_be_recorded_writes_nothing() {
    let f = setup();
    let saved = f
        .history
        .record_save_for_file(&f.a, CHART_A)
        .unwrap()
        .expect("recorded");
    let edited = "# chart a\nmark: areaY\n";
    fs::write(&f.a, edited).unwrap();

    // The chart's key directory is the only one in the store. A directory
    // named as its newest entry cannot be read as text, so the checkpoint the
    // restore records first fails on every platform and for every user.
    let key_dirs: Vec<PathBuf> = fs::read_dir(f.history.root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [key_dir] = key_dirs.as_slice() else {
        panic!("one key directory, found {key_dirs:?}");
    };
    fs::create_dir(key_dir.join("9999999999999-000-save.yaml")).unwrap();

    assert!(f.history.restore_for_file(&f.a, &saved.id).is_err());
    assert_eq!(fs::read_to_string(&f.a).unwrap(), edited);
}

#[test]
fn the_directory_calls_still_refuse_a_missing_directory_by_name() {
    let f = setup();
    let missing = f.dir.join("missing");
    let id = "1700000000000-000-save";
    let refusals = [
        (
            "record_save",
            f.history.record_save(&missing, SPEC).map(drop),
        ),
        (
            "record_save_unmerged",
            f.history.record_save_unmerged(&missing, SPEC).map(drop),
        ),
        (
            "record_checkpoint",
            f.history.record_checkpoint(&missing, SPEC).map(drop),
        ),
        ("restore", f.history.restore(&missing, id).map(drop)),
    ];
    for (call, result) in refusals {
        let refused = result.expect_err(call);
        assert!(
            matches!(&refused, Error::FileRead { path, .. } if *path == missing),
            "{call}: {refused}"
        );
    }
    assert!(!f.history.root().exists());
}

#[test]
fn the_file_calls_are_on_arc_spec_and_the_directory_calls_keep_their_signatures() {
    // Each coercion fails to compile if its call is missing from `arc::spec`
    // or its signature changes.
    type Record = fn(&LocalHistory, &Path, &str) -> Result<Option<HistoryEntry>>;
    type Entries = fn(&LocalHistory, &Path) -> Result<Vec<HistoryEntry>>;
    type Read = fn(&LocalHistory, &Path, &str) -> Result<String>;

    let by_dir: (Record, Record, Entries, Read, Read) = (
        LocalHistory::record_save,
        LocalHistory::record_checkpoint,
        LocalHistory::entries,
        LocalHistory::read,
        LocalHistory::restore,
    );
    let by_file: (Record, Record, Entries, Read, Read) = (
        LocalHistory::record_save_for_file,
        LocalHistory::record_checkpoint_for_file,
        LocalHistory::entries_for_file,
        LocalHistory::read_for_file,
        LocalHistory::restore_for_file,
    );

    // And the two sets reach one history for the spec.
    let f = setup();
    (by_dir.0)(&f.history, &f.dir, SPEC).unwrap();
    let manifest = f.dir.join(MANIFEST_FILENAME);
    assert_eq!(
        (by_file.2)(&f.history, &manifest).unwrap(),
        (by_dir.2)(&f.history, &f.dir).unwrap()
    );
}

// ------------------------------------------------- a text never written to its file

/// A text no file on disk holds: a trailing space, a line with no newline at
/// its end, a carriage return and a character outside ASCII, so a read that
/// trims, normalises or re-encodes it reads back something else.
const UNSAVED: &str = "# chart a \r\nmark: dot\ntitle: Δ revenue";

/// Each entry `file` lists as its kind and its text, oldest first.
fn kinds_and_texts(history: &LocalHistory, file: &Path) -> Vec<(HistoryKind, String)> {
    history
        .entries_for_file(file)
        .unwrap()
        .iter()
        .map(|e| (e.kind, history.read_for_file(file, &e.id).unwrap()))
        .collect()
}

#[test]
fn a_text_never_written_to_its_file_lists_as_unsaved_and_reads_back_byte_for_byte() {
    let f = setup();
    let recorded = f
        .history
        .record_unsaved_for_file(&f.a, UNSAVED)
        .unwrap()
        .expect("recorded");
    assert_eq!(recorded.kind, HistoryKind::Unsaved);
    assert_eq!(recorded.bytes, UNSAVED.len() as u64);

    let entries = f.history.entries_for_file(&f.a).unwrap();
    assert_eq!(entries, vec![recorded.clone()], "listed as it was returned");
    let kind = entries[0].kind;
    assert!(
        kind != HistoryKind::Save && kind != HistoryKind::Checkpoint,
        "an unsaved text lists as neither a save nor a checkpoint: {kind:?}"
    );
    assert_eq!(
        f.history.read_for_file(&f.a, &recorded.id).unwrap(),
        UNSAVED
    );
}

#[test]
fn recording_an_unsaved_text_leaves_its_file_as_it_was_and_creates_none() {
    let f = setup();
    f.history.record_unsaved_for_file(&f.a, UNSAVED).unwrap();
    assert_eq!(fs::read(&f.a).unwrap(), CHART_A.as_bytes());

    // A file never written at all keeps a history, and stays unwritten.
    let never = f.dir.join("panels").join("never.yaml");
    let recorded = f
        .history
        .record_unsaved_for_file(&never, UNSAVED)
        .unwrap()
        .expect("recorded");
    assert!(!never.exists(), "recording created {never:?}");
    assert_eq!(
        f.history.read_for_file(&never, &recorded.id).unwrap(),
        UNSAVED
    );

    let mut panels: Vec<String> = fs::read_dir(f.dir.join("panels"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    panels.sort();
    assert_eq!(
        panels,
        ["a.yaml", "b.yaml"],
        "nothing else lands beside them"
    );
    assert_eq!(fs::read(&f.b).unwrap(), CHART_B.as_bytes());
}

#[test]
fn two_unmerged_saves_of_a_file_inside_the_window_are_two_entries_of_kind_save_in_its_history() {
    let f = setup();
    f.history
        .record_save_unmerged_for_file(&f.a, "mark: lineY\n")
        .unwrap()
        .expect("recorded");
    f.history
        .record_save_unmerged_for_file(&f.a, "mark: areaY\n")
        .unwrap()
        .expect("recorded");

    let entries = f.history.entries_for_file(&f.a).unwrap();
    let span = entries[entries.len() - 1]
        .at
        .duration_since(entries[0].at)
        .unwrap();
    assert!(
        span <= HISTORY_MERGE_WINDOW,
        "the two were recorded {span:?} apart, outside the window this test is about"
    );
    assert_eq!(
        kinds_and_texts(&f.history, &f.a),
        vec![
            (HistoryKind::Save, "mark: lineY\n".to_string()),
            (HistoryKind::Save, "mark: areaY\n".to_string()),
        ],
        "a save with the merge off is a save, and neither replaced the other"
    );

    assert!(
        f.history.entries_for_file(&f.b).unwrap().is_empty(),
        "another file's history shows neither"
    );
    assert!(
        f.history.entries(&f.dir).unwrap().is_empty(),
        "nor does the Protocol's"
    );
}

#[test]
fn a_file_save_after_an_unmerged_one_still_merges_into_it() {
    let f = setup();
    f.history
        .record_save_unmerged_for_file(&f.a, "mark: lineY\n")
        .unwrap();
    f.history
        .record_save_for_file(&f.a, "mark: areaY\n")
        .unwrap();
    assert_eq!(
        kinds_and_texts(&f.history, &f.a),
        vec![(HistoryKind::Save, "mark: areaY\n".to_string())],
        "the merging call folds into the newest save inside the window"
    );
}

#[test]
fn an_unmerged_save_of_a_file_identical_to_its_newest_entry_is_not_recorded_again() {
    let f = setup();
    assert!(
        f.history
            .record_save_unmerged_for_file(&f.a, CHART_A)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.history
            .record_save_unmerged_for_file(&f.a, CHART_A)
            .unwrap(),
        None
    );
    assert_eq!(f.history.entries_for_file(&f.a).unwrap().len(), 1);
}

#[test]
fn an_unsaved_text_and_a_save_either_side_of_it_are_entries_of_their_own() {
    let f = setup();
    f.history
        .record_save_for_file(&f.a, "mark: lineY\n")
        .unwrap();
    f.history.record_unsaved_for_file(&f.a, UNSAVED).unwrap();
    f.history
        .record_save_for_file(&f.a, "mark: areaY\n")
        .unwrap();

    let entries = f.history.entries_for_file(&f.a).unwrap();
    let span = entries[entries.len() - 1]
        .at
        .duration_since(entries[0].at)
        .unwrap();
    assert!(
        span <= HISTORY_MERGE_WINDOW,
        "the three were recorded {span:?} apart, outside the window this test is about"
    );
    assert_eq!(
        kinds_and_texts(&f.history, &f.a),
        vec![
            (HistoryKind::Save, "mark: lineY\n".to_string()),
            (HistoryKind::Unsaved, UNSAVED.to_string()),
            (HistoryKind::Save, "mark: areaY\n".to_string()),
        ],
        "neither the unsaved text nor the save after it replaced the entry before it"
    );
}

#[test]
fn an_unsaved_text_identical_to_the_newest_entry_is_not_recorded_again() {
    let f = setup();
    f.history.record_save_for_file(&f.a, CHART_A).unwrap();
    assert_eq!(
        f.history.record_unsaved_for_file(&f.a, CHART_A).unwrap(),
        None
    );
    f.history.record_unsaved_for_file(&f.a, UNSAVED).unwrap();
    assert_eq!(
        f.history.record_unsaved_for_file(&f.a, UNSAVED).unwrap(),
        None
    );
    assert_eq!(
        kinds_and_texts(&f.history, &f.a),
        vec![
            (HistoryKind::Save, CHART_A.to_string()),
            (HistoryKind::Unsaved, UNSAVED.to_string()),
        ]
    );
}

#[test]
fn an_unsaved_entry_counts_toward_the_bound_and_the_oldest_goes_whatever_its_kind() {
    let f = setup();
    f.history.record_save_for_file(&f.a, "save\n").unwrap();
    f.history
        .record_unsaved_for_file(&f.a, "unsaved\n")
        .unwrap();
    for i in 0..HISTORY_MAX_ENTRIES - 2 {
        f.history
            .record_checkpoint_for_file(&f.a, &format!("checkpoint {i}\n"))
            .unwrap()
            .expect("recorded");
    }
    assert_eq!(
        f.history.entries_for_file(&f.a).unwrap().len(),
        HISTORY_MAX_ENTRIES
    );

    // An unsaved text recorded at the bound prunes the oldest entry, a save.
    f.history
        .record_unsaved_for_file(&f.a, "unsaved past the bound\n")
        .unwrap()
        .expect("recorded");
    let after = kinds_and_texts(&f.history, &f.a);
    assert_eq!(after.len(), HISTORY_MAX_ENTRIES);
    assert_eq!(after[0], (HistoryKind::Unsaved, "unsaved\n".to_string()));
    assert_eq!(
        after[HISTORY_MAX_ENTRIES - 1],
        (HistoryKind::Unsaved, "unsaved past the bound\n".to_string())
    );

    // The next entry prunes the oldest again, an unsaved one this time.
    f.history
        .record_checkpoint_for_file(&f.a, "one more\n")
        .unwrap()
        .expect("recorded");
    let after = kinds_and_texts(&f.history, &f.a);
    assert_eq!(after.len(), HISTORY_MAX_ENTRIES);
    assert_eq!(
        after[0],
        (HistoryKind::Checkpoint, "checkpoint 0\n".to_string())
    );
    assert_eq!(
        after
            .iter()
            .filter(|(k, _)| *k == HistoryKind::Unsaved)
            .count(),
        1,
        "the newest unsaved entry stays"
    );
}

#[test]
fn a_store_holding_an_unsaved_entry_lists_and_restores_its_saves_and_checkpoints() {
    let f = setup();
    let id = |e: Option<HistoryEntry>| e.expect("recorded").id;
    let save = id(f
        .history
        .record_save_for_file(&f.a, "mark: lineY\n")
        .unwrap());
    let checkpoint = id(f
        .history
        .record_checkpoint_for_file(&f.a, "mark: areaY\n")
        .unwrap());
    let unsaved = id(f.history.record_unsaved_for_file(&f.a, UNSAVED).unwrap());

    // A handle opened afresh on the same root reads the store from disk, so
    // each kind is read back from its entry's file name.
    let reopened = LocalHistory::at_root(f.history.root());
    let listed: Vec<(String, HistoryKind)> = reopened
        .entries_for_file(&f.a)
        .unwrap()
        .into_iter()
        .map(|e| (e.id, e.kind))
        .collect();
    assert_eq!(
        listed,
        vec![
            (save.clone(), HistoryKind::Save),
            (checkpoint.clone(), HistoryKind::Checkpoint),
            (unsaved.clone(), HistoryKind::Unsaved),
        ]
    );

    for (id, text) in [(&save, "mark: lineY\n"), (&checkpoint, "mark: areaY\n")] {
        assert_eq!(reopened.restore_for_file(&f.a, id).unwrap(), text);
        assert_eq!(fs::read_to_string(&f.a).unwrap(), text, "{id} restored");
    }
    assert_eq!(reopened.restore_for_file(&f.a, &unsaved).unwrap(), UNSAVED);
    assert_eq!(fs::read_to_string(&f.a).unwrap(), UNSAVED);
}
