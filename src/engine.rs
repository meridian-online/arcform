use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::manifest::{Manifest, Step};

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

    /// The platform the DuckDB the steps run on installs extensions for, as
    /// `PRAGMA platform` prints it: `linux_amd64`.
    fn extension_platform(&self) -> Result<String>;

    /// Install the community extension `name` on the DuckDB the steps run on, without
    /// loading it, and return the file DuckDB names as its `install_path` in
    /// `duckdb_extensions()`, or `None` when it names none. On a DuckDB that holds the
    /// extension already, DuckDB leaves the file as it is.
    fn install_community_extension(&self, name: &str) -> Result<Option<PathBuf>>;

    /// Install the build of the community extension `name` that the community registry
    /// serves, over any file DuckDB holds for it, on the DuckDB the steps run on; then load
    /// it, and return the file DuckDB names as its `install_path`, or `None` when it names
    /// none. DuckDB checks an extension's signature when it loads it, so a file returned is
    /// one DuckDB loads.
    fn force_install_community_extension(&self, name: &str) -> Result<Option<PathBuf>>;

    /// What the DuckDB the steps run on reports of itself, and of each extension in
    /// `extensions` that `duckdb_extensions()` lists, for the run record. It installs and
    /// loads nothing. A question DuckDB does not answer gives a report with each value absent,
    /// since recording refuses no run.
    fn report(&self, extensions: &[String]) -> EngineReport;
}

/// What the DuckDB the steps run on reports, as [`Engine::report`] asks it. Each value is
/// `None` where DuckDB gives none.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineReport {
    /// `SELECT version()`: `v1.5.5`.
    pub version: Option<String>,
    /// `PRAGMA platform`: `linux_amd64`.
    pub platform: Option<String>,
    /// What `duckdb_extensions()` gives for each extension asked about that it lists, by name.
    pub extensions: HashMap<String, ReportedExtension>,
}

/// What `duckdb_extensions()` gives for one extension.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportedExtension {
    /// `install_path`: the installed file, or `(BUILT-IN)` for an extension linked into
    /// DuckDB that no `INSTALL` has fetched a file for.
    pub path: Option<PathBuf>,
    /// `installed_from`: `core`, `community`, or the address it was installed from.
    pub repository: Option<String>,
    /// `extension_version`.
    pub version: Option<String>,
}

