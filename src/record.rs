//! Recording an exploration as a step: the promotion half of the write path.
//!
//! # What this is for
//!
//! A viewer sitting on top of a protocol lets a person *explore* — filter,
//! select, derive — as transient queries over tables the pipeline has already
//! materialised. Exploration is free precisely because it is not durable.
//! The moment the person decides an exploration should become part of the
//! protocol, something has to translate it into the durable document: a SQL
//! file under `models/` and a step in `arcform.yaml` that names it. That
//! translation is [`record_step`], and it lives here — beside the splice
//! machinery in [`edit`](crate::edit) — so every tool records the same way
//! `arc` itself would.
//!
//! **Recording never runs anything.** What the person saw while exploring was
//! a query; what they recorded is a promise; only `arc run` makes the promise
//! true. This module writes files and returns — it opens no protocol's
//! database, runs none of a step's SQL, and fabricates no run state. The one
//! database it opens is an empty in-memory one, on which DuckDB parses a value
//! or a statement and runs none of it: see `duckdb_statement_count` and
//! [`recognise`]. A freshly recorded step is a step that has never run, and any
//! tool showing run state must say so.
//!
//! # Ownership: the marker is the license to regenerate
//!
//! A hand-written model carries authorship — comments, formatting, reasoning —
//! that a machine rewrite would destroy. A machine-written model carries none.
//! The line between them is drawn in the file itself: every file this module
//! writes opens with the one-line [`GENERATED_MARKER`] header, and only a file
//! that carries the header may be rewritten by [`amend_step_sql`]. A file
//! without it is refused with [`Error::HandAuthoredSql`], and the refusal
//! offers the remedy: record a *new* step downstream instead of rewriting
//! bytes this tool did not author. The manifest side needs no marker, because
//! manifest edits go through the splice path, which never rewrites untargeted
//! bytes in the first place.
//!
//! That same line settles what local history owes a regenerated model:
//! nothing, and permanently nothing. The marker marks the *absence* of
//! authorship, so the bytes an amend replaces carry none to lose, and they
//! are recoverable without a snapshot — a generated model is a function of
//! the manifest step that names it and the exploration it was recorded from.
//! Local history therefore snapshots the authored artifact, the manifest; a
//! generated model under `models/` is a recorded non-goal for that store, not
//! a gap in it. Whole-tree history is version control's tier, which the
//! [`history`](crate::history) store deliberately does not duplicate — the
//! internal `checkpoint` seam carries the full rationale.
//!
//! # Refusal discipline
//!
//! Every refusal leaves the protocol directory untouched, byte for byte. The
//! step name, provenance note and description are gated first — each is
//! spliced into durable text, so a name or note that would not read back as
//! itself (a newline, a `#`, a `:`), and a description that is empty or spans
//! lines, is refused before anything else happens, and the reloaded document
//! is checked to carry exactly the step that was asked for, its description
//! included.
//! The manifest splice is applied and gated **in memory first**; the generated
//! model is written only where no file exists; the manifest write is atomic;
//! and if a write fails partway, the just-written model — and `models/`
//! itself, when this promotion created it — is removed so no orphan survives
//! a failed promotion.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use crate::edit::{
    PathPart, SpecEdit, ValidatedSpec, apply_edits, sequence_item_indent, write_atomic,
};
use crate::error::{Error, Result};
use crate::manifest::{MANIFEST_FILENAME, Manifest};

/// The one-line header every generated model file opens with. The text after
/// the marker is the caller's provenance note — which tool wrote the file and
/// what interaction it captured. Presence of the marker on the first line is
/// what licenses [`amend_step_sql`] to rewrite the file; see the module docs.
pub const GENERATED_MARKER: &str = "-- generated:";

/// Whether `sql` carries the [`GENERATED_MARKER`] on its first line — i.e.
/// whether the record path is licensed to rewrite it. Leading whitespace on
/// the marker line is tolerated; a marker anywhere past the first line is not
/// a marker, it is a comment.
#[must_use]
pub fn sql_is_generated(sql: &str) -> bool {
    sql.lines()
        .next()
        .is_some_and(|line| line.trim_start().starts_with(GENERATED_MARKER))
}

/// An exploration ready to become a step — plain data, inspectable before it
/// touches any file, like [`SpecEdit`] before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedStep {
    /// The step's name in the manifest. Must be unique among the steps; the
    /// spec gate refuses a duplicate before anything is written. It is spliced
    /// into the manifest as a plain, unquoted YAML scalar, so it must read
    /// back as exactly itself: names carrying newlines or other control
    /// characters, `#`, `:`, surrounding whitespace, or a leading YAML
    /// indicator are refused up front — see [`record_step`].
    pub name: String,

    /// The SQL body the exploration compiled to. Written verbatim under the
    /// marker line; a missing final newline is added, nothing else is touched.
    pub sql: String,

    /// A one-line note of where this came from — tool, verb, target — recorded
    /// after the marker. Refused if it spans lines, because the marker header
    /// is one line by contract.
    pub provenance: String,

    /// The step's `description:` — one line saying what it does and why — or
    /// `None` for a step with none, which is spliced exactly as before the field
    /// existed. Written after `sql:`, as a plain scalar where that reads back as
    /// exactly itself and single-quoted where it would not. Refused if it is
    /// empty, spans lines or holds another control character.
    pub description: Option<String>,
}

/// Promote an exploration into the protocol at `dir`: write its SQL as a new
/// numbered model and splice a step naming it onto the end of `steps`.
///
/// The generated model lands at `models/NN_<name>.sql`, where `NN` continues
/// the highest number-prefixed model already present (a hand-authored,
/// unnumbered model neither collides nor moves). The spliced step carries
/// `name:` and `sql:`, then `description:` when the step has one, and nothing
/// else — inputs and outputs are discovered
/// from the SQL itself at load, exactly as they are for a hand-written step,
/// so the recorded step is indistinguishable in shape from one typed in an
/// editor.
///
/// Order of operations is refusal-first: the splice is applied and gated in
/// memory before any write, the model is written only where no file exists,
/// and the manifest write is atomic — with the model removed again if that
/// last write fails. A refusal at any point leaves the directory untouched.
///
/// Returns the model's path relative to `dir` (as the manifest cites it) and
/// the validated spec now on disk.
///
/// # Errors
///
/// [`Error::ManifestNotFound`] when `dir` has no spec;
/// [`Error::EditTarget`] when the step name would not splice faithfully
/// (empty, control characters, `#`, `:`, surrounding whitespace, a leading
/// YAML indicator — or, as the structural backstop, any name the reloaded
/// document does not read back verbatim), when `steps` is missing, when the
/// provenance note spans lines, or when the description
/// is empty, spans lines, holds another control character or does not read
/// back verbatim;
/// [`Error::GeneratedSqlExists`] when the model path is already occupied; the
/// loader's own error when the spliced result would not load — a duplicate
/// step name, most commonly.
pub fn record_step(dir: &Path, step: &RecordedStep) -> Result<(PathBuf, ValidatedSpec)> {
    valid_step_name(&step.name)?;
    one_line(&step.provenance)?;
    if let Some(description) = &step.description {
        valid_description(description)?;
    }
    record_gated(dir, step)
}

/// [`record_step`] past its gates on the name, the provenance note and the
/// description: the splice, the read-back check and the writes. The gates
/// refuse every value they can name, so a test reaches the read-back check
/// only by calling this with a value they would have refused.
fn record_gated(dir: &Path, step: &RecordedStep) -> Result<(PathBuf, ValidatedSpec)> {
    write_recorded(dir, step, &model_contents(&step.provenance, &step.sql))
}

/// [`record_gated`]'s order — the splice applied and checked in memory, the
/// model written where no file exists, the manifest written atomically, the
/// model removed if that write fails — with the bytes of the model given as
/// `contents`. Of `step` it reads the name and the description: its `sql` and
/// `provenance` are not written, so a caller that writes other bytes than
/// [`model_contents`] makes puts them in `contents` and leaves those two empty.
fn write_recorded(
    dir: &Path,
    step: &RecordedStep,
    contents: &str,
) -> Result<(PathBuf, ValidatedSpec)> {
    let manifest_path = dir.join(MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Err(Error::ManifestNotFound);
    }
    let original = std::fs::read_to_string(&manifest_path).map_err(|e| Error::FileRead {
        path: manifest_path.clone(),
        source: e,
    })?;
    let steps_before = Manifest::from_yaml_str(&original)?.steps.len();

    // Where the model will land. Create mode: an occupied path is refused,
    // never overwritten — an existing file may carry authorship.
    let models_abs = dir.join("models");
    let filename = format!(
        "{:02}_{}.sql",
        next_model_number(&models_abs),
        filename_slug(&step.name)
    );
    let sql_cited = format!("models/{filename}");
    let sql_rel = Path::new("models").join(&filename);
    let sql_abs = models_abs.join(&filename);
    if sql_abs.exists() {
        return Err(Error::GeneratedSqlExists(sql_abs));
    }

    // The manifest splice, applied and gated in memory FIRST: a refusal here
    // means nothing has touched the disk. The new item copies the indentation
    // of the last existing step, so the appended lines match the document's
    // own convention.
    let steps_path = vec![PathPart::Key("steps".to_string())];
    let item = |indent: &str| {
        let mut item = format!(
            "{indent}- name: {}\n{indent}  sql: {sql_cited}\n",
            step.name
        );
        if let Some(description) = &step.description {
            item.push_str(&format!(
                "{indent}  description: {}\n",
                description_scalar(description)
            ));
        }
        item
    };
    // A Protocol with no steps yet, as `arc create-protocol` writes it
    // (`steps: []`), has no step to copy the indentation of: its empty value is
    // replaced by a block sequence holding the new step, indented two spaces.
    // A manifest with no `steps` key at all is refused by the splice.
    let edit = if steps_before == 0 {
        SpecEdit::Replace {
            path: steps_path,
            value: format!("\n{}", item("  ").trim_end_matches('\n')),
        }
    } else {
        SpecEdit::Append {
            item: item(&sequence_item_indent(&original, &steps_path)?),
            path: steps_path,
        }
    };
    let validated = apply_edits(&original, &[edit])?;

    // The reloaded document is the arbiter: see [`reads_back`].
    reads_back(validated.manifest(), steps_before, step, &sql_cited)?;

    // Both writes are now committed to. The checkpoint seam fires before the
    // first byte changes on disk.
    checkpoint(dir);

    let models_dir_created = !models_abs.exists();
    std::fs::create_dir_all(&models_abs)?;
    if let Err(e) = write_atomic(&sql_abs, contents.as_bytes()) {
        remove_orphan_model(&sql_abs, &models_abs, models_dir_created);
        return Err(e);
    }

    if let Err(e) = validated.write_to(dir) {
        // Take the model back out: a failed promotion leaves no orphan.
        remove_orphan_model(&sql_abs, &models_abs, models_dir_created);
        return Err(e);
    }
    Ok((sql_rel, validated))
}

/// Rewrite the SQL of the recorded step named `step_name` — permitted only
/// when the file on disk carries the [`GENERATED_MARKER`], which is the
/// license to regenerate. The file is replaced wholesale (marker line with the
/// new provenance, then the new body): a generated file has no authorship to
/// preserve, so there is nothing to splice around.
///
/// The manifest is not touched — the step still names the same file. Manifest-
/// side amendments (rename, reorder, delete, preconditions) are [`SpecEdit`]s
/// and go through the splice path as ever.
///
/// Returns the rewritten model's path relative to `dir`.
///
/// # Errors
///
/// [`Error::EditTarget`] when no step has that name, when the step is a
/// `command:`/`op:` step (no SQL to amend — record a new downstream step
/// instead), or when the provenance note spans lines;
/// [`Error::SqlFileNotFound`] when the manifest cites a file that is not
/// there; [`Error::HandAuthoredSql`] when the file lacks the marker — the
/// ownership refusal, with the downstream-step remedy in its message.
pub fn amend_step_sql(dir: &Path, step_name: &str, sql: &str, provenance: &str) -> Result<PathBuf> {
    one_line(provenance)?;

    let manifest = Manifest::load(dir)?;
    let Some(step) = manifest.steps.iter().find(|s| s.name == step_name) else {
        return Err(Error::EditTarget {
            path: "steps".to_string(),
            detail: format!("no step named '{step_name}'"),
        });
    };
    let Some(sql_rel) = step.sql.as_deref() else {
        return Err(Error::EditTarget {
            path: format!("steps.{step_name}"),
            detail: format!(
                "step '{step_name}' has no sql: file — its recipe was not machine-generated; \
                 record a new step downstream of it instead"
            ),
        });
    };

    let sql_abs = dir.join(sql_rel);
    let current = std::fs::read_to_string(&sql_abs).map_err(|_| Error::SqlFileNotFound {
        step: step_name.to_string(),
        path: sql_abs.clone(),
    })?;
    if !sql_is_generated(&current) {
        return Err(Error::HandAuthoredSql {
            step: step_name.to_string(),
            path: sql_abs,
        });
    }

    checkpoint(dir);
    write_atomic(&sql_abs, model_contents(provenance, sql).as_bytes())?;
    Ok(PathBuf::from(sql_rel))
}

// ------------------------------------------------------------------ internals

/// The seam every durable write passes through first. The local-history
/// store plugs in **above** this line, not inside it:
/// [`record_step_with_history`](crate::history::record_step_with_history)
/// snapshots the manifest before calling in here, refusing the write when
/// the snapshot cannot land. The bare road keeps this a no-op so the
/// ordering stays visible — the hook fires before the first byte moves —
/// and stays history-free on purpose for callers that bring their own net.
///
/// **Regenerating a model is outside that net, and permanently so.** The
/// rewrite in [`amend_step_sql`] passes through here too and takes no
/// snapshot of the bytes it replaces — by decision, not by omission. Three
/// facts settle it as a non-goal rather than a gap:
///
/// - Amend only ever rewrites a file carrying the [`GENERATED_MARKER`]; a
///   hand-authored model is refused with [`Error::HandAuthoredSql`]. So the
///   bytes it replaces hold no human authorship — no comments, reasoning or
///   formatting — and there is nothing irreplaceable for a snapshot to keep.
/// - A generated model is a *derivative*: it is a function of the manifest
///   step that names it and the exploration it was recorded from. The net
///   already snapshots the authored artifact — the manifest — and a manifest
///   restore brings the step back; the model regenerates from there. A model
///   snapshot would be a second copy of what the first already determines.
/// - Whole-tree history — every `models/*.sql` beside the spec — is version
///   control's tier, the one the local-history store deliberately does not
///   duplicate or automate (see the tiers in [`history`](crate::history)).
///   Bending a single-spec store into a working-tree mirror would erase the
///   line those tiers are drawn along.
///
/// Amend also leaves the manifest untouched — the step still names the same
/// file — so there is no manifest state for it to checkpoint either. Hence
/// `amend_step_sql` takes no history handle and there is no
/// `amend_step_sql_with_history` road; the frozen public surface in
/// `tests/public_surface.rs` is what holds it that way.
fn checkpoint(_dir: &Path) {}

/// Marker line plus body, with the single permitted normalisation: a missing
/// final newline is added.
fn model_contents(provenance: &str, sql: &str) -> String {
    let mut contents = format!("{GENERATED_MARKER} {provenance}\n{sql}");
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents
}

/// Characters YAML reserves as indicators: a plain scalar cannot open with
/// one, so a name that does would not read back as itself.
const YAML_INDICATORS: &str = "-?:,[]{}#&*!|>'\"%@`";

/// The step name is spliced into the manifest as a plain, unquoted YAML
/// scalar, so only a name YAML reads back exactly as written may pass —
/// anything else could smuggle structure into the durable document: a newline
/// injects manifest fields or whole steps, a `#` silently truncates the name
/// into a comment, a `:` opens a mapping. The refusals here are the clear,
/// named ones; the faithfulness check after the splice is the structural
/// backstop for anything not enumerated.
fn valid_step_name(name: &str) -> Result<()> {
    let refuse = |detail: String| {
        Err(Error::EditTarget {
            path: "(name)".to_string(),
            detail,
        })
    };
    if name.is_empty() {
        return refuse("the step name is empty".to_string());
    }
    if name.chars().any(char::is_control) {
        return refuse(format!(
            "the step name {name:?} contains a control character — a newline here would \
             inject fields or steps into the manifest"
        ));
    }
    if name != name.trim() {
        return refuse(format!(
            "the step name {name:?} has leading or trailing whitespace, which YAML would \
             silently drop"
        ));
    }
    if name.contains('#') {
        return refuse(format!(
            "the step name {name:?} contains '#', which YAML reads as a comment — the \
             recorded name would be silently truncated"
        ));
    }
    if name.contains(':') {
        return refuse(format!(
            "the step name {name:?} contains ':', which YAML reads as a mapping"
        ));
    }
    if name.starts_with(|c: char| YAML_INDICATORS.contains(c)) {
        return refuse(format!(
            "the step name {name:?} opens with a YAML indicator character, so it would \
             not read back as itself"
        ));
    }
    Ok(())
}

/// Take back the model-side writes of a promotion whose later write failed:
/// the just-written model is removed, and `models/` itself is removed when
/// this promotion created it — but only when nothing else has since landed in
/// it (removing a non-empty directory fails, and that failure is deliberately
/// ignored). A failed promotion thereby leaves the directory as it found it.
fn remove_orphan_model(sql_abs: &Path, models_abs: &Path, models_dir_created: bool) {
    let _ = std::fs::remove_file(sql_abs);
    if models_dir_created {
        let _ = std::fs::remove_dir(models_abs);
    }
}

/// The provenance note becomes the marker line, and the marker line is one
/// line; a note that spans lines would smuggle its tail into the SQL body.
fn one_line(provenance: &str) -> Result<()> {
    if provenance.contains('\n') || provenance.contains('\r') {
        return Err(Error::EditTarget {
            path: "(provenance)".to_string(),
            detail: "the provenance note must be a single line — it becomes the generated \
                     file's marker header"
                .to_string(),
        });
    }
    Ok(())
}

/// Whether `manifest`, reloaded from the spliced text, carries exactly the step
/// that was asked for: one step more than the `steps_before` it had, the last
/// of them named `step.name`, citing `sql_cited` and carrying `step.description`.
/// The name and description gates refuse the constructions they can name; this
/// refuses the ones they cannot — any value that parses but records something
/// other than itself.
fn reads_back(
    manifest: &Manifest,
    steps_before: usize,
    step: &RecordedStep,
    sql_cited: &str,
) -> Result<()> {
    let last = manifest.steps.last();
    let faithful = manifest.steps.len() == steps_before + 1
        && last
            .is_some_and(|last| last.name == step.name && last.sql.as_deref() == Some(sql_cited));
    if !faithful {
        return Err(Error::EditTarget {
            path: "(name)".to_string(),
            detail: format!(
                "the step name {:?} does not record faithfully — spliced into the manifest \
                 it reads back as something other than itself; use a plain single-line name",
                step.name
            ),
        });
    }
    if last.and_then(|last| last.description.as_deref()) != step.description.as_deref() {
        return Err(Error::EditTarget {
            path: "(description)".to_string(),
            detail: format!(
                "the description {:?} does not record faithfully — spliced into the manifest \
                 it reads back as something other than itself",
                step.description.as_deref().unwrap_or_default()
            ),
        });
    }
    Ok(())
}

/// Refuse a description that is not one line of text: empty or blank, holding
/// a line break — which YAML also reads in `\u{85}`, `\u{2028}` and
/// `\u{2029}` — or holding another control character. Each refusal names why.
fn valid_description(description: &str) -> Result<()> {
    let refuse = |detail: String| {
        Err(Error::EditTarget {
            path: "(description)".to_string(),
            detail,
        })
    };
    if description.trim().is_empty() {
        return refuse(
            "the description is empty — give one line saying what the step does and why, \
             or leave it out"
                .to_string(),
        );
    }
    if description
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'))
    {
        return refuse(format!(
            "the description {description:?} spans lines — a step's description is one line"
        ));
    }
    if description.chars().any(char::is_control) {
        return refuse(format!(
            "the description {description:?} contains a control character — a step's \
             description is one line of text"
        ));
    }
    Ok(())
}

/// `description` as the YAML scalar a step's `description:` is written with: the
/// text itself where YAML reads it back as exactly that string, and otherwise
/// single-quoted, its own `'` doubled — so a `:`, a `#`, a quote, a leading
/// indicator, surrounding spaces or a word YAML reads as another type (`true`,
/// `~`, `12`) read back as written.
fn description_scalar(description: &str) -> String {
    let plain = serde_yaml::from_str::<serde_yaml::Value>(&format!("d: {description}\n"))
        .ok()
        .and_then(|doc| doc.get("d").cloned());
    if plain == Some(serde_yaml::Value::String(description.to_string())) {
        description.to_string()
    } else {
        format!("'{}'", description.replace('\'', "''"))
    }
}

/// The step name, made safe for a filename: anything outside `[A-Za-z0-9_-]`
/// becomes `_`. The manifest keeps the real name; only the file on disk is
/// slugged. A name with nothing usable in it degrades to `step`.
fn filename_slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if slug.chars().all(|c| c == '_') {
        "step".to_string()
    } else {
        slug
    }
}

/// One past the highest `NN_`-prefixed model in `models_dir` — `1` when the
/// directory is empty, absent, or holds only unnumbered (hand-named) models.
/// Numbering continues rather than fills gaps, so a deleted recording's number
/// is never silently reused.
fn next_model_number(models_dir: &Path) -> u32 {
    let mut max = 0;
    if let Ok(entries) = std::fs::read_dir(models_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty()
                && name[digits.len()..].starts_with('_')
                && let Ok(n) = digits.parse::<u32>()
            {
                max = max.max(n);
            }
        }
    }
    max + 1
}

// ------------------------------------------------------------ the catalogue

/// The long name of the filter operation. The one place the name is written, so
/// renaming the operation is a change to this line alone.
const FILTER_ROWS: &str = "filter-rows";

/// The long name of the sort operation. The one place the name is written, as
/// [`FILTER_ROWS`] is. The operation is called `sort-rows` though its argument's
/// field reads `order by`: the verb comes first, the clause is what it writes.
const SORT_ROWS: &str = "sort-rows";

/// The long name of the operation that hides a column, written once as
/// [`FILTER_ROWS`] is.
const HIDE_COLUMN: &str = "hide-column";

/// The long name of the operation that renames a column, written once as
/// [`FILTER_ROWS`] is.
const RENAME_COLUMN: &str = "rename-column";

/// The long name of the operation that adds a column computed from a SQL
/// expression, written once as [`FILTER_ROWS`] is.
const DERIVE_COLUMN: &str = "derive-column";

/// The long name of the operation that converts a column to another type,
/// written once as [`FILTER_ROWS`] is.
const CAST_COLUMN: &str = "cast-column";

/// The long name of the operation that summarises the rows of a table, in groups
/// or as a whole, written once as [`FILTER_ROWS`] is.
const SUMMARISE_ROWS: &str = "summarise-rows";

/// The long name of the operation that keeps one row of each set of duplicates,
/// written once as [`FILTER_ROWS`] is.
const DEDUPE_ROWS: &str = "dedupe-rows";

/// The long name of the operation that keeps a given number of a table's rows,
/// the first in an order or as they come, written once as [`FILTER_ROWS`] is.
const LIMIT_ROWS: &str = "limit-rows";

/// The long name of the operation that fills a column's missing values, written
/// once as [`FILTER_ROWS`] is.
const FILL_NULLS: &str = "fill-nulls";

