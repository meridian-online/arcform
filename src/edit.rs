//! The spec write path: apply an edit to `arcform.yaml`, validate the result,
//! write it atomically — without destroying a byte the edit did not target.
//!
//! # The problem this solves
//!
//! An `arcform.yaml` is hand-authored: comments, blank lines, key order and quote
//! style all carry the author's intent. Rust cannot round-trip that through the
//! serialize-a-struct path — `serde_yaml` drops comments by design — so a tool
//! that edits a spec by loading and re-emitting it destroys the document it was
//! asked to change. The only writer that preserves authorship is one that edits
//! the **original bytes** and never re-serialises.
//!
//! So this module splices. Each [`SpecEdit`] resolves the node it targets to a
//! byte span (tree-sitter node spans, via `yamlpath`), replaces exactly those
//! bytes, and leaves every other byte identical. The result then passes through
//! [`Manifest::from_yaml_str`] — the same loader `arc run` uses — before it may
//! reach disk. **Validation is a gate, not a transform**: a candidate that will
//! not load is refused with the loader's reason, never "fixed".
//!
//! # An edit is a value
//!
//! [`SpecEdit`]s are plain data: they can be built, inspected, logged and thrown
//! away without touching a file. [`apply_edits`] turns original bytes plus edits
//! into a [`ValidatedSpec`] — or a refusal — entirely in memory; only
//! [`ValidatedSpec::write_to`] (or the one-shot [`edit_spec`]) touches disk, and
//! it writes atomically (temp file + rename), so an interrupted write can never
//! leave a truncated spec behind.
//!
//! # What "preserved" means, exactly
//!
//! Every byte outside the spans an edit targets is written back verbatim. No
//! canonicalisation, reflow or reformat happens as a side effect of writing. The
//! single permitted normalisation is the final newline: a result that does not
//! end in `\n` gets one. Trailing whitespace is deliberately **not** stripped —
//! inside a `command: |` block scalar, trailing spaces are part of the value,
//! and a writer that strips them has silently changed what the step runs.
//!
//! # Comment ownership
//!
//! Deleting or reordering a block-sequence item forces a decision no YAML parser
//! makes for you: which item owns the comment between two items? The convention
//! here: **a `#` block flush against an item — no blank line between, indented
//! no deeper than the item — is that item's header** and travels with it (moved
//! by [`SpecEdit::Reorder`], removed by [`SpecEdit::Delete`]). A comment
//! separated from the item below it by a blank line belongs to the sequence and
//! stays put. Item extents are derived from the **text**, not from parser node
//! boundaries — tree-sitter attaches an inter-item comment to the *preceding*
//! item's node, which is exactly the wrong owner for a section header.
//!
//! [`SpecEdit::Nest`] and [`SpecEdit::Lift`] move whole mapping entries by the
//! same rule: a moved key takes its flush header, every line of its value and
//! the comments among them, and the header of the first key moved under a new
//! sequence becomes the header of that sequence's item. A nest also writes a
//! key the caller names, so its result is held to the document it describes —
//! the loaded original with the entries moved — and text that loads to
//! anything else is refused.
//!
//! # Creating is not editing
//!
//! A spec authored from scratch has no prior authorship to protect, so
//! [`create_spec`] serialises a [`Manifest`] directly — none of the splicing
//! machinery is involved. It still passes the same validation gate and the same
//! atomic write, and it refuses to overwrite an existing spec: once a file
//! exists it may have been hand-edited, and edits go through [`edit_spec`].
//!
//! # Any YAML, not only a spec
//!
//! Nothing in the splice reads a field of the Protocol: a path resolves over
//! YAML text, and the spec is only the gate at the end. [`apply_yaml_edits`] is
//! that splice without the spec gate, for YAML a sibling tool owns — a chart
//! file — and checks against its own schema. Its result must still load as
//! YAML. [`apply_edits`] and [`edit_spec`] keep the spec gate, so text that is
//! not a Protocol is refused on those two entries.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};
use crate::manifest::{MANIFEST_FILENAME, Manifest};

// ------------------------------------------------------------------ the values

/// One step along a path into the YAML document: a mapping key or a sequence
/// index. `"steps".into()` and `2.into()` both work, so a path reads
/// `vec!["steps".into(), 2.into(), "command".into()]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPart {
    /// A mapping key, e.g. `steps` or `command`.
    Key(String),
    /// A zero-based index into a sequence.
    Index(usize),
}

impl From<&str> for PathPart {
    fn from(key: &str) -> Self {
        PathPart::Key(key.to_string())
    }
}

impl From<String> for PathPart {
    fn from(key: String) -> Self {
        PathPart::Key(key)
    }
}

impl From<usize> for PathPart {
    fn from(index: usize) -> Self {
        PathPart::Index(index)
    }
}

/// A proposed change to a spec — a value, inspectable and refusable before it
/// touches any file.
///
/// Replacement text is spliced **verbatim**: multi-line values must arrive
/// already indented for the position they land in, and a new sequence item must
/// carry its own `- ` line(s). Nothing here reformats on the caller's behalf —
/// that is what keeps the rest of the document byte-identical. A malformed
/// splice is refused before it is returned: [`apply_edits`] ends at the
/// [`Manifest::from_yaml_str`] gate, and [`apply_yaml_edits`], which edits YAML
/// that is not a spec, ends at a YAML load.
///
/// In a batch, each edit sees the document as the previous edits left it, so
/// indices refer to the already-partially-edited spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecEdit {
    /// Replace the value at `path` with `value`. For `key: value` pairs the key,
    /// separator and any trailing same-line comment are untouched; only the
    /// value's own bytes are replaced. Replacing the absent value of a bare
    /// `key:` is supported — start the replacement with the separating space
    /// (or a newline for a block value), since the splice lands right after
    /// the colon.
    Replace { path: Vec<PathPart>, value: String },

    /// Replace one occurrence of `from` with `to` inside the value at `path` —
    /// the way to touch three characters of a 20-line `command: |` block
    /// without owning the other lines. Refused unless `from` occurs exactly
    /// once in that value (zero is a miss, two is an ambiguity).
    RewriteFragment {
        path: Vec<PathPart>,
        from: String,
        to: String,
    },

    /// Add `key: value` to the mapping at `path` (the manifest root when `path`
    /// is empty). Block mappings gain a line after their last entry, indented
    /// like their other keys; flow mappings (`{ … }`) gain a `, key: value`
    /// before the closing brace; a bare `key:` with no value gains its first
    /// child. An empty `value` in a block mapping or at the root writes a bare
    /// `key:`, with no space after the colon, for a later `Add` to give its
    /// first child. Refused if the target is not a mapping.
    Add {
        path: Vec<PathPart>,
        key: String,
        value: String,
    },

    /// Append `item` to the sequence at `path`. For a block sequence the item
    /// text (its own `- ` line(s), pre-indented) lands after the last item's
    /// body — before any trailing comment that follows the sequence. For a flow
    /// sequence (`[ … ]`) the item is inserted before the closing bracket.
    /// Refused if the target is not a sequence.
    Append { path: Vec<PathPart>, item: String },

    /// Delete the element at `path`, including its flush comment header (see
    /// the module docs on comment ownership). Deleting a sequence item also
    /// consumes the blank line(s) that separated it from the next item, so no
    /// double gap is left behind. Refused for elements of flow collections —
    /// line-based removal inside `[a, b]` or `{a: 1}` would take neighbours
    /// with it; replace the whole collection instead.
    Delete { path: Vec<PathPart> },

    /// Move the item at index `from` of the block sequence at `path` so it ends
    /// up at index `to` (the `Vec::remove` + `Vec::insert` convention). The
    /// item's flush comment header moves with it.
    Reorder {
        path: Vec<PathPart>,
        from: usize,
        to: usize,
    },

    /// Move the entries `keys` of the block mapping at `path` (the document's
    /// root when `path` is empty) under a new key `under`, as the one mapping
    /// of a new block sequence: `plot:` and `width:` at the root become
    /// `vconcat:` over `  - plot:` and `    width:`. The moved lines keep their
    /// order, their comments and their blank lines, and each gains the
    /// indentation that puts it under the new entry; a flush comment header
    /// above the first moved key becomes the new entry's header. `under` takes
    /// the place of the first moved entry, and an entry `keys` does not name
    /// that sat between two moved ones follows it. Refused when `path` is not a
    /// block mapping, when `keys` is empty, repeats a key or names one the
    /// mapping does not hold, and when the mapping already holds `under`.
    /// [`SpecEdit::Lift`] is the inverse.
    Nest {
        path: Vec<PathPart>,
        keys: Vec<String>,
        under: String,
    },

    /// Put the one mapping of the block sequence at `key`, in the mapping at
    /// `path`, in `key`'s place, and take `key` out: the inverse of
    /// [`SpecEdit::Nest`]. The mapping's lines move to `key`'s column with
    /// their comments and blank lines. Refused unless `key` holds a block
    /// sequence of exactly one item, and that item is a block mapping whose
    /// first key shares the dash's line and whose keys the mapping at `path`
    /// does not already hold. A second item would be lost, so `Delete` it
    /// first.
    Lift { path: Vec<PathPart>, key: String },
}

/// The result of applying edits: candidate bytes that have already passed the
/// spec gate, plus the [`Manifest`] those bytes parse to. Holding one is proof
/// the text loads; [`ValidatedSpec::write_to`] is the only road to disk and it
/// is atomic.
#[derive(Debug, Clone)]
pub struct ValidatedSpec {
    text: String,
    manifest: Manifest,
}

impl ValidatedSpec {
    /// The exact bytes that will be written — inspect or diff them before
    /// committing to disk.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What those bytes parse to, through the real loader.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Atomically write the validated bytes as `<dir>/arcform.yaml`: the bytes
    /// go to a temp file in the same directory, are flushed, and are renamed
    /// over the target. An interrupt at any point leaves either the old spec or
    /// the new one — never a truncated file.
    pub fn write_to(&self, dir: &Path) -> Result<()> {
        write_atomic(&dir.join(MANIFEST_FILENAME), self.text.as_bytes())
    }
}

// ------------------------------------------------------------- the entry points

/// Apply `edits` to `original` in memory and gate the result through
/// [`Manifest::from_yaml_str`]. No file is touched: the return value is either
/// a [`ValidatedSpec`] ready to write, or the refusal — an [`Error::EditTarget`]
/// naming the path that failed to resolve, or the loader's own parse/validation
/// error for a result that would not load.
pub fn apply_edits(original: &str, edits: &[SpecEdit]) -> Result<ValidatedSpec> {
    let text = splice_edits(original, edits)?;
    let manifest = Manifest::from_yaml_str(&text)?;
    Ok(ValidatedSpec { text, manifest })
}

