//! SQL introspection: what a SQL step reads and produces.
//!
//! Two readers take a step apart. By default sqlparser-rs parses it with its DuckDB
//! dialect. With `ARC_SQL_READER=duckdb` DuckDB's own parse is read instead, through
//! [`crate::duckdb_lineage`], from the DuckDB arc runs, which has to be a 2.0 build. Either
//! reader's statements pass through the same rules here: which table function reads a
//! file, which reads no table, which one's arguments arc does not read, and whether a
//! `COPY … TO` writes a file or a directory.
//!
//! Extracted from each statement:
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
use std::ffi::OsString;
use std::ops::ControlFlow;

use sqlparser::ast::{
    CopyOption, CopySource, CopyTarget, Expr, FunctionArg, FunctionArgExpr, Insert, ObjectName,
    Statement, TableFactor, TableFunctionArgs, TableObject, Value, visit_expressions,
};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;

use crate::asset_kind::AssetKind;
use crate::duckdb_lineage::{self, ArgValue, Relation, TableCall};

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
    /// Table-valued functions this SQL calls with a string or a quoted identifier among
    /// their arguments, other than the file readers and row-generators this layer knows —
    /// `mlpack_random_forest_train("X", "Y", "params", "model")`, `query('SELECT …')`. What
    /// such an argument names depends on the function's signature, which arc does not
    /// carry, so the call records no input at all, not even one under the function's own
    /// name: the function is listed here for `AssetGraph::build` to ask the step for a
    /// `depends_on:` and a `produces:`. Lowercased; a function called twice is listed once.
    pub unread_table_functions: BTreeSet<String>,
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

