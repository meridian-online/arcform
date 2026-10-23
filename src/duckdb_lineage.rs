//! What each statement of a DuckDB SQL step reads and produces, taken from DuckDB's own
//! parse of the step.
//!
//! DuckDB 2.0 answers two questions about SQL without running it. `sql_tokenize` splits
//! the text into tokens, each with the byte it starts at, and `json_serialize_sql` gives
//! the tree of a `SELECT`. For a statement that is not a `SELECT` it answers `not
//! implemented`, which also says the statement parsed; for text it cannot parse it
//! answers `parser`, with its own message. DuckDB 1.x has no `sql_tokenize`, so the
//! reader reads the tokens of a 2.x build and refuses any other.
//!
//! The reader asks twice, through the DuckDB command line, as `arc run` runs a step.
//! First for the step's tokens, which come from [`crate::duckdb_tokens`]: the file cut into
//! the batches the command line hands its parser, the tokens of each, and the version, in
//! the one answer the refusals read too. Each batch is split into statements at each `;`
//! token: a `;` inside a string or a comment is part of that token, not one of its own.
//! Then for the tree of each statement, and of each query a statement holds: the query of
//! a `CREATE TABLE … AS`, of an `INSERT`, of a `COPY (…) TO`. Both questions reach DuckDB
//! written in hex digits, so a `\r` before a `\n` is not dropped on the way and a token's
//! start, or a node's place in a tree, is a byte of the text arc holds.
//!
//! Reads come from a tree: each table it names, less the names a `WITH` in scope
//! declares, and each table function it calls, with the arguments as written. What a
//! statement that is not a query creates, inserts into or copies is taken from its
//! words. A keyword is matched by its text and never by the part the tokenizer says the
//! token plays, because on DuckDB 2.0 that part changes with what came before it in
//! the same input: `INSERT` has come back as a column name, and a table after `FROM` as
//! a plain identifier. A token's text and start do not change, and they are all the
//! reader takes from a token.
//!
//! A table function is returned as a call, its name and its arguments, and a `COPY … TO`
//! with its options. Which function reads a file, which reads no table, and which options
//! write a directory, are decided by whoever reads the statement: `crate::introspect`,
//! which applies the same rules to what arc's other reader returns.

use std::cmp::Ordering;
use std::fmt;
use std::ops::Range;

