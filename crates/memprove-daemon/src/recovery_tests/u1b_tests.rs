//! Narrow tests of stage U-B of U1 (the U1 task plan,
//! section 4.1; ADR 0014, "Amendment (U1, U-B)"): integrity and storage.
//!
//! - B3 (T21 V1R-4): a Snapshot's `is_dirty` outside 0 and 1 does not
//!   decode, also on a dirty Snapshot (before: read as dirty, like 1).
//! - B2 (T21 V1R-3): a Task row that no Task Event names is compared with
//!   the chain, not only counted: the chain does not record it.
//! - B1 (T21 V1R-1): the ten columns ADR 0014's "Columns no check reads"
//!   still left out, each changed to another value of its form, change a
//!   verdict: `verifyLedger` names the record, or the Task Capsule and the
//!   Preview are `tampered` with the record among the damaged ones.
//! - B4 (original #32, Owner O5): the root Workspace is the Workspace of
//!   the repository the registry records that no `workspace.registered`
//!   Event announces, no longer the first by `created_at`; when its row is
//!   missing, `verifyLedger` names a `workspace` `first_failure`.
//! - B5 (T21 V1-N-3): a registry Project record that does not decode (its
//!   revision 0, its `created_at` not UTF-8) makes the Project's reads
//!   `409 capsule_tampered` with a body, as a state database not bound to
//!   its Project does; a write keeps `503`.
//! - B6 (T21 V1R-5): what a changed Session row `ended_at` does, which the
//!   corrected ADR sentence states.
//!
//! Every tamper is a writer that bypasses `memproved`: `sqlite3` on the
//! state database with the append-only triggers dropped and foreign keys
//! off; CHECK constraints are off where the case says so.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{
    Fixture,
    integrity_tests::{Ledger, assert_tampered_without_verified, quote},
    ok,
    p4h1_review_tests::{assert_tampered, other_project, project_reads},
    problem,
};
use crate::{
    catalog::{
        TAMPERED_RECORD, UNBOUND_STATE, create_session, create_task, get_project, list_sessions,
        register_project, resolve_workspace,
    },
    events::ingest_event,
    test_support::{git_repository, git_worktree, json_request},
};

/// `verifyLedger` of the fixture's Project, which must answer `200`.
fn verified(fixture: &Fixture) -> Value {
    ok(
        &fixture.verify_with(
            Some(&fixture.reader),
            Some(&fixture.key()),
            &fixture.project_id,
        ),
        OperationId::VerifyLedger,
        200,
    )
}

/// Asserts that `verifyLedger` names `record_id` of `record_type` as its
/// first failure.
fn assert_first_failure(fixture: &Fixture, record_type: &str, record_id: &str, case: &str) {
    let report = verified(fixture);
    assert_eq!(report["valid"], false, "{case}: {report}");
    assert_eq!(
        report["first_failure"]["record_type"], record_type,
        "{case}: {report}"
    );
    assert_eq!(
        report["first_failure"]["record_id"], record_id,
        "{case}: {report}"
    );
}

/// Asserts that the Task Capsule names `record_id` of `record_type` among
/// its damaged records.
fn assert_damaged(fixture: &Fixture, record_type: &str, record_id: &str, case: &str) {
    let (bytes, capsule) = fixture.capsule(None);
    assert_eq!(capsule["status"], "tampered", "{case}: {bytes}");
    assert!(
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["record_type"] == record_type && record["record_id"] == record_id),
        "{case}: {bytes}"
    );
}

/// B3 (T21 V1R-4): `is_dirty` of a dirty Snapshot (its porcelain status is
/// not empty) set to 2, -1, 2^53, or `i64::MAX`, the column's CHECK
/// bypassed, is a `snapshot` `first_failure` of `verifyLedger` and a
/// damaged record of the Task Capsule and the Preview (before: `valid`
/// true, the value read as dirty, like 1).
#[test]
fn b3_an_is_dirty_outside_zero_and_one_does_not_decode_on_a_dirty_snapshot() {
    for value in ["2", "-1", "9007199254740992", "9223372036854775807"] {
        let fixture = Fixture::new();
        std::fs::write(fixture.repo.join("untracked.txt"), "dirty").unwrap();
        let run = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
        let snapshot = run["snapshot"]["snapshot_id"].as_str().unwrap().to_owned();
        assert_eq!(
            fixture.column(&format!(
                "SELECT is_dirty FROM repository_snapshots WHERE snapshot_id = {};",
                quote(&snapshot)
            )),
            ["1"]
        );
        assert!(fixture.ledger_valid(), "{value}: intact ledger");
        fixture.tamper(&format!(
            "PRAGMA ignore_check_constraints = ON; UPDATE repository_snapshots \
             SET is_dirty = {value} WHERE snapshot_id = {};",
            quote(&snapshot)
        ));
        let case = format!("is_dirty = {value}");
        assert_first_failure(&fixture, "snapshot", &snapshot, &case);
        assert_damaged(&fixture, "snapshot", &snapshot, &case);
        assert_tampered_without_verified(&fixture, &case);
    }
}