/// Read the lines [`DuckDbEngine::report`]'s question prints after [`ANSWER_MARK`]: each a tag,
/// then its values, separated by tabs. A line with a tag it does not know is skipped.
fn read_report(answers: &[String]) -> EngineReport {
    let given = |value: &str| (!value.is_empty()).then(|| value.to_string());
    let mut report = EngineReport::default();
    for answer in answers {
        match answer.split_once('\t') {
            Some(("version", version)) => report.version = given(version),
            Some(("platform", platform)) => report.platform = given(platform),
            Some(("extension", fields)) => {
                // The path last, so a tab in it stays in it.
                let mut fields = fields.splitn(4, '\t');
                let (Some(name), Some(repository), Some(version), Some(path)) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                report.extensions.insert(
                    name.to_string(),
                    ReportedExtension {
                        path: given(path).map(PathBuf::from),
                        repository: given(repository),
                        version: given(version),
                    },
                );
            }
            _ => {}
        }
    }
    report
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

/// Whether `name` is one arc writes into an `INSTALL` as it is: ASCII letters, digits and
/// `_`, and not empty.
fn is_plain_extension_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// What arc prints before each value it asks DuckDB for, so a line a `~/.duckdbrc` prints
/// is not read as the answer.
const ANSWER_MARK: &str = "arc-answer:";

/// Run `sql` on an in-memory DuckDB, as a step's DuckDB is started and with no Protocol
/// database, and return each value it printed after [`ANSWER_MARK`]. `what` names the
/// question in the error when DuckDB exits non-zero.
fn ask_duckdb(sql: &str, what: &str) -> Result<Vec<String>> {
    let output = duckdb_program()?
        .command()
        .args(["-noheader", "-list", "-c", sql])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::EngineQuery {
            what: what.to_string(),
            reason: e.to_string(),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::EngineQuery {
            what: what.to_string(),
            reason: format!("it exited with {}: {}", output.status, stderr.trim()),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix(ANSWER_MARK))
        .map(str::to_string)
        .collect())
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

    fn extension_platform(&self) -> Result<String> {
        let what = "report its platform";
        let sql = format!("SELECT '{ANSWER_MARK}' || platform FROM pragma_platform();");
        ask_duckdb(&sql, what)?
            .into_iter()
            .next()
            .filter(|platform| !platform.is_empty())
            .ok_or_else(|| Error::EngineQuery {
                what: what.to_string(),
                reason: "it printed no platform".to_string(),
            })
    }

    fn install_community_extension(&self, name: &str) -> Result<Option<PathBuf>> {
        install_and_name(
            name,
            &format!("INSTALL {name} FROM community;"),
            &format!("install {name} from the community registry"),
        )
    }

    fn force_install_community_extension(&self, name: &str) -> Result<Option<PathBuf>> {
        install_and_name(
            name,
            &format!("FORCE INSTALL {name} FROM community; LOAD {name};"),
            &format!("install and load the build of {name} the community registry serves"),
        )
    }

    fn report(&self, extensions: &[String]) -> EngineReport {
        let mut sql = format!(
            "SELECT '{ANSWER_MARK}version' || chr(9) || version(); \
             SELECT '{ANSWER_MARK}platform' || chr(9) || platform FROM pragma_platform();"
        );
        if !extensions.is_empty() {
            let names: Vec<String> = extensions
                .iter()
                .map(|name| format!("'{}'", name.replace('\'', "''")))
                .collect();
            sql.push_str(&format!(
                " SELECT '{ANSWER_MARK}extension' || chr(9) || extension_name || chr(9) || coalesce(installed_from, '') || chr(9) || coalesce(extension_version, '') || chr(9) || coalesce(install_path, '') FROM duckdb_extensions() WHERE extension_name IN ({});",
                names.join(", ")
            ));
        }
        ask_duckdb(
            &sql,
            "report its version, its platform and the extensions it holds",
        )
        .map(|answers| read_report(&answers))
        .unwrap_or_default()
    }
}

/// Run `install`, SQL that installs the extension `name`, and return the file DuckDB then
/// names as its `install_path`, or `None` when it names none. `what` names the question in
/// the error when DuckDB exits non-zero.
fn install_and_name(name: &str, install: &str, what: &str) -> Result<Option<PathBuf>> {
    // The name comes from the vetted list, whose names are plain identifiers; one that is
    // not is refused rather than written into SQL.
    if !is_plain_extension_name(name) {
        return Err(Error::EngineQuery {
            what: format!("install {name}"),
            reason: "arc installs an extension by a plain name alone".to_string(),
        });
    }
    let sql = format!(
        "{install} SELECT '{ANSWER_MARK}' || install_path FROM duckdb_extensions() WHERE extension_name = '{name}' AND installed;"
    );
    Ok(ask_duckdb(&sql, what)?
        .into_iter()
        .find(|path| !path.is_empty())
        .map(PathBuf::from))
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

/// The settings that move where DuckDB keeps and finds the extensions it installs:
/// `extension_directory`, the list `extension_directories`, and `home_directory`, under which
/// DuckDB keeps them when neither of the others is set. A step that sets one installs to a
/// directory the check of each pinned extension does not read, and a second DuckDB does not
/// find the file there. DuckDB v1.5.5's `duckdb_settings()` lists these three and no other
/// that moves it, and a test holds the list to what the DuckDB CLI it runs lists.
const EXTENSION_DIRECTORY_SETTINGS: [&str; 3] = [
    "extension_directory",
    "extension_directories",
    "home_directory",
];

/// What switches DuckDB to its second parser. The DuckDB CLI loads the `autocomplete`
/// extension, whose parser replaces the default one once `allow_parser_override_extension` is
/// `'fallback'` or `'strict'`, and `enable_peg_parser()` sets it to `'strict'`. That parser
/// reads comments and strings by rules of its own: a block comment does not nest, and a
/// backslash escapes nothing inside `E'…'`. arc reads SQL by the default parser's rules, so a
/// statement naming either is refused, whatever value it sets. Everything before the switch
/// is read by the default rules, the only ones in force until it runs.
///
/// A string naming either is refused too: `query('FROM enable_peg_parser()')` runs the SQL a
/// string holds.
const PARSER_SWITCHES: [&str; 2] = ["enable_peg_parser", "allow_parser_override_extension"];

/// The functions that run SQL held in their argument: `query()` a `SELECT` in a string, and
/// `json_execute_serialized_sql()` one serialized as JSON. A step can assemble that argument
/// while it runs, as `'FROM enable_' || 'peg_parser()'`, and arc reads only its text.
const RUNS_SQL: [&str; 2] = ["query", "json_execute_serialized_sql"];

/// The words after which a name is one a statement declares or writes to, and a column list
/// can follow it: `CREATE TABLE query (a INT)` makes a table called `query`. DuckDB takes no
/// call there: `CREATE TABLE query(getvariable('q'))` does not parse.
const NAMED_BEFORE_A_COLUMN_LIST: [&str; 6] =
    ["table", "view", "into", "exists", "with", "recursive"];

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

/// The characters DuckDB's scanner skips between tokens: space, tab, `\n`, `\r` and form
/// feed. A vertical tab is not one of them, and neither is any character above ASCII: DuckDB
/// reads those as part of a word, after its parser has replaced the few in
/// [`unicode_space_len`] with a space.
fn is_scanner_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

/// Count the `\n` in `c[from..to]`, the one line end every reader here counts.
fn newlines(c: &[char], from: usize, to: usize) -> usize {
    c[from..to].iter().filter(|&&ch| ch == '\n').count()
}

/// Where a `'…'` string goes on after its closing quote at `j - 1`, if it does: DuckDB
/// reads `'anofox_'` and `'forecast'` as one string when all that separates them is
/// whitespace holding a `\n` or a `\r`, and `--` comments. The spaces and comments before
/// that line end are its own line's; after it, each comment has to end at a line end.
fn quote_continues(c: &[char], mut j: usize) -> Option<usize> {
    let comment_end = |j: usize| {
        (j + 2..c.len())
            .find(|&k| matches!(c[k], '\n' | '\r'))
            .unwrap_or(c.len())
    };
    loop {
        match c.get(j) {
            Some(' ' | '\t' | '\u{c}') => j += 1,
            Some('-') if c.get(j + 1) == Some(&'-') => j = comment_end(j),
            Some('\n' | '\r') => break,
            _ => return None,
        }
    }
    loop {
        match c.get(j) {
            Some(&ch) if is_scanner_space(ch) => j += 1,
            Some('-') if c.get(j + 1) == Some(&'-') => j = comment_end(j) + 1,
            Some('\'') => return Some(j + 1),
            _ => return None,
        }
    }
}

/// The character a backslash escape in an `E'…'` string stands for, as DuckDB's scanner
/// decodes it, and how many characters after the backslash the escape spans: `\b`, `\f`,
/// `\n`, `\r` and `\t`; one to three octal digits; `\x` and one or two hex digits. Any other
/// character stands for itself, and a backslash at the end of the input for a backslash.
/// DuckDB's CLI refuses a file whose SQL holds a `\u` or `\U` escape, so arc reads those as
/// the letter, which can only turn a name the list holds into one it does not.
fn e_escape(c: &[char], at: usize) -> (char, usize) {
    let digits = |from: usize, radix: u32, most: usize| {
        c.get(from..)
            .unwrap_or_default()
            .iter()
            .take(most)
            .take_while(|ch| ch.is_digit(radix))
            .count()
    };
    let value = |from: usize, len: usize, radix: u32| {
        c[from..from + len]
            .iter()
            .fold(0u32, |v, ch| v * radix + ch.to_digit(radix).unwrap_or(0))
    };
    let byte = |v: u32| char::from((v & 0xff) as u8);
    let Some(&ch) = c.get(at) else {
        return ('\\', 0);
    };
    match (ch, digits(at + 1, 16, 2)) {
        ('0'..='7', _) => {
            let len = digits(at, 8, 3);
            (byte(value(at, len, 8)), len)
        }
        ('x', len @ 1..) => (byte(value(at + 1, len, 16)), len + 1),
        ('b', _) => ('\u{8}', 1),
        ('f', _) => ('\u{c}', 1),
        ('n', _) => ('\n', 1),
        ('r', _) => ('\r', 1),
        ('t', _) => ('\t', 1),
        (other, _) => (other, 1),
    }
}

/// Read a quoted run from `*i`, just after its opening `quote`, to its closing quote or the
/// end of the input, and leave `*i` after it. A `'…'` run goes on where
/// [`quote_continues`] finds the next.
fn read_quoted(
    c: &[char],
    i: &mut usize,
    quote: char,
    escapes: Escapes,
    line: &mut usize,
) -> String {
    let mut text = String::new();
    while *i < c.len() {
        let ch = c[*i];
        if escapes == Escapes::Backslash && ch == '\\' {
            let (decoded, len) = e_escape(c, *i + 1);
            *line += newlines(c, *i + 1, *i + 1 + len);
            text.push(decoded);
            *i += 1 + len;
            continue;
        }
        if ch == quote {
            if escapes != Escapes::None && c.get(*i + 1) == Some(&quote) {
                text.push(quote);
                *i += 2;
                continue;
            }
            *i += 1;
            let Some(next) = (quote == '\'').then(|| quote_continues(c, *i)).flatten() else {
                return text;
            };
            *line += newlines(c, *i, next);
            *i = next;
            continue;
        }
        *line += usize::from(ch == '\n');
        text.push(ch);
        *i += 1;
    }
    text
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
/// `sql` is one batch, as [`cli_batches`] cuts it and [`strip_unicode_spaces`] leaves it.
/// Each rule is DuckDB's own scanner's, `third_party/libpg_query/scan.l` in DuckDB v1.5.5:
/// the characters in [`is_scanner_space`] are skipped; a `--` comment ends at `\n` or `\r`;
/// a `/* */` comment nests; `''` is a quote inside `'…'`; a backslash escapes only inside
/// `E'…'`; `$$…$$` and `$tag$…$tag$` are string constants; two `'…'` strings with a line
/// end between them are one. Text left open at the end of the input runs to the end, where
/// DuckDB refuses the statement rather than running it.
///
/// Every branch consumes the characters it matched before it calls a helper or loops, so the
/// scan advances on each pass whatever a helper returns: a lexer that can stall would hang
/// `arc run` on one bad edit and fill memory with tokens.
fn lex_sql(sql: &str) -> Vec<(Tok, usize)> {
    let c: Vec<char> = sql.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < n {
        let ch = c[i];
        let at = line;
        if is_scanner_space(ch) {
            line += usize::from(ch == '\n');
            i += 1;
        } else if ch == '-' && c.get(i + 1) == Some(&'-') {
            i += 2;
            while i < n && !matches!(c[i], '\n' | '\r') {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    line += usize::from(c[i] == '\n');
                    i += 1;
                }
            }
        } else if ch == '\'' || ch == '"' {
            i += 1;
            let text = read_quoted(&c, &mut i, ch, Escapes::Doubled, &mut line);
            out.push((
                if ch == '"' {
                    Tok::Word { text, quoted: true }
                } else {
                    Tok::Str(text)
                },
                at,
            ));
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
            i += 1;
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
                    i += 1;
                    let text = read_quoted(&c, &mut i, '\'', escapes, &mut line);
                    out.push((Tok::Str(text), at));
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

/// One place where a step's SQL can run SQL that is not in its file, which the check does not
/// read for an extension that SQL installs. Ordered as the warning names two on one line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RunTimeSql {
    /// A function in [`RUNS_SQL`], by the name arc prints, called on anything but one string
    /// literal, or on a literal whose text makes such a call.
    Call(&'static str),
    /// `IMPORT DATABASE`, or `import_database`: runs the SQL files of a directory, which a
    /// step can write with `COPY`.
    ImportDatabase,
    /// A string or a quoted name holding `.duckdbrc`, the file the DuckDB CLI runs before
    /// each step's file.
    Duckdbrc,
}

impl std::fmt::Display for RunTimeSql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunTimeSql::Call(name) => write!(f, "{name}()"),
            RunTimeSql::ImportDatabase => f.write_str("IMPORT DATABASE"),
            RunTimeSql::Duckdbrc => f.write_str(".duckdbrc"),
        }
    }
}

/// One thing a Protocol's SQL says about where DuckDB installs an extension from, or a place
/// where it runs SQL the check does not read.
#[derive(Debug, Clone, PartialEq)]
enum ExtensionSql {
    /// `[FORCE] INSTALL <name> [FROM <repository>]`, the name lower-cased as DuckDB does.
    Install { name: String, from: InstallFrom },
    /// `INSTALL '<path>'`: a name DuckDB reads as a file or an address, because it holds
    /// a `.`, a `/` or a `\`.
    InstallPath(String),
    /// A setting in [`EXTENSION_REPOSITORY_SETTINGS`] or [`EXTENSION_DIRECTORY_SETTINGS`],
    /// named outside a comment or a string.
    Setting(String),
    /// A name in [`PARSER_SWITCHES`], named outside a comment.
    ParserSwitch(String),
    /// SQL the step builds or writes while it runs, which draws a warning and not a refusal.
    RunTime(RunTimeSql),
}

// ---- A SQL file as DuckDB's CLI reads it ----
//
// `arc run` hands a SQL step to `duckdb <db> -f <file>`. What reaches DuckDB's scanner is not
// the file: the CLI reads it a line at a time, skips some lines, and hands the parser one
// batch of lines at a time; the parser then replaces some unicode spaces before its scanner
// runs. Each function below copies one of those steps from DuckDB v1.5.5's source, and
// v1.5.4's is the same: `tools/shell/shell.cpp` for the CLI, `src/parser/parser.cpp` for the
// pre-pass. A step is copied as far as it changes which text DuckDB reads as SQL, and no
// further.

/// The lines DuckDB's CLI reads from a file, each with the file line it starts on, as
/// `local_getline` reads them with `fgets`: a line ends at `\n`, and a NUL byte ends what the
/// line keeps of the `fgets` chunk it is in. The rest of that chunk is dropped, and the next
/// chunk joins the line, which for a short line is the next line of the file. A chunk holds
/// the room left in a buffer that starts at 100 bytes and grows to twice its size and 100
/// more whenever 100 bytes are not left, less one byte for the NUL `fgets` writes.
///
/// The `\r` DuckDB drops before each `\n` is kept: every rule after this one reads a `\r` as
/// it reads a space. So is a last line the CLI would not return because a NUL left it empty:
/// an empty line is no SQL.
fn cli_lines(bytes: &[u8]) -> Vec<(Vec<u8>, usize)> {
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut size = 100;
    let mut starts_on = 1;
    for (index, physical) in bytes.split_inclusive(|&b| b == b'\n').enumerate() {
        let mut at = 0;
        // Each pass reads one chunk of at least one byte, so a pass per byte is enough.
        for _ in 0..physical.len() {
            while line.len() + 100 > size {
                size = size * 2 + 100;
            }
            let take = (size - line.len() - 1).min(physical.len() - at);
            line.extend(physical[at..at + take].iter().take_while(|&&b| b != 0));
            at += take;
            if line.last() == Some(&b'\n') {
                line.pop();
                lines.push((std::mem::take(&mut line), starts_on));
                size = 100;
                starts_on = index + 2;
            }
        }
    }
    lines.push((line, starts_on));
    lines
}

/// A space to the CLI's own tests of a line: DuckDB's `CharacterIsSpace`, which unlike its
/// scanner counts a vertical tab.
fn is_cli_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// Whether `z` holds only spaces and comments, as the CLI's `_all_whitespace` reads it: a
/// `/* */` comment does not nest, and one left open is not whitespace; a `--` comment runs
/// to `\n`.
fn all_whitespace(z: &[u8]) -> bool {
    let mut i = 0;
    while let Some(&b) = z.get(i) {
        i = match (b, z.get(i + 1)) {
            _ if is_cli_space(b) => i + 1,
            (b'/', Some(b'*')) => match z[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(k) => i + 2 + k + 2,
                None => return false,
            },
            (b'-', Some(b'-')) => match z[i..].iter().position(|&b| b == b'\n') {
                Some(k) => i + k,
                None => return true,
            },
            _ => return false,
        };
    }
    true
}

/// A byte DuckDB allows in a dollar-quote tag: a letter, `_`, or a byte above ASCII, and a
/// digit anywhere but first. The CLI, the parser's pre-pass and the scanner agree on it.
fn is_tag_byte(b: u8, first: bool) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80 || !first && b.is_ascii_digit()
}

/// Whether the CLI hands the lines it holds to the parser now, as its `SQLIsComplete` reads
/// them: when they end in a `;` outside a string or comment. This is not the scanner's
/// reading: a `/* */` comment does not nest, a backslash escapes nothing, a `--` comment runs
/// to `\n` alone, and a `$tag$` opens anywhere, inside a word too. Each difference moves
/// where a batch ends, and so which lines after it are read as SQL.
fn sql_is_complete(z: &[u8]) -> bool {
    let mut semicolon = false;
    let mut i = 0;
    while let Some(&b) = z.get(i) {
        let rest = &z[i + 1..];
        // Where what opens at `i` ends, and whether it is `;`; `None` for a space or a
        // comment, which leaves the state as it was.
        let (end, token) = match b {
            b';' => (i + 1, Some(true)),
            b' ' | b'\r' | b'\t' | b'\n' | b'\x0c' => (i + 1, None),
            b'/' if rest.first() == Some(&b'*') => {
                match rest[1..].windows(2).position(|w| w == b"*/") {
                    Some(k) => (i + 2 + k + 2, None),
                    None => return false,
                }
            }
            b'-' if rest.first() == Some(&b'-') => match rest.iter().position(|&b| b == b'\n') {
                Some(k) => (i + 1 + k, None),
                None => return semicolon,
            },
            b'$' => {
                let tag = rest
                    .iter()
                    .enumerate()
                    .take_while(|&(k, &b)| is_tag_byte(b, k == 0))
                    .count();
                if rest.get(tag) == Some(&b'$') {
                    let delim = [b"$", &rest[..tag], b"$"].concat();
                    let body = i + tag + 2;
                    match (body..z.len()).find(|&k| z[k..].starts_with(&delim)) {
                        Some(k) => (k + delim.len(), None),
                        None => return false,
                    }
                } else {
                    (i + 1, Some(false))
                }
            }
            b'\'' | b'"' => match rest.iter().position(|&q| q == b) {
                Some(k) => (i + 1 + k + 1, None),
                None => return false,
            },
            _ => (i + 1, Some(false)),
        };
        if let Some(is_semicolon) = token {
            semicolon = is_semicolon;
        }
        i = end;
    }
    semicolon
}

/// The batches of SQL DuckDB's CLI hands its parser from a file, each with the file line
/// each of its lines starts on, as the CLI's `ProcessInput` cuts them. A line whose first
/// byte is `\x03` drops the lines held so far. A line starting with `.` or `#` while no line
/// is held is a dot command or a comment, and is not SQL, whatever quote or comment it opens.
/// Any other line is held, and the lines held go to the parser together once a line holding
/// a `;` completes them by [`sql_is_complete`], or at the end of the file. Lines that hold
/// only spaces and comments are dropped.
///
/// A dot command can itself run SQL or a program, such as `.read` and `.shell`; what it runs
/// is not read here.
fn cli_batches(bytes: &[u8]) -> Vec<(Vec<u8>, Vec<usize>)> {
    let mut batches = Vec::new();
    let mut sql = Vec::new();
    let mut lines = Vec::new();
    for (line, starts_on) in cli_lines(bytes) {
        if line.first() == Some(&0x03) {
            sql.clear();
            lines.clear();
            continue;
        }
        if sql.is_empty() && matches!(line.first(), Some(b'.' | b'#')) {
            continue;
        }
        let prior = sql.len();
        if !sql.is_empty() {
            sql.push(b'\n');
        }
        sql.extend_from_slice(&line);
        lines.push(starts_on);
        if sql[prior..].contains(&b';') && sql_is_complete(&sql) {
            batches.push((std::mem::take(&mut sql), std::mem::take(&mut lines)));
        } else if all_whitespace(&sql) {
            sql.clear();
            lines.clear();
        }
    }
    // The CLI hands over what is left unless it is only spaces and comments, which hold no
    // statement to find.
    batches.push((sql, lines));
    batches
}

/// The length of the unicode space DuckDB's parser replaces with a space at the start of
/// `q`, if one is there: U+00A0, U+2000 to U+200B, U+202F, U+205F, U+2060, U+3000, and
/// U+FEFF, the byte-order mark some editors write at the start of a file.
fn unicode_space_len(q: &[u8]) -> Option<usize> {
    match q {
        [0xC2, 0xA0, ..] => Some(2),
        [0xE2, 0x80, 0x80..=0x8B | 0xAF, ..]
        | [0xE2, 0x81, 0x9F | 0xA0, ..]
        | [0xE3, 0x80, 0x80, ..]
        | [0xEF, 0xBB, 0xBF, ..] => Some(3),
        _ => None,
    }
}

/// A batch as DuckDB's scanner receives it: each unicode space in [`unicode_space_len`]
/// replaced with a space, as the parser's `StripUnicodeSpaces` pre-pass does, except inside
/// what the pre-pass reads as a string or a comment. That is not the scanner's reading: a
/// `'` or `"` opens a string until the next of the same quote, a `$` and a tag open a
/// dollar-quoted string, a `--` comment ends at `\n` or `\r`, and `/* */` is not a comment.
/// So a `'` inside a block comment opens a string for the pre-pass, and a U+200B after it
/// stays, to be read by the scanner as part of a word.
///
/// DuckDB repeats the pass until it replaces nothing; one pass leaves nothing for a second,
/// because a space opens nothing and a unicode space after a `$` is read as part of a tag.
fn strip_unicode_spaces(q: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < q.len() {
        let rest = &q[pos..];
        if let Some(len) = unicode_space_len(rest) {
            out.push(b' ');
            pos += len;
            continue;
        }
        let end = match rest {
            [quote @ (b'\'' | b'"'), tail @ ..] => tail
                .iter()
                .position(|b| b == quote)
                .map_or(q.len(), |k| pos + k + 2),
            [b'-', b'-', ..] => rest
                .iter()
                .position(|&b| b == b'\n' || b == b'\r')
                .map_or(q.len(), |k| pos + k),
            [b'$', next, ..] if *next == b'$' || is_tag_byte(*next, true) => {
                let tag = rest[1..]
                    .iter()
                    .take_while(|&&b| is_tag_byte(b, false))
                    .count();
                let close = pos + 1 + tag;
                match q.get(close) {
                    // The search for the closing tag starts at the opening tag's last `$`,
                    // and ends on the closing tag's last `$`, which is read again.
                    Some(b'$') => {
                        let delim = [b"$", &q[pos + 1..close], b"$"].concat();
                        (close..q.len())
                            .find(|&k| q[k..].starts_with(&delim))
                            .map_or(q.len(), |k| k + delim.len() - 1)
                    }
                    // Not a tag: read on from the byte that ended it.
                    Some(_) => close,
                    None => q.len(),
                }
            }
            _ => pos + 1,
        };
        out.extend_from_slice(&q[pos..end]);
        pos = end;
    }
    out
}

/// Every place in a SQL file that says where DuckDB installs an extension from, and every
/// place where it runs SQL that is not in the file, each with the file line it is on. The
/// file is read as DuckDB's CLI reads it: [`cli_batches`], then [`strip_unicode_spaces`],
/// then [`lex_sql`], each batch alone.
fn scan_extension_sql(file: &[u8]) -> Vec<(ExtensionSql, usize)> {
    let mut found = Vec::new();
    for (batch, lines) in cli_batches(file) {
        let batch = strip_unicode_spaces(&batch);
        let toks = lex_sql(&String::from_utf8_lossy(&batch));
        let run_time = scan_run_time_sql(&toks)
            .into_iter()
            .map(|(shape, line)| (ExtensionSql::RunTime(shape), line));
        // A batch holds one `\n` between each two of its lines, and nothing else adds one.
        for (finding, line) in scan_tokens(&toks).into_iter().chain(run_time) {
            found.push((finding, lines[line - 1]));
        }
    }
    found
}

/// Every place in one batch's tokens where the SQL that runs is not in the file, each with
/// its line in the batch: a call in [`unread_calls`]; `IMPORT DATABASE` or `import_database`,
/// in any letter case and whatever follows; and a string or a quoted name whose text holds
/// `.duckdbrc` in any letter case, since macOS's default file system does not tell
/// `.DuckDBrc` from `.duckdbrc`. `import_database` is one word to [`lex_sql`], as `_` is a
/// word character, so `PRAGMA import_database('imp')` is looked for by its own name.
///
/// These are shapes, not a boundary: a path to `.duckdbrc` assembled from pieces is not among
/// them. A step that writes over a later step's or hook's SQL file is not a shape either: that
/// file is read again just before it runs, by [`ExtensionRecheck::check`].
fn scan_run_time_sql(toks: &[(Tok, usize)]) -> Vec<(RunTimeSql, usize)> {
    let mut found: Vec<(RunTimeSql, usize)> = unread_calls(toks)
        .into_iter()
        .map(|(name, line)| (RunTimeSql::Call(name), line))
        .collect();
    for (k, (tok, line)) in toks.iter().enumerate() {
        let shape = match (tok, toks.get(k + 1).map(|(next, _)| next)) {
            (Tok::Word { text, .. }, Some(Tok::Word { text: next, .. }))
                if text.eq_ignore_ascii_case("import") && next.eq_ignore_ascii_case("database") =>
            {
                RunTimeSql::ImportDatabase
            }
            (Tok::Word { text, .. }, _) if text.eq_ignore_ascii_case("import_database") => {
                RunTimeSql::ImportDatabase
            }
            (Tok::Word { text, quoted: true } | Tok::Str(text), _)
                if text.to_lowercase().contains(".duckdbrc") =>
            {
                RunTimeSql::Duckdbrc
            }
            _ => continue,
        };
        found.push((shape, *line));
    }
    found
}

/// Each call in `toks` to a function in [`RUNS_SQL`] whose argument arc does not read, by the
/// function's name, with the line of the call. The name is read in any letter case, quoted or
/// not and after a schema, as DuckDB runs `FROM "query"('SELECT 1')` and
/// `FROM system.main.query('SELECT 1')`; a name a column list follows is not a call.
fn unread_calls(toks: &[(Tok, usize)]) -> Vec<(&'static str, usize)> {
    let mut found = Vec::new();
    for (k, (tok, line)) in toks.iter().enumerate() {
        let Tok::Word { text, .. } = tok else {
            continue;
        };
        let Some(name) = RUNS_SQL
            .into_iter()
            .find(|name| text.eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let [(Tok::Other('('), _), argument @ ..] = &toks[k + 1..] else {
            continue;
        };
        if names_a_table(&toks[..k]) || reads_argument(name, argument) {
            continue;
        }
        found.push((name, *line));
    }
    found
}

/// Whether the name after `before` is one a statement declares or writes to: after a word in
/// [`NAMED_BEFORE_A_COLUMN_LIST`], read past a schema such as `main.`.
fn names_a_table(before: &[(Tok, usize)]) -> bool {
    let mut before = before;
    while let [rest @ .., (Tok::Word { .. }, _), (Tok::Other('.'), _)] = before {
        before = rest;
    }
    matches!(
        before.last(),
        Some((Tok::Word { text, .. }, _))
            if NAMED_BEFORE_A_COLUMN_LIST.iter().any(|word| text.eq_ignore_ascii_case(word))
    )
}

/// Whether arc reads the SQL a call to `name` runs, from the tokens after its `(`: one string
/// literal and the `)`, whose text makes no call arc does not read. `query()` runs the literal
/// as SQL, which DuckDB's parser reads after its unicode-space pass; the serialized SQL
/// `json_execute_serialized_sql()` runs is read by [`serialized_calls`].
fn reads_argument(name: &str, argument: &[(Tok, usize)]) -> bool {
    let [(Tok::Str(literal), _), (Tok::Other(')'), _), ..] = argument else {
        return false;
    };
    if name == "query" {
        let literal = strip_unicode_spaces(literal.as_bytes());
        unread_calls(&lex_sql(&String::from_utf8_lossy(&literal))).is_empty()
    } else {
        !serialized_calls(literal)
    }
}

/// Whether SQL serialized as JSON, as `json_execute_serialized_sql()` takes it, calls a
/// function in [`RUNS_SQL`]: an object whose `function_name` is one, at any depth. DuckDB
/// runs `query('SELECT ' || '42')` from its serialized form. Text arc cannot read as JSON is
/// read as making such a call.
fn serialized_calls(json: &str) -> bool {
    fn calls(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
                key == "function_name"
                    && value
                        .as_str()
                        .is_some_and(|name| RUNS_SQL.iter().any(|f| name.eq_ignore_ascii_case(f)))
                    || calls(value)
            }),
            serde_json::Value::Array(items) => items.iter().any(calls),
            _ => false,
        }
    }
    serde_json::from_str(json).map_or(true, |value| calls(&value))
}

