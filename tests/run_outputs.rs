//! End-to-end: a Protocol's declared `outputs:` are checked after the last step and
//! recorded, with each file's size and SHA-256, in the run record, through the real `arc`
//! binary. Needs a DuckDB 2 `duckdb` CLI on PATH, as any `arc run` with a SQL step does.

#[allow(dead_code)]
mod common;

use std::path::Path;

use sha2::{Digest, Sha256};

/// A Protocol in a fresh folder with `build/` made, since DuckDB's `COPY` creates no parent.
fn project(manifest: &str, model: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("arcform.yaml"), manifest).unwrap();
    if let Some(sql) = model {
        std::fs::create_dir_all(dir.path().join("models")).unwrap();
        std::fs::write(dir.path().join("models/out.sql"), sql).unwrap();
    }
    std::fs::create_dir_all(dir.path().join("build")).unwrap();
    dir
}

/// The one run record the run left.
fn record(dir: &Path) -> serde_json::Value {
    let runs = dir.join("build/.arcform/runs");
    let mut records: Vec<_> = std::fs::read_dir(&runs)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    serde_json::from_str(&std::fs::read_to_string(records.remove(0)).unwrap()).unwrap()
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

#[test]
fn records_file_outputs() {
    let dir = project(
        "name: outs\nsteps:\n  - name: out\n    sql: models/out.sql\noutputs:\n  - build/out.parquet\n",
        Some("COPY (SELECT 1 AS n) TO 'build/out.parquet';\n"),
    );
    common::arc_run(dir.path());
    let record = record(dir.path());
    let outputs = record["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 1, "{outputs:?}");
    assert_eq!(outputs[0]["path"], "build/out.parquet");
    assert_eq!(outputs[0]["kind"], "file");
    let files = outputs[0]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    let file = dir.path().join("build/out.parquet");
    assert_eq!(files[0]["path"], "build/out.parquet");
    assert_eq!(files[0]["bytes"], std::fs::metadata(&file).unwrap().len());
    assert_eq!(files[0]["sha256"], sha256(&file));
}

#[test]
fn records_a_directory_output() {
    let dir = project(
        "name: outs\nsteps:\n  - name: parts\n    sql: models/out.sql\n  - name: extra\n    command: mkdir -p build/parts/sub && printf x > build/parts/sub/extra.txt\noutputs:\n  - build/parts/\n  - arcform.yaml\n",
        Some(
            "CREATE OR REPLACE TABLE t AS SELECT * FROM (VALUES (1, 'a'), (2, 'b')) v(year, v);\n\
             COPY t TO 'build/parts' (FORMAT parquet, PARTITION_BY (year), OVERWRITE_OR_IGNORE);\n",
        ),
    );
    common::arc_run(dir.path());
    let record = record(dir.path());
    let outputs = record["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 2, "{outputs:?}");
    assert_eq!(outputs[0]["path"], "build/parts", "trailing slash dropped");
    assert_eq!(outputs[0]["kind"], "directory");
    assert_eq!(outputs[1]["path"], "arcform.yaml", "manifest order kept");
    let paths: Vec<&str> = outputs[0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"build/parts/sub/extra.txt"), "{paths:?}");
    assert!(
        paths
            .iter()
            .filter(|p| p.starts_with("build/parts/year="))
            .count()
            == 2,
        "{paths:?}"
    );
    let mut sorted = paths.clone();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    assert_eq!(paths, sorted);
    for f in outputs[0]["files"].as_array().unwrap() {
        let full = dir.path().join(f["path"].as_str().unwrap());
        assert_eq!(f["sha256"], sha256(&full), "{f}");
        assert_eq!(f["bytes"], std::fs::metadata(&full).unwrap().len());
    }
}

#[test]
fn missing_output_fails_the_run() {
    let dir = project(
        "name: outs\nsteps:\n  - name: ok\n    command: 'true'\noutputs:\n  - build/never.parquet\nhooks:\n  on_success:\n    name: yes\n    command: touch build/on_success\n  on_failure:\n    name: no\n    command: touch build/on_failure\n",
        None,
    );
    let (code, stdout, stderr) = common::arc_run_raw(dir.path());

    let failed_step = project(
        "name: fails\nsteps:\n  - name: bad\n    command: 'false'\n",
        None,
    );
    let (failed_code, _, _) = common::arc_run_raw(failed_step.path());

    assert_eq!(code, failed_code, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert_ne!(code, Some(0));
    assert!(stderr.contains("build/never.parquet"), "{stderr}");
    let record = record(dir.path());
    assert_ne!(record["run"]["outcome"], "success");
    assert_eq!(record["outputs"], serde_json::json!([]));
    assert!(!dir.path().join("build/on_success").exists());
    assert!(dir.path().join("build/on_failure").exists());
}

#[test]
fn a_symlink_output_fails_the_run() {
    let dir = project(
        "name: outs\nsteps:\n  - name: link\n    command: mkdir -p build/d && ln -s ../../arcform.yaml build/d/link\noutputs:\n  - build/d\n",
        None,
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0));
    assert!(
        stderr.contains("build/d/link") && stderr.contains("symlink"),
        "{stderr}"
    );
}

#[test]
fn a_symlinked_folder_on_an_outputs_path_fails_the_run() {
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("x.txt"), "outside").unwrap();
    let dir = project(
        &format!(
            "name: outs\nsteps:\n  - name: link\n    command: ln -s {} link\noutputs:\n  - link/x.txt\n",
            elsewhere.path().display()
        ),
        None,
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    assert_ne!(code, Some(0));
    assert!(
        stderr.contains("link/x.txt") && stderr.contains("symlink"),
        "{stderr}"
    );
}

#[test]
fn an_output_a_hook_removes_fails_the_run() {
    let dir = project(
        "name: outs\nsteps:\n  - name: write\n    command: printf x > build/x.txt\noutputs:\n  - build/x.txt\nhooks:\n  on_success:\n    name: remove\n    command: rm build/x.txt\n",
        None,
    );
    let (code, _, stderr) = common::arc_run_raw(dir.path());
    let failed_step = project(
        "name: fails\nsteps:\n  - name: bad\n    command: 'false'\n",
        None,
    );
    let (failed_code, _, _) = common::arc_run_raw(failed_step.path());
    assert_eq!(code, failed_code, "{stderr}");
    assert!(stderr.contains("build/x.txt"), "{stderr}");
    let record = record(dir.path());
    assert_ne!(record["run"]["outcome"], "success");
    assert_eq!(record["outputs"], serde_json::json!([]));
}

#[test]
fn no_outputs_records_empty_list() {
    let dir = project(
        "name: outs\nsteps:\n  - name: ok\n    command: 'true'\n",
        None,
    );
    common::arc_run(dir.path());
    assert_eq!(record(dir.path())["outputs"], serde_json::json!([]));
}
