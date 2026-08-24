//! End-to-end coverage for the native-Rust `datapackage_describe` operator, driven
//! through the REAL `arc` binary via a FAKE `finetype` on an isolated PATH — so
//! these need neither network nor a real finetype/uv install and run the same in
//! CI as on a developer machine.
//!
//! AC3 (no Python runtime) is the point of `describes_with_no_python_on_path`:
//! PATH resolves to the fake `finetype` and the real `duckdb` CLI ONLY — no
//! interpreter and no `uv` exist anywhere on it. The retired uv-run substrate
//! would fail to spawn under this PATH; the operator succeeds because it now
//! talks to `finetype` directly.
//!
//! AC2 (override precedence) is `override_wins_generated_fills_the_rest`: a
//! curated field key wins over finetype's value for that SAME key, and finetype's
//! value for every OTHER key on that field survives untouched.

use std::path::{Path, PathBuf};
use std::process::Command;

fn base_profile_json() -> &'static str {
    r#"{"name":"widgets","resources":[{"name":"widgets","path":"widgets.parquet","schema":{"fields":[{"name":"id","type":"integer","x-finetype-label":"identifier"},{"name":"note","type":"string","x-finetype-label":"representation.text.plain_text"}]}}]}"#
}

/// Write an executable `finetype` into `dir` that answers BOTH subcommands the
/// operator calls: `--version` (the floor gate) and `profile -f … -o datapackage`
/// (the machine-decidable half). `#!/bin/sh` + builtins only — this fake needs no
/// Python either, which is the whole point of the test it serves.
fn write_fake_finetype(dir: &Path, version: &str, profile_json: &str) {
    let script = dir.join("finetype");
    let quoted = format!("'{}'", profile_json.replace('\'', "'\\''"));
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo \"finetype {version}\"\nelse\n  echo {quoted}\nfi\n"
    );
    std::fs::write(&script, body).expect("write fake finetype");
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(&script).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&script, perm).unwrap();
}

/// Write an executable `finetype` whose `--version` answers normally but whose
/// `profile` subcommand FAILS (nonzero exit, stderr message) — proves a profile
/// failure propagates as a step failure rather than being silently swallowed into
/// a default/null base descriptor.
fn write_fake_finetype_profile_fails(dir: &Path, version: &str) {
    let script = dir.join("finetype");
    let body = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo \"finetype {version}\"\nelse\n  echo 'finetype: could not read parquet' >&2\n  exit 3\nfi\n"
    );
    std::fs::write(&script, body).expect("write fake finetype");
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(&script).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&script, perm).unwrap();
}

/// Write an executable `finetype` whose `--version` answers with `version_line`
/// VERBATIM (not necessarily a parseable dotted version) — for pinning the
/// unparseable-output refusal end to end, through a real `arc run`.
fn write_fake_finetype_version_only(dir: &Path, version_line: &str) {
    let script = dir.join("finetype");
    let body = format!("#!/bin/sh\necho \"{version_line}\"\n");
    std::fs::write(&script, body).expect("write fake finetype");
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(&script).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&script, perm).unwrap();
}

/// The directory holding the real `duckdb` CLI, found on THIS process's ambient
/// PATH. `arc run` always needs one (the same requirement `init_from_descriptor.rs`
/// states) independent of this operator, so the isolated PATH built for these
/// tests carries it through rather than excluding it along with everything else.
fn duckdb_dir() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("duckdb");
        if candidate.is_file() {
            return dir;
        }
    }
    panic!("no `duckdb` CLI found on PATH — required by every `arc run`, independent of this test");
}

/// A minimal Protocol: one `describe` step, a placeholder Parquet (never opened by
/// the fake finetype), and the given overrides sidecar.
fn write_project(project: &Path, overrides_json: &str) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::write(
        project.join("widgets.parquet"),
        b"not a real parquet file -- the fake finetype never opens it",
    )
    .unwrap();
    std::fs::write(project.join("descriptor.overrides.json"), overrides_json).unwrap();
    std::fs::write(
        project.join("arcform.yaml"),
        "name: describe_test\n\
         engine: duckdb\n\
         db: build/test.db\n\
         steps:\n\
        \x20 - name: describe\n\
        \x20   op: datapackage_describe@1\n\
        \x20   with:\n\
        \x20     parquet: widgets.parquet\n\
        \x20     overrides: descriptor.overrides.json\n\
        \x20     out: datapackage.json\n",
    )
    .unwrap();
}

