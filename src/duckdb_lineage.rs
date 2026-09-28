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
//! A table function is returned as a call, its name and its arguments. Which function
//! reads a file and which reads no table is decided by whoever reads the call.

// Nothing calls the reader yet. The attribute goes when `arc run` reads a step with it.
#![allow(dead_code)]

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
        for part in [&self.catalog, &self.schema].into_iter().flatten() {
            write!(f, "{part}.")?;
        }
        f.write_str(&self.name)
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
    /// Any other expression, as written.
    Expression(String),
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
                write!(f, "could not ask DuckDB `{program}` for its parse: {reason}")
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
        Ok((answers, String::from_utf8_lossy(&output.stderr).into_owned()))
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
        let query = frame.query.clone().zip(held.next());
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
    version.split('.').next()?.parse().ok()
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
        Refusal::Unreadable(format!("a token starts at byte {}, off the text", first.start))
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
        (w.is(0, "install") || w.is(0, "checkpoint")).then(Frame::no_data)
    } else {
        NO_DATA.iter().any(|kw| w.is(0, kw)).then(Frame::no_data)
    };
    framed.unwrap_or_else(|| Frame {
        form: Form::Unread(piece.tokens[0].word.to_uppercase()),
        reads: Vec::new(),
        produces: Vec::new(),
        query: None,
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
        w.group()?;
    }
    let query = if w.eat("as") {
        Some(w.rest().to_string())
    } else if form == Form::CreateView {
        return None;
    } else {
        None
    };
    Some(Frame {
        form,
        reads: Vec::new(),
        produces: vec![Relation::Table(name)],
        query,
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
        w.ident()?;
    }
    // A group opening with a query is the query; any other group lists columns.
    let opens_query = ["select", "with", "from", "values", "("]
        .iter()
        .any(|kw| w.is(1, kw));
    if w.is(0, "(") && !opens_query {
        w.group()?;
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
    })
}

/// `COPY (query) TO 'file'`, `COPY name [(cols)] TO 'file'`, `COPY name [(cols)] FROM
/// 'file'`
fn copy(w: &mut Words) -> Option<Frame> {
    if w.is(0, "(") {
        let inner = w.group()?;
        if !w.eat("to") {
            return None;
        }
        let file = w.string()?;
        return Some(Frame {
            form: Form::Copy,
            reads: Vec::new(),
            produces: vec![Relation::File(file)],
            query: Some(w.text[inner].to_string()),
        });
    }
    let table = Relation::Table(w.name()?);
    if w.is(0, "(") {
        w.group()?;
    }
    let (reads, produces) = if w.eat("to") {
        (table, Relation::File(w.string()?))
    } else if w.eat("from") {
        (Relation::File(w.string()?), table)
    } else {
        return None;
    };
    Some(Frame {
        form: Form::Copy,
        reads: vec![reads],
        produces: vec![produces],
        query: None,
    })
}

/// `PIVOT name|'file'|(query) ON …`. DuckDB gives no tree for a `PIVOT` whose columns
/// it would have to read the data to know.
fn pivot(w: &mut Words) -> Option<Frame> {
    let (reads, query) = if w.is(0, "(") {
        let inner = w.group()?;
        (Vec::new(), Some(w.text[inner].to_string()))
    } else if let Some(file) = w.string() {
        (vec![Relation::File(file)], None)
    } else {
        (vec![Relation::Table(w.name()?)], None)
    };
    Some(Frame {
        form: Form::Pivot,
        reads,
        produces: Vec::new(),
        query,
    })
}

