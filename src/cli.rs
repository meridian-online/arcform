use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;

use crate::edit::{IGNORE_FILENAME, IgnoreList, write_ignore_list};
use crate::engine::{ALLOW_UNTESTED_ENGINE_ENV, DuckDbEngine, Engine};
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::registry::transport::GitTarballTransport;
use crate::registry::{RunOptions, cache_root};
use crate::spec::{HistoryKind, HistoryWay, LocalHistory, PathPart, SpecEdit};
use crate::state::DuckDbStateBackend;

/// Default index URL — points to the (future) meridian-online/registry repo.
/// Override via `$ARCFORM_REGISTRY_INDEX` for testing or contributor mirrors.
const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/meridian-online/registry/main/registry.yaml";

const INDEX_URL_ENV: &str = "ARCFORM_REGISTRY_INDEX";
const VERBOSE_ENV: &str = "ARCFORM_VERBOSE";

#[derive(Parser)]
#[command(name = "arc", version, about = "Local-first data pipeline engine")]
pub struct Cli {
    /// Verbose output (firehose). Also enabled via $ARCFORM_VERBOSE.
    #[arg(long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new ArcForm project.
    Init {
        /// Project name (used as directory name).
        name: String,

        /// Generate a runnable Protocol from a Frictionless Data Package
        /// descriptor (semantic-typed fields + discovered foreignKeys) instead
        /// of an empty scaffold. `arc run` then runs the GENERATED manifest.
        #[arg(long = "from-descriptor", value_name = "DATAPACKAGE_JSON")]
        from_descriptor: Option<PathBuf>,
    },
    /// Create a new protocol: write `<DIR>/arcform.yaml` from scratch,
    /// through the same validation gate `arc run` loads with. Refuses if a
    /// spec already exists there — an existing spec may be hand-authored,
    /// and changes to it go through `edit-protocol`.
    CreateProtocol {
        /// Directory the protocol lives in (created if absent).
        dir: PathBuf,

        /// Protocol name. Defaults to the directory's file name.
        #[arg(long)]
        name: Option<String>,

        /// Engine CLI identifier (default: duckdb).
        #[arg(long)]
        engine: Option<String>,

        /// Database file path, relative to the protocol directory. Written to
        /// `arcform.yaml` as `db:` only when passed; without it the database is
        /// `<name>.duckdb` in arc's data folder (`$ARCFORM_DB_DIR`, else
        /// `~/.arcform/db`), keyed to the protocol directory, and not in the directory.
        #[arg(long)]
        db: Option<String>,
    },

    /// Edit an existing protocol's arcform.yaml in place. The edit is applied
    /// to the original bytes, validated by the same loader `arc run` uses,
    /// and written atomically — or refused with the reason, leaving the file
    /// untouched. Every byte the edit does not target is preserved verbatim:
    /// comments, key order, blank lines and quote style all survive, and no
    /// verb reformats the document as a side effect.
    EditProtocol {
        /// Protocol directory (where arcform.yaml lives).
        #[arg(long, default_value = ".")]
        dir: PathBuf,

        #[command(subcommand)]
        op: EditOp,
    },

    /// Local history for a protocol's spec, or for one file beside it: list,
    /// inspect and restore earlier states of `arcform.yaml`, or with
    /// `--file <FILE>` of that one file — no git repository or account
    /// required.
    ///
    /// The middle tier between editor undo and version control: saving
    /// records an entry and every machine edit checkpoints the state it is
    /// about to replace, into `$ARCFORM_HISTORY_DIR` (default
    /// `~/.arcform/history`) — outside the protocol directory, invisible to
    /// `git status`. Each file keeps a history of its own: `--file
    /// panels/a.yaml` lists, shows and restores that file's entries and no
    /// other's, and a relative `--file` is read from `--dir`. At most 50
    /// entries are kept per spec and per file, oldest pruned first, and
    /// saves within 10 seconds of the newest save merge into it. Nothing is
    /// ever promoted to git.
    History {
        #[command(subcommand)]
        cmd: HistoryCmd,
    },

