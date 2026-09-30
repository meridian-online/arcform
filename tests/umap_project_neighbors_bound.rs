//! `arc run` refuses a `umap_project` `neighbors:` above one below the row count of the
//! Parquet its `input:` names, and arc's description of `neighbors` states that bound.
//!
//! The script clamps `neighbors` to one below the row count, so above the bound a map
//! is the same map the bound draws and the run exits 0. arc counts the input when the
//! step runs, because an earlier step may write it, and refuses a set value above the
//! bound before the script starts and before anything is written.
//!
//! NONE OF THIS NEEDS A REAL `uv`. The refusal comes before `uv` is started, and where
//! a test needs to see what arc hands `uv` it puts a stand-in first on PATH that
//! records its arguments. Where a test needs NO `uv`, PATH holds only the `duckdb`
//! the SQL step shells out to, because CI's `mcp` step runs this suite after `uv` is
//! installed.

#[allow(dead_code)]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// The Protocol under test: `write_input` writes a Parquet of `rows` rows, `project`
/// is `umap_project` reading it with `extra` spliced into its `with:` block, and
/// `after` writes a file, so a run that reached it shows. `out` and `fit` sit under
/// `maps/`, which nothing creates before the run.
fn protocol(dir: &Path, rows: u64, extra: &str, step_extra: &str) {
    std::fs::create_dir_all(dir.join("models")).unwrap();
    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(
        dir.join("models/write_input.sql"),
        format!(
            "COPY (SELECT range AS id, range * 1.0 AS x, range * 2.0 AS y FROM range({rows})) \
             TO 'build/in.parquet' (FORMAT parquet);\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("models/after.sql"),
        "COPY (SELECT 1 AS ran) TO 'build/after.parquet' (FORMAT parquet);\n",
    )
    .unwrap();
    let manifest = format!(
        r#"name: umap_neighbors_bound
engine: duckdb
db: build/fixture.duckdb

steps:
  - name: write_input
    sql: models/write_input.sql

  - name: project
    op: umap_project@1
    with:
      input: build/in.parquet
      columns: [x, y]
      out: maps/out.parquet
      fit: maps/fit.umap
{extra}{step_extra}
  - name: after
    sql: models/after.sql
"#
    );
    std::fs::write(dir.join("arcform.yaml"), manifest).unwrap();
}

/// The `duckdb` executable on this machine's PATH, which the SQL steps shell out to.
fn duckdb_on_path() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("duckdb"))
        .find(|p| p.is_file())
        .expect("the duckdb CLI must be on PATH: arc runs a SQL step through it")
}

