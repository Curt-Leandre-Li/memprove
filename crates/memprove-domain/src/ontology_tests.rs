//! Table-driven tests of the propagation rules P1–P6 and verdict precedence.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::{
    Checkpoint, CheckpointInput, CheckpointStatement, CheckpointView, EventSummary,
    EvidenceCollector, EvidenceInput, LAUNCH_FAILED_EXIT_CODE, ProjectId, RecoveryDomainError,
    RepositorySnapshotId, RepositorySnapshotInput, SessionId, Task, TaskId, TaskLedgerView,
    TaskStatus, UtcTimestamp, recovery_preview,
};

const X: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const Y: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

use ReasonCode::{
    EvidenceNotPassed, EvidenceStale, EvidenceUnavailable, HeadChanged, IndexChanged,
    LedgerChainInvalid, ObjectDigestMismatch, ObjectMissing, ProjectMismatch, WorkspaceMismatch,
    WorktreeChanged,
};
use Verdict::{Partial, Stale, Tampered, Verified};

fn at(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).unwrap()
}

fn digest(label: &str) -> Sha256Digest {
    Sha256Digest::of_bytes(label.as_bytes())
}

fn judgement(verdict: Verdict, reasons: &[ReasonCode]) -> Judgement {
    Judgement::new(verdict, reasons.iter().copied())
}

/// The tests written before P3-D3 judge with no later runs: an empty
/// [`RunHistory`]. The P3-D3 tests call the module's functions directly.
fn judge_evidence(
    snapshot: &RepositorySnapshot,
    snapshot_evidence: &[Evidence],
    evidence: &Evidence,
    probe: &Probe,
) -> Judgement {
    super::judge_evidence(
        snapshot,
        snapshot_evidence,
        evidence,
        &RunHistory::default(),
        probe,
    )
}

fn assess(anchor: Anchor<'_>, ledger_chain_valid: bool, probe: &Probe) -> Assessment {
    super::assess(anchor, ledger_chain_valid, &RunHistory::default(), probe)
}

fn judge_completion(
    task_id: &TaskId,
    evidence: &[CompletionEvidence],
    probe: &Probe,
) -> Result<(), CompletionRefusal> {
    super::judge_completion(task_id, evidence, &RunHistory::default(), probe)
}

/// A Snapshot at X on `main` in Workspace W1 with `git_head`, `git_worktree`,
/// one selected file, and one passed test, and a Checkpoint whose single
/// `verified_fact` "tests pass" cites the test.
struct Fixture {
    workspace_id: WorkspaceId,
    snapshot: RepositorySnapshot,
    evidence: Vec<Evidence>,
    checkpoint: Checkpoint,
    head: EvidenceId,
    worktree: EvidenceId,
    file: EvidenceId,
    test: EvidenceId,
}

/// A passed three-case run of `sh tests/run.sh`.
fn passed_test() -> EvidencePayload {
    test_payload(0, Some(3), None)
}

fn test_payload(exit_code: i32, cases: Option<u64>, launch_error: Option<&str>) -> EvidencePayload {
    EvidencePayload::TestResult {
        argv: vec!["sh".to_owned(), "tests/run.sh".to_owned()],
        exit_code,
        stdout: "3 passed".to_owned(),
        stderr: String::new(),
        passed: EvidencePayload::test_passed(exit_code, cases),
        test_case_count: cases,
        test_format: None,
        started_at: None,
        finished_at: None,
        stdout_sha256: None,
        stderr_sha256: None,
        launch_error: launch_error.map(str::to_owned),
    }
}

impl Fixture {
    fn new(status: &str, selected_path: &str) -> Self {
        Self::with_test(status, selected_path, passed_test())
    }

    /// The Snapshot with `test` as its test Evidence. When `test` did not
    /// pass, `verified_completed_work` is empty, since no Checkpoint can
    /// claim it.
    #[allow(clippy::too_many_lines)] // One complete Snapshot and Checkpoint.
    fn with_test(status: &str, selected_path: &str, test: EvidencePayload) -> Self {
        let test_passed = test.run_status() == Some(RunStatus::Pass);
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let identity = digest("identity W1");
        let collector = EvidenceCollector {
            name: "fixture".to_owned(),
            version: "1".to_owned(),
        };
        let make = |kind, subject, payload| {
            Evidence::new(EvidenceInput {
                evidence_id: EvidenceId::new(),
                task_id: task_id.clone(),
                workspace_id: workspace_id.clone(),
                kind,
                subject,
                observed_at: at("2026-10-02T10:00:00Z"),
                collector: collector.clone(),
                payload,
            })
            .unwrap()
        };
        let head = make(
            EvidenceKind::GitHead,
            EvidenceSubject::GitHead {
                repository_identity: identity.clone(),
            },
            EvidencePayload::GitHead {
                head_oid: X.to_owned(),
                branch: Some("main".to_owned()),
            },
        );
        let worktree = make(
            EvidenceKind::GitWorktree,
            EvidenceSubject::GitWorktree {
                repository_identity: identity.clone(),
            },
            EvidencePayload::GitWorktree {
                status_porcelain_v2: status.to_owned(),
            },
        );
        let file = make(
            EvidenceKind::FileDigest,
            EvidenceSubject::FileDigest {
                path: selected_path.to_owned(),
            },
            EvidencePayload::FileDigest {
                sha256: digest("readme"),
            },
        );
        let test = make(
            EvidenceKind::TestResult,
            EvidenceSubject::TestResult {
                test_name: "suite".to_owned(),
            },
            test,
        );
        let manifest = repository_capture_manifest_digest(
            &identity,
            X,
            Some("main"),
            status,
            &digest("index X"),
            &digest("tracked X"),
            &[(selected_path, &digest("readme"))],
        );
        let evidence = vec![head.clone(), worktree.clone(), file.clone(), test.clone()];
        let snapshot = RepositorySnapshot::new(
            RepositorySnapshotInput {
                snapshot_id: RepositorySnapshotId::new(),
                project_id: ProjectId::new(),
                task_id: task_id.clone(),
                workspace_id: workspace_id.clone(),
                head_oid: X.to_owned(),
                branch: Some("main".to_owned()),
                worktree_status_porcelain_v2: status.to_owned(),
                index_digest: digest("index X"),
                tracked_worktree_digest: digest("tracked X"),
                capture_manifest_digest: manifest,
                git_head_evidence_id: head.evidence_id().clone(),
                git_worktree_evidence_id: worktree.evidence_id().clone(),
                file_evidence_ids: vec![file.evidence_id().clone()],
                command_evidence_ids: vec![],
                test_evidence_ids: vec![test.evidence_id().clone()],
                captured_at: at("2026-10-02T10:00:01Z"),
            },
            &evidence,
        )
        .unwrap();
        let checkpoint = Checkpoint::new(
            CheckpointInput {
                checkpoint_id: CheckpointId::new(),
                task_id,
                session_id: SessionId::new(),
                snapshot_id: snapshot.snapshot_id().clone(),
                project_event_sequence: 2,
                objective: "make the tests pass".to_owned(),
                verified_completed_work: if test_passed {
                    vec![
                        CheckpointStatement::new(
                            CheckpointClassification::VerifiedFact,
                            "tests pass".to_owned(),
                            vec![test.evidence_id().clone()],
                        )
                        .unwrap(),
                    ]
                } else {
                    vec![]
                },
                open_items: vec![
                    CheckpointStatement::new(
                        CheckpointClassification::Unknown,
                        "nothing known".to_owned(),
                        vec![],
                    )
                    .unwrap(),
                ],
                next_action: CheckpointStatement::new(
                    CheckpointClassification::Recommendation,
                    "rerun after the next change".to_owned(),
                    vec![],
                )
                .unwrap(),
                created_at: at("2026-10-02T10:00:02Z"),
            },
            &snapshot,
            &evidence,
        )
        .unwrap();
        Self {
            workspace_id,
            head: head.evidence_id().clone(),
            worktree: worktree.evidence_id().clone(),
            file: file.evidence_id().clone(),
            test: test.evidence_id().clone(),
            snapshot,
            evidence,
            checkpoint,
        }
    }

    fn clean() -> Self {
        Self::new("", "repo://README")
    }

    /// What a probe of W1 sees when nothing changed.
    fn observation(&self) -> RepositoryObservation {
        RepositoryObservation {
            same_repository: true,
            same_workspace: true,
            repository_identity: digest("identity W1"),
            head_oid: X.to_owned(),
            branch: Some("main".to_owned()),
            status_porcelain_v2: self.snapshot.worktree_status_porcelain_v2().to_owned(),
            index_digest: digest("index X"),
            tracked_worktree_digest: digest("tracked X"),
            capture_manifest_digest: self.snapshot.capture_manifest_digest().clone(),
            selected_files: BTreeMap::from([("repo://README".to_owned(), Some(digest("readme")))]),
            untracked_paths: BTreeSet::new(),
            changed_since_base: Some(BTreeSet::new()),
        }
    }

    fn probe(&self, observation: RepositoryObservation) -> Probe {
        Probe {
            workspace_id: Some(self.workspace_id.clone()),
            observation: ProbeObservation::Observed(observation),
        }
    }

    fn judge(&self, id: &EvidenceId, probe: &Probe) -> Judgement {
        let item = self
            .evidence
            .iter()
            .find(|item| item.evidence_id() == id)
            .unwrap();
        judge_evidence(&self.snapshot, &self.evidence, item, probe)
    }

    fn anchor(&self) -> Anchor<'_> {
        Anchor::Intact {
            checkpoint: &self.checkpoint,
            snapshot: &self.snapshot,
            evidence: &self.evidence,
        }
    }
}

