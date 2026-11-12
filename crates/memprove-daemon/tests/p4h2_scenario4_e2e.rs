//! P4-H2: acceptance scenario 4 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > After a stage, the agent reaffirms every stale fact in one call; a fact
//! > whose command was not rerun or did not pass does not become verified, and
//! > each is explained.
//!
//! Four facts, one on each of the checks `a`, `b`, `c`, and `d`, are
//! `verified` at X1. After the first phase (commit X2) all four are stale;
//! the agent reruns the four checks and one `checkpoint create
//! --reaffirm-all` reaffirms every stale fact, each `verified`. After the
//! second phase (commit X3) the agent reruns `a` and `b`, does not rerun
//! `c`, and reruns `d`, which fails: one `--reaffirm-all` reaffirms the
//! facts on `a` and `b`; the facts on `c` and `d` stay stale, are named one
//! by one with the reason, and the Pack shows them stale with their reruns.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use std::fs;

use p4b_support::{Setup, checkpoint, commit, git, last_lines, step, write};
use p4h2_support::{active_numbers, claim, claim_line, ledger_holds, line_with, lines_with};
use serde_json::Value;

/// The facts, one per check.
const FACTS: [(&str, &str); 4] = [
    ("a", "F1 the a check passes"),
    ("b", "F2 the b check passes"),
    ("c", "F3 the c check passes"),
    ("d", "F4 the d check passes"),
];

/// A test script of `d` that fails while `flags/fail-d` exists (`flags/` is
/// ignored, so the repository state stays the same).
const FLAKY_D: &str = "#!/bin/sh\nif [ -f flags/fail-d ]; then\n  echo 'test d ... FAILED'\n  \
                       echo 'test result: FAILED. 0 passed; 1 failed'\n  exit 1\nfi\n\
                       echo 'test d ... ok'\necho 'test result: ok. 1 passed; 0 failed'\n";

/// The number of the active fact whose text is `text`.
fn active_fact(capsule: &Value, text: &str) -> u64 {
    active_numbers(capsule, "fact")
        .into_iter()
        .find(|number| claim(capsule, *number)["text"] == text)
        .unwrap_or_else(|| panic!("no active fact 「{text}」"))
}

/// The verdict of the active fact whose text is `text`.
fn verdict(capsule: &Value, text: &str) -> String {
    claim(capsule, active_fact(capsule, text))["verdict"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// X1: the four checks pass and four facts cite them, `verified`.
fn four_verified_facts(setup: &Setup) {
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 4",
    ]);
    let mut words = vec!["--next".to_owned(), "N1 finish the first phase".to_owned()];
    for (check, text) in FACTS {
        let label = agent.test(&format!("tests/{check}.sh"));
        words.extend([
            "--fact".to_owned(),
            text.to_owned(),
            "--evidence".to_owned(),
            label,
        ]);
    }
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    checkpoint(agent, &words, 0);
    let capsule = agent.capsule();
    for (_, text) in FACTS {
        assert_eq!(verdict(&capsule, text), "verified", "{text}");
    }
}

/// A phase ends: README changes and is committed.
fn end_of_phase(setup: &Setup, phase: &str) {
    write(&setup.repo.join("README"), &format!("fixture\n{phase}\n"));
    commit(&setup.repo, phase);
}

/// The `reaffirmed` line `--reaffirm-all` prints for `rerun` (check and
/// run label, in the order of the facts), mapping each fact before the
/// call to the claim that reaffirms it after, all `verified`.
fn reaffirmed_line(before: &Value, after: &Value, rerun: &[(&str, String)]) -> String {
    let items = FACTS
        .iter()
        .filter_map(|(check, text)| {
            let (_, label) = rerun.iter().find(|(of, _)| of == check)?;
            let old = active_fact(before, text);
            let new = active_fact(after, text);
            assert!(new > old, "{text} was not reaffirmed");
            assert_eq!(claim(after, old)["standing"], "superseded");
            Some(format!("#{old} ({label}) as #{new}: verified"))
        })
        .collect::<Vec<_>>();
    format!("memprove: reaffirm-all reaffirmed: {}", items.join(", "))
}

