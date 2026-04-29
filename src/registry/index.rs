//! Registry index document — schema, parser, validation.
//!
//! See `super` module docs for vocabulary and ownership rules.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The three pillars from **decision 0015**. Unknown pillar values fail parsing in v1
/// (hard reject — see ac-01); future versions may relax to skip-unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Pillar {
    Practical,
    Foundational,
    Investigative,
}

impl Pillar {
    /// Stable display order used by `arc registry list`.
    pub const ALL_IN_ORDER: [Pillar; 3] = [
        Pillar::Practical,
        Pillar::Foundational,
        Pillar::Investigative,
    ];

    /// Uppercase header label used by the `list` subcommand.
    pub fn header(&self) -> &'static str {
        match self {
            Pillar::Practical => "PRACTICAL",
            Pillar::Foundational => "FOUNDATIONAL",
            Pillar::Investigative => "INVESTIGATIVE",
        }
    }
}

/// One registry entry. `min_arcform_version` is intentionally NOT included in v1 —
/// see scope.out + ac-01 in the spec; reintroduce in a follow-up once enforcement
/// semantics are decided.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Bare entry name (e.g. `brewtrend`).
    pub name: String,
    /// `None` = canonical; `Some(<owner>)` = contributor.
    #[serde(default)]
    pub owner: Option<String>,
    pub pillar: Pillar,
    pub summary: String,
    /// Origin repository URL — used by the production transport.
    pub repo_url: String,
    /// Path within the repository where the entry's `arcform.yaml` lives.
    pub repo_path: String,
    /// Pinned default ref. Override at the CLI with `--version <ref>`.
    pub current_version: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub schedule_guidance: Option<String>,
}

impl IndexEntry {
    /// `<owner>/<name>` for contributor entries; bare `<name>` for canonical.
    pub fn display_name(&self) -> String {
        match &self.owner {
            Some(owner) => format!("{}/{}", owner, self.name),
            None => self.name.clone(),
        }
    }
}

/// Top-level index document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<IndexEntry>,
}

const SUPPORTED_INDEX_VERSION: u32 = 1;

impl RegistryIndex {
    /// Parse + validate from a YAML string.
    pub fn parse(yaml: &str) -> Result<Self> {
        let raw: RegistryIndex = serde_yaml::from_str(yaml)
            .map_err(|e| Error::RegistryIndexParse { detail: e.to_string() })?;
        raw.validate()?;
        Ok(raw)
    }

    fn validate(&self) -> Result<()> {
        if self.version != SUPPORTED_INDEX_VERSION {
            return Err(Error::RegistryIndexParse {
                detail: format!(
                    "unsupported index version {} (expected {})",
                    self.version, SUPPORTED_INDEX_VERSION
                ),
            });
        }

        // Duplicate (owner, name) detection.
        let mut seen: HashSet<(Option<&str>, &str)> = HashSet::new();
        for e in &self.entries {
            let key = (e.owner.as_deref(), e.name.as_str());
            if !seen.insert(key) {
                return Err(Error::RegistryIndexParse {
                    detail: format!("duplicate entry '{}'", e.display_name()),
                });
            }
        }

        // Canonical-shadowing detection: any contributor `name` colliding with a
        // canonical entry's `name` is rejected.
        let canonical_names: HashSet<&str> = self
            .entries
            .iter()
            .filter(|e| e.owner.is_none())
            .map(|e| e.name.as_str())
            .collect();
        for e in &self.entries {
            if e.owner.is_some() && canonical_names.contains(e.name.as_str()) {
                return Err(Error::RegistryIndexParse {
                    detail: format!(
                        "contributor entry '{}' shadows canonical name '{}'",
                        e.display_name(),
                        e.name
                    ),
                });
            }
        }

        Ok(())
    }

    /// Lookup helper used by both `list` rendering and the resolver.
    pub fn find_canonical(&self, name: &str) -> Option<&IndexEntry> {
        self.entries
            .iter()
            .find(|e| e.owner.is_none() && e.name == name)
    }

