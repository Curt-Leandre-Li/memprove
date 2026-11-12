//! U4 S2 (Owner O6, O7; ADR 0016, "search.v2"): the ranking of a query
//! against one Task's statements, built in the calling process, in memory,
//! from the statements the caller passes (every claim of one Task Capsule),
//! and dropped on return. U16 (M1 O2) made it the only ranking (the query
//! profiles and `search.v1` are gone).
//!
//! The query is split at whitespace into terms, each once. What a term
//! matches depends on its length (rule a, which covers N-4):
//!
//! - three characters or more: the trigram FTS5 index, a substring of the
//!   text or the rationale, case folded, ranked by `bm25`;
//! - one or two characters: a whole word only, after
//!   [`char::to_lowercase`]: an occurrence whose neighbour on a side where
//!   the term has a word character is not a word character ([`is_word`]:
//!   alphanumeric), so `ID` matches `ID`, `ID:7`, and `the ID`, and not
//!   `valid`, `guide`, or `UUIDv7`.
//!
//! U17 (M1 O3): the text is English, and only this path is kept. Before
//! U17 a term of two Chinese, Japanese, or Korean characters matched as a
//! substring and a single one matched nothing; those rules are gone, so a
//! two-character term of any script is a whole word and the shortest
//! indexed term is three characters of any script.
//!
//! A term that is, whole, the label of a statement's kind (`fact`,
//! `open_item`, `decision`, `next_action`:
//! [`memprove_domain::pack_kind_label`]) also matches that statement,
//! unscored (rule f).
//!
//! The order (rule b): with several terms, the statements every term
//! matches come before those only some match; within each layer the
//! active statements before the superseded and withdrawn ones (rule e);
//! within each of those, the statements a three-character term matched by
//! `bm25` ascending, then the others; ties and the others by number
//! ascending.

use std::{cmp::Ordering, collections::BTreeSet};

use memprove_domain::PACK_SEARCH_TOKENIZER;
use rusqlite::{Connection, params};

use crate::search::{SearchError, folded, fts5_string, terms};

/// U4 S2 rule (a): the fewest characters of a term the index matches as
/// a substring; a shorter one matches whole words only.
pub const SEARCH_V2_MIN_TERM_CHARS: usize = 3;

/// One statement to index for `search.v2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchEntry<'a> {
    /// The claim number (`#n`).
    pub number: u64,
    /// The statement's text.
    pub text: &'a str,
    /// A Decision's rationale.
    pub rationale: Option<&'a str>,
    /// The label of its kind as a query term
    /// ([`memprove_domain::pack_kind_label`]: `fact`, `open_item`,
    /// `decision`, `next_action`).
    pub kind_label: &'a str,
    /// Whether it is active (neither superseded nor withdrawn).
    pub active: bool,
}

/// A word character for rule (a)'s whole words: alphanumeric.
fn is_word(character: char) -> bool {
    character.is_alphanumeric()
}

/// How one term matches (rule a).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    /// Three characters or more: the trigram index.
    Indexed(String),
    /// One or two characters: a folded whole word.
    Word(String),
}

impl Term {
    fn text(&self) -> &str {
        match self {
            Self::Indexed(text) | Self::Word(text) => text,
        }
    }
}

fn classify(term: &str) -> Term {
    if term.chars().count() >= SEARCH_V2_MIN_TERM_CHARS {
        Term::Indexed(term.to_owned())
    } else {
        Term::Word(folded(term))
    }
}

/// Whether the folded `word` occurs in the folded `text` as a whole word.
fn holds_word(text: &str, word: &str) -> bool {
    let (Some(first), Some(last)) = (word.chars().next(), word.chars().next_back()) else {
        return false;
    };
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        let joined_before = is_word(first) && before.is_some_and(is_word);
        let joined_after = is_word(last) && after.is_some_and(is_word);
        !(joined_before || joined_after)
    })
}

/// What one statement matched: the terms (by index) and its `bm25` score
/// when a three-character term matched it.
#[derive(Debug, Default)]
struct Matched {
    terms: BTreeSet<usize>,
    score: Option<f64>,
}