    /// Run the pipeline defined in arcform.yaml.
    Run {
        /// Force re-execution of all steps, ignoring staleness.
        #[arg(long)]
        force: bool,

        /// Set a runtime parameter (repeatable). Format: KEY=VALUE.
        /// Overrides dotenv and manifest defaults.
        #[arg(long = "param", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },

    /// Pin the build of a community extension on the vetted list that the community
    /// registry serves, for this machine's DuckDB version and platform. arc installs that
    /// build over any file DuckDB holds for the extension, loads it, and writes the
    /// installed file's SHA-256 under the `extensions:` key of arcform.yaml, replacing the
    /// pin there was for this version and platform. Every other byte of the file is kept,
    /// and the file as it was is a checkpoint in `arc history`. A Protocol that runs on
    /// several platforms is upgraded on each.
    Upgrade {
        /// The extension, as the vetted list names it: `mlpack`.
        name: String,

        /// Protocol directory (where arcform.yaml lives).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },

    /// Discover, fetch, and run curated registry pipelines.
    Registry {
        #[command(subcommand)]
        cmd: RegistryCmd,
    },

    /// List the SQL operations arc holds, describe what one takes, and record
    /// one as a step of a protocol.
    Operation {
        #[command(subcommand)]
        cmd: OperationCmd,
    },

    /// Serve a Model Context Protocol server over stdio, for AI-agent and editor
    /// integration. Federates the `finetype` CLI as tools (infer / profile / taxonomy
    /// / validate / generate) and adds `protocol_run` (run a Protocol, return its
    /// Protocol+Run contract), `operator_describe` (an operator's `with:` schema),
    /// `operation_describe` (the SQL operations arc holds, and what one takes) and
    /// `operation_record` (record an operation as a step of a Protocol).
    #[cfg(feature = "mcp")]
    Mcp,
}

/// The operation-catalogue verbs. Long names come from `arc operation list`.
#[derive(Subcommand)]
pub enum OperationCmd {
    /// Print the long name of each operation arc holds, with one line saying
    /// what it does.
    List {
        /// Print a JSON array of `long_name` and `summary` instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Print what an operation or an operator takes, as JSON: its name, what it
    /// does, what its step reads and writes, and a JSON Schema of its parameters.
    Describe {
        /// The operation's long name, as listed by `arc operation list`, or an
        /// operator's name, as a step's `op:` names it.
        long_name: String,
    },
    /// Record an operation as a new step at the end of a protocol: a generated
    /// model under `models/` and a step in arcform.yaml naming it. Runs nothing;
    /// `arc run` runs the step.
    Record {
        /// The operation's long name, as listed by `arc operation list`.
        long_name: String,

        /// The table the operation is applied to, one a step of the protocol makes.
        #[arg(long)]
        on: String,

        /// The new step's name, which is also the name of the table it makes.
        #[arg(long)]
        name: String,

        /// An argument of the operation (repeatable), as `arc operation describe`
        /// lists them. Format: KEY=VALUE.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,

        /// Protocol directory (where arcform.yaml lives).
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum RegistryCmd {
    /// List the entries available in the registry, grouped by pillar.
    List {
        /// Force a fresh fetch of the index regardless of TTL.
        #[arg(long)]
        refresh: bool,
    },
    /// Show metadata + README for a single entry.
    Show {
        name: String,
        #[arg(long)]
        refresh: bool,
    },
    /// Fetch an entry into the local cache without running it.
    Fetch {
        name: String,
        #[arg(long, conflicts_with = "latest")]
        version: Option<String>,
        #[arg(long, conflicts_with = "version")]
        latest: bool,
        #[arg(long)]
        refresh: bool,
    },
    /// Fetch (if needed) and run an entry's pipeline.
    Run {
        name: String,
        #[arg(long, conflicts_with = "latest")]
        version: Option<String>,
        #[arg(long, conflicts_with = "version")]
        latest: bool,
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        force: bool,
        #[arg(long = "param", value_name = "KEY=VALUE")]
        params: Vec<String>,
    },
}

/// One edit to a protocol spec, addressed by PATH: dot-separated mapping keys
/// with zero-based `[N]` sequence indices — `steps[2].command`,
/// `params.port.default` — and `.` alone for the manifest root.
///
/// Replacement text is spliced verbatim, exactly as the library's write path
/// takes it: multi-line values must arrive already indented for the position
/// they land in, and a sequence item carries its own `- ` line(s). Nothing
/// here reformats on the caller's behalf — that is what keeps the rest of the
/// document byte-identical.
#[derive(Subcommand)]
pub enum EditOp {
    /// Replace the value at PATH with VALUE. For `key: value` pairs the key,
    /// separator and any trailing same-line comment are untouched.
    Replace {
        path: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Rewrite one occurrence of FROM to TO inside the value at PATH — the
    /// way to touch three characters of a 20-line `command: |` block without
    /// owning the other lines. Refused unless FROM occurs exactly once there.
    Rewrite {
        path: String,
        #[arg(allow_hyphen_values = true)]
        from: String,
        #[arg(allow_hyphen_values = true)]
        to: String,
    },
    /// Add `KEY: VALUE` to the mapping at PATH (`.` for the manifest root).
    Add {
        path: String,
        key: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Append ITEM to the sequence at PATH. For a block sequence the item
    /// text carries its own pre-indented `- ` line(s).
    Append {
        path: String,
        #[arg(allow_hyphen_values = true)]
        item: String,
    },
    /// Delete the element at PATH, along with its flush comment header.
    Delete { path: String },
    /// Move the item at index FROM of the block sequence at PATH so it ends
    /// up at index TO.
    Reorder {
        path: String,
        from: usize,
        to: usize,
    },
}

/// The local-history verbs. Entry ids come from `arc history list`, and a
/// file's ids from `arc history list --file` for that same file.
#[derive(Subcommand)]
pub enum HistoryCmd {
    /// List the recorded states of the protocol's spec, or with `--file` of
    /// that one file, oldest first, with the retention policy that governs
    /// them.
    List {
        /// Protocol directory (where arcform.yaml lives).
        #[arg(long, default_value = ".")]
        dir: PathBuf,

        /// List this file's own history instead of the spec's — a chart file
        /// beside the spec, say. A relative path is read from `--dir`.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Print the exact bytes an entry recorded, to stdout.
    Show {
        /// The entry id, as listed by `arc history list`.
        id: String,

        /// Protocol directory (where arcform.yaml lives).
        #[arg(long, default_value = ".")]
        dir: PathBuf,

        /// Read the entry from this file's own history instead of the
        /// spec's. A relative path is read from `--dir`.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Roll the spec, or with `--file` that one file, back to the state an
    /// entry recorded. The state being replaced is checkpointed first, so a
    /// restore can itself be undone.
    Restore {
        /// The entry id, as listed by `arc history list`.
        id: String,

        /// Protocol directory (where arcform.yaml lives).
        #[arg(long, default_value = ".")]
        dir: PathBuf,

        /// Restore this file from its own history instead of the spec; no
        /// other file is written. A relative path is read from `--dir`.
        #[arg(long)]
        file: Option<PathBuf>,
    },
}

/// Execute the `arc create-protocol` command: assemble the manifest value the
/// arguments describe and hand it to the library's creation path — the same
/// gate and the same atomic write every other caller gets. Nothing is written
/// unless the result loads; an existing spec is refused, never overwritten.
pub fn create_protocol(
    dir: &Path,
    name: Option<String>,
    engine: Option<String>,
    db: Option<String>,
    history: &LocalHistory,
) -> Result<()> {
    let name = match name {
        Some(name) => name,
        None => dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| {
                Error::ManifestValidation(
                    "cannot derive a protocol name from that directory; pass --name".to_string(),
                )
            })?,
    };

    let mut manifest = Manifest::new_project(&name);
    if let Some(engine) = engine {
        manifest.engine = engine;
    }
    if let Some(db) = db {
        manifest.db = Some(db);
    }
    let validated = crate::spec::create_spec(dir, &manifest)?;

    // The new spec's first durable state enters local history at its save
    // boundary. Best-effort by design: the state is also the file itself, so
    // a store that cannot record costs nothing here — and creation, which
    // replaces nothing, must not fail on the safety net's account.
    let _ = history.record_save(dir, validated.text());

    // Written after the spec, so a refused or invalid manifest leaves nothing beside
    // it, and an ignore list the author wrote first is kept.
    let ignore = write_ignore_list(dir, manifest.db.as_deref())?;

    println!(
        "created {} — author steps with `arc edit-protocol`",
        dir.join(crate::spec::MANIFEST_FILENAME).display()
    );
    if ignore == IgnoreList::Written {
        println!("created {}", dir.join(IGNORE_FILENAME).display());
    }
    Ok(())
}

/// Execute the `arc edit-protocol` command: turn the op into the library's
/// edit value and hand it to the whole write path on its checkpointed road —
/// apply, validate, checkpoint the state being replaced, write atomically.
/// The CLI's job ends at argument parsing; a refusal (an edit that does not
/// apply, a result the loader rejects, or a checkpoint that cannot land)
/// surfaces here as the error, before the file is touched.
pub fn edit_protocol(dir: &Path, op: EditOp, history: &LocalHistory) -> Result<()> {
    let edit = spec_edit_of(op)?;
    crate::spec::edit_spec_with_history(dir, &[edit], history)?;
    println!(
        "edited {}",
        dir.join(crate::spec::MANIFEST_FILENAME).display()
    );
    Ok(())
}

/// The op as the library's edit value. Pure argument handling: PATH parsing
/// plus a field-for-field mapping — no edit logic lives on this side.
fn spec_edit_of(op: EditOp) -> Result<SpecEdit> {
    Ok(match op {
        EditOp::Replace { path, value } => SpecEdit::Replace {
            path: parse_path(&path)?,
            value,
        },
        EditOp::Rewrite { path, from, to } => SpecEdit::RewriteFragment {
            path: parse_path(&path)?,
            from,
            to,
        },
        EditOp::Add { path, key, value } => SpecEdit::Add {
            path: parse_path(&path)?,
            key,
            value,
        },
        EditOp::Append { path, item } => SpecEdit::Append {
            path: parse_path(&path)?,
            item,
        },
        EditOp::Delete { path } => SpecEdit::Delete {
            path: parse_path(&path)?,
        },
        EditOp::Reorder { path, from, to } => SpecEdit::Reorder {
            path: parse_path(&path)?,
            from,
            to,
        },
    })
}

/// `steps[2].command` → the write path's address parts; `.` alone is the
/// manifest root (an empty path — only `add` accepts one). A path whose
/// syntax does not parse is refused here, before any file is read.
fn parse_path(raw: &str) -> Result<Vec<PathPart>> {
    let syntax = |detail: String| Error::EditTarget {
        path: raw.to_string(),
        detail,
    };
    if raw == "." {
        return Ok(Vec::new());
    }
    if raw.is_empty() {
        return Err(syntax(
            "empty path (use `.` for the manifest root)".to_string(),
        ));
    }
    let mut parts = Vec::new();
    for segment in raw.split('.') {
        if segment.is_empty() {
            return Err(syntax("empty path segment".to_string()));
        }
        let (key, mut rest) = match segment.find('[') {
            Some(0) => (None, segment),
            Some(at) => (Some(&segment[..at]), &segment[at..]),
            None => (Some(segment), ""),
        };
        if let Some(key) = key {
            parts.push(PathPart::Key(key.to_string()));
        }
        while !rest.is_empty() {
            let (digits, tail) = rest
                .strip_prefix('[')
                .and_then(|r| r.split_once(']'))
                .ok_or_else(|| syntax(format!("malformed index in segment {segment:?}")))?;
            let index: usize = digits
                .parse()
                .map_err(|_| syntax(format!("index {digits:?} is not a number")))?;
            parts.push(PathPart::Index(index));
            rest = tail;
        }
    }
    Ok(parts)
}

/// The path `--file` names: a relative path is read from the protocol
/// directory, the way `--dir` scopes everything else a history verb touches,
/// and an absolute one is taken as it stands.
fn history_file(dir: &Path, file: &Path) -> PathBuf {
    dir.join(file)
}

/// Execute `arc history list`: the recorded states of the spec in `dir`, or
/// given `file` of that one file, oldest first (the newest lands beside the
/// prompt), with the retention policy printed under the entries it governs —
/// a user should never have to hunt for the rules deciding what this command
/// shows. Each state's line ends with the way arc was reached when it was
/// written, or `not recorded` for a state written without one; last, so the
/// columns before it stand where they always have.
pub fn history_list(
    dir: &Path,
    file: Option<&Path>,
    history: &LocalHistory,
    out: &mut impl Write,
) -> Result<()> {
    let (subject, noun, entries, restore) = match file {
        None => (
            dir.join(crate::spec::MANIFEST_FILENAME),
            "spec",
            history.entries(dir)?,
            "`arc history restore <id>`".to_string(),
        ),
        Some(file) => {
            let path = history_file(dir, file);
            let entries = history.entries_for_file(&path)?;
            let restore = format!("`arc history restore <id> --file {}`", file.display());
            (path, "file", entries, restore)
        }
    };
    if entries.is_empty() {
        writeln!(
            out,
            "no local history for {} yet — entries are recorded as the {noun} is saved or machine-edited",
            subject.display()
        )?;
    } else {
        for entry in &entries {
            writeln!(
                out,
                "{}  {:<10}  {}  {:>7} bytes  {}",
                entry.id,
                kind_word(entry.kind),
                humantime::format_rfc3339_seconds(entry.at),
                entry.bytes,
                entry
                    .way
                    .as_ref()
                    .map_or("not recorded", HistoryWay::as_str),
            )?;
        }
        writeln!(
            out,
            "({} recorded state(s), newest last — {restore} rolls back)",
            entries.len()
        )?;
    }
    writeln!(out, "{}", crate::history::policy_line(history.root()))?;
    Ok(())
}

/// Execute `arc history show`: the exact recorded bytes and nothing else —
/// the output is the historical spec, or given `file` that file's historical
/// text, fit for a diff or a redirect.
pub fn history_show(
    dir: &Path,
    file: Option<&Path>,
    id: &str,
    history: &LocalHistory,
    out: &mut impl Write,
) -> Result<()> {
    let text = match file {
        None => history.read(dir, id)?,
        Some(file) => history.read_for_file(&history_file(dir, file), id)?,
    };
    write!(out, "{text}")?;
    Ok(())
}

/// Execute `arc history restore`: roll the spec, or given `file` that one
/// file, back to a recorded state. The library checkpoints the state being
/// replaced first — a restore is a machine write like any other — and
/// recovery is byte-faithful rather than gated, so the one thing left to
/// check is whether a restored spec still loads; when it does not, that is
/// said out loud instead of silently handing back a spec `arc run` will
/// refuse. A file that is not the spec is not a spec, and is not asked to
/// load as one.
pub fn history_restore(
    dir: &Path,
    file: Option<&Path>,
    id: &str,
    history: &LocalHistory,
    out: &mut impl Write,
) -> Result<()> {
    let (target, text) = match file {
        None => (
            dir.join(crate::spec::MANIFEST_FILENAME),
            history.restore(dir, id)?,
        ),
        Some(file) => {
            let path = history_file(dir, file);
            let text = history.restore_for_file(&path, id)?;
            (path, text)
        }
    };
    writeln!(
        out,
        "restored {} to {id} — the replaced state was checkpointed first",
        target.display()
    )?;
    let is_spec = target.file_name() == Some(crate::spec::MANIFEST_FILENAME.as_ref());
    if is_spec && let Err(e) = Manifest::from_yaml_str(&text) {
        writeln!(out, "note: the restored state does not load as a spec: {e}")?;
    }
    Ok(())
}

fn kind_word(kind: HistoryKind) -> &'static str {
    match kind {
        HistoryKind::Save => "save",
        HistoryKind::Checkpoint => "checkpoint",
    }
}

/// Execute the `arc init` command in the current directory.
pub fn init(name: &str) -> Result<()> {
    init_at(name, &PathBuf::from("."))
}

/// Execute the `arc init` command in a given base directory.
/// Separated from `init` for testability — tests pass a tempdir as `base`.
pub fn init_at(name: &str, base: &std::path::Path) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::ManifestValidation(
            "project name cannot be empty".to_string(),
        ));
    }

    let project_dir = base.join(name);
    if project_dir.exists() {
        return Err(Error::ProjectExists(project_dir));
    }

    fs::create_dir_all(project_dir.join("models"))?;
    fs::create_dir_all(project_dir.join("sources"))?;

    let manifest = Manifest::new_project(name);
    let yaml = serde_yaml::to_string(&manifest).expect("failed to serialize manifest");
    fs::write(project_dir.join("arcform.yaml"), yaml)?;
    write_ignore_list(&project_dir, manifest.db.as_deref())?;

    println!("Initialized project '{}' with:", name);
    println!("  arcform.yaml");
    println!("  models/");
    println!("  sources/");
    println!("  {IGNORE_FILENAME}");
    println!();
    println!("For a complete, runnable example — SQL + command steps, preconditions,");
    println!("retries, and parameters — see examples/brewtrend in the arcform repo.");

    Ok(())
}

/// Execute the `arc run` command.
pub fn run_pipeline(force: bool, raw_params: &[String]) -> Result<()> {
    let cli_params = crate::runner::parse_params(raw_params)?;
    let cwd = std::env::current_dir()?;
    let manifest = Manifest::load(&cwd)?;
    let engine = DuckDbEngine;
    let state = DuckDbStateBackend::for_protocol(&manifest, &cwd)?;
    crate::runner::run_with_params(&cwd, &engine, &state, force, &cli_params)
}

/// Execute `arc upgrade <name>`: refuse a name off the vetted list before DuckDB is asked
/// anything, check the engine's version as `arc run` does, install and load the build of
/// `name` the community registry serves, and write its SHA-256 as the pin for this DuckDB
/// version and platform through the checkpointed write path. Each refusal comes before the
/// write, so arcform.yaml is as it was; a pin that holds the hash already is not rewritten.
pub fn upgrade_extension(
    dir: &Path,
    name: &str,
    engine: &dyn Engine,
    history: &LocalHistory,
) -> Result<()> {
    let manifest = Manifest::load(dir)?;
    let entry = crate::engine::vetted_extensions()
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| Error::ExtensionNotVetted {
            name: name.to_string(),
        })?;
    let failed = |reason: String| Error::ExtensionUpgradeFailed {
        name: name.to_string(),
        reason,
    };
    let Some(found) = engine.preflight()?.version else {
        return Err(failed(
            "arc could not read this engine's DuckDB version, which a pin is for".to_string(),
        ));
    };
    let allow_untested = std::env::var_os(ALLOW_UNTESTED_ENGINE_ENV).is_some_and(|v| !v.is_empty());
    let warnings = crate::runner::check_engine_version(
        Some(&found),
        manifest.engine_version.as_deref(),
        allow_untested,
    )?;
    let vetted_on = crate::engine::unvetted_version_warning(entry, Some(&found));
    for warning in warnings.iter().chain(vetted_on.iter()) {
        eprintln!("{} {}", "warning:".yellow(), warning);
    }
    let platform = engine
        .extension_platform()
        .map_err(|e| failed(e.to_string()))?;
    let path = engine
        .force_install_community_extension(name)
        .map_err(|e| failed(e.to_string()))?
        .ok_or_else(|| {
            failed(format!(
                "DuckDB named no installed file for {name} after installing it"
            ))
        })?;
    let hash = crate::fetch_cache::hash_file(&path).map_err(|e| {
        failed(format!(
            "arc could not read the installed file {}: {e}",
            path.display()
        ))
    })?;

    let version = format!("v{found}");
    let at = format!("{name} on DuckDB {version}, {platform}");
    let spec = dir.join(crate::spec::MANIFEST_FILENAME);
    let old = manifest
        .extensions
        .get(name)
        .and_then(|versions| versions.get(&version))
        .and_then(|platforms| platforms.get(&platform));
    if old == Some(&hash) {
        println!(
            "{at}: pinned {hash} already, the build the community registry serves; {} is unchanged",
            spec.display()
        );
        return Ok(());
    }
    let text = fs::read_to_string(&spec).map_err(|e| Error::FileRead {
        path: spec.clone(),
        source: e,
    })?;
    let edits = crate::edit::scalar_edits(&text, &["extensions", name, &version, &platform], &hash);
    crate::spec::edit_spec_with_history(dir, &edits, history)?;
    match old {
        Some(old) => println!(
            "{at}: replaced the pin {old} with {hash}, the build the community registry serves, in {}",
            spec.display()
        ),
        None => println!(
            "{at}: pinned {hash}, the build the community registry serves, in {}",
            spec.display()
        ),
    }
    Ok(())
}

/// The local-history store as every verb of the command line opens it: the
/// conventional root, reached by [`HistoryWay::TERMINAL`]. Every version a
/// verb writes — `create-protocol`'s first save, `edit-protocol`'s and
/// `upgrade`'s checkpoint and save, a recording's two, and the checkpoint a
/// restore takes — carries the way from here, so none can name another.
fn open_history() -> Result<LocalHistory> {
    Ok(LocalHistory::open_default()?.reached_by(HistoryWay::TERMINAL))
}

/// Dispatch CLI commands.
pub fn dispatch(cli: Cli) -> Result<()> {
    let verbose = cli.verbose || std::env::var_os(VERBOSE_ENV).is_some();
    match cli.command {
        Commands::Init {
            name,
            from_descriptor,
        } => match from_descriptor {
            Some(path) => crate::bridge::init_from_descriptor(&name, &path),
            None => init(&name),
        },
        Commands::CreateProtocol {
            dir,
            name,
            engine,
            db,
        } => create_protocol(&dir, name, engine, db, &open_history()?),
        Commands::EditProtocol { dir, op } => edit_protocol(&dir, op, &open_history()?),
        Commands::History { cmd } => dispatch_history(cmd),
        Commands::Run { force, params } => run_pipeline(force, &params),
        Commands::Upgrade { name, dir } => {
            upgrade_extension(&dir, &name, &DuckDbEngine, &open_history()?)
        }
        Commands::Registry { cmd } => dispatch_registry(cmd, verbose),
        Commands::Operation { cmd } => dispatch_operation(cmd, &mut std::io::stdout()),
        #[cfg(feature = "mcp")]
        Commands::Mcp => crate::mcp::serve(),
    }
}

/// Execute an `arc operation` verb. `list` and `describe` read no protocol and
/// open no database: the answer comes from the catalogue arc holds. `record`
/// writes a step into a protocol and opens no database either.
fn dispatch_operation(cmd: OperationCmd, out: &mut impl Write) -> Result<()> {
    match cmd {
        OperationCmd::List { json } => operation_list(json, out),
        OperationCmd::Describe { long_name } => operation_describe(&long_name, out),
        OperationCmd::Record {
            long_name,
            on,
            name,
            args,
            dir,
        } => {
            let arguments = operation_arguments(&args)?;
            let history = open_history()?;
            operation_record(&dir, &long_name, &on, &name, &arguments, &history, out)
        }
    }
}

/// Execute `arc operation list`: each long name with the one line saying what
/// the operation does, or the same as a JSON array of `long_name` and `summary`.
fn operation_list(json: bool, out: &mut impl Write) -> Result<()> {
    let operations = crate::record::operations();
    if json {
        let entries: Vec<serde_json::Value> = operations.iter().map(|op| op.listing()).collect();
        writeln!(out, "{:#}", serde_json::Value::Array(entries))?;
        return Ok(());
    }
    let width = operations
        .iter()
        .map(|op| op.long_name.len())
        .max()
        .unwrap_or(0);
    for op in operations {
        writeln!(out, "{:<width$}  {}", op.long_name, op.summary)?;
    }
    Ok(())
}

/// Execute `arc operation describe`: the description of the operation or the
/// operator called `name`, as JSON. A name the build holds neither of is refused
/// with nothing written to `out`.
fn operation_describe(name: &str, out: &mut impl Write) -> Result<()> {
    let Some(description) = crate::record::describe(name) else {
        return Err(Error::Io(std::io::Error::other(format!(
            "no operation or operator called `{name}` — `arc operation list` prints the operations arc holds"
        ))));
    };
    writeln!(out, "{description:#}")?;
    Ok(())
}

/// Execute `arc operation record`: hand the request to the record path, which
/// writes the step's SQL from the operation's catalogue entry. Nothing here
/// names an operation or an argument. A refusal is the error, with nothing
/// written to the protocol or to `out`.
fn operation_record(
    dir: &Path,
    long_name: &str,
    on: &str,
    name: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    history: &LocalHistory,
    out: &mut impl Write,
) -> Result<()> {
    let model = crate::record::record_operation(dir, long_name, on, name, arguments, history)?;
    writeln!(
        out,
        "recorded step {name} as {} in {} — `arc run` runs it",
        model.display(),
        dir.join(crate::spec::MANIFEST_FILENAME).display()
    )?;
    Ok(())
}

/// The `--arg KEY=VALUE` values as the operation's arguments, each a string.
/// An entry without `=` is refused, and so is a key given twice, rather than
/// one value silently winning.
fn operation_arguments(args: &[String]) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut arguments = serde_json::Map::new();
    for entry in args {
        let Some((key, value)) = entry.split_once('=') else {
            return Err(Error::Io(std::io::Error::other(format!(
                "`--arg {entry}` must be KEY=VALUE"
            ))));
        };
        if arguments
            .insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            )
            .is_some()
        {
            return Err(Error::Io(std::io::Error::other(format!(
                "`--arg {key}` is given more than once"
            ))));
        }
    }
    Ok(arguments)
}

fn dispatch_history(cmd: HistoryCmd) -> Result<()> {
    let history = open_history()?;
    let mut stdout = std::io::stdout();
    match cmd {
        HistoryCmd::List { dir, file } => {
            history_list(&dir, file.as_deref(), &history, &mut stdout)
        }
        HistoryCmd::Show { id, dir, file } => {
            history_show(&dir, file.as_deref(), &id, &history, &mut stdout)
        }
        HistoryCmd::Restore { id, dir, file } => {
            history_restore(&dir, file.as_deref(), &id, &history, &mut stdout)
        }
    }
}

fn index_url() -> String {
    std::env::var(INDEX_URL_ENV).unwrap_or_else(|_| DEFAULT_INDEX_URL.to_string())
}

fn dispatch_registry(cmd: RegistryCmd, verbose: bool) -> Result<()> {
    let root = cache_root()?;
    let url = index_url();
    let transport = GitTarballTransport;

    match cmd {
        RegistryCmd::List { refresh } => {
            let opts = RunOptions {
                transport: &transport,
                cache_root: root,
                index_url: url,
                refresh,
                verbose,
            };
            let mut stdout = std::io::stdout();
            crate::registry::handle_list(&opts, &mut stdout)
        }
        RegistryCmd::Show { name, refresh } => {
            let opts = RunOptions {
                transport: &transport,
                cache_root: root,
                index_url: url,
                refresh,
                verbose,
            };
            let mut stdout = std::io::stdout();
            crate::registry::handle_show(&opts, &name, &mut stdout)
        }
        RegistryCmd::Fetch {
            name,
            version,
            latest,
            refresh,
        } => {
            let opts = RunOptions {
                transport: &transport,
                cache_root: root,
                index_url: url,
                refresh,
                verbose,
            };
            let mut stdout = std::io::stdout();
            let mut stderr = std::io::stderr();
            crate::registry::handle_fetch(&opts, &name, version, latest, &mut stdout, &mut stderr)
        }
        RegistryCmd::Run {
            name,
            version,
            latest,
            refresh,
            force,
            params,
        } => {
            let opts = RunOptions {
                transport: &transport,
                cache_root: root,
                index_url: url,
                refresh,
                verbose,
            };
            crate::registry::handle_run(&opts, &name, version, latest, force, &params)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `arc init` creates arcform.yaml, models/, sources/.
    #[test]
    fn test_init_creates_project_structure() {
        let base = tempfile::tempdir().unwrap();
        init_at("my-project", base.path()).unwrap();

        let project = base.path().join("my-project");
        assert!(
            project.join("arcform.yaml").is_file(),
            "arcform.yaml should exist"
        );
        assert!(project.join("models").is_dir(), "models/ should exist");
        assert!(project.join("sources").is_dir(), "sources/ should exist");
    }

    // Generated manifest has correct defaults.
    #[test]
    fn test_init_manifest_defaults() {
        let base = tempfile::tempdir().unwrap();
        init_at("analytics", base.path()).unwrap();

        let yaml_path = base.path().join("analytics/arcform.yaml");
        let content = fs::read_to_string(&yaml_path).unwrap();
        let manifest: Manifest = serde_yaml::from_str(&content).unwrap();

        assert_eq!(manifest.name, "analytics");
        assert_eq!(manifest.engine, "duckdb");
        assert_eq!(manifest.db, None, "arc init writes no db: line");
        assert!(
            !content.lines().any(|l| l.starts_with("db")),
            "arc init writes no db: line:\n{content}"
        );
        let db = manifest.db_path(&base.path().join("analytics")).unwrap();
        assert!(
            !db.starts_with(base.path().canonicalize().unwrap()),
            "and its database is not beside the manifest: {}",
            db.display()
        );
        assert!(db.ends_with("analytics.duckdb"), "{}", db.display());
        assert!(manifest.steps.is_empty());
    }

    // Empty project name is rejected.
    #[test]
    fn test_init_empty_name_rejected() {
        let base = tempfile::tempdir().unwrap();
        let err = init_at("", base.path()).unwrap_err();
        assert!(
            err.to_string().contains("empty"),
            "should reject empty name: {err}"
        );
    }

    // Whitespace-only project name is rejected.
    #[test]
    fn test_init_whitespace_name_rejected() {
        let base = tempfile::tempdir().unwrap();
        let err = init_at("   ", base.path()).unwrap_err();
        assert!(
            err.to_string().contains("empty"),
            "should reject whitespace name: {err}"
        );
    }

    // Init fails if project directory already exists.
    #[test]
    fn test_init_project_already_exists() {
        let base = tempfile::tempdir().unwrap();
        init_at("my-project", base.path()).unwrap();
        let err = init_at("my-project", base.path()).unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "should reject existing project: {err}"
        );
    }

    use clap::Parser;

    // `arc registry list` parses.
    #[test]
    fn test_registry_list_parses() {
        let cli = Cli::try_parse_from(["arc", "registry", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Registry {
                cmd: RegistryCmd::List { refresh: false }
            }
        ));
    }

    // `arc registry list --refresh` parses.
    #[test]
    fn test_registry_list_refresh_parses() {
        let cli = Cli::try_parse_from(["arc", "registry", "list", "--refresh"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Registry {
                cmd: RegistryCmd::List { refresh: true }
            }
        ));
    }

    // `arc registry show <name>` parses.
    #[test]
    fn test_registry_show_parses() {
        let cli = Cli::try_parse_from(["arc", "registry", "show", "brewtrend"]).unwrap();
        match cli.command {
            Commands::Registry {
                cmd: RegistryCmd::Show { name, refresh },
            } => {
                assert_eq!(name, "brewtrend");
                assert!(!refresh);
            }
            _ => panic!("expected Show"),
        }
    }

    // `arc registry fetch <name> --version <ref>` parses.
    #[test]
    fn test_registry_fetch_with_version_parses() {
        let cli =
            Cli::try_parse_from(["arc", "registry", "fetch", "brewtrend", "--version", "v1.2"])
                .unwrap();
        match cli.command {
            Commands::Registry {
                cmd:
                    RegistryCmd::Fetch {
                        name,
                        version,
                        latest,
                        ..
                    },
            } => {
                assert_eq!(name, "brewtrend");
                assert_eq!(version.as_deref(), Some("v1.2"));
                assert!(!latest);
            }
            _ => panic!("expected Fetch"),
        }
    }

    // `--version` and `--latest` together are mutually exclusive at parse time.
    #[test]
    fn test_version_latest_mutually_exclusive() {
        let r = Cli::try_parse_from([
            "arc",
            "registry",
            "fetch",
            "brewtrend",
            "--version",
            "v1.0",
            "--latest",
        ]);
        assert!(
            r.is_err(),
            "version + latest together should error at parse time"
        );
    }

    // `arc registry run <name>` accepts repeated --param.
    #[test]
    fn test_registry_run_accepts_repeated_params() {
        let cli = Cli::try_parse_from([
            "arc",
            "registry",
            "run",
            "brewtrend",
            "--param",
            "DATE=2026-04-29",
            "--param",
            "MODE=local",
        ])
        .unwrap();
        match cli.command {
            Commands::Registry {
                cmd: RegistryCmd::Run { params, .. },
            } => {
                assert_eq!(params.len(), 2);
            }
            _ => panic!("expected Registry::Run"),
        }
    }

    // top-level --verbose flag is global.
    #[test]
    fn test_verbose_flag_is_global() {
        let cli = Cli::try_parse_from(["arc", "--verbose", "registry", "list"]).unwrap();
        assert!(cli.verbose);
    }

    // unknown subcommand errors.
    #[test]
    fn test_unknown_subcommand_errors() {
        let r = Cli::try_parse_from(["arc", "registry", "drop"]);
        assert!(r.is_err());
    }

    // `arc create-protocol <dir>` parses, with every option defaulted.
    #[test]
    fn test_create_protocol_parses() {
        let cli = Cli::try_parse_from(["arc", "create-protocol", "fieldbook"]).unwrap();
        match cli.command {
            Commands::CreateProtocol {
                dir,
                name,
                engine,
                db,
            } => {
                assert_eq!(dir, PathBuf::from("fieldbook"));
                assert!(name.is_none() && engine.is_none() && db.is_none());
            }
            _ => panic!("expected CreateProtocol"),
        }
    }

    // `arc edit-protocol --dir <d> replace <path> <value>` parses.
    #[test]
    fn test_edit_protocol_replace_parses() {
        let cli = Cli::try_parse_from([
            "arc",
            "edit-protocol",
            "--dir",
            "proto",
            "replace",
            "steps[0].command",
            "\"true\"",
        ])
        .unwrap();
        match cli.command {
            Commands::EditProtocol { dir, op } => {
                assert_eq!(dir, PathBuf::from("proto"));
                assert!(matches!(op, EditOp::Replace { .. }));
            }
            _ => panic!("expected EditProtocol"),
        }
    }

    // The edit-protocol directory defaults to the current directory.
    #[test]
    fn test_edit_protocol_dir_defaults_to_cwd() {
        let cli = Cli::try_parse_from(["arc", "edit-protocol", "delete", "steps[0]"]).unwrap();
        match cli.command {
            Commands::EditProtocol { dir, .. } => assert_eq!(dir, PathBuf::from(".")),
            _ => panic!("expected EditProtocol"),
        }
    }

    // The path grammar: keys, indices, the root — and the refusals.
    #[test]
    fn test_parse_path_grammar() {
        use PathPart::{Index, Key};
        assert_eq!(
            parse_path("steps[2].command").unwrap(),
            vec![Key("steps".into()), Index(2), Key("command".into())]
        );
        assert_eq!(
            parse_path("params.port.default").unwrap(),
            vec![
                Key("params".into()),
                Key("port".into()),
                Key("default".into())
            ]
        );
        assert_eq!(parse_path(".").unwrap(), vec![]);

        for bad in [
            "",
            "steps..command",
            "steps.",
            "steps[x]",
            "steps[0",
            "steps[0]tail",
            "steps[]",
        ] {
            let err = parse_path(bad).unwrap_err();
            assert!(
                matches!(err, Error::EditTarget { .. }),
                "{bad:?} must be refused as an edit-target error, got {err}"
            );
        }
    }

    // Each verb maps field-for-field onto the library's edit value — the CLI
    // adds argument surface, not semantics.
    #[test]
    fn test_ops_map_onto_the_library_edit_values() {
        let steps_cmd = || vec!["steps".into(), 0.into(), "command".into()];
        assert_eq!(
            spec_edit_of(EditOp::Replace {
                path: "steps[0].command".into(),
                value: "v".into()
            })
            .unwrap(),
            SpecEdit::Replace {
                path: steps_cmd(),
                value: "v".into()
            }
        );
        assert_eq!(
            spec_edit_of(EditOp::Rewrite {
                path: "steps[0].command".into(),
                from: "a".into(),
                to: "b".into()
            })
            .unwrap(),
            SpecEdit::RewriteFragment {
                path: steps_cmd(),
                from: "a".into(),
                to: "b".into()
            }
        );
        assert_eq!(
            spec_edit_of(EditOp::Add {
                path: ".".into(),
                key: "engine".into(),
                value: "duckdb".into()
            })
            .unwrap(),
            SpecEdit::Add {
                path: vec![],
                key: "engine".into(),
                value: "duckdb".into()
            }
        );
        assert_eq!(
            spec_edit_of(EditOp::Append {
                path: "steps".into(),
                item: "  - name: x\n".into()
            })
            .unwrap(),
            SpecEdit::Append {
                path: vec!["steps".into()],
                item: "  - name: x\n".into()
            }
        );
        assert_eq!(
            spec_edit_of(EditOp::Delete {
                path: "steps[1]".into()
            })
            .unwrap(),
            SpecEdit::Delete {
                path: vec!["steps".into(), 1.into()]
            }
        );
        assert_eq!(
            spec_edit_of(EditOp::Reorder {
                path: "steps".into(),
                from: 1,
                to: 0
            })
            .unwrap(),
            SpecEdit::Reorder {
                path: vec!["steps".into()],
                from: 1,
                to: 0
            }
        );
    }

    // create-protocol writes a loadable spec through the gate, derives the
    // name from the directory, and refuses a second create.
    #[test]
    fn test_create_protocol_writes_once_through_the_gate() {
        let base = tempfile::tempdir().unwrap();
        let history = LocalHistory::at_root(base.path().join("history"));
        let dir = base.path().join("notes");
        create_protocol(&dir, None, None, None, &history).unwrap();

        let m = Manifest::load(&dir).unwrap();
        assert_eq!(m.name, "notes");
        assert!(m.steps.is_empty());

        // No --db: the file carries no db line, and the database is <name>.duckdb in
        // arc's data folder rather than beside the manifest.
        let text = fs::read_to_string(dir.join("arcform.yaml")).unwrap();
        assert!(
            !text.lines().any(|l| l.starts_with("db")),
            "no db line without --db:\n{text}"
        );
        let db = m.db_path(&dir).unwrap();
        assert!(
            !db.starts_with(dir.canonicalize().unwrap()),
            "{}",
            db.display()
        );
        assert!(db.ends_with("notes.duckdb"), "{}", db.display());

        let err = create_protocol(&dir, None, None, None, &history).unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "an existing spec must be refused, never overwritten: {err}"
        );
    }

    // --name / --engine / --db override the derived defaults.
    #[test]
    fn test_create_protocol_applies_overrides() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("anything");
        create_protocol(
            &dir,
            Some("tides".to_string()),
            Some("sqlite3".to_string()),
            Some("state/tides.db".to_string()),
            &LocalHistory::at_root(base.path().join("history")),
        )
        .unwrap();

        let m = Manifest::load(&dir).unwrap();
        assert_eq!(m.name, "tides");
        assert_eq!(m.engine, "sqlite3");
        assert_eq!(m.db.as_deref(), Some("state/tides.db"));
        let text = fs::read_to_string(dir.join("arcform.yaml")).unwrap();
        assert!(
            text.lines().any(|l| l == "db: state/tides.db"),
            "--db writes the line:\n{text}"
        );
    }

