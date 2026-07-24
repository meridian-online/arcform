//! The published surface, enumerated.
//!
//! `arc` exports a spec loader and nothing else. That is a promise, and promises made
//! only in prose get broken by a stray `pub` in an unrelated commit. This test reads
//! the source and fails when the surface changes — widening it is then a deliberate
//! act with a diff to argue about, not a side effect.
//!
//! # Why it reads every file, and reads them with a lexer
//!
//! Rust lets an inherent `impl Manifest { pub fn … }` sit in *any* module of the
//! crate, and a `pub fn` on an exported type is exported whether anyone meant it or
//! not. A scan that looks at a hand-picked list of modules, or that truncates each
//! file at its first `#[cfg(test)]`, misses both of those shapes — a helper added to
//! `Manifest` from an unrelated module, or a `pub fn` placed after an inline test
//! module, widens the API with the guard green.
//!
//! So: every `.rs` file under `src/` is scanned, and scanning goes through
//! `blank_non_code`, which blanks comments, string literals and char literals so that
//! `#[cfg(test)]` items can be removed by brace matching rather than by truncation.
//! `the_scanner_sees_through_the_shapes_that_would_fool_a_naive_scan` holds the
//! scanner itself to those two shapes.
//!
//! # What is checked
//!
//! 1. The set of module files is frozen — a new module is a deliberate act.
//! 2. The crate root exports `spec`, plus the doc-hidden `cli_main` under the `cli`
//!    feature; `Cargo.toml` gates the feature so `default-features = false` leaves
//!    `spec` alone at the root.
//! 3. `arc::spec` re-exports exactly the documented contract.
//! 4. Every `impl` block on an exported type lives in that type's own module — so
//!    inherent methods (and trait impls) cannot be bolted on from somewhere this
//!    file does not freeze.
//! 5. Those modules' `pub` items are frozen.
//! 6. No `use … as …` renames an exported type, which would slip an `impl` past (4).
//! 7. The schema types' derive lists are frozen — a derive on an exported type is
//!    public API too.
//! 8. `Error` stays `#[non_exhaustive]`.
//!
//! Struct *fields* are not frozen. The exported types are the spec schema, and the
//! schema is expected to grow; a new field is a spec change reviewed as a spec change.
//!
//! If a change here is intentional: update the lists, update the contract doc in
//! `src/spec.rs`, and treat it as a version-visible change to the crate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- the contract

/// Every type, constant and function `arc::spec` re-exports, and the module that
/// owns it. An `impl` on any of these names may only appear in its owning file.
const EXPORTED: [(&str, &str); 31] = [
    ("Manifest", "manifest.rs"),
    ("Step", "manifest.rs"),
    ("Param", "manifest.rs"),
    ("Defaults", "manifest.rs"),
    ("RetryPolicy", "manifest.rs"),
    ("Hooks", "manifest.rs"),
    ("AssetOverride", "manifest.rs"),
    ("MANIFEST_FILENAME", "manifest.rs"),
    ("Precondition", "precondition.rs"),
    ("ModifiedAfterConfig", "precondition.rs"),
    ("FreshConfig", "precondition.rs"),
    // The write path: an edit as a value, its application, and the gated result.
    ("PathPart", "edit.rs"),
    ("SpecEdit", "edit.rs"),
    ("ValidatedSpec", "edit.rs"),
    ("apply_edits", "edit.rs"),
    ("create_spec", "edit.rs"),
    ("edit_spec", "edit.rs"),
    // The record path: an exploration promoted into a step, and the ownership
    // marker that licenses regeneration of what the machine wrote.
    ("RecordedStep", "record.rs"),
    ("record_step", "record.rs"),
    ("amend_step_sql", "record.rs"),
    ("GENERATED_MARKER", "record.rs"),
    ("sql_is_generated", "record.rs"),
    // Local history: the middle tier between undo and version control, and
    // the checkpointed roads machine edits take through it.
    ("LocalHistory", "history.rs"),
    ("HistoryEntry", "history.rs"),
    ("HistoryKind", "history.rs"),
    ("HISTORY_MAX_ENTRIES", "history.rs"),
    ("HISTORY_MERGE_WINDOW", "history.rs"),
    ("edit_spec_with_history", "history.rs"),
    ("record_step_with_history", "history.rs"),
    ("Error", "error.rs"),
    ("Result", "error.rs"),
];

