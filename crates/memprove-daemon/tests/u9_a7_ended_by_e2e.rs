//! U9 A7 (Owner O3; ADR 0010, "Amendment (U9)"; ADR 0013, "Amendment
//! (U9)"; ADR 0014, "Amendment (U9, S-A)") through the real binaries: who
//! ended a Session as abandoned.
//!
//! - `endSession` takes the optional member `ended_by` (`{session_id?,
//!   agent?}`) only with `outcome: abandoned`; with it the Event is
//!   `session.crashed@2` and the three `sessions` columns of migration 0009
//!   hold the ender's Session, agent name, and role. Without it the Event
//!   is `session.crashed@1` with the bytes of before, and the columns are
//!   NULL.
//! - `ended_by` without `abandoned`, a `session_id` that names no Session
//!   of the Project or the Session being ended, an `agent` outside the U8
//!   rules, and an `ended_by` that names neither are `400
//!   invalid_request`, and nothing is written.
//! - A changed `sessions.ended_by_agent_name` makes `verifyLedger` name the
//!   Session, and the Task Capsule list it as a damaged record.
//! - The CLI fills `ended_by` itself on `end --abandoned` and `session end
//!   <ID> --abandoned`: the Session its own context names when that is not
//!   the one it ends, and the agent it declares (environment, else the
//!   context file of that other Session); the Capsule's `sessions[]` and
//!   the `Session ` lines of `resume --pack` and `resume --text` show the
//!   ender after the A4 agent part, and a client that declares nothing
//!   leaves the line and the Capsule entry as they were.
//! - The MCP `end` tool with `abandoned` fills it the same way, its
//!   `agent_name`, `agent_role`, and `parent_session` inputs (whole, as
//!   `begin`'s) in place of the environment and the context's agent.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use std::{
    fmt::Write as _,
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use p4b_support::{Cli, Run, Setup, cli_binary, isolated, step, text};
use p4g_support::{Mcp, McpEnv};
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
            "Content-Type: application/json\r\nIdempotency-Key: u9-a7-idempotency-key-{key:04}\r\n\
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

    /// `createSession` continuing the Task; its `session_id`.
    fn create(&self) -> String {
        let created = http(
            &self.socket,
            "POST",
            "/v1/sessions",
            &self.token,
            Some(&json!({
                "schema_version": 1,
                "workspace_id": self.workspace_id,
                "agent_id": AGENT_ID,
                "task_id": self.task_id,
            })),
        );
        assert_eq!(created.status, 201, "{}", created.body);
        created.body["session_id"].as_str().unwrap().to_owned()
    }

    /// `endSession` of `session_id` at its listed revision, with the
    /// members of `extra`.
    fn end(&self, session_id: &str, extra: &Value) -> Http {
        let mut body = json!({
            "schema_version": 1,
            "expected_revision": self.revision(session_id),
            "reason": "taken over",
        });
        for (name, value) in extra.as_object().unwrap() {
            body[name] = value.clone();
        }
        http(
            &self.socket,
            "POST",
            &format!("/v1/sessions/{session_id}/end"),
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

    fn listed(&self, session_id: &str) -> Value {
        self.sessions()
            .into_iter()
            .find(|session| session["session_id"] == session_id)
            .unwrap()
    }

    fn revision(&self, session_id: &str) -> u64 {
        self.listed(session_id)["revision"].as_u64().unwrap()
    }
}

fn ledger_sequence(setup: &Setup) -> u64 {
    let verified = setup
        .agent
        .ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    verified["project_sequence"].as_u64().unwrap()
}

/// `(payload_schema, payload)` of the Session's `session.crashed` Event.
fn crashed(setup: &Setup, session_id: &str) -> (String, Value) {
    let row = setup.query(&format!(
        "SELECT payload_schema || ' ' || payload FROM events \
         WHERE event_type = 'session.crashed' AND session_id = '{session_id}'"
    ));
    let (schema, payload) = row.split_once(' ').unwrap();
    (schema.to_owned(), serde_json::from_str(payload).unwrap())
}

fn columns(setup: &Setup, session_id: &str) -> String {
    setup.query(&format!(
        "SELECT quote(ended_by_session_id) || ',' || quote(ended_by_agent_name) || ',' || \
         quote(ended_by_agent_role) FROM sessions WHERE session_id = '{session_id}'"
    ))
}

fn begin(cli: &Cli, project_id: &str, words: &[&str]) -> Value {
    cli.ok(&[&["begin", "--project", project_id][..], words].concat())
}

/// The Capsule's `sessions[]` item of `session_id`.
fn capsule_session(capsule: &Value, session_id: &str) -> Value {
    capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == session_id)
        .unwrap()
        .clone()
}