/// Apply `edits` to YAML text of any shape — a chart file, a config, anything
/// that is not an `arcform.yaml` — and return the edited text. It is the same
/// splice [`apply_edits`] runs, with the same byte preservation and the same
/// single normalisation (the final newline); what it does not do is ask
/// whether the result is a Protocol. The one gate it keeps is that the result
/// still loads as YAML, so a malformed splice is refused here rather than
/// handed back as text to write. Whether the YAML means anything is the
/// caller's schema to check.
///
/// Nothing is read or written: the caller owns the file and its write.
pub fn apply_yaml_edits(original: &str, edits: &[SpecEdit]) -> Result<String> {
    let text = splice_edits(original, edits)?;
    serde_yaml::from_str::<serde_yaml::Value>(&text).map_err(|e| Error::EditTarget {
        path: "(document)".to_string(),
        detail: format!("the edited text no longer loads as YAML: {e}"),
    })?;
    Ok(text)
}

/// The whole write path against a protocol directory: read `arcform.yaml`,
/// apply `edits`, validate, and atomically replace the file. Nothing is written
/// unless every edit applies and the result loads; on refusal the file on disk
/// is untouched, byte for byte.
pub fn edit_spec(dir: &Path, edits: &[SpecEdit]) -> Result<ValidatedSpec> {
    let path = dir.join(MANIFEST_FILENAME);
    if !path.exists() {
        return Err(Error::ManifestNotFound);
    }
    let original = std::fs::read_to_string(&path).map_err(|e| Error::FileRead {
        path: path.clone(),
        source: e,
    })?;
    let validated = apply_edits(&original, edits)?;
    validated.write_to(dir)?;
    Ok(validated)
}

/// Create `<dir>/arcform.yaml` from scratch by serialising `manifest` directly —
/// a new spec has no prior authorship to preserve, so none of the splicing
/// machinery is involved. The generated bytes still pass the same
/// [`Manifest::from_yaml_str`] gate and the same atomic write. Refuses with
/// [`Error::SpecExists`] if the file already exists: an existing spec may be
/// hand-authored, and edits to it go through [`edit_spec`].
pub fn create_spec(dir: &Path, manifest: &Manifest) -> Result<ValidatedSpec> {
    let path = dir.join(MANIFEST_FILENAME);
    if path.exists() {
        return Err(Error::SpecExists(path));
    }
    let text = serde_yaml::to_string(manifest)?;
    let manifest = Manifest::from_yaml_str(&text)?;
    std::fs::create_dir_all(dir)?;
    write_atomic(&path, text.as_bytes())?;
    Ok(ValidatedSpec { text, manifest })
}

// ------------------------------------------------------------- the ignore list

/// The ignore list's file name, written beside `arcform.yaml`.
pub(crate) const IGNORE_FILENAME: &str = ".gitignore";

/// What [`write_ignore_list`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IgnoreList {
    /// The list was written.
    Written,
    /// An entry named `.gitignore` was there already, and is as it was.
    Kept,
}

/// Write the ignore list beside `arcform.yaml` in `dir`, for the verbs that make a
/// Protocol directory. It names what arc knows without reading the Protocol: the run
/// records and tool stamps under `build/.arcform/`; when `db` — the manifest's `db:`
/// value — puts the database inside `dir`, that file, its write-ahead log and the
/// directory DuckDB spills to beside it; and each database in `copied` — the ones
/// `arc init --from-descriptor` copied into `dir` — with its write-ahead log.
/// What a step writes is the author's, and the list does not guess it.
///
/// An entry named `.gitignore` already in `dir` is left as it is: `create-protocol`
/// makes a directory that may exist, and the author may have written the list first.
/// `create_new` is the existence check and the creation in one call, so a list that
/// appears between a look and a write is not overwritten, and a dangling symlink of
/// that name is kept rather than written through. This looks for no repository and
/// runs no `git`.
pub(crate) fn write_ignore_list(
    dir: &Path,
    db: Option<&str>,
    copied: &[&str],
) -> Result<IgnoreList> {
    use std::io::Write;

    let path = dir.join(IGNORE_FILENAME);
    let named = |e: std::io::Error| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", path.display()),
        ))
    };
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(IgnoreList::Kept),
        Err(e) => return Err(named(e)),
    };
    if let Err(e) = file.write_all(ignore_list_text(dir, db, copied).as_bytes()) {
        // A half-written list would be read as the author's on the next run.
        let _ = std::fs::remove_file(&path);
        return Err(named(e));
    }
    Ok(IgnoreList::Written)
}

/// The text of the ignore list, anchored at the directory it sits in. DuckDB spills a
/// query that outgrows memory to `<database>.tmp/` beside the database, and a run
/// killed while spilling leaves files in it, so the database `db` names has that
/// directory listed with it. A database in `copied` was brought here by
/// `arc init --from-descriptor` and is listed with its log alone.
fn ignore_list_text(dir: &Path, db: Option<&str>, copied: &[&str]) -> String {
    let mut text = String::from(
        "# Written by `arc`: what a run records belongs to the machine that ran it.\n\
         /build/.arcform/\n",
    );
    if let Some(db) = db.and_then(|db| path_inside(dir, db)) {
        text.push_str(
            "# The database this Protocol names, its write-ahead log, and the directory \
             DuckDB spills to beside it.\n",
        );
        text.push_str(&format!("/{db}\n/{db}.wal\n/{db}.tmp/\n"));
    }
    let copied: Vec<String> = copied
        .iter()
        .filter_map(|db| path_inside(dir, db))
        .collect();
    if !copied.is_empty() {
        text.push_str(
            "# Each database `arc init --from-descriptor` copied in, and its write-ahead log. \
             A clone does not hold them: bring each to it another way.\n",
        );
        for db in copied {
            text.push_str(&format!("/{db}\n/{db}.wal\n"));
        }
    }
    text
}

/// `db`, as a run joins it to `dir`, written the way an ignore list in `dir` reads a
/// path: relative, `/`-separated, with the characters the list gives a meaning
/// escaped. `None` when it names a place outside `dir`, or holds a character a line of
/// the list cannot carry. A relative path is resolved lexically, so `./a/../w.duckdb`
/// is `w.duckdb` and `../w.duckdb` is outside; an absolute one is inside only when it
/// sits under `dir`, which is looked for as `dir` is written and then as it resolves.
fn path_inside(dir: &Path, db: &str) -> Option<String> {
    let relative = if Path::new(db).is_absolute() {
        let db = Path::new(db);
        // What is left of `db` after its prefix is a tail of a `&str`, so it is text.
        db.strip_prefix(dir)
            .ok()
            .or_else(|| db.strip_prefix(dir.canonicalize().ok()?).ok())?
            .to_string_lossy()
            .into_owned()
    } else {
        db.to_string()
    };
    let mut parts: Vec<String> = Vec::new();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(escape_ignore_pattern(part)?),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// One path component, with each character the ignore list reads as a pattern
/// escaped. `None` for a control character: a newline ends the line it would be on.
fn escape_ignore_pattern(part: &str) -> Option<String> {
    let mut out = String::new();
    for c in part.chars() {
        if c.is_control() {
            return None;
        }
        if matches!(c, '\\' | '*' | '?' | '[' | ' ') {
            out.push('\\');
        }
        out.push(c);
    }
    Some(out)
}

// ------------------------------------------------------------- the merge rule

/// The attributes file's name, written beside `arcform.yaml` and the ignore list.
pub(crate) const ATTRIBUTES_FILENAME: &str = ".gitattributes";

/// What [`write_merge_rule`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MergeRule {
    /// The attributes file was written, holding the rule.
    Written,
    /// An attributes file was there, and the rule is now its last line.
    Added,
    /// An entry named `.gitattributes` was there already, and is as it was.
    Kept,
}

/// The rule: git merges the Protocol's log by keeping both sides' lines. Anchored, so
/// it names the log beside the file it sits in and no other log below it.
fn merge_rule_line() -> String {
    format!("/{} merge=union\n", crate::history::LOG_FILENAME)
}

/// A fresh attributes file: the rule, under a comment that names no file, so the rule
/// is the one line that names the log.
fn merge_rule_file_text() -> String {
    format!(
        "# Written by `arc`: when two clones of a Protocol are merged, git keeps both \
         sides' lines of the log below.\n{}",
        merge_rule_line()
    )
}

/// Whether some line of `attributes` already sets the log's `merge` attribute, to
/// `union` or to anything else: an author who named another driver for the log made
/// that choice, and a later line of ours would override it.
fn sets_the_logs_merge(attributes: &[u8]) -> bool {
    let log = crate::history::LOG_FILENAME;
    String::from_utf8_lossy(attributes).lines().any(|line| {
        let mut words = line.split_ascii_whitespace();
        let names_the_log = words.next().is_some_and(|pattern| {
            pattern == log || pattern.strip_prefix('/').is_some_and(|p| p == log)
        });
        names_the_log
            && words.any(|attribute| {
                let name = attribute.strip_prefix(['-', '!']).unwrap_or(attribute);
                name.split('=').next() == Some("merge")
            })
    })
}

/// Write the rule that makes git keep both sides' lines of the Protocol's log, beside
/// `arcform.yaml` in `dir`, for the verbs that make a Protocol directory. Two clones
/// that each record a version append to the same log, and without the rule their merge
/// conflicts on it. The merged log holds each side's lines in the order git leaves
/// them, not in time order: each line carries its own time.
///
/// An attributes file already in `dir` keeps its lines and gains the rule as its last
/// line, unless a line of it already sets the log's `merge` attribute, in which case it
/// is left as it is. A name that is a link or a directory is left as it is too: a link
/// may lead to a file other folders share, and appending through it would edit theirs.
/// Like the ignore list this looks for no repository and runs no `git`: the rule is
/// written in a folder that is no repository, which may become one.
pub(crate) fn write_merge_rule(dir: &Path) -> Result<MergeRule> {
    use std::io::Write;

    let path = dir.join(ATTRIBUTES_FILENAME);
    let named = |e: std::io::Error| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", path.display()),
        ))
    };
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            if let Err(e) = file.write_all(merge_rule_file_text().as_bytes()) {
                // A half-written file would be read as the author's on the next run.
                let _ = std::fs::remove_file(&path);
                return Err(named(e));
            }
            return Ok(MergeRule::Written);
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(named(e)),
    }

    if !std::fs::symlink_metadata(&path).map_err(named)?.is_file() {
        return Ok(MergeRule::Kept);
    }
    let existing = std::fs::read(&path).map_err(named)?;
    if sets_the_logs_merge(&existing) {
        return Ok(MergeRule::Kept);
    }
    // One append, so the rule lands whole; a file that ends mid-line is ended first.
    let mut added = Vec::new();
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        added.push(b'\n');
    }
    added.extend_from_slice(merge_rule_line().as_bytes());
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(&added))
        .map_err(named)?;
    Ok(MergeRule::Added)
}

// ------------------------------------------------------------------- splicing

