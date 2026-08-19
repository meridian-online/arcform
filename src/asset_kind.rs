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
/// and [`default_kind_for_declared_produces`] are that site, and they read only what
/// such a string carries syntactically — a glob metacharacter, a path separator, and
/// which of the two lists it was written in — rather than enumerating extensions or
/// asking the filesystem.
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
    /// Not a filesystem path at all: a relational table or view identifier, or a
    /// hand-declared `produces:` name that addresses no path (see
    /// [`default_kind_for_declared_produces`]). Never hashed — staleness comes from
    /// the producing step's own config hash and downstream propagation.
    Table,
}

/// The default for a name in `depends_on:` (or an `assets:` dependency) with no
/// operator or SQL parser behind it to consult — a plain manifest string a human
/// wrote, with no type syntax available.
///
/// Decided on the one thing such a string carries syntactically: a glob metacharacter
/// means it can never resolve to one path. Everything else is a path. **A bare name
/// on this side is a path too**, and that is not symmetric with
/// [`default_kind_for_declared_produces`] by accident. Two places read `declared_kind`
/// for a name on the reads side — `runner::produced_artifact_hash`, which drops any
/// read that appears in `all_produced`, and `contract::build_assets`, which prefers a
/// producer's kind and consults a reader's when there is no producer. Both consult a
/// reader's kind for exactly the names nothing in the manifest produces, and an input
/// the pipeline does not produce comes from outside the pipeline — which is the
/// filesystem.
///
/// The failure mode when this lands on a name that is really a directory: `fs::read`
/// on a directory errors, which forces staleness exactly as a missing file does, so it
/// does not fabricate a false "unchanged" — the step re-runs on every run instead.
/// That is safe and it is expensive, and on this side it is also **silent**: nothing
/// prints, because `missing_declared_produces` reports the `produces:` side only.
pub fn default_kind_for_declared_name(raw: &str) -> AssetKind {
    if raw.contains(['*', '?', '[']) {
        AssetKind::Pattern
    } else {
        AssetKind::File
    }
}

/// The default for a name in `produces:` (or an `assets:` output) with no operator or
/// SQL parser behind it to consult.
///
/// Same glob rule, plus one more syntactic fact: a path separator. A `produces:` name
/// without one is not a filesystem address — it is the manifest's own vocabulary for
/// an ordering edge, the shape `examples/code-lists/arcform.yaml` writes as
/// `produces: [raw_tables]` and `produces: [license_cleared]`, against
/// `tests/fixtures/open_analytics/edgar_gleif/arcform.yaml`'s
/// `depends_on: [build/resolved.parquet]` for a real file. So it is classified the way
/// the one classifier in this crate with real syntax to read already classifies a bare
/// identifier: `Table` — not hashed, its staleness left to the config hash and the
/// downstream propagation the relational machinery has always applied to it.
///
/// **This is safe on the `produces:` side specifically, and the argument is why the
/// two sides differ.** A step that really writes bytes at a bare root name says so
/// where it says everything else: a `sql:` step's `COPY … TO 'out.parquet'` is read by
/// SQL introspection, an `op:` step's `dest:` by the operator's own config, and both
/// run before this default and win under `StepAssets::record`'s `or_insert`. A
/// `command:` step's produced bytes are not hashed at all, whatever kind they carry.
/// What is left for this default is a name the step's own work never mentions — which
/// is what an ordering token is.
///
/// **To declare a file or a directory in the protocol root, write a separator:**
/// `./output.parquet`, not `output.parquet`. That is the whole interface, and it is
/// the reason this is a rule rather than a guess — nothing here inspects an extension,
/// scans a directory or asks the filesystem, which is what each of the four earlier
/// attempts did before it got a different case wrong.
pub fn default_kind_for_declared_produces(raw: &str) -> AssetKind {
    match default_kind_for_declared_name(raw) {
        AssetKind::File if !raw.contains('/') && !raw.contains(std::path::MAIN_SEPARATOR) => {
            AssetKind::Table
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_produces_name_is_not_a_filesystem_path() {
        for name in ["raw_tables", "license_cleared", "open_catalog", "validated"] {
            assert_eq!(
                default_kind_for_declared_produces(name),
                AssetKind::Table,
                "{name} has no path separator"
            );
        }
    }

    // The trap an extension allowlist walked into from the other side: these names end
    // in something file-shaped and are ordering tokens in this repo's own examples
    // (`tides_csv` and `report_csv` in `examples/almanac`, `cask_json` in
    // `examples/brewtrend`). Spelling is not the signal; the separator is.
    #[test]
    fn a_file_shaped_bare_produces_name_is_still_not_a_path() {
        for name in ["tides_csv", "report_csv", "cask_json", "output.parquet"] {
            assert_eq!(
                default_kind_for_declared_produces(name),
                AssetKind::Table,
                "{name} has no path separator"
            );
        }
    }

    #[test]
    fn a_separator_makes_a_produces_name_a_path() {
        for name in ["build/resolved.parquet", "./output.parquet", "/tmp/x"] {
            assert_eq!(
                default_kind_for_declared_produces(name),
                AssetKind::File,
                "{name} carries a path separator"
            );
        }
    }

    // The reads side keeps every non-glob name a path, separator or not — an input
    // nothing produces has to come off the filesystem.
    #[test]
    fn a_bare_depends_on_name_stays_a_path() {
        for name in ["external.csv", "raw_tables", "build/edgar.parquet"] {
            assert_eq!(default_kind_for_declared_name(name), AssetKind::File);
        }
    }

    #[test]
    fn a_glob_is_a_pattern_on_both_sides() {
        for name in ["build/ncen/*/REGISTRANT.tsv", "*.parquet", "x?.csv"] {
            assert_eq!(default_kind_for_declared_name(name), AssetKind::Pattern);
            assert_eq!(default_kind_for_declared_produces(name), AssetKind::Pattern);
        }
    }
}
