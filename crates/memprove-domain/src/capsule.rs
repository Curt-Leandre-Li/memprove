//! The Task's claim graph and the deterministic Task Capsule (ADR 0013).
//!
//! A claim is one `CheckpointStatement` of any Checkpoint the ledger records
//! for the Task, or one Decision. The Task's claims accumulate over all its
//! Checkpoints in ledger order; nothing is matched by text. A claim stops
//! being current only through an explicit edge (`supersedes`, `withdraws`)
//! recorded by `memproved`, except `next_action`: each Checkpoint's
//! `next_action` supersedes the one before it (D1).
//!
//! Every function here is pure. A Capsule is a function of one ledger view
//! (Project sequence N) and one probe P: the same (N, P) yields the same JCS
//! bytes and the same `capsule_digest`, which is computed as
//! `preview_digest` is. Verdicts come only from [`judge_evidence`] through
//! [`judge_statements`] and [`judge_statement`]; standing and edges never
//! change a claim's verdict, they only decide which claims the Task verdict
//! takes (D4).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::{
    CanonicalJson, Checkpoint, CheckpointClassification, CheckpointId, ClaimRef, EventId, Evidence,
    EvidenceAssessment, EvidenceId, EvidenceKind, Gap, Judgement, LedgerRecordType,
    ONTOLOGY_VERSION, Probe, ProbeObservation, ReasonCode, RecoveryDomainError, RepositorySnapshot,
    RunHistory, RunStatus, Session, SessionId, Sha256Digest, StatementSection, Task, TaskId,
    Verdict, WorkspaceId, judge_evidence, judge_source, judge_statement, judge_statements,
    ontology::{argv_of, integrity_judgement},
    porcelain_v2_paths,
    recovery::passed_tests_hold,
    snapshot_repository_identity, unselected_untracked,
};

/// Most distinct base commits one probe compares with HEAD (D4).
pub const CAPSULE_BASE_LIMIT: usize = 16;

/// What kind of claim a statement or Decision is (D1, D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClaimKind {
    /// A `verified_fact`, in any section: the only kind the Task verdict
    /// takes.
    Fact,
    /// Any other statement of `open_items` (or of `verified_completed_work`,
    /// which only a record that bypassed the domain could hold).
    OpenItem,
    /// The `next_action` of a Checkpoint.
    NextAction,
    /// A Decision (`decision.recorded`).
    Decision,
}

impl ClaimKind {
    /// Every kind, in declaration order (U3 G1).
    pub const ALL: [Self; 4] = [Self::Fact, Self::OpenItem, Self::NextAction, Self::Decision];

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::OpenItem => "open_item",
            Self::NextAction => "next_action",
            Self::Decision => "decision",
        }
    }

    /// The kind of a statement of `section` with `classification`.
    #[must_use]
    pub const fn of(section: StatementSection, classification: CheckpointClassification) -> Self {
        match (section, classification) {
            (_, CheckpointClassification::VerifiedFact) => Self::Fact,
            (StatementSection::NextAction, _) => Self::NextAction,
            _ => Self::OpenItem,
        }
    }

    /// P3-C2 (Owner): a fact is superseded only by a fact; anything may be
    /// withdrawn. Whether a claim of kind `by` may supersede one of `self`.
    #[must_use]
    pub fn may_be_superseded_by(self, by: Self) -> bool {
        self != Self::Fact || by == Self::Fact
    }
}

/// Whether a claim is still current (D1). Standing never changes a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Standing {
    /// Neither superseded nor withdrawn.
    Active,
    /// A later claim declared itself its new version, or a later
    /// `next_action` followed it.
    Superseded,
    /// A later record withdrew it.
    Withdrawn,
}

impl Standing {
    /// Every standing, in declaration order (U3 G1).
    pub const ALL: [Self; 3] = [Self::Active, Self::Superseded, Self::Withdrawn];

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Withdrawn => "withdrawn",
        }
    }
}

/// The identity of a claim: a statement's `(checkpoint_id, section,
/// ordinal)`, or a Decision's `decision.recorded` Event.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClaimId {
    /// One `CheckpointStatement`.
    Statement(ClaimRef),
    /// One Decision.
    Decision(EventId),
}

impl ClaimId {
    /// The wire spelling: `<checkpoint_id>/<section>/<ordinal>` or
    /// `decision/<event_id>`.
    #[must_use]
    pub fn wire(&self) -> String {
        match self {
            Self::Statement(claim) => format!(
                "{}/{}/{}",
                claim.checkpoint_id,
                claim.section.as_str(),
                claim.ordinal
            ),
            Self::Decision(event_id) => format!("decision/{event_id}"),
        }
    }

    /// Parses [`ClaimId::wire`]; `None` for anything else.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(event_id) = value.strip_prefix("decision/") {
            return EventId::parse(event_id).ok().map(Self::Decision);
        }
        let mut parts = value.split('/');
        let (Some(checkpoint_id), Some(section), Some(ordinal), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let section = match section {
            "verified_completed_work" => StatementSection::VerifiedCompletedWork,
            "open_items" => StatementSection::OpenItems,
            "next_action" => StatementSection::NextAction,
            _ => return None,
        };
        // A canonical decimal ordinal only: no sign, no leading zero.
        if ordinal.is_empty()
            || !ordinal.bytes().all(|byte| byte.is_ascii_digit())
            || (ordinal.len() > 1 && ordinal.starts_with('0'))
        {
            return None;
        }
        Some(Self::Statement(ClaimRef {
            checkpoint_id: CheckpointId::parse(checkpoint_id).ok()?,
            section,
            ordinal: ordinal.parse().ok()?,
        }))
    }
}

/// The two relations between claims (D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeRelation {
    /// `by` is the new version of `target` (including "a fact completes an
    /// open item").
    Supersedes,
    /// `target` no longer holds; nothing replaces it.
    Withdraws,
}

impl EdgeRelation {
    /// Both relations, in declaration order (U3 G1).
    pub const ALL: [Self; 2] = [Self::Supersedes, Self::Withdraws];

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supersedes => "supersedes",
            Self::Withdraws => "withdraws",
        }
    }
}

/// One `claim.superseded` Event of `memproved` (payload
/// `claim.superseded@1`), decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimEdge {
    /// Project sequence of the Event.
    pub project_sequence: u64,
    /// The relation.
    pub relation: EdgeRelation,
    /// The earlier claim the edge points at.
    pub target: ClaimId,
    /// The newer claim, for `supersedes`; `None` for `withdraws`.
    pub by: Option<ClaimId>,
    /// Why the target no longer holds, for `withdraws` (P3-D2); `None` on
    /// an edge recorded without one, as every edge before P3-D2 is.
    pub reason: Option<String>,
}

impl ClaimEdge {
    /// Decodes a `claim.superseded@1` payload for `task_id`:
    /// `{"task_id", "relation": "supersedes" | "withdraws", "target":
    /// <claim id>, "by": <claim id> | null}` and, for `withdraws` only, an
    /// optional non-empty string `reason` (P3-D2), `by` required exactly for
    /// `supersedes`. `None` when the payload is for another Task or does not
    /// decode.
    #[must_use]
    pub fn from_payload(project_sequence: u64, task_id: &TaskId, payload: &Value) -> Option<Self> {
        if payload.get("task_id").and_then(Value::as_str) != Some(task_id.as_str()) {
            return None;
        }
        let relation = match payload.get("relation").and_then(Value::as_str)? {
            "supersedes" => EdgeRelation::Supersedes,
            "withdraws" => EdgeRelation::Withdraws,
            _ => return None,
        };
        let target = ClaimId::parse(payload.get("target").and_then(Value::as_str)?)?;
        let by = match payload.get("by") {
            None | Some(Value::Null) => None,
            Some(by) => Some(ClaimId::parse(by.as_str()?)?),
        };
        if (relation == EdgeRelation::Supersedes) != by.is_some() {
            return None;
        }
        let reason = match payload.get("reason") {
            None => None,
            Some(reason) => {
                let reason = reason.as_str().filter(|reason| !reason.is_empty())?;
                if relation != EdgeRelation::Withdraws {
                    return None;
                }
                Some(reason.to_owned())
            }
        };
        Some(Self {
            project_sequence,
            relation,
            target,
            by,
            reason,
        })
    }

