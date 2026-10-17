//! The MCP tools native to `arc` — the ones that make `arc mcp` more than a
//! FineType proxy.
//!
//! - `protocol_run` runs a Protocol and returns its live **Protocol+Run contract**
//!   (`contract_version` `b4/1`): the run's protocol, engine, params, assets and
//!   per-step outcome, the same JSON `arc run` writes under `build/.arcform/runs/`.
//!   A file beside `arcform.yaml` that may be a copy a synced drive set aside is named
//!   in its `run.protocol.possible_copies`, with the warning `arc run` prints for it.
//!   The run is a child `arc run` whose output this process reads, so the server's
//!   standard output carries JSON-RPC alone.
//! - `operator_describe` emits an operator's `with:` JSON Schema (or lists the
//!   catalog) so an agent or authoring UI can build and check a `with:` block.
//! - `operation_describe` lists the SQL operations arc holds, or describes an
//!   operation or an operator by its name: the same JSON `arc operation list --json`
//!   and `arc operation describe` print, both read through the one lookup in
//!   `record`.
//! - `operation_record` records an operation as a new step of a Protocol, through
//!   the same record path `arc operation record` takes, so the same request writes
//!   the same bytes from either.
//! - `operator_record` records an operator step of a Protocol, its `with:` checked
//!   against the operator's schema, through the same record path `arc operator
//!   record` takes, so the same request writes the same bytes from either.
//! - `protocol_edit` makes one edit to a Protocol's spec, through the same write
//!   path `arc edit-protocol` takes, and returns the edited step list.
//! - `history_list` and `history_restore` list the recorded states of a Protocol's
//!   spec, or of one file beside it, and roll it back to one, as `arc history list
//!   --json` and `arc history restore` do.
//! - `sql_recognise` answers what arc reads a SQL statement as: the object `arc sql
//!   recognise` prints, both read through the one reader in `record`.
//! - `sql_record` records a SQL statement as a new step of a Protocol, through the
//!   same record path `arc sql record` takes, so the same statement writes the same
//!   bytes from either.
//! - `table_describe` reads a table in the Protocol's database, read-only, as
//!   `arc table describe` does.
//! - `table_profile` and `table_validate` profile a table with FineType and check
//!   it against a schema file of the Protocol, as `arc table profile` and
//!   `arc table validate` do.
//! - `sql_query` runs SELECT statements against the Protocol's database,
//!   read-only, as `arc sql query` does.
//! - `protocol_create` makes a new Protocol in a directory, through the same
//!   creation `arc create-protocol` takes, so the same request writes the same files
//!   from either.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Annotations, ToolDef, ToolOutput, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// protocol_run
// ─────────────────────────────────────────────────────────────────────────────

fn protocol_run(args: &Value) -> ToolResult {
    let dir = match optional_string(args, "dir")? {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir()
            .map_err(|e| format!("no `dir` given and the current directory is unavailable: {e}"))?,
    };
    let force = match args.get("force") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(force)) => *force,
        Some(other) => return Err(format!("`force` must be a boolean, not {other}")),
    };
    let cli_params = parse_params(args.get("params"))?;

    // The run happens in a child `arc run`, so what the runner, the DuckDB command line,
    // a command step or an operator prints lands in a pipe this process drains, and the
    // server's own standard output carries JSON-RPC alone.
    //
    // The contract is written to `<dir>/build/.arcform/runs/<run_id>.json` at run end
    // (on success and failure alike). Snapshot the directory, run, then read the file
    // the run just added.
    let runs = crate::contract::runs_dir(&dir);
    let before = list_contracts(&runs);
    let child = run_child(&dir, force, &cli_params)
        .map_err(|e| format!("could not start `arc run`: {e}"))?;

    match newest_new_contract(&runs, &before) {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading run contract {}: {e}", path.display()))?;
            let contract: Value = serde_json::from_str(&text)
                .map_err(|e| format!("parsing run contract {}: {e}", path.display()))?;
            Ok(ToolOutput::json(contract))
        }
        None => {
            let mut message = if child.status.success() {
                "the run produced no contract (a protocol with no steps writes none)".to_string()
            } else {
                format!(
                    "run failed before a contract was written: `arc run` exited with {}",
                    child.status
                )
            };
            // The contract carries these in `run.protocol.possible_copies`; with none written,
            // the result an agent receives is the one place left to name them.
            for copy in crate::runner::possible_copies(&dir) {
                message.push_str("\nwarning: ");
                message.push_str(&copy.warning);
            }
            message.push_str("\n--- last output ---\n");
            message.push_str(&child.tail);
            Err(message)
        }
    }
}

/// The most of a child run's output [`run_child`] keeps: its last 4,000 bytes.
const TAIL_BYTES: usize = 4_000;

/// How long [`run_child`] waits, once `arc run` has exited, for the rest of what it
/// printed.
const READER_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// What a child `arc run` left: how it exited and the end of what it printed.
struct ChildRun {
    status: std::process::ExitStatus,
    /// The last [`TAIL_BYTES`] of standard output and standard error, interleaved in
    /// the order they arrived, read as UTF-8 with anything invalid replaced.
    tail: String,
}

/// Run `arc run` on the Protocol in `dir` as a child of this process, with `--force`
/// when `force` is set and one `--param=K=V` for each of `params`.
///
/// Its standard input is closed, and its standard output and standard error are piped
/// and drained on two threads into one buffer that keeps the last [`TAIL_BYTES`], so a
/// child that prints megabytes never blocks on a full pipe and never reaches this
/// process's standard output. Escape sequences are dropped from the tail.
fn run_child(dir: &Path, force: bool, params: &[(String, String)]) -> std::io::Result<ChildRun> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};

    let mut command = Command::new(std::env::current_exe()?);
    command.arg("run");
    if force {
        command.arg("--force");
    }
    for (key, value) in params {
        command.arg(format!("--param={key}={value}"));
    }
    let mut child = command
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let tail = Arc::new(Mutex::new(std::collections::VecDeque::<u8>::with_capacity(
        TAIL_BYTES,
    )));
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let drain = |mut pipe: Box<dyn Read + Send>| {
        let tail = Arc::clone(&tail);
        let done = done_tx.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                        tail.extend(&chunk[..n]);
                        let excess = tail.len().saturating_sub(TAIL_BYTES);
                        tail.drain(..excess);
                    }
                }
            }
            let _ = done.send(());
        })
    };
    let readers = [
        child.stdout.take().map(|p| drain(Box::new(p))),
        child.stderr.take().map(|p| drain(Box::new(p))),
    ];
    let status = child.wait()?;
    // A process the run left behind, such as a step's `sleep 30 &`, inherits the pipes
    // and holds them open after `arc run` exits. Wait a short while for the readers to
    // reach the end of what the run printed, then answer with what has arrived and
    // leave a reader still waiting on such a process to finish on its own.
    let deadline = std::time::Instant::now() + READER_GRACE;
    for _ in readers.iter().flatten() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if done_rx.recv_timeout(left).is_err() {
            break;
        }
    }

    let bytes: Vec<u8> = tail
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .collect();
    Ok(ChildRun {
        status,
        tail: strip_ansi(&String::from_utf8_lossy(&bytes)),
    })
}

