use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::engine::DuckDbEngine;
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::registry::transport::GitTarballTransport;
use crate::registry::{RunOptions, cache_root};
use crate::spec::{PathPart, SpecEdit};
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

        /// Database file path, relative to the protocol directory
        /// (default: `<name>.duckdb`).
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
    /// Discover, fetch, and run curated registry pipelines.
    Registry {
        #[command(subcommand)]
        cmd: RegistryCmd,
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

/// Execute the `arc create-protocol` command: assemble the manifest value the
/// arguments describe and hand it to the library's creation path — the same
/// gate and the same atomic write every other caller gets. Nothing is written
/// unless the result loads; an existing spec is refused, never overwritten.
pub fn create_protocol(
    dir: &Path,
    name: Option<String>,
    engine: Option<String>,
    db: Option<String>,
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
    crate::spec::create_spec(dir, &manifest)?;

    println!(
        "created {} — author steps with `arc edit-protocol`",
        dir.join(crate::spec::MANIFEST_FILENAME).display()
    );
    Ok(())
}

/// Execute the `arc edit-protocol` command: turn the op into the library's
/// edit value and hand it to the whole write path — apply, validate, write
/// atomically. The CLI's job ends at argument parsing; a refusal (an edit
/// that does not apply, or a result the loader rejects) surfaces here as the
/// error, before the file is touched.
pub fn edit_protocol(dir: &Path, op: EditOp) -> Result<()> {
    let edit = spec_edit_of(op)?;
    crate::spec::edit_spec(dir, &[edit])?;
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

    println!("Initialized project '{}' with:", name);
    println!("  arcform.yaml");
    println!("  models/");
    println!("  sources/");
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
    let db_path = manifest.db_path(&cwd);
    let engine = DuckDbEngine;
    let state = DuckDbStateBackend::new(&db_path);
    crate::runner::run_with_params(&cwd, &engine, &state, force, &cli_params)
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
        } => create_protocol(&dir, name, engine, db),
        Commands::EditProtocol { dir, op } => edit_protocol(&dir, op),
        Commands::Run { force, params } => run_pipeline(force, &params),
        Commands::Registry { cmd } => dispatch_registry(cmd, verbose),
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
        assert_eq!(manifest.db, Some("analytics.duckdb".to_string()));
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
        let dir = base.path().join("notes");
        create_protocol(&dir, None, None, None).unwrap();

        let m = Manifest::load(&dir).unwrap();
        assert_eq!(m.name, "notes");
        assert!(m.steps.is_empty());

        let err = create_protocol(&dir, None, None, None).unwrap_err();
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
        )
        .unwrap();

        let m = Manifest::load(&dir).unwrap();
        assert_eq!(m.name, "tides");
        assert_eq!(m.engine, "sqlite3");
        assert_eq!(m.db.as_deref(), Some("state/tides.db"));
    }

    // A directory with no usable file name needs --name.
    #[test]
    fn test_create_protocol_requires_a_derivable_name() {
        let err = create_protocol(Path::new("."), None, None, None).unwrap_err();
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
        )
        .unwrap_err();
        assert!(matches!(err, Error::ManifestNotFound), "{err}");
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
}
