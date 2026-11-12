use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::{
    CanonicalJson, CheckpointId, EvidenceId, ProjectId, RepositorySnapshotId, SessionId,
    Sha256Digest, TaskId, UtcTimestamp, WorkspaceId,
};

/// Validation failures for immutable recovery-state domain objects.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecoveryDomainError {
    #[error("Evidence kind, subject, and payload must match")]
    EvidenceKindMismatch,
    #[error("Evidence collector name and version must be non-empty and contain no controls")]
    InvalidCollector,
    #[error("invalid Evidence field: {0}")]
    InvalidEvidenceField(&'static str),
    #[error("referenced Evidence is missing or duplicated")]
    MissingOrDuplicateEvidence,
    #[error("Evidence task/workspace association does not match")]
    EvidenceAssociationMismatch,
    #[error("RepositorySnapshot Evidence role does not match Evidence kind")]
    SnapshotEvidenceRoleMismatch,
    #[error("RepositorySnapshot Git fields do not match Evidence payload")]
    SnapshotGitMismatch,
    #[error("RepositorySnapshot digest mismatch")]
    SnapshotDigestMismatch,
    #[error("RepositorySnapshot capture manifest does not match its material inputs")]
    SnapshotCaptureManifestMismatch,
    #[error("Evidence digest mismatch")]
    EvidenceDigestMismatch,
    #[error("CheckpointStatement text must be non-empty")]
    EmptyCheckpointStatement,
    #[error("verified_fact requires at least one Evidence reference")]
    VerifiedFactRequiresEvidence,
    #[error("verified_completed_work accepts only verified_fact statements")]
    InvalidCompletedWorkClassification,
    #[error("verified_fact requires successful executed test Evidence")]
    InvalidCompletedWorkEvidence,
    #[error("next_action accepts only recommendation or unknown")]
    InvalidNextActionClassification,
    #[error("Checkpoint task or snapshot association does not match")]
    CheckpointAssociationMismatch,
    #[error("Checkpoint Evidence must be contained in its RepositorySnapshot")]
    CheckpointEvidenceOutsideSnapshot,
    #[error("Checkpoint digest mismatch")]
    CheckpointDigestMismatch,
    #[error("Checkpoint objective must be non-empty")]
    EmptyCheckpointObjective,
    #[error("canonical recovery JSON could not be constructed")]
    Canonicalization,
}

/// The only Evidence kinds accepted by the recovery kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    GitHead,
    GitWorktree,
    FileDigest,
    CommandResult,
    TestResult,
}

impl EvidenceKind {
    /// Returns the stable persistence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitHead => "git_head",
            Self::GitWorktree => "git_worktree",
            Self::FileDigest => "file_digest",
            Self::CommandResult => "command_result",
            Self::TestResult => "test_result",
        }
    }
}

/// Strict tagged subject for one Evidence observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceSubject {
    GitHead { repository_identity: Sha256Digest },
    GitWorktree { repository_identity: Sha256Digest },
    FileDigest { path: String },
    CommandResult { command: String },
    TestResult { test_name: String },
}

impl EvidenceSubject {
    const fn kind(&self) -> EvidenceKind {
        match self {
            Self::GitHead { .. } => EvidenceKind::GitHead,
            Self::GitWorktree { .. } => EvidenceKind::GitWorktree,
            Self::FileDigest { .. } => EvidenceKind::FileDigest,
            Self::CommandResult { .. } => EvidenceKind::CommandResult,
            Self::TestResult { .. } => EvidenceKind::TestResult,
        }
    }
}

/// U9 K2 (Owner O6): the `--test-format` a test run was recorded with, so
/// that the arguments which rerun it as the same kind of test run are read
/// from the record instead of guessed from its argv: `cargo` and `pytest`
/// count the cases from the runner's own summary, `cases` is a count the
/// caller declared (`--test-cases`). A run recorded before U9, and a test
/// run that names neither, has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestFormat {
    /// `--test-format cargo`: counted from `cargo test`'s summary lines.
    Cargo,
    /// `--test-format pytest`: counted from pytest's summary.
    Pytest,
    /// `--test-cases <N>`: the case count is the caller's.
    Cases,
}

impl TestFormat {
    /// The closed set, in wire order.
    pub const ALL: [Self; 3] = [Self::Cargo, Self::Pytest, Self::Cases];

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Pytest => "pytest",
            Self::Cases => "cases",
        }
    }

    /// The format spelled `name`, if it is one of the three.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.as_str() == name)
    }
}

/// Strict tagged payload for one Evidence observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidencePayload {
    GitHead {
        head_oid: String,
        branch: Option<String>,
    },
    GitWorktree {
        status_porcelain_v2: String,
    },
    FileDigest {
        sha256: Sha256Digest,
    },
    /// A command the caller ran. The optional members (ADR 0012) are omitted
    /// from canonical bytes when absent, so earlier digests are unchanged;
    /// `stdout`/`stderr` then hold bounded tails, with the digests of the
    /// complete streams. Empty tails beside the digests of non-empty streams
    /// mean the caller kept only the digests (`memprove run
    /// --digest-only`, P2-C).
    ///
    /// `launch_error` (P2-C) records a command that could not be started:
    /// the caller's error text, with `exit_code` -1 because no process ever
    /// exited. Such a run is `UNAVAILABLE` (PRD FR-203) and never passes.
    CommandResult {
        argv: Vec<String>,
        exit_code: i32,
        stdout: String,
        stderr: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<UtcTimestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finished_at: Option<UtcTimestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_sha256: Option<Sha256Digest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_sha256: Option<Sha256Digest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launch_error: Option<String>,
    },
    /// A test command the caller ran. `passed` holds exactly when the exit
    /// code is 0 and the caller-declared `test_case_count` is above 0: a run
    /// with no count or zero cases never passes (PRD FR-203, ADR 0012). A
    /// test that could not be started carries `launch_error`, as a command
    /// does.
    TestResult {
        argv: Vec<String>,
        exit_code: i32,
        stdout: String,
        stderr: String,
        passed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        test_case_count: Option<u64>,
        /// U9 K2: how the run was recorded; omitted from canonical bytes
        /// when absent, so every earlier digest is unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        test_format: Option<TestFormat>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<UtcTimestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finished_at: Option<UtcTimestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdout_sha256: Option<Sha256Digest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stderr_sha256: Option<Sha256Digest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        launch_error: Option<String>,
    },
}

/// The exit code recorded for a command that could not be started: no
/// process exited, and no started process reports it (a signal is recorded
/// as `128 + signal`).
pub const LAUNCH_FAILED_EXIT_CODE: i32 = -1;

/// The PRD FR-203 outcome of a command or test Evidence object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// The command exited 0; a test also counted at least one case.
    Pass,
    /// The command ran and did not pass.
    Fail,
    /// A test exited 0 without a positive case count: nothing was shown to
    /// run.
    NotRun,
    /// The command could not be started.
    Unavailable,
}

impl RunStatus {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotRun => "NOT_RUN",
            Self::Unavailable => "UNAVAILABLE",
        }
    }
}

impl EvidencePayload {
    /// Whether a `TestResult` with these fields counts as passed.
    #[must_use]
    pub const fn test_passed(exit_code: i32, test_case_count: Option<u64>) -> bool {
        exit_code == 0 && matches!(test_case_count, Some(count) if count > 0)
    }

    /// U9 K2: the format a test run was recorded with, when it was.
    #[must_use]
    pub const fn test_format(&self) -> Option<TestFormat> {
        match self {
            Self::TestResult { test_format, .. } => *test_format,
            _ => None,
        }
    }

    /// The FR-203 outcome of a command or test; `None` for repository
    /// Evidence. Only `Pass` supports a claim that something passed.
    #[must_use]
    pub const fn run_status(&self) -> Option<RunStatus> {
        match self {
            Self::CommandResult {
                launch_error: Some(_),
                ..
            }
            | Self::TestResult {
                launch_error: Some(_),
                ..
            } => Some(RunStatus::Unavailable),
            Self::CommandResult { exit_code: 0, .. } | Self::TestResult { passed: true, .. } => {
                Some(RunStatus::Pass)
            }
            Self::TestResult { exit_code: 0, .. } => Some(RunStatus::NotRun),
            Self::CommandResult { .. } | Self::TestResult { .. } => Some(RunStatus::Fail),
            Self::GitHead { .. } | Self::GitWorktree { .. } | Self::FileDigest { .. } => None,
        }
    }
}

impl EvidencePayload {
    const fn kind(&self) -> EvidenceKind {
        match self {
            Self::GitHead { .. } => EvidenceKind::GitHead,
            Self::GitWorktree { .. } => EvidenceKind::GitWorktree,
            Self::FileDigest { .. } => EvidenceKind::FileDigest,
            Self::CommandResult { .. } => EvidenceKind::CommandResult,
            Self::TestResult { .. } => EvidenceKind::TestResult,
        }
    }
}

/// Versioned collector identity recorded in Evidence canonical bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCollector {
    pub name: String,
    pub version: String,
}

/// Stable, already-materialized input for an immutable Evidence object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceInput {
    pub evidence_id: EvidenceId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub kind: EvidenceKind,
    pub subject: EvidenceSubject,
    pub observed_at: UtcTimestamp,
    pub collector: EvidenceCollector,
    pub payload: EvidencePayload,
}

/// Immutable typed Evidence with recomputable canonical digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    input: EvidenceInput,
    payload_digest: Sha256Digest,
    canonical_digest: Sha256Digest,
}

