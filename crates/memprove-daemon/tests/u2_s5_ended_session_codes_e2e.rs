//! U2 stage W-B, item S5 (N-4, original #130; ADR 0014, "Amendment (U2,
//! W-B)", correcting A3 of "Amendment (U1, U-A)") through the real
//! binaries: which Problem code a write through the MCP server gets when
//! the server's own context (rule D, step 1) names a Session that was
//! ended elsewhere. No code changes with S5; this test pins what the
//! daemon answers.
//!
//! - `checkpoint` and `decide` carry no Session revision: `409
//!   invalid_transition` (the Session accepts no Event).
//! - `end` sends the revision its context file recorded, which the end
//!   elsewhere moved on: `409 revision_conflict` (the daemon checks the
//!   revision before the state).
//! - `end {session, abandoned}` reads the Session's current revision from
//!   `listSessions`: `409 invalid_transition`.
//! - `complete` transitions the Task (`transitionTask` carries the Task's
//!   revision, not the Session's), so the daemon would not refuse it; U3
//!   (K1, Owner O3) refuses it in the server: it reads the Session first and
//!   sends nothing (`session_not_active`).

mod p4b_support;
mod p4g_support;

use p4b_support::{Setup, step};
use p4g_support::{Called, Mcp, McpEnv};
use serde_json::json;

fn refused(called: &Called, status: u16, code: &str) {
    assert!(called.is_error, "{}", called.line);
    assert_eq!(called.failure(), Some("problem"), "{}", called.line);
    assert_eq!(called.structured["status"], status, "{}", called.line);
    assert_eq!(called.structured["code"], code, "{}", called.line);
}

#[test]
fn u2_s5_a_write_to_a_session_ended_elsewhere_gets_these_codes() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u2 s5",
    ]);
    let session = begun["session_id"].as_str().unwrap().to_owned();
    let r1 = agent.test("tests/a.sh");
    // `memprove run` records Events, which leave the Session's revision.
    let recorded = begun["session_revision"].as_u64().unwrap();

    step("the operator ends the Session elsewhere; the agent's context still names it");
    let ended = setup.operator.ok(&[
        "session",
        "end",
        &session,
        "--revision",
        &recorded.to_string(),
        "--reason",
        "ended elsewhere",
    ]);
    assert_eq!(ended["revision"].as_u64(), Some(recorded + 1), "{ended}");
    let shown = agent.run(&["context", "show"], true);
    assert!(shown.stdout.contains(&session), "{}", shown.stdout);

    step("the server's own context (rule D, step 1) names the ended Session");
    let mut mcp = Mcp::start(&McpEnv::of(
        &agent.config_dir,
        &agent.data_dir,
        &agent.token,
        &setup.repo,
    ));
    let checkpoint = mcp.call(
        "checkpoint",
        &json!({"facts": [{"text": "a passes", "evidence": [r1]}], "next": {"text": "n"}}),
    );
    refused(&checkpoint, 409, "invalid_transition");
    assert_eq!(checkpoint.structured["operation"], "createCheckpoint");
    assert_eq!(
        checkpoint.text,
        "invalid_transition: the state does not allow this operation (409 invalid_transition)"
    );
    let decide = mcp.call("decide", &json!({"text": "plan a"}));
    refused(&decide, 409, "invalid_transition");
    assert_eq!(decide.structured["operation"], "proposeDecision");

    // The revision the context recorded is no longer current.
    let end = mcp.call("end", &json!({"reason": "done"}));
    refused(&end, 409, "revision_conflict");
    assert_eq!(end.structured["operation"], "endSession");
    assert_eq!(
        end.text,
        "revision_conflict: the revision has changed (409 revision_conflict); a fresh read, then a retry"
    );

    // With the current revision, the state is what refuses.
    let abandoned = mcp.call("end", &json!({"session": 1, "abandoned": true}));
    refused(&abandoned, 409, "invalid_transition");
    assert_eq!(abandoned.structured["operation"], "endSession");
    // `complete` is a transition of the Task, not a write to the Session:
    // the daemon would not refuse it. U3 (K1, Owner O3): the server reads
    // the Session first and sends nothing while it is not active.
    let complete = mcp.call("complete", &json!({}));
    assert!(complete.is_error, "{}", complete.line);
    assert_eq!(complete.failure(), Some("session_not_active"));
    assert_eq!(complete.structured["state"], "ended");
    mcp.finish();

    step("no claim was written after the end");
    let capsule = agent.capsule();
    assert!(
        capsule["claims"].as_array().unwrap().is_empty(),
        "{capsule}"
    );
}
