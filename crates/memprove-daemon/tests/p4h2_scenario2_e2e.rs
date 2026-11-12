//! P4-H2: acceptance scenario 2 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > The Pack says: the Task's verdict; the verdict and reasons of every fact
//! > still in force; the commands to rerun to get back to "verified"; the open
//! > items, Decisions, and next action; whether the last Session ended normally;
//! > how much was left out and the command to see it.
//!
//! Session 1 records facts on the checks `a`, `b`, and `c`, an open item, a
//! next action, and a Decision, and ends normally. After a commit Session 2
//! reaffirms the fact on `a`, records a fact on a passing `c` that a later
//! failing `c` of the same state overrules, an open item citing that
//! failure, and a next action, and ends abandoned. The Pack then holds one
//! fact `verified`, one `stale`, one `partial`. At the default budget it
//! writes every line in full; at a budget that shortens bodies and at the
//! smallest budget that fits (level 5) it keeps the Task verdict, the
//! reruns, the next action, the Session lines, and says how many were left
//! out and the command that shows them, which gives the full lines back.
//! Running the commands the rerun lines name and one `--reaffirm-all`
//! brings every fact back to `verified`.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use std::fs;

use p4b_support::{Setup, commit, step, write};
use p4h2_support::{
    BUDGET, Omission, VERDICTS, active_numbers, claim, claim_line, codes, fact_label, kind_word,
    ledger_holds, line_with, lines_with, number_of, omission, ranges, session_line, session_number,
    session_part, short, standing, units, verdict_word,
};
use serde_json::Value;

const F1: &str = "F1 the a check passes";
const F3: &str = "F3 the c check passes";
const F4: &str = "F4 the c check passes in the new state";
const O1: &str = "O1 the d check is not written yet";
const N1: &str = "N1 commit the README change, then rerun the checks";
const N1B: &str = "N1b find out whether c passes in the new state";
const N2: &str = "N2 rerun b and c, then reaffirm every fact that is not verified";
const D1: &str = "D1 keep the checks as sh scripts";

/// F2: longer than the 120 characters a shortened body keeps.
fn f2() -> String {
    format!(
        "F2 the b check passes; {}",
        "b covers the parser and its errors. ".repeat(5)
    )
    .trim_end()
    .to_owned()
}

/// O2: longer than 120 characters; it cites the failing run of `c`.
fn o2() -> String {
    format!(
        "O2 the c check fails while flags/fail-c exists; {}",
        "find out why the flag breaks it. ".repeat(5)
    )
    .trim_end()
    .to_owned()
}

/// The Decision's rationale, longer than 120 characters with its text.
fn rationale() -> String {
    "no new tool is needed, and every agent can read a shell script; "
        .repeat(3)
        .trim_end()
        .to_owned()
}

/// A test script of `c` that fails while `flags/fail-c` exists (`flags/` is
/// ignored, so the repository state stays the same).
fn flaky_c() -> String {
    "#!/bin/sh\nif [ -f flags/fail-c ]; then\n  echo 'test c ... FAILED'\n  \
     echo 'test result: FAILED. 0 passed; 1 failed'\n  exit 1\nfi\necho 'test c ... ok'\n\
     echo 'test result: ok. 1 passed; 0 failed'\n"
        .to_owned()
}

/// Session 1 at X1: three facts, an open item, a next action, a Decision;
/// it ends normally.
fn first_session(setup: &Setup) {
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 2: what the Pack says",
    ]);
    let (a, b, c) = (
        agent.test("tests/a.sh"),
        agent.test("tests/b.sh"),
        agent.test("tests/c.sh"),
    );
    let f2 = f2();
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        F1,
        "--evidence",
        &a,
        "--fact",
        &f2,
        "--evidence",
        &b,
        "--fact",
        F3,
        "--evidence",
        &c,
        "--open",
        O1,
        "--next",
        N1,
    ]);
    agent.ok(&["decide", D1, "--rationale", &rationale()]);
    agent.ok(&["end", "--reason", "the first phase is done"]);
}

