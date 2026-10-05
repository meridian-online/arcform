//! Local history for `arcform.yaml` and the files beside it: the middle tier
//! between editor undo and version control, and the checkpoint a machine edit
//! takes before it lands.
//!
//! # The three tiers
//!
//! A hand-authored document accumulates three kinds of history, and mature
//! editors keep them strictly apart. **Undo** lives in memory, scoped to a
//! session, gone when it ends. **Local history** — this module — keeps its
//! snapshots on disk *outside* the project, written automatically at save
//! boundaries, debounced and bounded, and names each version in one line of a
//! log inside the project's folder. **Version control** is explicit, human-triggered,
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
//! # A text never written to its file
//!
//! A save and a checkpoint each record a text that was on disk: the one just
//! written, or the one about to be replaced. A caller can also hold a text
//! that never reached its file — an editor's buffer that was never saved —
//! and that text has nowhere on disk to be recovered from. A
//! [`HistoryKind::Unsaved`] entry records it, through
//! [`LocalHistory::record_unsaved_for_file`], keyed to the file it was meant
//! for. Recording it writes nothing to that file and does not create it
//! when it does not exist: the store keeps the text, the folder's log names
//! the entry, and the file stays as it was.
//! Restoring the entry writes the text to the file, with every restore's
//! discipline. Why the caller holds such a text is the caller's affair.
//!
//! # Where the store lives
//!
//! The snapshots live at `$ARCFORM_HISTORY_DIR` when set, else
//! `~/.arcform/history` — never inside the protocol directory. Both mature
//! precedents for this tier put them in user data, and the reasons hold here:
//! the snapshots stay out of `git status`, out of diffs, out of archives, out
//! of anything shared. Recording an entry writes nothing of a snapshot in the
//! protocol directory; it appends one line to the folder's log, which the
//! next section describes.
//!
//! Inside the root, each file gets a directory keyed by a hash of its
//! canonical path, holding a `spec-path` file (the path in the clear, for a
//! human inspecting the store) and one snapshot file per entry, named
//! `<millis>-<seq>-<kind>.<way>.yaml` — or `<millis>-<seq>-<kind>.yaml` for an
//! entry that names no way, which is every entry an arc before ways wrote.
//! The entry's id is the name up to its first `.` either way. Entries are
//! whole snapshots, not deltas: a spec is small, and a restore that needs no
//! reconstruction is a restore that cannot compound errors.
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
//! # The log in the protocol's folder
//!
//! Every entry the store records — a save, a checkpoint or an unsaved text,
//! of the spec or of a file beside it — appends one line to [`LOG_FILENAME`]
//! in the protocol's folder: the directory that holds the spec, or for a file
//! not named `arcform.yaml` the nearest directory at or above it that holds
//! one. One folder has one log, and each line names the file it is about. The
//! log is the provenance that travels with the folder, through git, a drive
//! or an archive; the snapshots are the safety net that stays on the machine.
//!
//! A line is one JSON object, so a person reads it and any JSON reader parses
//! it, the log as a whole being newline-delimited JSON:
//!
//! ```text
//! {"at":"2026-10-03T07:15:02.123Z","file":"arcform.yaml","kind":"save","interface":"terminal","version":"1791011702123-000-save","step":"big_orders","change":"added"}
//! ```
//!
//! — `at`, the time the version was recorded (RFC 3339, UTC, to the
//! millisecond); `file`, the file's path inside the folder, with `/` between
//! its parts; `kind`, `save`, `checkpoint` or `unsaved`; `interface`, the way
//! arc was reached, left out for a handle that names none; `version`, the id
//! `arc history list` prints; and the steps the version touched, as `step`
//! with `change` (`added`, `removed` or `changed`) when it touched one, or as
//! `steps`, a list of objects each holding a `step` and its `change`, when it
//! touched several. A line holds no contents of any file, so its length does
//! not grow with the file's. The README names the same keys.
//!
//! The steps come from two places. A verb that adds a step names it: the line
//! of [`record_step_with_history`]'s save says `added`. A plain save —
//! [`LocalHistory::record_save`] and [`LocalHistory::record_save_for_file`],
//! which take the whole text and are told nothing — is compared with the state
//! the store held before it, the newest entry whatever its kind, and the line
//! names each step the text added, removed or changed. The comparison reads
//! both texts as YAML and matches steps by `name`: a step is changed when what
//! it parses to differs, so a comment or the order of its keys is no change; a
//! renamed step is one removed and one added; and the steps are named in the
//! order they stand in the saved text, those removed after them. It is made
//! for the spec alone. A line names the file and no step when the file is not
//! `arcform.yaml`, when the store held no state to compare with, when either
//! text does not read as a sequence of steps with
//! one name each, and when no step differs — a key at the manifest's top, a
//! comment, the order of the steps. A checkpoint, an unsaved text and the save
//! after an edit name no step this way, and the comparison names no more than
//! the step: what changed inside it is in the snapshots.
//!
//! A line is appended in one write, so a reader never sees half of one, and
//! a log whose last line lacks its newline gains one first. The line is a
//! courtesy, as the save after a checkpointed write is: a folder arc cannot
//! write to, or a file in no protocol's folder, gets no line, and the version
//! is recorded all the same. A version the store does not record — a state
//! identical to the newest entry — gets no line either. A checkpoint a write
//! takes gets its line when that write lands, ahead of the save's: a write
//! refused after its checkpoint leaves the folder as it was, and the
//! checkpoint in the store alone. A line outlives its
//! snapshot: pruning and the save debounce remove snapshots, never lines.
//!
//! # The way arc was reached
//!
//! Every entry records, beside its kind and its time, the way arc was reached
//! when it was written — a [`HistoryWay`]: [`HistoryWay::TERMINAL`] for a
//! version the `arc` command line writes, [`HistoryWay::MCP`] for one a tool
//! of `arc mcp` writes, or a word a library caller names for itself with
//! [`HistoryWay::new`]. The way is the store handle's, set once where a
//! process opens the store ([`LocalHistory::reached_by`]), so a checkpoint
//! and the save after it, and the checkpoint a restore takes, carry the way
//! of the call that wrote them. An entry written by a handle that names no
//! way — and every entry an arc before ways wrote — has none, and says so.
//!
//! The way is kept in the store and in the folder's log, and never in the
//! spec or a step's file: those hold the same bytes whichever way wrote them,
//! because a way written into a step's file would be a changed step that runs
//! again. In the store it is kept in the entry's file name, so the snapshot
//! and its way land in one atomic write and no reader sees one without the
//! other.
//!
//! # A file's own history
//!
//! The calls above take a protocol directory and keep the history of the
//! `arcform.yaml` inside it. A file a tool writes beside the spec — a chart
//! file under `panels/`, say — keeps a history of its own through the twin
//! calls that take a file: [`LocalHistory::record_save_for_file`],
//! [`LocalHistory::record_checkpoint_for_file`],
//! [`LocalHistory::entries_for_file`], [`LocalHistory::read_for_file`] and
//! [`LocalHistory::restore_for_file`]. Each file is keyed by its own
//! canonical path, so two files in one directory never list, restore or prune
//! each other's entries. The
//! spec's key is the same either way: the calls that take a file, given a
//! directory's `arcform.yaml`, read and write the history the calls that take
//! the directory recorded. [`LocalHistory::record_unsaved_for_file`] is a call
//! on a file with no twin that takes a directory; given a directory's
//! `arcform.yaml`, it records into that spec's history.
//!
//! # Retention policy
//!
//! - At most [`HISTORY_MAX_ENTRIES`] entries per file; the oldest are pruned
//!   first, and one file's entries never count against another's.
//! - A save recorded through [`LocalHistory::record_save`] within
//!   [`HISTORY_MERGE_WINDOW`] of the newest entry — when that entry is
//!   itself a save — **merges into it**: the newer state replaces the older
//!   entry, so rapid saves debounce to one entry rather than flooding the
//!   bound. The merge is for bursts from the *same source*: checkpoints
//!   never merge, and the checkpointed roads record their after-images with
//!   the merge disabled — a machine edit must never fold away the state it
//!   just promised was recoverable. An unsaved entry never merges either way:
//!   it is not replaced by a save after it, and it does not replace a save
//!   before it.
//! - A state identical to the newest entry is not recorded again, whatever
//!   its kind.
//!
//! The same policy is printed by `arc history list`, so it lives where a
//! user managing entries will actually meet it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::edit::{SpecEdit, ValidatedSpec, apply_edits, write_atomic};
use crate::error::{Error, Result};
use crate::manifest::MANIFEST_FILENAME;
use crate::record::RecordedStep;

