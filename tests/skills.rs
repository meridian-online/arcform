//! The agent skills under `skills/`, held to the binary they teach.
//!
//! A skill is Markdown: `skills/<name>/SKILL.md` and any `references/**/*.md` beside it. Prose can
//! drift from the tools and verbs `arc` really has, so these tests read every skill file and check
//! what it names against the real `arc` binary. A folder with no `skills/` passes vacuously.
//!
//! Failures name the skill file and the line (`skills/x/SKILL.md:12: ...`).
//!
//! # What a skill author writes, and what is checked
//!
//! **An MCP tool** is recognised in exactly three places, so that prose such as `arcform.yaml` or
//! `row_count` is never mistaken for one:
//!
//!   1. *A verb table.* A table whose header row has a cell containing `MCP` (and one containing
//!      `CLI`). In each row's MCP cell the first backticked name is the tool; every further
//!      backticked name in that same cell that is a plain identifier (`[a-z][a-z0-9_]*`) is an
//!      argument of it. A cell that begins `None:` names no tool and is not read.
//!   2. *A tool named in prose.* Anywhere else outside a fence (a sentence, a trap table, a verb
//!      table's other cells), a backticked identifier followed by ` with ` is a tool, and the
//!      backticked identifiers after it, up to the end of the clause (the next `.`, `;`, `:`, `|`,
//!      `(` or `)`), are its arguments: "`sql_query` with `sql`, `dir`". So write a value as
//!      `` `edit` set to "delete" ``, not `` `edit` `delete` ``, and do not follow a backticked word
//!      that is not a tool with " with ".
//!   3. *A JSON tool-call block.* A fenced `json` block that parses to
//!      `{"name": "<tool>", "arguments": {...}}` (or the same under a `"params"` key), where the
//!      top-level keys of `arguments` are the arguments; or a fenced block whose info string is
//!      `json tool=<tool>`, whose body is the arguments object itself. Any `json` block with a
//!      `name` key is a tool call, with or without `arguments`, and a `json` block that does not
//!      parse is an error: show sample output, or anything else with a `name` key, under another
//!      info string such as `text`.
//!
//! Each tool must be advertised by `tools/list`, and each argument must be a key of that tool's
//! `inputSchema.properties`.
//!
//! **A CLI form** is `arc <verb> [<sub>] [--flags]`, found at the start of a command in a fenced
//! `sh` block (commands are split on `&&`, `||`, `|` and `;`; backslash continuations are joined)
//! or in a backticked span that begins `arc `. `arc <verb> [<sub>] --help` must exit 0 (the sub
//! is the token straight after the verb, when the verb has subcommands), and every `--flag` in the
//! form must appear in that help (or in `arc --help`, for global flags). A form whose verb is a
//! placeholder (a token beginning `<`, or `...` or `…`) is skipped, as is a placeholder
//! subcommand; any other verb or subcommand arc lacks is an error, so `arc table_describe` fails.
//! Leading `NAME=value` assignments are skipped.
//! Not checked: `finetype ...` (a separate binary), forms wrapped in `time`, `$(...)` and the
//! like, and forms in fences other than `sh`.
//!
//! **A verb table** (a table with both `CLI` and `MCP` header cells) must have something in both
//! cells of every row, and each MCP cell must begin with a backticked tool or with `None:` and why
//! no tool does that work. `every_verb_table_row_has_both_columns` prints the `None:` cells, so the
//! known gaps in the MCP tools stay in view.
//!
//! **An example** is a fenced block with the info string `sh arc-example`. It runs under `sh -c`,
//! with `set -e` and `arc` on `PATH`, in a fresh copy of `tests/fixtures/skills/` (or of
//! `tests/fixtures/<name>/` for `sh arc-example fixture=<name>`), and must exit 0. If the next
//! block is `text expect`, its trimmed text must appear in the example's stdout or stderr. A
//! pipeline's exit status is its last command's, so an example should not pipe `arc`'s output.
//!
//! An example says what it needs in its info string, and nothing else decides it: `needs=finetype`
//! when it runs the `finetype` binary, directly or through `arc table profile` (which runs it);
//! `needs=finetype-ext` when it needs FineType's DuckDB extension, as `arc table validate` does;
//! `needs=finetype,finetype-ext` for both. Any other need is an error. An example that needs
//! `finetype` is skipped, with a printed reason, when the binary (`FINETYPE_BIN`, or `finetype` on
//! `PATH`) is absent or older than the floor in `src/finetype_cli.rs`. One that needs the extension
//! is skipped when `FINETYPE_DUCKDB_EXT` is unset, unless `ARCFORM_REQUIRE_FINETYPE_EXT` is set: then
//! the test panics instead. CI stages the extension and sets both, so the extension's examples run
//! there; it has no `finetype` binary, so the `needs=finetype` examples are skipped there.
//!
//! **Frontmatter** (`SKILL.md`): valid YAML (so an unquoted description containing `:` is
//! refused), `name` equal to the folder, hyphen-case and at most 64 characters, `description`
//! present, at most 1,024 characters and free of `<` and `>` (which skill loaders refuse), and
//! `metadata.arc_version` equal to this crate's version.

#![cfg(feature = "mcp")]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Value, json};

const ARC: &str = env!("CARGO_BIN_EXE_arc");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ---------------------------------------------------------------------------------------------
// Reading skills
// ---------------------------------------------------------------------------------------------

