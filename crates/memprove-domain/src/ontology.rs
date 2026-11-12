//! Minimal work-state ontology and freshness propagation (ADR 0012 D1–D6,
//! rules P1–P6).
//!
//! Every function here is pure: verdicts are computed at read time from the
//! recorded Checkpoint, its Snapshot and Evidence, and one probe observation
//! of a Workspace (D3). Nothing reads a clock, a database, Git, or the
//! filesystem, and nothing is persisted.
//!
//! The propagation runs in one direction over a fixed path of five hops
//! ([`PROPAGATION_PATH`], P6): probe → Snapshot → Evidence → statement →
//! Checkpoint → Task. Verdicts use the `CapsuleValidation` vocabulary (D4)
//! with precedence `tampered > partial > stale > verified`; `partial` means
//! "cannot confirm".
//!
//! Only `verified_fact` statements are claims that propagate to the
//! Checkpoint (P5). Other statements (`open_items`, `next_action`,
//! recommendations, user context) are reported with their own verdict, which
//! for a statement without Evidence is always `partial` (P4), but they do not
//! make the Checkpoint unconfirmable.
//!
//! P2-E: `verified` needs both fresh Evidence and Evidence that passed. A
//! fresh command or test whose FR-203 status is not `PASS` is `partial` with
//! `evidence_not_passed` (P2-F; [`judge_evidence`], shared with Task
//! completion), and a `verified_fact` whose Evidence does not satisfy the
//! Evidence rule of `verified_completed_work` is at best `partial` too, in
//! whatever section it was recorded.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Checkpoint, CheckpointClassification, CheckpointId, CheckpointStatement, Evidence, EvidenceId,
    EvidenceKind, EvidencePayload, EvidenceSubject, RepositorySnapshot, RunStatus, Sha256Digest,
    TaskId, TestFormat, WorkspaceId, repository_capture_manifest_digest,
    verified_fact_evidence_holds,
};

/// Version of the ontology and its propagation rules, bound into every
/// Recovery Preview and Task Capsule. P2-F adds the reason code
/// `evidence_not_passed`, an enumerated value, so the version moved to v2
/// (ADR 0012, "P2-F amendments"). ADR 0013 adds the Task's claim graph
/// (claim kinds, standing, the supersedes and withdraws relations, Decision,
/// the Task-local number) and the gap `base_commits_over_limit`, so it moves
/// to v3. The four verdicts and the eleven reason codes are unchanged.
/// ADR 0014 (P4-C2, P4-D, P4-F) adds the record type `session`, the Task
/// Capsule's `actions`, a Session's `resumed_from`, and the Event
/// `capsule.recovered@1`, so it moves to v4; the Recovery Preview carries
/// the same version with its structure unchanged.
pub const ONTOLOGY_VERSION: &str = "continuity.work-state-ontology.v4";

/// Freshness verdict. The derived order is the precedence: a larger verdict
/// wins when verdicts combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verdict {
    /// Every observed dependency matches the record.
    Verified,
    /// An observed dependency differs from the record.
    Stale,
    /// The record cannot be confirmed.
    Partial,
    /// A digest, chain, or repository identity does not hold.
    Tampered,
}

impl Verdict {
    /// Every verdict, lowest precedence first.
    pub const ALL: [Self; 4] = [Self::Verified, Self::Stale, Self::Partial, Self::Tampered];

    /// The `CapsuleValidation.status` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Stale => "stale",
            Self::Partial => "partial",
            Self::Tampered => "tampered",
        }
    }
}

/// Closed reason codes of `CapsuleValidation.reasons`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReasonCode {
    /// HEAD or the branch differs.
    HeadChanged,
    /// The index differs.
    IndexChanged,
    /// The worktree status, tracked content, or a selected file differs.
    WorktreeChanged,
    /// A referenced record is missing.
    ObjectMissing,
    /// A recorded digest does not recompute.
    ObjectDigestMismatch,
    /// The Event chain does not verify.
    LedgerChainInvalid,
    /// A statement rests on stale Evidence.
    EvidenceStale,
    /// What is needed to confirm the record cannot be observed.
    EvidenceUnavailable,
    /// P2-F: a command or test Evidence object is still fresh, but its FR-203
    /// status is not `PASS`.
    EvidenceNotPassed,
    /// The probed repository is not the Project's repository.
    ProjectMismatch,
    /// The probed worktree is not the Workspace's worktree.
    WorkspaceMismatch,
}

impl ReasonCode {
    /// Every reason code, in contract order.
    pub const ALL: [Self; 11] = [
        Self::HeadChanged,
        Self::IndexChanged,
        Self::WorktreeChanged,
        Self::ObjectMissing,
        Self::ObjectDigestMismatch,
        Self::LedgerChainInvalid,
        Self::EvidenceStale,
        Self::EvidenceUnavailable,
        Self::EvidenceNotPassed,
        Self::ProjectMismatch,
        Self::WorkspaceMismatch,
    ];

    /// The contract spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HeadChanged => "head_changed",
            Self::IndexChanged => "index_changed",
            Self::WorktreeChanged => "worktree_changed",
            Self::ObjectMissing => "object_missing",
            Self::ObjectDigestMismatch => "object_digest_mismatch",
            Self::LedgerChainInvalid => "ledger_chain_invalid",
            Self::EvidenceStale => "evidence_stale",
            Self::EvidenceUnavailable => "evidence_unavailable",
            Self::EvidenceNotPassed => "evidence_not_passed",
            Self::ProjectMismatch => "project_mismatch",
            Self::WorkspaceMismatch => "workspace_mismatch",
        }
    }
}

/// A verdict and the reasons behind it. A `verified` judgement carries no
/// reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    verdict: Verdict,
    reasons: BTreeSet<ReasonCode>,
}

impl Judgement {
    /// Nothing differs.
    #[must_use]
    pub const fn verified() -> Self {
        Self {
            verdict: Verdict::Verified,
            reasons: BTreeSet::new(),
        }
    }

    /// `verdict` for `reasons`; a `verified` verdict drops them.
    #[must_use]
    pub fn new(verdict: Verdict, reasons: impl IntoIterator<Item = ReasonCode>) -> Self {
        Self {
            verdict,
            reasons: if verdict == Verdict::Verified {
                BTreeSet::new()
            } else {
                reasons.into_iter().collect()
            },
        }
    }

    /// The verdict.
    #[must_use]
    pub const fn verdict(&self) -> Verdict {
        self.verdict
    }

    /// The reasons, in contract order.
    #[must_use]
    pub const fn reasons(&self) -> &BTreeSet<ReasonCode> {
        &self.reasons
    }

    /// The worse verdict, with the reasons of both.
    #[must_use]
    pub fn combine(&self, other: &Self) -> Self {
        Self {
            verdict: self.verdict.max(other.verdict),
            reasons: self.reasons.union(&other.reasons).copied().collect(),
        }
    }

