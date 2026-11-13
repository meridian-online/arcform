//! Writing a Protocol's tables into a local DuckLake catalogue file as one upserted
//! snapshot, with each table's descriptor in the reserved `meta` schema.
//!
//! The catalogue is attached with its data path overridden to the lake's write
//! path: the same objects the persisted data path names, addressed so DuckDB can
//! read and write them (`r2://<bucket>/<prefix>/`, or `s3://`, live; a local folder otherwise).
//! DuckLake reads the files already in the lake there and writes new ones in place,
//! recorded relative to the persisted data path. Uploading the catalogue is the
//! caller's job.
//!
//! An unchanged commit makes no write at all, so DuckDB commits no snapshot: every
//! statement that could write is issued only once a comparison has shown it changes
//! something, and the commit message is set only when one did.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::meta::{META_SCHEMA, TableDescriptor, failed};
use crate::error::{Error, Result};
use crate::operator::sql_string_literal;

/// One table to write.
#[derive(Debug, Clone)]
pub struct TableInput {
    /// `schema.table`.
    pub table: String,
    pub parquet: PathBuf,
    pub descriptor: TableDescriptor,
    /// The columns rows are matched on. Empty: the table is replaced whole when it
    /// differs.
    pub key: Vec<String>,
    /// Drop and recreate the table when its columns changed other than by a column
    /// added at the end, instead of failing.
    pub replace_on_schema_change: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TableAction {
    /// The table was created, or its rows, columns, descriptor or comments changed.
    Registered,
    /// Nothing about the table changed.
    Unchanged,
    /// The table's rows were replaced whole: it was dropped and recreated after a
    /// schema change, or it has no key and its rows differed.
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TableChange {
    pub table: String,
    pub action: TableAction,
    pub inserted: u64,
    pub updated: u64,
    pub deleted: u64,
    /// The table's rows after the commit.
    pub rows: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommitOutcome {
    /// The catalogue's newest snapshot after the commit: this commit's when
    /// `committed`, the one already there otherwise.
    pub snapshot_id: i64,
    pub committed: bool,
    pub tables: Vec<TableChange>,
    /// The data and delete files this snapshot added, relative to the data path.
    pub new_files: Vec<String>,
}

/// The alias the catalogue is attached under, inside this commit's own in-memory
/// DuckDB.
const LAKE: &str = "arc_lake";
/// The catalogue's own metadata tables, which DuckLake attaches beside it.
const LAKE_METADATA: &str = "__ducklake_metadata_arc_lake";

fn db(context: &str) -> impl Fn(duckdb::Error) -> Error + '_ {
    move |e| failed(format!("{context}: {e}"))
}

/// A DuckDB double-quoted identifier.
fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// One table of the call, resolved.
struct Target<'a> {
    input: &'a TableInput,
    schema: &'a str,
    name: &'a str,
    /// `schema.table`, as `meta` names it.
    full: String,
    /// The temp table holding the input's rows.
    source: String,
}

impl Target<'_> {
    fn lake_table(&self) -> String {
        format!("{LAKE}.{}.{}", ident(self.schema), ident(self.name))
    }
}

/// `schema.table` → its parts; one part is a table in `main`.
fn split_table(table: &str) -> Option<(&str, &str)> {
    let (schema, name) = table.split_once('.').unwrap_or(("main", table));
    let valid = |part: &str| !part.is_empty() && !part.contains('.');
    (valid(schema) && valid(name)).then_some((schema, name))
}

fn targets(tables: &[TableInput]) -> Result<Vec<Target<'_>>> {
    let mut out: Vec<Target<'_>> = Vec::with_capacity(tables.len());
    for (i, input) in tables.iter().enumerate() {
        let (schema, name) = split_table(&input.table).ok_or_else(|| {
            failed(format!(
                "table '{}' must be `table` or `schema.table`",
                input.table
            ))
        })?;
        if schema.eq_ignore_ascii_case(META_SCHEMA) {
            return Err(failed(format!(
                "table '{}' is in the schema '{META_SCHEMA}', which is reserved for the \
                 lake's descriptors",
                input.table
            )));
        }
        let full = format!("{schema}.{name}");
        if out.iter().any(|t| t.full.eq_ignore_ascii_case(&full)) {
            return Err(failed(format!("table '{full}' is listed more than once")));
        }
        out.push(Target {
            input,
            schema,
            name,
            full,
            source: format!("temp.arc_source_{i}"),
        });
    }
    Ok(out)
}

/// The newest snapshot id.
fn current_snapshot(conn: &duckdb::Connection) -> Result<i64> {
    conn.query_row(
        &format!("SELECT max(snapshot_id) FROM ducklake_snapshots('{LAKE}')"),
        [],
        |r| r.get(0),
    )
    .map_err(db("read the catalogue's snapshots"))
}

/// A table's columns in order, with their DuckDB types.
fn columns(
    conn: &duckdb::Connection,
    database: &str,
    schema: &str,
    table: &str,
) -> Result<Vec<(String, String)>> {
    conn.prepare(
        "SELECT column_name, data_type FROM duckdb_columns() \
         WHERE database_name = ? AND schema_name = ? AND table_name = ? ORDER BY column_index",
    )
    .and_then(|mut stmt| {
        stmt.query_map([database, schema, table], |r| Ok((r.get(0)?, r.get(1)?)))
            .and_then(|rows| rows.collect())
    })
    .map_err(db(&format!("read the columns of {schema}.{table}")))
}

fn count(conn: &duckdb::Connection, sql: &str, context: &str) -> Result<u64> {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0))
        .map(|n| u64::try_from(n).unwrap_or(0))
        .map_err(db(context))
}

fn exec(conn: &duckdb::Connection, sql: &str, context: &str) -> Result<u64> {
    conn.execute(sql, []).map(|n| n as u64).map_err(db(context))
}

