//! U8 A1 (Owner O4; ADR 0010, "Amendment (U8)"; ADR 0014, "Amendment
//! (U8, S-A)") through the real binaries: `createSession` takes an optional
//! `agent` (`{name, role, parent_session_id?}`), writes it into
//! `session.started@3` and the three new `sessions` columns of migration
//! 0008, and the integrity checks compare the columns with the payload.
//!
//! - Without `agent` nothing changes: the `Session` has no `agent` member,
//!   and `session.started@1` carries `{agent_id, task_id}` as before.
//! - A `name` outside `^[A-Za-z0-9_-]{1,64}$`, a `role` outside
//!   {orchestrator, `sub_agent`, human}, or a `parent_session_id` that names
//!   no Session of the Project is `400 invalid_request`, and nothing is
//!   written.
//! - A `sessions.agent_name` changed past `memproved` makes `verifyLedger`
//!   name the Session (its `session.started` Event, record type `event`)
//!   and the Task Capsule list it as a damaged `session` record.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use std::{
    fmt::Write as _,
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use p4b_support::{Setup, step};
use serde_json::{Value, json};

/// A lowercase `UUIDv7` that names no Session.
const UNKNOWN_ID: &str = "018f47a0-0000-7000-8000-0000000000aa";
/// A fixed `UUIDv7` agent identity of the raw requests.
const AGENT_ID: &str = "018f47a0-0000-7000-8000-00000000a8e1";

static KEYS: AtomicU32 = AtomicU32::new(0);

struct Http {
    status: u16,
    body: Value,
}