    /// At best `ceiling`: the worse of this verdict and `ceiling`, adding
    /// `reason` (P3).
    #[must_use]
    pub fn at_best(&self, ceiling: Verdict, reason: ReasonCode) -> Self {
        self.combine(&Self::new(ceiling, [reason]))
    }

    /// The worst of `judgements`, or `None` when there are none.
    pub fn worst<'a>(judgements: impl IntoIterator<Item = &'a Self>) -> Option<Self> {
        judgements
            .into_iter()
            .fold(None, |worst: Option<Self>, item| {
                Some(worst.map_or_else(|| item.clone(), |worst| worst.combine(item)))
            })
    }
}

/// The reasons that make a verdict `tampered`: a record, the chain, or the
/// repository identity does not hold.
const INTEGRITY_REASONS: [ReasonCode; 5] = [
    ReasonCode::ObjectMissing,
    ReasonCode::ObjectDigestMismatch,
    ReasonCode::LedgerChainInvalid,
    ReasonCode::ProjectMismatch,
    ReasonCode::WorkspaceMismatch,
];

/// P3-C2 (Owner, T12 S-6): for a `tampered` document, the judgement every
/// verdict inside it takes on top of its own, so that no level of a
/// `tampered` Recovery Preview or Task Capsule reads `verified`: `tampered`
/// with the document's integrity reasons. `None` for any other verdict.
#[must_use]
pub(crate) fn integrity_judgement(document: &Judgement) -> Option<Judgement> {
    if document.verdict() != Verdict::Tampered {
        return None;
    }
    let integrity = document
        .reasons()
        .iter()
        .copied()
        .filter(|reason| INTEGRITY_REASONS.contains(reason))
        .collect::<BTreeSet<_>>();
    Some(Judgement::new(
        Verdict::Tampered,
        if integrity.is_empty() {
            document.reasons().clone()
        } else {
            integrity
        },
    ))
}

/// The section of a Checkpoint that holds a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatementSection {
    /// `verified_completed_work`.
    VerifiedCompletedWork,
    /// `open_items`.
    OpenItems,
    /// `next_action`.
    NextAction,
}

impl StatementSection {
    /// Every section, in the order of a Checkpoint (U3 G1).
    pub const ALL: [Self; 3] = [
        Self::VerifiedCompletedWork,
        Self::OpenItems,
        Self::NextAction,
    ];

    /// The stored and wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedCompletedWork => "verified_completed_work",
            Self::OpenItems => "open_items",
            Self::NextAction => "next_action",
        }
    }
}

/// Identity of one statement (D2): `(checkpoint_id, section, ordinal)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimRef {
    /// Checkpoint holding the statement.
    pub checkpoint_id: CheckpointId,
    /// Section of the statement.
    pub section: StatementSection,
    /// Zero-based position within the section.
    pub ordinal: usize,
}

/// One read-only observation of a Workspace's repository (P1). The probing
/// adapter compares the resolved paths with the registered records; the
/// rest is what Git and the filesystem reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryObservation {
    /// The resolved common directory is the Project's repository.
    pub same_repository: bool,
    /// The resolved worktree root and Git directory are the Workspace's.
    pub same_workspace: bool,
    /// Identity of the probed worktree, as in `git_head` Evidence subjects.
    pub repository_identity: Sha256Digest,
    /// Current HEAD.
    pub head_oid: String,
    /// Current branch, `None` when detached.
    pub branch: Option<String>,
    /// `git status --porcelain=v2 --untracked-files=all`.
    pub status_porcelain_v2: String,
    /// Digest of the index.
    pub index_digest: Sha256Digest,
    /// Digest of the tracked worktree content.
    pub tracked_worktree_digest: Sha256Digest,
    /// The probe's own capture manifest digest.
    pub capture_manifest_digest: Sha256Digest,
    /// Content digest of each selected `repo://` file of the Snapshot, `None`
    /// when it is no longer a readable regular file.
    pub selected_files: BTreeMap<String, Option<Sha256Digest>>,
    /// Untracked, not ignored paths, relative to the worktree root.
    pub untracked_paths: BTreeSet<String>,
    /// Paths `git diff --name-only` reports between the Snapshot's commit and
    /// the worktree, or `None` when that commit is unavailable (D5).
    pub changed_since_base: Option<BTreeSet<String>>,
}

/// What the probe saw.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // One observation per Preview.
pub enum ProbeObservation {
    /// The probe could not run: no Workspace to probe, its directory is gone,
    /// or Git failed.
    Unavailable,
    /// The probe ran.
    Observed(RepositoryObservation),
}

/// The probe P of one Recovery Preview: the Workspace it ran in and what it
/// saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The Workspace probed, when one could be chosen.
    pub workspace_id: Option<WorkspaceId>,
    /// The observation.
    pub observation: ProbeObservation,
}

/// A node of the propagation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Node {
    /// The probe P.
    Probe,
    /// The anchoring `RepositorySnapshot`.
    Snapshot,
    /// One Evidence object of the Snapshot.
    Evidence,
    /// One `CheckpointStatement`.
    Statement,
    /// The Checkpoint.
    Checkpoint,
    /// The Task.
    Task,
}

impl Node {
    /// The spelling of the ontology manifest (U3 G1).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Snapshot => "snapshot",
            Self::Evidence => "evidence",
            Self::Statement => "statement",
            Self::Checkpoint => "checkpoint",
            Self::Task => "task",
        }
    }
}

/// One directed propagation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hop {
    /// Where a verdict comes from.
    pub from: Node,
    /// Where it goes.
    pub to: Node,
    /// The rule applied.
    pub rule: &'static str,
}

/// The fixed, acyclic propagation path (P6). Verdicts only flow forward.
pub const PROPAGATION_PATH: [Hop; 5] = [
    Hop {
        from: Node::Probe,
        to: Node::Snapshot,
        rule: "P1",
    },
    Hop {
        from: Node::Snapshot,
        to: Node::Evidence,
        rule: "P2/P3",
    },
    Hop {
        from: Node::Evidence,
        to: Node::Statement,
        rule: "P4",
    },
    Hop {
        from: Node::Statement,
        to: Node::Checkpoint,
        rule: "P5",
    },
    Hop {
        from: Node::Checkpoint,
        to: Node::Task,
        rule: "P5",
    },
];

/// The repository identity a Snapshot was captured in: its `git_head`
/// Evidence subject.
#[must_use]
pub fn snapshot_repository_identity<'a>(
    snapshot: &RepositorySnapshot,
    evidence: &'a [Evidence],
) -> Option<&'a Sha256Digest> {
    evidence
        .iter()
        .find(|item| item.evidence_id() == snapshot.git_head_evidence_id())
        .and_then(|item| match item.subject() {
            EvidenceSubject::GitHead {
                repository_identity,
            } => Some(repository_identity),
            _ => None,
        })
}

