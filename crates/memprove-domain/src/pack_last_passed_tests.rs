//! U5 P1 (Owner O3, D1 F10; ADR 0014, "Amendment (U5, Z-P)"): the
//! `Last passed:` line. A Pack with no `Rerun:` line names the passed test run the
//! Capsule shows, with its argv and the first 12 digits of its commit, right
//! after the Session lines, so that the agent who takes over runs the tests
//! the way they last passed (D1's client ran `python3` where the run had
//! been `/usr/local/bin/python3.12`). Every Pack here is read back by the
//! strict parser of `pack_parse.rs`, which derives the line from the Capsule
//! JSON by its own code.

use serde_json::{Value, json};

use super::parse::{check, check_query, expected_last_passed, parse};
use super::{
    FROZEN_BUDGETS, H, World, actions, capsule_value, fixture, fixture_claims, session_id,
    tampered, units,
};
use crate::{
    PackCapsule, PackError, PackQuery, Sha256Digest, render_pack, select_pack, select_query_pack,
};

const E1: &str = "01a00000-0000-7000-8000-0000000001e1";
const E2: &str = "01a00000-0000-7000-8000-0000000001e2";
const E3: &str = "01a00000-0000-7000-8000-0000000001e3";
const PYTEST: [&str; 4] = ["/usr/local/bin/python3.12", "-m", "pytest", "-q"];
const COMPILEALL: [&str; 5] = ["/usr/local/bin/python3.12", "-m", "compileall", "-q", "."];
const LINE: &str = "Last passed: 「/usr/local/bin/python3.12 -m pytest -q」 (333333333333; --test --test-format pytest)";
const BUDGET: usize = crate::PACK_DEFAULT_BUDGET;

/// The fixture's Sessions and claims, verified: the stale fact and the open
/// item go, so no claim needs a rerun; #2 is the verified fact, bound to
/// the HEAD `H`.
fn verified() -> World {
    let mut world = fixture();
    world.status = "verified";
    world.reasons = vec![];
    world.claims = fixture_claims()
        .into_iter()
        .filter(|spec| !matches!(spec.number, 3 | 4))
        .collect();
    world.reruns = vec![];
    world.changed = vec![(H.to_owned(), true, true, vec![])];
    let open = vec![session_id(2)];
    world.actions = actions(&[], 1, &[], &open, &[]);
    world
}

/// One Evidence object as the Capsule lists it on a claim.
fn evidence(id: &str, kind: &str, status: &str, argv: &[&str], base: &str) -> Value {
    json!({
        "evidence_id": id, "kind": kind, "canonical_digest": null, "run_status": status,
        "argv": argv, "snapshot_id": null, "base_commit": base, "verdict": "verified",
        "reasons": [],
    })
}

/// The Capsule of `world` with claim #2 citing `cited`, the latest run
/// `latest` (`cite_latest_run`'s `args`: ID, kind, status, argv), and
/// `complete_task` citing `completion`.
fn with_runs(
    world: &World,
    cited: &[Value],
    latest: (&str, &str, &str, &[&str]),
    completion: &[&str],
) -> Value {
    let mut value = capsule_value(world);
    for claim in value["claims"].as_array_mut().unwrap() {
        if claim["number"] == 2 {
            claim["evidence"] = json!(cited);
        }
    }
    let (id, kind, status, argv) = latest;
    for action in value["actions"].as_array_mut().unwrap() {
        if action["action"] == "cite_latest_run" {
            action["args"]["evidence_id"] = json!(id);
            action["args"]["kind"] = json!(kind);
            action["args"]["run_status"] = json!(status);
            action["args"]["argv"] = json!(argv);
        }
        if action["action"] == "complete_task" {
            action["args"]["completion_evidence_ids"] = json!(completion);
        }
    }
    value
}

