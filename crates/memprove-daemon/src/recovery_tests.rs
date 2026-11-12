//! Narrow tests of `captureEvidence`, `getEvidence`, `createCheckpoint`,
//! `getCheckpoint` (ADR 0012, P2-A), and `getRecoveryPreview` (D6, P2-B)
//! through the real handlers, store, and Git probes. The edges and
//! Decisions of ADR 0013 P3-C are in `claim_edge_tests`.

use std::path::PathBuf;

use memprove_protocol::{phase1::OperationId, strict_json::parse_strict};
use serde_json::{Value, json};

use crate::{
    api::{AppState, WireResponse},
    catalog::{
        create_session, create_task, end_session, list_tasks, register_project, resolve_workspace,
        transition_task,
    },
    events::{list_events, verify_ledger},
    recovery::{
        capture_evidence, create_checkpoint, get_checkpoint, get_evidence, get_recovery_preview,
        get_task_capsule,
    },
    test_support::{Harness, field, git_repository, git_worktree, json_request},
};

const AGENT: &str = "018f47a0-0000-7000-8000-00000000a6e1";
const UNKNOWN: &str = "018f47a0-0000-7000-8000-000000000bad";

/// Producer sequences of the Events tests append straight to the store.
static APPENDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_000);

struct Fixture {
    harness: Harness,
    control: String,
    reader: String,
    writer: String,
    repos: tempfile::TempDir,
    repo: PathBuf,
    project_id: String,
    workspace_id: String,
    task_id: String,
    session_id: String,
}

fn ok(response: &WireResponse, operation: OperationId, status: u16) -> Value {
    assert_eq!(response.status, status, "{}", response.body);
    response.validate(Some(operation)).unwrap();
    serde_json::from_str(&response.body).unwrap()
}