/// Load each input into a temp table and refuse a key that is missing, NULL or
/// repeated. Runs before the transaction, so a refusal commits nothing.
fn stage_sources(conn: &duckdb::Connection, targets: &[Target<'_>]) -> Result<()> {
    for t in targets {
        let file = sql_string_literal(&t.input.parquet.display().to_string());
        conn.execute_batch(&format!(
            "CREATE TEMP TABLE {} AS FROM read_parquet({file});",
            t.source
        ))
        .map_err(db(&format!(
            "read {} for table '{}'",
            t.input.parquet.display(),
            t.full
        )))?;
        if t.input.key.is_empty() {
            continue;
        }
        let names: Vec<String> = columns(conn, "temp", "main", strip_temp(&t.source))?
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        if let Some(missing) = t.input.key.iter().find(|k| !names.contains(k)) {
            return Err(failed(format!(
                "the key of table '{}' names '{missing}', which its Parquet file lacks",
                t.full
            )));
        }
        for k in &t.input.key {
            let nulls = count(
                conn,
                &format!(
                    "SELECT count(*) FROM {} WHERE {} IS NULL",
                    t.source,
                    ident(k)
                ),
                "check the key for NULLs",
            )?;
            if nulls > 0 {
                return Err(failed(format!(
                    "table '{}' has {nulls} row(s) whose key column '{k}' is NULL; \
                     a key value must be present",
                    t.full
                )));
            }
        }
        let keys = t
            .input
            .key
            .iter()
            .map(|k| ident(k))
            .collect::<Vec<_>>()
            .join(", ");
        let shown = t
            .input
            .key
            .iter()
            .map(|k| format!("{}::VARCHAR", ident(k)))
            .collect::<Vec<_>>()
            .join(", ");
        let duplicate: Option<(String, i64)> = conn
            .query_row(
                &format!(
                    "SELECT concat_ws(', ', {shown}), count(*) FROM {} GROUP BY {keys} \
                     HAVING count(*) > 1 ORDER BY 1 LIMIT 1",
                    t.source
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                duckdb::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })
            .map_err(db("check the key for duplicates"))?;
        if let Some((value, n)) = duplicate {
            return Err(failed(format!(
                "table '{}' has duplicate key values: key ({}) = ({value}) appears in {n} rows",
                t.full,
                t.input.key.join(", ")
            )));
        }
    }
    Ok(())
}

fn strip_temp(source: &str) -> &str {
    source.strip_prefix("temp.").unwrap_or(source)
}

/// Stage every table's `meta` rows in temp tables, so they can be compared with what
/// the catalogue holds before anything is written.
fn stage_meta(conn: &duckdb::Connection, targets: &[Target<'_>]) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE arc_new_descriptors ({DESCRIPTORS_COLUMNS}); \
         CREATE TEMP TABLE arc_new_fields ({FIELDS_COLUMNS});"
    ))
    .map_err(db("stage the descriptors"))?;
    let mut descriptors = conn
        .prepare("INSERT INTO temp.arc_new_descriptors VALUES (?, ?::JSON)")
        .map_err(db("stage the descriptors"))?;
    let mut fields = conn
        .prepare("INSERT INTO temp.arc_new_fields VALUES (?, ?, ?, ?, ?, ?, ?::JSON, ?)")
        .map_err(db("stage the descriptors"))?;
    for t in targets {
        let d = &t.input.descriptor;
        descriptors
            .execute(duckdb::params![t.full, d.descriptor.to_string()])
            .map_err(db("stage the descriptors"))?;
        for f in &d.fields {
            fields
                .execute(duckdb::params![
                    t.full,
                    f.column_name,
                    f.ordinal,
                    f.r#type,
                    f.finetype_label,
                    f.description,
                    f.constraints.to_string(),
                    t.input.key.contains(&f.column_name),
                ])
                .map_err(db("stage the descriptors"))?;
        }
    }
    Ok(())
}

const DESCRIPTORS_COLUMNS: &str = "table_name VARCHAR, descriptor JSON";
const FIELDS_COLUMNS: &str = "table_name VARCHAR, column_name VARCHAR, ordinal INTEGER, \
     type VARCHAR, finetype_label VARCHAR, description VARCHAR, constraints JSON, \
     is_key BOOLEAN";

fn table_exists(conn: &duckdb::Connection, schema: &str, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_tables() \
         WHERE database_name = ? AND schema_name = ? AND table_name = ?",
        [LAKE, schema, table],
        |r| r.get(0),
    )
    .map_err(db("read the catalogue's tables"))
}

fn schema_exists(conn: &duckdb::Connection, schema: &str) -> Result<bool> {
    conn.query_row(
        "SELECT count(*) > 0 FROM duckdb_schemas() WHERE database_name = ? AND schema_name = ?",
        [LAKE, schema],
        |r| r.get(0),
    )
    .map_err(db("read the catalogue's schemas"))
}

/// Create a schema only when it is missing: a write, so never issued needlessly.
fn ensure_schema(conn: &duckdb::Connection, schema: &str) -> Result<bool> {
    if schema_exists(conn, schema)? {
        return Ok(false);
    }
    exec(
        conn,
        &format!("CREATE SCHEMA {LAKE}.{}", ident(schema)),
        &format!("create schema {schema}"),
    )
    .map(|_| true)
}

/// Create `meta` and its tables where missing. Whether anything was created.
fn ensure_meta(conn: &duckdb::Connection) -> Result<bool> {
    let mut created = ensure_schema(conn, META_SCHEMA)?;
    for (table, columns) in [
        ("descriptors", DESCRIPTORS_COLUMNS),
        ("fields", FIELDS_COLUMNS),
    ] {
        if !table_exists(conn, META_SCHEMA, table)? {
            exec(
                conn,
                &format!("CREATE TABLE {LAKE}.{META_SCHEMA}.{table} ({columns})"),
                &format!("create {META_SCHEMA}.{table}"),
            )?;
            created = true;
        }
    }
    Ok(created)
}

