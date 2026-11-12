//! Narrow tests of P4-C2 (Owner, T19 SF-1 and the Session set): a row of
//! the Task's read scope that does not decode is a damaged record and makes
//! the Task `tampered`, never `503`; the Sessions of a Task are the ones the
//! chain names, and a Session row or a Task–Session link row that is
//! missing, does not decode, or disagrees with the chain is a damaged record
//! of type `session`.
//!
//! Every tamper is a writer that bypasses `memproved` (`sqlite3`, the
//! append-only triggers dropped, foreign keys off), as in `integrity_tests`.

use memprove_domain::{PackCapsule, render_pack, says_verified, select_pack};
use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{
    Fixture,
    integrity_tests::{Ledger, quote},
    ok,
};

impl Fixture {
    /// Both documents answer `200`; returns the Capsule's bytes and body,
    /// and the Preview's body.
    fn both_documents(&self, case: &str) -> (String, Value, Value) {
        let capsule = self.capsule_with(Some(&self.reader), &self.task_id, None);
        assert_eq!(capsule.status, 200, "{case}: Capsule {}", capsule.body);
        let preview = self.preview_with(Some(&self.reader), &self.task_id, None);
        assert_eq!(preview.status, 200, "{case}: Preview {}", preview.body);
        let capsule_body = ok(&capsule, OperationId::GetTaskCapsule, 200);
        let preview_body = ok(&preview, OperationId::GetRecoveryPreview, 200);
        (capsule.body, capsule_body, preview_body)
    }