/// B2 (T21 V1R-3): a copy of the fixture's Task row under a new identity,
/// which no `task.created` or `task.transitioned` Event names, is a `task`
/// `first_failure` of `verifyLedger`, and its Capsule and Preview are
/// `tampered` with the Task as a damaged record (before: `valid` true, the
/// row counted; its Capsule `partial`, no damaged record).
#[test]
fn b2_a_task_row_no_task_event_names_is_a_failure() {
    let fixture = Fixture::new();
    assert!(fixture.ledger_valid());
    let injected = uuid::Uuid::now_v7().to_string();
    fixture.tamper(&format!(
        "INSERT INTO tasks (task_id, project_id, title, status, created_at, updated_at, \
                            revision, workspace_id, description) \
         SELECT {}, project_id, 'injected', status, created_at, updated_at, revision, \
                workspace_id, description \
         FROM tasks WHERE task_id = {};",
        quote(&injected),
        quote(&fixture.task_id)
    ));
    let report = verified(&fixture);
    assert_eq!(report["valid"], false, "{report}");
    assert_eq!(report["first_failure"]["record_type"], "task", "{report}");
    assert_eq!(report["first_failure"]["record_id"], injected, "{report}");
    assert_eq!(
        report["first_failure"]["project_sequence"],
        Value::Null,
        "{report}"
    );
    assert_eq!(report["checked"]["tasks"], 2, "{report}");
    let capsule = ok(
        &fixture.capsule_with(Some(&fixture.reader), &injected, None),
        OperationId::GetTaskCapsule,
        200,
    );
    assert_eq!(capsule["status"], "tampered", "{capsule}");
    assert!(
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["record_type"] == "task" && record["record_id"] == injected),
        "{capsule}"
    );
    let preview = ok(
        &fixture.preview_with(Some(&fixture.reader), &injected, None),
        OperationId::GetRecoveryPreview,
        200,
    );
    assert_eq!(preview["status"], "tampered", "{preview}");
    // The fixture's own Task is not touched.
    let (_, own) = fixture.capsule(None);
    assert_ne!(own["status"], "tampered", "{own}");
}