/// The key `meta.fields` records for a table, or `None` when it records none.
fn stored_key(conn: &duckdb::Connection, table: &str) -> Result<Option<Vec<String>>> {
    let rows: Vec<(String, bool)> = conn
        .prepare(&format!(
            "SELECT column_name, is_key FROM {LAKE}.{META_SCHEMA}.fields \
             WHERE table_name = ? ORDER BY ordinal"
        ))
        .and_then(|mut stmt| {
            stmt.query_map([table], |r| Ok((r.get(0)?, r.get(1)?)))
                .and_then(|rows| rows.collect())
        })
        .map_err(db("read the stored key"))?;
    Ok((!rows.is_empty()).then(|| {
        rows.into_iter()
            .filter(|(_, is_key)| *is_key)
            .map(|(name, _)| name)
            .collect()
    }))
}

/// How the input's columns differ from the table's, in words, or `None` when the
/// input only adds columns at the end.
fn schema_difference(
    existing: &[(String, String)],
    input: &[(String, String)],
    stored_key: Option<&[String]>,
    key: &[String],
) -> Option<String> {
    let mut found = Vec::new();
    for (name, ty) in existing {
        match input.iter().find(|(n, _)| n == name) {
            None => found.push(format!("column {} was dropped", ident(name))),
            Some((_, new)) if new != ty => {
                found.push(format!("column {} was {ty}, is now {new}", ident(name)))
            }
            Some(_) => {}
        }
    }
    let prefix = input.len() >= existing.len() && input[..existing.len()] == *existing;
    if found.is_empty() && !prefix {
        for (name, _) in input.iter().take(existing.len()) {
            if !existing.iter().any(|(n, _)| n == name) {
                found.push(format!(
                    "column {} was added before the last existing column",
                    ident(name)
                ));
            }
        }
        if found.is_empty() {
            found.push("the columns were reordered".to_string());
        }
    }
    // Which columns form the key, not the order the descriptor lists them in.
    let sorted = |k: &[String]| {
        let mut k = k.to_vec();
        k.sort();
        k
    };
    if let Some(stored) = stored_key
        && sorted(stored) != sorted(key)
    {
        found.push(format!(
            "the key was ({}), is now ({})",
            stored.join(", "),
            key.join(", ")
        ));
    }
    (!found.is_empty()).then(|| found.join("; "))
}

