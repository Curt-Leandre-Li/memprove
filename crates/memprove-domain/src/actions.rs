//! ADR 0014 E3 (P4-D): what can be done next with a Task, as the Task
//! Capsule's `actions[]`. Pure: no IO.
//!
//! Each action names one write and is computed at read time by the same
//! functions the daemon's write checks call, so that, under one ledger
//! state and one probe result, an action read as available is not refused
//! by those checks when executed at once, and one read as unavailable is
//! refused with one of the Problem codes it lists (Owner condition b). The
//! actions do not depend on the caller's token; each names the scope its
//! write needs (condition c).
//!
//! The actions (Owner decisions A1, B3, C1 of P4-D/F):
//!
//! - `complete_task`: `transitionTask` to `completed` citing the Task's
//!   passed runs that are `verified` now ([`completion_blocks`], P2-C,
//!   P3-L L3); unavailable when the Task is `tampered` (condition d).
//! - `begin_session`: `createSession` with the Task's `task_id`
//!   ([`session_link_refusals`], P4-D A1).
//! - `cite_latest_run`: `createCheckpoint` with a `verified_fact` citing the
//!   Task's latest run, on the Snapshot that first sealed it
//!   ([`verified_fact_evidence_holds`], [`RunHistory::overruled_by`]);
//!   unavailable (`invalid_transition`) when the Task takes no new Session
//!   and none of its Sessions is open (P4-H1, T20 N-1).
//!
//! There is no `reaffirm` action (B3): `createCheckpoint` does not judge
//! freshness when it writes, so "reaffirmable" is not "not refused"; the
//! continuation's class 5 (`rerun`, `reaffirm`) keeps saying which facts need
//! a rerun.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::{
    CheckpointClassification, CheckpointStatement, CompletionBlock, CompletionEvidence, Evidence,
    EvidenceId, EvidencePayload, Probe, RecordedRun, RunHistory, RunStatus, SessionEnd, Task,
    TaskCapsuleView, Verdict, WorkspaceId, completion_blocks, judge_evidence,
    verified_fact_evidence_holds,
};

/// The scope every action's write needs (`AnyOf[control, mcp-write]`; a
/// `control` token holds it too). Reading the Capsule needs `mcp-read`.
pub const ACTION_SCOPE: &str = "mcp-write";

/// One action of the Task Capsule, in the order `actions[]` lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionKind {
    /// `transitionTask` to `completed`.
    CompleteTask,
    /// `createSession` continuing the Task.
    BeginSession,
    /// `createCheckpoint` with a `verified_fact` citing the latest run.
    CiteLatestRun,
}

impl ActionKind {
    /// Every action, in `actions[]` order.
    pub const ALL: [Self; 3] = [Self::CompleteTask, Self::BeginSession, Self::CiteLatestRun];

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CompleteTask => "complete_task",
            Self::BeginSession => "begin_session",
            Self::CiteLatestRun => "cite_latest_run",
        }
    }

    /// The operation the action's write is.
    #[must_use]
    pub const fn operation(self) -> &'static str {
        match self {
            Self::CompleteTask => "transitionTask",
            Self::BeginSession => "createSession",
            Self::CiteLatestRun => "createCheckpoint",
        }
    }
}

/// Why an action is unavailable: the Problem code its write is refused
/// with (Owner condition a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionReason {
    /// `400 invalid_request`.
    InvalidRequest,
    /// `409 project_workspace_mismatch`.
    ProjectWorkspaceMismatch,
    /// `409 invalid_transition`.
    InvalidTransition,
    /// `409 evidence_stale`.
    EvidenceStale,
}

impl ActionReason {
    /// Every reason, in contract order.
    pub const ALL: [Self; 4] = [
        Self::InvalidRequest,
        Self::ProjectWorkspaceMismatch,
        Self::InvalidTransition,
        Self::EvidenceStale,
    ];

    /// The Problem code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::ProjectWorkspaceMismatch => "project_workspace_mismatch",
            Self::InvalidTransition => "invalid_transition",
            Self::EvidenceStale => "evidence_stale",
        }
    }
}

/// Why a Session cannot be linked to a Task (`createSession` with
/// `task_id`), in the order the store checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLinkRefusal {
    /// The Task is scoped to another Workspace (`409
    /// project_workspace_mismatch`).
    WorkspaceMismatch,
    /// P4-D (Owner A1): the Task is `completed` or `cancelled` (`409
    /// invalid_transition`).
    TaskFinished,
}

