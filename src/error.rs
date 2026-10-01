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

    // A Protocol that names no `db:` keeps its database in arc's data folder, and that
    // cannot be resolved: no $ARCFORM_DB_DIR and no home directory. The remedy is the
    // env var, or a `db:` line, so the message names both.
    #[error(
        "database: no data folder for a Protocol that names no db: (set ARCFORM_DB_DIR \
         to a writable directory, ensure a home directory exists, or name a db: in arcform.yaml)"
    )]
    DbRootMissing,

    #[error("history: no entry '{id}' for this spec (see `arc history list`)")]
    HistoryEntryNotFound { id: String },

    // A word that cannot be the way arc was reached: not one word arc can keep
    // in an entry's file name, or one of arc's own ways spelt by a caller.
    #[error("history: {way:?} cannot name the way arc was reached: {detail}")]
    HistoryWay { way: String, detail: String },

    #[error("engine '{engine}' not found on PATH or not executable")]
    EngineNotFound { engine: String },

    #[error("engine version mismatch: requires {required}, found {found}")]
    VersionMismatch { required: String, found: String },

    // The engine arc was told to run is unusable. The search path is deliberately not
    // tried instead: a told engine that silently becomes whichever `duckdb` sorts first
    // on PATH is two installs deciding which one writes your data.
    #[error(
        "{var} names '{}', which is not a usable DuckDB executable ({reason}); \
         point it at one, or unset it to use the `duckdb` on PATH",
        path.display()
    )]
    EngineBinInvalid {
        var: &'static str,
        path: PathBuf,
        reason: String,
    },

    #[error(
        "engine DuckDB {found} is outside the versions arc is tested on ({range}); \
         use a DuckDB in that range, or set {override_var}=1 to run on {found} at your own risk"
    )]
    UntestedEngine {
        found: String,
        range: &'static str,
        override_var: &'static str,
    },

    // A Protocol's SQL installs a DuckDB extension from somewhere arc has not vetted. One
    // line per statement found, so a Protocol with three is put right in one pass.
    #[error(
        "this Protocol's SQL installs a DuckDB extension arc has not vetted, so no step was run:\n{}\n\
         A step or hook may install a community extension on the vetted list, FROM community. \
         The list, and what this check does not read, are at {}",
        indented(refusals),
        crate::engine::VETTED_EXTENSIONS_DOC
    )]
    ExtensionRefused { refusals: Vec<String> },

    // A community extension the Protocol's SQL installs is not the build `arcform.yaml`
    // pins for this DuckDB and platform, or arc could not install it to compare. Checked
    // before any step or hook runs, so nothing ran; one line per extension, and each line
    // says which of the two it was, so the first line claims neither.
    #[error(
        "arc could not confirm that each extension this Protocol installs is the build its arcform.yaml pins, so no step or hook was run:\n{}\n\
         A pin sits under the extensions: key of arcform.yaml. See {}",
        indented(refusals),
        crate::engine::VETTED_EXTENSIONS_DOC
    )]
    ExtensionPinRefused { refusals: Vec<String> },

    // A step's or hook's SQL file, read again just before it ran, installs a DuckDB extension
    // from somewhere arc has not vetted: a step earlier in the run wrote over the file after
    // the check before the run read it, or wrote it where there was none. A step refused here
    // is a step that fails, so the message does not say that no step ran: the steps before it
    // ran and their tables stay, and it and the steps after it do not run. `step` is `None`
    // for a hook.
    #[error(
        "{place} was refused and did not run: its SQL file, read again just before it ran, installs a DuckDB extension arc has not vetted:\n{}\n\
         A step or hook may install a community extension on the vetted list, FROM community. \
         The list, and what this check does not read, are at {}",
        indented(refusals),
        crate::engine::VETTED_EXTENSIONS_DOC
    )]
    ExtensionRefusedBeforeItRan {
        place: String,
        step: Option<String>,
        refusals: Vec<String>,
    },

    // A community extension that a step's or hook's SQL file installs, first found when the
    // file was read again just before it ran, is not the build `arcform.yaml` pins for this
    // DuckDB and platform, or arc could not install it to compare. Refused as
    // `ExtensionRefusedBeforeItRan` is.
    #[error(
        "{place} was refused and did not run: arc could not confirm that each extension its SQL file, read again just before it ran, installs is the build its arcform.yaml pins:\n{}\n\
         A pin sits under the extensions: key of arcform.yaml. See {}",
        indented(refusals),
        crate::engine::VETTED_EXTENSIONS_DOC
    )]
    ExtensionPinRefusedBeforeItRan {
        place: String,
        step: Option<String>,
        refusals: Vec<String>,
    },

    // `arc upgrade` was asked to pin an extension that is not on the vetted list. Refused
    // before DuckDB is asked anything.
    #[error(
        "arc upgrade pins a community extension on the vetted list, and {name} is not on it, so arcform.yaml was not changed. The list is at {}",
        crate::engine::VETTED_EXTENSIONS_DOC
    )]
    ExtensionNotVetted { name: String },

    // `arc upgrade` could not find the build to pin: DuckDB's version or platform, the
    // install, the load, or the installed file. Nothing was written.
    #[error("arc could not pin {name}, so arcform.yaml was not changed: {reason}")]
    ExtensionUpgradeFailed { name: String, reason: String },

    // A pinned extension's file differs from its pin when the run ends: a step replaced
    // it, and steps after that one may have loaded it.
    #[error(
        "an extension this Protocol installs changed during the run, so the run failed:\n{}\n\
         A step that runs FORCE INSTALL, UPDATE EXTENSIONS or a command can replace an installed extension, \
         and steps after it may have loaded the file. See {}",
        indented(changes),
        crate::engine::VETTED_EXTENSIONS_DOC
    )]
    ExtensionChanged { changes: Vec<String> },

    // DuckDB, asked by arc itself rather than by a step, did not answer.
    #[error("DuckDB could not {what}: {reason}")]
    EngineQuery { what: String, reason: String },

    // `ARC_SQL_READER` names no reader, or names DuckDB's parse and the DuckDB arc runs
    // cannot give it. The message names the variable and what it takes.
    #[error("{0}")]
    SqlReader(String),

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
            | Error::PipelineTimeout { .. }
            | Error::ExtensionChanged { .. }
            | Error::ExtensionRefusedBeforeItRan { .. }
            | Error::ExtensionPinRefusedBeforeItRan { .. } => 2,
            _ => 1,
        }
    }

    /// Whether the check just before a step or hook runs refused it.
    pub(crate) fn refused_before_it_ran(&self) -> bool {
        matches!(
            self,
            Error::ExtensionRefusedBeforeItRan { .. }
                | Error::ExtensionPinRefusedBeforeItRan { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Each line on its own line, indented, for a message that lists several.
fn indented(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

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
    fn db_root_missing_names_the_env_var_and_the_db_line() {
        let s = Error::DbRootMissing.to_string();
        assert!(!s.contains('\n'), "Display must be single-line: {:?}", s);
        assert!(s.starts_with("database:"), "family prefix: {:?}", s);
        assert!(s.contains("ARCFORM_DB_DIR"), "remediation env var: {:?}", s);
        assert!(s.contains("db:"), "the other remedy, a db: line: {:?}", s);
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
    fn history_way_names_the_word_and_why_on_one_line() {
        let e = Error::HistoryWay {
            way: "a\nb".into(),
            detail: "'\\n' is not an ASCII letter, a digit, `-` or `_`".into(),
        };
        let s = e.to_string();
        assert!(!s.contains('\n'), "Display must be single-line: {:?}", s);
        assert!(s.starts_with("history:"), "family prefix: {:?}", s);
        assert!(s.contains(r#""a\nb""#), "the word, escaped: {:?}", s);
        assert!(s.contains("is not an ASCII letter"), "why: {:?}", s);
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
            Error::EngineBinInvalid {
                var: "ARC_DUCKDB_BIN",
                path: PathBuf::from("/nope/duckdb"),
                reason: "no such file".into(),
            },
            Error::UntestedEngine {
                found: "2.0.0".into(),
                range: ">=1.2, <2",
                override_var: "ARC_ALLOW_UNTESTED_ENGINE",
            },
            Error::StateBackend("locked".into()),
            Error::ExtensionPinRefused {
                refusals: vec!["mlpack".into()],
            },
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
            Error::ExtensionChanged {
                changes: vec!["mlpack".into()],
            },
            Error::ExtensionRefusedBeforeItRan {
                place: "step 's'".into(),
                step: Some("s".into()),
                refusals: vec!["anofox_forecast".into()],
            },
            Error::ExtensionPinRefusedBeforeItRan {
                place: "hook on_success 'h'".into(),
                step: None,
                refusals: vec!["mlpack".into()],
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
