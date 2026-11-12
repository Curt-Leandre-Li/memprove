//! U8 A6 (Owner O4, O13; ADR 0013, "Amendment (U8, S-A)") through the
//! real binaries: the Task Capsule's `claims[]` item of a withdrawn
//! statement carries `withdrawn_by {checkpoint_id, session_id}`, the
//! Checkpoint whose `withdraws` edge withdrew it and that Checkpoint's
//! Session; no other claim has the member, so a Task without a withdrawal
//! keeps its Capsule bytes. `resume --text --all` ends the withdrawn line
//! with ` (withdrawn by Session j)`. U8 S-F round 1, B2: when the Checkpoint that
//! wrote the edge cannot be read, `withdrawn_by` is `null`, never an
//! earlier readable Checkpoint, and the line says ` (withdrawer unreadable)`.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, checkpoint, claim, step};
use serde_json::{Value, json};

#[test]
fn u8_a6_a_withdrawn_statement_names_the_checkpoint_and_session_that_withdrew_it() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    let begun = lead.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u8 a6",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let run = lead.test("tests/a.sh");
    let words = [
        "--fact",
        "a passes",
        "--evidence",
        &run,
        "--open",
        "b is missing",
        "--next",
        "write b",
    ];
    checkpoint(lead, &words, 0);
    lead.bytes(&["end", "--reason", "lead done"]);
    step("nothing withdrawn: no claim has withdrawn_by");
    let capsule = lead.capsule();
    for item in capsule["claims"].as_array().unwrap() {
        assert!(item.get("withdrawn_by").is_none(), "{item}");
    }

    step("Session 2, another context, withdraws #2");
    let child = lead.elsewhere(setup.root_path.join("config-child"), "child");
    let begun = child.ok(&["begin", "--project", &setup.project_id, "--task", &task]);
    let second = begun["session_id"].as_str().unwrap().to_owned();
    child.test("tests/a.sh");
    let words = [
        "--withdraw",
        "2",
        "--withdraw-reason",
        "b is not needed",
        "--next",
        "nothing left",
    ];
    checkpoint(&child, &words, 0);
    let capsule = child.capsule();
    let withdrawn = claim(&capsule, 2);
    assert_eq!(withdrawn["standing"], "withdrawn");
    let by_checkpoint = claim(&capsule, 4)["checkpoint_id"].clone();
    assert_eq!(
        withdrawn["withdrawn_by"],
        json!({"checkpoint_id": by_checkpoint, "session_id": second})
    );
    for number in [1, 3, 4] {
        assert!(
            claim(&capsule, number).get("withdrawn_by").is_none(),
            "#{number}"
        );
    }

    step("resume --text --all says which Session withdrew it");
    let text = child.bytes(&["resume", "--text", "--all"]);
    let line = text
        .lines()
        .find(|line| line.starts_with("#2 withdrawn: "))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.ends_with(" (withdrawn by Session 2)"), "{line}");
}

/// U8 S-F round 1, B2 (the independent check's reproduction): the
/// withdrawing Checkpoint's record is changed so that it no longer
/// recomputes, and the store leaves it out of the view. `withdrawn_by` is
/// then `null`: not the withdrawn statements' own Checkpoint, which is the
/// readable one right before the edge, and the withdrawal is not put on
/// the Session it overruled.
#[test]
fn u8_f1_b2_a_withdrawer_whose_checkpoint_cannot_be_read_is_null() {
    let mut setup = Setup::new(&["a"]);
    let by_checkpoint = {
        let lead = &setup.agent;
        let begun = lead.ok(&[
            "begin",
            "--project",
            &setup.project_id,
            "--new-task",
            "u8 f1 b2",
        ]);
        let task = begun["task_id"].as_str().unwrap().to_owned();
        let run = lead.test("tests/a.sh");
        let words = [
            "--fact",
            "a passes",
            "--evidence",
            &run,
            "--open",
            "b is missing",
            "--next",
            "write b",
        ];
        checkpoint(lead, &words, 0);
        lead.bytes(&["end", "--reason", "lead done"]);
        let child = lead.elsewhere(setup.root_path.join("config-child"), "child");
        let begun = child.ok(&["begin", "--project", &setup.project_id, "--task", &task]);
        let second = begun["session_id"].as_str().unwrap().to_owned();
        child.test("tests/a.sh");
        let words = [
            "--withdraw",
            "1",
            "--withdraw",
            "2",
            "--withdraw-reason",
            "owner veto",
            "--next",
            "nothing left",
        ];
        checkpoint(&child, &words, 0);
        let capsule = child.capsule();
        let by_checkpoint = claim(&capsule, 4)["checkpoint_id"].clone();
        assert_eq!(
            claim(&capsule, 2)["withdrawn_by"],
            json!({"checkpoint_id": by_checkpoint, "session_id": second})
        );
        by_checkpoint.as_str().unwrap().to_owned()
    };

    step("the withdrawing Checkpoint's record no longer recomputes");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE checkpoints SET objective = 'changed' WHERE checkpoint_id = '{by_checkpoint}';"
    ));
    setup.start();
    let agent = &setup.agent;
    let capsule = agent.capsule();
    assert_eq!(capsule["status"], "tampered");
    for number in [1, 2] {
        let withdrawn = claim(&capsule, number);
        assert_eq!(withdrawn["standing"], "withdrawn", "#{number}");
        assert_eq!(withdrawn["withdrawn_by"], Value::Null, "#{number}");
    }

    step("resume --text --all says it cannot be read");
    let text = agent.bytes(&["resume", "--text", "--all"]);
    for number in [1, 2] {
        let line = text
            .lines()
            .find(|line| line.starts_with(&format!("#{number} withdrawn: ")))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(line.ends_with(" (withdrawer unreadable)"), "{line}");
    }
    assert!(!text.contains("(withdrawn by "), "{text}");
}
