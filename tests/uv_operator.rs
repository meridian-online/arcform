//! `op: uv@1` — the operator that lets a Protocol run its OWN Python script inside
//! the asset graph, driven through the real `arc` binary.
//!
//! WHAT NEEDS A REAL `uv` AND WHAT DOES NOT, because the split is the point.
//!
//! Three of the four properties under test are arcform's own and are proven on every
//! runner, with no `uv` and no network: the two refusals at manifest load
//! (`reads:`/`produces:` absent, a dependency left unpinned), the digest pin (which
//! refuses BEFORE anything is spawned), and the asset-graph edge — a declared read
//! marking the step stale while an undeclared file does not. The graph edge runs
//! against a STUB `uv` placed on PATH, which is also a second and independent pin on
//! the argv: the stub refuses unless every `--with` precedes `--script`, so an argv
//! that reordered would fail here as well as in the unit test.
//!
//! One property is genuinely `uv`'s: that the step can import a third-party package
//! the host `python3` does not have. That test is `#[ignore]`d, in this repo's
//! convention for a test the routine gate cannot satisfy from its own tree — and
//! `ci.yml`'s `build` job installs `uv` and runs this file with `--include-ignored`,
//! so it executes on every PR rather than waiting for a schedule. `require_uv`
//! panics rather than skipping if it is missing at that point, so the gate cannot
//! pass by running nothing.

use std::path::Path;
use std::process::Command;

mod common;
use common::step_outcome;