/// The most entries kept per file; recording past the bound prunes that
/// file's oldest. Fifty matches the established local-history precedent and holds a
/// few weeks of real editing.
pub const HISTORY_MAX_ENTRIES: usize = 50;

/// Saves arriving within this window of the newest save merge into it —
/// the debounce that keeps a burst of rapid saves from flooding the bound.
pub const HISTORY_MERGE_WINDOW: Duration = Duration::from_secs(10);

/// The log a protocol's folder holds beside `arcform.yaml`: one line per
/// version the store records of a file in that folder, as the module docs
/// describe. Its name does not end in `.log`, which the ignore lists of many
/// projects name, so `git add` does not pass over it.
pub const LOG_FILENAME: &str = "arcform-log.txt";

/// Overrides the store root; without it the root is `~/.arcform/history`.
const HISTORY_DIR_ENV: &str = "ARCFORM_HISTORY_DIR";

/// The file inside each per-spec directory naming the spec it snapshots.
const SPEC_PATH_FILE: &str = "spec-path";

/// The longest word a way may be. A way is a word, not a path or a sentence,
/// and it is part of every entry's file name.
const WAY_MAX_LEN: usize = 32;

// ------------------------------------------------------------------ the values

/// What an entry records: the after-image of a save, the before-image a
/// machine edit checkpoints, or a text that was never written to its file.
/// The kinds are related by presentation — one timeline, distinguishable
/// entries — never by different stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    /// The state as it was saved — recorded *after* the save boundary.
    Save,
    /// The state a machine edit was about to replace — recorded *before*
    /// the write landed.
    Checkpoint,
    /// A text that was never written to its file — recorded in the store
    /// alone, with the file left as it was.
    Unsaved,
}

impl HistoryKind {
    fn tag(self) -> &'static str {
        match self {
            HistoryKind::Save => "save",
            HistoryKind::Checkpoint => "checkpoint",
            HistoryKind::Unsaved => "unsaved",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "save" => Some(HistoryKind::Save),
            "checkpoint" => Some(HistoryKind::Checkpoint),
            "unsaved" => Some(HistoryKind::Unsaved),
            _ => None,
        }
    }
}

/// What a version did to the protocol's steps, as its line in the folder's
/// log names it.
#[derive(Debug, Clone)]
enum StepNote<'a> {
    /// Nothing that recorded the version named a step: a checkpoint, an edit,
    /// an unsaved text, a file that is not the spec, or a save that changed
    /// no step.
    Unnamed,
    /// A verb that adds a step recorded the version, adding the step of this
    /// name.
    Added(&'a str),
    /// A plain save: [`LocalHistory::record_keyed`] names the steps by
    /// comparing the saved text with the state the store held before it, and
    /// turns this into [`StepNote::Steps`] or [`StepNote::Unnamed`].
    Compare,
    /// The steps a comparison found the save to have added, removed or
    /// changed, never empty.
    Steps(Vec<StepChange>),
}

