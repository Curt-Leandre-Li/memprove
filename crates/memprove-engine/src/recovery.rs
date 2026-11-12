//! Evidence capture and Checkpoint application service (ADR 0012, P2-A),
//! the read-only Recovery Preview (ADR 0012 D6, P2-B), and the read-only
//! Task Capsule (ADR 0013 D4, P3-B).
//!
//! The caller runs a command in its own environment and submits the result;
//! the service turns it into typed Evidence. Repository capture, which uses
//! the fixed-argv read-only Git probes, and persistence are one port
//! implemented by the daemon over `memprove-repo` and
//! `memprove-store-sqlite`.

use memprove_domain::{
    CapsuleProbe, Checkpoint, CheckpointClassification, CheckpointId, CheckpointStatement, ClaimId,
    ClaimRef, DecisionPayload, EdgeRelation, EventId, Evidence, EvidenceCollector, EvidenceId,
    EvidenceInput, EvidenceKind, EvidencePayload, EvidenceSubject, Probe, ProjectId, ProposedEdge,
    RecoveryPreview, RepositorySnapshot, RepositorySnapshotId, RoleRule, SessionId, Sha256Digest,
    StatementSection, TaskCapsule, TaskCapsuleView, TaskId, TaskLedgerView, TestFormat,
    UtcTimestamp, WorkspaceId, recovery_preview, task_capsule,
};

use crate::catalog::Command;

/// Why a recovery operation did not succeed. Each variant maps to exactly one
/// contract Problem code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryError {
    /// The request is well-formed JSON but cannot be served as asked. The
    /// text is the `detail` of the `400` Problem: the field or parameter and
    /// the rule it violates, never a value from the request.
    InvalidRequest(String),
    /// No registered Workspace has the requested identity.
    WorkspaceNotResolved,
    /// The Task, Session, Snapshot, Evidence, or Checkpoint does not exist.
    ResourceNotFound,
    /// The Workspace, Task, Session, or Snapshot belong to different scopes.
    ProjectWorkspaceMismatch,
    /// The idempotency key or `snapshot_id` was reused with another request.
    IdempotencyConflict,
    /// The Session is no longer active.
    InvalidTransition,
    /// P4-H1 (Owner, T20 SF-1/SF-2): a stored record the read needs is
    /// missing or does not decode or recompute, so no retry helps. On the
    /// wire it is `409 capsule_tampered`.
    Tampered,
    /// P4-H1 (review of the first P4-H1 commit): the Project's state
    /// database is not bound to it: its binding (`state_metadata`) or its
    /// migration record (`schema_migrations`) is missing, does not decode,
    /// or differs, so none of it is read as the Project's and no retry
    /// helps. On the wire it is `409 capsule_tampered`.
    UnboundState,
    /// A backing store or the Git probe failed; the operation fails closed.
    Unavailable,
    /// U9 R1 (Owner O4; ADR 0020): a Session that declared the role
    /// `sub_agent` records a Decision, or declares an edge whose target
    /// another agent wrote and is not an open item; nothing is written. On
    /// the wire it is `403 role_forbidden` with the rule's fixed `detail`.
    RoleForbidden(RoleRule),
}

/// The test part of a `test_run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestDeclaration {
    /// Name of the test target; the argv joined by spaces when absent.
    pub test_name: Option<String>,
    /// Number of test cases the caller observed, if it knows.
    pub test_case_count: Option<u64>,
    /// U9 K2 (Owner O6): the `--test-format` the caller made the run with,
    /// when it said.
    pub test_format: Option<TestFormat>,
}

