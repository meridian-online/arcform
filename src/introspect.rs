//! SQL introspection via sqlparser-rs.
//!
//! Parses SQL files using the DuckDB dialect to extract:
//! - **Outputs**: tables/views created or written to (CREATE TABLE, CREATE VIEW, CTAS, INSERT INTO, COPY TO)
//! - **Inputs**: tables read from (FROM, JOIN clauses)
//!
//! **File-path lineage.** A DuckDB file-reader in a FROM clause — `read_parquet('x.parquet')`,
//! `read_csv(...)`, `read_json(['a.json','b.json'])` — is a table-valued function whose *first
//! argument is a filesystem path*. Rather than record the opaque function name (`read_parquet`)
//! as the input, we lift the path literal(s) it reads and a `COPY … TO 'file'` writes: those
//! path-shaped names become filesystem-backed assets downstream (see [`crate::contract`]) —
//! one file, a directory of files, or a glob, per [`SqlAssets::kinds`]. Lineage into and out of
//! files is thus *discovered from the SQL*, never hand-declared via `depends_on:`.

use std::collections::{BTreeMap, BTreeSet};

use sqlparser::ast::{
    CopyOption, CopySource, CopyTarget, Expr, FunctionArg, FunctionArgExpr, Insert, ObjectName,
    Statement, TableFactor, TableObject, Value,
};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;

use crate::asset_kind::AssetKind;

/// Assets discovered from parsing a SQL file — four-set model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SqlAssets {
    /// Tables/views this SQL creates, writes to, or modifies (ALTER).
    pub outputs: BTreeSet<String>,
    /// External tables this SQL reads data from (CTEs excluded).
    pub inputs: BTreeSet<String>,
    /// CTE names — step-internal assets visible in lineage but not cross-step dependencies.
    pub internal: BTreeSet<String>,
    /// Tables/views this SQL drops — destructive operations tracked separately.
    pub destroys: BTreeSet<String>,
    /// What each name in `outputs`/`inputs` actually is — set here, at the one place
    /// that already knows. A bare identifier from CREATE/FROM/JOIN is a `Table`. A
    /// path literal lifted from a file-reader argument is a `File`, or a `Pattern`
    /// when [`is_glob`] holds. A `COPY … TO` target is classified by
    /// [`copy_to_target_kind`] from the statement's own options and is never tested
    /// for glob metacharacters — DuckDB's COPY target is a literal path, not a
    /// pattern. Never reconstructed later from the string.
    pub kinds: BTreeMap<String, AssetKind>,
}

impl SqlAssets {
    fn record_output(&mut self, name: String, kind: AssetKind) {
        self.kinds.insert(name.clone(), kind);
        self.outputs.insert(name);
    }

    fn record_input(&mut self, name: String, kind: AssetKind) {
        self.kinds.insert(name.clone(), kind);
        self.inputs.insert(name);
    }
}

/// Whether a lifted path literal is a glob pattern rather than one literal path.
fn is_glob(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

/// The `COPY … TO 'target'` option names under which DuckDB writes a *directory* of
/// files at `target` instead of one file at `target`.
///
/// Read out of DuckDB's own source rather than assembled from the cases someone
/// happened to hit. `PhysicalCopyToFile::GetGlobalSinkState`
/// (`src/execution/operator/persistent/physical_copy_to_file.cpp`) creates `target`
/// as a directory on `partition_output || per_thread_output || rotate`;
/// `Binder::BindCopyTo` (`src/planner/binder/statement/bind_copy.cpp`) sets the
/// first from a non-empty `PARTITION_BY` column list, the second from
/// `PER_THREAD_OUTPUT`, and the third from `CopyFunction::rotate_files`. The two
/// `rotate_files` implementations that exist in the tree — `WriteCSVRotateFiles`
/// (`src/function/table/copy_csv.cpp`) and `ParquetWriteRotateFiles`
/// (`extension/parquet/parquet_extension.cpp`) — return true for `FILE_SIZE_BYTES`,
/// and the parquet one additionally for `ROW_GROUPS_PER_FILE`. That branch and its
/// three inputs are byte-identical in v1.5.2 (the `libduckdb-sys` this crate's
/// lockfile pins) and v1.5.5 (the newest published at the time of writing); CI links
/// v1.5.4, between them.
const DIRECTORY_WRITING_COPY_OPTIONS: [&str; 4] = [
    "PARTITION_BY",
    "PER_THREAD_OUTPUT",
    "FILE_SIZE_BYTES",
    "ROW_GROUPS_PER_FILE",
];

/// What a `COPY … TO 'filename'` writes, decided from the statement's own options
/// against [`DIRECTORY_WRITING_COPY_OPTIONS`].
fn copy_to_target_kind(options: &[CopyOption]) -> AssetKind {
    let writes_a_directory = options.iter().any(|opt| match opt {
        CopyOption::DuckDbOption { name, value } => {
            DIRECTORY_WRITING_COPY_OPTIONS
                .iter()
                .any(|known| name.value.eq_ignore_ascii_case(known))
                && copy_option_is_on(&name.value, value)
        }
        _ => false,
    });
    if writes_a_directory {
        AssetKind::Directory
    } else {
        AssetKind::File
    }
}

/// Whether an option carrying one of those names is actually switched on. DuckDB's
/// binder applies three different rules to these four tokens, so this does too:
///
/// * `PARTITION_BY` — `partition_output = !partition_cols.empty()`, so an empty
///   column list leaves it off.
/// * `PER_THREAD_OUTPUT` — `GetBooleanArg`, which is
///   `arg.empty() || arg[0].CastAs(BOOLEAN).GetValue<bool>()`. It **casts**, so the
///   argument does not have to be the `false` keyword: see [`boolean_arg`].
/// * `FILE_SIZE_BYTES` and `ROW_GROUPS_PER_FILE` — neither is read as a boolean at
///   all. `rotate` is `file_size_bytes.IsValid() || row_groups_per_file.IsValid()`,
///   set from the option carrying any value, so presence is the whole test. Measured
///   on DuckDB v1.5.4 and v1.5.5: `FILE_SIZE_BYTES 0` writes a directory.
fn copy_option_is_on(name: &str, value: &Option<Expr>) -> bool {
    if name.eq_ignore_ascii_case("PARTITION_BY") {
        !matches!(value, Some(Expr::Tuple(items)) if items.is_empty())
    } else if name.eq_ignore_ascii_case("PER_THREAD_OUTPUT") {
        boolean_arg(value)
    } else {
        true
    }
}

/// DuckDB's `GetBooleanArg` for a `COPY` option: no argument is true, and otherwise
/// the argument is **cast** to BOOLEAN rather than compared against a keyword.
///
/// Recognising only the `false` keyword is what this replaced, and it was wrong in
/// the direction that never settles: `PER_THREAD_OUTPUT 0` and
/// `PER_THREAD_OUTPUT 'false'` each wrote a single file on DuckDB v1.5.4 and v1.5.5,
/// while a `Directory` classification would `read_dir` that file, get `None`, and
/// re-run the step on every run while warning that it produced nothing.
///
/// The string arm is `TryCastStringBool` with `strict = false`, which is what
/// `Value::CastAs` defaults to: `t`/`y`/`1`/`yes`/`true` and `f`/`n`/`0`/`no`/`false`,
/// case-insensitively. A string outside that set is a conversion error in DuckDB and
/// the statement writes nothing at all, so what this returns for it cannot be
/// observed on disk; it stays `true`, the answer that forces staleness rather than
/// certifying an artifact.
fn boolean_arg(value: &Option<Expr>) -> bool {
    let Some(Expr::Value(v)) = value else {
        // No argument is a bare flag, which DuckDB reads as true. A non-literal
        // expression is not something this can evaluate; leave it on.
        return true;
    };
    match &v.value {
        Value::Boolean(b) => *b,
        Value::Number(n, _) => n.parse::<f64>().map(|x| x != 0.0).unwrap_or(true),
        Value::SingleQuotedString(s)
        | Value::DoubleQuotedString(s)
        | Value::TripleSingleQuotedString(s)
        | Value::TripleDoubleQuotedString(s) => cast_string_to_bool(s).unwrap_or(true),
        _ => true,
    }
}

/// `TryCastStringBool` with `strict = false`, from DuckDB's `cast_operators.hpp`.
/// `None` where DuckDB raises a conversion error.
fn cast_string_to_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "t" | "y" | "1" | "yes" | "true" => Some(true),
        "f" | "n" | "0" | "no" | "false" => Some(false),
        _ => None,
    }
}