/// Apply every edit in order, each to the text the previous one left, then
/// add the final newline if it is missing. It resolves paths over YAML text
/// and names no field of any schema; the gate is its callers'.
fn splice_edits(original: &str, edits: &[SpecEdit]) -> Result<String> {
    let mut text = original.to_string();
    for edit in edits {
        text = apply_one(&text, edit)?;
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// Apply a single edit to `text`, returning the new text or the refusal.
fn apply_one(text: &str, edit: &SpecEdit) -> Result<String> {
    match edit {
        SpecEdit::Replace { path, value } => {
            let doc = parse(text)?;
            let (start, end) = value_span(text, &doc, path)?;
            Ok(splice(text, start, end, value))
        }
        SpecEdit::RewriteFragment { path, from, to } => {
            let doc = parse(text)?;
            let (start, end) = value_span(text, &doc, path)?;
            let slice = &text[start..end];
            let hits: Vec<usize> = slice.match_indices(from.as_str()).map(|(i, _)| i).collect();
            match hits.as_slice() {
                [] => Err(target_err(path, format!("fragment {from:?} not found"))),
                [at] => Ok(splice(text, start + at, start + at + from.len(), to)),
                many => Err(target_err(
                    path,
                    format!(
                        "fragment {from:?} is ambiguous ({} occurrences)",
                        many.len()
                    ),
                )),
            }
        }
        SpecEdit::Add { path, key, value } => add_key(text, path, key, value),
        SpecEdit::Append { path, item } => append_item(text, path, item),
        SpecEdit::Delete { path } => delete_element(text, path),
        SpecEdit::Reorder { path, from, to } => reorder_items(text, path, *from, *to),
        SpecEdit::Nest { path, keys, under } => nest_entries(text, path, keys, under),
        SpecEdit::Lift { path, key } => lift_entry(text, path, key),
    }
}

/// Splice `new` over `text[start..end]`. Every byte outside the span is carried
/// over verbatim — this is the only way edited text is ever produced.
fn splice(text: &str, start: usize, end: usize, new: &str) -> String {
    let mut out = String::with_capacity(text.len() - (end - start) + new.len());
    out.push_str(&text[..start]);
    out.push_str(new);
    out.push_str(&text[end..]);
    out
}

// ------------------------------------------------------------ route plumbing

fn parse(text: &str) -> Result<yamlpath::Document> {
    yamlpath::Document::new(text).map_err(|e| Error::EditTarget {
        path: "(document)".to_string(),
        detail: format!("the spec no longer parses as YAML: {e}"),
    })
}

fn route_of(path: &[PathPart]) -> yamlpath::Route<'_> {
    yamlpath::Route::from(
        path.iter()
            .map(|part| match part {
                PathPart::Key(k) => yamlpath::Component::Key(k.as_str().into()),
                PathPart::Index(i) => yamlpath::Component::Index(*i),
            })
            .collect::<Vec<_>>(),
    )
}

/// `steps[2].command` — the human-readable spelling of a path, for refusals.
fn path_display(path: &[PathPart]) -> String {
    if path.is_empty() {
        return "(root)".to_string();
    }
    let mut out = String::new();
    for part in path {
        match part {
            PathPart::Key(k) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            PathPart::Index(i) => {
                out.push('[');
                out.push_str(&i.to_string());
                out.push(']');
            }
        }
    }
    out
}

fn target_err(path: &[PathPart], detail: impl std::fmt::Display) -> Error {
    Error::EditTarget {
        path: path_display(path),
        detail: detail.to_string(),
    }
}

/// The exact byte span of the *value* at `path`. For a `key: value` pair this
/// is the value node only — the key, the `: ` and any trailing same-line
/// comment stay outside it. For a bare `key:` with no value, an empty span just
/// after the colon (with a single space of separation) is returned, so a
/// Replace inserts cleanly.
fn value_span(text: &str, doc: &yamlpath::Document, path: &[PathPart]) -> Result<(usize, usize)> {
    if path.is_empty() {
        return Err(target_err(path, "cannot replace the whole document"));
    }
    let route = route_of(path);
    match doc.query_exact(&route) {
        Ok(Some(feature)) => Ok((feature.location.byte_span.0, feature.location.byte_span.1)),
        Ok(None) => {
            // `key:` with an absent value: point just after the colon.
            let key = doc
                .query_key_only(&route)
                .map_err(|e| target_err(path, e))?;
            let after_key = key.location.byte_span.1;
            let colon = text[after_key..]
                .find(':')
                .ok_or_else(|| target_err(path, "malformed pair: no `:` after the key"))?;
            let at = after_key + colon + 1;
            Ok((at, at))
        }
        Err(e) => Err(target_err(path, e)),
    }
}

// ------------------------------------------------------- text-derived extents

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// End of the line containing `at`, *including* its `\n` (or EOF).
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i + 1)
}

fn indent_of(text: &str, line_start: usize) -> usize {
    text[line_start..]
        .bytes()
        .take_while(|b| *b == b' ')
        .count()
}

fn line_is_blank(text: &str, line_start: usize) -> bool {
    text[line_start..line_end(text, line_start)]
        .trim()
        .is_empty()
}

/// The text extent of a block element — a sequence item or a mapping pair —
/// derived from the text, not from parser node boundaries (tree-sitter extends
/// an item's node over the comments that follow it, which is exactly the wrong
/// extent for delete and reorder).
///
/// The extent is the anchor line plus every continuation line. Blank lines
/// belong to the element only when more-deeply-indented content follows them
/// (a blank line inside a `command: |` block scalar); a blank followed by a
/// sibling or a dedent ends the element.
fn element_lines(text: &str, anchor: usize) -> (usize, usize) {
    let start = line_start(text, anchor);
    let base = indent_of(text, start);
    let mut end = line_end(text, start);
    loop {
        // Look ahead over any run of blank lines to what follows them.
        let mut probe = end;
        while probe < text.len() && line_is_blank(text, probe) {
            probe = line_end(text, probe);
        }
        if probe >= text.len() || indent_of(text, probe) <= base {
            break;
        }
        end = line_end(text, probe);
    }
    (start, end)
}

/// Extend `start` (a line start) upward over the element's flush comment
/// header: contiguous `#` lines directly above with no blank line between,
/// indented no deeper than the element. A more-deeply-indented comment above
/// the anchor belongs inside the previous element's body, not to this one.
fn with_flush_header(text: &str, start: usize) -> usize {
    let base = indent_of(text, start);
    let mut s = start;
    while s > 0 {
        let prev = line_start(text, s - 1);
        let line = &text[prev..s];
        if line.trim_start().starts_with('#') && indent_of(text, prev) <= base {
            s = prev;
        } else {
            break;
        }
    }
    s
}

/// Refuse elements that share a line with anything but indentation and a
/// sequence dash — deleting "the element's lines" inside a flow collection
/// (`[a, b]` / `{a: 1, b: 2}`) would take its neighbours with it.
fn require_own_line(text: &str, path: &[PathPart], anchor: usize) -> Result<()> {
    let prefix = &text[line_start(text, anchor)..anchor];
    let stripped = prefix.trim_start_matches(' ');
    if stripped.is_empty() || stripped == "- " || stripped == "-" {
        Ok(())
    } else {
        Err(target_err(
            path,
            "the element shares a line with its neighbours (a flow collection); \
             replace the whole collection instead",
        ))
    }
}

// ------------------------------------------------------------------- the ops

/// `Add`: insert `key: value` into the mapping at `path`.
fn add_key(text: &str, path: &[PathPart], key: &str, value: &str) -> Result<String> {
    if path.is_empty() {
        // The manifest root: a new top-level entry at the end of the document.
        let mut entry = String::new();
        if !text.is_empty() && !text.ends_with('\n') {
            entry.push('\n');
        }
        entry.push_str(&format!("{}\n", block_pair(key, value)));
        return Ok(splice(text, text.len(), text.len(), &entry));
    }

    let doc = parse(text)?;
    let route = route_of(path);
    let value_feature = doc.query_exact(&route).map_err(|e| target_err(path, e))?;

    match value_feature {
        None => {
            // A bare `key:` — this entry becomes the mapping's first child.
            let pair = doc.query_pretty(&route).map_err(|e| target_err(path, e))?;
            let anchor = pair.location.byte_span.0;
            let ls = line_start(text, anchor);
            let indent = " ".repeat(indent_of(text, ls) + 2);
            let at = line_end(text, ls);
            let mut entry = format!("{indent}{}\n", block_pair(key, value));
            if at == text.len() && !text.ends_with('\n') {
                entry.insert(0, '\n');
            }
            Ok(splice(text, at, at, &entry))
        }
        Some(feature) => match feature.kind() {
            yamlpath::FeatureKind::FlowMapping => {
                let (vs, ve) = feature.location.byte_span;
                let inner = &text[vs..ve];
                let close = inner
                    .rfind('}')
                    .ok_or_else(|| target_err(path, "malformed flow mapping: no `}`"))?;
                let body = inner[..close].trim_start_matches('{').trim();
                let insert = if body.is_empty() {
                    format!("{key}: {value}")
                } else {
                    format!(", {key}: {value}")
                };
                Ok(splice(text, vs + close, vs + close, &insert))
            }
            yamlpath::FeatureKind::BlockMapping => {
                let (start, end) = element_span_at(text, &doc, path)?;
                let first_key = feature.location.byte_span.0;
                let child_indent = block_child_indent(text, start, end, first_key);
                let mut entry = format!("{}{}\n", " ".repeat(child_indent), block_pair(key, value));
                if end == text.len() && !text.ends_with('\n') {
                    entry.insert(0, '\n');
                }
                Ok(splice(text, end, end, &entry))
            }
            other => Err(target_err(
                path,
                format!("cannot add a key to a {other:?}; the target must be a mapping"),
            )),
        },
    }
}

/// `key: value` on a line of its own, or a bare `key:` when `value` is empty, so no line
/// `Add` writes ends in a space.
fn block_pair(key: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{key}:")
    } else {
        format!("{key}: {value}")
    }
}

