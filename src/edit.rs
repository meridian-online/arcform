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
//! # Creating is not editing
//!
//! A spec authored from scratch has no prior authorship to protect, so
//! [`create_spec`] serialises a [`Manifest`] directly — none of the splicing
//! machinery is involved. It still passes the same validation gate and the same
//! atomic write, and it refuses to overwrite an existing spec: once a file
//! exists it may have been hand-edited, and edits go through [`edit_spec`].

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
/// splice cannot reach disk: every application ends at the
/// [`Manifest::from_yaml_str`] gate.
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
    /// child. Refused if the target is not a mapping.
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
    let mut text = original.to_string();
    for edit in edits {
        text = apply_one(&text, edit)?;
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let manifest = Manifest::from_yaml_str(&text)?;
    Ok(ValidatedSpec { text, manifest })
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

// ------------------------------------------------------------------- splicing

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
        entry.push_str(&format!("{key}: {value}\n"));
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
            let mut entry = format!("{indent}{key}: {value}\n");
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
                let child_indent = block_child_indent(text, start, end);
                let mut entry = format!("{}{key}: {value}\n", " ".repeat(child_indent));
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

/// The indentation for a new key in the block mapping spanning
/// `[start, end)`: the indent of its first existing child line, or two deeper
/// than the anchor line when no child line exists to read.
fn block_child_indent(text: &str, start: usize, end: usize) -> usize {
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
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
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
}
