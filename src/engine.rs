use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::manifest::Manifest;

/// Information about the detected engine, returned by preflight.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    /// Parsed semantic version of the installed engine CLI,
    /// or None if version output was unparseable.
    pub version: Option<semver::Version>,
}

/// Output captured from a step execution.
/// Stdout is inherited (streams to terminal in real-time) unless output capture is active.
/// Stderr is captured for error reporting but also streamed for SQL steps.
#[derive(Debug)]
#[allow(dead_code)]
pub struct StepOutput {
    pub stderr: String,
    /// Captured stdout from a command step when output capture is active.
    /// None for SQL steps and command steps without output capture.
    pub stdout: Option<String>,
    /// What an `op:` step reports about its own execution — `ducklake_publish`'s
    /// snapshot id, for one. The runner copies it into the run contract's step entry
    /// verbatim, so the record of a Run carries what the step did and not only that it
    /// succeeded. `None` for SQL and command steps and for operators that report nothing.
    pub report: Option<serde_json::Value>,
}

/// Read stderr from a child process, streaming it to the terminal in real-time
/// while capturing the full content for error reporting.
fn stream_stderr(child: &mut std::process::Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };

    let mut buf = [0u8; 4096];
    let mut captured = Vec::new();

    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let _ = std::io::Write::write_all(&mut std::io::stderr(), &buf[..n]);
                captured.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    String::from_utf8_lossy(&captured).to_string()
}

/// Trait for executing pipeline steps.
pub trait Engine {
    /// Execute a SQL file against a database.
    /// `env` contains ARC_PARAM_ variables to inject into the child process environment.
    /// `timeout` is the maximum duration before the step is killed.
    fn execute_sql(
        &self,
        db_path: &Path,
        sql_path: &Path,
        env: &HashMap<String, String>,
        timeout: Option<Duration>,
    ) -> Result<StepOutput>;

    /// Execute a raw shell command.
    /// `env` contains ARC_PARAM_ variables to inject into the child process environment.
    /// If `capture_stdout` is true, stdout is piped and captured instead of inherited.
    /// `timeout` is the maximum duration before the step is killed.
    fn execute_command(
        &self,
        command: &str,
        env: &HashMap<String, String>,
        capture_stdout: bool,
        timeout: Option<Duration>,
    ) -> Result<StepOutput>;

    /// Check that the engine CLI is available and return information about it.
    /// Returns EngineInfo with the detected version (or None if unparseable).
    fn preflight(&self) -> Result<EngineInfo>;
}

/// Wait for a child process with an optional timeout.
/// Polls try_wait() at ~100ms intervals. On timeout, kills the child.
/// `pub(crate)` so the operator subprocess substrate shares the identical
/// kill-on-deadline semantics (the uv-run operators: splink_resolve, gleif).
pub(crate) fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
    step_name: &str,
) -> Result<std::process::ExitStatus> {
    let Some(deadline_duration) = timeout else {
        // No timeout — wait normally.
        return child.wait().map_err(|e| Error::StepExecution {
            step: step_name.to_string(),
            source: e,
        });
    };

    let deadline = Instant::now() + deadline_duration;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // Reap the killed process.
                    return Err(Error::StepTimeout {
                        step: step_name.to_string(),
                    });
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return Err(Error::StepExecution {
                    step: step_name.to_string(),
                    source: e,
                });
            }
        }
    }
}

/// Names the DuckDB executable arc runs. A program that ships arc sets it, because an
/// app started outside a login shell does not see the PATH a terminal user has.
pub const DUCKDB_BIN_ENV: &str = "ARC_DUCKDB_BIN";

/// The DuckDB versions arc is tested on. Enforced on every run with a SQL step, whether or
/// not the manifest states an `engine_version:`; a manifest's constraint narrows this and
/// never widens it.
pub const SUPPORTED_ENGINE_RANGE: &str = ">=1.2, <2";

/// Set to any non-empty value, lifts [`SUPPORTED_ENGINE_RANGE`] for a person who accepts
/// an engine arc was not tested on. The run warns, and a manifest's own `engine_version:`
/// still refuses what it refused before.
pub const ALLOW_UNTESTED_ENGINE_ENV: &str = "ARC_ALLOW_UNTESTED_ENGINE";

