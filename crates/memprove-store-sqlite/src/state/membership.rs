//! P4-C2 (Owner, T19 SF-1 and the Session set): which Sessions continue a
//! Task, and the Task itself, read so that no row of the Task's read scope
//! can turn a read into an error.
//!
//! The Sessions of a Task are the ones the chain names: a `session.started`
//! or `task.session_linked` Event of `memproved` on that Session whose
//! payload names the Task. The rows behind them are checked against the
//! chain, and each disagreement is a damaged record of type `session`
//! (record ID the `session_id`):
//!
//! - a `task_sessions` row of the Task for a Session the chain does not
//!   link to it, or whose `linked_at` does not decode:
//!   `object_digest_mismatch`;
//! - a Session the chain links to the Task (`task.session_linked`) with no
//!   `task_sessions` row: `object_missing`;
//! - a Session the chain names with no `sessions` row: `object_missing`;
//!   with one that does not decode: `object_digest_mismatch`;
//! - U1 B1 (T21 V1R-1): a `sessions` row whose `started_at`, `agent_id`,
//!   or `workspace_id` is not what its `session.started` Event recorded,
//!   and a `task_sessions` row whose `linked_at` is not the `recorded_at` of
//!   the link's `task.session_linked` Event: `object_digest_mismatch`.
//!   U8 A1 (Owner O4): so is a row whose `agent_name`, `agent_role`, or
//!   `parent_session_id` is not the `agent` its `session.started@3` Event
//!   recorded (all three NULL for a `session.started@1` Event). U9 A7
//!   (Owner O3): and one whose `ended_by_session_id`,
//!   `ended_by_agent_name`, or `ended_by_agent_role` is not the `ended_by`
//!   its first end Event recorded (`session.crashed@2`; all three NULL for
//!   any other end and for a Session whose end the chain does not record).
//!
//! A Session that has no `session.started` Event at all predates P2-A
//! (unsupported ledgers, P3-C2): its link row is read as it is, as P4-B
//! reads the fields of such a Session from its row.
//!
//! A Session whose row is missing or does not decode is still listed, in
//! its place, from its `session.started` Event (its `recorded_at`,
//! Workspace, and `agent_id`), as P4-B reads every shown field of a
//! Session. A Session only a row links is neither listed nor read.
//!
//! A Task row that does not decode is a damaged record of type `task`
//! (`object_digest_mismatch`); the Task is then read from its Events: the
//! title and Workspace of `task.created@1`, the status and revision of its
//! last Task Event.

use std::collections::{BTreeMap, BTreeSet};