    /// The `claim.superseded@1` payload `memproved` writes for one edge
    /// (P3-C); [`ClaimEdge::from_payload`] decodes it. `by` is `null` for
    /// `withdraws`. P3-D2: `reason` is present only on a `withdraws` edge
    /// given one, so every earlier payload keeps its shape.
    #[must_use]
    pub fn payload(task_id: &TaskId, edge: &ProposedEdge) -> Value {
        let mut payload = json!({
            "task_id": task_id,
            "relation": edge.relation.as_str(),
            "target": edge.target.wire(),
            "by": edge.by.as_ref().map(ClaimId::wire),
        });
        if let (EdgeRelation::Withdraws, Some(reason)) = (edge.relation, &edge.reason) {
            payload["reason"] = json!(reason);
        }
        payload
    }
}

/// One edge a write declares before it is recorded (P3-C): a statement's
/// `supersedes`, a Decision's `supersedes`, or a Checkpoint's
/// `withdrawals`. `at` is its location in the request, for the `400`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedEdge {
    /// The relation.
    pub relation: EdgeRelation,
    /// The earlier claim it names.
    pub target: ClaimId,
    /// The new claim, for `supersedes`; `None` for `withdraws`.
    pub by: Option<ClaimId>,
    /// Why the target no longer holds, for `withdraws`: the request's
    /// `withdrawal_reason` (P3-D2).
    pub reason: Option<String>,
    /// Where the request names the target, for example
    /// `$.open_items[0].supersedes[1]` or `$.withdrawals[0]`.
    pub at: String,
}

/// Why a declared edge cannot be recorded (P3-C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeRule {
    /// The target is the claim that declares it.
    SelfReference,
    /// The target is another claim of the same write, so it is not earlier
    /// in the ledger.
    NotEarlier,
    /// The target is not a readable claim of the Task; the store says
    /// whether it belongs to another Task, cannot be read, or is unknown.
    NotAClaim,
    /// The request names the target more than once.
    Duplicate,
    /// P3-C2 (Owner): the target is a fact and the claim that would
    /// supersede it is not; a fact is superseded only by a fact.
    FactByNonFact,
    /// P3-D2 (Owner): the target of a `withdraws` is a fact and the request
    /// gives no reason; withdrawing a fact needs one.
    FactWithdrawnWithoutReason,
}

/// A declared edge that cannot be recorded, with where the request names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRefusal {
    /// The edge's location in the request.
    pub at: String,
    /// The rule it breaks.
    pub rule: EdgeRule,
    /// Its target.
    pub target: ClaimId,
}

impl EdgeRefusal {
    /// The `detail` of the `400`: the location and the rule, never the
    /// request's value. `NotAClaim` is refined by the store.
    #[must_use]
    pub fn detail(&self) -> String {
        let rule = match self.rule {
            EdgeRule::SelfReference => "names the claim that declares it",
            EdgeRule::NotEarlier => {
                "names a claim recorded by this same request, which is not earlier in the ledger"
            }
            EdgeRule::NotAClaim => "names no readable claim of this Task",
            EdgeRule::Duplicate => {
                "names a claim this request already names; each claim is superseded or \
                 withdrawn at most once per request"
            }
            EdgeRule::FactByNonFact => {
                "names a fact, which only a verified_fact statement can supersede; record the \
                 new fact, or withdraw this one"
            }
            EdgeRule::FactWithdrawnWithoutReason => {
                "names a fact; withdrawing a fact needs $.withdrawal_reason, which says why it \
                 no longer holds"
            }
        };
        format!("{} {rule}", self.at)
    }
}

/// Every readable claim of the view and its ledger position: each statement
/// of a readable Checkpoint (`checkpoint.created`, or its
/// `project_event_sequence` plus one) and each Decision (`decision.recorded`).
#[must_use]
pub fn readable_claims(view: &TaskCapsuleView) -> BTreeMap<ClaimId, u64> {
    let mut claims = BTreeMap::new();
    for record in &view.checkpoints {
        let checkpoint = &record.checkpoint;
        let mut add = |section, ordinal| {
            claims.insert(
                ClaimId::Statement(ClaimRef {
                    checkpoint_id: checkpoint.checkpoint_id().clone(),
                    section,
                    ordinal,
                }),
                record.project_sequence,
            );
        };
        for ordinal in 0..checkpoint.verified_completed_work().len() {
            add(StatementSection::VerifiedCompletedWork, ordinal);
        }
        for ordinal in 0..checkpoint.open_items().len() {
            add(StatementSection::OpenItems, ordinal);
        }
        add(StatementSection::NextAction, 0);
    }
    for decision in &view.decisions {
        claims.insert(
            ClaimId::Decision(decision.decision_id.clone()),
            decision.project_sequence,
        );
    }
    claims
}

/// The kind of every readable claim of the view (P3-C2).
pub(crate) fn readable_claim_kinds(view: &TaskCapsuleView) -> BTreeMap<ClaimId, ClaimKind> {
    let mut kinds = BTreeMap::new();
    for record in &view.checkpoints {
        let checkpoint = &record.checkpoint;
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
        for (section, statements) in sections {
            for (ordinal, statement) in statements.iter().enumerate() {
                kinds.insert(
                    ClaimId::Statement(ClaimRef {
                        checkpoint_id: checkpoint.checkpoint_id().clone(),
                        section,
                        ordinal,
                    }),
                    ClaimKind::of(section, statement.classification()),
                );
            }
        }
    }
    for decision in &view.decisions {
        kinds.insert(
            ClaimId::Decision(decision.decision_id.clone()),
            ClaimKind::Decision,
        );
    }
    kinds
}

/// Checks the edges one write declares, before anything is written (P3-C):
/// every target must be a readable claim of the Task (`view`) earlier in
/// the ledger than the write, which is recorded after every claim of the
/// view; not a claim the write itself records (`new_claims`, with their
/// kinds), and named at most once. P3-C2 (Owner): a fact is superseded only
/// by a fact. P3-D2 (Owner): a fact is withdrawn only with a reason. The
/// first refusal in request order is returned. Edges that
/// pass are exactly the ones the read path applies (D1).
///
/// # Errors
///
/// The first [`EdgeRefusal`].
pub fn check_edges(
    view: &TaskCapsuleView,
    new_claims: &[(ClaimId, ClaimKind)],
    edges: &[ProposedEdge],
) -> Result<(), EdgeRefusal> {
    let readable = readable_claims(view);
    let kinds = readable_claim_kinds(view);
    let mut named = BTreeSet::new();
    for edge in edges {
        let refuse = |rule| EdgeRefusal {
            at: edge.at.clone(),
            rule,
            target: edge.target.clone(),
        };
        if edge.by.as_ref() == Some(&edge.target) {
            return Err(refuse(EdgeRule::SelfReference));
        }
        if new_claims.iter().any(|(claim, _)| *claim == edge.target) {
            return Err(refuse(EdgeRule::NotEarlier));
        }
        match readable.get(&edge.target) {
            None => return Err(refuse(EdgeRule::NotAClaim)),
            // Every claim of the view precedes the write; a position past N
            // would come from a record the ledger does not order before it.
            Some(&position) if position > view.project_sequence => {
                return Err(refuse(EdgeRule::NotEarlier));
            }
            Some(_) => {}
        }
        if let (EdgeRelation::Supersedes, Some(by), Some(target)) =
            (edge.relation, &edge.by, kinds.get(&edge.target))
        {
            let by = new_claims
                .iter()
                .find(|(claim, _)| claim == by)
                .map(|(_, kind)| *kind)
                .or_else(|| kinds.get(by).copied());
            if by.is_none_or(|by| !target.may_be_superseded_by(by)) {
                return Err(refuse(EdgeRule::FactByNonFact));
            }
        }
        if edge.relation == EdgeRelation::Withdraws
            && edge.reason.is_none()
            && kinds.get(&edge.target) == Some(&ClaimKind::Fact)
        {
            return Err(refuse(EdgeRule::FactWithdrawnWithoutReason));
        }
        if !named.insert(edge.target.clone()) {
            return Err(refuse(EdgeRule::Duplicate));
        }
    }
    Ok(())
}

/// What a `decision.recorded@1` payload of `memproved` records:
/// `{"task_id", "text", "rationale": string | null, "evidence_ids": [...]}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionPayload {
    /// The decision.
    pub text: String,
    /// Why, when given.
    pub rationale: Option<String>,
    /// The Evidence it rests on, in recorded order.
    pub evidence_ids: Vec<EvidenceId>,
}

