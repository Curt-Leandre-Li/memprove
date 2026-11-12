//! Narrow tests of P3-C2 (the findings of T12): the Recovery Preview and
//! the Task Capsule share one Task-scope integrity check, a `tampered`
//! document holds no `verified`, the reserved Event types are read only at
//! payload schema `<type>@1` and only on a Session of the Task, a fact is
//! superseded only by a fact, and `capsule.recovered` is reserved.
//!
//! Every tamper is a writer that bypasses `memproved`: `sqlite3` on the
//! state database with the append-only triggers dropped, digests recomputed
//! with the domain's own constructors where the case says so.

use std::{fmt::Write as _, path::PathBuf};

use memprove_domain::{
    Checkpoint, CheckpointInput, EvidenceId, EvidencePayload, RepositorySnapshot,
    RepositorySnapshotId, RepositorySnapshotInput,
};
use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, ok, problem, strings};
use crate::{catalog::end_session, events::ingest_event, test_support::json_request};

/// One tamper of T12's step 9, given the ledger.
type Tamper = fn(&Ledger);

/// One tamper of T12's step 9 as SQL, given the ledger.
type TamperSql = Box<dyn Fn(&Ledger) -> String>;

/// The append-only triggers of the recovery tables and the Event ledger.
const REJECT_TRIGGERS: [&str; 21] = [
    "evidence_reject_update",
    "evidence_reject_delete",
    "evidence_reject_replace",
    "repository_snapshots_reject_update",
    "repository_snapshots_reject_delete",
    "repository_snapshots_reject_replace",
    "repository_snapshot_evidence_reject_update",
    "repository_snapshot_evidence_reject_delete",
    "repository_snapshot_evidence_reject_replace",
    "checkpoints_reject_update",
    "checkpoints_reject_delete",
    "checkpoints_reject_replace",
    "checkpoint_items_reject_update",
    "checkpoint_items_reject_delete",
    "checkpoint_items_reject_replace",
    "checkpoint_item_evidence_reject_update",
    "checkpoint_item_evidence_reject_delete",
    "checkpoint_item_evidence_reject_replace",
    "events_reject_update",
    "events_reject_delete",
    "events_reject_replace",
];

pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

impl Fixture {
    pub(super) fn database(&self) -> PathBuf {
        std::fs::canonicalize(self.harness.dir.path())
            .unwrap()
            .join("projects")
            .join(&self.project_id)
            .join("state.sqlite")
    }