/// One SQL operation arc holds: what it is called, what it does, what it reads
/// and writes, and what it takes — described so that it can be recorded as a
/// step by name. The catalogue holds the description and nothing that runs.
pub(crate) struct Operation {
    /// The operation's identity: lower case, hyphenated, the verb first.
    pub(crate) long_name: &'static str,
    /// One sentence saying what the operation does.
    pub(crate) summary: &'static str,
    /// Each thing the operation reads, in the order a caller names them: a
    /// table first.
    reads: &'static [Role],
    /// Each thing the operation's step writes.
    writes: &'static [Role],
    /// The operation's arguments as a JSON Schema.
    parameters: fn() -> serde_json::Value,
    /// The step's SQL, written from a recording whose arguments `parameters`
    /// has already admitted.
    sql: fn(&Recording) -> String,
    /// How a SQL statement is read as the operation, or `None` when the
    /// statement is not it. `None` in place of the reader for an operation no
    /// statement is read as yet.
    recognised: Option<fn(&Statement) -> Option<Reading>>,
    /// A check of the arguments taken together, run by [`Operation::admit`] once
    /// each value has passed its own, or `None` when each value checked alone is
    /// enough. It is for a gate no one value can be held to: [`DEDUPE_ROWS`]'
    /// `key` and `order_by` are written into one window, and are probed there
    /// together.
    checks: Option<Checks>,
}

/// A check of an operation's arguments taken together: `Ok`, or the refusal.
type Checks = fn(&serde_json::Map<String, serde_json::Value>) -> Result<()>;

/// What a statement read as an operation is read to: the table the operation
/// is applied to, and its arguments.
type Reading = (String, serde_json::Map<String, serde_json::Value>);

/// What an operation's SQL is written from: the new step's name, which is also
/// the name of the table the step makes; the table the operation is applied to,
/// as given; and the operation's arguments.
pub(crate) struct Recording<'a> {
    name: &'a str,
    on: &'a str,
    arguments: &'a serde_json::Map<String, serde_json::Value>,
}

impl Recording<'_> {
    /// The string argument `key`, or `""` when it is absent. The arguments are
    /// checked against the operation's `parameters` before its SQL is written, so
    /// a required string is present by then.
    fn text(&self, key: &str) -> &str {
        self.arguments
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
    }

    /// The string argument `key` as a column name, written as [`column_ident`]
    /// writes one: bare where DuckDB reads it bare as that column, quoted
    /// otherwise. A column argument is a name, not SQL, so this is the one way
    /// it reaches the step.
    fn column(&self, key: &str) -> String {
        column_ident(self.text(key))
    }
}

/// One thing a step reads or writes, named by the role it plays. An operation
/// and an operator describe what they read and write in this one shape.
///
/// A role is not an argument, so it is absent from the `parameters` schema. An
/// operation's is supplied by where the operation is asked from — the node in
/// view, the column under a cursor; an operator's is a key of its `with:` block,
/// taken out of `parameters` and described here instead.
pub(crate) struct Role {
    /// The role's name, unique among what the step reads, and among what it
    /// writes. An operator's is the `with:` key it is set under.
    pub(crate) name: &'static str,
    /// What kind of thing fills it: `table`, `file`, `directory` or `pattern`,
    /// the kinds of asset arc records; `column`; or `any`, for a role whose kind
    /// each value decides by how its name is written.
    pub(crate) kind: &'static str,
    /// Whether it is filled by a list of values rather than by one.
    pub(crate) list: bool,
    /// Whether a step cannot be recorded without it.
    pub(crate) required: bool,
    /// The `name` of an earlier role this one belongs to: a column is a column
    /// of a table. Set on a `column` role alone.
    pub(crate) of: Option<&'static str>,
    /// The fewest values a list takes. Set on a list alone.
    pub(crate) min_items: Option<u64>,
    /// One sentence saying what the role is to the step.
    pub(crate) description: &'static str,
}

/// A comparison a condition offers by word, with the SQL it is written as.
/// Offered, not enforced: a condition is any SQL condition.
struct Comparison {
    word: &'static str,
    sign: &'static str,
}

/// The comparisons a filter's condition offers, in the order they are offered.
const COMPARISONS: &[Comparison] = &[
    Comparison {
        word: "is",
        sign: "=",
    },
    Comparison {
        word: "is not",
        sign: "!=",
    },
    Comparison {
        word: "over",
        sign: ">",
    },
    Comparison {
        word: "under",
        sign: "<",
    },
    Comparison {
        word: "between",
        sign: "between",
    },
    Comparison {
        word: "is null",
        sign: "is null",
    },
];

/// The directions a sort's order offers by word, in the order they are offered.
/// A direction is a word and the SQL keyword it is written as, the shape a
/// [`Comparison`] has; offered, not enforced, since an order is any SQL clause.
const DIRECTIONS: &[Comparison] = &[
    Comparison {
        word: "ascending",
        sign: "asc",
    },
    Comparison {
        word: "descending",
        sign: "desc",
    },
];

/// What an operation's step writes: the one table it makes, which the step's
/// name names.
const STEP_TABLE: Role = Role {
    name: "out",
    kind: "table",
    list: false,
    required: true,
    of: None,
    min_items: None,
    description: "The table the step makes, which the step's name names.",
};

/// Every operation arc holds, in the order `arc operation list` prints them.
///
/// An operation reads a table, then the column it is asked from. The column is
/// not required: `arc operation record` records a step from the table and the
/// arguments alone.
const CATALOGUE: &[Operation] = &[
    Operation {
        long_name: FILTER_ROWS,
        summary: "Keeps the rows of a table for which a SQL condition holds.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table whose rows are kept.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "The column the condition is on; the condition may name others.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: filter_rows_parameters,
        sql: filter_rows_sql,
        recognised: Some(filter_rows_recognised),
        checks: None,
    },
    Operation {
        long_name: SORT_ROWS,
        summary: "Orders the rows of a table by a SQL `ORDER BY` clause.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table whose rows are ordered.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "The column the order is by; the order may name others.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: sort_rows_parameters,
        sql: sort_rows_sql,
        recognised: None,
        checks: None,
    },
    Operation {
        long_name: HIDE_COLUMN,
        summary: "Leaves one column out of a table, keeping every other column and every row.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table a column is hidden from.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "The column hidden, which the `column` argument names.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: hide_column_parameters,
        sql: hide_column_sql,
        recognised: None,
        checks: None,
    },
    Operation {
        long_name: RENAME_COLUMN,
        summary: "Renames one column of a table, keeping every column in its place and every row.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table a column is renamed in.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "The column renamed, which the `column` argument names.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: rename_column_parameters,
        sql: rename_column_sql,
        recognised: None,
        checks: None,
    },
    Operation {
        long_name: DERIVE_COLUMN,
        summary: "Adds one column to a table, computed for each row by a SQL expression, keeping every other column and every row.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table a column is added to.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "A column the expression reads; the expression may name others.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: derive_column_parameters,
        sql: derive_column_sql,
        recognised: None,
        checks: None,
    },
    Operation {
        long_name: CAST_COLUMN,
        summary: "Converts one column of a table to another type, keeping every other column and every row.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table a column is converted in.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "The column converted, which the `column` argument names.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: cast_column_parameters,
        sql: cast_column_sql,
        recognised: None,
        checks: None,
    },
    Operation {
        long_name: SUMMARISE_ROWS,
        summary: "Summarises the rows of a table with SQL aggregates, one row for each group or one for the whole table.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table whose rows are summarised.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "A column the rows are grouped by or summarised over, which `group_by` or `aggregates` may name with others.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: summarise_rows_parameters,
        sql: summarise_rows_sql,
        recognised: None,
        checks: None,
    },
    Operation {
        long_name: DEDUPE_ROWS,
        summary: "Keeps one row of each set of duplicates in a table: one of each set of identical rows, or the first in an order of each key's rows.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table whose duplicate rows are dropped.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "A column the rows are told apart by, which `key` may name with others.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: dedupe_rows_parameters,
        sql: dedupe_rows_sql,
        recognised: None,
        checks: Some(dedupe_rows_checks),
    },
    Operation {
        long_name: LIMIT_ROWS,
        summary: "Keeps a given number of the rows of a table: the first in a SQL `ORDER BY` order, or the first that come.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table whose first rows are kept.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "A column the rows are ordered by, which `order_by` may name with others.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: limit_rows_parameters,
        sql: limit_rows_sql,
        recognised: None,
        checks: Some(limit_rows_checks),
    },
    Operation {
        long_name: FILL_NULLS,
        summary: "Fills the missing values of one column of a table with a SQL expression, keeping every other column and every row.",
        reads: &[
            Role {
                name: "table",
                kind: "table",
                list: false,
                required: true,
                of: None,
                min_items: None,
                description: "The table whose missing values are filled.",
            },
            Role {
                name: "column",
                kind: "column",
                list: false,
                required: false,
                of: Some("table"),
                min_items: None,
                description: "The column whose missing values are filled, which the `column` argument names.",
            },
        ],
        writes: &[STEP_TABLE],
        parameters: fill_nulls_parameters,
        sql: fill_nulls_sql,
        recognised: None,
        checks: None,
    },
];

/// The `parameters` schema of [`FILTER_ROWS`]: one required string, `where`.
/// Closed to any other key, as an operator's `with:` schema is.
///
/// `where` is annotated rather than constrained. `x-kind` says what the value
/// is, and `x-comparisons` lists the comparisons offered by word; a JSON Schema
/// reader that does not know a key passes over it. The condition is recorded as
/// written, so the schema holds no `enum` and no `pattern` that would hold it
/// to the column or to the comparisons listed.
fn filter_rows_parameters() -> serde_json::Value {
    let comparisons: Vec<serde_json::Value> = COMPARISONS
        .iter()
        .map(|c| serde_json::json!({ "word": c.word, "sign": c.sign }))
        .collect();
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "where": {
                "type": "string",
                "description": "Rows for which the condition holds are kept. \
                    The condition is any SQL condition on the table, recorded as written; \
                    the comparisons listed are the ones offered by word, not a limit.",
                "x-kind": "condition",
                "x-comparisons": comparisons,
            }
        },
        "required": ["where"],
    })
}

/// The SQL step of [`FILTER_ROWS`]: a table named for the step, holding the rows
/// of the table it is applied to for which the condition holds, made by
/// [`filter_rows_query`].
fn filter_rows_sql(recording: &Recording) -> String {
    format!(
        "CREATE OR REPLACE TABLE {} AS\n{};\n",
        quote_ident(recording.name),
        filter_rows_query(&from_ident(recording.on), recording.text("where")),
    )
}

/// The query a [`FILTER_ROWS`] step makes its table from: every column of the
/// table `from`, written as [`from_ident`] writes a table, and the rows for which
/// `condition` holds. The condition is written as given. [`filter_rows_sql`]
/// writes it into the step, and [`filter_rows_recognised`] asks DuckDB whether a
/// typed statement reads as it.
fn filter_rows_query(from: &str, condition: &str) -> String {
    format!("SELECT *\nFROM {from}\nWHERE {condition}")
}

/// The table and the arguments of [`FILTER_ROWS`] when `statement` is a filter
/// of a table: when DuckDB reads it as the query the operation's step writes,
/// [`filter_rows_query`] of the table it reads and of some condition cut from
/// its own text, and reads it as the same query with that condition inside
/// parentheses. `None` when no condition is.
///
/// The table is the one DuckDB's tree names, written back through
/// [`from_ident_on`] as the step writes it, so a statement that reads a schema's
/// table, an alias or a file named in single quotes is not read as the
/// operation, and one that reads `"order"` is, the step writing the name in
/// quotes as the statement did. A file is told apart by its text alone: DuckDB
/// gives `FROM 'orders.csv'` the tree of a table called `orders.csv`, and its
/// text opens with the quote at the location DuckDB gives.
///
/// The condition is not cut out of the tree. DuckDB 2.0 gives a node its start
/// and its length, but the tree holds no node for a parenthesis, so a condition
/// that opens or closes with one would be cut short, and a comment inside it is in
/// no node at all. It is each text
/// [`conditions`] offers, tried in turn, and the first DuckDB reads to the same
/// statement is taken: so the condition is the text as typed, its parentheses,
/// strings and comments kept, and DuckDB decides where a string or a comment
/// ends. Each is tried twice, and taken when DuckDB reads both queries as the
/// statement. Inside parentheses, a candidate that runs on into a clause of its
/// own — `amount > 100 ORDER BY amount DESC`, a `LIMIT`, a `QUALIFY`, a
/// `WINDOW` — does not parse, where bare it would read as the statement it was
/// cut from. Bare, as the step writes it, a candidate whose `)` would close the
/// parentheses it is tried in — `amount > 100) ORDER BY (amount`, cut after a
/// `where` in a comment — does not parse, where inside them the query would
/// read as the statement, its `ORDER BY` included.
fn filter_rows_recognised(statement: &Statement) -> Option<Reading> {
    let from = &statement.tree["statements"][0]["node"]["from_table"];
    let written = from["query_location"]
        .as_u64()
        .and_then(|at| statement.text.as_bytes().get(at as usize));
    if written == Some(&b'\'') {
        return None;
    }
    from["table_name"].as_str().and_then(|on| {
        let from = from_ident_on(&statement.con, on);
        conditions(&statement.text)
            .find(|condition| {
                let enclosed = format!("(\n{condition}\n)");
                statement.reads_as(&filter_rows_query(&from, &enclosed))
                    && statement.reads_as(&filter_rows_query(&from, condition))
            })
            .map(|condition| {
                let arguments = [("where".to_string(), condition.into())];
                (on.to_string(), arguments.into_iter().collect())
            })
    })
}

/// Each text a filter's condition can be in `text`: what follows each `where`,
/// in any case, cut at each `;`, `-` and `/` in it and at its end, and trimmed;
/// shortest first after each `where`, and each `where` in the order it is
/// written. Most are not a condition — a `where` in a name, a string or a
/// comment, a cut inside a string — and DuckDB, not this, says which is: see
/// [`filter_rows_recognised`]. A cut at a `;`, a `--` or a `/*` leaves a
/// statement's closing `;` and a comment after the condition out of it.
fn conditions(text: &str) -> impl Iterator<Item = &str> {
    let starts: Vec<usize> = text
        .to_ascii_lowercase()
        .match_indices("where")
        .map(|(at, keyword)| at + keyword.len())
        .collect();
    starts.into_iter().flat_map(move |start| {
        let after = &text[start..];
        after
            .match_indices([';', '-', '/'])
            .map(|(cut, _)| cut)
            .chain([after.len()])
            .map(move |cut| after[..cut].trim())
    })
}

/// The `parameters` schema of [`SORT_ROWS`]: one required string, `order_by`.
/// Closed to any other key, as [`filter_rows_parameters`] is.
///
/// `order_by` is annotated rather than constrained, and named by its key with no
/// `title`, the way `where` is. `x-kind` says the value is an order, and
/// `x-directions` lists the directions offered by word; a JSON Schema reader that
/// does not know a key passes over it. The order is recorded as written, so the
/// schema holds no `enum` and no `pattern` that would hold it to a column or to
/// the directions listed.
fn sort_rows_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "order_by": {
                "type": "string",
                "description": "The rows are put in the order this SQL `ORDER BY` clause gives, \
                    recorded as written; the order may name several columns, each with a direction. \
                    The directions listed are the ones offered by word, not a limit.",
                "x-kind": "order",
                "x-directions": directions(),
            }
        },
        "required": ["order_by"],
    })
}

/// The [`DIRECTIONS`] an order offers, as an `x-directions` annotation lists them:
/// each a word and its sign. [`sort_rows_parameters`] and
/// [`dedupe_rows_parameters`] annotate their `order_by` with it.
fn directions() -> Vec<serde_json::Value> {
    DIRECTIONS
        .iter()
        .map(|d| serde_json::json!({ "word": d.word, "sign": d.sign }))
        .collect()
}

/// The SQL step of [`SORT_ROWS`]: a table named for the step, holding the rows of
/// the table it is applied to in the order the clause gives. The table read is
/// written as [`from_ident`] writes it, and the order as given.
fn sort_rows_sql(recording: &Recording) -> String {
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT *\nFROM {}\nORDER BY {};\n",
        quote_ident(recording.name),
        from_ident(recording.on),
        recording.text("order_by"),
    )
}

/// The `parameters` schema of [`HIDE_COLUMN`]: one required string, `column`.
/// Closed to any other key, as [`filter_rows_parameters`] is.
///
/// `column` is a name, not SQL: `x-kind` says so, and the step writes it through
/// [`column_ident`], so a name holding a space, a `"` or a reserved word is
/// carried as that name and cannot break out of the `EXCLUDE`. An empty name is
/// refused when the step is recorded ([`Operation::admit`]).
fn hide_column_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "column": {
                "type": "string",
                "description": "The name of the column left out of the table, as the table names it; \
                    a name, not SQL, so a name that needs quotes is given without them.",
                "x-kind": "column",
            }
        },
        "required": ["column"],
    })
}

/// The SQL step of [`HIDE_COLUMN`]: a table named for the step, holding every
/// row of the table it is applied to and every column but the one named. The
/// table read is written as [`from_ident`] writes it, and the column as
/// [`column_ident`] does.
fn hide_column_sql(recording: &Recording) -> String {
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT * EXCLUDE ({})\nFROM {};\n",
        quote_ident(recording.name),
        recording.column("column"),
        from_ident(recording.on),
    )
}

/// The `parameters` schema of [`RENAME_COLUMN`]: two required strings, `column`
/// and `to`. Closed to any other key, as [`hide_column_parameters`] is.
///
/// Both are names, not SQL: `x-kind` says so, and the step writes each through
/// [`column_ident`], so a name holding a space, a `"` or a reserved word is
/// carried as that name and cannot break out of the `RENAME`. An empty name in
/// either is refused when the step is recorded ([`Operation::admit`]).
fn rename_column_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "column": {
                "type": "string",
                "description": "The name of the column renamed, as the table names it; \
                    a name, not SQL, so a name that needs quotes is given without them.",
                "x-kind": "column",
            },
            "to": {
                "type": "string",
                "description": "The name the column takes; a name, not SQL, \
                    so a name that needs quotes is given without them.",
                "x-kind": "column",
            }
        },
        "required": ["column", "to"],
    })
}

/// The SQL step of [`RENAME_COLUMN`]: a table named for the step, holding every
/// row and every column of the table it is applied to, then an `ALTER TABLE` that
/// renames the column named by `column` to `to`, in the place it held. The table
/// read is written as [`from_ident`] writes it, and each column as
/// [`column_ident`] does.
///
/// It is two statements and not `SELECT * RENAME (..)` because that clause is
/// silent where a rename should fail: a `column` the table does not have renames
/// nothing, and a `to` that names another column gives the column a suffixed
/// name (`id_1`) rather than refusing. `ALTER TABLE .. RENAME COLUMN` makes
/// DuckDB refuse both when the step runs, and the run stops at the first
/// statement that fails.
///
/// The two are one transaction, so a failed `ALTER` leaves no table made under
/// the step's name with the column unrenamed: the run stops before the
/// `COMMIT`, and DuckDB rolls the open transaction back as the run ends.
fn rename_column_sql(recording: &Recording) -> String {
    let table = quote_ident(recording.name);
    format!(
        "BEGIN TRANSACTION;\nCREATE OR REPLACE TABLE {table} AS\nSELECT *\nFROM {};\nALTER TABLE {table} RENAME COLUMN {} TO {};\nCOMMIT;\n",
        from_ident(recording.on),
        recording.column("column"),
        recording.column("to"),
    )
}

/// The `parameters` schema of [`DERIVE_COLUMN`]: two required strings, `column`
/// and `expression`. Closed to any other key, as [`hide_column_parameters`] is.
///
/// `column` is the name of the new column, not SQL, written through
/// [`column_ident`] as [`rename_column_parameters`]' names are. `expression` is
/// SQL, recorded as written, so `x-kind` marks it an expression and
/// [`Operation::admit`] holds it to one: DuckDB must read it, in the probe
/// [`EXPRESSION`] writes, as one item of a select that reads no table.
fn derive_column_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "column": {
                "type": "string",
                "description": "The name of the new column; a name, not SQL, \
                    so a name that needs quotes is given without them. \
                    A name the table already has fails the run. \
                    The step adds the column as `__arc_derived` first and then renames it, \
                    so a table that already holds a column of that name is not one this operation can add to.",
                "x-kind": "column",
            },
            "expression": {
                "type": "string",
                "description": "The SQL expression the new column holds for each row, \
                    recorded as written; it may name any column of the table.",
                "x-kind": "expression",
            }
        },
        "required": ["column", "expression"],
    })
}

/// The SQL step of [`DERIVE_COLUMN`]: a table named for the step, holding every
/// row and every column of the table it is applied to and, last, the expression
/// as a column called `__arc_derived`; then an `ALTER TABLE` that renames that
/// column to `column`. The table read is written as [`from_ident`] writes it, the
/// expression as given, and the column as [`column_ident`] does.
///
/// The new column is named by the `ALTER` and not by an alias in the select for
/// the reason [`rename_column_sql`] gives: a select whose alias names a column the
/// table already has gives the new one a suffixed name (`amount_1`) rather than
/// refusing, and `ALTER TABLE .. RENAME COLUMN` makes DuckDB refuse it when the
/// step runs. arc does not open the database when it records, so it cannot
/// refuse the clash then. `__arc_derived` is a name the step reserves: a table
/// that already holds a column called that is not one this operation can add to,
/// since the select would suffix the new column and the `ALTER` rename the old.
///
/// The two are one transaction, as [`rename_column_sql`]'s are, so a clash leaves
/// no table made under the step's name with the column under its temporary name.
fn derive_column_sql(recording: &Recording) -> String {
    let table = quote_ident(recording.name);
    format!(
        "BEGIN TRANSACTION;\nCREATE OR REPLACE TABLE {table} AS\nSELECT *, {} AS __arc_derived\nFROM {};\nALTER TABLE {table} RENAME COLUMN __arc_derived TO {};\nCOMMIT;\n",
        recording.text("expression"),
        from_ident(recording.on),
        recording.column("column"),
    )
}

/// The `parameters` schema of [`CAST_COLUMN`]: two required strings, `column`
/// and `type`. Closed to any other key, as [`hide_column_parameters`] is.
///
/// `column` is a name, not SQL, written through [`column_ident`]. `type` is SQL,
/// recorded as written, so `x-kind` marks it a type and [`Operation::admit`]
/// holds it to one: DuckDB must read it, in the probe [`TYPE`] writes, as the one
/// type of a `CAST`.
fn cast_column_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "column": {
                "type": "string",
                "description": "The name of the column converted, as the table names it; \
                    a name, not SQL, so a name that needs quotes is given without them. \
                    A name the table does not have fails the run.",
                "x-kind": "column",
            },
            "type": {
                "type": "string",
                "description": "The type the column is converted to: any DuckDB type, \
                    written as SQL (`VARCHAR`, `DECIMAL(10, 2)`, `INTEGER[]`). \
                    The conversion is a `CAST`, not a `TRY_CAST`, so a value that does not convert fails the run.",
                "x-kind": "type",
            }
        },
        "required": ["column", "type"],
    })
}

/// The SQL step of [`CAST_COLUMN`]: a table named for the step, holding every
/// row and every column of the table it is applied to, the one named converted
/// in place by `REPLACE`. The table read is written as [`from_ident`] writes it,
/// the type as given, and the column as [`column_ident`] does, twice.
///
/// One statement is enough: DuckDB fails the run when the column is not in the
/// table (`Column "nope" in REPLACE list not found`), and arc does not open the
/// database when it records, so it could not refuse the name then.
fn cast_column_sql(recording: &Recording) -> String {
    let column = recording.column("column");
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT * REPLACE (CAST({column} AS {}) AS {column})\nFROM {};\n",
        quote_ident(recording.name),
        recording.text("type"),
        from_ident(recording.on),
    )
}

