//! P4-H2: acceptance scenario 6 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > After the context is compacted in the middle of a session, the Pack is
//! > injected again and reflects the ledger of that moment.
//!
//! Session 1 records a verified fact on `a` and ends. Client session
//! `client-6` starts (`SessionStart`, source `startup`): the hook injects
//! the Pack of that moment, chosen by the directory's context. The agent
//! begins Session 2 as that client session, records a fact on `b`, and
//! commits a change, which makes both facts stale. Claude Code then
//! compacts the context (`SessionStart`, source `compact`): the hook
//! injects the Pack again, now chosen by the client session's context, and
//! it is the Pack of the ledger at that moment: its first line names the
//! new ledger sequence, the new Capsule digest, and the new HEAD; the fact
//! on `b` is listed; both facts are stale; Session 2 is listed as not
//! ended and as continuing from the first Pack; and it is byte for byte
//! what `resume --pack` prints then. After a rerun of `b`, its
//! reaffirmation, and a Decision, the next compaction shows the fact on `b`
//! verified and the Decision; a compaction with no write in between injects
//! the same bytes. No hook run writes to the ledger.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use std::fs;

use p4b_support::{Run, Setup, commit, git, step, write};
use p4h2_support::{
    as_client, bytes_as, claim, claim_line, env_file, fact_label, high_water, ledger_holds,
    line_with, ok_as, pack_digest, sequence_of, session_line, session_start, statement_and_pack,
};
use serde_json::Value;

/// The client session Claude Code runs the agent in.
const CLIENT: &str = "client-6";
const FA: &str = "FA the a check passes";
const FB: &str = "FB the b check passes";
const N1: &str = "N1 add the b check";
const N2: &str = "N2 rerun a after the README change";
const D1: &str = "D1 keep the checks as sh scripts";

/// The hook's run for `source` (exit 0), as the statement and the Pack.
fn injected(setup: &Setup, source: &str) -> (String, String) {
    let run = session_start(setup, CLIENT, source);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    statement_and_pack(&run.stdout)
}

/// `memprove run` of a test as the client session; returns its label.
fn test_as_client(setup: &Setup, script: &str) -> String {
    let run: Run = as_client(
        &setup.agent,
        Some(CLIENT),
        &["run", "--test-format", "cargo", "--", "sh", script],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    run.stderr
        .lines()
        .last()
        .and_then(|line| line.rsplit_once(", label "))
        .unwrap_or_else(|| panic!("no label: {}", run.stderr))
        .1
        .to_owned()
}

/// What the first line of `pack` says: the Capsule digest, the ledger
/// sequence, and the HEAD the probe read, as the Capsule `capsule` has
/// them.
fn check_first_line(pack: &str, capsule: &Value, head: &str) {
    let first = pack.lines().next().unwrap();
    assert_eq!(
        first,
        format!(
            "Pack: memprove-pack v1; capsule_digest {}; ledger sequence {}; probed HEAD {head}; \
             budget 9000 UTF-16 code units",
            capsule["capsule_digest"].as_str().unwrap(),
            capsule["bindings"]["ledger"]["project_sequence"],
        )
    );
    assert_eq!(capsule["bindings"]["probe"]["head_oid"], head);
}

/// The number of the active claim of `capsule` whose text is `text`.
fn active_number(capsule: &Value, text: &str) -> u64 {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["text"] == text && claim["standing"] == "active")
        .unwrap_or_else(|| panic!("no active claim 「{text}」"))["number"]
        .as_u64()
        .unwrap()
}

/// The fact line of `text` in `pack` starts with its number and the label
/// of the verdict `verdict`, which is the Capsule's.
fn check_fact(pack: &str, capsule: &Value, text: &str, verdict: &str) {
    let number = active_number(capsule, text);
    assert_eq!(claim(capsule, number)["verdict"], verdict, "{text}");
    let line = claim_line(pack, number).unwrap_or_else(|| panic!("#{number} is not listed"));
    assert!(
        line.starts_with(&format!("#{number} {}: 「{text}」", fact_label(verdict))),
        "{line}"
    );
}

/// Session 1: a verified fact on `a`, then `end`. Returns the Task.
fn first_session(setup: &Setup) -> String {
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 6: the Pack after compaction",
    ]);
    let a = agent.test("tests/a.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        FA,
        "--evidence",
        &a,
        "--next",
        N1,
    ]);
    agent.bytes(&["end", "--reason", "the first phase is done"]);
    begun["task_id"].as_str().unwrap().to_owned()
}

