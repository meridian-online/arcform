use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::precondition::Precondition;

/// The manifest filename `arc` looks for in a protocol directory.
///
/// Published so a tool that writes a spec addresses the same file the loader reads,
/// rather than hardcoding the string a second time.
pub const MANIFEST_FILENAME: &str = "arcform.yaml";

/// A declared pipeline parameter with an optional default value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    /// Default value. If None, the parameter is required (must be provided via CLI or dotenv).
    #[serde(default)]
    pub default: Option<String>,
}

/// Retry policy for a step — retry on failure with exponential backoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of attempts (including the first). Must be >= 1.
    pub max_attempts: u32,

    /// Base backoff in seconds. Actual delay: backoff_sec * 2^(attempt-1).
    /// Defaults to 1.0.
    #[serde(default = "default_backoff")]
    pub backoff_sec: f64,
}

fn default_backoff() -> f64 {
    1.0
}

/// Manifest-level defaults inheritable by steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    /// Default retry policy applied to steps that don't declare their own.
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
}

/// The top-level ArcForm project manifest (arcform.yaml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Project name.
    pub name: String,

    /// Target SQL engine CLI identifier (default: "duckdb").
    #[serde(default = "default_engine")]
    pub engine: String,

    /// Semver version constraint for the engine CLI (e.g. ">=1.5").
    /// Optional — omitting skips version check. Uses Cargo-style syntax.
    #[serde(default)]
    pub engine_version: Option<String>,

    /// The build of each community extension the Protocol's SQL installs: extension name,
    /// then the DuckDB version as DuckDB prints it (`v1.5.5`), then the platform as
    /// `PRAGMA platform` prints it (`linux_amd64`), then the SHA-256 of the installed file
    /// as 64 lower-case hex digits. `arc run` installs each one the SQL installs and has a
    /// pin for, before any step or hook runs, and refuses a run on which the file differs.
    /// `arc run` never writes this key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,

    /// Path to the database file, relative to the manifest directory. When unset the
    /// database is `<name>.duckdb` in arc's data folder, `$ARCFORM_DB_DIR` or
    /// `~/.arcform/db`, under a key made from the Protocol directory's canonical path,
    /// so the directory carries no database. An unset value is left out of a
    /// manifest arc writes: `arc create-protocol` writes this key only when `--db`
    /// names a path, and `arc init` and `arc init --from-descriptor` write none. A
    /// manifest carrying `db: null` still loads with it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,

    /// Named runtime parameters with optional defaults.
    /// Insertion order is preserved for deterministic merge semantics.
    #[serde(default)]
    pub params: IndexMap<String, Param>,

    /// Paths to dotenv files, loaded in declared order (later overrides earlier).
    /// Missing files are silently skipped.
    #[serde(default)]
    pub dotenv: Vec<String>,

    /// Pipeline-level timeout in seconds. If total execution exceeds this,
    /// the pipeline halts with a PipelineTimeout error.
    #[serde(default)]
    pub timeout_sec: Option<f64>,

    /// Manifest-level defaults inheritable by steps.
    #[serde(default)]
    pub defaults: Option<Defaults>,

    /// Lifecycle hooks (on_init, on_success, on_failure, on_exit).
    #[serde(default)]
    pub hooks: Hooks,

    /// Ordered list of transform steps.
    #[serde(default)]
    pub steps: Vec<Step>,

    /// Optional asset overrides. Keys are asset names; values specify
    /// which step produces the asset and its dependencies.
    #[serde(default)]
    pub assets: std::collections::HashMap<String, AssetOverride>,

    /// The finished files and folders a run leaves for publishing, as paths relative to
    /// the manifest directory. Load refuses an absolute path, a `..` or `.` component, a
    /// duplicate and a path inside `build/.arcform/`. After the last step of a run that
    /// would otherwise succeed, `arc run` fails the run when one is missing, and records
    /// each one's files with their sizes and SHA-256 in the run record, which is what the
    /// runner image uploads. Left out of a manifest serialised from this struct when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,

    /// How far back `measure_history` reads: the last `runs: N` runs (default 30) or the
    /// runs `since: <duration>` ago (`90d`, `12h`). Exactly one of the two.
    #[serde(default, skip_serializing_if = "MeasureHistory::is_default")]
    pub measure_history: MeasureHistory,

    /// Whether a failing check records a sample of the rows it returned (default true).
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub check_samples: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// The window `measure_history` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasureHistory {
    /// The most recent N runs; N is at least 1.
    Runs(u32),
    /// The runs within this long of now.
    Since(std::time::Duration),
}

impl MeasureHistory {
    fn is_default(&self) -> bool {
        *self == MeasureHistory::default()
    }
}

impl Default for MeasureHistory {
    fn default() -> Self {
        MeasureHistory::Runs(30)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeasureHistoryRaw {
    runs: Option<u32>,
    since: Option<String>,
}

fn invalid<E: serde::de::Error>(message: impl std::fmt::Display) -> E {
    E::custom(message)
}

impl<'de> Deserialize<'de> for MeasureHistory {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = MeasureHistoryRaw::deserialize(d)?;
        match (raw.runs, raw.since) {
            (Some(0), None) => Err(invalid::<D::Error>(
                "measure_history.runs must be at least 1: say how many runs to read",
            )),
            (Some(n), None) => Ok(MeasureHistory::Runs(n)),
            (None, Some(since)) => match humantime::parse_duration(&since) {
                Ok(d) if d.is_zero() => Err(invalid::<D::Error>(
                    "measure_history.since must be longer than zero: write one such as 90d or 12h",
                )),
                Ok(d) => Ok(MeasureHistory::Since(d)),
                Err(e) => Err(invalid::<D::Error>(format!(
                    "measure_history.since '{since}' is not a duration ({e}): write one such as 90d or 12h"
                ))),
            },
            (Some(_), Some(_)) => Err(invalid::<D::Error>(
                "measure_history takes runs: or since:, not both",
            )),
            (None, None) => Err(invalid::<D::Error>("measure_history needs runs: or since:")),
        }
    }
}

impl Serialize for MeasureHistory {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(1))?;
        match self {
            MeasureHistory::Runs(n) => map.serialize_entry("runs", n)?,
            MeasureHistory::Since(d) => {
                map.serialize_entry("since", &humantime::format_duration(*d).to_string())?
            }
        }
        map.end()
    }
}