/// Registers the linked worktree `path` of the fixture's Project as a
/// Workspace (`resolveWorkspace`, which writes `workspace.registered`);
/// returns its identity.
fn resolve(fixture: &Fixture, project_id: &str, path: &std::path::Path) -> String {
    let body = json!({ "schema_version": 1, "project_id": project_id, "path": path });
    ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(Some(&fixture.writer), None, &body.to_string()),
        ),
        OperationId::ResolveWorkspace,
        200,
    )["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The `root_workspace_id` `getProject` answers for `project_id`.
fn root_of(fixture: &Fixture, project_id: &str) -> String {
    ok(
        &get_project(fixture.state(), Some(&fixture.reader), project_id),
        OperationId::GetProject,
        200,
    )["root_workspace_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `verifyLedger` of `project_id`, which must answer `200`.
fn verified_project(fixture: &Fixture, project_id: &str) -> Value {
    ok(
        &fixture.verify_with(Some(&fixture.reader), Some(&fixture.key()), project_id),
        OperationId::VerifyLedger,
        200,
    )
}

/// B4 (original #32, O5): with a linked worktree registered next to the
/// root, the root Workspace is the one `registerProject` wrote, also when
/// the linked row's `created_at` is earlier (before: the first row by
/// `created_at`, so `getProject` named the linked Workspace). With the
/// root's row deleted, `getProject` and an unscoped `createTask` answer
/// `409 capsule_tampered`, and `verifyLedger` names a `workspace`
/// `first_failure` with the root's identity as its unscoped Task Event
/// recorded it (before: `getProject` named the linked Workspace as the
/// root, `createTask` recorded under it, and the ledger was `valid`).
#[test]
fn b4_the_root_workspace_is_not_chosen_by_created_at_and_a_missing_root_is_reported() {
    let fixture = Fixture::new();
    let root = fixture.workspace_id.clone();
    // An unscoped Task, whose Task Events are recorded under the root.
    let body = json!({
        "schema_version": 1, "project_id": fixture.project_id, "title": "unscoped",
    });
    ok(
        &create_task(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &body.to_string(),
            ),
        ),
        OperationId::CreateTask,
        201,
    );
    let linked_path = git_worktree(
        &fixture.repo,
        &fixture.repos.path().join("linked"),
        "linked",
    );
    let linked = resolve(&fixture, &fixture.project_id, &linked_path);
    assert_eq!(root_of(&fixture, &fixture.project_id), root);

    fixture.tamper(&format!(
        "UPDATE workspaces SET created_at = '2000-01-01T00:00:00Z' WHERE workspace_id = {};",
        quote(&linked)
    ));
    assert_eq!(
        root_of(&fixture, &fixture.project_id),
        root,
        "created_at moved"
    );
    assert!(
        fixture.ledger_valid(),
        "a moved created_at is no finding of B4"
    );

    fixture.tamper(&format!(
        "DELETE FROM workspaces WHERE workspace_id = {};",
        quote(&root)
    ));
    assert_tampered(
        "getProject",
        OperationId::GetProject,
        &get_project(fixture.state(), Some(&fixture.reader), &fixture.project_id),
        TAMPERED_RECORD,
    );
    assert_tampered(
        "createTask",
        OperationId::CreateTask,
        &create_task(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &body.to_string(),
            ),
        ),
        TAMPERED_RECORD,
    );
    let report = verified_project(&fixture, &fixture.project_id);
    assert_eq!(report["valid"], false, "{report}");
    let failure = &report["first_failure"];
    assert_eq!(failure["record_type"], "workspace", "{report}");
    assert_eq!(failure["record_id"], root.as_str(), "{report}");
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .starts_with("record is missing"),
        "{report}"
    );
    // The linked Workspace stays what it was.
    assert_eq!(
        fixture.column(&format!(
            "SELECT count(*) FROM workspaces WHERE workspace_id = {};",
            quote(&linked)
        )),
        ["1"]
    );
}

/// B4: a Project registered from a linked worktree keeps that worktree as
/// its root when the repository's main worktree is registered later (the
/// main worktree's Git directory is the registry's common directory, but
/// `resolveWorkspace` announced it), and its missing root row is reported
/// as for any other Project.
#[test]
fn b4_a_project_registered_from_a_linked_worktree_keeps_that_root() {
    let fixture = Fixture::new();
    let main = git_repository(fixture.repos.path(), "second");
    let linked_path = git_worktree(&main, &fixture.repos.path().join("second-linked"), "lk");
    let body = json!({ "schema_version": 1, "root": linked_path }).to_string();
    let registered = ok(
        &register_project(
            fixture.state(),
            &json_request(Some(&fixture.control), Some(&fixture.key()), &body),
        ),
        OperationId::RegisterProject,
        201,
    );
    let project = registered["project_id"].as_str().unwrap().to_owned();
    let root = registered["root_workspace_id"].as_str().unwrap().to_owned();
    let main_id = resolve(&fixture, &project, &main);
    assert_ne!(main_id, root);
    assert_eq!(root_of(&fixture, &project), root);
    assert_eq!(verified_project(&fixture, &project)["valid"], true);

    let database = std::fs::canonicalize(fixture.harness.dir.path())
        .unwrap()
        .join("projects")
        .join(&project)
        .join("state.sqlite");
    let output = std::process::Command::new("sqlite3")
        .arg(&database)
        .arg(format!(
            "PRAGMA foreign_keys = OFF; DELETE FROM workspaces WHERE workspace_id = {};",
            quote(&root)
        ))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_tampered(
        "getProject",
        OperationId::GetProject,
        &get_project(fixture.state(), Some(&fixture.reader), &project),
        TAMPERED_RECORD,
    );
    let report = verified_project(&fixture, &project);
    assert_eq!(report["valid"], false, "{report}");
    assert_eq!(
        report["first_failure"]["record_type"], "workspace",
        "{report}"
    );
}

/// Another Project identity, of the form every `project_id` column holds.
const OTHER_PROJECT: &str = "01a11a00-0000-7000-8000-000000000001";

