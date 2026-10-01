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
//!      oldest-first, whether or not their file names a way, and an identical
//!      or rapid-repeat save does not flood the store;
//!   5. **the way arc was reached** — a way a caller names is on every entry
//!      its handle records, through each recording call, and `arc history
//!      list` prints it; a handle that names none records entries that say
//!      so; and a way arc cannot store, or one of arc's own words spelt by a
//!      caller, is refused before anything is written.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use arc::spec::{
    Error, HISTORY_MAX_ENTRIES, HistoryKind, HistoryWay, LocalHistory, MANIFEST_FILENAME,
    RecordedStep, SpecEdit, edit_spec_with_history, record_step_with_history,
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

// No checkpoint, no write, when it is the checkpoint's own snapshot that
// cannot be written: the spec's place in the store takes no new file, so the
// entry fails to land and the edit is refused with the spec as it was.
#[cfg(unix)]
#[test]
fn an_edit_whose_checkpoint_cannot_be_written_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, dir, history) = setup();
    history.record_save(&dir, "name: earlier\n").unwrap();
    let entries_before = history.entries(&dir).unwrap();
    let key_dirs: Vec<PathBuf> = fs::read_dir(history.root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [key_dir] = key_dirs.as_slice() else {
        panic!("one key directory, found {key_dirs:?}");
    };

    // Reads stay allowed; creating the entry's temp file does not.
    fs::set_permissions(key_dir, fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores permission bits, so the failure cannot be staged there —
    // probe, and stand down rather than mis-assert.
    let probe = key_dir.join(".probe");
    if fs::write(&probe, b"x").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(key_dir, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let result = edit_spec_with_history(&dir, &[rename_edit("renamed")], &history);
    let checkpoint = history.record_checkpoint(&dir, "name: other\n");
    fs::set_permissions(key_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err(), "the edit must be refused, got {result:?}");
    assert!(
        checkpoint.is_err(),
        "the checkpoint must fail, got {checkpoint:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
        SPEC,
        "the spec is as it was"
    );
    assert_eq!(history.entries(&dir).unwrap(), entries_before);
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

/// The entry files in the one key directory `history`'s store holds, sorted
/// by name: every `.yaml` file there, and not the `spec-path` marker beside
/// them. Panics when the store holds anything but one key directory.
fn entry_files(history: &LocalHistory) -> Vec<String> {
    let key_dirs: Vec<PathBuf> = fs::read_dir(history.root())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [key_dir] = key_dirs.as_slice() else {
        panic!("one key directory, found {key_dirs:?}");
    };
    let mut names: Vec<String> = fs::read_dir(key_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".yaml"))
        .collect();
    names.sort();
    names
}

/// Record `HISTORY_MAX_ENTRIES + 10` distinct states for the spec in `dir`
/// through `history`, as checkpoints so none merges into the one before, and
/// return each entry's id in the order recorded.
fn record_past_the_bound(history: &LocalHistory, dir: &Path) -> Vec<String> {
    (0..HISTORY_MAX_ENTRIES + 10)
        .map(|n| {
            history
                .record_checkpoint(dir, &format!("name: v{n}\nsteps: []\n"))
                .unwrap()
                .expect("a state no entry holds is recorded")
                .id
        })
        .collect()
}

// Every entry a handle that names a way records carries the way in its file
// name, and prune removes an entry by that name. Past the bound the store
// holds exactly the newest `HISTORY_MAX_ENTRIES` — listed and on disk — and the
// ten oldest are gone from both, for each way arc itself is reached by and for
// a caller's own.
#[test]
fn the_bound_holds_for_a_store_whose_entries_name_a_way() {
    let ways = [
        HistoryWay::TERMINAL,
        HistoryWay::MCP,
        HistoryWay::new("app").unwrap(),
    ];
    for way in ways {
        let (_tmp, dir, history) = setup();
        let history = history.reached_by(way.clone());
        let recorded = record_past_the_bound(&history, &dir);
        let kept = &recorded[10..];

        let listed: Vec<String> = history
            .entries(&dir)
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
            entry_files(&history),
            on_disk,
            "{}: the key directory holds exactly the kept entries' files, and none of the pruned",
            way.as_str()
        );
    }
}

// A store older entries wrote without a way and newer ones wrote with one:
// prune removes each entry by the name it has, so the ten oldest, which name no
// way, go and the bound holds over a directory of both kinds of file name.
#[test]
fn the_bound_holds_when_the_oldest_entries_name_no_way() {
    let (_tmp, dir, history) = setup();
    let mut recorded: Vec<String> = (0..10)
        .map(|n| {
            history
                .record_checkpoint(&dir, &format!("name: earlier{n}\nsteps: []\n"))
                .unwrap()
                .expect("recorded")
                .id
        })
        .collect();
    let history = history.reached_by(HistoryWay::TERMINAL);
    recorded.extend((0..HISTORY_MAX_ENTRIES).map(|n| {
        history
            .record_checkpoint(&dir, &format!("name: v{n}\nsteps: []\n"))
            .unwrap()
            .expect("recorded")
            .id
    }));
    let kept = &recorded[10..];

    let listed: Vec<String> = history
        .entries(&dir)
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(listed, kept, "the ten that named no way were the oldest");
    let mut on_disk: Vec<String> = kept
        .iter()
        .map(|id| format!("{id}.terminal.yaml"))
        .collect();
    on_disk.sort();
    assert_eq!(
        entry_files(&history),
        on_disk,
        "no file of a pruned entry is left in the key directory"
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
        description: None,
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

// ------------------------------------------------------------------ the way

/// `arc history list` for the spec in `dir`, run by the real binary against
/// `history`'s store: each entry line's id and the words after its size.
fn listed_ways(history: &LocalHistory, dir: &Path) -> Vec<(String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_HISTORY_DIR", history.root())
        .args(["history", "list"])
        .output()
        .expect("spawn arc");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let (head, way) = line.split_once(" bytes  ")?;
            Some((head.split_whitespace().next()?.to_string(), way.to_string()))
        })
        .collect()
}

fn rename_edit(to: &str) -> SpecEdit {
    SpecEdit::Replace {
        path: vec!["name".into()],
        value: to.to_string(),
    }
}

// A caller names `app` once, on the handle, and every call that records
// through it — a save, a checkpoint, both checkpointed roads, the calls on a
// file and a restore — writes entries naming `app`: on the entry the call
// returns, in `entries`, and in the list the command line prints.
#[test]
fn a_way_a_caller_names_is_on_every_entry_its_calls_record() {
    let (tmp, dir, history) = setup();
    let app = HistoryWay::new("app").unwrap();
    assert_eq!(app.as_str(), "app");
    let history = history.reached_by(app.clone());

    let saved = history.record_save(&dir, "name: a\n").unwrap().unwrap();
    assert_eq!(saved.way, Some(app.clone()), "the entry a save returns");
    let checkpoint = history
        .record_checkpoint(&dir, "name: b\n")
        .unwrap()
        .unwrap();
    assert_eq!(
        checkpoint.way,
        Some(app.clone()),
        "the entry a checkpoint returns"
    );
    assert_eq!(
        history.entries(&dir).unwrap(),
        [saved.clone(), checkpoint],
        "the entries the calls return are the entries the store lists, time and size included"
    );

    // The file on disk is SPEC, so each road checkpoints it and saves after.
    edit_spec_with_history(&dir, &[rename_edit("renamed")], &history).unwrap();
    fs::write(dir.join(MANIFEST_FILENAME), SPEC).unwrap();
    let step = RecordedStep {
        name: "derived".to_string(),
        sql: "SELECT 1 AS x".to_string(),
        provenance: "history contract test".to_string(),
        description: None,
    };
    record_step_with_history(&dir, &step, &history).unwrap();
    // A restore checkpoints what it replaces, through the same handle; the
    // file changes by hand first, so the state replaced is not the newest
    // entry's and is not skipped as a duplicate.
    fs::write(dir.join(MANIFEST_FILENAME), "name: by-hand\n").unwrap();
    history.restore(&dir, &saved.id).unwrap();

    let kinds: Vec<HistoryKind> = history
        .entries(&dir)
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    use HistoryKind::{Checkpoint, Save};
    assert_eq!(
        kinds,
        [
            Save, Checkpoint, Checkpoint, Save, Checkpoint, Save, Checkpoint
        ],
        "a save, a checkpoint, two for each road and the restore's checkpoint"
    );
    for entry in history.entries(&dir).unwrap() {
        assert_eq!(entry.way, Some(app.clone()), "{entry:?} in `entries`");
    }
    let listed = listed_ways(&history, &dir);
    assert_eq!(listed.len(), 7, "{listed:?}");
    for (id, way) in &listed {
        assert_eq!(way, "app", "`arc history list` prints the way for {id}");
    }

    // The calls on a file carry the handle's way too.
    let chart = tmp.path().join("protocol").join("chart.yaml");
    let on_file = history
        .record_save_for_file(&chart, "mark: dot\n")
        .unwrap()
        .unwrap();
    assert_eq!(on_file.way, Some(app.clone()));
    let on_file = history
        .record_checkpoint_for_file(&chart, "mark: bar\n")
        .unwrap()
        .unwrap();
    assert_eq!(on_file.way, Some(app.clone()));
    for entry in history.entries_for_file(&chart).unwrap() {
        assert_eq!(
            entry.way,
            Some(app.clone()),
            "{entry:?} in `entries_for_file`"
        );
    }
}

// A handle that names no way writes entries that name none, and the list says
// so rather than printing a way nobody recorded.
#[test]
fn a_handle_that_names_no_way_records_entries_that_say_so() {
    let (_tmp, dir, history) = setup();
    let saved = history.record_save(&dir, SPEC).unwrap().unwrap();
    assert_eq!(saved.way, None);
    assert_eq!(history.entries(&dir).unwrap()[0].way, None);
    assert_eq!(
        listed_ways(&history, &dir),
        [(saved.id, "not recorded".to_string())]
    );
}

// A word that is not one arc can keep — empty, too long, or holding anything
// but an ASCII letter, a digit, `-` or `_` — cannot be made into a way, so a
// caller's recording stops before it starts: the file and the history are as
// they were, and the refusal names the word and why.
#[test]
fn a_way_that_cannot_be_stored_is_refused_before_anything_is_written() {
    let (tmp, dir, history) = setup();
    history.record_save(&dir, SPEC).unwrap();
    let entries_before = history.entries(&dir).unwrap();
    let store_before = files_under(history.root());
    let too_long = "a".repeat(33);

    let refused: [(&str, &str); 11] = [
        ("", "empty"),
        (" ", "' ' is not an ASCII letter"),
        ("my app", "' ' is not an ASCII letter"),
        ("a/b", "'/' is not an ASCII letter"),
        ("../app", "'.' is not an ASCII letter"),
        ("a.b", "'.' is not an ASCII letter"),
        ("app\n", "'\\n' is not an ASCII letter"),
        ("caf\u{e9}", "'\u{e9}' is not an ASCII letter"),
        ("a:b", "':' is not an ASCII letter"),
        ("app*", "'*' is not an ASCII letter"),
        (too_long.as_str(), "at most 32 characters"),
    ];
    for (word, why) in refused {
        let attempt = || -> arc::spec::Result<()> {
            let way = HistoryWay::new(word)?;
            edit_spec_with_history(
                &dir,
                &[rename_edit("renamed")],
                &history.clone().reached_by(way),
            )?;
            Ok(())
        };
        match attempt().unwrap_err() {
            Error::HistoryWay { way, detail } => {
                assert_eq!(way, word, "the refusal names the word it was given");
                assert!(detail.contains(why), "{word:?}: {detail:?} names {why:?}");
            }
            other => panic!("{word:?} was refused as something else: {other}"),
        }
        assert_eq!(
            fs::read_to_string(dir.join(MANIFEST_FILENAME)).unwrap(),
            SPEC,
            "{word:?} changed the file"
        );
        assert_eq!(history.entries(&dir).unwrap(), entries_before, "{word:?}");
        assert_eq!(files_under(history.root()), store_before, "{word:?}");
    }
    assert_eq!(
        files_under(&tmp.path().join("protocol")),
        [MANIFEST_FILENAME]
    );
}

// Every word of ASCII letters, digits, `-` and `_`, one to 32 of them, is a
// way, and it is kept as it was given.
#[test]
fn a_word_of_letters_digits_hyphens_and_underscores_is_a_way() {
    let longest = "b".repeat(32);
    for word in ["app", "A", "7", "-", "_", "my-App_2", longest.as_str()] {
        let way = HistoryWay::new(word).unwrap_or_else(|e| panic!("{word:?}: {e}"));
        assert_eq!(way.as_str(), word);
    }
}

// `terminal` and `mcp` are arc's. A caller spelling either, in any case, is
// refused and told which value means it; the value itself records that way.
#[test]
fn arcs_own_ways_are_a_callers_only_by_the_value_that_means_them() {
    assert_eq!(HistoryWay::TERMINAL.as_str(), "terminal");
    assert_eq!(HistoryWay::MCP.as_str(), "mcp");
    for (word, value) in [
        ("terminal", "HistoryWay::TERMINAL"),
        ("Terminal", "HistoryWay::TERMINAL"),
        ("TERMINAL", "HistoryWay::TERMINAL"),
        ("mcp", "HistoryWay::MCP"),
        ("MCP", "HistoryWay::MCP"),
        ("Mcp", "HistoryWay::MCP"),
    ] {
        match HistoryWay::new(word).unwrap_err() {
            Error::HistoryWay { way, detail } => {
                assert_eq!(way, word);
                assert!(detail.contains(value), "{word:?}: {detail:?} names {value}");
                assert!(detail.contains("arc's own way"), "{detail:?}");
            }
            other => panic!("{word:?} was refused as something else: {other}"),
        }
    }

    let (_tmp, dir, history) = setup();
    let history = history.reached_by(HistoryWay::MCP);
    let entry = history.record_save(&dir, SPEC).unwrap().unwrap();
    assert_eq!(entry.way, Some(HistoryWay::MCP));
    assert_eq!(listed_ways(&history, &dir), [(entry.id, "mcp".to_string())]);
}