fn problem(response: &WireResponse, operation: OperationId, status: u16, code: &str) {
    assert_eq!(response.status, status, "{}", response.body);
    response.validate(Some(operation)).unwrap();
    assert_eq!(field(&parse_strict(&response.body).unwrap(), "code"), code);
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

impl Fixture {
    fn new() -> Self {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let (_, reader) = harness.issue(&control, "reader", &["mcp-read"]);
        let (_, writer) = harness.issue(&control, "writer", &["mcp-write"]);
        let repos = tempfile::tempdir().unwrap();
        let repo = git_repository(repos.path(), "repo");
        let mut fixture = Self {
            harness,
            control,
            reader,
            writer,
            repos,
            repo,
            project_id: String::new(),
            workspace_id: String::new(),
            task_id: String::new(),
            session_id: String::new(),
        };
        let body = json!({ "schema_version": 1, "root": fixture.repo }).to_string();
        let registered = ok(
            &register_project(
                fixture.state(),
                &json_request(Some(&fixture.control), Some(&fixture.key()), &body),
            ),
            OperationId::RegisterProject,
            201,
        );
        fixture.project_id = registered["project_id"].as_str().unwrap().to_owned();
        fixture.workspace_id = registered["root_workspace_id"].as_str().unwrap().to_owned();
        fixture.task_id = fixture.task(Some(&fixture.workspace_id.clone()));
        fixture.session_id = fixture.session(
            &fixture.workspace_id.clone(),
            Some(&fixture.task_id.clone()),
        );
        fixture
    }

    fn state(&self) -> &AppState {
        &self.harness.state
    }

    fn key(&self) -> String {
        self.harness.fresh_key()
    }

    fn task(&self, workspace_id: Option<&str>) -> String {
        let body = json!({
            "schema_version": 1, "project_id": self.project_id, "workspace_id": workspace_id,
            "title": "make the tests pass",
        });
        let created = create_task(
            self.state(),
            &json_request(Some(&self.writer), Some(&self.key()), &body.to_string()),
        );
        ok(&created, OperationId::CreateTask, 201)["task_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn session(&self, workspace_id: &str, task_id: Option<&str>) -> String {
        let body = json!({
            "schema_version": 1, "workspace_id": workspace_id, "agent_id": AGENT,
            "task_id": task_id,
        });
        let created = create_session(
            self.state(),
            &json_request(Some(&self.writer), Some(&self.key()), &body.to_string()),
        );
        ok(&created, OperationId::CreateSession, 201)["session_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// A `captureEvidence` body for a `cargo test` run.
    fn run_body(&self, artifact: &str, exit_code: i32, cases: Option<u64>) -> Value {
        let mut metadata = json!({
            "argv": ["cargo", "test"],
            "exit_code": exit_code,
            "started_at": "2026-10-02T10:00:00Z",
            "finished_at": "2026-10-02T10:00:02.500Z",
            "stdout_tail": "test result: ok",
            "stderr_tail": "",
            "stdout_sha256": digest('a'),
            "stderr_sha256": digest('b'),
            "collector": { "name": "memprove-cli", "version": "0.1.0" },
        });
        if artifact == "test_run" {
            metadata["test_name"] = json!("unit");
            if let Some(cases) = cases {
                metadata["test_case_count"] = json!(cases);
            }
        }
        json!({
            "schema_version": 1,
            "workspace_id": self.workspace_id,
            "task_id": self.task_id,
            "session_id": self.session_id,
            "artifact_type": artifact,
            "snapshot_id": uuid::Uuid::now_v7().to_string(),
            "metadata": metadata,
        })
    }

    fn capture_with(&self, bearer: &str, key: &str, body: &Value) -> WireResponse {
        capture_evidence(
            self.state(),
            &json_request(Some(bearer), Some(key), &body.to_string()),
        )
    }

    fn capture(&self, body: &Value) -> Value {
        ok(
            &self.capture_with(&self.writer, &self.key(), body),
            OperationId::CaptureEvidence,
            201,
        )
    }

    fn checkpoint_with(&self, bearer: &str, key: &str, body: &Value) -> WireResponse {
        create_checkpoint(
            self.state(),
            &json_request(Some(bearer), Some(key), &body.to_string()),
        )
    }

    /// A `createCheckpoint` body over `snapshot` claiming "tests pass" with
    /// `evidence`.
    fn checkpoint_body(&self, snapshot: &str, evidence: &[&str]) -> Value {
        json!({
            "schema_version": 1,
            "workspace_id": self.workspace_id,
            "task_id": self.task_id,
            "session_id": self.session_id,
            "snapshot_id": snapshot,
            "objective": "make the tests pass",
            "verified_completed_work": [
                { "classification": "verified_fact", "text": "tests pass", "evidence_ids": evidence },
            ],
            "open_items": [{ "classification": "unknown", "text": "docs not reviewed" }],
            "next_action": { "classification": "recommendation", "text": "open a pull request" },
        })
    }

    fn events(&self) -> Vec<Value> {
        let page = list_events(
            self.state(),
            Some(&self.control),
            Some(&format!("project_id={}&limit=500", self.project_id)),
        );
        ok(&page, OperationId::ListEvents, 200)["items"]
            .as_array()
            .unwrap()
            .clone()
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Every captureEvidence outcome against one Workspace.
fn capture_evidence_records_typed_results_pinned_to_a_snapshot() {
    let fixture = Fixture::new();
    let op = OperationId::CaptureEvidence;
    let head = String::from_utf8(
        std::process::Command::new("git")
            .args(["-C", fixture.repo.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    // A passing test run with a positive case count.
    let body = fixture.run_body("test_run", 0, Some(3));
    let passed = fixture.capture(&body);
    assert_eq!(passed["kind"], "test_result");
    assert_eq!(passed["artifact_type"], "test_run");
    assert_eq!(passed["status"], "PASS");
    assert_eq!(passed["payload"]["passed"], true);
    assert_eq!(passed["payload"]["test_case_count"], 3);
    assert_eq!(passed["payload"]["stdout_sha256"], "a".repeat(64));
    assert_eq!(passed["observed_at"], "2026-10-02T10:00:02.500Z");
    assert_eq!(passed["collector"]["name"], "memprove-cli");
    let snapshot = &passed["snapshot"];
    assert_eq!(snapshot["snapshot_id"], body["snapshot_id"]);
    assert_eq!(snapshot["head_oid"], head.trim());
    assert_eq!(snapshot["branch"], "main");
    assert_eq!(snapshot["is_dirty"], false);
    assert_eq!(
        snapshot["test_evidence_ids"],
        json!([passed["evidence_id"]])
    );
    assert_eq!(passed["snapshot_ids"], json!([body["snapshot_id"]]));

    // FR-203: zero cases or no count is NOT_RUN, never a pass; a non-zero
    // exit is FAIL.
    for (exit_code, cases, status) in [
        (0, Some(0), "NOT_RUN"),
        (0, None, "NOT_RUN"),
        (101, Some(3), "FAIL"),
    ] {
        let recorded = fixture.capture(&fixture.run_body("test_run", exit_code, cases));
        assert_eq!(recorded["status"], status, "{exit_code} {cases:?}");
        assert_eq!(recorded["payload"]["passed"], false);
    }
    let command = fixture.capture(&fixture.run_body("command_run", 2, None));
    assert_eq!(
        (command["kind"].as_str(), command["status"].as_str()),
        (Some("command_result"), Some("FAIL"))
    );

    // Each capture appended exactly one command.completed to the Session.
    let events = fixture.events();
    let completed = events
        .iter()
        .filter(|event| event["event_type"] == "command.completed")
        .collect::<Vec<_>>();
    assert_eq!(completed.len(), 5);
    assert_eq!(completed[0]["session_id"], fixture.session_id.as_str());
    assert_eq!(completed[0]["producer"]["name"], "continuityd");
    assert_eq!(completed[0]["occurred_at"], "2026-10-02T10:00:02.500Z");
    let payload = &completed[0]["payload"];
    assert_eq!(payload["command"]["evidence_id"], passed["evidence_id"]);
    assert_eq!(payload["command"]["passed"], true);
    assert_eq!(
        payload["repository_snapshot"]["snapshot_id"],
        body["snapshot_id"]
    );
    assert_eq!(payload["evidence"].as_array().unwrap().len(), 3);

    // getEvidence (control only) reads the same record back.
    let id = passed["evidence_id"].as_str().unwrap();
    let read = ok(
        &get_evidence(fixture.state(), Some(&fixture.control), id),
        OperationId::GetEvidence,
        200,
    );
    // P3-D: the four later runs share its repository state and carry it
    // over (ADR 0013 D3); the first Snapshot is still the one it was
    // recorded with.
    let mut expected = passed.clone();
    let carriers = completed
        .iter()
        .map(|event| event["payload"]["repository_snapshot"]["snapshot_id"].clone())
        .collect::<Vec<_>>();
    expected["snapshot_ids"] = Value::Array(carriers);
    assert_eq!(read, expected);
    assert_eq!(
        completed[4]["payload"]["evidence"]
            .as_array()
            .unwrap()
            .len(),
        3 + 4
    );
    let git_head = snapshot["git_head_evidence_id"].as_str().unwrap();
    let head_evidence = ok(
        &get_evidence(fixture.state(), Some(&fixture.control), git_head),
        OperationId::GetEvidence,
        200,
    );
    assert_eq!(head_evidence["kind"], "git_head");
    assert_eq!(head_evidence["status"], Value::Null);
    assert_eq!(head_evidence["collector"]["name"], "continuityd-git");
    for bearer in [&fixture.reader, &fixture.writer] {
        problem(
            &get_evidence(fixture.state(), Some(bearer), id),
            OperationId::GetEvidence,
            403,
            "policy_denied",
        );
    }
    problem(
        &get_evidence(fixture.state(), Some(&fixture.control), UNKNOWN),
        OperationId::GetEvidence,
        404,
        "resource_not_found",
    );
    problem(
        &get_evidence(fixture.state(), Some(&fixture.control), "nope"),
        OperationId::GetEvidence,
        400,
        "invalid_request",
    );

    // Idempotency: a replay returns the first record; the same key with
    // another request, or the same snapshot_id under another key, conflicts.
    let key = fixture.key();
    let body = fixture.run_body("command_run", 0, None);
    let first = ok(&fixture.capture_with(&fixture.writer, &key, &body), op, 201);
    let again = ok(&fixture.capture_with(&fixture.writer, &key, &body), op, 201);
    assert_eq!(again, first);
    let mut changed = body.clone();
    changed["metadata"]["exit_code"] = json!(1);
    problem(
        &fixture.capture_with(&fixture.writer, &key, &changed),
        op,
        409,
        "idempotency_conflict",
    );
    problem(
        &fixture.capture_with(&fixture.writer, &fixture.key(), &body),
        op,
        409,
        "idempotency_conflict",
    );
    assert_eq!(
        fixture
            .events()
            .iter()
            .filter(|event| event["event_type"] == "command.completed")
            .count(),
        6
    );

    // Scope: mcp-read cannot write; control can.
    problem(
        &fixture.capture_with(
            &fixture.reader,
            &fixture.key(),
            &fixture.run_body("command_run", 0, None),
        ),
        op,
        403,
        "policy_denied",
    );
    ok(
        &fixture.capture_with(
            &fixture.control,
            &fixture.key(),
            &fixture.run_body("command_run", 0, None),
        ),
        op,
        201,
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Every captureEvidence refusal.
fn capture_evidence_refuses_invalid_and_mismatched_requests() {
    let fixture = Fixture::new();
    let op = OperationId::CaptureEvidence;
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut body = fixture.run_body("test_run", 0, Some(1));
        edit(&mut body);
        fixture.capture_with(&fixture.writer, &fixture.key(), &body)
    };
    for (label, edit) in [
        (
            "other artifact",
            &(|body: &mut Value| body["artifact_type"] = json!("diff")) as &dyn Fn(&mut Value),
        ),
        ("object id", &|body: &mut Value| {
            body["object_id"] = json!(digest('c'));
        }),
        ("no argv", &|body: &mut Value| {
            body["metadata"]["argv"] = json!([]);
        }),
        ("bad digest", &|body: &mut Value| {
            body["metadata"]["stdout_sha256"] = json!("abc");
        }),
        ("bad time", &|body: &mut Value| {
            body["metadata"]["started_at"] = json!("yesterday");
        }),
        ("reversed run", &|body: &mut Value| {
            body["metadata"]["started_at"] = json!("2026-10-02T11:00:00Z");
        }),
        ("test fields on a command", &|body: &mut Value| {
            body["artifact_type"] = json!("command_run");
        }),
        ("bad uuid", &|body: &mut Value| {
            body["task_id"] = json!("not-a-uuid");
        }),
        ("missing session", &|body: &mut Value| {
            body.as_object_mut().unwrap().remove("session_id");
        }),
    ] {
        let response = with(edit);
        assert_eq!(response.status, 400, "{label}: {}", response.body);
        response.validate(Some(op)).unwrap();
    }
    problem(
        &with(&|body| body["workspace_id"] = json!(UNKNOWN)),
        op,
        404,
        "workspace_not_resolved",
    );
    problem(
        &with(&|body| body["task_id"] = json!(UNKNOWN)),
        op,
        404,
        "resource_not_found",
    );
    problem(
        &with(&|body| body["session_id"] = json!(UNKNOWN)),
        op,
        404,
        "resource_not_found",
    );

    // A Session that continues no Task, or another Task, is refused.
    let loose = fixture.session(&fixture.workspace_id, None);
    problem(
        &with(&|body| body["session_id"] = json!(loose)),
        op,
        400,
        "invalid_request",
    );
    let other_task = fixture.task(None);
    problem(
        &with(&|body| body["task_id"] = json!(other_task)),
        op,
        400,
        "invalid_request",
    );

    // A Session or Task of another Workspace is a mismatch.
    let linked = git_worktree(
        &fixture.repo,
        &fixture.repos.path().join("linked"),
        "feature",
    );
    let body = json!({ "schema_version": 1, "project_id": fixture.project_id, "path": linked });
    let linked_id = ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(Some(&fixture.writer), None, &body.to_string()),
        ),
        OperationId::ResolveWorkspace,
        200,
    )["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let floating = fixture.task(None);
    let linked_session = fixture.session(&linked_id, Some(&floating));
    problem(
        &with(&|body| {
            body["task_id"] = json!(floating);
            body["session_id"] = json!(linked_session);
        }),
        op,
        409,
        "project_workspace_mismatch",
    );
    problem(
        &with(&|body| {
            body["workspace_id"] = json!(linked_id);
            body["session_id"] = json!(linked_session);
        }),
        op,
        409,
        "project_workspace_mismatch",
    );

    // An ended Session accepts no Evidence.
    let ended = end_session(
        fixture.state(),
        &json_request(
            Some(&fixture.writer),
            Some(&fixture.key()),
            r#"{"schema_version":1,"expected_revision":1,"reason":"done"}"#,
        ),
        &fixture.session_id,
    );
    ok(&ended, OperationId::EndSession, 200);
    problem(&with(&|_| {}), op, 409, "invalid_transition");

    // Nothing above recorded Evidence or a command.completed Event.
    assert!(
        fixture
            .events()
            .iter()
            .all(|event| event["event_type"] != "command.completed")
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Every createCheckpoint and getCheckpoint outcome.
fn checkpoints_anchor_statements_to_recorded_snapshot_evidence() {
    let fixture = Fixture::new();
    let op = OperationId::CreateCheckpoint;
    let passed = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    let snapshot = passed["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let evidence = passed["evidence_id"].as_str().unwrap().to_owned();
    let before = fixture.events().len();

    let key = fixture.key();
    let body = fixture.checkpoint_body(&snapshot, &[&evidence]);
    let created = ok(
        &fixture.checkpoint_with(&fixture.writer, &key, &body),
        op,
        201,
    );
    assert_eq!(created["snapshot_id"], snapshot.as_str());
    assert_eq!(created["workspace_id"], fixture.workspace_id.as_str());
    assert_eq!(created["project_event_sequence"], before);
    assert_eq!(created["verified_completed_work"][0]["text"], "tests pass");
    assert_eq!(
        created["verified_completed_work"][0]["evidence_ids"],
        json!([evidence])
    );
    assert_eq!(created["next_action"]["classification"], "recommendation");
    assert!(created["digest"].as_str().unwrap().starts_with("sha256:"));
    let events = fixture.events();
    assert_eq!(events.len(), before + 1);
    let event = events.last().unwrap();
    assert_eq!(event["event_type"], "checkpoint.created");
    assert_eq!(
        event["payload"]["checkpoint"]["checkpoint_id"],
        created["checkpoint_id"]
    );

    // Replay and conflict.
    let replay = ok(
        &fixture.checkpoint_with(&fixture.writer, &key, &body),
        op,
        201,
    );
    assert_eq!(replay, created);
    let mut changed = body.clone();
    changed["objective"] = json!("something else");
    problem(
        &fixture.checkpoint_with(&fixture.writer, &key, &changed),
        op,
        409,
        "idempotency_conflict",
    );
    assert_eq!(fixture.events().len(), before + 1);

    // Reads: control and mcp-read; mcp-write is refused.
    let id = created["checkpoint_id"].as_str().unwrap();
    for bearer in [&fixture.control, &fixture.reader] {
        let read = ok(
            &get_checkpoint(fixture.state(), Some(bearer), id),
            OperationId::GetCheckpoint,
            200,
        );
        assert_eq!(read, created);
    }
    problem(
        &get_checkpoint(fixture.state(), Some(&fixture.writer), id),
        OperationId::GetCheckpoint,
        403,
        "policy_denied",
    );
    problem(
        &get_checkpoint(fixture.state(), Some(&fixture.control), UNKNOWN),
        OperationId::GetCheckpoint,
        404,
        "resource_not_found",
    );
    problem(
        &fixture.checkpoint_with(&fixture.reader, &fixture.key(), &body),
        op,
        403,
        "policy_denied",
    );

    // FR-203: a zero-case or failed test cannot support "tests pass".
    for (exit_code, cases) in [(0, Some(0)), (0, None), (1, Some(3))] {
        let weak = fixture.capture(&fixture.run_body("test_run", exit_code, cases));
        let weak_body = fixture.checkpoint_body(
            weak["snapshot"]["snapshot_id"].as_str().unwrap(),
            &[weak["evidence_id"].as_str().unwrap()],
        );
        problem(
            &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &weak_body),
            op,
            400,
            "invalid_request",
        );
    }

    // Evidence outside the Snapshot, a wrong classification, and a foreign
    // Snapshot are refused; so are unknown and mismatched references.
    let other = fixture.capture(&fixture.run_body("test_run", 0, Some(1)));
    let outside = fixture.checkpoint_body(&snapshot, &[other["evidence_id"].as_str().unwrap()]);
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &outside),
        op,
        400,
        "invalid_request",
    );
    let mut verified_next = body.clone();
    verified_next["next_action"] =
        json!({ "classification": "verified_fact", "text": "x", "evidence_ids": [evidence] });
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &verified_next),
        op,
        400,
        "invalid_request",
    );
    let mut unbacked = body.clone();
    unbacked["verified_completed_work"][0]["evidence_ids"] = json!([]);
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &unbacked),
        op,
        400,
        "invalid_request",
    );
    let mut unknown_snapshot = body.clone();
    unknown_snapshot["snapshot_id"] = json!(UNKNOWN);
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &unknown_snapshot),
        op,
        404,
        "resource_not_found",
    );
    let mut unknown_workspace = body.clone();
    unknown_workspace["workspace_id"] = json!(UNKNOWN);
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &unknown_workspace),
        op,
        404,
        "workspace_not_resolved",
    );
    let other_task = fixture.task(None);
    let other_session = fixture.session(&fixture.workspace_id, Some(&other_task));
    let mut foreign = body.clone();
    foreign["task_id"] = json!(other_task);
    foreign["session_id"] = json!(other_session);
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &foreign),
        op,
        400,
        "invalid_request",
    );
    let loose = fixture.session(&fixture.workspace_id, None);
    let mut unbound = body.clone();
    unbound["session_id"] = json!(loose);
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &unbound),
        op,
        400,
        "invalid_request",
    );

    // An ended Session records no Checkpoint.
    ok(
        &end_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                r#"{"schema_version":1,"expected_revision":1}"#,
            ),
            &fixture.session_id,
        ),
        OperationId::EndSession,
        200,
    );
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body.clone()),
        op,
        409,
        "invalid_transition",
    );

    // The ledger still verifies offline.
    let data_dir = std::fs::canonicalize(fixture.harness.dir.path()).unwrap();
    let registry = memprove_store_sqlite::RegistryStore::open(&data_dir).unwrap();
    let project = registry
        .get_project(&memprove_domain::ProjectId::parse(&fixture.project_id).unwrap())
        .unwrap();
    memprove_store_sqlite::StateStore::open(&data_dir, &project)
        .unwrap()
        .verify_event_chain()
        .unwrap();
}

