//! The tokens DuckDB 2.0 reads in a SQL file, asked of the DuckDB command line that runs it.
//!
//! `sql_tokenize` gives each token of a text with the byte it starts at; it runs nothing,
//! and it tokenises text its parser cannot parse. arc's refusals read a SQL step's tokens
//! from it, through the same command line that runs the step, so the refusals read the
//! file as that DuckDB reads it. What the command line does to a file before its parser
//! sees it (dot commands, `#` lines, how lines batch into statements) is not in
//! `sql_tokenize`, so the file is first cut into batches by `engine::cli_batches`, and each
//! batch has the parser's unicode-space pass applied by `engine::strip_unicode_spaces`. Every
//! batch's tokens come from one DuckDB process.
//!
//! A token is read by its text alone. `token_type`, the part the tokenizer says the token
//! plays, is kept for the record and read nowhere: on 2.0 it changes with what came before
//! the token in the same text.
//!
//! A file's tokens are kept in a [`TokenCache`] by the SHA-256 of its bytes, so a file read
//! again unchanged is not asked about again.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::duckdb_lineage::{Refusal, major};
use crate::engine::{DuckDbProgram, Tok, cli_batches, strip_unicode_spaces};

/// A token as `sql_tokenize` gives it. `start` is a byte offset into the text asked
/// about: on v2.0.0 a token after a two-byte `é` starts two bytes on, not one. `token_type`
/// is kept for the record and read nowhere; see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
pub(crate) struct Token {
    pub(crate) start: usize,
    #[serde(default)]
    pub(crate) token_type: String,
    pub(crate) word: String,
}

impl Token {
    /// A comment, by its text: `sql_tokenize` returns a comment as a token of its own.
    pub(crate) fn is_comment(&self) -> bool {
        is_comment(&self.word)
    }
}

fn is_comment(word: &str) -> bool {
    word.starts_with("--") || word.starts_with("/*")
}

/// A file's tokens, batch by batch, as one DuckDB answered them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileTokens {
    /// The version the DuckDB asked reported, as `version()` gives it.
    pub(crate) version: String,
    pub(crate) batches: Vec<BatchTokens>,
    /// The tokens of each string a `query(…)` call is given, by its text after the
    /// parser's unicode-space pass: `query()` runs that text as SQL, and the scan reads the
    /// calls it makes. Asked in a second question, and only when the file holds one.
    pub(crate) literals: BTreeMap<String, Vec<Token>>,
}

/// One batch of a file, as the DuckDB command line hands it to its parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchTokens {
    /// The file line each of the batch's lines starts on.
    pub(crate) lines: Vec<usize>,
    /// The batch as DuckDB's tokenizer reads it: after the parser's unicode-space pass,
    /// and with any byte that is not UTF-8 replaced, as DuckDB refuses a batch holding one
    /// and runs none of it.
    pub(crate) text: String,
    pub(crate) tokens: Vec<Token>,
}

/// The DuckDB command line asked, and how it is asked.
pub(crate) struct Process {
    pub(crate) program: DuckDbProgram,
}