/// What a save did to one step of the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StepChange {
    step: String,
    change: Change,
}

/// What happened to a step between two states of the spec. A step that was
/// renamed reads as one removed and one added, because nothing in the text
/// says the two are the same step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Added,
    Removed,
    Changed,
}

impl Change {
    fn word(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Removed => "removed",
            Change::Changed => "changed",
        }
    }
}

/// A checkpoint's line in the folder's log, held until the write the
/// checkpoint was taken for has landed.
struct HeldLine {
    /// The spec the checkpoint is of, whose folder's log takes the line.
    spec_path: PathBuf,
    entry: HistoryEntry,
}

impl HeldLine {
    /// Append the line, now that the write has landed. Best-effort, as every
    /// line is.
    fn land(self) {
        let _ = append_log_line(&self.spec_path, &self.entry, StepNote::Unnamed);
    }
}

/// The way arc was reached when an entry was written: [`TERMINAL`] for the
/// `arc` command line, [`MCP`] for `arc mcp`, or a caller's own word named
/// with [`new`] — `app`, say. A store handle carries one
/// ([`LocalHistory::reached_by`]) and every entry it records carries it too.
///
/// A way is one word of ASCII letters, digits, `-` and `_`, at most 32 of
/// them; anything else cannot be constructed, so it cannot reach the store.
/// Plain data, like the other history values.
///
/// [`TERMINAL`]: HistoryWay::TERMINAL
/// [`MCP`]: HistoryWay::MCP
/// [`new`]: HistoryWay::new
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryWay(Cow<'static, str>);

impl HistoryWay {
    /// The `arc` command line: every version a verb of `arc` writes.
    pub const TERMINAL: HistoryWay = HistoryWay(Cow::Borrowed("terminal"));

    /// `arc mcp`: every version one of its tools writes.
    pub const MCP: HistoryWay = HistoryWay(Cow::Borrowed("mcp"));

    /// A caller's own way, named by `word`. Refused with
    /// [`Error::HistoryWay`] when `word` is empty, longer than 32 characters,
    /// or holds a character other than an ASCII letter, a digit, `-` or `_`;
    /// and when it spells one of arc's own ways in any case, so an entry a
    /// caller records cannot read as arc's own unless the caller passes
    /// [`TERMINAL`](Self::TERMINAL) or [`MCP`](Self::MCP) to mean it.
    pub fn new(word: &str) -> Result<Self> {
        let refuse = |detail: String| Error::HistoryWay {
            way: word.to_string(),
            detail,
        };
        if let Some(fault) = word_fault(word) {
            return Err(refuse(fault));
        }
        for (own, name) in [(Self::TERMINAL, "TERMINAL"), (Self::MCP, "MCP")] {
            if own.as_str().eq_ignore_ascii_case(word) {
                return Err(refuse(format!(
                    "`{}` is arc's own way; pass `HistoryWay::{name}` to mean it",
                    own.as_str()
                )));
            }
        }
        Ok(HistoryWay(Cow::Owned(word.to_string())))
    }

    /// The way as the word `arc history list` prints.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A way read back from an entry's file name: any word a way may be, arc's
    /// own included.
    fn stored(word: &str) -> Option<Self> {
        word_fault(word)
            .is_none()
            .then(|| HistoryWay(Cow::Owned(word.to_string())))
    }
}

/// Why `word` cannot be a way, or `None` when it can.
fn word_fault(word: &str) -> Option<String> {
    if word.is_empty() {
        return Some("a way is one word, and this is empty".to_string());
    }
    if let Some(c) = word
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && *c != '-' && *c != '_')
    {
        return Some(format!("{c:?} is not an ASCII letter, a digit, `-` or `_`"));
    }
    if word.len() > WAY_MAX_LEN {
        return Some(format!("a way is at most {WAY_MAX_LEN} characters"));
    }
    None
}

/// One recorded state of a spec. Plain data: the id addresses the entry in
/// [`LocalHistory::read`] and [`LocalHistory::restore`], and begins the
/// snapshot's file name for anyone inspecting the store directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// `<millis>-<seq>-<kind>` — sortable, and stable once recorded.
    pub id: String,
    /// Save entry, machine-edit checkpoint, or a text never written to its
    /// file.
    pub kind: HistoryKind,
    /// The way arc was reached when the entry was written, or `None` for an
    /// entry written by a handle that named none — which is every entry an
    /// arc before ways wrote.
    pub way: Option<HistoryWay>,
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
    way: Option<HistoryWay>,
}

impl LocalHistory {
    /// The store at the conventional root: `$ARCFORM_HISTORY_DIR` when set,
    /// else `~/.arcform/history`. Refused with [`Error::HistoryRootMissing`]
    /// when neither resolves — the message names the env var, because that is
    /// the remedy.
    pub fn open_default() -> Result<Self> {
        resolve_root(std::env::var_os(HISTORY_DIR_ENV), dirs::home_dir()).map(Self::at_root)
    }

    /// The store rooted at `root`, created lazily on first record. The handle
    /// names no way until [`reached_by`](Self::reached_by) gives it one.
    pub fn at_root(root: impl Into<PathBuf>) -> Self {
        LocalHistory {
            root: root.into(),
            way: None,
        }
    }

