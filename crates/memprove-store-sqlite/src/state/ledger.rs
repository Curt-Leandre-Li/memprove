//! The read-only ledger check behind `verifyLedger` (P2-C).
//!
//! One read transaction recomputes the Project's Event chain row by row,
//! then every Evidence object, `RepositorySnapshot`, and Checkpoint. A row
//! that does not decode or recompute is reported as the first failure, not
//! as an error; only database failures are errors. Nothing is written.
//!
//! P2-E (T9 S3): a record that recomputes must also agree with the ledger.
//! Its digest must equal the one `memproved`'s own `command.completed` or
//! `checkpoint.created` Event recorded for it, a Checkpoint's
//! `project_event_sequence` must be the sequence just before its
//! `checkpoint.created`, and a Task row's status and revision must be those
//! of its last `task.created`/`task.transitioned` Event. The Recovery
//! Preview applies the same [`LedgerReferences`] to its anchor.
//!
//! P2-K (T11 S-1): the other direction too. Every record such an Event
//! announces must exist; a deleted one is reported with the Event's Project
//! sequence. Task completion applies the same agreement to the Evidence it
//! cites ([`StateStore::completion_evidence`], T11 S-2).

use std::collections::{BTreeMap, BTreeSet};

use memprove_domain::{
    CheckpointId, CompletionEvidence, DAEMON_PRODUCER_NAME, EventChainVerifier, Evidence,
    EvidenceId, LedgerFailure, LedgerRecordType, LedgerVerification, RepositorySnapshot,
    RepositorySnapshotId, SESSION_CRASHED_ENDED_BY_SCHEMA, SESSION_STARTED_AGENT_SCHEMA,
    SessionEndedBy, Sha256Digest, TaskId,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use super::{
    EVENT_COLUMNS, StateStore,
    capsule::crashed_ended_by,
    event_from_row, latest_project_sequence, lossy_optional_text, lossy_text,
    recovery::{checkpoint_by_id, evidence_by_id, evidence_record, snapshot_by_id},
    root_workspace_id, stored_json, task_from_row, workspace_from_row,
};
use crate::{StoreError, error::is_decode_failure};

impl StateStore {
    /// Recomputes the whole ledger of this Project: the Event chain in
    /// Project order, then Evidence, Snapshots, and Checkpoints in identity
    /// order. Every row is counted; the first that does not hold is named.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] only when the database cannot be read.
    pub fn verify_ledger(&self) -> Result<LedgerVerification, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let verification = verify(&transaction)?;
        transaction.commit()?;
        Ok(verification)
    }

    /// One cited completion Evidence object with the first Snapshot that
    /// seals it and that Snapshot's Evidence, every digest recomputed, read
    /// in one read transaction (P2-C). P2-K (T11 S-2): the Snapshot and each
    /// of its Evidence objects must also agree with the ledger, the check the
    /// Recovery Preview applies to its anchor
    /// ([`LedgerReferences::sealed_failures`]).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] for unknown Evidence,
    /// [`StoreError::PersistedDataIntegrity`] for a record that does not
    /// recompute or disagrees with the ledger, or a database failure.
    pub fn completion_evidence(
        &self,
        evidence_id: &EvidenceId,
    ) -> Result<CompletionEvidence, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let record = evidence_record(&transaction, evidence_id)?;
        let snapshot = record.snapshots.into_iter().next();
        let snapshot_evidence = match &snapshot {
            Some(snapshot) => snapshot
                .all_evidence_ids()
                .map(|id| evidence_by_id(&transaction, id)?.ok_or(StoreError::NotFound("Evidence")))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        let references = LedgerReferences::read(&transaction)?;
        let failure = match &snapshot {
            Some(snapshot) => {
                let (snapshot_failure, evidence_failures) =
                    references.sealed_failures(snapshot, &snapshot_evidence);
                snapshot_failure
                    .map(|reason| format!("its Snapshot {}: {reason}", snapshot.snapshot_id()))
                    .or_else(|| {
                        evidence_failures
                            .into_iter()
                            .next()
                            .map(|(id, reason)| format!("Evidence {id}: {reason}"))
                    })
            }
            None => references
                .evidence_failure(
                    record.evidence.evidence_id(),
                    record.evidence.canonical_digest(),
                )
                .map(|reason| format!("Evidence {evidence_id}: {reason}")),
        };
        transaction.commit()?;
        if let Some(reason) = failure {
            return Err(StoreError::PersistedDataIntegrity(format!(
                "completion Evidence {evidence_id} disagrees with the ledger: {reason}"
            )));
        }
        Ok(CompletionEvidence {
            evidence: record.evidence,
            snapshot,
            snapshot_evidence,
        })
    }
}