    /// The Sessions of the Capsule, in its order.
    fn capsule_sessions(&self) -> Vec<String> {
        let (_, capsule) = self.capsule(None);
        capsule["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["session_id"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// The Capsule and the Preview are `tampered`, hold no `verified`, and the
/// Capsule's Pack shows no "verified" at any level; the Capsule's damaged
/// records are returned.
fn assert_tampered(fixture: &Fixture, case: &str) -> Vec<Value> {
    let (capsule_bytes, capsule, preview) = fixture.both_documents(case);
    assert_eq!(capsule["status"], "tampered", "{case}: {capsule_bytes}");
    assert_eq!(preview["status"], "tampered", "{case}: {preview}");
    assert!(!capsule_bytes.contains("\"verified\""), "{case}");
    assert!(!preview.to_string().contains("\"verified\""), "{case}");
    let pack =
        PackCapsule::from_json(&capsule_bytes).unwrap_or_else(|error| panic!("{case}: {error}"));
    for budget in [1_000_000, 9_000, 3_000] {
        if let Ok(selection) = select_pack(&pack, budget) {
            let text = render_pack(&selection);
            assert!(!says_verified(&text), "{case} at {budget}:\n{text}");
        }
    }
    let damaged = capsule["damaged_records"].as_array().unwrap().clone();
    assert!(
        !damaged.is_empty() || capsule["bindings"]["ledger"]["chain_valid"] == false,
        "{case}"
    );
    damaged
}

fn session_record(session_id: &str, reason: &str) -> Value {
    json!({ "record_type": "session", "record_id": session_id, "reason": reason })
}

/// T19 SF-1: a Session row whose `agent_id` is not a UUID made every read
/// of the Task `503 service_not_ready`. Both documents now answer `200`,
/// `tampered`, with the Session named; the Session stays listed, in its
/// place, from the chain.
#[test]
fn sf1_a_session_row_that_does_not_decode_is_a_damaged_record() {
    for (case, column, value) in [
        ("agent_id", "agent_id", "someone-else"),
        ("started_at", "started_at", "not a time"),
        ("workspace_id", "workspace_id", "not-a-uuid"),
    ] {
        let ledger = Ledger::new();
        let fixture = &ledger.fixture;
        let sessions = fixture.capsule_sessions();
        let first = &sessions[0];
        fixture.tamper(&format!(
            "UPDATE sessions SET {column} = {} WHERE session_id = {};",
            quote(value),
            quote(first)
        ));
        let damaged = assert_tampered(fixture, case);
        assert_eq!(
            damaged,
            vec![session_record(first, "object_digest_mismatch")],
            "{case}"
        );
        assert_eq!(fixture.capsule_sessions(), sessions, "{case}");
    }
}

/// One undecodable field per table the Task's reads cover, on `ledger`:
/// the case, its SQL, and the record type it is named by (`chain`: the
/// Event chain no longer verifies; `any`: the record that holds it).
fn table_cases(ledger: &Ledger) -> [(&'static str, String, &'static str); 9] {
    let task = quote(&ledger.fixture.task_id);
    let session = quote(&ledger.fixture.session_id);
    let checkpoint = quote(&ledger.cp1.checkpoint);
    let snapshot = quote(&ledger.cp1.snapshot);
    let evidence = quote(&ledger.cp1.evidence);
    [
        (
            "tasks.workspace_id",
            format!("UPDATE tasks SET workspace_id = 'not-a-uuid' WHERE task_id = {task};"),
            "task",
        ),
        (
            "tasks.created_at",
            format!("UPDATE tasks SET created_at = 'not a time' WHERE task_id = {task};"),
            "task",
        ),
        (
            "task_sessions.linked_at",
            format!(
                "UPDATE task_sessions SET linked_at = 'not a time' WHERE session_id = {session};"
            ),
            "session",
        ),
        (
            "events.recorded_at",
            format!(
                "UPDATE events SET recorded_at = 'not a time' WHERE session_id = {session} \
                 AND event_type = 'session.started';"
            ),
            "chain",
        ),
        (
            "checkpoints.created_at",
            format!(
                "UPDATE checkpoints SET created_at = 'not a time' \
                 WHERE checkpoint_id = {checkpoint};"
            ),
            "checkpoint",
        ),
        (
            "checkpoint_item_evidence.evidence_id",
            format!(
                "UPDATE checkpoint_item_evidence SET evidence_id = 'not-a-uuid' \
                 WHERE checkpoint_id = {checkpoint};"
            ),
            "any",
        ),
        (
            "repository_snapshots.captured_at",
            format!(
                "UPDATE repository_snapshots SET captured_at = 'not a time' \
                 WHERE snapshot_id = {snapshot};"
            ),
            "snapshot",
        ),
        (
            "repository_snapshot_evidence.evidence_id",
            format!(
                "UPDATE repository_snapshot_evidence SET evidence_id = 'not-a-uuid' \
                 WHERE snapshot_id = {snapshot} AND role = 'test';"
            ),
            "any",
        ),
        (
            "evidence.observed_at",
            format!(
                "UPDATE evidence SET observed_at = 'not a time' WHERE evidence_id = {evidence};"
            ),
            "evidence",
        ),
    ]
}

/// SF-1 over every other table the Task's reads cover: a field that does
/// not decode is a damaged record, `200` and `tampered` in both documents.
/// `checkpoint_items` holds no field its STRICT columns and CHECKs let be
/// undecodable; its text is covered by the digest (`integrity_tests`).
#[test]
fn sf1_a_field_that_does_not_decode_in_any_table_is_a_damaged_record() {
    for index in 0..9 {
        let ledger = Ledger::new();
        let fixture = &ledger.fixture;
        let (case, sql, record_type) = table_cases(&ledger)[index].clone();
        fixture.tamper(&sql);
        let damaged = assert_tampered(fixture, case);
        let types = damaged
            .iter()
            .map(|record| record["record_type"].as_str().unwrap())
            .collect::<Vec<_>>();
        match record_type {
            // A row of the chain that does not decode breaks the chain.
            "chain" => {
                let (_, capsule) = fixture.capsule(None);
                assert_eq!(
                    capsule["bindings"]["ledger"]["chain_valid"], false,
                    "{case}"
                );
            }
            "any" => {}
            expected => assert!(types.contains(&expected), "{case}: {damaged:?}"),
        }
    }
}

/// SF-1: a Workspace row is not a record of the Task; one that does not
/// decode leaves the probe unavailable, never `503`.
#[test]
fn sf1_a_workspace_row_that_does_not_decode_leaves_the_probe_unavailable() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    fixture.tamper(&format!(
        "UPDATE workspaces SET created_at = 'not a time' WHERE workspace_id = {};",
        quote(&fixture.workspace_id)
    ));
    let (_, capsule, preview) = fixture.both_documents("workspaces.created_at");
    assert_eq!(capsule["bindings"]["probe"]["available"], false);
    assert_ne!(capsule["status"], "verified");
    assert_ne!(preview["status"], "verified");
}

/// P4-C2 (Owner): the Sessions of a Task are the ones the chain names.
/// Deleting a link row or a Session row, or forging a link row, makes both
/// documents `tampered` and names the Session.
#[test]
fn the_session_set_is_the_chain_and_each_disagreement_is_named() {
    // A link row deleted: the chain still names the Session.
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let sessions = fixture.capsule_sessions();
    fixture.tamper(&format!(
        "DELETE FROM task_sessions WHERE session_id = {};",
        quote(&sessions[0])
    ));
    let damaged = assert_tampered(fixture, "link row deleted");
    assert_eq!(
        damaged,
        vec![session_record(&sessions[0], "object_missing")]
    );
    assert_eq!(fixture.capsule_sessions(), sessions);

    // A Session row deleted: listed from the chain, named as missing.
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let sessions = fixture.capsule_sessions();
    fixture.tamper(&format!(
        "DELETE FROM sessions WHERE session_id = {};",
        quote(&sessions[1])
    ));
    let damaged = assert_tampered(fixture, "Session row deleted");
    assert_eq!(
        damaged,
        vec![session_record(&sessions[1], "object_missing")]
    );
    assert_eq!(fixture.capsule_sessions(), sessions);

    // A link row forged for a Session of no Task: not listed, not read,
    // named as disagreeing with the chain; the numbering stays (T19 N-4).
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let sessions = fixture.capsule_sessions();
    let (_, before) = fixture.capsule(None);
    let stranger = fixture.session(&fixture.workspace_id.clone(), None);
    fixture.tamper(&format!(
        "INSERT INTO task_sessions (task_id, session_id, linked_at, revision) \
         VALUES ({}, {}, '2026-10-06T00:00:00Z', 1);",
        quote(&fixture.task_id),
        quote(&stranger)
    ));
    let damaged = assert_tampered(fixture, "link row forged");
    assert_eq!(
        damaged,
        vec![session_record(&stranger, "object_digest_mismatch")]
    );
    assert_eq!(fixture.capsule_sessions(), sessions);
    let (_, after) = fixture.capsule(None);
    let numbers = |capsule: &Value| {
        capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .map(|claim| (claim["number"].clone(), claim["claim_id"].clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(numbers(&after), numbers(&before));
}

/// Untouched, a ledger names no Session record, and a Session that no Task
/// links is not read by the Task.
#[test]
fn an_untouched_ledger_names_no_session_record() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    fixture.session(&fixture.workspace_id.clone(), None);
    let (_, capsule, preview) = fixture.both_documents("untouched");
    assert_eq!(capsule["status"], "verified");
    assert_eq!(preview["status"], "verified");
    assert_eq!(capsule["damaged_records"], json!([]));
    assert_eq!(fixture.capsule_sessions().len(), 2);
}
