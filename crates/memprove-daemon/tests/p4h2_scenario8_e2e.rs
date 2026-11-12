//! P4-H2: acceptance scenario 8 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > No sentence writes failed or stale evidence as verified; when the Task is
//! > judged tampered, "verified" appears nowhere; when the budget leaves things
//! > out, no count and no verdict looks better than it is.
//!
//! At X1 a fact rests on a passing `b`; after a commit (X2) it is stale. At
//! X2 a fact rests on a passing `a` (verified) and a fact on a passing `c`;
//! then `c` fails in the same state, which overrules that pass. A new fact
//! on the failing run, or on the overruled pass, is refused (`400
//! invalid_request`) and writes nothing; an open item and a Decision cite
//! the failing run and say `evidence: failed`. Every statement's text holds the
//! word verified, which stays inside 「」. At the default budget, and at the
//! largest budget of every level the Pack reaches (0, 2, 3, 4, 5) and the
//! smallest that fits, verified stands outside 「」 only where ADR 0014 E1
//! revision c allows it and the Capsule says so; the Task line is the
//! Capsule's word for word; every active claim is listed (with the
//! Capsule's verdict and reasons), folded (a `verified` fact), or counted
//! on the `Not listed: ` line exactly as the Capsule judges it; the `Omitted: `
//! counts are what the Pack does.
//!
//! Then a writer that bypasses `memproved` deletes or garbles the
//! Evidence row or the Snapshot row of the verified fact, or the link
//! between them, one at a time on a copy of the ledger. The Task is
//! `tampered`; `resume`, `resume --text`, `resume --pack` at every level it
//! reaches, the `SessionStart` hook, and the MCP `pack` tool all answer,
//! the last three with the same bytes, and no line says verified outside
//! 「」. The first integrity line ends with claim numbers may differ from before, and each
//! damaged record has its line. Before P4-H2 `resume --pack` exited 4 ("kind
//! is not a string") on these rows while `resume` and `resume --text` said
//! `tampered`. With the rows put back the Pack is the first one, byte for
//! byte.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;
mod p4h2_support;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use memprove_domain::{PACK_MAX_BUDGET, PACK_MIN_BUDGET, PackCapsule, render_pack, select_pack};

use p4b_support::{Cli, Setup, commit, git, step, write};
use p4g_support::{Mcp, McpEnv};
use p4h2_support::{
    VERDICTS, claim, claim_line, codes, fact_label, high_water, kind_word, ledger_holds, line_with,
    lines_with, number_of, numbers_in, omission, outside_quotes, pack_digest, ranges,
    session_start, smallest_budget, standing, statement_and_pack, units, verdict_word,
};
use serde_json::{Value, json};

const TITLE: &str = "scenario 8: nothing looks better than it is";
const FB: &str = "FB the b check passes, verified in the agent's words";
const FC: &str = "FC the c check passes, verified in the agent's words";
const OC: &str = "OC the c check fails while flags/fail-c exists; verified is not true";
const N1: &str = "N1 commit the README change";
const N2: &str = "N2 find out whether c keeps passing";
const N3: &str = "N3 find out why c fails, then rerun b";
const WORD: &str = "verified";

/// How often `text` says [`WORD`] as a word of its own (not inside
/// `verified_fact` or `no_verified_fact`), the reading of
/// `memprove_domain::says_verified`.
fn times_said(text: &str) -> usize {
    let word = |character: Option<char>| {
        character.is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    };
    text.match_indices(WORD)
        .filter(|(at, _)| {
            !word(text[..*at].chars().next_back()) && !word(text[at + WORD.len()..].chars().next())
        })
        .count()
}

/// FA: the verified fact, longer than the 120 characters a shortened body
/// keeps.
fn fa() -> String {
    format!(
        "FA the a check passes; verified in the agent's words; {}",
        "a covers the reader and every error it reports. ".repeat(4)
    )
    .trim_end()
    .to_owned()
}

/// DC: a Decision citing the failing run of `c`; with its rationale longer
/// than 120 characters.
const DC: &str = "DC keep c as it is; verified is the agent's word here";