/// The DuckDB executable arc runs: the one `ARC_DUCKDB_BIN` names when it is set, else
/// `duckdb` found on the search path.
///
/// A set variable is final. When it names something that is not an executable file the
/// run is refused and PATH is not consulted, so a told engine can never silently become
/// a different install. A relative value is made absolute against the working directory,
/// because a bare file name handed to `Command::new` is itself a PATH lookup.
pub(crate) fn duckdb_program() -> Result<DuckDbProgram> {
    match std::env::var_os(DUCKDB_BIN_ENV) {
        None => Ok(DuckDbProgram::SearchPath),
        Some(raw) => {
            let path = PathBuf::from(raw);
            let invalid = |reason: String| Error::EngineBinInvalid {
                var: DUCKDB_BIN_ENV,
                path: path.clone(),
                reason,
            };
            if path.as_os_str().is_empty() {
                return Err(invalid("it is empty".to_string()));
            }
            let path_abs = std::path::absolute(&path).map_err(|e| invalid(e.to_string()))?;
            let meta = std::fs::metadata(&path_abs).map_err(|e| invalid(e.to_string()))?;
            if !meta.is_file() {
                return Err(invalid("not a file".to_string()));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 == 0 {
                    return Err(invalid("not executable".to_string()));
                }
            }
            Ok(DuckDbProgram::Told(path_abs))
        }
    }
}

/// Where the DuckDB executable came from, so a failure can name the route that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DuckDbProgram {
    /// `ARC_DUCKDB_BIN`, made absolute.
    Told(PathBuf),
    /// `duckdb`, resolved on PATH by the operating system.
    SearchPath,
}

impl DuckDbProgram {
    fn command(&self) -> Command {
        match self {
            DuckDbProgram::Told(path) => Command::new(path),
            DuckDbProgram::SearchPath => Command::new("duckdb"),
        }
    }
}

/// DuckDB CLI engine implementation. It resolves its executable on every call through
/// [`duckdb_program`], so the version preflight and the SQL steps reach the same one.
pub struct DuckDbEngine;

impl Engine for DuckDbEngine {
    fn execute_sql(
        &self,
        db_path: &Path,
        sql_path: &Path,
        env: &HashMap<String, String>,
        timeout: Option<Duration>,
    ) -> Result<StepOutput> {
        let step_name = sql_path.display().to_string();
        let mut child = duckdb_program()?
            .command()
            .arg(db_path)
            .arg("-f")
            .arg(sql_path)
            .envs(env)
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::StepExecution {
                step: step_name.clone(),
                source: e,
            })?;

        let stderr = stream_stderr(&mut child);
        let status = wait_with_timeout(&mut child, timeout, &step_name)?;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            return Err(Error::StepFailed {
                step: String::new(),
                code,
                stderr,
            });
        }

        Ok(StepOutput {
            stderr,
            stdout: None,
            report: None,
        })
    }

    fn execute_command(
        &self,
        command: &str,
        env: &HashMap<String, String>,
        capture_stdout: bool,
        timeout: Option<Duration>,
    ) -> Result<StepOutput> {
        let stdout_cfg = if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::inherit()
        };
        // Stderr: inherited for command steps (streams to terminal).
        // When capturing stdout, stderr remains inherited so errors are visible.
        let stderr_cfg = Stdio::inherit();

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .envs(env)
            .stdout(stdout_cfg)
            .stderr(stderr_cfg)
            .spawn()
            .map_err(|e| Error::StepExecution {
                step: command.to_string(),
                source: e,
            })?;

        // If capturing, read stdout before wait() to avoid deadlocks.
        // Timeout applies to the wait after stdout drain (constraint #10).
        let captured_stdout = if capture_stdout {
            let mut stdout_buf = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut stdout_buf);
            }
            let trimmed = stdout_buf.trim_end_matches('\n').to_string();
            Some(trimmed)
        } else {
            None
        };

        let status = wait_with_timeout(&mut child, timeout, command)?;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            return Err(Error::StepFailed {
                step: String::new(),
                code,
                stderr: String::new(),
            });
        }

        Ok(StepOutput {
            stderr: String::new(),
            stdout: captured_stdout,
            report: None,
        })
    }

    fn preflight(&self) -> Result<EngineInfo> {
        let program = duckdb_program()?;
        let output = program.command().arg("--version").output();

        match (output, program) {
            (Ok(o), _) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let version = parse_version_output(&stdout);
                Ok(EngineInfo { version })
            }
            (output, DuckDbProgram::Told(path)) => Err(Error::EngineBinInvalid {
                var: DUCKDB_BIN_ENV,
                path,
                reason: match output {
                    Ok(o) => format!("`--version` exited with {}", o.status),
                    Err(e) => format!("`--version` could not start: {e}"),
                },
            }),
            (_, DuckDbProgram::SearchPath) => Err(Error::EngineNotFound {
                engine: "duckdb".to_string(),
            }),
        }
    }
}