/// The `parameters` schema of [`SUMMARISE_ROWS`]: a required string,
/// `aggregates`, and an optional one, `group_by`. Closed to any other key, as
/// [`hide_column_parameters`] is.
///
/// Both are SQL select lists, recorded as written, so `x-kind` marks each a
/// `select-list` and [`Operation::admit`] holds it to one: DuckDB must read it, in
/// the probe [`SELECT_LIST`] writes, as the items of a select that reads no
/// table. Whether an item of `aggregates` is an aggregate is left to DuckDB when
/// the step runs: arc does not sort functions into aggregates and others.
fn summarise_rows_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "aggregates": {
                "type": "string",
                "description": "What each row of the summary holds: a SQL select list of aggregates, \
                    recorded as written, each item of which may carry an alias \
                    (`count(*) AS n, sum(amount) AS total`). \
                    A column neither aggregated nor in `group_by` fails the run.",
                "x-kind": "select-list",
            },
            "group_by": {
                "type": "string",
                "description": "What the rows are grouped by: a SQL select list, recorded as written, \
                    each item of which may carry an alias (`date_trunc('month', at) AS month`). \
                    The step groups by each item by its place (`GROUP BY 1, 2`) and gives one row for each group, \
                    the items first, so an aggregate here fails the run; \
                    without it, the step gives one row for the whole table (`GROUP BY ()`).",
                "x-kind": "select-list",
            }
        },
        "required": ["aggregates"],
    })
}

/// The SQL step of [`SUMMARISE_ROWS`]: a table named for the step, holding the
/// `group_by` items and then the `aggregates`, grouped by each `group_by` item by
/// its place, `GROUP BY 1, 2, …`; or, without `group_by`, the `aggregates` alone
/// grouped by nothing, `GROUP BY ()`, one row for the whole table. The table read
/// is written as [`from_ident`] writes it, and both lists as given.
///
/// The grouping is written by place so that a grouping item may carry an alias
/// and is not written twice, and so that it holds the `group_by` items and nothing
/// else: DuckDB fails the run when an item of `aggregates` is a column neither
/// aggregated nor grouped, or when an item of `group_by` is an aggregate. The
/// number of places is the number of items DuckDB reads in `group_by`'s own probe,
/// asked again here through [`probe_items`], so it counts what the gate admitted
/// and not commas. `GROUP BY ()` holds a window alone, `count(*) OVER ()`, to one
/// row, and fails the run on a bare column, as a grouping would. arc does not
/// open the database when it records, so neither failure can be refused then.
fn summarise_rows_sql(recording: &Recording) -> String {
    let grouped = recording.arguments.get("group_by").and_then(|v| v.as_str());
    let (select, group_by) = match grouped {
        Some(group_by) => {
            // The arguments are admitted before the SQL is written, so the probe
            // reads; its last item is the sentinel, not one of `group_by`'s.
            let items = probe_items(&(SELECT_LIST.probe)(group_by))
                .expect("an admitted `group_by` reads as a select list")
                .len()
                - 1;
            let places: Vec<String> = (1..=items).map(|place| place.to_string()).collect();
            (format!("{group_by}, "), places.join(", "))
        }
        None => (String::new(), "()".to_string()),
    };
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT {select}{}\nFROM {}\nGROUP BY {group_by};\n",
        quote_ident(recording.name),
        recording.text("aggregates"),
        from_ident(recording.on),
    )
}

/// The `parameters` schema of [`DEDUPE_ROWS`]: two optional strings, `key` and
/// `order_by`, given together or not at all. Closed to any other key, as
/// [`hide_column_parameters`] is.
///
/// `dependentRequired` says each needs the other, and [`Operation::admit`] holds
/// the arguments to it. A key alone would keep an arbitrary row of each key, so
/// the step's table could differ from one run to the next; an order alone would
/// be a sort. `key` is SQL, recorded as written, so `x-kind` marks it a
/// `partition` and [`Operation::admit`] holds it to one: DuckDB must read it, in
/// the probe [`PARTITION`] writes, as the items of a select that reads no table.
/// `order_by` keeps [`sort_rows_parameters`]' annotation, an `order` with its
/// `x-directions`, and is checked as that clause; [`dedupe_rows_checks`] then
/// probes the two in the window the step writes.
fn dedupe_rows_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "key": {
                "type": "string",
                "description": "What tells one row's duplicates from another's: a SQL list of expressions, \
                    recorded as written, with no alias (`note`, `lower(note), id`). \
                    The step keeps one row for each key, the first in `order_by`, \
                    and `key` is given with `order_by` or not at all; \
                    without both, the step keeps one of each set of rows identical in every column (`SELECT DISTINCT *`).",
                "x-kind": "partition",
            },
            "order_by": {
                "type": "string",
                "description": "Which row of each key is kept: the first in the order this SQL `ORDER BY` clause gives, \
                    recorded as written, given with `key` or not at all. \
                    Where two rows of a key tie on the order, either may be kept. \
                    The directions listed are the ones offered by word, not a limit.",
                "x-kind": "order",
                "x-directions": directions(),
            }
        },
        "dependentRequired": {
            "key": ["order_by"],
            "order_by": ["key"],
        },
    })
}

/// The SQL step of [`DEDUPE_ROWS`]: a table named for the step. With `key` and
/// `order_by`, it holds every column of the rows the [`dedupe_rows_window`] numbers
/// first, one for each key; without them, one row of each set of rows identical in
/// every column, `SELECT DISTINCT *`. The table read is written as [`from_ident`]
/// writes it, and both arguments as given.
///
/// No tie-break over the whole row is added to the order: the order is the
/// analyst's statement of which row is kept, and where two rows of a key tie on
/// it, either may be.
fn dedupe_rows_sql(recording: &Recording) -> String {
    let from = from_ident(recording.on);
    let query = match recording.arguments.get("key").and_then(|v| v.as_str()) {
        Some(key) => format!(
            "SELECT *\nFROM {from}\nQUALIFY {} = 1",
            dedupe_rows_window(key, recording.text("order_by"))
        ),
        None => format!("SELECT DISTINCT *\nFROM {from}"),
    };
    format!(
        "CREATE OR REPLACE TABLE {} AS\n{query};\n",
        quote_ident(recording.name)
    )
}

/// The window a keyed [`DEDUPE_ROWS`] step numbers each key's rows in:
/// `row_number() OVER (PARTITION BY <key> ORDER BY <order_by>\n)`, both as given.
/// The newline before the `)` ends a `--` comment the order closes with, so the
/// comment cannot swallow the window's close. [`dedupe_rows_sql`] writes it into
/// the step, and [`dedupe_rows_checks`] into the probe of the two arguments.
fn dedupe_rows_window(key: &str, order_by: &str) -> String {
    format!("row_number() OVER (PARTITION BY {key} ORDER BY {order_by}\n)")
}

/// Whether `key` and `order_by`, each already admitted on its own, read together
/// as the one window the step writes: DuckDB must read
/// `SELECT <window> AS __arc_probe`, the [`dedupe_rows_window`] of the two, as a
/// bare select ([`bare_select`]) of one `WINDOW` item, the function `row_number`,
/// called [`SENTINEL`], over the window's default frame. An alias in the key,
/// which `PARTITION BY` does not take, does not parse; an order that closes the
/// window, `amount) = 1 OR (1`, leaves an item that is not the window; and one
/// that runs on into a frame or a `LIMIT` gives a frame that is not the default,
/// or does not parse. Without either argument there is no window, and nothing to
/// check.
///
/// A literal constant in either is refused too, naming the argument. Inside
/// `OVER` a constant is the same value for every row, not a column's place as a
/// number is in `sort-rows`' order or `summarise-rows`' grouping: `key=1` would
/// make the whole table one key, and `order_by=2 desc` or `order_by='x'` would tie
/// every row, so the row kept would be arbitrary. Only a literal is refused, the
/// likely mistake for a column's place; an order that is constant by other means,
/// an uncorrelated subquery (`(SELECT max(id) FROM orders)`) or `1+0`, is the
/// analyst's to avoid.
fn dedupe_rows_checks(arguments: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let text = |key| arguments.get(key).and_then(serde_json::Value::as_str);
    let (Some(key), Some(order_by)) = (text("key"), text("order_by")) else {
        return Ok(());
    };
    let probe = format!("SELECT {} AS {SENTINEL}", dedupe_rows_window(key, order_by));
    let items = probe_items(&probe).filter(|_| !order_by.to_ascii_lowercase().contains(SENTINEL));
    let window = match items.as_deref() {
        Some([item])
            if item["alias"] == SENTINEL
                && item["class"] == "WINDOW"
                && item["function_name"] == "row_number"
                && item["start"] == "UNBOUNDED_PRECEDING"
                && item["end"] == "CURRENT_ROW_RANGE"
                && item["exclude_clause"] == "NO_OTHER" =>
        {
            item
        }
        _ => {
            return Err(refused(format!(
                "the partition key `key` and the order `order_by` of `{DEDUPE_ROWS}` do not read as one \
                 `row_number()` window, `PARTITION BY` the key `ORDER BY` the order: {key} / {order_by}"
            )));
        }
    };
    let constant = |node: &serde_json::Value| node["class"] == "CONSTANT";
    let mut partitions = window["partitions"].as_array().into_iter().flatten();
    let mut orders = window["orders"].as_array().into_iter().flatten();
    for (noun, name, value, holds) in [
        ("partition key", "key", key, partitions.any(constant)),
        (
            "order",
            "order_by",
            order_by,
            orders.any(|order| constant(&order["expression"])),
        ),
    ] {
        if holds {
            return Err(refused(format!(
                "the {noun} `{name}` of `{DEDUPE_ROWS}` holds a constant, which inside the window \
                 is the same value for every row, not a column, so every row would tie: {value}"
            )));
        }
    }
    Ok(())
}

/// The `parameters` schema of [`LIMIT_ROWS`]: one required integer, `count`, at
/// least `0` and at most the largest `BIGINT`, the type of DuckDB's `LIMIT`, and
/// one optional string, `order_by`. Closed to any other key, as
/// [`hide_column_parameters`] is.
///
/// `count` is a JSON integer, and [`Operation::admit`] holds it to its type, its
/// `minimum` and its `maximum`; the command line gives every `--arg` as a string, and
/// [`typed_arguments`] turns one that reads as an integer into that integer
/// first. `order_by` keeps [`sort_rows_parameters`]' annotation, an `order` with
/// its `x-directions`, and is checked as that clause; [`limit_rows_checks`] then
/// probes it with the `LIMIT` the step writes after it.
fn limit_rows_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "count": {
                "type": "integer",
                "minimum": 0,
                "maximum": i64::MAX,
                "description": "How many rows the step keeps, a whole number from 0: the first this many in `order_by`'s order. \
                    Without an order, the rows kept are whichever come first, which DuckDB does not promise \
                    to be the same rows from one run to the next. A table with fewer rows keeps them all.",
            },
            "order_by": {
                "type": "string",
                "description": "Which rows are first: the order this SQL `ORDER BY` clause gives, recorded as written; \
                    the step's table holds the rows kept in that order. Where rows tie on the order at the cut, \
                    which of them are kept may differ from one run to the next. \
                    The directions listed are the ones offered by word, not a limit.",
                "x-kind": "order",
                "x-directions": directions(),
            }
        },
        "required": ["count"],
    })
}

/// The SQL step of [`LIMIT_ROWS`]: a table named for the step, holding the first
/// `count` rows of the table it is applied to, `ORDER BY` the order when one is
/// given. The table read is written as [`from_ident`] writes it, the order as
/// given, and the count from the JSON integer, so it is a number and nothing
/// else. The `LIMIT` opens a line of its own, which ends a `--` comment the order
/// closes with, so the comment cannot swallow the count.
fn limit_rows_sql(recording: &Recording) -> String {
    let order = match recording.arguments.get("order_by").and_then(|v| v.as_str()) {
        Some(order_by) => format!("ORDER BY {order_by}\n"),
        None => String::new(),
    };
    let count = recording
        .arguments
        .get("count")
        .map_or_else(String::new, ToString::to_string);
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT *\nFROM {}\n{order}LIMIT {count};\n",
        quote_ident(recording.name),
        from_ident(recording.on),
    )
}

/// Whether `order_by`, already admitted as an order, stays an order once the
/// step's `LIMIT` follows it: DuckDB must read `SELECT 1 ORDER BY <order_by>`,
/// a newline and `LIMIT 1`, the shape of the step, as one select whose modifiers
/// are one `ORDER_MODIFIER` and then one `LIMIT_MODIFIER` with no `OFFSET`, and
/// nothing else. An order that brings a `LIMIT` of its own, `amount LIMIT 1`,
/// does not parse with the step's after it; one that brings an `OFFSET`,
/// `amount OFFSET 2`, parses into the step's `LIMIT` and would skip rows; and one
/// that ends its statement, `amount desc;`, leaves the step's `LIMIT` a second. A
/// constant is not refused: at the top of a statement `ORDER BY 2` orders by the
/// second column, as it does in `sort-rows`. Without an order there is nothing to
/// check.
fn limit_rows_checks(arguments: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let Some(order_by) = arguments
        .get("order_by")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    let probe = format!("SELECT 1 ORDER BY {order_by}\nLIMIT 1");
    let limited = on_reader(|con| tree(con, &probe))
        .flatten()
        .is_some_and(|tree| {
            let statements = tree["statements"].as_array().map(Vec::as_slice);
            let Some([statement]) = statements else {
                return false;
            };
            let node = &statement["node"];
            let modifiers = node["modifiers"].as_array().map(Vec::as_slice);
            node["type"] == "SELECT_NODE"
                && matches!(
                    modifiers,
                    Some([order, limit])
                        if order["type"] == "ORDER_MODIFIER"
                            && limit["type"] == "LIMIT_MODIFIER"
                            && limit["offset"].is_null()
                )
        });
    if limited {
        Ok(())
    } else {
        Err(refused(format!(
            "the order `order_by` of `{LIMIT_ROWS}` does not read as one `ORDER BY` the step's \
             `LIMIT` can follow: {order_by}"
        )))
    }
}

/// The `parameters` schema of [`FILL_NULLS`]: two required strings, `column`
/// and `value`. Closed to any other key, as [`hide_column_parameters`] is.
///
/// `column` is a name, not SQL, written through [`column_ident`]. `value` is SQL,
/// recorded as written, so `x-kind` marks it an expression and
/// [`Operation::admit`] holds it to one, as it holds [`derive_column_parameters`]'
/// `expression`: DuckDB must read it, in the probe [`EXPRESSION`] writes, as one
/// item of a select that reads no table.
fn fill_nulls_parameters() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "column": {
                "type": "string",
                "description": "The name of the column whose missing values are filled, as the table names it; \
                    a name, not SQL, so a name that needs quotes is given without them. \
                    A name the table does not have fails the run.",
                "x-kind": "column",
            },
            "value": {
                "type": "string",
                "description": "What each missing value is filled with: a SQL expression, recorded as written, \
                    such as a literal (`0`, `'unknown'`) or any expression, another column among them (`amount * 2`). \
                    A value of a wider type widens the column (`0.5` on an `INTEGER` column makes it a `DECIMAL`), \
                    and a value of a type DuckDB cannot combine with the column's fails the run.",
                "x-kind": "expression",
            }
        },
        "required": ["column", "value"],
    })
}

/// The SQL step of [`FILL_NULLS`]: a table named for the step, holding every row
/// and every column of the table it is applied to, the one named replaced in
/// place by `REPLACE` with `coalesce(<column>, <value>)`, so a value already there
/// is kept and a missing one filled. The table read is written as [`from_ident`]
/// writes it, the value as given, and the column as [`column_ident`] does, twice.
///
/// One statement is enough, for the reason [`cast_column_sql`] gives: DuckDB
/// fails the run when the column is not in the table (`Column "nope" in REPLACE
/// list not found`), and arc does not open the database when it records.
fn fill_nulls_sql(recording: &Recording) -> String {
    let column = recording.column("column");
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT * REPLACE (coalesce({column}, {}) AS {column})\nFROM {};\n",
        quote_ident(recording.name),
        recording.text("value"),
        from_ident(recording.on),
    )
}

/// `name` as a double-quoted SQL identifier, a `"` inside it doubled. The step's
/// own name is always quoted, so a name arc admits that would otherwise read as
/// SQL — [`valid_step_name`] admits a `"` — cannot break out of the statement.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A table name for the `FROM` clause, written as it is read. A name of only
/// ASCII letters, digits and underscores that does not open with a digit, and
/// that DuckDB reads bare as the table of that name, is written bare — a plain
/// `orders` unchanged, folded by DuckDB as it always was, so `FROM orders` reads
/// the same, and brightfield's `to_pushdown_sql` writes the same line for it.
/// Any other name is quoted through [`quote_ident`]: one a step could only have
/// made under quotes, holding a space, a `"` or a `;`, so a table the protocol
/// makes cannot break out of the `FROM` into SQL of its own, the same class as
/// the condition's terminator; and one DuckDB does not read bare as a table, a
/// reserved word such as `order`, which a step made under quotes and which
/// `FROM order` would not parse.
///
/// DuckDB is asked, on the thread's [`with_reader`] database, rather than given a
/// list of the words: a list copied into the source would stay as it was when
/// DuckDB's own grows. A database that cannot be opened gives the quoted name,
/// which DuckDB reads as the table whatever the name.
fn from_ident(name: &str) -> String {
    with_reader(name, from_ident_on)
}

/// [`from_ident`] on a database already open, for a caller that writes more than
/// one table in a row.
fn from_ident_on(con: &duckdb::Connection, name: &str) -> String {
    if is_plain(name) && reads_bare_as_table(con, name) {
        name.to_string()
    } else {
        quote_ident(name)
    }
}

/// Whether DuckDB, asked on `con`, reads `FROM <name>` as a read of the table
/// called `name`: it parses, and its tree names a base table of that name. A
/// reserved word gives a parser error in place of a tree, and a word DuckDB
/// parses as anything else gives a tree that names no such table.
fn reads_bare_as_table(con: &duckdb::Connection, name: &str) -> bool {
    tree(con, &format!("SELECT * FROM {name}")).is_some_and(|tree| {
        let from = &tree["statements"][0]["node"]["from_table"];
        from["type"] == "BASE_TABLE" && from["table_name"] == name
    })
}

/// A column name, written as it is read: the rule [`from_ident`] applies to a
/// table, applied to a column. A name of only ASCII letters, digits and
/// underscores that does not open with a digit, and that DuckDB reads bare as the
/// column of that name, is written bare, so `amount` stays `amount`. Any other
/// name is quoted through [`quote_ident`]: one holding a space, a `"` or a `)`,
/// so it cannot break out of the clause it is written into, and a reserved word
/// such as `order`, which DuckDB does not read bare as a column.
///
/// DuckDB is asked, on the thread's [`with_reader`] database, as [`from_ident`]
/// asks it. A database that cannot be opened gives the quoted name, which DuckDB
/// reads as the column whatever the name.
fn column_ident(name: &str) -> String {
    with_reader(name, |con, name| {
        if is_plain(name) && reads_bare_as_column(con, name) {
            name.to_string()
        } else {
            quote_ident(name)
        }
    })
}

/// `write` of `name`, asked on an in-memory DuckDB database opened once for the
/// thread, which [`from_ident`] and [`column_ident`] both ask whether a name reads
/// bare; or `name` through [`quote_ident`] when the database cannot be opened.
///
/// The connection is never dropped: a thread's storage is torn down after DuckDB 2.0's own
/// per-thread state, and closing a database then crashes the process. It is one small
/// in-memory database for each thread that asks, kept until the process ends.
fn with_reader(name: &str, write: impl FnOnce(&duckdb::Connection, &str) -> String) -> String {
    on_reader(|con| write(con, name)).unwrap_or_else(|| quote_ident(name))
}

/// `ask` of the in-memory DuckDB database opened once for the thread, or `None`
/// when the database cannot be opened. [`with_reader`] asks it of a name, and
/// [`probe_items`] of a probe.
fn on_reader<T>(ask: impl FnOnce(&duckdb::Connection) -> T) -> Option<T> {
    thread_local! {
        static READER: Option<std::mem::ManuallyDrop<duckdb::Connection>> =
            duckdb::Connection::open_in_memory().ok().map(std::mem::ManuallyDrop::new);
    }
    READER.with(|con| con.as_ref().map(|con| ask(con)))
}

/// Whether `name` is of only ASCII letters, digits and underscores, and does not
/// open with a digit: the names [`from_ident`] and [`column_ident`] ask DuckDB
/// whether to write bare.
fn is_plain(name: &str) -> bool {
    !name.is_empty()
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether DuckDB, asked on `con`, reads `SELECT <name>` as a select of the
/// column called `name` and of nothing else: it parses, and its tree's select
/// list is one column reference whose one name is `name`. A reserved word gives
/// a parser error in place of a tree, and a word DuckDB parses as anything else —
/// a constant, a function of no arguments — gives a tree that holds no such
/// reference.
fn reads_bare_as_column(con: &duckdb::Connection, name: &str) -> bool {
    tree(con, &format!("SELECT {name}")).is_some_and(|tree| {
        let list = &tree["statements"][0]["node"]["select_list"];
        list.as_array().is_some_and(|items| {
            items.len() == 1
                && items[0]["class"] == "COLUMN_REF"
                && items[0]["column_names"] == serde_json::json!([name])
        })
    })
}

/// Every operation arc holds, in catalogue order.
pub(crate) fn operations() -> &'static [Operation] {
    CATALOGUE
}

/// The operation called `long_name`, or `None` when arc holds none by that name.
/// The match is exact: a long name is an identity, not a search term.
pub(crate) fn operation(long_name: &str) -> Option<&'static Operation> {
    CATALOGUE.iter().find(|op| op.long_name == long_name)
}

/// `arguments` as the command line gives them, each a string, with each value
/// the `parameters` of the operation `long_name` type `integer` turned into that
/// integer where it reads as one, `"2"` as `2` and `"-1"` as `-1`. Any other
/// value is left as given: a string that does not read as an integer, `"ten"`,
/// stays a string for [`Operation::admit`] to refuse as the wrong type, naming
/// the argument, and a `minimum` or a `maximum` is admit's to hold, not this. A
/// whole number written in digits alone that is too large for an `i64` is turned
/// into the number it is, a `u64` or a float, so that admit refuses it as beyond
/// the `maximum` rather than as a string. An operation arc
/// does not hold leaves the arguments as given, for [`record_operation`] to
/// refuse. `arc mcp` passes JSON, whose integers are already integers, and does
/// not call this.
pub(crate) fn typed_arguments(
    long_name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let Some(op) = operation(long_name) else {
        return arguments;
    };
    let schema = (op.parameters)();
    arguments
        .into_iter()
        .map(|(key, value)| {
            let integer = schema["properties"][key.as_str()]["type"] == "integer";
            let parsed = value.as_str().filter(|_| integer).and_then(whole_number);
            (key, parsed.unwrap_or(value))
        })
        .collect()
}

/// `text` as the JSON number it writes, when it reads as a whole number: an
/// `i64` where it fits, as `"+2"` and `"-1"` do; otherwise, where it is digits
/// alone after an optional sign, a `u64` where that fits and a float beyond it,
/// the largest finite float where the digits overflow even that. `None` for any
/// other text, `"ten"`, `" 2"` or `"1e3"`.
fn whole_number(text: &str) -> Option<serde_json::Value> {
    if let Ok(integer) = text.parse::<i64>() {
        return Some(integer.into());
    }
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let integer = match text.parse::<u64>() {
        Ok(integer) => return Some(integer.into()),
        Err(_) => text.parse::<f64>().ok()?,
    };
    // Past about 309 digits the float is infinite, which JSON cannot hold; the
    // largest finite float of the same sign stands in, still beyond any bound.
    let finite = if integer.is_finite() {
        integer
    } else {
        f64::MAX.copysign(integer)
    };
    serde_json::Number::from_f64(finite).map(serde_json::Value::Number)
}