/// A fenced code block.
struct Block {
    /// 1-based line of the opening fence.
    line: usize,
    /// 1-based line of the closing fence (or the last line, if unclosed).
    end: usize,
    info: String,
    body: String,
}

/// One Markdown file of a skill, split into fenced blocks and the prose lines outside them.
struct Doc {
    /// Path shown in failures, relative to the repository root.
    shown: String,
    blocks: Vec<Block>,
    /// (1-based line, text) for lines outside any fence.
    prose: Vec<(usize, String)>,
    lines: Vec<String>,
}

impl Doc {
    fn parse(path: &Path) -> Doc {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let shown = path
            .strip_prefix(root())
            .unwrap_or(path)
            .display()
            .to_string();
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let mut blocks = Vec::new();
        let mut prose = Vec::new();
        let mut open: Option<(char, usize, usize, String, usize, Vec<String>)> = None;
        for (i, raw) in lines.iter().enumerate() {
            let n = i + 1;
            let trimmed = raw.trim_start();
            if let Some((ch, len, start, info, indent, mut body)) = open.take() {
                let run = trimmed.chars().take_while(|c| *c == ch).count();
                if run >= len && trimmed.trim_end().chars().all(|c| c == ch) {
                    blocks.push(Block {
                        line: start,
                        end: n,
                        info,
                        body: body.join("\n"),
                    });
                } else {
                    let drop = raw.len() - trimmed.len();
                    body.push(raw[drop.min(indent)..].to_string());
                    open = Some((ch, len, start, info, indent, body));
                }
                continue;
            }
            let ch = trimmed.chars().next();
            if matches!(ch, Some('`') | Some('~')) {
                let ch = ch.unwrap();
                let run = trimmed.chars().take_while(|c| *c == ch).count();
                if run >= 3 {
                    let info = trimmed[run..].trim().to_string();
                    open = Some((ch, run, n, info, raw.len() - trimmed.len(), Vec::new()));
                    continue;
                }
            }
            prose.push((n, raw.clone()));
        }
        if let Some((_, _, start, info, _, body)) = open {
            blocks.push(Block {
                line: start,
                end: lines.len(),
                info,
                body: body.join("\n"),
            });
        }
        Doc {
            shown,
            blocks,
            prose,
            lines,
        }
    }

    fn at(&self, line: usize) -> String {
        format!("{}:{line}", self.shown)
    }
}

/// Every skill folder under `skills/`, by folder name. Plain files (the README) are not skills.
fn skill_dirs(skills: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(skills) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = read
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn markdown_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = read.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            markdown_under(&p, out);
        } else if p.extension().is_some_and(|e| e == "md") {
            out.push(p);
        }
    }
}

/// `SKILL.md` and every Markdown file under `references/`, for every skill.
fn skill_docs(skills: &Path) -> Vec<Doc> {
    let mut docs = Vec::new();
    for dir in skill_dirs(skills) {
        let mut files = Vec::new();
        if dir.join("SKILL.md").is_file() {
            files.push(dir.join("SKILL.md"));
        }
        markdown_under(&dir.join("references"), &mut files);
        docs.extend(files.iter().map(|f| Doc::parse(f)));
    }
    docs
}

/// The skills the repository ships.
fn real_skills() -> PathBuf {
    root().join("skills")
}

/// Test-only skills that exercise the checker: `good/` must pass, `bad/` must fail as asserted.
fn fixture_skills() -> PathBuf {
    root().join("tests/fixtures/skills-docs")
}

// ---------------------------------------------------------------------------------------------
// Tables and inline code
// ---------------------------------------------------------------------------------------------

/// Split a table row into cells, ignoring `|` inside backticks.
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let (mut cells, mut cur, mut in_code) = (Vec::new(), String::new(), false);
    for c in t.chars() {
        match c {
            '`' => {
                in_code = !in_code;
                cur.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

struct Table {
    header: Vec<String>,
    /// (1-based line, cells)
    rows: Vec<(usize, Vec<String>)>,
}

impl Table {
    fn column(&self, word: &str) -> Option<usize> {
        self.header
            .iter()
            .position(|h| h.to_lowercase().contains(&word.to_lowercase()))
    }
}

fn tables(doc: &Doc) -> Vec<Table> {
    let mut out = Vec::new();
    let mut group: Vec<(usize, &str)> = Vec::new();
    let flush = |group: &mut Vec<(usize, &str)>, out: &mut Vec<Table>| {
        if group.len() >= 2 {
            let sep = split_row(group[1].1);
            let is_sep = !sep.is_empty()
                && sep
                    .iter()
                    .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
            if is_sep {
                out.push(Table {
                    header: split_row(group[0].1),
                    rows: group[2..].iter().map(|(n, l)| (*n, split_row(l))).collect(),
                });
            }
        }
        group.clear();
    };
    let mut last = 0;
    for (n, line) in &doc.prose {
        if line.trim_start().starts_with('|') {
            if last + 1 != *n {
                flush(&mut group, &mut out);
            }
            group.push((*n, line));
            last = *n;
        } else {
            flush(&mut group, &mut out);
        }
    }
    flush(&mut group, &mut out);
    out
}

fn backticked(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`]+)`").unwrap())
        .captures_iter(text)
        .map(|c| c[1].trim().to_string())
        .collect()
}

fn is_identifier(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]*$").unwrap())
        .is_match(s)
}

// ---------------------------------------------------------------------------------------------
// MCP tool references
// ---------------------------------------------------------------------------------------------

struct ToolRef {
    at: String,
    tool: String,
    args: Vec<String>,
}

/// An MCP cell that says no tool does this row's work: `None: <why>`.
fn is_none_cell(cell: &str) -> bool {
    cell.trim_start().starts_with("None:")
}

/// `` `tool` with `arg`, `arg` `` in prose (outside verb tables' MCP cells), up to the end of the
/// clause: the next `.`, `;`, `:`, `|`, `(` or `)`.
fn prose_tool_refs(doc: &Doc) -> Vec<ToolRef> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"`([a-z][a-z0-9_]*)` with ([^.;:|()]*)").unwrap());
    let mut mcp_cells: BTreeMap<usize, usize> = BTreeMap::new();
    for table in tables(doc) {
        if let Some(mcp) = table.column("mcp") {
            for (n, _) in &table.rows {
                mcp_cells.insert(*n, mcp);
            }
        }
    }
    let mut refs = Vec::new();
    for (n, line) in &doc.prose {
        let text = match mcp_cells.get(n) {
            Some(&mcp) => {
                let mut cells = split_row(line);
                if mcp < cells.len() {
                    cells.remove(mcp);
                }
                cells.join(" | ")
            }
            None => line.clone(),
        };
        for c in re.captures_iter(&text) {
            refs.push(ToolRef {
                at: doc.at(*n),
                tool: c[1].to_string(),
                args: backticked(&c[2])
                    .into_iter()
                    .filter(|a| is_identifier(a))
                    .collect(),
            });
        }
    }
    refs
}