/// A directory holding a link to `duckdb` and, when `stand_in` is set, a `uv` that
/// writes each argument it is given to `args_file` on its own line and exits 0.
fn bin_dir(project: &Path, stand_in: Option<&Path>) -> PathBuf {
    let bin = project.join(".bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(duckdb_on_path(), bin.join("duckdb")).unwrap();
    if let Some(args_file) = stand_in {
        let uv = bin.join("uv");
        std::fs::write(
            &uv,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                args_file.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// One `arc run` with PATH set to `path`, returning (exit code, stdout, stderr).
fn arc_run(project: &Path, path: &Path) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .env("PATH", path)
        .arg("run")
        .output()
        .expect("spawn arc run");
    (
        out.status.code(),
        common::strip_ansi(&String::from_utf8_lossy(&out.stdout)),
        common::strip_ansi(&String::from_utf8_lossy(&out.stderr)),
    )
}

/// The one run contract this project's single run wrote.
fn run_contract(project: &Path) -> serde_json::Value {
    let runs = project.join("build/.arcform/runs");
    let contracts: Vec<PathBuf> = std::fs::read_dir(&runs)
        .unwrap_or_else(|e| panic!("no run contracts under {}: {e}", runs.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    assert_eq!(contracts.len(), 1, "one run, one contract: {contracts:?}");
    serde_json::from_str(&std::fs::read_to_string(&contracts[0]).unwrap()).unwrap()
}

/// The contract's record of the step called `name`.
fn step_record<'a>(contract: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    contract["steps"]
        .as_array()
        .expect("the contract lists its steps")
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no step '{name}' in the contract: {contract}"))
}

/// Does stdout carry a step line for `step`?
fn has_step_line(stdout: &str, step: &str) -> bool {
    stdout
        .lines()
        .any(|l| l.starts_with('[') && l.contains(&format!("] {step} ")))
}

const REFUSAL: &str = "`neighbors: 60` is above 47, one below the 48 rows of build/in.parquet";

/// AC: on 48 rows, `neighbors: 60` is refused after the step that writes the input
/// has run, naming the value, the bound and the row count; nothing is written under
/// the directory `out` and `fit` were set under, and the step after it never runs.
#[test]
fn a_neighbors_above_one_below_the_row_count_is_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    protocol(project, 48, "      neighbors: 60\n", "");
    let bin = bin_dir(project, None);

    let (code, stdout, stderr) = arc_run(project, &bin);

    assert_eq!(
        code,
        Some(1),
        "the refusal exits 1:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert_eq!(common::step_outcome(&stdout, "write_input"), "ran");
    assert!(project.join("build/in.parquet").is_file());
    for named in ["umap_project", REFUSAL] {
        assert!(
            stderr.contains(named),
            "stderr must name {named:?}:\n{stderr}"
        );
    }
    for written in ["maps/out.parquet", "maps/fit.umap", "maps"] {
        assert!(
            !project.join(written).exists(),
            "{written} must not exist after a refused step"
        );
    }
    assert!(
        !has_step_line(&stdout, "after") && !project.join("build/after.parquet").exists(),
        "no step after the refused one runs:\n{stdout}"
    );
}

/// AC: the refusal is not retried. With two attempts allowed, no second attempt is
/// announced and the run contract records one.
#[test]
fn the_refusal_is_not_retried_and_the_contract_records_one_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    protocol(
        project,
        48,
        "      neighbors: 60\n",
        "    retry: {max_attempts: 2, backoff_sec: 0}\n",
    );
    let bin = bin_dir(project, None);

    let (code, stdout, stderr) = arc_run(project, &bin);

    assert_eq!(code, Some(1), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.contains(REFUSAL), "stderr:\n{stderr}");
    assert!(
        !stdout.contains("[retry 2/2") && !stderr.contains("[retry 2/2"),
        "a refusal is not retried:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let contract = run_contract(project);
    let project_step = step_record(&contract, "project");
    assert_eq!(
        project_step["retry"]["max_attempts"], 2,
        "the retry policy reached the step: {project_step}"
    );
    assert_eq!(
        project_step["attempts"], 1,
        "the contract records one attempt: {project_step}"
    );
}

/// AC: the bound itself runs. On 48 rows `neighbors: 47` reaches `uv` as
/// `--neighbors 47`, and with no `uv` on PATH the step fails starting it, not on the
/// refusal.
#[test]
fn a_neighbors_at_the_bound_reaches_uv() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    protocol(project, 48, "      neighbors: 47\n", "");
    let args_file = project.join("uv-args.txt");
    let bin = bin_dir(project, Some(&args_file));

    let (_, stdout, stderr) = arc_run(project, &bin);

    let args = std::fs::read_to_string(&args_file).unwrap_or_else(|e| {
        panic!("the stand-in uv was not started ({e}):\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    let args: Vec<&str> = args.lines().collect();
    assert!(
        args.windows(2).any(|w| w == ["--neighbors", "47"]),
        "uv must be handed --neighbors 47: {args:?}"
    );
    assert!(!stderr.contains("is above"), "47 is not refused:\n{stderr}");

    // The same Protocol with no `uv` at all fails where `uv` is started.
    let bare = tempfile::tempdir().unwrap();
    protocol(bare.path(), 48, "      neighbors: 47\n", "");
    let bin = bin_dir(bare.path(), None);

    let (code, stdout, stderr) = arc_run(bare.path(), &bin);

    assert_eq!(
        code,
        Some(2),
        "a uv that cannot be started is a step failure, exit 2:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("step 'umap_project' failed: No such file or directory"),
        "the step fails starting uv:\n{stderr}"
    );
    assert!(
        !stderr.contains("is above") && !stderr.contains("invalid manifest"),
        "and not on the refusal:\n{stderr}"
    );
}

/// AC: an unset `neighbors` is not counted against the input. On 10 rows, below the
/// default of 15, the step reaches `uv` with no `--neighbors`, and the script's clamp
/// gives it 9.
#[test]
fn an_unset_neighbors_on_a_small_table_is_not_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    protocol(project, 10, "", "");
    let args_file = project.join("uv-args.txt");
    let bin = bin_dir(project, Some(&args_file));

    let (_, stdout, stderr) = arc_run(project, &bin);

    let args = std::fs::read_to_string(&args_file).unwrap_or_else(|e| {
        panic!("the stand-in uv was not started ({e}):\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    let args: Vec<&str> = args.lines().collect();
    assert!(
        args.contains(&"--input") && !args.contains(&"--neighbors"),
        "an unset neighbors is left out of uv's arguments: {args:?}"
    );
    assert!(!stderr.contains("is above"), "stderr:\n{stderr}");
}

/// AC: `operator_describe` over `arc mcp` hands a program the bound as a rule on the
/// input's row count, the default it bounds, and a description saying both.
#[cfg(feature = "mcp")]
#[test]
fn operator_describe_states_the_neighbors_bound_over_arc_mcp() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn arc mcp");
    {
        let mut stdin = child.stdin.take().unwrap();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "operator_describe", "arguments": { "operator": "umap_project" } },
        });
        writeln!(stdin, "{request}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "arc mcp failed: {out:?}");
    let response: serde_json::Value =
        serde_json::from_str(String::from_utf8(out.stdout).unwrap().trim()).unwrap();
    let result = &response["result"];
    assert_eq!(result["isError"], false, "result: {result}");
    let neighbors = &result["structuredContent"]["properties"]["neighbors"];

    assert_eq!(neighbors["minimum"], 2, "{neighbors}");
    assert_eq!(neighbors["default"], 15, "{neighbors}");
    assert_eq!(
        neighbors["x-at-most"],
        serde_json::json!({ "rows_of": "input", "minus": 1, "bounds_default": true }),
        "{neighbors}"
    );
    let described = neighbors["description"].as_str().unwrap();
    for said in [
        "one below the input's row count",
        "refuses a value above that bound",
        "An unset value takes 15 or that bound, whichever is smaller",
    ] {
        assert!(described.contains(said), "must say {said:?}: {described}");
    }
}
