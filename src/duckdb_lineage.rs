//! What each statement of a DuckDB SQL step reads and produces, taken from DuckDB's own
//! parse of the step.
//!
//! DuckDB 2.0 answers two questions about SQL without running it. `sql_tokenize` splits
//! the text into tokens, each with the byte it starts at, and `json_serialize_sql` gives
//! the tree of a `SELECT`. For a statement that is not a `SELECT` it answers `not
//! implemented`, which also says the statement parsed; for text it cannot parse it
//! answers `parser`, with its own message. DuckDB 1.x has no `sql_tokenize`, so the
//! reader asks a 2.0 build and refuses any other.
//!
//! The reader asks twice, through the DuckDB command line, as `arc run` runs a step.
//! First for the version and the step's tokens, which it splits into statements at each
//! `;` token: a `;` inside a string or a comment is part of that token, not one of its
//! own. Then for the tree of each statement, and of each query a statement holds: the
//! query of a `CREATE TABLE … AS`, of an `INSERT`, of a `COPY (…) TO`.
//!
//! Reads come from a tree: each table it names, less the names a `WITH` in scope
//! declares, and each table function it calls, with the arguments as written. What a
//! statement that is not a query creates, inserts into or copies is taken from its
//! words. A keyword is matched by its text and never by the part the tokenizer says the
//! token plays, because on the 2.0 preview that part changes with what came before it in
//! the same input: `INSERT` has come back as a column name, and a table after `FROM` as
//! a plain identifier. A token's text and start do not change, and they are all the
//! reader takes from a token.
//!
//! A table function is returned as a call, its name and its arguments, and a `COPY … TO`
//! with its options. Which function reads a file, which reads no table, and which options
//! write a directory, are decided by whoever reads the statement: `crate::introspect`,
//! which applies the same rules to what arc's other reader returns.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fmt;
use std::io::Write;
use std::ops::Range;
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::Value;

/// A table as a statement names it, with its schema and catalog when it gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableName {
    pub(crate) catalog: Option<String>,
    pub(crate) schema: Option<String>,
    pub(crate) name: String,
}

impl fmt::Display for TableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts = [&self.catalog, &self.schema].into_iter().flatten();
        let parts: Vec<&str> = parts.chain([&self.name]).map(String::as_str).collect();
        f.write_str(&parts.join("."))
    }
}

/// What a statement reads or produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Relation {
    /// A table or a view.
    Table(TableName),
    /// A file named by a string: `COPY t TO 'out.csv'`, `FROM 'data.csv'`.
    File(String),
}

impl fmt::Display for Relation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Relation::Table(table) => write!(f, "{table}"),
            Relation::File(path) => write!(f, "file '{path}'"),
        }
    }
}

/// A call to a table function, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableCall {
    pub(crate) function: String,
    pub(crate) args: Vec<Arg>,
}

/// One argument of a table function call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Arg {
    /// The parameter it is passed to, when it is passed by name: `header = true` and
    /// `header := true` both name `header`.
    pub(crate) name: Option<String>,
    pub(crate) value: ArgValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArgValue {
    /// A number, as written.
    Number(String),
    /// A string, without its quotes.
    String(String),
    /// A double-quoted name, without its quotes.
    QuotedIdentifier(String),
    /// A bare name.
    Identifier(String),
    /// A list, `['a.json', 'b.json']`, or a `COPY` option's parenthesised values.
    List(Vec<ArgValue>),
    /// Any other expression, as written. `quotes` is whether it holds a string or a
    /// double-quoted name at any depth: `lower('X')` and `"Q" + 1` do, `1 + 2` does not.
    Expression { text: String, quotes: bool },
}

/// An option of a `COPY … TO`, `FORMAT parquet` or `HEADER`, with its value as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyOption {
    pub(crate) name: String,
    /// `None` for an option given without one: `HEADER`.
    pub(crate) value: Option<ArgValue>,
}