    /// This store, recording every entry it writes from here on as reached by
    /// `way` — a save, a checkpoint, the checkpoint a restore takes, and both
    /// entries of a checkpointed road. Set once where a process opens the
    /// store: the way is how arc was reached, not what the call does.
    pub fn reached_by(self, way: HistoryWay) -> Self {
        LocalHistory {
            way: Some(way),
            ..self
        }
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
    ///
    /// The folder's log names the version's steps: each one `text` added,
    /// removed or changed since the newest entry before this call, as the
    /// module docs describe.
    pub fn record_save(&self, dir: &Path, text: &str) -> Result<Option<HistoryEntry>> {
        self.record_noted(
            dir,
            text,
            HistoryKind::Save,
            SystemTime::now(),
            true,
            Some(StepNote::Compare),
        )
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
        self.restore_keyed(self.key_dir(dir)?, &dir.join(MANIFEST_FILENAME), id)
    }

    // ------------------------------------------------------ the calls on a file

    /// [`record_save`](Self::record_save) for the file at `file` — a chart
    /// file beside a Protocol's spec, say. The file's history is its own:
    /// another file in the same directory, the Protocol's spec included,
    /// neither sees these entries nor shares the bound.
    ///
    /// The file is keyed by its canonical path, which is the key the calls
    /// that take a directory give `<dir>/arcform.yaml`: this call given a
    /// Protocol's `arcform.yaml` reads and writes that Protocol's history.
    /// The file need not exist — a deleted file's history stays readable and
    /// restorable — but its directory must, and a path naming a directory, or
    /// naming no file at all, is refused.
    pub fn record_save_for_file(&self, file: &Path, text: &str) -> Result<Option<HistoryEntry>> {
        self.record_keyed(
            self.file_key(file)?,
            text,
            HistoryKind::Save,
            SystemTime::now(),
            true,
            Some(StepNote::Compare),
        )
    }

    /// [`record_checkpoint`](Self::record_checkpoint) for the file at
    /// `file`, keyed as [`record_save_for_file`](Self::record_save_for_file)
    /// describes.
    pub fn record_checkpoint_for_file(
        &self,
        file: &Path,
        text: &str,
    ) -> Result<Option<HistoryEntry>> {
        self.record_keyed(
            self.file_key(file)?,
            text,
            HistoryKind::Checkpoint,
            SystemTime::now(),
            false,
            Some(StepNote::Unnamed),
        )
    }

    /// Record `text`, a text that was never written to the file at `file`,
    /// as an unsaved entry in that file's history, keyed as
    /// [`record_save_for_file`](Self::record_save_for_file) describes.
    ///
    /// The store is written, and the folder's log gains the entry's line: the
    /// file keeps its bytes, and a file that does not exist is not created.
    /// The entry is never merged — a save
    /// recorded moments later is an entry of its own, and so is this one
    /// after a save — and it counts toward the file's bound like any other.
    /// Only an exact duplicate of the newest entry is skipped (`Ok(None)`).
    /// [`restore_for_file`](Self::restore_for_file) given its id writes the
    /// text to the file.
    pub fn record_unsaved_for_file(&self, file: &Path, text: &str) -> Result<Option<HistoryEntry>> {
        self.record_keyed(
            self.file_key(file)?,
            text,
            HistoryKind::Unsaved,
            SystemTime::now(),
            false,
            Some(StepNote::Unnamed),
        )
    }

    /// Every entry recorded for the file at `file`, oldest first — that
    /// file's entries and no other's.
    pub fn entries_for_file(&self, file: &Path) -> Result<Vec<HistoryEntry>> {
        let (key_dir, _) = self.file_key(file)?;
        entries_in(&key_dir)
    }

    /// The exact bytes entry `id` recorded for the file at `file`.
    pub fn read_for_file(&self, file: &Path, id: &str) -> Result<String> {
        let (key_dir, _) = self.file_key(file)?;
        read_entry(&key_dir, id)
    }

    /// Roll the file at `file` back to the state entry `id` recorded, with
    /// [`restore`](Self::restore)'s discipline: the text being replaced is
    /// checkpointed in this file's history first, then the recorded bytes land
    /// atomically at `file`.
    pub fn restore_for_file(&self, file: &Path, id: &str) -> Result<String> {
        self.restore_keyed(self.file_key(file)?, file, id)
    }

    // ---------------------------------------------------------------- internals

    /// The restore both kinds of call share: read entry `id` under `key`,
    /// checkpoint what `target` holds now under the same key — no checkpoint,
    /// no write — and write the entry's bytes to `target`.
    fn restore_keyed(&self, key: (PathBuf, PathBuf), target: &Path, id: &str) -> Result<String> {
        let text = read_entry(&key.0, id)?;
        let spec_path = key.1.clone();
        let mut held = None;
        if target.exists() {
            let current = std::fs::read_to_string(target).map_err(|e| Error::FileRead {
                path: target.to_path_buf(),
                source: e,
            })?;
            held = self
                .record_keyed(
                    key,
                    &current,
                    HistoryKind::Checkpoint,
                    SystemTime::now(),
                    false,
                    None,
                )?
                .map(|entry| HeldLine { spec_path, entry });
        }
        write_atomic(target, text.as_bytes())?;
        // The checkpoint's line lands with the write it was taken for.
        if let Some(held) = held {
            held.land();
        }
        Ok(text)
    }

    /// Record one entry for the spec in `dir`. `now` is a parameter so the
    /// debounce window and ordering rules are testable without a clock;
    /// `merge` engages the save debounce, which the save-boundary entry
    /// points ask for through [`record_noted`](Self::record_noted) — see the
    /// retention-policy discussion in the module docs. The folder's log names
    /// no step for a version recorded here; the save-boundary entry points ask
    /// `record_noted` for a comparison.
    fn record(
        &self,
        dir: &Path,
        text: &str,
        kind: HistoryKind,
        now: SystemTime,
        merge: bool,
    ) -> Result<Option<HistoryEntry>> {
        self.record_noted(dir, text, kind, now, merge, Some(StepNote::Unnamed))
    }

    /// [`record`](Self::record), with `line` as what the folder's log says the
    /// version did to the steps, or `None` to hold the line for the caller.
    fn record_noted(
        &self,
        dir: &Path,
        text: &str,
        kind: HistoryKind,
        now: SystemTime,
        merge: bool,
        line: Option<StepNote>,
    ) -> Result<Option<HistoryEntry>> {
        self.record_keyed(self.key_dir(dir)?, text, kind, now, merge, line)
    }

    /// Record `text` as a checkpoint for the spec in `dir`, as
    /// [`record_checkpoint`](Self::record_checkpoint) does, and hold its line
    /// in the folder's log: the checkpoint is taken before a write, and its
    /// line lands with the write, through [`HeldLine::land`]. A write refused
    /// after the checkpoint leaves the folder as it was, and the checkpoint in
    /// the store alone.
    fn checkpoint_held(&self, dir: &Path, text: &str) -> Result<Option<HeldLine>> {
        let entry = self.record_noted(
            dir,
            text,
            HistoryKind::Checkpoint,
            SystemTime::now(),
            false,
            None,
        )?;
        Ok(entry.map(|entry| HeldLine {
            spec_path: dir.join(MANIFEST_FILENAME),
            entry,
        }))
    }

    /// Record one entry under `key`, the pair [`key_for`](Self::key_for)
    /// returns.
    fn record_keyed(
        &self,
        (key_dir, spec_path): (PathBuf, PathBuf),
        text: &str,
        kind: HistoryKind,
        now: SystemTime,
        merge: bool,
        line: Option<StepNote>,
    ) -> Result<Option<HistoryEntry>> {
        std::fs::create_dir_all(&key_dir)?;

        // Name the spec in the clear for anyone inspecting the store.
        let marker = key_dir.join(SPEC_PATH_FILE);
        if !marker.exists() {
            write_atomic(&marker, format!("{}\n", spec_path.display()).as_bytes())?;
        }

        let existing = entries_in(&key_dir)?;
        let newest = existing.last();

        // The state the store held before this one: the newest entry's bytes,
        // which the line's comparison reads too.
        let before = match newest {
            Some(newest) => Some(read_at(&key_dir, newest)?),
            None => None,
        };

        // The newest entry already records exactly this state: nothing new
        // to keep, whatever the kind and whatever the way.
        if before.as_deref() == Some(text) {
            return Ok(None);
        }

        // The debounce: a save hard on the heels of the newest save merges
        // into it — the newer state replaces the older entry. Checkpoints and
        // unsaved entries never merge, and a save never merges across an
        // intervening one (the newest entry would not be a save).
        let merge_into = newest
            .filter(|n| {
                merge
                    && kind == HistoryKind::Save
                    && n.kind == HistoryKind::Save
                    && now
                        .duration_since(n.at)
                        .is_ok_and(|gap| gap <= HISTORY_MERGE_WINDOW)
            })
            .cloned();

        let now_millis = now
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let (millis, seq) = next_stamp(newest, now_millis);
        let entry = HistoryEntry {
            id: entry_id(millis, seq, kind),
            kind,
            way: self.way.clone(),
            at: UNIX_EPOCH + Duration::from_millis(millis),
            bytes: text.len() as u64,
        };

        // Write the new entry before removing anything, so no moment holds
        // fewer recorded states than before. A merged save keeps the way of
        // the state that replaced the one it merged into.
        write_atomic(&entry_path(&key_dir, &entry), text.as_bytes())?;
        if let Some(old) = merge_into {
            let _ = std::fs::remove_file(entry_path(&key_dir, &old));
        }
        prune(&key_dir)?;

        // The folder's log names the version once it is recorded, unless the
        // caller holds the line for a write still to land. A courtesy: a line
        // arc cannot write refuses nothing the store has kept.
        if let Some(note) = line {
            let note = match note {
                StepNote::Compare => name_steps(&spec_path, before.as_deref(), text),
                other => other,
            };
            let _ = append_log_line(&spec_path, &entry, note);
        }

        Ok(Some(entry))
    }

    /// The key of the spec in `dir`: its `arcform.yaml` under the canonical
    /// directory.
    fn key_dir(&self, dir: &Path) -> Result<(PathBuf, PathBuf)> {
        let canonical = dir.canonicalize().map_err(|e| Error::FileRead {
            path: dir.to_path_buf(),
            source: e,
        })?;
        Ok(self.key_for(canonical.join(MANIFEST_FILENAME)))
    }

    /// The key of the file at `file`: its name under its canonical directory.
    /// The directory is canonicalised and the name is not, so the key is the
    /// one [`key_dir`](Self::key_dir) gives the file's directory when the
    /// name is `arcform.yaml`, whether or not the file exists or is a link.
    fn file_key(&self, file: &Path) -> Result<(PathBuf, PathBuf)> {
        let refuse = |why: &str| Error::FileRead {
            path: file.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, why),
        };
        // `Path::file_name` reads `a/..` as naming no file; `a/.` it reads as
        // `a`, so the directory check below is what refuses that shape.
        let name = file
            .file_name()
            .ok_or_else(|| refuse("the path names no file"))?;
        if file.is_dir() {
            return Err(refuse(
                "the path is a directory; local history keeps a file's versions",
            ));
        }
        let parent = match file.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let canonical = parent.canonicalize().map_err(|e| Error::FileRead {
            path: parent.to_path_buf(),
            source: e,
        })?;
        Ok(self.key_for(canonical.join(name)))
    }