/// How seriously a failing check is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A failure fails the run and stops publishing.
    Error,
    /// A failure is recorded and reported; the run carries on.
    Warn,
}

impl Severity {
    /// The word the manifest, the record and the output use for it.
    #[allow(dead_code)] // read by the runner once checks run
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warn => "warn",
        }
    }
}

fn default_engine() -> String {
    "duckdb".to_string()
}

/// A single step in the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Unique name for this step.
    pub name: String,

    /// Relative path to a SQL file. Mutually exclusive with `command`.
    #[serde(default)]
    pub sql: Option<String>,

    /// What the step does and why, in one line of its author's or its agent's
    /// words, for whoever reads the Protocol next. It is read and written and
    /// never run: a step runs the same with or without one. `arc operation
    /// record --description` and `arc mcp`'s `operation_record` write it, and
    /// refuse one that is empty or spans lines. Left out of a manifest
    /// serialised from this struct when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Raw shell command string. Mutually exclusive with `sql`/`op`.
    #[serde(default)]
    pub command: Option<String>,

    /// First-class operator reference: `op: <name>@<semver>`. Mutually exclusive with
    /// `sql`/`command`. Configured via the typed `with:` block; resolved from the
    /// operator catalog. `command:` is the flagged escape hatch — prefer `op:`.
    #[serde(default)]
    pub op: Option<String>,

    /// Typed config for an `op:` step, validated against the operator's schema at load.
    #[serde(default)]
    pub with: Option<serde_yaml::Value>,

    /// Assets this step produces (primarily for command steps).
    /// SQL steps auto-discover their outputs via sqlparser-rs.
    #[serde(default)]
    pub produces: Vec<String>,

    /// Assets this step depends on (primarily for command steps).
    /// SQL steps auto-discover their inputs via sqlparser-rs.
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// Typed preconditions for freshness checks.
    /// If all preconditions pass, the step is considered fresh and may be skipped.
    /// Empty = no preconditions (command steps always re-run, SQL steps use hash).
    #[serde(default)]
    pub preconditions: Vec<Precondition>,

    /// Output capture name. When set on a command step, stdout is piped and captured,
    /// then injected as ARC_PARAM_{OUTPUT_UPPERCASED} for downstream steps.
    /// Mutually exclusive with SQL steps (validated at manifest load).
    #[serde(default)]
    pub output: Option<String>,

    /// Retry policy for this step. Overrides manifest defaults.retry wholesale.
    #[serde(default)]
    pub retry: Option<RetryPolicy>,

    /// Step-level timeout in seconds. Clamped to remaining pipeline time.
    #[serde(default)]
    pub timeout_sec: Option<f64>,

    /// Relative path to a SQL file that selects the rows that break a rule. A check
    /// passes when it returns none. Needs `risk:`. Mutually exclusive with the other
    /// step arms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,

    /// Relative path to a SQL file that selects one row of numbers to record each run.
    /// Mutually exclusive with the other step arms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measure: Option<String>,

    /// What goes wrong for the analyst if a check fails, in one line. Required on a
    /// check, refused elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,

    /// `error` (the default) or `warn`. Only a check takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

/// Lifecycle hooks for pipeline setup, teardown, and notification.
///
/// Each hook is an optional Step. Execution order:
/// on_init → steps[0..n] → (on_success | on_failure) → on_exit
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    /// Runs before any pipeline step. Fatal on failure.
    #[serde(default)]
    pub on_init: Option<Step>,

    /// Runs after all steps succeed.
    #[serde(default)]
    pub on_success: Option<Step>,

    /// Runs when a step fails. Receives ARC_FAILED_STEP, ARC_EXIT_CODE.
    #[serde(default)]
    pub on_failure: Option<Step>,

    /// Always runs if on_init was attempted. Receives ARC_PIPELINE_STATUS.
    #[serde(default)]
    pub on_exit: Option<Step>,
}

/// An asset override entry in the top-level `assets:` section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetOverride {
    /// Name of the step that produces this asset.
    pub produced_by: String,

    /// Asset names this asset depends on.
    #[serde(default)]
    pub depends_on: Vec<String>,
}

impl Step {
    /// A bare step with just a name — every other field at its empty default.
    /// Callers set `sql`/`command`/`op` and any deps they need. Used by the
    /// descriptor bridge to build generated steps.
    pub(crate) fn new(name: &str) -> Self {
        Step {
            name: name.to_string(),
            sql: None,
            description: None,
            command: None,
            op: None,
            with: None,
            produces: Vec::new(),
            depends_on: Vec::new(),
            preconditions: Vec::new(),
            output: None,
            retry: None,
            timeout_sec: None,
            check: None,
            measure: None,
            risk: None,
            severity: None,
        }
    }

    /// The SQL file this step runs: its `sql`, `check` or `measure` path, whichever is set.
    pub(crate) fn sql_file(&self) -> Option<&str> {
        self.sql
            .as_deref()
            .or(self.check.as_deref())
            .or(self.measure.as_deref())
    }

    /// True for a `check:` step.
    pub(crate) fn is_check(&self) -> bool {
        self.check.is_some()
    }