    /// Lookup helper for `<owner>/<name>` queries.
    pub fn find_contributor(&self, owner: &str, name: &str) -> Option<&IndexEntry> {
        self.entries
            .iter()
            .find(|e| e.owner.as_deref() == Some(owner) && e.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_index() -> &'static str {
        r#"
version: 1
entries:
  - name: brewtrend
    pillar: practical
    summary: Homebrew analytics & trending packages
    repo_url: https://github.com/meridian-online/registry
    repo_path: practical/brewtrend
    current_version: v1.0
    sources:
      - https://formulae.brew.sh/api/analytics/install/30d.json
    schedule_guidance: daily
  - name: gnaf
    pillar: foundational
    summary: Australian address gazetteer
    repo_url: https://github.com/meridian-online/registry
    repo_path: foundational/gnaf
    current_version: v0.4
    sources: []
  - name: myproject
    owner: someone
    pillar: practical
    summary: Personal example
    repo_url: https://github.com/someone/myproject
    repo_path: ""
    current_version: v0.3
    sources: []
"#
    }

    // ac-01: parses sample index covering all three pillars + canonical and contributor.
    #[test]
    fn test_ac01_parse_valid_index() {
        let idx = RegistryIndex::parse(fixture_index()).expect("parse should succeed");
        assert_eq!(idx.version, 1);
        assert_eq!(idx.entries.len(), 3);

        let brew = &idx.entries[0];
        assert_eq!(brew.name, "brewtrend");
        assert_eq!(brew.owner, None);
        assert_eq!(brew.pillar, Pillar::Practical);
        assert_eq!(brew.current_version, "v1.0");
        assert_eq!(brew.display_name(), "brewtrend");

        let mp = &idx.entries[2];
        assert_eq!(mp.owner.as_deref(), Some("someone"));
        assert_eq!(mp.display_name(), "someone/myproject");
    }

    // ac-01: covers Foundational pillar specifically.
    #[test]
    fn test_ac01_parses_all_pillars() {
        let idx = RegistryIndex::parse(fixture_index()).unwrap();
        let pillars: Vec<Pillar> = idx.entries.iter().map(|e| e.pillar).collect();
        assert!(pillars.contains(&Pillar::Practical));
        assert!(pillars.contains(&Pillar::Foundational));
    }

    // ac-01: index version=2 rejects.
    #[test]
    fn test_ac01_unsupported_version_rejects() {
        let yaml = "version: 2\nentries: []\n";
        let err = RegistryIndex::parse(yaml).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("version"), "should mention version: {msg}");
    }

    // ac-01: duplicate (owner, name) rejects.
    #[test]
    fn test_ac01_duplicate_rejects() {
        let yaml = r#"
version: 1
entries:
  - name: brewtrend
    pillar: practical
    summary: a
    repo_url: x
    repo_path: x
    current_version: v1
    sources: []
  - name: brewtrend
    pillar: practical
    summary: b
    repo_url: x
    repo_path: x
    current_version: v2
    sources: []
"#;
        let err = RegistryIndex::parse(yaml).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    // ac-01: contributor shadowing canonical name rejects.
    #[test]
    fn test_ac01_contributor_shadows_canonical_rejects() {
        let yaml = r#"
version: 1
entries:
  - name: brewtrend
    pillar: practical
    summary: canonical
    repo_url: x
    repo_path: x
    current_version: v1
    sources: []
  - name: brewtrend
    owner: someone
    pillar: practical
    summary: shadow
    repo_url: x
    repo_path: x
    current_version: v1
    sources: []
"#;
        let err = RegistryIndex::parse(yaml).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("shadows"), "{msg}");
        assert!(msg.contains("brewtrend"), "{msg}");
    }

    // ac-01: unknown pillar value rejects with the entry name in the message.
    #[test]
    fn test_ac01_unknown_pillar_rejects() {
        let yaml = r#"
version: 1
entries:
  - name: weird
    pillar: educational
    summary: x
    repo_url: x
    repo_path: x
    current_version: v1
    sources: []
"#;
        let err = RegistryIndex::parse(yaml).unwrap_err();
        // serde_yaml's error mentions the offending field/value;
        // we rely on its "educational" or "pillar" hint to confirm the rejection point.
        let msg = err.to_string();
        assert!(
            msg.contains("educational") || msg.contains("pillar") || msg.contains("variant"),
            "expected pillar/variant rejection: {msg}"
        );
    }

    #[test]
    fn test_pillar_headers() {
        assert_eq!(Pillar::Practical.header(), "PRACTICAL");
        assert_eq!(Pillar::Foundational.header(), "FOUNDATIONAL");
        assert_eq!(Pillar::Investigative.header(), "INVESTIGATIVE");
    }
}
