//! Narrow tests of the review of the first P4-H1 commit (Owner decisions
//! P4-H1: no read of the Project ledger answers `5xx` because a row of it
//! is missing or does not decode; `503` stays for what a retry can cure):
//!
//! - a stored value that decodes as its SQL type but not as what its column
//!   holds: a zero revision or Project sequence, text whose bytes are not
//!   UTF-8, an empty identity;
//! - `resolveWorkspace` at a path whose Workspace row does not decode;
//! - `listSessions` of a Workspace whose row is missing (T20 SF-1);
//! - a state database that is not bound to its Project.
//!
//! Every tamper is a writer that bypasses `memproved`: `sqlite3` on the
//! state database with every trigger dropped, foreign keys and CHECK
//! constraints off.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, UNKNOWN, integrity_tests::Ledger, integrity_tests::quote, ok, problem};
use crate::{
    api::WireResponse,
    catalog::{
        TAMPERED_RECORD, UNBOUND_STATE, create_task, get_project, list_sessions, list_tasks,
        register_project, resolve_workspace,
    },
    events::{get_event, list_events},
    recovery::{get_checkpoint, get_evidence},
    test_support::{git_repository, git_worktree, json_request},
};

/// `409 capsule_tampered`, not retryable, with `detail`.
pub(super) fn assert_tampered(
    case: &str,
    operation: OperationId,
    response: &WireResponse,
    detail: &str,
) {
    assert_eq!(response.status, 409, "{case}: {}", response.body);
    response.validate(Some(operation)).unwrap();
    let body: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["code"], "capsule_tampered", "{case}");
    assert_eq!(body["retryable"], false, "{case}");
    assert_eq!(body["detail"], detail, "{case}");
}

/// The Capsule of `task`, which must answer `200` within its contract.
fn capsule(fixture: &Fixture, task: &str) -> Value {
    ok(
        &fixture.capsule_with(Some(&fixture.reader), task, None),
        OperationId::GetTaskCapsule,
        200,
    )
}

/// The Preview of `task`, which must answer `200` within its contract.
fn preview(fixture: &Fixture, task: &str) -> Value {
    ok(
        &fixture.preview_with(Some(&fixture.reader), task, None),
        OperationId::GetRecoveryPreview,
        200,
    )
}

/// The ledger check, which must answer `200` within its contract.
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

/// Whether `document` names the damaged record.
fn names(document: &Value, record_type: &str, record_id: &str, reason: &str) -> bool {
    document["damaged_records"]
        .as_array()
        .unwrap()
        .contains(&json!({ "record_type": record_type, "record_id": record_id, "reason": reason }))
}

/// A zero revision of a Task or a Session row, which no row the store
/// wrote holds, does not decode: a list page that holds it is
/// `409 capsule_tampered`, and the Capsule names the row as damaged. A
/// zero Project sequence is a `first_failure` with no sequence. None of
/// them is rendered, so no response leaves its contract (a bodiless `500`
/// before).
#[test]
fn a_zero_revision_or_project_sequence_does_not_decode() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let pristine = std::fs::read(fixture.database()).unwrap();
    let (task, session) = (fixture.task_id.as_str(), fixture.session_id.as_str());

    fixture.corrupt(&format!(
        "UPDATE tasks SET revision = 0 WHERE task_id = {};",
        quote(task)
    ));
    let query = format!("project_id={}", fixture.project_id);
    assert_tampered(
        "a Task revision 0",
        OperationId::ListTasks,
        &list_tasks(fixture.state(), Some(&fixture.reader), Some(&query)),
        TAMPERED_RECORD,
    );
    let document = capsule(fixture, task);
    assert_eq!(document["status"], "tampered", "{document}");
    assert!(
        names(&document, "task", task, "object_digest_mismatch"),
        "{document}"
    );
    assert_eq!(preview(fixture, task)["status"], "tampered");

    std::fs::write(fixture.database(), &pristine).unwrap();
    fixture.corrupt(&format!(
        "UPDATE sessions SET revision = 0 WHERE session_id = {};",
        quote(session)
    ));
    let query = format!("workspace_id={}", fixture.workspace_id);
    assert_tampered(
        "a Session revision 0",
        OperationId::ListSessions,
        &list_sessions(fixture.state(), Some(&fixture.reader), Some(&query)),
        TAMPERED_RECORD,
    );
    let document = capsule(fixture, task);
    assert_eq!(document["status"], "tampered", "{document}");
    assert!(
        names(&document, "session", session, "object_digest_mismatch"),
        "{document}"
    );

    std::fs::write(fixture.database(), &pristine).unwrap();
    fixture.corrupt(
        "UPDATE events SET project_sequence = 0 WHERE rowid = (SELECT MIN(rowid) FROM events);",
    );
    let checked = verified(fixture);
    assert_eq!(checked["valid"], false, "{checked}");
    assert_eq!(
        checked["first_failure"]["record_type"], "event",
        "{checked}"
    );
    assert_eq!(
        checked["first_failure"]["project_sequence"],
        Value::Null,
        "{checked}"
    );
    assert_eq!(capsule(fixture, task)["status"], "tampered");
}