use crate::duckdb_tokens::{BatchTokens, FileTokens, Process, Token, hex_text, shared_tokens};
use crate::engine::DuckDbProgram;

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
    /// A parenthesised query, `(SELECT src, dst FROM edges)`: what it reads is among the
    /// statement's reads, and its strings are its own.
    Query,
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
    /// A statement DuckDB cannot parse, with DuckDB's message. It fails when the step runs;
    /// what its words made certain is kept, as for [`Form::Unread`].
    NotParsed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Statement {
    /// The statement as written, without its `;` or the comments before it.
    pub(crate) text: String,
    /// The line of the file it starts on, counting from 1.
    pub(crate) line: usize,
    /// The statement's `[start, end)` bytes in the file, `;` included when it has one, as
    /// the command line's batches and DuckDB's tokens place them. `None` when they cannot be
    /// placed in the file.
    pub(crate) byte_range: Option<(usize, usize)>,
    pub(crate) form: Form,
    pub(crate) reads: Vec<Relation>,
    pub(crate) produces: Vec<Relation>,
    pub(crate) calls: Vec<TableCall>,
    /// The names its `WITH` clauses declare, lowercased, in the order the tree gives them.
    pub(crate) with: Vec<String>,
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
    /// The DuckDB named could not be started: it is not there, or not a program.
    NotStarted { program: String, reason: String },
    /// The DuckDB named was started and exited with an error.
    NotRun { program: String, reason: String },
    /// The DuckDB asked answered with tokens from a version that is not 2.x, whose tokens
    /// arc reads.
    NotDuckDb2 { version: String },
    /// DuckDB answered something the reader cannot read, such as an answer with no version.
    Unreadable(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotStarted { program, reason } | Refusal::NotRun { program, reason } => {
                write!(
                    f,
                    "could not ask DuckDB `{program}` for its parse: {reason}"
                )
            }
            Refusal::NotDuckDb2 { version } => write!(
                f,
                "the tokens came from DuckDB {version}, and arc reads the tokens of a DuckDB 2.x"
            ),
            Refusal::Unreadable(reason) => write!(f, "DuckDB's parse could not be read: {reason}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Reads a step with `duckdb`, the program the steps run on.
pub(crate) fn read_step(duckdb: &DuckDbProgram, sql: &str) -> Result<StepReading, Refusal> {
    read_step_with(
        &mut Process {
            program: duckdb.clone(),
        },
        sql,
    )
}

/// Whether `duckdb` gives the tree of a query: the reader's second question. Its first, for
/// a file's tokens, is the one the refusals ask of the same program before a run, which
/// refuses a DuckDB that is not a 2.x build.
pub(crate) fn gives_trees(duckdb: &DuckDbProgram) -> Result<(), Refusal> {
    gives_trees_with(&mut Process {
        program: duckdb.clone(),
    })
}

fn gives_trees_with(ask: &mut dyn Ask) -> Result<(), Refusal> {
    let trees = ask.trees(&["SELECT 1".to_string()])?;
    match trees.first().map(answer) {
        Some(Answer::Tree(_)) => Ok(()),
        Some(Answer::Parser(reason) | Answer::Other(reason)) => Err(Refusal::Unreadable(format!(
            "no tree for `SELECT 1`: {reason}"
        ))),
        Some(Answer::NotImplemented) | None => {
            Err(Refusal::Unreadable("no tree for `SELECT 1`".to_string()))
        }
    }
}

/// The two questions the reader asks. A DuckDB process answers them, and the tests
/// answer them from what DuckDB v2.0.0 answered.
trait Ask {
    /// The tokens of the step's file, batch by batch, as [`crate::duckdb_tokens`] reads them.
    fn tokens(&mut self, file: &[u8]) -> Result<FileTokens, Refusal>;
    /// DuckDB's answer from `json_serialize_sql` for each text, in order.
    fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal>;
}

impl Ask for Process {
    fn tokens(&mut self, file: &[u8]) -> Result<FileTokens, Refusal> {
        shared_tokens(&self.program, file)
    }

    fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<String> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| format!("({i}, {})", hex_text(text)))
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

fn read_step_with(ask: &mut dyn Ask, sql: &str) -> Result<StepReading, Refusal> {
    // The tokens are refused there unless they came from a DuckDB 2.x: the one rule on the
    // version, for the refusals and the reader both.
    let file = ask.tokens(sql.as_bytes())?;
    let mut pieces = Vec::new();
    for batch in &file.batches {
        pieces.extend(split(batch)?);
    }
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
    for ((piece, frame), tree) in pieces.iter().zip(frames).zip(own) {
        let query = frame.query.clone().map(|text| (text, held.next()));
        statements.push(read_statement(piece, frame, tree, query)?);
    }
    // Defensive: a range ends on the `;` or on trimmed text, so it ends on a character
    // boundary. If a mapping ever ends one inside a character, the range keeps it whole.
    for statement in &mut statements {
        statement.byte_range = statement.byte_range.and_then(|(start, mut end)| {
            while end <= sql.len() && !sql.is_char_boundary(end) {
                end += 1;
            }
            (sql.is_char_boundary(start) && end <= sql.len()).then_some((start, end))
        });
    }
    Ok(StepReading {
        duckdb_version: file.version,
        statements,
    })
}

/// The major version in `v2.0.0-alpha43569`.
pub(crate) fn major(version: &str) -> Option<u64> {
    let version = version.strip_prefix('v').unwrap_or(version);
    version
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
}

/// One statement's text, the file line it starts on, and its tokens other than comments,
/// their starts made relative to the text.
struct Piece {
    text: String,
    line: usize,
    /// Its `[start, end)` bytes in the file; see [`Statement::byte_range`].
    byte_range: Option<(usize, usize)>,
    tokens: Vec<Token>,
}

/// The statements of a batch, whose text is `sql`, whose tokens are `tokens` and whose lines
/// start on the file lines `lines`.
fn split(batch: &BatchTokens) -> Result<Vec<Piece>, Refusal> {
    let mut pieces = Vec::new();
    let mut words: Vec<&Token> = Vec::new();
    for token in &batch.tokens {
        if token.word == ";" {
            pieces.extend(piece(batch, &words, token.start, true)?);
            words.clear();
        } else if !token.is_comment() {
            words.push(token);
        }
    }
    pieces.extend(piece(batch, &words, batch.text.len(), false)?);
    Ok(pieces)
}

/// The statement of `words`, which ends where `end` says in the batch: at its `;` when
/// `terminated`, else at the batch's end.
fn piece(
    batch: &BatchTokens,
    words: &[&Token],
    end: usize,
    terminated: bool,
) -> Result<Option<Piece>, Refusal> {
    let (sql, lines) = (batch.text.as_str(), &batch.lines);
    let Some(first) = words.first() else {
        return Ok(None);
    };
    let text = sql.get(first.start..end).ok_or_else(|| {
        Refusal::Unreadable(format!(
            "a token starts at byte {}, off the text",
            first.start
        ))
    })?;
    // A batch holds one `\n` between each two of its lines.
    let batch_line = sql[..first.start].matches('\n').count();
    let line = lines.get(batch_line).copied().ok_or_else(|| {
        Refusal::Unreadable(format!(
            "a statement starts on line {} of a batch of {} lines",
            batch_line + 1,
            lines.len()
        ))
    })?;
    // The range in the file: from the first word to the `;` that ends the statement, which
    // the range includes, or to the end of what the statement holds.
    let batch_end = if terminated {
        end + 1
    } else {
        first.start + text.trim_end().len()
    };
    let byte_range = batch
        .origin
        .at(first.start)
        .zip(batch.origin.at(batch_end - 1).map(|last| last + 1))
        .filter(|(start, end)| start < end);
    let tokens = words
        .iter()
        .map(|t| Token {
            start: t.start - first.start,
            ..(*t).clone()
        })
        .collect();
    Ok(Some(Piece {
        text: text.trim_end().to_string(),
        line,
        byte_range,
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
    piece: &Piece,
    frame: Frame,
    tree: &Value,
    query: Option<(String, Option<&Value>)>,
) -> Result<Statement, Refusal> {
    let mut statement = Statement {
        text: piece.text.clone(),
        line: piece.line,
        byte_range: piece.byte_range,
        form: frame.form,
        reads: frame.reads,
        produces: frame.produces,
        calls: Vec::new(),
        with: Vec::new(),
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
            statement.form = Form::NotParsed(message);
            return Ok(statement);
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
    statement.with = found.with;
    Ok(statement)
}

/// What a tree reads, each in the order its text names it.
struct Found {
    reads: Vec<Relation>,
    calls: Vec<TableCall>,
    with: Vec<String>,
}

fn walk(tree: &Value, text: &str) -> Found {
    let mut reads = Vec::new();
    let mut calls = Vec::new();
    let mut with = Vec::new();
    visit(tree, text, &[], &mut reads, &mut calls, &mut with);
    reads.sort_by(by_location);
    calls.sort_by(by_location);
    let mut found = Found {
        reads: Vec::new(),
        calls: calls.into_iter().map(|(_, call)| call).collect(),
        with,
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
    declared: &mut Vec<String>,
) {
    let node = match v {
        Value::Array(items) => {
            for item in items {
                visit(item, text, scope, reads, calls, declared);
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
        visit(&cte["value"], text, &body_scope, reads, calls, declared);
        if !declared.contains(&name) {
            declared.push(name.clone());
        }
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
            visit(child, text, &inner, reads, calls, declared);
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
        // `(SELECT src, dst FROM edges)`. Only a subquery holds a `subquery_type`, and
        // `IN (SELECT …)` and `EXISTS (SELECT …)` hold another: they are expressions.
        _ if expression["subquery_type"] == "SCALAR" => ArgValue::Query,
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
/// from the answers DuckDB v2.0.0 gave. Each is read by a check below, so it is recorded.
#[cfg(test)]
pub(crate) mod recorded_steps {
    pub(crate) const ASOF: &str =
        "CREATE TABLE r AS SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k);";
    pub(crate) const READ_XLSX: &str =
        "CREATE TABLE b AS SELECT * FROM read_xlsx('build/budget.xlsx');";
    pub(crate) const RANGE: &str = "CREATE TABLE g AS SELECT * FROM range(10);";
    pub(crate) const MLPACK: &str = "CREATE TABLE m AS SELECT * FROM \
         mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\");";
    pub(crate) const SUBQUERY_CALL: &str =
        "CREATE TABLE paths AS SELECT * FROM onager_pth_dijkstra((SELECT src, dst, w FROM edges));";
    pub(crate) const SUBQUERY_WITH_STRING: &str = "CREATE TABLE r AS SELECT * FROM \
         summary((SELECT src, dst FROM edges WHERE kind = 'road'));";
    pub(crate) const SUBQUERY_BESIDE_STRING: &str =
        "CREATE TABLE m AS SELECT * FROM my_ext_fit((SELECT x, y FROM train), 'model');";
    pub(crate) const COPY_QUERY: &str = "COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER);";
    pub(crate) const COPY_PARTITIONED: &str =
        "COPY (SELECT * FROM a) TO 'parts' (FORMAT parquet, PARTITION_BY (region));";
    pub(crate) const COPY_ONE_FILE_PER_THREAD_OFF: &str =
        "COPY a TO 'one.parquet' (FORMAT parquet, PER_THREAD_OUTPUT false);";
    pub(crate) const PIVOT_JOIN: &str = "PIVOT t JOIN u USING (k) ON c USING sum(v);";

    // The reader-mapping cases: how each shape of a table function's arguments, a
    // table's case and a statement of a bare name come through to the asset graph.
    pub(crate) const READ_JSON_LIST: &str =
        "CREATE TABLE j AS SELECT * FROM read_json(['a.json', 'b.json']);";
    pub(crate) const READ_CSV_NAMED: &str =
        "CREATE TABLE c AS SELECT * FROM read_csv('x.csv', sep := ',');";
    pub(crate) const STRING_ARGUMENT: &str =
        "CREATE TABLE p AS SELECT * FROM read_parquet('data/in.parquet');";
    pub(crate) const EXPRESSION_ARGUMENT: &str = "CREATE TABLE w AS SELECT * FROM recent(7 * 2);";
    pub(crate) const MIXED_CASE: &str =
        "CREATE TABLE Out AS SELECT * FROM \"MyTable\" JOIN Other USING (k);";
    pub(crate) const GLOB: &str = "CREATE TABLE d AS SELECT * FROM read_csv('data/*.csv');";
    /// DuckDB 2.0 reads a statement of a bare name as `FROM tbl`.
    pub(crate) const BARE_NAME: &str = "tbl;";

    /// The steps above, each with the form, reads and produces of its one statement.
    /// The form is the statement's first word, or `QUERY` for one DuckDB gives a tree.
    pub(crate) const ALL: [(&str, &str, &[&str], &[&str]); 18] = [
        (ASOF, "CREATE", &["t", "u"], &["r"]),
        (READ_XLSX, "CREATE", &[], &["b"]),
        (RANGE, "CREATE", &[], &["g"]),
        (MLPACK, "CREATE", &[], &["m"]),
        (SUBQUERY_CALL, "CREATE", &["edges"], &["paths"]),
        (SUBQUERY_WITH_STRING, "CREATE", &["edges"], &["r"]),
        (SUBQUERY_BESIDE_STRING, "CREATE", &["train"], &["m"]),
        (COPY_QUERY, "COPY", &["a"], &["file 'out.csv'"]),
        (COPY_PARTITIONED, "COPY", &["a"], &["file 'parts'"]),
        (
            COPY_ONE_FILE_PER_THREAD_OFF,
            "COPY",
            &["a"],
            &["file 'one.parquet'"],
        ),
        (PIVOT_JOIN, "PIVOT", &[], &[]),
        (READ_JSON_LIST, "CREATE", &[], &["j"]),
        (READ_CSV_NAMED, "CREATE", &[], &["c"]),
        (STRING_ARGUMENT, "CREATE", &[], &["p"]),
        (EXPRESSION_ARGUMENT, "CREATE", &[], &["w"]),
        (MIXED_CASE, "CREATE", &["MyTable", "Other"], &["Out"]),
        (GLOB, "CREATE", &[], &["d"]),
        (BARE_NAME, "QUERY", &["tbl"], &[]),
    ];

    /// The reading of `sql`, one of the steps above, from the recorded answers.
    pub(crate) fn read(sql: &str) -> Vec<super::Statement> {
        reading(sql).statements
    }

    /// [`read`], with the version that answered.
    pub(crate) fn reading(sql: &str) -> super::StepReading {
        super::read_step_with(&mut super::tests::Recorded::load(), sql)
            .unwrap_or_else(|e| panic!("reading {sql:?} was refused: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::duckdb_tokens::tokenize_with;

    /// Names the DuckDB 2.0 build the ignored tests ask. `ci.yml` points it at the v2.0.0 release CLI
    /// and sets it.
    const RECORD_ENV: &str = "ARC_DUCKDB_RECORD_BIN";

    /// What DuckDB v2.0.0 answered for the tree of each text the reader asked about, keyed
    /// by that text. The tokens of each step are the ones `crate::duckdb_tokens::recorded`
    /// holds, as the refusals' tests read them. The ignored test
    /// `the_recorded_answers_are_what_duckdb_answers` compares these with what the
    /// DuckDB `ARC_DUCKDB_RECORD_BIN` names answers now, and prints a fresh copy when they
    /// differ.
    const RECORDED: &str = r##"{"version":"v2.0.0",
"trees":{
"(SELECT * FROM c)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["c"]},"query_location":15,"query_location_length":1,"sample":null,"schema_name":"","table_name":"c","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":8,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"COPY (SELECT * FROM a WHERE x > 1) TO 'out.csv' (HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY (SELECT * FROM a) TO 'e' WITH (PARTITION_BY (), PER_THREAD_OUTPUT 0)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY (SELECT * FROM a) TO 'parts' (FORMAT parquet, PARTITION_BY (region))":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY (SELECT * FROM a) TO out_csv":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY FROM DATABASE a TO b":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY a TO 'one.parquet' (FORMAT parquet, PER_THREAD_OUTPUT false)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY a TO 'out.csv' (HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY s.t (a) FROM 'in.csv' (HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t FROM 'in.csv'":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t FROM 'in.csv' (HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t TO 'd' (FORMAT parquet, PARTITION_BY (a, b), PER_THREAD_OUTPUT false, FILE_SIZE_BYTES '1MB', HEADER)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t TO 'it''s.csv'":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"COPY t TO out_csv":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE MACRO m() AS 1":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE OR REPLACE TEMP TABLE \"R \"\"x\"\"\" AS SELECT * FROM a":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABL r AS SELECT 1":{"error":true,"error_message":"syntax error at or near \"TABL\"","error_subtype":"SYNTAX_ERROR","error_type":"parser","location":"[7,4]","position":"7"},
"CREATE TABLE IF NOT EXISTS db.s.r2 (a INT)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE Out AS SELECT * FROM \"MyTable\" JOIN Other USING (k)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE b AS SELECT * FROM read_xlsx('build/budget.xlsx')":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE c AS SELECT * FROM read_csv('x.csv', sep := ',')":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE d AS SELECT * FROM read_csv('data/*.csv')":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE g AS SELECT * FROM range(10)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE j AS SELECT * FROM read_json(['a.json', 'b.json'])":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE m AS SELECT * FROM mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\")":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE m AS SELECT * FROM my_ext_fit((SELECT x, y FROM train), 'model')":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE p AS SELECT * FROM read_parquet('data/in.parquet')":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE paths AS SELECT * FROM onager_pth_dijkstra((SELECT src, dst, w FROM edges))":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS PIVOT t ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS SELECT * FROM a":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS SELECT * FROM a JOIN b USING (k) WHERE x IN (SELECT x FROM c)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE r AS SELECT * FROM summary((SELECT src, dst FROM edges WHERE kind = 'road'))":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE t AS\n  SELECT 2":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE t AS\r\nSELECT 2":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE t AS SELECT 1":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"CREATE TABLE w AS SELECT * FROM recent(7 * 2)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
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
"PIVOT 'p.csv' JOIN u USING (k) ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT 'p.csv' ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT (FROM t) JOIN u USING (k) ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT (SELECT * FROM a) ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT read_csv('p.csv') ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT t AS x JOIN u USING (k) ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT t AS x ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT t JOIN u USING (k) ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT t ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PIVOT_WIDER s.t ON c USING sum(v)":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"PRAGMA threads = 4":{"error":true,"error_message":"Only SELECT statements can be serialized to json!","error_type":"not implemented"},
"SELECT $$a;b$$":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"a;b"},"query_location":7,"query_location_length":7,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT 'a;b' AS s":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"s","class":"CONSTANT","literal":{"kind":"STRING","text":"a;b"},"query_location":7,"query_location_length":5,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT (SELECT max(d) FROM events)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":7,"query_location_length":27,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["events"]},"query_location":27,"query_location_length":6,"sample":null,"schema_name":"","table_name":"events","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["d"],"query_location":19,"query_location_length":1,"type":"COLUMN_REF"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"max","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["max"]},"query_location":15,"query_location_length":6,"schema":"","type":"FUNCTION"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"SCALAR","type":"SUBQUERY"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * EXCLUDE (a) FROM t ASOF LEFT JOIN u USING (k)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":false,"join_type":"LEFT","left":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["t"]},"query_location":26,"query_location_length":1,"sample":null,"schema_name":"","table_name":"t","type":"BASE_TABLE"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":28,"query_location_length":26,"ranking_expression":null,"ref_type":"ASOF","right":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["u"]},"query_location":43,"query_location_length":1,"sample":null,"schema_name":"","table_name":"u","type":"BASE_TABLE"},"sample":null,"type":"JOIN","using_columns":["k"]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":["a"],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":13,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM \"MyTable\" JOIN Other USING (k)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":false,"join_type":"INNER","left":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["MyTable"]},"query_location":14,"query_location_length":9,"sample":null,"schema_name":"","table_name":"MyTable","type":"BASE_TABLE"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":24,"query_location_length":20,"ranking_expression":null,"ref_type":"REGULAR","right":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["Other"]},"query_location":29,"query_location_length":5,"sample":null,"schema_name":"","table_name":"Other","type":"BASE_TABLE"},"sample":null,"type":"JOIN","using_columns":["k"]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM a":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM a JOIN b USING (k) WHERE x IN (SELECT x FROM c)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":false,"join_type":"INNER","left":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":16,"query_location_length":16,"ranking_expression":null,"ref_type":"REGULAR","right":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["b"]},"query_location":21,"query_location_length":1,"sample":null,"schema_name":"","table_name":"b","type":"BASE_TABLE"},"sample":null,"type":"JOIN","using_columns":["k"]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":{"alias":"","child":{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":39,"query_location_length":1,"type":"COLUMN_REF"},"class":"SUBQUERY","comparison_type":"COMPARE_EQUAL","query_location":44,"query_location_length":17,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["c"]},"query_location":59,"query_location_length":1,"sample":null,"schema_name":"","table_name":"c","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":52,"query_location_length":1,"type":"COLUMN_REF"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"ANY","type":"SUBQUERY"}}}]},
"SELECT * FROM a WHERE x > 1":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["a"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"a","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":22,"query_location_length":1,"type":"COLUMN_REF"},"query_location":22,"query_location_length":5,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":26,"query_location_length":1,"type":"VALUE_CONSTANT"},"type":"COMPARE_GREATERTHAN"}}}]},
"SELECT * FROM b":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["b"]},"query_location":14,"query_location_length":1,"sample":null,"schema_name":"","table_name":"b","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\")":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["X"],"query_location":41,"query_location_length":3,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["Y"],"query_location":46,"query_location_length":3,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["params"],"query_location":51,"query_location_length":8,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["model"],"query_location":61,"query_location_length":7,"type":"COLUMN_REF"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"mlpack_random_forest_train","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["mlpack_random_forest_train"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":55,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM my_ext_fit((SELECT x, y FROM train), 'model')":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":25,"query_location_length":24,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["train"]},"query_location":43,"query_location_length":5,"sample":null,"schema_name":"","table_name":"train","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":33,"query_location_length":1,"type":"COLUMN_REF"},{"alias":"","class":"COLUMN_REF","column_names":["y"],"query_location":36,"query_location_length":1,"type":"COLUMN_REF"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"SCALAR","type":"SUBQUERY"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"model"},"query_location":51,"query_location_length":7,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"my_ext_fit","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["my_ext_fit"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":45,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM onager_pth_dijkstra((SELECT src FROM edges), directed := false), f(name := (SELECT 1)), g(1 IN (SELECT x FROM t)), h((SELECT 1) + 1)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":34,"query_location_length":23,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["edges"]},"query_location":51,"query_location_length":5,"sample":null,"schema_name":"","table_name":"edges","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"COLUMN_REF","column_names":["src"],"query_location":42,"query_location_length":3,"type":"COLUMN_REF"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"SCALAR","type":"SUBQUERY"},"name":""},{"expression":{"alias":"directed","class":"CONSTANT","literal":{"kind":"BOOLEAN","text":"false"},"query_location":71,"query_location_length":5,"type":"VALUE_CONSTANT"},"name":"directed"}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"onager_pth_dijkstra","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["onager_pth_dijkstra"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":63,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":18446744073709551615,"query_location_length":0,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"name","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":89,"query_location_length":10,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":97,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"SCALAR","type":"SUBQUERY"},"name":"name"}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"f","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["f"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":79,"query_location_length":21,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":18446744073709551615,"query_location_length":0,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","child":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":104,"query_location_length":1,"type":"VALUE_CONSTANT"},"class":"SUBQUERY","comparison_type":"COMPARE_EQUAL","query_location":109,"query_location_length":17,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["t"]},"query_location":124,"query_location_length":1,"sample":null,"schema_name":"","table_name":"t","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"COLUMN_REF","column_names":["x"],"query_location":117,"query_location_length":1,"type":"COLUMN_REF"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"ANY","type":"SUBQUERY"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"g","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["g"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":102,"query_location_length":25,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":9,"query_location_length":137,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","arguments":[{"expression":{"alias":"","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":131,"query_location_length":10,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":139,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"SCALAR","type":"SUBQUERY"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":144,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"+","is_operator":true,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["+"]},"query_location":142,"query_location_length":0,"schema":"","type":"FUNCTION"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"h","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["h"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":129,"query_location_length":17,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM onager_pth_dijkstra((SELECT src, dst, w FROM edges))":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":34,"query_location_length":31,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["edges"]},"query_location":59,"query_location_length":5,"sample":null,"schema_name":"","table_name":"edges","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"COLUMN_REF","column_names":["src"],"query_location":42,"query_location_length":3,"type":"COLUMN_REF"},{"alias":"","class":"COLUMN_REF","column_names":["dst"],"query_location":47,"query_location_length":3,"type":"COLUMN_REF"},{"alias":"","class":"COLUMN_REF","column_names":["w"],"query_location":52,"query_location_length":1,"type":"COLUMN_REF"}],"type":"SELECT_NODE","where_clause":null}},"subquery_type":"SCALAR","type":"SUBQUERY"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"onager_pth_dijkstra","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["onager_pth_dijkstra"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":52,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM range(10)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"10"},"query_location":20,"query_location_length":2,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"range","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["range"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":9,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM range(3), read_csv('x.csv'), mlpack_random_forest_train(\"X\", \"Y\", \"params\", \"model\")":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"3"},"query_location":20,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"range","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["range"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":8,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":18446744073709551615,"query_location_length":0,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"x.csv"},"query_location":33,"query_location_length":7,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_csv","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_csv"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":24,"query_location_length":17,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":9,"query_location_length":89,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["X"],"query_location":70,"query_location_length":3,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["Y"],"query_location":75,"query_location_length":3,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["params"],"query_location":80,"query_location_length":8,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["model"],"query_location":90,"query_location_length":7,"type":"COLUMN_REF"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"mlpack_random_forest_train","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["mlpack_random_forest_train"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":43,"query_location_length":55,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_csv('data/*.csv')":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"data/*.csv"},"query_location":23,"query_location_length":12,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_csv","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_csv"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":22,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_csv('x.csv', header = true, sep := ',', \"quote\" = 'q'), f(bare, 1 + 2, t.col, 1.5, NULL)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"x.csv"},"query_location":23,"query_location_length":7,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["header"],"query_location":32,"query_location_length":6,"type":"COLUMN_REF"},"query_location":32,"query_location_length":13,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"BOOLEAN","text":"true"},"query_location":41,"query_location_length":4,"type":"VALUE_CONSTANT"},"type":"COMPARE_EQUAL"},"name":""},{"expression":{"alias":"sep","class":"CONSTANT","literal":{"kind":"STRING","text":","},"query_location":54,"query_location_length":3,"type":"VALUE_CONSTANT"},"name":"sep"},{"expression":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["quote"],"query_location":59,"query_location_length":7,"type":"COLUMN_REF"},"query_location":59,"query_location_length":13,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"q"},"query_location":69,"query_location_length":3,"type":"VALUE_CONSTANT"},"type":"COMPARE_EQUAL"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_csv","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_csv"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":59,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":9,"query_location_length":98,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["bare"],"query_location":77,"query_location_length":4,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":83,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"2"},"query_location":87,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"+","is_operator":true,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["+"]},"query_location":85,"query_location_length":0,"schema":"","type":"FUNCTION"},"name":""},{"expression":{"alias":"","class":"COLUMN_REF","column_names":["t","col"],"query_location":90,"query_location_length":5,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"NUMERIC","text":"1.5"},"query_location":97,"query_location_length":3,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"NULL_LITERAL","text":""},"query_location":102,"query_location_length":4,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"f","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["f"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":75,"query_location_length":32,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_csv('x.csv', sep := ',')":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"x.csv"},"query_location":23,"query_location_length":7,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"sep","class":"CONSTANT","literal":{"kind":"STRING","text":","},"query_location":39,"query_location_length":3,"type":"VALUE_CONSTANT"},"name":"sep"}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_csv","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_csv"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":29,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_json(['a.json', 'b.json'])":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"a.json"},"query_location":25,"query_location_length":8,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"b.json"},"query_location":35,"query_location_length":8,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"list_value","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["list_value"]},"query_location":24,"query_location_length":20,"schema":"","type":"FUNCTION"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_json","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_json"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":31,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_json(['a.json', 'b.json']), f(lower('X')), g(\"Q\" + 1)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","condition":null,"delim_flipped":false,"duplicate_eliminated_columns":[],"is_implicit":true,"join_type":"INNER","left":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"a.json"},"query_location":25,"query_location_length":8,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"b.json"},"query_location":35,"query_location_length":8,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"list_value","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["list_value"]},"query_location":24,"query_location_length":20,"schema":"","type":"FUNCTION"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_json","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_json"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":31,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":18446744073709551615,"query_location_length":0,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"X"},"query_location":55,"query_location_length":3,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"lower","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["lower"]},"query_location":49,"query_location_length":10,"schema":"","type":"FUNCTION"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"f","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["f"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":47,"query_location_length":13,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"nearest_approx":false,"nearest_count":1,"nearest_order_type":"ASCENDING","query_location":9,"query_location_length":63,"ranking_expression":null,"ref_type":"CROSS","right":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","arguments":[{"expression":{"alias":"","class":"COLUMN_REF","column_names":["Q"],"query_location":64,"query_location_length":3,"type":"COLUMN_REF"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":70,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"+","is_operator":true,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["+"]},"query_location":68,"query_location_length":0,"schema":"","type":"FUNCTION"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"g","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["g"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":62,"query_location_length":10,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"sample":null,"type":"JOIN","using_columns":[]},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_parquet('data/in.parquet')":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"data/in.parquet"},"query_location":27,"query_location_length":17,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_parquet","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_parquet"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":31,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM read_xlsx('build/budget.xlsx')":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"build/budget.xlsx"},"query_location":24,"query_location_length":19,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"read_xlsx","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["read_xlsx"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":30,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM recent(7 * 2)":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","arguments":[{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"7"},"query_location":21,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""},{"expression":{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"2"},"query_location":25,"query_location_length":1,"type":"VALUE_CONSTANT"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"*","is_operator":true,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["*"]},"query_location":21,"query_location_length":5,"schema":"","type":"FUNCTION"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"recent","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["recent"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":13,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT * FROM summary((SELECT src, dst FROM edges WHERE kind = 'road'))":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","column_name_alias":[],"function":{"alias":"","arguments":[{"expression":{"alias":"","child":null,"class":"SUBQUERY","comparison_type":"INVALID","query_location":22,"query_location_length":48,"subquery":{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["edges"]},"query_location":44,"query_location_length":5,"sample":null,"schema_name":"","table_name":"edges","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"COLUMN_REF","column_names":["src"],"query_location":30,"query_location_length":3,"type":"COLUMN_REF"},{"alias":"","class":"COLUMN_REF","column_names":["dst"],"query_location":35,"query_location_length":3,"type":"COLUMN_REF"}],"type":"SELECT_NODE","where_clause":{"alias":"","class":"COMPARISON","left":{"alias":"","class":"COLUMN_REF","column_names":["kind"],"query_location":56,"query_location_length":4,"type":"COLUMN_REF"},"query_location":56,"query_location_length":13,"right":{"alias":"","class":"CONSTANT","literal":{"kind":"STRING","text":"road"},"query_location":63,"query_location_length":6,"type":"VALUE_CONSTANT"},"type":"COMPARE_EQUAL"}}},"subquery_type":"SCALAR","type":"SUBQUERY"},"name":""}],"catalog":"","class":"FUNCTION","distinct":false,"export_state":false,"filter":null,"function_name":"summary","is_operator":false,"order_bys":{"orders":[],"type":"ORDER_MODIFIER"},"qualified_name":{"path":["summary"]},"query_location":18446744073709551615,"query_location_length":0,"schema":"","type":"FUNCTION"},"query_location":14,"query_location_length":57,"sample":null,"type":"TABLE_FUNCTION","with_ordinality":"WITHOUT_ORDINALITY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":7,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT 1":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"1"},"query_location":7,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}]},
"SELECT 2":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","query_location":18446744073709551615,"query_location_length":0,"sample":null,"type":"EMPTY"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"CONSTANT","literal":{"kind":"INTEGER","text":"2"},"query_location":7,"query_location_length":1,"type":"VALUE_CONSTANT"}],"type":"SELECT_NODE","where_clause":null}}]},
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
"WITH x AS (SELECT * FROM y), y AS (SELECT * FROM x) SELECT * FROM y":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[{"key":"x","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["y"]},"query_location":25,"query_location_length":1,"sample":null,"schema_name":"","table_name":"y","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}},{"key":"y","value":{"aliases":[],"key_targets":[],"materialized":"CTE_MATERIALIZE_DEFAULT","payload_aggregates":[],"query_node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["x"]},"query_location":49,"query_location_length":1,"sample":null,"schema_name":"","table_name":"x","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":42,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}}]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["y"]},"query_location":66,"query_location_length":1,"sample":null,"schema_name":"","table_name":"y","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":59,"query_location_length":1,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]},
"tbl":{"error":false,"statements":[{"named_param_map":[],"node":{"aggregate_handling":"STANDARD_HANDLING","cte_map":{"map":[]},"from_table":{"alias":"","at_clause":null,"catalog_name":"","column_name_alias":[],"qualified_name":{"path":["tbl"]},"query_location":18446744073709551615,"query_location_length":0,"sample":null,"schema_name":"","table_name":"tbl","type":"BASE_TABLE"},"group_expressions":[],"group_sets":[],"having":null,"modifiers":[],"qualify":null,"sample":null,"select_list":[{"alias":"","class":"STAR","columns":false,"exclude_list":[],"expr":null,"qualified_exclude_list":[],"query_location":18446744073709551615,"query_location_length":0,"relation_name":"","rename_list":[],"replace_list":[],"type":"STAR"}],"type":"SELECT_NODE","where_clause":null}}]}}}"##;

    pub(super) struct Recorded {
        version: String,
        trees: BTreeMap<String, Value>,
    }

    impl Recorded {
        pub(super) fn load() -> Recorded {
            let all: Value = serde_json::from_str(RECORDED).expect("RECORDED is JSON");
            Recorded {
                version: all["version"].as_str().unwrap_or_default().to_string(),
                trees: serde_json::from_value(all["trees"].clone()).unwrap_or_default(),
            }
        }
    }

    impl Ask for Recorded {
        /// The recorded tokens, answered as from the version this holds.
        fn tokens(&mut self, file: &[u8]) -> Result<FileTokens, Refusal> {
            let version = self.version.clone();
            tokenize_with(file, &mut |texts| {
                let (_, tokens) = crate::duckdb_tokens::recorded::ask(texts)?;
                Ok((version.clone(), tokens))
            })
        }

        fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal> {
            let tree = |text: &String| {
                self.trees.get(text).cloned().unwrap_or_else(|| {
                    panic!(
                        "no recorded tree for {text:?}; record it with ARC_DUCKDB_RECORD_BIN set"
                    )
                })
            };
            Ok(texts.iter().map(tree).collect())
        }
    }

    /// The DuckDB `ARC_DUCKDB_RECORD_BIN` names. It prints the version it answered
    /// with, so a log says which build a test ran on.
    struct Live(Process);

    fn live() -> Live {
        let bin = std::env::var_os(RECORD_ENV).unwrap_or_else(|| {
            panic!(
                "{RECORD_ENV} must name a DuckDB 2.0 build: this test is #[ignore]d so an \
                 ordinary run skips it, and running it with --include-ignored says one is \
                 staged"
            )
        });
        Live(Process {
            program: DuckDbProgram::Told(bin.into()),
        })
    }

    impl Ask for Live {
        fn tokens(&mut self, file: &[u8]) -> Result<FileTokens, Refusal> {
            let tokens = self.0.tokens(file)?;
            eprintln!(
                "asked DuckDB {} at {}",
                tokens.version,
                self.0.program.display_name()
            );
            Ok(tokens)
        }

        fn trees(&mut self, texts: &[String]) -> Result<Vec<Value>, Refusal> {
            self.0.trees(texts)
        }
    }

    /// Keeps what another `Ask` answered, to be written into `RECORDED`.
    struct Recording<A> {
        inner: A,
        version: String,
        trees: BTreeMap<String, Value>,
    }

    impl<A: Ask> Ask for Recording<A> {
        fn tokens(&mut self, file: &[u8]) -> Result<FileTokens, Refusal> {
            let tokens = self.inner.tokens(file)?;
            self.version = tokens.version.clone();
            Ok(tokens)
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
        /// `RECORDED`'s text: one line per tree, so a new capture diffs by the entry.
        fn fixture(&self) -> String {
            let trees: Vec<String> = self
                .trees
                .iter()
                .map(|(key, value)| format!("{}:{value}", Value::from(key.as_str())))
                .collect();
            format!(
                "{{\"version\":{},\n\"trees\":{{\n{}}}}}",
                Value::from(self.version.as_str()),
                trees.join(",\n"),
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
        assert_eq!(s[0].with, ["c"], "the WITH's name is the statement's own");
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

    fn a_subquery_argument_is_a_query_and_no_other_expression_is(ask: &mut dyn Ask) {
        let s = read(
            ask,
            "SELECT * FROM onager_pth_dijkstra((SELECT src FROM edges), directed := false), \
             f(name := (SELECT 1)), g(1 IN (SELECT x FROM t)), h((SELECT 1) + 1);",
        );
        let expression = |value: &ArgValue| matches!(value, ArgValue::Expression { .. });
        let calls = &s[0].calls;
        assert_eq!(
            calls[0].args[0],
            arg(None, ArgValue::Query),
            "a parenthesised subquery is a query"
        );
        assert!(
            expression(&calls[0].args[1].value),
            "the argument after it is told apart from it"
        );
        assert_eq!(
            calls[1].args,
            [arg(Some("name"), ArgValue::Query)],
            "a subquery passed by name is a query"
        );
        assert!(
            expression(&calls[2].args[0].value),
            "`IN (SELECT …)` is a subquery of another type, and an expression"
        );
        assert!(
            expression(&calls[3].args[0].value),
            "a subquery inside a sum is an expression that holds one"
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

    fn a_statement_duckdb_cannot_parse_keeps_duckdbs_message(ask: &mut dyn Ask) {
        let s = read(ask, "SELECT 1;\nCREATE TABL r AS SELECT 1;");
        assert_eq!(s.len(), 2, "two statements: {s:#?}");
        assert_eq!(
            (s[1].text.as_str(), s[1].line),
            ("CREATE TABL r AS SELECT 1", 2),
            "the statement, and the line it starts on"
        );
        assert_eq!(
            s[1].form,
            Form::NotParsed("syntax error at or near \"TABL\"".to_string()),
            "a statement DuckDB cannot parse holds DuckDB's own message, and the one before \
             it is read"
        );
        assert_eq!(s[0].form, Form::Query);
    }

    fn a_dot_command_is_no_statement_and_each_statement_knows_its_line(ask: &mut dyn Ask) {
        let s = read(
            ask,
            ".mode list\nSELECT * FROM a;\n\n# a note\nSELECT * FROM b;\n",
        );
        let read: Vec<(Vec<String>, usize)> = s.iter().map(|s| (names(&s.reads), s.line)).collect();
        assert_eq!(
            read,
            [(vec!["a".to_string()], 2), (vec!["b".to_string()], 5)],
            "the command line runs a `.` line and a `#` line as no SQL, and each statement \
             starts on the file line it is on"
        );
    }

    /// The file's text at each statement's byte range.
    fn slices<'a>(ask: &mut dyn Ask, file: &'a str) -> Vec<&'a str> {
        read(ask, file)
            .iter()
            .map(|s| {
                let (start, end) = s
                    .byte_range
                    .unwrap_or_else(|| panic!("no byte range for {s:?} in {file:?}"));
                &file[start..end]
            })
            .collect()
    }

    fn a_dollar_quoted_semicolon_does_not_split_the_ranges(ask: &mut dyn Ask) {
        assert_eq!(
            slices(ask, "SELECT $$a;b$$; CREATE TABLE t AS SELECT 1;"),
            ["SELECT $$a;b$$;", "CREATE TABLE t AS SELECT 1;"],
            "a range is the statement's text from its first word to its `;`, whole"
        );
    }

    fn ranges_skip_the_lines_the_command_line_drops(ask: &mut dyn Ask) {
        assert_eq!(
            slices(
                ask,
                ".mode list\n# a note\nSELECT 1;\n-- c\nCREATE TABLE t AS\n  SELECT 2\n"
            ),
            ["SELECT 1;", "CREATE TABLE t AS\n  SELECT 2"],
            "a dot command and a `#` line are no SQL, so no range starts in them; a comment \
             before a statement is not in its range; the last statement needs no `;`"
        );
    }

    fn ranges_in_a_crlf_file_keep_their_carriage_returns(ask: &mut dyn Ask) {
        assert_eq!(
            slices(ask, "SELECT 1;\r\nCREATE TABLE t AS\r\nSELECT 2;\r\n"),
            ["SELECT 1;", "CREATE TABLE t AS\r\nSELECT 2;"],
            "the ranges are bytes of the file, and a `\\r` inside a statement is in it"
        );
    }

    fn ranges_follow_a_byte_order_mark_and_unicode_spaces(ask: &mut dyn Ask) {
        assert_eq!(
            slices(ask, "\u{feff}SELECT 1;\nSELECT\u{a0}2\u{a0};\n"),
            ["SELECT 1;", "SELECT\u{a0}2\u{a0};"],
            "the parser reads a byte-order mark and a no-break space as one space each, and \
             the ranges are still the file's bytes, which are longer"
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
                "QUERY" => Form::Query,
                _ => Form::Unread(first_word.to_string()),
            };
            assert_eq!(shape(&s[0]), expect(form, reads, produces), "{sql}");
        }
    }

    /// Each check runs twice: from the recorded answers in every `cargo test`, and
    /// against the DuckDB `ARC_DUCKDB_RECORD_BIN` names when it is set.
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

            mod on_duckdb {
                $(
                    #[test]
                    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; \
                                ci.yml's build job runs it on the v2.0.0 release CLI and passes \
                                --include-ignored"]
                    fn $check() {
                        super::$check(&mut super::live());
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
        a_subquery_argument_is_a_query_and_no_other_expression_is,
        a_semicolon_in_a_string_or_comment_does_not_split,
        comments_alone_are_no_statement_and_a_last_semicolon_is_optional,
        a_statement_duckdb_cannot_parse_keeps_duckdbs_message,
        a_dot_command_is_no_statement_and_each_statement_knows_its_line,
        a_dollar_quoted_semicolon_does_not_split_the_ranges,
        ranges_skip_the_lines_the_command_line_drops,
        ranges_in_a_crlf_file_keep_their_carriage_returns,
        ranges_follow_a_byte_order_mark_and_unicode_spaces,
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
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build \
                job runs it on the v2.0.0 release CLI with --include-ignored"]
    fn the_recorded_answers_are_what_duckdb_answers() {
        let mut recording = Recording {
            inner: live(),
            version: String::new(),
            trees: BTreeMap::new(),
        };
        for (name, check) in CHECKS {
            eprintln!("recording {name}");
            check(&mut recording);
        }
        let recorded = Recorded::load();
        if recorded.trees != recording.trees {
            eprintln!("--- RECORDED ---\n{}\n--- END ---", recording.fixture());
            panic!(
                "DuckDB {} answers differently from RECORDED ({}); the fresh answers \
                 are printed above",
                recording.version, recorded.version
            );
        }
    }

    /// A DuckDB before 2.0 has no `sql_tokenize`. The answer is the recorded one with a 1.x
    /// version, since the recorded answers are v2.0.0's. The rule is the one the
    /// refusals' tokens are held to, in `crate::duckdb_tokens`.
    #[test]
    fn a_duckdb_older_than_2_is_refused_naming_its_version() {
        let mut ask = Recorded::load();
        ask.version = "v1.5.6".to_string();
        let refusal = read_step_with(&mut ask, "WITH c AS (SELECT * FROM a) SELECT * FROM c;")
            .expect_err("a 1.x DuckDB is refused");
        let message = refusal.to_string();
        assert!(
            matches!(&refusal, Refusal::NotDuckDb2 { version } if version == "v1.5.6")
                && message.contains("v1.5.6")
                && message.contains("DuckDB 2.x"),
            "the refusal names the version found and says arc reads a 2.x: {message}"
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

    /// The reader hands DuckDB each text written in hex digits. The DuckDB
    /// `ARC_DUCKDB_RECORD_BIN` names reads it back: a quote, a `;`, a backslash, a comment, a `\r` before a `\n` and a
    /// line that would be a command to the DuckDB shell come back as the text sent.
    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build job \
                runs it on the v2.0.0 release CLI with --include-ignored"]
    fn duckdb_reads_the_step_back_as_the_text_sent() {
        let text = "it's; a \\ b\r\n-- not a comment\r\n.quit\n''";
        let duckdb = live().0;
        let (answers, stderr) = duckdb
            .run(&format!("SELECT {} AS s;\n", hex_text(text)))
            .expect("the DuckDB named runs");
        assert_eq!(
            answers.first().map(|rows| &rows[0]["s"]),
            Some(&Value::from(text)),
            "DuckDB reads the literal back as the text sent; stderr: {stderr}"
        );
    }

    /// DuckDB 2.0's preview builds printed a warning they logged into the JSON on stdout,
    /// which then could not be read. This checks that the reader asks the DuckDB
    /// `ARC_DUCKDB_RECORD_BIN` names with logging off; the checks against a real DuckDB show what that buys.
    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build job \
                runs it on the v2.0.0 release CLI with --include-ignored"]
    fn duckdb_is_asked_with_logging_off() {
        let duckdb = live().0;
        let (answers, stderr) = duckdb
            .run("SELECT current_setting('enable_logging')::BOOLEAN AS logging;\n")
            .expect("the DuckDB named runs");
        assert_eq!(
            answers.first().map(|rows| &rows[0]["logging"]),
            Some(&Value::from(false)),
            "the reader's script runs with logging off; stderr: {stderr}"
        );
    }

    /// A file with CRLF line ends, read by the DuckDB `ARC_DUCKDB_RECORD_BIN` names. Its tokens and its trees
    /// reach DuckDB with each `\r`, so each start and place DuckDB gives is a byte of the
    /// text arc holds: the string after the `\r\n` inside the first statement's literal
    /// is still read as a file, and the second statement starts on line 3.
    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build job \
                runs it on the v2.0.0 release CLI with --include-ignored"]
    fn a_crlf_step_is_read_at_the_places_duckdb_gives() {
        let sql = "SELECT 'a\r\nb' AS s FROM 'data.csv';\r\nCREATE TABLE r AS SELECT * FROM t;\r\n";
        let reading = read_step(&live().0.program, sql).expect("the DuckDB named reads it");
        let s = reading.statements;
        assert_eq!(
            s.iter()
                .map(|s| (s.text.as_str(), s.line))
                .collect::<Vec<_>>(),
            [
                ("SELECT 'a\r\nb' AS s FROM 'data.csv'", 1),
                ("CREATE TABLE r AS SELECT * FROM t", 3),
            ],
            "each statement keeps its `\\r`, and starts on its own line"
        );
        assert_eq!(
            shape(&s[0]),
            expect(Form::Query, &["file 'data.csv'"], &[]),
            "the string DuckDB places after the `\\r\\n` is the one read as a file"
        );
        assert_eq!(shape(&s[1]), expect(Form::CreateTable, &["t"], &["r"]));
    }

    #[test]
    fn a_duckdb_that_gives_a_tree_is_one_the_reader_can_ask() {
        assert_eq!(gives_trees_with(&mut Recorded::load()), Ok(()));
        let refusal = gives_trees(&DuckDbProgram::Told("true".into()))
            .expect_err("a program that prints nothing gives no tree");
        assert!(
            matches!(&refusal, Refusal::Unreadable(reason) if reason.contains("no trees")),
            "{refusal:?}"
        );
    }

    #[test]
    fn a_duckdb_that_cannot_be_run_is_named() {
        let refusal = read_step(
            &DuckDbProgram::Told("/nonexistent/duckdb".into()),
            "SELECT 1;",
        )
        .expect_err("no such program");
        assert!(
            matches!(&refusal, Refusal::NotStarted { program, reason }
                if program == "/nonexistent/duckdb" && reason.contains("No such file")),
            "the refusal names the program and why it did not run: {refusal:?}"
        );
    }

    /// A program that runs and prints no version is not a DuckDB.
    #[test]
    fn a_program_that_gives_no_version_is_refused() {
        let refusal =
            read_step(&DuckDbProgram::Told("true".into()), "SELECT 1;").expect_err("not a DuckDB");
        assert!(
            matches!(&refusal, Refusal::Unreadable(reason) if reason.contains("no version")),
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

    /// Answers made by hand, for what DuckDB does not answer when asked well.
    struct Scripted {
        tokens: Result<Vec<Token>, String>,
        trees: Result<Vec<Value>, Refusal>,
    }

    impl Ask for Scripted {
        fn tokens(&mut self, file: &[u8]) -> Result<FileTokens, Refusal> {
            let tokens = self.tokens.clone().map_err(Refusal::Unreadable)?;
            tokenize_with(file, &mut |texts| {
                Ok((
                    "v2.0.0".to_string(),
                    texts.iter().map(|_| tokens.clone()).collect(),
                ))
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
