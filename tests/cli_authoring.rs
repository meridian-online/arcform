//! The CLI as an authoring modality: a protocol created, run, amended and
//! re-run through the `arc` binary alone — no other tool in the loop — plus
//! proof that the CLI *is* the library write path rather than a second
//! implementation of it:
//!
//!   1. **author end to end** — `create-protocol` scaffolds, `edit-protocol`
//!      authors the steps, `run` executes, another edit amends, `run` again;
//!   2. **byte preservation, identically** — editing a hand-authored commented
//!      spec through the binary produces byte-for-byte the document the
//!      library path produces for the same edit;
//!   3. **refuse before the file is touched** — an invalid edit exits nonzero
//!      with the reason on stderr and the spec byte-identical on disk.
//!
//! The commented corpus is `examples/almanac`, the same one the library's own
//! write-path contract tests use; tests that write operate on a copy in a
//! temp dir.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arc::spec::{MANIFEST_FILENAME, SpecEdit, apply_edits};

/// Run the real `arc` binary with `args` in `dir`.
fn arc_cmd(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn arc")
}

/// Run the binary and demand success.
fn arc_ok(dir: &Path, args: &[&str]) -> Output {
    let out = arc_cmd(dir, args);
    assert!(
        out.status.success(),
        "arc {args:?} failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Run the binary, demand refusal, and return its stderr — the reason.
fn arc_refused(dir: &Path, args: &[&str]) -> String {
    let out = arc_cmd(dir, args);
    assert!(
        !out.status.success(),
        "arc {args:?} should have been refused:\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

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

// -------------------------------------------------------- authoring end to end

/// From nothing to a running pipeline and through an amendment, with no tool
/// in the loop but the binary: create, author the steps, run, amend, run.
#[test]
fn a_protocol_is_authored_and_amended_through_the_binary_alone() {
    let base = tempfile::tempdir().expect("tempdir");
    let proto = base.path().join("fieldbook");

    // Create: a fresh spec, no steps yet.
    arc_ok(base.path(), &["create-protocol", "fieldbook"]);
    assert!(proto.join(MANIFEST_FILENAME).is_file());

    // Author: give the empty `steps: []` its first block item, then grow it.
    arc_ok(
        base.path(),
        &[
            "edit-protocol",
            "--dir",
            "fieldbook",
            "replace",
            "steps",
            "\n  - name: gather\n    command: \"echo sun,4 > field.csv\"\n    produces: [field_csv]",
        ],
    );
    arc_ok(
        base.path(),
        &[
            "edit-protocol",
            "--dir",
            "fieldbook",
            "append",
            "steps",
            "  - name: tally\n    command: \"wc -l < field.csv | tr -d ' ' > tally.txt\"\n    depends_on: [field_csv]",
        ],
    );

    // Run: the authored protocol executes, in dependency order.
    arc_ok(&proto, &["run"]);
    let field = std::fs::read_to_string(proto.join("field.csv")).unwrap();
    assert_eq!(field.trim(), "sun,4");
    let tally = std::fs::read_to_string(proto.join("tally.txt")).unwrap();
    assert_eq!(tally.trim(), "1");

    // Amend: three characters inside a command, and a new key on a step —
    // still nothing but the binary.
    arc_ok(
        base.path(),
        &[
            "edit-protocol",
            "--dir",
            "fieldbook",
            "rewrite",
            "steps[0].command",
            "sun,4",
            "sun,9",
        ],
    );
    arc_ok(
        base.path(),
        &[
            "edit-protocol",
            "--dir",
            "fieldbook",
            "add",
            "steps[1]",
            "timeout_sec",
            "30",
        ],
    );

    // Re-run: the amendments reached the pipeline.
    arc_ok(&proto, &["run"]);
    let field = std::fs::read_to_string(proto.join("field.csv")).unwrap();
    assert_eq!(field.trim(), "sun,9", "the amendment is what ran");
}

// ------------------------------------------------- one write path, not two

/// The CLI and the library are one write path: the same edit, expressed once
/// as argv and once as a value, produces byte-identical documents — comments,
/// key order, trailing same-line comments, the blank line inside a block
/// scalar, all preserved the same way, because the same code preserved them.
#[test]
fn cli_editing_matches_the_library_path_byte_for_byte() {
    let original = corpus();

    // A scalar with a trailing same-line comment two characters to its right.
    let oracle = apply_edits(
        &original,
        &[SpecEdit::Replace {
            path: vec!["params".into(), "port".into(), "default".into()],
            value: "\"hobart\"".into(),
        }],
    )
    .expect("the library applies the edit");

    let dir = corpus_copy();
    arc_ok(
        dir.path(),
        &[
            "edit-protocol",
            "replace",
            "params.port.default",
            "\"hobart\"",
        ],
    );
    let on_disk = std::fs::read_to_string(dir.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(on_disk, oracle.text(), "one write path, not two");

    // A fragment inside a multi-line block scalar with an interior blank line.
    let oracle = apply_edits(
        &original,
        &[SpecEdit::RewriteFragment {
            path: vec!["steps".into(), 0.into(), "command".into()],
            from: "3,dover,5.9".into(),
            to: "3,dover,6.2".into(),
        }],
    )
    .expect("the fragment is unique");

    let dir = corpus_copy();
    arc_ok(
        dir.path(),
        &[
            "edit-protocol",
            "rewrite",
            "steps[0].command",
            "3,dover,5.9",
            "3,dover,6.2",
        ],
    );
    let on_disk = std::fs::read_to_string(dir.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(on_disk, oracle.text());

    // And against the original directly: the only difference is the fragment
    // itself — no verb reformatted anything as a side effect.
    assert_eq!(
        on_disk,
        original.replacen("3,dover,5.9", "3,dover,6.2", 1),
        "every untargeted byte is identical"
    );
}

// ------------------------------------------------- refusal, before the disk

/// The refuse-with-a-reason gate, reached from argv: an edit that cannot
/// apply, or whose result would not load, exits nonzero, names the defect on
/// stderr, and leaves the spec byte-identical on disk.
#[test]
fn an_invalid_edit_is_refused_with_the_reason_and_the_file_untouched() {
    let dir = corpus_copy();
    let before = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();

    // Valid YAML, invalid spec: a duplicate step name. The loader's reason
    // reaches the user.
    let stderr = arc_refused(
        dir.path(),
        &["edit-protocol", "replace", "steps[1].name", "tide_table"],
    );
    assert!(
        stderr.contains("duplicate step name"),
        "the refusal names the defect: {stderr}"
    );

    // A target that does not resolve names its path.
    let stderr = arc_refused(
        dir.path(),
        &["edit-protocol", "replace", "steps[9].command", "\"true\""],
    );
    assert!(stderr.contains("steps[9].command"), "{stderr}");

    // A path that does not even parse is refused in argument handling.
    let stderr = arc_refused(dir.path(), &["edit-protocol", "delete", "steps[x]"]);
    assert!(stderr.contains("not a number"), "{stderr}");

    let after = std::fs::read(dir.path().join(MANIFEST_FILENAME)).unwrap();
    assert_eq!(
        after, before,
        "a refused edit leaves the spec untouched, byte for byte"
    );
}

/// Creation goes through the same gate and the same refusals: an existing
/// spec is never overwritten, and an invalid manifest never becomes a file.
#[test]
fn create_refuses_an_existing_spec_and_an_invalid_manifest() {
    let base = tempfile::tempdir().expect("tempdir");

    arc_ok(base.path(), &["create-protocol", "notes"]);
    let spec = base.path().join("notes").join(MANIFEST_FILENAME);
    let before = std::fs::read(&spec).unwrap();

    // A second create refuses: the file may have been hand-edited since.
    let stderr = arc_refused(base.path(), &["create-protocol", "notes"]);
    assert!(stderr.contains("already exists"), "{stderr}");
    assert_eq!(
        std::fs::read(&spec).unwrap(),
        before,
        "the refusal left the existing spec alone"
    );

    // The gate runs before anything exists: an empty name is refused and no
    // directory or spec appears.
    let stderr = arc_refused(base.path(), &["create-protocol", "blank", "--name", ""]);
    assert!(stderr.contains("name cannot be empty"), "{stderr}");
    assert!(
        !base.path().join("blank").exists(),
        "a refused create leaves nothing behind"
    );
}