/// One command the caller ran, as submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRun {
    /// Exact argument vector.
    pub argv: Vec<String>,
    /// Process exit code.
    pub exit_code: i32,
    /// When the command started.
    pub started_at: UtcTimestamp,
    /// When the command finished.
    pub finished_at: UtcTimestamp,
    /// Bounded tail of standard output.
    pub stdout_tail: String,
    /// Bounded tail of standard error.
    pub stderr_tail: String,
    /// Digest of the complete standard output.
    pub stdout_sha256: Sha256Digest,
    /// Digest of the complete standard error.
    pub stderr_sha256: Sha256Digest,
    /// The program that ran the command and collected its result.
    pub collector: EvidenceCollector,
    /// Present for a `test_run`.
    pub test: Option<TestDeclaration>,
    /// Why the command could not be started; the run is then `UNAVAILABLE`
    /// and `exit_code` is [`memprove_domain::LAUNCH_FAILED_EXIT_CODE`].
    pub launch_error: Option<String>,
}

/// `captureEvidence` after wire validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRequest {
    /// Workspace the command ran in.
    pub workspace_id: WorkspaceId,
    /// Task the Evidence supports.
    pub task_id: TaskId,
    /// Session that ran the command; it continues `task_id`.
    pub session_id: SessionId,
    /// Caller-chosen identity of the new Snapshot.
    pub snapshot_id: RepositorySnapshotId,
    /// The command result.
    pub run: CommandRun,
}

/// A capture ready for the port: the typed command Evidence and the
/// daemon-chosen identities of everything else written with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSubmission {
    /// Owning Project.
    pub project_id: ProjectId,
    /// Workspace to capture.
    pub workspace_id: WorkspaceId,
    /// Task the Evidence supports.
    pub task_id: TaskId,
    /// Session receiving `command.completed`.
    pub session_id: SessionId,
    /// Identity of the new Snapshot.
    pub snapshot_id: RepositorySnapshotId,
    /// The command or test Evidence.
    pub command: Evidence,
    /// Identity of the Snapshot's `git_head` Evidence.
    pub git_head_evidence_id: EvidenceId,
    /// Identity of the Snapshot's `git_worktree` Evidence.
    pub git_worktree_evidence_id: EvidenceId,
    /// Identity of the `command.completed` Event.
    pub event_id: EventId,
    /// Capture and receipt time.
    pub captured_at: UtcTimestamp,
}

/// One Evidence object and the Snapshots that contain it, earliest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedEvidence {
    /// The Evidence.
    pub evidence: Evidence,
    /// Snapshots containing it.
    pub snapshots: Vec<RepositorySnapshot>,
}

/// One statement as submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementRequest {
    /// Statement classification.
    pub classification: CheckpointClassification,
    /// Statement text.
    pub text: String,
    /// Evidence of the Snapshot that supports it.
    pub evidence_ids: Vec<EvidenceId>,
    /// Earlier claims of the Task this statement is the new version of
    /// (ADR 0013 D1, P3-C).
    pub supersedes: Vec<ClaimId>,
}

/// `createCheckpoint` after wire validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRequest {
    /// Workspace of the Session and Snapshot.
    pub workspace_id: WorkspaceId,
    /// Task the Checkpoint is about.
    pub task_id: TaskId,
    /// Session recording it; it continues `task_id`.
    pub session_id: SessionId,
    /// Recorded Snapshot it anchors to.
    pub snapshot_id: RepositorySnapshotId,
    /// What the work is for.
    pub objective: String,
    /// `verified_fact` statements, each backed by a passed test.
    pub verified_completed_work: Vec<StatementRequest>,
    /// Open items.
    pub open_items: Vec<StatementRequest>,
    /// A recommendation or unknown.
    pub next_action: StatementRequest,
    /// Earlier claims of the Task that no longer hold, with nothing that
    /// replaces them (ADR 0013 D1, P3-C).
    pub withdrawals: Vec<ClaimId>,
    /// Why the withdrawn claims no longer hold; required when one of them
    /// is a fact (P3-D2), recorded on each `withdraws` edge.
    pub withdrawal_reason: Option<String>,
}

