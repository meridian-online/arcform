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
        return Err(Error::ManifestValidation(format!(
            "no database for this Protocol yet at {}: run the Protocol (arc run) to make it",
            path.display()
        )));
    }
    let config = Config::default()
        .access_mode(AccessMode::ReadOnly)
        .map_err(db_error)?;
    let conn = Connection::open_with_flags(&path, config).map_err(|e| {
        if is_lock_error(&e.to_string()) {
            Error::ManifestValidation(format!(
                "the Protocol's database at {} is in use by another process; try again when the run or app holding it has finished",
                path.display()
            ))
        } else {
            db_error(e)
        }
    })?;
    Ok((conn, path))
}

fn is_lock_error(message: &str) -> bool {
    message.to_ascii_lowercase().contains("lock")
}

fn db_error(e: duckdb::Error) -> Error {
    Error::ManifestValidation(format!("database: {e}"))
}

/// A SQL identifier, quoted: wrapped in `"` with each `"` inside doubled.
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// What the table or view `table` holds: its row count, each column's name, type and
/// null count, and up to five rows as DuckDB's own JSON.
pub(crate) fn describe_table(dir: &Path, table: &str) -> Result<Value> {
    let (conn, path) = open_read_only(dir)?;
    describe_in(&conn, &path, table).map_err(db_error_or)
}

/// An engine error from DuckDB is shown as such; one of ours passes through.
fn db_error_or(e: DescribeError) -> Error {
    match e {
        DescribeError::Ours(e) => e,
        DescribeError::Duck(e) => db_error(e),
    }
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

fn describe_in(
    conn: &Connection,
    path: &Path,
    table: &str,
) -> std::result::Result<Value, DescribeError> {
    let mut stmt = conn.prepare(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema NOT IN ('information_schema', 'pg_catalog') \
         ORDER BY table_name",
    )?;
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if !tables.iter().any(|t| t.eq_ignore_ascii_case(table)) {
        return Err(DescribeError::Ours(Error::ManifestValidation(format!(
            "no table or view named {table} in {}; the tables are: {}",
            path.display(),
            tables.join(", ")
        ))));
    }
    let quoted = quote(table);

    let mut stmt = conn.prepare(&format!(
        "SELECT column_name, column_type FROM (DESCRIBE {quoted})"
    ))?;
    let described: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut select = String::from("SELECT count(*)");
    for (name, _) in &described {
        select.push_str(&format!(", count(*) - count({})", quote(name)));
    }
    select.push_str(&format!(" FROM {quoted}"));
    let counts: Vec<i64> = conn.query_row(&select, [], |r| {
        (0..=described.len()).map(|i| r.get::<_, i64>(i)).collect()
    })?;

    let columns: Vec<Value> = described
        .iter()
        .zip(&counts[1..])
        .map(|((name, ty), nulls)| json!({ "name": name, "type": ty, "null_count": nulls }))
        .collect();

    let mut stmt = conn.prepare(&format!(
        "SELECT to_json(t) FROM {quoted} t LIMIT {SAMPLE_ROWS}"
    ))?;
    let sample: Vec<Value> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .map(|row| {
            let text = row?;
            serde_json::from_str(&text).map_err(|e| {
                DescribeError::Ours(Error::ManifestValidation(format!(
                    "database: a sample row was not JSON: {e}"
                )))
            })
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
