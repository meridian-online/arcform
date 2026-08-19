//! What kind of thing a declared asset name refers to.

/// Set once, at the point an asset is declared — an operator's own config, SQL
/// introspection, or a manifest author's explicit `produces:`/`depends_on:` — and
/// carried alongside its case-preserved spelling from then on, rather than
/// reconstructed later by inspecting the name or the filesystem. Every earlier
/// attempt to answer "how do I check this asset's bytes for staleness" from the
/// string alone got a different case wrong — `contains('/')`, a directory scan, an
/// extension allowlist, `is_dir()` used to opt *out* of hashing instead of choosing
/// *how*. The kind is knowable exactly where the asset is declared; nothing after
/// that point should have to guess it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssetKind {
    /// A single, literal, readable regular file. Staleness hashes its bytes.
    File,
    /// A directory. Staleness hashes a manifest of its contents — every regular
    /// file within it, path plus content hash — so a directory that is emptied,
    /// has a file added or removed, or has any one file's bytes changed all read
    /// as changed, not just a directory that vanishes outright (which already read
    /// as changed via a plain missing-file check).
    Directory,
    /// A glob pattern, or any other name that can never resolve to one literal
    /// path. Never hashed directly — staleness comes from whatever step produces
    /// the files it matches, through the asset graph, not from the pattern string.
    Pattern,
    /// A relational table or view identifier — not a filesystem path at all.
    Table,
}

/// The default for a name with no operator or SQL parser behind it to consult — an
/// explicit `produces:`/`depends_on:` entry, or an `assets:` override, both plain
/// manifest strings a human wrote with no type syntax available. A glob is still
/// syntactically detectable; anything else defaults to `File`. If that guess is
/// wrong (the name actually names a directory), the failure mode is safe rather than
/// silent: `std::fs::read` on a directory errors, which forces staleness exactly as
/// a missing file does — it does not fabricate a false "unchanged."
pub fn default_kind_for_declared_name(raw: &str) -> AssetKind {
    if raw.contains(['*', '?', '[']) {
        AssetKind::Pattern
    } else {
        AssetKind::File
    }
}