impl Operation {
    /// The listing entry: the long name and the one line saying what it does.
    pub(crate) fn listing(&self) -> serde_json::Value {
        serde_json::json!({
            "long_name": self.long_name,
            "summary": self.summary,
        })
    }

    /// The full description: the listing entry, what the operation reads and
    /// writes, and a `parameters` JSON Schema of what it takes.
    pub(crate) fn description(&self) -> serde_json::Value {
        description(
            self.long_name,
            self.summary,
            self.reads,
            self.writes,
            (self.parameters)(),
        )
    }
}

impl Role {
    /// The entry as a description prints it; `of` and `min_items` only where
    /// they are set.
    fn entry(&self) -> serde_json::Value {
        let mut entry = serde_json::json!({
            "name": self.name,
            "kind": self.kind,
            "list": self.list,
            "required": self.required,
            "description": self.description,
        });
        if let Some(of) = self.of {
            entry["of"] = of.into();
        }
        if let Some(min_items) = self.min_items {
            entry["min_items"] = min_items.into();
        }
        entry
    }
}

/// A description in the one shape an operation and an operator share: the
/// name, one sentence saying what it does, what its step reads and writes by
/// role, and a `parameters` JSON Schema of what it takes besides.
pub(crate) fn description(
    long_name: &str,
    summary: &str,
    reads: &[Role],
    writes: &[Role],
    parameters: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "long_name": long_name,
        "summary": summary,
        "reads": reads.iter().map(Role::entry).collect::<Vec<_>>(),
        "writes": writes.iter().map(Role::entry).collect::<Vec<_>>(),
        "parameters": parameters,
    })
}

/// The description of the operation or the operator called `name`, or `None`
/// when this build holds neither. The one lookup `arc operation describe` and
/// `arc mcp`'s `operation_describe` both make, so the two ways of asking cannot
/// differ: the operation catalogue first, then the operator catalog.
pub(crate) fn describe(name: &str) -> Option<serde_json::Value> {
    operation(name)
        .map(Operation::description)
        .or_else(|| crate::operator::description(name))
}

// ------------------------------------------------ recognising a SQL statement

/// What `sql`, one SQL statement, is read as: the operation arc holds that it
/// is, as `operation`, `on` and `arguments` — the long name, the table the
/// operation is applied to and its arguments, the three keys `arc mcp`'s
/// `operation_record` takes under those names — or `operation` `null` when DuckDB
/// parses the statement and arc reads it as none. `arc sql recognise` and `arc
/// mcp`'s `sql_recognise` both answer from here, so the two cannot differ. Each
/// operation in the catalogue says how a statement is read as it.
///
/// The reading is DuckDB's own parse, asked of the DuckDB library arc is linked
/// with, so no `duckdb` executable is needed; it binds and runs nothing, and
/// writes no file — see [`Statement`].
///
/// Refused: text DuckDB cannot parse as a statement, an empty text and a comment
/// alone among it, and text DuckDB splits into more than one statement, each
/// counted by [`duckdb_statement_count`]; and, from [`Statement::parse`], a
/// DuckDB library that gives no tree.
pub(crate) fn recognise(sql: &str) -> Result<serde_json::Value> {
    Ok(match read_as_operation(sql)? {
        Some((operation, (on, arguments))) => serde_json::json!({
            "operation": operation,
            "on": on,
            "arguments": arguments,
        }),
        None => serde_json::json!({ "operation": null }),
    })
}

/// The operation `sql` is read as, by its long name, with the [`Reading`] —
/// the table and the arguments — or `None` when it is read as none. The one
/// reading [`recognise`] answers from and [`record_sql`] records on, refused as
/// [`recognise`] describes.
fn read_as_operation(sql: &str) -> Result<Option<(&'static str, Reading)>> {
    match duckdb_statement_count(sql) {
        1 => {}
        0 => {
            return Err(refused(
                "DuckDB cannot parse the text as a SQL statement, so arc cannot read it as one"
                    .to_string(),
            ));
        }
        count => {
            return Err(refused(format!(
                "the text holds more than one statement: DuckDB splits it into {count}, and \
                 arc reads one statement at a time"
            )));
        }
    }
    let statement = Statement::parse(sql)?;
    for op in CATALOGUE {
        let Some(recognised) = op.recognised else {
            continue;
        };
        if let Some(reading) = recognised(&statement) {
            return Ok(Some((op.long_name, reading)));
        }
    }
    Ok(None)
}

/// One SQL statement as typed and DuckDB's tree of it, with the connection the
/// tree was asked on, so that a reader can ask for the tree of another text and
/// compare the two.
///
/// A tree is `json_serialize_sql`'s, asked of the DuckDB library arc is linked
/// with on a fresh in-memory database, which parses the text and binds and runs
/// none of it. A `SELECT` gives its node. A statement DuckDB parses and does not
/// serialize — each that is not a `SELECT` — gives an error in its place, and so
/// does text it cannot parse; a tree with no node is read as no operation, so
/// a reader needs no arm for either.
struct Statement {
    /// The statement as typed.
    text: String,
    /// DuckDB's tree of `text`.
    tree: serde_json::Value,
    /// `tree` with no locations ([`unlocated`]): what [`Statement::reads_as`] compares.
    shape: serde_json::Value,
    /// The in-memory database `tree` was asked on.
    con: duckdb::Connection,
}

impl Statement {
    /// `text` and DuckDB's tree of it. Refused when the library gives no tree,
    /// as one with no `json_serialize_sql` does: reading no tree as no operation
    /// would read each filter as a statement arc holds none of.
    fn parse(text: &str) -> Result<Statement> {
        let refused_by = |reason: String| {
            Error::Io(std::io::Error::other(format!(
                "the DuckDB library arc is linked with {reason}, so arc cannot read the statement"
            )))
        };
        let con = duckdb::Connection::open_in_memory()
            .map_err(|e| refused_by(format!("opened no in-memory database ({e})")))?;
        let Some(tree) = tree(&con, text) else {
            return Err(refused_by(
                "gave no tree from `json_serialize_sql`".to_string(),
            ));
        };
        Ok(Statement {
            text: text.to_string(),
            shape: unlocated(&tree),
            tree,
            con,
        })
    }

    /// Whether DuckDB reads `text` as this statement: whether its tree equals
    /// this one with each node's location left out ([`unlocated`]), since a
    /// location says where a node is written and not what it is. A text DuckDB cannot
    /// parse, or does not serialize, gives an error in place of a tree, which
    /// is not this statement's.
    fn reads_as(&self, text: &str) -> bool {
        tree(&self.con, text).is_some_and(|tree| unlocated(&tree) == self.shape)
    }
}

/// DuckDB's tree of `text`, from `json_serialize_sql` on `con`, or `None` when
/// the query fails or answers something other than JSON. The function takes a
/// `VARCHAR` and refuses a parameter of no type, and answers DuckDB's `JSON`
/// type, so each side is cast.
fn tree(con: &duckdb::Connection, text: &str) -> Option<serde_json::Value> {
    let query = "SELECT json_serialize_sql(?::VARCHAR)::VARCHAR";
    con.query_row(query, [text], |row| row.get::<_, String>(0))
        .ok()
        .and_then(|tree| serde_json::from_str(&tree).ok())
}

/// `tree` with each `query_location` and `query_location_length` key left out, at
/// every depth: DuckDB 2.0 gives a node the length of its text as well as where it
/// starts, and `"order"` is longer than `order`, or `*` than nothing.
fn unlocated(tree: &serde_json::Value) -> serde_json::Value {
    match tree {
        serde_json::Value::Object(node) => node
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "query_location" | "query_location_length"))
            .map(|(key, value)| (key.clone(), unlocated(value)))
            .collect(),
        serde_json::Value::Array(items) => items.iter().map(unlocated).collect(),
        other => other.clone(),
    }
}

// ------------------------------------------------- recording an operation

/// What a caller asks [`record_operation`] to record. The command line and `arc
/// mcp` each build one from what they were given and hand it over unchanged.
#[derive(Clone, Copy)]
pub(crate) struct OperationRequest<'a> {
    /// The operation's long name, as `arc operation list` lists it.
    pub(crate) long_name: &'a str,
    /// The table the operation is applied to.
    pub(crate) on: &'a str,
    /// The new step's name, which is also the name of the table it makes.
    pub(crate) name: &'a str,
    /// The operation's arguments.
    pub(crate) arguments: &'a serde_json::Map<String, serde_json::Value>,
    /// The step's `description:`, or `None` for a step with none.
    pub(crate) description: Option<&'a str>,
}

/// Record the operation `request.long_name`, applied to the table `request.on`,
/// as a new step named `request.name` at the end of the protocol at `dir`, with
/// `request.description` as its `description:` when one is given, and return
/// the model's path relative to `dir`.
///
/// The operation's SQL is written from its catalogue entry, so a caller names no
/// operation and no argument of its own: the command line and `arc mcp` hand
/// over what they were given, and an operation added to the catalogue is
/// recorded through both with no edit to either. The model's first line is the
/// long name and the table, ` on ` between them, and nothing else, so the same
/// request writes the same bytes whoever sends it; a description is the caller's
/// own words, written on the step as given, so it too is the same whoever sends
/// it. The step is recorded through
/// [`record_step_with_history`](crate::history::record_step_with_history), so
/// each recording is a version of the protocol, and it runs nothing.
///
/// Refused, with the protocol's directory untouched: an operation arc does not
/// hold; arguments the operation's `parameters` do not admit, whether one is
/// missing, one is not taken or one is of the wrong type; a condition or an order
/// DuckDB's own parser reads as more than one statement, or cannot parse at all
/// (see [`one_statement`]); a table no step of the protocol makes; and a
/// description that is empty or spans lines (see [`record_step`]). Each
/// refusal's message names what was wrong.
pub(crate) fn record_operation(
    dir: &Path,
    request: &OperationRequest,
    history: &crate::history::LocalHistory,
) -> Result<PathBuf> {
    let OperationRequest {
        long_name,
        on,
        name,
        arguments,
        description,
    } = *request;
    let Some(op) = operation(long_name) else {
        let held: Vec<&str> = CATALOGUE.iter().map(|op| op.long_name).collect();
        return Err(refused(format!(
            "no operation called `{long_name}`; the operations arc holds are {}",
            held.join(", ")
        )));
    };
    op.admit(arguments)?;
    made_by_a_step(dir, on)?;

    let step = RecordedStep {
        name: name.to_string(),
        sql: (op.sql)(&Recording {
            name,
            on,
            arguments,
        }),
        provenance: format!("{} on {on}", op.long_name),
        description: description.map(str::to_string),
    };
    let (sql_rel, _) = crate::history::record_step_with_history(dir, &step, history)?;
    Ok(sql_rel)
}

// ----------------------------------------------- recording an operator step

/// What a caller asks [`record_operator`] to record. The command line and `arc
/// mcp` each build one from what they were given and hand it over unchanged.
#[derive(Clone, Copy)]
pub(crate) struct OperatorRequest<'a> {
    /// The operator's name, as a step's `op:` names it.
    pub(crate) operator: &'a str,
    /// The new step's name.
    pub(crate) name: &'a str,
    /// The step's `with:` block, an object.
    pub(crate) with: &'a serde_json::Value,
    /// The step's `description:`, or `None` for a step with none.
    pub(crate) description: Option<&'a str>,
}

/// Record an `op:` step of the operator `request.operator`, named `request.name`
/// and configured by `request.with`, at the end of `steps` of the protocol at
/// `dir`, and return the validated spec now on disk.
///
/// The step is `name:`, `op:`, `with:` as a block mapping, then `description:`
/// when one is given, and nothing else. `with` is checked against the
/// operator's `with:` schema (see [`check_with`](crate::operator::check_with))
/// before anything is written, so a block that misses a required field or
/// carries a value of the wrong type is refused here, with the field named,
/// rather than at the next load. The splice is applied and gated in memory
/// first, then written through
/// [`record_validated_step_with_history`](crate::history::record_validated_step_with_history),
/// so the recording is a version of the protocol. Nothing runs.
///
/// Refused, with the protocol's directory untouched: an operator arc does not
/// hold; a `with` that is not an object or that the schema does not admit; a
/// step name or a description the record path refuses (see [`record_step`]);
/// and a result the loader refuses, a name a step already has among them. Each
/// refusal's message names what was wrong.
pub(crate) fn record_operator(
    dir: &Path,
    request: &OperatorRequest,
    history: &crate::history::LocalHistory,
) -> Result<ValidatedSpec> {
    let OperatorRequest {
        operator,
        name,
        with,
        description,
    } = *request;
    let Some(faults) = crate::operator::check_with(operator, with) else {
        return Err(refused(format!(
            "unknown operator `{operator}` — call operator_describe with no arguments to list the catalog"
        )));
    };
    if !faults.is_empty() {
        return Err(refused(format!(
            "`with` does not fit the schema of `{operator}`: {}",
            faults.join("; ")
        )));
    }
    valid_step_name(name)?;
    if let Some(description) = description {
        valid_description(description)?;
    }

    let manifest_path = dir.join(MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Err(Error::ManifestNotFound);
    }
    let original = std::fs::read_to_string(&manifest_path).map_err(|e| Error::FileRead {
        path: manifest_path.clone(),
        source: e,
    })?;
    let steps_before = Manifest::from_yaml_str(&original)?.steps.len();

    // `with` as a block mapping, every line pushed in under the step's keys. An
    // empty object has no block form and is written inline.
    let with_yaml = serde_yaml::to_string(with)
        .map_err(|e| refused(format!("`with` cannot be written as YAML: {e}")))?;
    let item = |indent: &str| {
        let mut item = format!("{indent}- name: {name}\n{indent}  op: {operator}\n");
        if with.as_object().is_some_and(|map| map.is_empty()) {
            item.push_str(&format!("{indent}  with: {{}}\n"));
        } else {
            item.push_str(&format!("{indent}  with:\n"));
            for line in with_yaml.lines() {
                item.push_str(&format!("{indent}    {line}\n"));
            }
        }
        if let Some(description) = description {
            item.push_str(&format!(
                "{indent}  description: {}\n",
                description_scalar(description)
            ));
        }
        item
    };
    let steps_path = vec![PathPart::Key("steps".to_string())];
    let edit = if steps_before == 0 {
        SpecEdit::Replace {
            path: steps_path,
            value: format!("\n{}", item("  ").trim_end_matches('\n')),
        }
    } else {
        SpecEdit::Append {
            item: item(&sequence_item_indent(&original, &steps_path)?),
            path: steps_path,
        }
    };
    let validated = apply_edits(&original, &[edit])?;

    // The reloaded document is the arbiter, as it is for [`record_step`].
    let manifest = validated.manifest();
    let wanted = serde_yaml::to_value(with).ok();
    let faithful = manifest.steps.len() == steps_before + 1
        && manifest.steps.last().is_some_and(|last| {
            last.name == name
                && last.op.as_deref() == Some(operator)
                && last.with == wanted
                && last.description.as_deref() == description
        });
    if !faithful {
        return Err(Error::EditTarget {
            path: "(step)".to_string(),
            detail: format!(
                "the step {name:?} does not record faithfully — spliced into the manifest it \
                 reads back as something other than itself; use a plain single-line name"
            ),
        });
    }
    crate::history::record_validated_step_with_history(dir, name, validated, history)
}

// ------------------------------------------------- recording a SQL statement

/// What a caller asks [`record_sql`] to record. The command line and `arc mcp`
/// each build one from what they were given and hand it over unchanged.
#[derive(Clone, Copy)]
pub(crate) struct SqlRequest<'a> {
    /// One SQL statement, as typed.
    pub(crate) sql: &'a str,
    /// The new step's name. It is also the name of the table the step makes,
    /// for a statement read as an operation and for a `SELECT`.
    pub(crate) name: &'a str,
    /// The step's `description:`, or `None` for a step with none.
    pub(crate) description: Option<&'a str>,
}

/// What [`record_sql`] recorded a statement as.
pub(crate) struct SqlRecorded {
    /// The model's path, relative to the protocol's directory.
    pub(crate) model: PathBuf,
    /// The operation the statement was recorded as and the table it was applied
    /// to, or `None` when it was recorded as a SQL step.
    pub(crate) operation: Option<(&'static str, String)>,
}

/// A step whose model's bytes are given: what [`record_sql_step`] writes.
#[derive(Clone, Copy)]
pub(crate) struct SqlStep<'a> {
    /// The step's name, as [`RecordedStep::name`] is gated.
    pub(crate) name: &'a str,
    /// The step's `description:`, or `None` for a step with none.
    pub(crate) description: Option<&'a str>,
    /// The bytes of the model, written as given.
    pub(crate) contents: &'a str,
}

/// Record the SQL statement `request.sql` as a new step named `request.name` at
/// the end of the protocol at `dir`, with `request.description` as its
/// `description:` when one is given, and say what it was recorded as.
///
/// A statement [`recognise`] reads as an operation is recorded by
/// [`record_operation`], on the table and with the arguments of that reading, so
/// it leaves the bytes the same request for that operation leaves, whichever way
/// it was asked for. Any other statement DuckDB parses is recorded as a SQL step:
/// a model of the bytes [`sql_step_contents`] makes, which carries no
/// [`GENERATED_MARKER`], so [`amend_step_sql`] reads it as the analyst's own text
/// and refuses to rewrite it. Either way the step is recorded through local
/// history and nothing runs.
///
/// Refused, with the protocol's directory untouched: text DuckDB cannot parse and
/// text it splits into more than one statement (see [`recognise`]); a statement
/// read as an operation that [`record_operation`] refuses, a table no step makes
/// among them; a step name or a description the record path refuses; and a
/// statement [`sql_step_contents`] refuses. Each refusal's message names what was
/// wrong.
pub(crate) fn record_sql(
    dir: &Path,
    request: &SqlRequest,
    history: &crate::history::LocalHistory,
) -> Result<SqlRecorded> {
    let SqlRequest {
        sql,
        name,
        description,
    } = *request;
    if let Some((long_name, (on, arguments))) = read_as_operation(sql)? {
        let operation = OperationRequest {
            long_name,
            on: &on,
            name,
            arguments: &arguments,
            description,
        };
        let model = record_operation(dir, &operation, history)?;
        return Ok(SqlRecorded {
            model,
            operation: Some((long_name, on)),
        });
    }
    let contents = sql_step_contents(name, sql)?;
    let step = SqlStep {
        name,
        description,
        contents: &contents,
    };
    let (model, _) = crate::history::record_sql_step_with_history(dir, &step, history)?;
    Ok(SqlRecorded {
        model,
        operation: None,
    })
}

/// [`record_step`] for a model whose bytes are given: the same gates on the name
/// and the description, the same order of writes, and no marker line.
pub(crate) fn record_sql_step(dir: &Path, step: &SqlStep) -> Result<(PathBuf, ValidatedSpec)> {
    valid_step_name(step.name)?;
    if let Some(description) = step.description {
        valid_description(description)?;
    }
    let recorded = RecordedStep {
        name: step.name.to_string(),
        sql: String::new(),
        provenance: String::new(),
        description: step.description.map(str::to_string),
    };
    write_recorded(dir, &recorded, step.contents)
}

/// The bytes of the model of a SQL step recorded from `sql`, one statement as
/// typed, for the step `name`.
///
/// The statement is written as typed, then one final newline when it ends in
/// none. Where `CREATE OR REPLACE TABLE "<name>" AS`, a line and a newline, in
/// front of the statement is text DuckDB parses, the line is written first, so
/// the step makes a table of the step's name when it runs: a bare `SELECT` runs
/// and prints its rows and makes no table. That is each `SELECT` DuckDB reads, a
/// `WITH` and a statement that opens with `FROM` among them, and on DuckDB 2.0 a
/// `DESCRIBE`, a `SHOW` and a `SUMMARIZE` too. A statement the line cannot go in
/// front of — a `CREATE TABLE`, an `UPDATE`, an `INSERT` — is written as typed, and
/// what it makes, it makes itself. It is DuckDB's parse of the line and the
/// statement together that decides, and not whether DuckDB gives the statement a
/// tree: DuckDB 1.5 gave one for a `DESCRIBE` and refused the line in front of it.
///
/// No `-- generated:` line is written: that line licenses [`amend_step_sql`] to
/// rewrite a file, and the statement is the analyst's own text. So a statement
/// written as typed whose own first line opens with it is refused, since the file
/// would read as one arc may rewrite.
fn sql_step_contents(name: &str, sql: &str) -> Result<String> {
    let tabled = format!("CREATE OR REPLACE TABLE {} AS\n{sql}", quote_ident(name));
    let mut contents = if duckdb_statement_count(&tabled) == 1 {
        tabled
    } else {
        sql.to_string()
    };
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    if sql_is_generated(&contents) {
        return Err(refused(format!(
            "the statement opens with `{GENERATED_MARKER}`, the line that marks a file arc may \
             rewrite, and arc does not write the analyst's own text under it; remove the line"
        )));
    }
    Ok(contents)
}

impl Operation {
    /// Whether `arguments` are what the operation's `parameters` schema admits:
    /// each required argument present, no argument the schema does not name when
    /// it is closed, and each value of the type the schema gives it. A value
    /// annotated with a clause `x-kind` — a `condition` or an `order` — is also
    /// checked as that clause, through [`one_statement`]; one annotated with a
    /// probed `x-kind` — an `expression` — is checked in its probe, through
    /// [`probed`]; one annotated as a `column`, a name and not SQL, is checked
    /// only to be a name at all, not empty or blank, since [`column_ident`]
    /// carries any other name as written. An argument the schema's
    /// `dependentRequired` ties to others is refused without them, a number below
    /// its property's `minimum` or above its `maximum` is refused naming the
    /// argument and the bound, before its type is checked, so a whole number too
    /// large for an integer is refused as too large and not as no integer; and the
    /// operation's own `checks`, when it has them, run last, on the arguments
    /// together.
    fn admit(&self, arguments: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
        let schema = (self.parameters)();
        let empty = serde_json::Map::new();
        let properties = schema["properties"].as_object().unwrap_or(&empty);
        let taken = || {
            properties
                .keys()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };

        if schema["additionalProperties"] == serde_json::Value::Bool(false) {
            for key in arguments.keys() {
                if !properties.contains_key(key) {
                    return Err(refused(format!(
                        "`{}` takes no argument `{key}`; it takes {}",
                        self.long_name,
                        taken()
                    )));
                }
            }
        }
        let required = schema["required"].as_array().into_iter().flatten();
        for required in required.filter_map(serde_json::Value::as_str) {
            if !arguments.contains_key(required) {
                return Err(refused(format!(
                    "`{}` needs the argument `{required}`, and none was given",
                    self.long_name
                )));
            }
        }
        let dependent = schema["dependentRequired"]
            .as_object()
            .into_iter()
            .flatten();
        for (given, needs) in dependent.filter(|(given, _)| arguments.contains_key(*given)) {
            let needs = needs.as_array().into_iter().flatten();
            for needed in needs.filter_map(serde_json::Value::as_str) {
                if !arguments.contains_key(needed) {
                    return Err(refused(format!(
                        "the argument `{given}` of `{}` needs `{needed}` as well, and none was given",
                        self.long_name
                    )));
                }
            }
        }
        for (key, value) in arguments {
            let Some(property) = properties.get(key) else {
                continue;
            };
            for (bound, beyond, limit) in [
                ("minimum", Ordering::Less, &property["minimum"]),
                ("maximum", Ordering::Greater, &property["maximum"]),
            ] {
                if compare_numbers(value, limit) == Some(beyond) {
                    return Err(refused(format!(
                        "the argument `{key}` of `{}` has a {bound} of {limit}, not {value}",
                        self.long_name
                    )));
                }
            }
            if let Some(ty) = property["type"].as_str()
                && !is_of_type(value, ty)
            {
                let article = if ty.starts_with(['a', 'e', 'i', 'o', 'u']) {
                    "an"
                } else {
                    "a"
                };
                return Err(refused(format!(
                    "the argument `{key}` of `{}` is {article} {ty}, not {value}",
                    self.long_name
                )));
            }
            if let Some(clause) = Clause::of(&property["x-kind"])
                && let Some(text) = value.as_str()
            {
                one_statement(key, clause, text)?;
            }
            if let Some(probe) = Probe::of(&property["x-kind"])
                && let Some(text) = value.as_str()
            {
                probed(self.long_name, key, probe, text)?;
            }
            if property["x-kind"] == "column"
                && value.as_str().is_some_and(|name| name.trim().is_empty())
            {
                return Err(refused(format!(
                    "the column `{key}` of `{}` is empty or blank",
                    self.long_name
                )));
            }
        }
        self.checks.map_or(Ok(()), |checks| checks(arguments))
    }
}

