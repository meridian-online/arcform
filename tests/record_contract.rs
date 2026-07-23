//! The record path, held to the promises that make promotion safe:
//!
//!   1. **byte preservation** — recording a step onto a hand-authored,
//!      commented spec changes exactly the appended lines and nothing else,
//!      proven against an independently constructed oracle;
//!   2. **refusal leaves no trace** — a refused promotion (duplicate name,
//!      hostile name, occupied model path, failed manifest write) leaves the
//!      manifest byte-identical AND writes no model file, in either order of
//!      failure;
//!   3. **ownership** — the marker is the license to regenerate: amending a
//!      generated model rewrites it, amending a hand-authored one is refused
//!      with the file untouched and the downstream-step remedy in the reason;
//!   4. **parity** — a protocol grown purely by recorded steps runs under the
//!      bare `arc` binary, with the recorded SQL doing real work in a real
//!      engine. Recording itself runs nothing: until `arc run`, the recorded
//!      step's asset exists nowhere but the promise.
//!
//! The manifest corpus is `examples/almanac` — the same hostile fixture the
//! write-path contract tests use: flush comment headers, a blank-separated
//! section label, trailing same-line comments, a block scalar with an interior
//! blank line, and a footer comment after the last step. Tests that write
//! operate on a copy in a temp dir.

use std::path::{Path, PathBuf};
use std::process::Command;

use arc::spec::{
    Error, GENERATED_MARKER, MANIFEST_FILENAME, Manifest, RecordedStep, Step, amend_step_sql,
    create_spec, record_step, sql_is_generated,
};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/almanac")
}

fn corpus() -> String {
    std::fs::read_to_string(corpus_dir().join(MANIFEST_FILENAME)).expect("corpus is readable")
}

/// Copy the corpus spec into a fresh protocol directory.
fn corpus_copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(MANIFEST_FILENAME), corpus()).expect("copy corpus");
    dir
}

/// Run the real `arc` binary in `dir` and demand success.
fn arc_run(dir: &Path) {
    let run = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
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
}

/// A promotion value with a grid-filter shape: the narrowest real capture.
fn tide_capture() -> RecordedStep {
    RecordedStep {
        name: "dover_tides".to_string(),
        sql: "CREATE OR REPLACE TABLE dover_tides AS\n\
              SELECT * FROM read_csv('data/tides.csv') WHERE port = 'dover';"
            .to_string(),
        provenance: "grid filter on tide_table (port = 'dover')".to_string(),
    }
}

// ------------------------------------------------------------ byte preservation

/// Recording onto the hostile corpus touches exactly the appended lines: the
/// oracle is built by textual substitution, and the model file is the marker
/// line plus the SQL verbatim.
#[test]
fn recording_appends_the_step_and_preserves_every_other_byte() {
    let dir = corpus_copy();
    let original = corpus();

    let (sql_rel, validated) = record_step(dir.path(), &tide_capture()).expect("records");

    // The model landed where the manifest says, with the marker and the body.
    assert_eq!(sql_rel, Path::new("models").join("01_dover_tides.sql"));
    let model = std::fs::read_to_string(dir.path().join(&sql_rel)).unwrap();
    assert!(sql_is_generated(&model), "the marker licenses regeneration");
    assert_eq!(
        model,
        format!(
            "{GENERATED_MARKER} grid filter on tide_table (port = 'dover')\n{}\n",
            tide_capture().sql
        ),
        "marker line, then the SQL verbatim, then the final newline"
    );

    // The manifest changed by exactly the appended item — before the footer,
    // indented like the document's own steps.
    let item = "  - name: dover_tides\n    sql: models/01_dover_tides.sql\n";
    let expected = original.replacen("\n# fin —", &format!("{item}\n# fin —"), 1);
    let on_disk = std::fs::read_to_string(dir.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(on_disk, expected, "every untargeted byte is identical");
    assert_eq!(
        on_disk,
        validated.text(),
        "what was returned is what is on disk"
    );

    // The loader sees the recorded step last, shaped like a hand-written one.
    let reloaded = Manifest::load(dir.path()).expect("the grown spec loads");
    let last = reloaded.steps.last().unwrap();
    assert_eq!(last.name, "dover_tides");
    assert_eq!(last.sql.as_deref(), Some("models/01_dover_tides.sql"));
    assert!(
        last.produces.is_empty() && last.depends_on.is_empty(),
        "wiring is introspection's job, exactly as for a hand-written sql step"
    );
}

/// Model numbering continues from the highest recorded model, and a second
/// recording extends the same document the same way.
#[test]
fn a_second_recording_takes_the_next_number() {
    let dir = corpus_copy();
    record_step(dir.path(), &tide_capture()).expect("first records");

    let second = RecordedStep {
        name: "spring_tides".to_string(),
        sql: "CREATE OR REPLACE TABLE spring_tides AS\n\
              SELECT * FROM dover_tides WHERE tide_m > 5.5;"
            .to_string(),
        provenance: "grid filter on dover_tides (tide_m > 5.5)".to_string(),
    };
    let (sql_rel, validated) = record_step(dir.path(), &second).expect("second records");
    assert_eq!(sql_rel, Path::new("models").join("02_spring_tides.sql"));

    let names: Vec<&str> = validated
        .manifest()
        .steps
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "tide_table",
            "moon_phases",
            "join_report",
            "show",
            "dover_tides",
            "spring_tides"
        ]
    );
}