fn verify(connection: &Connection) -> Result<LedgerVerification, StoreError> {
    let mut first_failure = None;
    let mut note = |failure: LedgerFailure| {
        if first_failure.is_none() {
            first_failure = Some(failure);
        }
    };
    let (events, head_event_sha256) = verify_events(connection, &mut note)?;
    let references = LedgerReferences::read(connection)?;
    let evidence = verify_records(
        connection,
        "SELECT evidence_id FROM evidence ORDER BY evidence_id",
        (&references.evidence, ANNOUNCED_BY),
        LedgerRecordType::Evidence,
        |connection, id| match EvidenceId::parse(id) {
            Ok(id) => present(evidence_by_id(connection, &id), |evidence| {
                references.evidence_failure(evidence.evidence_id(), evidence.canonical_digest())
            }),
            Err(error) => Ok(Some(error.to_string())),
        },
        &mut note,
    )?;
    let snapshots = verify_records(
        connection,
        "SELECT snapshot_id FROM repository_snapshots ORDER BY snapshot_id",
        (&references.snapshots, ANNOUNCED_BY),
        LedgerRecordType::Snapshot,
        |connection, id| match RepositorySnapshotId::parse(id) {
            Ok(id) => present(snapshot_by_id(connection, &id), |snapshot| {
                references.snapshot_failure(snapshot)
            }),
            Err(error) => Ok(Some(error.to_string())),
        },
        &mut note,
    )?;
    let checkpoints = verify_records(
        connection,
        "SELECT checkpoint_id FROM checkpoints ORDER BY checkpoint_id",
        (&references.checkpoints, "checkpoint.created"),
        LedgerRecordType::Checkpoint,
        |connection, id| match CheckpointId::parse(id) {
            Ok(id) => present(checkpoint_by_id(connection, &id), |checkpoint| {
                references.checkpoint_failure(
                    checkpoint.checkpoint_id(),
                    checkpoint.digest(),
                    checkpoint.project_event_sequence(),
                )
            }),
            Err(error) => Ok(Some(error.to_string())),
        },
        &mut note,
    )?;
    let tasks = verify_tasks(connection, &mut note)?;
    verify_workspaces(connection, &mut note)?;
    verify_root(connection, &mut note)?;
    verify_session_agents(connection, &mut note)?;
    verify_session_enders(connection, &mut note)?;
    Ok(LedgerVerification {
        project_sequence: latest_project_sequence(connection)?,
        events,
        evidence,
        snapshots,
        checkpoints,
        tasks,
        head_event_sha256,
        first_failure,
    })
}

/// What one `memproved` Event recorded about a record: its Project
/// sequence and the digest it named.
type Recorded = Vec<(u64, String)>;

/// The Event types that announce Evidence and Snapshots.
const ANNOUNCED_BY: &str = "command.completed or checkpoint.created";

/// One `checkpoint.created` Event of `memproved`: its Project sequence,
/// its Session, and the Checkpoint it records (P2-K).
#[derive(Debug)]
pub(super) struct CheckpointAnnouncement {
    pub(super) sequence: u64,
    pub(super) session_id: String,
    pub(super) checkpoint_id: CheckpointId,
}

/// The digests `memproved`'s own `command.completed` and
/// `checkpoint.created` Events recorded for the Evidence, Snapshots, and
/// Checkpoints they announce, by stored identity. Only Events produced by
/// `memproved` on a Session count; since P2-E no client can write one.
#[derive(Debug, Default)]
pub(super) struct LedgerReferences {
    evidence: BTreeMap<String, Recorded>,
    snapshots: BTreeMap<String, Recorded>,
    checkpoints: BTreeMap<String, Recorded>,
    /// Every `checkpoint.created` Event in Project order (P2-K).
    announcements: Vec<CheckpointAnnouncement>,
    /// U1 B1 (T21 V1R-1): the `recorded_at` of the first
    /// `command.completed` Event that records each Snapshot, by stored
    /// identity: the capture that wrote it.
    snapshot_recorded_at: BTreeMap<String, String>,
    /// The Session of each Event that announces a Snapshot or an Evidence
    /// object, with the record's type and stored identity (P3-C2), and the
    /// Workspace the Event was recorded in, when it names one (P3-D3).
    records: Vec<(String, LedgerRecordType, String, Option<String>)>,
}