/// Another time, of the form every time column holds.
const OTHER_TIME: &str = "2000-01-01T00:00:00Z";

/// B1: runs `sql` on a fresh verified ledger and returns it.
fn b1_ledger(sql: impl Fn(&Ledger) -> String) -> Ledger {
    let ledger = Ledger::new();
    ledger.fixture.tamper(&sql(&ledger));
    ledger
}

/// B1: the Task Capsule and the Preview are `tampered` (before: `verified`)
/// and the Capsule names `record_id` of `record_type` as damaged.
fn assert_b1_documents(ledger: &Ledger, record_type: &str, record_id: &str, case: &str) {
    let fixture = &ledger.fixture;
    assert_damaged(fixture, record_type, record_id, case);
    let (bytes, preview) = fixture.preview(None);
    assert_eq!(preview["status"], "tampered", "{case}: {bytes}");
    assert!(!bytes.contains("\"verified\""), "{case}: {bytes}");
}

/// B1 `tasks.project_id`: another Project's identity, which the schema's
/// insert trigger refuses, is a `task` `first_failure` and makes the Task
/// a damaged record (before: `valid`; only `listTasks` showed it).
#[test]
fn b1_tasks_project_id() {
    let ledger = b1_ledger(|ledger| {
        format!(
            "UPDATE tasks SET project_id = {} WHERE task_id = {};",
            quote(OTHER_PROJECT),
            quote(&ledger.fixture.task_id)
        )
    });
    let task = ledger.fixture.task_id.clone();
    assert_first_failure(&ledger.fixture, "task", &task, "tasks.project_id");
    assert_b1_documents(&ledger, "task", &task, "tasks.project_id");
}

/// B1 `tasks.created_at`: a time other than the `recorded_at` of the
/// Task's `task.created` Event (before: no output changed).
#[test]
fn b1_tasks_created_at() {
    let ledger = b1_ledger(|ledger| {
        format!(
            "UPDATE tasks SET created_at = {} WHERE task_id = {};",
            quote(OTHER_TIME),
            quote(&ledger.fixture.task_id)
        )
    });
    let task = ledger.fixture.task_id.clone();
    assert_first_failure(&ledger.fixture, "task", &task, "tasks.created_at");
    assert_b1_documents(&ledger, "task", &task, "tasks.created_at");
}

/// B1 `tasks.updated_at`: a time other than the `recorded_at` of the
/// Task's last Task Event (before: no output changed).
#[test]
fn b1_tasks_updated_at() {
    let ledger = b1_ledger(|ledger| {
        format!(
            "UPDATE tasks SET updated_at = {} WHERE task_id = {};",
            quote(OTHER_TIME),
            quote(&ledger.fixture.task_id)
        )
    });
    let task = ledger.fixture.task_id.clone();
    assert_first_failure(&ledger.fixture, "task", &task, "tasks.updated_at");
    assert_b1_documents(&ledger, "task", &task, "tasks.updated_at");
}

/// B1 `workspaces.project_id`: another Project's identity is a
/// `workspace` `first_failure` (before: no output changed). The Task
/// Capsule names no Workspace record (P4-H1, T20 SF-1).
#[test]
fn b1_workspaces_project_id() {
    let ledger = b1_ledger(|ledger| {
        format!(
            "UPDATE workspaces SET project_id = {} WHERE workspace_id = {};",
            quote(OTHER_PROJECT),
            quote(&ledger.fixture.workspace_id)
        )
    });
    let workspace = ledger.fixture.workspace_id.clone();
    assert_first_failure(
        &ledger.fixture,
        "workspace",
        &workspace,
        "workspaces.project_id",
    );
}