impl Process {
    /// The version, and the tokens of each of `texts` in order, from one process.
    ///
    /// Each text is its own `SELECT` from `sql_tokenize`, after a line marking which text
    /// it is, and is sent as [`hex_text`] writes it. Each token comes back as its start and
    /// its length in bytes, one to a
    /// line in DuckDB's `-list` output: a token's text is the span of the text asked about,
    /// and a line holds nothing a token's text could make ambiguous. That keeps a
    /// five-megabyte file's answer quick to read. A text whose spans do not each start
    /// after the last one ends, on a character boundary, is asked again for its tokens'
    /// words: on v2.0.0 a `$` read as an operator leaves the next token's start on itself.
    pub(crate) fn ask_texts(&self, texts: &[String]) -> Result<Answer, Refusal> {
        let mut script = format!("SELECT '{MARK}version:' || version();\n");
        for (n, text) in texts.iter().enumerate() {
            script.push_str(&format!(
                "SELECT '{MARK}text:{n}';\nSELECT start || ',' || strlen(word) FROM sql_tokenize({});\n",
                hex_text(text)
            ));
        }
        let output = self.output(&["-list", "-noheader"], &script)?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(Refusal::NotRun {
                program: self.program.display_name(),
                reason: format!("it exited with {}: {}", output.status, stderr.trim()),
            });
        }
        let unreadable = |reason: String| {
            Refusal::Unreadable(format!(
                "DuckDB's answer could not be read as tokens: {reason}; {}",
                stderr.trim()
            ))
        };
        let (version, spans) = read_spans(&output.stdout, texts.len()).map_err(unreadable)?;
        let mut answers = Vec::with_capacity(texts.len());
        for (text, spans) in texts.iter().zip(spans) {
            answers.push(match words(text, &spans) {
                Some(tokens) => tokens,
                None => self.words(text)?,
            });
        }
        Ok((version, answers))
    }

    /// The tokens of `text`, each with its word as DuckDB prints it.
    fn words(&self, text: &str) -> Result<Vec<Token>, Refusal> {
        let script = format!(
            "SELECT start, token_type, word FROM sql_tokenize({});\n",
            hex_text(text)
        );
        let (answers, stderr) = self.run(&script)?;
        let rows = answers.first().ok_or_else(|| {
            Refusal::Unreadable(format!("no tokens for a text: {}", stderr.trim()))
        })?;
        serde_json::from_value(rows.clone())
            .map_err(|e| Refusal::Unreadable(format!("the tokens of a text: {e}")))
    }

    /// Runs `script` and returns each result DuckDB printed, as JSON, with its stderr.
    pub(crate) fn run(&self, script: &str) -> Result<(Vec<Value>, String), Refusal> {
        let output = self.output(&["-json"], script)?;
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

    /// Runs `script` on an in-memory DuckDB with `-no-init` and the output `mode`, so a
    /// `~/.duckdbrc` changes neither what runs nor how it prints.
    ///
    /// Logging is turned off first. The 2.0 shell prints a warning DuckDB logs, such as
    /// the one for parsing the deprecated `PRAGMA threads = 4`, to stdout in the middle
    /// of the JSON it is printing, and the answer can no longer be read.
    fn output(&self, mode: &[&str], script: &str) -> Result<std::process::Output, Refusal> {
        let not_started = |e: std::io::Error| Refusal::NotStarted {
            program: self.program.display_name(),
            reason: e.to_string(),
        };
        let not_run = |e: std::io::Error| Refusal::NotRun {
            program: self.program.display_name(),
            reason: e.to_string(),
        };
        let mut child = self
            .program
            .command()
            .arg("-no-init")
            .args(mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(not_started)?;
        // Written from a thread: DuckDB answers each statement as it reads it, and an
        // answer larger than the pipe would stall a write made from this thread.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let script = format!("SET enable_logging = false;\n{script}");
        let writer = std::thread::spawn(move || stdin.write_all(script.as_bytes()));
        let output = child.wait_with_output().map_err(not_run)?;
        // A write cut short by DuckDB exiting shows in its stderr and its exit status.
        let _ = writer.join();
        Ok(output)
    }
}

/// What arc prints before each line of its own in a token answer, which no token's span
/// line can start with.
const MARK: &str = "arc-tokens:";

/// Each text's tokens as their starts and lengths in bytes.
type Spans = Vec<Vec<(usize, usize)>>;

/// The version and the spans, start and length, of `count` texts' tokens in `stdout`: a
/// version line, then for each text in order a line naming it and a line for each token.
/// Any other line is an answer arc cannot read.
fn read_spans(stdout: &[u8], count: usize) -> Result<(String, Spans), String> {
    let stdout = std::str::from_utf8(stdout).map_err(|e| e.to_string())?;
    let mut lines = stdout.lines();
    let version = lines
        .next()
        .and_then(|line| line.strip_prefix(MARK)?.strip_prefix("version:"))
        .ok_or("no version")?
        .to_string();
    let mut texts: Vec<Vec<(usize, usize)>> = Vec::with_capacity(count);
    for line in lines {
        if let Some(n) = line
            .strip_prefix(MARK)
            .and_then(|l| l.strip_prefix("text:"))
        {
            if n != texts.len().to_string() {
                return Err(format!("text {n} where text {} was due", texts.len()));
            }
            texts.push(Vec::new());
            continue;
        }
        let span = line
            .split_once(',')
            .and_then(|(start, len)| Some((start.parse().ok()?, len.parse().ok()?)))
            .ok_or_else(|| format!("a line that is not a token: {line:?}"))?;
        texts
            .last_mut()
            .ok_or("a token before the first text")?
            .push(span);
    }
    if texts.len() != count {
        return Err(format!("{} answers for {count} texts", texts.len()));
    }
    Ok((version, texts))
}

/// The tokens of `text` its `spans` give, or `None` when a span does not start where the
/// last one ended or later, or does not lie on character boundaries inside the text.
fn words(text: &str, spans: &[(usize, usize)]) -> Option<Vec<Token>> {
    let mut end = 0;
    let mut tokens = Vec::with_capacity(spans.len());
    for &(start, len) in spans {
        if start < end {
            return None;
        }
        end = start.checked_add(len)?;
        tokens.push(Token {
            start,
            token_type: String::new(),
            word: text.get(start..end)?.to_string(),
        });
    }
    Some(tokens)
}

/// `text` as SQL that evaluates to it, written in hex digits alone: `decode(unhex('…'))`.
///
/// The script goes to the DuckDB command line on its standard input, which the command line
/// reads a line at a time: it drops the `\r` before each `\n`, even inside a string, and
/// reads a line opening with `.` as a command when no statement is open. A text written in
/// hex digits reaches `sql_tokenize` byte for byte. `text` is UTF-8, which `decode` requires.
pub(crate) fn hex_text(text: &str) -> String {
    let mut hex = String::with_capacity(text.len() * 2 + 18);
    hex.push_str("decode(unhex('");
    for byte in text.bytes() {
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        hex.push(char::from(b"0123456789abcdef"[usize::from(byte & 0xf)]));
    }
    hex.push_str("'))");
    hex
}

/// What a question for the tokens of several texts answers: the version, and each text's
/// tokens in order.
pub(crate) type Answer = (String, Vec<Vec<Token>>);

/// Each batch's tokens in `file`, asked of `program` in one process, and the tokens of each
/// string a `query(…)` call is given, asked in another when there is one.
pub(crate) fn tokenize_file(program: &DuckDbProgram, file: &[u8]) -> Result<FileTokens, Refusal> {
    let process = Process {
        program: program.clone(),
    };
    tokenize_with(file, &mut |texts| process.ask_texts(texts))
}

/// [`tokenize_file`], with `ask` answering for DuckDB.
pub(crate) fn tokenize_with(
    file: &[u8],
    ask: &mut dyn FnMut(&[String]) -> Result<Answer, Refusal>,
) -> Result<FileTokens, Refusal> {
    let cut: Vec<(String, Vec<usize>)> = cli_batches(file)
        .into_iter()
        .map(|(batch, lines)| (as_tokenized(&batch), lines))
        .collect();
    // A text of spaces alone holds no token, and is not asked about: a file with no SQL,
    // empty or only dot commands and `#` lines, starts no DuckDB and has nothing to refuse.
    let asking: Vec<String> = cut
        .iter()
        .map(|(text, _)| text.clone())
        .filter(|text| !is_blank(text))
        .collect();
    let (version, mut asked_answers) = if asking.is_empty() {
        (String::new(), Vec::new())
    } else {
        asked(ask, &asking)?
    };
    asked_answers.reverse();
    let answers: Vec<Vec<Token>> = cut
        .iter()
        .map(|(text, _)| {
            if is_blank(text) {
                Vec::new()
            } else {
                asked_answers.pop().unwrap_or_default()
            }
        })
        .collect();
    let batches: Vec<BatchTokens> = cut
        .into_iter()
        .zip(answers)
        .map(|((text, lines), tokens)| BatchTokens {
            lines,
            text,
            tokens,
        })
        .collect();

    let mut literals = BTreeMap::new();
    let mut pending: Vec<String> = Vec::new();
    for batch in &batches {
        add_query_literals(&mut pending, &literals, &batch.text, &batch.tokens);
    }
    // Each round asks about strings held inside the last round's, which are shorter, so
    // the rounds end.
    while !pending.is_empty() {
        let (_, answers) = asked(ask, &pending)?;
        let mut next = Vec::new();
        for (text, tokens) in std::mem::take(&mut pending).into_iter().zip(answers) {
            add_query_literals(&mut next, &literals, &text, &tokens);
            literals.insert(text, tokens);
        }
        next.retain(|text| !literals.contains_key(text));
        pending = next;
    }
    Ok(FileTokens {
        version,
        batches,
        literals,
    })
}

/// A batch as DuckDB's tokenizer reads it: the parser's unicode-space pass, and each byte
/// that is not UTF-8 replaced. DuckDB 2.0's parser refuses a batch that is not UTF-8 before
/// it reads a token of it (`Parser::NormalizeSQLString`, `src/parser/parser.cpp`), so such a
/// batch runs nothing, and reading it with the bytes replaced can refuse only what would not
/// have run.
fn as_tokenized(batch: &[u8]) -> String {
    String::from_utf8_lossy(&strip_unicode_spaces(batch)).into_owned()
}

/// Whether `text` holds only the characters DuckDB's tokenizer skips between tokens
/// (`StringUtil::CharacterIsSpace`, `src/include/duckdb/common/string_util.hpp`): it holds
/// no token.
fn is_blank(text: &str) -> bool {
    text.bytes()
        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'))
}

/// Asks `ask` about `texts`, and refuses an answer from a DuckDB that is not a 2.x build,
/// whose tokenizer arc reads by, an answer that does not hold one token list per text, or a
/// token that starts off its text.
fn asked(
    ask: &mut dyn FnMut(&[String]) -> Result<Answer, Refusal>,
    texts: &[String],
) -> Result<Answer, Refusal> {
    let (version, answers) = ask(texts)?;
    if major(&version) != Some(2) {
        return Err(Refusal::NotDuckDb2 { version });
    }
    if answers.len() != texts.len() {
        return Err(Refusal::Unreadable(format!(
            "{} token lists for {} texts",
            answers.len(),
            texts.len()
        )));
    }
    for (text, tokens) in texts.iter().zip(&answers) {
        if let Some(token) = tokens.iter().find(|t| t.start > text.len()) {
            return Err(Refusal::Unreadable(format!(
                "a token starts at byte {}, off a text of {} bytes",
                token.start,
                text.len()
            )));
        }
    }
    Ok((version, answers))
}

/// Adds to `pending` each string a `query(…)` call in `tokens` is given that is not in
/// `known`, after the parser's unicode-space pass, as `query()` hands it to the parser.
fn add_query_literals(
    pending: &mut Vec<String>,
    known: &BTreeMap<String, Vec<Token>>,
    text: &str,
    tokens: &[Token],
) {
    // Most texts name no `query`, and reading those is all the cost of a large one.
    let names_query = |t: &Token| {
        t.word.len() <= "`query`".len()
            && t.word
                .trim_matches(['"', '`'])
                .eq_ignore_ascii_case("query")
    };
    if !tokens.iter().any(names_query) {
        return;
    }
    let toks = to_tok(text.as_bytes(), tokens);
    for window in toks.windows(3) {
        let [
            (Tok::Word { text: name, .. }, _),
            (Tok::Other('('), _),
            (Tok::Str { text: literal, .. }, _),
        ] = window
        else {
            continue;
        };
        if !name.eq_ignore_ascii_case("query") {
            continue;
        }
        let literal = as_tokenized(literal.as_bytes());
        if !known.contains_key(&literal) && !pending.contains(&literal) {
            pending.push(literal);
        }
    }
}

/// Counts the lines of a batch up to each token, reading on from the last token asked
/// about.
struct Lines<'a> {
    text: &'a [u8],
    at: usize,
    line: usize,
}

