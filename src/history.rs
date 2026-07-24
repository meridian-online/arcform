//! Local history for `arcform.yaml`: the middle tier between editor undo and
//! version control, and the checkpoint a machine edit takes before it lands.
//!
//! # The three tiers
//!
//! A hand-authored document accumulates three kinds of history, and mature
//! editors keep them strictly apart. **Undo** lives in memory, scoped to a
//! session, gone when it ends. **Local history** — this module — lives on
//! disk *outside* the project, written automatically at save boundaries,
//! debounced and bounded. **Version control** is explicit, human-triggered,
//! in-project and shared. The only automatic promotion between tiers is the
//! first: a state crosses from undo into local history when it is saved.
//! **Nothing is ever promoted to version control automatically** — this
//! module never runs git, never stages, never commits, never even looks for
//! a repository. A protocol that has never seen git gets the same safety net
//! as one that lives in it, and rolling a spec back requires no repository
//! and no account of any kind.
//!
//! # Machine edits want a checkpoint, not a save entry
//!
//! When a person saves, the state worth keeping is the one they just made.
//! When a *machine* rewrites the spec — a tool acting on an interaction, an
//! agent acting on an instruction — the state worth keeping is the one about
//! to be **replaced**. The two records therefore differ in kind and in
//! timing: a [`HistoryKind::Save`] entry is the after-image recorded at a
//! save boundary, and a [`HistoryKind::Checkpoint`] entry is the before-image
//! recorded as part of a machine write, before the first byte changes on
//! disk. The checkpointed roads — [`edit_spec_with_history`] and
//! [`record_step_with_history`] — treat the snapshot as load-bearing:
//! **no checkpoint, no write.** That ordering is what makes accepting a
//! machine's edit safe: whatever it replaces is already recoverable.
//!
//! The bare roads ([`edit_spec`](crate::edit::edit_spec),
//! [`record_step`](crate::record::record_step)) remain history-free for
//! callers that bring their own net; a tool surface should prefer the
//! checkpointed roads.
//!
//! # Where the store lives
//!
//! `$ARCFORM_HISTORY_DIR` when set, else `~/.arcform/history` — never inside
//! the protocol directory. Both mature precedents for this tier put it in
//! user data, and the reasons hold here: entries stay out of `git status`,
//! out of diffs, out of archives, out of anything shared. Recording an entry
//! changes nothing in the protocol directory.
//!
//! Inside the root, each spec gets a directory keyed by a hash of its
//! canonical path, holding a `spec-path` file (the path in the clear, for a
//! human inspecting the store) and one snapshot file per entry, named
//! `<millis>-<seq>-<kind>.yaml`. Entries are whole snapshots, not deltas: a
//! spec is small, and a restore that needs no reconstruction is a restore
//! that cannot compound errors.
//!
//! **The snapshot is the spec file, and only the spec file.** A protocol's
//! generated SQL under `models/` is not snapshotted here, and that is a
//! recorded non-goal rather than a missing feature: a generated model is a
//! derivative of the manifest step that names it, machine-authored under the
//! marker that licenses its regeneration (see
//! [`amend_step_sql`](crate::record::amend_step_sql)), so the manifest — the
//! authored artifact — is what the net keeps. Versioning the whole working
//! tree is the third tier's job, not this one's.
//!
//! # Retention policy
//!
//! - At most [`HISTORY_MAX_ENTRIES`] entries per spec; the oldest are pruned
//!   first.
//! - A save recorded through [`LocalHistory::record_save`] within
//!   [`HISTORY_MERGE_WINDOW`] of the newest entry — when that entry is
//!   itself a save — **merges into it**: the newer state replaces the older
//!   entry, so rapid saves debounce to one entry rather than flooding the
//!   bound. The merge is for bursts from the *same source*: checkpoints
//!   never merge, and the checkpointed roads record their after-images with
//!   the merge disabled — a machine edit must never fold away the state it
//!   just promised was recoverable.
//! - A state identical to the newest entry is not recorded again, whatever
//!   its kind.
//!
//! The same policy is printed by `arc history list`, so it lives where a
//! user managing entries will actually meet it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::edit::{SpecEdit, ValidatedSpec, apply_edits, write_atomic};
use crate::error::{Error, Result};
use crate::manifest::MANIFEST_FILENAME;
use crate::record::RecordedStep;