/// Same fixture as `write_project`, but the manifest also pins
/// `expect_finetype_version` — the ONLY way `run()`'s `require_exact_finetype_version`
/// call is reached at all (no test drove this end to end before: the manifest field
/// existed and was unit-tested in isolation, but nothing wired it through a real
/// `arc run`).
fn write_project_with_expect_version(project: &Path, overrides_json: &str, expect_version: &str) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::write(
        project.join("widgets.parquet"),
        b"not a real parquet file -- the fake finetype never opens it",
    )
    .unwrap();
    std::fs::write(project.join("descriptor.overrides.json"), overrides_json).unwrap();
    std::fs::write(
        project.join("arcform.yaml"),
        format!(
            "name: describe_test\n\
             engine: duckdb\n\
             db: build/test.db\n\
             steps:\n\
            \x20 - name: describe\n\
            \x20   op: datapackage_describe@1\n\
            \x20   with:\n\
            \x20     parquet: widgets.parquet\n\
            \x20     overrides: descriptor.overrides.json\n\
            \x20     out: datapackage.json\n\
            \x20     expect_finetype_version: \"{expect_version}\"\n"
        ),
    )
    .unwrap();
}

/// Run `arc run` in `project` with PATH resolving ONLY to `finetype_bin` (the
/// fake) and the real `duckdb`'s directory — no other directory, so nothing else
/// on the host's PATH (a real `finetype`, a Python interpreter, `uv`) can be
/// resolved by accident.
fn run_arc_with_fake_finetype(project: &Path, finetype_bin: &Path) -> std::process::Output {
    let isolated_path = std::env::join_paths([finetype_bin.to_path_buf(), duckdb_dir()]).unwrap();
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(project)
        .arg("run")
        .env("PATH", isolated_path)
        .output()
        .expect("spawn arc run")
}

#[test]
fn describes_with_no_python_on_path() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(&project, r#"{"title": "Widgets"}"#);

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        out.status.success(),
        "arc run failed with no Python on PATH:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let bytes = std::fs::read(project.join("datapackage.json")).unwrap();
    let descriptor: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(descriptor["title"], "Widgets");
    // The resolved binary's version is what gets stamped, not a constant.
    assert_eq!(descriptor["x-finetype-version"], "9.9.9");
    // `json.dump(..., indent=2, sort_keys=True)` parity: two-space indent, a
    // trailing newline, and every object's keys in sorted order.
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.ends_with("}\n"), "must end with `}}\\n`: {text:?}");
    assert!(text.contains("\n  \"name\""), "keys are two-space indented");
}

#[test]
fn override_wins_generated_fills_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(
        &project,
        r#"{"fields": {"note": {"description": "a curated note"}}}"#,
    );

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let descriptor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.join("datapackage.json")).unwrap()).unwrap();
    let fields = descriptor["resources"][0]["schema"]["fields"]
        .as_array()
        .unwrap();
    let note = fields.iter().find(|f| f["name"] == "note").unwrap();
    // The override wins for the key it names.
    assert_eq!(note["description"], "a curated note");
    // finetype's value for every OTHER key on that field survives — the override
    // is a shallow per-key replace, not a wipe of the whole field.
    assert_eq!(note["x-finetype-label"], "representation.text.plain_text");
    // A field the sidecar never mentions is untouched too.
    let id = fields.iter().find(|f| f["name"] == "id").unwrap();
    assert_eq!(id["x-finetype-label"], "identifier");
}

#[test]
fn primary_key_naming_a_missing_column_stops_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(&project, r#"{"primaryKey": ["not_a_real_column"]}"#);

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        !out.status.success(),
        "a curated primaryKey naming a column absent from the Parquet must stop the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not_a_real_column"), "stderr: {stderr}");
    assert!(
        !project.join("datapackage.json").exists(),
        "a refused descriptor must not be written"
    );
}

#[test]
fn stale_finetype_below_the_floor_is_refused() {
    // 0.6.41 is the real stale-engine version that once shipped wrong labels —
    // MIN_FINETYPE_VERSION (0.6.54) must sit above it.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(&project, "{}");

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "0.6.41", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        !out.status.success(),
        "a finetype below the floor must be refused before it types anything"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("0.6.41"), "stderr: {stderr}");
    assert!(stderr.contains("older"), "stderr: {stderr}");
    assert!(
        !project.join("datapackage.json").exists(),
        "a refused run must not write a descriptor"
    );
}

#[test]
fn missing_finetype_is_refused() {
    // PATH resolves to the real `duckdb` only — no `finetype` anywhere on it.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(&project, "{}");

    let empty_bin = tempfile::tempdir().unwrap();
    let out = run_arc_with_fake_finetype(&project, empty_bin.path());
    assert!(
        !out.status.success(),
        "a missing `finetype` binary must stop the step, not silently skip typing"
    );
}

#[test]
fn stale_field_override_warns_but_does_not_fail() {
    // A sidecar field override naming a column absent from the Parquet is a stale
    // entry — it warns (stderr), it does not stop the run. Regression-proofs the
    // warning branch: silently dropping this check would not break the descriptor,
    // only the operator's ability to say "this sidecar entry is dead, clean it up".
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(
        &project,
        r#"{"fields": {"ghost_column": {"description": "does not exist"}}}"#,
    );

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        out.status.success(),
        "a stale field override must warn, not fail:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("WARNING"), "stderr: {stderr}");
    assert!(stderr.contains("ghost_column"), "stderr: {stderr}");
    assert!(
        project.join("datapackage.json").exists(),
        "a merely-stale override must not stop the descriptor being written"
    );
}