/// Drop terminal escape sequences, which arc writes whatever `NO_COLOR` says, so the
/// tail an agent reads is plain text. A control sequence (`ESC [` up to its final
/// byte) is dropped whole, and any other escape with the one character after it.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// Runtime parameter overrides, accepted as an object `{key: value}` or an array of
/// `"KEY=VALUE"` strings.
fn parse_params(value: Option<&Value>) -> std::result::Result<Vec<(String, String)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(map) = value.as_object() {
        return map
            .iter()
            .map(|(key, value)| {
                // Each param reaches the run as `--param KEY=VALUE`, which splits at the
                // first `=`, so a key holding one, or none at all, cannot get there whole.
                if key.is_empty() || key.contains('=') {
                    return Err(format!(
                        "param key `{key}` must be non-empty and hold no `=`"
                    ));
                }
                let value = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                Ok((key.clone(), value))
            })
            .collect();
    }
    if let Some(array) = value.as_array() {
        let mut out = Vec::new();
        for item in array {
            let entry = item
                .as_str()
                .ok_or_else(|| "`params` array items must be \"KEY=VALUE\" strings".to_string())?;
            let (key, value) = entry
                .split_once('=')
                .ok_or_else(|| format!("param `{entry}` must be KEY=VALUE"))?;
            let key = key.trim();
            if key.is_empty() {
                return Err(format!("param `{entry}` has an empty key"));
            }
            out.push((key.to_string(), value.to_string()));
        }
        return Ok(out);
    }
    Err("`params` must be an object {key: value} or an array of \"KEY=VALUE\" strings".to_string())
}

/// The `.json` contract files currently in `runs` (empty if the directory is absent).
fn list_contracts(runs: &Path) -> BTreeSet<PathBuf> {
    let mut set = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(runs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                set.insert(path);
            }
        }
    }
    set
}

/// The newest `.json` contract in `runs` that was not in `exclude` — i.e. the one this
/// run just wrote.
fn newest_new_contract(runs: &Path, exclude: &BTreeSet<PathBuf>) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries = std::fs::read_dir(runs).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") || exclude.contains(&path) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(best_time, _)| modified >= *best_time)
        {
            best = Some((modified, path));
        }
    }
    best.map(|(_, path)| path)
}

fn protocol_run_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." },
            "force": { "type": "boolean", "description": "Re-run every step, ignoring staleness." },
            "params": {
                "type": "object",
                "description": "Runtime parameter overrides, as { key: value }.",
                "additionalProperties": { "type": "string" }
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// operator_describe
// ─────────────────────────────────────────────────────────────────────────────

fn operator_describe(args: &Value) -> ToolResult {
    match args.get("operator").and_then(Value::as_str) {
        None => {
            let operators = crate::operator::catalog_names();
            Ok(ToolOutput::json(json!({
                "operators": operators,
                "hint": "call operator_describe with { \"operator\": \"<name>\" } for its with: JSON Schema",
            })))
        }
        Some(name) => match crate::operator::with_schema(name) {
            Some(schema) => Ok(ToolOutput::json(schema)),
            None => Err(format!(
                "unknown operator `{name}` — call operator_describe with no arguments to list the catalog"
            )),
        },
    }
}

fn operator_describe_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operator": {
                "type": "string",
                "description": "Operator name (e.g. parquet_export). Omit to list every operator in the catalog."
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// operation_describe
// ─────────────────────────────────────────────────────────────────────────────

/// List the operations arc holds, or describe the operation or the operator named
/// by `operation`.
///
/// The answer is the catalogue's own: the listing is what `arc operation list
/// --json` prints, under one key, and a description is what `arc operation
/// describe` prints, found through the same lookup. Nothing here builds a
/// description of its own. The listing holds operations alone. An `operation` that
/// is not a string is refused rather than read as absent, so a client that sends
/// the wrong type is told so and does not get a listing back.
fn operation_describe(args: &Value) -> ToolResult {
    match args.get("operation") {
        None | Some(Value::Null) => {
            let operations: Vec<Value> = crate::record::operations()
                .iter()
                .map(|op| op.listing())
                .collect();
            Ok(ToolOutput::json(json!({ "operations": operations })))
        }
        Some(Value::String(name)) => match crate::record::describe(name) {
            Some(description) => Ok(ToolOutput::json(description)),
            None => Err(format!(
                "no operation or operator called `{name}` — call operation_describe with no arguments to list the operations arc holds"
            )),
        },
        Some(other) => Err(format!(
            "`operation` must be a string, the long name of an operation, not {other} — call operation_describe with no arguments to list the operations arc holds"
        )),
    }
}

fn operation_describe_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "description": "An operation's long name (e.g. filter-rows) or an operator's name (e.g. splink_resolve). Omit to list every operation arc holds."
            }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// operation_record
// ─────────────────────────────────────────────────────────────────────────────

/// Record an operation as a new step at the end of a Protocol.
///
/// The request is handed to the record path `arc operation record` takes, which
/// writes the step's SQL from the operation's catalogue entry; nothing here names
/// an operation or an argument. `dir` defaults to `.`, the server's working
/// directory, as `--dir` does. `arguments` is an object, absent when the
/// operation takes none. `description` is a string, absent for a step with none,
/// as `--description` is. A refusal is an error result naming what was wrong, with
/// the Protocol's directory untouched.
fn operation_record(args: &Value) -> ToolResult {
    let dir = PathBuf::from(optional_string(args, "dir")?.unwrap_or("."));
    let operation = required_string(args, "operation")?;
    let on = required_string(args, "on")?;
    let name = required_string(args, "name")?;
    let arguments = match args.get("arguments") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(other) => {
            return Err(format!(
                "`arguments` must be an object of the operation's arguments, not {other}"
            ));
        }
    };
    let description = match args.get("description") {
        None | Some(Value::Null) => None,
        Some(Value::String(description)) => Some(description.as_str()),
        Some(other) => return Err(format!("`description` must be a string, not {other}")),
    };
    let history = open_history()?;
    let request = crate::record::OperationRequest {
        long_name: operation,
        on,
        name,
        arguments: &arguments,
        description,
    };
    let model =
        crate::record::record_operation(&dir, &request, &history).map_err(|e| e.to_string())?;
    Ok(ToolOutput::json(json!({
        "step": name,
        "model": model.display().to_string(),
    })))
}

