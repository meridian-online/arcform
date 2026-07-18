//! Generate a runnable Protocol from a Frictionless Data Package descriptor.
//!
//! `arc init <name> --from-descriptor <datapackage.json>` turns a descriptor —
//! the kind a discovery tool emits, carrying semantic-typed fields and verified
//! `foreignKeys` — into a populated `arcform.yaml` plus `models/*.sql` that
//! `arc run` executes. Nothing is hand-authored: the described tables and the
//! accepted relationships between them become the steps.
//!
//! The split is deliberate. The descriptor DESCRIBES; the generated
//! `arcform.yaml` + SQL EXECUTES. This module is the bridge, and it owns the
//! translation — descriptor JSON in, arcform manifest + SQL out. `arc run`
//! always runs the generated manifest, never the descriptor.
//!
//! What each descriptor construct maps to:
//!   - a `duckdb`-format resource `path: <db>#<table>` → a `load_<table>` step
//!     whose SQL attaches the database read-only and copies the table into the
//!     project database. DuckDB is the only format handled here; other formats
//!     (csv/parquet/json) are a follow-up and are skipped with a warning.
//!   - an **accepted** foreign key (`x-dovetailStatus: "accepted"`) → an
//!     `fk_<child>_<parent>` step: a validating anti-join that counts orphan
//!     rows, with `depends_on` fanning in from both tables.
//!   - a non-accepted foreign key (suggested / review / anything else) → a
//!     comment in `arcform.yaml`, never an executed step.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::manifest::{Manifest, Step};

// ─────────────────────────────────────────────────────────────────────────────
// Descriptor shape (only the fields the bridge reads)
// ─────────────────────────────────────────────────────────────────────────────

/// A Frictionless Data Package descriptor — the discovery tool's output.
#[derive(Debug, Deserialize)]
struct Descriptor {
    #[serde(default)]
    resources: Vec<Resource>,
}

/// One resource: a described table backed by a data file.
#[derive(Debug, Deserialize)]
struct Resource {
    name: String,
    /// `<db>#<table>` for a duckdb resource; a plain path otherwise.
    path: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    schema: Schema,
}

/// A resource's Table Schema. Foreign keys ride inside it (Frictionless).
#[derive(Debug, Default, Deserialize)]
struct Schema {
    #[serde(default, rename = "foreignKeys")]
    foreign_keys: Vec<ForeignKey>,
}

/// A discovered foreign key. `x-dovetailStatus` gates whether it executes.
#[derive(Debug, Deserialize)]
struct ForeignKey {
    fields: Vec<String>,
    reference: Reference,
    #[serde(default, rename = "x-dovetailStatus")]
    status: String,
    #[serde(default, rename = "x-dovetailConfidence")]
    confidence: Option<f64>,
}

/// The parent side of a foreign key.
#[derive(Debug, Deserialize)]
struct Reference {
    resource: String,
    fields: Vec<String>,
}

/// The status string that marks a foreign key as verified and safe to execute.
const ACCEPTED: &str = "accepted";

// ─────────────────────────────────────────────────────────────────────────────
// The generated project (pure translation, no filesystem effects)
// ─────────────────────────────────────────────────────────────────────────────

/// Everything the bridge produces from a descriptor, before it touches disk.
#[derive(Debug)]
struct Generated {
    /// The populated manifest (`arcform.yaml`).
    manifest: Manifest,
    /// `(relative path under the project, SQL body)` for each model file.
    models: Vec<(String, String)>,
    /// Databases to copy into the project: `(source path, destination basename)`.
    db_sources: Vec<(PathBuf, String)>,
    /// Foreign keys surfaced for review — rendered as `arcform.yaml` comments.
    review_comments: Vec<String>,
}