/// Session 2 at X2 (it declares the Pack read after Session 1): `a`
/// reaffirmed and F4 on a passing `c` superseding F3, with N1b; then `c`
/// fails in the same state, and O2 cites the failure, superseding O1, with
/// N2. It ends abandoned.
fn second_session(setup: &Setup) {
    let agent = &setup.agent;
    write(&setup.repo.join("README"), "fixture\nX2\n");
    commit(&setup.repo, "X2");
    agent.ok(&["begin"]);
    let capsule = agent.capsule();
    let a = agent.test("tests/a.sh");
    let c = agent.test("tests/c.sh");
    let (one, three) = (number_of(&capsule, F1), number_of(&capsule, F3));
    agent.ok(&[
        "checkpoint",
        "create",
        "--reaffirm",
        &one.to_string(),
        "--evidence",
        &a,
        "--fact",
        F4,
        "--evidence",
        &c,
        "--supersedes",
        &three.to_string(),
        "--next",
        N1B,
    ]);
    fs::create_dir_all(setup.repo.join("flags")).unwrap();
    write(&setup.repo.join("flags/fail-c"), "1\n");
    let failed = agent.test("tests/c.sh");
    let o2 = o2();
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        &o2,
        "--evidence",
        &failed,
        "--supersedes",
        &number_of(&capsule, O1).to_string(),
        "--next",
        N2,
    ]);
    agent.ok(&["end", "--abandoned", "--reason", "the agent stopped"]);
}

/// The fact line of `claim` with its body as `body`: label, reasons,
/// Evidence binding, and the Session it came from.
fn fact_line(capsule: &Value, claim: &Value, body: &str) -> String {
    let (head, tail) = fact_skeleton(capsule, claim);
    format!("{head}{body}{tail}")
}