fn http(socket: &Path, method: &str, target: &str, token: &str, body: Option<&Value>) -> Http {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut head = format!(
        "{method} {target} HTTP/1.1\r\nHost: continuity.local\r\nConnection: close\r\n\
         Authorization: Bearer {token}\r\n"
    );
    let body = body.map(Value::to_string);
    if let Some(body) = &body {
        let key = KEYS.fetch_add(1, Ordering::SeqCst);
        write!(
            head,
            "Content-Type: application/json\r\nIdempotency-Key: u8-a1-idempotency-key-{key:04}\r\n\
             Content-Length: {}\r\n",
            body.len()
        )
        .unwrap();
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    if let Some(body) = &body {
        stream.write_all(body.as_bytes()).unwrap();
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    println!("$ {method} {target}\n{head}\n{body}");
    Http {
        status: head[9..12].parse().unwrap(),
        body: serde_json::from_str(body).unwrap_or(Value::Null),
    }
}

struct Wire {
    socket: PathBuf,
    token: String,
    workspace_id: String,
    task_id: String,
}

impl Wire {
    fn of(setup: &Setup, begun: &Value) -> Self {
        Self {
            socket: setup.agent.data_dir.join("memproved.sock"),
            token: fs::read_to_string(&setup.agent.token)
                .unwrap()
                .trim()
                .to_owned(),
            workspace_id: begun["workspace_id"].as_str().unwrap().to_owned(),
            task_id: begun["task_id"].as_str().unwrap().to_owned(),
        }
    }

    /// `createSession` continuing the Task, with `agent` when given.
    fn create(&self, agent: Option<Value>) -> Http {
        let mut body = json!({
            "schema_version": 1,
            "workspace_id": self.workspace_id,
            "agent_id": AGENT_ID,
            "task_id": self.task_id,
        });
        if let Some(agent) = agent {
            body["agent"] = agent;
        }
        http(
            &self.socket,
            "POST",
            "/v1/sessions",
            &self.token,
            Some(&body),
        )
    }

    fn sessions(&self) -> Vec<Value> {
        let target = format!("/v1/sessions?workspace_id={}&limit=100", self.workspace_id);
        let page = http(&self.socket, "GET", &target, &self.token, None);
        assert_eq!(page.status, 200, "{}", page.body);
        page.body["items"].as_array().unwrap().clone()
    }
}

fn session_id(created: &Http) -> String {
    assert_eq!(created.status, 201, "{}", created.body);
    created.body["session_id"].as_str().unwrap().to_owned()
}

fn ledger_sequence(setup: &Setup) -> u64 {
    let verified = setup
        .agent
        .ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    verified["project_sequence"].as_u64().unwrap()
}

/// `(payload_schema, payload)` of the Session's `session.started` Event.
fn started(setup: &Setup, session_id: &str) -> (String, Value) {
    let row = setup.query(&format!(
        "SELECT payload_schema || ' ' || payload FROM events \
         WHERE event_type = 'session.started' AND session_id = '{session_id}'"
    ));
    let (schema, payload) = row.split_once(' ').unwrap();
    (schema.to_owned(), serde_json::from_str(payload).unwrap())
}

fn columns(setup: &Setup, session_id: &str) -> String {
    setup.query(&format!(
        "SELECT quote(agent_name) || ',' || quote(agent_role) || ',' || quote(parent_session_id) \
         FROM sessions WHERE session_id = '{session_id}'"
    ))
}

fn begin(setup: &Setup, title: &str) -> Value {
    setup
        .agent
        .ok(&["begin", "--project", &setup.project_id, "--new-task", title])
}

#[test]
fn u8_a1_create_session_records_an_optional_agent() {
    let setup = Setup::new(&["a"]);
    let begun = begin(&setup, "u8 a1");
    let wire = Wire::of(&setup, &begun);
    let parent = begun["session_id"].as_str().unwrap().to_owned();

    step("without agent: no agent member, session.started@1 as before");
    let plain = wire.create(None);
    let plain_id = session_id(&plain);
    assert!(plain.body.get("agent").is_none(), "{}", plain.body);
    let (schema, payload) = started(&setup, &plain_id);
    assert_eq!(schema, "session.started@1");
    assert_eq!(
        payload,
        json!({"agent_id": AGENT_ID, "task_id": wire.task_id})
    );
    assert_eq!(columns(&setup, &plain_id), "NULL,NULL,NULL");

    step("with a name, a role, and a parent Session");
    let agent = json!({"name": "u8-sa_child-1", "role": "sub_agent", "parent_session_id": parent});
    let child = wire.create(Some(agent.clone()));
    let child_id = session_id(&child);
    assert_eq!(child.body["agent"], agent, "{}", child.body);
    let (schema, payload) = started(&setup, &child_id);
    assert_eq!(schema, "session.started@3");
    assert_eq!(
        payload,
        json!({"agent": agent, "agent_id": AGENT_ID, "task_id": wire.task_id})
    );
    assert_eq!(
        columns(&setup, &child_id),
        format!("'u8-sa_child-1','sub_agent','{parent}'")
    );

    step("with a name and a role only: no parent_session_id member");
    let lead = wire.create(Some(json!({"name": "lead", "role": "orchestrator"})));
    let lead_id = session_id(&lead);
    assert_eq!(
        lead.body["agent"],
        json!({"name": "lead", "role": "orchestrator"})
    );
    let (_, payload) = started(&setup, &lead_id);
    assert_eq!(
        payload["agent"],
        json!({"name": "lead", "role": "orchestrator", "parent_session_id": null})
    );

    step("listSessions shows the same agent members");
    let listed = wire.sessions();
    let of = |id: &str| {
        listed
            .iter()
            .find(|session| session["session_id"] == id)
            .unwrap()
            .clone()
    };
    assert!(of(&plain_id).get("agent").is_none());
    assert!(of(&parent).get("agent").is_none());
    assert_eq!(of(&child_id)["agent"], agent);
    assert_eq!(of(&lead_id)["agent"], lead.body["agent"]);
    ledger_sequence(&setup);
}

#[test]
fn u8_a1_an_invalid_agent_is_refused_and_writes_nothing() {
    let setup = Setup::new(&["a"]);
    let begun = begin(&setup, "u8 a1 refused");
    let wire = Wire::of(&setup, &begun);
    let before = ledger_sequence(&setup);
    let cases = [
        (
            json!({"name": "x".repeat(65), "role": "human"}),
            "$.agent.name",
        ),
        (json!({"name": "", "role": "human"}), "$.agent.name"),
        (json!({"name": "a b", "role": "human"}), "$.agent.name"),
        (json!({"name": "nämé", "role": "human"}), "$.agent.name"),
        (json!({"name": "a", "role": "admin"}), "$.agent.role"),
        (json!({"name": "a"}), "$.agent"),
        (
            json!({"name": "a", "role": "human", "parent_session_id": UNKNOWN_ID}),
            "$.agent.parent_session_id",
        ),
        (
            json!({"name": "a", "role": "human", "parent_session_id": "not-a-uuid"}),
            "$.agent.parent_session_id",
        ),
    ];
    for (agent, at) in cases {
        step(&format!("refused: {agent}"));
        let refused = wire.create(Some(agent));
        assert_eq!(refused.status, 400, "{}", refused.body);
        assert_eq!(refused.body["code"], "invalid_request", "{}", refused.body);
        let detail = refused.body["detail"].as_str().unwrap();
        assert!(detail.starts_with(at), "{detail} does not name {at}");
    }
    assert_eq!(ledger_sequence(&setup), before);
    assert_eq!(wire.sessions().len(), 1);
    // The longest name is accepted.
    let longest = "a".repeat(64);
    let created = wire.create(Some(json!({"name": longest, "role": "human"})));
    session_id(&created);
}

#[test]
fn u8_a1_a_changed_agent_column_is_found_by_both_integrity_checks() {
    let mut setup = Setup::new(&["a"]);
    let begun = begin(&setup, "u8 a1 tampered");
    let wire = Wire::of(&setup, &begun);
    let named = session_id(&wire.create(Some(json!({"name": "w-sa", "role": "sub_agent"}))));
    let event_id = setup.query(&format!(
        "SELECT event_id FROM events \
         WHERE event_type = 'session.started' AND session_id = '{named}'"
    ));
    let sequence = setup.query(&format!(
        "SELECT project_sequence FROM events WHERE event_id = '{event_id}'"
    ));
    ledger_sequence(&setup);

    step("agent_name is changed past memproved");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE sessions SET agent_name = 'forged' WHERE session_id = '{named}';"
    ));
    setup.start();
    let run = setup
        .agent
        .run(&["ledger", "verify", "--project", &setup.project_id], false);
    assert_eq!(run.code, Some(5), "{}{}", run.stdout, run.stderr);
    let verification: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    let failure = &verification["first_failure"];
    assert_eq!(failure["record_type"], "event", "{verification}");
    assert_eq!(failure["record_id"], event_id.as_str(), "{verification}");
    assert_eq!(failure["project_sequence"].to_string(), sequence);
    let reason = failure["reason"].as_str().unwrap();
    assert!(reason.contains(&named), "{reason}");
    assert!(reason.contains("agent_name"), "{reason}");

    step("the Task Capsule lists the Session as a damaged record");
    let capsule = setup.agent.capsule();
    assert_eq!(capsule["status"], "tampered", "{capsule}");
    assert!(
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .contains(&json!({
                "record_type": "session",
                "record_id": named,
                "reason": "object_digest_mismatch",
            })),
        "{}",
        capsule["damaged_records"]
    );
}