/// The Pack of `value` at the default budget, checked by the strict reader.
fn pack_of(value: &Value) -> String {
    let capsule = PackCapsule::from_json(&value.to_string()).unwrap();
    let text = render_pack(&select_pack(&capsule, BUDGET).unwrap());
    check(value, &text, BUDGET).unwrap_or_else(|error| panic!("{error}\n{text}"));
    text
}

fn last_passed_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with("Last passed: "))
        .collect()
}

/// D1's case: the fact cites the pytest run, and the latest run is a
/// `compileall` command run. The line names the pytest run's argv and
/// commit, and stands right after the last `Session ` line, before the action
/// lines and the claims.
#[test]
fn a_verified_task_names_its_last_passed_test_run_after_the_session_lines() {
    let value = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    assert_eq!(
        expected_last_passed(&value),
        Some((
            PYTEST.join(" "),
            "333333333333".to_owned(),
            "--test --test-format pytest".to_owned()
        ))
    );
    let text = pack_of(&value);
    println!("{text}");
    assert_eq!(last_passed_lines(&text), [LINE]);
    let lines = text.lines().collect::<Vec<_>>();
    let at = lines.iter().position(|line| *line == LINE).unwrap();
    assert!(lines[at - 1].starts_with("Session "), "{}", lines[at - 1]);
    assert!(lines[at + 1].starts_with("Can: ") || lines[at + 1].starts_with("Cannot: "));
    assert!(
        lines[..at]
            .iter()
            .all(|line| !line.starts_with('#') && !line.starts_with("Base: "))
    );

    // The latest run is itself a passed test run no claim cites; it is
    // `verified` now (`complete_task` cites it), so it is bound to HEAD.
    let latest = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E3, "test_result", "PASS", &["cargo", "test"]),
        &[E1, E3],
    );
    assert_eq!(
        last_passed_lines(&pack_of(&latest)),
        ["Last passed: 「cargo test」 (333333333333; --test --test-format cargo)"]
    );

    // A latest passed test run the Capsule gives no commit for (no claim
    // cites it, it is not `verified` now) gives way to the cited one.
    let unbound = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E3, "test_result", "PASS", &["cargo", "test"]),
        &[E1],
    );
    assert_eq!(last_passed_lines(&pack_of(&unbound)), [LINE]);

    // Of the cited passed test runs, the last one `complete_task` cites
    // (ledger order) is named, whatever order the claim cites them in.
    let two = with_runs(
        &verified(),
        &[
            evidence(E1, "test_result", "PASS", &PYTEST, H),
            evidence(E3, "test_result", "PASS", &["cargo", "test"], H),
        ],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E3, E1, E2],
    );
    assert_eq!(last_passed_lines(&pack_of(&two)), [LINE]);
}

/// No line when the Pack has a `Rerun:` line (it gives the argv), when no
/// passed test run shows, or when the Capsule is `tampered`.
#[test]
fn no_last_passed_line_with_a_rerun_without_a_passed_test_run_or_when_tampered() {
    let mut stale = verified();
    stale.reruns = vec![(
        PYTEST.iter().map(|word| (*word).to_owned()).collect(),
        vec![2],
    )];
    let value = with_runs(
        &stale,
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    let text = pack_of(&value);
    assert!(last_passed_lines(&text).is_empty(), "{text}");
    assert!(text.contains("Rerun: 「/usr/local/bin/python3.12 -m pytest -q」"));

    // The only test run failed; the passed run is a command run.
    let failed = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "FAIL", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E2],
    );
    assert!(last_passed_lines(&pack_of(&failed)).is_empty());
    // A command run only.
    let none = with_runs(
        &verified(),
        &[],
        (E1, "command_result", "PASS", &COMPILEALL),
        &[E1],
    );
    assert!(last_passed_lines(&pack_of(&none)).is_empty());

    // A tampered Capsule names no run as passed, though its latest run is
    // a passed test run.
    let value = capsule_value(&tampered());
    assert_eq!(value["actions"][2]["args"]["kind"], "test_result");
    assert_eq!(value["actions"][2]["args"]["run_status"], "PASS");
    let text = pack_of(&value);
    assert!(last_passed_lines(&text).is_empty(), "{text}");
}

