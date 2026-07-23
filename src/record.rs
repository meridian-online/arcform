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
/// store now exists, and it plugs in **above** this line, not inside it:
/// [`record_step_with_history`](crate::history::record_step_with_history)
/// snapshots the manifest before calling in here, refusing the write when
/// the snapshot cannot land. The bare road keeps this no-op so the ordering
/// stays visible — the hook fires before the first byte moves — and stays
/// history-free on purpose for callers that bring their own net. (The
/// model rewrite in [`amend_step_sql`] passes here too; snapshotting
/// *generated* SQL, which the marker already licenses this tool to
/// regenerate, is a deliberate non-goal for now.)
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
}
