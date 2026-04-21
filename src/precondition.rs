use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

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

/// YAML format:
/// ```yaml
/// preconditions:
///   - modified_after:
///       path: data/cask.json
///       period: 24h
///   - command: "test $SKIP_FETCH"
/// ```
///
/// Uses `#[serde(untagged)]` — each variant is identified by its unique key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Precondition {
    /// Check that a file was modified after a given period ago.
    /// Path is relative to the manifest directory.
    /// If the file is missing or inaccessible, evaluates as stale (not an error).
    ModifiedAfter {
        modified_after: ModifiedAfterConfig,
    },

    /// Run a shell command. Exit 0 = fresh (skip), non-zero = stale (run).
    /// Execution errors (binary not found, crash) halt the pipeline.
    Command {
        command: String,
    },
}

impl Precondition {
    /// Evaluate this precondition. Returns Ok(true) if the step is fresh
    /// (should be skipped), Ok(false) if stale (should run).
    ///
    /// For `ModifiedWithin`: file issues return Ok(false) — stale, not error.
    /// For `Command`: execution errors return Err — pipeline halts.
    pub fn evaluate(&self, manifest_dir: &Path, step_name: &str, env: &HashMap<String, String>) -> Result<bool> {
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
            Precondition::Command { command: cmd } => {
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(manifest_dir)
                    .envs(env)
                    .output();

                match output {
                    Ok(o) => Ok(o.status.success()),
                    Err(e) => Err(Error::PreconditionError {
                        step: step_name.to_string(),
                        command: cmd.clone(),
                        detail: e.to_string(),
                    }),
                }
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
            Precondition::Command { command: cmd } => {
                if cmd.is_empty() {
                    return Err(Error::ManifestValidation(
                        "command precondition: command cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
        }
    }
}

/// Evaluate all preconditions for a step. Returns Ok(true) if ALL pass (step is fresh).
/// Returns Ok(false) if any precondition says stale.
/// Returns Err if a command precondition fails to execute.
pub fn evaluate_all(
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

    // pre ac-01: Precondition enum exists with both variants.
    #[test]
    fn test_pre_ac01_enum_variants() {
        let ma = Precondition::ModifiedAfter {
            modified_after: ModifiedAfterConfig {
                path: "data/file.json".to_string(),
                period: "24h".to_string(),
            },
        };
        let cmd = Precondition::Command { command: "true".to_string() };
        // Both variants compile and construct.
        assert!(matches!(ma, Precondition::ModifiedAfter { .. }));
        assert!(matches!(cmd, Precondition::Command { .. }));
    }

    // pre ac-01: Serde round-trip for both variants.
    #[test]
    fn test_pre_ac01_serde_roundtrip() {
        let preconditions = vec![
            Precondition::ModifiedAfter {
                modified_after: ModifiedAfterConfig {
                    path: "data/cask.json".to_string(),
                    period: "24h".to_string(),
                },
            },
            Precondition::Command { command: "test $SKIP".to_string() },
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
        Precondition::Command { command: command.to_string() }
    }

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    // pre ac-03: modified_after evaluates file mtime — fresh file.
    #[test]
    fn test_pre_ac03_modified_after_fresh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        assert!(ma("data.json", "1h").evaluate(dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-03: modified_after evaluates file mtime — stale file.
    #[test]
    fn test_pre_ac03_modified_after_stale() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("data.json");
        fs::write(&file_path, "{}").unwrap();

        // Set mtime to 48 hours ago.
        let old_time = SystemTime::now() - Duration::from_secs(48 * 3600);
        let ft = filetime::FileTime::from_system_time(old_time);
        filetime::set_file_mtime(&file_path, ft).unwrap();

        assert!(!ma("data.json", "24h").evaluate(dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-04: modified_after with missing file = stale (not error).
    #[test]
    fn test_pre_ac04_missing_file_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!ma("nonexistent.json", "24h").evaluate(dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-05: command precondition — exit 0 = fresh.
    #[test]
    fn test_pre_ac05_command_exit_zero_fresh() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cmd("true").evaluate(dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-05: command precondition — non-zero = stale.
    #[test]
    fn test_pre_ac05_command_nonzero_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!cmd("false").evaluate(dir.path(), "test-step", &empty_env()).unwrap());
    }

    // Command precondition receives resolved params as env vars.
    #[test]
    fn test_command_precondition_receives_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert("ARC_PARAM_SEARCH_TERM".to_string(), "hello".to_string());

        // Command checks if the env var is set — should return fresh (exit 0).
        assert!(cmd("test -n \"$ARC_PARAM_SEARCH_TERM\"").evaluate(dir.path(), "test-step", &env).unwrap());

        // Without the env var, the same command returns stale (exit non-zero).
        assert!(!cmd("test -n \"$ARC_PARAM_SEARCH_TERM\"").evaluate(dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-06: AND semantics — all must pass.
    #[test]
    fn test_pre_ac06_and_semantics_all_fresh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.json"), "{}").unwrap();
        fs::write(dir.path().join("b.json"), "{}").unwrap();

        let preconditions = vec![ma("a.json", "1h"), ma("b.json", "1h")];
        assert!(evaluate_all(&preconditions, dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-06: AND semantics — one stale makes all stale.
    #[test]
    fn test_pre_ac06_and_semantics_one_stale() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("fresh.json"), "{}").unwrap();

        let preconditions = vec![ma("fresh.json", "1h"), ma("stale.json", "1h")];
        assert!(!evaluate_all(&preconditions, dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-12: command non-zero exit = stale (not error).
    #[test]
    fn test_pre_ac12_command_nonzero_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!cmd("exit 1").evaluate(dir.path(), "test-step", &empty_env()).unwrap());
    }

    // pre ac-13: duration parsing supports various formats.
    #[test]
    fn test_pre_ac13_duration_parsing() {
        for duration_str in &["24h", "60m", "7d", "1h30m", "30s", "2d12h"] {
            assert!(humantime::parse_duration(duration_str).is_ok(), "Failed: {}", duration_str);
        }
    }

    // pre ac-15: validation rejects empty path.
    #[test]
    fn test_pre_ac15_validate_empty_path() {
        assert!(ma("", "24h").validate().is_err());
    }

    // pre ac-15: validation rejects empty period.
    #[test]
    fn test_pre_ac15_validate_empty_period() {
        assert!(ma("file.json", "").validate().is_err());
    }

    // pre ac-15: validation rejects invalid duration.
    #[test]
    fn test_pre_ac15_validate_invalid_duration() {
        assert!(ma("file.json", "banana").validate().is_err());
    }

    // pre ac-15: validation rejects empty command.
    #[test]
    fn test_pre_ac15_validate_empty_command() {
        assert!(cmd("").validate().is_err());
    }

    // pre ac-15: validation passes for valid preconditions.
    #[test]
    fn test_pre_ac15_validate_valid() {
        assert!(ma("data.json", "24h").validate().is_ok());
        assert!(cmd("test -f output.csv").validate().is_ok());
    }
}