impl Fixture {
    fn preview_with(
        &self,
        bearer: Option<&str>,
        task_id: &str,
        query: Option<&str>,
    ) -> WireResponse {
        get_recovery_preview(self.state(), bearer, task_id, query)
    }

    fn preview(&self, query: Option<&str>) -> (String, Value) {
        let response = self.preview_with(Some(&self.reader), &self.task_id, query);
        let body = ok(&response, OperationId::GetRecoveryPreview, 200);
        (response.body, body)
    }

    /// A passed three-case test run and a Checkpoint "tests pass" citing it.
    fn checkpointed(&self) -> (String, String) {
        let passed = self.capture(&self.run_body("test_run", 0, Some(3)));
        let evidence_id = passed["evidence_id"].as_str().unwrap().to_owned();
        let snapshot_id = passed["snapshot"]["snapshot_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let created = self.checkpoint_with(
            &self.writer,
            &self.key(),
            &self.checkpoint_body(&snapshot_id, &[&evidence_id]),
        );
        let checkpoint = ok(&created, OperationId::CreateCheckpoint, 201);
        (
            evidence_id,
            checkpoint["checkpoint_id"].as_str().unwrap().to_owned(),
        )
    }

    fn state_file(&self) -> Vec<u8> {
        let path = std::fs::canonicalize(self.harness.dir.path())
            .unwrap()
            .join("projects")
            .join(&self.project_id)
            .join("state.sqlite");
        std::fs::read(path).unwrap()
    }

    fn commit(&self, message: &str) -> String {
        commit_in(&self.repo, message)
    }
}

/// Commits every tracked change in the worktree at `directory`; returns
/// the new HEAD.
fn commit_in(directory: &std::path::Path, message: &str) -> String {
    let run = |arguments: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{arguments:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    run(&[
        "-c",
        "user.name=memprove Test",
        "-c",
        "user.email=memprove@example.invalid",
        "commit",
        "--quiet",
        "-am",
        message,
    ]);
    run(&["rev-parse", "HEAD"]).trim().to_owned()
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)] // One Checkpoint judged across repository changes.
fn recovery_preview_judges_the_latest_checkpoint_and_writes_nothing() {
    let fixture = Fixture::new();
    let (evidence_id, checkpoint_id) = fixture.checkpointed();
    let events = fixture.events().len();
    let stored = fixture.state_file();

    let (bytes, verified) = fixture.preview(None);
    assert_eq!(verified["status"], "verified");
    assert_eq!(strings(&verified["reasons"]), Vec::<&str>::new());
    assert_eq!(
        verified["bindings"]["checkpoint"]["checkpoint_id"],
        checkpoint_id.as_str()
    );
    assert_eq!(
        verified["bindings"]["probe"]["workspace_id"],
        fixture.workspace_id.as_str()
    );
    assert_eq!(
        verified["bindings"]["snapshot"]["head_oid"],
        verified["bindings"]["probe"]["head_oid"]
    );
    assert_eq!(verified["facts"][0]["text"], "tests pass");
    assert_eq!(
        verified["facts"][0]["evidence"][0]["evidence_id"],
        evidence_id.as_str()
    );
    assert_eq!(verified["facts"][0]["verdict"], "verified");
    // The other statements cite no Evidence: partial, but not propagated,
    // so they are not a gap (Owner decision, P2-C).
    assert_eq!(verified["facts"][1]["verdict"], "partial");
    assert_eq!(verified["facts"][1]["propagates"], false);
    assert_eq!(strings(&verified["gaps"]), Vec::<&str>::new());
    assert_eq!(strings(&verified["changed_paths"]), Vec::<&str>::new());
    // JCS bytes; the digest covers the document without itself.
    let canonical = memprove_domain::CanonicalJson::parse(&bytes).unwrap();
    assert_eq!(canonical.as_str(), bytes);
    let mut without = verified.clone();
    without.as_object_mut().unwrap().remove("preview_digest");
    let digest = memprove_domain::CanonicalJson::parse(&without.to_string())
        .unwrap()
        .sha256();
    assert_eq!(verified["preview_digest"], format!("sha256:{digest}"));
    // The same N and probe give the same bytes; nothing is written.
    assert_eq!(fixture.preview(None).0, bytes);
    assert_eq!(fixture.events().len(), events);
    assert_eq!(
        fixture.state_file(),
        stored,
        "the Preview wrote to state.sqlite"
    );

    std::fs::write(fixture.repo.join("README"), b"edited\n").unwrap();
    let (_, edited) = fixture.preview(None);
    assert_eq!(edited["status"], "stale");
    assert_eq!(
        strings(&edited["reasons"]),
        ["worktree_changed", "evidence_stale"]
    );
    assert_eq!(strings(&edited["changed_paths"]), ["repo://README"]);
    assert_eq!(
        edited["next_checks"][0]["evidence_id"],
        evidence_id.as_str()
    );
    assert_eq!(
        strings(&edited["next_checks"][0]["argv"]),
        ["cargo", "test"]
    );

    let head = fixture.commit("change README");
    let (_, committed) = fixture.preview(None);
    assert_eq!(committed["status"], "stale");
    assert_eq!(
        strings(&committed["reasons"]),
        ["head_changed", "index_changed", "evidence_stale"]
    );
    assert_eq!(committed["bindings"]["probe"]["head_oid"], head.as_str());
    assert_ne!(committed["bindings"]["snapshot"]["head_oid"], head.as_str());
    assert_eq!(strings(&committed["changed_paths"]), ["repo://README"]);
    assert_eq!(committed["next_checks"][0]["at_head"], head.as_str());
    assert_eq!(fixture.events().len(), events);
    assert_eq!(fixture.state_file(), stored);
}

