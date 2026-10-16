//! `arc skills install` and `arc skills list`: the analyst skills under `skills/` ship inside
//! the binary and are written into an agent's skills folder at the binary's version.
//!
//! Every test runs the real `arc` binary with `HOME` set to a temporary folder, so nothing
//! here reads or writes the developer's own home.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The folder of this crate's `skills/`.
fn skills_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
}

/// Run `arc` with `home` as its home directory, from inside `home`.
fn arc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arc"))
        .current_dir(home)
        .env("HOME", home)
        .env_remove("USERPROFILE")
        .args(args)
        .output()
        .expect("spawn arc")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "arc failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// The skill folder names under `skills/`: each directory holding a `SKILL.md`.
fn skill_names() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(skills_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("SKILL.md").is_file())
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every file under `root`, by its path relative to `root`, with its bytes. Hidden files
/// (such as a stray `.DS_Store`) are left out, as the embedding leaves them out.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy();
                out.insert(rel.replace('\\', "/"), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// What `skills/` holds of the skill folders alone (not the maintainer's README beside them).
fn skill_files() -> BTreeMap<String, Vec<u8>> {
    let names = skill_names();
    tree(&skills_dir())
        .into_iter()
        .filter(|(rel, _)| names.iter().any(|n| rel.starts_with(&format!("{n}/"))))
        .collect()
}

#[test]
fn installs_into_each_detected_agent() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();
    fs::create_dir(home.path().join(".gemini")).unwrap();

    let out = arc(home.path(), &["skills", "install"]);
    assert_ok(&out);
    let stdout = text(&out.stdout);

    for agent_dir in [".claude", ".gemini"] {
        let skills = home.path().join(agent_dir).join("skills");
        for name in skill_names() {
            let folder = skills.join(&name);
            assert!(
                folder.join("SKILL.md").is_file(),
                "{} missing",
                folder.display()
            );
            let marker = fs::read_to_string(folder.join(".arc-skill")).unwrap();
            assert!(
                marker.contains(env!("CARGO_PKG_VERSION")),
                "the marker records the arc version: {marker}"
            );
            assert!(
                stdout.contains(&folder.display().to_string()),
                "each folder written is printed; {} is not in:\n{stdout}",
                folder.display()
            );
        }
    }
    // An agent whose config folder is absent is left alone.
    assert!(!home.path().join(".cursor").exists());
    assert!(!home.path().join(".agents").exists());
}

#[test]
fn refuses_user_folder_of_same_name() {
    let home = tempfile::tempdir().unwrap();
    let mine = home.path().join(".claude/skills/using-arcform");
    fs::create_dir_all(&mine).unwrap();
    fs::write(mine.join("SKILL.md"), "my own skill\n").unwrap();

    let out = arc(home.path(), &["skills", "install"]);
    assert!(!out.status.success(), "a user's folder must be refused");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains(&mine.display().to_string()) && stderr.contains(".arc-skill"),
        "the refusal names the folder and why: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(mine.join("SKILL.md")).unwrap(),
        "my own skill\n",
        "the user's skill is untouched"
    );
    // Nothing is written when any folder is refused: a partial set of skills that name
    // each other is worse than none.
    for name in skill_names().iter().filter(|n| *n != "using-arcform") {
        assert!(!home.path().join(".claude/skills").join(name).exists());
    }
}

#[test]
fn replaces_older_install_and_drops_removed_files() {
    let home = tempfile::tempdir().unwrap();
    let folder = home.path().join(".claude/skills/using-arcform");
    fs::create_dir_all(folder.join("references")).unwrap();
    fs::write(folder.join(".arc-skill"), "arc 0.0.1\n").unwrap();
    fs::write(folder.join("SKILL.md"), "an older arc's skill\n").unwrap();
    fs::write(
        folder.join("references/dropped.md"),
        "gone in this version\n",
    )
    .unwrap();

    let out = arc(home.path(), &["skills", "install"]);
    assert_ok(&out);

    assert!(
        !folder.join("references/dropped.md").exists(),
        "a file the new version dropped is removed"
    );
    assert_eq!(
        fs::read(folder.join("SKILL.md")).unwrap(),
        fs::read(skills_dir().join("using-arcform/SKILL.md")).unwrap()
    );
    let marker = fs::read_to_string(folder.join(".arc-skill")).unwrap();
    assert!(marker.contains(env!("CARGO_PKG_VERSION")), "{marker}");
    assert!(
        text(&out.stdout).contains("0.0.1"),
        "the replaced version is reported:\n{}",
        text(&out.stdout)
    );
}

#[test]
fn no_agent_detected_prints_targets_and_exits_1() {
    let home = tempfile::tempdir().unwrap();

    let out = arc(home.path(), &["skills", "install"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    for dir in [
        ".claude/skills",
        ".cursor/skills",
        ".gemini/skills",
        ".agents/skills",
    ] {
        assert!(stderr.contains(dir), "{dir} is not named in:\n{stderr}");
    }
    assert!(
        stderr.contains("--agent") && stderr.contains("--target"),
        "says how to name a target:\n{stderr}"
    );
    assert_eq!(
        fs::read_dir(home.path()).unwrap().count(),
        0,
        "nothing is written"
    );
}

#[test]
fn agent_flag_installs_for_an_undetected_agent() {
    let home = tempfile::tempdir().unwrap();

    let out = arc(home.path(), &["skills", "install", "--agent", "cursor"]);
    assert_ok(&out);
    assert!(
        home.path()
            .join(".cursor/skills/using-arcform/SKILL.md")
            .is_file()
    );
}

#[test]
fn target_flag_installs_there() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir(home.path().join(".claude")).unwrap();
    let target = home.path().join("elsewhere");

    let out = arc(
        home.path(),
        &["skills", "install", "--target", target.to_str().unwrap()],
    );
    assert_ok(&out);
    for name in skill_names() {
        assert!(target.join(&name).join("SKILL.md").is_file());
    }
    assert!(
        !home.path().join(".claude/skills").exists(),
        "a named target replaces detection"
    );
}

#[test]
fn embedded_skills_match_skills_dir() {
    let home = tempfile::tempdir().unwrap();
    let target = home.path().join("out");

    let out = arc(
        home.path(),
        &["skills", "install", "--target", target.to_str().unwrap()],
    );
    assert_ok(&out);

    let installed = tree(&target);
    let expected = skill_files();
    assert_eq!(
        installed.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "the embedded files are the files of the skill folders under skills/"
    );
    for (rel, bytes) in &expected {
        assert_eq!(&installed[rel], bytes, "{rel} differs from skills/{rel}");
    }
    for name in skill_names() {
        assert!(target.join(&name).join(".arc-skill").is_file());
    }
}

#[test]
fn list_prints_each_skill_and_its_arc_version() {
    let home = tempfile::tempdir().unwrap();
    let out = arc(home.path(), &["skills", "list"]);
    assert_ok(&out);
    let stdout = text(&out.stdout);
    for name in skill_names() {
        let line = stdout
            .lines()
            .find(|l| l.split_whitespace().next() == Some(name.as_str()))
            .unwrap_or_else(|| panic!("{name} not listed:\n{stdout}"));
        assert!(line.contains(env!("CARGO_PKG_VERSION")), "{line}");
    }
}
