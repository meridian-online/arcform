use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The top-level ArcForm project manifest (arcform.yaml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Project name.
    pub name: String,

    /// Target SQL engine CLI identifier (default: "duckdb").
    #[serde(default = "default_engine")]
    pub engine: String,

    /// Path to the database file, relative to the manifest directory.
    /// Defaults to "<name>.duckdb" if not specified.
    #[serde(default)]
    pub db: Option<String>,

    /// Ordered list of transform steps.
    #[serde(default)]
    pub steps: Vec<Step>,
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

    /// Raw shell command string. Mutually exclusive with `sql`.
    #[serde(default)]
    pub command: Option<String>,
}

impl Step {
    /// Returns true if this step uses the engine (sql field).
    pub fn is_sql(&self) -> bool {
        self.sql.is_some()
    }
}

impl Manifest {
    /// Load and validate a manifest from the given directory.
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("arcform.yaml");
        if !path.exists() {
            return Err(Error::ManifestNotFound);
        }
        let contents = std::fs::read_to_string(&path).map_err(|e| Error::FileRead {
            path: path.clone(),
            source: e,
        })?;
        let manifest: Manifest = serde_yaml::from_str(&contents)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Resolve the database file path relative to the manifest directory.
    pub fn db_path(&self, manifest_dir: &Path) -> PathBuf {
        let db = self
            .db
            .clone()
            .unwrap_or_else(|| format!("{}.duckdb", self.name));
        manifest_dir.join(db)
    }

    /// Validate manifest constraints.
    fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::ManifestValidation(
                "project name cannot be empty".to_string(),
            ));
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

            // Check for duplicate step names.
            if !seen_names.insert(&step.name) {
                return Err(Error::ManifestValidation(format!(
                    "duplicate step name: '{}'",
                    step.name
                )));
            }

            // Each step must have exactly one of sql or command.
            match (&step.sql, &step.command) {
                (Some(_), Some(_)) => {
                    return Err(Error::ManifestValidation(format!(
                        "step '{}': must have either 'sql' or 'command', not both",
                        step.name
                    )));
                }
                (None, None) => {
                    return Err(Error::ManifestValidation(format!(
                        "step '{}': must have either 'sql' or 'command'",
                        step.name
                    )));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Generate a default manifest for a new project.
    pub fn new_project(name: &str) -> Self {
        Manifest {
            name: name.to_string(),
            engine: "duckdb".to_string(),
            db: Some(format!("{}.duckdb", name)),
            steps: Vec::new(),
        }
    }

    /// Check if any steps use the SQL engine.
    pub fn has_sql_steps(&self) -> bool {
        self.steps.iter().any(|s| s.is_sql())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // AC-2: Manifest::new_project generates correct defaults.
    #[test]
    fn test_new_project_defaults() {
        let m = Manifest::new_project("test-pipeline");
        assert_eq!(m.name, "test-pipeline");
        assert_eq!(m.engine, "duckdb");
        assert_eq!(m.db, Some("test-pipeline.duckdb".to_string()));
        assert!(m.steps.is_empty());
    }

    // AC-3: Database path defaults to <name>.duckdb.
    #[test]
    fn test_db_path_default() {
        let m = Manifest {
            name: "my-proj".to_string(),
            engine: "duckdb".to_string(),
            db: None,
            steps: vec![],
        };
        let path = m.db_path(Path::new("/tmp/project"));
        assert_eq!(path, PathBuf::from("/tmp/project/my-proj.duckdb"));
    }

    // AC-3: Explicit db field overrides the default path.
    #[test]
    fn test_db_path_explicit() {
        let m = Manifest {
            name: "my-proj".to_string(),
            engine: "duckdb".to_string(),
            db: Some("custom.duckdb".to_string()),
            steps: vec![],
        };
        let path = m.db_path(Path::new("/tmp/project"));
        assert_eq!(path, PathBuf::from("/tmp/project/custom.duckdb"));
    }

    // AC-10: Duplicate step names are rejected during validation.
    #[test]
    fn test_validate_duplicate_step_names() {
        let m = Manifest {
            name: "test".to_string(),
            engine: "duckdb".to_string(),
            db: None,
            steps: vec![
                Step {
                    name: "a".to_string(),
                    sql: Some("a.sql".to_string()),
                    command: None,
                },
                Step {
                    name: "a".to_string(),
                    sql: Some("b.sql".to_string()),
                    command: None,
                },
            ],
        };
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate step name"));
    }

    // AC-10: Step with both sql and command is rejected.
    #[test]
    fn test_validate_step_both_fields() {
        let m = Manifest {
            name: "test".to_string(),
            engine: "duckdb".to_string(),
            db: None,
            steps: vec![Step {
                name: "bad".to_string(),
                sql: Some("a.sql".to_string()),
                command: Some("echo hi".to_string()),
            }],
        };
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("not both"));
    }

    // AC-10: Step with neither sql nor command is rejected.
    #[test]
    fn test_validate_step_neither_field() {
        let m = Manifest {
            name: "test".to_string(),
            engine: "duckdb".to_string(),
            db: None,
            steps: vec![Step {
                name: "bad".to_string(),
                sql: None,
                command: None,
            }],
        };
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("must have either"));
    }

    // AC-5: Missing arcform.yaml produces a clear error.
    #[test]
    fn test_load_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let err = Manifest::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    // AC-3: Valid manifest loads and parses correctly.
    #[test]
    fn test_load_valid_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine: duckdb\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.name, "test");
        assert_eq!(m.steps.len(), 1);
    }

    // AC-5: Malformed YAML produces a parse error.
    #[test]
    fn test_load_malformed_yaml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("arcform.yaml"), "{{{{invalid").unwrap();
        let err = Manifest::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("parse"));
    }

    // AC-6: has_sql_steps gates whether preflight is called.
    #[test]
    fn test_has_sql_steps() {
        let m = Manifest {
            name: "test".to_string(),
            engine: "duckdb".to_string(),
            db: None,
            steps: vec![Step {
                name: "cmd".to_string(),
                sql: None,
                command: Some("echo hi".to_string()),
            }],
        };
        assert!(!m.has_sql_steps());
    }

    // AC-3: Steps maintain declaration order (Vec, not HashMap).
    #[test]
    fn test_sequential_order_preserved() {
        let m = Manifest {
            name: "test".to_string(),
            engine: "duckdb".to_string(),
            db: None,
            steps: vec![
                Step {
                    name: "c".to_string(),
                    sql: Some("c.sql".to_string()),
                    command: None,
                },
                Step {
                    name: "a".to_string(),
                    sql: Some("a.sql".to_string()),
                    command: None,
                },
                Step {
                    name: "b".to_string(),
                    sql: Some("b.sql".to_string()),
                    command: None,
                },
            ],
        };
        let names: Vec<&str> = m.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }
}
