//! The read-only ledger view behind `getRecoveryPreview` (ADR 0012 D6).
//!
//! Everything is read in one read transaction, so the view is the ledger at
//! one Project sequence N. Integrity failures of the recovery records or the
//! Event chain are reported in the view, for the ontology to judge
//! `tampered`; only database failures are errors. Nothing is written.
//!
//! P2-E: the records behind the Preview must also agree with the ledger, as
//! `verifyLedger` checks them ([`LedgerReferences`]): every Checkpoint of the
//! Task (digest and `project_event_sequence`, which decides the latest), and
//! the latest one's Snapshot and Evidence. A disagreement reads the latest
//! Checkpoint as damaged.
//!
//! P2-K (T11 S-1): the latest Checkpoint is the one the Task's last
//! `checkpoint.created` Event of `memproved` records, and every Checkpoint
//! those Events record must exist. A missing one reads as damaged; the
//! Preview never falls back to an earlier Checkpoint.
//!
//! P3-C2 (Owner, T12 S-4/N1): the view also carries the Task Capsule's
//! Task-scope integrity check ([`task_integrity`]): every Checkpoint,
//! Snapshot, and Evidence object of the Task recomputed, the Evidence its
//! Decisions cite, and the Task row against its last Task Event. Any damaged
//! record makes the Preview `tampered`, as it makes the Capsule. P3-D3: the
//! same check returns the Task's runs, so the Preview judges passed runs by
//! the Capsule's `RunHistory`.

use std::collections::BTreeSet;

use memprove_domain::{
    CheckpointId, CheckpointView, EventSummary, EvidenceId, PREVIEW_EVENT_LIMIT,
    PREVIEW_SESSION_LIMIT, RepositorySnapshotId, Sha256Digest, TaskId, TaskLedgerView,
    UtcTimestamp, verify_event_chain,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    EVENT_COLUMNS, StateStore,
    capsule::task_integrity,
    event_from_row, latest_project_sequence,
    ledger::LedgerReferences,
    lossy_text,
    membership::{task_for_view, task_sessions},
    recovery::{checkpoint_by_id, evidence_by_id, snapshot_by_id},
};
use crate::{StoreError, error::is_decode_failure};

impl StateStore {
    /// Reads the ledger view of `task_id` for a Recovery Preview: N, whether
    /// the Event chain verifies, the Task, the Sessions continuing it (latest
    /// `started_at` first, at most [`PREVIEW_SESSION_LIMIT`]), its latest
    /// Checkpoint (the one its last `checkpoint.created` Event records;
    /// without one, `project_event_sequence`, then `checkpoint_id`,
    /// descending) with that Checkpoint's Snapshot and Evidence, and those
    /// Sessions' Events after the Checkpoint up to N (type, time, and
    /// producer only, at most [`PREVIEW_EVENT_LIMIT`]).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] for an unknown Task, or a database failure.
    pub fn task_ledger_view(&self, task_id: &TaskId) -> Result<TaskLedgerView, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let view = ledger_view(&transaction, task_id)?;
        transaction.commit()?;
        Ok(view)
    }
}

