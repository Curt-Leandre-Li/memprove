use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use memprove_domain::{
    CanonicalJson, Checkpoint, CheckpointClassification, CheckpointId, CheckpointInput,
    CheckpointStatement, ClaimId, ClaimKind, ClaimRef, Event, EventId, EventInput, EventProducer,
    Evidence, EvidenceCollector, EvidenceId, EvidenceInput, EvidenceKind, EvidencePayload,
    EvidenceSubject, IdempotencyContext, PrivacyClass, ProjectId, ProposedEdge,
    RecoveryDomainError, RepositorySnapshot, RepositorySnapshotId, RepositorySnapshotInput,
    SessionId, Sha256Digest, StatementSection, TaskId, UtcTimestamp, WorkspaceId,
};
use memprove_engine::recovery::RequestedCitations;
use memprove_repo::RepositoryProbe;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;

use super::{
    CheckpointCaptureContext, DaemonEvent, StateStore, append_daemon_event_versioned,
    append_event_in, ledger::LedgerReferences, next_project_sequence,
};
use crate::{Idempotent, OverruledCitation, StoreError, map_sqlite_error};

type StoredIdempotency = (String, String, Option<String>, Option<String>);

impl StateStore {
    /// Atomically commits a materialized Evidence → Snapshot → Checkpoint → Event bundle.
    ///
    /// Repository and capability-root checks complete before the `SQLite` write transaction.
    /// The transaction then revalidates only database-resident bindings and the frozen Project
    /// Event sequence before writing the bundle.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for validation, integrity, idempotency, recheck, or database errors.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_checkpoint_bundle(
        &mut self,
        capture: &CheckpointCaptureContext,
        evidence: &[Evidence],
        snapshot: &RepositorySnapshot,
        probe: &RepositoryProbe,
        checkpoint: &Checkpoint,
        event_id: &EventId,
        event_recorded_at: &UtcTimestamp,
        idempotency: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Checkpoint>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        if let Some(record) = bundle_idempotency(&self.connection, idempotency)? {
            let replayed =
                recover_bundle_idempotency(&self.connection, checkpoint, request_digest, record)?;
            capability_root.verify_identity()?;
            return Ok(Idempotent::Replayed(replayed));
        }
        verify_repository_preflight(capture, evidence, snapshot, probe, checkpoint)?;
        capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(record) = bundle_idempotency(&transaction, idempotency)? {
            let replayed =
                recover_bundle_idempotency(&transaction, checkpoint, request_digest, record)?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(replayed));
        }

        validate_bundle(&transaction, evidence, snapshot, checkpoint)?;
        validate_capture_context(&transaction, capture, snapshot, checkpoint)?;
        for item in evidence {
            insert_or_replay_evidence(&transaction, item)?;
        }
        insert_snapshot(&transaction, snapshot)?;
        insert_checkpoint(&transaction, checkpoint)?;
        let event = append_checkpoint_event(
            &transaction,
            evidence,
            snapshot,
            checkpoint,
            event_id,
            event_recorded_at,
        )?;
        transaction
            .execute(
                "INSERT INTO state_idempotency_records (
                    principal_id, operation_id, idempotency_key,
                    canonical_request_sha256, state, result_record_type,
                    result_record_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'SUCCEEDED', 'Checkpoint', ?5, ?6, ?6)",
                params![
                    idempotency.principal_id.as_str(),
                    idempotency.operation_id.as_str(),
                    idempotency.key.as_str(),
                    request_digest.as_str(),
                    checkpoint.checkpoint_id().as_str(),
                    event.recorded_at().as_str(),
                ],
            )
            .map_err(map_sqlite_error)?;
        transaction.commit()?;
        Ok(Idempotent::Created(checkpoint.clone()))
    }

    /// Freezes the database-bound repository identity and Project Event high-water mark before
    /// repository capture begins.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when Task, Workspace, Project, or repository bindings are invalid.
    pub fn begin_checkpoint_capture(
        &self,
        task_id: &TaskId,
        workspace_id: &WorkspaceId,
    ) -> Result<CheckpointCaptureContext, StoreError> {
        let project_id = state_project_id(&self.connection)?;
        let task_project = self
            .connection
            .query_row(
                "SELECT project_id FROM tasks WHERE task_id = ?1",
                [task_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("Task for checkpoint capture"))?;
        let workspace = self
            .connection
            .query_row(
                "SELECT project_id, canonical_worktree_path, git_dir
                 FROM workspaces WHERE workspace_id = ?1",
                [workspace_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::NotFound("Workspace for checkpoint capture"))?;
        if task_project != project_id.as_str() || workspace.0 != project_id.as_str() {
            return Err(StoreError::ConstraintViolation(
                "checkpoint capture Task/Workspace project association is invalid".to_owned(),
            ));
        }
        let repository_common_dir_sha256 = self.connection.query_row(
            "SELECT repository_common_dir_sha256 FROM state_metadata WHERE metadata_key = 1",
            [],
            |row| row.get::<_, String>(0),
        )?;
        Ok(CheckpointCaptureContext {
            project_event_sequence: project_event_sequence(&self.connection, &project_id)?,
            project_id,
            task_id: task_id.clone(),
            workspace_id: workspace_id.clone(),
            repository_common_dir_sha256: Sha256Digest::parse(&repository_common_dir_sha256)
                .map_err(|error| {
                    integrity(format!("invalid repository binding digest: {error}"))
                })?,
            worktree_root: PathBuf::from(workspace.1),
            git_dir: PathBuf::from(workspace.2),
        })
    }

    /// Reads and independently revalidates one immutable Evidence object.
    ///
    /// # Errors
    ///
    /// Returns a typed integrity error for missing, malformed, or digest-mismatched data.
    pub fn get_evidence(&self, evidence_id: &EvidenceId) -> Result<Evidence, StoreError> {
        evidence_by_id(&self.connection, evidence_id)?.ok_or(StoreError::NotFound("Evidence"))
    }

    /// Reads and independently revalidates one `RepositorySnapshot` and all of its Evidence.
    ///
    /// # Errors
    ///
    /// Returns a typed integrity error for impossible roles, associations, or digests.
    pub fn get_repository_snapshot(
        &self,
        snapshot_id: &RepositorySnapshotId,
    ) -> Result<RepositorySnapshot, StoreError> {
        snapshot_by_id(&self.connection, snapshot_id)?
            .ok_or(StoreError::NotFound("RepositorySnapshot"))
    }

    /// Reads and independently revalidates one Checkpoint, statements, refs, and snapshot.
    ///
    /// # Errors
    ///
    /// Returns a typed integrity error rather than returning an unverified object.
    pub fn get_checkpoint(&self, checkpoint_id: &CheckpointId) -> Result<Checkpoint, StoreError> {
        checkpoint_by_id(&self.connection, checkpoint_id)?.ok_or(StoreError::NotFound("Checkpoint"))
    }

    /// Atomically commits one captured command or test result: its Evidence,
    /// the `RepositorySnapshot` that pins it with fresh `git_head` and
    /// `git_worktree` Evidence, and one daemon-produced `command.completed`
    /// Event in the caller's Session (ADR 0012), or replays the result of the
    /// same idempotent request.
    ///
    /// The repository is rechecked against the Snapshot before the write
    /// transaction; inside it, the Session must be active, in the Snapshot's
    /// Workspace, and continue the Snapshot's Task.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdempotencyConflict`] for a reused key or `snapshot_id`,
    /// [`StoreError::NotFound`] for an unknown Session,
    /// [`StoreError::SessionWorkspaceMismatch`] for a Session or Task scoped
    /// to another Workspace, [`StoreError::SessionNotBoundToTask`],
    /// [`StoreError::SessionNotActive`], or another [`StoreError`].
    pub fn commit_evidence_capture(
        &mut self,
        capture: &EvidenceCapture<'_>,
        idempotency: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<EvidenceRecord>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        if let Some(evidence_id) =
            recorded_result(&self.connection, idempotency, request_digest, "Evidence")?
        {
            let replayed = evidence_record(&self.connection, &parse_result_id(&evidence_id)?)?;
            capability_root.verify_identity()?;
            return Ok(Idempotent::Replayed(replayed));
        }
        verify_snapshot_preflight(
            capture.context,
            capture.evidence,
            capture.snapshot,
            capture.probe,
        )?;
        capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(evidence_id) =
            recorded_result(&transaction, idempotency, request_digest, "Evidence")?
        {
            let replayed = evidence_record(&transaction, &parse_result_id(&evidence_id)?)?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(replayed));
        }
        let snapshot = capture.snapshot;
        if !capture_binding_matches(&transaction, capture.context, snapshot)? {
            return Err(StoreError::ConstraintViolation(
                "evidence capture binding changed".to_owned(),
            ));
        }
        let taken: bool = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM repository_snapshots WHERE snapshot_id = ?1)",
            [snapshot.snapshot_id().as_str()],
            |row| row.get(0),
        )?;
        if taken {
            return Err(StoreError::IdempotencyConflict);
        }
        check_task_scope(&transaction, snapshot.task_id(), snapshot.workspace_id())?;
        check_session_continues(
            &transaction,
            capture.session_id,
            snapshot.task_id(),
            snapshot.workspace_id(),
        )?;
        let subject = capture
            .evidence
            .iter()
            .find(|item| item.evidence_id() == capture.subject_evidence_id)
            .filter(|item| {
                matches!(
                    item.kind(),
                    EvidenceKind::CommandResult | EvidenceKind::TestResult
                )
            })
            .ok_or_else(|| constraint("captured command Evidence is missing"))?;
        for item in capture.evidence {
            insert_or_replay_evidence(&transaction, item)?;
        }
        // P3-D (ADR 0013 D3): earlier runs of the same state are sealed into
        // this Snapshot too, so one Checkpoint over it can cite them.
        let carried = super::carry_over::carried_evidence(&transaction, snapshot)?;
        let (snapshot, evidence) = if carried.is_empty() {
            (snapshot.clone(), capture.evidence.to_vec())
        } else {
            with_carried_evidence(snapshot, capture.evidence, carried)?
        };
        insert_snapshot(&transaction, &snapshot)?;
        append_command_event(&transaction, capture, &snapshot, &evidence, subject)?;
        record_result(
            &transaction,
            idempotency,
            request_digest,
            "Evidence",
            subject.evidence_id().as_str(),
            capture.recorded_at,
        )?;
        let record = evidence_record(&transaction, subject.evidence_id())?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(record))
    }

    /// Atomically creates one Checkpoint over an already recorded
    /// `RepositorySnapshot`, with its `checkpoint.created` Event, or replays
    /// the result of the same idempotent request. The repository is not
    /// probed again: the Snapshot is the anchor, and freshness is judged when
    /// read (ADR 0012). The Checkpoint's `project_event_sequence` is the
    /// Project's latest Event sequence inside the write transaction.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdempotencyConflict`], [`StoreError::NotFound`] for an
    /// unknown Snapshot, Task, or Session,
    /// [`StoreError::SessionWorkspaceMismatch`] when the Workspace, Session,
    /// and Snapshot disagree, [`StoreError::SessionNotBoundToTask`],
    /// [`StoreError::RecoveryInputRejected`] for statements the domain
    /// refuses, [`StoreError::SessionNotActive`], or another [`StoreError`].
    pub fn create_checkpoint(
        &mut self,
        draft: &CheckpointDraft,
        event_id: &EventId,
        recorded_at: &UtcTimestamp,
        idempotency: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Checkpoint>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(checkpoint_id) =
            recorded_result(&transaction, idempotency, request_digest, "Checkpoint")?
        {
            let checkpoint_id = CheckpointId::parse(&checkpoint_id)
                .map_err(|error| integrity(format!("invalid Checkpoint result ID: {error}")))?;
            let replayed = checkpoint_by_id(&transaction, &checkpoint_id)?
                .ok_or_else(|| integrity("Checkpoint idempotency result is missing"))?;
            capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(replayed));
        }
        let snapshot = snapshot_by_id(&transaction, &draft.snapshot_id)?
            .ok_or(StoreError::NotFound("RepositorySnapshot"))?;
        if snapshot.workspace_id() != &draft.workspace_id {
            return Err(StoreError::SessionWorkspaceMismatch);
        }
        if super::task_by_id(&transaction, &draft.task_id)?.is_none() {
            return Err(StoreError::NotFound("Task"));
        }
        if snapshot.task_id() != &draft.task_id {
            return Err(StoreError::RecoveryInputRejected(
                "the RepositorySnapshot belongs to another Task".to_owned(),
            ));
        }
        check_session_continues(
            &transaction,
            &draft.session_id,
            &draft.task_id,
            &draft.workspace_id,
        )?;
        let evidence = snapshot
            .all_evidence_ids()
            .map(|id| {
                evidence_by_id(&transaction, id)?
                    .ok_or_else(|| integrity("RepositorySnapshot referenced Evidence is missing"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let checkpoint = Checkpoint::create(
            CheckpointInput {
                checkpoint_id: draft.checkpoint_id.clone(),
                task_id: draft.task_id.clone(),
                session_id: draft.session_id.clone(),
                snapshot_id: draft.snapshot_id.clone(),
                project_event_sequence: super::latest_project_sequence(&transaction)?,
                objective: draft.objective.clone(),
                verified_completed_work: draft.verified_completed_work.clone(),
                open_items: draft.open_items.clone(),
                next_action: draft.next_action.clone(),
                created_at: draft.created_at.clone(),
            },
            &snapshot,
            &evidence,
        )
        .map_err(|error| StoreError::RecoveryInputRejected(error.to_string()))?;
        // P3-D3: no verified_fact rests on a passed run a later run of its
        // command in its state did not pass.
        check_overruled_runs(&transaction, &checkpoint, &draft.cited)?;
        // P3-C: every edge names an earlier readable claim of the Task.
        // U9 R1: and a `sub_agent` Session's edges name only open items
        // and its own agent's claims.
        super::decision::check_declared_edges(
            &transaction,
            &draft.task_id,
            &draft.session_id,
            &checkpoint_claims(&checkpoint),
            super::decision::Declared::Checkpoint(&draft.edges),
        )?;
        insert_checkpoint(&transaction, &checkpoint)?;
        append_checkpoint_event(
            &transaction,
            &evidence,
            &snapshot,
            &checkpoint,
            event_id,
            recorded_at,
        )?;
        super::decision::append_edges(
            &transaction,
            &draft.task_id,
            &draft.session_id,
            &draft.workspace_id,
            &draft.edges,
            recorded_at,
        )?;
        record_result(
            &transaction,
            idempotency,
            request_digest,
            "Checkpoint",
            checkpoint.checkpoint_id().as_str(),
            recorded_at,
        )?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(checkpoint))
    }

    /// Reads one Evidence object with the Snapshots that contain it,
    /// revalidating every digest.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] or a typed integrity error.
    pub fn get_evidence_record(
        &self,
        evidence_id: &EvidenceId,
    ) -> Result<EvidenceRecord, StoreError> {
        evidence_record(&self.connection, evidence_id)
    }
}

/// One materialized command or test capture, ready to commit.
pub struct EvidenceCapture<'a> {
    /// Database-bound repository identity frozen before capture.
    pub context: &'a CheckpointCaptureContext,
    /// Every Evidence object the Snapshot references.
    pub evidence: &'a [Evidence],
    /// The Snapshot pinning the command result to the repository state.
    pub snapshot: &'a RepositorySnapshot,
    /// The probe to recheck before the write transaction.
    pub probe: &'a RepositoryProbe,
    /// The command or test Evidence the caller submitted.
    pub subject_evidence_id: &'a EvidenceId,
    /// The caller's Session, which receives `command.completed`.
    pub session_id: &'a SessionId,
    /// Identity of the `command.completed` Event.
    pub event_id: &'a EventId,
    /// Receipt time.
    pub recorded_at: &'a UtcTimestamp,
}

