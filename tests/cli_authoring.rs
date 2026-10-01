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

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arc::spec::{MANIFEST_FILENAME, SpecEdit, apply_edits};

/// Run the real `arc` binary with `args` in `dir`. The spawned binary gets a
/// test-scoped local-history root and data folder: authoring commands record
/// history, and a run of a Protocol that names no `db:` keeps its database, as
/// they do in production, but a test run must never write into the
/// developer's real `~/.arcform`.
fn arc_cmd(dir: &Path, args: &[&str]) -> Output {
    arc_cmd_on_path(dir, args, None)
}

/// [`arc_cmd`], with `PATH` set to `path` for the spawned binary when one is given.
fn arc_cmd_on_path(dir: &Path, args: &[&str], path: Option<&OsStr>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_arc"));
    cmd.current_dir(dir)
        .env(
            "ARCFORM_HISTORY_DIR",
            std::env::temp_dir().join("arc-cli-authoring-history"),
        )
        .env(
            "ARCFORM_DB_DIR",
            std::env::temp_dir().join("arc-cli-authoring-db"),
        )
        .args(args);
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    cmd.output().expect("spawn arc")
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

// ------------------------------------------------- where the database lives

/// The lines of an `arcform.yaml` that begin `db`, the way an author reading the file
/// sees them.
fn db_lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(MANIFEST_FILENAME))
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("db"))
        .map(str::to_string)
        .collect()
}

/// The first step of a Protocol, as `edit-protocol replace steps` takes it.
const FIRST_STEP: &str =
    "\n  - name: gather\n    command: \"echo sun,4 > field.csv\"\n    produces: [field_csv]";

/// `create-protocol` names a database in the manifest it writes only when `--db` names
/// one. A manifest with none runs with no `<name>.duckdb` beside it — its database is in
/// arc's data folder, which `tests/working_database.rs` pins — and one with a path runs
/// against that path and builds no other.
#[test]
fn create_protocol_writes_db_only_when_asked_and_run_builds_it_where_it_says() {
    let base = tempfile::tempdir().expect("tempdir");

    // No --db: no line, and the first run builds no `<name>.duckdb` beside the manifest.
    arc_ok(base.path(), &["create-protocol", "fieldbook"]);
    let unnamed = base.path().join("fieldbook");
    assert_eq!(db_lines(&unnamed), Vec::<String>::new(), "no db line");
    assert!(
        !unnamed.join("fieldbook.duckdb").exists(),
        "creating a Protocol builds no database"
    );
    arc_ok(
        base.path(),
        &[
            "edit-protocol",
            "--dir",
            "fieldbook",
            "replace",
            "steps",
            FIRST_STEP,
        ],
    );
    assert_eq!(db_lines(&unnamed), Vec::<String>::new(), "still no db line");
    arc_ok(&unnamed, &["run"]);
    assert!(
        !unnamed.join("fieldbook.duckdb").exists(),
        "a manifest with no db builds no <name>.duckdb beside it"
    );

    // --db: the line is written, and the run builds that file and not `<name>.duckdb`.
    arc_ok(
        base.path(),
        &["create-protocol", "kept", "--db", "work.duckdb"],
    );
    let named = base.path().join("kept");
    assert_eq!(db_lines(&named), vec!["db: work.duckdb"]);
    arc_ok(
        base.path(),
        &[
            "edit-protocol",
            "--dir",
            "kept",
            "replace",
            "steps",
            FIRST_STEP,
        ],
    );
    arc_ok(&named, &["run"]);
    assert!(named.join("work.duckdb").is_file(), "the named database");
    assert!(
        !named.join("kept.duckdb").exists(),
        "and not the default beside it"
    );
}

/// A manifest an earlier arc wrote, carrying `db: null`, loads with its database unset,
/// and one carrying a path loads with that path. `edit-protocol` on either leaves the
/// `db` line, and every byte above the key it edits, as it was.
#[test]
fn edit_protocol_leaves_a_db_line_as_it_was_and_the_protocol_runs_where_it_says() {
    // (the db line, the file a run builds beside the manifest, the file it must not
    // build there). `db: null` is unset, so its database is in arc's data folder.
    let cases = [
        ("db: null", None, ["p.duckdb", "kept.duckdb"]),
        (
            "db: kept.duckdb",
            Some("kept.duckdb"),
            ["p.duckdb", "p.duckdb"],
        ),
    ];
    for (db_line, built, not_built) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let head = format!(
            "# Written by an earlier arc.\nname: p\nengine: duckdb\nengine_version: '>=1.0'\n{db_line}\nparams: {{}}\ndotenv: []\n"
        );
        let original = format!("{head}steps: []\nassets: {{}}\n");
        std::fs::write(dir.path().join(MANIFEST_FILENAME), &original).unwrap();

        arc_ok(
            dir.path(),
            &["edit-protocol", "replace", "steps", FIRST_STEP],
        );

        let edited = std::fs::read_to_string(dir.path().join(MANIFEST_FILENAME)).unwrap();
        assert!(
            edited.starts_with(&head),
            "{db_line}: every byte above the edited key is as it was:\n{edited}"
        );
        assert!(
            edited.contains("name: gather"),
            "the edit applied:\n{edited}"
        );

        arc_ok(dir.path(), &["run"]);
        if let Some(built) = built {
            assert!(
                dir.path().join(built).is_file(),
                "{db_line}: builds {built}"
            );
        }
        for not_built in not_built {
            assert!(
                !dir.path().join(not_built).exists(),
                "{db_line}: builds no {not_built}"
            );
        }
    }
}

