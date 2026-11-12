//! U3 stage X-K, item K1 (W-B-1, original #52, Owner O3; ADR 0014,
//! "Amendment (U3, X-K)", revising A3) through the real binaries: the MCP
//! `complete` tool and the CLI's `task complete` read the context's Session
//! once before they send `transitionTask`, and refuse with a fixed text,
//! sending nothing, when it is not active. The Task stays `in_progress` and
//! the Project's ledger sequence does not move. A Session that is active
//! still completes, as before.

mod p4b_support;
mod p4g_support;

use p4b_support::{Cli, Setup, step};
use p4g_support::{Mcp, McpEnv};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

/// The fixed sentence of the tool error `session_not_active`.
const NOT_ACTIVE: &str = "the context's Session is not an active Session (state ended); nothing was \
                          sent; completing the Task needs an active Session: begin, then memprove \
                          run on the command line, then complete";

/// The Project's ledger sequence and the Task's state and revision.
fn observed(setup: &Setup, task: &str) -> (u64, Value) {
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    let tasks = setup
        .operator
        .ok(&["task", "list", "--project", &setup.project_id]);
    let listed = tasks["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["task_id"] == task)
        .cloned()
        .unwrap();
    (
        verified["project_sequence"].as_u64().unwrap(),
        json!({"state": listed["state"], "revision": listed["revision"]}),
    )
}

fn server(agent: &Cli, setup: &Setup) -> Mcp {
    Mcp::start(&McpEnv::of(
        &agent.config_dir,
        &agent.data_dir,
        &agent.token,
        &setup.repo,
    ))
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario: the refusals, then the controls.
fn u3_k1_complete_is_refused_when_the_contexts_session_is_not_active() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u3 k1",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let session = begun["session_id"].as_str().unwrap().to_owned();
    let recorded = begun["session_revision"].as_u64().unwrap();
    agent.test("tests/a.sh");

    step("the operator ends the Session elsewhere; the agent's context still names it");
    setup.operator.ok(&[
        "session",
        "end",
        &session,
        "--revision",
        &recorded.to_string(),
        "--reason",
        "ended elsewhere",
    ]);
    let shown = agent.run(&["context", "show"], true);
    assert!(shown.stdout.contains(&session), "{}", shown.stdout);
    let before = observed(&setup, &task);
    assert_eq!(before.1["state"], "in_progress", "{}", before.1);

    step("the MCP complete tool: refused with a fixed text, nothing sent");
    let mut mcp = server(agent, &setup);
    for arguments in [
        json!({}),
        json!({"session": 1}),
        json!({"evidence": ["r1"]}),
    ] {
        let refused = mcp.call("complete", &arguments);
        assert!(refused.is_error, "{}", refused.line);
        assert_eq!(
            refused.failure(),
            Some("session_not_active"),
            "{}",
            refused.line
        );
        assert_eq!(refused.text, NOT_ACTIVE, "{}", refused.line);
        assert_eq!(refused.structured["state"], "ended", "{}", refused.line);
        assert_eq!(refused.structured["task_id"], task.as_str());
        assert_eq!(refused.structured["session_id"], session.as_str());
    }
    mcp.finish();
    // `transitionTask` would complete the Task (U2 S5): nothing was sent.
    assert_eq!(observed(&setup, &task), before);

    step("the CLI's task complete: refused with a fixed text, nothing sent");
    for words in [
        &["task", "complete"][..],
        &["task", "complete", "--evidence", "r1"][..],
    ] {
        let refused = agent.run(words, false);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert!(refused.stdout.is_empty(), "{}", refused.stdout);
        assert_eq!(
            refused.stderr,
            format!(
                "memprove: error: the Session {session} of the current context is ended, \
                 not active; task complete was not sent. Completing a Task needs an active \
                 Session: run `memprove begin`, then `memprove run`, then \
                 `memprove task complete`\n"
            ),
        );
    }
    assert_eq!(observed(&setup, &task), before);

    step("a context that names no Session: refused before anything is sent");
    agent.ok(&["begin", "--task", &task]);
    agent.ok(&["end", "--reason", "ended here"]);
    let before = observed(&setup, &task);
    let refused = agent.run(&["task", "complete", "--evidence", "r1"], false);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .starts_with("memprove: error: task complete needs the active Session"),
        "{}",
        refused.stderr
    );
    assert_eq!(observed(&setup, &task), before);

    step("the control: an active Session completes, through the CLI and the tool");
    agent.ok(&["begin", "--task", &task]);
    agent.test("tests/a.sh");
    let completed = agent.ok(&["task", "complete"]);
    assert_eq!(completed["state"], "completed", "{completed}");
    agent.ok(&["end", "--reason", "done"]);
    let second = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u3 k1 tool",
    ]);
    agent.test("tests/a.sh");
    let mut mcp = server(agent, &setup);
    let completed = mcp.ok("complete", &json!({}));
    assert_eq!(completed.structured["state"], "completed");
    assert_eq!(completed.structured["task_id"], second["task_id"]);
    mcp.finish();
}

