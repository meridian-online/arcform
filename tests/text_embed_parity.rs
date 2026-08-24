//! One embedder, checked against itself from the two ends an analyst can reach it
//! from: a Protocol run, and a SQL session.
//!
//! WHY THIS EXISTS. Until the operator was moved onto the extension, the same
//! capability was implemented twice — Python in the operator, Rust in the extension —
//! and the two disagreed. Measured over a corpus of the shapes below against
//! byte-identical weights: ordinary text agreed to float32 noise, text made only of
//! tokens the vocabulary does not carry differed by 1.8e-01 (one side averaged the
//! tokenizer's unknown-token row, the other dropped it), text mixed with such tokens
//! by 2.2e-02, and text past the 512-token cap by 2.1e-03. Nothing on either side went
//! red, because each was correct on its own terms.
//!
//! WHAT IS ASSERTED, AND WHY IT IS NOT A TAUTOLOGY. Both sides now call the same
//! `embed()`, so the interesting question is no longer which arithmetic each does — it
//! is whether the operator delivers what `embed()` returned, unaltered, to the row it
//! belongs to. Between the call and the analyst's file sit a NULL bridge, a cast to a
//! fixed-width vector, a join on an ordinal, an ORDER BY, and a Parquet round trip.
//! Each of those can lose or move a value while every vector in the file still looks
//! like a plausible embedding. The comparison is exact — no tolerance — because with
//! one implementation there is no rounding left to forgive.
//!
//! WHAT RUNS WHERE. Everything that produces a vector needs the built extension
//! (`ARC_STATICEMBED_EXTENSION`) and `uv`; CI has neither, so those tests return early
//! there. `the_comparison_notices_a_single_float_out_of_place` needs nothing and runs
//! everywhere — it drives the same predicate the comparison uses and fails if that
//! predicate stops distinguishing vectors. A comparison whose predicate has gone blind
//! is green against agreement and green against disagreement, and the two look
//! identical from the outside.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

/// The built embedding extension, or `None`.
fn extension_artifact() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("ARC_STATICEMBED_EXTENSION")?);
    path.is_file().then_some(path)
}

/// A directory holding the three files of the model the extension bundles, or `None`.
/// Only the tests that exercise the model check want it.
fn model_directory() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("ARC_STATICEMBED_MODEL")?);
    path.is_dir().then_some(path)
}

/// The release the model directory above was taken from. Declared rather than
/// discovered: the extension hashes the identity and the revision together with the
/// files, so a directory alone cannot say which release it is.
fn model_release() -> String {
    std::env::var("ARC_STATICEMBED_MODEL_RELEASE").unwrap_or_else(|_| {
        "minishlab/potion-base-8M@bf8b056651a2c21b8d2565580b8569da283cab23".to_string()
    })
}

