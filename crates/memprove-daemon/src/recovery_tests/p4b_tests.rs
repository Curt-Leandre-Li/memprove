//! Narrow tests of P4-B item D (Owner, P4-0 preflight D): "the first
//! Snapshot that seals an Evidence object" is the one the ledger records
//! first, in the three places that pick it: a Decision's Evidence, Task
//! completion, and same-state carry-over. `captured_at` is in no digest and
//! no Event, so rewriting it must change none of them.
//!
//! The same Evidence sealed again at a new HEAD cannot be reached through
//! `captureEvidence` (the engine gives every captured run a new identity);
//! the store accepts an Evidence object identical to a recorded one
//! (`insert_or_replay_evidence`), so the tests seal it again through the
//! store's capture path, as `memproved` itself would record it.

use memprove_domain::{
    EventId, EvidenceCollector, EvidenceId, EvidenceKind, IdempotencyContext, IdempotencyKey,
    OperationId as DomainOperationId, PrincipalId, ProjectId, RepositorySnapshotId, SessionId,
    Sha256Digest, TaskId, UtcTimestamp, WorkspaceId,
};
use memprove_protocol::phase1::OperationId;
use memprove_store_sqlite::EvidenceCapture;
use serde_json::{Value, json};

use super::{Fixture, completion, ok, problem};
use crate::recovery::get_evidence;

impl Fixture {
    /// Seals the recorded Evidence `evidence_id` again into a new Snapshot
    /// of the current repository state, through the store's capture path,
    /// with its `command.completed` Event; returns the new Snapshot.
    pub(super) fn reseal(&self, evidence_id: &str) -> String {
        let mut store = self.store();
        let evidence = store
            .get_evidence_record(&EvidenceId::parse(evidence_id).unwrap())
            .unwrap()
            .evidence;
        let task_id = TaskId::parse(&self.task_id).unwrap();
        let workspace_id = WorkspaceId::parse(&self.workspace_id).unwrap();
        let context = store
            .begin_checkpoint_capture(&task_id, &workspace_id)
            .unwrap();
        let (command_evidence, test_evidence) = match evidence.kind() {
            EvidenceKind::TestResult => (Vec::new(), vec![evidence.clone()]),
            _ => (vec![evidence.clone()], Vec::new()),
        };
        let now = UtcTimestamp::now();
        let captured = memprove_repo::RepositorySnapshotCollector::capture(
            context.worktree_root(),
            memprove_repo::RepositoryCaptureRequest {
                project_id: ProjectId::parse(&self.project_id).unwrap(),
                task_id,
                workspace_id,
                snapshot_id: RepositorySnapshotId::new(),
                git_head_evidence_id: EvidenceId::new(),
                git_worktree_evidence_id: EvidenceId::new(),
                selected_files: Vec::new(),
                command_evidence,
                test_evidence,
                observed_at: now.clone(),
                captured_at: now.clone(),
                collector: EvidenceCollector {
                    name: "memproved-git".to_owned(),
                    version: "0.1.0".to_owned(),
                },
            },
        )
        .unwrap();
        let key = self.key();
        store
            .commit_evidence_capture(
                &EvidenceCapture {
                    context: &context,
                    evidence: &captured.evidence,
                    snapshot: &captured.snapshot,
                    probe: &captured.probe,
                    subject_evidence_id: evidence.evidence_id(),
                    session_id: &SessionId::parse(&self.session_id).unwrap(),
                    event_id: &EventId::new(),
                    recorded_at: &now,
                },
                &IdempotencyContext {
                    principal_id: PrincipalId::parse("p4b-reseal").unwrap(),
                    operation_id: DomainOperationId::parse("captureEvidence").unwrap(),
                    key: IdempotencyKey::parse(&key).unwrap(),
                },
                &Sha256Digest::of_bytes(key.as_bytes()),
            )
            .unwrap();
        captured.snapshot.snapshot_id().as_str().to_owned()
    }

    /// Rewrites a Snapshot row's `captured_at` behind `memproved`.
    pub(super) fn backdate(&self, snapshot_id: &str) {
        self.tamper(&format!(
            "UPDATE repository_snapshots SET captured_at = '2000-01-01T00:00:00Z' \
             WHERE snapshot_id = '{snapshot_id}';"
        ));
    }

    /// Changes the README and commits it; returns the new HEAD.
    pub(super) fn next_commit(&self, contents: &str) -> String {
        std::fs::write(self.repo.join("README"), contents).unwrap();
        self.commit(contents)
    }

    fn passed_test(&self) -> (String, String) {
        let run = self.capture(&self.run_body("test_run", 0, Some(3)));
        (
            run["evidence_id"].as_str().unwrap().to_owned(),
            run["snapshot"]["snapshot_id"].as_str().unwrap().to_owned(),
        )
    }
}