impl LedgerReferences {
    /// Reads every announcing Event. A payload that does not parse names
    /// nothing; the chain check already reports a payload that was altered.
    /// P4-H1: so does a payload that is not UTF-8, and a Session or
    /// Workspace identity that is not reads as one that names nothing
    /// ([`lossy_text`]).
    pub(super) fn read(connection: &Connection) -> Result<Self, StoreError> {
        let mut references = Self::default();
        let mut statement = connection.prepare(
            "SELECT project_sequence, session_id, event_type, payload, workspace_id, recorded_at
             FROM events
             WHERE producer_name = ?1 AND session_id IS NOT NULL
               AND event_type IN ('command.completed', 'checkpoint.created')
             ORDER BY project_sequence",
        )?;
        let rows = statement
            .query_map([DAEMON_PRODUCER_NAME], |row| {
                Ok((
                    stored_sequence(row, 0)?,
                    lossy_text(row, 1)?,
                    lossy_text(row, 2)?,
                    stored_json(row, 3)?,
                    lossy_optional_text(row, 4)?,
                    lossy_text(row, 5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (sequence, session_id, event_type, payload, workspace_id, recorded_at) in rows {
            let (Some(sequence), Some(payload)) = (sequence, payload) else {
                continue;
            };
            let note = |map: &mut BTreeMap<String, Recorded>, item: &Value, id, digest| {
                if let (Some(id), Some(digest)) = (
                    item.get(id).and_then(Value::as_str),
                    item.get(digest).and_then(Value::as_str),
                ) {
                    map.entry(id.to_owned())
                        .or_default()
                        .push((sequence, digest.to_owned()));
                }
            };
            let mut announce = |record_type, item: &Value, id: &str| {
                if let Some(id) = item.get(id).and_then(Value::as_str) {
                    references.records.push((
                        session_id.clone(),
                        record_type,
                        id.to_owned(),
                        workspace_id.clone(),
                    ));
                }
            };
            for item in payload
                .get("evidence")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                announce(LedgerRecordType::Evidence, item, "evidence_id");
            }
            if let Some(item) = payload.get("repository_snapshot") {
                announce(LedgerRecordType::Snapshot, item, "snapshot_id");
            }
            for item in payload
                .get("evidence")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                note(
                    &mut references.evidence,
                    item,
                    "evidence_id",
                    "canonical_digest",
                );
            }
            if let Some(item) = payload.get("repository_snapshot") {
                note(
                    &mut references.snapshots,
                    item,
                    "snapshot_id",
                    "canonical_digest",
                );
                references.note_capture_time(&event_type, item, &recorded_at);
            }
            if let Some(item) = payload.get("checkpoint") {
                note(&mut references.checkpoints, item, "checkpoint_id", "digest");
                if let (true, Some(Ok(checkpoint_id))) = (
                    event_type == "checkpoint.created",
                    item.get("checkpoint_id")
                        .and_then(Value::as_str)
                        .map(CheckpointId::parse),
                ) {
                    references.announcements.push(CheckpointAnnouncement {
                        sequence,
                        session_id,
                        checkpoint_id,
                    });
                }
            }
        }
        Ok(references)
    }

    /// U1 B1: records the time of the capture a `command.completed` Event
    /// records for the Snapshot `item` names, the first one only.
    fn note_capture_time(&mut self, event_type: &str, item: &Value, recorded_at: &str) {
        if let (true, Some(id)) = (
            event_type == "command.completed",
            item.get("snapshot_id").and_then(Value::as_str),
        ) {
            self.snapshot_recorded_at
                .entry(id.to_owned())
                .or_insert_with(|| recorded_at.to_owned());
        }
    }

    /// The `checkpoint.created` Events on any of `sessions`, in Project
    /// order: the Checkpoints the ledger says a Task has (P2-K).
    pub(super) fn announced_on<'a>(
        &'a self,
        sessions: &'a BTreeSet<String>,
    ) -> impl Iterator<Item = &'a CheckpointAnnouncement> {
        self.announcements
            .iter()
            .filter(|announcement| sessions.contains(&announcement.session_id))
    }

    /// The Snapshots and Evidence objects the `command.completed` and
    /// `checkpoint.created` Events on any of `sessions` announce, by type and
    /// stored identity (P3-C2: the records the ledger says a Task has), with
    /// the Workspace each Event was recorded in (P3-D3: the record's).
    pub(super) fn records_announced_on<'a>(
        &'a self,
        sessions: &'a BTreeSet<String>,
    ) -> impl Iterator<Item = (LedgerRecordType, &'a str, Option<&'a str>)> {
        self.records
            .iter()
            .filter(|(session_id, _, _, _)| sessions.contains(session_id))
            .map(|(_, record_type, id, workspace_id)| {
                (*record_type, id.as_str(), workspace_id.as_deref())
            })
    }

    /// The Project sequence of the first Event that records `snapshot_id`
    /// (ADR 0013: a run's ledger position).
    pub(super) fn snapshot_sequence(&self, snapshot_id: &RepositorySnapshotId) -> Option<u64> {
        self.stored_snapshot_sequence(snapshot_id.as_str())
    }

    /// [`Self::snapshot_sequence`] of a stored Snapshot identity.
    fn stored_snapshot_sequence(&self, snapshot_id: &str) -> Option<u64> {
        self.snapshots
            .get(snapshot_id)
            .and_then(|recorded| recorded.first())
            .map(|(sequence, _)| *sequence)
    }

    /// P4-B (P4-0 item D): sorts stored Snapshot identities by their ledger
    /// position, then identity; a Snapshot no Event records comes last.
    pub(super) fn sort_snapshots_by_ledger(&self, snapshot_ids: &mut [String]) {
        snapshot_ids.sort_by(|left, right| {
            let key = |id: &String| {
                (
                    self.stored_snapshot_sequence(id).unwrap_or(u64::MAX),
                    id.clone(),
                )
            };
            key(left).cmp(&key(right))
        });
    }