/// B1: the first Session of the Task, as the Capsule lists it.
fn first_session(ledger: &Ledger) -> String {
    let (_, capsule) = ledger.fixture.capsule(None);
    capsule["sessions"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// B1 `sessions.started_at`: a time other than the `recorded_at` of the
/// Session's `session.started` Event makes the Session a damaged record
/// (before: no output changed; the Capsule reads the start from the chain).
#[test]
fn b1_sessions_started_at() {
    let ledger = Ledger::new();
    let session = first_session(&ledger);
    ledger.fixture.tamper(&format!(
        "UPDATE sessions SET started_at = {} WHERE session_id = {};",
        quote(OTHER_TIME),
        quote(&session)
    ));
    assert_b1_documents(&ledger, "session", &session, "sessions.started_at");
}

/// B1 `sessions.agent_id`: an agent other than the one the Session's
/// `session.started` Event records (before: only `listSessions` showed it).
#[test]
fn b1_sessions_agent_id() {
    let ledger = Ledger::new();
    let session = first_session(&ledger);
    ledger.fixture.tamper(&format!(
        "UPDATE sessions SET agent_id = {} WHERE session_id = {};",
        quote(OTHER_PROJECT),
        quote(&session)
    ));
    assert_b1_documents(&ledger, "session", &session, "sessions.agent_id");
}

/// B1 `sessions.workspace_id`: another Workspace of the Project than the
/// one the Session's `session.started` Event was recorded in (before: only
/// `listSessions` showed it).
#[test]
fn b1_sessions_workspace_id() {
    let ledger = Ledger::new();
    let linked_path = git_worktree(
        &ledger.fixture.repo,
        &ledger.fixture.repos.path().join("linked"),
        "linked",
    );
    let linked = resolve(&ledger.fixture, &ledger.fixture.project_id, &linked_path);
    let session = first_session(&ledger);
    ledger.fixture.tamper(&format!(
        "UPDATE sessions SET workspace_id = {} WHERE session_id = {};",
        quote(&linked),
        quote(&session)
    ));
    assert_b1_documents(&ledger, "session", &session, "sessions.workspace_id");
}

/// B1 `task_sessions.linked_at`: a time other than the `recorded_at` of the
/// link's `task.session_linked` Event (before: no output changed).
#[test]
fn b1_task_sessions_linked_at() {
    let ledger = Ledger::new();
    let session = first_session(&ledger);
    ledger.fixture.tamper(&format!(
        "UPDATE task_sessions SET linked_at = {} WHERE session_id = {};",
        quote(OTHER_TIME),
        quote(&session)
    ));
    assert_b1_documents(&ledger, "session", &session, "task_sessions.linked_at");
}

/// B1 `repository_snapshots.project_id`: another Project's identity, which
/// the schema's insert trigger refuses, does not decode (before: no output
/// changed).
#[test]
fn b1_repository_snapshots_project_id() {
    let ledger = b1_ledger(|ledger| {
        format!(
            "UPDATE repository_snapshots SET project_id = {} WHERE snapshot_id = {};",
            quote(OTHER_PROJECT),
            quote(&ledger.cp3.snapshot)
        )
    });
    let snapshot = ledger.cp3.snapshot.clone();
    assert_first_failure(
        &ledger.fixture,
        "snapshot",
        &snapshot,
        "repository_snapshots.project_id",
    );
    assert_b1_documents(
        &ledger,
        "snapshot",
        &snapshot,
        "repository_snapshots.project_id",
    );
}

/// B1 `repository_snapshots.captured_at`: a time other than the
/// `recorded_at` of the first Event of `memproved` that records the
/// Snapshot (before: only `getEvidence` and `getCheckpoint` showed it).
#[test]
fn b1_repository_snapshots_captured_at() {
    let ledger = b1_ledger(|ledger| {
        format!(
            "UPDATE repository_snapshots SET captured_at = {} WHERE snapshot_id = {};",
            quote(OTHER_TIME),
            quote(&ledger.cp3.snapshot)
        )
    });
    let snapshot = ledger.cp3.snapshot.clone();
    assert_first_failure(
        &ledger.fixture,
        "snapshot",
        &snapshot,
        "repository_snapshots.captured_at",
    );
    assert_b1_documents(
        &ledger,
        "snapshot",
        &snapshot,
        "repository_snapshots.captured_at",
    );
}

/// B5 (T21 V1-N-3): runs `sql` with `sqlite3` on the registry database,
/// CHECK constraints off: a writer that bypasses `memproved`.
fn tamper_registry(fixture: &Fixture, sql: &str) {
    let registry = std::fs::canonicalize(fixture.harness.dir.path())
        .unwrap()
        .join("registry.sqlite");
    let output = std::process::Command::new("sqlite3")
        .arg(registry)
        .arg(format!("PRAGMA ignore_check_constraints = ON; {sql}"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{sql}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// B5 (T21 V1-N-3): the fixture's registry record with revision 0, or a
/// `created_at` that is not UTF-8, does not decode. Every read of the
/// Project answers `409 capsule_tampered` with a body, not retryable, with
/// the fixed detail of a state database not bound to its Project (before:
/// `getProject` of revision 0 answered `500` without a body, which the CLI
/// reported with exit 4, and the other reads `200`; with `created_at` not
/// UTF-8 every read answered `503 service_not_ready`). A write keeps `503`
/// (O7), and another Project still reads.
#[test]
fn b5_a_registry_record_that_does_not_decode_is_capsule_tampered_with_a_body() {
    for (case, value) in [
        ("revision 0", "revision = 0"),
        ("created_at not UTF-8", "created_at = CAST(X'FF' AS TEXT)"),
    ] {
        let ledger = Ledger::new();
        let fixture = &ledger.fixture;
        let (other, _) = other_project(fixture);
        let event = fixture.column("SELECT event_id FROM events ORDER BY rowid LIMIT 1")[0].clone();
        tamper_registry(
            fixture,
            &format!(
                "UPDATE projects SET {value} WHERE project_id = {};",
                quote(&fixture.project_id)
            ),
        );
        for (read, operation, response) in &project_reads(&ledger, &event) {
            assert_tampered(
                &format!("{case}: {read}"),
                *operation,
                response,
                UNBOUND_STATE,
            );
        }
        let body = json!({
            "schema_version": 1, "workspace_id": fixture.workspace_id, "agent_id": super::AGENT,
            "task_id": fixture.task_id,
        });
        problem(
            &create_session(
                fixture.state(),
                &json_request(
                    Some(&fixture.writer),
                    Some(&fixture.key()),
                    &body.to_string(),
                ),
            ),
            OperationId::CreateSession,
            503,
            "service_not_ready",
        );
        ok(
            &get_project(fixture.state(), Some(&fixture.reader), &other),
            OperationId::GetProject,
            200,
        );
    }
}

/// B6 (T21 V1R-5): a Session row's `ended_at` is read by the writes, not
/// by the documents. Set on a Session the chain holds open, the Task
/// Capsule and the Preview do not change and `listSessions` shows the
/// Session ended, while an Event on it is refused `409
/// invalid_transition`; moved from one time to another on an ended Session
/// it shows nowhere.
#[test]
fn b6_a_session_row_s_ended_at_is_read_by_the_writes_only() {
    let fixture = Fixture::new();
    let (capsule_before, _) = fixture.capsule(None);
    let (preview_before, _) = fixture.preview(None);
    fixture.tamper(&format!(
        "UPDATE sessions SET ended_at = '2030-01-01T00:00:00Z' WHERE session_id = {};",
        quote(&fixture.session_id)
    ));
    assert_eq!(fixture.capsule(None).0, capsule_before);
    assert_eq!(fixture.preview(None).0, preview_before);
    assert!(fixture.ledger_valid());
    let query = format!("workspace_id={}", fixture.workspace_id);
    let listed = ok(
        &list_sessions(fixture.state(), Some(&fixture.reader), Some(&query)),
        OperationId::ListSessions,
        200,
    );
    let item = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["session_id"] == fixture.session_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(item["state"], "ended", "{listed}");
    let envelope = json!({
        "envelope_version": "1.0",
        "event_id": uuid::Uuid::now_v7().to_string(),
        "project_id": fixture.project_id,
        "workspace_id": fixture.workspace_id,
        "session_id": fixture.session_id,
        "event_type": "turn.started",
        "payload_schema": "turn.started@1",
        "producer": { "name": "agent", "version": "1", "sequence": 1 },
        "privacy_class": "internal",
        "payload": {},
    });
    problem(
        &ingest_event(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &envelope.to_string(),
            ),
        ),
        OperationId::IngestEvent,
        409,
        "invalid_transition",
    );

    // An ended Session's end moved: nothing a read returns changes.
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let ended = first_session(&ledger);
    let (capsule_before, _) = fixture.capsule(None);
    let listed = |fixture: &Fixture| {
        let query = format!("workspace_id={}", fixture.workspace_id);
        list_sessions(fixture.state(), Some(&fixture.reader), Some(&query)).body
    };
    let sessions_before = listed(fixture);
    fixture.tamper(&format!(
        "UPDATE sessions SET ended_at = '2030-01-01T00:00:00Z' WHERE session_id = {};",
        quote(&ended)
    ));
    assert_eq!(fixture.capsule(None).0, capsule_before);
    assert_eq!(listed(fixture), sessions_before);
    assert!(fixture.ledger_valid());
}