/// `SET VARIABLE name = expr` reads what `SELECT expr` reads. Any other `SET` changes a
/// setting.
fn set(w: &mut Words) -> Option<Frame> {
    if !w.eat("variable") {
        return Some(Frame::no_data());
    }
    w.ident()?;
    if !(w.eat("=") || w.eat("to")) {
        return None;
    }
    Some(Frame {
        form: Form::SetVariable,
        reads: Vec::new(),
        produces: Vec::new(),
        query: Some(format!("SELECT {}", w.rest())),
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
        self.tokens.get(self.at).map_or(self.text.len(), |t| t.start)
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
            if depth == 0 && (self.is(0, "returning") || (self.is(0, "on") && self.is(1, "conflict"))) {
                break;
            }
            self.at += 1;
        }
        self.text[start..self.offset()].trim_end()
    }

    /// A name, bare or double-quoted.
    fn ident(&mut self) -> Option<String> {
        let word = self.word(0)?;
        let name = if let Some(quoted) = word.strip_prefix('"').and_then(|w| w.strip_suffix('"'))
        {
            quoted.replace("\"\"", "\"")
        } else if word.starts_with(|c: char| c.is_alphabetic() || c == '_') {
            word.to_string()
        } else {
            return None;
        };
        self.at += 1;
        Some(name)
    }

    /// `name`, `schema.name` or `catalog.schema.name`.
    fn name(&mut self) -> Option<TableName> {
        let mut parts = vec![self.ident()?];
        while self.eat(".") {
            parts.push(self.ident()?);
        }
        let name = parts.pop()?;
        let schema = parts.pop();
        let catalog = parts.pop();
        parts.is_empty().then_some(TableName {
            catalog,
            schema,
            name,
        })
    }

    /// A single-quoted string, without its quotes.
    fn string(&mut self) -> Option<String> {
        let word = self.word(0)?;
        let inner = word.strip_prefix('\'')?.strip_suffix('\'')?.replace("''", "'");
        self.at += 1;
        Some(inner)
    }

    /// Steps past a parenthesised group and returns the range of text inside it.
    fn group(&mut self) -> Option<Range<usize>> {
        let open = self.tokens.get(self.at)?.start;
        let mut depth = 0usize;
        while let Some(token) = self.tokens.get(self.at) {
            self.at += 1;
            match token.word.as_str() {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(open + 1..token.start);
                    }
                }
                _ => {}
            }
        }
        None
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
    query: Option<(String, &Value)>,
) -> Result<Statement, Refusal> {
    let mut statement = Statement {
        text: piece.text.clone(),
        form: frame.form,
        reads: frame.reads,
        produces: frame.produces,
        calls: Vec::new(),
    };
    let (tree, text) = match answer(tree) {
        Answer::Tree(tree) => {
            statement.form = Form::Query;
            statement.reads.clear();
            statement.produces.clear();
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
            Some((text, held)) => match answer(held) {
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
    let with = node["cte_map"]["map"].as_array().into_iter().flatten();
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
    let location = node["query_location"].as_u64().unwrap_or(u64::MAX);
    match node["type"].as_str() {
        Some("BASE_TABLE") => reads.extend(base_table(node, text, &inner).map(|r| (location, r))),
        Some("TABLE_FUNCTION") => calls.extend(table_call(&node["function"], text).map(|c| (location, c))),
        _ => {}
    }
    for (key, child) in node {
        if key != "cte_map" {
            visit(child, text, &inner, reads, calls);
        }
    }
}

/// The text a node was parsed from, by the location DuckDB gives it.
fn source<'t>(node: &serde_json::Map<String, Value>, text: &'t str) -> &'t str {
    let at = |key: &str| node.get(key).and_then(Value::as_u64).map(|n| n as usize);
    at("query_location")
        .zip(at("query_location_length"))
        .and_then(|(start, len)| text.get(start..start + len))
        .unwrap_or_default()
}

fn base_table(
    node: &serde_json::Map<String, Value>,
    text: &str,
    with: &[String],
) -> Option<Relation> {
    let part = |key: &str| {
        node.get(key)
            .and_then(Value::as_str)
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
    if source(node, text).starts_with('\'') {
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
    if expression["type"] == "COMPARE_EQUAL" {
        if let [Value::String(name)] = expression["left"]["column_names"].as_array().map_or(&[][..], Vec::as_slice) {
            return Arg {
                name: Some(name.clone()),
                value: arg_value(&expression["right"], text),
            };
        }
    }
    Arg {
        name: None,
        value: arg_value(expression, text),
    }
}

fn arg_value(expression: &Value, text: &str) -> ArgValue {
    let empty = serde_json::Map::new();
    let written = source(expression.as_object().unwrap_or(&empty), text);
    let literal = &expression["literal"];
    let literal_text = literal["text"].as_str().unwrap_or_default().to_string();
    match (expression["class"].as_str(), literal["kind"].as_str()) {
        (Some("CONSTANT"), Some("INTEGER" | "NUMERIC")) => ArgValue::Number(literal_text),
        (Some("CONSTANT"), Some("STRING")) => ArgValue::String(literal_text),
        (Some("COLUMN_REF"), _) => match expression["column_names"].as_array().map(Vec::as_slice) {
            Some([Value::String(name)]) if written.starts_with('"') => {
                ArgValue::QuotedIdentifier(name.clone())
            }
            Some([Value::String(name)]) => ArgValue::Identifier(name.clone()),
            _ => ArgValue::Expression(written.to_string()),
        },
        _ => ArgValue::Expression(written.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
}
