//! U5 H5 (D1 F4, Owner O7; ADR 0014 E3, "Amendment (U5)"): the `Cannot: `
//! line of citing the latest run says why `invalid_request`, in the words
//! of the daemon's own Problem `detail`, through the real binaries.
//!
//! D1's case: the Task's latest run is a command run (`compileall`), not a
//! test run, so the write is refused with `400 invalid_request`, `verified_fact
//! requires successful executed test Evidence`, and the Pack before the
//! attempt only said `invalid_request`. The sentence after the code is no
//! statement text and the Capsule JSON is the same as before.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use p4b_support::{Setup, step};
use p4h2_support::{high_water, line_with};
use serde_json::{Value, json};

/// The `cite_latest_run` action of the Capsule.
fn cite_latest_run(capsule: &Value) -> &Value {
    capsule["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["action"] == "cite_latest_run")
        .unwrap()
}

/// The `detail` of the Problem `memprove` printed on standard error
/// (`… failed: 400 invalid_request: <detail>`), whatever follows the code.
fn problem_text(stderr: &str) -> String {
    stderr
        .lines()
        .find_map(|line| {
            let (_, rest) = line.split_once(" failed: ")?;
            Some(rest.to_owned())
        })
        .unwrap_or_else(|| panic!("no Problem in {stderr}"))
}

/// The `createCheckpoint` request the action describes: a `verified_fact`
/// citing the run the Capsule names, on its Snapshot, sent as a file.
fn cite_latest_request(setup: &Setup, begun: &Value, args: &Value) -> std::path::PathBuf {
    let request = json!({
        "schema_version": 1,
        "workspace_id": args["workspace_id"],
        "objective": "u5 h5",
        "task_id": begun["task_id"],
        "session_id": begun["session_id"],
        "snapshot_id": args["snapshot_id"],
        "verified_completed_work": [{
            "classification": "verified_fact",
            "text": "the check passes",
            "evidence_ids": [args["evidence_id"]],
        }],
        "next_action": {"classification": "recommendation", "text": "go on"},
    });
    let path = setup.root_path.join("cite-latest.json");
    std::fs::write(&path, request.to_string()).unwrap();
    path
}

/// The sentence after `invalid_request` in the Problem `memprove`
/// printed (`… failed: 400 invalid_request: <detail>`).
fn detail_of(stderr: &str) -> String {
    let problem = problem_text(stderr);
    problem
        .strip_prefix("400 invalid_request: ")
        .unwrap_or_else(|| panic!("not a 400 invalid_request: {problem}"))
        .to_owned()
}

/// What the Pack's line must be for `detail`.
fn expected_line(detail: &str) -> String {
    format!(
        "Cannot: record a fact citing the latest run (createCheckpoint; reasons: invalid_request \
         ({detail}); needs mcp-write)"
    )
}

/// The line of an action refused with `invalid_request` and nothing after
/// the code.
fn expected_line_without_sentence() -> &'static str {
    "Cannot: record a fact citing the latest run (createCheckpoint; reasons: invalid_request; needs \
     mcp-write)"
}

/// What the Pack's line says in brackets after `invalid_request`, if it does.
fn sentence_of(line: &str) -> Option<String> {
    let rest = line.split_once("reasons: invalid_request (")?.1;
    Some(rest.split_once("); ")?.0.to_owned())
}

