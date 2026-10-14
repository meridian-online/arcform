//! The write path, exercised the way a sibling editor would use it: link the
//! crate, build [`SpecEdit`] values, apply them through `arcform::spec`, and hold
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

use arcform::spec::{
    Error, MANIFEST_FILENAME, Manifest, SpecEdit, Step, apply_edits, apply_yaml_edits, create_spec,
    edit_spec,
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
        .env("ARCFORM_DB_DIR", std::env::temp_dir().join("arc-tests-db"))
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

/// A block value replacing `[]` starts on the next line: the space after the
/// colon goes with the old value, so the key's line does not end in a space.
#[test]
fn a_block_value_replacing_a_flow_value_leaves_no_space_after_the_colon() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into()],
        value: "\n  - x".into(),
    };
    assert_eq!(
        apply_yaml_edits("steps: []\n", &[edit]).unwrap(),
        "steps:\n  - x\n"
    );
}

/// A comment after the old value stays on the key's line, ahead of the block.
#[test]
fn a_block_value_keeps_the_comment_on_the_key_line() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into()],
        value: "\n  - x".into(),
    };
    assert_eq!(
        apply_yaml_edits("steps: []  # none yet\nnext: 1\n", &[edit]).unwrap(),
        "steps:  # none yet\n  - x\nnext: 1\n"
    );
}

/// An old value on one line is replaced exactly: only its own bytes change, and
/// a trailing comment on its line and the comment line after it stay where they
/// were, whether the new value is a scalar or a block.
#[test]
fn a_one_line_old_value_is_replaced_exactly() {
    let text = "a: 1  # one\n# section\nb: 2\n";
    let scalar = SpecEdit::Replace {
        path: vec!["a".into()],
        value: "3".into(),
    };
    assert_eq!(
        apply_yaml_edits(text, &[scalar]).unwrap(),
        "a: 3  # one\n# section\nb: 2\n"
    );
    let block = SpecEdit::Replace {
        path: vec!["a".into()],
        value: "\n  x: 1\n".into(),
    };
    assert_eq!(
        apply_yaml_edits(text, &[block]).unwrap(),
        "a:  # one\n  x: 1\n\n# section\nb: 2\n"
    );
}

/// A full-line comment after a block mapping value is not the value's: a
/// block value replacing the mapping leaves it where it was, byte for byte.
#[test]
fn a_comment_line_after_a_block_mapping_value_stays_where_it_was() {
    let edit = SpecEdit::Replace {
        path: vec!["params".into()],
        value: "\n  b: 2".into(),
    };
    assert_eq!(
        apply_yaml_edits("params:\n  a: 1\n# section\nnext: 1\n", &[edit]).unwrap(),
        "params:\n  b: 2\n# section\nnext: 1\n"
    );
}

/// The same for a block scalar value: the comment line after it stays put.
#[test]
fn a_comment_line_after_a_block_scalar_value_stays_where_it_was() {
    let edit = SpecEdit::Replace {
        path: vec!["note".into()],
        value: "\n  - x".into(),
    };
    assert_eq!(
        apply_yaml_edits("note: |\n  first\n  second\n# section\nnext: 1\n", &[edit]).unwrap(),
        "note:\n  - x\n# section\nnext: 1\n"
    );
}

/// A `# …` line indented as far as a block scalar's text is the scalar's own
/// text, and is replaced with it.
#[test]
fn a_hash_line_inside_a_block_scalar_is_replaced_with_it() {
    let edit = SpecEdit::Replace {
        path: vec!["note".into()],
        value: "\n  - x".into(),
    };
    assert_eq!(
        apply_yaml_edits("note: |\n  first\n  # not a comment\nnext: 1\n", &[edit]).unwrap(),
        "note:\n  - x\nnext: 1\n"
    );
}

/// A block sequence written at its key's own indent keeps the comment after it.
#[test]
fn a_comment_line_after_a_sequence_at_the_key_indent_stays_where_it_was() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into()],
        value: "\n  - x".into(),
    };
    assert_eq!(
        apply_yaml_edits("steps:\n- a\n- b\n# section\nnext: 1\n", &[edit]).unwrap(),
        "steps:\n  - x\n# section\nnext: 1\n"
    );
}

/// A value that is not a block keeps the comment after an old block value too.
#[test]
fn a_flow_value_replacing_a_block_mapping_keeps_the_comment_after_it() {
    let edit = SpecEdit::Replace {
        path: vec!["params".into()],
        value: "{b: 2}".into(),
    };
    assert_eq!(
        apply_yaml_edits("params:\n  a: 1\n# section\nnext: 1\n", &[edit]).unwrap(),
        "params:\n  {b: 2}\n# section\nnext: 1\n"
    );
}

