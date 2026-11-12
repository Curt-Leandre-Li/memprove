//! Narrow tests of the T21 fixes (fix round 1) in the daemon:
//!
//! - V1-S-2: `verifyLedger` reports a Task row any column of which does not
//!   decode as `listTasks` and the Task Capsule decode it, and a Task its
//!   Task Events name that no row holds.
//! - V1-N-5: the writes take a Session's end and a Task's Workspace scope
//!   from the Event chain, as the Task Capsule reads them, not from the
//!   rows no digest covers.
//! - V1-R-2 (fix round 2): the state database of an `ACTIVE` Project that
//!   is missing, of 0 bytes, or without any object is not created again by
//!   a read or a write; it is not read as the Project's ledger.
//!
//! Every tamper is a writer that bypasses `memproved`: `sqlite3` on the
//! state database with the append-only triggers dropped and foreign keys
//! off.

use std::fmt::Write as _;

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{
    Fixture,
    integrity_tests::{Ledger, quote},
    ok,
    p4h1_review_tests::{assert_tampered, other_project, project_reads},
    problem,
};
use crate::{
    catalog::{
        UNBOUND_STATE, create_session, end_session, get_project, register_project,
        resolve_workspace,
    },
    events::{get_event, ingest_event},
    test_support::{git_worktree, json_request},
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

/// The Project sequence of the `task.created` Event of `task_id`:
/// `listEvents` lists the Project's Events in Project order, which is
/// contiguous from 1.
fn created_at_sequence(fixture: &Fixture, task_id: &str) -> u64 {
    let position = fixture
        .events()
        .iter()
        .position(|event| {
            event["event_type"] == "task.created" && event["payload"]["task_id"] == task_id
        })
        .unwrap();
    u64::try_from(position).unwrap() + 1
}

/// V1-S-2: each column of a Task row that the Capsule and `listTasks`
/// decode, made a value that does not decode, is a `task` `first_failure`
/// of `verifyLedger` (before: `valid` true, only `task_id`, `status`, and
/// `revision` were read); a Task whose Task Events remain but whose row is
/// deleted is reported missing with the Project sequence of its
/// `task.created` Event, and still counted (before: `valid` true, one Task
/// fewer).
#[test]
fn v1_s2_ledger_verify_names_a_task_row_that_does_not_decode_or_is_missing() {
    let cases = [
        ("project_id", "'zz'"),
        ("created_at", "CAST(X'ff41' AS TEXT)"),
        ("updated_at", "''"),
        ("workspace_id", "'zz'"),
        ("title", "''"),
        ("title", "CAST(X'ff' AS TEXT)"),
        ("description", "CAST(X'ff' AS TEXT)"),
    ];
    for (column, value) in cases {
        let fixture = Fixture::new();
        let intact = verified(&fixture);
        assert_eq!(intact["valid"], true, "{intact}");
        fixture.tamper(&format!(
            "PRAGMA ignore_check_constraints = ON; UPDATE tasks SET {column} = {value} \
             WHERE task_id = {};",
            quote(&fixture.task_id)
        ));
        let report = verified(&fixture);
        let case = format!("{column} = {value}: {report}");
        assert_eq!(report["valid"], false, "{case}");
        assert_eq!(report["first_failure"]["record_type"], "task", "{case}");
        assert_eq!(
            report["first_failure"]["record_id"],
            fixture.task_id.as_str(),
            "{case}"
        );
        assert_eq!(
            report["first_failure"]["project_sequence"],
            Value::Null,
            "{case}"
        );
        assert_eq!(report["checked"], intact["checked"], "{case}");
    }

    let fixture = Fixture::new();
    let intact = verified(&fixture);
    let sequence = created_at_sequence(&fixture, &fixture.task_id);
    fixture.tamper(&format!(
        "DELETE FROM tasks WHERE task_id = {};",
        quote(&fixture.task_id)
    ));
    let report = verified(&fixture);
    assert_eq!(report["valid"], false, "{report}");
    let failure = &report["first_failure"];
    assert_eq!(failure["record_type"], "task", "{report}");
    assert_eq!(failure["record_id"], fixture.task_id.as_str(), "{report}");
    assert_eq!(failure["project_sequence"], sequence, "{report}");
    assert!(
        failure["reason"]
            .as_str()
            .unwrap()
            .starts_with("record is missing"),
        "{report}"
    );
    assert_eq!(report["checked"], intact["checked"], "{report}");
}

/// V1-N-5: a Session whose end the chain records refuses every write that
/// appends on it, `409 invalid_transition`, also when a writer that
/// bypasses `memproved` has cleared its row's `ended_at` (before: each
/// write succeeded, and the ledger held Events after `session.ended`).
#[test]
fn v1_n5_a_session_the_chain_ended_refuses_writes_whatever_its_row_says() {
    let fixture = Fixture::new();
    let run = fixture.capture(&fixture.run_body("test_run", 0, Some(2)));
    let snapshot = run["snapshot"]["snapshot_id"].as_str().unwrap().to_owned();
    let evidence = run["evidence_id"].as_str().unwrap().to_owned();
    let body = json!({ "schema_version": 1, "expected_revision": 1, "reason": "done" });
    ok(
        &end_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &body.to_string(),
            ),
            &fixture.session_id,
        ),
        OperationId::EndSession,
        200,
    );
    fixture.tamper(&format!(
        "UPDATE sessions SET ended_at = NULL WHERE session_id = {};",
        quote(&fixture.session_id)
    ));
    let events = fixture.events().len();

    problem(
        &fixture.capture_with(
            &fixture.writer,
            &fixture.key(),
            &fixture.run_body("test_run", 0, Some(2)),
        ),
        OperationId::CaptureEvidence,
        409,
        "invalid_transition",
    );
    problem(
        &fixture.checkpoint_with(
            &fixture.writer,
            &fixture.key(),
            &fixture.checkpoint_body(&snapshot, &[&evidence]),
        ),
        OperationId::CreateCheckpoint,
        409,
        "invalid_transition",
    );
    problem(
        &fixture.decision_with(
            Some(&fixture.writer),
            &fixture.key(),
            &fixture.decision_body("keep it", &[], &[]),
        ),
        OperationId::ProposeDecision,
        409,
        "invalid_transition",
    );
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
    let again = json!({ "schema_version": 1, "expected_revision": 2, "reason": "again" });
    problem(
        &end_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &again.to_string(),
            ),
            &fixture.session_id,
        ),
        OperationId::EndSession,
        409,
        "invalid_transition",
    );
    assert_eq!(fixture.events().len(), events);
    assert!(fixture.ledger_valid());
}