fn ledger_view(connection: &Connection, task_id: &TaskId) -> Result<TaskLedgerView, StoreError> {
    // P4-B (ADR 0014 E8): the title, the Workspace, and each Session's
    // start, Workspace, and end are read from `memproved`'s Events, as the
    // Task Capsule reads them. P4-C2 (Owner, T19 SF-1): a Task row that does
    // not decode, and the Sessions as the chain names them with every row
    // behind them that disagrees, as the Task Capsule reads them too.
    let mut damaged = BTreeSet::new();
    let task = task_for_view(connection, task_id, &mut damaged)?;
    let project_sequence = latest_project_sequence(connection)?;
    let ledger_chain_valid = chain_valid(connection)?;
    let membership = task_sessions(connection, task_id)?;
    damaged.extend(membership.damaged);
    let task_sessions = membership.ids;
    let mut sessions = membership.sessions;
    sessions.sort_by(|left, right| {
        (right.started_at.as_str(), right.session_id.as_str())
            .cmp(&(left.started_at.as_str(), left.session_id.as_str()))
    });
    let sessions_truncated = sessions.len() > PREVIEW_SESSION_LIMIT;
    sessions.truncate(PREVIEW_SESSION_LIMIT);
    let references = LedgerReferences::read(connection)?;
    let announced = references
        .announced_on(&task_sessions)
        .map(|announcement| (announcement.sequence, &announcement.checkpoint_id))
        .collect::<Vec<_>>();
    let latest = match announced.last() {
        // Events after the Checkpoint follow its `checkpoint.created`.
        Some((sequence, checkpoint_id)) => Some(((*checkpoint_id).clone(), *sequence)),
        None => connection
            .query_row(
                "SELECT checkpoint_id, project_event_sequence FROM checkpoints
                 WHERE task_id = ?1
                 ORDER BY project_event_sequence DESC, checkpoint_id DESC
                 LIMIT 1",
                [task_id.as_str()],
                // P4-H1: a negative sequence, which no row the store wrote
                // holds, reads as 0; the Checkpoint then does not recompute.
                // An identity that is not UTF-8 does not parse below.
                |row| {
                    Ok((
                        lossy_text(row, 0)?,
                        u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                    ))
                },
            )
            .optional()?
            .map(|(checkpoint_id, sequence)| {
                let checkpoint_id = CheckpointId::parse(&checkpoint_id).map_err(|error| {
                    StoreError::PersistedDataIntegrity(format!("invalid checkpoint_id: {error}"))
                })?;
                // The Checkpoint's own `checkpoint.created` takes the next
                // sequence in the same transaction; later Events follow it.
                Ok::<_, StoreError>((checkpoint_id, sequence.saturating_add(1)))
            })
            .transpose()?,
    };
    let (checkpoint, after_sequence) = match latest {
        None => (CheckpointView::None, 0),
        Some((checkpoint_id, after_sequence)) => {
            let view = if checkpoint_exists(connection, &checkpoint_id)? {
                checkpoint_view(connection, &checkpoint_id)?
            } else {
                // Announced by the ledger, deleted from the tables.
                CheckpointView::Damaged {
                    checkpoint_id,
                    stored_digest: None,
                    missing_object: true,
                    invalid_evidence_ids: Vec::new(),
                }
            };
            let announced = announced.iter().map(|(_, id)| *id).collect::<Vec<_>>();
            (
                agree_with_ledger(connection, task_id, &references, &announced, view)?,
                after_sequence,
            )
        }
    };
    let mut events = events_after(connection, &task_sessions, after_sequence, project_sequence)?;
    let events_truncated = events.len() > PREVIEW_EVENT_LIMIT;
    events.truncate(PREVIEW_EVENT_LIMIT);
    // P3-C2 (T12 S-4/N1): the Task Capsule's own Task-scope check, and
    // (P3-D3) the runs it reads.
    let (task_damage, runs) = task_integrity(connection, &task, &references, &task_sessions)?;
    damaged.extend(task_damage);
    Ok(TaskLedgerView {
        project_sequence,
        ledger_chain_valid,
        task,
        sessions,
        sessions_truncated,
        checkpoint,
        events,
        events_truncated,
        damaged_records: damaged,
        runs,
    })
}

