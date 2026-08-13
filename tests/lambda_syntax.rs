//! End-to-end: a model written in DuckDB's `lambda x: expr` syntax keeps its
//! assets in the lineage graph, instead of degrading the whole step to an
//! opaque node.
//!
//! Before the parser fork learned this syntax, `arc run` on this exact manifest
//! printed `could not parse … — treating as opaque step` on stderr and the
//! transform step's output/input never appeared in the asset graph (see
//! `vendor/sqlparser-0.55.0/MERIDIAN_PATCH.md`, addition #3). This test pins
//! both the negative (no opaque warning) and the positive (both assets show up,
//! wired together) outcome against a real `arc run`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn write_manifest(project: &Path) {
    std::fs::create_dir_all(project.join("models")).unwrap();
    std::fs::write(
        project.join("arcform.yaml"),
        "name: lambda_demo\n\
         engine: duckdb\n\
         db: lambda_demo.duckdb\n\
         steps:\n\
         \x20\x20- name: load_source\n\
         \x20\x20\x20\x20sql: models/load_source.sql\n\
         \x20\x20- name: transform\n\
         \x20\x20\x20\x20sql: models/transform.sql\n",
    )
    .unwrap();
    std::fs::write(
        project.join("models/load_source.sql"),
        "CREATE TABLE source_table AS SELECT * FROM (VALUES (1), (2), (3)) AS t(x);\n",
    )
    .unwrap();
    std::fs::write(
        project.join("models/transform.sql"),
        "CREATE TABLE bumped AS \
         SELECT x, list_transform([x, x + 1], lambda c: c + 1) AS y \
         FROM source_table;\n",
    )
    .unwrap();
}

/// Read the single run contract under `<project>/build/.arcform/runs/*.json`.
fn read_contract(project: &Path) -> serde_json::Value {
    let runs = project.join("build/.arcform/runs");
    let mut json_files: Vec<PathBuf> = std::fs::read_dir(&runs)
        .unwrap_or_else(|e| panic!("runs dir {}: {e}", runs.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert_eq!(
        json_files.len(),
        1,
        "expected exactly one contract, got {json_files:?}"
    );
    let bytes = std::fs::read(json_files.remove(0)).unwrap();
    serde_json::from_slice(&bytes).expect("contract is valid JSON")
}

#[test]
fn a_model_using_lambda_colon_syntax_keeps_its_assets_in_the_graph() {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    let project = workspace.path().join("lambda_demo");
    std::fs::create_dir_all(&project).unwrap();
    write_manifest(&project);

    let run = Command::new(arc)
        .current_dir(&project)
        .arg("run")
        .output()
        .expect("spawn arc run");

    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    assert!(
        run.status.success(),
        "arc run failed (code {:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
        run.status.code(),
    );

    // The negative outcome measured against the unpatched parser.
    assert!(
        !stderr.contains("treating as opaque step"),
        "the lambda-colon step must not degrade to opaque:\nstderr:\n{stderr}"
    );

    // Both assets are in the rendered DAG, not just one.
    assert!(
        stdout.contains("Asset graph (2 node"),
        "both assets should appear in the rendered graph:\n{stdout}"
    );

    // And they are wired together correctly in the emitted contract.
    let contract = read_contract(&project);
    let assets = contract["assets"].as_array().expect("assets array");
    let find = |name: &str| assets.iter().find(|a| a["name"] == name);

    let source = find("source_table").expect("source_table asset discovered");
    let bumped = find("bumped").expect("bumped asset discovered");

    assert_eq!(source["produced_by"], "load_source");
    assert_eq!(bumped["produced_by"], "transform");

    let consumed_by: Vec<String> = source["consumed_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        consumed_by.contains(&"transform".to_string()),
        "the lambda step consumes source_table as an input, got {consumed_by:?}"
    );
}
