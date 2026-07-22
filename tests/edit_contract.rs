//! The write path, exercised the way a sibling editor would use it: link the
//! crate, build [`SpecEdit`] values, apply them through `arc::spec`, and hold
//! the result to the two promises that matter —
//!
//!   1. **byte preservation** — every byte an edit did not target is identical
//!      afterwards, asserted by constructing the expected bytes independently
//!      (`str::replacen` on a fixture substring) and comparing whole files, not
//!      by eyeballing; and
//!   2. **still runs under `arc`** — the edited spec is executed by the real
//!      binary, not just re-parsed.
//!
//! The corpus is `examples/almanac`, a hand-authored spec whose steps are
//! inline `command: |` block scalars (one with a blank line inside the scalar),
//! with flush comment headers, a blank-separated section label, and trailing
//! same-line comments — the shapes a format-preserving editor has to respect.
//! Tests that write to disk operate on a copy in a temp dir; the checked-in
//! example is never modified.

use std::path::{Path, PathBuf};
use std::process::Command;

use arc::spec::{
    Error, MANIFEST_FILENAME, Manifest, SpecEdit, Step, apply_edits, create_spec, edit_spec,
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

/// Step names, in order, as the loader sees them.
fn step_names(manifest: &Manifest) -> Vec<&str> {
    manifest.steps.iter().map(|s| s.name.as_str()).collect()
}

// ------------------------------------------------------------ byte preservation

/// Replacing one scalar changes that scalar's bytes and nothing else — the
/// trailing same-line comment two characters to its right included.
#[test]
fn replace_changes_only_the_targeted_bytes() {
    let original = corpus();
    let edit = SpecEdit::Replace {
        path: vec!["params".into(), "port".into(), "default".into()],
        value: "\"hobart\"".into(),
    };
    let out = apply_edits(&original, &[edit]).expect("a clean replace applies");

    // The oracle is built without the write path: the fixture substring is
    // unique, so the expected document is a one-shot textual substitution.
    let expected = original.replacen("\"dover\"   # try hobart", "\"hobart\"   # try hobart", 1);
    assert_eq!(
        out.text(),
        expected,
        "only the value's own bytes may differ"
    );
    assert_eq!(
        out.manifest().params["port"].default.as_deref(),
        Some("hobart")
    );
}

/// The block-scalar case: rewrite three characters inside a multi-line
/// `command: |` and prove the other twenty-odd lines are untouched — including
/// the blank line *inside* the scalar, which a naive line-walker would treat
/// as the end of the step.
#[test]
fn rewrite_fragment_edits_inside_a_block_scalar_with_an_interior_blank_line() {
    let original = corpus();
    assert!(
        original.contains("\n\n      printf '1,hobart"),
        "the corpus must keep a blank line inside the tide_table block scalar; \
         this test exists to cover that shape"
    );

    let edit = SpecEdit::RewriteFragment {
        path: vec!["steps".into(), 0.into(), "command".into()],
        from: "3,dover,5.9".into(),
        to: "3,dover,6.2".into(),
    };
    let out = apply_edits(&original, &[edit]).expect("the fragment is unique");

    let expected = original.replacen("3,dover,5.9", "3,dover,6.2", 1);
    assert_eq!(out.text(), expected);

    // And the edited spec still runs — block scalar intact, both halves of it.
    let dir = corpus_copy();
    out.write_to(dir.path()).expect("atomic write");
    arc_run(dir.path());
    let tides = std::fs::read_to_string(dir.path().join("data/tides.csv")).unwrap();
    assert!(tides.contains("3,dover,6.2"), "the edit reached the run");
    assert!(
        tides.contains("3,hobart,0.9"),
        "the scalar's second half ran too"
    );
}

/// Appending a step lands it after the last item's body and before the footer
/// comment, and the grown spec still runs end to end.
#[test]
fn append_a_step_before_the_footer_and_still_run() {
    let original = corpus();
    // NOT a `"\` continuation literal: that escape eats the next line's leading
    // whitespace, and the indentation is load-bearing here.
    let item = "  - name: row_count\n    command: \"test -s data/rows.txt\"\n    depends_on: [report_csv]\n";
    let edit = SpecEdit::Append {
        path: vec!["steps".into()],
        item: item.into(),
    };
    let out = apply_edits(&original, &[edit]).expect("append applies");

    let expected = original.replacen("\n# fin —", &format!("{item}\n# fin —"), 1);
    assert_eq!(
        out.text(),
        expected,
        "the new item belongs directly after the last step's body, before the footer"
    );
    assert_eq!(
        step_names(out.manifest()).last(),
        Some(&"row_count"),
        "the loader sees the appended step last"
    );

    let dir = corpus_copy();
    out.write_to(dir.path()).expect("atomic write");
    arc_run(dir.path());
}

/// Deleting a step takes its flush comment header with it, consumes the blank
/// separator, and leaves everything else — the footer included — untouched.
#[test]
fn delete_takes_the_flush_header_and_spares_the_footer() {
    let original = corpus();
    // NOT a `"\` continuation literal: that escape eats the next line's leading
    // whitespace, and the oracle must carry the exact indentation.
    let doomed = concat!(
        "  # Console tail: the demo ends by printing the table it built.\n",
        "  - name: show\n",
        "    command: \"cat data/report.csv\"\n",
        "    depends_on: [report_csv]\n",
        "\n",
    );
    assert!(
        original.contains(doomed),
        "the oracle substring must match the fixture"
    );

    let edit = SpecEdit::Delete {
        path: vec!["steps".into(), 3.into()],
    };
    let out = apply_edits(&original, &[edit]).expect("delete applies");

    let expected = original.replacen(doomed, "", 1);
    assert_eq!(out.text(), expected);
    assert!(
        out.text().contains("# fin —"),
        "the footer after the deleted item survives"
    );
    assert_eq!(
        step_names(out.manifest()),
        ["tide_table", "moon_phases", "join_report"]
    );

    let dir = corpus_copy();
    out.write_to(dir.path()).expect("atomic write");
    arc_run(dir.path());
}

/// A comment separated from the step below it by a blank line is a section
/// label owned by the sequence: deleting that step leaves the label standing.
#[test]
fn delete_leaves_a_blank_separated_section_label_in_place() {
    let original = corpus();
    let edit = SpecEdit::Delete {
        path: vec!["steps".into(), 2.into()],
    };
    // join_report goes; `show`'s depends_on now names an asset nothing
    // produces, but asset wiring is the runner's concern — the spec still loads.
    let out = apply_edits(&original, &[edit]).expect("delete applies");
    assert!(
        out.text().contains("# ── Report: keep the chosen port"),
        "the blank-separated section label belongs to the sequence, not the item:\n{}",
        out.text()
    );
    assert!(!out.text().contains("join_report"));
}

/// Reordering moves an item and its flush header as one block: the same lines,
/// rearranged — no line edited, none invented, none lost.
#[test]
fn reorder_moves_the_item_with_its_header_and_still_runs() {
    let original = corpus();
    let edit = SpecEdit::Reorder {
        path: vec!["steps".into()],
        from: 1,
        to: 0,
    };
    let out = apply_edits(&original, &[edit]).expect("reorder applies");

    assert_eq!(
        step_names(out.manifest()),
        ["moon_phases", "tide_table", "join_report", "show"]
    );

    let mut before: Vec<&str> = original.lines().collect();
    let mut after: Vec<&str> = out.text().lines().collect();
    before.sort_unstable();
    after.sort_unstable();
    assert_eq!(before, after, "reorder may move lines but never edit them");

    // The moved item's flush header is still directly above it.
    let text = out.text();
    let header = text.find("# The lunar table").expect("header survives");
    let moved = text.find("- name: moon_phases").expect("item survives");
    let displaced = text
        .find("- name: tide_table")
        .expect("other item survives");
    assert!(
        header < moved && moved < displaced,
        "header travelled with its item"
    );

    let dir = corpus_copy();
    out.write_to(dir.path()).expect("atomic write");
    arc_run(dir.path());
}

// --------------------------------------------------------- refusal, before disk

/// An edit that would produce a spec the loader rejects is refused with the
/// loader's reason, and the file on disk is not touched — byte for byte.
#[test]
fn a_breaking_edit_is_refused_and_the_file_is_untouched() {
    let dir = corpus_copy();
    let before = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();

    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 1.into(), "name".into()],
        value: "tide_table".into(), // duplicate step name: valid YAML, invalid spec
    };
    match edit_spec(dir.path(), &[edit]) {
        Err(Error::ManifestValidation(msg)) => {
            assert!(
                msg.contains("duplicate step name"),
                "the refusal names the defect: {msg}"
            );
        }
        other => panic!("expected the gate to refuse, got {other:?}"),
    }

    let after = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(
        after, before,
        "a refused edit must leave the spec untouched"
    );
}