/// U4 stage Y-K, item Y1 (N-3, original #86; ADR 0014, "Amendment (U4,
/// Y1)"): the Session check comes before the Evidence is resolved, so the
/// refusal of K1 wins over "no run to cite" and the like, in the CLI and in
/// the tool; and a Session whose state cannot be read (a Problem, a daemon
/// that does not answer) is refused too, with nothing sent.
#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario: ended, active, Problem, no daemon.
fn u4_y1_complete_reads_the_session_before_the_evidence_and_refuses_when_unread() {
    let mut setup = Setup::new(&["a"]);
    let agent = setup.agent.clone();
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u4 y1",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let session = begun["session_id"].as_str().unwrap().to_owned();
    let recorded = begun["session_revision"].as_u64().unwrap();
    let before = observed(&setup, &task);

    step("an active Session with no run: the Evidence mistakes are the old ones");
    let usage = agent.run(&["task", "complete"], false);
    assert_eq!(usage.code, Some(2), "{}", usage.stderr);
    assert!(!usage.stderr.contains("not active"), "{}", usage.stderr);
    let usage = agent.run(&["task", "complete", "--evidence", "r1"], false);
    assert_eq!(usage.code, Some(2), "{}", usage.stderr);
    assert!(!usage.stderr.contains("not active"), "{}", usage.stderr);
    let mut mcp = server(&agent, &setup);
    let none = mcp.call("complete", &json!({}));
    assert_eq!(none.failure(), Some("no_run_to_cite"), "{}", none.line);
    let named = mcp.call("complete", &json!({"evidence": ["r1"]}));
    assert_ne!(
        named.failure(),
        Some("session_not_active"),
        "{}",
        named.line
    );
    assert!(named.is_error, "{}", named.line);
    mcp.finish();
    assert_eq!(observed(&setup, &task), before);

    step("the Session read is refused (a token without mcp-read): nothing is sent");
    let created = setup.operator.run(
        &[
            "token",
            "create",
            "--label",
            "write only",
            "--scope",
            "mcp-write",
        ],
        true,
    );
    assert_eq!(created.code, Some(0), "{}", created.stderr);
    let created: Value = serde_json::from_str(created.stdout.trim()).unwrap();
    let write_only = setup.root_path.join("write-only.token");
    std::fs::write(&write_only, created["secret"].as_str().unwrap()).unwrap();
    std::fs::set_permissions(&write_only, std::fs::Permissions::from_mode(0o600)).unwrap();
    let blind = Cli {
        token: write_only,
        label: "agent/mcp-write",
        ..agent.clone()
    };
    for words in [
        &["task", "complete"][..],
        &["task", "complete", "--evidence", "r1"][..],
    ] {
        let refused = blind.run(words, false);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert!(refused.stdout.is_empty(), "{}", refused.stdout);
        assert_eq!(
            refused.stderr,
            "memprove: error: listSessions was refused; task complete was not sent\n"
        );
    }
    let mut mcp = server(&blind, &setup);
    for arguments in [json!({}), json!({"evidence": ["r1"]})] {
        let refused = mcp.call("complete", &arguments);
        assert!(refused.is_error, "{}", refused.line);
        assert_eq!(refused.failure(), Some("problem"), "{}", refused.line);
        assert_eq!(refused.structured["operation"], "listSessions");
        assert_eq!(refused.structured["code"], "policy_denied");
    }
    mcp.finish();
    assert_eq!(observed(&setup, &task), before);

    step("the daemon does not answer: refused, nothing sent");
    setup.stop();
    let down = agent.run(&["task", "complete"], false);
    assert_eq!(down.code, Some(3), "{}", down.stderr);
    assert!(down.stderr.contains("task complete was not sent"));
    let mut mcp = server(&agent, &setup);
    let down = mcp.call("complete", &json!({}));
    assert!(down.is_error, "{}", down.line);
    assert_eq!(down.failure(), Some("daemon_not_running"), "{}", down.line);
    mcp.finish();
    setup.start();
    assert_eq!(observed(&setup, &task), before);

    step("an ended Session with no run: the refusal of K1, not the Evidence's");
    setup.operator.ok(&[
        "session",
        "end",
        &session,
        "--revision",
        &recorded.to_string(),
        "--reason",
        "ended elsewhere",
    ]);
    let before = observed(&setup, &task);
    let mut mcp = server(&agent, &setup);
    for arguments in [
        json!({}),
        json!({"evidence": ["r1"]}),
        json!({"evidence": ["last"]}),
    ] {
        let refused = mcp.call("complete", &arguments);
        assert_eq!(
            refused.failure(),
            Some("session_not_active"),
            "{}",
            refused.line
        );
        assert_eq!(refused.text, NOT_ACTIVE, "{}", refused.line);
    }
    mcp.finish();
    for words in [
        &["task", "complete"][..],
        &["task", "complete", "--evidence", "r1"][..],
        &["task", "complete", "--evidence", "last"][..],
    ] {
        let refused = agent.run(words, false);
        assert_eq!(refused.code, Some(1), "{words:?}: {}", refused.stderr);
        assert!(refused.stdout.is_empty(), "{}", refused.stdout);
        assert_eq!(
            refused.stderr,
            format!(
                "memprove: error: the Session {session} of the current context is ended, \
                 not active; task complete was not sent. Completing a Task needs an active \
                 Session: run `memprove begin`, then `memprove run`, then \
                 `memprove task complete`\n"
            ),
        );
    }
    assert_eq!(observed(&setup, &task), before);
}
