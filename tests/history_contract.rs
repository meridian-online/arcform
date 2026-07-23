//! The local-history contract, exercised through the published surface the
//! way an editing tool would use it:
//!
//!   1. **outside the project** — recording touches nothing in the protocol
//!      directory: no new files, no changed spec, nothing for `git status`;
//!   2. **checkpoint before machine edit** — the checkpointed roads record
//!      the state being replaced, distinct in kind from a save entry, before
//!      the write lands — and a refusal touches neither file nor history;
//!   3. **rollback with no git** — a spec in a directory that has never seen
//!      a repository rolls back to any recorded state, and the rollback is
//!      itself reversible because restore checkpoints what it replaces;
//!   4. **bounded by the stated policy** — entries past the bound prune
//!      oldest-first, and an identical or rapid-repeat save does not flood
//!      the store.

use std::fs;
use std::path::{Path, PathBuf};

use arc::spec::{
    Error, HISTORY_MAX_ENTRIES, HistoryKind, LocalHistory, MANIFEST_FILENAME, RecordedStep,
    SpecEdit, edit_spec_with_history, record_step_with_history,
};

const SPEC: &str = "\
name: fixture
engine: duckdb

steps:
  # The only step. This comment is authorship the write path preserves.
  - name: only
    command: \"true\"
";

/// A protocol directory and a history store, both inside one temp dir but
/// disjoint — the store is never inside the protocol.
fn setup() -> (tempfile::TempDir, PathBuf, LocalHistory) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("protocol");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(MANIFEST_FILENAME), SPEC).unwrap();
    let history = LocalHistory::at_root(tmp.path().join("history"));
    (tmp, dir, history)
}

/// Every file under `dir`, relative, sorted — the whole observable content
/// of the protocol directory.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push(path.strip_prefix(base).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn a_save_records_outside_the_protocol_and_the_protocol_is_untouched() {
    let (_tmp, dir, history) = setup();
    let before = files_under(&dir);

    let entry = history.record_save(&dir, SPEC).unwrap().expect("recorded");
    assert_eq!(entry.kind, HistoryKind::Save);

    // The protocol directory is exactly what it was: nothing new for a diff,
    // nothing new for `git status`.
    assert_eq!(files_under(&dir), before);
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC
    );

    // The store exists, outside the protocol.
    assert!(history.root().exists());
    assert!(!history.root().starts_with(&dir));

    // And the entry is legible from it.
    let entries = history.entries(&dir).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(history.read(&dir, &entries[0].id).unwrap(), SPEC);
}

#[test]
fn an_identical_save_is_not_recorded_twice() {
    let (_tmp, dir, history) = setup();
    assert!(history.record_save(&dir, SPEC).unwrap().is_some());
    assert!(history.record_save(&dir, SPEC).unwrap().is_none());
    assert_eq!(history.entries(&dir).unwrap().len(), 1);
}

#[test]
fn rapid_saves_debounce_into_one_entry() {
    let (_tmp, dir, history) = setup();
    history.record_save(&dir, "name: a\nsteps: []\n").unwrap();
    history.record_save(&dir, "name: b\nsteps: []\n").unwrap();
    let entries = history.entries(&dir).unwrap();
    assert_eq!(entries.len(), 1, "saves within the window merge");
    assert_eq!(
        history.read(&dir, &entries[0].id).unwrap(),
        "name: b\nsteps: []\n",
        "the newer state wins the merge"
    );
}

#[test]
fn a_machine_edit_checkpoints_the_state_it_replaces_before_it_lands() {
    let (_tmp, dir, history) = setup();

    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 0.into(), "command".into()],
        value: "\"false\"".to_string(),
    };
    let validated = edit_spec_with_history(&dir, &[edit], &history).unwrap();

    let entries = history.entries(&dir).unwrap();
    assert_eq!(entries.len(), 2);

    // The before-image, distinctly a checkpoint, recorded first.
    assert_eq!(entries[0].kind, HistoryKind::Checkpoint);
    assert_eq!(
        history.read(&dir, &entries[0].id).unwrap(),
        SPEC,
        "the checkpoint carries the bytes the edit replaced"
    );

    // The after-image, distinctly a save.
    assert_eq!(entries[1].kind, HistoryKind::Save);
    assert_eq!(
        history.read(&dir, &entries[1].id).unwrap(),
        validated.text()
    );
    assert!(validated.text().contains("\"false\""));
}

#[test]
fn a_refused_edit_touches_neither_the_file_nor_the_history() {
    let (_tmp, dir, history) = setup();
    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 9.into(), "command".into()],
        value: "\"x\"".to_string(),
    };
    let err = edit_spec_with_history(&dir, &[edit], &history).unwrap_err();
    assert!(matches!(err, Error::EditTarget { .. }), "{err}");
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC
    );
    assert_eq!(
        history.entries(&dir).unwrap().len(),
        0,
        "a refusal records nothing — the file did not change"
    );
}