/// V1-N-5: a Task scoped to a Workspace by its `task.created@1` Event takes
/// no Session and no run from another Workspace, `409
/// project_workspace_mismatch`, also when a writer that bypasses
/// `memproved` has cleared its row's `workspace_id` (before: both
/// succeeded while the Capsule still read the Task as scoped).
#[test]
fn v1_n5_a_task_row_that_lost_its_scope_still_scopes_the_writes() {
    let mut fixture = Fixture::new();
    let linked = git_worktree(
        &fixture.repo,
        &fixture.repos.path().join("linked"),
        "linked",
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
    let begin = |fixture: &Fixture| {
        let body = json!({
            "schema_version": 1, "workspace_id": linked_id, "agent_id": super::AGENT,
            "task_id": fixture.task_id,
        });
        create_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &body.to_string(),
            ),
        )
    };
    problem(
        &begin(&fixture),
        OperationId::CreateSession,
        409,
        "project_workspace_mismatch",
    );
    fixture.tamper(&format!(
        "UPDATE tasks SET workspace_id = NULL WHERE task_id = {};",
        quote(&fixture.task_id)
    ));
    let events = fixture.events().len();
    problem(
        &begin(&fixture),
        OperationId::CreateSession,
        409,
        "project_workspace_mismatch",
    );

    // A run in the other Workspace on a Session whose row was moved there
    // too: only the Task's scope refuses it.
    fixture.tamper(&format!(
        "UPDATE sessions SET workspace_id = {} WHERE session_id = {};",
        quote(&linked_id),
        quote(&fixture.session_id)
    ));
    fixture.workspace_id.clone_from(&linked_id);
    problem(
        &fixture.capture_with(
            &fixture.writer,
            &fixture.key(),
            &fixture.run_body("test_run", 0, Some(2)),
        ),
        OperationId::CaptureEvidence,
        409,
        "project_workspace_mismatch",
    );
    assert_eq!(fixture.events().len(), events);
}