/// Phase 1: every check is rerun and passes; one call reaffirms every
/// stale fact.
fn first_phase(setup: &Setup) {
    let agent = &setup.agent;
    end_of_phase(setup, "X2");
    let before = agent.capsule();
    for (_, text) in FACTS {
        assert_eq!(verdict(&before, text), "stale", "{text}");
    }
    let rerun = FACTS
        .iter()
        .map(|(check, _)| (*check, agent.test(&format!("tests/{check}.sh"))))
        .collect::<Vec<_>>();
    let run = checkpoint(
        agent,
        &["--reaffirm-all", "--next", "N2 finish the second phase"],
        0,
    );
    let after = agent.capsule();
    assert_eq!(
        last_lines(&run.stderr, 3),
        [
            reaffirmed_line(&before, &after, &rerun),
            "memprove: reaffirm-all skipped: none".to_owned(),
            "memprove: reaffirmed 4 (verified 4), skipped 0".to_owned(),
        ]
    );
    for (_, text) in FACTS {
        assert_eq!(verdict(&after, text), "verified", "{text}");
    }
    assert_eq!(after["status"], "verified");
    let pack = agent.bytes(&["resume", "--pack"]);
    assert!(lines_with(&pack, "Rerun: ").is_empty(), "{pack}");
    assert!(
        line_with(&pack, "Task「").contains("(in_progress): verified; "),
        "{pack}"
    );
}

/// Phase 2: `a` and `b` are rerun and pass, `c` is not rerun, `d` is rerun
/// and fails. One call reaffirms the facts on `a` and `b`; the facts on `c`
/// and `d` are named one by one and stay stale.
fn second_phase(setup: &Setup) {
    let agent = &setup.agent;
    end_of_phase(setup, "X3");
    let before = agent.capsule();
    let rerun = vec![
        ("a", agent.test("tests/a.sh")),
        ("b", agent.test("tests/b.sh")),
    ];
    fs::create_dir_all(setup.repo.join("flags")).unwrap();
    write(&setup.repo.join("flags/fail-d"), "1\n");
    let failed = agent.test("tests/d.sh");
    let run = checkpoint(
        agent,
        &["--reaffirm-all", "--next", "N3 fix d, rerun c and d"],
        0,
    );
    let after = agent.capsule();
    let (c, d) = (
        active_fact(&before, FACTS[2].1),
        active_fact(&before, FACTS[3].1),
    );
    assert_eq!(
        last_lines(&run.stderr, 3),
        [
            reaffirmed_line(&before, &after, &rerun),
            format!(
                "memprove: reaffirm-all skipped: #{c} (「sh tests/c.sh」 has no run in this \
                 repository state), #{d} (the latest run {failed} of 「sh tests/d.sh」 in this \
                 repository state did not pass (FAIL))"
            ),
            "memprove: reaffirmed 2 (verified 2), skipped 2".to_owned(),
        ]
    );
    for (check, text) in FACTS {
        let expected = if matches!(check, "a" | "b") {
            "verified"
        } else {
            "stale"
        };
        assert_eq!(verdict(&after, text), expected, "{text}");
    }
    assert_eq!(
        (
            active_fact(&after, FACTS[2].1),
            active_fact(&after, FACTS[3].1)
        ),
        (c, d)
    );
    check_the_pack(setup, &after, c, d);
}

/// The Pack shows the facts on `c` and `d` stale, never `verified`, each
/// on the rerun of its own command; the Task is not `verified`.
fn check_the_pack(setup: &Setup, capsule: &Value, c: u64, d: u64) {
    let pack = setup.agent.bytes(&["resume", "--pack"]);
    print!("{pack}");
    for number in [c, d] {
        let line = claim_line(&pack, number).unwrap();
        assert!(
            line.starts_with(&format!("#{number} fact stale: ")),
            "{line}"
        );
    }
    for (check, number) in [("c", c), ("d", d)] {
        assert_eq!(
            line_with(&pack, &format!("Rerun: 「sh tests/{check}.sh」")),
            format!("Rerun: 「sh tests/{check}.sh」 on the current HEAD (for #{number})")
        );
    }
    for text in [FACTS[0].1, FACTS[1].1] {
        let number = active_fact(capsule, text);
        let line = claim_line(&pack, number).unwrap();
        assert!(line.starts_with(&format!("#{number} verified: ")), "{line}");
    }
    assert_ne!(capsule["status"], "verified");
    assert!(
        !line_with(&pack, "Task「").contains("): verified; "),
        "{pack}"
    );
}

#[test]
fn scenario_4_one_call_reaffirms_every_stale_fact_and_names_the_rest() {
    let setup = Setup::new(&["a", "b", "c"]);
    write(&setup.repo.join("tests/d.sh"), FLAKY_D);
    git(&setup.repo, &["add", "."]);
    commit(&setup.repo, "d can fail on a flag");
    step("X1: four verified facts");
    four_verified_facts(&setup);

    step("after the first phase: every check rerun, one call reaffirms all four");
    first_phase(&setup);

    step("after the second phase: c not rerun, d fails; both named, both stay stale");
    second_phase(&setup);
    ledger_holds(&setup);
}
