//! End-to-end for the `embed_project` operator: a real `arc run` over the fixture
//! Protocol in `tests/fixtures/embed_project/`, which turns a text column into the two
//! coordinates a map needs.
//!
//! WHAT NEEDS `uv` AND WHAT DOES NOT. The projection itself runs Python on the uv-run
//! substrate, so every test that produces a map skips where `uv`
//! is not installed, exactly as the `finetype_validate` gate skips without its
//! extension. CI has no `uv`, so what CI runs here is the one test that does not need
//! it: the refusal when the model asset is missing, which is decided in Rust before
//! anything is spawned. The rest are for a developer machine, and they are where the
//! byte-identity claim is actually checked.
//!
//! FIRST RUN COSTS A DOWNLOAD. `uv` resolves umap-learn, numba, scipy and
//! scikit-learn on first use (a few hundred megabytes, cached thereafter) and numba
//! JIT-compiles UMAP on first call. Subsequent runs are seconds.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/embed_project")
}

/// Is `uv` on PATH? Every test that produces a map needs it.
fn have_uv() -> bool {
    Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Copy the fixture tree, skipping `build/` — a developer who has run `arc run` in the
/// fixture directory itself must not have last week's artifacts staged into a test.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "build" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// The fixture Protocol, staged in a temp directory so a run cannot dirty the tree.
fn staged_protocol() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&fixture_dir(), tmp.path());
    tmp
}

/// One `arc run` with extra environment, returning (exit code, stdout, stderr).
fn arc_run_with_env(project: &Path, env: &HashMap<&str, &str>) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .arg("run")
        .envs(env)
        .output()
        .expect("spawn arc run");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn projected(project: &Path) -> PathBuf {
    project.join("build/corpus_projected.parquet")
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    format!("{:x}", Sha256::digest(&bytes))
}

/// Wipe everything a previous run produced — artifacts, the pipeline database and
/// arcform's own run state under `build/.arcform` — so the next `arc run` re-executes
/// every step rather than reporting one of them clean.
fn clear_cache(project: &Path) {
    let build = project.join("build");
    std::fs::remove_dir_all(&build).unwrap_or_else(|e| panic!("clear {}: {e}", build.display()));
    assert!(!build.exists(), "the cache must actually be gone");
}

/// `(column, type)` for the projected Parquet, read back through DuckDB.
fn columns_of(parquet: &Path) -> Vec<(String, String)> {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "DESCRIBE SELECT * FROM read_parquet('{}')",
            parquet.display()
        ))
        .unwrap();
    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

/// AC2, and the half of it that CI runs. The model is a declared input, so a Protocol
/// pointed at one nothing has fetched stops — naming the file it looked for — instead
/// of quietly reaching the network for a model of its own choosing.
#[test]
fn a_model_that_was_never_fetched_stops_the_run_naming_the_file() {
    let tmp = staged_protocol();
    let project = tmp.path();
    std::fs::remove_dir_all(project.join("model")).unwrap();

    let (code, stdout, stderr) = common::arc_run_raw(project);
    assert_ne!(code, Some(0), "the run must fail:\n{stdout}\n{stderr}");
    let told = format!("{stdout}{stderr}");
    assert!(
        told.contains("the model asset model is not on disk"),
        "the refusal names the declared model asset:\n{told}"
    );
    assert!(
        told.contains("this operator does not download one"),
        "the refusal says fetching the model is the Protocol's job:\n{told}"
    );
    assert!(
        !projected(project).exists(),
        "nothing may be written when the model is missing"
    );
    // The model is in the graph, not beside it: `arc run` prints it as a node the
    // projection step feeds from.
    let graph = common::strip_ansi(&stdout);
    assert!(
        graph.contains("model [directory]"),
        "the model is an asset-graph node, not an invisible dependency:\n{graph}"
    );
}

