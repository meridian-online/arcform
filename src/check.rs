//! Checks and measures: SQL a Protocol asks of its own data. A `check:` or `measure:` step
//! names one file, read here as Protocol SQL: through the same reader as any other step, and
//! refused before a run starts unless it holds exactly one query.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::asset::AssetGraph;
use crate::asset_kind::AssetKind;
use crate::contract::{self, CheckSection, Contract, ParamEntry};
use crate::engine::{AnswerDir, Engine, LoadedExtensions};
use crate::error::{Error, Result};
use crate::manifest::{Manifest, MeasureHistory, Step};

/// How many of a failing check's rows the record keeps.
const SAMPLE_ROWS: usize = 20;

/// How many characters of a sample cell the record keeps; a longer cell is cut and ends `…`.
const SAMPLE_CELL_CHARS: usize = 200;

/// One earlier run's measure, as a check reads it from `measure_history`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct HistoryRow {
    pub(crate) run_id: String,
    pub(crate) run_at: String,
    pub(crate) name: String,
    pub(crate) value: Option<f64>,
}

/// Seconds since the Unix epoch for an ISO timestamp as a run record holds it
/// (`2026-10-01T22:54:17Z`); `None` for any other shape.
fn iso_to_epoch_secs(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Howard Hinnant's days-from-civil, the inverse of the one the run record's clock uses.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}

/// Whether two runs resolved the same parameters: the same keys, each with the same value,
/// in any order and whatever supplied them. A value redacted on either side (a secret-looking
/// key, or a dotenv value in a record restored from the published copy) matches any value,
/// since the record no longer says what it was.
fn same_params(a: &[ParamEntry], b: &[ParamEntry]) -> bool {
    let redacted = contract::is_redaction_marker;
    let sorted = |p: &[ParamEntry]| {
        let mut v: Vec<(String, String)> =
            p.iter().map(|e| (e.key.clone(), e.value.clone())).collect();
        v.sort();
        v
    };
    let (a, b) = (sorted(a), sorted(b));
    a.len() == b.len()
        && a.iter()
            .zip(&b)
            .all(|((ka, va), (kb, vb))| ka == kb && (va == vb || redacted(va) || redacted(vb)))
}

/// Earlier runs' measures, for `measure_history`: every record in `runs_dir` that reads as
/// a contract, is not `current_run_id`, succeeded, has a `started_at` and ran with
/// `current_params`. Each step's `measure` section becomes rows stamped with the run's
/// `started_at`. `Runs(n)` keeps the `n` newest runs that measured anything, `Since(d)` those
/// started at or after `now - d`. Oldest run first, steps in record order. A record that is
/// unreadable or written before measures existed contributes nothing and is not an error.
pub(crate) fn load_history(
    runs_dir: &Path,
    current_run_id: &str,
    current_params: &[ParamEntry],
    window: &MeasureHistory,
    now: std::time::SystemTime,
) -> Vec<HistoryRow> {
    let mut runs: Vec<(String, String, Vec<HistoryRow>)> = Vec::new();
    for path in contract::contracts_by_age(runs_dir) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<Contract>(&text) else {
            continue;
        };
        let run = &record.run;
        let Some(started_at) = run.started_at.clone() else {
            continue;
        };
        if run.run_id == current_run_id
            || run.outcome != "success"
            || !same_params(&run.params, current_params)
        {
            continue;
        }
        let rows: Vec<HistoryRow> = record
            .steps
            .iter()
            .filter_map(|step| step.measure.as_ref())
            .flat_map(|section| section.values.iter())
            .map(|(name, value)| HistoryRow {
                run_id: run.run_id.clone(),
                run_at: started_at.clone(),
                name: name.clone(),
                value: *value,
            })
            .collect();
        if !rows.is_empty() {
            runs.push((started_at, run.run_id.clone(), rows));
        }
    }
    runs.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    match window {
        MeasureHistory::Runs(n) => {
            let drop = runs.len().saturating_sub(*n as usize);
            runs.drain(..drop);
        }
        MeasureHistory::Since(span) => {
            let now = now
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            let cutoff = now - span.as_secs() as i64;
            runs.retain(|(started, _, _)| iso_to_epoch_secs(started).is_some_and(|t| t >= cutoff));
        }
    }
    runs.into_iter().flat_map(|(_, _, rows)| rows).collect()
}