/// The most entries kept per spec; recording past the bound prunes the
/// oldest. Fifty matches the established local-history precedent and holds a
/// few weeks of real editing.
pub const HISTORY_MAX_ENTRIES: usize = 50;

/// Saves arriving within this window of the newest save merge into it —
/// the debounce that keeps a burst of rapid saves from flooding the bound.
pub const HISTORY_MERGE_WINDOW: Duration = Duration::from_secs(10);

/// Overrides the store root; without it the root is `~/.arcform/history`.
const HISTORY_DIR_ENV: &str = "ARCFORM_HISTORY_DIR";

/// The file inside each per-spec directory naming the spec it snapshots.
const SPEC_PATH_FILE: &str = "spec-path";

// ------------------------------------------------------------------ the values

/// What an entry records: the after-image of a save, or the before-image a
/// machine edit checkpoints. The kinds are related by presentation — one
/// timeline, distinguishable entries — never by different stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    /// The state as it was saved — recorded *after* the save boundary.
    Save,
    /// The state a machine edit was about to replace — recorded *before*
    /// the write landed.
    Checkpoint,
}

impl HistoryKind {
    fn tag(self) -> &'static str {
        match self {
            HistoryKind::Save => "save",
            HistoryKind::Checkpoint => "checkpoint",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "save" => Some(HistoryKind::Save),
            "checkpoint" => Some(HistoryKind::Checkpoint),
            _ => None,
        }
    }
}

/// One recorded state of a spec. Plain data: the id addresses the entry in
/// [`LocalHistory::read`] and [`LocalHistory::restore`], and doubles as the
/// snapshot's file stem for anyone inspecting the store directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// `<millis>-<seq>-<kind>` — sortable, and stable once recorded.
    pub id: String,
    /// Save entry or machine-edit checkpoint.
    pub kind: HistoryKind,
    /// When the entry was recorded.
    pub at: SystemTime,
    /// The snapshot's size in bytes.
    pub bytes: u64,
}

/// A handle on the local-history store: a root directory and nothing else.
/// [`LocalHistory::open_default`] resolves the conventional root; a tool
/// embedding the store (or a test) can put it anywhere with
/// [`LocalHistory::at_root`].
#[derive(Debug, Clone)]
pub struct LocalHistory {
    root: PathBuf,
}

impl LocalHistory {
    /// The store at the conventional root: `$ARCFORM_HISTORY_DIR` when set,
    /// else `~/.arcform/history`. Refused with [`Error::HistoryRootMissing`]
    /// when neither resolves — the message names the env var, because that is
    /// the remedy.
    pub fn open_default() -> Result<Self> {
        resolve_root(std::env::var_os(HISTORY_DIR_ENV), dirs::home_dir()).map(Self::at_root)
    }

    /// The store rooted at `root`, created lazily on first record.
    pub fn at_root(root: impl Into<PathBuf>) -> Self {
        LocalHistory { root: root.into() }
    }

    /// Where this store keeps its entries.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Record `text` as a save entry for the spec in `dir` — the automatic
    /// promotion from undo to local history at the save boundary, and the
    /// only automatic promotion there is.
    ///
    /// Debounced and deduplicated per the module policy: returns
    /// `Ok(Some(entry))` for a recorded (or merged) entry, `Ok(None)` when
    /// the state was already the newest entry and nothing needed recording.
    pub fn record_save(&self, dir: &Path, text: &str) -> Result<Option<HistoryEntry>> {
        self.record(dir, text, HistoryKind::Save, SystemTime::now(), true)
    }