/// Parse a version string from engine CLI output.
///
/// Handles formats like:
/// - "v1.5.2 (Variegata) 8a5851971f"
/// - "v0.10.0 1234abc"
/// - "1.5.2"
///
/// Returns None if the version cannot be parsed.
pub fn parse_version_output(output: &str) -> Option<semver::Version> {
    // Find the first token that looks like a version (with or without leading 'v').
    for token in output.split_whitespace() {
        let stripped = token.strip_prefix('v').unwrap_or(token);
        if let Ok(ver) = semver::Version::parse(stripped) {
            return Some(ver);
        }
    }
    None
}

// ---- The community extensions a Protocol's SQL installs ----

/// The community extensions a Protocol's SQL may install, as arc holds them. The page
/// [`VETTED_EXTENSIONS_DOC`] is the same list for a person, and a test fails when the two
/// differ.
const VETTED_EXTENSIONS_JSON: &str = include_str!("vetted_extensions.json");

/// The page every refusal and warning about an extension points to.
pub(crate) const VETTED_EXTENSIONS_DOC: &str =
    "https://github.com/meridian-online/arcform/blob/main/docs/VETTED_EXTENSIONS.md";

/// Settings that move where DuckDB fetches an extension from without an `INSTALL` naming
/// the address. After `SET custom_extension_repository = '/tmp/ext'` a bare
/// `INSTALL mlpack;` resolves under `/tmp/ext`, and `autoinstall_extension_repository` does
/// the same for an extension DuckDB installs on its own when a function needs one.
const EXTENSION_REPOSITORY_SETTINGS: [&str; 2] = [
    "custom_extension_repository",
    "autoinstall_extension_repository",
];

/// One community extension a Protocol may install from the community registry. The file
/// also carries each entry's licence, repository and proof, which the page shows and the
/// check does not read.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct VettedExtension {
    pub(crate) name: String,
    /// The DuckDB versions its build was probed for and its proof was run on, as DuckDB
    /// prints them: `v1.5.5`.
    pub(crate) duckdb_versions: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct VettedList {
    extensions: Vec<VettedExtension>,
}

/// The vetted list arc holds, read once from the file compiled into the binary.
pub(crate) fn vetted_extensions() -> &'static [VettedExtension] {
    static LIST: OnceLock<Vec<VettedExtension>> = OnceLock::new();
    LIST.get_or_init(|| {
        serde_json::from_str::<VettedList>(VETTED_EXTENSIONS_JSON)
            .expect("src/vetted_extensions.json is a valid vetted list")
            .extensions
    })
}

/// A token of DuckDB SQL, as much of one as the extension check reads.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// A keyword or an identifier; `quoted` for a `"double-quoted"` one, kept as written.
    Word { text: String, quoted: bool },
    /// A string constant: `'…'`, `E'…'`, `X'…'`, `$$…$$` or `$tag$…$tag$`.
    Str(String),
    /// Any other character outside a comment: `;`, an operator, a digit, a parenthesis.
    Other(char),
}

