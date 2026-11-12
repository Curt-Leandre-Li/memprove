//! Tests of the query Pack (U3 G4, Owner O8, O9; ADR 0016, "Query
//! selection"): a Capsule of 64 claims, superseded and withdrawn ones
//! among them, queried with matches the test chooses (the ranking itself
//! is the FTS5 index's, tested where it lives, in
//! `memprove-store-sqlite`). Every query Pack is read back by the strict
//! parser of `pack_parse.rs` and checked against the Capsule JSON.

use std::collections::{BTreeMap, BTreeSet};

use super::parse::{check_query, expected_query_order, parse};
use super::{
    FROZEN_BUDGETS, H, NOT_PASSED, STALE, Spec, UNAVAILABLE, World, X1, X2, actions, body,
    capsule_value, digest_of, fixture_sessions, session_id, spec,
};
use crate::{
    PackCapsule, PackCut, PackError, PackQuery, QueryRefusal, Selected, check_pack_query,
    render_pack, select_pack, select_query_pack,
};

/// U4 S2: the `SQLite` version the tests' queries name.
const SQLITE: &str = "3.49.1";

/// The texts of the 64 claims, by number: Chinese and English, about the
/// Pack's budget, its omission levels, the query index, and filler.
fn text_of(number: u64) -> String {
    match number {
        1 => "The Pack budget counts UTF-16 code units".to_owned(),
        2 => "The budget is counted in UTF-16 units, 9000 by default".to_owned(),
        3 => "Level 5 omission stops listing claims one by one".to_owned(),
        4 => "There are five omission levels; the fifth stops listing claims".to_owned(),
        // #10 is superseded by #11, which #12 supersedes.
        10 => "Query results are ordered by claim number (old)".to_owned(),
        11 => "Query results are ordered by rank (second version, outdated)".to_owned(),
        12 => "Query results are ordered by bm25 rank, ties by ascending number".to_owned(),
        20 => "The trigram tokenizer was rejected".to_owned(),
        30 => "A long body about the budget ".repeat(12),
        63 => "The old next action: write the tests first".to_owned(),
        64 => "Next action: implement the query Pack".to_owned(),
        number => format!("an unrelated record filler {number}"),
    }
}

/// The 64 claims: #10 and #11 superseded (by #11 and #12), #20 and #40
/// withdrawn, #63 a superseded next action and #64 the active one, facts,
/// open items, and Decisions in between, two of them verified.
fn query_claims() -> Vec<Spec> {
    (1..=64)
        .map(|number| {
            let text = text_of(number);
            let (kind, standing, verdict, reasons, bases): (_, _, _, &[&str], &[&str]) =
                match number {
                    1 | 3 => ("fact", "active", "verified", &[], &[H]),
                    2 | 4 | 12 => ("fact", "active", "stale", &STALE, &[X1]),
                    10 | 11 => ("fact", "superseded", "stale", &STALE, &[X2]),
                    20 => ("open_item", "withdrawn", "partial", &UNAVAILABLE, &[]),
                    30 => ("open_item", "active", "partial", &NOT_PASSED, &[X1]),
                    40 => ("fact", "withdrawn", "stale", &STALE, &[X2]),
                    63 => ("next_action", "superseded", "partial", &UNAVAILABLE, &[]),
                    64 => ("next_action", "active", "partial", &UNAVAILABLE, &[]),
                    number if number % 5 == 0 => ("decision", "active", "partial", &[], &[]),
                    number if number % 2 == 0 => {
                        ("open_item", "active", "partial", &UNAVAILABLE, &[])
                    }
                    _ => ("fact", "active", "stale", &STALE, &[X1]),
                };
            let rationale = (kind == "decision").then(|| format!("rationale {number}"));
            let mut claim = spec(
                number,
                kind,
                standing,
                verdict,
                reasons,
                &text,
                rationale.as_deref(),
                1 + number % 3,
                bases,
            );
            if number == 30 {
                claim.run_status = "FAIL";
            }
            claim
        })
        .collect()
}

