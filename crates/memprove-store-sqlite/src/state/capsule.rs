//! The read-only ledger view behind `getTaskCapsule` (ADR 0013 D4).
//!
//! One read transaction reads the whole ledger of one Task at Project
//! sequence N: its Sessions and how each ended, every Checkpoint the ledger
//! records for it with Snapshot and Evidence, every Snapshot of the Task,
//! the edges and Decisions, and every record that is missing, does not
//! recompute, or disagrees with `memproved`'s Events (D7). Integrity
//! failures are reported in the view, never as errors; nothing is written.
//!
//! `claim.superseded`, `decision.recorded`, and `session.crashed` count only
//! when `memproved` wrote them on a Session that continues the Task, the
//! criterion of every other daemon Event (D8), with `payload_schema`
//! `<type>@1` (P3-C2), and `session.crashed` also with
//! `session.crashed@2` (U9 A7), which names who ended the Session; a
//! same-named Event a client wrote before they were reserved is ignored. A ledger written by a binary older than P2-E, when a
//! client could still name itself `memproved`, is not supported (Owner
//! decision, P3-C2).
//!
//! P3-C2: the damaged records are the one Task-scope integrity check the
//! Recovery Preview applies too ([`task_integrity`]).
//!
//! P3-D3 (Owner, T14 S-2): that check also holds every Snapshot and Evidence
//! object the ledger announces on a Session of the Task to its owner: the
//! row must exist, recompute, and still belong to the Task and to the
//! Workspace the announcing Event was recorded in. A record moved to
//! another Task or Workspace is named in `damaged_records`.

use std::collections::{BTreeMap, BTreeSet};

use memprove_domain::{
    CheckpointId, CheckpointRecord, ClaimEdge, DAEMON_PRODUCER_NAME, DamagedRecord,
    DecisionPayload, DecisionRecord, EventId, Evidence, EvidenceId, LedgerRecordType, ReasonCode,
    RecordedRun, RepositorySnapshot, RepositorySnapshotId, ResumedFrom,
    SESSION_CRASHED_ENDED_BY_SCHEMA, SESSION_STARTED_AGENT_SCHEMA, SealedEvidence, Session,
    SessionEnd, SessionEndedBy, SessionId, SessionRecord, Task, TaskCapsuleView, TaskId,
    UtcTimestamp, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use super::{
    StateStore, latest_project_sequence,
    ledger::{LedgerReferences, state_project, stored_sequence, task_chains},
    lossy_optional_text, lossy_text,
    membership::{damaged_record_id, started_agent, task_for_view, task_sessions},
    preview::{chain_valid, is_integrity_failure},
    recovery::{checkpoint_by_id, evidence_by_id, sealing_snapshot_ids, snapshot_by_id},
    stored_json,
};
use crate::StoreError;

impl StateStore {
    /// Reads the whole ledger view of `task_id` for a Task Capsule in one
    /// read transaction (ADR 0013 D4).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] for an unknown Task, or a database failure.
    pub fn task_capsule_view(&self, task_id: &TaskId) -> Result<TaskCapsuleView, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let view = capsule_view(&transaction, task_id)?;
        transaction.commit()?;
        Ok(view)
    }
}

/// Collects the damaged records of one view.
#[derive(Default)]
struct Damage(BTreeSet<DamagedRecord>);

impl Damage {
    fn note(&mut self, record_type: LedgerRecordType, record_id: &str, reason: ReasonCode) {
        self.0.insert(DamagedRecord {
            record_type,
            record_id: damaged_record_id(record_id),
            reason,
        });
    }

    fn missing(&mut self, record_type: LedgerRecordType, record_id: &str) {
        self.note(record_type, record_id, ReasonCode::ObjectMissing);
    }

    fn mismatch(&mut self, record_type: LedgerRecordType, record_id: &str) {
        self.note(record_type, record_id, ReasonCode::ObjectDigestMismatch);
    }

    /// Notes the Snapshot and Evidence of `snapshot` that disagree with the
    /// ledger; `true` when any does.
    fn sealed(
        &mut self,
        references: &LedgerReferences,
        snapshot: &RepositorySnapshot,
        evidence: &[Evidence],
    ) -> bool {
        let (snapshot_failure, evidence_failures) = references.sealed_failures(snapshot, evidence);
        if snapshot_failure.is_some() {
            self.mismatch(LedgerRecordType::Snapshot, snapshot.snapshot_id().as_str());
        }
        for (id, _) in &evidence_failures {
            self.mismatch(LedgerRecordType::Evidence, id.as_str());
        }
        snapshot_failure.is_some() || !evidence_failures.is_empty()
    }
}