/// P1: what the probe alone establishes for a Snapshot. `Err` carries the
/// judgement every downstream node inherits: `partial` when the probe could
/// not run, `tampered` when the probed repository is not the Snapshot's.
///
/// # Errors
///
/// The inherited judgement when the probe does not support comparison.
pub fn judge_source<'a>(
    snapshot: &RepositorySnapshot,
    snapshot_identity: Option<&Sha256Digest>,
    probe: &'a Probe,
) -> Result<&'a RepositoryObservation, Judgement> {
    let ProbeObservation::Observed(observation) = &probe.observation else {
        return Err(Judgement::new(
            Verdict::Partial,
            [ReasonCode::EvidenceUnavailable],
        ));
    };
    let Some(snapshot_identity) = snapshot_identity else {
        return Err(Judgement::new(
            Verdict::Tampered,
            [ReasonCode::ObjectDigestMismatch],
        ));
    };
    if !observation.same_repository {
        return Err(Judgement::new(
            Verdict::Tampered,
            [ReasonCode::ProjectMismatch],
        ));
    }
    let own_workspace = probe.workspace_id.as_ref() == Some(snapshot.workspace_id());
    if !observation.same_workspace
        || (own_workspace && observation.repository_identity != *snapshot_identity)
    {
        return Err(Judgement::new(
            Verdict::Tampered,
            [ReasonCode::WorkspaceMismatch],
        ));
    }
    Ok(observation)
}

/// P2 and P3: the verdict of one Evidence object of `snapshot` by its
/// dependency surface, and (P2-E) by its outcome.
///
/// - `git_head`: HEAD and branch (`head_changed`).
/// - `git_worktree`: the porcelain status (`worktree_changed`).
/// - `file_digest`: that file's digest (`worktree_changed`).
/// - `command_result`/`test_result`: the whole capture manifest, with the
///   Workspace location and the branch name excluded so a linked worktree
///   on another branch can be compared (`head_changed`, `index_changed`,
///   `worktree_changed`); with untracked files the Snapshot did not select,
///   at best `partial`. The same commit, index, and worktree as at capture
///   are `verified` on any branch and in any worktree (Owner decisions, P2-C
///   and P2-E: the worktree need not be clean, only unchanged).
/// - P2-E: fresh and passed are both required for `verified`; the Recovery
///   Preview and Task completion both apply this function. P2-F: a command
///   or test that is still fresh but whose FR-203 status is not `PASS`
///   (`FAIL`, `NOT_RUN`, `UNAVAILABLE`) is `partial` with
///   `evidence_not_passed`. One that is not fresh keeps its freshness
///   verdict: `stale` with what changed, or `partial` with
///   `evidence_unavailable` when its capture cannot be confirmed (Owner
///   decision, P2-F).
/// - P3-D3 (Owner, T14 S-1): a passed run that `history` overrules (the
///   latest run of its command in its state did not pass) is judged as a
///   run that did not pass: `partial` with `evidence_not_passed` where it
///   would be `verified`. The Recovery Preview, the Task Capsule, and Task
///   completion all pass the Task's [`RunHistory`].
///
/// Commits are compared, not trees: a false `stale` is preferred to a false
/// `verified`.
#[must_use]
pub fn judge_evidence(
    snapshot: &RepositorySnapshot,
    snapshot_evidence: &[Evidence],
    evidence: &Evidence,
    history: &RunHistory,
    probe: &Probe,
) -> Judgement {
    if !snapshot.contains_evidence(evidence.evidence_id()) {
        return Judgement::new(Verdict::Tampered, [ReasonCode::ObjectMissing]);
    }
    let identity = snapshot_repository_identity(snapshot, snapshot_evidence);
    let observation = match judge_source(snapshot, identity, probe) {
        Ok(observation) => observation,
        Err(inherited) => return inherited,
    };
    let differs = |reason: ReasonCode, changed: bool| {
        if changed {
            Judgement::new(Verdict::Stale, [reason])
        } else {
            Judgement::verified()
        }
    };
    match (evidence.subject(), evidence.payload()) {
        (_, EvidencePayload::GitHead { head_oid, branch }) => differs(
            ReasonCode::HeadChanged,
            *head_oid != observation.head_oid || *branch != observation.branch,
        ),
        (
            _,
            EvidencePayload::GitWorktree {
                status_porcelain_v2,
            },
        ) => differs(
            ReasonCode::WorktreeChanged,
            *status_porcelain_v2 != observation.status_porcelain_v2,
        ),
        (EvidenceSubject::FileDigest { path }, EvidencePayload::FileDigest { sha256 }) => differs(
            ReasonCode::WorktreeChanged,
            observation.selected_files.get(path) != Some(&Some(sha256.clone())),
        ),
        (_, EvidencePayload::CommandResult { .. } | EvidencePayload::TestResult { .. }) => {
            let manifest = manifest_judgement(
                snapshot,
                snapshot_evidence,
                identity.unwrap_or(&observation.repository_identity),
                observation,
            );
            let captured = if unselected_untracked(snapshot, snapshot_evidence).is_empty() {
                manifest
            } else {
                manifest.at_best(Verdict::Partial, ReasonCode::EvidenceUnavailable)
            };
            let passed = evidence.payload().run_status() == Some(RunStatus::Pass)
                && history.overruled_by(evidence.evidence_id()).is_none();
            if passed || captured.verdict() != Verdict::Verified {
                captured
            } else {
                Judgement::new(Verdict::Partial, [ReasonCode::EvidenceNotPassed])
            }
        }
        (_, EvidencePayload::FileDigest { .. }) => {
            Judgement::new(Verdict::Tampered, [ReasonCode::ObjectDigestMismatch])
        }
    }
}

/// The selected `repo://` files of a Snapshot and their recorded digests.
fn selected_files<'a>(
    snapshot: &RepositorySnapshot,
    evidence: &'a [Evidence],
) -> Vec<(&'a str, &'a Sha256Digest)> {
    evidence
        .iter()
        .filter(|item| snapshot.file_evidence_ids().contains(item.evidence_id()))
        .filter_map(|item| match (item.subject(), item.payload()) {
            (EvidenceSubject::FileDigest { path }, EvidencePayload::FileDigest { sha256 }) => {
                Some((path.as_str(), sha256))
            }
            _ => None,
        })
        .collect()
}

