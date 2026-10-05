//! Two clones of a Protocol's repository that each record a version, and the merge
//! of their logs, asked of real `git` and the real `arc` binary.
//!
//!   1. **both lines kept** — the clones' merge has no conflict, and the merged log
//!      holds every line each clone had, once, and nothing else; the same when the
//!      shared history held no log yet and each clone created its own;
//!   2. **the rule does it** — with `.gitattributes` left out, the same two clones
//!      conflict on the log and on nothing else.
//!
//! The two clones edit different keys of the spec, so the spec itself merges and the
//! log is the only file a conflict could be on. `create-protocol` records the
//! Protocol's first version and `init` records none, which is the difference between
//! a shared history that holds a log and one that does not.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use arc::spec::{LOG_FILENAME, MANIFEST_FILENAME};

/// `git` in `dir` with the developer's own configuration out of reach, so a global
/// attributes file cannot make a merge succeed for a reason that is not arc's.
fn git_in(dir: &Path, args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::new("git")
        .current_dir(dir)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "arc test")
        .env("GIT_AUTHOR_EMAIL", "arc-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "arc test")
        .env("GIT_COMMITTER_EMAIL", "arc-test@example.invalid")
        .args(args)
        .output()
        .expect("spawn git")
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_in(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `arc` in `dir`, recording into `store`: each clone has a history store of its own,
/// as two machines would.
fn arc(dir: &Path, store: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(dir)
        .env("ARCFORM_HISTORY_DIR", store)
        .args(args)
        .output()
        .expect("spawn arc");
    assert!(
        out.status.success(),
        "arc {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A step that is not in a fresh Protocol, for one clone's edit.
const STEP: &str = "\n  - name: other\n    command: \"echo hi\"";

/// The log's lines in `dir`, in the order the file holds them.
fn log_lines(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join(LOG_FILENAME))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A shared repository and two clones of it, `first` and `second`, each of which has
/// recorded one edit and committed it. `first` has pushed; `second` has not pulled.
struct Clones {
    _root: tempfile::TempDir,
    first: PathBuf,
    second: PathBuf,
    /// The log's lines in the shared history, before either clone recorded.
    shared: Vec<String>,
}

/// How the shared history was made.
#[derive(Clone, Copy)]
struct Shared {
    /// The verb that made the Protocol: `create-protocol` records a first version and
    /// so leaves a log, `init` records nothing and leaves none.
    verb: &'static str,
    /// Whether the attributes file arc wrote stays in the shared history.
    keeps_the_rule: bool,
}

impl Clones {
    fn new(shared: Shared) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let base = root.path();
        let origin = base.join("origin.git");
        fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare", "-b", "main"]);

        // The Protocol the two people share: made once, committed, pushed.
        let seed = base.join("seed");
        arc(base, &base.join("seed-store"), &[shared.verb, "seed"]);
        let attributes = seed.join(".gitattributes");
        assert!(attributes.is_file(), "{} wrote the rule", shared.verb);
        if !shared.keeps_the_rule {
            fs::remove_file(&attributes).unwrap();
        }
        git(&seed, &["init", "-q", "-b", "main"]);
        git(&seed, &["add", "--all"]);
        git(&seed, &["commit", "-q", "-m", "a Protocol"]);
        git(
            &seed,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git(&seed, &["push", "-q", "origin", "main"]);

        let first = base.join("first");
        let second = base.join("second");
        git(base, &["clone", "-q", origin.to_str().unwrap(), "first"]);
        git(base, &["clone", "-q", origin.to_str().unwrap(), "second"]);
        let shared_lines = log_lines(&first);

        // Each records one edit, to a different key of the spec, and commits it.
        arc(
            &first,
            &base.join("first-store"),
            &["edit-protocol", "--dir", ".", "replace", "name", "alpha"],
        );
        git(&first, &["add", "--all"]);
        git(&first, &["commit", "-q", "-m", "first records"]);
        git(&first, &["push", "-q", "origin", "main"]);
        arc(
            &second,
            &base.join("second-store"),
            &["edit-protocol", "--dir", ".", "replace", "steps", STEP],
        );
        git(&second, &["add", "--all"]);
        git(&second, &["commit", "-q", "-m", "second records"]);

        Clones {
            _root: root,
            first,
            second,
            shared: shared_lines,
        }
    }

    /// What each clone added to the log, after the lines the shared history held.
    fn added(&self, clone: &Path) -> Vec<String> {
        let lines = log_lines(clone);
        assert_eq!(
            lines[..self.shared.len()],
            self.shared[..],
            "the clone's log leads with the shared history's lines"
        );
        lines[self.shared.len()..].to_vec()
    }

    /// `second` takes `first`'s commit, as a pull does.
    fn second_merges_first(&self) -> Output {
        git(&self.second, &["fetch", "-q", "origin"]);
        git_in(&self.second, &["merge", "--no-edit", "origin/main"])
    }
}

/// The merge of the two clones' logs keeps every line each clone had, once, and adds
/// nothing, whether the shared history held a log or each clone created its own.
#[test]
fn two_clones_that_each_record_a_version_merge_with_each_lines_kept() {
    for (case, verb, has_log) in [
        ("a shared log", "create-protocol", true),
        ("no shared log", "init", false),
    ] {
        let clones = Clones::new(Shared {
            verb,
            keeps_the_rule: true,
        });
        assert_eq!(
            !clones.shared.is_empty(),
            has_log,
            "{case}: the shared history is what the case says it is"
        );
        let first = clones.added(&clones.first);
        let second = clones.added(&clones.second);
        assert!(
            !first.is_empty() && !second.is_empty(),
            "{case}: each clone recorded a version"
        );

        let merge = clones.second_merges_first();
        assert!(
            merge.status.success(),
            "{case}: the merge conflicted:\n{}{}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );

        let merged = log_lines(&clones.second);
        let expected: Vec<&String> = clones.shared.iter().chain(&first).chain(&second).collect();
        for line in &expected {
            assert_eq!(
                merged.iter().filter(|l| l == line).count(),
                1,
                "{case}: a line is held once: {line}"
            );
        }
        assert_eq!(
            merged.len(),
            expected.len(),
            "{case}: the merged log holds those lines and nothing else:\n{merged:#?}"
        );
        // The spec merged too, so what the log held is not what let the merge pass.
        let spec = fs::read_to_string(clones.second.join(MANIFEST_FILENAME)).unwrap();
        assert!(
            spec.starts_with("name: alpha\n") && spec.contains("name: other"),
            "{case}: both clones' edits are in the merged spec:\n{spec}"
        );
    }
}

/// Without the attributes line the same two clones conflict on the log, and on
/// nothing else, which is what shows the line is what let the merge above pass.
#[test]
fn without_the_rule_the_same_two_clones_conflict_on_the_log() {
    let clones = Clones::new(Shared {
        verb: "create-protocol",
        keeps_the_rule: false,
    });
    assert!(
        !clones.added(&clones.first).is_empty() && !clones.added(&clones.second).is_empty(),
        "each clone recorded a version"
    );

    let merge = clones.second_merges_first();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );
    assert!(!merge.status.success(), "the merge conflicts:\n{said}");
    assert!(
        said.contains(&format!(
            "CONFLICT (content): Merge conflict in {LOG_FILENAME}"
        )),
        "git names the log:\n{said}"
    );
    let unmerged = git(&clones.second, &["diff", "--name-only", "--diff-filter=U"]);
    assert_eq!(
        unmerged.lines().collect::<Vec<_>>(),
        [LOG_FILENAME],
        "the log is the only file in conflict"
    );
}
