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

    #[error("engine version mismatch: requires {required}, found {found}")]
    VersionMismatch { required: String, found: String },

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

    #[error("dependency order violation: step '{reader}' reads asset '{asset}' but '{asset}' is produced by step '{producer}' which runs after it")]
    DependencyOrder {
        reader: String,
        asset: String,
        producer: String,
    },

    #[error("precondition error for step '{step}': command '{command}' failed to execute: {detail}")]
    PreconditionError {
        step: String,
        command: String,
        detail: String,
    },

    #[error("missing required parameter '{name}' (no default, not in dotenv or CLI)")]
    MissingParam { name: String },

    #[error("step '{step}' timed out")]
    StepTimeout { step: String },

    #[error("pipeline timeout after {elapsed_sec:.1}s — step '{step}' was running")]
    PipelineTimeout { step: String, elapsed_sec: f64 },

    #[error("state backend error: {0}")]
    StateBackend(String),

    // Constructed by FixtureTransport (cfg(test)) and by the production transport's
    // sister-work fetch path. Allowed because non-test builds today only see the
    // cfg(test) construction site.
    #[allow(dead_code)]
    #[error("registry: failed to fetch index from {url}: {detail}")]
    RegistryIndexFetch { url: String, detail: String },

    #[error("registry: failed to parse index: {detail}")]
    RegistryIndexParse { detail: String },

    #[error("registry: unknown entry '{query}' (try `arc registry list`)")]
    RegistryUnknownEntry { query: String },

    #[error("registry: malformed query '{query}' (expected `<name>` or `<owner>/<name>`)")]
    RegistryAmbiguousQuery { query: String },

    #[error("registry: transport error: {detail}")]
    RegistryTransport { detail: String },

    #[error("registry: cache I/O at {path}: {source}")]
    RegistryCacheIo {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("registry: cache root unavailable (set ARCFORM_REGISTRY_CACHE to a writable directory)")]
    RegistryCacheRootMissing,

    #[error("registry: '{feature}' is not implemented in v1")]
    RegistryUnimplemented { feature: String },

    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
