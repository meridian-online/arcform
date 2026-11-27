//! What each operation arc holds is called: its title, the one sentence saying
//! what an analyst gets from it, and the words other tools use for it.
//!
//! The words come from Meridian's operation catalogue, which publishes them as
//! `operations.json`. arc embeds a copy of that file, so a build answers with no
//! network and no catalogue checkout, and [`landscape_commit`] names the
//! catalogue commit the copy was taken from. To take a newer copy, replace
//! `src/operations.json` with the catalogue's and run the tests: they hold each
//! operation arc holds to exactly one entry.
//!
//! An alias is for finding an operation only. An operation is still recorded,
//! described and looked up by its exact long name, and no alias is ever written
//! into a Protocol.
//!
//! This module is published, and compiled with no feature, so a sibling tool
//! linking arc with `default-features = false` reads the same words arc does.

use std::sync::LazyLock;

use serde::Deserialize;

/// Another tool's word for an operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Alias {
    /// The word, spelled as the tool spells it.
    pub word: String,
    /// The tool or the spec that uses the word, when the catalogue may name it;
    /// `None` for a word whose tool goes unnamed.
    #[serde(default)]
    pub source: Option<String>,
}

/// What one operation is called.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OperationWords {
    /// The operation's identity, as `arc operation list` prints it.
    pub long_name: String,
    /// The name an analyst reads, such as "Fill in missing values".
    pub title: String,
    /// One sentence saying what the operation gives the analyst.
    pub sentence: String,
    /// The words other tools use for the operation, sorted case-folded.
    pub aliases: Vec<Alias>,
}

/// The embedded file, as the catalogue writes it.
#[derive(Deserialize)]
struct Extract {
    version: u32,
    landscape_commit: String,
    operations: Vec<OperationWords>,
}

/// The one format of the extract this build reads.
const VERSION: u32 = 1;

static EXTRACT: LazyLock<Extract> = LazyLock::new(|| {
    let extract: Extract = serde_json::from_str(include_str!("operations.json"))
        .expect("the embedded operations.json parses");
    assert_eq!(
        extract.version, VERSION,
        "the embedded operations.json is format {}, and this build reads format {VERSION}",
        extract.version
    );
    extract
});

/// Every operation's words, in the catalogue's order.
pub fn operations() -> &'static [OperationWords] {
    &EXTRACT.operations
}

/// The words of the operation called `long_name`, or `None` when the catalogue
/// holds no operation by that name. The match is exact, as a long name's is
/// everywhere in arc.
pub fn of(long_name: &str) -> Option<&'static OperationWords> {
    operations().iter().find(|op| op.long_name == long_name)
}

/// The catalogue commit the embedded words were taken from.
pub fn landscape_commit() -> &'static str {
    &EXTRACT.landscape_commit
}

impl OperationWords {
    /// The words as a description carries them: `title`, `sentence` and
    /// `aliases`, each alias's `source` only where it has one.
    pub(crate) fn fields(&self) -> [(&'static str, serde_json::Value); 3] {
        let aliases = self
            .aliases
            .iter()
            .map(|alias| match &alias.source {
                Some(source) => serde_json::json!({ "word": alias.word, "source": source }),
                None => serde_json::json!({ "word": alias.word }),
            })
            .collect::<Vec<_>>();
        [
            ("title", self.title.clone().into()),
            ("sentence", self.sentence.clone().into()),
            ("aliases", aliases.into()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded file parses, and the catalogue and arc agree on which
    /// operations exist: each operation arc holds has one entry, and each entry
    /// is an operation arc holds.
    #[test]
    fn every_catalogue_operation_has_words_and_every_entry_an_operation() {
        let held = crate::record::operations();
        for op in held {
            assert!(
                of(op.long_name).is_some(),
                "operations.json has no entry for {}",
                op.long_name
            );
        }
        for words in operations() {
            assert!(
                crate::record::operation(&words.long_name).is_some(),
                "operations.json describes {}, which arc does not hold",
                words.long_name
            );
        }
        assert_eq!(operations().len(), held.len());
    }

    #[test]
    fn each_operation_has_a_title_a_sentence_and_distinct_aliases() {
        for words in operations() {
            assert!(
                !words.title.trim().is_empty(),
                "{} has no title",
                words.long_name
            );
            assert!(
                words.sentence.ends_with('.'),
                "{}'s sentence does not end with a full stop",
                words.long_name
            );
            let mut folded: Vec<String> = words
                .aliases
                .iter()
                .map(|a| a.word.to_lowercase())
                .collect();
            let count = folded.len();
            folded.sort();
            folded.dedup();
            assert_eq!(folded.len(), count, "{} repeats an alias", words.long_name);
        }
    }

    #[test]
    fn the_extract_names_the_catalogue_commit_it_came_from() {
        let commit = landscape_commit();
        assert!(
            commit.len() >= 7 && commit.chars().all(|c| c.is_ascii_hexdigit()),
            "landscape_commit {commit:?} is not a commit id"
        );
    }

    #[test]
    fn an_alias_keeps_its_source_only_where_it_has_one() {
        let fill = of("fill-nulls").expect("fill-nulls has words");
        assert_eq!(fill.title, "Fill in missing values");
        assert!(fill.aliases.contains(&Alias {
            word: "fill_null".to_string(),
            source: Some("Polars".to_string()),
        }));
        let filter = of("filter-rows").expect("filter-rows has words");
        assert!(filter.aliases.iter().any(|a| a.source.is_none()));
    }

    #[test]
    fn a_long_name_is_matched_exactly() {
        assert!(of("Fill-Nulls").is_none());
        assert!(of("fill").is_none());
    }
}
