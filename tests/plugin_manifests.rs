//! The agent plugin manifests are generated, and the committed copies must match.
//!
//! Each agent reads its own manifest format (`.claude-plugin/`, `.codex-plugin/`,
//! `.cursor-plugin/`, `gemini-extension.json`), but none of them is the source: every one is
//! written by `scripts/generate-plugins.py` from `plugin/plugin.meta.json`, with the version
//! taken from `Cargo.toml`. This test runs the generator into a temporary folder and compares
//! what it writes, byte for byte, with the files committed at the repository root, so a
//! manifest edited by hand, or a version bump without a regenerate, fails here with the file
//! to edit and the command to run.
//!
//! It needs `python3` (3.11 or later, for `tomllib`) on `PATH`, as CI's runners have.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = "plugin/plugin.meta.json";
const REGENERATE: &str = "python3 scripts/generate-plugins.py";

/// The manifests each agent looks for. The generator may write more, but never fewer.
const EXPECTED: &[&str] = &[
    ".claude-plugin/plugin.json",
    ".claude-plugin/marketplace.json",
    ".codex-plugin/plugin.json",
    ".cursor-plugin/plugin.json",
    "gemini-extension.json",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every file under `root`, hidden ones included, by its `/`-separated path relative to `root`.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                let rel = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn manifests_match_source() {
    let root = repo_root();
    let out = tempfile::tempdir().unwrap();
    let run = Command::new("python3")
        .arg(root.join("scripts/generate-plugins.py"))
        .arg("--out")
        .arg(out.path())
        .current_dir(&root)
        .output()
        .expect("run python3: the plugin manifest test needs python3 (3.11 or later) on PATH");
    assert!(
        run.status.success(),
        "scripts/generate-plugins.py failed (code {:?}):\nstdout:\n{}\nstderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let generated = tree(out.path());
    for path in EXPECTED {
        assert!(
            generated.contains_key(*path),
            "scripts/generate-plugins.py did not write {path}; it wrote {:?}",
            generated.keys().collect::<Vec<_>>()
        );
    }

    let mut drift = Vec::new();
    for (path, want) in &generated {
        match fs::read(root.join(path)) {
            Ok(have) if &have == want => {}
            Ok(_) => drift.push(format!("  {path} differs from what {SOURCE} generates")),
            Err(_) => drift.push(format!("  {path} is missing")),
        }
    }
    assert!(
        drift.is_empty(),
        "The agent plugin manifests are generated from {SOURCE} (and the version from Cargo.toml), \
         and the committed ones have drifted:\n{}\n\nDo not edit them by hand. Edit {SOURCE}, \
         run `{REGENERATE}` from the repository root, and commit what it writes.",
        drift.join("\n")
    );
}