    /// Runs `sql` with `sqlite3` on the state database, the append-only
    /// triggers dropped: a writer that bypasses `memproved`.
    pub(super) fn tamper(&self, sql: &str) {
        let mut drops = String::new();
        for name in REJECT_TRIGGERS {
            write!(drops, "DROP TRIGGER IF EXISTS {name};").unwrap();
        }
        let output = std::process::Command::new("sqlite3")
            .arg(self.database())
            .arg(format!("PRAGMA foreign_keys = OFF; {drops} {sql}"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{sql}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub(super) fn ledger_valid(&self) -> bool {
        let verified = ok(
            &self.verify_with(Some(&self.control), Some(&self.key()), &self.project_id),
            OperationId::VerifyLedger,
            200,
        );
        verified["valid"].as_bool().unwrap()
    }

    /// The Preview and the Capsule of the Task under one probe, bodies
    /// included.
    fn documents(&self) -> ((String, Value), (String, Value)) {
        (self.preview(None), self.capsule(None))
    }

    /// Rewrites each Evidence object's payload and recomputes every digest
    /// that covers it (the Evidence, its Snapshot, and the Checkpoint over
    /// that Snapshot) with the domain's own constructors, as a writer that
    /// knows the digest rules would. The records then recompute; only the
    /// ledger's Events disagree. The one exception: a Checkpoint whose
    /// `verified_fact` would rest on a failed test is refused by the
    /// domain's constructor, so its digest stays and it no longer
    /// recomputes either.
    pub(super) fn rewrite_payloads(&self, edits: &[(&str, &str, &str, EvidencePayload)]) {
        let store = self.store();
        let mut sql = String::new();
        let mut rewritten = std::collections::BTreeMap::new();
        let mut recomputed = Vec::new();
        for (evidence_id, _, _, payload) in edits {
            let id = EvidenceId::parse(evidence_id).unwrap();
            let mut input = store.get_evidence(&id).unwrap().to_input();
            input.payload = payload.clone();
            let evidence = memprove_domain::Evidence::new(input).unwrap();
            let text = memprove_domain::CanonicalJson::parse(
                &serde_json::to_string(evidence.payload()).unwrap(),
            )
            .unwrap();
            write!(
                sql,
                "UPDATE evidence SET payload = {}, payload_digest = {}, canonical_digest = {} \
                 WHERE evidence_id = {};",
                quote(text.as_str()),
                quote(evidence.payload_digest().as_str()),
                quote(evidence.canonical_digest().as_str()),
                quote(evidence_id),
            )
            .unwrap();
            rewritten.insert(id, evidence);
        }
        for (_, snapshot_id, checkpoint_id, _) in edits {
            let snapshot = store
                .get_repository_snapshot(&RepositorySnapshotId::parse(snapshot_id).unwrap())
                .unwrap();
            let evidence = snapshot
                .all_evidence_ids()
                .map(|id| {
                    rewritten
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| store.get_evidence(id).unwrap())
                })
                .collect::<Vec<_>>();
            let snapshot = RepositorySnapshot::new(snapshot_input(&snapshot), &evidence).unwrap();
            let checkpoint = store
                .get_checkpoint(&memprove_domain::CheckpointId::parse(checkpoint_id).unwrap())
                .unwrap();
            write!(
                sql,
                "UPDATE repository_snapshots SET canonical_digest = {} WHERE snapshot_id = {};",
                quote(snapshot.canonical_digest().as_str()),
                quote(snapshot_id),
            )
            .unwrap();
            if let Ok(checkpoint) =
                Checkpoint::new(checkpoint_input(&checkpoint), &snapshot, &evidence)
            {
                write!(
                    sql,
                    "UPDATE checkpoints SET digest = {} WHERE checkpoint_id = {};",
                    quote(checkpoint.digest().as_str()),
                    quote(checkpoint_id),
                )
                .unwrap();
                recomputed.push(*checkpoint_id);
            }
        }
        drop(store);
        self.tamper(&sql);
        // Precondition of the case: every rewritten record recomputes.
        let store = self.store();
        for (evidence_id, snapshot_id, _, _) in edits {
            store
                .get_evidence(&EvidenceId::parse(evidence_id).unwrap())
                .unwrap();
            store
                .get_repository_snapshot(&RepositorySnapshotId::parse(snapshot_id).unwrap())
                .unwrap();
        }
        for checkpoint_id in recomputed {
            store
                .get_checkpoint(&memprove_domain::CheckpointId::parse(checkpoint_id).unwrap())
                .unwrap();
        }
    }
}

fn snapshot_input(snapshot: &RepositorySnapshot) -> RepositorySnapshotInput {
    RepositorySnapshotInput {
        snapshot_id: snapshot.snapshot_id().clone(),
        project_id: snapshot.project_id().clone(),
        task_id: snapshot.task_id().clone(),
        workspace_id: snapshot.workspace_id().clone(),
        head_oid: snapshot.head_oid().to_owned(),
        branch: snapshot.branch().map(str::to_owned),
        worktree_status_porcelain_v2: snapshot.worktree_status_porcelain_v2().to_owned(),
        index_digest: snapshot.index_digest().clone(),
        tracked_worktree_digest: snapshot.tracked_worktree_digest().clone(),
        capture_manifest_digest: snapshot.capture_manifest_digest().clone(),
        git_head_evidence_id: snapshot.git_head_evidence_id().clone(),
        git_worktree_evidence_id: snapshot.git_worktree_evidence_id().clone(),
        file_evidence_ids: snapshot.file_evidence_ids().to_vec(),
        command_evidence_ids: snapshot.command_evidence_ids().to_vec(),
        test_evidence_ids: snapshot.test_evidence_ids().to_vec(),
        captured_at: snapshot.captured_at().clone(),
    }
}

fn checkpoint_input(checkpoint: &Checkpoint) -> CheckpointInput {
    CheckpointInput {
        checkpoint_id: checkpoint.checkpoint_id().clone(),
        task_id: checkpoint.task_id().clone(),
        session_id: checkpoint.session_id().clone(),
        snapshot_id: checkpoint.snapshot_id().clone(),
        project_event_sequence: checkpoint.project_event_sequence(),
        objective: checkpoint.objective().to_owned(),
        verified_completed_work: checkpoint.verified_completed_work().to_vec(),
        open_items: checkpoint.open_items().to_vec(),
        next_action: checkpoint.next_action().clone(),
        created_at: checkpoint.created_at().clone(),
    }
}

/// A test run's payload with `exit_code` (three cases).
fn test_payload(exit_code: i32) -> EvidencePayload {
    EvidencePayload::TestResult {
        argv: vec!["cargo".to_owned(), "test".to_owned()],
        exit_code,
        stdout: "test result: ok".to_owned(),
        stderr: String::new(),
        passed: EvidencePayload::test_passed(exit_code, Some(3)),
        test_case_count: Some(3),
        test_format: None,
        started_at: Some(memprove_domain::UtcTimestamp::parse("2026-10-02T10:00:00Z").unwrap()),
        finished_at: Some(
            memprove_domain::UtcTimestamp::parse("2026-10-02T10:00:02.500Z").unwrap(),
        ),
        stdout_sha256: Some(memprove_domain::Sha256Digest::of_bytes(b"a")),
        stderr_sha256: Some(memprove_domain::Sha256Digest::of_bytes(b"b")),
        launch_error: None,
    }
}

/// One recorded run and the Checkpoint over it.
pub(super) struct Recorded {
    pub(super) evidence: String,
    pub(super) snapshot: String,
    pub(super) checkpoint: String,
}

/// T12's tamper ledger (step 9): two Sessions and three Checkpoints. CP1
/// (Session 1) states "tests pass" on a passed run; CP2 (Session 1) an
/// open item on a failed run; CP3 (Session 2) "tests pass" on a passed run.
/// Untouched, both documents are `verified`.
pub(super) struct Ledger {
    pub(super) fixture: Fixture,
    pub(super) cp1: Recorded,
    pub(super) cp2: Recorded,
    pub(super) cp3: Recorded,
}

impl Ledger {
    pub(super) fn new() -> Self {
        let mut fixture = Fixture::new();
        let record = |fixture: &Fixture, exit_code: i32, fact: bool| {
            let run = fixture.capture(&fixture.run_body("test_run", exit_code, Some(3)));
            let evidence = run["evidence_id"].as_str().unwrap().to_owned();
            let snapshot = run["snapshot"]["snapshot_id"].as_str().unwrap().to_owned();
            let mut body = fixture.checkpoint_body(&snapshot, &[&evidence]);
            if !fact {
                body["verified_completed_work"] = json!([]);
                body["open_items"] = json!([
                    { "classification": "unknown", "text": "it fails", "evidence_ids": [evidence] },
                    { "classification": "unknown", "text": "second" },
                ]);
            }
            let created = ok(
                &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
                OperationId::CreateCheckpoint,
                201,
            );
            Recorded {
                evidence,
                snapshot,
                checkpoint: created["checkpoint_id"].as_str().unwrap().to_owned(),
            }
        };
        let cp1 = record(&fixture, 0, true);
        let cp2 = record(&fixture, 1, false);
        ok(
            &end_session(
                fixture.state(),
                &json_request(
                    Some(&fixture.writer),
                    Some(&fixture.key()),
                    r#"{"schema_version":1,"expected_revision":1,"reason":"done"}"#,
                ),
                &fixture.session_id,
            ),
            OperationId::EndSession,
            200,
        );
        fixture.session_id = fixture.session(
            &fixture.workspace_id.clone(),
            Some(&fixture.task_id.clone()),
        );
        let cp3 = record(&fixture, 0, true);
        let ledger = Self {
            fixture,
            cp1,
            cp2,
            cp3,
        };
        let ((_, preview), (_, capsule)) = ledger.fixture.documents();
        assert_eq!(preview["status"], "verified");
        assert_eq!(capsule["status"], "verified");
        assert!(ledger.fixture.ledger_valid());
        ledger
    }

    /// The Project sequence of the `checkpoint.created` Event of `checkpoint`:
    /// `memproved`'s producer sequence is the Project sequence it takes.
    fn announced_at(&self, checkpoint: &str) -> u64 {
        self.fixture
            .events()
            .iter()
            .find(|event| {
                event["event_type"] == "checkpoint.created"
                    && event["payload"]["checkpoint"]["checkpoint_id"] == checkpoint
            })
            .unwrap()["producer"]["sequence"]
            .as_u64()
            .unwrap()
    }
}

/// Every tamper here is one `verifyLedger` finds. Both documents are then
/// `tampered`, together, and neither holds `verified` anywhere.
pub(super) fn assert_tampered_without_verified(fixture: &Fixture, case: &str) {
    assert!(!fixture.ledger_valid(), "{case}: verifyLedger still valid");
    let ((preview_bytes, preview), (capsule_bytes, capsule)) = fixture.documents();
    assert_eq!(preview["status"], "tampered", "{case}: {preview_bytes}");
    assert_eq!(capsule["status"], "tampered", "{case}: {capsule_bytes}");
    assert!(
        !preview_bytes.contains("\"verified\""),
        "{case}: {preview_bytes}"
    );
    assert!(
        !capsule_bytes.contains("\"verified\""),
        "{case}: {capsule_bytes}"
    );
    assert!(
        strings(&capsule["gaps"]).contains(&"record_integrity")
            || strings(&capsule["gaps"]).contains(&"ledger_chain_invalid"),
        "{case}"
    );
}

/// T12 S-4 (repro `repro_preview_old_checkpoint.py`): an earlier
/// Checkpoint that no longer recomputes, by its Evidence payload or by a
/// statement's text, digests left as they were. `verifyLedger` and the
/// Capsule saw it; the Preview was still `verified`. Both are `tampered`
/// now, with nothing `verified` left in either.
#[test]
fn t12_s4_an_earlier_checkpoint_that_no_longer_recomputes_makes_both_tampered() {
    for mode in ["evidence payload", "statement text"] {
        let ledger = Ledger::new();
        let fixture = &ledger.fixture;
        match mode {
            "evidence payload" => fixture.tamper(&format!(
                "UPDATE evidence SET payload = replace(payload, '\"exit_code\":1', '\"exit_code\":0') \
                 WHERE evidence_id = {};",
                quote(&ledger.cp2.evidence)
            )),
            _ => fixture.tamper(&format!(
                "UPDATE checkpoint_items SET text = 'rewritten' \
                 WHERE checkpoint_id = {} AND section = 'open_items' AND ordinal = 0;",
                quote(&ledger.cp2.checkpoint)
            )),
        }
        assert_tampered_without_verified(fixture, mode);
        let (_, preview) = fixture.preview(None);
        if mode == "evidence payload" {
            // P3-D: the three runs share one repository state, so CP3's
            // Snapshot carries CP2's Evidence over (ADR 0013 D3) and seals
            // the changed row too; the latest Checkpoint no longer reads.
            assert_eq!(preview["bindings"]["checkpoint"]["intact"], false);
        } else {
            // The latest Checkpoint is intact and still listed.
            assert_eq!(preview["bindings"]["checkpoint"]["intact"], true);
            assert_eq!(preview["facts"][0]["text"], "tests pass");
        }
        assert!(strings(&preview["gaps"]).contains(&"record_integrity"));
        let (_, capsule) = fixture.capsule(None);
        let damaged = capsule["damaged_records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["record_type"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(
            damaged.contains(&if mode == "evidence payload" {
                "evidence"
            } else {
                "checkpoint"
            }),
            "{mode}: {damaged:?}"
        );
    }
}

/// T12 N1 (step 9, the two `tasks` cases): a Task row that disagrees with
/// its last Task Event is found by `verifyLedger`, and now makes both
/// documents `tampered` and is named a damaged record.
#[test]
fn t12_n1_a_task_row_that_disagrees_with_its_last_task_event_makes_both_tampered() {
    for (case, sql) in [
        (
            "completed, revision + 5",
            "UPDATE tasks SET status = 'completed', revision = revision + 5;",
        ),
        (
            "blocked, same revision",
            "UPDATE tasks SET status = 'blocked';",
        ),
    ] {
        let ledger = Ledger::new();
        let fixture = &ledger.fixture;
        fixture.tamper(sql);
        assert_tampered_without_verified(fixture, case);
        let (_, capsule) = fixture.capsule(None);
        assert_eq!(
            capsule["damaged_records"],
            json!([{
                "record_type": "task",
                "record_id": fixture.task_id,
                "reason": "object_digest_mismatch",
            }]),
            "{case}"
        );
    }
}

/// T12 step 9: every tamper `verifyLedger` finds, by records whose digests
/// were recomputed. Deleting only client Events at the chain's tail is the
/// known boundary (ADR 0012) and not here.
#[test]
fn t12_step9_recomputed_records_that_disagree_with_the_ledger_leave_no_verified() {
    let cases: [(&str, Tamper); 4] = [
        (
            "earlier Evidence FAIL -> PASS, digests recomputed",
            |ledger| {
                ledger.fixture.rewrite_payloads(&[(
                    &ledger.cp2.evidence,
                    &ledger.cp2.snapshot,
                    &ledger.cp2.checkpoint,
                    test_payload(0),
                )]);
            },
        ),
        (
            "earlier Evidence PASS -> FAIL, digests recomputed",
            |ledger| {
                ledger.fixture.rewrite_payloads(&[(
                    &ledger.cp1.evidence,
                    &ledger.cp1.snapshot,
                    &ledger.cp1.checkpoint,
                    test_payload(1),
                )]);
            },
        ),
        (
            "latest Evidence PASS -> FAIL, digests recomputed",
            |ledger| {
                ledger.fixture.rewrite_payloads(&[(
                    &ledger.cp3.evidence,
                    &ledger.cp3.snapshot,
                    &ledger.cp3.checkpoint,
                    test_payload(1),
                )]);
            },
        ),
        (
            "two passed payloads swapped, digests recomputed",
            |ledger| {
                let mut first = test_payload(0);
                let mut second = test_payload(0);
                if let (
                    EvidencePayload::TestResult { stdout: one, .. },
                    EvidencePayload::TestResult { stdout: two, .. },
                ) = (&mut first, &mut second)
                {
                    one.push_str(" (cp3)");
                    two.push_str(" (cp1)");
                }
                ledger.fixture.rewrite_payloads(&[
                    (
                        &ledger.cp1.evidence,
                        &ledger.cp1.snapshot,
                        &ledger.cp1.checkpoint,
                        first,
                    ),
                    (
                        &ledger.cp3.evidence,
                        &ledger.cp3.snapshot,
                        &ledger.cp3.checkpoint,
                        second,
                    ),
                ]);
            },
        ),
    ];
    for (case, tamper) in cases {
        let ledger = Ledger::new();
        tamper(&ledger);
        assert_tampered_without_verified(&ledger.fixture, case);
    }
}

/// T12 step 9, the rest: records changed without their digests, deleted
/// records, a broken chain, and a chain tail cut through the last
/// `checkpoint.created`.
#[test]
fn t12_step9_changed_deleted_and_reordered_records_leave_no_verified() {
    let delete_checkpoint = |checkpoint: &str| {
        let id = quote(checkpoint);
        format!(
            "DELETE FROM checkpoint_item_evidence WHERE checkpoint_id = {id};
             DELETE FROM checkpoint_items WHERE checkpoint_id = {id};
             DELETE FROM checkpoints WHERE checkpoint_id = {id};"
        )
    };
    let cases: [(&str, TamperSql); 7] = [
        (
            "earlier Evidence FAIL -> PASS, digests not recomputed",
            Box::new(|ledger| {
                format!(
                    "UPDATE evidence SET payload = replace(payload, '\"exit_code\":1', \
                     '\"exit_code\":0') WHERE evidence_id = {};",
                    quote(&ledger.cp2.evidence)
                )
            }),
        ),
        (
            "earlier Checkpoint deleted",
            Box::new(move |ledger| delete_checkpoint(&ledger.cp1.checkpoint)),
        ),
        (
            "latest Checkpoint deleted",
            Box::new(move |ledger| delete_checkpoint(&ledger.cp3.checkpoint)),
        ),
        (
            "earlier Checkpoint deleted with its Snapshot and Evidence",
            Box::new(move |ledger| {
                let snapshot = quote(&ledger.cp1.snapshot);
                format!(
                    "{} DELETE FROM evidence WHERE evidence_id IN (SELECT evidence_id FROM \
                     repository_snapshot_evidence WHERE snapshot_id = {snapshot});
                     DELETE FROM repository_snapshot_evidence WHERE snapshot_id = {snapshot};
                     DELETE FROM repository_snapshots WHERE snapshot_id = {snapshot};",
                    delete_checkpoint(&ledger.cp1.checkpoint)
                )
            }),
        ),
        (
            "two Events' project_sequence swapped",
            Box::new(|_| {
                "UPDATE events SET project_sequence = 9007199254740991 WHERE project_sequence = 2;
                 UPDATE events SET project_sequence = 2 WHERE project_sequence = 3;
                 UPDATE events SET project_sequence = 3 WHERE project_sequence = 9007199254740991;"
                    .to_owned()
            }),
        ),
        (
            "two statement texts of the earlier Checkpoint swapped",
            Box::new(|ledger| {
                let id = quote(&ledger.cp2.checkpoint);
                format!(
                    "UPDATE checkpoint_items SET text = 'second' WHERE checkpoint_id = {id} \
                     AND section = 'open_items' AND ordinal = 0;
                     UPDATE checkpoint_items SET text = 'it fails' WHERE checkpoint_id = {id} \
                     AND section = 'open_items' AND ordinal = 1;"
                )
            }),
        ),
        (
            "chain tail cut through the last checkpoint.created",
            Box::new(|ledger| {
                format!(
                    "DELETE FROM events WHERE project_sequence >= {};",
                    ledger.announced_at(&ledger.cp3.checkpoint)
                )
            }),
        ),
    ];
    for (case, sql) in cases {
        let ledger = Ledger::new();
        ledger.fixture.tamper(&sql(&ledger));
        assert_tampered_without_verified(&ledger.fixture, case);
    }
}

/// ADR 0013 D4, extended (P3-C2): under one ledger and one probe the
/// Preview is `tampered` exactly when the Capsule is, on an intact ledger
/// across repository changes and on a damaged one.
#[test]
fn the_preview_is_tampered_exactly_when_the_capsule_is() {
    let ledger = Ledger::new();
    let fixture = &ledger.fixture;
    let same = |state: &str| {
        let ((_, preview), (_, capsule)) = fixture.documents();
        assert_eq!(
            preview["status"] == "tampered",
            capsule["status"] == "tampered",
            "{state}: Preview {}, Capsule {}",
            preview["status"],
            capsule["status"]
        );
        preview["status"] == "tampered"
    };
    assert!(!same("intact"));
    std::fs::write(fixture.repo.join("README"), b"edited\n").unwrap();
    assert!(!same("worktree edited"));
    fixture.commit("change README");
    assert!(!same("new commit"));
    fixture.tamper(&format!(
        "UPDATE evidence SET payload = replace(payload, '\"exit_code\":0', '\"exit_code\":2') \
         WHERE evidence_id = {};",
        quote(&ledger.cp1.evidence)
    ));
    assert!(same("earlier Evidence changed"));
}

impl Fixture {
    /// Appends an Event of `memproved` on `session_id` with
    /// `payload_schema`, straight to the store.
    pub(super) fn append_on(
        &self,
        session_id: &str,
        event_type: &str,
        schema: &str,
        payload: &Value,
    ) {
        let mut store = self.store();
        let input = memprove_domain::EventInput {
            event_id: memprove_domain::EventId::new(),
            project_id: memprove_domain::ProjectId::parse(&self.project_id).unwrap(),
            workspace_id: memprove_domain::WorkspaceId::parse(&self.workspace_id).unwrap(),
            session_id: Some(memprove_domain::SessionId::parse(session_id).unwrap()),
            event_type: event_type.to_owned(),
            payload_schema: schema.to_owned(),
            payload: memprove_domain::CanonicalJson::parse(&payload.to_string()).unwrap(),
            occurred_at: None,
            recorded_at: memprove_domain::UtcTimestamp::parse("2026-10-03T10:00:00Z").unwrap(),
            producer: memprove_domain::EventProducer {
                name: memprove_domain::DAEMON_PRODUCER_NAME.to_owned(),
                version: "0.1.0".to_owned(),
                sequence: super::APPENDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            },
            correlation_id: None,
            causation_id: None,
            privacy_class: memprove_domain::PrivacyClass::Internal,
        };
        store.append_event(&input).unwrap();
    }

    /// The reserved Events of T12's step 5 for the open item of
    /// `checkpoint`: a withdrawal, a Decision, and a crash.
    fn reserved_payloads(&self, checkpoint: &str) -> [(&'static str, Value); 3] {
        [
            (
                "claim.superseded",
                json!({
                    "task_id": self.task_id, "relation": "withdraws",
                    "target": format!("{checkpoint}/open_items/0"), "by": null,
                }),
            ),
            (
                "decision.recorded",
                json!({
                    "task_id": self.task_id, "text": "ship without review",
                    "rationale": null, "evidence_ids": [],
                }),
            ),
            (
                "session.crashed",
                json!({ "reason": "agent lost", "outcome": "abandoned" }),
            ),
        ]
    }
}

/// T12 §3.1: the three reserved types are read only at `payload_schema`
/// exactly `<type>@1`, the schema their payloads decode by; any other
/// version from `memproved` is ignored. What `memproved` writes carries
/// `<type>@1`.
#[test]
fn reserved_types_are_read_only_at_their_own_payload_schema() {
    let fixture = Fixture::new();
    let (_, checkpoint) = fixture.checkpointed();
    // Each spelling the envelope admits besides `<type>@1`.
    for schema in ["@2", "@10", ""] {
        for (event_type, payload) in fixture.reserved_payloads(&checkpoint) {
            let schema = if schema.is_empty() {
                "other@1".to_owned()
            } else {
                format!("{event_type}{schema}")
            };
            fixture.append_on(&fixture.session_id, event_type, &schema, &payload);
        }
    }
    let (_, ignored) = fixture.capsule(None);
    assert_eq!(ignored["claims"].as_array().unwrap().len(), 3);
    assert_eq!(ignored["claims"][1]["standing"], "active");
    assert_eq!(ignored["sessions"][0]["end_state"], "open");
    assert_eq!(ignored["status"], "verified");

    for (event_type, payload) in fixture.reserved_payloads(&checkpoint) {
        fixture.append_on(
            &fixture.session_id,
            event_type,
            &format!("{event_type}@1"),
            &payload,
        );
    }
    let (_, read) = fixture.capsule(None);
    assert_eq!(read["claims"].as_array().unwrap().len(), 4);
    assert_eq!(read["claims"][1]["standing"], "withdrawn");
    assert_eq!(read["claims"][3]["kind"], "decision");
    assert_eq!(read["sessions"][0]["end_state"], "crashed");

    // memproved's own Decision and edge carry `<type>@1`. T21 (V1-N-5):
    // the Session above ended with `session.crashed@1` (its row was left
    // as it was), so it takes no write; the Decision goes on a new Session
    // of the Task.
    let session = fixture.session(
        &fixture.workspace_id.clone(),
        Some(&fixture.task_id.clone()),
    );
    let mut body =
        fixture.decision_body("keep JCS", &[], &[&format!("{checkpoint}/next_action/0")]);
    body["session_id"] = json!(session);
    let decided = ok(
        &fixture.decision_with(Some(&fixture.writer), &fixture.key(), &body),
        OperationId::ProposeDecision,
        201,
    );
    assert_eq!(decided["content"], "keep JCS");
    let schemas = fixture
        .events()
        .iter()
        .filter(|event| {
            event["producer"]["name"] == memprove_domain::DAEMON_PRODUCER_NAME
                && matches!(
                    event["event_type"].as_str(),
                    Some("decision.recorded" | "claim.superseded")
                )
                && !event["occurred_at"].is_null()
        })
        .map(|event| {
            (
                event["event_type"].as_str().unwrap().to_owned(),
                event["payload_schema"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        schemas,
        [
            (
                "decision.recorded".to_owned(),
                "decision.recorded@1".to_owned()
            ),
            (
                "claim.superseded".to_owned(),
                "claim.superseded@1".to_owned()
            ),
        ]
    );
}

/// T12 S-7 (M2b): a reserved Event of `memproved` on a Session that does
/// not continue the Task is ignored, even when its payload names the Task.
#[test]
fn reserved_types_on_a_session_of_another_task_are_ignored() {
    let fixture = Fixture::new();
    let (_, checkpoint) = fixture.checkpointed();
    let other_task = fixture.task(Some(&fixture.workspace_id.clone()));
    let other_session = fixture.session(&fixture.workspace_id.clone(), Some(&other_task));
    for (event_type, payload) in fixture.reserved_payloads(&checkpoint) {
        fixture.append_on(
            &other_session,
            event_type,
            &format!("{event_type}@1"),
            &payload,
        );
    }
    let (_, capsule) = fixture.capsule(None);
    let claims = capsule["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 3);
    assert!(claims.iter().all(|claim| claim["standing"] == "active"));
    assert!(claims.iter().all(|claim| claim["kind"] != "decision"));
    assert_eq!(capsule["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(capsule["sessions"][0]["end_state"], "open");
    assert_eq!(capsule["status"], "verified");
}

/// T12 N5 (Owner, P3-C2): a fact is superseded only by a fact. Superseding
/// one with an open item or a Decision is `400` naming the rule, nothing
/// written; withdrawing it, or superseding it with a fact, is accepted. An
/// edge already in the ledger from a non-fact onto a fact changes no
/// standing.
#[test]
#[allow(clippy::too_many_lines)] // Write refusals, then the read path.
fn a_fact_is_superseded_only_by_a_fact() {
    let fixture = Fixture::new();
    let (evidence, checkpoint) = fixture.checkpointed();
    let fact = format!("{checkpoint}/verified_completed_work/0");
    let events = fixture.events().len();
    let passed = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    let snapshot = passed["snapshot"]["snapshot_id"].as_str().unwrap();
    let new_evidence = passed["evidence_id"].as_str().unwrap();
    let rule = "names a fact, which only a verified_fact statement can supersede; record the new \
                fact, or withdraw this one";
    let mut open_supersedes = fixture.checkpoint_body(snapshot, &[new_evidence]);
    open_supersedes["open_items"][0]["supersedes"] = json!([fact]);
    let refused = fixture.checkpoint_with(&fixture.writer, &fixture.key(), &open_supersedes);
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    assert_eq!(
        serde_json::from_str::<Value>(&refused.body).unwrap()["detail"],
        format!("$.open_items[0].supersedes[0] {rule}")
    );
    let refused = fixture.decision_with(
        Some(&fixture.writer),
        &fixture.key(),
        &fixture.decision_body("drop it", &[], &[&fact]),
    );
    problem(
        &refused,
        OperationId::ProposeDecision,
        400,
        "invalid_request",
    );
    assert_eq!(
        serde_json::from_str::<Value>(&refused.body).unwrap()["detail"],
        format!("$.supersedes[0] {rule}")
    );
    // Nothing was written but the run.
    assert_eq!(fixture.events().len(), events + 1);
    // A fact superseding the fact is accepted.
    let mut fact_supersedes = fixture.checkpoint_body(snapshot, &[new_evidence]);
    fact_supersedes["verified_completed_work"][0]["supersedes"] = json!([fact]);
    let created = ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &fact_supersedes),
        OperationId::CreateCheckpoint,
        201,
    );
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(capsule["claims"][0]["standing"], "superseded");
    let second = created["checkpoint_id"].as_str().unwrap();

    // Read path: an edge in the ledger from an open item onto a fact is not
    // applied (written straight to the store, bypassing the write check).
    let fresh = Fixture::new();
    let (_, checkpoint) = fresh.checkpointed();
    let (_, later) = fresh.checkpointed();
    fresh.append_on(
        &fresh.session_id,
        "claim.superseded",
        "claim.superseded@1",
        &json!({
            "task_id": fresh.task_id, "relation": "supersedes",
            "target": format!("{checkpoint}/verified_completed_work/0"),
            "by": format!("{later}/open_items/0"),
        }),
    );
    let (_, capsule) = fresh.capsule(None);
    assert_eq!(capsule["claims"][0]["kind"], "fact");
    assert_eq!(capsule["claims"][0]["standing"], "active");
    assert_eq!(capsule["claims"][0]["superseded_by"], json!([]));
    // Withdrawal takes any kind.
    let withdrawal = fixture.checkpoint_body(snapshot, &[new_evidence]);
    let mut withdrawal = withdrawal;
    withdrawal["withdrawals"] = json!([format!("{second}/verified_completed_work/0")]);
    // P3-D2: a withdrawn fact needs a reason.
    withdrawal["withdrawal_reason"] = json!("superseded elsewhere");
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &withdrawal),
        OperationId::CreateCheckpoint,
        201,
    );
    let _ = evidence;
}

/// T12 N6 (Owner, P3-C2): `capsule.recovered` is reserved to `memproved`
/// before any writer exists; a client's is `400 invalid_request`.
#[test]
fn a_client_cannot_write_capsule_recovered() {
    let fixture = Fixture::new();
    assert!(memprove_domain::DAEMON_RESERVED_EVENT_TYPES.contains(&"capsule.recovered"));
    let envelope = json!({
        "envelope_version": "1.0",
        "event_id": uuid::Uuid::now_v7().to_string(),
        "project_id": fixture.project_id,
        "workspace_id": fixture.workspace_id,
        "session_id": fixture.session_id,
        "event_type": "capsule.recovered",
        "payload_schema": "capsule.recovered@1",
        "producer": { "name": "agent", "version": "1", "sequence": 1 },
        "privacy_class": "internal",
        "payload": { "task_id": fixture.task_id },
    });
    let events = fixture.events().len();
    for bearer in [&fixture.writer, &fixture.control] {
        let refused = ingest_event(
            fixture.state(),
            &json_request(Some(bearer), Some(&fixture.key()), &envelope.to_string()),
        );
        problem(&refused, OperationId::IngestEvent, 400, "invalid_request");
        assert!(
            serde_json::from_str::<Value>(&refused.body).unwrap()["detail"]
                .as_str()
                .unwrap()
                .starts_with("$.event_type"),
            "{}",
            refused.body
        );
    }
    assert_eq!(fixture.events().len(), events);
}

/// T12 S-7 (M1c): a `verified_fact` resting on a passed command and no
/// passed test, recorded through the store's bundle path (which predates
/// the P2-E rule and does not apply it), is `partial` in the Capsule as in
/// the Preview, on an intact ledger.
#[test]
#[allow(clippy::too_many_lines)] // One legacy bundle, built by hand.
fn a_fact_on_a_passed_command_alone_is_partial_in_both() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let task_id = memprove_domain::TaskId::parse(&fixture.task_id).unwrap();
    let workspace_id = memprove_domain::WorkspaceId::parse(&fixture.workspace_id).unwrap();
    let context = store
        .begin_checkpoint_capture(&task_id, &workspace_id)
        .unwrap();
    let at = |value: &str| memprove_domain::UtcTimestamp::parse(value).unwrap();
    let command = memprove_domain::Evidence::new(memprove_domain::EvidenceInput {
        evidence_id: EvidenceId::new(),
        task_id: task_id.clone(),
        workspace_id: workspace_id.clone(),
        kind: memprove_domain::EvidenceKind::CommandResult,
        subject: memprove_domain::EvidenceSubject::CommandResult {
            command: "make build".to_owned(),
        },
        observed_at: at("2026-10-03T10:00:00Z"),
        collector: memprove_domain::EvidenceCollector {
            name: "memprove-cli".to_owned(),
            version: "0.1.0".to_owned(),
        },
        payload: EvidencePayload::CommandResult {
            argv: vec!["make".to_owned(), "build".to_owned()],
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            started_at: None,
            finished_at: None,
            stdout_sha256: None,
            stderr_sha256: None,
            launch_error: None,
        },
    })
    .unwrap();
    let command_id = command.evidence_id().clone();
    let captured = memprove_repo::RepositorySnapshotCollector::capture(
        context.worktree_root(),
        memprove_repo::RepositoryCaptureRequest {
            project_id: memprove_domain::ProjectId::parse(&fixture.project_id).unwrap(),
            task_id: task_id.clone(),
            workspace_id: workspace_id.clone(),
            snapshot_id: RepositorySnapshotId::new(),
            git_head_evidence_id: EvidenceId::new(),
            git_worktree_evidence_id: EvidenceId::new(),
            selected_files: vec![],
            command_evidence: vec![command],
            test_evidence: vec![],
            observed_at: at("2026-10-03T10:00:00Z"),
            captured_at: at("2026-10-03T10:00:01Z"),
            collector: memprove_domain::EvidenceCollector {
                name: "memproved-git".to_owned(),
                version: "0.1.0".to_owned(),
            },
        },
    )
    .unwrap();
    // The bundle path counts only the Events of Sessions (it predates the
    // Session-less Task Events of P2-C); the column is aligned with the
    // ledger below, outside the digest.
    let sequence = context.project_event_sequence();
    let statement = |classification, text: &str, evidence: Vec<EvidenceId>| {
        memprove_domain::CheckpointStatement::new(classification, text.to_owned(), evidence)
            .unwrap()
    };
    let input = CheckpointInput {
        checkpoint_id: memprove_domain::CheckpointId::new(),
        task_id: task_id.clone(),
        session_id: memprove_domain::SessionId::parse(&fixture.session_id).unwrap(),
        snapshot_id: captured.snapshot.snapshot_id().clone(),
        project_event_sequence: sequence,
        objective: "build".to_owned(),
        verified_completed_work: vec![],
        open_items: vec![statement(
            memprove_domain::CheckpointClassification::VerifiedFact,
            "it builds",
            vec![command_id],
        )],
        next_action: statement(
            memprove_domain::CheckpointClassification::Recommendation,
            "add a test",
            vec![],
        ),
        created_at: at("2026-10-03T10:00:02Z"),
    };
    // The API path refuses it today (P2-E).
    assert!(Checkpoint::create(input.clone(), &captured.snapshot, &captured.evidence).is_err());
    let checkpoint = Checkpoint::new(input, &captured.snapshot, &captured.evidence).unwrap();
    store
        .commit_checkpoint_bundle(
            &context,
            &captured.evidence,
            &captured.snapshot,
            &captured.probe,
            &checkpoint,
            &memprove_domain::EventId::new(),
            &at("2026-10-03T10:00:03Z"),
            &memprove_domain::IdempotencyContext {
                principal_id: memprove_domain::PrincipalId::parse("legacy-writer").unwrap(),
                operation_id: memprove_domain::OperationId::parse("commitCheckpointBundle")
                    .unwrap(),
                key: memprove_domain::IdempotencyKey::parse("legacy-fact-bundle").unwrap(),
            },
            &memprove_domain::Sha256Digest::of_bytes(b"legacy fact bundle"),
        )
        .unwrap();
    drop(store);
    let announced = fixture
        .events()
        .iter()
        .find(|event| {
            event["event_type"] == "checkpoint.created"
                && event["payload"]["checkpoint"]["checkpoint_id"]
                    == checkpoint.checkpoint_id().as_str()
        })
        .unwrap()["producer"]["sequence"]
        .as_u64()
        .unwrap();
    fixture.tamper(&format!(
        "UPDATE checkpoints SET project_event_sequence = {} WHERE checkpoint_id = {};",
        announced - 1,
        quote(checkpoint.checkpoint_id().as_str())
    ));
    // Precondition: the ledger holds; nothing is damaged.
    assert!(fixture.ledger_valid());
    let ((_, preview), (_, capsule)) = fixture.documents();
    assert_eq!(preview["status"], "partial");
    assert_eq!(preview["facts"][0]["classification"], "verified_fact");
    assert_eq!(preview["facts"][0]["verdict"], "partial");
    assert_eq!(preview["facts"][0]["evidence"][0]["verdict"], "verified");
    assert_eq!(capsule["status"], "partial");
    assert_eq!(capsule["claims"][0]["kind"], "fact");
    assert_eq!(capsule["claims"][0]["verdict"], "partial");
    assert_eq!(
        capsule["claims"][0]["reasons"],
        preview["facts"][0]["reasons"]
    );
    assert_eq!(
        strings(&capsule["claims"][0]["reasons"]),
        ["evidence_unavailable"]
    );
}
