use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ingress_meta::FetchMeta;

/// A typed precondition that determines whether a step is fresh.
///
/// Preconditions are evaluated during staleness computation. If all
/// preconditions for a step pass (return "fresh"), the step may be skipped.
/// The `preconditions` field uses Dagu-compatible naming.
/// Configuration for the `modified_after` precondition type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifiedAfterConfig {
    /// Relative path to the file whose mtime is checked.
    pub path: String,
    /// Maximum period since last modification (e.g. "24h", "7d", "60m"). Parsed via humantime.
    pub period: String,
}

/// Configuration for the `fresh` precondition — content-aware ingress freshness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FreshConfig {
    /// Relative path to a fetched artifact whose `<path>.arcmeta` sidecar records
    /// its remote identity and content hash (written by `http_fetch`). The
    /// precondition checks the file on disk against the recorded `sha256`, then
    /// HEAD-probes the recorded remote — the content-addressed replacement for
    /// `modified_after`'s clock check.
    pub path: String,
}

/// Configuration for the `tool` precondition — the identity of an external binary or
/// artifact a step depends on and did not produce.
///
/// A step's staleness hash covers what the manifest says (a SQL file's bytes, an
/// operator ref and its `with:` block) and nothing about the environment it runs in, so
/// a step whose output is decided by a binary somewhere on the machine is clean for ever
/// while that binary moves underneath it. This kind is how the manifest says which thing
/// that is. The engine does not guess: it resolves exactly what it was told to resolve.
///
/// Two halves, each declared rather than inferred:
///
/// * **Where the tool is** — exactly one of `name` (an executable to find on `PATH`),
///   `path` (a filesystem path, absolute or relative to the manifest directory), or
///   `env` (the name of an environment variable holding that path). A tool found on
///   `PATH` and one a deployment points at through an env var are the same dependency
///   in different clothes, and both have to be expressible.
/// * **What identifies it** — exactly one of `version` (a shell command whose output is
///   the tool's reported identity) or `contents: true` (the SHA-256 of the resolved
///   file's bytes). A released binary announces a version; an artifact rebuilt in place
///   keeps its path *and* its version and changes only its bytes, so contents is the
///   only term that moves for it.
///
/// The `version` command runs under `sh -c` in the manifest directory with the step's
/// resolved parameters in its environment, plus `ARC_TOOL` bound to the path that was
/// resolved. Writing `version: "$ARC_TOOL --version"` is what ties the reported identity
/// to the file this precondition resolved; a command naming the binary again is free to
/// interrogate a different one, and the precondition cannot tell.
///
/// [`Default`] is every field unset, which `validate` refuses — it exists so a caller
/// building one names the two fields it means and no others.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolConfig {
    /// Name of an executable to find on `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Path to the tool — absolute, or relative to the manifest directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Name of an environment variable whose value is the path to the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Shell command that reports the tool's version. `$ARC_TOOL` is the resolved path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Identify the resolved file by the SHA-256 of its bytes instead of by a reported
    /// version.
    #[serde(default, skip_serializing_if = "is_false")]
    pub contents: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// YAML format:
/// ```yaml
/// preconditions:
///   - modified_after:
///       path: data/cask.json
///       period: 24h
///   - command: "test $SKIP_FETCH"
///   - tool:
///       name: finetype
///       version: "$ARC_TOOL --version"
///   - tool:
///       env: FINETYPE_DUCKDB_EXT
///       contents: true
/// ```
///
/// Uses `#[serde(untagged)]` — each variant is identified by its unique key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Precondition {
    /// Check that a file was modified after a given period ago.
    /// Path is relative to the manifest directory.
    /// If the file is missing or inaccessible, evaluates as stale (not an error).
    ModifiedAfter { modified_after: ModifiedAfterConfig },

    /// Content-aware ingress freshness, in two hops against a fetched artifact's
    /// `<path>.arcmeta` sidecar. First the bytes on disk are hashed and compared
    /// with the `sha256` the fetch recorded; a mismatch is stale, the remote is not
    /// contacted, and the sidecar is discarded so the re-fetch is unconditional.
    /// Then the recorded remote is HEAD-probed and its `ETag` / `Last-Modified`
    /// compared: unchanged remote = fresh (skip the fetch and everything
    /// downstream), changed or unprobeable = stale (re-fetch). Replaces the
    /// clock-based `modified_after` for `http_fetch` outputs.
    Fresh { fresh: FreshConfig },

    /// Run a shell command. Exit 0 = fresh (skip), non-zero = stale (run).
    /// Execution errors (binary not found, crash) halt the pipeline.
    Command { command: String },

    /// The identity of an external binary or artifact the step depends on and did not
    /// produce: same identity as the step's last successful run = fresh (skip), any
    /// difference = stale (run). No prior identity — a first run, or a build directory
    /// that has been cleaned — is stale, like every other kind's missing-state case.
    ///
    /// The identity is observed at plan time and recorded after the step succeeds, which
    /// is [`Precondition::Fresh`]'s shape rather than a hash term: the artifact records
    /// what it was, and the next run compares that against what it is. Evaluation writes
    /// nothing, so asking "is this stale?" cannot change the answer.
    ///
    /// **A tool that cannot be identified is an error, not "stale", and never "fresh".**
    /// The two existing kinds disagree about a missing thing —
    /// [`Precondition::ModifiedAfter`] calls it stale, [`Precondition::Command`] halts —
    /// and this kind follows `Command`, because the reason `ModifiedAfter` can afford to
    /// be lenient does not hold here. A missing file there is a file the step itself
    /// produces, so running the step is the remedy. Nothing a step does creates the tool
    /// it was told to depend on: "stale" would re-run it for ever against an environment
    /// that cannot satisfy it, and the failure would then surface from whichever
    /// operator tripped over the absence rather than from the declaration that named it.
    /// So the run stops, at plan time, with the step name and the path the lookup
    /// reached. The one verdict this must never reach is "fresh": a binary that has
    /// disappeared silently turning a step permanently clean is precisely the failure
    /// this kind exists to remove.
    Tool { tool: ToolConfig },
}