    /// True for a `measure:` step.
    pub(crate) fn is_measure(&self) -> bool {
        self.measure.is_some()
    }

    /// The step's severity, `error` when it names none.
    #[allow(dead_code)] // read by the runner once checks run
    pub(crate) fn severity_or_default(&self) -> Severity {
        self.severity.unwrap_or(Severity::Error)
    }
}

/// Refuse an `outputs:` entry that could name something outside the Protocol, that names
/// arc's own run state, or that repeats another entry once a trailing `/` is dropped.
fn validate_outputs(outputs: &[String]) -> Result<()> {
    use std::path::Component;
    let mut seen = std::collections::HashSet::new();
    for raw in outputs {
        let refuse = |why: &str| Err(Error::ManifestValidation(format!("outputs: '{raw}' {why}")));
        let trimmed = raw.trim_end_matches('/');
        if trimmed.is_empty() {
            return refuse("is empty: an output is a path relative to the Protocol's folder");
        }
        let path = Path::new(trimmed);
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
                Component::ParentDir => {
                    return refuse(
                        "has a '..' component: an output stays inside the Protocol's folder",
                    );
                }
                Component::CurDir => {
                    return refuse("has a '.' or empty component: write the path plainly");
                }
                Component::RootDir | Component::Prefix(_) => {
                    return refuse(
                        "is absolute: an output is a path relative to the Protocol's folder",
                    );
                }
            }
        }
        // `Path::components` drops `.` components and repeated separators, so check the
        // raw segments for those as written.
        if trimmed.split('/').any(|seg| seg == "." || seg.is_empty()) {
            return refuse("has a '.' or empty component: write the path plainly");
        }
        if parts.len() >= 2 && parts[0] == "build" && parts[1] == ".arcform" || parts == ["build"] {
            return refuse(
                "is or holds arc's run state under build/.arcform/, which is not an output",
            );
        }
        let joined = parts.join("/");
        // An output inside another would have its files recorded, and uploaded, twice.
        if let Some(other) = seen.iter().find(|other: &&String| {
            joined.starts_with(&format!("{other}/")) || other.starts_with(&format!("{joined}/"))
        }) {
            return refuse(&format!(
                "is inside, or holds, the declared output '{other}': declare one of them"
            ));
        }
        if !seen.insert(joined) {
            return refuse("is declared twice");
        }
    }
    Ok(())
}

/// Validate a retry policy's fields.
fn validate_retry_policy(policy: &RetryPolicy, context: &str) -> Result<()> {
    if policy.max_attempts < 1 {
        return Err(Error::ManifestValidation(format!(
            "{}: retry max_attempts must be >= 1, got {}",
            context, policy.max_attempts
        )));
    }
    if policy.backoff_sec < 0.0 {
        return Err(Error::ManifestValidation(format!(
            "{}: retry backoff_sec must be >= 0, got {}",
            context, policy.backoff_sec
        )));
    }
    Ok(())
}