/// How a quoted run treats a quote or a backslash inside it.
#[derive(Clone, Copy, PartialEq)]
enum Escapes {
    /// `''` is one quote: `'…'` and `"…"`.
    Doubled,
    /// `''` and a backslash both escape: `E'…'`.
    Backslash,
    /// The first quote ends it: `X'…'` and `B'…'`.
    None,
}

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_' || !ch.is_ascii()
}

fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$' || !ch.is_ascii()
}

/// Read a quoted run that starts just after its opening `quote`, returning its text and the
/// index after its closing quote, or the end of the input when it has none.
fn read_quoted(
    c: &[char],
    mut i: usize,
    quote: char,
    escapes: Escapes,
    line: &mut usize,
) -> (String, usize) {
    let mut text = String::new();
    while i < c.len() {
        let ch = c[i];
        if escapes == Escapes::Backslash && ch == '\\' {
            if let Some(&next) = c.get(i + 1) {
                *line += usize::from(next == '\n');
                text.push(next);
            }
            i += 2;
            continue;
        }
        if ch == quote {
            if escapes != Escapes::None && c.get(i + 1) == Some(&quote) {
                text.push(quote);
                i += 2;
                continue;
            }
            return (text, i + 1);
        }
        *line += usize::from(ch == '\n');
        text.push(ch);
        i += 1;
    }
    (text, c.len())
}

/// The length of the `$tag$` or `$$` that opens a dollar-quoted string at `i`, if one does.
/// A `$` followed by a digit is a parameter, and a `$` inside an identifier is part of it.
fn dollar_tag_len(c: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if c.get(j).is_some_and(|&ch| is_ident_start(ch)) {
        while c.get(j).is_some_and(|&ch| is_ident_char(ch) && ch != '$') {
            j += 1;
        }
    }
    (c.get(j) == Some(&'$')).then_some(j + 1 - i)
}

/// Split DuckDB SQL into tokens, each with the line it starts on, and drop the comments.
///
/// Each rule follows DuckDB's own scanner, as measured on DuckDB v1.5.5: a `/* */` comment
/// nests; `''` is a quote inside `'…'`; a backslash escapes only inside `E'…'`; `$$…$$` and
/// `$tag$…$tag$` are string constants. Text left open at the end of the input runs to the
/// end, where DuckDB refuses the statement rather than running it.
fn lex_sql(sql: &str) -> Vec<(Tok, usize)> {
    let c: Vec<char> = sql.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < n {
        let ch = c[i];
        let at = line;
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if ch == '-' && c.get(i + 1) == Some(&'-') {
            while i < n && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 0usize;
            while i < n {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    line += usize::from(c[i] == '\n');
                    i += 1;
                }
            }
        } else if ch == '\'' || ch == '"' {
            let (text, next) = read_quoted(&c, i + 1, ch, Escapes::Doubled, &mut line);
            out.push((
                if ch == '"' {
                    Tok::Word { text, quoted: true }
                } else {
                    Tok::Str(text)
                },
                at,
            ));
            i = next;
        } else if let Some(tag_len) = (ch == '$').then(|| dollar_tag_len(&c, i)).flatten() {
            let tag = &c[i..i + tag_len];
            let body = i + tag_len;
            let close = (body..n).find(|&k| c[k..].starts_with(tag));
            let end = close.unwrap_or(n);
            line += c[body..end].iter().filter(|&&ch| ch == '\n').count();
            out.push((Tok::Str(c[body..end].iter().collect()), at));
            i = close.map_or(n, |k| k + tag_len);
        } else if is_ident_start(ch) {
            let start = i;
            while i < n && is_ident_char(c[i]) {
                i += 1;
            }
            let text: String = c[start..i].iter().collect();
            let prefixed = match text.as_str() {
                "e" | "E" => Some(Escapes::Backslash),
                "x" | "X" | "b" | "B" => Some(Escapes::None),
                _ => None,
            };
            match prefixed {
                Some(escapes) if c.get(i) == Some(&'\'') => {
                    let (text, next) = read_quoted(&c, i + 1, '\'', escapes, &mut line);
                    out.push((Tok::Str(text), at));
                    i = next;
                }
                _ => out.push((
                    Tok::Word {
                        text,
                        quoted: false,
                    },
                    at,
                )),
            }
        } else {
            out.push((Tok::Other(ch), at));
            i += 1;
        }
    }
    out
}