impl Precondition {
    /// Evaluate this precondition. Returns Ok(true) if the step is fresh
    /// (should be skipped), Ok(false) if stale (should run).
    ///
    /// For `ModifiedWithin`: file issues return Ok(false) — stale, not error.
    /// For `Command`: execution errors return Err — pipeline halts.
    pub(crate) fn evaluate(
        &self,
        manifest_dir: &Path,
        step_name: &str,
        env: &HashMap<String, String>,
    ) -> Result<bool> {
        match self {
            Precondition::ModifiedAfter { modified_after } => {
                let period_duration =
                    humantime::parse_duration(&modified_after.period).map_err(|e| {
                        Error::ManifestValidation(format!(
                            "invalid duration '{}': {}",
                            modified_after.period, e
                        ))
                    })?;

                let file_path = manifest_dir.join(&modified_after.path);

                // Any failure to stat or get mtime → stale (not an error).
                let fresh = (|| -> Option<bool> {
                    let metadata = std::fs::metadata(&file_path).ok()?;
                    let modified = metadata.modified().ok()?;
                    let elapsed = SystemTime::now().duration_since(modified).ok()?;
                    Some(elapsed < period_duration)
                })();

                Ok(fresh.unwrap_or(false))
            }
            Precondition::Fresh { fresh } => {
                let file_path = manifest_dir.join(&fresh.path);
                // No artifact or no sidecar → stale: the fetch must run. (A probe
                // failure below is likewise stale — safe, and cheap: the operator's
                // own conditional GET then 304s rather than re-downloading.)
                if !file_path.exists() {
                    return Ok(false);
                }
                let Some(meta) = crate::ingress_meta::read(&file_path) else {
                    return Ok(false);
                };

                // The local copy is checked against the hash the fetch recorded before
                // the remote is asked anything. A file that is not the bytes its sidecar
                // describes is stale whatever the remote answers, and refusing here costs
                // no round trip. The sidecar goes with it, so the re-run's fetch has no
                // validator to replay and downloads rather than 304s.
                if let Some(refusal) = local_copy_refusal(&file_path, &meta) {
                    let remedy = discard_sidecar(&file_path);
                    eprintln!("{} {}{}", "warning:".yellow(), refusal, remedy);
                    return Ok(false);
                }

                // The remote HEAD probe is the ureq path; it rides with the
                // `http_fetch` operator behind the `http-fetch` feature (see
                // src/operator.rs for the reachability note). Without that feature
                // there is no HTTP client to probe with, so fall back to "stale" —
                // identical to how an unreachable probe is treated below, and safe:
                // the fetch, if it then runs, 304s rather than re-downloading. This
                // fallback is dead in practice — preconditions are evaluated only by
                // the runner, which is CLI-only, and `cli` enables `http-fetch`.
                #[cfg(feature = "http-fetch")]
                {
                    let mut req =
                        ureq::head(&meta.url).set("User-Agent", crate::ingress_meta::DEFAULT_UA);
                    for (k, v) in &meta.request_headers {
                        req = req.set(k, v);
                    }
                    if let Some(ref etag) = meta.etag {
                        req = req.set("If-None-Match", etag);
                    }
                    if let Some(ref lm) = meta.last_modified {
                        req = req.set("If-Modified-Since", lm);
                    }

                    let fresh = match req.call() {
                        // Server confirms unchanged.
                        Ok(resp) if resp.status() == 304 => true,
                        Ok(resp) => {
                            // No 304 (e.g. server ignores conditionals) — compare identity
                            // ourselves: ETag first, then Last-Modified.
                            let etag_match = matches!(
                                (&meta.etag, resp.header("ETag")),
                                (Some(a), Some(b)) if a == b
                            );
                            let lm_match = matches!(
                                (&meta.last_modified, resp.header("Last-Modified")),
                                (Some(a), Some(b)) if a == b
                            );
                            etag_match || lm_match
                        }
                        Err(ureq::Error::Status(304, _)) => true,
                        // Unreachable/errored probe → stale.
                        Err(_) => false,
                    };
                    Ok(fresh)
                }
                #[cfg(not(feature = "http-fetch"))]
                {
                    Ok(false)
                }
            }
            Precondition::Command { command: cmd } => {
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(manifest_dir)
                    .envs(env)
                    .output();

                match output {
                    Ok(o) => Ok(o.status.success()),
                    Err(e) => Err(Error::Precondition {
                        step: step_name.to_string(),
                        command: cmd.clone(),
                        detail: e.to_string(),
                    }),
                }
            }
            Precondition::Tool { tool } => {
                // Observe first: an unidentifiable tool is an error whatever is on
                // record, so a stamped step cannot outlive the thing it was stamped
                // against.
                let observed = tool.observe(manifest_dir, step_name, env)?;
                let recorded = recorded_identity(manifest_dir, step_name, &tool.stamp_key());
                Ok(recorded.as_deref() == Some(observed.as_str()))
            }
        }
    }