/// Inputs of a Checkpoint over a recorded Snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointDraft {
    pub checkpoint_id: CheckpointId,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub snapshot_id: RepositorySnapshotId,
    pub objective: String,
    pub verified_completed_work: Vec<CheckpointStatement>,
    pub open_items: Vec<CheckpointStatement>,
    pub next_action: CheckpointStatement,
    pub created_at: UtcTimestamp,
    /// One `claim.superseded` Event each, appended after
    /// `checkpoint.created` (ADR 0013 P3-C).
    pub edges: Vec<ProposedEdge>,
    /// Each statement's Evidence IDs in request order, for the positions a
    /// `400` detail names (P3-L L1). A statement missing here is named in
    /// its stored (ascending) order.
    pub cited: RequestedCitations,
}

/// P3-D3 (Owner, T14 S-1): a `verified_fact`, in any section, may not cite
/// a passed run that the Task's [`memprove_domain::RunHistory`] overrules:
/// the latest run of the same argv in the same Task, Workspace, and
/// repository state did not pass, so the read path could never judge it
/// `verified`. [`StoreError::OverruledCitations`] lists, in request order,
/// every such citation by its position in the request (`cited`; P3-L L1,
/// T15 S-1), its Evidence, and the later run; for a test run the second
/// rule overrules (P3-L L2), the latest test run of its argv (P3-L3, T16
/// S-1). The daemon composes the `400` detail from the list.
fn check_overruled_runs(
    connection: &Connection,
    checkpoint: &Checkpoint,
    cited: &RequestedCitations,
) -> Result<(), StoreError> {
    let history = memprove_domain::run_history(&super::capsule::task_runs(
        connection,
        checkpoint.task_id(),
    )?);
    let sections = [
        (
            StatementSection::VerifiedCompletedWork,
            checkpoint.verified_completed_work(),
        ),
        (StatementSection::OpenItems, checkpoint.open_items()),
        (
            StatementSection::NextAction,
            std::slice::from_ref(checkpoint.next_action()),
        ),
    ];
    let mut overruled = Vec::new();
    for (section, statements) in sections {
        for (ordinal, statement) in statements.iter().enumerate() {
            if statement.classification() != CheckpointClassification::VerifiedFact {
                continue;
            }
            let requested = cited
                .of(section, ordinal)
                .unwrap_or_else(|| statement.evidence_ids());
            for (index, evidence_id) in requested.iter().enumerate() {
                let Some(later) = history.overruled_by(evidence_id) else {
                    continue;
                };
                let test_run = history.overruled_by_test_run(evidence_id);
                overruled.push(OverruledCitation {
                    position: if section == StatementSection::NextAction {
                        format!("$.next_action.evidence_ids[{index}]")
                    } else {
                        format!("$.{}[{ordinal}].evidence_ids[{index}]", section.as_str())
                    },
                    evidence_id: evidence_id.clone(),
                    later: test_run.unwrap_or(later).clone(),
                    by_test_run: test_run.is_some(),
                });
            }
        }
    }
    if overruled.is_empty() {
        Ok(())
    } else {
        Err(StoreError::OverruledCitations(overruled))
    }
}