/// The local-history store as a tool of `arc mcp` opens it: the conventional
/// root, reached by [`HistoryWay::MCP`](crate::history::HistoryWay::MCP). A tool
/// that writes a version opens the store here, so every version `arc mcp`
/// writes names `mcp` and none can name another.
fn open_history() -> Result<crate::history::LocalHistory, String> {
    crate::history::LocalHistory::open_default()
        .map(|history| history.reached_by(crate::history::HistoryWay::MCP))
        .map_err(|e| e.to_string())
}

/// The string `key` of a tool's arguments, refused by name when absent or not a string.
fn required_string<'a>(args: &'a Value, key: &str) -> std::result::Result<&'a str, String> {
    match args.get(key) {
        Some(Value::String(value)) => Ok(value),
        None | Some(Value::Null) => Err(format!("`{key}` is required")),
        Some(other) => Err(format!("`{key}` must be a string, not {other}")),
    }
}

/// The string `key` of a tool's arguments, `None` when absent or null, refused by
/// name when it is anything other than a string — so a mistyped `dir` is an error,
/// never the server's working directory.
fn optional_string<'a>(args: &'a Value, key: &str) -> std::result::Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(other) => Err(format!("`{key}` must be a string, not {other}")),
    }
}

fn operation_record_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operation": { "type": "string", "description": "The operation's long name (e.g. filter-rows), as operation_describe lists them." },
            "on": { "type": "string", "description": "The table the operation is applied to: one a step of the Protocol makes." },
            "name": { "type": "string", "description": "The new step's name, which is also the name of the table it makes." },
            "arguments": { "type": "object", "description": "The operation's arguments, as the `parameters` JSON Schema operation_describe returns for it describes them." },
            "description": { "type": "string", "description": "One line saying what the step does and why, written on the step for the person who reads the Protocol next." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["operation", "on", "name"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// operator_record
// ─────────────────────────────────────────────────────────────────────────────

/// Record an operator step at the end of a Protocol.
///
/// The request is handed to the record path `arc operator record` takes, which
/// checks `with` against the operator's schema before it writes the step. `dir`
/// defaults to `.`, the server's working directory, as `--dir` does, and
/// `description` is absent for a step with none. A refusal is an error result
/// naming what was wrong, with the Protocol's directory untouched.
fn operator_record(args: &Value) -> ToolResult {
    let dir = dir_arg(args)?;
    let operator = required_string(args, "operator")?;
    let name = required_string(args, "name")?;
    let with = match args.get("with") {
        Some(with @ Value::Object(_)) => with,
        None | Some(Value::Null) => return Err("`with` is required".to_string()),
        Some(other) => {
            return Err(format!(
                "`with` must be an object of the operator's `with:` block, not {other}"
            ));
        }
    };
    let description = match args.get("description") {
        None | Some(Value::Null) => None,
        Some(Value::String(description)) => Some(description.as_str()),
        Some(other) => return Err(format!("`description` must be a string, not {other}")),
    };
    let history = open_history()?;
    let request = crate::record::OperatorRequest {
        operator,
        name,
        with,
        description,
    };
    crate::record::record_operator(&dir, &request, &history).map_err(|e| e.to_string())?;
    Ok(ToolOutput::json(json!({
        "step": name,
        "operator": operator,
    })))
}

fn operator_record_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "operator": { "type": "string", "description": "The operator's name (e.g. http_fetch), as operator_describe lists them." },
            "name": { "type": "string", "description": "The new step's name." },
            "with": { "type": "object", "description": "The step's `with:` block, as the JSON Schema operator_describe returns for the operator describes it." },
            "description": { "type": "string", "description": "One line saying what the step does and why, written on the step for the person who reads the Protocol next." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["operator", "name", "with"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// protocol_edit
// ─────────────────────────────────────────────────────────────────────────────

/// The string `key` of an edit's arguments, refused by name when absent.
fn edit_string(args: &Value, key: &str) -> std::result::Result<String, String> {
    required_string(args, key).map(str::to_string)
}

/// The index `key` of an edit's arguments, refused by name when absent or not a
/// non-negative whole number.
fn edit_index(args: &Value, key: &str) -> std::result::Result<usize, String> {
    match args.get(key) {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| format!("`{key}` must be a non-negative whole number, not {n}")),
        None | Some(Value::Null) => Err(format!("`{key}` is required")),
        Some(other) => Err(format!(
            "`{key}` must be a non-negative whole number, not {other}"
        )),
    }
}

/// Make one edit to a Protocol's spec as `arc edit-protocol` does: the arguments
/// name the same edit and carry the same fields as the command's, and are handed
/// to the one write path, which applies the edit to the file's bytes, validates
/// the result and writes it atomically, checkpointing the state it replaces. The
/// result is the edited Protocol's step list. A refusal is an error result naming
/// what was wrong, with the Protocol's directory untouched.
fn protocol_edit(args: &Value) -> ToolResult {
    use crate::cli::EditOp;
    let dir = dir_arg(args)?;
    let path = edit_string(args, "path")?;
    let op = match required_string(args, "edit")? {
        "replace" => EditOp::Replace {
            path,
            value: edit_string(args, "value")?,
        },
        "rewrite" => EditOp::Rewrite {
            path,
            from: edit_string(args, "from")?,
            to: edit_string(args, "to")?,
        },
        "add" => EditOp::Add {
            path,
            key: edit_string(args, "key")?,
            value: edit_string(args, "value")?,
        },
        "append" => EditOp::Append {
            path,
            item: edit_string(args, "item")?,
        },
        "delete" => EditOp::Delete { path },
        "reorder" => EditOp::Reorder {
            path,
            from: edit_index(args, "from")?,
            to: edit_index(args, "to")?,
        },
        other => {
            return Err(format!(
                "`edit` must be one of replace, rewrite, add, append, delete, reorder, not `{other}`"
            ));
        }
    };
    let history = open_history()?;
    crate::cli::edit_protocol_files(&dir, op, &history).map_err(|e| e.to_string())?;
    let manifest = crate::manifest::Manifest::load(&dir).map_err(|e| e.to_string())?;
    let steps = serde_json::to_value(&manifest.steps).map_err(|e| e.to_string())?;
    Ok(ToolOutput::json(steps))
}

fn protocol_edit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "edit": { "type": "string", "enum": ["replace", "rewrite", "add", "append", "delete", "reorder"], "description": "Which edit to make." },
            "path": { "type": "string", "description": "Where in arcform.yaml: dot-separated mapping keys with zero-based [N] sequence indices, such as `steps[2].command` or `params.port.default`; `.` alone is the manifest root (only `add` takes it)." },
            "value": { "type": "string", "description": "replace and add: the new value, spliced verbatim; a multi-line value arrives already indented for where it lands." },
            "from": { "description": "rewrite: the text to change, which must occur exactly once in the value at `path`. reorder: the index the item is at (a whole number)." },
            "to": { "description": "rewrite: the text that replaces it. reorder: the index the item should end up at (a whole number)." },
            "key": { "type": "string", "description": "add: the key to add to the mapping at `path`." },
            "item": { "type": "string", "description": "append: the item to append to the sequence at `path`; for a block sequence it carries its own pre-indented `- ` line(s)." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["edit", "path"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// history_list, history_restore
// ─────────────────────────────────────────────────────────────────────────────

/// The file an optional `file` argument names, read from `dir` when relative, as
/// `--file` is.
fn optional_file(args: &Value) -> std::result::Result<Option<PathBuf>, String> {
    Ok(optional_string(args, "file")?.map(PathBuf::from))
}

/// The recorded states of a Protocol's spec, or given `file` of that one file,
/// oldest first: the array `arc history list --json` prints. Reads only.
fn history_list(args: &Value) -> ToolResult {
    let dir = dir_arg(args)?;
    let file = optional_file(args)?;
    let history = open_history()?;
    crate::cli::history_entries_json(&dir, file.as_deref(), &history)
        .map(ToolOutput::json)
        .map_err(|e| e.to_string())
}

fn history_list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file": { "type": "string", "description": "List this file's own history instead of the spec's, such as a chart file beside the spec. The path is relative to the Protocol folder (`dir`) and must stay inside it; absolute paths and `..` are refused." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        }
    })
}

/// Roll a Protocol's spec, or given `file` that one file, back to the state an
/// entry recorded, as `arc history restore` does: the state being replaced is
/// checkpointed first, so a restore can itself be undone. When the restored spec
/// does not load, `note` says why. An unknown entry is an error result, with
/// nothing written.
fn history_restore(args: &Value) -> ToolResult {
    let dir = dir_arg(args)?;
    let id = required_string(args, "id")?;
    let file = optional_file(args)?;
    let history = open_history()?;
    let restored = crate::cli::history_restore_files(&dir, file.as_deref(), id, &history)
        .map_err(|e| e.to_string())?;
    let mut result = json!({
        "restored": id,
        // Relative to the Protocol, as protocol_write_file and report_write give theirs.
        "file": file
            .clone()
            .unwrap_or_else(|| PathBuf::from(crate::spec::MANIFEST_FILENAME))
            .display()
            .to_string(),
    });
    if let Some(fault) = restored.load_fault {
        result["note"] = json!(format!(
            "the restored state does not load as a spec: {fault}"
        ));
    }
    Ok(ToolOutput::json(result))
}

fn history_restore_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "The entry's id, as history_list returns it for the same file." },
            "file": { "type": "string", "description": "Restore this file from its own history instead of the spec; no other file is written. The path is relative to the Protocol folder (`dir`) and must stay inside it; absolute paths and `..` are refused." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["id"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// sql_recognise
// ─────────────────────────────────────────────────────────────────────────────

/// What arc reads one SQL statement as: the object `arc sql recognise` prints,
/// read through the same reader. Writes no file and runs nothing. Text DuckDB
/// cannot parse as one statement is an error result naming what was wrong.
fn sql_recognise(args: &Value) -> ToolResult {
    let sql = required_string(args, "sql")?;
    crate::record::recognise(sql)
        .map(ToolOutput::json)
        .map_err(|e| e.to_string())
}

fn sql_recognise_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sql": { "type": "string", "description": "One SQL statement, as typed." }
        },
        "required": ["sql"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// sql_record
// ─────────────────────────────────────────────────────────────────────────────

/// Record a SQL statement as a new step at the end of a Protocol.
///
/// The request is handed to the record path `arc sql record` takes, which reads
/// the statement as `sql_recognise` does and writes the step; nothing here names
/// an operation. `dir` defaults to `.`, the server's working directory, as
/// `--dir` does, and `description` is absent for a step with none. The result
/// names the step, the model, and under `operation` the long name of the
/// operation the statement was recorded as, `null` for a SQL step, with the
/// table it was applied to under `on`. A refusal is an error result naming what
/// was wrong, with the Protocol's directory untouched.
fn sql_record(args: &Value) -> ToolResult {
    let dir = PathBuf::from(optional_string(args, "dir")?.unwrap_or("."));
    let sql = required_string(args, "sql")?;
    let name = required_string(args, "name")?;
    let description = match args.get("description") {
        None | Some(Value::Null) => None,
        Some(Value::String(description)) => Some(description.as_str()),
        Some(other) => return Err(format!("`description` must be a string, not {other}")),
    };
    let history = open_history()?;
    let request = crate::record::SqlRequest {
        sql,
        name,
        description,
    };
    let recorded =
        crate::record::record_sql(&dir, &request, &history).map_err(|e| e.to_string())?;
    let (operation, on) = match recorded.operation {
        Some((operation, on)) => (json!(operation), json!(on)),
        None => (Value::Null, Value::Null),
    };
    Ok(ToolOutput::json(json!({
        "step": name,
        "model": recorded.model.display().to_string(),
        "operation": operation,
        "on": on,
    })))
}

fn sql_record_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sql": { "type": "string", "description": "One SQL statement, as typed." },
            "name": { "type": "string", "description": "The new step's name, which is also the name of the table it makes when the statement is a SELECT or a filter." },
            "description": { "type": "string", "description": "One line saying what the step does and why, written on the step for the person who reads the Protocol next." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["sql", "name"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// protocol_create
// ─────────────────────────────────────────────────────────────────────────────

/// Make a new Protocol in `dir` as `arc create-protocol` does: `arcform.yaml`, the
/// ignore list and the log's merge rule, through the one creation both share. An
/// existing spec is refused and left as it was. `files_written` lists only the files
/// written or added to; the tool prints nothing, since standard output is JSON-RPC.
fn protocol_create(args: &Value) -> ToolResult {
    let dir = PathBuf::from(required_string(args, "dir")?);
    let optional = |key: &str| -> std::result::Result<Option<String>, String> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(other) => Err(format!("`{key}` must be a string, not {other}")),
        }
    };
    let (name, engine, db) = (optional("name")?, optional("engine")?, optional("db")?);
    let history = open_history()?;
    let created = crate::cli::create_protocol_files(&dir, name, engine, db, &history)
        .map_err(|e| e.to_string().replace("pass --name", "pass `name`"))?;
    let mut files_written = vec![crate::spec::MANIFEST_FILENAME];
    if created.ignore == crate::edit::IgnoreList::Written {
        files_written.push(crate::edit::IGNORE_FILENAME);
    }
    if created.merge_rule != crate::edit::MergeRule::Kept {
        files_written.push(crate::edit::ATTRIBUTES_FILENAME);
    }
    Ok(ToolOutput::json(json!({
        "manifest": created.manifest.display().to_string(),
        "files_written": files_written,
    })))
}

fn protocol_create_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dir": { "type": "string", "description": "The directory to make the Protocol in; arcform.yaml is written there. Made if absent." },
            "name": { "type": "string", "description": "The Protocol's name. Defaults to the directory's name." },
            "engine": { "type": "string", "description": "The SQL engine the Protocol targets. Defaults to duckdb." },
            "db": { "type": "string", "description": "The Protocol's working database file, relative to the directory. Defaults to none." }
        },
        "required": ["dir"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// protocol_write_file
// ─────────────────────────────────────────────────────────────────────────────

/// Write a schema, panel or README file of a Protocol, as `arc protocol
/// write-file` does: both call `write_protocol_file`. `dir` defaults to `.`.
/// The result names the file's `path`, its `kind` and any `warnings`; a refusal
/// is an error result with the folder untouched.
fn protocol_write_file(args: &Value) -> ToolResult {
    let dir = dir_arg(args)?;
    let path = required_string(args, "path")?;
    let content = required_string(args, "content")?;
    let history = open_history()?;
    let written =
        crate::protocol_files::write_protocol_file(&dir, Path::new(path), content, &history)
            .map_err(|e| e.to_string())?;
    Ok(ToolOutput::json(json!({
        "path": written.path.display().to_string(),
        "kind": written.kind.word(),
        "warnings": written.warnings,
    })))
}

fn protocol_write_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "The file's path relative to the Protocol directory: schemas/<name>.json, panels/<name>.yaml or README.md." },
            "content": { "type": "string", "description": "The whole file. A schema must be JSON and a panel YAML with a `plot` key." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["path", "content"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// report_write
// ─────────────────────────────────────────────────────────────────────────────

/// Write the report of a Protocol's run, as `arc report write` does: both call
/// `write_report`. `dir` defaults to `.`. The result names the report's `path`.
fn report_write(args: &Value) -> ToolResult {
    let dir = dir_arg(args)?;
    let prose = required_string(args, "prose")?;
    let run = optional_string(args, "run")?;
    let history = open_history()?;
    let path =
        crate::report::write_report(&dir, prose, run, &history).map_err(|e| e.to_string())?;
    Ok(ToolOutput::json(
        json!({ "path": path.display().to_string() }),
    ))
}

fn report_write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prose": { "type": "string", "description": "Your part of the report: the answer, its caveats and the next steps. arc adds the sections it knows from the run." },
            "run": { "type": "string", "description": "The run whose contract to use. Defaults to the newest run." },
            "dir": { "type": "string", "description": "Protocol directory (where arcform.yaml lives). Defaults to the current directory." }
        },
        "required": ["prose"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// table_describe
// ─────────────────────────────────────────────────────────────────────────────

/// What a table in the Protocol's database holds, as `arc table describe` prints it.
/// The database is opened read-only; the answer is the verb's JSON.
fn table_describe(args: &Value) -> ToolResult {
    let table = required_string(args, "table")?;
    let dir = match args.get("dir") {
        None | Some(Value::Null) => PathBuf::from("."),
        Some(Value::String(dir)) => PathBuf::from(dir),
        Some(other) => return Err(format!("`dir` must be a string, not {other}")),
    };
    crate::inspect::describe_table(&dir, table)
        .map(ToolOutput::json)
        .map_err(|e| e.to_string())
}

fn table_describe_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "table": { "type": "string", "description": "The table or view to describe: a bare name, or schema.table." },
            "dir": { "type": "string", "description": "The Protocol's directory. Defaults to the server's working directory." }
        },
        "required": ["table"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// panel_check
// ─────────────────────────────────────────────────────────────────────────────

/// What each panel of a Protocol names that the Protocol does not make, as `arc
/// panel check` prints it. A panel with findings is a result, not an error.
fn panel_check(args: &Value) -> ToolResult {
    let dir = dir_arg(args)?;
    let checked = crate::panel_check::check_panels(&dir)
        .map_err(|e| e.to_string())?
        .1;
    let panels: Vec<Value> = checked
        .iter()
        .map(|(panel, findings)| {
            json!({
                "panel": panel.display().to_string(),
                "findings": findings.iter().map(ToString::to_string).collect::<Vec<_>>(),
            })
        })
        .collect();
    let clean = checked.iter().all(|(_, findings)| findings.is_empty());
    Ok(ToolOutput::json(
        json!({ "clean": clean, "panels": panels }),
    ))
}

fn panel_check_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "dir": { "type": "string", "description": "The Protocol's directory. Defaults to the server's working directory." }
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// table_profile and table_validate
// ─────────────────────────────────────────────────────────────────────────────

fn dir_arg(args: &Value) -> std::result::Result<PathBuf, String> {
    match args.get("dir") {
        None | Some(Value::Null) => Ok(PathBuf::from(".")),
        Some(Value::String(dir)) => Ok(PathBuf::from(dir)),
        Some(other) => Err(format!("`dir` must be a string, not {other}")),
    }
}

/// FineType's profile of a table in the Protocol's database, as `arc table profile`
/// prints it.
fn table_profile(args: &Value) -> ToolResult {
    let table = required_string(args, "table")?;
    let dir = dir_arg(args)?;
    let format = match args.get("format") {
        None | Some(Value::Null) => crate::inspect::ProfileFormat::Json,
        Some(Value::String(f)) if f == "json" => crate::inspect::ProfileFormat::Json,
        Some(Value::String(f)) if f == "json-schema" => crate::inspect::ProfileFormat::JsonSchema,
        Some(other) => {
            return Err(format!(
                "`format` must be \"json\" or \"json-schema\", not {other}"
            ));
        }
    };
    let sample_size = match args.get("sample_size") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| format!("`sample_size` must be a whole number, not {value}"))?,
        ),
    };
    crate::inspect::profile_table(&dir, table, format, sample_size)
        .map(ToolOutput::json)
        .map_err(|e| e.to_string())
}

fn table_profile_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "table": { "type": "string", "description": "The table or view to profile: a bare name, or schema.table." },
            "format": { "type": "string", "enum": ["json", "json-schema"], "default": "json", "description": "json gives each column's type; json-schema gives a JSON Schema the rows pass, which protocol_write_file can save as schemas/<name>.json." },
            "sample_size": { "type": "integer", "minimum": 1, "description": "The most rows to copy and profile. Defaults to FineType's 100." },
            "dir": { "type": "string", "description": "The Protocol's directory. Defaults to the server's working directory." }
        },
        "required": ["table"]
    })
}