/// `SessionStart` (startup) of `client-6`: the Pack of the directory's
/// context at that moment. Returns the Pack and the ledger sequence.
fn at_startup(setup: &Setup, task: &str) -> (String, u64) {
    let agent = &setup.agent;
    let x1 = git(&setup.repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let at_start = high_water(setup);
    let (statement, first) = injected(setup, "startup");
    assert_eq!(
        statement,
        format!(
            "memprove: below is the Context Pack of Task {task} at SessionStart (startup), chosen \
             from this directory's context."
        )
    );
    let capsule = agent.capsule();
    assert_eq!(first, agent.bytes(&["resume", "--pack"]));
    check_first_line(&first, &capsule, &x1);
    assert_eq!(sequence_of(&first), at_start);
    check_fact(&first, &capsule, FA, "verified");
    assert!(!first.contains(FB), "{first}");
    (first, at_start)
}

/// The agent's work as `client-6`: `begin`, a fact on `b`, a commit.
/// Returns the new HEAD.
fn first_work(setup: &Setup, task: &str) -> String {
    let agent = &setup.agent;
    let begun = ok_as(agent, Some(CLIENT), &["begin"]);
    assert_eq!(begun["task_id"], task);
    let b = test_as_client(setup, "tests/b.sh");
    ok_as(
        agent,
        Some(CLIENT),
        &[
            "checkpoint",
            "create",
            "--fact",
            FB,
            "--evidence",
            &b,
            "--next",
            N2,
        ],
    );
    write(&setup.repo.join("README"), "fixture\nX2\n");
    commit(&setup.repo, "X2")
}

/// `SessionStart` (compact) of `client-6`: the hook writes nothing to the
/// ledger and injects what `resume --pack` of that client session prints at
/// that moment, chosen by the client session's context, its first line
/// naming that moment's ledger sequence, Capsule, and HEAD. Returns the
/// Pack and the Capsule.
fn at_compaction(setup: &Setup, task: &str, head: &str) -> (String, Value) {
    let agent = &setup.agent;
    let sequence = high_water(setup);
    let (statement, pack) = injected(setup, "compact");
    assert_eq!(
        statement,
        format!(
            "memprove: below is the Context Pack of Task {task} at SessionStart (compact), chosen \
             from this client session's context."
        )
    );
    assert_eq!(high_water(setup), sequence, "the hook wrote to the ledger");
    let capsule = agent.capsule();
    assert_eq!(
        pack,
        bytes_as(agent, Some(CLIENT), &["resume", "--pack"]),
        "the injected Pack is resume --pack at that moment"
    );
    check_first_line(&pack, &capsule, head);
    assert_eq!(sequence_of(&pack), sequence);
    (pack, capsule)
}

/// A rerun of `b` and its reaffirmation, then a Decision, as `client-6`.
fn second_work(setup: &Setup, capsule: &Value) {
    let agent = &setup.agent;
    let b = test_as_client(setup, "tests/b.sh");
    let fact_b = active_number(capsule, FB).to_string();
    ok_as(
        agent,
        Some(CLIENT),
        &[
            "checkpoint",
            "create",
            "--reaffirm",
            &fact_b,
            "--evidence",
            &b,
            "--next",
            N2,
        ],
    );
    ok_as(agent, Some(CLIENT), &["decide", D1]);
}

#[test]
fn scenario_6_after_compaction_the_hook_reinjects_the_pack_of_that_moment() {
    let setup = Setup::new(&["a", "b"]);
    step("scenario 6: Session 1 records a verified fact and ends");
    let task = first_session(&setup);

    step("client-6 starts: SessionStart (startup)");
    let (first, at_start) = at_startup(&setup, &task);
    let first_digest = pack_digest(&first);

    step("client-6 works: begin, a fact on b, a commit");
    let x2 = first_work(&setup, &task);
    assert!(high_water(&setup) > at_start);

    step("the context is compacted: SessionStart (compact)");
    let (second, capsule) = at_compaction(&setup, &task, &x2);
    assert_ne!(pack_digest(&second), first_digest);
    check_fact(&second, &capsule, FA, "stale");
    check_fact(&second, &capsule, FB, "stale");
    let session = line_with(&second, "Session 2 ");
    assert_eq!(session, session_line(&capsule, 2));
    assert!(session.contains("not ended"), "{session}");
    assert!(
        session.ends_with(&format!(
            "; continues from the Pack at ledger sequence {at_start} (self-reported, digest {})",
            &first_digest[.."sha256:".len() + 12]
        )),
        "{session}"
    );
    assert!(
        line_with(&second, "Task「").contains("(in_progress): stale"),
        "{second}"
    );

    step("a rerun, a Decision, and the next compaction shows them");
    second_work(&setup, &capsule);
    let (third, capsule) = at_compaction(&setup, &task, &x2);
    assert!(sequence_of(&third) > sequence_of(&second));
    check_fact(&third, &capsule, FA, "stale");
    check_fact(&third, &capsule, FB, "verified");
    let decision = active_number(&capsule, D1);
    assert!(
        claim_line(&third, decision)
            .unwrap()
            .starts_with(&format!("#{decision} decision: 「{D1}」")),
        "{third}"
    );
    assert!(claim_line(&second, decision).is_none());

    step("compaction with no write in between: the same Pack");
    let (fourth, _) = at_compaction(&setup, &task, &x2);
    assert_eq!(fourth, third, "same ledger, probe, and budget");
    assert_eq!(
        fs::read_to_string(env_file(&setup)).unwrap(),
        format!("export MEMPROVE_CLIENT_SESSION='{CLIENT}'\n"),
        "the hook exports the client session once"
    );
    ledger_holds(&setup);
}