#[test]
fn u9_a7_end_session_records_an_optional_ended_by() {
    let setup = Setup::new(&["a"]);
    let begun = begin(&setup.agent, &setup.project_id, &["--new-task", "u9 a7"]);
    let wire = Wire::of(&setup, &begun);
    let taker_session = begun["session_id"].as_str().unwrap().to_owned();

    step("abandoned without ended_by: session.crashed@1 as before");
    let plain = wire.create();
    let ended = wire.end(&plain, &json!({"outcome": "abandoned"}));
    assert_eq!(ended.status, 200, "{}", ended.body);
    assert!(ended.body.get("ended_by").is_none(), "{}", ended.body);
    let (schema, payload) = crashed(&setup, &plain);
    assert_eq!(schema, "session.crashed@1");
    assert_eq!(
        payload,
        json!({"outcome": "abandoned", "reason": "taken over"})
    );
    assert_eq!(columns(&setup, &plain), "NULL,NULL,NULL");

    step("with the ender's Session and agent (with a parent)");
    let both = wire.create();
    let agent = json!({"name": "taker", "role": "sub_agent", "parent_session_id": taker_session});
    let ended = wire.end(
        &both,
        &json!({"outcome": "abandoned", "ended_by": {"session_id": taker_session, "agent": agent}}),
    );
    assert_eq!(ended.status, 200, "{}", ended.body);
    // The row keeps the agent's name and role (migration 0009); the wire
    // Session shows those.
    let shown =
        json!({"session_id": taker_session, "agent": {"name": "taker", "role": "sub_agent"}});
    assert_eq!(ended.body["ended_by"], shown, "{}", ended.body);
    let (schema, payload) = crashed(&setup, &both);
    assert_eq!(schema, "session.crashed@2");
    assert_eq!(
        payload,
        json!({
            "ended_by": {"agent": agent, "session_id": taker_session},
            "outcome": "abandoned",
            "reason": "taken over",
        })
    );
    assert_eq!(
        columns(&setup, &both),
        format!("'{taker_session}','taker','sub_agent'")
    );
    assert_eq!(wire.listed(&both)["ended_by"], shown);

    step("with an agent only: no session_id member");
    let named = wire.create();
    let ended = wire.end(
        &named,
        &json!({"outcome": "abandoned", "ended_by": {"agent": {"name": "x", "role": "human"}}}),
    );
    assert_eq!(ended.status, 200, "{}", ended.body);
    assert_eq!(
        ended.body["ended_by"],
        json!({"agent": {"name": "x", "role": "human"}})
    );
    let (schema, payload) = crashed(&setup, &named);
    assert_eq!(schema, "session.crashed@2");
    assert_eq!(
        payload["ended_by"],
        json!({"agent": {"name": "x", "role": "human", "parent_session_id": null}})
    );
    assert_eq!(columns(&setup, &named), "NULL,'x','human'");

    step("with a Session only: no agent member");
    let by_session = wire.create();
    let ended = wire.end(
        &by_session,
        &json!({"outcome": "abandoned", "ended_by": {"session_id": taker_session}}),
    );
    assert_eq!(ended.status, 200, "{}", ended.body);
    assert_eq!(ended.body["ended_by"], json!({"session_id": taker_session}));
    assert_eq!(
        crashed(&setup, &by_session).1["ended_by"],
        json!({"session_id": taker_session})
    );
    assert_eq!(
        columns(&setup, &by_session),
        format!("'{taker_session}',NULL,NULL")
    );

    step("the Capsule shows ended_by from the Event, and nothing for the others");
    let capsule = setup.agent.capsule();
    assert_ne!(capsule["status"], "tampered", "{capsule}");
    assert!(capsule_session(&capsule, &plain).get("ended_by").is_none());
    assert!(
        capsule_session(&capsule, &taker_session)
            .get("ended_by")
            .is_none()
    );
    assert_eq!(
        capsule_session(&capsule, &both)["ended_by"],
        json!({"session_id": taker_session, "agent": agent})
    );
    assert_eq!(
        capsule_session(&capsule, &named)["ended_by"],
        json!({"agent": {"name": "x", "role": "human", "parent_session_id": null}})
    );
    assert_eq!(
        capsule_session(&capsule, &by_session)["ended_by"],
        json!({"session_id": taker_session})
    );
    // The `@1` Event and the `@2` Events verify alike.
    ledger_sequence(&setup);
}