    /// Record `text` — the bytes a machine edit is about to replace — as a
    /// checkpoint entry for the spec in `dir`. Distinct in kind from a save
    /// entry, never merged; only an exact duplicate of the newest entry is
    /// skipped (`Ok(None)`).
    pub fn record_checkpoint(&self, dir: &Path, text: &str) -> Result<Option<HistoryEntry>> {
        self.record(dir, text, HistoryKind::Checkpoint, SystemTime::now(), false)
    }

    /// Every entry recorded for the spec in `dir`, oldest first. A spec with
    /// no history yet has an empty list, not an error.
    pub fn entries(&self, dir: &Path) -> Result<Vec<HistoryEntry>> {
        let (key_dir, _) = self.key_dir(dir)?;
        entries_in(&key_dir)
    }

    /// The exact bytes entry `id` recorded for the spec in `dir`.
    pub fn read(&self, dir: &Path, id: &str) -> Result<String> {
        let (key_dir, _) = self.key_dir(dir)?;
        read_entry(&key_dir, id)
    }

    /// Roll the spec in `dir` back to the state entry `id` recorded, and
    /// return that state's bytes.
    ///
    /// A restore is itself a machine-initiated write, so it follows the same
    /// discipline: the state being replaced is checkpointed **first** — no
    /// checkpoint, no write — which makes every rollback itself reversible.
    /// The recorded bytes then land through the same atomic write as every
    /// other spec write.
    ///
    /// Recovery is byte-faithful and deliberately **not** gated through the
    /// loader: a safety net must be able to reach every state it recorded,
    /// including one that no longer loads. A surface offering restore should
    /// tell the user when the restored state does not load — `arc history
    /// restore` does.
    pub fn restore(&self, dir: &Path, id: &str) -> Result<String> {
        let text = self.read(dir, id)?;
        let manifest_path = dir.join(MANIFEST_FILENAME);
        if manifest_path.exists() {
            let current = std::fs::read_to_string(&manifest_path).map_err(|e| Error::FileRead {
                path: manifest_path.clone(),
                source: e,
            })?;
            self.record_checkpoint(dir, &current)?;
        }
        write_atomic(&manifest_path, text.as_bytes())?;
        Ok(text)
    }

    // ---------------------------------------------------------------- internals

    /// Record one entry. `now` is a parameter so the debounce window and
    /// ordering rules are testable without a clock; `merge` engages the save
    /// debounce, and is true only for the save-boundary entry point — see
    /// the retention-policy discussion in the module docs.
    fn record(
        &self,
        dir: &Path,
        text: &str,
        kind: HistoryKind,
        now: SystemTime,
        merge: bool,
    ) -> Result<Option<HistoryEntry>> {
        let (key_dir, spec_path) = self.key_dir(dir)?;
        std::fs::create_dir_all(&key_dir)?;

        // Name the spec in the clear for anyone inspecting the store.
        let marker = key_dir.join(SPEC_PATH_FILE);
        if !marker.exists() {
            write_atomic(&marker, format!("{}\n", spec_path.display()).as_bytes())?;
        }

        let existing = entries_in(&key_dir)?;
        let newest = existing.last();

        // The newest entry already records exactly this state: nothing new
        // to keep, whatever the kind.
        if let Some(newest) = newest
            && read_entry(&key_dir, &newest.id)? == text
        {
            return Ok(None);
        }

        // The debounce: a save hard on the heels of the newest save merges
        // into it — the newer state replaces the older entry. Checkpoints
        // never merge, and a save never merges across an intervening
        // checkpoint (the newest entry would not be a save).
        let merge_into = newest
            .filter(|n| {
                merge
                    && kind == HistoryKind::Save
                    && n.kind == HistoryKind::Save
                    && now
                        .duration_since(n.at)
                        .is_ok_and(|gap| gap <= HISTORY_MERGE_WINDOW)
            })
            .map(|n| n.id.clone());

        let now_millis = now
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let (millis, seq) = next_stamp(newest, now_millis);
        let id = entry_id(millis, seq, kind);

        // Write the new entry before removing anything, so no moment holds
        // fewer recorded states than before.
        write_atomic(&entry_path(&key_dir, &id), text.as_bytes())?;
        if let Some(old) = merge_into {
            let _ = std::fs::remove_file(entry_path(&key_dir, &old));
        }
        prune(&key_dir)?;

        Ok(Some(HistoryEntry {
            id,
            kind,
            at: UNIX_EPOCH + Duration::from_millis(millis),
            bytes: text.len() as u64,
        }))
    }