/// Parse a SQL string and extract the assets it produces and consumes.
///
/// Returns `Ok(SqlAssets)` on success, or `Err(warnings)` if the SQL
/// cannot be parsed. The caller should treat parse failures as opaque
/// steps (warn, don't block).
pub fn extract_assets(sql: &str) -> Result<SqlAssets, Vec<String>> {
    let dialect = DuckDbDialect {};
    let statements = Parser::parse_sql(&dialect, sql).map_err(|e| vec![e.to_string()])?;

    let mut assets = SqlAssets::default();

    for stmt in &statements {
        extract_from_statement(stmt, &mut assets);
    }

    // CTE filtering: CTE names were collected in `internal` during parsing.
    // Remove them from `inputs` — a CTE reference in FROM is step-internal,
    // not an external dependency.
    for cte_name in &assets.internal {
        assets.inputs.remove(cte_name);
    }

    Ok(assets)
}

/// Parse a SQL string and extract assets **per statement**, in source order.
///
/// Like [`extract_assets`] but returns one [`SqlAssets`] per top-level statement
/// instead of a single merged set — so a contract can record which tables each
/// statement produces/reads. CTE names are filtered out of `inputs` per statement,
/// exactly as the merged path does.
///
/// Returns `Ok(Vec<SqlAssets>)` on success, or `Err(warnings)` if the SQL cannot be
/// parsed (caller treats a parse failure as an opaque step).
pub fn extract_per_statement(sql: &str) -> Result<Vec<SqlAssets>, Vec<String>> {
    let dialect = DuckDbDialect {};
    let statements = Parser::parse_sql(&dialect, sql).map_err(|e| vec![e.to_string()])?;

    let mut per_statement = Vec::with_capacity(statements.len());
    for stmt in &statements {
        let mut assets = SqlAssets::default();
        extract_from_statement(stmt, &mut assets);
        // Per-statement CTE filtering: a CTE reference is step-internal, not an input.
        let internal: Vec<String> = assets.internal.iter().cloned().collect();
        for cte_name in internal {
            assets.inputs.remove(&cte_name);
        }
        per_statement.push(assets);
    }
    Ok(per_statement)
}

/// The `[start, end)` byte offset of each top-level statement in `sql`, in source order.
///
/// A lexical splitter (not the AST) that walks the raw bytes so a renderer can slice
/// `sql` and show the exact source of each statement. It splits on top-level `;`,
/// skipping over single-quoted strings, double-quoted identifiers, `--` line comments,
/// `/* … */` block comments, and `$tag$ … $tag$` dollar-quoted bodies so a `;` inside
/// any of those never splits. Comment-only / whitespace-only segments are dropped, so
/// on well-formed SQL the count matches [`extract_per_statement`]; the caller zips the
/// two and falls back to no ranges if they ever disagree. Ranges are trimmed of leading
/// whitespace/comments and trailing whitespace.
pub fn statement_byte_ranges(sql: &str) -> Vec<(usize, usize)> {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut ranges = Vec::new();
    let mut seg_start = 0usize;
    let mut i = 0usize;

    while i < n {
        match bytes[i] {
            b'\'' => i = skip_string(bytes, i, b'\''),
            b'"' => i = skip_string(bytes, i, b'"'),
            b'-' if i + 1 < n && bytes[i + 1] == b'-' => i = skip_line_comment(bytes, i),
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => i = skip_block_comment(bytes, i),
            b'$' => match skip_dollar_quote(bytes, i) {
                Some(j) => i = j,
                None => i += 1,
            },
            b';' => {
                // Include the terminating `;` in the range so a rendered slice reads as a
                // complete statement.
                if let Some(r) = trim_code_span(sql, seg_start, i + 1) {
                    ranges.push(r);
                }
                i += 1;
                seg_start = i;
            }
            _ => i += 1,
        }
    }
    // The tail after the last `;` (a file need not terminate its final statement).
    if let Some(r) = trim_code_span(sql, seg_start, n) {
        ranges.push(r);
    }
    ranges
}

