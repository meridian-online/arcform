//! Embeds the agent skills under `skills/` into the binary, so `arc skills install` writes the
//! skills of the arc version that is running rather than whatever happens to be on disk.
//!
//! Only skill folders are embedded: each directory directly under `skills/` that holds a
//! `SKILL.md`, with every file beneath it except hidden ones (a stray `.DS_Store`). The
//! maintainer's `skills/README.md` is not a skill and stays out. The result is
//! `$OUT_DIR/skills.rs`, a `static SKILLS: &[(&str, &[u8])]` of `<skill>/<path>` and bytes,
//! sorted by path, which `src/skills.rs` includes.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("skills");
    // A directory here makes Cargo rerun this script when anything beneath it changes.
    println!("cargo:rerun-if-changed=skills");

    let mut files = Vec::new();
    if root.is_dir() {
        let mut skills: Vec<PathBuf> = fs::read_dir(&root)
            .expect("read skills/")
            .map(|e| e.expect("read skills/ entry").path())
            .filter(|p| p.join("SKILL.md").is_file() && !hidden(p))
            .collect();
        skills.sort();
        for skill in skills {
            walk(&root, &skill, &mut files);
        }
    }
    files.sort();

    let mut out = String::from("pub static SKILLS: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &files {
        out.push_str(&format!("    ({rel:?}, include_bytes!({abs:?})),\n"));
    }
    out.push_str("];\n");

    let dest = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("skills.rs");
    fs::write(dest, out).expect("write skills.rs");
}

fn hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

/// Every non-hidden file beneath `dir`, as (path relative to `root` with `/` separators,
/// absolute path).
fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).expect("read a skill folder") {
        let path = entry.expect("read a skill folder entry").path();
        if hidden(&path) {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, files);
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("under skills/")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((rel, path.to_string_lossy().into_owned()));
        }
    }
}
