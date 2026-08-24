//! End-to-end for the `umap_project` operator: a real `arc run` over the fixture
//! Protocol in `tests/fixtures/umap_project/`, which reduces four columns that are
//! ALREADY NUMBERS to the two coordinates a map is drawn from.
//!
//! THIS IS THE CASE THE MERGED OPERATOR COULD NOT SERVE. Before the split, the only
//! way into a projection was through an embedding of a text column, so a table of
//! longitudes and latitudes — the shape of the published Embedding Atlas housing
//! example — had no route through the step at all. The fixture directory holds no
//! model and the manifest names no text column; that is the property, not a detail of
//! the setup.
//!
//! WHAT NEEDS `uv` AND WHAT DOES NOT. The projection itself runs Python on the uv-run
//! substrate, so every test here that produces a map skips where `uv` is not
//! installed, exactly as the `finetype_validate` gate skips without its extension. CI
//! has no `uv`. What CI runs for this operator is elsewhere: the load-time refusals,
//! the argv and the frozen-script contract in `src/operator.rs`'s unit tests, and the
//! script's own type classifier in
//! `operators/umap_project/test_umap_project.py`, which is stdlib-only and runs in the
//! `operators` job. These are for a developer machine, and they are where the
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/umap_project")
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
    project.join("build/homes_projected.parquet")
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

/// `(column, type)` for a Parquet, read back through DuckDB.
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

/// The coordinates, with the district each row belongs to.
fn placed(parquet: &Path) -> Vec<(String, f64, f64)> {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT district, projection_x, projection_y FROM read_parquet('{}') ORDER BY id",
            parquet.display()
        ))
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

/// The fit's own fingerprint — the same value on every row of one output (a separate
/// test proves that), so reading the first row's is enough.
fn fit_id_of(parquet: &Path) -> String {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT projection_fit_id FROM read_parquet('{}') LIMIT 1",
            parquet.display()
        ))
        .unwrap();
    stmt.query_row([], |r| r.get::<_, String>(0)).unwrap()
}

/// THE CARD'S FIRST CRITERION, end to end. Columns that are already numbers become
/// two coordinates, with no text column in the manifest and no model anywhere in the
/// Protocol — and the coordinates carry the numbers' structure rather than being two
/// columns of anything.
///
/// The structure claim has teeth because the two districts INTERLEAVE by id: odd rows
/// are coastal, even rows inland. A projection that laid points out by row number —
/// or that ignored its input and wrote a constant, or noise — would put neighbouring
/// ids together, and the assertion below would fail on most of the table. The old
/// fixture's two subjects were contiguous blocks of ids, where a row-number layout
/// would have passed the same check.
#[test]
fn a_map_of_plain_numbers_needs_no_model_and_no_text_column() {
    if !have_uv() {
        eprintln!("skipping a_map_of_plain_numbers: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();

    // Not a setup detail: there is nothing in this Protocol that could embed anything.
    //
    // Read as PARSED YAML rather than as text. A `contains("model")` over the file
    // matches this fixture's own comments, which are about the absence of a model —
    // a scan of prose cannot tell a field from a sentence about that field, and the
    // first version of this assertion failed on exactly that.
    let manifest: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(project.join("arcform.yaml")).unwrap())
            .unwrap();
    let with = &manifest["steps"][2]["with"];
    assert_eq!(
        manifest["steps"][2]["op"].as_str(),
        Some("umap_project@1"),
        "the third step is the one under test"
    );
    let fields: Vec<&str> = with
        .as_mapping()
        .unwrap()
        .keys()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        vec!["input", "columns", "out", "metric"],
        "the step reaches a map with these fields and no others — no text column and          no model appear anywhere in its configuration"
    );
    assert!(
        !project.join("model").exists(),
        "no model directory may exist in a Protocol that only projects numbers"
    );

    let stdout = common::arc_run(project);

    // The run's own asset graph, not the manifest: a model would be a Directory node
    // in it, the way the text fixture's is. There is none, so nothing in this
    // Protocol depends on bytes a model would have supplied.
    let graph = common::strip_ansi(&stdout);
    assert!(
        !graph.contains("[directory]"),
        "a projection of plain numbers declares no directory asset — a model would \
         appear here as one:\n{graph}"
    );

    let out = projected(project);
    assert_eq!(
        columns_of(&out),
        vec![
            ("id".to_string(), "INTEGER".to_string()),
            ("district".to_string(), "VARCHAR".to_string()),
            ("longitude".to_string(), "DOUBLE".to_string()),
            ("latitude".to_string(), "DOUBLE".to_string()),
            ("median_income".to_string(), "DOUBLE".to_string()),
            ("rooms_per_household".to_string(), "DOUBLE".to_string()),
            ("projection_x".to_string(), "DOUBLE".to_string()),
            ("projection_y".to_string(), "DOUBLE".to_string()),
            ("projection_fit_id".to_string(), "VARCHAR".to_string()),
        ],
        "every input column, in order — including the VARCHAR the projection did not \
         name — then the two coordinates as floating point, then the fit fingerprint"
    );

    let rows = placed(&out);
    assert_eq!(rows.len(), 48, "one coordinate pair per property");
    assert!(
        rows.iter().all(|(_, x, y)| x.is_finite() && y.is_finite()),
        "a coordinate that is NaN or infinite is not a position on a map"
    );

    let centre = |group: &[&(String, f64, f64)]| {
        let n = group.len() as f64;
        (
            group.iter().map(|r| r.1).sum::<f64>() / n,
            group.iter().map(|r| r.2).sum::<f64>() / n,
        )
    };
    let (coastal, inland): (Vec<_>, Vec<_>) = rows.iter().partition(|(d, _, _)| d == "coastal");
    assert_eq!(coastal.len(), 24, "the fixture is half coastal");
    assert_eq!(inland.len(), 24, "the fixture is half inland");
    let coastal_centre = centre(&coastal);
    let inland_centre = centre(&inland);
    let distance = |(x, y): (f64, f64), (cx, cy): (f64, f64)| (x - cx).hypot(y - cy);
    for (district, x, y) in &rows {
        let (own, other) = if district == "coastal" {
            (coastal_centre, inland_centre)
        } else {
            (inland_centre, coastal_centre)
        };
        assert!(
            distance((*x, *y), own) < distance((*x, *y), other),
            "a {district} property at ({x:.3}, {y:.3}) is nearer the other district's \
             centre — the coordinates do not carry the numbers"
        );
    }
}