#[test]
#[allow(clippy::too_many_lines)] // Every getRecoveryPreview input and credential.
fn recovery_preview_refuses_invalid_input_and_enforces_its_access_rule() {
    let fixture = Fixture::new();
    let op = OperationId::GetRecoveryPreview;
    let task = fixture.task_id.clone();

    // A Task without a Checkpoint is partial, not an error.
    let (_, empty) = fixture.preview(None);
    assert_eq!(empty["status"], "partial");
    assert_eq!(strings(&empty["reasons"]), ["evidence_unavailable"]);
    assert!(strings(&empty["gaps"]).contains(&"no_checkpoint"));
    assert_eq!(empty["bindings"]["checkpoint"], Value::Null);
    fixture.checkpointed();

    // Access: control or mcp-read only.
    ok(
        &fixture.preview_with(Some(&fixture.control), &task, None),
        op,
        200,
    );
    ok(
        &fixture.preview_with(Some(&fixture.reader), &task, None),
        op,
        200,
    );
    problem(
        &fixture.preview_with(Some(&fixture.writer), &task, None),
        op,
        403,
        "policy_denied",
    );
    for bearer in [None, Some("A".repeat(43))] {
        let response = fixture.preview_with(bearer.as_deref(), &task, None);
        problem(&response, op, 401, "authentication_required");
        assert!(response.bearer_challenge);
    }

    // Malformed input.
    let workspace = fixture.workspace_id.clone();
    for (task_id, query) in [
        ("not-a-uuid", None),
        ("018f47a0-0000-4000-8000-000000000001", None),
        (task.as_str(), Some("workspace_id=nope".to_owned())),
        (task.as_str(), Some(format!("workspace={workspace}"))),
        (
            task.as_str(),
            Some(format!("workspace_id={workspace}&workspace_id={workspace}")),
        ),
        (
            task.as_str(),
            Some(format!("workspace_id={workspace}&limit=1")),
        ),
        (task.as_str(), Some("workspace_id".to_owned())),
    ] {
        problem(
            &fixture.preview_with(Some(&fixture.reader), task_id, query.as_deref()),
            op,
            400,
            "invalid_request",
        );
    }
    problem(
        &fixture.preview_with(Some(&fixture.reader), UNKNOWN, None),
        op,
        404,
        "resource_not_found",
    );
    problem(
        &fixture.preview_with(
            Some(&fixture.reader),
            &task,
            Some(&format!("workspace_id={UNKNOWN}")),
        ),
        op,
        404,
        "workspace_not_resolved",
    );

    // A Workspace of another Project.
    let other_repo = git_repository(fixture.repos.path(), "other");
    let body = json!({ "schema_version": 1, "root": other_repo }).to_string();
    let other = ok(
        &register_project(
            fixture.state(),
            &json_request(Some(&fixture.control), Some(&fixture.key()), &body),
        ),
        OperationId::RegisterProject,
        201,
    );
    let foreign = other["root_workspace_id"].as_str().unwrap();
    problem(
        &fixture.preview_with(
            Some(&fixture.reader),
            &task,
            Some(&format!("workspace_id={foreign}")),
        ),
        op,
        409,
        "project_workspace_mismatch",
    );

    // A linked worktree of the same Project: the probe runs there.
    let linked_path = git_worktree(&fixture.repo, &fixture.repos.path().join("linked"), "side");
    let body = json!({
        "schema_version": 1, "project_id": fixture.project_id, "path": linked_path,
    });
    let linked = ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(Some(&fixture.writer), None, &body.to_string()),
        ),
        OperationId::ResolveWorkspace,
        200,
    );
    let linked_id = linked["workspace_id"].as_str().unwrap();
    let (_, asked) = fixture.preview(Some(&format!("workspace_id={linked_id}")));
    assert_eq!(asked["bindings"]["probe"]["workspace_id"], linked_id);
    assert_eq!(asked["bindings"]["probe"]["branch"], "side");
    assert_eq!(asked["bindings"]["probe"]["same_workspace"], true);
    // Same commit, clean, no untracked files, other branch and worktree:
    // the test still holds (Owner decision, P2-C); both branches are shown.
    assert_eq!(asked["status"], "verified", "{asked}");
    assert_eq!(strings(&asked["reasons"]), Vec::<&str>::new());
    assert_eq!(asked["bindings"]["snapshot"]["branch"], "main");
    assert_eq!(asked["facts"][0]["verdict"], "verified");
    // Another commit in that worktree: HEAD differs, so it is stale.
    std::fs::write(linked_path.join("README"), b"changed on side\n").unwrap();
    let side_head = commit_in(&linked_path, "change README on side");
    let (_, moved) = fixture.preview(Some(&format!("workspace_id={linked_id}")));
    assert_eq!(moved["status"], "stale");
    assert_eq!(moved["bindings"]["probe"]["head_oid"], side_head.as_str());
    assert!(strings(&moved["reasons"]).contains(&"head_changed"));
    assert!(strings(&moved["reasons"]).contains(&"evidence_stale"));

    // The linked worktree is gone: partial, not an error.
    std::fs::remove_dir_all(&linked_path).unwrap();
    let (_, gone) = fixture.preview(Some(&format!("workspace_id={linked_id}")));
    assert_eq!(gone["status"], "partial");
    assert_eq!(gone["bindings"]["probe"]["available"], false);
    assert!(strings(&gone["gaps"]).contains(&"workspace_unavailable"));
    assert_eq!(gone["next_checks"][0]["at_head"], Value::Null);
}

/// PRD FR-203 / P2-C: a command the caller could not start is recorded as
/// `UNAVAILABLE` and cannot back a "tests pass" statement.
#[test]
fn a_command_that_could_not_start_is_recorded_unavailable() {
    let fixture = Fixture::new();
    for artifact in ["command_run", "test_run"] {
        let mut body = fixture.run_body(artifact, -1, Some(3));
        body["metadata"]["launch_error"] = json!("No such file or directory (os error 2)");
        body["metadata"]["stdout_tail"] = json!("");
        let recorded = fixture.capture(&body);
        assert_eq!(recorded["status"], "UNAVAILABLE", "{recorded}");
        assert_eq!(recorded["payload"]["exit_code"], -1);
        assert_eq!(
            recorded["payload"]["launch_error"],
            "No such file or directory (os error 2)"
        );
        if artifact == "test_run" {
            assert_eq!(recorded["payload"]["passed"], false);
            let snapshot = recorded["snapshot"]["snapshot_id"].as_str().unwrap();
            let evidence = recorded["evidence_id"].as_str().unwrap();
            problem(
                &fixture.checkpoint_with(
                    &fixture.writer,
                    &fixture.key(),
                    &fixture.checkpoint_body(snapshot, &[evidence]),
                ),
                OperationId::CreateCheckpoint,
                400,
                "invalid_request",
            );
        }
    }
    // A launch error with a real exit code is refused.
    let mut body = fixture.run_body("command_run", 0, None);
    body["metadata"]["launch_error"] = json!("could not start");
    problem(
        &fixture.capture_with(&fixture.writer, &fixture.key(), &body),
        OperationId::CaptureEvidence,
        400,
        "invalid_request",
    );
    // Without launch_error a started run keeps its usual outcome.
    let started = fixture.capture(&fixture.run_body("command_run", 3, None));
    assert_eq!(started["status"], "FAIL");
    assert!(started["payload"].get("launch_error").is_none());
}

impl Fixture {
    fn verify_with(
        &self,
        bearer: Option<&str>,
        key: Option<&str>,
        project_id: &str,
    ) -> WireResponse {
        let body = json!({ "schema_version": 1, "project_id": project_id }).to_string();
        verify_ledger(self.state(), &json_request(bearer, key, &body))
    }
}

