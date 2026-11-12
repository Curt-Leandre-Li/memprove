//! U8 A2 (Owner O4; ADR 0013, "Amendment (U8, S-A)") through the real
//! binaries: the Task Capsule's `sessions[]` carries the optional member
//! `agent` (`{name, role, parent_session_id}`) of each Session that
//! declared one, read from its `session.started@3` Event; a Session that
//! declared none has no such member, so a Capsule of such Sessions is the
//! bytes it was. The response passes the `getTaskCapsule` validator.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use memprove_protocol::{
    phase1::{OperationId, validate_response},
    strict_json::parse_strict,
};
use p4b_support::{Setup, step};
use serde_json::{Value, json};

const AGENT_ID: &str = "018f47a0-0000-7000-8000-00000000a8e2";

fn post(socket: &Path, token: &str, key: &str, body: &Value) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let body = body.to_string();
    let head = format!(
        "POST /v1/sessions HTTP/1.1\r\nHost: continuity.local\r\nConnection: close\r\n\
         Authorization: Bearer {token}\r\nContent-Type: application/json\r\n\
         Idempotency-Key: {key}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    println!("$ POST /v1/sessions\n{head}\n{body}");
    assert!(head.starts_with("HTTP/1.1 201"), "{head}\n{body}");
    serde_json::from_str(body).unwrap()
}

fn session<'a>(capsule: &'a Value, id: &str) -> &'a Value {
    capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == id)
        .unwrap_or_else(|| panic!("no Session {id}"))
}

#[test]
fn u8_a2_the_capsule_shows_the_agent_of_each_session_that_declared_one() {
    let setup = Setup::new(&["a"]);
    let begun = setup.agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u8 a2",
    ]);
    let socket = setup.agent.data_dir.join("memproved.sock");
    let token = fs::read_to_string(&setup.agent.token).unwrap();
    let parent = begun["session_id"].as_str().unwrap().to_owned();

    step("a Capsule of Sessions without an agent has no agent member");
    let plain = setup.agent.bytes(&["resume"]);
    let capsule: Value = serde_json::from_str(plain.trim()).unwrap();
    assert!(
        session(&capsule, &parent).get("agent").is_none(),
        "{capsule}"
    );

    step("two named Sessions continue the Task, one dispatched by the first");
    let body = |agent: Value| {
        json!({
            "schema_version": 1,
            "workspace_id": begun["workspace_id"],
            "agent_id": AGENT_ID,
            "task_id": begun["task_id"],
            "agent": agent,
        })
    };
    let lead_agent = json!({"name": "u8-lead", "role": "orchestrator"});
    let lead = post(
        &socket,
        token.trim(),
        "u8-a2-idempotency-key-0001",
        &body(lead_agent),
    );
    let lead_id = lead["session_id"].as_str().unwrap().to_owned();
    let child_agent =
        json!({"name": "u8-child", "role": "sub_agent", "parent_session_id": lead_id});
    let child = post(
        &socket,
        token.trim(),
        "u8-a2-idempotency-key-0002",
        &body(child_agent.clone()),
    );
    let child_id = child["session_id"].as_str().unwrap().to_owned();

    step("the Capsule shows both agents; the unnamed Session is as before");
    let named = setup.agent.bytes(&["resume"]);
    let capsule: Value = serde_json::from_str(named.trim()).unwrap();
    assert!(
        session(&capsule, &parent).get("agent").is_none(),
        "{capsule}"
    );
    assert_eq!(
        session(&capsule, &lead_id)["agent"],
        json!({"name": "u8-lead", "role": "orchestrator", "parent_session_id": null})
    );
    assert_eq!(session(&capsule, &child_id)["agent"], child_agent);
    validate_response(
        OperationId::GetTaskCapsule,
        200,
        "application/json",
        &parse_strict(named.trim()).unwrap(),
    )
    .unwrap();
    // The unnamed Session's entry is the bytes it was.
    let entry = |capsule: &Value| session(capsule, &parent).to_string();
    let before: Value = serde_json::from_str(plain.trim()).unwrap();
    assert_eq!(entry(&capsule), entry(&before));
}