/// Where one `INSTALL` asks DuckDB to fetch an extension from.
#[derive(Debug, Clone, PartialEq)]
enum InstallFrom {
    /// DuckDB's own repository: no `FROM`, or `FROM core`.
    Core,
    /// `FROM community`.
    Community,
    /// A repository alias other than `core` and `community`, such as `core_nightly`.
    Alias(String),
    /// `FROM '<directory or address>'`.
    Address(String),
}

/// One thing a Protocol's SQL says about where DuckDB installs an extension from.
#[derive(Debug, Clone, PartialEq)]
enum ExtensionSql {
    /// `[FORCE] INSTALL <name> [FROM <repository>]`, the name lower-cased as DuckDB does.
    Install { name: String, from: InstallFrom },
    /// `INSTALL '<path>'`: a name DuckDB reads as a file or an address, because it holds
    /// a `.`, a `/` or a `\`.
    InstallPath(String),
    /// A setting in [`EXTENSION_REPOSITORY_SETTINGS`], named outside a comment or a string.
    Setting(String),
}

/// Every place in `sql` that says where DuckDB installs an extension from, each with its
/// line.
///
/// `INSTALL` is looked for anywhere in a statement and not only at its start, because
/// DuckDB runs one that follows `EXPLAIN ANALYZE`. A repository alias is compared as
/// written, because DuckDB refuses `FROM Community` as an unknown repository.
fn scan_extension_sql(sql: &str) -> Vec<(ExtensionSql, usize)> {
    let toks = lex_sql(sql);
    let mut found = Vec::new();
    for (k, (tok, line)) in toks.iter().enumerate() {
        let Tok::Word { text, quoted } = tok else {
            continue;
        };
        if let Some(setting) = EXTENSION_REPOSITORY_SETTINGS
            .iter()
            .find(|s| text.eq_ignore_ascii_case(s))
        {
            found.push((ExtensionSql::Setting(setting.to_string()), *line));
            continue;
        }
        // `t.install` is a column, not a statement.
        if *quoted
            || !text.eq_ignore_ascii_case("install")
            || k > 0 && toks[k - 1].0 == Tok::Other('.')
        {
            continue;
        }
        let name = match toks.get(k + 1) {
            Some((Tok::Word { text, quoted }, _))
                if *quoted || !text.eq_ignore_ascii_case("from") =>
            {
                text
            }
            Some((Tok::Str(text), _)) => text,
            _ => continue,
        };
        if name.contains(['.', '/', '\\']) {
            found.push((ExtensionSql::InstallPath(name.clone()), *line));
            continue;
        }
        let from = match (toks.get(k + 2), toks.get(k + 3)) {
            (
                Some((
                    Tok::Word {
                        text: kw,
                        quoted: false,
                    },
                    _,
                )),
                Some((repo, _)),
            ) if kw.eq_ignore_ascii_case("from") => {
                match repo {
                    Tok::Word { text, .. } if text == "core" => InstallFrom::Core,
                    Tok::Word { text, .. } if text == "community" => InstallFrom::Community,
                    Tok::Word { text, .. } => InstallFrom::Alias(text.clone()),
                    Tok::Str(address) => InstallFrom::Address(address.clone()),
                    // `INSTALL x FROM;` does not parse, and DuckDB runs nothing of it.
                    Tok::Other(_) => continue,
                }
            }
            _ => InstallFrom::Core,
        };
        let name = name.to_lowercase();
        found.push((ExtensionSql::Install { name, from }, *line));
    }
    found
}

/// One SQL file a Protocol runs, and the step or hook that runs it.
pub(crate) struct ProtocolSql {
    /// `step 'load'`, or `hook on_init 'setup'`.
    pub(crate) place: String,
    /// The file as the manifest names it.
    pub(crate) file: String,
    pub(crate) path: PathBuf,
}