impl DecisionPayload {
    /// Decodes a `decision.recorded@1` payload for `task_id`; `None` when it
    /// is for another Task or does not decode.
    #[must_use]
    pub fn from_payload(task_id: &TaskId, payload: &Value) -> Option<Self> {
        if payload.get("task_id").and_then(Value::as_str) != Some(task_id.as_str()) {
            return None;
        }
        let text = payload.get("text").and_then(Value::as_str)?;
        if text.is_empty() {
            return None;
        }
        let rationale = match payload.get("rationale") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_str()?.to_owned()),
        };
        let evidence_ids = match payload.get("evidence_ids") {
            None => Vec::new(),
            Some(ids) => ids
                .as_array()?
                .iter()
                .map(|id| id.as_str().and_then(|id| EvidenceId::parse(id).ok()))
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self {
            text: text.to_owned(),
            rationale,
            evidence_ids,
        })
    }

    /// The `decision.recorded@1` payload `memproved` writes (P3-C);
    /// [`DecisionPayload::from_payload`] decodes it.
    #[must_use]
    pub fn payload(&self, task_id: &TaskId) -> Value {
        json!({
            "task_id": task_id,
            "text": self.text,
            "rationale": self.rationale,
            "evidence_ids": self.evidence_ids,
        })
    }
}

/// One Evidence object as read for a Decision: with the first Snapshot that
/// seals it and that Snapshot's Evidence, or `None` when it cannot be read
/// (the store then also lists a damaged record). P3-C2 (T12 N4): Evidence
/// of another Task is not read (`other_task`): it is unavailable, not
/// damaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedEvidence {
    /// The cited identity.
    pub evidence_id: EvidenceId,
    /// The Evidence, its Snapshot, and that Snapshot's Evidence.
    pub sealed: Option<(Evidence, RepositorySnapshot, Vec<Evidence>)>,
    /// The cited Evidence belongs to another Task.
    pub other_task: bool,
}

/// One Decision of the Task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// Identity: the `decision.recorded` Event.
    pub decision_id: EventId,
    /// Project sequence of that Event.
    pub project_sequence: u64,
    /// The Session that recorded it.
    pub session_id: SessionId,
    /// The payload.
    pub payload: DecisionPayload,
    /// Its Evidence, in recorded order.
    pub evidence: Vec<SealedEvidence>,
    /// Some of its Evidence, or a Snapshot sealing it, is missing or
    /// disagrees with the ledger.
    pub damaged: bool,
}

/// One readable Checkpoint of the Task: its record, Snapshot, and Snapshot
/// Evidence all recompute. `damaged` when one of them disagrees with what
/// `memproved`'s Events recorded (ADR 0012, "What verifyLedger
/// guarantees"); its statements are then listed with `damaged: true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRecord {
    /// Project sequence of its `checkpoint.created` Event (for a record
    /// without one, its `project_event_sequence` plus one).
    pub project_sequence: u64,
    /// The Checkpoint.
    pub checkpoint: Checkpoint,
    /// Its Snapshot.
    pub snapshot: RepositorySnapshot,
    /// The Snapshot's Evidence.
    pub evidence: Vec<Evidence>,
    /// Disagrees with the ledger.
    pub damaged: bool,
}

/// One readable Snapshot of the Task with its Evidence: a run, whether or
/// not a Checkpoint anchors it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRun {
    /// Project sequence of the first `command.completed` or
    /// `checkpoint.created` Event that records the Snapshot.
    pub project_sequence: u64,
    /// The Snapshot.
    pub snapshot: RepositorySnapshot,
    /// Its Evidence.
    pub evidence: Vec<Evidence>,
}

/// P3-D3: the [`RunHistory`] of a Task's runs, the one the Task Capsule,
/// the Recovery Preview, Task completion, and the Checkpoint write check
/// all judge passed runs by.
#[must_use]
pub fn run_history(runs: &[RecordedRun]) -> RunHistory {
    RunHistory::of(
        runs.iter()
            .map(|run| (run.project_sequence, &run.snapshot, run.evidence.as_slice())),
    )
}

/// How a Session ended, by the ledger (D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionEnd {
    /// No `session.ended` and no `session.crashed` of `memproved`.
    Open,
    /// A `session.ended` of `memproved`.
    Ended,
    /// A `session.crashed` of `memproved`.
    Crashed,
}

impl SessionEnd {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Ended => "ended",
            Self::Crashed => "crashed",
        }
    }
}

/// One Session continuing the Task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// The Session row.
    pub session: Session,
    /// How it ended, by the ledger.
    pub end: SessionEnd,
    /// P3-L L7: when the ledger recorded its latest Event (`recorded_at`),
    /// `None` when the ledger holds no Event of it.
    pub last_recorded_at: Option<crate::UtcTimestamp>,
    /// ADR 0014 E6 (P4-F): the Pack the Session declared it continues from,
    /// read from its `capsule.recovered@1` Event of `memproved`; `None`
    /// without one.
    pub resumed_from: Option<crate::ResumedFrom>,
}

/// One record that does not recompute, is missing, or disagrees with the
/// ledger (D7).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DamagedRecord {
    /// The kind of record.
    pub record_type: LedgerRecordType,
    /// Its identity as stored or recorded.
    pub record_id: String,
    /// `object_missing` or `object_digest_mismatch`.
    pub reason: ReasonCode,
}

/// The whole ledger of one Task at Project sequence N, read in one read
/// transaction (D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCapsuleView {
    /// N.
    pub project_sequence: u64,
    /// Whether the Project's complete Event chain verifies.
    pub ledger_chain_valid: bool,
    /// The Task.
    pub task: Task,
    /// Every Session continuing the Task, `started_at` then `session_id`
    /// ascending.
    pub sessions: Vec<SessionRecord>,
    /// Every readable Checkpoint of the Task, in ledger order.
    pub checkpoints: Vec<CheckpointRecord>,
    /// Every Decision of the Task, in ledger order.
    pub decisions: Vec<DecisionRecord>,
    /// Every edge of the Task, in ledger order.
    pub edges: Vec<ClaimEdge>,
    /// Every readable Snapshot of the Task, in ledger order.
    pub runs: Vec<RecordedRun>,
    /// Every damaged record, sorted.
    pub damaged_records: BTreeSet<DamagedRecord>,
}

/// The probe P of a Capsule: one observation of the asker's Workspace and,
/// for each probed base commit, the paths that may differ between it and
/// the worktree (`None` when that commit is not in the repository).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleProbe {
    /// The probe.
    pub probe: Probe,
    /// Changed paths per probed base commit.
    pub changed_since: BTreeMap<String, Option<BTreeSet<String>>>,
}

/// The base commits of the Task's claims, most recent ledger occurrence
/// first: the first [`CAPSULE_BASE_LIMIT`] are probed, the rest are listed
/// unprobed (D4, Owner revision b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleBases {
    /// Bases the probe compares with HEAD.
    pub probed: Vec<String>,
    /// Bases over the limit: listed, not compared.
    pub omitted: Vec<String>,
}

/// Every Snapshot a claim's Evidence is bound to, with the ledger position
/// that binds it, in ledger order.
fn claim_snapshots(view: &TaskCapsuleView) -> Vec<(u64, &RepositorySnapshot, &[Evidence])> {
    let mut snapshots = view
        .checkpoints
        .iter()
        .map(|record| {
            (
                record.project_sequence,
                &record.snapshot,
                record.evidence.as_slice(),
            )
        })
        .chain(view.decisions.iter().flat_map(|decision| {
            decision.evidence.iter().filter_map(|item| {
                item.sealed.as_ref().map(|(_, snapshot, evidence)| {
                    (decision.project_sequence, snapshot, evidence.as_slice())
                })
            })
        }))
        .collect::<Vec<_>>();
    snapshots.sort_by_key(|(sequence, _, _)| *sequence);
    snapshots
}

/// The base commits to compare with HEAD (D4).
#[must_use]
pub fn capsule_bases(view: &TaskCapsuleView) -> CapsuleBases {
    let mut ordered: Vec<String> = Vec::new();
    for (_, snapshot, _) in claim_snapshots(view).into_iter().rev() {
        if !ordered.iter().any(|base| base == snapshot.head_oid()) {
            ordered.push(snapshot.head_oid().to_owned());
        }
    }
    let omitted = ordered.split_off(ordered.len().min(CAPSULE_BASE_LIMIT));
    CapsuleBases {
        probed: ordered,
        omitted,
    }
}