/// The claims a Checkpoint records: each of its statements with its kind
/// (ADR 0013 D1; P3-C2: a fact is superseded only by a fact).
fn checkpoint_claims(checkpoint: &Checkpoint) -> Vec<(ClaimId, ClaimKind)> {
    let sections = [
        (
            StatementSection::VerifiedCompletedWork,
            checkpoint.verified_completed_work(),
        ),
        (StatementSection::OpenItems, checkpoint.open_items()),
        (
            StatementSection::NextAction,
            std::slice::from_ref(checkpoint.next_action()),
        ),
    ];
    sections
        .into_iter()
        .flat_map(|(section, statements)| {
            statements
                .iter()
                .enumerate()
                .map(move |(ordinal, statement)| {
                    (
                        ClaimId::Statement(ClaimRef {
                            checkpoint_id: checkpoint.checkpoint_id().clone(),
                            section,
                            ordinal,
                        }),
                        ClaimKind::of(section, statement.classification()),
                    )
                })
        })
        .collect()
}

/// One Evidence object and the Snapshots that contain it, the first sealer
/// first ([`sealing_snapshot_ids`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRecord {
    pub evidence: Evidence,
    pub snapshots: Vec<RepositorySnapshot>,
}

/// P4-B (Owner, P4-0 item D): the stored identities of the Snapshots that
/// seal `evidence_id`, the first sealer first, in ledger order: by the
/// Project sequence of the first `memproved` Event that records each
/// Snapshot (its ledger position, as a run's position in
/// [`memprove_domain::RunHistory`]), then by `snapshot_id`. A Snapshot no
/// such Event records comes last. `captured_at` is not read: neither a
/// digest nor an Event holds it, so rewriting it must not choose the first
/// sealer of a Decision's Evidence, of a completion's Evidence, or of
/// carry-over.
pub(super) fn sealing_snapshot_ids(
    connection: &Connection,
    references: &LedgerReferences,
    evidence_id: &EvidenceId,
) -> Result<Vec<String>, StoreError> {
    let mut snapshot_ids = connection
        .prepare(
            "SELECT snapshot.snapshot_id
             FROM repository_snapshot_evidence AS reference
             JOIN repository_snapshots AS snapshot USING (snapshot_id)
             WHERE reference.evidence_id = ?1",
        )?
        // P4-H1: an identity that is not UTF-8 does not parse (`lossy_text`).
        .query_map([evidence_id.as_str()], |row| super::lossy_text(row, 0))?
        .collect::<Result<Vec<_>, _>>()?;
    references.sort_snapshots_by_ledger(&mut snapshot_ids);
    Ok(snapshot_ids)
}