/// Every SQL file a Protocol's steps and hooks run: the steps in order, then the hooks.
pub(crate) fn protocol_sql(manifest: &Manifest, dir: &Path) -> Vec<ProtocolSql> {
    let hooks = [
        ("on_init", &manifest.hooks.on_init),
        ("on_success", &manifest.hooks.on_success),
        ("on_failure", &manifest.hooks.on_failure),
        ("on_exit", &manifest.hooks.on_exit),
    ];
    let steps = manifest
        .steps
        .iter()
        .map(|step| (format!("step '{}'", step.name), step));
    let hooks = hooks.into_iter().filter_map(|(slot, hook)| {
        hook.as_ref()
            .map(|step| (format!("hook {slot} '{}'", step.name), step))
    });
    steps
        .chain(hooks)
        .filter_map(|(place, step)| {
            step.sql.as_ref().map(|file| ProtocolSql {
                place,
                file: file.clone(),
                path: dir.join(file),
            })
        })
        .collect()
}

/// Refuse a Protocol whose SQL installs a DuckDB extension off the vetted list, before a
/// step runs; otherwise return one warning for each vetted extension it installs whose
/// entry does not name the engine's version.
///
/// Refused: `INSTALL <name> FROM community` for a name not on the list; an `INSTALL` from
/// an address or from a repository other than `core` and `community`; an `INSTALL` whose
/// name is a path or an address; and a statement naming a setting in
/// [`EXTENSION_REPOSITORY_SETTINGS`]. An `INSTALL` from `core` is not checked, and neither
/// is `LOAD`, because SQL does not say where a loaded extension was installed from. No
/// variable lifts the refusal.
///
/// It reads each file as it is before the run, and it is not a boundary: a `command:` step
/// can start a DuckDB of its own and install what it likes.
pub(crate) fn check_extension_installs(
    sql: &[ProtocolSql],
    engine_version: Option<&semver::Version>,
) -> Result<Vec<String>> {
    let vetted = vetted_extensions();
    let mut refusals = Vec::new();
    let mut installed: Vec<&VettedExtension> = Vec::new();
    for source in sql {
        // A file that is not there before the run is left to the step that runs it.
        let Ok(bytes) = std::fs::read(&source.path) else {
            continue;
        };
        for (found, line) in scan_extension_sql(&String::from_utf8_lossy(&bytes)) {
            let why = match found {
                ExtensionSql::Install {
                    from: InstallFrom::Core,
                    ..
                } => continue,
                ExtensionSql::Install {
                    name,
                    from: InstallFrom::Community,
                } => match vetted.iter().find(|entry| entry.name == name) {
                    Some(entry) => {
                        if !installed.iter().any(|seen| seen.name == entry.name) {
                            installed.push(entry);
                        }
                        continue;
                    }
                    None => format!(
                        "installs {name} from the community registry, and {name} is not on the vetted list"
                    ),
                },
                ExtensionSql::Install {
                    name,
                    from: InstallFrom::Alias(repository),
                } => format!(
                    "installs {name} from the repository {repository}; a community extension is installed FROM community"
                ),
                ExtensionSql::Install {
                    name,
                    from: InstallFrom::Address(address),
                } => format!("installs {name} from the address '{address}'"),
                ExtensionSql::InstallPath(path) => {
                    format!("installs the extension at '{path}'")
                }
                ExtensionSql::Setting(setting) => format!(
                    "names the setting {setting}, which moves where DuckDB installs an extension from"
                ),
            };
            refusals.push(format!(
                "{} ({}, line {line}) {why}",
                source.place, source.file
            ));
        }
    }
    if !refusals.is_empty() {
        return Err(Error::ExtensionRefused { refusals });
    }
    let engine = engine_version.map(|v| format!("v{v}"));
    Ok(installed
        .into_iter()
        .filter(|entry| {
            !engine
                .as_ref()
                .is_some_and(|v| entry.duckdb_versions.contains(v))
        })
        .map(|entry| {
            let vetted_on = entry.duckdb_versions.join(", ");
            let engine = engine.as_deref().map_or_else(
                || "arc could not read this engine's version".to_string(),
                |v| format!("this engine is DuckDB {v}"),
            );
            format!(
                "{} is vetted on DuckDB {vetted_on}, and {engine}; running it anyway ({VETTED_EXTENSIONS_DOC})",
                entry.name
            )
        })
        .collect())
}

