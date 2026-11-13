//! Moving a lake onto a new catalogue: `arc lake rebase`.
//!
//! DuckLake cannot change a catalogue's persisted data path, so a lake whose
//! catalogue names the wrong one is rebuilt. The new catalogue is created with the
//! new data path, every table of the old one is created in it, and each table's
//! current data files are registered by address, with the views over them. The
//! data files themselves are not touched. One snapshot holds the whole move.
//!
//! A table the move cannot carry faithfully (row deletions, or rows DuckLake
//! inlined in the catalogue instead of writing to a file) is refused by name.

use std::path::Path;

use super::meta::META_SCHEMA;
use super::store::{ObjectStore, PublicHttpStore, PutCondition, S3Store};
use crate::error::{Error, Result};
use crate::operator::sql_string_literal;

/// What a rebase carried over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseReport {
    /// `schema.table` and the number of data files registered for it.
    pub tables: Vec<(String, usize)>,
    /// `schema.view`.
    pub views: Vec<String>,
}

const OLD: &str = "arc_old";
const NEW: &str = "arc_new";
const OLD_METADATA: &str = "__ducklake_metadata_arc_old";

fn failed(msg: impl std::fmt::Display) -> Error {
    Error::StepFailed {
        step: String::new(),
        code: 1,
        stderr: format!("lake rebase: {msg}"),
    }
}

fn db(context: &str) -> impl Fn(duckdb::Error) -> Error + '_ {
    move |e| failed(format!("{context}: {e}"))
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `from` as a local file: itself, or, for an https URL, a copy downloaded into
/// `scratch`.
fn local_catalogue(from: &str, scratch: &Path) -> Result<std::path::PathBuf> {
    if !(from.starts_with("https://") || from.starts_with("http://")) {
        let path = Path::new(from);
        if !path.is_file() {
            return Err(failed(format!("the catalogue {from} does not exist")));
        }
        return Ok(path.to_path_buf());
    }
    let (base, key) = from
        .rsplit_once('/')
        .filter(|(_, key)| !key.is_empty())
        .ok_or_else(|| failed(format!("{from} does not name a catalogue file")))?;
    let (bytes, _) = PublicHttpStore::new(base)
        .get(key)
        .map_err(|e| failed(format!("download the catalogue {from}: {e}")))?
        .ok_or_else(|| failed(format!("the catalogue {from} was not found")))?;
    let local = scratch.join("from.ducklake");
    std::fs::write(&local, bytes).map_err(|e| failed(format!("save the catalogue: {e}")))?;
    Ok(local)
}