/// `value` compared with `bound`, two JSON numbers: exactly when both are
/// integers, so a `maximum` of the largest `BIGINT` holds against one past it,
/// and as floating point otherwise. `None` when either is not a number, as a
/// bound a property does not set is not.
fn compare_numbers(value: &serde_json::Value, bound: &serde_json::Value) -> Option<Ordering> {
    let integer = |v: &serde_json::Value| {
        v.as_i64()
            .map(i128::from)
            .or_else(|| v.as_u64().map(i128::from))
    };
    match (integer(value), integer(bound)) {
        (Some(value), Some(bound)) => Some(value.cmp(&bound)),
        _ => value.as_f64()?.partial_cmp(&bound.as_f64()?),
    }
}

/// Whether `value` is of the JSON Schema type `ty`. A type the schema language
/// does not name admits any value.
fn is_of_type(value: &serde_json::Value, ty: &str) -> bool {
    match ty {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// The SQL clause an annotated argument is written into, which fixes both the
/// probe [`one_statement`] parses it inside and the word a refusal calls it by.
/// `filter-rows`' `where` is a `WHERE` condition; `sort-rows`' `order_by` is an
/// `ORDER BY` order. A value whose `x-kind` names neither has no clause and is
/// not parse-checked: a `column` is a name, not a clause, and [`Operation::admit`]
/// checks it apart.
struct Clause {
    /// The SQL keyword the value follows in the one-statement probe.
    keyword: &'static str,
    /// What a refusal calls the value.
    noun: &'static str,
}

impl Clause {
    /// The clause a value carrying this `x-kind` is written into, or `None` when
    /// the annotation names no clause arc parse-checks.
    fn of(x_kind: &serde_json::Value) -> Option<Clause> {
        match x_kind.as_str() {
            Some("condition") => Some(Clause {
                keyword: "WHERE",
                noun: "condition",
            }),
            Some("order") => Some(Clause {
                keyword: "ORDER BY",
                noun: "order",
            }),
            _ => None,
        }
    }
}

/// A clause goes into the step's SQL verbatim, so arc records it only when DuckDB
/// reads `SELECT 1 <clause> <value>` as exactly one statement — `SELECT 1 WHERE
/// amount > 100` for a `filter-rows` condition, `SELECT 1 ORDER BY amount desc`
/// for a `sort-rows` order. The value is put after `clause.keyword` in that probe
/// and handed to [`duckdb_statement_count`], which asks DuckDB to split the text
/// into statements without binding or running any of them. The two other
/// outcomes are refused at record time, the directory untouched:
///
/// * DuckDB reads more than one statement — a `;` outside every string and
///   comment, with SQL after it, such as `amount > 100; DROP TABLE orders` or
///   `amount desc; DROP TABLE orders` — which would make the recorded step do
///   more than the operation.
/// * DuckDB cannot parse the probe at all (`duckdb_statement_count` is `0`).
///   `arc run` hands the recorded file to the `duckdb` CLI's `-f`, which runs
///   each statement it can read before it reaches the part it cannot, so a
///   runnable `;`-terminated prefix followed by an unparseable tail —
///   `amount > 100; DROP TABLE orders; zzz`, or the same closing with a `.`
///   dot-command — would still drop `orders` on a run
///   (`arc_run_runs_a_valid_prefix_before_an_unparseable_tail` in
///   `tests/operation_record.rs` shows the run doing exactly that). arc will
///   not record a value it cannot confirm is a single statement.
///
/// Because DuckDB's own parser draws the line, however it reads a string or a
/// comment — `E'\''` backslash escapes and a `--` comment ended by a carriage
/// return included — a `;` inside one stays in the one statement. A value ending
/// in a bare `;` with nothing after it, such as `amount > 100;`, is a single
/// statement and is recorded. Whether a value also binds to the table, rather
/// than only parsing as one statement, is a later parse's to judge, so
/// `amount > 100 UNION ALL SELECT * FROM orders` and `amount desc LIMIT 1` — each
/// one statement — are recorded as written.
fn one_statement(key: &str, clause: Clause, value: &str) -> Result<()> {
    let probe = format!("SELECT 1 {} {value}", clause.keyword);
    match duckdb_statement_count(&probe) {
        1 => Ok(()),
        0 => Err(refused(format!(
            "the {} `{key}` is not one SQL statement DuckDB can parse, so arc cannot \
             record it as one: {value}",
            clause.noun
        ))),
        _ => Err(second_statement(clause.noun, key, value)),
    }
}

/// The refusal of a value whose probe DuckDB reads as more than one statement:
/// a `;` outside every string and comment, with SQL after it. [`one_statement`]
/// and [`probed`] give it in the one wording, `noun` saying what the value is.
fn second_statement(noun: &str, key: &str, value: &str) -> Error {
    refused(format!(
        "the {noun} `{key}` holds a `;` outside a string or a comment, which would end \
         the step's statement and begin another: {value}"
    ))
}

/// The alias each probe ends in. A trailing `--` comment swallows it and a comma
/// list puts another item after it, so a probe whose last item is not called this
/// is one in which the value did not stay where the step puts it.
const SENTINEL: &str = "__arc_probe";

/// A SQL fragment an annotated argument is, gated in a probe that reproduces what
/// follows the fragment in the step: what a refusal calls it, the probe it is
/// written into, and what the probe's select list must be for the fragment to be
/// admitted. A `condition` and an `order` are gated as a [`Clause`] instead, by
/// [`one_statement`] alone.
struct Probe {
    /// What a refusal calls the value.
    noun: &'static str,
    /// The probe `value` is written into, ending in the [`SENTINEL`] alias.
    probe: fn(value: &str) -> String,
    /// Whether the select list DuckDB reads in the probe is the one the fragment
    /// should give: `Ok`, or what the fragment is instead, as a refusal says it
    /// after the value's name.
    admits: fn(items: &[serde_json::Value]) -> std::result::Result<(), &'static str>,
}

/// What a refusal says of a fragment whose probe is not the one select its kind
/// gives, after the value's name.
const NOT_ONE: &str = "is not one SQL";

/// The probe of an `expression`: `SELECT <value> AS __arc_probe`, which must read
/// as exactly one item, called [`SENTINEL`], that is one value for each row
/// ([`one_value_per_row`]). `1 FROM orders` and `a UNION SELECT b` are not a bare
/// select, `amount, note` is two items, and `amount * 2 -- double` leaves the one
/// item without the alias.
const EXPRESSION: Probe = Probe {
    noun: "expression",
    probe: |value| format!("SELECT {value} AS {SENTINEL}"),
    admits: |items| match items {
        [item] if item["alias"] == SENTINEL => {
            one_value_per_row(item).map_err(|spread| match spread {
                Spread::Columns => "expands to several columns, where the step adds one",
                Spread::Rows => "changes the number of rows, where the step keeps every row",
            })
        }
        _ => Err(NOT_ONE),
    },
};

/// The probe of a `select-list`: `SELECT <value>, 1 AS __arc_probe`, which must
/// read as at least two items, the last called [`SENTINEL`], each item before it
/// one value for each row ([`one_value_per_row`]). The value may list several
/// items, each with an alias. `count(*) FROM other` and `a UNION SELECT b` are not
/// a bare select, and `count(*) AS n -- all` swallows the sentinel item. A star
/// would widen the list to every column of the table, and an `unnest` would change
/// the number of rows the step groups or summarises, so each is refused. The
/// number of items DuckDB reads before the sentinel is the number
/// [`summarise_rows_sql`] groups by.
const SELECT_LIST: Probe = Probe {
    noun: "select list",
    probe: |value| format!("SELECT {value}, 1 AS {SENTINEL}"),
    admits: |items| match items {
        [listed @ .., last] if !listed.is_empty() && last["alias"] == SENTINEL => listed
            .iter()
            .try_for_each(one_value_per_row)
            .map_err(|spread| match spread {
                Spread::Columns => {
                    "has an item that expands to several columns, where each item gives one"
                }
                Spread::Rows => "changes the number of rows the step reads",
            }),
        _ => Err(NOT_ONE),
    },
};

/// The probe of a `partition` key: the [`SELECT_LIST`] probe and its check, in
/// the words of a partition key. A `)` that would close the window the key is
/// written into does not parse in it, a clause such as `ORDER BY` is not a bare
/// select, and a star or an `unnest` is refused as in a select list. An alias,
/// which a select list takes and `PARTITION BY` does not, passes here and is
/// refused by [`dedupe_rows_checks`], which probes the key in its window.
const PARTITION: Probe = Probe {
    noun: "partition key",
    ..SELECT_LIST
};

/// The probe of a `type`: `SELECT CAST(NULL AS <value>) AS __arc_probe`, which
/// must read as exactly one item, a `CAST`, called [`SENTINEL`], whose child is
/// the probe's own `NULL`. `INTEGER) AS x, (1` closes the cast and adds an item,
/// so the select lists two; `INTEGER) --` leaves the item without the alias; and
/// `INTEGER)::DECIMAL(10,2` is one `CAST` item too, but of a cast, so the step
/// would convert through `INTEGER` first. None is a type.
const TYPE: Probe = Probe {
    noun: "type",
    probe: |value| format!("SELECT CAST(NULL AS {value}) AS {SENTINEL}"),
    admits: |items| match items {
        [item]
            if item["alias"] == SENTINEL
                && item["class"] == "CAST"
                && item["child"]["class"] == "CONSTANT" =>
        {
            Ok(())
        }
        _ => Err(NOT_ONE),
    },
};

/// Whether `node`, an item of DuckDB's tree, gives one value for each row it is
/// written into: `Ok`, or why not. A star — `*`, `orders.*`, `* EXCLUDE (..)`,
/// `COLUMNS(..)`, alone or inside a function — and a `*COLUMNS(..)` unpacked into
/// a call read as one item in the probe but expand to several columns in the
/// step; an `unnest`, an `unlist` or a `generate_subscripts` gives several rows
/// for one, the last a row for each index of a list. A subquery is not
/// looked inside: its star reads its own `FROM`, and it gives one value, so
/// `EXISTS (SELECT * FROM orders)` is admitted. `count(*)` holds no star node.
/// Each probe that walks an item says in its own words what the [`Spread`] would
/// do to its step.
fn one_value_per_row(node: &serde_json::Value) -> std::result::Result<(), Spread> {
    match node {
        serde_json::Value::Object(fields) => {
            if node["class"] == "SUBQUERY" {
                return Ok(());
            }
            if node["class"] == "STAR" || node["type"] == "OPERATOR_UNPACK" {
                return Err(Spread::Columns);
            }
            if node["class"] == "FUNCTION"
                && node["function_name"].as_str().is_some_and(|name| {
                    ["unnest", "unlist", "generate_subscripts"]
                        .iter()
                        .any(|spreading| name.eq_ignore_ascii_case(spreading))
                })
            {
                return Err(Spread::Rows);
            }
            fields.values().try_for_each(one_value_per_row)
        }
        serde_json::Value::Array(items) => items.iter().try_for_each(one_value_per_row),
        _ => Ok(()),
    }
}

/// How an item that is not one value for each row spreads, as
/// [`one_value_per_row`] finds it.
enum Spread {
    /// A star or an unpacked `COLUMNS(..)`: one item in the probe, several
    /// columns in the step.
    Columns,
    /// An `unnest`, an `unlist` or a `generate_subscripts`: several rows for one.
    Rows,
}

impl Probe {
    /// The probe a value carrying this `x-kind` is gated in, or `None` when the
    /// annotation names no fragment arc probes.
    fn of(x_kind: &serde_json::Value) -> Option<Probe> {
        match x_kind.as_str() {
            Some("expression") => Some(EXPRESSION),
            Some("type") => Some(TYPE),
            Some("select-list") => Some(SELECT_LIST),
            Some("partition") => Some(PARTITION),
            _ => None,
        }
    }
}

/// Whether `value`, the argument `key` of the operation `operation`, is the one
/// fragment `probe` gates. Refused, the directory untouched, when:
///
/// * it holds [`SENTINEL`], in any case, since a value that supplies the alias
///   itself could hide what follows it behind a comment and still pass;
/// * DuckDB reads `SELECT <value>` as more than one statement, in the wording
///   [`one_statement`] gives the same `;`. The value is counted on its own and
///   not in its probe, since what the probe puts after it can stop a second
///   statement from parsing — `amount; DROP TABLE orders AS __arc_probe` — and
///   the `;` would then be refused as something else;
/// * DuckDB cannot parse the probe, or reads it as anything but a bare select
///   ([`bare_select`]) whose select list `probe.admits`, with the reason
///   `admits` gives.
fn probed(operation: &str, key: &str, probe: Probe, value: &str) -> Result<()> {
    if value.to_ascii_lowercase().contains(SENTINEL) {
        return Err(refused(format!(
            "the {} `{key}` of `{operation}` holds `{SENTINEL}`, the name arc's check of it \
             reserves: {value}",
            probe.noun
        )));
    }
    if duckdb_statement_count(&format!("SELECT {value}")) > 1 {
        return Err(second_statement(probe.noun, key, value));
    }
    let noun = probe.noun;
    let admitted = probe_items(&(probe.probe)(value))
        .ok_or(NOT_ONE)
        .and_then(|items| (probe.admits)(&items));
    admitted.map_err(|reason| {
        let reason = if reason == NOT_ONE {
            format!("{NOT_ONE} {noun}")
        } else {
            reason.to_string()
        };
        refused(format!(
            "the {noun} `{key}` of `{operation}` {reason}: {value}"
        ))
    })
}

/// The select list of `probe`, when DuckDB reads it as one statement whose tree
/// [`bare_select`] accepts; `None` when DuckDB cannot parse it, reads more than
/// one statement in it, or reads a select that does more than list its items. It
/// is asked on the thread's [`on_reader`] database, and binds and runs nothing.
/// The statements are counted in the tree itself: DuckDB gives a tree for each
/// statement of a text that is all selects, and an error for a text that does not
/// parse or holds anything else, so no second parse is needed.
fn probe_items(probe: &str) -> Option<Vec<serde_json::Value>> {
    let tree = on_reader(|con| tree(con, probe))??;
    bare_select(&tree)?;
    tree["statements"][0]["node"]["select_list"]
        .as_array()
        .cloned()
}

/// The number of items in the select list of `tree`, DuckDB's tree of a text,
/// when it is one statement that is a bare select: a `SELECT_NODE` that reads no
/// table and has no `WHERE`, `GROUP BY`, `HAVING`, `QUALIFY`, sample, modifier
/// (`ORDER BY`, `LIMIT`, `DISTINCT`) or CTE. `None` for any other tree, a parse
/// error's included. A `GROUP BY ALL` leaves the grouping lists empty and is told
/// by the node's aggregate handling, which a bare select leaves standard.
fn bare_select(tree: &serde_json::Value) -> Option<usize> {
    let statements = tree["statements"].as_array()?;
    let [statement] = statements.as_slice() else {
        return None;
    };
    let node = &statement["node"];
    let empty = |field: &serde_json::Value| field.as_array().is_some_and(Vec::is_empty);
    let bare = node["type"] == "SELECT_NODE"
        && node["from_table"]["type"] == "EMPTY"
        && ["where_clause", "having", "qualify", "sample"]
            .iter()
            .all(|field| node[field].is_null())
        && ["group_expressions", "group_sets", "modifiers"]
            .iter()
            .all(|field| empty(&node[field]))
        && empty(&node["cte_map"]["map"])
        && node["aggregate_handling"] == "STANDARD_HANDLING";
    if !bare {
        return None;
    }
    node["select_list"].as_array().map(Vec::len)
}

/// The number of SQL statements DuckDB parses in `sql`, or `0` when DuckDB
/// cannot parse it (a parse error, or a string DuckDB's C API cannot take
/// because it holds a NUL). It asks DuckDB's own parser through the `duckdb`
/// crate arc already links, on a fresh in-memory database that binds and runs
/// nothing: `duckdb_extract_statements` splits a query into statements, and this
/// reads only the count it returns. No extracted statement is prepared, bound or
/// executed, so a condition carrying `DROP TABLE orders` after a `;` is counted,
/// never run, by this check. A parse error and a statement count share one
/// return, `usize`: [`one_statement`] admits only a `1` and refuses both a `0`
/// and a count above one, with a distinct message for each, so the two need not
/// be told apart by the type.
fn duckdb_statement_count(sql: &str) -> usize {
    use duckdb::ffi;
    // A NUL byte cannot reach DuckDB's C API, so such a string cannot be parsed
    // into statements here; it counts `0`, which the caller refuses.
    let Ok(query) = std::ffi::CString::new(sql) else {
        return 0;
    };
    // SAFETY: `db` and `con` are null until DuckDB fills them and are checked
    // for success before use; each is destroyed exactly once, in reverse order
    // of creation, before returning. The connection is in-memory, and the only
    // call made on it is `duckdb_extract_statements`, which parses the query and
    // does not prepare, bind or execute any statement in it.
    unsafe {
        let mut db: ffi::duckdb_database = std::ptr::null_mut();
        if ffi::duckdb_open(std::ptr::null(), &mut db) != ffi::DuckDBSuccess {
            ffi::duckdb_close(&mut db);
            return 0;
        }
        let mut con: ffi::duckdb_connection = std::ptr::null_mut();
        if ffi::duckdb_connect(db, &mut con) != ffi::DuckDBSuccess {
            ffi::duckdb_disconnect(&mut con);
            ffi::duckdb_close(&mut db);
            return 0;
        }
        let mut extracted: ffi::duckdb_extracted_statements = std::ptr::null_mut();
        // `duckdb_extract_statements` returns 0 on a parse error, else the
        // number of statements it split the query into.
        let count = ffi::duckdb_extract_statements(con, query.as_ptr(), &mut extracted);
        ffi::duckdb_destroy_extracted(&mut extracted);
        ffi::duckdb_disconnect(&mut con);
        ffi::duckdb_close(&mut db);
        count as usize
    }
}

/// Whether a step of the protocol at `dir` makes the table `on`, as `arc run`
/// reads what each step makes. A table name matches in any case, as DuckDB's
/// does; a file a step writes is not a table.
fn made_by_a_step(dir: &Path, on: &str) -> Result<()> {
    let manifest = Manifest::load(dir)?;
    let graph = crate::asset::AssetGraph::build(&manifest, dir);
    for unreadable in &graph.unreadable {
        eprintln!("warning: {}", unreadable.warning());
    }
    let wanted = on.to_lowercase();
    let made = graph.steps.values().any(|assets| {
        assets.produces.contains(&wanted)
            && assets.declared_kind.get(&wanted) == Some(&crate::asset_kind::AssetKind::Table)
    });
    if !made {
        return Err(refused(format!(
            "no step of the protocol makes a table called `{on}`; an operation is applied to \
             a table a step makes"
        )));
    }
    Ok(())
}

/// A request to record an operation that is refused before anything is written.
pub(crate) fn refused(message: String) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message,
    ))
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_is_read_from_the_first_line_only() {
        assert!(sql_is_generated(
            "-- generated: grid filter on tides\nSELECT 1"
        ));
        assert!(sql_is_generated(
            "  -- generated: indented marker\nSELECT 1"
        ));
        assert!(!sql_is_generated("SELECT 1\n-- generated: too late"));
        assert!(!sql_is_generated("-- a hand comment\nSELECT 1"));
        assert!(!sql_is_generated(""));
    }

    #[test]
    fn slugs_keep_safe_characters_and_degrade_to_step() {
        assert_eq!(filename_slug("filter_tides"), "filter_tides");
        assert_eq!(filename_slug("top-10 ports!"), "top-10_ports_");
        assert_eq!(filename_slug("///"), "step");
        assert_eq!(filename_slug(""), "step");
    }

    #[test]
    fn numbering_continues_from_the_highest_numbered_model() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            next_model_number(dir.path()),
            1,
            "an absent dir starts at 1"
        );

        std::fs::create_dir_all(dir.path().join("models")).unwrap();
        let models = dir.path().join("models");
        assert_eq!(next_model_number(&models), 1, "an empty dir starts at 1");

        std::fs::write(models.join("load_data.sql"), "SELECT 1").unwrap();
        assert_eq!(
            next_model_number(&models),
            1,
            "unnumbered models don't count"
        );

        std::fs::write(models.join("03_filter.sql"), "SELECT 1").unwrap();
        std::fs::write(models.join("01_seed.sql"), "SELECT 1").unwrap();
        assert_eq!(next_model_number(&models), 4, "continues past the highest");
    }

    #[test]
    fn model_contents_normalises_only_the_final_newline() {
        assert_eq!(
            model_contents("grid filter", "SELECT 1"),
            "-- generated: grid filter\nSELECT 1\n"
        );
        let terminated = model_contents("v", "SELECT 1\n");
        assert_eq!(terminated, "-- generated: v\nSELECT 1\n");
    }

    /// The read-back check holds each part of the recorded step on its own: a
    /// manifest that differs from the request in any one of them is refused,
    /// naming the part, and one that matches is not. No input reaches these
    /// refusals through `record_step` today, because the name and description
    /// gates refuse first, so the check is driven here with manifests built to
    /// disagree.
    #[test]
    fn reads_back_refuses_a_manifest_that_differs_from_the_request_in_any_part() {
        let manifest = |steps: &str| {
            Manifest::from_yaml_str(&format!("name: shop\nsteps:\n{steps}"))
                .expect("the manifest loads")
        };
        let first = "  - name: orders\n    sql: models/01_orders.sql\n";
        let step = RecordedStep {
            name: "big".to_string(),
            sql: "SELECT 1;".to_string(),
            provenance: "test".to_string(),
            description: Some("Keep the big ones".to_string()),
        };
        let sql = "models/02_big.sql";
        let recorded =
            "  - name: big\n    sql: models/02_big.sql\n    description: Keep the big ones\n";
        assert!(
            reads_back(&manifest(&format!("{first}{recorded}")), 1, &step, sql).is_ok(),
            "the manifest that carries exactly the request is refused"
        );

        for (what, steps, expected) in [
            (
                "another name",
                format!(
                    "{first}  - name: small\n    sql: models/02_big.sql\n    description: Keep the big ones\n"
                ),
                "(name)",
            ),
            (
                "another file",
                format!(
                    "{first}  - name: big\n    sql: models/03_big.sql\n    description: Keep the big ones\n"
                ),
                "(name)",
            ),
            (
                "two new steps",
                format!("{first}  - name: extra\n    sql: models/09_extra.sql\n{recorded}"),
                "(name)",
            ),
            ("no new step", first.to_string(), "(name)"),
            (
                "another description",
                format!(
                    "{first}  - name: big\n    sql: models/02_big.sql\n    description: Keep the small ones\n"
                ),
                "(description)",
            ),
            (
                "no description",
                format!("{first}  - name: big\n    sql: models/02_big.sql\n"),
                "(description)",
            ),
        ] {
            match reads_back(&manifest(&steps), 1, &step, sql) {
                Err(Error::EditTarget { path, .. }) => assert_eq!(path, expected, "{what}"),
                other => panic!("{what}: expected the {expected} refusal, got {other:?}"),
            }
        }

        let undescribed = RecordedStep {
            description: None,
            ..step.clone()
        };
        assert!(
            reads_back(
                &manifest(&format!(
                    "{first}  - name: big\n    sql: models/02_big.sql\n"
                )),
                1,
                &undescribed,
                sql
            )
            .is_ok(),
            "a step asked for with no description is refused when it reads back with none"
        );
        match reads_back(
            &manifest(&format!("{first}{recorded}")),
            1,
            &undescribed,
            sql,
        ) {
            Err(Error::EditTarget { path, .. }) => assert_eq!(path, "(description)"),
            other => panic!("a description nobody asked for: got {other:?}"),
        }
    }

    /// The read-back check is wired into the record path: a name or a
    /// description the gates would have refused, handed straight to the splice,
    /// reads back as something else and is refused by the check, naming the
    /// part, with the directory untouched.
    #[test]
    fn past_the_gates_a_step_that_does_not_read_back_is_refused_with_the_directory_untouched() {
        for (what, name, description, expected) in [
            ("a name holding ` #`", "big # draft", None, "(name)"),
            (
                "a description that spans lines",
                "big",
                Some("Keep the big ones\n      and no others"),
                "(description)",
            ),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let manifest = dir.path().join(MANIFEST_FILENAME);
            std::fs::write(
                &manifest,
                "name: shop\nsteps:\n  - name: orders\n    sql: models/01_orders.sql\n",
            )
            .unwrap();
            let step = RecordedStep {
                name: name.to_string(),
                sql: "SELECT 1;".to_string(),
                provenance: "test".to_string(),
                description: description.map(str::to_string),
            };
            match record_gated(dir.path(), &step) {
                Err(Error::EditTarget { path, .. }) => assert_eq!(path, expected, "{what}"),
                other => panic!("{what}: expected the {expected} refusal, got {other:?}"),
            }
            assert_eq!(
                std::fs::read_to_string(&manifest).unwrap(),
                "name: shop\nsteps:\n  - name: orders\n    sql: models/01_orders.sql\n",
                "{what}: the manifest changed"
            );
            assert!(
                !dir.path().join("models").exists(),
                "{what}: a model was written"
            );
        }
    }

    #[test]
    fn a_sql_step_gets_the_line_where_the_line_and_the_statement_parse() {
        for (sql, expected) in [
            // Each form of `SELECT`: bare, ended by a `;`, with a trailing comment,
            // a `WITH`, and a statement that opens with `FROM`.
            ("SELECT 1", "CREATE OR REPLACE TABLE \"t\" AS\nSELECT 1\n"),
            ("SELECT 1;", "CREATE OR REPLACE TABLE \"t\" AS\nSELECT 1;\n"),
            (
                "SELECT 1 -- one",
                "CREATE OR REPLACE TABLE \"t\" AS\nSELECT 1 -- one\n",
            ),
            (
                "WITH x AS (SELECT 1) SELECT * FROM x",
                "CREATE OR REPLACE TABLE \"t\" AS\nWITH x AS (SELECT 1) SELECT * FROM x\n",
            ),
            (
                "FROM orders SELECT id",
                "CREATE OR REPLACE TABLE \"t\" AS\nFROM orders SELECT id\n",
            ),
            // A final newline the statement has is the one it keeps.
            ("SELECT 1\n", "CREATE OR REPLACE TABLE \"t\" AS\nSELECT 1\n"),
            // DuckDB 2.0 parses `CREATE TABLE … AS DESCRIBE …` and `… AS SHOW TABLES`,
            // and runs them, so these get the line too.
            (
                "DESCRIBE orders",
                "CREATE OR REPLACE TABLE \"t\" AS\nDESCRIBE orders\n",
            ),
            (
                "SHOW TABLES\n",
                "CREATE OR REPLACE TABLE \"t\" AS\nSHOW TABLES\n",
            ),
            // A statement the line cannot go in front of is written as typed, with
            // one final newline: the line before it is a parser error.
            (
                "CREATE TABLE t2 AS SELECT 1",
                "CREATE TABLE t2 AS SELECT 1\n",
            ),
            (
                "UPDATE orders SET amount = 0",
                "UPDATE orders SET amount = 0\n",
            ),
            (
                "INSERT INTO orders VALUES (1)\n",
                "INSERT INTO orders VALUES (1)\n",
            ),
        ] {
            assert_eq!(
                sql_step_contents("t", sql).expect("the statement is recorded"),
                expected,
                "{sql:?}"
            );
        }
    }

    #[test]
    fn the_line_quotes_a_name_that_would_read_as_sql() {
        assert_eq!(
            sql_step_contents("a\"b c", "SELECT 1").unwrap(),
            "CREATE OR REPLACE TABLE \"a\"\"b c\" AS\nSELECT 1\n"
        );
    }

    #[test]
    fn a_statement_written_as_typed_never_opens_with_the_generated_line() {
        for sql in [
            "-- generated: mine\nUPDATE orders SET amount = 0",
            "  -- generated: mine\nINSERT INTO orders VALUES (1)",
        ] {
            let refusal = sql_step_contents("t", sql)
                .expect_err("refused")
                .to_string();
            assert!(refusal.contains("-- generated:"), "{sql:?}: {refusal}");
        }
        // A `SELECT`'s file, and on DuckDB 2.0 a `DESCRIBE`'s, opens with the line arc
        // adds, so it never reads so.
        for sql in [
            "-- generated: mine\nSELECT 1",
            "-- generated: mine\nDESCRIBE orders",
        ] {
            let contents = sql_step_contents("t", sql).unwrap();
            assert!(!sql_is_generated(&contents), "{contents:?}");
        }
    }

    #[test]
    fn a_multi_line_provenance_is_refused() {
        match one_line("line one\nline two") {
            Err(Error::EditTarget { path, .. }) => assert_eq!(path, "(provenance)"),
            other => panic!("expected EditTarget, got {other:?}"),
        }
    }

    #[test]
    fn names_that_would_not_read_back_as_themselves_are_refused() {
        for hostile in [
            "",                      // nothing to record
            "x\n    timeout_sec: 1", // field injection
            "x\n  - name: injected", // step injection
            "x\ry",                  // carriage return is a break too
            "x\ty",                  // tab is a control character
            "top10 # draft",         // '#' silently truncates
            "a: b",                  // ':' opens a mapping
            " padded",               // YAML drops the padding
            "padded ",               // ... on either side
            "- item",                // leading indicator
            "[list]",                // leading indicator
            "*anchor",               // leading indicator
        ] {
            match valid_step_name(hostile) {
                Err(Error::EditTarget { path, .. }) => assert_eq!(path, "(name)", "{hostile:?}"),
                other => panic!("expected {hostile:?} refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn plain_names_pass_the_gate() {
        for plain in ["filter_tides", "top-10 ports", "dover", "Reprise 2", "café"] {
            assert!(valid_step_name(plain).is_ok(), "{plain:?} should pass");
        }
    }

    #[test]
    fn rollback_removes_the_model_and_a_directory_this_promotion_created() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let model = models.join("01_x.sql");
        std::fs::write(&model, "SELECT 1;").unwrap();

        remove_orphan_model(&model, &models, true);
        assert!(
            !models.exists(),
            "a created dir is taken back out with the model"
        );
    }

    #[test]
    fn rollback_leaves_a_pre_existing_models_directory_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let model = models.join("01_x.sql");
        std::fs::write(&model, "SELECT 1;").unwrap();

        remove_orphan_model(&model, &models, false);
        assert!(!model.exists(), "the orphan model is removed");
        assert!(
            models.exists(),
            "a directory the promotion found is not its to remove"
        );
    }

    #[test]
    fn rollback_never_takes_out_a_directory_something_else_now_occupies() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        let model = models.join("01_x.sql");
        std::fs::write(&model, "SELECT 1;").unwrap();
        let bystander = models.join("theirs.sql");
        std::fs::write(&bystander, "SELECT 2;").unwrap();

        remove_orphan_model(&model, &models, true);
        assert!(!model.exists(), "the orphan model is removed");
        assert!(bystander.exists(), "the bystander survives");
        assert!(models.exists(), "a non-empty directory is left standing");
    }

    // ------------------------------------------------------- the catalogue

    /// `text` is one sentence: it opens with a capital, ends at its one full
    /// stop, and holds no line break. The panel prints it on its own line.
    fn is_one_sentence(text: &str) -> bool {
        let Some(body) = text.strip_suffix('.') else {
            return false;
        };
        text.chars().next().is_some_and(char::is_uppercase)
            && !text.contains('\n')
            && !body.contains(['.', '?', '!'])
    }

    #[test]
    fn the_sentence_check_refuses_what_is_not_one_sentence() {
        assert!(is_one_sentence("The table whose rows are kept."));
        for not_one in [
            "",
            "the table.",
            "The table",
            "The table. The rows.",
            "The table\nwhose rows.",
            "Is it? Yes.",
        ] {
            assert!(
                !is_one_sentence(not_one),
                "{not_one:?} was read as one sentence"
            );
        }
    }

    #[test]
    fn every_long_name_is_lower_case_hyphenated_and_appears_once() {
        let mut seen = std::collections::BTreeSet::new();
        for op in operations() {
            assert!(
                !op.long_name.is_empty()
                    && op
                        .long_name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c == '-'),
                "`{}` is not a lower case, hyphenated long name",
                op.long_name
            );
            assert!(
                seen.insert(op.long_name),
                "`{}` is held twice, so a lookup could not tell them apart",
                op.long_name
            );
            assert!(
                is_one_sentence(op.summary),
                "`{}` has no one-sentence summary: {:?}",
                op.long_name,
                op.summary
            );
        }
    }

    /// The name of every operation and every operator this build describes.
    fn described_names() -> Vec<&'static str> {
        operations()
            .iter()
            .map(|op| op.long_name)
            .chain(crate::operator::catalog_names())
            .collect()
    }

    /// The kinds a role may be of: the four kinds of asset arc records, a column,
    /// and `any`, for a role whose kind each value decides.
    const ROLE_KINDS: [&str; 6] = ["table", "file", "directory", "pattern", "column", "any"];

    // Every description, of an operation or of an operator, is read the same way
    // by a caller: the same five keys, and each role in `reads` and `writes` named
    // once, of a kind arc knows, with a sentence to print, an `of` that points back
    // at an earlier read that holds columns, and `min_items` on a list alone. An
    // operation reads a table first; an operator may read nothing.
    #[test]
    fn every_description_names_each_role_once_in_the_one_shape() {
        for name in described_names() {
            let description =
                describe(name).unwrap_or_else(|| panic!("`{name}` is held but has no description"));
            let mut keys: Vec<&str> = description
                .as_object()
                .expect("a description is an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                ["long_name", "parameters", "reads", "summary", "writes"],
                "`{name}`: the description holds a key the shape does not name:\n{description}"
            );
            assert_eq!(description["long_name"], name, "`{name}`: {description}");
            assert!(
                description["summary"].as_str().is_some_and(is_one_sentence),
                "`{name}` has no one-sentence summary: {}",
                description["summary"]
            );
            assert_eq!(
                description["parameters"]["type"], "object",
                "`{name}`: `parameters` is not an object schema"
            );

            let reads = description["reads"].as_array().expect("`reads` is a list");
            let writes = description["writes"]
                .as_array()
                .expect("`writes` is a list");
            if operation(name).is_some() {
                assert_eq!(
                    reads.first().map(|r| &r["kind"]),
                    Some(&serde_json::json!("table")),
                    "`{name}` is an operation and does not read a table first"
                );
            }
            for (direction, roles) in [("reads", reads), ("writes", writes)] {
                let mut seen: Vec<&str> = Vec::new();
                for role in roles {
                    let role_name = role["name"].as_str().expect("a role has a name");
                    let kind = role["kind"].as_str().expect("a role has a kind");
                    let list = role["list"].as_bool().expect("`list` is a boolean");
                    assert!(
                        role["required"].is_boolean(),
                        "`{name}` {direction} `{role_name}`: `required` is not a boolean"
                    );
                    assert!(
                        ROLE_KINDS.contains(&kind),
                        "`{name}` {direction} `{role_name}` has kind `{kind}`, which arc does not know"
                    );
                    assert!(
                        !seen.contains(&role_name),
                        "`{name}` {direction} `{role_name}` twice"
                    );
                    assert!(
                        role["description"].as_str().is_some_and(is_one_sentence),
                        "`{name}` {direction} `{role_name}`, which has no one-sentence description: {}",
                        role["description"]
                    );
                    let mut expected = vec!["description", "kind", "list", "name", "required"];
                    if kind == "column" {
                        expected.push("of");
                    }
                    if list {
                        expected.push("min_items");
                    }
                    expected.sort_unstable();
                    let mut held: Vec<&str> = role
                        .as_object()
                        .expect("a role is an object")
                        .keys()
                        .map(String::as_str)
                        .collect();
                    held.sort_unstable();
                    assert_eq!(
                        held, expected,
                        "`{name}` {direction} `{role_name}`: `of` belongs on a column alone and \
                         `min_items` on a list alone:\n{role}"
                    );
                    if kind == "column" {
                        let of = role["of"].as_str().expect("`of` is a string");
                        assert!(
                            reads
                                .iter()
                                .take_while(|r| *r != role)
                                .any(|r| r["name"] == of
                                    && ["table", "file"]
                                        .contains(&r["kind"].as_str().unwrap_or(""))),
                            "`{name}` {direction} the column `{role_name}` `of` `{of}`, which is no \
                             earlier read of a table or a file"
                        );
                    }
                    seen.push(role_name);
                }
            }
        }
    }

    // One lookup answers for both catalogues, so a name held in both would be
    // answered as the operation and the operator would never be described.
    #[test]
    fn no_operation_is_called_what_an_operator_is_called() {
        for op in operations() {
            assert!(
                !crate::operator::catalog_names().contains(&op.long_name)
                    && crate::operator::with_schema(op.long_name).is_none(),
                "`{}` is the long name of an operation and the name of an operator, so \
                 describing it can answer for only one of them",
                op.long_name
            );
        }
    }

    #[test]
    fn an_operation_is_found_by_its_exact_long_name_and_no_other_spelling() {
        assert_eq!(
            operation(FILTER_ROWS).map(|op| op.long_name),
            Some(FILTER_ROWS)
        );
        for near_miss in ["", "Filter-Rows", "filter", "filter-rows ", "filter_rows"] {
            assert!(
                operation(near_miss).is_none(),
                "`{near_miss}` was found as if it were `{FILTER_ROWS}`"
            );
        }
    }

    #[test]
    fn a_description_extends_the_listing_with_what_it_reads_and_writes_and_its_schema() {
        let op = operation(FILTER_ROWS).expect("the filter is held");
        let listing = op.listing();
        let description = op.description();

        assert_eq!(listing["long_name"], FILTER_ROWS);
        assert_eq!(description["long_name"], listing["long_name"]);
        assert_eq!(description["summary"], listing["summary"]);
        assert_eq!(listing["summary"], op.summary);
        assert_eq!(
            description["reads"],
            serde_json::json!([
                {
                    "name": "table",
                    "kind": "table",
                    "list": false,
                    "required": true,
                    "description": op.reads[0].description,
                },
                {
                    "name": "column",
                    "kind": "column",
                    "list": false,
                    "required": false,
                    "of": "table",
                    "description": op.reads[1].description,
                },
            ]),
            "the filter reads a table, then a column of that table"
        );
        assert_eq!(
            description["writes"],
            serde_json::json!([
                {
                    "name": "out",
                    "kind": "table",
                    "list": false,
                    "required": true,
                    "description": STEP_TABLE.description,
                },
            ]),
            "the filter writes the one table its step makes"
        );
        assert_eq!(description["parameters"], filter_rows_parameters());
        assert_eq!(describe(FILTER_ROWS), Some(description));
    }

    #[test]
    fn the_filter_takes_one_required_condition_and_no_other_key() {
        let schema = filter_rows_parameters();
        let where_ = &schema["properties"]["where"];

        assert_eq!(
            schema["required"],
            serde_json::json!(["where"]),
            "`where` is the one required parameter"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is closed to any other key"
        );
        assert_eq!(
            schema["properties"].as_object().map(|p| p.len()),
            Some(1),
            "`where` is the one parameter; the column is not an argument"
        );
        assert_eq!(where_["type"], "string", "`where` is a string");
        assert_eq!(
            where_["x-kind"], "condition",
            "`where` does not say its value is a condition"
        );
        assert!(
            where_["description"]
                .as_str()
                .is_some_and(|d| is_one_sentence(d.split_inclusive(". ").next().unwrap().trim())),
            "`where` has no description whose first sentence the panel can print: {where_}"
        );
        for constraint in ["enum", "pattern"] {
            assert!(
                where_.get(constraint).is_none(),
                "`where` holds `{constraint}`, which would hold the condition to it: {where_}"
            );
        }
    }

    #[test]
    fn the_condition_offers_six_comparisons_by_word_and_sign_in_order() {
        let schema = filter_rows_parameters();
        assert_eq!(
            schema["properties"]["where"]["x-comparisons"],
            serde_json::json!([
                { "word": "is", "sign": "=" },
                { "word": "is not", "sign": "!=" },
                { "word": "over", "sign": ">" },
                { "word": "under", "sign": "<" },
                { "word": "between", "sign": "between" },
                { "word": "is null", "sign": "is null" },
            ]),
            "the comparisons offered, in order, as a word and a sign"
        );
    }

    #[test]
    fn the_sort_takes_one_required_order_and_no_other_key() {
        let schema = sort_rows_parameters();
        let order_by = &schema["properties"]["order_by"];

        assert_eq!(
            schema["required"],
            serde_json::json!(["order_by"]),
            "`order_by` is the one required parameter"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is closed to any other key"
        );
        assert_eq!(
            schema["properties"].as_object().map(|p| p.len()),
            Some(1),
            "`order_by` is the one parameter; the column is not an argument"
        );
        assert_eq!(order_by["type"], "string", "`order_by` is a string");
        assert_eq!(
            order_by["x-kind"], "order",
            "`order_by` does not say its value is an order"
        );
        assert!(
            order_by["description"]
                .as_str()
                .is_some_and(|d| !d.trim().is_empty()),
            "`order_by` has no description: {order_by}"
        );
        // Described by its key, recorded as written: no `title`, and nothing that
        // would hold the order to a column or to the directions listed.
        for absent in ["title", "enum", "pattern", "x-comparisons"] {
            assert!(
                order_by.get(absent).is_none(),
                "`order_by` holds `{absent}`, which it must not: {order_by}"
            );
        }
    }

    #[test]
    fn the_order_offers_two_directions_by_word_and_sign_in_order() {
        let schema = sort_rows_parameters();
        assert_eq!(
            schema["properties"]["order_by"]["x-directions"],
            serde_json::json!([
                { "word": "ascending", "sign": "asc" },
                { "word": "descending", "sign": "desc" },
            ]),
            "the directions offered, in order, as a word and a sign"
        );
    }

    #[test]
    fn the_hide_takes_one_required_column_and_no_other_key() {
        let schema = hide_column_parameters();
        let column = &schema["properties"]["column"];

        assert_eq!(
            schema["required"],
            serde_json::json!(["column"]),
            "`column` is not the one required parameter"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is open to a key it does not name"
        );
        assert_eq!(
            schema["properties"].as_object().map(|p| p.len()),
            Some(1),
            "the schema takes a parameter besides `column`"
        );
        assert_eq!(column["type"], "string", "`column` is not a string");
        assert_eq!(
            column["x-kind"], "column",
            "`column` does not say its value is a column name"
        );
        assert!(
            column["description"]
                .as_str()
                .is_some_and(|d| !d.trim().is_empty()),
            "`column` has no description: {column}"
        );
    }

    #[test]
    fn a_column_is_written_bare_only_when_duckdb_reads_it_bare_as_that_column() {
        // A plain name DuckDB reads bare as the column is written bare.
        assert_eq!(column_ident("amount"), "amount");
        assert_eq!(column_ident("big_amount"), "big_amount");
        // A reserved word, a name holding a space or a `"`, and one opening with a
        // digit are quoted, each `"` doubled. Writing any of them bare reddens this.
        assert_eq!(column_ident("order"), "\"order\"");
        assert_eq!(column_ident("my col"), "\"my col\"");
        assert_eq!(column_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(column_ident("1st"), "\"1st\"");
        // A plain word DuckDB parses as something other than a column — a
        // constant — is quoted too, so it is read as the column it names.
        assert_eq!(column_ident("true"), "\"true\"");
    }

    #[test]
    fn the_hide_writes_the_column_into_an_exclude_of_every_column() {
        let arguments = arguments(serde_json::json!({ "column": "note" }));
        let recording = Recording {
            name: "no_note",
            on: "orders",
            arguments: &arguments,
        };
        assert_eq!(
            hide_column_sql(&recording),
            "CREATE OR REPLACE TABLE \"no_note\" AS\nSELECT * EXCLUDE (note)\nFROM orders;\n"
        );
    }

    #[test]
    fn an_empty_or_blank_column_is_refused_naming_the_argument_and_the_operation() {
        let op = operation(HIDE_COLUMN).unwrap();
        for blank in ["", " "] {
            let err = op
                .admit(&arguments(serde_json::json!({ "column": blank })))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("the column `column` of `hide-column` is empty or blank"),
                "{blank:?} was refused with another message: {err}"
            );
        }
        assert!(
            op.admit(&arguments(serde_json::json!({ "column": "note" })))
                .is_ok(),
            "a column with a name was refused"
        );
    }

    #[test]
    fn the_rename_takes_a_required_column_and_to_and_no_other_key() {
        let schema = rename_column_parameters();

        assert_eq!(
            schema["required"],
            serde_json::json!(["column", "to"]),
            "`column` and `to` are not the required parameters"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is open to a key it does not name"
        );
        assert_eq!(
            schema["properties"].as_object().map(|p| p.len()),
            Some(2),
            "the schema takes a parameter besides `column` and `to`"
        );
        for key in ["column", "to"] {
            let property = &schema["properties"][key];
            assert_eq!(property["type"], "string", "`{key}` is not a string");
            assert_eq!(
                property["x-kind"], "column",
                "`{key}` does not say its value is a column name"
            );
            assert!(
                property["description"]
                    .as_str()
                    .is_some_and(|d| !d.trim().is_empty()),
                "`{key}` has no description: {property}"
            );
        }
    }

    #[test]
    fn the_rename_copies_the_table_then_renames_the_column_with_an_alter() {
        let arguments = arguments(serde_json::json!({ "column": "note", "to": "order" }));
        let recording = Recording {
            name: "relabelled",
            on: "orders",
            arguments: &arguments,
        };
        assert_eq!(
            rename_column_sql(&recording),
            "BEGIN TRANSACTION;\nCREATE OR REPLACE TABLE \"relabelled\" AS\nSELECT *\nFROM orders;\nALTER TABLE \"relabelled\" RENAME COLUMN note TO \"order\";\nCOMMIT;\n"
        );
    }

    #[test]
    fn an_empty_or_blank_new_name_is_refused_naming_the_argument_and_the_operation() {
        let op = operation(RENAME_COLUMN).unwrap();
        for blank in ["", " "] {
            let err = op
                .admit(&arguments(
                    serde_json::json!({ "column": "note", "to": blank }),
                ))
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("the column `to` of `rename-column` is empty or blank"),
                "{blank:?} was refused with another message: {err}"
            );
        }
        assert!(
            op.admit(&arguments(
                serde_json::json!({ "column": "note", "to": "label" })
            ))
            .is_ok(),
            "a rename to a name was refused"
        );
    }

    #[test]
    fn the_derive_takes_a_required_column_and_expression_and_no_other_key() {
        let schema = derive_column_parameters();

        assert_eq!(
            schema["required"],
            serde_json::json!(["column", "expression"]),
            "`column` and `expression` are not the required parameters"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is open to a key it does not name"
        );
        assert_eq!(
            schema["properties"].as_object().map(|p| p.len()),
            Some(2),
            "the schema takes a parameter besides `column` and `expression`"
        );
        for (key, kind) in [("column", "column"), ("expression", "expression")] {
            let property = &schema["properties"][key];
            assert_eq!(property["type"], "string", "`{key}` is not a string");
            assert_eq!(property["x-kind"], kind, "`{key}` is not marked a {kind}");
            assert!(
                property["description"]
                    .as_str()
                    .is_some_and(|d| !d.trim().is_empty()),
                "`{key}` has no description: {property}"
            );
        }
        assert!(
            schema["properties"]["column"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("new column")),
            "`column` does not say it names the new column"
        );
    }

    #[test]
    fn the_derive_adds_the_expression_under_a_temporary_name_then_renames_it_with_an_alter() {
        let arguments = arguments(serde_json::json!({
            "column": "order",
            "expression": "amount * 2",
        }));
        let recording = Recording {
            name: "doubled_orders",
            on: "orders",
            arguments: &arguments,
        };
        assert_eq!(
            derive_column_sql(&recording),
            "BEGIN TRANSACTION;\nCREATE OR REPLACE TABLE \"doubled_orders\" AS\nSELECT *, amount * 2 AS __arc_derived\nFROM orders;\nALTER TABLE \"doubled_orders\" RENAME COLUMN __arc_derived TO \"order\";\nCOMMIT;\n"
        );
    }

    #[test]
    fn an_expression_duckdb_reads_as_one_item_of_a_bare_select_is_admitted() {
        let op = operation(DERIVE_COLUMN).unwrap();
        for expression in [
            "amount * 2",
            "upper(note)",
            "CASE WHEN amount > 100 THEN 'big' ELSE 'small' END",
            "regexp_replace(note, 'a', 'b')",
            "EXISTS (SELECT * FROM orders)", // a star inside a subquery reads its own FROM
            "count(*) OVER ()",              // `count(*)` holds no star
        ] {
            let admitted = op.admit(&arguments(
                serde_json::json!({ "column": "derived", "expression": expression }),
            ));
            assert!(admitted.is_ok(), "{expression:?} was refused: {admitted:?}");
        }
    }

    #[test]
    fn what_is_not_one_expression_is_refused_naming_the_argument() {
        let op = operation(DERIVE_COLUMN).unwrap();
        for expression in [
            "1 FROM orders",              // a select that reads a table
            "amount; DROP TABLE orders",  // a second statement
            "1) FROM orders WHERE (1",    // parentheses that close around the step
            "",                           // nothing at all
            "amount, note",               // two items
            "amount * 2 -- double",       // a comment that swallows the alias
            "amount AS __arc_probe -- c", // the alias supplied by the caller
            "1 UNION SELECT 2",           // a set operation, not a select
            "amount ORDER BY 1",          // a modifier
            "*",
            "orders.*",
            "* EXCLUDE (amount)",
            "* REPLACE (0 AS amount)",
            "COLUMNS(*)",
            "coalesce(COLUMNS('amount|id'), 0)",
            "unnest([1, 2])",
        ] {
            let err = op
                .admit(&arguments(
                    serde_json::json!({ "column": "derived", "expression": expression }),
                ))
                .expect_err(expression)
                .to_string();
            assert!(
                err.contains("`expression`"),
                "{expression:?}: the refusal does not name the argument: {err}"
            );
        }
    }

    #[test]
    fn an_expression_that_is_not_one_is_refused_in_the_expression_wording() {
        let err = probed(DERIVE_COLUMN, "expression", EXPRESSION, "amount, note")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(
                "the expression `expression` of `derive-column` is not one SQL expression"
            ),
            "{err}"
        );
    }

    #[test]
    fn a_second_statement_in_an_expression_keeps_the_wording_a_clause_gives_it() {
        for expression in ["amount; SELECT 2", "amount; DROP TABLE orders"] {
            let err = probed(DERIVE_COLUMN, "expression", EXPRESSION, expression)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(
                    "the expression `expression` holds a `;` outside a string or a comment, \
                     which would end the step's statement and begin another"
                ),
                "{expression:?}: {err}"
            );
        }
    }

    #[test]
    fn a_star_or_an_unnest_is_refused_saying_what_it_would_do_to_the_step() {
        for (expression, reason) in [
            ("*", "expands to several columns"),
            ("orders.*", "expands to several columns"),
            ("* EXCLUDE (amount)", "expands to several columns"),
            ("* REPLACE (0 AS amount)", "expands to several columns"),
            ("COLUMNS(*)", "expands to several columns"),
            (
                "coalesce(COLUMNS('amount|id'), 0)",
                "expands to several columns",
            ),
            ("concat(*COLUMNS(*))", "expands to several columns"),
            ("unnest([1, 2])", "changes the number of rows"),
            ("UNLIST([1, 2]) + 1", "changes the number of rows"),
            (
                "generate_subscripts([1, 2], 1)",
                "changes the number of rows",
            ),
        ] {
            let err = probed(DERIVE_COLUMN, "expression", EXPRESSION, expression)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!(
                    "the expression `expression` of `derive-column` {reason}"
                )),
                "{expression:?} was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn a_type_duckdb_reads_as_the_one_type_of_a_cast_is_admitted() {
        let op = operation(CAST_COLUMN).unwrap();
        for type_name in ["VARCHAR", "DECIMAL(10, 2)", "INTEGER[]"] {
            let admitted = op.admit(&arguments(
                serde_json::json!({ "column": "amount", "type": type_name }),
            ));
            assert!(admitted.is_ok(), "{type_name:?} was refused: {admitted:?}");
        }
    }

    #[test]
    fn what_is_not_one_type_is_refused_naming_the_argument() {
        let op = operation(CAST_COLUMN).unwrap();
        for type_name in [
            "INTEGER) AS x, (1",
            "INTEGER; DROP TABLE orders",
            "",
            "INTEGER) AS __arc_probe -- c",
            "INTEGER) -- c",
            "INTEGER)::DECIMAL(10,2",
            "INTEGER)::VARCHAR::DECIMAL(10,2",
            "INTEGER)::VARCHAR(1",
        ] {
            let err = op
                .admit(&arguments(
                    serde_json::json!({ "column": "amount", "type": type_name }),
                ))
                .expect_err(type_name)
                .to_string();
            assert!(
                err.contains("`type`"),
                "{type_name:?}: the refusal does not name the argument: {err}"
            );
        }
    }

    #[test]
    fn the_summary_takes_a_required_aggregates_an_optional_group_by_and_no_other_key() {
        let schema = summarise_rows_parameters();

        assert_eq!(
            schema["required"],
            serde_json::json!(["aggregates"]),
            "`aggregates` is not the one required parameter"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is open to a key it does not name"
        );
        assert_eq!(
            schema["properties"].as_object().map(|p| p.len()),
            Some(2),
            "the schema takes a parameter besides `aggregates` and `group_by`"
        );
        for key in ["aggregates", "group_by"] {
            let property = &schema["properties"][key];
            assert_eq!(property["type"], "string", "`{key}` is not a string");
            assert_eq!(
                property["x-kind"], "select-list",
                "`{key}` is not marked a select list"
            );
            assert!(
                property["description"]
                    .as_str()
                    .is_some_and(|d| d.contains("select list") && d.contains("alias")),
                "`{key}` does not say it is a select list that may carry aliases: {property}"
            );
        }
        assert!(
            schema["properties"]["group_by"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("GROUP BY 1, 2") && d.contains("GROUP BY ()")),
            "`group_by` does not say how the step groups with and without it"
        );
    }

    #[test]
    fn the_summary_groups_by_the_places_of_its_group_by_or_by_nothing_without_one() {
        let grouped = arguments(serde_json::json!({
            "group_by": "amount > 100 AS big",
            "aggregates": "count(*) AS n",
        }));
        let recording = Recording {
            name: "order_counts",
            on: "orders",
            arguments: &grouped,
        };
        assert_eq!(
            summarise_rows_sql(&recording),
            "CREATE OR REPLACE TABLE \"order_counts\" AS\nSELECT amount > 100 AS big, count(*) AS n\nFROM orders\nGROUP BY 1;\n"
        );
        let whole = arguments(serde_json::json!({ "aggregates": "count(*) AS n" }));
        let recording = Recording {
            name: "order_counts",
            on: "orders",
            arguments: &whole,
        };
        assert_eq!(
            summarise_rows_sql(&recording),
            "CREATE OR REPLACE TABLE \"order_counts\" AS\nSELECT count(*) AS n\nFROM orders\nGROUP BY ();\n"
        );
        let two = arguments(serde_json::json!({
            "group_by": "amount > 100 AS big, concat(note, ',') AS tagged",
            "aggregates": "count(*) AS n",
        }));
        let recording = Recording {
            name: "order_counts",
            on: "orders",
            arguments: &two,
        };
        assert!(
            summarise_rows_sql(&recording).contains("\nGROUP BY 1, 2;\n"),
            "a group_by of two items, one holding a comma in a string, is not grouped by two places"
        );
    }

    #[test]
    fn a_select_list_duckdb_reads_as_the_items_of_a_bare_select_is_admitted() {
        let op = operation(SUMMARISE_ROWS).unwrap();
        for (aggregates, group_by) in [
            ("count(*) AS n, sum(amount) AS total", None),
            ("count(*)", Some("note")),
            ("count(*) AS n", Some("amount > 100 AS big, note")),
            (
                "count(*) AS n",
                Some("date_trunc('month', DATE '2026-01-02') AS month"),
            ),
            ("max(amount) FILTER (WHERE id > 1) AS top", None),
            ("count(*) AS n", Some("(SELECT max(id) FROM orders) AS m")),
        ] {
            let mut given = serde_json::json!({ "aggregates": aggregates });
            if let Some(group_by) = group_by {
                given["group_by"] = group_by.into();
            }
            let admitted = op.admit(&arguments(given));
            assert!(
                admitted.is_ok(),
                "{aggregates:?} by {group_by:?} was refused: {admitted:?}"
            );
        }
    }

    #[test]
    fn what_is_not_one_select_list_is_refused_naming_the_argument() {
        let op = operation(SUMMARISE_ROWS).unwrap();
        for aggregates in [
            "count(*) FROM other",             // a select that reads a table
            "a UNION SELECT b",                // a set operation, not a select
            "count(*); DROP TABLE orders",     // a second statement
            "count(*) AS n -- all",            // a comment that swallows the sentinel item
            "",                                // nothing at all
            "count(*) AS n, 1 AS __arc_probe", // the sentinel supplied by the caller
            "*",                               // a star that widens the list to every column
            "count(*), orders.*",
            "COLUMNS('amount|id')",
            "unnest([1, 2]) AS u", // more rows for the summary
        ] {
            let err = op
                .admit(&arguments(serde_json::json!({ "aggregates": aggregates })))
                .expect_err(aggregates)
                .to_string();
            assert!(
                err.contains("`aggregates`"),
                "{aggregates:?}: the refusal does not name the argument: {err}"
            );
        }
        for group_by in ["big -- by size", "*", "unnest([1, 2])", "note FROM other"] {
            let err = op
                .admit(&arguments(serde_json::json!({
                    "aggregates": "count(*) AS n",
                    "group_by": group_by,
                })))
                .expect_err(group_by)
                .to_string();
            assert!(
                err.contains("`group_by`"),
                "{group_by:?}: the refusal does not name the argument: {err}"
            );
        }
    }

    #[test]
    fn a_select_list_that_is_not_one_is_refused_in_the_select_list_wording() {
        for (list, reason) in [
            ("count(*) AS n -- all", "is not one SQL select list"),
            ("note, *", "has an item that expands to several columns"),
            ("unnest([1, 2])", "changes the number of rows"),
            (
                "generate_subscripts([1, 2], 1) AS i",
                "changes the number of rows",
            ),
        ] {
            let err = probed(SUMMARISE_ROWS, "group_by", SELECT_LIST, list)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!(
                    "the select list `group_by` of `summarise-rows` {reason}"
                )),
                "{list:?} was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn the_dedupe_takes_a_key_and_an_order_each_needing_the_other_and_no_other_key() {
        let schema = dedupe_rows_parameters();

        assert_eq!(
            schema.get("required"),
            None,
            "the dedupe requires an argument, where both are optional"
        );
        assert_eq!(
            schema["dependentRequired"],
            serde_json::json!({ "key": ["order_by"], "order_by": ["key"] }),
            "`key` and `order_by` do not each need the other"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is open to a key it does not name"
        );
        assert_eq!(schema["properties"]["key"]["x-kind"], "partition");
        assert_eq!(schema["properties"]["order_by"]["x-kind"], "order");
        assert_eq!(
            schema["properties"]["order_by"]["x-directions"],
            sort_rows_parameters()["properties"]["order_by"]["x-directions"],
            "the dedupe's order does not offer the directions a sort's does"
        );
    }

    #[test]
    fn the_dedupe_keeps_distinct_rows_or_the_first_of_each_key() {
        let whole = arguments(serde_json::json!({}));
        let recording = Recording {
            name: "unique_orders",
            on: "orders",
            arguments: &whole,
        };
        assert_eq!(
            dedupe_rows_sql(&recording),
            "CREATE OR REPLACE TABLE \"unique_orders\" AS\nSELECT DISTINCT *\nFROM orders;\n"
        );
        let keyed = arguments(serde_json::json!({ "key": "note", "order_by": "amount desc" }));
        let recording = Recording {
            name: "unique_orders",
            on: "orders",
            arguments: &keyed,
        };
        assert_eq!(
            dedupe_rows_sql(&recording),
            "CREATE OR REPLACE TABLE \"unique_orders\" AS\nSELECT *\nFROM orders\nQUALIFY row_number() OVER (PARTITION BY note ORDER BY amount desc\n) = 1;\n"
        );
    }

    #[test]
    fn a_key_and_an_order_that_read_as_one_window_are_admitted() {
        let op = operation(DEDUPE_ROWS).unwrap();
        for (key, order_by) in [
            ("note", "amount desc"),
            ("lower(note), id", "amount, id desc NULLS LAST"),
            ("note", "amount desc -- the largest first"),
            ("note", "id * -1"),
            // constant by other means than a literal, which is the analyst's to avoid
            ("note", "(SELECT max(id) FROM orders)"),
        ] {
            let admitted = op.admit(&arguments(
                serde_json::json!({ "key": key, "order_by": order_by }),
            ));
            assert!(
                admitted.is_ok(),
                "{key:?} by {order_by:?} was refused: {admitted:?}"
            );
        }
        assert!(op.admit(&arguments(serde_json::json!({}))).is_ok());
    }

    #[test]
    fn a_key_or_an_order_given_alone_is_refused_naming_both() {
        let op = operation(DEDUPE_ROWS).unwrap();
        for (given, needed) in [("key", "order_by"), ("order_by", "key")] {
            let err = op
                .admit(&arguments(serde_json::json!({ (given): "note" })))
                .expect_err(given)
                .to_string();
            assert!(
                err.contains(&format!(
                    "`{given}` of `dedupe-rows` needs `{needed}` as well"
                )),
                "{given} alone was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn a_key_or_an_order_that_does_not_stay_in_the_window_is_refused() {
        let op = operation(DEDUPE_ROWS).unwrap();
        for (key, order_by, named) in [
            ("note AS n", "amount", "`order_by`"), // an alias, which PARTITION BY does not take
            ("note ORDER BY amount", "amount", "`key`"), // a clause of the key's own
            (
                "note) = 1 OR TRUE OR row_number() OVER (PARTITION BY note",
                "amount",
                "`key`",
            ), // a key that closes the window
            ("*", "amount", "`key`"),              // every column at once
            ("note -- by note", "amount", "`key`"), // a comment that swallows the order
            ("note", "amount LIMIT 1", "`order_by`"), // a clause after the order
            ("note", "amount) = 1 OR (1", "`order_by`"), // an order that closes the window
            ("note", "amount ROWS UNBOUNDED PRECEDING", "`order_by`"), // a frame
            ("note", "amount; DROP TABLE orders", "`order_by`"),
            (
                "1",
                "amount",
                "the partition key `key` of `dedupe-rows` holds a constant",
            ),
            (
                "note",
                "2 desc",
                "the order `order_by` of `dedupe-rows` holds a constant",
            ),
            (
                "note",
                "'x'",
                "the order `order_by` of `dedupe-rows` holds a constant",
            ),
        ] {
            let err = op
                .admit(&arguments(
                    serde_json::json!({ "key": key, "order_by": order_by }),
                ))
                .expect_err(key)
                .to_string();
            assert!(
                err.contains(named),
                "{key:?} by {order_by:?}: the refusal does not name {named}: {err}"
            );
        }
    }

    #[test]
    fn a_pair_that_is_not_one_window_is_refused_naming_both_arguments() {
        for (key, order_by) in [
            ("note AS n", "amount"),
            ("note", "amount LIMIT 1"),
            ("note", "amount) = 1 OR (1"),
            ("note", "amount ROWS UNBOUNDED PRECEDING"),
            ("note", "amount\n) AS __arc_probe --"),
        ] {
            let err = dedupe_rows_checks(&arguments(
                serde_json::json!({ "key": key, "order_by": order_by }),
            ))
            .unwrap_err()
            .to_string();
            assert!(
                err.contains(
                    "the partition key `key` and the order `order_by` of `dedupe-rows` do not read as one `row_number()` window"
                ),
                "{key:?} by {order_by:?} was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn a_partition_key_that_is_not_one_is_refused_in_the_partition_key_wording() {
        for (key, reason) in [
            ("note -- by note", "is not one SQL partition key"),
            ("note, *", "has an item that expands to several columns"),
            ("unnest([1, 2])", "changes the number of rows"),
        ] {
            let err = probed(DEDUPE_ROWS, "key", PARTITION, key)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(&format!(
                    "the partition key `key` of `dedupe-rows` {reason}"
                )),
                "{key:?} was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn the_limit_takes_a_count_of_at_least_zero_and_an_optional_order() {
        let schema = limit_rows_parameters();
        assert_eq!(schema["required"], serde_json::json!(["count"]));
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is open to a key it does not name"
        );
        assert_eq!(schema["properties"]["count"]["type"], "integer");
        assert_eq!(schema["properties"]["count"]["minimum"], 0);
        assert_eq!(schema["properties"]["count"]["maximum"], i64::MAX);
        assert_eq!(schema["properties"]["order_by"]["x-kind"], "order");
        assert_eq!(
            schema["properties"]["order_by"]["x-directions"],
            sort_rows_parameters()["properties"]["order_by"]["x-directions"],
            "the limit's order does not offer the directions a sort's does"
        );
    }

    #[test]
    fn the_limit_keeps_the_first_rows_in_the_order_or_as_they_come() {
        for (given, query) in [
            (
                serde_json::json!({ "count": 2 }),
                "SELECT *\nFROM orders\nLIMIT 2",
            ),
            (
                serde_json::json!({ "count": 2, "order_by": "amount desc" }),
                "SELECT *\nFROM orders\nORDER BY amount desc\nLIMIT 2",
            ),
            (
                serde_json::json!({ "count": 0, "order_by": "amount -- c" }),
                "SELECT *\nFROM orders\nORDER BY amount -- c\nLIMIT 0",
            ),
        ] {
            let given = arguments(given);
            let recording = Recording {
                name: "top_orders",
                on: "orders",
                arguments: &given,
            };
            assert_eq!(
                limit_rows_sql(&recording),
                format!("CREATE OR REPLACE TABLE \"top_orders\" AS\n{query};\n")
            );
        }
    }

    #[test]
    fn typed_arguments_turn_an_integer_argument_into_an_integer_and_leave_the_rest() {
        let typed = typed_arguments(
            LIMIT_ROWS,
            arguments(serde_json::json!({ "count": "2", "order_by": "1" })),
        );
        assert_eq!(
            serde_json::Value::Object(typed),
            serde_json::json!({ "count": 2, "order_by": "1" })
        );
        for (given, typed) in [
            ("+2", serde_json::json!(2)),
            (
                "9223372036854775808",
                serde_json::json!(9_223_372_036_854_775_808_u64),
            ),
            ("100000000000000000000", serde_json::json!(1e20)),
            (&"9".repeat(400), serde_json::json!(f64::MAX)),
            (
                &format!("-{}", "9".repeat(400)),
                serde_json::json!(-f64::MAX),
            ),
        ] {
            assert_eq!(
                typed_arguments(LIMIT_ROWS, arguments(serde_json::json!({ "count": given })))["count"],
                typed,
                "{given:?}"
            );
        }
        for (operation, given) in [
            (LIMIT_ROWS, serde_json::json!({ "count": "ten" })),
            (LIMIT_ROWS, serde_json::json!({ "count": "2.5" })),
            (LIMIT_ROWS, serde_json::json!({ "count": " 2" })),
            (LIMIT_ROWS, serde_json::json!({ "count": "1e3" })),
            (LIMIT_ROWS, serde_json::json!({ "count": "+" })),
            (FILTER_ROWS, serde_json::json!({ "where": "2" })),
            ("no-such-operation", serde_json::json!({ "count": "2" })),
        ] {
            let typed = typed_arguments(operation, arguments(given.clone()));
            assert_eq!(serde_json::Value::Object(typed), given, "{operation}");
        }
    }

    #[test]
    fn a_count_below_its_minimum_or_not_an_integer_is_refused_naming_count() {
        let op = operation(LIMIT_ROWS).unwrap();
        for (given, named) in [
            (
                serde_json::json!({ "count": -1 }),
                "the argument `count` of `limit-rows` has a minimum of 0, not -1",
            ),
            (
                serde_json::json!({ "count": 9_223_372_036_854_775_808_u64 }),
                "the argument `count` of `limit-rows` has a maximum of 9223372036854775807, \
                 not 9223372036854775808",
            ),
            (
                serde_json::json!({ "count": 1e20 }),
                "the argument `count` of `limit-rows` has a maximum of 9223372036854775807",
            ),
            (
                serde_json::json!({ "count": f64::MAX }),
                "the argument `count` of `limit-rows` has a maximum of 9223372036854775807",
            ),
            (
                serde_json::json!({ "count": "ten" }),
                "the argument `count` of `limit-rows` is an integer",
            ),
            (
                serde_json::json!({ "count": 2.5 }),
                "the argument `count` of `limit-rows` is an integer",
            ),
            (
                serde_json::json!({ "order_by": "amount" }),
                "`limit-rows` needs the argument `count`",
            ),
        ] {
            let err = op
                .admit(&arguments(given.clone()))
                .expect_err(named)
                .to_string();
            assert!(
                err.contains(named),
                "{given} was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn a_limit_order_is_admitted_unless_it_brings_a_clause_the_limit_cannot_follow() {
        let op = operation(LIMIT_ROWS).unwrap();
        for order_by in [
            "amount desc",
            "amount -- c",
            "2 desc",
            "amount, id desc NULLS LAST",
        ] {
            let admitted = op.admit(&arguments(
                serde_json::json!({ "count": 2, "order_by": order_by }),
            ));
            assert!(admitted.is_ok(), "{order_by:?} was refused: {admitted:?}");
        }
        assert!(
            op.admit(&arguments(serde_json::json!({ "count": 2 })))
                .is_ok()
        );
        for order_by in [
            "amount LIMIT 1",
            "amount OFFSET 2",
            "amount LIMIT 1 OFFSET 2",
            "amount desc;",
        ] {
            let err = op
                .admit(&arguments(
                    serde_json::json!({ "count": 2, "order_by": order_by }),
                ))
                .expect_err(order_by)
                .to_string();
            assert!(
                err.contains(
                    "the order `order_by` of `limit-rows` does not read as one `ORDER BY` the step's `LIMIT` can follow"
                ),
                "{order_by:?} was refused with another message: {err}"
            );
        }
        let err = op
            .admit(&arguments(
                serde_json::json!({ "count": 2, "order_by": "amount; DROP TABLE orders" }),
            ))
            .unwrap_err()
            .to_string();
        assert!(err.contains("the order `order_by` holds a `;`"), "{err}");
    }

    #[test]
    fn the_fill_takes_a_required_column_and_value_and_no_other_key() {
        let schema = fill_nulls_parameters();
        assert_eq!(
            schema["required"],
            serde_json::json!(["column", "value"]),
            "`column` and `value` are not the required parameters"
        );
        assert_eq!(
            schema["additionalProperties"],
            serde_json::json!(false),
            "the schema is open to a key it does not name"
        );
        assert_eq!(
            schema["properties"].as_object().map(|p| p.len()),
            Some(2),
            "the schema takes a parameter besides `column` and `value`"
        );
        for (key, kind) in [("column", "column"), ("value", "expression")] {
            let property = &schema["properties"][key];
            assert_eq!(property["type"], "string", "`{key}` is not a string");
            assert_eq!(property["x-kind"], kind, "`{key}` is not marked a {kind}");
        }
        let value = schema["properties"]["value"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(
            value.contains("SQL expression") && value.contains("fails the run"),
            "`value` does not say it is SQL, or that a type DuckDB cannot combine fails the run: {value}"
        );
    }

    #[test]
    fn the_fill_replaces_the_column_with_its_coalesce_with_the_value() {
        let arguments = arguments(serde_json::json!({ "column": "order", "value": "0" }));
        let recording = Recording {
            name: "filled_orders",
            on: "orders",
            arguments: &arguments,
        };
        assert_eq!(
            fill_nulls_sql(&recording),
            "CREATE OR REPLACE TABLE \"filled_orders\" AS\nSELECT * REPLACE (coalesce(\"order\", 0) AS \"order\")\nFROM orders;\n"
        );
    }

    #[test]
    fn a_fill_value_duckdb_reads_as_one_expression_is_admitted() {
        let op = operation(FILL_NULLS).unwrap();
        for value in ["0", "'unknown'", "id", "amount * 2", "NULL"] {
            let admitted = op.admit(&arguments(
                serde_json::json!({ "column": "amount", "value": value }),
            ));
            assert!(admitted.is_ok(), "{value:?} was refused: {admitted:?}");
        }
    }

    #[test]
    fn a_fill_value_that_is_not_one_expression_is_refused_naming_value() {
        let op = operation(FILL_NULLS).unwrap();
        for (value, named) in [
            ("0 --", "the expression `value` of `fill-nulls`"), // a comment that swallows the close
            ("0) , (1", "the expression `value` of `fill-nulls`"), // parentheses that close the coalesce
            ("0; DROP TABLE orders", "the expression `value` holds a `;`"),
            (
                "*",
                "the expression `value` of `fill-nulls` expands to several columns",
            ),
            (
                "unnest([1])",
                "the expression `value` of `fill-nulls` changes the number of rows",
            ),
        ] {
            let err = op
                .admit(&arguments(
                    serde_json::json!({ "column": "amount", "value": value }),
                ))
                .expect_err(value)
                .to_string();
            assert!(
                err.contains(named),
                "{value:?} was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn a_fragment_holding_the_sentinel_in_any_case_is_refused() {
        for expression in ["__arc_probe", "amount AS __ARC_PROBE", "'__Arc_Probe'"] {
            let err = probed(DERIVE_COLUMN, "expression", EXPRESSION, expression)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("holds `__arc_probe`"),
                "{expression:?} was refused with another message: {err}"
            );
        }
    }

    #[test]
    fn a_probe_is_bare_only_when_it_lists_items_and_does_nothing_else() {
        assert_eq!(
            probe_items("SELECT 1 AS a, 2 AS b").map(|i| i.len()),
            Some(2)
        );
        let con = duckdb::Connection::open_in_memory().unwrap();
        assert_eq!(
            bare_select(&tree(&con, "SELECT amount * 2, note").unwrap()),
            Some(2)
        );
        for probe in [
            "SELECT 1 FROM orders",
            "SELECT 1 WHERE true",
            "SELECT 1 GROUP BY 1",
            "SELECT 1 GROUP BY ALL",
            "SELECT 1 HAVING true",
            "SELECT 1 QUALIFY true",
            "SELECT 1 USING SAMPLE 1",
            "SELECT 1 ORDER BY 1",
            "SELECT 1 LIMIT 1",
            "SELECT DISTINCT 1",
            "WITH c AS (SELECT 1) SELECT 1",
            "SELECT 1 UNION SELECT 2",
            "SELECT 1; SELECT 2",
            "SELECT 1)",
            "DROP TABLE orders",
        ] {
            assert!(probe_items(probe).is_none(), "{probe:?} was read as bare");
        }
    }

    // ------------------------------------------------- recording an operation

    /// An operation with the given `parameters`, whose SQL is its `where`.
    fn operation_taking(parameters: fn() -> serde_json::Value) -> Operation {
        Operation {
            long_name: "test-op",
            summary: "A test operation.",
            reads: &[],
            writes: &[],
            parameters,
            sql: |recording| recording.text("where").to_string(),
            recognised: None,
            checks: None,
        }
    }

    #[test]
    fn an_argument_given_without_one_it_depends_on_is_refused_naming_both() {
        let op = operation_taking(|| {
            serde_json::json!({
                "type": "object",
                "properties": { "from": { "type": "string" }, "to": { "type": "string" } },
                "dependentRequired": { "from": ["to"] },
            })
        });
        assert!(
            op.admit(&arguments(serde_json::json!({ "to": "b" })))
                .is_ok()
        );
        assert!(
            op.admit(&arguments(serde_json::json!({ "from": "a", "to": "b" })))
                .is_ok()
        );
        let err = op
            .admit(&arguments(serde_json::json!({ "from": "a" })))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("the argument `from` of `test-op` needs `to` as well"),
            "the refusal does not name both arguments: {err}"
        );
    }

    fn arguments(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().expect("an object").clone()
    }

    /// The two clauses arc parse-checks, named as [`Clause::of`] names them, for
    /// the [`one_statement`] tests that call it directly.
    const WHERE: Clause = Clause {
        keyword: "WHERE",
        noun: "condition",
    };
    const ORDER_BY: Clause = Clause {
        keyword: "ORDER BY",
        noun: "order",
    };

    #[test]
    fn a_clause_is_chosen_by_x_kind_and_only_for_the_two_arc_checks() {
        let condition = Clause::of(&serde_json::json!("condition")).expect("condition is a clause");
        assert_eq!((condition.keyword, condition.noun), ("WHERE", "condition"));
        let order = Clause::of(&serde_json::json!("order")).expect("order is a clause");
        assert_eq!((order.keyword, order.noun), ("ORDER BY", "order"));
        // Any other annotation names no clause, so its value is not parse-checked;
        // this is also what pins the two arms above from being widened to a `_`
        // that would answer every x-kind.
        for other in [
            serde_json::json!("sort"),
            serde_json::json!(""),
            serde_json::json!(null),
            serde_json::json!(1),
        ] {
            assert!(Clause::of(&other).is_none(), "{other} was read as a clause");
        }
    }

    #[test]
    fn an_open_schema_admits_an_argument_it_does_not_name() {
        let op = operation_taking(
            || serde_json::json!({ "type": "object", "properties": { "a": { "type": "string" } } }),
        );
        assert!(op.admit(&arguments(serde_json::json!({ "b": 1 }))).is_ok());
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused_naming_the_argument_and_its_type() {
        let op = operation(FILTER_ROWS).unwrap();
        let err = op
            .admit(&arguments(serde_json::json!({ "where": 100 })))
            .unwrap_err()
            .to_string();
        assert!(err.contains("`where`") && err.contains("string"), "{err}");
    }

    #[test]
    fn only_a_value_annotated_as_a_condition_is_held_to_one_statement() {
        let op = operation_taking(|| {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "label": { "type": "string" },
                    "where": { "type": "string", "x-kind": "condition" },
                },
            })
        });
        // The same text — a `;` before a second, parseable statement — passes as
        // a plain `label` and is refused as a `where`, because only the value
        // annotated `x-kind: condition` is held to one statement.
        assert!(
            op.admit(&arguments(
                serde_json::json!({ "label": "a > 1; DROP TABLE t" })
            ))
            .is_ok()
        );
        assert!(
            op.admit(&arguments(
                serde_json::json!({ "where": "a > 1; DROP TABLE t" })
            ))
            .is_err()
        );
    }

    #[test]
    fn a_terminator_is_found_where_duckdb_ends_a_statement() {
        // Refused: DuckDB parses a second statement, so the `;` is a real
        // terminator — a `;` outside every string and comment with more SQL
        // after it. This is what would make the recorded step more than the
        // operation.
        for condition in [
            "a > 1; DROP TABLE orders",
            "a > 1;\nDROP TABLE orders", // a newline does not hide the terminator
            "1 = 1; SELECT 42",
        ] {
            assert!(
                one_statement("where", WHERE, condition).is_err(),
                "{condition:?} was admitted"
            );
        }
        // Recorded: DuckDB parses exactly one statement. A `;` inside a string
        // or a comment stays there, whatever DuckDB's own rule for where that
        // string or comment ends; a bare trailing `;`, and an empty statement
        // after it, add no second statement. Whether such a condition binds is a
        // later parse's to judge.
        for condition in [
            "a = ';'",
            "a > 1 -- ;",
            "a > 1 /* ; */",
            "\"a;b\" > 1",
            "$$;$$ = a",
            "a > 1;",  // bare trailing terminator: one statement, nothing after
            "a > 1;;", // ... and an empty statement after it is still nothing
            "amount > 100 UNION ALL SELECT * FROM orders", // one statement; a bind is a later parse's
        ] {
            assert!(
                one_statement("where", WHERE, condition).is_ok(),
                "{condition:?} was refused"
            );
        }
    }

    /// A probe DuckDB cannot parse at all counts `0`, and is refused, not
    /// recorded. `arc run` runs the statements the `duckdb` CLI can read in a
    /// step's file before it reaches the one it cannot, so a runnable
    /// `;`-terminated prefix with an unparseable tail — a bare word, or a `.`
    /// dot-command — would drop a table on a run though the probe as a whole
    /// does not parse. Round two read a `0` as one statement's worth of text and
    /// admitted these; round three refuses them, since arc cannot confirm they
    /// are one statement.
    #[test]
    fn a_probe_duckdb_cannot_parse_at_all_is_refused() {
        for condition in [
            "amount > 100; DROP TABLE orders; zzz", // runnable DROP, unparseable word after
            "amount > 100; DROP TABLE orders;\n.print done", // ... a `.` dot-command after
            "a = 'b;",                              // an unterminated string
            "a > 1; /*", // a runnable prefix, then an unterminated block comment
        ] {
            let refusal = one_statement("where", WHERE, condition);
            assert!(refusal.is_err(), "{condition:?} was admitted");
            let message = refusal.unwrap_err().to_string();
            assert!(
                message.contains("`where`"),
                "{condition:?}: the refusal does not name the argument"
            );
            // The count-0 message, distinct from the more-than-one-statement one:
            // deleting the `0` arm would route these through the `;` message and
            // redden here, not just leave them refused.
            assert!(
                message.contains("is not one SQL statement DuckDB can parse"),
                "{condition:?}: the refusal is not the cannot-parse one: {message}"
            );
        }
    }

    #[test]
    fn quote_ident_doubles_an_embedded_quote() {
        // The step's name is always quoted, even a plain one, so the model's
        // CREATE line reads `CREATE OR REPLACE TABLE "big_orders" AS`.
        assert_eq!(quote_ident("big_orders"), "\"big_orders\"");
        // A `"` in the name — `valid_step_name` admits one — is doubled, so the
        // CREATE parses. Removing the doubling reddens this (the mutation that
        // survived round one); a bare `"a"b"` would not parse as one identifier.
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn the_from_table_is_quoted_only_when_a_bare_name_would_not_carry_it() {
        // A plain identifier is written bare, so the model's line reads
        // `FROM orders` and DuckDB folds the reference as it did before.
        assert_eq!(from_ident("orders"), "orders");
        assert_eq!(from_ident("big_orders"), "big_orders");
        // A name a step could only have made quoted is quoted here, each `"`
        // doubled, so it cannot break out of the `FROM`. Writing it bare — the
        // round-one behaviour — reddens each of these.
        assert_eq!(from_ident("weird; drop"), "\"weird; drop\"");
        assert_eq!(from_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(from_ident("has space"), "\"has space\"");
        assert_eq!(from_ident("1st"), "\"1st\"");
        // A reserved word is a name of only plain characters that DuckDB does not
        // read bare as a table, so it is quoted, in either case. Writing it bare
        // reddens each of these.
        assert_eq!(from_ident("order"), "\"order\"");
        assert_eq!(from_ident("ORDER"), "\"ORDER\"");
        // A word DuckDB lists as a keyword and reads bare as a table is written
        // as an ordinary name is. Quoting each keyword reddens each of these.
        assert_eq!(from_ident("data"), "data");
        assert_eq!(from_ident("Data"), "Data");
    }

    /// Each word `duckdb_keywords()` lists, as listed and in upper case, names a
    /// column, and a hide of it is a step DuckDB runs that leaves out that column
    /// and no other. A column written bare where DuckDB does not read it bare as
    /// that column reddens this, with the word and the step it wrote.
    #[test]
    fn a_hide_of_a_column_named_with_each_keyword_duckdb_lists_leaves_out_that_column() {
        let con = duckdb::Connection::open_in_memory().unwrap();
        let listed: Vec<String> = con
            .prepare("SELECT keyword_name FROM duckdb_keywords()")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|word| word.unwrap())
            .collect();
        for word in listed
            .iter()
            .flat_map(|word| [word.clone(), word.to_uppercase()])
        {
            let named = if word.eq_ignore_ascii_case("kept") {
                "other"
            } else {
                "kept"
            };
            con.execute_batch(&format!(
                "CREATE OR REPLACE TABLE t AS SELECT 7 AS \"{named}\", 8 AS \"{word}\""
            ))
            .unwrap_or_else(|e| panic!("DuckDB makes no column called {word:?}: {e}"));
            let arguments = serde_json::json!({ "column": word })
                .as_object()
                .unwrap()
                .clone();
            let step = hide_column_sql(&Recording {
                name: "hidden",
                on: "t",
                arguments: &arguments,
            });
            con.execute_batch(&step).unwrap_or_else(|e| {
                panic!("hide-column of a column named {word:?} does not run:\n{step}\n{e}")
            });
            let columns: Vec<String> = con
                .prepare("SELECT name FROM pragma_table_info('hidden')")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(|name| name.unwrap())
                .collect();
            assert_eq!(
                columns,
                [named],
                "hide-column of a column named {word:?} left the wrong columns:\n{step}"
            );
        }
    }

    /// Each word `duckdb_keywords()` lists, as listed and in upper case, names a
    /// column, and a rename of it is a step DuckDB runs: with the word as the old
    /// name, from a table made with it, and as the new name, for a column called
    /// something else. Either way the columns that result are the two, the renamed
    /// one in its place. A name written bare where DuckDB does not read it bare as
    /// that column reddens this, with the word and the step it wrote.
    #[test]
    fn a_rename_of_a_column_named_with_each_keyword_duckdb_lists_renames_that_column() {
        let con = duckdb::Connection::open_in_memory().unwrap();
        let listed: Vec<String> = con
            .prepare("SELECT keyword_name FROM duckdb_keywords()")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|word| word.unwrap())
            .collect();
        for word in listed
            .iter()
            .flat_map(|word| [word.clone(), word.to_uppercase()])
        {
            let other = if word.eq_ignore_ascii_case("kept") {
                "other"
            } else {
                "kept"
            };
            // (the table's columns, the column renamed, its new name, the result)
            let cases = [
                (
                    [word.as_str(), other],
                    word.as_str(),
                    "fresh",
                    ["fresh", other],
                ),
                ([other, "old"], "old", word.as_str(), [other, word.as_str()]),
            ];
            for (made, column, to, expected) in cases {
                con.execute_batch(&format!(
                    "CREATE OR REPLACE TABLE t AS SELECT 7 AS \"{}\", 8 AS \"{}\"",
                    made[0], made[1]
                ))
                .unwrap_or_else(|e| panic!("DuckDB makes no column called {word:?}: {e}"));
                let arguments = serde_json::json!({ "column": column, "to": to })
                    .as_object()
                    .unwrap()
                    .clone();
                let step = rename_column_sql(&Recording {
                    name: "renamed",
                    on: "t",
                    arguments: &arguments,
                });
                con.execute_batch(&step).unwrap_or_else(|e| {
                    panic!("rename-column of {column:?} to {to:?} does not run:\n{step}\n{e}")
                });
                let columns: Vec<String> = con
                    .prepare("SELECT name FROM pragma_table_info('renamed')")
                    .unwrap()
                    .query_map([], |row| row.get::<_, String>(0))
                    .unwrap()
                    .map(|name| name.unwrap())
                    .collect();
                assert_eq!(
                    columns, expected,
                    "rename-column of {column:?} to {to:?} left the wrong columns:\n{step}"
                );
            }
        }
    }

    /// Each word `duckdb_keywords()` lists, as listed and in upper case, names a
    /// table, and a filter and a sort recorded on it are steps DuckDB runs and
    /// that read that table. The list is asked of the DuckDB arc is linked with,
    /// so a word DuckDB adds is in it. A name written bare where DuckDB does not
    /// read it bare reddens this, with the word and the step it wrote.
    #[test]
    fn a_filter_and_a_sort_on_a_table_named_with_each_keyword_duckdb_lists_read_that_table() {
        let con = duckdb::Connection::open_in_memory().unwrap();
        let listed: Vec<String> = con
            .prepare("SELECT keyword_name FROM duckdb_keywords()")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|word| word.unwrap())
            .collect();
        for named in ["order", "data"] {
            assert!(
                listed.iter().any(|word| word == named),
                "duckdb_keywords() lists no `{named}`, so this test reads no reserved word and no \
                 word DuckDB reads bare"
            );
        }
        let arguments = serde_json::json!({ "where": "amount > 1", "order_by": "amount" })
            .as_object()
            .unwrap()
            .clone();
        type Writes = fn(&Recording) -> String;
        let operations: [(&str, Writes); 2] = [
            ("filter-rows", filter_rows_sql),
            ("sort-rows", sort_rows_sql),
        ];
        for word in listed
            .iter()
            .flat_map(|word| [word.clone(), word.to_uppercase()])
        {
            con.execute_batch(&format!(
                "CREATE OR REPLACE TABLE \"{word}\" AS SELECT 7 AS amount, '{word}' AS marker"
            ))
            .unwrap_or_else(|e| panic!("DuckDB makes no table called {word:?}: {e}"));
            for (operation, sql) in operations {
                let step = sql(&Recording {
                    name: "kept",
                    on: &word,
                    arguments: &arguments,
                });
                con.execute_batch(&step).unwrap_or_else(|e| {
                    panic!("{operation} on a table named {word:?} does not run:\n{step}\n{e}")
                });
                let marker: String = con
                    .query_row("SELECT marker FROM kept", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(
                    marker, word,
                    "{operation} on a table named {word:?} read another table:\n{step}"
                );
                con.execute_batch("DROP TABLE kept").unwrap();
            }
        }
    }

    /// The two inputs round one admitted, because its `statement_byte_ranges`
    /// splitter read a string escape and a comment end differently from DuckDB.
    /// DuckDB's own parser ends the `E'\''` string at the escaped-then-closed
    /// quote and the `--` comment at the carriage return, so each carries a
    /// top-level `;` and a second statement, and each is refused. The lower-case
    /// `e'...'` is the same string literal.
    #[test]
    fn the_forms_a_hand_written_splitter_missed_are_refused() {
        for condition in [
            r"note = E'\'' ; DROP TABLE orders ; --'",
            r"note = e'\'' ; DROP TABLE orders ; --'",
            "amount > 100 --\r; DROP TABLE orders",
        ] {
            assert!(
                one_statement("where", WHERE, condition).is_err(),
                "{condition:?} was admitted"
            );
        }
    }

    /// An order is held to one statement by the `ORDER BY` probe, not the `WHERE`
    /// one: `amount desc` parses after `ORDER BY` and would not after `WHERE`, so
    /// the clause is what admits it. A refusal calls it an order and names its
    /// argument, and the two refusals are told apart by their wording.
    #[test]
    fn an_order_is_held_to_one_statement_by_its_own_probe_and_named_an_order() {
        // Recorded: DuckDB parses `SELECT 1 ORDER BY <order>` as one statement,
        // including a multi-column order and one carrying a `;` inside a comment
        // or a trailing `LIMIT` a later parse would judge.
        for order in [
            "amount desc",
            "region, amount desc",
            "amount desc LIMIT 1",
            "amount desc /* ; */",
            "note = 'a;b', amount desc",
        ] {
            assert!(
                one_statement("order_by", ORDER_BY, order).is_ok(),
                "{order:?} was refused"
            );
        }

        // Refused as more than one statement — a real terminator.
        let two = one_statement("order_by", ORDER_BY, "amount desc; DROP TABLE orders")
            .unwrap_err()
            .to_string();
        assert!(
            two.contains("the order `order_by`") && two.contains("`;`"),
            "a terminator refusal names the order and the `;`: {two}"
        );

        // Refused as a probe DuckDB cannot parse whole (count 0), with the message
        // distinct from the terminator one so its arm is pinned.
        let zero = one_statement("order_by", ORDER_BY, "amount desc; DROP TABLE orders; zzz")
            .unwrap_err()
            .to_string();
        assert!(
            zero.contains("the order `order_by`")
                && zero.contains("is not one SQL statement DuckDB can parse"),
            "a cannot-parse refusal names the order and says it cannot parse: {zero}"
        );
    }

    #[test]
    fn each_json_schema_type_admits_its_own_values_only() {
        let values = [
            ("string", serde_json::json!("a")),
            ("boolean", serde_json::json!(true)),
            ("integer", serde_json::json!(3)),
            ("number", serde_json::json!(2.5)),
            ("array", serde_json::json!([])),
            ("object", serde_json::json!({})),
            ("null", serde_json::Value::Null),
        ];
        for (ty, _) in &values {
            for (other, value) in &values {
                let admitted = is_of_type(value, ty);
                let expected = ty == other || (*ty == "number" && *other == "integer");
                assert_eq!(admitted, expected, "{value} as a {ty}");
            }
        }
        assert!(is_of_type(&serde_json::json!(u64::MAX), "integer"));
        assert!(is_of_type(&serde_json::json!("a"), "no-such-type"));
    }

    #[test]
    fn the_table_is_one_a_step_makes_in_any_case_and_not_a_file_it_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("models")).unwrap();
        std::fs::write(
            dir.path().join(MANIFEST_FILENAME),
            "name: shop\nsteps:\n  - name: orders\n    sql: models/orders.sql\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("models/orders.sql"),
            "CREATE TABLE orders AS SELECT 1 AS id;\nCOPY orders TO 'build/orders.csv';\n",
        )
        .unwrap();
        assert!(made_by_a_step(dir.path(), "orders").is_ok());
        assert!(made_by_a_step(dir.path(), "ORDERS").is_ok());
        let err = made_by_a_step(dir.path(), "build/orders.csv").unwrap_err();
        assert!(err.to_string().contains("`build/orders.csv`"), "{err}");
    }
}