/// Translate a parsed descriptor into a runnable project. Pure: no I/O beyond
/// resolving database source paths relative to `descriptor_dir`.
fn generate(name: &str, descriptor: &Descriptor, descriptor_dir: &Path) -> Result<Generated> {
    let mut manifest = Manifest::new_project(name);
    let mut load_steps: Vec<Step> = Vec::new();
    let mut fk_steps: Vec<Step> = Vec::new();
    let mut models: Vec<(String, String)> = Vec::new();
    let mut review_comments: Vec<String> = Vec::new();

    // Databases referenced by duckdb resources, de-duplicated by basename so a
    // multi-table database is copied once.
    let mut db_seen: BTreeSet<String> = BTreeSet::new();
    let mut db_sources: Vec<(PathBuf, String)> = Vec::new();

    // Which tables actually became load steps — a foreign key only executes if
    // both of its tables were loaded.
    let mut loaded_tables: BTreeSet<String> = BTreeSet::new();

    // --- Load steps: one per duckdb resource ---
    for res in &descriptor.resources {
        if res.format.as_deref() != Some("duckdb") {
            eprintln!(
                "arc init: skipping resource '{}' (format {:?}) — only duckdb resources are loaded for now",
                res.name, res.format
            );
            continue;
        }

        let (db_basename, table) = split_duckdb_path(&res.path).ok_or_else(|| {
            Error::ManifestValidation(format!(
                "resource '{}': duckdb path '{}' must be '<db>#<table>'",
                res.name, res.path
            ))
        })?;

        // Resolve and record the database to copy into the project.
        if db_seen.insert(db_basename.to_string()) {
            let src = descriptor_dir.join(db_basename);
            if !src.is_file() {
                return Err(Error::ManifestValidation(format!(
                    "resource '{}': database '{}' not found next to the descriptor ({})",
                    res.name,
                    db_basename,
                    src.display()
                )));
            }
            db_sources.push((src, db_basename.to_string()));
        }

        let step_name = format!("load_{}", res.name);
        let model_path = format!("models/{step_name}.sql");
        models.push((model_path.clone(), load_sql(&res.name, db_basename, table)));

        let mut step = Step::new(&step_name);
        step.sql = Some(model_path);
        load_steps.push(step);
        loaded_tables.insert(res.name.clone());
    }

    if load_steps.is_empty() {
        return Err(Error::ManifestValidation(
            "descriptor has no duckdb-format resources to load".to_string(),
        ));
    }

    // --- Foreign-key steps: one per ACCEPTED edge; others become comments ---
    for res in &descriptor.resources {
        for fk in &res.schema.foreign_keys {
            let child = res.name.as_str();
            let parent = fk.reference.resource.as_str();
            let child_field = fk.fields.first().map(String::as_str).unwrap_or("");
            let parent_field = fk.reference.fields.first().map(String::as_str).unwrap_or("");
            let edge = format!("{child}.{child_field} -> {parent}.{parent_field}");

            if !fk.status.eq_ignore_ascii_case(ACCEPTED) {
                review_comments.push(format!(
                    "# foreign key surfaced for review (x-dovetailStatus: {}{}): {} — not executed",
                    if fk.status.is_empty() { "unset" } else { &fk.status },
                    fk.confidence
                        .map(|c| format!(", confidence {c:.2}"))
                        .unwrap_or_default(),
                    edge
                ));
                continue;
            }

            // Guard: an accepted edge whose tables were not both loaded cannot
            // be validated — surface it rather than emit a broken step.
            if !loaded_tables.contains(child) || !loaded_tables.contains(parent) {
                review_comments.push(format!(
                    "# accepted foreign key {edge} skipped — a referenced table was not loaded"
                ));
                continue;
            }

            let step_name = format!("fk_{child}_{parent}");
            let model_path = format!("models/{step_name}.sql");
            models.push((
                model_path.clone(),
                fk_sql(&step_name, child, parent, child_field, parent_field),
            ));

            let mut step = Step::new(&step_name);
            step.sql = Some(model_path);
            // Fan-in edge: the validating join depends on both tables.
            step.depends_on = vec![child.to_string(), parent.to_string()];
            fk_steps.push(step);
        }
    }

    // Load steps first (in resource order), then the foreign-key checks.
    manifest.steps = load_steps;
    manifest.steps.extend(fk_steps);

    Ok(Generated {
        manifest,
        models,
        db_sources,
        review_comments,
    })
}