/// Every reason a Session in `workspace_id` cannot be linked to `task`, in
/// check order; empty when it can. `workspace_id` `None` checks the state
/// only (a link of an existing Session, which has its own Workspace rule).
/// The store refuses with the first; the Task Capsule's `begin_session`
/// lists them all.
#[must_use]
pub fn session_link_refusals(
    task: &Task,
    workspace_id: Option<&WorkspaceId>,
) -> Vec<SessionLinkRefusal> {
    let mut refusals = Vec::new();
    if let (Some(scope), Some(workspace_id)) = (&task.workspace_id, workspace_id)
        && scope != workspace_id
    {
        refusals.push(SessionLinkRefusal::WorkspaceMismatch);
    }
    if !task.status.takes_sessions() {
        refusals.push(SessionLinkRefusal::TaskFinished);
    }
    refusals
}

/// One action as the Capsule lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskAction {
    /// Which action.
    pub kind: ActionKind,
    /// Why it is unavailable, in check order, each once; empty when it is
    /// available.
    pub reasons: Vec<ActionReason>,
    /// What its write is called with.
    pub args: Value,
}

impl TaskAction {
    /// Whether the action is available.
    #[must_use]
    pub fn available(&self) -> bool {
        self.reasons.is_empty()
    }

    /// The `actions[]` item: `{action, available, reasons, scope, args}`.
    #[must_use]
    pub fn document(&self) -> Value {
        json!({
            "action": self.kind.as_str(),
            "available": self.available(),
            "reasons": self.reasons.iter().map(|reason| reason.as_str()).collect::<Vec<_>>(),
            "scope": ACTION_SCOPE,
            "args": self.args,
        })
    }
}

fn once(reasons: impl IntoIterator<Item = ActionReason>) -> Vec<ActionReason> {
    let mut seen = BTreeSet::new();
    reasons
        .into_iter()
        .filter(|reason| seen.insert(*reason))
        .collect()
}

/// A run: one command or test Evidence object of the Task with the first
/// Snapshot, in ledger order, that seals it (ADR 0014 item D), as
/// [`RunHistory`] places it.
fn first_sealed(runs: &[RecordedRun]) -> Vec<(&RecordedRun, &Evidence)> {
    let mut ordered = runs.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (left.project_sequence, left.snapshot.snapshot_id())
            .cmp(&(right.project_sequence, right.snapshot.snapshot_id()))
    });
    let mut placed: BTreeSet<&EvidenceId> = BTreeSet::new();
    let mut found = Vec::new();
    for run in ordered {
        for id in run
            .snapshot
            .command_evidence_ids()
            .iter()
            .chain(run.snapshot.test_evidence_ids())
        {
            if placed.contains(id) {
                continue;
            }
            let Some(item) = run.evidence.iter().find(|item| item.evidence_id() == id) else {
                continue;
            };
            if item.payload().run_status().is_none() {
                continue;
            }
            placed.insert(id);
            found.push((run, item));
        }
    }
    found
}

fn completion_evidence(run: &RecordedRun, evidence: &Evidence) -> CompletionEvidence {
    CompletionEvidence {
        evidence: evidence.clone(),
        snapshot: Some(run.snapshot.clone()),
        snapshot_evidence: run.evidence.clone(),
    }
}

/// `complete_task`: the passed runs that are `verified` now are cited, and
/// [`completion_blocks`] judges them as `transitionTask` does.
fn complete_task(view: &TaskCapsuleView, history: &RunHistory, probe: &Probe) -> TaskAction {
    let cited = first_sealed(&view.runs)
        .into_iter()
        .filter(|(run, evidence)| {
            evidence.payload().run_status() == Some(RunStatus::Pass)
                && judge_evidence(&run.snapshot, &run.evidence, evidence, history, probe).verdict()
                    == Verdict::Verified
        })
        .map(|(run, evidence)| completion_evidence(run, evidence))
        .collect::<Vec<_>>();
    // P3-L L3: the Task Capsule's integrity check, as the write applies it.
    let tampered = !view.damaged_records.is_empty() || !view.ledger_chain_valid;
    let reasons = completion_blocks(
        &view.task.task_id,
        view.task.status,
        tampered,
        &[],
        &cited,
        history,
        probe,
    )
    .into_iter()
    .map(|block| match block {
        CompletionBlock::InvalidTransition => ActionReason::InvalidTransition,
        CompletionBlock::Tampered | CompletionBlock::Missing | CompletionBlock::NotCompleted(_) => {
            ActionReason::EvidenceStale
        }
    });
    TaskAction {
        kind: ActionKind::CompleteTask,
        reasons: once(reasons),
        args: json!({
            "operation": ActionKind::CompleteTask.operation(),
            "target_state": "completed",
            "expected_revision": view.task.revision,
            "completion_evidence_ids": cited
                .iter()
                .map(|item| item.evidence.evidence_id())
                .collect::<Vec<_>>(),
            "workspace_id": probe.workspace_id,
        }),
    }
}