impl Evidence {
    /// Validates typed fields and computes both frozen digests.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryDomainError`] when types or fields violate the frozen contract.
    pub fn new(input: EvidenceInput) -> Result<Self, RecoveryDomainError> {
        validate_evidence_input(&input)?;
        let payload_digest = payload_digest(&input.payload)?;
        let canonical_digest = evidence_digest(&input, &payload_digest)?;
        Ok(Self {
            input,
            payload_digest,
            canonical_digest,
        })
    }

    /// Rehydrates stored Evidence only when both stored digests match recomputation.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryDomainError::EvidenceDigestMismatch`] for either mismatch.
    pub fn from_stored(
        input: EvidenceInput,
        stored_payload_digest: &Sha256Digest,
        stored_canonical_digest: &Sha256Digest,
    ) -> Result<Self, RecoveryDomainError> {
        let evidence = Self::new(input)?;
        if &evidence.payload_digest != stored_payload_digest
            || &evidence.canonical_digest != stored_canonical_digest
        {
            return Err(RecoveryDomainError::EvidenceDigestMismatch);
        }
        Ok(evidence)
    }

    #[must_use]
    pub fn evidence_id(&self) -> &EvidenceId {
        &self.input.evidence_id
    }

    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.input.task_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.input.workspace_id
    }

    #[must_use]
    pub const fn kind(&self) -> EvidenceKind {
        self.input.kind
    }

    #[must_use]
    pub fn subject(&self) -> &EvidenceSubject {
        &self.input.subject
    }

    #[must_use]
    pub fn observed_at(&self) -> &UtcTimestamp {
        &self.input.observed_at
    }

    #[must_use]
    pub fn collector(&self) -> &EvidenceCollector {
        &self.input.collector
    }

    #[must_use]
    pub fn payload(&self) -> &EvidencePayload {
        &self.input.payload
    }

    #[must_use]
    pub fn payload_digest(&self) -> &Sha256Digest {
        &self.payload_digest
    }

    #[must_use]
    pub fn canonical_digest(&self) -> &Sha256Digest {
        &self.canonical_digest
    }

    #[must_use]
    pub fn to_input(&self) -> EvidenceInput {
        self.input.clone()
    }

    /// Recomputes the payload digest from the typed payload.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryDomainError::Canonicalization`] if serialization fails.
    pub fn recompute_payload_digest(&self) -> Result<Sha256Digest, RecoveryDomainError> {
        payload_digest(&self.input.payload)
    }

    /// Recomputes the canonical digest from every frozen canonical field.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryDomainError::Canonicalization`] if serialization fails.
    pub fn recompute_canonical_digest(&self) -> Result<Sha256Digest, RecoveryDomainError> {
        evidence_digest(&self.input, &self.recompute_payload_digest()?)
    }
}

fn validate_evidence_input(input: &EvidenceInput) -> Result<(), RecoveryDomainError> {
    if input.kind != input.subject.kind() || input.kind != input.payload.kind() {
        return Err(RecoveryDomainError::EvidenceKindMismatch);
    }
    if invalid_text(&input.collector.name) || invalid_text(&input.collector.version) {
        return Err(RecoveryDomainError::InvalidCollector);
    }
    match &input.subject {
        EvidenceSubject::FileDigest { path }
            if !path.starts_with("repo://") || path.len() == "repo://".len() =>
        {
            return Err(RecoveryDomainError::InvalidEvidenceField("file path"));
        }
        EvidenceSubject::CommandResult { command } if invalid_text(command) => {
            return Err(RecoveryDomainError::InvalidEvidenceField("command"));
        }
        EvidenceSubject::TestResult { test_name } if invalid_text(test_name) => {
            return Err(RecoveryDomainError::InvalidEvidenceField("test_name"));
        }
        EvidenceSubject::GitHead { .. }
        | EvidenceSubject::GitWorktree { .. }
        | EvidenceSubject::FileDigest { .. }
        | EvidenceSubject::CommandResult { .. }
        | EvidenceSubject::TestResult { .. } => {}
    }
    if let EvidencePayload::GitHead { head_oid, branch } = &input.payload {
        if !valid_git_oid(head_oid) {
            return Err(RecoveryDomainError::InvalidEvidenceField("head_oid"));
        }
        if branch.as_ref().is_some_and(|value| invalid_text(value)) {
            return Err(RecoveryDomainError::InvalidEvidenceField("branch"));
        }
    }
    match &input.payload {
        EvidencePayload::TestResult {
            argv,
            exit_code,
            passed,
            test_case_count,
            started_at,
            finished_at,
            launch_error,
            ..
        } => {
            if argv.is_empty() || argv.iter().any(|argument| invalid_text(argument)) {
                return Err(RecoveryDomainError::InvalidEvidenceField("test argv"));
            }
            if *passed != EvidencePayload::test_passed(*exit_code, *test_case_count) {
                return Err(RecoveryDomainError::InvalidEvidenceField("test result"));
            }
            validate_launch_error(launch_error.as_deref(), *exit_code)?;
            validate_run_interval(started_at.as_ref(), finished_at.as_ref())?;
        }
        EvidencePayload::CommandResult {
            exit_code,
            started_at,
            finished_at,
            launch_error,
            ..
        } => {
            validate_launch_error(launch_error.as_deref(), *exit_code)?;
            validate_run_interval(started_at.as_ref(), finished_at.as_ref())?;
        }
        EvidencePayload::GitHead { .. }
        | EvidencePayload::GitWorktree { .. }
        | EvidencePayload::FileDigest { .. } => {}
    }
    Ok(())
}

/// A run that could not be started names why, and records
/// [`LAUNCH_FAILED_EXIT_CODE`]; a started run has no launch error.
fn validate_launch_error(
    launch_error: Option<&str>,
    exit_code: i32,
) -> Result<(), RecoveryDomainError> {
    match launch_error {
        Some(error) if invalid_text(error) || exit_code != LAUNCH_FAILED_EXIT_CODE => {
            Err(RecoveryDomainError::InvalidEvidenceField("launch_error"))
        }
        _ => Ok(()),
    }
}

/// A run that records both times cannot finish before it started.
fn validate_run_interval(
    started_at: Option<&UtcTimestamp>,
    finished_at: Option<&UtcTimestamp>,
) -> Result<(), RecoveryDomainError> {
    let (Some(started_at), Some(finished_at)) = (started_at, finished_at) else {
        return Ok(());
    };
    let instant = |value: &UtcTimestamp| chrono::DateTime::parse_from_rfc3339(value.as_str());
    match (instant(started_at), instant(finished_at)) {
        (Ok(started), Ok(finished)) if finished >= started => Ok(()),
        _ => Err(RecoveryDomainError::InvalidEvidenceField("run interval")),
    }
}

fn invalid_text(value: &str) -> bool {
    value.is_empty() || value.chars().any(char::is_control)
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn payload_digest(payload: &EvidencePayload) -> Result<Sha256Digest, RecoveryDomainError> {
    canonical_value(payload).map(|canonical| canonical.sha256())
}

fn evidence_digest(
    input: &EvidenceInput,
    payload_digest: &Sha256Digest,
) -> Result<Sha256Digest, RecoveryDomainError> {
    canonical_json(&json!({
        "schema_version": 1,
        "object_type": "evidence",
        "evidence_id": input.evidence_id,
        "task_id": input.task_id,
        "workspace_id": input.workspace_id,
        "kind": input.kind,
        "subject": input.subject,
        "observed_at": input.observed_at,
        "collector": input.collector,
        "payload": input.payload,
        "payload_digest": payload_digest,
    }))
    .map(|canonical| canonical.sha256())
}

/// Stable materialized input for a `RepositorySnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySnapshotInput {
    pub snapshot_id: RepositorySnapshotId,
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub head_oid: String,
    pub branch: Option<String>,
    pub worktree_status_porcelain_v2: String,
    pub index_digest: Sha256Digest,
    pub tracked_worktree_digest: Sha256Digest,
    pub capture_manifest_digest: Sha256Digest,
    pub git_head_evidence_id: EvidenceId,
    pub git_worktree_evidence_id: EvidenceId,
    pub file_evidence_ids: Vec<EvidenceId>,
    pub command_evidence_ids: Vec<EvidenceId>,
    pub test_evidence_ids: Vec<EvidenceId>,
    pub captured_at: UtcTimestamp,
}

/// Immutable repository state bound to typed Evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySnapshot {
    input: RepositorySnapshotInput,
    is_dirty: bool,
    canonical_digest: Sha256Digest,
}

impl RepositorySnapshot {
    /// Validates associations and roles, canonicalizes refs, and computes the digest.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryDomainError`] for missing, mismatched, or invalid Evidence.
    pub fn new(
        mut input: RepositorySnapshotInput,
        evidence: &[Evidence],
    ) -> Result<Self, RecoveryDomainError> {
        canonicalize_ids(&mut input.file_evidence_ids);
        canonicalize_ids(&mut input.command_evidence_ids);
        canonicalize_ids(&mut input.test_evidence_ids);
        validate_snapshot(&input, evidence, true)?;
        let is_dirty = !input.worktree_status_porcelain_v2.is_empty();
        let canonical_digest = snapshot_digest(&input, is_dirty, evidence)?;
        Ok(Self {
            input,
            is_dirty,
            canonical_digest,
        })
    }