impl Manifest {
    /// Load and validate `arcform.yaml` from the given directory.
    ///
    /// Errors: [`Error::ManifestNotFound`] if the file is absent, [`Error::FileRead`]
    /// if it cannot be read, [`Error::ManifestParse`] if it is not YAML in the shape of
    /// a manifest, [`Error::ManifestValidation`] if the shape is right but the content
    /// is not — see [`Manifest::from_yaml_str`] for what that covers.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST_FILENAME);
        if !path.exists() {
            return Err(Error::ManifestNotFound);
        }
        let contents = std::fs::read_to_string(&path).map_err(|e| Error::FileRead {
            path: path.clone(),
            source: e,
        })?;
        Self::from_yaml_str(&contents)
    }

    /// Parse and validate manifest YAML that is not (yet) on disk.
    ///
    /// The same parse and the same checks [`Manifest::load`] performs, minus the file
    /// read — so a caller holding candidate bytes can ask whether `arc` would accept
    /// them before those bytes become a file. Parsing here is a gate: the returned
    /// [`Manifest`] is a by-product, and a caller that only wants the verdict should
    /// discard it and write the bytes it already has. Nothing here re-serialises, so
    /// the comments, key order and formatting in those bytes survive untouched.
    ///
    /// Checks the manifest in isolation: names non-empty, unique and free of `:`, exactly
    /// one of `sql`/`command`/`op`/`check`/`measure` per step, a `risk:` on each check and
    /// none on any other step, no `produces:`/`output:`/`preconditions:` on a check or measure, `op:` steps resolvable against the operator
    /// catalog with a `with:` block that deserialises, well-formed preconditions,
    /// retry policies and `engine_version` constraint, and the hook restrictions. It
    /// does not touch the filesystem — referenced SQL files, dotenv files and fetched
    /// artifacts are the runner's preflight, not this.
    pub fn from_yaml_str(yaml: &str) -> Result<Self> {
        let manifest: Manifest = serde_yaml::from_str(yaml)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Where this Protocol's working database is: `db:` relative to the manifest
    /// directory when the manifest names one, else `<name>.duckdb` in arc's data folder
    /// under a key made from the directory's canonical path (see [`crate::working_db`]).
    /// Refused when the directory cannot be canonicalised or the data folder cannot be
    /// resolved. It reads the filesystem and writes nothing.
    pub fn db_path(&self, manifest_dir: &Path) -> Result<PathBuf> {
        match &self.db {
            Some(db) => Ok(manifest_dir.join(db)),
            None => crate::working_db::locate(&self.name, manifest_dir).map(|db| db.path),
        }
    }

    /// Validate manifest constraints.
    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::ManifestValidation(
                "project name cannot be empty".to_string(),
            ));
        }

        // Validate defaults.retry if present.
        if let Some(ref defaults) = self.defaults
            && let Some(ref retry) = defaults.retry
        {
            validate_retry_policy(retry, "defaults")?;
        }

        // Validate engine_version is parseable semver constraint.
        if let Some(ref ev) = self.engine_version
            && semver::VersionReq::parse(ev).is_err()
        {
            return Err(Error::ManifestValidation(format!(
                "invalid engine_version '{}': must be a valid semver constraint (e.g. '>=1.5')",
                ev
            )));
        }

        // A pin is compared with a file's SHA-256, so one that cannot be a SHA-256 is refused
        // here rather than read as a changed build on every run.
        for (name, versions) in &self.extensions {
            for (version, platforms) in versions {
                for (platform, pin) in platforms {
                    if pin.len() != 64
                        || !pin.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    {
                        return Err(Error::ManifestValidation(format!(
                            "extensions.{name}.{version}.{platform} is '{pin}', which is not a SHA-256: \
                             a pin is 64 lower-case hex digits"
                        )));
                    }
                }
            }
        }

        let mut seen_names = std::collections::HashSet::new();
        for (i, step) in self.steps.iter().enumerate() {
            // Check for empty step names.
            if step.name.trim().is_empty() {
                return Err(Error::ManifestValidation(format!(
                    "step {} has an empty name",
                    i + 1
                )));
            }

            if step.name.contains(':') {
                return Err(Error::ManifestValidation(format!(
                    "step '{}': a step name cannot contain ':', which names a table or file in a check; rename the step",
                    step.name
                )));
            }

            // Check for duplicate step names.
            if !seen_names.insert(&step.name) {
                return Err(Error::ManifestValidation(format!(
                    "duplicate step name: '{}'",
                    step.name
                )));
            }

            // Validate preconditions.
            for precondition in &step.preconditions {
                precondition.validate().map_err(|e| {
                    Error::ManifestValidation(format!("step '{}': {}", step.name, e))
                })?;
            }

            // Each step must have exactly one of sql, command, op, check or measure.
            let arms = [
                step.sql.is_some(),
                step.command.is_some(),
                step.op.is_some(),
                step.check.is_some(),
                step.measure.is_some(),
            ];
            match arms.iter().filter(|present| **present).count() {
                0 => {
                    return Err(Error::ManifestValidation(format!(
                        "step '{}': must have one of 'sql', 'command', 'op', 'check' or 'measure'",
                        step.name
                    )));
                }
                1 => {}
                _ => {
                    return Err(Error::ManifestValidation(format!(
                        "step '{}': must have exactly one of 'sql', 'command', 'op', 'check' or 'measure', not several",
                        step.name
                    )));
                }
            }

            if step.is_check() || step.is_measure() {
                let kind = if step.is_check() { "check" } else { "measure" };
                for (set, field) in [
                    (!step.produces.is_empty(), "produces"),
                    (step.output.is_some(), "output"),
                    (!step.preconditions.is_empty(), "preconditions"),
                ] {
                    if set {
                        return Err(Error::ManifestValidation(format!(
                            "step '{}': a {kind} step cannot declare {field}: it produces nothing and always runs; remove it",
                            step.name
                        )));
                    }
                }
            }
            if step.is_check() {
                match step.risk.as_deref() {
                    None => {
                        return Err(Error::ManifestValidation(format!(
                            "step '{}': a check needs risk: one line on what goes wrong for the analyst if it fails",
                            step.name
                        )));
                    }
                    Some(risk) if risk.trim().is_empty() || risk.contains(['\n', '\r']) => {
                        return Err(Error::ManifestValidation(format!(
                            "step '{}': risk: must be one non-empty line",
                            step.name
                        )));
                    }
                    Some(_) => {}
                }
            } else {
                for (set, field) in [
                    (step.risk.is_some(), "risk:"),
                    (step.severity.is_some(), "severity:"),
                ] {
                    if set {
                        return Err(Error::ManifestValidation(format!(
                            "step '{}': only a check step takes {field}; remove it or make the step a check",
                            step.name
                        )));
                    }
                }
            }

            // Validate op steps at load: the operator must exist, its version must be
            // satisfied, and its `with:` config must deserialize (fail fast, not at run).
            if let Some(ref op_ref) = step.op {
                crate::operator::assets_for(op_ref, step.with.as_ref()).map_err(|e| {
                    Error::ManifestValidation(format!("step '{}': {}", step.name, e))
                })?;
            }

            // SQL steps cannot declare an output field.
            if step.sql.is_some() && step.output.is_some() {
                return Err(Error::ManifestValidation(format!(
                    "step '{}': SQL steps cannot declare an output field",
                    step.name
                )));
            }

            // Validate step-level retry policy if present.
            if let Some(ref retry) = step.retry {
                validate_retry_policy(retry, &format!("step '{}'", step.name))?;
            }
        }

        validate_outputs(&self.outputs)?;

        // Validate lifecycle hooks.
        let hook_entries: [(&str, &Option<Step>); 4] = [
            ("hooks.on_init", &self.hooks.on_init),
            ("hooks.on_success", &self.hooks.on_success),
            ("hooks.on_failure", &self.hooks.on_failure),
            ("hooks.on_exit", &self.hooks.on_exit),
        ];

        for (label, hook_opt) in &hook_entries {
            if let Some(hook) = hook_opt {
                // Hook must have sql xor command.
                match (&hook.sql, &hook.command) {
                    (Some(_), Some(_)) => {
                        return Err(Error::ManifestValidation(format!(
                            "{}: must have either 'sql' or 'command', not both",
                            label
                        )));
                    }
                    (None, None) => {
                        return Err(Error::ManifestValidation(format!(
                            "{}: must have either 'sql' or 'command'",
                            label
                        )));
                    }
                    _ => {}
                }

                // Hook name must not be empty.
                if hook.name.trim().is_empty() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hook has an empty name",
                        label
                    )));
                }

                // Hook name must not collide with pipeline step names.
                if seen_names.contains(&hook.name) {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hook name '{}' collides with a pipeline step name",
                        label, hook.name
                    )));
                }

                // Hooks must not have preconditions.
                if !hook.preconditions.is_empty() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot have preconditions",
                        label
                    )));
                }

                // Hooks must not have produces.
                if !hook.produces.is_empty() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot have produces",
                        label
                    )));
                }

                // Hooks must not have depends_on.
                if !hook.depends_on.is_empty() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot have depends_on",
                        label
                    )));
                }

                // Hooks cannot be checks or measures, and take neither risk nor severity.
                if hook.check.is_some() || hook.measure.is_some() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot be a check or measure",
                        label
                    )));
                }
                if hook.risk.is_some() || hook.severity.is_some() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot have risk or severity: only a check step takes them",
                        label
                    )));
                }

                // Hooks must not have retry.
                if hook.retry.is_some() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot have retry",
                        label
                    )));
                }

                // Hooks must not have timeout_sec.
                if hook.timeout_sec.is_some() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot have timeout_sec",
                        label
                    )));
                }

                // Hooks must not have output.
                if hook.output.is_some() {
                    return Err(Error::ManifestValidation(format!(
                        "{}: hooks cannot have output",
                        label
                    )));
                }
            }
        }

        Ok(())
    }

    /// Generate a default manifest for a new project. It names no database: `db` is
    /// unset, so the manifest is written without the key and its database is
    /// `<name>.duckdb` in arc's data folder; a caller with a path sets `db` after.
    pub(crate) fn new_project(name: &str) -> Self {
        Manifest {
            name: name.to_string(),
            engine: "duckdb".to_string(),
            engine_version: Some(">=1.0".to_string()),
            extensions: BTreeMap::new(),
            db: None,
            params: IndexMap::new(),
            dotenv: Vec::new(),
            timeout_sec: None,
            defaults: None,
            hooks: Hooks::default(),
            steps: Vec::new(),
            assets: std::collections::HashMap::new(),
            outputs: Vec::new(),
            measure_history: Default::default(),
            check_samples: true,
        }
    }

    /// Check if any steps or hooks use the SQL engine.
    pub(crate) fn has_sql_steps(&self) -> bool {
        let hook_has_sql = [
            &self.hooks.on_init,
            &self.hooks.on_success,
            &self.hooks.on_failure,
            &self.hooks.on_exit,
        ]
        .iter()
        .any(|h| h.as_ref().is_some_and(|s| s.sql_file().is_some()));
        self.steps.iter().any(|s| s.sql_file().is_some()) || hook_has_sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a SQL step with no asset declarations.
    fn sql_step(name: &str, sql: &str) -> Step {
        Step {
            name: name.to_string(),
            sql: Some(sql.to_string()),
            description: None,
            command: None,
            produces: vec![],
            depends_on: vec![],
            preconditions: vec![],
            op: None,
            with: None,
            output: None,
            retry: None,
            timeout_sec: None,
            check: None,
            measure: None,
            risk: None,
            severity: None,
        }
    }

    /// Helper: create a command step with no asset declarations.
    fn cmd_step(name: &str, command: &str) -> Step {
        Step {
            name: name.to_string(),
            sql: None,
            description: None,
            command: Some(command.to_string()),
            produces: vec![],
            depends_on: vec![],
            preconditions: vec![],
            op: None,
            with: None,
            output: None,
            retry: None,
            timeout_sec: None,
            check: None,
            measure: None,
            risk: None,
            severity: None,
        }
    }

    /// Helper: create a test manifest with given steps and no asset overrides.
    fn test_manifest(name: &str, steps: Vec<Step>) -> Manifest {
        Manifest {
            name: name.to_string(),
            engine: "duckdb".to_string(),
            engine_version: None,
            extensions: BTreeMap::new(),
            db: None,
            params: IndexMap::new(),
            dotenv: Vec::new(),
            timeout_sec: None,
            defaults: None,
            hooks: Hooks::default(),
            steps,
            assets: std::collections::HashMap::new(),
            outputs: Vec::new(),
            measure_history: Default::default(),
            check_samples: true,
        }
    }

    // Manifest::new_project generates correct defaults.
    #[test]
    fn test_new_project_defaults() {
        let m = Manifest::new_project("test-pipeline");
        assert_eq!(m.name, "test-pipeline");
        assert_eq!(m.engine, "duckdb");
        assert_eq!(m.db, None, "a new project names no database");
        let dir = tempfile::tempdir().unwrap();
        let path = m.db_path(dir.path()).unwrap();
        assert!(
            !path.starts_with(dir.path().canonicalize().unwrap()),
            "and its database is not beside the manifest: {}",
            path.display()
        );
        assert!(path.ends_with("test-pipeline.duckdb"), "{}", path.display());
        assert!(m.steps.is_empty());
        assert!(m.assets.is_empty());
    }

    // With no db: the database is <name>.duckdb in arc's data folder, keyed to the
    // directory — the resolution `working_db` makes, not a path beside the manifest.
    #[test]
    fn test_db_path_default() {
        let m = test_manifest("my-proj", vec![]);
        let dir = tempfile::tempdir().unwrap();
        let path = m.db_path(dir.path()).unwrap();
        assert_eq!(
            path,
            crate::working_db::locate("my-proj", dir.path())
                .unwrap()
                .path
        );
        assert!(
            !path.starts_with(dir.path().canonicalize().unwrap()),
            "{}",
            path.display()
        );
    }

    // Explicit db field overrides the default path.
    #[test]
    fn test_db_path_explicit() {
        let mut m = test_manifest("my-proj", vec![]);
        m.db = Some("custom.duckdb".to_string());
        let path = m.db_path(Path::new("/tmp/project")).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/project/custom.duckdb"));
    }

    // Duplicate step names are rejected during validation.
    #[test]
    fn test_validate_duplicate_step_names() {
        let m = test_manifest("test", vec![sql_step("a", "a.sql"), sql_step("a", "b.sql")]);
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate step name"));
    }

    // Step with both sql and command is rejected.
    #[test]
    fn test_validate_step_both_fields() {
        let m = test_manifest(
            "test",
            vec![Step {
                name: "bad".to_string(),
                sql: Some("a.sql".to_string()),
                description: None,
                command: Some("echo hi".to_string()),
                produces: vec![],
                depends_on: vec![],
                preconditions: vec![],
                op: None,
                with: None,
                output: None,
                retry: None,
                timeout_sec: None,
                check: None,
                measure: None,
                risk: None,
                severity: None,
            }],
        );
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("not several"));
    }

    // Step with neither sql nor command is rejected.
    #[test]
    fn test_validate_step_neither_field() {
        let m = test_manifest(
            "test",
            vec![Step {
                name: "bad".to_string(),
                sql: None,
                description: None,
                command: None,
                produces: vec![],
                depends_on: vec![],
                preconditions: vec![],
                op: None,
                with: None,
                output: None,
                retry: None,
                timeout_sec: None,
                check: None,
                measure: None,
                risk: None,
                severity: None,
            }],
        );
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("must have one of"));
    }

    // Missing arcform.yaml produces a clear error.
    #[test]
    fn test_load_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let err = Manifest::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // Valid manifest loads and parses correctly.
    #[test]
    fn test_load_valid_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine: duckdb\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.name, "test");
        assert_eq!(m.steps.len(), 1);
    }

    // Malformed YAML produces a parse error.
    #[test]
    fn test_load_malformed_yaml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("arcform.yaml"), "{{{{invalid").unwrap();
        let err = Manifest::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("parse"));
    }

    // has_sql_steps gates whether preflight is called.
    #[test]
    fn test_has_sql_steps() {
        let m = test_manifest("test", vec![cmd_step("cmd", "echo hi")]);
        assert!(!m.has_sql_steps());
    }

    // Steps maintain declaration order (Vec, not HashMap).
    #[test]
    fn test_sequential_order_preserved() {
        let m = test_manifest(
            "test",
            vec![
                sql_step("c", "c.sql"),
                sql_step("a", "a.sql"),
                sql_step("b", "b.sql"),
            ],
        );
        let names: Vec<&str> = m.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    // Command steps parse produces and depends_on from YAML.
    #[test]
    fn test_command_step_with_asset_fields() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: export
    command: "duckdb db.duckdb -c \"COPY customers TO 'out.csv'\""
    produces:
      - customers_csv
    depends_on:
      - customers
"#;
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.steps[0].produces, vec!["customers_csv"]);
        assert_eq!(m.steps[0].depends_on, vec!["customers"]);
    }

    // Top-level assets section parses correctly.
    #[test]
    fn test_assets_override_section() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