/// What a fact line keeps around its body at every level that lists it:
/// number and verdict before, reasons, binding, and Session after.
fn fact_skeleton(capsule: &Value, claim: &Value) -> (String, String) {
    let reasons = if claim["reasons"].as_array().unwrap().is_empty() {
        String::new()
    } else {
        format!("reasons: {}; ", codes(claim))
    };
    let bases = claim["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|evidence| short(evidence["base_commit"].as_str().unwrap()))
        .collect::<Vec<_>>()
        .join(", ");
    (
        format!(
            "#{} {}: ",
            claim["number"],
            fact_label(claim["verdict"].as_str().unwrap())
        ),
        format!(
            " ({reasons}evidence bound to commit {bases}; from Session {})",
            session_number(capsule, &claim["session_id"])
        ),
    )
}

/// The line of an open item, Decision, or next action: no verdict, no
/// reason code; `no evidence cited`, or the cited Evidence's result and whether its
/// commit is still the current state.
fn other_line(capsule: &Value, claim: &Value, head: &str) -> String {
    let evidence = claim["evidence"].as_array().unwrap();
    let cited = match &evidence[..] {
        [] => "no evidence cited".to_owned(),
        [only] => {
            let base = only["base_commit"].as_str().unwrap();
            assert_eq!(
                base, head,
                "a test of this scenario cites the current state"
            );
            let result = if only["run_status"] == "PASS" {
                "passed"
            } else {
                "failed"
            };
            format!(
                "evidence: {result}, the bound commit {} is still the current state",
                short(base)
            )
        }
        _ => panic!("this scenario cites one run at most"),
    };
    let text = format!("「{}」", claim["text"].as_str().unwrap());
    let body = match claim["rationale"].as_str() {
        Some(rationale) => format!("{text}, rationale 「{rationale}」"),
        None => text,
    };
    format!(
        "#{} {}: {body} ({cited}; from Session {})",
        claim["number"],
        kind_word(claim["kind"].as_str().unwrap()),
        session_number(capsule, &claim["session_id"])
    )
}

/// The full line of every active claim, by number.
fn full_lines(capsule: &Value) -> Vec<(u64, String)> {
    let head = capsule["bindings"]["probe"]["head_oid"].as_str().unwrap();
    standing(capsule, "active")
        .iter()
        .map(|claim| {
            let line = if claim["kind"] == "fact" {
                let body = format!("「{}」", claim["text"].as_str().unwrap());
                fact_line(capsule, claim, &body)
            } else {
                other_line(capsule, claim, head)
            };
            (claim["number"].as_u64().unwrap(), line)
        })
        .collect()
}

/// The Task line: its verdict and reasons are the Capsule's.
fn check_task_line(pack: &str, capsule: &Value) {
    let task = line_with(pack, "Task「");
    let verdict = capsule["status"].as_str().unwrap();
    assert_ne!(verdict, "verified", "a stale fact stands");
    let prefix = format!(
        "Task「scenario 2: what the Pack says」 (in_progress): {} (reasons: {}); active claims 6: \
         facts 3 (verified 1 / stale 1 / unconfirmed 1), open items 1, decisions 1, next \
         actions 1; ",
        verdict_word(verdict),
        codes(capsule)
    );
    assert!(task.starts_with(&prefix), "{task}\n{prefix}");
}

/// The rerun lines are the Capsule's reruns, and every fact that is not
/// `verified` is on one.
fn check_reruns(pack: &str, capsule: &Value) {
    let expected = capsule["rerun"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rerun| {
            let argv = rerun["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|word| word.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(" ");
            let claims = rerun["claims"]
                .as_array()
                .unwrap()
                .iter()
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Rerun: 「{argv}」 on the current HEAD (for {claims})")
        })
        .collect::<Vec<_>>();
    assert_eq!(lines_with(pack, "Rerun: "), expected);
    assert!(lines_with(pack, "No rerun needed: ").is_empty());
    for claim in standing(capsule, "active") {
        if claim["kind"] == "fact" && claim["verdict"] != "verified" {
            let number = format!("#{}", claim["number"]);
            assert!(
                expected.iter().any(|line| line.contains(&number)),
                "{number} has no rerun"
            );
        }
    }
}

/// The Session lines of `pack`: the summary counts every Session; the
/// latest that ended (the previous Session) is listed with how it ended.
fn check_sessions(pack: &str, capsule: &Value, summary: &str, listed: &[(usize, &str)]) {
    let mut expected = vec![summary.to_owned()];
    for (number, state) in listed {
        let line = session_line(capsule, *number);
        assert!(line.contains(&format!(" {state} (started ")), "{line}");
        expected.push(line);
    }
    assert_eq!(session_part(pack), expected);
}

/// After Session 1: the previous Session ended normally.
fn check_after_first_session(setup: &Setup) {
    let capsule = setup.agent.capsule();
    let pack = setup.agent.bytes(&["resume", "--pack"]);
    check_sessions(
        &pack,
        &capsule,
        "Sessions: 1 in all, 1 ended normally, 0 ended abandoned, 0 not ended, 0 declared a resumed_from Pack",
        &[(1, "ended normally")],
    );
}

/// What every Pack of this Task says at any level.
fn check_always(pack: &str, capsule: &Value) {
    check_task_line(pack, capsule);
    check_reruns(pack, capsule);
    // Session 1 ended normally before; only the latest that ended, Session
    // 2, is listed, abandoned (P4-H1). Session 2 declared the Pack read
    // after Session 1.
    check_sessions(
        pack,
        capsule,
        "Sessions: 2 in all, 1 ended normally, 1 ended abandoned, 0 not ended, 1 declared a resumed_from Pack",
        &[(2, "ended abandoned")],
    );
    let next = claim(capsule, active_numbers(capsule, "next_action")[0]);
    let head = capsule["bindings"]["probe"]["head_oid"].as_str().unwrap();
    assert_eq!(next["text"], N2);
    let next_line = other_line(capsule, next, head);
    assert_eq!(
        claim_line(pack, next["number"].as_u64().unwrap()),
        Some(next_line.as_str())
    );
    assert!(units(pack) <= budget_of(pack), "{} code units", units(pack));
}

/// The budget the first line names.
fn budget_of(pack: &str) -> usize {
    let first = pack.lines().next().unwrap();
    first
        .rsplit_once("; budget ")
        .and_then(|(_, rest)| rest.strip_suffix(" UTF-16 code units"))
        .unwrap()
        .parse()
        .unwrap()
}

/// At the default budget: every claim line in full, nothing left out.
fn check_default(pack: &str, capsule: &Value) {
    assert_eq!(budget_of(pack), BUDGET);
    check_always(pack, capsule);
    for (number, line) in full_lines(capsule) {
        assert_eq!(claim_line(pack, number), Some(line.as_str()), "#{number}");
    }
    assert_eq!(
        omission(pack),
        Omission {
            level: 0,
            bases_counted: 0,
            shortened: 0,
            omitted: 0,
            folded: 0,
            unlisted: 0
        }
    );
    for superseded in standing(capsule, "superseded") {
        assert!(claim_line(pack, superseded["number"].as_u64().unwrap()).is_none());
    }
}

/// `resume --pack --claims <numbers>` gives back the full lines.
fn check_full_text(setup: &Setup, capsule: &Value, numbers: &[u64]) {
    assert!(!numbers.is_empty());
    let list = numbers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let shown = setup.agent.bytes(&["resume", "--pack", "--claims", &list]);
    let full = full_lines(capsule);
    for number in numbers {
        let (_, line) = full.iter().find(|(of, _)| of == number).unwrap();
        assert_eq!(
            claim_line(&shown, *number),
            Some(line.as_str()),
            "#{number}"
        );
    }
}

/// At a budget that shortens bodies: the skeleton of every fact stays
/// (verdict, reasons, binding), the counts on the `Omitted: ` line are the
/// shortened and omitted lines, and the command shows them in full.
fn check_shortened(setup: &Setup, pack: &str, capsule: &Value) {
    check_always(pack, capsule);
    let cut = omission(pack);
    assert!((2..=3).contains(&cut.level), "{cut:?}");
    let shortened = lines_with(pack, "#")
        .iter()
        .filter(|line| line.contains("… (body "))
        .count();
    let omitted = lines_with(pack, "#")
        .iter()
        .filter(|line| line.contains("(body omitted, "))
        .count();
    assert_eq!(
        (cut.shortened, cut.omitted),
        (shortened as u64, omitted as u64)
    );
    assert!(shortened + omitted > 0);
    let mut cut_numbers = Vec::new();
    for claim in standing(capsule, "active") {
        let number = claim["number"].as_u64().unwrap();
        let line = claim_line(pack, number).unwrap_or_else(|| panic!("#{number} is not listed"));
        if claim["kind"] == "fact" {
            let (head, tail) = fact_skeleton(capsule, claim);
            assert!(line.starts_with(&head) && line.ends_with(&tail), "{line}");
        }
        if line.contains("… (body ") || line.contains("(body omitted, ") {
            cut_numbers.push(number);
        }
    }
    check_full_text(setup, capsule, &cut_numbers);
}

/// At level 5: the verified facts are folded into one line (level 4) and
/// the claims not listed are counted, facts by verdict, the others by
/// kind, with their numbers; the command shows them all in full.
fn check_level_5(setup: &Setup, pack: &str, capsule: &Value) {
    check_always(pack, capsule);
    let cut = omission(pack);
    assert_eq!(cut.level, 5, "{cut:?}");
    let folded = active_numbers(capsule, "fact")
        .into_iter()
        .filter(|number| claim(capsule, *number)["verdict"] == "verified")
        .collect::<Vec<_>>();
    assert_eq!(cut.folded, folded.len() as u64);
    assert_eq!(
        line_with(pack, "Folded: "),
        format!(
            "Folded: verified active facts {}: {} (bodies and evidence bindings not listed one by one)",
            folded.len(),
            ranges(&folded)
        )
    );
    let unlisted = standing(capsule, "active")
        .iter()
        .map(|claim| claim["number"].as_u64().unwrap())
        .filter(|number| claim_line(pack, *number).is_none() && !folded.contains(number))
        .collect::<Vec<_>>();
    assert_eq!(cut.unlisted, unlisted.len() as u64);
    let of = |kind: &str, verdict: Option<&str>| {
        unlisted
            .iter()
            .copied()
            .filter(|number| {
                let claim = claim(capsule, *number);
                claim["kind"] == kind && verdict.is_none_or(|verdict| claim["verdict"] == verdict)
            })
            .collect::<Vec<_>>()
    };
    let facts = VERDICTS
        .iter()
        .filter(|verdict| !of("fact", Some(verdict)).is_empty())
        .map(|verdict| {
            let numbers = of("fact", Some(verdict));
            format!(
                "{} {}: {}",
                verdict_word(verdict),
                numbers.len(),
                ranges(&numbers)
            )
        })
        .collect::<Vec<_>>();
    let mut parts = vec![format!(
        "facts {} ({})",
        of("fact", None).len(),
        facts.join(" / ")
    )];
    for kind in ["decision", "open_item"] {
        let numbers = of(kind, None);
        if !numbers.is_empty() {
            parts.push(format!(
                "{}s {}: {}",
                kind_word(kind),
                numbers.len(),
                ranges(&numbers)
            ));
        }
    }
    assert_eq!(
        line_with(pack, "Not listed: "),
        format!(
            "Not listed: {} (bodies and evidence bindings not listed one by one)",
            parts.join("; ")
        )
    );
    check_full_text(setup, capsule, &[folded, unlisted].concat());
}

/// The smallest budget at which the Pack fits: what `resume --pack`
/// names when the budget of 1000 is too small, else 1000.
fn smallest_budget(setup: &Setup) -> String {
    let run = setup
        .agent
        .run(&["resume", "--pack", "--budget", "1000"], false);
    if run.code == Some(0) {
        return "1000".to_owned();
    }
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    run.stderr
        .split_once("needs ")
        .and_then(|(_, rest)| rest.split(' ').next())
        .unwrap()
        .to_owned()
}

/// The commands the rerun lines name, then one `--reaffirm-all`: every
/// fact is `verified` again and the Pack has no rerun line.
fn rerun_as_the_pack_says(setup: &Setup, capsule: &Value) {
    let agent = &setup.agent;
    fs::remove_file(setup.repo.join("flags/fail-c")).unwrap();
    agent.ok(&["begin"]);
    for rerun in capsule["rerun"].as_array().unwrap() {
        let argv = rerun["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap())
            .collect::<Vec<_>>();
        let words = [&["run", "--test-format", "cargo", "--"][..], &argv].concat();
        let run = agent.run(&words, false);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
    }
    let run = agent.run(
        &["checkpoint", "create", "--reaffirm-all", "--next", N2],
        false,
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let after = agent.capsule();
    assert_eq!(after["status"], "verified");
    for number in active_numbers(&after, "fact") {
        assert_eq!(claim(&after, number)["verdict"], "verified", "#{number}");
    }
    let pack = agent.bytes(&["resume", "--pack"]);
    assert!(lines_with(&pack, "Rerun: ").is_empty(), "{pack}");
    assert!(
        line_with(&pack, "Task「").contains("): verified; "),
        "{pack}"
    );
}

#[test]
fn scenario_2_the_pack_states_what_the_next_agent_needs() {
    let setup = Setup::new(&["a", "b"]);
    write(&setup.repo.join("tests/c.sh"), &flaky_c());
    p4b_support::git(&setup.repo, &["add", "."]);
    commit(&setup.repo, "c can fail on a flag");
    step("Session 1 ends normally; Session 2 ends abandoned");
    first_session(&setup);
    check_after_first_session(&setup);
    second_session(&setup);
    let capsule = setup.agent.capsule();
    let verdicts = active_numbers(&capsule, "fact")
        .iter()
        .map(|number| {
            claim(&capsule, *number)["verdict"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        verdicts,
        ["stale", "verified", "partial"],
        "F2, F1 again, F4"
    );

    step("the default budget: everything in full");
    let pack = setup.agent.bytes(&["resume", "--pack"]);
    print!("{pack}");
    check_default(&pack, &capsule);

    step("a budget that shortens bodies");
    let budget = (units(&pack) - 60).to_string();
    let shorter = setup
        .agent
        .bytes(&["resume", "--pack", "--budget", &budget]);
    print!("{shorter}");
    check_shortened(&setup, &shorter, &capsule);

    step("the smallest budget that fits: level 5");
    let smallest = smallest_budget(&setup);
    let least = setup
        .agent
        .bytes(&["resume", "--pack", "--budget", &smallest]);
    print!("{least}");
    check_level_5(&setup, &least, &capsule);

    step("the commands the rerun lines name bring every fact back to verified");
    rerun_as_the_pack_says(&setup, &capsule);
    ledger_holds(&setup);
}
