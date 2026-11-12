//! P4-A (Owner, T17 N-1): the boundary of the promise that every position
//! of up to eight overruled citations fits in the `createCheckpoint` detail
//! (`overruled::overruled_detail`, 512 characters).
//!
//! Measured: with eight citations in the longest section
//! (`verified_completed_work`), every position fits while the decimal digits
//! of the statement and Evidence indices of the eight positions add up to at
//! most 53; so whenever each index is below 1000 (at most six digits per
//! position, 48 in all). With 54 digits or more (for example every
//! statement index from 1000 and every Evidence index from 100) the last
//! position no longer fits and the sentence ends `, and 1 more.`.

use std::fmt::Write as _;

use memprove_domain::EvidenceId;
use memprove_protocol::phase1::PROBLEM_DETAIL_MAX_CHARS;
use memprove_store_sqlite::OverruledCitation;

use crate::overruled::overruled_detail;

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn digits(value: usize) -> usize {
    value.to_string().len()
}

fn citation(position: String, by_test_run: bool) -> OverruledCitation {
    OverruledCitation {
        position,
        evidence_id: EvidenceId::new(),
        later: EvidenceId::new(),
        by_test_run,
    }
}

/// Eight citations of `verified_completed_work`, the k-th at
/// `[statements[k]].evidence_ids[evidence[k]]`.
fn eight(
    statements: [usize; 8],
    evidence: [usize; 8],
    by_test_run: bool,
) -> Vec<OverruledCitation> {
    statements
        .iter()
        .zip(evidence)
        .map(|(statement, index)| {
            citation(
                format!("$.verified_completed_work[{statement}].evidence_ids[{index}]"),
                by_test_run,
            )
        })
        .collect()
}

/// The description the daemon writes for the `number`-th citation, spelled
/// out here so that a cut one is not mistaken for a whole one.
fn description(number: usize, citation: &OverruledCitation) -> String {
    if citation.by_test_run {
        format!(
            "({number}) test run Evidence {} passed; the latest test run {} did not pass: it \
             needs a later passed test run.",
            citation.evidence_id, citation.later
        )
    } else {
        format!(
            "({number}) Evidence {} passed; the later run {} of its argv in this state did not \
             pass: rerun until it passes.",
            citation.evidence_id, citation.later
        )
    }
}

/// The first sentence when every position fits.
fn all_positions(citations: &[OverruledCitation]) -> String {
    let named = citations
        .iter()
        .enumerate()
        .map(|(index, citation)| format!("({}) {}", index + 1, citation.position))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} verified_fact citations name overruled passed runs: {named}.",
        citations.len()
    )
}

/// The first sentence when only the first `named` positions fit.
fn some_positions(citations: &[OverruledCitation], named: usize) -> String {
    let count = citations.len();
    let mut listed = String::new();
    for (index, citation) in citations[..named].iter().enumerate() {
        write!(listed, "({}) {}, ", index + 1, citation.position).expect("a String takes it");
    }
    format!(
        "{count} verified_fact citations name overruled passed runs: {listed}and {} more.",
        count - named
    )
}

/// Checks the detail's shape and returns how many positions it names.
/// Either every position is named, followed only by whole descriptions and
/// at most an ending that counts the rest; or the sentence names a prefix
/// and ends `, and <m> more.` with nothing after it.
fn check(citations: &[OverruledCitation]) -> usize {
    let count = citations.len();
    let detail = overruled_detail(citations);
    assert!(chars(&detail) <= PROBLEM_DETAIL_MAX_CHARS, "{detail}");
    let first = all_positions(citations);
    if let Some(rest) = detail.strip_prefix(&first) {
        let mut rebuilt = String::new();
        let mut described = 0;
        for (index, citation) in citations.iter().enumerate() {
            let item = format!(" {}", description(index + 1, citation));
            if !rest[rebuilt.len()..].starts_with(&item) {
                break;
            }
            rebuilt.push_str(&item);
            described += 1;
        }
        if described < count {
            write!(rebuilt, " {} more not described.", count - described)
                .expect("a String takes it");
        }
        assert_eq!(rest, rebuilt, "{detail}");
        return count;
    }
    let named = (0..count)
        .rev()
        .find(|named| detail == some_positions(citations, *named))
        .unwrap_or_else(|| panic!("neither every position nor a prefix: {detail}"));
    assert!(detail.ends_with(&format!("and {} more.", count - named)));
    named
}

