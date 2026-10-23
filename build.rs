//! Embeds the agent skills under `skills/` into the binary, so `arc skills install` writes the
//! skills of the arc version that is running rather than whatever happens to be on disk.
//!
//! Only skill folders are embedded: each directory directly under `skills/` that holds a
//! `SKILL.md`, with every file beneath it except hidden ones (a stray `.DS_Store`). The
//! maintainer's `skills/README.md` is not a skill and stays out. The result is
//! `$OUT_DIR/skills.rs`, a `static SKILLS: &[(&str, &[u8])]` of `<skill>/<path>` and bytes,
//! sorted by path, which `src/skills.rs` includes.
//!
//! It also passes one linker flag for `bundled-duckdb` on Linux; see `link_bundled_duckdb`.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    link_bundled_duckdb();

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

// With `bundled-duckdb`, libduckdb-sys links DuckDB's static-extension autoregister
// archive whole-archive, after libduckdb itself. That archive calls
// `duckdb_register_static_extensions`, which libduckdb defines and nothing before it
// references. GNU ld resolves archives in command-line order, so by the time it reads
// the call it has already passed over the definition, and the link fails with an
// undefined reference. rust-lld (the default linker for x86_64 Linux) and macOS's ld
// look back and link it; GNU ld, the default for aarch64 Linux, does not. Naming the
// symbol undefined up front makes GNU ld pull the definition in when it reads
// libduckdb. `-u` is harmless to a linker that needs no help. Applies to arc's own
// binaries and tests; a crate that links this library with `bundled-duckdb` on Linux
// needs the same flag until libduckdb-sys orders the archives itself.
fn link_bundled_duckdb() {
    let bundled = std::env::var_os("CARGO_FEATURE_BUNDLED_DUCKDB").is_some();
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if bundled && linux {
        println!("cargo::rustc-link-arg=-Wl,-u,duckdb_register_static_extensions");
    }
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