/// A working folder beside the output, removed when it drops.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn beside(out: &Path) -> Result<Self> {
        let mut name = out.as_os_str().to_owned();
        name.push(".work");
        let dir = std::path::PathBuf::from(name);
        std::fs::create_dir(&dir).map_err(|e| {
            failed(format!(
                "create the working folder {}: {e}; if an earlier rebase left it behind, delete it",
                dir.display()
            ))
        })?;
        Ok(Scratch(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Remove a half-built catalogue and its WAL, so neither is mistaken for a result
/// and the next rebase to the same path does not replay the WAL into its catalogue.
fn discard(out: &Path) {
    let _ = std::fs::remove_file(out);
    let mut wal = out.as_os_str().to_owned();
    wal.push(".wal");
    let _ = std::fs::remove_file(std::path::PathBuf::from(wal));
}

struct OldTable {
    schema: String,
    name: String,
    columns: Vec<(String, String)>,
    /// Each current data file's address in the new lake.
    files: Vec<String>,
}

fn strings(conn: &duckdb::Connection, sql: &str, width: usize) -> Result<Vec<Vec<String>>> {
    let mut stmt = conn.prepare(sql).map_err(db("read the old catalogue"))?;
    let rows = stmt
        .query_map([], |r| {
            (0..width)
                .map(|i| r.get::<_, String>(i))
                .collect::<std::result::Result<Vec<_>, _>>()
        })
        .map_err(db("read the old catalogue"))?;
    rows.map(|r| r.map_err(db("read the old catalogue")))
        .collect()
}

/// Builds a catalogue at `out` with `data_path` as its persisted data path, from
/// the catalogue `from` (a file, or an https URL to download).
pub fn rebase(from: &str, out: &Path, data_path: &str) -> Result<RebaseReport> {
    if out.exists() {
        return Err(failed(format!(
            "{} already exists; rebase writes a new catalogue and never overwrites one",
            out.display()
        )));
    }
    let mut new_data_path = data_path.to_string();
    if !new_data_path.ends_with('/') {
        new_data_path.push('/');
    }
    let scratch = Scratch::beside(out)?;
    let old_file = local_catalogue(from, scratch.path())?;

    let conn = duckdb::Connection::open_in_memory().map_err(db("open duckdb"))?;
    conn.execute_batch("INSTALL ducklake; LOAD ducklake; INSTALL httpfs; LOAD httpfs;")
        .map_err(db("load ducklake"))?;
    conn.execute_batch(&format!(
        "ATTACH {} AS {OLD} (READ_ONLY);",
        sql_string_literal(&format!("ducklake:{}", old_file.display()))
    ))
    .map_err(db("attach the old catalogue"))?;

    let old_data_path: String = conn
        .query_row(
            &format!("SELECT value FROM {OLD_METADATA}.ducklake_metadata WHERE key = 'data_path'"),
            [],
            |r| r.get(0),
        )
        .map_err(db("read the old catalogue's data path"))?;
    let tables = old_tables(&conn, &old_data_path, &new_data_path)?;
    let views = strings(
        &conn,
        &format!(
            "SELECT schema_name, view_name, sql FROM duckdb_views() \
             WHERE database_name = '{OLD}' AND NOT internal AND schema_name <> '{META_SCHEMA}' \
             ORDER BY schema_name, view_name"
        ),
        3,
    )?;
    let ordered_views = views_in_dependency_order(views.clone());
    let schemas = strings(
        &conn,
        &format!(
            "SELECT schema_name FROM duckdb_schemas() \
             WHERE database_name = '{OLD}' AND NOT internal AND schema_name <> '{META_SCHEMA}' \
             ORDER BY schema_name"
        ),
        1,
    )?;

    // The new catalogue is created with the new data path, then attached with its
    // data path overridden by a folder nothing is written to: no data file is
    // written here, only registered.
    let new_catalog = sql_string_literal(&format!("ducklake:{}", out.display()));
    let result = (|| -> Result<()> {
        conn.execute_batch(&format!(
            "ATTACH {new_catalog} AS {NEW} (DATA_PATH {}); DETACH {NEW};",
            sql_string_literal(&new_data_path)
        ))
        .map_err(db("create the new catalogue"))?;
        conn.execute_batch(&format!(
            "ATTACH {new_catalog} AS {NEW} (DATA_PATH {}, OVERRIDE_DATA_PATH true);",
            sql_string_literal(&format!("{}/", scratch.path().join("unused").display()))
        ))
        .map_err(db("attach the new catalogue"))?;
        conn.execute_batch("BEGIN").map_err(db("begin"))?;
        for schema in schemas.iter().map(|s| &s[0]).filter(|s| *s != "main") {
            conn.execute_batch(&format!("CREATE SCHEMA {NEW}.{};", ident(schema)))
                .map_err(db(&format!("create the schema {schema}")))?;
        }
        for t in &tables {
            let cols = t
                .columns
                .iter()
                .map(|(n, ty)| format!("{} {ty}", ident(n)))
                .collect::<Vec<_>>()
                .join(", ");
            let full = format!("{NEW}.{}.{}", ident(&t.schema), ident(&t.name));
            conn.execute_batch(&format!("CREATE TABLE {full} ({cols});"))
                .map_err(db(&format!("create {}.{}", t.schema, t.name)))?;
            for file in &t.files {
                conn.execute_batch(&format!(
                    "CALL ducklake_add_data_files('{NEW}', {}, {}, schema => {});",
                    sql_string_literal(&t.name),
                    sql_string_literal(file),
                    sql_string_literal(&t.schema)
                ))
                .map_err(db(&format!("register {file} in {}.{}", t.schema, t.name)))?;
            }
        }
        conn.execute_batch(&format!("USE {NEW};"))
            .map_err(db("select the new catalogue"))?;
        for v in &ordered_views {
            conn.execute_batch(&strip_alias(&v[2]))
                .map_err(db(&format!("recreate the view {}.{}", v[0], v[1])))?;
        }
        conn.execute_batch(&format!(
            "CALL {NEW}.set_commit_message('arcform', 'arc lake rebase');"
        ))
        .map_err(db("set the commit message"))?;
        conn.execute_batch("COMMIT").map_err(db("commit"))
    })();
    if let Err(e) = result {
        let _ = conn.execute_batch("ROLLBACK");
        drop(conn);
        discard(out);
        return Err(e);
    }

    Ok(RebaseReport {
        tables: tables
            .iter()
            .map(|t| (format!("{}.{}", t.schema, t.name), t.files.len()))
            .collect(),
        views: views.iter().map(|v| format!("{}.{}", v[0], v[1])).collect(),
    })
}

/// The old catalogue's tables, each with its columns and current data files
/// addressed in the new lake. A table with deletions or inlined rows is refused.
fn old_tables(
    conn: &duckdb::Connection,
    old_data_path: &str,
    new_data_path: &str,
) -> Result<Vec<OldTable>> {
    let listed = strings(
        conn,
        &format!(
            "SELECT schema_name, table_name FROM duckdb_tables() \
             WHERE database_name = '{OLD}' AND NOT internal AND schema_name <> '{META_SCHEMA}' \
             ORDER BY schema_name, table_name"
        ),
        2,
    )?;
    let mut out = Vec::with_capacity(listed.len());
    for t in listed {
        let (schema, name) = (t[0].clone(), t[1].clone());
        let label = format!("{schema}.{name}");
        let columns = strings(
            conn,
            &format!(
                "SELECT column_name, data_type FROM duckdb_columns() \
                 WHERE database_name = '{OLD}' AND schema_name = {} AND table_name = {} \
                 ORDER BY column_index",
                sql_string_literal(&schema),
                sql_string_literal(&name)
            ),
            2,
        )?
        .into_iter()
        .map(|c| (c[0].clone(), c[1].clone()))
        .collect();

        let deletes: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(delete_file) FROM ducklake_list_files('{OLD}', {}, schema => {})",
                    sql_string_literal(&name),
                    sql_string_literal(&schema)
                ),
                [],
                |r| r.get(0),
            )
            .map_err(db(&format!("list the files of {label}")))?;
        if deletes > 0 {
            return Err(failed(format!(
                "table {label} has delete files; row deletions cannot be carried by \
                 registering its data files"
            )));
        }
        let (inlined_rows, inlined_deletes) = inlined(conn, &schema, &name)?;
        if inlined_deletes > 0 {
            return Err(failed(format!(
                "table {label} has deleted rows; row deletions cannot be carried by \
                 registering its data files"
            )));
        }
        if inlined_rows > 0 {
            return Err(failed(format!(
                "table {label} holds {inlined_rows} rows inside the catalogue rather than in a \
                 data file; they cannot be carried by registering files"
            )));
        }
        let files = strings(
            conn,
            &format!(
                "SELECT data_file FROM ducklake_list_files('{OLD}', {}, schema => {}) \
                 ORDER BY data_file",
                sql_string_literal(&name),
                sql_string_literal(&schema)
            ),
            1,
        )?
        .into_iter()
        .map(|f| match f[0].strip_prefix(old_data_path) {
            Some(rest) if !old_data_path.is_empty() => format!("{new_data_path}{rest}"),
            _ => f[0].clone(),
        })
        .collect();
        out.push(OldTable {
            schema,
            name,
            columns,
            files,
        });
    }
    Ok(out)
}

