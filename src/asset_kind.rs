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
///
/// One declaration site has no parser and no operator to consult: a `produces:`,
/// `depends_on:` or `assets:` string a human wrote. [`default_kind_for_declared_name`]
/// is that site, and it reads only what such a string carries syntactically — a glob
/// metacharacter — rather than enumerating extensions, splitting on a path separator,
/// or asking the filesystem.
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
    /// Not a filesystem path at all: a relational table or view identifier, the way
    /// SQL introspection reads a bare `CREATE OR REPLACE TABLE` target. Never hashed
    /// — staleness comes from the producing step's own config hash and downstream
    /// propagation.
    Table,
}

/// The default for a `produces:`, `depends_on:` or `assets:` name with no operator
/// or SQL parser behind it to consult — a plain manifest string a human wrote, with
/// no type syntax available.
///
/// Decided on the two things such a string carries syntactically, and nothing else.
/// A glob metacharacter means it can never resolve to one path, so the name is a
/// [`AssetKind::Pattern`]. A TRAILING path separator means it can never be a regular
/// file — `build/parts/` and `build/parts` name the same place, and only the first
/// spelling says which of the two it is — so the name is a [`AssetKind::Directory`].
/// Everything else is a file. Both rules read the string itself; neither enumerates
/// extensions and neither asks the filesystem.
///
/// The trailing separator used to fall through to [`AssetKind::File`], and the cost
/// was silent: `fs::read` on a directory errors, `runner::produced_artifact_hash`
/// withholds the hash, and the step re-runs on every run — for a `depends_on:`
/// entry with nothing on stderr, because `runner::missing_declared_produces`
/// reports the `produces:` side only.
///
/// **A separator-free `produces:` token is a path too, and for the ordering-edge
/// shape `examples/code-lists/arcform.yaml` writes as `produces: [raw_tables]` that
/// is a deliberate, loud cost.** There is no file at `<protocol dir>/raw_tables`, so
/// `fs::read` fails, `runner::produced_artifact_hash` returns `None`, the step is
/// stale on every run, and `runner::missing_declared_produces` names it on stderr
/// each time.
///
/// The alternative — reading a separator-free `produces:` name as [`AssetKind::Table`]
/// and dropping it out of the staleness path entirely — was built, driven and
/// withdrawn. It fabricates a false "unchanged": what the code can test is whether
/// arcform's introspection recorded the name, not whether the step's own work wrote
/// bytes under it, and those differ wherever a step writes through a path
/// introspection does not model — `ATTACH 'side.db'` inside a `sql:` step, or an
/// `assets:` override, which exists precisely for an asset arcform cannot discover.
/// A `side.db` declared that way could be truncated or deleted and the step still
/// reported `[skip: hash_clean]` at exit 0. A step that re-runs forever is expensive
/// and visible; a step certifying an artifact it can no longer read is neither.
/// `runner::tests::test_bare_produces_ordering_token_reruns_rather_than_settling`
/// and `tests/bare_produces_ordering_token.rs` pin that trade from both ends.
///
/// The failure mode when this lands on a name that is really a directory: `fs::read`
/// on a directory errors, which forces staleness exactly as a missing file does, so it
/// does not fabricate a false "unchanged" — the step re-runs on every run instead.
/// That is safe and it is expensive, and on the `depends_on:` side it is also
/// **silent**: nothing prints, because `missing_declared_produces` reports the
/// `produces:` side only.
pub fn default_kind_for_declared_name(raw: &str) -> AssetKind {
    if raw.contains(['*', '?', '[']) {
        AssetKind::Pattern
    } else if raw.ends_with(['/', std::path::MAIN_SEPARATOR]) {
        AssetKind::Directory
    } else {
        AssetKind::File
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ordering-token shape this repo's own examples ship, and the file-shaped
    // spellings an extension allowlist walked into from the other side (`tides_csv`
    // and `report_csv` in `examples/almanac`, `cask_json` in `examples/brewtrend`).
    // Each is a path here; beyond a glob metacharacter the spelling is not read.
    #[test]
    fn every_non_glob_name_is_a_path_on_both_sides() {
        for name in [
            "raw_tables",
            "license_cleared",
            "tides_csv",
            "cask_json",
            "output.parquet",
            "build/resolved.parquet",
            "./output.parquet",
            "external.csv",
        ] {
            assert_eq!(
                default_kind_for_declared_name(name),
                AssetKind::File,
                "{name} carries no glob metacharacter"
            );
        }
    }

    #[test]
    fn a_glob_is_a_pattern() {
        for name in ["build/ncen/*/REGISTRANT.tsv", "*.parquet", "x?.csv"] {
            assert_eq!(default_kind_for_declared_name(name), AssetKind::Pattern);
        }
    }

    // A name ending in a separator cannot be a regular file, and that is the whole
    // of the reasoning — nothing is enumerated and the filesystem is not asked.
    #[test]
    fn a_trailing_separator_is_a_directory() {
        for name in ["build/parts/", "build/", "data/ncen/2026q2/"] {
            assert_eq!(
                default_kind_for_declared_name(name),
                AssetKind::Directory,
                "{name} ends in a separator, so it cannot be a regular file"
            );
        }
    }

    // The rule reads the LAST character, not any separator: the same name without
    // its trailing slash stays a file, which is what keeps every existing
    // declaration on its current side.
    #[test]
    fn the_same_name_without_the_trailing_separator_stays_a_file() {
        for name in ["build/parts", "build", "data/ncen/2026q2"] {
            assert_eq!(default_kind_for_declared_name(name), AssetKind::File);
        }
    }

    // A glob wins over a trailing separator: `build/*/` can still match several
    // directories, so it is not one directory to hash.
    #[test]
    fn a_glob_with_a_trailing_separator_is_still_a_pattern() {
        assert_eq!(
            default_kind_for_declared_name("build/*/"),
            AssetKind::Pattern
        );
    }
}