/// The warning for one step or hook whose SQL holds places in [`scan_run_time_sql`], naming
/// each by its line, in line order, and each shape a line holds once. The scan gives a batch's
/// calls before its other shapes, and one line can hold a shape twice with another between.
fn run_time_warning(source: &ProtocolSql, mut shapes: Vec<(RunTimeSql, usize)>) -> String {
    shapes.sort_by_key(|(shape, line)| (*line, shape.clone()));
    shapes.dedup();
    let places: Vec<String> = shapes
        .iter()
        .map(|(shape, line)| format!("{shape} on line {line}"))
        .collect();
    format!(
        "{} ({}) can run or write SQL that is not in its file: {}. arc does not read the SQL a step builds or writes while it runs, for an extension that SQL installs; running it anyway ({VETTED_EXTENSIONS_DOC})",
        source.place,
        source.file,
        places.join(", ")
    )
}

/// Every place in one batch's tokens that says where DuckDB installs an extension from, each
/// with its line in the batch.
///
/// `INSTALL` is looked for anywhere in a statement and not only at its start, because
/// DuckDB runs one that follows `EXPLAIN ANALYZE`. A repository alias is compared as
/// written, because DuckDB refuses `FROM Community` as an unknown repository.
fn scan_tokens(toks: &[(Tok, usize)]) -> Vec<(ExtensionSql, usize)> {
    let mut found = Vec::new();
    for (k, (tok, line)) in toks.iter().enumerate() {
        let switch = match tok {
            Tok::Word { text, .. } => PARSER_SWITCHES
                .iter()
                .find(|name| text.eq_ignore_ascii_case(name)),
            Tok::Str(text) => {
                let text = text.to_ascii_lowercase();
                PARSER_SWITCHES.iter().find(|name| text.contains(*name))
            }
            Tok::Other(_) => None,
        };
        if let Some(name) = switch {
            found.push((ExtensionSql::ParserSwitch(name.to_string()), *line));
        }
        let Tok::Word { text, quoted } = tok else {
            continue;
        };
        if let Some(setting) = EXTENSION_REPOSITORY_SETTINGS
            .iter()
            .chain(&EXTENSION_DIRECTORY_SETTINGS)
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
    /// The step's name, or `None` for a hook: what `ARC_FAILED_STEP` names when the check
    /// just before it runs refuses it.
    pub(crate) step: Option<String>,
    /// The file as the manifest names it.
    pub(crate) file: String,
    pub(crate) path: PathBuf,
}

impl ProtocolSql {
    /// The SQL file `step` runs, or `None` when it runs a command or an operator.
    pub(crate) fn step(step: &Step, dir: &Path) -> Option<Self> {
        Self::new(
            format!("step '{}'", step.name),
            Some(step.name.clone()),
            step,
            dir,
        )
    }

    /// The SQL file the hook in `slot` runs, or `None` when it runs a command.
    pub(crate) fn hook(slot: &str, hook: &Step, dir: &Path) -> Option<Self> {
        Self::new(format!("hook {slot} '{}'", hook.name), None, hook, dir)
    }

    fn new(place: String, name: Option<String>, step: &Step, dir: &Path) -> Option<Self> {
        step.sql.as_ref().map(|file| ProtocolSql {
            place,
            step: name,
            file: file.clone(),
            path: dir.join(file),
        })
    }
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
        .filter_map(|step| ProtocolSql::step(step, dir));
    let hooks = hooks
        .into_iter()
        .filter_map(|(slot, hook)| ProtocolSql::hook(slot, hook.as_ref()?, dir));
    steps.chain(hooks).collect()
}

/// What [`check_extension_installs`] found in a Protocol it did not refuse.
#[derive(Debug, PartialEq)]
pub(crate) struct ExtensionInstalls {
    /// One warning for each vetted extension whose entry does not name the engine's
    /// version, then one for each step or hook whose SQL can run or write SQL that is not
    /// in its file.
    pub(crate) warnings: Vec<String>,
    /// Each vetted community extension the SQL installs `FROM community`, once, in the order
    /// the Protocol first installs it.
    pub(crate) installed: Vec<String>,
    /// Each extension the SQL installs by name, from DuckDB's own repository or `FROM
    /// community`, once, in the order the Protocol first installs it: what the run record
    /// names.
    pub(crate) recorded: Vec<String>,
}

/// What the check reads in one SQL file: a line for each statement it refuses, each vetted
/// community extension the file installs `FROM community`, once, in the order the file first
/// installs it, each extension it installs by name from DuckDB's own repository or `FROM
/// community`, likewise, and each place the file runs SQL that is not in it.
struct FileCheck {
    refusals: Vec<String>,
    installed: Vec<&'static VettedExtension>,
    recorded: Vec<String>,
    shapes: Vec<(RunTimeSql, usize)>,
}

/// Add each name in `names` that `list` does not hold to its end, in order.
fn add_new(list: &mut Vec<String>, names: impl IntoIterator<Item = String>) {
    for name in names {
        if !list.contains(&name) {
            list.push(name);
        }
    }
}

/// Check the SQL file `source` runs, holding `bytes`, as [`check_extension_installs`] checks
/// each file.
fn check_file(source: &ProtocolSql, bytes: &[u8]) -> FileCheck {
    let vetted = vetted_extensions();
    let mut found = FileCheck {
        refusals: Vec::new(),
        installed: Vec::new(),
        recorded: Vec::new(),
        shapes: Vec::new(),
    };
    for (finding, line) in scan_extension_sql(bytes) {
        let why = match finding {
            ExtensionSql::RunTime(shape) => {
                found.shapes.push((shape, line));
                continue;
            }
            ExtensionSql::Install {
                name,
                from: InstallFrom::Core,
            } => {
                add_new(&mut found.recorded, [name]);
                continue;
            }
            ExtensionSql::Install {
                name,
                from: InstallFrom::Community,
            } => match vetted.iter().find(|entry| entry.name == name) {
                Some(entry) => {
                    if !found.installed.iter().any(|seen| seen.name == entry.name) {
                        found.installed.push(entry);
                    }
                    add_new(&mut found.recorded, [name]);
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
            ExtensionSql::Setting(setting)
                if EXTENSION_DIRECTORY_SETTINGS.contains(&setting.as_str()) =>
            {
                format!(
                    "names the setting {setting}, which moves where DuckDB keeps the extensions it installs"
                )
            }
            ExtensionSql::Setting(setting) => format!(
                "names the setting {setting}, which moves where DuckDB installs an extension from"
            ),
            ExtensionSql::ParserSwitch(name) => format!(
                "names {name}, which switches DuckDB to a second parser whose comments and strings arc does not read"
            ),
        };
        found.refusals.push(format!(
            "{} ({}, line {line}) {why}",
            source.place, source.file
        ));
    }
    found
}

/// Refuse a Protocol whose SQL installs a DuckDB extension off the vetted list, before a
/// step runs; otherwise return the vetted community extensions it installs, one warning
/// for each whose entry does not name the engine's version, and one for each step or hook
/// whose SQL can run or write SQL that is not in its file, as [`scan_run_time_sql`] finds it.
///
/// Refused: `INSTALL <name> FROM community` for a name not on the list; an `INSTALL` from
/// an address or from a repository other than `core` and `community`; an `INSTALL` whose
/// name is a path or an address; a statement naming a setting in
/// [`EXTENSION_REPOSITORY_SETTINGS`] or [`EXTENSION_DIRECTORY_SETTINGS`]; and a statement
/// naming a name in [`PARSER_SWITCHES`]. An `INSTALL` from `core` is not checked, and neither
/// is `LOAD`, because SQL does not say where a loaded extension was installed from. No
/// variable lifts the refusal.
///
/// It reads each file as it is before the run, and [`ExtensionRecheck::check`] reads each
/// step's and hook's file again, and checks it again, just before it runs. Neither is a
/// boundary: a `command:` step can start a DuckDB
/// of its own and install what it likes, and a process an earlier step leaves running can
/// write a file after the check just before it runs has read it.
pub(crate) fn check_extension_installs(
    sql: &[ProtocolSql],
    engine_version: Option<&semver::Version>,
) -> Result<ExtensionInstalls> {
    let mut refusals = Vec::new();
    let mut installed: Vec<&VettedExtension> = Vec::new();
    let mut recorded = Vec::new();
    let mut unread = Vec::new();
    for source in sql {
        // A file that is not there before the run is checked just before the step or hook
        // that runs it.
        let Ok(bytes) = std::fs::read(&source.path) else {
            continue;
        };
        let found = check_file(source, &bytes);
        refusals.extend(found.refusals);
        for entry in found.installed {
            if !installed.iter().any(|seen| seen.name == entry.name) {
                installed.push(entry);
            }
        }
        add_new(&mut recorded, found.recorded);
        if !found.shapes.is_empty() {
            unread.push((source, found.shapes));
        }
    }
    if !refusals.is_empty() {
        return Err(Error::ExtensionRefused { refusals });
    }
    let names = installed.iter().map(|entry| entry.name.clone()).collect();
    let warnings = installed
        .into_iter()
        .filter_map(|entry| unvetted_version_warning(entry, engine_version))
        .chain(
            unread
                .into_iter()
                .map(|(source, shapes)| run_time_warning(source, shapes)),
        )
        .collect();
    Ok(ExtensionInstalls {
        warnings,
        installed: names,
        recorded,
    })
}

/// The check of each SQL step's and hook's file just before it runs, for a file a step
/// earlier in the run wrote over or wrote where there was none. It holds what the checks
/// before the run found, and what each check since has found, so a file that has not changed
/// since the check before the run read it draws nothing: it holds no statement that check
/// refused, each extension it installs has been compared with its pin, and each warning it
/// draws has been printed.
#[derive(Default)]
pub(crate) struct ExtensionRecheck {
    /// Each vetted community extension a check has found.
    installed: Vec<String>,
    /// Each extension a file a check passed installs by name, from DuckDB's own repository
    /// or `FROM community`, once, in the order the run first found it.
    recorded: Vec<String>,
    /// Each pinned extension a check installed and found equal to its pin.
    pinned: Vec<PinnedExtension>,
    /// Each warning printed so far.
    warned: HashSet<String>,
    pins: ExtensionPins,
    engine_version: Option<semver::Version>,
}

impl ExtensionRecheck {
    /// From what the checks before the run found: the vetted extensions
    /// [`check_extension_installs`] found, the extensions [`check_extension_pins`] found equal
    /// to their pins, and each warning the two printed.
    pub(crate) fn new(
        installs: ExtensionInstalls,
        pinned: Vec<PinnedExtension>,
        pin_warnings: Vec<String>,
        pins: &ExtensionPins,
        engine_version: Option<&semver::Version>,
    ) -> Self {
        ExtensionRecheck {
            installed: installs.installed,
            recorded: installs.recorded,
            pinned,
            warned: installs.warnings.into_iter().chain(pin_warnings).collect(),
            pins: pins.clone(),
            engine_version: engine_version.cloned(),
        }
    }

    /// Each pinned extension a check found equal to its pin, before the run or just before a
    /// step or hook ran, which [`recheck_extension_pins`] hashes again when the run ends.
    pub(crate) fn pinned(&self) -> &[PinnedExtension] {
        &self.pinned
    }

    /// Each extension a file a check passed installs by name, from DuckDB's own repository or
    /// `FROM community`, once, in the order the run first found it: the extensions the run
    /// record names.
    pub(crate) fn recorded(&self) -> &[String] {
        &self.recorded
    }

    /// Just before the step or hook `source` names runs: read its file again, refuse it on
    /// what [`check_extension_installs`] refuses in a file, and check each vetted community
    /// extension it installs that no check has found against its pin, as
    /// [`check_extension_pins`] does. Returns each warning those two give that the run has not
    /// printed. A file arc cannot read is left to the engine, which reads it next.
    pub(crate) fn check(
        &mut self,
        engine: &dyn Engine,
        source: &ProtocolSql,
    ) -> Result<Vec<String>> {
        let Ok(bytes) = std::fs::read(&source.path) else {
            return Ok(Vec::new());
        };
        let found = check_file(source, &bytes);
        if !found.refusals.is_empty() {
            return Err(Error::ExtensionRefusedBeforeItRan {
                place: source.place.clone(),
                step: source.step.clone(),
                refusals: found.refusals,
            });
        }
        let new: Vec<&VettedExtension> = found
            .installed
            .into_iter()
            .filter(|entry| !self.installed.contains(&entry.name))
            .collect();
        let names: Vec<String> = new.iter().map(|entry| entry.name.clone()).collect();
        let (pinned, pin_warnings) =
            check_extension_pins(engine, &names, &self.pins, self.engine_version.as_ref())
                .map_err(|e| match e {
                    Error::ExtensionPinRefused { refusals } => {
                        Error::ExtensionPinRefusedBeforeItRan {
                            place: source.place.clone(),
                            step: source.step.clone(),
                            refusals,
                        }
                    }
                    e => e,
                })?;
        let unread = (!found.shapes.is_empty()).then(|| run_time_warning(source, found.shapes));
        let warnings = new
            .into_iter()
            .filter_map(|entry| unvetted_version_warning(entry, self.engine_version.as_ref()))
            .chain(unread)
            .chain(pin_warnings)
            .filter(|warning| self.warned.insert(warning.clone()))
            .collect();
        self.installed.extend(names);
        add_new(&mut self.recorded, found.recorded);
        self.pinned.extend(pinned);
        Ok(warnings)
    }
}

/// The warning for a vetted extension whose entry does not name `engine_version`, or `None`
/// when it names it. `arc run` and `arc upgrade` print it and go ahead.
pub(crate) fn unvetted_version_warning(
    entry: &VettedExtension,
    engine_version: Option<&semver::Version>,
) -> Option<String> {
    let engine = engine_version.map(|v| format!("v{v}"));
    if engine
        .as_ref()
        .is_some_and(|v| entry.duckdb_versions.contains(v))
    {
        return None;
    }
    let vetted_on = entry.duckdb_versions.join(", ");
    let engine = engine.as_deref().map_or_else(
        || "arc could not read this engine's version".to_string(),
        |v| format!("this engine is DuckDB {v}"),
    );
    Some(format!(
        "{} is vetted on DuckDB {vetted_on}, and {engine}; running it anyway ({VETTED_EXTENSIONS_DOC})",
        entry.name
    ))
}

/// A community extension the check before the run installed and found equal to its pin,
/// which [`recheck_extension_pins`] hashes again when the run ends.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PinnedExtension {
    pub(crate) name: String,
    /// The DuckDB version the pin is for, as DuckDB prints it: `v1.5.5`.
    pub(crate) version: String,
    pub(crate) platform: String,
    pub(crate) pin: String,
    /// The file DuckDB named as the extension's `install_path`.
    pub(crate) path: PathBuf,
}

/// The pins under a manifest's `extensions:` key: name, DuckDB version, platform, SHA-256.
pub(crate) type ExtensionPins = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

/// Before any step or hook runs: install each community extension in `installed` that
/// `pins` pins for the engine's DuckDB version and platform, on the DuckDB the steps run on,
/// and refuse the run when the installed file's SHA-256 differs from its pin, when the
/// install fails, or when DuckDB names no installed file. Return the extensions it found
/// equal to their pins, and one warning for each extension in `installed` with no pin for
/// this version and platform.
///
/// DuckDB is asked its platform only when an extension in `installed` has a pin for the
/// engine's version, so a Protocol with no pin makes the engine calls it made before pins.
/// With no engine version, no pin can be chosen, and each extension is unpinned.
pub(crate) fn check_extension_pins(
    engine: &dyn Engine,
    installed: &[String],
    pins: &ExtensionPins,
    engine_version: Option<&semver::Version>,
) -> Result<(Vec<PinnedExtension>, Vec<String>)> {
    let version = engine_version.map(|v| format!("v{v}"));
    let for_version = |name: &String| {
        version
            .as_ref()
            .and_then(|v| pins.get(name)?.get(v))
            .filter(|platforms| !platforms.is_empty())
    };
    let mut refusals = Vec::new();
    let platform = if installed.iter().any(|name| for_version(name).is_some()) {
        match engine.extension_platform() {
            Ok(platform) => Some(platform),
            Err(e) => {
                for name in installed.iter().filter(|name| for_version(name).is_some()) {
                    refusals.push(format!(
                        "{name} has a pin for DuckDB {}, and arc could not choose it: {e}",
                        version.as_deref().unwrap_or_default()
                    ));
                }
                return Err(Error::ExtensionPinRefused { refusals });
            }
        }
    } else {
        None
    };
    let mut pinned = Vec::new();
    let mut warnings = Vec::new();
    for name in installed {
        let pin = for_version(name)
            .zip(platform.as_ref())
            .and_then(|(platforms, platform)| platforms.get(platform));
        let (Some(version), Some(platform), Some(pin)) = (&version, &platform, pin) else {
            let place = match (&version, &platform) {
                (None, _) => {
                    "arc could not read this engine's DuckDB version to choose one".to_string()
                }
                (Some(v), None) => format!("for DuckDB {v}"),
                (Some(v), Some(p)) => format!("for DuckDB {v} on {p}"),
            };
            warnings.push(format!(
                "{name} has no pin under the extensions: key of arcform.yaml ({place}), so arc runs whichever build DuckDB installs; `arc upgrade {name}` pins the build the community registry serves ({VETTED_EXTENSIONS_DOC})"
            ));
            continue;
        };
        let at = format!("{name} on DuckDB {version}, {platform}");
        let path = match engine.install_community_extension(name) {
            Ok(Some(path)) => path,
            Ok(None) => {
                refusals.push(format!(
                    "{at}: DuckDB named no installed file for {name} after installing it"
                ));
                continue;
            }
            Err(e) => {
                refusals.push(format!(
                    "{at}: arc could not install {name} to compare it with its pin: {e}"
                ));
                continue;
            }
        };
        match crate::fetch_cache::hash_file(&path) {
            Ok(found) if found == *pin => pinned.push(PinnedExtension {
                name: name.clone(),
                version: version.clone(),
                platform: platform.clone(),
                pin: pin.clone(),
                path,
            }),
            Ok(found) => refusals.push(format!(
                "{at}: pinned {pin}, and the installed file {} hashes to {found}; `arc upgrade {name}` installs the build the community registry serves and pins it",
                path.display()
            )),
            Err(e) => refusals.push(format!(
                "{at}: arc could not read the installed file {}: {e}",
                path.display()
            )),
        }
    }
    if !refusals.is_empty() {
        return Err(Error::ExtensionPinRefused { refusals });
    }
    Ok((pinned, warnings))
}

/// When the run ends: hash each file [`check_extension_pins`] found equal to its pin
/// again, and fail the run when one differs, because a step replaced it.
pub(crate) fn recheck_extension_pins(pinned: &[PinnedExtension]) -> Result<()> {
    let changes: Vec<String> = pinned
        .iter()
        .filter_map(|ext| {
            let at = format!("{} on DuckDB {}, {}", ext.name, ext.version, ext.platform);
            match crate::fetch_cache::hash_file(&ext.path) {
                Ok(found) if found == ext.pin => None,
                Ok(found) => Some(format!(
                    "{at}: pinned {}, and when the run ended {} hashed to {found}",
                    ext.pin,
                    ext.path.display()
                )),
                Err(e) => Some(format!(
                    "{at}: pinned {}, and when the run ended {} could not be read: {e}",
                    ext.pin,
                    ext.path.display()
                )),
            }
        })
        .collect();
    if changes.is_empty() {
        Ok(())
    } else {
        Err(Error::ExtensionChanged { changes })
    }
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
        /// The platform `extension_platform` reports; `None` makes it fail.
        pub platform: RefCell<Option<String>>,
        /// What `install_community_extension` answers for each name: the installed file,
        /// no file, or a failure's reason. A name not here names no file.
        pub installs: RefCell<HashMap<String, std::result::Result<Option<PathBuf>, String>>>,
        /// What `report` answers. Asking it is not one of `calls`, which the tests in
        /// `runner.rs` count; `reported` holds what each ask named.
        pub report: RefCell<EngineReport>,
        /// The extensions each call of `report` asked about, in order.
        pub reported: RefCell<Vec<Vec<String>>>,
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
        Platform,
        Install {
            name: String,
        },
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
                platform: RefCell::new(Some("linux_amd64".to_string())),
                installs: RefCell::new(HashMap::new()),
                report: RefCell::new(EngineReport::default()),
                reported: RefCell::new(Vec::new()),
            }
        }

        /// Set what `install_community_extension` answers for `name`.
        pub fn set_install(
            &self,
            name: &str,
            answer: std::result::Result<Option<PathBuf>, String>,
        ) {
            self.installs.borrow_mut().insert(name.to_string(), answer);
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

        fn extension_platform(&self) -> Result<String> {
            self.calls.borrow_mut().push(MockCall::Platform);
            self.platform
                .borrow()
                .clone()
                .ok_or_else(|| Error::EngineQuery {
                    what: "report its platform".to_string(),
                    reason: "the mock was told to fail".to_string(),
                })
        }

        fn install_community_extension(&self, name: &str) -> Result<Option<PathBuf>> {
            self.calls.borrow_mut().push(MockCall::Install {
                name: name.to_string(),
            });
            match self.installs.borrow().get(name) {
                None => Ok(None),
                Some(Ok(path)) => Ok(path.clone()),
                Some(Err(reason)) => Err(Error::EngineQuery {
                    what: format!("install {name} from the community registry"),
                    reason: reason.clone(),
                }),
            }
        }

        /// `arc run` never force-installs, so a call here fails the test that made it.
        /// `arc upgrade`, the one caller, is tested on a fake DuckDB in
        /// `tests/vetted_extensions.rs`.
        fn force_install_community_extension(&self, name: &str) -> Result<Option<PathBuf>> {
            panic!("the mock engine was asked to force-install {name}, which arc run never does")
        }

        fn report(&self, extensions: &[String]) -> EngineReport {
            self.reported.borrow_mut().push(extensions.to_vec());
            self.report.borrow().clone()
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

#[cfg(test)]
mod extension_tests {
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
        Tok::Str(text.to_string())
    }

    /// The tokens of `sql`, without their lines.
    fn toks(sql: &str) -> Vec<Tok> {
        lex_sql(sql).into_iter().map(|(tok, _)| tok).collect()
    }

    /// The findings of `sql`, without their lines.
    fn scan(sql: &str) -> Vec<ExtensionSql> {
        scan_extension_sql(sql.as_bytes())
            .into_iter()
            .map(|(f, _)| f)
            .collect()
    }

    fn install(name: &str, from: InstallFrom) -> ExtensionSql {
        ExtensionSql::Install {
            name: name.to_string(),
            from,
        }
    }

    // ---- the lexer, rule by rule ----

    #[test]
    fn lex_splits_words_strings_and_punctuation() {
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
    fn lex_drops_a_line_comment_to_the_end_of_its_line() {
        assert_eq!(
            lex_sql("-- INSTALL a\nINSTALL b"),
            vec![(word("INSTALL"), 2), (word("b"), 2)]
        );
    }

    #[test]
    fn lex_nests_block_comments_as_duckdb_does() {
        assert_eq!(
            toks("/* /* */ INSTALL a; */ INSTALL b"),
            vec![word("INSTALL"), word("b")]
        );
        assert_eq!(toks("/* a */ x /* b */"), vec![word("x")]);
        assert_eq!(lex_sql("/*\n\n*/ x"), vec![(word("x"), 3)]);
        assert_eq!(toks("/* open /* INSTALL a */ INSTALL b"), Vec::<Tok>::new());
    }

    #[test]
    fn lex_reads_a_doubled_quote_as_one_and_a_backslash_as_itself() {
        assert_eq!(toks("'it''s' x"), vec![string("it's"), word("x")]);
        assert_eq!(toks(r"'a\' x"), vec![string(r"a\"), word("x")]);
        assert_eq!(toks("'open"), vec![string("open")]);
        assert_eq!(toks(r#""a""b" c"#), vec![quoted(r#"a"b"#), word("c")]);
    }

    #[test]
    fn lex_honours_a_backslash_only_inside_an_e_string() {
        assert_eq!(toks(r"E'a\'b' x"), vec![string("a'b"), word("x")]);
        assert_eq!(toks(r"e'a\'b' x"), vec![string("a'b"), word("x")]);
        assert_eq!(toks("E'a''b'"), vec![string("a'b")]);
        assert_eq!(toks("e x"), vec![word("e"), word("x")]);
        assert_eq!(
            toks(r"ex'a\' x"),
            vec![word("ex"), string(r"a\"), word("x")]
        );
        assert_eq!(
            lex_sql("E'\\\n' x"),
            vec![(string("\n"), 1), (word("x"), 2)]
        );
    }

    #[test]
    fn lex_ends_a_bit_or_hex_string_at_its_first_quote() {
        assert_eq!(toks("X'ab''cd'"), vec![string("ab"), string("cd")]);
        assert_eq!(toks("x'ab''cd'"), vec![string("ab"), string("cd")]);
        assert_eq!(toks("B'01''10'"), vec![string("01"), string("10")]);
        assert_eq!(toks("b'01''10'"), vec![string("01"), string("10")]);
    }

    #[test]
    fn lex_reads_dollar_quoted_strings() {
        assert_eq!(toks("$$ a ' b $$ x"), vec![string(" a ' b "), word("x")]);
        assert_eq!(toks("$t$ $$ $t$ x"), vec![string(" $$ "), word("x")]);
        assert_eq!(toks("$$ open"), vec![string(" open")]);
        // `$1` is a parameter, so `$1a$` opens no string: a tag cannot start with a digit.
        assert_eq!(
            toks("$1a$ x"),
            vec![Tok::Other('$'), Tok::Other('1'), word("a$"), word("x")]
        );
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
        assert_eq!(
            lex_sql("$$\n\n$$ x"),
            vec![(string("\n\n"), 1), (word("x"), 3)]
        );
    }

    #[test]
    fn lex_counts_lines_inside_a_string() {
        assert_eq!(
            lex_sql("'a\nb' x\ny"),
            vec![(string("a\nb"), 1), (word("x"), 2), (word("y"), 3)]
        );
    }

    // ---- what the scan finds ----

    #[test]
    fn scan_reads_where_each_install_fetches_from() {
        assert_eq!(
            scan("INSTALL mlpack FROM community;"),
            vec![install("mlpack", InstallFrom::Community)]
        );
        assert_eq!(
            scan("INSTALL spatial FROM core; INSTALL excel; INSTALL x VERSION 'v1';"),
            vec![
                install("spatial", InstallFrom::Core),
                install("excel", InstallFrom::Core),
                install("x", InstallFrom::Core)
            ]
        );
        assert_eq!(
            scan("INSTALL mlpack FROM core_nightly;"),
            vec![install("mlpack", InstallFrom::Alias("core_nightly".into()))]
        );
        assert_eq!(
            scan("INSTALL mlpack FROM 'https://example.org/ext';"),
            vec![install(
                "mlpack",
                InstallFrom::Address("https://example.org/ext".into())
            )]
        );
    }

    #[test]
    fn scan_compares_a_repository_as_written() {
        assert_eq!(
            scan("INSTALL mlpack FROM Community;"),
            vec![install("mlpack", InstallFrom::Alias("Community".into()))]
        );
        assert_eq!(
            scan(r#"INSTALL mlpack FROM "community";"#),
            vec![install("mlpack", InstallFrom::Community)]
        );
        assert_eq!(
            scan("INSTALL mlpack FROM 'community';"),
            vec![install("mlpack", InstallFrom::Address("community".into()))]
        );
    }

    #[test]
    fn scan_lowercases_a_name_and_reads_force_and_any_case() {
        assert_eq!(
            scan("force install ANOFOX_forecast from community;"),
            vec![install("anofox_forecast", InstallFrom::Community)]
        );
        assert_eq!(
            scan(r#"INSTALL "MLPACK" FROM community; INSTALL 'Rapidfuzz' FROM community;"#),
            vec![
                install("mlpack", InstallFrom::Community),
                install("rapidfuzz", InstallFrom::Community)
            ]
        );
        assert_eq!(
            scan(r#"INSTALL "from" FROM community;"#),
            vec![install("from", InstallFrom::Community)]
        );
    }

    #[test]
    fn scan_finds_an_install_after_explain_analyze() {
        assert_eq!(
            scan("EXPLAIN ANALYZE INSTALL anofox_forecast FROM community;"),
            vec![install("anofox_forecast", InstallFrom::Community)]
        );
    }

    #[test]
    fn scan_reads_a_name_holding_a_path_character_as_a_path() {
        assert_eq!(
            scan("INSTALL '/tmp/ext/mlpack.duckdb_extension';"),
            vec![ExtensionSql::InstallPath(
                "/tmp/ext/mlpack.duckdb_extension".into()
            )]
        );
        assert_eq!(
            scan(r#"INSTALL 'a.b'; INSTALL 'c\d'; INSTALL "e/f" FROM community;"#),
            vec![
                ExtensionSql::InstallPath("a.b".into()),
                ExtensionSql::InstallPath(r"c\d".into()),
                ExtensionSql::InstallPath("e/f".into())
            ]
        );
    }

    #[test]
    fn scan_skips_what_is_not_an_install_statement() {
        assert_eq!(scan("SELECT t.install FROM community;"), vec![]);
        assert_eq!(scan("SELECT t.install x FROM community;"), vec![]);
        assert_eq!(scan("SELECT install FROM community;"), vec![]);
        assert_eq!(scan(r#"SELECT "install" x FROM community;"#), vec![]);
        assert_eq!(scan("INSTALL x FROM; INSTALL; INSTALL"), vec![]);
        assert_eq!(scan("LOAD mlpack; LOAD '/tmp/x.duckdb_extension';"), vec![]);
        assert_eq!(
            scan(
                "-- INSTALL a FROM community;\n/* INSTALL b FROM community; */ SELECT 'INSTALL c FROM community';"
            ),
            vec![]
        );
    }

    #[test]
    fn scan_finds_a_repository_setting_in_any_form() {
        assert_eq!(
            scan("SET custom_extension_repository = '/tmp/ext'; INSTALL mlpack;"),
            vec![
                ExtensionSql::Setting("custom_extension_repository".into()),
                install("mlpack", InstallFrom::Core)
            ]
        );
        assert_eq!(
            scan(
                r#"PRAGMA Autoinstall_Extension_Repository='x'; SET GLOBAL "custom_extension_repository" TO 'y';"#
            ),
            vec![
                ExtensionSql::Setting("autoinstall_extension_repository".into()),
                ExtensionSql::Setting("custom_extension_repository".into())
            ]
        );
        assert_eq!(
            scan("SELECT current_setting('custom_extension_repository');"),
            vec![]
        );
    }

    #[test]
    fn scan_finds_the_extension_directory_setting_in_any_form() {
        assert_eq!(
            scan(
                r#"SET extension_directory = 'ext'; SET GLOBAL "Extension_Directory" TO 'x'; PRAGMA extension_directory='y';"#
            ),
            vec![ExtensionSql::Setting("extension_directory".into()); 3]
        );
        assert_eq!(
            scan("SELECT current_setting('extension_directory');"),
            vec![]
        );
    }

    #[test]
    fn check_refuses_the_extension_directory_setting_saying_what_it_moves() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[("step 'a'", "SELECT 1;\nSET extension_directory = 'ext';")],
        );
        assert_eq!(
            refusals(check_extension_installs(&sql, Some(&v("1.5.5")))),
            vec![
                "step 'a' (s0.sql, line 2) names the setting extension_directory, which moves where DuckDB keeps the extensions it installs"
            ]
        );
    }

    #[test]
    fn check_lists_each_vetted_community_install_once_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[
                ("step 'a'", "INSTALL onager FROM community; INSTALL excel;"),
                (
                    "step 'b'",
                    "FORCE INSTALL mlpack FROM community; INSTALL onager FROM community;",
                ),
                (
                    "hook on_exit 'h'",
                    "LOAD finetype; INSTALL spatial FROM core;",
                ),
            ],
        );
        assert_eq!(
            check_extension_installs(&sql, Some(&v("1.5.5")))
                .unwrap()
                .installed,
            vec!["onager".to_string(), "mlpack".to_string()]
        );
    }

    // ---- the pins ----

    const PIN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// `extensions:` holding each `(name, version, platform, pin)`.
    fn pins(entries: &[(&str, &str, &str, &str)]) -> ExtensionPins {
        let mut pins = ExtensionPins::new();
        for (name, version, platform, pin) in entries {
            pins.entry(name.to_string())
                .or_default()
                .entry(version.to_string())
                .or_default()
                .insert(platform.to_string(), pin.to_string());
        }
        pins
    }

    /// A file holding `bytes`, and its SHA-256.
    fn installed_file(dir: &Path, name: &str, bytes: &[u8]) -> (PathBuf, String) {
        let path = dir.join(format!("{name}.duckdb_extension"));
        std::fs::write(&path, bytes).unwrap();
        let hash = crate::fetch_cache::hash_file(&path).unwrap();
        (path, hash)
    }

    fn calls(engine: &mock::MockEngine) -> Vec<String> {
        engine
            .calls
            .borrow()
            .iter()
            .map(|c| match c {
                mock::MockCall::Platform => "platform".to_string(),
                mock::MockCall::Install { name } => format!("install {name}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn pin_refusals(result: Result<(Vec<PinnedExtension>, Vec<String>)>) -> Vec<String> {
        match result {
            Err(Error::ExtensionPinRefused { refusals }) => refusals,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_pin_equal_to_the_installed_file_passes_and_is_kept_for_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let (path, hash) = installed_file(dir.path(), "mlpack", b"build one");
        let engine = mock::MockEngine::new();
        engine.set_install("mlpack", Ok(Some(path.clone())));
        let (pinned, warnings) = check_extension_pins(
            &engine,
            &["mlpack".into()],
            &pins(&[("mlpack", "v1.5.5", "linux_amd64", &hash)]),
            Some(&v("1.5.5")),
        )
        .unwrap();
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(
            pinned,
            vec![PinnedExtension {
                name: "mlpack".into(),
                version: "v1.5.5".into(),
                platform: "linux_amd64".into(),
                pin: hash,
                path,
            }]
        );
        assert_eq!(calls(&engine), vec!["platform", "install mlpack"]);
    }

    #[test]
    fn a_pin_that_differs_is_refused_naming_each_part() {
        let dir = tempfile::tempdir().unwrap();
        let (path, hash) = installed_file(dir.path(), "mlpack", b"build two");
        let engine = mock::MockEngine::new();
        engine.set_install("mlpack", Ok(Some(path.clone())));
        let refused = pin_refusals(check_extension_pins(
            &engine,
            &["mlpack".into()],
            &pins(&[("mlpack", "v1.5.5", "linux_amd64", PIN_A)]),
            Some(&v("1.5.5")),
        ));
        assert_eq!(
            refused,
            vec![format!(
                "mlpack on DuckDB v1.5.5, linux_amd64: pinned {PIN_A}, and the installed file {} hashes to {hash}; `arc upgrade mlpack` installs the build the community registry serves and pins it",
                path.display()
            )]
        );
    }

    #[test]
    fn an_install_that_fails_or_names_no_file_is_refused_and_not_warned() {
        let pinned = pins(&[("mlpack", "v1.5.5", "linux_amd64", PIN_A)]);
        let engine = mock::MockEngine::new();
        engine.set_install("mlpack", Err("HTTP 404".into()));
        let failed = pin_refusals(check_extension_pins(
            &engine,
            &["mlpack".into()],
            &pinned,
            Some(&v("1.5.5")),
        ));
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(
            failed[0]
                .starts_with("mlpack on DuckDB v1.5.5, linux_amd64: arc could not install mlpack")
                && failed[0].contains("HTTP 404"),
            "{failed:?}"
        );

        let engine = mock::MockEngine::new();
        engine.set_install("mlpack", Ok(None));
        let no_file = pin_refusals(check_extension_pins(
            &engine,
            &["mlpack".into()],
            &pinned,
            Some(&v("1.5.5")),
        ));
        assert_eq!(
            no_file,
            vec![
                "mlpack on DuckDB v1.5.5, linux_amd64: DuckDB named no installed file for mlpack after installing it"
            ]
        );

        let engine = mock::MockEngine::new();
        engine.set_install(
            "mlpack",
            Ok(Some(PathBuf::from("/nonexistent/mlpack.duckdb_extension"))),
        );
        let unreadable = pin_refusals(check_extension_pins(
            &engine,
            &["mlpack".into()],
            &pinned,
            Some(&v("1.5.5")),
        ));
        assert!(
            unreadable[0]
                .contains("could not read the installed file /nonexistent/mlpack.duckdb_extension"),
            "{unreadable:?}"
        );
    }

    #[test]
    fn a_platform_duckdb_cannot_report_refuses_each_extension_with_a_pin() {
        let engine = mock::MockEngine::new();
        *engine.platform.borrow_mut() = None;
        let refused = pin_refusals(check_extension_pins(
            &engine,
            &["mlpack".into(), "onager".into()],
            &pins(&[("mlpack", "v1.5.5", "linux_amd64", PIN_A)]),
            Some(&v("1.5.5")),
        ));
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused[0]
                .starts_with("mlpack has a pin for DuckDB v1.5.5, and arc could not choose it"),
            "{refused:?}"
        );
        assert_eq!(calls(&engine), vec!["platform"]);
    }

    #[test]
    fn an_extension_with_no_pin_for_this_version_and_platform_warns_once_and_asks_only_what_it_needs()
     {
        let tail = format!(
            ", so arc runs whichever build DuckDB installs; `arc upgrade mlpack` pins the build the community registry serves ({VETTED_EXTENSIONS_DOC})"
        );
        // (label, pins, version, the warning's place, the calls DuckDB receives)
        type Case = (
            &'static str,
            ExtensionPins,
            Option<semver::Version>,
            &'static str,
            Vec<&'static str>,
        );
        let cases: [Case; 4] = [
            (
                "no key",
                pins(&[]),
                Some(v("1.5.5")),
                "for DuckDB v1.5.5",
                vec![],
            ),
            (
                "another version",
                pins(&[("mlpack", "v1.5.4", "linux_amd64", PIN_A)]),
                Some(v("1.5.5")),
                "for DuckDB v1.5.5",
                vec![],
            ),
            (
                "another platform",
                pins(&[("mlpack", "v1.5.5", "osx_arm64", PIN_A)]),
                Some(v("1.5.5")),
                "for DuckDB v1.5.5 on linux_amd64",
                vec!["platform"],
            ),
            (
                "no version",
                pins(&[("mlpack", "v1.5.5", "linux_amd64", PIN_A)]),
                None,
                "arc could not read this engine's DuckDB version to choose one",
                vec![],
            ),
        ];
        for (label, pins, version, place, asked) in cases {
            let engine = mock::MockEngine::new();
            let (pinned, warnings) =
                check_extension_pins(&engine, &["mlpack".into()], &pins, version.as_ref()).unwrap();
            assert!(pinned.is_empty(), "[{label}]");
            assert_eq!(
                warnings,
                vec![format!(
                    "mlpack has no pin under the extensions: key of arcform.yaml ({place}){tail}"
                )],
                "[{label}]"
            );
            assert_eq!(calls(&engine), asked, "[{label}]");
        }
    }

    #[test]
    fn a_pin_for_an_extension_the_sql_does_not_install_is_neither_installed_nor_compared() {
        let engine = mock::MockEngine::new();
        let (pinned, warnings) = check_extension_pins(
            &engine,
            &[],
            &pins(&[("mlpack", "v1.5.5", "linux_amd64", PIN_A)]),
            Some(&v("1.5.5")),
        )
        .unwrap();
        assert!(pinned.is_empty() && warnings.is_empty());
        assert_eq!(calls(&engine), Vec::<String>::new());
    }

    #[test]
    fn the_end_of_the_run_fails_on_a_file_that_changed_and_passes_one_that_did_not() {
        let dir = tempfile::tempdir().unwrap();
        let (path, hash) = installed_file(dir.path(), "mlpack", b"build one");
        let (other, other_hash) = installed_file(dir.path(), "onager", b"build one");
        let pinned = vec![
            PinnedExtension {
                name: "mlpack".into(),
                version: "v1.5.5".into(),
                platform: "linux_amd64".into(),
                pin: hash.clone(),
                path: path.clone(),
            },
            PinnedExtension {
                name: "onager".into(),
                version: "v1.5.5".into(),
                platform: "linux_amd64".into(),
                pin: other_hash,
                path: other,
            },
        ];
        recheck_extension_pins(&pinned).unwrap();
        std::fs::write(&path, b"build one!").unwrap();
        let found = crate::fetch_cache::hash_file(&path).unwrap();
        match recheck_extension_pins(&pinned) {
            Err(Error::ExtensionChanged { changes }) => assert_eq!(
                changes,
                vec![format!(
                    "mlpack on DuckDB v1.5.5, linux_amd64: pinned {hash}, and when the run ended {} hashed to {found}",
                    path.display()
                )]
            ),
            other => panic!("expected the run to fail, got {other:?}"),
        }
    }

    #[test]
    fn scan_finds_a_parser_switch_in_any_form() {
        let switch = |name: &str| ExtensionSql::ParserSwitch(name.into());
        assert_eq!(
            scan("CALL enable_peg_parser();\nFROM Enable_Peg_Parser();"),
            vec![switch("enable_peg_parser"), switch("enable_peg_parser")]
        );
        assert_eq!(
            scan(
                r#"SET allow_parser_override_extension = 'fallback'; PRAGMA ALLOW_PARSER_OVERRIDE_EXTENSION='strict'; SET GLOBAL "allow_parser_override_extension" TO 'default';"#
            ),
            vec![switch("allow_parser_override_extension"); 3]
        );
        // A string DuckDB runs as SQL, with its escapes read.
        assert_eq!(
            scan(
                r"FROM query('FROM ENABLE_PEG_PARSER()'); FROM query(E'FROM enable\x5fpeg_parser()');"
            ),
            vec![switch("enable_peg_parser"); 2]
        );
        // In a comment, inside a longer name, and the function that switches back.
        assert_eq!(
            scan(
                "-- CALL enable_peg_parser();\n/* SET allow_parser_override_extension = 'strict'; */ CALL disable_peg_parser(); SELECT my_enable_peg_parser;"
            ),
            vec![]
        );
    }

    #[test]
    fn scan_gives_each_finding_its_line() {
        assert_eq!(
            scan_extension_sql(
                b"SELECT 1;\n\nINSTALL a FROM community;\nSET custom_extension_repository = 'x';"
            ),
            vec![
                (install("a", InstallFrom::Community), 3),
                (
                    ExtensionSql::Setting("custom_extension_repository".into()),
                    4
                )
            ]
        );
    }

    #[test]
    fn scan_gives_a_finding_the_file_line_of_its_batch_line() {
        assert_eq!(
            scan_extension_sql(
                b"SELECT 1;\n\nINSTALL a\nFROM community; INSTALL\nb FROM community;"
            ),
            vec![
                (install("a", InstallFrom::Community), 3),
                (install("b", InstallFrom::Community), 4)
            ]
        );
    }

    // ---- SQL a step builds or writes while it runs ----

    /// The places in `sql` where it runs or writes SQL that is not in the file, with their
    /// lines.
    fn run_time(sql: &str) -> Vec<(RunTimeSql, usize)> {
        scan_extension_sql(sql.as_bytes())
            .into_iter()
            .filter_map(|(found, line)| match found {
                ExtensionSql::RunTime(shape) => Some((shape, line)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn run_time_reads_a_call_on_anything_but_one_string() {
        let call = |name| vec![(RunTimeSql::Call(name), 1)];
        for sql in [
            "FROM query(getvariable('q'));",
            "FROM query('SELECT ' || '42');",
            "FROM query();",
            "FROM query('SELECT 1', 'x');",
            "FROM main.query(getvariable('q'));",
            "FROM system.main.query(getvariable('q'));",
            "FROM \"query\"(getvariable('q'));",
            "FROM Query ( getvariable('q') );",
            "FROM t, main.query(getvariable('q'));",
        ] {
            assert_eq!(run_time(sql), call("query"), "{sql}");
        }
        assert_eq!(
            run_time("FROM json_execute_serialized_sql(json_serialize_sql('SELECT 1'));"),
            call("json_execute_serialized_sql")
        );
        for sql in [
            "FROM query('SELECT 42');",
            "FROM query(E'SELECT 42');",
            "FROM query($t$SELECT 42$t$);",
            "FROM query('SELECT '\n'42');",
            "SELECT query FROM log;",
            "SELECT count(query) FROM log;",
        ] {
            assert_eq!(run_time(sql), vec![], "{sql}");
        }
    }

    #[test]
    fn run_time_reads_the_calls_a_literal_query_runs_as_calls_in_the_file() {
        for sql in [
            "FROM query('FROM query(getvariable(''q''))');",
            "FROM query($$FROM query(getvariable('q'))$$);",
            "FROM query('FROM query(''FROM query(getvariable(''''q''''))'')');",
            // DuckDB reads a zero-width space in the literal as a space, as in a file.
            "FROM query('FROM\u{200b}query(getvariable(''q''))');",
        ] {
            assert_eq!(run_time(sql), vec![(RunTimeSql::Call("query"), 1)], "{sql}");
        }
        // The line is the call's in the file, not the line inside the literal.
        assert_eq!(
            run_time(
                "SELECT 1;\nFROM query('SELECT 1\n\nUNION ALL FROM query(getvariable(''q''))');"
            ),
            vec![(RunTimeSql::Call("query"), 2)]
        );
        assert_eq!(run_time("FROM query('FROM query(''SELECT 42'')');"), vec![]);
    }

    #[test]
    fn run_time_reads_the_calls_serialized_sql_makes() {
        let json = |name: &str| {
            format!(
                r#"{{"statements":[{{"node":{{"from_table":{{"function":{{"class":"FUNCTION","function_name":"{name}","children":[]}}}}}}}}]}}"#
            )
        };
        // A serialized call to either function, in any letter case and escaped, and text that
        // is not JSON.
        for literal in [
            json("query"),
            json("QUERY"),
            json("json_execute_serialized_sql"),
            json(r"query"),
            "not json".to_string(),
        ] {
            let sql = format!("FROM json_execute_serialized_sql('{literal}');");
            assert_eq!(
                run_time(&sql),
                vec![(RunTimeSql::Call("json_execute_serialized_sql"), 1)],
                "{sql}"
            );
        }
        // Serialized SQL that calls another function, or none, and a name that is not a
        // function's.
        for literal in [
            json("abs"),
            r#"{"statements":[]}"#.to_string(),
            r#"{"alias":"query"}"#.to_string(),
        ] {
            let sql = format!("FROM json_execute_serialized_sql('{literal}');");
            assert_eq!(run_time(&sql), vec![], "{sql}");
        }
        // `query()` runs its literal as SQL, not JSON.
        assert_eq!(
            run_time(&format!("FROM query('{}');", json("query"))),
            vec![]
        );
    }

    #[test]
    fn run_time_reads_a_name_a_column_list_follows_as_no_call() {
        for sql in [
            "CREATE TABLE query (a INT);",
            "CREATE TABLE main.query (a INT);",
            "CREATE TABLE IF NOT EXISTS query (a INT);",
            "INSERT INTO query (a) VALUES (1);",
            "CREATE VIEW query (a) AS SELECT 1;",
            "WITH query (a) AS (SELECT 1) FROM query;",
            "WITH RECURSIVE query (a) AS (SELECT 1) FROM query;",
            "CREATE TABLE json_execute_serialized_sql (a INT);",
        ] {
            assert_eq!(run_time(sql), vec![], "{sql}");
        }
    }

    #[test]
    fn run_time_reads_importing_a_database_and_naming_duckdbrc() {
        for sql in [
            "IMPORT DATABASE 'imp';",
            "import Database 'imp';",
            "PRAGMA import_database('imp');",
            "PRAGMA IMPORT_DATABASE('imp');",
            "PRAGMA \"import_database\"('imp');",
        ] {
            assert_eq!(
                run_time(sql),
                vec![(RunTimeSql::ImportDatabase, 1)],
                "{sql}"
            );
        }
        for sql in [
            "COPY t TO '~/.duckdbrc';",
            "COPY t TO \"~/.duckdbrc\";",
            "SELECT '/Users/me/.DuckDBrc';",
            "SELECT $$.duckdbrc$$;",
        ] {
            assert_eq!(run_time(sql), vec![(RunTimeSql::Duckdbrc, 1)], "{sql}");
        }
        for sql in [
            "EXPORT DATABASE 'imp';",
            "DETACH DATABASE lib;",
            "SELECT 'IMPORT DATABASE imp';",
            "-- IMPORT DATABASE 'imp';",
            "SELECT '~/.duck' || 'dbrc';",
            "SELECT duckdbrc FROM t;",
        ] {
            assert_eq!(run_time(sql), vec![], "{sql}");
        }
    }

    // ---- the check a run makes ----

    #[test]
    fn check_warns_once_per_file_naming_each_line_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[
                (
                    "step 'a'",
                    "IMPORT DATABASE 'imp';\nFROM query(getvariable('q')) UNION ALL FROM query(getvariable('r'));\nCOPY (SELECT 1) TO '~/.duckdbrc';",
                ),
                ("step 'b'", "SELECT 1;"),
                // One batch: the scan gives its call first, and its `.duckdbrc` on an earlier
                // line after it.
                (
                    "step 'c'",
                    "SELECT '~/.duckdbrc'\nUNION ALL FROM query(getvariable('q'));",
                ),
                // One line holding a shape twice with another between.
                (
                    "step 'd'",
                    "IMPORT DATABASE 'a'; SELECT '.duckdbrc'; IMPORT DATABASE 'b';",
                ),
                (
                    "hook on_exit 'h'",
                    "INSTALL mlpack FROM community;\nPRAGMA import_database('imp');",
                ),
            ],
        );
        let tail = format!(
            ". arc does not read the SQL a step builds or writes while it runs, for an extension that SQL installs; running it anyway ({VETTED_EXTENSIONS_DOC})"
        );
        assert_eq!(
            check_extension_installs(&sql, Some(&v("1.5.4")))
                .unwrap()
                .warnings,
            vec![
                format!(
                    "mlpack is vetted on DuckDB v1.5.5, and this engine is DuckDB v1.5.4; running it anyway ({VETTED_EXTENSIONS_DOC})"
                ),
                format!(
                    "step 'a' (s0.sql) can run or write SQL that is not in its file: IMPORT DATABASE on line 1, query() on line 2, .duckdbrc on line 3{tail}"
                ),
                format!(
                    "step 'c' (s2.sql) can run or write SQL that is not in its file: .duckdbrc on line 1, query() on line 2{tail}"
                ),
                format!(
                    "step 'd' (s3.sql) can run or write SQL that is not in its file: IMPORT DATABASE on line 1, .duckdbrc on line 1{tail}"
                ),
                format!(
                    "hook on_exit 'h' (s4.sql) can run or write SQL that is not in its file: IMPORT DATABASE on line 2{tail}"
                ),
            ]
        );
    }

    #[test]
    fn check_refuses_a_protocol_whatever_it_builds_at_run_time() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[
                ("step 'a'", "IMPORT DATABASE 'imp';"),
                ("step 'b'", "INSTALL anofox_forecast FROM community;"),
            ],
        );
        assert_eq!(
            refusals(check_extension_installs(&sql, Some(&v("1.5.5")))),
            vec![
                "step 'b' (s1.sql, line 1) installs anofox_forecast from the community registry, and anofox_forecast is not on the vetted list"
            ]
        );
    }

    /// Each `(place, sql)` written to a file of its own in `dir`.
    fn sources(dir: &Path, files: &[(&str, &str)]) -> Vec<ProtocolSql> {
        files
            .iter()
            .enumerate()
            .map(|(i, (place, sql))| {
                let file = format!("s{i}.sql");
                std::fs::write(dir.join(&file), sql).unwrap();
                ProtocolSql {
                    place: place.to_string(),
                    step: None,
                    path: dir.join(&file),
                    file,
                }
            })
            .collect()
    }

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    fn refusals(result: Result<ExtensionInstalls>) -> Vec<String> {
        match result {
            Err(Error::ExtensionRefused { refusals }) => refusals,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_plain_extension_name_is_letters_digits_and_underscores() {
        for name in ["mlpack", "splink_udfs", "h3", "us_address_standardizer"] {
            assert!(is_plain_extension_name(name), "{name}");
        }
        for name in ["", "x'y", "a.b", "a b", "a;b", "a-b", "ä"] {
            assert!(!is_plain_extension_name(name), "{name:?}");
        }
        for entry in vetted_extensions() {
            assert!(
                is_plain_extension_name(&entry.name),
                "{} on the vetted list is not a plain name",
                entry.name
            );
        }
    }

    #[test]
    fn duckdb_is_not_started_to_install_a_name_that_is_not_plain() {
        match DuckDbEngine.install_community_extension("x'; SELECT 1; --") {
            Err(Error::EngineQuery { what, reason }) => {
                assert!(what.contains("x'; SELECT 1; --"), "{what}");
                assert_eq!(reason, "arc installs an extension by a plain name alone");
            }
            other => panic!("expected the name to be refused, got {other:?}"),
        }
    }

    #[test]
    fn the_list_arc_holds_names_each_extension_once() {
        let names: Vec<&str> = vetted_extensions()
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "a name appears twice: {names:?}");
        assert!(names.contains(&"mlpack"), "{names:?}");
        assert!(!names.contains(&"anofox_forecast"), "{names:?}");
        assert!(
            vetted_extensions()
                .iter()
                .all(|e| !e.duckdb_versions.is_empty()),
            "every entry names a DuckDB version"
        );
    }

    #[test]
    fn check_refuses_each_unvetted_install_with_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[
                ("step 'a'", "INSTALL anofox_forecast FROM community;"),
                (
                    "hook on_init 'h'",
                    "SELECT 1;\nINSTALL mlpack FROM core_nightly;\nINSTALL mlpack FROM 'https://x.org/e';",
                ),
                (
                    "step 'b'",
                    "INSTALL '/tmp/m.duckdb_extension';\nSET custom_extension_repository = '/tmp';\nCALL enable_peg_parser();",
                ),
            ],
        );
        assert_eq!(
            refusals(check_extension_installs(&sql, Some(&v("1.5.5")))),
            vec![
                "step 'a' (s0.sql, line 1) installs anofox_forecast from the community registry, and anofox_forecast is not on the vetted list",
                "hook on_init 'h' (s1.sql, line 2) installs mlpack from the repository core_nightly; a community extension is installed FROM community",
                "hook on_init 'h' (s1.sql, line 3) installs mlpack from the address 'https://x.org/e'",
                "step 'b' (s2.sql, line 1) installs the extension at '/tmp/m.duckdb_extension'",
                "step 'b' (s2.sql, line 2) names the setting custom_extension_repository, which moves where DuckDB installs an extension from",
                "step 'b' (s2.sql, line 3) names enable_peg_parser, which switches DuckDB to a second parser whose comments and strings arc does not read",
            ]
        );
    }

    #[test]
    fn check_refuses_whatever_else_the_protocol_installs() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[
                ("step 'a'", "INSTALL mlpack FROM community;"),
                ("step 'b'", "INSTALL anofox_forecast FROM community;"),
            ],
        );
        assert_eq!(
            refusals(check_extension_installs(&sql, Some(&v("1.5.4")))).len(),
            1
        );
    }

    #[test]
    fn check_passes_core_installs_and_a_missing_file_without_a_word() {
        let dir = tempfile::tempdir().unwrap();
        let mut sql = sources(
            dir.path(),
            &[(
                "step 'a'",
                "INSTALL spatial FROM core; INSTALL excel; LOAD mlpack;",
            )],
        );
        sql.push(ProtocolSql {
            place: "step 'later'".into(),
            step: Some("later".into()),
            file: "later.sql".into(),
            path: dir.path().join("later.sql"),
        });
        assert_eq!(
            check_extension_installs(&sql, Some(&v("1.5.4")))
                .unwrap()
                .warnings,
            Vec::<String>::new()
        );
    }

    #[test]
    fn check_warns_once_per_vetted_extension_on_a_version_its_entry_does_not_name() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[
                ("step 'a'", "INSTALL mlpack FROM community; LOAD mlpack;"),
                ("step 'b'", "FORCE INSTALL mlpack FROM community;"),
                ("step 'c'", "INSTALL h3 FROM community;"),
            ],
        );
        assert_eq!(
            check_extension_installs(&sql, Some(&v("1.5.4")))
                .unwrap()
                .warnings,
            vec![
                format!(
                    "mlpack is vetted on DuckDB v1.5.5, and this engine is DuckDB v1.5.4; running it anyway ({VETTED_EXTENSIONS_DOC})"
                ),
                format!(
                    "h3 is vetted on DuckDB v1.5.5, and this engine is DuckDB v1.5.4; running it anyway ({VETTED_EXTENSIONS_DOC})"
                ),
            ]
        );
        assert_eq!(
            check_extension_installs(&sql, Some(&v("1.5.5")))
                .unwrap()
                .warnings,
            Vec::<String>::new(),
            "an engine the entry names draws no warning"
        );
        assert_eq!(
            check_extension_installs(&sql[..1], None).unwrap().warnings,
            vec![format!(
                "mlpack is vetted on DuckDB v1.5.5, and arc could not read this engine's version; running it anyway ({VETTED_EXTENSIONS_DOC})"
            )]
        );
    }

    #[test]
    fn the_refusal_lists_each_line_and_names_the_page() {
        let message = Error::ExtensionRefused {
            refusals: vec!["step 'a' one".into(), "step 'b' two".into()],
        }
        .to_string();
        assert!(
            message.contains("\n  step 'a' one\n  step 'b' two\n"),
            "{message}"
        );
        assert!(message.contains("docs/VETTED_EXTENSIONS.md"), "{message}");
        assert!(message.contains("no step was run"), "{message}");
    }

    #[test]
    fn protocol_sql_lists_the_sql_steps_then_the_sql_hooks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("arcform.yaml"),
            "name: p\nsteps:\n  - name: one\n    sql: a.sql\n  - name: two\n    command: echo\n  - name: three\n    sql: c.sql\nhooks:\n  on_init:\n    name: i\n    sql: i.sql\n  on_success:\n    name: s\n    command: echo\n  on_failure:\n    name: f\n    sql: f.sql\n  on_exit:\n    name: x\n    sql: x.sql\n",
        )
        .unwrap();
        let manifest = Manifest::load(dir.path()).unwrap();
        let listed: Vec<(String, String, PathBuf)> = protocol_sql(&manifest, dir.path())
            .into_iter()
            .map(|s| (s.place, s.file, s.path))
            .collect();
        let at = |f: &str| dir.path().join(f);
        assert_eq!(
            listed,
            vec![
                ("step 'one'".into(), "a.sql".into(), at("a.sql")),
                ("step 'three'".into(), "c.sql".into(), at("c.sql")),
                ("hook on_init 'i'".into(), "i.sql".into(), at("i.sql")),
                ("hook on_failure 'f'".into(), "f.sql".into(), at("f.sql")),
                ("hook on_exit 'x'".into(), "x.sql".into(), at("x.sql")),
            ]
        );
    }

    // ---- what the run record names ----

    #[test]
    fn the_report_reads_each_tagged_answer_and_skips_the_rest() {
        let answers: Vec<String> = [
            "version\tv1.5.4",
            "platform\tlinux_amd64",
            "extension\tmlpack\tcommunity\tv1.5.5\t/x/mlpack.duckdb_extension",
            // DuckDB gives no repository and no version for a file with no `.info` beside it,
            // and a tab in the path stays in it.
            "extension\thttpfs\t\t\t/x/a\tb/httpfs.duckdb_extension",
            "extension\tjson\tcore\tv1.5.4\t",
            // Too few fields, an answer to another question, and a tag arc does not ask for.
            "extension\tshort\tcore",
            "linux_amd64",
            "other\tvalue",
        ]
        .map(String::from)
        .to_vec();
        let report = read_report(&answers);
        assert_eq!(report.version.as_deref(), Some("v1.5.4"));
        assert_eq!(report.platform.as_deref(), Some("linux_amd64"));
        let expected: HashMap<String, ReportedExtension> = [
            (
                "mlpack",
                ReportedExtension {
                    path: Some("/x/mlpack.duckdb_extension".into()),
                    repository: Some("community".into()),
                    version: Some("v1.5.5".into()),
                },
            ),
            (
                "httpfs",
                ReportedExtension {
                    path: Some("/x/a\tb/httpfs.duckdb_extension".into()),
                    repository: None,
                    version: None,
                },
            ),
            (
                "json",
                ReportedExtension {
                    path: None,
                    repository: Some("core".into()),
                    version: Some("v1.5.4".into()),
                },
            ),
        ]
        .into_iter()
        .map(|(name, ext)| (name.to_string(), ext))
        .collect();
        assert_eq!(report.extensions, expected);

        let empty = ["version\t", "platform\t"].map(String::from).to_vec();
        assert_eq!(read_report(&empty), EngineReport::default());
    }

    #[test]
    fn the_scan_records_each_install_by_name_once_in_the_order_the_protocol_first_installs_it() {
        let dir = tempfile::tempdir().unwrap();
        let sql = sources(
            dir.path(),
            &[
                (
                    "step 'a'",
                    "INSTALL httpfs; INSTALL mlpack FROM community; INSTALL httpfs FROM core;",
                ),
                (
                    "step 'b'",
                    "FORCE INSTALL mlpack FROM community; INSTALL spatial; LOAD json;",
                ),
            ],
        );
        let installs = check_extension_installs(&sql, Some(&v("1.5.5"))).unwrap();
        assert_eq!(installs.recorded, vec!["httpfs", "mlpack", "spatial"]);
        assert_eq!(installs.installed, vec!["mlpack"]);
    }

    #[test]
    fn the_check_before_a_file_runs_records_what_it_installs_that_no_check_found() {
        let dir = tempfile::tempdir().unwrap();
        let before = sources(dir.path(), &[("step 'a'", "INSTALL httpfs;")]);
        let installs = check_extension_installs(&before, Some(&v("1.5.5"))).unwrap();
        let mut recheck = ExtensionRecheck::new(
            installs,
            Vec::new(),
            Vec::new(),
            &ExtensionPins::new(),
            Some(&v("1.5.5")),
        );
        assert_eq!(recheck.recorded(), ["httpfs"]);
        let engine = mock::MockEngine::new();

        // A file the check refuses ran nothing, and adds nothing.
        let refused = sources(
            dir.path(),
            &[("step 'b'", "INSTALL excel; INSTALL nope FROM community;")],
        );
        assert!(recheck.check(&engine, &refused[0]).is_err());
        assert_eq!(recheck.recorded(), ["httpfs"]);

        let passed = sources(
            dir.path(),
            &[(
                "step 'c'",
                "INSTALL httpfs; INSTALL excel; INSTALL mlpack FROM community;",
            )],
        );
        recheck.check(&engine, &passed[0]).unwrap();
        assert_eq!(recheck.recorded(), ["httpfs", "excel", "mlpack"]);
    }
}