    /// Why a Snapshot, and which of its Evidence objects, disagree with the
    /// ledger: the check the Recovery Preview applies to its anchor and Task
    /// completion to each cited Evidence object (P2-K, T11 S-2).
    pub(super) fn sealed_failures<'a>(
        &self,
        snapshot: &RepositorySnapshot,
        evidence: &'a [Evidence],
    ) -> (Option<String>, Vec<(&'a EvidenceId, String)>) {
        let snapshot_failure = self.snapshot_failure(snapshot);
        let evidence_failures = evidence
            .iter()
            .filter_map(|item| {
                self.evidence_failure(item.evidence_id(), item.canonical_digest())
                    .map(|reason| (item.evidence_id(), reason))
            })
            .collect();
        (snapshot_failure, evidence_failures)
    }

    fn digest_failure(
        recorded: Option<&Recorded>,
        stored: &Sha256Digest,
        announced_by: &str,
    ) -> Option<String> {
        let Some(recorded) = recorded else {
            return Some(format!("no {announced_by} Event of memproved records it"));
        };
        recorded
            .iter()
            .find(|(_, digest)| Sha256Digest::parse(digest).as_ref() != Ok(stored))
            .map(|(sequence, digest)| {
                format!(
                    "the {announced_by} Event at Project sequence {sequence} records digest \
                     {digest}, the record recomputes to {stored}"
                )
            })
    }

    /// Why Evidence with this recomputed digest disagrees with the ledger.
    pub(super) fn evidence_failure(
        &self,
        evidence_id: &EvidenceId,
        digest: &Sha256Digest,
    ) -> Option<String> {
        Self::digest_failure(
            self.evidence.get(evidence_id.as_str()),
            digest,
            ANNOUNCED_BY,
        )
    }

    /// Why a Snapshot, recomputed, disagrees with the ledger: its digest, or
    /// (U1 B1, T21 V1R-1) a `captured_at` that is not the `recorded_at` of
    /// the `command.completed` Event of the capture that wrote it:
    /// `captureEvidence` writes both from one time, and no digest covers
    /// `captured_at`. A Snapshot no `command.completed` Event records (the
    /// test-only checkpoint bundle, or a ledger older than P2-A) has no
    /// such time to compare with.
    pub(super) fn snapshot_failure(&self, snapshot: &RepositorySnapshot) -> Option<String> {
        let snapshot_id = snapshot.snapshot_id().as_str();
        if let Some(failure) = Self::digest_failure(
            self.snapshots.get(snapshot_id),
            snapshot.canonical_digest(),
            ANNOUNCED_BY,
        ) {
            return Some(failure);
        }
        let captured_at = snapshot.captured_at().as_str();
        self.snapshot_recorded_at
            .get(snapshot_id)
            .filter(|recorded_at| recorded_at.as_str() != captured_at)
            .map(|recorded_at| {
                format!(
                    "captured_at {captured_at} is not the recorded_at {recorded_at} of the \
                     command.completed Event that records it"
                )
            })
    }

    /// Why a Checkpoint disagrees with the ledger: its digest, or a
    /// `project_event_sequence` that is not the sequence just before its
    /// `checkpoint.created` Event (the store appends that Event at the next
    /// sequence in the same transaction).
    pub(super) fn checkpoint_failure(
        &self,
        checkpoint_id: &CheckpointId,
        digest: &Sha256Digest,
        project_event_sequence: u64,
    ) -> Option<String> {
        let recorded = self.checkpoints.get(checkpoint_id.as_str());
        if let Some(failure) = Self::digest_failure(recorded, digest, "checkpoint.created") {
            return Some(failure);
        }
        let expected = project_event_sequence.checked_add(1);
        recorded
            .into_iter()
            .flatten()
            .find(|(sequence, _)| Some(*sequence) != expected)
            .map(|(sequence, _)| {
                format!(
                    "project_event_sequence {project_event_sequence} does not precede its \
                     checkpoint.created Event at Project sequence {sequence}"
                )
            })
    }
}