#[test]
fn u9_a7_an_invalid_ended_by_is_refused_and_writes_nothing() {
    let setup = Setup::new(&["a"]);
    let begun = begin(
        &setup.agent,
        &setup.project_id,
        &["--new-task", "u9 a7 refused"],
    );
    let wire = Wire::of(&setup, &begun);
    let taker_session = begun["session_id"].as_str().unwrap().to_owned();
    let target = wire.create();
    let before = ledger_sequence(&setup);
    let abandoned = |ended_by: Value| json!({"outcome": "abandoned", "ended_by": ended_by});
    let cases = [
        // Only with `abandoned`.
        (
            json!({"ended_by": {"session_id": taker_session}}),
            "$.ended_by",
        ),
        (
            json!({"outcome": "completed", "ended_by": {"session_id": taker_session}}),
            "$.ended_by",
        ),
        // A Session of the Project, not the one being ended.
        (
            abandoned(json!({"session_id": UNKNOWN_ID})),
            "$.ended_by.session_id",
        ),
        (
            abandoned(json!({"session_id": target})),
            "$.ended_by.session_id",
        ),
        (
            abandoned(json!({"session_id": "not-a-uuid"})),
            "$.ended_by.session_id",
        ),
        // The agent rules of U8 O4.
        (
            abandoned(json!({"agent": {"name": "a b", "role": "human"}})),
            "$.ended_by.agent.name",
        ),
        (
            abandoned(json!({"agent": {"name": "x".repeat(65), "role": "human"}})),
            "$.ended_by.agent.name",
        ),
        (
            abandoned(json!({"agent": {"name": "a", "role": "admin"}})),
            "$.ended_by.agent.role",
        ),
        (
            abandoned(json!({"agent": {"name": "a"}})),
            "$.ended_by.agent",
        ),
        (
            abandoned(
                json!({"agent": {"name": "a", "role": "human", "parent_session_id": UNKNOWN_ID}}),
            ),
            "$.ended_by.agent.parent_session_id",
        ),
        // At least one of the two, and nothing else.
        (abandoned(json!({})), "$.ended_by"),
        (abandoned(json!({"other": 1})), "$.ended_by"),
        (abandoned(json!("x")), "$.ended_by"),
    ];
    for (extra, at) in cases {
        step(&format!("refused: {extra}"));
        let refused = wire.end(&target, &extra);
        assert_eq!(refused.status, 400, "{}", refused.body);
        assert_eq!(refused.body["code"], "invalid_request", "{}", refused.body);
        let detail = refused.body["detail"].as_str().unwrap();
        assert!(detail.starts_with(at), "{detail} does not name {at}");
    }
    assert_eq!(ledger_sequence(&setup), before);
    assert_eq!(wire.listed(&target)["state"], "active");
    assert_eq!(columns(&setup, &target), "NULL,NULL,NULL");
    // The Session still ends, with a valid ended_by.
    let ended = wire.end(
        &target,
        &abandoned(json!({"session_id": taker_session, "agent": {"name": "a", "role": "human"}})),
    );
    assert_eq!(ended.status, 200, "{}", ended.body);
}