// ----------------------------------------------------- refusal leaves no trace

/// A promotion the spec gate refuses — a duplicate step name — writes nothing:
/// the manifest is byte-identical and no model file appears.
#[test]
fn a_refused_promotion_leaves_manifest_and_models_untouched() {
    let dir = corpus_copy();
    let before = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();

    let duplicate = RecordedStep {
        name: "show".to_string(), // already a step in the corpus
        sql: "SELECT 1;".to_string(),
        provenance: "duplicate".to_string(),
    };
    match record_step(dir.path(), &duplicate) {
        Err(Error::ManifestValidation(msg)) => {
            assert!(msg.contains("duplicate step name"), "{msg}");
        }
        other => panic!("expected the gate to refuse, got {other:?}"),
    }

    let after = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(after, before, "the manifest is untouched, byte for byte");
    assert!(
        !dir.path().join("models").exists(),
        "no model file survives a refused promotion"
    );
}

/// Create mode never overwrites: a model already occupying a number is left
/// byte-untouched, and the recording sidesteps to the next number instead.
/// (The [`Error::GeneratedSqlExists`] refusal remains as the backstop for the
/// race this sidestep cannot see — a file appearing between the scan and the
/// write — so no future change can make an overwrite silent.)
#[test]
fn an_occupied_model_number_is_sidestepped_never_overwritten() {
    let dir = corpus_copy();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let occupied = models.join("01_dover_tides.sql");
    std::fs::write(&occupied, "-- someone's file\nSELECT 1;\n").unwrap();

    let (sql_rel, _) = record_step(dir.path(), &tide_capture()).expect("records beside it");
    assert_eq!(
        sql_rel,
        Path::new("models").join("02_dover_tides.sql"),
        "the recording takes the next number rather than the occupied one"
    );
    assert_eq!(
        std::fs::read_to_string(&occupied).unwrap(),
        "-- someone's file\nSELECT 1;\n",
        "the occupying file is untouched, byte for byte"
    );
}

/// An empty protocol has nothing to explore, so it has nothing to record
/// against — refused with the sequence named, before anything is written.
#[test]
fn recording_into_a_protocol_without_steps_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(MANIFEST_FILENAME),
        "name: bare\nsteps: []\n",
    )
    .unwrap();

    match record_step(dir.path(), &tide_capture()) {
        Err(Error::EditTarget { path, .. }) => assert_eq!(path, "steps"),
        other => panic!("expected EditTarget, got {other:?}"),
    }
    assert!(!dir.path().join("models").exists());
}

/// The provenance note is the one-line marker header; a note that spans lines
/// would smuggle its tail into the SQL body, so it is refused up front.
#[test]
fn a_multi_line_provenance_is_refused_before_anything_happens() {
    let dir = corpus_copy();
    let mut capture = tide_capture();
    capture.provenance = "line one\nline two".to_string();
    match record_step(dir.path(), &capture) {
        Err(Error::EditTarget { path, .. }) => assert_eq!(path, "(provenance)"),
        other => panic!("expected EditTarget, got {other:?}"),
    }
}

