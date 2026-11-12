//! U8 A5 (Owner O4; ADR 0014, "Amendment (U8, S-A)") through the real
//! binaries: a person corrects by Session. `resume --text --session <k>`
//! lists only the statements `Session k` wrote, and `checkpoint create
//! --withdraw-session <k> --withdraw-reason <why>` withdraws every one of
//! them that is still in force, as that many `--withdraw <number>`: the
//! ledger holds one `withdraws` edge per statement and no new Event type.
//! A `k` that names no Session, or a Session with nothing in force, is a
//! usage error (exit 2), and more than 500 statements are refused, both
//! with nothing sent.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Cli, Setup, checkpoint, step};
use serde_json::Value;

fn claim_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

/// The `withdraws` edges `memproved` recorded.
fn withdraw_edges(operator: &Cli, project: &str) -> usize {
    let page = operator.ok(&["event", "list", "--project", project, "--limit", "500"]);
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| {
            event["event_type"] == "claim.superseded" && event["payload"]["relation"] == "withdraws"
        })
        .count()
}

/// Session 1 (the orchestrator) and Session 2 (a sub-agent in its own context)
/// each record a fact, an open item, and a next action, and end.
fn two_sessions(setup: &Setup) -> Cli {
    let lead = &setup.agent;
    let begun = lead.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u8 a5",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let run = lead.test("tests/a.sh");
    let words = [
        "--fact",
        "lead fact",
        "--evidence",
        &run,
        "--open",
        "lead open",
        "--next",
        "lead next",
    ];
    checkpoint(lead, &words, 0);
    lead.bytes(&["end", "--reason", "lead done"]);
    let child = lead.elsewhere(setup.root_path.join("config-child"), "child");
    child.ok(&["begin", "--project", &setup.project_id, "--task", &task]);
    let run = child.test("tests/a.sh");
    let words = [
        "--fact",
        "child fact",
        "--evidence",
        &run,
        "--open",
        "child open",
        "--next",
        "child next",
    ];
    checkpoint(&child, &words, 0);
    child.bytes(&["end", "--reason", "child done"]);
    child
}

#[test]
fn u8_a5_a_person_reads_and_withdraws_one_sessions_statements() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    two_sessions(&setup);

    step("resume --text --session k lists only Session k's statements");
    for (k, mine, other) in [("2", "child", "lead"), ("1", "lead", "child")] {
        let text = lead.bytes(&["resume", "--text", "--all", "--session", k]);
        let lines = claim_lines(&text);
        assert!(!lines.is_empty(), "{text}");
        for line in &lines {
            assert!(line.contains(&format!("from Session {k}")), "{line}");
            assert!(!line.contains(other), "{line}");
        }
        assert!(text.contains(&format!("{mine} fact")), "{text}");
        assert!(text.starts_with("Task「u8 a5」"), "{text}");
    }
    for k in ["3", "0", "x"] {
        let run = lead.run(&["resume", "--text", "--session", k], false);
        assert_eq!(run.code, Some(2), "{}", run.stderr);
    }

    step("--withdraw-session 2 withdraws each of its statements, one edge each");
    let before = withdraw_edges(&setup.operator, &setup.project_id);
    lead.ok(&["begin"]);
    lead.test("tests/a.sh");
    let words = [
        "--withdraw-session",
        "2",
        "--withdraw-reason",
        "the sub-agent's work is redone",
        "--next",
        "redo the child's part",
    ];
    checkpoint(lead, &words, 0);
    assert_eq!(
        withdraw_edges(&setup.operator, &setup.project_id),
        before + 3
    );
    let capsule = lead.capsule();
    let child_session = &capsule["sessions"][1]["session_id"];
    for claim in capsule["claims"].as_array().unwrap() {
        let standing = claim["standing"].as_str().unwrap();
        if claim["session_id"] == *child_session {
            assert_eq!(standing, "withdrawn", "{claim}");
            assert_eq!(claim["withdrawal_reason"], "the sub-agent's work is redone");
        } else {
            assert_ne!(standing, "withdrawn", "{claim}");
        }
    }

    step("nothing of Session 2 is in force any more: a usage error, nothing sent");
    let sequence = |capsule: &Value| capsule["bindings"]["ledger"]["project_sequence"].clone();
    let at = sequence(&lead.capsule());
    let run = checkpoint(lead, &words, 2);
    assert!(run.stderr.contains("Session 2"), "{}", run.stderr);
    let run = checkpoint(lead, &["--withdraw-session", "2", "--next", "n"], 2);
    assert!(run.stderr.contains("--withdraw-reason"), "{}", run.stderr);
    assert_eq!(sequence(&lead.capsule()), at);
}

#[test]
fn u8_a5_more_than_500_statements_are_refused_and_nothing_is_sent() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    lead.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u8 a5 limit",
    ]);
    lead.test("tests/a.sh");
    let texts = (1..=500).map(|n| format!("open {n}")).collect::<Vec<_>>();
    let mut words = Vec::new();
    for text in &texts {
        words.extend(["--open", text.as_str()]);
    }
    words.extend(["--next", "the 501st"]);
    checkpoint(lead, &words, 0);
    let at = lead.capsule()["bindings"]["ledger"]["project_sequence"].clone();
    let refused = [
        "--withdraw-session",
        "1",
        "--withdraw-reason",
        "too many",
        "--next",
        "n",
    ];
    let run = checkpoint(lead, &refused, 2);
    assert!(run.stderr.contains("500"), "{}", run.stderr);
    assert_eq!(lead.capsule()["bindings"]["ledger"]["project_sequence"], at);
}