/// A table in the Protocol's database checked against a schema file of the
/// Protocol, as `arc table validate` prints it. Rejected rows are an answer
/// (`valid: false`), not an error.
fn table_validate(args: &Value) -> ToolResult {
    let table = required_string(args, "table")?;
    let schema = required_string(args, "schema")?;
    let dir = dir_arg(args)?;
    crate::inspect::validate_table(&dir, table, std::path::Path::new(schema))
        .map(ToolOutput::json)
        .map_err(|e| e.to_string())
}

fn table_validate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "table": { "type": "string", "description": "The table or view to validate: a bare name, or schema.table." },
            "schema": { "type": "string", "description": "The JSON Schema file, relative to the Protocol's directory and inside it (e.g. schemas/orders.json)." },
            "dir": { "type": "string", "description": "The Protocol's directory. Defaults to the server's working directory." }
        },
        "required": ["table", "schema"]
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// sql_query
// ─────────────────────────────────────────────────────────────────────────────

/// The rows SELECT statements return from the Protocol's database, as `arc sql
/// query` prints them. The database is opened read-only, and a text holding
/// anything but SELECTs is refused before any of it runs.
fn sql_query(args: &Value) -> ToolResult {
    let sql = required_string(args, "sql")?;
    let dir = match args.get("dir") {
        None | Some(Value::Null) => PathBuf::from("."),
        Some(Value::String(dir)) => PathBuf::from(dir),
        Some(other) => return Err(format!("`dir` must be a string, not {other}")),
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => 100,
        Some(value) => value
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| format!("`limit` must be a whole number, not {value}"))?,
    };
    crate::inspect::query(&dir, sql, limit)
        .map(ToolOutput::json)
        .map_err(|e| e.to_string())
}

