//! The two writes of ADR 0013 P3-C: `claim.superseded` edges declared by a
//! Checkpoint or a Decision, and `proposeDecision`'s `decision.recorded`.
//!
//! Every edge is checked against the Task's readable claims, read in the
//! write transaction (the Capsule's own ledger view), before anything is
//! written; the edge Events follow the Event of the claim that declares
//! them in the same transaction, so an idempotent replay, which returns the
//! recorded result first, never appends one again.

use memprove_domain::{
    ClaimEdge, ClaimId, ClaimKind, DAEMON_PRODUCER_NAME, DecisionPayload, EdgeRefusal,
    EdgeRelation, EdgeRule, Event, EventId, EvidenceId, IdempotencyContext, ProposedEdge,
    RoleWrite, SessionId, Sha256Digest, TaskCapsuleView, TaskId, UtcTimestamp, WorkspaceId,
    check_edges, role_refusal,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::{
    DaemonEvent, StateStore, append_daemon_event_in,
    capsule::capsule_view,
    preview,
    recovery::{evidence_by_id, record_result, recorded_result},
};
use crate::StoreError;

/// Inputs of one Decision (ADR 0013 D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionDraft {
    /// Identity of the `decision.recorded` Event, and so of the Decision.
    pub decision_id: EventId,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub payload: DecisionPayload,
    /// The Decision's `supersedes`, in request order.
    pub edges: Vec<ProposedEdge>,
    pub recorded_at: UtcTimestamp,
}

/// One recorded Decision, read back from its Events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    pub decision_id: EventId,
    pub project_sequence: u64,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub payload: DecisionPayload,
    /// The claims it supersedes, in recorded order.
    pub supersedes: Vec<ClaimId>,
    pub recorded_at: UtcTimestamp,
}

impl StateStore {
    /// Records one Decision: its `decision.recorded` Event and one
    /// `claim.superseded` Event per edge, in one transaction, or replays the
    /// result of the same idempotent request (ADR 0013 D2, P3-C).
    ///
    /// # Errors
    ///
    /// [`StoreError::IdempotencyConflict`], [`StoreError::NotFound`] for an
    /// unknown Task or Session, [`StoreError::SessionNotBoundToTask`],
    /// [`StoreError::SessionNotActive`], [`StoreError::RecoveryInputRejected`]
    /// for Evidence or an edge that cannot be recorded, or another
    /// [`StoreError`].
    pub fn record_decision(
        &mut self,
        draft: &DecisionDraft,
        idempotency: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<DecisionRow, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(decision_id) =
            recorded_result(&transaction, idempotency, request_digest, "Decision")?
        {
            let decision_id = EventId::parse(&decision_id)
                .map_err(|error| integrity(format!("invalid Decision result ID: {error}")))?;
            let replayed = decision_row(&transaction, &decision_id)?;
            capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(replayed);
        }
        if super::task_by_id(&transaction, &draft.task_id)?.is_none() {
            return Err(StoreError::NotFound("Task"));
        }
        let workspace_id = session_of_task(&transaction, &draft.session_id, &draft.task_id)?;
        check_evidence(&transaction, &draft.task_id, &draft.payload.evidence_ids)?;
        check_declared_edges(
            &transaction,
            &draft.task_id,
            &draft.session_id,
            &[(
                ClaimId::Decision(draft.decision_id.clone()),
                ClaimKind::Decision,
            )],
            Declared::Decision(&draft.edges),
        )?;
        let payload = draft.payload.payload(&draft.task_id);
        let event = append_daemon_event_in(
            &transaction,
            &DaemonEvent {
                event_id: &draft.decision_id,
                workspace_id: &workspace_id,
                session_id: Some(&draft.session_id),
                event_type: "decision.recorded",
                payload: &payload,
                occurred_at: &draft.recorded_at,
                recorded_at: &draft.recorded_at,
            },
        )?;
        append_edges(
            &transaction,
            &draft.task_id,
            &draft.session_id,
            &workspace_id,
            &draft.edges,
            &draft.recorded_at,
        )?;
        record_result(
            &transaction,
            idempotency,
            request_digest,
            "Decision",
            draft.decision_id.as_str(),
            &draft.recorded_at,
        )?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(DecisionRow {
            decision_id: draft.decision_id.clone(),
            project_sequence: event.project_sequence(),
            task_id: draft.task_id.clone(),
            session_id: draft.session_id.clone(),
            payload: draft.payload.clone(),
            supersedes: draft.edges.iter().map(|edge| edge.target.clone()).collect(),
            recorded_at: draft.recorded_at.clone(),
        })
    }
}