/// What kind of statement it is, as far as what it reads and produces goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Form {
    /// A statement DuckDB gives a tree for: a `SELECT`, `FROM`-first, `VALUES`,
    /// `SUMMARIZE`, `DESCRIBE`.
    Query,
    CreateTable,
    CreateView,
    Insert,
    Copy,
    Pivot,
    /// `SET VARIABLE`: it reads what its value's expression reads.
    SetVariable,
    /// A statement that reads and produces no table: `INSTALL`, `LOAD`, `SET`, `RESET`,
    /// `PRAGMA`, `USE`, `CHECKPOINT` and the transaction statements.
    NoData,
    /// A statement whose reads the reader could not take, named by its first word: a form
    /// it does not know, or one holding a query DuckDB gives no tree for. What it lists
    /// is what its words made certain, and what else it reads is unknown.
    Unread(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Statement {
    /// The statement as written, without its `;` or the comments before it.
    pub(crate) text: String,
    pub(crate) form: Form,
    pub(crate) reads: Vec<Relation>,
    pub(crate) produces: Vec<Relation>,
    pub(crate) calls: Vec<TableCall>,
    /// The options of a `COPY … TO`, in order. Empty for any other statement.
    pub(crate) options: Vec<CopyOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepReading {
    /// The version the DuckDB asked reported, as `version()` gives it.
    pub(crate) duckdb_version: String,
    pub(crate) statements: Vec<Statement>,
}

/// Why a step was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The DuckDB named could not be run, or gave no version.
    NotRun { program: String, reason: String },
    /// The DuckDB named is not a 2.0 build.
    NotDuckDb2 { program: String, version: String },
    /// DuckDB could not parse a statement. `statement` counts from 1.
    Syntax {
        statement: usize,
        text: String,
        message: String,
    },
    /// DuckDB answered something the reader cannot read.
    Unreadable(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotRun { program, reason } => {
                write!(
                    f,
                    "could not ask DuckDB `{program}` for its parse: {reason}"
                )
            }
            Refusal::NotDuckDb2 { program, version } => write!(
                f,
                "`{program}` is DuckDB {version}; reading a step with DuckDB's own parser \
                 needs a DuckDB 2.0 build"
            ),
            Refusal::Syntax {
                statement,
                text,
                message,
            } => write!(
                f,
                "DuckDB cannot parse statement {statement} of the step: {message}\n    {text}"
            ),
            Refusal::Unreadable(reason) => write!(f, "DuckDB's parse could not be read: {reason}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Reads a step with the DuckDB `duckdb` names: a path, or a name found on `PATH`.
pub(crate) fn read_step(duckdb: &OsStr, sql: &str) -> Result<StepReading, Refusal> {
    read_step_with(&mut Process { program: duckdb }, sql)
}

/// A token as `sql_tokenize` gives it. `start` is a byte offset into the text asked
/// about. `token_type` is kept for the record and read nowhere; see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
struct Token {
    start: usize,
    token_type: String,
    word: String,
}

impl Token {
    fn is_comment(&self) -> bool {
        self.word.starts_with("--") || self.word.starts_with("/*")
    }
}

/// The first answer: the version, and the tokens or the error DuckDB gave instead.
struct Tokenized {
    version: String,
    tokens: Result<Vec<Token>, String>,
}

/// The two questions the reader asks. A DuckDB process answers them, and the tests
/// answer them from what the preview answered.
trait Ask {
    /// The DuckDB asked, for a refusal to name.
    fn program(&self) -> String;
    fn tokens(&mut self, sql: &str) -> Result<Tokenized, Refusal>;
    /// DuckDB's answer from `json_serialize_sql` for each text, in order.
    fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal>;
}

struct Process<'a> {
    program: &'a OsStr,
}

impl Ask for Process<'_> {
    fn program(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    fn tokens(&mut self, sql: &str) -> Result<Tokenized, Refusal> {
        let script = format!(
            "SELECT version() AS version;\n\
             SELECT start, token_type, word FROM sql_tokenize({});\n",
            literal(sql)
        );
        let (answers, stderr) = self.run(&script)?;
        let version = answers
            .first()
            .and_then(|rows| rows.get(0)?.get("version")?.as_str())
            .ok_or_else(|| Refusal::NotRun {
                program: self.program(),
                reason: format!("it gave no version: {}", stderr.trim()),
            })?;
        let tokens = match answers.get(1) {
            Some(rows) => serde_json::from_value(rows.clone()).map_err(|e| e.to_string()),
            None => Err(stderr.trim().to_string()),
        };
        Ok(Tokenized {
            version: version.to_string(),
            tokens,
        })
    }

    fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<String> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| format!("({i}, {})", literal(text)))
            .collect();
        let script = format!(
            "SELECT json_serialize_sql(q) AS tree FROM (VALUES {}) AS v(i, q) ORDER BY i;\n",
            rows.join(", ")
        );
        let (answers, stderr) = self.run(&script)?;
        let rows = answers
            .first()
            .and_then(Value::as_array)
            .ok_or_else(|| Refusal::Unreadable(format!("no trees: {}", stderr.trim())))?;
        Ok(rows.iter().map(|row| row["tree"].clone()).collect())
    }
}

impl Process<'_> {
    /// Runs `script` and returns each result DuckDB printed, as JSON, with its stderr.
    fn run(&self, script: &str) -> Result<(Vec<Value>, String), Refusal> {
        let not_run = |e: std::io::Error| Refusal::NotRun {
            program: self.program(),
            reason: e.to_string(),
        };
        let mut child = Command::new(self.program)
            .args(["-no-init", "-json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(not_run)?;
        // Written from a thread: DuckDB answers each statement as it reads it, and an
        // answer larger than the pipe would stall a write made from this thread.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let script = script.to_owned();
        let writer = std::thread::spawn(move || stdin.write_all(script.as_bytes()));
        let output = child.wait_with_output().map_err(not_run)?;
        // A write cut short by DuckDB exiting shows in its stderr, which the caller reads.
        let _ = writer.join();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let answers = serde_json::Deserializer::from_str(&stdout)
            .into_iter::<Value>()
            .map_while(Result::ok)
            .collect();
        Ok((
            answers,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

/// `text` as a SQL string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn read_step_with(ask: &mut dyn Ask, sql: &str) -> Result<StepReading, Refusal> {
    let tokenized = ask.tokens(sql)?;
    if major(&tokenized.version) != Some(2) {
        return Err(Refusal::NotDuckDb2 {
            program: ask.program(),
            version: tokenized.version,
        });
    }
    let tokens = tokenized.tokens.map_err(Refusal::Unreadable)?;
    let pieces = split(sql, &tokens)?;
    let frames: Vec<Frame> = pieces.iter().map(frame).collect();

    let mut texts: Vec<String> = pieces.iter().map(|p| p.text.clone()).collect();
    texts.extend(frames.iter().filter_map(|f| f.query.clone()));
    let trees = ask.trees(&texts)?;
    if trees.len() != texts.len() {
        return Err(Refusal::Unreadable(format!(
            "{} answers for {} texts",
            trees.len(),
            texts.len()
        )));
    }
    let (own, held) = trees.split_at(pieces.len());
    let mut held = held.iter();

    let mut statements = Vec::new();
    for (n, ((piece, frame), tree)) in pieces.iter().zip(frames).zip(own).enumerate() {
        let query = frame.query.clone().map(|text| (text, held.next()));
        statements.push(read_statement(n + 1, piece, frame, tree, query)?);
    }
    Ok(StepReading {
        duckdb_version: tokenized.version,
        statements,
    })
}

/// The major version in `v2.0.0-alpha43569`.
fn major(version: &str) -> Option<u64> {
    let version = version.strip_prefix('v').unwrap_or(version);
    version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
}

/// One statement's text, and its tokens other than comments, their starts made
/// relative to the text.
struct Piece {
    text: String,
    tokens: Vec<Token>,
}

fn split(sql: &str, tokens: &[Token]) -> Result<Vec<Piece>, Refusal> {
    let mut pieces = Vec::new();
    let mut words: Vec<&Token> = Vec::new();
    for token in tokens {
        if token.word == ";" {
            pieces.extend(piece(sql, &words, token.start)?);
            words.clear();
        } else if !token.is_comment() {
            words.push(token);
        }
    }
    pieces.extend(piece(sql, &words, sql.len())?);
    Ok(pieces)
}

fn piece(sql: &str, words: &[&Token], end: usize) -> Result<Option<Piece>, Refusal> {
    let Some(first) = words.first() else {
        return Ok(None);
    };
    let text = sql.get(first.start..end).ok_or_else(|| {
        Refusal::Unreadable(format!(
            "a token starts at byte {}, off the text",
            first.start
        ))
    })?;
    let tokens = words
        .iter()
        .map(|t| Token {
            start: t.start - first.start,
            ..(*t).clone()
        })
        .collect();
    Ok(Some(Piece {
        text: text.trim_end().to_string(),
        tokens,
    }))
}

/// What a statement's words say, used when DuckDB gives no tree for the statement.
struct Frame {
    form: Form,
    reads: Vec<Relation>,
    produces: Vec<Relation>,
    /// A query the statement holds, to be asked for its tree.
    query: Option<String>,
    options: Vec<CopyOption>,
}

fn frame(piece: &Piece) -> Frame {
    let mut w = Words {
        text: &piece.text,
        tokens: &piece.tokens,
        at: 0,
    };
    let framed = if w.eat("create") {
        create(&mut w)
    } else if w.eat("insert") {
        insert(&mut w)
    } else if w.eat("copy") {
        copy(&mut w)
    } else if ["pivot", "unpivot", "pivot_wider", "pivot_longer"]
        .iter()
        .any(|kw| w.eat(kw))
    {
        pivot(&mut w)
    } else if w.eat("set") {
        set(&mut w)
    } else if w.eat("force") {
        // `FORCE INSTALL` and `FORCE CHECKPOINT`, the two statements that open with it.
        Some(Frame::no_data())
    } else {
        NO_DATA.iter().any(|kw| w.is(0, kw)).then(Frame::no_data)
    };
    framed.unwrap_or_else(|| Frame {
        form: Form::Unread(piece.tokens[0].word.to_uppercase()),
        reads: Vec::new(),
        produces: Vec::new(),
        query: None,
        options: Vec::new(),
    })
}

/// First words of a statement that reads and produces no table. `SET` is framed on its
/// own, because `SET VARIABLE` can read one.
const NO_DATA: [&str; 10] = [
    "install",
    "load",
    "pragma",
    "reset",
    "use",
    "checkpoint",
    "begin",
    "commit",
    "rollback",
    "abort",
];

impl Frame {
    fn no_data() -> Frame {
        Frame {
            form: Form::NoData,
            reads: Vec::new(),
            produces: Vec::new(),
            query: None,
            options: Vec::new(),
        }
    }
}

/// `CREATE [OR REPLACE] [TEMP] TABLE|VIEW [IF NOT EXISTS] name [(…)] [AS query]`
fn create(w: &mut Words) -> Option<Frame> {
    w.eat_all(&["or", "replace"]);
    let _ = w.eat("temp") || w.eat("temporary") || w.eat("persistent");
    let form = if w.eat("table") {
        Form::CreateTable
    } else if w.eat("view") {
        Form::CreateView
    } else {
        return None;
    };
    w.eat_all(&["if", "not", "exists"]);
    let name = w.name()?;
    if w.is(0, "(") {
        w.group();
    }
    let query = w.eat("as").then(|| w.rest().to_string());
    Some(Frame {
        form,
        reads: Vec::new(),
        produces: vec![Relation::Table(name)],
        query,
        options: Vec::new(),
    })
}

/// `INSERT [OR REPLACE|IGNORE] INTO name [AS alias] [(cols)] [BY NAME|POSITION] query
/// [ON CONFLICT …] [RETURNING …]`
fn insert(w: &mut Words) -> Option<Frame> {
    if w.eat("or") && !(w.eat("replace") || w.eat("ignore")) {
        return None;
    }
    if !w.eat("into") {
        return None;
    }
    let name = w.name()?;
    if w.eat("as") {
        let _alias = w.ident();
    }
    // A group opening with a query is the query; any other group lists columns.
    let opens_query = ["select", "with", "from", "values", "("]
        .iter()
        .any(|kw| w.is(1, kw));
    if w.is(0, "(") && !opens_query {
        w.group();
    }
    if w.eat("by") && !(w.eat("name") || w.eat("position")) {
        return None;
    }
    let query = if w.eat_all(&["default", "values"]) {
        None
    } else {
        Some(w.rest_until_insert_tail().to_string())
    };
    Some(Frame {
        form: Form::Insert,
        reads: Vec::new(),
        produces: vec![Relation::Table(name)],
        query,
        options: Vec::new(),
    })
}

/// `COPY name [(cols)] TO 'file' [options]`, `COPY name [(cols)] FROM 'file'`, `COPY
/// (query) TO 'file' [options]`. A target DuckDB takes as a bare name, `COPY t TO out`, is
/// not framed.
fn copy(w: &mut Words) -> Option<Frame> {
    let Some(name) = w.name() else {
        let inner = w.group();
        if !w.eat("to") {
            return None;
        }
        let file = w.string()?;
        return Some(Frame {
            form: Form::Copy,
            reads: Vec::new(),
            produces: vec![Relation::File(file)],
            query: Some(w.text[inner].to_string()),
            options: copy_options(w),
        });
    };
    let table = Relation::Table(name);
    if w.is(0, "(") {
        w.group();
    }
    let (reads, produces, options) = if w.eat("to") {
        let file = Relation::File(w.string()?);
        (table, file, copy_options(w))
    } else if w.eat("from") {
        (Relation::File(w.string()?), table, Vec::new())
    } else {
        return None;
    };
    Some(Frame {
        form: Form::Copy,
        reads: vec![reads],
        produces: vec![produces],
        query: None,
        options,
    })
}

/// `[WITH] (name [value], …)` after a `COPY … TO`'s file. A value is one word, a
/// parenthesised list, or an expression running to the next `,` or `)`.
fn copy_options(w: &mut Words) -> Vec<CopyOption> {
    let mut options = Vec::new();
    w.eat("with");
    if !w.eat("(") {
        return options;
    }
    while let Some(name) = w.ident() {
        let value = if w.is(0, ",") || w.is(0, ")") {
            None
        } else if w.is(0, "(") {
            let from = w.at + 1;
            w.group();
            let inside = &w.tokens[from..w.at.saturating_sub(1)];
            Some(ArgValue::List(
                items(inside)
                    .into_iter()
                    .map(|item| w.value(item))
                    .collect(),
            ))
        } else {
            let from = w.at;
            let mut depth = 0usize;
            while let Some(word) = w.word(0) {
                match word {
                    "(" => depth += 1,
                    ")" if depth == 0 => break,
                    ")" => depth -= 1,
                    "," if depth == 0 => break,
                    _ => {}
                }
                w.at += 1;
            }
            Some(w.value(&w.tokens[from..w.at]))
        };
        options.push(CopyOption { name, value });
        if !w.eat(",") {
            break;
        }
    }
    options
}

/// `tokens` split at each `,` outside a group, with an empty list for no tokens.
fn items(tokens: &[Token]) -> Vec<&[Token]> {
    let mut items = Vec::new();
    let (mut from, mut depth) = (0, 0usize);
    for (i, token) in tokens.iter().enumerate() {
        match token.word.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                items.push(&tokens[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    if from < tokens.len() {
        items.push(&tokens[from..]);
    }
    items
}

/// `PIVOT name|'file'|(query) [[AS] alias] ON …`. DuckDB gives no tree for a `PIVOT`
/// whose columns it would have to read the data to know. A source that calls a function
/// is not framed, and neither is one that `ON` does not follow: `PIVOT t JOIN u USING (k)
/// ON …` pivots the rows of both tables, and a frame reading the first alone would
/// under-read it.
fn pivot(w: &mut Words) -> Option<Frame> {
    let (reads, query) = if let Some(name) = w.name() {
        if w.is(0, "(") {
            return None;
        }
        (vec![Relation::Table(name)], None)
    } else if let Some(file) = w.string() {
        (vec![Relation::File(file)], None)
    } else {
        let inner = w.group();
        (Vec::new(), Some(w.text[inner].to_string()))
    };
    if !w.eat("on") {
        w.eat("as");
        w.ident()?;
        if !w.eat("on") {
            return None;
        }
    }
    Some(Frame {
        form: Form::Pivot,
        reads,
        produces: Vec::new(),
        query,
        options: Vec::new(),
    })
}

/// `SET VARIABLE name = expr` reads what `SELECT expr` reads. Any other `SET` changes a
/// setting.
fn set(w: &mut Words) -> Option<Frame> {
    if !w.eat("variable") {
        return Some(Frame::no_data());
    }
    let _name = w.ident();
    if !(w.eat("=") || w.eat("to")) {
        return None;
    }
    Some(Frame {
        form: Form::SetVariable,
        reads: Vec::new(),
        produces: Vec::new(),
        query: Some(format!("SELECT {}", w.rest())),
        options: Vec::new(),
    })
}

/// A cursor over a statement's tokens.
struct Words<'a> {
    text: &'a str,
    tokens: &'a [Token],
    at: usize,
}

impl Words<'_> {
    fn word(&self, ahead: usize) -> Option<&str> {
        self.tokens.get(self.at + ahead).map(|t| t.word.as_str())
    }

    /// Whether the token `ahead` of the cursor is `kw`, in any case. A quoted name or a
    /// string never is, since its quotes are part of its text.
    fn is(&self, ahead: usize, kw: &str) -> bool {
        self.word(ahead).is_some_and(|w| w.eq_ignore_ascii_case(kw))
    }

    fn eat(&mut self, kw: &str) -> bool {
        self.eat_all(&[kw])
    }

    fn eat_all(&mut self, kws: &[&str]) -> bool {
        let found = kws.iter().enumerate().all(|(i, kw)| self.is(i, kw));
        if found {
            self.at += kws.len();
        }
        found
    }

    /// Where the cursor's token starts, or the end of the text.
    fn offset(&self) -> usize {
        self.tokens
            .get(self.at)
            .map_or(self.text.len(), |t| t.start)
    }

    /// The text from the cursor to the end.
    fn rest(&self) -> &str {
        &self.text[self.offset()..]
    }

    /// The text from the cursor to an `ON CONFLICT` or `RETURNING` outside any group.
    fn rest_until_insert_tail(&mut self) -> &str {
        let start = self.offset();
        let mut depth = 0usize;
        while let Some(word) = self.word(0) {
            match word {
                "(" => depth += 1,
                ")" => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth == 0
                && (self.is(0, "returning") || (self.is(0, "on") && self.is(1, "conflict")))
            {
                break;
            }
            self.at += 1;
        }
        self.text[start..self.offset()].trim_end()
    }

    /// A name, bare or double-quoted.
    fn ident(&mut self) -> Option<String> {
        let name = match self.word(0) {
            Some(word) if word.starts_with('"') => word[1..word.len() - 1].replace("\"\"", "\""),
            Some(word) if word.starts_with(|c: char| c.is_alphabetic() || c == '_') => {
                word.to_string()
            }
            _ => return None,
        };
        self.at += 1;
        Some(name)
    }

    /// `name`, `schema.name` or `catalog.schema.name`.
    fn name(&mut self) -> Option<TableName> {
        let mut parts = vec![self.ident()?];
        while self.eat(".") {
            parts.extend(self.ident());
        }
        let part = |p: &String| Some(p.clone());
        let (catalog, schema, name) = match parts.as_slice() {
            [name] => (None, None, name),
            [schema, name] => (None, part(schema), name),
            [catalog, schema, name] => (part(catalog), part(schema), name),
            _ => return None,
        };
        Some(TableName {
            catalog,
            schema,
            name: name.clone(),
        })
    }

    /// What `tokens`, a run of this statement's tokens, write as a value: a string, a
    /// double-quoted name, a number or a bare word when they are one token, and an
    /// expression otherwise.
    fn value(&self, tokens: &[Token]) -> ArgValue {
        let unquote =
            |word: &str, quote: &str| word[1..word.len() - 1].replace(&quote.repeat(2), quote);
        match tokens {
            [one] if one.word.starts_with('\'') => ArgValue::String(unquote(&one.word, "'")),
            [one] if one.word.starts_with('"') => {
                ArgValue::QuotedIdentifier(unquote(&one.word, "\""))
            }
            [one]
                if one
                    .word
                    .starts_with(|c: char| c.is_ascii_digit() || c == '.') =>
            {
                ArgValue::Number(one.word.clone())
            }
            [one] => ArgValue::Identifier(one.word.clone()),
            _ => {
                let start = tokens.first().map_or(0, |t| t.start);
                let end = tokens.last().map_or(0, |t| t.start + t.word.len());
                ArgValue::Expression {
                    text: self.text.get(start..end).unwrap_or_default().to_string(),
                    quotes: tokens.iter().any(|t| t.word.starts_with(['\'', '"'])),
                }
            }
        }
    }

    /// A single-quoted string, without its quotes.
    fn string(&mut self) -> Option<String> {
        let value = match self.word(0) {
            Some(word) if word.starts_with('\'') => word[1..word.len() - 1].replace("''", "'"),
            _ => return None,
        };
        self.at += 1;
        Some(value)
    }

    /// Steps past the parenthesised group at the cursor and returns the text inside it.
    /// A frame is used only for a statement DuckDB parsed, where the group closes.
    fn group(&mut self) -> Range<usize> {
        let open = self.offset();
        let mut depth = 0usize;
        while let Some(token) = self.tokens.get(self.at) {
            self.at += 1;
            match token.word.as_str() {
                "(" => depth += 1,
                ")" => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return open + 1..token.start;
                    }
                }
                _ => {}
            }
        }
        open + 1..self.text.len()
    }
}

/// How DuckDB answered `json_serialize_sql`.
enum Answer<'a> {
    Tree(&'a Value),
    Parser(String),
    NotImplemented,
    Other(String),
}

fn answer(v: &Value) -> Answer<'_> {
    if v["error"] == Value::Bool(false) {
        return Answer::Tree(&v["statements"]);
    }
    let message = v["error_message"].as_str().unwrap_or_default().to_string();
    match v["error_type"].as_str() {
        Some("parser") => Answer::Parser(message),
        Some("not implemented") => Answer::NotImplemented,
        other => Answer::Other(format!("{}: {message}", other.unwrap_or("no error_type"))),
    }
}

fn read_statement(
    n: usize,
    piece: &Piece,
    frame: Frame,
    tree: &Value,
    query: Option<(String, Option<&Value>)>,
) -> Result<Statement, Refusal> {
    let mut statement = Statement {
        text: piece.text.clone(),
        form: frame.form,
        reads: frame.reads,
        produces: frame.produces,
        calls: Vec::new(),
        options: frame.options,
    };
    let (tree, text) = match answer(tree) {
        Answer::Tree(tree) => {
            statement.form = Form::Query;
            statement.reads.clear();
            statement.produces.clear();
            statement.options.clear();
            (tree, piece.text.as_str())
        }
        Answer::Parser(message) => {
            return Err(Refusal::Syntax {
                statement: n,
                text: piece.text.clone(),
                message,
            });
        }
        Answer::NotImplemented => match &query {
            None => return Ok(statement),
            Some((text, held)) => match answer(held.unwrap_or(&Value::Null)) {
                Answer::Tree(tree) => (tree, text.as_str()),
                _ => {
                    statement.form = Form::Unread(piece.tokens[0].word.to_uppercase());
                    return Ok(statement);
                }
            },
        },
        Answer::Other(reason) => return Err(Refusal::Unreadable(reason)),
    };
    let found = walk(tree, text);
    for read in found.reads {
        if !statement.reads.contains(&read) {
            statement.reads.push(read);
        }
    }
    statement.calls = found.calls;
    Ok(statement)
}

/// What a tree reads, each in the order its text names it.
struct Found {
    reads: Vec<Relation>,
    calls: Vec<TableCall>,
}

fn walk(tree: &Value, text: &str) -> Found {
    let mut reads = Vec::new();
    let mut calls = Vec::new();
    visit(tree, text, &[], &mut reads, &mut calls);
    reads.sort_by(by_location);
    calls.sort_by(by_location);
    let mut found = Found {
        reads: Vec::new(),
        calls: calls.into_iter().map(|(_, call)| call).collect(),
    };
    for (_, read) in reads {
        if !found.reads.contains(&read) {
            found.reads.push(read);
        }
    }
    found
}

fn by_location<T>(a: &(u64, T), b: &(u64, T)) -> Ordering {
    a.0.cmp(&b.0)
}

/// Visits every node, keeping the names of the `WITH` queries in scope. A `WITH`
/// query's body sees the ones declared before it, and itself only when it is recursive:
/// a body naming itself, or one declared after it, reads the table of that name.
fn visit(
    v: &Value,
    text: &str,
    scope: &[String],
    reads: &mut Vec<(u64, Relation)>,
    calls: &mut Vec<(u64, TableCall)>,
) {
    let node = match v {
        Value::Array(items) => {
            for item in items {
                visit(item, text, scope, reads, calls);
            }
            return;
        }
        Value::Object(node) => node,
        _ => return,
    };
    let mut inner = scope.to_vec();
    let with = v["cte_map"]["map"].as_array().into_iter().flatten();
    for cte in with {
        let name = cte["key"].as_str().unwrap_or_default().to_lowercase();
        let recursive = cte["value"]["query_node"]["type"] == "RECURSIVE_CTE_NODE";
        let mut body_scope = inner.clone();
        if recursive {
            body_scope.push(name.clone());
        }
        visit(&cte["value"], text, &body_scope, reads, calls);
        inner.push(name);
    }
    let location = v["query_location"].as_u64().unwrap_or(u64::MAX);
    match v["type"].as_str() {
        Some("BASE_TABLE") => reads.extend(base_table(v, text, &inner).map(|r| (location, r))),
        Some("TABLE_FUNCTION") => {
            calls.extend(table_call(&v["function"], text).map(|c| (location, c)))
        }
        _ => {}
    }
    for (key, child) in node {
        if key != "cte_map" {
            visit(child, text, &inner, reads, calls);
        }
    }
}

/// The text a node was parsed from, from the first place DuckDB gives it or its parts to
/// the last. An operator's own place is the operator alone: `+` in `1 + 2`.
fn written<'t>(v: &Value, text: &'t str) -> &'t str {
    fn span(v: &Value) -> Option<(usize, usize)> {
        let start = v["query_location"]
            .as_u64()
            .filter(|&start| start != u64::MAX);
        let own = start
            .zip(v["query_location_length"].as_u64())
            .map(|(start, len)| (start as usize, start.saturating_add(len) as usize));
        let parts: Vec<&Value> = match v {
            Value::Object(node) => node.values().collect(),
            Value::Array(items) => items.iter().collect(),
            _ => Vec::new(),
        };
        let spans = own.into_iter().chain(parts.into_iter().filter_map(span));
        spans.reduce(|(s1, e1), (s2, e2)| (s1.min(s2), e1.max(e2)))
    }
    span(v)
        .and_then(|(start, end)| text.get(start..end))
        .unwrap_or_default()
}

fn base_table(node: &Value, text: &str, with: &[String]) -> Option<Relation> {
    let part = |key: &str| {
        node[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let name = part("table_name")?;
    let schema = part("schema_name");
    let catalog = part("catalog_name");
    if schema.is_none() && catalog.is_none() && with.contains(&name.to_lowercase()) {
        return None;
    }
    // `FROM 'data.csv'` names a file DuckDB scans; `FROM "data.csv"` names a table.
    if written(node, text).starts_with('\'') {
        return Some(Relation::File(name));
    }
    Some(Relation::Table(TableName {
        catalog,
        schema,
        name,
    }))
}

fn table_call(function: &Value, text: &str) -> Option<TableCall> {
    let name = function["function_name"].as_str()?;
    let args = function["arguments"].as_array().into_iter().flatten();
    Some(TableCall {
        function: name.to_string(),
        args: args.map(|a| arg(a, text)).collect(),
    })
}

fn arg(entry: &Value, text: &str) -> Arg {
    let expression = &entry["expression"];
    if let Some(name) = entry["name"].as_str().filter(|s| !s.is_empty()) {
        return Arg {
            name: Some(name.to_string()),
            value: arg_value(expression, text),
        };
    }
    // DuckDB binds `name = value` in a table function's arguments as a named parameter.
    let left = expression["left"]["column_names"].as_array();
    if expression["type"] == "COMPARE_EQUAL"
        && let Some([Value::String(name)]) = left.map(Vec::as_slice)
    {
        return Arg {
            name: Some(name.clone()),
            value: arg_value(&expression["right"], text),
        };
    }
    Arg {
        name: None,
        value: arg_value(expression, text),
    }
}

fn arg_value(expression: &Value, text: &str) -> ArgValue {
    let written = written(expression, text);
    let literal = &expression["literal"];
    let literal_text = literal["text"].as_str().unwrap_or_default().to_string();
    let other = || ArgValue::Expression {
        text: written.to_string(),
        quotes: quotes(expression, text),
    };
    match (expression["class"].as_str(), literal["kind"].as_str()) {
        (Some("CONSTANT"), Some("INTEGER" | "NUMERIC")) => ArgValue::Number(literal_text),
        (Some("CONSTANT"), Some("STRING")) => ArgValue::String(literal_text),
        (Some("COLUMN_REF"), _) => match expression["column_names"].as_array().map(Vec::as_slice) {
            Some([Value::String(name)]) if written.starts_with('"') => {
                ArgValue::QuotedIdentifier(name.clone())
            }
            Some([Value::String(name)]) => ArgValue::Identifier(name.clone()),
            _ => other(),
        },
        // `['a.json', 'b.json']` is DuckDB's `list_value` of its elements.
        (Some("FUNCTION"), _)
            if expression["function_name"] == "list_value"
                && expression["is_operator"] == Value::Bool(false) =>
        {
            let items = expression["arguments"].as_array().into_iter().flatten();
            ArgValue::List(items.map(|a| arg_value(&a["expression"], text)).collect())
        }
        _ => other(),
    }
}

/// Whether an expression holds a string, or a double-quoted name, at any depth.
fn quotes(v: &Value, text: &str) -> bool {
    match v {
        Value::Array(items) => items.iter().any(|item| quotes(item, text)),
        Value::Object(node) => {
            (v["class"] == "CONSTANT" && v["literal"]["kind"] == "STRING")
                || (v["class"] == "COLUMN_REF" && written(v, text).contains('"'))
                || node.values().any(|child| quotes(child, text))
        }
        _ => false,
    }
}

/// Steps `crate::introspect`'s tests read through the reader, and the reading of each
/// from the answers the preview gave. Each is read by a check below, so it is recorded.
#[cfg(test)]
pub(crate) mod recorded_steps {
    pub(crate) const ASOF: &str =
        "CREATE TABLE r AS SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k);";
    pub(crate) const READ_XLSX: &str =
        "CREATE TABLE b AS SELECT * FROM read_xlsx('build/budget.xlsx');";
    pub(crate) const RANGE: &str = "CREATE TABLE g AS SELECT * FROM range(10);";
    pub(crate) const MLPACK: &str = "CREATE TABLE m AS SELECT * FROM \
         mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\");";
    pub(crate) const COPY_QUERY: &str = "COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER);";
    pub(crate) const COPY_PARTITIONED: &str =
        "COPY (SELECT * FROM a) TO 'parts' (FORMAT parquet, PARTITION_BY (region));";
    pub(crate) const COPY_ONE_FILE_PER_THREAD_OFF: &str =
        "COPY a TO 'one.parquet' (FORMAT parquet, PER_THREAD_OUTPUT false);";
    pub(crate) const PIVOT_JOIN: &str = "PIVOT t JOIN u USING (k) ON c USING sum(v);";

    /// The steps above, each with the form, reads and produces of its one statement.
    pub(crate) const ALL: [(&str, &str, &[&str], &[&str]); 8] = [
        (ASOF, "CREATE", &["t", "u"], &["r"]),
        (READ_XLSX, "CREATE", &[], &["b"]),
        (RANGE, "CREATE", &[], &["g"]),
        (MLPACK, "CREATE", &[], &["m"]),
        (COPY_QUERY, "COPY", &["a"], &["file 'out.csv'"]),
        (COPY_PARTITIONED, "COPY", &["a"], &["file 'parts'"]),
        (
            COPY_ONE_FILE_PER_THREAD_OFF,
            "COPY",
            &["a"],
            &["file 'one.parquet'"],
        ),
        (PIVOT_JOIN, "PIVOT", &[], &[]),
    ];

    /// The reading of `sql`, one of the steps above, from the recorded answers.
    pub(crate) fn read(sql: &str) -> Vec<super::Statement> {
        super::read_step_with(&mut super::tests::Recorded::load(), sql)
            .unwrap_or_else(|e| panic!("reading {sql:?} was refused: {e}"))
            .statements
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// Names the DuckDB 2.0 build the ignored tests ask. `ci.yml` downloads the preview
    /// and sets it.
    const PREVIEW_ENV: &str = "ARC_DUCKDB_PREVIEW_BIN";

    /// What the 2.0 preview answered for each step the checks below read and each text
    /// the reader asked about, keyed by that text. The ignored test
    /// `the_recorded_answers_are_what_the_preview_answers` compares these with what the
    /// preview answers now, and prints a fresh copy when they differ.
    const RECORDED: &str = r##"{"version":"v2.0.0-alpha43569",
"tokens":{
"-- nothing but a comment\n":[{"start":0,"token_type":"COMMENT","word":"-- nothing but a comment\n"}],
"COPY s.t (a) FROM 'in.csv' (HEADER);\nCOPY t TO 'it''s.csv';\nCOPY FROM DATABASE a TO b;\nCOPY (SELECT * FROM a) TO out_csv;\nCOPY t TO out_csv;":[{"start":0,"token_type":"KEYWORD","word":"COPY"},{"start":5,"token_type":"SCHEMA_NAME","word":"s"},{"start":6,"token_type":"NUMBER_LITERAL","word":"."},{"start":7,"token_type":"TABLE_NAME","word":"t"},{"start":9,"token_type":"OPERATOR","word":"("},{"start":10,"token_type":"IDENTIFIER","word":"a"},{"start":11,"token_type":"OPERATOR","word":")"},{"start":13,"token_type":"KEYWORD","word":"FROM"},{"start":18,"token_type":"STRING_LITERAL","word":"'in.csv'"},{"start":27,"token_type":"OPERATOR","word":"("},{"start":28,"token_type":"IDENTIFIER","word":"HEADER"},{"start":34,"token_type":"OPERATOR","word":")"},{"start":35,"token_type":"TERMINATOR","word":";"},{"start":37,"token_type":"KEYWORD","word":"COPY"},{"start":42,"token_type":"TABLE_NAME","word":"t"},{"start":44,"token_type":"KEYWORD","word":"TO"},{"start":47,"token_type":"STRING_LITERAL","word":"'it''s.csv'"},{"start":58,"token_type":"TERMINATOR","word":";"},{"start":60,"token_type":"KEYWORD","word":"COPY"},{"start":65,"token_type":"KEYWORD","word":"FROM"},{"start":70,"token_type":"KEYWORD","word":"DATABASE"},{"start":79,"token_type":"IDENTIFIER","word":"a"},{"start":81,"token_type":"KEYWORD","word":"TO"},{"start":84,"token_type":"IDENTIFIER","word":"b"},{"start":85,"token_type":"TERMINATOR","word":";"},{"start":87,"token_type":"KEYWORD","word":"COPY"},{"start":92,"token_type":"OPERATOR","word":"("},{"start":93,"token_type":"KEYWORD","word":"SELECT"},{"start":100,"token_type":"OPERATOR","word":"*"},{"start":102,"token_type":"KEYWORD","word":"FROM"},{"start":107,"token_type":"TABLE_NAME","word":"a"},{"start":108,"token_type":"OPERATOR","word":")"},{"start":110,"token_type":"KEYWORD","word":"TO"},{"start":113,"token_type":"IDENTIFIER","word":"out_csv"},{"start":120,"token_type":"TERMINATOR","word":";"},{"start":122,"token_type":"KEYWORD","word":"COPY"},{"start":127,"token_type":"TABLE_NAME","word":"t"},{"start":129,"token_type":"KEYWORD","word":"TO"},{"start":132,"token_type":"IDENTIFIER","word":"out_csv"},{"start":139,"token_type":"TERMINATOR","word":";"}],
"CREATE OR REPLACE TEMP TABLE \"R \"\"x\"\"\" AS SELECT * FROM a;\nCREATE TABLE IF NOT EXISTS db.s.r2 (a INT);\nCREATE VIEW v (x) AS SELECT * FROM b;\nCREATE TEMPORARY VIEW w AS FROM c;\nCREATE MACRO m() AS 1;":[{"start":0,"token_type":"KEYWORD","word":"CREATE"},{"start":7,"token_type":"KEYWORD","word":"OR"},{"start":10,"token_type":"KEYWORD","word":"REPLACE"},{"start":18,"token_type":"KEYWORD","word":"TEMP"},{"start":23,"token_type":"KEYWORD","word":"TABLE"},{"start":29,"token_type":"IDENTIFIER","word":"\"R \"\"x\"\"\""},{"start":39,"token_type":"KEYWORD","word":"AS"},{"start":42,"token_type":"KEYWORD","word":"SELECT"},{"start":49,"token_type":"OPERATOR","word":"*"},{"start":51,"token_type":"KEYWORD","word":"FROM"},{"start":56,"token_type":"TABLE_NAME","word":"a"},{"start":57,"token_type":"TERMINATOR","word":";"},{"start":59,"token_type":"KEYWORD","word":"CREATE"},{"start":66,"token_type":"KEYWORD","word":"TABLE"},{"start":72,"token_type":"KEYWORD","word":"IF"},{"start":75,"token_type":"KEYWORD","word":"NOT"},{"start":79,"token_type":"KEYWORD","word":"EXISTS"},{"start":86,"token_type":"CATALOG_NAME","word":"db"},{"start":88,"token_type":"NUMBER_LITERAL","word":"."},{"start":89,"token_type":"SCHEMA_NAME","word":"s"},{"start":90,"token_type":"NUMBER_LITERAL","word":"."},{"start":91,"token_type":"IDENTIFIER","word":"r2"},{"start":94,"token_type":"OPERATOR","word":"("},{"start":95,"token_type":"IDENTIFIER","word":"a"},{"start":97,"token_type":"KEYWORD","word":"INT"},{"start":100,"token_type":"OPERATOR","word":")"},{"start":101,"token_type":"TERMINATOR","word":";"},{"start":103,"token_type":"KEYWORD","word":"CREATE"},{"start":110,"token_type":"KEYWORD","word":"VIEW"},{"start":115,"token_type":"IDENTIFIER","word":"v"},{"start":117,"token_type":"OPERATOR","word":"("},{"start":118,"token_type":"IDENTIFIER","word":"x"},{"start":119,"token_type":"OPERATOR","word":")"},{"start":121,"token_type":"KEYWORD","word":"AS"},{"start":124,"token_type":"KEYWORD","word":"SELECT"},{"start":131,"token_type":"OPERATOR","word":"*"},{"start":133,"token_type":"KEYWORD","word":"FROM"},{"start":138,"token_type":"TABLE_NAME","word":"b"},{"start":139,"token_type":"TERMINATOR","word":";"},{"start":141,"token_type":"KEYWORD","word":"CREATE"},{"start":148,"token_type":"KEYWORD","word":"TEMPORARY"},{"start":158,"token_type":"KEYWORD","word":"VIEW"},{"start":163,"token_type":"IDENTIFIER","word":"w"},{"start":165,"token_type":"KEYWORD","word":"AS"},{"start":168,"token_type":"KEYWORD","word":"FROM"},{"start":173,"token_type":"TABLE_NAME","word":"c"},{"start":174,"token_type":"TERMINATOR","word":";"},{"start":176,"token_type":"KEYWORD","word":"CREATE"},{"start":183,"token_type":"KEYWORD","word":"MACRO"},{"start":189,"token_type":"IDENTIFIER","word":"m"},{"start":190,"token_type":"OPERATOR","word":"("},{"start":191,"token_type":"OPERATOR","word":")"},{"start":193,"token_type":"KEYWORD","word":"AS"},{"start":196,"token_type":"NUMBER_LITERAL","word":"1"},{"start":197,"token_type":"TERMINATOR","word":";"}],
"CREATE TABLE r AS SELECT * FROM a JOIN b USING (k) WHERE x IN (SELECT x FROM c);":[{"start":0,"token_type":"KEYWORD","word":"CREATE"},{"start":7,"token_type":"KEYWORD","word":"TABLE"},{"start":13,"token_type":"IDENTIFIER","word":"r"},{"start":15,"token_type":"KEYWORD","word":"AS"},{"start":18,"token_type":"KEYWORD","word":"SELECT"},{"start":25,"token_type":"OPERATOR","word":"*"},{"start":27,"token_type":"KEYWORD","word":"FROM"},{"start":32,"token_type":"TABLE_NAME","word":"a"},{"start":34,"token_type":"KEYWORD","word":"JOIN"},{"start":39,"token_type":"TABLE_NAME","word":"b"},{"start":41,"token_type":"KEYWORD","word":"USING"},{"start":47,"token_type":"OPERATOR","word":"("},{"start":48,"token_type":"COLUMN_NAME","word":"k"},{"start":49,"token_type":"OPERATOR","word":")"},{"start":51,"token_type":"KEYWORD","word":"WHERE"},{"start":57,"token_type":"COLUMN_NAME","word":"x"},{"start":59,"token_type":"KEYWORD","word":"IN"},{"start":62,"token_type":"OPERATOR","word":"("},{"start":63,"token_type":"KEYWORD","word":"SELECT"},{"start":70,"token_type":"COLUMN_NAME","word":"x"},{"start":72,"token_type":"KEYWORD","word":"FROM"},{"start":77,"token_type":"TABLE_NAME","word":"c"},{"start":78,"token_type":"OPERATOR","word":")"},{"start":79,"token_type":"TERMINATOR","word":";"}],
"INSERT INTO s.t AS x (a, b) SELECT * FROM a ON CONFLICT DO NOTHING RETURNING *;\nINSERT OR IGNORE INTO t BY NAME SELECT * FROM b RETURNING (SELECT 1 FROM z);\nINSERT INTO t (SELECT * FROM c);\nINSERT INTO t DEFAULT VALUES;\nINSERT OR REPLACE INTO t BY POSITION VALUES (1);":[{"start":0,"token_type":"KEYWORD","word":"INSERT"},{"start":7,"token_type":"KEYWORD","word":"INTO"},{"start":12,"token_type":"SCHEMA_NAME","word":"s"},{"start":13,"token_type":"NUMBER_LITERAL","word":"."},{"start":14,"token_type":"TABLE_NAME","word":"t"},{"start":16,"token_type":"KEYWORD","word":"AS"},{"start":19,"token_type":"IDENTIFIER","word":"x"},{"start":21,"token_type":"OPERATOR","word":"("},{"start":22,"token_type":"IDENTIFIER","word":"a"},{"start":23,"token_type":"OPERATOR","word":","},{"start":25,"token_type":"IDENTIFIER","word":"b"},{"start":26,"token_type":"OPERATOR","word":")"},{"start":28,"token_type":"KEYWORD","word":"SELECT"},{"start":35,"token_type":"OPERATOR","word":"*"},{"start":37,"token_type":"KEYWORD","word":"FROM"},{"start":42,"token_type":"TABLE_NAME","word":"a"},{"start":44,"token_type":"KEYWORD","word":"ON"},{"start":47,"token_type":"KEYWORD","word":"CONFLICT"},{"start":56,"token_type":"KEYWORD","word":"DO"},{"start":59,"token_type":"KEYWORD","word":"NOTHING"},{"start":67,"token_type":"KEYWORD","word":"RETURNING"},{"start":77,"token_type":"OPERATOR","word":"*"},{"start":78,"token_type":"TERMINATOR","word":";"},{"start":80,"token_type":"KEYWORD","word":"INSERT"},{"start":87,"token_type":"KEYWORD","word":"OR"},{"start":90,"token_type":"KEYWORD","word":"IGNORE"},{"start":97,"token_type":"KEYWORD","word":"INTO"},{"start":102,"token_type":"TABLE_NAME","word":"t"},{"start":104,"token_type":"KEYWORD","word":"BY"},{"start":107,"token_type":"KEYWORD","word":"NAME"},{"start":112,"token_type":"KEYWORD","word":"SELECT"},{"start":119,"token_type":"OPERATOR","word":"*"},{"start":121,"token_type":"KEYWORD","word":"FROM"},{"start":126,"token_type":"TABLE_NAME","word":"b"},{"start":128,"token_type":"KEYWORD","word":"RETURNING"},{"start":138,"token_type":"OPERATOR","word":"("},{"start":139,"token_type":"KEYWORD","word":"SELECT"},{"start":146,"token_type":"NUMBER_LITERAL","word":"1"},{"start":148,"token_type":"KEYWORD","word":"FROM"},{"start":153,"token_type":"TABLE_NAME","word":"z"},{"start":154,"token_type":"OPERATOR","word":")"},{"start":155,"token_type":"TERMINATOR","word":";"},{"start":157,"token_type":"KEYWORD","word":"INSERT"},{"start":164,"token_type":"KEYWORD","word":"INTO"},{"start":169,"token_type":"TABLE_NAME","word":"t"},{"start":171,"token_type":"OPERATOR","word":"("},{"start":172,"token_type":"KEYWORD","word":"SELECT"},{"start":179,"token_type":"OPERATOR","word":"*"},{"start":181,"token_type":"KEYWORD","word":"FROM"},{"start":186,"token_type":"TABLE_NAME","word":"c"},{"start":187,"token_type":"OPERATOR","word":")"},{"start":188,"token_type":"TERMINATOR","word":";"},{"start":190,"token_type":"KEYWORD","word":"INSERT"},{"start":197,"token_type":"KEYWORD","word":"INTO"},{"start":202,"token_type":"TABLE_NAME","word":"t"},{"start":204,"token_type":"KEYWORD","word":"DEFAULT"},{"start":212,"token_type":"KEYWORD","word":"VALUES"},{"start":218,"token_type":"TERMINATOR","word":";"},{"start":220,"token_type":"KEYWORD","word":"INSERT"},{"start":227,"token_type":"KEYWORD","word":"OR"},{"start":230,"token_type":"KEYWORD","word":"REPLACE"},{"start":238,"token_type":"KEYWORD","word":"INTO"},{"start":243,"token_type":"TABLE_NAME","word":"t"},{"start":245,"token_type":"KEYWORD","word":"BY"},{"start":248,"token_type":"KEYWORD","word":"POSITION"},{"start":257,"token_type":"KEYWORD","word":"VALUES"},{"start":264,"token_type":"OPERATOR","word":"("},{"start":265,"token_type":"NUMBER_LITERAL","word":"1"},{"start":266,"token_type":"OPERATOR","word":")"},{"start":267,"token_type":"TERMINATOR","word":";"}],
"INSERT INTO t SELECT * FROM a;\nCREATE VIEW v AS SELECT * FROM a;\nCOPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER);\nCOPY a TO 'out.csv' (HEADER);\nCOPY t FROM 'in.csv';":[{"start":0,"token_type":"KEYWORD","word":"INSERT"},{"start":7,"token_type":"KEYWORD","word":"INTO"},{"start":12,"token_type":"TABLE_NAME","word":"t"},{"start":14,"token_type":"KEYWORD","word":"SELECT"},{"start":21,"token_type":"OPERATOR","word":"*"},{"start":23,"token_type":"KEYWORD","word":"FROM"},{"start":28,"token_type":"TABLE_NAME","word":"a"},{"start":29,"token_type":"TERMINATOR","word":";"},{"start":31,"token_type":"KEYWORD","word":"CREATE"},{"start":38,"token_type":"KEYWORD","word":"VIEW"},{"start":43,"token_type":"IDENTIFIER","word":"v"},{"start":45,"token_type":"KEYWORD","word":"AS"},{"start":48,"token_type":"KEYWORD","word":"SELECT"},{"start":55,"token_type":"OPERATOR","word":"*"},{"start":57,"token_type":"KEYWORD","word":"FROM"},{"start":62,"token_type":"TABLE_NAME","word":"a"},{"start":63,"token_type":"TERMINATOR","word":";"},{"start":65,"token_type":"KEYWORD","word":"COPY"},{"start":70,"token_type":"OPERATOR","word":"("},{"start":71,"token_type":"KEYWORD","word":"SELECT"},{"start":78,"token_type":"OPERATOR","word":"*"},{"start":80,"token_type":"KEYWORD","word":"FROM"},{"start":85,"token_type":"TABLE_NAME","word":"a"},{"start":87,"token_type":"KEYWORD","word":"WHERE"},{"start":93,"token_type":"COLUMN_NAME","word":"x"},{"start":95,"token_type":"OPERATOR","word":">"},{"start":97,"token_type":"NUMBER_LITERAL","word":"1"},{"start":98,"token_type":"OPERATOR","word":")"},{"start":100,"token_type":"KEYWORD","word":"TO"},{"start":103,"token_type":"STRING_LITERAL","word":"'out.csv'"},{"start":113,"token_type":"OPERATOR","word":"("},{"start":114,"token_type":"IDENTIFIER","word":"HEADER"},{"start":120,"token_type":"OPERATOR","word":")"},{"start":121,"token_type":"TERMINATOR","word":";"},{"start":123,"token_type":"KEYWORD","word":"COPY"},{"start":128,"token_type":"TABLE_NAME","word":"a"},{"start":130,"token_type":"KEYWORD","word":"TO"},{"start":133,"token_type":"STRING_LITERAL","word":"'out.csv'"},{"start":143,"token_type":"OPERATOR","word":"("},{"start":144,"token_type":"IDENTIFIER","word":"HEADER"},{"start":150,"token_type":"OPERATOR","word":")"},{"start":151,"token_type":"TERMINATOR","word":";"},{"start":153,"token_type":"KEYWORD","word":"COPY"},{"start":158,"token_type":"TABLE_NAME","word":"t"},{"start":160,"token_type":"KEYWORD","word":"FROM"},{"start":165,"token_type":"STRING_LITERAL","word":"'in.csv'"},{"start":173,"token_type":"TERMINATOR","word":";"}],
"INSTALL mlpack FROM community; LOAD mlpack; SET VARIABLE cutoff = DATE '2026-01-01'; PRAGMA threads = 4; CREATE TABLE r AS SELECT * FROM a;":[{"start":0,"token_type":"KEYWORD","word":"INSTALL"},{"start":8,"token_type":"IDENTIFIER","word":"mlpack"},{"start":15,"token_type":"KEYWORD","word":"FROM"},{"start":20,"token_type":"IDENTIFIER","word":"community"},{"start":29,"token_type":"TERMINATOR","word":";"},{"start":31,"token_type":"KEYWORD","word":"LOAD"},{"start":36,"token_type":"IDENTIFIER","word":"mlpack"},{"start":42,"token_type":"TERMINATOR","word":";"},{"start":44,"token_type":"KEYWORD","word":"SET"},{"start":48,"token_type":"KEYWORD","word":"VARIABLE"},{"start":57,"token_type":"IDENTIFIER","word":"cutoff"},{"start":64,"token_type":"OPERATOR","word":"="},{"start":66,"token_type":"TYPE_NAME","word":"DATE"},{"start":71,"token_type":"STRING_LITERAL","word":"'2026-01-01'"},{"start":83,"token_type":"TERMINATOR","word":";"},{"start":85,"token_type":"KEYWORD","word":"PRAGMA"},{"start":92,"token_type":"SETTING_NAME","word":"threads"},{"start":100,"token_type":"OPERATOR","word":"="},{"start":102,"token_type":"NUMBER_LITERAL","word":"4"},{"start":103,"token_type":"TERMINATOR","word":";"},{"start":105,"token_type":"KEYWORD","word":"CREATE"},{"start":112,"token_type":"KEYWORD","word":"TABLE"},{"start":118,"token_type":"IDENTIFIER","word":"r"},{"start":120,"token_type":"KEYWORD","word":"AS"},{"start":123,"token_type":"KEYWORD","word":"SELECT"},{"start":130,"token_type":"OPERATOR","word":"*"},{"start":132,"token_type":"KEYWORD","word":"FROM"},{"start":137,"token_type":"TABLE_NAME","word":"a"},{"start":138,"token_type":"TERMINATOR","word":";"}],
"PIVOT (SELECT * FROM a) ON c USING sum(v);\nPIVOT 'p.csv' ON c USING sum(v);\nPIVOT_WIDER s.t ON c USING sum(v);\nUNPIVOT u ON a, b INTO NAME n VALUE v;\nPIVOT read_csv('p.csv') ON c USING sum(v);":[{"start":0,"token_type":"KEYWORD","word":"PIVOT"},{"start":6,"token_type":"OPERATOR","word":"("},{"start":7,"token_type":"KEYWORD","word":"SELECT"},{"start":14,"token_type":"OPERATOR","word":"*"},{"start":16,"token_type":"KEYWORD","word":"FROM"},{"start":21,"token_type":"TABLE_NAME","word":"a"},{"start":22,"token_type":"OPERATOR","word":")"},{"start":24,"token_type":"KEYWORD","word":"ON"},{"start":27,"token_type":"COLUMN_NAME","word":"c"},{"start":29,"token_type":"KEYWORD","word":"USING"},{"start":35,"token_type":"SCALAR_FUNCTION","word":"sum"},{"start":38,"token_type":"OPERATOR","word":"("},{"start":39,"token_type":"COLUMN_NAME","word":"v"},{"start":40,"token_type":"OPERATOR","word":")"},{"start":41,"token_type":"TERMINATOR","word":";"},{"start":43,"token_type":"KEYWORD","word":"PIVOT"},{"start":49,"token_type":"TABLE_NAME","word":"'p.csv'"},{"start":57,"token_type":"KEYWORD","word":"ON"},{"start":60,"token_type":"COLUMN_NAME","word":"c"},{"start":62,"token_type":"KEYWORD","word":"USING"},{"start":68,"token_type":"SCALAR_FUNCTION","word":"sum"},{"start":71,"token_type":"OPERATOR","word":"("},{"start":72,"token_type":"COLUMN_NAME","word":"v"},{"start":73,"token_type":"OPERATOR","word":")"},{"start":74,"token_type":"TERMINATOR","word":";"},{"start":76,"token_type":"KEYWORD","word":"PIVOT_WIDER"},{"start":88,"token_type":"SCHEMA_NAME","word":"s"},{"start":89,"token_type":"NUMBER_LITERAL","word":"."},{"start":90,"token_type":"TABLE_NAME","word":"t"},{"start":92,"token_type":"KEYWORD","word":"ON"},{"start":95,"token_type":"COLUMN_NAME","word":"c"},{"start":97,"token_type":"KEYWORD","word":"USING"},{"start":103,"token_type":"SCALAR_FUNCTION","word":"sum"},{"start":106,"token_type":"OPERATOR","word":"("},{"start":107,"token_type":"COLUMN_NAME","word":"v"},{"start":108,"token_type":"OPERATOR","word":")"},{"start":109,"token_type":"TERMINATOR","word":";"},{"start":111,"token_type":"KEYWORD","word":"UNPIVOT"},{"start":119,"token_type":"TABLE_NAME","word":"u"},{"start":121,"token_type":"KEYWORD","word":"ON"},{"start":124,"token_type":"COLUMN_NAME","word":"a"},{"start":125,"token_type":"OPERATOR","word":","},{"start":127,"token_type":"COLUMN_NAME","word":"b"},{"start":129,"token_type":"KEYWORD","word":"INTO"},{"start":134,"token_type":"KEYWORD","word":"NAME"},{"start":139,"token_type":"IDENTIFIER","word":"n"},{"start":141,"token_type":"KEYWORD","word":"VALUE"},{"start":147,"token_type":"IDENTIFIER","word":"v"},{"start":148,"token_type":"TERMINATOR","word":";"},{"start":150,"token_type":"KEYWORD","word":"PIVOT"},{"start":156,"token_type":"TABLE_FUNCTION","word":"read_csv"},{"start":164,"token_type":"OPERATOR","word":"("},{"start":165,"token_type":"STRING_LITERAL","word":"'p.csv'"},{"start":172,"token_type":"OPERATOR","word":")"},{"start":174,"token_type":"KEYWORD","word":"ON"},{"start":177,"token_type":"COLUMN_NAME","word":"c"},{"start":179,"token_type":"KEYWORD","word":"USING"},{"start":185,"token_type":"SCALAR_FUNCTION","word":"sum"},{"start":188,"token_type":"OPERATOR","word":"("},{"start":189,"token_type":"COLUMN_NAME","word":"v"},{"start":190,"token_type":"OPERATOR","word":")"},{"start":191,"token_type":"TERMINATOR","word":";"}],
"SELECT 'a;b' AS s; -- c; d\nSELECT 2 /* ; */;":[{"start":0,"token_type":"KEYWORD","word":"SELECT"},{"start":7,"token_type":"STRING_LITERAL","word":"'a;b'"},{"start":13,"token_type":"KEYWORD","word":"AS"},{"start":16,"token_type":"IDENTIFIER","word":"s"},{"start":17,"token_type":"TERMINATOR","word":";"},{"start":19,"token_type":"COMMENT","word":"-- c; d\n"},{"start":27,"token_type":"KEYWORD","word":"SELECT"},{"start":34,"token_type":"NUMBER_LITERAL","word":"2"},{"start":36,"token_type":"COMMENT","word":"/* ; */"},{"start":43,"token_type":"TERMINATOR","word":";"}],
"SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k);\nSUMMARIZE t;\nPIVOT t ON c USING sum(v);":[{"start":0,"token_type":"KEYWORD","word":"SELECT"},{"start":7,"token_type":"OPERATOR","word":"*"},{"start":9,"token_type":"KEYWORD","word":"EXCLUDE"},{"start":17,"token_type":"OPERATOR","word":"("},{"start":18,"token_type":"IDENTIFIER","word":"a"},{"start":19,"token_type":"OPERATOR","word":")"},{"start":21,"token_type":"KEYWORD","word":"FROM"},{"start":26,"token_type":"TABLE_NAME","word":"t"},{"start":28,"token_type":"KEYWORD","word":"ASOF"},{"start":33,"token_type":"KEYWORD","word":"LEFT"},{"start":38,"token_type":"KEYWORD","word":"JOIN"},{"start":43,"token_type":"TABLE_NAME","word":"u"},{"start":45,"token_type":"KEYWORD","word":"USING"},{"start":51,"token_type":"OPERATOR","word":"("},{"start":52,"token_type":"COLUMN_NAME","word":"k"},{"start":53,"token_type":"OPERATOR","word":")"},{"start":54,"token_type":"TERMINATOR","word":";"},{"start":56,"token_type":"KEYWORD","word":"SUMMARIZE"},{"start":66,"token_type":"TABLE_NAME","word":"t"},{"start":67,"token_type":"TERMINATOR","word":";"},{"start":69,"token_type":"KEYWORD","word":"PIVOT"},{"start":75,"token_type":"TABLE_NAME","word":"t"},{"start":77,"token_type":"KEYWORD","word":"ON"},{"start":80,"token_type":"COLUMN_NAME","word":"c"},{"start":82,"token_type":"KEYWORD","word":"USING"},{"start":88,"token_type":"SCALAR_FUNCTION","word":"sum"},{"start":91,"token_type":"OPERATOR","word":"("},{"start":92,"token_type":"COLUMN_NAME","word":"v"},{"start":93,"token_type":"OPERATOR","word":")"},{"start":94,"token_type":"TERMINATOR","word":";"}],
"SELECT * FROM a; /* trailing */\nSELECT * FROM b":[{"start":0,"token_type":"KEYWORD","word":"SELECT"},{"start":7,"token_type":"OPERATOR","word":"*"},{"start":9,"token_type":"KEYWORD","word":"FROM"},{"start":14,"token_type":"TABLE_NAME","word":"a"},{"start":15,"token_type":"TERMINATOR","word":";"},{"start":17,"token_type":"COMMENT","word":"/* trailing */"},{"start":32,"token_type":"KEYWORD","word":"SELECT"},{"start":39,"token_type":"OPERATOR","word":"*"},{"start":41,"token_type":"KEYWORD","word":"FROM"},{"start":46,"token_type":"IDENTIFIER","word":"b"}],
"SELECT * FROM range(3), read_csv('x.csv'), mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\");":[{"start":0,"token_type":"KEYWORD","word":"SELECT"},{"start":7,"token_type":"OPERATOR","word":"*"},{"start":9,"token_type":"KEYWORD","word":"FROM"},{"start":14,"token_type":"TABLE_FUNCTION","word":"range"},{"start":19,"token_type":"OPERATOR","word":"("},{"start":20,"token_type":"NUMBER_LITERAL","word":"3"},{"start":21,"token_type":"OPERATOR","word":")"},{"start":22,"token_type":"OPERATOR","word":","},{"start":24,"token_type":"TABLE_FUNCTION","word":"read_csv"},{"start":32,"token_type":"OPERATOR","word":"("},{"start":33,"token_type":"STRING_LITERAL","word":"'x.csv'"},{"start":40,"token_type":"OPERATOR","word":")"},{"start":41,"token_type":"OPERATOR","word":","},{"start":43,"token_type":"TABLE_FUNCTION","word":"mlpack_random_forest_train"},{"start":69,"token_type":"OPERATOR","word":"("},{"start":70,"token_type":"COLUMN_NAME","word":"\"X\""},{"start":73,"token_type":"OPERATOR","word":","},{"start":75,"token_type":"COLUMN_NAME","word":"\"Y\""},{"start":78,"token_type":"OPERATOR","word":","},{"start":80,"token_type":"COLUMN_NAME","word":"\"params\""},{"start":88,"token_type":"OPERATOR","word":","},{"start":90,"token_type":"COLUMN_NAME","word":"\"model\""},{"start":97,"token_type":"OPERATOR","word":")"},{"start":98,"token_type":"TERMINATOR","word":";"}],
"SELECT * FROM read_csv('x.csv', header = true, sep := ',', \"quote\" = 'q'), f(bare, 1 + 2, t.col, 1.5, NULL);":[{"start":0,"token_type":"KEYWORD","word":"SELECT"},{"start":7,"token_type":"OPERATOR","word":"*"},{"start":9,"token_type":"KEYWORD","word":"FROM"},{"start":14,"token_type":"TABLE_FUNCTION","word":"read_csv"},{"start":22,"token_type":"OPERATOR","word":"("},{"start":23,"token_type":"STRING_LITERAL","word":"'x.csv'"},{"start":30,"token_type":"OPERATOR","word":","},{"start":32,"token_type":"COLUMN_NAME","word":"header"},{"start":39,"token_type":"OPERATOR","word":"="},{"start":41,"token_type":"KEYWORD","word":"true"},{"start":45,"token_type":"OPERATOR","word":","},{"start":47,"token_type":"IDENTIFIER","word":"sep"},{"start":51,"token_type":"OPERATOR","word":":="},{"start":54,"token_type":"STRING_LITERAL","word":"','"},{"start":57,"token_type":"OPERATOR","word":","},{"start":59,"token_type":"COLUMN_NAME","word":"\"quote\""},{"start":67,"token_type":"OPERATOR","word":"="},{"start":69,"token_type":"STRING_LITERAL","word":"'q'"},{"start":72,"token_type":"OPERATOR","word":")"},{"start":73,"token_type":"OPERATOR","word":","},{"start":75,"token_type":"TABLE_FUNCTION","word":"f"},{"start":76,"token_type":"OPERATOR","word":"("},{"start":77,"token_type":"COLUMN_NAME","word":"bare"},{"start":81,"token_type":"OPERATOR","word":","},{"start":83,"token_type":"NUMBER_LITERAL","word":"1"},{"start":85,"token_type":"OPERATOR","word":"+"},{"start":87,"token_type":"NUMBER_LITERAL","word":"2"},{"start":88,"token_type":"OPERATOR","word":","},{"start":90,"token_type":"TABLE_NAME","word":"t"},{"start":91,"token_type":"NUMBER_LITERAL","word":"."},{"start":92,"token_type":"COLUMN_NAME","word":"col"},{"start":95,"token_type":"OPERATOR","word":","},{"start":97,"token_type":"NUMBER_LITERAL","word":"1.5"},{"start":100,"token_type":"OPERATOR","word":","},{"start":102,"token_type":"KEYWORD","word":"NULL"},{"start":106,"token_type":"OPERATOR","word":")"},{"start":107,"token_type":"TERMINATOR","word":";"}],
"SELECT 1;\nCREATE TABL r AS SELECT 1;":[{"start":0,"token_type":"KEYWORD","word":"SELECT"},{"start":7,"token_type":"NUMBER_LITERAL","word":"1"},{"start":8,"token_type":"TERMINATOR","word":";"},{"start":10,"token_type":"KEYWORD","word":"CREATE"},{"start":17,"token_type":"IDENTIFIER","word":"TABL"},{"start":22,"token_type":"IDENTIFIER","word":"r"},{"start":24,"token_type":"KEYWORD","word":"AS"},{"start":27,"token_type":"KEYWORD","word":"SELECT"},{"start":34,"token_type":"NUMBER_LITERAL","word":"1"},{"start":35,"token_type":"TERMINATOR","word":";"}],
"SET VARIABLE m = (SELECT max(d) FROM events);\nSET threads TO 4;\nFORCE INSTALL spatial;\nDELETE FROM t USING u WHERE t.k = u.k;\nCREATE TABLE r AS PIVOT t ON c USING sum(v);":[{"start":0,"token_type":"KEYWORD","word":"SET"},{"start":4,"token_type":"KEYWORD","word":"VARIABLE"},{"start":13,"token_type":"IDENTIFIER","word":"m"},{"start":15,"token_type":"OPERATOR","word":"="},{"start":17,"token_type":"OPERATOR","word":"("},{"start":18,"token_type":"KEYWORD","word":"SELECT"},{"start":25,"token_type":"SCALAR_FUNCTION","word":"max"},{"start":28,"token_type":"OPERATOR","word":"("},{"start":29,"token_type":"COLUMN_NAME","word":"d"},{"start":30,"token_type":"OPERATOR","word":")"},{"start":32,"token_type":"KEYWORD","word":"FROM"},{"start":37,"token_type":"TABLE_NAME","word":"events"},{"start":43,"token_type":"OPERATOR","word":")"},{"start":44,"token_type":"TERMINATOR","word":";"},{"start":46,"token_type":"KEYWORD","word":"SET"},{"start":50,"token_type":"SETTING_NAME","word":"threads"},{"start":58,"token_type":"KEYWORD","word":"TO"},{"start":61,"token_type":"NUMBER_LITERAL","word":"4"},{"start":62,"token_type":"TERMINATOR","word":";"},{"start":64,"token_type":"KEYWORD","word":"FORCE"},{"start":70,"token_type":"KEYWORD","word":"INSTALL"},{"start":78,"token_type":"IDENTIFIER","word":"spatial"},{"start":85,"token_type":"TERMINATOR","word":";"},{"start":87,"token_type":"KEYWORD","word":"DELETE"},{"start":94,"token_type":"KEYWORD","word":"FROM"},{"start":99,"token_type":"TABLE_NAME","word":"t"},{"start":101,"token_type":"KEYWORD","word":"USING"},{"start":107,"token_type":"TABLE_NAME","word":"u"},{"start":109,"token_type":"KEYWORD","word":"WHERE"},{"start":115,"token_type":"TABLE_NAME","word":"t"},{"start":116,"token_type":"NUMBER_LITERAL","word":"."},{"start":117,"token_type":"COLUMN_NAME","word":"k"},{"start":119,"token_type":"OPERATOR","word":"="},{"start":121,"token_type":"TABLE_NAME","word":"u"},{"start":122,"token_type":"NUMBER_LITERAL","word":"."},{"start":123,"token_type":"COLUMN_NAME","word":"k"},{"start":124,"token_type":"TERMINATOR","word":";"},{"start":126,"token_type":"KEYWORD","word":"CREATE"},{"start":133,"token_type":"KEYWORD","word":"TABLE"},{"start":139,"token_type":"IDENTIFIER","word":"r"},{"start":141,"token_type":"KEYWORD","word":"AS"},{"start":144,"token_type":"KEYWORD","word":"PIVOT"},{"start":150,"token_type":"TABLE_NAME","word":"t"},{"start":152,"token_type":"KEYWORD","word":"ON"},{"start":155,"token_type":"COLUMN_NAME","word":"c"},{"start":157,"token_type":"KEYWORD","word":"USING"},{"start":163,"token_type":"SCALAR_FUNCTION","word":"sum"},{"start":166,"token_type":"OPERATOR","word":"("},{"start":167,"token_type":"COLUMN_NAME","word":"v"},{"start":168,"token_type":"OPERATOR","word":")"},{"start":169,"token_type":"TERMINATOR","word":";"}],
"WITH c AS (SELECT * FROM a) SELECT * FROM c;":[{"start":0,"token_type":"KEYWORD","word":"WITH"},{"start":5,"token_type":"IDENTIFIER","word":"c"},{"start":7,"token_type":"KEYWORD","word":"AS"},{"start":10,"token_type":"OPERATOR","word":"("},{"start":11,"token_type":"KEYWORD","word":"SELECT"},{"start":18,"token_type":"OPERATOR","word":"*"},{"start":20,"token_type":"KEYWORD","word":"FROM"},{"start":25,"token_type":"TABLE_NAME","word":"a"},{"start":26,"token_type":"OPERATOR","word":")"},{"start":28,"token_type":"KEYWORD","word":"SELECT"},{"start":35,"token_type":"OPERATOR","word":"*"},{"start":37,"token_type":"KEYWORD","word":"FROM"},{"start":42,"token_type":"TABLE_NAME","word":"c"},{"start":43,"token_type":"TERMINATOR","word":";"}],
"WITH x AS (SELECT * FROM y), y AS (SELECT * FROM x) SELECT * FROM y;\nWITH a AS (SELECT * FROM a) SELECT * FROM a;\nWITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT * FROM r;\nWITH C AS (SELECT 1) SELECT * FROM main.c, c;":[{"start":0,"token_type":"KEYWORD","word":"WITH"},{"start":5,"token_type":"IDENTIFIER","word":"x"},{"start":7,"token_type":"KEYWORD","word":"AS"},{"start":10,"token_type":"OPERATOR","word":"("},{"start":11,"token_type":"KEYWORD","word":"SELECT"},{"start":18,"token_type":"OPERATOR","word":"*"},{"start":20,"token_type":"KEYWORD","word":"FROM"},{"start":25,"token_type":"TABLE_NAME","word":"y"},{"start":26,"token_type":"OPERATOR","word":")"},{"start":27,"token_type":"OPERATOR","word":","},{"start":29,"token_type":"IDENTIFIER","word":"y"},{"start":31,"token_type":"KEYWORD","word":"AS"},{"start":34,"token_type":"OPERATOR","word":"("},{"start":35,"token_type":"KEYWORD","word":"SELECT"},{"start":42,"token_type":"OPERATOR","word":"*"},{"start":44,"token_type":"KEYWORD","word":"FROM"},{"start":49,"token_type":"TABLE_NAME","word":"x"},{"start":50,"token_type":"OPERATOR","word":")"},{"start":52,"token_type":"KEYWORD","word":"SELECT"},{"start":59,"token_type":"OPERATOR","word":"*"},{"start":61,"token_type":"KEYWORD","word":"FROM"},{"start":66,"token_type":"TABLE_NAME","word":"y"},{"start":67,"token_type":"TERMINATOR","word":";"},{"start":69,"token_type":"KEYWORD","word":"WITH"},{"start":74,"token_type":"IDENTIFIER","word":"a"},{"start":76,"token_type":"KEYWORD","word":"AS"},{"start":79,"token_type":"OPERATOR","word":"("},{"start":80,"token_type":"KEYWORD","word":"SELECT"},{"start":87,"token_type":"OPERATOR","word":"*"},{"start":89,"token_type":"KEYWORD","word":"FROM"},{"start":94,"token_type":"TABLE_NAME","word":"a"},{"start":95,"token_type":"OPERATOR","word":")"},{"start":97,"token_type":"KEYWORD","word":"SELECT"},{"start":104,"token_type":"OPERATOR","word":"*"},{"start":106,"token_type":"KEYWORD","word":"FROM"},{"start":111,"token_type":"TABLE_NAME","word":"a"},{"start":112,"token_type":"TERMINATOR","word":";"},{"start":114,"token_type":"KEYWORD","word":"WITH"},{"start":119,"token_type":"KEYWORD","word":"RECURSIVE"},{"start":129,"token_type":"IDENTIFIER","word":"r"},{"start":131,"token_type":"KEYWORD","word":"AS"},{"start":134,"token_type":"OPERATOR","word":"("},{"start":135,"token_type":"KEYWORD","word":"SELECT"},{"start":142,"token_type":"NUMBER_LITERAL","word":"1"},{"start":144,"token_type":"KEYWORD","word":"AS"},{"start":147,"token_type":"IDENTIFIER","word":"n"},{"start":149,"token_type":"KEYWORD","word":"UNION"},{"start":155,"token_type":"KEYWORD","word":"ALL"},{"start":159,"token_type":"KEYWORD","word":"SELECT"},{"start":166,"token_type":"COLUMN_NAME","word":"n"},{"start":168,"token_type":"OPERATOR","word":"+"},{"start":170,"token_type":"NUMBER_LITERAL","word":"1"},{"start":172,"token_type":"KEYWORD","word":"FROM"},{"start":177,"token_type":"TABLE_NAME","word":"r"},{"start":179,"token_type":"KEYWORD","word":"WHERE"},{"start":185,"token_type":"COLUMN_NAME","word":"n"},{"start":187,"token_type":"OPERATOR","word":"<"},{"start":189,"token_type":"NUMBER_LITERAL","word":"3"},{"start":190,"token_type":"OPERATOR","word":")"},{"start":192,"token_type":"KEYWORD","word":"SELECT"},{"start":199,"token_type":"OPERATOR","word":"*"},{"start":201,"token_type":"KEYWORD","word":"FROM"},{"start":206,"token_type":"TABLE_NAME","word":"r"},{"start":207,"token_type":"TERMINATOR","word":";"},{"start":209,"token_type":"KEYWORD","word":"WITH"},{"start":214,"token_type":"IDENTIFIER","word":"C"},{"start":216,"token_type":"KEYWORD","word":"AS"},{"start":219,"token_type":"OPERATOR","word":"("},{"start":220,"token_type":"KEYWORD","word":"SELECT"},{"start":227,"token_type":"NUMBER_LITERAL","word":"1"},{"start":228,"token_type":"OPERATOR","word":")"},{"start":230,"token_type":"KEYWORD","word":"SELECT"},{"start":237,"token_type":"OPERATOR","word":"*"},{"start":239,"token_type":"KEYWORD","word":"FROM"},{"start":244,"token_type":"SCHEMA_NAME","word":"main"},{"start":248,"token_type":"NUMBER_LITERAL","word":"."},{"start":249,"token_type":"TABLE_NAME","word":"c"},{"start":250,"token_type":"OPERATOR","word":","},{"start":252,"token_type":"TABLE_NAME","word":"c"},{"start":253,"token_type":"TERMINATOR","word":";"}]},
"trees":{
"(SELECT * FROM c)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["c"]},"query_location":15,"query_location_length":1,"sample":null,"schema_name":"","table_name":"c","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":8,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY (SELECT * FROM a) TO out_csv":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY FROM DATABASE a TO b":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY a TO 'out.csv' (HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY s.t (a) FROM 'in.csv' (HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t FROM 'in.csv'":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t TO 'it''s.csv'":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t TO out_csv":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE MACRO m() AS 1":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE OR REPLACE TEMP TABLE \"R \"\"x\"\"\" AS SELECT * FROM a":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABL r AS SELECT 1":{"error":true,"error_message":"syntax error at or near \"TABL\"","error_subtype":"SYNTAX_ERROR","error_type":"parser","location":"[7,4]","position":"7"},
"CREATE TABLE IF NOT EXISTS db.s.r2 (a INT)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS PIVOT t ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS SELECT * FROM a":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS SELECT * FROM a JOIN b USING (k) WHERE x IN (SELECT x FROM c)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TEMPORARY VIEW w AS FROM c":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE VIEW v (x) AS SELECT * FROM b":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE VIEW v AS SELECT * FROM a":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"DELETE FROM t USING u WHERE t.k = u.k":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"FORCE INSTALL spatial":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"FROM c":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["c"]},"query_location":5,"query_location_length":1,"sample":null,"schema_name":"","table_name":"c","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18446744073709551615,"query_location_length":0,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"INSERT INTO s.t AS x (a, b) SELECT * FROM a ON CONFLICT DO NOTHING RETURNING *":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"INSERT INTO t (SELECT * FROM c)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"INSERT INTO t DEFAULT VALUES":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"INSERT INTO t SELECT * FROM a":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"INSERT OR IGNORE INTO t BY NAME SELECT * FROM b RETURNING (SELECT 1 FROM z)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"INSERT OR REPLACE INTO t BY POSITION VALUES (1)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"INSTALL mlpack FROM community":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"LOAD mlpack":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT 'p.csv' ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT (SELECT * FROM a) ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT read_csv('p.csv') ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT t ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT_WIDER s.t ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PRAGMA threads = 4":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"SELECT 'a;b' AS s":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"s","class":"CONSTANT","literal":{"kind":"STRING","text":"a;b"},"query_location":7,"query_location_length":5,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT (SELECT max(d) FROM events)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":7,"query_location_length":27,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["events"]},"query_location":27,"query_location_length":6,"sample":null,"schema_name":"","table_name":"events","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["d"],"query_location":19,"query_location_length":1,"type":"COLUMN_REF"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"max","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["max"]},"query_location":15,"query_location_length":6,"schema":"","type":"FUNCTION"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"SCALAR","type":"SUBQUERY"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":false,"join_type":"LEFT","left":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["t"]},"query_location":26,"query_location_length":1,"sample":null,"schema_name":"","table_name":"t","type":"BASE_TABLE"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":28,"query_location_length":26,"ranking_expression":null,"ref_type":"ASOF","right":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["u"]},"query_location":43,"query_location_length":1,"sample":null,"schema_name":"","table_name":"u","type":"BASE_TABLE"},"sample":null,"type":"JOIN","using_columns":["k"]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":["a"],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":13,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM a":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM a JOIN b USING (k) WHERE x IN (SELECT x FROM c)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":false,"join_type":"INNER","left":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":16,"query_location_length":16,"ranking_expression":null,"ref_type":"REGULAR","right":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["b"]},"query_location":21,"query_location_length":1,"sample":null,"schema_name":"","table_name":"b","type":"BASE_TABLE"},"sample":null,"type":"JOIN","using_columns":["k"]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":{"alias":"","child":{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":39,"query_location_length":1,"type":"COLUMN_REF"},"class":"SUBQUERY","comparison_type":"COMPARE_EQUAL","query_location":44,"query_location_length":17,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["c"]},"query_location":59,"query_location_length":1,"sample":null,"schema_name":"","table_name":"c","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":52,"query_location_length":1,"type":"COLUMN_REF"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"ANY","type":"SUBQUERY"}}}]},
"SELECT * FROM a WHERE x > 1":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":22,"query_location_length":1,"type":"COLUMN_REF"},"query_location":22,"query_location_length":5,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":26,"query_location_length":1,"type":"VALUE_CONSTANT"},"type":"COMPARE_GREATERTHAN"}}}]},
"SELECT * FROM b":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["b"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"b","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM range(3), read_csv('x.csv'), mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\")":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"3"},"query_location":20,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"range","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["range"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":8,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":18446744073709551615,"query_location_length":0,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"x.csv"},"query_location":33,"query_location_length":7,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_csv","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_csv"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":24,"query_location_length":17,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":9,"query_location_length":89,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["X"],"query_location":70,"query_location_length":3,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["Y"],"query_location":75,"query_location_length":3,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["params"],"query_location":80,"query_location_length":8,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["model"],"query_location":90,"query_location_length":7,"type":"COLUMN_REF"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"mlpack_random_forest_train","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["mlpack_random_forest_train"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":43,"query_location_length":55,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_csv('x.csv', header = true, sep := ',', \"quote\" = 'q'), f(bare, 1 + 2, t.col, 1.5, NULL)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"x.csv"},"query_location":23,"query_location_length":7,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["header"],"query_location":32,"query_location_length":6,"type":"COLUMN_REF"},"query_location":32,"query_location_length":13,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"BOOLEAN","text":"true"},"query_location":41,"query_location_length":4,"type":"VALUE_CONSTANT"},"type":"COMPARE_EQUAL"},"name":""},{"expression":{"alias":"sep","class":"CONSTANT","literal":{"kind":"STRING","text":","},"query_location":54,"query_location_length":3,"type":"VALUE_CONSTANT"},"name":"sep"},{"expression":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["quote"],"query_location":59,"query_location_length":7,"type":"COLUMN_REF"},"query_location":59,"query_location_length":13,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"q"},"query_location":69,"query_location_length":3,"type":"VALUE_CONSTANT"},"type":"COMPARE_EQUAL"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_csv","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_csv"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":59,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":9,"query_location_length":98,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["bare"],"query_location":77,"query_location_length":4,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":83,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"2"},"query_location":87,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"+","is_operator":true,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["+"]},"query_location":85,"query_location_length":0,"schema":"","type":"FUNCTION"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["t","col"],"query_location":90,"query_location_length":5,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"NUMERIC","text":"1.5"},"query_location":97,"query_location_length":3,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"NULL_LITERAL","text":""},"query_location":102,"query_location_length":4,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"f","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["f"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":75,"query_location_length":32,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT 1":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":7,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT 2 /* ; */":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"2"},"query_location":7,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT DATE '2026-01-01'":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","child":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"2026-01-01"},"query_location":18446744073709551615,"query_location_length":0,"type":"VALUE_CONSTANT"},"class":"CAST","query_location":7,"query_location_length":17,"try_cast":false,"type":"OPERATOR_CAST","type_expr":{"alias":"","catalog":"","children":[],"class":"TYPE","qualified_name":{"path":["DATE"]},"query_location":7,"query_location_length":4,"schema":"","type":"TYPE","type_name":"DATE"}}],"type":"SELECT_NODE","where_clause":null}}]},
"SET VARIABLE cutoff = DATE '2026-01-01'":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"SET VARIABLE m = (SELECT max(d) FROM events)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"SET threads TO 4":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"SUMMARIZE t":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","catalog_name":"","qualified_name":{"path":[]},"query":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["t"]},"query_location":18446744073709551615,"query_location_length":0,"sample":null,"schema_name":"","table_name":"t","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18446744073709551615,"query_location_length":0,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null},"query_location":18446744073709551615,"query_location_length":0,"sample":null,"schema_name":"","show_type":"SUMMARY","table_name":"","type":"SHOW_REF"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18446744073709551615,"query_location_length":0,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"UNPIVOT u ON a, b INTO NAME n VALUE v":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"aggregates":[],"alias":"","column_name_alias":[],"groups":[],"include_nulls":false,"pivots":[{"entries":[{"alias":"","star_expr":{"alias":"","class":"COLUMN_REF","column_names":["a"],"query_location":13,"query_location_length":1,"type":"COLUMN_REF"},"values":[]},{"alias":"","star_expr":{"alias":"","class":"COLUMN_REF","column_names":["b"],"query_location":16,"query_location_length":1,"type":"COLUMN_REF"},"values":[]}],"pivot_enum":"","pivot_expressions":[],"unpivot_names":["n"]}],"query_location":18446744073709551615,"query_location_length":0,"sample":null,"source":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["u"]},"query_location":8,"query_location_length":1,"sample":null,"schema_name":"","table_name":"u","type":"BASE_TABLE"},"type":"PIVOT","unpivot_names":["v"]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18446744073709551615,"query_location_length":0,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"VALUES (1)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"valueslist","expected_names":[],"expected_types":[],"query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EXPRESSION_LIST","values":[[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":8,"query_location_length":1,"type":"VALUE_CONSTANT"}]]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18446744073709551615,"query_location_length":0,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"WITH C AS (SELECT 1) SELECT * FROM main.c, c":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[{"key":"C","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":18,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}}]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["main","c"]},"query_location":35,"query_location_length":6,"sample":null,"schema_name":"main","table_name":"c","type":"BASE_TABLE"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":30,"query_location_length":14,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["c"]},"query_location":43,"query_location_length":1,"sample":null,"schema_name":"","table_name":"c","type":"BASE_TABLE"},"sample":null,"type":"JOIN","using_columns":[]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":28,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"WITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT * FROM r":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[{"key":"r","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aliases":[],"cte_map":{"map":[]},"cte_name":"r","key_targets":[],"left":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"n","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":28,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null},"modifiers":[],"right":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["r"]},"query_location":63,"query_location_length":1,"sample":null,"schema_name":"","table_name":"r","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["n"],"query_location":52,"query_location_length":1,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":56,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"+","is_operator":true,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["+"]},"query_location":54,"query_location_length":0,"schema":"","type":"FUNCTION"}],"type":"SELECT_NODE","where_clause":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["n"],"query_location":71,"query_location_length":1,"type":"COLUMN_REF"},"query_location":71,"query_location_length":5,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"3"},"query_location":75,"query_location_length":1,"type":"VALUE_CONSTANT"},"type":"COMPARE_LESSTHAN"}},"type":"RECURSIVE_CTE_NODE","union_all":true}}}]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["r"]},"query_location":92,"query_location_length":1,"sample":null,"schema_name":"","table_name":"r","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":85,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"WITH a AS (SELECT * FROM a) SELECT * FROM a":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[{"key":"a","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":25,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}}]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":42,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":35,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"WITH c AS (SELECT * FROM a) SELECT * FROM c":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[{"key":"c","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":25,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}}]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["c"]},"query_location":42,"query_location_length":1,"sample":null,"schema_name":"","table_name":"c","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":35,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"WITH x AS (SELECT * FROM y), y AS (SELECT * FROM x) SELECT * FROM y":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[{"key":"x","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["y"]},"query_location":25,"query_location_length":1,"sample":null,"schema_name":"","table_name":"y","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}},{"key":"y","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["x"]},"query_location":49,"query_location_length":1,"sample":null,"schema_name":"","table_name":"x","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":42,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}}]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["y"]},"query_location":66,"query_location_length":1,"sample":null,"schema_name":"","table_name":"y","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":59,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]}}}"##;

    pub(super) struct Recorded {
        version: String,
        tokens: BTreeMap<String, Vec<Token>>,
        trees: BTreeMap<String, Value>,
    }

    impl Recorded {
        pub(super) fn load() -> Recorded {
            let all: Value = serde_json::from_str(RECORDED).expect("RECORDED is JSON");
            Recorded {
                version: all["version"].as_str().unwrap_or_default().to_string(),
                tokens: serde_json::from_value(all["tokens"].clone()).unwrap_or_default(),
                trees: serde_json::from_value(all["trees"].clone()).unwrap_or_default(),
            }
        }
    }

    impl Ask for Recorded {
        fn program(&self) -> String {
            "recorded".to_string()
        }

        fn tokens(&mut self, sql: &str) -> Result<Tokenized, Refusal> {
            let tokens = self.tokens.get(sql).unwrap_or_else(|| {
                panic!("no recorded tokens for {sql:?}; record them with the preview")
            });
            Ok(Tokenized {
                version: self.version.clone(),
                tokens: Ok(tokens.clone()),
            })
        }

        fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal> {
            let tree = |text: &String| {
                self.trees.get(text).cloned().unwrap_or_else(|| {
                    panic!("no recorded tree for {text:?}; record it with the preview")
                })
            };
            Ok(texts.iter().map(tree).collect())
        }
    }

    /// The preview `ARC_DUCKDB_PREVIEW_BIN` names. It prints the version it answered
    /// with, so a log says which build a test ran on.
    struct Preview(Process<'static>);

    fn preview() -> Preview {
        let bin = std::env::var_os(PREVIEW_ENV).unwrap_or_else(|| {
            panic!(
                "{PREVIEW_ENV} must name a DuckDB 2.0 build: this test is #[ignore]d so an \
                 ordinary run skips it, and running it with --include-ignored says one is \
                 staged"
            )
        });
        Preview(Process {
            program: Box::leak(bin.into_boxed_os_str()),
        })
    }

    impl Ask for Preview {
        fn program(&self) -> String {
            self.0.program()
        }

        fn tokens(&mut self, sql: &str) -> Result<Tokenized, Refusal> {
            let tokenized = self.0.tokens(sql)?;
            eprintln!("asked DuckDB {} at {}", tokenized.version, self.program());
            Ok(tokenized)
        }

        fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal> {
            self.0.trees(texts)
        }
    }

    /// Keeps what another `Ask` answered, to be written into `RECORDED`.
    struct Recording<A> {
        inner: A,
        version: String,
        tokens: BTreeMap<String, Vec<Token>>,
        trees: BTreeMap<String, Value>,
    }

    impl<A: Ask> Ask for Recording<A> {
        fn program(&self) -> String {
            self.inner.program()
        }

        fn tokens(&mut self, sql: &str) -> Result<Tokenized, Refusal> {
            let tokenized = self.inner.tokens(sql)?;
            self.version = tokenized.version.clone();
            if let Ok(tokens) = &tokenized.tokens {
                self.tokens.insert(sql.to_string(), tokens.clone());
            }
            Ok(tokenized)
        }

        fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal> {
            let trees = self.inner.trees(texts)?;
            for (text, tree) in texts.iter().zip(&trees) {
                self.trees.insert(text.clone(), tree.clone());
            }
            Ok(trees)
        }
    }

    impl<A> Recording<A> {
        /// `RECORDED`'s text: one line per step and one per tree, so a new capture
        /// diffs by the entry.
        fn fixture(&self) -> String {
            let entries = |map: Vec<(&String, String)>| {
                map.into_iter()
                    .map(|(key, value)| format!("{}:{value}", Value::from(key.as_str())))
                    .collect::<Vec<_>>()
                    .join(",\n")
            };
            let tokens = self
                .tokens
                .iter()
                .map(|(k, v)| (k, serde_json::to_string(v).unwrap()));
            let trees = self.trees.iter().map(|(k, v)| (k, v.to_string()));
            format!(
                "{{\"version\":{},\n\"tokens\":{{\n{}}},\n\"trees\":{{\n{}}}}}",
                Value::from(self.version.as_str()),
                entries(tokens.collect()),
                entries(trees.collect()),
            )
        }
    }

    fn read(ask: &mut dyn Ask, sql: &str) -> Vec<Statement> {
        read_step_with(ask, sql)
            .unwrap_or_else(|e| panic!("reading {sql:?} was refused: {e}"))
            .statements
    }

    fn names(relations: &[Relation]) -> Vec<String> {
        relations.iter().map(ToString::to_string).collect()
    }

    /// A statement's form, reads and produces, for one assertion to compare.
    fn shape(statement: &Statement) -> (Form, Vec<String>, Vec<String>) {
        (
            statement.form.clone(),
            names(&statement.reads),
            names(&statement.produces),
        )
    }

    fn expect(form: Form, reads: &[&str], produces: &[&str]) -> (Form, Vec<String>, Vec<String>) {
        let owned = |list: &[&str]| list.iter().map(ToString::to_string).collect();
        (form, owned(reads), owned(produces))
    }

    fn arg(name: Option<&str>, value: ArgValue) -> Arg {
        Arg {
            name: name.map(str::to_string),
            value,
        }
    }

    fn create_table_as_reads_its_query_and_produces_its_table(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "CREATE TABLE r AS SELECT * FROM a JOIN b USING (k) WHERE x IN (SELECT x FROM c);",
        );
        assert_eq!(s.len(), 1, "one statement: {s:#?}");
        assert_eq!(
            shape(&s[0]),
            expect(Form::CreateTable, &["a", "b", "c"], &["r"]),
            "CREATE TABLE … AS reads each table of its query, the subquery's too, and \
             produces its table"
        );
    }

    fn a_with_name_is_not_read(ask: &mut dyn Ask) {
        let s = read(ask, "WITH c AS (SELECT * FROM a) SELECT * FROM c;");
        assert_eq!(
            shape(&s[0]),
            expect(Form::Query, &["a"], &[]),
            "the query reads the table its WITH reads, and not the WITH's own name"
        );
    }

    fn a_with_name_is_in_scope_where_duckdb_binds_it(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "WITH x AS (SELECT * FROM y), y AS (SELECT * FROM x) SELECT * FROM y;\n\
             WITH a AS (SELECT * FROM a) SELECT * FROM a;\n\
             WITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r WHERE n < 3) \
             SELECT * FROM r;\n\
             WITH C AS (SELECT 1) SELECT * FROM main.c, c;",
        );
        let reads: Vec<Vec<String>> = s.iter().map(|s| names(&s.reads)).collect();
        assert_eq!(
            reads,
            [vec!["y"], vec!["a"], vec![], vec!["main.c"]],
            "a WITH body names the table, not the WITH query, when the name is its own or \
             declared after it; a recursive one names itself; a name with a schema is a \
             table and a name is matched in any case"
        );
    }

    fn asof_summarize_and_pivot_are_read(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k);\n\
             SUMMARIZE t;\n\
             PIVOT t ON c USING sum(v);",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::Query, &["t", "u"], &[]),
                expect(Form::Query, &["t"], &[]),
                expect(Form::Pivot, &["t"], &[]),
            ],
            "an ASOF join reads both sides; SUMMARIZE and PIVOT read their table"
        );
    }

    fn insert_view_and_copy_produce_what_they_write(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "INSERT INTO t SELECT * FROM a;\n\
             CREATE VIEW v AS SELECT * FROM a;\n\
             COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER);\n\
             COPY a TO 'out.csv' (HEADER);\n\
             COPY t FROM 'in.csv';",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::Insert, &["a"], &["t"]),
                expect(Form::CreateView, &["a"], &["v"]),
                expect(Form::Copy, &["a"], &["file 'out.csv'"]),
                expect(Form::Copy, &["a"], &["file 'out.csv'"]),
                expect(Form::Copy, &["file 'in.csv'"], &["t"]),
            ],
            "INSERT and CREATE VIEW produce their table; COPY … TO produces its file and \
             COPY … FROM reads it"
        );
    }

    fn setup_statements_read_and_produce_nothing(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "INSTALL mlpack FROM community; LOAD mlpack; \
             SET VARIABLE cutoff = DATE '2026-01-01'; PRAGMA threads = 4; \
             CREATE TABLE r AS SELECT * FROM a;",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::NoData, &[], &[]),
                expect(Form::NoData, &[], &[]),
                expect(Form::SetVariable, &[], &[]),
                expect(Form::NoData, &[], &[]),
                expect(Form::CreateTable, &["a"], &["r"]),
            ],
            "INSTALL, LOAD, SET VARIABLE to a constant and PRAGMA read and produce nothing"
        );
    }

    fn table_functions_are_calls_with_their_arguments(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "SELECT * FROM range(3), read_csv('x.csv'), \
             mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\");",
        );
        assert_eq!(s[0].reads, [], "a table function is not a table read");
        let quoted = |name: &str| arg(None, ArgValue::QuotedIdentifier(name.to_string()));
        assert_eq!(
            s[0].calls,
            [
                TableCall {
                    function: "range".to_string(),
                    args: vec![arg(None, ArgValue::Number("3".to_string()))],
                },
                TableCall {
                    function: "read_csv".to_string(),
                    args: vec![arg(None, ArgValue::String("x.csv".to_string()))],
                },
                TableCall {
                    function: "mlpack_random_forest_train".to_string(),
                    args: vec![quoted("X"), quoted("Y"), quoted("params"), quoted("model")],
                },
            ],
            "each call holds its function's name and its arguments as written"
        );
    }

    fn named_and_other_arguments_are_told_apart(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "SELECT * FROM read_csv('x.csv', header = true, sep := ',', \"quote\" = 'q'), \
             f(bare, 1 + 2, t.col, 1.5, NULL);",
        );
        let expression = |text: &str| ArgValue::Expression {
            text: text.to_string(),
            quotes: false,
        };
        assert_eq!(
            s[0].calls[0].args,
            [
                arg(None, ArgValue::String("x.csv".to_string())),
                arg(Some("header"), expression("true")),
                arg(Some("sep"), ArgValue::String(",".to_string())),
                arg(Some("quote"), ArgValue::String("q".to_string())),
            ],
            "`name = value` and `name := value` pass a named parameter"
        );
        assert_eq!(
            s[0].calls[1].args,
            [
                arg(None, ArgValue::Identifier("bare".to_string())),
                arg(None, expression("1 + 2")),
                arg(None, expression("t.col")),
                arg(None, ArgValue::Number("1.5".to_string())),
                arg(None, expression("NULL")),
            ],
            "a bare name, an expression, a qualified name, a decimal and NULL"
        );
    }

    fn a_semicolon_in_a_string_or_comment_does_not_split(ask: &mut dyn Ask) {
        let s = read(ask, "SELECT 'a;b' AS s; -- c; d\nSELECT 2 /* ; */;");
        let texts: Vec<&str> = s.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            ["SELECT 'a;b' AS s", "SELECT 2 /* ; */"],
            "two statements, split at the `;` outside the string and the comments"
        );
    }

    fn comments_alone_are_no_statement_and_a_last_semicolon_is_optional(ask: &mut dyn Ask) {
        let s = read(ask, "-- nothing but a comment\n");
        assert_eq!(s, [], "a step of comments holds no statement");
        let s = read(ask, "SELECT * FROM a; /* trailing */\nSELECT * FROM b");
        let reads: Vec<Vec<String>> = s.iter().map(|s| names(&s.reads)).collect();
        assert_eq!(
            reads,
            [vec!["a"], vec!["b"]],
            "a comment after the last `;` is no statement, and text after it with no `;` is one"
        );
    }

    fn a_statement_duckdb_cannot_parse_refuses_the_step(ask: &mut dyn Ask) {
        let refusal = read_step_with(ask, "SELECT 1;\nCREATE TABL r AS SELECT 1;")
            .expect_err("a statement DuckDB cannot parse refuses the step");
        assert_eq!(
            refusal,
            Refusal::Syntax {
                statement: 2,
                text: "CREATE TABL r AS SELECT 1".to_string(),
                message: "syntax error at or near \"TABL\"".to_string(),
            },
            "the refusal names the statement and holds DuckDB's own message"
        );
        assert!(
            refusal
                .to_string()
                .contains("syntax error at or near \"TABL\""),
            "the message a person reads holds DuckDB's: {refusal}"
        );
    }

    fn inserts_are_framed_around_their_query(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "INSERT INTO s.t AS x (a, b) SELECT * FROM a ON CONFLICT DO NOTHING RETURNING *;\n\
             INSERT OR IGNORE INTO t BY NAME SELECT * FROM b RETURNING (SELECT 1 FROM z);\n\
             INSERT INTO t (SELECT * FROM c);\n\
             INSERT INTO t DEFAULT VALUES;\n\
             INSERT OR REPLACE INTO t BY POSITION VALUES (1);",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::Insert, &["a"], &["s.t"]),
                expect(Form::Insert, &["b"], &["t"]),
                expect(Form::Insert, &["c"], &["t"]),
                expect(Form::Insert, &[], &["t"]),
                expect(Form::Insert, &[], &["t"]),
            ],
            "an INSERT reads its query, not its alias, columns, ON CONFLICT or RETURNING"
        );
    }

    fn creates_are_framed_around_their_query(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "CREATE OR REPLACE TEMP TABLE \"R \"\"x\"\"\" AS SELECT * FROM a;\n\
             CREATE TABLE IF NOT EXISTS db.s.r2 (a INT);\n\
             CREATE VIEW v (x) AS SELECT * FROM b;\n\
             CREATE TEMPORARY VIEW w AS FROM c;\n\
             CREATE MACRO m() AS 1;",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::CreateTable, &["a"], &["R \"x\""]),
                expect(Form::CreateTable, &[], &["db.s.r2"]),
                expect(Form::CreateView, &["b"], &["v"]),
                expect(Form::CreateView, &["c"], &["w"]),
                expect(Form::Unread("CREATE".to_string()), &[], &[]),
            ],
            "CREATE TABLE and CREATE VIEW produce their name and read their query; a CREATE \
             of anything else is unread"
        );
    }

    fn copies_are_framed_from_their_words(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "COPY s.t (a) FROM 'in.csv' (HEADER);\n\
             COPY t TO 'it''s.csv';\n\
             COPY FROM DATABASE a TO b;\n\
             COPY (SELECT * FROM a) TO out_csv;\n\
             COPY t TO out_csv;",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::Copy, &["file 'in.csv'"], &["s.t"]),
                expect(Form::Copy, &["t"], &["file 'it's.csv'"]),
                expect(Form::Unread("COPY".to_string()), &[], &[]),
                expect(Form::Unread("COPY".to_string()), &[], &[]),
                expect(Form::Unread("COPY".to_string()), &[], &[]),
            ],
            "COPY reads or writes the file its string names; COPY FROM DATABASE, and a COPY \
             to a bare name, are unread"
        );
    }

    fn pivots_read_their_source(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "PIVOT (SELECT * FROM a) ON c USING sum(v);\n\
             PIVOT 'p.csv' ON c USING sum(v);\n\
             PIVOT_WIDER s.t ON c USING sum(v);\n\
             UNPIVOT u ON a, b INTO NAME n VALUE v;\n\
             PIVOT read_csv('p.csv') ON c USING sum(v);",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::Pivot, &["a"], &[]),
                expect(Form::Pivot, &["file 'p.csv'"], &[]),
                expect(Form::Pivot, &["s.t"], &[]),
                expect(Form::Query, &["u"], &[]),
                expect(Form::Unread("PIVOT".to_string()), &[], &[]),
            ],
            "a PIVOT reads its query, file or table; DuckDB gives UNPIVOT a tree; a PIVOT of \
             a function call is unread"
        );
    }

    fn what_the_reader_cannot_frame_is_unread(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "SET VARIABLE m = (SELECT max(d) FROM events);\n\
             SET threads TO 4;\n\
             FORCE INSTALL spatial;\n\
             DELETE FROM t USING u WHERE t.k = u.k;\n\
             CREATE TABLE r AS PIVOT t ON c USING sum(v);",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        assert_eq!(
            shapes,
            [
                expect(Form::SetVariable, &["events"], &[]),
                expect(Form::NoData, &[], &[]),
                expect(Form::NoData, &[], &[]),
                expect(Form::Unread("DELETE".to_string()), &[], &[]),
                expect(Form::Unread("CREATE".to_string()), &[], &["r"]),
            ],
            "SET VARIABLE reads what its value reads; a form the reader does not frame, or \
             a query DuckDB gives no tree for, is unread and keeps what its words made certain"
        );
    }

    fn a_pivot_whose_source_is_joined_is_unread(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "PIVOT t JOIN u USING (k) ON c USING sum(v);\n\
             PIVOT (FROM t) JOIN u USING (k) ON c USING sum(v);\n\
             PIVOT t AS x JOIN u USING (k) ON c USING sum(v);\n\
             PIVOT 'p.csv' JOIN u USING (k) ON c USING sum(v);\n\
             PIVOT t ON c USING sum(v);\n\
             PIVOT t AS x ON c USING sum(v);",
        );
        let shapes: Vec<_> = s.iter().map(shape).collect();
        let unread = || expect(Form::Unread("PIVOT".to_string()), &[], &[]);
        assert_eq!(
            shapes,
            [
                unread(),
                unread(),
                unread(),
                unread(),
                expect(Form::Pivot, &["t"], &[]),
                expect(Form::Pivot, &["t"], &[]),
            ],
            "a PIVOT whose source is joined to another relation is unread, not read as the \
             first relation alone; a PIVOT of one table, aliased or not, reads it"
        );
    }

    fn copy_to_options_are_read(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "COPY t TO 'd' (FORMAT parquet, PARTITION_BY (a, b), PER_THREAD_OUTPUT false, \
             FILE_SIZE_BYTES '1MB', HEADER);\n\
             COPY (SELECT * FROM a) TO 'e' WITH (PARTITION_BY (), PER_THREAD_OUTPUT 0);\n\
             COPY t FROM 'in.csv' (HEADER);",
        );
        let option = |name: &str, value: Option<ArgValue>| CopyOption {
            name: name.to_string(),
            value,
        };
        let word = |w: &str| Some(ArgValue::Identifier(w.to_string()));
        assert_eq!(
            s[0].options,
            [
                option("FORMAT", word("parquet")),
                option(
                    "PARTITION_BY",
                    Some(ArgValue::List(vec![
                        ArgValue::Identifier("a".to_string()),
                        ArgValue::Identifier("b".to_string()),
                    ])),
                ),
                option("PER_THREAD_OUTPUT", word("false")),
                option("FILE_SIZE_BYTES", Some(ArgValue::String("1MB".to_string()))),
                option("HEADER", None),
            ],
            "each option of a COPY … TO, with its value as written"
        );
        assert_eq!(
            s[1].options,
            [
                option("PARTITION_BY", Some(ArgValue::List(Vec::new()))),
                option("PER_THREAD_OUTPUT", Some(ArgValue::Number("0".to_string()))),
            ],
            "WITH before the options, and an empty list"
        );
        assert_eq!(s[2].options, [], "a COPY … FROM keeps no options");
    }

    fn lists_and_quoted_expressions_are_told_apart(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "SELECT * FROM read_json(['a.json', 'b.json']), f(lower('X')), g(\"Q\" + 1);",
        );
        let args: Vec<_> = s[0].calls.iter().map(|c| c.args.clone()).collect();
        let expression = |text: &str, quotes: bool| ArgValue::Expression {
            text: text.to_string(),
            quotes,
        };
        assert_eq!(
            args,
            [
                vec![arg(
                    None,
                    ArgValue::List(vec![
                        ArgValue::String("a.json".to_string()),
                        ArgValue::String("b.json".to_string()),
                    ]),
                )],
                vec![arg(None, expression("lower('X')", true))],
                vec![arg(None, expression("\"Q\" + 1", true))],
            ],
            "a list holds its values; an expression says whether it holds a string or a \
             quoted name"
        );
    }

    fn the_steps_introspect_reads_are_read(ask: &mut dyn Ask) {
        for (sql, first_word, reads, produces) in recorded_steps::ALL {
            let s = read(ask, sql);
            assert_eq!(s.len(), 1, "one statement in {sql:?}");
            let form = match first_word {
                "CREATE" => Form::CreateTable,
                "COPY" => Form::Copy,
                _ => Form::Unread(first_word.to_string()),
            };
            assert_eq!(shape(&s[0]), expect(form, reads, produces), "{sql}");
        }
    }

    /// Each check runs twice: from the preview's recorded answers in every `cargo test`,
    /// and against the preview itself when it is staged.
    macro_rules! checks {
        ($($check:ident),* $(,)?) => {
            const CHECKS: &[(&str, fn(&mut dyn Ask))] = &[$((stringify!($check), $check)),*];

            mod recorded {
                $(
                    #[test]
                    fn $check() {
                        super::$check(&mut super::Recorded::load());
                    }
                )*
            }

            mod on_the_preview {
                $(
                    #[test]
                    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_PREVIEW_BIN names; \
                                ci.yml's build job downloads the preview and passes \
                                --include-ignored"]
                    fn $check() {
                        super::$check(&mut super::preview());
                    }
                )*
            }
        };
    }

    checks!(
        create_table_as_reads_its_query_and_produces_its_table,
        a_with_name_is_not_read,
        a_with_name_is_in_scope_where_duckdb_binds_it,
        asof_summarize_and_pivot_are_read,
        insert_view_and_copy_produce_what_they_write,
        setup_statements_read_and_produce_nothing,
        table_functions_are_calls_with_their_arguments,
        named_and_other_arguments_are_told_apart,
        a_semicolon_in_a_string_or_comment_does_not_split,
        comments_alone_are_no_statement_and_a_last_semicolon_is_optional,
        a_statement_duckdb_cannot_parse_refuses_the_step,
        inserts_are_framed_around_their_query,
        creates_are_framed_around_their_query,
        copies_are_framed_from_their_words,
        pivots_read_their_source,
        what_the_reader_cannot_frame_is_unread,
        a_pivot_whose_source_is_joined_is_unread,
        copy_to_options_are_read,
        lists_and_quoted_expressions_are_told_apart,
        the_steps_introspect_reads_are_read,
    );

    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_PREVIEW_BIN names; ci.yml's build \
                job downloads the preview and passes --include-ignored"]
    fn the_recorded_answers_are_what_the_preview_answers() {
        let mut recording = Recording {
            inner: preview(),
            version: String::new(),
            tokens: BTreeMap::new(),
            trees: BTreeMap::new(),
        };
        for (name, check) in CHECKS {
            eprintln!("recording {name}");
            check(&mut recording);
        }
        let recorded = Recorded::load();
        if recorded.tokens != recording.tokens || recorded.trees != recording.trees {
            eprintln!("--- RECORDED ---\n{}\n--- END ---", recording.fixture());
            panic!(
                "the preview ({}) answers differently from RECORDED ({}); the fresh answers \
                 are printed above",
                recording.version, recorded.version
            );
        }
    }

    /// A DuckDB before 2.0 has no `sql_tokenize`. The `duckdb` on `PATH` is the one
    /// `ci.yml` pins, a 1.x release.
    #[test]
    fn a_duckdb_older_than_2_is_refused_naming_its_version() {
        let refusal =
            read_step(OsStr::new("duckdb"), "SELECT 1;").expect_err("a 1.x DuckDB is refused");
        let Refusal::NotDuckDb2 { version, .. } = &refusal else {
            panic!("expected the version refusal, got {refusal:?}");
        };
        assert!(
            version.starts_with("v1."),
            "the version found is named: {refusal}"
        );
        let message = refusal.to_string();
        assert!(
            message.contains(version.as_str()) && message.contains("needs a DuckDB 2.0 build"),
            "the message names the version found and says a 2.0 build is needed: {message}"
        );
    }

    #[test]
    fn only_a_2_x_version_is_accepted() {
        for (version, accepted) in [
            ("v1.5.5", false),
            ("v2.0.0-alpha43569", true),
            ("2.1.0", true),
            ("v3.0.0", false),
            ("unknown", false),
        ] {
            let mut ask = Recorded::load();
            ask.version = version.to_string();
            let result = read_step_with(&mut ask, "WITH c AS (SELECT * FROM a) SELECT * FROM c;");
            assert_eq!(
                result.is_ok(),
                accepted,
                "version {version:?} accepted: {result:?}"
            );
        }
    }

    /// The reader hands DuckDB the step inside a string literal. The `duckdb` on `PATH`
    /// reads it back: a quote, a `;`, a backslash, a comment and a line that would be a
    /// command to the DuckDB shell come back as the text sent.
    #[test]
    fn duckdb_reads_the_step_back_as_the_text_sent() {
        let text = "it's; a \\ b\n-- not a comment\n.quit\n''";
        let duckdb = Process {
            program: OsStr::new("duckdb"),
        };
        let (answers, stderr) = duckdb
            .run(&format!("SELECT {} AS s;\n", literal(text)))
            .expect("the duckdb on PATH runs");
        assert_eq!(
            answers.first().map(|rows| &rows[0]["s"]),
            Some(&Value::from(text)),
            "DuckDB reads the literal back as the text sent; stderr: {stderr}"
        );
    }

    #[test]
    fn a_duckdb_that_cannot_be_run_is_named() {
        let refusal =
            read_step(OsStr::new("/nonexistent/duckdb"), "SELECT 1;").expect_err("no such program");
        assert!(
            matches!(&refusal, Refusal::NotRun { program, reason }
                if program == "/nonexistent/duckdb" && reason.contains("No such file")),
            "the refusal names the program and why it did not run: {refusal:?}"
        );
    }

    /// A program that runs and prints no version is not a DuckDB.
    #[test]
    fn a_program_that_gives_no_version_is_refused() {
        let refusal = read_step(OsStr::new("true"), "SELECT 1;").expect_err("not a DuckDB");
        assert!(
            matches!(&refusal, Refusal::NotRun { reason, .. } if reason.contains("gave no version")),
            "the refusal says no version came back: {refusal:?}"
        );
    }

    /// A node DuckDB gives no name is neither a read nor a call.
    #[test]
    fn a_node_without_a_name_is_no_read_and_no_call() {
        let tree = serde_json::json!([
            {"type": "BASE_TABLE", "table_name": ""},
            {"type": "TABLE_FUNCTION", "function": {"arguments": []}},
        ]);
        let found = walk(&tree, "");
        assert!(
            found.reads.is_empty() && found.calls.is_empty(),
            "a nameless table or function is not read: {:?} {:?}",
            found.reads,
            found.calls
        );
    }

    /// Answers made by hand, for what the preview does not answer when asked well.
    struct Scripted {
        tokens: Result<Vec<Token>, String>,
        trees: Result<Vec<Value>, Refusal>,
    }

    impl Ask for Scripted {
        fn program(&self) -> String {
            "scripted".to_string()
        }

        fn tokens(&mut self, _: &str) -> Result<Tokenized, Refusal> {
            Ok(Tokenized {
                version: "v2.0.0".to_string(),
                tokens: self.tokens.clone(),
            })
        }

        fn trees(&mut self, _: &[String]) -> Result<Vec<Value>, Refusal> {
            self.trees.clone()
        }
    }

    fn token(start: usize, word: &str) -> Token {
        Token {
            start,
            token_type: "IDENTIFIER".to_string(),
            word: word.to_string(),
        }
    }

    #[test]
    fn an_answer_the_reader_cannot_read_refuses_the_step() {
        let tree = serde_json::json!({"error": false, "statements": []});
        let other =
            serde_json::json!({"error": true, "error_type": "binder", "error_message": "m"});
        let failed = Refusal::Unreadable("no trees: m".to_string());
        let x = || Ok(vec![token(0, "x")]);
        let cases = [
            (
                Err("Catalog Error".to_string()),
                Ok(vec![]),
                "Catalog Error",
            ),
            (Ok(vec![token(9, "x")]), Ok(vec![]), "byte 9"),
            (
                Ok(vec![token(9, "x"), token(10, ";")]),
                Ok(vec![]),
                "byte 9",
            ),
            (x(), Ok(vec![tree.clone(), tree]), "2 answers for 1 texts"),
            (x(), Ok(vec![other]), "binder: m"),
            (x(), Err(failed), "no trees: m"),
        ];
        for (tokens, trees, reason) in cases {
            let refusal = read_step_with(&mut Scripted { tokens, trees }, "x")
                .expect_err("an unreadable answer refuses");
            assert!(
                matches!(&refusal, Refusal::Unreadable(r) if r.contains(reason)),
                "expected a refusal holding {reason:?}, got {refusal:?}"
            );
        }
    }

    /// The reader takes nothing from the parser arc reads other steps with.
    #[test]
    fn the_reader_uses_nothing_of_the_other_parser() {
        let source = include_str!("duckdb_lineage.rs");
        assert!(
            !source.contains(concat!("sql", "parser")),
            "src/duckdb_lineage.rs names the other parser"
        );
    }
}
