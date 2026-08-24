//! End-to-end for the `text_embed` operator and the chain it makes possible: a real
//! `arc run` over the fixture Protocol in `tests/fixtures/text_embed/`, which turns a
//! text column into a vector column and then — as a SEPARATE step — into the two
//! coordinates a map needs.
//!
//! THE SPLIT IS WHAT IS UNDER TEST. `build/corpus_embedded.parquet` is asserted on its
//! own: an analyst who wanted vectors for similarity, clustering, deduplication or
//! classifier features has them there, without a 2-D map they did not ask for. The
//! projection that follows is handed a numeric column and is not told an embedding
//! produced it. One merged step made each half reachable only through the other.
//!
//! WHAT NEEDS WHAT, AND WHAT CI ACTUALLY RUNS. The embedding needs the loadable
//! embedding extension, which is tens of megabytes and is not committed; the
//! projection needs `uv`. CI has neither, so every test below that produces vectors
//! returns early, and the one CI executes is the refusal when the extension asset is
//! missing — decided in Rust before anything is spawned. `ARC_STATICEMBED_EXTENSION`
//! names a built artifact and turns the rest on.
//!
//! A TEST THAT RETURNS EARLY REPORTS AS `ok`, which is indistinguishable from a test
//! that ran. That is a property of the harness and not something this file can fix,
//! so each early return says on stderr which input it wanted, and the parity suite in
//! `text_embed_parity.rs` carries a check of its own comparison logic that runs
//! everywhere.
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/text_embed")
}

/// Is `uv` on PATH? The projection step needs it.
fn have_uv() -> bool {
    Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The built embedding extension, or `None`. Every vector below comes out of this
/// artifact, so without it there is nothing to assert.
pub fn extension_artifact() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("ARC_STATICEMBED_EXTENSION")?);
    path.is_file().then_some(path)
}

/// The vector width the extension itself reports, asked of the artifact rather than
/// written down here. A constant would have to be edited in lockstep with a model
/// swap, and would not fail if it were not.
fn extension_dim(artifact: &Path) -> i64 {
    let config = duckdb::Config::default()
        .allow_unsigned_extensions()
        .expect("allow unsigned extensions");
    let conn = duckdb::Connection::open_in_memory_with_flags(config).unwrap();
    conn.execute_batch(&format!("LOAD '{}';", artifact.display()))
        .expect("the artifact loads");
    conn.query_row("SELECT len(embed('width probe'))", [], |r| r.get(0))
        .expect("the extension answers")
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
/// The extension is copied in only when there is one; the Protocol names it either
/// way, which is what makes the missing-asset refusal testable.
fn staged_protocol() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&fixture_dir(), tmp.path());
    if let Some(artifact) = extension_artifact() {
        std::fs::copy(artifact, tmp.path().join("staticembed.duckdb_extension")).unwrap();
    }
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

fn embedded(project: &Path) -> PathBuf {
    project.join("build/corpus_embedded.parquet")
}