type Mutation = fn(&mut RepositoryObservation);
type Expected = (Verdict, &'static [ReasonCode]);

fn head_moved(observation: &mut RepositoryObservation) {
    observation.head_oid = Y.to_owned();
    observation.index_digest = digest("index Y");
    observation.tracked_worktree_digest = digest("tracked Y");
}

fn worktree_edit(observation: &mut RepositoryObservation) {
    observation.status_porcelain_v2 =
        "1 .M N... 100644 100644 100644 0000 0000 README\n".to_owned();
    observation.tracked_worktree_digest = digest("tracked X edited");
    observation
        .selected_files
        .insert("repo://README".to_owned(), Some(digest("readme edited")));
}

#[test]
#[allow(clippy::too_many_lines)] // One table row per repository change.
fn p2_each_evidence_kind_is_judged_by_its_own_dependency_surface() {
    let cases: [(&str, Mutation, [Expected; 4]); 8] = [
        (
            "nothing changed",
            |_| {},
            [
                (Verified, &[]),
                (Verified, &[]),
                (Verified, &[]),
                (Verified, &[]),
            ],
        ),
        (
            "commit Y",
            head_moved,
            [
                (Stale, &[HeadChanged]),
                (Verified, &[]),
                (Verified, &[]),
                (Stale, &[HeadChanged, IndexChanged]),
            ],
        ),
        (
            // A test does not depend on the branch name (Owner decision,
            // P2-C); the git_head Evidence still does.
            "branch switched at the same commit",
            |observation| observation.branch = Some("other".to_owned()),
            [
                (Stale, &[HeadChanged]),
                (Verified, &[]),
                (Verified, &[]),
                (Verified, &[]),
            ],
        ),
        (
            "worktree edit, not committed",
            worktree_edit,
            [
                (Verified, &[]),
                (Stale, &[WorktreeChanged]),
                (Stale, &[WorktreeChanged]),
                (Stale, &[WorktreeChanged]),
            ],
        ),
        (
            "edit staged",
            |observation| {
                worktree_edit(observation);
                observation.status_porcelain_v2 =
                    "1 M. N... 100644 100644 100644 0000 1111 README\n".to_owned();
                observation.index_digest = digest("index staged");
            },
            [
                (Verified, &[]),
                (Stale, &[WorktreeChanged]),
                (Stale, &[WorktreeChanged]),
                (Stale, &[IndexChanged, WorktreeChanged]),
            ],
        ),
        (
            "selected file removed",
            |observation| {
                observation
                    .selected_files
                    .insert("repo://README".to_owned(), None);
            },
            [
                (Verified, &[]),
                (Verified, &[]),
                (Stale, &[WorktreeChanged]),
                (Stale, &[WorktreeChanged]),
            ],
        ),
        (
            "tracked content changed under an unchanged index",
            |observation| observation.tracked_worktree_digest = digest("tracked drift"),
            [
                (Verified, &[]),
                (Verified, &[]),
                (Verified, &[]),
                (Stale, &[WorktreeChanged]),
            ],
        ),
        (
            "new untracked file",
            |observation| {
                observation.status_porcelain_v2 = "? notes.txt\n".to_owned();
                observation.untracked_paths.insert("notes.txt".to_owned());
            },
            [
                (Verified, &[]),
                (Stale, &[WorktreeChanged]),
                (Verified, &[]),
                (Stale, &[WorktreeChanged]),
            ],
        ),
    ];
    let fixture = Fixture::clean();
    for (name, mutate, expected) in cases {
        let mut observation = fixture.observation();
        mutate(&mut observation);
        let probe = fixture.probe(observation);
        let ids = [
            &fixture.head,
            &fixture.worktree,
            &fixture.file,
            &fixture.test,
        ];
        for (id, (verdict, reasons)) in ids.into_iter().zip(expected) {
            assert_eq!(
                fixture.judge(id, &probe),
                judgement(verdict, reasons),
                "{name}: {id}"
            );
        }
    }
}

#[test]
fn p1_the_probe_source_decides_partial_and_tampered_for_everything_downstream() {
    let fixture = Fixture::clean();
    let observed = |mutate: fn(&mut RepositoryObservation)| {
        let mut observation = fixture.observation();
        mutate(&mut observation);
        fixture.probe(observation)
    };
    let other_workspace = {
        let mut observation = fixture.observation();
        observation.repository_identity = digest("identity W2");
        Probe {
            workspace_id: Some(WorkspaceId::new()),
            observation: ProbeObservation::Observed(observation),
        }
    };
    let cases: [(&str, Probe, Expected); 6] = [
        (
            "probe could not run",
            Probe {
                workspace_id: Some(fixture.workspace_id.clone()),
                observation: ProbeObservation::Unavailable,
            },
            (Partial, &[EvidenceUnavailable]),
        ),
        (
            "no Workspace to probe",
            Probe {
                workspace_id: None,
                observation: ProbeObservation::Unavailable,
            },
            (Partial, &[EvidenceUnavailable]),
        ),
        (
            "another repository",
            observed(|observation| observation.same_repository = false),
            (Tampered, &[ProjectMismatch]),
        ),
        (
            "another worktree at the Workspace path",
            observed(|observation| observation.same_workspace = false),
            (Tampered, &[WorkspaceMismatch]),
        ),
        (
            "the Snapshot's Workspace with another identity",
            observed(|observation| observation.repository_identity = digest("identity W9")),
            (Tampered, &[WorkspaceMismatch]),
        ),
        (
            "a linked worktree of the same repository is compared without its location",
            other_workspace,
            (Verified, &[]),
        ),
    ];
    for (name, probe, (verdict, reasons)) in cases {
        for id in [
            &fixture.head,
            &fixture.worktree,
            &fixture.file,
            &fixture.test,
        ] {
            assert_eq!(
                fixture.judge(id, &probe),
                judgement(verdict, reasons),
                "{name}: {id}"
            );
        }
    }
}

#[test]
fn p3_unselected_untracked_files_cap_commands_and_tests_at_partial() {
    let untracked = Fixture::new("? notes.txt\n", "repo://README");
    let selected = Fixture::new("? README\n", "repo://README");
    let cases: [(&str, &Fixture, Mutation, Expected, Expected); 5] = [
        (
            "unselected untracked file, nothing changed",
            &untracked,
            |_| {},
            (Partial, &[EvidenceUnavailable]),
            (Verified, &[]),
        ),
        (
            "unselected untracked file, commit Y",
            &untracked,
            head_moved,
            (Partial, &[EvidenceUnavailable, HeadChanged, IndexChanged]),
            (Verified, &[]),
        ),
        (
            "unselected untracked file, another repository stays tampered",
            &untracked,
            |observation| observation.same_repository = false,
            (Tampered, &[ProjectMismatch]),
            (Tampered, &[ProjectMismatch]),
        ),
        (
            "untracked file captured as selected",
            &selected,
            |_| {},
            (Verified, &[]),
            (Verified, &[]),
        ),
        (
            "untracked file captured as selected, commit Y",
            &selected,
            head_moved,
            (Stale, &[HeadChanged, IndexChanged]),
            (Verified, &[]),
        ),
    ];
    for (
        name,
        fixture,
        mutate,
        (test_verdict, test_reasons),
        (worktree_verdict, worktree_reasons),
    ) in cases
    {
        let mut observation = fixture.observation();
        mutate(&mut observation);
        let probe = fixture.probe(observation);
        assert_eq!(
            fixture.judge(&fixture.test, &probe),
            judgement(test_verdict, test_reasons),
            "{name}: test"
        );
        assert_eq!(
            fixture.judge(&fixture.worktree, &probe),
            judgement(worktree_verdict, worktree_reasons),
            "{name}: git_worktree"
        );
    }
    assert_eq!(
        unselected_untracked(&untracked.snapshot, &untracked.evidence),
        BTreeSet::from(["notes.txt".to_owned()])
    );
    assert!(unselected_untracked(&selected.snapshot, &selected.evidence).is_empty());
}

fn assessed(verdict: Verdict, reasons: &[ReasonCode]) -> EvidenceAssessment {
    EvidenceAssessment {
        evidence_id: EvidenceId::new(),
        kind: EvidenceKind::TestResult,
        canonical_digest: digest("e"),
        argv: None,
        run_status: Some(RunStatus::Pass),
        test_format: None,
        judgement: judgement(verdict, reasons),
    }
}

#[test]
fn p4_a_statement_takes_its_worst_evidence_and_is_partial_without_any() {
    let cases: [(&str, Vec<EvidenceAssessment>, Expected); 6] = [
        ("no Evidence", vec![], (Partial, &[EvidenceUnavailable])),
        (
            "one verified",
            vec![assessed(Verified, &[])],
            (Verified, &[]),
        ),
        (
            "verified and stale",
            vec![assessed(Verified, &[]), assessed(Stale, &[HeadChanged])],
            (Stale, &[HeadChanged, EvidenceStale]),
        ),
        (
            "stale and partial",
            vec![
                assessed(Stale, &[WorktreeChanged]),
                assessed(Partial, &[EvidenceUnavailable]),
            ],
            (
                Partial,
                &[WorktreeChanged, EvidenceStale, EvidenceUnavailable],
            ),
        ),
        (
            "partial only",
            vec![assessed(Partial, &[EvidenceUnavailable])],
            (Partial, &[EvidenceUnavailable]),
        ),
        (
            "tampered and stale",
            vec![
                assessed(Stale, &[HeadChanged]),
                assessed(Tampered, &[ObjectDigestMismatch]),
            ],
            (
                Tampered,
                &[HeadChanged, ObjectDigestMismatch, EvidenceStale],
            ),
        ),
    ];
    for (name, evidence, (verdict, reasons)) in cases {
        assert_eq!(
            judge_statement(&evidence),
            judgement(verdict, reasons),
            "{name}"
        );
    }
}

fn statement(propagates: bool, verdict: Verdict, reasons: &[ReasonCode]) -> StatementAssessment {
    StatementAssessment {
        claim: ClaimRef {
            checkpoint_id: CheckpointId::new(),
            section: StatementSection::VerifiedCompletedWork,
            ordinal: 0,
        },
        classification: if propagates {
            CheckpointClassification::VerifiedFact
        } else {
            CheckpointClassification::Recommendation
        },
        text: "x".to_owned(),
        propagates,
        evidence: vec![],
        judgement: judgement(verdict, reasons),
    }
}

#[test]
fn p5_checkpoint_and_task_take_the_worst_claim_and_are_partial_without_one() {
    let cases: [(&str, Vec<StatementAssessment>, Expected); 5] = [
        ("no statements", vec![], (Partial, &[EvidenceUnavailable])),
        (
            "only non-claims",
            vec![statement(false, Partial, &[EvidenceUnavailable])],
            (Partial, &[EvidenceUnavailable]),
        ),
        (
            "a verified claim beside an unsupported recommendation",
            vec![
                statement(true, Verified, &[]),
                statement(false, Partial, &[EvidenceUnavailable]),
            ],
            (Verified, &[]),
        ),
        (
            "a verified and a stale claim",
            vec![
                statement(true, Verified, &[]),
                statement(true, Stale, &[EvidenceStale, HeadChanged]),
            ],
            (Stale, &[EvidenceStale, HeadChanged]),
        ),
        (
            "a stale and a tampered claim",
            vec![
                statement(true, Stale, &[EvidenceStale]),
                statement(true, Tampered, &[ObjectMissing]),
            ],
            (Tampered, &[EvidenceStale, ObjectMissing]),
        ),
    ];
    for (name, statements, (verdict, reasons)) in cases {
        let checkpoint = judge_checkpoint(&statements);
        assert_eq!(checkpoint, judgement(verdict, reasons), "{name}");
        assert_eq!(judge_task(Some(&checkpoint)), checkpoint, "{name}: task");
    }
    assert_eq!(judge_task(None), judgement(Partial, &[EvidenceUnavailable]));
}

#[test]
fn verdict_precedence_is_tampered_partial_stale_verified() {
    assert_eq!(Verdict::ALL, [Verified, Stale, Partial, Tampered]);
    for (index, left) in Verdict::ALL.into_iter().enumerate() {
        for (other, right) in Verdict::ALL.into_iter().enumerate() {
            let combined = judgement(left, &[HeadChanged]).combine(&judgement(right, &[]));
            assert_eq!(combined.verdict(), Verdict::ALL[index.max(other)]);
        }
    }
    let worst = Judgement::worst(&[
        judgement(Stale, &[HeadChanged]),
        judgement(Verified, &[]),
        judgement(Partial, &[EvidenceUnavailable]),
    ])
    .unwrap();
    assert_eq!(
        worst,
        judgement(Partial, &[HeadChanged, EvidenceUnavailable])
    );
    assert_eq!(Judgement::worst(&[]), None);
    assert!(judgement(Verified, &[HeadChanged]).reasons().is_empty());
    assert_eq!(
        ReasonCode::ALL.map(ReasonCode::as_str),
        [
            "head_changed",
            "index_changed",
            "worktree_changed",
            "object_missing",
            "object_digest_mismatch",
            "ledger_chain_invalid",
            "evidence_stale",
            "evidence_unavailable",
            "evidence_not_passed",
            "project_mismatch",
            "workspace_mismatch"
        ]
    );
}

#[test]
fn p6_propagation_is_one_direction_over_five_fixed_hops() {
    assert_eq!(PROPAGATION_PATH.len(), 5);
    assert_eq!(PROPAGATION_PATH[0].from, Node::Probe);
    assert_eq!(PROPAGATION_PATH[4].to, Node::Task);
    let mut seen = BTreeSet::from([Node::Probe]);
    for pair in PROPAGATION_PATH.windows(2) {
        assert_eq!(pair[0].to, pair[1].from);
    }
    for hop in PROPAGATION_PATH {
        assert!(seen.insert(hop.to), "{:?} is visited twice", hop.to);
    }
    // Downstream verdicts never improve on upstream ones.
    let fixture = Fixture::clean();
    for mutate in [
        (|_| {}) as Mutation,
        head_moved,
        worktree_edit,
        |observation| observation.same_repository = false,
    ] {
        let mut observation = fixture.observation();
        mutate(&mut observation);
        let probe = fixture.probe(observation);
        let assessment = assess(fixture.anchor(), true, &probe);
        let claim = &assessment.facts[0];
        for item in &claim.evidence {
            assert!(claim.judgement.verdict() >= item.judgement.verdict());
        }
        assert!(assessment.judgement.verdict() >= claim.judgement.verdict());
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Every anchor state through the whole path.
fn assess_walks_the_whole_path_for_each_anchor_state() {
    let fixture = Fixture::clean();
    let unchanged = fixture.probe(fixture.observation());
    let moved = {
        let mut observation = fixture.observation();
        head_moved(&mut observation);
        observation.changed_since_base = Some(BTreeSet::from(["README".to_owned()]));
        fixture.probe(observation)
    };
    let gone = Probe {
        workspace_id: Some(fixture.workspace_id.clone()),
        observation: ProbeObservation::Unavailable,
    };
    let damaged_ids = [fixture.test.clone()];

    let verified = assess(fixture.anchor(), true, &unchanged);
    assert_eq!(verified.judgement, Judgement::verified());
    assert!(verified.next_checks.is_empty());
    assert!(verified.invalid_evidence_ids.is_empty());
    // The open item and next action cite nothing, but they do not propagate,
    // so they leave no gap (Owner decision, P2-C).
    assert_eq!(verified.gaps, BTreeSet::new());
    assert_eq!(verified.facts.len(), 3);
    assert_eq!(
        verified
            .facts
            .iter()
            .map(|fact| (
                fact.claim.section,
                fact.propagates,
                fact.judgement.verdict()
            ))
            .collect::<Vec<_>>(),
        [
            (StatementSection::VerifiedCompletedWork, true, Verified),
            (StatementSection::OpenItems, false, Partial),
            (StatementSection::NextAction, false, Partial),
        ]
    );

    let stale = assess(fixture.anchor(), true, &moved);
    assert_eq!(
        stale.judgement,
        judgement(Stale, &[HeadChanged, IndexChanged, EvidenceStale])
    );
    assert_eq!(
        stale.changed_paths,
        ChangedPaths {
            paths: BTreeSet::from(["repo://README".to_owned()]),
            complete: true,
        }
    );
    assert_eq!(
        stale.next_checks,
        [NextCheck {
            evidence_id: fixture.test.clone(),
            argv: vec!["sh".to_owned(), "tests/run.sh".to_owned()],
            workspace_id: Some(fixture.workspace_id.clone()),
            at_head: Some(Y.to_owned()),
        }]
    );
    assert_eq!(
        stale.invalid_evidence_ids,
        BTreeSet::from([fixture.test.clone()])
    );

    let partial = assess(fixture.anchor(), true, &gone);
    assert_eq!(
        partial.judgement,
        judgement(Partial, &[EvidenceUnavailable])
    );
    assert!(partial.gaps.contains(&Gap::WorkspaceUnavailable));
    assert_eq!(partial.next_checks[0].at_head, None);
    assert!(!partial.changed_paths.complete);

    let none = assess(Anchor::None, true, &gone);
    assert_eq!(none.judgement, judgement(Partial, &[EvidenceUnavailable]));
    assert!(none.gaps.contains(&Gap::NoCheckpoint));

    for (missing_object, reason) in [(false, ObjectDigestMismatch), (true, ObjectMissing)] {
        let damaged = assess(
            Anchor::Damaged {
                missing_object,
                invalid_evidence_ids: &damaged_ids,
            },
            true,
            &unchanged,
        );
        assert_eq!(damaged.judgement, judgement(Tampered, &[reason]));
        assert!(damaged.facts.is_empty());
        assert!(damaged.gaps.contains(&Gap::RecordIntegrity));
        assert_eq!(
            damaged.invalid_evidence_ids,
            BTreeSet::from([fixture.test.clone()])
        );
    }

    // A broken ledger chain makes any answer tampered.
    for probe in [&unchanged, &moved, &gone] {
        let broken = assess(fixture.anchor(), false, probe);
        assert_eq!(broken.judgement.verdict(), Tampered);
        assert!(broken.judgement.reasons().contains(&LedgerChainInvalid));
        assert!(broken.gaps.contains(&Gap::LedgerChainInvalid));
    }

    // X garbage-collected: still stale by HEAD, but the changed paths are
    // incomplete.
    let mut observation = fixture.observation();
    head_moved(&mut observation);
    observation.changed_since_base = None;
    observation.untracked_paths.insert("new.txt".to_owned());
    let collected = assess(fixture.anchor(), true, &fixture.probe(observation));
    assert_eq!(collected.judgement.verdict(), Stale);
    assert!(collected.gaps.contains(&Gap::BaseCommitUnavailable));
    assert_eq!(
        collected.changed_paths,
        ChangedPaths {
            paths: BTreeSet::from(["repo://new.txt".to_owned()]),
            complete: false,
        }
    );
}

/// `statement_without_evidence` names only statements that propagate
/// (Owner decision, P2-C): unevidenced statements of any other
/// classification, in any section, leave no gap. An unevidenced
/// `verified_fact`, the only statement that could, is refused when recorded.
#[test]
fn only_a_propagating_statement_without_evidence_is_a_gap() {
    let fixture = Fixture::clean();
    let probe = fixture.probe(fixture.observation());
    let statement = |classification, text: &str, evidence: Vec<EvidenceId>| {
        CheckpointStatement::new(classification, text.to_owned(), evidence).unwrap()
    };
    let with_open_items = |open_items: Vec<CheckpointStatement>| {
        Checkpoint::new(
            CheckpointInput {
                checkpoint_id: CheckpointId::new(),
                task_id: fixture.checkpoint.task_id().clone(),
                session_id: SessionId::new(),
                snapshot_id: fixture.snapshot.snapshot_id().clone(),
                project_event_sequence: 2,
                objective: "make the tests pass".to_owned(),
                verified_completed_work: vec![statement(
                    CheckpointClassification::VerifiedFact,
                    "tests pass",
                    vec![fixture.test.clone()],
                )],
                open_items,
                next_action: statement(CheckpointClassification::Unknown, "none", vec![]),
                created_at: at("2026-10-02T10:00:02Z"),
            },
            &fixture.snapshot,
            &fixture.evidence,
        )
        .unwrap()
    };
    let gaps = |checkpoint: &Checkpoint| {
        assess(
            Anchor::Intact {
                checkpoint,
                snapshot: &fixture.snapshot,
                evidence: &fixture.evidence,
            },
            true,
            &probe,
        )
        .gaps
    };

    let quiet = with_open_items(vec![
        statement(
            CheckpointClassification::UserSuppliedContext,
            "asked",
            vec![],
        ),
        statement(CheckpointClassification::Recommendation, "refactor", vec![]),
        statement(CheckpointClassification::Unknown, "docs?", vec![]),
    ]);
    assert!(!gaps(&quiet).contains(&Gap::StatementWithoutEvidence));

    assert!(
        CheckpointStatement::new(
            CheckpointClassification::VerifiedFact,
            "lint is clean".to_owned(),
            vec![],
        )
        .is_err()
    );
}

#[test]
fn d5_changed_paths_union_the_diff_untracked_and_dirty_snapshot_paths() {
    let dirty = Fixture::new(
        "1 .M N... 100644 100644 100644 0000 0000 src/lib.rs\n? README\n",
        "repo://README",
    );
    let mut observation = dirty.observation();
    observation.changed_since_base = Some(BTreeSet::from(["docs/a b.md".to_owned()]));
    observation.untracked_paths = BTreeSet::from(["new.txt".to_owned()]);
    assert_eq!(
        changed_paths(&dirty.snapshot, &observation),
        ChangedPaths {
            paths: [
                "repo://README",
                "repo://docs/a b.md",
                "repo://new.txt",
                "repo://src/lib.rs"
            ]
            .map(str::to_owned)
            .into(),
            complete: true,
        }
    );
    let clean = Fixture::clean();
    let observation = clean.observation();
    assert_eq!(
        changed_paths(&clean.snapshot, &observation).paths,
        BTreeSet::new()
    );
}

#[test]
fn porcelain_v2_paths_cover_every_entry_kind_and_quoting() {
    let status = "1 .M N... 100644 100644 100644 1111 1111 src/a b.rs\n\
                  2 R. N... 100644 100644 100644 1111 1111 R100 new name.rs\told.rs\n\
                  u UU N... 100644 100644 100644 100644 1111 2222 3333 conflict.rs\n\
                  ? \"caf\\303\\251 \\\"x\\\".txt\"\n\
                  ! ignored.log\n\
                  # branch.oid abc\n";
    let paths = porcelain_v2_paths(status);
    assert_eq!(
        paths.all,
        [
            "café \"x\".txt",
            "conflict.rs",
            "ignored.log",
            "new name.rs",
            "old.rs",
            "src/a b.rs"
        ]
        .map(str::to_owned)
        .into()
    );
    assert_eq!(
        paths.untracked,
        BTreeSet::from(["café \"x\".txt".to_owned()])
    );
    assert_eq!(porcelain_v2_paths(""), PorcelainPaths::default());
}

/// A Checkpoint input over `fixture` whose only `open_items` statement is a
/// `verified_fact` "tests pass" citing `evidence`, as T9 B1 recorded before
/// P2-E.
fn open_item_claim(fixture: &Fixture, evidence: Vec<EvidenceId>) -> CheckpointInput {
    CheckpointInput {
        checkpoint_id: CheckpointId::new(),
        task_id: fixture.checkpoint.task_id().clone(),
        session_id: SessionId::new(),
        snapshot_id: fixture.snapshot.snapshot_id().clone(),
        project_event_sequence: 2,
        objective: "make the tests pass".to_owned(),
        verified_completed_work: vec![],
        open_items: vec![
            CheckpointStatement::new(
                CheckpointClassification::VerifiedFact,
                "tests pass".to_owned(),
                evidence,
            )
            .unwrap(),
        ],
        next_action: CheckpointStatement::new(
            CheckpointClassification::Recommendation,
            "continue".to_owned(),
            vec![],
        )
        .unwrap(),
        created_at: at("2026-10-02T10:00:02Z"),
    }
}

fn completion_of(fixture: &Fixture, id: &EvidenceId) -> CompletionEvidence {
    CompletionEvidence {
        evidence: fixture
            .evidence
            .iter()
            .find(|item| item.evidence_id() == id)
            .unwrap()
            .clone(),
        snapshot: Some(fixture.snapshot.clone()),
        snapshot_evidence: fixture.evidence.clone(),
    }
}

/// P2-E (T9 B1): `verified` needs Evidence that is fresh and passed. A
/// `verified_fact` in any section is refused when created unless its
/// Evidence satisfies the `verified_completed_work` rule, and a record
/// written before that, read back with `Checkpoint::new`, is never
/// `verified`. Task completion refuses the same Evidence through the same
/// `judge_evidence`.
#[test]
#[allow(clippy::too_many_lines)] // Every non-PASS outcome on both sides.
fn p2e_verified_needs_fresh_and_passed_evidence_in_every_section() {
    let failing = [
        ("FAIL", test_payload(1, Some(3), None), RunStatus::Fail),
        (
            "NOT_RUN without a count",
            test_payload(0, None, None),
            RunStatus::NotRun,
        ),
        (
            "NOT_RUN with zero cases",
            test_payload(0, Some(0), None),
            RunStatus::NotRun,
        ),
        (
            "UNAVAILABLE",
            test_payload(LAUNCH_FAILED_EXIT_CODE, None, Some("No such file")),
            RunStatus::Unavailable,
        ),
    ];
    for (name, payload, status) in failing {
        let fixture = Fixture::with_test("", "repo://README", payload);
        let probe = fixture.probe(fixture.observation());
        assert_eq!(
            fixture.judge(&fixture.test, &probe),
            judgement(Partial, &[EvidenceNotPassed]),
            "{name}: fresh but not passed"
        );
        assert_eq!(
            fixture.judge(&fixture.head, &probe),
            Judgement::verified(),
            "{name}: repository Evidence has no outcome"
        );
        let mut moved = fixture.observation();
        head_moved(&mut moved);
        assert_eq!(
            fixture.judge(&fixture.test, &fixture.probe(moved)),
            judgement(Stale, &[HeadChanged, IndexChanged]),
            "{name}: stale and not passed is stale (P2-F)"
        );

        let input = open_item_claim(&fixture, vec![fixture.test.clone()]);
        assert_eq!(
            Checkpoint::create(input.clone(), &fixture.snapshot, &fixture.evidence),
            Err(RecoveryDomainError::InvalidCompletedWorkEvidence),
            "{name}: refused when created"
        );
        let legacy = Checkpoint::new(input, &fixture.snapshot, &fixture.evidence).unwrap();
        let assessment = assess(
            Anchor::Intact {
                checkpoint: &legacy,
                snapshot: &fixture.snapshot,
                evidence: &fixture.evidence,
            },
            true,
            &probe,
        );
        assert_eq!(
            assessment.judgement,
            judgement(Partial, &[EvidenceNotPassed]),
            "{name}: a stored record is not verified"
        );
        let fact = &assessment.facts[0];
        assert_eq!(fact.claim.section, StatementSection::OpenItems);
        assert!(fact.propagates);
        assert_eq!(fact.evidence[0].run_status, Some(status), "{name}");
        assert_eq!(
            assessment.invalid_evidence_ids,
            BTreeSet::from([fixture.test.clone()])
        );
        assert_eq!(assessment.next_checks[0].evidence_id, fixture.test);

        let mut view = view(&fixture);
        view.checkpoint = CheckpointView::Intact {
            checkpoint: legacy,
            snapshot: fixture.snapshot.clone(),
            evidence: fixture.evidence.clone(),
        };
        let document: serde_json::Value =
            serde_json::from_str(recovery_preview(&view, &probe).unwrap().as_str()).unwrap();
        assert_eq!(document["status"], "partial", "{name}");
        assert_eq!(
            document["reasons"],
            serde_json::json!(["evidence_not_passed"])
        );
        assert_eq!(document["facts"][0]["verdict"], "partial");
        assert_eq!(
            document["facts"][0]["evidence"][0]["run_status"],
            status.as_str()
        );

        match judge_completion(
            fixture.checkpoint.task_id(),
            &[completion_of(&fixture, &fixture.test)],
            &probe,
        ) {
            Err(CompletionRefusal::NotVerified(id, refused)) => {
                assert_eq!(id, fixture.test);
                assert_eq!(refused, judgement(Partial, &[EvidenceNotPassed]));
            }
            other => panic!("{name}: {other:?}"),
        }
    }

    // A passed test: the open_items claim is accepted and verified, and the
    // Preview shows the outcome.
    let fixture = Fixture::clean();
    let probe = fixture.probe(fixture.observation());
    let created = Checkpoint::create(
        open_item_claim(&fixture, vec![fixture.test.clone()]),
        &fixture.snapshot,
        &fixture.evidence,
    )
    .unwrap();
    let assessment = assess(
        Anchor::Intact {
            checkpoint: &created,
            snapshot: &fixture.snapshot,
            evidence: &fixture.evidence,
        },
        true,
        &probe,
    );
    assert_eq!(assessment.judgement, Judgement::verified());
    assert_eq!(
        assessment.facts[0].evidence[0].run_status,
        Some(RunStatus::Pass)
    );
    let document: serde_json::Value =
        serde_json::from_str(recovery_preview(&view(&fixture), &probe).unwrap().as_str()).unwrap();
    assert_eq!(document["facts"][0]["evidence"][0]["run_status"], "PASS");

    // A verified_fact resting only on fresh repository Evidence: refused
    // when created, and only partial when read back.
    let input = open_item_claim(&fixture, vec![fixture.head.clone()]);
    assert_eq!(
        Checkpoint::create(input.clone(), &fixture.snapshot, &fixture.evidence),
        Err(RecoveryDomainError::InvalidCompletedWorkEvidence)
    );
    let legacy = Checkpoint::new(input, &fixture.snapshot, &fixture.evidence).unwrap();
    let assessment = assess(
        Anchor::Intact {
            checkpoint: &legacy,
            snapshot: &fixture.snapshot,
            evidence: &fixture.evidence,
        },
        true,
        &probe,
    );
    assert_eq!(
        assessment.judgement,
        judgement(Partial, &[EvidenceUnavailable])
    );
    assert_eq!(
        assessment.facts[0].evidence[0].judgement,
        Judgement::verified()
    );
    assert_eq!(assessment.facts[0].evidence[0].run_status, None);
    // Repository Evidence never completes a Task.
    assert_eq!(
        judge_completion(
            fixture.checkpoint.task_id(),
            &[completion_of(&fixture, &fixture.head)],
            &probe,
        ),
        Err(CompletionRefusal::NotPassed(fixture.head.clone()))
    );
    // Statements that do not claim verification may cite a failed run.
    let failed = Fixture::with_test("", "repo://README", test_payload(1, Some(3), None));
    let mut input = open_item_claim(&failed, vec![failed.test.clone()]);
    input.open_items = vec![
        CheckpointStatement::new(
            CheckpointClassification::Unknown,
            "tests fail at X".to_owned(),
            vec![failed.test.clone()],
        )
        .unwrap(),
    ];
    assert!(Checkpoint::create(input, &failed.snapshot, &failed.evidence).is_ok());
}

/// P2-F: `evidence_not_passed` is the reason exactly when a command or test
/// is still fresh and did not pass. Evidence that is not fresh keeps its
/// freshness verdict whatever its outcome: `stale` when the commit moved
/// (also when the commit is gone, which only adds the gap
/// `base_commit_unavailable`), `partial` with `evidence_unavailable` when
/// the probe cannot run or the capture left untracked files out.
#[test]
#[allow(clippy::too_many_lines)] // One table per outcome, then the records.
fn p2f_fresh_runs_that_did_not_pass_are_evidence_not_passed() {
    fn gone(observation: &mut RepositoryObservation) {
        head_moved(observation);
        observation.changed_since_base = None;
    }
    let outcomes = [
        ("PASS", passed_test(), true),
        ("FAIL", test_payload(1, Some(3), None), false),
        ("NOT_RUN", test_payload(0, Some(0), None), false),
        (
            "UNAVAILABLE",
            test_payload(LAUNCH_FAILED_EXIT_CODE, None, Some("No such file")),
            false,
        ),
    ];
    let unavailable = |fixture: &Fixture| Probe {
        workspace_id: Some(fixture.workspace_id.clone()),
        observation: ProbeObservation::Unavailable,
    };
    for (name, payload, passed) in outcomes {
        let fixture = Fixture::with_test("", "repo://README", payload.clone());
        let observed = |mutate: Mutation| {
            let mut observation = fixture.observation();
            mutate(&mut observation);
            fixture.probe(observation)
        };
        let fresh = if passed {
            Judgement::verified()
        } else {
            judgement(Partial, &[EvidenceNotPassed])
        };
        let cases: [(&str, Probe, Judgement); 4] = [
            ("fresh", observed(|_| {}), fresh),
            (
                "the commit moved",
                observed(head_moved),
                judgement(Stale, &[HeadChanged, IndexChanged]),
            ),
            (
                "the commit is gone",
                observed(gone),
                judgement(Stale, &[HeadChanged, IndexChanged]),
            ),
            (
                "the probe cannot run",
                unavailable(&fixture),
                judgement(Partial, &[EvidenceUnavailable]),
            ),
        ];
        for (case, probe, expected) in cases {
            assert_eq!(
                fixture.judge(&fixture.test, &probe),
                expected,
                "{name}: {case}"
            );
        }
        let collected = assess(fixture.anchor(), true, &observed(gone));
        assert!(
            collected.gaps.contains(&Gap::BaseCommitUnavailable),
            "{name}"
        );

        // Unselected untracked files: the capture cannot be confirmed, so
        // the outcome is not reached.
        let untracked = Fixture::with_test("? notes.txt\n", "repo://README", payload);
        assert_eq!(
            untracked.judge(&untracked.test, &untracked.probe(untracked.observation())),
            judgement(Partial, &[EvidenceUnavailable]),
            "{name}: unselected untracked file"
        );
        if passed {
            continue;
        }

        // A record stored before P2-E whose verified_fact cites the failed
        // run: fresh, it is evidence_not_passed alone; stale, it is never
        // verified, and the missing passed test is evidence_unavailable.
        let input = open_item_claim(&fixture, vec![fixture.test.clone()]);
        let digest = Checkpoint::new(input.clone(), &fixture.snapshot, &fixture.evidence)
            .unwrap()
            .digest()
            .clone();
        let stored =
            Checkpoint::from_stored(input, &digest, &fixture.snapshot, &fixture.evidence).unwrap();
        let anchor = Anchor::Intact {
            checkpoint: &stored,
            snapshot: &fixture.snapshot,
            evidence: &fixture.evidence,
        };
        let fresh = assess(anchor, true, &observed(|_| {}));
        assert_eq!(
            fresh.judgement,
            judgement(Partial, &[EvidenceNotPassed]),
            "{name}: legacy, fresh"
        );
        assert_eq!(
            fresh.facts[0].judgement,
            judgement(Partial, &[EvidenceNotPassed])
        );
        assert_eq!(fresh.next_checks[0].evidence_id, fixture.test);
        assert_eq!(
            assess(anchor, true, &observed(head_moved)).judgement,
            judgement(
                Partial,
                &[
                    HeadChanged,
                    IndexChanged,
                    EvidenceStale,
                    EvidenceUnavailable
                ]
            ),
            "{name}: legacy, stale"
        );

        // A statement that does not propagate carries the reason; the
        // Checkpoint has no verified fact, which is evidence_unavailable.
        let mut input = open_item_claim(&fixture, vec![fixture.test.clone()]);
        input.open_items = vec![
            CheckpointStatement::new(
                CheckpointClassification::Unknown,
                "tests fail at X".to_owned(),
                vec![fixture.test.clone()],
            )
            .unwrap(),
        ];
        let honest = Checkpoint::create(input, &fixture.snapshot, &fixture.evidence).unwrap();
        let assessment = assess(
            Anchor::Intact {
                checkpoint: &honest,
                snapshot: &fixture.snapshot,
                evidence: &fixture.evidence,
            },
            true,
            &observed(|_| {}),
        );
        assert_eq!(
            assessment.judgement,
            judgement(Partial, &[EvidenceUnavailable])
        );
        assert!(!assessment.facts[0].propagates);
        assert_eq!(
            assessment.facts[0].judgement,
            judgement(Partial, &[EvidenceNotPassed])
        );
        assert_eq!(
            assessment.facts[0].evidence[0].judgement,
            judgement(Partial, &[EvidenceNotPassed])
        );
    }
}

/// T9 S7: an empty commit moves HEAD and nothing else. The capture manifest
/// is compared at the probe's HEAD, so the Evidence is stale and cannot
/// complete the Task; ignoring HEAD there would make it `verified`.
#[test]
fn p2e_only_head_changed_is_stale_for_the_preview_and_completion() {
    let fixture = Fixture::clean();
    let mut observation = fixture.observation();
    observation.head_oid = Y.to_owned();
    let probe = fixture.probe(observation);
    assert_eq!(
        fixture.judge(&fixture.test, &probe),
        judgement(Stale, &[HeadChanged])
    );
    assert_eq!(
        fixture.judge(&fixture.head, &probe),
        judgement(Stale, &[HeadChanged])
    );
    let assessment = assess(fixture.anchor(), true, &probe);
    assert_eq!(
        assessment.judgement,
        judgement(Stale, &[HeadChanged, EvidenceStale])
    );
    assert_eq!(assessment.next_checks[0].at_head.as_deref(), Some(Y));
    match judge_completion(
        fixture.checkpoint.task_id(),
        &[completion_of(&fixture, &fixture.test)],
        &probe,
    ) {
        Err(CompletionRefusal::NotVerified(id, refused)) => {
            assert_eq!(id, fixture.test);
            assert_eq!(refused, judgement(Stale, &[HeadChanged]));
        }
        other => panic!("{other:?}"),
    }
}

fn view(fixture: &Fixture) -> TaskLedgerView {
    TaskLedgerView {
        project_sequence: 5,
        ledger_chain_valid: true,
        task: Task {
            task_id: fixture.snapshot.task_id().clone(),
            project_id: fixture.snapshot.project_id().clone(),
            workspace_id: None,
            title: "t".to_owned(),
            description: None,
            status: TaskStatus::InProgress,
            created_at: at("2026-10-02T09:00:00Z"),
            updated_at: at("2026-10-02T09:00:00Z"),
            revision: 2,
            completion_evidence_ids: Vec::new(),
        },
        sessions: vec![],
        sessions_truncated: false,
        checkpoint: CheckpointView::Intact {
            checkpoint: fixture.checkpoint.clone(),
            snapshot: fixture.snapshot.clone(),
            evidence: fixture.evidence.clone(),
        },
        events: vec![EventSummary {
            project_sequence: 5,
            event_type: "session.ended".to_owned(),
            occurred_at: Some(at("2026-10-02T10:00:03Z")),
            recorded_at: at("2026-10-02T10:00:03Z"),
            producer_name: "continuityd".to_owned(),
            producer_version: "0.1.0".to_owned(),
        }],
        events_truncated: false,
        damaged_records: std::collections::BTreeSet::new(),
        runs: Vec::new(),
    }
}

/// P3-C2 (Owner, T12 S-4/S-6): a damaged record anywhere in the Task makes
/// the Preview `tampered` although its latest Checkpoint reads intact, and a
/// `tampered` Preview holds no `verified` at any level: every statement and
/// Evidence verdict is `tampered`, keeping its own reasons. A broken chain
/// does the same.
#[test]
fn p3c2_a_tampered_preview_holds_no_verified() {
    let fixture = Fixture::clean();
    let probe = fixture.probe(fixture.observation());
    let intact = recovery_preview(&view(&fixture), &probe).unwrap();
    assert!(intact.as_str().contains("\"verified\""));

    let mut damaged = view(&fixture);
    damaged.damaged_records.insert(crate::DamagedRecord {
        record_type: crate::LedgerRecordType::Checkpoint,
        record_id: crate::CheckpointId::new().to_string(),
        reason: ReasonCode::ObjectDigestMismatch,
    });
    let mut broken = view(&fixture);
    broken.ledger_chain_valid = false;
    for (view, reason) in [
        (damaged, ReasonCode::ObjectDigestMismatch),
        (broken, ReasonCode::LedgerChainInvalid),
    ] {
        let preview = recovery_preview(&view, &probe).unwrap();
        assert!(!preview.as_str().contains("\"verified\""), "{reason:?}");
        let document: serde_json::Value = serde_json::from_str(preview.as_str()).unwrap();
        assert_eq!(document["status"], "tampered");
        assert_eq!(document["reasons"], serde_json::json!([reason.as_str()]));
        // The latest Checkpoint is intact and still listed.
        assert_eq!(document["bindings"]["checkpoint"]["intact"], true);
        let facts = document["facts"].as_array().unwrap();
        assert!(!facts.is_empty());
        for fact in facts {
            assert_eq!(fact["verdict"], "tampered");
            assert!(
                fact["reasons"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(reason.as_str()))
            );
            for item in fact["evidence"].as_array().unwrap() {
                assert_eq!(item["verdict"], "tampered");
                assert_eq!(item["reasons"], serde_json::json!([reason.as_str()]));
            }
        }
        // Every Evidence object of a propagating statement is listed.
        assert_eq!(
            document["invalid_evidence_ids"],
            serde_json::json!([fixture.test.to_string()])
        );
    }
}

#[test]
fn one_ledger_view_and_one_probe_render_identical_bytes_and_digest() {
    let fixture = Fixture::clean();
    let view = view(&fixture);
    let probe = fixture.probe(fixture.observation());
    let first = recovery_preview(&view, &probe).unwrap();
    let second = recovery_preview(&view, &probe).unwrap();
    assert_eq!(first.as_str(), second.as_str());
    assert_eq!(first.digest(), second.digest());

    let mut document: serde_json::Value = serde_json::from_str(first.as_str()).unwrap();
    assert_eq!(
        document["preview_digest"],
        format!("sha256:{}", first.digest())
    );
    assert_eq!(document["status"], "verified");
    assert_eq!(document["ontology_version"], ONTOLOGY_VERSION);
    assert_eq!(document["bindings"]["ledger"]["project_sequence"], 5);
    assert_eq!(document["bindings"]["snapshot"]["head_oid"], X);
    assert_eq!(document["bindings"]["probe"]["head_oid"], X);
    assert!(document.get("generated_at").is_none());
    document.as_object_mut().unwrap().remove("preview_digest");
    let without = CanonicalJsonProbe::canonical(&document);
    assert_eq!(Sha256Digest::of_bytes(without.as_bytes()), *first.digest());

    let mut moved = fixture.observation();
    head_moved(&mut moved);
    let stale = recovery_preview(&view, &fixture.probe(moved)).unwrap();
    assert_ne!(stale.digest(), first.digest());
    let stale_document: serde_json::Value = serde_json::from_str(stale.as_str()).unwrap();
    assert_eq!(stale_document["status"], "stale");
    assert_eq!(stale_document["bindings"]["probe"]["head_oid"], Y);
    assert_eq!(stale_document["next_checks"][0]["at_head"], Y);
    // The Task's status is reported, never changed.
    assert_eq!(stale_document["bindings"]["task"]["status"], "in_progress");

    let mut later = view.clone();
    later.project_sequence = 6;
    assert_ne!(
        recovery_preview(&later, &probe).unwrap().digest(),
        first.digest()
    );
}

/// Canonicalizes a test document the way the Preview does.
struct CanonicalJsonProbe;

impl CanonicalJsonProbe {
    fn canonical(value: &serde_json::Value) -> String {
        crate::CanonicalJson::from_trusted_value(value)
            .unwrap()
            .as_str()
            .to_owned()
    }
}

/// One repository state of the carry-over grid (ADR 0013 D3, P3-D).
#[derive(Debug, Clone, PartialEq, Eq)]
struct GridState {
    head: &'static str,
    branch: Option<&'static str>,
    status: &'static str,
    index: &'static str,
    tracked: &'static str,
    readme: &'static str,
}

/// Every combination of HEAD, branch, porcelain status (clean, a modified
/// tracked file, an untracked file), index, tracked content, and the
/// selected file's content.
fn grid() -> Vec<GridState> {
    let mut states = Vec::new();
    for head in [X, Y] {
        for branch in [Some("main"), Some("feature"), None] {
            for status in [
                "",
                "1 .M N... 100644 100644 100644 0000 0000 README\n",
                "? scratch.txt\n",
            ] {
                for index in ["index 1", "index 2"] {
                    for tracked in ["tracked 1", "tracked 2"] {
                        for readme in ["readme 1", "readme 2"] {
                            states.push(GridState {
                                head,
                                branch,
                                status,
                                index,
                                tracked,
                                readme,
                            });
                        }
                    }
                }
            }
        }
    }
    states
}

/// The Evidence of a Snapshot in `state` (its `git_head`, `git_worktree`,
/// and selected file) for `task_id` in `workspace_id`.
fn state_evidence(
    state: &GridState,
    task_id: &TaskId,
    workspace_id: &WorkspaceId,
) -> Vec<Evidence> {
    let identity = digest("identity W1");
    let make = |kind, subject, payload| {
        Evidence::new(EvidenceInput {
            evidence_id: EvidenceId::new(),
            task_id: task_id.clone(),
            workspace_id: workspace_id.clone(),
            kind,
            subject,
            observed_at: at("2026-10-02T10:00:00Z"),
            collector: EvidenceCollector {
                name: "fixture".to_owned(),
                version: "1".to_owned(),
            },
            payload,
        })
        .unwrap()
    };
    vec![
        make(
            EvidenceKind::GitHead,
            EvidenceSubject::GitHead {
                repository_identity: identity.clone(),
            },
            EvidencePayload::GitHead {
                head_oid: state.head.to_owned(),
                branch: state.branch.map(str::to_owned),
            },
        ),
        make(
            EvidenceKind::GitWorktree,
            EvidenceSubject::GitWorktree {
                repository_identity: identity,
            },
            EvidencePayload::GitWorktree {
                status_porcelain_v2: state.status.to_owned(),
            },
        ),
        make(
            EvidenceKind::FileDigest,
            EvidenceSubject::FileDigest {
                path: "repo://README".to_owned(),
            },
            EvidencePayload::FileDigest {
                sha256: digest(state.readme),
            },
        ),
    ]
}

/// A Snapshot in `state` sealing `own` (its repository Evidence) and the
/// test runs `runs`.
fn state_snapshot(
    state: &GridState,
    project_id: &ProjectId,
    own: &[Evidence],
    runs: &[Evidence],
) -> (RepositorySnapshot, Vec<Evidence>) {
    let manifest = repository_capture_manifest_digest(
        &digest("identity W1"),
        state.head,
        state.branch,
        state.status,
        &digest(state.index),
        &digest(state.tracked),
        &[("repo://README", &digest(state.readme))],
    );
    let evidence = own.iter().chain(runs).cloned().collect::<Vec<_>>();
    let snapshot = RepositorySnapshot::new(
        RepositorySnapshotInput {
            snapshot_id: RepositorySnapshotId::new(),
            project_id: project_id.clone(),
            task_id: own[0].task_id().clone(),
            workspace_id: own[0].workspace_id().clone(),
            head_oid: state.head.to_owned(),
            branch: state.branch.map(str::to_owned),
            worktree_status_porcelain_v2: state.status.to_owned(),
            index_digest: digest(state.index),
            tracked_worktree_digest: digest(state.tracked),
            capture_manifest_digest: manifest,
            git_head_evidence_id: own[0].evidence_id().clone(),
            git_worktree_evidence_id: own[1].evidence_id().clone(),
            file_evidence_ids: vec![own[2].evidence_id().clone()],
            command_evidence_ids: vec![],
            test_evidence_ids: runs.iter().map(|run| run.evidence_id().clone()).collect(),
            captured_at: at("2026-10-02T10:00:01Z"),
        },
        &evidence,
    )
    .unwrap();
    (snapshot, evidence)
}

/// What a probe of W1 sees in `state`.
fn state_probe(state: &GridState, workspace_id: &WorkspaceId) -> Probe {
    let manifest = repository_capture_manifest_digest(
        &digest("identity W1"),
        state.head,
        state.branch,
        state.status,
        &digest(state.index),
        &digest(state.tracked),
        &[("repo://README", &digest(state.readme))],
    );
    Probe {
        workspace_id: Some(workspace_id.clone()),
        observation: ProbeObservation::Observed(RepositoryObservation {
            same_repository: true,
            same_workspace: true,
            repository_identity: digest("identity W1"),
            head_oid: state.head.to_owned(),
            branch: state.branch.map(str::to_owned),
            status_porcelain_v2: state.status.to_owned(),
            index_digest: digest(state.index),
            tracked_worktree_digest: digest(state.tracked),
            capture_manifest_digest: manifest,
            selected_files: BTreeMap::from([(
                "repo://README".to_owned(),
                Some(digest(state.readme)),
            )]),
            untracked_paths: porcelain_v2_paths(state.status).untracked,
            changed_since_base: Some(BTreeSet::new()),
        }),
    }
}

/// ADR 0013 D3 (P3-D), as a property over every pair of grid states: a
/// later Snapshot carries an earlier one's runs over exactly when both are
/// of one Task and Workspace, in the same state, and neither lists an
/// untracked file; and then every run, passed or not, is judged through
/// the later Snapshot exactly as through its own, under every probe of the
/// grid and an unavailable one, so never better.
#[test]
fn carry_over_never_judges_a_run_better_than_its_own_snapshot() {
    let states = grid();
    let project_id = ProjectId::new();
    let task_id = TaskId::new();
    let workspace_id = WorkspaceId::new();
    let mut probes = states
        .iter()
        .map(|state| state_probe(state, &workspace_id))
        .collect::<Vec<_>>();
    probes.push(Probe {
        workspace_id: Some(workspace_id.clone()),
        observation: ProbeObservation::Unavailable,
    });
    let runs = |task_id: &TaskId, workspace_id: &WorkspaceId| {
        [passed_test(), test_payload(1, Some(3), None)].map(|payload| {
            Evidence::new(EvidenceInput {
                evidence_id: EvidenceId::new(),
                task_id: task_id.clone(),
                workspace_id: workspace_id.clone(),
                kind: EvidenceKind::TestResult,
                subject: EvidenceSubject::TestResult {
                    test_name: "suite".to_owned(),
                },
                observed_at: at("2026-10-02T10:00:00Z"),
                collector: EvidenceCollector {
                    name: "fixture".to_owned(),
                    version: "1".to_owned(),
                },
                payload,
            })
            .unwrap()
        })
    };
    let earlier_runs = runs(&task_id, &workspace_id);
    // Each state's earlier Snapshot (sealing the runs) and later Snapshot
    // (its own run-less capture), built once.
    let snapshots = states
        .iter()
        .map(|state| {
            let earlier_own = state_evidence(state, &task_id, &workspace_id);
            let later_own = state_evidence(state, &task_id, &workspace_id);
            let earlier = state_snapshot(state, &project_id, &earlier_own, &earlier_runs);
            let later = state_snapshot(state, &project_id, &later_own, &[]).0;
            (state, earlier, later, later_own)
        })
        .collect::<Vec<_>>();
    let mut carried_pairs = 0;
    let mut judged = 0;
    for (earlier_state, (earlier, earlier_evidence), _, _) in &snapshots {
        for (later_state, _, later, later_own) in &snapshots {
            let expected = earlier_state == later_state && !earlier_state.status.starts_with('?');
            assert_eq!(
                carries_over(earlier, later),
                expected,
                "{earlier_state:?} -> {later_state:?}"
            );
            if !expected {
                continue;
            }
            carried_pairs += 1;
            let (carrier, carrier_evidence) =
                state_snapshot(later_state, &project_id, later_own, &earlier_runs);
            for run in &earlier_runs {
                for probe in &probes {
                    let own = judge_evidence(earlier, earlier_evidence, run, probe);
                    let through = judge_evidence(&carrier, &carrier_evidence, run, probe);
                    assert!(
                        through.verdict() >= own.verdict(),
                        "better: {later_state:?}"
                    );
                    assert_eq!(through, own, "{later_state:?}");
                    judged += 1;
                }
            }
        }
    }
    assert_eq!(carried_pairs, 96);
    assert_eq!(judged, 96 * 2 * 145);

    // Another Task or another Workspace never carries over, in any state.
    let other_task = TaskId::new();
    let other_workspace = WorkspaceId::new();
    for state in states.iter().filter(|state| !state.status.starts_with('?')) {
        let own = state_evidence(state, &task_id, &workspace_id);
        let (earlier, _) = state_snapshot(state, &project_id, &own, &[]);
        for (task, workspace) in [(&other_task, &workspace_id), (&task_id, &other_workspace)] {
            let other = state_evidence(state, task, workspace);
            let (later, _) = state_snapshot(state, &project_id, &other, &[]);
            assert!(!carries_over(&earlier, &later));
            assert!(!carries_over(&later, &earlier));
        }
    }
}

/// A three-case test run of `argv` that exited `exit_code` (P3-D3);
/// `cases` 0 is a run that did not run any test (`NOT_RUN`).
fn run_of(
    task_id: &TaskId,
    workspace_id: &WorkspaceId,
    argv: &[&str],
    exit_code: i32,
    cases: u64,
) -> Evidence {
    Evidence::new(EvidenceInput {
        evidence_id: EvidenceId::new(),
        task_id: task_id.clone(),
        workspace_id: workspace_id.clone(),
        kind: EvidenceKind::TestResult,
        subject: EvidenceSubject::TestResult {
            test_name: "suite".to_owned(),
        },
        observed_at: at("2026-10-02T10:00:00Z"),
        collector: EvidenceCollector {
            name: "fixture".to_owned(),
            version: "1".to_owned(),
        },
        payload: EvidencePayload::TestResult {
            argv: argv.iter().map(|word| (*word).to_owned()).collect(),
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
            passed: EvidencePayload::test_passed(exit_code, Some(cases)),
            test_case_count: Some(cases),
            test_format: None,
            started_at: None,
            finished_at: None,
            stdout_sha256: None,
            stderr_sha256: None,
            launch_error: None,
        },
    })
    .unwrap()
}

/// The Snapshots of one Project in ledger order, each recording one run and
/// carrying over, as `captureEvidence` does (ADR 0013 D3), the runs of
/// every earlier Snapshot [`carries_over`] allows, from their first sealer.
#[derive(Default)]
struct RunLedger {
    project_id: Option<ProjectId>,
    snapshots: Vec<(u64, RepositorySnapshot, Vec<Evidence>)>,
}

impl RunLedger {
    /// Records `run` in `state`; returns whether anything was carried.
    fn record(&mut self, state: &GridState, run: &Evidence) -> bool {
        let project_id = self.project_id.get_or_insert_with(ProjectId::new).clone();
        let own = state_evidence(state, run.task_id(), run.workspace_id());
        let (bare, _) = state_snapshot(state, &project_id, &own, &[]);
        let mut runs = vec![run.clone()];
        for (_, earlier, evidence) in &self.snapshots {
            if !carries_over(earlier, &bare) {
                continue;
            }
            for id in earlier.test_evidence_ids() {
                let first = self
                    .snapshots
                    .iter()
                    .find(|(_, snapshot, _)| snapshot.contains_evidence(id))
                    .map(|(_, snapshot, _)| snapshot.snapshot_id());
                if first == Some(earlier.snapshot_id())
                    && !runs.iter().any(|item| item.evidence_id() == id)
                {
                    runs.push(
                        evidence
                            .iter()
                            .find(|item| item.evidence_id() == id)
                            .unwrap()
                            .clone(),
                    );
                }
            }
        }
        let carried = runs.len() > 1;
        let (snapshot, evidence) = state_snapshot(state, &project_id, &own, &runs);
        let position = 10 + u64::try_from(self.snapshots.len()).unwrap();
        self.snapshots.push((position, snapshot, evidence));
        carried
    }

    fn history(&self) -> RunHistory {
        RunHistory::of(
            self.snapshots
                .iter()
                .map(|(position, snapshot, evidence)| (*position, snapshot, evidence.as_slice())),
        )
    }

    /// `run` judged through every Snapshot that seals it, with `history`;
    /// every one of them must agree (D3), and the first is returned.
    fn judge(&self, run: &EvidenceId, history: &RunHistory, probe: &Probe) -> Judgement {
        let mut judged = self
            .snapshots
            .iter()
            .filter(|(_, snapshot, _)| snapshot.contains_evidence(run))
            .map(|(_, snapshot, evidence)| {
                let item = evidence
                    .iter()
                    .find(|item| item.evidence_id() == run)
                    .unwrap();
                super::judge_evidence(snapshot, evidence, item, history, probe)
            });
        let first = judged.next().unwrap();
        for other in judged {
            assert_eq!(other, first);
        }
        first
    }

    /// `run` as Task completion reads it: through its first Snapshot.
    fn completion(&self, run: &EvidenceId) -> CompletionEvidence {
        let (_, snapshot, evidence) = self
            .snapshots
            .iter()
            .find(|(_, snapshot, _)| snapshot.contains_evidence(run))
            .unwrap();
        CompletionEvidence {
            evidence: evidence
                .iter()
                .find(|item| item.evidence_id() == run)
                .unwrap()
                .clone(),
            snapshot: Some(snapshot.clone()),
            snapshot_evidence: evidence.clone(),
        }
    }
}

const CHECK: &[&str] = &["sh", "-c", "grep -q A ignored.cfg"];
const OTHER: &[&str] = &["cargo", "test"];

fn clean_state() -> GridState {
    GridState {
        head: X,
        branch: Some("main"),
        status: "",
        index: "index 1",
        tracked: "tracked 1",
        readme: "readme 1",
    }
}

/// P3-D3 (Owner, T14 S-1): a passed run, then a run of the same argv in the
/// same state that failed (carried over into one Snapshot, as T14's
/// ignored-file and write-bit cases): the passed run is `partial` with
/// `evidence_not_passed` through either Snapshot, completes nothing, and
/// its fact is not `verified`; without the rule it would be `verified`.
#[test]
#[allow(clippy::too_many_lines)] // The run, completion, and the fact, in order.
fn a_later_run_of_the_same_argv_that_did_not_pass_overrules_a_pass() {
    let task_id = TaskId::new();
    let workspace_id = WorkspaceId::new();
    let state = clean_state();
    let probe = state_probe(&state, &workspace_id);
    let passed = run_of(&task_id, &workspace_id, CHECK, 0, 1);
    let failed = run_of(&task_id, &workspace_id, CHECK, 1, 1);
    let mut ledger = RunLedger::default();
    assert!(!ledger.record(&state, &passed));
    assert!(ledger.record(&state, &failed));
    let history = ledger.history();
    assert_eq!(
        history.overruled_by(passed.evidence_id()),
        Some(failed.evidence_id())
    );
    assert_eq!(history.overruled_by(failed.evidence_id()), None);
    let not_passed = judgement(Partial, &[EvidenceNotPassed]);
    assert_eq!(
        ledger.judge(passed.evidence_id(), &RunHistory::default(), &probe),
        Judgement::verified()
    );
    assert_eq!(
        ledger.judge(passed.evidence_id(), &history, &probe),
        not_passed
    );
    assert_eq!(
        ledger.judge(failed.evidence_id(), &history, &probe),
        not_passed
    );
    // Stale stays stale: the rule only takes `verified` away.
    let mut moved = state.clone();
    moved.head = Y;
    assert_eq!(
        ledger
            .judge(
                passed.evidence_id(),
                &history,
                &state_probe(&moved, &workspace_id)
            )
            .verdict(),
        Stale
    );
    // Task completion refuses it, by the same judgement.
    assert_eq!(
        super::judge_completion(
            &task_id,
            &[ledger.completion(passed.evidence_id())],
            &history,
            &probe
        ),
        Err(CompletionRefusal::NotVerified(
            passed.evidence_id().clone(),
            not_passed.clone()
        ))
    );
    assert!(
        super::judge_completion(
            &task_id,
            &[ledger.completion(passed.evidence_id())],
            &RunHistory::default(),
            &probe
        )
        .is_ok()
    );
    // A fact over the carrier citing the passed run is `partial`.
    let (_, carrier, evidence) = &ledger.snapshots[1];
    let checkpoint = Checkpoint::new(
        CheckpointInput {
            checkpoint_id: CheckpointId::new(),
            task_id: task_id.clone(),
            session_id: SessionId::new(),
            snapshot_id: carrier.snapshot_id().clone(),
            project_event_sequence: 12,
            objective: "the check passes".to_owned(),
            verified_completed_work: vec![
                CheckpointStatement::new(
                    CheckpointClassification::VerifiedFact,
                    "the check passes".to_owned(),
                    vec![passed.evidence_id().clone()],
                )
                .unwrap(),
            ],
            open_items: vec![],
            next_action: CheckpointStatement::new(
                CheckpointClassification::Recommendation,
                "go on".to_owned(),
                vec![],
            )
            .unwrap(),
            created_at: at("2026-10-02T10:00:02Z"),
        },
        carrier,
        evidence,
    )
    .unwrap();
    let anchor = Anchor::Intact {
        checkpoint: &checkpoint,
        snapshot: carrier,
        evidence,
    };
    let judged = super::assess(anchor, true, &history, &probe);
    assert_eq!(judged.judgement, not_passed);
    assert_eq!(judged.facts[0].judgement, not_passed);
    assert!(judged.invalid_evidence_ids.contains(passed.evidence_id()));
    assert_eq!(judged.next_checks.len(), 1);
    assert_eq!(
        super::assess(anchor, true, &RunHistory::default(), &probe).judgement,
        Judgement::verified()
    );
}

/// P3-D3: pass, fail, pass in one state: each run is judged by its own
/// outcome, since the latest run passed; a fact on the last one is
/// `verified`.
#[test]
fn pass_fail_pass_judges_each_run_by_its_own_outcome() {
    let task_id = TaskId::new();
    let workspace_id = WorkspaceId::new();
    let state = clean_state();
    let probe = state_probe(&state, &workspace_id);
    let first = run_of(&task_id, &workspace_id, CHECK, 0, 1);
    let failed = run_of(&task_id, &workspace_id, CHECK, 1, 1);
    let last = run_of(&task_id, &workspace_id, CHECK, 0, 1);
    let mut ledger = RunLedger::default();
    for run in [&first, &failed, &last] {
        ledger.record(&state, run);
    }
    let history = ledger.history();
    assert_eq!(
        ledger.judge(first.evidence_id(), &history, &probe),
        Judgement::verified()
    );
    assert_eq!(
        ledger.judge(failed.evidence_id(), &history, &probe),
        judgement(Partial, &[EvidenceNotPassed])
    );
    assert_eq!(
        ledger.judge(last.evidence_id(), &history, &probe),
        Judgement::verified()
    );
    assert!(
        super::judge_completion(
            &task_id,
            &[ledger.completion(last.evidence_id())],
            &history,
            &probe
        )
        .is_ok()
    );
    // A run that did not run any test is not a pass either: it overrules
    // both passed runs again.
    let empty = run_of(&task_id, &workspace_id, CHECK, 0, 0);
    ledger.record(&state, &empty);
    let history = ledger.history();
    for run in [&first, &last] {
        assert_eq!(
            history.overruled_by(run.evidence_id()),
            Some(empty.evidence_id())
        );
    }
}

/// P3-D3: the rule reads the ledger, not the carry-over. Both states list
/// the same untracked file (selected, so nothing is left uncaptured) with
/// equal manifests: nothing is carried, and the later failure still
/// overrules the earlier pass.
#[test]
fn the_later_run_rule_holds_without_carry_over() {
    let task_id = TaskId::new();
    let workspace_id = WorkspaceId::new();
    let state = GridState {
        status: "? README\n",
        ..clean_state()
    };
    let probe = state_probe(&state, &workspace_id);
    let passed = run_of(&task_id, &workspace_id, CHECK, 0, 1);
    let failed = run_of(&task_id, &workspace_id, CHECK, 1, 1);
    let mut ledger = RunLedger::default();
    assert!(!ledger.record(&state, &passed));
    assert!(!ledger.record(&state, &failed));
    let (_, earlier, _) = &ledger.snapshots[0];
    let (_, later, _) = &ledger.snapshots[1];
    assert_eq!(
        earlier.capture_manifest_digest(),
        later.capture_manifest_digest()
    );
    assert!(!carries_over(earlier, later));
    assert!(!later.contains_evidence(passed.evidence_id()));
    assert_eq!(
        ledger.judge(passed.evidence_id(), &RunHistory::default(), &probe),
        Judgement::verified()
    );
    assert_eq!(
        ledger.judge(passed.evidence_id(), &ledger.history(), &probe),
        judgement(Partial, &[EvidenceNotPassed])
    );
}

/// P3-D3: runs of another argv, another Task, another Workspace, or
/// another state overrule nothing.
#[test]
fn runs_of_another_argv_task_workspace_or_state_overrule_nothing() {
    let task_id = TaskId::new();
    let workspace_id = WorkspaceId::new();
    let state = clean_state();
    let probe = state_probe(&state, &workspace_id);
    let passed = run_of(&task_id, &workspace_id, CHECK, 0, 1);
    let mut other_state = state.clone();
    other_state.index = "index 2";
    let failures = [
        (run_of(&task_id, &workspace_id, OTHER, 1, 1), state.clone()),
        (
            run_of(&TaskId::new(), &workspace_id, CHECK, 1, 1),
            state.clone(),
        ),
        (
            run_of(&task_id, &WorkspaceId::new(), CHECK, 1, 1),
            state.clone(),
        ),
        (run_of(&task_id, &workspace_id, CHECK, 1, 1), other_state),
    ];
    let mut ledger = RunLedger::default();
    ledger.record(&state, &passed);
    for (failed, state) in &failures {
        ledger.record(state, failed);
        let history = ledger.history();
        assert_eq!(history.overruled_by(passed.evidence_id()), None);
        assert_eq!(
            ledger.judge(passed.evidence_id(), &history, &probe),
            Judgement::verified()
        );
    }
}

/// A small deterministic generator (no crate).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// Property (P3-D3): with the later-run rule, no Evidence object is judged
/// better than without it, under any probe; where they differ, the rule
/// turned `verified` into `partial` with `evidence_not_passed`, exactly for
/// an overruled passed run; and completion it allows, it allowed before.
#[test]
fn property_the_later_run_rule_never_judges_a_run_better() {
    let tasks = [TaskId::new(), TaskId::new()];
    let workspaces = [WorkspaceId::new(), WorkspaceId::new()];
    let mut states = vec![clean_state()];
    states.push(GridState {
        index: "index 2",
        ..clean_state()
    });
    states.push(GridState {
        head: Y,
        ..clean_state()
    });
    states.push(GridState {
        status: "? README\n",
        ..clean_state()
    });
    let outcomes: [(i32, u64); 4] = [(0, 3), (0, 3), (1, 3), (0, 0)];
    let mut compared = 0;
    let mut differed = 0;
    for seed in 0..1000 {
        let mut random = Lcg(seed);
        let mut ledger = RunLedger::default();
        let mut runs = Vec::new();
        for _ in 0..(2 + random.below(7)) {
            let task = &tasks[usize::try_from(random.below(2)).unwrap()];
            let workspace = &workspaces[usize::try_from(random.below(2)).unwrap()];
            let state = &states[usize::try_from(random.below(4)).unwrap()];
            let argv = if random.below(3) == 0 { OTHER } else { CHECK };
            let (exit_code, cases) = outcomes[usize::try_from(random.below(4)).unwrap()];
            let run = run_of(task, workspace, argv, exit_code, cases);
            ledger.record(state, &run);
            runs.push(run);
        }
        let history = ledger.history();
        for run in &runs {
            let mut probes = states
                .iter()
                .map(|state| state_probe(state, run.workspace_id()))
                .collect::<Vec<_>>();
            probes.push(Probe {
                workspace_id: Some(run.workspace_id().clone()),
                observation: ProbeObservation::Unavailable,
            });
            for probe in &probes {
                let with = ledger.judge(run.evidence_id(), &history, probe);
                let without = ledger.judge(run.evidence_id(), &RunHistory::default(), probe);
                assert!(with.verdict() >= without.verdict(), "seed {seed}");
                if with != without {
                    differed += 1;
                    assert_eq!(without, Judgement::verified(), "seed {seed}");
                    assert_eq!(
                        with,
                        judgement(Partial, &[EvidenceNotPassed]),
                        "seed {seed}"
                    );
                    assert!(history.overruled_by(run.evidence_id()).is_some());
                }
                let completion = [ledger.completion(run.evidence_id())];
                if super::judge_completion(run.task_id(), &completion, &history, probe).is_ok() {
                    assert!(
                        super::judge_completion(
                            run.task_id(),
                            &completion,
                            &RunHistory::default(),
                            probe
                        )
                        .is_ok()
                    );
                }
                compared += 1;
            }
        }
    }
    assert!(compared > 25_000, "{compared}");
    assert!(differed > 80, "{differed}");
}
