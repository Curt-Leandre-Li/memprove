//! Same-state carry-over (ADR 0013 D3, P3-D): `captureEvidence` seals into
//! its new Snapshot the command and test Evidence of earlier Snapshots of the
//! same Task and Workspace in the same repository state, so that one
//! Checkpoint, which anchors one Snapshot, can cite several runs of that
//! state. No table, trigger, digest input, or Event payload changes: the new
//! Snapshot lists more Evidence, and its `command.completed` Event records
//! each of them as it records any Evidence of the Snapshot.

use std::collections::BTreeSet;

use memprove_domain::{
    Evidence, EvidenceId, RepositorySnapshot, RepositorySnapshotId, carries_over,
};
use rusqlite::Connection;

use super::{
    ledger::LedgerReferences,
    preview::is_integrity_failure,
    recovery::{evidence_by_id, sealing_snapshot_ids, snapshot_by_id},
};
use crate::StoreError;

/// The command and test Evidence that `snapshot`, about to be recorded,
/// carries over: every such object of an earlier Snapshot `S` for which
/// [`carries_over`]`(S, snapshot)` holds, in the order the ledger recorded
/// the earlier Snapshots (P4-B; before, the order they were captured).
///
/// Only Evidence that can be checked now is carried (Owner decision,
/// P3-D): an object is carried from the first Snapshot that seals it, and
/// only when that Snapshot and every one of its Evidence objects recompute
/// and agree with what `memproved`'s Events recorded for them. Evidence
/// whose first Snapshot is not intact, or is in another state, is not
/// carried, whichever later Snapshot also lists it.
///
/// # Errors
///
/// A database failure. A record that does not recompute is not an error
/// here; it is not carried.
pub(super) fn carried_evidence(
    connection: &Connection,
    snapshot: &RepositorySnapshot,
) -> Result<Vec<Evidence>, StoreError> {
    let mut candidates = connection
        .prepare(
            "SELECT snapshot_id FROM repository_snapshots
             WHERE task_id = ?1 AND workspace_id = ?2 AND capture_manifest_digest = ?3
               AND capture_format_version = 2 AND snapshot_id != ?4",
        )?
        .query_map(
            [
                snapshot.task_id().as_str(),
                snapshot.workspace_id().as_str(),
                snapshot.capture_manifest_digest().as_str(),
                snapshot.snapshot_id().as_str(),
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let references = LedgerReferences::read(connection)?;
    // P4-B (P4-0 item D): in ledger order, not `captured_at` order.
    references.sort_snapshots_by_ledger(&mut candidates);
    let mut decided = BTreeSet::new();
    let mut carried = Vec::new();
    for candidate in candidates {
        let Ok(snapshot_id) = RepositorySnapshotId::parse(&candidate) else {
            continue;
        };
        let runs = run_evidence_ids(connection, &snapshot_id)?;
        let intact = intact_source(connection, &references, &snapshot_id, snapshot)?;
        for evidence_id in runs {
            if !decided.insert(evidence_id.clone()) {
                continue;
            }
            let Some(earlier) = &intact else {
                continue;
            };
            if first_sealing_snapshot(connection, &references, &evidence_id)?.as_ref()
                != Some(&snapshot_id)
            {
                continue;
            }
            if let Some(item) = earlier
                .1
                .iter()
                .find(|item| item.evidence_id() == &evidence_id)
            {
                carried.push(item.clone());
            }
        }
    }
    Ok(carried)
}

/// The command and test Evidence identities a Snapshot row lists, whether
/// or not the Snapshot recomputes.
fn run_evidence_ids(
    connection: &Connection,
    snapshot_id: &RepositorySnapshotId,
) -> Result<Vec<EvidenceId>, StoreError> {
    let ids = connection
        .prepare(
            "SELECT evidence_id FROM repository_snapshot_evidence
             WHERE snapshot_id = ?1 AND role IN ('command', 'test')
             ORDER BY role, ordinal, evidence_id",
        )?
        .query_map([snapshot_id.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids
        .iter()
        .filter_map(|id| EvidenceId::parse(id).ok())
        .collect())
}

/// The earlier Snapshot with its Evidence when it recomputes, agrees with
/// the ledger, and carries over into `later`; `None` otherwise.
fn intact_source(
    connection: &Connection,
    references: &LedgerReferences,
    snapshot_id: &RepositorySnapshotId,
    later: &RepositorySnapshot,
) -> Result<Option<(RepositorySnapshot, Vec<Evidence>)>, StoreError> {
    let earlier = match snapshot_by_id(connection, snapshot_id) {
        Ok(Some(earlier)) => earlier,
        Ok(None) => return Ok(None),
        Err(error) if is_integrity_failure(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !carries_over(&earlier, later) {
        return Ok(None);
    }
    let mut evidence = Vec::new();
    for id in earlier.all_evidence_ids() {
        match evidence_by_id(connection, id) {
            Ok(Some(item)) => evidence.push(item),
            Ok(None) => return Ok(None),
            Err(error) if is_integrity_failure(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    let (snapshot_failure, evidence_failures) = references.sealed_failures(&earlier, &evidence);
    if snapshot_failure.is_some() || !evidence_failures.is_empty() {
        return Ok(None);
    }
    Ok(Some((earlier, evidence)))
}

/// The first Snapshot that seals an Evidence object, in the order Evidence
/// records list them: ledger order (P4-B, P4-0 item D), then `snapshot_id`.
fn first_sealing_snapshot(
    connection: &Connection,
    references: &LedgerReferences,
    evidence_id: &EvidenceId,
) -> Result<Option<RepositorySnapshotId>, StoreError> {
    let first = sealing_snapshot_ids(connection, references, evidence_id)?
        .into_iter()
        .next();
    Ok(first.and_then(|id| RepositorySnapshotId::parse(&id).ok()))
}