/// Rows of the table that DuckLake holds in the catalogue itself, and rows of its
/// data files that it deleted there, rather than in a file.
fn inlined(conn: &duckdb::Connection, schema: &str, name: &str) -> Result<(i64, i64)> {
    let ids = strings(
        conn,
        &format!(
            "SELECT t.table_id::VARCHAR FROM {OLD_METADATA}.ducklake_table t \
             JOIN {OLD_METADATA}.ducklake_schema s ON s.schema_id = t.schema_id \
             WHERE t.end_snapshot IS NULL AND s.end_snapshot IS NULL \
             AND t.table_name = {} AND s.schema_name = {}",
            sql_string_literal(name),
            sql_string_literal(schema)
        ),
        1,
    )?;
    let mut rows = 0;
    let mut deletes = 0;
    for id in ids {
        let data = strings(
            conn,
            &format!(
                "SELECT table_name FROM {OLD_METADATA}.ducklake_inlined_data_tables \
                 WHERE table_id = {}",
                id[0]
            ),
            1,
        )?;
        for t in data {
            // The end-snapshot column is named differently across DuckLake versions.
            let live = |column: &str| {
                count(
                    conn,
                    &format!(
                        "SELECT count(*) FROM {OLD_METADATA}.{} WHERE {column} IS NULL",
                        ident(&t[0])
                    ),
                )
            };
            rows += live("_ducklake_end_snapshot").or_else(|_| live("end_snapshot"))?;
        }
        let table = ident(&format!("ducklake_inlined_delete_{}", id[0]));
        deletes += count(
            conn,
            &format!("SELECT count(*) FROM {OLD_METADATA}.{table}"),
        )
        .or_else(|e| match e {
            // Absent until a row has been deleted from one of the table's files.
            Error::StepFailed { stderr, .. } if stderr.contains("does not exist") => Ok(0),
            other => Err(other),
        })?;
    }
    Ok((rows, deletes))
}

