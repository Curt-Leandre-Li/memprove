//! The read-only Recovery Preview document (ADR 0012 D6, FR-208).
//!
//! A Preview is a pure function of one frozen ledger view (Project sequence
//! N) and one probe P: the same (N, P) yields the same JCS bytes and the same
//! `preview_digest`. It carries no generation time.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::{
    Assessment, CanonicalJson, Checkpoint, CheckpointId, DamagedRecord, Evidence, EvidenceId, Gap,
    Judgement, ONTOLOGY_VERSION, Probe, ProbeObservation, RecordedRun, RecoveryDomainError,
    RepositorySnapshot, RunStatus, Session, Sha256Digest, Task, UtcTimestamp, Verdict, assess,
    ontology::{Anchor, seal_tampered},
    run_history,
};

/// Most Sessions a Preview lists (ADR 0012 context query, step 2).
pub const PREVIEW_SESSION_LIMIT: usize = 20;
/// Most Events after the Checkpoint a Preview lists (step 4).
pub const PREVIEW_EVENT_LIMIT: usize = 50;

/// The Task's latest Checkpoint as read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // One view per Preview.
pub enum CheckpointView {
    /// The Task has no Checkpoint.
    None,
    /// Read with every digest recomputed.
    Intact {
        /// The Checkpoint.
        checkpoint: Checkpoint,
        /// Its Snapshot.
        snapshot: RepositorySnapshot,
        /// Every Evidence object of the Snapshot.
        evidence: Vec<Evidence>,
    },
    /// A digest did not recompute or a referenced record is missing.
    Damaged {
        /// The Checkpoint's identity.
        checkpoint_id: CheckpointId,
        /// The stored Checkpoint digest, when it parses.
        stored_digest: Option<Sha256Digest>,
        /// A referenced record is missing.
        missing_object: bool,
        /// Evidence of the Snapshot whose digests do not recompute.
        invalid_evidence_ids: Vec<EvidenceId>,
    },
}

/// Type, time, and producer of one Event; no payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSummary {
    /// Project sequence.
    pub project_sequence: u64,
    /// Event type.
    pub event_type: String,
    /// Producer-declared time, when given.
    pub occurred_at: Option<UtcTimestamp>,
    /// Receipt time.
    pub recorded_at: UtcTimestamp,
    /// Producer name.
    pub producer_name: String,
    /// Producer version.
    pub producer_version: String,
}

/// What the ledger holds for one Task at Project sequence N, read in one
/// read transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLedgerView {
    /// N: the Project's latest Event sequence, 0 without Events.
    pub project_sequence: u64,
    /// Whether the Project's complete Event chain verifies.
    pub ledger_chain_valid: bool,
    /// The Task.
    pub task: Task,
    /// Sessions continuing the Task, latest `started_at` first.
    pub sessions: Vec<Session>,
    /// More Sessions continue the Task than listed.
    pub sessions_truncated: bool,
    /// The latest Checkpoint.
    pub checkpoint: CheckpointView,
    /// Those Sessions' Events after the Checkpoint, up to N, in sequence
    /// order.
    pub events: Vec<EventSummary>,
    /// More Events followed than listed.
    pub events_truncated: bool,
    /// P3-C2 (Owner, T12 S-4/N1): every record of the Task that is missing,
    /// does not recompute, or disagrees with the ledger, by the check the
    /// Task Capsule applies (its Checkpoints, Snapshots, and Evidence, its
    /// Decisions' Evidence, and the Task row against its last Task Event).
    pub damaged_records: BTreeSet<DamagedRecord>,
    /// P3-D3: every readable Snapshot of the Task that agrees with the
    /// ledger, as the Task Capsule reads them, for the Task's
    /// [`crate::RunHistory`]. Not rendered.
    pub runs: Vec<RecordedRun>,
}

/// One rendered Preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPreview {
    document: CanonicalJson,
    digest: Sha256Digest,
    assessment: Assessment,
}