    /// The per-file directory for `spec_path`, a canonical file path: the
    /// store root plus a hash of that path. Hashing makes the key
    /// filesystem-safe for any path; `spec-path` inside the directory keeps
    /// it human-legible. Returns the directory and `spec_path`.
    fn key_for(&self, spec_path: PathBuf) -> (PathBuf, PathBuf) {
        let digest = Sha256::digest(spec_path.as_os_str().as_encoded_bytes());
        let mut key = String::with_capacity(16);
        for byte in &digest[..8] {
            key.push_str(&format!("{byte:02x}"));
        }
        (self.root.join(key), spec_path)
    }
}

// ------------------------------------------------------- the checkpointed roads

/// The whole spec write path with the middle history tier engaged: apply
/// `edits` to `<dir>/arcform.yaml`, gate the result through the loader,
/// checkpoint the bytes being replaced, write atomically, and record the
/// after-image as a save entry. Both entries carry `history`'s way.
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
    let held = history.checkpoint_held(dir, &original)?;
    validated.write_to(dir)?;
    if let Some(held) = held {
        held.land();
    }
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
/// in [`edit_spec_with_history`]). Both entries carry `history`'s way.
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
    record_with_history(dir, history, &step.name, || {
        crate::record::record_step(dir, step)
    })
}