/// The Task's Sessions' Events in `(after, up_to]`, one more than the limit
/// so truncation can be told. P4-C2: the Sessions are the ones the chain
/// names (`task_sessions`).
fn events_after(
    connection: &Connection,
    task_sessions: &BTreeSet<String>,
    after_sequence: u64,
    project_sequence: u64,
) -> Result<Vec<EventSummary>, StoreError> {
    let limit = PREVIEW_EVENT_LIMIT.saturating_add(1);
    let mut statement = connection.prepare(
        "SELECT project_sequence, event_type, producer_occurred_at, recorded_at,
                producer_name, producer_version, session_id
         FROM events
         WHERE session_id IS NOT NULL
           AND project_sequence > ?1 AND project_sequence <= ?2
         ORDER BY project_sequence",
    )?;
    let mut rows = statement.query(params![after_sequence, project_sequence])?;
    let mut read = Vec::new();
    while read.len() < limit {
        let Some(row) = rows.next()? else {
            break;
        };
        if !task_sessions.contains(&lossy_text(row, 6)?) {
            continue;
        }
        let summary = (|| {
            Ok::<_, rusqlite::Error>((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })();
        match summary {
            Ok(summary) => read.push(summary),
            // P4-H1: an Event field that does not decode (text that is not
            // UTF-8) does not decode in the chain check either, which then
            // reports the ledger invalid; the Preview lists no Events.
            Err(error) if is_decode_failure(&error) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
    }
    let events = read
        .into_iter()
        .map(
            |(sequence, event_type, occurred_at, recorded_at, name, version)| {
                let timestamp = |value: &str| {
                    UtcTimestamp::parse(value).map_err(|error| {
                        StoreError::PersistedDataIntegrity(format!("invalid Event time: {error}"))
                    })
                };
                Ok(EventSummary {
                    project_sequence: sequence,
                    event_type,
                    occurred_at: occurred_at.as_deref().map(timestamp).transpose()?,
                    recorded_at: timestamp(&recorded_at)?,
                    producer_name: name,
                    producer_version: version,
                })
            },
        )
        .collect::<Result<Vec<_>, StoreError>>();
    // A malformed Event time does not decode in the chain check either, which
    // then reports the ledger invalid; the Preview lists no Events.
    match events {
        Ok(events) => Ok(events),
        Err(error) if is_integrity_failure(&error) => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Whether the Project's complete Event chain recomputes. A row that does
/// not decode or recompute, or a pre-envelope ledger, does not verify.
pub(super) fn chain_valid(connection: &Connection) -> Result<bool, StoreError> {
    let legacy: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE event_digest_version IS NULL)",
        [],
        |row| row.get(0),
    )?;
    if legacy {
        return Ok(false);
    }
    let rows = connection
        .prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM events ORDER BY project_sequence"
        ))?
        .query_map([], event_from_row)?
        .collect::<Result<Vec<_>, _>>();
    match rows {
        Ok(events) => Ok(verify_event_chain(events.iter()).is_ok()),
        Err(error) if is_decode_failure(&error) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn is_integrity_failure(error: &StoreError) -> bool {
    error.is_undecodable()
}

fn checkpoint_exists(
    connection: &Connection,
    checkpoint_id: &CheckpointId,
) -> Result<bool, StoreError> {
    Ok(connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM checkpoints WHERE checkpoint_id = ?1)",
        [checkpoint_id.as_str()],
        |row| row.get(0),
    )?)
}

/// `view` of the latest Checkpoint, read as damaged when any Checkpoint of
/// the Task, or the latest one's Snapshot or Evidence, disagrees with what
/// `memproved`'s own Events recorded (P2-E), or when a Checkpoint those
/// Events record for the Task (`announced`) is not one of its rows (P2-K).
fn agree_with_ledger(
    connection: &Connection,
    task_id: &TaskId,
    references: &LedgerReferences,
    announced: &[&CheckpointId],
    view: CheckpointView,
) -> Result<CheckpointView, StoreError> {
    let CheckpointView::Intact {
        checkpoint,
        snapshot,
        evidence,
    } = view
    else {
        return Ok(view);
    };
    let rows = connection
        .prepare(
            "SELECT checkpoint_id, digest, project_event_sequence FROM checkpoints
             WHERE task_id = ?1",
        )?
        .query_map([task_id.as_str()], |row| {
            Ok((
                lossy_text(row, 0)?,
                lossy_text(row, 1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let all_present = announced
        .iter()
        .all(|announced| rows.iter().any(|(id, _, _)| id == announced.as_str()));
    let checkpoints_agree = rows.iter().all(|(id, digest, sequence)| {
        match (
            CheckpointId::parse(id),
            Sha256Digest::parse(digest),
            u64::try_from(*sequence),
        ) {
            (Ok(id), Ok(digest), Ok(sequence)) => references
                .checkpoint_failure(&id, &digest, sequence)
                .is_none(),
            _ => false,
        }
    });
    let (snapshot_failure, evidence_failures) = references.sealed_failures(&snapshot, &evidence);
    let invalid_evidence_ids = evidence_failures
        .into_iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if all_present
        && checkpoints_agree
        && snapshot_failure.is_none()
        && invalid_evidence_ids.is_empty()
    {
        return Ok(CheckpointView::Intact {
            checkpoint,
            snapshot,
            evidence,
        });
    }
    Ok(CheckpointView::Damaged {
        checkpoint_id: checkpoint.checkpoint_id().clone(),
        stored_digest: Some(checkpoint.digest().clone()),
        missing_object: !all_present,
        invalid_evidence_ids,
    })
}

/// The Checkpoint with recomputed digests, or what can be said of it when
/// they do not recompute.
fn checkpoint_view(
    connection: &Connection,
    checkpoint_id: &CheckpointId,
) -> Result<CheckpointView, StoreError> {
    let intact = checkpoint_by_id(connection, checkpoint_id).and_then(|checkpoint| {
        let checkpoint = checkpoint.ok_or_else(|| {
            StoreError::PersistedDataIntegrity("the latest Checkpoint is missing".to_owned())
        })?;
        let snapshot = snapshot_by_id(connection, checkpoint.snapshot_id())?.ok_or_else(|| {
            StoreError::PersistedDataIntegrity("Checkpoint Snapshot is missing".to_owned())
        })?;
        let evidence = snapshot
            .all_evidence_ids()
            .map(|id| {
                evidence_by_id(connection, id)?.ok_or_else(|| {
                    StoreError::PersistedDataIntegrity("Snapshot Evidence is missing".to_owned())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CheckpointView::Intact {
            checkpoint,
            snapshot,
            evidence,
        })
    });
    match intact {
        Err(error) if is_integrity_failure(&error) => damaged(connection, checkpoint_id),
        other => other,
    }
}

/// What the raw rows of a Checkpoint that does not recompute still show:
/// its stored digest, whether a referenced record is missing, and which
/// Evidence does not recompute.
fn damaged(
    connection: &Connection,
    checkpoint_id: &CheckpointId,
) -> Result<CheckpointView, StoreError> {
    let (snapshot_id, digest) = connection.query_row(
        "SELECT snapshot_id, digest FROM checkpoints WHERE checkpoint_id = ?1",
        [checkpoint_id.as_str()],
        |row| Ok((lossy_text(row, 0)?, lossy_text(row, 1)?)),
    )?;
    let snapshot_present = RepositorySnapshotId::parse(&snapshot_id).is_ok()
        && connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM repository_snapshots WHERE snapshot_id = ?1)",
            [&snapshot_id],
            |row| row.get::<_, bool>(0),
        )?;
    let referenced = connection
        .prepare(
            "SELECT evidence_id FROM repository_snapshot_evidence WHERE snapshot_id = ?1
             UNION
             SELECT evidence_id FROM checkpoint_item_evidence WHERE checkpoint_id = ?2
             ORDER BY evidence_id",
        )?
        .query_map(params![snapshot_id, checkpoint_id.as_str()], |row| {
            lossy_text(row, 0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut missing_object = !snapshot_present;
    let mut invalid_evidence_ids = Vec::new();
    for id in referenced {
        let Ok(evidence_id) = EvidenceId::parse(&id) else {
            missing_object = true;
            continue;
        };
        match evidence_by_id(connection, &evidence_id) {
            Ok(Some(_)) => {}
            Ok(None) => missing_object = true,
            Err(error) if is_integrity_failure(&error) => invalid_evidence_ids.push(evidence_id),
            Err(error) => return Err(error),
        }
    }
    Ok(CheckpointView::Damaged {
        checkpoint_id: checkpoint_id.clone(),
        stored_digest: Sha256Digest::parse(&digest).ok(),
        missing_object,
        invalid_evidence_ids,
    })
}