impl Lines<'_> {
    /// The batch line, counting from 1, the byte at `start` is on: one more than the `\n`
    /// before it. A batch holds one `\n` between each two of its lines.
    fn at(&mut self, start: usize) -> usize {
        let start = start.min(self.text.len());
        let newlines = |span: &[u8]| span.iter().filter(|&&b| b == b'\n').count();
        if start < self.at {
            // A start before the last one, as after a `$` read as an operator: step back.
            self.line -= newlines(&self.text[start..self.at]);
        } else {
            self.line += newlines(&self.text[self.at..start]);
        }
        self.at = start;
        self.line
    }
}

/// The prefix of a single-quoted string in `word`, as DuckDB 2.0's `GetSpecialStringInfo`
/// reads it (`src/include/duckdb/parser/peg/special_string_utils.hpp`): `'` alone, or one of
/// `E`, `N`, `X` and `B` in either case before it.
fn quote_prefix(word: &str) -> Option<usize> {
    match word.as_bytes() {
        [b'\'', ..] => Some(1),
        [
            b'E' | b'e' | b'N' | b'n' | b'X' | b'x' | b'B' | b'b',
            b'\'',
            ..,
        ] => Some(2),
        _ => None,
    }
}

/// A single-quoted string's text, as DuckDB 2.0's `StringLiteralMatcher` strips it
/// (`src/include/duckdb/parser/peg/matcher/string_literal_matcher.hpp`): the prefix and the
/// quotes off, and each `''` read as one `'`. One left open runs to the end of its text,
/// where DuckDB refuses the statement rather than running it.
fn strip_quoted(word: &str, prefix: usize) -> String {
    let body = &word[prefix..];
    let body = if word.len() > prefix && word.ends_with('\'') {
        &body[..body.len() - 1]
    } else {
        body
    };
    body.replace("''", "'")
}

/// Whether `word` is a single-quoted string DuckDB closed.
fn closed_quoted(word: &str, prefix: usize) -> bool {
    word.len() > prefix && word.ends_with('\'')
}