fn rationale() -> String {
    "c fails in this state and nobody knows why yet, so nothing about c changes; "
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

/// X1: a fact on `b`; then a commit (X2) makes it stale. At X2: a fact on
/// `a` and a fact on a passing `c`; then `c` fails in the same state, which
/// overrules that pass. A fact can cite neither the failing run nor the
/// overruled pass (refused, nothing written); an open item and a Decision
/// cite the failing run.
fn record(setup: &Setup) {
    let agent = &setup.agent;
    agent.ok(&["begin", "--project", &setup.project_id, "--new-task", TITLE]);
    let b = agent.test("tests/b.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        FB,
        "--evidence",
        &b,
        "--next",
        N1,
    ]);
    step("a commit: the fact on b is stale");
    write(&setup.repo.join("README"), "fixture\nX2\n");
    commit(&setup.repo, "X2");
    step("X2: a passes, c passes; facts on both");
    let a = agent.test("tests/a.sh");
    let passed = agent.test("tests/c.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        &fa(),
        "--evidence",
        &a,
        "--fact",
        FC,
        "--evidence",
        &passed,
        "--next",
        N2,
    ]);
    step("c fails in the same state");
    fs::create_dir_all(setup.repo.join("flags")).unwrap();
    write(&setup.repo.join("flags/fail-c"), "1\n");
    let failed = agent.test("tests/c.sh");
    step("a fact on the failing run or on the overruled pass is refused");
    let before = high_water(setup);
    for evidence in [&failed, &passed] {
        let refused = agent.run(
            &[
                "checkpoint",
                "create",
                "--fact",
                "FX the c check passes",
                "--evidence",
                evidence,
                "--next",
                N3,
            ],
            false,
        );
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert!(
            refused
                .stderr
                .contains("createCheckpoint failed: 400 invalid_request"),
            "{}",
            refused.stderr
        );
    }
    assert_eq!(high_water(setup), before, "a refusal wrote to the ledger");
    step("an open item and a Decision cite the failing run");
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        OC,
        "--evidence",
        &failed,
        "--next",
        N3,
    ]);
    agent.ok(&[
        "decide",
        DC,
        "--rationale",
        &rationale(),
        "--evidence",
        &failed,
    ]);
}

/// The verdict and reasons the Capsule gives the claim whose text is
/// `text`.
fn judged<'a>(capsule: &'a Value, text: &str) -> &'a Value {
    claim(capsule, number_of(capsule, text))
}