/// The step name is spliced into the manifest verbatim, so a name YAML would
/// read as anything other than itself is refused with the name called out and
/// nothing written. The corpus here is the smuggling constructions themselves:
/// a name that injects a manifest field, one that injects wiring, one that
/// injects a whole second step (citing a model that does not exist), one a
/// comment marker would silently truncate, and one that opens a mapping.
#[test]
fn a_hostile_step_name_is_refused_with_nothing_written() {
    for hostile in [
        "x\n    timeout_sec: 1",                               // injects a field
        "x\n    depends_on: [tide_table]",                     // injects wiring
        "x\n    sql: models/09_ghost.sql\n  - name: injected", // injects a second step
        "top10 # draft",                                       // truncates to 'top10'
        "a: b",                                                // opens a mapping
        " dover",                                              // YAML drops the pad
        "- dover",                                             // leading indicator
    ] {
        let dir = corpus_copy();
        let before = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();

        let mut capture = tide_capture();
        capture.name = hostile.to_string();
        match record_step(dir.path(), &capture) {
            Err(Error::EditTarget { path, .. }) => assert_eq!(path, "(name)", "{hostile:?}"),
            other => panic!("expected {hostile:?} refused, got {other:?}"),
        }

        assert_eq!(
            std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap(),
            before,
            "the manifest is untouched after refusing {hostile:?}"
        );
        assert!(
            !dir.path().join("models").exists(),
            "no model file survives the refusal of {hostile:?}"
        );
    }
}

/// Force the last write of a promotion — the manifest — to fail, and demand a
/// full retreat: the protocol directory is made read-only while a pre-existing
/// `models/` stays writable, so the model lands and the manifest cannot. The
/// rollback must take the model back out and leave `models/` (which the
/// promotion did not create) standing.
#[cfg(unix)]
#[test]
fn a_failed_manifest_write_takes_the_model_back_out() {
    use std::os::unix::fs::PermissionsExt;

    let dir = corpus_copy();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let before = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();

    // Reads stay allowed; creating the manifest's temp file does not.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    // Root ignores permission bits, so the failure cannot be staged there —
    // probe, and stand down rather than mis-assert.
    let probe = dir.path().join(".probe");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let result = record_step(dir.path(), &tide_capture());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(result.is_err(), "the promotion must fail, got {result:?}");
    assert_eq!(
        std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap(),
        before,
        "the manifest is untouched, byte for byte"
    );
    assert_eq!(
        std::fs::read_dir(&models).unwrap().count(),
        0,
        "no orphan model survives the failed promotion"
    );
    assert!(
        models.exists(),
        "the pre-existing models/ dir is not the rollback's to remove"
    );
}

// -------------------------------------------------------------------- ownership

/// The marker is the license: amending a recorded step rewrites its model
/// wholesale — new provenance, new body — and leaves the manifest alone.
#[test]
fn amend_regenerates_a_marked_model_and_only_that() {
    let dir = corpus_copy();
    record_step(dir.path(), &tide_capture()).expect("records");
    let manifest_before = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();

    let rewritten = amend_step_sql(
        dir.path(),
        "dover_tides",
        "CREATE OR REPLACE TABLE dover_tides AS\n\
         SELECT * FROM read_csv('data/tides.csv') WHERE port = 'hobart';",
        "grid filter on tide_table (port = 'hobart')",
    )
    .expect("a marked model may be regenerated");

    assert_eq!(rewritten, Path::new("models").join("01_dover_tides.sql"));
    let model = std::fs::read_to_string(dir.path().join(&rewritten)).unwrap();
    assert!(model.starts_with(GENERATED_MARKER));
    assert!(
        model.contains("port = 'hobart'"),
        "the new body is in place"
    );
    assert!(!model.contains("port = 'dover'"), "the old body is gone");

    assert_eq!(
        std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap(),
        manifest_before,
        "amending a model never touches the manifest"
    );
}

/// The ownership refusal: a model without the marker was authored by a person,
/// and its bytes are never machine-rewritten — the refusal names the remedy.
#[test]
fn amend_refuses_a_hand_authored_model_and_leaves_it_untouched() {
    let dir = corpus_copy();
    let models = dir.path().join("models");
    std::fs::create_dir_all(&models).unwrap();
    let hand = "-- Tuned by hand: the read_csv options matter here.\n\
                CREATE OR REPLACE TABLE curated AS SELECT 1;\n";
    std::fs::write(models.join("curated.sql"), hand).unwrap();

    // Cite the hand-authored model from a step, through the write path.
    let edit = arc::spec::SpecEdit::Append {
        path: vec!["steps".into()],
        item: "  - name: curated\n    sql: models/curated.sql\n".to_string(),
    };
    arc::spec::edit_spec(dir.path(), &[edit]).expect("the citation splices");

    match amend_step_sql(dir.path(), "curated", "SELECT 2;", "retune") {
        Err(Error::HandAuthoredSql { step, path }) => {
            assert_eq!(step, "curated");
            assert!(path.ends_with("models/curated.sql"));
            let reason = Error::HandAuthoredSql { step, path }.to_string();
            assert!(
                reason.contains("record a new step downstream"),
                "the refusal offers the remedy: {reason}"
            );
        }
        other => panic!("expected HandAuthoredSql, got {other:?}"),
    }

    assert_eq!(
        std::fs::read_to_string(models.join("curated.sql")).unwrap(),
        hand,
        "a hand-authored model is untouched, byte for byte"
    );
}