pub(super) fn capsule_view(
    connection: &Connection,
    task_id: &TaskId,
) -> Result<TaskCapsuleView, StoreError> {
    // P4-C2 (Owner, T19 SF-1): a Task row that does not decode is a damaged
    // record, not an error; the Sessions are the ones the chain names, and
    // every row behind them that disagrees is a damaged record.
    let mut damaged = BTreeSet::new();
    let task = task_for_view(connection, task_id, &mut damaged)?;
    let project_sequence = latest_project_sequence(connection)?;
    let ledger_chain_valid = chain_valid(connection)?;
    let references = LedgerReferences::read(connection)?;
    let membership = task_sessions(connection, task_id)?;
    let mut sessions = membership.sessions;
    sessions.sort_by(|left, right| {
        (left.started_at.as_str(), left.session_id.as_str())
            .cmp(&(right.started_at.as_str(), right.session_id.as_str()))
    });
    let records = task_records(connection, &task, &references, &membership.ids)?;
    damaged.extend(membership.damaged);
    damaged.extend(records.damage.0);
    let sessions = sessions
        .into_iter()
        .map(|session| {
            Ok(SessionRecord {
                end: records
                    .ends
                    .get(session.session_id.as_str())
                    .copied()
                    .unwrap_or(SessionEnd::Open),
                last_recorded_at: last_recorded_at(connection, &session.session_id)?,
                resumed_from: records.resumed.get(session.session_id.as_str()).cloned(),
                session,
            })
        })
        .collect::<Result<_, StoreError>>()?;
    Ok(TaskCapsuleView {
        project_sequence,
        ledger_chain_valid,
        task,
        sessions,
        checkpoints: records.checkpoints,
        decisions: records.decisions,
        edges: records.edges,
        runs: records.runs,
        damaged_records: damaged,
    })
}

/// P3-C2 (Owner, T12 S-4/N1): the one Task-scope integrity check the Task
/// Capsule and the Recovery Preview share. Every record of the Task that is
/// missing, does not recompute, or disagrees with `memproved`'s Events:
/// every Checkpoint, Snapshot, and Evidence object of the Task (by row, and
/// by every Event on a Session of the Task that announces one, P3-D3: which
/// must still own it), the Evidence its Decisions cite, and the Task row
/// against its last Task Event. Whatever `verifyLedger` finds about this
/// Task is here. Also returns the Task's readable runs, for its
/// [`memprove_domain::RunHistory`] (P3-D3).
pub(super) fn task_integrity(
    connection: &Connection,
    task: &Task,
    references: &LedgerReferences,
    task_sessions: &BTreeSet<String>,
) -> Result<(BTreeSet<DamagedRecord>, Vec<RecordedRun>), StoreError> {
    let records = task_records(connection, task, references, task_sessions)?;
    Ok((records.damage.0, records.runs))
}

/// P3-D3: every readable Snapshot of the Task that agrees with the ledger,
/// as the Task Capsule reads its runs, for the Checkpoint write check.
pub(super) fn task_runs(
    connection: &Connection,
    task_id: &TaskId,
) -> Result<Vec<RecordedRun>, StoreError> {
    let references = LedgerReferences::read(connection)?;
    runs(connection, task_id, &references, &mut Damage::default())
}

/// What the Task's records hold: the readable ones, the edges and
/// Decisions, how each Session ended, and every damaged record.
struct TaskRecords {
    checkpoints: Vec<CheckpointRecord>,
    runs: Vec<RecordedRun>,
    decisions: Vec<DecisionRecord>,
    edges: Vec<ClaimEdge>,
    ends: BTreeMap<String, SessionEnd>,
    resumed: BTreeMap<String, ResumedFrom>,
    damage: Damage,
}

