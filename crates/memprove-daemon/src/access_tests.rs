//! ADR 0012 access and transport checks: the complete authentication matrix
//! over every protected operation (including `getRecoveryPreview`,
//! `getTaskCapsule`, and P3-C's `proposeDecision`), and the explicit
//! request-body limit.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::Arc,
};

use memprove_protocol::{phase1::OperationId, strict_json::parse_strict};
use serde_json::json;

use crate::{
    api::{MAX_REQUEST_BODY_BYTES, WireResponse, get_status, get_version, router},
    catalog::{
        create_session, create_task, end_session, get_project, list_sessions, list_tasks,
        register_project, resolve_workspace, transition_task,
    },
    events::{get_event, ingest_event, list_events},
    recovery::{
        capture_evidence, create_checkpoint, get_checkpoint, get_evidence, get_recovery_preview,
        get_task_capsule, propose_decision,
    },
    test_support::{Harness, field, git_repository, json_request},
    tokens::{TokenRequest, create_client_token, list_client_tokens, revoke_client_token},
};

const AGENT: &str = "018f47a0-0000-7000-8000-00000000a6e1";

/// Which credentials an operation admits, per `x-memprove-access`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// `Authenticated`: any active token.
    Authenticated,
    /// `AnyOf[control]`.
    Control,
    /// `AnyOf[control, mcp-read]`.
    Read,
    /// `AnyOf[control, mcp-write]`.
    Write,
}

impl Rule {
    fn admits(self, scope: &str) -> bool {
        match self {
            Self::Authenticated => true,
            Self::Control => scope == "control",
            Self::Read => matches!(scope, "control" | "mcp-read"),
            Self::Write => matches!(scope, "control" | "mcp-write"),
        }
    }
}

/// Every protected operation, its rule, and its success status.
const PROTECTED: [(OperationId, Rule, u16); 24] = [
    (OperationId::GetStatus, Rule::Control, 200),
    (OperationId::GetVersion, Rule::Authenticated, 200),
    (OperationId::CreateClientToken, Rule::Control, 201),
    (OperationId::ListClientTokens, Rule::Control, 200),
    (OperationId::RevokeClientToken, Rule::Control, 200),
    (OperationId::RegisterProject, Rule::Control, 201),
    (OperationId::GetProject, Rule::Read, 200),
    (OperationId::ResolveWorkspace, Rule::Write, 200),
    (OperationId::CreateSession, Rule::Write, 201),
    (OperationId::ListSessions, Rule::Read, 200),
    (OperationId::EndSession, Rule::Write, 200),
    (OperationId::CreateTask, Rule::Write, 201),
    (OperationId::ListTasks, Rule::Read, 200),
    (OperationId::TransitionTask, Rule::Write, 200),
    (OperationId::IngestEvent, Rule::Write, 202),
    (OperationId::ListEvents, Rule::Control, 200),
    (OperationId::GetEvent, Rule::Control, 200),
    (OperationId::CaptureEvidence, Rule::Write, 201),
    (OperationId::GetEvidence, Rule::Control, 200),
    (OperationId::CreateCheckpoint, Rule::Write, 201),
    (OperationId::GetCheckpoint, Rule::Read, 200),
    (OperationId::GetRecoveryPreview, Rule::Read, 200),
    (OperationId::GetTaskCapsule, Rule::Read, 200),
    (OperationId::ProposeDecision, Rule::Write, 201),
];

struct World {
    harness: Harness,
    control: String,
    _repos: tempfile::TempDir,
    repo: PathBuf,
    project_id: String,
    workspace_id: String,
    session_id: String,
    event_id: String,
    task_id: String,
    agent_session_id: String,
    snapshot_id: String,
    evidence_id: String,
    checkpoint_id: String,
}