/// Steps that are not sql steps, and steps that do not exist, are refused with
/// the path and the reason — there is nothing this path may rewrite.
#[test]
fn amend_refuses_command_steps_and_unknown_steps() {
    let dir = corpus_copy();

    // `tide_table` is a command step: its recipe is hand-authored by definition.
    match amend_step_sql(dir.path(), "tide_table", "SELECT 1;", "nope") {
        Err(Error::EditTarget { path, detail }) => {
            assert_eq!(path, "steps.tide_table");
            assert!(detail.contains("record a new step downstream"), "{detail}");
        }
        other => panic!("expected EditTarget, got {other:?}"),
    }

    match amend_step_sql(dir.path(), "absent", "SELECT 1;", "nope") {
        Err(Error::EditTarget { path, detail }) => {
            assert_eq!(path, "steps");
            assert!(detail.contains("absent"), "{detail}");
        }
        other => panic!("expected EditTarget, got {other:?}"),
    }
}

// ----------------------------------------------------------------------- parity

/// A protocol grown purely by recorded steps runs under the bare binary: the
/// recorded SQL is real SQL against a real engine, and nothing about the grown
/// spec needs the recording tool at run time. Recording itself ran nothing —
/// the recorded steps' outputs exist only after `arc run` says so.
#[test]
fn a_spec_grown_by_recording_runs_under_the_bare_binary() {
    let base = tempfile::tempdir().expect("tempdir");
    let proto = base.path().join("grown");

    // The seed: one command step that fetches raw data. Everything after it
    // is recorded.
    let mut manifest = Manifest {
        name: "grown".into(),
        engine: "duckdb".into(),
        engine_version: None,
        db: None,
        params: Default::default(),
        dotenv: Vec::new(),
        timeout_sec: None,
        defaults: None,
        hooks: Default::default(),
        steps: Vec::new(),
        assets: Default::default(),
    };
    manifest.steps.push(Step {
        name: "fetch".into(),
        sql: None,
        command: Some(
            "printf 'day,port,tide_m\\n1,dover,5.1\\n2,hobart,1.4\\n3,dover,5.9\\n' > tides.csv"
                .into(),
        ),
        op: None,
        with: None,
        produces: vec!["tides_csv".into()],
        depends_on: Vec::new(),
        preconditions: Vec::new(),
        output: None,
        retry: None,
        timeout_sec: None,
    });
    create_spec(&proto, &manifest).expect("the seed spec is created");

    // Promotion one: load the raw file into a table.
    let (first, _) = record_step(
        &proto,
        &RecordedStep {
            name: "tides".into(),
            sql: "CREATE OR REPLACE TABLE tides AS SELECT * FROM read_csv('tides.csv');".into(),
            provenance: "table load of tides.csv".into(),
        },
    )
    .expect("first promotion records");
    assert_eq!(first, Path::new("models").join("01_tides.sql"));

    // Promotion two: a grid filter over the first recording's table, exported
    // so the filesystem can testify that the step really ran.
    record_step(
        &proto,
        &RecordedStep {
            name: "dover".into(),
            sql: "COPY (SELECT * FROM tides WHERE port = 'dover') TO 'dover.csv' \
                  (FORMAT CSV, HEADER);"
                .into(),
            provenance: "grid filter on tides (port = 'dover')".into(),
        },
    )
    .expect("second promotion records");

    // Recording ran nothing: no database, no outputs — only files of intent.
    assert!(
        !proto.join("grown.duckdb").exists() && !proto.join("dover.csv").exists(),
        "recording must not execute anything"
    );

    // The bare binary makes the promises true.
    arc_run(&proto);
    let dover = std::fs::read_to_string(proto.join("dover.csv")).unwrap();
    assert!(
        dover.contains("1,dover,5.1") && dover.contains("3,dover,5.9"),
        "{dover}"
    );
    assert!(!dover.contains("hobart"), "the filter filtered: {dover}");
}