/// The variable `arc test` sets to a CSV of earlier runs' measures. When it is set, a run
/// reads `measure_history` from that file in place of its run records. It is `arc test`'s
/// own, so a test can show a comparison check failing without earlier runs.
pub(crate) const MEASURE_HISTORY_ENV: &str = "ARC_TEST_MEASURE_HISTORY";

/// Earlier runs' measures from a CSV with the header `run_id,run_at,name,value`, read with
/// the linked library, every row taken as given and in file order: no window and no
/// parameter filter. An empty `value` is SQL `NULL`.
pub(crate) fn history_from_csv(path: &Path) -> Result<Vec<HistoryRow>> {
    let refuse = |reason: String| Error::MeasureHistoryFile {
        var: MEASURE_HISTORY_ENV,
        path: path.to_path_buf(),
        reason,
    };
    let conn = duckdb::Connection::open_in_memory().map_err(|e| refuse(e.to_string()))?;
    let literal = path.to_string_lossy().replace('\'', "''");
    let sql = format!(
        "SELECT run_id, run_at, name, value FROM read_csv('{literal}', columns = \
         {{run_id: 'VARCHAR', run_at: 'VARCHAR', name: 'VARCHAR', value: 'DOUBLE'}}, \
         header = true)"
    );
    let mut statement = conn.prepare(&sql).map_err(|e| refuse(e.to_string()))?;
    let rows = statement
        .query_map([], |r| {
            Ok(HistoryRow {
                run_id: r.get(0)?,
                run_at: r.get(1)?,
                name: r.get(2)?,
                value: r.get(3)?,
            })
        })
        .map_err(|e| refuse(e.to_string()))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| refuse(e.to_string()))
}

/// What a check's query runs with: the step's DuckDB, database, parameters and timeout, and
/// the measures it may compare.
pub(crate) struct QueryContext<'a> {
    pub(crate) engine: &'a dyn Engine,
    pub(crate) db_path: &'a Path,
    pub(crate) env: &'a HashMap<String, String>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) loaded: &'a mut LoadedExtensions,
    /// This run's measures so far, one per name, as `measures` holds them.
    pub(crate) measures: &'a IndexMap<String, Option<f64>>,
    /// Earlier runs' measures, as `measure_history` holds them.
    pub(crate) history: &'a [HistoryRow],
}

/// A check or measure step's one query, as written, and how many lines of its file come
/// before the line it starts on.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StepQuery {
    pub(crate) sql: String,
    pub(crate) lines_before: usize,
}

/// What the wrapper writes to `answer.json` for a check.
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

/// The SQL that runs `query`: `measures` and `measure_history` stood up as temporary
/// tables, the query's rows kept in a third, `__arc_rows`, and `answer`, a `SELECT` over
/// that third, written to `answer.json` in `dir`.
fn wrapper(dir: &Path, query: &StepQuery, answer: &str) -> String {
    let measures = sql_path(&dir.join("measures.json"));
    let history = sql_path(&dir.join("history.json"));
    let answer_file = sql_path(&dir.join("answer.json"));
    // DuckDB's `LINE n` counts from the start of the statement, so the query sits as many
    // lines below the `CREATE` as it sits below the top of its file, and `LINE n` is the
    // file's own line n. The `;` stays on a line of its own after any trailing `--` comment.
    let lines_before = "\n".repeat(query.lines_before);
    let sql = &query.sql;
    format!(
        "CREATE TEMP TABLE measures AS\n  \
         SELECT * FROM read_json({measures}, columns = {{name: 'VARCHAR', value: 'DOUBLE'}});\n\
         CREATE TEMP TABLE measure_history AS\n  \
         SELECT * FROM read_json({history},\n    \
         columns = {{run_id: 'VARCHAR', run_at: 'TIMESTAMP', name: 'VARCHAR', value: 'DOUBLE'}});\n\
         CREATE TEMP TABLE __arc_rows AS {lines_before}{sql}\n\
         ;\n\
         COPY ({answer}) TO {answer_file} (FORMAT json);\n"
    )
}