#[test]
fn a_machine_save_cannot_fold_away_a_just_checkpointed_state() {
    // A save entry records the current state; a machine edit follows within
    // the merge window. If its after-image save were allowed to merge, the
    // save entry holding the replaced state would be overwritten — the exact
    // loss the checkpoint discipline exists to prevent.
    let (_tmp, dir, history) = setup();
    history.record_save(&dir, SPEC).unwrap();

    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 0.into(), "command".into()],
        value: "\"false\"".to_string(),
    };
    edit_spec_with_history(&dir, &[edit], &history).unwrap();

    let entries = history.entries(&dir).unwrap();
    let texts: Vec<String> = entries
        .iter()
        .map(|e| history.read(&dir, &e.id).unwrap())
        .collect();
    assert!(
        texts.contains(&SPEC.to_string()),
        "the replaced state must survive the edit: {texts:?}"
    );
}

#[test]
fn a_spec_rolls_back_with_no_git_and_the_rollback_is_itself_reversible() {
    let (tmp, dir, history) = setup();
    // No repository anywhere in sight: not the protocol, not the store.
    assert!(!tmp.path().join(".git").exists());
    assert!(!dir.join(".git").exists());

    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 0.into(), "command".into()],
        value: "\"false\"".to_string(),
    };
    let validated = edit_spec_with_history(&dir, &[edit], &history).unwrap();
    let edited = validated.text().to_string();

    // Roll back to the checkpointed original.
    let entries = history.entries(&dir).unwrap();
    let checkpoint = entries
        .iter()
        .find(|e| e.kind == HistoryKind::Checkpoint)
        .expect("the edit checkpointed");
    let restored = history.restore(&dir, &checkpoint.id).unwrap();
    assert_eq!(restored, SPEC);
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC
    );

    // The rollback is reversible: the state it replaced is in the history —
    // here as the newest save entry, against which the rollback's own
    // checkpoint deduplicated (an identical state is never recorded twice).
    let entries = history.entries(&dir).unwrap();
    let replaced = entries
        .iter()
        .rev()
        .find(|e| history.read(&dir, &e.id).unwrap() == edited)
        .expect("the replaced state is recorded");
    history.restore(&dir, &replaced.id).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        edited,
        "restoring the rollback's checkpoint goes forward again"
    );

    // Still no repository anywhere: nothing was promoted to git.
    assert!(!tmp.path().join(".git").exists());
    assert!(files_under(&dir) == vec![MANIFEST_FILENAME.to_string()]);
}

#[test]
fn the_store_is_bounded_by_the_stated_policy() {
    let (_tmp, dir, history) = setup();
    for n in 0..HISTORY_MAX_ENTRIES + 5 {
        history
            .record_checkpoint(&dir, &format!("name: v{n}\nsteps: []\n"))
            .unwrap();
    }
    let entries = history.entries(&dir).unwrap();
    assert_eq!(entries.len(), HISTORY_MAX_ENTRIES);
    assert_eq!(
        history.read(&dir, &entries[0].id).unwrap(),
        "name: v5\nsteps: []\n",
        "the oldest entries were pruned first"
    );
}

#[test]
fn protocols_do_not_share_history() {
    let tmp = tempfile::tempdir().unwrap();
    let history = LocalHistory::at_root(tmp.path().join("history"));
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    for dir in [&a, &b] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(MANIFEST_FILENAME), SPEC).unwrap();
    }
    history.record_save(&a, SPEC).unwrap();
    assert_eq!(history.entries(&a).unwrap().len(), 1);
    assert_eq!(history.entries(&b).unwrap().len(), 0);
}

#[test]
fn an_unknown_entry_is_refused_by_name() {
    let (_tmp, dir, history) = setup();
    for id in ["nope", "1700000000000-000-save", "../escape"] {
        match history.read(&dir, id) {
            Err(Error::HistoryEntryNotFound { id: named }) => assert_eq!(named, id),
            other => panic!("expected HistoryEntryNotFound for {id:?}, got {other:?}"),
        }
    }
}

#[test]
fn a_recorded_step_checkpoints_the_manifest_first() {
    let (_tmp, dir, history) = setup();
    let step = RecordedStep {
        name: "derived".to_string(),
        sql: "SELECT 1 AS x".to_string(),
        provenance: "history contract test".to_string(),
    };
    let (sql_rel, validated) = record_step_with_history(&dir, &step, &history).unwrap();
    assert!(dir.join(&sql_rel).exists());
    assert_eq!(validated.manifest().steps.len(), 2);

    let entries = history.entries(&dir).unwrap();
    assert_eq!(entries[0].kind, HistoryKind::Checkpoint);
    assert_eq!(
        history.read(&dir, &entries[0].id).unwrap(),
        SPEC,
        "the pre-promotion manifest is the checkpoint"
    );
    assert_eq!(entries.last().unwrap().kind, HistoryKind::Save);
}
