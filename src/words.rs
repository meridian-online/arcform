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
//! The module also holds the one matcher that finds an operation from a text:
//! [`rank`] places a query against one operation in six tiers, and [`search`]
//! lists every operation a query finds, best first. The palette and an agent
//! searching arc read the same answer.
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

/// What a query matched on an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matched {
    /// The operation's title, such as "Fill in missing values".
    Title,
    /// The operation's long name, such as `fill-nulls`.
    LongName,
    /// Another tool's word for the operation, with the tool or spec that uses
    /// it where the catalogue may name it.
    Alias {
        word: String,
        source: Option<String>,
    },
    /// The operation's one sentence.
    Sentence,
}

/// How well a query matched an operation.
///
/// The tiers, highest first:
///
/// 1. the whole title or long name;
/// 2. the start of the title or long name, or of any word in either, a long
///    name's words being split at its hyphens;
/// 3. a whole alias;
/// 4. the start of any word inside an alias;
/// 5. a fuzzy subsequence of the title or long name;
/// 6. a piece of the sentence.
///
/// At each degree of match the operation's own names rank above another tool's
/// word, and an alias never matches fuzzily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rank {
    /// The tier, 1 being the highest.
    pub tier: u8,
    /// How well the query matched within its tier: higher is better. Scores
    /// compare only within one tier.
    pub score: i32,
    /// What the query matched.
    pub matched: Matched,
}

/// What [`rank`] matches a query against. A verb that is not an operation has
/// no title, no aliases and, in place of a sentence, its help text.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub title: Option<&'a str>,
    pub long_name: &'a str,
    pub aliases: &'a [Alias],
    pub sentence: &'a str,
}

impl<'a> From<&'a OperationWords> for Candidate<'a> {
    fn from(words: &'a OperationWords) -> Self {
        Candidate {
            title: Some(&words.title),
            long_name: &words.long_name,
            aliases: &words.aliases,
            sentence: &words.sentence,
        }
    }
}

/// An operation a query found, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub long_name: &'static str,
    pub title: &'static str,
    pub rank: Rank,
}

/// A query or a name as the matcher compares it: trimmed, case-folded, with a
/// typographic apostrophe read as a straight one and each run of whitespace
/// read as one space.
pub fn normalise(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .replace('\u{2019}', "'")
}

/// How well `query` matches `candidate`, or `None` when it does not match.
/// When several of the candidate's names match, the best tier wins, then the
/// best score; between equals the title wins in tiers 1 and 2, and the long
/// name in tier 5, where a query is an abbreviation of the working vocabulary.
pub fn rank(query: &str, candidate: &Candidate) -> Option<Rank> {
    let query = normalise(query);
    if query.is_empty() {
        return None;
    }
    let title = candidate.title.map(normalise);
    let long_name = normalise(candidate.long_name);
    // The long name read as words, so "fill nulls" is the whole of fill-nulls.
    let long_words = long_name.replace('-', " ");
    // The operation's own names, the title first so it wins a tie.
    let names: Vec<(&str, Matched)> = title
        .iter()
        .map(|t| (t.as_str(), Matched::Title))
        .chain([
            (long_name.as_str(), Matched::LongName),
            (long_words.as_str(), Matched::LongName),
        ])
        .collect();

    // Tier 1: a whole name.
    if let Some((_, matched)) = names.iter().find(|(name, _)| *name == query) {
        return Some(Rank {
            tier: 1,
            score: 0,
            matched: matched.clone(),
        });
    }

    // Tier 2: the start of a name, or of a word in it.
    if let Some((score, matched)) = best(
        names
            .iter()
            .filter_map(|(name, matched)| word_prefix(&query, name).map(|s| (s, matched.clone()))),
    ) {
        return Some(Rank {
            tier: 2,
            score,
            matched,
        });
    }

    let aliases: Vec<(String, &Alias)> = candidate
        .aliases
        .iter()
        .map(|alias| (normalise(&alias.word), alias))
        .collect();
    let as_matched = |alias: &Alias| Matched::Alias {
        word: alias.word.clone(),
        source: alias.source.clone(),
    };

    // Tier 3: a whole alias.
    if let Some((_, alias)) = aliases.iter().find(|(word, _)| *word == query) {
        return Some(Rank {
            tier: 3,
            score: 0,
            matched: as_matched(alias),
        });
    }

    // Tier 4: the start of a word inside an alias. A shorter alias is a closer
    // match, so it scores higher.
    if let Some((score, matched)) = best(aliases.iter().filter_map(|(word, alias)| {
        word_prefix(&query, word).map(|s| (s - word.chars().count() as i32, as_matched(alias)))
    })) {
        return Some(Rank {
            tier: 4,
            score,
            matched,
        });
    }

    // Tier 5: a fuzzy subsequence of a name, the long name first so it wins a
    // tie. The query's spaces are not matched.
    let squeezed: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some((score, matched)) =
        best(names.iter().rev().filter_map(|(name, matched)| {
            subsequence(&squeezed, name).map(|s| (s, matched.clone()))
        }))
    {
        return Some(Rank {
            tier: 5,
            score,
            matched,
        });
    }

    // Tier 6: a piece of the sentence. An earlier piece scores higher.
    normalise(candidate.sentence).find(&query).map(|at| Rank {
        tier: 6,
        score: -(at as i32),
        matched: Matched::Sentence,
    })
}