/// Split a duckdb resource path `"<db>#<table>"` into `(db_basename, table)`.
fn split_duckdb_path(path: &str) -> Option<(&str, &str)> {
    let (db, table) = path.split_once('#')?;
    if db.is_empty() || table.is_empty() {
        return None;
    }
    Some((db, table))
}

/// Load SQL for one duckdb table: attach the database read-only under a
/// resource-scoped alias (so multiple attachments of one file don't clash), then
/// copy the table into the project database. The created table auto-registers as
/// a produced asset via arcform's SQL introspector.
fn load_sql(res: &str, db_basename: &str, table: &str) -> String {
    format!(
        "-- Load '{table}' from the attached descriptor database (read-only source).\n\
         ATTACH '{db_basename}' AS src_{res} (READ_ONLY);\n\
         CREATE OR REPLACE TABLE {table} AS SELECT * FROM src_{res}.\"{table}\";\n"
    )
}

/// Validating anti-join for an accepted foreign key: count child rows whose key
/// has no match in the parent. Zero orphans confirms referential integrity holds.
fn fk_sql(step: &str, child: &str, parent: &str, child_field: &str, parent_field: &str) -> String {
    format!(
        "-- Validate the accepted foreign key {child}.{child_field} -> {parent}.{parent_field}.\n\
         -- Counts orphan rows; 0 means referential integrity holds.\n\
         CREATE OR REPLACE TABLE {step} AS\n\
         SELECT count(*) AS orphans\n\
         FROM {child} c LEFT JOIN {parent} p ON c.\"{child_field}\" = p.\"{parent_field}\"\n\
         WHERE p.\"{parent_field}\" IS NULL;\n"
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Entry points
// ─────────────────────────────────────────────────────────────────────────────

/// Execute `arc init <name> --from-descriptor <path>` in the current directory.
pub fn init_from_descriptor(name: &str, descriptor_path: &Path) -> Result<()> {
    init_from_descriptor_at(name, descriptor_path, &PathBuf::from("."))
}

/// Generate a project from a descriptor under `base`. Separated from
/// [`init_from_descriptor`] for testability — tests pass a tempdir as `base`.
pub fn init_from_descriptor_at(name: &str, descriptor_path: &Path, base: &Path) -> Result<()> {
    if name.trim().is_empty() {
        return Err(Error::ManifestValidation(
            "project name cannot be empty".to_string(),
        ));
    }

    let bytes = std::fs::read(descriptor_path).map_err(|source| Error::FileRead {
        path: descriptor_path.to_path_buf(),
        source,
    })?;
    let descriptor: Descriptor = serde_json::from_slice(&bytes).map_err(|e| {
        Error::ManifestValidation(format!(
            "descriptor {}: invalid JSON: {e}",
            descriptor_path.display()
        ))
    })?;
    let descriptor_dir = descriptor_path.parent().unwrap_or_else(|| Path::new("."));

    let project_dir = base.join(name);
    if project_dir.exists() {
        return Err(Error::ProjectExists(project_dir));
    }

    let generated = generate(name, &descriptor, descriptor_dir)?;

    // Create the project tree.
    std::fs::create_dir_all(project_dir.join("models"))?;

    // Copy each referenced database in — the project is self-contained.
    for (src, dest_basename) in &generated.db_sources {
        std::fs::copy(src, project_dir.join(dest_basename))?;
    }

    // Write model SQL.
    for (rel_path, sql) in &generated.models {
        std::fs::write(project_dir.join(rel_path), sql)?;
    }

    // Write arcform.yaml: a provenance header, the serialized manifest, then any
    // review comments for foreign keys that were not accepted.
    let mut yaml = String::new();
    yaml.push_str(&provenance_header(descriptor_path));
    yaml.push_str(
        &serde_yaml::to_string(&generated.manifest).expect("failed to serialize manifest"),
    );
    if !generated.review_comments.is_empty() {
        yaml.push_str(
            "\n# --- foreign keys surfaced for review (not executed) ---------------------\n",
        );
        for comment in &generated.review_comments {
            yaml.push_str(comment);
            yaml.push('\n');
        }
    }
    std::fs::write(project_dir.join("arcform.yaml"), yaml)?;

    // Keep a copy of the descriptor alongside the project as companion metadata —
    // it describes the inputs; it is never executed.
    std::fs::write(project_dir.join("datapackage.json"), &bytes)?;

    // Summary.
    let n_load = generated
        .manifest
        .steps
        .iter()
        .filter(|s| s.name.starts_with("load_"))
        .count();
    let n_fk = generated.manifest.steps.len() - n_load;
    println!("Generated Protocol '{name}' from {}:", descriptor_path.display());
    println!("  arcform.yaml   ({} steps: {n_load} load, {n_fk} foreign-key check(s))", generated.manifest.steps.len());
    println!("  models/        ({} SQL file(s))", generated.models.len());
    for (_, dest) in &generated.db_sources {
        println!("  {dest}   (copied in)");
    }
    println!("  datapackage.json   (companion descriptor, not executed)");
    if !generated.review_comments.is_empty() {
        println!(
            "  {} foreign key(s) surfaced for review (see arcform.yaml comments)",
            generated.review_comments.len()
        );
    }
    println!();
    println!("Run it with:  cd {name} && arc run");

    Ok(())
}

/// A short comment block recording where the Protocol came from.
fn provenance_header(descriptor_path: &Path) -> String {
    let basename = descriptor_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| descriptor_path.display().to_string());
    format!(
        "# Generated by `arc init --from-descriptor` from {basename}.\n\
         # The descriptor DESCRIBES the data (discovered tables, semantic types,\n\
         # verified foreign keys); this Protocol EXECUTES it. Each load step copies a\n\
         # described table out of the attached database; each accepted foreign key\n\
         # becomes a validating anti-join that counts orphan rows. Edit freely.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real descriptor `dovetail relate` produced for the committed fixture.
    const SIGNUPS_DESCRIPTOR: &str =
        include_str!("../tests/fixtures/signups.datapackage.json");

    fn parse(json: &str) -> Descriptor {
        serde_json::from_str(json).expect("descriptor parses")
    }

    // The authentic descriptor generates exactly the three expected steps, in
    // order: load_accounts, load_logins, fk_logins_accounts.
    #[test]
    fn signups_generates_three_steps_in_order() {
        let d = parse(SIGNUPS_DESCRIPTOR);
        let g = generate("assemble_demo", &d, Path::new("tests/fixtures")).unwrap();
        let names: Vec<&str> = g.manifest.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["load_accounts", "load_logins", "fk_logins_accounts"]);
        assert!(g.review_comments.is_empty(), "no non-accepted FKs in the fixture");
    }

    // The fk step fans in from both tables and joins account_id -> id.
    #[test]
    fn fk_step_depends_on_both_tables_and_joins_correctly() {
        let d = parse(SIGNUPS_DESCRIPTOR);
        let g = generate("assemble_demo", &d, Path::new("tests/fixtures")).unwrap();
        let fk = g
            .manifest
            .steps
            .iter()
            .find(|s| s.name == "fk_logins_accounts")
            .expect("fk step present");
        assert_eq!(fk.depends_on, vec!["logins", "accounts"]);

        let sql = &g
            .models
            .iter()
            .find(|(p, _)| p == "models/fk_logins_accounts.sql")
            .expect("fk model present")
            .1;
        assert!(sql.contains("LEFT JOIN accounts p"), "joins parent: {sql}");
        assert!(sql.contains(r#"c."account_id" = p."id""#), "joins account_id -> id: {sql}");
        assert!(sql.contains("orphans"), "counts orphans: {sql}");
    }

    // A generated load SQL round-trips through arcform's introspector: the table
    // it creates is discovered as a produced asset (so lineage + the contract see
    // it). This also proves the ATTACH statement parses.
    #[test]
    fn load_sql_registers_the_table_as_a_produced_asset() {
        let sql = load_sql("accounts", "signups.duckdb", "accounts");
        let assets = crate::introspect::extract_assets(&sql).expect("load SQL parses");
        assert!(assets.outputs.contains("accounts"), "produces accounts: {assets:?}");
    }

    // GATING: the fk step appears ONLY because the FK is accepted. Flip the same
    // edge to "suggested" and it becomes a review comment, not a step.
    #[test]
    fn suggested_fk_becomes_a_comment_not_a_step() {
        let json = r#"{
          "resources": [
            {"name": "accounts", "path": "signups.duckdb#accounts", "format": "duckdb",
             "schema": {"fields": [{"name": "id", "type": "integer"}]}},
            {"name": "logins", "path": "signups.duckdb#logins", "format": "duckdb",
             "schema": {"fields": [{"name": "account_id", "type": "integer"}],
               "foreignKeys": [{"fields": ["account_id"],
                 "reference": {"resource": "accounts", "fields": ["id"]},
                 "x-dovetailStatus": "suggested", "x-dovetailConfidence": 0.72}]}}
          ]
        }"#;
        let d = parse(json);
        let g = generate("demo", &d, Path::new("tests/fixtures")).unwrap();
        let names: Vec<&str> = g.manifest.steps.iter().map(|s| s.name.as_str()).collect();
        // Only the two load steps — no fk step.
        assert_eq!(names, ["load_accounts", "load_logins"]);
        assert!(!g.manifest.steps.iter().any(|s| s.name.starts_with("fk_")));
        assert_eq!(g.review_comments.len(), 1);
        assert!(g.review_comments[0].contains("suggested"));
        assert!(g.review_comments[0].contains("account_id -> accounts.id"));
    }

    // Accepted-vs-suggested is the ONLY difference that flips a comment into a
    // step: same edge, status "accepted" → a step exists.
    #[test]
    fn accepted_fk_becomes_a_step() {
        let json = r#"{
          "resources": [
            {"name": "accounts", "path": "signups.duckdb#accounts", "format": "duckdb",
             "schema": {"fields": [{"name": "id", "type": "integer"}]}},
            {"name": "logins", "path": "signups.duckdb#logins", "format": "duckdb",
             "schema": {"fields": [{"name": "account_id", "type": "integer"}],
               "foreignKeys": [{"fields": ["account_id"],
                 "reference": {"resource": "accounts", "fields": ["id"]},
                 "x-dovetailStatus": "accepted", "x-dovetailConfidence": 0.95}]}}
          ]
        }"#;
        let d = parse(json);
        let g = generate("demo", &d, Path::new("tests/fixtures")).unwrap();
        assert!(g.manifest.steps.iter().any(|s| s.name == "fk_logins_accounts"));
        assert!(g.review_comments.is_empty());
    }

    // Non-duckdb resources are skipped; a descriptor with none is an error.
    #[test]
    fn non_duckdb_only_descriptor_errors() {
        let json = r#"{"resources": [
            {"name": "t", "path": "t.csv", "format": "csv", "schema": {"fields": []}}
        ]}"#;
        let d = parse(json);
        let err = generate("demo", &d, Path::new("tests/fixtures")).unwrap_err();
        assert!(err.to_string().contains("no duckdb"), "got: {err}");
    }

    #[test]
    fn split_duckdb_path_parses_and_rejects() {
        assert_eq!(split_duckdb_path("signups.duckdb#accounts"), Some(("signups.duckdb", "accounts")));
        assert_eq!(split_duckdb_path("no-fragment.duckdb"), None);
        assert_eq!(split_duckdb_path("#accounts"), None);
        assert_eq!(split_duckdb_path("db#"), None);
    }
}