impl World {
    fn new() -> Self {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let repos = tempfile::tempdir().unwrap();
        let repo = git_repository(repos.path(), "repo");
        let mut world = Self {
            harness,
            control,
            _repos: repos,
            repo,
            project_id: String::new(),
            workspace_id: String::new(),
            session_id: String::new(),
            event_id: String::new(),
            task_id: String::new(),
            agent_session_id: String::new(),
            snapshot_id: String::new(),
            evidence_id: String::new(),
            checkpoint_id: String::new(),
        };
        let registered = Self::ok(&world.call(OperationId::RegisterProject, None), 201);
        world.project_id = field(&registered, "project_id");
        world.workspace_id = field(&registered, "root_workspace_id");
        world.session_id = world.fresh_session();
        let accepted = Self::ok(&world.call(OperationId::IngestEvent, None), 202);
        world.event_id = field(&accepted, "resource_id");
        world.task_id = world.fresh_task();
        let body = json!({
            "schema_version": 1, "workspace_id": world.workspace_id, "agent_id": AGENT,
            "task_id": world.task_id,
        });
        let created = create_session(
            &world.harness.state,
            &json_request(
                Some(&world.control),
                Some(&world.harness.fresh_key()),
                &body.to_string(),
            ),
        );
        world.agent_session_id = field(&Self::ok(&created, 201), "session_id");
        let captured = Self::ok(&world.call(OperationId::CaptureEvidence, None), 201);
        world.evidence_id = field(&captured, "evidence_id");
        world.snapshot_id = captured
            .get("snapshot")
            .map(|snapshot| field(snapshot, "snapshot_id"))
            .unwrap();
        let checkpoint = Self::ok(&world.call(OperationId::CreateCheckpoint, None), 201);
        world.checkpoint_id = field(&checkpoint, "checkpoint_id");
        world
    }

    fn capture_body(&self) -> String {
        let digest = format!("sha256:{}", "0".repeat(64));
        json!({
            "schema_version": 1,
            "workspace_id": self.workspace_id,
            "task_id": self.task_id,
            "session_id": self.agent_session_id,
            "artifact_type": "test_run",
            "snapshot_id": uuid::Uuid::now_v7().to_string(),
            "metadata": {
                "argv": ["cargo", "test"], "exit_code": 0,
                "started_at": "2026-10-02T10:00:00Z", "finished_at": "2026-10-02T10:00:01Z",
                "stdout_tail": "", "stderr_tail": "",
                "stdout_sha256": digest, "stderr_sha256": digest,
                "collector": { "name": "matrix", "version": "1" }, "test_case_count": 1,
            },
        })
        .to_string()
    }

    fn checkpoint_body(&self) -> String {
        json!({
            "schema_version": 1,
            "workspace_id": self.workspace_id,
            "task_id": self.task_id,
            "session_id": self.agent_session_id,
            "snapshot_id": self.snapshot_id,
            "objective": "matrix",
            "verified_completed_work": [{
                "classification": "verified_fact", "text": "tests pass",
                "evidence_ids": [self.evidence_id],
            }],
            "next_action": { "classification": "unknown", "text": "none" },
        })
        .to_string()
    }

    fn decision_body(&self) -> String {
        json!({
            "schema_version": 1,
            "task_id": self.task_id,
            "session_id": self.agent_session_id,
            "content": "matrix",
            "evidence_ids": [self.evidence_id],
        })
        .to_string()
    }

    fn ok(response: &WireResponse, status: u16) -> memprove_protocol::strict_json::JsonValue {
        assert_eq!(response.status, status, "{}", response.body);
        parse_strict(&response.body).unwrap()
    }

    fn fresh_session(&self) -> String {
        let body = json!({
            "schema_version": 1, "workspace_id": self.workspace_id, "agent_id": AGENT,
        });
        let created = create_session(
            &self.harness.state,
            &json_request(
                Some(&self.control),
                Some(&self.harness.fresh_key()),
                &body.to_string(),
            ),
        );
        field(&Self::ok(&created, 201), "session_id")
    }

    fn fresh_task(&self) -> String {
        let body = json!({ "schema_version": 1, "project_id": self.project_id, "title": "t" });
        let created = create_task(
            &self.harness.state,
            &json_request(
                Some(&self.control),
                Some(&self.harness.fresh_key()),
                &body.to_string(),
            ),
        );
        field(&Self::ok(&created, 201), "task_id")
    }

