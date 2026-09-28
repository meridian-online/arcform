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
//!      oldest entry and none of another file's.
//!
//! Nothing here needs the `arc` binary, so the file runs with the `cli`
//! feature off: `cargo test --no-default-features --test history_per_file`.

use std::fs;
use std::path::{Path, PathBuf};

use arc::spec::{
    Error, HISTORY_MAX_ENTRIES, HistoryEntry, HistoryKind, LocalHistory, MANIFEST_FILENAME, Result,
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
    assert_eq!(fs::read(f.dir.join(MANIFEST_FILENAME)).unwrap(), spec_before);
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
    assert_eq!(a.first().map(String::as_str), Some("a: 1\n"), "oldest pruned");
    assert_eq!(a.last().map(String::as_str), Some("a: one more\n"));
    assert_eq!(texts_for_file(&f.history, &f.b), vec!["b: 0\n", "b: 1\n"]);
}

#[test]
fn a_path_naming_a_directory_or_no_file_is_refused() {
    let f = setup();
    for path in [f.dir.clone(), f.dir.join("panels").join("..")] {
        let refused = f.history.record_save_for_file(&path, SPEC).unwrap_err();
        assert!(
            matches!(&refused, Error::FileRead { path: p, .. } if *p == path),
            "{path:?}: {refused}"
        );
    }
    // Refused before anything is recorded: the store was never created.
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