/// Compares the Snapshot's capture manifest with the one the observation
/// yields under the Snapshot's own repository identity and branch: the
/// branch name is not part of what a command or test depends on.
fn manifest_judgement(
    snapshot: &RepositorySnapshot,
    snapshot_evidence: &[Evidence],
    snapshot_identity: &Sha256Digest,
    observation: &RepositoryObservation,
) -> Judgement {
    let recorded = selected_files(snapshot, snapshot_evidence);
    let mut files_changed = false;
    let observed = recorded
        .iter()
        .filter_map(|(path, digest)| {
            let current = observation
                .selected_files
                .get(*path)
                .and_then(Option::as_ref);
            files_changed |= current != Some(*digest);
            current.map(|current| (*path, current))
        })
        .collect::<Vec<_>>();
    let digest = repository_capture_manifest_digest(
        snapshot_identity,
        &observation.head_oid,
        snapshot.branch(),
        &observation.status_porcelain_v2,
        &observation.index_digest,
        &observation.tracked_worktree_digest,
        &observed,
    );
    if digest == *snapshot.capture_manifest_digest() && !files_changed {
        return Judgement::verified();
    }
    let head_changed = snapshot.head_oid() != observation.head_oid;
    let index_changed = snapshot.index_digest() != &observation.index_digest;
    // The tracked-content digest also covers the index's object IDs, so a
    // change explained by the index alone is not a worktree change.
    let worktree_changed = snapshot.worktree_status_porcelain_v2()
        != observation.status_porcelain_v2
        || files_changed
        || (!index_changed
            && snapshot.tracked_worktree_digest() != &observation.tracked_worktree_digest);
    let mut reasons = BTreeSet::new();
    if head_changed {
        reasons.insert(ReasonCode::HeadChanged);
    }
    if index_changed {
        reasons.insert(ReasonCode::IndexChanged);
    }
    if worktree_changed || reasons.is_empty() {
        reasons.insert(ReasonCode::WorktreeChanged);
    }
    Judgement::new(Verdict::Stale, reasons)
}

/// Untracked paths in the Snapshot's status that no selected file covers
/// (P3): their content was never captured.
#[must_use]
pub fn unselected_untracked(
    snapshot: &RepositorySnapshot,
    snapshot_evidence: &[Evidence],
) -> BTreeSet<String> {
    let selected = selected_files(snapshot, snapshot_evidence)
        .into_iter()
        .filter_map(|(path, _)| path.strip_prefix("repo://"))
        .collect::<BTreeSet<_>>();
    porcelain_v2_paths(snapshot.worktree_status_porcelain_v2())
        .untracked
        .into_iter()
        .filter(|path| !selected.contains(path.as_str()))
        .collect()
}

/// ADR 0013 D3 (P3-D): whether the command and test Evidence of `earlier`
/// may be carried over into `later`, so that one Checkpoint over `later` can
/// cite runs of one repository state recorded at different times. Both are
/// Snapshots of the same Task and Workspace with equal
/// `capture_manifest_digest` (the repository identity, HEAD, branch,
/// porcelain status, index, tracked content, and selected files), and
/// (Owner decision, P3-D) neither status lists an untracked file.
///
/// Under this condition [`judge_evidence`] gives an Evidence object of
/// `earlier` the same judgement through `later` as through `earlier`: every
/// input it reads from the Snapshot is covered by the manifest, and no
/// untracked file is left unselected.
#[must_use]
pub fn carries_over(earlier: &RepositorySnapshot, later: &RepositorySnapshot) -> bool {
    earlier.task_id() == later.task_id()
        && earlier.workspace_id() == later.workspace_id()
        && earlier.capture_manifest_digest() == later.capture_manifest_digest()
        && !lists_untracked(earlier)
        && !lists_untracked(later)
}

/// Whether a Snapshot's porcelain status lists an untracked file.
fn lists_untracked(snapshot: &RepositorySnapshot) -> bool {
    !porcelain_v2_paths(snapshot.worktree_status_porcelain_v2())
        .untracked
        .is_empty()
}

/// P3-D3 (Owner, T14 S-1): the later runs of a Task that take away what an
/// earlier passed run says. A run is a command or test Evidence object with
/// the first Snapshot, in ledger order, that seals it. Runs of the same Task
/// and Workspace with equal `capture_manifest_digest` and the same argv
/// (element by element) are runs of one command in one repository state;
/// when the latest of them did not pass, none of its passed runs can
/// support `verified`: [`judge_evidence`] gives such a run `partial` with
/// `evidence_not_passed` where it would be `verified`. A later passed run
/// restores the earlier ones. The rule reads the ledger only, so it holds
/// whether or not a run was carried over (ADR 0013 D3), and it only ever
/// takes `verified` away.
///
/// The manifest does not observe ignored files, permission bits other than
/// the executable bit, or the environment, so two runs of one manifest can
/// differ in what the command read; the latest run is the one the state
/// answers with.
///
/// P3-L L2 (Owner O2, T15 N1-R7): a passed test run is also overruled when
/// the latest test run of its group did not pass, whatever command runs of
/// the same argv came after it. This only tightens: command runs are judged
/// as before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunHistory {
    /// Each overruled passed run, with the latest run of its command that
    /// did not pass.
    overruled: BTreeMap<EvidenceId, EvidenceId>,
    /// P3-L3 (Owner, T16 S-1 item 2): each passed test run the second rule
    /// (P3-L L2) overrules, with the latest test run of its group, which
    /// did not pass.
    test_overruled: BTreeMap<EvidenceId, EvidenceId>,
}

/// Where one run belongs: Task, Workspace, manifest, and argv.
type RunGroup = (TaskId, WorkspaceId, Sha256Digest, Vec<String>);