#[test]
fn eight_positions_fit_while_every_index_is_below_one_thousand() {
    for by_test_run in [false, true] {
        for statement in [0, 9, 10, 99, 100, 999] {
            for index in [0, 9, 10, 99, 100, 999] {
                let citations = eight([statement; 8], [index; 8], by_test_run);
                assert_eq!(check(&citations), 8, "[{statement}].evidence_ids[{index}]");
            }
        }
        // The largest spread of indices below 1000 across the eight.
        let citations = eight(
            [992, 993, 994, 995, 996, 997, 998, 999],
            [999, 998, 997, 996, 995, 994, 993, 992],
            by_test_run,
        );
        assert_eq!(check(&citations), 8);
    }
}

#[test]
fn eight_positions_fit_exactly_while_their_index_digits_add_up_to_at_most_53() {
    // Spread the digits over the eight positions, statement index first.
    let spread = |total: usize| -> ([usize; 8], [usize; 8]) {
        let mut statements = [0usize; 8];
        let mut evidence = [0usize; 8];
        for position in 0..8 {
            let share = total / 8 + usize::from(position < total % 8);
            let statement_digits = share.div_ceil(2);
            let evidence_digits = share - statement_digits;
            statements[position] = 10usize.pow(u32::try_from(statement_digits - 1).unwrap());
            evidence[position] = if evidence_digits == 0 {
                0
            } else {
                10usize.pow(u32::try_from(evidence_digits - 1).unwrap())
            };
            let actual = digits(statements[position]) + digits(evidence[position]);
            assert_eq!(actual, share);
        }
        (statements, evidence)
    };
    for total in [16, 48, 52, 53] {
        let (statements, evidence) = spread(total);
        for by_test_run in [false, true] {
            assert_eq!(
                check(&eight(statements, evidence, by_test_run)),
                8,
                "{total} digits"
            );
        }
    }
    for total in [54, 56, 64] {
        let (statements, evidence) = spread(total);
        for by_test_run in [false, true] {
            assert_eq!(
                check(&eight(statements, evidence, by_test_run)),
                7,
                "{total} digits"
            );
        }
    }
}

#[test]
fn beyond_the_bound_the_sentence_counts_the_rest_and_nothing_is_cut() {
    // T17 N-1: statement indices from 1000, Evidence indices from 100.
    let citations = eight(
        [1000, 1001, 1002, 1003, 1004, 1005, 1006, 1007],
        [100; 8],
        false,
    );
    assert_eq!(check(&citations), 7);
    let detail = overruled_detail(&citations);
    assert!(detail.ends_with("$.verified_completed_work[1006].evidence_ids[100], and 1 more."));
    assert!(
        !detail.contains("Evidence "),
        "no description follows: {detail}"
    );
    // Far beyond: every shape still holds, and something is always named.
    for (statement, index) in [(10_000, 10), (100_000, 100_000), (usize::MAX, usize::MAX)] {
        for by_test_run in [false, true] {
            let citations = eight([statement; 8], [index; 8], by_test_run);
            let named = check(&citations);
            assert!((1..8).contains(&named), "{statement} {index}: {named}");
        }
    }
}

#[test]
fn the_shorter_sections_fit_eight_positions_with_indices_below_one_thousand() {
    for position in [
        "$.open_items[999].evidence_ids[999]",
        "$.next_action.evidence_ids[999]",
    ] {
        let citations: Vec<_> = (0..8)
            .map(|_| citation(position.to_owned(), false))
            .collect();
        assert_eq!(check(&citations), 8, "{position}");
    }
}