fn sql_query_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sql": { "type": "string", "description": "One or more SELECT statements, separated by `;`." },
            "dir": { "type": "string", "description": "The Protocol's directory. Defaults to the server's working directory." },
            "limit": { "type": "integer", "minimum": 0, "maximum": 100000, "description": "The most rows returned for each statement. Defaults to 100; at most 100000." }
        },
        "required": ["sql"]
    })
}

/// The tools native to `arc`.
pub(super) fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "protocol_run",
            description: "Run an arc Protocol and return its live Protocol+Run contract (protocol, engine, params, assets, per-step outcome). A file beside arcform.yaml that may be a copy a synced drive set aside, which the run did not read, is named with its warning in run.protocol.possible_copies.",
            input_schema: protocol_run_schema,
            handler: protocol_run,
            annotations: Annotations::RUNS,
        },
        ToolDef {
            name: "operator_describe",
            description: "Emit an operator's `with:` JSON Schema for authoring — or list the operator catalog when called with no operator.",
            input_schema: operator_describe_schema,
            handler: operator_describe,
            annotations: Annotations::READ_ONLY,
        },
        ToolDef {
            name: "operation_describe",
            description: "List the SQL operations arc holds when called with no operation — or describe one operation or operator (what it does, what its step reads and writes by role, and the JSON Schema of what it takes) by its name.",
            input_schema: operation_describe_schema,
            handler: operation_describe,
            annotations: Annotations::READ_ONLY,
        },
        ToolDef {
            name: "operation_record",
            description: "Record an SQL operation as a new step at the end of a Protocol, from its long name, the table it is applied to, the step's name and its arguments. Writes a generated model and the step naming it; runs nothing.",
            input_schema: operation_record_schema,
            handler: operation_record,
            annotations: Annotations::WRITES,
        },
        ToolDef {
            name: "operator_record",
            description: "Record an operator step (a fetch, an entity resolution, an embedding, an export) as a new step at the end of a Protocol, from the operator's name, the step's name and its `with:` block. The `with:` is checked against the operator's schema, the one operator_describe returns, and a block that does not fit is refused naming the field, with the Protocol untouched. Writes the step; runs nothing.",
            input_schema: operator_record_schema,
            handler: operator_record,
            annotations: Annotations::WRITES,
        },
        ToolDef {
            name: "protocol_edit",
            description: "Make one edit to a Protocol's arcform.yaml, as `arc edit-protocol` does: replace a value, rewrite one fragment of it, add a key, append a sequence item, delete an element or reorder a sequence, addressed by a path such as `steps[2].command`. The edit is applied to the file's bytes, so every byte it does not target survives (comments, key order, quoting), and is checked by the loader before anything is written; one that does not apply or does not load is refused with the file untouched. The state replaced is kept in history. Returns the edited Protocol's steps. Runs nothing.",
            input_schema: protocol_edit_schema,
            handler: protocol_edit,
            annotations: Annotations::EDITS,
        },
        ToolDef {
            name: "history_list",
            description: "List the recorded states of a Protocol's arcform.yaml, or with `file` of one file beside it, oldest first: each entry's `id`, `kind` (save, checkpoint or unsaved), `at`, `bytes` and `way` arc was reached by. The ids are what history_restore takes. Changes nothing.",
            input_schema: history_list_schema,
            handler: history_list,
            annotations: Annotations::READ_ONLY,
        },
        ToolDef {
            name: "history_restore",
            description: "Roll a Protocol's arcform.yaml, or with `file` that one file alone, back to the state a history_list entry recorded. The state being replaced is kept in history first, so a restore can itself be undone. A restored spec that no longer loads is restored all the same and says so in `note`.",
            input_schema: history_restore_schema,
            handler: history_restore,
            annotations: Annotations::EDITS,
        },
        ToolDef {
            name: "sql_recognise",
            description: "Answer what arc reads one SQL statement as: the operation arc holds that it is, with `operation`, `on` and `arguments` as operation_record takes them, or `operation` null when it is none. Read from DuckDB's own parse; writes no file and runs nothing.",
            input_schema: sql_recognise_schema,
            handler: sql_recognise,
            annotations: Annotations::READ_ONLY,
        },
        ToolDef {
            name: "sql_record",
            description: "Record one SQL statement as a new step at the end of a Protocol. A statement sql_recognise reads as an operation is recorded as the step operation_record writes for it; any other statement DuckDB parses is recorded as a SQL step, whose model has no `-- generated:` line, so arc does not rewrite it: for a SELECT the model's first line is `CREATE OR REPLACE TABLE \"<name>\" AS` and the statement as typed follows it, so the step makes a table of its name when it runs. Writes the step; runs nothing.",
            input_schema: sql_record_schema,
            handler: sql_record,
            annotations: Annotations::WRITES,
        },
        ToolDef {
            name: "protocol_create",
            description: "Make a new Protocol in a directory, as `arc create-protocol` does: writes arcform.yaml with no steps, and the .gitignore and .gitattributes beside it. Refuses a directory that already holds an arcform.yaml and leaves it as it was. Runs nothing.",
            input_schema: protocol_create_schema,
            handler: protocol_create,
            annotations: Annotations::WRITES,
        },
        ToolDef {
            name: "protocol_write_file",
            description: "Write one file of a Protocol, as `arc protocol write-file` does: a table's schema (schemas/<name>.json, JSON), a panel (panels/<name>.yaml, YAML with a `plot` key) or README.md. The file is written whole, replacing one already there, and the state it replaces is kept in its history. A path outside the Protocol's folder, any other place and content that does not read as its kind are refused with nothing written; report.md has a tool of its own, report_write. Runs nothing.",
            input_schema: protocol_write_file_schema,
            handler: protocol_write_file,
            annotations: Annotations::EDITS,
        },
        ToolDef {
            name: "report_write",
            description: "Write report.md beside a Protocol, as `arc report write` does: your `prose` (the answer, its caveats and the next steps) first, then three sections arc writes from the run's contract. \"How this was derived\" lists each step in order with its description, status and the tables it made with their row counts; \"Charts\" lists each panel with its title and anything panel_check would say of it; \"Run\" gives the run id, when it finished, its outcome and each asset's content hash. Uses the newest run unless `run` names one; arc's sections follow your prose, so leave your own headings out of it. The panel findings are checked against the Protocol's current database, not the run's (or, before it has one, against the tables its steps make), and a database that cannot be opened is noted in the section instead of stopping the report. Replaces a report already there, and the state it replaces is kept in its history. A Protocol that has not run, or a `run` with no contract, is refused with nothing written: run the Protocol first with protocol_run. Runs nothing.",
            input_schema: report_write_schema,
            handler: report_write,
            annotations: Annotations::EDITS,
        },
        ToolDef {
            name: "panel_check",
            description: "Check each panel (panels/<name>.yaml) of a Protocol against what the Protocol makes, as `arc panel check` does. A mark's `data.from` must be a key of the panel's own `data:` or a table in the Protocol's database, and a channel written as a bare string (`x: name`) must be a column of that table; marks nested under `hconcat`/`vconcat` are checked too. Not checked: object-valued channels (expressions and aggregates), a `data:` that is a bare string, and three-part `catalog.schema.table` names. A file that is not YAML or holds no `plot` is reported as a finding. Returns `clean` and, for each panel, its `findings` as plain sentences. Before the Protocol has run there is no database, so a table is checked against the relations its steps produce and columns are not checked. Opens the database read-only. protocol_write_file reports the same findings as `warnings` when it writes a panel.",
            input_schema: panel_check_schema,
            handler: panel_check,
            annotations: Annotations::READS_OPEN,
        },
        ToolDef {
            name: "table_describe",
            description: "Describe a table or view in a Protocol's database: its row count, each column's name, type and null count, and up to five rows as arrays of values in the order of the columns. A bare name is the table in main, or the one schema that holds it; `schema.table` names another. Opens the database read-only and never writes it; a view may read the files and URLs it names, but no extension is installed for it. A database the Protocol has not made yet is refused, and so is one another process holds.",
            input_schema: table_describe_schema,
            handler: table_describe,
            annotations: Annotations::READS_OPEN,
        },
        ToolDef {
            name: "table_profile",
            description: "Profile a table or view in a Protocol's database with FineType: the type of each column (`format` json), or a JSON Schema its rows pass (`format` json-schema, which protocol_write_file can save as schemas/<name>.json). Up to `sample_size` rows (100 by default) are copied to a temporary CSV outside the Protocol and `finetype profile` is run over it; for `json`, `table` and `rows_profiled` are added to what FineType gives, and `json-schema` returns FineType's schema with an `$id` naming the table. A table with no rows says so for `json` and is refused for `json-schema`, which has nothing to infer a schema from. Opens the database read-only; needs finetype on PATH at the version arc requires.",
            input_schema: table_profile_schema,
            handler: table_profile,
            annotations: Annotations::READS_OPEN,
        },
        ToolDef {
            name: "table_validate",
            description: "Validate a table or view in a Protocol's database against a JSON Schema file of the Protocol, in DuckDB with FineType's extension (the FINETYPE_DUCKDB_EXT environment variable names it; without it the call is refused). Returns `valid` and, for each column, `total`, `rejects` and a `sample_message`; rejected rows give `valid` false, not an error. A schema outside the Protocol's folder is refused. Opens the database read-only.",
            input_schema: table_validate_schema,
            handler: table_validate,
            annotations: Annotations::READS_OPEN,
        },
        ToolDef {
            name: "sql_query",
            description: "Run SELECT statements against a Protocol's database and return, for each, its columns and types and up to `limit` rows (100 by default, at most 100000) as arrays of values in the order of the columns, with `capped` and `rows_held_back` saying when more rows were there; `rows_held_back` is a recount, running the statement a second time. Before anything runs, DuckDB reads the whole text, and a text holding anything but SELECTs is refused whole. A statement that fails while running gives `{statement, error}` in its place and the others are kept. The SQL may read files and URLs it names; the database is opened read-only and never written. `keep` says how to record a query as a step: the `arc sql record` command, or the sql_record tool.",
            input_schema: sql_query_schema,
            handler: sql_query,
            annotations: Annotations::READS_OPEN,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_describe_lists_the_catalog() {
        let result = operator_describe(&json!({})).expect("listing succeeds");
        let operators = result.structured.as_ref().unwrap()["operators"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(operators.contains(&"parquet_export".to_string()));
        assert!(operators.contains(&"finetype_validate".to_string()));
    }

    #[test]
    fn operator_describe_emits_a_with_schema() {
        let result =
            operator_describe(&json!({ "operator": "http_fetch" })).expect("describe succeeds");
        let schema = result.structured.expect("a structured schema");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["url"].is_object());
        assert!(schema["properties"]["out"].is_object());
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(required.contains(&"url") && required.contains(&"out"));
    }

    #[test]
    fn operator_describe_unknown_operator_errors() {
        let err = operator_describe(&json!({ "operator": "nope" })).unwrap_err();
        assert!(err.contains("unknown operator"), "message: {err}");
    }

    #[test]
    fn operation_describe_lists_each_operation_under_one_key() {
        for args in [json!({}), json!({ "operation": null })] {
            let result = operation_describe(&args).expect("listing succeeds");
            let listed = result.structured.as_ref().expect("a structured listing");
            let object = listed.as_object().expect("the listing is an object");
            assert_eq!(object.len(), 1, "one key only: {listed}");
            let entries = object["operations"]
                .as_array()
                .expect("an array of entries");
            assert!(!entries.is_empty(), "the catalogue holds an operation");
            for entry in entries {
                let keys: Vec<&str> = entry
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert_eq!(keys, ["long_name", "summary"], "entry: {entry}");
            }
            let long_names: Vec<&str> = entries
                .iter()
                .filter_map(|e| e["long_name"].as_str())
                .collect();
            assert!(
                long_names.contains(&"filter-rows"),
                "listed: {long_names:?}"
            );
        }
    }

    #[test]
    fn operation_describe_returns_the_catalogues_description() {
        let result =
            operation_describe(&json!({ "operation": "filter-rows" })).expect("describe succeeds");
        let described = result.structured.expect("a structured description");
        assert_eq!(described["long_name"], "filter-rows");
        assert!(described["reads"].is_array(), "described: {described}");
        assert!(described["writes"].is_array(), "described: {described}");
        assert!(
            described.get("applied_to").is_none(),
            "described: {described}"
        );
        assert_eq!(described["parameters"]["required"], json!(["where"]));
        // The text an MCP client shows is the same document.
        let from_text: Value = serde_json::from_str(&result.text).expect("text is JSON");
        assert_eq!(from_text, described);
    }

    #[test]
    fn operation_describe_unknown_operation_errors_naming_it() {
        let err = operation_describe(&json!({ "operation": "nope" })).unwrap_err();
        assert!(err.contains("`nope`"), "message: {err}");
        assert!(
            err.contains("no operation or operator called"),
            "message: {err}"
        );
    }

    #[test]
    fn operation_describe_refuses_an_operation_that_is_not_a_string() {
        let err = operation_describe(&json!({ "operation": ["filter-rows"] })).unwrap_err();
        assert!(err.contains("must be a string"), "message: {err}");
        assert!(err.contains("[\"filter-rows\"]"), "message: {err}");
    }

    #[test]
    fn operation_record_refuses_a_request_missing_a_key_or_of_the_wrong_shape() {
        let dir = tempfile::tempdir().unwrap();
        let request = json!({
            "dir": dir.path().to_str().unwrap(),
            "operation": "filter-rows",
            "on": "orders",
            "name": "big_orders",
            "arguments": { "where": "amount > 100" },
        });
        let refusals = [
            ("operation", Value::Null, "`operation` is required"),
            ("on", json!(null), "`on` is required"),
            (
                "name",
                json!(["big_orders"]),
                "`name` must be a string, not [\"big_orders\"]",
            ),
            (
                "arguments",
                json!("where=amount > 100"),
                "`arguments` must be an object",
            ),
            ("dir", json!(7), "`dir` must be a string, not 7"),
        ];
        for (key, value, message) in refusals {
            let mut args = request.clone();
            if value.is_null() && key == "operation" {
                args.as_object_mut().unwrap().remove(key);
            } else {
                args[key] = value;
            }
            let err = operation_record(&args).unwrap_err();
            assert!(err.contains(message), "{key}: {err}");
        }
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "a refused request wrote into the directory"
        );
    }

    #[test]
    fn operator_record_refuses_arguments_of_the_wrong_shape_before_anything_is_read() {
        let base = json!({ "operator": "http_fetch", "name": "x", "with": {} });
        for (key, value, message) in [
            ("dir", json!(7), "`dir` must be a string, not 7"),
            ("with", json!("text"), "`with` must be an object"),
            ("with", Value::Null, "`with` is required"),
            (
                "description",
                json!(3),
                "`description` must be a string, not 3",
            ),
            ("operator", json!(null), "`operator` is required"),
        ] {
            let mut args = base.clone();
            args[key] = value;
            let err = operator_record(&args).unwrap_err();
            assert!(err.contains(message), "{key}: {err}");
        }
    }

    #[test]
    fn sql_record_refuses_a_dir_that_is_not_a_string() {
        // Not the server's working directory: a mistyped `dir` is refused by name
        // before anything is read or written.
        let err = sql_record(&json!({
            "dir": 7,
            "sql": "SELECT 1 AS one",
            "name": "one",
        }))
        .unwrap_err();
        assert!(
            err.contains("`dir` must be a string, not 7"),
            "message: {err}"
        );
    }

    #[test]
    fn parse_params_accepts_objects_and_arrays() {
        let from_object = parse_params(Some(&json!({ "threshold": "25", "n": 3 }))).unwrap();
        assert!(from_object.contains(&("threshold".to_string(), "25".to_string())));
        assert!(from_object.contains(&("n".to_string(), "3".to_string())));

        let from_array = parse_params(Some(&json!(["date=2026-07-25"]))).unwrap();
        assert_eq!(
            from_array,
            vec![("date".to_string(), "2026-07-25".to_string())]
        );

        assert_eq!(parse_params(None).unwrap(), Vec::new());
        assert!(parse_params(Some(&json!("nope"))).is_err());
    }

    #[test]
    fn parse_params_refuses_a_key_the_command_line_cannot_carry() {
        let err = parse_params(Some(&json!({ "a=b": "c" }))).unwrap_err();
        assert!(err.contains("`a=b`"), "{err}");
        let err = parse_params(Some(&json!({ "": "c" }))).unwrap_err();
        assert!(err.contains("``"), "{err}");
        let err = parse_params(Some(&json!(["=c"]))).unwrap_err();
        assert!(err.contains("`=c`"), "{err}");
    }

    #[test]
    fn strip_ansi_drops_escape_sequences_and_keeps_the_text() {
        assert_eq!(
            strip_ansi("\u{1b}[2m[hook]\u{1b}[0m \u{1b}[1;31msetup\u{1b}[0m ...\nplain"),
            "[hook] setup ...\nplain"
        );
    }
}