/// The statements are judged as their Evidence says: nothing failed or
/// stale is `verified`.
fn check_the_judgements(capsule: &Value) {
    let verdict = |text: &str| {
        judged(capsule, text)["verdict"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let reasons = |text: &str| codes(judged(capsule, text));
    assert_eq!(verdict(&fa()), "verified");
    assert_eq!(verdict(FB), "stale");
    assert!(reasons(FB).contains("head_changed"), "{}", reasons(FB));
    assert_eq!(verdict(FC), "partial");
    assert!(
        reasons(FC).contains("evidence_not_passed"),
        "{}",
        reasons(FC)
    );
    for text in [OC, DC] {
        assert_ne!(verdict(text), "verified", "{text}");
        let evidence = &judged(capsule, text)["evidence"];
        assert_eq!(evidence[0]["run_status"], "FAIL", "{text}");
    }
    assert_ne!(capsule["status"], "verified");
}

/// The Task line the Capsule gives, word for word: its verdict and reasons,
/// the count of each kind, the facts by verdict, the superseded and the
/// withdrawn.
fn task_line(capsule: &Value) -> String {
    let active = standing(capsule, "active");
    let count = |kind: &str| active.iter().filter(|claim| claim["kind"] == kind).count();
    let breakdown = VERDICTS
        .iter()
        .filter_map(|verdict| {
            let facts = active
                .iter()
                .filter(|claim| claim["kind"] == "fact" && claim["verdict"] == *verdict)
                .count();
            (facts > 0).then(|| format!("{} {facts}", verdict_word(verdict)))
        })
        .collect::<Vec<_>>();
    let reasons = if capsule["reasons"].as_array().unwrap().is_empty() {
        String::new()
    } else {
        format!(" (reasons: {})", codes(capsule))
    };
    let withdrawn = standing(capsule, "withdrawn");
    format!(
        "Task「{}」 ({}): {}{reasons}; active claims {}: facts {}{}, open items {}, decisions {}, \
         next actions {}; superseded {}; withdrawn {} (facts {})",
        capsule["task"]["title"].as_str().unwrap(),
        capsule["task"]["status"].as_str().unwrap(),
        verdict_word(capsule["status"].as_str().unwrap()),
        active.len(),
        count("fact"),
        if breakdown.is_empty() {
            String::new()
        } else {
            format!(" ({})", breakdown.join(" / "))
        },
        count("open_item"),
        count("decision"),
        count("next_action"),
        standing(capsule, "superseded").len(),
        withdrawn.len(),
        withdrawn
            .iter()
            .filter(|claim| claim["kind"] == "fact")
            .count(),
    )
}

/// Outside 「」, verified stands only where ADR 0014 E1 revision c allows it
/// and the Capsule says so: the Task line (the Task's verdict, or the count
/// of verified facts), an active fact judged `verified`, the fold line of
/// such facts. A `tampered` Capsule has it nowhere.
fn check_the_word(pack: &str, capsule: &Value) {
    let verified_facts = standing(capsule, "active")
        .iter()
        .filter(|claim| claim["kind"] == "fact" && claim["verdict"] == "verified")
        .count();
    for line in pack.lines() {
        let outside = outside_quotes(line);
        let times = times_said(&outside);
        if times == 0 {
            continue;
        }
        assert_ne!(
            capsule["status"], "tampered",
            "{WORD} in a tampered Pack: {line}"
        );
        if line.starts_with("Task「") {
            let expected =
                usize::from(capsule["status"] == "verified") + usize::from(verified_facts > 0);
            assert_eq!(times, expected, "{line}");
            if verified_facts > 0 {
                assert!(
                    outside.contains(&format!("{WORD} {verified_facts}")),
                    "{line}"
                );
            }
        } else if line.starts_with("Folded: ") {
            assert_eq!(times, 1, "{line}");
            for number in numbers_in(line) {
                let folded = claim(capsule, number);
                assert_eq!(
                    (&folded["kind"], &folded["standing"], &folded["verdict"]),
                    (&json!("fact"), &json!("active"), &json!("verified")),
                    "#{number} folded: {line}"
                );
            }
        } else if let Some(rest) = line.strip_prefix('#') {
            let number = rest.split(' ').next().unwrap().parse::<u64>().unwrap();
            let fact = claim(capsule, number);
            assert_eq!(
                (&fact["kind"], &fact["standing"], &fact["verdict"]),
                (&json!("fact"), &json!("active"), &json!("verified")),
                "{line}"
            );
            assert_eq!(times, 1, "{line}");
            assert!(line.starts_with(&format!("#{number} {WORD}: ")), "{line}");
        } else {
            panic!("{WORD} outside 「」 on a line that may not say it: {line}");
        }
    }
}

/// The `Not listed: ` line the Capsule gives for the claims `numbers`: facts
/// by verdict, Decisions and open items by kind only (P4-C2 revision 4).
fn unlisted_line(capsule: &Value, numbers: &[u64]) -> String {
    let of_kind = |kind: &str| {
        numbers
            .iter()
            .copied()
            .filter(|number| claim(capsule, *number)["kind"] == kind)
            .collect::<Vec<_>>()
    };
    let mut parts = Vec::new();
    let facts = of_kind("fact");
    if !facts.is_empty() {
        let by_verdict = VERDICTS
            .iter()
            .filter_map(|verdict| {
                let of = facts
                    .iter()
                    .copied()
                    .filter(|number| claim(capsule, *number)["verdict"] == *verdict)
                    .collect::<Vec<_>>();
                (!of.is_empty())
                    .then(|| format!("{} {}: {}", verdict_word(verdict), of.len(), ranges(&of)))
            })
            .collect::<Vec<_>>();
        parts.push(format!(
            "facts {} ({})",
            facts.len(),
            by_verdict.join(" / ")
        ));
    }
    for kind in ["decision", "open_item"] {
        let of = of_kind(kind);
        if !of.is_empty() {
            parts.push(format!(
                "{}s {}: {}",
                kind_word(kind),
                of.len(),
                ranges(&of)
            ));
        }
    }
    format!(
        "Not listed: {} (bodies and evidence bindings not listed one by one)",
        parts.join("; ")
    )
}

/// Omission never looks better than the Capsule: the Task line is the
/// Capsule's word for word; every active claim has its own line, is on the
/// fold line (a `verified` fact), or is counted on the `Not listed: ` line as
/// the Capsule judges it, exactly once; a fact line starts with the
/// Capsule's verdict and reasons; the `Omitted: ` counts are what the Pack
/// does. Returns the level.
fn check_the_counts(pack: &str, capsule: &Value) -> u64 {
    assert_eq!(line_with(pack, "Task「"), task_line(capsule));
    let folded = lines_with(pack, "Folded: ")
        .first()
        .map(|line| numbers_in(line))
        .unwrap_or_default();
    let unlisted = lines_with(pack, "Not listed: ")
        .first()
        .map(|line| {
            let numbers = numbers_in(line);
            assert_eq!(*line, unlisted_line(capsule, &numbers));
            numbers
        })
        .unwrap_or_default();
    let mut seen = BTreeSet::new();
    for active in standing(capsule, "active") {
        let number = active["number"].as_u64().unwrap();
        let kind = active["kind"].as_str().unwrap();
        let line = claim_line(pack, number);
        let ways = [
            line.is_some(),
            folded.contains(&number),
            unlisted.contains(&number),
        ];
        assert_eq!(
            ways.iter().filter(|way| **way).count(),
            1,
            "#{number}: {ways:?}"
        );
        if kind == "next_action" {
            assert!(line.is_some(), "the next action #{number} is not listed");
        }
        if let Some(line) = line {
            let label = if kind == "fact" {
                fact_label(active["verdict"].as_str().unwrap())
            } else {
                kind_word(kind).to_owned()
            };
            assert!(line.starts_with(&format!("#{number} {label}: ")), "{line}");
            if kind == "fact" && !active["reasons"].as_array().unwrap().is_empty() {
                assert!(
                    outside_quotes(line).contains(&format!("(reasons: {}; ", codes(active))),
                    "{line}"
                );
            }
        }
        seen.insert(number);
    }
    let cut = omission(pack);
    assert_eq!(cut.folded, u64::try_from(folded.len()).unwrap(), "{cut:?}");
    assert_eq!(
        cut.unlisted,
        u64::try_from(unlisted.len()).unwrap(),
        "{cut:?}"
    );
    assert!(
        folded
            .iter()
            .chain(&unlisted)
            .all(|number| seen.contains(number)),
        "a number that is not an active claim"
    );
    cut.level
}

/// U7 P1 (Owner O3, amended 2026-10-21): `other`, read by a reader for
/// whom the open Session of `pack`'s reader is someone else's, differs
/// from `pack` only there: that Session's line ends with the takeover hint,
/// and every other line but the digest line is the same.
fn only_the_takeover_hint(other: &str, pack: &str) {
    let lines = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with("Digest: "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let (theirs, ours) = (lines(other), lines(pack));
    assert_eq!(theirs.len(), ours.len(), "{other}");
    for (their, our) in theirs.iter().zip(&ours) {
        if their != our {
            assert!(
                our.starts_with("Session ") && our.contains(" not ended ("),
                "{our}"
            );
            // U10 T7: the hint names that Session's ID and number.
            assert_eq!(*their, format!("{our}{}", p4b_support::takeover_hint(our)));
        }
    }
    assert_eq!(
        other.matches("; end it before taking over: ").count(),
        1,
        "{other}"
    );
    assert!(!pack.contains("; end it before taking over: "), "{pack}");
}

/// Every check of one Pack: it fits its budget, its digest holds, the word
/// and the counts.
fn check_pack(pack: &str, capsule: &Value, budget: usize) -> u64 {
    assert!(
        units(pack) <= budget,
        "{} code units over {budget}",
        units(pack)
    );
    pack_digest(pack);
    check_the_word(pack, capsule);
    check_the_counts(pack, capsule)
}

/// `resume --pack --budget <budget>` of the agent.
fn pack_at(agent: &Cli, budget: usize) -> String {
    agent.bytes(&["resume", "--pack", "--budget", &budget.to_string()])
}

/// The largest budget at which the Pack of the Capsule `body` stops at each
/// level, from the full Pack's length down to the smallest budget that
/// fits, as `memprove-domain`'s own selection finds them in process; and
/// that smallest budget. U7 P1 (Owner O3, amended 2026-10-21): read as the
/// agent reads it, whose own Session is `own` (the one its context names),
/// so that its open line has no takeover hint, as in `resume --pack`.
fn level_budgets(body: &str, own: Option<&str>) -> (BTreeMap<u8, usize>, usize) {
    let capsule = PackCapsule::from_json(body).unwrap().with_own_session(own);
    let full = select_pack(&capsule, PACK_MAX_BUDGET).unwrap();
    let mut found = BTreeMap::new();
    let mut smallest = PACK_MAX_BUDGET;
    for budget in (PACK_MIN_BUDGET..=units(&render_pack(&full))).rev() {
        match select_pack(&capsule, budget) {
            Ok(selection) => {
                found.entry(selection.level()).or_insert(budget);
                smallest = budget;
            }
            Err(_) => break,
        }
    }
    (found, smallest)
}

/// At the largest budget of every level the Pack reaches, and at the
/// smallest budget that fits, the real `resume --pack` passes
/// [`check_pack`] and stops at that level. Returns the levels.
fn sweep(agent: &Cli, capsule: &Value) -> BTreeSet<u64> {
    // U7 P1: the Session the agent's context names is its own.
    let shown = agent.bytes(&["context", "show"]);
    let own = shown
        .lines()
        .find_map(|line| line.strip_prefix("Session:"))
        .and_then(|rest| rest.split_whitespace().next())
        .filter(|session| *session != "none");
    let (budgets, smallest) = level_budgets(&agent.bytes(&["resume"]), own);
    assert_eq!(smallest_budget(agent), smallest);
    let mut levels = BTreeSet::new();
    for (level, budget) in &budgets {
        let pack = pack_at(agent, *budget);
        assert_eq!(check_pack(&pack, capsule, *budget), u64::from(*level));
        levels.insert(u64::from(*level));
    }
    check_pack(&pack_at(agent, smallest), capsule, smallest);
    println!("level: largest budget {budgets:?}");
    levels
}

/// The Pack of a `tampered` Task: its Task line says tampered, its first
/// integrity line ends with claim numbers may differ from before, and each damaged record
/// of the Capsule has its own integrity line.
fn check_tampered(pack: &str, capsule: &Value) {
    assert_eq!(capsule["status"], "tampered");
    let task = line_with(pack, "Task「");
    assert!(
        task.starts_with(&format!("Task「{TITLE}」 (in_progress): tampered")),
        "{task}"
    );
    let integrity = lines_with(pack, "Integrity: ");
    assert!(
        integrity[0].ends_with("; claim numbers may differ from before"),
        "{}",
        integrity[0]
    );
    let damaged = capsule["damaged_records"].as_array().unwrap();
    assert!(!damaged.is_empty());
    for record in damaged {
        let prefix = format!(
            "Integrity: {} {} is missing, does not recompute, or disagrees with the ledger ({}); \
             every conclusion is treated as tampered",
            record["record_type"].as_str().unwrap(),
            record["record_id"].as_str().unwrap(),
            record["reason"].as_str().unwrap(),
        );
        assert!(
            integrity.iter().any(|line| line.starts_with(&prefix)),
            "no line {prefix}\n{pack}"
        );
    }
    assert!(
        pack.lines()
            .any(|line| line.contains(WORD) && !outside_quotes(line).contains(WORD)),
        "the agent's own words are quoted"
    );
}

/// What one writer that bypasses `memproved` does to the verified
/// fact's records.
struct Damage {
    what: &'static str,
    sql: String,
}

fn damages(capsule: &Value) -> Vec<Damage> {
    let cited = &judged(capsule, &fa())["evidence"][0];
    let evidence = cited["evidence_id"].as_str().unwrap();
    let snapshot = cited["snapshot_id"].as_str().unwrap();
    vec![
        Damage {
            what: "the Evidence row of the verified fact is deleted",
            sql: format!("DELETE FROM evidence WHERE evidence_id = '{evidence}';"),
        },
        Damage {
            what: "the Evidence row of the verified fact does not decode",
            sql: format!(
                "UPDATE evidence SET payload = 'not json' WHERE evidence_id = '{evidence}';"
            ),
        },
        Damage {
            what: "the Snapshot row of the verified fact is deleted",
            sql: format!("DELETE FROM repository_snapshots WHERE snapshot_id = '{snapshot}';"),
        },
        Damage {
            what: "the Snapshot's link to the verified fact's Evidence is deleted",
            sql: format!(
                "DELETE FROM repository_snapshot_evidence WHERE snapshot_id = '{snapshot}' \
                 AND evidence_id = '{evidence}';"
            ),
        },
        Damage {
            what: "the Snapshot row of the verified fact does not decode",
            sql: format!(
                "UPDATE repository_snapshots SET head_oid = '{}' WHERE snapshot_id = '{snapshot}';",
                "z".repeat(40)
            ),
        },
    ]
}

/// One damage: every read answers and the Task is `tampered`; the Pack of
/// `resume --pack`, of the hook, and of the MCP `pack` tool is the same and
/// has no verified outside 「」 at any budget.
fn check_damage(setup: &Setup, damage: &Damage, client: &str) {
    let agent = &setup.agent;
    let capsule = agent.capsule();
    println!("damaged records: {}", capsule["damaged_records"]);
    assert_eq!(capsule["status"], "tampered", "{}", damage.what);
    let text = agent.run(&["resume", "--text"], false);
    assert_eq!(text.code, Some(0), "{}: {}", damage.what, text.stderr);
    for line in text.stdout.lines() {
        assert!(!outside_quotes(line).contains(WORD), "{line}");
    }
    let run = agent.run(&["resume", "--pack"], false);
    assert_eq!(run.code, Some(0), "{}: {}", damage.what, run.stderr);
    let pack = run.stdout;
    check_tampered(&pack, &capsule);
    check_pack(&pack, &capsule, 9_000);
    let levels = sweep(agent, &capsule);
    assert!(
        !levels.contains(&4),
        "a tampered Pack folds no verified fact"
    );
    assert!(levels.is_superset(&[0, 3, 5].into()), "{levels:?}");

    let hook = session_start(setup, client, "startup");
    assert_eq!(hook.code, Some(0), "{}", hook.stderr);
    let (statement, injected) = statement_and_pack(&hook.stdout);
    assert!(statement.contains(" Context Pack of Task "), "{statement}");
    // U7 P1 (Owner O3, amended 2026-10-21): a new client session's Pack:
    // resume --pack but for the takeover hint on the agent's open Session.
    only_the_takeover_hint(&injected, &pack);

    let mut mcp = Mcp::start(&McpEnv::of(
        &agent.config_dir,
        &agent.data_dir,
        &agent.token,
        &setup.repo,
    ));
    let called = mcp.ok("pack", &json!({"budget": 9000}));
    assert_eq!(called.text, pack, "the MCP pack tool returns resume --pack");
    mcp.finish();
}

#[test]
fn scenario_8_no_sentence_makes_failed_stale_or_tampered_evidence_look_verified() {
    let mut setup = Setup::new(&["a", "b"]);
    write(&setup.repo.join("tests/c.sh"), &flaky_c());
    git(&setup.repo, &["add", "."]);
    commit(&setup.repo, "the c check");
    step("scenario 8: a failing run, a stale fact, an overruled pass");
    record(&setup);
    let agent = setup.agent.clone();
    let capsule = agent.capsule();
    check_the_judgements(&capsule);

    step("every budget: the word and the counts are the Capsule's");
    let pack = agent.bytes(&["resume", "--pack"]);
    println!("{pack}");
    assert_eq!(check_pack(&pack, &capsule, 9_000), 0);
    for text in [FB, FC] {
        let number = number_of(&capsule, text);
        let line = claim_line(&pack, number).unwrap();
        assert!(!outside_quotes(line).contains(WORD), "{line}");
        assert!(line.contains(WORD), "{line}");
    }
    for text in [OC, DC] {
        let line = claim_line(&pack, number_of(&capsule, text)).unwrap();
        assert!(outside_quotes(line).contains("evidence: failed"), "{line}");
        assert!(!outside_quotes(line).contains(WORD), "{line}");
    }
    let levels = sweep(&agent, &capsule);
    assert!(levels.is_superset(&[0, 2, 3, 4, 5].into()), "{levels:?}");

    step("records of the verified fact damaged by a writer that bypasses memproved");
    setup.stop();
    let clean = setup.save("clean");
    for (index, damage) in damages(&capsule).iter().enumerate() {
        step(damage.what);
        setup.restore(&clean);
        setup.tamper(&damage.sql);
        setup.start();
        check_damage(&setup, damage, &format!("client-8-{index}"));
        setup.stop();
    }

    step("the records put back: the same Pack as before, byte for byte");
    setup.restore(&clean);
    setup.start();
    assert_eq!(agent.bytes(&["resume", "--pack"]), pack);
    ledger_holds(&setup);
    setup.stop();
}