/// The `bm25` score of every statement the indexed terms match, and which
/// of them matches which statement.
fn indexed(
    entries: &[SearchEntry<'_>],
    terms: &[(usize, &str)],
    matched: &mut [Matched],
) -> Result<(), SearchError> {
    if terms.is_empty() {
        return Ok(());
    }
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(&format!(
        "CREATE VIRTUAL TABLE statement USING fts5(text, rationale, tokenize = '{PACK_SEARCH_TOKENIZER}');"
    ))?;
    {
        let mut insert = connection
            .prepare("INSERT INTO statement (rowid, text, rationale) VALUES (?1, ?2, ?3)")?;
        for (index, entry) in entries.iter().enumerate() {
            let rowid = i64::try_from(index)
                .map_err(|_| SearchError::new("more statements than SQLite rows"))?;
            insert.execute(params![rowid, entry.text, entry.rationale])?;
        }
    }
    let rows = |expression: String| -> Result<Vec<(usize, f64)>, SearchError> {
        let mut select = connection
            .prepare("SELECT rowid, bm25(statement) FROM statement WHERE statement MATCH ?1")?;
        let found = select
            .query_map([expression], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        found
            .into_iter()
            .map(|(rowid, score)| {
                usize::try_from(rowid)
                    .map(|index| (index, score))
                    .map_err(|_| SearchError::new("a negative rowid"))
            })
            .collect()
    };
    let expression = terms
        .iter()
        .map(|(_, term)| fts5_string(term))
        .collect::<Vec<_>>()
        .join(" OR ");
    for (index, score) in rows(expression)? {
        matched[index].score = Some(score);
    }
    for (number, term) in terms {
        for (index, _) in rows(fts5_string(term))? {
            matched[index].terms.insert(*number);
        }
    }
    Ok(())
}

/// The numbers of the statements `query` matches by `search.v2`, best
/// first (see the module documentation).
///
/// # Errors
///
/// [`SearchError`] when the in-memory index cannot be built or queried
/// (an `SQLite` without FTS5 or the trigram tokenizer).
pub fn rank_statements_v2(
    entries: &[SearchEntry<'_>],
    query: &str,
) -> Result<Vec<u64>, SearchError> {
    let terms = terms(query).into_iter().map(classify).collect::<Vec<_>>();
    let mut matched = entries
        .iter()
        .map(|_| Matched::default())
        .collect::<Vec<_>>();
    let long = terms
        .iter()
        .enumerate()
        .filter_map(|(index, term)| match term {
            Term::Indexed(text) => Some((index, text.as_str())),
            Term::Word(_) => None,
        })
        .collect::<Vec<_>>();
    indexed(entries, &long, &mut matched)?;
    for (entry, found) in entries.iter().zip(&mut matched) {
        let text = folded(entry.text);
        let rationale = entry.rationale.map(folded).unwrap_or_default();
        for (index, term) in terms.iter().enumerate() {
            let holds = match term {
                Term::Indexed(_) => false,
                Term::Word(word) => holds_word(&text, word) || holds_word(&rationale, word),
            };
            // Rule (f): a term that is the statement's kind label.
            if holds || term.text() == entry.kind_label {
                found.terms.insert(index);
            }
        }
    }
    let mut hits = entries
        .iter()
        .zip(&matched)
        .filter(|(_, found)| !found.terms.is_empty())
        .collect::<Vec<_>>();
    // Rule (b): every term matched (layer 0) before some terms matched.
    let layer = |found: &Matched| usize::from(found.terms.len() < terms.len());
    hits.sort_by(|(left, left_found), (right, right_found)| {
        layer(left_found)
            .cmp(&layer(right_found))
            // Rule (e): within a layer, active before superseded or
            // withdrawn.
            .then(right.active.cmp(&left.active))
            .then(order(left_found.score, right_found.score))
            .then(left.number.cmp(&right.number))
    });
    let mut seen = BTreeSet::new();
    Ok(hits
        .into_iter()
        .map(|(entry, _)| entry.number)
        .filter(|number| seen.insert(*number))
        .collect())
}

/// Scored before unscored; scores ascending (`bm25`: the best first).
fn order(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::{SearchEntry, rank_statements_v2};

    fn entry(number: u64, text: &'static str) -> SearchEntry<'static> {
        SearchEntry {
            number,
            text,
            rationale: None,
            kind_label: "fact",
            active: true,
        }
    }

    fn rank(entries: &[SearchEntry<'_>], query: &str) -> Vec<u64> {
        rank_statements_v2(entries, query).unwrap()
    }

    /// Rule (a), N-4: a term of one or two characters matches only a whole
    /// word, never the inside of a longer word.
    #[test]
    fn a_two_letter_term_no_longer_matches_a_substring() {
        let entries = [
            entry(1, "the task ID is printed"),
            entry(2, "a valid guide"),
            entry(3, "memproved builds UUIDv7 ids"),
            entry(4, "for example ID must not hit valid"),
            entry(5, "UI and CLI"),
            entry(6, "the design of search.v2"),
            entry(7, "ID:7 and (id)"),
            entry(8, "IDs and v20"),
            entry(9, "the short word ID, then a comma"),
        ];
        assert_eq!(rank(&entries, "ID"), [1, 4, 7, 9]);
        assert_eq!(rank(&entries, "id"), [1, 4, 7, 9]);
        assert_eq!(rank(&entries, "UI"), [5]);
        assert_eq!(rank(&entries, "v2"), [6]);
        assert_eq!(rank(&entries, "I"), Vec::<u64>::new());
        // A term of three or more characters still matches inside a word,
        // through the trigram index.
        assert_eq!(rank(&entries, "uuid"), [3]);
    }

    /// U17: a two-character term of any script is a whole word, and a
    /// single character of any script is one too (the CJK rules of U4 S2
    /// are gone with the Chinese text).
    #[test]
    fn two_character_terms_of_any_script_are_whole_words() {
        let entries = [
            entry(1, "le budget est 9000"),
            entry(2, "où est le budget"),
            entry(3, "Pack budget"),
        ];
        assert_eq!(rank(&entries, "le"), [1, 2]);
        assert_eq!(rank(&entries, "où"), [2]);
        assert!(rank(&entries, "l").is_empty());
        assert!(rank(&entries, "ou").is_empty());
    }

    /// As in `search.v1`, a query is literal text, never FTS5 syntax.
    #[test]
    fn a_query_is_literal_text_never_fts5_syntax() {
        let entries = [
            entry(1, "say \"hi\" OR NEAR(a b) text:col"),
            entry(2, "plain text"),
        ];
        for query in ["\"hi\"", "OR", "NEAR(a", "text:col", "*", "\"", "-plain"] {
            assert!(!rank(&entries, query).contains(&2), "{query}");
        }
        assert_eq!(rank(&entries, "\"hi\""), [1]);
        assert_eq!(rank(&entries, "NEAR(a"), [1]);
        assert!(rank(&entries, "   ").is_empty());
    }

    /// Rule (b): with several terms, the statements every term matches
    /// come before those only some match, each layer by `bm25`, then the
    /// unscored ones by number.
    #[test]
    fn every_term_matched_ranks_before_some_terms_matched() {
        let entries = [
            entry(1, "budget budget budget"),
            entry(
                2,
                "a much longer statement that names the budget once and the level once among many words",
            ),
            entry(3, "level level"),
            entry(4, "the budget level"),
            entry(5, "an ok"),
            entry(6, "level, ok, and many other words that dilute it"),
            entry(7, "nothing"),
        ];
        // Alone, bm25 ranks the short repeated statements first.
        let alone = rank(&entries, "budget");
        assert_eq!(alone.first(), Some(&1), "{alone:?}");
        let ranked = rank(&entries, "budget level");
        assert_eq!(&ranked[..2], [4, 2], "{ranked:?}");
        let mut rest = ranked[2..].to_vec();
        rest.sort_unstable();
        assert_eq!(rest, [1, 3, 6]);
        // A two-character term counts as a term: #6 holds both.
        let ranked = rank(&entries, "ok level");
        assert_eq!(ranked[0], 6, "{ranked:?}");
        assert_eq!(ranked.len(), 5, "{ranked:?}");
        // Partial matches: those with a bm25 score first, then the rest.
        assert_eq!(ranked.last(), Some(&5), "{ranked:?}");
        // One term: one layer.
        assert_eq!(rank(&entries, "ok"), [5, 6]);
    }

    /// Rule (e): within a layer, the active statements come before the
    /// superseded and withdrawn ones; the layers still come first.
    #[test]
    fn active_statements_rank_before_retired_ones_within_a_layer() {
        let retired = |number, text| SearchEntry {
            active: false,
            ..entry(number, text)
        };
        let entries = [
            retired(1, "the matches are ranked by bm25"),
            retired(2, "the matches are ranked by bm25"),
            entry(
                3,
                "the matches are ranked by bm25, and within a layer the active statements come before the superseded ones, plus many words",
            ),
            retired(4, "by only"),
            entry(5, "by also"),
        ];
        // The active #3 first though bm25 scores the shorter #1 and #2
        // better; the retired ones keep their bm25 and number order.
        assert_eq!(rank(&entries, "bm25"), [3, 1, 2]);
        // Unscored matches too: active first, then by number.
        assert_eq!(rank(&entries, "by"), [3, 5, 1, 2, 4]);
        // Every term matched still comes first, retired or not.
        assert_eq!(rank(&entries, "bm25 by")[..3], [3, 1, 2]);
    }

    /// Rule (f), the Owner's revision of O6: a term that is a kind's label
    /// (`fact`, `open_item`, `decision`, `next_action`) matches every
    /// statement of that kind, besides the texts that hold it; only the
    /// whole label does.
    #[test]
    fn a_kind_label_matches_the_statements_of_that_kind() {
        let of = |number, text, kind_label| SearchEntry {
            kind_label,
            ..entry(number, text)
        };
        let entries = [
            of(1, "the index uses the trigram tokenizer", "decision"),
            of(
                2,
                "the default profile is a decision of the evaluation",
                "open_item",
            ),
            of(3, "Use bm25 for ranking", "decision"),
            of(4, "write the fixed-corpus evaluation first", "next_action"),
            of(5, "measure the baseline before the next_action", "fact"),
            of(6, "unrelated", "fact"),
        ];
        // #2 says it in its text and gets a bm25 score; the label matches of #1 and #3 are
        // unscored and follow by number.
        assert_eq!(rank(&entries, "decision"), [2, 1, 3]);
        let next = rank(&entries, "next_action");
        let mut sorted = next.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, [4, 5], "{next:?}");
        assert_eq!(rank(&entries, "fact"), [5, 6]);
        assert_eq!(rank(&entries, "open_item"), [2]);
        // Only the whole label: a longer word does not match as a label.
        assert!(rank(&entries, "decisions").is_empty());
        // `action` is in #5's text, not #4's label.
        assert_eq!(rank(&entries, "action"), [5]);
        // A label term is a term of the layers: #3 holds both terms and comes first; of the
        // one-term matches, #2 (scored by bm25 on its text) precedes #1 (label only).
        assert_eq!(rank(&entries, "decision bm25"), [3, 2, 1]);
    }
}