    /// Validate this precondition at manifest load time (catch errors early).
    pub fn validate(&self) -> Result<()> {
        match self {
            Precondition::ModifiedAfter { modified_after } => {
                if modified_after.path.is_empty() {
                    return Err(Error::ManifestValidation(
                        "modified_after precondition: 'path' cannot be empty".to_string(),
                    ));
                }
                if modified_after.period.is_empty() {
                    return Err(Error::ManifestValidation(
                        "modified_after precondition: 'period' cannot be empty".to_string(),
                    ));
                }
                humantime::parse_duration(&modified_after.period).map_err(|e| {
                    Error::ManifestValidation(format!(
                        "modified_after precondition: invalid duration '{}': {}",
                        modified_after.period, e
                    ))
                })?;
                Ok(())
            }
            Precondition::Fresh { fresh } => {
                if fresh.path.is_empty() {
                    return Err(Error::ManifestValidation(
                        "fresh precondition: 'path' cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
            Precondition::Command { command: cmd } => {
                if cmd.is_empty() {
                    return Err(Error::ManifestValidation(
                        "command precondition: command cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
            Precondition::Tool { tool } => tool.validate(),
        }
    }
}

impl ToolConfig {
    /// Reject at manifest load anything that cannot be evaluated: a declaration with no
    /// tool to resolve, or one that never says what identifies it. Both halves are
    /// required, and each is exactly one choice — a tool named two ways, or identified
    /// two ways, is an ambiguity the engine would have to break silently.
    fn validate(&self) -> Result<()> {
        let mut resolvers: Vec<&str> = Vec::new();
        if non_empty(&self.name).is_some() {
            resolvers.push("name");
        }
        if non_empty(&self.path).is_some() {
            resolvers.push("path");
        }
        if non_empty(&self.env).is_some() {
            resolvers.push("env");
        }
        match resolvers.len() {
            0 => {
                return Err(Error::ManifestValidation(
                    "tool precondition: names no tool — give exactly one of `name:` (an \
                     executable on PATH), `path:` (a file) or `env:` (an environment \
                     variable holding a path)"
                        .to_string(),
                ));
            }
            1 => {}
            _ => {
                return Err(Error::ManifestValidation(format!(
                    "tool precondition: names the tool {} ways ({}) — give exactly one of \
                     `name:`, `path:` or `env:`",
                    resolvers.len(),
                    resolvers.join(", ")
                )));
            }
        }

        let has_version = non_empty(&self.version).is_some();
        match (has_version, self.contents) {
            (false, false) => Err(Error::ManifestValidation(
                "tool precondition: says nothing about what identifies the tool — give \
                 `version:` (a shell command that reports it, with `$ARC_TOOL` bound to the \
                 resolved path) or `contents: true` (the sha256 of the resolved file)"
                    .to_string(),
            )),
            (true, true) => Err(Error::ManifestValidation(
                "tool precondition: `version:` and `contents: true` are two identities for \
                 one tool — give one"
                    .to_string(),
            )),
            _ => Ok(()),
        }
    }

    /// The declaration as written, for a refusal message and for the stamp key.
    fn declared(&self) -> String {
        if let Some(name) = non_empty(&self.name) {
            format!("`{name}` on PATH")
        } else if let Some(path) = non_empty(&self.path) {
            format!("path `{path}`")
        } else if let Some(var) = non_empty(&self.env) {
            format!("${var}")
        } else {
            // Unreachable through a loaded manifest — `validate` refuses this shape.
            "(no tool named)".to_string()
        }
    }

    /// The key this declaration's identity is recorded under, within its step. It
    /// carries the identity mode as well as the tool, so editing the manifest from a
    /// reported version to a content hash — or repointing the tool — leaves no prior
    /// identity to match and the step runs.
    fn stamp_key(&self) -> String {
        let identity = if self.contents {
            "contents".to_string()
        } else {
            format!("version:{}", non_empty(&self.version).unwrap_or_default())
        };
        format!("{}|{}", self.declared(), identity)
    }

    /// Resolve the declaration to the file it names. `Err` carries the detail for the
    /// refusal: the resolved path when the lookup got that far, otherwise what was
    /// searched and came up empty.
    fn resolve(
        &self,
        manifest_dir: &Path,
        env: &HashMap<String, String>,
    ) -> std::result::Result<PathBuf, String> {
        if let Some(name) = non_empty(&self.name) {
            // A bare name means "the executable a shell would find", so the search is
            // PATH and the test is the executable bit — a readable file of that name in
            // a PATH directory is not the tool.
            let path_var = env
                .get("PATH")
                .cloned()
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            for entry in std::env::split_paths(&path_var) {
                let candidate = entry.join(name);
                if is_executable_file(&candidate) {
                    return Ok(candidate);
                }
            }
            return Err(format!(
                "no executable `{name}` on PATH ({})",
                if path_var.is_empty() {
                    "PATH is unset".to_string()
                } else {
                    path_var
                }
            ));
        }

        // `path:` and `env:` differ only in where the string comes from; an absolute one
        // stands, a relative one is read against the manifest directory like every other
        // path in a manifest.
        let (declared_path, provenance) = if let Some(path) = non_empty(&self.path) {
            (path.to_string(), String::new())
        } else if let Some(var) = non_empty(&self.env) {
            let raw = env
                .get(var)
                .cloned()
                .or_else(|| std::env::var(var).ok())
                .unwrap_or_default();
            if raw.trim().is_empty() {
                return Err(format!(
                    "environment variable {var} is unset or empty, so there is no path to \
                     identify"
                ));
            }
            (raw.trim().to_string(), format!("{var}="))
        } else {
            return Err("no tool named".to_string());
        };

        let resolved = manifest_dir.join(&declared_path);
        if !resolved.exists() {
            return Err(format!(
                "{provenance}{declared_path} resolves to {}, which does not exist",
                resolved.display()
            ));
        }
        Ok(resolved)
    }

    /// What this declaration identifies right now: `sha256:<hex>` of the resolved file's
    /// bytes, or `version:<what the command reported>`.
    ///
    /// Every failure is an [`Error::ToolPrecondition`] carrying the step and the
    /// declaration — see [`Precondition::Tool`] for why this halts rather than returning
    /// a verdict.
    fn observe(
        &self,
        manifest_dir: &Path,
        step_name: &str,
        env: &HashMap<String, String>,
    ) -> Result<String> {
        let resolved = self
            .resolve(manifest_dir, env)
            .map_err(|detail| self.refusal(step_name, detail))?;

        if self.contents {
            let hex = crate::fetch_cache::hash_file(&resolved).map_err(|e| {
                self.refusal(
                    step_name,
                    format!("{} could not be read to hash it: {e}", resolved.display()),
                )
            })?;
            return Ok(format!("sha256:{hex}"));
        }

        let command = non_empty(&self.version).unwrap_or_default();
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(manifest_dir)
            .envs(env)
            .env("ARC_TOOL", &resolved)
            .output()
            .map_err(|e| {
                self.refusal(
                    step_name,
                    format!(
                        "`{command}` (ARC_TOOL={}) failed to execute: {e}",
                        resolved.display()
                    ),
                )
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !output.status.success() {
            return Err(self.refusal(
                step_name,
                format!(
                    "`{command}` (ARC_TOOL={}) exited {}: {}",
                    resolved.display(),
                    output
                        .status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "on a signal".to_string()),
                    if stderr.is_empty() { &stdout } else { &stderr }
                ),
            ));
        }
        // Version banners land on stderr about as often as stdout, so both are read —
        // but silence is not an identity, and treating it as one would make every such
        // tool equal to every other.
        let reported = if stdout.is_empty() { stderr } else { stdout };
        if reported.is_empty() {
            return Err(self.refusal(
                step_name,
                format!(
                    "`{command}` (ARC_TOOL={}) reported no version on stdout or stderr",
                    resolved.display()
                ),
            ));
        }
        Ok(format!("version:{reported}"))
    }

    fn refusal(&self, step_name: &str, detail: String) -> Error {
        Error::ToolPrecondition {
            step: step_name.to_string(),
            tool: self.declared(),
            detail,
        }
    }
}

/// `Some(trimmed)` for a field that carries something, `None` for absent-or-blank — the
/// two are the same declaration as far as validation is concerned.
fn non_empty(field: &Option<String>) -> Option<&str> {
    field.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Where the identities observed at each step's last successful run are recorded.
///
/// Beside the run records — [`crate::contract::runs_dir`] is `build/.arcform/runs` —
/// because it is run state rather than something a human maintains, and because
/// deleting `build/` should forget it: a forgotten identity makes the step stale, which
/// is the safe direction.
fn stamp_store_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .join("build")
        .join(".arcform")
        .join("tools.json")
}

/// step name → stamp key → identity.
type ToolStamps = BTreeMap<String, BTreeMap<String, String>>;

/// The recorded identities, or an empty set if the file is absent or unreadable. An
/// unreadable store means nothing matches, which makes every tool-gated step stale.
fn read_stamps(manifest_dir: &Path) -> ToolStamps {
    std::fs::read_to_string(stamp_store_path(manifest_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn recorded_identity(manifest_dir: &Path, step_name: &str, key: &str) -> Option<String> {
    read_stamps(manifest_dir)
        .get(step_name)
        .and_then(|step| step.get(key))
        .cloned()
}

/// Record what each of a step's `tool` preconditions identifies, after that step has
/// executed successfully.
///
/// Called from the runner at the success point rather than from `evaluate`, so the
/// recorded identity means "what was in place when this step last ran" rather than "what
/// was in place when someone last asked". A step un-skipped by this gate and then never
/// reached — an earlier step failed, the run was interrupted — leaves the old identity
/// standing and runs again next time.
///
/// Nothing here can fail the run: the step has already succeeded. A tool that cannot be
/// identified now drops its entry instead, so the next run is stale and says why.
pub(crate) fn record_all(
    preconditions: &[Precondition],
    manifest_dir: &Path,
    step_name: &str,
    env: &HashMap<String, String>,
) {
    let tools: Vec<&ToolConfig> = preconditions
        .iter()
        .filter_map(|p| match p {
            Precondition::Tool { tool } => Some(tool),
            _ => None,
        })
        .collect();
    if tools.is_empty() {
        return;
    }

    let mut stamps = read_stamps(manifest_dir);
    {
        let step_stamps = stamps.entry(step_name.to_string()).or_default();
        for tool in tools {
            match tool.observe(manifest_dir, step_name, env) {
                Ok(identity) => {
                    step_stamps.insert(tool.stamp_key(), identity);
                }
                Err(e) => {
                    step_stamps.remove(&tool.stamp_key());
                    eprintln!(
                        "{} {e} — the step ran, but its tool identity was not recorded, so it \
                         will run again",
                        "warning:".yellow()
                    );
                }
            }
        }
    }

    let path = stamp_store_path(manifest_dir);
    let written = path
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|()| serde_json::to_string_pretty(&stamps).map_err(std::io::Error::other))
        .and_then(|json| std::fs::write(&path, json + "\n"));
    if let Err(e) = written {
        eprintln!(
            "{} could not record tool identities for step '{step_name}' at {} ({e}) — the \
             step will run again",
            "warning:".yellow(),
            path.display()
        );
    }
}

/// Check a fetched artifact's bytes against the `sha256` its `<path>.arcmeta` sidecar
/// recorded at fetch time.
///
/// `None` means the file on disk hashes to the value the fetch wrote. `Some` is the
/// refusal text, which names the file and attributes the failure to the local copy —
/// the remote has not been contacted when this runs.
///
/// A sidecar carrying no `sha256` is refused rather than waved through: the field is
/// `#[serde(default)]`, so an empty value is what a hand-edited or pre-field sidecar
/// deserialises to, and an unverifiable artifact is not a verified one.
///
/// This decides only the verdict. [`discard_sidecar`] is what makes the re-run replace
/// the bytes.
fn local_copy_refusal(file_path: &Path, meta: &FetchMeta) -> Option<String> {
    let recorded = meta.sha256.trim();
    if recorded.is_empty() {
        return Some(format!(
            "fresh: local copy {} cannot be verified: its .arcmeta records no sha256 \
             — the remote was not consulted",
            file_path.display()
        ));
    }
    let on_disk = match crate::fetch_cache::hash_file(file_path) {
        Ok(hex) => hex,
        Err(e) => {
            return Some(format!(
                "fresh: local copy {} could not be read to verify it ({e}) \
                 — the remote was not consulted",
                file_path.display()
            ));
        }
    };
    if on_disk.eq_ignore_ascii_case(recorded) {
        return None;
    }
    Some(format!(
        "fresh: local copy {} does not match the sha256 its fetch recorded \
         (recorded {recorded}, on disk {on_disk}) — the remote was not consulted",
        file_path.display()
    ))
}

/// Delete the sidecar of an artifact [`local_copy_refusal`] has just refused, and
/// return the clause describing what that leaves the re-run to do.
///
/// Returning stale only un-skips the step; it does not decide what the step's fetch
/// sends. With the sidecar still in place `http_fetch` replays its `ETag` as an
/// `If-None-Match`, a byte-identical remote answers `304`, and the operator keeps the
/// file it already has — the file this gate just refused. Removing the sidecar removes
/// the *local* validator, and the refused bytes are then replaced by whichever source
/// still has them: the shared fetch cache, whose copy is verified against its content
/// hash on the way out, or — where nothing is cached — an unconditional `GET`.
fn discard_sidecar(file_path: &Path) -> String {
    match crate::ingress_meta::remove(file_path) {
        Ok(()) => "; its .arcmeta is discarded, so the re-fetch replaces these bytes".to_string(),
        Err(e) => format!(
            "; its .arcmeta could not be discarded ({e}), \
             so the re-fetch may be answered with a 304 and leave these bytes in place"
        ),
    }
}

/// Evaluate all preconditions for a step. Returns Ok(true) if ALL pass (step is fresh).
/// Returns Ok(false) if any precondition says stale.
/// Returns Err if a command precondition fails to execute.
pub(crate) fn evaluate_all(
    preconditions: &[Precondition],
    manifest_dir: &Path,
    step_name: &str,
    env: &HashMap<String, String>,
) -> Result<bool> {
    for p in preconditions {
        if !p.evaluate(manifest_dir, step_name, env)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    // Precondition enum exists with both variants.
    #[test]
    fn test_pre_enum_variants() {
        let ma = Precondition::ModifiedAfter {
            modified_after: ModifiedAfterConfig {
                path: "data/file.json".to_string(),
                period: "24h".to_string(),
            },
        };
        let cmd = Precondition::Command {
            command: "true".to_string(),
        };
        // Both variants compile and construct.
        assert!(matches!(ma, Precondition::ModifiedAfter { .. }));
        assert!(matches!(cmd, Precondition::Command { .. }));
    }

    // Serde round-trip for both variants.
    #[test]
    fn test_pre_serde_roundtrip() {
        let preconditions = vec![
            Precondition::ModifiedAfter {
                modified_after: ModifiedAfterConfig {
                    path: "data/cask.json".to_string(),
                    period: "24h".to_string(),
                },
            },
            Precondition::Command {
                command: "test $SKIP".to_string(),
            },
        ];
        let yaml = serde_yaml::to_string(&preconditions).unwrap();
        let parsed: Vec<Precondition> = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed[0], Precondition::ModifiedAfter { .. }));
        assert!(matches!(parsed[1], Precondition::Command { .. }));
    }

    fn ma(path: &str, period: &str) -> Precondition {
        Precondition::ModifiedAfter {
            modified_after: ModifiedAfterConfig {
                path: path.to_string(),
                period: period.to_string(),
            },
        }
    }

    fn cmd(command: &str) -> Precondition {
        Precondition::Command {
            command: command.to_string(),
        }
    }

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    // modified_after evaluates file mtime — fresh file.
    #[test]
    fn test_pre_modified_after_fresh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        assert!(
            ma("data.json", "1h")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // modified_after evaluates file mtime — stale file.
    #[test]
    fn test_pre_modified_after_stale() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.json");
        fs::write(&file_path, "{}").unwrap();

        // Set mtime to 48 hours ago.
        let old_time = SystemTime::now() - Duration::from_secs(48 * 3600);
        let ft = filetime::FileTime::from_system_time(old_time);
        filetime::set_file_mtime(&file_path, ft).unwrap();

        assert!(
            !ma("data.json", "24h")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // modified_after with missing file = stale (not error).
    #[test]
    fn test_pre_missing_file_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !ma("nonexistent.json", "24h")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // command precondition — exit 0 = fresh.
    #[test]
    fn test_pre_command_exit_zero_fresh() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            cmd("true")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // command precondition — non-zero = stale.
    #[test]
    fn test_pre_command_nonzero_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !cmd("false")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // Command precondition receives resolved params as env vars.
    #[test]
    fn test_command_precondition_receives_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("ARC_PARAM_SEARCH_TERM".to_string(), "hello".to_string());

        // Command checks if the env var is set — should return fresh (exit 0).
        assert!(
            cmd("test -n \"$ARC_PARAM_SEARCH_TERM\"")
                .evaluate(dir.path(), "test-step", &env)
                .unwrap()
        );

        // Without the env var, the same command returns stale (exit non-zero).
        assert!(
            !cmd("test -n \"$ARC_PARAM_SEARCH_TERM\"")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // AND semantics — all must pass.
    #[test]
    fn test_pre_and_semantics_all_fresh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.json"), "{}").unwrap();
        fs::write(dir.path().join("b.json"), "{}").unwrap();

        let preconditions = vec![ma("a.json", "1h"), ma("b.json", "1h")];
        assert!(evaluate_all(&preconditions, dir.path(), "test-step", &empty_env()).unwrap());
    }

    // AND semantics — one stale makes all stale.
    #[test]
    fn test_pre_and_semantics_one_stale() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("fresh.json"), "{}").unwrap();

        let preconditions = vec![ma("fresh.json", "1h"), ma("stale.json", "1h")];
        assert!(!evaluate_all(&preconditions, dir.path(), "test-step", &empty_env()).unwrap());
    }

    // command non-zero exit = stale (not error).
    #[test]
    fn test_pre_command_nonzero_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !cmd("exit 1")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // duration parsing supports various formats.
    #[test]
    fn test_pre_duration_parsing() {
        for duration_str in &["24h", "60m", "7d", "1h30m", "30s", "2d12h"] {
            assert!(
                humantime::parse_duration(duration_str).is_ok(),
                "Failed: {}",
                duration_str
            );
        }
    }

    // validation rejects empty path.
    #[test]
    fn test_pre_validate_empty_path() {
        assert!(ma("", "24h").validate().is_err());
    }

    // validation rejects empty period.
    #[test]
    fn test_pre_validate_empty_period() {
        assert!(ma("file.json", "").validate().is_err());
    }

    // validation rejects invalid duration.
    #[test]
    fn test_pre_validate_invalid_duration() {
        assert!(ma("file.json", "banana").validate().is_err());
    }

    // validation rejects empty command.
    #[test]
    fn test_pre_validate_empty_command() {
        assert!(cmd("").validate().is_err());
    }

    // validation passes for valid preconditions.
    #[test]
    fn test_pre_validate_valid() {
        assert!(ma("data.json", "24h").validate().is_ok());
        assert!(cmd("test -f output.csv").validate().is_ok());
    }

    fn fresh(path: &str) -> Precondition {
        Precondition::Fresh {
            fresh: FreshConfig {
                path: path.to_string(),
            },
        }
    }

    // fresh: no artifact on disk → stale (must fetch), never a network probe.
    #[test]
    fn test_fresh_missing_artifact_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !fresh("build/edgar.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // fresh: artifact present but no `.arcmeta` sidecar → stale (nothing to probe).
    #[test]
    fn test_fresh_missing_sidecar_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("data.parquet"), b"bytes").unwrap();
        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
    }

    // A 127.0.0.1 listener that answers every request `304 Not Modified` and counts the
    // connections it accepted. The `fresh` probe reads a 304 as fresh, so a gate that
    // reaches this server declares the artifact fresh; the counter is how a test asserts
    // the gate did not reach it.
    #[cfg(feature = "http-fetch")]
    fn probe_server_304() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n");
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}/artifact"), hits)
    }

    #[cfg(feature = "http-fetch")]
    fn probe_hits(hits: &std::sync::atomic::AtomicUsize) -> usize {
        hits.load(std::sync::atomic::Ordering::SeqCst)
    }

    // Write `fetched` to `<dir>/data.parquet` and the sidecar `http_fetch` would have
    // written for it, then leave `on_disk` in the file's place.
    fn fetched_then_replaced(dir: &Path, url: &str, fetched: &[u8], on_disk: &[u8]) {
        let out = dir.join("data.parquet");
        fs::write(&out, fetched).unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url: url.to_string(),
                etag: Some("\"v1\"".to_string()),
                sha256: crate::state::content_hash(fetched),
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(&out, on_disk).unwrap();
    }

    // fresh: the artifact the fetch left in place is fresh, and the verdict comes from
    // the remote probe — the control for the two corruption tests below, which point at
    // the same 304 server and must reach a different verdict.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_intact_artifact_is_fresh_via_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        let bytes = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        fetched_then_replaced(dir.path(), &url, bytes, bytes);

        assert!(
            fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
        assert_eq!(probe_hits(&hits), 1, "the remote was probed");
    }

    // fresh: one byte flipped, length unchanged — stale, and the remote is never asked.
    // A size check would not see this mutation; the sidecar's sha256 does.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_corrupted_artifact_is_stale_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        fetched_then_replaced(
            dir.path(),
            &url,
            b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n",
            b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT395\n",
        );

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap(),
            "a corrupted artifact must not pass the gate"
        );
        assert_eq!(probe_hits(&hits), 0, "the remote must not be consulted");
    }

    // fresh: the artifact truncated mid-record — stale, and the remote is never asked.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_truncated_artifact_is_stale_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        fetched_then_replaced(
            dir.path(),
            &url,
            b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n",
            b"cik,lei\n0000320193,HWU",
        );

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap(),
            "a truncated artifact must not pass the gate"
        );
        assert_eq!(probe_hits(&hits), 0, "the remote must not be consulted");
    }

    // fresh: a sidecar with no recorded sha256 leaves the artifact unverifiable → stale.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_sidecar_without_sha256_is_stale_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        let (url, hits) = probe_server_304();
        let out = dir.path().join("data.parquet");
        fs::write(&out, b"bytes").unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url,
                etag: Some("\"v1\"".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
        assert_eq!(probe_hits(&hits), 0, "the remote must not be consulted");
    }

    // A 127.0.0.1 listener that serves `body` with `ETag: "v1"`, answering `304` to any
    // request that carries a validator. It records one bool per request — whether that
    // request was conditional — which is how the tests below read what the operator sent.
    #[cfg(feature = "http-fetch")]
    fn etag_server(body: &'static [u8]) -> (String, std::sync::Arc<std::sync::Mutex<Vec<bool>>>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let conditional =
                    head.contains("if-none-match") || head.contains("if-modified-since");
                log.lock().unwrap().push(conditional);
                if conditional {
                    let _ = stream.write_all(
                        b"HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
                    );
                } else {
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: {}\r\n\
                             Connection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    let _ = stream.write_all(body);
                }
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}/artifact"), requests)
    }