/// A check's answer: the count, the column names and the first rows, each cell as text.
fn check_answer() -> String {
    // One character past the cap, so arc can tell a cell it must mark as cut.
    let cut = SAMPLE_CELL_CHARS + 1;
    // `[*COLUMNS(*)]` packs a row into a list only once every column is one type, so the
    // inner query casts them first; DuckDB 2.0 expands `[COLUMNS(*)::VARCHAR]` into one
    // single-element list per column instead. Cells are cut in SQL, so a huge value is never
    // written out whole.
    format!(
        "SELECT\n  \
         (SELECT count(*) FROM __arc_rows) AS rows_returned,\n  \
         (SELECT list(column_name) FROM (DESCRIBE __arc_rows)) AS columns,\n  \
         (SELECT coalesce(list(r), []) FROM (SELECT [*COLUMNS(*)] AS r FROM \
         (SELECT left(COLUMNS(*)::VARCHAR, {cut}) FROM __arc_rows LIMIT {SAMPLE_ROWS}))) AS sample\n"
    )
}

/// A measure's answer: the count, each column's name and type, and the first row's values
/// cast to `DOUBLE` and written as text, so an infinity or a NaN reaches arc to be refused
/// rather than breaking the JSON. A column that is not a number casts to `NULL`; arc refuses
/// it by its type.
const MEASURE_ANSWER: &str = "SELECT\n  \
     (SELECT count(*) FROM __arc_rows) AS rows_returned,\n  \
     (SELECT list({'name': column_name, 'type': column_type}) FROM (DESCRIBE __arc_rows)) AS columns,\n  \
     (SELECT coalesce(list(r), []) FROM (SELECT [*COLUMNS(*)] AS r FROM \
     (SELECT TRY_CAST(COLUMNS(*) AS DOUBLE)::VARCHAR FROM __arc_rows LIMIT 1))) AS first\n";

/// What the wrapper writes to `answer.json` for a measure.
#[derive(Deserialize)]
struct MeasureAnswer {
    rows_returned: u64,
    columns: Vec<MeasureColumn>,
    first: Vec<Vec<Option<String>>>,
}

#[derive(Deserialize)]
struct MeasureColumn {
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

/// True for a DuckDB column type a measure may return: an integer of any width, signed or
/// not, a `FLOAT`, a `DOUBLE` or a `DECIMAL`.
fn is_numeric_type(kind: &str) -> bool {
    const NUMERIC: [&str; 12] = [
        "TINYINT",
        "SMALLINT",
        "INTEGER",
        "BIGINT",
        "HUGEINT",
        "UTINYINT",
        "USMALLINT",
        "UINTEGER",
        "UBIGINT",
        "UHUGEINT",
        "FLOAT",
        "DOUBLE",
    ];
    NUMERIC.contains(&kind) || (kind.starts_with("DECIMAL(") && kind.ends_with(')'))
}

/// `cell` cut to [`SAMPLE_CELL_CHARS`] characters, ending `…` when it was longer.
fn truncate_cell(cell: String) -> String {
    match cell.char_indices().nth(SAMPLE_CELL_CHARS) {
        Some((at, _)) => format!("{}…", &cell[..at]),
        None => cell,
    }
}

/// The error that fails `step` for `why`, named as the step, not arc's wrapper.
fn step_failed(step: &Step, why: impl Into<String>) -> Error {
    Error::StepExecution {
        step: step.name.clone(),
        source: std::io::Error::other(why.into()),
    }
}

/// Run `query` for `step` inside the wrapper, with `answer` as its final `SELECT`, and give
/// back the text of `answer.json`.
fn run_wrapped(
    step: &Step,
    query: &StepQuery,
    ctx: &mut QueryContext,
    answer: &str,
) -> Result<String> {
    let failed = |source: std::io::Error| Error::StepExecution {
        step: step.name.clone(),
        source,
    };
    let answer_dir = AnswerDir::new().ok_or_else(|| {
        step_failed(
            step,
            format!(
                "arc could not make a temporary directory for {}'s answer",
                what_step(step)
            ),
        )
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
        serde_json::to_string(ctx.history).map_err(|e| failed(std::io::Error::other(e)))?,
    )?;
    write("wrapper.sql", wrapper(dir, query, answer))?;
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
            Error::StepExecution { source, .. } => failed(source),
            other => other,
        })?;
    std::fs::read_to_string(dir.join("answer.json")).map_err(failed)
}

