//! Look inside a Protocol's database without changing it: the one place a read-only
//! connection is opened, and the verbs built on it: `arc table describe` and
//! `arc sql query`.
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
///
/// DuckDB's external access stays on, so a view over a Parquet or CSV file, and a
/// query naming a file or a URL, can be read. Installing an extension DuckDB knows
/// is off: an extension already installed may still load, but none is fetched.
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
        .and_then(|c| c.with("autoinstall_known_extensions", "false"))
        .map_err(db_error)?;
    let conn = Connection::open_with_flags(&path, config).map_err(|e| {
        if is_lock_error(&e.to_string()) {
            lock_refusal(&path)
        } else {
            db_error(e)
        }
    })?;
    Ok((conn, path))
}

/// The refusal for a database file another process holds.
fn lock_refusal(path: &Path) -> Error {
    refusal(format!(
        "the Protocol's database at {} is in use by another process; try again when the run or app holding it has finished",
        path.display()
    ))
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

/// The select-list expression that gives the value of `expr`, a column of DuckDB
/// type `ty`, as JSON text. A HUGEINT or UHUGEINT at the top level of a row comes
/// back as a JSON number while it lies within ±2^53, where a double holds every
/// integer exactly, and as a JSON string of its exact digits outside that range. A
/// DECIMAL of more than 15 digits at the top level of a row is always cast to text,
/// so it comes back as a string of its exact digits rather than a rounded number. A
/// value nested in a LIST, STRUCT or MAP is written as DuckDB's `to_json` writes it.
fn json_value_sql(expr: &str, ty: &str) -> String {
    const EXACT: u64 = 1 << 53;
    // A UHUGEINT is compared with no negative bound: DuckDB reads a negative literal
    // beside one as a HUGEINT, which the widest UHUGEINTs overflow.
    let exact = match ty.trim().to_ascii_uppercase().as_str() {
        "HUGEINT" => format!("{expr} BETWEEN -{EXACT} AND {EXACT}"),
        "UHUGEINT" => format!("{expr} <= {EXACT}"),
        _ if is_wide_decimal(ty) => return format!("to_json({expr}::VARCHAR)"),
        _ => return format!("to_json({expr})"),
    };
    format!("CASE WHEN {exact} THEN to_json({expr}::BIGINT) ELSE to_json({expr}::VARCHAR) END")
}

/// Whether a column of DuckDB type `ty` is a DECIMAL whose numbers a double cannot
/// hold exactly.
fn is_wide_decimal(ty: &str) -> bool {
    let ty = ty.trim().to_ascii_uppercase();
    // A DECIMAL at the top level of a row: its type ends at the `)` of its
    // precision and scale. `DECIMAL(38,2)[]` or a STRUCT holding one is a nested
    // value, written as DuckDB's `to_json` writes it.
    if !ty.ends_with(')') {
        return false;
    }
    let precision = ty
        .strip_prefix("DECIMAL(")
        .or_else(|| ty.strip_prefix("NUMERIC("))
        .and_then(|rest| rest.split([',', ')']).next())
        .and_then(|p| p.trim().parse::<u32>().ok());
    precision.is_some_and(|p| p > 15)
}

/// One cell of `to_json` text as a JSON value. DuckDB writes a non-finite double as
/// a bare `NaN`, `Infinity` or `-Infinity`, which is not JSON; a cell that does not
/// parse is kept as a JSON string of DuckDB's text, never an error.
fn cell_value(text: Option<String>) -> Value {
    match text {
        None => Value::Null,
        Some(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
    }
}

/// The rows `sql` returns, each an array of its `width` cells read by [`cell_value`].
/// Each cell of `sql`'s select list is one `to_json` text, as [`json_value_sql`]
/// writes it.
fn json_rows(
    conn: &Connection,
    sql: &str,
    width: usize,
) -> std::result::Result<Vec<Value>, duckdb::Error> {
    let mut stmt = conn.prepare(sql)?;
    stmt.query_map([], |r| {
        (0..width)
            .map(|i| r.get::<_, Option<String>>(i).map(cell_value))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(Value::Array)
    })?
    .collect()
}

/// What the table or view `table` holds: its row count, each column's name, type and
/// null count, and up to five rows, each an array of the row's values as DuckDB's
/// `to_json` writes them, in the order of `columns`. A value JSON cannot carry as
/// DuckDB writes it (`NaN`, `Infinity`) and a number a double cannot hold exactly
/// come back as strings: see [`json_value_sql`] and [`cell_value`].
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

/// Which `(schema, table)` of the database `table` names, or why none does.
fn find_table(
    conn: &Connection,
    table: &str,
    path: &Path,
) -> std::result::Result<(String, String), DescribeError> {
    let mut stmt = conn.prepare(
        "SELECT table_schema, table_name FROM information_schema.tables \
         WHERE table_schema NOT IN ('information_schema', 'pg_catalog')",
    )?;
    let entries: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    resolve(&entries, table, path)
}

/// The table `table` names in the Protocol's database `conn` is open on, as its
/// quoted `schema.table`, or the refusal.
fn locate_table(conn: &Connection, table: &str, path: &Path) -> Result<(String, String)> {
    find_table(conn, table, path).map_err(|e| match e {
        DescribeError::Ours(e) => e,
        DescribeError::Duck(e) => db_error(e),
    })
}

fn describe_in(
    conn: &Connection,
    path: &Path,
    table: &str,
) -> std::result::Result<Value, DescribeError> {
    let (schema, name) = find_table(conn, table, path)?;
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
        .map(|(column, ty)| json_value_sql(&quote(column), ty))
        .collect();
    let sample = json_rows(
        conn,
        &format!(
            "SELECT {} FROM {quoted} LIMIT {SAMPLE_ROWS}",
            values.join(", ")
        ),
        described.len(),
    )?;

    Ok(json!({
        "table": table,
        "database": path.display().to_string(),
        "row_count": counts[0],
        "columns": columns,
        "sample": sample,
    }))
}

/// What `arc sql query` and the `sql_query` tool give after the results: how to
/// keep a query as a step, from the command line and from `arc mcp`.
const KEEP: &str =
    "to keep a query as a step: `arc sql record '<statement>' --name <name>` (MCP: sql_record)";

/// The start of every refusal the gate makes.
const GATE_REFUSAL: &str =
    "only read-only SELECT statements can be queried; refused before anything ran";

/// The most rows `query` returns for each statement.
pub(crate) const MAX_LIMIT: usize = 100_000;

/// The query nodes a SELECT statement is made of, as `json_serialize_sql` names them:
/// a plain SELECT, a UNION and its kin, and a WITH.
const SELECT_NODES: [&str; 4] = [
    "SELECT_NODE",
    "SET_OPERATION_NODE",
    "CTE_NODE",
    "RECURSIVE_CTE_NODE",
];

/// Run the SELECT statements in `sql` against the Protocol's database in `dir`,
/// read-only, and return each one's columns and up to `limit` rows.
///
/// **The gate.** Before anything runs, the whole text is handed to DuckDB's
/// `json_serialize_sql` on the read-only connection. It serialises SELECT statements
/// alone, so if it reports an error (a statement of another kind, or text it cannot
/// parse), or a statement in it is not a SELECT, the whole text is refused and none
/// of it runs. The read-only connection is the backstop for a SELECT that would
/// still write, such as one calling `nextval`. A SELECT may still read the files
/// and URLs it names (see [`open_read_only`]); the database is never written.
///
/// **Each statement.** The statements are the ones DuckDB's parse split the text
/// into for the gate, each turned back into SQL by `json_deserialize_sql`: DuckDB
/// gives no other way to have a statement's text from its split. Each statement's
/// columns and types come from `DESCRIBE`, and its rows from the statement wrapped
/// as a subquery whose columns are renamed by position, so a column name cannot
/// shadow another, each value read as [`describe_table`]'s sample is. When more
/// than `limit` rows come back, `capped` is true and `rows_held_back` says how many
/// rows past `limit` the statement returns. That is a recount: the statement runs a
/// second time under `count(*)`, so over data that changed in between it is the
/// second run's count, never below 0.
///
/// **Errors.** A statement that fails while running gives `{"statement": n,
/// "error": <DuckDB's message>}` in its place, and the others' results are kept.
/// The `Err` this returns is a refusal before anything ran: the gate, a `limit`
/// above [`MAX_LIMIT`], a database that is missing or held by another process.
pub(crate) fn query(dir: &Path, sql: &str, limit: usize) -> Result<Value> {
    if limit > MAX_LIMIT {
        return Err(refusal(format!(
            "the limit is at most {MAX_LIMIT} rows for each statement; {limit} was asked for"
        )));
    }
    let (conn, _path) = open_read_only(dir)?;
    let statements = gate(&conn, sql)?;
    let results: Vec<Value> = statements
        .iter()
        .enumerate()
        .map(|(index, statement)| {
            run_statement(&conn, index + 1, statement, limit)
                .unwrap_or_else(|e| json!({ "statement": index + 1, "error": e.to_string() }))
        })
        .collect();
    Ok(json!({ "results": results, "keep": KEEP }))
}

/// The numbers of the statements in a [`query`] answer that failed while running.
pub(crate) fn failed_statements(answer: &Value) -> Vec<u64> {
    answer["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r.get("error").is_some())
        .filter_map(|r| r["statement"].as_u64())
        .collect()
}

/// The SQL of each statement in `sql`, or the gate's refusal: see [`query`].
fn gate(conn: &Connection, sql: &str) -> Result<Vec<String>> {
    let serialised: String = conn
        .query_row("SELECT json_serialize_sql(?::VARCHAR)", [sql], |r| r.get(0))
        .map_err(db_error)?;
    let parsed: Value = serde_json::from_str(&serialised)
        .map_err(|e| refusal(format!("database: json_serialize_sql gave no JSON: {e}")))?;
    let refuse = |why: String| refusal(format!("{GATE_REFUSAL}: {why}"));
    if parsed["error"] != Value::Bool(false) {
        let message = parsed["error_message"]
            .as_str()
            .unwrap_or("DuckDB could not read the text");
        // DuckDB's own wording here names its serialiser, not the query; a parser
        // error is passed through as DuckDB gives it.
        if message.contains("Only SELECT statements can be serialized") {
            return Err(refuse("a statement is not a SELECT".to_string()));
        }
        return Err(refuse(message.to_string()));
    }
    let statements = parsed["statements"].as_array().cloned().unwrap_or_default();
    if statements.is_empty() {
        return Err(refuse("the text holds no SQL statement".to_string()));
    }
    for (index, statement) in statements.iter().enumerate() {
        let node = statement["node"]["type"].as_str().unwrap_or("");
        if !SELECT_NODES.contains(&node) {
            return Err(refuse(format!(
                "statement {} is not a SELECT (DuckDB reads it as {node})",
                index + 1
            )));
        }
    }
    statements
        .into_iter()
        .map(|statement| {
            let one = json!({ "error": false, "statements": [statement] }).to_string();
            conn.query_row("SELECT json_deserialize_sql(?::JSON)", [one], |r| {
                r.get::<_, String>(0)
            })
            .map_err(db_error)
        })
        .collect()
}

/// One statement's result: see [`query`].
fn run_statement(
    conn: &Connection,
    number: usize,
    statement: &str,
    limit: usize,
) -> std::result::Result<Value, duckdb::Error> {
    let mut stmt = conn.prepare(&format!(
        "SELECT column_name, column_type FROM (DESCRIBE ({statement}))"
    ))?;
    let described: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;

    // The statement's columns are renamed `c0`, `c1`, ... by position, and read
    // qualified by the subquery's alias, so no name the statement chose (two
    // columns called `a`, a column called like the alias) can shadow another.
    let renamed: Vec<String> = (0..described.len()).map(|i| format!("c{i}")).collect();
    let values: Vec<String> = described
        .iter()
        .zip(&renamed)
        .map(|((_, ty), column)| json_value_sql(&format!("q.{column}"), ty))
        .collect();
    let wrapped = format!("({statement}) AS q({})", renamed.join(", "));
    let fetch = limit.saturating_add(1);
    let mut rows = json_rows(
        conn,
        &format!("SELECT {} FROM {wrapped} LIMIT {fetch}", values.join(", ")),
        described.len(),
    )?;

    let capped = rows.len() > limit;
    let held_back = if capped {
        rows.truncate(limit);
        let total: i64 =
            conn.query_row(&format!("SELECT count(*) FROM {wrapped}"), [], |r| r.get(0))?;
        json!((total - limit as i64).max(0))
    } else {
        Value::Null
    };
    let columns: Vec<Value> = described
        .iter()
        .map(|(name, ty)| json!({ "name": name, "type": ty }))
        .collect();
    Ok(json!({
        "statement": number,
        "columns": columns,
        "row_count": rows.len(),
        "rows": rows,
        "capped": capped,
        "rows_held_back": held_back,
    }))
}

// ─────────────────────────────────────────────────────────────────────────────
// profile and validate
// ─────────────────────────────────────────────────────────────────────────────

/// The rows `profile_table` copies when it is not told how many: FineType's own
/// default sample.
const DEFAULT_SAMPLE_SIZE: usize = 100;

/// What `finetype profile` writes for a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileFormat {
    /// Each column's type, confidence and transform.
    Json,
    /// A table-level JSON Schema that the table's rows pass.
    JsonSchema,
}

impl ProfileFormat {
    fn finetype_word(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::JsonSchema => "json-schema",
        }
    }
}

/// A folder made for one profile, outside the Protocol, that is removed when it
/// goes out of scope.
struct ScratchDir(PathBuf);

impl ScratchDir {
    /// A new, empty folder in the temporary directory: one that already exists is
    /// never reused.
    fn new() -> Result<Self> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        for attempt in 0..100u32 {
            let path = std::env::temp_dir().join(format!(
                "arc-profile-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(ScratchDir(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(refusal(
            "could not make a temporary folder for the profile".to_string(),
        ))
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Profile the table or view `table` of the Protocol's database with FineType.
///
/// Up to `sample_size` rows (FineType's default of 100 when none is given) are
/// copied to a temporary CSV outside the Protocol, through a read-only
/// connection, and `finetype profile` is run over it. The answer is FineType's
/// JSON with `table` and `rows_profiled` added; for `JsonSchema` it is the schema
/// alone, its `$id` naming the table. A table with no rows is not an
/// error: the answer says so, and FineType is not run.
pub(crate) fn profile_table(
    dir: &Path,
    table: &str,
    format: ProfileFormat,
    sample_size: Option<usize>,
) -> Result<Value> {
    if sample_size == Some(0) {
        return Err(refusal(
            "sample_size must be at least 1: a sample of no rows profiles nothing".to_string(),
        ));
    }
    let limit = sample_size.unwrap_or(DEFAULT_SAMPLE_SIZE);
    let (conn, path) = open_read_only(dir)?;
    let (schema, name) = locate_table(&conn, table, &path)?;
    let quoted = format!("{}.{}", quote(&schema), quote(&name));
    let rows = format!("SELECT * FROM {quoted} LIMIT {limit}");
    let copied: i64 = conn
        .query_row(&format!("SELECT count(*) FROM ({rows})"), [], |r| r.get(0))
        .map_err(db_error)?;
    if copied == 0 {
        return Ok(json!({
            "table": table,
            "rows_profiled": 0,
            "message": format!("{table} has no rows to profile"),
        }));
    }

    let bin = crate::finetype_cli::finetype_bin();
    crate::finetype_cli::ensure_min_version(&bin).map_err(refusal)?;
    let scratch = ScratchDir::new()?;
    let csv = scratch.0.join("table.csv");
    conn.execute_batch(&format!(
        "COPY ({rows}) TO '{}' (HEADER)",
        csv.display().to_string().replace('\'', "''")
    ))
    .map_err(db_error)?;

    let mut args = vec![
        "profile".to_string(),
        "--file".to_string(),
        csv.display().to_string(),
        "--output".to_string(),
        format.finetype_word().to_string(),
    ];
    if let Some(n) = sample_size {
        args.push("--sample-size".to_string());
        args.push(n.to_string());
    }
    let stdout = crate::finetype_cli::run_cli(&bin, &args, None).map_err(refusal)?;
    let mut profile: Value = serde_json::from_str(&stdout)
        .map_err(|e| refusal(format!("finetype profile did not give JSON: {e}")))?;
    let Some(object) = profile.as_object_mut() else {
        return Err(refusal(
            "finetype profile did not give a JSON object".to_string(),
        ));
    };
    match format {
        ProfileFormat::Json => {
            object.insert("table".to_string(), json!(table));
            object.insert("rows_profiled".to_string(), json!(copied));
        }
        // A schema is kept as FineType wrote it, so what is saved is a clean JSON
        // Schema; only the `$id`, which names the scratch file, is made to name the table.
        ProfileFormat::JsonSchema => {
            if object.contains_key("$id") {
                object.insert("$id".to_string(), json!(format!("finetype://{table}")));
            }
        }
    }
    Ok(profile)
}

/// Validate the table or view `table` of the Protocol's database against the
/// JSON Schema file `schema` (relative to `dir`, and inside it) with FineType's
/// DuckDB extension: the database is attached read-only to an in-memory
/// connection that has the extension loaded, and `ft_validate` is run over the
/// table. The answer is `{table, schema, valid, columns}`, each column with
/// `column_name`, `total`, `rejects` and `sample_message`; `valid` is true when
/// no row of any column is rejected.
pub(crate) fn validate_table(dir: &Path, table: &str, schema: &Path) -> Result<Value> {
    let outside = || {
        refusal(format!(
            "{} is outside the Protocol folder {}; name a file inside it",
            schema.display(),
            dir.display()
        ))
    };
    if !crate::protocol_files::stays_inside_by_name(schema) {
        return Err(outside());
    }
    let schema_path = dir.join(schema);
    if !crate::protocol_files::resolves_inside(dir, &schema_path)? {
        return Err(outside());
    }
    if !schema_path.is_file() {
        return Err(refusal(format!(
            "no schema file at {}; write one with `arc protocol write-file` or `arc table profile --format json-schema`",
            schema_path.display()
        )));
    }
    let ext = crate::operator::resolve_finetype_ext(None).map_err(|(_, m)| refusal(m))?;

    // The read-only open finds the table and refuses a database that is missing or
    // held by another process; it is closed before the database is attached again.
    let (conn, path) = open_read_only(dir)?;
    let (found_schema, found_name) = locate_table(&conn, table, &path)?;
    drop(conn);

    let conn = crate::operator::open_finetype_connection(&ext).map_err(refusal)?;
    // As `open_read_only` does: a view over a remote file must not fetch an extension.
    conn.execute_batch("SET autoinstall_known_extensions = false")
        .map_err(db_error)?;
    conn.execute_batch(&format!(
        "ATTACH '{}' AS arc_protocol (READ_ONLY); \
         CREATE TEMP VIEW _arc_table AS SELECT * FROM arc_protocol.{}.{};",
        path.display().to_string().replace('\'', "''"),
        quote(&found_schema),
        quote(&found_name)
    ))
    .map_err(|e| {
        if is_lock_error(&e.to_string()) {
            lock_refusal(&path)
        } else {
            db_error(e)
        }
    })?;
    let report =
        crate::operator::ft_validate_report(&conn, "_arc_table", &schema_path).map_err(refusal)?;

    let valid = report.iter().all(|c| c.rejects == 0);
    let columns: Vec<Value> = report
        .into_iter()
        .map(|c| {
            json!({
                "column_name": c.column,
                "total": c.total,
                "rejects": c.rejects,
                "sample_message": c.message,
            })
        })
        .collect();
    Ok(json!({
        "table": table,
        "schema": schema.display().to_string(),
        "valid": valid,
        "columns": columns,
    }))
}