impl RunHistory {
    /// The history of `runs`: every readable Snapshot of the Task with its
    /// ledger position and Evidence, in any order. Each command or test
    /// Evidence object is placed at the earliest position of a Snapshot that
    /// seals it, in that Snapshot's group. Runs at the same position are not
    /// ordered: when the latest position of a group holds a run that did not
    /// pass, every passed run of the group is overruled, the ones at that
    /// position included.
    pub fn of<'a>(
        runs: impl IntoIterator<Item = (u64, &'a RepositorySnapshot, &'a [Evidence])>,
    ) -> Self {
        let mut ordered = runs.into_iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            (left.0, left.1.snapshot_id()).cmp(&(right.0, right.1.snapshot_id()))
        });
        let mut placed: BTreeSet<&EvidenceId> = BTreeSet::new();
        let mut groups: BTreeMap<RunGroup, Vec<(u64, &EvidenceId, bool, bool)>> = BTreeMap::new();
        for (position, snapshot, evidence) in ordered {
            for id in snapshot
                .command_evidence_ids()
                .iter()
                .chain(snapshot.test_evidence_ids())
            {
                if placed.contains(id) {
                    continue;
                }
                let Some(item) = evidence.iter().find(|item| item.evidence_id() == id) else {
                    continue;
                };
                let (Some(argv), Some(status)) = (argv_of(item), item.payload().run_status())
                else {
                    continue;
                };
                placed.insert(id);
                groups
                    .entry((
                        snapshot.task_id().clone(),
                        snapshot.workspace_id().clone(),
                        snapshot.capture_manifest_digest().clone(),
                        argv,
                    ))
                    .or_default()
                    .push((
                        position,
                        id,
                        status == RunStatus::Pass,
                        matches!(item.payload(), EvidencePayload::TestResult { .. }),
                    ));
            }
        }
        let mut overruled = BTreeMap::new();
        let mut test_overruled = BTreeMap::new();
        for runs in groups.values() {
            // Every run of the group, then (P3-L L2) its test runs alone, which
            // answer for its passed tests: a later command run does not
            // restore a test that a later test run did not pass.
            let tests = runs
                .iter()
                .filter(|(_, _, _, test)| *test)
                .copied()
                .collect::<Vec<_>>();
            for (runs, by_tests) in [(runs.as_slice(), false), (tests.as_slice(), true)] {
                let Some(failed) = latest_failure(runs) else {
                    continue;
                };
                for (_, id, ..) in runs.iter().filter(|(_, _, passed, _)| *passed) {
                    overruled
                        .entry((*id).clone())
                        .or_insert_with(|| failed.clone());
                    if by_tests {
                        test_overruled.insert((*id).clone(), failed.clone());
                    }
                }
            }
        }
        Self {
            overruled,
            test_overruled,
        }
    }

    /// The latest run that did not pass and overrules the passed run
    /// `evidence_id`, if any.
    #[must_use]
    pub fn overruled_by(&self, evidence_id: &EvidenceId) -> Option<&EvidenceId> {
        self.overruled.get(evidence_id)
    }

    /// P3-L3 (T16 S-1 item 2): when the passed test run `evidence_id` is
    /// overruled because the latest test run of its argv in its state did
    /// not pass (P3-L L2), that test run. Only a later passed test run of
    /// the argv restores it; a passed command run does not. `None` for a
    /// command run and for a test run only the first rule overrules.
    #[must_use]
    pub fn overruled_by_test_run(&self, evidence_id: &EvidenceId) -> Option<&EvidenceId> {
        self.test_overruled.get(evidence_id)
    }
}

/// The run that did not pass at the latest position of `runs`, if any; the
/// last such run in `runs` order when several share that position.
fn latest_failure<'a>(runs: &[(u64, &'a EvidenceId, bool, bool)]) -> Option<&'a EvidenceId> {
    let latest = runs.iter().map(|(position, ..)| *position).max()?;
    runs.iter()
        .rfind(|(position, _, passed, _)| *position == latest && !passed)
        .map(|(_, failed, ..)| *failed)
}

/// One completion Evidence object of a `transitionTask` to `completed`
/// (P2-C), with the Snapshot that seals it and that Snapshot's Evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEvidence {
    /// The cited Evidence.
    pub evidence: Evidence,
    /// The first Snapshot that contains it, `None` when none does.
    pub snapshot: Option<RepositorySnapshot>,
    /// Every Evidence object of that Snapshot.
    pub snapshot_evidence: Vec<Evidence>,
}

/// Why completion Evidence does not establish that a Task is complete; each
/// is `409 evidence_stale` on the wire (P2-C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionRefusal {
    /// No completion Evidence was cited.
    NoEvidence,
    /// A cited Evidence object does not exist in the Task's Project.
    Missing(EvidenceId),
    /// A cited Evidence object belongs to another Task.
    OtherTask(EvidenceId),
    /// A cited Evidence object is repository Evidence, not a command or test.
    NotPassed(EvidenceId),
    /// A cited Evidence object is not `verified` by [`judge_evidence`]: it is
    /// not fresh against the probe, its FR-203 status is not `PASS`, or a
    /// later run of its command in its state did not pass (P3-D3).
    NotVerified(EvidenceId, Judgement),
}

/// P2-C: whether `evidence` completes `task_id`: non-empty, every object of
/// the Task, a command or test, and `verified` against `probe` with
/// [`judge_evidence`], the function the Recovery Preview uses, which also
/// requires `PASS` (P2-E) not overruled by a later run of the Task's
/// `history` (P3-D3). The first refusal in citation order is returned.
///
/// # Errors
///
/// The first [`CompletionRefusal`].
pub fn judge_completion(
    task_id: &TaskId,
    evidence: &[CompletionEvidence],
    history: &RunHistory,
    probe: &Probe,
) -> Result<(), CompletionRefusal> {
    if evidence.is_empty() {
        return Err(CompletionRefusal::NoEvidence);
    }
    for item in evidence {
        let id = item.evidence.evidence_id();
        if item.evidence.task_id() != task_id {
            return Err(CompletionRefusal::OtherTask(id.clone()));
        }
        if item.evidence.payload().run_status().is_none() {
            return Err(CompletionRefusal::NotPassed(id.clone()));
        }
        let judgement = match &item.snapshot {
            Some(snapshot) => judge_evidence(
                snapshot,
                &item.snapshot_evidence,
                &item.evidence,
                history,
                probe,
            ),
            None => Judgement::new(Verdict::Tampered, [ReasonCode::ObjectMissing]),
        };
        if judgement.verdict() != Verdict::Verified {
            return Err(CompletionRefusal::NotVerified(id.clone(), judgement));
        }
    }
    Ok(())
}

/// Why `transitionTask` to `completed` is refused, in the order the daemon
/// checks (P2-C, P3-L L3), after the revision. ADR 0014 E3 (P4-D): the
/// write and the Task Capsule's `complete_task` action both call
/// [`completion_blocks`], so the action reads exactly the checks the write
/// applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionBlock {
    /// The Task's state has no edge to `completed` (`409 invalid_transition`).
    InvalidTransition,
    /// P3-L L3: the Task's integrity check fails (`409 evidence_stale`).
    Tampered,
    /// A cited Evidence object is missing or disagrees with the ledger
    /// (`409 evidence_stale`).
    Missing,
    /// [`judge_completion`] refuses the cited Evidence (`409 evidence_stale`).
    NotCompleted(CompletionRefusal),
}

/// Every reason `transitionTask` to `completed` is refused for, in check
/// order: the state graph, the integrity check, the cited Evidence that is
/// missing, and [`judge_completion`]. Empty when the transition is
/// accepted. The daemon refuses with the first one.
#[must_use]
pub fn completion_blocks(
    task_id: &TaskId,
    status: crate::TaskStatus,
    tampered: bool,
    missing: &[EvidenceId],
    evidence: &[CompletionEvidence],
    history: &RunHistory,
    probe: &Probe,
) -> Vec<CompletionBlock> {
    let mut blocks = Vec::new();
    if status
        .check_transition(crate::TaskStatus::Completed, true)
        .is_err()
    {
        blocks.push(CompletionBlock::InvalidTransition);
    }
    if tampered {
        blocks.push(CompletionBlock::Tampered);
    }
    if !missing.is_empty() {
        blocks.push(CompletionBlock::Missing);
    }
    if let Err(refusal) = judge_completion(task_id, evidence, history, probe) {
        blocks.push(CompletionBlock::NotCompleted(refusal));
    }
    blocks
}