fn integrity(message: impl Into<String>) -> StoreError {
    StoreError::InvalidPersistedValue(message.into())
}

/// The Workspace of an active Session that continues the Task.
fn session_of_task(
    connection: &Connection,
    session_id: &SessionId,
    task_id: &TaskId,
) -> Result<WorkspaceId, StoreError> {
    let (workspace, active) = connection
        .query_row(
            "SELECT workspace_id, ended_at IS NULL FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()?
        .ok_or(StoreError::NotFound("Session"))?;
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
    if !active {
        return Err(StoreError::SessionNotActive);
    }
    WorkspaceId::parse(&workspace).map_err(|error| integrity(error.to_string()))
}

/// Every Evidence object a Decision cites is recorded, recomputes, and
/// belongs to the Task (P3-C).
fn check_evidence(
    connection: &Connection,
    task_id: &TaskId,
    evidence_ids: &[EvidenceId],
) -> Result<(), StoreError> {
    for (index, evidence_id) in evidence_ids.iter().enumerate() {
        let at = format!("$.evidence_ids[{index}]");
        let evidence = match evidence_by_id(connection, evidence_id) {
            Ok(Some(evidence)) => evidence,
            Ok(None) => {
                return Err(StoreError::RecoveryInputRejected(format!(
                    "{at} names no recorded Evidence"
                )));
            }
            Err(error) if preview::is_integrity_failure(&error) => {
                return Err(StoreError::RecoveryInputRejected(format!(
                    "{at} names Evidence whose digest does not recompute"
                )));
            }
            Err(error) => return Err(error),
        };
        if evidence.task_id() != task_id {
            return Err(StoreError::RecoveryInputRejected(format!(
                "{at} names Evidence of another Task; a Decision cites Evidence of its own Task"
            )));
        }
    }
    Ok(())
}

/// The write whose edges [`check_declared_edges`] checks.
#[derive(Debug, Clone, Copy)]
pub(super) enum Declared<'a> {
    /// A Decision and the edges it declares.
    Decision(&'a [ProposedEdge]),
    /// The edges a Checkpoint declares.
    Checkpoint(&'a [ProposedEdge]),
}

/// Checks the edges a write declares against the Task's readable claims in
/// this transaction; a refusal becomes a `400` that names the location and
/// the rule. Then U9 R1 (Owner O4; ADR 0020): a Decision of a `sub_agent`
/// Session, and an edge of one whose target another agent wrote and is
/// not an open item, are [`StoreError::RoleForbidden`], judged on the same
/// ledger view (its `sessions[].agent` and each claim's Session).
pub(super) fn check_declared_edges(
    connection: &Connection,
    task_id: &TaskId,
    session_id: &SessionId,
    new_claims: &[(ClaimId, ClaimKind)],
    declared: Declared<'_>,
) -> Result<(), StoreError> {
    let (edges, write) = match declared {
        Declared::Decision(edges) => (edges, RoleWrite::Decision),
        // A Checkpoint without edges breaks no rule of R1, so it reads no
        // view, as before.
        Declared::Checkpoint([]) => return Ok(()),
        Declared::Checkpoint(edges) => (edges, RoleWrite::Edges(edges)),
    };
    let view = capsule_view(connection, task_id)?;
    if !edges.is_empty() {
        check_edges(&view, new_claims, edges).map_err(|refusal| {
            StoreError::RecoveryInputRejected(refused(connection, &view, &refusal))
        })?;
    }
    match role_refusal(&view, session_id, write) {
        Some(rule) => Err(StoreError::RoleForbidden(rule)),
        None => Ok(()),
    }
}

/// The `detail` of a refused edge; a target that is not a readable claim of
/// the Task is told apart: another Task's claim, a claim that cannot be
/// read, or no recorded claim.
fn refused(connection: &Connection, view: &TaskCapsuleView, refusal: &EdgeRefusal) -> String {
    if refusal.rule != EdgeRule::NotAClaim {
        return refusal.detail();
    }
    let at = &refusal.at;
    let rule = match &refusal.target {
        ClaimId::Statement(claim) => {
            let owner = connection
                .query_row(
                    "SELECT task_id FROM checkpoints WHERE checkpoint_id = ?1",
                    [claim.checkpoint_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten();
            match owner {
                None => "names no recorded claim",
                Some(owner) if owner != view.task.task_id.as_str() => {
                    "names a claim of another Task; edges stay within one Task"
                }
                Some(_)
                    if view.checkpoints.iter().any(|record| {
                        record.checkpoint.checkpoint_id() == &claim.checkpoint_id
                    }) =>
                {
                    "names no statement of that Checkpoint"
                }
                Some(_) => "names a claim of a Checkpoint that cannot be read",
            }
        }
        ClaimId::Decision(event_id) => {
            let decision = connection
                .query_row(
                    "SELECT payload FROM events
                     WHERE event_id = ?1 AND event_type = 'decision.recorded'
                       AND producer_name = ?2",
                    [event_id.as_str(), DAEMON_PRODUCER_NAME],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
                .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok());
            match decision {
                None => "names no recorded claim",
                Some(payload)
                    if payload.get("task_id").and_then(serde_json::Value::as_str)
                        != Some(view.task.task_id.as_str()) =>
                {
                    "names a claim of another Task; edges stay within one Task"
                }
                Some(_) => "names a Decision that cannot be read",
            }
        }
    };
    format!("{at} {rule}")
}

/// Appends one `claim.superseded` Event per edge, in order, after the Event
/// of the claim that declares them.
pub(super) fn append_edges(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    session_id: &SessionId,
    workspace_id: &WorkspaceId,
    edges: &[ProposedEdge],
    recorded_at: &UtcTimestamp,
) -> Result<Vec<Event>, StoreError> {
    edges
        .iter()
        .map(|edge| {
            append_daemon_event_in(
                transaction,
                &DaemonEvent {
                    event_id: &EventId::new(),
                    workspace_id,
                    session_id: Some(session_id),
                    event_type: "claim.superseded",
                    payload: &ClaimEdge::payload(task_id, edge),
                    occurred_at: recorded_at,
                    recorded_at,
                },
            )
        })
        .collect()
}

/// A recorded Decision read back from its `decision.recorded` Event and the
/// `claim.superseded` Events it declared.
fn decision_row(connection: &Connection, decision_id: &EventId) -> Result<DecisionRow, StoreError> {
    let event = super::event_by_id(connection, decision_id)?
        .ok_or_else(|| integrity("Decision idempotency result Event is missing"))?;
    let payload: serde_json::Value = serde_json::from_str(event.payload().as_str())
        .map_err(|error| integrity(error.to_string()))?;
    let task_id = payload
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| TaskId::parse(id).ok())
        .ok_or_else(|| integrity("decision.recorded payload has no Task"))?;
    let decoded = DecisionPayload::from_payload(&task_id, &payload)
        .ok_or_else(|| integrity("decision.recorded payload does not decode"))?;
    let session_id = event
        .session_id()
        .cloned()
        .ok_or_else(|| integrity("decision.recorded has no Session"))?;
    let by = ClaimId::Decision(decision_id.clone());
    let supersedes = connection
        .prepare(
            "SELECT project_sequence, payload FROM events
             WHERE event_type = 'claim.superseded' AND producer_name = ?1
               AND payload_schema = 'claim.superseded@1' AND project_sequence > ?2
             ORDER BY project_sequence",
        )?
        .query_map(
            rusqlite::params![DAEMON_PRODUCER_NAME, event.project_sequence()],
            |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|(sequence, raw)| {
            let value = serde_json::from_str(&raw).ok()?;
            ClaimEdge::from_payload(sequence, &task_id, &value)
        })
        .filter(|edge| edge.relation == EdgeRelation::Supersedes && edge.by.as_ref() == Some(&by))
        .map(|edge| edge.target)
        .collect();
    Ok(DecisionRow {
        decision_id: decision_id.clone(),
        project_sequence: event.project_sequence(),
        task_id,
        session_id,
        payload: decoded,
        supersedes,
        recorded_at: event.recorded_at().clone(),
    })
}