/// A change made to the Project's state database.
type BreakState = fn(&Fixture);

/// The files of the Project's directory and their bytes; `None` when the
/// directory does not exist.
fn project_files(fixture: &Fixture) -> Option<Vec<(String, Vec<u8>)>> {
    let directory = fixture.database().parent().unwrap().to_owned();
    let entries = std::fs::read_dir(directory).ok()?;
    let mut files = entries
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    Some(files)
}

/// `endSession` of `session`, the first revision.
fn end(fixture: &Fixture, session: &str) -> crate::api::WireResponse {
    end_session(
        fixture.state(),
        &json_request(
            Some(&fixture.writer),
            Some(&fixture.key()),
            r#"{"schema_version":1,"expected_revision":1,"reason":"done"}"#,
        ),
        session,
    )
}

/// V1-R-2 (fix round 2): the state database of an `ACTIVE` Project
/// deleted, made 0 bytes, removed with its directory, or stripped of every
/// object is not created again: every read of the Project is `409
/// capsule_tampered` with the fixed detail of a database not bound to its
/// Project, a write and a repeated registration are `503`, and none of
/// them creates or writes a file (before:
/// the first read created and migrated an empty database, `verifyLedger`
/// answered `valid` and `listTasks` an empty page). Another Project still
/// reads and writes, its Sessions found past the broken one.
#[test]
fn v1_r2_a_missing_or_empty_state_database_is_not_created_again() {
    let cases: [(&str, BreakState); 4] = [
        ("deleted", |fixture| {
            std::fs::remove_file(fixture.database()).unwrap();
        }),
        ("0 bytes", |fixture| {
            std::fs::write(fixture.database(), b"").unwrap();
        }),
        ("its directory removed", |fixture| {
            std::fs::remove_dir_all(fixture.database().parent().unwrap()).unwrap();
        }),
        ("every object dropped", |fixture| {
            let mut drops = String::new();
            for row in
                fixture.rows("SELECT type, name FROM sqlite_master WHERE type IN ('table', 'view')")
            {
                write!(
                    drops,
                    "DROP {} IF EXISTS \"{}\";",
                    row[0].to_uppercase(),
                    row[1]
                )
                .unwrap();
            }
            fixture.corrupt(&drops);
            assert!(
                fixture
                    .column("SELECT name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
                    .is_empty()
            );
        }),
    ];
    for (case, break_it) in cases {
        let ledger = Ledger::new();
        let fixture = &ledger.fixture;
        let (other, other_event) = other_project(fixture);
        let event = fixture.column("SELECT event_id FROM events ORDER BY rowid LIMIT 1")[0].clone();
        break_it(fixture);
        let before = project_files(fixture);

        for (read, operation, response) in &project_reads(&ledger, &event) {
            assert_tampered(
                &format!("{case}: {read}"),
                *operation,
                response,
                UNBOUND_STATE,
            );
        }
        problem(
            &end(fixture, &fixture.session_id),
            OperationId::EndSession,
            503,
            "service_not_ready",
        );
        // Registering the repository again replays the ACTIVE Project; it
        // does not provision a new, empty ledger for it.
        let body = json!({ "schema_version": 1, "root": fixture.repo }).to_string();
        problem(
            &register_project(
                fixture.state(),
                &json_request(Some(&fixture.control), Some(&fixture.key()), &body),
            ),
            OperationId::RegisterProject,
            503,
            "service_not_ready",
        );
        assert_eq!(project_files(fixture), before, "{case}: a file was written");

        let state = fixture.state();
        let root = ok(
            &get_project(state, Some(&fixture.reader), &other),
            OperationId::GetProject,
            200,
        )["root_workspace_id"]
            .as_str()
            .unwrap()
            .to_owned();
        ok(
            &get_event(state, Some(&fixture.control), &other_event),
            OperationId::GetEvent,
            200,
        );
        let session = fixture.session(&root, None);
        ok(&end(fixture, &session), OperationId::EndSession, 200);
        assert_eq!(project_files(fixture), before, "{case}: a file was written");
    }
}
