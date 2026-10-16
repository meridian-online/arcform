//! The files beside a Protocol's spec that `arc protocol write-file` and the
//! `protocol_write_file` tool write: a table's schema, a panel, the README.
//!
//! One function does the work for both surfaces, so the same request writes the
//! same bytes from either. Every refusal is decided before anything is created,
//! so a refused request leaves the folder as it was: no new `schemas/` or
//! `panels/` folder, and no file.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::edit::write_atomic;
use crate::error::{Error, Result};
use crate::history::LocalHistory;
use crate::manifest::MANIFEST_FILENAME;

/// What a path inside a Protocol's folder is, among the files this module writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolFileKind {
    Schema,
    Panel,
    Readme,
    Report,
}

impl ProtocolFileKind {
    /// The kind of the file at `rel`, a path relative to the Protocol's folder:
    /// `schemas/<name>.json`, `panels/<name>.yaml`, `README.md` or `report.md`.
    /// Any other path is none.
    pub(crate) fn from_path(rel: &Path) -> Option<Self> {
        let parts: Vec<&str> = rel
            .components()
            .filter(|c| !matches!(c, Component::CurDir))
            .map(|c| match c {
                Component::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect::<Option<_>>()?;
        let named = |file: &str, extension: &str| {
            file.strip_suffix(extension)
                .is_some_and(|stem| !stem.is_empty())
        };
        match parts.as_slice() {
            ["README.md"] => Some(Self::Readme),
            ["report.md"] => Some(Self::Report),
            ["schemas", file] if named(file, ".json") => Some(Self::Schema),
            ["panels", file] if named(file, ".yaml") => Some(Self::Panel),
            _ => None,
        }
    }

    /// The word the result names the kind by.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Panel => "panel",
            Self::Readme => "readme",
            Self::Report => "report",
        }
    }
}

/// A file written, as the caller reports it.
#[derive(Debug)]
pub(crate) struct WrittenFile {
    /// The file's path relative to the Protocol's folder.
    pub(crate) path: PathBuf,
    pub(crate) kind: ProtocolFileKind,
    /// Things the content does that the writer would have the author know; the
    /// file is written all the same.
    pub(crate) warnings: Vec<String>,
}

/// Whether `rel` names a place inside a folder by its components alone: a
/// relative path with no `..`, root or drive prefix.
pub(crate) fn stays_inside_by_name(rel: &Path) -> bool {
    rel.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Whether `path`, a place under `dir`, resolves inside `dir`: the nearest
/// ancestor of it that exists, canonicalised, must sit under the canonical
/// `dir`, which catches a symbolic link that points out. Creates nothing.
pub(crate) fn resolves_inside(dir: &Path, path: &Path) -> Result<bool> {
    let root = fs::canonicalize(dir)?;
    let mut probe = path;
    let held = loop {
        match fs::canonicalize(probe) {
            Ok(held) => break held,
            Err(_) => match probe.parent() {
                Some(parent) => probe = parent,
                None => return Ok(false),
            },
        }
    };
    Ok(held.starts_with(&root))
}

/// Replace the file at `path` with `content`, keeping both the old and the new
/// state in the file's history.
///
/// A file already there may never have been written through arc: its text is
/// kept before it is replaced, and the write waits until that is kept. The new
/// text is never merged into the last entry, so two quick writes keep both
/// versions. The caller has made the parent folder and decided the path is
/// inside the Protocol.
pub(crate) fn replace_file_with_history(
    path: &Path,
    content: &str,
    history: &LocalHistory,
) -> Result<()> {
    let mut held = None;
    if path.exists() {
        let current = fs::read_to_string(path).map_err(|e| Error::FileRead {
            path: path.to_path_buf(),
            source: e,
        })?;
        held = history.checkpoint_held_for_file(path, &current)?;
    }
    write_atomic(path, content.as_bytes())?;
    // The checkpoint's line in the folder's log lands with the write it was for.
    if let Some(held) = held {
        held.land();
    }
    history.record_save_unmerged_for_file(path, content)?;
    Ok(())
}

/// Write `content` to the file at `rel` in the Protocol folder `dir`, and
/// record it in that file's history.
///
/// Refused, with nothing written or created: a path that leaves the folder by
/// name or through a link, a path that is none of the four places
/// [`ProtocolFileKind::from_path`] names, `report.md` (which has a verb of its
/// own), a folder with no `arcform.yaml`, and content that does not read as its
/// kind: a schema is JSON and a panel is YAML with a `plot` key.
pub(crate) fn write_protocol_file(
    dir: &Path,
    rel: &Path,
    content: &str,
    history: &LocalHistory,
) -> Result<WrittenFile> {
    let refuse = |message: String| Error::ManifestValidation(message);
    if !stays_inside_by_name(rel) {
        return Err(refuse(format!(
            "{} is outside the Protocol folder {}; name a file inside it",
            rel.display(),
            dir.display()
        )));
    }
    let kind = ProtocolFileKind::from_path(rel).ok_or_else(|| {
        refuse(format!(
            "{} is not a file arc writes; the places are schemas/<name>.json, \
             panels/<name>.yaml, README.md and report.md",
            rel.display()
        ))
    })?;
    if kind == ProtocolFileKind::Report {
        return Err(refuse(
            "report.md is written by the report_write tool, not this one".to_string(),
        ));
    }
    if !dir.join(MANIFEST_FILENAME).is_file() {
        return Err(refuse(format!(
            "{} holds no {MANIFEST_FILENAME}; make the Protocol first",
            dir.display()
        )));
    }
    match kind {
        ProtocolFileKind::Schema => {
            serde_json::from_str::<serde_json::Value>(content)
                .map_err(|e| refuse(format!("the schema is not valid JSON: {e}")))?;
        }
        ProtocolFileKind::Panel => {
            let panel: serde_yaml::Value = serde_yaml::from_str(content)
                .map_err(|e| refuse(format!("the panel is not valid YAML: {e}")))?;
            if panel.get("plot").is_none() {
                return Err(refuse("the panel has no `plot` key".to_string()));
            }
        }
        ProtocolFileKind::Readme | ProtocolFileKind::Report => {}
    }
    let path = dir.join(rel);
    if !resolves_inside(dir, &path)? {
        return Err(refuse(format!(
            "{} is outside the Protocol folder {}; name a file inside it",
            rel.display(),
            dir.display()
        )));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    replace_file_with_history(&path, content, history)?;
    // The panel is kept whatever it names; what the Protocol does not make is said.
    let warnings = if kind == ProtocolFileKind::Panel {
        match crate::panel_check::check_panel(dir, rel) {
            Ok(findings) => findings.iter().map(ToString::to_string).collect(),
            Err(e) => vec![format!(
                "the panel was not checked against the Protocol: {e}"
            )],
        }
    } else {
        Vec::new()
    };
    Ok(WrittenFile {
        path: rel
            .components()
            .filter(|c| !matches!(c, Component::CurDir))
            .collect(),
        kind,
        warnings,
    })
}