    /// Rehydrates a snapshot only when stored derived state and digest match recomputation.
    ///
    /// # Errors
    ///
    /// Returns a typed integrity error for any mismatch.
    pub fn from_stored(
        input: RepositorySnapshotInput,
        stored_is_dirty: bool,
        stored_canonical_digest: &Sha256Digest,
        evidence: &[Evidence],
    ) -> Result<Self, RecoveryDomainError> {
        let snapshot = Self::new(input, evidence)?;
        if snapshot.is_dirty != stored_is_dirty
            || &snapshot.canonical_digest != stored_canonical_digest
        {
            return Err(RecoveryDomainError::SnapshotDigestMismatch);
        }
        Ok(snapshot)
    }

    /// Rehydrates a snapshot written by the original recovery-kernel migration.
    ///
    /// This compatibility path validates the original canonical digest while retaining the
    /// migration-supplied unavailable markers for capture fields that did not yet exist.
    ///
    /// # Errors
    ///
    /// Returns a typed integrity error for any legacy association or digest mismatch.
    pub fn from_stored_legacy_v1(
        mut input: RepositorySnapshotInput,
        stored_is_dirty: bool,
        stored_canonical_digest: &Sha256Digest,
        evidence: &[Evidence],
    ) -> Result<Self, RecoveryDomainError> {
        canonicalize_ids(&mut input.file_evidence_ids);
        canonicalize_ids(&mut input.command_evidence_ids);
        canonicalize_ids(&mut input.test_evidence_ids);
        validate_snapshot(&input, evidence, false)?;
        let is_dirty = !input.worktree_status_porcelain_v2.is_empty();
        let canonical_digest = legacy_snapshot_digest(&input, is_dirty, evidence)?;
        if is_dirty != stored_is_dirty || &canonical_digest != stored_canonical_digest {
            return Err(RecoveryDomainError::SnapshotDigestMismatch);
        }
        Ok(Self {
            input,
            is_dirty,
            canonical_digest,
        })
    }

    #[must_use]
    pub fn snapshot_id(&self) -> &RepositorySnapshotId {
        &self.input.snapshot_id
    }

    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.input.project_id
    }

    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.input.task_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.input.workspace_id
    }

    #[must_use]
    pub fn head_oid(&self) -> &str {
        &self.input.head_oid
    }

    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        self.input.branch.as_deref()
    }

    #[must_use]
    pub fn worktree_status_porcelain_v2(&self) -> &str {
        &self.input.worktree_status_porcelain_v2
    }

    #[must_use]
    pub fn index_digest(&self) -> &Sha256Digest {
        &self.input.index_digest
    }

    #[must_use]
    pub fn tracked_worktree_digest(&self) -> &Sha256Digest {
        &self.input.tracked_worktree_digest
    }

    #[must_use]
    pub fn capture_manifest_digest(&self) -> &Sha256Digest {
        &self.input.capture_manifest_digest
    }

    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.is_dirty
    }

    #[must_use]
    pub fn git_head_evidence_id(&self) -> &EvidenceId {
        &self.input.git_head_evidence_id
    }

    #[must_use]
    pub fn git_worktree_evidence_id(&self) -> &EvidenceId {
        &self.input.git_worktree_evidence_id
    }

    #[must_use]
    pub fn file_evidence_ids(&self) -> &[EvidenceId] {
        &self.input.file_evidence_ids
    }

    #[must_use]
    pub fn command_evidence_ids(&self) -> &[EvidenceId] {
        &self.input.command_evidence_ids
    }

    #[must_use]
    pub fn test_evidence_ids(&self) -> &[EvidenceId] {
        &self.input.test_evidence_ids
    }

    #[must_use]
    pub fn captured_at(&self) -> &UtcTimestamp {
        &self.input.captured_at
    }

    #[must_use]
    pub fn canonical_digest(&self) -> &Sha256Digest {
        &self.canonical_digest
    }

    #[must_use]
    pub fn to_input(&self) -> RepositorySnapshotInput {
        self.input.clone()
    }

    #[must_use]
    pub fn contains_evidence(&self, evidence_id: &EvidenceId) -> bool {
        self.all_evidence_ids()
            .any(|candidate| candidate == evidence_id)
    }

    pub fn all_evidence_ids(&self) -> impl Iterator<Item = &EvidenceId> {
        std::iter::once(&self.input.git_head_evidence_id)
            .chain(std::iter::once(&self.input.git_worktree_evidence_id))
            .chain(self.input.file_evidence_ids.iter())
            .chain(self.input.command_evidence_ids.iter())
            .chain(self.input.test_evidence_ids.iter())
    }
}

fn validate_snapshot(
    input: &RepositorySnapshotInput,
    evidence: &[Evidence],
    require_capture_manifest: bool,
) -> Result<(), RecoveryDomainError> {
    if !valid_git_oid(&input.head_oid)
        || input
            .branch
            .as_ref()
            .is_some_and(|branch| invalid_text(branch))
    {
        return Err(RecoveryDomainError::SnapshotGitMismatch);
    }
    let lookup = evidence_lookup(evidence)?;
    let roles = [
        (&input.git_head_evidence_id, EvidenceKind::GitHead),
        (&input.git_worktree_evidence_id, EvidenceKind::GitWorktree),
    ];
    for (id, kind) in roles
        .into_iter()
        .chain(
            input
                .file_evidence_ids
                .iter()
                .map(|id| (id, EvidenceKind::FileDigest)),
        )
        .chain(
            input
                .command_evidence_ids
                .iter()
                .map(|id| (id, EvidenceKind::CommandResult)),
        )
        .chain(
            input
                .test_evidence_ids
                .iter()
                .map(|id| (id, EvidenceKind::TestResult)),
        )
    {
        let item = lookup
            .get(id)
            .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)?;
        if item.kind() != kind {
            return Err(RecoveryDomainError::SnapshotEvidenceRoleMismatch);
        }
        if item.task_id() != &input.task_id || item.workspace_id() != &input.workspace_id {
            return Err(RecoveryDomainError::EvidenceAssociationMismatch);
        }
    }
    let head = lookup
        .get(&input.git_head_evidence_id)
        .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)?;
    let worktree = lookup
        .get(&input.git_worktree_evidence_id)
        .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)?;
    let (
        EvidenceSubject::GitHead {
            repository_identity: head_repository,
        },
        EvidencePayload::GitHead {
            head_oid: payload_head,
            branch: payload_branch,
        },
    ) = (head.subject(), head.payload())
    else {
        return Err(RecoveryDomainError::SnapshotEvidenceRoleMismatch);
    };
    let (
        EvidenceSubject::GitWorktree {
            repository_identity: worktree_repository,
        },
        EvidencePayload::GitWorktree {
            status_porcelain_v2: payload_status,
        },
    ) = (worktree.subject(), worktree.payload())
    else {
        return Err(RecoveryDomainError::SnapshotEvidenceRoleMismatch);
    };
    if head_repository != worktree_repository
        || payload_head != &input.head_oid
        || payload_branch != &input.branch
        || payload_status != &input.worktree_status_porcelain_v2
    {
        return Err(RecoveryDomainError::SnapshotGitMismatch);
    }
    if require_capture_manifest {
        validate_snapshot_capture_manifest(input, &lookup, head_repository)?;
    }
    let unique = input.all_ids().collect::<BTreeSet<_>>();
    if unique.len() != input.all_ids().count() {
        return Err(RecoveryDomainError::MissingOrDuplicateEvidence);
    }
    Ok(())
}

fn validate_snapshot_capture_manifest(
    input: &RepositorySnapshotInput,
    lookup: &BTreeMap<&EvidenceId, &Evidence>,
    repository_identity: &Sha256Digest,
) -> Result<(), RecoveryDomainError> {
    let selected_files = input
        .file_evidence_ids
        .iter()
        .map(|id| {
            let item = lookup
                .get(id)
                .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)?;
            match (item.subject(), item.payload()) {
                (EvidenceSubject::FileDigest { path }, EvidencePayload::FileDigest { sha256 }) => {
                    Ok((path.as_str(), sha256))
                }
                _ => Err(RecoveryDomainError::SnapshotEvidenceRoleMismatch),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let manifest_digest = repository_capture_manifest_digest(
        repository_identity,
        &input.head_oid,
        input.branch.as_deref(),
        &input.worktree_status_porcelain_v2,
        &input.index_digest,
        &input.tracked_worktree_digest,
        &selected_files,
    );
    if manifest_digest != input.capture_manifest_digest {
        return Err(RecoveryDomainError::SnapshotCaptureManifestMismatch);
    }
    Ok(())
}

impl RepositorySnapshotInput {
    fn all_ids(&self) -> impl Iterator<Item = &EvidenceId> {
        std::iter::once(&self.git_head_evidence_id)
            .chain(std::iter::once(&self.git_worktree_evidence_id))
            .chain(self.file_evidence_ids.iter())
            .chain(self.command_evidence_ids.iter())
            .chain(self.test_evidence_ids.iter())
    }
}

fn snapshot_digest(
    input: &RepositorySnapshotInput,
    is_dirty: bool,
    evidence: &[Evidence],
) -> Result<Sha256Digest, RecoveryDomainError> {
    let lookup = evidence_lookup(evidence)?;
    let reference = |id: &EvidenceId| -> Result<EvidenceStateReference<'_>, RecoveryDomainError> {
        let item = lookup
            .get(id)
            .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)?;
        Ok(EvidenceStateReference::from(*item))
    };
    let refs =
        |ids: &[EvidenceId]| -> Result<Vec<EvidenceStateReference<'_>>, RecoveryDomainError> {
            let references = ids.iter().map(reference).collect::<Result<Vec<_>, _>>()?;
            let mut keyed = references
                .into_iter()
                .map(|item| {
                    canonical_value(&item).map(|canonical| (canonical.as_str().to_owned(), item))
                })
                .collect::<Result<Vec<_>, _>>()?;
            keyed.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(keyed.into_iter().map(|(_, item)| item).collect())
        };
    canonical_json(&json!({
        "schema_version": 1,
        "object_type": "repository_snapshot_state",
        "head_oid": input.head_oid,
        "branch": input.branch,
        "worktree_status_porcelain_v2": input.worktree_status_porcelain_v2,
        "is_dirty": is_dirty,
        "index_digest": input.index_digest,
        "tracked_worktree_digest": input.tracked_worktree_digest,
        "capture_manifest_digest": input.capture_manifest_digest,
        "git_head_evidence": reference(&input.git_head_evidence_id)?,
        "git_worktree_evidence": reference(&input.git_worktree_evidence_id)?,
        "file_evidence": refs(&input.file_evidence_ids)?,
        "command_evidence": refs(&input.command_evidence_ids)?,
        "test_evidence": refs(&input.test_evidence_ids)?,
    }))
    .map(|canonical| canonical.sha256())
}