fn count(conn: &duckdb::Connection, sql: &str) -> Result<i64> {
    conn.query_row(sql, [], |r| r.get(0))
        .map_err(db("count rows of the old catalogue"))
}

/// A view's SQL with the old catalogue's alias removed where it qualifies a name, so
/// it resolves against the new catalogue, which is the default while the views are
/// created. Only the alias as a whole identifier is removed, never inside a string
/// literal or a longer identifier.
fn strip_alias(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let alias: Vec<char> = OLD.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            // Copy the quoted run whole; a doubled quote is an escaped one.
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == c {
                    if chars.get(i + 1) == Some(&c) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(chars.len());
            let run: String = chars[start..i].iter().collect();
            if c == '"' && run == format!("\"{OLD}\"") && chars.get(i) == Some(&'.') {
                i += 1; // the quoted alias and its dot
            } else {
                out.push_str(&run);
            }
            continue;
        }
        let boundary = i == 0 || !is_ident(chars[i - 1]);
        if boundary && chars[i..].starts_with(&alias) && chars.get(i + alias.len()) == Some(&'.') {
            i += alias.len() + 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The dotted identifier chains in `sql` outside string literals, lowercased, with
/// quoted identifiers unquoted.
fn identifier_chains(sql: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = sql.chars().collect();
    let mut chains: Vec<Vec<String>> = Vec::new();
    let mut open = false; // the last token was a dot, so the next identifier extends
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            open = false;
        } else if c == '"' || c.is_alphabetic() || c == '_' {
            let mut word = String::new();
            if c == '"' {
                i += 1;
                while i < chars.len() {
                    if chars[i] == '"' {
                        if chars.get(i + 1) == Some(&'"') {
                            word.push('"');
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    word.push(chars[i]);
                    i += 1;
                }
                i += 1;
            } else {
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
                {
                    word.push(chars[i]);
                    i += 1;
                }
            }
            let word = word.to_lowercase();
            match chains.last_mut() {
                Some(chain) if open => chain.push(word),
                _ => chains.push(vec![word]),
            }
            open = false;
        } else if c == '.' && !open && !chains.is_empty() {
            open = true;
            i += 1;
        } else {
            i += 1;
            if !c.is_whitespace() {
                open = false;
            }
        }
    }
    chains
}

/// The views a view's SQL refers to, among `views`: by `schema.name` (any catalogue
/// before it ignored), or by bare name for a view in the same schema or in `main`.
/// The view's own `CREATE VIEW <name> AS` header is not a reference.
fn view_references(view: &[String], views: &[Vec<String>]) -> Vec<usize> {
    let chains = identifier_chains(&view[2]);
    let body = chains
        .iter()
        .position(|c| c == &["view".to_string()])
        .and_then(|v| {
            chains[v..]
                .iter()
                .position(|c| c == &["as".to_string()])
                .map(|a| v + a + 1)
        })
        .unwrap_or(0);
    let own_schema = view[0].to_lowercase();
    views
        .iter()
        .enumerate()
        .filter(|(_, other)| {
            let (schema, name) = (other[0].to_lowercase(), other[1].to_lowercase());
            chains[body..].iter().any(|c| match c.as_slice() {
                [n] => *n == name && (schema == own_schema || schema == "main"),
                [.., sch, n] => *sch == schema && *n == name,
                [] => false,
            })
        })
        .map(|(i, _)| i)
        .collect()
}

/// Views in an order each can be created in: a view after every view it refers to.
/// A cycle leaves the rest in name order, and creating them then fails naming the
/// view. Views over tables only keep their name order.
fn views_in_dependency_order(views: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let deps: Vec<Vec<usize>> = views
        .iter()
        .enumerate()
        .map(|(i, v)| {
            view_references(v, &views)
                .into_iter()
                .filter(|j| *j != i)
                .collect()
        })
        .collect();
    let mut done = vec![false; views.len()];
    let mut order: Vec<usize> = Vec::with_capacity(views.len());
    while order.len() < views.len() {
        let next = (0..views.len())
            .find(|&i| !done[i] && deps[i].iter().all(|&j| done[j]))
            .or_else(|| (0..views.len()).find(|&i| !done[i]))
            .expect("a view is left");
        done[next] = true;
        order.push(next);
    }
    order.into_iter().map(|i| views[i].clone()).collect()
}

/// The key a catalogue URL names in its bucket: the URL's path without the leading
/// slash.
pub fn upload_key(url: &str) -> Result<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| failed("--upload must be the catalogue's https URL"))?;
    let key = rest.split_once('/').map_or("", |(_, path)| path);
    if key.is_empty() || key.ends_with('/') {
        return Err(failed("--upload must name a catalogue file, not a folder"));
    }
    Ok(key.to_string())
}

