//! Checks and measures: SQL a Protocol asks of its own data. A `check:` or `measure:` step
//! names one file, read here as Protocol SQL: through the same reader as any other step, and
//! refused before a run starts unless it holds exactly one query.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::asset::AssetGraph;
use crate::contract::CheckSection;
use crate::engine::{AnswerDir, Engine, LoadedExtensions};
use crate::error::{Error, Result};
use crate::manifest::{Manifest, Step};

/// How many of a failing check's rows the record keeps.
const SAMPLE_ROWS: usize = 20;

/// How many characters of a sample cell the record keeps; a longer cell is cut and ends `…`.
const SAMPLE_CELL_CHARS: usize = 200;

/// One earlier run's measure, as a check reads it from `measure_history`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(dead_code)] // built once arc reads earlier runs' measures
pub(crate) struct HistoryRow {
    pub(crate) run_id: String,
    pub(crate) run_at: String,
    pub(crate) name: String,
    pub(crate) value: Option<f64>,
}

/// What a check's query runs with: the step's DuckDB, database, parameters and timeout, and
/// the measures it may compare.
pub(crate) struct QueryContext<'a> {
    pub(crate) engine: &'a dyn Engine,
    pub(crate) db_path: &'a Path,
    pub(crate) env: &'a HashMap<String, String>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) loaded: &'a mut LoadedExtensions,
    /// This run's measures so far, as `measures` holds them.
    pub(crate) measures: &'a [(String, Option<f64>)],
    /// Earlier runs' measures, as `measure_history` holds them.
    pub(crate) history: &'a [HistoryRow],
}

/// What the wrapper writes to `answer.json`.
#[derive(Deserialize)]
struct Answer {
    rows_returned: u64,
    columns: Vec<String>,
    sample: Vec<Vec<Option<String>>>,
}

/// `path` as a SQL string literal.
fn sql_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

/// The SQL that runs `query` for a check: `measures` and `measure_history` stood up as
/// temporary tables, the query's rows kept in a third, and the count, the column names and
/// the first rows, each cell as text, written to `answer.json` in `dir`.
fn wrapper(dir: &Path, query: &str) -> String {
    let measures = sql_path(&dir.join("measures.json"));
    let history = sql_path(&dir.join("history.json"));
    let answer = sql_path(&dir.join("answer.json"));
    // `[*COLUMNS(*)]` packs a row into a list only once every column is one type, so the
    // inner query casts them first; DuckDB 2.0 expands `[COLUMNS(*)::VARCHAR]` into one
    // single-element list per column instead.
    format!(
        "CREATE TEMP TABLE measures AS\n  \
         SELECT * FROM read_json({measures}, columns = {{name: 'VARCHAR', value: 'DOUBLE'}});\n\
         CREATE TEMP TABLE measure_history AS\n  \
         SELECT * FROM read_json({history},\n    \
         columns = {{run_id: 'VARCHAR', run_at: 'TIMESTAMP', name: 'VARCHAR', value: 'DOUBLE'}});\n\
         CREATE TEMP TABLE __arc_rows AS\n\
         {query}\n\
         ;\n\
         COPY (SELECT\n  \
         (SELECT count(*) FROM __arc_rows) AS rows_returned,\n  \
         (SELECT list(column_name) FROM (DESCRIBE __arc_rows)) AS columns,\n  \
         (SELECT coalesce(list(r), []) FROM (SELECT [*COLUMNS(*)] AS r FROM \
         (SELECT COLUMNS(*)::VARCHAR FROM __arc_rows LIMIT {SAMPLE_ROWS}))) AS sample\n\
         ) TO {answer} (FORMAT json);\n"
    )
}

/// `cell` cut to [`SAMPLE_CELL_CHARS`] characters, ending `…` when it was longer.
fn truncate_cell(cell: String) -> String {
    match cell.char_indices().nth(SAMPLE_CELL_CHARS) {
        Some((at, _)) => format!("{}…", &cell[..at]),
        None => cell,
    }
}

/// Run the check `step`, whose one query is `query`, and say what it found. An error is a
/// query that did not run; a query that returned rows is a section with `success` false.
pub(crate) fn run_check(step: &Step, query: &str, ctx: &mut QueryContext) -> Result<CheckSection> {
    let failed = |source: std::io::Error| Error::StepExecution {
        step: step.name.clone(),
        source,
    };
    let answer_dir = AnswerDir::new().ok_or_else(|| {
        failed(std::io::Error::other(
            "arc could not make a temporary directory for the check's answer",
        ))
    })?;
    let dir = answer_dir.path();
    let measures: Vec<serde_json::Value> = ctx
        .measures
        .iter()
        .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
        .collect();
    let write = |file: &str, text: String| std::fs::write(dir.join(file), text).map_err(failed);
    write(
        "measures.json",
        serde_json::Value::from(measures).to_string(),
    )?;
    write(
        "history.json",
        serde_json::to_string(ctx.history).unwrap_or_else(|_| "[]".to_string()),
    )?;
    write("wrapper.sql", wrapper(dir, query))?;
    ctx.engine
        .execute_sql(
            ctx.db_path,
            &dir.join("wrapper.sql"),
            ctx.env,
            ctx.timeout,
            ctx.loaded,
        )
        .map_err(|e| match e {
            // The engine names the file it ran, which is arc's wrapper, not the check.
            Error::StepTimeout { .. } => Error::StepTimeout {
                step: step.name.clone(),
            },
            other => other,
        })?;
    let text = std::fs::read_to_string(dir.join("answer.json")).map_err(failed)?;
    let answer: Answer = serde_json::from_str(text.trim()).map_err(|e| {
        failed(std::io::Error::other(format!(
            "arc could not read the check's answer: {e}"
        )))
    })?;
    Ok(CheckSection {
        name: step.name.clone(),
        risk: step.risk.clone().unwrap_or_default(),
        severity: step.severity_or_default().as_str().to_string(),
        success: answer.rows_returned == 0,
        rows_returned: answer.rows_returned,
        sample: answer
            .sample
            .into_iter()
            .map(|row| row.into_iter().map(|c| c.map(truncate_cell)).collect())
            .collect(),
        sample_columns: answer.columns,
        error: None,
    })
}

/// The section of a check step whose query did not run: failed, with no rows and `why`.
pub(crate) fn not_run(step: &Step, why: &Error) -> CheckSection {
    CheckSection {
        name: step.name.clone(),
        risk: step.risk.clone().unwrap_or_default(),
        severity: step.severity_or_default().as_str().to_string(),
        success: false,
        rows_returned: 0,
        sample: Vec::new(),
        sample_columns: Vec::new(),
        error: Some(why.to_string()),
    }
}

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
    fn a_cell_is_cut_at_200_characters_not_bytes() {
        let short = "é".repeat(200);
        assert_eq!(truncate_cell(short.clone()), short);
        let cut = truncate_cell("é".repeat(300));
        assert_eq!(cut.chars().count(), 201);
        assert_eq!(cut, format!("{}…", "é".repeat(200)));
    }

    #[test]
    fn a_path_with_a_quote_is_one_sql_literal() {
        assert_eq!(
            sql_path(Path::new("/tmp/o'brien/answer.json")),
            "'/tmp/o''brien/answer.json'"
        );
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