/// A blank line between an old block value and the comment after it stays.
#[test]
fn a_blank_line_before_the_comment_after_a_block_value_stays() {
    let edit = SpecEdit::Replace {
        path: vec!["params".into()],
        value: "\n  b: 2".into(),
    };
    assert_eq!(
        apply_yaml_edits("params:\n  a: 1\n\n# section\nnext: 1\n", &[edit]).unwrap(),
        "params:\n  b: 2\n\n# section\nnext: 1\n"
    );
}

/// The key of a sequence item's first pair is measured from its own column,
/// past the `- `, so a comment at that column after its block value stays.
#[test]
fn a_comment_at_a_sequence_items_key_column_stays_after_its_block_mapping() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 0.into(), "params".into()],
        value: "\n      b: 2".into(),
    };
    assert_eq!(
        apply_yaml_edits(
            "steps:\n  - params:\n      a: 1\n    # c\n  - name: y\n",
            &[edit]
        )
        .unwrap(),
        "steps:\n  - params:\n      b: 2\n    # c\n  - name: y\n"
    );
}

/// The same holds for a sequence item nested deeper.
#[test]
fn a_comment_at_a_nested_sequence_items_key_column_stays() {
    let edit = SpecEdit::Replace {
        path: vec!["outer".into(), "steps".into(), 0.into(), "a".into()],
        value: "\n        y: 2".into(),
    };
    assert_eq!(
        apply_yaml_edits(
            "outer:\n  steps:\n    - a:\n        x: 1\n      # c\n    - b: 2\n",
            &[edit]
        )
        .unwrap(),
        "outer:\n  steps:\n    - a:\n        y: 2\n      # c\n    - b: 2\n"
    );
}

/// A block sequence under a sequence item's first key keeps the comment after it.
#[test]
fn a_comment_at_a_sequence_items_key_column_stays_after_its_block_sequence() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 0.into(), "deps".into()],
        value: "\n      - z".into(),
    };
    assert_eq!(
        apply_yaml_edits(
            "steps:\n  - deps:\n      - x\n      - w\n    # c\n  - name: y\n",
            &[edit]
        )
        .unwrap(),
        "steps:\n  - deps:\n      - z\n    # c\n  - name: y\n"
    );
}

/// A sequence item has no key: a comment indented no further than the item's
/// dash is after it and stays.
#[test]
fn a_comment_after_a_replaced_sequence_item_stays() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 0.into()],
        value: "{a: 3}".into(),
    };
    assert_eq!(
        apply_yaml_edits("steps:\n- a: 1\n  b: 2\n# c\n- name: y\n", &[edit]).unwrap(),
        "steps:\n- {a: 3}\n# c\n- name: y\n"
    );
}

/// A block value replacing an old one on a later line leaves no spaces-only
/// line, even when the key's line carries a comment.
#[test]
fn a_block_value_under_a_commented_key_leaves_no_spaces_only_line() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into()],
        value: "\n  - name: b".into(),
    };
    assert_eq!(
        apply_yaml_edits("steps:  # pipeline\n  - name: a\nnext: 1\n", &[edit]).unwrap(),
        "steps:  # pipeline\n  - name: b\nnext: 1\n"
    );
}

/// A block scalar item may indent its text less than its `|`: a `#` line
/// deeper than the item's dash is still its text and is replaced with it.
#[test]
fn a_hash_line_inside_a_shallow_block_scalar_item_is_replaced_with_it() {
    let edit = SpecEdit::Replace {
        path: vec!["steps".into(), 0.into()],
        value: "x2".into(),
    };
    assert_eq!(
        apply_yaml_edits("steps:\n- |\n a\n # b\n- x\n", &[edit]).unwrap(),
        "steps:\n- x2\n- x\n"
    );
}

/// The absent value of a bare `key:` is replaced just after the colon, as it was.
#[test]
fn a_bare_key_takes_its_value_just_after_the_colon() {
    let block = SpecEdit::Replace {
        path: vec!["steps".into()],
        value: "\n  - x".into(),
    };
    assert_eq!(
        apply_yaml_edits("steps:\nnext: 1\n", &[block]).unwrap(),
        "steps:\n  - x\nnext: 1\n"
    );
    let scalar = SpecEdit::Replace {
        path: vec!["name".into()],
        value: " shop".into(),
    };
    assert_eq!(
        apply_yaml_edits("name:\nnext: 1\n", &[scalar]).unwrap(),
        "name: shop\nnext: 1\n"
    );
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
        extensions: Default::default(),
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
        description: None,
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