#[test]
fn u9_a7_a_changed_ended_by_column_is_found_by_both_integrity_checks() {
    let mut setup = Setup::new(&["a"]);
    let begun = begin(
        &setup.agent,
        &setup.project_id,
        &["--new-task", "u9 a7 tampered"],
    );
    let wire = Wire::of(&setup, &begun);
    let target = wire.create();
    let ended = wire.end(
        &target,
        &json!({"outcome": "abandoned", "ended_by": {"agent": {"name": "w-sa", "role": "sub_agent"}}}),
    );
    assert_eq!(ended.status, 200, "{}", ended.body);
    let event_id = setup.query(&format!(
        "SELECT event_id FROM events \
         WHERE event_type = 'session.crashed' AND session_id = '{target}'"
    ));
    let sequence = setup.query(&format!(
        "SELECT project_sequence FROM events WHERE event_id = '{event_id}'"
    ));
    ledger_sequence(&setup);

    step("ended_by_agent_name is changed past memproved");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE sessions SET ended_by_agent_name = 'forged' WHERE session_id = '{target}';"
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
    assert!(reason.contains(&target), "{reason}");
    assert!(reason.contains("ended_by_agent_name"), "{reason}");

    step("the Task Capsule lists the Session as a damaged record");
    let capsule = setup.agent.capsule();
    assert_eq!(capsule["status"], "tampered", "{capsule}");
    assert!(
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .contains(&json!({
                "record_type": "session",
                "record_id": target,
                "reason": "object_digest_mismatch",
            })),
        "{}",
        capsule["damaged_records"]
    );

    step("an ended_by on a Session whose end the ledger does not record");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE sessions SET ended_by_agent_name = 'w-sa' WHERE session_id = '{target}';"
    ));
    let open = begun["session_id"].as_str().unwrap().to_owned();
    setup.tamper(&format!(
        "UPDATE sessions SET ended_by_session_id = '{target}' WHERE session_id = '{open}';"
    ));
    setup.start();
    let run = setup
        .agent
        .run(&["ledger", "verify", "--project", &setup.project_id], false);
    assert_eq!(run.code, Some(5), "{}{}", run.stdout, run.stderr);
    let verification: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    let reason = verification["first_failure"]["reason"].as_str().unwrap();
    assert!(
        reason.contains(&open) && reason.contains("ended_by_session_id"),
        "{reason}"
    );
    let capsule = setup.agent.capsule();
    assert!(
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .contains(&json!({
                "record_type": "session",
                "record_id": open,
                "reason": "object_digest_mismatch",
            })),
        "{}",
        capsule["damaged_records"]
    );
}

/// `memprove <words>` as `cli` runs it, with `env` set.
fn run_with(cli: &Cli, env: &[(&str, &str)], words: &[&str]) -> Run {
    let output = isolated(cli_binary())
        .current_dir(&cli.cwd)
        .arg("--data-dir")
        .arg(&cli.data_dir)
        .arg("--token-file")
        .arg(&cli.token)
        .args(words)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .envs(env.iter().copied())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "[{}] $ {env:?} ct {}\n{}{}[exit {:?}]",
        cli.label,
        words.join(" "),
        run.stdout,
        run.stderr,
        run.code
    );
    run
}

fn session_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("Session "))
        .map(str::to_owned)
        .collect()
}

/// `session end <ID> --abandoned` by `cli` with `env`, at the Session's
/// listed revision.
fn end_other(cli: &Cli, env: &[(&str, &str)], workspace_id: &str, session_id: &str) {
    let page = cli.ok(&["session", "list", "--workspace", workspace_id]);
    let revision = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["session_id"] == session_id)
        .unwrap()["revision"]
        .to_string();
    let run = run_with(
        cli,
        env,
        &[
            "session",
            "end",
            session_id,
            "--revision",
            &revision,
            "--abandoned",
            "--reason",
            "taken over",
        ],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
}

