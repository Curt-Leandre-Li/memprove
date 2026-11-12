//! The review of the first P4-H1 commit through the real binaries (Owner
//! decisions P4-H1: no read of the Project ledger answers `5xx` because a
//! row of it is missing or does not decode).
//!
//! - A zero Project sequence: `ledger verify` exits 5 with the Event as its
//!   `first_failure` and no sequence (before: a bodiless `500`, exit 4).
//! - An Event payload whose bytes are not UTF-8: `ledger verify` exits 5
//!   (before: `503 service_not_ready`, exit 1); `resume` and `resume --text`
//!   exit 0 with the Task `tampered` (before: exit 1).
//! - A Snapshot identity whose bytes are not UTF-8: `ledger verify` exits
//!   5, `resume` exits 0 with the Task `tampered` and the Snapshot named
//!   `\u{fffd}`, and `resume --pack` renders (before: `409`, exit 1).
//! - A zero Task revision: `resume` and `recover preview` exit 0 with the
//!   Task `tampered`, and `task list` answers `409 capsule_tampered`
//!   (before: a bodiless `500`, exit 4).
//! - A state database whose binding (`state_metadata`) is missing: `ledger
//!   verify` and `resume` answer `409 capsule_tampered`, which no retry
//!   cures (before: `503 service_not_ready`, retryable).
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use memprove_domain::says_verified;
use p4b_support::{Setup, step};
use serde_json::Value;

/// A Task with a verified fact in the Workspace of `setup`; returns it.
fn verified_task(setup: &Setup) -> String {
    let agent = &setup.agent;
    step("a verified fact");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "P4-H1 review",
    ]);
    agent.test("tests/a.sh");
    agent.ok(&["checkpoint", "create", "--fact", "a passes", "--next", "n1"]);
    assert_eq!(agent.ok(&["resume"])["status"], "verified");
    begun["task_id"].as_str().unwrap().to_owned()
}

/// `ledger verify`, which must exit 5; returns its `first_failure`.
fn first_failure(setup: &Setup) -> Value {
    let run = setup.agent.run(&["ledger", "verify"], false);
    assert_eq!(run.code, Some(5), "{}{}", run.stdout, run.stderr);
    let verified: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(verified["valid"], false);
    verified["first_failure"].clone()
}

#[test]
fn zero_values_and_text_that_is_not_utf8_never_answer_5xx() {
    let mut setup = Setup::new(&["a"]);
    let task = verified_task(&setup);
    setup.stop();
    let pristine = setup.save("pristine");

    step("the first Event's Project sequence is 0");
    setup.tamper(
        "PRAGMA ignore_check_constraints = ON; \
         UPDATE events SET project_sequence = 0 WHERE rowid = (SELECT MIN(rowid) FROM events);",
    );
    setup.start();
    let failure = first_failure(&setup);
    assert_eq!(failure["record_type"], "event", "{failure}");
    assert_eq!(failure["project_sequence"], Value::Null, "{failure}");
    setup.stop();
    setup.restore(&pristine);

    step("a command.completed payload is not UTF-8");
    setup.tamper(
        "UPDATE events SET payload = CAST(X'FF' AS TEXT) WHERE rowid = \
         (SELECT MIN(rowid) FROM events WHERE event_type = 'command.completed');",
    );
    setup.start();
    assert_eq!(first_failure(&setup)["record_type"], "event");
    assert_eq!(setup.agent.ok(&["resume"])["status"], "tampered");
    let text = setup.agent.run(&["resume", "--text"], false);
    assert_eq!(text.code, Some(0), "{}", text.stderr);
    assert!(!says_verified(&text.stdout), "{}", text.stdout);
    setup.stop();
    setup.restore(&pristine);

    step("a Snapshot identity is not UTF-8");
    setup.tamper(
        "UPDATE repository_snapshots SET snapshot_id = CAST(X'FF' AS TEXT) \
         WHERE rowid = (SELECT MIN(rowid) FROM repository_snapshots);",
    );
    setup.start();
    assert_eq!(first_failure(&setup)["record_type"], "snapshot");
    let capsule = setup.agent.ok(&["resume"]);
    assert_eq!(capsule["status"], "tampered");
    assert!(
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["record_id"] == "\\u{fffd}"),
        "{capsule}"
    );
    let pack = setup.agent.run(&["resume", "--pack"], false);
    assert_eq!(pack.code, Some(0), "{}", pack.stderr);
    assert!(!says_verified(&pack.stdout), "{}", pack.stdout);
    setup.stop();
    setup.restore(&pristine);

    step("the Task's revision is 0");
    setup.tamper(&format!(
        "PRAGMA ignore_check_constraints = ON; \
         UPDATE tasks SET revision = 0 WHERE task_id = '{task}';"
    ));
    setup.start();
    let agent = &setup.agent;
    assert_eq!(agent.ok(&["resume"])["status"], "tampered");
    assert_eq!(agent.ok(&["recover", "preview"])["status"], "tampered");
    let listed = agent.run(&["task", "list", "--project", &setup.project_id], false);
    assert_eq!(listed.code, Some(1), "{}", listed.stderr);
    assert!(
        listed.stderr.contains("409 capsule_tampered"),
        "{}",
        listed.stderr
    );
    setup.stop();
}

#[test]
fn a_state_database_not_bound_to_its_project_is_not_retryable() {
    let mut setup = Setup::new(&["a"]);
    verified_task(&setup);
    setup.stop();

    step("the state database's binding row is deleted");
    setup.tamper("DELETE FROM state_metadata;");
    setup.start();
    for words in [&["ledger", "verify"][..], &["resume"]] {
        let run = setup.agent.run(words, false);
        assert_eq!(run.code, Some(1), "{words:?}: {}", run.stderr);
        assert!(
            run.stderr.contains("409 capsule_tampered"),
            "{words:?}: {}",
            run.stderr
        );
        assert!(!run.stderr.contains("503"), "{words:?}: {}", run.stderr);
    }
    setup.stop();
}