/// Every operation `query` matches, best first: by tier, then score, then long
/// name. Each operation is listed once, at its best match.
pub fn search(query: &str) -> Vec<Hit> {
    search_in(query, operations())
        .into_iter()
        .map(|(words, rank)| Hit {
            long_name: &words.long_name,
            title: &words.title,
            rank,
        })
        .collect()
}

/// [`search`] over any list of operations' words.
fn search_in<'a>(query: &str, all: &'a [OperationWords]) -> Vec<(&'a OperationWords, Rank)> {
    let mut hits: Vec<_> = all
        .iter()
        .filter_map(|words| rank(query, &words.into()).map(|rank| (words, rank)))
        .collect();
    hits.sort_by(|(a, ra), (b, rb)| {
        ra.tier
            .cmp(&rb.tier)
            .then(rb.score.cmp(&ra.score))
            .then(a.long_name.cmp(&b.long_name))
    });
    hits
}

/// The match with the highest score, the first among equals.
fn best(matches: impl Iterator<Item = (i32, Matched)>) -> Option<(i32, Matched)> {
    matches.fold(None, |kept, (score, matched)| match kept {
        Some((top, _)) if top >= score => kept,
        _ => Some((score, matched)),
    })
}

/// Where `query` starts `text` or a word of it, as a score: 0 at the start of
/// the text, lower for each character further in. A word starts the text or
/// follows a character that is neither a letter, a digit nor an apostrophe,
/// so "kee" starts "Keep Only" but not "Peek".
fn word_prefix(query: &str, text: &str) -> Option<i32> {
    let mut previous: Option<char> = None;
    for (at, c) in text.char_indices() {
        let starts_word = previous.is_none_or(|p| !(p.is_alphanumeric() || p == '\''));
        if starts_word && c.is_alphanumeric() && text[at..].starts_with(query) {
            return Some(-(text[..at].chars().count() as i32));
        }
        previous = Some(c);
    }
    None
}