#[test]
fn u9_a7_end_abandoned_fills_ended_by_and_the_session_lines_show_it() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    let project = setup.project_id.as_str();
    step("Session 1: the orchestrator's Session, open, in the directory's context");
    let begun = begin(
        lead,
        project,
        &[
            "--new-task",
            "u9 a7 lines",
            "--agent-name",
            "lead",
            "--agent-role",
            "orchestrator",
        ],
    );
    let first = begun["session_id"].as_str().unwrap().to_owned();
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let workspace = begun["workspace_id"].as_str().unwrap().to_owned();
    step("Session 2–4: three sub-agents' Sessions, left open");
    let [second, third, fourth] = workers(&setup, &task, &first);

    step("another Task: X (no agent, open) and Y (declared, ends itself)");
    let other = lead.elsewhere(setup.root_path.join("config-x"), "x");
    let other_task = begin(&other, project, &["--new-task", "u9 a7 other"]);
    let x = other_task["session_id"].as_str().unwrap().to_owned();
    let other_task_id = other_task["task_id"].as_str().unwrap().to_owned();
    let y_cli = lead.elsewhere(setup.root_path.join("config-y"), "y");
    let y = begin(
        &y_cli,
        project,
        &[
            "--task",
            &other_task_id,
            "--agent-name",
            "c2",
            "--agent-role",
            "human",
        ],
    )["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    step("the orchestrator ends Session 2: its own Session and its context's agent");
    end_other(lead, &[], &workspace, &second);
    step("a client with no context, declared by its environment, ends Session 3");
    let taker = lead.elsewhere(setup.root_path.join("config-taker"), "taker");
    end_other(
        &taker,
        &[
            ("MEMPROVE_AGENT_NAME", "taker"),
            ("MEMPROVE_AGENT_ROLE", "sub_agent"),
        ],
        &workspace,
        &third,
    );
    step("X, whose Session this Task does not list, ends Session 4");
    end_other(&other, &[], &workspace, &fourth);
    step("Y ends its own Session with end --abandoned: no ended_by");
    let run = run_with(&y_cli, &[], &["end", "--abandoned", "--reason", "gave up"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);

    step("the Capsule's sessions[]");
    check_capsule(lead, [&first, &second, &third, &fourth], &x);
    let y_capsule = other.capsule();
    let y_entry = capsule_session(&y_capsule, &y);
    assert_eq!(y_entry["end_state"], "crashed");
    assert!(y_entry.get("ended_by").is_none(), "{y_entry}");

    step("resume --pack and resume --text name the ender after the agent part");
    check_lines(lead, [&second, &third, &fourth], &x);

    step("a Session ended without ended_by keeps its line: only the A4 part");
    let y_pack = other.bytes(&["resume", "--pack"]);
    let y_line = session_lines(&y_pack)
        .into_iter()
        .find(|line| line.contains(&y))
        .unwrap();
    assert!(
        y_line.starts_with(&format!("Session 2 {y} ended abandoned (started "))
            && y_line.ends_with("); agent「c2」(human)"),
        "{y_line}"
    );
    ledger_sequence(&setup);
}

#[test]
fn u9_a7_mcp_end_fills_ended_by_from_its_inputs_or_its_context() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    let project = setup.project_id.as_str();
    let begun = begin(
        lead,
        project,
        &[
            "--new-task",
            "u9 a7 mcp",
            "--agent-name",
            "lead",
            "--agent-role",
            "orchestrator",
        ],
    );
    let first = begun["session_id"].as_str().unwrap().to_owned();
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let open = |config: &str, label: &'static str| {
        let cli = lead.elsewhere(setup.root_path.join(config), label);
        begin(&cli, project, &["--task", task.as_str()])["session_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let second = open("config-2", "two");
    let third = open("config-3", "three");
    let mut mcp = Mcp::start(&McpEnv::of(
        &lead.config_dir,
        &lead.data_dir,
        &lead.token,
        &setup.repo,
    ));

    step("half an agent is invalid_arguments, and nothing is sent");
    let refused = mcp.call(
        "end",
        &json!({"session": 2, "abandoned": true, "agent_name": "taker"}),
    );
    assert_eq!(
        refused.failure(),
        Some("invalid_arguments"),
        "{}",
        refused.line
    );
    assert_eq!(refused.structured["parameter"], "agent_role");

    step("Session 2: the inputs name the agent; the server's context its Session");
    let ended = mcp.call(
        "end",
        &json!({
            "session": 2,
            "abandoned": true,
            "reason": "taken over",
            "agent_name": "mcp-taker",
            "agent_role": "sub_agent",
            "parent_session": first,
        }),
    );
    assert!(!ended.is_error, "{}", ended.line);
    step("Session 3: no input, so the agent of the server's context");
    let ended = mcp.call("end", &json!({"session": 3, "abandoned": true}));
    assert!(!ended.is_error, "{}", ended.line);
    mcp.finish();

    let capsule = lead.capsule();
    assert_eq!(
        capsule_session(&capsule, &second)["ended_by"],
        json!({
            "session_id": first,
            "agent": {"name": "mcp-taker", "role": "sub_agent", "parent_session_id": first},
        })
    );
    assert_eq!(
        capsule_session(&capsule, &third)["ended_by"],
        json!({
            "session_id": first,
            "agent": {"name": "lead", "role": "orchestrator", "parent_session_id": null},
        })
    );
    ledger_sequence(&setup);
}

/// The `Session 2`–`Session 4` lines of `lead`'s Pack and `resume --text`: each
/// abandoned Session's A4 part, then its ender.
fn check_lines(lead: &Cli, [second, third, fourth]: [&str; 3], x: &str) {
    let pack = lead.bytes(&["resume", "--pack"]);
    let lines = session_lines(&pack);
    assert_eq!(lines.len(), 4, "{pack}");
    let line_of = |number: u64, session: &str, rest: &str| {
        (
            format!("Session {number} {session} ended abandoned (started "),
            format!("){rest}"),
        )
    };
    let expected = [
        line_of(
            2,
            second,
            "; agent「child」(sub_agent, dispatched by Session 1); ended by Session 1",
        ),
        line_of(
            3,
            third,
            "; agent「w3」(human); ended by agent「taker」(sub_agent)",
        ),
        line_of(
            4,
            fourth,
            &format!(
                "; agent「w4」(sub_agent, dispatched by Session 1); ended by unlisted Session {x}"
            ),
        ),
    ];
    for (line, (head, tail)) in lines[1..].iter().zip(&expected) {
        assert!(
            line.starts_with(head.as_str()) && line.ends_with(tail.as_str()),
            "{line}"
        );
    }
    let text = lead.bytes(&["resume", "--text"]);
    let lines = session_lines(&text);
    assert_eq!(lines.len(), 4, "{text}");
    for (line, (head, tail)) in lines[1..].iter().zip(&expected) {
        let head = head.replace(
            "ended abandoned (started ",
            "ended abandoned (session.crashed; started ",
        );
        assert!(
            line.starts_with(&head) && line.ends_with(tail.as_str()),
            "{line}"
        );
    }
}

/// The `ended_by` of `lead`'s Capsule's `sessions[]`: none on Session 1, then
/// the orchestrator's (Session and agent), the taker's (agent), and X's
/// (Session).
fn check_capsule(lead: &Cli, [first, second, third, fourth]: [&str; 4], x: &str) {
    let capsule = lead.capsule();
    assert!(capsule_session(&capsule, first).get("ended_by").is_none());
    assert_eq!(
        capsule_session(&capsule, second)["ended_by"],
        json!({
            "session_id": first,
            "agent": {"name": "lead", "role": "orchestrator", "parent_session_id": null},
        })
    );
    assert_eq!(
        capsule_session(&capsule, third)["ended_by"],
        json!({"agent": {"name": "taker", "role": "sub_agent", "parent_session_id": null}})
    );
    assert_eq!(
        capsule_session(&capsule, fourth)["ended_by"],
        json!({"session_id": x})
    );
}

/// Session 2–4 of the Task: Sessions of three sub-agents, each in its own
/// context, left open; the first and the last dispatched by `first`.
fn workers(setup: &Setup, task: &str, first: &str) -> [String; 3] {
    let worker = |config: &str, label: &'static str, words: &[&str]| {
        let cli = setup.agent.elsewhere(setup.root_path.join(config), label);
        let begun = begin(
            &cli,
            &setup.project_id,
            &[&["--task", task][..], words].concat(),
        );
        begun["session_id"].as_str().unwrap().to_owned()
    };
    let second = worker(
        "config-child",
        "child",
        &[
            "--agent-name",
            "child",
            "--agent-role",
            "sub_agent",
            "--parent-session",
            first,
        ],
    );
    let third = worker(
        "config-w3",
        "w3",
        &["--agent-name", "w3", "--agent-role", "human"],
    );
    let fourth = worker(
        "config-w4",
        "w4",
        &[
            "--agent-name",
            "w4",
            "--agent-role",
            "sub_agent",
            "--parent-session",
            first,
        ],
    );
    [second, third, fourth]
}