    // Run the catalog's real `http_fetch` against `url`, writing `data.parquet` into `dir`
    // — the same operator the runner invokes for the step the gate has just un-skipped.
    #[cfg(feature = "http-fetch")]
    fn http_fetch_into(dir: &Path, url: &str) {
        http_fetch_into_with_cache(dir, url, None);
    }

    // The same fetch, with a shared cache in the context — the runner's arrangement
    // when `$ARCFORM_FETCH_CACHE` is not `off`.
    #[cfg(feature = "http-fetch")]
    fn http_fetch_into_with_cache(
        dir: &Path,
        url: &str,
        cache: Option<&crate::fetch_cache::FetchCache>,
    ) {
        use crate::operator::OpContext;

        let with: serde_yaml::Value =
            serde_yaml::from_str(&format!("url: {url}\nout: data.parquet\n")).unwrap();
        let env = empty_env();
        let db = dir.join("pipeline.duckdb");
        crate::operator::resolve("http_fetch")
            .expect("http_fetch is in the catalog")
            .run(
                &with,
                &OpContext {
                    dir,
                    db_path: &db,
                    env: &env,
                    timeout: None,
                    cache,
                },
            )
            .expect("the fetch succeeds");
    }

    // The control: a sidecar the gate has NOT refused still makes the next fetch
    // conditional, so a byte-identical remote answers `304` and nothing is downloaded.
    // This is the behaviour the refusal path has to defeat.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_intact_sidecar_keeps_the_refetch_conditional() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let (url, requests) = etag_server(bytes);
        fetched_then_replaced(dir.path(), &url, bytes, bytes);

