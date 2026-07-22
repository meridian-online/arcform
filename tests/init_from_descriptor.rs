//! End-to-end: `arc init --from-descriptor` generates a runnable Protocol from a
//! Frictionless descriptor, and `arc run` executes the GENERATED manifest.
//!
//! Self-contained: the committed `tests/fixtures/signups.datapackage.json` (and
//! its `signups.duckdb`) were produced by running the shipped `dovetail relate`
//! over `tests/fixtures/build.sql`. This test needs no sibling repo — only the
//! `arc` binary and a `duckdb` CLI on PATH (the same requirement as any `arc run`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
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
fn generates_and_runs_a_protocol_from_a_descriptor() {
    let arc = env!("CARGO_BIN_EXE_arc");
    let workspace = tempfile::tempdir().unwrap();
    let descriptor = fixtures_dir().join("signups.datapackage.json");

    // --- arc init --from-descriptor -----------------------------------------
    let init = Command::new(arc)
        .current_dir(workspace.path())
        .args(["init", "assemble_demo", "--from-descriptor"])
        .arg(&descriptor)
        .output()
        .expect("spawn arc init");
    assert!(
        init.status.success(),
        "arc init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let project = workspace.path().join("assemble_demo");
    assert!(project.join("arcform.yaml").is_file());
    assert!(
        project.join("signups.duckdb").is_file(),
        "database copied in"
    );
    assert!(
        project.join("datapackage.json").is_file(),
        "companion descriptor written"
    );

    // --- generated arcform.yaml: exactly three steps, in order --------------
    let yaml = std::fs::read_to_string(project.join("arcform.yaml")).unwrap();
    let manifest: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let steps = manifest["steps"].as_sequence().expect("steps sequence");
    let step_names: Vec<&str> = steps.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(
        step_names,
        ["load_accounts", "load_logins", "fk_logins_accounts"],
        "generated steps"
    );

    // The fk step fans in from both tables.
    let fk = steps
        .iter()
        .find(|s| s["name"] == "fk_logins_accounts")
        .unwrap();
    let deps: Vec<&str> = fk["depends_on"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        deps.contains(&"logins") && deps.contains(&"accounts"),
        "fk depends_on both tables: {deps:?}"
    );

    // The fk model joins account_id -> id.
    let fk_sql = std::fs::read_to_string(project.join("models/fk_logins_accounts.sql")).unwrap();
    assert!(
        fk_sql.contains(r#"c."account_id" = p."id""#),
        "fk joins account_id -> id: {fk_sql}"
    );

    // --- arc run on the GENERATED project -----------------------------------
    let run = Command::new(arc)
        .current_dir(&project)
        .arg("run")
        .output()
        .expect("spawn arc run");
    assert!(
        run.status.success(),
        "arc run failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    // --- the emitted b4/1 contract ------------------------------------------
    let contract = read_contract(&project);
    assert_eq!(contract["contract_version"], "b4/1");
    assert_eq!(contract["run"]["outcome"], "success");

    let assets = contract["assets"].as_array().expect("assets array");
    let find = |name: &str| assets.iter().find(|a| a["name"] == name);

    let accounts = find("accounts").expect("accounts asset");
    let logins = find("logins").expect("logins asset");
    let fk_asset = find("fk_logins_accounts").expect("fk asset");

    // Each table is produced by its load step.
    assert_eq!(accounts["produced_by"], "load_accounts");
    assert_eq!(logins["produced_by"], "load_logins");
    assert_eq!(fk_asset["produced_by"], "fk_logins_accounts");

    // The fk asset consumes BOTH tables: accounts and logins are each consumed by
    // the fk step.
    let consumed_by = |a: &serde_json::Value| -> Vec<String> {
        a["consumed_by"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    assert!(
        consumed_by(accounts).contains(&"fk_logins_accounts".to_string()),
        "accounts consumed by fk"
    );
    assert!(
        consumed_by(logins).contains(&"fk_logins_accounts".to_string()),
        "logins consumed by fk"
    );
}
