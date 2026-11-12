//! Table-driven tests of the Task Capsule rules (ADR 0013 D1, D2, D4, D5,
//! D7), and three property tests over generated ledgers.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::{
    CheckpointInput, CheckpointStatement, EvidenceCollector, EvidenceInput, EvidencePayload,
    EvidenceSubject, ProjectId, RepositoryObservation, RepositorySnapshotId,
    RepositorySnapshotInput, TaskStatus, UtcTimestamp, repository_capture_manifest_digest,
};

const X: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const Y: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const Z: &str = "cccccccccccccccccccccccccccccccccccccccc";

use ReasonCode::{
    EvidenceNotPassed, EvidenceStale, EvidenceUnavailable, HeadChanged, IndexChanged,
    LedgerChainInvalid, ObjectDigestMismatch, ObjectMissing,
};
use Verdict::{Partial, Stale, Tampered, Verified};

fn at(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).unwrap()
}

fn digest(label: &str) -> Sha256Digest {
    Sha256Digest::of_bytes(label.as_bytes())
}

/// `judge_evidence` with no later runs, as the tests written before P3-D3
/// compare with it.
fn judge_evidence(
    snapshot: &RepositorySnapshot,
    snapshot_evidence: &[Evidence],
    evidence: &Evidence,
    probe: &Probe,
) -> Judgement {
    crate::judge_evidence(
        snapshot,
        snapshot_evidence,
        evidence,
        &RunHistory::default(),
        probe,
    )
}

fn judgement(verdict: Verdict, reasons: &[ReasonCode]) -> Judgement {
    Judgement::new(verdict, reasons.iter().copied())
}

fn argv(command: &str) -> Vec<String> {
    command.split(' ').map(str::to_owned).collect()
}

/// One Task in one Workspace of one repository, and the records built for
/// it.
struct World {
    project_id: ProjectId,
    task_id: TaskId,
    workspace_id: WorkspaceId,
    session_id: SessionId,
    identity: Sha256Digest,
}

/// A run: a Snapshot at `head` with its `git_head`, `git_worktree`, and one
/// test or command per entry.
struct Run {
    snapshot: RepositorySnapshot,
    evidence: Vec<Evidence>,
    /// The command or test Evidence, in the order given.
    commands: Vec<EvidenceId>,
}

/// One command of a run: its argv, whether it is a test, and its exit code.
struct Command<'a> {
    argv: &'a str,
    test: bool,
    exit_code: i32,
}

const fn test(argv: &str) -> Command<'_> {
    Command {
        argv,
        test: true,
        exit_code: 0,
    }
}

const fn failing(argv: &str) -> Command<'_> {
    Command {
        argv,
        test: true,
        exit_code: 1,
    }
}

impl World {
    fn new() -> Self {
        Self {
            project_id: ProjectId::new(),
            task_id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            session_id: SessionId::new(),
            identity: digest("identity W"),
        }
    }

    fn evidence(
        &self,
        kind: EvidenceKind,
        subject: EvidenceSubject,
        payload: EvidencePayload,
    ) -> Evidence {
        Evidence::new(EvidenceInput {
            evidence_id: EvidenceId::new(),
            task_id: self.task_id.clone(),
            workspace_id: self.workspace_id.clone(),
            kind,
            subject,
            observed_at: at("2026-10-03T10:00:00Z"),
            collector: EvidenceCollector {
                name: "fixture".to_owned(),
                version: "1".to_owned(),
            },
            payload,
        })
        .unwrap()
    }