impl RecoveryPreview {
    /// The JCS bytes of the whole document, `preview_digest` included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.document.as_str()
    }

    /// SHA-256 of the JCS bytes of the document without `preview_digest`.
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    /// The judgement behind the document.
    #[must_use]
    pub const fn assessment(&self) -> &Assessment {
        &self.assessment
    }
}

fn wire(digest: &Sha256Digest) -> String {
    format!("sha256:{digest}")
}

fn judged(judgement: &Judgement) -> (Value, Value) {
    (
        json!(judgement.verdict().as_str()),
        json!(
            judgement
                .reasons()
                .iter()
                .map(|reason| reason.as_str())
                .collect::<Vec<_>>()
        ),
    )
}

/// Renders the Preview of `view` under `probe`.
///
/// # Errors
///
/// [`RecoveryDomainError::Canonicalization`] if the document cannot be
/// canonicalized.
pub fn recovery_preview(
    view: &TaskLedgerView,
    probe: &Probe,
) -> Result<RecoveryPreview, RecoveryDomainError> {
    let anchor = match &view.checkpoint {
        CheckpointView::None => Anchor::None,
        CheckpointView::Intact {
            checkpoint,
            snapshot,
            evidence,
        } => Anchor::Intact {
            checkpoint,
            snapshot,
            evidence,
        },
        CheckpointView::Damaged {
            missing_object,
            invalid_evidence_ids,
            ..
        } => Anchor::Damaged {
            missing_object: *missing_object,
            invalid_evidence_ids,
        },
    };
    let mut assessment = assess(
        anchor,
        view.ledger_chain_valid,
        &run_history(&view.runs),
        probe,
    );
    // P3-C2: any damaged record of the Task makes the Preview `tampered`,
    // as it makes the Task Capsule, even when the latest Checkpoint reads
    // intact; then no level of it reads `verified`.
    if !view.damaged_records.is_empty() {
        assessment.gaps.insert(Gap::RecordIntegrity);
        assessment.judgement = assessment.judgement.combine(&Judgement::new(
            Verdict::Tampered,
            view.damaged_records.iter().map(|record| record.reason),
        ));
    }
    seal_tampered(&mut assessment);
    if view.sessions_truncated {
        assessment.gaps.insert(Gap::SessionsTruncated);
    }
    if view.events_truncated {
        assessment.gaps.insert(Gap::EventsTruncated);
    }
    let mut document = document(view, probe, &assessment);
    let digest = canonical(&document)?.sha256();
    document["preview_digest"] = json!(wire(&digest));
    Ok(RecoveryPreview {
        document: canonical(&document)?,
        digest,
        assessment,
    })
}

fn canonical(value: &Value) -> Result<CanonicalJson, RecoveryDomainError> {
    CanonicalJson::from_trusted_value(value).map_err(|_| RecoveryDomainError::Canonicalization)
}

