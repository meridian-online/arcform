use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::engine::DuckDbEngine;
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::registry::transport::GitTarballTransport;
use crate::registry::{cache_root, RunOptions};
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

    Ok(())
}

/// Parse --param KEY=VALUE flags into (key, value) pairs.
/// Splits on the first '=' — keys cannot contain '=', values can.
pub fn parse_params(raw: &[String]) -> Result<Vec<(String, String)>> {
    let mut parsed = Vec::new();
    for param in raw {
        if let Some(pos) = param.find('=') {
            let key = param[..pos].to_string();
            let value = param[pos + 1..].to_string();
            if key.is_empty() {
                return Err(Error::ManifestValidation(format!(
                    "invalid --param '{}': key cannot be empty",
                    param
                )));
            }
            parsed.push((key, value));
        } else {
            return Err(Error::ManifestValidation(format!(
                "invalid --param '{}': expected KEY=VALUE format",
                param
            )));
        }
    }
    Ok(parsed)
}

/// Execute the `arc run` command.
pub fn run_pipeline(force: bool, raw_params: &[String]) -> Result<()> {
    let cli_params = parse_params(raw_params)?;
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
        Commands::Init { name } => init(&name),
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
            crate::registry::handle_fetch(
                &opts,
                &name,
                version,
                latest,
                &mut stdout,
                &mut stderr,
            )
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

    // AC-1: `arc init` creates arcform.yaml, models/, sources/.
    #[test]
    fn test_ac01_init_creates_project_structure() {
        let base = tempfile::tempdir().unwrap();
        init_at("my-project", base.path()).unwrap();

        let project = base.path().join("my-project");
        assert!(project.join("arcform.yaml").is_file(), "arcform.yaml should exist");
        assert!(project.join("models").is_dir(), "models/ should exist");
        assert!(project.join("sources").is_dir(), "sources/ should exist");
    }

    // AC-2: Generated manifest has correct defaults.
    #[test]
    fn test_ac02_init_manifest_defaults() {
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

    // AC-2: Empty project name is rejected.
    #[test]
    fn test_ac02_init_empty_name_rejected() {
        let base = tempfile::tempdir().unwrap();
        let err = init_at("", base.path()).unwrap_err();
        assert!(err.to_string().contains("empty"), "should reject empty name: {err}");
    }

    // AC-2: Whitespace-only project name is rejected.
    #[test]
    fn test_ac02_init_whitespace_name_rejected() {
        let base = tempfile::tempdir().unwrap();
        let err = init_at("   ", base.path()).unwrap_err();
        assert!(err.to_string().contains("empty"), "should reject whitespace name: {err}");
    }

    // AC-1: Init fails if project directory already exists.
    #[test]
    fn test_ac01_init_project_already_exists() {
        let base = tempfile::tempdir().unwrap();
        init_at("my-project", base.path()).unwrap();
        let err = init_at("my-project", base.path()).unwrap_err();
        assert!(err.to_string().contains("already exists"), "should reject existing project: {err}");
    }

    use clap::Parser;

    // ac-06: `arc registry list` parses.
    #[test]
    fn test_ac06_registry_list_parses() {
        let cli = Cli::try_parse_from(["arc", "registry", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Registry {
                cmd: RegistryCmd::List { refresh: false }
            }
        ));
    }

    // ac-06: `arc registry list --refresh` parses.
    #[test]
    fn test_ac06_registry_list_refresh_parses() {
        let cli = Cli::try_parse_from(["arc", "registry", "list", "--refresh"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Registry {
                cmd: RegistryCmd::List { refresh: true }
            }
        ));
    }

    // ac-06: `arc registry show <name>` parses.
    #[test]
    fn test_ac06_registry_show_parses() {
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

    // ac-06: `arc registry fetch <name> --version <ref>` parses.
    #[test]
    fn test_ac06_registry_fetch_with_version_parses() {
        let cli = Cli::try_parse_from(["arc", "registry", "fetch", "brewtrend", "--version", "v1.2"])
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

    // ac-06: `--version` and `--latest` together are mutually exclusive at parse time.
    #[test]
    fn test_ac06_version_latest_mutually_exclusive() {
        let r = Cli::try_parse_from([
            "arc",
            "registry",
            "fetch",
            "brewtrend",
            "--version",
            "v1.0",
            "--latest",
        ]);
        assert!(r.is_err(), "version + latest together should error at parse time");
    }

    // ac-06: `arc registry run <name>` accepts repeated --param.
    #[test]
    fn test_ac06_registry_run_accepts_repeated_params() {
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

    // ac-06: top-level --verbose flag is global.
    #[test]
    fn test_ac06_verbose_flag_is_global() {
        let cli = Cli::try_parse_from(["arc", "--verbose", "registry", "list"]).unwrap();
        assert!(cli.verbose);
    }

    // ac-06: unknown subcommand errors.
    #[test]
    fn test_ac06_unknown_subcommand_errors() {
        let r = Cli::try_parse_from(["arc", "registry", "drop"]);
        assert!(r.is_err());
    }

    // ac-12: module documentation contains the four vocabulary anchors.
    #[test]
    fn test_ac12_registry_module_doc_contains_anchors() {
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