/// Advance past a quoted string/identifier opened by `quote` at `open`. Handles the
/// doubled-delimiter escape (`''` / `""`). Returns the index just past the closing quote
/// (or the input end if unterminated).
fn skip_string(bytes: &[u8], open: usize, quote: u8) -> usize {
    let n = bytes.len();
    let mut i = open + 1;
    while i < n {
        if bytes[i] == quote {
            if i + 1 < n && bytes[i + 1] == quote {
                i += 2; // Escaped delimiter — stay inside the string.
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    n
}

/// Advance past a `-- …` line comment. Returns the index just past the newline (or end).
fn skip_line_comment(bytes: &[u8], open: usize) -> usize {
    let n = bytes.len();
    let mut i = open + 2;
    while i < n && bytes[i] != b'\n' {
        i += 1;
    }
    if i < n { i + 1 } else { n }
}

/// Advance past a `/* … */` block comment. Returns the index just past `*/` (or end).
fn skip_block_comment(bytes: &[u8], open: usize) -> usize {
    let n = bytes.len();
    let mut i = open + 2;
    while i + 1 < n {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    n
}

/// If a `$tag$` dollar-quote opens at `open`, return the index just past the matching
/// `$tag$` close (or the input end if unterminated). Returns `None` if `open` is not a
/// valid dollar-quote opener, so the caller treats `$` as an ordinary byte.
fn skip_dollar_quote(bytes: &[u8], open: usize) -> Option<usize> {
    let n = bytes.len();
    // Tag runs from just after the opening `$` to the next `$`; tags are [A-Za-z0-9_]*.
    let mut j = open + 1;
    while j < n && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    if j >= n || bytes[j] != b'$' {
        return None; // Not a `$…$`-delimited opener.
    }
    let tag = &bytes[open..=j]; // The full `$tag$` delimiter, reused to find the close.
    let mut i = j + 1;
    while i < n {
        if bytes[i] == b'$' && bytes[i..].starts_with(tag) {
            return Some(i + tag.len());
        }
        i += 1;
    }
    Some(n)
}

/// Trim `[start, end)` to the code it contains: skip leading whitespace and comments,
/// then drop trailing ASCII whitespace. Returns `None` if the span is empty or made up
/// entirely of whitespace/comments (so blank or comment-only segments are not counted).
fn trim_code_span(sql: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let bytes = sql.as_bytes();
    let mut i = start;
    loop {
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < end && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            i = skip_line_comment(bytes, i).min(end);
            continue;
        }
        if i + 1 < end && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i = skip_block_comment(bytes, i).min(end);
            continue;
        }
        break;
    }
    if i >= end {
        return None; // Nothing but whitespace/comments.
    }
    let mut j = end;
    while j > i && bytes[j - 1].is_ascii_whitespace() {
        j -= 1;
    }
    Some((i, j))
}

/// Extract table names from a single SQL statement.
fn extract_from_statement(stmt: &Statement, assets: &mut SqlAssets) {
    match stmt {
        // CREATE TABLE foo (...)
        // CREATE TABLE foo AS SELECT ...
        Statement::CreateTable(create) => {
            let name = object_name_to_string(&create.name);
            assets.record_output(name, AssetKind::Table);

            // If it's a CTAS, the query's FROM tables are inputs.
            if let Some(ref query) = create.query {
                extract_inputs_from_query(query, assets);
            }
        }

        // CREATE VIEW foo AS SELECT ...
        Statement::CreateView { name, query, .. } => {
            assets.record_output(object_name_to_string(name), AssetKind::Table);
            extract_inputs_from_query(query, assets);
        }

        // INSERT INTO foo SELECT ...
        Statement::Insert(Insert { table, source, .. }) => {
            if let TableObject::TableName(name) = table {
                assets.record_output(object_name_to_string(name), AssetKind::Table);
            }
            if let Some(src) = source {
                extract_inputs_from_query(src.as_ref(), assets);
            }
        }

        // COPY foo TO 'file.csv'
        // COPY foo FROM 'file.csv'
        Statement::Copy {
            source,
            target,
            options,
            ..
        } => {
            match source {
                CopySource::Table { table_name, .. } => {
                    // COPY <table> ... — table is the source being read/written
                    match target {
                        CopyTarget::File { filename } => {
                            // COPY table TO 'file' — reading the table, producing the file.
                            // The file path is a first-class produced asset (file-path
                            // lineage). Whether that name is one file or a directory of
                            // files is decided by the COPY's own options — see
                            // `copy_to_target_kind` for the enumeration and where it came
                            // from — so it is known here rather than guessed later.
                            assets
                                .record_input(object_name_to_string(table_name), AssetKind::Table);
                            assets.record_output(
                                filename.clone(),
                                copy_to_target_kind(options.as_slice()),
                            );
                        }
                        CopyTarget::Stdout => {
                            // COPY table TO STDOUT — reading from the table
                            assets
                                .record_input(object_name_to_string(table_name), AssetKind::Table);
                        }
                        CopyTarget::Stdin => {
                            // COPY table FROM STDIN — writing to the table
                            assets
                                .record_output(object_name_to_string(table_name), AssetKind::Table);
                        }
                        _ => {}
                    }
                }
                CopySource::Query(query) => {
                    extract_inputs_from_query(query, assets);
                }
            }
        }

        // DROP TABLE/VIEW — destructive operation
        Statement::Drop { names, .. } => {
            for name in names {
                assets.destroys.insert(object_name_to_string(name));
            }
        }

        // ALTER TABLE — modifies the asset (output), does not read data from it
        Statement::AlterTable { name, .. } => {
            assets.record_output(object_name_to_string(name), AssetKind::Table);
        }

        // ALTER VIEW — modifies the view (output), new query reads from tables (inputs)
        Statement::AlterView { name, query, .. } => {
            assets.record_output(object_name_to_string(name), AssetKind::Table);
            extract_inputs_from_query(query, assets);
        }

        // MERGE INTO target USING source — target is written, source is read
        Statement::Merge { table, source, .. } => {
            // Target table → outputs
            if let TableFactor::Table { name, .. } = table {
                assets.record_output(object_name_to_string(name), AssetKind::Table);
            }
            // Source table → inputs
            extract_inputs_from_table_factor(source, assets);
        }

        // SELECT ... FROM — standalone select, extract inputs
        Statement::Query(query) => {
            extract_inputs_from_query(query, assets);
        }

        // All other statements — no asset extraction
        _ => {}
    }
}

/// Extract input table names from a query (SELECT ... FROM ... JOIN ...).
/// Also collects CTE names into `assets.internal`.
fn extract_inputs_from_query(query: &sqlparser::ast::Query, assets: &mut SqlAssets) {
    extract_inputs_from_set_expr(&query.body, assets);

    // Handle CTEs — they define local names, and their queries read from tables.
    // CTE names are captured in `internal` (step-internal assets).
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            // Record the CTE name as an internal asset.
            assets.internal.insert(cte.alias.name.value.to_lowercase());
            // The CTE's body reads from tables — those are real inputs.
            extract_inputs_from_query(&cte.query, assets);
        }
    }
}