/// Run the check `step`, whose one query is `query`, and say what it found. An error is a
/// query that did not run; a query that returned rows is a section with `success` false.
pub(crate) fn run_check(
    step: &Step,
    query: &StepQuery,
    ctx: &mut QueryContext,
) -> Result<CheckSection> {
    let text = run_wrapped(step, query, ctx, &check_answer())?;
    let answer: Answer = serde_json::from_str(text.trim())
        .map_err(|e| step_failed(step, format!("arc could not read the check's answer: {e}")))?;
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

/// Run the measure `step`, whose one query is `query`, and give its values, each named
/// `<step>.<column>`, in the query's column order. Refused, failing the step, unless the
/// query returns exactly one row whose every column is a number with a name free of `.`.
pub(crate) fn run_measure(
    step: &Step,
    query: &StepQuery,
    ctx: &mut QueryContext,
) -> Result<IndexMap<String, Option<f64>>> {
    let text = run_wrapped(step, query, ctx, MEASURE_ANSWER)?;
    let answer: MeasureAnswer = serde_json::from_str(text.trim()).map_err(|e| {
        step_failed(
            step,
            format!("arc could not read the measure's answer: {e}"),
        )
    })?;
    measure_values(step, answer)
}

/// The values of a measure's answer, or the refusal that fails `step`.
fn measure_values(step: &Step, answer: MeasureAnswer) -> Result<IndexMap<String, Option<f64>>> {
    let refuse = |why: String| Err(step_failed(step, why));
    if answer.rows_returned != 1 {
        return refuse(format!(
            "the measure returned {} rows; a measure query returns exactly one row",
            answer.rows_returned
        ));
    }
    let row = answer.first.into_iter().next().unwrap_or_default();
    let mut values = IndexMap::new();
    for (at, column) in answer.columns.iter().enumerate() {
        let name = &column.name;
        if name.contains('.') {
            return refuse(format!(
                "measure column '{name}' has a '.' in its name; rename it, as each measure is \
                 named <step>.<column>"
            ));
        }
        if !is_numeric_type(&column.kind) {
            return refuse(format!(
                "measure column '{name}' is {}; every column of a measure query is numeric \
                 (an integer, FLOAT, DOUBLE or DECIMAL), so cast it or leave it out",
                column.kind
            ));
        }
        let value = match row.get(at).cloned().flatten() {
            None => None,
            Some(cell) => match cell.parse::<f64>() {
                Ok(v) if v.is_finite() => Some(v),
                _ => {
                    return refuse(format!(
                        "measure column '{name}' is {cell}, which is not a finite number; \
                         a measure is a finite number or NULL"
                    ));
                }
            },
        };
        values.insert(format!("{}.{name}", step.name), value);
    }
    Ok(values)
}

/// `table:<t>.row_count` and `table:<t>.bytes` for each of `tables`, read from the database
/// at `db_path` with the linked library, read-only, once the step that made them has exited.
/// A table arc cannot measure (not a simple identifier, a view, gone) is left out, as is
/// every table when the database cannot be opened.
pub(crate) fn measure_tables(db_path: &Path, tables: &[String]) -> IndexMap<String, Option<f64>> {
    let mut values = IndexMap::new();
    if tables.is_empty() || !db_path.is_file() {
        return values;
    }
    let Ok(config) = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .and_then(|c| c.with("autoinstall_known_extensions", "false"))
    else {
        return values;
    };
    let Ok(conn) = duckdb::Connection::open_with_flags(db_path, config) else {
        return values;
    };
    for table in tables {
        if !crate::contract::is_simple_ident(table) {
            continue;
        }
        // The blocks the table's columns are stored in, counted once each, times the
        // database's block size: DuckDB's own account of the table's bytes. A constant
        // segment has no block (`-1`). `pragma_storage_info` refuses a view. The name is
        // quoted, so a table called `order` or `group` is read as a table, not a keyword.
        let ident = format!("\"{}\"", table.replace('"', "\"\""));
        let sql = format!(
            "SELECT (SELECT count(*) FROM {ident}),\n  \
             (SELECT count(DISTINCT b) FROM (\n    \
             SELECT block_id AS b FROM pragma_storage_info('{ident}')\n    \
             UNION ALL SELECT unnest(additional_block_ids) FROM pragma_storage_info('{ident}')\n  \
             ) WHERE b >= 0)\n  \
             * (SELECT block_size FROM pragma_database_size() WHERE database_name = current_database())"
        );
        let Ok((rows, bytes)) = conn.query_row(&sql, [], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?))
        }) else {
            continue;
        };
        values.insert(format!("table:{table}.row_count"), rows.map(|n| n as f64));
        values.insert(format!("table:{table}.bytes"), bytes.map(|n| n as f64));
    }
    values
}