fn query_world() -> World {
    World {
        status: "stale",
        reasons: STALE.to_vec(),
        title: "Query Pack fixture".to_owned(),
        task_status: "in_progress",
        sequence: 120,
        head: Some(H.to_owned()),
        chain_valid: true,
        sessions: fixture_sessions(),
        claims: query_claims(),
        reruns: vec![(vec!["cargo".to_owned(), "test".to_owned()], vec![2, 4])],
        reaffirms: vec![],
        changed: vec![
            (
                X1.to_owned(),
                true,
                true,
                vec![
                    "repo://src/pack.rs".to_owned(),
                    "repo://query_selection.rs".to_owned(),
                ],
            ),
            (X2.to_owned(), true, true, vec!["repo://old.rs".to_owned()]),
            (H.to_owned(), true, true, vec![]),
        ],
        gaps: vec![],
        damaged: vec![],
        actions: actions(&["evidence_stale"], 0, &[], &[session_id(2)], &[]),
        resumed: BTreeMap::from([(session_id(1), (30, digest_of("pack 30")))]),
    }
}

fn query(text: &str, matches: &[u64]) -> PackQuery {
    PackQuery::new(text, matches.to_vec(), SQLITE).unwrap()
}

/// The claim lines' numbers of a Pack, in order.
fn claim_lines(pack: &str) -> Vec<u64> {
    pack.lines()
        .filter_map(|line| line.strip_prefix('#'))
        .map(|rest| rest.split(' ').next().unwrap().parse().unwrap())
        .collect()
}