fn tool_refs(doc: &Doc, errors: &mut Vec<String>) -> Vec<ToolRef> {
    let mut refs = prose_tool_refs(doc);
    for table in tables(doc) {
        let Some(mcp) = table.column("mcp") else {
            continue;
        };
        for (n, cells) in &table.rows {
            let Some(cell) = cells.get(mcp).filter(|c| !is_none_cell(c)) else {
                continue;
            };
            let names = backticked(cell);
            let Some((tool, rest)) = names.split_first() else {
                continue;
            };
            if !is_identifier(tool) {
                errors.push(format!(
                    "{}: `{tool}` in an MCP column is not a tool name (snake_case expected)",
                    doc.at(*n)
                ));
                continue;
            }
            refs.push(ToolRef {
                at: doc.at(*n),
                tool: tool.clone(),
                args: rest.iter().filter(|a| is_identifier(a)).cloned().collect(),
            });
        }
    }
    for b in &doc.blocks {
        let mut words = b.info.split_whitespace();
        if words.next() != Some("json") {
            continue;
        }
        let tagged = words.find_map(|w| w.strip_prefix("tool=").map(str::to_string));
        let value: Value = match serde_json::from_str(&b.body) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!(
                    "{}: json block does not parse ({e}); give a block that is not JSON another info string",
                    doc.at(b.line)
                ));
                continue;
            }
        };
        let (tool, args) = if let Some(tool) = tagged {
            if !value.is_object() {
                errors.push(format!(
                    "{}: the body of a `json tool={tool}` block is not a JSON object",
                    doc.at(b.line)
                ));
                continue;
            }
            (tool, value.as_object().cloned())
        } else {
            let envelope = value
                .get("params")
                .filter(|p| p.is_object())
                .unwrap_or(&value);
            let Some(name) = envelope.get("name").and_then(Value::as_str) else {
                continue; // no `name` key: not a tool call
            };
            match envelope.get("arguments") {
                None => (name.to_string(), None),
                Some(a) if a.is_object() => (name.to_string(), a.as_object().cloned()),
                Some(_) => {
                    errors.push(format!(
                        "{}: `arguments` of the `{name}` call is not a JSON object",
                        doc.at(b.line)
                    ));
                    continue;
                }
            }
        };
        refs.push(ToolRef {
            at: doc.at(b.line),
            tool,
            args: args
                .map(|a| a.keys().cloned().collect())
                .unwrap_or_default(),
        });
    }
    refs
}

/// tool name -> argument names, from a real `arc mcp` `tools/list`.
fn advertised() -> BTreeMap<String, BTreeSet<String>> {
    let mut child = Command::new(ARC)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        writeln!(stdin, "{request}").unwrap();
    }
    let out = child.wait_with_output().expect("wait for arc mcp");
    assert!(
        out.status.success(),
        "arc mcp failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let response: Value = serde_json::from_str(stdout.lines().next().expect("a response")).unwrap();
    response["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools")
        .iter()
        .map(|t| {
            let props = t["inputSchema"]["properties"]
                .as_object()
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default();
            (t["name"].as_str().unwrap().to_string(), props)
        })
        .collect()
}