/// `begin_session`: a Session in the probed Workspace (else the Task's)
/// linked to the Task, as [`session_link_refusals`] judges it.
fn begin_session(view: &TaskCapsuleView, probe: &Probe) -> TaskAction {
    let workspace_id = probe
        .workspace_id
        .clone()
        .or_else(|| view.task.workspace_id.clone());
    let reasons = session_link_refusals(&view.task, workspace_id.as_ref())
        .into_iter()
        .map(|refusal| match refusal {
            SessionLinkRefusal::WorkspaceMismatch => ActionReason::ProjectWorkspaceMismatch,
            SessionLinkRefusal::TaskFinished => ActionReason::InvalidTransition,
        });
    TaskAction {
        kind: ActionKind::BeginSession,
        reasons: once(reasons),
        args: json!({
            "operation": ActionKind::BeginSession.operation(),
            "task_id": view.task.task_id,
            "workspace_id": workspace_id,
            "task_status": view.task.status.as_str(),
            "open_session_ids": view
                .sessions
                .iter()
                .filter(|record| record.end == SessionEnd::Open)
                .map(|record| &record.session.session_id)
                .collect::<Vec<_>>(),
        }),
    }
}

/// Whether a Session of the Task has not ended: the ledger has neither
/// `session.ended` nor `session.crashed` of it and its row has no
/// `ended_at` (either one ending it is enough; the write checks the row).
fn has_open_session(view: &TaskCapsuleView) -> bool {
    view.sessions
        .iter()
        .any(|record| record.end == SessionEnd::Open && record.session.is_active())
}

/// `cite_latest_run`: a `verified_fact` citing the Task's latest run on the
/// Snapshot that first sealed it, judged by the Evidence rule
/// `Checkpoint::create` applies ([`verified_fact_evidence_holds`]) and the
/// overruled-run check of P3-D3 ([`RunHistory::overruled_by`]). Without a
/// run there is nothing to cite: a request with no Snapshot is `400`.
///
/// P4-H1 (Owner, T20 N-1): a Task that takes no new Session (`completed`
/// or `cancelled`, [`crate::TaskStatus::takes_sessions`]) and has no
/// Session that has not ended has no Session the write could use: an ended
/// Session refuses the `checkpoint.created` Event (`409
/// invalid_transition`), and no new one can be linked. That reason comes
/// last, as the write appends the Event after every `400` check.
fn cite_latest_run(view: &TaskCapsuleView, history: &RunHistory) -> TaskAction {
    let latest = first_sealed(&view.runs).pop();
    let mut reasons = Vec::new();
    let args = match latest {
        None => {
            reasons.push(ActionReason::InvalidRequest);
            json!({
                "operation": ActionKind::CiteLatestRun.operation(),
                "evidence_id": null,
                "snapshot_id": null,
                "workspace_id": null,
                "kind": null,
                "run_status": null,
                "argv": null,
                "overruled_by": null,
            })
        }
        Some((run, evidence)) => {
            let holds = CheckpointStatement::new(
                CheckpointClassification::VerifiedFact,
                ActionKind::CiteLatestRun.as_str().to_owned(),
                vec![evidence.evidence_id().clone()],
            )
            .is_ok_and(|statement| verified_fact_evidence_holds(&statement, &run.evidence));
            if !holds {
                reasons.push(ActionReason::InvalidRequest);
            }
            let overruled_by = history.overruled_by(evidence.evidence_id());
            if overruled_by.is_some() {
                reasons.push(ActionReason::InvalidRequest);
            }
            let mut args = json!({
                "operation": ActionKind::CiteLatestRun.operation(),
                "evidence_id": evidence.evidence_id(),
                "snapshot_id": run.snapshot.snapshot_id(),
                "workspace_id": run.snapshot.workspace_id(),
                "kind": evidence.kind().as_str(),
                "run_status": evidence.payload().run_status().map(RunStatus::as_str),
                "argv": match evidence.payload() {
                    EvidencePayload::CommandResult { argv, .. }
                    | EvidencePayload::TestResult { argv, .. } => Some(argv),
                    _ => None,
                },
                "overruled_by": overruled_by,
            });
            // U9 K2 (Owner O6): only a latest run that is a test run that
            // recorded its format has the member.
            if let Some(format) = evidence.payload().test_format() {
                args["test_format"] = json!(format.as_str());
            }
            args
        }
    };
    if !view.task.status.takes_sessions() && !has_open_session(view) {
        reasons.push(ActionReason::InvalidTransition);
    }
    TaskAction {
        kind: ActionKind::CiteLatestRun,
        reasons: once(reasons),
        args,
    }
}

/// The Task Capsule's `actions[]` (ADR 0014 E3, P4-D), in
/// [`ActionKind::ALL`] order: a pure function of the ledger view, the
/// Task's run history, and the probe.
#[must_use]
pub fn task_actions(
    view: &TaskCapsuleView,
    history: &RunHistory,
    probe: &Probe,
) -> Vec<TaskAction> {
    vec![
        complete_task(view, history, probe),
        begin_session(view, probe),
        cite_latest_run(view, history),
    ]
}