/// Every selected `repo://` file of every Snapshot a claim is bound to,
/// sorted: what the probe must read.
#[must_use]
pub fn capsule_selected_paths(view: &TaskCapsuleView) -> Vec<String> {
    claim_snapshots(view)
        .into_iter()
        .flat_map(|(_, snapshot, evidence)| {
            evidence
                .iter()
                .filter(|item| snapshot.file_evidence_ids().contains(item.evidence_id()))
                .filter_map(|item| match item.subject() {
                    crate::EvidenceSubject::FileDigest { path } => Some(path.clone()),
                    _ => None,
                })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The Workspace probed when the asker names none: the latest readable
/// Checkpoint's Snapshot Workspace, else the Task's scope (the Recovery
/// Preview's rule).
#[must_use]
pub fn capsule_default_workspace(view: &TaskCapsuleView) -> Option<WorkspaceId> {
    view.checkpoints
        .last()
        .map(|record| record.snapshot.workspace_id().clone())
        .or_else(|| view.task.workspace_id.clone())
}

/// One Evidence object of a claim, as judged, with the Snapshot it is bound
/// to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimEvidence {
    /// The judgement, as for a Recovery Preview fact.
    pub assessment: EvidenceAssessment,
    /// Whether the object could be read; `canonical_digest` is meaningless
    /// when not.
    pub present: bool,
    /// The Snapshot that binds it.
    pub snapshot_id: Option<crate::RepositorySnapshotId>,
    /// That Snapshot's HEAD.
    pub base_commit: Option<String>,
    /// Ledger position of that Snapshot.
    pub snapshot_sequence: u64,
}

/// One claim of the Task, judged (D1, D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Task-local number, 1-based, in ledger order (Owner revision a).
    pub number: u64,
    /// Identity.
    pub id: ClaimId,
    /// Kind.
    pub kind: ClaimKind,
    /// Classification of a statement; `None` for a Decision.
    pub classification: Option<CheckpointClassification>,
    /// Text.
    pub text: String,
    /// A Decision's rationale.
    pub rationale: Option<String>,
    /// The Session that recorded it.
    pub session_id: SessionId,
    /// The Checkpoint that holds it; `None` for a Decision.
    pub checkpoint_id: Option<CheckpointId>,
    /// Ledger position: its `checkpoint.created` or `decision.recorded`.
    pub project_sequence: u64,
    /// Whether it is current.
    pub standing: Standing,
    /// The claims that superseded it, by number.
    pub superseded_by: Vec<u64>,
    /// Why it was withdrawn: the reason of the first `withdraws` edge that
    /// applies to it (P3-D2); `None` when it is not withdrawn or that edge
    /// carries none.
    pub withdrawal_reason: Option<String>,
    /// U8 A6 (Owner O4): the Checkpoint whose `withdraws` edge (that first
    /// edge) withdrew it, and that Checkpoint's Session; `None` when it is
    /// not withdrawn or the Checkpoint that recorded the edge cannot be read
    /// (never an earlier one; U8 S-F round 1, B2).
    pub withdrawn_by: Option<(CheckpointId, SessionId)>,
    /// Whether it rests on a record that disagrees with the ledger.
    pub damaged: bool,
    /// Its Evidence, in reference order.
    pub evidence: Vec<ClaimEvidence>,
    /// Its verdict.
    pub judgement: Judgement,
}

impl Claim {
    /// Only an active `verified_fact` reaches the Task verdict.
    #[must_use]
    pub fn propagates(&self) -> bool {
        self.kind == ClaimKind::Fact
    }
}

/// One rerun the Capsule recommends: a stored argv whose passed run behind
/// an active fact is no longer `verified` (D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rerun {
    /// The argv, compared element by element.
    pub argv: Vec<String>,
    /// The stale or unconfirmed Evidence with this argv, in first-seen
    /// order.
    pub evidence_ids: Vec<EvidenceId>,
    /// The claims that rest on it, by number.
    pub claims: Vec<u64>,
    /// The worst verdict among that Evidence.
    pub judgement: Judgement,
    /// A later run of the Task with the same argv that is `verified` now:
    /// cite it instead of rerunning (`reaffirm`).
    pub verified_run: Option<(EvidenceId, crate::RepositorySnapshotId)>,
}

/// Changed paths since one base commit (D4, D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseChanges {
    /// The base commit.
    pub base_commit: String,
    /// Whether the probe compared it (Owner revision b).
    pub probed: bool,
    /// Whether the diff from it was available.
    pub complete: bool,
    /// `repo://` paths.
    pub paths: BTreeSet<String>,
}

/// One rendered Task Capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCapsule {
    document: CanonicalJson,
    digest: Sha256Digest,
    judgement: Judgement,
    claims: Vec<Claim>,
    reruns: Vec<Rerun>,
}

impl TaskCapsule {
    /// The JCS bytes of the whole document, `capsule_digest` included.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.document.as_str()
    }

    /// SHA-256 of the JCS bytes of the document without `capsule_digest`.
    #[must_use]
    pub const fn digest(&self) -> &Sha256Digest {
        &self.digest
    }

    /// The Task's verdict.
    #[must_use]
    pub const fn judgement(&self) -> &Judgement {
        &self.judgement
    }

    /// Every claim, by number.
    #[must_use]
    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    /// The deduplicated reruns and reaffirmations, in first-seen order.
    #[must_use]
    pub fn reruns(&self) -> &[Rerun] {
        &self.reruns
    }
}

/// Resolves a Task-local number to a claim identity (Owner revision a).
#[must_use]
pub fn claim_by_number(claims: &[Claim], number: u64) -> Option<&ClaimId> {
    claims
        .iter()
        .find(|claim| claim.number == number)
        .map(|claim| &claim.id)
}

fn section_rank(section: StatementSection) -> u8 {
    match section {
        StatementSection::VerifiedCompletedWork => 0,
        StatementSection::OpenItems => 1,
        StatementSection::NextAction => 2,
    }
}

/// Every claim of the view in ledger order, judged, numbered, with standing
/// and the damage of the ledger applied (D1, D2, D7).
fn claims(view: &TaskCapsuleView, history: &RunHistory, probe: &Probe) -> Vec<Claim> {
    let mut claims = Vec::new();
    for record in &view.checkpoints {
        let statements = judge_statements(
            &record.checkpoint,
            &record.snapshot,
            &record.evidence,
            history,
            probe,
        );
        for statement in statements {
            claims.push(Claim {
                number: 0,
                kind: ClaimKind::of(statement.claim.section, statement.classification),
                id: ClaimId::Statement(statement.claim),
                classification: Some(statement.classification),
                text: statement.text,
                rationale: None,
                session_id: record.checkpoint.session_id().clone(),
                checkpoint_id: Some(record.checkpoint.checkpoint_id().clone()),
                project_sequence: record.project_sequence,
                standing: Standing::Active,
                superseded_by: Vec::new(),
                withdrawal_reason: None,
                withdrawn_by: None,
                damaged: record.damaged,
                evidence: statement
                    .evidence
                    .into_iter()
                    .map(|assessment| ClaimEvidence {
                        assessment,
                        present: true,
                        snapshot_id: Some(record.snapshot.snapshot_id().clone()),
                        base_commit: Some(record.snapshot.head_oid().to_owned()),
                        snapshot_sequence: record.project_sequence,
                    })
                    .collect(),
                judgement: statement.judgement,
            });
        }
    }
    for decision in &view.decisions {
        let evidence = decision
            .evidence
            .iter()
            .map(|item| decision_evidence(item, decision.project_sequence, history, probe))
            .collect::<Vec<_>>();
        let assessments = evidence
            .iter()
            .map(|item| item.assessment.clone())
            .collect::<Vec<_>>();
        // P3-D2 (Owner, T13 N9): a Decision is judged by the rule of a
        // `verified_fact`: only fresh, passed tests make it `verified`.
        let mut judgement = judge_statement(&assessments);
        if !decision_evidence_holds(&decision.evidence)
            && !judgement.reasons().contains(&ReasonCode::EvidenceNotPassed)
        {
            judgement = judgement.at_best(Verdict::Partial, ReasonCode::EvidenceUnavailable);
        }
        claims.push(Claim {
            number: 0,
            id: ClaimId::Decision(decision.decision_id.clone()),
            kind: ClaimKind::Decision,
            classification: None,
            text: decision.payload.text.clone(),
            rationale: decision.payload.rationale.clone(),
            session_id: decision.session_id.clone(),
            checkpoint_id: None,
            project_sequence: decision.project_sequence,
            standing: Standing::Active,
            superseded_by: Vec::new(),
            withdrawal_reason: None,
            withdrawn_by: None,
            damaged: decision.damaged
                || decision
                    .evidence
                    .iter()
                    .any(|item| item.sealed.is_none() && !item.other_task),
            judgement,
            evidence,
        });
    }
    claims.sort_by_key(|claim| {
        let (rank, ordinal) = match &claim.id {
            ClaimId::Statement(statement) => (section_rank(statement.section), statement.ordinal),
            ClaimId::Decision(_) => (0, 0),
        };
        (claim.project_sequence, rank, ordinal)
    });
    for (index, claim) in claims.iter_mut().enumerate() {
        claim.number = u64::try_from(index).map_or(u64::MAX, |index| index + 1);
    }
    apply_standing(&mut claims, &view.edges, &view.checkpoints);
    apply_damage(&mut claims, view);
    claims
}