fn have_uv() -> bool {
    Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sql_lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// SQL that is TRUE when two vector expressions are not the same value.
///
/// The single place the comparison is defined, used by the real comparison and by the
/// self-test below. `IS DISTINCT FROM` over `FLOAT[]` compares element by element and
/// treats a NULL as a value, so a row the operator failed to write is a difference
/// rather than a row that quietly drops out of a `WHERE`.
fn mismatch(got: &str, want: &str) -> String {
    format!("(CAST({got} AS FLOAT[]) IS DISTINCT FROM CAST({want} AS FLOAT[]))")
}

/// A DuckDB connection with the extension loaded. Unsigned, because a locally built
/// artifact is not signed.
fn session(artifact: &Path) -> duckdb::Connection {
    let config = duckdb::Config::default()
        .allow_unsigned_extensions()
        .expect("allow unsigned extensions");
    let conn = duckdb::Connection::open_in_memory_with_flags(config).unwrap();
    conn.execute_batch(&format!("LOAD '{}';", artifact.display()))
        .expect("the artifact loads");
    conn
}

// ── the corpus ───────────────────────────────────────────────────────────────
//
// The cases that diverged when the two implementations were measured against each
// other, plus the ordinary text that did not — a comparison over well-behaved input
// is the version that passes while the interesting cases are wrong.

/// Words the model's vocabulary carries. Used to build texts long enough to cross the
/// 512-token cap. IN-VOCABULARY IS THE POINT: a filler of unknown tokens would be
/// dropped, and the length probes below would compare a text against itself.
const FILLER: &[&str] = &[
    "harbour", "tide", "vessel", "cargo", "market", "revenue", "quarter", "capital",
    "channel", "survey", "anchor", "freight", "margin", "invoice", "auditor", "berth",
    "current", "weather", "signal", "engine", "rudder", "compass", "ledger", "dividend",
    "shipment", "customs", "warehouse", "forecast", "tonnage", "pilot",
];

/// Words appended to probe the cap. Distinct from FILLER so that adding them changes
/// the mean of a short text — the control that proves the probe can see a change at
/// all.
const APPENDED: &[&str] = &[
    "volcano", "orchestra", "penguin", "trombone", "meadow", "lantern", "cinnamon",
    "quarry", "saffron", "trellis", "mackerel", "pumice", "juniper", "cobalt",
];

fn repeat_words(words: &[&str], count: usize) -> String {
    (0..count)
        .map(|i| words[i % words.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

/// `(id, label, text)`. A `None` text is SQL NULL — a case in its own right, and the
/// only one where the two sides legitimately differ before the bridge is applied.
fn corpus() -> Vec<(i32, &'static str, Option<String>)> {
    let over_cap = repeat_words(FILLER, 700);
    let under_cap = repeat_words(FILLER, 30);
    let appended = APPENDED.join(" ");
    vec![
        (1, "ordinary", Some("the harbour wakes as the tide turns and the fishing boats leave the quay".into())),
        (2, "case folded", Some("The HARBOUR Wakes As The TIDE Turns".into())),
        (3, "accents", Some("café naïve façade".into())),
        (4, "accents decomposed", Some("cafe\u{301} nai\u{308}ve fac\u{327}ade".into())),
        (5, "empty", Some(String::new())),
        (6, "whitespace only", Some("  \t \n ".into())),
        (7, "out of vocabulary only, runic", Some("ᚠᚢᚦᚨᚱᚲ".into())),
        (8, "out of vocabulary only, alchemical", Some("🜁🜂🜃🜄".into())),
        (9, "out of vocabulary mixed in", Some("steel ᚠ valve".into())),
        (10, "out of vocabulary mixed in, emoji", Some("the 🜁 tide turns".into())),
        (11, "around 120 tokens", Some(repeat_words(FILLER, 120))),
        (12, "past the cap", Some(over_cap.clone())),
        (13, "past the cap, extended", Some(format!("{over_cap} {appended}"))),
        (14, "under the cap", Some(under_cap.clone())),
        (15, "under the cap, extended", Some(format!("{under_cap} {appended}"))),
        (16, "an apostrophe and a quote", Some("the pilot's log said \"slack water\"".into())),
        (17, "null", None),
    ]
}

/// Write the corpus as a Parquet the operator can read.
fn write_corpus(conn: &duckdb::Connection, dest: &Path) {
    let values = corpus()
        .into_iter()
        .map(|(id, label, text)| {
            let text = match text {
                Some(t) => sql_lit(&t),
                None => "NULL".to_string(),
            };
            format!("({id}, {}, {text})", sql_lit(label))
        })
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_batch(&format!(
        "COPY (SELECT * FROM (VALUES {values}) AS t(id, label, description) ORDER BY id) \
         TO '{}' (FORMAT parquet);",
        dest.display()
    ))
    .expect("write the corpus");
}

/// Stage a one-step Protocol that embeds the corpus, run it, and return the project
/// directory. `model` adds the optional model check.
fn run_protocol(
    artifact: &Path,
    model: Option<(&Path, &str)>,
) -> (tempfile::TempDir, Option<i32>, String) {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    let conn = duckdb::Connection::open_in_memory().unwrap();
    write_corpus(&conn, &project.join("corpus.parquet"));
    std::fs::copy(artifact, project.join("staticembed.duckdb_extension")).unwrap();

    let model_lines = match model {
        Some((dir, release)) => format!(
            "      model: {}\n      model_release: {release}\n",
            dir.display()
        ),
        None => String::new(),
    };
    std::fs::write(
        project.join("arcform.yaml"),
        format!(
            "name: text_embed_parity\n\
             engine: duckdb\n\
             db: build/parity.duckdb\n\
             \n\
             steps:\n\
             \x20 - name: embed\n\
             \x20   op: text_embed@1\n\
             \x20   with:\n\
             \x20     input: corpus.parquet\n\
             \x20     text_column: description\n\
             \x20     extension: staticembed.duckdb_extension\n\
             \x20     out: build/embedded.parquet\n\
             {model_lines}"
        ),
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .arg("run")
        .output()
        .expect("spawn arc run");
    let told = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (tmp, out.status.code(), told)
}

// ── AC2 and AC3 ──────────────────────────────────────────────────────────────

/// THE CARD'S WHOLE CLAIM. Every vector in the Protocol's output file is the value a
/// SQL session returns for the same text, exactly — including the cases where the two
/// implementations used to disagree, and including the NULL the operator bridges.
#[test]
fn a_protocol_run_and_a_sql_session_return_the_same_vector_for_every_case() {
    let Some(artifact) = extension_artifact() else {
        eprintln!("skipping the parity comparison: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    if !have_uv() {
        eprintln!("skipping the parity comparison: no `uv` on PATH");
        return;
    }
    let (tmp, code, told) = run_protocol(&artifact, None);
    assert_eq!(code, Some(0), "the embedding step must succeed:\n{told}");
    let project = tmp.path();

    let conn = session(&artifact);
    // The Protocol's file on one side; a plain SQL session over the same corpus on the
    // other. `coalesce` is the bridge the operator applies and the extension
    // documents — an analyst reaching for a vector column writes the same thing.
    let sql = format!(
        "SELECT c.id, c.label, {mismatch} \
         FROM read_parquet('{corpus}') c \
         JOIN read_parquet('{out}') o USING (id) \
         ORDER BY c.id",
        mismatch = mismatch("o.embedding", "embed(coalesce(c.description, ''))"),
        corpus = project.join("corpus.parquet").display(),
        out = project.join("build/embedded.parquet").display(),
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows: Vec<(i32, String, bool)> = stmt
        .query_map([], |r| {
            Ok((r.get::<_, i32>(0)?, r.get::<_, String>(1)?, r.get::<_, bool>(2)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(
        rows.len(),
        corpus().len(),
        "every case has to reach the comparison — a row that fell out of the join is \
         a case that was not checked rather than a case that passed"
    );
    let differing: Vec<&(i32, String, bool)> = rows.iter().filter(|(_, _, bad)| *bad).collect();
    assert!(
        differing.is_empty(),
        "a Protocol run and a SQL session disagree on {} of {} cases: {}",
        differing.len(),
        rows.len(),
        differing
            .iter()
            .map(|(id, label, _)| format!("{id} ({label})"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// AC3's cases are only worth comparing if they are the shapes they claim to be. This
/// asserts the corpus still contains what it was built to contain — text that
/// tokenises to nothing, text past the cap, and ordinary text — so that a future edit
/// to the word lists cannot quietly turn the comparison above into a comparison over
/// seventeen ordinary sentences.
#[test]
fn the_corpus_still_contains_the_shapes_it_was_built_from() {
    let Some(artifact) = extension_artifact() else {
        eprintln!("skipping the corpus shape check: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    let conn = session(&artifact);
    let by_label = |label: &str| -> String {
        corpus()
            .into_iter()
            .find(|(_, l, _)| *l == label)
            .and_then(|(_, _, t)| t)
            .unwrap_or_else(|| panic!("no case labelled {label}"))
    };
    let is_zero = |text: &str| -> bool {
        conn.query_row(
            &format!(
                "SELECT list_sum(list_transform(embed({}), x -> abs(x))) = 0",
                sql_lit(text)
            ),
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap()
    };
    let same = |a: &str, b: &str| -> bool {
        conn.query_row(
            &format!(
                "SELECT NOT {}",
                mismatch(
                    &format!("embed({})", sql_lit(a)),
                    &format!("embed({})", sql_lit(b))
                )
            ),
            [],
            |r| r.get::<_, bool>(0),
        )
        .unwrap()
    };

    for label in [
        "empty",
        "whitespace only",
        "out of vocabulary only, runic",
        "out of vocabulary only, alchemical",
    ] {
        assert!(
            is_zero(&by_label(label)),
            "`{label}` has to carry no signal, or it is not the case it is named for"
        );
    }
    for label in ["ordinary", "out of vocabulary mixed in", "past the cap"] {
        assert!(
            !is_zero(&by_label(label)),
            "`{label}` has to carry signal, or the comparison is comparing two zero \
             vectors and would pass on any implementation"
        );
    }

    // THE CAP, probed so that it can fail. Appending the same words to a text already
    // past the cap must change nothing, and appending them to a short text must change
    // the vector. The second half is the control: without it, a probe whose appended
    // words were all outside the vocabulary — or whose two texts had the same token
    // multiset — would report "truncated" against an implementation that truncates
    // nothing.
    assert!(
        same(&by_label("past the cap"), &by_label("past the cap, extended")),
        "text past the cap is embedded from its opening, so appending to it must not \
         move the vector"
    );
    assert!(
        !same(&by_label("under the cap"), &by_label("under the cap, extended")),
        "appending those same words to a SHORT text must move the vector — otherwise \
         the check above passes on an implementation that truncates nothing"
    );
}

/// THE CHECK ON THE CHECK, and the only test in this file CI executes. The comparison
/// above is worth exactly as much as its predicate: one that had lost the ability to
/// tell two vectors apart would report agreement whether or not there was any.
#[test]
fn the_comparison_notices_a_single_float_out_of_place() {
    let conn = duckdb::Connection::open_in_memory().unwrap();
    let ask = |got: &str, want: &str| -> bool {
        conn.query_row(&format!("SELECT {}", mismatch(got, want)), [], |r| {
            r.get::<_, bool>(0)
        })
        .unwrap()
    };

    assert!(
        !ask("[1.0, 2.0, 3.0]", "[1.0, 2.0, 3.0]"),
        "identical vectors are not a difference, or every comparison fails and the \
         suite says nothing about agreement"
    );
    assert!(
        ask("[1.0, 2.0, 3.0]", "[1.0, 2.5, 3.0]"),
        "a changed element in the middle is a difference"
    );
    assert!(
        ask("[1.0, 2.0, 3.0]", "[1.0, 2.0, 4.0]"),
        "a changed element at the END is a difference — a predicate that compared only \
         the head would pass the case above and fail here"
    );
    assert!(
        ask("[1.0, 2.0, 3.0]", "[1.0, 2.0]"),
        "a vector of the wrong width is a difference, not a prefix match"
    );
    assert!(
        ask("[1.0, 2.0, 3.0]", "[1.0, 2.0, 3.0000002]"),
        "the comparison is EXACT: with one implementation there is no rounding to \
         forgive, so a tolerance quietly reintroduced here has to redden"
    );
    assert!(
        ask("NULL", "[1.0, 2.0, 3.0]"),
        "a missing vector is a difference — a row the operator failed to write must \
         not drop silently out of the comparison"
    );
}

// ── AC4 ──────────────────────────────────────────────────────────────────────

/// A Protocol that declares a model the extension does not carry is stopped, and told
/// both addresses — the one its own files produce and the one the extension reports.
#[test]
fn a_model_the_extension_does_not_carry_is_refused_naming_both() {
    let Some(artifact) = extension_artifact() else {
        eprintln!("skipping the model mismatch check: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    if !have_uv() {
        eprintln!("skipping the model mismatch check: no `uv` on PATH");
        return;
    }
    let held = tempfile::tempdir().unwrap();
    let model = held.path().join("some-other-model");
    std::fs::create_dir_all(&model).unwrap();
    for (part, bytes) in [
        ("tokenizer.json", &b"{\"not\": \"the tokenizer\"}"[..]),
        ("model.safetensors", &b"not the weights"[..]),
        ("config.json", &b"{\"normalize\": true}"[..]),
    ] {
        std::fs::write(model.join(part), bytes).unwrap();
    }

    let (_tmp, code, told) = run_protocol(&artifact, Some((&model, "someone/other-model@0123456789abcdef0123456789abcdef01234567")));
    assert_ne!(code, Some(0), "a model mismatch must stop the run:\n{told}");
    assert!(
        told.contains("someone/other-model"),
        "the refusal names what the Protocol declared:\n{told}"
    );
    assert!(
        told.contains("the extension reports"),
        "the refusal names what the extension carries, so the reader can see which \
         of the two to change:\n{told}"
    );
}

/// The other half, and the reason the test above is not a check that refuses
/// everything: the model the extension DOES carry is accepted, and the run produces
/// vectors.
#[test]
fn the_model_the_extension_carries_is_accepted() {
    let (Some(artifact), Some(model)) = (extension_artifact(), model_directory()) else {
        eprintln!("skipping the model match check: needs ARC_STATICEMBED_EXTENSION and ARC_STATICEMBED_MODEL");
        return;
    };
    if !have_uv() {
        eprintln!("skipping the model match check: no `uv` on PATH");
        return;
    }
    let release = model_release();
    let (tmp, code, told) = run_protocol(&artifact, Some((&model, &release)));
    assert_eq!(
        code,
        Some(0),
        "the declared model IS the one the extension carries, so the run must \
         proceed:\n{told}"
    );
    assert!(
        tmp.path().join("build/embedded.parquet").is_file(),
        "an accepted model check leaves the vectors written:\n{told}"
    );
}

/// The address covers THREE files, and this is the test that pins the third. Copy the
/// model the extension carries, change one byte of `config.json` — the file a
/// two-file check would never look at — and the run has to stop. Without this, an
/// address computed over the tokenizer and the weights alone passes every other test
/// in this file.
#[test]
fn changing_one_byte_of_the_third_file_is_a_different_model() {
    let (Some(artifact), Some(model)) = (extension_artifact(), model_directory()) else {
        eprintln!("skipping the third-file check: needs ARC_STATICEMBED_EXTENSION and ARC_STATICEMBED_MODEL");
        return;
    };
    if !have_uv() {
        eprintln!("skipping the third-file check: no `uv` on PATH");
        return;
    }
    let held = tempfile::tempdir().unwrap();
    let altered = held.path().join("potion-altered");
    std::fs::create_dir_all(&altered).unwrap();
    for part in ["tokenizer.json", "model.safetensors", "config.json"] {
        std::fs::copy(model.join(part), altered.join(part)).unwrap();
    }
    // One byte, in the file neither the tokenizer nor the weights: a trailing space is
    // JSON-insignificant and address-significant, which is exactly the point.
    let config = altered.join("config.json");
    let mut bytes = std::fs::read(&config).unwrap();
    bytes.push(b' ');
    std::fs::write(&config, bytes).unwrap();

    let (_tmp, code, told) = run_protocol(&artifact, Some((&altered, &model_release())));
    assert_ne!(
        code,
        Some(0),
        "a model whose config.json differs by one byte is a different model:\n{told}"
    );
    assert!(
        told.contains("the extension reports"),
        "the refusal is the address mismatch, not something incidental:\n{told}"
    );
}
