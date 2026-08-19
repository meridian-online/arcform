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

    // Local history cannot resolve a store root: no $ARCFORM_HISTORY_DIR and
    // no home directory. The remedy is the env var, so the message names it.
    #[error(
        "history: no store root (set ARCFORM_HISTORY_DIR to a writable directory, \
         or ensure a home directory exists)"
    )]
    HistoryRootMissing,

    #[error("history: no entry '{id}' for this spec (see `arc history list`)")]
    HistoryEntryNotFound { id: String },

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

    /// A `tool:` precondition could not establish what the tool it names currently is.
    /// Carries the step, the declaration as written, and where the lookup got to — the
    /// resolved path when there was one, otherwise what was searched.
    #[error("precondition error for step '{step}': tool {tool} could not be identified: {detail}")]
    ToolPrecondition {
        step: String,
        tool: String,
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

impl Error {
    /// The process exit code `cli_main` reports for this error — two buckets, not one:
    ///
    /// - **1, "cannot run"**: `arc` (or this step) never got to attempt the substantive
    ///   work — a manifest that will not load or validate, a config or input that is
    ///   missing or malformed, infrastructure (the state backend, the registry
    ///   transport) that is unavailable. Nothing was tried; nothing to distinguish.
    /// - **2, "found a problem"**: `arc` attempted the work, and the attempt itself is
    ///   what failed — a step's command or SQL, a precondition check, a timeout. This
    ///   is the bucket a step forced to re-run by a corrected staleness decision lands
    ///   in if that re-run then fails for a real reason.
    ///
    /// Kept distinguishable on purpose: a mutation test on the staleness gate that
    /// reverts the fix and finds the run still exits 1 either way would prove nothing —
    /// see [`crate::runner`]'s `is_hash_stale`, the gate this distinction exists for.
    pub(crate) fn exit_code(&self) -> i32 {
        match self {
            Error::StepFailed { .. }
            | Error::StepExecution { .. }
            | Error::Precondition { .. }
            | Error::ToolPrecondition { .. }
            | Error::StepTimeout { .. }
            | Error::PipelineTimeout { .. } => 2,
            _ => 1,
        }
    }
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

    // The history variants follow the same discipline: single-line Display,
    // a `history:` family prefix, and the remediation in the message.
    #[test]
    fn history_root_missing_names_the_env_var() {
        let e = Error::HistoryRootMissing;
        let s = e.to_string();
        assert!(!s.contains('\n'), "Display must be single-line: {:?}", s);
        assert!(s.starts_with("history:"), "family prefix: {:?}", s);
        assert!(
            s.contains("ARCFORM_HISTORY_DIR"),
            "remediation env var must appear: {:?}",
            s
        );
    }

    #[test]
    fn history_entry_not_found_names_the_entry_and_the_listing() {
        let e = Error::HistoryEntryNotFound {
            id: "1700000000000-000-save".into(),
        };
        let s = e.to_string();
        assert!(!s.contains('\n'), "Display must be single-line: {:?}", s);
        assert!(s.starts_with("history:"), "family prefix: {:?}", s);
        assert!(s.contains("1700000000000-000-save"));
        assert!(s.contains("arc history list"));
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

#[cfg(test)]
mod exit_code_tests {
    //! `cannot run` (1) and `found a problem` (2) must stay two different numbers, or
    //! a mutation test on whichever check raises one of these variants cannot tell "the
    //! gate did not fire" from "something unrelated stopped the run before it could."
    use super::*;

    #[test]
    fn manifest_and_config_problems_cannot_run() {
        for e in [
            Error::ManifestNotFound,
            Error::ManifestValidation("bad".into()),
            Error::SqlFileNotFound {
                step: "s".into(),
                path: PathBuf::from("missing.sql"),
            },
            Error::MissingParam { name: "p".into() },
            Error::EngineNotFound {
                engine: "duckdb".into(),
            },
            Error::StateBackend("locked".into()),
        ] {
            assert_eq!(e.exit_code(), 1, "expected 'cannot run' (1) for {e:?}");
        }
    }

    #[test]
    fn execution_failures_found_a_problem() {
        for e in [
            Error::StepFailed {
                step: "s".into(),
                code: 1,
                stderr: "boom".into(),
            },
            Error::Precondition {
                step: "s".into(),
                command: "test -f x".into(),
                detail: "exit 1".into(),
            },
            Error::ToolPrecondition {
                step: "s".into(),
                tool: "duckdb".into(),
                detail: "not found".into(),
            },
            Error::StepTimeout { step: "s".into() },
            Error::PipelineTimeout {
                step: "s".into(),
                elapsed_sec: 3.0,
            },
        ] {
            assert_eq!(e.exit_code(), 2, "expected 'found a problem' (2) for {e:?}");
        }
    }

    #[test]
    fn the_two_codes_are_actually_different() {
        // A test with no non-trivial assertion (`1 != 1`) would pass whichever integer
        // the two buckets happened to share — this pins that they are not the same.
        assert_ne!(
            Error::ManifestNotFound.exit_code(),
            Error::StepTimeout { step: "s".into() }.exit_code()
        );
    }
}