/// What an `E'…'` string evaluates to, as DuckDB 2.0's `StringLiteralParseResult` decodes
/// it (`src/include/duckdb/parser/peg/transformer/parse_result.hpp`): `\b`, `\f`, `\n`,
/// `\r`, `\t`, `\\` and `\'`; one to three octal digits; `\x` and one or two hex digits, or
/// the letter alone; any other character stands for itself, and a backslash at the end for
/// itself. DuckDB refuses a decoded string that is not UTF-8; arc reads its bytes replaced.
fn e_unescape(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b[i] {
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'0'..=b'7' => {
                let end = (i..b.len().min(i + 3))
                    .find(|&k| !(b'0'..=b'7').contains(&b[k]))
                    .unwrap_or(b.len().min(i + 3));
                let value = b[i..end]
                    .iter()
                    .fold(0u32, |v, d| v * 8 + u32::from(d - b'0'));
                out.push((value & 0xff) as u8);
                i = end - 1;
            }
            b'x' => {
                let end = (i + 1..b.len().min(i + 3))
                    .find(|&k| !b[k].is_ascii_hexdigit())
                    .unwrap_or(b.len().min(i + 3));
                if end > i + 1 {
                    let hex = std::str::from_utf8(&b[i + 1..end]).unwrap_or("0");
                    out.push(u8::from_str_radix(hex, 16).unwrap_or(0));
                    i = end - 1;
                } else {
                    out.push(b'x');
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A dollar-quoted string's text: what lies between its `$tag$` and the closing one, or to
/// the end of its text when it is left open, as DuckDB 2.0's `StringLiteralMatcher` finds
/// the tag (`TryGetDollarQuoteDelimiterLength`) and strips it (`StripStringLiteral`), in
/// `src/include/duckdb/parser/peg/matcher/string_literal_matcher.hpp`.
fn dollar_body(word: &str) -> Option<&str> {
    let rest = word.strip_prefix('$')?;
    let close = rest.find('$')?;
    let tag = &word[..close + 2];
    let body = &word[tag.len()..];
    Some(match body.strip_suffix(tag) {
        Some(inner) if word.len() >= 2 * tag.len() => inner,
        _ => body,
    })
}

/// A word DuckDB's tokenizer reads as a keyword or an identifier: one it starts on anything
/// that is not a space, a digit or ASCII punctuation other than `_`
/// (`Tokenizer::TokenizeInputInternal`, `src/parser/peg/tokenizer/base_tokenizer.cpp`, with
/// `StringUtil::CharacterIsOperator`). That is a letter, `_`, a control character or any
/// character above ASCII.
fn is_word_start(c: char) -> bool {
    c == '_' || !(c.is_ascii_digit() || c.is_ascii_punctuation() || c.is_ascii_whitespace())
}

/// The tokens of `batch`, as the refusals read them, each with the batch line it starts on.
///
/// A token is classified by its text, never by its `token_type`:
///
/// - one opening with `--` or `/*` is a comment, and is dropped;
/// - one opening with `'`, or `E'`, `N'`, `X'` or `B'` in either case, is a [`Tok::Str`]:
///   `raw` the text DuckDB strips from it, and `text` that with an `E'…'` string's escapes
///   decoded. A plain `'…'` string that follows a single-quoted one across whitespace or
///   `--` comments holding a line end, and no `/* */` comment, continues it, as DuckDB's
///   `StringLiteralMatcher` joins them;
/// - one opening with `$` and a tag is a dollar-quoted [`Tok::Str`];
/// - one opening with `"` or `` ` `` is a quoted [`Tok::Word`], its doubled quotes read as
///   one: DuckDB 2.0 takes either as an identifier's quotes;
/// - one [`is_word_start`] opens is a [`Tok::Word`];
/// - anything else, a number or an operator, is a [`Tok::Other`] per character.
pub(crate) fn to_tok(batch: &[u8], tokens: &[Token]) -> Vec<(Tok, usize)> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut lines = Lines {
        text: batch,
        at: 0,
        line: 1,
    };
    let mut k = 0;
    while k < tokens.len() {
        let token = &tokens[k];
        let word = token.word.as_str();
        k += 1;
        if is_comment(word) {
            continue;
        }
        let line = lines.at(token.start);
        if let Some(prefix) = quote_prefix(word) {
            let mut raw = strip_quoted(word, prefix);
            let mut end = token.start + word.len();
            let mut closed = closed_quoted(word, prefix);
            while closed {
                let Some((next, after)) = continuation(batch, tokens, k, end) else {
                    break;
                };
                raw.push_str(&strip_quoted(&tokens[next].word, 1));
                end = tokens[next].start + tokens[next].word.len();
                closed = closed_quoted(&tokens[next].word, 1);
                k = after;
            }
            let text = if word.starts_with(['E', 'e']) {
                e_unescape(&raw)
            } else {
                raw.clone()
            };
            out.push((Tok::Str { text, raw }, line));
            continue;
        }
        if let Some(body) = dollar_body(word) {
            out.push((
                Tok::Str {
                    text: body.to_string(),
                    raw: body.to_string(),
                },
                line,
            ));
            continue;
        }
        // DuckDB 2.0 takes `"` or a backtick as an identifier's quotes, a doubled one inside
        // as one (`Tokenizer::IsQuotedIdentifierDelimiter`,
        // `src/parser/peg/tokenizer/base_tokenizer.cpp`).
        if let Some(delimiter) = word.chars().next().filter(|c| matches!(c, '"' | '`')) {
            let body = &word[1..];
            let body = match body.strip_suffix(delimiter) {
                Some(inner) if word.len() >= 2 => inner,
                _ => body,
            };
            let doubled: String = [delimiter, delimiter].iter().collect();
            out.push((
                Tok::Word {
                    text: body.replace(&doubled, &delimiter.to_string()),
                    quoted: true,
                },
                line,
            ));
            continue;
        }
        if word.chars().next().is_some_and(is_word_start) {
            out.push((
                Tok::Word {
                    text: word.to_string(),
                    quoted: false,
                },
                line,
            ));
            continue;
        }
        out.extend(word.chars().map(|ch| (Tok::Other(ch), line)));
    }
    out
}

/// The plain `'…'` string that continues a single-quoted string ending at byte `end`, from
/// the token at `from` on, and the index after it: the next token that is not a comment,
/// when it is a plain single-quoted string, the text between holds `\n` or `\r`, and no
/// `/* */` comment lies between. DuckDB 2.0's tokenizer marks a token preceded by a line end
/// or a block comment (`TokenizerBehavior::PushToken`, `base_tokenizer.cpp`), and its
/// `StringLiteralMatcher` joins on the first mark and not the second.
fn continuation(batch: &[u8], tokens: &[Token], from: usize, end: usize) -> Option<(usize, usize)> {
    let mut block = false;
    let mut next = from;
    while let Some(token) = tokens.get(next) {
        if !token.is_comment() {
            break;
        }
        block |= token.word.starts_with("/*");
        next += 1;
    }
    let token = tokens.get(next)?;
    let gap = batch.get(end..token.start)?;
    let plain = token.word.starts_with('\'') && closed_quoted(&token.word, 1);
    (plain && !block && gap.iter().any(|&b| b == b'\n' || b == b'\r')).then_some((next, next + 1))
}

/// The tokens of each file a run has asked about, by the SHA-256 of its bytes. The checks
/// before a run and just before each step share one, so a file read again unchanged is not
/// asked about again.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct TokenCache {
    files: HashMap<[u8; 32], FileTokens>,
}

impl TokenCache {
    /// The tokens of `bytes`, from what was asked before when the same bytes were, else
    /// from `ask`, which is kept for the next time. A failed question is not kept.
    pub(crate) fn get(
        &mut self,
        bytes: &[u8],
        ask: impl FnOnce(&[u8]) -> Result<FileTokens, Refusal>,
    ) -> Result<&FileTokens, Refusal> {
        let key: [u8; 32] = Sha256::digest(bytes).into();
        Ok(match self.files.entry(key) {
            std::collections::hash_map::Entry::Occupied(known) => known.into_mut(),
            std::collections::hash_map::Entry::Vacant(new) => new.insert(ask(bytes)?),
        })
    }

    /// Whether a file this holds has a token, so a statement for a reader to read.
    pub(crate) fn holds_sql(&self) -> bool {
        self.files
            .values()
            .any(|file| file.batches.iter().any(|batch| !batch.tokens.is_empty()))
    }

    /// The tokens of `bytes` when they have been asked about before.
    fn known(&self, bytes: &[u8]) -> Option<&FileTokens> {
        let key: [u8; 32] = Sha256::digest(bytes).into();
        self.files.get(&key)
    }
}

/// The tokens of `file` as `program` gives them, shared by everything in this process that
/// reads a SQL file: the refusals and the asset graph read one answer for the same bytes,
/// and a file read again unchanged is not asked about again. Each program has a cache of its
/// own. A program that could not be started is not asked again in the process, so a command that
/// reads many files with no DuckDB starts none after the first; any other failed question is
/// not kept.
///
/// The cache is not held while DuckDB is asked, so two threads asking about different files
/// do not wait on each other; two asking about the same file at once may both ask.
pub(crate) fn shared_tokens(program: &DuckDbProgram, file: &[u8]) -> Result<FileTokens, Refusal> {
    shared_tokens_with(program, file, |file| tokenize_file(program, file))
}

/// [`shared_tokens`], with `ask` asking `program` for a file's tokens.
fn shared_tokens_with(
    program: &DuckDbProgram,
    file: &[u8],
    ask: impl FnOnce(&[u8]) -> Result<FileTokens, Refusal>,
) -> Result<FileTokens, Refusal> {
    #[derive(Default)]
    struct Shared {
        caches: BTreeMap<String, TokenCache>,
        not_run: BTreeMap<String, Refusal>,
    }
    static SHARED: Mutex<Option<Shared>> = Mutex::new(None);
    let name = program.display_name();
    let lock = || SHARED.lock().unwrap_or_else(PoisonError::into_inner);
    {
        let mut held = lock();
        let shared = held.get_or_insert_with(Shared::default);
        if let Some(refusal) = shared.not_run.get(&name) {
            return Err(refusal.clone());
        }
        if let Some(known) = shared.caches.get(&name).and_then(|cache| cache.known(file)) {
            return Ok(known.clone());
        }
    }
    let tokens = match ask(file) {
        Ok(tokens) => tokens,
        Err(refusal) => {
            if matches!(refusal, Refusal::NotStarted { .. }) {
                let mut held = lock();
                let shared = held.get_or_insert_with(Shared::default);
                shared.not_run.insert(name, refusal.clone());
            }
            return Err(refusal);
        }
    };
    let mut held = lock();
    let shared = held.get_or_insert_with(Shared::default);
    shared
        .caches
        .entry(name)
        .or_default()
        .get(file, |_| Ok(tokens.clone()))
        .cloned()
}

/// Tokens for the tests, recorded from the DuckDB v2.0.0 command line, or asked of the one
/// `ARC_DUCKDB_RECORD_BIN` names when it is set.
///
/// A test that reads a text the recording does not hold fails, naming the text. To record
/// it, run the tests with `ARC_DUCKDB_RECORD_BIN` naming DuckDB v2.0.0 and
/// `ARC_RECORD_TOKENS=1`, which adds each text the tests ask about to
/// `src/duckdb_tokens_recorded.json`. The ignored test
/// `the_recorded_tokens_are_what_duckdb_answers` compares the recording with what that
/// DuckDB answers now.
#[cfg(test)]
pub(crate) mod recorded {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Names the DuckDB 2.0 build the ignored tests ask. `ci.yml` points it at the v2.0.0
    /// release CLI and sets it.
    pub(crate) const BIN_ENV: &str = "ARC_DUCKDB_RECORD_BIN";
    const RECORD_ENV: &str = "ARC_RECORD_TOKENS";
    const RECORDED: &str = include_str!("duckdb_tokens_recorded.json");
    const RECORDING: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/duckdb_tokens_recorded.json"
    );

    /// The recording: the version that answered, and each text's tokens as start and word.
    #[derive(Deserialize, Default)]
    pub(crate) struct Recording {
        pub(crate) version: String,
        pub(crate) tokens: BTreeMap<String, Vec<(usize, String)>>,
    }

    pub(crate) fn recording() -> &'static Recording {
        static LOADED: OnceLock<Recording> = OnceLock::new();
        LOADED.get_or_init(|| serde_json::from_str(RECORDED).expect("the recording is JSON"))
    }

    /// The DuckDB `ARC_DUCKDB_RECORD_BIN` names, if it is set.
    pub(crate) fn record_bin() -> Option<DuckDbProgram> {
        std::env::var_os(BIN_ENV).map(|bin| DuckDbProgram::Told(bin.into()))
    }

    /// The DuckDB `ARC_DUCKDB_RECORD_BIN` names, for a test that is `#[ignore]`d because
    /// it needs one.
    pub(crate) fn record_program() -> DuckDbProgram {
        record_bin().unwrap_or_else(|| {
            panic!(
                "{BIN_ENV} must name a DuckDB 2.0 build: this test is #[ignore]d so an \
                 ordinary run skips it, and running it with --include-ignored says one is \
                 staged"
            )
        })
    }

    /// `file`'s tokens, from the recording or the DuckDB `ARC_DUCKDB_RECORD_BIN` names.
    pub(crate) fn tokenize(file: &[u8]) -> Result<FileTokens, Refusal> {
        tokenize_with(file, &mut ask)
    }

    /// The tokens of `texts`, as [`tokenize_with`] asks for them.
    pub(crate) fn ask(texts: &[String]) -> Result<Answer, Refusal> {
        if let Some(program) = record_bin() {
            let answer = Process { program }.ask_texts(texts)?;
            if std::env::var_os(RECORD_ENV).is_some() {
                record(&answer.0, texts, &answer.1);
            }
            return Ok(answer);
        }
        let recording = recording();
        let tokens = texts
            .iter()
            .map(|text| {
                let recorded = recording.tokens.get(text).unwrap_or_else(|| {
                    panic!(
                        "no tokens are recorded for {text:?}: run the tests with \
                         {BIN_ENV} naming DuckDB v2.0.0 and {RECORD_ENV}=1 to record them"
                    )
                });
                recorded
                    .iter()
                    .map(|(start, word)| Token {
                        start: *start,
                        token_type: String::new(),
                        word: word.clone(),
                    })
                    .collect()
            })
            .collect();
        Ok((recording.version.clone(), tokens))
    }

    /// Adds each of `texts` to the recording on disk, one text to a line.
    fn record(version: &str, texts: &[String], answers: &[Vec<Token>]) {
        static WRITING: Mutex<()> = Mutex::new(());
        let _held = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let mut on_disk: Recording = std::fs::read_to_string(RECORDING)
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        on_disk.version = version.to_string();
        for (text, tokens) in texts.iter().zip(answers) {
            on_disk.tokens.insert(
                text.clone(),
                tokens.iter().map(|t| (t.start, t.word.clone())).collect(),
            );
        }
        std::fs::write(RECORDING, write_recording(&on_disk)).expect("the recording is written");
    }

    pub(crate) fn write_recording(recording: &Recording) -> String {
        let mut json = format!(
            "{{\"version\":{},\n\"tokens\":{{\n",
            serde_json::to_string(&recording.version).unwrap()
        );
        let entries: Vec<String> = recording
            .tokens
            .iter()
            .map(|(text, tokens)| {
                format!(
                    "{}:{}",
                    serde_json::to_string(text).unwrap(),
                    serde_json::to_string(tokens).unwrap()
                )
            })
            .collect();
        json.push_str(&entries.join(",\n"));
        json.push_str("\n}}\n");
        json
    }
}

