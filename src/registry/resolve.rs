//! Resolve a (query, version-override) pair to a concrete entry + ref.

use crate::error::{Error, Result};
use crate::registry::index::{IndexEntry, Pillar, RegistryIndex};

/// CLI-provided version override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionSpec {
    /// `--version <ref>` — explicit pin.
    Pinned(String),
    /// `--latest` — RESERVED in v1; the resolver errors with `RegistryUnimplemented`
    /// before any transport work.
    Latest,
}

/// Output of the resolver: a concrete entry plus the concrete ref.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    pub name: String,
    pub owner: Option<String>,
    pub ref_: String,
    pub repo_url: String,
    pub repo_path: String,
    // Pillar is captured for future surface (e.g. `arc registry show` already prints it)
    // but the resolver-side reader hasn't landed yet; keep the field on the struct so
    // downstream consumers don't need to re-thread it.
    #[allow(dead_code)]
    pub pillar: Pillar,
}

impl ResolvedEntry {
    pub fn display_name(&self) -> String {
        match &self.owner {
            Some(o) => format!("{}/{}", o, self.name),
            None => self.name.clone(),
        }
    }
}

/// Parse a query string into (owner, name).
///
/// - No slash → canonical query: `(None, query)`.
/// - One slash → contributor query: `(Some(owner), name)`.
/// - More than one slash, empty owner, or empty name → `RegistryAmbiguousQuery`.
fn parse_query(query: &str) -> Result<(Option<&str>, &str)> {
    if query.is_empty() {
        return Err(Error::RegistryAmbiguousQuery {
            query: query.to_string(),
        });
    }
    match query.matches('/').count() {
        0 => Ok((None, query)),
        1 => {
            let (owner, name) = query.split_once('/').unwrap();
            if owner.is_empty() || name.is_empty() {
                Err(Error::RegistryAmbiguousQuery {
                    query: query.to_string(),
                })
            } else {
                Ok((Some(owner), name))
            }
        }
        _ => Err(Error::RegistryAmbiguousQuery {
            query: query.to_string(),
        }),
    }
}

fn lookup<'a>(index: &'a RegistryIndex, query: &str) -> Result<&'a IndexEntry> {
    let (owner, name) = parse_query(query)?;
    let hit = match owner {
        None => index.find_canonical(name),
        Some(o) => index.find_contributor(o, name),
    };
    hit.ok_or_else(|| Error::RegistryUnknownEntry {
        query: query.to_string(),
    })
}

/// Resolve a (query, version-override) pair to a concrete `ResolvedEntry`.
///
/// `Latest` is reserved in v1 — see ac-02 + spec scope.out — and errors with
/// `RegistryUnimplemented` before any transport work.
pub fn resolve(
    index: &RegistryIndex,
    query: &str,
    version_override: Option<VersionSpec>,
) -> Result<ResolvedEntry> {
    let entry = lookup(index, query)?;
    let ref_ = match version_override {
        None => entry.current_version.clone(),
        Some(VersionSpec::Pinned(r)) => r,
        Some(VersionSpec::Latest) => {
            return Err(Error::RegistryUnimplemented {
                feature: "--latest rolling resolution".to_string(),
            });
        }
    };
    Ok(ResolvedEntry {
        name: entry.name.clone(),
        owner: entry.owner.clone(),
        ref_,
        repo_url: entry.repo_url.clone(),
        repo_path: entry.repo_path.clone(),
        pillar: entry.pillar,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx() -> RegistryIndex {
        let yaml = r#"
version: 1
entries:
  - name: brewtrend
    pillar: practical
    summary: a
    repo_url: https://example.com/r
    repo_path: practical/brewtrend
    current_version: v1.0
    sources: []
  - name: myproject
    owner: someone
    pillar: practical
    summary: a
    repo_url: https://example.com/u
    repo_path: ""
    current_version: v0.3
    sources: []
"#;
        RegistryIndex::parse(yaml).unwrap()
    }

    // ac-02: canonical query resolves to current_version.
    #[test]
    fn test_ac02_canonical_default() {
        let r = resolve(&idx(), "brewtrend", None).unwrap();
        assert_eq!(r.name, "brewtrend");
        assert_eq!(r.owner, None);
        assert_eq!(r.ref_, "v1.0");
        assert_eq!(r.display_name(), "brewtrend");
    }

    // ac-02: contributor query resolves correctly.
    #[test]
    fn test_ac02_contributor_query() {
        let r = resolve(&idx(), "someone/myproject", None).unwrap();
        assert_eq!(r.owner.as_deref(), Some("someone"));
        assert_eq!(r.ref_, "v0.3");
        assert_eq!(r.display_name(), "someone/myproject");
    }

    // ac-02: pinned override returns the pinned ref.
    #[test]
    fn test_ac02_pinned_override() {
        let r = resolve(
            &idx(),
            "brewtrend",
            Some(VersionSpec::Pinned("v0.5".to_string())),
        )
        .unwrap();
        assert_eq!(r.ref_, "v0.5");
    }

    // ac-02: --latest errors with RegistryUnimplemented naming --latest.
    #[test]
    fn test_ac02_latest_errors_unimplemented() {
        let err = resolve(&idx(), "brewtrend", Some(VersionSpec::Latest)).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, Error::RegistryUnimplemented { .. }));
        assert!(msg.contains("--latest"), "{msg}");
    }

    // ac-02: unknown query errors.
    #[test]
    fn test_ac02_unknown_query_errors() {
        let err = resolve(&idx(), "ghost", None).unwrap_err();
        assert!(matches!(err, Error::RegistryUnknownEntry { .. }));
    }

    // ac-02: malformed `a/b/c` errors.
    #[test]
    fn test_ac02_malformed_three_slash_errors() {
        let err = resolve(&idx(), "a/b/c", None).unwrap_err();
        assert!(matches!(err, Error::RegistryAmbiguousQuery { .. }));
    }

    // ac-02: empty halves of slash form error.
    #[test]
    fn test_ac02_malformed_empty_owner_errors() {
        let err = resolve(&idx(), "/foo", None).unwrap_err();
        assert!(matches!(err, Error::RegistryAmbiguousQuery { .. }));
    }

    #[test]
    fn test_ac02_malformed_empty_name_errors() {
        let err = resolve(&idx(), "owner/", None).unwrap_err();
        assert!(matches!(err, Error::RegistryAmbiguousQuery { .. }));
    }
}