/// The object store a `--credential-env` value names. The value is
/// `<type>:<param>=<VAR>,...`: each `<VAR>` is the name of an environment variable
/// that holds the parameter, so no secret is ever on the command line. Types: `r2`
/// (`account_id`, `key_id`, `secret`) and `s3` (`endpoint`, `key_id`, `secret`,
/// and optionally `region`, default `auto`).
pub fn store_from_env(
    bucket: &str,
    spec: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<S3Store> {
    let (kind, params) = spec.split_once(':').ok_or_else(|| {
        failed("--credential-env must be `<type>:<param>=<VAR>,...`, for example `r2:account_id=ACCOUNT,key_id=KEY,secret=SECRET`")
    })?;
    let mut values: Vec<(String, String)> = Vec::new();
    for pair in params.split(',').filter(|p| !p.is_empty()) {
        let (param, var) = pair
            .split_once('=')
            .filter(|(p, v)| !p.is_empty() && !v.is_empty())
            .ok_or_else(|| {
                failed(format!(
                    "--credential-env entry '{pair}' is not <param>=<VAR>"
                ))
            })?;
        let value = env(var).filter(|v| !v.is_empty()).ok_or_else(|| {
            failed(format!(
                "the environment variable {var} (for {param}) is not set"
            ))
        })?;
        values.push((param.to_string(), value));
    }
    let get = |param: &str| -> Result<&str> {
        values
            .iter()
            .find(|(p, _)| p == param)
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| {
                failed(format!(
                    "--credential-env {kind} needs the parameter {param}"
                ))
            })
    };
    let known: &[&str] = match kind {
        "r2" => &["account_id", "key_id", "secret"],
        "s3" => &["endpoint", "region", "key_id", "secret"],
        _ => {
            return Err(failed(format!(
                "--credential-env type '{kind}' is not r2 or s3"
            )));
        }
    };
    if let Some((p, _)) = values.iter().find(|(p, _)| !known.contains(&p.as_str())) {
        return Err(failed(format!(
            "--credential-env {kind} has no parameter {p}"
        )));
    }
    // The store's own error never carries a credential.
    let built = match kind {
        "r2" => S3Store::r2(get("account_id")?, bucket, get("key_id")?, get("secret")?),
        _ => S3Store::new(
            get("endpoint")?,
            bucket,
            get("region").unwrap_or("auto"),
            get("key_id")?,
            get("secret")?,
        ),
    };
    built.map_err(|e| failed(format!("the object store: {e}")))
}