#[cfg(test)]
#[allow(dead_code)]
pub mod mock {
    use super::*;
    use std::cell::RefCell;
    use std::fs;

    /// Records all calls for test assertions.
    pub struct MockEngine {
        pub calls: RefCell<Vec<MockCall>>,
        /// If set, fail on every call.
        pub should_fail: RefCell<Option<(i32, String)>>,
        /// If set, fail only on the Nth execution call (0-indexed, excludes preflight).
        pub fail_on_call: RefCell<Option<usize>>,
        /// Tracks the current execution call index (excludes preflight).
        exec_count: RefCell<usize>,
        /// If true, preflight returns EngineNotFound.
        pub preflight_should_fail: RefCell<bool>,
        /// Version to report from preflight. Defaults to 1.5.4, inside the range arc is
        /// tested on, so a test that does not care about the version is not refused by it.
        pub version: RefCell<Option<semver::Version>>,
        /// Simulated stdout for command steps with capture_stdout=true.
        pub simulated_stdout: RefCell<Option<String>>,
        /// If true, return StepTimeout when timeout is Some(_).
        pub timeout_should_fire: RefCell<bool>,
    }

    #[derive(Debug, Clone)]
    pub enum MockCall {
        Sql {
            db_path: String,
            sql_content: String,
            env: HashMap<String, String>,
        },
        Command {
            command: String,
            env: HashMap<String, String>,
            capture_stdout: bool,
        },
        Preflight,
    }

    impl MockEngine {
        pub fn new() -> Self {
            MockEngine {
                calls: RefCell::new(Vec::new()),
                should_fail: RefCell::new(None),
                fail_on_call: RefCell::new(None),
                exec_count: RefCell::new(0),
                preflight_should_fail: RefCell::new(false),
                version: RefCell::new(Some(semver::Version::new(1, 5, 4))),
                simulated_stdout: RefCell::new(None),
                timeout_should_fire: RefCell::new(false),
            }
        }

        /// Set simulated stdout for command steps with capture_stdout=true.
        pub fn set_simulated_stdout(&self, stdout: &str) {
            *self.simulated_stdout.borrow_mut() = Some(stdout.to_string());
        }

        /// Make the mock return StepTimeout when a timeout is provided.
        pub fn set_timeout_fire(&self) {
            *self.timeout_should_fire.borrow_mut() = true;
        }

        /// Set the version that preflight will report.
        pub fn set_version(&self, version: Option<semver::Version>) {
            *self.version.borrow_mut() = version;
        }

        /// Fail on every execution call.
        pub fn set_failure(&self, code: i32, stderr: &str) {
            *self.should_fail.borrow_mut() = Some((code, stderr.to_string()));
        }

        /// Make preflight return EngineNotFound.
        pub fn set_preflight_failure(&self) {
            *self.preflight_should_fail.borrow_mut() = true;
        }

        /// Fail only on the Nth execution call (0-indexed, excludes preflight).
        pub fn set_fail_on_call(&self, n: usize, code: i32, stderr: &str) {
            *self.fail_on_call.borrow_mut() = Some(n);
            *self.should_fail.borrow_mut() = Some((code, stderr.to_string()));
        }

        /// Check if this execution call should fail.
        fn should_fail_now(&self) -> Option<(i32, String)> {
            let current = *self.exec_count.borrow();
            *self.exec_count.borrow_mut() += 1;

            if let Some(fail_at) = *self.fail_on_call.borrow() {
                if current == fail_at {
                    return self.should_fail.borrow().clone();
                }
                return None;
            }

            // No fail_on_call set — use global should_fail.
            self.should_fail.borrow().clone()
        }
    }

