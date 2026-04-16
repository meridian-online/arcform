use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::engine::DuckDbEngine;
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::state::DuckDbStateBackend;

#[derive(Parser)]
#[command(name = "arc", version, about = "Local-first data pipeline engine")]
pub struct Cli {
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

/// Execute the `arc run` command.
pub fn run_pipeline(force: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manifest = Manifest::load(&cwd)?;
    let db_path = manifest.db_path(&cwd);
    let engine = DuckDbEngine;
    let state = DuckDbStateBackend::new(&db_path);
    crate::runner::run(&cwd, &engine, &state, force)
}

/// Dispatch CLI commands.
pub fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { name } => init(&name),
        Commands::Run { force } => run_pipeline(force),
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
}