/// Whether a Decision's Evidence satisfies the Evidence rule of a
/// `verified_fact` (P3-D2): every object is read, every test it cites
/// passed, and at least one is a test.
fn decision_evidence_holds(evidence: &[SealedEvidence]) -> bool {
    evidence
        .iter()
        .map(|item| {
            item.sealed
                .as_ref()
                .filter(|_| !item.other_task)
                .map(|(evidence, _, _)| evidence)
        })
        .collect::<Option<Vec<_>>>()
        .is_some_and(|evidence| passed_tests_hold(evidence.into_iter()))
}

/// A Decision's Evidence object judged against the Snapshot that seals it.
/// P3-C2 (T12 N4): Evidence of another Task cannot confirm a Decision of
/// this one: `partial` with `evidence_unavailable`, as missing Evidence
/// would be without a damaged record.
fn decision_evidence(
    item: &SealedEvidence,
    sequence: u64,
    history: &RunHistory,
    probe: &Probe,
) -> ClaimEvidence {
    if item.other_task {
        return ClaimEvidence {
            assessment: EvidenceAssessment {
                evidence_id: item.evidence_id.clone(),
                kind: EvidenceKind::CommandResult,
                canonical_digest: Sha256Digest::of_bytes(b""),
                argv: None,
                run_status: None,
                test_format: None,
                judgement: Judgement::new(Verdict::Partial, [ReasonCode::EvidenceUnavailable]),
            },
            present: false,
            snapshot_id: None,
            base_commit: None,
            snapshot_sequence: sequence,
        };
    }
    match &item.sealed {
        Some((evidence, snapshot, snapshot_evidence)) => ClaimEvidence {
            assessment: EvidenceAssessment {
                evidence_id: item.evidence_id.clone(),
                kind: evidence.kind(),
                canonical_digest: evidence.canonical_digest().clone(),
                argv: argv_of(evidence),
                run_status: evidence.payload().run_status(),
                test_format: evidence.payload().test_format(),
                judgement: judge_evidence(snapshot, snapshot_evidence, evidence, history, probe),
            },
            present: true,
            snapshot_id: Some(snapshot.snapshot_id().clone()),
            base_commit: Some(snapshot.head_oid().to_owned()),
            snapshot_sequence: sequence,
        },
        None => ClaimEvidence {
            assessment: EvidenceAssessment {
                evidence_id: item.evidence_id.clone(),
                kind: EvidenceKind::CommandResult,
                canonical_digest: Sha256Digest::of_bytes(b""),
                argv: None,
                run_status: None,
                test_format: None,
                judgement: Judgement::new(Verdict::Tampered, [ReasonCode::ObjectMissing]),
            },
            present: false,
            snapshot_id: None,
            base_commit: None,
            snapshot_sequence: sequence,
        },
    }
}

