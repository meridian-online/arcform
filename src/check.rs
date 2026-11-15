//! Checks and measures: SQL a Protocol asks of its own data. A `check:` or `measure:` step
//! names one file, read here as Protocol SQL: through the same reader as any other step, and
//! refused before a run starts unless it holds exactly one query.

use std::path::Path;

use crate::asset::AssetGraph;
use crate::error::{Error, Result};
use crate::manifest::{Manifest, Step};

/// The tables arc keeps for a run's checks and measures. A step may read them and none may
/// make them, so a check on them has nothing in the asset graph to depend on.
const RESERVED_TABLES: [&str; 2] = ["measures", "measure_history"];

/// True when `name` is one of arc's own check and measure tables, in any case.
pub(crate) fn is_reserved_table(name: &str) -> bool {
    RESERVED_TABLES
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

/// What a step's file is called in a refusal.
fn what(step: &Step) -> &'static str {
    if step.is_measure() {
        "a measure file"
    } else {
        "a check file"
    }
}

/// Refuse a Protocol whose check or measure files are not one query each, and one in which a
/// step produces a table arc keeps for itself. Nothing has run when this answers.
pub(crate) fn refuse_check_shapes(
    manifest: &Manifest,
    dir: &Path,
    graph: &AssetGraph,
) -> Result<()> {
    for step in &manifest.steps {
        if let Some(assets) = graph.steps.get(&step.name)
            && let Some(reserved) = assets.produces.iter().find(|t| is_reserved_table(t))
        {
            return Err(Error::ManifestValidation(format!(
                "step '{}' produces '{reserved}', which arc reserves for checks and measures; \
                 name the table something else",
                step.name
            )));
        }
        if !(step.is_check() || step.is_measure()) {
            continue;
        }
        // A file that is not there is the run's own refusal, as for any SQL step.
        if step.sql_file().is_some_and(|file| dir.join(file).exists()) {
            check_query(dir, step)?;
        }
    }
    Ok(())
}

/// The one query of a check or measure step's file, as written, without its trailing `;`.
pub(crate) fn check_query(dir: &Path, step: &Step) -> Result<String> {
    let file = step.sql_file().unwrap_or_default();
    let path = dir.join(file);
    let text = std::fs::read_to_string(&path).map_err(|source| Error::FileRead {
        path: path.clone(),
        source,
    })?;
    let refuse = |why: String| {
        Err(Error::ManifestValidation(format!(
            "step '{}': {file} {why}; {} holds exactly one query",
            step.name,
            what(step)
        )))
    };
    let read = crate::introspect::read_sql(&text).map_err(Error::SqlReader)?;
    let count = read.statements.len();
    if count != 1 {
        let s = if count == 1 { "" } else { "s" };
        return refuse(format!("holds {count} statement{s}"));
    }
    if !read.is_query[0] {
        return refuse("holds a statement that is not a query".to_string());
    }
    let Some((start, end)) = read.byte_ranges[0] else {
        return refuse("holds a statement arc cannot place in the file".to_string());
    };
    let query = text.get(start..end).unwrap_or_default().trim();
    Ok(query.trim_end_matches(';').trim_end().to_string())
}
