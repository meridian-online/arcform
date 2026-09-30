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
//! true. This module writes files and returns — it opens no database, executes
//! no SQL, and fabricates no run state. A freshly recorded step is a step that
//! has never run, and any tool showing run state must say so.
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
//! step name and provenance note are gated first — both are spliced into
//! durable text verbatim, so a value that would not read back as itself
//! (a newline, a `#`, a `:`) is refused before anything else happens, and the
//! reloaded document is checked to carry exactly the step that was asked for.
//! The manifest splice is applied and gated **in memory first**; the generated
//! model is written only where no file exists; the manifest write is atomic;
//! and if a write fails partway, the just-written model — and `models/`
//! itself, when this promotion created it — is removed so no orphan survives
//! a failed promotion.

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
}

/// Promote an exploration into the protocol at `dir`: write its SQL as a new
/// numbered model and splice a step naming it onto the end of `steps`.
///
/// The generated model lands at `models/NN_<name>.sql`, where `NN` continues
/// the highest number-prefixed model already present (a hand-authored,
/// unnumbered model neither collides nor moves). The spliced step carries
/// `name:` and `sql:` and nothing else — inputs and outputs are discovered
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
/// document does not read back verbatim), when `steps` is missing or empty
/// (an empty protocol has nothing to explore, so it has nothing to record
/// against), or when the provenance note spans lines;
/// [`Error::GeneratedSqlExists`] when the model path is already occupied; the
/// loader's own error when the spliced result would not load — a duplicate
/// step name, most commonly.
pub fn record_step(dir: &Path, step: &RecordedStep) -> Result<(PathBuf, ValidatedSpec)> {
    valid_step_name(&step.name)?;
    one_line(&step.provenance)?;

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
    let indent = sequence_item_indent(&original, &steps_path)?;
    let item = format!(
        "{indent}- name: {}\n{indent}  sql: {sql_cited}\n",
        step.name
    );
    let validated = apply_edits(
        &original,
        &[SpecEdit::Append {
            path: steps_path,
            item,
        }],
    )?;

    // The reloaded document is the arbiter: the splice must read back as
    // exactly one new step carrying exactly the asked-for name and file. The
    // name gate above refuses the smuggling constructions it can name; this
    // equality check refuses the ones it cannot — any name that parses but
    // records something other than itself.
    let faithful = validated.manifest().steps.len() == steps_before + 1
        && validated.manifest().steps.last().is_some_and(|last| {
            last.name == step.name && last.sql.as_deref() == Some(sql_cited.as_str())
        });
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

    // Both writes are now committed to. The checkpoint seam fires before the
    // first byte changes on disk.
    checkpoint(dir);

    let models_dir_created = !models_abs.exists();
    std::fs::create_dir_all(&models_abs)?;
    if let Err(e) = write_atomic(
        &sql_abs,
        model_contents(&step.provenance, &step.sql).as_bytes(),
    ) {
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
}

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
/// of the table it is applied to for which the condition holds. The table read
/// and the condition are written as given.
fn filter_rows_sql(recording: &Recording) -> String {
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT *\nFROM {}\nWHERE {};\n",
        quote_ident(recording.name),
        from_ident(recording.on),
        recording.text("where"),
    )
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
    let directions: Vec<serde_json::Value> = DIRECTIONS
        .iter()
        .map(|d| serde_json::json!({ "word": d.word, "sign": d.sign }))
        .collect();
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
                "x-directions": directions,
            }
        },
        "required": ["order_by"],
    })
}

/// The SQL step of [`SORT_ROWS`]: a table named for the step, holding the rows of
/// the table it is applied to in the order the clause gives. The table read and
/// the order are written as given, as [`filter_rows_sql`]'s condition is.
fn sort_rows_sql(recording: &Recording) -> String {
    format!(
        "CREATE OR REPLACE TABLE {} AS\nSELECT *\nFROM {}\nORDER BY {};\n",
        quote_ident(recording.name),
        from_ident(recording.on),
        recording.text("order_by"),
    )
}

/// `name` as a double-quoted SQL identifier, a `"` inside it doubled. The step's
/// own name is always quoted, so a name arc admits that would otherwise read as
/// SQL — [`valid_step_name`] admits a `"` — cannot break out of the statement.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A table name for the `FROM` clause. A name of only ASCII letters, digits and
/// underscores that does not open with a digit is written bare — a plain
/// `orders` unchanged, folded by DuckDB as it always was, so `FROM orders` reads
/// the same, and brightfield's `to_pushdown_sql` writes the same line. Any other
/// name — one a step could only have made under quotes, holding a space, a `"`
/// or a `;` — is quoted through [`quote_ident`], so a table the protocol makes
/// cannot break out of the `FROM` into SQL of its own, the same class as the
/// condition's terminator.
fn from_ident(name: &str) -> String {
    let bare = !name.is_empty()
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if bare {
        name.to_string()
    } else {
        quote_ident(name)
    }
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

// ------------------------------------------------- recording an operation

/// Record the operation called `long_name`, applied to the table `on`, as a new
/// step named `name` at the end of the protocol at `dir`, and return the model's
/// path relative to `dir`.
///
/// The operation's SQL is written from its catalogue entry, so a caller names no
/// operation and no argument of its own: the command line and `arc mcp` hand
/// over what they were given, and an operation added to the catalogue is
/// recorded through both with no edit to either. The model's first line is the
/// long name and the table, ` on ` between them, and nothing else, so the same
/// request writes the same bytes whoever sends it. The step is recorded through
/// [`record_step_with_history`](crate::history::record_step_with_history), so
/// each recording is a version of the protocol, and it runs nothing.
///
/// Refused, with the protocol's directory untouched: an operation arc does not
/// hold; arguments the operation's `parameters` do not admit, whether one is
/// missing, one is not taken or one is of the wrong type; a condition or an order
/// DuckDB's own parser reads as more than one statement, or cannot parse at all
/// (see [`one_statement`]); and a table no step of the protocol makes. Each
/// refusal's message names what was wrong.
pub(crate) fn record_operation(
    dir: &Path,
    long_name: &str,
    on: &str,
    name: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    history: &crate::history::LocalHistory,
) -> Result<PathBuf> {
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
    };
    let (sql_rel, _) = crate::history::record_step_with_history(dir, &step, history)?;
    Ok(sql_rel)
}

impl Operation {
    /// Whether `arguments` are what the operation's `parameters` schema admits:
    /// each required argument present, no argument the schema does not name when
    /// it is closed, and each value of the type the schema gives it. A value
    /// annotated with a clause `x-kind` — a `condition` or an `order` — is also
    /// checked as that clause, through [`one_statement`].
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
        for (key, value) in arguments {
            let Some(property) = properties.get(key) else {
                continue;
            };
            if let Some(ty) = property["type"].as_str()
                && !is_of_type(value, ty)
            {
                return Err(refused(format!(
                    "the argument `{key}` of `{}` is a {ty}, not {value}",
                    self.long_name
                )));
            }
            if let Some(clause) = Clause::of(&property["x-kind"])
                && let Some(text) = value.as_str()
            {
                one_statement(key, clause, text)?;
            }
        }
        Ok(())
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
/// not parse-checked.
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
        _ => Err(refused(format!(
            "the {} `{key}` holds a `;` outside a string or a comment, which would end \
             the step's statement and begin another: {value}",
            clause.noun
        ))),
    }
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
fn refused(message: String) -> Error {
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
        }
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