    // A directory with no usable file name needs --name.
    #[test]
    fn test_create_protocol_requires_a_derivable_name() {
        let base = tempfile::tempdir().unwrap();
        let history = LocalHistory::at_root(base.path().join("history"));
        let err = create_protocol(Path::new("."), None, None, None, &history).unwrap_err();
        assert!(err.to_string().contains("--name"), "{err}");
    }

    // edit-protocol refuses when there is no spec to edit.
    #[test]
    fn test_edit_protocol_requires_a_spec() {
        let base = tempfile::tempdir().unwrap();
        let err = edit_protocol(
            base.path(),
            EditOp::Delete {
                path: "steps[0]".into(),
            },
            &LocalHistory::at_root(base.path().join("history")),
        )
        .unwrap_err();
        assert!(matches!(err, Error::ManifestNotFound), "{err}");
    }

    // `arc history list` / `show <id>` / `restore <id>` parse, with the
    // directory defaulting to the cwd and no file unless `--file` names one.
    #[test]
    fn test_history_subcommands_parse() {
        let cli = Cli::try_parse_from(["arc", "history", "list"]).unwrap();
        match cli.command {
            Commands::History {
                cmd: HistoryCmd::List { dir, file },
            } => {
                assert_eq!(dir, PathBuf::from("."));
                assert_eq!(file, None);
            }
            _ => panic!("expected History/List"),
        }

        let cli =
            Cli::try_parse_from(["arc", "history", "list", "--file", "panels/a.yaml"]).unwrap();
        match cli.command {
            Commands::History {
                cmd: HistoryCmd::List { file, .. },
            } => assert_eq!(file, Some(PathBuf::from("panels/a.yaml"))),
            _ => panic!("expected History/List"),
        }

        let cli = Cli::try_parse_from([
            "arc",
            "history",
            "show",
            "1700000000000-000-save",
            "--file",
            "panels/a.yaml",
        ])
        .unwrap();
        match cli.command {
            Commands::History {
                cmd: HistoryCmd::Show { id, file, .. },
            } => {
                assert_eq!(id, "1700000000000-000-save");
                assert_eq!(file, Some(PathBuf::from("panels/a.yaml")));
            }
            _ => panic!("expected History/Show"),
        }

        let cli = Cli::try_parse_from([
            "arc",
            "history",
            "restore",
            "1700000000000-000-save",
            "--dir",
            "notes",
        ])
        .unwrap();
        match cli.command {
            Commands::History {
                cmd: HistoryCmd::Restore { id, dir, file },
            } => {
                assert_eq!(id, "1700000000000-000-save");
                assert_eq!(dir, PathBuf::from("notes"));
                assert_eq!(file, None);
            }
            _ => panic!("expected History/Restore"),
        }

        let cli = Cli::try_parse_from([
            "arc",
            "history",
            "restore",
            "1700000000000-000-save",
            "--file",
            "panels/a.yaml",
        ])
        .unwrap();
        match cli.command {
            Commands::History {
                cmd: HistoryCmd::Restore { file, .. },
            } => assert_eq!(file, Some(PathBuf::from("panels/a.yaml"))),
            _ => panic!("expected History/Restore"),
        }
    }