/// Determinism, over an input with no text and no model in it. Two runs, each from a
/// cache cleared to nothing, produce the same bytes — and the claim is not vacuous,
/// because changing which columns are projected moves them. A projection that wrote a
/// constant would pass the first half and fail the second.
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

    // Project a different set of columns of the same table: same code, same seed,
    // same metric, different numbers — so the bytes must move.
    let manifest = project.join("arcform.yaml");
    let rewritten = std::fs::read_to_string(&manifest).unwrap().replace(
        "columns: [longitude, latitude, median_income, rooms_per_household]",
        "columns: [longitude, latitude]",
    );
    std::fs::write(&manifest, rewritten).unwrap();
    clear_cache(project);
    common::arc_run(project);
    assert_ne!(
        first,
        sha256(&projected(project)),
        "projecting different columns must produce different coordinates — identical \
         bytes here would mean the map does not depend on the numbers"
    );
}

/// `projection_fit_id` is the file's own answer to "was this the same fit" — see
/// operators/umap_project/README.md, "Telling a refit from an append". Broadcast to
/// every row, so first: one value covers the whole file. Then: two runs over
/// byte-identical input reproduce the identical id — it has to, given the determinism
/// `two_runs_from_a_cleared_cache_emit_byte_identical_parquet` already proves — and a
/// run over DIFFERENT columns of the same table carries a DIFFERENT id, because an id
/// that could not move would tell a reader nothing about whether a refit happened.
#[test]
fn projection_fit_id_agrees_with_an_identical_refit_and_moves_with_a_different_one() {
    if !have_uv() {
        eprintln!("skipping projection_fit_id: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();

    common::arc_run(project);
    let out = projected(project);
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let distinct_ids: i64 = conn
        .query_row(
            &format!(
                "SELECT count(DISTINCT projection_fit_id) FROM read_parquet('{}')",
                out.display()
            ),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        distinct_ids, 1,
        "one fit produces one id — every row of the same output must carry it"
    );
    let first_id = fit_id_of(&out);

    clear_cache(project);
    common::arc_run(project);
    assert_eq!(
        fit_id_of(&projected(project)),
        first_id,
        "refitting byte-identical input under byte-identical settings must reproduce \
         the identical fit_id, not merely identical coordinates"
    );

    let manifest = project.join("arcform.yaml");
    let rewritten = std::fs::read_to_string(&manifest).unwrap().replace(
        "columns: [longitude, latitude, median_income, rooms_per_household]",
        "columns: [longitude, latitude]",
    );
    std::fs::write(&manifest, rewritten).unwrap();
    clear_cache(project);
    common::arc_run(project);
    assert_ne!(
        fit_id_of(&projected(project)),
        first_id,
        "projecting different columns is a different fit and must carry a different \
         fit_id"
    );
}

/// The three documented knobs change the MAP, not just the command line.
///
/// `umap_project_args_pass_every_column_and_knob_through` asserts that `neighbors:`,
/// `min_dist:` and `metric:` reach the argv as the right strings, and that is a weaker
/// claim than it looks: a script that accepted all three flags and then projected with
/// its own defaults would satisfy it completely, while every Protocol setting any of
/// them silently got a map it did not ask for. So this projects the same columns of
/// the same table four times, moving one knob at a time, and compares the bytes.
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
    let path = |name: &str| project.join(format!("build/{name}.parquet"));
    let out = |name: &str| sha256(&path(name));
    for step in [
        "project_default",
        "project_more_neighbours",
        "project_looser_packing",
        "project_cosine",
    ] {
        assert_eq!(
            common::step_outcome(&stdout, step),
            "ran",
            "every projection has to have executed, or the comparison below is between \
             two files that were never written:\n{stdout}"
        );
    }

    for (name, knob) in [
        (
            "more_neighbours",
            "`neighbors: 40` against the script's default of 15",
        ),
        (
            "looser_packing",
            "`min_dist: 0.9` against the script's default of 0.1",
        ),
        (
            "cosine",
            "`metric: cosine` against the script's default of euclidean",
        ),
    ] {
        assert_ne!(
            out("default"),
            out(name),
            "{knob} produced the same bytes — the field is documented, reaches the \
             argv, and does not reach the layout"
        );
        // The coordinates moving is the whole layout claim above; this is the
        // narrower one that `projection_fit_id` itself is built from the knobs and
        // not only from the feature matrix — the SAME columns of the SAME table feed
        // every one of these four fits, so a fit_id that only hashed the matrix would
        // be identical across all four and the id would lie about "default" and
        // "{name}" being different fits.
        assert_ne!(
            fit_id_of(&path("default")),
            fit_id_of(&path(name)),
            "{knob} produced the same fit_id as the default despite different \
             coordinates — the id has to depend on the knob, not just the input rows"
        );
    }
}

/// Which columns exist and what type they are is not decidable from a manifest, so
/// this refusal is the script's rather than a load-time one — and it has to name the
/// column AND the type it found, because "that did not work" sends the author back to
/// the schema to guess which of several columns was the problem.
#[test]
fn a_column_that_is_not_a_number_is_refused_naming_the_column_and_its_type() {
    if !have_uv() {
        eprintln!("skipping a_column_that_is_not_a_number: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();
    std::fs::copy(
        project.join("not_numeric.yaml"),
        project.join("arcform.yaml"),
    )
    .unwrap();

    let (code, stdout, stderr) = common::arc_run_raw(project);
    assert_ne!(code, Some(0), "the run must fail:\n{stdout}\n{stderr}");
    let told = format!("{stdout}{stderr}");
    assert!(
        told.contains("'district'"),
        "the refusal names the column that cannot be projected:\n{told}"
    );
    assert!(
        told.contains("VARCHAR"),
        "the refusal names the type it found, not just that it was wrong:\n{told}"
    );
    assert!(
        !projected(project).exists(),
        "nothing may be written when a projected column is not a number"
    );
}

/// With `uv`'s cache warm, the step needs nothing from the network and consults no
/// credential. There is no model in this Protocol at all, so the only thing that could
/// reach out is the script itself.
///
/// Outbound HTTP is made unavailable two ways, because one is not enough. `UV_OFFLINE`
/// stops `uv` reaching a package registry, and nothing else — it does not constrain
/// what the script itself does. So the proxy variables point every HTTP client in the
/// step at a port nothing listens on: `urllib`, `requests`, `httpx` and
/// `huggingface_hub` all honour them, which is the whole realistic set of ways a
/// Python operator reaches out. A raw socket to an IP address would still get through
/// and nothing here would see it; that is the limit of what this test asserts.
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
