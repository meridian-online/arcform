use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("arcform.yaml not found in current directory")]
    ManifestNotFound,

    #[error("failed to read {path}: {source}")]
    FileRead {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse arcform.yaml: {0}")]
    ManifestParse(#[from] serde_yaml::Error),

    #[error("invalid manifest: {0}")]
    ManifestValidation(String),

    #[error("step '{step}': sql file not found: {path}")]
    SqlFileNotFound { step: String, path: PathBuf },

    #[error("engine '{engine}' not found on PATH or not executable")]
    EngineNotFound { engine: String },

    #[error("step '{step}' failed (exit code {code}):\n{stderr}")]
    StepFailed {
        step: String,
        code: i32,
        stderr: String,
    },

    #[error("step '{step}' failed: {source}")]
    StepExecution {
        step: String,
        source: std::io::Error,
    },

    #[error("project directory already exists: {0}")]
    ProjectExists(PathBuf),

    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