/// A Checkpoint ready for the port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointSubmission {
    /// Owning Project.
    pub project_id: ProjectId,
    /// New Checkpoint identity.
    pub checkpoint_id: CheckpointId,
    /// Workspace of the Session and Snapshot.
    pub workspace_id: WorkspaceId,
    /// Task.
    pub task_id: TaskId,
    /// Session.
    pub session_id: SessionId,
    /// Snapshot.
    pub snapshot_id: RepositorySnapshotId,
    /// Objective.
    pub objective: String,
    /// Verified completed work.
    pub verified_completed_work: Vec<CheckpointStatement>,
    /// Open items.
    pub open_items: Vec<CheckpointStatement>,
    /// Next action.
    pub next_action: CheckpointStatement,
    /// Creation time.
    pub created_at: UtcTimestamp,
    /// Identity of the `checkpoint.created` Event.
    pub event_id: EventId,
    /// One `claim.superseded` Event each, in request order: every
    /// statement's `supersedes`, then the `withdrawals` (P3-C).
    pub edges: Vec<ProposedEdge>,
    /// Each statement's Evidence IDs in request order (P3-L L1).
    pub cited: RequestedCitations,
}

/// The Evidence IDs each statement of a `createCheckpoint` request cited, in
/// the order the request listed them (P3-L L1, T15 S-1). A
/// [`CheckpointStatement`] keeps its IDs sorted and deduplicated, so a
/// `400` detail that names `evidence_ids[j]` reads the position here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestedCitations {
    /// `$.verified_completed_work[i].evidence_ids`.
    pub verified_completed_work: Vec<Vec<EvidenceId>>,
    /// `$.open_items[i].evidence_ids`.
    pub open_items: Vec<Vec<EvidenceId>>,
    /// `$.next_action.evidence_ids`.
    pub next_action: Vec<EvidenceId>,
}

impl RequestedCitations {
    /// The Evidence IDs of the statement at `ordinal` of `section` as the
    /// request listed them, or `None` when they were not recorded.
    #[must_use]
    pub fn of(&self, section: StatementSection, ordinal: usize) -> Option<&[EvidenceId]> {
        match section {
            StatementSection::VerifiedCompletedWork => {
                self.verified_completed_work.get(ordinal).map(Vec::as_slice)
            }
            StatementSection::OpenItems => self.open_items.get(ordinal).map(Vec::as_slice),
            StatementSection::NextAction => (ordinal == 0).then_some(self.next_action.as_slice()),
        }
    }
}

/// `proposeDecision` after wire validation (ADR 0013 D2, P3-C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRequest {
    /// The Task the Decision is about.
    pub task_id: TaskId,
    /// The active Session recording it; it continues `task_id`.
    pub session_id: SessionId,
    /// The decision.
    pub text: String,
    /// Why, when given.
    pub rationale: Option<String>,
    /// Evidence of the Task it rests on.
    pub evidence_ids: Vec<EvidenceId>,
    /// Earlier claims of the Task it is the new version of.
    pub supersedes: Vec<ClaimId>,
}

/// A Decision ready for the port: its identity is its `decision.recorded`
/// Event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionSubmission {
    /// The Task.
    pub task_id: TaskId,
    /// The Session.
    pub session_id: SessionId,
    /// Identity of the `decision.recorded` Event, and so of the Decision.
    pub decision_id: EventId,
    /// What `decision.recorded@1` records.
    pub payload: DecisionPayload,
    /// One `claim.superseded` Event each, in request order.
    pub edges: Vec<ProposedEdge>,
    /// Receipt time.
    pub recorded_at: UtcTimestamp,
}

/// One recorded Decision, as written or replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedDecision {
    /// Identity: the `decision.recorded` Event.
    pub decision_id: EventId,
    /// Project sequence of that Event.
    pub project_sequence: u64,
    /// The Task.
    pub task_id: TaskId,
    /// The Session that recorded it.
    pub session_id: SessionId,
    /// What it records.
    pub payload: DecisionPayload,
    /// The claims it supersedes, in recorded order.
    pub supersedes: Vec<ClaimId>,
    /// When it was recorded.
    pub recorded_at: UtcTimestamp,
}

/// One Checkpoint and the Snapshot it anchors to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCheckpoint {
    /// The Checkpoint.
    pub checkpoint: Checkpoint,
    /// Its Snapshot.
    pub snapshot: RepositorySnapshot,
}

