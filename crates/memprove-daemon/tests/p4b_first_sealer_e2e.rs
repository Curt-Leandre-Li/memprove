//! P4-B item D (Owner, P4-0 preflight D) through the real binaries: the
//! same Evidence sealed again at a new HEAD, then the later Snapshot's
//! `captured_at` moved before the first one's. A Decision's Evidence, Task
//! completion, and carry-over all keep taking the Snapshot the ledger
//! records first.
//!
//! `captureEvidence` gives every run a new identity, so no request sends
//! the same Evidence twice; the store's capture path accepts an Evidence
//! object identical to a recorded one, and this test seals it again through
//! that path while `memproved` is stopped, as `memproved` would record
//! it (a `command.completed` Event on the agent's Session). Then it rewrites
//! `captured_at` with `sqlite3`, which no digest covers. U1 B1 (T21
//! V1R-1): the rewrite is now itself found (the `captured_at` is not the
//! `recorded_at` of the capture's Event), so `ledger verify` exits 5 and
//! the Capsule is `tampered`; the first sealer is still the ledger's.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use memprove_domain::{
    EventId, EvidenceCollector, EvidenceId, IdempotencyContext, IdempotencyKey, OperationId,
    PrincipalId, ProjectId, RepositorySnapshotId, SessionId, Sha256Digest, TaskId, UtcTimestamp,
    WorkspaceId,
};
use memprove_store_sqlite::{EvidenceCapture, RegistryStore, StateStore};
use p4b_support::{Setup, commit, step, write};
use serde_json::Value;

/// Seals the recorded test Evidence `evidence_id` again into a new Snapshot
/// of the repository's current state through the store's capture path, on
/// `session_id`; returns the new Snapshot. `memproved` must be stopped.
fn reseal(setup: &Setup, begun: &Value, evidence_id: &str) -> String {
    let data_dir = &setup.operator.data_dir;
    let registry = RegistryStore::open(data_dir).unwrap();
    let project_id = ProjectId::parse(&setup.project_id).unwrap();
    let project = registry.get_project(&project_id).unwrap();
    let mut store = StateStore::open(data_dir, &project).unwrap();
    let evidence = store
        .get_evidence_record(&EvidenceId::parse(evidence_id).unwrap())
        .unwrap()
        .evidence;
    let id = |name: &str| begun[name].as_str().unwrap().to_owned();
    let task_id = TaskId::parse(&id("task_id")).unwrap();
    let workspace_id = WorkspaceId::parse(&id("workspace_id")).unwrap();
    let context = store
        .begin_checkpoint_capture(&task_id, &workspace_id)
        .unwrap();
    let now = UtcTimestamp::now();
    let captured = memprove_repo::RepositorySnapshotCollector::capture(
        context.worktree_root(),
        memprove_repo::RepositoryCaptureRequest {
            project_id,
            task_id,
            workspace_id,
            snapshot_id: RepositorySnapshotId::new(),
            git_head_evidence_id: EvidenceId::new(),
            git_worktree_evidence_id: EvidenceId::new(),
            selected_files: Vec::new(),
            command_evidence: Vec::new(),
            test_evidence: vec![evidence.clone()],
            observed_at: now.clone(),
            captured_at: now.clone(),
            collector: EvidenceCollector {
                name: "memproved-git".to_owned(),
                version: "0.1.0".to_owned(),
            },
        },
    )
    .unwrap();
    store
        .commit_evidence_capture(
            &EvidenceCapture {
                context: &context,
                evidence: &captured.evidence,
                snapshot: &captured.snapshot,
                probe: &captured.probe,
                subject_evidence_id: evidence.evidence_id(),
                session_id: &SessionId::parse(&id("session_id")).unwrap(),
                event_id: &EventId::new(),
                recorded_at: &now,
            },
            &IdempotencyContext {
                principal_id: PrincipalId::parse("p4b-reseal").unwrap(),
                operation_id: OperationId::parse("captureEvidence").unwrap(),
                key: IdempotencyKey::parse("p4b-reseal-1").unwrap(),
            },
            &Sha256Digest::of_bytes(b"p4b reseal"),
        )
        .unwrap();
    captured.snapshot.snapshot_id().as_str().to_owned()
}

/// The Decision of a Capsule and its one Evidence item.
fn decision(capsule: &Value) -> (Value, Value) {
    let claim = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["kind"] == "decision")
        .unwrap()
        .clone();
    let item = claim["evidence"][0].clone();
    (claim, item)
}

#[test]
fn the_first_sealer_is_the_ledger_s_through_real_binaries() {
    let mut setup = Setup::new(&["a"]);
    let agent = setup.agent.clone();
    step("X1: a passes (r1); a Decision cites it");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "First sealer",
    ]);
    let (_, run) = agent.run_command(&["--test-format", "cargo"], &["sh", "tests/a.sh"]);
    let evidence = run["evidence_id"].as_str().unwrap().to_owned();
    let first = run["snapshot"]["snapshot_id"].as_str().unwrap().to_owned();
    agent.ok(&["decide", "keep the shell tests", "--evidence", "r1"]);
    step("X2: the same Evidence sealed again, then its captured_at moved first");
    write(&setup.repo.join("README"), "fixture\nsecond\n");
    commit(&setup.repo, "README: second");
    setup.stop();
    let later = reseal(&setup, &begun, &evidence);
    setup.tamper(&format!(
        "UPDATE repository_snapshots SET captured_at = '2000-01-01T00:00:00Z' \
         WHERE snapshot_id = '{later}';"
    ));
    println!(
        "captured_at: first {}, later {}",
        setup.query(&format!(
            "SELECT captured_at FROM repository_snapshots WHERE snapshot_id = '{first}'"
        )),
        setup.query(&format!(
            "SELECT captured_at FROM repository_snapshots WHERE snapshot_id = '{later}'"
        ))
    );
    setup.start();
    let verified = agent.run(&["ledger", "verify"], false);
    assert_eq!(verified.code, Some(5), "U1 B1: the rewrite is found");
    let report: Value = serde_json::from_str(verified.stdout.trim()).unwrap();
    assert_eq!(report["first_failure"]["record_type"], "snapshot");
    assert_eq!(report["first_failure"]["record_id"], later.as_str());

    step("the Decision's Evidence is judged through the first sealer, X1");
    let capsule = agent.capsule();
    assert_eq!(capsule["status"], "tampered");
    let (claim, item) = decision(&capsule);
    assert_eq!(item["snapshot_id"], first.as_str());
    assert_ne!(item["verdict"], "verified");
    assert_ne!(claim["verdict"], "verified");

    step("completion with that Evidence is refused at X2");
    let refused = agent.run(&["task", "complete", "--evidence", &evidence], false);
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("evidence_stale"),
        "{}",
        refused.stderr
    );

    step("a new run at X2 does not carry it over");
    let (_, next) = agent.run_command(&["--test-format", "cargo"], &["sh", "tests/a.sh"]);
    let sealed = ["command_evidence_ids", "test_evidence_ids"]
        .iter()
        .flat_map(|name| next["snapshot"][name].as_array().unwrap().clone())
        .collect::<Vec<_>>();
    assert_eq!(sealed, [next["evidence_id"].clone()]);
    setup.stop();
}