/// AC1. Every input column survives, the two coordinates arrive beside them as
/// floating-point, and they carry the text's structure rather than being two columns
/// of anything: the corpus is 48 texts on two subjects, and each one lands nearer its
/// own subject's centre than the other's.
#[test]
fn the_projection_adds_two_coordinates_to_every_input_column() {
    if !have_uv() {
        eprintln!("skipping the_projection_adds_two_coordinates: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();
    common::arc_run(project);

    let out = projected(project);
    assert_eq!(
        columns_of(&out),
        vec![
            ("id".to_string(), "INTEGER".to_string()),
            ("title".to_string(), "VARCHAR".to_string()),
            ("description".to_string(), "VARCHAR".to_string()),
            ("projection_x".to_string(), "DOUBLE".to_string()),
            ("projection_y".to_string(), "DOUBLE".to_string()),
        ],
        "every input column, in order, then the two coordinates as floating point"
    );

    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, projection_x, projection_y FROM read_parquet('{}') ORDER BY id",
            out.display()
        ))
        .unwrap();
    let rows: Vec<(i32, f64, f64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i32>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows.len(), 48, "one coordinate pair per corpus row");
    assert!(
        rows.iter().all(|(_, x, y)| x.is_finite() && y.is_finite()),
        "a coordinate that is NaN or infinite is not a position on a map"
    );

    // corpus.csv rows 1..=24 are the harbour subject, 25..=48 the company-results
    // subject. A projection that ignored the text — zeros, or the row number — would
    // put the two subjects on top of each other.
    let centre = |group: &[(i32, f64, f64)]| {
        let n = group.len() as f64;
        (
            group.iter().map(|r| r.1).sum::<f64>() / n,
            group.iter().map(|r| r.2).sum::<f64>() / n,
        )
    };
    let (harbour, markets): (Vec<_>, Vec<_>) = rows.iter().partition(|(id, _, _)| *id <= 24);
    let harbour_centre = centre(&harbour);
    let markets_centre = centre(&markets);
    let distance = |(x, y): (f64, f64), (cx, cy): (f64, f64)| (x - cx).hypot(y - cy);
    for (id, x, y) in &rows {
        let own = if *id <= 24 {
            harbour_centre
        } else {
            markets_centre
        };
        let other = if *id <= 24 {
            markets_centre
        } else {
            harbour_centre
        };
        assert!(
            distance((*x, *y), own) < distance((*x, *y), other),
            "row {id} at ({x:.3}, {y:.3}) is nearer the other subject's centre — the \
             coordinates do not carry the text"
        );
    }
}