/// What a Recovery Preview is computed from: the ledger view at Project
/// sequence N, read first, and then one probe P.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewInputs {
    /// The Task's ledger view.
    pub view: TaskLedgerView,
    /// The probe of the asker's Workspace.
    pub probe: Probe,
}

/// What a Task Capsule is computed from (ADR 0013 D4): the Task's whole
/// ledger view at Project sequence N, read first, and then one probe P with
/// the changed paths since each probed base commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleInputs {
    /// The Task's whole ledger view.
    pub view: TaskCapsuleView,
    /// The probe of the asker's Workspace.
    pub probe: CapsuleProbe,
}

/// Capture and persistence port.
pub trait RecoveryStore: Send + Sync {
    /// Finds the `ACTIVE` Project that owns `workspace_id`.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::WorkspaceNotResolved`] or [`RecoveryError::Unavailable`].
    fn workspace_project(&self, workspace_id: &WorkspaceId) -> Result<ProjectId, RecoveryError>;

    /// Captures the Workspace, seals the command Evidence into the new
    /// Snapshot, and records both with `command.completed`, or replays.
    ///
    /// # Errors
    ///
    /// Any [`RecoveryError`].
    fn capture_evidence(
        &self,
        submission: &EvidenceSubmission,
        command: &Command,
    ) -> Result<RecordedEvidence, RecoveryError>;

    /// Reads one Evidence object in any `ACTIVE` Project.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::ResourceNotFound`] or [`RecoveryError::Unavailable`].
    fn evidence(&self, evidence_id: &EvidenceId) -> Result<RecordedEvidence, RecoveryError>;

    /// Records a Checkpoint over a recorded Snapshot, or replays.
    ///
    /// # Errors
    ///
    /// Any [`RecoveryError`].
    fn create_checkpoint(
        &self,
        submission: &CheckpointSubmission,
        command: &Command,
    ) -> Result<RecordedCheckpoint, RecoveryError>;

    /// Reads one Checkpoint in any `ACTIVE` Project.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::ResourceNotFound`] or [`RecoveryError::Unavailable`].
    fn checkpoint(&self, checkpoint_id: &CheckpointId)
    -> Result<RecordedCheckpoint, RecoveryError>;

    /// Records a Decision with its `decision.recorded` Event and one
    /// `claim.superseded` Event per edge, in the Project that owns the Task,
    /// or replays (ADR 0013 D2, P3-C).
    ///
    /// # Errors
    ///
    /// Any [`RecoveryError`].
    fn record_decision(
        &self,
        submission: &DecisionSubmission,
        command: &Command,
    ) -> Result<RecordedDecision, RecoveryError>;