fn task_records(
    connection: &Connection,
    task: &Task,
    references: &LedgerReferences,
    task_sessions: &BTreeSet<String>,
) -> Result<TaskRecords, StoreError> {
    let task_id = &task.task_id;
    let mut damage = Damage::default();
    let checkpoints = checkpoints(connection, task_id, task_sessions, references, &mut damage)?;
    let runs = runs(connection, task_id, references, &mut damage)?;
    task_evidence(connection, task_id, references, &mut damage)?;
    announced_records(connection, task_id, references, task_sessions, &mut damage)?;
    task_row(connection, task, &mut damage)?;
    let daemon_events = daemon_events(connection, task_sessions)?;
    let edges = daemon_events
        .iter()
        .filter(|event| event.event_type == "claim.superseded")
        .filter_map(|event| ClaimEdge::from_payload(event.sequence, task_id, &event.payload))
        .collect();
    let mut decisions = Vec::new();
    for event in daemon_events
        .iter()
        .filter(|event| event.event_type == "decision.recorded")
    {
        let Some(payload) = DecisionPayload::from_payload(task_id, &event.payload) else {
            continue;
        };
        let mut damaged = false;
        let evidence = payload
            .evidence_ids
            .iter()
            .map(|id| {
                // T12 N4: Evidence of another Task is not read; the
                // Decision cannot rest on it.
                if evidence_task(connection, id)?.is_some_and(|owner| owner != task_id.as_str()) {
                    return Ok(SealedEvidence {
                        evidence_id: id.clone(),
                        sealed: None,
                        other_task: true,
                    });
                }
                let sealed = sealed_evidence(connection, id, references, &mut damage)?;
                damaged |= sealed.as_ref().is_none_or(|(_, damaged)| *damaged);
                Ok(SealedEvidence {
                    evidence_id: id.clone(),
                    sealed: sealed.map(|(sealed, _)| sealed),
                    other_task: false,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        decisions.push(DecisionRecord {
            decision_id: event.event_id.clone(),
            project_sequence: event.sequence,
            session_id: event.session_id.clone(),
            payload,
            evidence,
            damaged,
        });
    }
    let ends = daemon_events
        .iter()
        .filter_map(|event| match event.event_type.as_str() {
            "session.ended" => Some((event.session_id.as_str(), SessionEnd::Ended)),
            // `session.crashed@1` (P3-D): `{"reason", "outcome": "abandoned"}`;
            // U9 A7: `@2` adds `ended_by`.
            "session.crashed" if crashed_payload(&event.payload_schema, &event.payload) => {
                Some((event.session_id.as_str(), SessionEnd::Crashed))
            }
            _ => None,
        })
        .fold(BTreeMap::new(), |mut ends, (session_id, end)| {
            let entry = ends.entry(session_id.to_owned()).or_insert(end);
            *entry = (*entry).max(end);
            ends
        });
    // ADR 0014 E6 (P4-F): the Pack each Session declared it continues
    // from, the first `capsule.recovered@1` of `memproved` on it.
    let mut resumed = BTreeMap::new();
    for event in daemon_events
        .iter()
        .filter(|event| event.event_type == "capsule.recovered")
    {
        if let Some(from) = ResumedFrom::from_payload(task_id, &event.payload) {
            resumed
                .entry(event.session_id.as_str().to_owned())
                .or_insert(from);
        }
    }
    Ok(TaskRecords {
        checkpoints,
        runs,
        decisions,
        edges,
        ends,
        resumed,
        damage,
    })
}

/// The Task that owns a stored Evidence row, without reading the record.
fn evidence_task(
    connection: &Connection,
    evidence_id: &EvidenceId,
) -> Result<Option<String>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT task_id FROM evidence WHERE evidence_id = ?1",
            [evidence_id.as_str()],
            |row| lossy_text(row, 0),
        )
        .optional()?)
}

/// Every Evidence row of the Task recomputes and agrees with the ledger,
/// whether or not a Snapshot of the Task still references it (P3-C2).
fn task_evidence(
    connection: &Connection,
    task_id: &TaskId,
    references: &LedgerReferences,
    damage: &mut Damage,
) -> Result<(), StoreError> {
    let ids = connection
        .prepare("SELECT evidence_id FROM evidence WHERE task_id = ?1")?
        .query_map([task_id.as_str()], |row| lossy_text(row, 0))?
        .collect::<Result<Vec<_>, _>>()?;
    for raw in ids {
        let Ok(id) = EvidenceId::parse(&raw) else {
            damage.mismatch(LedgerRecordType::Evidence, &raw);
            continue;
        };
        match evidence_by_id(connection, &id) {
            Ok(Some(evidence)) => {
                if references
                    .evidence_failure(evidence.evidence_id(), evidence.canonical_digest())
                    .is_some()
                {
                    damage.mismatch(LedgerRecordType::Evidence, &raw);
                }
            }
            Ok(None) => damage.missing(LedgerRecordType::Evidence, &raw),
            Err(error) if is_integrity_failure(&error) => {
                damage.mismatch(LedgerRecordType::Evidence, &raw);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Every Snapshot and Evidence object an Event on a Session of the Task
/// announces must still be stored (P2-K, P3-C2): a deleted run is named.
/// P3-D3 (Owner, T14 S-2): and still be the Task's, in the Workspace the
/// Event was recorded in, and recompute. A row the Task still owns is read
/// and recomputed by [`runs`] and [`task_evidence`]; a row moved to another
/// Task or Workspace is named here, whether or not it still recomputes.
fn announced_records(
    connection: &Connection,
    task_id: &TaskId,
    references: &LedgerReferences,
    task_sessions: &BTreeSet<String>,
    damage: &mut Damage,
) -> Result<(), StoreError> {
    for (record_type, id, workspace_id) in references.records_announced_on(task_sessions) {
        let query = match record_type {
            LedgerRecordType::Snapshot => {
                "SELECT task_id, workspace_id FROM repository_snapshots WHERE snapshot_id = ?1"
            }
            _ => "SELECT task_id, workspace_id FROM evidence WHERE evidence_id = ?1",
        };
        let owner = connection
            .query_row(query, [id], |row| {
                Ok((lossy_text(row, 0)?, lossy_text(row, 1)?))
            })
            .optional()?;
        match owner {
            None => damage.missing(record_type, id),
            Some((owner_task, owner_workspace))
                if owner_task != task_id.as_str()
                    || workspace_id.is_some_and(|recorded| recorded != owner_workspace) =>
            {
                damage.mismatch(record_type, id);
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// The Task row against its last `task.created` or `task.transitioned`
/// Event of `memproved`, as `verifyLedger` compares it (T12 N1). U1 B2
/// (T21 V1R-3): a Task with no Task Event is not the chain's, as
/// `verifyLedger` reports it too (before: it could not be compared).
///
/// U1 B1 (T21 V1R-1): and its `project_id`, `created_at`, and `updated_at`
/// against the state database's Project and the times of its Task Events
/// ([`super::ledger::TaskChain::row_failure`]).
fn task_row(connection: &Connection, task: &Task, damage: &mut Damage) -> Result<(), StoreError> {
    let project = state_project(connection)?;
    match task_chains(connection)?.get(task.task_id.as_str()) {
        Some(chain)
            if chain.last.1.as_deref() == Some(task.status.as_str())
                && chain.last.2 == Some(task.revision)
                && chain
                    .row_failure(
                        &project,
                        task.project_id.as_str(),
                        task.created_at.as_str(),
                        task.updated_at.as_str(),
                    )
                    .is_none() => {}
        _ => damage.mismatch(LedgerRecordType::Task, task.task_id.as_str()),
    }
    Ok(())
}

/// P4-B (ADR 0014 E8): the Task's title and Workspace as `memproved`'s
/// `task.created@1` Event recorded them, in place of the row's, which no
/// check covers (T16 N8): the Event is in the hash chain, and neither field
/// changes after creation. A Task without that Event (created before
/// migration 0007) or a payload without the member keeps the row's value.
/// Status and revision stay the row's: the Task-scope integrity check
/// compares them with the Task's last Event ([`task_row`]).
pub(super) fn task_from_ledger(
    connection: &Connection,
    mut task: Task,
) -> Result<Task, StoreError> {
    if let Some(created) = task_created(connection, task.task_id.as_str())? {
        if let Some(title) = created.get("title").and_then(Value::as_str) {
            title.clone_into(&mut task.title);
        }
        match created.get("workspace_id") {
            Some(Value::Null) => task.workspace_id = None,
            Some(Value::String(workspace_id)) => {
                if let Ok(workspace_id) = WorkspaceId::parse(workspace_id) {
                    task.workspace_id = Some(workspace_id);
                }
            }
            _ => {}
        }
    }
    Ok(task)
}

/// The payload of the `task.created@1` Event of `memproved` that records
/// `task_id`, `None` without one.
fn task_created(connection: &Connection, task_id: &str) -> Result<Option<Value>, StoreError> {
    let payloads = connection
        .prepare(
            "SELECT payload FROM events
             WHERE producer_name = ?1 AND session_id IS NULL
               AND event_type = 'task.created' AND payload_schema = 'task.created@1'
             ORDER BY project_sequence",
        )?
        .query_map([DAEMON_PRODUCER_NAME], |row| stored_json(row, 0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(payloads
        .into_iter()
        .flatten()
        .find(|payload| payload.get("task_id").and_then(Value::as_str) == Some(task_id)))
}

/// T21 (V1-N-5): the Workspace scope of `task_id` as the Task Capsule reads
/// it ([`task_from_ledger`]): that of its `task.created@1` Event, else
/// `row`, the `workspace_id` of its row. A write that accepts work only
/// from the Task's Workspace takes this one; no digest covers the row's.
pub(super) fn ledger_scope(
    connection: &Connection,
    task_id: &str,
    row: Option<String>,
) -> Result<Option<String>, StoreError> {
    // The rule of `task_from_ledger`: `null` is no scope, an identity that
    // decodes is that Workspace, anything else leaves the row's.
    let created = task_created(connection, task_id)?;
    Ok(
        match created
            .as_ref()
            .and_then(|created| created.get("workspace_id"))
        {
            Some(Value::Null) => None,
            Some(Value::String(workspace_id)) if WorkspaceId::parse(workspace_id).is_ok() => {
                Some(workspace_id.clone())
            }
            _ => row,
        },
    )
}

/// P4-B (ADR 0014 E8): a Session's start, Workspace, and end as
/// `memproved`'s Events recorded them, in place of the row's, which no
/// check covers (ADR 0012, T14 N5): `started_at` and `workspace_id` are
/// those of its `session.started` Event (its `recorded_at`, the time
/// `createSession` wrote into both), and `ended_at` is the `recorded_at` of
/// its first `session.ended` or `session.crashed@1` (U9 A7: or `@2`) Event, `None` without
/// one. A Session without `session.started` (created before P2-A) keeps its
/// row as it is. Revision stays the row's; it is not shown. U8 A1 (Owner
/// O4): `session.started` is read at `session.started@1` and, for a
/// Session that declared an agent, `session.started@3`, whose `agent` is
/// the Session's (the membership check compares the row's agent columns
/// with it); the agent of a `@1` Session is none. U9 A7 (Owner O3): its
/// ender is the `ended_by` of its first end Event when that is
/// `session.crashed@2`, none otherwise (the membership check compares the
/// row's ender columns with it).
pub(super) fn session_from_ledger(
    connection: &Connection,
    mut session: Session,
) -> Result<Session, StoreError> {
    let events = connection
        .prepare(
            "SELECT event_type, recorded_at, workspace_id, payload_schema, payload FROM events
             WHERE producer_name = ?1 AND session_id = ?2
               AND event_type IN ('session.started', 'session.ended', 'session.crashed')
               AND (event_type = 'session.ended' OR payload_schema = event_type || '@1'
                    OR payload_schema = ?3
                    OR (event_type = 'session.crashed' AND payload_schema = ?4))
             ORDER BY project_sequence",
        )?
        .query_map(
            rusqlite::params![
                DAEMON_PRODUCER_NAME,
                session.session_id.as_str(),
                SESSION_STARTED_AGENT_SCHEMA,
                SESSION_CRASHED_ENDED_BY_SCHEMA
            ],
            |row| {
                Ok((
                    lossy_text(row, 0)?,
                    lossy_text(row, 1)?,
                    lossy_optional_text(row, 2)?.unwrap_or_default(),
                    lossy_optional_text(row, 3)?.unwrap_or_default(),
                    stored_json(row, 4)?.unwrap_or(Value::Null),
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        // P4-C2: an Event whose time does not decode is not read; the
        // chain check fails on it. P4-H1: nor one that is not UTF-8; a
        // NULL Workspace names none.
        .filter_map(|(event_type, recorded_at, workspace_id, schema, payload)| {
            // U8 A1: the agent of `session.started`; U9 A7: the ender of
            // `session.crashed@2` (none for `@1` and `session.ended`).
            let (agent, ended_by) = if event_type == "session.started" {
                (started_agent(&schema, &payload).ok()?, None)
            } else {
                (None, crashed_ended_by(&schema, &payload).ok()?)
            };
            Some((
                event_type,
                UtcTimestamp::parse(&recorded_at).ok()?,
                workspace_id,
                agent,
                ended_by,
            ))
        })
        .collect::<Vec<_>>();
    let Some((_, started_at, workspace_id, agent, _)) = events
        .iter()
        .find(|(event_type, ..)| event_type == "session.started")
    else {
        return Ok(session);
    };
    session.started_at = started_at.clone();
    if let Ok(workspace_id) = WorkspaceId::parse(workspace_id) {
        session.workspace_id = workspace_id;
    }
    session.agent.clone_from(agent);
    let end = events
        .iter()
        .find(|(event_type, ..)| event_type != "session.started");
    session.ended_at = end.map(|(_, recorded_at, ..)| recorded_at.clone());
    session.ended_by = end.and_then(|(.., ended_by)| ended_by.clone());
    Ok(session)
}

/// P3-L L7: the `recorded_at` of the latest Event of `session_id` in this
/// read transaction, `None` when there is none. P4-C2: a time that does not
/// decode reads as none; the chain check then fails, and the Task is
/// `tampered`.
fn last_recorded_at(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<Option<UtcTimestamp>, StoreError> {
    Ok(connection
        .query_row(
            "SELECT recorded_at FROM events WHERE session_id = ?1
             ORDER BY sequence DESC LIMIT 1",
            [session_id.as_str()],
            |row| lossy_text(row, 0),
        )
        .optional()?
        .and_then(|recorded_at| UtcTimestamp::parse(&recorded_at).ok()))
}

/// One Event of `memproved` on a Session of the Task, of the types the
/// Capsule reads from Events.
struct DaemonEvent {
    event_id: EventId,
    sequence: u64,
    session_id: SessionId,
    event_type: String,
    /// U9 A7: `session.crashed` is read at `@1` and `@2`.
    payload_schema: String,
    payload: Value,
}

/// Whether a `session.crashed` payload decodes as `session.crashed@1`
/// (ADR 0013 D6): exactly `reason` (a string or `null`) and `outcome`
/// `abandoned`; U9 A7 (ADR 0010, "Amendment (U9)"), at
/// `session.crashed@2`, those two and `ended_by`, which must decode.
pub(super) fn crashed_payload(payload_schema: &str, payload: &Value) -> bool {
    let members = if payload_schema == SESSION_CRASHED_ENDED_BY_SCHEMA {
        3
    } else {
        2
    };
    crashed_ended_by(payload_schema, payload).is_ok()
        && payload.as_object().is_some_and(|object| {
            object.len() == members
                && object
                    .get("reason")
                    .is_some_and(|reason| reason.is_null() || reason.is_string())
                && object.get("outcome").and_then(Value::as_str) == Some("abandoned")
        })
}

/// U9 A7 (Owner O3): the ender a `session.crashed` Event recorded: the
/// `ended_by` member of a `session.crashed@2` payload; none for any other
/// schema (`session.crashed@1` names no ender, and `session.ended` is not
/// read here). `Err` when a `@2` payload's `ended_by` does not decode.
pub(super) fn crashed_ended_by(
    payload_schema: &str,
    payload: &Value,
) -> Result<Option<SessionEndedBy>, ()> {
    if payload_schema != SESSION_CRASHED_ENDED_BY_SCHEMA {
        return Ok(None);
    }
    payload
        .get("ended_by")
        .and_then(SessionEndedBy::from_recorded)
        .map(Some)
        .ok_or(())
}

/// The `claim.superseded`, `decision.recorded`, `session.ended`, and
/// `session.crashed` Events `memproved` wrote on the Task's Sessions, in
/// Project order. A payload or identity that does not decode names nothing;
/// the chain check reports an altered row. P3-C2 (T12 §3.1): a reserved
/// type counts only with `payload_schema` exactly `<type>@1`, the schema
/// these payloads are decoded by; any other version is ignored. U9 A7
/// (ADR 0010, "Amendment (U9)"): `session.crashed@2` counts too.
fn daemon_events(
    connection: &Connection,
    task_sessions: &BTreeSet<String>,
) -> Result<Vec<DaemonEvent>, StoreError> {
    let rows = connection
        .prepare(
            "SELECT event_id, project_sequence, session_id, event_type, payload,
                    coalesce(payload_schema, '')
             FROM events
             WHERE producer_name = ?1 AND session_id IS NOT NULL
               AND event_type IN ('claim.superseded', 'decision.recorded',
                                  'session.ended', 'session.crashed', 'capsule.recovered')
               AND (event_type = 'session.ended' OR payload_schema = event_type || '@1'
                    OR (event_type = 'session.crashed' AND payload_schema = ?2))
             ORDER BY project_sequence",
        )?
        .query_map(
            rusqlite::params![DAEMON_PRODUCER_NAME, SESSION_CRASHED_ENDED_BY_SCHEMA],
            |row| {
                Ok((
                    lossy_text(row, 0)?,
                    stored_sequence(row, 1)?,
                    lossy_text(row, 2)?,
                    lossy_text(row, 3)?,
                    stored_json(row, 4)?,
                    lossy_text(row, 5)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, _, session_id, ..)| task_sessions.contains(session_id))
        .filter_map(
            |(event_id, sequence, session_id, event_type, payload, payload_schema)| {
                Some(DaemonEvent {
                    event_id: EventId::parse(&event_id).ok()?,
                    sequence: sequence?,
                    session_id: SessionId::parse(&session_id).ok()?,
                    event_type,
                    payload_schema,
                    payload: payload?,
                })
            },
        )
        .collect())
}

/// Every Checkpoint of the Task: those its Sessions' `checkpoint.created`
/// Events record, at that Event's sequence, and every row of the Task no
/// such Event records, at its `project_event_sequence` plus one; in that
/// order. A readable one is returned; every missing, unreadable, or
/// disagreeing record is noted.
fn checkpoints(
    connection: &Connection,
    task_id: &TaskId,
    task_sessions: &BTreeSet<String>,
    references: &LedgerReferences,
    damage: &mut Damage,
) -> Result<Vec<CheckpointRecord>, StoreError> {
    let mut ordered: BTreeMap<String, u64> = BTreeMap::new();
    for announcement in references.announced_on(task_sessions) {
        ordered
            .entry(announcement.checkpoint_id.as_str().to_owned())
            .or_insert(announcement.sequence);
    }
    let rows = connection
        .prepare(
            "SELECT checkpoint_id, project_event_sequence FROM checkpoints WHERE task_id = ?1",
        )?
        .query_map([task_id.as_str()], |row| {
            Ok((lossy_text(row, 0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (checkpoint_id, sequence) in rows {
        let sequence = u64::try_from(sequence).unwrap_or(0).saturating_add(1);
        ordered.entry(checkpoint_id).or_insert(sequence);
    }
    let mut ordered = ordered.into_iter().collect::<Vec<_>>();
    ordered.sort_by(|(left_id, left), (right_id, right)| (left, left_id).cmp(&(right, right_id)));
    let mut checkpoints = Vec::new();
    for (raw_id, sequence) in ordered {
        let Ok(checkpoint_id) = CheckpointId::parse(&raw_id) else {
            damage.mismatch(LedgerRecordType::Checkpoint, &raw_id);
            continue;
        };
        if let Some(record) =
            checkpoint_record(connection, &checkpoint_id, sequence, references, damage)?
        {
            checkpoints.push(record);
        }
    }
    Ok(checkpoints)
}

/// One Checkpoint with its Snapshot and Evidence when all three recompute;
/// otherwise `None`, with what is missing or does not recompute noted.
fn checkpoint_record(
    connection: &Connection,
    checkpoint_id: &CheckpointId,
    sequence: u64,
    references: &LedgerReferences,
    damage: &mut Damage,
) -> Result<Option<CheckpointRecord>, StoreError> {
    let row = connection
        .query_row(
            "SELECT snapshot_id FROM checkpoints WHERE checkpoint_id = ?1",
            [checkpoint_id.as_str()],
            |row| lossy_text(row, 0),
        )
        .optional()?;
    let Some(snapshot_id) = row else {
        // Recorded by the ledger, deleted from the tables.
        damage.missing(LedgerRecordType::Checkpoint, checkpoint_id.as_str());
        return Ok(None);
    };
    let read = checkpoint_by_id(connection, checkpoint_id).and_then(|checkpoint| {
        let checkpoint = checkpoint.ok_or(StoreError::NotFound("Checkpoint"))?;
        let snapshot = snapshot_by_id(connection, checkpoint.snapshot_id())?
            .ok_or(StoreError::NotFound("Snapshot"))?;
        let evidence = snapshot
            .all_evidence_ids()
            .map(|id| evidence_by_id(connection, id)?.ok_or(StoreError::NotFound("Evidence")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((checkpoint, snapshot, evidence))
    });
    let (checkpoint, snapshot, evidence) = match read {
        Ok(read) => read,
        Err(error) if is_integrity_failure(&error) || matches!(error, StoreError::NotFound(_)) => {
            unreadable(connection, checkpoint_id, &snapshot_id, damage)?;
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut damaged = false;
    if references
        .checkpoint_failure(
            checkpoint.checkpoint_id(),
            checkpoint.digest(),
            checkpoint.project_event_sequence(),
        )
        .is_some()
    {
        damage.mismatch(LedgerRecordType::Checkpoint, checkpoint_id.as_str());
        damaged = true;
    }
    damaged |= damage.sealed(references, &snapshot, &evidence);
    Ok(Some(CheckpointRecord {
        project_sequence: sequence,
        checkpoint,
        snapshot,
        evidence,
        damaged,
    }))
}

/// Names what makes a Checkpoint unreadable: its Snapshot or an Evidence
/// object it or its Snapshot references, missing or not recomputing; the
/// Checkpoint record itself when nothing else explains it.
fn unreadable(
    connection: &Connection,
    checkpoint_id: &CheckpointId,
    snapshot_id: &str,
    damage: &mut Damage,
) -> Result<(), StoreError> {
    let before = damage.0.len();
    let referenced = connection
        .prepare(
            "SELECT evidence_id FROM checkpoint_item_evidence WHERE checkpoint_id = ?1
             ORDER BY evidence_id",
        )?
        .query_map([checkpoint_id.as_str()], |row| lossy_text(row, 0))?
        .collect::<Result<Vec<_>, _>>()?;
    note_evidence(connection, &referenced, damage)?;
    match RepositorySnapshotId::parse(snapshot_id) {
        Err(_) => damage.missing(LedgerRecordType::Snapshot, snapshot_id),
        Ok(id) => {
            sealed_snapshot(connection, &id, damage)?;
        }
    }
    if damage.0.len() == before {
        damage.mismatch(LedgerRecordType::Checkpoint, checkpoint_id.as_str());
    }
    Ok(())
}

/// Notes each of `referenced` that is missing or does not recompute; `true`
/// when any is.
fn note_evidence(
    connection: &Connection,
    referenced: &[String],
    damage: &mut Damage,
) -> Result<bool, StoreError> {
    let mut noted = false;
    for raw in referenced {
        match EvidenceId::parse(raw) {
            Err(_) => {
                damage.missing(LedgerRecordType::Evidence, raw);
                noted = true;
            }
            Ok(id) => match evidence_by_id(connection, &id) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    damage.missing(LedgerRecordType::Evidence, raw);
                    noted = true;
                }
                Err(error) if is_integrity_failure(&error) => {
                    damage.mismatch(LedgerRecordType::Evidence, raw);
                    noted = true;
                }
                Err(error) => return Err(error),
            },
        }
    }
    Ok(noted)
}

/// Every readable Snapshot of the Task that agrees with the ledger, at the
/// sequence of the first Event that records it, in that order. The others
/// are noted.
fn runs(
    connection: &Connection,
    task_id: &TaskId,
    references: &LedgerReferences,
    damage: &mut Damage,
) -> Result<Vec<RecordedRun>, StoreError> {
    let ids = connection
        .prepare("SELECT snapshot_id FROM repository_snapshots WHERE task_id = ?1")?
        .query_map([task_id.as_str()], |row| lossy_text(row, 0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut runs = Vec::new();
    for raw in ids {
        let Ok(snapshot_id) = RepositorySnapshotId::parse(&raw) else {
            damage.mismatch(LedgerRecordType::Snapshot, &raw);
            continue;
        };
        let Some((snapshot, evidence)) = sealed_snapshot(connection, &snapshot_id, damage)? else {
            continue;
        };
        if damage.sealed(references, &snapshot, &evidence) {
            continue;
        }
        let Some(sequence) = references.snapshot_sequence(&snapshot_id) else {
            continue;
        };
        runs.push(RecordedRun {
            project_sequence: sequence,
            snapshot,
            evidence,
        });
    }
    runs.sort_by(|left, right| {
        (left.project_sequence, left.snapshot.snapshot_id())
            .cmp(&(right.project_sequence, right.snapshot.snapshot_id()))
    });
    Ok(runs)
}

/// A Snapshot and its Evidence when all recompute; otherwise `None`, with
/// the failing record noted.
fn sealed_snapshot(
    connection: &Connection,
    snapshot_id: &RepositorySnapshotId,
    damage: &mut Damage,
) -> Result<Option<(RepositorySnapshot, Vec<Evidence>)>, StoreError> {
    let snapshot = match snapshot_by_id(connection, snapshot_id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            damage.missing(LedgerRecordType::Snapshot, snapshot_id.as_str());
            return Ok(None);
        }
        Err(error) if is_integrity_failure(&error) => {
            // A Snapshot's digest covers its Evidence: name the Evidence
            // that does not recompute, and the Snapshot only when none
            // explains it.
            let referenced = connection
                .prepare(
                    "SELECT evidence_id FROM repository_snapshot_evidence
                     WHERE snapshot_id = ?1 ORDER BY evidence_id",
                )?
                .query_map([snapshot_id.as_str()], |row| lossy_text(row, 0))?
                .collect::<Result<Vec<_>, _>>()?;
            if !note_evidence(connection, &referenced, damage)? {
                damage.mismatch(LedgerRecordType::Snapshot, snapshot_id.as_str());
            }
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut evidence = Vec::new();
    let mut readable = true;
    for id in snapshot.all_evidence_ids() {
        match evidence_by_id(connection, id) {
            Ok(Some(item)) => evidence.push(item),
            Ok(None) => {
                damage.missing(LedgerRecordType::Evidence, id.as_str());
                readable = false;
            }
            Err(error) if is_integrity_failure(&error) => {
                damage.mismatch(LedgerRecordType::Evidence, id.as_str());
                readable = false;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(readable.then_some((snapshot, evidence)))
}

/// One Evidence object a Decision cites, with the first Snapshot that seals
/// it and that Snapshot's Evidence, and whether any of it disagrees with the
/// ledger; `None` when it cannot be read (noted).
#[allow(clippy::type_complexity)] // One record and its seal, read together.
fn sealed_evidence(
    connection: &Connection,
    evidence_id: &EvidenceId,
    references: &LedgerReferences,
    damage: &mut Damage,
) -> Result<Option<((Evidence, RepositorySnapshot, Vec<Evidence>), bool)>, StoreError> {
    let evidence = match evidence_by_id(connection, evidence_id) {
        Ok(Some(evidence)) => evidence,
        Ok(None) => {
            damage.missing(LedgerRecordType::Evidence, evidence_id.as_str());
            return Ok(None);
        }
        Err(error) if is_integrity_failure(&error) => {
            damage.mismatch(LedgerRecordType::Evidence, evidence_id.as_str());
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    // P4-B (P4-0 item D): the first sealer in ledger order.
    let first = sealing_snapshot_ids(connection, references, evidence_id)?
        .into_iter()
        .next();
    let Some(Ok(snapshot_id)) = first.as_deref().map(RepositorySnapshotId::parse) else {
        // No Snapshot seals it: the record that pins it to a commit is
        // missing.
        damage.missing(LedgerRecordType::Evidence, evidence_id.as_str());
        return Ok(None);
    };
    let Some((snapshot, snapshot_evidence)) = sealed_snapshot(connection, &snapshot_id, damage)?
    else {
        return Ok(None);
    };
    let damaged = damage.sealed(references, &snapshot, &snapshot_evidence);
    Ok(Some(((evidence, snapshot, snapshot_evidence), damaged)))
}