fn legacy_snapshot_digest(
    input: &RepositorySnapshotInput,
    is_dirty: bool,
    evidence: &[Evidence],
) -> Result<Sha256Digest, RecoveryDomainError> {
    let lookup = evidence_lookup(evidence)?;
    let reference = |id: &EvidenceId| -> Result<EvidenceReference<'_>, RecoveryDomainError> {
        lookup
            .get(id)
            .copied()
            .map(EvidenceReference::from)
            .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)
    };
    let refs = |ids: &[EvidenceId]| -> Result<Vec<EvidenceReference<'_>>, RecoveryDomainError> {
        ids.iter().map(reference).collect()
    };
    canonical_json(&json!({
        "schema_version": 1,
        "object_type": "repository_snapshot",
        "snapshot_id": input.snapshot_id,
        "project_id": input.project_id,
        "task_id": input.task_id,
        "workspace_id": input.workspace_id,
        "head_oid": input.head_oid,
        "branch": input.branch,
        "worktree_status_porcelain_v2": input.worktree_status_porcelain_v2,
        "is_dirty": is_dirty,
        "git_head_evidence": reference(&input.git_head_evidence_id)?,
        "git_worktree_evidence": reference(&input.git_worktree_evidence_id)?,
        "file_evidence": refs(&input.file_evidence_ids)?,
        "command_evidence": refs(&input.command_evidence_ids)?,
        "test_evidence": refs(&input.test_evidence_ids)?,
        "captured_at": input.captured_at,
    }))
    .map(|canonical| canonical.sha256())
}

#[derive(Serialize)]
struct EvidenceStateReference<'a> {
    kind: EvidenceKind,
    subject: &'a EvidenceSubject,
    collector: &'a EvidenceCollector,
    payload_digest: &'a Sha256Digest,
}

impl<'a> From<&'a Evidence> for EvidenceStateReference<'a> {
    fn from(evidence: &'a Evidence) -> Self {
        Self {
            kind: evidence.kind(),
            subject: evidence.subject(),
            collector: evidence.collector(),
            payload_digest: evidence.payload_digest(),
        }
    }
}

#[derive(Serialize)]
struct EvidenceReference<'a> {
    evidence_id: &'a EvidenceId,
    canonical_digest: &'a Sha256Digest,
}

impl<'a> From<&'a Evidence> for EvidenceReference<'a> {
    fn from(evidence: &'a Evidence) -> Self {
        Self {
            evidence_id: evidence.evidence_id(),
            canonical_digest: evidence.canonical_digest(),
        }
    }
}

/// Frozen Checkpoint statement classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointClassification {
    VerifiedFact,
    UserSuppliedContext,
    Recommendation,
    Unknown,
}

impl CheckpointClassification {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedFact => "verified_fact",
            Self::UserSuppliedContext => "user_supplied_context",
            Self::Recommendation => "recommendation",
            Self::Unknown => "unknown",
        }
    }
}

/// One ordered statement with canonicalized Evidence references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointStatement {
    classification: CheckpointClassification,
    text: String,
    evidence_ids: Vec<EvidenceId>,
}

impl CheckpointStatement {
    /// Creates a statement while preserving text and canonicalizing refs.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for empty text or unsupported verified facts.
    pub fn new(
        classification: CheckpointClassification,
        text: String,
        mut evidence_ids: Vec<EvidenceId>,
    ) -> Result<Self, RecoveryDomainError> {
        if text.is_empty() {
            return Err(RecoveryDomainError::EmptyCheckpointStatement);
        }
        canonicalize_ids(&mut evidence_ids);
        if classification == CheckpointClassification::VerifiedFact && evidence_ids.is_empty() {
            return Err(RecoveryDomainError::VerifiedFactRequiresEvidence);
        }
        Ok(Self {
            classification,
            text,
            evidence_ids,
        })
    }

    #[must_use]
    pub const fn classification(&self) -> CheckpointClassification {
        self.classification
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn evidence_ids(&self) -> &[EvidenceId] {
        &self.evidence_ids
    }
}

/// Stable materialized Checkpoint input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointInput {
    pub checkpoint_id: CheckpointId,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub snapshot_id: RepositorySnapshotId,
    pub project_event_sequence: u64,
    pub objective: String,
    pub verified_completed_work: Vec<CheckpointStatement>,
    pub open_items: Vec<CheckpointStatement>,
    pub next_action: CheckpointStatement,
    pub created_at: UtcTimestamp,
}

/// Immutable checkpoint tied to one snapshot and session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    input: CheckpointInput,
    digest: Sha256Digest,
}

impl Checkpoint {
    /// Validates checkpoint classifications and Evidence containment, then computes its digest.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryDomainError`] for any frozen-contract violation.
    pub fn new(
        input: CheckpointInput,
        snapshot: &RepositorySnapshot,
        evidence: &[Evidence],
    ) -> Result<Self, RecoveryDomainError> {
        validate_checkpoint(&input, snapshot, evidence)?;
        let digest = checkpoint_digest(&input, evidence)?;
        Ok(Self { input, digest })
    }

    /// Creates a new Checkpoint: [`Checkpoint::new`], and every
    /// `verified_fact` in any section must satisfy the Evidence rule of
    /// `verified_completed_work` (Owner decision, P2-E). Stored Checkpoints
    /// are read back with [`Checkpoint::from_stored`], which does not apply
    /// the extension, so a record written before it still reads back and the
    /// Recovery Preview judges it instead.
    ///
    /// # Errors
    ///
    /// [`RecoveryDomainError::InvalidCompletedWorkEvidence`] for an
    /// unsupported `verified_fact`, or any error of [`Checkpoint::new`].
    pub fn create(
        input: CheckpointInput,
        snapshot: &RepositorySnapshot,
        evidence: &[Evidence],
    ) -> Result<Self, RecoveryDomainError> {
        let checkpoint = Self::new(input, snapshot, evidence)?;
        let lookup = evidence_lookup(evidence)?;
        for statement in checkpoint
            .input
            .open_items
            .iter()
            .chain(std::iter::once(&checkpoint.input.next_action))
            .filter(|statement| statement.classification == CheckpointClassification::VerifiedFact)
        {
            check_verified_fact_evidence(statement, &lookup)?;
        }
        Ok(checkpoint)
    }

    /// Rehydrates a Checkpoint only when its digest recomputes exactly.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryDomainError::CheckpointDigestMismatch`] for persisted corruption.
    pub fn from_stored(
        input: CheckpointInput,
        stored_digest: &Sha256Digest,
        snapshot: &RepositorySnapshot,
        evidence: &[Evidence],
    ) -> Result<Self, RecoveryDomainError> {
        let checkpoint = Self::new(input, snapshot, evidence)?;
        if &checkpoint.digest != stored_digest {
            return Err(RecoveryDomainError::CheckpointDigestMismatch);
        }
        Ok(checkpoint)
    }

    #[must_use]
    pub fn checkpoint_id(&self) -> &CheckpointId {
        &self.input.checkpoint_id
    }

    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.input.task_id
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.input.session_id
    }

    #[must_use]
    pub fn snapshot_id(&self) -> &RepositorySnapshotId {
        &self.input.snapshot_id
    }

    #[must_use]
    pub const fn project_event_sequence(&self) -> u64 {
        self.input.project_event_sequence
    }

    #[must_use]
    pub fn objective(&self) -> &str {
        &self.input.objective
    }

    #[must_use]
    pub fn verified_completed_work(&self) -> &[CheckpointStatement] {
        &self.input.verified_completed_work
    }

    #[must_use]
    pub fn open_items(&self) -> &[CheckpointStatement] {
        &self.input.open_items
    }

    #[must_use]
    pub fn next_action(&self) -> &CheckpointStatement {
        &self.input.next_action
    }

    #[must_use]
    pub fn created_at(&self) -> &UtcTimestamp {
        &self.input.created_at
    }

    #[must_use]
    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    #[must_use]
    pub fn to_input(&self) -> CheckpointInput {
        self.input.clone()
    }
}

