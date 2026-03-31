use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::engine::DuckDbEngine;
use crate::error::{Error, Result};
use crate::manifest::Manifest;

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
    Run,
}

/// Execute the `arc init` command.
pub fn init(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::ManifestValidation(
            "project name cannot be empty".to_string(),
        ));
    }

    let project_dir = PathBuf::from(name);
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
pub fn run_pipeline() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let engine = DuckDbEngine;
    crate::runner::run(&cwd, &engine)
}

/// Dispatch CLI commands.
pub fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { name } => init(&name),
        Commands::Run => run_pipeline(),
    }
}