        http_fetch_into(dir.path(), &url);

        assert_eq!(
            *requests.lock().unwrap(),
            vec![true],
            "the stored ETag was replayed"
        );
    }

    // The refusal is a repair, not a warning: the gate deletes the sidecar it just found
    // lying, so the step's re-fetch has no validator to replay, the server sends bytes
    // instead of a `304`, and the corrupted artifact on disk is replaced. Then the
    // rewritten sidecar matches the new bytes, so the next run passes the gate — the cost
    // is one full download, not one refusal per run for ever. This run has no shared
    // fetch cache; where one holds the same URL the repair costs no download at all,
    // which is the test below.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_refusal_forces_an_unconditional_refetch_that_replaces_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let good = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let corrupt = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT395\n";
        let (url, requests) = etag_server(good);
        fetched_then_replaced(dir.path(), &url, good, corrupt);
        let out = dir.path().join("data.parquet");

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap(),
            "a corrupted artifact must not pass the gate"
        );
        assert!(
            !crate::ingress_meta::meta_path(&out).exists(),
            "the refusal discards the sidecar"
        );

        http_fetch_into(dir.path(), &url);

        assert_eq!(
            *requests.lock().unwrap(),
            vec![false],
            "the re-fetch carried no validator"
        );
        assert_eq!(
            fs::read(&out).unwrap(),
            good,
            "the corrupted bytes are replaced by the remote's"
        );

        let rewritten = crate::ingress_meta::read(&out).expect("the fetch rewrote the sidecar");
        assert_eq!(rewritten.sha256, crate::state::content_hash(good));
        assert!(
            local_copy_refusal(&out, &rewritten).is_none(),
            "the repaired artifact passes the gate"
        );
    }

    // The same repair with a shared fetch cache holding the URL: the sidecar still
    // goes, but the validator the re-fetch replays is the store's, so the origin
    // answers `304` and the refused bytes are replaced from a copy verified against
    // its content hash. The gate's message says "replaces these bytes" rather than
    // "downloads" because of this route.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_refusal_is_repaired_from_the_shared_cache_without_a_download() {
        let good = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let corrupt = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT395\n";
        let (url, requests) = etag_server(good);
        let store = tempfile::tempdir().unwrap();
        let cache = crate::fetch_cache::FetchCache::at(store.path().join("cache"));

        // Another Protocol fetched these bytes, so the store holds them.
        let seeded = tempfile::tempdir().unwrap();
        http_fetch_into_with_cache(seeded.path(), &url, Some(&cache));
        assert!(cache.holds(&url));

        // This Protocol's copy has rotted.
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("data.parquet");
        fetched_then_replaced(dir.path(), &url, good, corrupt);
        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap(),
            "a corrupted artifact must not pass the gate"
        );

        requests.lock().unwrap().clear();
        http_fetch_into_with_cache(dir.path(), &url, Some(&cache));

        assert_eq!(
            *requests.lock().unwrap(),
            vec![true],
            "the re-fetch was conditional, so the server answered 304 and sent no body"
        );
        assert_eq!(
            fs::read(&out).unwrap(),
            good,
            "and the rotted bytes are replaced from the store"
        );
        let rewritten = crate::ingress_meta::read(&out).expect("the sidecar is rewritten");
        assert_eq!(rewritten.sha256, crate::state::content_hash(good));
        assert!(local_copy_refusal(&out, &rewritten).is_none());
    }

    // A sidecar with no `sha256` is the pre-field case, and it is repaired the same way:
    // discarded, so one unconditional fetch writes a sidecar that does carry the hash.
    #[cfg(feature = "http-fetch")]
    #[test]
    fn test_fresh_sidecar_without_sha256_is_repaired_by_one_unconditional_refetch() {
        let dir = tempfile::tempdir().unwrap();
        let good = b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n";
        let (url, requests) = etag_server(good);
        let out = dir.path().join("data.parquet");
        fs::write(&out, good).unwrap();
        crate::ingress_meta::write(
            &out,
            &crate::ingress_meta::FetchMeta {
                url: url.clone(),
                etag: Some("\"v1\"".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            !fresh("data.parquet")
                .evaluate(dir.path(), "test-step", &empty_env())
                .unwrap()
        );
        http_fetch_into(dir.path(), &url);

        assert_eq!(*requests.lock().unwrap(), vec![false]);
        assert_eq!(
            crate::ingress_meta::read(&out)
                .expect("sidecar rewritten")
                .sha256,
            crate::state::content_hash(good),
            "the sidecar now carries the hash it lacked"
        );
    }

    // The refusal reaches stderr. `evaluate` prints it with `eprintln!`, which libtest
    // swallows in-process, so the test re-executes its own binary with `--nocapture` and
    // reads the child's stderr: the assertions below are on bytes the operator would see.
    // The child arm is this same test, told by the env var to build the fixture and
    // evaluate rather than to spawn.
    #[test]
    fn test_fresh_refusal_is_printed_to_stderr() {
        const CHILD: &str = "ARC_TEST_FRESH_REFUSAL_CHILD";

        if std::env::var_os(CHILD).is_some() {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("edgar.parquet");
            fs::write(&out, b"cik,lei\n0000320193,HWU").unwrap();
            crate::ingress_meta::write(
                &out,
                &crate::ingress_meta::FetchMeta {
                    // Port 1 on the loopback: were the probe ever reached, it would
                    // refuse the connection rather than answer.
                    url: "http://127.0.0.1:1/never-probed".to_string(),
                    sha256: crate::state::content_hash(
                        b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n",
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(
                !fresh("edgar.parquet")
                    .evaluate(dir.path(), "test-step", &empty_env())
                    .unwrap()
            );
            return;
        }

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "precondition::tests::test_fresh_refusal_is_printed_to_stderr",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("re-execute the test binary");
        assert!(output.status.success(), "child run: {:?}", output.status);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("warning:"), "warns: {stderr}");
        assert!(stderr.contains("edgar.parquet"), "names the file: {stderr}");
        assert!(
            stderr.contains("local copy"),
            "names the local copy: {stderr}"
        );
        assert!(
            stderr.contains("the remote was not consulted"),
            "clears the remote: {stderr}"
        );
        assert!(
            stderr.contains(".arcmeta is discarded, so the re-fetch replaces these bytes"),
            "states the repair it performed: {stderr}"
        );
    }

    // The refusal names the file that failed and attributes the failure to the local
    // copy. This string is what `evaluate` prints to stderr before returning stale.
    #[test]
    fn test_fresh_refusal_names_the_file_and_the_local_copy() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("edgar.parquet");
        fs::write(&out, b"cik,lei\n0000320193,HWU").unwrap();
        let meta = crate::ingress_meta::FetchMeta {
            sha256: crate::state::content_hash(b"cik,lei\n0000320193,HWUPKR0MPOU8FGXBT394\n"),
            ..Default::default()
        };

        let msg = local_copy_refusal(&out, &meta).expect("a mismatch is refused");
        assert!(msg.contains("edgar.parquet"), "names the file: {msg}");
        assert!(msg.contains("local copy"), "names the local copy: {msg}");
        assert!(
            msg.contains("the remote was not consulted"),
            "clears the remote: {msg}"
        );
        assert!(!msg.contains('\n'), "single line: {msg}");
    }

    // The refusal for a sidecar with no hash carries the same attribution.
    #[test]
    fn test_fresh_refusal_for_missing_hash_names_the_local_copy() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("edgar.parquet");
        fs::write(&out, b"bytes").unwrap();

        let msg = local_copy_refusal(&out, &crate::ingress_meta::FetchMeta::default())
            .expect("an unverifiable artifact is refused");
        assert!(msg.contains("edgar.parquet"), "names the file: {msg}");
        assert!(msg.contains("local copy"), "names the local copy: {msg}");
        assert!(
            msg.contains("the remote was not consulted"),
            "clears the remote: {msg}"
        );
    }

    // ─── tool: an external binary or artifact the step did not produce ───────────

    fn tool(cfg: ToolConfig) -> Precondition {
        Precondition::Tool { tool: cfg }
    }

    // A tool identified by what it reports, resolved by `path:` relative to the manifest.
    fn versioned(path: &str) -> Precondition {
        tool(ToolConfig {
            path: Some(path.to_string()),
            version: Some("$ARC_TOOL".to_string()),
            ..ToolConfig::default()
        })
    }

    // A tool identified by its bytes, resolved through an environment variable.
    fn by_contents_from_env(var: &str) -> Precondition {
        tool(ToolConfig {
            env: Some(var.to_string()),
            contents: true,
            ..ToolConfig::default()
        })
    }

    // The stand-in for a released binary — `body` is the whole script, so a test
    // controls exactly what the version command reports.
    //
    // Does NOT write an executable at `path` and exec it: that is the exact
    // write-then-exec race `fake_finetype` in src/mcp/finetype.rs used to have, and
    // this crate has twelve call sites of it (see
    // tests/fixtures/fake_tool_dispatcher.sh for the mechanism this repeats and why
    // it removes the race, not just narrows it). `path` becomes a symlink to that
    // checked-in dispatcher — `symlink()` is a directory-entry operation, never an
    // open-for-write of its target, so creating or replacing it can't race anyone's
    // fork — and `body` lands in a sidecar `<path>.script` that the dispatcher hands
    // to `sh` to interpret. `sh` reading that sidecar is a read(), never an
    // execve() of it, so a concurrent write to the sidecar (never marked
    // executable, unlike the file this replaced) can't trip ETXTBSY either.
    #[cfg(unix)]
    fn write_tool(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let dispatcher = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_tool_dispatcher.sh"
        ));
        let mut script_name = path.as_os_str().to_owned();
        script_name.push(".script");
        fs::write(PathBuf::from(script_name), body).unwrap();
        // A repeat call at the same path (several tests rewrite the same tool to
        // change its reported version) replaces a prior symlink; `symlink` itself
        // refuses to overwrite, so the stale entry is unlinked first. Removing a
        // directory entry is not a write to the dispatcher it may point at, so this
        // cannot reintroduce the race either.
        let _ = fs::remove_file(path);
        std::os::unix::fs::symlink(dispatcher, path).unwrap();
    }

    // No symlinks off unix, and every one of these tests already runs only where the
    // exec they drive (`sh -c "$ARC_TOOL"`) can. Kept compiling rather than exec-safe,
    // matching this crate's existing unix-only CI.
    #[cfg(not(unix))]
    fn write_tool(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn banner(version: &str) -> String {
        format!("#!/bin/sh\necho 'faketool {version}'\n")
    }

    // Stand in for the runner: the step ran and succeeded, so its tool identities are
    // recorded. Every fresh verdict in the tests below is reached through this.
    fn step_ran(p: &Precondition, dir: &Path, step: &str, env: &HashMap<String, String>) {
        record_all(std::slice::from_ref(p), dir, step, env);
    }

    // A manifest step accepts the kind, and both halves of the declaration are required
    // at load: a `tool:` naming no tool, and one naming no way to identify it, are each
    // refused by the same parse that accepts the well-formed pair.
    #[test]
    fn test_tool_manifest_load_requires_a_tool_and_an_identity() {
        let manifest = |precondition: &str| {
            format!(
                "name: test\nsteps:\n  - name: describe\n    command: \"echo hi\"\n    \
                 preconditions:\n      - tool:\n{precondition}"
            )
        };

        let accepted = crate::manifest::Manifest::from_yaml_str(&manifest(
            "          name: faketool\n          version: \"$ARC_TOOL --version\"\n",
        ))
        .expect("a well-formed tool precondition loads");
        assert!(matches!(
            accepted.steps[0].preconditions[0],
            Precondition::Tool { .. }
        ));

        let no_tool = crate::manifest::Manifest::from_yaml_str(&manifest(
            "          version: \"faketool --version\"\n",
        ))
        .expect_err("a tool precondition naming no tool is refused at load");
        assert!(
            no_tool.to_string().contains("names no tool"),
            "says which half is missing: {no_tool}"
        );
        assert!(
            no_tool.to_string().contains("describe"),
            "names the step: {no_tool}"
        );

        let no_identity =
            crate::manifest::Manifest::from_yaml_str(&manifest("          name: faketool\n"))
                .expect_err("a tool precondition with no identity is refused at load");
        assert!(
            no_identity
                .to_string()
                .contains("says nothing about what identifies the tool"),
            "says which half is missing: {no_identity}"
        );
    }

    // Ambiguity is refused too: a tool named two ways, or identified two ways, would
    // have to be broken silently by whichever branch the engine happened to test first.
    #[test]
    fn test_tool_validate_refuses_ambiguous_declarations() {
        let two_tools = tool(ToolConfig {
            name: Some("faketool".to_string()),
            path: Some("bin/faketool".to_string()),
            version: Some("$ARC_TOOL".to_string()),
            ..ToolConfig::default()
        });
        let err = two_tools.validate().unwrap_err().to_string();
        assert!(err.contains("names the tool 2 ways"), "{err}");
        assert!(err.contains("name, path"), "names which two: {err}");

        let two_identities = tool(ToolConfig {
            name: Some("faketool".to_string()),
            version: Some("$ARC_TOOL".to_string()),
            contents: true,
            ..ToolConfig::default()
        });
        let err = two_identities.validate().unwrap_err().to_string();
        assert!(err.contains("two identities for one tool"), "{err}");

        // A blank string is the same declaration as an absent one.
        let blank = tool(ToolConfig {
            name: Some("   ".to_string()),
            version: Some("$ARC_TOOL".to_string()),
            ..ToolConfig::default()
        });
        assert!(blank.validate().is_err(), "a blank name names no tool");
    }

    // The three resolutions reach the same file: a name on PATH, an absolute path, and
    // an environment variable holding one. All three are fresh against the identity the
    // step last ran with, which is what makes the env-var case usable at all —
    // `finetype_validate` takes its DuckDB extension from a per-step `extension:` or
    // else from `FINETYPE_DUCKDB_EXT`, and no manifest in this repo sets the former.
    #[test]
    fn test_tool_resolves_from_path_from_a_file_path_and_from_an_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        write_tool(&bin.join("faketool"), &banner("1.0.0"));

        let mut env = empty_env();
        // The fake comes first, and the system directories stay on the end: a step's
        // PATH is what the version command is run with, so a PATH holding nothing but
        // the tool has no shell to run it.
        env.insert(
            "PATH".to_string(),
            format!("{}:/usr/bin:/bin", bin.display()),
        );
        env.insert(
            "FAKETOOL_BIN".to_string(),
            bin.join("faketool").display().to_string(),
        );

        let on_path = tool(ToolConfig {
            name: Some("faketool".to_string()),
            version: Some("$ARC_TOOL".to_string()),
            ..ToolConfig::default()
        });
        let at_path = tool(ToolConfig {
            path: Some(bin.join("faketool").display().to_string()),
            version: Some("$ARC_TOOL".to_string()),
            ..ToolConfig::default()
        });
        let from_env = tool(ToolConfig {
            env: Some("FAKETOOL_BIN".to_string()),
            version: Some("$ARC_TOOL".to_string()),
            ..ToolConfig::default()
        });

        for (label, p) in [
            ("PATH", &on_path),
            ("absolute path", &at_path),
            ("env var", &from_env),
        ] {
            let step = format!("step-via-{label}");
            assert!(
                !p.evaluate(dir.path(), &step, &env).unwrap(),
                "{label}: with nothing recorded the step is stale"
            );
            step_ran(p, dir.path(), &step, &env);
            assert!(
                p.evaluate(dir.path(), &step, &env).unwrap(),
                "{label}: the same tool as the last run is fresh"
            );
        }
    }

    // The behaviour the kind exists for: same reported version = skip, different
    // reported version = run, with nothing else about the step touched. The tool here
    // is one file whose bytes change; only what it reports decides the verdict.
    #[test]
    fn test_tool_version_decides_the_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin/faketool");
        write_tool(&bin, &banner("1.0.0"));
        let p = versioned("bin/faketool");

        assert!(
            !p.evaluate(dir.path(), "describe", &empty_env()).unwrap(),
            "a step that has never run against this tool is stale"
        );
        step_ran(&p, dir.path(), "describe", &empty_env());
        assert!(
            p.evaluate(dir.path(), "describe", &empty_env()).unwrap(),
            "an unchanged tool is fresh"
        );

        write_tool(&bin, &banner("1.0.1"));
        assert!(
            !p.evaluate(dir.path(), "describe", &empty_env()).unwrap(),
            "a tool reporting a different version is stale"
        );

        // And the new identity is what the next run is measured against.
        step_ran(&p, dir.path(), "describe", &empty_env());
        assert!(p.evaluate(dir.path(), "describe", &empty_env()).unwrap());
    }

    // A tool rebuilt in place: same path, same length, different bytes. This is the shape
    // of `finetype_validate`'s dependency — a DuckDB extension resolved from an env var,
    // identified by `contents:` because it answers no version command — and nothing but a
    // content hash moves for it. The length is held equal across the rebuild on purpose:
    // identifying the artifact by path or by size would call this fresh.
    #[test]
    fn test_tool_contents_redden_when_the_artifact_is_rebuilt_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let ext = dir.path().join("build/finetype.duckdb_extension");
        fs::create_dir_all(ext.parent().unwrap()).unwrap();
        fs::write(&ext, b"EXTENSION-BUILD-AAAA").unwrap();

        let mut env = empty_env();
        env.insert("FINETYPE_EXT".to_string(), ext.display().to_string());
        let p = by_contents_from_env("FINETYPE_EXT");

        step_ran(&p, dir.path(), "gate", &env);
        assert!(
            p.evaluate(dir.path(), "gate", &env).unwrap(),
            "the artifact the step last ran against is fresh"
        );

        // Rebuilt in place — the path in the env var has not moved and the file is the
        // same size.
        fs::write(&ext, b"EXTENSION-BUILD-BBBB").unwrap();
        assert_eq!(
            fs::metadata(&ext).unwrap().len(),
            "EXTENSION-BUILD-AAAA".len() as u64,
            "the rebuild changed no size, so only the bytes can carry the difference"
        );
        assert!(
            !p.evaluate(dir.path(), "gate", &env).unwrap(),
            "a rebuilt artifact at an unchanged path is stale"
        );
    }

    // A missing tool is an error naming the step and where the lookup got to — never a
    // fresh verdict, which would leave the step permanently clean against a tool that is
    // not there. One case per way of naming a tool, plus a version command that fails.
    #[test]
    fn test_tool_that_cannot_be_identified_halts_and_names_the_step() {
        let dir = tempfile::tempdir().unwrap();
        let empty_path_dir = dir.path().join("empty");
        fs::create_dir_all(&empty_path_dir).unwrap();
        let mut path_env = empty_env();
        path_env.insert("PATH".to_string(), empty_path_dir.display().to_string());

        // A readable file that is not executable is not a tool a shell would find.
        let unreadable_dir = dir.path().join("notexec");
        fs::create_dir_all(&unreadable_dir).unwrap();
        fs::write(unreadable_dir.join("faketool"), "not executable").unwrap();
        let mut notexec_env = empty_env();
        notexec_env.insert("PATH".to_string(), unreadable_dir.display().to_string());

        let mut pointing_nowhere = empty_env();
        pointing_nowhere.insert(
            "FAKETOOL_BIN".to_string(),
            dir.path().join("gone/faketool").display().to_string(),
        );

        write_tool(&dir.path().join("bin/broken"), "#!/bin/sh\nexit 3\n");

        let named = |name: &str| {
            tool(ToolConfig {
                name: Some(name.to_string()),
                version: Some("$ARC_TOOL".to_string()),
                ..ToolConfig::default()
            })
        };
        let from_env = tool(ToolConfig {
            env: Some("FAKETOOL_BIN".to_string()),
            version: Some("$ARC_TOOL".to_string()),
            ..ToolConfig::default()
        });
        let unset_var = tool(ToolConfig {
            env: Some("ARC_TEST_NEVER_SET_VAR".to_string()),
            contents: true,
            ..ToolConfig::default()
        });

        let cases: [(&str, &Precondition, &HashMap<String, String>, &str); 6] = [
            ("not on PATH", &named("faketool"), &path_env, "on PATH"),
            (
                "on PATH but not executable",
                &named("faketool"),
                &notexec_env,
                "on PATH",
            ),
            (
                "env var points at nothing",
                &from_env,
                &pointing_nowhere,
                "does not exist",
            ),
            (
                "env var unset",
                &unset_var,
                &empty_env(),
                "ARC_TEST_NEVER_SET_VAR",
            ),
            (
                "the file is missing",
                &versioned("bin/absent"),
                &empty_env(),
                "does not exist",
            ),
            (
                "the version command fails",
                &versioned("bin/broken"),
                &empty_env(),
                "exited 3",
            ),
        ];

        for (label, p, env, expected) in cases {
            let result = p.evaluate(dir.path(), "describe", env);
            let err = match result {
                Err(e) => e,
                Ok(verdict) => panic!("{label}: evaluated as {verdict} instead of halting"),
            };
            let msg = err.to_string();
            assert!(msg.contains("describe"), "{label}: names the step: {msg}");
            assert!(
                msg.contains(expected),
                "{label}: says where it got to: {msg}"
            );
        }
    }

    // The identity of a tool that has vanished is not recorded, and any identity already
    // on record for it is dropped — so a tool removed after a successful run leaves a
    // stale step and a refusal, not a clean one.
    #[test]
    fn test_tool_identity_is_dropped_when_the_tool_goes_away() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin/faketool");
        write_tool(&bin, &banner("1.0.0"));
        let p = versioned("bin/faketool");

        step_ran(&p, dir.path(), "describe", &empty_env());
        assert!(p.evaluate(dir.path(), "describe", &empty_env()).unwrap());

        fs::remove_file(&bin).unwrap();
        step_ran(&p, dir.path(), "describe", &empty_env());
        write_tool(&bin, &banner("1.0.0"));
        assert!(
            !p.evaluate(dir.path(), "describe", &empty_env()).unwrap(),
            "the identity dropped when the tool vanished is not silently restored"
        );
    }

    // Evaluation is a question, not a write: asking whether a step is stale records
    // nothing, so `arc` cannot make a step fresh by looking at it twice.
    #[test]
    fn test_tool_evaluation_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write_tool(&dir.path().join("bin/faketool"), &banner("1.0.0"));
        let p = versioned("bin/faketool");

        assert!(!p.evaluate(dir.path(), "describe", &empty_env()).unwrap());
        assert!(
            !stamp_store_path(dir.path()).exists(),
            "evaluate wrote a stamp store"
        );
        assert!(
            !p.evaluate(dir.path(), "describe", &empty_env()).unwrap(),
            "a second look reaches the same verdict"
        );
    }

    // Two tools on one step are two independent identities: whichever moves makes the
    // step stale, and the one that did not move is not what decided it.
    #[test]
    fn test_tool_two_declarations_on_one_step_are_tracked_separately() {
        let dir = tempfile::tempdir().unwrap();
        write_tool(&dir.path().join("bin/alpha"), &banner("1.0.0"));
        write_tool(&dir.path().join("bin/beta"), &banner("2.0.0"));
        let alpha = versioned("bin/alpha");
        let beta = versioned("bin/beta");
        let both = vec![alpha.clone(), beta.clone()];

        record_all(&both, dir.path(), "describe", &empty_env());
        assert!(evaluate_all(&both, dir.path(), "describe", &empty_env()).unwrap());

        write_tool(&dir.path().join("bin/beta"), &banner("2.0.1"));
        assert!(
            !evaluate_all(&both, dir.path(), "describe", &empty_env()).unwrap(),
            "one moved tool is enough to make the step stale"
        );
        assert!(
            alpha
                .evaluate(dir.path(), "describe", &empty_env())
                .unwrap(),
            "and the tool that did not move is still fresh"
        );
    }

    // An identity recorded for one step is not an identity for another: two steps
    // declaring the same tool each answer for their own last run.
    #[test]
    fn test_tool_identities_are_per_step() {
        let dir = tempfile::tempdir().unwrap();
        write_tool(&dir.path().join("bin/faketool"), &banner("1.0.0"));
        let p = versioned("bin/faketool");

        step_ran(&p, dir.path(), "describe", &empty_env());
        assert!(p.evaluate(dir.path(), "describe", &empty_env()).unwrap());
        assert!(
            !p.evaluate(dir.path(), "validate", &empty_env()).unwrap(),
            "a step that has not run is stale whatever its sibling recorded"
        );
    }

    // Serde round-trip: the untagged enum reads a `tool:` map back as this variant and
    // writes only the fields the declaration set.
    #[test]
    fn test_tool_serde_roundtrip() {
        let yaml = serde_yaml::to_string(&vec![
            versioned("bin/faketool"),
            by_contents_from_env("FINETYPE_EXT"),
        ])
        .unwrap();
        assert!(
            !yaml.contains("null"),
            "an unset resolver is omitted rather than serialised as null: {yaml}"
        );
        let parsed: Vec<Precondition> = serde_yaml::from_str(&yaml).unwrap();
        assert!(matches!(parsed[0], Precondition::Tool { .. }));
        assert!(matches!(parsed[1], Precondition::Tool { .. }));
        match &parsed[1] {
            Precondition::Tool { tool } => {
                assert_eq!(tool.env.as_deref(), Some("FINETYPE_EXT"));
                assert!(tool.contents);
                assert!(tool.version.is_none());
            }
            other => panic!("expected a tool precondition: {other:?}"),
        }
    }

    // fresh: validation + serde round-trip.
    #[test]
    fn test_fresh_validate_and_roundtrip() {
        assert!(fresh("build/x.parquet").validate().is_ok());
        assert!(fresh("").validate().is_err());
        let yaml = serde_yaml::to_string(&vec![fresh("build/x.parquet")]).unwrap();
        let parsed: Vec<Precondition> = serde_yaml::from_str(&yaml).unwrap();
        assert!(matches!(parsed[0], Precondition::Fresh { .. }));
    }
}