pub(super) fn evidence_record(
    connection: &Connection,
    evidence_id: &EvidenceId,
) -> Result<EvidenceRecord, StoreError> {
    let evidence =
        evidence_by_id(connection, evidence_id)?.ok_or(StoreError::NotFound("Evidence"))?;
    let references = LedgerReferences::read(connection)?;
    let snapshot_ids = sealing_snapshot_ids(connection, &references, evidence_id)?;
    let snapshots = snapshot_ids
        .iter()
        .map(|id| {
            let id = RepositorySnapshotId::parse(id)
                .map_err(|error| integrity(format!("invalid snapshot_id: {error}")))?;
            snapshot_by_id(connection, &id)?
                .ok_or_else(|| integrity("referenced RepositorySnapshot is missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EvidenceRecord {
        evidence,
        snapshots,
    })
}

/// The result ID of a completed idempotent request of `record_type`, or
/// `None` when the key is new.
pub(super) fn recorded_result(
    connection: &Connection,
    context: &IdempotencyContext,
    request_digest: &Sha256Digest,
    record_type: &str,
) -> Result<Option<String>, StoreError> {
    let Some((digest, state, result_type, result_id)) = bundle_idempotency(connection, context)?
    else {
        return Ok(None);
    };
    if digest != request_digest.as_str() {
        return Err(StoreError::IdempotencyConflict);
    }
    if state != "SUCCEEDED" || result_type.as_deref() != Some(record_type) {
        return Err(integrity(format!(
            "invalid {record_type} idempotency state or result type"
        )));
    }
    result_id
        .map(Some)
        .ok_or_else(|| integrity(format!("{record_type} idempotency result ID missing")))
}

fn parse_result_id(value: &str) -> Result<EvidenceId, StoreError> {
    EvidenceId::parse(value)
        .map_err(|error| integrity(format!("invalid Evidence result ID: {error}")))
}

pub(super) fn record_result(
    transaction: &Transaction<'_>,
    context: &IdempotencyContext,
    request_digest: &Sha256Digest,
    record_type: &str,
    record_id: &str,
    at: &UtcTimestamp,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO state_idempotency_records (
                principal_id, operation_id, idempotency_key,
                canonical_request_sha256, state, result_record_type,
                result_record_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'SUCCEEDED', ?5, ?6, ?7, ?7)",
            params![
                context.principal_id.as_str(),
                context.operation_id.as_str(),
                context.key.as_str(),
                request_digest.as_str(),
                record_type,
                record_id,
                at.as_str(),
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

/// Appends the daemon-produced `command.completed` Event of a capture: the
/// Snapshot's Evidence digests, the Snapshot digest, and the command outcome.
fn append_command_event(
    transaction: &Transaction<'_>,
    capture: &EvidenceCapture<'_>,
    snapshot: &RepositorySnapshot,
    evidence: &[Evidence],
    subject: &Evidence,
) -> Result<Event, StoreError> {
    let ordered = snapshot
        .all_evidence_ids()
        .map(|id| {
            evidence
                .iter()
                .find(|item| item.evidence_id() == id)
                .map(|item| {
                    json!({
                        "evidence_id": item.evidence_id(),
                        "canonical_digest": item.canonical_digest(),
                    })
                })
                .ok_or_else(|| constraint("snapshot Evidence missing from capture"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (exit_code, passed, finished_at) = match subject.payload() {
        EvidencePayload::CommandResult {
            exit_code,
            finished_at,
            ..
        } => (*exit_code, None, finished_at.as_ref()),
        EvidencePayload::TestResult {
            exit_code,
            passed,
            finished_at,
            ..
        } => (*exit_code, Some(*passed), finished_at.as_ref()),
        _ => return Err(constraint("captured Evidence is not a command result")),
    };
    let mut command = json!({
        "evidence_id": subject.evidence_id(),
        "kind": subject.kind(),
        "exit_code": exit_code,
        "passed": passed,
    });
    // U9 K2 (Owner O6): a test run that recorded its `--test-format` is
    // announced by `command.completed@2`, which adds that member; every
    // other run is `command.completed@1`, byte for byte as before.
    let test_format = subject.payload().test_format();
    if let Some(format) = test_format {
        command["test_format"] = json!(format.as_str());
    }
    append_daemon_event_versioned(
        transaction,
        &DaemonEvent {
            event_id: capture.event_id,
            workspace_id: snapshot.workspace_id(),
            session_id: Some(capture.session_id),
            event_type: "command.completed",
            payload: &json!({
                "evidence": ordered,
                "repository_snapshot": {
                    "snapshot_id": snapshot.snapshot_id(),
                    "canonical_digest": snapshot.canonical_digest(),
                },
                "command": command,
            }),
            occurred_at: finished_at.unwrap_or(capture.recorded_at),
            recorded_at: capture.recorded_at,
        },
        if test_format.is_some() {
            COMMAND_COMPLETED_FORMAT_SCHEMA
        } else {
            "command.completed@1"
        },
    )
}

/// U9 K2 (Owner O6): the payload schema of a `command.completed` Event
/// whose `command` names the `test_format` of the test run it records.
const COMMAND_COMPLETED_FORMAT_SCHEMA: &str = "command.completed@2";

/// The Snapshot of a capture with the carried command and test Evidence
/// added to its own (P3-D): the same state, more Evidence, so its digest is
/// recomputed by the domain over the same input format.
fn with_carried_evidence(
    snapshot: &RepositorySnapshot,
    own: &[Evidence],
    carried: Vec<Evidence>,
) -> Result<(RepositorySnapshot, Vec<Evidence>), StoreError> {
    let mut input = snapshot.to_input();
    for item in &carried {
        let ids = match item.kind() {
            EvidenceKind::CommandResult => &mut input.command_evidence_ids,
            EvidenceKind::TestResult => &mut input.test_evidence_ids,
            _ => return Err(constraint("only command and test Evidence is carried over")),
        };
        ids.push(item.evidence_id().clone());
    }
    let mut evidence = own.to_vec();
    evidence.extend(carried);
    let snapshot = RepositorySnapshot::new(input, &evidence)
        .map_err(|error| constraint(format!("carried-over Snapshot is invalid: {error}")))?;
    Ok((snapshot, evidence))
}

/// A Task scoped to a Workspace accepts work only from that Workspace.
fn check_task_scope(
    connection: &Connection,
    task_id: &TaskId,
    workspace_id: &WorkspaceId,
) -> Result<(), StoreError> {
    let scope = connection
        .query_row(
            "SELECT workspace_id FROM tasks WHERE task_id = ?1",
            [task_id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Task"))?;
    // T21 (V1-N-5): the scope the Task Capsule reads, that of the Task's
    // `task.created@1` Event.
    let scope = super::capsule::ledger_scope(connection, task_id.as_str(), scope)?;
    if scope.is_some_and(|scope| scope != workspace_id.as_str()) {
        return Err(StoreError::SessionWorkspaceMismatch);
    }
    Ok(())
}

/// The Session exists, belongs to `workspace_id`, and continues `task_id`.
/// Whether it is still active is left to [`append_event_in`].
fn check_session_continues(
    connection: &Connection,
    session_id: &SessionId,
    task_id: &TaskId,
    workspace_id: &WorkspaceId,
) -> Result<(), StoreError> {
    let session_workspace = connection
        .query_row(
            "SELECT workspace_id FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Session"))?;
    if session_workspace != workspace_id.as_str() {
        return Err(StoreError::SessionWorkspaceMismatch);
    }
    let continued = connection
        .query_row(
            "SELECT task_id FROM task_sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if continued.as_deref() != Some(task_id.as_str()) {
        return Err(StoreError::SessionNotBoundToTask);
    }
    Ok(())
}

fn bundle_idempotency(
    connection: &Connection,
    context: &IdempotencyContext,
) -> Result<Option<StoredIdempotency>, StoreError> {
    connection
        .query_row(
            "SELECT canonical_request_sha256, state, result_record_type, result_record_id
             FROM state_idempotency_records
             WHERE principal_id = ?1 AND operation_id = ?2 AND idempotency_key = ?3",
            params![
                context.principal_id.as_str(),
                context.operation_id.as_str(),
                context.key.as_str(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(StoreError::from)
}

fn recover_bundle_idempotency(
    connection: &Connection,
    proposed: &Checkpoint,
    request_digest: &Sha256Digest,
    record: StoredIdempotency,
) -> Result<Checkpoint, StoreError> {
    let (digest, state, record_type, record_id) = record;
    if digest != request_digest.as_str() {
        return Err(StoreError::IdempotencyConflict);
    }
    if state != "SUCCEEDED" || record_type.as_deref() != Some("Checkpoint") {
        return Err(integrity(
            "invalid Checkpoint idempotency state or result type",
        ));
    }
    let record_id =
        record_id.ok_or_else(|| integrity("Checkpoint idempotency result ID missing"))?;
    let checkpoint_id = CheckpointId::parse(&record_id)
        .map_err(|error| integrity(format!("invalid Checkpoint result ID: {error}")))?;
    if &checkpoint_id != proposed.checkpoint_id() {
        return Err(integrity(
            "idempotency result does not match materialized Checkpoint",
        ));
    }
    checkpoint_by_id(connection, &checkpoint_id)?
        .ok_or_else(|| integrity("Checkpoint idempotency result is missing"))
}

fn validate_bundle(
    connection: &Connection,
    evidence: &[Evidence],
    snapshot: &RepositorySnapshot,
    checkpoint: &Checkpoint,
) -> Result<(), StoreError> {
    let mut supplied_ids = BTreeSet::new();
    for item in evidence {
        Evidence::from_stored(
            item.to_input(),
            item.payload_digest(),
            item.canonical_digest(),
        )
        .map_err(constraint)?;
        if !supplied_ids.insert(item.evidence_id()) {
            return Err(StoreError::ConstraintViolation(
                "duplicate Evidence in bundle".to_owned(),
            ));
        }
    }
    let snapshot_ids = snapshot.all_evidence_ids().collect::<BTreeSet<_>>();
    if supplied_ids != snapshot_ids {
        return Err(StoreError::ConstraintViolation(
            "bundle Evidence must equal RepositorySnapshot refs".to_owned(),
        ));
    }
    RepositorySnapshot::from_stored(
        snapshot.to_input(),
        snapshot.is_dirty(),
        snapshot.canonical_digest(),
        evidence,
    )
    .map_err(constraint)?;
    Checkpoint::from_stored(
        checkpoint.to_input(),
        checkpoint.digest(),
        snapshot,
        evidence,
    )
    .map_err(constraint)?;

    let project_id = connection.query_row(
        "SELECT project_id FROM state_metadata WHERE metadata_key = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    let project_id = ProjectId::parse(&project_id)
        .map_err(|error| integrity(format!("invalid state project_id: {error}")))?;
    if &project_id != snapshot.project_id() {
        return Err(StoreError::ConstraintViolation(
            "RepositorySnapshot project does not match state database".to_owned(),
        ));
    }
    let task_project = connection
        .query_row(
            "SELECT project_id FROM tasks WHERE task_id = ?1",
            [snapshot.task_id().as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Task for RepositorySnapshot"))?;
    let workspace_project = connection
        .query_row(
            "SELECT project_id FROM workspaces WHERE workspace_id = ?1",
            [snapshot.workspace_id().as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Workspace for RepositorySnapshot"))?;
    if task_project != project_id.as_str() || workspace_project != project_id.as_str() {
        return Err(StoreError::ConstraintViolation(
            "Snapshot Task/Workspace project association is invalid".to_owned(),
        ));
    }
    let session_workspace = connection
        .query_row(
            "SELECT workspace_id FROM sessions WHERE session_id = ?1",
            [checkpoint.session_id().as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Session for Checkpoint"))?;
    if session_workspace != snapshot.workspace_id().as_str() {
        return Err(StoreError::ConstraintViolation(
            "Checkpoint Session workspace does not match Snapshot".to_owned(),
        ));
    }
    let linked: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM task_sessions WHERE task_id = ?1 AND session_id = ?2
         )",
        params![
            checkpoint.task_id().as_str(),
            checkpoint.session_id().as_str()
        ],
        |row| row.get(0),
    )?;
    if !linked {
        return Err(StoreError::ConstraintViolation(
            "Checkpoint Session is not bound to Task".to_owned(),
        ));
    }
    Ok(())
}

fn state_project_id(connection: &Connection) -> Result<ProjectId, StoreError> {
    let value = connection.query_row(
        "SELECT project_id FROM state_metadata WHERE metadata_key = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    ProjectId::parse(&value)
        .map_err(|error| integrity(format!("invalid state project_id: {error}")))
}

fn project_event_sequence(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<u64, StoreError> {
    connection
        .query_row(
            "SELECT COUNT(*)
             FROM events AS event
             JOIN sessions AS session ON session.session_id = event.session_id
             JOIN workspaces AS workspace ON workspace.workspace_id = session.workspace_id
             WHERE workspace.project_id = ?1",
            [project_id.as_str()],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn validate_capture_context(
    transaction: &Transaction<'_>,
    capture: &CheckpointCaptureContext,
    snapshot: &RepositorySnapshot,
    checkpoint: &Checkpoint,
) -> Result<(), StoreError> {
    let project_id = state_project_id(transaction)?;
    let workspace = transaction
        .query_row(
            "SELECT project_id, canonical_worktree_path, git_dir
             FROM workspaces WHERE workspace_id = ?1",
            [capture.workspace_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound("Workspace for checkpoint capture"))?;
    let task_project = transaction
        .query_row(
            "SELECT project_id FROM tasks WHERE task_id = ?1",
            [capture.task_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Task for checkpoint capture"))?;
    let repository_digest = transaction.query_row(
        "SELECT repository_common_dir_sha256 FROM state_metadata WHERE metadata_key = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    let current_event_sequence = project_event_sequence(transaction, &project_id)?;
    if project_id != capture.project_id
        || task_project != project_id.as_str()
        || workspace.0 != project_id.as_str()
        || Path::new(&workspace.1) != capture.worktree_root
        || Path::new(&workspace.2) != capture.git_dir
        || repository_digest != capture.repository_common_dir_sha256.as_str()
        || !capture_binding_matches(transaction, capture, snapshot)?
        || current_event_sequence != capture.project_event_sequence
        || checkpoint.project_event_sequence() != capture.project_event_sequence
    {
        return Err(StoreError::ConstraintViolation(
            "checkpoint capture binding or Project Event sequence changed".to_owned(),
        ));
    }
    Ok(())
}

/// Inside the write transaction: the Project, Task, Workspace path, Git
/// directory, and repository binding are still the ones frozen before
/// capture, and the Snapshot names them.
fn capture_binding_matches(
    connection: &Connection,
    capture: &CheckpointCaptureContext,
    snapshot: &RepositorySnapshot,
) -> Result<bool, StoreError> {
    let project_id = state_project_id(connection)?;
    let workspace = connection
        .query_row(
            "SELECT project_id, canonical_worktree_path, git_dir
             FROM workspaces WHERE workspace_id = ?1",
            [capture.workspace_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound("Workspace for checkpoint capture"))?;
    let task_project = connection
        .query_row(
            "SELECT project_id FROM tasks WHERE task_id = ?1",
            [capture.task_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Task for checkpoint capture"))?;
    let repository_digest = connection.query_row(
        "SELECT repository_common_dir_sha256 FROM state_metadata WHERE metadata_key = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    Ok(project_id == capture.project_id
        && task_project == project_id.as_str()
        && workspace.0 == project_id.as_str()
        && Path::new(&workspace.1) == capture.worktree_root
        && Path::new(&workspace.2) == capture.git_dir
        && repository_digest == capture.repository_common_dir_sha256.as_str()
        && snapshot.project_id() == &capture.project_id
        && snapshot.task_id() == &capture.task_id
        && snapshot.workspace_id() == &capture.workspace_id)
}

fn verify_repository_preflight(
    capture: &CheckpointCaptureContext,
    evidence: &[Evidence],
    snapshot: &RepositorySnapshot,
    probe: &RepositoryProbe,
    checkpoint: &Checkpoint,
) -> Result<(), StoreError> {
    if checkpoint.project_event_sequence() != capture.project_event_sequence {
        return Err(StoreError::ConstraintViolation(
            "checkpoint capture context does not match materialized objects".to_owned(),
        ));
    }
    verify_snapshot_preflight(capture, evidence, snapshot, probe)
}

/// Rechecks, outside any transaction, that the repository still matches the
/// materialized Snapshot and its Project/Workspace binding.
fn verify_snapshot_preflight(
    capture: &CheckpointCaptureContext,
    evidence: &[Evidence],
    snapshot: &RepositorySnapshot,
    probe: &RepositoryProbe,
) -> Result<(), StoreError> {
    if snapshot.project_id() != &capture.project_id
        || snapshot.task_id() != &capture.task_id
        || snapshot.workspace_id() != &capture.workspace_id
    {
        return Err(StoreError::ConstraintViolation(
            "checkpoint capture context does not match materialized objects".to_owned(),
        ));
    }
    probe.verify_unchanged().map_err(|error| {
        StoreError::ConstraintViolation(format!(
            "repository changed during checkpoint preflight: {error}"
        ))
    })?;
    let common_digest = Sha256Digest::of_bytes(
        probe
            .repository_common_dir()
            .display()
            .to_string()
            .as_bytes(),
    );
    if probe.worktree_root() != capture.worktree_root
        || probe.git_dir() != capture.git_dir
        || common_digest != capture.repository_common_dir_sha256
    {
        return Err(StoreError::StateBindingMismatch);
    }
    let repository_identity = evidence
        .iter()
        .find_map(|item| {
            (item.evidence_id() == snapshot.git_head_evidence_id()).then(|| match item.subject() {
                EvidenceSubject::GitHead {
                    repository_identity,
                } => Some(repository_identity),
                _ => None,
            })
        })
        .flatten()
        .ok_or_else(|| constraint("Snapshot git_head repository identity is missing"))?;
    if repository_identity != probe.repository_identity() {
        return Err(StoreError::StateBindingMismatch);
    }
    if snapshot.head_oid() != probe.head_oid()
        || snapshot.branch() != probe.branch()
        || snapshot.worktree_status_porcelain_v2() != probe.status_porcelain_v2()
        || snapshot.index_digest() != probe.index_digest()
        || snapshot.tracked_worktree_digest() != probe.tracked_worktree_digest()
        || snapshot.capture_manifest_digest() != probe.capture_manifest_digest()
    {
        return Err(StoreError::ConstraintViolation(
            "repository changed or does not match Project/Workspace binding".to_owned(),
        ));
    }
    Ok(())
}

fn insert_or_replay_evidence(
    transaction: &Transaction<'_>,
    proposed: &Evidence,
) -> Result<(), StoreError> {
    if let Some(existing) = evidence_by_id(transaction, proposed.evidence_id())? {
        if existing == *proposed {
            return Ok(());
        }
        return Err(StoreError::ConstraintViolation(
            "Evidence identity already exists with different canonical fields".to_owned(),
        ));
    }
    let subject = canonical_subject(proposed.subject())?;
    let payload = canonical_payload(proposed.payload())?;
    transaction
        .execute(
            "INSERT INTO evidence (
                evidence_id, task_id, workspace_id, kind, subject, observed_at,
                collector_name, collector_version, payload, payload_digest, canonical_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                proposed.evidence_id().as_str(),
                proposed.task_id().as_str(),
                proposed.workspace_id().as_str(),
                proposed.kind().as_str(),
                subject,
                proposed.observed_at().as_str(),
                proposed.collector().name,
                proposed.collector().version,
                payload,
                proposed.payload_digest().as_str(),
                proposed.canonical_digest().as_str(),
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn insert_snapshot(
    transaction: &Transaction<'_>,
    snapshot: &RepositorySnapshot,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO repository_snapshots (
                snapshot_id, project_id, task_id, workspace_id, head_oid, branch,
                worktree_status_porcelain_v2, capture_format_version, index_digest,
                tracked_worktree_digest, capture_manifest_digest, is_dirty, captured_at,
                canonical_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 2, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                snapshot.snapshot_id().as_str(),
                snapshot.project_id().as_str(),
                snapshot.task_id().as_str(),
                snapshot.workspace_id().as_str(),
                snapshot.head_oid(),
                snapshot.branch(),
                snapshot.worktree_status_porcelain_v2(),
                snapshot.index_digest().as_str(),
                snapshot.tracked_worktree_digest().as_str(),
                snapshot.capture_manifest_digest().as_str(),
                snapshot.is_dirty(),
                snapshot.captured_at().as_str(),
                snapshot.canonical_digest().as_str(),
            ],
        )
        .map_err(map_sqlite_error)?;
    insert_snapshot_ref(
        transaction,
        snapshot,
        snapshot.git_head_evidence_id(),
        "git_head",
        0,
    )?;
    insert_snapshot_ref(
        transaction,
        snapshot,
        snapshot.git_worktree_evidence_id(),
        "git_worktree",
        0,
    )?;
    for (role, ids) in [
        ("file", snapshot.file_evidence_ids()),
        ("command", snapshot.command_evidence_ids()),
        ("test", snapshot.test_evidence_ids()),
    ] {
        for (ordinal, id) in ids.iter().enumerate() {
            insert_snapshot_ref(transaction, snapshot, id, role, ordinal)?;
        }
    }
    Ok(())
}

fn insert_snapshot_ref(
    transaction: &Transaction<'_>,
    snapshot: &RepositorySnapshot,
    evidence_id: &EvidenceId,
    role: &str,
    ordinal: usize,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO repository_snapshot_evidence (
                snapshot_id, evidence_id, role, ordinal
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                snapshot.snapshot_id().as_str(),
                evidence_id.as_str(),
                role,
                ordinal
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn insert_checkpoint(
    transaction: &Transaction<'_>,
    checkpoint: &Checkpoint,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO checkpoints (
                checkpoint_id, task_id, session_id, snapshot_id, project_event_sequence,
                objective, created_at, digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                checkpoint.checkpoint_id().as_str(),
                checkpoint.task_id().as_str(),
                checkpoint.session_id().as_str(),
                checkpoint.snapshot_id().as_str(),
                checkpoint.project_event_sequence(),
                checkpoint.objective(),
                checkpoint.created_at().as_str(),
                checkpoint.digest().as_str(),
            ],
        )
        .map_err(map_sqlite_error)?;
    for (section, statements) in [
        (
            "verified_completed_work",
            checkpoint.verified_completed_work(),
        ),
        ("open_items", checkpoint.open_items()),
        (
            "next_action",
            std::slice::from_ref(checkpoint.next_action()),
        ),
    ] {
        for (ordinal, statement) in statements.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO checkpoint_items (
                        checkpoint_id, section, ordinal, classification, text
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        checkpoint.checkpoint_id().as_str(),
                        section,
                        ordinal,
                        statement.classification().as_str(),
                        statement.text(),
                    ],
                )
                .map_err(map_sqlite_error)?;
            for (evidence_ordinal, evidence_id) in statement.evidence_ids().iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO checkpoint_item_evidence (
                            checkpoint_id, section, item_ordinal, evidence_id, evidence_ordinal
                         ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            checkpoint.checkpoint_id().as_str(),
                            section,
                            ordinal,
                            evidence_id.as_str(),
                            evidence_ordinal,
                        ],
                    )
                    .map_err(map_sqlite_error)?;
            }
        }
    }
    Ok(())
}

fn append_checkpoint_event(
    transaction: &Transaction<'_>,
    evidence: &[Evidence],
    snapshot: &RepositorySnapshot,
    checkpoint: &Checkpoint,
    event_id: &EventId,
    recorded_at: &UtcTimestamp,
) -> Result<Event, StoreError> {
    let ordered_evidence = snapshot
        .all_evidence_ids()
        .map(|id| {
            evidence
                .iter()
                .find(|item| item.evidence_id() == id)
                .ok_or_else(|| constraint("snapshot Evidence missing from bundle"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let payload_text = serde_json::to_string(&json!({
        "evidence": ordered_evidence
            .iter()
            .map(|item| json!({
                "evidence_id": item.evidence_id(),
                "canonical_digest": item.canonical_digest(),
            }))
            .collect::<Vec<_>>(),
        "repository_snapshot": {
            "snapshot_id": snapshot.snapshot_id(),
            "canonical_digest": snapshot.canonical_digest(),
        },
        "checkpoint": {
            "checkpoint_id": checkpoint.checkpoint_id(),
            "digest": checkpoint.digest(),
        },
    }))
    .map_err(|error| constraint(format!("checkpoint Event payload: {error}")))?;
    let payload = CanonicalJson::parse(&payload_text).map_err(constraint)?;
    let workspace_id = transaction
        .query_row(
            "SELECT workspace_id FROM sessions WHERE session_id = ?1",
            [checkpoint.session_id().as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Session for checkpoint Event"))?;
    let workspace_id = WorkspaceId::parse(&workspace_id)
        .map_err(|error| integrity(format!("invalid Session workspace_id: {error}")))?;
    // The daemon is this Event's producer; its producer sequence is the
    // Project sequence the Event is about to take (ADR 0010 D6).
    let input = EventInput {
        event_id: event_id.clone(),
        project_id: state_project_id(transaction)?,
        workspace_id,
        session_id: Some(checkpoint.session_id().clone()),
        event_type: "checkpoint.created".to_owned(),
        payload_schema: "checkpoint.created@1".to_owned(),
        payload,
        occurred_at: Some(checkpoint.created_at().clone()),
        recorded_at: recorded_at.clone(),
        producer: EventProducer {
            name: memprove_domain::DAEMON_PRODUCER_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            sequence: next_project_sequence(transaction)?,
        },
        correlation_id: None,
        causation_id: None,
        privacy_class: PrivacyClass::Internal,
    };
    append_event_in(transaction, &input)
}

type StoredEvidence = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

pub(super) fn evidence_by_id(
    connection: &Connection,
    evidence_id: &EvidenceId,
) -> Result<Option<Evidence>, StoreError> {
    let stored = connection
        .query_row(
            "SELECT evidence_id, task_id, workspace_id, kind, subject, observed_at,
                    collector_name, collector_version, payload, payload_digest, canonical_digest
             FROM evidence WHERE evidence_id = ?1",
            [evidence_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )
        .optional()?;
    stored.map(decode_evidence).transpose()
}

fn decode_evidence(stored: StoredEvidence) -> Result<Evidence, StoreError> {
    let kind = parse_evidence_kind(&stored.3)?;
    let input = EvidenceInput {
        evidence_id: EvidenceId::parse(&stored.0)
            .map_err(|error| integrity(format!("invalid evidence_id: {error}")))?,
        task_id: TaskId::parse(&stored.1)
            .map_err(|error| integrity(format!("invalid Evidence task_id: {error}")))?,
        workspace_id: WorkspaceId::parse(&stored.2)
            .map_err(|error| integrity(format!("invalid Evidence workspace_id: {error}")))?,
        kind,
        subject: serde_json::from_str::<EvidenceSubject>(&stored.4)
            .map_err(|error| integrity(format!("invalid Evidence subject: {error}")))?,
        observed_at: UtcTimestamp::parse(&stored.5)
            .map_err(|error| integrity(format!("invalid Evidence observed_at: {error}")))?,
        collector: EvidenceCollector {
            name: stored.6,
            version: stored.7,
        },
        payload: serde_json::from_str::<EvidencePayload>(&stored.8)
            .map_err(|error| integrity(format!("invalid Evidence payload: {error}")))?,
    };
    let payload_digest = Sha256Digest::parse(&stored.9)
        .map_err(|error| integrity(format!("invalid Evidence payload_digest: {error}")))?;
    let canonical_digest = Sha256Digest::parse(&stored.10)
        .map_err(|error| integrity(format!("invalid Evidence canonical_digest: {error}")))?;
    Evidence::from_stored(input, &payload_digest, &canonical_digest)
        .map_err(|error| integrity(format!("Evidence digest validation: {error}")))
}

fn parse_evidence_kind(value: &str) -> Result<EvidenceKind, StoreError> {
    match value {
        "git_head" => Ok(EvidenceKind::GitHead),
        "git_worktree" => Ok(EvidenceKind::GitWorktree),
        "file_digest" => Ok(EvidenceKind::FileDigest),
        "command_result" => Ok(EvidenceKind::CommandResult),
        "test_result" => Ok(EvidenceKind::TestResult),
        _ => Err(integrity(format!("unknown Evidence kind {value}"))),
    }
}

type StoredSnapshot = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    u64,
    String,
    String,
    String,
    i64,
    String,
    String,
);

pub(super) fn snapshot_by_id(
    connection: &Connection,
    snapshot_id: &RepositorySnapshotId,
) -> Result<Option<RepositorySnapshot>, StoreError> {
    let stored = connection
        .query_row(
            "SELECT snapshot_id, project_id, task_id, workspace_id, head_oid, branch,
                    worktree_status_porcelain_v2, capture_format_version, index_digest,
                    tracked_worktree_digest, capture_manifest_digest, is_dirty, captured_at,
                    canonical_digest
             FROM repository_snapshots WHERE snapshot_id = ?1",
            [snapshot_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ))
            },
        )
        .optional()?;
    stored
        .map(|stored| decode_snapshot(connection, stored))
        .transpose()
}

fn decode_snapshot(
    connection: &Connection,
    stored: StoredSnapshot,
) -> Result<RepositorySnapshot, StoreError> {
    let snapshot_id = RepositorySnapshotId::parse(&stored.0)
        .map_err(|error| integrity(format!("invalid snapshot_id: {error}")))?;
    let mut git_head = Vec::new();
    let mut git_worktree = Vec::new();
    let mut files = Vec::new();
    let mut commands = Vec::new();
    let mut tests = Vec::new();
    let refs = connection
        .prepare(
            "SELECT evidence_id, role, ordinal
             FROM repository_snapshot_evidence
             WHERE snapshot_id = ?1
             ORDER BY CASE role
                WHEN 'git_head' THEN 0 WHEN 'git_worktree' THEN 1 WHEN 'file' THEN 2
                WHEN 'command' THEN 3 WHEN 'test' THEN 4 ELSE 5 END,
                ordinal, evidence_id",
        )?
        .query_map([snapshot_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (id, role, ordinal) in refs {
        let id = EvidenceId::parse(&id)
            .map_err(|error| integrity(format!("invalid Snapshot evidence_id: {error}")))?;
        match role.as_str() {
            "git_head" if ordinal == 0 => git_head.push(id),
            "git_worktree" if ordinal == 0 => git_worktree.push(id),
            "file" => files.push(id),
            "command" => commands.push(id),
            "test" => tests.push(id),
            _ => {
                return Err(integrity(
                    "invalid RepositorySnapshot Evidence role/ordinal",
                ));
            }
        }
    }
    if git_head.len() != 1 || git_worktree.len() != 1 {
        return Err(integrity(
            "RepositorySnapshot requires exactly one git_head and git_worktree Evidence",
        ));
    }
    let input = RepositorySnapshotInput {
        snapshot_id,
        project_id: ProjectId::parse(&stored.1)
            .map_err(|error| integrity(format!("invalid Snapshot project_id: {error}")))?,
        task_id: TaskId::parse(&stored.2)
            .map_err(|error| integrity(format!("invalid Snapshot task_id: {error}")))?,
        workspace_id: WorkspaceId::parse(&stored.3)
            .map_err(|error| integrity(format!("invalid Snapshot workspace_id: {error}")))?,
        head_oid: stored.4,
        branch: stored.5,
        worktree_status_porcelain_v2: stored.6,
        index_digest: Sha256Digest::parse(&stored.8)
            .map_err(|error| integrity(format!("invalid Snapshot index digest: {error}")))?,
        tracked_worktree_digest: Sha256Digest::parse(&stored.9).map_err(|error| {
            integrity(format!("invalid Snapshot tracked worktree digest: {error}"))
        })?,
        capture_manifest_digest: Sha256Digest::parse(&stored.10)
            .map_err(|error| integrity(format!("invalid Snapshot capture manifest: {error}")))?,
        git_head_evidence_id: git_head.remove(0),
        git_worktree_evidence_id: git_worktree.remove(0),
        file_evidence_ids: files,
        command_evidence_ids: commands,
        test_evidence_ids: tests,
        captured_at: UtcTimestamp::parse(&stored.12)
            .map_err(|error| integrity(format!("invalid Snapshot captured_at: {error}")))?,
    };
    check_snapshot_project(connection, &input.project_id)?;
    let canonical_digest = Sha256Digest::parse(&stored.13)
        .map_err(|error| integrity(format!("invalid Snapshot digest: {error}")))?;
    let evidence = input
        .all_ids_for_store()
        .map(|id| {
            evidence_by_id(connection, id)?
                .ok_or_else(|| integrity("RepositorySnapshot referenced Evidence is missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let is_dirty = stored_is_dirty(stored.11)?;
    match stored.7 {
        1 => {
            RepositorySnapshot::from_stored_legacy_v1(input, is_dirty, &canonical_digest, &evidence)
        }
        2 => RepositorySnapshot::from_stored(input, is_dirty, &canonical_digest, &evidence),
        version => {
            return Err(integrity(format!(
                "unknown RepositorySnapshot capture format {version}"
            )));
        }
    }
    .map_err(|error| integrity(format!("RepositorySnapshot validation: {error}")))
}

/// U1 B3 (T21 V1R-4): a Snapshot's `is_dirty` is 0 or 1; any other
/// integer, which the column's CHECK refuses, does not decode (before: read
/// as dirty, like 1, when the porcelain status was not empty).
fn stored_is_dirty(value: i64) -> Result<bool, StoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(integrity(format!("invalid Snapshot is_dirty {value}"))),
    }
}

/// U1 B1 (T21 V1R-1): a Snapshot names the Project of the state database,
/// which the schema's insert trigger enforces and no digest covers; one
/// that names another does not decode.
fn check_snapshot_project(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<(), StoreError> {
    let state_project = state_project_id(connection)?;
    if *project_id != state_project {
        return Err(integrity(format!(
            "Snapshot project_id {project_id} is not the Project of the state database, \
             {state_project}"
        )));
    }
    Ok(())
}

trait SnapshotInputIds {
    fn all_ids_for_store(&self) -> Box<dyn Iterator<Item = &EvidenceId> + '_>;
}

impl SnapshotInputIds for RepositorySnapshotInput {
    fn all_ids_for_store(&self) -> Box<dyn Iterator<Item = &EvidenceId> + '_> {
        Box::new(
            std::iter::once(&self.git_head_evidence_id)
                .chain(std::iter::once(&self.git_worktree_evidence_id))
                .chain(self.file_evidence_ids.iter())
                .chain(self.command_evidence_ids.iter())
                .chain(self.test_evidence_ids.iter()),
        )
    }
}

type StoredCheckpoint = (String, String, String, String, u64, String, String, String);

pub(super) fn checkpoint_by_id(
    connection: &Connection,
    checkpoint_id: &CheckpointId,
) -> Result<Option<Checkpoint>, StoreError> {
    let stored = connection
        .query_row(
            "SELECT checkpoint_id, task_id, session_id, snapshot_id, project_event_sequence,
                    objective, created_at, digest
             FROM checkpoints WHERE checkpoint_id = ?1",
            [checkpoint_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    stored
        .map(|stored| decode_checkpoint(connection, stored))
        .transpose()
}

fn decode_checkpoint(
    connection: &Connection,
    stored: StoredCheckpoint,
) -> Result<Checkpoint, StoreError> {
    let checkpoint_id = CheckpointId::parse(&stored.0)
        .map_err(|error| integrity(format!("invalid checkpoint_id: {error}")))?;
    let snapshot_id = RepositorySnapshotId::parse(&stored.3)
        .map_err(|error| integrity(format!("invalid Checkpoint snapshot_id: {error}")))?;
    let snapshot = snapshot_by_id(connection, &snapshot_id)?
        .ok_or_else(|| integrity("Checkpoint RepositorySnapshot is missing"))?;
    let raw_items = connection
        .prepare(
            "SELECT section, ordinal, classification, text
             FROM checkpoint_items WHERE checkpoint_id = ?1
             ORDER BY CASE section
                WHEN 'verified_completed_work' THEN 0 WHEN 'open_items' THEN 1
                WHEN 'next_action' THEN 2 ELSE 3 END,
                ordinal",
        )?
        .query_map([checkpoint_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut completed = Vec::new();
    let mut open = Vec::new();
    let mut next = Vec::new();
    for (section, ordinal, classification, text) in raw_items {
        let evidence_ids = connection
            .prepare(
                "SELECT evidence_id FROM checkpoint_item_evidence
                 WHERE checkpoint_id = ?1 AND section = ?2 AND item_ordinal = ?3
                 ORDER BY evidence_ordinal, evidence_id",
            )?
            .query_map(params![checkpoint_id.as_str(), section, ordinal], |row| {
                row.get::<_, String>(0)
            })?
            .map(|result| {
                let value = result?;
                EvidenceId::parse(&value)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let parsed_classification = parse_classification(&classification)?;
        let statement = CheckpointStatement::new(parsed_classification, text, evidence_ids.clone())
            .map_err(|error| integrity(format!("invalid CheckpointStatement: {error}")))?;
        if statement.evidence_ids() != evidence_ids {
            return Err(integrity(
                "Checkpoint Evidence refs are not canonically ordered",
            ));
        }
        match section.as_str() {
            "verified_completed_work" if usize::try_from(ordinal).ok() == Some(completed.len()) => {
                completed.push(statement);
            }
            "open_items" if usize::try_from(ordinal).ok() == Some(open.len()) => {
                open.push(statement);
            }
            "next_action" if ordinal == 0 => next.push(statement),
            _ => return Err(integrity("Checkpoint item section or ordinal is invalid")),
        }
    }
    if next.len() != 1 {
        return Err(integrity("Checkpoint requires exactly one next_action"));
    }
    let input = CheckpointInput {
        checkpoint_id,
        task_id: TaskId::parse(&stored.1)
            .map_err(|error| integrity(format!("invalid Checkpoint task_id: {error}")))?,
        session_id: SessionId::parse(&stored.2)
            .map_err(|error| integrity(format!("invalid Checkpoint session_id: {error}")))?,
        snapshot_id,
        project_event_sequence: stored.4,
        objective: stored.5,
        verified_completed_work: completed,
        open_items: open,
        next_action: next.remove(0),
        created_at: UtcTimestamp::parse(&stored.6)
            .map_err(|error| integrity(format!("invalid Checkpoint created_at: {error}")))?,
    };
    let digest = Sha256Digest::parse(&stored.7)
        .map_err(|error| integrity(format!("invalid Checkpoint digest: {error}")))?;
    let evidence = snapshot
        .all_evidence_ids()
        .map(|id| {
            evidence_by_id(connection, id)?
                .ok_or_else(|| integrity("Checkpoint Snapshot Evidence is missing"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Checkpoint::from_stored(input, &digest, &snapshot, &evidence)
        .map_err(|error| integrity(format!("Checkpoint validation: {error}")))
}

fn parse_classification(value: &str) -> Result<CheckpointClassification, StoreError> {
    match value {
        "verified_fact" => Ok(CheckpointClassification::VerifiedFact),
        "user_supplied_context" => Ok(CheckpointClassification::UserSuppliedContext),
        "recommendation" => Ok(CheckpointClassification::Recommendation),
        "unknown" => Ok(CheckpointClassification::Unknown),
        _ => Err(integrity(format!(
            "unknown Checkpoint classification {value}"
        ))),
    }
}

fn canonical_subject(value: &EvidenceSubject) -> Result<String, StoreError> {
    let raw = serde_json::to_string(value)
        .map_err(|error| constraint(format!("typed recovery JSON serialization: {error}")))?;
    CanonicalJson::parse(&raw)
        .map(|canonical| canonical.as_str().to_owned())
        .map_err(constraint)
}

fn canonical_payload(value: &EvidencePayload) -> Result<String, StoreError> {
    let raw = serde_json::to_string(value)
        .map_err(|error| constraint(format!("typed recovery JSON serialization: {error}")))?;
    CanonicalJson::parse(&raw)
        .map(|canonical| canonical.as_str().to_owned())
        .map_err(constraint)
}

fn constraint(error: impl std::fmt::Display) -> StoreError {
    StoreError::ConstraintViolation(error.to_string())
}

fn integrity(error: impl Into<String>) -> StoreError {
    StoreError::PersistedDataIntegrity(error.into())
}

impl From<RecoveryDomainError> for StoreError {
    fn from(error: RecoveryDomainError) -> Self {
        integrity(error.to_string())
    }
}