#[allow(clippy::too_many_lines)] // The whole document in one reviewable literal.
fn document(view: &TaskLedgerView, probe: &Probe, assessment: &Assessment) -> Value {
    let (status, reasons) = judged(&assessment.judgement);
    let (checkpoint, snapshot) = match &view.checkpoint {
        CheckpointView::None => (Value::Null, Value::Null),
        CheckpointView::Intact {
            checkpoint,
            snapshot,
            ..
        } => (
            json!({
                "checkpoint_id": checkpoint.checkpoint_id(),
                "digest": wire(checkpoint.digest()),
                "project_event_sequence": checkpoint.project_event_sequence(),
                "session_id": checkpoint.session_id(),
                "objective": checkpoint.objective(),
                "created_at": checkpoint.created_at(),
                "intact": true,
            }),
            json!({
                "snapshot_id": snapshot.snapshot_id(),
                "workspace_id": snapshot.workspace_id(),
                "canonical_digest": wire(snapshot.canonical_digest()),
                "capture_manifest_digest": wire(snapshot.capture_manifest_digest()),
                "head_oid": snapshot.head_oid(),
                "branch": snapshot.branch(),
                "is_dirty": snapshot.is_dirty(),
                "captured_at": snapshot.captured_at(),
            }),
        ),
        CheckpointView::Damaged {
            checkpoint_id,
            stored_digest,
            ..
        } => (
            json!({
                "checkpoint_id": checkpoint_id,
                "digest": stored_digest.as_ref().map(wire),
                "intact": false,
            }),
            Value::Null,
        ),
    };
    let probe_binding = match &probe.observation {
        ProbeObservation::Unavailable => json!({
            "workspace_id": probe.workspace_id,
            "available": false,
        }),
        ProbeObservation::Observed(observation) => json!({
            "workspace_id": probe.workspace_id,
            "available": true,
            "head_oid": observation.head_oid,
            "branch": observation.branch,
            "is_dirty": !observation.status_porcelain_v2.is_empty(),
            "capture_manifest_digest": wire(&observation.capture_manifest_digest),
            "same_repository": observation.same_repository,
            "same_workspace": observation.same_workspace,
        }),
    };
    let facts = assessment
        .facts
        .iter()
        .map(|fact| {
            let (verdict, reasons) = judged(&fact.judgement);
            json!({
                "claim": {
                    "checkpoint_id": fact.claim.checkpoint_id,
                    "section": fact.claim.section.as_str(),
                    "ordinal": fact.claim.ordinal,
                },
                "classification": fact.classification.as_str(),
                "text": fact.text,
                "propagates": fact.propagates,
                "verdict": verdict,
                "reasons": reasons,
                "evidence": fact.evidence.iter().map(|item| {
                    let (verdict, reasons) = judged(&item.judgement);
                    json!({
                        "evidence_id": item.evidence_id,
                        "kind": item.kind.as_str(),
                        "canonical_digest": wire(&item.canonical_digest),
                        "run_status": item.run_status.map(RunStatus::as_str),
                        "argv": item.argv,
                        "verdict": verdict,
                        "reasons": reasons,
                    })
                }).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_version": 1,
        "ontology_version": ONTOLOGY_VERSION,
        "task_id": view.task.task_id,
        "status": status,
        "reasons": reasons,
        "bindings": {
            "task": {
                "task_id": view.task.task_id,
                "revision": view.task.revision,
                "status": view.task.status.as_str(),
                "workspace_id": view.task.workspace_id,
            },
            "ledger": {
                "project_sequence": view.project_sequence,
                "chain_valid": view.ledger_chain_valid,
            },
            "checkpoint": checkpoint,
            "snapshot": snapshot,
            "probe": probe_binding,
        },
        "facts": facts,
        "invalid_evidence_ids": assessment.invalid_evidence_ids,
        "changed_paths": assessment.changed_paths.paths,
        "changed_paths_complete": assessment.changed_paths.complete,
        "gaps": assessment.gaps.iter().map(|gap| gap.as_str()).collect::<Vec<_>>(),
        "next_checks": assessment.next_checks.iter().map(|check| json!({
            "action": "rerun",
            "evidence_id": check.evidence_id,
            "argv": check.argv,
            "workspace_id": check.workspace_id,
            "at_head": check.at_head,
        })).collect::<Vec<_>>(),
        "sessions": view.sessions.iter().map(|session| json!({
            "session_id": session.session_id,
            "workspace_id": session.workspace_id,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
        })).collect::<Vec<_>>(),
        "events_since_checkpoint": view.events.iter().map(|event| json!({
            "project_sequence": event.project_sequence,
            "event_type": event.event_type,
            "occurred_at": event.occurred_at,
            "recorded_at": event.recorded_at,
            "producer": { "name": event.producer_name, "version": event.producer_version },
        })).collect::<Vec<_>>(),
    })
}