// -------------------------------------------------------------- the ignore list
//
// The directories `create-protocol` and `init` make carry a `.gitignore` beside the
// manifest. These tests ask real `git` what `git add --all` stages, because the
// property is what a person's `git add` does, and a test of the list's text alone
// would pass over a list that git reads differently from the way it was written.

/// What `create-protocol` and `init` write when no `--db` puts a database inside the
/// directory.
const IGNORE_LIST: &str = "# Written by `arc`: what a run records belongs to the machine that ran it.\n/build/.arcform/\n";

/// A step that generates a model file, the way a Protocol's first step might.
const MODEL_STEP: &str = "\n  - name: generate\n    command: \"mkdir -p models && echo 'SELECT 1 AS n' > models/gen.sql\"";

/// `git` in `dir`, with the developer's own configuration out of reach. A global
/// ignore file that names `*.duckdb` would make a database look as though arc's list
/// had excluded it, and a test that passed for that reason would pass whatever arc wrote.
fn git(dir: &Path, args: &[&str]) -> String {
    let home = std::env::temp_dir().join("arc-cli-authoring-git-home");
    let out = Command::new("git")
        .current_dir(dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("spawn git: these tests read what `git add --all` stages");
    assert!(
        out.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The paths `git add --all` has staged in `dir` (a repository is made there if there
/// is none), sorted. Staging is cumulative, so a call after more files appear lists
/// what the earlier call staged too.
fn staged_by_add_all(dir: &Path) -> Vec<String> {
    git(dir, &["init", "-q"]);
    git(dir, &["add", "--all"]);
    let listing = git(dir, &["ls-files", "--cached", "-z"]);
    let mut paths: Vec<String> = listing
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    paths.sort();
    paths
}

/// `create-protocol` with no `--db`: `git add --all` stages the manifest and the list;
/// after a first step is authored and a run completes, it stages the model the step
/// generated and nothing arc recorded under `build/.arcform/`.
#[test]
fn a_fresh_protocol_stages_the_manifest_and_not_arcs_run_records() {
    let base = tempfile::tempdir().expect("tempdir");
    arc_ok(base.path(), &["create-protocol", "fieldbook"]);
    let proto = base.path().join("fieldbook");

    assert_eq!(
        std::fs::read_to_string(proto.join(".gitignore")).unwrap(),
        IGNORE_LIST
    );
    assert_eq!(
        staged_by_add_all(&proto),
        [".gitignore", "arcform.yaml"],
        "a fresh Protocol stages the manifest and the list"
    );

    arc_ok(
        base.path(),
        &[
            "edit-protocol",
            "--dir",
            "fieldbook",
            "replace",
            "steps",
            MODEL_STEP,
        ],
    );
    arc_ok(&proto, &["run"]);
    let runs = proto.join("build/.arcform/runs");
    assert!(
        std::fs::read_dir(&runs).is_ok_and(|mut records| records.next().is_some()),
        "the run wrote its records under {}, so the check below has something to exclude",
        runs.display()
    );

    let staged = staged_by_add_all(&proto);
    assert_eq!(
        staged,
        [".gitignore", "arcform.yaml", "models/gen.sql"],
        "the generated model is staged and no run record is"
    );
}

/// A `--db` that puts the database inside the directory is named in the list with its
/// write-ahead log, wherever in the directory it sits and however its name reads to
/// `git`; one that puts it outside is not named, since there is no file in the
/// directory to stage.
#[test]
fn a_database_inside_the_directory_is_not_staged_and_one_outside_is_not_named() {
    // The control: a directory with no list stages a database and its log. Without it
    // the assertions below could pass for a reason that is not arc's list.
    let control = tempfile::tempdir().expect("tempdir");
    std::fs::write(control.path().join("work.duckdb"), b"db").unwrap();
    std::fs::write(control.path().join("work.duckdb.wal"), b"wal").unwrap();
    assert_eq!(
        staged_by_add_all(control.path()),
        ["work.duckdb", "work.duckdb.wal"],
        "this git stages a database nothing ignores"
    );

    for db in [
        "work.duckdb",
        "build/work.duckdb",
        "./build/../scratch/work.duckdb",
        "my work.duckdb",
        "work[1].duckdb",
    ] {
        let base = tempfile::tempdir().expect("tempdir");
        arc_ok(base.path(), &["create-protocol", "kept", "--db", db]);
        let proto = base.path().join("kept");
        arc_ok(
            base.path(),
            &[
                "edit-protocol",
                "--dir",
                "kept",
                "replace",
                "steps",
                FIRST_STEP,
            ],
        );
        arc_ok(&proto, &["run"]);
        assert!(proto.join(db).is_file(), "{db}: the run built the database");
        // The log exists only while DuckDB has writes to replay; put one there so the
        // check is on a file that is.
        let wal = proto.join(format!("{db}.wal"));
        std::fs::write(&wal, b"wal").unwrap();

        let staged = staged_by_add_all(&proto);
        assert!(
            staged
                .iter()
                .all(|p| !p.ends_with(".wal") && !p.ends_with(".duckdb")),
            "{db}: staged a database or its log: {staged:?}"
        );
        assert!(
            staged.contains(&"arcform.yaml".to_string())
                && staged.contains(&"field.csv".to_string()),
            "{db}: the rest of the Protocol is staged: {staged:?}"
        );
    }

    // Outside the directory: nothing of it is in the directory, and the list does not
    // name it.
    let base = tempfile::tempdir().expect("tempdir");
    arc_ok(
        base.path(),
        &["create-protocol", "away", "--db", "../elsewhere.duckdb"],
    );
    assert_eq!(
        std::fs::read_to_string(base.path().join("away/.gitignore")).unwrap(),
        IGNORE_LIST,
        "a database outside the directory is not in the list"
    );
}

/// `arc init` writes the same list the other verb does, and the lines it prints naming
/// what it made name it.
#[test]
fn init_writes_the_same_list_and_says_so() {
    let base = tempfile::tempdir().expect("tempdir");
    let out = arc_ok(base.path(), &["init", "scaffold"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l.trim() == ".gitignore"),
        "init names the list among what it made:\n{stdout}"
    );

    let proto = base.path().join("scaffold");
    assert_eq!(
        std::fs::read_to_string(proto.join(".gitignore")).unwrap(),
        IGNORE_LIST
    );
    // `models/` and `sources/` are empty, which git does not stage.
    assert_eq!(staged_by_add_all(&proto), [".gitignore", "arcform.yaml"]);
}

/// A `.gitignore` the author wrote before `create-protocol` ran is as they left it,
/// to the byte. So is a name that is a link to nothing: it is not written through.
#[test]
fn a_gitignore_already_there_is_left_as_it_was() {
    let base = tempfile::tempdir().expect("tempdir");
    let proto = base.path().join("mine");
    std::fs::create_dir(&proto).unwrap();
    let theirs: &[u8] = b"*.log\r\nscratch/";
    std::fs::write(proto.join(".gitignore"), theirs).unwrap();

    let out = arc_ok(base.path(), &["create-protocol", "mine"]);
    assert_eq!(std::fs::read(proto.join(".gitignore")).unwrap(), theirs);
    assert!(
        proto.join(MANIFEST_FILENAME).is_file(),
        "the spec is written"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains(".gitignore"),
        "it does not say it made a list it did not make"
    );

    #[cfg(unix)]
    {
        let linked = base.path().join("linked");
        std::fs::create_dir(&linked).unwrap();
        let target = base.path().join("nowhere");
        std::os::unix::fs::symlink(&target, linked.join(".gitignore")).unwrap();
        arc_ok(base.path(), &["create-protocol", "linked"]);
        assert!(
            !target.exists(),
            "the list was not written through the link"
        );
        assert_eq!(
            std::fs::read_link(linked.join(".gitignore")).unwrap(),
            target
        );
    }
}

/// A create that is refused writes no list: the spec that is there may be hand-authored,
/// and the directory beside it is not arc's to add to.
#[test]
fn a_refused_create_writes_no_list() {
    let base = tempfile::tempdir().expect("tempdir");
    let proto = base.path().join("notes");
    std::fs::create_dir(&proto).unwrap();
    std::fs::write(proto.join(MANIFEST_FILENAME), "name: notes\nsteps: []\n").unwrap();

    arc_refused(base.path(), &["create-protocol", "notes"]);
    assert!(
        !proto.join(".gitignore").exists(),
        "a refused create left a list beside a spec it did not write"
    );
}

/// `arc history list` prints the retention policy it printed before the list existed,
/// word for word, and the word *git* in it is still the one claim arc makes about git.
#[test]
fn history_list_prints_the_policy_line_it_always_printed() {
    let base = tempfile::tempdir().expect("tempdir");
    arc_ok(base.path(), &["create-protocol", "notes"]);
    let out = arc_ok(base.path(), &["history", "list", "--dir", "notes"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let root = std::env::temp_dir().join("arc-cli-authoring-history");
    let expected = format!(
        "policy: keeps the last 50 states per spec (oldest pruned first); saves within 10s \
         of the newest save merge into it; stored outside the protocol at {}; never promoted \
         to git",
        root.display()
    );
    assert!(
        stdout.lines().any(|l| l == expected),
        "the policy line, byte for byte:\n{expected}\nin:\n{stdout}"
    );
}

/// What `create-protocol`, `edit-protocol`, `init` and `run` left behind.
#[derive(Debug, PartialEq)]
struct Outcome {
    /// What `create-protocol` and `init` printed.
    stdout: Vec<String>,
    /// Every file under the base outside a `build/.arcform/`, by path, with its bytes.
    /// A database file keeps its name and not its bytes, which DuckDB stamps.
    files: BTreeMap<String, Vec<u8>>,
    /// How many files the run wrote under `build/.arcform/`.
    run_records: usize,
}

fn tree(root: &Path, dir: &Path, into: &mut BTreeMap<String, Vec<u8>>, run_records: &mut usize) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let record = rel.contains("/build/.arcform");
        if record && path.is_file() {
            *run_records += 1;
        }
        if path.is_dir() {
            tree(root, &path, into, run_records);
        } else if !record {
            let keeps_bytes = !rel.ends_with(".duckdb") && !rel.ends_with(".wal");
            let bytes = if keeps_bytes {
                std::fs::read(&path).unwrap()
            } else {
                Vec::new()
            };
            into.insert(rel, bytes);
        }
    }
}

/// Create a Protocol with a database inside the directory, author a step, run it, and
/// `init` another, all with `PATH` as given.
fn author_and_run(path: Option<&OsStr>) -> Outcome {
    let base = tempfile::tempdir().expect("tempdir");
    let ok = |args: &[&str]| {
        let out = arc_cmd_on_path(base.path(), args, path);
        assert!(
            out.status.success(),
            "arc {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let mut stdout = vec![ok(&["create-protocol", "fieldbook", "--db", "work.duckdb"])];
    ok(&[
        "edit-protocol",
        "--dir",
        "fieldbook",
        "replace",
        "steps",
        FIRST_STEP,
    ]);
    stdout.push(ok(&["init", "scaffold"]));
    let out = arc_cmd_on_path(&base.path().join("fieldbook"), &["run"], path);
    assert!(
        out.status.success(),
        "arc run failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut files = BTreeMap::new();
    let mut run_records = 0;
    tree(base.path(), base.path(), &mut files, &mut run_records);
    Outcome {
        stdout,
        files,
        run_records,
    }
}

/// A `PATH` that holds the tools an `arc run` of a command step needs and no `git`.
#[cfg(unix)]
fn path_without_git() -> PathBuf {
    let bin = std::env::temp_dir().join("arc-cli-authoring-no-git");
    std::fs::create_dir_all(&bin).unwrap();
    let on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect::<Vec<_>>();
    for tool in ["sh", "duckdb"] {
        if let Some(found) = on_path.iter().map(|d| d.join(tool)).find(|p| p.is_file()) {
            // Another test may be linking the same name at the same moment.
            let _ = std::os::unix::fs::symlink(found, bin.join(tool));
        }
    }
    bin
}

/// arc looks for no repository and runs no `git`: the three verbs leave the same
/// directory, with the same bytes and the same printed lines, on a `PATH` that has no
/// `git` as on one that has.
#[cfg(unix)]
#[test]
fn create_init_and_run_give_the_same_result_with_no_git_on_path() {
    let bin = path_without_git();
    let finds_git = |path: Option<&OsStr>| {
        let mut probe = Command::new("sh");
        probe.args(["-c", "command -v git"]);
        if let Some(path) = path {
            probe.env("PATH", path);
        }
        probe.output().expect("spawn sh").status.success()
    };
    assert!(finds_git(None), "the ordinary PATH has a git");
    assert!(
        !finds_git(Some(bin.as_os_str())),
        "the PATH built for this test has none, or the comparison below compares a run with itself"
    );

    let with_git = author_and_run(None);
    let without_git = author_and_run(Some(bin.as_os_str()));
    assert!(with_git.run_records > 0, "the run wrote records");
    assert_eq!(with_git, without_git);
    assert!(
        with_git.files.contains_key("fieldbook/.gitignore")
            && with_git.files.contains_key("scaffold/.gitignore"),
        "both lists were written: {:?}",
        with_git.files.keys().collect::<Vec<_>>()
    );
}