#[cfg(test)]
mod tests {
    use super::recorded::{record_program, recording, tokenize};
    use super::*;

    fn word(text: &str) -> Tok {
        Tok::Word {
            text: text.to_string(),
            quoted: false,
        }
    }

    fn quoted(text: &str) -> Tok {
        Tok::Word {
            text: text.to_string(),
            quoted: true,
        }
    }

    fn string(text: &str) -> Tok {
        Tok::Str {
            text: text.to_string(),
            raw: text.to_string(),
        }
    }

    /// The tokens of `sql`, each with its line in `sql`.
    fn lex(sql: &str) -> Vec<(Tok, usize)> {
        let tokens = tokenize(sql.as_bytes()).expect("tokens");
        tokens
            .batches
            .iter()
            .flat_map(|batch| {
                to_tok(batch.text.as_bytes(), &batch.tokens)
                    .into_iter()
                    .map(|(tok, line)| (tok, batch.lines[line - 1]))
            })
            .collect()
    }

    /// The tokens of `sql`, without their lines.
    fn toks(sql: &str) -> Vec<Tok> {
        lex(sql).into_iter().map(|(tok, _)| tok).collect()
    }

    #[test]
    fn to_tok_splits_words_strings_and_punctuation() {
        assert_eq!(
            toks("INSTALL x FROM 'a';"),
            vec![
                word("INSTALL"),
                word("x"),
                word("FROM"),
                string("a"),
                Tok::Other(';')
            ]
        );
        assert_eq!(
            toks("1 - 2 / _a é"),
            vec![
                Tok::Other('1'),
                Tok::Other('-'),
                Tok::Other('2'),
                Tok::Other('/'),
                word("_a"),
                word("é")
            ]
        );
        assert_eq!(toks("a$$b c1"), vec![word("a$$b"), word("c1")]);
    }