/// D1: explicit edges, valid only when both ends are claims of this Task
/// and the target comes first; then each `next_action` superseded by the
/// next one. Withdrawn wins over superseded. P3-C2 (Owner): a fact is
/// superseded only by a fact; any other `supersedes` edge on a fact is not
/// an edge (the `next_action` chain holds no fact: a `verified_fact`
/// `next_action` is of kind fact).
fn apply_standing(claims: &mut [Claim], edges: &[ClaimEdge], checkpoints: &[CheckpointRecord]) {
    let numbers = claims
        .iter()
        .map(|claim| {
            (
                claim.id.clone(),
                (claim.number, claim.project_sequence, claim.kind),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut superseded: BTreeMap<u64, BTreeSet<u64>> = BTreeMap::new();
    let mut withdrawn: BTreeMap<u64, (Option<String>, u64)> = BTreeMap::new();
    for edge in edges {
        let Some(&(target, target_sequence, target_kind)) = numbers.get(&edge.target) else {
            continue;
        };
        match (edge.relation, &edge.by) {
            (EdgeRelation::Supersedes, Some(by)) => {
                if let Some(&(by, _, by_kind)) = numbers.get(by)
                    && target < by
                    && target_kind.may_be_superseded_by(by_kind)
                {
                    superseded.entry(target).or_default().insert(by);
                }
            }
            (EdgeRelation::Withdraws, None) if target_sequence < edge.project_sequence => {
                withdrawn
                    .entry(target)
                    .or_insert_with(|| (edge.reason.clone(), edge.project_sequence));
            }
            _ => {}
        }
    }
    let next_actions = claims
        .iter()
        .filter(|claim| claim.kind == ClaimKind::NextAction)
        .map(|claim| claim.number)
        .collect::<Vec<_>>();
    for pair in next_actions.windows(2) {
        superseded.entry(pair[0]).or_default().insert(pair[1]);
    }
    let edge_sequences = edges
        .iter()
        .map(|edge| edge.project_sequence)
        .collect::<BTreeSet<_>>();
    for claim in claims.iter_mut() {
        if let Some(by) = superseded.get(&claim.number) {
            claim.superseded_by = by.iter().copied().collect();
            claim.standing = Standing::Superseded;
        }
        if let Some((reason, sequence)) = withdrawn.get(&claim.number) {
            claim.standing = Standing::Withdrawn;
            claim.withdrawal_reason.clone_from(reason);
            claim.withdrawn_by = withdrawing_checkpoint(checkpoints, &edge_sequences, *sequence);
        }
    }
}

/// U8 A6 (Owner O4): the Checkpoint that recorded the `withdraws` edge at
/// Project `sequence`, and its Session. `createCheckpoint` appends its own
/// `checkpoint.created` Event and then its edges, one `claim.superseded`
/// Event each, in one write transaction (and only it records `withdraws`
/// edges), so the edge's Checkpoint is the one whose `checkpoint.created`
/// comes right before the unbroken run of edges (`edges`, their Project
/// sequences) that holds the edge. `None` when no readable Checkpoint of
/// the view is there: the store leaves out one that cannot be read, and an
/// earlier Checkpoint never stands in for it (U8 S-F round 1, B2).
fn withdrawing_checkpoint(
    checkpoints: &[CheckpointRecord],
    edges: &BTreeSet<u64>,
    sequence: u64,
) -> Option<(CheckpointId, SessionId)> {
    let mut created = sequence.checked_sub(1)?;
    while edges.contains(&created) {
        created = created.checked_sub(1)?;
    }
    checkpoints
        .iter()
        .find(|record| record.project_sequence == created)
        .map(|record| {
            (
                record.checkpoint.checkpoint_id().clone(),
                record.checkpoint.session_id().clone(),
            )
        })
}

/// D7: a broken Event chain or any damaged record makes every claim
/// `tampered`; each claim says whether it rests on a damaged record. Its
/// Evidence is sealed with the rest of the document ([`seal_tampered`]).
fn apply_damage(claims: &mut [Claim], view: &TaskCapsuleView) {
    let mut reasons = view
        .damaged_records
        .iter()
        .map(|record| record.reason)
        .collect::<BTreeSet<_>>();
    if !view.ledger_chain_valid {
        reasons.insert(ReasonCode::LedgerChainInvalid);
    }
    if reasons.is_empty() {
        return;
    }
    let tampered = Judgement::new(Verdict::Tampered, reasons);
    for claim in claims {
        claim.judgement = claim.judgement.combine(&tampered);
    }
}

/// P3-C2 (Owner, T12 S-6): a `tampered` Capsule holds no `verified` at any
/// level. Every claim, every Evidence verdict inside it, and every rerun
/// takes the Task's integrity reasons on top of its own (its own reasons
/// stay); no later run is offered for reaffirmation.
fn seal_tampered(claims: &mut [Claim], reruns: &mut [Rerun], judgement: &Judgement) {
    let Some(integrity) = integrity_judgement(judgement) else {
        return;
    };
    for claim in claims.iter_mut() {
        claim.judgement = claim.judgement.combine(&integrity);
        for item in &mut claim.evidence {
            item.assessment.judgement = item.assessment.judgement.combine(&integrity);
        }
    }
    for rerun in reruns.iter_mut() {
        rerun.judgement = rerun.judgement.combine(&integrity);
        rerun.verified_run = None;
    }
}

/// D4: the worst verdict of the active facts; none is `partial`. Damage
/// already made every claim `tampered`, so it reaches the Task too; with no
/// active fact it is applied here.
fn task_judgement(claims: &[Claim], view: &TaskCapsuleView) -> Judgement {
    let facts = claims
        .iter()
        .filter(|claim| claim.propagates() && claim.standing == Standing::Active)
        .map(|claim| &claim.judgement);
    let mut judgement = Judgement::worst(facts)
        .unwrap_or_else(|| Judgement::new(Verdict::Partial, [ReasonCode::EvidenceUnavailable]));
    let mut reasons = view
        .damaged_records
        .iter()
        .map(|record| record.reason)
        .collect::<BTreeSet<_>>();
    if !view.ledger_chain_valid {
        reasons.insert(ReasonCode::LedgerChainInvalid);
    }
    if !reasons.is_empty() {
        judgement = judgement.combine(&Judgement::new(Verdict::Tampered, reasons));
    }
    judgement
}

/// D5: the passed runs behind active facts that are `stale` or `partial`
/// and have an argv, deduplicated by argv (element by element) in first-seen
/// order. A run that did not pass is never rerun here. When the Task has a
/// later run with the same argv that is `verified` now, the entry names it
/// for reaffirmation instead.
fn reruns(
    claims: &[Claim],
    view: &TaskCapsuleView,
    history: &RunHistory,
    probe: &Probe,
) -> Vec<Rerun> {
    let mut reruns: Vec<(Rerun, u64)> = Vec::new();
    for claim in claims
        .iter()
        .filter(|claim| claim.propagates() && claim.standing == Standing::Active)
    {
        for item in &claim.evidence {
            let assessment = &item.assessment;
            let (Some(argv), Some(RunStatus::Pass)) = (&assessment.argv, assessment.run_status)
            else {
                continue;
            };
            if !matches!(
                assessment.judgement.verdict(),
                Verdict::Stale | Verdict::Partial
            ) {
                continue;
            }
            match reruns.iter_mut().find(|(rerun, _)| rerun.argv == *argv) {
                Some((rerun, latest)) => {
                    if !rerun.evidence_ids.contains(&assessment.evidence_id) {
                        rerun.evidence_ids.push(assessment.evidence_id.clone());
                    }
                    if !rerun.claims.contains(&claim.number) {
                        rerun.claims.push(claim.number);
                    }
                    rerun.judgement = rerun.judgement.combine(&assessment.judgement);
                    *latest = (*latest).max(item.snapshot_sequence);
                }
                None => reruns.push((
                    Rerun {
                        argv: argv.clone(),
                        evidence_ids: vec![assessment.evidence_id.clone()],
                        claims: vec![claim.number],
                        judgement: assessment.judgement.clone(),
                        verified_run: None,
                    },
                    item.snapshot_sequence,
                )),
            }
        }
    }
    // P3-D: a run is the Snapshot that first seals its Evidence; a later
    // Snapshot that carries the same Evidence over (D3) is not a later run.
    let mut first_run: BTreeMap<&EvidenceId, u64> = BTreeMap::new();
    for run in &view.runs {
        for id in run.snapshot.all_evidence_ids() {
            first_run.entry(id).or_insert(run.project_sequence);
        }
    }
    reruns
        .into_iter()
        .map(|(mut rerun, latest)| {
            let argv = &rerun.argv;
            let first_run = &first_run;
            rerun.verified_run = view
                .runs
                .iter()
                .filter(|run| run.project_sequence > latest)
                .flat_map(|run| {
                    run.evidence.iter().filter_map(move |evidence| {
                        let passed = evidence.payload().run_status() == Some(RunStatus::Pass);
                        let same = argv_of(evidence).as_ref() == Some(argv);
                        let own =
                            first_run.get(evidence.evidence_id()) == Some(&run.project_sequence);
                        (passed
                            && same
                            && own
                            && run.snapshot.contains_evidence(evidence.evidence_id()))
                        .then_some((run, evidence))
                    })
                })
                .rfind(|(run, evidence)| {
                    judge_evidence(&run.snapshot, &run.evidence, evidence, history, probe).verdict()
                        == Verdict::Verified
                })
                .map(|(run, evidence)| {
                    (
                        evidence.evidence_id().clone(),
                        run.snapshot.snapshot_id().clone(),
                    )
                });
            rerun
        })
        .collect()
}

/// Changed paths per base commit: what the probe reported since that commit
/// (the `git diff-tree` to HEAD and the status's tracked paths), the
/// untracked paths, and the status paths of every dirty Snapshot at that
/// commit. Bases over the limit are listed unprobed.
fn base_changes(
    view: &TaskCapsuleView,
    capsule_probe: &CapsuleProbe,
    bases: &CapsuleBases,
    observable: bool,
) -> Vec<BaseChanges> {
    let observation = match &capsule_probe.probe.observation {
        ProbeObservation::Observed(observation) if observable => Some(observation),
        _ => None,
    };
    let snapshots = claim_snapshots(view);
    let probed = bases.probed.iter().map(|base| {
        let since = observation.and_then(|_| capsule_probe.changed_since.get(base).cloned());
        let mut paths = BTreeSet::new();
        let complete = matches!(since, Some(Some(_)));
        if let (Some(observation), Some(Some(since))) = (observation, since) {
            paths.extend(since);
            paths.extend(observation.untracked_paths.iter().cloned());
            for (_, snapshot, _) in snapshots
                .iter()
                .filter(|(_, snapshot, _)| snapshot.head_oid() == base && snapshot.is_dirty())
            {
                paths.extend(porcelain_v2_paths(snapshot.worktree_status_porcelain_v2()).all);
            }
        }
        BaseChanges {
            base_commit: base.clone(),
            probed: true,
            complete,
            paths: paths
                .into_iter()
                .map(|path| format!("repo://{path}"))
                .collect(),
        }
    });
    let omitted = bases.omitted.iter().map(|base| BaseChanges {
        base_commit: base.clone(),
        probed: false,
        complete: false,
        paths: BTreeSet::new(),
    });
    probed.chain(omitted).collect()
}

/// Renders the Capsule of `view` under `capsule_probe` (D4, D5).
///
/// # Errors
///
/// [`RecoveryDomainError::Canonicalization`] if the document cannot be
/// canonicalized.
pub fn task_capsule(
    view: &TaskCapsuleView,
    capsule_probe: &CapsuleProbe,
) -> Result<TaskCapsule, RecoveryDomainError> {
    let probe = &capsule_probe.probe;
    let history = run_history(&view.runs);
    let actions = crate::task_actions(view, &history, probe);
    let mut claims = claims(view, &history, probe);
    let judgement = task_judgement(&claims, view);
    let mut reruns = reruns(&claims, view, &history, probe);
    seal_tampered(&mut claims, &mut reruns, &judgement);
    let bases = capsule_bases(view);
    let mut gaps = BTreeSet::new();
    let mut identity_holds = true;
    for (_, snapshot, evidence) in claim_snapshots(view) {
        if let Err(inherited) = judge_source(
            snapshot,
            snapshot_repository_identity(snapshot, evidence),
            probe,
        ) && inherited.verdict() == Verdict::Tampered
        {
            identity_holds = false;
            gaps.insert(Gap::WorkspaceIdentity);
        }
    }
    let changes = base_changes(view, capsule_probe, &bases, identity_holds);
    if view.checkpoints.is_empty()
        && !view
            .damaged_records
            .iter()
            .any(|record| record.record_type == LedgerRecordType::Checkpoint)
    {
        gaps.insert(Gap::NoCheckpoint);
    }
    let active_facts = claims
        .iter()
        .filter(|claim| claim.propagates() && claim.standing == Standing::Active)
        .collect::<Vec<_>>();
    if active_facts.is_empty() {
        gaps.insert(Gap::NoVerifiedFact);
    }
    if active_facts.iter().any(|claim| claim.evidence.is_empty()) {
        gaps.insert(Gap::StatementWithoutEvidence);
    }
    let untracked = view.checkpoints.iter().any(|record| {
        active_facts
            .iter()
            .any(|claim| claim.checkpoint_id.as_ref() == Some(record.checkpoint.checkpoint_id()))
            && !unselected_untracked(&record.snapshot, &record.evidence).is_empty()
    });
    if untracked {
        gaps.insert(Gap::UntrackedFilesNotCaptured);
    }
    if !view.damaged_records.is_empty() {
        gaps.insert(Gap::RecordIntegrity);
    }
    if !view.ledger_chain_valid {
        gaps.insert(Gap::LedgerChainInvalid);
    }
    if matches!(probe.observation, ProbeObservation::Unavailable) {
        gaps.insert(Gap::WorkspaceUnavailable);
    }
    if changes
        .iter()
        .any(|change| change.probed && !change.complete)
        && matches!(probe.observation, ProbeObservation::Observed(_))
        && identity_holds
    {
        gaps.insert(Gap::BaseCommitUnavailable);
    }
    if !bases.omitted.is_empty() {
        gaps.insert(Gap::BaseCommitsOverLimit);
    }
    let parts = Parts {
        view,
        probe,
        claims: &claims,
        judgement: &judgement,
        reruns: &reruns,
        changes: &changes,
        gaps: &gaps,
        actions: &actions,
    };
    let mut document = document(&parts)?;
    let digest = canonical(&document)?.sha256();
    document["capsule_digest"] = json!(wire(&digest));
    Ok(TaskCapsule {
        document: canonical(&document)?,
        digest,
        judgement,
        claims,
        reruns,
    })
}

struct Parts<'a> {
    view: &'a TaskCapsuleView,
    probe: &'a Probe,
    claims: &'a [Claim],
    judgement: &'a Judgement,
    reruns: &'a [Rerun],
    changes: &'a [BaseChanges],
    gaps: &'a BTreeSet<Gap>,
    actions: &'a [crate::TaskAction],
}

fn canonical(value: &Value) -> Result<CanonicalJson, RecoveryDomainError> {
    CanonicalJson::from_trusted_value(value).map_err(|_| RecoveryDomainError::Canonicalization)
}

fn wire(digest: &Sha256Digest) -> String {
    format!("sha256:{digest}")
}

fn reasons(judgement: &Judgement) -> Vec<&'static str> {
    judgement
        .reasons()
        .iter()
        .map(|reason| reason.as_str())
        .collect()
}

/// FR-406: what a claim rests on, independent of the probe: its identity,
/// kind, classification, text, and the identities and digests of its
/// Evidence.
fn grounding_digest(claim: &Claim) -> Result<Sha256Digest, RecoveryDomainError> {
    canonical(&json!({
        "claim_id": claim.id.wire(),
        "kind": claim.kind.as_str(),
        "classification": claim.classification.map(CheckpointClassification::as_str),
        "text": claim.text,
        "rationale": claim.rationale,
        "evidence": claim.evidence.iter().map(|item| json!({
            "evidence_id": item.assessment.evidence_id,
            "canonical_digest": item.present.then(|| wire(&item.assessment.canonical_digest)),
        })).collect::<Vec<_>>(),
    }))
    .map(|canonical| canonical.sha256())
}

fn claim_document(
    claim: &Claim,
    by_number: &BTreeMap<u64, &Claim>,
) -> Result<Value, RecoveryDomainError> {
    let mut document = json!({
        "number": claim.number,
        "claim_id": claim.id.wire(),
        "kind": claim.kind.as_str(),
        "classification": claim.classification.map(CheckpointClassification::as_str),
        "text": claim.text,
        "rationale": claim.rationale,
        "session_id": claim.session_id,
        "checkpoint_id": claim.checkpoint_id,
        "project_sequence": claim.project_sequence,
        "standing": claim.standing.as_str(),
        "withdrawal_reason": claim.withdrawal_reason,
        "superseded_by": claim.superseded_by.iter().map(|number| json!({
            "number": number,
            "claim_id": by_number.get(number).map(|by| by.id.wire()),
        })).collect::<Vec<_>>(),
        "propagates": claim.propagates(),
        "damaged": claim.damaged,
        "verdict": claim.judgement.verdict().as_str(),
        "reasons": reasons(&claim.judgement),
        "evidence": claim.evidence.iter().map(|item| {
            let mut evidence = json!({
                "evidence_id": item.assessment.evidence_id,
                "kind": item.present.then(|| item.assessment.kind.as_str()),
                "canonical_digest": item.present.then(|| wire(&item.assessment.canonical_digest)),
                "run_status": item.assessment.run_status.map(RunStatus::as_str),
                "argv": item.assessment.argv,
                "snapshot_id": item.snapshot_id,
                "base_commit": item.base_commit,
                "verdict": item.assessment.judgement.verdict().as_str(),
                "reasons": reasons(&item.assessment.judgement),
            });
            // U9 K2 (Owner O6): only a test run that recorded its format
            // has the member, so every other Capsule keeps its bytes.
            if let Some(format) = item.assessment.test_format {
                evidence["test_format"] = json!(format.as_str());
            }
            evidence
        }).collect::<Vec<_>>(),
        "grounding_digest": wire(&grounding_digest(claim)?),
    });
    // U8 A6 (Owner O4): only a withdrawn claim has `withdrawn_by`, so the
    // Capsule of a Task without a withdrawal is the bytes it was; `null`
    // when no readable Checkpoint recorded the edge.
    if claim.standing == Standing::Withdrawn {
        document["withdrawn_by"] = claim
            .withdrawn_by
            .as_ref()
            .map_or(Value::Null, |(checkpoint_id, session_id)| {
                json!({ "checkpoint_id": checkpoint_id, "session_id": session_id })
            });
    }
    Ok(document)
}

/// The base commits of a claim's Evidence, in reference order, once each.
fn claim_bases(claim: &Claim) -> Vec<&str> {
    let mut bases = Vec::new();
    for base in claim
        .evidence
        .iter()
        .filter_map(|item| item.base_commit.as_deref())
    {
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    bases
}

/// One continuation item (D5): its class orders it, `template` and `args`
/// carry what a client renders; wording is not in the document.
fn item(
    class: u8,
    template: &str,
    object_id: &str,
    session_id: Option<&SessionId>,
    judgement: Option<&Judgement>,
    standing: Option<Standing>,
    args: Value,
) -> Value {
    let mut item = json!({
        "class": class,
        "template": template,
        "object_id": object_id,
        "session_id": session_id,
        "verdict": judgement.map(|judgement| judgement.verdict().as_str()),
        "reasons": judgement.map(reasons).unwrap_or_default(),
        "standing": standing.map(Standing::as_str),
    });
    item["args"] = args;
    item
}

/// D5: class ascending, ledger order within a class. Every claim appears
/// exactly once: active ones by kind and verdict, the rest in class 9.
#[allow(clippy::too_many_lines)] // Every class in order, in one place.
fn continuation(parts: &Parts<'_>) -> Vec<Value> {
    let view = parts.view;
    let head = match &parts.probe.observation {
        ProbeObservation::Observed(observation) => Some(observation.head_oid.as_str()),
        ProbeObservation::Unavailable => None,
    };
    let task_id = view.task.task_id.as_str();
    let mut items = vec![item(
        0,
        "header",
        task_id,
        None,
        Some(parts.judgement),
        None,
        json!({
            "title": view.task.title,
            "task_status": view.task.status.as_str(),
            "project_sequence": view.project_sequence,
            "head_oid": head,
            "withdrawn_facts": parts
                .claims
                .iter()
                .filter(|claim| {
                    claim.kind == ClaimKind::Fact && claim.standing == Standing::Withdrawn
                })
                .count(),
        }),
    )];
    if !view.ledger_chain_valid {
        items.push(item(
            1,
            "ledger_chain_invalid",
            task_id,
            None,
            Some(&Judgement::new(
                Verdict::Tampered,
                [ReasonCode::LedgerChainInvalid],
            )),
            None,
            json!({}),
        ));
    }
    for record in &view.damaged_records {
        items.push(item(
            1,
            "damaged_record",
            &record.record_id,
            None,
            Some(&Judgement::new(Verdict::Tampered, [record.reason])),
            None,
            json!({ "record_type": record.record_type.as_str() }),
        ));
    }
    for record in view
        .sessions
        .iter()
        .filter(|record| record.end != SessionEnd::Ended)
    {
        items.push(item(
            2,
            match record.end {
                SessionEnd::Crashed => "session_crashed",
                _ => "session_open",
            },
            record.session.session_id.as_str(),
            Some(&record.session.session_id),
            None,
            None,
            json!({
                "workspace_id": record.session.workspace_id,
                "started_at": record.session.started_at,
            }),
        ));
    }
    let claim_item = |class: u8, template: &str, claim: &Claim, args: Value| {
        item(
            class,
            template,
            &claim.id.wire(),
            Some(&claim.session_id),
            Some(&claim.judgement),
            Some(claim.standing),
            args,
        )
    };
    let active = |claim: &&Claim| claim.standing == Standing::Active;
    let text_args = |claim: &Claim| json!({ "number": claim.number, "text": claim.text });
    let fact_args = |claim: &Claim| {
        json!({
            "number": claim.number,
            "text": claim.text,
            "base_commits": claim_bases(claim),
            "head_oid": head,
        })
    };
    for claim in parts
        .claims
        .iter()
        .filter(active)
        .filter(|claim| claim.kind == ClaimKind::OpenItem)
    {
        items.push(claim_item(3, "open_item", claim, text_args(claim)));
    }
    for claim in parts.claims.iter().filter(active).filter(|claim| {
        claim.kind == ClaimKind::Fact && claim.judgement.verdict() != Verdict::Verified
    }) {
        items.push(claim_item(4, "fact", claim, fact_args(claim)));
    }
    for rerun in parts.reruns {
        let object_id = rerun
            .evidence_ids
            .first()
            .map_or_else(String::new, ToString::to_string);
        items.push(match &rerun.verified_run {
            None => item(
                5,
                "rerun",
                &object_id,
                None,
                Some(&rerun.judgement),
                None,
                json!({
                    "argv": rerun.argv,
                    "evidence_ids": rerun.evidence_ids,
                    "numbers": rerun.claims,
                    "workspace_id": parts.probe.workspace_id,
                    "head_oid": head,
                }),
            ),
            Some((evidence_id, snapshot_id)) => item(
                5,
                "reaffirm",
                &object_id,
                None,
                Some(&rerun.judgement),
                None,
                json!({
                    "argv": rerun.argv,
                    "evidence_ids": rerun.evidence_ids,
                    "numbers": rerun.claims,
                    "verified_evidence_id": evidence_id,
                    "verified_snapshot_id": snapshot_id,
                }),
            ),
        });
    }
    for claim in parts.claims.iter().filter(active).filter(|claim| {
        claim.kind == ClaimKind::Fact && claim.judgement.verdict() == Verdict::Verified
    }) {
        items.push(claim_item(6, "fact", claim, fact_args(claim)));
    }
    for claim in parts
        .claims
        .iter()
        .filter(active)
        .filter(|claim| claim.kind == ClaimKind::Decision)
    {
        items.push(claim_item(
            7,
            "decision",
            claim,
            json!({
                "number": claim.number,
                "text": claim.text,
                "rationale": claim.rationale,
            }),
        ));
    }
    for claim in parts
        .claims
        .iter()
        .filter(active)
        .filter(|claim| claim.kind == ClaimKind::NextAction)
    {
        items.push(claim_item(8, "next_action", claim, text_args(claim)));
    }
    for claim in parts.claims.iter().filter(|claim| !active(claim)) {
        let (template, args) = match claim.standing {
            Standing::Withdrawn => (
                "withdrawn",
                json!({
                    "number": claim.number,
                    "text": claim.text,
                    "superseded_by": claim.superseded_by,
                    "withdrawal_reason": claim.withdrawal_reason,
                }),
            ),
            _ => (
                "superseded",
                json!({
                    "number": claim.number,
                    "text": claim.text,
                    "superseded_by": claim.superseded_by,
                }),
            ),
        };
        items.push(claim_item(9, template, claim, args));
    }
    items
}

/// One item of `sessions`. U8 A2 (Owner O4): `agent` only for a Session
/// that declared one, so the Capsule of Sessions that declared none is the
/// bytes it was. U9 A7 (Owner O3): `ended_by` likewise.
fn session_document(record: &SessionRecord) -> Value {
    let mut session = json!({
        "session_id": record.session.session_id,
        "workspace_id": record.session.workspace_id,
        "started_at": record.session.started_at,
        "ended_at": record.session.ended_at,
        "end_state": record.end.as_str(),
        "last_recorded_at": record.last_recorded_at,
        "resumed_from": record.resumed_from.as_ref().map(|from| from.wire(true)),
    });
    if let Some(agent) = &record.session.agent {
        session["agent"] = agent.recorded();
    }
    // U9 A7 (Owner O3): who ended an abandoned Session, only when its
    // `session.crashed@2` Event names one, so the Capsule of every other
    // Session is the bytes it was.
    if let Some(ended_by) = &record.session.ended_by {
        session["ended_by"] = ended_by.recorded();
    }
    session
}

fn document(parts: &Parts<'_>) -> Result<Value, RecoveryDomainError> {
    let view = parts.view;
    let probe = parts.probe;
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
    let by_number = parts
        .claims
        .iter()
        .map(|claim| (claim.number, claim))
        .collect::<BTreeMap<_, _>>();
    let claims = parts
        .claims
        .iter()
        .map(|claim| claim_document(claim, &by_number))
        .collect::<Result<Vec<_>, _>>()?;
    let sessions = view
        .sessions
        .iter()
        .map(session_document)
        .collect::<Vec<_>>();
    let rerun = parts
        .reruns
        .iter()
        .filter(|rerun| rerun.verified_run.is_none())
        .map(|rerun| {
            json!({
                "argv": rerun.argv,
                "evidence_ids": rerun.evidence_ids,
                "claims": rerun.claims,
                "verdict": rerun.judgement.verdict().as_str(),
                "reasons": reasons(&rerun.judgement),
                "workspace_id": probe.workspace_id,
                "at_head": match &probe.observation {
                    ProbeObservation::Observed(observation) => Some(&observation.head_oid),
                    ProbeObservation::Unavailable => None,
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema_version": 1,
        "ontology_version": ONTOLOGY_VERSION,
        "task": {
            "task_id": view.task.task_id,
            "title": view.task.title,
            "revision": view.task.revision,
            "status": view.task.status.as_str(),
            "workspace_id": view.task.workspace_id,
        },
        "status": parts.judgement.verdict().as_str(),
        "reasons": reasons(parts.judgement),
        "bindings": {
            "ledger": {
                "project_sequence": view.project_sequence,
                "chain_valid": view.ledger_chain_valid,
            },
            "probe": probe_binding,
        },
        "sessions": sessions,
        "claims": claims,
        "rerun": rerun,
        "changed_paths_by_base": parts.changes.iter().map(|change| json!({
            "base_commit": change.base_commit,
            "probed": change.probed,
            "complete": change.complete,
            "paths": change.paths,
        })).collect::<Vec<_>>(),
        "gaps": parts.gaps.iter().map(|gap| gap.as_str()).collect::<Vec<_>>(),
        "damaged_records": view.damaged_records.iter().map(|record| json!({
            "record_type": record.record_type.as_str(),
            "record_id": record.record_id,
            "reason": record.reason.as_str(),
        })).collect::<Vec<_>>(),
        "continuation": continuation(parts),
        "omitted": [],
        "actions": parts.actions.iter().map(crate::TaskAction::document).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
#[path = "capsule_tests.rs"]
pub(crate) mod tests;