/// Recursively extract input table names from a set expression.
/// Handles SELECT, UNION/EXCEPT/INTERSECT, and nested queries.
fn extract_inputs_from_set_expr(set_expr: &sqlparser::ast::SetExpr, assets: &mut SqlAssets) {
    match set_expr {
        sqlparser::ast::SetExpr::Select(select) => {
            for table in &select.from {
                extract_inputs_from_table_factor(&table.relation, assets);
                for join in &table.joins {
                    extract_inputs_from_table_factor(&join.relation, assets);
                }
            }
        }
        sqlparser::ast::SetExpr::SetOperation { left, right, .. } => {
            extract_inputs_from_set_expr(left, assets);
            extract_inputs_from_set_expr(right, assets);
        }
        sqlparser::ast::SetExpr::Query(query) => {
            extract_inputs_from_query(query, assets);
        }
        // DuckDB statement-form PIVOT/UNPIVOT (`PIVOT t ON … USING …`): the source
        // relation being (un)pivoted is a real input, exactly as a FROM table is.
        // (Needs the vendored sqlparser fork; the SQL-standard `FROM t PIVOT (…)`
        // table-factor form is handled in `extract_inputs_from_table_factor`.)
        sqlparser::ast::SetExpr::Pivot(pivot) => {
            extract_inputs_from_table_factor(&pivot.source, assets);
        }
        sqlparser::ast::SetExpr::Unpivot(unpivot) => {
            extract_inputs_from_table_factor(&unpivot.source, assets);
        }
        // Values, Insert, Update, Table — no table references to extract.
        _ => {}
    }
}

/// Extract a table name from a table factor (FROM clause item).
fn extract_inputs_from_table_factor(factor: &TableFactor, assets: &mut SqlAssets) {
    match factor {
        // A bare table reference, or a table-valued function call (`args: Some`).
        TableFactor::Table { name, args, .. } => {
            match args {
                // `read_parquet('x.parquet')` / `read_csv([...])` etc: the function reads
                // files — lift its path literal(s) as file inputs, not the opaque fn name.
                Some(table_args) if is_file_reader(&object_name_to_string(name)) => {
                    for arg in &table_args.args {
                        if let FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) = arg {
                            extract_path_literals(expr, assets);
                        }
                    }
                }
                // Any other table-valued function (`range(…)`, `generate_series(…)`) or a
                // plain table name: record the name itself as the input, as before.
                _ => {
                    assets.record_input(object_name_to_string(name), AssetKind::Table);
                }
            }
        }
        TableFactor::Derived { subquery, .. } => {
            extract_inputs_from_query(subquery, assets);
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            extract_inputs_from_table_factor(&table_with_joins.relation, assets);
            for join in &table_with_joins.joins {
                extract_inputs_from_table_factor(&join.relation, assets);
            }
        }
        // PIVOT wraps a source table — extract the inner table as an input.
        TableFactor::Pivot { table, .. } => {
            extract_inputs_from_table_factor(table, assets);
        }
        // UNPIVOT wraps a source table — extract the inner table as an input.
        TableFactor::Unpivot { table, .. } => {
            extract_inputs_from_table_factor(table, assets);
        }
        // TableFunction, MatchRecognize, etc. — skip
        _ => {}
    }
}

/// Convert an ObjectName (potentially qualified: schema.table) to a simple string.
/// Uses the last identifier (the table name itself), lowercased for consistency.
fn object_name_to_string(name: &ObjectName) -> String {
    // ObjectName contains Vec<ObjectNamePart>; take the last part (table name).
    name.0
        .last()
        .and_then(|part| part.as_ident())
        .map(|ident| ident.value.to_lowercase())
        .unwrap_or_default()
}

/// Whether a table-valued function name is a DuckDB file reader whose first argument
/// is a path (or list of paths). Matched case-insensitively against the fn name that
/// [`object_name_to_string`] already lowercased.
fn is_file_reader(fn_name: &str) -> bool {
    const READERS: [&str; 12] = [
        "read_parquet",
        "parquet_scan",
        "read_csv",
        "read_csv_auto",
        "read_json",
        "read_json_auto",
        "read_json_objects",
        "read_ndjson",
        "read_ndjson_auto",
        "read_ndjson_objects",
        "read_text",
        "read_blob",
    ];
    READERS.contains(&fn_name)
}