/// `verifyLedger` (P2-C): read-only, `control` or `mcp-read`, an
/// `Idempotency-Key` required but not recorded, and the counts of an intact
/// ledger.
#[test]
fn verify_ledger_reports_an_intact_ledger_and_enforces_its_access_rule() {
    let fixture = Fixture::new();
    let op = OperationId::VerifyLedger;
    fixture.checkpointed();
    let events = fixture.events();
    let stored = fixture.state_file();
    let key = fixture.key();
    let body = ok(
        &fixture.verify_with(Some(&fixture.reader), Some(&key), &fixture.project_id),
        op,
        200,
    );
    assert_eq!(body["valid"], true, "{body}");
    assert_eq!(body["first_failure"], Value::Null);
    assert_eq!(body["project_id"], fixture.project_id.as_str());
    assert_eq!(body["project_sequence"], events.len());
    assert_eq!(body["checked"]["events"], events.len());
    // git_head, git_worktree, and the test of the one capture.
    assert_eq!(body["checked"]["evidence"], 3);
    assert_eq!(body["checked"]["snapshots"], 1);
    assert_eq!(body["checked"]["checkpoints"], 1);
    assert!(
        body["head_event_sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    // The same key again verifies again; nothing was written.
    ok(
        &fixture.verify_with(Some(&fixture.control), Some(&key), &fixture.project_id),
        op,
        200,
    );
    assert_eq!(fixture.state_file(), stored, "verifyLedger wrote");
    assert_eq!(fixture.events().len(), events.len());

    problem(
        &fixture.verify_with(
            Some(&fixture.writer),
            Some(&fixture.key()),
            &fixture.project_id,
        ),
        op,
        403,
        "policy_denied",
    );
    let response = fixture.verify_with(None, Some(&fixture.key()), &fixture.project_id);
    problem(&response, op, 401, "authentication_required");
    problem(
        &fixture.verify_with(Some(&fixture.reader), None, &fixture.project_id),
        op,
        400,
        "invalid_request",
    );
    problem(
        &fixture.verify_with(Some(&fixture.reader), Some(&fixture.key()), "nope"),
        op,
        400,
        "invalid_request",
    );
    problem(
        &fixture.verify_with(Some(&fixture.reader), Some(&fixture.key()), UNKNOWN),
        op,
        404,
        "project_not_found",
    );
}

impl Fixture {
    fn transition_with(&self, task_id: &str, key: &str, body: &Value) -> WireResponse {
        transition_task(
            self.state(),
            &json_request(Some(&self.writer), Some(key), &body.to_string()),
            task_id,
        )
    }

    fn step(&self, task_id: &str, revision: u64, target: &str) -> Value {
        ok(
            &self.transition_with(
                task_id,
                &self.key(),
                &json!({
                    "schema_version": 1, "expected_revision": revision, "target_state": target,
                }),
            ),
            OperationId::TransitionTask,
            200,
        )
    }
}

fn completion(revision: u64, evidence: &[&str]) -> Value {
    json!({
        "schema_version": 1, "expected_revision": revision, "target_state": "completed",
        "completion_evidence_ids": evidence,
    })
}

/// P2-C: `completed` needs Evidence of the Task, `PASS`, and `verified` now
/// by the Recovery Preview's judgement; the Evidence is recorded in the
/// `task.transitioned` Event and read back with the Task.
#[test]
#[allow(clippy::too_many_lines)] // Every completion refusal, then the completion.
fn completing_a_task_requires_verified_pass_evidence_of_the_task() {
    let fixture = Fixture::new();
    let op = OperationId::TransitionTask;
    let task = fixture.task_id.clone();
    let ready_key = fixture.key();
    let ready_body =
        json!({ "schema_version": 1, "expected_revision": 1, "target_state": "ready" });
    let ready = ok(
        &fixture.transition_with(&task, &ready_key, &ready_body),
        op,
        200,
    );
    fixture.step(&task, 2, "in_progress");
    let (passed, _) = fixture.checkpointed();
    let stale = |body: &Value| {
        problem(
            &fixture.transition_with(&task, &fixture.key(), body),
            op,
            409,
            "evidence_stale",
        );
    };

    // An empty list, an unknown Evidence, a failing run, a run with no cases.
    // P3-D3: the failing and the empty run are another command than the
    // passed one; a later run of the same argv in this state that did not
    // pass would overrule it (the P3-D3 tests).
    let other_command = |exit_code: i32, cases: u64| {
        let mut body = fixture.run_body("test_run", exit_code, Some(cases));
        body["metadata"]["argv"] = json!(["cargo", "test", "--", "--ignored"]);
        body
    };
    stale(&completion(3, &[]));
    stale(&json!({ "schema_version": 1, "expected_revision": 3, "target_state": "completed" }));
    stale(&completion(3, &[UNKNOWN]));
    let failed = fixture.capture(&other_command(1, 3));
    stale(&completion(3, &[failed["evidence_id"].as_str().unwrap()]));
    let empty = fixture.capture(&other_command(0, 0));
    assert_eq!(empty["status"], "NOT_RUN");
    stale(&completion(3, &[empty["evidence_id"].as_str().unwrap()]));
    // A passed run among them is not enough.
    stale(&completion(
        3,
        &[&passed, failed["evidence_id"].as_str().unwrap()],
    ));

    // Evidence of another Task, even a passed one.
    let other = fixture.task(Some(&fixture.workspace_id.clone()));
    let other_session = fixture.session(&fixture.workspace_id.clone(), Some(&other));
    let mut body = fixture.run_body("test_run", 0, Some(3));
    body["task_id"] = json!(other);
    body["session_id"] = json!(other_session);
    let foreign = fixture.capture(&body);
    assert_eq!(foreign["status"], "PASS");
    stale(&completion(3, &[foreign["evidence_id"].as_str().unwrap()]));

    // Still refused: force_reason, Evidence on another target, a bad
    // workspace_id; and the cheap checks come before the Evidence.
    let mut forced = completion(3, &[&passed]);
    forced["force_reason"] = json!("trust me");
    problem(
        &fixture.transition_with(&task, &fixture.key(), &forced),
        op,
        400,
        "invalid_request",
    );
    let mut blocked = completion(3, &[&passed]);
    blocked["target_state"] = json!("blocked");
    problem(
        &fixture.transition_with(&task, &fixture.key(), &blocked),
        op,
        400,
        "invalid_request",
    );
    let mut nowhere = completion(3, &[&passed]);
    nowhere["workspace_id"] = json!(UNKNOWN);
    problem(
        &fixture.transition_with(&task, &fixture.key(), &nowhere),
        op,
        404,
        "workspace_not_resolved",
    );
    problem(
        &fixture.transition_with(&task, &fixture.key(), &completion(2, &[&passed])),
        op,
        409,
        "revision_conflict",
    );

    // The file changed since the run: stale.
    std::fs::write(fixture.repo.join("README"), b"edited\n").unwrap();
    stale(&completion(3, &[&passed]));
    let preview = fixture.preview(None).1;
    assert_eq!(preview["status"], "stale", "the Preview agrees");

    // Back as it was: the same judgement now holds, and the Task completes.
    git_restore(&fixture.repo, "README");
    assert_eq!(fixture.preview(None).1["status"], "verified");
    let key = fixture.key();
    let done = ok(
        &fixture.transition_with(&task, &key, &completion(3, &[&passed])),
        op,
        200,
    );
    assert_eq!(done["state"], "completed");
    assert_eq!(done["revision"], 4);
    assert_eq!(strings(&done["completion_evidence_ids"]), [passed.as_str()]);

    // The Evidence is read back with the Task and recorded in its Event.
    let listed = ok(
        &list_tasks(
            fixture.state(),
            Some(&fixture.reader),
            Some(&format!("project_id={}", fixture.project_id)),
        ),
        OperationId::ListTasks,
        200,
    );
    let row = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["task_id"] == task.as_str())
        .unwrap()
        .clone();
    assert_eq!(row, done);
    let transitions = fixture
        .events()
        .into_iter()
        .filter(|event| {
            event["event_type"] == "task.transitioned"
                && event["payload"]["task_id"] == task.as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        transitions
            .iter()
            .map(|event| event["payload"]["to"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ready", "in_progress", "completed"]
    );
    let last = transitions.last().unwrap();
    assert_eq!(last["session_id"], Value::Null);
    assert_eq!(last["producer"]["name"], "continuityd");
    assert_eq!(
        strings(&last["payload"]["completion_evidence_ids"]),
        [passed.as_str()]
    );

    // An earlier transition replays as it was, without the later Evidence.
    let replayed_ready = ok(
        &fixture.transition_with(&task, &ready_key, &ready_body),
        op,
        200,
    );
    assert_eq!(replayed_ready, ready);
    assert_eq!(
        strings(&replayed_ready["completion_evidence_ids"]),
        Vec::<&str>::new()
    );
    // A replay is not judged again, even once the file changes.
    std::fs::write(fixture.repo.join("README"), b"edited again\n").unwrap();
    assert_eq!(
        ok(
            &fixture.transition_with(&task, &key, &completion(3, &[&passed])),
            op,
            200
        ),
        done
    );
    // completed is terminal.
    problem(
        &fixture.transition_with(&task, &fixture.key(), &completion(4, &[&passed])),
        op,
        409,
        "invalid_transition",
    );
    let verification = ok(
        &fixture.verify_with(
            Some(&fixture.reader),
            Some(&fixture.key()),
            &fixture.project_id,
        ),
        OperationId::VerifyLedger,
        200,
    );
    assert_eq!(verification["valid"], true);
}

/// P2-C: the completion probe follows the Preview: a linked worktree at the
/// same commit on another branch verifies the Evidence through
/// `workspace_id`.
#[test]
fn a_task_completes_from_a_linked_worktree_at_the_same_commit() {
    let fixture = Fixture::new();
    let task = fixture.task_id.clone();
    fixture.step(&task, 1, "ready");
    fixture.step(&task, 2, "in_progress");
    let (passed, _) = fixture.checkpointed();
    let linked_path = git_worktree(&fixture.repo, &fixture.repos.path().join("side"), "side");
    let linked = ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                None,
                &json!({
                    "schema_version": 1, "project_id": fixture.project_id, "path": linked_path,
                })
                .to_string(),
            ),
        ),
        OperationId::ResolveWorkspace,
        200,
    );
    // The original worktree changes; the linked one is still at the commit.
    std::fs::write(fixture.repo.join("README"), b"edited\n").unwrap();
    let mut body = completion(3, &[&passed]);
    body["workspace_id"] = linked["workspace_id"].clone();
    let done = ok(
        &fixture.transition_with(&task, &fixture.key(), &body),
        OperationId::TransitionTask,
        200,
    );
    assert_eq!(done["state"], "completed");
    // The registration was recorded without a Session.
    let registered = fixture
        .events()
        .into_iter()
        .find(|event| event["event_type"] == "workspace.registered")
        .unwrap();
    assert_eq!(registered["session_id"], Value::Null);
    assert_eq!(registered["workspace_id"], linked["workspace_id"]);
}

/// `git checkout -- <path>` in `repository`.
fn git_restore(repository: &std::path::Path, path: &str) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["checkout", "--quiet", "--", path])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
}