/// Text whose bytes are not UTF-8 in a column a read walks in bulk is a
/// `first_failure` of the ledger check (not `503`) and makes the Capsule
/// and the Preview `tampered` (not `409`).
#[test]
fn text_that_is_not_utf8_is_a_finding_or_a_damaged_record() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let task = fixture.task_id.clone();
    let pristine = std::fs::read(fixture.database()).unwrap();
    let not_utf8 = "CAST(X'FF' AS TEXT)";
    let cases = [
        (
            "a command.completed payload",
            format!(
                "UPDATE events SET payload = {not_utf8} WHERE rowid = (SELECT MIN(rowid) \
                 FROM events WHERE event_type = 'command.completed');"
            ),
            "event",
        ),
        (
            "an Evidence identity",
            format!(
                "UPDATE evidence SET evidence_id = {not_utf8} WHERE evidence_id = {};",
                quote(&ledger.cp1.evidence)
            ),
            "evidence",
        ),
        (
            "a Snapshot identity",
            format!(
                "UPDATE repository_snapshots SET snapshot_id = {not_utf8} \
                 WHERE snapshot_id = {};",
                quote(&ledger.cp1.snapshot)
            ),
            "snapshot",
        ),
        (
            "a Checkpoint identity",
            format!(
                "UPDATE checkpoints SET checkpoint_id = {not_utf8} WHERE checkpoint_id = {};",
                quote(&ledger.cp1.checkpoint)
            ),
            "checkpoint",
        ),
        (
            "a Task status",
            format!(
                "UPDATE tasks SET status = {not_utf8} WHERE task_id = {};",
                quote(&task)
            ),
            "task",
        ),
    ];
    for (case, sql, record_type) in &cases {
        std::fs::write(fixture.database(), &pristine).unwrap();
        fixture.corrupt(sql);
        let checked = verified(fixture);
        assert_eq!(checked["valid"], false, "{case}: {checked}");
        assert_eq!(
            checked["first_failure"]["record_type"], *record_type,
            "{case}: {checked}"
        );
        assert_eq!(capsule(fixture, &task)["status"], "tampered", "{case}");
        assert_eq!(preview(fixture, &task)["status"], "tampered", "{case}");
    }
}

/// A Task row whose `task_id` does not decode is a `first_failure` of the
/// ledger check (SF-2: a row the check reads that does not decode), even
/// though no Event names that identity.
#[test]
fn a_task_identity_that_does_not_decode_is_a_first_failure() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let other = fixture.task(None);
    assert_eq!(verified(fixture)["valid"], true);
    fixture.corrupt(&format!(
        "UPDATE tasks SET task_id = 'x' WHERE task_id = {};",
        quote(&other)
    ));
    let checked = verified(fixture);
    assert_eq!(
        checked["first_failure"],
        json!({
            "record_type": "task",
            "record_id": "x",
            "project_sequence": null,
            "reason": "task_id must be a UUIDv7",
        }),
        "{checked}"
    );
}