    fn fresh_token(&self) -> String {
        self.harness.issue(&self.control, "victim", &["mcp-read"]).0
    }

    /// Calls `operation` with `bearer` (the bootstrap control token when
    /// `None`) on fresh targets, so every admitted call can succeed.
    #[allow(clippy::too_many_lines)] // One request per protected operation.
    fn call(&self, operation: OperationId, bearer: Option<&str>) -> WireResponse {
        let bearer = Some(bearer.unwrap_or(&self.control));
        let state = &self.harness.state;
        let key = self.harness.fresh_key();
        let send = |body: &serde_json::Value| body.to_string();
        match operation {
            OperationId::GetStatus => get_status(&state.plane, bearer),
            OperationId::GetVersion => get_version(&state.plane, bearer),
            OperationId::CreateClientToken => {
                let body = send(&json!({
                    "schema_version": 1, "client_label": "matrix", "scopes": ["mcp-read"],
                }));
                create_client_token(state, &json_request(bearer, Some(&key), &body))
            }
            OperationId::ListClientTokens => list_client_tokens(state, bearer, None),
            OperationId::RevokeClientToken => revoke_client_token(
                state,
                &TokenRequest {
                    bearer,
                    idempotency_key: Some(&key),
                    content_type: None,
                    body: Some(b""),
                },
                &self.fresh_token(),
            ),
            OperationId::RegisterProject => {
                let body = send(&json!({ "schema_version": 1, "root": self.repo }));
                register_project(state, &json_request(bearer, Some(&key), &body))
            }
            OperationId::GetProject => get_project(state, bearer, &self.project_id),
            OperationId::ResolveWorkspace => {
                let body = send(&json!({
                    "schema_version": 1, "project_id": self.project_id, "path": self.repo,
                }));
                resolve_workspace(state, &json_request(bearer, None, &body))
            }
            OperationId::CreateSession => {
                let body = send(&json!({
                    "schema_version": 1, "workspace_id": self.workspace_id, "agent_id": AGENT,
                }));
                create_session(state, &json_request(bearer, Some(&key), &body))
            }
            OperationId::ListSessions => list_sessions(
                state,
                bearer,
                Some(&format!("workspace_id={}", self.workspace_id)),
            ),
            OperationId::EndSession => end_session(
                state,
                &json_request(
                    bearer,
                    Some(&key),
                    r#"{"schema_version":1,"expected_revision":1}"#,
                ),
                &self.fresh_session(),
            ),
            OperationId::CreateTask => {
                let body = send(&json!({
                    "schema_version": 1, "project_id": self.project_id, "title": "matrix",
                }));
                create_task(state, &json_request(bearer, Some(&key), &body))
            }
            OperationId::ListTasks => list_tasks(
                state,
                bearer,
                Some(&format!("project_id={}", self.project_id)),
            ),
            OperationId::TransitionTask => transition_task(
                state,
                &json_request(
                    bearer,
                    Some(&key),
                    r#"{"schema_version":1,"expected_revision":1,"target_state":"ready"}"#,
                ),
                &self.fresh_task(),
            ),
            OperationId::IngestEvent => {
                let body = send(&json!({
                    "envelope_version": "1.0",
                    "event_id": uuid::Uuid::now_v7().to_string(),
                    "project_id": self.project_id,
                    "workspace_id": self.workspace_id,
                    "session_id": self.session_id,
                    "event_type": "turn.started",
                    "payload_schema": "turn.started@1",
                    "producer": { "name": "matrix", "version": "1", "sequence": 0 },
                    "privacy_class": "internal",
                    "payload": {},
                }));
                ingest_event(state, &json_request(bearer, Some(&key), &body))
            }
            OperationId::ListEvents => list_events(
                state,
                bearer,
                Some(&format!("project_id={}", self.project_id)),
            ),
            OperationId::GetEvent => get_event(state, bearer, &self.event_id),
            OperationId::CaptureEvidence => capture_evidence(
                state,
                &json_request(bearer, Some(&key), &self.capture_body()),
            ),
            OperationId::GetEvidence => get_evidence(state, bearer, &self.evidence_id),
            OperationId::CreateCheckpoint => create_checkpoint(
                state,
                &json_request(bearer, Some(&key), &self.checkpoint_body()),
            ),
            OperationId::GetCheckpoint => get_checkpoint(state, bearer, &self.checkpoint_id),
            OperationId::GetRecoveryPreview => {
                get_recovery_preview(state, bearer, &self.task_id, None)
            }
            OperationId::GetTaskCapsule => get_task_capsule(state, bearer, &self.task_id, None),
            OperationId::ProposeDecision => propose_decision(
                state,
                &json_request(bearer, Some(&key), &self.decision_body()),
            ),
            other => panic!("{other} is not a routed protected operation"),
        }
    }
}