/// U5 Z-F round 1 (S3): the action has no run, so no Snapshot and no
/// Workspace to put in the request; the daemon's `detail` is the first
/// member it finds missing or null, and which that is depends on the
/// request the client builds, which the Capsule does not hold. Every shape
/// a client can build from the action is sent to the real daemon, and the
/// `detail` of each reply is returned.
fn no_run_details(setup: &Setup, begun: &Value, action: &Value) -> [String; 4] {
    let agent = &setup.agent;
    let workspace = &begun["workspace_id"];
    let shape = |workspace: &Value, snapshot: Option<Value>| {
        let mut request = json!({
            "schema_version": 1,
            "workspace_id": workspace,
            "objective": "u5 h5",
            "task_id": begun["task_id"],
            "session_id": begun["session_id"],
            "verified_completed_work": [{
                "classification": "verified_fact",
                "text": "the check passes",
                "evidence_ids": [action["args"]["evidence_id"]],
            }],
            "next_action": {"classification": "recommendation", "text": "go on"},
        });
        match snapshot {
            Some(snapshot) => request["snapshot_id"] = snapshot,
            None if workspace.is_null() => {
                request.as_object_mut().unwrap().remove("workspace_id");
            }
            None => {}
        }
        let path = setup.root_path.join("cite-latest.json");
        std::fs::write(&path, request.to_string()).unwrap();
        let refused = agent.run(&["checkpoint", "create", path.to_str().unwrap()], false);
        assert_eq!(
            refused.code,
            Some(1),
            "{}{}",
            refused.stdout,
            refused.stderr
        );
        detail_of(&refused.stderr)
    };
    [
        // Every member as the action's `args` give it: null.
        shape(&Value::Null, Some(Value::Null)),
        // The Workspace known, the Snapshot absent, then null.
        shape(workspace, None),
        shape(workspace, Some(Value::Null)),
        // Neither member there.
        shape(&Value::Null, None),
    ]
}

#[test]
fn u5_h5_the_pack_says_why_citing_the_latest_run_is_invalid_request() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u5 h5",
    ]);

    step("no run yet: no request shape has one detail, so the Pack gives no sentence");
    let capsule = agent.capsule();
    let action = cite_latest_run(&capsule);
    assert_eq!(action["reasons"], json!(["invalid_request"]), "{action}");
    let pack = agent.bytes(&["resume", "--pack"]);
    let line = line_with(&pack, "Cannot: record a fact citing the latest run");
    let details = no_run_details(&setup, &begun, action);
    let distinct = details.iter().collect::<std::collections::BTreeSet<_>>();
    assert!(
        distinct.len() > 1,
        "one detail for every shape: {details:?}"
    );
    let attached = sentence_of(line);
    assert!(
        details.iter().all(|detail| attached
            .as_ref()
            .is_none_or(|s| detail.starts_with(s.as_str()))),
        "the Pack says {attached:?} for {details:?}: {line}"
    );
    assert_eq!(attached, None, "{line}");
    assert_eq!(line, expected_line_without_sentence());

    step("D1's case: the latest run is a command run, not a test run");
    agent.command("tests/a.sh");
    let capsule = agent.capsule();
    let action = cite_latest_run(&capsule);
    assert_eq!(action["reasons"], json!(["invalid_request"]), "{action}");
    assert_eq!(action["args"]["kind"], "command_result");
    let pack = agent.bytes(&["resume", "--pack"]);
    let line = line_with(&pack, "Cannot: record a fact citing the latest run").to_owned();
    let request = cite_latest_request(&setup, &begun, &action["args"]);
    let before = high_water(&setup);
    let refused = agent.run(&["checkpoint", "create", request.to_str().unwrap()], false);
    assert_eq!(
        refused.code,
        Some(1),
        "{}{}",
        refused.stdout,
        refused.stderr
    );
    let detail = detail_of(&refused.stderr);
    assert_eq!(
        detail,
        "verified_fact requires successful executed test Evidence"
    );
    assert_eq!(line, expected_line(&detail));
    assert_eq!(high_water(&setup), before, "a refusal wrote to the ledger");

    step("the sentence is in the Pack only: the Capsule JSON holds no such text");
    let capsule_text = agent.bytes(&["resume"]);
    assert!(!capsule_text.contains(&detail), "{capsule_text}");
    let hook_and_text = agent.bytes(&["resume", "--text"]);
    assert!(
        !hook_and_text.contains("successful executed test"),
        "{hook_and_text}"
    );
}