#[test]
fn resource_path_override_wins_end_to_end() {
    // Every production descriptor.overrides.json sets `resource.path` to the
    // public URL, because finetype writes the local build path. If this merge
    // regressed, every published descriptor would point consumers at a path that
    // does not resolve on their machine.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(
        &project,
        r#"{"resource": {"path": "https://openlake.meridian.online/widgets.parquet"}}"#,
    );

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let descriptor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.join("datapackage.json")).unwrap()).unwrap();
    assert_eq!(
        descriptor["resources"][0]["path"],
        "https://openlake.meridian.online/widgets.parquet"
    );
    // A resource key the override did not mention survives from finetype's half.
    assert_eq!(descriptor["resources"][0]["name"], "widgets");
}

#[test]
fn finetype_profile_failure_is_not_silently_swallowed() {
    // `--version` succeeds (passes the floor gate) but `profile` itself fails —
    // must stop the run rather than continue with a default/null base descriptor.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(&project, "{}");

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype_profile_fails(finetype_dir.path(), "9.9.9");

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        !out.status.success(),
        "a failing `finetype profile` must stop the run, not silently continue"
    );
    // Pinned to the REAL child stderr, not just "something failed" — a fold to
    // `.unwrap_or_default()` would still fail downstream (merge_datapackage refuses
    // a non-object base), but with a DIFFERENT message that never mentions this.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not read parquet"),
        "the real finetype failure must be the reason given: {stderr}"
    );
    assert!(
        !project.join("datapackage.json").exists(),
        "a swallowed profile failure would still write a (wrong) descriptor; \
         a propagated one must not write anything"
    );
}

#[test]
fn missing_overrides_file_is_refused() {
    // The overrides sidecar itself is absent — the read must propagate as a step
    // failure, not panic, not silently describe with no curation at all.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(&project, "{}");
    std::fs::remove_file(project.join("descriptor.overrides.json")).unwrap();

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        !out.status.success(),
        "a missing overrides sidecar must stop the run"
    );
    // Pinned to the READ failing, not just "something failed downstream" — a fold
    // to `.unwrap_or_default()` (empty string) would still fail at the NEXT line
    // (invalid JSON), but through a different message that never says "read".
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("datapackage_describe: read"),
        "must fail at the read, not one step later at the parse: {stderr}"
    );
    assert!(!project.join("datapackage.json").exists());
}

#[test]
fn unparseable_finetype_version_is_refused_end_to_end() {
    // `require_finetype`'s `resolve_finetype_version(&stdout)?` folded to
    // `.unwrap_or_default()` still fails downstream (a (0,0,0) resolved version
    // trips the floor gate) — so this pins the run failing for the RIGHT reason:
    // the specific "could not parse" message, not just "something failed".
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(&project, "{}");

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype_version_only(finetype_dir.path(), "finetype (unknown build)");

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        !out.status.success(),
        "an unparseable `finetype --version` must stop the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("could not parse"), "stderr: {stderr}");
    assert!(!project.join("datapackage.json").exists());
}

#[test]
fn field_override_for_a_present_column_does_not_warn() {
    // The stale-override warning fires ONLY for a name absent from the Parquet.
    // Overriding a column that IS present (`note`, from base_profile_json) must
    // produce no "WARNING" at all — the other half of the guard
    // `stale_field_override_warns_but_does_not_fail` alone cannot pin, since a
    // guard forced to fire unconditionally still warns correctly for an absent
    // name.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project(
        &project,
        r#"{"fields": {"note": {"description": "a real column"}}}"#,
    );

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("WARNING"),
        "overriding a column that exists must not warn: {stderr}"
    );
}

#[test]
fn expect_finetype_version_matching_runs_and_stamps() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project_with_expect_version(&project, "{}", "9.9.9");

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let descriptor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.join("datapackage.json")).unwrap()).unwrap();
    assert_eq!(descriptor["x-finetype-version"], "9.9.9");
}

#[test]
fn expect_finetype_version_mismatch_refuses_end_to_end() {
    // The manifest pins an exact release the resolved binary does not report —
    // must refuse, naming both versions, before anything is written. This is the
    // ONLY path that reaches `run()`'s `require_exact_finetype_version(...)?call —
    // no prior test wired `expect_finetype_version` through a real `arc run`.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    write_project_with_expect_version(&project, "{}", "9.9.8");

    let finetype_dir = tempfile::tempdir().unwrap();
    write_fake_finetype(finetype_dir.path(), "9.9.9", base_profile_json());

    let out = run_arc_with_fake_finetype(&project, finetype_dir.path());
    assert!(
        !out.status.success(),
        "a resolved version that does not match the pin must be refused"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("9.9.9"), "stderr: {stderr}");
    assert!(stderr.contains("9.9.8"), "stderr: {stderr}");
    assert!(!project.join("datapackage.json").exists());
}