/// The digest a `uv@1` step has to pin.
fn sha256_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Is `uv` on PATH?
fn have_uv() -> bool {
    Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `uv` on PATH, or a hard failure. Only called from a test carrying `#[ignore]` —
/// reaching this line means the run asked for the ignored tests, so a missing `uv`
/// is the run being wrong rather than the machine being ordinary.
fn require_uv() {
    assert!(
        have_uv(),
        "`uv` must be on PATH: this test is #[ignore]d precisely so an ordinary \
         machine skips it, and running it with --include-ignored is a claim that \
         `uv` is staged."
    );
}

/// A stand-in for `uv` that asserts the invocation shape and then runs the script
/// under the host `python3`. It cannot install anything, which is why the test that
/// needs a third-party import uses the real one.
///
/// The three refusals are the argv contract read from the outside: `run` first,
/// every `--with` before `--script`, then the script path, then the script's own
/// arguments. Reorder any of it and this exits 64 with the reason on stderr.
const STUB_UV: &str = r#"#!/bin/sh
set -eu
[ "${1:-}" = "run" ] || { echo "stub uv: expected 'run', got '${1:-}'" >&2; exit 64; }
shift
while [ "${1:-}" = "--with" ]; do
  [ $# -ge 2 ] || { echo "stub uv: --with with no requirement" >&2; exit 64; }
  shift 2
done
[ "${1:-}" = "--script" ] || { echo "stub uv: expected --script after the --with flags, got '${1:-}'" >&2; exit 64; }
shift
script="$1"; shift
exec python3 "$script" "$@"
"#;

/// A Protocol directory holding one `uv@1` step, its script, and one declared input.
/// `extra` is spliced into the step's `with:` block, so each test states only what it
/// changes.
///
/// The script is pure standard library and writes a file whose bytes are a function
/// of its input, so a run that did not happen is visible as an unchanged output.
fn protocol(dir: &Path, extra: &str) -> String {
    let script = r#"import json, sys
src, dest = sys.argv[1], sys.argv[2]
text = open(src).read()
json.dump({"chars": len(text), "text": text}, open(dest, "w"))
"#;
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(dir.join("scripts/derive.py"), script).unwrap();
    std::fs::write(dir.join("data/input.txt"), "one\n").unwrap();
    // Never named by the step. The other half of the staleness claim needs a file
    // that changes and must NOT count.
    std::fs::write(dir.join("data/unread.txt"), "ignored\n").unwrap();
    let digest = sha256_of(script.as_bytes());
    let manifest = format!(
        r#"name: uv_operator_fixture
engine: duckdb
db: build/fixture.duckdb

steps:
  - name: derive
    op: uv@1
    with:
      script: scripts/derive.py
      sha256: "{digest}"
      args: ["data/input.txt", "build/derived.json"]
      reads: ["data/input.txt"]
      produces: ["build/derived.json"]
{extra}"#
    );
    std::fs::write(dir.join("arcform.yaml"), &manifest).unwrap();
    digest
}

/// One `arc run` with a stub `uv` on PATH, returning (exit code, stdout, stderr).
fn arc_run_stubbed(project: &Path) -> (Option<i32>, String, String) {
    let bin = project.join(".stub-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let uv = bin.join("uv");
    std::fs::write(&uv, STUB_UV).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .env("PATH", path)
        .arg("run")
        .output()
        .expect("spawn arc run");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Replace a line of the fixture manifest. Each refusal test mutates the ONE field it
/// is about, from a manifest that is otherwise known to run.
fn edit_manifest(dir: &Path, from: &str, to: &str) {
    let path = dir.join("arcform.yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains(from),
        "the fixture no longer carries `{from}`, so this mutation is testing nothing"
    );
    std::fs::write(&path, text.replace(from, to)).unwrap();
}

// ─────────────────────────────────────────────────────────────────────────────
// The refusals. Every one runs on an ordinary machine.
// ─────────────────────────────────────────────────────────────────────────────

/// A `uv@1` step that declares no `reads:` cannot be loaded, and the engine names
/// the step and the field. This is the property that separates the operator from a
/// `command:` step, so it is refused rather than warned about.
#[test]
fn a_step_that_declares_no_reads_is_refused_at_manifest_load() {
    let tmp = tempfile::tempdir().unwrap();
    protocol(tmp.path(), "");
    edit_manifest(tmp.path(), "      reads: [\"data/input.txt\"]\n", "");

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    let all = format!("{stdout}{stderr}");
    assert_ne!(code, Some(0), "the run must not proceed:\n{all}");
    assert!(all.contains("derive"), "the refusal names the step: {all}");
    assert!(
        all.contains("`reads:` declares nothing"),
        "and the field: {all}"
    );
    assert!(
        !tmp.path().join("build/derived.json").exists(),
        "nothing may run before the manifest is accepted"
    );
}

/// The same for `produces:`.
#[test]
fn a_step_that_declares_no_produces_is_refused_at_manifest_load() {
    let tmp = tempfile::tempdir().unwrap();
    protocol(tmp.path(), "");
    edit_manifest(tmp.path(), "      produces: [\"build/derived.json\"]\n", "");

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    let all = format!("{stdout}{stderr}");
    assert_ne!(code, Some(0), "the run must not proceed:\n{all}");
    assert!(
        all.contains("derive") && all.contains("`produces:` declares nothing"),
        "the refusal names the step and the field: {all}"
    );
}

/// An unpinned dependency is refused at load, quoting the entry. A range would let
/// two Runs a month apart install different code with nothing in the manifest, the
/// digest or the graph reading as changed.
#[test]
fn an_unpinned_dependency_is_refused_at_manifest_load() {
    let tmp = tempfile::tempdir().unwrap();
    protocol(tmp.path(), "      deps: [\"tomli_w>=1.0\"]\n");

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    let all = format!("{stdout}{stderr}");
    assert_ne!(code, Some(0), "the run must not proceed:\n{all}");
    assert!(
        all.contains("tomli_w>=1.0") && all.contains("package==version"),
        "the refusal quotes the entry and states the pinned form: {all}"
    );
}

/// A script whose bytes differ from the pin fails the Run, naming the step, the
/// digest expected and the digest found. Nothing is spawned — the check is before
/// `uv`, which is why this needs no `uv` to prove.
#[test]
fn a_script_that_does_not_match_its_pin_fails_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let pinned = protocol(tmp.path(), "");
    let edited = "import sys\nopen(sys.argv[2], 'w').write('{}')\n";
    std::fs::write(tmp.path().join("scripts/derive.py"), edited).unwrap();

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    let all = format!("{stdout}{stderr}");
    assert_ne!(code, Some(0), "the run must fail:\n{all}");
    assert!(all.contains("derive"), "names the step: {all}");
    assert!(all.contains(&pinned), "names the digest pinned: {all}");
    assert!(
        all.contains(&sha256_of(edited.as_bytes())),
        "names the digest found, so re-pinning is a copy-paste: {all}"
    );
    assert!(
        !tmp.path().join("build/derived.json").exists(),
        "an unpinned script must not run at all"
    );
}

/// The hole the digest cannot close, driven through the real binary.
///
/// `uv run --script` reads the script's OWN PEP 723 header and installs what it names,
/// on top of every `--with`. So this manifest declares no `deps:` at all and pins its
/// script byte-for-byte, and before the check existed `arc run` installed an unpinned
/// package and exited 0 — because the unpinned declaration is part of the bytes the
/// digest pins.
///
/// It needs no `uv`: the refusal is before the spawn, which is the point.
#[test]
fn a_dependency_the_script_declares_for_itself_is_pinned_too() {
    let tmp = tempfile::tempdir().unwrap();
    protocol(tmp.path(), "");
    let script = "# /// script\n# requires-python = \">=3.12\"\n# dependencies = [\n#   \"iniconfig\",\n# ]\n# ///\nimport json, sys\nsrc, dest = sys.argv[1], sys.argv[2]\njson.dump({\"chars\": len(open(src).read())}, open(dest, \"w\"))\n";
    std::fs::write(tmp.path().join("scripts/derive.py"), script).unwrap();
    edit_manifest(
        tmp.path(),
        &std::fs::read_to_string(tmp.path().join("arcform.yaml"))
            .unwrap()
            .lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix("sha256: \"")
                    .map(|d| d.trim_end_matches('"').to_string())
            })
            .unwrap(),
        &sha256_of(script.as_bytes()),
    );

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    let all = format!("{stdout}{stderr}");
    assert_ne!(code, Some(0), "the run must be refused:\n{all}");
    assert!(all.contains("iniconfig"), "names the entry: {all}");
    assert!(
        all.contains("PEP 723"),
        "and says where it was declared, since the manifest declares no deps at all: {all}"
    );
    assert!(
        !tmp.path().join("build/derived.json").exists(),
        "nothing may run with an unpinned dependency reachable"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The asset-graph edge, in both directions.
// ─────────────────────────────────────────────────────────────────────────────

/// The claim this operator exists to make: a change to a file the step DECLARES
/// marks it stale, and a change to a file it does not declare leaves it alone.
///
/// Driven in one test rather than two because the two halves are only meaningful
/// against each other — a step that re-runs on everything satisfies the first half
/// and is exactly as useless as a step that re-runs on nothing.
#[test]
fn a_declared_read_marks_the_step_stale_and_an_undeclared_file_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    protocol(tmp.path(), "");
    let out = tmp.path().join("build/derived.json");

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    assert_eq!(code, Some(0), "first run:\nstdout:\n{stdout}\n{stderr}");
    assert_eq!(step_outcome(&stdout, "derive"), "ran");
    let first = std::fs::read_to_string(&out).unwrap();
    assert!(first.contains("one"), "the script really ran: {first}");

    // Nothing changed.
    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    assert_eq!(code, Some(0), "second run:\nstdout:\n{stdout}\n{stderr}");
    assert_eq!(
        step_outcome(&stdout, "derive"),
        "skip: hash_clean",
        "an unchanged tree must not re-run the step"
    );

    // A file the step does not name. This is the half a `command:` step cannot get
    // wrong only because it re-runs unconditionally.
    std::fs::write(tmp.path().join("data/unread.txt"), "changed\n").unwrap();
    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    assert_eq!(code, Some(0), "third run:\nstdout:\n{stdout}\n{stderr}");
    assert_eq!(
        step_outcome(&stdout, "derive"),
        "skip: hash_clean",
        "a file outside `reads:` must not mark the step stale"
    );

    // A file the step DOES name.
    std::fs::write(tmp.path().join("data/input.txt"), "one two three\n").unwrap();
    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    assert_eq!(code, Some(0), "fourth run:\nstdout:\n{stdout}\n{stderr}");
    assert_eq!(
        step_outcome(&stdout, "derive"),
        "ran",
        "a changed declared read must mark the step stale"
    );
    let second = std::fs::read_to_string(&out).unwrap();
    assert!(
        second.contains("one two three"),
        "and the re-run must have written the new bytes: {second}"
    );
}

/// The argv, asserted from OUTSIDE the process that builds it. The stub refuses
/// unless `run` comes first, every `--with` precedes `--script`, and the script's own
/// arguments follow the path — so this is the one test on the routine gate that would
/// catch a reordering, and it is why the stub carries those three refusals at all.
///
/// The declared dependencies are deliberately not imported by the script: the stub
/// cannot install anything, and what is under test here is where they land in the
/// argv, not what they provide. That they provide anything at all is
/// `a_uv_step_imports_a_package_the_host_python_does_not_have`, with a real `uv`.
#[test]
fn every_declared_dependency_reaches_uv_before_the_script_path() {
    let tmp = tempfile::tempdir().unwrap();
    protocol(
        tmp.path(),
        "      deps: [\"tomli_w==1.2.0\", \"iniconfig==2.0.0\"]\n",
    );

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    let all = format!("{stdout}{stderr}");
    assert_eq!(code, Some(0), "the stub accepted the invocation:\n{all}");
    assert!(
        !all.contains("stub uv:"),
        "the stub refuses an argv it does not recognise, and it refused: {all}"
    );
    assert_eq!(step_outcome(&stdout, "derive"), "ran");
    assert!(
        tmp.path().join("build/derived.json").exists(),
        "the script ran with its arguments intact"
    );
}

/// Re-pinning the digest is itself what marks the step stale, which is what makes
/// the accepted cost — editing a `.py` file is a manifest edit too — also the
/// mechanism. Without this the script would be the one input a Run could not see.
#[test]
fn re_pinning_an_edited_script_marks_the_step_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let pinned = protocol(tmp.path(), "");

    let (code, stdout, _) = arc_run_stubbed(tmp.path());
    assert_eq!(code, Some(0));
    assert_eq!(step_outcome(&stdout, "derive"), "ran");

    let edited = r#"import json, sys
src, dest = sys.argv[1], sys.argv[2]
json.dump({"chars": len(open(src).read()), "marker": "second version"}, open(dest, "w"))
"#;
    std::fs::write(tmp.path().join("scripts/derive.py"), edited).unwrap();
    edit_manifest(tmp.path(), &pinned, &sha256_of(edited.as_bytes()));

    let (code, stdout, stderr) = arc_run_stubbed(tmp.path());
    assert_eq!(code, Some(0), "after re-pinning:\n{stdout}\n{stderr}");
    assert_eq!(
        step_outcome(&stdout, "derive"),
        "ran",
        "a re-pinned digest is a changed `with:` block, so the step is stale"
    );
    let text = std::fs::read_to_string(tmp.path().join("build/derived.json")).unwrap();
    assert!(
        text.contains("second version"),
        "the new script ran: {text}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The half that is genuinely `uv`'s.
// ─────────────────────────────────────────────────────────────────────────────

/// The reason this operator exists at all: a Protocol's Python step gets the
/// packages it declares, whether or not the machine running it has them.
///
/// `tomli_w` is chosen because it is pure Python, has no dependencies of its own,
/// and is not in any base image's `python3` — so the assertion that the host cannot
/// import it is checked here rather than assumed, and the test says so if it is
/// wrong.
#[test]
#[ignore = "needs `uv` — run under ci.yml's build job, which installs it and passes \
            --include-ignored"]
fn a_uv_step_imports_a_package_the_host_python_does_not_have() {
    require_uv();
    let tmp = tempfile::tempdir().unwrap();

    let script = r#"import sys, tomli_w
open(sys.argv[1], "wb").write(tomli_w.dumps({"ok": True}).encode())
"#;
    std::fs::create_dir_all(tmp.path().join("scripts")).unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();
    std::fs::create_dir_all(tmp.path().join("build")).unwrap();
    std::fs::write(tmp.path().join("scripts/emit.py"), script).unwrap();
    std::fs::write(tmp.path().join("data/input.txt"), "one\n").unwrap();

    // The claim the test rests on, checked rather than assumed.
    let host = Command::new("python3")
        .args(["-c", "import tomli_w"])
        .output()
        .expect("spawn python3");
    assert!(
        !host.status.success(),
        "this test proves `uv` supplies a package the host lacks, and this host \
         already has tomli_w — pick a package it does not have"
    );

    std::fs::write(
        tmp.path().join("arcform.yaml"),
        format!(
            r#"name: uv_operator_deps
engine: duckdb
db: build/fixture.duckdb

steps:
  - name: emit
    op: uv@1
    with:
      script: scripts/emit.py
      sha256: "{}"
      deps: ["tomli_w==1.2.0"]
      args: ["build/emitted.toml"]
      reads: ["data/input.txt"]
      produces: ["build/emitted.toml"]
"#,
            sha256_of(script.as_bytes())
        ),
    )
    .unwrap();

    common::arc_run(tmp.path());
    let emitted = std::fs::read_to_string(tmp.path().join("build/emitted.toml"))
        .expect("the step wrote its declared output");
    assert_eq!(emitted.trim(), "ok = true");
}