/// One Evidence object as judged for a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceAssessment {
    /// The Evidence.
    pub evidence_id: EvidenceId,
    /// Its kind.
    pub kind: EvidenceKind,
    /// Its canonical digest.
    pub canonical_digest: Sha256Digest,
    /// The stored argv of a command or test.
    pub argv: Option<Vec<String>>,
    /// The FR-203 outcome of a command or test; `None` for repository
    /// Evidence and for a missing object.
    pub run_status: Option<RunStatus>,
    /// U9 K2: the format a test run was recorded with, when it was.
    pub test_format: Option<TestFormat>,
    /// Its verdict.
    pub judgement: Judgement,
}

/// P4: a statement takes the worst verdict of its Evidence, plus
/// `evidence_stale` when any of it is stale; a statement without Evidence is
/// always `partial`.
#[must_use]
pub fn judge_statement(evidence: &[EvidenceAssessment]) -> Judgement {
    match Judgement::worst(evidence.iter().map(|item| &item.judgement)) {
        None => Judgement::new(Verdict::Partial, [ReasonCode::EvidenceUnavailable]),
        Some(worst) => {
            if evidence
                .iter()
                .any(|item| item.judgement.verdict() == Verdict::Stale)
            {
                worst.combine(&Judgement::new(Verdict::Stale, [ReasonCode::EvidenceStale]))
            } else {
                worst
            }
        }
    }
}

/// One statement of the Checkpoint as judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementAssessment {
    /// Its identity.
    pub claim: ClaimRef,
    /// Its classification.
    pub classification: CheckpointClassification,
    /// Its text.
    pub text: String,
    /// Whether its verdict propagates to the Checkpoint: only
    /// `verified_fact` statements do.
    pub propagates: bool,
    /// Its Evidence, in reference order.
    pub evidence: Vec<EvidenceAssessment>,
    /// Its verdict.
    pub judgement: Judgement,
}

/// P5, statement → Checkpoint: the worst verdict of its `verified_fact`
/// statements; a Checkpoint with none is `partial`.
#[must_use]
pub fn judge_checkpoint(statements: &[StatementAssessment]) -> Judgement {
    Judgement::worst(
        statements
            .iter()
            .filter(|statement| statement.propagates)
            .map(|statement| &statement.judgement),
    )
    .unwrap_or_else(|| Judgement::new(Verdict::Partial, [ReasonCode::EvidenceUnavailable]))
}

/// P5, Checkpoint → Task: the latest Checkpoint's verdict; no Checkpoint is
/// `partial`. `Task.status` is never changed by a verdict.
#[must_use]
pub fn judge_task(checkpoint: Option<&Judgement>) -> Judgement {
    checkpoint
        .cloned()
        .unwrap_or_else(|| Judgement::new(Verdict::Partial, [ReasonCode::EvidenceUnavailable]))
}

/// The recorded anchor of a Task's latest Checkpoint.
#[derive(Debug, Clone, Copy)]
pub enum Anchor<'a> {
    /// The Task has no Checkpoint.
    None,
    /// The Checkpoint, its Snapshot, and every Evidence object of the
    /// Snapshot, all with recomputed digests.
    Intact {
        /// The Checkpoint.
        checkpoint: &'a Checkpoint,
        /// Its Snapshot.
        snapshot: &'a RepositorySnapshot,
        /// The Snapshot's Evidence.
        evidence: &'a [Evidence],
    },
    /// The Checkpoint, its Snapshot, or their Evidence did not recompute or
    /// is missing.
    Damaged {
        /// A referenced record is missing.
        missing_object: bool,
        /// Evidence whose digests do not recompute.
        invalid_evidence_ids: &'a [EvidenceId],
    },
}

/// Something the Preview cannot tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Gap {
    /// The Task has no Checkpoint.
    NoCheckpoint,
    /// The Checkpoint has no `verified_fact` statement.
    NoVerifiedFact,
    /// A `verified_fact` statement cites no Evidence.
    StatementWithoutEvidence,
    /// A Checkpoint, Snapshot, or Evidence record does not recompute.
    RecordIntegrity,
    /// The Event chain does not verify.
    LedgerChainInvalid,
    /// No Workspace could be probed.
    WorkspaceUnavailable,
    /// The probed repository or worktree is not the registered one.
    WorkspaceIdentity,
    /// The Snapshot's commit is unavailable, so changed paths are incomplete.
    BaseCommitUnavailable,
    /// The Snapshot had untracked files whose content it did not capture.
    UntrackedFilesNotCaptured,
    /// More Sessions continue the Task than the Preview lists.
    SessionsTruncated,
    /// More Events followed the Checkpoint than the Preview lists.
    EventsTruncated,
    /// ADR 0013 D4: the Task Capsule's Evidence is bound to more distinct
    /// commits than the probe compares; the rest are listed unprobed and
    /// their changed paths are not counted. Never in a Recovery Preview.
    BaseCommitsOverLimit,
}

impl Gap {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoCheckpoint => "no_checkpoint",
            Self::NoVerifiedFact => "no_verified_fact",
            Self::StatementWithoutEvidence => "statement_without_evidence",
            Self::RecordIntegrity => "record_integrity",
            Self::LedgerChainInvalid => "ledger_chain_invalid",
            Self::WorkspaceUnavailable => "workspace_unavailable",
            Self::WorkspaceIdentity => "workspace_identity",
            Self::BaseCommitUnavailable => "base_commit_unavailable",
            Self::UntrackedFilesNotCaptured => "untracked_files_not_captured",
            Self::SessionsTruncated => "sessions_truncated",
            Self::EventsTruncated => "events_truncated",
            Self::BaseCommitsOverLimit => "base_commits_over_limit",
        }
    }
}

/// D5: the paths that may have changed since the Snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPaths {
    /// `repo://` paths, sorted.
    pub paths: BTreeSet<String>,
    /// Whether the diff from the Snapshot's commit was available.
    pub complete: bool,
}

/// D5: `git diff --name-only <X>` unioned with untracked paths, and with the
/// Snapshot's dirty paths when it was dirty. A superset when the Snapshot
/// was dirty; incomplete when `X` is unavailable.
#[must_use]
pub fn changed_paths(
    snapshot: &RepositorySnapshot,
    observation: &RepositoryObservation,
) -> ChangedPaths {
    let mut paths = observation
        .changed_since_base
        .iter()
        .flatten()
        .chain(&observation.untracked_paths)
        .cloned()
        .collect::<BTreeSet<_>>();
    if snapshot.is_dirty() {
        paths.extend(porcelain_v2_paths(snapshot.worktree_status_porcelain_v2()).all);
    }
    ChangedPaths {
        paths: paths
            .into_iter()
            .map(|path| format!("repo://{path}"))
            .collect(),
        complete: observation.changed_since_base.is_some(),
    }
}