    #[allow(clippy::too_many_lines)] // One complete Snapshot and its Evidence.
    fn run(&self, head: &str, status: &str, commands: &[Command<'_>]) -> Run {
        let git_head = self.evidence(
            EvidenceKind::GitHead,
            EvidenceSubject::GitHead {
                repository_identity: self.identity.clone(),
            },
            EvidencePayload::GitHead {
                head_oid: head.to_owned(),
                branch: Some("main".to_owned()),
            },
        );
        let git_worktree = self.evidence(
            EvidenceKind::GitWorktree,
            EvidenceSubject::GitWorktree {
                repository_identity: self.identity.clone(),
            },
            EvidencePayload::GitWorktree {
                status_porcelain_v2: status.to_owned(),
            },
        );
        let mut tests = Vec::new();
        let mut plain = Vec::new();
        let mut order = Vec::new();
        for command in commands {
            let evidence = if command.test {
                let cases = Some(3);
                self.evidence(
                    EvidenceKind::TestResult,
                    EvidenceSubject::TestResult {
                        test_name: command.argv.to_owned(),
                    },
                    EvidencePayload::TestResult {
                        argv: argv(command.argv),
                        exit_code: command.exit_code,
                        stdout: String::new(),
                        stderr: String::new(),
                        passed: EvidencePayload::test_passed(command.exit_code, cases),
                        test_case_count: cases,
                        test_format: None,
                        started_at: None,
                        finished_at: None,
                        stdout_sha256: None,
                        stderr_sha256: None,
                        launch_error: None,
                    },
                )
            } else {
                self.evidence(
                    EvidenceKind::CommandResult,
                    EvidenceSubject::CommandResult {
                        command: command.argv.to_owned(),
                    },
                    EvidencePayload::CommandResult {
                        argv: argv(command.argv),
                        exit_code: command.exit_code,
                        stdout: String::new(),
                        stderr: String::new(),
                        started_at: None,
                        finished_at: None,
                        stdout_sha256: None,
                        stderr_sha256: None,
                        launch_error: None,
                    },
                )
            };
            order.push(evidence.evidence_id().clone());
            if command.test {
                tests.push(evidence);
            } else {
                plain.push(evidence);
            }
        }
        let manifest = repository_capture_manifest_digest(
            &self.identity,
            head,
            Some("main"),
            status,
            &digest(&format!("index {head}")),
            &digest(&format!("tracked {head} {status}")),
            &[],
        );
        let mut evidence = vec![git_head.clone(), git_worktree.clone()];
        evidence.extend(tests.iter().cloned());
        evidence.extend(plain.iter().cloned());
        let snapshot = RepositorySnapshot::new(
            RepositorySnapshotInput {
                snapshot_id: RepositorySnapshotId::new(),
                project_id: self.project_id.clone(),
                task_id: self.task_id.clone(),
                workspace_id: self.workspace_id.clone(),
                head_oid: head.to_owned(),
                branch: Some("main".to_owned()),
                worktree_status_porcelain_v2: status.to_owned(),
                index_digest: digest(&format!("index {head}")),
                tracked_worktree_digest: digest(&format!("tracked {head} {status}")),
                capture_manifest_digest: manifest,
                git_head_evidence_id: git_head.evidence_id().clone(),
                git_worktree_evidence_id: git_worktree.evidence_id().clone(),
                file_evidence_ids: vec![],
                command_evidence_ids: plain
                    .iter()
                    .map(|item| item.evidence_id().clone())
                    .collect(),
                test_evidence_ids: tests
                    .iter()
                    .map(|item| item.evidence_id().clone())
                    .collect(),
                captured_at: at("2026-10-03T10:00:01Z"),
            },
            &evidence,
        )
        .unwrap();
        Run {
            snapshot,
            evidence,
            commands: order,
        }
    }

    /// A Checkpoint over `run` at Project sequence `sequence`: `facts` are
    /// `verified_fact`s in `verified_completed_work`, `open` are `unknown`
    /// open items, and `next` the `next_action`.
    fn checkpoint(
        &self,
        sequence: u64,
        run: &Run,
        facts: &[(&str, &[usize])],
        open: &[(&str, &[usize])],
        next: &str,
    ) -> CheckpointRecord {
        let statement = |classification, text: &str, cited: &[usize]| {
            CheckpointStatement::new(
                classification,
                text.to_owned(),
                cited
                    .iter()
                    .map(|index| run.commands[*index].clone())
                    .collect(),
            )
            .unwrap()
        };
        let checkpoint = Checkpoint::create(
            CheckpointInput {
                checkpoint_id: CheckpointId::new(),
                task_id: self.task_id.clone(),
                session_id: self.session_id.clone(),
                snapshot_id: run.snapshot.snapshot_id().clone(),
                project_event_sequence: sequence - 1,
                objective: "finish the Task".to_owned(),
                verified_completed_work: facts
                    .iter()
                    .map(|(text, cited)| {
                        statement(CheckpointClassification::VerifiedFact, text, cited)
                    })
                    .collect(),
                open_items: open
                    .iter()
                    .map(|(text, cited)| statement(CheckpointClassification::Unknown, text, cited))
                    .collect(),
                next_action: statement(CheckpointClassification::Recommendation, next, &[]),
                created_at: at("2026-10-03T10:00:02Z"),
            },
            &run.snapshot,
            &run.evidence,
        )
        .unwrap();
        CheckpointRecord {
            project_sequence: sequence,
            checkpoint,
            snapshot: run.snapshot.clone(),
            evidence: run.evidence.clone(),
            damaged: false,
        }
    }

    fn view(&self, checkpoints: Vec<CheckpointRecord>) -> TaskCapsuleView {
        TaskCapsuleView {
            project_sequence: checkpoints
                .last()
                .map_or(0, |record| record.project_sequence),
            ledger_chain_valid: true,
            task: Task {
                task_id: self.task_id.clone(),
                project_id: self.project_id.clone(),
                workspace_id: None,
                title: "the Task".to_owned(),
                description: None,
                status: TaskStatus::InProgress,
                created_at: at("2026-10-03T09:00:00Z"),
                updated_at: at("2026-10-03T09:00:00Z"),
                revision: 2,
                completion_evidence_ids: Vec::new(),
            },
            sessions: vec![],
            runs: checkpoints
                .iter()
                .map(|record| RecordedRun {
                    project_sequence: record.project_sequence,
                    snapshot: record.snapshot.clone(),
                    evidence: record.evidence.clone(),
                })
                .collect(),
            checkpoints,
            decisions: vec![],
            edges: vec![],
            damaged_records: BTreeSet::new(),
        }
    }

    /// What the probe sees at `head` with `status`, in the Task's Workspace.
    fn observation(&self, head: &str, status: &str) -> RepositoryObservation {
        RepositoryObservation {
            same_repository: true,
            same_workspace: true,
            repository_identity: self.identity.clone(),
            head_oid: head.to_owned(),
            branch: Some("main".to_owned()),
            status_porcelain_v2: status.to_owned(),
            index_digest: digest(&format!("index {head}")),
            tracked_worktree_digest: digest(&format!("tracked {head} {status}")),
            capture_manifest_digest: digest(&format!("manifest {head} {status}")),
            selected_files: BTreeMap::new(),
            untracked_paths: BTreeSet::new(),
            changed_since_base: None,
        }
    }

    /// A probe at `head`, clean, with `changed` paths since every base.
    fn probe(&self, head: &str, changed: &[(&str, Option<&[&str]>)]) -> CapsuleProbe {
        CapsuleProbe {
            probe: Probe {
                workspace_id: Some(self.workspace_id.clone()),
                observation: ProbeObservation::Observed(self.observation(head, "")),
            },
            changed_since: changed
                .iter()
                .map(|(base, paths)| {
                    (
                        (*base).to_owned(),
                        paths.map(|paths| paths.iter().map(|path| (*path).to_owned()).collect()),
                    )
                })
                .collect(),
        }
    }
}

fn render(view: &TaskCapsuleView, probe: &CapsuleProbe) -> (TaskCapsule, Value) {
    let capsule = task_capsule(view, probe).unwrap();
    let document = serde_json::from_str(capsule.as_str()).unwrap();
    (capsule, document)
}

fn claim(capsule: &TaskCapsule, number: u64) -> &Claim {
    capsule
        .claims()
        .iter()
        .find(|claim| claim.number == number)
        .unwrap()
}

fn statement_id(record: &CheckpointRecord, section: StatementSection, ordinal: usize) -> ClaimId {
    ClaimId::Statement(ClaimRef {
        checkpoint_id: record.checkpoint.checkpoint_id().clone(),
        section,
        ordinal,
    })
}

#[test]
fn d1_claim_ids_round_trip_and_reject_anything_else() {
    let checkpoint = CheckpointId::new();
    let event = EventId::new();
    for id in [
        ClaimId::Statement(ClaimRef {
            checkpoint_id: checkpoint.clone(),
            section: StatementSection::VerifiedCompletedWork,
            ordinal: 0,
        }),
        ClaimId::Statement(ClaimRef {
            checkpoint_id: checkpoint.clone(),
            section: StatementSection::OpenItems,
            ordinal: 12,
        }),
        ClaimId::Statement(ClaimRef {
            checkpoint_id: checkpoint.clone(),
            section: StatementSection::NextAction,
            ordinal: 0,
        }),
        ClaimId::Decision(event.clone()),
    ] {
        assert_eq!(
            ClaimId::parse(&id.wire()),
            Some(id.clone()),
            "{}",
            id.wire()
        );
    }
    assert_eq!(
        ClaimId::Decision(event.clone()).wire(),
        format!("decision/{event}")
    );
    for invalid in [
        String::new(),
        format!("{checkpoint}"),
        format!("{checkpoint}/open_items"),
        format!("{checkpoint}/open_items/01"),
        format!("{checkpoint}/open_items/-1"),
        format!("{checkpoint}/open_items/+1"),
        format!("{checkpoint}/open_items/1/2"),
        format!("{checkpoint}/facts/0"),
        format!("{}/open_items/0", checkpoint.as_str().to_uppercase()),
        "decision/".to_owned(),
        format!("decision/{checkpoint}/0"),
        format!("Decision/{event}"),
        "550e8400-e29b-41d4-a716-446655440000/open_items/0".to_owned(),
    ] {
        assert_eq!(ClaimId::parse(&invalid), None, "{invalid}");
    }
}

#[test]
fn d1_kind_follows_classification_then_section() {
    use CheckpointClassification::{Recommendation, Unknown, UserSuppliedContext, VerifiedFact};
    use StatementSection::{NextAction, OpenItems, VerifiedCompletedWork};
    for (section, classification, kind) in [
        (VerifiedCompletedWork, VerifiedFact, ClaimKind::Fact),
        (OpenItems, VerifiedFact, ClaimKind::Fact),
        (OpenItems, Unknown, ClaimKind::OpenItem),
        (OpenItems, Recommendation, ClaimKind::OpenItem),
        (OpenItems, UserSuppliedContext, ClaimKind::OpenItem),
        (NextAction, Recommendation, ClaimKind::NextAction),
        (NextAction, Unknown, ClaimKind::NextAction),
        (VerifiedCompletedWork, Unknown, ClaimKind::OpenItem),
    ] {
        assert_eq!(ClaimKind::of(section, classification), kind);
    }
}

/// D1: the Task's claims are every statement of every Checkpoint, numbered
/// in ledger order (Owner revision a); each `next_action` is superseded by
/// the next one.
#[test]
fn d1_claims_accumulate_over_checkpoints_and_next_action_supersedes() {
    use ClaimKind::{Fact, NextAction, OpenItem};
    use Standing::{Active, Superseded};
    let world = World::new();
    let first = world.run(X, "", &[test("sh t1")]);
    let second = world.run(X, "", &[test("sh t2")]);
    let third = world.run(X, "", &[test("sh t3")]);
    let view = world.view(vec![
        world.checkpoint(3, &first, &[("F1", &[0])], &[("O1", &[])], "N1"),
        world.checkpoint(
            6,
            &second,
            &[("F2", &[0])],
            &[("O2", &[0]), ("O3", &[])],
            "N2",
        ),
        world.checkpoint(9, &third, &[], &[], "N3"),
    ]);
    let (capsule, document) = render(&view, &world.probe(X, &[(X, Some(&[]))]));
    let listed = capsule
        .claims()
        .iter()
        .map(|claim| {
            (
                claim.number,
                claim.text.as_str(),
                claim.kind,
                claim.standing,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        listed,
        [
            (1, "F1", Fact, Active),
            (2, "O1", OpenItem, Active),
            (3, "N1", NextAction, Superseded),
            (4, "F2", Fact, Active),
            (5, "O2", OpenItem, Active),
            (6, "O3", OpenItem, Active),
            (7, "N2", NextAction, Superseded),
            (8, "N3", NextAction, Active),
        ]
    );
    assert_eq!(claim(&capsule, 3).superseded_by, [7]);
    assert_eq!(claim(&capsule, 7).superseded_by, [8]);
    assert_eq!(
        claim_by_number(capsule.claims(), 5),
        Some(&statement_id(
            &view.checkpoints[1],
            StatementSection::OpenItems,
            0
        ))
    );
    assert_eq!(claim_by_number(capsule.claims(), 9), None);
    assert_eq!(document["claims"][2]["superseded_by"][0]["number"], 7);
    assert_eq!(
        document["claims"][0]["claim_id"],
        format!(
            "{}/verified_completed_work/0",
            view.checkpoints[0].checkpoint.checkpoint_id()
        )
    );
    // Both Checkpoints' facts are verified at X; the Task takes them all.
    assert_eq!(capsule.judgement(), &Judgement::verified());
}

/// D1: explicit edges, by the rules: both ends claims of the Task, the
/// target first; withdrawn wins over superseded; standing never changes a
/// verdict.
#[test]
fn d1_edges_apply_only_when_valid() {
    use Standing::{Active, Superseded, Withdrawn};
    let world = World::new();
    let first = world.run(X, "", &[test("sh t")]);
    let second = world.run(Y, "", &[test("sh t")]);
    let view = world.view(vec![
        world.checkpoint(3, &first, &[("F1", &[0])], &[("O1", &[])], "N1"),
        world.checkpoint(6, &second, &[("F2", &[0])], &[("O2", &[])], "N2"),
    ]);
    let c1 = &view.checkpoints[0];
    let c2 = &view.checkpoints[1];
    let f1 = statement_id(c1, StatementSection::VerifiedCompletedWork, 0);
    let o1 = statement_id(c1, StatementSection::OpenItems, 0);
    let f2 = statement_id(c2, StatementSection::VerifiedCompletedWork, 0);
    let o2 = statement_id(c2, StatementSection::OpenItems, 0);
    let unknown = ClaimId::Decision(EventId::new());
    let supersedes = |sequence, target: &ClaimId, by: &ClaimId| ClaimEdge {
        project_sequence: sequence,
        relation: EdgeRelation::Supersedes,
        target: target.clone(),
        by: Some(by.clone()),
        reason: None,
    };
    let withdraws = |sequence, target: &ClaimId| ClaimEdge {
        project_sequence: sequence,
        relation: EdgeRelation::Withdraws,
        target: target.clone(),
        by: None,
        reason: None,
    };
    // (edges, standing of F1, O1, F2, O2)
    let cases: Vec<(Vec<ClaimEdge>, [Standing; 4])> = vec![
        (vec![], [Active, Active, Active, Active]),
        // F2 completes O1.
        (
            vec![supersedes(7, &o1, &f2)],
            [Active, Superseded, Active, Active],
        ),
        // F2 is the new version of F1.
        (
            vec![supersedes(7, &f1, &f2)],
            [Superseded, Active, Active, Active],
        ),
        // A target after its replacement is not an edge.
        (
            vec![supersedes(7, &f2, &f1)],
            [Active, Active, Active, Active],
        ),
        // A claim cannot supersede itself.
        (
            vec![supersedes(7, &f2, &f2)],
            [Active, Active, Active, Active],
        ),
        // An end that is not a claim of the Task is not an edge.
        (
            vec![supersedes(7, &unknown, &f2)],
            [Active, Active, Active, Active],
        ),
        (
            vec![supersedes(7, &o1, &unknown)],
            [Active, Active, Active, Active],
        ),
        (
            vec![withdraws(7, &unknown)],
            [Active, Active, Active, Active],
        ),
        // A withdrawal after its target holds.
        (vec![withdraws(7, &o2)], [Active, Active, Active, Withdrawn]),
        (vec![withdraws(4, &o1)], [Active, Withdrawn, Active, Active]),
        // A withdrawal recorded before its target does not.
        (vec![withdraws(5, &o2)], [Active, Active, Active, Active]),
        // Withdrawn wins over superseded.
        (
            vec![supersedes(7, &o1, &f2), withdraws(8, &o1)],
            [Active, Withdrawn, Active, Active],
        ),
        // A relation with the wrong shape is not an edge.
        (
            vec![ClaimEdge {
                project_sequence: 7,
                relation: EdgeRelation::Withdraws,
                target: o1.clone(),
                by: Some(f2.clone()),
                reason: None,
            }],
            [Active, Active, Active, Active],
        ),
    ];
    let probe = world.probe(Y, &[(X, Some(&["src/lib.rs"])), (Y, Some(&[]))]);
    let (plain, _) = render(&view, &probe);
    for (edges, expected) in cases {
        let mut edged = view.clone();
        edged.edges = edges.clone();
        let (capsule, _) = render(&edged, &probe);
        let standing = [1, 2, 4, 5].map(|number| claim(&capsule, number).standing);
        assert_eq!(standing, expected, "{edges:?}");
        for (with, without) in capsule.claims().iter().zip(plain.claims()) {
            assert_eq!(with.judgement, without.judgement, "{edges:?}");
        }
    }
    // F1 is stale (bound to X, HEAD is Y); superseded by F2, the Task no
    // longer takes it.
    assert_eq!(plain.judgement().verdict(), Stale);
    let mut edged = view.clone();
    edged.edges = vec![supersedes(7, &f1, &f2)];
    let (capsule, document) = render(&edged, &probe);
    assert_eq!(capsule.judgement(), &Judgement::verified());
    assert_eq!(claim(&capsule, 1).judgement.verdict(), Stale);
    assert_eq!(
        document["claims"][0]["superseded_by"][0]["claim_id"],
        f2.wire()
    );
}

#[test]
fn d1_edge_payloads_decode_only_for_the_task() {
    let task = TaskId::new();
    let target = ClaimId::Decision(EventId::new()).wire();
    let by = format!("{}/open_items/0", CheckpointId::new());
    let edge = |value: Value| ClaimEdge::from_payload(9, &task, &value);
    let decoded = edge(json!({
        "task_id": task, "relation": "supersedes", "target": target, "by": by,
    }))
    .unwrap();
    assert_eq!(decoded.relation, EdgeRelation::Supersedes);
    assert_eq!(decoded.project_sequence, 9);
    assert_eq!(decoded.by.unwrap().wire(), by);
    let withdrawn = edge(json!({
        "task_id": task, "relation": "withdraws", "target": target, "by": null,
    }))
    .unwrap();
    assert_eq!(withdrawn.by, None);
    for invalid in [
        json!({ "task_id": TaskId::new(), "relation": "supersedes", "target": target, "by": by }),
        json!({ "relation": "supersedes", "target": target, "by": by }),
        json!({ "task_id": task, "relation": "replaces", "target": target, "by": by }),
        json!({ "task_id": task, "relation": "supersedes", "target": target }),
        json!({ "task_id": task, "relation": "supersedes", "target": target, "by": null }),
        json!({ "task_id": task, "relation": "withdraws", "target": target, "by": by }),
        json!({ "task_id": task, "relation": "withdraws", "target": "claim 1" }),
        json!({ "task_id": task, "relation": "withdraws", "target": 1 }),
        json!("supersedes"),
    ] {
        assert_eq!(edge(invalid.clone()), None, "{invalid}");
    }
}

/// D2: a Decision is not a fact; without Evidence it is `partial`
/// (`evidence_unavailable`), with Evidence the worst `judge_evidence`.
/// P3-D2 (Owner, T13 N9): it is judged by the rule of a `verified_fact`:
/// `verified` only on fresh, passed tests, so a Decision citing only
/// repository Evidence is at best `partial`.
#[test]
#[allow(clippy::too_many_lines)] // Six Decisions, then the payload decoder.
fn d2_decisions_are_judged_by_their_evidence_and_never_reach_the_task() {
    let world = World::new();
    let run = world.run(X, "", &[test("sh t"), failing("sh f")]);
    let mut view = world.view(vec![world.checkpoint(3, &run, &[("F", &[0])], &[], "N")]);
    let sealed = |index: usize| SealedEvidence {
        evidence_id: run.commands[index].clone(),
        sealed: Some((
            run.evidence
                .iter()
                .find(|item| item.evidence_id() == &run.commands[index])
                .unwrap()
                .clone(),
            run.snapshot.clone(),
            run.evidence.clone(),
        )),
        other_task: false,
    };
    let decision = |sequence, text: &str, evidence: Vec<SealedEvidence>| DecisionRecord {
        decision_id: EventId::new(),
        project_sequence: sequence,
        session_id: world.session_id.clone(),
        payload: DecisionPayload {
            text: text.to_owned(),
            rationale: Some("because".to_owned()),
            evidence_ids: evidence
                .iter()
                .map(|item| item.evidence_id.clone())
                .collect(),
        },
        evidence,
        damaged: false,
    };
    let missing = SealedEvidence {
        evidence_id: EvidenceId::new(),
        sealed: None,
        other_task: false,
    };
    // The run's `git_head` Evidence, which is fresh.
    let git_head = SealedEvidence {
        evidence_id: run.evidence[0].evidence_id().clone(),
        sealed: Some((
            run.evidence[0].clone(),
            run.snapshot.clone(),
            run.evidence.clone(),
        )),
        other_task: false,
    };
    assert_eq!(run.evidence[0].kind(), EvidenceKind::GitHead);
    view.decisions = vec![
        decision(4, "D none", vec![]),
        decision(5, "D passed", vec![sealed(0)]),
        decision(6, "D failed", vec![sealed(1)]),
        decision(7, "D missing", vec![missing]),
        decision(8, "D git only", vec![git_head.clone()]),
        decision(9, "D git and passed", vec![git_head, sealed(0)]),
    ];
    let (capsule, document) = render(&view, &world.probe(X, &[(X, Some(&[]))]));
    // The fact is verified; the Decisions do not change the Task verdict.
    assert_eq!(capsule.judgement(), &Judgement::verified());
    for (number, text, expected, damaged) in [
        (
            3,
            "D none",
            judgement(Partial, &[EvidenceUnavailable]),
            false,
        ),
        (4, "D passed", Judgement::verified(), false),
        (
            5,
            "D failed",
            judgement(Partial, &[EvidenceNotPassed]),
            false,
        ),
        (
            6,
            "D missing",
            judgement(Tampered, &[ObjectMissing, EvidenceUnavailable]),
            true,
        ),
        (
            7,
            "D git only",
            judgement(Partial, &[EvidenceUnavailable]),
            false,
        ),
        (8, "D git and passed", Judgement::verified(), false),
    ] {
        let claim = claim(&capsule, number);
        assert_eq!(claim.text, text);
        assert_eq!(claim.kind, ClaimKind::Decision);
        assert_eq!(claim.judgement, expected, "{text}");
        assert_eq!(claim.damaged, damaged, "{text}");
        assert!(!claim.propagates());
    }
    assert_eq!(document["claims"][2]["classification"], Value::Null);
    assert_eq!(document["claims"][2]["rationale"], "because");
    assert_eq!(
        document["claims"][5]["evidence"][0]["canonical_digest"],
        Value::Null
    );

    let task = &world.task_id;
    let decoded = DecisionPayload::from_payload(
        task,
        &json!({ "task_id": task, "text": "use JCS", "rationale": null, "evidence_ids": [] }),
    )
    .unwrap();
    assert_eq!(decoded.rationale, None);
    for invalid in [
        json!({ "task_id": TaskId::new(), "text": "t", "evidence_ids": [] }),
        json!({ "task_id": task, "text": "", "evidence_ids": [] }),
        json!({ "task_id": task, "evidence_ids": [] }),
        json!({ "task_id": task, "text": "t", "rationale": 3 }),
        json!({ "task_id": task, "text": "t", "evidence_ids": ["no"] }),
        json!({ "task_id": task, "text": "t", "evidence_ids": "none" }),
    ] {
        assert_eq!(
            DecisionPayload::from_payload(task, &invalid),
            None,
            "{invalid}"
        );
    }
}

/// D4: the Task takes the worst active fact; superseded facts, open items,
/// next actions, and Decisions do not count; no active fact is `partial`.
#[test]
fn d4_task_verdict_is_the_worst_active_fact() {
    let world = World::new();
    let at_x = world.run(X, "", &[test("sh t")]);
    let at_y = world.run(Y, "", &[test("sh t")]);
    let probe = world.probe(Y, &[]);
    let cases: Vec<(Vec<CheckpointRecord>, Judgement)> = vec![
        (vec![], judgement(Partial, &[EvidenceUnavailable])),
        (
            vec![world.checkpoint(3, &at_y, &[], &[("O", &[0])], "N")],
            judgement(Partial, &[EvidenceUnavailable]),
        ),
        (
            vec![world.checkpoint(3, &at_y, &[("F", &[0])], &[], "N")],
            Judgement::verified(),
        ),
        (
            vec![
                world.checkpoint(3, &at_x, &[("F old", &[0])], &[], "N"),
                world.checkpoint(6, &at_y, &[("F new", &[0])], &[], "N"),
            ],
            judgement(Stale, &[HeadChanged, IndexChanged, EvidenceStale]),
        ),
    ];
    for (checkpoints, expected) in cases {
        let (capsule, document) = render(&world.view(checkpoints), &probe);
        assert_eq!(capsule.judgement(), &expected);
        assert_eq!(document["status"], expected.verdict().as_str());
    }
}

/// D7: a damaged record or a broken chain makes every claim `tampered`;
/// each claim says whether it rests on a damaged record and the text of
/// every readable claim is listed. P3-C2 (Owner, T12 S-6): its Evidence
/// verdicts are `tampered` too, each keeping its own reasons.
#[test]
fn d7_damage_makes_the_whole_task_tampered_and_names_each_record() {
    let world = World::new();
    let first = world.run(X, "", &[test("sh t")]);
    let second = world.run(X, "", &[test("sh t")]);
    let mut view = world.view(vec![
        world.checkpoint(3, &first, &[("F1", &[0])], &[], "N1"),
        world.checkpoint(6, &second, &[("F2", &[0])], &[], "N2"),
    ]);
    let probe = world.probe(X, &[(X, Some(&[]))]);
    let (intact, _) = render(&view, &probe);
    assert_eq!(intact.judgement(), &Judgement::verified());

    view.checkpoints[0].damaged = true;
    view.damaged_records.insert(DamagedRecord {
        record_type: LedgerRecordType::Evidence,
        record_id: first.commands[0].to_string(),
        reason: ObjectDigestMismatch,
    });
    view.damaged_records.insert(DamagedRecord {
        record_type: LedgerRecordType::Checkpoint,
        record_id: CheckpointId::new().to_string(),
        reason: ObjectMissing,
    });
    let (damaged, document) = render(&view, &probe);
    assert_eq!(
        damaged.judgement(),
        &judgement(Tampered, &[ObjectMissing, ObjectDigestMismatch])
    );
    for claim in damaged.claims() {
        assert_eq!(claim.judgement.verdict(), Tampered, "{}", claim.text);
        assert_eq!(claim.damaged, claim.text.ends_with('1'), "{}", claim.text);
        for item in &claim.evidence {
            assert_eq!(
                item.assessment.judgement,
                judgement(Tampered, &[ObjectMissing, ObjectDigestMismatch])
            );
        }
    }
    assert!(!damaged.as_str().contains("\"verified\""));
    assert_eq!(document["claims"][2]["text"], "F2");
    assert_eq!(document["claims"][2]["damaged"], false);
    assert_eq!(document["damaged_records"].as_array().unwrap().len(), 2);
    assert!(
        document["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!("record_integrity"))
    );
    let classes = document["continuation"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["class"] == 1)
        .map(|item| item["template"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(classes, ["damaged_record", "damaged_record"]);

    let mut broken = world.view(view.checkpoints.clone());
    broken.checkpoints[0].damaged = false;
    broken.ledger_chain_valid = false;
    let (capsule, document) = render(&broken, &probe);
    assert_eq!(
        capsule.judgement(),
        &judgement(Tampered, &[LedgerChainInvalid])
    );
    assert!(
        capsule
            .claims()
            .iter()
            .all(|claim| { claim.judgement.verdict() == Tampered && !claim.damaged })
    );
    assert_eq!(
        document["continuation"][1]["template"],
        "ledger_chain_invalid"
    );
}

/// D5: reruns are the passed runs behind active facts that are stale or
/// partial, deduplicated by argv in first-seen order; a failed run is never
/// rerun; a later verified run with the same argv is cited instead.
#[test]
#[allow(clippy::too_many_lines)] // Every rerun rule over one ledger.
fn d5_reruns_deduplicate_by_argv_and_prefer_a_later_verified_run() {
    let world = World::new();
    let first = world.run(X, "", &[test("cargo test"), test("cargo clippy")]);
    let second = world.run(Y, "", &[test("cargo test"), test("sh other")]);
    let third = world.run(Y, "", &[failing("sh failing"), test("cargo test")]);
    let view = world.view(vec![
        world.checkpoint(3, &first, &[("F1", &[0, 1])], &[], "N1"),
        world.checkpoint(6, &second, &[("F2", &[0]), ("F3", &[1])], &[], "N2"),
        world.checkpoint(9, &third, &[], &[("O", &[0])], "N3"),
    ]);
    let probe = world.probe(Z, &[]);
    let (capsule, document) = render(&view, &probe);
    let reruns = capsule
        .reruns()
        .iter()
        .map(|rerun| {
            (
                rerun.argv.join(" "),
                rerun.claims.clone(),
                rerun.evidence_ids.len(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reruns,
        [
            ("cargo test".to_owned(), vec![1, 3], 2),
            ("cargo clippy".to_owned(), vec![1], 1),
            ("sh other".to_owned(), vec![4], 1),
        ]
    );
    assert!(
        capsule
            .reruns()
            .iter()
            .all(|rerun| rerun.verified_run.is_none())
    );
    assert_eq!(document["rerun"].as_array().unwrap().len(), 3);
    assert_eq!(document["rerun"][0]["at_head"], Z);
    assert_eq!(document["rerun"][0]["claims"], json!([1, 3]));

    // At Y, the third run's `cargo test` is verified and later than both
    // stale ones: cite it instead of rerunning.
    let probe = world.probe(Y, &[]);
    let (capsule, document) = render(&view, &probe);
    let cargo = &capsule.reruns()[0];
    assert_eq!(cargo.argv, argv("cargo test"));
    assert_eq!(cargo.claims, [1]);
    assert_eq!(
        cargo.verified_run,
        Some((
            third.commands[1].clone(),
            third.snapshot.snapshot_id().clone()
        ))
    );
    // `cargo test` at Y in F2 is verified, so only F1's is a candidate.
    assert_eq!(cargo.evidence_ids, [first.commands[0].clone()]);
    let listed = document["rerun"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rerun| rerun["argv"].clone())
        .collect::<Vec<_>>();
    assert_eq!(listed, [json!(["cargo", "clippy"])]);
    let templates = document["continuation"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["class"] == 5)
        .map(|item| item["template"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(templates, ["reaffirm", "rerun"]);

    // A superseded fact is never rerun; nor a verified one; the failed run
    // behind the open item never is.
    let mut edged = view.clone();
    edged.edges = vec![ClaimEdge {
        project_sequence: 10,
        relation: EdgeRelation::Withdraws,
        target: statement_id(
            &view.checkpoints[0],
            StatementSection::VerifiedCompletedWork,
            0,
        ),
        by: None,
        reason: None,
    }];
    let (capsule, _) = render(&edged, &world.probe(Y, &[]));
    assert!(capsule.reruns().is_empty());
    let (capsule, _) = render(&view, &world.probe(Z, &[]));
    assert!(
        capsule
            .reruns()
            .iter()
            .all(|rerun| rerun.argv != argv("sh failing"))
    );
    // An unconfirmed (partial) passed run is rerun too.
    let mut unavailable = world.probe(Z, &[]);
    unavailable.probe.observation = ProbeObservation::Unavailable;
    let (capsule, _) = render(&view, &unavailable);
    assert_eq!(capsule.reruns()[0].judgement.verdict(), Partial);
}

/// D4 and Owner revision b: at most 16 base commits are compared, the most
/// recent first; the rest are listed unprobed with the gap
/// `base_commits_over_limit`, and no verdict changes.
#[test]
fn d4_base_commits_over_the_limit_are_listed_not_counted() {
    let world = World::new();
    let heads = (0..20)
        .map(|index| format!("{index:040x}"))
        .collect::<Vec<_>>();
    let records = heads
        .iter()
        .enumerate()
        .map(|(index, head)| {
            let run = world.run(head, "", &[test("sh t")]);
            world.checkpoint(3 * (index as u64 + 1), &run, &[("F", &[0])], &[], "N")
        })
        .collect::<Vec<_>>();
    let view = world.view(records);
    let bases = capsule_bases(&view);
    let newest_first = heads.iter().rev().cloned().collect::<Vec<_>>();
    assert_eq!(bases.probed, newest_first[..16]);
    assert_eq!(bases.omitted, newest_first[16..]);
    let changed = bases
        .probed
        .iter()
        .map(|base| (base.as_str(), Some(&["src/lib.rs"][..])))
        .collect::<Vec<_>>();
    let probe = world.probe(&heads[19], &changed);
    let (capsule, document) = render(&view, &probe);
    let listed = document["changed_paths_by_base"].as_array().unwrap();
    assert_eq!(listed.len(), 20);
    for (index, entry) in listed.iter().enumerate() {
        assert_eq!(entry["base_commit"], newest_first[index]);
        assert_eq!(entry["probed"], index < 16);
        assert_eq!(entry["complete"], index < 16);
        let paths = if index < 16 {
            json!(["repo://src/lib.rs"])
        } else {
            json!([])
        };
        assert_eq!(entry["paths"], paths);
    }
    assert!(
        document["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!("base_commits_over_limit"))
    );
    // Verdicts do not depend on the limit: the same claims with every base
    // probed judge the same.
    let mut unlimited = world.probe(&heads[19], &changed);
    for base in &bases.omitted {
        unlimited
            .changed_since
            .insert(base.clone(), Some(BTreeSet::new()));
    }
    let (all, _) = render(&view, &unlimited);
    for (left, right) in capsule.claims().iter().zip(all.claims()) {
        assert_eq!(left.judgement, right.judgement);
    }
    assert_eq!(capsule.judgement(), all.judgement());
}

/// D5 changed paths: the diff and status since the base, the untracked
/// paths, and a dirty Snapshot's status paths; an unavailable base is
/// incomplete with the gap `base_commit_unavailable`.
#[test]
fn d5_changed_paths_per_base() {
    let world = World::new();
    let dirty = "1 .M N... 100644 100644 100644 a b src/dirty.rs";
    let at_x = world.run(X, dirty, &[test("sh t")]);
    let at_y = world.run(Y, "", &[test("sh t")]);
    let view = world.view(vec![
        world.checkpoint(3, &at_x, &[("F1", &[0])], &[], "N"),
        world.checkpoint(6, &at_y, &[("F2", &[0])], &[], "N"),
    ]);
    let mut probe = world.probe(Z, &[(X, Some(&["src/lib.rs"])), (Y, None)]);
    if let ProbeObservation::Observed(observation) = &mut probe.probe.observation {
        observation.untracked_paths.insert("notes.txt".to_owned());
    }
    let (_, document) = render(&view, &probe);
    assert_eq!(
        document["changed_paths_by_base"],
        json!([
            { "base_commit": Y, "probed": true, "complete": false, "paths": [] },
            {
                "base_commit": X, "probed": true, "complete": true,
                "paths": ["repo://notes.txt", "repo://src/dirty.rs", "repo://src/lib.rs"],
            },
        ])
    );
    let gaps = document["gaps"].as_array().unwrap();
    assert!(gaps.contains(&json!("base_commit_unavailable")));
    // A probe that cannot run reports no paths and no base gap.
    probe.probe.observation = ProbeObservation::Unavailable;
    let (_, document) = render(&view, &probe);
    assert!(
        document["changed_paths_by_base"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["complete"] == false && entry["paths"] == json!([]))
    );
    assert_eq!(document["gaps"], json!(["workspace_unavailable"]));
}

/// D5: classes ascending, ledger order within each; every claim exactly
/// once; Sessions that did not end normally in class 2.
#[test]
#[allow(clippy::too_many_lines)] // One full continuation.
fn d5_continuation_orders_classes_and_lists_every_claim_once() {
    let world = World::new();
    let at_x = world.run(X, "", &[test("sh a"), failing("sh f")]);
    let at_y = world.run(Y, "", &[test("sh b")]);
    let mut view = world.view(vec![
        world.checkpoint(3, &at_x, &[("F stale", &[0])], &[("O failed", &[1])], "N1"),
        world.checkpoint(6, &at_y, &[("F fresh", &[0])], &[("O open", &[])], "N2"),
    ]);
    view.decisions = vec![DecisionRecord {
        decision_id: EventId::new(),
        project_sequence: 7,
        session_id: world.session_id.clone(),
        payload: DecisionPayload {
            text: "D".to_owned(),
            rationale: None,
            evidence_ids: vec![],
        },
        evidence: vec![],
        damaged: false,
    }];
    let session = |end, minute: u32| SessionRecord {
        session: Session {
            session_id: SessionId::new(),
            workspace_id: world.workspace_id.clone(),
            agent_id: crate::AgentId::new(),
            started_at: at(&format!("2026-10-03T10:{minute:02}:00Z")),
            ended_at: None,
            revision: 1,
            agent: None,
            ended_by: None,
        },
        end,
        last_recorded_at: Some(at(&format!("2026-10-03T10:{minute:02}:30Z"))),
        resumed_from: None,
    };
    view.sessions = vec![
        session(SessionEnd::Ended, 1),
        session(SessionEnd::Open, 2),
        session(SessionEnd::Crashed, 3),
    ];
    let (capsule, document) = render(&view, &world.probe(Y, &[]));
    let items = document["continuation"].as_array().unwrap();
    let shape = items
        .iter()
        .map(|item| {
            (
                item["class"].as_u64().unwrap(),
                item["template"].as_str().unwrap().to_owned(),
                item["args"]["number"].as_u64(),
            )
        })
        .collect::<Vec<_>>();
    let expected: Vec<(u64, &str, Option<u64>)> = vec![
        (0, "header", None),
        (2, "session_open", None),
        (2, "session_crashed", None),
        (3, "open_item", Some(2)),
        (3, "open_item", Some(5)),
        (4, "fact", Some(1)),
        (5, "rerun", None),
        (6, "fact", Some(4)),
        (7, "decision", Some(7)),
        (8, "next_action", Some(6)),
        (9, "superseded", Some(3)),
    ];
    assert_eq!(
        shape,
        expected
            .iter()
            .map(|(class, template, number)| (*class, (*template).to_owned(), *number))
            .collect::<Vec<_>>()
    );
    let numbers = shape
        .iter()
        .filter_map(|(class, template, number)| {
            (*class >= 3 && template != "rerun").then_some(*number)
        })
        .flatten()
        .collect::<BTreeSet<_>>();
    assert_eq!(numbers, (1..=7).collect());
    assert_eq!(numbers.len(), capsule.claims().len());
    // Only a verified claim's item says verified.
    for item in items {
        if item["verdict"] == "verified" {
            assert!(
                matches!(item["class"].as_u64(), Some(0 | 6 | 7 | 8)),
                "{item}"
            );
        }
    }
    assert_eq!(items[5]["args"]["base_commits"], json!([X]));
    assert_eq!(items[5]["args"]["head_oid"], Y);
    assert_eq!(items[0]["args"]["head_oid"], Y);
    // The Task takes its stale active fact.
    assert_eq!(items[0]["verdict"], "stale");
    assert_eq!(document["omitted"], json!([]));
    assert_eq!(
        document["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["end_state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ended", "open", "crashed"]
    );
    // P3-L L7: each Session carries the time of its latest ledger write.
    assert_eq!(
        document["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| session["last_recorded_at"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "2026-10-03T10:01:30Z",
            "2026-10-03T10:02:30Z",
            "2026-10-03T10:03:30Z"
        ]
    );
}

#[test]
fn d4_gaps_name_what_cannot_be_told() {
    let world = World::new();
    let run = world.run(X, "? untracked.txt", &[test("sh t")]);
    let probe = world.probe(X, &[(X, Some(&[]))]);
    for (checkpoints, expected) in [
        (vec![], vec!["no_checkpoint", "no_verified_fact"]),
        (
            vec![world.checkpoint(3, &run, &[], &[("O", &[])], "N")],
            vec!["no_verified_fact"],
        ),
        (
            vec![world.checkpoint(3, &run, &[("F", &[0])], &[], "N")],
            vec!["untracked_files_not_captured"],
        ),
    ] {
        let (_, document) = render(&world.view(checkpoints), &probe);
        assert_eq!(document["gaps"], json!(expected));
    }
}

#[test]
fn capsule_digest_is_the_digest_of_the_document_without_it() {
    let world = World::new();
    let run = world.run(X, "", &[test("sh t")]);
    let view = world.view(vec![world.checkpoint(3, &run, &[("F", &[0])], &[], "N")]);
    let (capsule, mut document) = render(&view, &world.probe(X, &[(X, Some(&[]))]));
    assert_eq!(
        document["capsule_digest"],
        format!("sha256:{}", capsule.digest())
    );
    assert_eq!(document["ontology_version"], ONTOLOGY_VERSION);
    assert_eq!(ONTOLOGY_VERSION, "continuity.work-state-ontology.v4");
    assert!(document.get("generated_at").is_none());
    document.as_object_mut().unwrap().remove("capsule_digest");
    let bytes = CanonicalJson::from_trusted_value(&document).unwrap();
    assert_eq!(bytes.sha256(), *capsule.digest());
    // The grounding digest does not depend on the probe.
    let (moved, moved_document) = render(&view, &world.probe(Y, &[]));
    assert_ne!(moved.digest(), capsule.digest());
    assert_eq!(
        moved_document["claims"][0]["grounding_digest"],
        document["claims"][0]["grounding_digest"]
    );
}

/// A small deterministic generator (`SplitMix64`), so the property tests
/// need no crate.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[usize::try_from(self.below(items.len() as u64)).unwrap()]
    }
}

/// One generated ledger: 1–4 Checkpoints at random commits, each with
/// facts citing passed tests and open items citing any run, Decisions,
/// random edges, and a random probe.
#[allow(clippy::too_many_lines)] // One whole generated ledger.
fn generated(seed: u64) -> (World, TaskCapsuleView, CapsuleProbe) {
    let mut random = Random(seed);
    let world = World::new();
    let heads = [X, Y, Z];
    let statuses = [
        "",
        "? new.txt",
        "1 .M N... 100644 100644 100644 a b src/lib.rs",
    ];
    let commands = ["cargo test", "sh t", "cargo clippy"];
    let mut checkpoints = Vec::new();
    let mut sequence = 0;
    for _ in 0..=random.below(3) {
        sequence += 1 + random.below(3);
        let mut run_commands = Vec::new();
        for _ in 0..=random.below(2) {
            let argv = *random.pick(&commands);
            run_commands.push(if random.below(4) == 0 {
                failing(argv)
            } else {
                test(argv)
            });
        }
        let run = world.run(random.pick(&heads), random.pick(&statuses), &run_commands);
        let passed = run_commands
            .iter()
            .enumerate()
            .filter(|(_, command)| command.exit_code == 0)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let all = (0..run_commands.len()).collect::<Vec<_>>();
        let facts = if passed.is_empty() {
            vec![]
        } else {
            vec![("fact", passed.clone())]
        };
        let opens = (0..random.below(3))
            .map(|_| {
                let cited = all
                    .iter()
                    .copied()
                    .filter(|_| random.below(2) == 0)
                    .collect::<Vec<_>>();
                ("open", cited)
            })
            .collect::<Vec<_>>();
        let facts = facts
            .iter()
            .map(|(text, cited)| (*text, cited.as_slice()))
            .collect::<Vec<_>>();
        let opens = opens
            .iter()
            .map(|(text, cited)| (*text, cited.as_slice()))
            .collect::<Vec<_>>();
        checkpoints.push(world.checkpoint(sequence, &run, &facts, &opens, "next"));
    }
    let mut view = world.view(checkpoints);
    let ids = view
        .checkpoints
        .iter()
        .flat_map(|record| {
            let checkpoint = &record.checkpoint;
            let mut ids = (0..checkpoint.verified_completed_work().len())
                .map(|ordinal| {
                    statement_id(record, StatementSection::VerifiedCompletedWork, ordinal)
                })
                .collect::<Vec<_>>();
            ids.extend(
                (0..checkpoint.open_items().len())
                    .map(|ordinal| statement_id(record, StatementSection::OpenItems, ordinal)),
            );
            ids
        })
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        for _ in 0..random.below(4) {
            let target = random.pick(&ids).clone();
            let by = random.pick(&ids).clone();
            view.edges.push(if random.below(2) == 0 {
                ClaimEdge {
                    project_sequence: sequence + 1,
                    relation: EdgeRelation::Supersedes,
                    target,
                    by: Some(by),
                    reason: None,
                }
            } else {
                ClaimEdge {
                    project_sequence: sequence + 1,
                    relation: EdgeRelation::Withdraws,
                    target,
                    by: None,
                    reason: None,
                }
            });
        }
    }
    let head = *random.pick(&heads);
    let mut probe = world.probe(
        head,
        &[(X, Some(&["src/lib.rs"])), (Y, None), (Z, Some(&[]))],
    );
    match random.below(5) {
        0 => probe.probe.observation = ProbeObservation::Unavailable,
        1 => {
            let status = *random.pick(&statuses);
            probe.probe.observation = ProbeObservation::Observed(world.observation(head, status));
        }
        _ => {}
    }
    (world, view, probe)
}

/// P4-C: the JCS bytes of the Capsule of the generated ledger `seed`, for
/// the property tests of the Context Pack.
pub(crate) fn generated_capsule(seed: u64) -> String {
    let (_, view, probe) = generated(seed);
    task_capsule(&view, &probe).unwrap().as_str().to_owned()
}

/// Every Evidence object of `claim`, judged straight from the records with
/// `judge_evidence` and (P3-D3) the Task's run history.
fn direct_verdicts(view: &TaskCapsuleView, claim: &Claim, probe: &Probe) -> Vec<Verdict> {
    let history = run_history(&view.runs);
    let record = view
        .checkpoints
        .iter()
        .find(|record| Some(record.checkpoint.checkpoint_id()) == claim.checkpoint_id.as_ref())
        .unwrap();
    claim
        .evidence
        .iter()
        .map(|item| {
            let evidence = record
                .evidence
                .iter()
                .find(|evidence| evidence.evidence_id() == &item.assessment.evidence_id)
                .unwrap();
            crate::judge_evidence(
                &record.snapshot,
                &record.evidence,
                evidence,
                &history,
                probe,
            )
            .verdict()
        })
        .collect()
}

/// Property: on an intact ledger, a claim is `verified` exactly when it
/// cites Evidence and `judge_evidence` gives `verified` for every object.
#[test]
fn property_a_claim_is_verified_iff_judge_evidence_verifies_its_evidence() {
    for seed in 0..300 {
        let (_, view, probe) = generated(seed);
        let (capsule, _) = render(&view, &probe);
        for claim in capsule.claims() {
            let direct = direct_verdicts(&view, claim, &probe.probe);
            let expected = !direct.is_empty() && direct.iter().all(|verdict| *verdict == Verified);
            assert_eq!(
                claim.judgement.verdict() == Verified,
                expected,
                "seed {seed}, claim {} {:?}: {direct:?}",
                claim.number,
                claim.judgement
            );
            for (item, verdict) in claim.evidence.iter().zip(&direct) {
                assert_eq!(item.assessment.judgement.verdict(), *verdict, "seed {seed}");
            }
        }
    }
}

/// Property: standing and edges change no claim's verdict.
#[test]
fn property_standing_and_edges_change_no_verdict() {
    for seed in 0..300 {
        let (_, view, probe) = generated(seed);
        let mut bare = view.clone();
        bare.edges.clear();
        let (with, _) = render(&view, &probe);
        let (without, _) = render(&bare, &probe);
        assert_eq!(with.claims().len(), without.claims().len());
        for (left, right) in with.claims().iter().zip(without.claims()) {
            assert_eq!(left.id, right.id, "seed {seed}");
            assert_eq!(left.judgement, right.judgement, "seed {seed}");
            assert_eq!(
                left.evidence
                    .iter()
                    .map(|item| &item.assessment.judgement)
                    .collect::<Vec<_>>(),
                right
                    .evidence
                    .iter()
                    .map(|item| &item.assessment.judgement)
                    .collect::<Vec<_>>(),
                "seed {seed}"
            );
        }
    }
}

/// Property (P3-D3): the later-run rule never makes a claim, an Evidence
/// verdict inside one, or the Task better: the Capsule over the Task's runs
/// against the same Capsule with no runs to overrule anything.
#[test]
fn property_the_later_run_rule_never_judges_a_claim_better() {
    let mut differed = 0;
    for seed in 0..300 {
        let (_, view, probe) = generated(seed);
        let mut bare = view.clone();
        bare.runs.clear();
        let (with, _) = render(&view, &probe);
        let (without, _) = render(&bare, &probe);
        assert!(with.judgement().verdict() >= without.judgement().verdict());
        for (left, right) in with.claims().iter().zip(without.claims()) {
            assert!(
                left.judgement.verdict() >= right.judgement.verdict(),
                "seed {seed}, claim {}",
                left.number
            );
            differed += usize::from(left.judgement != right.judgement);
            for (item, plain) in left.evidence.iter().zip(&right.evidence) {
                assert!(
                    item.assessment.judgement.verdict() >= plain.assessment.judgement.verdict(),
                    "seed {seed}"
                );
            }
        }
    }
    assert!(differed > 0);
}

/// Property: the same input renders the same bytes twice.
#[test]
fn property_the_same_input_renders_identical_bytes() {
    for seed in 0..300 {
        let (_, view, probe) = generated(seed);
        let first = task_capsule(&view, &probe).unwrap();
        let second = task_capsule(&view.clone(), &probe.clone()).unwrap();
        assert_eq!(first.as_str(), second.as_str(), "seed {seed}");
        assert_eq!(first.digest(), second.digest(), "seed {seed}");
    }
}

/// P3-C: what `memproved` writes for an edge and a Decision decodes back
/// to the same edge and Decision.
#[test]
fn p3c_written_payloads_decode_back() {
    let task = TaskId::new();
    let target = ClaimId::Decision(EventId::new());
    let by = ClaimId::parse(&format!("{}/open_items/0", CheckpointId::new())).unwrap();
    for (relation, by) in [
        (EdgeRelation::Supersedes, Some(by)),
        (EdgeRelation::Withdraws, None),
    ] {
        let proposed = ProposedEdge {
            relation,
            target: target.clone(),
            by: by.clone(),
            reason: None,
            at: "$.x".to_owned(),
        };
        let payload = ClaimEdge::payload(&task, &proposed);
        assert_eq!(payload["by"].is_null(), by.is_none());
        let decoded = ClaimEdge::from_payload(4, &task, &payload).unwrap();
        assert_eq!(
            (decoded.relation, decoded.target, decoded.by),
            (relation, target.clone(), by)
        );
        assert_eq!(ClaimEdge::from_payload(4, &TaskId::new(), &payload), None);
    }
    for rationale in [None, Some("why".to_owned())] {
        let decision = DecisionPayload {
            text: "use JCS".to_owned(),
            rationale,
            evidence_ids: vec![EvidenceId::new()],
        };
        let payload = decision.payload(&task);
        assert_eq!(
            DecisionPayload::from_payload(&task, &payload),
            Some(decision)
        );
    }
}

/// P3-C: a declared edge must name an earlier readable claim of the Task,
/// not one the write records itself, and each target once; the first
/// refusal in request order is reported with its location.
#[test]
#[allow(clippy::too_many_lines)] // Every rule, then the read path.
fn p3c_check_edges_refuses_each_rule_in_request_order() {
    let world = World::new();
    let run = world.run(X, "", &[test("sh t")]);
    let mut view = world.view(vec![world.checkpoint(
        3,
        &run,
        &[("F1", &[0])],
        &[("O1", &[])],
        "N1",
    )]);
    let decision = EventId::new();
    view.decisions.push(DecisionRecord {
        decision_id: decision.clone(),
        project_sequence: 4,
        session_id: world.session_id.clone(),
        payload: DecisionPayload {
            text: "D".to_owned(),
            rationale: None,
            evidence_ids: Vec::new(),
        },
        evidence: Vec::new(),
        damaged: false,
    });
    view.project_sequence = 4;
    let c1 = &view.checkpoints[0];
    let f1 = statement_id(c1, StatementSection::VerifiedCompletedWork, 0);
    let o1 = statement_id(c1, StatementSection::OpenItems, 0);
    let n1 = statement_id(c1, StatementSection::NextAction, 0);
    let d = ClaimId::Decision(decision);
    assert_eq!(
        readable_claims(&view),
        BTreeMap::from([
            (f1.clone(), 3),
            (o1.clone(), 3),
            (n1.clone(), 3),
            (d.clone(), 4)
        ])
    );
    let new_checkpoint = CheckpointId::new();
    let new = |section, ordinal| {
        ClaimId::Statement(ClaimRef {
            checkpoint_id: new_checkpoint.clone(),
            section,
            ordinal,
        })
    };
    let new_fact = new(StatementSection::VerifiedCompletedWork, 0);
    let new_open = new(StatementSection::OpenItems, 0);
    let new_claims = [
        (new_fact.clone(), ClaimKind::Fact),
        (new_open.clone(), ClaimKind::OpenItem),
    ];
    let supersedes = |at: &str, target: &ClaimId, by: &ClaimId| ProposedEdge {
        relation: EdgeRelation::Supersedes,
        target: target.clone(),
        by: Some(by.clone()),
        reason: None,
        at: at.to_owned(),
    };
    let withdraws = |at: &str, target: &ClaimId| ProposedEdge {
        relation: EdgeRelation::Withdraws,
        target: target.clone(),
        by: None,
        reason: Some("no longer holds".to_owned()),
        at: at.to_owned(),
    };
    let unknown = ClaimId::Decision(EventId::new());
    let out_of_range = ClaimId::Statement(ClaimRef {
        checkpoint_id: c1.checkpoint.checkpoint_id().clone(),
        section: StatementSection::OpenItems,
        ordinal: 1,
    });
    // Every kind of earlier claim may be named, once each.
    assert_eq!(
        check_edges(
            &view,
            &new_claims,
            &[
                supersedes("$.a", &o1, &new_fact),
                supersedes("$.b", &d, &new_open),
                withdraws("$.c", &f1),
                withdraws("$.d", &n1),
            ],
        ),
        Ok(())
    );
    for (edges, at, rule) in [
        (
            vec![supersedes("$.a", &new_open, &new_open)],
            "$.a",
            EdgeRule::SelfReference,
        ),
        (
            vec![supersedes("$.a", &new_fact, &new_open)],
            "$.a",
            EdgeRule::NotEarlier,
        ),
        (
            vec![withdraws("$.a", &new_fact)],
            "$.a",
            EdgeRule::NotEarlier,
        ),
        (vec![withdraws("$.a", &unknown)], "$.a", EdgeRule::NotAClaim),
        (
            vec![withdraws("$.a", &out_of_range)],
            "$.a",
            EdgeRule::NotAClaim,
        ),
        (
            vec![supersedes("$.a", &o1, &new_fact), withdraws("$.b", &o1)],
            "$.b",
            EdgeRule::Duplicate,
        ),
        (
            vec![
                withdraws("$.a", &f1),
                withdraws("$.b", &unknown),
                withdraws("$.c", &f1),
            ],
            "$.b",
            EdgeRule::NotAClaim,
        ),
    ] {
        let refusal = check_edges(&view, &new_claims, &edges).unwrap_err();
        assert_eq!((refusal.at.as_str(), refusal.rule), (at, rule), "{edges:?}");
        assert!(refusal.detail().starts_with(at));
    }
    // A claim the ledger places after the write is not earlier.
    let mut later = view.clone();
    later.project_sequence = 3;
    assert_eq!(
        check_edges(&later, &new_claims, &[withdraws("$.a", &d)])
            .unwrap_err()
            .rule,
        EdgeRule::NotEarlier
    );
    // Edges that pass are applied when read back.
    let mut edged = view.clone();
    edged
        .checkpoints
        .push(world.checkpoint(5, &run, &[("F2", &[0])], &[], "N2"));
    let c2 = edged.checkpoints[1].checkpoint.checkpoint_id().clone();
    let f2 = ClaimId::Statement(ClaimRef {
        checkpoint_id: c2,
        section: StatementSection::VerifiedCompletedWork,
        ordinal: 0,
    });
    edged.edges = [supersedes("$.a", &o1, &f2), withdraws("$.b", &d)]
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            ClaimEdge::from_payload(
                6 + index as u64,
                &world.task_id,
                &ClaimEdge::payload(&world.task_id, edge),
            )
            .unwrap()
        })
        .collect();
    let probe = world.probe(X, &[(X, Some(&[]))]);
    let (capsule, _) = render(&edged, &probe);
    let standing = |id: &ClaimId| {
        capsule
            .claims()
            .iter()
            .find(|claim| &claim.id == id)
            .unwrap()
            .standing
    };
    assert_eq!(standing(&o1), Standing::Superseded);
    assert_eq!(standing(&d), Standing::Withdrawn);
}

/// P3-C2 (Owner, T12 S-6): a `tampered` Capsule holds no `verified` at any
/// level: claims, the Evidence inside them, reruns, and continuation items
/// are `tampered`, and a later verified run is not offered for
/// reaffirmation. Each item keeps its own reasons.
#[test]
fn p3c2_a_tampered_capsule_holds_no_verified() {
    let world = World::new();
    let first = world.run(X, "", &[test("cargo test")]);
    let second = world.run(Y, "", &[test("cargo test")]);
    let view = world.view(vec![
        world.checkpoint(3, &first, &[("F1", &[0])], &[], "N1"),
        world.checkpoint(6, &second, &[("F2", &[0])], &[], "N2"),
    ]);
    let probe = world.probe(Y, &[(X, Some(&["src/lib.rs"])), (Y, Some(&[]))]);
    // Intact: F2 is verified and F1's stale run is reaffirmed by F2's.
    let (intact, _) = render(&view, &probe);
    assert!(intact.as_str().contains("\"verified\""));
    assert!(intact.as_str().contains("\"reaffirm\""));

    let mut damaged = view.clone();
    damaged.damaged_records.insert(DamagedRecord {
        record_type: LedgerRecordType::Task,
        record_id: world.task_id.to_string(),
        reason: ObjectDigestMismatch,
    });
    let mut broken = view.clone();
    broken.ledger_chain_valid = false;
    for (view, reason) in [
        (damaged, ObjectDigestMismatch),
        (broken, LedgerChainInvalid),
    ] {
        let (capsule, document) = render(&view, &probe);
        assert!(!capsule.as_str().contains("\"verified\""), "{reason:?}");
        assert!(!capsule.as_str().contains("\"reaffirm\""), "{reason:?}");
        assert_eq!(capsule.judgement().verdict(), Tampered);
        for claim in capsule.claims() {
            assert!(claim.judgement.reasons().contains(&reason));
            for item in &claim.evidence {
                assert_eq!(item.assessment.judgement.verdict(), Tampered);
                assert!(item.assessment.judgement.reasons().contains(&reason));
            }
        }
        // F1 keeps its own reasons beside the integrity reason.
        assert_eq!(
            claim(&capsule, 1).evidence[0].assessment.judgement,
            judgement(Tampered, &[HeadChanged, IndexChanged, reason])
        );
        // The stale run is a rerun now, tampered.
        assert_eq!(capsule.reruns().len(), 1);
        assert_eq!(capsule.reruns()[0].verified_run, None);
        assert_eq!(document["rerun"][0]["verdict"], "tampered");
    }
}

/// P3-C2 (Owner, T12 N5): a fact is superseded only by a fact; withdrawal
/// takes any kind. The write check refuses the edge, and the read path does
/// not apply one already recorded.
#[test]
#[allow(clippy::too_many_lines)] // The write check, then the read path.
fn p3c2_a_fact_is_superseded_only_by_a_fact() {
    let world = World::new();
    let run = world.run(X, "", &[test("sh t")]);
    let mut view = world.view(vec![world.checkpoint(
        3,
        &run,
        &[("F1", &[0])],
        &[("O1", &[])],
        "N1",
    )]);
    let decision = EventId::new();
    view.decisions.push(DecisionRecord {
        decision_id: decision.clone(),
        project_sequence: 4,
        session_id: world.session_id.clone(),
        payload: DecisionPayload {
            text: "D".to_owned(),
            rationale: None,
            evidence_ids: Vec::new(),
        },
        evidence: Vec::new(),
        damaged: false,
    });
    view.project_sequence = 4;
    let c1 = &view.checkpoints[0];
    let f1 = statement_id(c1, StatementSection::VerifiedCompletedWork, 0);
    let o1 = statement_id(c1, StatementSection::OpenItems, 0);
    let d = ClaimId::Decision(decision);
    let new_checkpoint = CheckpointId::new();
    let new = |section, ordinal| {
        ClaimId::Statement(ClaimRef {
            checkpoint_id: new_checkpoint.clone(),
            section,
            ordinal,
        })
    };
    let new_fact = new(StatementSection::VerifiedCompletedWork, 0);
    let new_open = new(StatementSection::OpenItems, 0);
    let new_next = new(StatementSection::NextAction, 0);
    let new_decision = ClaimId::Decision(EventId::new());
    let new_claims = [
        (new_fact.clone(), ClaimKind::Fact),
        (new_open.clone(), ClaimKind::OpenItem),
        (new_next.clone(), ClaimKind::NextAction),
        (new_decision.clone(), ClaimKind::Decision),
    ];
    let supersedes = |target: &ClaimId, by: &ClaimId| ProposedEdge {
        relation: EdgeRelation::Supersedes,
        target: target.clone(),
        by: Some(by.clone()),
        reason: None,
        at: "$.open_items[0].supersedes[0]".to_owned(),
    };
    let withdraws = |target: &ClaimId| ProposedEdge {
        relation: EdgeRelation::Withdraws,
        target: target.clone(),
        by: None,
        reason: Some("no longer holds".to_owned()),
        at: "$.withdrawals[0]".to_owned(),
    };
    for by in [&new_open, &new_next, &new_decision] {
        let refusal = check_edges(&view, &new_claims, &[supersedes(&f1, by)]).unwrap_err();
        assert_eq!(refusal.rule, EdgeRule::FactByNonFact, "{by:?}");
        assert_eq!(
            refusal.detail(),
            "$.open_items[0].supersedes[0] names a fact, which only a verified_fact statement \
             can supersede; record the new fact, or withdraw this one"
        );
    }
    for edges in [
        vec![supersedes(&f1, &new_fact)],
        vec![withdraws(&f1)],
        vec![supersedes(&o1, &new_open)],
        vec![supersedes(&o1, &new_decision)],
        vec![supersedes(&d, &new_open)],
        vec![supersedes(&d, &new_fact)],
    ] {
        assert_eq!(check_edges(&view, &new_claims, &edges), Ok(()), "{edges:?}");
    }

    // Read path: an edge already recorded from a non-fact onto a fact is
    // not applied; the fact stays active and still decides the Task.
    let mut later = view.clone();
    later
        .checkpoints
        .push(world.checkpoint(6, &run, &[("F2", &[0])], &[("O2", &[])], "N2"));
    let c2 = &later.checkpoints[1];
    let f2 = statement_id(c2, StatementSection::VerifiedCompletedWork, 0);
    let o2 = statement_id(c2, StatementSection::OpenItems, 0);
    let edge = |sequence, target: &ClaimId, by: Option<&ClaimId>| ClaimEdge {
        project_sequence: sequence,
        relation: if by.is_some() {
            EdgeRelation::Supersedes
        } else {
            EdgeRelation::Withdraws
        },
        target: target.clone(),
        by: by.cloned(),
        reason: None,
    };
    for (edges, f1_standing) in [
        (vec![edge(7, &f1, Some(&o2))], Standing::Active),
        (vec![edge(7, &f1, Some(&f2))], Standing::Superseded),
        (vec![edge(7, &f1, None)], Standing::Withdrawn),
    ] {
        let mut edged = later.clone();
        edged.edges = edges;
        let (capsule, _) = render(&edged, &world.probe(X, &[(X, Some(&[]))]));
        assert_eq!(claim(&capsule, 1).standing, f1_standing);
    }
}

/// P3-C2 (T12 N4): Evidence of another Task cited by a Decision cannot
/// confirm it: `partial` with `evidence_unavailable`, not damaged.
#[test]
fn p3c2_decision_evidence_of_another_task_is_unavailable() {
    let world = World::new();
    let run = world.run(X, "", &[test("sh t")]);
    let mut view = world.view(vec![world.checkpoint(3, &run, &[("F", &[0])], &[], "N")]);
    let foreign = SealedEvidence {
        evidence_id: EvidenceId::new(),
        sealed: None,
        other_task: true,
    };
    view.decisions.push(DecisionRecord {
        decision_id: EventId::new(),
        project_sequence: 4,
        session_id: world.session_id.clone(),
        payload: DecisionPayload {
            text: "D".to_owned(),
            rationale: None,
            evidence_ids: vec![foreign.evidence_id.clone()],
        },
        evidence: vec![foreign],
        damaged: false,
    });
    view.project_sequence = 4;
    let (capsule, document) = render(&view, &world.probe(X, &[(X, Some(&[]))]));
    let decision = claim(&capsule, 3);
    assert_eq!(decision.kind, ClaimKind::Decision);
    assert_eq!(
        decision.judgement,
        judgement(Partial, &[EvidenceUnavailable])
    );
    assert!(!decision.damaged);
    assert_eq!(document["claims"][2]["evidence"][0]["kind"], Value::Null);
    // The Task is not tampered by it.
    assert_eq!(capsule.judgement(), &Judgement::verified());
}

/// P3-C2 (T12 S-7, M1c): a `verified_fact` recorded before the Evidence
/// rule held in every section (here an open item resting only on a passed
/// command) is `partial` in the Capsule as in the Preview: both judge
/// statements with `judge_statements`.
#[test]
fn p3c2_a_legacy_fact_on_a_passed_command_alone_is_partial_in_both() {
    let world = World::new();
    let run = world.run(
        X,
        "",
        &[Command {
            argv: "sh build",
            test: false,
            exit_code: 0,
        }],
    );
    let legacy = Checkpoint::new(
        CheckpointInput {
            checkpoint_id: CheckpointId::new(),
            task_id: world.task_id.clone(),
            session_id: world.session_id.clone(),
            snapshot_id: run.snapshot.snapshot_id().clone(),
            project_event_sequence: 2,
            objective: "finish the Task".to_owned(),
            verified_completed_work: vec![],
            open_items: vec![
                CheckpointStatement::new(
                    CheckpointClassification::VerifiedFact,
                    "it builds".to_owned(),
                    vec![run.commands[0].clone()],
                )
                .unwrap(),
            ],
            next_action: CheckpointStatement::new(
                CheckpointClassification::Recommendation,
                "N".to_owned(),
                vec![],
            )
            .unwrap(),
            created_at: at("2026-10-03T10:00:02Z"),
        },
        &run.snapshot,
        &run.evidence,
    )
    .unwrap();
    // The write path refuses it today.
    assert!(Checkpoint::create(legacy_input(&legacy), &run.snapshot, &run.evidence).is_err());
    let view = world.view(vec![CheckpointRecord {
        project_sequence: 3,
        checkpoint: legacy.clone(),
        snapshot: run.snapshot.clone(),
        evidence: run.evidence.clone(),
        damaged: false,
    }]);
    let probe = world.probe(X, &[(X, Some(&[]))]);
    let (capsule, _) = render(&view, &probe);
    let fact = claim(&capsule, 1);
    assert_eq!(fact.kind, ClaimKind::Fact);
    // Its Evidence alone is fresh and passed.
    assert_eq!(fact.evidence[0].assessment.judgement, Judgement::verified());
    assert_eq!(fact.judgement, judgement(Partial, &[EvidenceUnavailable]));
    assert_eq!(capsule.judgement().verdict(), Partial);
    let preview = crate::recovery_preview(
        &crate::TaskLedgerView {
            project_sequence: 3,
            ledger_chain_valid: true,
            task: view.task.clone(),
            sessions: vec![],
            sessions_truncated: false,
            checkpoint: crate::CheckpointView::Intact {
                checkpoint: legacy,
                snapshot: run.snapshot.clone(),
                evidence: run.evidence.clone(),
            },
            events: vec![],
            events_truncated: false,
            damaged_records: BTreeSet::new(),
            runs: view.runs.clone(),
        },
        &probe.probe,
    )
    .unwrap();
    let document: Value = serde_json::from_str(preview.as_str()).unwrap();
    assert_eq!(document["status"], "partial");
    assert_eq!(document["facts"][0]["verdict"], "partial");
    assert_eq!(
        document["facts"][0]["reasons"],
        json!(["evidence_unavailable"])
    );
}

fn legacy_input(checkpoint: &Checkpoint) -> CheckpointInput {
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

/// P3-D (ADR 0013 D3): a run is the Snapshot that first seals its
/// Evidence. A later Snapshot that also seals an earlier run's Evidence (a
/// carry-over) is not a later run of that argv, so the Capsule never offers
/// the earlier Evidence back as the run that reaffirms it, even when it
/// reads as `verified` through the later Snapshot.
#[test]
fn a_carried_copy_is_not_a_later_run_for_reaffirmation() {
    let world = World::new();
    let first = world.run(X, "", &[test("cargo test")]);
    // A Snapshot at Z that seals the first run's Evidence and nothing of
    // its own: not a state the store carries into (the manifests differ),
    // so the rule must hold on the view alone.
    let mut carrier = world.run(Z, "", &[]);
    let mut input = carrier.snapshot.to_input();
    input.test_evidence_ids = vec![first.commands[0].clone()];
    carrier
        .evidence
        .extend(first.evidence.iter().skip(2).cloned());
    carrier.snapshot = RepositorySnapshot::new(input, &carrier.evidence).unwrap();
    let mut view = world.view(vec![world.checkpoint(
        3,
        &first,
        &[("F1", &[0])],
        &[],
        "N1",
    )]);
    view.runs.push(RecordedRun {
        project_sequence: 5,
        snapshot: carrier.snapshot.clone(),
        evidence: carrier.evidence.clone(),
    });
    let probe = world.probe(Z, &[(X, Some(&["README"]))]);
    // Through the carrier, the Evidence would be verified at Z.
    let evidence = &first.evidence[2];
    assert_eq!(
        judge_evidence(&carrier.snapshot, &carrier.evidence, evidence, &probe.probe).verdict(),
        Verdict::Verified
    );
    let (capsule, document) = render(&view, &probe);
    assert_eq!(capsule.reruns().len(), 1);
    assert_eq!(capsule.reruns()[0].verified_run, None);
    assert_eq!(document["rerun"][0]["argv"], json!(["cargo", "test"]));
    assert!(!capsule.as_str().contains("\"reaffirm\""));
}

/// P3-D2 (Owner, T13 N2): withdrawing a fact needs a reason; withdrawing
/// any other kind does not. The reason travels in the `claim.superseded@1`
/// payload, an edge recorded before P3-D2 (no `reason`) still decodes, and
/// the Capsule lists the reason on the withdrawn claim and counts the
/// withdrawn facts in its header.
#[test]
#[allow(clippy::too_many_lines)] // The write check, the payload, then the document.
fn p3d2_a_fact_is_withdrawn_only_with_a_reason() {
    let world = World::new();
    let run = world.run(X, "", &[test("sh t")]);
    let mut view = world.view(vec![world.checkpoint(
        3,
        &run,
        &[("F1", &[0]), ("F2", &[0])],
        &[("O1", &[])],
        "N1",
    )]);
    view.project_sequence = 3;
    let c1 = &view.checkpoints[0];
    let f1 = statement_id(c1, StatementSection::VerifiedCompletedWork, 0);
    let f2 = statement_id(c1, StatementSection::VerifiedCompletedWork, 1);
    let o1 = statement_id(c1, StatementSection::OpenItems, 0);
    let withdraws = |target: &ClaimId, reason: Option<&str>| ProposedEdge {
        relation: EdgeRelation::Withdraws,
        target: target.clone(),
        by: None,
        reason: reason.map(str::to_owned),
        at: "$.withdrawals[1]".to_owned(),
    };
    let refusal = check_edges(&view, &[], &[withdraws(&f1, None)]).unwrap_err();
    assert_eq!(refusal.rule, EdgeRule::FactWithdrawnWithoutReason);
    assert_eq!(
        refusal.detail(),
        "$.withdrawals[1] names a fact; withdrawing a fact needs $.withdrawal_reason, which \
         says why it no longer holds"
    );
    assert_eq!(check_edges(&view, &[], &[withdraws(&o1, None)]), Ok(()));
    assert_eq!(
        check_edges(&view, &[], &[withdraws(&f1, Some("flaky test"))]),
        Ok(())
    );

    // The payload: `reason` only on a withdrawal given one.
    let task = &world.task_id;
    let with = ClaimEdge::payload(task, &withdraws(&f1, Some("flaky test")));
    assert_eq!(with["reason"], "flaky test");
    let without = ClaimEdge::payload(task, &withdraws(&o1, None));
    assert_eq!(without.get("reason"), None);
    assert_eq!(
        without,
        json!({ "task_id": task, "relation": "withdraws", "target": o1.wire(), "by": null })
    );
    assert_eq!(
        ClaimEdge::from_payload(5, task, &with).unwrap().reason,
        Some("flaky test".to_owned())
    );
    assert_eq!(
        ClaimEdge::from_payload(5, task, &without).unwrap().reason,
        None
    );
    for invalid in [
        json!({ "task_id": task, "relation": "withdraws", "target": f1.wire(), "by": null, "reason": "" }),
        json!({ "task_id": task, "relation": "withdraws", "target": f1.wire(), "by": null, "reason": 3 }),
        json!({ "task_id": task, "relation": "supersedes", "target": f1.wire(), "by": f2.wire(), "reason": "x" }),
    ] {
        assert_eq!(
            ClaimEdge::from_payload(5, task, &invalid),
            None,
            "{invalid}"
        );
    }

    // The document: two withdrawals of F1 (the first one's reason counts),
    // an old edge without a reason on O1, and F2 still active.
    let edge = |sequence, target: &ClaimId, reason: Option<&str>| ClaimEdge {
        project_sequence: sequence,
        relation: EdgeRelation::Withdraws,
        target: target.clone(),
        by: None,
        reason: reason.map(str::to_owned),
    };
    view.edges = vec![
        edge(4, &f1, Some("flaky test")),
        edge(5, &f1, Some("again")),
        edge(6, &o1, None),
    ];
    view.project_sequence = 6;
    let (capsule, document) = render(&view, &world.probe(X, &[(X, Some(&[]))]));
    let f1_claim = claim(&capsule, 1);
    assert_eq!(f1_claim.standing, Standing::Withdrawn);
    assert_eq!(f1_claim.withdrawal_reason.as_deref(), Some("flaky test"));
    assert_eq!(claim(&capsule, 3).withdrawal_reason, None);
    assert_eq!(claim(&capsule, 2).withdrawal_reason, None);
    assert_eq!(document["claims"][0]["withdrawal_reason"], "flaky test");
    assert_eq!(document["claims"][1]["withdrawal_reason"], Value::Null);
    assert_eq!(document["claims"][2]["withdrawal_reason"], Value::Null);
    let continuation = document["continuation"].as_array().unwrap();
    assert_eq!(continuation[0]["template"], "header");
    assert_eq!(continuation[0]["args"]["withdrawn_facts"], 1);
    let withdrawn_items = continuation
        .iter()
        .filter(|item| item["template"] == "withdrawn")
        .collect::<Vec<_>>();
    assert_eq!(withdrawn_items.len(), 2);
    assert_eq!(
        withdrawn_items[0]["args"]["withdrawal_reason"],
        "flaky test"
    );
    assert_eq!(withdrawn_items[1]["args"]["withdrawal_reason"], Value::Null);
    // Withdrawal changes no verdict; the Task takes the remaining fact.
    assert_eq!(f1_claim.judgement, Judgement::verified());
    assert_eq!(capsule.judgement(), &Judgement::verified());
}

/// ADR 0014 (P4-D/F): the members v4 adds, and the ones whose value moves
/// with the version and the digest.
fn without_v4_members(mut document: Value) -> Value {
    let object = document.as_object_mut().unwrap();
    for member in ["actions", "ontology_version", "capsule_digest"] {
        object.remove(member);
    }
    for session in document["sessions"].as_array_mut().unwrap() {
        session.as_object_mut().unwrap().remove("resumed_from");
    }
    document
}

/// P4-D/F: taking away `actions`, every Session's `resumed_from`, the
/// version, and the digest leaves exactly the members of ontology v3, and
/// the Capsule is still a pure function of the ledger view and the probe.
#[test]
fn v4_adds_only_actions_and_resumed_from() {
    let v3_top = [
        "schema_version",
        "task",
        "status",
        "reasons",
        "bindings",
        "sessions",
        "claims",
        "rerun",
        "changed_paths_by_base",
        "gaps",
        "damaged_records",
        "continuation",
        "omitted",
    ];
    let v3_session = [
        "session_id",
        "workspace_id",
        "started_at",
        "ended_at",
        "end_state",
        "last_recorded_at",
    ];
    for seed in 0..60 {
        let (_, mut view, probe) = generated(seed);
        let first = task_capsule(&view, &probe).unwrap();
        assert_eq!(
            task_capsule(&view, &probe).unwrap().as_str(),
            first.as_str()
        );
        let document: Value = serde_json::from_str(first.as_str()).unwrap();
        assert_eq!(
            document["ontology_version"],
            "continuity.work-state-ontology.v4"
        );
        assert_eq!(document["actions"].as_array().unwrap().len(), 3);
        let stripped = without_v4_members(document);
        assert_eq!(
            stripped
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>(),
            v3_top
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<BTreeSet<_>>()
                .iter()
                .collect()
        );
        for session in stripped["sessions"].as_array().unwrap() {
            assert_eq!(
                session.as_object().unwrap().keys().collect::<BTreeSet<_>>(),
                v3_session
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<BTreeSet<_>>()
                    .iter()
                    .collect()
            );
        }
        // A declared Pack changes the Session's member only.
        if let Some(record) = view.sessions.first_mut() {
            record.resumed_from = Some(crate::ResumedFrom {
                capsule_digest: crate::Sha256Digest::of_bytes(b"capsule"),
                project_sequence: 3,
                pack_digest: crate::Sha256Digest::of_bytes(b"pack"),
                budget: 9000,
            });
            let declared: Value =
                serde_json::from_str(task_capsule(&view, &probe).unwrap().as_str()).unwrap();
            assert_eq!(declared["sessions"][0]["resumed_from"]["declared"], true);
            assert_eq!(without_v4_members(declared), stripped, "seed {seed}");
        }
    }
}

/// Up to three Sessions of the Task, each open, ended, or ended abandoned
/// (`session.crashed`), its row agreeing with the ledger.
fn random_sessions(world: &World, random: &mut Random) -> Vec<SessionRecord> {
    (0..random.below(4))
        .map(|minute| {
            let end = [SessionEnd::Open, SessionEnd::Ended, SessionEnd::Crashed]
                [usize::try_from(random.below(3)).unwrap()];
            let started = at(&format!("2026-10-03T11:{minute:02}:00Z"));
            SessionRecord {
                session: Session {
                    session_id: SessionId::new(),
                    workspace_id: world.workspace_id.clone(),
                    agent_id: crate::AgentId::new(),
                    started_at: started.clone(),
                    ended_at: (end != SessionEnd::Open).then_some(started),
                    revision: 1 + u64::from(end != SessionEnd::Open),
                    agent: None,
                    ended_by: None,
                },
                end,
                last_recorded_at: None,
                resumed_from: None,
            }
        })
        .collect()
}

/// P4-H1 (Owner, T20 N-1): `cite_latest_run` over generated ledgers in
/// every Task state, with and without a Session that has not ended. The
/// Evidence part of its reasons does not depend on the Task's state or its
/// Sessions; `invalid_transition` is added, last (the write appends its
/// Event, which an ended Session refuses, after every `400` check), exactly
/// when the Task takes no new Session and none of its Sessions is open.
#[test]
fn property_cite_latest_run_needs_a_session_that_can_write() {
    use crate::{ActionReason, TaskStatus, task_actions};
    const STATES: [TaskStatus; 6] = [
        TaskStatus::Proposed,
        TaskStatus::Ready,
        TaskStatus::InProgress,
        TaskStatus::Blocked,
        TaskStatus::Completed,
        TaskStatus::Cancelled,
    ];
    let mut seen = BTreeSet::new();
    for seed in 0..240 {
        let (world, mut view, probe) = generated(seed);
        let mut random = Random(seed.wrapping_mul(0x51) ^ 0x0a11);
        let history = run_history(&view.runs);
        let base = task_actions(&view, &history, &probe.probe).remove(2);
        view.task.status = STATES[usize::try_from(random.below(6)).unwrap()];
        view.sessions = random_sessions(&world, &mut random);
        let cite = task_actions(&view, &history, &probe.probe).remove(2);
        assert_eq!(cite.args, base.args, "seed {seed}");
        let open = view
            .sessions
            .iter()
            .any(|record| record.end == SessionEnd::Open && record.session.is_active());
        let finished = !view.task.status.takes_sessions();
        let blocked = finished && !open;
        let mut expected = base.reasons.clone();
        if blocked {
            expected.push(ActionReason::InvalidTransition);
        }
        assert_eq!(
            cite.reasons,
            expected,
            "seed {seed}: {} with open Session {open}",
            view.task.status.as_str()
        );
        seen.insert((finished, open, cite.available()));
    }
    // A finished Task is seen with and without an open Session, citable
    // only with one; an unfinished Task is citable without one.
    for case in [
        (true, true, true),
        (true, true, false),
        (true, false, false),
        (false, false, true),
        (false, true, true),
    ] {
        assert!(seen.contains(&case), "{case:?}: {seen:?}");
    }
    assert!(!seen.contains(&(true, false, true)), "{seen:?}");
}

/// ADR 0014 E3 over generated ledgers: `complete_task` cites exactly the
/// passed runs that are `verified` now and is available exactly when
/// `completion_blocks` finds nothing in them; a `tampered` Task never
/// completes (condition d); `begin_session` follows the Task's state.
#[test]
fn property_actions_follow_the_write_checks() {
    use crate::{ActionKind, TaskStatus, completion_blocks, task_actions};
    let mut both = BTreeSet::new();
    for seed in 0..200 {
        let (_, mut view, probe) = generated(seed);
        if seed % 3 == 0 {
            view.task.status = TaskStatus::Proposed;
        }
        if seed % 7 == 0 {
            view.ledger_chain_valid = false;
        }
        let history = run_history(&view.runs);
        let actions = task_actions(&view, &history, &probe.probe);
        assert_eq!(
            actions.iter().map(|action| action.kind).collect::<Vec<_>>(),
            ActionKind::ALL
        );
        let complete = &actions[0];
        let cited = complete.args["completion_evidence_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>();
        let mut evidence = Vec::new();
        for run in &view.runs {
            for item in &run.evidence {
                if cited.contains(item.evidence_id().as_str())
                    && !evidence.iter().any(|seen: &crate::CompletionEvidence| {
                        seen.evidence.evidence_id() == item.evidence_id()
                    })
                {
                    assert_eq!(item.payload().run_status(), Some(crate::RunStatus::Pass));
                    evidence.push(crate::CompletionEvidence {
                        evidence: item.clone(),
                        snapshot: Some(run.snapshot.clone()),
                        snapshot_evidence: run.evidence.clone(),
                    });
                }
            }
        }
        let tampered = !view.ledger_chain_valid || !view.damaged_records.is_empty();
        let blocks = completion_blocks(
            &view.task.task_id,
            view.task.status,
            tampered,
            &[],
            &evidence,
            &history,
            &probe.probe,
        );
        assert_eq!(complete.available(), blocks.is_empty(), "seed {seed}");
        if tampered || view.task.status != TaskStatus::InProgress {
            assert!(!complete.available(), "seed {seed}");
        }
        assert_eq!(
            actions[1].available(),
            view.task.status.takes_sessions(),
            "seed {seed}"
        );
        both.insert(complete.available());
    }
    assert_eq!(both, BTreeSet::from([false, true]));
}

/// U8 S-F round 1, B2 (ADR 0013, "Amendment (U8, S-A)", A6): a withdrawn
/// claim's `withdrawn_by` is the Checkpoint that wrote its `withdraws`
/// edge: `createCheckpoint` appends its `checkpoint.created` Event and then
/// its edges, one Event each, in one write transaction, so that Checkpoint
/// is the one whose `checkpoint.created` comes right before the unbroken
/// run of edges that holds the edge. When that Checkpoint is not in the
/// view (the store leaves out one that cannot be read), or the run is
/// broken, it is `None` (`null` in the document), never an earlier readable
/// Checkpoint.
#[test]
fn u8_f1_b2_withdrawn_by_is_the_checkpoint_that_wrote_the_edge_or_none() {
    let world = World::new();
    let run = world.run(X, "", &[test("cargo test")]);
    let first = world.checkpoint(3, &run, &[("F1", &[0])], &[("O1", &[])], "N1");
    let second = world.checkpoint(5, &run, &[], &[], "N2");
    let f1 = statement_id(&first, StatementSection::VerifiedCompletedWork, 0);
    let o1 = statement_id(&first, StatementSection::OpenItems, 0);
    let withdraws = |sequence, target: &ClaimId| ClaimEdge {
        project_sequence: sequence,
        relation: EdgeRelation::Withdraws,
        target: target.clone(),
        by: None,
        reason: Some("owner veto".to_owned()),
    };
    let probe = world.probe(X, &[(X, Some(&[]))]);
    let withdrawn_by = |checkpoints: Vec<CheckpointRecord>, edges: Vec<ClaimEdge>| {
        let mut view = world.view(checkpoints);
        view.edges = edges;
        view.project_sequence = 8;
        let (capsule, document) = render(&view, &probe);
        [&f1, &o1]
            .into_iter()
            .map(|id| {
                let claim = capsule
                    .claims()
                    .iter()
                    .find(|claim| claim.id == *id)
                    .unwrap();
                assert_eq!(claim.standing, Standing::Withdrawn);
                let item = document["claims"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["number"] == claim.number)
                    .unwrap();
                assert_eq!(
                    item["withdrawn_by"],
                    claim.withdrawn_by.as_ref().map_or(Value::Null, |(c, s)| {
                        json!({"checkpoint_id": c, "session_id": s})
                    })
                );
                claim.withdrawn_by.clone()
            })
            .collect::<Vec<_>>()
    };
    let by_second = Some((
        second.checkpoint.checkpoint_id().clone(),
        second.checkpoint.session_id().clone(),
    ));
    let edges = vec![withdraws(6, &o1), withdraws(7, &f1)];

    // The second Checkpoint wrote both edges, right after its Event.
    assert_eq!(
        withdrawn_by(vec![first.clone(), second.clone()], edges.clone()),
        [by_second.clone(), by_second]
    );
    // It cannot be read, so the store left it out: no withdrawer, not the
    // first Checkpoint.
    assert_eq!(withdrawn_by(vec![first.clone()], edges), [None, None]);
    // Something that is not an edge (here, Project sequence 6) lies between
    // the second Checkpoint's Event and the edges: no withdrawer either.
    assert_eq!(
        withdrawn_by(
            vec![first, second],
            vec![withdraws(7, &o1), withdraws(8, &f1)]
        ),
        [None, None]
    );
}