fn projected(project: &Path) -> PathBuf {
    project.join("build/corpus_projected.parquet")
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    format!("{:x}", Sha256::digest(&bytes))
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

/// The half of this file that CI runs. The extension is a declared input, so a
/// Protocol pointed at one nothing has put there stops — naming the file it looked
/// for — instead of quietly fetching a build of its own choosing or installing one
/// from a registry.
#[test]
fn an_extension_that_was_never_staged_stops_the_run_naming_the_file() {
    let tmp = staged_protocol();
    let project = tmp.path();
    let artifact = project.join("staticembed.duckdb_extension");
    if artifact.exists() {
        std::fs::remove_file(&artifact).unwrap();
    }

    let (code, stdout, stderr) = common::arc_run_raw(project);
    assert_ne!(code, Some(0), "the run must fail:\n{stdout}\n{stderr}");
    let told = format!("{stdout}{stderr}");
    assert!(
        told.contains("the extension asset staticembed.duckdb_extension is not on disk"),
        "the refusal names the declared extension asset:\n{told}"
    );
    assert!(
        told.contains("does not download one"),
        "the refusal says putting it there is the Protocol's job:\n{told}"
    );
    assert!(
        told.contains("does not install one from a registry"),
        "an extension has a second way to arrive by itself, and the refusal closes \
         that one too:\n{told}"
    );
    assert!(
        !embedded(project).exists(),
        "nothing may be written when the extension is missing"
    );
    // The extension is in the graph, not beside it: `arc run` prints it as a node the
    // embedding step feeds from.
    let graph = common::strip_ansi(&stdout);
    assert!(
        graph.contains("staticembed.duckdb_extension"),
        "the extension is an asset-graph node, not an invisible dependency:\n{graph}"
    );
}

/// THE HEADLINE OF THE SPLIT. The embedding is a finished artifact of its own: every
/// input column plus one vector column, and NO coordinates. An analyst who wanted
/// vectors — for similarity, clustering, deduplication, or as classifier features —
/// stops here. Before the split there was no such file: the only way to reach an
/// embedding was to also compute a 2-D map.
#[test]
fn an_embedding_is_a_finished_artifact_with_no_map_attached() {
    let Some(artifact) = extension_artifact() else {
        eprintln!("skipping an_embedding_is_a_finished_artifact: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    if !have_uv() {
        eprintln!("skipping an_embedding_is_a_finished_artifact: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();
    let stdout = common::arc_run(project);
    assert_eq!(
        common::step_outcome(&stdout, "embed"),
        "ran",
        "the embedding has to have executed, or what is asserted below is whatever \
         happened to be on disk:\n{stdout}"
    );

    let out = embedded(project);
    assert_eq!(
        columns_of(&out),
        vec![
            ("id".to_string(), "INTEGER".to_string()),
            ("title".to_string(), "VARCHAR".to_string()),
            ("description".to_string(), "VARCHAR".to_string()),
            ("embedding".to_string(), "FLOAT[]".to_string()),
        ],
        "every input column, in order, then the vector column — and nothing else. A \
         coordinate column here would mean the embedding still drags a map behind it"
    );

    // The vectors are real: the extension's own width, and L2-normalised, so a cosine
    // metric downstream reads them as directions. The width is not written down here —
    // it is read from the extension, because a constant would have to be updated in
    // lockstep with a model swap and would not fail if it were not.
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT len(embedding), \
             sqrt(list_sum(list_transform(embedding, x -> CAST(x AS DOUBLE) * \
             CAST(x AS DOUBLE)))) \
             FROM read_parquet('{}') ORDER BY id",
            out.display()
        ))
        .unwrap();
    let vectors: Vec<(i64, f64)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(vectors.len(), 48, "one vector per corpus row");
    let dim = extension_dim(&artifact);
    for (width, norm) in &vectors {
        assert_eq!(
            *width, dim,
            "the vector width is the extension's, and the extension says {dim}"
        );
        assert!(
            (norm - 1.0).abs() < 1e-5,
            "an embedding is L2-normalised; this one has norm {norm}"
        );
    }
}

/// The proof that the vectors carry the text. The projection is handed the vector
/// column and nothing else — it never sees `description` — so if the map separates
/// the corpus's two subjects, the separation came through the embedding.
#[test]
fn the_route_from_a_text_column_to_coordinates_still_works() {
    let Some(_) = extension_artifact() else {
        eprintln!("skipping the_route_from_a_text_column_to_coordinates: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    if !have_uv() {
        eprintln!("skipping the_route_from_a_text_column_to_coordinates: no `uv` on PATH");
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
            ("embedding".to_string(), "FLOAT[]".to_string()),
            ("projection_x".to_string(), "DOUBLE".to_string()),
            ("projection_y".to_string(), "DOUBLE".to_string()),
            ("projection_fit_id".to_string(), "VARCHAR".to_string()),
        ],
        "the coordinates arrive beside the vectors rather than instead of them — the \
         analyst who wanted both keeps both"
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
    // subject. The projection saw only `embedding`, so a map that separates them is a
    // map whose structure came through the text.
    let centre = |group: &[&(i32, f64, f64)]| {
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
        let (own, other) = if *id <= 24 {
            (harbour_centre, markets_centre)
        } else {
            (markets_centre, harbour_centre)
        };
        assert!(
            distance((*x, *y), own) < distance((*x, *y), other),
            "row {id} at ({x:.3}, {y:.3}) is nearer the other subject's centre — the \
             coordinates do not carry the text"
        );
    }
}

/// With the extension on disk and `uv`'s cache warm, neither step needs anything from
/// the network and neither consults a credential.
///
/// Outbound HTTP is made unavailable two ways, because one is not enough. `UV_OFFLINE`
/// stops `uv` reaching a package registry, and nothing else — it does not constrain
/// what the scripts themselves do. So the proxy variables point every HTTP client in
/// the steps at a port nothing listens on: `urllib`, `requests`, `httpx` and
/// `huggingface_hub` all honour them, which is the whole realistic set of ways a
/// Python operator reaches out. A raw socket to an IP address would still get through
/// and nothing here would see it; that is the limit of what this test asserts.
///
/// Three plausible API-key variables are set to values that would break anything that
/// used them, and the run has to land on the same bytes as the one that had a network.
#[test]
fn the_steps_complete_with_the_network_disabled_and_no_credentials() {
    let Some(_) = extension_artifact() else {
        eprintln!(
            "skipping the_steps_complete_with_the_network_disabled: no ARC_STATICEMBED_EXTENSION"
        );
        return;
    };
    if !have_uv() {
        eprintln!("skipping the_steps_complete_with_the_network_disabled: no `uv` on PATH");
        return;
    }
    let tmp = staged_protocol();
    let project = tmp.path();

    // The first run is what warms `uv`'s cache on this machine; the claim is about the
    // second. A machine that has run this before pays nothing for it.
    common::arc_run(project);
    let online = (sha256(&embedded(project)), sha256(&projected(project)));

    let build = project.join("build");
    std::fs::remove_dir_all(&build).unwrap_or_else(|e| panic!("clear {}: {e}", build.display()));
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
        "both steps must complete with the network disabled:\n{stdout}\n{stderr}"
    );
    for step in ["embed", "project"] {
        assert_eq!(
            common::step_outcome(&stdout, step),
            "ran",
            "the offline run has to have re-executed `{step}` — a skipped step would \
             compare a file against itself and prove nothing:\n{stdout}"
        );
    }
    assert_eq!(
        online,
        (sha256(&embedded(project)), sha256(&projected(project))),
        "an offline run must produce the same vectors and the same map as an online one"
    );
}