fn code(response: &WireResponse) -> String {
    field(&parse_strict(&response.body).unwrap(), "code")
}

#[test]
fn every_protected_operation_enforces_its_access_rule_for_every_credential() {
    let world = World::new();
    let control = world.control.as_str();
    let (revoked_id, revoked) = world.harness.issue(control, "revoked", &["control"]);
    let revoke = revoke_client_token(
        &world.harness.state,
        &TokenRequest {
            bearer: Some(control),
            idempotency_key: Some(&world.harness.fresh_key()),
            content_type: None,
            body: Some(b""),
        },
        &revoked_id,
    );
    assert_eq!(revoke.status, 200, "{}", revoke.body);
    let (_, reader) = world.harness.issue(control, "reader", &["mcp-read"]);
    let (_, writer) = world.harness.issue(control, "writer", &["mcp-write"]);
    let invalid = "A".repeat(43);

    let mut table = Vec::new();
    for (operation, rule, success) in PROTECTED {
        for (label, bearer) in [
            ("none", None),
            ("invalid", Some(invalid.as_str())),
            ("revoked", Some(revoked.as_str())),
        ] {
            let response = match bearer {
                None => world.call_anonymous(operation),
                Some(bearer) => world.call(operation, Some(bearer)),
            };
            assert_eq!(
                response.status, 401,
                "{operation} {label}: {}",
                response.body
            );
            assert_eq!(code(&response), "authentication_required");
            assert!(response.bearer_challenge, "{operation} {label}");
            response.validate(Some(operation)).unwrap();
        }
        let mut row = Vec::new();
        for (scope, bearer) in [
            ("mcp-read", reader.as_str()),
            ("mcp-write", writer.as_str()),
            ("control", control),
        ] {
            let response = world.call(operation, Some(bearer));
            response.validate(Some(operation)).unwrap();
            if rule.admits(scope) {
                assert_eq!(
                    response.status, success,
                    "{operation} {scope}: {}",
                    response.body
                );
                row.push(scope);
            } else {
                assert_eq!(
                    response.status, 403,
                    "{operation} {scope}: {}",
                    response.body
                );
                assert_eq!(code(&response), "policy_denied");
            }
        }
        table.push(format!("{operation}: {}", row.join(", ")));
    }
    println!("accepted scopes:\n{}", table.join("\n"));
}

