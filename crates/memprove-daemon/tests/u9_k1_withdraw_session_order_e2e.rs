//! U9 K1 (U8 V-N3) through the real binaries: `checkpoint create
//! --withdraw-session <k>` validates `k` (it names a Session of the Task,
//! with statements in force, at most 500) before it asks for `--snapshot`.
//! A context that records no Snapshot used to hear "requires --snapshot"
//! first, and learned that `k` was wrong only after recording a run.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, checkpoint, sequence, step};

#[test]
fn u9_k1_withdraw_session_is_checked_before_the_snapshot() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    let begun = lead.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u9 k1",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let at = sequence(&lead.capsule());

    step("no Snapshot and a k that names no Session: k's error, exit 2, nothing sent");
    let words = [
        "--open",
        "o",
        "--next",
        "n",
        "--withdraw-session",
        "9",
        "--withdraw-reason",
        "redone",
    ];
    let run = checkpoint(lead, &words, 2);
    assert!(
        run.stderr.contains("--withdraw-session 9 names no Session"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("--snapshot"), "{}", run.stderr);
    assert_eq!(sequence(&lead.capsule()), at);

    step("no Snapshot and a Session with nothing in force: that error, exit 2");
    let words = [
        "--open",
        "o",
        "--next",
        "n",
        "--withdraw-session",
        "1",
        "--withdraw-reason",
        "redone",
    ];
    let run = checkpoint(lead, &words, 2);
    assert!(
        run.stderr.contains("has no statement in force"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("--snapshot"), "{}", run.stderr);

    step("no --withdraw-session: the Snapshot is still required, as before");
    let run = checkpoint(lead, &["--open", "o", "--next", "n"], 2);
    assert!(run.stderr.contains("requires --snapshot"), "{}", run.stderr);

    step("a valid k and no Snapshot: now the Snapshot is asked for, exit 2");
    let run_label = lead.test("tests/a.sh");
    let words = [
        "--fact",
        "lead fact",
        "--evidence",
        &run_label,
        "--next",
        "n",
    ];
    checkpoint(lead, &words, 0);
    lead.bytes(&["end", "--reason", "lead done"]);
    let child = lead.elsewhere(setup.root_path.join("config-child"), "child");
    child.ok(&["begin", "--project", &setup.project_id, "--task", &task]);
    let at = sequence(&child.capsule());
    let words = [
        "--open",
        "o",
        "--next",
        "n",
        "--withdraw-session",
        "1",
        "--withdraw-reason",
        "redone",
    ];
    let run = checkpoint(&child, &words, 2);
    assert!(run.stderr.contains("requires --snapshot"), "{}", run.stderr);
    assert_eq!(sequence(&child.capsule()), at);
    let words = [
        "--open",
        "o",
        "--next",
        "n",
        "--withdraw-session",
        "5",
        "--withdraw-reason",
        "redone",
    ];
    let run = checkpoint(&child, &words, 2);
    assert!(
        run.stderr.contains("--withdraw-session 5 names no Session"),
        "{}",
        run.stderr
    );
}

/// U10 N5 (U9 VN5): a `k` whose Session has more than 500 statements in
/// force (the most one Checkpoint withdraws) is told before the missing
/// Snapshot, as a `k` that names no Session is: the check of `k` includes
/// the limit. Without a Snapshot and with one the error is the same, and
/// nothing is sent.
#[test]
fn u10_n5_a_k_over_500_is_told_before_the_missing_snapshot() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    let begun = lead.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u10 n5",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    lead.test("tests/a.sh");

    step("the first Session leaves 510 open items in force (three Checkpoints of 170)");
    for chunk in 0..3 {
        let mut words = Vec::new();
        for item in 0..170 {
            words.push("--open".to_owned());
            words.push(format!("open item {chunk}-{item}"));
        }
        words.extend(["--next".to_owned(), format!("next {chunk}")]);
        let words = words.iter().map(String::as_str).collect::<Vec<_>>();
        checkpoint(lead, &words, 0);
    }
    lead.bytes(&["end", "--reason", "lead done"]);

    step(
        "a second Session has no Snapshot: k = 1 is told as over the limit, not as 'requires --snapshot'",
    );
    let child = lead.elsewhere(setup.root_path.join("config-child"), "child");
    child.ok(&["begin", "--project", &setup.project_id, "--task", &task]);
    let at = sequence(&child.capsule());
    let words = [
        "--open",
        "o",
        "--next",
        "n",
        "--withdraw-session",
        "1",
        "--withdraw-reason",
        "redone",
    ];
    let run = checkpoint(&child, &words, 2);
    assert!(
        run.stderr
            .contains("has 511 statements in force, more than the 500 one Checkpoint withdraws"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("--snapshot"), "{}", run.stderr);
    assert_eq!(sequence(&child.capsule()), at);

    step("with a Snapshot the error is the same");
    child.test("tests/a.sh");
    let at = sequence(&child.capsule());
    let run = checkpoint(&child, &words, 2);
    assert!(
        run.stderr
            .contains("has 511 statements in force, more than the 500 one Checkpoint withdraws"),
        "{}",
        run.stderr
    );
    assert_eq!(sequence(&child.capsule()), at);
}