use memprove_domain::{
    AgentId, DAEMON_PRODUCER_NAME, DamagedRecord, LedgerRecordType, ProjectId, ReasonCode,
    SESSION_STARTED_AGENT_SCHEMA, Session, SessionAgent, SessionEndedBy, SessionId, Task, TaskId,
    TaskStatus, UtcTimestamp, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use super::{
    SESSION_COLUMNS,
    capsule::{session_from_ledger, task_from_ledger},
    ledger::last_task_events,
    lossy_optional_text, lossy_text,
    preview::is_integrity_failure,
    session_from_row, stored_json, task_by_id_connection,
};
use crate::{StoreError, error::is_decode_failure};

/// The time of a Task whose row holds none that decodes; it is shown
/// nowhere.
const EPOCH: &str = "1970-01-01T00:00:00Z";

fn session_damage(session_id: &str, reason: ReasonCode) -> DamagedRecord {
    DamagedRecord {
        record_type: LedgerRecordType::Session,
        record_id: damaged_record_id(session_id),
        reason,
    }
}

/// The `record_id` of a damaged record whose stored identity is `raw`.
/// P4-H1 (review of the first P4-H1 commit): a damaged record names its
/// record in plain printable ASCII, as every identity the store writes is,
/// and never with an empty string: an empty identity is named `""`, and
/// each character of another that is not printable ASCII, or is a
/// backslash, is written `\u{<hex>}` (a space, a control, U+FFFD for
/// bytes that are not UTF-8, any other non-ASCII character). An identity
/// the store wrote is named as it is.
pub(super) fn damaged_record_id(raw: &str) -> String {
    if raw.is_empty() {
        return "\"\"".to_owned();
    }
    raw.chars()
        .map(|character| {
            if character.is_ascii_graphic() && character != '\\' {
                character.to_string()
            } else {
                format!("\\u{{{:x}}}", u32::from(character))
            }
        })
        .collect()
}

/// The Sessions of one Task, by the chain, and what disagrees with it.
pub(super) struct TaskSessions {
    /// Every Session the chain names for the Task.
    pub(super) ids: BTreeSet<String>,
    /// The listed Sessions, unordered.
    pub(super) sessions: Vec<Session>,
    /// Every Session record that is missing, does not decode, or disagrees
    /// with the chain.
    pub(super) damaged: BTreeSet<DamagedRecord>,
}

/// One `session.started` or `task.session_linked` Event of `memproved`.
struct Membership {
    session_id: String,
    event_type: String,
    payload_schema: String,
    payload: Value,
    workspace_id: String,
    recorded_at: String,
}

/// U8 A1 (Owner O4): the agent a `session.started` Event recorded: the
/// `agent` member of a `session.started@3` payload; none for any other
/// schema (`session.started@1` records no agent). `Err` when a `@3`
/// payload's `agent` does not decode.
pub(super) fn started_agent(
    payload_schema: &str,
    payload: &Value,
) -> Result<Option<SessionAgent>, ()> {
    if payload_schema != SESSION_STARTED_AGENT_SCHEMA {
        return Ok(None);
    }
    payload
        .get("agent")
        .and_then(SessionAgent::from_recorded)
        .map(Some)
        .ok_or(())
}

fn memberships(connection: &Connection) -> Result<Vec<Membership>, StoreError> {
    Ok(connection
        .prepare(
            "SELECT session_id, event_type, payload, workspace_id, recorded_at,
                    coalesce(payload_schema, '')
             FROM events
             WHERE producer_name = ?1 AND session_id IS NOT NULL
               AND event_type IN ('session.started', 'task.session_linked')
             ORDER BY project_sequence",
        )?
        // P4-H1: a field that is not UTF-8 names nothing (`lossy_text`); the
        // chain check reports the Event.
        .query_map([DAEMON_PRODUCER_NAME], |row| {
            Ok(Membership {
                session_id: lossy_text(row, 0)?,
                event_type: lossy_text(row, 1)?,
                payload: stored_json(row, 2)?.unwrap_or(Value::Null),
                // A NULL Workspace names none either.
                workspace_id: lossy_optional_text(row, 3)?.unwrap_or_default(),
                recorded_at: lossy_text(row, 4)?,
                payload_schema: lossy_text(row, 5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

/// Reads the Sessions of `task_id` (see the module).
pub(super) fn task_sessions(
    connection: &Connection,
    task_id: &TaskId,
) -> Result<TaskSessions, StoreError> {
    let events = memberships(connection)?;
    let names_task = |event: &&Membership| {
        event.payload.get("task_id").and_then(Value::as_str) == Some(task_id.as_str())
    };
    let mut ids = events
        .iter()
        .filter(names_task)
        .map(|event| event.session_id.clone())
        .collect::<BTreeSet<_>>();
    let linked = events
        .iter()
        .filter(names_task)
        .filter(|event| event.event_type == "task.session_linked")
        .map(|event| event.session_id.as_str())
        .collect::<BTreeSet<_>>();
    let started = events
        .iter()
        .filter(|event| event.event_type == "session.started")
        .fold(BTreeMap::new(), |mut first, event| {
            first.entry(event.session_id.as_str()).or_insert(event);
            first
        });
    let mut damaged = BTreeSet::new();
    let rows = connection
        .prepare("SELECT session_id, linked_at FROM task_sessions WHERE task_id = ?1")?
        .query_map([task_id.as_str()], |row| {
            Ok((lossy_text(row, 0)?, lossy_text(row, 1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // U1 B1 (T21 V1R-1): the time of each link the chain records.
    let link_times = events
        .iter()
        .filter(names_task)
        .filter(|event| event.event_type == "task.session_linked")
        .fold(BTreeMap::new(), |mut first, event| {
            first
                .entry(event.session_id.as_str())
                .or_insert(event.recorded_at.as_str());
            first
        });
    for (session_id, linked_at) in &rows {
        // A Session the chain has no `session.started` of at all predates
        // P2-A: its link row is read as it is, as P4-B reads its fields.
        if !started.contains_key(session_id.as_str()) {
            ids.insert(session_id.clone());
        } else if !ids.contains(session_id)
            || UtcTimestamp::parse(linked_at).is_err()
            || link_times
                .get(session_id.as_str())
                .is_some_and(|recorded_at| recorded_at != linked_at)
        {
            damaged.insert(session_damage(session_id, ReasonCode::ObjectDigestMismatch));
        }
    }
    for session_id in &linked {
        if !rows.iter().any(|(row, _)| row == session_id) {
            damaged.insert(session_damage(session_id, ReasonCode::ObjectMissing));
        }
    }
    let mut sessions = Vec::new();
    let mut statement = connection.prepare(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = ?1"
    ))?;
    for session_id in &ids {
        let row = match statement
            .query_row([session_id], session_from_row)
            .optional()
        {
            Ok(Some(session)) => {
                // U1 B1 (T21 V1R-1): the row's start, agent, and Workspace
                // are those its `session.started` Event recorded; no
                // digest covers them.
                if started
                    .get(session_id.as_str())
                    .is_some_and(|event| !agrees_with_started(&session, event))
                {
                    damaged.insert(session_damage(session_id, ReasonCode::ObjectDigestMismatch));
                }
                Some(session)
            }
            Ok(None) => {
                damaged.insert(session_damage(session_id, ReasonCode::ObjectMissing));
                None
            }
            Err(error) if is_decode_failure(&error) => {
                damaged.insert(session_damage(session_id, ReasonCode::ObjectDigestMismatch));
                None
            }
            Err(error) => return Err(error.into()),
        };
        let from_row = row.is_some();
        let session = match row {
            Some(session) => Some(session),
            None => started
                .get(session_id.as_str())
                .and_then(|event| from_started(event)),
        };
        if let Some(session) = session {
            sessions.push(from_ledger(connection, session, from_row, &mut damaged)?);
        }
    }
    Ok(TaskSessions {
        ids,
        sessions,
        damaged,
    })
}

/// The Session as the ledger records it ([`session_from_ledger`]). U9 A7
/// (Owner O3): for a Session read from its row (`from_row`), the row's
/// ender columns must be the `ended_by` its `session.crashed@2` Event
/// recorded (all three NULL for any other end, and for a Session the chain
/// records no end of); no digest covers them, and a row that disagrees is
/// a damaged record.
fn from_ledger(
    connection: &Connection,
    session: Session,
    from_row: bool,
    damaged: &mut BTreeSet<DamagedRecord>,
) -> Result<Session, StoreError> {
    let row = SessionEndedBy::columns(session.ended_by.as_ref());
    let read = session_from_ledger(connection, session)?;
    if from_row && row != SessionEndedBy::columns(read.ended_by.as_ref()) {
        damaged.insert(session_damage(
            read.session_id.as_str(),
            ReasonCode::ObjectDigestMismatch,
        ));
    }
    Ok(read)
}

/// U1 B1 (T21 V1R-1): whether a Session row's `started_at`, `agent_id`,
/// and `workspace_id` are those of its `session.started` Event:
/// `createSession` writes the row and that Event from one time, agent, and
/// Workspace. U8 A1 (Owner O4): and its agent columns are the `agent` the
/// Event recorded.
fn agrees_with_started(session: &Session, event: &Membership) -> bool {
    session.started_at.as_str() == event.recorded_at
        && event.payload.get("agent_id").and_then(Value::as_str) == Some(session.agent_id.as_str())
        && session.workspace_id.as_str() == event.workspace_id
        && started_agent(&event.payload_schema, &event.payload).as_ref() == Ok(&session.agent)
}

/// A Session as its `session.started` Event recorded it, for one whose row
/// is missing or does not decode; `None` when the Event does not decode.
fn from_started(event: &Membership) -> Option<Session> {
    Some(Session {
        session_id: SessionId::parse(&event.session_id).ok()?,
        workspace_id: WorkspaceId::parse(&event.workspace_id).ok()?,
        agent_id: AgentId::parse(event.payload.get("agent_id")?.as_str()?).ok()?,
        started_at: UtcTimestamp::parse(&event.recorded_at).ok()?,
        ended_at: None,
        revision: 1,
        agent: started_agent(&event.payload_schema, &event.payload).ok()?,
        ended_by: None,
    })
}

/// The Task of a read view: its row, with the title and Workspace of its
/// `task.created@1` Event (P4-B); when the row does not decode, the Task as
/// its Events recorded it, and a damaged record of type `task`.
///
/// # Errors
///
/// [`StoreError::NotFound`] when there is no row for `task_id`.
pub(super) fn task_for_view(
    connection: &Connection,
    task_id: &TaskId,
    damaged: &mut BTreeSet<DamagedRecord>,
) -> Result<Task, StoreError> {
    match task_by_id_connection(connection, task_id) {
        Ok(Some(task)) => task_from_ledger(connection, task),
        Ok(None) => Err(StoreError::NotFound("Task")),
        Err(error) if is_integrity_failure(&error) => {
            damaged.insert(DamagedRecord {
                record_type: LedgerRecordType::Task,
                record_id: task_id.as_str().to_owned(),
                reason: ReasonCode::ObjectDigestMismatch,
            });
            task_from_events(connection, task_id)
        }
        Err(error) => Err(error),
    }
}

/// A Task whose row does not decode, from its Events and whatever of the
/// row still reads; nothing of it but the fields the documents show is
/// used.
fn task_from_events(connection: &Connection, task_id: &TaskId) -> Result<Task, StoreError> {
    let (project_id, title, created_at) = connection
        .query_row(
            "SELECT task.project_id, task.title, task.created_at, metadata.project_id
             FROM tasks AS task, state_metadata AS metadata
             WHERE task.task_id = ?1 AND metadata.metadata_key = 1",
            [task_id.as_str()],
            // P4-H1: a field that is not UTF-8 reads lossily: the Task is
            // already a damaged record, and its title and time come from
            // its Events where they decode.
            |row| {
                let metadata = lossy_text(row, 3)?;
                Ok((
                    ProjectId::parse(&lossy_text(row, 0)?)
                        .or_else(|_| ProjectId::parse(&metadata))
                        .ok(),
                    lossy_text(row, 1)?,
                    lossy_text(row, 2)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound("Task"))?;
    let project_id = project_id.ok_or_else(|| {
        StoreError::PersistedDataIntegrity("no Project of the state database decodes".to_owned())
    })?;
    let (status, revision) = last_task_events(connection)?
        .get(task_id.as_str())
        .map(|(_, status, revision)| (status.clone(), *revision))
        .unwrap_or_default();
    let status = status
        .as_deref()
        .and_then(TaskStatus::parse)
        .unwrap_or(TaskStatus::Proposed);
    let at = UtcTimestamp::parse(&created_at)
        .or_else(|_| UtcTimestamp::parse(EPOCH))
        .map_err(|error| StoreError::PersistedDataIntegrity(error.to_string()))?;
    task_from_ledger(
        connection,
        Task {
            task_id: task_id.clone(),
            project_id,
            workspace_id: None,
            title,
            description: None,
            status,
            created_at: at.clone(),
            updated_at: at,
            revision: revision.unwrap_or(1),
            completion_evidence_ids: Vec::new(),
        },
    )
}