/// The Decision of a Capsule and its one Evidence item.
fn decision(capsule: &Value) -> (&Value, &Value) {
    let claim = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["kind"] == "decision")
        .unwrap();
    (claim, &claim["evidence"][0])
}

/// The command and test Evidence a capture response's Snapshot seals.
fn sealed(run: &Value) -> Vec<String> {
    ["command_evidence_ids", "test_evidence_ids"]
        .iter()
        .flat_map(|name| run["snapshot"][name].as_array().unwrap().clone())
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

/// D, the Decision: its Evidence is judged through the Snapshot the ledger
/// records first (X1, `stale` at X2), before and after the later sealer's
/// `captured_at` is moved before the first one's. U1 B1 (T21 V1R-1): the
/// moved `captured_at` is now itself a finding (the later Snapshot is a
/// damaged record and the Capsule `tampered`); the Decision still names
/// the first sealer in the ledger.
#[test]
fn a_decision_judges_its_evidence_through_the_first_sealer_in_the_ledger() {
    let fixture = Fixture::new();
    let (evidence, first) = fixture.passed_test();
    fixture.decide("keep the parser", &[&evidence], &[]);
    let (_, fresh) = fixture.capsule(None);
    assert_eq!(decision(&fresh).0["verdict"], "verified");
    fixture.next_commit("second\n");
    let later = fixture.reseal(&evidence);
    assert_ne!(later, first);
    let (before, capsule) = fixture.capsule(None);
    let (claim, item) = decision(&capsule);
    assert_eq!(item["snapshot_id"], first.as_str());
    assert_eq!(claim["verdict"], "stale");
    assert!(fixture.ledger_valid());
    fixture.backdate(&later);
    assert!(!fixture.ledger_valid());
    let (after, capsule) = fixture.capsule(None);
    let (claim, item) = decision(&capsule);
    assert_eq!(item["snapshot_id"], first.as_str());
    assert_ne!(claim["verdict"], "verified");
    assert_eq!(capsule["status"], "tampered", "{after}");
    assert!(
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["record_type"] == "snapshot" && record["record_id"] == later),
        "{after}"
    );
    assert_ne!(after, before);
}

/// D, Task completion: the cited Evidence is judged through the Snapshot
/// the ledger records first, so a pass of X1 sealed again at X2 completes
/// nothing at X2, whatever the rows' `captured_at` say. U1 B1: a moved
/// `captured_at` is now a finding of its own, so the row is put back
/// before the fresh pass completes the Task.
#[test]
fn completion_judges_its_evidence_through_the_first_sealer_in_the_ledger() {
    let fixture = Fixture::new();
    let task = fixture.task_id.clone();
    fixture.step(&task, 1, "ready");
    fixture.step(&task, 2, "in_progress");
    let (evidence, _) = fixture.passed_test();
    fixture.next_commit("second\n");
    let later = fixture.reseal(&evidence);
    let refuse = || {
        problem(
            &fixture.transition_with(&task, &fixture.key(), &completion(3, &[&evidence])),
            OperationId::TransitionTask,
            409,
            "evidence_stale",
        );
    };
    refuse();
    let captured_at = fixture.column(&format!(
        "SELECT captured_at FROM repository_snapshots WHERE snapshot_id = '{later}';"
    ));
    fixture.backdate(&later);
    assert!(!fixture.ledger_valid());
    refuse();
    fixture.tamper(&format!(
        "UPDATE repository_snapshots SET captured_at = '{}' WHERE snapshot_id = '{later}';",
        captured_at[0]
    ));
    assert!(fixture.ledger_valid());
    // A fresh pass at X2 completes it.
    let (fresh, _) = fixture.passed_test();
    let done = ok(
        &fixture.transition_with(&task, &fixture.key(), &completion(3, &[&fresh])),
        OperationId::TransitionTask,
        200,
    );
    assert_eq!(done["state"], "completed");
}

/// D, carry-over: an object is carried only from the Snapshot the ledger
/// records first as sealing it. A pass of X1 sealed again at X2 is not
/// carried into the next run at X2, whatever the rows' `captured_at` say
/// (U1 B1: a moved `captured_at` is now also a finding of its own).
#[test]
fn carry_over_takes_the_first_sealer_in_the_ledger() {
    let fixture = Fixture::new();
    let (evidence, first) = fixture.passed_test();
    fixture.next_commit("second\n");
    let later = fixture.reseal(&evidence);
    fixture.backdate(&later);
    assert!(!fixture.ledger_valid());
    let next = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    assert_eq!(sealed(&next), [next["evidence_id"].as_str().unwrap()]);
    // `getEvidence` lists the sealers in the same order, the first first.
    let read = ok(
        &get_evidence(fixture.state(), Some(&fixture.control), &evidence),
        OperationId::GetEvidence,
        200,
    );
    assert_eq!(read["snapshot"]["snapshot_id"], first.as_str());
    assert_eq!(read["snapshot_ids"], json!([first, later]));
}
