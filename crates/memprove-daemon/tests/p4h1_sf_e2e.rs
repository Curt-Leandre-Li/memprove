//! P4-H1 (Owner, T20 SF-1 and SF-2) through the real binaries.
//!
//! - SF-1: with the Workspace's `workspaces` row deleted (a writer that
//!   bypasses `memproved`, foreign keys off), `resume` (with and without
//!   `--workspace`), `resume --text`, `resume --pack`, `recover preview`,
//!   and `task list` exit 0: the probe is unavailable, nothing is
//!   `verified`, no damaged record is named. Before P4-H1 they were
//!   `503 service_not_ready`, `retryable` (exit 1).
//! - SF-2: with a statement's `evidence_id` of the latest Checkpoint made
//!   `x`, `ledger verify` exits 5 and names that Checkpoint as its
//!   `first_failure`. Before P4-H1 it was `503 service_not_ready` (exit 1).
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use memprove_domain::says_verified;
use p4b_support::{Setup, step};
use serde_json::{Value, json};

/// A Task with a verified fact in the Workspace of `setup`; returns the
/// Workspace, the Task, and the latest Checkpoint.
fn verified_task(setup: &Setup) -> (String, String, String) {
    let agent = &setup.agent;
    step("a verified fact");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "P4-H1",
    ]);
    agent.test("tests/a.sh");
    let created = agent.ok(&["checkpoint", "create", "--fact", "a passes", "--next", "n1"]);
    assert_eq!(agent.ok(&["resume"])["status"], "verified");
    (
        begun["workspace_id"].as_str().unwrap().to_owned(),
        begun["task_id"].as_str().unwrap().to_owned(),
        created["checkpoint_id"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn sf1_a_missing_workspace_row_leaves_the_probe_unavailable() {
    let mut setup = Setup::new(&["a"]);
    let (workspace, task, _) = verified_task(&setup);

    step("the Workspace row is deleted");
    setup.stop();
    setup.tamper(&format!(
        "DELETE FROM workspaces WHERE workspace_id = '{workspace}';"
    ));
    assert_eq!(
        setup.query(&format!(
            "SELECT count(*) FROM workspaces WHERE workspace_id = '{workspace}';"
        )),
        "0"
    );
    setup.start();
    let agent = &setup.agent;
    for words in [&["resume"][..], &["resume", "--workspace", &workspace]] {
        let capsule = agent.ok(words);
        assert_eq!(
            capsule["bindings"]["probe"]["available"], false,
            "{words:?}"
        );
        assert_eq!(
            capsule["bindings"]["probe"]["workspace_id"],
            workspace.as_str()
        );
        assert_ne!(capsule["status"], "verified", "{words:?}");
        assert_eq!(capsule["damaged_records"], json!([]), "{words:?}");
        assert!(!capsule.to_string().contains("\"verified\""), "{words:?}");
    }
    let preview: Value = agent.ok(&["recover", "preview"]);
    assert_eq!(preview["bindings"]["probe"]["available"], false);
    assert_ne!(preview["status"], "verified");
    for words in [&["resume", "--text"][..], &["resume", "--pack"]] {
        let run = agent.run(words, false);
        assert_eq!(run.code, Some(0), "{words:?}: {}", run.stderr);
        assert!(!says_verified(&run.stdout), "{words:?}: {}", run.stdout);
    }
    let tasks = agent.ok(&["task", "list", "--project", &setup.project_id]);
    assert!(
        tasks["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["task_id"] == task.as_str()),
        "{tasks}"
    );
    setup.stop();
}

#[test]
fn sf2_ledger_verify_names_a_statement_evidence_id_that_does_not_decode() {
    let mut setup = Setup::new(&["a"]);
    let (_, _, checkpoint) = verified_task(&setup);
    assert_eq!(setup.agent.run(&["ledger", "verify"], false).code, Some(0));

    step("a statement's evidence_id of the latest Checkpoint is not a UUID");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE checkpoint_item_evidence SET evidence_id = 'x' \
         WHERE checkpoint_id = '{checkpoint}';"
    ));
    setup.start();
    let run = setup.agent.run(&["ledger", "verify"], false);
    assert_eq!(run.code, Some(5), "{}", run.stderr);
    let verified: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(verified["valid"], false);
    assert_eq!(verified["first_failure"]["record_type"], "checkpoint");
    assert_eq!(verified["first_failure"]["record_id"], checkpoint.as_str());
    assert!(
        verified["first_failure"]["reason"]
            .as_str()
            .unwrap()
            .contains("evidence_id"),
        "{verified}"
    );
    let capsule = setup.agent.ok(&["resume"]);
    assert_eq!(capsule["status"], "tampered");
    setup.stop();
}
