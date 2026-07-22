//! An external consumer of the `arc` library, exercised the way a sibling tool would
//! use it: link the crate, call `arc::spec`, never shell out to the binary.
//!
//! Everything here goes through the published surface only. If a future refactor
//! moves the loader behind a private door, this file stops compiling — which is the
//! point. It also loads the repo's real `examples/` specs rather than a fixture, so
//! the thing being validated is a spec a human actually wrote, comments and all.

use std::path::{Path, PathBuf};

use arc::spec::{Error, MANIFEST_FILENAME, Manifest, Precondition};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Every protocol directory under `examples/`.
fn example_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples"))
        .expect("examples/ must exist")
        .map(|e| e.expect("readable dir entry").path())
        .filter(|p| p.join(MANIFEST_FILENAME).is_file())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "expected at least one example protocol");
    dirs
}

/// Load and validate every shipped example through the library API.
#[test]
fn every_example_spec_loads_and_validates() {
    for dir in example_dirs() {
        let manifest = Manifest::load(&dir)
            .unwrap_or_else(|e| panic!("{} must load and validate: {e}", dir.display()));

        assert!(
            !manifest.name.trim().is_empty(),
            "{}: a validated manifest has a name",
            dir.display()
        );
        assert!(
            !manifest.steps.is_empty(),
            "{}: a validated manifest has steps",
            dir.display()
        );
    }
}

/// The exported schema is rich enough to *read* a spec, not merely to accept it —
/// a consumer can walk steps, params, defaults and preconditions without guessing.
#[test]
fn the_exported_schema_describes_the_spec_it_loaded() {
    let dir = repo_root().join("examples/brewtrend");
    let manifest = Manifest::load(&dir).expect("brewtrend loads");

    assert_eq!(manifest.name, "brewtrend");
    assert_eq!(manifest.engine, "duckdb");
    assert_eq!(manifest.engine_version.as_deref(), Some(">=1.2"));
    assert_eq!(manifest.db.as_deref(), Some("trends.db"));

    // Declared parameters, in declaration order.
    let params: Vec<&str> = manifest.params.keys().map(String::as_str).collect();
    assert_eq!(params, ["trend_threshold"]);
    assert_eq!(
        manifest.params["trend_threshold"].default.as_deref(),
        Some("10")
    );

    // Manifest-level defaults reach the caller intact.
    let retry = manifest
        .defaults
        .as_ref()
        .and_then(|d| d.retry.as_ref())
        .expect("brewtrend declares defaults.retry");
    assert_eq!(retry.max_attempts, 3);
    assert_eq!(retry.backoff_sec, 2.0);

    // Steps: names, ordering, and the sql/command/op arm each one took.
    let step_names: Vec<&str> = manifest.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(step_names.first(), Some(&"fetch_cask_catalogue"));
    assert!(step_names.contains(&"trending"));

    let trending = manifest
        .steps
        .iter()
        .find(|s| s.name == "trending")
        .expect("trending step");
    assert_eq!(trending.sql.as_deref(), Some("models/trending.sql"));
    assert!(trending.command.is_none());

    let fetch = &manifest.steps[0];
    assert!(fetch.command.is_some());
    assert_eq!(fetch.produces, ["cask_json"]);

    // Preconditions come back as the typed enum, not an opaque blob.
    match fetch
        .preconditions
        .first()
        .expect("fetch step declares a precondition")
    {
        Precondition::ModifiedAfter { modified_after } => {
            assert_eq!(modified_after.path, "data/cask/cask.json");
            assert_eq!(modified_after.period, "24h");
        }
        other => panic!("expected a modified_after precondition, got {other:?}"),
    }
}

/// Validation is a gate over bytes, not a transform: the caller's bytes go in, a
/// verdict comes out, and nothing is handed back to write. This is what lets a
/// hand-authored spec survive a machine edit.
#[test]
fn validating_bytes_leaves_the_bytes_alone() {
    let path = repo_root()
        .join("examples/brewtrend")
        .join(MANIFEST_FILENAME);
    let original = std::fs::read(&path).expect("brewtrend manifest is readable");
    let text = String::from_utf8(original.clone()).expect("manifest is utf-8");

    assert!(
        text.contains("# Runtime parameter"),
        "the fixture must carry comments for this test to mean anything"
    );

    let manifest = Manifest::from_yaml_str(&text).expect("the same bytes validate off disk");
    assert_eq!(manifest.name, "brewtrend");

    // The gate returned a verdict; the bytes a caller would write are still theirs.
    let after = std::fs::read(&path).expect("manifest still readable");
    assert_eq!(after, original, "validation must not rewrite the file");
}

/// A machine edit that breaks the spec is caught by the gate rather than written.
#[test]
fn an_invalid_edit_is_rejected_before_it_reaches_disk() {
    let path = repo_root()
        .join("examples/brewtrend")
        .join(MANIFEST_FILENAME);
    let text = std::fs::read_to_string(&path).expect("brewtrend manifest is readable");

    // Duplicate a step name — valid YAML, invalid spec.
    let edited = format!("{text}\n  - name: trending\n    command: \"true\"\n");

    match Manifest::from_yaml_str(&edited) {
        Err(Error::ManifestValidation(msg)) => {
            assert!(
                msg.contains("duplicate step name"),
                "the error must say what is wrong: {msg}"
            );
        }
        Err(other) => panic!("expected a validation error, got {other:?}"),
        Ok(_) => panic!("a duplicate step name must not validate"),
    }
}

/// YAML that is not a manifest fails at the parse stage, distinguishably.
#[test]
fn unparseable_yaml_is_a_parse_error_not_a_validation_error() {
    match Manifest::from_yaml_str("steps: [") {
        Err(Error::ManifestParse(_)) => {}
        other => panic!("expected a parse error, got {other:?}"),
    }
}

/// A directory with no manifest is reported as such, not as an io error.
#[test]
fn a_missing_manifest_is_reported_as_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    match Manifest::load(dir.path()) {
        Err(Error::ManifestNotFound) => {}
        other => panic!("expected ManifestNotFound, got {other:?}"),
    }
}