/// An edit whose target does not exist is refused with the path in the reason.
#[test]
fn a_missing_target_is_refused_with_its_path() {
    let dir = corpus_copy();
    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 9.into(), "command".into()],
        value: "\"true\"".into(),
    };
    match edit_spec(dir.path(), &[edit]) {
        Err(Error::EditTarget { path, .. }) => assert_eq!(path, "steps[9].command"),
        other => panic!("expected EditTarget, got {other:?}"),
    }
}

// ------------------------------------------------------------- the whole path

/// The one-shot library operation: apply → validate → atomic write, leaving no
/// temp droppings, and the written spec both reloads and runs.
#[test]
fn edit_spec_applies_validates_and_writes_atomically() {
    let dir = corpus_copy();
    let edits = [
        SpecEdit::Replace {
            path: vec!["params".into(), "port".into(), "default".into()],
            value: "\"hobart\"".into(),
        },
        SpecEdit::Add {
            path: vec!["steps".into(), 3.into()],
            key: "timeout_sec".into(),
            value: "15".into(),
        },
    ];
    let out = edit_spec(dir.path(), &edits).expect("the write path succeeds");

    // What edit_spec returned is exactly what is on disk.
    let on_disk = std::fs::read_to_string(dir.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(on_disk, out.text());

    // No temp files or other litter next to the spec.
    let mut entries: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        [MANIFEST_FILENAME],
        "temp files must not survive the write"
    );

    // The file reloads through the loader and runs under the binary; the
    // edited param default is what the run used.
    let reloaded = Manifest::load(dir.path()).expect("the written spec loads");
    assert_eq!(reloaded.params["port"].default.as_deref(), Some("hobart"));
    assert_eq!(reloaded.steps[3].timeout_sec, Some(15.0));
    arc_run(dir.path());
    let report = std::fs::read_to_string(dir.path().join("data/report.csv")).unwrap();
    assert!(
        report.contains("1.1"),
        "the hobart rows made the report: {report}"
    );
    assert!(!report.contains("5.1"), "the dover rows did not: {report}");
}

/// Creating from scratch is direct serialisation — no preservation machinery —
/// through the same gate and the same atomic write, and the result runs.
#[test]
fn create_spec_serialises_directly_and_the_result_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("fresh");

    let mut manifest = Manifest {
        name: "fresh".into(),
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
        name: "hello".into(),
        sql: None,
        command: Some("printf 'hello\\n' > out.txt".into()),
        op: None,
        with: None,
        produces: vec!["out_txt".into()],
        depends_on: Vec::new(),
        preconditions: Vec::new(),
        output: None,
        retry: None,
        timeout_sec: None,
    });

    let out = create_spec(&project, &manifest).expect("create succeeds");
    assert_eq!(out.manifest().name, "fresh");

    let written = Manifest::load(&project).expect("the created spec loads");
    assert_eq!(step_names(&written), ["hello"]);
    arc_run(&project);
    assert!(project.join("out.txt").is_file());

    // A second create refuses: the file may have been hand-edited since.
    match create_spec(&project, &manifest) {
        Err(Error::SpecExists(path)) => assert!(path.ends_with(MANIFEST_FILENAME)),
        other => panic!("expected SpecExists, got {other:?}"),
    }
}