    /// The per-spec directory: the store root plus a hash of the canonical
    /// spec path. Hashing makes the key filesystem-safe for any path;
    /// `spec-path` inside the directory keeps it human-legible.
    fn key_dir(&self, dir: &Path) -> Result<(PathBuf, PathBuf)> {
        let canonical = dir.canonicalize().map_err(|e| Error::FileRead {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let spec_path = canonical.join(MANIFEST_FILENAME);
        let digest = Sha256::digest(spec_path.as_os_str().as_encoded_bytes());
        let mut key = String::with_capacity(16);
        for byte in &digest[..8] {
            key.push_str(&format!("{byte:02x}"));
        }
        Ok((self.root.join(key), spec_path))
    }
}

// ------------------------------------------------------- the checkpointed roads

/// The whole spec write path with the middle history tier engaged: apply
/// `edits` to `<dir>/arcform.yaml`, gate the result through the loader,
/// checkpoint the bytes being replaced, write atomically, and record the
/// after-image as a save entry.
///
/// The order is the contract. A refusal — an edit that does not apply, or a
/// result that will not load — happens **before** anything is written *or
/// recorded*: the file and the history are both untouched. Once the write is
/// committed to, the checkpoint of the original bytes is load-bearing: if it
/// cannot be recorded, the write is refused and the spec on disk stays
/// byte-identical. The trailing save entry is a courtesy, not a guarantee —
/// its state is also the file now on disk, so a failure to record it loses
/// nothing and is deliberately not surfaced.
pub fn edit_spec_with_history(
    dir: &Path,
    edits: &[SpecEdit],
    history: &LocalHistory,
) -> Result<ValidatedSpec> {
    let path = dir.join(MANIFEST_FILENAME);
    if !path.exists() {
        return Err(Error::ManifestNotFound);
    }
    let original = std::fs::read_to_string(&path).map_err(|e| Error::FileRead {
        path: path.clone(),
        source: e,
    })?;
    let validated = apply_edits(&original, edits)?;
    history.record_checkpoint(dir, &original)?;
    validated.write_to(dir)?;
    // Merge disabled: when the checkpoint deduplicated against a save entry
    // recorded moments ago, a merging save here would replace that entry —
    // folding away the state this write just promised was recoverable.
    let _ = history.record(
        dir,
        validated.text(),
        HistoryKind::Save,
        SystemTime::now(),
        false,
    );
    Ok(validated)
}

/// [`record_step`](crate::record::record_step) with the middle history tier
/// engaged: the manifest as it stands is checkpointed **before** the
/// promotion writes anything — no checkpoint, no write — and the promoted
/// manifest is recorded as a save entry afterwards (best-effort, exactly as
/// in [`edit_spec_with_history`]).
///
/// A promotion the record path refuses leaves the protocol untouched as
/// ever; the checkpoint of the untouched state may remain, which is
/// harmless — it records bytes that are still on disk, and a repeat attempt
/// will not record them twice.
pub fn record_step_with_history(
    dir: &Path,
    step: &RecordedStep,
    history: &LocalHistory,
) -> Result<(PathBuf, ValidatedSpec)> {
    let path = dir.join(MANIFEST_FILENAME);
    if !path.exists() {
        return Err(Error::ManifestNotFound);
    }
    let original = std::fs::read_to_string(&path).map_err(|e| Error::FileRead {
        path: path.clone(),
        source: e,
    })?;
    history.record_checkpoint(dir, &original)?;
    let (sql_rel, validated) = crate::record::record_step(dir, step)?;
    // Merge disabled for the same reason as in `edit_spec_with_history`.
    let _ = history.record(
        dir,
        validated.text(),
        HistoryKind::Save,
        SystemTime::now(),
        false,
    );
    Ok((sql_rel, validated))
}

// ------------------------------------------------------------------ internals

/// The retention policy in one line, for the surfaces that print it beside
/// the entries it governs.
pub(crate) fn policy_line(root: &Path) -> String {
    format!(
        "policy: keeps the last {HISTORY_MAX_ENTRIES} states per spec (oldest pruned first); \
         saves within {}s of the newest save merge into it; stored outside the protocol \
         at {}; never promoted to git",
        HISTORY_MERGE_WINDOW.as_secs(),
        root.display()
    )
}

/// `$ARCFORM_HISTORY_DIR` when set and non-empty, else `~/.arcform/history`.
fn resolve_root(env: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(dir) = env
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    home.map(|home| home.join(".arcform").join("history"))
        .ok_or(Error::HistoryRootMissing)
}

/// `<millis>-<seq>-<kind>`, fixed-width so the id orders the same way the
/// timestamp does.
fn entry_id(millis: u64, seq: u32, kind: HistoryKind) -> String {
    format!("{millis:013}-{seq:03}-{}", kind.tag())
}

/// The reverse of [`entry_id`]. Anything that does not parse is not an entry
/// id — which also keeps a path-shaped "id" from ever reaching the
/// filesystem.
fn parse_id(id: &str) -> Option<(u64, u32, HistoryKind)> {
    let mut parts = id.splitn(3, '-');
    let millis = parts.next()?.parse().ok()?;
    let seq = parts.next()?.parse().ok()?;
    let kind = HistoryKind::from_tag(parts.next()?)?;
    Some((millis, seq, kind))
}

fn entry_path(key_dir: &Path, id: &str) -> PathBuf {
    key_dir.join(format!("{id}.yaml"))
}

/// The next `(millis, seq)` stamp: strictly after the newest entry even when
/// the clock stands still or steps backwards, so recorded order and id order
/// never disagree.
fn next_stamp(newest: Option<&HistoryEntry>, now_millis: u64) -> (u64, u32) {
    match newest.and_then(|n| parse_id(&n.id)) {
        Some((millis, seq, _)) if now_millis <= millis => (millis, seq + 1),
        _ => (now_millis, 0),
    }
}

/// Every entry under `key_dir`, oldest first. A missing directory is an
/// empty history; a file that does not parse as an entry is not one.
fn entries_in(key_dir: &Path) -> Result<Vec<HistoryEntry>> {
    let read = match std::fs::read_dir(key_dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut found = Vec::new();
    for entry in read {
        let entry = entry?;
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".yaml")) else {
            continue;
        };
        let Some((millis, seq, kind)) = parse_id(stem) else {
            continue;
        };
        found.push((
            millis,
            seq,
            HistoryEntry {
                id: stem.to_string(),
                kind,
                at: UNIX_EPOCH + Duration::from_millis(millis),
                bytes: entry.metadata()?.len(),
            },
        ));
    }
    found.sort_by_key(|(millis, seq, _)| (*millis, *seq));
    Ok(found.into_iter().map(|(_, _, entry)| entry).collect())
}

fn read_entry(key_dir: &Path, id: &str) -> Result<String> {
    if parse_id(id).is_none() {
        return Err(Error::HistoryEntryNotFound { id: id.to_string() });
    }
    match std::fs::read_to_string(entry_path(key_dir, id)) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(Error::HistoryEntryNotFound { id: id.to_string() })
        }
        Err(e) => Err(e.into()),
    }
}