/// Lift filesystem-path string literals out of a file-reader argument expression.
///
/// Handles a single quoted path (`'x.parquet'`) and a bracketed/`ARRAY` list of them
/// (`['a.json', 'b.json']`) — DuckDB's multi-file glob form. Paths keep their original
/// case (filesystems are case-sensitive); everything else is ignored, so reader options
/// like `format => 'array'` never masquerade as inputs (they arrive as named args, which
/// the caller already skips, but a stray literal is harmless).
fn extract_path_literals(expr: &Expr, assets: &mut SqlAssets) {
    match expr {
        Expr::Value(v) => {
            if let Value::SingleQuotedString(path) = &v.value {
                let kind = if is_glob(path) {
                    AssetKind::Pattern
                } else {
                    AssetKind::File
                };
                assets.record_input(path.clone(), kind);
            }
        }
        Expr::Array(array) => {
            for elem in &array.elem {
                extract_path_literals(elem, assets);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CREATE TABLE is discovered as an output.
    #[test]
    fn test_create_table_output() {
        let sql = "CREATE TABLE customers (id INT, name TEXT);";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("customers"));
        assert!(assets.inputs.is_empty());
    }

    // CREATE VIEW is discovered as an output.
    #[test]
    fn test_create_view_output() {
        let sql = "CREATE VIEW active_customers AS SELECT * FROM customers WHERE active = true;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("active_customers"));
        assert!(assets.inputs.contains("customers"));
    }

    // CREATE TABLE AS SELECT (CTAS) discovers both output and inputs.
    #[test]
    fn test_ctas_output_and_inputs() {
        let sql = "CREATE TABLE summary AS SELECT count(*) AS total FROM orders;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("summary"));
        assert!(assets.inputs.contains("orders"));
    }

    // Multiple DDL statements in one file.
    #[test]
    fn test_multiple_creates() {
        let sql = "CREATE TABLE foo (id INT);\nCREATE TABLE bar (id INT);\nCREATE VIEW baz AS SELECT * FROM foo;";
        let assets = extract_assets(sql).unwrap();
        assert_eq!(
            assets.outputs,
            BTreeSet::from(["foo".into(), "bar".into(), "baz".into()])
        );
        assert!(assets.inputs.contains("foo"));
    }

    // FROM clause tables are discovered as inputs.
    #[test]
    fn test_from_clause_inputs() {
        let sql = "SELECT * FROM customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.outputs.is_empty());
    }

    // JOIN tables are discovered as inputs.
    #[test]
    fn test_join_inputs() {
        let sql = "SELECT c.name, o.total FROM customers c JOIN orders o ON c.id = o.customer_id;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("orders"));
    }

    // Subqueries in FROM clause.
    #[test]
    fn test_subquery_inputs() {
        let sql = "SELECT * FROM (SELECT * FROM raw_data) sub;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("raw_data"));
    }

    // INSERT INTO is discovered as an output.
    #[test]
    fn test_insert_into_output() {
        let sql = "INSERT INTO summary SELECT count(*) FROM customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("summary"));
        assert!(assets.inputs.contains("customers"));
    }

    // COPY TO reads from a table (input).
    #[test]
    fn test_copy_to_file() {
        let sql = "COPY customers TO 'customers.csv';";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
    }

    // Unparseable SQL returns an error (caller treats as opaque).
    #[test]
    fn test_unparseable_sql() {
        let sql = "THIS IS NOT VALID SQL AT ALL %%%";
        let result = extract_assets(sql);
        assert!(result.is_err());
    }

    // UNION ALL discovers inputs from both branches.
    #[test]
    fn test_union_all_inputs() {
        let sql = "SELECT * FROM customers UNION ALL SELECT * FROM archived_customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("archived_customers"));
    }

    // CTAS with UNION discovers output and all inputs.
    #[test]
    fn test_ctas_union_inputs() {
        let sql = "CREATE TABLE all_customers AS SELECT * FROM customers UNION ALL SELECT * FROM archived_customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("all_customers"));
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("archived_customers"));
    }

    // EXCEPT discovers inputs from both sides.
    #[test]
    fn test_except_inputs() {
        let sql = "SELECT id FROM customers EXCEPT SELECT id FROM blocklist;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("customers"));
        assert!(assets.inputs.contains("blocklist"));
    }

    // CTE names go to internal, not inputs.
    #[test]
    fn test_cte_internal_not_inputs() {
        let sql =
            "WITH recent AS (SELECT * FROM orders WHERE date > '2026-01-01') SELECT * FROM recent;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.inputs.contains("orders"),
            "real table should be in inputs"
        );
        assert!(
            !assets.inputs.contains("recent"),
            "CTE name should NOT be in inputs"
        );
        assert!(
            assets.internal.contains("recent"),
            "CTE name should be in internal"
        );
    }

    // Edge case: Qualified table names use the last component.
    #[test]
    fn test_qualified_name() {
        let sql = "CREATE TABLE main.customers (id INT);";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.outputs.contains("customers"));
    }

    // Edge case: Empty SQL produces empty assets.
    #[test]
    fn test_empty_sql() {
        // sqlparser may reject empty input, so use a comment-only file
        let sql = "-- just a comment";
        // This may either parse as empty or error — both are acceptable
        let result = extract_assets(sql);
        // An Err is also acceptable — comment-only input is treated as opaque.
        if let Ok(assets) = result {
            assert!(assets.outputs.is_empty());
            assert!(assets.inputs.is_empty());
        }
    }

    // Nested CTEs — both captured in internal.
    #[test]
    fn test_nested_ctes_in_internal() {
        let sql = "WITH a AS (SELECT * FROM raw_data), b AS (SELECT * FROM a) SELECT * FROM b;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.internal.contains("a"),
            "CTE 'a' should be in internal"
        );
        assert!(
            assets.internal.contains("b"),
            "CTE 'b' should be in internal"
        );
        assert!(
            assets.inputs.contains("raw_data"),
            "real table should be in inputs"
        );
        assert!(
            !assets.inputs.contains("a"),
            "CTE 'a' should NOT be in inputs"
        );
        assert!(
            !assets.inputs.contains("b"),
            "CTE 'b' should NOT be in inputs"
        );
    }

    // CTE name shadowing a real table — CTE goes to internal, real table stays in inputs.
    #[test]
    fn test_cte_shadows_real_table() {
        let sql = "WITH customers AS (SELECT * FROM raw_customers) SELECT * FROM customers;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.internal.contains("customers"),
            "CTE 'customers' should be in internal"
        );
        assert!(
            assets.inputs.contains("raw_customers"),
            "real table should be in inputs"
        );
        assert!(
            !assets.inputs.contains("customers"),
            "CTE 'customers' should NOT be in inputs"
        );
    }

    // DROP TABLE populates destroys.
    #[test]
    fn test_drop_table_destroys() {
        let sql = "DROP TABLE foo;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.destroys.contains("foo"),
            "dropped table should be in destroys"
        );
        assert!(assets.outputs.is_empty(), "drop should not add to outputs");
        assert!(assets.inputs.is_empty(), "drop should not add to inputs");
    }

    // DROP VIEW also populates destroys.
    #[test]
    fn test_drop_view_destroys() {
        let sql = "DROP VIEW IF EXISTS my_view;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.destroys.contains("my_view"),
            "dropped view should be in destroys"
        );
    }

    // DROP + CREATE in same file — both destroys and outputs populated.
    #[test]
    fn test_drop_then_create() {
        let sql = "DROP TABLE IF EXISTS foo; CREATE TABLE foo AS SELECT * FROM bar;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.destroys.contains("foo"),
            "dropped table should be in destroys"
        );
        assert!(
            assets.outputs.contains("foo"),
            "created table should be in outputs"
        );
        assert!(
            assets.inputs.contains("bar"),
            "source table should be in inputs"
        );
    }

    // ALTER TABLE populates outputs only.
    #[test]
    fn test_alter_table_outputs_only() {
        let sql = "ALTER TABLE customers ADD COLUMN email TEXT;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.outputs.contains("customers"),
            "altered table should be in outputs"
        );
        assert!(assets.inputs.is_empty(), "alter should not add to inputs");
    }

    // MERGE INTO — target in outputs, source in inputs.
    #[test]
    fn test_merge_into() {
        let sql = "MERGE INTO target USING source ON target.id = source.id WHEN MATCHED THEN UPDATE SET target.name = source.name;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.outputs.contains("target"),
            "merge target should be in outputs"
        );
        assert!(
            assets.inputs.contains("source"),
            "merge source should be in inputs"
        );
    }

    // CREATE OR REPLACE TABLE is handled as output.
    #[test]
    fn test_create_or_replace() {
        let sql = "CREATE OR REPLACE TABLE foo AS SELECT * FROM bar;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.outputs.contains("foo"),
            "replaced table should be in outputs"
        );
        assert!(
            assets.inputs.contains("bar"),
            "source table should be in inputs"
        );
    }

    // PIVOT source table is extracted as input.
    #[test]
    fn test_pivot_source_table() {
        // sqlparser-rs 0.55 supports PIVOT syntax
        let sql =
            "SELECT * FROM monthly_sales PIVOT (SUM(amount) FOR month IN ('Jan', 'Feb', 'Mar'));";
        let result = extract_assets(sql);
        match result {
            Ok(assets) => {
                assert!(
                    assets.inputs.contains("monthly_sales"),
                    "pivot source should be in inputs"
                );
            }
            Err(_) => {
                // If sqlparser doesn't support this syntax, graceful degradation is acceptable
            }
        }
    }

    // UNPIVOT source table is extracted as input.
    #[test]
    fn test_unpivot_source_table() {
        let sql = "SELECT * FROM quarterly_report UNPIVOT (value FOR quarter IN (q1, q2, q3, q4));";
        let result = extract_assets(sql);
        match result {
            Ok(assets) => {
                assert!(
                    assets.inputs.contains("quarterly_report"),
                    "unpivot source should be in inputs"
                );
            }
            Err(_) => {
                // If sqlparser doesn't support this syntax, graceful degradation is acceptable
            }
        }
    }

    // Edge case: Recursive CTE — self-reference within CTE body.
    #[test]
    fn test_recursive_cte() {
        let sql = "WITH RECURSIVE tree AS (SELECT id, parent_id FROM nodes WHERE parent_id IS NULL UNION ALL SELECT n.id, n.parent_id FROM nodes n JOIN tree t ON n.parent_id = t.id) SELECT * FROM tree;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.internal.contains("tree"),
            "recursive CTE should be in internal"
        );
        assert!(
            assets.inputs.contains("nodes"),
            "real table should be in inputs"
        );
        assert!(
            !assets.inputs.contains("tree"),
            "CTE should NOT be in inputs"
        );
    }

    // Edge case: CTE with subquery — inner subquery tables discovered.
    #[test]
    fn test_cte_with_subquery() {
        let sql = "WITH a AS (SELECT * FROM (SELECT * FROM raw) sub) SELECT * FROM a;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.internal.contains("a"), "CTE should be in internal");
        assert!(
            assets.inputs.contains("raw"),
            "subquery source should be in inputs"
        );
        assert!(!assets.inputs.contains("a"), "CTE should NOT be in inputs");
    }

    // Edge case: ALTER VIEW — modifies view (output), reads from tables (inputs).
    #[test]
    fn test_alter_view_outputs_and_inputs() {
        let sql = "ALTER VIEW active_customers AS SELECT * FROM customers WHERE active = true;";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.outputs.contains("active_customers"),
            "altered view should be in outputs"
        );
        assert!(
            assets.inputs.contains("customers"),
            "source table should be in inputs"
        );
    }

    // Edge case: DROP multiple tables in one statement.
    #[test]
    fn test_drop_multiple_tables() {
        let sql = "DROP TABLE foo, bar, baz;";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.destroys.contains("foo"));
        assert!(assets.destroys.contains("bar"));
        assert!(assets.destroys.contains("baz"));
    }

    // read_parquet('path') contributes the *file path* as input, not the fn name.
    #[test]
    fn test_read_parquet_lifts_path() {
        let sql = "CREATE TABLE t AS SELECT * FROM read_parquet('build/edgar.parquet');";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.inputs.contains("build/edgar.parquet"),
            "path is the input"
        );
        assert!(
            !assets.inputs.contains("read_parquet"),
            "fn name is not an input"
        );
        assert!(assets.outputs.contains("t"));
    }

    // read_csv keeps original case in the path (filesystems are case-sensitive).
    #[test]
    fn test_read_csv_preserves_case() {
        let sql = "SELECT * FROM read_csv('Data/Raw/GLEIF.csv');";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.inputs.contains("Data/Raw/GLEIF.csv"),
            "path case preserved"
        );
        assert!(!assets.inputs.contains("read_csv"));
    }

    // read_json over a list of files lifts every path; named options are ignored.
    #[test]
    fn test_read_json_list_and_options() {
        let sql = "CREATE TABLE brew AS SELECT * FROM read_json(['a/30d.json', 'a/90d.json'], format = 'array');";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("a/30d.json"), "first path lifted");
        assert!(assets.inputs.contains("a/90d.json"), "second path lifted");
        assert!(
            !assets.inputs.contains("read_json"),
            "fn name is not an input"
        );
        assert!(
            !assets.inputs.contains("array"),
            "option value is not an input"
        );
        assert!(assets.outputs.contains("brew"));
    }

    // COPY <table> TO 'file' produces the file path as an output (file-path lineage).
    #[test]
    fn test_copy_to_produces_file() {
        let sql = "COPY ranking TO 'data/ranking.parquet';";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("ranking"), "table is read");
        assert!(
            assets.outputs.contains("data/ranking.parquet"),
            "file is produced"
        );
    }

    // a non-file table function keeps recording its name (unchanged behaviour).
    #[test]
    fn test_non_file_table_function_unchanged() {
        let sql = "SELECT * FROM generate_series(1, 10);";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.inputs.contains("generate_series"),
            "non-file TVF name still recorded"
        );
    }

    // Byte ranges: one range per top-level statement, each slicing its own source.
    #[test]
    fn byte_ranges_split_top_level_statements() {
        let sql = "CREATE TABLE foo (id INT);\nSELECT * FROM foo;";
        let ranges = statement_byte_ranges(sql);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&sql[ranges[0].0..ranges[0].1], "CREATE TABLE foo (id INT);");
        assert_eq!(&sql[ranges[1].0..ranges[1].1], "SELECT * FROM foo;");
        // Ranges align 1:1 with the parsed statements.
        assert_eq!(ranges.len(), extract_per_statement(sql).unwrap().len());
    }

    // Byte ranges: a `;` inside a string literal must not split the statement.
    #[test]
    fn byte_ranges_ignore_semicolons_in_strings() {
        let sql = "INSERT INTO t VALUES ('a;b;c');";
        let ranges = statement_byte_ranges(sql);
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            &sql[ranges[0].0..ranges[0].1],
            "INSERT INTO t VALUES ('a;b;c');"
        );
    }

    // Byte ranges: comment-only and blank segments are dropped, not counted.
    #[test]
    fn byte_ranges_drop_comment_and_blank_segments() {
        let sql = "-- header comment\nSELECT 1; /* trailing */ \n\n";
        let ranges = statement_byte_ranges(sql);
        assert_eq!(ranges.len(), 1);
        // Range starts at the code, past the leading comment, and trims trailing space.
        assert_eq!(&sql[ranges[0].0..ranges[0].1], "SELECT 1;");
    }

    // Byte ranges: a `;` inside a `--` line comment does not split.
    #[test]
    fn byte_ranges_ignore_semicolons_in_line_comments() {
        let sql = "SELECT 1 -- a; b; c\nFROM t;";
        let ranges = statement_byte_ranges(sql);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&sql[ranges[0].0..ranges[0].1], sql.trim_end_matches('\n'));
    }

    // Byte ranges: a `;` inside a dollar-quoted body does not split.
    #[test]
    fn byte_ranges_ignore_semicolons_in_dollar_quotes() {
        let sql = "SELECT $$a; b; c$$ AS s;";
        let ranges = statement_byte_ranges(sql);
        assert_eq!(ranges.len(), 1);
        assert_eq!(&sql[ranges[0].0..ranges[0].1], "SELECT $$a; b; c$$ AS s;");
    }

    // ---- DuckDB statement-form PIVOT/UNPIVOT + multi-option COPY. ----
    // These forms need the vendored sqlparser fork; without it the parse fails and the
    // whole step degrades to an opaque node (see AssetGraph::build), defeating the
    // structural-transparency principle.

    // Statement-form PIVOT over a real table lifts the source as an input, not an
    // opaque step.
    #[test]
    fn test_pivot_statement_lifts_source() {
        let sql = "PIVOT monthly_sales ON month USING SUM(amount) GROUP BY country;";
        let assets = extract_assets(sql).expect("statement-form PIVOT must parse");
        assert!(
            assets.inputs.contains("monthly_sales"),
            "pivot source should be an input, got {:?}",
            assets.inputs
        );
    }

    // PIVOT as a CTAS body — output AND source input are both discovered.
    #[test]
    fn test_pivot_statement_as_ctas_body() {
        let sql =
            "CREATE OR REPLACE TABLE installs AS PIVOT wide_sales ON days USING SUM(installs);";
        let assets = extract_assets(sql).expect("CTAS over a PIVOT must parse");
        assert!(
            assets.outputs.contains("installs"),
            "CTAS output discovered"
        );
        assert!(
            assets.inputs.contains("wide_sales"),
            "pivot source lifted as input, got {:?}",
            assets.inputs
        );
    }

    // The brewtrend shape — CTAS + WITH + PIVOT over the CTE. The CTE name is filtered
    // out of inputs; the CTE's own source table is the real input.
    #[test]
    fn test_pivot_ctas_with_cte_shape() {
        let sql = "CREATE OR REPLACE TABLE installs AS \
                   WITH install_counts AS (SELECT category, name, days, installs FROM categories) \
                   PIVOT install_counts ON days USING SUM(installs);";
        let assets = extract_assets(sql).expect("brewtrend-shape PIVOT must parse");
        assert!(assets.outputs.contains("installs"), "output discovered");
        assert!(
            assets.inputs.contains("categories"),
            "real underlying table is the input, got {:?}",
            assets.inputs
        );
        assert!(
            !assets.inputs.contains("install_counts"),
            "the pivoted CTE name is internal, not an external input"
        );
        assert!(
            assets.internal.contains("install_counts"),
            "CTE tracked as internal"
        );
    }

    // Statement-form UNPIVOT lifts the source as an input.
    #[test]
    fn test_unpivot_statement_lifts_source() {
        let sql = "UNPIVOT quarterly_report ON q1, q2, q3, q4 INTO NAME quarter VALUE amount;";
        let assets = extract_assets(sql).expect("statement-form UNPIVOT must parse");
        assert!(
            assets.inputs.contains("quarterly_report"),
            "unpivot source should be an input, got {:?}",
            assets.inputs
        );
    }

    // UNPIVOT with the shorthand (no INTO clause) still parses + lifts source.
    #[test]
    fn test_unpivot_statement_shorthand() {
        let sql = "UNPIVOT sensor_readings ON temp, humidity, pressure;";
        let assets = extract_assets(sql).expect("shorthand UNPIVOT must parse");
        assert!(assets.inputs.contains("sensor_readings"), "source lifted");
    }

    // Multi-option COPY parses; the table is read and the file is produced, and the
    // extra DuckDB options (COMPRESSION) do not break introspection.
    #[test]
    fn test_multi_option_copy() {
        let sql = "COPY ranking TO 'data/ranking.parquet' (FORMAT parquet, COMPRESSION zstd);";
        let assets = extract_assets(sql).expect("multi-option COPY must parse");
        assert!(assets.inputs.contains("ranking"), "table is read");
        assert!(
            assets.outputs.contains("data/ranking.parquet"),
            "file is produced (file-path lineage), got {:?}",
            assets.outputs
        );
    }

    // COPY with a parenthesized PARTITION_BY value list also parses.
    #[test]
    fn test_copy_partition_by() {
        let sql = "COPY orders TO 'out/orders' (FORMAT parquet, PARTITION_BY (year, month), OVERWRITE_OR_IGNORE);";
        let assets = extract_assets(sql).expect("COPY with PARTITION_BY must parse");
        assert!(assets.inputs.contains("orders"), "table is read");
        assert!(
            assets.outputs.contains("out/orders"),
            "output path produced"
        );
        // PARTITION_BY makes DuckDB write a directory of Hive-partitioned files under
        // this name, not one file — the COPY's own options say so, so this is known
        // here rather than guessed later from the string or the filesystem.
        assert_eq!(
            assets.kinds.get("out/orders"),
            Some(&AssetKind::Directory),
            "PARTITION_BY target must be classified as a directory, not a file"
        );
    }

    // A COPY … TO carrying none of `copy_to_target_kind`'s directory-writing options
    // writes one file — it must not be classified a directory just for sharing the
    // COPY statement shape.
    #[test]
    fn test_copy_without_a_directory_writing_option_is_a_file() {
        let sql = "COPY orders TO 'out/orders.parquet' (FORMAT parquet);";
        let assets = extract_assets(sql).expect("plain COPY must parse");
        assert_eq!(
            assets.kinds.get("out/orders.parquet"),
            Some(&AssetKind::File)
        );
    }

    // The other three directory-writing options, each on its own. Until this round
    // only PARTITION_BY was tested for, and the comment above this test asserted that
    // a COPY without it "writes exactly one file" — false on the DuckDB this crate
    // links: PER_THREAD_OUTPUT and FILE_SIZE_BYTES each wrote a directory, the
    // File-kind classification then made `fs::read` fail on it, and the step re-ran
    // forever while warning that nothing had been produced.
    #[test]
    fn test_per_thread_output_target_is_a_directory() {
        let sql = "COPY orders TO 'out/pto' (FORMAT parquet, PER_THREAD_OUTPUT true);";
        let assets = extract_assets(sql).expect("PER_THREAD_OUTPUT COPY must parse");
        assert_eq!(assets.kinds.get("out/pto"), Some(&AssetKind::Directory));
    }

    #[test]
    fn test_per_thread_output_bare_flag_is_a_directory() {
        let sql = "COPY orders TO 'out/pto' (FORMAT parquet, PER_THREAD_OUTPUT);";
        let assets = extract_assets(sql).expect("bare-flag COPY must parse");
        assert_eq!(assets.kinds.get("out/pto"), Some(&AssetKind::Directory));
    }

    // DuckDB's own `GetBooleanArg` reads an explicit `false` as off, so this one
    // really does write a single file and classifying it a directory would send the
    // step into the same perpetual re-run from the other side.
    #[test]
    fn test_per_thread_output_false_is_a_file() {
        let sql = "COPY orders TO 'out/one.parquet' (FORMAT parquet, PER_THREAD_OUTPUT false);";
        let assets = extract_assets(sql).expect("PER_THREAD_OUTPUT false COPY must parse");
        assert_eq!(assets.kinds.get("out/one.parquet"), Some(&AssetKind::File));
    }

    // `GetBooleanArg` CASTS its argument to BOOLEAN; it does not compare it against
    // the `false` keyword. Each of these spellings wrote a single 198-byte parquet
    // file when driven on the DuckDB CLI at v1.5.4 and at v1.5.5, and each was
    // classified `Directory` here until this round — `read_dir` on a regular file
    // returns `None`, so the step re-ran on every run while warning that it had
    // produced nothing.
    #[test]
    fn test_per_thread_output_cast_to_false_is_a_file() {
        for arg in ["0", "'false'", "'FALSE'", "'no'", "'f'"] {
            let sql = format!(
                "COPY orders TO 'out/one.parquet' (FORMAT parquet, PER_THREAD_OUTPUT {arg});"
            );
            let assets = extract_assets(&sql).expect("COPY must parse");
            assert_eq!(
                assets.kinds.get("out/one.parquet"),
                Some(&AssetKind::File),
                "PER_THREAD_OUTPUT {arg} casts to false and writes one file"
            );
        }
    }

    // The same cast in the other direction, so the arm above cannot be satisfied by
    // reading every PER_THREAD_OUTPUT argument as off. Each of these wrote a
    // directory on both engines.
    #[test]
    fn test_per_thread_output_cast_to_true_is_a_directory() {
        for arg in ["1", "'true'", "'yes'", "'t'", "'Y'"] {
            let sql =
                format!("COPY orders TO 'out/pto' (FORMAT parquet, PER_THREAD_OUTPUT {arg});");
            let assets = extract_assets(&sql).expect("COPY must parse");
            assert_eq!(
                assets.kinds.get("out/pto"),
                Some(&AssetKind::Directory),
                "PER_THREAD_OUTPUT {arg} casts to true and writes a directory"
            );
        }
    }

    // The cast belongs to PER_THREAD_OUTPUT alone. `rotate` is set from
    // `file_size_bytes.IsValid()`, not from a boolean, so a zero here is still on —
    // `FILE_SIZE_BYTES 0` wrote a directory on both engines. Applying the boolean
    // cast to all four names uniformly would get this one wrong.
    #[test]
    fn test_file_size_bytes_zero_is_still_a_directory() {
        let sql = "COPY orders TO 'out/sized' (FORMAT parquet, FILE_SIZE_BYTES 0);";
        let assets = extract_assets(sql).expect("FILE_SIZE_BYTES 0 COPY must parse");
        assert_eq!(assets.kinds.get("out/sized"), Some(&AssetKind::Directory));
    }

    #[test]
    fn test_file_size_bytes_target_is_a_directory() {
        let sql = "COPY orders TO 'out/sized' (FORMAT parquet, FILE_SIZE_BYTES '1MB');";
        let assets = extract_assets(sql).expect("FILE_SIZE_BYTES COPY must parse");
        assert_eq!(assets.kinds.get("out/sized"), Some(&AssetKind::Directory));
    }

    #[test]
    fn test_row_groups_per_file_target_is_a_directory() {
        let sql = "COPY orders TO 'out/rgpf' (FORMAT parquet, ROW_GROUPS_PER_FILE 1);";
        let assets = extract_assets(sql).expect("ROW_GROUPS_PER_FILE COPY must parse");
        assert_eq!(assets.kinds.get("out/rgpf"), Some(&AssetKind::Directory));
    }

    // An option NOT in the directory-writing set must not flip the classification,
    // however directory-ish it reads: FILENAME_PATTERN and FILE_EXTENSION only shape
    // the names DuckDB uses once something else has already made the target a
    // directory, and OVERWRITE only decides what happens to what is already there.
    #[test]
    fn test_neighbouring_copy_options_do_not_make_a_directory() {
        for sql in [
            "COPY orders TO 'out/o.parquet' (FORMAT parquet, FILENAME_PATTERN 'part_{i}');",
            "COPY orders TO 'out/o.parquet' (FORMAT parquet, FILE_EXTENSION 'pq');",
            "COPY orders TO 'out/o.parquet' (FORMAT parquet, OVERWRITE_OR_IGNORE);",
            "COPY orders TO 'out/o.parquet' (FORMAT parquet, ROW_GROUP_SIZE 100000);",
        ] {
            let assets = extract_assets(sql).expect("COPY must parse");
            assert_eq!(
                assets.kinds.get("out/o.parquet"),
                Some(&AssetKind::File),
                "not a directory-writing option: {sql}"
            );
        }
    }

    // ---- DuckDB's Python-style `lambda x: expr` lambda syntax. ----
    // DuckDB is retiring the single-arrow lambda; without this the fork, a model
    // using the new form fails to parse and the whole step degrades to an opaque
    // node (see AssetGraph::build), contributing no assets to the lineage graph.

    // A CTAS whose SELECT list uses `lambda c: ...` still parses and still
    // discovers both the output and the FROM-clause input — the lambda sits in
    // an expression that asset extraction never inspects, so the only way it
    // can affect `assets` at all is by breaking the parse.
    #[test]
    fn test_lambda_colon_single_param_does_not_block_introspection() {
        let sql = "CREATE TABLE bumped AS \
                   SELECT list_transform([x], lambda c: c + 1) AS y FROM source_table;";
        let assets = extract_assets(sql).expect("lambda colon syntax must parse");
        assert!(assets.outputs.contains("bumped"), "CTAS output discovered");
        assert!(
            assets.inputs.contains("source_table"),
            "FROM table still discovered as input, got {:?}",
            assets.inputs
        );
    }

    // The multi-param colon form (`lambda acc, v: ...`, no parens) parses too.
    #[test]
    fn test_lambda_colon_multi_param_does_not_block_introspection() {
        let sql = "CREATE TABLE totals AS \
                   SELECT list_reduce(xs, lambda acc, v: acc + v) AS total FROM source_table;";
        let assets = extract_assets(sql).expect("multi-param lambda colon syntax must parse");
        assert!(assets.outputs.contains("totals"));
        assert!(assets.inputs.contains("source_table"));
    }
}