/// Write one table's rows: create, alter, replace or upsert. Whether it wrote
/// anything, and its change.
fn write_rows(conn: &duckdb::Connection, t: &Target<'_>) -> Result<(bool, TableChange)> {
    let lake_table = t.lake_table();
    let source = &t.source;
    let rows = count(
        conn,
        &format!("SELECT count(*) FROM {source}"),
        "count the input's rows",
    )?;
    let mut change = TableChange {
        table: t.full.clone(),
        action: TableAction::Unchanged,
        inserted: 0,
        updated: 0,
        deleted: 0,
        rows,
    };
    let context = format!("write table '{}'", t.full);
    let create = format!("CREATE TABLE {lake_table} AS FROM {source}");

    let mut wrote = ensure_schema(conn, t.schema)?;
    if !table_exists(conn, t.schema, t.name)? {
        exec(conn, &create, &context)?;
        change.action = TableAction::Registered;
        change.inserted = rows;
        return Ok((true, change));
    }

    let existing = columns(conn, LAKE, t.schema, t.name)?;
    let input = columns(conn, "temp", "main", strip_temp(source))?;
    let stored = stored_key(conn, &t.full)?;
    if let Some(difference) = schema_difference(&existing, &input, stored.as_deref(), &t.input.key)
    {
        if !t.input.replace_on_schema_change {
            return Err(failed(format!(
                "table '{}' changed shape: {difference}. Set `on_schema_change: replace` \
                 to drop and recreate it",
                t.full
            )));
        }
        change.deleted = count(
            conn,
            &format!("SELECT count(*) FROM {lake_table}"),
            &context,
        )?;
        exec(conn, &format!("DROP TABLE {lake_table}"), &context)?;
        exec(conn, &create, &context)?;
        change.action = TableAction::Replaced;
        change.inserted = rows;
        return Ok((true, change));
    }
    for (name, ty) in &input[existing.len()..] {
        exec(
            conn,
            &format!("ALTER TABLE {lake_table} ADD COLUMN {} {ty}", ident(name)),
            &context,
        )?;
        wrote = true;
    }

    if t.input.key.is_empty() {
        let differing = count(
            conn,
            &format!(
                "SELECT (SELECT count(*) FROM (FROM {lake_table} EXCEPT ALL FROM {source})) \
                 + (SELECT count(*) FROM (FROM {source} EXCEPT ALL FROM {lake_table}))"
            ),
            &context,
        )?;
        if differing > 0 {
            change.deleted = exec(conn, &format!("DELETE FROM {lake_table}"), &context)?;
            change.inserted = exec(
                conn,
                &format!("INSERT INTO {lake_table} BY NAME FROM {source}"),
                &context,
            )?;
            change.action = TableAction::Replaced;
            return Ok((true, change));
        }
    } else {
        let on = t
            .input
            .key
            .iter()
            .map(|k| format!("tgt.{0} = src.{0}", ident(k)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let values: Vec<&str> = input
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| !t.input.key.iter().any(|k| k == n))
            .collect();
        if !values.is_empty() {
            let set = values
                .iter()
                .map(|n| format!("{0} = src.{0}", ident(n)))
                .collect::<Vec<_>>()
                .join(", ");
            let differs = values
                .iter()
                .map(|n| format!("tgt.{0} IS DISTINCT FROM src.{0}", ident(n)))
                .collect::<Vec<_>>()
                .join(" OR ");
            change.updated = exec(
                conn,
                &format!(
                    "UPDATE {lake_table} AS tgt SET {set} FROM {source} AS src \
                     WHERE {on} AND ({differs})"
                ),
                &context,
            )?;
        }
        change.deleted = exec(
            conn,
            &format!(
                "DELETE FROM {lake_table} AS tgt \
                 WHERE NOT EXISTS (SELECT 1 FROM {source} AS src WHERE {on})"
            ),
            &context,
        )?;
        change.inserted = exec(
            conn,
            &format!(
                "INSERT INTO {lake_table} BY NAME SELECT * FROM {source} AS src \
                 WHERE NOT EXISTS (SELECT 1 FROM {lake_table} AS tgt WHERE {on})"
            ),
            &context,
        )?;
    }
    if wrote || change.updated + change.deleted + change.inserted > 0 {
        change.action = TableAction::Registered;
        wrote = true;
    }
    Ok((wrote, change))
}

/// Replace a table's rows in `meta.descriptors` and `meta.fields` where they differ
/// from the staged ones. Whether it wrote anything.
fn write_meta(conn: &duckdb::Connection, table: &str) -> Result<bool> {
    let context = format!("write the descriptor of table '{table}'");
    let mut wrote = false;
    for (meta, compare) in [
        ("descriptors", "s.descriptor IS DISTINCT FROM n.descriptor"),
        (
            "fields",
            "s.ordinal IS DISTINCT FROM n.ordinal OR s.type IS DISTINCT FROM n.type \
             OR s.finetype_label IS DISTINCT FROM n.finetype_label \
             OR s.description IS DISTINCT FROM n.description \
             OR s.constraints IS DISTINCT FROM n.constraints \
             OR s.is_key IS DISTINCT FROM n.is_key",
        ),
    ] {
        let join = if meta == "fields" {
            "s.column_name = n.column_name"
        } else {
            "true"
        };
        let differs: bool = conn
            .query_row(
                &format!(
                    "SELECT count(*) > 0 FROM \
                     (SELECT * FROM {LAKE}.{META_SCHEMA}.{meta} WHERE table_name = $1) AS s \
                     FULL JOIN (SELECT * FROM temp.arc_new_{meta} WHERE table_name = $1) AS n \
                     ON {join} \
                     WHERE s.table_name IS NULL OR n.table_name IS NULL OR {compare}"
                ),
                [table],
                |r| r.get(0),
            )
            .map_err(db(&context))?;
        if differs {
            conn.execute(
                &format!("DELETE FROM {LAKE}.{META_SCHEMA}.{meta} WHERE table_name = ?"),
                [table],
            )
            .and_then(|_| {
                conn.execute(
                    &format!(
                        "INSERT INTO {LAKE}.{META_SCHEMA}.{meta} \
                         SELECT * FROM temp.arc_new_{meta} WHERE table_name = ?"
                    ),
                    [table],
                )
            })
            .map_err(db(&context))?;
            wrote = true;
        }
    }
    Ok(wrote)
}

/// Set the table's and its columns' comments from the descriptor where the text
/// differs. `COMMENT ON` commits a snapshot even when it sets the text already there,
/// so each is compared first. Whether it wrote anything.
fn write_comments(conn: &duckdb::Connection, t: &Target<'_>) -> Result<bool> {
    let context = format!("comment on table '{}'", t.full);
    let comment_sql = |text: Option<&str>| text.map_or("NULL".to_string(), sql_string_literal);
    let mut wrote = false;
    let current: Option<String> = conn
        .query_row(
            "SELECT comment FROM duckdb_tables() \
             WHERE database_name = ? AND schema_name = ? AND table_name = ?",
            [LAKE, t.schema, t.name],
            |r| r.get(0),
        )
        .map_err(db(&context))?;
    let wanted = t.input.descriptor.table_comment.as_deref();
    if current.as_deref() != wanted {
        exec(
            conn,
            &format!(
                "COMMENT ON TABLE {} IS {}",
                t.lake_table(),
                comment_sql(wanted)
            ),
            &context,
        )?;
        wrote = true;
    }
    let current: Vec<(String, Option<String>)> = conn
        .prepare(
            "SELECT column_name, comment FROM duckdb_columns() \
             WHERE database_name = ? AND schema_name = ? AND table_name = ?",
        )
        .and_then(|mut stmt| {
            stmt.query_map([LAKE, t.schema, t.name], |r| Ok((r.get(0)?, r.get(1)?)))
                .and_then(|rows| rows.collect())
        })
        .map_err(db(&context))?;
    for (column, comment) in current {
        let wanted = t
            .input
            .descriptor
            .fields
            .iter()
            .find(|f| f.column_name == column)
            .and_then(|f| f.description.as_deref());
        if comment.as_deref() != wanted {
            exec(
                conn,
                &format!(
                    "COMMENT ON COLUMN {}.{} IS {}",
                    t.lake_table(),
                    ident(&column),
                    comment_sql(wanted)
                ),
                &context,
            )?;
            wrote = true;
        }
    }
    Ok(wrote)
}

/// The data and delete files `snapshot` added, relative to the data path.
fn new_files(conn: &duckdb::Connection, snapshot: i64) -> Result<Vec<String>> {
    let live = |alias: &str| {
        format!(
            "{alias}.begin_snapshot <= f.begin_snapshot \
             AND ({alias}.end_snapshot IS NULL OR {alias}.end_snapshot > f.begin_snapshot)"
        )
    };
    let files = |table: &str| {
        format!(
            "SELECT s.path || t.path || f.path, \
             s.path_is_relative AND t.path_is_relative AND f.path_is_relative \
             FROM {LAKE_METADATA}.{table} AS f \
             JOIN {LAKE_METADATA}.ducklake_table AS t ON t.table_id = f.table_id AND {} \
             JOIN {LAKE_METADATA}.ducklake_schema AS s ON s.schema_id = t.schema_id AND {} \
             WHERE f.begin_snapshot = $1",
            live("t"),
            live("s"),
        )
    };
    let rows: Vec<(String, bool)> = conn
        .prepare(&format!(
            "{} UNION ALL {} ORDER BY 1",
            files("ducklake_data_file"),
            files("ducklake_delete_file")
        ))
        .and_then(|mut stmt| {
            stmt.query_map([snapshot], |r| Ok((r.get(0)?, r.get(1)?)))
                .and_then(|rows| rows.collect())
        })
        .map_err(db("read the files the snapshot added"))?;
    rows.into_iter()
        .map(|(path, relative)| {
            if relative {
                Ok(path)
            } else {
                Err(failed(format!(
                    "the snapshot added '{path}', which is not under the data path"
                )))
            }
        })
        .collect()
}

/// Write `tables` into the DuckLake catalogue at `catalog_file` as one snapshot,
/// creating the catalogue with `data_path` as its persisted data path if the file is
/// absent. Data files are read and written under `write_path`, a local folder or an
/// `r2://` or `s3://` URL naming the same objects as `data_path`. `secret_sql`,
/// when given, runs before the attach (the caller's `CREATE TEMPORARY SECRET`); it
/// never appears in an error. An unchanged commit makes no write and commits no
/// snapshot.
pub fn commit(
    catalog_file: &Path,
    data_path: &str,
    write_path: &str,
    secret_sql: Option<&str>,
    tables: &[TableInput],
    commit_info: &serde_json::Value,
) -> Result<CommitOutcome> {
    let targets = targets(tables)?;
    let mut conn = duckdb::Connection::open_in_memory().map_err(db("open duckdb"))?;
    conn.execute_batch("INSTALL ducklake; LOAD ducklake;")
        .map_err(db("load ducklake"))?;
    let remote = write_path.contains("://");
    if remote {
        conn.execute_batch("INSTALL httpfs; LOAD httpfs;")
            .map_err(db("load httpfs"))?;
    }
    if let Some(secret) = secret_sql {
        // The statement holds the credential: its error is not passed on.
        conn.execute_batch(secret)
            .map_err(|_| failed("creating the object-store secret failed"))?;
    }
    let catalog = sql_string_literal(&format!("ducklake:{}", catalog_file.display()));
    if !catalog_file.exists() {
        conn.execute_batch(&format!(
            "ATTACH {catalog} AS {LAKE} (DATA_PATH {}); DETACH {LAKE};",
            sql_string_literal(data_path)
        ))
        .map_err(db(&format!(
            "create the catalogue {}",
            catalog_file.display()
        )))?;
    }
    if !remote {
        std::fs::create_dir_all(write_path)
            .map_err(|e| failed(format!("create {write_path}: {e}")))?;
    }
    let mut write_path = write_path.to_string();
    if !write_path.ends_with('/') {
        write_path.push('/');
    }
    conn.execute_batch(&format!(
        "ATTACH {catalog} AS {LAKE} (DATA_PATH {}, OVERRIDE_DATA_PATH true);",
        sql_string_literal(&write_path)
    ))
    .map_err(db(&format!(
        "attach the catalogue {}",
        catalog_file.display()
    )))?;
    let before = current_snapshot(&conn)?;

    stage_sources(&conn, &targets)?;
    stage_meta(&conn, &targets)?;

    let tx = conn
        .transaction()
        .map_err(db("begin the commit's transaction"))?;
    let mut wrote = ensure_meta(&tx)?;
    let mut changes = Vec::with_capacity(targets.len());
    for t in &targets {
        let (rows_written, mut change) = write_rows(&tx, t)?;
        let meta_written = write_meta(&tx, &t.full)?;
        let comments_written = write_comments(&tx, t)?;
        if (meta_written || comments_written) && change.action == TableAction::Unchanged {
            change.action = TableAction::Registered;
        }
        wrote |= rows_written || meta_written || comments_written;
        changes.push(change);
    }
    if wrote {
        let message = format!(
            "ducklake_write: {}",
            targets
                .iter()
                .map(|t| t.full.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        tx.execute_batch(&format!(
            "CALL {LAKE}.set_commit_message('arcform', {}, extra_info => {});",
            sql_string_literal(&message),
            sql_string_literal(&commit_info.to_string())
        ))
        .map_err(db("set the commit message"))?;
    }
    tx.commit().map_err(db("commit"))?;

    let snapshot_id = current_snapshot(&conn)?;
    let committed = snapshot_id != before;
    let new_files = if committed {
        new_files(&conn, snapshot_id)?
    } else {
        Vec::new()
    };
    Ok(CommitOutcome {
        snapshot_id,
        committed,
        tables: changes,
        new_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lake::meta::table_descriptor;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    const DATA_PATH: &str = "https://example.invalid/lake/";

    struct Lake {
        dir: TempDir,
    }

    impl Lake {
        fn new() -> Self {
            Lake {
                dir: TempDir::new().unwrap(),
            }
        }

        fn catalog(&self) -> PathBuf {
            self.dir.path().join("lake.ducklake")
        }

        fn files(&self) -> PathBuf {
            self.dir.path().join("files")
        }

        /// Write `select` to a Parquet file named `name` in the test's folder.
        fn parquet(&self, name: &str, select: &str) -> PathBuf {
            let path = self.dir.path().join("in").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "COPY ({select}) TO {} (FORMAT parquet);",
                sql_string_literal(&path.display().to_string())
            ))
            .unwrap();
            path
        }

        fn commit(&self, tables: &[TableInput]) -> Result<CommitOutcome> {
            self.commit_with(tables, &json!({"run": "r1"}))
        }

        fn commit_with(&self, tables: &[TableInput], info: &Value) -> Result<CommitOutcome> {
            commit(
                &self.catalog(),
                DATA_PATH,
                &format!("{}/", self.files().display()),
                None,
                tables,
                info,
            )
        }

        /// Read the catalogue through a connection of the test's own, reading data
        /// files from the write path, closed before the next commit opens it.
        fn read<T>(&self, f: impl FnOnce(&duckdb::Connection) -> T) -> T {
            let conn = duckdb::Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!(
                "LOAD ducklake; ATTACH {} AS lake (DATA_PATH {}, OVERRIDE_DATA_PATH true);",
                sql_string_literal(&format!("ducklake:{}", self.catalog().display())),
                sql_string_literal(&format!("{}/", self.files().display())),
            ))
            .unwrap();
            f(&conn)
        }

        fn strings(&self, sql: &str) -> Vec<String> {
            self.read(|c| {
                let mut stmt = c.prepare(sql).unwrap();
                stmt.query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .map(|r| r.unwrap())
                    .collect()
            })
        }

        fn count(&self, sql: &str) -> i64 {
            self.read(|c| c.query_row(sql, [], |r| r.get(0)).unwrap())
        }

        fn snapshots(&self) -> i64 {
            self.count("SELECT count(*) FROM ducklake_snapshots('lake')")
        }
    }

    /// A descriptor for `table` with `columns` (name, Frictionless type), keyed on
    /// `key`, through Task 2's reader.
    fn descriptor(table: &str, columns: &[(&str, &str)], key: &[&str]) -> TableDescriptor {
        let fields: Vec<Value> = columns
            .iter()
            .map(|(name, ty)| json!({"name": name, "type": ty, "description": format!("the {name}")}))
            .collect();
        let package = json!({
            "name": table,
            "description": format!("all about {table}"),
            "resources": [{
                "name": table,
                "format": "parquet",
                "schema": {"fields": fields, "primaryKey": key},
            }],
        });
        let names: Vec<String> = columns.iter().map(|(n, _)| n.to_string()).collect();
        table_descriptor(table, &package, &names).unwrap()
    }

    fn input(table: &str, parquet: PathBuf, descriptor: TableDescriptor) -> TableInput {
        TableInput {
            table: table.to_string(),
            parquet,
            key: descriptor.key.clone(),
            descriptor,
            replace_on_schema_change: false,
        }
    }

    fn id_v(table: &str) -> TableDescriptor {
        descriptor(table, &[("id", "integer"), ("v", "string")], &["id"])
    }

    /// Twenty rows `id 0..20`, `v = 'v<id>'`.
    const TWENTY: &str = "SELECT range AS id, 'v' || range AS v FROM range(20)";

    fn change<'a>(outcome: &'a CommitOutcome, table: &str) -> &'a TableChange {
        outcome.tables.iter().find(|t| t.table == table).unwrap()
    }

    #[test]
    fn a_first_commit_creates_the_tables_and_meta() {
        let lake = Lake::new();
        let a = lake.parquet("a.parquet", TWENTY);
        let b = lake.parquet("b.parquet", "SELECT range AS id, 'w' AS v FROM range(3)");
        let out = lake
            .commit(&[input("s.a", a, id_v("s.a")), input("s.b", b, id_v("s.b"))])
            .unwrap();
        assert!(out.committed);
        assert_eq!(
            lake.snapshots(),
            2,
            "the catalogue's first snapshot and ours"
        );
        assert_eq!(
            out.snapshot_id,
            lake.count("SELECT max(snapshot_id) FROM ducklake_snapshots('lake')")
        );
        assert_eq!(lake.count("SELECT count(*) FROM lake.s.a"), 20);
        assert_eq!(lake.count("SELECT count(*) FROM lake.s.b"), 3);
        let a = change(&out, "s.a");
        assert_eq!(a.action, TableAction::Registered);
        assert_eq!((a.inserted, a.rows), (20, 20));
        assert_eq!(
            lake.strings(
                "SELECT column_name FROM lake.meta.fields WHERE table_name = 's.a' ORDER BY ordinal"
            ),
            ["id", "v"]
        );
        assert_eq!(lake.count("SELECT count(*) FROM lake.meta.fields"), 4);
        assert_eq!(
            lake.count("SELECT count(*) FROM lake.meta.fields WHERE table_name = 's.a' AND is_key"),
            1
        );
        assert_eq!(
            lake.strings("SELECT table_name FROM lake.meta.descriptors ORDER BY 1"),
            ["s.a", "s.b"]
        );
        assert_eq!(
            lake.strings(
                "SELECT comment FROM duckdb_tables() WHERE database_name = 'lake' \
                 AND schema_name = 's' AND table_name = 'a'"
            ),
            ["all about s.a"]
        );
        assert_eq!(
            lake.strings(
                "SELECT comment FROM duckdb_columns() WHERE database_name = 'lake' \
                 AND schema_name = 's' AND table_name = 'a' AND column_name = 'v'"
            ),
            ["the v"]
        );
    }

    #[test]
    fn a_rebuild_with_three_rows_changed_records_only_those() {
        let lake = Lake::new();
        let first = lake.parquet("1.parquet", TWENTY);
        lake.commit(&[input("s.t", first, id_v("s.t"))]).unwrap();
        let before = lake.count("SELECT max(snapshot_id) FROM ducklake_snapshots('lake')");
        // 5, 6, 7 changed; 19 gone; 20 new.
        let second = lake.parquet(
            "2.parquet",
            "SELECT range AS id, CASE WHEN range IN (5, 6, 7) THEN 'changed' ELSE 'v' || range END AS v \
             FROM range(21) WHERE range <> 19",
        );
        let out = lake.commit(&[input("s.t", second, id_v("s.t"))]).unwrap();
        assert!(out.committed);
        let t = change(&out, "s.t");
        assert_eq!(
            (t.action, t.updated, t.deleted, t.inserted, t.rows),
            (TableAction::Registered, 3, 1, 1, 20)
        );
        assert_eq!(
            out.snapshot_id,
            before + 1,
            "one snapshot for the whole commit"
        );
        let changes = |kind: &str| {
            lake.count(&format!(
                "SELECT count(*) FROM ducklake_table_changes('lake', 's', 't', {0}, {0}) \
                 WHERE change_type = '{kind}'",
                out.snapshot_id
            ))
        };
        assert_eq!(changes("update_preimage"), 3);
        assert_eq!(changes("update_postimage"), 3);
        assert_eq!(changes("delete"), 1);
        assert_eq!(changes("insert"), 1);
    }

    #[test]
    fn an_unchanged_rebuild_commits_nothing() {
        let lake = Lake::new();
        let p = lake.parquet("t.parquet", TWENTY);
        let first = lake
            .commit(&[input("s.t", p.clone(), id_v("s.t"))])
            .unwrap();
        let snapshots = lake.snapshots();
        let out = lake.commit(&[input("s.t", p, id_v("s.t"))]).unwrap();
        assert!(!out.committed);
        assert_eq!(out.snapshot_id, first.snapshot_id);
        assert_eq!(lake.snapshots(), snapshots);
        assert!(out.new_files.is_empty());
        let t = change(&out, "s.t");
        assert_eq!(
            (t.action, t.inserted, t.updated, t.deleted, t.rows),
            (TableAction::Unchanged, 0, 0, 0, 20)
        );
    }

    #[test]
    fn a_new_run_with_the_same_data_and_descriptor_commits_nothing() {
        let lake = Lake::new();
        let p = lake.parquet("t.parquet", TWENTY);
        lake.commit_with(
            &[input("s.t", p.clone(), id_v("s.t"))],
            &json!({"run": "one"}),
        )
        .unwrap();
        let out = lake
            .commit_with(&[input("s.t", p, id_v("s.t"))], &json!({"run": "two"}))
            .unwrap();
        assert!(!out.committed);
    }

    #[test]
    fn null_values_compare_as_values() {
        let lake = Lake::new();
        let first = lake.parquet(
            "1.parquet",
            "SELECT range AS id, NULL::VARCHAR AS v FROM range(3)",
        );
        lake.commit(&[input("s.t", first.clone(), id_v("s.t"))])
            .unwrap();
        let same = lake.commit(&[input("s.t", first, id_v("s.t"))]).unwrap();
        assert!(!same.committed, "NULL to NULL is no change");
        let second = lake.parquet(
            "2.parquet",
            "SELECT range AS id, CASE WHEN range = 1 THEN 'x' END AS v FROM range(3)",
        );
        let out = lake.commit(&[input("s.t", second, id_v("s.t"))]).unwrap();
        assert!(out.committed);
        assert_eq!(change(&out, "s.t").updated, 1);
    }

    #[test]
    fn quoted_column_names_publish_and_upsert() {
        let lake = Lake::new();
        let d = || {
            descriptor(
                "s.q",
                &[
                    ("order", "integer"),
                    ("Code Display", "string"),
                    ("MixedCase", "string"),
                ],
                &["order"],
            )
        };
        let first = lake.parquet(
            "1.parquet",
            "SELECT range AS \"order\", 'c' || range AS \"Code Display\", 'm' AS \"MixedCase\" \
             FROM range(5)",
        );
        lake.commit(&[input("s.q", first, d())]).unwrap();
        let second = lake.parquet(
            "2.parquet",
            "SELECT range AS \"order\", 'c' || range AS \"Code Display\", \
             CASE WHEN range = 2 THEN 'M' ELSE 'm' END AS \"MixedCase\" FROM range(5)",
        );
        let out = lake.commit(&[input("s.q", second, d())]).unwrap();
        assert_eq!(change(&out, "s.q").updated, 1);
        assert_eq!(
            lake.strings("SELECT \"MixedCase\" FROM lake.s.q WHERE \"order\" = 2"),
            ["M"]
        );
        assert_eq!(
            lake.strings(
                "SELECT comment FROM duckdb_columns() WHERE database_name = 'lake' \
                 AND table_name = 'q' AND column_name = 'Code Display'"
            ),
            ["the Code Display"]
        );
    }

    #[test]
    fn duplicate_keys_are_refused() {
        let lake = Lake::new();
        let p = lake.parquet(
            "t.parquet",
            "SELECT * FROM (VALUES (1, 'a'), (42, 'b'), (42, 'c')) AS t(id, v)",
        );
        let err = lake
            .commit(&[input("s.t", p, id_v("s.t"))])
            .unwrap_err()
            .to_string();
        assert!(err.contains("id") && err.contains("42"), "{err}");
        assert!(err.contains("duplicate"), "{err}");
        assert_eq!(
            lake.snapshots(),
            1,
            "only the catalogue's own first snapshot"
        );
    }

    #[test]
    fn null_keys_are_refused() {
        let lake = Lake::new();
        let p = lake.parquet(
            "t.parquet",
            "SELECT * FROM (VALUES (1, 'a'), (NULL, 'b')) AS t(id, v)",
        );
        let err = lake
            .commit(&[input("s.t", p, id_v("s.t"))])
            .unwrap_err()
            .to_string();
        assert!(err.contains("'id'") && err.contains("NULL"), "{err}");
        assert_eq!(lake.snapshots(), 1);
    }

    #[test]
    fn an_added_trailing_column_is_applied_in_the_same_snapshot() {
        let lake = Lake::new();
        let first = lake.parquet("1.parquet", TWENTY);
        let before = lake
            .commit(&[input("s.t", first, id_v("s.t"))])
            .unwrap()
            .snapshot_id;
        let second = lake.parquet(
            "2.parquet",
            &format!("SELECT *, id * 2 AS w FROM ({TWENTY})"),
        );
        let d = descriptor(
            "s.t",
            &[("id", "integer"), ("v", "string"), ("w", "integer")],
            &["id"],
        );
        let out = lake.commit(&[input("s.t", second, d)]).unwrap();
        assert_eq!(out.snapshot_id, before + 1);
        assert_eq!(change(&out, "s.t").updated, 20);
        assert_eq!(lake.count("SELECT sum(w) FROM lake.s.t"), 380);
        let old_columns = lake.read(|c| {
            let mut stmt = c
                .prepare(&format!(
                    "SELECT * FROM lake.s.t AT (VERSION => {before}) LIMIT 1"
                ))
                .unwrap();
            drop(stmt.query([]).unwrap());
            stmt.column_names()
        });
        assert_eq!(old_columns, ["id", "v"]);
    }

    #[test]
    fn a_retyped_column_is_refused_unless_replace_is_set() {
        let lake = Lake::new();
        let first = lake.parquet("1.parquet", TWENTY);
        lake.commit(&[input("s.t", first, id_v("s.t"))]).unwrap();
        let snapshots = lake.snapshots();
        let second = lake.parquet(
            "2.parquet",
            "SELECT range AS id, range::DOUBLE AS v FROM range(4)",
        );
        let d = || descriptor("s.t", &[("id", "integer"), ("v", "number")], &["id"]);
        let err = lake
            .commit(&[input("s.t", second.clone(), d())])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("\"v\"") && err.contains("VARCHAR") && err.contains("DOUBLE"),
            "{err}"
        );
        assert_eq!(lake.snapshots(), snapshots);

        let mut replace = input("s.t", second, d());
        replace.replace_on_schema_change = true;
        let out = lake.commit(&[replace]).unwrap();
        let t = change(&out, "s.t");
        assert_eq!(
            (t.action, t.deleted, t.inserted, t.rows),
            (TableAction::Replaced, 20, 4, 4)
        );
        assert_eq!(
            lake.strings(
                "SELECT data_type FROM duckdb_columns() WHERE database_name = 'lake' \
                 AND table_name = 't' AND column_name = 'v'"
            ),
            ["DOUBLE"]
        );
        assert_eq!(lake.snapshots(), snapshots + 1);
        assert_eq!(
            lake.strings(
                "SELECT comment FROM duckdb_tables() WHERE database_name = 'lake' AND table_name = 't'"
            ),
            ["all about s.t"],
            "the recreated table keeps its comment"
        );
    }

    #[test]
    fn a_changed_key_is_refused_unless_replace_is_set() {
        let lake = Lake::new();
        let p = lake.parquet("t.parquet", TWENTY);
        lake.commit(&[input("s.t", p.clone(), id_v("s.t"))])
            .unwrap();
        let by_v = || descriptor("s.t", &[("id", "integer"), ("v", "string")], &["v"]);
        let err = lake
            .commit(&[input("s.t", p.clone(), by_v())])
            .unwrap_err()
            .to_string();
        assert!(err.contains("key"), "{err}");
        let mut replace = input("s.t", p, by_v());
        replace.replace_on_schema_change = true;
        let out = lake.commit(&[replace]).unwrap();
        assert_eq!(change(&out, "s.t").action, TableAction::Replaced);
        assert_eq!(
            lake.strings("SELECT column_name FROM lake.meta.fields WHERE is_key"),
            ["v"]
        );
    }

    #[test]
    fn a_composite_key_out_of_field_order_republishes_unchanged() {
        let lake = Lake::new();
        let d = || {
            descriptor(
                "s.c",
                &[("id", "integer"), ("region", "string"), ("v", "string")],
                &["region", "id"],
            )
        };
        let p = lake.parquet(
            "c.parquet",
            "SELECT range AS id, 'r' || (range % 2) AS region, 'v' AS v FROM range(6)",
        );
        assert!(
            lake.commit(&[input("s.c", p.clone(), d())])
                .unwrap()
                .committed
        );
        let again = lake.commit(&[input("s.c", p, d())]).unwrap();
        assert!(!again.committed, "{again:?}");
        assert_eq!(change(&again, "s.c").action, TableAction::Unchanged);
    }

    #[test]
    fn a_table_without_a_key_is_replaced_whole() {
        let lake = Lake::new();
        let d = || descriptor("s.k", &[("id", "integer"), ("v", "string")], &[]);
        let first = lake.parquet("1.parquet", TWENTY);
        lake.commit(&[input("s.k", first.clone(), d())]).unwrap();
        let same = lake.commit(&[input("s.k", first, d())]).unwrap();
        assert!(
            !same.committed,
            "an unchanged keyless table commits nothing"
        );
        let second = lake.parquet(
            "2.parquet",
            "SELECT range AS id, 'v' || range AS v FROM range(1, 21)",
        );
        let out = lake.commit(&[input("s.k", second, d())]).unwrap();
        let t = change(&out, "s.k");
        assert_eq!(
            (t.action, t.deleted, t.inserted, t.updated, t.rows),
            (TableAction::Replaced, 20, 20, 0, 20)
        );
        assert_eq!(lake.count("SELECT min(id) FROM lake.s.k"), 1);
    }

    #[test]
    fn the_meta_schema_is_reserved() {
        let lake = Lake::new();
        let p = lake.parquet("t.parquet", TWENTY);
        let err = lake
            .commit(&[input("meta.fields", p, id_v("meta.fields"))])
            .unwrap_err()
            .to_string();
        assert!(err.contains("meta") && err.contains("reserved"), "{err}");
        assert!(
            !lake.catalog().exists(),
            "refused before anything is created"
        );
    }

    #[test]
    fn new_files_name_the_files_this_snapshot_added() {
        let lake = Lake::new();
        let first = lake.parquet("1.parquet", TWENTY);
        let out = lake.commit(&[input("s.t", first, id_v("s.t"))]).unwrap();
        assert!(
            !out.new_files.is_empty(),
            "twenty rows are written to a file"
        );
        for f in &out.new_files {
            assert!(f.starts_with("s/t/"), "{f}");
            assert!(lake.files().join(f).is_file(), "{f}");
        }
        // Deleting 15 rows of the file writes a delete file.
        let second = lake.parquet(
            "2.parquet",
            "SELECT range AS id, 'v' || range AS v FROM range(5)",
        );
        let out2 = lake
            .commit(&[input("s.t", second.clone(), id_v("s.t"))])
            .unwrap();
        assert!(!out2.new_files.is_empty());
        for f in &out2.new_files {
            assert!(
                !out.new_files.contains(f),
                "{f} was already the first snapshot's"
            );
            assert!(lake.files().join(f).is_file(), "{f}");
        }
        let unchanged = lake.commit(&[input("s.t", second, id_v("s.t"))]).unwrap();
        assert!(unchanged.new_files.is_empty());
    }

    #[test]
    fn a_failing_secret_is_never_quoted() {
        let lake = Lake::new();
        let err = commit(
            &lake.catalog(),
            DATA_PATH,
            "s3://bucket/lake/",
            Some("CREATE TEMPORARY SECRET x (TYPE s3, KEY_ID 'sentinel-xyz', BOGUS 1)"),
            &[],
            &json!({}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("secret"), "{err}");
        assert!(!err.contains("sentinel-xyz"), "{err}");
    }
}