#[test]
fn the_fixture_has_sixty_four_claims_with_superseded_and_withdrawn_ones() {
    let value = capsule_value(&query_world());
    let claims = value["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 64);
    let of = |standing: &str| {
        claims
            .iter()
            .filter(|claim| claim["standing"] == standing)
            .count()
    };
    assert_eq!((of("superseded"), of("withdrawn")), (3, 2));
    assert!(PackCapsule::from_json(&body(&query_world())).is_ok());
}

/// The matches, best first, each followed by what supersedes it, then the
/// active next action; the `Query:` line is the third line; the Pack reads
/// back strictly.
#[test]
fn a_query_pack_lists_the_matches_by_rank_with_their_chains_and_the_next_action() {
    let world = query_world();
    let value = capsule_value(&world);
    let capsule = PackCapsule::from_json(&body(&world)).unwrap();
    let asked = query("query results rank", &[11, 2, 10, 4]);
    let selection = select_query_pack(&capsule, &asked, 30_000).unwrap();
    assert_eq!(selection.level(), 0);
    let pack = render_pack(&selection);
    // #11 then #12 (it supersedes #11), #2, #10 (#11 is listed already),
    // #4, then the active next action #64; the superseded #63 is not.
    assert_eq!(claim_lines(&pack), [11, 12, 2, 10, 4, 64]);
    assert_eq!(selection.claim_order(), [11, 12, 2, 10, 4, 64]);
    assert_eq!(selection.selected(12), Some(Selected::Supersession));
    assert_eq!(selection.selected(10), Some(Selected::Match));
    assert_eq!(selection.selected(64), Some(Selected::NextAction));
    assert_eq!(selection.selected(1), None);
    let lines = pack.lines().collect::<Vec<_>>();
    assert_eq!(
        lines[2],
        "Query: 「query results rank」; 4 matches, by rank: #11, #2, #10, #4"
    );
    assert!(lines[3].starts_with("Task「"), "{pack}");
    // A superseded match shows its standing and what superseded it.
    let line_11 = lines.iter().find(|line| line.starts_with("#11 ")).unwrap();
    assert!(
        line_11.starts_with("#11 fact, superseded by #12: "),
        "{line_11}"
    );
    // No reruns and no Superseded and withdrawn line in a query Pack.
    assert!(!pack.contains("\nRerun: "), "{pack}");
    assert!(!pack.contains("\nSuperseded and withdrawn: "), "{pack}");
    let parsed = check_query(&value, &pack, 30_000, "query results rank", &[11, 2, 10, 4]).unwrap();
    assert_eq!(parsed.query.unwrap().2, [11, 2, 10, 4]);
    assert_eq!(
        expected_query_order(&value, &[11, 2, 10, 4]),
        [11, 12, 2, 10, 4, 64]
    );
    // The same Capsule and query give the same bytes.
    assert_eq!(
        render_pack(&select_query_pack(&capsule, &asked, 30_000).unwrap()),
        pack
    );
}

#[test]
fn a_matched_next_action_keeps_its_rank_and_a_withdrawn_match_shows_its_reason() {
    let world = query_world();
    let value = capsule_value(&world);
    let capsule = PackCapsule::from_json(&body(&world)).unwrap();
    let pack = render_pack(
        &select_query_pack(&capsule, &query("next action", &[64, 63, 20]), 9_000).unwrap(),
    );
    assert_eq!(claim_lines(&pack), [64, 63, 20]);
    let withdrawn = pack.lines().find(|line| line.starts_with("#20 ")).unwrap();
    assert!(
        withdrawn.starts_with("#20 open item, withdrawn: "),
        "{withdrawn}"
    );
    assert!(
        withdrawn.contains("withdrawal reason 「no longer holds」"),
        "{withdrawn}"
    );
    check_query(&value, &pack, 9_000, "next action", &[64, 63, 20]).unwrap();
}

/// No match: the `Query:` line says so, there is no claim, base, fold, or
/// unlisted line (not even the next action), and the Pack is whole.
#[test]
fn no_match_is_a_whole_pack_with_no_claim_line() {
    let world = query_world();
    let value = capsule_value(&world);
    let capsule = PackCapsule::from_json(&body(&world)).unwrap();
    let asked = query("nothing", &[]);
    let selection = select_query_pack(&capsule, &asked, 9_000).unwrap();
    let pack = render_pack(&selection);
    assert!(claim_lines(&pack).is_empty(), "{pack}");
    assert!(
        pack.lines()
            .any(|line| line == "Query: 「nothing」; 0 matches"),
        "{pack}"
    );
    for prefix in [
        "Base: ",
        "Folded: ",
        "Not listed: ",
        "Rerun: ",
        "Superseded and withdrawn: ",
    ] {
        assert!(
            !pack.lines().any(|line| line.starts_with(prefix)),
            "{prefix}"
        );
    }
    for prefix in [
        "Pack: ",
        "Ontology: ",
        "Task「",
        "Integrity: ",
        "Sessions: ",
        "Session ",
        "Can: ",
        "Omitted: ",
    ] {
        assert!(
            pack.lines().any(|line| line.starts_with(prefix)),
            "{prefix}"
        );
    }
    check_query(&value, &pack, 9_000, "nothing", &[]).unwrap();
    assert_eq!(selection.query().unwrap().sqlite_version(), SQLITE);
    assert!(selection.claim_order().is_empty());
    assert!((1..=64).all(|number| selection.selected(number).is_none()));
}

/// Over budget: the five levels run on the selected claims only, the
/// superseded and withdrawn ones first; every claim keeps its rank and why
/// it was selected, and the claim lines are the selection's order less
/// what the levels folded or unlisted.
#[test]
fn over_budget_the_levels_cut_the_selection() {
    let world = query_world();
    let value = capsule_value(&world);
    let capsule = PackCapsule::from_json(&body(&world)).unwrap();
    // Every claim matches, in a rank order that is not the number order.
    let mut matches = (1..=64).rev().collect::<Vec<u64>>();
    matches.swap(0, 33);
    let asked = query("every", &matches);
    let mut levels = BTreeSet::new();
    let whole = render_pack(&select_query_pack(&capsule, &asked, 1_000_000).unwrap())
        .encode_utf16()
        .count();
    let mut budgets = FROZEN_BUDGETS.to_vec();
    budgets.extend((1_000..=whole).rev().step_by(23));
    // Level 1 saves a few code units only: every budget just below whole.
    budgets.extend(whole - 80..whole);
    budgets.sort_unstable();
    budgets.dedup();
    for budget in budgets {
        let selection = match select_query_pack(&capsule, &asked, budget) {
            Ok(selection) => selection,
            Err(PackError::OverBudget { needed, .. }) => {
                levels.insert(None);
                let retry = select_query_pack(&capsule, &asked, needed).unwrap();
                assert_eq!(retry.level(), 5);
                continue;
            }
            Err(error) => panic!("{error}"),
        };
        levels.insert(Some(selection.level()));
        let pack = render_pack(&selection);
        let parsed = check_query(&value, &pack, budget, "every", &matches)
            .unwrap_or_else(|error| panic!("budget {budget}: {error}\n{pack}"));
        // Every claim is listed once as a match (#64, which #63's chain
        // brings in early, is a match too); the claims with a line are the
        // selection's order less the folded and unlisted ones.
        let mut listed_once = selection.claim_order().to_vec();
        listed_once.sort_unstable();
        assert_eq!(listed_once, (1..=64).collect::<Vec<u64>>());
        for number in &matches {
            assert_eq!(
                selection.selected(*number),
                Some(Selected::Match),
                "#{number}"
            );
        }
        let listed = selection
            .claim_order()
            .iter()
            .copied()
            .filter(|number| {
                !matches!(
                    selection.cut(*number),
                    Some(PackCut::Unlisted | PackCut::Folded)
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(claim_lines(&pack), listed, "budget {budget}");
        // Level 5 unlists the superseded and withdrawn claims first.
        if selection.level() == 5 {
            assert!(!parsed.unlisted_retired.is_empty(), "budget {budget}");
            let active_unlisted = (1..=64)
                .filter(|number| ![10, 11, 20, 40, 63].contains(number))
                .any(|number| selection.cut(number) == Some(PackCut::Unlisted));
            if active_unlisted {
                for retired in [10, 11, 20, 40, 63] {
                    assert_eq!(selection.cut(retired), Some(PackCut::Unlisted));
                }
            }
        }
    }
    let every = [None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)];
    assert_eq!(levels, BTreeSet::from(every));
}

/// Level 2 shortens the superseded and withdrawn claims' bodies before
/// the active ones', which `resume.v1` never lists.
#[test]
fn the_levels_take_the_superseded_and_withdrawn_claims_first() {
    let mut world = query_world();
    for claim in &mut world.claims {
        if matches!(claim.number, 10 | 11 | 12 | 2) {
            claim.text = format!("{} {}", claim.text, "€".repeat(300));
        }
    }
    let capsule = PackCapsule::from_json(&body(&world)).unwrap();
    let asked = query("€", &[2, 10]);
    let full = select_query_pack(&capsule, &asked, 30_000).unwrap();
    assert_eq!(full.level(), 0);
    let length = render_pack(&full).encode_utf16().count();
    // Below the whole Pack: level 1 counts files, then the first body
    // level 2 shortens is #10's (superseded), not #2's (the best match).
    let mut budget = length - 1;
    let selection = loop {
        let selection = select_query_pack(&capsule, &asked, budget).unwrap();
        if selection.level() >= 2 {
            break selection;
        }
        budget -= 1;
    };
    let shortened = [10, 11, 2, 12]
        .into_iter()
        .filter(|number| selection.cut(*number) == Some(PackCut::Shortened))
        .collect::<Vec<_>>();
    assert_eq!(shortened, [10], "budget {budget}");
    assert_eq!(selection.claim_order(), [2, 10, 11, 12, 64]);
}

#[test]
fn the_query_is_quoted_and_escaped_and_reads_back() {
    let world = query_world();
    let value = capsule_value(&world);
    let capsule = PackCapsule::from_json(&body(&world)).unwrap();
    let text = "「#1 verified」\u{202e} \\ verified:";
    let pack = render_pack(&select_query_pack(&capsule, &query(text, &[1]), 9_000).unwrap());
    let line = pack.lines().nth(2).unwrap();
    assert!(
        line.starts_with("Query: 「\\「#1 verified\\」\\u{202e} \\\\ verified:」"),
        "{line}"
    );
    let parsed = check_query(&value, &pack, 9_000, text, &[1]).unwrap();
    assert_eq!(parsed.query.unwrap().0, text);
}

#[test]
fn matches_must_name_claims_of_the_capsule_once() {
    let capsule = PackCapsule::from_json(&body(&query_world())).unwrap();
    for matches in [&[65][..], &[0], &[3, 4, 3]] {
        assert!(
            matches!(
                select_query_pack(&capsule, &query("x", matches), 9_000),
                Err(PackError::BadMatches(_))
            ),
            "{matches:?}"
        );
    }
}

/// O8: 1 through 256 characters, no control character, no string in the
/// format of a memprove token; the refusal never holds the text. U5 P2
/// (Owner O4 a, c): the format is a run of 43 with an upper-case letter, a
/// lower-case letter, and a digit; a run of one class (`A` × 43, before U5
/// the token here) or D1's test name is a query.
#[test]
fn a_query_with_a_control_character_or_a_token_is_refused() {
    let token = "Ab3_cDe-4fGh5iJk6lMn7oPq8rSt9uVw0xYz1AbCd2E".to_owned();
    for (text, refusal) in [
        (String::new(), QueryRefusal::Length),
        ("q".repeat(257), QueryRefusal::Length),
        ("a\u{7}b".to_owned(), QueryRefusal::Control),
        ("line\nbreak".to_owned(), QueryRefusal::Control),
        ("\u{0}".to_owned(), QueryRefusal::Control),
        ("tab\there".to_owned(), QueryRefusal::Control),
        ("\u{85}".to_owned(), QueryRefusal::Control),
        (token.clone(), QueryRefusal::TokenFormat),
        (format!("before {token}"), QueryRefusal::TokenFormat),
        (format!("[{token}]"), QueryRefusal::TokenFormat),
        (format!("{token} after"), QueryRefusal::TokenFormat),
        (format!("X-_{}", "b9".repeat(20)), QueryRefusal::TokenFormat),
    ] {
        assert_eq!(check_pack_query(&text), Err(refusal), "{text:?}");
        assert_eq!(PackQuery::new(&text, vec![], SQLITE), Err(refusal));
        let said = refusal.to_string();
        if !text.is_empty() {
            assert!(!said.contains(&text), "{said}");
        }
        assert!(!said.contains(&token), "{said}");
    }
    for text in [
        "q".repeat(256),
        "a".repeat(42),
        "a".repeat(44),
        format!("{} {}", "a".repeat(42), "b".repeat(44)),
        "01a00000-0000-7000-8000-0000000000aa".to_owned(),
        "sha256:3333333333333333333333333333333333333333333333333333333333333333".to_owned(),
        " ".to_owned(),
        "\u{202e}".to_owned(),
        "A".repeat(43),
        format!("x-_{}", "b".repeat(40)),
        "test_calculate_metrics_rejects_empty_series".to_owned(),
        format!(
            "why does {} fail",
            "test_calculate_metrics_rejects_empty_series"
        ),
    ] {
        assert_eq!(check_pack_query(&text), Ok(()), "{text:?}");
    }
}

/// A Pack made without a query has no `Query:` line and the parser refuses
/// one where it does not belong.
#[test]
fn the_query_line_is_only_the_third_line_of_a_query_pack() {
    let capsule = PackCapsule::from_json(&body(&query_world())).unwrap();
    let plain = render_pack(&select_pack(&capsule, 30_000).unwrap());
    assert!(!plain.contains("Query: "));
    let asked = query("budget", &[1, 30]);
    let pack = render_pack(&select_query_pack(&capsule, &asked, 30_000).unwrap());
    assert!(parse(&pack).is_ok());
    // Move the Query line one down: out of place.
    let mut lines = pack.lines().map(str::to_owned).collect::<Vec<_>>();
    lines.swap(2, 3);
    let moved = lines[..lines.len() - 1].join("\n") + "\n";
    let digest = crate::Sha256Digest::of_bytes(moved.as_bytes());
    let moved = format!("{moved}Digest: sha256:{digest}\n");
    assert!(
        parse(&moved).unwrap_err().contains("out of place"),
        "{moved}"
    );
}

/// Random matches over random budgets: every query Pack reads back.
#[test]
fn property_random_matches_read_back() {
    let world = query_world();
    let value = capsule_value(&world);
    let capsule = PackCapsule::from_json(&body(&world)).unwrap();
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..60 {
        let count = usize::try_from(next() % 20).unwrap();
        let mut matches = Vec::new();
        while matches.len() < count {
            let number = 1 + next() % 64;
            if !matches.contains(&number) {
                matches.push(number);
            }
        }
        let asked = query("q", &matches);
        for budget in [1_200, 2_000, 3_000, 5_000, 9_000] {
            match select_query_pack(&capsule, &asked, budget) {
                Ok(selection) => {
                    let pack = render_pack(&selection);
                    check_query(&value, &pack, budget, "q", &matches).unwrap_or_else(|error| {
                        panic!("round {round}, budget {budget}: {error}\n{pack}")
                    });
                }
                Err(PackError::OverBudget { .. }) => {}
                Err(error) => panic!("{error}"),
            }
        }
    }
}