/// P2-E (T9 B1): a `verified_fact` in any section needs a passed test, so a
/// failed, unrun, or unstartable test cannot back "tests pass" in
/// `open_items`. A failure is recorded under another classification, and
/// the Preview shows each Evidence object's outcome.
#[test]
#[allow(clippy::too_many_lines)] // Every non-PASS outcome, then the honest record.
fn p2e_a_verified_fact_in_any_section_needs_a_passed_test() {
    let fixture = Fixture::new();
    let op = OperationId::CreateCheckpoint;
    let unavailable = {
        let mut body = fixture.run_body("test_run", -1, Some(3));
        body["metadata"]["launch_error"] = json!("No such file or directory (os error 2)");
        body
    };
    let open_item_claim = |recorded: &Value, classification: &str, text: &str| {
        let mut body =
            fixture.checkpoint_body(recorded["snapshot"]["snapshot_id"].as_str().unwrap(), &[]);
        body["verified_completed_work"] = json!([]);
        body["open_items"] = json!([{
            "classification": classification, "text": text,
            "evidence_ids": [recorded["evidence_id"]],
        }]);
        body
    };
    for (body, status) in [
        (fixture.run_body("test_run", 1, Some(3)), "FAIL"),
        (fixture.run_body("test_run", 0, Some(0)), "NOT_RUN"),
        (fixture.run_body("test_run", 0, None), "NOT_RUN"),
        (unavailable, "UNAVAILABLE"),
    ] {
        let recorded = fixture.capture(&body);
        assert_eq!(recorded["status"], status);
        problem(
            &fixture.checkpoint_with(
                &fixture.writer,
                &fixture.key(),
                &open_item_claim(&recorded, "verified_fact", "tests pass"),
            ),
            op,
            400,
            "invalid_request",
        );
    }
    // A verified_fact on a passed command alone is refused too: the rule asks
    // for a passed test.
    let command = fixture.capture(&fixture.run_body("command_run", 0, None));
    problem(
        &fixture.checkpoint_with(
            &fixture.writer,
            &fixture.key(),
            &open_item_claim(&command, "verified_fact", "build passes"),
        ),
        op,
        400,
        "invalid_request",
    );

    // The failure itself is recorded as unknown, citing the failed run.
    let failed = fixture.capture(&fixture.run_body("test_run", 1, Some(3)));
    ok(
        &fixture.checkpoint_with(
            &fixture.writer,
            &fixture.key(),
            &open_item_claim(&failed, "unknown", "tests fail"),
        ),
        op,
        201,
    );
    let preview = fixture.preview(None).1;
    assert_eq!(preview["status"], "partial");
    assert_eq!(strings(&preview["gaps"]), ["no_verified_fact"]);
    let fact = &preview["facts"][0];
    assert_eq!(fact["claim"]["section"], "open_items");
    assert_eq!(fact["propagates"], false);
    assert_eq!(fact["evidence"][0]["run_status"], "FAIL");
    assert_eq!(fact["verdict"], "partial");
    // P2-F: the fresh failed run is evidence_not_passed, and the response
    // passed the daemon's own validation in `ok`; with no verified fact the
    // Checkpoint is evidence_unavailable.
    assert_eq!(strings(&fact["reasons"]), ["evidence_not_passed"]);
    assert_eq!(
        strings(&fact["evidence"][0]["reasons"]),
        ["evidence_not_passed"]
    );
    assert_eq!(fact["evidence"][0]["argv"], json!(["cargo", "test"]));
    assert_eq!(strings(&preview["reasons"]), ["evidence_unavailable"]);

    // A passed test in open_items is a verified fact like any other.
    let passed = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    ok(
        &fixture.checkpoint_with(
            &fixture.writer,
            &fixture.key(),
            &open_item_claim(&passed, "verified_fact", "tests pass"),
        ),
        op,
        201,
    );
    let preview = fixture.preview(None).1;
    assert_eq!(preview["status"], "verified");
    assert_eq!(preview["facts"][0]["evidence"][0]["run_status"], "PASS");
}

/// T9 S7: an empty commit changes only HEAD; the Preview is stale and the
/// old Evidence cannot complete the Task.
#[test]
fn p2e_an_empty_commit_is_stale_and_refuses_completion() {
    let fixture = Fixture::new();
    let task = fixture.task_id.clone();
    fixture.step(&task, 1, "ready");
    fixture.step(&task, 2, "in_progress");
    let (passed, _) = fixture.checkpointed();
    assert_eq!(fixture.preview(None).1["status"], "verified");

    let head = commit_empty(&fixture.repo);
    let preview = fixture.preview(None).1;
    assert_eq!(preview["status"], "stale");
    assert_eq!(
        strings(&preview["reasons"]),
        ["head_changed", "evidence_stale"]
    );
    assert_eq!(preview["bindings"]["probe"]["head_oid"], head.as_str());
    assert_eq!(strings(&preview["changed_paths"]), Vec::<&str>::new());
    assert_eq!(preview["next_checks"][0]["at_head"], head.as_str());
    problem(
        &fixture.transition_with(&task, &fixture.key(), &completion(3, &[&passed])),
        OperationId::TransitionTask,
        409,
        "evidence_stale",
    );
}

/// `git commit --allow-empty` in `directory`; returns the new HEAD.
fn commit_empty(directory: &std::path::Path) -> String {
    let run = |arguments: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{arguments:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    run(&[
        "-c",
        "user.name=memprove Test",
        "-c",
        "user.email=memprove@example.invalid",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "empty",
    ]);
    run(&["rev-parse", "HEAD"]).trim().to_owned()
}

/// P2-E (T9 S1): every Event type `memproved` actually writes is in the
/// one list `ingestEvent` refuses, `DAEMON_EVENT_TYPES`. Every daemon write
/// is driven once; the list holds nothing the daemon never writes.
#[test]
fn every_event_type_memproved_writes_is_in_the_daemon_list() {
    let fixture = Fixture::new();
    let task = fixture.task_id.clone();
    fixture.step(&task, 1, "ready");
    fixture.step(&task, 2, "in_progress");
    // P4-D (Owner A1): a completed Task takes no new Session, so the Session
    // ended as abandoned below starts before the Task completes. P4-F: it
    // declares a Pack, so memproved writes capsule.recovered.
    let left = fixture.resumed_session(&task, 1);
    let (passed, _) = fixture.checkpointed();
    let linked_path = git_worktree(&fixture.repo, &fixture.repos.path().join("side"), "side");
    ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                None,
                &json!({
                    "schema_version": 1, "project_id": fixture.project_id, "path": linked_path,
                })
                .to_string(),
            ),
        ),
        OperationId::ResolveWorkspace,
        200,
    );
    ok(
        &fixture.transition_with(&task, &fixture.key(), &completion(3, &[&passed])),
        OperationId::TransitionTask,
        200,
    );
    // P3-C: a Decision that supersedes an open item writes decision.recorded
    // and claim.superseded.
    let (_, checkpoint_id) = fixture.checkpointed();
    fixture.decide(
        "use JCS",
        &[&passed],
        &[&format!("{checkpoint_id}/open_items/0")],
    );
    ok(
        &end_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                r#"{"schema_version":1,"expected_revision":1}"#,
            ),
            &fixture.session_id,
        ),
        OperationId::EndSession,
        200,
    );
    // P3-D: a Session ended as abandoned writes session.crashed.
    ok(
        &end_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                r#"{"schema_version":1,"expected_revision":1,"outcome":"abandoned"}"#,
            ),
            &left,
        ),
        OperationId::EndSession,
        200,
    );
    let mut written = std::collections::BTreeSet::new();
    for event in fixture.events() {
        let event_type = event["event_type"].as_str().unwrap().to_owned();
        if event["producer"]["name"] == memprove_domain::DAEMON_PRODUCER_NAME {
            assert!(
                memprove_domain::DAEMON_EVENT_TYPES.contains(&event_type.as_str()),
                "{event_type} is written by memproved but not listed"
            );
            written.insert(event_type);
        } else {
            assert!(
                !memprove_domain::DAEMON_EVENT_TYPES.contains(&event_type.as_str()),
                "{event_type} is listed but came from a client"
            );
        }
    }
    assert_eq!(
        written,
        memprove_domain::DAEMON_EVENT_TYPES
            .iter()
            .map(|event_type| (*event_type).to_owned())
            .collect()
    );
}