/// Writes the catalogue at `file` to the key `catalogue_url` names, only if nothing
/// is there, so a live catalogue is never overwritten. Returns the key.
pub fn upload_catalogue(
    file: &Path,
    catalogue_url: &str,
    store: &dyn ObjectStore,
) -> Result<String> {
    let key = upload_key(catalogue_url)?;
    store
        .put_file(
            &key,
            file,
            PutCondition::IfAbsent,
            Some("public, max-age=60"),
        )
        .map_err(|e| match e {
            super::store::StoreError::PreconditionFailed => {
                failed(format!("{key} already exists; it is not overwritten"))
            }
            other => failed(format!("upload {key}: {other}")),
        })?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lake::commit::{TableInput, commit};
    use crate::lake::meta::table_descriptor;
    use serde_json::json;
    use tempfile::TempDir;

    const NEW_DATA_PATH: &str = "https://example.invalid/lake/";

    fn exec(sql: &str) {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("LOAD ducklake; {sql}"))
            .unwrap();
    }

    /// An old catalogue persisting a local data path, with `a` (two files) and `b`
    /// (one) registered by absolute path, a view over `a`, and a `meta` table.
    fn old_lake(dir: &TempDir) -> std::path::PathBuf {
        let p = dir.path();
        let files = p.join("files");
        std::fs::create_dir_all(&files).unwrap();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT range AS id, 'a' || range AS v FROM range(0, 3)) TO '{f}/a1.parquet'; \
             COPY (SELECT range AS id, 'a' || range AS v FROM range(3, 5)) TO '{f}/a2.parquet'; \
             COPY (SELECT 7 AS n) TO '{f}/b1.parquet';",
            f = files.display()
        ))
        .unwrap();
        let catalog = p.join("old.ducklake");
        exec(&format!(
            "ATTACH 'ducklake:{c}' AS o (DATA_PATH '{p}/olddata/'); \
             CREATE SCHEMA o.s; \
             CREATE TABLE o.s.a (id BIGINT, v VARCHAR); \
             CREATE TABLE o.b (n INTEGER); \
             CREATE SCHEMA o.meta; CREATE TABLE o.meta.descriptors (x INTEGER); \
             CALL ducklake_add_data_files('o', 'a', '{f}/a1.parquet', schema => 's'); \
             CALL ducklake_add_data_files('o', 'a', '{f}/a2.parquet', schema => 's'); \
             CALL ducklake_add_data_files('o', 'b', '{f}/b1.parquet'); \
             CREATE VIEW o.s.a_view AS SELECT id FROM o.s.a WHERE id > 1;",
            c = catalog.display(),
            p = p.display(),
            f = files.display()
        ));
        catalog
    }

    fn query_i64(catalog: &Path, data_path: &str, sql: &str) -> i64 {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "LOAD ducklake; ATTACH 'ducklake:{}' AS n (DATA_PATH '{data_path}', OVERRIDE_DATA_PATH true);",
            catalog.display()
        ))
        .unwrap();
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn rebase_carries_tables_files_and_views() {
        let dir = TempDir::new().unwrap();
        let old = old_lake(&dir);
        let out = dir.path().join("new.ducklake");
        let report = rebase(old.to_str().unwrap(), &out, NEW_DATA_PATH).unwrap();
        assert_eq!(
            report.tables,
            vec![("main.b".to_string(), 1), ("s.a".to_string(), 2)]
        );
        assert_eq!(report.views, vec!["s.a_view".to_string()]);

        let local = format!("{}/x/", dir.path().display());
        assert_eq!(query_i64(&out, &local, "SELECT count(*) FROM n.s.a"), 5);
        assert_eq!(query_i64(&out, &local, "SELECT sum(n) FROM n.main.b"), 7);
        assert_eq!(
            query_i64(&out, &local, "SELECT count(*) FROM n.s.a_view"),
            3
        );
        assert_eq!(
            query_i64(
                &out,
                &local,
                "SELECT count(*) FROM __ducklake_metadata_n.ducklake_metadata \
                 WHERE key = 'data_path' AND value = 'https://example.invalid/lake/'"
            ),
            1
        );
        // One snapshot for the move, after the one that creates the catalogue.
        assert_eq!(
            query_i64(&out, &local, "SELECT count(*) FROM ducklake_snapshots('n')"),
            2
        );
    }

    #[test]
    fn rebase_refuses_a_table_with_deletions() {
        let dir = TempDir::new().unwrap();
        let old = old_lake(&dir);
        exec(&format!(
            "ATTACH 'ducklake:{c}' AS o (DATA_PATH '{p}/olddata/'); \
             DELETE FROM o.s.a WHERE id = 1;",
            c = old.display(),
            p = dir.path().display()
        ));
        let out = dir.path().join("new.ducklake");
        let err = rebase(old.to_str().unwrap(), &out, NEW_DATA_PATH).unwrap_err();
        assert!(err.to_string().contains("s.a"), "{err}");
        assert!(err.to_string().contains("delete"), "{err}");
        assert!(!out.exists(), "no catalogue is left behind");
    }

    /// A failure after the new catalogue is created removes it, and its WAL.
    #[test]
    fn a_failed_rebase_leaves_no_catalogue_or_wal() {
        let dir = TempDir::new().unwrap();
        let old = old_lake(&dir);
        // A registered file gone missing fails the rebase partway through.
        std::fs::remove_file(dir.path().join("files/b1.parquet")).unwrap();
        let out = dir.path().join("new.ducklake");
        let err = rebase(old.to_str().unwrap(), &out, NEW_DATA_PATH).unwrap_err();
        assert!(err.to_string().contains("b1.parquet"), "{err}");
        assert!(!out.exists(), "no catalogue is left behind");
        assert!(
            !dir.path().join("new.ducklake.wal").exists(),
            "no WAL is left behind"
        );
    }

    /// DuckDB checkpoints as the connection closes, so a WAL survives only an
    /// interrupted checkpoint; `discard` removes one all the same, since the next
    /// rebase to that path would replay it into its new catalogue.
    #[test]
    fn discard_removes_the_catalogue_and_its_wal() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("new.ducklake");
        std::fs::write(&out, b"catalogue").unwrap();
        std::fs::write(dir.path().join("new.ducklake.wal"), b"wal").unwrap();
        discard(&out);
        assert!(!out.exists());
        assert!(!dir.path().join("new.ducklake.wal").exists());
        discard(&out);
    }

    #[test]
    fn a_rebased_catalogue_accepts_a_ducklake_write_commit() {
        let dir = TempDir::new().unwrap();
        let old = old_lake(&dir);
        let out = dir.path().join("new.ducklake");
        rebase(old.to_str().unwrap(), &out, NEW_DATA_PATH).unwrap();

        let changed = dir.path().join("changed.parquet");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT range::BIGINT AS id, 'new' AS v FROM range(0, 20)) TO '{}';",
            changed.display()
        ))
        .unwrap();
        let package = json!({"name": "a", "resources": [{"name": "a", "format": "parquet",
            "schema": {"fields": [{"name": "id", "type": "integer"},
                                  {"name": "v", "type": "string"}]}}]});
        let descriptor =
            table_descriptor("s.a", &package, &["id".to_string(), "v".to_string()]).unwrap();
        let write_path = format!("{}/write/", dir.path().display());
        let outcome = commit(
            &out,
            NEW_DATA_PATH,
            &write_path,
            None,
            &[TableInput {
                table: "s.a".to_string(),
                parquet: changed,
                key: Vec::new(),
                descriptor,
                replace_on_schema_change: false,
            }],
            &json!({"run": "r1"}),
        )
        .unwrap();
        assert!(outcome.committed);
        assert!(!outcome.new_files.is_empty());
        assert!(
            outcome
                .new_files
                .iter()
                .all(|f| { f.contains("ducklake-") && dir.path().join("write").join(f).exists() }),
            "{:?}",
            outcome.new_files
        );
        assert_eq!(
            query_i64(&out, &write_path, "SELECT count(*) FROM n.s.a"),
            20
        );
        assert_eq!(
            query_i64(&out, &write_path, "SELECT count(*) FROM n.s.a_view"),
            18
        );
        // The two registered files are logically deleted.
        assert_eq!(
            query_i64(
                &out,
                &write_path,
                "SELECT count(*) FROM __ducklake_metadata_n.ducklake_data_file \
                 WHERE end_snapshot IS NOT NULL"
            ),
            2
        );
    }

    #[test]
    fn credential_env_reads_values_from_the_environment_and_never_echoes_them() {
        let env = |name: &str| match name {
            "A" => Some("acct".to_string()),
            "K" => Some("key-id".to_string()),
            "S" => Some("s3cr3t-value".to_string()),
            _ => None,
        };
        assert!(store_from_env("b", "r2:account_id=A,key_id=K,secret=S", &env).is_ok());
        let err = store_from_env("b", "r2:account_id=A,key_id=K,secret=MISSING", &env)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("MISSING"), "{err}");
        let err = store_from_env("b", "r2:account_id=A,key_id=K,bogus=S", &env)
            .err()
            .unwrap()
            .to_string();
        assert!(!err.contains("s3cr3t-value"), "{err}");
        assert!(store_from_env("b", "gcs:x=A", &env).is_err());
        assert!(store_from_env("b", "r2", &env).is_err());
    }

    #[test]
    fn upload_refuses_an_existing_catalogue() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("c.ducklake");
        std::fs::write(&file, b"x").unwrap();
        let store = crate::lake::store::DirStore::new(dir.path().join("bucket"));
        let url = "https://example.invalid/catalog/open.ducklake";
        assert_eq!(
            upload_catalogue(&file, url, &store).unwrap(),
            "catalog/open.ducklake"
        );
        let err = upload_catalogue(&file, url, &store).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn strip_alias_rewrites_only_the_whole_identifier_outside_literals() {
        assert_eq!(
            strip_alias(
                "SELECT x, 'arc_old.y' AS lit, \"my arc_old.col\", my_arc_old.z, arc_old.s.t.c \
                 FROM arc_old.s.t JOIN \"arc_old\".s.u ON (arc_old.s.u.k = 1)"
            ),
            "SELECT x, 'arc_old.y' AS lit, \"my arc_old.col\", my_arc_old.z, s.t.c \
             FROM s.t JOIN s.u ON (s.u.k = 1)"
        );
    }

    #[test]
    fn views_over_later_named_views_are_recreated() {
        let dir = TempDir::new().unwrap();
        let old = old_lake(&dir);
        exec(&format!(
            "ATTACH 'ducklake:{c}' AS o (DATA_PATH '{p}/olddata/'); \
             CREATE VIEW o.s.b_base AS SELECT id, 'arc_old.x' AS lit FROM o.s.a; \
             CREATE VIEW o.s.a_summary AS SELECT count(*) AS n FROM o.s.b_base;",
            c = old.display(),
            p = dir.path().display()
        ));
        let out = dir.path().join("new.ducklake");
        let report = rebase(old.to_str().unwrap(), &out, NEW_DATA_PATH).unwrap();
        assert_eq!(report.views.len(), 3);
        let local = format!("{}/x/", dir.path().display());
        assert_eq!(query_i64(&out, &local, "SELECT n FROM n.s.a_summary"), 5);
    }

    #[test]
    fn same_named_views_in_two_schemas_and_case_differences_are_ordered() {
        let dir = TempDir::new().unwrap();
        let old = old_lake(&dir);
        exec(&format!(
            "ATTACH 'ducklake:{c}' AS o (DATA_PATH '{p}/olddata/'); \
             CREATE SCHEMA o.mart; CREATE SCHEMA o.staging; \
             CREATE VIEW o.staging.orders AS SELECT id FROM o.s.a; \
             CREATE VIEW o.mart.orders AS SELECT id FROM o.staging.orders; \
             CREATE VIEW o.s.b_base AS SELECT id FROM o.s.a; \
             CREATE VIEW o.s.a_upper AS SELECT id FROM \"B_BASE\";",
            c = old.display(),
            p = dir.path().display()
        ));
        let out = dir.path().join("new.ducklake");
        rebase(old.to_str().unwrap(), &out, NEW_DATA_PATH).unwrap();
        let local = format!("{}/x/", dir.path().display());
        assert_eq!(
            query_i64(&out, &local, "SELECT count(*) FROM n.mart.orders"),
            5
        );
        assert_eq!(
            query_i64(&out, &local, "SELECT count(*) FROM n.s.a_upper"),
            5
        );
    }
}