/// [`record_step_with_history`] for [`record_sql_step`](crate::record::record_sql_step):
/// the same checkpoint before the write and the same save after it, for a step
/// whose model's bytes are given.
pub(crate) fn record_sql_step_with_history(
    dir: &Path,
    step: &crate::record::SqlStep,
    history: &LocalHistory,
) -> Result<(PathBuf, ValidatedSpec)> {
    record_with_history(dir, history, step.name, || {
        crate::record::record_sql_step(dir, step)
    })
}

/// The checkpoint, the promotion `record` makes and the save, in that order, as
/// [`record_step_with_history`] describes them. The save's line in the
/// folder's log names `name` as the step added.
fn record_with_history(
    dir: &Path,
    history: &LocalHistory,
    name: &str,
    record: impl FnOnce() -> Result<(PathBuf, ValidatedSpec)>,
) -> Result<(PathBuf, ValidatedSpec)> {
    let path = dir.join(MANIFEST_FILENAME);
    if !path.exists() {
        return Err(Error::ManifestNotFound);
    }
    let original = std::fs::read_to_string(&path).map_err(|e| Error::FileRead {
        path: path.clone(),
        source: e,
    })?;
    let held = history.checkpoint_held(dir, &original)?;
    let (sql_rel, validated) = record()?;
    if let Some(held) = held {
        held.land();
    }
    // Merge disabled for the same reason as in `edit_spec_with_history`.
    let _ = history.record_noted(
        dir,
        validated.text(),
        HistoryKind::Save,
        SystemTime::now(),
        false,
        Some(StepNote::Added(name)),
    );
    Ok((sql_rel, validated))
}

// ------------------------------------------------------------------ internals

/// The retention policy in one line, for the surfaces that print it beside
/// the entries it governs. It is the snapshots' policy: the folder's log, which
/// [`log_place_line`] places, is neither pruned nor kept outside the protocol.
pub(crate) fn policy_line(root: &Path) -> String {
    format!(
        "policy: keeps the last {HISTORY_MAX_ENTRIES} snapshots per file (oldest pruned first); \
         saves within {}s of the newest save merge into it; the snapshots are stored outside \
         the protocol at {} and never promoted to git",
        HISTORY_MERGE_WINDOW.as_secs(),
        root.display()
    )
}

/// Where the log naming the versions of `file` is, in one line, for the
/// surfaces that print the policy: the log of `file`'s protocol folder, or the
/// words that say `file` is in no protocol's folder and gets no line.
pub(crate) fn log_place_line(file: &Path) -> String {
    let dir = match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let folder = file
        .file_name()
        .and_then(|name| protocol_folder(&dir.join(name)).map(Path::to_path_buf));
    match folder {
        Some(folder) => {
            let log = folder.join(LOG_FILENAME);
            format!(
                "log: one line per version, naming its file and no contents, at {}{} — inside \
                 the protocol, so it goes with the folder and `git add` stages it",
                log.display(),
                if log.exists() {
                    ""
                } else {
                    " (no line written yet)"
                }
            )
        }
        None => format!(
            "log: none — no folder at or above {} holds {MANIFEST_FILENAME}, so no line names \
             its versions",
            dir.display()
        ),
    }
}

/// The folder whose log names the versions of `file`, a path under a canonical
/// directory: the file's own directory when it is named `arcform.yaml`, else the
/// nearest directory at or above it that holds an `arcform.yaml`. `None` for a
/// file in no protocol's folder.
fn protocol_folder(file: &Path) -> Option<&Path> {
    let dir = file.parent()?;
    if file.file_name() == Some(std::ffi::OsStr::new(MANIFEST_FILENAME)) {
        return Some(dir);
    }
    dir.ancestors()
        .find(|dir| dir.join(MANIFEST_FILENAME).is_file())
}

/// The steps a save of the file at `spec_path` added, removed or changed,
/// found by comparing `after`, the text saved, with `before`, the state the
/// store held before it, as the note the save's line carries.
///
/// Only the spec has steps, so a file with any other name is [`StepNote::Unnamed`],
/// as is a save with no earlier state to compare with, a text on either side
/// that does not read as a spec's steps, and a save that changed none: a key at
/// the manifest's top, a comment, the order the steps stand in. The line then
/// names the file, which every line does, and no step.
fn name_steps(spec_path: &Path, before: Option<&str>, after: &str) -> StepNote<'static> {
    let is_spec = spec_path.file_name() == Some(std::ffi::OsStr::new(MANIFEST_FILENAME));
    let (true, Some(before)) = (is_spec, before) else {
        return StepNote::Unnamed;
    };
    let (Some(before), Some(after)) = (steps_of(before), steps_of(after)) else {
        return StepNote::Unnamed;
    };
    let mut found = Vec::new();
    for (name, step) in &after {
        let change = match before.iter().find(|(earlier, _)| earlier == name) {
            None => Change::Added,
            Some((_, earlier)) if earlier != step => Change::Changed,
            Some(_) => continue,
        };
        found.push(StepChange {
            step: name.clone(),
            change,
        });
    }
    for (name, _) in &before {
        if !after.iter().any(|(kept, _)| kept == name) {
            found.push(StepChange {
                step: name.clone(),
                change: Change::Removed,
            });
        }
    }
    if found.is_empty() {
        StepNote::Unnamed
    } else {
        StepNote::Steps(found)
    }
}