/// The edits that make the scalar at the mapping keys `keys`, from the root, hold `value`:
/// a [`SpecEdit::Replace`] when the last key is present, and otherwise one
/// [`SpecEdit::Add`] for each missing key, the outermost first, each a bare `key:` but the
/// last. Under a flow mapping (`{ … }`) the missing keys go in as one flow value instead,
/// since a bare key inside braces takes no block child. Every key present keeps its bytes.
/// Text that does not parse as YAML has no key this can read, so it gets the `Add` of
/// each key, and applying those refuses it with the parser's reason.
pub(crate) fn scalar_edits(text: &str, keys: &[&str], value: &str) -> Vec<SpecEdit> {
    let path = |n: usize| -> Vec<PathPart> { keys[..n].iter().map(|&k| k.into()).collect() };
    let Ok(doc) = parse(text) else {
        return block_adds(keys, 0, value);
    };
    let present = (1..=keys.len())
        .rev()
        .find(|&n| doc.query_exists(&route_of(&path(n))))
        .unwrap_or(0);
    if present == keys.len() {
        // A bare `key:` is replaced right after its colon, so its value brings the space.
        let bare = matches!(doc.query_exact(&route_of(&path(present))), Ok(None));
        return vec![SpecEdit::Replace {
            path: path(present),
            value: if bare {
                format!(" {value}")
            } else {
                value.to_string()
            },
        }];
    }
    // With no key present this asks of the document's root, a block mapping in a spec.
    let under_flow = matches!(
        doc.query_exact(&route_of(&path(present))),
        Ok(Some(feature)) if feature.kind() == yamlpath::FeatureKind::FlowMapping
    );
    if under_flow {
        let value = keys[present + 1..]
            .iter()
            .rev()
            .fold(value.to_string(), |inner, key| {
                format!("{{{key}: {inner}}}")
            });
        return vec![SpecEdit::Add {
            path: path(present),
            key: keys[present].to_string(),
            value,
        }];
    }
    block_adds(keys, present, value)
}

/// One [`SpecEdit::Add`] for each of `keys` from `from` on, each a bare `key:` in the one
/// before it, and the last holding `value`.
fn block_adds(keys: &[&str], from: usize, value: &str) -> Vec<SpecEdit> {
    (from..keys.len())
        .map(|n| SpecEdit::Add {
            path: keys[..n].iter().map(|&k| k.into()).collect(),
            key: keys[n].to_string(),
            value: if n + 1 == keys.len() {
                value.to_string()
            } else {
                String::new()
            },
        })
        .collect()
}

/// `Append`: insert `item` after the last item of the sequence at `path`.
fn append_item(text: &str, path: &[PathPart], item: &str) -> Result<String> {
    let doc = parse(text)?;
    let route = route_of(path);
    let feature = doc
        .query_exact(&route)
        .map_err(|e| target_err(path, e))?
        .ok_or_else(|| {
            target_err(
                path,
                "the sequence has no items yet; Replace the empty value instead",
            )
        })?;

    match feature.kind() {
        yamlpath::FeatureKind::FlowSequence => {
            let (vs, ve) = feature.location.byte_span;
            let inner = &text[vs..ve];
            let close = inner
                .rfind(']')
                .ok_or_else(|| target_err(path, "malformed flow sequence: no `]`"))?;
            let body = inner[..close].trim_start_matches('[').trim();
            let insert = if body.is_empty() {
                item.to_string()
            } else {
                format!(", {item}")
            };
            Ok(splice(text, vs + close, vs + close, &insert))
        }
        yamlpath::FeatureKind::BlockSequence => {
            let last = item_count(&doc, path)? - 1;
            let mut item_path = path.to_vec();
            item_path.push(PathPart::Index(last));
            let anchor = doc
                .query_pretty(&route_of(&item_path))
                .map_err(|e| target_err(&item_path, e))?
                .location
                .byte_span
                .0;
            let (_, end) = element_lines(text, anchor);
            let mut block = item.to_string();
            if !block.ends_with('\n') {
                block.push('\n');
            }
            if end == text.len() && !text.ends_with('\n') {
                block.insert(0, '\n');
            }
            Ok(splice(text, end, end, &block))
        }
        other => Err(target_err(
            path,
            format!("cannot append to a {other:?}; the target must be a sequence"),
        )),
    }
}

/// `Delete`: remove the element at `path` with its flush comment header; for
/// sequence items, also the blank separator that followed it.
fn delete_element(text: &str, path: &[PathPart]) -> Result<String> {
    if path.is_empty() {
        return Err(target_err(path, "cannot delete the whole document"));
    }
    let doc = parse(text)?;
    let (start, mut end) = element_span_at(text, &doc, path)?;
    let start = with_flush_header(text, start);
    if matches!(path.last(), Some(PathPart::Index(_))) {
        // Consume the blank run that separated this item from the next, so the
        // neighbours are not left with a double gap.
        while end < text.len() && line_is_blank(text, end) {
            end = line_end(text, end);
        }
    }
    Ok(splice(text, start, end, ""))
}

/// `Reorder`: move the item at `from` in the block sequence at `path` to `to`.
fn reorder_items(text: &str, path: &[PathPart], from: usize, to: usize) -> Result<String> {
    let doc = parse(text)?;
    let n = item_count(&doc, path)?;
    if from >= n || to >= n {
        return Err(target_err(
            path,
            format!("reorder {from} -> {to} out of range for {n} item(s)"),
        ));
    }
    if from == to {
        return Ok(text.to_string());
    }

    // Lift the item (with its flush header) out…
    let (start, end) = item_chunk(text, &doc, path, from)?;
    let mut chunk = text[start..end].to_string();
    if !chunk.ends_with('\n') {
        chunk.push('\n');
    }
    let removed = splice(text, start, end, "");

    // …re-resolve against the shortened document, and put it back.
    let doc = parse(&removed)?;
    let target = if to == n - 1 {
        // Move to the end: after the last remaining item's body.
        let (_, end) = item_chunk(&removed, &doc, path, n - 2)?;
        end
    } else {
        // `Vec::remove(from)` then `Vec::insert(to)`: inserting before the
        // element now at index `to` puts the moved item at final index `to`,
        // whichever direction it travelled.
        let (start, _) = item_chunk(&removed, &doc, path, to)?;
        start
    };
    let mut block = chunk;
    if target == removed.len() && !removed.ends_with('\n') {
        block.insert(0, '\n');
    }
    Ok(splice(&removed, target, target, &block))
}

/// `Nest`: move the entries `keys` of the block mapping at `path` under `under`,
/// as the one mapping of a new block sequence.
fn nest_entries(text: &str, path: &[PathPart], keys: &[String], under: &str) -> Result<String> {
    let owned = with_final_newline(text);
    let text: &str = &owned;
    if keys.is_empty() {
        return Err(target_err(path, "the nest names no key to move"));
    }
    if under.is_empty() {
        return Err(target_err(path, "the nest names no key to move them under"));
    }
    let doc = parse(text)?;
    require_block_mapping(&doc, path, "nest the entries of")?;
    if doc.query_exists(&route_of(&child(path, under))) {
        return Err(target_err(
            path,
            format!("the mapping already holds `{under}`"),
        ));
    }
    let mut anchors = keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            if keys[..i].contains(key) {
                return Err(target_err(path, format!("the nest names `{key}` twice")));
            }
            let key_path = child(path, key.as_str());
            let pair = doc
                .query_pretty(&route_of(&key_path))
                .map_err(|e| target_err(&key_path, e))?;
            Ok(pair.location.byte_span.0)
        })
        .collect::<Result<Vec<_>>>()?;
    anchors.sort_unstable();

    let expected = described(text, path, |mapping| {
        let mut moved = serde_yaml::Mapping::new();
        for key in keys {
            // A key the loader reads as other than its text leaves a null here,
            // and the comparison below refuses the result.
            let value = mapping.remove(key.as_str()).unwrap_or_default();
            moved.insert(key.as_str().into(), value);
        }
        // `under` is written as given, as `Add` writes its key, so the key it
        // describes is what YAML reads that text as.
        mapping.insert(
            serde_yaml::from_str(under).unwrap_or_default(),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(moved)]),
        );
        Ok(())
    })?;

    // The first moved key's line: `plot:` at the mapping's column, or `- plot:`
    // when the mapping is a sequence item opening on its dash's line, whose
    // header above belongs to the item and so stays.
    let first = anchors[0];
    let first_line = line_start(text, first);
    let column = first - first_line;
    let prefix = &text[first_line..first];
    let on_dash = !prefix.bytes().all(|b| b == b' ');

    // Each moved entry's extent: its flush header, its key's line and the lines
    // of its value.
    let chunks: Vec<(usize, usize)> = anchors
        .iter()
        .enumerate()
        .map(|(i, &anchor)| {
            let line = line_start(text, anchor);
            let start = if i == 0 && on_dash {
                line
            } else {
                with_flush_header(text, line)
            };
            (start, entry_lines(text, anchor).1)
        })
        .collect();

    let mut out = String::new();
    out.push_str(&text[..chunks[0].0]);
    out.push_str(prefix);
    out.push_str(under);
    out.push_str(":\n");
    out.push_str(&indented(&text[chunks[0].0..first_line], 2));
    out.push_str(&" ".repeat(column + 2));
    out.push_str("- ");
    out.push_str(&text[first..line_end(text, first)]);
    out.push_str(&indented(&text[line_end(text, first)..chunks[0].1], 4));

    // Between two moved entries, what precedes the first entry the nest does not
    // name moves with them; that entry, with its header, and the rest of the gap
    // stay, after the new key.
    let mut stays = String::new();
    let mut at = chunks[0].1;
    for &(start, end) in &chunks[1..] {
        let split =
            first_content_line(text, at, start).map_or(start, |line| with_flush_header(text, line));
        out.push_str(&indented(&text[at..split], 4));
        stays.push_str(&text[split..start]);
        out.push_str(&indented(&text[start..end], 4));
        at = end;
    }
    out.push_str(&stays);
    out.push_str(&text[at..]);

    // The extents are read from the text, and `under` is the caller's: hold the
    // result to the document the nest describes.
    if load_value(path, &out)? != expected {
        return Err(target_err(
            path,
            "the edited text does not load to the mapping the edit describes",
        ));
    }
    Ok(out)
}

