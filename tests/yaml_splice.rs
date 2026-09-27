//! The splice over YAML that is not a spec, exercised the way a sibling tool
//! editing a chart file would use it: link the crate, build [`SpecEdit`]
//! values, and hand them to `arc::spec::apply_yaml_edits` with the chart's text.
//!
//! The promise held here is the one `tests/edit_contract.rs` holds for a spec:
//! every byte an edit did not target is identical afterwards, asserted by
//! building the expected bytes independently (`str::replacen` on a substring
//! the fixture holds exactly once) and comparing whole files. The other half is
//! that the Protocol gate stays where it was: `apply_edits` and `edit_spec`
//! still refuse this text.
//!
//! The corpus is `tests/fixtures/mosaic_chart.yaml`: a header comment, a list
//! of plots each under a flush comment, a blank line between plots, and a
//! trailing comment on an attribute's line.
//!
//! Nothing here needs the `arc` binary, so the file runs with the `cli` feature
//! off: `cargo test --no-default-features --test yaml_splice`.

use arc::spec::{Error, MANIFEST_FILENAME, SpecEdit, apply_edits, apply_yaml_edits, edit_spec};

const CHART: &str = include_str!("fixtures/mosaic_chart.yaml");

/// The expected document: `from` replaced by `to`, where `from` occurs in the
/// chart exactly once, so the substitution cannot land somewhere else.
fn once(from: &str, to: &str) -> String {
    assert_eq!(
        CHART.matches(from).count(),
        1,
        "the oracle substring {from:?} must occur exactly once in the fixture"
    );
    CHART.replacen(from, to, 1)
}

fn replace_y_scale() -> SpecEdit {
    SpecEdit::Replace {
        path: vec!["vconcat".into(), 0.into(), "yScale".into()],
        value: "log".into(),
    }
}

/// The text is YAML and not a Protocol: the splice edits it, and the entry
/// that parses its result as a Protocol refuses the same text and edit.
#[test]
fn a_chart_file_is_edited_where_the_spec_entry_refuses_it() {
    assert!(
        !CHART.lines().any(|l| l.starts_with("name:")),
        "the fixture must carry no top-level `name:` key, or it tests nothing"
    );

    let refused = apply_edits(CHART, &[replace_y_scale()])
        .expect_err("a chart file is not a Protocol, so apply_edits refuses it");
    assert!(
        matches!(
            refused,
            Error::ManifestParse(_) | Error::ManifestValidation(_)
        ),
        "the refusal is the Protocol loader's, not the splice's: {refused}"
    );

    let out = apply_yaml_edits(CHART, &[replace_y_scale()])
        .expect("the same edit applies to the same text as YAML");
    assert_ne!(out, CHART, "the edit changed the text");
}

/// Replacing one attribute's value changes that value's bytes and nothing else:
/// the header comment, the comment above each plot, the blank line between
/// plots and the trailing comment two spaces right of the value are identical.
#[test]
fn replacing_an_attribute_changes_only_its_value_and_keeps_every_comment() {
    let out = apply_yaml_edits(CHART, &[replace_y_scale()]).expect("a clean replace applies");

    let expected = once(
        "    yScale: linear   # throw to log when one port dwarfs the rest\n",
        "    yScale: log   # throw to log when one port dwarfs the rest\n",
    );
    assert_eq!(out, expected, "only the value's own bytes may differ");
}

/// Adding a key the plot's mapping does not carry adds one line inside that
/// mapping, at its keys' indentation: after its last entry, and before the
/// blank line and the comment that head the next plot.
#[test]
fn adding_a_key_adds_one_line_inside_the_plots_mapping() {
    let edit = SpecEdit::Add {
        path: vec!["vconcat".into(), 0.into()],
        key: "width".into(),
        value: "680".into(),
    };
    let out = apply_yaml_edits(CHART, &[edit]).expect("a clean add applies");

    let expected = once(
        "    height: 240\n\n  # The same weeks",
        "    height: 240\n    width: 680\n\n  # The same weeks",
    );
    assert_eq!(
        out, expected,
        "one line is added and every other byte is kept"
    );
    assert_eq!(out.lines().count(), CHART.lines().count() + 1);
}

/// A path the chart does not have is refused with the path named, as the spec
/// entries refuse it, and no text comes back: a refusal from the splice is
/// never swallowed into an empty document, which would load as YAML.
#[test]
fn a_missing_target_is_refused_with_its_path() {
    let edit = SpecEdit::Replace {
        path: vec!["vconcat".into(), 5.into(), "yScale".into()],
        value: "log".into(),
    };
    let err = apply_yaml_edits(CHART, &[edit]).expect_err("the chart has two plots, not six");
    match err {
        Error::EditTarget { path, .. } => assert_eq!(path, "vconcat[5].yScale"),
        other => panic!("expected the splice's refusal naming the path, got: {other}"),
    }
}

/// The one gate the YAML splice keeps: a result that no longer loads as YAML is
/// refused with the reason, not handed back as text to write.
#[test]
fn a_splice_whose_result_will_not_load_as_yaml_is_refused() {
    let edit = SpecEdit::Replace {
        path: vec!["meta".into(), "title".into()],
        value: "[Port throughput".into(),
    };
    let err = apply_yaml_edits(CHART, &[edit]).expect_err("an unclosed flow sequence is refused");
    match err {
        Error::EditTarget { path, detail } => {
            assert_eq!(path, "(document)");
            assert!(
                detail.contains("no longer loads as YAML"),
                "the refusal says why: {detail}"
            );
        }
        other => panic!("expected the YAML gate's refusal, got: {other}"),
    }
}

/// The Protocol gate stays on the whole write path too: a chart file sitting
/// where a spec belongs is refused by `edit_spec`, and the file is untouched.
#[test]
fn edit_spec_refuses_a_chart_file_and_leaves_it_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(MANIFEST_FILENAME);
    std::fs::write(&path, CHART).expect("write the chart where a spec belongs");

    edit_spec(dir.path(), &[replace_y_scale()])
        .expect_err("edit_spec parses its result as a Protocol, and a chart is not one");

    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        CHART,
        "a refused edit writes nothing"
    );
}