/// The steps of the spec in `text`, each with its name, in the order they stand
/// there. The text is read as YAML and not through the loader, which refuses a
/// spec for much besides its steps, and a state the store recorded may be one
/// the loader refuses. `None` when `text` is not a mapping, when its `steps` is
/// not a sequence of mappings each with a text `name`, or when two steps share
/// a name, since a step cannot then be told from its namesake. A spec with no
/// `steps` has none, and a step compares as the value it parses to, so a comment
/// or the order of its keys is no change.
fn steps_of(text: &str) -> Option<Vec<(String, serde_yaml::Value)>> {
    let spec = match serde_yaml::from_str::<serde_yaml::Value>(text) {
        Ok(serde_yaml::Value::Mapping(spec)) => spec,
        _ => return None,
    };
    let steps = match spec.get("steps") {
        None | Some(serde_yaml::Value::Null) => return Some(Vec::new()),
        Some(serde_yaml::Value::Sequence(steps)) => steps,
        Some(_) => return None,
    };
    let mut named: Vec<(String, serde_yaml::Value)> = Vec::new();
    for step in steps {
        let name = step.get("name")?.as_str()?.to_string();
        if named.iter().any(|(earlier, _)| *earlier == name) {
            return None;
        }
        named.push((name, step.clone()));
    }
    Some(named)
}

/// Append the line naming `entry`, a version of the file at `file`, to the log
/// of `file`'s protocol folder, creating the log when there is none. The line
/// lands in one write, so no reader sees half of it; a log whose last byte is
/// not a newline gets one in the same write, ahead of the line. A file in no
/// protocol's folder gets no line.
fn append_log_line(file: &Path, entry: &HistoryEntry, note: StepNote) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let Some(folder) = protocol_folder(file) else {
        return Ok(());
    };
    let inside = file.strip_prefix(folder).unwrap_or(file);
    let name = inside
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");

    let mut log = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(folder.join(LOG_FILENAME))?;
    let mut bytes = Vec::new();
    if log.metadata()?.len() > 0 {
        let mut last = [0u8; 1];
        log.seek(SeekFrom::End(-1))?;
        log.read_exact(&mut last)?;
        if last[0] != b'\n' {
            bytes.push(b'\n');
        }
    }
    bytes.extend_from_slice(log_line(&name, entry, note).as_bytes());
    log.write_all(&bytes)
}

/// The log's line for `entry`, a version of the file named `name` inside its
/// protocol folder: one JSON object with the keys the module docs give,
/// newline included.
fn log_line(name: &str, entry: &HistoryEntry, note: StepNote) -> String {
    /// One line of the log. The fields serialise in this order, and a field
    /// that is `None` is left out of the object.
    #[derive(serde::Serialize)]
    struct Line<'a> {
        at: String,
        file: &'a str,
        kind: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        interface: Option<&'a str>,
        version: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        step: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        change: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        steps: Option<Vec<Named<'a>>>,
    }
    /// One of several steps a save touched.
    #[derive(serde::Serialize)]
    struct Named<'a> {
        step: &'a str,
        change: &'static str,
    }
    let (mut step, mut change, mut steps) = (None, None, None);
    match &note {
        StepNote::Unnamed | StepNote::Compare => {}
        StepNote::Added(added) => (step, change) = (Some(*added), Some(Change::Added.word())),
        StepNote::Steps(found) => match found.as_slice() {
            [one] => (step, change) = (Some(one.step.as_str()), Some(one.change.word())),
            several => {
                steps = Some(
                    several
                        .iter()
                        .map(|one| Named {
                            step: &one.step,
                            change: one.change.word(),
                        })
                        .collect(),
                )
            }
        },
    }
    let line = Line {
        at: humantime::format_rfc3339_millis(entry.at).to_string(),
        file: name,
        kind: entry.kind.tag(),
        interface: entry.way.as_ref().map(HistoryWay::as_str),
        version: &entry.id,
        step,
        change,
        steps,
    };
    let mut text = serde_json::to_string(&line).expect("a line of strings serialises");
    text.push('\n');
    text
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

/// An entry id's parts: its millis, its sequence number and its kind.
type IdParts = (u64, u32, HistoryKind);

/// The reverse of [`entry_id`]. Anything that does not parse is not an entry
/// id.
fn parse_id(id: &str) -> Option<IdParts> {
    let mut parts = id.splitn(3, '-');
    let millis = parts.next()?.parse().ok()?;
    let seq = parts.next()?.parse().ok()?;
    let kind = HistoryKind::from_tag(parts.next()?)?;
    Some((millis, seq, kind))
}

/// An entry file's stem as its id, the id's parts, and its way: `<id>` for an
/// entry that names no way — the one shape an arc before ways wrote — and
/// `<id>.<way>` for one that does. A way holds no `.`, so the first `.` is the
/// split, and a stem whose id or way does not parse is not an entry.
fn parse_stem(stem: &str) -> Option<(&str, IdParts, Option<HistoryWay>)> {
    let (id, way) = match stem.split_once('.') {
        None => (stem, None),
        Some((id, word)) => (id, Some(HistoryWay::stored(word)?)),
    };
    Some((id, parse_id(id)?, way))
}