/// P2-E (T9 N6): a client collector cannot use memproved's name. P2-K
/// (T11 S-4): nor a look-alike of it, by case or by character; a collector
/// name holds only ASCII letters, digits, `.`, `_`, and `-`.
#[test]
fn capture_refuses_a_collector_named_like_continuityd() {
    let fixture = Fixture::new();
    for name in [
        "continuityd",
        "continuityd-git",
        "continuityd-cli",
        "Continuityd",
        "CONTINUITYD-git",
        " continuityd",
        "c\u{43e}ntinuityd",
        "\u{200b}continuityd",
        "memprove cli",
    ] {
        let mut body = fixture.run_body("test_run", 0, Some(3));
        body["metadata"]["collector"]["name"] = json!(name);
        problem(
            &fixture.capture_with(&fixture.writer, &fixture.key(), &body),
            OperationId::CaptureEvidence,
            400,
            "invalid_request",
        );
    }
}

/// Every file under `directory` with the SHA-256 of its bytes and its
/// modification time in nanoseconds.
fn tree_state(directory: &std::path::Path) -> std::collections::BTreeMap<PathBuf, (String, i128)> {
    let mut state = std::collections::BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path);
            } else {
                let modified = metadata
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let digest =
                    memprove_domain::Sha256Digest::of_bytes(&std::fs::read(&path).unwrap());
                state.insert(
                    path,
                    (digest.to_string(), i128::try_from(modified).unwrap()),
                );
            }
        }
    }
    state
}

/// Makes `path`'s stat data disagree with the index without changing its
/// content: its modification time moves 100 seconds back, so it is older
/// than the index (not racily clean) and a refreshing Git command would
/// rehash it and rewrite the index.
fn make_stat_dirty(path: &std::path::Path) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(100))
        .unwrap();
}

/// P2-E (T9 S2): the Preview and Task completion write nothing under
/// `.git` (the common directory, which holds the linked worktree's
/// administrative directory too), compared by SHA-256 and modification
/// time, even when the index's stat data is out of date; and a file whose
/// stat data alone is out of date is not reported as changed.
#[test]
fn preview_and_completion_leave_every_git_file_unchanged_when_stat_is_stale() {
    let fixture = Fixture::new();
    let task = fixture.task_id.clone();
    fixture.step(&task, 1, "ready");
    fixture.step(&task, 2, "in_progress");
    let (passed, _) = fixture.checkpointed();
    let linked_path = git_worktree(&fixture.repo, &fixture.repos.path().join("side"), "side");
    let linked = ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                None,
                &json!({
                    "schema_version": 1, "project_id": fixture.project_id, "path": linked_path,
                })
                .to_string(),
            ),
        ),
        OperationId::ResolveWorkspace,
        200,
    );
    let linked_id = linked["workspace_id"].as_str().unwrap().to_owned();
    make_stat_dirty(&fixture.repo.join("README"));
    make_stat_dirty(&linked_path.join("README"));
    let git_dir = fixture.repo.join(".git");
    assert!(
        git_dir
            .join("worktrees")
            .join("side")
            .join("index")
            .is_file()
    );
    let before = tree_state(&git_dir);

    for query in [None, Some(format!("workspace_id={linked_id}"))] {
        let preview = fixture.preview(query.as_deref()).1;
        assert_eq!(preview["status"], "verified", "{query:?}: {preview}");
        assert_eq!(strings(&preview["changed_paths"]), Vec::<&str>::new());
    }
    let done = ok(
        &fixture.transition_with(&task, &fixture.key(), &completion(3, &[&passed])),
        OperationId::TransitionTask,
        200,
    );
    assert_eq!(done["state"], "completed");
    assert_eq!(tree_state(&git_dir), before, "a probe wrote under .git");

    // Control: the stat data really was stale, so a plain `git status`
    // (optional locks allowed) rewrites the index.
    let index = git_dir.join("index");
    let index_before = tree_state(&git_dir)[&index].clone();
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&fixture.repo)
        .arg("status")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_ne!(tree_state(&git_dir)[&index], index_before);
}

impl Fixture {
    fn capsule_with(
        &self,
        bearer: Option<&str>,
        task_id: &str,
        query: Option<&str>,
    ) -> WireResponse {
        get_task_capsule(self.state(), bearer, task_id, query)
    }

    fn capsule(&self, query: Option<&str>) -> (String, Value) {
        let response = self.capsule_with(Some(&self.reader), &self.task_id, query);
        let body = ok(&response, OperationId::GetTaskCapsule, 200);
        (response.body, body)
    }

    fn head(&self) -> String {
        String::from_utf8(
            std::process::Command::new("git")
                .args(["-C", self.repo.to_str().unwrap(), "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    fn store(&self) -> memprove_store_sqlite::StateStore {
        let data_dir = std::fs::canonicalize(self.harness.dir.path()).unwrap();
        let registry = memprove_store_sqlite::RegistryStore::open(&data_dir).unwrap();
        let project = registry
            .get_project(&memprove_domain::ProjectId::parse(&self.project_id).unwrap())
            .unwrap();
        memprove_store_sqlite::StateStore::open(&data_dir, &project).unwrap()
    }

    /// Appends an Event of `event_type` on the fixture's Session straight to
    /// the store, as `producer` would have recorded it.
    fn append(&self, producer: &str, event_type: &str, payload: &Value) {
        let mut store = self.store();
        let project_id = memprove_domain::ProjectId::parse(&self.project_id).unwrap();
        let input = memprove_domain::EventInput {
            event_id: memprove_domain::EventId::new(),
            project_id,
            workspace_id: memprove_domain::WorkspaceId::parse(&self.workspace_id).unwrap(),
            session_id: Some(memprove_domain::SessionId::parse(&self.session_id).unwrap()),
            event_type: event_type.to_owned(),
            payload_schema: format!("{event_type}@1"),
            payload: memprove_domain::CanonicalJson::parse(&payload.to_string()).unwrap(),
            occurred_at: None,
            recorded_at: memprove_domain::UtcTimestamp::parse("2026-10-03T10:00:00Z").unwrap(),
            producer: memprove_domain::EventProducer {
                name: producer.to_owned(),
                version: "0.1.0".to_owned(),
                sequence: APPENDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            },
            correlation_id: None,
            causation_id: None,
            privacy_class: memprove_domain::PrivacyClass::Internal,
        };
        store.append_event(&input).unwrap();
    }
}

fn verdicts(evidence: &Value) -> Vec<(String, String, Vec<String>)> {
    evidence
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["evidence_id"].as_str().unwrap().to_owned(),
                item["verdict"].as_str().unwrap().to_owned(),
                strings(&item["reasons"])
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            )
        })
        .collect()
}

