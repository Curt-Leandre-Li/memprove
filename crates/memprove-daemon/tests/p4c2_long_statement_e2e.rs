//! P4-C2 revision 7 through the real binaries: `checkpoint create` notes on
//! standard error a fact or open item longer than 240 characters (the
//! Context Pack may show only its first 120); 240 characters get no note;
//! the exit code and the recorded Checkpoint do not change.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, checkpoint, step};

#[test]
fn a_long_statement_gets_a_note_and_the_same_exit_code() {
    let mut setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Long statements",
    ]);
    agent.test("tests/a.sh");
    let note = "may show only its first 120 characters";
    step("an open item of 241 characters");
    let long = "€".repeat(241);
    let run = checkpoint(agent, &["--open", &long, "--next", "n1"], 0);
    assert!(
        run.stderr
            .contains("memprove: note: the open item at $.open_items[0] is 241 characters long"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains(note), "{}", run.stderr);
    step("an open item of 240 characters, and a next action of 1000");
    let exact = "€".repeat(240);
    let next = "n".repeat(1_000);
    let run = checkpoint(agent, &["--open", &exact, "--next", &next], 0);
    assert!(!run.stderr.contains(note), "{}", run.stderr);
    step("a fact of 241 characters");
    let fact = "f".repeat(241);
    let run = checkpoint(agent, &["--fact", &fact, "--next", "n2"], 0);
    assert!(
        run.stderr
            .contains("the fact at $.verified_completed_work[0] is 241 characters long"),
        "{}",
        run.stderr
    );
    let capsule = agent.capsule();
    let texts = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|claim| claim["text"].as_str().unwrap().chars().count())
        .collect::<Vec<_>>();
    assert!(texts.contains(&241) && texts.contains(&240), "{texts:?}");
    setup.stop();
}
