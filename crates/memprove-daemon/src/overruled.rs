//! P3-L3 (Owner, T16 S-1): the `400 invalid_request` detail of a
//! `createCheckpoint` whose `verified_fact` statements cite passed runs that
//! a later run overrules (P3-D3, P3-L L1, L2).
//!
//! The detail first names, in one compact sentence, the position in the
//! request of every overruled citation, numbered `(1)`, `(2)`, ...; then,
//! in the room left under [`PROBLEM_DETAIL_MAX_CHARS`], it describes them
//! one by one. A description is never cut: when the next one does not fit,
//! the detail ends with how many are left undescribed. Up to eight
//! positions all fit while every statement index and Evidence index in
//! them is below 1000; past that, fewer may (P4-A, T17 N-1: eight positions
//! of `verified_completed_work` fit while the decimal digits of their
//! indices add up to at most 53, see `overruled_bound_tests`). When the
//! positions do not all fit, the sentence ends with how many more there
//! are, and nothing follows it.
//!
//! A citation overruled by the latest test run of its argv (P3-L L2) names
//! that test run and says that only a passed test run restores it.

use std::fmt::Write as _;

use memprove_protocol::phase1::PROBLEM_DETAIL_MAX_CHARS;
use memprove_store_sqlite::OverruledCitation;

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// The ending when `left` citations are not described.
fn undescribed(left: usize) -> String {
    format!("{left} more not described.")
}

/// The opening of the compact sentence.
fn opening(count: usize) -> String {
    if count == 1 {
        "1 verified_fact citation names an overruled passed run: ".to_owned()
    } else {
        format!("{count} verified_fact citations name overruled passed runs: ")
    }
}

/// The compact sentence of every position, and whether it names them all.
/// It leaves room for the ending of [`undescribed`] after it.
fn positions(citations: &[OverruledCitation]) -> (String, bool) {
    let count = citations.len();
    let budget = PROBLEM_DETAIL_MAX_CHARS - 1 - chars(&undescribed(count));
    let mut sentence = opening(count);
    for (index, citation) in citations.iter().enumerate() {
        let separator = if index == 0 { "" } else { ", " };
        let entry = format!("{separator}({}) {}", index + 1, citation.position);
        let left = count - index - 1;
        let tail = if left == 0 {
            ".".to_owned()
        } else {
            format!(", and {left} more.")
        };
        // The entry must fit with whatever ends the sentence after it.
        if chars(&sentence) + chars(&entry) + chars(&tail) > budget {
            let and = if index == 0 { "" } else { ", " };
            write!(sentence, "{and}and {} more.", count - index)
                .expect("writing to a String cannot fail");
            return (sentence, false);
        }
        sentence.push_str(&entry);
    }
    sentence.push('.');
    (sentence, true)
}

/// The description of the `number`-th citation.
fn describe(number: usize, citation: &OverruledCitation) -> String {
    let OverruledCitation {
        evidence_id, later, ..
    } = citation;
    if citation.by_test_run {
        format!(
            "({number}) test run Evidence {evidence_id} passed; the latest test run {later} did \
             not pass: it needs a later passed test run."
        )
    } else {
        format!(
            "({number}) Evidence {evidence_id} passed; the later run {later} of its argv in this \
             state did not pass: rerun until it passes."
        )
    }
}

/// The detail for `citations`, in request order; at most
/// [`PROBLEM_DETAIL_MAX_CHARS`] characters.
pub(crate) fn overruled_detail(citations: &[OverruledCitation]) -> String {
    let count = citations.len();
    let (mut detail, complete) = positions(citations);
    if !complete {
        return detail;
    }
    for (index, citation) in citations.iter().enumerate() {
        let item = describe(index + 1, citation);
        let left = count - index - 1;
        let reserve = if left == 0 {
            0
        } else {
            1 + chars(&undescribed(left))
        };
        detail.push(' ');
        if chars(&detail) + chars(&item) + reserve > PROBLEM_DETAIL_MAX_CHARS {
            detail.push_str(&undescribed(count - index));
            break;
        }
        detail.push_str(&item);
    }
    detail
}

#[cfg(test)]
mod tests {
    use memprove_domain::EvidenceId;

    use super::*;

    fn citation(position: &str, by_test_run: bool) -> OverruledCitation {
        OverruledCitation {
            position: position.to_owned(),
            evidence_id: EvidenceId::new(),
            later: EvidenceId::new(),
            by_test_run,
        }
    }

    fn many(count: usize, by_test_run: bool) -> Vec<OverruledCitation> {
        (0..count)
            .map(|index| {
                citation(
                    &format!(
                        "$.verified_completed_work[{}].evidence_ids[{}]",
                        index + 10,
                        index + 90
                    ),
                    by_test_run,
                )
            })
            .collect()
    }

    /// Every position, numbered in order, in the first sentence; each
    /// description whole or absent; the ending says how many are left.
    #[test]
    fn every_position_is_named_and_no_description_is_cut() {
        for count in 1..=40 {
            for by_test_run in [false, true] {
                let citations = many(count, by_test_run);
                let detail = overruled_detail(&citations);
                assert!(chars(&detail) <= PROBLEM_DETAIL_MAX_CHARS, "{detail}");
                let (first, _) = positions(&citations);
                assert!(detail.starts_with(&first));
                let named = citations
                    .iter()
                    .filter(|citation| first.contains(&citation.position))
                    .count();
                if count <= 8 {
                    assert_eq!(named, count, "{detail}");
                }
                if named < count {
                    assert!(
                        first.ends_with(&format!("and {} more.", count - named)),
                        "{detail}"
                    );
                    assert_eq!(detail, first);
                    continue;
                }
                let described = (1..=count)
                    .take_while(|number| {
                        detail.contains(&describe(*number, &citations[number - 1]))
                    })
                    .count();
                if described < count {
                    assert!(
                        detail.ends_with(&format!(" {}", undescribed(count - described))),
                        "{detail}"
                    );
                } else {
                    assert!(detail.ends_with(&describe(count, &citations[count - 1])));
                }
                // Nothing but the described items and the ending follows.
                let mut rebuilt = first.clone();
                for number in 1..=described {
                    rebuilt.push(' ');
                    rebuilt.push_str(&describe(number, &citations[number - 1]));
                }
                if described < count {
                    rebuilt.push(' ');
                    rebuilt.push_str(&undescribed(count - described));
                }
                assert_eq!(detail, rebuilt);
            }
        }
    }

    /// One and two citations are described in full; a test run names the
    /// test run and asks for a passed one.
    #[test]
    fn short_lists_are_described_in_full() {
        let one = many(1, true);
        let detail = overruled_detail(&one);
        assert_eq!(
            detail,
            format!(
                "1 verified_fact citation names an overruled passed run: (1) {}. (1) test run \
                 Evidence {} passed; the latest test run {} did not pass: it needs a later \
                 passed test run.",
                one[0].position, one[0].evidence_id, one[0].later
            )
        );
        for by_test_run in [false, true] {
            let two = many(2, by_test_run);
            let detail = overruled_detail(&two);
            assert!(detail.ends_with(&describe(2, &two[1])), "{detail}");
            assert!(detail.contains(&describe(1, &two[0])), "{detail}");
        }
    }
}
