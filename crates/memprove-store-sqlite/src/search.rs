//! U3 G4 (Owner O8; ADR 0016, "Query selection"): the pieces of the
//! in-memory statement index that `search.v2` ([`crate::search_v2`]) is
//! built from, for `memprove resume --pack --query` and the MCP `pack`
//! tool's `query`: the error type, the `SQLite` version, and how a query is
//! split into terms and quoted for FTS5.
//!
//! The index is built in the calling process, in memory, from the
//! statements the caller passes (the claims of one Task Capsule, superseded
//! and withdrawn ones too) and dropped when the call returns: nothing is
//! persisted, no database of the data directory is opened, and nothing is
//! written. It is an `SQLite` FTS5 table with the `trigram` tokenizer (the
//! bundled `SQLite` is built with FTS5), ranked by `bm25`.
//!
//! U16 (M1 O2) removed the query profiles, and with them `search.v1`
//! (`rank_statements`), which no code called any more (U16-N1); U17 (M1 O3)
//! took its last pieces out of this module.

use std::collections::BTreeSet;

use thiserror::Error;

/// The index could not be built or read.
#[derive(Debug, Error)]
#[error("the in-memory statement index could not be built or read: {0}")]
pub struct SearchError(String);

impl SearchError {
    pub(crate) fn new(why: &str) -> Self {
        Self(why.to_owned())
    }
}

impl From<rusqlite::Error> for SearchError {
    fn from(error: rusqlite::Error) -> Self {
        Self(error.to_string())
    }
}

/// U4 S2 (Owner O8): the version of the `SQLite` this binary links (the
/// bundled one), whose FTS5 index ranks a query; a query Pack's
/// `ContextManifest` names it.
#[must_use]
pub fn sqlite_version() -> &'static str {
    rusqlite::version()
}

/// The terms of `query`: its whitespace-separated words, each once, in
/// their first order.
pub(crate) fn terms(query: &str) -> Vec<&str> {
    let mut seen = BTreeSet::new();
    query
        .split(char::is_whitespace)
        .filter(|term| !term.is_empty() && seen.insert(*term))
        .collect()
}

/// An FTS5 string holding `term` literally.
pub(crate) fn fts5_string(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

pub(crate) fn folded(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
mod tests {
    use super::sqlite_version;

    /// The bundled `SQLite` has FTS5 and its `trigram` tokenizer: a table
    /// with it is created and matches a substring inside a word, which
    /// the default tokenizer does not.
    #[test]
    fn the_bundled_sqlite_has_fts5_and_the_trigram_tokenizer() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let options: Vec<String> = connection
            .prepare("PRAGMA compile_options")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            options.iter().any(|option| option == "ENABLE_FTS5"),
            "{options:?}"
        );
        connection
            .execute_batch(
                "CREATE VIRTUAL TABLE t USING fts5(x, tokenize = 'trigram');
                 INSERT INTO t (rowid, x) VALUES (1, 'the omission levels'), (2, 'budget and the omission levels');",
            )
            .unwrap();
        let hits = |query: &str| -> Vec<i64> {
            connection
                .prepare("SELECT rowid FROM t WHERE t MATCH ?1 ORDER BY rowid")
                .unwrap()
                .query_map([query], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(hits("\"missio\""), [1, 2]);
        assert_eq!(hits("\"OMISSION\""), [1, 2]);
        assert_eq!(hits("\"udget\""), [2]);
        // Fewer than three characters match nothing through the index.
        assert!(hits("\"bu\"").is_empty());
        // The default tokenizer matches whole words only.
        connection
            .execute_batch(
                "CREATE VIRTUAL TABLE u USING fts5(x);
                 INSERT INTO u (rowid, x) VALUES (1, 'the omission levels');",
            )
            .unwrap();
        let words: Vec<i64> = connection
            .prepare("SELECT rowid FROM u WHERE u MATCH '\"missio\"'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(words.is_empty());
    }

    /// U4 S2: the version a manifest names is the linked `SQLite`'s.
    #[test]
    fn the_sqlite_version_is_the_linked_one() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let version: String = connection
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sqlite_version(), version);
        assert!(version.starts_with("3."), "{version}");
    }
}