    /// Reads the Task's ledger view, then probes the asker's Workspace:
    /// `workspace_id` when given, else the latest Checkpoint's Snapshot
    /// Workspace, else the Task's scoped Workspace. Writes nothing.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::ResourceNotFound`] for an unknown Task,
    /// [`RecoveryError::WorkspaceNotResolved`] for an unknown `workspace_id`,
    /// [`RecoveryError::ProjectWorkspaceMismatch`] for a Workspace of another
    /// Project, or [`RecoveryError::Unavailable`].
    fn preview_inputs(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<PreviewInputs, RecoveryError>;

    /// Reads the Task's whole ledger view, then probes the asker's
    /// Workspace (the Preview's rule) for the Snapshots its claims are bound
    /// to and at most `CAPSULE_BASE_LIMIT` base commits. Writes nothing.
    ///
    /// # Errors
    ///
    /// As [`RecoveryStore::preview_inputs`].
    fn capsule_inputs(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<CapsuleInputs, RecoveryError>;
}

impl<T: RecoveryStore + ?Sized> RecoveryStore for std::sync::Arc<T> {
    fn workspace_project(&self, workspace_id: &WorkspaceId) -> Result<ProjectId, RecoveryError> {
        (**self).workspace_project(workspace_id)
    }

    fn capture_evidence(
        &self,
        submission: &EvidenceSubmission,
        command: &Command,
    ) -> Result<RecordedEvidence, RecoveryError> {
        (**self).capture_evidence(submission, command)
    }

    fn evidence(&self, evidence_id: &EvidenceId) -> Result<RecordedEvidence, RecoveryError> {
        (**self).evidence(evidence_id)
    }

    fn create_checkpoint(
        &self,
        submission: &CheckpointSubmission,
        command: &Command,
    ) -> Result<RecordedCheckpoint, RecoveryError> {
        (**self).create_checkpoint(submission, command)
    }

    fn checkpoint(
        &self,
        checkpoint_id: &CheckpointId,
    ) -> Result<RecordedCheckpoint, RecoveryError> {
        (**self).checkpoint(checkpoint_id)
    }

    fn record_decision(
        &self,
        submission: &DecisionSubmission,
        command: &Command,
    ) -> Result<RecordedDecision, RecoveryError> {
        (**self).record_decision(submission, command)
    }

    fn preview_inputs(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<PreviewInputs, RecoveryError> {
        (**self).preview_inputs(task_id, workspace_id)
    }

    fn capsule_inputs(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<CapsuleInputs, RecoveryError> {
        (**self).capsule_inputs(task_id, workspace_id)
    }
}

/// Application service for Evidence and Checkpoints.
pub struct Recovery {
    store: Box<dyn RecoveryStore>,
}

impl Recovery {
    /// Creates the service over its port.
    #[must_use]
    pub fn new(store: Box<dyn RecoveryStore>) -> Self {
        Self { store }
    }

    /// `captureEvidence`: types the submitted command result, then captures
    /// and records it.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::InvalidRequest`] for a result the domain refuses, or
    /// the port's error.
    pub fn capture_evidence(
        &self,
        request: EvidenceRequest,
        command: &Command,
    ) -> Result<RecordedEvidence, RecoveryError> {
        let project_id = self.store.workspace_project(&request.workspace_id)?;
        let evidence = command_evidence(&request.task_id, &request.workspace_id, request.run)?;
        let submission = EvidenceSubmission {
            project_id,
            workspace_id: request.workspace_id,
            task_id: request.task_id,
            session_id: request.session_id,
            snapshot_id: request.snapshot_id,
            command: evidence,
            git_head_evidence_id: EvidenceId::new(),
            git_worktree_evidence_id: EvidenceId::new(),
            event_id: EventId::new(),
            captured_at: UtcTimestamp::now(),
        };
        self.store.capture_evidence(&submission, command)
    }

    /// `getEvidence`.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::ResourceNotFound`] or [`RecoveryError::Unavailable`].
    pub fn get_evidence(
        &self,
        evidence_id: &EvidenceId,
    ) -> Result<RecordedEvidence, RecoveryError> {
        self.store.evidence(evidence_id)
    }

    /// `createCheckpoint`.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::InvalidRequest`] for a statement the domain refuses,
    /// or the port's error.
    pub fn create_checkpoint(
        &self,
        request: CheckpointRequest,
        command: &Command,
    ) -> Result<RecordedCheckpoint, RecoveryError> {
        let project_id = self.store.workspace_project(&request.workspace_id)?;
        let checkpoint_id = CheckpointId::new();
        let edges = checkpoint_edges(&checkpoint_id, &request);
        let requested = |items: &[StatementRequest]| {
            items
                .iter()
                .map(|item| item.evidence_ids.clone())
                .collect::<Vec<_>>()
        };
        let cited = RequestedCitations {
            verified_completed_work: requested(&request.verified_completed_work),
            open_items: requested(&request.open_items),
            next_action: request.next_action.evidence_ids.clone(),
        };
        let statements = |items: Vec<StatementRequest>, member: &str| {
            items
                .into_iter()
                .enumerate()
                .map(|(index, item)| statement(item, &format!("$.{member}[{index}]")))
                .collect::<Result<Vec<_>, _>>()
        };
        let submission = CheckpointSubmission {
            project_id,
            checkpoint_id,
            workspace_id: request.workspace_id,
            task_id: request.task_id,
            session_id: request.session_id,
            snapshot_id: request.snapshot_id,
            objective: request.objective,
            verified_completed_work: statements(
                request.verified_completed_work,
                "verified_completed_work",
            )?,
            open_items: statements(request.open_items, "open_items")?,
            next_action: statement(request.next_action, "$.next_action")?,
            created_at: UtcTimestamp::now(),
            event_id: EventId::new(),
            edges,
            cited,
        };
        self.store.create_checkpoint(&submission, command)
    }

    /// `proposeDecision` (ADR 0013 D2, P3-C): records the Decision and its
    /// edges. There is no accept step.
    ///
    /// # Errors
    ///
    /// The port's error.
    pub fn propose_decision(
        &self,
        request: DecisionRequest,
        command: &Command,
    ) -> Result<RecordedDecision, RecoveryError> {
        let decision_id = EventId::new();
        let by = ClaimId::Decision(decision_id.clone());
        let edges = request
            .supersedes
            .into_iter()
            .enumerate()
            .map(|(index, target)| ProposedEdge {
                relation: EdgeRelation::Supersedes,
                target,
                by: Some(by.clone()),
                reason: None,
                at: format!("$.supersedes[{index}]"),
            })
            .collect();
        let submission = DecisionSubmission {
            task_id: request.task_id,
            session_id: request.session_id,
            decision_id,
            payload: DecisionPayload {
                text: request.text,
                rationale: request.rationale,
                evidence_ids: request.evidence_ids,
            },
            edges,
            recorded_at: UtcTimestamp::now(),
        };
        self.store.record_decision(&submission, command)
    }

    /// `getCheckpoint`.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::ResourceNotFound`] or [`RecoveryError::Unavailable`].
    pub fn get_checkpoint(
        &self,
        checkpoint_id: &CheckpointId,
    ) -> Result<RecordedCheckpoint, RecoveryError> {
        self.store.checkpoint(checkpoint_id)
    }

    /// `getRecoveryPreview`: freezes the ledger at N, probes once, and
    /// judges the Task's latest Checkpoint. Read-only; one (N, P) yields the
    /// same bytes.
    ///
    /// # Errors
    ///
    /// The port's error, or [`RecoveryError::Unavailable`] when the document
    /// cannot be rendered.
    pub fn recovery_preview(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<RecoveryPreview, RecoveryError> {
        let inputs = self.store.preview_inputs(task_id, workspace_id)?;
        recovery_preview(&inputs.view, &inputs.probe).map_err(|_| RecoveryError::Unavailable)
    }

    /// `getTaskCapsule`: freezes the ledger at N, probes once, and judges
    /// every claim of the Task (ADR 0013 D4). Read-only; one (N, P) yields
    /// the same bytes.
    ///
    /// # Errors
    ///
    /// The port's error, or [`RecoveryError::Unavailable`] when the document
    /// cannot be rendered.
    pub fn task_capsule(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<TaskCapsule, RecoveryError> {
        let inputs = self.store.capsule_inputs(task_id, workspace_id)?;
        task_capsule(&inputs.view, &inputs.probe).map_err(|_| RecoveryError::Unavailable)
    }
}

fn statement(request: StatementRequest, at: &str) -> Result<CheckpointStatement, RecoveryError> {
    CheckpointStatement::new(request.classification, request.text, request.evidence_ids)
        .map_err(|error| RecoveryError::InvalidRequest(format!("{at}: {error}")))
}

/// The edges a Checkpoint declares (P3-C), in request order: each
/// statement's `supersedes` (the statement is `by`), then `withdrawals`.
fn checkpoint_edges(
    checkpoint_id: &CheckpointId,
    request: &CheckpointRequest,
) -> Vec<ProposedEdge> {
    let mut edges = Vec::new();
    let sections = [
        (
            StatementSection::VerifiedCompletedWork,
            "$.verified_completed_work",
            request.verified_completed_work.as_slice(),
        ),
        (
            StatementSection::OpenItems,
            "$.open_items",
            request.open_items.as_slice(),
        ),
    ];
    let mut declare = |section, at: String, ordinal, item: &StatementRequest| {
        let by = ClaimId::Statement(ClaimRef {
            checkpoint_id: checkpoint_id.clone(),
            section,
            ordinal,
        });
        for (index, target) in item.supersedes.iter().enumerate() {
            edges.push(ProposedEdge {
                relation: EdgeRelation::Supersedes,
                target: target.clone(),
                by: Some(by.clone()),
                reason: None,
                at: format!("{at}.supersedes[{index}]"),
            });
        }
    };
    for (section, member, items) in sections {
        for (ordinal, item) in items.iter().enumerate() {
            declare(section, format!("{member}[{ordinal}]"), ordinal, item);
        }
    }
    declare(
        StatementSection::NextAction,
        "$.next_action".to_owned(),
        0,
        &request.next_action,
    );
    for (index, target) in request.withdrawals.iter().enumerate() {
        edges.push(ProposedEdge {
            relation: EdgeRelation::Withdraws,
            target: target.clone(),
            by: None,
            reason: request.withdrawal_reason.clone(),
            at: format!("$.withdrawals[{index}]"),
        });
    }
    edges
}

/// The typed command or test Evidence of `run`: observed when the command
/// finished, collected by the caller's collector. A test passes only with
/// exit code 0 and a positive case count.
///
/// # Errors
///
/// [`RecoveryError::InvalidRequest`] when the domain refuses the result.
pub fn command_evidence(
    task_id: &TaskId,
    workspace_id: &WorkspaceId,
    run: CommandRun,
) -> Result<Evidence, RecoveryError> {
    let command_line = run.argv.join(" ");
    let (kind, subject, payload) = match run.test {
        None => (
            EvidenceKind::CommandResult,
            EvidenceSubject::CommandResult {
                command: command_line,
            },
            EvidencePayload::CommandResult {
                argv: run.argv,
                exit_code: run.exit_code,
                stdout: run.stdout_tail,
                stderr: run.stderr_tail,
                started_at: Some(run.started_at),
                finished_at: Some(run.finished_at.clone()),
                stdout_sha256: Some(run.stdout_sha256),
                stderr_sha256: Some(run.stderr_sha256),
                launch_error: run.launch_error,
            },
        ),
        Some(test) => (
            EvidenceKind::TestResult,
            EvidenceSubject::TestResult {
                test_name: test.test_name.unwrap_or(command_line),
            },
            EvidencePayload::TestResult {
                argv: run.argv,
                exit_code: run.exit_code,
                stdout: run.stdout_tail,
                stderr: run.stderr_tail,
                passed: EvidencePayload::test_passed(run.exit_code, test.test_case_count),
                test_case_count: test.test_case_count,
                test_format: test.test_format,
                started_at: Some(run.started_at),
                finished_at: Some(run.finished_at.clone()),
                stdout_sha256: Some(run.stdout_sha256),
                stderr_sha256: Some(run.stderr_sha256),
                launch_error: run.launch_error,
            },
        ),
    };
    Evidence::new(EvidenceInput {
        evidence_id: EvidenceId::new(),
        task_id: task_id.clone(),
        workspace_id: workspace_id.clone(),
        kind,
        subject,
        observed_at: run.finished_at,
        collector: run.collector,
        payload,
    })
    .map_err(|error| RecoveryError::InvalidRequest(format!("$.metadata: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(test: Option<TestDeclaration>) -> CommandRun {
        CommandRun {
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            exit_code: 0,
            started_at: UtcTimestamp::parse("2026-10-02T10:00:00Z").unwrap(),
            finished_at: UtcTimestamp::parse("2026-10-02T10:00:02Z").unwrap(),
            stdout_tail: "test result: ok. 3 passed".to_owned(),
            stderr_tail: String::new(),
            stdout_sha256: Sha256Digest::of_bytes(b"out"),
            stderr_sha256: Sha256Digest::of_bytes(b""),
            collector: EvidenceCollector {
                name: "memprove-cli".to_owned(),
                version: "0.1.0".to_owned(),
            },
            test,
            launch_error: None,
        }
    }

    fn passed(evidence: &Evidence) -> Option<bool> {
        match evidence.payload() {
            EvidencePayload::TestResult { passed, .. } => Some(*passed),
            _ => None,
        }
    }

    #[test]
    fn a_test_passes_only_with_cases_and_exit_zero() {
        let (task, workspace) = (TaskId::new(), WorkspaceId::new());
        let test = |count| {
            command_evidence(
                &task,
                &workspace,
                run(Some(TestDeclaration {
                    test_name: None,
                    test_case_count: count,
                    test_format: None,
                })),
            )
            .unwrap()
        };
        assert_eq!(passed(&test(Some(3))), Some(true));
        assert_eq!(passed(&test(Some(0))), Some(false));
        assert_eq!(passed(&test(None)), Some(false));
        let failing = command_evidence(
            &task,
            &workspace,
            CommandRun {
                exit_code: 101,
                ..run(Some(TestDeclaration {
                    test_name: Some("unit".to_owned()),
                    test_case_count: Some(3),
                    test_format: None,
                }))
            },
        )
        .unwrap();
        assert_eq!(passed(&failing), Some(false));
        assert_eq!(
            failing.subject(),
            &EvidenceSubject::TestResult {
                test_name: "unit".to_owned()
            }
        );
        let command = command_evidence(&task, &workspace, run(None)).unwrap();
        assert_eq!(command.kind(), EvidenceKind::CommandResult);
        assert_eq!(command.observed_at().as_str(), "2026-10-02T10:00:02Z");
    }

    /// PRD FR-203 / P2-C: a command that could not be started is recorded as
    /// `UNAVAILABLE` and never passes, whatever it declares.
    #[test]
    fn a_command_that_could_not_start_is_unavailable_and_never_passes() {
        use memprove_domain::{LAUNCH_FAILED_EXIT_CODE, RunStatus};
        let (task, workspace) = (TaskId::new(), WorkspaceId::new());
        let unavailable = |test: Option<TestDeclaration>, exit_code| CommandRun {
            exit_code,
            stdout_tail: String::new(),
            launch_error: Some("No such file or directory (os error 2)".to_owned()),
            ..run(test)
        };
        let declared = || {
            Some(TestDeclaration {
                test_name: None,
                test_case_count: Some(3),
                test_format: None,
            })
        };
        for test in [None, declared()] {
            let evidence = command_evidence(
                &task,
                &workspace,
                unavailable(test, LAUNCH_FAILED_EXIT_CODE),
            )
            .unwrap();
            assert_eq!(
                evidence.payload().run_status(),
                Some(RunStatus::Unavailable)
            );
            assert_ne!(passed(&evidence), Some(true));
        }
        // A launch error with any other exit code, or an empty one, is refused.
        assert_eq!(
            command_evidence(&task, &workspace, unavailable(declared(), 0)),
            Err(RecoveryError::InvalidRequest(
                "$.metadata: invalid Evidence field: launch_error".to_owned()
            ))
        );
        let empty = CommandRun {
            launch_error: Some(String::new()),
            ..unavailable(None, LAUNCH_FAILED_EXIT_CODE)
        };
        assert_eq!(
            command_evidence(&task, &workspace, empty),
            Err(RecoveryError::InvalidRequest(
                "$.metadata: invalid Evidence field: launch_error".to_owned()
            ))
        );
        // A started run keeps its FR-203 outcome.
        let ok = command_evidence(&task, &workspace, run(declared())).unwrap();
        assert_eq!(ok.payload().run_status(), Some(RunStatus::Pass));
    }

    #[test]
    fn a_run_that_ends_before_it_starts_is_refused() {
        let backwards = CommandRun {
            finished_at: UtcTimestamp::parse("2026-10-02T09:59:59Z").unwrap(),
            ..run(None)
        };
        assert_eq!(
            command_evidence(&TaskId::new(), &WorkspaceId::new(), backwards),
            Err(RecoveryError::InvalidRequest(
                "$.metadata: invalid Evidence field: run interval".to_owned()
            ))
        );
    }
}
