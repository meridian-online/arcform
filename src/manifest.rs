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

    /// Semver version constraint for the engine CLI (e.g. ">=1.5").
    /// Optional — omitting skips version check. Uses Cargo-style syntax.
    #[serde(default)]
    pub engine_version: Option<String>,

    /// Path to the database file, relative to the manifest directory.
    /// Defaults to "<name>.duckdb" if not specified.
    #[serde(default)]
    pub db: Option<String>,

    /// Ordered list of transform steps.
    #[serde(default)]
    pub steps: Vec<Step>,

    /// Optional asset overrides. Keys are asset names; values specify
    /// which step produces the asset and its dependencies.
    #[serde(default)]
    pub assets: std::collections::HashMap<String, AssetOverride>,
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

    /// Assets this step produces (primarily for command steps).
    /// SQL steps auto-discover their outputs via sqlparser-rs.
    #[serde(default)]
    pub produces: Vec<String>,

    /// Assets this step depends on (primarily for command steps).
    /// SQL steps auto-discover their inputs via sqlparser-rs.
    #[serde(default)]
    pub depends_on: Vec<String>,
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

        // Validate engine_version is parseable semver constraint.
        if let Some(ref ev) = self.engine_version {
            if semver::VersionReq::parse(ev).is_err() {
                return Err(Error::ManifestValidation(format!(
                    "invalid engine_version '{}': must be a valid semver constraint (e.g. '>=1.5')",
                    ev
                )));
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
            engine_version: Some(">=1.0".to_string()),
            db: Some(format!("{}.duckdb", name)),
            steps: Vec::new(),
            assets: std::collections::HashMap::new(),
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

    /// Helper: create a SQL step with no asset declarations.
    fn sql_step(name: &str, sql: &str) -> Step {
        Step {
            name: name.to_string(),
            sql: Some(sql.to_string()),
            command: None,
            produces: vec![],
            depends_on: vec![],
        }
    }

    /// Helper: create a command step with no asset declarations.
    fn cmd_step(name: &str, command: &str) -> Step {
        Step {
            name: name.to_string(),
            sql: None,
            command: Some(command.to_string()),
            produces: vec![],
            depends_on: vec![],
        }
    }

    /// Helper: create a test manifest with given steps and no asset overrides.
    fn test_manifest(name: &str, steps: Vec<Step>) -> Manifest {
        Manifest {
            name: name.to_string(),
            engine: "duckdb".to_string(),
            engine_version: None,
            db: None,
            steps,
            assets: std::collections::HashMap::new(),
        }
    }

    // AC-2: Manifest::new_project generates correct defaults.
    #[test]
    fn test_new_project_defaults() {
        let m = Manifest::new_project("test-pipeline");
        assert_eq!(m.name, "test-pipeline");
        assert_eq!(m.engine, "duckdb");
        assert_eq!(m.db, Some("test-pipeline.duckdb".to_string()));
        assert!(m.steps.is_empty());
        assert!(m.assets.is_empty());
    }

    // AC-3: Database path defaults to <name>.duckdb.
    #[test]
    fn test_db_path_default() {
        let m = test_manifest("my-proj", vec![]);
        let path = m.db_path(Path::new("/tmp/project"));
        assert_eq!(path, PathBuf::from("/tmp/project/my-proj.duckdb"));
    }

    // AC-3: Explicit db field overrides the default path.
    #[test]
    fn test_db_path_explicit() {
        let mut m = test_manifest("my-proj", vec![]);
        m.db = Some("custom.duckdb".to_string());
        let path = m.db_path(Path::new("/tmp/project"));
        assert_eq!(path, PathBuf::from("/tmp/project/custom.duckdb"));
    }

    // AC-10: Duplicate step names are rejected during validation.
    #[test]
    fn test_validate_duplicate_step_names() {
        let m = test_manifest(
            "test",
            vec![sql_step("a", "a.sql"), sql_step("a", "b.sql")],
        );
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate step name"));
    }

    // AC-10: Step with both sql and command is rejected.
    #[test]
    fn test_validate_step_both_fields() {
        let m = test_manifest(
            "test",
            vec![Step {
                name: "bad".to_string(),
                sql: Some("a.sql".to_string()),
                command: Some("echo hi".to_string()),
                produces: vec![],
                depends_on: vec![],
            }],
        );
        let err = m.validate().unwrap_err();
        assert!(err.to_string().contains("not both"));
    }

    // AC-10: Step with neither sql nor command is rejected.
    #[test]
    fn test_validate_step_neither_field() {
        let m = test_manifest(
            "test",
            vec![Step {
                name: "bad".to_string(),
                sql: None,
                command: None,
                produces: vec![],
                depends_on: vec![],
            }],
        );
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
        let m = test_manifest("test", vec![cmd_step("cmd", "echo hi")]);
        assert!(!m.has_sql_steps());
    }

    // AC-3: Steps maintain declaration order (Vec, not HashMap).
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

    // v0.2 AC-04: Command steps parse produces and depends_on from YAML.
    #[test]
    fn test_ac04_command_step_with_asset_fields() {
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

    // v0.2 AC-05: Top-level assets section parses correctly.
    #[test]
    fn test_ac05_assets_override_section() {
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

    // v0.2 AC-08: Manifest without asset fields works identically to v0.1.
    #[test]
    fn test_ac08_v1_manifest_backwards_compatible() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert!(m.steps[0].produces.is_empty());
        assert!(m.steps[0].depends_on.is_empty());
        assert!(m.assets.is_empty());
    }

    // lrp ac-04: Manifest with engine_version parses correctly.
    #[test]
    fn test_lrp_ac04_manifest_with_engine_version() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine_version: '>=1.5'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert_eq!(m.engine_version, Some(">=1.5".to_string()));
    }

    // lrp ac-04: Manifest without engine_version is backwards compatible.
    #[test]
    fn test_lrp_ac04_manifest_without_engine_version() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let m = Manifest::load(dir.path()).unwrap();
        assert!(m.engine_version.is_none());
    }

    // lrp ac-08: Invalid engine_version syntax is rejected.
    #[test]
    fn test_lrp_ac08_invalid_engine_version_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "name: test\nengine_version: 'banana'\nsteps:\n  - name: s1\n    sql: models/s1.sql\n";
        fs::write(dir.path().join("arcform.yaml"), yaml).unwrap();
        let err = Manifest::load(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("engine_version"), "error should mention engine_version: {msg}");
        assert!(msg.contains("banana"), "error should show the invalid value: {msg}");
    }

    // lrp ac-09: arc init scaffolds engine_version.
    #[test]
    fn test_lrp_ac09_init_scaffolds_engine_version() {
        let m = Manifest::new_project("test-pipeline");
        assert!(m.engine_version.is_some(), "new_project should include engine_version");
        let ev = m.engine_version.unwrap();
        assert!(!ev.is_empty(), "engine_version should not be empty");
        // Should be a valid semver constraint.
        assert!(
            semver::VersionReq::parse(&ev).is_ok(),
            "engine_version '{}' should be valid semver",
            ev
        );
    }
}