/// A damaged record names its record in plain printable ASCII: an empty
/// stored identity as `""` (a damaged record's `record_id` is never empty:
/// a contract violation, so a bodiless `500`, before), one whose bytes are
/// not UTF-8 as `\u{fffd}` (`409` before), so that the Pack can show it.
#[test]
fn a_damaged_record_names_its_record_in_plain_ascii() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let pristine = std::fs::read(fixture.database()).unwrap();
    for (value, named) in [("''", "\"\""), ("CAST(X'FF' AS TEXT)", "\\u{fffd}")] {
        std::fs::write(fixture.database(), &pristine).unwrap();
        fixture.corrupt(&format!(
            "UPDATE repository_snapshots SET snapshot_id = {value} WHERE snapshot_id = {};",
            quote(&ledger.cp1.snapshot)
        ));
        let document = capsule(fixture, &fixture.task_id);
        assert_eq!(document["status"], "tampered", "{document}");
        assert!(
            names(&document, "snapshot", named, "object_digest_mismatch"),
            "{value}: {document}"
        );
    }
}

/// `resolveWorkspace` at a path whose Workspace row does not decode is
/// `409 capsule_tampered`, which no retry cures (`503` before). U1 B4
/// (O5): when that row is the root's, `resolveWorkspace` at the linked
/// worktree answers the same, since it reads the root first; before, the
/// root was the first row by `created_at`, so the linked row (`'x'` sorts
/// after every time) was taken as the root and resolved.
#[test]
fn resolve_workspace_at_a_row_that_does_not_decode_is_capsule_tampered() {
    let fixture = Fixture::new();
    let linked = git_worktree(
        &fixture.repo,
        &fixture.repos.path().join("linked"),
        "linked",
    );
    let resolve = |path: &std::path::Path| {
        let body = json!({ "schema_version": 1, "project_id": fixture.project_id, "path": path });
        resolve_workspace(
            fixture.state(),
            &json_request(Some(&fixture.writer), None, &body.to_string()),
        )
    };
    let linked_id = ok(&resolve(&linked), OperationId::ResolveWorkspace, 200)["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.corrupt(&format!(
        "UPDATE workspaces SET created_at = 'x' WHERE workspace_id = {};",
        quote(&fixture.workspace_id)
    ));
    assert_tampered(
        "the root Workspace row does not decode",
        OperationId::ResolveWorkspace,
        &resolve(&fixture.repo),
        TAMPERED_RECORD,
    );
    assert_tampered(
        "the root Workspace row does not decode, at the linked worktree",
        OperationId::ResolveWorkspace,
        &resolve(&linked),
        TAMPERED_RECORD,
    );
    assert_eq!(
        fixture.column(&format!(
            "SELECT count(*) FROM workspaces WHERE workspace_id = {};",
            quote(&linked_id)
        )),
        ["1"]
    );
}

/// T20 SF-1 for `listSessions`: a Workspace whose row is missing is read as
/// one whose row does not decode, so its Sessions are listed (`404` before);
/// a Workspace that neither a row nor an Event names is still not resolved.
#[test]
fn list_sessions_of_a_workspace_whose_row_is_missing_reads_as_one_that_does_not_decode() {
    let fixture = Fixture::new();
    let pristine = std::fs::read(fixture.database()).unwrap();
    let query = format!("workspace_id={}", fixture.workspace_id);
    let listed = || {
        ok(
            &list_sessions(fixture.state(), Some(&fixture.reader), Some(&query)),
            OperationId::ListSessions,
            200,
        )
    };
    fixture.corrupt(&format!(
        "UPDATE workspaces SET created_at = 'not a time' WHERE workspace_id = {};",
        quote(&fixture.workspace_id)
    ));
    let undecodable = listed();
    std::fs::write(fixture.database(), &pristine).unwrap();
    fixture.corrupt(&format!(
        "DELETE FROM workspaces WHERE workspace_id = {};",
        quote(&fixture.workspace_id)
    ));
    let missing = listed();
    assert_eq!(missing, undecodable);
    assert_eq!(
        missing["items"][0]["session_id"],
        fixture.session_id.as_str()
    );
    problem(
        &list_sessions(
            fixture.state(),
            Some(&fixture.reader),
            Some(&format!("workspace_id={UNKNOWN}")),
        ),
        OperationId::ListSessions,
        404,
        "workspace_not_resolved",
    );
}