    impl Engine for MockEngine {
        fn execute_sql(
            &self,
            db_path: &Path,
            sql_path: &Path,
            env: &HashMap<String, String>,
            timeout: Option<Duration>,
        ) -> Result<StepOutput> {
            let sql_content = fs::read_to_string(sql_path).map_err(|e| Error::FileRead {
                path: sql_path.to_path_buf(),
                source: e,
            })?;

            self.calls.borrow_mut().push(MockCall::Sql {
                db_path: db_path.display().to_string(),
                sql_content,
                env: env.clone(),
            });

            // Simulate timeout if configured and a timeout was provided.
            if timeout.is_some() && *self.timeout_should_fire.borrow() {
                return Err(Error::StepTimeout {
                    step: sql_path.display().to_string(),
                });
            }

            if let Some((code, stderr)) = self.should_fail_now() {
                return Err(Error::StepFailed {
                    step: String::new(),
                    code,
                    stderr,
                });
            }

            Ok(StepOutput {
                stderr: String::new(),
                stdout: None,
                report: None,
            })
        }

        fn execute_command(
            &self,
            command: &str,
            env: &HashMap<String, String>,
            capture_stdout: bool,
            timeout: Option<Duration>,
        ) -> Result<StepOutput> {
            self.calls.borrow_mut().push(MockCall::Command {
                command: command.to_string(),
                env: env.clone(),
                capture_stdout,
            });

            // Simulate timeout if configured and a timeout was provided.
            if timeout.is_some() && *self.timeout_should_fire.borrow() {
                return Err(Error::StepTimeout {
                    step: command.to_string(),
                });
            }

            if let Some((code, stderr)) = self.should_fail_now() {
                return Err(Error::StepFailed {
                    step: String::new(),
                    code,
                    stderr,
                });
            }

            let stdout = if capture_stdout {
                Some(self.simulated_stdout.borrow().clone().unwrap_or_default())
            } else {
                None
            };

            Ok(StepOutput {
                stderr: String::new(),
                stdout,
                report: None,
            })
        }

        fn preflight(&self) -> Result<EngineInfo> {
            self.calls.borrow_mut().push(MockCall::Preflight);
            if *self.preflight_should_fail.borrow() {
                return Err(Error::EngineNotFound {
                    engine: "duckdb".to_string(),
                });
            }
            Ok(EngineInfo {
                version: self.version.borrow().clone(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Parse version with codename (standard DuckDB format).
    #[test]
    fn test_lrp_parse_version_with_codename() {
        let version = parse_version_output("v1.5.2 (Variegata) 8a5851971f");
        assert_eq!(version.unwrap(), semver::Version::new(1, 5, 2));
    }

    // Parse version without codename.
    #[test]
    fn test_lrp_parse_version_without_codename() {
        let version = parse_version_output("v0.10.0 1234abc");
        assert_eq!(version.unwrap(), semver::Version::new(0, 10, 0));
    }

    // Parse bare version (no leading 'v').
    #[test]
    fn test_lrp_parse_bare_version() {
        let version = parse_version_output("1.5.2");
        assert_eq!(version.unwrap(), semver::Version::new(1, 5, 2));
    }

    // Unparseable output returns None.
    #[test]
    fn test_lrp_unparseable_version_returns_none() {
        assert!(parse_version_output("not a version").is_none());
        assert!(parse_version_output("").is_none());
        assert!(parse_version_output("duckdb").is_none());
    }

    // MockEngine returns configurable version.
    #[test]
    fn test_lrp_mock_engine_configurable_version() {
        let engine = mock::MockEngine::new();

        // Default is 1.5.4, inside the supported range.
        let info = engine.preflight().unwrap();
        assert_eq!(info.version.unwrap(), semver::Version::new(1, 5, 4));

        // Set custom version.
        engine.set_version(Some(semver::Version::new(1, 3, 0)));
        let info = engine.preflight().unwrap();
        assert_eq!(info.version.unwrap(), semver::Version::new(1, 3, 0));

        // Set None (unparseable).
        engine.set_version(None);
        let info = engine.preflight().unwrap();
        assert!(info.version.is_none());
    }
}
