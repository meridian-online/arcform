//! One embedder, checked against itself from the two ends an analyst can reach it
//! from: a Protocol run, and a SQL session.
//!
//! WHY THIS EXISTS. Until the operator was moved onto the extension, the same
//! capability was implemented twice — Python in the operator, Rust in the extension —
//! and the two disagreed. Measured over the corpus below against byte-identical
//! weights: ordinary text agreed to float32 summation order, and three shapes did not.
//! Text made only of tokens the vocabulary does not carry (one side averaged the
//! tokenizer's unknown-token row into a unit-norm vector, the other dropped those ids
//! and returned a zero vector); text with such tokens mixed into real words, for the
//! same reason; and long text, which the extension truncates and the Python path did
//! not. Nothing on either side went red, because each was correct on its own terms.
//! No magnitude is quoted: the Python path is deleted, so nothing regenerates a
//! difference against it and no figure written here can be made to redden.
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

use sha2::{Digest, Sha256};
use std::fmt::Write as _;
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

/// TRUE when the extension returns the same vector for both texts. The single place
/// two texts are compared through `embed()`, built on the same `mismatch` predicate
/// the Protocol-versus-SQL comparison uses and that
/// `the_comparison_notices_a_single_float_out_of_place` exercises.
fn same_vector(conn: &duckdb::Connection, a: &str, b: &str) -> bool {
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
/// truncation boundary. IN-VOCABULARY IS THE POINT: a filler of unknown tokens would
/// be dropped, and the length probes below would compare a text against itself.
///
/// ASCII, and it has to stay ASCII: the character cut counts Unicode scalars, so the
/// byte slices the boundary probe takes are only the right characters while every word
/// here is one byte per character. `the_truncation_boundary_is_two_cuts` asserts it.
const FILLER: &[&str] = &[
    "harbour",
    "tide",
    "vessel",
    "cargo",
    "market",
    "revenue",
    "quarter",
    "capital",
    "channel",
    "survey",
    "anchor",
    "freight",
    "margin",
    "invoice",
    "auditor",
    "berth",
    "current",
    "weather",
    "signal",
    "engine",
    "rudder",
    "compass",
    "ledger",
    "dividend",
    "shipment",
    "customs",
    "warehouse",
    "forecast",
    "tonnage",
    "pilot",
];

/// Words appended to probe truncation. Distinct from FILLER so that adding them
/// changes the mean of a short text — the control that proves the probe can see a
/// change at all.
const APPENDED: &[&str] = &[
    "volcano",
    "orchestra",
    "penguin",
    "trombone",
    "meadow",
    "lantern",
    "cinnamon",
    "quarry",
    "saffron",
    "trellis",
    "mackerel",
    "pumice",
    "juniper",
    "cobalt",
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
        (
            1,
            "ordinary",
            Some("the harbour wakes as the tide turns and the fishing boats leave the quay".into()),
        ),
        (
            2,
            "case folded",
            Some("The HARBOUR Wakes As The TIDE Turns".into()),
        ),
        (3, "accents", Some("café naïve façade".into())),
        (
            4,
            "accents decomposed",
            Some("cafe\u{301} nai\u{308}ve fac\u{327}ade".into()),
        ),
        (5, "empty", Some(String::new())),
        (6, "whitespace only", Some("  \t \n ".into())),
        (7, "out of vocabulary only, runic", Some("ᚠᚢᚦᚨᚱᚲ".into())),
        (8, "out of vocabulary only, alchemical", Some("🜁🜂🜃🜄".into())),
        (
            9,
            "out of vocabulary mixed in",
            Some("steel ᚠ valve".into()),
        ),
        (
            10,
            "out of vocabulary mixed in, emoji",
            Some("the 🜁 tide turns".into()),
        ),
        (11, "around 120 tokens", Some(repeat_words(FILLER, 120))),
        (12, "past the cap", Some(over_cap.clone())),
        (
            13,
            "past the cap, extended",
            Some(format!("{over_cap} {appended}")),
        ),
        (14, "under the cap", Some(under_cap.clone())),
        (
            15,
            "under the cap, extended",
            Some(format!("{under_cap} {appended}")),
        ),
        (
            16,
            "an apostrophe and a quote",
            Some("the pilot's log said \"slack water\"".into()),
        ),
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
            Ok((
                r.get::<_, i32>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, bool>(2)?,
            ))
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

/// THE OTHER HALF OF WHAT THIS OPERATOR USED TO GET WRONG. Text made only of tokens
/// outside the vocabulary was handed back as a unit-norm average of the tokenizer's
/// unknown-token row — a vector for text nothing had been understood of — and was NOT
/// counted, so nothing said it had happened. Now it embeds as a zero vector like the
/// empty string, and it is counted with them.
///
/// RUN DIRECTLY, not through `arc run`, because that is where the count is observable:
/// the engine captures a successful step's output and does not print it, so the line
/// this asserts exists but never reaches a person running a Protocol. That is a gap in
/// the engine rather than in the operator, and closing it is not this file's to do.
#[test]
fn the_rows_that_carry_no_signal_are_counted_on_stderr() {
    let Some(artifact) = extension_artifact() else {
        eprintln!("skipping the zero-vector count: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    if !have_uv() {
        eprintln!("skipping the zero-vector count: no `uv` on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path();
    let corpus_path = project.join("corpus.parquet");
    write_corpus(&duckdb::Connection::open_in_memory().unwrap(), &corpus_path);

    // The expected number is derived from the corpus THROUGH THE EXTENSION rather than
    // written down here, so adding a case to the corpus cannot leave this asserting a
    // stale figure.
    let conn = session(&artifact);
    let expected: i64 = conn
        .query_row(
            &format!(
                "SELECT count(*) FROM read_parquet('{}') WHERE \
                 list_sum(list_transform(embed(coalesce(description, '')), x -> abs(x))) = 0",
                corpus_path.display()
            ),
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert!(
        expected > 0,
        "the corpus has to contain text that carries no signal, or this asserts that \
         zero rows were reported and passes on an operator that reports nothing"
    );

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("operators/text_embed/text_embed.py");
    let out = Command::new("uv")
        .current_dir(project)
        .args(["run", &script.display().to_string()])
        .args(["--input", &corpus_path.display().to_string()])
        .args(["--text-column", "description"])
        .args(["--extension", &artifact.display().to_string()])
        .args([
            "--out",
            &project.join("embedded.parquet").display().to_string(),
        ])
        .output()
        .expect("spawn uv run");
    let told = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the script must succeed:\n{told}");
    assert!(
        told.contains(&format!(
            "[text_embed] {expected} of {} rows tokenised to nothing",
            corpus().len()
        )),
        "all {expected} rows that embed as a zero vector have to be reported:\n{told}"
    );
}

/// AC3's cases are only worth comparing if they are the shapes they claim to be. This
/// asserts the corpus still contains what it was built to contain — text that
/// tokenises to nothing, text past the truncation boundary, and ordinary text — so
/// that a future edit to the word lists cannot quietly turn the comparison above into
/// a comparison over ordinary sentences.
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
    let same = |a: &str, b: &str| same_vector(&conn, a, b);

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

    // TRUNCATION HAPPENS, probed so that it can fail. Appending the same words to a
    // text already past the boundary must change nothing, and appending them to a
    // short text must change the vector. The second half is the control: without it, a
    // probe whose appended words were all outside the vocabulary — or whose two texts
    // had the same token multiset — would report "truncated" against an implementation
    // that truncates nothing.
    //
    // WHERE the boundary is, as opposed to that there is one, is
    // `the_truncation_boundary_is_two_cuts` below. This test only needs the corpus to
    // still straddle it.
    assert!(
        same(
            &by_label("past the cap"),
            &by_label("past the cap, extended")
        ),
        "text past the boundary is embedded from its opening, so appending to it must \
         not move the vector"
    );
    assert!(
        !same(
            &by_label("under the cap"),
            &by_label("under the cap, extended")
        ),
        "appending those same words to a SHORT text must move the vector — otherwise \
         the check above passes on an implementation that truncates nothing"
    );
}

/// The raw text is cut to this many CHARACTERS before it is tokenised. model2vec
/// computes it as `TOKEN_CUT × the model's median token length`, which is six for the
/// bundled `potion-base-8M`, so it MOVES WITH THE MODEL the extension carries. A red
/// here after the extension is rebuilt on a different model is a number to update in
/// this file and in the operator README, not a fault.
const CHAR_CUT: usize = 3072;

/// …and the token ids are then cut to this many.
const TOKEN_CUT: usize = 512;

/// WHERE the truncation boundary is, pinned to the character and to the token.
///
/// The extension truncates TWICE: model2vec cuts the raw string to `CHAR_CUT`
/// characters BEFORE tokenising it, then cuts the token ids to `TOKEN_CUT`. **The
/// boundary is whichever comes first**, and for ordinary English it is the character
/// cut, because English runs shorter than the six characters a token that cut assumes.
/// The operator's README states both numbers and this is what makes them reddenable —
/// before this test, "text past 512 tokens is truncated" was documented, wrong, and
/// unfalsifiable, because a text can be truncated while under 512 tokens.
///
/// THE PAIRS ARE THE POINT, not either half. `embed(t) == embed(t[..CUT])` on its own
/// passes for any boundary at or below CUT; `embed(t) != embed(t[..CUT - 1])` on its
/// own passes for any boundary above it. Only together do they hold at exactly CUT, so
/// a constant one character or one token out of place reddens this test.
#[test]
fn the_truncation_boundary_is_two_cuts() {
    let Some(artifact) = extension_artifact() else {
        eprintln!("skipping the truncation boundary: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    let conn = session(&artifact);
    let same = |a: &str, b: &str| same_vector(&conn, a, b);

    // ── the CHARACTER cut ────────────────────────────────────────────────────
    //
    // The cut counts Unicode scalars, so a byte slice is the right prefix only while
    // the text is ASCII. Asserted rather than assumed: a non-ASCII word added to
    // FILLER would silently move every index below.
    let long = repeat_words(FILLER, 700);
    assert!(
        long.is_ascii(),
        "the boundary probe slices by BYTE and the extension cuts by CHARACTER, so \
         the filler has to stay one byte per character"
    );
    assert!(
        long.len() > CHAR_CUT,
        "the probe text is {} characters and has to exceed the {CHAR_CUT}-character \
         cut, or nothing below is measuring truncation at all",
        long.len()
    );
    assert!(
        same(&long, &long[..CHAR_CUT]),
        "a text longer than {CHAR_CUT} characters has to embed to the same vector as \
         its first {CHAR_CUT} — if it does not, the character cut is somewhere BELOW \
         {CHAR_CUT}"
    );
    assert!(
        !same(&long, &long[..CHAR_CUT - 1]),
        "…and to a DIFFERENT vector from its first {} — if it does not, the character \
         cut is somewhere ABOVE {CHAR_CUT} and the assertion before this one passed \
         only because both texts were being cut to the same shorter prefix",
        CHAR_CUT - 1
    );

    // ── the TOKEN cut ────────────────────────────────────────────────────────
    //
    // `tide` and `salt` are each exactly one token in this model's vocabulary, so a
    // text of N of them is N tokens. That is load-bearing and self-checking: if `tide`
    // stopped being one token, the text below would already be past the cut and the
    // `!=` assertion would fail rather than pass quietly.
    let at_cut = repeat_words(&["tide"], TOKEN_CUT);
    let below_cut = repeat_words(&["tide"], TOKEN_CUT - 1);
    let at_cut_extended = format!("{at_cut} salt");
    let below_cut_extended = format!("{below_cut} salt");
    for (label, text) in [
        ("at the cut", &at_cut),
        ("at the cut, extended", &at_cut_extended),
        ("below the cut", &below_cut),
        ("below the cut, extended", &below_cut_extended),
    ] {
        assert!(
            text.len() < CHAR_CUT,
            "`{label}` is {} characters, which is past the {CHAR_CUT}-character cut — \
             every text in this half has to stay under it, or the character cut is \
             what these assertions measure and the token cut is untested",
            text.len()
        );
    }
    assert!(
        same(&at_cut, &at_cut_extended),
        "a text of {TOKEN_CUT} one-token words must not move when a {}th token is \
         appended — if it moves, the token cut is above {TOKEN_CUT}",
        TOKEN_CUT + 1
    );
    assert!(
        !same(&below_cut, &below_cut_extended),
        "…but a text of {} must move when the {TOKEN_CUT}th is appended — if it does \
         not, the token cut is below {TOKEN_CUT} and the assertion before this one \
         passed on a cut that had already bitten",
        TOKEN_CUT - 1
    );

    // ── and the character cut is the one that bites, for ordinary English ────
    //
    // This is the half the old documentation got wrong. 450 words of the same filler
    // are past the character cut while what survives them is well under the token cut,
    // so a reader told "under 512 tokens and your text is whole" is wrong at a length
    // a real description reaches.
    let english = repeat_words(FILLER, 450);
    assert!(
        english.len() > CHAR_CUT,
        "the ordinary-English case is {} characters and has to exceed {CHAR_CUT}",
        english.len()
    );
    assert!(
        same(&english, &english[..CHAR_CUT]),
        "{} characters of ordinary English are truncated by the character cut",
        english.len()
    );
    // What survives is under the token cut: a prefix short enough to escape the
    // character cut still takes a new word into its vector, which a text sitting at
    // the token cut could not. Without this the paragraph above would be consistent
    // with the token cut having done the truncating.
    let head = &english[..CHAR_CUT - 12];
    let head_extended = format!("{head} volcano");
    assert!(
        head_extended.len() < CHAR_CUT,
        "the control has to stay under the character cut to say anything about tokens"
    );
    assert!(
        !same(head, &head_extended),
        "{} characters of this filler are still under the {TOKEN_CUT}-token cut — a \
         new word has to reach the vector. If it does not, the two cuts coincide here \
         and the claim that a text can be truncated while under {TOKEN_CUT} tokens is \
         not what this file is showing",
        head.len()
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

    let (_tmp, code, told) = run_protocol(
        &artifact,
        Some((
            &model,
            "someone/other-model@0123456789abcdef0123456789abcdef01234567",
        )),
    );
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
    // The WHOLE published address, not the phrase around it. A check weakened by
    // shortening the published side — comparing and then reporting a prefix of it —
    // would still print "the extension reports" and would fail here.
    let published = published_key(&session(&artifact));
    assert!(
        told.contains(&published),
        "the refusal quotes every character of the address the extension published \
         ({published}), because that is how many the operator compared:\n{told}"
    );
}

/// The other half, and the reason the test above is not a check that refuses
/// everything: the model the extension DOES carry is accepted, and the run produces
/// vectors.
#[test]
fn the_model_the_extension_carries_is_accepted() {
    let (Some(artifact), Some(model)) = (extension_artifact(), model_directory()) else {
        eprintln!(
            "skipping the model match check: needs ARC_STATICEMBED_EXTENSION and ARC_STATICEMBED_MODEL"
        );
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

/// The address covers THREE files, and this is the test that pins the third.
///
/// TWO RUNS, and the first one is why this test can fail for its own reason. Refusing
/// the altered directory proves nothing on its own — an address computed over the
/// tokenizer and the weights alone would ALSO refuse it, and would refuse the
/// unaltered directory too. Measured 2026-08-25: dropping `config.json` from the
/// digest left this test green when it asserted only the refusal. So the unaltered
/// copy has to be ACCEPTED first, in the same test, and only then does the refusal of
/// a copy differing by one byte of the third file mean the third file is hashed.
#[test]
fn changing_one_byte_of_the_third_file_is_a_different_model() {
    let (Some(artifact), Some(model)) = (extension_artifact(), model_directory()) else {
        eprintln!(
            "skipping the third-file check: needs ARC_STATICEMBED_EXTENSION and ARC_STATICEMBED_MODEL"
        );
        return;
    };
    if !have_uv() {
        eprintln!("skipping the third-file check: no `uv` on PATH");
        return;
    }
    let held = tempfile::tempdir().unwrap();
    let copied = held.path().join("potion-copied");
    std::fs::create_dir_all(&copied).unwrap();
    for part in ["tokenizer.json", "model.safetensors", "config.json"] {
        std::fs::copy(model.join(part), copied.join(part)).unwrap();
    }
    let release = model_release();
    let (_intact, intact_code, intact_told) = run_protocol(&artifact, Some((&copied, &release)));
    assert_eq!(
        intact_code,
        Some(0),
        "the copy is byte-for-byte the model the extension carries and has to be \
         accepted — without this the refusal below happens for any broken address at \
         all:\n{intact_told}"
    );

    // One byte, in the file that is neither the tokenizer nor the weights: a trailing
    // space is JSON-insignificant and address-significant, which is exactly the point.
    let config = copied.join("config.json");
    let mut bytes = std::fs::read(&config).unwrap();
    bytes.push(b' ');
    std::fs::write(&config, bytes).unwrap();

    let (_tmp, code, told) = run_protocol(&artifact, Some((&copied, &release)));
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

// ── AC4: how much of the address is compared, not just which way it points ────

/// How many leading characters of the published address the near-miss fixture below
/// shares with it. A model check narrowed to fewer than this many characters accepts
/// that fixture and reddens the test; raising this multiplies the search by sixteen.
///
/// FIVE, not one. The refusal that produced this test used one — a check comparing a
/// single hex character left every other test in this file green, and would accept a
/// genuinely different model about one time in sixteen. Pinning five moves the
/// weakest surviving check from one character to six, so a different model gets
/// through a weakened check at worst about one time in sixteen million rather than
/// one time in sixteen. It cannot be pushed much further: finding a fixture that
/// agrees on k characters costs 16^k digests.
const NEAR_MISS_CHARS: usize = 5;

/// How many candidate fixtures the search below will try. The expected cost is
/// `16^NEAR_MISS_CHARS`; the ceiling is twenty times that, so exhausting it means
/// something is wrong with the derivation rather than that the search was unlucky.
const NEAR_MISS_SEARCH_LIMIT: u64 = 20 * (1 << (4 * NEAR_MISS_CHARS as u64));

/// The content address the extension published, out of its own version line.
fn published_key(conn: &duckdb::Connection) -> String {
    let line: String = conn
        .query_row("SELECT staticembed_version()", [], |r| r.get(0))
        .expect("the extension answers staticembed_version()");
    line.split("key ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap_or_else(|| panic!("no `key <hex>` in the version line: {line}"))
        .trim()
        .to_string()
}

/// A DECLARED MODEL WHOSE ADDRESS NEARLY MATCHES IS STILL REFUSED.
///
/// WHY THE OTHER AC4 TESTS ARE NOT ENOUGH. They pin that the check points the right
/// way — a wrong model is refused, the right one is accepted, the third file is
/// hashed. None of them pins its STRENGTH. Measured 2026-08-25: narrowing the
/// comparison in `check_declared_model` to a single hex character left all of them
/// green, and a run would then report success while embedding with weights the
/// Protocol had not declared, which is the failure AC4 exists to stop. Direction is
/// cheap to test and worth nothing on its own; strength needs an input that a weak
/// check and a strong check disagree about, and that input is this fixture.
///
/// HOW THE FIXTURE IS BUILT. The address is a SHA-256 over a domain tag, the release
/// identity and revision, and the three model files in a fixed order — so holding
/// everything but the tail of `config.json` fixed lets a counter be appended and the
/// digest state cloned per candidate. The search takes the first counter whose address
/// agrees with the published one for `NEAR_MISS_CHARS` characters.
///
/// AND THE DERIVATION IS CHECKED BEFORE IT IS TRUSTED. This test recomputes the
/// address a second time, in Rust, which would be worthless if it had drifted from the
/// operator's Python — a "near miss" that was not near would be refused by any check,
/// weak or strong, and this test would pass while proving nothing. So a candidate
/// chosen to share NO leading character is run first, and the address the operator
/// itself reports for it has to be the one computed here.
#[test]
fn a_model_whose_address_nearly_matches_is_still_refused() {
    let Some(artifact) = extension_artifact() else {
        eprintln!("skipping the near-miss address check: no ARC_STATICEMBED_EXTENSION");
        return;
    };
    if !have_uv() {
        eprintln!("skipping the near-miss address check: no `uv` on PATH");
        return;
    }
    let published = published_key(&session(&artifact));
    assert!(
        published.len() > NEAR_MISS_CHARS,
        "the extension publishes {} characters of address, so a fixture agreeing on \
         {NEAR_MISS_CHARS} of them would BE the published address — this test would \
         then be asserting that the RIGHT model is refused",
        published.len()
    );

    // The declared release and the two files that are not varied. Deliberately not the
    // model the extension carries: the point is an address that nearly matches, not a
    // model that does.
    const RELEASE_ID: &str = "someone/other-model";
    const RELEASE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const TOKENIZER: &[u8] = b"{\"not\": \"the tokenizer\"}";
    const WEIGHTS: &[u8] = b"not the weights";
    const CONFIG: &[u8] = b"{\"normalize\": true}";
    // Mirrors `MODEL_KEY_DOMAIN` and `MODEL_PARTS` in text_embed.py. The assertion
    // below is what keeps the mirror honest.
    let mut base = Sha256::new();
    base.update(b"staticembed/model-key/v1");
    base.update(RELEASE_ID.as_bytes());
    base.update([0u8]);
    base.update(RELEASE_REVISION.as_bytes());
    base.update([0u8]);
    base.update(TOKENIZER);
    base.update(WEIGHTS);
    base.update(CONFIG);

    let digest_of = |counter: u64| {
        let mut digest = base.clone();
        digest.update(counter.to_le_bytes());
        digest.finalize()
    };
    let address_of = |counter: u64| -> String {
        let mut hex = String::with_capacity(published.len());
        for byte in digest_of(counter).iter() {
            let _ = write!(hex, "{byte:02x}");
        }
        hex.truncate(published.len());
        hex
    };
    // Nibble-wise so the search does not format a string per candidate.
    let want: Vec<u8> = published
        .chars()
        .map(|c| c.to_digit(16).expect("the address is hex") as u8)
        .collect();
    let agreement_of = |counter: u64| -> usize {
        let out = digest_of(counter);
        (0..want.len())
            .take_while(|&i| {
                let byte = out[i / 2];
                let nibble = if i % 2 == 0 { byte >> 4 } else { byte & 0x0f };
                nibble == want[i]
            })
            .count()
    };
    let search = |wanted: fn(usize) -> bool| -> u64 {
        (0..NEAR_MISS_SEARCH_LIMIT)
            .find(|&counter| wanted(agreement_of(counter)))
            .unwrap_or_else(|| {
                panic!(
                    "no candidate in {NEAR_MISS_SEARCH_LIMIT} agreed as asked with \
                     {published}; the address derivation mirrored here has almost \
                     certainly drifted from the operator's"
                )
            })
    };

    let held = tempfile::tempdir().unwrap();
    let write_fixture = |name: &str, counter: u64| -> PathBuf {
        let dir = held.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tokenizer.json"), TOKENIZER).unwrap();
        std::fs::write(dir.join("model.safetensors"), WEIGHTS).unwrap();
        // Not valid JSON past the counter, and it does not need to be: the address is
        // over bytes, and nothing in this path parses the file.
        let mut config = CONFIG.to_vec();
        config.extend_from_slice(&counter.to_le_bytes());
        std::fs::write(dir.join("config.json"), config).unwrap();
        dir
    };
    let release = format!("{RELEASE_ID}@{RELEASE_REVISION}");

    // STEP ONE — check the mirror. A fixture sharing no leading character is refused
    // by a check of any strength, so the address the operator reports for it is
    // readable whatever state the check is in, and it has to be the one computed here.
    let far = search(|agreed| agreed == 0);
    let far_dir = write_fixture("far", far);
    let (_far_tmp, far_code, far_told) = run_protocol(&artifact, Some((&far_dir, &release)));
    assert_ne!(
        far_code,
        Some(0),
        "a model sharing no character of the published address must be refused:\n{far_told}"
    );
    let reported = far_told
        .split("addresses to ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .unwrap_or_else(|| panic!("the refusal has to name the recomputed address:\n{far_told}"))
        .trim();
    assert_eq!(
        reported,
        address_of(far),
        "this test recomputes the model address a second time so it can build a near \
         miss, and the operator disagrees with it — every conclusion below would be \
         about an address the operator never computes:\n{far_told}"
    );

    // STEP TWO — the near miss. Same shape of fixture, chosen so its address agrees
    // with the published one for its first NEAR_MISS_CHARS characters and diverges
    // after. A check comparing every published character refuses it; a check narrowed
    // to NEAR_MISS_CHARS or fewer accepts it and the run embeds with weights the
    // Protocol did not declare.
    let near = search(|agreed| agreed >= NEAR_MISS_CHARS);
    let near_address = address_of(near);
    assert_ne!(
        near_address, published,
        "the near miss has to be a DIFFERENT model — an address equal to the published \
         one would make the refusal below wrong rather than strong"
    );
    assert_eq!(
        near_address[..NEAR_MISS_CHARS],
        published[..NEAR_MISS_CHARS],
        "the fixture only tests what it was built to test if it really does agree for \
         {NEAR_MISS_CHARS} characters"
    );
    let near_dir = write_fixture("near", near);
    let (_near_tmp, near_code, near_told) = run_protocol(&artifact, Some((&near_dir, &release)));
    assert_ne!(
        near_code,
        Some(0),
        "a declared model whose address is {near_address} against the extension's \
         {published} — the same first {NEAR_MISS_CHARS} characters, a different model \
         — must stop the run. Accepting it means the check compares a prefix of the \
         address rather than the address, and a Protocol can embed with weights it did \
         not declare while the run reports success:\n{near_told}"
    );
}