/// Paths named by `git status --porcelain=v2` text (without `-z`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PorcelainPaths {
    /// Every path named, including both sides of a rename.
    pub all: BTreeSet<String>,
    /// Untracked (`?`) paths.
    pub untracked: BTreeSet<String>,
}

/// Parses the paths of porcelain v2 status text; C-quoted paths are
/// unquoted. Header and unknown lines are skipped.
#[must_use]
pub fn porcelain_v2_paths(status: &str) -> PorcelainPaths {
    let mut paths = PorcelainPaths::default();
    for line in status.lines() {
        let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
        let named: Vec<&str> = match kind {
            "1" => rest.splitn(8, ' ').nth(7).into_iter().collect(),
            "2" => rest
                .splitn(9, ' ')
                .nth(8)
                .map(|tail| tail.split('\t').collect())
                .unwrap_or_default(),
            "u" => rest.splitn(10, ' ').nth(9).into_iter().collect(),
            "?" | "!" => vec![rest],
            _ => Vec::new(),
        };
        for raw in named.into_iter().filter(|raw| !raw.is_empty()) {
            let path = unquote_c_style(raw);
            if kind == "?" {
                paths.untracked.insert(path.clone());
            }
            paths.all.insert(path);
        }
    }
    paths
}

/// Git's C-style path quoting, undone; unquoted input is returned as is.
fn unquote_c_style(raw: &str) -> String {
    let Some(inner) = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return raw.to_owned();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        match chars.next() {
            Some(b'a') => bytes.push(0x07),
            Some(b'b') => bytes.push(0x08),
            Some(b't') => bytes.push(b'\t'),
            Some(b'n') => bytes.push(b'\n'),
            Some(b'v') => bytes.push(0x0b),
            Some(b'f') => bytes.push(0x0c),
            Some(b'r') => bytes.push(b'\r'),
            Some(digit @ b'0'..=b'7') => {
                let mut value = u32::from(digit - b'0');
                for _ in 0..2 {
                    if let Some(next @ b'0'..=b'7') = chars.peek().copied() {
                        value = value * 8 + u32::from(next - b'0');
                        chars.next();
                    }
                }
                bytes.push(u8::try_from(value & 0xff).unwrap_or(u8::MAX));
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A check that would confirm a record again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextCheck {
    /// The stale or unconfirmed command or test Evidence.
    pub evidence_id: EvidenceId,
    /// Its stored argv, to rerun.
    pub argv: Vec<String>,
    /// The Workspace to rerun it in.
    pub workspace_id: Option<WorkspaceId>,
    /// The HEAD to rerun it at, when the probe saw one.
    pub at_head: Option<String>,
}

/// The complete judgement of one Task from its latest Checkpoint and one
/// probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    /// The Task's verdict.
    pub judgement: Judgement,
    /// Every statement of the Checkpoint, in section and ordinal order.
    pub facts: Vec<StatementAssessment>,
    /// Evidence supporting a `verified_fact` that is not `verified`.
    pub invalid_evidence_ids: BTreeSet<EvidenceId>,
    /// D5 changed paths.
    pub changed_paths: ChangedPaths,
    /// What cannot be told.
    pub gaps: BTreeSet<Gap>,
    /// "Rerun the stored argv at HEAD Y" for stale or unconfirmed commands
    /// and tests behind a `verified_fact`.
    pub next_checks: Vec<NextCheck>,
}

fn sections(checkpoint: &Checkpoint) -> Vec<(StatementSection, usize, &CheckpointStatement)> {
    [
        (
            StatementSection::VerifiedCompletedWork,
            checkpoint.verified_completed_work(),
        ),
        (StatementSection::OpenItems, checkpoint.open_items()),
        (
            StatementSection::NextAction,
            std::slice::from_ref(checkpoint.next_action()),
        ),
    ]
    .into_iter()
    .flat_map(|(section, statements)| {
        statements
            .iter()
            .enumerate()
            .map(move |(ordinal, statement)| (section, ordinal, statement))
    })
    .collect()
}

pub(crate) fn argv_of(evidence: &Evidence) -> Option<Vec<String>> {
    match evidence.payload() {
        EvidencePayload::CommandResult { argv, .. } | EvidencePayload::TestResult { argv, .. } => {
            Some(argv.clone())
        }
        _ => None,
    }
}

/// The whole propagation (P1–P6) for one Task: probe → Snapshot → Evidence
/// → statements → Checkpoint → Task, then the ledger chain, which only a
/// `tampered` verdict can follow. `history` is the Task's [`RunHistory`].
#[must_use]
pub fn assess(
    anchor: Anchor<'_>,
    ledger_chain_valid: bool,
    history: &RunHistory,
    probe: &Probe,
) -> Assessment {
    let mut gaps = BTreeSet::new();
    let mut assessment = match anchor {
        Anchor::None => {
            gaps.insert(Gap::NoCheckpoint);
            Assessment {
                judgement: judge_task(None),
                facts: Vec::new(),
                invalid_evidence_ids: BTreeSet::new(),
                changed_paths: ChangedPaths {
                    paths: BTreeSet::new(),
                    complete: false,
                },
                gaps: BTreeSet::new(),
                next_checks: Vec::new(),
            }
        }
        Anchor::Damaged {
            missing_object,
            invalid_evidence_ids,
        } => {
            gaps.insert(Gap::RecordIntegrity);
            let reason = if missing_object {
                ReasonCode::ObjectMissing
            } else {
                ReasonCode::ObjectDigestMismatch
            };
            let checkpoint = Judgement::new(Verdict::Tampered, [reason]);
            Assessment {
                judgement: judge_task(Some(&checkpoint)),
                facts: Vec::new(),
                invalid_evidence_ids: invalid_evidence_ids.iter().cloned().collect(),
                changed_paths: ChangedPaths {
                    paths: BTreeSet::new(),
                    complete: false,
                },
                gaps: BTreeSet::new(),
                next_checks: Vec::new(),
            }
        }
        Anchor::Intact {
            checkpoint,
            snapshot,
            evidence,
        } => assess_intact(checkpoint, snapshot, evidence, history, probe, &mut gaps),
    };
    if !ledger_chain_valid {
        gaps.insert(Gap::LedgerChainInvalid);
        assessment.judgement = assessment.judgement.combine(&Judgement::new(
            Verdict::Tampered,
            [ReasonCode::LedgerChainInvalid],
        ));
    }
    if matches!(probe.observation, ProbeObservation::Unavailable) {
        gaps.insert(Gap::WorkspaceUnavailable);
    }
    assessment.gaps.extend(gaps);
    assessment
}

/// P3-C2: a `tampered` Preview holds no `verified`: every statement and
/// every Evidence verdict inside it takes the document's integrity reasons
/// on top of its own, and each Evidence object of a propagating statement
/// is then listed in `invalid_evidence_ids`. Reruns stay as judged.
pub(crate) fn seal_tampered(assessment: &mut Assessment) {
    let Some(integrity) = integrity_judgement(&assessment.judgement) else {
        return;
    };
    for fact in &mut assessment.facts {
        fact.judgement = fact.judgement.combine(&integrity);
        for item in &mut fact.evidence {
            item.judgement = item.judgement.combine(&integrity);
            if fact.propagates {
                assessment
                    .invalid_evidence_ids
                    .insert(item.evidence_id.clone());
            }
        }
    }
}

/// P2–P4 over every statement of one intact Checkpoint, in section and
/// ordinal order: each cited Evidence object judged once with
/// [`judge_evidence`], each statement with [`judge_statement`], and a
/// `verified_fact` whose Evidence does not satisfy the Evidence rule of
/// `verified_completed_work` at best `partial` (P2-E, P2-F). The Recovery
/// Preview and the Task Capsule (ADR 0013) both judge statements here, with
/// the Task's [`RunHistory`] (P3-D3).
#[must_use]
pub fn judge_statements(
    checkpoint: &Checkpoint,
    snapshot: &RepositorySnapshot,
    evidence: &[Evidence],
    history: &RunHistory,
    probe: &Probe,
) -> Vec<StatementAssessment> {
    let lookup = evidence
        .iter()
        .map(|item| (item.evidence_id(), item))
        .collect::<BTreeMap<_, _>>();
    let mut judged = BTreeMap::new();
    let mut facts = Vec::new();
    for (section, ordinal, statement) in sections(checkpoint) {
        let assessed = statement
            .evidence_ids()
            .iter()
            .map(|id| {
                let judgement = judged
                    .entry(id.clone())
                    .or_insert_with(|| match lookup.get(id) {
                        Some(item) => judge_evidence(snapshot, evidence, item, history, probe),
                        None => Judgement::new(Verdict::Tampered, [ReasonCode::ObjectMissing]),
                    })
                    .clone();
                let item = lookup.get(id);
                EvidenceAssessment {
                    evidence_id: id.clone(),
                    kind: item.map_or(EvidenceKind::CommandResult, |item| item.kind()),
                    canonical_digest: item.map_or_else(
                        || Sha256Digest::of_bytes(b""),
                        |item| item.canonical_digest().clone(),
                    ),
                    argv: item.and_then(|item| argv_of(item)),
                    run_status: item.and_then(|item| item.payload().run_status()),
                    test_format: item.and_then(|item| item.payload().test_format()),
                    judgement,
                }
            })
            .collect::<Vec<_>>();
        let propagates = statement.classification() == CheckpointClassification::VerifiedFact;
        let mut judgement = judge_statement(&assessed);
        // P2-E: a verified_fact must rest on the Evidence the
        // verified_completed_work rule demands, in any section; a record
        // written before that rule was enforced everywhere is not confirmed.
        // P2-F: when a fresh run that did not pass already explains it
        // (`evidence_not_passed`), that is the reason; otherwise the passed
        // test the claim needs cannot be observed.
        if propagates
            && !verified_fact_evidence_holds(statement, evidence)
            && !judgement.reasons().contains(&ReasonCode::EvidenceNotPassed)
        {
            judgement = judgement.at_best(Verdict::Partial, ReasonCode::EvidenceUnavailable);
        }
        facts.push(StatementAssessment {
            claim: ClaimRef {
                checkpoint_id: checkpoint.checkpoint_id().clone(),
                section,
                ordinal,
            },
            classification: statement.classification(),
            text: statement.text().to_owned(),
            propagates,
            judgement,
            evidence: assessed,
        });
    }
    facts
}

fn assess_intact(
    checkpoint: &Checkpoint,
    snapshot: &RepositorySnapshot,
    evidence: &[Evidence],
    history: &RunHistory,
    probe: &Probe,
    gaps: &mut BTreeSet<Gap>,
) -> Assessment {
    let facts = judge_statements(checkpoint, snapshot, evidence, history, probe);
    // Only a statement whose verdict reaches the Checkpoint can leave a gap
    // by citing nothing (Owner decision, P2-C).
    if facts
        .iter()
        .any(|fact| fact.propagates && fact.evidence.is_empty())
    {
        gaps.insert(Gap::StatementWithoutEvidence);
    }
    if !facts.iter().any(|fact| fact.propagates) {
        gaps.insert(Gap::NoVerifiedFact);
    }
    if !unselected_untracked(snapshot, evidence).is_empty() {
        gaps.insert(Gap::UntrackedFilesNotCaptured);
    }
    let checkpoint_judgement = judge_checkpoint(&facts);
    let observation = judge_source(
        snapshot,
        snapshot_repository_identity(snapshot, evidence),
        probe,
    );
    if let Err(inherited) = &observation
        && inherited.verdict() == Verdict::Tampered
    {
        gaps.insert(Gap::WorkspaceIdentity);
    }
    let changed = match observation {
        Ok(observation) => {
            let changed = changed_paths(snapshot, observation);
            if !changed.complete {
                gaps.insert(Gap::BaseCommitUnavailable);
            }
            changed
        }
        Err(_) => ChangedPaths {
            paths: BTreeSet::new(),
            complete: false,
        },
    };
    let at_head = match &probe.observation {
        ProbeObservation::Observed(observation) => Some(observation.head_oid.clone()),
        ProbeObservation::Unavailable => None,
    };
    let mut invalid_evidence_ids = BTreeSet::new();
    let mut next_checks: Vec<NextCheck> = Vec::new();
    for item in facts
        .iter()
        .filter(|fact| fact.propagates)
        .flat_map(|fact| &fact.evidence)
    {
        if item.judgement.verdict() == Verdict::Verified {
            continue;
        }
        invalid_evidence_ids.insert(item.evidence_id.clone());
        let rerun = matches!(item.judgement.verdict(), Verdict::Stale | Verdict::Partial);
        if let (true, Some(argv)) = (rerun, &item.argv)
            && !next_checks
                .iter()
                .any(|check| check.evidence_id == item.evidence_id)
        {
            next_checks.push(NextCheck {
                evidence_id: item.evidence_id.clone(),
                argv: argv.clone(),
                workspace_id: probe.workspace_id.clone(),
                at_head: at_head.clone(),
            });
        }
    }
    Assessment {
        judgement: judge_task(Some(&checkpoint_judgement)),
        facts,
        invalid_evidence_ids,
        changed_paths: changed,
        gaps: BTreeSet::new(),
        next_checks,
    }
}

#[cfg(test)]
#[path = "ontology_tests.rs"]
mod tests;