/// A query Pack has the same line, in the same place, by the same rule.
#[test]
fn a_query_pack_has_the_same_last_passed_line() {
    let value = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    let capsule = PackCapsule::from_json(&value.to_string()).unwrap();
    for matches in [vec![2], vec![]] {
        let query = PackQuery::new("pytest", matches.clone(), "3.49.1").unwrap();
        let text = render_pack(&select_query_pack(&capsule, &query, BUDGET).unwrap());
        check_query(&value, &text, BUDGET, "pytest", &matches)
            .unwrap_or_else(|error| panic!("{error}\n{text}"));
        assert_eq!(last_passed_lines(&text), [LINE], "{text}");
        let lines = text.lines().collect::<Vec<_>>();
        let at = lines.iter().position(|line| *line == LINE).unwrap();
        assert!(lines[at - 1].starts_with("Session "), "{text}");
    }
    let mut stale = verified();
    stale.reruns = vec![(vec!["cargo".to_owned(), "test".to_owned()], vec![2])];
    let value = with_runs(
        &stale,
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    let capsule = PackCapsule::from_json(&value.to_string()).unwrap();
    let query = PackQuery::new("pytest", vec![2], "3.49.1").unwrap();
    let text = render_pack(&select_query_pack(&capsule, &query, BUDGET).unwrap());
    check_query(&value, &text, BUDGET, "pytest", &[2]).unwrap();
    assert!(last_passed_lines(&text).is_empty(), "{text}");
}

/// The line is never omitted: at every budget a Pack fits, it has the line
/// whole; the least budget (level 5) counts it, one unit less is
/// `OverBudget` naming that budget, and the line makes the least budget
/// longer by exactly its own length, a long argv included.
#[test]
fn the_last_passed_line_is_never_omitted_and_counts_in_the_least_budget() {
    let long = "x".repeat(300);
    let argv = ["sh", long.as_str()];
    let line = format!("Last passed: 「sh {long}」 (333333333333; --test --test-cases <N>)");
    let with = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &argv, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    // The same Capsule but for the kind of that run: a command run, so no
    // line; nothing else in the Pack differs.
    let without = with_runs(
        &verified(),
        &[evidence(E1, "command_result", "PASS", &argv, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    let least = |value: &Value| {
        let capsule = PackCapsule::from_json(&value.to_string()).unwrap();
        let Err(PackError::OverBudget { needed, .. }) = select_pack(&capsule, 1) else {
            panic!("one unit fits a Pack");
        };
        needed
    };
    let (needed, needed_without) = (least(&with), least(&without));
    assert_eq!(needed - needed_without, units(&line) + 1);
    let capsule = PackCapsule::from_json(&with.to_string()).unwrap();
    let fits = render_pack(&select_pack(&capsule, needed).unwrap());
    check(&with, &fits, needed).unwrap();
    assert_eq!(last_passed_lines(&fits), [line.as_str()]);
    // One less: either nothing fits, with the same `needed`, or (U17: a
    // cut can lengthen a Pack, since the English fold line is longer than
    // a claim line with its body omitted, so a lower level may fit at one
    // less) a Pack that still has the line.
    match select_pack(&capsule, needed - 1) {
        Err(error) => assert_eq!(
            error,
            PackError::OverBudget {
                budget: needed - 1,
                needed
            }
        ),
        Ok(selection) => {
            assert_eq!(last_passed_lines(&render_pack(&selection)), [line.as_str()]);
        }
    }
    for budget in FROZEN_BUDGETS {
        if let Ok(selection) = select_pack(&capsule, budget) {
            let text = render_pack(&selection);
            check(&with, &text, budget).unwrap();
            assert_eq!(last_passed_lines(&text), [line.as_str()], "budget {budget}");
        }
    }
}

/// The strict reader takes the line only in its place, once, and only as
/// the Capsule shows that run: a Pack without it, with it twice, with it
/// elsewhere, or with another commit is refused.
#[test]
fn the_strict_reader_holds_the_line_to_its_place_and_to_the_capsule() {
    let value = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    let text = pack_of(&value);
    let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let at = lines.iter().position(|line| line == LINE).unwrap();
    let rebuilt = |lines: &[String]| {
        let mut body = String::new();
        for line in &lines[..lines.len() - 1] {
            body.push_str(line);
            body.push('\n');
        }
        format!(
            "{body}Digest: sha256:{}\n",
            Sha256Digest::of_bytes(body.as_bytes())
        )
    };
    let budget_of = |lines: &[String]| {
        let text = rebuilt(lines);
        (text.encode_utf16().count(), text)
    };
    let mut without = lines.clone();
    without.remove(at);
    let (_, without) = budget_of(&without);
    let error = check(&value, &without, BUDGET).unwrap_err();
    assert!(error.contains("last-passed line"), "{error}");
    let mut twice = lines.clone();
    twice.insert(at, LINE.to_owned());
    assert!(parse(&rebuilt(&twice)).is_err());
    let mut early = lines.clone();
    early.remove(at);
    early.insert(3, LINE.to_owned());
    assert!(parse(&rebuilt(&early)).is_err());
    let mut late = lines.clone();
    late.remove(at);
    late.insert(at + 1, LINE.to_owned());
    assert!(parse(&rebuilt(&late)).is_err());
    let mut other_commit = lines;
    other_commit[at] = LINE.replace("333333333333", "111111111111");
    let error = check(&value, &rebuilt(&other_commit), BUDGET).unwrap_err();
    assert!(error.contains("last-passed line"), "{error}");
    assert!(super::parse_lines(&format!("{LINE}\n"), true).is_err());
}

/// U8 K5 (E1 revision 11; U7 V-N12): the line ends with the `memprove run`
/// arguments that rerun the run as a test run, after the commit and a `;`.
/// The runner is named by the recorded argv, the first word that names one
/// deciding: `cargo` and `test` give `--test-format cargo`, `pytest` (a
/// path's last component, or after `-m`) gives `--test-format pytest`, and
/// an argv that names neither gives `--test --test-cases <N>`, the count
/// left to the caller. The strict reader accepts only these three.
#[test]
fn the_last_passed_line_ends_with_the_arguments_that_rerun_it_as_a_test_run() {
    let cases: [(&[&str], &str); 9] = [
        (&PYTEST, "--test --test-format pytest"),
        (
            &["cargo", "test", "--offline"],
            "--test --test-format cargo",
        ),
        (
            &[
                "env",
                "A=1",
                "/opt/bin/cargo",
                "+nightly",
                "test",
                "-p",
                "x",
            ],
            "--test --test-format cargo",
        ),
        (&["/usr/bin/pytest", "-q"], "--test --test-format pytest"),
        (&["py.test", "tests"], "--test --test-format pytest"),
        // The first runner named decides, whatever follows.
        (
            &["cargo", "test", "-p", "pytest"],
            "--test --test-format cargo",
        ),
        (
            &["pytest", "-k", "cargo test"],
            "--test --test-format pytest",
        ),
        // No runner named: the case count is the caller's.
        (&["cargo", "build"], "--test --test-cases <N>"),
        (&["sh", "tests/check.sh"], "--test --test-cases <N>"),
    ];
    for (argv, rerun) in cases {
        let value = with_runs(
            &verified(),
            &[evidence(E1, "test_result", "PASS", argv, H)],
            (E2, "command_result", "PASS", &COMPILEALL),
            &[E1, E2],
        );
        let line = format!(
            "Last passed: 「{}」 (333333333333; {rerun})",
            argv.join(" ")
        );
        let text = pack_of(&value);
        assert_eq!(last_passed_lines(&text), [line.as_str()], "{argv:?}");
        let owned: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
        assert_eq!(crate::rerun_arguments(&owned), rerun);
    }
}

/// The strict reader refuses a line whose rerun arguments are not one of the
/// three, or are missing, or whose commit stands alone as before.
#[test]
fn the_strict_reader_holds_the_rerun_arguments_to_the_three_spellings() {
    let value = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    let text = pack_of(&value);
    let rebuild = |line: &str| {
        let mut body = String::new();
        for old in text.lines().filter(|old| !old.starts_with("Digest: ")) {
            body.push_str(if old == LINE { line } else { old });
            body.push('\n');
        }
        format!(
            "{body}Digest: sha256:{}\n",
            Sha256Digest::of_bytes(body.as_bytes())
        )
    };
    let refused = |line: &str| check(&value, &rebuild(line), BUDGET).is_err();
    // The wrong arguments for this run, and spellings that are none.
    assert!(refused(&LINE.replace("pytest)", "cargo)")));
    assert!(refused(&LINE.replace("; --test --test-format pytest", "")));
    assert!(refused(
        &LINE.replace("--test --test-format pytest", "--test")
    ));
    assert!(refused(&LINE.replace("pytest)", "pytest )")));
    assert!(refused(&LINE.replace("; --test", ";--test")));
    // Untouched, it is read.
    assert!(!refused(LINE));
}

/// U8 F1 (SF-1): `last_passed_test_argv` is the argv of the Task's latest
/// run when that is a passed test run, whether or not the Pack places it (no
/// statement cites it, so the Pack has no `Last passed:` line), else the run
/// the line names.
#[test]
fn the_last_passed_test_run_is_read_whether_or_not_the_pack_places_it() {
    let argv_of = |value: &Value| {
        PackCapsule::from_json(&value.to_string())
            .unwrap()
            .last_passed_test_argv()
            .map(<[String]>::to_vec)
    };
    let strings = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };

    // The latest run is a passed test run that no statement cites and
    // `complete_task` does not cite either: no line, the argv all the same.
    let uncited = with_runs(&verified(), &[], (E1, "test_result", "PASS", &PYTEST), &[]);
    assert!(last_passed_lines(&pack_of(&uncited)).is_empty());
    assert_eq!(argv_of(&uncited), Some(strings(&PYTEST)));

    // The latest run is a command run: the run the line names.
    let command = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    assert_eq!(argv_of(&command), Some(strings(&PYTEST)));

    // A test run that failed, and a Task with no run, give nothing.
    let failed = with_runs(&verified(), &[], (E1, "test_result", "FAIL", &PYTEST), &[]);
    assert_eq!(argv_of(&failed), None);
    let mut none = with_runs(&verified(), &[], ("", "", "", &[]), &[]);
    for action in none["actions"].as_array_mut().unwrap() {
        if action["action"] == "cite_latest_run" {
            for member in ["evidence_id", "kind", "run_status", "argv"] {
                action["args"][member] = Value::Null;
            }
        }
    }
    assert_eq!(argv_of(&none), None);
}

/// A cited Evidence object as `evidence` lists it, with the `test_format`
/// it recorded.
fn evidence_with_format(id: &str, argv: &[&str], format: &str) -> Value {
    let mut item = evidence(id, "test_result", "PASS", argv, H);
    item["test_format"] = json!(format);
    item
}

/// U9 K2 (Owner O6): the format a test run recorded decides the rerun
/// arguments of the line, whatever its argv looks like; a run that recorded
/// none (every run before U9) is read from its argv as before, and a latest
/// run that is the one named says its format on `cite_latest_run` too.
#[test]
fn a_recorded_test_format_decides_the_rerun_arguments_and_an_old_run_keeps_the_guess() {
    let check = ["sh", "tests/check.sh"];
    for (format, rerun) in [
        ("cargo", "--test --test-format cargo"),
        ("pytest", "--test --test-format pytest"),
        ("cases", "--test --test-cases <N>"),
    ] {
        let line = format!("Last passed: 「sh tests/check.sh」 (333333333333; {rerun})");
        let cited = with_runs(
            &verified(),
            &[evidence_with_format(E1, &check, format)],
            (E2, "command_result", "PASS", &COMPILEALL),
            &[E1, E2],
        );
        assert_eq!(
            last_passed_lines(&pack_of(&cited)),
            [line.as_str()],
            "{format}"
        );
        let capsule = PackCapsule::from_json(&cited.to_string()).unwrap();
        assert_eq!(
            capsule.last_passed_test().map(|(_, format)| format),
            Some(crate::TestFormat::parse(format)),
            "{format}"
        );

        // The latest run, a passed test run `complete_task` cites, names it
        // on `cite_latest_run`; a cited run of other words is not the one.
        let mut latest = with_runs(&verified(), &[], (E3, "test_result", "PASS", &check), &[E3]);
        for action in latest["actions"].as_array_mut().unwrap() {
            if action["action"] == "cite_latest_run" {
                action["args"]["test_format"] = json!(format);
            }
        }
        assert_eq!(
            last_passed_lines(&pack_of(&latest)),
            [line.as_str()],
            "{format}"
        );
    }
    // The same words recorded with no format: the guess from the argv, a
    // count the caller's, as before U9.
    let old = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &check, H)],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    assert_eq!(
        last_passed_lines(&pack_of(&old)),
        ["Last passed: 「sh tests/check.sh」 (333333333333; --test --test-cases <N>)"]
    );
    // A run of `cargo test` recorded as `pytest` says what was recorded.
    let odd = with_runs(
        &verified(),
        &[evidence_with_format(E1, &["cargo", "test"], "pytest")],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    assert_eq!(
        last_passed_lines(&pack_of(&odd)),
        ["Last passed: 「cargo test」 (333333333333; --test --test-format pytest)"]
    );
    // The Capsule bytes of a run without a format are read to the same Pack
    // as ever: the member is the only difference between the two Capsules.
    assert!(!old.to_string().contains("test_format"));
}

/// A `test_format` outside the closed set is not a Task Capsule the Pack
/// reads, on a claim's Evidence and on `cite_latest_run` alike.
#[test]
fn a_test_format_outside_the_set_is_malformed() {
    let mut value = with_runs(
        &verified(),
        &[evidence_with_format(E1, &PYTEST, "junit")],
        (E2, "command_result", "PASS", &COMPILEALL),
        &[E1, E2],
    );
    assert!(PackCapsule::from_json(&value.to_string()).is_err());
    value["claims"][1]["evidence"][0]["test_format"] = json!("cargo");
    assert!(PackCapsule::from_json(&value.to_string()).is_ok());
    // The latest run, a passed test run, names a format outside the set.
    let mut latest = with_runs(
        &verified(),
        &[evidence(E1, "test_result", "PASS", &PYTEST, H)],
        (E3, "test_result", "PASS", &PYTEST),
        &[E1, E3],
    );
    for action in latest["actions"].as_array_mut().unwrap() {
        if action["action"] == "cite_latest_run" {
            action["args"]["test_format"] = json!("junit");
        }
    }
    assert!(PackCapsule::from_json(&latest.to_string()).is_err());
}

/// [`crate::rerun_arguments_for`]: the record first, the argv without one.
#[test]
fn the_rerun_arguments_follow_the_record_before_the_argv() {
    let words = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    let cargo = words(&["cargo", "test"]);
    let plain = words(&["sh", "tests/check.sh"]);
    for (argv, format, rerun) in [
        (&cargo, None, "--test --test-format cargo"),
        (&plain, None, "--test --test-cases <N>"),
        (
            &plain,
            Some(crate::TestFormat::Cargo),
            "--test --test-format cargo",
        ),
        (
            &plain,
            Some(crate::TestFormat::Pytest),
            "--test --test-format pytest",
        ),
        (
            &cargo,
            Some(crate::TestFormat::Cases),
            "--test --test-cases <N>",
        ),
    ] {
        assert_eq!(
            crate::rerun_arguments_for(argv, format),
            rerun,
            "{argv:?} {format:?}"
        );
    }
}
