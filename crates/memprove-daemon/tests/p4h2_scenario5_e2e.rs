//! P4-H2: acceptance scenario 5 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > When the conditions for completing the Task do not hold, the agent sees
//! > "cannot complete now" and why in the Pack beforehand, without running
//! > into a refusal first.
//!
//! A Task with a passed run and a fact at X1. After a commit (X2) with no
//! run since, the Pack, read before any attempt, says `Cannot: complete the Task`
//! with `evidence_stale`; `memprove task complete` is then refused with
//! that code. Blocked as well, the Pack lists `invalid_transition` first,
//! and the refusal is `invalid_transition`. A refusal writes nothing. Back
//! in progress with a passed run of X2, the Pack says `Can: complete the Task`,
//! and `task complete` succeeds; the completed Task's Pack says it cannot
//! be completed again.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use p4b_support::{Setup, commit, step, write};
use p4h2_support::{high_water, ledger_holds, line_with, lines_with};
use serde_json::Value;

/// The `complete_task` action of the Capsule.
fn complete_task(capsule: &Value) -> &Value {
    capsule["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["action"] == "complete_task")
        .unwrap()
}

/// The reasons of an action, in check order.
fn reasons(action: &Value) -> Vec<String> {
    action["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reason| reason.as_str().unwrap().to_owned())
        .collect()
}

/// The Problem code `memprove` reported on standard error
/// (`… failed: <status> <code>…`).
fn refusal(stderr: &str) -> (String, String) {
    stderr
        .lines()
        .find_map(|line| {
            let (_, rest) = line.split_once(" failed: ")?;
            let (status, code) = rest.split_once(' ')?;
            let code = code.split([':', ' ']).next()?;
            Some((status.to_owned(), code.to_owned()))
        })
        .unwrap_or_else(|| panic!("no Problem in {stderr}"))
}

/// Reads the Pack before any attempt: one `Cannot: complete the Task` line with
/// `reasons`, the Capsule's own; then `task complete` is refused with the
/// first of them, `409`, and the ledger does not move.
fn cannot_complete(setup: &Setup, expected: &[&str]) {
    let agent = &setup.agent;
    let capsule = agent.capsule();
    let action = complete_task(&capsule);
    assert_eq!(action["available"], false, "{action}");
    assert_eq!(reasons(action), expected);
    let pack = agent.bytes(&["resume", "--pack"]);
    assert!(
        lines_with(&pack, "Can: complete the Task").is_empty(),
        "{pack}"
    );
    assert_eq!(
        line_with(&pack, "Cannot: complete the Task"),
        format!(
            "Cannot: complete the Task (transitionTask; reasons: {}; needs mcp-write)",
            expected.join(", ")
        )
    );
    let before = high_water(setup);
    let run = agent.run(&["task", "complete"], false);
    assert_eq!(run.code, Some(1), "{}{}", run.stdout, run.stderr);
    assert_eq!(
        refusal(&run.stderr),
        ("409".to_owned(), expected[0].to_owned())
    );
    assert_eq!(high_water(setup), before, "a refusal wrote to the ledger");
    assert_eq!(agent.capsule()["task"]["status"], capsule["task"]["status"]);
}

/// `task transition` of the context's Task to `target`.
fn transition(setup: &Setup, task: &str, target: &str) {
    let revision = setup.agent.capsule()["task"]["revision"].to_string();
    setup.agent.ok(&[
        "task",
        "transition",
        task,
        "--to",
        target,
        "--revision",
        &revision,
    ]);
}

#[test]
fn scenario_5_the_pack_says_beforehand_whether_the_task_can_be_completed() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    step("X1: a passes, one fact");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 5",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    agent.test("tests/a.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F1 a passes",
        "--next",
        "N1",
    ]);

    step("X2 without a run: Cannot with evidence_stale, before any attempt");
    write(&setup.repo.join("README"), "fixture\nX2\n");
    commit(&setup.repo, "X2");
    cannot_complete(&setup, &["evidence_stale"]);

    step("blocked as well: invalid_transition is listed first and answered");
    transition(&setup, &task, "blocked");
    cannot_complete(&setup, &["invalid_transition", "evidence_stale"]);

    step("in progress, a passes at X2: Can, and completion succeeds");
    transition(&setup, &task, "in_progress");
    agent.test("tests/a.sh");
    let capsule = agent.capsule();
    assert_eq!(complete_task(&capsule)["available"], true);
    assert!(reasons(complete_task(&capsule)).is_empty());
    let pack = agent.bytes(&["resume", "--pack"]);
    assert!(
        lines_with(&pack, "Cannot: complete the Task").is_empty(),
        "{pack}"
    );
    assert_eq!(
        line_with(&pack, "Can: complete the Task"),
        "Can: complete the Task (transitionTask; cites 1 passed runs of the current repository state; needs mcp-write)"
    );
    let completed = agent.ok(&["task", "complete"]);
    assert_eq!(completed["state"], "completed", "{completed}");

    step("completed: the Pack says it cannot be completed again");
    let after = agent.capsule();
    assert_eq!(after["task"]["status"], "completed");
    assert_eq!(reasons(complete_task(&after)), ["invalid_transition"]);
    let pack = agent.bytes(&["resume", "--pack"]);
    assert!(line_with(&pack, "Task「").starts_with("Task「scenario 5」 (completed): "));
    assert_eq!(
        line_with(&pack, "Cannot: complete the Task"),
        "Cannot: complete the Task (transitionTask; reasons: invalid_transition; needs mcp-write)"
    );
    ledger_holds(&setup);
}
