use std::path::PathBuf;

/// Every failure `arc` reports, from spec loading through to the registry client.
///
/// Published as part of the spec contract, so it is `#[non_exhaustive]`: a caller
/// matching on it must carry a `_` arm, and adding a variant is therefore not a
/// breaking change for them. Only the manifest variants can be raised by the loading
/// and validation surface — the rest belong to the private engine and are reachable
/// through this type only because they share it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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

    // The spec write path's refusal for an edit that cannot be applied: the
    // route did not resolve, the fragment was missing or ambiguous, or the op
    // does not fit the shape it found. Distinct from `ManifestValidation`,
    // which is the refusal for an edit that applied but produced a spec that
    // will not load.
    #[error("edit: {path}: {detail}")]
    EditTarget { path: String, detail: String },

    #[error("a spec already exists at {0} — edit it rather than re-creating it")]
    SpecExists(PathBuf),

    // The record path's create-mode collision: a generated SQL file is only
    // ever written where no file exists, because an existing file may carry
    // authorship this tool must not destroy.
    #[error("a sql file already exists at {0} — a recorded step never overwrites a model")]
    GeneratedSqlExists(PathBuf),

    // The record path's ownership refusal: the file lacks the generated
    // marker, so its bytes were not machine-authored and are never machine-
    // rewritten. The remedy is offered in the message because the caller is
    // expected to surface it to a person.
    #[error(
        "step '{step}': {path} is hand-authored (no generated marker) and will not be \
         rewritten — record a new step downstream of it instead"
    )]
    HandAuthoredSql { step: String, path: PathBuf },

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

    #[error(
        "dependency order violation: step '{reader}' reads asset '{asset}' but '{asset}' is produced by step '{producer}' which runs after it"
    )]
    DependencyOrder {
        reader: String,
        asset: String,
        producer: String,
    },

    #[error(
        "precondition error for step '{step}': command '{command}' failed to execute: {detail}"
    )]
    Precondition {
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

    #[error(
        "registry: cache root unavailable (set ARCFORM_REGISTRY_CACHE to a writable directory)"
    )]
    RegistryCacheRootMissing,

    #[error("registry: '{feature}' is not implemented in v1")]
    RegistryUnimplemented { feature: String },

    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod format_tests {
    //! format each new registry variant; assert single-line default
    //! and that the message carries the `registry:` prefix so callers can
    //! distinguish registry surface errors from other arcform error families.
    //!
    //! These tests are deliberately narrow — they check Display output, not
    //! the runtime construction sites (those are exercised by the registry
    //! module's own unit tests).
    use super::*;
    use std::io;
    use std::path::PathBuf;

    fn assert_single_line_registry(err: &Error) {
        let s = err.to_string();
        assert!(!s.contains('\n'), "Display must be single-line: {:?}", s);
        assert!(
            s.starts_with("registry:"),
            "expected `registry:` prefix, got: {:?}",
            s
        );
    }

    #[test]
    fn registry_index_fetch() {
        let e = Error::RegistryIndexFetch {
            url: "https://example/index.yaml".into(),
            detail: "boom".into(),
        };
        assert_single_line_registry(&e);
    }

    #[test]
    fn registry_index_parse() {
        let e = Error::RegistryIndexParse {
            detail: "bad yaml".into(),
        };
        assert_single_line_registry(&e);
    }

    #[test]
    fn registry_unknown_entry() {
        let e = Error::RegistryUnknownEntry {
            query: "nope".into(),
        };
        assert_single_line_registry(&e);
        assert!(e.to_string().contains("nope"));
    }

    #[test]
    fn registry_ambiguous_query() {
        let e = Error::RegistryAmbiguousQuery {
            query: "//bad".into(),
        };
        assert_single_line_registry(&e);
    }

    #[test]
    fn registry_transport() {
        let e = Error::RegistryTransport {
            detail: "tarball walked outside <dest>".into(),
        };
        assert_single_line_registry(&e);
    }

    #[test]
    fn registry_cache_io() {
        let e = Error::RegistryCacheIo {
            path: PathBuf::from("/tmp/cache/index.yaml"),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        };
        assert_single_line_registry(&e);
    }

    #[test]
    fn registry_cache_root_missing() {
        let e = Error::RegistryCacheRootMissing;
        assert_single_line_registry(&e);
        // The remediation hint must surface in the default Display.
        assert!(
            e.to_string().contains("ARCFORM_REGISTRY_CACHE"),
            "remediation env var must appear: {:?}",
            e.to_string()
        );
    }

    #[test]
    fn registry_unimplemented() {
        let e = Error::RegistryUnimplemented {
            feature: "--latest rolling resolution".into(),
        };
        assert_single_line_registry(&e);
        assert!(e.to_string().contains("--latest rolling resolution"));
    }
}