    // The numbers a user reads in `arc history --help` must be the numbers
    // the store enforces. This fails when the policy constants move without
    // the stated policy moving with them.
    #[test]
    fn test_the_stated_policy_matches_the_enforced_policy() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let about = cmd
            .find_subcommand("history")
            .expect("history subcommand exists")
            .get_long_about()
            .expect("history carries a long about")
            .to_string();
        assert!(
            about.contains(&format!("{} entries", crate::spec::HISTORY_MAX_ENTRIES)),
            "the stated bound drifted from HISTORY_MAX_ENTRIES:
{about}"
        );
        assert!(
            about.contains(&format!(
                "{} seconds",
                crate::spec::HISTORY_MERGE_WINDOW.as_secs()
            )),
            "the stated merge window drifted from HISTORY_MERGE_WINDOW:
{about}"
        );
    }

    // The CLI surface end to end: create records the first save, an edit
    // checkpoints the state it replaces, list prints the policy beside the
    // entries, show hands back exact bytes, restore rolls back and reports.
    #[test]
    fn test_history_cli_round_trip() {
        let base = tempfile::tempdir().unwrap();
        let history = LocalHistory::at_root(base.path().join("history"));
        let dir = base.path().join("notes");
        create_protocol(&dir, None, None, None, &history).unwrap();
        let created = fs::read_to_string(dir.join("arcform.yaml")).unwrap();

        edit_protocol(
            &dir,
            EditOp::Replace {
                path: "name".into(),
                value: "renamed".into(),
            },
            &history,
        )
        .unwrap();
        let edited = fs::read_to_string(dir.join("arcform.yaml")).unwrap();
        assert_ne!(created, edited);

        let mut listed = Vec::new();
        history_list(&dir, None, &history, &mut listed).unwrap();
        let listed = String::from_utf8(listed).unwrap();
        assert!(listed.contains("save"), "{listed}");
        assert!(
            listed.contains("policy:"),
            "the policy prints under the listing: {listed}"
        );

        // The pre-edit state is recorded; show returns its exact bytes.
        let entries = history.entries(&dir).unwrap();
        let pre_edit = entries
            .iter()
            .find(|e| history.read(&dir, &e.id).unwrap() == created)
            .expect("the created state is recorded");
        let mut shown = Vec::new();
        history_show(&dir, None, &pre_edit.id, &history, &mut shown).unwrap();
        assert_eq!(String::from_utf8(shown).unwrap(), created);

        // Restore rolls the file back and says so.
        let mut out = Vec::new();
        history_restore(&dir, None, &pre_edit.id.clone(), &history, &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("restored"), "{out}");
        assert_eq!(
            fs::read_to_string(dir.join("arcform.yaml")).unwrap(),
            created
        );

        // An unknown id is refused by name.
        let mut sink = Vec::new();
        let err = history_restore(&dir, None, "not-an-id", &history, &mut sink).unwrap_err();
        assert!(matches!(err, Error::HistoryEntryNotFound { .. }), "{err}");
    }

    // module documentation contains the four vocabulary anchors.
    #[test]
    fn test_registry_module_doc_contains_anchors() {
        let body = include_str!("registry/mod.rs");
        let lower = body.to_lowercase();
        for anchor in ["asset", "two-tier", "transport", "sister work"] {
            assert!(
                lower.contains(anchor),
                "registry/mod.rs doc should mention '{anchor}'"
            );
        }
    }

    // ------------------------------------------------------ arc operation

    /// A writer whose every write fails, standing in for a closed pipe or a
    /// full disk on stdout.
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("stdout is closed"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // A listing that could not be written is an error, not an empty success:
    // an agent reading a truncated list must be told it is truncated.
    #[test]
    fn operation_list_reports_a_failed_write_in_both_forms() {
        for json in [false, true] {
            let err = operation_list(json, &mut FailingWriter).unwrap_err();
            assert!(
                err.to_string().contains("stdout is closed"),
                "json={json}: {err}"
            );
        }
    }

    #[test]
    fn operation_describe_reports_a_failed_write() {
        let err = operation_describe("filter-rows", &mut FailingWriter).unwrap_err();
        assert!(err.to_string().contains("stdout is closed"), "{err}");
    }

    #[test]
    fn operation_record_reports_a_failed_write() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("shop");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(
            dir.join("arcform.yaml"),
            "name: shop\nsteps:\n  - name: orders\n    sql: models/orders.sql\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("models/orders.sql"),
            "CREATE TABLE orders AS SELECT 1 AS amount;\n",
        )
        .unwrap();
        let arguments = operation_arguments(&["where=amount > 0".to_string()]).unwrap();
        let history = LocalHistory::at_root(root.path().join("history"));
        let err = operation_record(
            &dir,
            "filter-rows",
            "orders",
            "big",
            &arguments,
            &history,
            &mut FailingWriter,
        )
        .unwrap_err();
        assert!(err.to_string().contains("stdout is closed"), "{err}");
    }

    #[test]
    fn operation_arguments_split_each_at_its_first_equals_sign() {
        let arguments =
            operation_arguments(&["where=region = 'north'".to_string(), "label=".to_string()])
                .unwrap();
        assert_eq!(
            serde_json::Value::Object(arguments),
            serde_json::json!({ "where": "region = 'north'", "label": "" })
        );
    }

    #[test]
    fn operation_arguments_refuse_an_entry_without_a_value_and_a_repeated_key() {
        let err = operation_arguments(&["where".to_string()]).unwrap_err();
        assert!(
            err.to_string().contains("`--arg where` must be KEY=VALUE"),
            "{err}"
        );
        let err = operation_arguments(&["where=a > 1".to_string(), "where=b > 1".to_string()])
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("`--arg where` is given more than once"),
            "{err}"
        );
    }
}
