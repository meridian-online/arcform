//! Checks and measures: SQL a Protocol asks of its own data. A `check:` or `measure:` step
//! names one file, read here as Protocol SQL: through the same reader as any other step, and
//! refused before a run starts unless it holds exactly one query.

use std::collections::BTreeSet;
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
    // A Protocol with no check or measure may keep any table it likes, `measures` included.
    let has_checks = manifest
        .steps
        .iter()
        .any(|s| s.is_check() || s.is_measure());
    for step in &manifest.steps {
        if has_checks
            && let Some(assets) = graph.steps.get(&step.name)
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
        return refuse(format!("holds {count} statements"));
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

/// Refuse a Protocol in which an `error` check comes after a step that publishes data the
/// check reads, directly or through the tables and files it was built from: a failing
/// check would come too late to stop the publish.
pub(crate) fn refuse_publish_before_error_check(
    manifest: &Manifest,
    graph: &AssetGraph,
) -> Result<()> {
    for (at, publish) in manifest.steps.iter().enumerate() {
        let publishes = publish
            .op
            .as_deref()
            .and_then(|op| crate::operator::resolve(op).ok())
            .is_some_and(|op| op.publishes());
        let Some(published) = graph.steps.get(&publish.name).filter(|_| publishes) else {
            continue;
        };
        let guarded: BTreeSet<String> = graph.upstream_assets(&published.reads);
        for check in manifest.steps[at + 1..]
            .iter()
            .filter(|s| s.is_check() && s.severity_or_default() == crate::manifest::Severity::Error)
        {
            let Some(reads) = graph.steps.get(&check.name).map(|a| &a.reads) else {
                continue;
            };
            if let Some(asset) = reads.iter().find(|a| guarded.contains(*a)) {
                return Err(Error::ManifestValidation(format!(
                    "step '{}' publishes before the error check '{}' that reads {asset}, so a \
                     failing check would come too late: move the check before it, or make it \
                     severity: warn",
                    publish.name, check.name
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLISH: &str = "  - name: publish\n    op: ducklake_publish@1\n    with:\n      file: build/soc.parquet\n      catalog: build/lake/test.ducklake\n      table: demo.soc\n";

    /// `load`, `export`, then `publish` and `check` in the order given.
    fn refusal(check_extra: &str, check_reads: &str, check_first: bool) -> Result<()> {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, sql: &str| {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, sql).unwrap();
        };
        write("load.sql", "CREATE TABLE soc AS SELECT 1 AS n;");
        write("other.sql", "CREATE TABLE other AS SELECT 1 AS n;");
        write(
            "export.sql",
            "COPY soc TO 'build/soc.parquet' (FORMAT parquet);",
        );
        write(
            "check.sql",
            &format!("SELECT * FROM {check_reads} WHERE n < 0;"),
        );
        let check = format!(
            "  - name: check\n    check: check.sql\n    risk: a wrong table\n{check_extra}"
        );
        let tail = if check_first {
            format!("{check}{PUBLISH}")
        } else {
            format!("{PUBLISH}{check}")
        };
        let yaml = format!(
            "name: t\nsteps:\n  - name: load\n    sql: load.sql\n  - name: other\n    sql: other.sql\n  - name: export\n    sql: export.sql\n{tail}"
        );
        let manifest = Manifest::from_yaml_str(&yaml).unwrap();
        let graph = AssetGraph::build(&manifest, dir.path());
        refuse_publish_before_error_check(&manifest, &graph)
    }

    #[test]
    fn an_error_check_after_a_publish_of_its_table_is_refused() {
        let err = refusal("", "soc", false).unwrap_err().to_string();
        assert!(err.contains("step 'publish'"), "{err}");
        assert!(err.contains("error check 'check'"), "{err}");
        assert!(err.contains("reads soc"), "{err}");
    }

    #[test]
    fn a_warn_check_after_the_publish_is_allowed() {
        refusal("    severity: warn\n", "soc", false).unwrap();
    }

    #[test]
    fn an_error_check_before_the_publish_is_allowed() {
        refusal("", "soc", true).unwrap();
    }

    #[test]
    fn an_error_check_on_an_unrelated_table_after_the_publish_is_allowed() {
        refusal("", "other", false).unwrap();
    }
}