/// The tables `step` produces, as the asset graph has them, in name order.
pub(crate) fn produced_tables(graph: &AssetGraph, step: &Step) -> Vec<String> {
    graph
        .steps
        .get(&step.name)
        .map(|assets| {
            assets
                .produces
                .iter()
                .filter(|t| assets.declared_kind.get(*t) == Some(&AssetKind::Table))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
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

/// What a step is called in a message about its query.
fn what_step(step: &Step) -> &'static str {
    if step.is_measure() {
        "the measure"
    } else {
        "the check"
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

/// The one query of a check or measure step's file, as written, without its trailing `;`,
/// and how many lines of the file come before it.
pub(crate) fn check_query(dir: &Path, step: &Step) -> Result<StepQuery> {
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
    Ok(query_at(&text, start, end))
}

/// The query in `text` between `start` and `end`, trimmed, and the lines before it.
fn query_at(text: &str, start: usize, end: usize) -> StepQuery {
    let range = text.get(start..end).unwrap_or_default();
    let query = range.trim_start();
    let begins = start + (range.len() - query.len());
    StepQuery {
        sql: query
            .trim_end()
            .trim_end_matches(';')
            .trim_end()
            .to_string(),
        lines_before: text[..begins].matches('\n').count(),
    }
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

    #[test]
    fn history_from_a_csv_is_every_row_as_given() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("it's history.csv");
        std::fs::write(
            &path,
            "run_id,run_at,name,value\nr2,2026-02-01T00:00:00Z,size.codes,12.5\nr1,later,size.codes,\n",
        )
        .unwrap();
        let rows = history_from_csv(&path).unwrap();
        assert_eq!(
            rows,
            [
                HistoryRow {
                    run_id: "r2".into(),
                    run_at: "2026-02-01T00:00:00Z".into(),
                    name: "size.codes".into(),
                    value: Some(12.5),
                },
                HistoryRow {
                    run_id: "r1".into(),
                    run_at: "later".into(),
                    name: "size.codes".into(),
                    value: None,
                },
            ]
        );
    }

    #[test]
    fn a_history_csv_that_cannot_be_read_names_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let err = history_from_csv(&dir.path().join("missing.csv")).unwrap_err();
        let said = err.to_string();
        assert!(said.contains("missing.csv"), "{said}");
        assert!(said.contains(MEASURE_HISTORY_ENV), "{said}");
    }

    /// A run record for the history tests: `measures` are `(name, value)` in one step.
    fn record(
        run_id: &str,
        started_at: Option<&str>,
        outcome: &str,
        release: &str,
        measures: &[(&str, f64)],
    ) -> String {
        let values: serde_json::Map<String, serde_json::Value> = measures
            .iter()
            .map(|(n, v)| (n.to_string(), serde_json::json!(v)))
            .collect();
        serde_json::json!({
            "contract_version": "b5/1",
            "run": {"run_id": run_id, "attempt_id": run_id,
                "protocol": {"name": "t", "manifest_sha256": null, "dir": "."},
                "engine": {"arc": "0", "duckdb": null, "duckdb_cli": null,
                           "platform": null, "extensions": null},
                "params": [{"key": "release", "value": release, "source": "cli"}],
                "started_at": started_at, "finished_at": null, "outcome": outcome},
            "assets": [],
            "steps": [{"name": "m", "kind": "measure", "op_ref": null, "resolved_with": null,
                "sql": null, "status": {"state": "success", "skip_reason": null},
                "attempts": 1, "duration_sec": null, "retry": null, "timeout_sec": null,
                "io": {"stdout_path": null, "stderr_path": null}, "ingress_meta": null,
                "report": null, "narrative": {"label": null, "stage": null, "doc": null},
                "measure": {"values": values}}]
        })
        .to_string()
    }

    fn release(value: &str) -> Vec<ParamEntry> {
        vec![ParamEntry {
            key: "release".into(),
            value: value.into(),
            source: "cli".into(),
        }]
    }

    fn at(secs: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn write_runs(files: &[(&str, String)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    fn ids(rows: &[HistoryRow]) -> Vec<&str> {
        rows.iter().map(|r| r.run_id.as_str()).collect()
    }

    #[test]
    fn history_keeps_the_newest_n_successful_runs_oldest_first() {
        // File order is deliberately not start order.
        let dir = write_runs(&[
            (
                "c.json",
                record(
                    "c",
                    Some("2026-10-03T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 3.0), ("m.b", 30.0)],
                ),
            ),
            (
                "a.json",
                record(
                    "a",
                    Some("2026-10-01T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 1.0)],
                ),
            ),
            (
                "b.json",
                record(
                    "b",
                    Some("2026-10-02T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 2.0)],
                ),
            ),
        ]);
        let rows = load_history(
            dir.path(),
            "now",
            &release("2018"),
            &MeasureHistory::Runs(2),
            at(0),
        );
        assert_eq!(ids(&rows), ["b", "c", "c"]);
        assert_eq!(rows[1].name, "m.a");
        assert_eq!(rows[2].name, "m.b");
        assert_eq!(rows[2].value, Some(30.0));
        assert_eq!(rows[0].run_at, "2026-10-02T00:00:00Z");
    }

    #[test]
    fn history_since_keeps_runs_inside_the_window() {
        let dir = write_runs(&[
            (
                "a.json",
                record(
                    "a",
                    Some("2026-10-01T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 1.0)],
                ),
            ),
            (
                "b.json",
                record(
                    "b",
                    Some("2026-10-09T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 2.0)],
                ),
            ),
            (
                "c.json",
                record(
                    "c",
                    Some("2026-10-10T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 3.0)],
                ),
            ),
        ]);
        // 2026-10-10T12:00:00Z, with a window of two days: the cut is 2026-10-08T12:00:00Z.
        let now = iso_to_epoch_secs("2026-10-10T12:00:00Z").unwrap() as u64;
        let window = MeasureHistory::Since(Duration::from_secs(2 * 86_400));
        let rows = load_history(dir.path(), "now", &release("2018"), &window, at(now));
        assert_eq!(ids(&rows), ["b", "c"]);
        // A start exactly at the cut stays in.
        let exact = MeasureHistory::Since(Duration::from_secs(86_400 + 12 * 3_600));
        let rows = load_history(dir.path(), "now", &release("2018"), &exact, at(now));
        assert_eq!(ids(&rows), ["b", "c"]);
    }

    #[test]
    fn history_keeps_only_runs_with_the_same_params() {
        let dir = write_runs(&[
            (
                "a.json",
                record(
                    "a",
                    Some("2026-10-01T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 1.0)],
                ),
            ),
            (
                "b.json",
                record(
                    "b",
                    Some("2026-10-02T00:00:00Z"),
                    "success",
                    "2010",
                    &[("m.a", 2.0)],
                ),
            ),
        ]);
        let rows = load_history(
            dir.path(),
            "now",
            &release("2018"),
            &MeasureHistory::Runs(30),
            at(0),
        );
        assert_eq!(ids(&rows), ["a"]);
    }

    fn params(pairs: &[(&str, &str)]) -> Vec<ParamEntry> {
        pairs
            .iter()
            .map(|(k, v)| ParamEntry {
                key: k.to_string(),
                value: v.to_string(),
                source: "cli".into(),
            })
            .collect()
    }

    #[test]
    fn same_params_ignores_the_order_params_are_declared_in() {
        let a = params(&[("release", "2018"), ("region", "eu")]);
        let b = params(&[("region", "eu"), ("release", "2018")]);
        assert!(same_params(&a, &b));
    }

    /// A record restored from the published copy holds `[redacted]` for a dotenv value, and
    /// a secret-looking key is redacted in the local record too: either matches any value.
    #[test]
    fn same_params_matches_a_redacted_value_to_any_value() {
        let current = params(&[("release", "2018")]);
        assert!(same_params(&params(&[("release", "[redacted]")]), &current));
        assert!(same_params(&current, &params(&[("release", "[redacted]")])));
        assert!(same_params(
            &params(&[("release", "***REDACTED***")]),
            &current
        ));
    }

    #[test]
    fn same_params_still_tells_plain_values_and_keys_apart() {
        let current = params(&[("release", "2018")]);
        assert!(!same_params(&params(&[("release", "2010")]), &current));
        assert!(!same_params(&params(&[("year", "2018")]), &current));
        assert!(!same_params(
            &params(&[("release", "2018"), ("region", "eu")]),
            &current
        ));
    }

    #[test]
    fn history_runs_window_counts_only_runs_that_measured() {
        let dir = write_runs(&[
            (
                "a.json",
                record(
                    "a",
                    Some("2026-10-01T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 1.0)],
                ),
            ),
            (
                "b.json",
                record("b", Some("2026-10-02T00:00:00Z"), "success", "2018", &[]),
            ),
        ]);
        let rows = load_history(
            dir.path(),
            "now",
            &release("2018"),
            &MeasureHistory::Runs(1),
            at(0),
        );
        assert_eq!(ids(&rows), ["a"]);
    }

    #[test]
    fn history_compares_params_by_key_and_value_whatever_their_source() {
        let with_source = |id: &str, started: &str, value: &str, source: &str| {
            let mut v: serde_json::Value = serde_json::from_str(&record(
                id,
                Some(started),
                "success",
                value,
                &[("m.a", 1.0)],
            ))
            .unwrap();
            v["run"]["params"][0]["source"] = source.into();
            v.to_string()
        };
        let dir = write_runs(&[
            (
                "a.json",
                with_source("a", "2026-10-01T00:00:00Z", "2018", "default"),
            ),
            (
                "b.json",
                with_source("b", "2026-10-02T00:00:00Z", "2018", "env"),
            ),
            (
                "c.json",
                with_source("c", "2026-10-03T00:00:00Z", "2010", "cli"),
            ),
        ]);
        let rows = load_history(
            dir.path(),
            "now",
            &release("2018"),
            &MeasureHistory::Runs(30),
            at(0),
        );
        assert_eq!(ids(&rows), ["a", "b"]);
    }

    #[test]
    fn history_skips_unreadable_old_failed_and_current_records() {
        let mut old: serde_json::Value = serde_json::from_str(&record(
            "old",
            Some("2026-10-01T00:00:00Z"),
            "success",
            "2018",
            &[],
        ))
        .unwrap();
        old["contract_version"] = "b4/1".into();
        old["steps"][0].as_object_mut().unwrap().remove("measure");
        let dir = write_runs(&[
            ("truncated.json", "{\"run\": {".to_string()),
            ("old.json", old.to_string()),
            (
                "partial.json",
                record(
                    "partial",
                    Some("2026-10-02T00:00:00Z"),
                    "partial",
                    "2018",
                    &[("m.a", 1.0)],
                ),
            ),
            (
                "current.json",
                record(
                    "now",
                    Some("2026-10-03T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 2.0)],
                ),
            ),
            (
                "unstarted.json",
                record("unstarted", None, "success", "2018", &[("m.a", 3.0)]),
            ),
            (
                "good.json",
                record(
                    "good",
                    Some("2026-10-04T00:00:00Z"),
                    "success",
                    "2018",
                    &[("m.a", 4.0)],
                ),
            ),
        ]);
        let rows = load_history(
            dir.path(),
            "now",
            &release("2018"),
            &MeasureHistory::Runs(30),
            at(0),
        );
        assert_eq!(ids(&rows), ["good"]);
        // A folder that is not there is an empty history.
        let none = load_history(
            &dir.path().join("nope"),
            "now",
            &[],
            &MeasureHistory::Runs(30),
            at(0),
        );
        assert!(none.is_empty());
    }

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
    fn a_query_knows_the_lines_above_it() {
        let text = "-- a header\n-- of two lines\n\n  SELECT 1\nFROM t;\n";
        let start = text.find("--").unwrap();
        let query = query_at(text, start, text.len());
        // The reader's range can start at a comment; the lines are counted to the query.
        assert_eq!(query.lines_before, 0);
        let start = text.find("SELECT").unwrap() - 2;
        let query = query_at(text, start, text.len());
        assert_eq!(query.sql, "SELECT 1\nFROM t");
        assert_eq!(query.lines_before, 3);
        let query = query_at("SELECT 1;", 0, 9);
        assert_eq!((query.sql.as_str(), query.lines_before), ("SELECT 1", 0));
    }

    #[test]
    fn a_wrapper_puts_the_query_on_its_files_line() {
        let query = StepQuery {
            sql: "SELECT 1".into(),
            lines_before: 2,
        };
        let sql = wrapper(Path::new("/tmp/a"), &query, "SELECT 1");
        assert!(
            sql.contains("CREATE TEMP TABLE __arc_rows AS \n\nSELECT 1\n;\n"),
            "{sql}"
        );
        let first = StepQuery {
            lines_before: 0,
            ..query
        };
        let sql = wrapper(Path::new("/tmp/a"), &first, "SELECT 1");
        assert!(
            sql.contains("CREATE TEMP TABLE __arc_rows AS SELECT 1\n;\n"),
            "{sql}"
        );
    }

    #[test]
    fn numeric_types_are_the_integers_floats_and_decimals() {
        for kind in [
            "TINYINT",
            "UHUGEINT",
            "HUGEINT",
            "UBIGINT",
            "FLOAT",
            "DOUBLE",
            "DECIMAL(18,3)",
        ] {
            assert!(is_numeric_type(kind), "{kind}");
        }
        for kind in [
            "VARCHAR",
            "BOOLEAN",
            "DATE",
            "INTEGER[]",
            "DECIMAL",
            "BIGNUM",
        ] {
            assert!(!is_numeric_type(kind), "{kind}");
        }
    }

    fn measure_step() -> Step {
        let yaml = "name: t\nsteps:\n  - name: m\n    measure: m.sql\n";
        Manifest::from_yaml_str(yaml).unwrap().steps.remove(0)
    }

    fn answer(rows: u64, columns: &[(&str, &str)], first: &[Option<&str>]) -> MeasureAnswer {
        MeasureAnswer {
            rows_returned: rows,
            columns: columns
                .iter()
                .map(|(name, kind)| MeasureColumn {
                    name: name.to_string(),
                    kind: kind.to_string(),
                })
                .collect(),
            first: vec![first.iter().map(|c| c.map(String::from)).collect()],
        }
    }

    #[test]
    fn a_measure_keeps_its_column_order_and_nulls() {
        let values = measure_values(
            &measure_step(),
            answer(
                1,
                &[("z", "INTEGER"), ("a", "DECIMAL(2,1)"), ("n", "BIGINT")],
                &[None, Some("0.5"), Some("3.0")],
            ),
        )
        .unwrap();
        assert_eq!(
            values.into_iter().collect::<Vec<_>>(),
            [
                ("m.z".to_string(), None),
                ("m.a".to_string(), Some(0.5)),
                ("m.n".to_string(), Some(3.0))
            ]
        );
    }

    #[test]
    fn a_measure_is_refused_unless_one_finite_numeric_row() {
        let step = measure_step();
        let refused = |a: MeasureAnswer| measure_values(&step, a).unwrap_err().to_string();
        let two = refused(answer(2, &[("n", "BIGINT")], &[Some("1.0")]));
        assert!(
            two.contains("step 'm'") && two.contains("exactly one row"),
            "{two}"
        );
        let none = refused(MeasureAnswer {
            rows_returned: 0,
            columns: Vec::new(),
            first: Vec::new(),
        });
        assert!(none.contains("returned 0 rows"), "{none}");
        let text = refused(answer(1, &[("s", "VARCHAR")], &[None]));
        assert!(
            text.contains("'s' is VARCHAR") && text.contains("numeric"),
            "{text}"
        );
        let dot = refused(answer(1, &[("a.b", "INTEGER")], &[Some("1.0")]));
        assert!(dot.contains("'a.b'"), "{dot}");
        let inf = refused(answer(1, &[("x", "DOUBLE")], &[Some("inf")]));
        assert!(inf.contains("not a finite number"), "{inf}");
    }

    #[test]
    fn a_table_is_measured_and_a_view_or_odd_name_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.duckdb");
        {
            let conn = duckdb::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE t AS SELECT * FROM range(3) r(n); CREATE VIEW v AS SELECT 1 AS n;",
            )
            .unwrap();
        }
        let tables = ["t", "v", "gone", "build/x.parquet"].map(String::from);
        let values = measure_tables(&db, &tables);
        assert_eq!(
            values.keys().collect::<Vec<_>>(),
            ["table:t.row_count", "table:t.bytes"]
        );
        assert_eq!(values["table:t.row_count"], Some(3.0));
        assert!(
            values["table:t.bytes"].is_some_and(|b| b > 0.0),
            "{values:?}"
        );
        assert!(measure_tables(&dir.path().join("none.duckdb"), &tables).is_empty());
    }

    #[test]
    fn a_table_named_as_a_keyword_is_measured() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("m.duckdb");
        {
            let conn = duckdb::Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE \"order\" AS SELECT * FROM range(2) r(n);")
                .unwrap();
        }
        let values = measure_tables(&db, &["order".to_string()]);
        assert_eq!(
            values.get("table:order.row_count"),
            Some(&Some(2.0)),
            "{values:?}"
        );
        assert!(
            values["table:order.bytes"].is_some_and(|b| b > 0.0),
            "{values:?}"
        );
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
