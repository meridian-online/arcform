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
//! # Refusal discipline
//!
//! Every refusal leaves the protocol directory untouched, byte for byte. The
//! manifest splice is applied and gated **in memory first**; the generated
//! model is written only where no file exists; the manifest write is atomic;
//! and if that final write fails, the just-written model is removed so no
//! orphan survives a failed promotion.

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
    /// spec gate refuses a duplicate before anything is written.
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
/// [`Error::EditTarget`] when `steps` is missing or empty (an empty protocol
/// has nothing to explore, so it has nothing to record against) or the
/// provenance note spans lines; [`Error::GeneratedSqlExists`] when the model
/// path is already occupied; the loader's own error when the spliced result
/// would not load — a duplicate step name, most commonly.
pub fn record_step(dir: &Path, step: &RecordedStep) -> Result<(PathBuf, ValidatedSpec)> {
    one_line(&step.provenance)?;

    let manifest_path = dir.join(MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Err(Error::ManifestNotFound);
    }
    let original = std::fs::read_to_string(&manifest_path).map_err(|e| Error::FileRead {
        path: manifest_path.clone(),
        source: e,
    })?;

    // Where the model will land. Create mode: an occupied path is refused,
    // never overwritten — an existing file may carry authorship.
    let filename = format!(
        "{:02}_{}.sql",
        next_model_number(&dir.join("models")),
        filename_slug(&step.name)
    );
    let sql_rel = Path::new("models").join(&filename);
    let sql_abs = dir.join(&sql_rel);
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
        "{indent}- name: {}\n{indent}  sql: models/{filename}\n",
        step.name
    );
    let validated = apply_edits(
        &original,
        &[SpecEdit::Append {
            path: steps_path,
            item,
        }],
    )?;

    // Both writes are now committed to. The checkpoint seam fires before the
    // first byte changes on disk.
    checkpoint(dir);

    if let Some(parent) = sql_abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(
        &sql_abs,
        model_contents(&step.provenance, &step.sql).as_bytes(),
    )?;

    if let Err(e) = validated.write_to(dir) {
        // Take the model back out: a failed promotion leaves no orphan.
        let _ = std::fs::remove_file(&sql_abs);
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

/// The seam every durable write passes through first. A no-op today, on
/// purpose: a local-history store that snapshots the bytes about to change
/// plugs in here, so every machine edit gains a restore point without any
/// caller having to remember to ask for one. Until that store exists, the
/// guarantee is only ordering — the hook fires before the first byte moves.
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
}
