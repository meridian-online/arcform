//! Look inside a Protocol's database without changing it: the one place a read-only
//! connection is opened, and the verbs built on it (`arc table describe` first).
//!
//! The database is opened with DuckDB's read-only access mode, so nothing here can
//! write to it, and a file that is not there is refused rather than created.

use std::path::{Path, PathBuf};

use duckdb::{AccessMode, Config, Connection};
use serde_json::{Value, json};

use crate::error::{Error, Result};

/// How many rows `describe_table` samples.
const SAMPLE_ROWS: usize = 5;

/// Open the Protocol's database in `dir` read-only, and say where it is.
///
/// The manifest names the database (`db:`, else arc's data folder). A database that
/// does not exist yet is refused, since a read-only open would not make it and the
/// reader should be told to run the Protocol. A file another process holds for
/// writing is refused with a message that says to try again.
pub(crate) fn open_read_only(dir: &Path) -> Result<(Connection, PathBuf)> {
    let manifest = crate::manifest::Manifest::load(dir)?;
    let path = manifest.db_path(dir)?;
    if !path.is_file() {
        return Err(refusal(format!(
            "no database for this Protocol yet at {}: run the Protocol (arc run) to make it",
            path.display()
        )));
    }
    let config = Config::default()
        .access_mode(AccessMode::ReadOnly)
        .map_err(db_error)?;
    let conn = Connection::open_with_flags(&path, config).map_err(|e| {
        if is_lock_error(&e.to_string()) {
            refusal(format!(
                "the Protocol's database at {} is in use by another process; try again when the run or app holding it has finished",
                path.display()
            ))
        } else {
            db_error(e)
        }
    })?;
    Ok((conn, path))
}

/// DuckDB's own wording when another process holds the file.
const LOCK_WORDING: [&str; 2] = ["Could not set lock on file", "Conflicting lock is held"];

fn is_lock_error(message: &str) -> bool {
    LOCK_WORDING.iter().any(|w| message.contains(w))
}

/// A refusal printed as its message alone: `Error::Io`'s text has no prefix, and a
/// missing database or a missing table is not an invalid manifest.
fn refusal(message: String) -> Error {
    Error::Io(std::io::Error::other(message))
}

fn db_error(e: duckdb::Error) -> Error {
    refusal(format!("database: {e}"))
}

/// A SQL identifier, quoted: wrapped in `"` with each `"` inside doubled.
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// What the table or view `table` holds: its row count, each column's name, type and
/// null count, and up to five rows, each an array of the row's values as DuckDB's
/// `to_json` writes them, in the order of `columns`.
///
/// `table` is `schema.table` or a bare name. A bare name is the one in `main` if
/// there is one, else the one schema that holds it.
pub(crate) fn describe_table(dir: &Path, table: &str) -> Result<Value> {
    let (conn, path) = open_read_only(dir)?;
    describe_in(&conn, &path, table).map_err(|e| match e {
        DescribeError::Ours(e) => e,
        DescribeError::Duck(e) => db_error(e),
    })
}

enum DescribeError {
    Ours(Error),
    Duck(duckdb::Error),
}

impl From<duckdb::Error> for DescribeError {
    fn from(e: duckdb::Error) -> Self {
        DescribeError::Duck(e)
    }
}

fn ours(message: String) -> DescribeError {
    DescribeError::Ours(refusal(message))
}

/// The name that can be passed back: bare in `main`, `schema.table` elsewhere.
fn display(schema: &str, name: &str) -> String {
    if schema == "main" {
        name.to_string()
    } else {
        format!("{schema}.{name}")
    }
}

/// Which `(schema, table)` the caller means, or why it cannot be told.
fn resolve(
    entries: &[(String, String)],
    table: &str,
    path: &Path,
) -> std::result::Result<(String, String), DescribeError> {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    if let Some((schema, name)) = table.split_once('.')
        && let Some((s, n)) = entries
            .iter()
            .find(|(s, n)| same(s, schema) && same(n, name))
    {
        return Ok((s.clone(), n.clone()));
    }
    let candidates: Vec<&(String, String)> =
        entries.iter().filter(|(_, n)| same(n, table)).collect();
    if let Some((s, n)) = candidates.iter().find(|(s, _)| s == "main") {
        return Ok((s.clone(), n.clone()));
    }
    match candidates.as_slice() {
        [(s, n)] => Ok((s.clone(), n.clone())),
        [] => {
            let mut names: Vec<String> = entries.iter().map(|(s, n)| display(s, n)).collect();
            names.sort();
            names.dedup();
            Err(ours(format!(
                "no table or view named {table} in {}; the tables are: {}",
                path.display(),
                names.join(", ")
            )))
        }
        several => {
            let mut names: Vec<String> = several.iter().map(|(s, n)| format!("{s}.{n}")).collect();
            names.sort();
            Err(ours(format!(
                "{table} is in more than one schema in {}; name one of: {}",
                path.display(),
                names.join(", ")
            )))
        }
    }
}

fn describe_in(
    conn: &Connection,
    path: &Path,
    table: &str,
) -> std::result::Result<Value, DescribeError> {
    let mut stmt = conn.prepare(
        "SELECT table_schema, table_name FROM information_schema.tables \
         WHERE table_schema NOT IN ('information_schema', 'pg_catalog')",
    )?;
    let entries: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let (schema, name) = resolve(&entries, table, path)?;
    let quoted = format!("{}.{}", quote(&schema), quote(&name));

    let mut stmt = conn.prepare(&format!(
        "SELECT column_name, column_type FROM (DESCRIBE {quoted})"
    ))?;
    let described: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut select = String::from("SELECT count(*)");
    for (column, _) in &described {
        select.push_str(&format!(", count(*) - count({})", quote(column)));
    }
    select.push_str(&format!(" FROM {quoted}"));
    let counts: Vec<i64> = conn.query_row(&select, [], |r| {
        (0..=described.len()).map(|i| r.get::<_, i64>(i)).collect()
    })?;

    let columns: Vec<Value> = described
        .iter()
        .zip(&counts[1..])
        .map(|((column, ty), nulls)| json!({ "name": column, "type": ty, "null_count": nulls }))
        .collect();

    // Each value is converted on its own, with no alias for the row, so a column
    // named like the alias cannot shadow it, and the values keep the column order.
    let values: Vec<String> = described
        .iter()
        .map(|(column, _)| format!("to_json({})", quote(column)))
        .collect();
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM {quoted} LIMIT {SAMPLE_ROWS}",
        values.join(", ")
    ))?;
    let sample: Vec<Value> = stmt
        .query_map([], |r| {
            (0..described.len())
                .map(|i| r.get::<_, Option<String>>(i))
                .collect::<std::result::Result<Vec<_>, _>>()
        })?
        .map(|row| {
            let cells = row?;
            let parsed: std::result::Result<Vec<Value>, _> = cells
                .iter()
                .map(|c| match c {
                    Some(text) => serde_json::from_str(text),
                    None => Ok(Value::Null),
                })
                .collect();
            parsed
                .map(Value::Array)
                .map_err(|e| ours(format!("database: a sample value was not JSON: {e}")))
        })
        .collect::<std::result::Result<_, DescribeError>>()?;

    Ok(json!({
        "table": table,
        "database": path.display().to_string(),
        "row_count": counts[0],
        "columns": columns,
        "sample": sample,
    }))
}