/// Every module file in the crate. Frozen so a new one cannot be added without
/// passing under the checks below.
const MODULE_FILES: [&str; 28] = [
    "asset.rs",
    "bridge.rs",
    "cli.rs",
    "contract.rs",
    "edit.rs",
    "engine.rs",
    "error.rs",
    "history.rs",
    "ingress_meta.rs",
    "introspect.rs",
    "lib.rs",
    "main.rs",
    "manifest.rs",
    // The `arc mcp` server (behind the `mcp` feature): a stdio JSON-RPC MCP
    // entry point that federates the FineType CLI and exposes protocol-run +
    // operator-describe. Private to the crate — it exports nothing at the root.
    "mcp/finetype.rs",
    "mcp/hero.rs",
    "mcp/mod.rs",
    "operator.rs",
    "precondition.rs",
    "record.rs",
    "registry/cache.rs",
    "registry/index.rs",
    "registry/mod.rs",
    "registry/resolve.rs",
    "registry/run.rs",
    "registry/transport.rs",
    "runner.rs",
    "spec.rs",
    "state.rs",
];

/// The derives on each exported schema type, frozen. A derive on an exported type is
/// public API — `Serialize` hands every consumer a round-trip, `PartialEq` a
/// comparison contract — so growing or shrinking a list here is a version-visible
/// change to the crate, reviewed like one.
///
/// `Serialize` is on the lists because *generated*-manifest emission — `create_spec`
/// in the write path, and `arc`'s own `arc init` scaffolding — serialises into a
/// place that has no manifest yet, the one case with no author bytes to lose. It is
/// explicitly **out of contract** for any other use — see the round-trip warning in
/// `src/spec.rs`: editing an existing spec goes through the write path, which splices
/// original bytes and never re-serialises. This freeze keeps any change to that
/// arrangement a deliberate, version-visible act instead of a silent drift.
const FROZEN_DERIVES: [(&str, &str, &[&str]); 18] = [
    (
        "manifest.rs",
        "Param",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "manifest.rs",
        "RetryPolicy",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "manifest.rs",
        "Defaults",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "manifest.rs",
        "Manifest",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "manifest.rs",
        "Step",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "manifest.rs",
        "Hooks",
        &["Debug", "Clone", "Default", "Serialize", "Deserialize"],
    ),
    (
        "manifest.rs",
        "AssetOverride",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "precondition.rs",
        "ModifiedAfterConfig",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "precondition.rs",
        "FreshConfig",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    (
        "precondition.rs",
        "Precondition",
        &["Debug", "Clone", "Serialize", "Deserialize"],
    ),
    // The edit values are plain data — comparable and cloneable so a caller can
    // build, inspect and log a proposed edit. Deliberately NOT serde types: an
    // edit travels inside a process, not over a wire, and keeping serde off the
    // list means no one can mistake it for a persistence format.
    (
        "edit.rs",
        "PathPart",
        &["Debug", "Clone", "PartialEq", "Eq"],
    ),
    (
        "edit.rs",
        "SpecEdit",
        &["Debug", "Clone", "PartialEq", "Eq"],
    ),
    ("edit.rs", "ValidatedSpec", &["Debug", "Clone"]),
    // Plain data like the edit values, and NOT serde types for the same
    // reason: a promotion travels inside a process, not over a wire.
    (
        "record.rs",
        "RecordedStep",
        &["Debug", "Clone", "PartialEq", "Eq"],
    ),
    // The history values are plain data like the edit values, and NOT serde
    // types for the same reason; the store handle is a root path, cloneable
    // so a tool can hold one per surface.
    (
        "history.rs",
        "HistoryKind",
        &["Debug", "Clone", "Copy", "PartialEq", "Eq"],
    ),
    (
        "history.rs",
        "HistoryEntry",
        &["Debug", "Clone", "PartialEq", "Eq"],
    ),
    ("history.rs", "LocalHistory", &["Debug", "Clone"]),
    ("error.rs", "Error", &["Debug", "thiserror::Error"]),
];

// ------------------------------------------------------------------- reading

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn src(file: &str) -> String {
    let path = src_root().join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every `.rs` file under `src/`, as a path relative to `src/`, sorted.
fn module_files() -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}"));
        for entry in entries {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(
                    path.strip_prefix(base)
                        .expect("under src/")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(&src_root(), &src_root(), &mut out);
    out.sort();
    out
}

// -------------------------------------------------------------- the scanner

/// A copy of `source` in which every byte belonging to a comment, a string literal
/// or a char literal is replaced by a space (newlines kept, so offsets and line
/// structure are preserved).
///
/// Everything else here scans the blanked copy and slices the original at the same
/// offsets. That is what makes brace matching trustworthy: a `{` inside a doc
/// comment or a YAML fixture string cannot throw the item boundaries off.
fn blank_non_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut out = bytes.to_vec();

    fn blank(out: &mut [u8], from: usize, to: usize) {
        let to = to.min(out.len());
        for b in out.iter_mut().take(to).skip(from) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }

    let ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0usize;
    while i < n {
        match bytes[i] {
            b'/' if i + 1 < n && bytes[i + 1] == b'/' => {
                let end = bytes[i..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(n, |k| i + k);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                let mut depth = 1usize;
                let mut j = i + 2;
                while j < n && depth > 0 {
                    if bytes[j] == b'/' && j + 1 < n && bytes[j + 1] == b'*' {
                        depth += 1;
                        j += 2;
                    } else if bytes[j] == b'*' && j + 1 < n && bytes[j + 1] == b'/' {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            }
            // Raw and byte strings: `r"…"`, `r#"…"#`, `b"…"`, `br#"…"#`.
            b'r' | b'b' if !(i > 0 && ident_byte(bytes[i - 1])) => {
                let raw = bytes[i] == b'r';
                let mut j = i + 1;
                if !raw && j < n && bytes[j] == b'r' {
                    j += 1;
                }
                let hash_start = j;
                while j < n && bytes[j] == b'#' {
                    j += 1;
                }
                let hashes = j - hash_start;
                if j < n && bytes[j] == b'"' {
                    let escapes = hashes == 0 && !raw && bytes[i] == b'b';
                    let mut k = j + 1;
                    let end = loop {
                        if k >= n {
                            break n;
                        }
                        if escapes && bytes[k] == b'\\' {
                            k += 2;
                            continue;
                        }
                        if bytes[k] == b'"'
                            && bytes[k + 1..].iter().take(hashes).all(|b| *b == b'#')
                        {
                            break (k + 1 + hashes).min(n);
                        }
                        k += 1;
                    };
                    blank(&mut out, i, end);
                    i = end;
                } else {
                    i += 1;
                }
            }
            b'"' => {
                let mut k = i + 1;
                while k < n {
                    match bytes[k] {
                        b'\\' => k += 2,
                        b'"' => break,
                        _ => k += 1,
                    }
                }
                blank(&mut out, i, (k + 1).min(n));
                i = (k + 1).min(n);
            }
            b'\'' => {
                // A char literal, or a lifetime. `'\n'` and `'a'` are literals;
                // `'a` on its own is a lifetime and must not open a "string".
                let is_char_literal =
                    (i + 1 < n && bytes[i + 1] == b'\\') || (i + 2 < n && bytes[i + 2] == b'\'');
                if is_char_literal {
                    let mut k = i + 1;
                    while k < n {
                        match bytes[k] {
                            b'\\' => k += 2,
                            b'\'' => break,
                            _ => k += 1,
                        }
                    }
                    blank(&mut out, i, (k + 1).min(n));
                    i = (k + 1).min(n);
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    String::from_utf8(out).expect("blanking only ever writes spaces over whole tokens")
}

/// `source` with every `#[cfg(test)]` item removed — the attribute and the item it
/// applies to, found by brace matching on the blanked copy rather than by chopping
/// the file at the first occurrence.
fn without_test_items(source: &str) -> String {
    const ATTR: &str = "#[cfg(test)]";
    let mut code = blank_non_code(source);
    let mut kept = source.to_string();

    while let Some(at) = code.find(ATTR) {
        let bytes = code.as_bytes();
        let mut i = at + ATTR.len();
        // Walk to the end of the attributed item: either `;` or a balanced `{…}`.
        let end = loop {
            if i >= bytes.len() {
                break bytes.len();
            }
            match bytes[i] {
                b';' => break i + 1,
                b'{' => {
                    let mut depth = 0usize;
                    let mut j = i;
                    while j < bytes.len() {
                        match bytes[j] {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    break (j + 1).min(bytes.len());
                }
                _ => i += 1,
            }
        };
        kept.replace_range(at..end, "");
        code.replace_range(at..end, "");
    }
    kept
}

const ITEM_KEYWORDS: [&str; 8] = [
    "fn", "struct", "enum", "type", "const", "trait", "mod", "union",
];

/// Names of `pub` item declarations (not fields, not `pub(crate)`, not test helpers).
fn pub_items(source: &str) -> BTreeSet<String> {
    let code = blank_non_code(&without_test_items(source));
    let mut items = BTreeSet::new();
    for line in code.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("pub ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        if !ITEM_KEYWORDS.contains(&keyword) {
            continue;
        }
        let Some(raw) = words.next() else { continue };
        let name: String = raw
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            items.insert(name);
        }
    }
    items
}

/// Names bound by `pub use` statements.
fn pub_use_names(source: &str) -> BTreeSet<String> {
    let body = blank_non_code(&without_test_items(source));
    let mut names = BTreeSet::new();
    let mut rest = body.as_str();
    while let Some(at) = rest.find("pub use ") {
        let tail = &rest[at + "pub use ".len()..];
        let end = tail.find(';').expect("a `pub use` must be terminated");
        let statement = &tail[..end];
        let listed = match (statement.find('{'), statement.rfind('}')) {
            (Some(open), Some(close)) => statement[open + 1..close].to_string(),
            _ => statement.rsplit("::").next().unwrap_or("").to_string(),
        };
        for name in listed.split(',') {
            let name = name.trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
        rest = &tail[end..];
    }
    names
}

/// The self type of every `impl` block in `source`: the name after `impl` for an
/// inherent impl, the name after `for` for a trait impl, in both cases reduced to
/// the last path segment with generics stripped.
fn impl_self_types(source: &str) -> Vec<String> {
    let body = blank_non_code(&without_test_items(source));
    let bytes = body.as_bytes();
    let mut found = Vec::new();
    let mut i = 0usize;
    while let Some(offset) = body[i..].find("impl") {
        let at = i + offset;
        i = at + 4;
        // `impl` must be the first token on its line, and a whole keyword.
        if !body[..at]
            .rsplit('\n')
            .next()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            continue;
        }
        if bytes
            .get(at + 4)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            continue;
        }
        let Some(brace) = body[at..].find('{') else {
            continue;
        };
        let header = &body[at + 4..at + brace];
        let header = header.split(" where ").next().unwrap_or(header);
        let header = strip_leading_generics(header);
        // A trait impl names the self type after `for`.
        let target = split_on_top_level_for(header).unwrap_or(header);
        let name = target
            .trim()
            .trim_start_matches('&')
            .split(['<', ' ', '\t'])
            .next()
            .unwrap_or("")
            .rsplit("::")
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !name.is_empty() {
            found.push(name);
        }
    }
    found
}

fn strip_leading_generics(header: &str) -> &str {
    let trimmed = header.trim_start();
    if !trimmed.starts_with('<') {
        return trimmed;
    }
    let mut depth = 0usize;
    for (idx, ch) in trimmed.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &trimmed[idx + 1..];
                }
            }
            _ => {}
        }
    }
    trimmed
}

/// The text after a top-level ` for ` — i.e. one not nested inside `<…>` or `(…)`.
fn split_on_top_level_for(header: &str) -> Option<&str> {
    let bytes = header.as_bytes();
    let mut depth = 0usize;
    for idx in 0..bytes.len() {
        match bytes[idx] {
            b'<' | b'(' => depth += 1,
            b'>' | b')' => depth = depth.saturating_sub(1),
            b'f' if depth == 0
                && header[idx..].starts_with("for ")
                && (idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric()) =>
            {
                return Some(&header[idx + 4..]);
            }
            _ => {}
        }
    }
    None
}

/// Every `#[derive(…)]` block that annotates a `pub struct` or `pub enum`, as
/// (type name, derive list) pairs in source order. Attributes between the derive
/// and the item (`#[serde(untagged)]`, `#[non_exhaustive]`, …) are skipped.
fn pub_type_derives(source: &str) -> Vec<(String, Vec<String>)> {
    let code = blank_non_code(&without_test_items(source));
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut i = 0usize;
    const MARK: &str = "#[derive(";
    while let Some(offset) = code[i..].find(MARK) {
        let start = i + offset + MARK.len();
        let Some(close) = code[start..].find(')') else {
            break;
        };
        let derives: Vec<String> = code[start..start + close]
            .split(',')
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        // Walk past `)]`, any further attributes, and whitespace to the item.
        let mut j = start + close + 2;
        loop {
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if code[j..].starts_with("#[") {
                let mut depth = 0usize;
                while j < bytes.len() {
                    match bytes[j] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
            } else {
                break;
            }
        }
        let rest = &code[j..];
        for keyword in ["pub struct ", "pub enum "] {
            if let Some(tail) = rest.strip_prefix(keyword) {
                let name: String = tail
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    found.push((name, derives.clone()));
                }
                break;
            }
        }
        i = start + close;
    }
    found
}

fn frozen(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

fn exported_names() -> BTreeSet<String> {
    EXPORTED
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect()
}

// --------------------------------------------------------------------- tests

/// The scanner's own regression test: the two shapes that defeat a naive scan — an
/// `impl` added from an unrelated module, and a `pub fn` after an inline test
/// module — must be visible to it.
#[test]
fn the_scanner_sees_through_the_shapes_that_would_fool_a_naive_scan() {
    // A `pub fn` sitting after an inline test module, mid-file.
    let after_test_module = "\
pub fn kept_before() {}
#[cfg(test)]
mod probe_helpers { pub fn helper() {} }
impl Manifest {
    pub fn probe_invisible(&self) -> usize { 0 }
}
pub fn kept_after() {}
";
    let items = pub_items(after_test_module);
    assert!(
        items.contains("kept_before") && items.contains("kept_after"),
        "items either side of an inline test module must both be seen: {items:?}"
    );
    assert!(
        items.contains("probe_invisible"),
        "a `pub fn` after a `#[cfg(test)]` item must be seen: {items:?}"
    );
    assert!(
        !items.contains("helper"),
        "items inside a `#[cfg(test)]` module are not surface: {items:?}"
    );
    assert_eq!(impl_self_types(after_test_module), ["Manifest"]);

    // Braces inside comments and string literals must not move item boundaries.
    let noisy = r####"
// } } } {
/// A doc comment with { unbalanced braces.
#[cfg(test)]
mod tests {
    const YAML: &str = "steps: [{ a: b }]";
    const RAW: &str = r#"{{{"#;
    fn brace() { let c = '{'; let _ = c; }
}
pub fn still_visible() {}
"####;
    assert!(
        pub_items(noisy).contains("still_visible"),
        "brace matching must ignore comments, strings and char literals"
    );

    // A trait impl's self type is the name after `for`, not the trait.
    assert_eq!(
        impl_self_types("impl serde::Serialize for Manifest {\n}\n"),
        ["Manifest"]
    );
    assert_eq!(
        impl_self_types("impl<T: Clone> Wrapper<T> {\n}\n"),
        ["Wrapper"]
    );
    assert_eq!(
        impl_self_types("impl crate::manifest::Manifest {\n}\n"),
        ["Manifest"]
    );

    // Derive lists are read through intervening attributes, and only pub types count.
    let derived = "\
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Precondition { A }
#[derive(Debug)]
struct Internal;
";
    assert_eq!(
        pub_type_derives(derived),
        [(
            "Precondition".to_string(),
            vec![
                "Debug".to_string(),
                "Clone".to_string(),
                "Serialize".to_string(),
                "Deserialize".to_string()
            ]
        )]
    );
}

/// The set of modules is frozen. Every check below walks this list, so a module
/// added without updating it would be a module nothing here looks at.
#[test]
fn the_module_list_is_frozen() {
    assert_eq!(
        module_files(),
        MODULE_FILES,
        "a new module under `src/` is not scanned by this file until it is listed \
         here. Add it, then re-read the checks below to see what it now has to obey."
    );
}

/// The crate root exports one module. Everything else is private, so nothing behind
/// it — the runner, the engine, introspection, the registry, the operator catalog —
/// is reachable from outside.
#[test]
fn the_crate_root_exports_only_spec_and_the_gated_binary_entry_point() {
    let lib = src("lib.rs");

    assert_eq!(
        pub_items(&lib),
        frozen(&["spec", "cli_main"]),
        "the crate root exports `pub mod spec` and, under the `cli` feature, the \
         doc-hidden binary entry point. Adding to this list widens the contract."
    );

    assert!(
        pub_use_names(&lib).is_empty(),
        "the crate root must not re-export anything; put it in `spec` deliberately or \
         leave it private"
    );

    // `cli_main` runs `clap` over the *caller's* argv and can exit the caller's
    // process, so a library consumer must be able to compile it out entirely.
    assert!(
        lib.contains("#[cfg(feature = \"cli\")]\n#[doc(hidden)]\npub fn cli_main()"),
        "`cli_main` must stay `#[cfg(feature = \"cli\")]` and `#[doc(hidden)]` — with \
         `default-features = false` the crate root is `spec` and nothing else"
    );
    assert!(
        lib.contains("#[cfg(feature = \"cli\")]\nmod cli;"),
        "the `cli` module goes with `cli_main`; leaving it in would pull `clap` and \
         the command dispatch into a library-only build for no reason"
    );
}

/// The feature that gates the binary entry point has to exist, be on by default (so
/// `cargo build` still produces `arc`) and be required by the binary target.
#[test]
fn cargo_toml_gates_the_binary_entry_point() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("Cargo.toml is readable");

    assert!(
        manifest.contains("default = [\"cli\"]"),
        "`cli` must be a default feature or `cargo build` stops producing the binary"
    );
    assert!(
        manifest.contains("\ncli = [\"dep:clap\"]"),
        "the `cli` feature must be declared, and must own `clap`: with the feature off \
         the argv parser has no reason to be in a library consumer's dependency graph"
    );
    assert!(
        manifest.contains("clap = { version = \"4\", features = [\"derive\"], optional = true }"),
        "`clap` must be optional, or turning `cli` off still builds it"
    );
    assert!(
        manifest.contains("required-features = [\"cli\"]"),
        "the `arc` binary must require `cli`, so `--no-default-features` builds the \
         library alone instead of failing on a missing `cli_main`"
    );
}

/// The contract itself: what `arc::spec` re-exports.
#[test]
fn the_spec_module_exports_exactly_the_documented_contract() {
    let spec = src("spec.rs");

    assert_eq!(
        pub_use_names(&spec),
        exported_names(),
        "`arc::spec` is the published contract. If this assertion fails, either the \
         export was unintended — revert it — or it is intended, in which case update \
         `EXPORTED` here and the contract doc at the top of `src/spec.rs` together."
    );

    assert!(
        pub_items(&spec).is_empty(),
        "`spec` is a re-export list, not a home for new items: define them in the \
         module that owns them so there is one place to review"
    );
}

/// An `impl` on an exported type is public API wherever it is written — Rust does
/// not care which module it sits in. Pin each exported type's impls to its own
/// module, which is the only place the item lists below freeze.
#[test]
fn impls_on_exported_types_live_only_in_their_own_module() {
    let owners = std::collections::BTreeMap::from(EXPORTED);
    for file in MODULE_FILES {
        for target in impl_self_types(&src(file)) {
            let Some(owner) = owners.get(target.as_str()) else {
                continue;
            };
            assert_eq!(
                &file, owner,
                "`impl … {target}` in src/{file} widens the published API from a module \
                 no list in this file freezes. Inherent methods and trait impls on an \
                 exported type both belong in src/{owner}."
            );
        }
    }
}

/// A rename would let an `impl` on an exported type wear a name the check above does
/// not recognise. There is no reason to alias the schema types internally.
#[test]
fn no_module_renames_an_exported_type() {
    let exported = exported_names();
    for file in MODULE_FILES {
        let code = blank_non_code(&src(file));
        for (lineno, line) in code.lines().enumerate() {
            let line = line.trim();
            if !line.starts_with("use ") && !line.starts_with("pub use ") {
                continue;
            }
            let Some(alias_at) = line.find(" as ") else {
                continue;
            };
            let before = &line[..alias_at];
            for name in &exported {
                assert!(
                    !before
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .any(|word| word == name),
                    "src/{file}:{} renames the exported type `{name}`; an `impl` on the \
                     alias would slip past the impl-location check",
                    lineno + 1
                );
            }
        }
    }
}

/// Inherent `pub fn`s on exported types are exported too. Freeze them, or the surface
/// grows every time someone reaches for a convenience method.
#[test]
fn exported_modules_declare_only_contracted_items() {
    assert_eq!(
        pub_items(&src("manifest.rs")),
        frozen(&[
            // Re-exported types.
            "Manifest",
            "Step",
            "Param",
            "Defaults",
            "RetryPolicy",
            "Hooks",
            "AssetOverride",
            "MANIFEST_FILENAME",
            // Re-exported entry points. Everything else on these types —
            // `new_project`, `db_path`, `has_sql_steps`, `Step::new`, `Step::is_sql` —
            // is `pub(crate)` on purpose: authoring and execution are not the contract.
            "load",
            "from_yaml_str",
        ]),
        "a `pub` item in `manifest.rs` is public API once its type is re-exported; \
         use `pub(crate)` unless you mean to promise it"
    );

    assert_eq!(
        pub_items(&src("precondition.rs")),
        frozen(&[
            "Precondition",
            "ModifiedAfterConfig",
            "FreshConfig",
            // `validate` is validation; `evaluate` and `evaluate_all` run commands and
            // stat files, so they are `pub(crate)` — this surface does not execute.
            "validate",
        ]),
        "a `pub` item in `precondition.rs` is public API once its type is re-exported"
    );

    assert_eq!(
        pub_items(&src("error.rs")),
        frozen(&["Error", "Result"]),
        "`error.rs` contributes exactly the error type and its alias"
    );

    assert_eq!(
        pub_items(&src("edit.rs")),
        frozen(&[
            // Re-exported types.
            "PathPart",
            "SpecEdit",
            "ValidatedSpec",
            // Re-exported entry points.
            "apply_edits",
            "create_spec",
            "edit_spec",
            // `ValidatedSpec`'s accessors and its atomic write. The splicing
            // internals — span resolution, extent derivation, the temp-file
            // dance — are private: the contract is the ops, not the mechanism.
            "text",
            "manifest",
            "write_to",
        ]),
        "a `pub` item in `edit.rs` is public API once its types are re-exported; \
         use `pub(crate)` unless you mean to promise it"
    );

    assert_eq!(
        pub_items(&src("history.rs")),
        frozen(&[
            // Re-exported types and the policy constants.
            "LocalHistory",
            "HistoryEntry",
            "HistoryKind",
            "HISTORY_MAX_ENTRIES",
            "HISTORY_MERGE_WINDOW",
            // Re-exported entry points: the store handle's surface and the
            // checkpointed roads. The internals — the key hash, the entry
            // naming, the debounce mechanics — are private: the contract is
            // the tiers and the ordering, not the store format.
            "open_default",
            "at_root",
            "root",
            "record_save",
            "record_checkpoint",
            "entries",
            "read",
            "restore",
            "edit_spec_with_history",
            "record_step_with_history",
        ]),
        "a `pub` item in `history.rs` is public API once its types are re-exported; \
         use `pub(crate)` unless you mean to promise it"
    );

    assert_eq!(
        pub_items(&src("record.rs")),
        frozen(&[
            // Re-exported types and the ownership marker.
            "RecordedStep",
            "GENERATED_MARKER",
            // Re-exported entry points. The internals — the slug, the model
            // numbering, the checkpoint seam — are private: the contract is
            // the promotion and the ownership rule, not the mechanism.
            "record_step",
            "amend_step_sql",
            "sql_is_generated",
        ]),
        "a `pub` item in `record.rs` is public API once its type is re-exported; \
         use `pub(crate)` unless you mean to promise it"
    );
}

/// A derive on an exported type is part of the surface. Freeze the lists so neither
/// a new derive (`PartialEq`, another serde trait) nor a silent removal ships
/// without being reviewed as the API change it is. The `Serialize` entries carry a
/// health warning — see `FROZEN_DERIVES` and the round-trip note in `src/spec.rs`.
#[test]
fn the_schema_types_derive_exactly_what_is_recorded() {
    for owner in [
        "manifest.rs",
        "precondition.rs",
        "edit.rs",
        "record.rs",
        "history.rs",
        "error.rs",
    ] {
        let found = pub_type_derives(&src(owner));
        let expected: Vec<(String, Vec<String>)> = FROZEN_DERIVES
            .iter()
            .filter(|(file, _, _)| *file == owner)
            .map(|(_, name, derives)| {
                (
                    (*name).to_string(),
                    derives.iter().map(|d| (*d).to_string()).collect(),
                )
            })
            .collect();
        assert_eq!(
            found, expected,
            "src/{owner}: the derives on its pub types must match FROZEN_DERIVES — \
             changing a derive on an exported type is a version-visible API change"
        );
    }
}

/// The exported error type must stay `#[non_exhaustive]`: the engine behind the
/// private line keeps adding failure modes, and that must not break consumers.
#[test]
fn the_error_type_stays_non_exhaustive() {
    assert!(
        src("error.rs").contains("#[non_exhaustive]\npub enum Error"),
        "removing `#[non_exhaustive]` from `Error` makes every new engine failure a \
         breaking change for consumers"
    );
}