/// ADR 0013 D1/D4: the Capsule lists the statements of every Checkpoint of
/// the Task, judged, numbered, and with standing; it writes nothing, one
/// (N, P) gives the same bytes, and the latest Checkpoint's statements get
/// the Preview's Evidence verdicts.
#[test]
#[allow(clippy::too_many_lines)] // Two Checkpoints across a commit, read whole.
fn task_capsule_accumulates_every_checkpoint_and_writes_nothing() {
    let fixture = Fixture::new();
    let base = fixture.head();
    let (first, first_checkpoint) = fixture.checkpointed();
    std::fs::write(fixture.repo.join("README"), b"edited\n").unwrap();
    let head = fixture.commit("change README");
    let (second, second_checkpoint) = fixture.checkpointed();
    let events = fixture.events().len();
    let stored = fixture.state_file();

    let (bytes, capsule) = fixture.capsule(None);
    let claims = capsule["claims"].as_array().unwrap();
    let listed = claims
        .iter()
        .map(|claim| {
            (
                claim["number"].as_u64().unwrap(),
                claim["text"].as_str().unwrap(),
                claim["kind"].as_str().unwrap(),
                claim["standing"].as_str().unwrap(),
                claim["verdict"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        [
            (1, "tests pass", "fact", "active", "stale"),
            (2, "docs not reviewed", "open_item", "active", "partial"),
            (
                3,
                "open a pull request",
                "next_action",
                "superseded",
                "partial"
            ),
            (4, "tests pass", "fact", "active", "verified"),
            (5, "docs not reviewed", "open_item", "active", "partial"),
            (6, "open a pull request", "next_action", "active", "partial"),
        ]
    );
    assert_eq!(
        claims[0]["claim_id"],
        format!("{first_checkpoint}/verified_completed_work/0")
    );
    assert_eq!(claims[3]["checkpoint_id"], second_checkpoint.as_str());
    assert_eq!(claims[0]["evidence"][0]["evidence_id"], first.as_str());
    assert_eq!(claims[0]["evidence"][0]["base_commit"], base.as_str());
    assert_eq!(claims[3]["evidence"][0]["base_commit"], head.as_str());
    assert_eq!(claims[2]["superseded_by"][0]["number"], 6);
    // Both facts are active, so the Task takes the stale one.
    assert_eq!(capsule["status"], "stale");
    assert_eq!(capsule["bindings"]["probe"]["head_oid"], head.as_str());
    assert_eq!(
        capsule["changed_paths_by_base"],
        json!([
            { "base_commit": head, "probed": true, "complete": true, "paths": [] },
            { "base_commit": base, "probed": true, "complete": true, "paths": ["repo://README"] },
        ])
    );
    // The stale `cargo test` has a later verified run: reaffirm, not rerun.
    assert_eq!(capsule["rerun"], json!([]));
    let reaffirm = capsule["continuation"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["template"] == "reaffirm")
        .unwrap();
    assert_eq!(reaffirm["args"]["verified_evidence_id"], second.as_str());
    assert_eq!(reaffirm["args"]["numbers"], json!([1]));
    assert_eq!(capsule["sessions"][0]["end_state"], "open");
    assert_eq!(strings(&capsule["gaps"]), Vec::<&str>::new());

    // JCS bytes; the digest covers the document without itself.
    let canonical = memprove_domain::CanonicalJson::parse(&bytes).unwrap();
    assert_eq!(canonical.as_str(), bytes);
    let mut without = capsule.clone();
    without.as_object_mut().unwrap().remove("capsule_digest");
    let digest = memprove_domain::CanonicalJson::parse(&without.to_string())
        .unwrap()
        .sha256();
    assert_eq!(capsule["capsule_digest"], format!("sha256:{digest}"));
    assert_eq!(fixture.capsule(None).0, bytes);
    assert_eq!(fixture.events().len(), events);
    assert_eq!(
        fixture.state_file(),
        stored,
        "the Capsule wrote to state.sqlite"
    );

    // ADR 0013 D4: under one (N, P), the latest Checkpoint's statements have
    // the Preview's Evidence verdicts, item by item, before and after the
    // worktree changes.
    for edit in [false, true] {
        if edit {
            std::fs::write(fixture.repo.join("README"), b"edited again\n").unwrap();
        }
        let (_, preview) = fixture.preview(None);
        let (_, capsule) = fixture.capsule(None);
        let latest = capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|claim| claim["checkpoint_id"] == second_checkpoint.as_str())
            .collect::<Vec<_>>();
        let facts = preview["facts"].as_array().unwrap();
        // P3-C2: `tampered` in one exactly when in the other.
        assert_eq!(
            preview["status"] == "tampered",
            capsule["status"] == "tampered"
        );
        assert_eq!(latest.len(), facts.len());
        for (claim, fact) in latest.iter().zip(facts) {
            assert_eq!(claim["text"], fact["text"]);
            assert_eq!(verdicts(&claim["evidence"]), verdicts(&fact["evidence"]));
            assert_eq!(claim["verdict"], fact["verdict"]);
            assert_eq!(claim["reasons"], fact["reasons"]);
        }
    }
}

/// ADR 0013 D4: `getTaskCapsule` takes the Preview's input rules and its
/// access rule, `control` or `mcp-read`.
#[test]
#[allow(clippy::too_many_lines)] // Every getTaskCapsule input and credential.
fn task_capsule_refuses_invalid_input_and_enforces_its_access_rule() {
    let fixture = Fixture::new();
    let op = OperationId::GetTaskCapsule;
    let task = fixture.task_id.clone();

    // A Task without a Checkpoint: partial, a header, no claims.
    let (_, empty) = fixture.capsule(None);
    assert_eq!(empty["status"], "partial");
    assert_eq!(strings(&empty["reasons"]), ["evidence_unavailable"]);
    assert_eq!(
        strings(&empty["gaps"]),
        ["no_checkpoint", "no_verified_fact"]
    );
    assert_eq!(empty["claims"], json!([]));
    assert_eq!(empty["continuation"][0]["template"], "header");
    fixture.checkpointed();

    ok(
        &fixture.capsule_with(Some(&fixture.control), &task, None),
        op,
        200,
    );
    ok(
        &fixture.capsule_with(Some(&fixture.reader), &task, None),
        op,
        200,
    );
    problem(
        &fixture.capsule_with(Some(&fixture.writer), &task, None),
        op,
        403,
        "policy_denied",
    );
    for bearer in [None, Some("A".repeat(43))] {
        let response = fixture.capsule_with(bearer.as_deref(), &task, None);
        problem(&response, op, 401, "authentication_required");
        assert!(response.bearer_challenge);
    }

    let workspace = fixture.workspace_id.clone();
    for (task_id, query) in [
        ("not-a-uuid", None),
        ("018f47a0-0000-4000-8000-000000000001", None),
        (task.as_str(), Some("workspace_id=nope".to_owned())),
        (task.as_str(), Some(format!("workspace={workspace}"))),
        (
            task.as_str(),
            Some(format!("workspace_id={workspace}&workspace_id={workspace}")),
        ),
        (
            task.as_str(),
            Some(format!("workspace_id={workspace}&limit=1")),
        ),
        (task.as_str(), Some("workspace_id".to_owned())),
    ] {
        problem(
            &fixture.capsule_with(Some(&fixture.reader), task_id, query.as_deref()),
            op,
            400,
            "invalid_request",
        );
    }
    problem(
        &fixture.capsule_with(Some(&fixture.reader), UNKNOWN, None),
        op,
        404,
        "resource_not_found",
    );
    problem(
        &fixture.capsule_with(
            Some(&fixture.reader),
            &task,
            Some(&format!("workspace_id={UNKNOWN}")),
        ),
        op,
        404,
        "workspace_not_resolved",
    );
    let other_repo = git_repository(fixture.repos.path(), "other");
    let body = json!({ "schema_version": 1, "root": other_repo }).to_string();
    let other = ok(
        &register_project(
            fixture.state(),
            &json_request(Some(&fixture.control), Some(&fixture.key()), &body),
        ),
        OperationId::RegisterProject,
        201,
    );
    let foreign = other["root_workspace_id"].as_str().unwrap();
    problem(
        &fixture.capsule_with(
            Some(&fixture.reader),
            &task,
            Some(&format!("workspace_id={foreign}")),
        ),
        op,
        409,
        "project_workspace_mismatch",
    );
    // The asker's own Workspace, named.
    let (_, named) = fixture.capsule(Some(&format!("workspace_id={workspace}")));
    assert_eq!(
        named["bindings"]["probe"]["workspace_id"],
        workspace.as_str()
    );
    assert_eq!(named["status"], "verified");
}

/// ADR 0013 D8: `claim.superseded`, `decision.recorded`, and
/// `session.crashed` count only when `memproved` wrote them; same-named
/// Events a client wrote before they were reserved are ignored.
#[test]
fn task_capsule_reads_reserved_types_only_from_memproved() {
    let fixture = Fixture::new();
    let (_, checkpoint) = fixture.checkpointed();
    let open_item = format!("{checkpoint}/open_items/0");
    let edge = json!({
        "task_id": fixture.task_id, "relation": "withdraws", "target": open_item, "by": null,
    });
    let decision = json!({
        "task_id": fixture.task_id, "text": "use JCS for every digest", "rationale": null,
        "evidence_ids": [],
    });
    let crash = json!({ "reason": "agent lost", "outcome": "abandoned" });
    for (event_type, payload) in [
        ("claim.superseded", &edge),
        ("decision.recorded", &decision),
        ("session.crashed", &crash),
    ] {
        fixture.append("agent-cli", event_type, payload);
    }
    let (_, ignored) = fixture.capsule(None);
    assert_eq!(ignored["claims"].as_array().unwrap().len(), 3);
    assert_eq!(ignored["claims"][1]["standing"], "active");
    assert_eq!(ignored["sessions"][0]["end_state"], "open");

    for (event_type, payload) in [
        ("claim.superseded", &edge),
        ("decision.recorded", &decision),
        ("session.crashed", &crash),
    ] {
        fixture.append(memprove_domain::DAEMON_PRODUCER_NAME, event_type, payload);
    }
    let (_, read) = fixture.capsule(None);
    let claims = read["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 4);
    assert_eq!(claims[1]["standing"], "withdrawn");
    assert_eq!(claims[3]["kind"], "decision");
    assert_eq!(claims[3]["text"], "use JCS for every digest");
    assert_eq!(claims[3]["verdict"], "partial");
    assert_eq!(strings(&claims[3]["reasons"]), ["evidence_unavailable"]);
    assert!(
        claims[3]["claim_id"]
            .as_str()
            .unwrap()
            .starts_with("decision/")
    );
    assert_eq!(read["sessions"][0]["end_state"], "crashed");
    // The withdrawn open item moved to class 9; the Task verdict is the
    // fact's, unchanged.
    assert_eq!(read["status"], "verified");
    let withdrawn = read["continuation"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["template"] == "withdrawn")
        .unwrap();
    assert_eq!(withdrawn["args"]["number"], 2);
    assert_eq!(withdrawn["class"], 9);
}

mod carry_over_tests;
mod claim_edge_tests;
mod integrity_tests;
mod p3d2_tests;
mod p3d3_tests;
mod p3l_tests;
mod p4b_tests;
mod p4c2_tests;
mod p4d_tests;
mod p4h1_review_tests;
mod p4h1_tests;
mod session_outcome_tests;
mod t21_tests;
mod u1b_tests;
mod u1c_tests;
mod u3k_tests;
mod u5p_tests;
mod u9_k2_tests;