/// Enforce [`HISTORY_MAX_ENTRIES`]: remove the oldest entries beyond the
/// bound.
fn prune(key_dir: &Path) -> Result<()> {
    let entries = entries_in(key_dir)?;
    if entries.len() > HISTORY_MAX_ENTRIES {
        for entry in &entries[..entries.len() - HISTORY_MAX_ENTRIES] {
            let _ = std::fs::remove_file(entry_path(key_dir, &entry.id));
        }
    }
    Ok(())
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, LocalHistory) {
        let tmp = tempfile::tempdir().unwrap();
        let history = LocalHistory::at_root(tmp.path().join("history"));
        (tmp, history)
    }

    fn protocol(tmp: &tempfile::TempDir) -> PathBuf {
        let dir = tmp.path().join("protocol");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST_FILENAME), "name: fixture\n").unwrap();
        dir
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    const T0: u64 = 1_700_000_000;

    #[test]
    fn saves_within_the_window_merge_and_the_newer_state_wins() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        history
            .record(&dir, "a\n", HistoryKind::Save, at(T0), true)
            .unwrap();
        history
            .record(&dir, "b\n", HistoryKind::Save, at(T0 + 5), true)
            .unwrap();
        let entries = history.entries(&dir).unwrap();
        assert_eq!(entries.len(), 1, "rapid saves debounce to one entry");
        assert_eq!(history.read(&dir, &entries[0].id).unwrap(), "b\n");
        assert_eq!(entries[0].at, at(T0 + 5), "the merged entry is the newer");
    }

    #[test]
    fn saves_outside_the_window_stay_separate() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        history
            .record(&dir, "a\n", HistoryKind::Save, at(T0), true)
            .unwrap();
        history
            .record(&dir, "b\n", HistoryKind::Save, at(T0 + 11), true)
            .unwrap();
        assert_eq!(history.entries(&dir).unwrap().len(), 2);
    }

    #[test]
    fn checkpoints_never_merge_and_block_a_save_merge_across_them() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        history
            .record(&dir, "a\n", HistoryKind::Checkpoint, at(T0), false)
            .unwrap();
        history
            .record(&dir, "b\n", HistoryKind::Checkpoint, at(T0 + 1), false)
            .unwrap();
        assert_eq!(
            history.entries(&dir).unwrap().len(),
            2,
            "checkpoints are each kept"
        );

        // A save right after a checkpoint records fresh — the checkpoint is
        // not merged into.
        history
            .record(&dir, "c\n", HistoryKind::Save, at(T0 + 2), true)
            .unwrap();
        assert_eq!(history.entries(&dir).unwrap().len(), 3);
    }

    #[test]
    fn a_state_identical_to_the_newest_entry_is_not_recorded_again() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        let first = history
            .record(&dir, "a\n", HistoryKind::Save, at(T0), true)
            .unwrap();
        assert!(first.is_some());
        let repeat = history
            .record(&dir, "a\n", HistoryKind::Checkpoint, at(T0 + 60), false)
            .unwrap();
        assert!(repeat.is_none(), "kind does not matter to the dedupe");
        assert_eq!(history.entries(&dir).unwrap().len(), 1);
    }

    #[test]
    fn the_bound_prunes_the_oldest_entries() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        for n in 0..HISTORY_MAX_ENTRIES + 5 {
            history
                .record(
                    &dir,
                    &format!("state {n}\n"),
                    HistoryKind::Save,
                    at(T0 + (n as u64) * 60),
                    true,
                )
                .unwrap();
        }
        let entries = history.entries(&dir).unwrap();
        assert_eq!(entries.len(), HISTORY_MAX_ENTRIES);
        assert_eq!(
            history.read(&dir, &entries[0].id).unwrap(),
            "state 5\n",
            "the oldest five were pruned"
        );
        assert_eq!(
            history.read(&dir, &entries.last().unwrap().id).unwrap(),
            format!("state {}\n", HISTORY_MAX_ENTRIES + 4)
        );
    }

    #[test]
    fn a_stalled_or_backwards_clock_never_reorders_entries() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        history
            .record(&dir, "a\n", HistoryKind::Checkpoint, at(T0), false)
            .unwrap();
        // Same instant, then five seconds earlier: both must land after.
        history
            .record(&dir, "b\n", HistoryKind::Checkpoint, at(T0), false)
            .unwrap();
        history
            .record(&dir, "c\n", HistoryKind::Checkpoint, at(T0 - 5), false)
            .unwrap();
        let entries = history.entries(&dir).unwrap();
        let texts: Vec<String> = entries
            .iter()
            .map(|e| history.read(&dir, &e.id).unwrap())
            .collect();
        assert_eq!(texts, ["a\n", "b\n", "c\n"]);
        let mut ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        let sorted = ids.clone();
        ids.sort_unstable();
        assert_eq!(ids, sorted, "id order and recorded order agree");
    }

    #[test]
    fn a_machine_save_never_folds_away_the_state_it_replaced() {
        // The trap: a save entry records state A; a machine edit follows
        // within the merge window. Its checkpoint of A deduplicates against
        // the save — so if its after-image save were allowed to merge, the
        // only record of A would be replaced by B and the before-image lost.
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        history
            .record(&dir, "a\n", HistoryKind::Save, at(T0), true)
            .unwrap();
        let deduped = history
            .record(&dir, "a\n", HistoryKind::Checkpoint, at(T0 + 2), false)
            .unwrap();
        assert!(
            deduped.is_none(),
            "the checkpoint deduplicates against the save"
        );
        history
            .record(&dir, "b\n", HistoryKind::Save, at(T0 + 2), false)
            .unwrap();
        let entries = history.entries(&dir).unwrap();
        let texts: Vec<String> = entries
            .iter()
            .map(|e| history.read(&dir, &e.id).unwrap())
            .collect();
        assert_eq!(texts, ["a\n", "b\n"], "the replaced state must survive");
    }

    #[test]
    fn the_root_resolves_env_first_then_home_then_refuses() {
        assert_eq!(
            resolve_root(Some("/elsewhere".into()), Some(PathBuf::from("/home/x"))).unwrap(),
            PathBuf::from("/elsewhere")
        );
        assert_eq!(
            resolve_root(None, Some(PathBuf::from("/home/x"))).unwrap(),
            PathBuf::from("/home/x/.arcform/history")
        );
        assert_eq!(
            resolve_root(Some("".into()), Some(PathBuf::from("/home/x"))).unwrap(),
            PathBuf::from("/home/x/.arcform/history"),
            "an empty env var is not a root"
        );
        assert!(matches!(
            resolve_root(None, None),
            Err(Error::HistoryRootMissing)
        ));
    }

    #[test]
    fn entry_ids_round_trip_and_refuse_path_shapes() {
        let id = entry_id(1_700_000_000_123, 7, HistoryKind::Checkpoint);
        assert_eq!(id, "1700000000123-007-checkpoint");
        assert_eq!(
            parse_id(&id),
            Some((1_700_000_000_123, 7, HistoryKind::Checkpoint))
        );
        for bad in ["", "x", "123-000", "123-000-commit", "../123-000-save"] {
            assert_eq!(parse_id(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn the_policy_line_states_the_bound_the_window_and_the_git_stance() {
        let line = policy_line(Path::new("/somewhere"));
        assert!(line.contains(&HISTORY_MAX_ENTRIES.to_string()));
        assert!(line.contains(&format!("{}s", HISTORY_MERGE_WINDOW.as_secs())));
        assert!(line.contains("never promoted to git"));
        assert!(line.contains("/somewhere"));
    }
}