/// Where `entry`'s snapshot lives: its id, then its way when it names one.
fn entry_path(key_dir: &Path, entry: &HistoryEntry) -> PathBuf {
    match &entry.way {
        None => key_dir.join(format!("{}.yaml", entry.id)),
        Some(way) => key_dir.join(format!("{}.{}.yaml", entry.id, way.as_str())),
    }
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
        let Some((id, (millis, seq, kind), way)) = parse_stem(stem) else {
            continue;
        };
        found.push((
            millis,
            seq,
            HistoryEntry {
                id: id.to_string(),
                kind,
                way,
                at: UNIX_EPOCH + Duration::from_millis(millis),
                bytes: entry.metadata()?.len(),
            },
        ));
    }
    found.sort_by_key(|(millis, seq, _)| (*millis, *seq));
    Ok(found.into_iter().map(|(_, _, entry)| entry).collect())
}

/// The bytes entry `id` recorded under `key_dir`. The id is looked up among
/// the entries listed there, whatever way each names, so an id an earlier arc
/// printed and an id this one prints are found alike — and a path-shaped "id"
/// never reaches the filesystem.
fn read_entry(key_dir: &Path, id: &str) -> Result<String> {
    let entry = entries_in(key_dir)?
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| Error::HistoryEntryNotFound { id: id.to_string() })?;
    read_at(key_dir, &entry)
}

/// The bytes `entry` recorded under `key_dir`. The entry was listed, so a
/// file gone by now is a failed read, named by its path.
fn read_at(key_dir: &Path, entry: &HistoryEntry) -> Result<String> {
    let path = entry_path(key_dir, entry);
    std::fs::read_to_string(&path).map_err(|e| Error::FileRead { path, source: e })
}

/// Enforce [`HISTORY_MAX_ENTRIES`]: remove the oldest entries beyond the
/// bound.
fn prune(key_dir: &Path) -> Result<()> {
    let entries = entries_in(key_dir)?;
    if entries.len() > HISTORY_MAX_ENTRIES {
        for entry in &entries[..entries.len() - HISTORY_MAX_ENTRIES] {
            let _ = std::fs::remove_file(entry_path(key_dir, entry));
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
    fn an_entry_file_names_its_way_after_its_id_and_an_earlier_arcs_names_none() {
        let entry = |way| HistoryEntry {
            id: "1700000000123-007-save".to_string(),
            kind: HistoryKind::Save,
            way,
            at: UNIX_EPOCH,
            bytes: 0,
        };
        let key_dir = Path::new("/store/key");
        assert_eq!(
            entry_path(key_dir, &entry(None)),
            key_dir.join("1700000000123-007-save.yaml")
        );
        assert_eq!(
            entry_path(key_dir, &entry(Some(HistoryWay::MCP))),
            key_dir.join("1700000000123-007-save.mcp.yaml")
        );

        let parts = (1_700_000_000_123, 7, HistoryKind::Save);
        assert_eq!(
            parse_stem("1700000000123-007-save"),
            Some(("1700000000123-007-save", parts, None))
        );
        assert_eq!(
            parse_stem("1700000000123-007-save.my-App_2"),
            Some((
                "1700000000123-007-save",
                parts,
                Some(HistoryWay::new("my-App_2").unwrap())
            ))
        );
        assert_eq!(
            parse_stem("1700000000123-007-save.terminal"),
            Some(("1700000000123-007-save", parts, Some(HistoryWay::TERMINAL))),
            "arc's own words are read back as arc's own"
        );
        for bad in [
            "1700000000123-007-save.",
            "1700000000123-007-save.a b",
            "1700000000123-007-save.a.b",
            "1700000000123-007-commit.app",
            "x.app",
        ] {
            assert_eq!(parse_stem(bad), None, "{bad:?} is not an entry");
        }
    }

    #[test]
    fn a_store_that_cannot_be_listed_is_not_reported_as_a_missing_entry() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        let (key_dir, _) = history.key_dir(&dir).unwrap();
        std::fs::create_dir_all(key_dir.parent().unwrap()).unwrap();
        // The spec's place in the store is a file, so its entries cannot be
        // listed; a lookup by id says that, not that the id names nothing.
        std::fs::write(&key_dir, "not a directory\n").unwrap();
        for err in [
            history.read(&dir, "1700000000000-000-save").unwrap_err(),
            history.restore(&dir, "1700000000000-000-save").unwrap_err(),
        ] {
            assert!(matches!(err, Error::Io(_)), "{err}");
        }
    }

    #[test]
    fn a_merged_save_keeps_the_way_of_the_state_that_replaced_it() {
        let (tmp, history) = store();
        let dir = protocol(&tmp);
        let app = history.clone().reached_by(HistoryWay::new("app").unwrap());
        let terminal = history.reached_by(HistoryWay::TERMINAL);
        app.record(&dir, "a\n", HistoryKind::Save, at(T0), true)
            .unwrap();
        terminal
            .record(&dir, "b\n", HistoryKind::Save, at(T0 + 5), true)
            .unwrap();
        let entries = terminal.entries(&dir).unwrap();
        assert_eq!(entries.len(), 1, "the saves merged: {entries:?}");
        assert_eq!(entries[0].way, Some(HistoryWay::TERMINAL));
        assert_eq!(terminal.read(&dir, &entries[0].id).unwrap(), "b\n");

        // A state identical to the newest entry is not recorded again, whatever
        // way records it.
        let again = app
            .record(&dir, "b\n", HistoryKind::Checkpoint, at(T0 + 60), false)
            .unwrap();
        assert_eq!(again, None);
    }

    #[test]
    fn the_policy_line_states_the_bound_the_window_and_the_git_stance() {
        let line = policy_line(Path::new("/somewhere"));
        assert!(line.contains(&format!("{HISTORY_MAX_ENTRIES} snapshots per file")));
        assert!(line.contains(&format!("{}s", HISTORY_MERGE_WINDOW.as_secs())));
        assert!(line.ends_with(
            "the snapshots are stored outside the protocol at /somewhere and never \
             promoted to git"
        ));
    }
}