fn validate_checkpoint(
    input: &CheckpointInput,
    snapshot: &RepositorySnapshot,
    evidence: &[Evidence],
) -> Result<(), RecoveryDomainError> {
    if input.objective.is_empty() {
        return Err(RecoveryDomainError::EmptyCheckpointObjective);
    }
    if input.task_id != *snapshot.task_id() || input.snapshot_id != *snapshot.snapshot_id() {
        return Err(RecoveryDomainError::CheckpointAssociationMismatch);
    }
    if input
        .verified_completed_work
        .iter()
        .any(|statement| statement.classification != CheckpointClassification::VerifiedFact)
    {
        return Err(RecoveryDomainError::InvalidCompletedWorkClassification);
    }
    if !matches!(
        input.next_action.classification,
        CheckpointClassification::Recommendation | CheckpointClassification::Unknown
    ) {
        return Err(RecoveryDomainError::InvalidNextActionClassification);
    }
    let lookup = evidence_lookup(evidence)?;
    for statement in &input.verified_completed_work {
        check_verified_fact_evidence(statement, &lookup)?;
    }
    for statement in input
        .verified_completed_work
        .iter()
        .chain(&input.open_items)
        .chain(std::iter::once(&input.next_action))
    {
        for evidence_id in &statement.evidence_ids {
            if !snapshot.contains_evidence(evidence_id) {
                return Err(RecoveryDomainError::CheckpointEvidenceOutsideSnapshot);
            }
            let item = lookup
                .get(evidence_id)
                .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)?;
            if item.task_id() != snapshot.task_id()
                || item.workspace_id() != snapshot.workspace_id()
            {
                return Err(RecoveryDomainError::EvidenceAssociationMismatch);
            }
        }
    }
    Ok(())
}

/// The Evidence rule of a `verified_fact`: it cites at least one test, and
/// every test it cites exited 0 with a positive case count. Applied to
/// `verified_completed_work` whenever a Checkpoint is built, to every
/// section when one is created (P2-E), and by the Recovery Preview to every
/// propagating statement it reads.
fn check_verified_fact_evidence(
    statement: &CheckpointStatement,
    lookup: &BTreeMap<&EvidenceId, &Evidence>,
) -> Result<(), RecoveryDomainError> {
    let cited = statement
        .evidence_ids
        .iter()
        .map(|evidence_id| {
            lookup
                .get(evidence_id)
                .copied()
                .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if passed_tests_hold(cited.into_iter()) {
        Ok(())
    } else {
        Err(RecoveryDomainError::InvalidCompletedWorkEvidence)
    }
}

/// The Evidence rule of a `verified_fact` over the objects it cites: every
/// test among them passed (exit code 0 and a positive case count), and
/// there is at least one. P3-D2 judges a Decision by the same rule.
pub(crate) fn passed_tests_hold<'a>(cited: impl Iterator<Item = &'a Evidence>) -> bool {
    let mut has_successful_test = false;
    for item in cited {
        if let EvidencePayload::TestResult {
            exit_code, passed, ..
        } = item.payload()
        {
            if *exit_code != 0 || !passed {
                return false;
            }
            has_successful_test = true;
        }
    }
    has_successful_test
}

/// Whether `statement`, a `verified_fact`, satisfies the Evidence rule of
/// `verified_completed_work` over `evidence` (the Snapshot's Evidence). The
/// Recovery Preview uses it so that a Checkpoint recorded before P2-E with an
/// unsupported `verified_fact` outside `verified_completed_work` is never
/// `verified`.
#[must_use]
pub fn verified_fact_evidence_holds(
    statement: &CheckpointStatement,
    evidence: &[Evidence],
) -> bool {
    evidence_lookup(evidence)
        .and_then(|lookup| check_verified_fact_evidence(statement, &lookup))
        .is_ok()
}

fn checkpoint_digest(
    input: &CheckpointInput,
    evidence: &[Evidence],
) -> Result<Sha256Digest, RecoveryDomainError> {
    let lookup = evidence_lookup(evidence)?;
    let statements = |items: &[CheckpointStatement]| -> Result<Vec<_>, RecoveryDomainError> {
        items
            .iter()
            .map(|statement| statement_canonical(statement, &lookup))
            .collect()
    };
    canonical_json(&json!({
        "schema_version": 1,
        "object_type": "checkpoint",
        "checkpoint_id": input.checkpoint_id,
        "task_id": input.task_id,
        "session_id": input.session_id,
        "snapshot_id": input.snapshot_id,
        "objective": input.objective,
        "verified_completed_work": statements(&input.verified_completed_work)?,
        "open_items": statements(&input.open_items)?,
        "next_action": statement_canonical(&input.next_action, &lookup)?,
        "created_at": input.created_at,
    }))
    .map(|canonical| canonical.sha256())
}

fn statement_canonical<'a>(
    statement: &'a CheckpointStatement,
    lookup: &BTreeMap<&'a EvidenceId, &'a Evidence>,
) -> Result<serde_json::Value, RecoveryDomainError> {
    let refs = statement
        .evidence_ids
        .iter()
        .map(|id| {
            lookup
                .get(id)
                .copied()
                .map(EvidenceReference::from)
                .ok_or(RecoveryDomainError::MissingOrDuplicateEvidence)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "classification": statement.classification,
        "text": statement.text,
        "evidence": refs,
    }))
}

fn canonicalize_ids(ids: &mut Vec<EvidenceId>) {
    ids.sort_by_key(|id| *id.as_uuid().as_bytes());
    ids.dedup();
}

/// Computes the deterministic digest of every repository/filesystem input materialized by the
/// current recovery capture contract.
#[must_use]
pub fn repository_capture_manifest_digest(
    repository_identity: &Sha256Digest,
    head_oid: &str,
    branch: Option<&str>,
    status_porcelain_v2: &str,
    index_digest: &Sha256Digest,
    tracked_worktree_digest: &Sha256Digest,
    selected_files: &[(&str, &Sha256Digest)],
) -> Sha256Digest {
    let mut manifest = Vec::new();
    append_digest_field(&mut manifest, b"continuity.repository-capture-manifest.v1");
    append_digest_field(&mut manifest, repository_identity.as_str().as_bytes());
    append_digest_field(&mut manifest, head_oid.as_bytes());
    match branch {
        Some(branch) => {
            manifest.push(1);
            append_digest_field(&mut manifest, branch.as_bytes());
        }
        None => manifest.push(0),
    }
    append_digest_field(&mut manifest, status_porcelain_v2.as_bytes());
    append_digest_field(&mut manifest, index_digest.as_str().as_bytes());
    append_digest_field(&mut manifest, tracked_worktree_digest.as_str().as_bytes());
    let mut selected_files = selected_files.to_vec();
    selected_files.sort_by(|left, right| left.0.cmp(right.0));
    for (path, content_digest) in selected_files {
        append_digest_field(&mut manifest, path.as_bytes());
        append_digest_field(&mut manifest, b"regular_file");
        append_digest_field(&mut manifest, content_digest.as_str().as_bytes());
    }
    Sha256Digest::of_bytes(&manifest)
}

fn append_digest_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn evidence_lookup(
    evidence: &[Evidence],
) -> Result<BTreeMap<&EvidenceId, &Evidence>, RecoveryDomainError> {
    let mut lookup = BTreeMap::new();
    for item in evidence {
        if lookup.insert(item.evidence_id(), item).is_some() {
            return Err(RecoveryDomainError::MissingOrDuplicateEvidence);
        }
    }
    Ok(lookup)
}

fn canonical_value<T: Serialize>(value: &T) -> Result<CanonicalJson, RecoveryDomainError> {
    let value = serde_json::to_value(value).map_err(|_| RecoveryDomainError::Canonicalization)?;
    canonical_json(&value)
}