/// `Lift`: put the one mapping of the block sequence at `key` in `key`'s place.
fn lift_entry(text: &str, path: &[PathPart], key: &str) -> Result<String> {
    let owned = with_final_newline(text);
    let text: &str = &owned;
    let doc = parse(text)?;
    require_block_mapping(&doc, path, "lift into")?;
    let key_path = child(path, key);
    let sequence = doc
        .query_exact(&route_of(&key_path))
        .map_err(|e| target_err(&key_path, e))?
        .ok_or_else(|| target_err(&key_path, "the key holds no sequence to lift from"))?;
    if sequence.kind() != yamlpath::FeatureKind::BlockSequence {
        return Err(target_err(
            &key_path,
            format!(
                "cannot lift out of a {:?}; the key must hold a block sequence",
                sequence.kind()
            ),
        ));
    }
    let items = item_count(&doc, &key_path)?;
    if items != 1 {
        return Err(target_err(
            &key_path,
            format!(
                "the sequence holds {items} items and a lift takes the only one; \
                 Delete the others first"
            ),
        ));
    }
    let item_path = child(&key_path, 0);
    let item = doc
        .query_exact(&route_of(&item_path))
        .map_err(|e| target_err(&item_path, e))?
        .ok_or_else(|| target_err(&item_path, "the item is empty"))?;
    if item.kind() != yamlpath::FeatureKind::BlockMapping {
        return Err(target_err(
            &item_path,
            format!(
                "cannot lift a {:?}; the item must be a block mapping",
                item.kind()
            ),
        ));
    }
    let first = item.location.byte_span.0;
    let first_line = line_start(text, first);
    if text[first_line..first].trim() != "-" {
        return Err(target_err(
            &item_path,
            "the item's first key does not share the dash's line",
        ));
    }

    // A key of the item the mapping already holds would be written twice.
    let loaded = load_value(path, text)?;
    let mapping = mapping_at(&loaded, path);
    let lifted = mapping
        .and_then(|m| m.get(key))
        .and_then(|s| s.get(0))
        .and_then(serde_yaml::Value::as_mapping);
    if let (Some(mapping), Some(lifted)) = (mapping, lifted)
        && let Some(held) = lifted
            .keys()
            .find(|k| k.as_str() != Some(key) && mapping.contains_key(*k))
    {
        return Err(target_err(
            path,
            format!("the mapping already holds `{}`", key_display(held)),
        ));
    }

    let anchor = doc
        .query_pretty(&route_of(&key_path))
        .map_err(|e| target_err(&key_path, e))?
        .location
        .byte_span
        .0;
    let key_line = line_start(text, anchor);
    let column = anchor - key_line;
    let key_end = doc
        .query_key_only(&route_of(&key_path))
        .map_err(|e| target_err(&key_path, e))?
        .location
        .byte_span
        .1;
    // What follows `key:` on its line: nothing, or a comment kept on a line of
    // its own where the key's line was.
    let after = text[key_end..line_end(text, key_line)].trim();
    let comment = match after.strip_prefix(':').map(str::trim) {
        Some("") => None,
        Some(c) if c.starts_with('#') => Some(c),
        _ => {
            return Err(target_err(
                &key_path,
                "the key's line carries more than the key and a comment",
            ));
        }
    };

    let (_, end) = entry_lines(text, anchor);
    let dash_column = indent_of(text, first_line);
    let item_column = first - first_line;
    let mut out = String::new();
    out.push_str(&text[..key_line]);
    if let Some(comment) = comment {
        out.push_str(&" ".repeat(indent_of(text, key_line)));
        out.push_str(comment);
        out.push('\n');
    }
    out.push_str(&dedented(
        &text[line_end(text, key_line)..first_line],
        dash_column.saturating_sub(column),
    ));
    out.push_str(&text[key_line..anchor]);
    out.push_str(&text[first..line_end(text, first)]);
    out.push_str(&dedented(
        &text[line_end(text, first)..end],
        item_column - column,
    ));
    out.push_str(&text[end..]);
    Ok(out)
}

/// `path` with one more part on the end.
fn child(path: &[PathPart], part: impl Into<PathPart>) -> Vec<PathPart> {
    let mut out = path.to_vec();
    out.push(part.into());
    out
}

/// `text` ending in `\n`, so a line moved from the end of the document is a
/// whole line wherever it lands; the splice adds that newline to its result
/// regardless.
fn with_final_newline(text: &str) -> std::borrow::Cow<'_, str> {
    if text.ends_with('\n') {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(format!("{text}\n"))
    }
}

fn require_block_mapping(doc: &yamlpath::Document, path: &[PathPart], verb: &str) -> Result<()> {
    match doc
        .query_exact(&route_of(path))
        .map_err(|e| target_err(path, e))?
    {
        Some(feature) if feature.kind() == yamlpath::FeatureKind::BlockMapping => Ok(()),
        Some(feature) => Err(target_err(
            path,
            format!(
                "cannot {verb} a {:?}; the target must be a block mapping",
                feature.kind()
            ),
        )),
        None => Err(target_err(
            path,
            "the key has no value; the target must be a block mapping",
        )),
    }
}

/// What `text` loads to once `reshape` has changed the mapping at `path`: the
/// document a nest describes, built from the loaded value and not from the
/// text, so the nest's result can be held to it.
fn described(
    text: &str,
    path: &[PathPart],
    reshape: impl FnOnce(&mut serde_yaml::Mapping) -> Result<()>,
) -> Result<serde_yaml::Value> {
    let mut value = load_value(path, text)?;
    let mapping = path
        .iter()
        .try_fold(&mut value, |value, part| match part {
            PathPart::Key(k) => value.get_mut(k.as_str()),
            PathPart::Index(i) => value.get_mut(*i),
        })
        .and_then(serde_yaml::Value::as_mapping_mut)
        .ok_or_else(|| target_err(path, "the loaded document holds no mapping here"))?;
    reshape(mapping)?;
    Ok(value)
}

/// The mapping at `path` in a loaded document.
fn mapping_at<'v>(
    value: &'v serde_yaml::Value,
    path: &[PathPart],
) -> Option<&'v serde_yaml::Mapping> {
    path.iter()
        .try_fold(value, |value, part| match part {
            PathPart::Key(k) => value.get(k.as_str()),
            PathPart::Index(i) => value.get(*i),
        })
        .and_then(serde_yaml::Value::as_mapping)
}

fn load_value(path: &[PathPart], text: &str) -> Result<serde_yaml::Value> {
    serde_yaml::from_str(text)
        .map_err(|e| target_err(path, format!("the text does not load as YAML: {e}")))
}

/// A mapping key as YAML spells it, for a refusal.
fn key_display(key: &serde_yaml::Value) -> String {
    serde_yaml::to_string(key)
        .map(|s| s.trim_end().to_string())
        .unwrap_or_default()
}

/// The text extent of the mapping entry whose key starts at `anchor`: the key's
/// line and every line of its value. Unlike [`element_lines`] it measures from
/// the key's own column, so the first key of a sequence item (`- plot:`) does
/// not take its siblings with it. A `-` line at that column continues the
/// entry, since a block sequence may sit at its key's indentation; a blank
/// line, or a comment no deeper than the key, belongs to the entry only when
/// more of its value follows.
fn entry_lines(text: &str, anchor: usize) -> (usize, usize) {
    let start = line_start(text, anchor);
    let column = anchor - start;
    let mut end = line_end(text, start);
    loop {
        let mut probe = end;
        while probe != text.len()
            && (line_is_blank(text, probe) || is_comment_within(text, probe, column))
        {
            probe = line_end(text, probe);
        }
        // Past the last line there is nothing to continue the entry.
        if !continues_entry(text, probe, column) {
            break;
        }
        end = line_end(text, probe);
    }
    (start, end)
}

fn is_comment_within(text: &str, line: usize, column: usize) -> bool {
    text[line..line_end(text, line)]
        .trim_start()
        .starts_with('#')
        && indent_of(text, line) <= column
}

fn continues_entry(text: &str, line: usize, column: usize) -> bool {
    let indent = indent_of(text, line);
    let body = text[line + indent..line_end(text, line)].trim_end();
    indent > column || (indent == column && (body == "-" || body.starts_with("- ")))
}

/// The first line in `[from, to)` holding something other than a comment.
fn first_content_line(text: &str, from: usize, to: usize) -> Option<usize> {
    text[from..to]
        .split_inclusive('\n')
        .scan(from, |at, line| {
            let start = *at;
            *at += line.len();
            Some((start, line.trim()))
        })
        .find(|(_, body)| !body.is_empty() && !body.starts_with('#'))
        .map(|(start, _)| start)
}

/// `block` with `by` spaces before each line that holds anything; an empty line
/// stays empty, so no line gains trailing whitespace.
fn indented(block: &str, by: usize) -> String {
    let pad = " ".repeat(by);
    block
        .split_inclusive('\n')
        .map(|line| {
            if line.trim_end_matches(['\n', '\r']).is_empty() {
                line.to_string()
            } else {
                format!("{pad}{line}")
            }
        })
        .collect()
}

/// `block` with up to `by` leading spaces taken off each line.
fn dedented(block: &str, by: usize) -> String {
    block
        .split_inclusive('\n')
        .map(|line| {
            let n = line.bytes().take(by).take_while(|b| *b == b' ').count();
            &line[n..]
        })
        .collect()
}

/// The full text extent of the element at `path` — anchor line through its last
/// continuation line — with the flow-collection guard applied.
fn element_span_at(
    text: &str,
    doc: &yamlpath::Document,
    path: &[PathPart],
) -> Result<(usize, usize)> {
    let feature = doc
        .query_pretty(&route_of(path))
        .map_err(|e| target_err(path, e))?;
    let anchor = feature.location.byte_span.0;
    require_own_line(text, path, anchor)?;
    Ok(element_lines(text, anchor))
}

/// The extent of block-sequence item `index` under `path`, including its flush
/// comment header.
fn item_chunk(
    text: &str,
    doc: &yamlpath::Document,
    path: &[PathPart],
    index: usize,
) -> Result<(usize, usize)> {
    let mut item_path = path.to_vec();
    item_path.push(PathPart::Index(index));
    let (start, end) = element_span_at(text, doc, &item_path)?;
    Ok((with_flush_header(text, start), end))
}

/// How many items the sequence at `path` has, by probing indices.
fn item_count(doc: &yamlpath::Document, path: &[PathPart]) -> Result<usize> {
    let mut n = 0;
    loop {
        let mut probe = path.to_vec();
        probe.push(PathPart::Index(n));
        if !doc.query_exists(&route_of(&probe)) {
            break;
        }
        n += 1;
    }
    if n == 0 {
        return Err(target_err(path, "the sequence has no items"));
    }
    Ok(n)
}

/// The exact leading-space prefix of the last item of the block sequence at
/// `path` — what a new sibling item must be indented with to join it. Reads
/// the text the way the other extent helpers do, so an appended item matches
/// the document's own convention instead of imposing one. Refused when the
/// sequence has no items (there is no convention to read) or the path does
/// not resolve.
pub(crate) fn sequence_item_indent(text: &str, path: &[PathPart]) -> Result<String> {
    let doc = parse(text)?;
    let last = item_count(&doc, path)? - 1;
    let mut item_path = path.to_vec();
    item_path.push(PathPart::Index(last));
    let anchor = doc
        .query_pretty(&route_of(&item_path))
        .map_err(|e| target_err(&item_path, e))?
        .location
        .byte_span
        .0;
    let ls = line_start(text, anchor);
    Ok(" ".repeat(indent_of(text, ls)))
}

/// The indentation for a new key in the block mapping spanning
/// `[start, end)`: the column of its keys. When the first key sits on the
/// element's own line — a sequence item's `- key:` — that key's column is the
/// answer, because the line below it may be the first key's nested value
/// (`- plot:` over `    - mark: …`) rather than a sibling. Otherwise it is the
/// indent of the first child line, or two deeper than the anchor line when no
/// child line exists to read.
fn block_child_indent(text: &str, start: usize, end: usize, first_key: usize) -> usize {
    if line_start(text, first_key) == start {
        return first_key - start;
    }
    let anchor_indent = indent_of(text, start);
    let mut at = line_end(text, start);
    while at < end {
        if !line_is_blank(text, at) {
            let line = &text[at..line_end(text, at)];
            if !line.trim_start().starts_with('#') {
                return indent_of(text, at);
            }
        }
        at = line_end(text, at);
    }
    anchor_indent + 2
}