    #[test]
    fn to_tok_drops_a_line_comment_to_the_end_of_its_line() {
        assert_eq!(
            lex("-- INSTALL a\nINSTALL b"),
            vec![(word("INSTALL"), 2), (word("b"), 2)]
        );
    }

    #[test]
    fn to_tok_nests_block_comments_as_duckdb_does() {
        assert_eq!(
            toks("/* /* */ INSTALL a; */ INSTALL b"),
            vec![word("INSTALL"), word("b")]
        );
        assert_eq!(toks("/* a */ x /* b */"), vec![word("x")]);
        assert_eq!(lex("/*\n\n*/ x"), vec![(word("x"), 3)]);
        assert_eq!(toks("/* open /* INSTALL a */ INSTALL b"), Vec::<Tok>::new());
    }

    #[test]
    fn to_tok_reads_a_doubled_quote_as_one_and_a_backslash_as_itself() {
        assert_eq!(toks("'it''s' x"), vec![string("it's"), word("x")]);
        assert_eq!(toks(r"'a\' x"), vec![string(r"a\"), word("x")]);
        assert_eq!(toks("'open"), vec![string("open")]);
        assert_eq!(toks(r#""a""b" c"#), vec![quoted(r#"a"b"#), word("c")]);
        // DuckDB 2.0 takes a backtick as an identifier's quote too.
        assert_eq!(toks("`a``b` c"), vec![quoted("a`b"), word("c")]);
    }

    #[test]
    fn to_tok_honours_a_backslash_only_inside_an_e_string() {
        let e = |text: &str, raw: &str| Tok::Str {
            text: text.to_string(),
            raw: raw.to_string(),
        };
        assert_eq!(toks(r"E'a\'b' x"), vec![e("a'b", r"a\'b"), word("x")]);
        assert_eq!(toks(r"e'a\'b' x"), vec![e("a'b", r"a\'b"), word("x")]);
        assert_eq!(toks("E'a''b'"), vec![string("a'b")]);
        assert_eq!(toks("e x"), vec![word("e"), word("x")]);
        assert_eq!(
            toks(r"ex'a\' x"),
            vec![word("ex"), string(r"a\"), word("x")]
        );
        assert_eq!(lex("E'\\\n' x"), vec![(e("\n", "\\\n"), 1), (word("x"), 2)]);
        assert_eq!(
            toks(r"E'\x5f\137\x\q\''"),
            vec![e("__xq'", r"\x5f\137\x\q\'")]
        );
    }

    /// DuckDB 2.0 reads `X'…'` and `B'…'` as it reads `'…'`, a doubled quote and all; 1.5's
    /// scanner ended them at their first quote.
    #[test]
    fn to_tok_reads_a_bit_or_hex_string_as_one_string() {
        assert_eq!(toks("X'ab''cd'"), vec![string("ab'cd")]);
        assert_eq!(toks("b'01''10'"), vec![string("01'10")]);
        assert_eq!(toks("N'n'"), vec![string("n")]);
    }

    #[test]
    fn to_tok_reads_dollar_quoted_strings() {
        assert_eq!(toks("$$ a ' b $$ x"), vec![string(" a ' b "), word("x")]);
        assert_eq!(toks("$t$ $$ $t$ x"), vec![string(" $$ "), word("x")]);
        assert_eq!(toks("$$ open"), vec![string(" open")]);
        // `$1` is a parameter, so `$1a$` opens no string: a tag cannot start with a digit.
        assert_eq!(
            toks("$1 $name x"),
            vec![
                Tok::Other('$'),
                Tok::Other('1'),
                Tok::Other('$'),
                word("name"),
                word("x")
            ]
        );
        assert_eq!(lex("$$\n\n$$ x"), vec![(string("\n\n"), 1), (word("x"), 3)]);
    }

    #[test]
    fn to_tok_counts_lines_inside_a_string() {
        assert_eq!(
            lex("'a\nb' x\ny"),
            vec![(string("a\nb"), 1), (word("x"), 2), (word("y"), 3)]
        );
    }

    #[test]
    fn to_tok_joins_a_string_continued_on_a_later_line_as_duckdb_does() {
        for sql in [
            "'h3'\n'x'",
            "'h3'\r'x'",
            "'h3' -- note\n'x'",
            "'h3'\n  -- note\n\t'x'",
        ] {
            assert_eq!(toks(sql), vec![string("h3x")], "{sql:?}");
        }
        // An E'…' string's escapes reach the string that continues it.
        assert_eq!(
            toks("E'h\\x33'\n'\\x78'"),
            vec![Tok::Str {
                text: "h3x".into(),
                raw: r"h\x33\x78".into()
            }]
        );
        // Not on one line, not after a block comment, and not a string with a prefix.
        assert_eq!(toks("'h3' 'x'"), vec![string("h3"), string("x")]);
        assert_eq!(toks("'h3'\n/* c */\n'x'"), vec![string("h3"), string("x")]);
        assert_eq!(toks("'h3'\nE'x'").len(), 2);
        assert_eq!(toks("'h3'\n$$x$$").len(), 2);
    }

    #[test]
    fn an_empty_or_dot_command_only_file_has_nothing_to_refuse() {
        for file in ["", "  \n\t\n", ".mode list\n", "# a note\n.print hi\n"] {
            let tokens = tokenize(file.as_bytes()).expect("an empty answer is an answer");
            assert!(
                tokens.batches.iter().all(|batch| batch.tokens.is_empty()),
                "{file:?}: {tokens:?}"
            );
        }
    }

    #[test]
    fn a_file_with_no_sql_text_starts_no_duckdb() {
        let mut fails = |_: &[String]| -> Result<Answer, Refusal> {
            panic!("a file with no SQL text asks DuckDB nothing")
        };
        for file in ["", "  \n\t\r\n", ".mode list\n", "# a note\n.print hi\n"] {
            let tokens = tokenize_with(file.as_bytes(), &mut fails).expect("nothing to ask");
            assert!(
                tokens.batches.iter().all(|b| b.tokens.is_empty()),
                "{file:?}"
            );
        }
        // Only the text holding SQL is asked about.
        let mut asked = Vec::new();
        let mut ask = |texts: &[String]| {
            asked.extend(texts.iter().cloned());
            Ok((
                "v2.0.0".to_string(),
                texts.iter().map(|_| Vec::new()).collect(),
            ))
        };
        tokenize_with(b".mode list\nSELECT 1;\n", &mut ask).unwrap();
        assert_eq!(asked, vec!["SELECT 1;".to_string()]);
    }

    /// A program that could not be started is not asked again in the process: with no DuckDB,
    /// a command reading many files starts it once.
    #[test]
    fn a_program_that_could_not_be_started_is_not_asked_again() {
        let program = DuckDbProgram::Told("/nonexistent/shared-tokens-test/duckdb".into());
        let refused = shared_tokens_with(&program, b"SELECT 1;", |_| {
            Err(Refusal::NotStarted {
                program: program.display_name(),
                reason: "No such file".to_string(),
            })
        });
        assert!(
            matches!(refused, Err(Refusal::NotStarted { .. })),
            "{refused:?}"
        );
        let again = shared_tokens_with(&program, b"SELECT 2;", |_| {
            panic!("a program that could not be started was asked again")
        });
        assert_eq!(again, refused, "the first refusal is given again");

        // A program that ran and failed is asked again: the failure can be the file's.
        let failing = DuckDbProgram::Told("/nonexistent/shared-tokens-test/failing".into());
        let not_run = || {
            Err(Refusal::NotRun {
                program: "failing".to_string(),
                reason: "exit 1".to_string(),
            })
        };
        let _ = shared_tokens_with(&failing, b"SELECT 1;", |_| not_run());
        let mut asked = false;
        let _ = shared_tokens_with(&failing, b"SELECT 1;", |_| {
            asked = true;
            not_run()
        });
        assert!(asked, "a program that ran and failed is asked again");
    }

    #[test]
    fn tokens_from_a_duckdb_that_is_not_2_x_are_refused() {
        for version in ["v1.5.6", "v3.0.0", ""] {
            let mut ask = |texts: &[String]| {
                Ok((
                    version.to_string(),
                    texts.iter().map(|_| Vec::new()).collect(),
                ))
            };
            assert!(
                matches!(
                    tokenize_with(b"SELECT 1;", &mut ask),
                    Err(Refusal::NotDuckDb2 { .. })
                ),
                "{version:?}"
            );
        }
    }

    #[test]
    fn a_line_after_a_start_that_steps_back_is_counted_from_where_it_was() {
        let text = b"a\nb\nc\nd";
        let mut lines = Lines {
            text,
            at: 0,
            line: 1,
        };
        assert_eq!(lines.at(6), 4);
        assert_eq!(lines.at(2), 2);
        assert_eq!(lines.at(4), 3);
        assert_eq!(lines.at(0), 1);
    }

    #[test]
    fn an_answer_with_a_list_too_few_or_a_token_off_its_text_is_refused() {
        let mut too_few = |_: &[String]| Ok(("v2.0.0".to_string(), Vec::new()));
        assert!(matches!(
            tokenize_with(b"SELECT 1;", &mut too_few),
            Err(Refusal::Unreadable(_))
        ));
        let mut off = |texts: &[String]| {
            Ok((
                "v2.0.0".to_string(),
                texts
                    .iter()
                    .map(|_| {
                        vec![Token {
                            start: 99,
                            token_type: String::new(),
                            word: "x".into(),
                        }]
                    })
                    .collect(),
            ))
        };
        assert!(matches!(
            tokenize_with(b"SELECT 1;", &mut off),
            Err(Refusal::Unreadable(_))
        ));
    }

    #[test]
    fn the_answer_is_a_version_then_each_text_and_its_spans_and_nothing_else() {
        let (version, spans) = read_spans(
            b"arc-tokens:version:v2.0.0\narc-tokens:text:0\narc-tokens:text:1\n0,6\n7,1\n",
            2,
        )
        .unwrap();
        assert_eq!(version, "v2.0.0");
        assert_eq!(spans, vec![vec![], vec![(0, 6), (7, 1)]]);
        for bad in [
            &b"arc-tokens:version:v2.0.0\narc-tokens:text:0\n"[..],
            b"arc-tokens:version:v2.0.0\narc-tokens:text:1\narc-tokens:text:0\n",
            b"arc-tokens:version:v2.0.0\narc-tokens:text:0\n0,6\n| a |\narc-tokens:text:1\n",
            b"arc-tokens:version:v2.0.0\n0,6\narc-tokens:text:0\narc-tokens:text:1\n",
            b"| version |\n",
        ] {
            assert!(
                read_spans(bad, 2).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    /// A `\r` before a `\n`, a line opening with `.` and a quote reach DuckDB as written,
    /// so each token starts where it starts in the text arc holds.
    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build \
                job runs it"]
    fn each_text_reaches_duckdb_byte_for_byte() {
        let text = "SELECT 'a\r\n.quit\n''é';\r\nINSTALL x;".to_string();
        let process = Process {
            program: record_program(),
        };
        let (_, answers) = process.ask_texts(std::slice::from_ref(&text)).unwrap();
        let words: Vec<&str> = answers[0].iter().map(|t| t.word.as_str()).collect();
        assert_eq!(
            words,
            vec!["SELECT", "'a\r\n.quit\n''é'", ";", "INSTALL", "x", ";"]
        );
        let spans = |tokens: &[Token]| -> Vec<(usize, String)> {
            tokens.iter().map(|t| (t.start, t.word.clone())).collect()
        };
        assert_eq!(spans(&process.words(&text).unwrap()), spans(&answers[0]));
    }

    #[test]
    fn spans_that_overlap_or_split_a_character_give_no_words() {
        let words_of = |spans: &[(usize, usize)]| {
            words("SELECT 'é';", spans).map(|t| t.into_iter().map(|t| t.word).collect::<Vec<_>>())
        };
        assert_eq!(
            words_of(&[(0, 6), (7, 4), (11, 1)]),
            Some(vec!["SELECT".into(), "'é'".into(), ";".into()])
        );
        assert_eq!(words_of(&[(0, 6), (5, 1)]), None);
        assert_eq!(words_of(&[(0, 6), (7, 2)]), None);
        assert_eq!(words_of(&[(0, 6), (11, 9)]), None);
    }

    /// A `$` DuckDB reads as an operator leaves the start of the token after it on itself,
    /// so those spans overlap and the text's words are asked for instead.
    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build \
                job runs it"]
    fn a_text_whose_spans_overlap_is_read_from_its_words() {
        let tokens = tokenize_file(&record_program(), b"SELECT $(1);").unwrap();
        let words: Vec<&str> = tokens.batches[0]
            .tokens
            .iter()
            .map(|t| t.word.as_str())
            .collect();
        assert_eq!(words, vec!["SELECT", "$", "(", "1", ")", ";"]);
    }

    #[test]
    fn a_file_read_again_unchanged_is_not_asked_again() {
        let mut cache = TokenCache::default();
        let mut asked = 0;
        for bytes in [&b"SELECT 1;"[..], b"SELECT 2;", b"SELECT 1;"] {
            cache
                .get(bytes, |b| {
                    asked += 1;
                    tokenize(b)
                })
                .unwrap();
        }
        assert_eq!(asked, 2, "one question for each distinct text");
        // A failed question is not kept, and the next asks again.
        let mut fresh = TokenCache::default();
        assert!(
            fresh
                .get(b"SELECT 3;", |_| Err(Refusal::Unreadable("no".into())))
                .is_err()
        );
        assert!(
            fresh
                .get(b"SELECT 3;", |_| Ok(tokenize(b"SELECT 3;").unwrap()))
                .is_ok()
        );
    }

    /// `start` is a byte offset into the text asked about: the token after a two-byte `é`
    /// starts two bytes on.
    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build \
                job runs it"]
    fn a_token_starts_at_a_byte_offset() {
        let tokens = tokenize_file(&record_program(), "SELECT 'é', x;".as_bytes()).unwrap();
        let starts: Vec<(usize, &str)> = tokens.batches[0]
            .tokens
            .iter()
            .map(|t| (t.start, t.word.as_str()))
            .collect();
        assert_eq!(
            starts,
            vec![(0, "SELECT"), (7, "'é'"), (11, ","), (13, "x"), (14, ";")]
        );
    }

    /// Each text the recording holds is tokenised as DuckDB tokenises it now.
    #[test]
    #[ignore = "starts the DuckDB 2.0 build ARC_DUCKDB_RECORD_BIN names; ci.yml's build \
                job runs it"]
    fn the_recorded_tokens_are_what_duckdb_answers() {
        let process = Process {
            program: record_program(),
        };
        let texts: Vec<String> = recording().tokens.keys().cloned().collect();
        let (version, answers) = process.ask_texts(&texts).unwrap();
        let mut differ = Vec::new();
        for (text, tokens) in texts.iter().zip(&answers) {
            let now: Vec<(usize, String)> =
                tokens.iter().map(|t| (t.start, t.word.clone())).collect();
            if recording().tokens[text] != now {
                differ.push(format!("{text:?}: now {now:?}"));
            }
        }
        assert_eq!(
            version,
            recording().version,
            "the recording names the version"
        );
        assert!(
            differ.is_empty(),
            "the recording differs:\n{}",
            differ.join("\n")
        );
    }
}