/// How well `query`'s characters appear in order in `text`, or `None` when
/// they do not. Each matched character scores one, more when it starts a word
/// or follows the previous match directly; the best placement is kept.
fn subsequence(query: &[char], text: &str) -> Option<i32> {
    const WORD_START: i32 = 4;
    const RUN: i32 = 5;
    let text: Vec<char> = text.chars().collect();
    if query.is_empty() || query.len() > text.len() {
        return None;
    }
    // best[j]: the best score with the query so far matched and its last
    // character placed at text[j].
    let mut best: Vec<Option<i32>> = vec![None; text.len()];
    for (i, q) in query.iter().enumerate() {
        let mut next = vec![None; text.len()];
        // The best score of any placement ending before j, for a gap.
        let mut before: Option<i32> = None;
        for j in 0..text.len() {
            if text[j] == *q {
                let start = j == 0 || !text[j - 1].is_alphanumeric();
                let gain = 1 + if start { WORD_START } else { 0 };
                let placed = if i == 0 {
                    Some(gain)
                } else {
                    let run = (j > 0)
                        .then(|| best[j - 1])
                        .flatten()
                        .map(|s| s + gain + RUN);
                    let gap = before.map(|s| s + gain);
                    run.max(gap)
                };
                next[j] = placed;
            }
            if i > 0 {
                before = before.max(best[j]);
            }
        }
        best = next;
    }
    best.into_iter().flatten().max()
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

    fn words(long_name: &str) -> &'static OperationWords {
        of(long_name).unwrap_or_else(|| panic!("{long_name} has words"))
    }

    fn ranked(query: &str, long_name: &str) -> Option<Rank> {
        rank(query, &words(long_name).into())
    }

    fn alias(word: &str) -> Matched {
        Matched::Alias {
            word: word.to_string(),
            source: None,
        }
    }

    /// One row per tier, each with what the query matched. Where an operation's
    /// own name and an alias both match, the better tier wins.
    #[test]
    fn ranks_follow_the_six_tiers() {
        let rows = [
            ("Fill in missing values", "fill-nulls", 1, Matched::Title),
            ("fill-nulls", "fill-nulls", 1, Matched::LongName),
            ("fill nulls", "fill-nulls", 1, Matched::LongName),
            ("derive", "derive-column", 2, Matched::LongName),
            ("column", "hide-column", 2, Matched::LongName),
            ("filter", "filter-rows", 2, Matched::Title),
            ("missing val", "fill-nulls", 2, Matched::Title),
            ("keep only", "filter-rows", 3, alias("Keep Only")),
            ("kee", "filter-rows", 4, alias("Keep")),
            ("keep on", "filter-rows", 4, alias("Keep Only")),
            ("fltr", "filter-rows", 5, Matched::LongName),
            ("each missing value", "fill-nulls", 6, Matched::Sentence),
        ];
        for (query, long_name, tier, matched) in rows {
            let got = ranked(query, long_name)
                .unwrap_or_else(|| panic!("{query:?} does not match {long_name}"));
            assert_eq!(
                (got.tier, &got.matched),
                (tier, &matched),
                "{query:?} on {long_name}"
            );
        }
    }

    #[test]
    fn an_alias_match_carries_its_source() {
        let got = ranked("fill_null", "fill-nulls").expect("fill_null matches");
        assert_eq!(got.tier, 3);
        assert_eq!(
            got.matched,
            Matched::Alias {
                word: "fill_null".to_string(),
                source: Some("Polars".to_string()),
            }
        );
    }

    #[test]
    fn aliases_never_match_fuzzily() {
        // "kp nly" is a subsequence of the alias "Keep Only", and of no name.
        let got = ranked("kp nly", "filter-rows");
        assert!(
            got.as_ref()
                .is_none_or(|r| !matches!(r.matched, Matched::Alias { .. })),
            "{got:?}"
        );
    }

    #[test]
    fn peek_does_not_find_keep() {
        for hit in search("eek") {
            assert!(
                !matches!(hit.rank.matched, Matched::Alias { .. }),
                "eek matched an alias of {}: {:?}",
                hit.long_name,
                hit.rank
            );
        }
    }

    #[test]
    fn case_spacing_and_apostrophes_fold() {
        for query in [
            "CHANGE A COLUMN\u{2019}S TYPE ",
            "change a column's type",
            "  Change   a column's   type",
        ] {
            let got = ranked(query, "cast-column").expect("the title matches");
            assert_eq!((got.tier, got.matched), (1, Matched::Title), "{query:?}");
        }
        let got = ranked("KEEP ONLY ", "filter-rows").expect("the alias matches");
        assert_eq!((got.tier, got.matched), (3, alias("Keep Only")));
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(search("").is_empty());
        assert!(search("   ").is_empty());
    }

    #[test]
    fn filter_lists_filter_rows_before_any_alias_named_filter() {
        let hits = search("filter");
        let at = hits
            .iter()
            .position(|h| h.long_name == "filter-rows")
            .expect("filter finds filter-rows");
        assert!(
            hits.iter()
                .any(|h| h.long_name == "hide-column" && h.rank.tier == 3),
            "hide-column's alias \"Filter\" is listed: {hits:?}"
        );
        for hit in &hits[..at] {
            assert!(
                hit.rank.tier < 3,
                "{} lists before filter-rows",
                hit.long_name
            );
        }
    }

    #[test]
    fn hits_order_by_tier_then_score_then_long_name() {
        let hits = search("column");
        assert!(hits.len() > 1, "{hits:?}");
        for pair in hits.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                (a.rank.tier, -a.rank.score, a.long_name)
                    <= (b.rank.tier, -b.rank.score, b.long_name),
                "{a:?} lists before {b:?}"
            );
        }
    }

    #[test]
    fn a_word_shared_by_operations_lists_each_once() {
        let fixture = |long_name: &str| OperationWords {
            long_name: long_name.to_string(),
            title: format!("Title of {long_name}"),
            sentence: "A sentence.".to_string(),
            aliases: vec![Alias {
                word: "Select".to_string(),
                source: None,
            }],
        };
        let all = [fixture("pick-b"), fixture("pick-a")];
        let hits: Vec<&str> = search_in("select", &all)
            .into_iter()
            .map(|(words, rank)| {
                assert_eq!(rank.tier, 3);
                words.long_name.as_str()
            })
            .collect();
        assert_eq!(hits, ["pick-a", "pick-b"]);

        // Three of arc's operations carry the alias "SELECT".
        let hits = search("select");
        let mut names: Vec<&str> = hits.iter().map(|h| h.long_name).collect();
        let count = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), count, "an operation is listed twice: {hits:?}");
        for long_name in ["cast-column", "hide-column", "rename-column"] {
            assert!(names.contains(&long_name), "select finds {long_name}");
        }
    }

    #[test]
    fn a_verb_with_no_title_matches_on_its_long_name_and_help() {
        let verb = Candidate {
            title: None,
            long_name: "undo-last-step",
            aliases: &[],
            sentence: "Take back the step you recorded last.",
        };
        assert_eq!(rank("undo-last-step", &verb).map(|r| r.tier), Some(1));
        assert_eq!(rank("last", &verb).map(|r| r.tier), Some(2));
        assert_eq!(rank("unls", &verb).map(|r| r.tier), Some(5));
        assert_eq!(rank("recorded last", &verb).map(|r| r.tier), Some(6));
        assert_eq!(rank("zzz", &verb), None);
    }

    #[test]
    fn the_subsequence_scorer_rewards_runs_and_word_starts() {
        let q: Vec<char> = "fr".chars().collect();
        // f and r each start a word in "filter rows", and neither does in "offer".
        assert!(subsequence(&q, "filter rows") > subsequence(&q, "offer"));
        let q: Vec<char> = "fil".chars().collect();
        assert!(subsequence(&q, "file") > subsequence(&q, "f i l"));
        assert_eq!(subsequence(&q, "lif"), None);
    }
}