impl World {
    /// The same request without any `Authorization` header.
    fn call_anonymous(&self, operation: OperationId) -> WireResponse {
        let state = &self.harness.state;
        let body = r#"{"schema_version":1}"#;
        let request = json_request(None, Some("anonymous-key-0001"), body);
        match operation {
            OperationId::GetStatus => get_status(&state.plane, None),
            OperationId::GetVersion => get_version(&state.plane, None),
            OperationId::CreateClientToken => create_client_token(state, &request),
            OperationId::ListClientTokens => list_client_tokens(state, None, None),
            OperationId::RevokeClientToken => {
                revoke_client_token(state, &request, &self.fresh_token())
            }
            OperationId::RegisterProject => register_project(state, &request),
            OperationId::GetProject => get_project(state, None, &self.project_id),
            OperationId::ResolveWorkspace => resolve_workspace(state, &request),
            OperationId::CreateSession => create_session(state, &request),
            OperationId::ListSessions => list_sessions(
                state,
                None,
                Some(&format!("workspace_id={}", self.workspace_id)),
            ),
            OperationId::EndSession => end_session(state, &request, &self.session_id),
            OperationId::CreateTask => create_task(state, &request),
            OperationId::ListTasks => list_tasks(
                state,
                None,
                Some(&format!("project_id={}", self.project_id)),
            ),
            OperationId::TransitionTask => transition_task(state, &request, &self.fresh_task()),
            OperationId::IngestEvent => ingest_event(state, &request),
            OperationId::ListEvents => list_events(
                state,
                None,
                Some(&format!("project_id={}", self.project_id)),
            ),
            OperationId::GetEvent => get_event(state, None, &self.event_id),
            OperationId::CaptureEvidence => capture_evidence(state, &request),
            OperationId::GetEvidence => get_evidence(state, None, &self.evidence_id),
            OperationId::CreateCheckpoint => create_checkpoint(state, &request),
            OperationId::GetCheckpoint => get_checkpoint(state, None, &self.checkpoint_id),
            OperationId::GetRecoveryPreview => {
                get_recovery_preview(state, None, &self.task_id, None)
            }
            OperationId::GetTaskCapsule => get_task_capsule(state, None, &self.task_id, None),
            OperationId::ProposeDecision => propose_decision(state, &request),
            other => panic!("{other} is not a routed protected operation"),
        }
    }
}

fn exchange(socket: &Path, request: &[u8]) -> String {
    let mut stream = UnixStream::connect(socket).unwrap();
    // The server may answer and close before reading a rejected body.
    let _ = stream.write_all(request);
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    String::from_utf8_lossy(&response).into_owned()
}

fn post(path: &str, bearer: &str, key: &str, body: &[u8]) -> Vec<u8> {
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
         Authorization: Bearer {bearer}\r\nIdempotency-Key: {key}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    request
}

/// An `ingestEvent` body padded to exactly `size` bytes.
fn padded_envelope(world: &World, size: usize) -> Vec<u8> {
    let envelope = |padding: &str| {
        json!({
            "envelope_version": "1.0",
            "event_id": uuid::Uuid::now_v7().to_string(),
            "project_id": world.project_id,
            "workspace_id": world.workspace_id,
            "session_id": world.session_id,
            "event_type": "turn.started",
            "payload_schema": "turn.started@1",
            "producer": { "name": "limit", "version": "1", "sequence": 0 },
            "privacy_class": "internal",
            "payload": { "pad": padding },
        })
        .to_string()
    };
    let overhead = envelope("").len();
    let body = envelope(&"x".repeat(size - overhead));
    assert_eq!(body.len(), size);
    body.into_bytes()
}

#[test]
fn request_bodies_are_limited_to_two_mebibytes() {
    assert_eq!(MAX_REQUEST_BODY_BYTES, 2_097_152);
    let world = World::new();
    let control = world.control.clone();
    let at_limit = padded_envelope(&world, MAX_REQUEST_BODY_BYTES);
    let over_limit = padded_envelope(&world, MAX_REQUEST_BODY_BYTES + 1);
    let (dir, state) = world.harness.into_parts();
    let socket = dir.path().join("limit.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                axum::serve(listener, router(Arc::new(state))).await
            })
    });

    let accepted = exchange(
        &socket,
        &post("/v1/events", &control, "limit-key-0000001", &at_limit),
    );
    assert!(accepted.starts_with("HTTP/1.1 202"), "{accepted:.200}");
    let refused = exchange(
        &socket,
        &post("/v1/events", &control, "limit-key-0000002", &over_limit),
    );
    assert!(refused.starts_with("HTTP/1.1 400"), "{refused:.200}");
    assert!(refused.contains(r#""code":"invalid_request""#), "{refused}");
    // Authentication still comes first.
    let anonymous = exchange(
        &socket,
        &post("/v1/events", "nope", "limit-key-0000003", &over_limit),
    );
    assert!(anonymous.starts_with("HTTP/1.1 401"), "{anonymous:.200}");
    // The daemon keeps serving.
    let live = exchange(
        &socket,
        b"GET /health/live HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    );
    assert!(live.starts_with("HTTP/1.1 200"), "{live}");
    drop(dir);
}