/// AC3. Two runs, each from a cache cleared to nothing, produce the same bytes — and
/// the claim is not vacuous, because changing which column is embedded moves them.
/// A projection that wrote a constant would pass the first half and fail the second.
#[test]
fn two_runs_from_a_cleared_cache_emit_byte_identical_parquet() {
    if !have_uv() {
        eprintln!("skipping two_runs_from_a_cleared_cache: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();

    let ran = |stdout: &str| {
        assert_eq!(
            common::step_outcome(stdout, "project"),
            "ran",
            "the projection has to have re-executed — a skipped step would compare a \
             file against itself and prove nothing:\n{stdout}"
        );
    };

    ran(&common::arc_run(project));
    let first = sha256(&projected(project));

    clear_cache(project);
    ran(&common::arc_run(project));
    let second = sha256(&projected(project));
    assert_eq!(
        first, second,
        "two runs of the same Protocol over the same input must emit the same bytes"
    );

    // Embed a different column of the same corpus: same code, same seed, same model,
    // different text — so the bytes must move.
    let manifest = project.join("arcform.yaml");
    let rewritten = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("text_column: description", "text_column: title");
    std::fs::write(&manifest, rewritten).unwrap();
    clear_cache(project);
    common::arc_run(project);
    assert_ne!(
        first,
        sha256(&projected(project)),
        "projecting a different text column must produce different coordinates — \
         identical bytes here would mean the map does not depend on the text"
    );
}

/// AC5. With the model on disk and `uv`'s cache warm, the step needs nothing from the
/// network and consults no credential.
///
/// Outbound HTTP is made unavailable two ways, because one is not enough. `UV_OFFLINE`
/// stops `uv` reaching a package registry, and nothing else — it does not constrain
/// what the script itself does. So the proxy variables point every HTTP client in the
/// step at a port nothing listens on: `urllib`, `requests`, `httpx` and
/// `huggingface_hub` all honour them, which is the whole realistic set of ways a
/// Python operator reaches out. A raw socket to an IP address would still get through
/// and nothing here would see it; that is the limit of what this test asserts.
///
/// Three plausible API-key variables are set to values that would break anything that
/// used them, and the run has to land on the same bytes as the one that had a network.
#[test]
fn the_step_completes_with_the_network_disabled_and_no_credentials() {
    if !have_uv() {
        eprintln!("skipping the_step_completes_with_the_network_disabled: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();

    // The first run is what warms `uv`'s cache on this machine; the claim is about the
    // second. A machine that has run this before pays nothing for it.
    common::arc_run(project);
    let online = sha256(&projected(project));

    clear_cache(project);
    let mut env = HashMap::new();
    env.insert("UV_OFFLINE", "1");
    env.insert("HF_HUB_OFFLINE", "1");
    // Port 1 on the loopback: a connection there is refused immediately rather than
    // hanging, so a step that reaches for HTTP fails fast instead of timing out.
    env.insert("HTTP_PROXY", "http://127.0.0.1:1");
    env.insert("HTTPS_PROXY", "http://127.0.0.1:1");
    env.insert("ALL_PROXY", "http://127.0.0.1:1");
    env.insert("NO_PROXY", "");
    env.insert("OPENAI_API_KEY", "broken-on-purpose");
    env.insert("ANTHROPIC_API_KEY", "broken-on-purpose");
    env.insert("HF_TOKEN", "broken-on-purpose");
    let (code, stdout, stderr) = arc_run_with_env(project, &env);
    assert_eq!(
        code,
        Some(0),
        "the step must complete with the network disabled:\n{stdout}\n{stderr}"
    );
    assert_eq!(
        online,
        sha256(&projected(project)),
        "an offline run must produce the same map as an online one"
    );
}

/// The two documented knobs change the MAP, not just the command line.
///
/// `embed_project_args_pass_the_knobs_through` asserts that `neighbors:` and
/// `min_dist:` reach the argv as the right strings, and that is a weaker claim than it
/// looks: a script that accepted both flags and then projected with its own defaults
/// would satisfy it completely, while every Protocol setting either field silently got
/// a map it did not ask for. So this projects the same corpus against the same model
/// three times, moving one knob at a time, and compares the bytes.
#[test]
fn the_knobs_a_manifest_sets_change_the_map_not_just_the_argv() {
    if !have_uv() {
        eprintln!("skipping the_knobs_a_manifest_sets_change_the_map: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();
    std::fs::copy(project.join("knobs.yaml"), project.join("arcform.yaml")).unwrap();

    let stdout = common::arc_run(project);
    let out = |name: &str| sha256(&project.join(format!("build/{name}.parquet")));
    for step in [
        "project_default",
        "project_more_neighbours",
        "project_looser_packing",
    ] {
        assert_eq!(
            common::step_outcome(&stdout, step),
            "ran",
            "every projection has to have executed, or the comparison below is between \
             two files that were never written:\n{stdout}"
        );
    }

    assert_ne!(
        out("default"),
        out("more_neighbours"),
        "`neighbors: 40` produced the same bytes as the script's default of 15 — the \
         field is documented, reaches the argv, and does not reach the layout"
    );
    assert_ne!(
        out("default"),
        out("looser_packing"),
        "`min_dist: 0.9` produced the same bytes as the script's default of 0.1 — the \
         field is documented, reaches the argv, and does not reach the layout"
    );
}
