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

/// The other shape a mapping takes: its first key on a line of its own below
/// the parent key. The new line lands after the mapping's last entry at its
/// keys' column, read from that first key's line.
#[test]
fn adding_a_key_to_a_mapping_that_opens_on_its_own_line_uses_its_keys_column() {
    let edit = SpecEdit::Add {
        path: vec!["data".into(), "flows".into()],
        key: "format".into(),
        value: "parquet".into(),
    };
    let out = apply_yaml_edits(CHART, &[edit]).expect("a clean add applies");

    let expected = once(
        "    file: data/flows.parquet\n",
        "    file: data/flows.parquet\n    format: parquet\n",
    );
    assert_eq!(
        out, expected,
        "one line is added and every other byte is kept"
    );
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

// ------------------------------------------------------------- nest and lift

/// A chart as brightfield writes one before its legend moves: the plot's keys
/// at the document's root beside `meta:` and `data:`, a comment at the head of
/// the file, one inside the `plot:` list and one at the end of a line of it.
const FLAT_CHART: &str = "\
# Weekly tonnage through each port, one line per port.
meta:
  title: Port throughput
data:
  flows:
    file: data/flows.parquet
plot:
  # One line per port; the bars come later.
  - mark: lineY
    data: { from: flows }
    x: week
    y: tonnes   # summed per week upstream
yScale: linear
height: 240
";

/// The same chart once `plot:`, `yScale:` and `height:` sit in the first entry
/// of a `vconcat:`, written out by hand: the oracle the nest is compared with.
const NESTED_CHART: &str = "\
# Weekly tonnage through each port, one line per port.
meta:
  title: Port throughput
data:
  flows:
    file: data/flows.parquet
vconcat:
  - plot:
      # One line per port; the bars come later.
      - mark: lineY
        data: { from: flows }
        x: week
        y: tonnes   # summed per week upstream
    yScale: linear
    height: 240
";

const FLAT_COMMENTS: [&str; 3] = [
    "# Weekly tonnage through each port, one line per port.",
    "# One line per port; the bars come later.",
    "# summed per week upstream",
];

fn nest_plot(path: Vec<arc::spec::PathPart>) -> SpecEdit {
    SpecEdit::Nest {
        path,
        keys: vec!["plot".into(), "yScale".into(), "height".into()],
        under: "vconcat".into(),
    }
}

fn lift_vconcat(path: Vec<arc::spec::PathPart>) -> SpecEdit {
    SpecEdit::Lift {
        path,
        key: "vconcat".into(),
    }
}

fn load(text: &str) -> serde_yaml::Value {
    serde_yaml::from_str(text).expect("the text loads as YAML")
}

/// Nesting the plot's three keys at the root puts their lines, in their order,
/// under the first entry of a new `vconcat:`, keeps each of the three comments,
/// leaves `meta:` and `data:` as they were, and loads to the mapping the edit
/// describes: the root with those three keys replaced by a `vconcat:` holding
/// one mapping of them.
#[test]
fn nesting_the_plots_keys_at_the_root_moves_their_lines_and_keeps_their_comments() {
    let out = apply_yaml_edits(FLAT_CHART, &[nest_plot(vec![])]).expect("the nest applies");

    assert_eq!(
        out, NESTED_CHART,
        "the moved lines sit under the entry in their order"
    );
    for comment in FLAT_COMMENTS {
        assert_eq!(
            out.matches(comment).count(),
            1,
            "the comment {comment:?} is kept, once"
        );
    }
    let untouched =
        "meta:\n  title: Port throughput\ndata:\n  flows:\n    file: data/flows.parquet\n";
    assert!(
        FLAT_CHART.contains(untouched) && out.contains(untouched),
        "`meta:` and `data:` keep their bytes"
    );

    let described = load(
        "meta: {title: Port throughput}
data: {flows: {file: data/flows.parquet}}
vconcat:
  - plot: [{mark: lineY, data: {from: flows}, x: week, y: tonnes}]
    yScale: linear
    height: 240
",
    );
    assert_eq!(
        load(&out),
        described,
        "the text loads to the nested mapping"
    );
}

/// A chart whose second `hconcat:` entry is the plot to nest, with a comment
/// heading that entry, one inside its `plot:` list and one at a line's end.
const HCONCAT_CHART: &str = "\
hconcat:
  - plot:
      - mark: barY
        x: port
    width: 320
  # The weekly lines, beside the bars.
  - plot:
      # One line per port.
      - mark: lineY
        x: week   # ISO weeks
    yScale: linear
    height: 240
";

const HCONCAT_NESTED: &str = "\
hconcat:
  - plot:
      - mark: barY
        x: port
    width: 320
  # The weekly lines, beside the bars.
  - vconcat:
      - plot:
          # One line per port.
          - mark: lineY
            x: week   # ISO weeks
        yScale: linear
        height: 240
";

/// The same nest on an entry of a sequence: `hconcat[1]` stays a mapping, now
/// holding only `vconcat:`, whose first entry is the mapping the entry was,
/// with its comments; the comment heading the entry stays above it, and the
/// first entry is untouched.
#[test]
fn nesting_an_hconcat_entry_leaves_it_a_mapping_whose_vconcat_holds_what_it_was() {
    let out = apply_yaml_edits(
        HCONCAT_CHART,
        &[nest_plot(vec!["hconcat".into(), 1.into()])],
    )
    .expect("the nest applies");

    assert_eq!(out, HCONCAT_NESTED);

    let before = load(HCONCAT_CHART);
    let after = load(&out);
    assert_eq!(
        after["hconcat"][0], before["hconcat"][0],
        "the first entry is untouched"
    );
    let entry = after["hconcat"][1]
        .as_mapping()
        .expect("the entry is still a mapping");
    assert_eq!(entry.len(), 1, "the entry holds only `vconcat:`");
    assert_eq!(
        after["hconcat"][1]["vconcat"],
        serde_yaml::Value::Sequence(vec![before["hconcat"][1].clone()]),
        "`vconcat:`'s one entry is the mapping the entry was"
    );
    for comment in [
        "# The weekly lines, beside the bars.",
        "# One line per port.",
        "# ISO weeks",
    ] {
        assert_eq!(out.matches(comment).count(), 1, "{comment:?} is kept, once");
    }
}

/// Lifting `vconcat:`'s first entry back into its place, on the text each nest
/// returned, gives back the text the nest started from: it loads to the same
/// document and holds each comment it started with, byte for byte.
#[test]
fn lifting_the_nested_entry_returns_the_text_the_nest_started_from() {
    let lifted = apply_yaml_edits(NESTED_CHART, &[lift_vconcat(vec![])]).expect("the lift applies");
    assert_eq!(lifted, FLAT_CHART);
    assert_eq!(load(&lifted), load(FLAT_CHART));
    for comment in FLAT_COMMENTS {
        assert!(lifted.contains(comment), "{comment:?} is kept");
    }

    let nested = apply_yaml_edits(
        HCONCAT_CHART,
        &[nest_plot(vec!["hconcat".into(), 1.into()])],
    )
    .expect("the nest applies");
    let lifted = apply_yaml_edits(&nested, &[lift_vconcat(vec!["hconcat".into(), 1.into()])])
        .expect("the lift applies");
    assert_eq!(lifted, HCONCAT_CHART);
    assert_eq!(load(&lifted), load(HCONCAT_CHART));
}

/// The legend the caller appends after the nest is a second entry, which a
/// lift would drop; the lift refuses rather than lose it, and lifts once the
/// entry is deleted in the same batch.
#[test]
fn a_lift_refuses_a_sequence_of_two_and_lifts_once_the_second_is_deleted() {
    let legend = SpecEdit::Append {
        path: vec!["vconcat".into()],
        item: "  - legend: color\n".into(),
    };
    let with_legend =
        apply_yaml_edits(FLAT_CHART, &[nest_plot(vec![]), legend]).expect("nest, then append");

    let err = apply_yaml_edits(&with_legend, &[lift_vconcat(vec![])])
        .expect_err("a lift of one item out of two is refused");
    match err {
        Error::EditTarget { path, detail } => {
            assert_eq!(path, "vconcat");
            assert!(
                detail.contains("holds 2 items"),
                "the reason says why: {detail}"
            );
        }
        other => panic!("expected the splice's refusal, got: {other}"),
    }

    let delete = SpecEdit::Delete {
        path: vec!["vconcat".into(), 1.into()],
    };
    let lifted = apply_yaml_edits(&with_legend, &[delete, lift_vconcat(vec![])])
        .expect("delete the legend, then lift");
    assert_eq!(lifted, FLAT_CHART);
}

/// A nest whose path names no element, and one whose new key the mapping
/// already holds, each return no text and a reason naming where.
#[test]
fn a_nest_with_no_target_or_a_held_key_is_refused_with_a_reason() {
    for (path, shown) in [
        (vec!["chart".into()], "chart"),
        (vec!["hconcat".into(), 5.into()], "hconcat[5]"),
    ] {
        let err = apply_yaml_edits(HCONCAT_CHART, &[nest_plot(path)])
            .expect_err("the path names no element");
        match err {
            Error::EditTarget { path, .. } => assert_eq!(path, shown),
            other => panic!("expected the splice's refusal naming the path, got: {other}"),
        }
    }

    let held = SpecEdit::Nest {
        path: vec![],
        keys: vec!["plot".into(), "yScale".into(), "height".into()],
        under: "data".into(),
    };
    let err = apply_yaml_edits(FLAT_CHART, &[held]).expect_err("the root already holds `data:`");
    match err {
        Error::EditTarget { path, detail } => {
            assert_eq!(path, "(root)");
            assert!(
                detail.contains("already holds `data`"),
                "the reason names the key: {detail}"
            );
        }
        other => panic!("expected the splice's refusal, got: {other}"),
    }
}