name: test
steps:
  - name: load
    sql: models/load.sql
assets:
  customers:
    produced_by: load
    depends_on:
      - raw_data
      - lookups
"#;
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        let asset = m.assets.get("customers").expect("asset should exist");
        assert_eq!(asset.produced_by, "load");
        assert_eq!(asset.depends_on, vec!["raw_data", "lookups"]);
    }

    // Manifest without asset fields works identically to v0.1.
    #[test]
    fn test_v1_manifest_backwards_compatible() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert!(m.steps[0].produces.is_empty());
        assert!(m.steps[0].depends_on.is_empty());
        assert!(m.assets.is_empty());
    }

    // Manifest with engine_version parses correctly.
    #[test]
    fn test_lrp_manifest_with_engine_version() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "name: test\nengine_version: '>=1.5'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.engine_version, Some(">=1.5".to_string()));
    }

    // Manifest without engine_version is backwards compatible.
    #[test]
    fn test_lrp_manifest_without_engine_version() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert!(m.engine_version.is_none());
    }

    // Invalid engine_version syntax is rejected.
    #[test]
    fn test_lrp_invalid_engine_version_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let yaml =
            "name: test\nengine_version: 'banana'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let err = Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("engine_version"),
            "error should mention engine_version: {msg}"
        );
        assert!(
            msg.contains("banana"),
            "error should show the invalid value: {msg}"
        );
    }

    // arc init scaffolds engine_version.
    #[test]
    fn test_lrp_init_scaffolds_engine_version() {
        let m = Manifest::new_project("test-pipeline");
        assert!(
            m.engine_version.is_some(),
            "new_project should include engine_version"
        );
        let ev = m.engine_version.unwrap();
        assert!(!ev.is_empty(), "engine_version should not be empty");
        // Should be a valid semver constraint.
        assert!(
            semver::VersionReq::parse(&ev).is_ok(),
            "engine_version '{}' should be valid semver",
            ev
        );
    }

    const PIN: &str = "e097b5dfe9ba6de62b87791857cf7f173049f3dc3ec4ad5b32deeca0d3fddacc";

    #[test]
    fn extensions_reads_a_pin_per_extension_version_and_platform() {
        let manifest = Manifest::from_yaml_str(&format!(
            "name: p\nengine_version: \">=1.5\"\nextensions:\n  mlpack:\n    v1.5.5:\n      linux_amd64: {PIN}\n"
        ))
        .unwrap();
        assert_eq!(
            manifest.extensions["mlpack"]["v1.5.5"]["linux_amd64"], PIN,
            "{:?}",
            manifest.extensions
        );
    }

    #[test]
    fn a_pin_that_is_not_a_sha256_is_refused_naming_its_path() {
        for bad in [
            &PIN[1..],
            &PIN.to_uppercase(),
            &format!("{}g", &PIN[1..]),
            "sha256:e097",
        ] {
            let err = Manifest::from_yaml_str(&format!(
                "name: p\nextensions:\n  mlpack:\n    v1.5.5:\n      linux_amd64: '{bad}'\n"
            ))
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("extensions.mlpack.v1.5.5.linux_amd64")
                    && err.contains("not a SHA-256"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn a_manifest_with_no_pin_writes_no_extensions_key() {
        let yaml = serde_yaml::to_string(&Manifest::new_project("p")).unwrap();
        assert!(!yaml.contains("extensions"), "{yaml}");
    }

    /// The lines of `yaml` that begin `db`, the way an author reading the file sees them.
    fn db_lines(yaml: &str) -> Vec<&str> {
        yaml.lines().filter(|l| l.starts_with("db")).collect()
    }

    #[test]
    fn a_manifest_with_no_db_is_written_without_the_key() {
        let mut m = Manifest::new_project("p");
        assert_eq!(m.db, None);
        let yaml = serde_yaml::to_string(&m).unwrap();
        assert_eq!(db_lines(&yaml), Vec::<&str>::new(), "no db line: {yaml}");

        // Every other unset optional key is written as it was: this change is one key.
        assert!(yaml.contains("timeout_sec: null\n"), "{yaml}");
        assert!(yaml.contains("defaults: null\n"), "{yaml}");

        m.db = Some("work.duckdb".to_string());
        let yaml = serde_yaml::to_string(&m).unwrap();
        assert_eq!(db_lines(&yaml), vec!["db: work.duckdb"], "{yaml}");
    }

    #[test]
    fn a_manifest_written_with_db_null_or_a_path_loads_as_written() {
        let head = "name: p\nengine: duckdb\nengine_version: '>=1.0'\n";
        let tail = "params: {}\ndotenv: []\nsteps: []\nassets: {}\n";

        // Written by an arc before the key was left out, or by hand: `db: null` is unset.
        let unset = Manifest::from_yaml_str(&format!("{head}db: null\n{tail}")).unwrap();
        assert_eq!(unset.db, None);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            unset.db_path(dir.path()).unwrap(),
            crate::working_db::locate("p", dir.path()).unwrap().path
        );

        // No key at all reads the same way.
        let absent = Manifest::from_yaml_str(&format!("{head}{tail}")).unwrap();
        assert_eq!(absent.db, None);

        // An explicit path is that path, and its resolution follows it.
        let named = Manifest::from_yaml_str(&format!("{head}db: state/p.db\n{tail}")).unwrap();
        assert_eq!(named.db.as_deref(), Some("state/p.db"));
        assert_eq!(
            named.db_path(Path::new("/tmp/project")).unwrap(),
            PathBuf::from("/tmp/project/state/p.db")
        );
    }

    fn outputs_error(outputs: &str) -> String {
        Manifest::from_yaml_str(&format!("name: p\noutputs:\n{outputs}"))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn outputs_parse() {
        let manifest = Manifest::from_yaml_str(
            "name: p\noutputs:\n  - build/out.parquet\n  - datapackage.json\n",
        )
        .unwrap();
        assert_eq!(manifest.outputs, ["build/out.parquet", "datapackage.json"]);
        let none = Manifest::from_yaml_str("name: p\n").unwrap();
        assert!(none.outputs.is_empty());
        assert!(
            !serde_yaml::to_string(&none).unwrap().contains("outputs"),
            "an empty list is left out of a written manifest"
        );
    }

    #[test]
    fn outputs_refuse_absolute() {
        let err = outputs_error("  - /etc/passwd\n");
        assert!(
            err.contains("'/etc/passwd'") && err.contains("absolute"),
            "{err}"
        );
    }

    #[test]
    fn outputs_refuse_parent_and_dot() {
        for (bad, why) in [
            ("../secret", "'..'"),
            ("build/../../x", "'..'"),
            ("./build/out.csv", "'.'"),
            ("build/./out.csv", "'.'"),
            ("build//out.csv", "empty"),
            ("/", "empty"),
        ] {
            let err = outputs_error(&format!("  - '{bad}'\n"));
            assert!(
                err.contains(&format!("'{bad}'")) && err.contains(why),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn outputs_refuse_duplicates() {
        let err = outputs_error("  - build/parts\n  - build/parts/\n");
        assert!(
            err.contains("'build/parts/'") && err.contains("twice"),
            "{err}"
        );
    }

    #[test]
    fn outputs_refuse_nesting() {
        for (first, second) in [
            ("build/parts", "build/parts/a.txt"),
            ("build/parts/a.txt", "build/parts/"),
        ] {
            let err = outputs_error(&format!("  - {first}\n  - {second}\n"));
            assert!(
                err.contains(&format!("'{second}'"))
                    && err.contains(&format!("'{}'", first.trim_end_matches('/')))
                    && err.contains("inside"),
                "{err}"
            );
        }
        let ok = Manifest::from_yaml_str("name: p\noutputs:\n  - build/parts\n  - build/parts2\n");
        assert!(ok.is_ok(), "siblings load: {ok:?}");
    }

    #[test]
    fn outputs_refuse_run_state() {
        for bad in [
            "build/.arcform",
            "build/.arcform/runs/x.json",
            "build",
            "build/",
        ] {
            let err = outputs_error(&format!("  - '{bad}'\n"));
            assert!(
                err.contains(&format!("'{bad}'")) && err.contains("run state"),
                "{bad}: {err}"
            );
        }
    }

    fn load_err(yaml: &str) -> String {
        Manifest::from_yaml_str(yaml).unwrap_err().to_string()
    }

    fn check_yaml(extra: &str) -> String {
        format!("name: p\nsteps:\n  - name: a\n    check: checks/a.sql\n{extra}")
    }

    #[test]
    fn a_check_step_loads_with_risk_and_default_severity() {
        let m = Manifest::from_yaml_str(&check_yaml("    risk: x\n")).unwrap();
        let step = &m.steps[0];
        assert!(step.is_check() && !step.is_measure());
        assert_eq!(step.severity_or_default(), Severity::Error);
        assert_eq!(step.sql_file(), Some("checks/a.sql"));
    }

    #[test]
    fn a_check_without_risk_is_refused() {
        assert!(load_err(&check_yaml("")).contains("step 'a': a check needs risk:"));
    }

    #[test]
    fn an_empty_or_multiline_risk_is_refused() {
        assert!(
            load_err(&check_yaml("    risk: \"\"\n")).contains("risk: must be one non-empty line")
        );
        assert!(
            load_err(&check_yaml("    risk: \"a\\nb\"\n"))
                .contains("risk: must be one non-empty line")
        );
    }

    #[test]
    fn severity_warn_loads_and_other_values_are_refused() {
        let m = Manifest::from_yaml_str(&check_yaml("    risk: x\n    severity: warn\n")).unwrap();
        assert_eq!(m.steps[0].severity_or_default(), Severity::Warn);
        assert_eq!(Severity::Warn.as_str(), "warn");
        assert!(matches!(
            Manifest::from_yaml_str(&check_yaml("    risk: x\n    severity: info\n")),
            Err(Error::ManifestParse(_))
        ));
    }

    #[test]
    fn risk_or_severity_on_a_non_check_is_refused() {
        for arm in ["sql: a.sql", "measure: m.sql"] {
            let risk = format!("name: p\nsteps:\n  - name: a\n    {arm}\n    risk: x\n");
            assert!(load_err(&risk).contains("only a check step takes risk:"));
            let sev = format!("name: p\nsteps:\n  - name: a\n    {arm}\n    severity: warn\n");
            assert!(load_err(&sev).contains("only a check step takes severity:"));
        }
    }

    #[test]
    fn check_and_measure_count_as_step_arms() {
        let both = "name: p\nsteps:\n  - name: a\n    sql: a.sql\n    check: c.sql\n    risk: x\n";
        assert!(
            load_err(both)
                .contains("must have exactly one of 'sql', 'command', 'op', 'check' or 'measure'")
        );
        let none = "name: p\nsteps:\n  - name: a\n";
        assert!(
            load_err(none)
                .contains("must have one of 'sql', 'command', 'op', 'check' or 'measure'")
        );
    }

    #[test]
    fn check_and_measure_refuse_produces_output_and_preconditions() {
        let cases = [
            ("check: c.sql\n    risk: x", "produces: [t]", "produces"),
            ("check: c.sql\n    risk: x", "output: o", "output"),
            ("measure: m.sql", "produces: [t]", "produces"),
            ("measure: m.sql", "output: o", "output"),
            (
                "measure: m.sql",
                "preconditions:\n      - command: 'true'",
                "preconditions",
            ),
        ];
        for (arm, extra, field) in cases {
            let yaml = format!("name: p\nsteps:\n  - name: a\n    {arm}\n    {extra}\n");
            let err = load_err(&yaml);
            assert!(err.contains(&format!("cannot declare {field}")), "{err}");
        }
    }

    #[test]
    fn a_step_name_with_a_colon_is_refused() {
        let yaml = "name: p\nsteps:\n  - name: 'table:x'\n    sql: a.sql\n";
        assert!(load_err(yaml).contains("cannot contain ':'"));
    }

    #[test]
    fn measure_history_defaults_to_30_runs_and_reads_runs_or_since() {
        let load = |extra: &str| Manifest::from_yaml_str(&format!("name: p\n{extra}"));
        assert_eq!(load("").unwrap().measure_history, MeasureHistory::Runs(30));
        assert_eq!(
            load("measure_history: { runs: 5 }\n")
                .unwrap()
                .measure_history,
            MeasureHistory::Runs(5)
        );
        assert_eq!(
            load("measure_history: { since: 90d }\n")
                .unwrap()
                .measure_history,
            MeasureHistory::Since(std::time::Duration::from_secs(90 * 86_400))
        );
        for bad in [
            "measure_history: { runs: 0 }\n",
            "measure_history: { since: soon }\n",
            "measure_history: { since: 0s }\n",
            "measure_history: { runs: 5, since: 90d }\n",
        ] {
            assert!(load(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn check_samples_defaults_to_true() {
        assert!(Manifest::from_yaml_str("name: p\n").unwrap().check_samples);
        let off = Manifest::from_yaml_str("name: p\ncheck_samples: false\n").unwrap();
        assert!(!off.check_samples);
    }

    #[test]
    fn a_hook_cannot_be_a_check() {
        let yaml = "name: p\nhooks:\n  on_exit:\n    name: h\n    check: c.sql\n    risk: x\n";
        assert!(load_err(yaml).contains("hooks.on_exit: must have either 'sql' or 'command'"));
        let with_sql = "name: p\nhooks:\n  on_exit:\n    name: h\n    sql: a.sql\n    check: c.sql\n    risk: x\n";
        assert!(load_err(with_sql).contains("hooks.on_exit: hooks cannot be a check or measure"));
        let measure =
            "name: p\nhooks:\n  on_exit:\n    name: h\n    sql: a.sql\n    measure: m.sql\n";
        assert!(load_err(measure).contains("hooks cannot be a check or measure"));
        let risk = "name: p\nhooks:\n  on_exit:\n    name: h\n    sql: a.sql\n    risk: x\n";
        assert!(load_err(risk).contains("hooks.on_exit: hooks cannot have risk or severity"));
    }
}