/// A stored Project sequence; `None` when the row holds none, or one below
/// 1, which no row the store wrote holds (CHECK `BETWEEN 1 AND 2^53 - 1`;
/// P4-H1: such a row names nothing, and the chain check reports it).
pub(super) fn stored_sequence(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<u64>> {
    Ok(row
        .get::<_, Option<i64>>(index)?
        .and_then(|sequence| u64::try_from(sequence).ok())
        .filter(|sequence| *sequence >= 1))
}

/// The Project sequence, status, and revision one Task Event recorded.
pub(super) type TaskRecord = (u64, Option<String>, Option<u64>);

/// The status and revision the last `task.created` or `task.transitioned`
/// Event of `memproved` (without a Session) recorded for each Task.
pub(super) fn last_task_events(
    connection: &Connection,
) -> Result<BTreeMap<String, TaskRecord>, StoreError> {
    Ok(task_chains(connection)?
        .into_iter()
        .map(|(task_id, chain)| (task_id, chain.last))
        .collect())
}

/// What the Task Events of `memproved` (without a Session) record about
/// one Task.
#[derive(Debug, Clone)]
pub(super) struct TaskChain {
    /// The Project sequence of the first Task Event.
    pub(super) first: u64,
    /// What the last Task Event recorded.
    pub(super) last: TaskRecord,
    /// U1 B1: the `recorded_at` of the Task's `task.created` Event, `None`
    /// without one.
    pub(super) created_at: Option<String>,
    /// U1 B1: the `recorded_at` of the last Task Event.
    pub(super) updated_at: String,
}

impl TaskChain {
    /// U1 B1 (T21 V1R-1): why a Task row's `project_id`, `created_at`, or
    /// `updated_at`, as stored, disagrees with the state database's Project
    /// (`state_project`) or the chain: `createTask` writes the row and its
    /// `task.created` Event from one time, and `transitionTask` the row's
    /// `updated_at` and its `task.transitioned` Event; no digest covers
    /// these columns.
    pub(super) fn row_failure(
        &self,
        state_project: &str,
        project_id: &str,
        created_at: &str,
        updated_at: &str,
    ) -> Option<String> {
        if project_id != state_project {
            return Some(format!(
                "project_id {project_id} is not the Project of the state database, \
                 {state_project}"
            ));
        }
        if let Some(recorded) = &self.created_at
            && recorded != created_at
        {
            return Some(format!(
                "created_at {created_at} is not the recorded_at {recorded} of its task.created \
                 Event"
            ));
        }
        (updated_at != self.updated_at).then(|| {
            format!(
                "updated_at {updated_at} is not the recorded_at {} of its last Task Event at \
                 Project sequence {}",
                self.updated_at, self.last.0
            )
        })
    }
}

/// The Project identity the state database is bound to, as stored; empty
/// when there is none (the database would not have opened).
pub(super) fn state_project(connection: &Connection) -> Result<String, StoreError> {
    Ok(connection
        .query_row(
            "SELECT project_id FROM state_metadata WHERE metadata_key = 1",
            [],
            |row| lossy_text(row, 0),
        )
        .optional()?
        .unwrap_or_default())
}

/// For each Task a `task.created` or `task.transitioned` Event of
/// `memproved` (without a Session) names: the Project sequence of the
/// first such Event, what the last one recorded, and (U1 B1) the times of
/// its `task.created` and its last Task Event.
pub(super) fn task_chains(
    connection: &Connection,
) -> Result<BTreeMap<String, TaskChain>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT project_sequence, event_type, payload, recorded_at FROM events
         WHERE producer_name = ?1 AND session_id IS NULL
           AND event_type IN ('task.created', 'task.transitioned')
         ORDER BY project_sequence",
    )?;
    let rows = statement
        .query_map([DAEMON_PRODUCER_NAME], |row| {
            Ok((
                stored_sequence(row, 0)?,
                lossy_text(row, 1)?,
                stored_json(row, 2)?,
                lossy_text(row, 3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut events = BTreeMap::<String, TaskChain>::new();
    for (sequence, event_type, payload, recorded_at) in rows {
        let (Some(sequence), Some(payload)) = (sequence, payload) else {
            continue;
        };
        let Some(task_id) = payload.get("task_id").and_then(Value::as_str) else {
            continue;
        };
        let status = payload
            .get(if event_type == "task.created" {
                "status"
            } else {
                "to"
            })
            .and_then(Value::as_str)
            .map(str::to_owned);
        let revision = payload.get("revision").and_then(Value::as_u64);
        let record = (sequence, status, revision);
        let chain = events
            .entry(task_id.to_owned())
            .or_insert_with(|| TaskChain {
                first: sequence,
                last: record.clone(),
                created_at: None,
                updated_at: recorded_at.clone(),
            });
        chain.last = record;
        chain.updated_at.clone_from(&recorded_at);
        if event_type == "task.created" && chain.created_at.is_none() {
            chain.created_at = Some(recorded_at);
        }
    }
    Ok(events)
}

/// One `tasks` row as `verifyLedger` reads it; text that is not UTF-8
/// reads lossily (P4-H1).
struct TaskRow {
    rowid: i64,
    task_id: String,
    status: String,
    revision: i64,
    project_id: String,
    created_at: String,
    updated_at: String,
}

/// Why a Task row that no Task Event names fails (U1 B2).
const NO_TASK_EVENT: &str =
    "no task.created or task.transitioned Event of memproved records this Task";

/// Compares every Task row with its last Task Event. U1 B2 (T21 V1R-3): a
/// Task row that no Task Event names is a finding. Since P2-C every Task is
/// created with its `task.created` Event in one transaction, and a ledger
/// written before P2-E (which includes every Task from before migration
/// 0007) is not supported (Owner, P3-C2); before, such a row was counted
/// and not compared. P4-H1 (Owner, T20 SF-2): a row whose `task_id` does not decode is a
/// finding, whether or not an Event names it.
///
/// T21 (V1-S-2): so is a row any other column of which does not decode as
/// the Task Capsule and `listTasks` decode it (`project_id`, `title`,
/// `created_at`, `updated_at`, `workspace_id`, `description`), and, after
/// the rows, a Task a Task Event names that no row holds, reported missing
/// with the Project sequence of its first Task Event (as P2-K reports
/// Evidence, Snapshots, and Checkpoints); it is counted too.
fn verify_tasks(
    connection: &Connection,
    note: &mut impl FnMut(LedgerFailure),
) -> Result<u64, StoreError> {
    let events = task_chains(connection)?;
    let project = state_project(connection)?;
    let rows = connection
        .prepare(
            "SELECT rowid, task_id, status, revision, project_id, created_at, updated_at
             FROM tasks ORDER BY task_id",
        )?
        .query_map([], |row| {
            Ok(TaskRow {
                rowid: row.get::<_, i64>(0)?,
                task_id: lossy_text(row, 1)?,
                status: lossy_text(row, 2)?,
                revision: row.get::<_, i64>(3)?,
                project_id: lossy_text(row, 4)?,
                created_at: lossy_text(row, 5)?,
                updated_at: lossy_text(row, 6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for TaskRow {
        rowid,
        task_id,
        status,
        revision,
        project_id,
        created_at,
        updated_at,
    } in &rows
    {
        if let Err(error) = TaskId::parse(task_id) {
            note(LedgerFailure {
                record_type: LedgerRecordType::Task,
                record_id: task_id.clone(),
                project_sequence: None,
                reason: error.to_string(),
            });
            continue;
        }
        let Some(chain) = events.get(task_id) else {
            // U1 B2 (T21 V1R-3): the chain does not record this Task.
            note(LedgerFailure {
                record_type: LedgerRecordType::Task,
                record_id: task_id.clone(),
                project_sequence: None,
                reason: NO_TASK_EVENT.to_owned(),
            });
            continue;
        };
        let (sequence, recorded_status, recorded_revision) = &chain.last;
        let revision = u64::try_from(*revision).ok();
        if recorded_status.as_deref() != Some(status.as_str()) || *recorded_revision != revision {
            note(LedgerFailure {
                record_type: LedgerRecordType::Task,
                record_id: task_id.clone(),
                project_sequence: None,
                reason: format!(
                    "status {status} at revision {} disagrees with its last Task Event at \
                     Project sequence {sequence} ({} at revision {})",
                    revision.map_or_else(|| "?".to_owned(), |value| value.to_string()),
                    recorded_status.as_deref().unwrap_or("?"),
                    recorded_revision.map_or_else(|| "?".to_owned(), |value| value.to_string()),
                ),
            });
            continue;
        }
        let reason = match undecodable_task_row(connection, *rowid)? {
            Some(reason) => Some(reason),
            None => chain.row_failure(&project, project_id, created_at, updated_at),
        };
        if let Some(reason) = reason {
            note(LedgerFailure {
                record_type: LedgerRecordType::Task,
                record_id: task_id.clone(),
                project_sequence: None,
                reason,
            });
        }
    }
    let stored = rows
        .iter()
        .map(|row| row.task_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut missing = 0_usize;
    for (task_id, TaskChain { first, .. }) in &events {
        if stored.contains(task_id.as_str()) {
            continue;
        }
        missing += 1;
        note(LedgerFailure {
            record_type: LedgerRecordType::Task,
            record_id: task_id.clone(),
            project_sequence: Some(*first),
            reason: format!(
                "record is missing; the task.created or task.transitioned Event at Project \
                 sequence {first} records it"
            ),
        });
    }
    Ok(u64::try_from(rows.len() + missing).unwrap_or(u64::MAX))
}

/// U1 B1 (T21 V1R-1): every Workspace row decodes as the reads decode it
/// (`workspace_from_row`) and names the Project of the state database,
/// which the schema's insert trigger enforces and no digest covers; a row
/// that does not is a `workspace` failure, in identity order, after every
/// Task. A Workspace is not counted in `checked`.
fn verify_workspaces(
    connection: &Connection,
    note: &mut impl FnMut(LedgerFailure),
) -> Result<(), StoreError> {
    let project = state_project(connection)?;
    let rows = connection
        .prepare("SELECT rowid, workspace_id FROM workspaces ORDER BY workspace_id")?
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, lossy_text(row, 1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (rowid, workspace_id) in rows {
        let reason = match connection.query_row(
            "SELECT workspace_id, project_id, canonical_worktree_path, git_dir, head_commit,
                    branch_or_detached_state, created_at, revision
             FROM workspaces WHERE rowid = ?1",
            [rowid],
            workspace_from_row,
        ) {
            Ok(workspace) if workspace.project_id.as_str() == project => None,
            Ok(workspace) => Some(format!(
                "project_id {} is not the Project of the state database, {project}",
                workspace.project_id
            )),
            Err(error) if is_decode_failure(&error) => Some(decode_reason(&error)),
            Err(error) => return Err(error.into()),
        };
        if let Some(reason) = reason {
            note(LedgerFailure {
                record_type: LedgerRecordType::Workspace,
                record_id: workspace_id,
                project_sequence: None,
                reason,
            });
        }
    }
    Ok(())
}

/// U1 B4 (original #32, Owner O5): the Project's root Workspace row
/// ([`super::root_workspace_id`]) must exist. When none is the root, the
/// failure names the root as the chain records it, the Workspace of the
/// first `task.created` Event of `memproved` of a Task without a scope
/// (such Events are recorded under the root): `record is missing` when no
/// row holds that identity, with that Event's Project sequence; that the
/// row is not the root (it is not of the Project's repository, or a
/// `workspace.registered` Event announces it) otherwise. Without such an
/// Event the record ID is empty. A Workspace is not counted in `checked`.
fn verify_root(
    connection: &Connection,
    note: &mut impl FnMut(LedgerFailure),
) -> Result<(), StoreError> {
    if root_workspace_id(connection)?.is_some() {
        return Ok(());
    }
    let recorded = connection
        .prepare(
            "SELECT project_sequence, workspace_id, payload FROM events
             WHERE producer_name = ?1 AND session_id IS NULL AND event_type = 'task.created'
             ORDER BY project_sequence",
        )?
        .query_map([DAEMON_PRODUCER_NAME], |row| {
            Ok((
                stored_sequence(row, 0)?,
                lossy_optional_text(row, 1)?,
                stored_json(row, 2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .find_map(|(sequence, workspace_id, payload)| {
            let unscoped = payload?.get("workspace_id").is_some_and(Value::is_null);
            unscoped.then_some((sequence?, workspace_id?))
        });
    let failure = match recorded {
        Some((sequence, workspace_id)) => {
            let present = connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM workspaces WHERE workspace_id = ?1)",
                [&workspace_id],
                |row| row.get::<_, bool>(0),
            )?;
            LedgerFailure {
                record_type: LedgerRecordType::Workspace,
                record_id: workspace_id,
                project_sequence: Some(sequence),
                reason: if present {
                    format!(
                        "the row is not the Project's root Workspace: it is not a worktree of \
                         the Project's repository, or a workspace.registered Event announces \
                         it; the task.created Event at Project sequence {sequence} records it \
                         as the root"
                    )
                } else {
                    format!(
                        "record is missing; the task.created Event at Project sequence \
                         {sequence} records it as the root Workspace"
                    )
                },
            }
        }
        None => LedgerFailure {
            record_type: LedgerRecordType::Workspace,
            record_id: String::new(),
            project_sequence: None,
            reason: "no Workspace row is the Project's root: none is a worktree of the \
                     Project's repository that no workspace.registered Event announces"
                .to_owned(),
        },
    };
    note(failure);
    Ok(())
}

/// The three agent columns of migration 0008, in the order U8 A1 compares
/// them.
const AGENT_COLUMNS: [&str; 3] = ["agent_name", "agent_role", "parent_session_id"];

/// U8 A1 (Owner O4; ADR 0014, "Amendment (U8, S-A)"): every Session row's
/// `agent_name`, `agent_role`, and `parent_session_id` are the `name`,
/// `role`, and `parent_session_id` of the `agent` its first
/// `session.started` Event of `memproved` recorded (`session.started@3`),
/// and all three are NULL for a `session.started@1` Event, which records no
/// agent. No digest covers the row. A row that disagrees is reported on
/// that Event: record type `event` (the wire's `first_failure` names no
/// Session record type), the Event's identity and Project sequence, and a
/// reason naming the Session and the first column that disagrees. Checked
/// after every other record; a Session row without such an Event (before
/// P2-A) or such an Event without a row is not compared here (the Task
/// Capsule reports the latter). Sessions are not counted in `checked`.
fn verify_session_agents(
    connection: &Connection,
    note: &mut impl FnMut(LedgerFailure),
) -> Result<(), StoreError> {
    let started = connection
        .prepare(
            "SELECT event_id, project_sequence, session_id, coalesce(payload_schema, ''), payload
             FROM events
             WHERE producer_name = ?1 AND session_id IS NOT NULL
               AND event_type = 'session.started'
             ORDER BY project_sequence",
        )?
        .query_map([DAEMON_PRODUCER_NAME], |row| {
            Ok((
                lossy_text(row, 0)?,
                stored_sequence(row, 1)?,
                lossy_text(row, 2)?,
                lossy_text(row, 3)?,
                stored_json(row, 4)?.unwrap_or(Value::Null),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT agent_name, agent_role, parent_session_id FROM sessions WHERE session_id = ?1",
    )?;
    for (event_id, sequence, session_id, schema, payload) in started {
        if !seen.insert(session_id.clone()) {
            continue;
        }
        let Some(stored) = statement
            .query_row([&session_id], |row| {
                Ok([
                    lossy_optional_text(row, 0)?,
                    lossy_optional_text(row, 1)?,
                    lossy_optional_text(row, 2)?,
                ])
            })
            .optional()?
        else {
            continue;
        };
        let agent = (schema == SESSION_STARTED_AGENT_SCHEMA)
            .then(|| payload.get("agent"))
            .flatten();
        let recorded = ["name", "role", "parent_session_id"].map(|member| {
            agent
                .and_then(|agent| agent.get(member))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        if let Some(column) = AGENT_COLUMNS
            .iter()
            .zip(stored.iter().zip(&recorded))
            .find_map(|(column, (stored, recorded))| (stored != recorded).then_some(column))
        {
            note(LedgerFailure {
                record_type: LedgerRecordType::Event,
                record_id: event_id,
                project_sequence: sequence,
                reason: format!(
                    "Session {session_id}: its sessions row's {column} is not what this \
                     session.started Event recorded"
                ),
            });
        }
    }
    Ok(())
}

/// The three ender columns of migration 0009, in the order U9 A7 compares
/// them.
const ENDER_COLUMNS: [&str; 3] = [
    "ended_by_session_id",
    "ended_by_agent_name",
    "ended_by_agent_role",
];

/// U9 A7 (Owner O3; ADR 0010, "Amendment (U9)"): every Session row's
/// `ended_by_session_id`, `ended_by_agent_name`, and `ended_by_agent_role`
/// are the Session, agent name, and agent role of the `ended_by` its first
/// end Event of `memproved` recorded (`session.crashed@2`), and all three
/// are NULL for any other end (`session.ended`, `session.crashed@1`) and
/// for a Session whose end the ledger does not record. As U8 A1's check, a
/// row that disagrees is reported on an Event: the end Event, or the
/// Session's first `session.started` Event when there is none, with a
/// reason naming the Session and the first column that disagrees. Checked
/// last; a Session row without a `session.started` Event of `memproved`
/// (before P2-A) is not compared, nor counted.
fn verify_session_enders(
    connection: &Connection,
    note: &mut impl FnMut(LedgerFailure),
) -> Result<(), StoreError> {
    let events = connection
        .prepare(
            "SELECT event_id, project_sequence, session_id, event_type,
                    coalesce(payload_schema, ''), payload
             FROM events
             WHERE producer_name = ?1 AND session_id IS NOT NULL
               AND (event_type IN ('session.started', 'session.ended')
                    OR (event_type = 'session.crashed'
                        AND payload_schema IN ('session.crashed@1', ?2)))
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
                    lossy_text(row, 4)?,
                    stored_json(row, 5)?.unwrap_or(Value::Null),
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    // Each Session's first `session.started` Event and first end Event.
    let mut sessions: BTreeMap<String, [Option<usize>; 2]> = BTreeMap::new();
    let mut order = Vec::new();
    for (index, (_, _, session_id, event_type, ..)) in events.iter().enumerate() {
        let slots = sessions.entry(session_id.clone()).or_insert_with(|| {
            order.push(session_id.clone());
            [None, None]
        });
        let slot = usize::from(event_type != "session.started");
        slots[slot].get_or_insert(index);
    }
    let mut statement = connection.prepare(
        "SELECT ended_by_session_id, ended_by_agent_name, ended_by_agent_role
         FROM sessions WHERE session_id = ?1",
    )?;
    for session_id in order {
        let [Some(started), end] = sessions[&session_id] else {
            continue;
        };
        let Some(stored) = statement
            .query_row([&session_id], |row| {
                Ok([
                    lossy_optional_text(row, 0)?,
                    lossy_optional_text(row, 1)?,
                    lossy_optional_text(row, 2)?,
                ])
            })
            .optional()?
        else {
            continue;
        };
        let recorded = end.and_then(|end| {
            let (.., schema, payload) = &events[end];
            crashed_ended_by(schema, payload).unwrap_or_default()
        });
        let Some(column) = ENDER_COLUMNS
            .iter()
            .zip(
                stored
                    .iter()
                    .zip(&SessionEndedBy::columns(recorded.as_ref())),
            )
            .find_map(|(column, (stored, recorded))| (stored != recorded).then_some(column))
        else {
            continue;
        };
        let (event_id, sequence, _, event_type, ..) = &events[end.unwrap_or(started)];
        note(LedgerFailure {
            record_type: LedgerRecordType::Event,
            record_id: event_id.clone(),
            project_sequence: *sequence,
            reason: if end.is_some() {
                format!(
                    "Session {session_id}: its sessions row's {column} is not what this \
                     {event_type} Event recorded"
                )
            } else {
                format!(
                    "Session {session_id}: its sessions row's {column} is set, and the ledger \
                     records no end of the Session"
                )
            },
        });
    }
    Ok(())
}

/// Why the Task row at `rowid` does not decode as `listTasks` and the Task
/// Capsule decode it (`task_from_row`), `None` when it does. Only the row's
/// own columns are read: the completion Evidence, which comes from the
/// Event chain, is the chain check's.
fn undecodable_task_row(connection: &Connection, rowid: i64) -> Result<Option<String>, StoreError> {
    match connection.query_row(
        "SELECT task_id, project_id, title, status, created_at, updated_at, revision,
                workspace_id, description, NULL
         FROM tasks WHERE rowid = ?1",
        [rowid],
        task_from_row,
    ) {
        Ok(_) => Ok(None),
        Err(error) if is_decode_failure(&error) => Ok(Some(decode_reason(&error))),
        Err(error) => Err(error.into()),
    }
}

/// Walks every Event row in Project order (rows without a Project sequence,
/// which predate the envelope, first). Returns the row count and the stored
/// digest of the last row.
fn verify_events(
    connection: &Connection,
    note: &mut impl FnMut(LedgerFailure),
) -> Result<(u64, Option<Sha256Digest>), StoreError> {
    let mut statement = connection.prepare(&format!(
        "SELECT {EVENT_COLUMNS} FROM events ORDER BY project_sequence, event_id"
    ))?;
    let mut rows = statement.query([])?;
    let mut chain = EventChainVerifier::default();
    let mut count = 0_u64;
    let mut head = None;
    let mut broken = false;
    while let Some(row) = rows.next()? {
        count += 1;
        let event_id = lossy_optional_text(row, 0).ok().flatten();
        // P4-H1: a stored sequence below 1 is none (`stored_sequence`).
        let project_sequence = stored_sequence(row, 5).ok().flatten();
        head = row
            .get::<_, Option<String>>(19)
            .ok()
            .flatten()
            .and_then(|digest| Sha256Digest::parse(&digest).ok());
        if broken {
            continue;
        }
        let checked = match event_from_row(row) {
            Ok(event) => chain.push(&event).map_err(|error| error.to_string()),
            Err(error) if is_decode_failure(&error) => Err(decode_reason(&error)),
            Err(error) => return Err(error.into()),
        };
        if let Err(reason) = checked {
            broken = true;
            note(LedgerFailure {
                record_type: LedgerRecordType::Event,
                record_id: event_id.unwrap_or_default(),
                project_sequence,
                reason,
            });
        }
    }
    Ok((count, head))
}

/// Recomputes each record whose identity `ids` lists with `check`, which
/// returns why a record does not hold. P2-K (T11 S-1): every identity a
/// `memproved` Event announces (`announced`) is checked too, in identity
/// order with the stored ones; one no row holds is reported missing with the
/// Project sequence of the first Event that records it, and counted.
fn verify_records(
    connection: &Connection,
    ids: &str,
    (announced, announced_by): (&BTreeMap<String, Recorded>, &str),
    record_type: LedgerRecordType,
    check: impl Fn(&Connection, &str) -> Result<Option<String>, StoreError>,
    note: &mut impl FnMut(LedgerFailure),
) -> Result<u64, StoreError> {
    // P4-H1: an identity that is not UTF-8 is read as one that does not
    // parse, so `check` reports it.
    let stored = connection
        .prepare(ids)?
        .query_map([], |row| lossy_text(row, 0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    let ids = stored
        .iter()
        .chain(announced.keys())
        .collect::<BTreeSet<_>>();
    for id in &ids {
        if !stored.contains(*id) {
            let sequence = announced
                .get(*id)
                .and_then(|recorded| recorded.first())
                .map(|(sequence, _)| *sequence);
            note(LedgerFailure {
                record_type,
                record_id: (*id).clone(),
                project_sequence: sequence,
                reason: format!(
                    "record is missing; the {announced_by} Event at Project sequence {} \
                     records it",
                    sequence.map_or_else(|| "?".to_owned(), |sequence| sequence.to_string())
                ),
            });
        } else if let Some(reason) = check(connection, id)? {
            note(LedgerFailure {
                record_type,
                record_id: (*id).clone(),
                project_sequence: None,
                reason,
            });
        }
    }
    Ok(u64::try_from(ids.len()).unwrap_or(u64::MAX))
}

/// A record read back by its own identity must exist, recompute, and agree
/// with the ledger (`agrees`); a row that does not decode is a finding, any
/// other failure an error. P4-H1 (Owner, T20 SF-2): "does not decode"
/// includes an identity the store reads back as a parameter of another
/// query, such as a Checkpoint statement's `evidence_id`.
fn present<T>(
    found: Result<Option<T>, StoreError>,
    agrees: impl FnOnce(&T) -> Option<String>,
) -> Result<Option<String>, StoreError> {
    match found {
        Ok(Some(record)) => Ok(agrees(&record)),
        Ok(None) => Ok(Some("record is missing".to_owned())),
        Err(
            error @ (StoreError::PersistedDataIntegrity(_) | StoreError::InvalidPersistedValue(_)),
        ) => Ok(Some(error.to_string())),
        Err(StoreError::Database(error)) if is_decode_failure(&error) => {
            Ok(Some(decode_reason(&error)))
        }
        Err(error) => Err(error),
    }
}

fn decode_reason(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::FromSqlConversionFailure(_, _, cause) => cause.to_string(),
        other => other.to_string(),
    }
}