// -------------------------------------------------------------- atomic write

/// Write `bytes` to `path` atomically: a uniquely named temp file in the same
/// directory, flushed to disk, then renamed over the target. A crash or
/// interrupt at any point leaves either the old file or the new one — never a
/// truncation. (Same-directory placement is what makes the rename atomic: a
/// rename across filesystems degrades to copy-and-delete.)
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(MANIFEST_FILENAME);
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    /// A commented fixture with every structural shape the ops must respect:
    /// flush headers, a blank-separated section label, a trailing same-line
    /// comment, a block scalar with an interior blank line, and flow
    /// collections.
    const SPEC: &str = "\
name: fixture
engine: duckdb

steps:
  # Builds the input file. This header is flush: it belongs to the step.
  - name: first
    command: |
      printf 'a\\n' > data.txt

      printf 'b\\n' >> data.txt
    produces: [data]

  # A label for the back half of the pipeline.

  - name: second
    command: \"wc -l < data.txt\"   # trailing comment on the value's line
    depends_on: [data]
";

    fn p(parts: &[&str]) -> Vec<PathPart> {
        parts
            .iter()
            .map(|s| match s.parse::<usize>() {
                Ok(i) => PathPart::Index(i),
                Err(_) => PathPart::Key((*s).to_string()),
            })
            .collect()
    }

    #[test]
    fn replace_touches_only_the_value_and_keeps_the_trailing_comment() {
        let edit = SpecEdit::Replace {
            path: p(&["steps", "1", "command"]),
            value: "\"cat data.txt\"".to_string(),
        };
        let out = apply_edits(SPEC, std::slice::from_ref(&edit)).unwrap();
        let expected = SPEC.replacen("\"wc -l < data.txt\"", "\"cat data.txt\"", 1);
        assert_eq!(out.text(), expected, "only the value's bytes may change");
        assert!(
            out.text()
                .contains("# trailing comment on the value's line")
        );
    }

    #[test]
    fn rewrite_fragment_edits_inside_a_block_scalar() {
        let edit = SpecEdit::RewriteFragment {
            path: p(&["steps", "0", "command"]),
            from: "'b\\n'".to_string(),
            to: "'c\\n'".to_string(),
        };
        let out = apply_edits(SPEC, &[edit]).unwrap();
        let expected = SPEC.replacen("'b\\n'", "'c\\n'", 1);
        assert_eq!(out.text(), expected);
    }

    #[test]
    fn rewrite_fragment_refuses_a_miss_and_an_ambiguity() {
        let miss = SpecEdit::RewriteFragment {
            path: p(&["steps", "0", "command"]),
            from: "absent".to_string(),
            to: "x".to_string(),
        };
        match apply_edits(SPEC, &[miss]) {
            Err(Error::EditTarget { path, detail }) => {
                assert_eq!(path, "steps[0].command");
                assert!(detail.contains("not found"), "{detail}");
            }
            other => panic!("expected EditTarget, got {other:?}"),
        }

        let ambiguous = SpecEdit::RewriteFragment {
            path: p(&["steps", "0", "command"]),
            from: "printf".to_string(),
            to: "echo".to_string(),
        };
        match apply_edits(SPEC, &[ambiguous]) {
            Err(Error::EditTarget { detail, .. }) => {
                assert!(detail.contains("ambiguous"), "{detail}");
            }
            other => panic!("expected EditTarget, got {other:?}"),
        }
    }

    #[test]
    fn the_block_scalar_interior_blank_line_stays_inside_the_item_extent() {
        // Deleting the first step must take the whole block scalar — including
        // the blank line inside it — and stop before the section label.
        let edit = SpecEdit::Delete {
            path: p(&["steps", "0"]),
        };
        let out = apply_edits(SPEC, &[edit]).unwrap();
        assert!(!out.text().contains("printf"), "{}", out.text());
        assert!(
            out.text().contains("# A label for the back half"),
            "the blank-separated label belongs to the sequence and must stay:\n{}",
            out.text()
        );
        assert!(
            !out.text().contains("This header is flush"),
            "the flush header belongs to the deleted item and must go:\n{}",
            out.text()
        );
        assert_eq!(out.manifest().steps.len(), 1);
    }

    #[test]
    fn delete_refuses_flow_collection_elements() {
        let edit = SpecEdit::Delete {
            path: p(&["steps", "0", "produces", "0"]),
        };
        match apply_edits(SPEC, &[edit]) {
            Err(Error::EditTarget { detail, .. }) => {
                assert!(detail.contains("flow collection"), "{detail}");
            }
            other => panic!("expected EditTarget, got {other:?}"),
        }
    }

    #[test]
    fn add_derives_the_sibling_indent_and_flow_append_extends_the_bracket() {
        let out = apply_edits(
            SPEC,
            &[
                SpecEdit::Add {
                    path: p(&["steps", "1"]),
                    key: "timeout_sec".to_string(),
                    value: "30".to_string(),
                },
                SpecEdit::Append {
                    path: p(&["steps", "1", "depends_on"]),
                    item: "data2".to_string(),
                },
            ],
        );
        // `data2` is not produced by any step, but asset wiring is the
        // runner's concern; the spec gate accepts it.
        let out = out.unwrap();
        assert!(out.text().contains("    timeout_sec: 30\n"));
        assert!(out.text().contains("depends_on: [data, data2]"));
    }

    #[test]
    fn add_to_the_root_and_replace_an_absent_value() {
        let bare = "name: fixture\ndotenv:\nsteps:\n  - name: only\n    command: \"true\"\n";
        let out = apply_edits(
            bare,
            &[
                SpecEdit::Add {
                    path: vec![],
                    key: "engine".to_string(),
                    value: "duckdb".to_string(),
                },
                SpecEdit::Replace {
                    path: p(&["dotenv"]),
                    value: " [.env]".to_string(),
                },
            ],
        )
        .unwrap();
        assert!(out.text().ends_with("engine: duckdb\n"));
        assert!(out.text().contains("dotenv: [.env]\n"));
        assert_eq!(out.manifest().dotenv, vec![".env".to_string()]);
    }

    #[test]
    fn reorder_moves_the_flush_header_with_its_item() {
        let edit = SpecEdit::Reorder {
            path: p(&["steps"]),
            from: 1,
            to: 0,
        };
        let out = apply_edits(SPEC, &[edit]).unwrap();
        let names: Vec<&str> = out
            .manifest()
            .steps
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["second", "first"]);

        // Nothing was lost or invented: the same lines, re-arranged.
        let mut before: Vec<&str> = SPEC.lines().collect();
        let mut after: Vec<&str> = out.text().lines().collect();
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(
            before, after,
            "reorder must only move lines, never edit them"
        );

        // The flush header still sits directly above its item.
        let text = out.text();
        let header = text.find("This header is flush").expect("header survives");
        let item = text.find("- name: first").expect("item survives");
        assert!(header < item, "the header moved with its owning item");
    }

    #[test]
    fn a_result_that_does_not_load_is_refused_with_the_loader_reason() {
        let edit = SpecEdit::Replace {
            path: p(&["steps", "1", "name"]),
            value: "first".to_string(),
        };
        match apply_edits(SPEC, &[edit]) {
            Err(Error::ManifestValidation(msg)) => {
                assert!(msg.contains("duplicate step name"), "{msg}");
            }
            other => panic!("expected the validation gate to refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_target_is_refused_with_the_path_in_the_reason() {
        let edit = SpecEdit::Replace {
            path: p(&["steps", "9", "name"]),
            value: "x".to_string(),
        };
        match apply_edits(SPEC, &[edit]) {
            Err(Error::EditTarget { path, .. }) => assert_eq!(path, "steps[9].name"),
            other => panic!("expected EditTarget, got {other:?}"),
        }
    }

    #[test]
    fn the_final_newline_is_the_only_normalisation() {
        let unterminated = "name: fixture\nsteps:\n  - name: only\n    command: \"true\"";
        let out = apply_edits(unterminated, &[]).unwrap();
        assert_eq!(out.text(), format!("{unterminated}\n"));
    }

    /// The keys of a pin, as `arc upgrade` writes one.
    const PIN_KEYS: [&str; 4] = ["extensions", "mlpack", "v1.5.5", "linux_amd64"];
    const PIN: &str = "1d98039edd0bf1547fb8daf8f8fbce54a239759f169025cc8db9e4fda7388cb8";
    const OTHER: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    /// `text` with the mlpack pin set to [`PIN`], through the spec gate.
    fn set_pin(text: &str) -> String {
        let edits = scalar_edits(text, &PIN_KEYS, PIN);
        apply_edits(text, &edits).unwrap().text().to_string()
    }

    #[test]
    fn scalar_edits_add_each_missing_key_as_a_block_with_no_trailing_space() {
        let before = "name: p\n# the last line\n";
        assert_eq!(
            set_pin(before),
            format!("{before}extensions:\n  mlpack:\n    v1.5.5:\n      linux_amd64: {PIN}\n")
        );

        // The new key joins its siblings at their indent, four here, and each key below
        // it sits two deeper than its parent.
        let before = format!(
            "name: p\nextensions:\n    h3:\n        v1.5.5:\n            linux_amd64: {OTHER}\nsteps: []\n"
        );
        assert_eq!(
            set_pin(&before),
            before.replace(
                "steps: []",
                &format!("    mlpack:\n      v1.5.5:\n        linux_amd64: {PIN}\nsteps: []")
            )
        );

        // A pin for another platform alone gains one line beside it.
        let before =
            format!("name: p\nextensions:\n  mlpack:\n    v1.5.5:\n      osx_arm64: {OTHER}\n");
        assert_eq!(
            set_pin(&before),
            format!("{before}      linux_amd64: {PIN}\n")
        );
    }

    #[test]
    fn scalar_edits_replace_a_present_value_and_keep_its_comment() {
        let before = format!(
            "name: p\nextensions:\n  mlpack:\n    v1.5.5:\n      linux_amd64: '{OTHER}'  # the old build\n"
        );
        let edits = scalar_edits(&before, &PIN_KEYS, PIN);
        assert_eq!(
            edits,
            vec![SpecEdit::Replace {
                path: p(&PIN_KEYS),
                value: PIN.to_string()
            }]
        );
        assert_eq!(set_pin(&before), before.replace(&format!("'{OTHER}'"), PIN));

        // A bare key's value lands after its colon, and brings the space.
        let bare = "a:\n  b:\n";
        let edits = scalar_edits(bare, &["a", "b"], "x");
        assert_eq!(apply_yaml_edits(bare, &edits).unwrap(), "a:\n  b: x\n");
    }

    #[test]
    fn scalar_edits_on_text_that_does_not_parse_add_each_key_and_are_refused() {
        let broken = "name: p\nsteps: [\n";
        assert_eq!(
            scalar_edits(broken, &["a", "b"], "x"),
            vec![
                SpecEdit::Add {
                    path: vec![],
                    key: "a".to_string(),
                    value: String::new()
                },
                SpecEdit::Add {
                    path: p(&["a"]),
                    key: "b".to_string(),
                    value: "x".to_string()
                },
            ]
        );
        assert!(apply_edits(broken, &scalar_edits(broken, &PIN_KEYS, PIN)).is_err());
    }

    #[test]
    fn scalar_edits_add_the_missing_keys_to_a_flow_mapping_as_one_flow_value() {
        let before = format!("name: p\nextensions: {{h3: {{v1.5.5: {{linux_amd64: {OTHER}}}}}}}\n");
        assert_eq!(
            set_pin(&before),
            format!(
                "name: p\nextensions: {{h3: {{v1.5.5: {{linux_amd64: {OTHER}}}}}, mlpack: {{v1.5.5: {{linux_amd64: {PIN}}}}}}}\n"
            )
        );
        let empty = "name: p\nextensions: {}\n";
        assert_eq!(
            set_pin(empty),
            format!("name: p\nextensions: {{mlpack: {{v1.5.5: {{linux_amd64: {PIN}}}}}}}\n")
        );
    }

    #[test]
    fn the_ignore_list_names_a_database_only_where_it_is_inside_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let d = &dir.path().canonicalize().unwrap();
        let inside = |db: &str| path_inside(d, db);

        assert_eq!(inside("work.duckdb").as_deref(), Some("work.duckdb"));
        assert_eq!(inside("build/w.duckdb").as_deref(), Some("build/w.duckdb"));
        assert_eq!(
            inside("./a/../build/w.duckdb").as_deref(),
            Some("build/w.duckdb"),
            "resolved the way a run joins it, then written the way the list reads it"
        );
        assert_eq!(
            inside("build//w.duckdb").as_deref(),
            Some("build/w.duckdb"),
            "an empty segment is no segment"
        );
        assert_eq!(
            inside("a/b\nc.duckdb"),
            None,
            "a segment a line of the list cannot carry is not named, and does not name its parent"
        );
        assert_eq!(inside("../w.duckdb"), None, "outside the directory");
        assert_eq!(
            inside("a/../../w.duckdb"),
            None,
            "outside, by way of a child"
        );
        assert_eq!(
            inside("/elsewhere/w.duckdb"),
            None,
            "absolute, not under it"
        );
        assert_eq!(inside(""), None);
        assert_eq!(inside("."), None, "the directory itself is not a file");

        let under = d.join("w.duckdb");
        assert_eq!(
            inside(under.to_str().unwrap()).as_deref(),
            Some("w.duckdb"),
            "an absolute path under the directory is inside it"
        );

        // A directory that cannot be resolved names no absolute database: the lexical
        // look fails, and there is no real location to look under.
        assert_eq!(path_inside(&d.join("missing"), "/elsewhere/w.duckdb"), None);

        // A directory reached through a link: the run's database path is the real one.
        #[cfg(unix)]
        {
            let real = d.join("real");
            std::fs::create_dir(&real).unwrap();
            let link = d.join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert_eq!(
                path_inside(&link, real.join("build/w.duckdb").to_str().unwrap()).as_deref(),
                Some("build/w.duckdb"),
                "an absolute path under the directory's real location is inside it"
            );
        }
    }

    #[test]
    fn a_character_the_list_reads_as_a_pattern_is_escaped_and_a_newline_is_refused() {
        assert_eq!(
            escape_ignore_pattern("a b[1]*?\\c").as_deref(),
            Some("a\\ b\\[1]\\*\\?\\\\c")
        );
        assert_eq!(
            escape_ignore_pattern("plain.duckdb").as_deref(),
            Some("plain.duckdb")
        );
        assert_eq!(escape_ignore_pattern("two\nlines"), None);
    }

    #[test]
    fn the_ignore_list_text_is_anchored_and_names_the_database_its_log_and_its_spill_directory() {
        let dir = tempfile::tempdir().unwrap();
        let none = "# Written by `arc`: what a run records belongs to the machine that ran it.\n\
                    /build/.arcform/\n";
        assert_eq!(ignore_list_text(dir.path(), None, &[]), none);
        assert_eq!(
            ignore_list_text(dir.path(), Some("../out.duckdb"), &[]),
            none,
            "a database outside the directory is not named"
        );
        assert_eq!(
            ignore_list_text(dir.path(), Some("build/w.duckdb"), &[]),
            format!(
                "{none}# The database this Protocol names, its write-ahead log, and the directory \
                 DuckDB spills to beside it.\n\
                 /build/w.duckdb\n/build/w.duckdb.wal\n/build/w.duckdb.tmp/\n"
            )
        );
    }

    #[test]
    fn each_database_copied_in_is_named_with_its_log_and_with_no_spill_directory() {
        let dir = tempfile::tempdir().unwrap();
        let none = "# Written by `arc`: what a run records belongs to the machine that ran it.\n\
                    /build/.arcform/\n";
        let copied = "# Each database `arc init --from-descriptor` copied in, and its write-ahead \
                      log. A clone does not hold them: bring each to it another way.\n";
        assert_eq!(
            ignore_list_text(dir.path(), None, &["signups.duckdb", "my logs.duckdb"]),
            format!(
                "{none}{copied}/signups.duckdb\n/signups.duckdb.wal\n\
                 /my\\ logs.duckdb\n/my\\ logs.duckdb.wal\n"
            ),
            "each copied database is named with its log, escaped as the list reads it"
        );
        assert_eq!(
            ignore_list_text(dir.path(), Some("w.duckdb"), &["signups.duckdb"]),
            format!(
                "{none}# The database this Protocol names, its write-ahead log, and the directory \
                 DuckDB spills to beside it.\n\
                 /w.duckdb\n/w.duckdb.wal\n/w.duckdb.tmp/\n\
                 {copied}/signups.duckdb\n/signups.duckdb.wal\n"
            ),
            "the manifest's database and a copied one are each named"
        );
    }

    #[test]
    fn the_ignore_list_is_written_once_and_a_list_already_there_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(IGNORE_FILENAME);

        assert_eq!(
            write_ignore_list(dir.path(), None, &[]).unwrap(),
            IgnoreList::Written
        );
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, ignore_list_text(dir.path(), None, &[]));

        std::fs::write(&path, "theirs").unwrap();
        assert_eq!(
            write_ignore_list(dir.path(), None, &[]).unwrap(),
            IgnoreList::Kept
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs");
    }

    #[test]
    fn the_merge_rule_is_anchored_and_is_the_one_line_that_names_the_log() {
        let log = crate::history::LOG_FILENAME;
        assert_eq!(merge_rule_line(), format!("/{log} merge=union\n"));

        let text = merge_rule_file_text();
        let naming: Vec<&str> = text.lines().filter(|l| l.contains(log)).collect();
        assert_eq!(
            naming,
            [format!("/{log} merge=union")],
            "one line names the log, and it is the rule:\n{text}"
        );
        assert!(
            text.lines().all(|l| l.starts_with('#') || l == naming[0]),
            "everything else in the file is a comment:\n{text}"
        );
    }

    #[test]
    fn a_fresh_merge_rule_is_written_once_and_the_file_it_made_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ATTRIBUTES_FILENAME);

        assert_eq!(write_merge_rule(dir.path()).unwrap(), MergeRule::Written);
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, merge_rule_file_text().as_bytes());

        assert_eq!(write_merge_rule(dir.path()).unwrap(), MergeRule::Kept);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            written,
            "a folder that holds the rule is left byte-identical"
        );
    }

    #[test]
    fn an_attributes_file_already_there_keeps_its_lines_and_gains_the_rule() {
        let rule = merge_rule_line();
        // Two end mid-line, one of them after a CRLF line, and one is empty: the rule never
        // joins the end of their last line, and their bytes are never rewritten.
        for theirs in [
            "*.csv -diff\n".to_string(),
            "*.csv -diff".to_string(),
            "*.csv -diff\r\n# notes".to_string(),
            String::new(),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(ATTRIBUTES_FILENAME);
            std::fs::write(&path, &theirs).unwrap();

            assert_eq!(write_merge_rule(dir.path()).unwrap(), MergeRule::Added);
            let after = std::fs::read_to_string(&path).unwrap();
            assert!(after.starts_with(&theirs), "their bytes lead: {after:?}");
            let tail = &after[theirs.len()..];
            let expected_tail = if theirs.is_empty() || theirs.ends_with('\n') {
                rule.clone()
            } else {
                format!("\n{rule}")
            };
            assert_eq!(tail, expected_tail, "after {theirs:?}");
            assert_eq!(
                after.lines().last(),
                Some(rule.trim_end()),
                "the rule is the last line"
            );
        }
    }

    #[test]
    fn a_line_that_already_sets_the_logs_merge_is_left_alone() {
        let log = crate::history::LOG_FILENAME;
        // Our own line, the same line unanchored, one with more attributes, one that
        // names another driver, and one that turns merging off: each is the author's.
        for theirs in [
            format!("/{log} merge=union\n"),
            format!("{log} merge=union\n"),
            format!("*.csv -diff\n/{log}   text  merge=union\r\n"),
            format!("{log} merge=ours\n"),
            format!("/{log} -merge\n"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(ATTRIBUTES_FILENAME);
            std::fs::write(&path, &theirs).unwrap();

            assert_eq!(
                write_merge_rule(dir.path()).unwrap(),
                MergeRule::Kept,
                "{theirs:?}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), theirs);
        }
    }

    #[test]
    fn a_line_that_does_not_set_the_logs_merge_is_not_taken_for_one() {
        let log = crate::history::LOG_FILENAME;
        for theirs in [
            // A comment, a longer name, another file's merge, the log's other attributes,
            // and a directory pattern that names more than the log.
            format!("# {log} merge=union\n"),
            format!("old-{log} merge=union\n"),
            format!("{log}.bak merge=union\n"),
            "notes.txt merge=union\n".to_string(),
            format!("/{log} -diff\n"),
            format!("/{log} nomerge=union\n"),
            format!("/sub/{log} merge=union\n"),
        ] {
            assert!(!sets_the_logs_merge(theirs.as_bytes()), "{theirs:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_or_a_directory_named_as_the_attributes_file_is_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared-attributes");
        std::fs::write(&shared, "*.csv -diff\n").unwrap();
        let linked = dir.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(&shared, linked.join(ATTRIBUTES_FILENAME)).unwrap();
        assert_eq!(write_merge_rule(&linked).unwrap(), MergeRule::Kept);
        assert_eq!(std::fs::read_to_string(&shared).unwrap(), "*.csv -diff\n");

        let dangling = dir.path().join("dangling");
        std::fs::create_dir(&dangling).unwrap();
        let nowhere = dir.path().join("nowhere");
        std::os::unix::fs::symlink(&nowhere, dangling.join(ATTRIBUTES_FILENAME)).unwrap();
        assert_eq!(write_merge_rule(&dangling).unwrap(), MergeRule::Kept);
        assert!(
            !nowhere.exists(),
            "the rule was not written through the link"
        );

        let directory = dir.path().join("directory");
        std::fs::create_dir_all(directory.join(ATTRIBUTES_FILENAME)).unwrap();
        assert_eq!(write_merge_rule(&directory).unwrap(), MergeRule::Kept);
    }

    /// A file arc cannot read is one it cannot tell holds the rule, and one it cannot
    /// append to cannot gain it: each is refused with the file named and left as it was,
    /// not taken for an empty file or reported as added to.
    #[cfg(unix)]
    #[test]
    fn an_attributes_file_arc_cannot_read_or_append_to_is_refused_and_left_as_it_was() {
        use std::os::unix::fs::PermissionsExt;

        for (case, mode) in [("unreadable", 0o200), ("read-only", 0o444)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(ATTRIBUTES_FILENAME);
            let theirs = "*.csv -diff\n";
            std::fs::write(&path, theirs).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let bound = if mode == 0o200 {
                std::fs::read(&path).is_err()
            } else {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .is_err()
            };
            if !bound {
                // Running as a user the mode does not bind: there is no such file to test.
                continue;
            }

            let refused = write_merge_rule(dir.path());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let message = refused
                .err()
                .unwrap_or_else(|| panic!("{case}: the file is refused"))
                .to_string();
            assert!(
                message.contains(ATTRIBUTES_FILENAME),
                "{case}: the refusal names the file: {message}"
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                theirs,
                "{case}: the file is as it was"
            );
        }
    }

    #[test]
    fn create_spec_writes_the_spec_and_no_ignore_list() {
        let dir = tempfile::tempdir().unwrap();
        create_spec(dir.path(), &Manifest::new_project("p")).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [MANIFEST_FILENAME],
            "the list is the verbs' to write, and the library path is as it was"
        );
    }

    #[test]
    fn write_atomic_replaces_without_leaving_droppings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(MANIFEST_FILENAME);
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, b"new bytes").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new bytes");
        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, [MANIFEST_FILENAME], "no temp files left behind");
    }

    fn nest(path: &[&str], keys: &[&str], under: &str) -> SpecEdit {
        SpecEdit::Nest {
            path: p(path),
            keys: keys.iter().map(|k| k.to_string()).collect(),
            under: under.to_string(),
        }
    }

    fn lift(path: &[&str], key: &str) -> SpecEdit {
        SpecEdit::Lift {
            path: p(path),
            key: key.to_string(),
        }
    }

    /// Keys named out of order, an entry between them with a header of its own
    /// and a loose comment above that, and no final newline: the loose comment
    /// moves, the entry between and its header stay after the new key, and the
    /// last moved line is a whole line.
    #[test]
    fn a_nest_of_keys_that_are_not_adjacent_leaves_the_key_between_after_the_new_one() {
        let text = "plot: 1\n# loose\n\n# about data\ndata:\n  a: 2\n# about width\nwidth: 3";
        let out = apply_yaml_edits(text, &[nest(&[], &["width", "plot"], "vconcat")]).unwrap();
        assert_eq!(
            out,
            "vconcat:\n  - plot: 1\n    # loose\n\n    # about width\n    width: 3\n\
             # about data\ndata:\n  a: 2\n"
        );
    }

    #[test]
    fn a_flush_header_above_the_first_moved_key_heads_the_new_entry_and_lifts_back() {
        let text = "meta: 1\n# the plot\nplot:\n  - mark: x\nwidth: 3\n";
        let nested = apply_yaml_edits(text, &[nest(&[], &["plot", "width"], "vconcat")]).unwrap();
        assert_eq!(
            nested,
            "meta: 1\nvconcat:\n  # the plot\n  - plot:\n      - mark: x\n    width: 3\n"
        );
        let lifted = apply_yaml_edits(&nested, &[lift(&[], "vconcat")]).unwrap();
        assert_eq!(lifted, text);
    }

    #[test]
    fn a_sequence_at_its_keys_indentation_moves_with_the_key() {
        let text = "plot:\n- mark: x\n  # deeper\n# between\n- mark: y\nwidth: 3\n";
        let out = apply_yaml_edits(text, &[nest(&[], &["plot"], "vconcat")]).unwrap();
        assert_eq!(
            out,
            "vconcat:\n  - plot:\n    - mark: x\n      # deeper\n    # between\n    - mark: y\n\
             width: 3\n"
        );
        let lifted = apply_yaml_edits(&out, &[lift(&[], "vconcat")]).unwrap();
        assert_eq!(lifted, text);
    }

    #[test]
    fn a_lift_keeps_a_comment_on_the_keys_line_and_lifts_an_indentless_sequence() {
        let text = "vconcat:   # the stack\n- plot: 1\n  width: 3\n";
        let out = apply_yaml_edits(text, &[lift(&[], "vconcat")]).unwrap();
        assert_eq!(out, "# the stack\nplot: 1\nwidth: 3\n");

        let text = "outer:\n  vconcat:   # the stack\n    - plot: 1\n";
        let out = apply_yaml_edits(text, &[lift(&["outer"], "vconcat")]).unwrap();
        assert_eq!(out, "outer:\n  # the stack\n  plot: 1\n");
    }

    /// A mapping under a key, its first key on a line of its own below a header:
    /// the new key sits at the mapping's column, the header heads the item, and
    /// the key after the mapping is untouched; the lift puts it all back.
    #[test]
    fn a_nest_in_a_mapping_under_a_key_keeps_its_column_and_lifts_back() {
        let text = "outer:\n  # the plot\n  plot: 1\n  width: 2\nafter: 3\n";
        let nested =
            apply_yaml_edits(text, &[nest(&["outer"], &["plot", "width"], "vconcat")]).unwrap();
        assert_eq!(
            nested,
            "outer:\n  vconcat:\n    # the plot\n    - plot: 1\n      width: 2\nafter: 3\n"
        );
        let lifted = apply_yaml_edits(&nested, &[lift(&["outer"], "vconcat")]).unwrap();
        assert_eq!(lifted, text);
    }

    #[test]
    fn a_nest_refuses_an_empty_list_a_repeat_a_missing_key_and_a_flow_mapping() {
        let text = "plot: 1\nwidth: 3\nflow: {a: 1}\nbare:\n";
        for (edit, says) in [
            (nest(&["bare"], &["a"], "vconcat"), "has no value"),
            (nest(&[], &[], "vconcat"), "names no key"),
            (nest(&[], &["plot", "plot"], "vconcat"), "`plot` twice"),
            (nest(&[], &["plot"], ""), "names no key to move them under"),
            (nest(&["flow"], &["a"], "vconcat"), "FlowMapping"),
        ] {
            match apply_yaml_edits(text, &[edit]) {
                Err(Error::EditTarget { detail, .. }) => {
                    assert!(detail.contains(says), "{says:?} in {detail}")
                }
                other => panic!("expected EditTarget, got {other:?}"),
            }
        }
        match apply_yaml_edits(text, &[nest(&[], &["plot", "height"], "vconcat")]) {
            Err(Error::EditTarget { path, .. }) => assert_eq!(path, "height"),
            other => panic!("expected EditTarget, got {other:?}"),
        }
    }

    #[test]
    fn a_lift_refuses_what_it_cannot_put_back_whole() {
        for (text, path, says) in [
            ("vconcat: 1\n", &[][..], "Scalar"),
            ("vconcat:\n  - 1\n", &[], "Scalar"),
            (
                "vconcat:\n  -\n    plot: 1\n",
                &[],
                "does not share the dash's line",
            ),
            (
                "plot: 0\nvconcat:\n  - plot: 1\n",
                &[],
                "already holds `plot`",
            ),
            ("vconcat: &a\n  - plot: 1\n", &[], "more than the key"),
            ("meta: 1\n", &["meta"], "must be a block mapping"),
        ] {
            match apply_yaml_edits(text, &[lift(path, "vconcat")]) {
                Err(Error::EditTarget { detail, .. }) => {
                    assert!(detail.contains(says), "{text:?}: {says:?} in {detail}")
                }
                other => panic!("{text:?}: expected EditTarget, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_lift_of_an_item_that_holds_the_lifted_key_puts_that_key_back() {
        let out = apply_yaml_edits("vconcat:\n  - vconcat: 1\n", &[lift(&[], "vconcat")]).unwrap();
        assert_eq!(out, "vconcat: 1\n");
    }

    /// Text the parser reads and the loader refuses, a key written twice, is
    /// refused with the loader's reason before a line moves.
    #[test]
    fn a_nest_or_lift_of_text_the_loader_refuses_is_refused_with_its_reason() {
        for (text, edit) in [
            (
                "plot: 1\nplot: 2\nwidth: 3\n",
                nest(&[], &["width"], "vconcat"),
            ),
            (
                "plot: 1\nplot: 2\nvconcat:\n  - width: 3\n",
                lift(&[], "vconcat"),
            ),
        ] {
            match apply_yaml_edits(text, &[edit]) {
                Err(Error::EditTarget { detail, .. }) => {
                    assert!(
                        detail.contains("the text does not load as YAML"),
                        "{detail}"
                    )
                }
                other => panic!("expected EditTarget, got {other:?}"),
            }
        }
    }

    /// A new key YAML reads as a comment leaves text that loads, to a sequence
    /// at the root rather than the mapping the nest describes; the edit is
    /// refused rather than returned.
    #[test]
    fn a_nest_whose_text_loads_to_another_document_is_refused() {
        let text = "plot: 1\nwidth: 3\n";
        match apply_yaml_edits(text, &[nest(&[], &["plot", "width"], "#stack")]) {
            Err(Error::EditTarget { detail, .. }) => assert!(
                detail.contains("does not load to the mapping the edit describes"),
                "{detail}"
            ),
            other => panic!("expected EditTarget, got {other:?}"),
        }
    }

    /// An explicit key's value sits on a `:` line the text reading does not
    /// take with the key; the moved text does not load, and is refused.
    #[test]
    fn a_nest_of_an_explicit_key_is_refused() {
        let text = "? plot\n: [a]\nwidth: 3\n";
        match apply_yaml_edits(text, &[nest(&[], &["plot"], "vconcat")]) {
            Err(Error::EditTarget { detail, .. }) => {
                assert!(detail.contains("does not load as YAML"), "{detail}")
            }
            other => panic!("expected EditTarget, got {other:?}"),
        }
    }

    #[test]
    fn a_new_key_is_written_as_given_and_compared_as_yaml_reads_it() {
        let out = apply_yaml_edits("plot: 1\n", &[nest(&[], &["plot"], "1")]).unwrap();
        assert_eq!(out, "1:\n  - plot: 1\n");
    }
}