/// What a path a statement reads names: a glob pattern, or one file.
fn read_path_kind(path: &str) -> AssetKind {
    if is_glob(path) {
        AssetKind::Pattern
    } else {
        AssetKind::File
    }
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

/// A `COPY … TO` option's value, as either reader gives it: what
/// [`copy_option_is_on`] reads of it.
#[derive(Debug, Clone, PartialEq)]
enum CopyOptionValue {
    /// The option is given without a value: `PER_THREAD_OUTPUT`.
    Absent,
    Boolean(bool),
    Number(String),
    String(String),
    /// `()`.
    EmptyList,
    /// Anything else: a bare word, a list with something in it, an expression.
    Other,
}

/// What a `COPY … TO 'filename'` writes, decided from the statement's own options
/// against [`DIRECTORY_WRITING_COPY_OPTIONS`].
fn copy_to_target_kind(options: &[(String, CopyOptionValue)]) -> AssetKind {
    let writes_a_directory = options.iter().any(|(name, value)| {
        DIRECTORY_WRITING_COPY_OPTIONS
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
            && copy_option_is_on(name, value)
    });
    if writes_a_directory {
        AssetKind::Directory
    } else {
        AssetKind::File
    }
}

/// sqlparser's `COPY` options, as [`copy_to_target_kind`] reads them.
fn sqlparser_copy_options(options: &[CopyOption]) -> Vec<(String, CopyOptionValue)> {
    let value = |value: &Option<Expr>| match value {
        None => CopyOptionValue::Absent,
        Some(Expr::Tuple(items)) if items.is_empty() => CopyOptionValue::EmptyList,
        Some(Expr::Value(v)) => match &v.value {
            Value::Boolean(b) => CopyOptionValue::Boolean(*b),
            Value::Number(n, _) => CopyOptionValue::Number(n.clone()),
            Value::SingleQuotedString(s)
            | Value::DoubleQuotedString(s)
            | Value::TripleSingleQuotedString(s)
            | Value::TripleDoubleQuotedString(s) => CopyOptionValue::String(s.clone()),
            _ => CopyOptionValue::Other,
        },
        Some(_) => CopyOptionValue::Other,
    };
    options
        .iter()
        .filter_map(|opt| match opt {
            CopyOption::DuckDbOption { name, value: v } => Some((name.value.clone(), value(v))),
            _ => None,
        })
        .collect()
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
fn copy_option_is_on(name: &str, value: &CopyOptionValue) -> bool {
    if name.eq_ignore_ascii_case("PARTITION_BY") {
        *value != CopyOptionValue::EmptyList
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
/// certifying an artifact. A value that is not a literal is not something this can
/// evaluate, and stays `true` for the same reason.
fn boolean_arg(value: &CopyOptionValue) -> bool {
    match value {
        CopyOptionValue::Boolean(b) => *b,
        CopyOptionValue::Number(n) => n.parse::<f64>().map(|x| x != 0.0).unwrap_or(true),
        CopyOptionValue::String(s) => cast_string_to_bool(s).unwrap_or(true),
        CopyOptionValue::Absent | CopyOptionValue::EmptyList | CopyOptionValue::Other => true,
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

/// Names the reader that takes a SQL step's reads and produces. Unset, sqlparser-rs
/// reads the step; `duckdb` reads DuckDB's own parse of it.
pub(crate) const SQL_READER_ENV: &str = "ARC_SQL_READER";

/// The value of [`SQL_READER_ENV`] that chooses DuckDB's parse.
const DUCKDB_READER: &str = "duckdb";

/// Which reader [`SQL_READER_ENV`] chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reader {
    Sqlparser,
    DuckDb,
}

fn reader() -> Result<Reader, String> {
    match std::env::var_os(SQL_READER_ENV) {
        None => Ok(Reader::Sqlparser),
        Some(value) if value == DUCKDB_READER => Ok(Reader::DuckDb),
        Some(value) => Err(format!(
            "{SQL_READER_ENV} is '{}', and the one value it takes is `{DUCKDB_READER}`, which \
             reads a SQL step with DuckDB's own parser; unset it to read steps with arc's own",
            value.to_string_lossy()
        )),
    }
}

/// The DuckDB arc runs, which is the one asked for its parse.
fn duckdb_program() -> Result<OsString, String> {
    match crate::engine::duckdb_program().map_err(|e| e.to_string())? {
        crate::engine::DuckDbProgram::Told(path) => Ok(path.into_os_string()),
        crate::engine::DuckDbProgram::SearchPath => Ok(OsString::from("duckdb")),
    }
}

/// Refuses a run, before a step runs, when [`SQL_READER_ENV`] names no reader, or names
/// DuckDB's and the DuckDB arc runs is not a 2.0 build that gives its parse. The DuckDB is
/// asked only when the Protocol has a SQL step for it to read.
pub(crate) fn check_reader(has_sql_steps: bool) -> Result<(), String> {
    if reader()? == Reader::DuckDb && has_sql_steps {
        duckdb_lineage::read_step(&duckdb_program()?, "")
            .map_err(|refusal| format!("{SQL_READER_ENV}={DUCKDB_READER}: {refusal}"))?;
    }
    Ok(())
}

/// DuckDB's reading of each statement of `sql`, as [`SqlAssets`].
fn read_with_duckdb(sql: &str) -> Result<Vec<SqlAssets>, Vec<String>> {
    let program = duckdb_program().map_err(|e| vec![e])?;
    let reading = duckdb_lineage::read_step(&program, sql).map_err(|e| vec![e.to_string()])?;
    Ok(reading.statements.iter().map(duckdb_statement).collect())
}

/// Parse a SQL string and extract the assets it produces and consumes.
///
/// Returns `Ok(SqlAssets)` on success, or `Err(warnings)` if the SQL
/// cannot be parsed. The caller should treat parse failures as opaque
/// steps (warn, don't block).
pub fn extract_assets(sql: &str) -> Result<SqlAssets, Vec<String>> {
    if reader().map_err(|e| vec![e])? == Reader::DuckDb {
        // DuckDB's reading leaves a `WITH` name out of the reads of the statement it is
        // in scope in, so nothing is filtered here.
        let mut assets = SqlAssets::default();
        for statement in read_with_duckdb(sql)? {
            assets.outputs.extend(statement.outputs);
            assets.inputs.extend(statement.inputs);
            assets.kinds.extend(statement.kinds);
            assets
                .unread_table_functions
                .extend(statement.unread_table_functions);
        }
        return Ok(assets);
    }

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
    if reader().map_err(|e| vec![e])? == Reader::DuckDb {
        return read_with_duckdb(sql);
    }
    sqlparser_per_statement(sql)
}

/// [`extract_per_statement`] with sqlparser-rs as the reader.
fn sqlparser_per_statement(sql: &str) -> Result<Vec<SqlAssets>, Vec<String>> {
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

/// One statement of DuckDB's reading, through the rules sqlparser's statements pass
/// through. A table is named by its last part, lowercased, as [`object_name_to_string`]
/// names one. A statement the reader could not read keeps what its words made certain.
fn duckdb_statement(statement: &duckdb_lineage::Statement) -> SqlAssets {
    let mut assets = SqlAssets::default();
    let options: Vec<(String, CopyOptionValue)> = statement
        .options
        .iter()
        .map(|option| (option.name.clone(), duckdb_copy_option(&option.value)))
        .collect();
    for produced in &statement.produces {
        match produced {
            Relation::Table(table) => {
                assets.record_output(table.name.to_lowercase(), AssetKind::Table)
            }
            // The one file a statement produces is a `COPY … TO` target.
            Relation::File(path) => {
                assets.record_output(path.clone(), copy_to_target_kind(&options))
            }
        }
    }
    for read in &statement.reads {
        match read {
            Relation::Table(table) => {
                assets.record_input(table.name.to_lowercase(), AssetKind::Table)
            }
            Relation::File(path) => assets.record_input(path.clone(), read_path_kind(path)),
        }
    }
    for call in &statement.calls {
        record_table_call(&duckdb_call(call), &mut assets);
    }
    assets
}

/// A DuckDB `COPY` option's value, as [`copy_to_target_kind`] reads it.
fn duckdb_copy_option(value: &Option<ArgValue>) -> CopyOptionValue {
    match value {
        None => CopyOptionValue::Absent,
        Some(ArgValue::Identifier(word)) if word.eq_ignore_ascii_case("true") => {
            CopyOptionValue::Boolean(true)
        }
        Some(ArgValue::Identifier(word)) if word.eq_ignore_ascii_case("false") => {
            CopyOptionValue::Boolean(false)
        }
        Some(ArgValue::Number(n)) => CopyOptionValue::Number(n.clone()),
        Some(ArgValue::String(s)) => CopyOptionValue::String(s.clone()),
        Some(ArgValue::List(items)) if items.is_empty() => CopyOptionValue::EmptyList,
        Some(_) => CopyOptionValue::Other,
    }
}

/// A DuckDB table function call, as [`record_table_call`] reads it.
fn duckdb_call(call: &TableCall) -> FunctionCall {
    fn paths(value: &ArgValue, out: &mut Vec<String>) {
        match value {
            ArgValue::String(path) => out.push(path.clone()),
            ArgValue::List(items) => items.iter().for_each(|item| paths(item, out)),
            _ => {}
        }
    }
    fn names_something(value: &ArgValue) -> bool {
        match value {
            ArgValue::String(_) | ArgValue::QuotedIdentifier(_) => true,
            ArgValue::List(items) => items.iter().any(names_something),
            ArgValue::Expression { quotes, .. } => *quotes,
            // A query's strings are its own: they name nothing the call reads.
            ArgValue::Query | ArgValue::Number(_) | ArgValue::Identifier(_) => false,
        }
    }
    let mut unnamed_paths = Vec::new();
    for arg in call.args.iter().filter(|arg| arg.name.is_none()) {
        paths(&arg.value, &mut unnamed_paths);
    }
    FunctionCall {
        function: call.function.to_lowercase(),
        paths: unnamed_paths,
        names_something: call.args.iter().any(|arg| names_something(&arg.value)),
        reads_a_query: call
            .args
            .iter()
            .any(|arg| matches!(arg.value, ArgValue::Query)),
    }
}

/// A table function call, as either reader gives it: what [`record_table_call`] reads.
struct FunctionCall {
    /// Lowercased, without a schema.
    function: String,
    /// The strings among its unnamed arguments, and in the lists among them: the paths a
    /// file reader reads.
    paths: Vec<String>,
    /// Whether any argument, named or not, holds a string or a quoted identifier at any
    /// depth — the two forms in which a DuckDB call names a table, a file or a query
    /// (`"X"`, `'x.parquet'`, `'SELECT …'`). A call with neither (`recent()`,
    /// `range(10)`, `f(days := 7)`) names nothing arc could be missing.
    names_something: bool,
    /// Whether any argument, named or not, is a parenthesised query:
    /// `summary((SELECT src, dst FROM edges))`. The reader has read that query's tables
    /// as the statement's own, so the call's name is not one.
    reads_a_query: bool,
}

/// What a table function call reads, by the same rules whichever reader found it.
fn record_table_call(call: &FunctionCall, assets: &mut SqlAssets) {
    if is_file_reader(&call.function) || call.function == "glob" {
        // `read_parquet('x.parquet')` / `read_csv([...])` / `glob('data/*.csv')` etc: the
        // function reads a path — file contents, or, for `glob`, the filenames it lists —
        // so its path literal(s) are file/pattern inputs, not the opaque fn name.
        for path in &call.paths {
            assets.record_input(path.clone(), read_path_kind(path));
        }
    } else if reads_no_table(&call.function) {
        // DuckDB row-generators (`range(…)`, `generate_series(…)`) and catalog
        // introspection functions (`duckdb_functions()`, `duckdb_tables()`,
        // `duckdb_secrets()`) produce rows with no backing table at all — there is
        // nothing to record as an input, and none as the fn name either.
    } else if call.names_something {
        // Any other table-valued function whose arguments hold a string or a quoted
        // identifier (an extension's `mlpack_…_train("X", "Y", …)`, `query('…')`): those
        // arguments name tables, files or queries, and arc does not carry the signature
        // that says which. The call records no input — the function's own name is not a
        // table — and is reported as unread instead.
        assets.unread_table_functions.insert(call.function.clone());
    } else if call.reads_a_query {
        // A function called on a query (`onager_pth_dijkstra((SELECT src, dst, w FROM
        // edges))`): the query's tables are what the call reads, and the reader has
        // recorded them. The function's own name is not a table.
    } else {
        // A table macro (`recent()`), or a function called with numbers and bare names
        // only: record the name itself as the input, as before.
        assets.record_input(call.function.clone(), AssetKind::Table);
    }
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
                                copy_to_target_kind(&sqlparser_copy_options(options)),
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
                    // COPY (SELECT …) TO 'file' — the query reads its tables and the
                    // COPY produces the file, the same two facts `COPY <table> TO
                    // 'file'` records above, with the target classified the same way.
                    // The parser refuses `COPY (query) FROM`, so a query source is
                    // always a `TO`.
                    extract_inputs_from_query(query, assets);
                    if let CopyTarget::File { filename } = target {
                        assets.record_output(
                            filename.clone(),
                            copy_to_target_kind(&sqlparser_copy_options(options)),
                        );
                    }
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
            let fn_name = object_name_to_string(name);
            match args {
                Some(table_args) => {
                    let mut paths = Vec::new();
                    let mut reads_a_query = false;
                    for arg in &table_args.args {
                        if let Some(query) = query_argument(arg) {
                            extract_inputs_from_query(query, assets);
                            reads_a_query = true;
                        } else if let FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) = arg {
                            path_literals(expr, &mut paths);
                        }
                    }
                    let call = FunctionCall {
                        function: fn_name,
                        paths,
                        names_something: args_name_something(table_args),
                        reads_a_query,
                    };
                    record_table_call(&call, assets);
                }
                None => assets.record_input(fn_name, AssetKind::Table),
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
    const READERS: [&str; 17] = [
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
        "read_xlsx",
        "read_xml",
        "read_dta",
        "read_stat",
        "st_read",
    ];
    READERS.contains(&fn_name)
}

/// Whether a table-valued function name produces rows with no backing table to
/// record — a row-generator (`range`, `generate_series`) or a catalog/introspection
/// function (`duckdb_functions`, `duckdb_tables`, `duckdb_secrets`). Matched
/// case-insensitively against the fn name that [`object_name_to_string`] already
/// lowercased. `glob` is not here: it reads a path pattern, handled alongside
/// [`is_file_reader`] instead.
fn reads_no_table(fn_name: &str) -> bool {
    const NO_TABLE: [&str; 5] = [
        "range",
        "generate_series",
        "duckdb_functions",
        "duckdb_tables",
        "duckdb_secrets",
    ];
    NO_TABLE.contains(&fn_name)
}

/// Whether a table-valued function's arguments hold a string literal or a quoted
/// identifier, at any depth — the two forms in which a DuckDB call names a table, a file
/// or a query (`"X"`, `'x.parquet'`, `'SELECT …'`). A call with neither
/// (`recent()`, `range(10)`, `f(days := 7)`) names nothing arc could be missing.
///
/// A query among the arguments (see [`query_argument`]) is not walked: its strings are
/// its own (`WHERE kind = 'road'`), and its tables are read, not named.
fn args_name_something(table_args: &TableFunctionArgs) -> bool {
    table_args
        .args
        .iter()
        .filter(|arg| query_argument(arg).is_none())
        .any(arg_names_something)
}

fn arg_names_something(arg: &FunctionArg) -> bool {
    visit_expressions(arg, |expr| {
        let names = match expr {
            Expr::Value(v) => matches!(
                v.value,
                Value::SingleQuotedString(_)
                    | Value::DoubleQuotedString(_)
                    | Value::DollarQuotedString(_)
                    | Value::EscapedStringLiteral(_)
                    | Value::NationalStringLiteral(_)
            ),
            Expr::Identifier(ident) => ident.quote_style.is_some(),
            Expr::CompoundIdentifier(parts) => parts.iter().any(|p| p.quote_style.is_some()),
            _ => false,
        };
        if names {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

/// The query an argument is, when it is one: `(SELECT src, dst FROM edges)`, named or
/// not, and with any parentheses around it. A query nested deeper in an argument's
/// expression is not one: arc's own reader reads a select's `FROM` and its joins, and
/// not the subqueries an expression holds.
fn query_argument(arg: &FunctionArg) -> Option<&sqlparser::ast::Query> {
    fn query(expr: &Expr) -> Option<&sqlparser::ast::Query> {
        match expr {
            Expr::Subquery(query) => Some(query),
            Expr::Nested(inner) => query(inner),
            _ => None,
        }
    }
    match arg {
        FunctionArg::Named {
            arg: FunctionArgExpr::Expr(expr),
            ..
        }
        | FunctionArg::ExprNamed {
            arg: FunctionArgExpr::Expr(expr),
            ..
        }
        | FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => query(expr),
        _ => None,
    }
}

/// The filesystem-path string literals of a file-reader argument expression.
///
/// Handles a single quoted path (`'x.parquet'`) and a bracketed/`ARRAY` list of them
/// (`['a.json', 'b.json']`) — DuckDB's multi-file glob form. Paths keep their original
/// case (filesystems are case-sensitive); everything else is ignored, so reader options
/// like `format => 'array'` never masquerade as inputs (they arrive as named args, which
/// the caller already skips, but a stray literal is harmless).
fn path_literals(expr: &Expr, paths: &mut Vec<String>) {
    match expr {
        Expr::Value(v) => {
            if let Value::SingleQuotedString(path) = &v.value {
                paths.push(path.clone());
            }
        }
        Expr::Array(array) => {
            for elem in &array.elem {
                path_literals(elem, paths);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duckdb_lineage::recorded_steps;

    /// What the rules make of DuckDB's reading of `sql`, from the answers the 2.0 preview
    /// gave for it: what [`extract_per_statement`] returns with `ARC_SQL_READER=duckdb`.
    fn read_by_duckdb(sql: &str) -> Vec<SqlAssets> {
        recorded_steps::read(sql)
            .iter()
            .map(duckdb_statement)
            .collect()
    }

    fn read_by_sqlparser(sql: &str) -> Vec<SqlAssets> {
        sqlparser_per_statement(sql).unwrap_or_else(|e| panic!("sqlparser refused {sql:?}: {e:?}"))
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn duckdb_reads_an_asof_join_sqlparser_cannot_parse() {
        let sql = recorded_steps::ASOF;
        assert!(
            sqlparser_per_statement(sql).is_err(),
            "sqlparser parses {sql:?} now; the step no longer shows what the switch is for"
        );
        let read = read_by_duckdb(sql);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].outputs, set(&["r"]), "{sql}");
        assert_eq!(read[0].inputs, set(&["t", "u"]), "{sql}");
        assert_eq!(read[0].kinds.get("r"), Some(&AssetKind::Table));
    }

    #[test]
    fn table_functions_are_read_by_the_same_rules_under_either_reader() {
        for sql in [
            recorded_steps::READ_XLSX,
            recorded_steps::RANGE,
            recorded_steps::MLPACK,
        ] {
            assert_eq!(read_by_duckdb(sql), read_by_sqlparser(sql), "{sql}");
        }
        let xlsx = &read_by_duckdb(recorded_steps::READ_XLSX)[0];
        assert_eq!(
            xlsx.inputs,
            set(&["build/budget.xlsx"]),
            "read_xlsx reads its file"
        );
        assert_eq!(xlsx.kinds.get("build/budget.xlsx"), Some(&AssetKind::File));
        let range = &read_by_duckdb(recorded_steps::RANGE)[0];
        assert_eq!(range.inputs, set(&[]), "range reads no table");
        assert_eq!(range.unread_table_functions, set(&[]));
        let mlpack = &read_by_duckdb(recorded_steps::MLPACK)[0];
        assert_eq!(
            mlpack.inputs,
            set(&[]),
            "the call names no table arc can read"
        );
        assert_eq!(
            mlpack.unread_table_functions,
            set(&["mlpack_random_forest_train"]),
            "the call is listed for the warning that asks for depends_on: and produces:"
        );
    }

    #[test]
    fn a_table_function_called_on_a_subquery_is_read_by_the_same_rules_under_either_reader() {
        let cases: [(&str, &[&str], &[&str]); 3] = [
            (recorded_steps::SUBQUERY_CALL, &["edges"], &[]),
            // A string inside the subquery is the subquery's own.
            (recorded_steps::SUBQUERY_WITH_STRING, &["edges"], &[]),
            // A string beside it still names something arc does not read.
            (
                recorded_steps::SUBQUERY_BESIDE_STRING,
                &["train"],
                &["my_ext_fit"],
            ),
        ];
        for (sql, reads, unread) in cases {
            let by_duckdb = read_by_duckdb(sql);
            assert_eq!(by_duckdb, read_by_sqlparser(sql), "{sql}");
            assert_eq!(by_duckdb[0].inputs, set(reads), "reads of {sql}");
            assert_eq!(
                by_duckdb[0].unread_table_functions,
                set(unread),
                "unread functions of {sql}"
            );
        }
    }

    #[test]
    fn a_copy_is_read_by_the_same_rules_under_either_reader() {
        for (sql, file, kind) in [
            (recorded_steps::COPY_QUERY, "out.csv", AssetKind::File),
            (
                recorded_steps::COPY_PARTITIONED,
                "parts",
                AssetKind::Directory,
            ),
            (
                recorded_steps::COPY_ONE_FILE_PER_THREAD_OFF,
                "one.parquet",
                AssetKind::File,
            ),
        ] {
            let read = read_by_duckdb(sql);
            assert_eq!(read, read_by_sqlparser(sql), "{sql}");
            assert_eq!(read[0].outputs, set(&[file]), "{sql}");
            assert_eq!(read[0].kinds.get(file), Some(&kind), "{sql}");
            assert_eq!(read[0].inputs, set(&["a"]), "{sql}");
        }
    }

    // DuckDB reads `1_000` as 1000, which casts to true: measured on v1.5.5,
    // `PER_THREAD_OUTPUT 1_000` wrote a directory, and the 2.0 preview's tokenizer gives
    // `1_000` as one number. Rust's `f64` parse refuses the separator, so the number is
    // one the rule cannot read, and a value it cannot read leaves the option on.
    #[test]
    fn a_number_the_rule_cannot_read_leaves_per_thread_output_on() {
        let value = duckdb_copy_option(&Some(ArgValue::Number("1_000".to_string())));
        assert_eq!(value, CopyOptionValue::Number("1_000".to_string()));
        assert_eq!(
            copy_to_target_kind(&[("PER_THREAD_OUTPUT".to_string(), value)]),
            AssetKind::Directory,
            "PER_THREAD_OUTPUT 1_000 writes a directory"
        );
    }

    #[test]
    fn a_pivot_joined_to_another_table_is_not_read_as_its_first() {
        let read = read_by_duckdb(recorded_steps::PIVOT_JOIN);
        assert_eq!(read.len(), 1);
        assert_ne!(
            read[0].inputs,
            set(&["t"]),
            "the pivot reads the rows of t and u, and is read as reading t alone"
        );
        assert_eq!(read[0].inputs, set(&[]), "the statement is unread");
    }

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

    // read_xlsx, read_xml, read_dta, read_stat and ST_Read lift the path they read,
    // exactly like read_parquet/read_csv/read_json — not the opaque fn name.
    #[test]
    fn test_five_new_readers_lift_path_not_function_name() {
        for (reader, path) in [
            ("read_xlsx", "build/budget.xlsx"),
            ("read_xml", "build/catalog.xml"),
            ("read_dta", "build/survey.dta"),
            ("read_stat", "build/survey.sas7bdat"),
            ("ST_Read", "data/shapes.shp"),
        ] {
            let sql = format!("CREATE TABLE r AS SELECT * FROM {reader}('{path}');");
            let assets = extract_assets(&sql).unwrap();
            assert!(
                assets.inputs.contains(path),
                "{reader}: path {path} should be the input, got {:?}",
                assets.inputs
            );
            assert!(
                !assets.inputs.contains(&reader.to_lowercase()),
                "{reader}: fn name must not be recorded as a table input"
            );
            assert_eq!(assets.kinds.get(path), Some(&AssetKind::File));
        }
    }

    // A glob-shaped path to one of the five new readers is a Pattern, same as it
    // already is for the twelve existing readers.
    #[test]
    fn test_five_new_readers_glob_path_is_a_pattern() {
        for (reader, pattern) in [
            ("read_xlsx", "build/*.xlsx"),
            ("read_xml", "build/report-?.xml"),
            ("ST_Read", "data/[a-z].shp"),
        ] {
            let sql = format!("CREATE TABLE r AS SELECT * FROM {reader}('{pattern}');");
            let assets = extract_assets(&sql).unwrap();
            assert_eq!(
                assets.kinds.get(pattern),
                Some(&AssetKind::Pattern),
                "{reader}: {pattern} should be a Pattern"
            );
        }
    }

    // range, generate_series and DuckDB's catalog-introspection table functions
    // produce rows from no backing table at all: nothing is recorded as an input,
    // and the fn name is not recorded as a table either.
    #[test]
    fn test_row_generators_and_catalog_functions_read_no_table() {
        for call in [
            "range(3)",
            "generate_series(1, 5)",
            "duckdb_functions()",
            "duckdb_tables()",
            "duckdb_secrets()",
        ] {
            let sql = format!("CREATE TABLE c AS SELECT * FROM {call};");
            let assets = extract_assets(&sql).unwrap();
            assert!(
                assets.inputs.is_empty(),
                "{call}: no table should be read, got {:?}",
                assets.inputs
            );
        }
    }

    // glob('pattern') lifts the pattern itself as an input, not a table named `glob`.
    #[test]
    fn test_glob_function_lifts_pattern_not_table_name() {
        let sql = "CREATE TABLE c AS SELECT * FROM glob('data/*.csv');";
        let assets = extract_assets(sql).unwrap();
        assert!(assets.inputs.contains("data/*.csv"), "pattern is the input");
        assert!(!assets.inputs.contains("glob"), "fn name is not an input");
        assert_eq!(assets.kinds.get("data/*.csv"), Some(&AssetKind::Pattern));
    }

    // An extension-supplied table function that reads tables named in its own
    // arguments (`"X"` is a quoted identifier) records no input — not even one under its
    // own name, which is not a table — and is listed as unread, so the step can be
    // asked for what it reads and writes.
    #[test]
    fn test_table_function_with_quoted_identifier_args_is_unread_not_recorded() {
        let sql = r#"CREATE TABLE fitted AS SELECT * FROM mlpack_random_forest_train("X", "Y", "params", "model");"#;
        let assets = extract_assets(sql).unwrap();
        assert!(
            !assets.inputs.contains("mlpack_random_forest_train"),
            "the function's name is not a table, got {:?}",
            assets.inputs
        );
        assert!(
            assets.inputs.is_empty(),
            "no input at all: {:?}",
            assets.inputs
        );
        assert_eq!(
            assets.unread_table_functions,
            BTreeSet::from(["mlpack_random_forest_train".to_string()])
        );
        assert!(
            assets.outputs.contains("fitted"),
            "the CTAS target is still produced"
        );
    }

    // The other form a call names something in: a string. Each of these is a table
    // function arc has no signature for, called with a string at some depth — a plain
    // argument, a list element, a named argument's value, a schema-qualified call.
    #[test]
    fn test_table_function_with_a_string_arg_is_unread() {
        for sql in [
            "SELECT * FROM query('SELECT 1');",
            "SELECT * FROM my_ext_scan(['a', 'b']);",
            "SELECT * FROM my_ext_scan(3, source := 'orders');",
            "SELECT * FROM my_ext.my_ext_scan('orders');",
        ] {
            let assets = extract_assets(sql).unwrap();
            assert!(
                assets.inputs.is_empty(),
                "{sql}: records no input, got {:?}",
                assets.inputs
            );
            assert_eq!(
                assets.unread_table_functions.len(),
                1,
                "{sql}: one function is unread, got {:?}",
                assets.unread_table_functions
            );
        }
    }

    // A function called twice is listed once; two functions are listed twice.
    #[test]
    fn test_unread_table_functions_are_a_set_of_names() {
        let sql = "SELECT * FROM my_scan('a'); SELECT * FROM my_scan('b'); SELECT * FROM other_scan('c');";
        let assets = extract_assets(sql).unwrap();
        assert_eq!(
            assets.unread_table_functions,
            BTreeSet::from(["my_scan".to_string(), "other_scan".to_string()])
        );
    }

    // A call with no string and no quoted identifier names nothing arc could be missing:
    // it is not listed as unread. `recent()` and `recent(days := 7)` (a table macro)
    // keep recording their own name; `range(10)` records nothing, as before. A call on a
    // subquery records what the subquery reads instead; see the tests for it below.
    #[test]
    fn test_table_function_with_no_string_or_quoted_identifier_is_not_unread() {
        for (sql, records) in [
            ("SELECT * FROM range(10);", None),
            ("SELECT * FROM recent();", Some("recent")),
            (
                "SELECT * FROM recent(days := 7, strict := true);",
                Some("recent"),
            ),
            (
                "SELECT * FROM my_ext_scan(10, 2.5, x);",
                Some("my_ext_scan"),
            ),
            // An argument that is not a leaf but holds no string: a list of numbers, a
            // sum, a negation. Only a string or a quoted identifier names anything.
            ("SELECT * FROM my_ext_scan([1, 2]);", Some("my_ext_scan")),
            ("SELECT * FROM my_ext_scan(1 + 2, -3);", Some("my_ext_scan")),
        ] {
            let assets = extract_assets(sql).unwrap();
            assert!(
                assets.unread_table_functions.is_empty(),
                "{sql}: not unread, got {:?}",
                assets.unread_table_functions
            );
            assert_eq!(
                assets.inputs,
                records
                    .map(str::to_string)
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                "{sql}: inputs"
            );
        }
    }

    // A table function called on a subquery reads the tables the subquery reads, and its
    // own name is not one — whether the function is an extension's, DuckDB's own, or a
    // row generator. The subquery may be named, parenthesised again, or nest another in
    // its `FROM`.
    #[test]
    fn test_table_function_called_on_a_subquery_reads_the_subquerys_tables() {
        for (sql, reads) in [
            (
                "CREATE TABLE paths AS SELECT * FROM onager_pth_dijkstra((SELECT src, dst, w FROM edges));",
                &["edges"][..],
            ),
            (
                "CREATE TABLE r AS SELECT * FROM onager_pth_dijkstra((SELECT src, dst, w FROM (SELECT * FROM edges) e));",
                &["edges"],
            ),
            (
                "CREATE TABLE r AS SELECT * FROM summary((SELECT e.src FROM edges e JOIN nodes n ON n.id = e.src));",
                &["edges", "nodes"],
            ),
            (
                "CREATE TABLE r AS SELECT * FROM range((SELECT count(*) FROM edges));",
                &["edges"],
            ),
            (
                "CREATE TABLE r AS SELECT * FROM my_ext_scan(source := (SELECT src FROM edges));",
                &["edges"],
            ),
            (
                "CREATE TABLE r AS SELECT * FROM my_ext_scan(((SELECT src FROM edges)));",
                &["edges"],
            ),
        ] {
            let assets = extract_assets(sql).unwrap();
            assert_eq!(assets.inputs, set(reads), "{sql}: the subquery's tables");
            assert!(
                assets.unread_table_functions.is_empty(),
                "{sql}: the call is read, got {:?}",
                assets.unread_table_functions
            );
            assert_eq!(assets.outputs.len(), 1, "{sql}: the target is produced");
        }
    }

    // The statement that showed the defect: two extension functions called on subqueries
    // and joined.
    #[test]
    fn test_two_table_functions_called_on_subqueries_and_joined_read_the_table() {
        let sql = "CREATE TEMP TABLE result AS \
            SELECT d.node_id, CASE WHEN isinf(d.distance) THEN NULL ELSE d.distance END AS distance, c.component \
            FROM onager_pth_dijkstra((SELECT src, dst, w FROM edges), directed := false, source := 0) d \
            JOIN onager_cmm_components((SELECT src, dst FROM edges)) c USING (node_id);";
        let assets = extract_assets(sql).unwrap();
        assert_eq!(assets.inputs, set(&["edges"]));
        assert_eq!(assets.outputs, set(&["result"]));
        assert!(assets.unread_table_functions.is_empty());
    }

    // A string, or a quoted identifier, inside the subquery is the subquery's own: it
    // names nothing the call reads, so it does not send the call to the unread arm.
    #[test]
    fn test_a_string_inside_a_subquery_argument_does_not_make_the_call_unread() {
        for sql in [
            "CREATE TABLE r AS SELECT * FROM summary((SELECT src, dst FROM edges WHERE kind = 'road'));",
            r#"CREATE TABLE r AS SELECT * FROM summary((SELECT "src" FROM edges));"#,
        ] {
            let assets = extract_assets(sql).unwrap();
            assert_eq!(assets.inputs, set(&["edges"]), "{sql}");
            assert!(
                assets.unread_table_functions.is_empty(),
                "{sql}: not unread, got {:?}",
                assets.unread_table_functions
            );
        }
    }

    // A string or a quoted identifier beside the subquery still names something arc does
    // not read: the call keeps the warning, and the subquery's tables are still read.
    #[test]
    fn test_a_string_beside_a_subquery_argument_keeps_the_call_unread() {
        for sql in [
            "CREATE TABLE m AS SELECT * FROM my_ext_fit((SELECT x, y FROM train), 'model');",
            r#"CREATE TABLE m AS SELECT * FROM my_ext_fit((SELECT x, y FROM train), "model");"#,
            "CREATE TABLE m AS SELECT * FROM my_ext_fit((SELECT x, y FROM train), target := 'model');",
        ] {
            let assets = extract_assets(sql).unwrap();
            assert_eq!(
                assets.inputs,
                set(&["train"]),
                "{sql}: no read named for the call"
            );
            assert_eq!(
                assets.unread_table_functions,
                set(&["my_ext_fit"]),
                "{sql}: the call is listed for the warning"
            );
        }
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

    // COPY (SELECT …) TO 'file' reads what its query reads and produces the file, as
    // COPY <table> TO 'file' does.
    #[test]
    fn test_copy_from_query_produces_file() {
        let sql = "COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER);";
        let assets = extract_assets(sql).unwrap();
        assert_eq!(
            assets.inputs,
            BTreeSet::from(["a".to_string()]),
            "the query's table is read"
        );
        assert_eq!(
            assets.outputs,
            BTreeSet::from(["out.csv".to_string()]),
            "the file is produced, and nothing else is"
        );
        assert_eq!(assets.kinds.get("out.csv"), Some(&AssetKind::File));
    }

    // A query that reads several tables (a CTE and a join) has each of them recorded as
    // read, and the CTE's own name is not one of them.
    #[test]
    fn test_copy_from_query_reads_every_table_its_query_reads() {
        let sql = "COPY (WITH c AS (SELECT * FROM a) \
                   SELECT * FROM c JOIN b ON c.x = b.x) \
                   TO 'out.csv';";
        let assets = extract_per_statement(sql).unwrap().remove(0);
        assert_eq!(
            assets.inputs,
            ["a", "b"].map(str::to_string).into_iter().collect(),
            "tables the query reads"
        );
        assert_eq!(assets.outputs, BTreeSet::from(["out.csv".to_string()]));
    }

    // The target of COPY (query) is classified from the statement's own options, and
    // classified as COPY <table> TO classifies it: the same options give the same kind
    // whichever the source. The expected kinds are listed too, so two arms that were
    // both wrong the same way would not pass.
    #[test]
    fn test_copy_from_query_target_kind_follows_the_options_as_copy_from_table_does() {
        for (options, expected) in [
            ("(HEADER)", AssetKind::File),
            ("(FORMAT parquet)", AssetKind::File),
            (
                "(FORMAT parquet, PARTITION_BY (region))",
                AssetKind::Directory,
            ),
            ("(PER_THREAD_OUTPUT)", AssetKind::Directory),
            ("(PER_THREAD_OUTPUT false)", AssetKind::File),
            ("(FORMAT csv, FILE_SIZE_BYTES '1GB')", AssetKind::Directory),
            (
                "(FORMAT parquet, ROW_GROUPS_PER_FILE 2)",
                AssetKind::Directory,
            ),
        ] {
            let from_table = extract_assets(&format!("COPY a TO 'out' {options};")).unwrap();
            let from_query =
                extract_assets(&format!("COPY (SELECT * FROM a) TO 'out' {options};")).unwrap();
            assert_eq!(
                from_table.kinds.get("out"),
                Some(&expected),
                "COPY a TO 'out' {options}"
            );
            assert_eq!(
                from_query.kinds.get("out"),
                Some(&expected),
                "COPY (SELECT * FROM a) TO 'out' {options}"
            );
        }
    }

    // Only a file target produces an asset: a COPY (query) to STDOUT reads its tables
    // and writes nothing arc can track.
    #[test]
    fn test_copy_from_query_to_stdout_produces_nothing() {
        let assets = extract_assets("COPY (SELECT * FROM a) TO STDOUT;").unwrap();
        assert_eq!(assets.inputs, BTreeSet::from(["a".to_string()]));
        assert!(
            assets.outputs.is_empty(),
            "no output for STDOUT, got {:?}",
            assets.outputs
        );
    }

    // The two COPY <table> forms record what they recorded before COPY (query) did.
    // `COPY a FROM 'in.csv'` records the file as *produced* — the direction is not
    // read from the statement — and this change leaves that as it is.
    #[test]
    fn test_copy_table_forms_record_what_they_recorded_before() {
        let to = extract_assets("COPY a TO 'out.csv' (HEADER);").unwrap();
        assert_eq!(to.inputs, BTreeSet::from(["a".to_string()]), "TO: reads");
        assert_eq!(
            to.outputs,
            BTreeSet::from(["out.csv".to_string()]),
            "TO: produces"
        );
        assert_eq!(to.kinds.get("out.csv"), Some(&AssetKind::File));

        let from = extract_assets("COPY a FROM 'in.csv';").unwrap();
        assert_eq!(
            from.inputs,
            BTreeSet::from(["a".to_string()]),
            "FROM: reads"
        );
        assert_eq!(
            from.outputs,
            BTreeSet::from(["in.csv".to_string()]),
            "FROM: produces"
        );
        assert_eq!(from.kinds.get("in.csv"), Some(&AssetKind::File));
    }

    // A table function called with no string, no quoted identifier and no subquery — a
    // table macro such as `recent()` — keeps recording its name. One called with a string
    // or a quoted identifier records nothing under its name; see the `unread` tests
    // above. One called on a subquery records the tables the subquery reads.
    #[test]
    fn test_non_file_table_function_unchanged() {
        // A table macro (an extension-supplied table function that is neither a file
        // reader nor a row-generator, called with no string, no quoted identifier and no
        // subquery) still records its own name — it may read tables named in its own definition,
        // which this layer cannot see, so the name is the only handle lineage has on it.
        let sql = "SELECT * FROM recent();";
        let assets = extract_assets(sql).unwrap();
        assert!(
            assets.inputs.contains("recent"),
            "a table macro called with no string or quoted identifier still records its name"
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
