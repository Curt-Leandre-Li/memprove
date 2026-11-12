//! U5 P2 (Owner O4, D1 F2; ADR 0014 C4, "Amendment (U5, Z-P)"): the shape of
//! a memprove token in free text, the one rule `memproved`, the CLI, and
//! the MCP server apply.
//!
//! A token secret is 32 random bytes as 43 unpadded base64url characters.
//! C4 (U1) refused any maximal run of exactly 43 characters of `A-Z`, `a-z`,
//! `0-9`, `-`, and `_`; a long identifier such as the test name
//! `test_calculate_metrics_rejects_empty_series` is such a run, and D1 met
//! the refusal with it. The rule is now that run *with* at least one
//! upper-case letter, one lower-case letter, and one digit: almost every
//! token has all three (a random one lacks one of them with a probability
//! of about 6.5 × 10⁻⁴, nearly all of it "no digit"), and an identifier in
//! `snake_case`, `kebab-case`, or `camelCase` without a digit does not.
//! `memproved` also compares each such run, whatever its characters,
//! with the tokens it issued ([`token_candidates`]); that comparison needs
//! the registry and is the daemon's.

/// The characters of a token secret: 32 bytes as unpadded base64url.
pub const TOKEN_SECRET_CHARS: usize = 43;

/// Whether `character` is one of a token's alphabet (`A-Z`, `a-z`, `0-9`,
/// `-`, `_`).
const fn in_alphabet(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

/// Every maximal run of exactly [`TOKEN_SECRET_CHARS`] characters of a
/// token's alphabet in `text`, whatever classes of characters it holds: the
/// strings `memproved` compares with the tokens it issued. A run of 42 or
/// 44 such characters is none (a token has a space, a quote, `=`, a colon,
/// a dot, or the end of the text next to it).
pub fn token_candidates(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !in_alphabet(character))
        .filter(|run| run.len() == TOKEN_SECRET_CHARS)
}

/// Whether `candidate`, a run of [`token_candidates`], has the shape of a
/// token: at least one upper-case letter, one lower-case letter, and one
/// digit.
#[must_use]
pub fn is_token_shaped(candidate: &str) -> bool {
    candidate.len() == TOKEN_SECRET_CHARS
        && candidate.chars().all(in_alphabet)
        && candidate
            .chars()
            .any(|character| character.is_ascii_uppercase())
        && candidate
            .chars()
            .any(|character| character.is_ascii_lowercase())
        && candidate
            .chars()
            .any(|character| character.is_ascii_digit())
}

/// U5 P2 (Owner O4 a): whether `text` holds a string in the format of a
/// memprove token: a maximal run of exactly 43 characters of `A-Z`,
/// `a-z`, `0-9`, `-`, and `_` with at least one upper-case letter, one
/// lower-case letter, and one digit.
#[must_use]
pub fn holds_token_shape(text: &str) -> bool {
    token_candidates(text).any(is_token_shaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D1's test name: 43 characters, no upper-case letter, no digit.
    const D1_TEST_NAME: &str = "test_calculate_metrics_rejects_empty_series";
    /// 43 characters of all three classes (and `-`, `_`).
    const MIXED: &str = "Ab3_cDe-4fGh5iJk6lMn7oPq8rSt9uVw0xYz1AbCd2E";

    #[test]
    fn three_classes_make_the_shape() {
        assert_eq!(D1_TEST_NAME.len(), 43);
        assert_eq!(MIXED.len(), 43);
        assert!(!holds_token_shape(D1_TEST_NAME));
        assert_eq!(
            token_candidates(D1_TEST_NAME).collect::<Vec<_>>(),
            [D1_TEST_NAME]
        );
        assert!(holds_token_shape(MIXED));
        for (text, shaped) in [
            (format!("ran {MIXED} once"), true),
            (format!("last:{MIXED}"), true),
            (format!("é{MIXED}ü"), true),
            (format!("{MIXED}="), true),
            (format!("x{MIXED}"), false),
            (MIXED[..42].to_owned(), false),
            (format!("{} {}", &MIXED[..42], "a".repeat(44)), false),
            (format!("{}.{}", &MIXED[..20], &MIXED[20..]), false),
            ("A".repeat(43), false),
            ("a".repeat(43), false),
            ("7".repeat(43), false),
            (format!("{}a1", "B".repeat(41)), true),
            (format!("{}a", "1".repeat(42)), false),
            (format!("{}B", "a1".repeat(21)), true),
            ("a1".repeat(21) + "b", false),
            ("01a00000-0000-7000-8000-000000000001".to_owned(), false),
        ] {
            assert_eq!(holds_token_shape(&text), shaped, "{text}");
        }
    }

    #[test]
    fn a_candidate_is_any_run_of_43_whatever_its_classes() {
        let text = format!("{D1_TEST_NAME} and {MIXED}; {}", "z".repeat(44));
        assert_eq!(
            token_candidates(&text).collect::<Vec<_>>(),
            [D1_TEST_NAME, MIXED]
        );
        assert!(!is_token_shaped(D1_TEST_NAME));
        assert!(is_token_shaped(MIXED));
        assert!(!is_token_shaped(&MIXED[1..]));
    }
}