fn check_tools(docs: &[Doc]) -> Vec<String> {
    let mut errors = Vec::new();
    let refs: Vec<ToolRef> = docs
        .iter()
        .flat_map(|d| tool_refs(d, &mut errors))
        .collect();
    if !refs.is_empty() {
        let tools = advertised();
        for r in &refs {
            let Some(props) = tools.get(&r.tool) else {
                errors.push(format!(
                    "{}: `{}` is not a tool `arc mcp` advertises",
                    r.at, r.tool
                ));
                continue;
            };
            for a in &r.args {
                if !props.contains(a) {
                    errors.push(format!(
                        "{}: `{a}` is not an argument of `{}` (it takes: {})",
                        r.at,
                        r.tool,
                        props.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
    }
    errors
}

#[test]
fn every_named_tool_is_advertised() {
    let errors = check_tools(&skill_docs(&real_skills()));
    assert!(errors.is_empty(), "\n{}", errors.join("\n"));
}

// ---------------------------------------------------------------------------------------------
// CLI forms
// ---------------------------------------------------------------------------------------------

/// Split a shell line into commands of tokens: quotes and backslashes honoured, and unquoted
/// `&&`, `||`, `|` and `;` ending a command.
fn commands(line: &str) -> Vec<Vec<String>> {
    let (mut out, mut cmd, mut tok) = (Vec::new(), Vec::new(), String::new());
    let mut started = false;
    let mut chars = line.chars().peekable();
    let end_tok = |cmd: &mut Vec<String>, tok: &mut String, started: &mut bool| {
        if *started {
            cmd.push(std::mem::take(tok));
            *started = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    tok.push(q);
                }
            }
            '"' => {
                started = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = chars.next() {
                                tok.push(e);
                            }
                        }
                        _ => tok.push(q),
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(e) = chars.next() {
                    tok.push(e);
                }
            }
            c if c.is_whitespace() => end_tok(&mut cmd, &mut tok, &mut started),
            '&' | '|' | ';' => {
                end_tok(&mut cmd, &mut tok, &mut started);
                if matches!(c, '&' | '|') && chars.peek() == Some(&c) {
                    chars.next();
                }
                if !cmd.is_empty() {
                    out.push(std::mem::take(&mut cmd));
                }
            }
            _ => {
                started = true;
                tok.push(c);
            }
        }
    }
    end_tok(&mut cmd, &mut tok, &mut started);
    if !cmd.is_empty() {
        out.push(cmd);
    }
    out
}

struct CliForm {
    at: String,
    /// Tokens after `arc`.
    tokens: Vec<String>,
}

/// Join backslash continuations of a block body into logical lines, with the line each starts on.
fn logical_lines(b: &Block) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut carry: Option<(usize, String)> = None;
    for (i, raw) in b.body.lines().enumerate() {
        let n = b.line + 1 + i;
        let (start, mut text) = carry.take().unwrap_or((n, String::new()));
        text.push_str(raw.trim_end_matches('\\'));
        if raw.ends_with('\\') {
            text.push(' ');
            carry = Some((start, text));
        } else {
            out.push((start, text));
        }
    }
    if let Some(c) = carry {
        out.push(c);
    }
    out
}

fn cli_forms(doc: &Doc) -> Vec<CliForm> {
    let mut forms = Vec::new();
    let mut from_text = |n: usize, text: &str| {
        let text = text.trim().trim_start_matches("$ ");
        if text.starts_with('#') {
            return;
        }
        for mut cmd in commands(text) {
            // Leading `NAME=value` assignments belong to the command, not the form.
            while cmd.first().is_some_and(|t| {
                t.split_once('=').is_some_and(|(k, _)| {
                    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            }) {
                cmd.remove(0);
            }
            if cmd.first().map(String::as_str) == Some("arc") {
                forms.push(CliForm {
                    at: doc.at(n),
                    tokens: cmd[1..].to_vec(),
                });
            }
        }
    };
    for b in &doc.blocks {
        if b.info.split_whitespace().next() == Some("sh") {
            for (n, l) in logical_lines(b) {
                from_text(n, &l);
            }
        }
    }
    for (n, line) in &doc.prose {
        for span in backticked(line) {
            if span.starts_with("arc ") {
                from_text(*n, &span);
            }
        }
    }
    forms
}

/// `arc ... --help`, run once per argument list.
struct Helps(HashMap<Vec<String>, (bool, String)>);

impl Helps {
    fn get(&mut self, args: &[&str]) -> &(bool, String) {
        let key: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.0.entry(key).or_insert_with(|| {
            let out = Command::new(ARC)
                .args(args)
                .arg("--help")
                .output()
                .expect("spawn arc --help");
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        })
    }
}

/// Subcommand names listed under `Commands:` in clap help.
fn subcommands(help: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in help.lines() {
        if line.starts_with("Commands:") {
            inside = true;
        } else if inside {
            if line.starts_with("  ") && !line.starts_with("   ") {
                if let Some(n) = line.split_whitespace().next() {
                    names.push(n.to_string());
                }
            } else if !line.starts_with(' ') {
                break;
            }
        }
    }
    names
}

fn help_has_flag(help: &str, flag: &str) -> bool {
    let mut from = 0;
    while let Some(i) = help[from..].find(flag) {
        let end = from + i + flag.len();
        let next = help[end..].chars().next();
        if !next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '-') {
            return true;
        }
        from = end;
    }
    false
}

/// A token standing for a verb or subcommand rather than naming one: `<verb>`, `...`, `…`.
fn is_placeholder(token: &str) -> bool {
    token.starts_with('<') || token == "..." || token == "\u{2026}"
}

fn check_cli_form(form: &CliForm, helps: &mut Helps, errors: &mut Vec<String>) {
    let mut i = 0;
    while form.tokens.get(i).is_some_and(|t| t.starts_with('-')) {
        i += 1;
    }
    let mut scope: Vec<String> = Vec::new();
    if let Some(verb) = form.tokens.get(i) {
        if is_placeholder(verb) {
            return;
        }
        let (ok, help) = helps.get(&[verb]).clone();
        if !ok {
            errors.push(format!("{}: `arc {verb}` is not a verb arc has", form.at));
            return;
        }
        scope.push(verb.clone());
        let subs = subcommands(&help);
        if !subs.is_empty()
            && let Some(next) = form.tokens.get(i + 1).filter(|t| !t.starts_with('-'))
        {
            if subs.contains(next) {
                scope.push(next.clone());
            } else if !is_placeholder(next) {
                errors.push(format!(
                    "{}: `arc {verb} {next}` is not a subcommand of `arc {verb}` (it has: {})",
                    form.at,
                    subs.join(", ")
                ));
                return;
            }
        }
        let args: Vec<&str> = scope.iter().map(String::as_str).collect();
        let (ok, _) = helps.get(&args).clone();
        if !ok {
            errors.push(format!(
                "{}: `arc {} --help` fails",
                form.at,
                scope.join(" ")
            ));
            return;
        }
    }
    let args: Vec<&str> = scope.iter().map(String::as_str).collect();
    let own = helps.get(&args).1.clone();
    let root_help = helps.get(&[]).1.clone();
    for t in &form.tokens {
        let Some(rest) = t.strip_prefix("--") else {
            continue;
        };
        let name = rest.split('=').next().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let flag = format!("--{name}");
        if !help_has_flag(&own, &flag) && !help_has_flag(&root_help, &flag) {
            errors.push(format!(
                "{}: `{flag}` is not a flag of `arc {}`",
                form.at,
                scope.join(" ")
            ));
        }
    }
}

fn check_cli(docs: &[Doc]) -> Vec<String> {
    let mut helps = Helps(HashMap::new());
    let mut errors = Vec::new();
    for doc in docs {
        for form in cli_forms(doc) {
            check_cli_form(&form, &mut helps, &mut errors);
        }
    }
    errors
}

#[test]
fn every_cli_form_exists() {
    let errors = check_cli(&skill_docs(&real_skills()));
    assert!(errors.is_empty(), "\n{}", errors.join("\n"));
}

fn check_tables(docs: &[Doc]) -> Vec<String> {
    let mut errors = Vec::new();
    for doc in docs {
        for table in tables(doc) {
            let Some(mcp) = table.column("mcp") else {
                continue;
            };
            let Some(cli) = table.column("cli") else {
                errors.push(format!(
                    "{}: a table with an MCP column needs a CLI column beside it",
                    doc.shown
                ));
                continue;
            };
            for (n, cells) in &table.rows {
                for (col, name) in [(cli, "CLI"), (mcp, "MCP")] {
                    let empty = cells.get(col).is_none_or(|c| {
                        let c = c.trim_matches(|ch: char| {
                            matches!(ch, '-' | '\u{2013}' | '\u{2014}') || ch.is_whitespace()
                        });
                        c.is_empty() || c.eq_ignore_ascii_case("n/a")
                    });
                    if empty {
                        errors.push(format!(
                            "{}: the {name} cell of this row is empty",
                            doc.at(*n)
                        ));
                    }
                }
                if let Some(cell) = cells.get(mcp).map(|c| c.trim())
                    && !cell.is_empty()
                    && !cell.starts_with('`')
                    && !is_none_cell(cell)
                    && !cell.eq_ignore_ascii_case("n/a")
                    && !cell
                        .chars()
                        .all(|ch| matches!(ch, '-' | '\u{2013}' | '\u{2014}'))
                {
                    errors.push(format!(
                        "{}: the MCP cell of this row must begin with a backticked tool, or with `None:` and why no tool does it",
                        doc.at(*n)
                    ));
                }
            }
        }
    }
    errors
}

/// Every `None:` MCP cell, as `file:line: text`: the work the MCP tools cannot yet do.
fn none_cells(docs: &[Doc]) -> Vec<String> {
    let mut out = Vec::new();
    for doc in docs {
        for table in tables(doc) {
            let Some(mcp) = table.column("mcp") else {
                continue;
            };
            for (n, cells) in &table.rows {
                if let Some(cell) = cells.get(mcp).filter(|c| is_none_cell(c)) {
                    out.push(format!("{}: {cell}", doc.at(*n)));
                }
            }
        }
    }
    out
}

#[test]
fn every_verb_table_row_has_both_columns() {
    let docs = skill_docs(&real_skills());
    let gaps = none_cells(&docs);
    println!(
        "{} MCP cells say no tool does the work:\n{}",
        gaps.len(),
        gaps.join("\n")
    );
    let errors = check_tables(&docs);
    assert!(errors.is_empty(), "\n{}", errors.join("\n"));
}

// ---------------------------------------------------------------------------------------------
// Frontmatter
// ---------------------------------------------------------------------------------------------

/// Everything wrong with a `SKILL.md`'s frontmatter, as (line, message).
fn frontmatter_problems(text: &str, folder: &str, version: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return vec![(
            1,
            "SKILL.md must begin with a `---` frontmatter line".into(),
        )];
    }
    let Some(close) = lines[1..].iter().position(|l| l.trim_end() == "---") else {
        return vec![(1, "the frontmatter is never closed with `---`".into())];
    };
    let yaml = lines[1..=close].join("\n");
    let desc_line = lines[1..=close]
        .iter()
        .position(|l| l.starts_with("description:"))
        .map_or(1, |p| p + 2);
    let value: serde_yaml::Value = match serde_yaml::from_str(&yaml) {
        Ok(v) => v,
        Err(e) => {
            return vec![(
                desc_line,
                format!(
                    "frontmatter is not valid YAML ({e}); quote a description that contains a colon"
                ),
            )];
        }
    };
    let mut problems = Vec::new();
    let hyphen_case = Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").unwrap();
    match value["name"].as_str() {
        Some(n) => {
            if n != folder {
                problems.push((
                    1,
                    format!("name `{n}` must equal the folder name `{folder}`"),
                ));
            }
            if !hyphen_case.is_match(n) {
                problems.push((
                    1,
                    format!("name `{n}` must be hyphen-case (lowercase letters, digits and `-`)"),
                ));
            }
            if n.chars().count() > 64 {
                problems.push((
                    1,
                    format!("name is {} characters; the limit is 64", n.chars().count()),
                ));
            }
        }
        None => problems.push((1, "frontmatter has no string `name`".into())),
    }
    match value["description"].as_str() {
        Some(d) if d.trim().is_empty() => problems.push((desc_line, "description is empty".into())),
        Some(d) if d.chars().count() > 1024 => problems.push((
            desc_line,
            format!(
                "description is {} characters; the limit is 1,024",
                d.chars().count()
            ),
        )),
        Some(d) if d.contains(['<', '>']) => problems.push((
            desc_line,
            "description contains `<` or `>`, which skill loaders refuse".into(),
        )),
        Some(_) => {}
        None => problems.push((1, "frontmatter has no string `description`".into())),
    }
    match value["metadata"]["arc_version"].as_str() {
        Some(v) if v == version => {}
        Some(v) => problems.push((
            1,
            format!("metadata.arc_version is `{v}`; this crate is {version}"),
        )),
        None => problems.push((1, "frontmatter has no string `metadata.arc_version`".into())),
    }
    problems
}

fn check_frontmatter(dirs: &[PathBuf], version: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for dir in dirs {
        let folder = dir.file_name().unwrap().to_string_lossy().into_owned();
        let file = dir.join("SKILL.md");
        let shown = file
            .strip_prefix(root())
            .unwrap_or(&file)
            .display()
            .to_string();
        let Ok(text) = std::fs::read_to_string(&file) else {
            errors.push(format!("{shown}:1: the skill has no SKILL.md"));
            continue;
        };
        for (n, msg) in frontmatter_problems(&text, &folder, version) {
            errors.push(format!("{shown}:{n}: {msg}"));
        }
    }
    errors
}

#[test]
fn frontmatter_is_loadable() {
    let errors = check_frontmatter(&skill_dirs(&real_skills()), env!("CARGO_PKG_VERSION"));
    assert!(errors.is_empty(), "\n{}", errors.join("\n"));
}

/// The checker's own rule, tried on text: an unquoted colon is refused, a quoted one is not.
#[test]
fn frontmatter_refuses_an_unquoted_colon() {
    let bad =
        "---\nname: x\ndescription: Use when: asked\nmetadata:\n  arc_version: \"1.0.0\"\n---\n";
    let good = "---\nname: x\ndescription: \"Use when: asked\"\nmetadata:\n  arc_version: \"1.0.0\"\n---\n";
    assert!(!frontmatter_problems(bad, "x", "1.0.0").is_empty());
    assert!(frontmatter_problems(good, "x", "1.0.0").is_empty());
}

/// The checker's own rules for names and angle brackets, tried on text.
#[test]
fn frontmatter_refuses_bad_names_and_angle_brackets() {
    let doc = |name: &str, description: &str| {
        format!(
            "---\nname: {name}\ndescription: \"{description}\"\nmetadata:\n  arc_version: \"1.0.0\"\n---\n"
        )
    };
    let problems = |name: &str, description: &str| {
        frontmatter_problems(&doc(name, description), name, "1.0.0")
            .into_iter()
            .map(|(_, m)| m)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(problems("a-skill-2", "Use it.").is_empty());
    assert!(problems("a_skill", "Use it.").contains("hyphen-case"));
    assert!(problems("A-Skill", "Use it.").contains("hyphen-case"));
    assert!(problems("-a", "Use it.").contains("hyphen-case"));
    let long = "a".repeat(65);
    assert!(problems(&long, "Use it.").contains("the limit is 64"));
    assert!(problems(&"a".repeat(64), "Use it.").is_empty());
    assert!(problems("x", "Writes panels/<name>.yaml").contains("`<` or `>`"));
    assert!(problems("x", "Goes a -> b").contains("`<` or `>`"));
}

// ---------------------------------------------------------------------------------------------
// Examples
// ---------------------------------------------------------------------------------------------

/// The finetype floor, read from `src/finetype_cli.rs` so the two cannot drift.
fn finetype_floor() -> (u64, u64, u64) {
    let src = std::fs::read_to_string(root().join("src/finetype_cli.rs")).unwrap();
    let c = Regex::new(r"MIN_VERSION: \(u64, u64, u64\) = \((\d+), (\d+), (\d+)\)")
        .unwrap()
        .captures(&src)
        .expect("MIN_VERSION in src/finetype_cli.rs");
    (
        c[1].parse().unwrap(),
        c[2].parse().unwrap(),
        c[3].parse().unwrap(),
    )
}

/// Why finetype cannot be used, or None if a recent enough one is on hand.
fn finetype_unavailable() -> Option<String> {
    let bin = std::env::var("FINETYPE_BIN").unwrap_or_else(|_| "finetype".into());
    let out = match Command::new(&bin).arg("--version").output() {
        Ok(o) => o,
        Err(_) => return Some(format!("`{bin}` is not installed")),
    };
    // clap prints the version to stdout; fall back to stderr as `detect_version` does.
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).into_owned()
    } else {
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let token = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().next_back());
    let Some(found) = token.and_then(|t| semver::Version::parse(t.trim_start_matches('v')).ok())
    else {
        return Some(format!(
            "could not parse a finetype version from `{}`",
            text.trim()
        ));
    };
    let floor = finetype_floor();
    if found >= semver::Version::new(floor.0, floor.1, floor.2) {
        None
    } else {
        Some(format!(
            "finetype {found} is older than the floor {}.{}.{}",
            floor.0, floor.1, floor.2
        ))
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn strip_ansi(s: &str) -> String {
    Regex::new("\u{1b}\\[[0-9;]*[A-Za-z]")
        .unwrap()
        .replace_all(s, "")
        .into_owned()
}

fn check_examples(docs: &[Doc]) -> Vec<String> {
    let mut errors = Vec::new();
    let bin = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(ARC, bin.path().join("arc")).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let finetype = std::cell::OnceCell::new();
    for doc in docs {
        for (i, b) in doc.blocks.iter().enumerate() {
            let words: Vec<&str> = b.info.split_whitespace().collect();
            if b.info == "text expect" {
                let follows_example = i > 0
                    && doc.blocks[i - 1].info.starts_with("sh arc-example")
                    && doc.lines[doc.blocks[i - 1].end..b.line - 1]
                        .iter()
                        .all(|l| l.trim().is_empty());
                if !follows_example {
                    errors.push(format!(
                        "{}: a `text expect` block must directly follow a `sh arc-example` block",
                        doc.at(b.line)
                    ));
                }
                continue;
            }
            if words.first() != Some(&"sh") || words.get(1) != Some(&"arc-example") {
                continue;
            }
            let attr = |key: &str| {
                words
                    .iter()
                    .find_map(|w| w.strip_prefix(key).map(str::to_string))
            };
            let fixture = attr("fixture=").unwrap_or_else(|| "skills".into());
            let needs: Vec<String> = attr("needs=")
                .map(|v| v.split(',').map(str::to_string).collect())
                .unwrap_or_default();
            if let Some(odd) = needs
                .iter()
                .find(|n| !matches!(n.as_str(), "finetype" | "finetype-ext"))
            {
                errors.push(format!(
                    "{}: `needs={odd}` is not a need the checker knows (`finetype`, `finetype-ext`)",
                    doc.at(b.line)
                ));
                continue;
            }
            let needs_finetype = needs.iter().any(|n| n == "finetype");
            let needs_ext = needs.iter().any(|n| n == "finetype-ext");
            if needs_ext && std::env::var_os("FINETYPE_DUCKDB_EXT").is_none() {
                assert!(
                    std::env::var_os("ARCFORM_REQUIRE_FINETYPE_EXT").is_none(),
                    "ARCFORM_REQUIRE_FINETYPE_EXT is set, so this run promised the finetype \
                     DuckDB extension, but FINETYPE_DUCKDB_EXT is unset: the example at {} \
                     would be skipped",
                    doc.at(b.line)
                );
                println!(
                    "skipping example at {}: FINETYPE_DUCKDB_EXT is not set",
                    doc.at(b.line)
                );
                continue;
            }
            if needs_finetype && let Some(why) = finetype.get_or_init(finetype_unavailable) {
                println!("skipping example at {}: {why}", doc.at(b.line));
                continue;
            }
            let source = root().join("tests/fixtures").join(&fixture);
            if !source.is_dir() {
                errors.push(format!(
                    "{}: no fixture `tests/fixtures/{fixture}`",
                    doc.at(b.line)
                ));
                continue;
            }
            let work = tempfile::tempdir().unwrap();
            let cwd = work.path().join("protocol");
            copy_dir(&source, &cwd);
            let out = Command::new("sh")
                .arg("-c")
                .arg(format!("set -e\n{}", b.body))
                .current_dir(&cwd)
                .env("PATH", &path)
                .env("ARCFORM_HISTORY_DIR", work.path().join("history"))
                .env("ARCFORM_DB_DIR", work.path().join("db"))
                .env("ARCFORM_FETCH_CACHE", work.path().join("fetch-cache"))
                .env_remove("ARCFORM_VERBOSE")
                .output()
                .expect("spawn sh");
            let combined = strip_ansi(&format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
            if !out.status.success() {
                errors.push(format!(
                    "{}: example exited {:?}:\n{combined}",
                    doc.at(b.line),
                    out.status.code()
                ));
                continue;
            }
            if let Some(next) = doc.blocks.get(i + 1).filter(|n| {
                n.info == "text expect"
                    && doc.lines[b.end..n.line - 1]
                        .iter()
                        .all(|l| l.trim().is_empty())
            }) {
                let want = next.body.trim();
                if !combined.contains(want) {
                    errors.push(format!(
                        "{}: example output lacks the expected text {want:?}; it printed:\n{combined}",
                        doc.at(next.line)
                    ));
                }
            }
        }
    }
    errors
}

#[test]
fn examples_run() {
    let errors = check_examples(&skill_docs(&real_skills()));
    assert!(errors.is_empty(), "\n{}", errors.join("\n"));
}

// ---------------------------------------------------------------------------------------------
// The checker, tried on test-only skills (tests/fixtures/skills-docs/)
// ---------------------------------------------------------------------------------------------

fn fixture_docs(name: &str) -> Vec<Doc> {
    let docs: Vec<Doc> = skill_docs(&fixture_skills())
        .into_iter()
        .filter(|d| d.shown.contains(&format!("/{name}/")))
        .collect();
    assert!(!docs.is_empty(), "no fixture skill `{name}`");
    docs
}

/// 1-based line of the first line of `docs[0]` containing `marker`.
fn line_of(doc: &Doc, marker: &str) -> usize {
    doc.lines
        .iter()
        .position(|l| l.contains(marker))
        .unwrap_or_else(|| panic!("no line with {marker}"))
        + 1
}

fn assert_error_at(errors: &[String], doc: &Doc, marker: &str, fragment: &str) {
    assert_error_on(errors, doc, line_of(doc, marker), fragment);
}

fn assert_error_on(errors: &[String], doc: &Doc, line: usize, fragment: &str) {
    let at = doc.at(line);
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with(&at) && e.contains(fragment)),
        "expected an error at {at} containing {fragment:?}; got:\n{}",
        errors.join("\n")
    );
}

#[test]
fn the_good_fixture_skill_passes_every_check() {
    let docs = fixture_docs("good");
    for (name, errors) in [
        ("tools", check_tools(&docs)),
        ("cli", check_cli(&docs)),
        ("tables", check_tables(&docs)),
        ("examples", check_examples(&docs)),
        (
            "frontmatter",
            check_frontmatter(&[fixture_skills().join("good")], "0.0.0"),
        ),
    ] {
        assert!(errors.is_empty(), "{name}:\n{}", errors.join("\n"));
    }
}

#[test]
fn the_bad_fixture_skill_fails_each_check_at_its_line() {
    let docs = fixture_docs("bad");
    let doc = &docs[0];
    let tools = check_tools(&docs);
    assert_error_at(&tools, doc, "UNKNOWN-TOOL", "`no_such_tool` is not a tool");
    assert_error_at(&tools, doc, "UNKNOWN-ARG", "`bogus_arg` is not an argument");
    let fence = |marker: &str| line_of(doc, marker) - 1;
    assert_error_on(
        &tools,
        doc,
        fence("fake_json_tool"),
        "`fake_json_tool` is not a tool",
    );
    assert_error_on(
        &tools,
        doc,
        fence("\"arguments\":"),
        "json block does not parse",
    );

    assert_error_on(
        &tools,
        doc,
        fence("PARAMS-WRAPPED"),
        "`params_wrapped_tool` is not a tool",
    );
    assert_error_on(
        &tools,
        doc,
        fence("\"bogus_key\""),
        "`bogus_key` is not an argument of `sql_query`",
    );
    assert_error_on(
        &tools,
        doc,
        fence("ARRAY-ARGS"),
        "`arguments` of the `table_describe` call is not a JSON object",
    );
    assert_error_on(&tools, doc, fence("[1, 2]"), "is not a JSON object");

    let cli = check_cli(&docs);
    assert_error_at(
        &cli,
        doc,
        "FOO=bar arc nosuchverb",
        "`arc nosuchverb` is not a verb",
    );
    assert_error_at(
        &cli,
        doc,
        "`arc nosuchverb2`",
        "`arc nosuchverb2` is not a verb",
    );
    assert_error_at(&cli, doc, "arc nosuchverb", "not a verb arc has");
    assert_error_at(&cli, doc, "arc table nosuchsub", "not a subcommand");
    assert_error_at(&cli, doc, "--bogus-flag", "`--bogus-flag` is not a flag");
    // An underscore is not a placeholder: a misspelt verb or subcommand is still refused.
    assert_error_at(
        &cli,
        doc,
        "arc table_describe",
        "`arc table_describe` is not a verb",
    );
    assert_error_at(
        &cli,
        doc,
        "arc sql record_step",
        "`arc sql record_step` is not a subcommand",
    );

    assert_error_at(
        &tools,
        doc,
        "`no_prose_tool` with",
        "`no_prose_tool` is not a tool",
    );
    assert_error_at(
        &tools,
        doc,
        "`prose_bogus_arg`",
        "`prose_bogus_arg` is not an argument of `sql_query`",
    );

    let tables = check_tables(&docs);
    assert_error_at(&tables, doc, "EMPTY-CELL", "MCP cell of this row is empty");
    assert_error_at(
        &tables,
        doc,
        "FREE-TEXT-CELL",
        "must begin with a backticked tool, or with `None:`",
    );

    let frontmatter = check_frontmatter(
        &[
            fixture_skills().join("bad"),
            fixture_skills().join("bad_name"),
        ],
        "0.0.0",
    );
    assert_error_at(&frontmatter, doc, "description:", "`<` or `>`");
    assert!(
        frontmatter.iter().any(|e| e
            .starts_with("tests/fixtures/skills-docs/bad_name/SKILL.md:1:")
            && e.contains("hyphen-case")),
        "{frontmatter:?}"
    );

    // The first command fails and the last succeeds: `set -e` must still fail the example.
    let examples = check_examples(&docs);
    let at = doc.at(line_of(doc, "```sh arc-example"));
    assert!(
        examples
            .iter()
            .any(|e| e.starts_with(&at) && e.contains("example exited")),
        "{examples:?}"
    );
    assert_error_at(
        &examples,
        doc,
        "needs=nonsense",
        "`needs=nonsense` is not a need",
    );
}
