//! P4-C2 (Owner, T19 SF-1) through the real binaries: a Session row whose
//! `agent_id` is not a UUID made every read of its Task `503
//! service_not_ready` (`resume`, `recover preview`, `resume --pack`). Now the
//! reads answer, the Task is `tampered`, the Session is named a damaged
//! record of type `session`, and no "verified" is left in the Pack.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use memprove_domain::says_verified;
use p4b_support::{Setup, step};
use serde_json::Value;

#[test]
fn a_session_row_that_does_not_decode_makes_the_task_tampered_not_unavailable() {
    let mut setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    step("a verified fact in Session 1");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "SF-1",
    ]);
    let session = begun["session_id"].as_str().unwrap().to_owned();
    agent.test("tests/a.sh");
    agent.ok(&["checkpoint", "create", "--fact", "a passes", "--next", "n1"]);
    let before: Value = agent.ok(&["resume"]);
    assert_eq!(before["status"], "verified");
    let pack = agent.run(&["resume", "--pack"], false);
    assert_eq!(pack.code, Some(0), "{}", pack.stderr);
    assert!(says_verified(&pack.stdout));

    step("the Session row's agent_id is not a UUID");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE sessions SET agent_id = 'someone-else' WHERE session_id = '{session}';"
    ));
    setup.start();
    let agent = &setup.agent;
    let after: Value = agent.ok(&["resume"]);
    assert_eq!(after["status"], "tampered");
    assert_eq!(
        after["damaged_records"],
        serde_json::json!([{
            "record_type": "session",
            "record_id": session,
            "reason": "object_digest_mismatch",
        }])
    );
    let preview: Value = agent.ok(&["recover", "preview"]);
    assert_eq!(preview["status"], "tampered");
    let pack = agent.run(&["resume", "--pack"], false);
    assert_eq!(pack.code, Some(0), "{}", pack.stderr);
    println!("{}", pack.stdout);
    assert!(!says_verified(&pack.stdout), "{}", pack.stdout);
    assert!(
        pack.stdout
            .lines()
            .any(|line| line.starts_with(&format!("Integrity: session {session} "))),
        "{}",
        pack.stdout
    );
    let text = agent.run(&["resume", "--text"], false);
    assert_eq!(text.code, Some(0), "{}", text.stderr);
    setup.stop();
}