/// A second Project with one Task; returns the Project and the Event of
/// that Task.
pub(super) fn other_project(fixture: &Fixture) -> (String, String) {
    let state = fixture.state();
    let control = Some(fixture.control.as_str());
    let other_repo = git_repository(fixture.repos.path(), "other");
    let body = json!({ "schema_version": 1, "root": other_repo }).to_string();
    let other = ok(
        &register_project(state, &json_request(control, Some(&fixture.key()), &body)),
        OperationId::RegisterProject,
        201,
    )["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = json!({ "schema_version": 1, "project_id": other, "title": "elsewhere" });
    ok(
        &create_task(
            state,
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &body.to_string(),
            ),
        ),
        OperationId::CreateTask,
        201,
    );
    let event = ok(
        &list_events(state, control, Some(&format!("project_id={other}"))),
        OperationId::ListEvents,
        200,
    )["items"][0]["event_id"]
        .as_str()
        .unwrap()
        .to_owned();
    (other, event)
}

/// Every read of the Project of `ledger`: one Event, Evidence object, and
/// Checkpoint of it included.
pub(super) fn project_reads(
    ledger: &Ledger,
    event: &str,
) -> Vec<(&'static str, OperationId, WireResponse)> {
    let fixture = &ledger.fixture;
    let state = fixture.state();
    let (control, reader) = (
        Some(fixture.control.as_str()),
        Some(fixture.reader.as_str()),
    );
    let project = fixture.project_id.as_str();
    let task = fixture.task_id.as_str();
    let sessions = format!("workspace_id={}", fixture.workspace_id);
    let tasks = format!("project_id={project}");
    vec![
        (
            "getProject",
            OperationId::GetProject,
            get_project(state, reader, project),
        ),
        (
            "listTasks",
            OperationId::ListTasks,
            list_tasks(state, reader, Some(&tasks)),
        ),
        (
            "listEvents",
            OperationId::ListEvents,
            list_events(state, control, Some(&tasks)),
        ),
        (
            "listSessions",
            OperationId::ListSessions,
            list_sessions(state, reader, Some(&sessions)),
        ),
        (
            "verifyLedger",
            OperationId::VerifyLedger,
            fixture.verify_with(reader, Some(&fixture.key()), project),
        ),
        (
            "getTaskCapsule",
            OperationId::GetTaskCapsule,
            fixture.capsule_with(reader, task, None),
        ),
        (
            "getRecoveryPreview",
            OperationId::GetRecoveryPreview,
            fixture.preview_with(reader, task, None),
        ),
        (
            "getEvent",
            OperationId::GetEvent,
            get_event(state, control, event),
        ),
        (
            "getEvidence",
            OperationId::GetEvidence,
            get_evidence(state, control, &ledger.cp1.evidence),
        ),
        (
            "getCheckpoint",
            OperationId::GetCheckpoint,
            get_checkpoint(state, reader, &ledger.cp1.checkpoint),
        ),
    ]
}

/// A state database whose binding (`state_metadata`) is missing is not
/// read as the Project's: every read of the Project, the ledger check
/// included, is `409 capsule_tampered` with a detail that says so (`503`
/// before). Another Project still reads, its records included.
#[test]
fn a_state_database_not_bound_to_its_project_is_capsule_tampered_for_its_reads() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let (other, other_event) = other_project(fixture);
    let event = fixture.column("SELECT event_id FROM events ORDER BY rowid LIMIT 1")[0].clone();
    for (case, operation, response) in &project_reads(&ledger, &event) {
        assert_eq!(response.status, 200, "{case}: untouched: {}", response.body);
        response.validate(Some(*operation)).unwrap();
    }

    fixture.corrupt("DELETE FROM state_metadata;");
    for (case, operation, response) in &project_reads(&ledger, &event) {
        assert_tampered(case, *operation, response, UNBOUND_STATE);
    }
    let (state, control) = (fixture.state(), Some(fixture.control.as_str()));
    ok(
        &get_project(state, Some(&fixture.reader), &other),
        OperationId::GetProject,
        200,
    );
    let found = ok(
        &get_event(state, control, &other_event),
        OperationId::GetEvent,
        200,
    );
    assert_eq!(found["event_id"], other_event.as_str());
}