fn canonical_json(value: &serde_json::Value) -> Result<CanonicalJson, RecoveryDomainError> {
    CanonicalJson::from_trusted_value(value).map_err(|_| RecoveryDomainError::Canonicalization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CheckpointId, EvidenceId, ProjectId, RepositorySnapshotId, SessionId, Sha256Digest, TaskId,
        UtcTimestamp, WorkspaceId,
    };

    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::parse(value).expect("valid timestamp")
    }

    fn collector() -> EvidenceCollector {
        EvidenceCollector {
            name: "memprove-repo".to_owned(),
            version: "0.1.0".to_owned(),
        }
    }

    fn evidence(
        task_id: &TaskId,
        workspace_id: &WorkspaceId,
        kind: EvidenceKind,
        subject: EvidenceSubject,
        payload: EvidencePayload,
    ) -> Evidence {
        Evidence::new(EvidenceInput {
            evidence_id: EvidenceId::new(),
            task_id: task_id.clone(),
            workspace_id: workspace_id.clone(),
            kind,
            subject,
            observed_at: timestamp("2026-08-16T01:02:03Z"),
            collector: collector(),
            payload,
        })
        .expect("valid Evidence")
    }

    fn git_evidence(task_id: &TaskId, workspace_id: &WorkspaceId) -> (Evidence, Evidence) {
        let repository_identity = Sha256Digest::of_bytes(b"repository identity");
        let head = evidence(
            task_id,
            workspace_id,
            EvidenceKind::GitHead,
            EvidenceSubject::GitHead {
                repository_identity: repository_identity.clone(),
            },
            EvidencePayload::GitHead {
                head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                branch: Some("main".to_owned()),
            },
        );
        let worktree = evidence(
            task_id,
            workspace_id,
            EvidenceKind::GitWorktree,
            EvidenceSubject::GitWorktree {
                repository_identity,
            },
            EvidencePayload::GitWorktree {
                status_porcelain_v2: String::new(),
            },
        );
        (head, worktree)
    }

    fn capture_manifest(
        index_digest: &Sha256Digest,
        tracked_worktree_digest: &Sha256Digest,
        files: &[&Evidence],
    ) -> Sha256Digest {
        let selected_files = files
            .iter()
            .map(|item| match (item.subject(), item.payload()) {
                (EvidenceSubject::FileDigest { path }, EvidencePayload::FileDigest { sha256 }) => {
                    (path.as_str(), sha256)
                }
                _ => panic!("capture manifest file Evidence"),
            })
            .collect::<Vec<_>>();
        repository_capture_manifest_digest(
            &Sha256Digest::of_bytes(b"repository identity"),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some("main"),
            "",
            index_digest,
            tracked_worktree_digest,
            &selected_files,
        )
    }

    fn test_result_input(argv: Vec<String>, exit_code: i32, passed: bool) -> EvidenceInput {
        EvidenceInput {
            evidence_id: EvidenceId::new(),
            task_id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            kind: EvidenceKind::TestResult,
            subject: EvidenceSubject::TestResult {
                test_name: "recovery kernel".to_owned(),
            },
            observed_at: timestamp("2026-08-16T01:02:03Z"),
            collector: collector(),
            payload: EvidencePayload::TestResult {
                argv,
                exit_code,
                stdout: String::new(),
                stderr: String::new(),
                passed,
                test_case_count: Some(1),
                test_format: None,
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        }
    }

    fn checkpoint_with_completion_evidence(
        kind: EvidenceKind,
        subject: EvidenceSubject,
        payload: EvidencePayload,
    ) -> Result<Checkpoint, RecoveryDomainError> {
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let (head, worktree) = git_evidence(&task_id, &workspace_id);
        let supporting = evidence(&task_id, &workspace_id, kind, subject, payload);
        let mut file_evidence_ids = Vec::new();
        let mut command_evidence_ids = Vec::new();
        let mut test_evidence_ids = Vec::new();
        match supporting.kind() {
            EvidenceKind::FileDigest => file_evidence_ids.push(supporting.evidence_id().clone()),
            EvidenceKind::CommandResult => {
                command_evidence_ids.push(supporting.evidence_id().clone());
            }
            EvidenceKind::TestResult => test_evidence_ids.push(supporting.evidence_id().clone()),
            EvidenceKind::GitHead | EvidenceKind::GitWorktree => unreachable!("supporting kind"),
        }
        let evidence = vec![head.clone(), worktree.clone(), supporting.clone()];
        let index_digest = Sha256Digest::of_bytes(b"index");
        let tracked_worktree_digest = Sha256Digest::of_bytes(b"tracked worktree");
        let selected_files = match supporting.kind() {
            EvidenceKind::FileDigest => vec![&supporting],
            _ => vec![],
        };
        let snapshot = RepositorySnapshot::new(
            RepositorySnapshotInput {
                snapshot_id: RepositorySnapshotId::new(),
                project_id,
                task_id: task_id.clone(),
                workspace_id,
                head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                branch: Some("main".to_owned()),
                worktree_status_porcelain_v2: String::new(),
                capture_manifest_digest: capture_manifest(
                    &index_digest,
                    &tracked_worktree_digest,
                    &selected_files,
                ),
                index_digest,
                tracked_worktree_digest,
                git_head_evidence_id: head.evidence_id().clone(),
                git_worktree_evidence_id: worktree.evidence_id().clone(),
                file_evidence_ids,
                command_evidence_ids,
                test_evidence_ids,
                captured_at: timestamp("2026-08-16T01:02:04Z"),
            },
            &evidence,
        )?;
        Checkpoint::new(
            CheckpointInput {
                checkpoint_id: CheckpointId::new(),
                task_id,
                session_id: SessionId::new(),
                snapshot_id: snapshot.snapshot_id().clone(),
                project_event_sequence: 1,
                objective: "verify completed work".to_owned(),
                verified_completed_work: vec![CheckpointStatement::new(
                    CheckpointClassification::VerifiedFact,
                    "tests passed".to_owned(),
                    vec![supporting.evidence_id().clone()],
                )?],
                open_items: vec![],
                next_action: CheckpointStatement::new(
                    CheckpointClassification::Recommendation,
                    "continue".to_owned(),
                    vec![],
                )?,
                created_at: timestamp("2026-08-16T01:02:05Z"),
            },
            &snapshot,
            &evidence,
        )
    }

    #[test]
    fn all_five_evidence_variants_are_typed_and_deterministic() {
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let (head, worktree) = git_evidence(&task_id, &workspace_id);
        let file = evidence(
            &task_id,
            &workspace_id,
            EvidenceKind::FileDigest,
            EvidenceSubject::FileDigest {
                path: "repo://tracked.txt".to_owned(),
            },
            EvidencePayload::FileDigest {
                sha256: Sha256Digest::of_bytes(b"tracked fixture\n"),
            },
        );
        let command = evidence(
            &task_id,
            &workspace_id,
            EvidenceKind::CommandResult,
            EvidenceSubject::CommandResult {
                command: "git status".to_owned(),
            },
            EvidencePayload::CommandResult {
                argv: vec!["git".to_owned(), "status".to_owned()],
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        );
        let test = evidence(
            &task_id,
            &workspace_id,
            EvidenceKind::TestResult,
            EvidenceSubject::TestResult {
                test_name: "domain".to_owned(),
            },
            EvidencePayload::TestResult {
                argv: vec!["cargo".to_owned(), "test".to_owned()],
                exit_code: 0,
                stdout: "ok".to_owned(),
                stderr: String::new(),
                passed: true,
                test_case_count: Some(1),
                test_format: None,
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        );

        assert_eq!(head.kind(), EvidenceKind::GitHead);
        assert_eq!(worktree.kind(), EvidenceKind::GitWorktree);
        assert_eq!(file.kind(), EvidenceKind::FileDigest);
        assert_eq!(command.kind(), EvidenceKind::CommandResult);
        assert_eq!(test.kind(), EvidenceKind::TestResult);
        for item in [&head, &worktree, &file, &command, &test] {
            assert_eq!(
                item.payload_digest(),
                &item.recompute_payload_digest().unwrap()
            );
            assert_eq!(
                item.canonical_digest(),
                &item.recompute_canonical_digest().unwrap()
            );
        }
        assert_eq!(
            head,
            Evidence::new(head.to_input()).expect("same stable object recomputes identically")
        );
    }

    #[test]
    fn test_result_rejects_empty_argv() {
        assert!(Evidence::new(test_result_input(vec![], 0, true)).is_err());
    }

    #[test]
    fn test_result_rejects_nonzero_exit_with_success() {
        assert!(Evidence::new(test_result_input(vec!["cargo".to_owned()], 17, true)).is_err());
    }

    #[test]
    fn test_result_rejects_zero_exit_with_failure() {
        assert!(Evidence::new(test_result_input(vec!["cargo".to_owned()], 0, false)).is_err());
    }

    fn counted_test(
        exit_code: i32,
        passed: bool,
        test_case_count: Option<u64>,
    ) -> Result<Evidence, RecoveryDomainError> {
        let mut input = test_result_input(vec!["cargo".to_owned(), "test".to_owned()], 0, false);
        input.payload = EvidencePayload::TestResult {
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
            passed,
            test_case_count,
            test_format: None,
            started_at: Some(timestamp("2026-10-02T10:00:00Z")),
            finished_at: Some(timestamp("2026-10-02T10:00:01.500Z")),
            stdout_sha256: Some(Sha256Digest::of_bytes(b"")),
            stderr_sha256: Some(Sha256Digest::of_bytes(b"")),
            launch_error: None,
        };
        Evidence::new(input)
    }

    #[test]
    fn a_test_without_cases_never_passes() {
        // ADR 0012 / FR-203: zero cases or no declared count is not a pass.
        for count in [None, Some(0)] {
            assert!(counted_test(0, true, count).is_err(), "{count:?}");
            assert!(counted_test(0, false, count).is_ok(), "{count:?}");
        }
        assert!(counted_test(0, true, Some(3)).is_ok());
        assert!(counted_test(0, false, Some(3)).is_err());
        assert!(counted_test(1, true, Some(3)).is_err());
        assert!(counted_test(1, false, Some(3)).is_ok());
        assert!(EvidencePayload::test_passed(0, Some(1)));
        assert!(!EvidencePayload::test_passed(0, Some(0)));
        assert!(!EvidencePayload::test_passed(0, None));
        assert!(!EvidencePayload::test_passed(2, Some(1)));
    }

    #[test]
    fn a_run_cannot_finish_before_it_started() {
        let mut input = test_result_input(vec!["cargo".to_owned()], 0, false);
        input.payload = EvidencePayload::TestResult {
            argv: vec!["cargo".to_owned()],
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            passed: false,
            test_case_count: None,
            test_format: None,
            started_at: Some(timestamp("2026-10-02T10:00:01Z")),
            finished_at: Some(timestamp("2026-10-02T10:00:00.999Z")),
            stdout_sha256: None,
            stderr_sha256: None,
            launch_error: None,
        };
        assert_eq!(
            Evidence::new(input),
            Err(RecoveryDomainError::InvalidEvidenceField("run interval"))
        );
    }

    #[test]
    fn absent_optional_members_keep_earlier_payload_digests() {
        // ADR 0012 appends optional members; omitted ones change no bytes.
        let legacy = EvidencePayload::CommandResult {
            argv: vec!["git".to_owned(), "status".to_owned()],
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            started_at: None,
            finished_at: None,
            stdout_sha256: None,
            stderr_sha256: None,
            launch_error: None,
        };
        assert_eq!(
            canonical_value(&legacy).unwrap().as_str(),
            r#"{"argv":["git","status"],"exit_code":0,"stderr":"","stdout":"","type":"command_result"}"#
        );
        let parsed: EvidencePayload = serde_json::from_str(
            r#"{"type":"test_result","argv":["t"],"exit_code":0,"stdout":"","stderr":"","passed":false}"#,
        )
        .unwrap();
        assert!(matches!(
            parsed,
            EvidencePayload::TestResult {
                test_case_count: None,
                ..
            }
        ));
    }

    #[test]
    fn zero_case_test_cannot_support_verified_completion() {
        let result = checkpoint_with_completion_evidence(
            EvidenceKind::TestResult,
            EvidenceSubject::TestResult {
                test_name: "empty suite".to_owned(),
            },
            EvidencePayload::TestResult {
                argv: vec!["cargo".to_owned(), "test".to_owned()],
                exit_code: 0,
                stdout: "running 0 tests".to_owned(),
                stderr: String::new(),
                passed: false,
                test_case_count: Some(0),
                test_format: None,
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        );
        assert_eq!(
            result,
            Err(RecoveryDomainError::InvalidCompletedWorkEvidence)
        );
    }

    #[test]
    fn failed_test_cannot_support_verified_completion() {
        let result = checkpoint_with_completion_evidence(
            EvidenceKind::TestResult,
            EvidenceSubject::TestResult {
                test_name: "recovery kernel".to_owned(),
            },
            EvidencePayload::TestResult {
                argv: vec!["cargo".to_owned(), "test".to_owned()],
                exit_code: 17,
                stdout: String::new(),
                stderr: "failed".to_owned(),
                passed: false,
                test_case_count: Some(1),
                test_format: None,
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn not_run_without_executed_test_cannot_support_verified_completion() {
        let result = checkpoint_with_completion_evidence(
            EvidenceKind::FileDigest,
            EvidenceSubject::FileDigest {
                path: "repo://not-run.txt".to_owned(),
            },
            EvidencePayload::FileDigest {
                sha256: Sha256Digest::of_bytes(b"NOT_RUN"),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn unavailable_without_executed_test_cannot_support_verified_completion() {
        let result = checkpoint_with_completion_evidence(
            EvidenceKind::CommandResult,
            EvidenceSubject::CommandResult {
                command: "cargo test".to_owned(),
            },
            EvidencePayload::CommandResult {
                argv: vec!["cargo".to_owned(), "test".to_owned()],
                exit_code: 127,
                stdout: String::new(),
                stderr: "tool unavailable".to_owned(),
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn successful_executed_test_can_support_verified_completion() {
        let result = checkpoint_with_completion_evidence(
            EvidenceKind::TestResult,
            EvidenceSubject::TestResult {
                test_name: "recovery kernel".to_owned(),
            },
            EvidencePayload::TestResult {
                argv: vec!["cargo".to_owned(), "test".to_owned()],
                exit_code: 0,
                stdout: "ok".to_owned(),
                stderr: String::new(),
                passed: true,
                test_case_count: Some(1),
                test_format: None,
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn evidence_rejects_kind_subject_payload_mismatch() {
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let repository_identity = Sha256Digest::of_bytes(b"repository identity");
        let result = Evidence::new(EvidenceInput {
            evidence_id: EvidenceId::new(),
            task_id,
            workspace_id,
            kind: EvidenceKind::GitHead,
            subject: EvidenceSubject::FileDigest {
                path: "repo://tracked.txt".to_owned(),
            },
            observed_at: timestamp("2026-08-16T01:02:03Z"),
            collector: collector(),
            payload: EvidencePayload::GitHead {
                head_oid: repository_identity.as_str()[..40].to_owned(),
                branch: None,
            },
        });

        assert!(matches!(
            result,
            Err(RecoveryDomainError::EvidenceKindMismatch)
        ));
    }

    #[test]
    fn tagged_evidence_enums_reject_unknown_variants_fields_and_arbitrary_json() {
        let digest = Sha256Digest::of_bytes(b"repository identity");
        let unknown_field = format!(
            r#"{{"type":"git_head","repository_identity":"{}","extra":true}}"#,
            digest.as_str()
        );
        assert!(serde_json::from_str::<EvidenceSubject>(&unknown_field).is_err());
        assert!(serde_json::from_str::<EvidenceSubject>(r#"{"arbitrary":{}}"#).is_err());
        assert!(serde_json::from_str::<EvidencePayload>(r#"{"type":"future_kind"}"#).is_err());
        assert!(serde_json::from_str::<EvidenceKind>(r#""future_kind""#).is_err());
    }

    #[test]
    fn snapshot_canonicalizes_reference_order_and_derives_dirty_state() {
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let (head, worktree) = git_evidence(&task_id, &workspace_id);
        let first_file = evidence(
            &task_id,
            &workspace_id,
            EvidenceKind::FileDigest,
            EvidenceSubject::FileDigest {
                path: "repo://a".to_owned(),
            },
            EvidencePayload::FileDigest {
                sha256: Sha256Digest::of_bytes(b"a"),
            },
        );
        let second_file = evidence(
            &task_id,
            &workspace_id,
            EvidenceKind::FileDigest,
            EvidenceSubject::FileDigest {
                path: "repo://b".to_owned(),
            },
            EvidencePayload::FileDigest {
                sha256: Sha256Digest::of_bytes(b"b"),
            },
        );
        let base = RepositorySnapshotInput {
            snapshot_id: RepositorySnapshotId::new(),
            project_id,
            task_id,
            workspace_id,
            head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            branch: Some("main".to_owned()),
            worktree_status_porcelain_v2: String::new(),
            index_digest: Sha256Digest::of_bytes(b"index"),
            tracked_worktree_digest: Sha256Digest::of_bytes(b"tracked worktree"),
            capture_manifest_digest: capture_manifest(
                &Sha256Digest::of_bytes(b"index"),
                &Sha256Digest::of_bytes(b"tracked worktree"),
                &[&first_file, &second_file],
            ),
            git_head_evidence_id: head.evidence_id().clone(),
            git_worktree_evidence_id: worktree.evidence_id().clone(),
            file_evidence_ids: vec![
                second_file.evidence_id().clone(),
                first_file.evidence_id().clone(),
            ],
            command_evidence_ids: vec![],
            test_evidence_ids: vec![],
            captured_at: timestamp("2026-08-16T01:02:04Z"),
        };
        let evidence = vec![head, worktree, first_file, second_file];
        let first = RepositorySnapshot::new(base.clone(), &evidence).expect("valid snapshot");
        let mut reordered = base;
        reordered.file_evidence_ids.reverse();
        let second = RepositorySnapshot::new(reordered, &evidence).expect("valid snapshot");

        assert!(!first.is_dirty());
        assert_eq!(first.file_evidence_ids(), second.file_evidence_ids());
        assert_eq!(first.canonical_digest(), second.canonical_digest());
    }

    #[test]
    fn snapshot_state_digest_excludes_capture_identity_and_time() {
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let repository_identity = Sha256Digest::of_bytes(b"repository identity");
        let evidence_at = |observed_at: &str| {
            let head = Evidence::new(EvidenceInput {
                evidence_id: EvidenceId::new(),
                task_id: task_id.clone(),
                workspace_id: workspace_id.clone(),
                kind: EvidenceKind::GitHead,
                subject: EvidenceSubject::GitHead {
                    repository_identity: repository_identity.clone(),
                },
                observed_at: timestamp(observed_at),
                collector: collector(),
                payload: EvidencePayload::GitHead {
                    head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    branch: Some("main".to_owned()),
                },
            })
            .expect("head Evidence");
            let worktree = Evidence::new(EvidenceInput {
                evidence_id: EvidenceId::new(),
                task_id: task_id.clone(),
                workspace_id: workspace_id.clone(),
                kind: EvidenceKind::GitWorktree,
                subject: EvidenceSubject::GitWorktree {
                    repository_identity: repository_identity.clone(),
                },
                observed_at: timestamp(observed_at),
                collector: collector(),
                payload: EvidencePayload::GitWorktree {
                    status_porcelain_v2: String::new(),
                },
            })
            .expect("worktree Evidence");
            (head, worktree)
        };
        let build = |observed_at: &str, captured_at: &str| {
            let (head, worktree) = evidence_at(observed_at);
            let evidence = vec![head.clone(), worktree.clone()];
            RepositorySnapshot::new(
                RepositorySnapshotInput {
                    snapshot_id: RepositorySnapshotId::new(),
                    project_id: project_id.clone(),
                    task_id: task_id.clone(),
                    workspace_id: workspace_id.clone(),
                    head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    branch: Some("main".to_owned()),
                    worktree_status_porcelain_v2: String::new(),
                    index_digest: Sha256Digest::of_bytes(b"index"),
                    tracked_worktree_digest: Sha256Digest::of_bytes(b"tracked worktree"),
                    capture_manifest_digest: capture_manifest(
                        &Sha256Digest::of_bytes(b"index"),
                        &Sha256Digest::of_bytes(b"tracked worktree"),
                        &[],
                    ),
                    git_head_evidence_id: head.evidence_id().clone(),
                    git_worktree_evidence_id: worktree.evidence_id().clone(),
                    file_evidence_ids: vec![],
                    command_evidence_ids: vec![],
                    test_evidence_ids: vec![],
                    captured_at: timestamp(captured_at),
                },
                &evidence,
            )
            .expect("snapshot")
        };

        let first = build("2026-08-16T01:02:03Z", "2026-08-16T01:02:04Z");
        let second = build("2026-08-16T02:02:03Z", "2026-08-16T02:02:04Z");

        assert_ne!(first.snapshot_id(), second.snapshot_id());
        assert_eq!(first.canonical_digest(), second.canonical_digest());
    }

    #[test]
    fn snapshot_state_digest_changes_for_distinct_tracked_bytes() {
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let (head, worktree) = git_evidence(&task_id, &workspace_id);
        let evidence = vec![head.clone(), worktree.clone()];
        let first_tracked_worktree_digest = Sha256Digest::of_bytes(b"first tracked bytes");
        let input = RepositorySnapshotInput {
            snapshot_id: RepositorySnapshotId::new(),
            project_id,
            task_id,
            workspace_id,
            head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            branch: Some("main".to_owned()),
            worktree_status_porcelain_v2: String::new(),
            index_digest: Sha256Digest::of_bytes(b"same index"),
            capture_manifest_digest: capture_manifest(
                &Sha256Digest::of_bytes(b"same index"),
                &first_tracked_worktree_digest,
                &[],
            ),
            tracked_worktree_digest: first_tracked_worktree_digest,
            git_head_evidence_id: head.evidence_id().clone(),
            git_worktree_evidence_id: worktree.evidence_id().clone(),
            file_evidence_ids: vec![],
            command_evidence_ids: vec![],
            test_evidence_ids: vec![],
            captured_at: timestamp("2026-08-16T01:02:04Z"),
        };
        let first = RepositorySnapshot::new(input.clone(), &evidence).expect("first snapshot");
        let second_tracked_worktree_digest = Sha256Digest::of_bytes(b"second tracked bytes");
        let second = RepositorySnapshot::new(
            RepositorySnapshotInput {
                capture_manifest_digest: capture_manifest(
                    &Sha256Digest::of_bytes(b"same index"),
                    &second_tracked_worktree_digest,
                    &[],
                ),
                tracked_worktree_digest: second_tracked_worktree_digest,
                ..input
            },
            &evidence,
        )
        .expect("second snapshot");

        assert_eq!(
            first.worktree_status_porcelain_v2(),
            second.worktree_status_porcelain_v2()
        );
        assert_ne!(first.canonical_digest(), second.canonical_digest());
    }

    #[test]
    fn checkpoint_rejects_unverified_completion_and_invalid_next_action() {
        assert!(matches!(
            CheckpointStatement::new(
                CheckpointClassification::VerifiedFact,
                "done".to_owned(),
                vec![],
            ),
            Err(RecoveryDomainError::VerifiedFactRequiresEvidence)
        ));

        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let (head, worktree) = git_evidence(&task_id, &workspace_id);
        let snapshot = RepositorySnapshot::new(
            RepositorySnapshotInput {
                snapshot_id: RepositorySnapshotId::new(),
                project_id,
                task_id: task_id.clone(),
                workspace_id,
                head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                branch: Some("main".to_owned()),
                worktree_status_porcelain_v2: String::new(),
                index_digest: Sha256Digest::of_bytes(b"index"),
                tracked_worktree_digest: Sha256Digest::of_bytes(b"tracked worktree"),
                capture_manifest_digest: capture_manifest(
                    &Sha256Digest::of_bytes(b"index"),
                    &Sha256Digest::of_bytes(b"tracked worktree"),
                    &[],
                ),
                git_head_evidence_id: head.evidence_id().clone(),
                git_worktree_evidence_id: worktree.evidence_id().clone(),
                file_evidence_ids: vec![],
                command_evidence_ids: vec![],
                test_evidence_ids: vec![],
                captured_at: timestamp("2026-08-16T01:02:04Z"),
            },
            &[head.clone(), worktree.clone()],
        )
        .expect("snapshot");
        let verified = CheckpointStatement::new(
            CheckpointClassification::VerifiedFact,
            "HEAD observed".to_owned(),
            vec![head.evidence_id().clone()],
        )
        .expect("verified statement");
        let invalid = Checkpoint::new(
            CheckpointInput {
                checkpoint_id: CheckpointId::new(),
                task_id,
                session_id: SessionId::new(),
                snapshot_id: snapshot.snapshot_id().clone(),
                project_event_sequence: 1,
                objective: "capture recovery state".to_owned(),
                verified_completed_work: vec![verified],
                open_items: vec![],
                next_action: CheckpointStatement::new(
                    CheckpointClassification::VerifiedFact,
                    "continue".to_owned(),
                    vec![head.evidence_id().clone()],
                )
                .expect("statement is locally valid"),
                created_at: timestamp("2026-08-16T01:02:05Z"),
            },
            &snapshot,
            &[head, worktree],
        );

        assert!(matches!(
            invalid,
            Err(RecoveryDomainError::InvalidNextActionClassification)
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn checkpoint_reference_order_is_canonical_but_statement_order_is_semantic() {
        let project_id = ProjectId::new();
        let task_id = TaskId::new();
        let workspace_id = WorkspaceId::new();
        let (head, worktree) = git_evidence(&task_id, &workspace_id);
        let test = evidence(
            &task_id,
            &workspace_id,
            EvidenceKind::TestResult,
            EvidenceSubject::TestResult {
                test_name: "ordering".to_owned(),
            },
            EvidencePayload::TestResult {
                argv: vec!["cargo".to_owned(), "test".to_owned()],
                exit_code: 0,
                stdout: "ok".to_owned(),
                stderr: String::new(),
                passed: true,
                test_case_count: Some(1),
                test_format: None,
                started_at: None,
                finished_at: None,
                stdout_sha256: None,
                stderr_sha256: None,
                launch_error: None,
            },
        );
        let evidence = vec![head.clone(), worktree.clone(), test.clone()];
        let snapshot = RepositorySnapshot::new(
            RepositorySnapshotInput {
                snapshot_id: RepositorySnapshotId::new(),
                project_id,
                task_id: task_id.clone(),
                workspace_id,
                head_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                branch: Some("main".to_owned()),
                worktree_status_porcelain_v2: String::new(),
                index_digest: Sha256Digest::of_bytes(b"index"),
                tracked_worktree_digest: Sha256Digest::of_bytes(b"tracked worktree"),
                capture_manifest_digest: capture_manifest(
                    &Sha256Digest::of_bytes(b"index"),
                    &Sha256Digest::of_bytes(b"tracked worktree"),
                    &[],
                ),
                git_head_evidence_id: head.evidence_id().clone(),
                git_worktree_evidence_id: worktree.evidence_id().clone(),
                file_evidence_ids: vec![],
                command_evidence_ids: vec![],
                test_evidence_ids: vec![test.evidence_id().clone()],
                captured_at: timestamp("2026-08-16T01:02:04Z"),
            },
            &evidence,
        )
        .expect("snapshot");
        let checkpoint_id = CheckpointId::new();
        let session_id = SessionId::new();
        let next_action = CheckpointStatement::new(
            CheckpointClassification::Unknown,
            "unknown".to_owned(),
            vec![],
        )
        .expect("next action");
        let statement = |text: &str, refs: Vec<EvidenceId>| {
            CheckpointStatement::new(
                CheckpointClassification::VerifiedFact,
                text.to_owned(),
                refs,
            )
            .expect("statement")
        };
        let input = CheckpointInput {
            checkpoint_id,
            task_id,
            session_id,
            snapshot_id: snapshot.snapshot_id().clone(),
            project_event_sequence: 1,
            objective: "ordering".to_owned(),
            verified_completed_work: vec![
                statement(
                    "first",
                    vec![
                        worktree.evidence_id().clone(),
                        test.evidence_id().clone(),
                        head.evidence_id().clone(),
                    ],
                ),
                statement(
                    "second",
                    vec![head.evidence_id().clone(), test.evidence_id().clone()],
                ),
            ],
            open_items: vec![],
            next_action,
            created_at: timestamp("2026-08-16T01:02:05Z"),
        };
        let canonical = Checkpoint::new(input.clone(), &snapshot, &evidence).expect("checkpoint");
        let mut ref_reordered = input.clone();
        ref_reordered.verified_completed_work[0] = statement(
            "first",
            vec![
                head.evidence_id().clone(),
                worktree.evidence_id().clone(),
                test.evidence_id().clone(),
            ],
        );
        let same = Checkpoint::new(ref_reordered, &snapshot, &evidence).expect("same checkpoint");
        assert_eq!(canonical.digest(), same.digest());

        let mut later_event_sequence = input.clone();
        later_event_sequence.project_event_sequence = 2;
        let same_state = Checkpoint::new(later_event_sequence, &snapshot, &evidence)
            .expect("event sequence is persistence context, not canonical state");
        assert_eq!(canonical.digest(), same_state.digest());

        let mut statements_reordered = input;
        statements_reordered.verified_completed_work.reverse();
        let changed =
            Checkpoint::new(statements_reordered, &snapshot, &evidence).expect("reordered");
        assert_ne!(canonical.digest(), changed.digest());
    }
}
