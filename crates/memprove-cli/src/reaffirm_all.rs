//! `checkpoint create --reaffirm-all` (P4-A, Owner): every fact that
//! awaits reaffirmation becomes a `--reaffirm` of the same request, resolved
//! through the Task Capsule read just before sending.
//!
//! A fact awaits reaffirmation when it stands, is not `verified`, and this
//! command names it nowhere else (`--supersedes`, `--withdraw`,
//! `--reaffirm`). With `--evidence` after `--reaffirm-all`, every such fact
//! cites that Evidence, exactly as one `--reaffirm <N> --evidence ...` per
//! fact would. Without it, a fact is reaffirmed only when, for each command
//! its Evidence ran, the latest run of that command (same argv, element by
//! element, compared by the digest the context records, as `task complete`
//! compares runs) among the runs of the current repository state passed;
//! it then cites those runs. Otherwise it is skipped, with the command that
//! did not run or did not pass. The rule is the domain's
//! [`memprove_domain::match_reaffirmation`]; the daemon checks every
//! reaffirmation as before.
//!
//! P4-B (Owner, T18): a run answers only for Evidence of its own kind (SF-1;
//! the context records each run's kind); with `--evidence`, a fact whose
//! cited runs ran other commands than its Evidence is named on standard
//! error before sending (N-a); runs of another Workspace say so (N-c); after
//! the write, the actual verdict of each reaffirmation is read back (N-b);
//! a refusal names the facts the request would have reaffirmed and maps the
//! detail's positions to them; with nothing to reaffirm, a `--next` that
//! differs from the next action in force is still sent (P4-A judgment 8).

use std::collections::BTreeSet;

use memprove_protocol::strict_json::{JsonValue, parse_strict};

use super::{
    CapsuleClaims, CheckpointDraft, ClaimError, ClaimKind, ClaimRef, Draft, Listed, ReaffirmSkip,
    Resolver, RunKind, Standing, StateRun, StateSkip, StatementDraft, StatementKind, Verdict,
    argv_digest, awaits_reaffirmation, command, match_reaffirmation, runs_are_current,
};

/// What `--reaffirm-all` did: the facts it reaffirms, each with the runs
/// it cites, and the facts it skipped, each with its reasons.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReaffirmAllReport {
    /// `(number, cited runs)`, in number order.
    pub reaffirmed: Vec<(u64, Vec<String>)>,
    /// `(number, reasons)`, in number order.
    pub skipped: Vec<(u64, Vec<String>)>,
    /// P4-B (T18 N-a): with `--evidence`, one warning per fact whose cited
    /// runs are not runs of the commands its Evidence ran, in number order.
    pub warnings: Vec<String>,
    /// P4-B: the position of each reaffirmation among the request's
    /// `verified_completed_work`, with the number of the fact it reaffirms.
    pub positions: Vec<(usize, u64)>,
    /// P4-G (ADR 0014 E7.5.6): the code of each reason in
    /// [`ReaffirmAllReport::skipped`], in the same order.
    pub skip_codes: Vec<(u64, Vec<SkipCode>)>,
    /// P4-G: the number of each fact [`ReaffirmAllReport::warnings`] names,
    /// in the same order; each warning's code is
    /// [`WARNING_OTHER_COMMANDS`].
    pub warned: Vec<u64>,
}

/// P4-G (ADR 0014 E7.5.6): why `--reaffirm-all` skipped a fact, one code
/// per clause the CLI prints for it. The CLI's sentence names the command;
/// the code names the case. A closed set: [`SkipCode::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipCode {
    /// This context records no run of the current repository state.
    NoRunRecorded,
    /// The Workspace could not be probed.
    StateUnobserved,
    /// The repository state changed since the latest `memprove run`.
    StateChanged,
    /// The runs this context holds are of another Workspace.
    OtherWorkspace,
    /// The fact cites no Evidence.
    NoEvidence,
    /// A cited Evidence is not a command or test run.
    EvidenceNotARun,
    /// A command of the fact has no run in this repository state.
    NoRunInState,
    /// The latest run of a command of the fact did not pass.
    LatestRunNotPassed,
    /// A command has runs in this state, but no test run, and its Evidence
    /// is a test run.
    NoTestRunInState,
    /// A command has runs in this state, but no command run, and its
    /// Evidence is a command run.
    NoCommandRunInState,
    /// The latest run of the cited kind of a command did not pass.
    LatestRunOfKindNotPassed,
    /// The fact's Evidence holds no test run.
    NoTestEvidence,
}

impl SkipCode {
    /// Every code, in declaration order.
    pub const ALL: [Self; 12] = [
        Self::NoRunRecorded,
        Self::StateUnobserved,
        Self::StateChanged,
        Self::OtherWorkspace,
        Self::NoEvidence,
        Self::EvidenceNotARun,
        Self::NoRunInState,
        Self::LatestRunNotPassed,
        Self::NoTestRunInState,
        Self::NoCommandRunInState,
        Self::LatestRunOfKindNotPassed,
        Self::NoTestEvidence,
    ];

    /// The code as the MCP result writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoRunRecorded => "no_run_recorded",
            Self::StateUnobserved => "state_unobserved",
            Self::StateChanged => "state_changed",
            Self::OtherWorkspace => "other_workspace",
            Self::NoEvidence => "no_evidence",
            Self::EvidenceNotARun => "evidence_not_a_run",
            Self::NoRunInState => "no_run_in_state",
            Self::LatestRunNotPassed => "latest_run_not_passed",
            Self::NoTestRunInState => "no_test_run_in_state",
            Self::NoCommandRunInState => "no_command_run_in_state",
            Self::LatestRunOfKindNotPassed => "latest_run_of_kind_not_passed",
            Self::NoTestEvidence => "no_test_evidence",
        }
    }

    /// The code of one skip reason.
    fn of(skip: &ReaffirmSkip<String, usize>) -> Self {
        match skip {
            ReaffirmSkip::State(StateSkip::Unrecorded) => Self::NoRunRecorded,
            ReaffirmSkip::State(StateSkip::Unobserved) => Self::StateUnobserved,
            ReaffirmSkip::State(StateSkip::Changed) => Self::StateChanged,
            ReaffirmSkip::State(StateSkip::OtherWorkspace) => Self::OtherWorkspace,
            ReaffirmSkip::NoEvidence => Self::NoEvidence,
            ReaffirmSkip::NotACommand { .. } => Self::EvidenceNotARun,
            ReaffirmSkip::NotRun { .. } => Self::NoRunInState,
            ReaffirmSkip::NotPassed { .. } => Self::LatestRunNotPassed,
            ReaffirmSkip::NoRunOfKind {
                kind: RunKind::Test,
                ..
            } => Self::NoTestRunInState,
            ReaffirmSkip::NoRunOfKind {
                kind: RunKind::Command,
                ..
            } => Self::NoCommandRunInState,
            ReaffirmSkip::KindNotPassed { .. } => Self::LatestRunOfKindNotPassed,
            ReaffirmSkip::NoTest => Self::NoTestEvidence,
        }
    }
}

/// P4-G (ADR 0014 E7.5.6): the code of the one warning `--reaffirm-all
/// --evidence` gives: the cited runs are not, as this context recorded
/// them, runs of the commands the fact's Evidence ran.
pub const WARNING_OTHER_COMMANDS: &str = "cited_runs_of_other_commands";

/// `items` joined by `, `, or `none`.
fn listed(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

impl ReaffirmAllReport {
    /// The lines for standard error, in the form of the P3-L L8 report;
    /// the last one counts both. P4-B (T18 N-b): with `after`, the Task
    /// Capsule read after the write and the recorded Checkpoint, each
    /// reaffirmed fact shows the claim that reaffirms it with its actual
    /// verdict and reason codes, and the count says how many of them are
    /// `verified`; without it, when there are reaffirmations, the count says
    /// that their verdicts could not be read.
    #[must_use]
    pub fn lines(&self, after: Option<(&str, &str)>) -> Vec<String> {
        let outcomes =
            after.and_then(|(capsule, checkpoint_id)| self.outcomes(capsule, checkpoint_id));
        let reaffirmed = self
            .reaffirmed
            .iter()
            .enumerate()
            .map(|(index, (number, runs))| {
                let cited = format!("#{number} ({})", runs.join(", "));
                match outcomes.as_ref().and_then(|outcomes| outcomes.get(index)) {
                    Some(Some(outcome)) => format!("{cited} as {}", outcome.shown()),
                    _ => cited,
                }
            })
            .collect::<Vec<_>>();
        let verified = match &outcomes {
            _ if self.reaffirmed.is_empty() => " (verified 0)".to_owned(),
            Some(outcomes) if outcomes.iter().all(Option::is_some) => format!(
                " (verified {})",
                outcomes
                    .iter()
                    .flatten()
                    .filter(|outcome| outcome.verdict == "verified")
                    .count()
            ),
            _ => " (the verified count could not be read back)".to_owned(),
        };
        vec![
            format!("reaffirm-all reaffirmed: {}", listed(&reaffirmed)),
            format!("reaffirm-all skipped: {}", self.skipped_list()),
            format!(
                "reaffirmed {}{verified}, skipped {}",
                self.reaffirmed.len(),
                self.skipped.len()
            ),
        ]
    }

    fn skipped_list(&self) -> String {
        listed(
            &self
                .skipped
                .iter()
                .map(|(number, reasons)| format!("#{number} ({})", reasons.join("; ")))
                .collect::<Vec<_>>(),
        )
    }

    /// The claim that reaffirms each fact, in report order, read from a
    /// Task Capsule after the write of `checkpoint_id`; `None` when the
    /// Capsule is not readable.
    #[must_use]
    pub fn outcomes(&self, capsule: &str, checkpoint_id: &str) -> Option<Vec<Option<Outcome>>> {
        let capsule = parse_strict(capsule).ok()?;
        let claims = capsule.get("claims")?.as_array()?;
        Some(
            self.reaffirmed
                .iter()
                .map(|(number, _)| {
                    let (position, _) = self.positions.iter().find(|(_, fact)| fact == number)?;
                    let claim_id = format!("{checkpoint_id}/verified_completed_work/{position}");
                    let claim = claims.iter().find(|claim| {
                        claim.get("claim_id").and_then(JsonValue::as_str) == Some(claim_id.as_str())
                    })?;
                    Some(Outcome {
                        number: claim.get("number")?.as_i64()?,
                        verdict: claim.get("verdict")?.as_str()?.to_owned(),
                        reasons: claim
                            .get("reasons")?
                            .as_array()?
                            .iter()
                            .filter_map(JsonValue::as_str)
                            .map(str::to_owned)
                            .collect(),
                    })
                })
                .collect(),
        )
    }

    /// P4-B (T18 SF-1): the lines after the daemon refused the request:
    /// nothing was written, what it would have reaffirmed and skipped, and
    /// which fact each position the detail names (`$.verified_completed_work[i]`)
    /// reaffirms; all the positions when the detail names none of them.
    #[must_use]
    pub fn refused_lines(&self, detail: Option<&str>) -> Vec<String> {
        let reaffirmed = self
            .reaffirmed
            .iter()
            .map(|(number, runs)| format!("#{number} ({})", runs.join(", ")))
            .collect::<Vec<_>>();
        let named = detail.map(positions_in).unwrap_or_default();
        let mentioned = self
            .positions
            .iter()
            .filter(|(position, _)| named.contains(position))
            .collect::<Vec<_>>();
        let (label, shown) = if mentioned.is_empty() {
            ("positions in the request", self.positions.iter().collect())
        } else {
            ("positions in the detail", mentioned)
        };
        let mut lines = vec![
            "reaffirm-all: the daemon refused the request, so nothing was written".to_owned(),
            format!(
                "reaffirm-all would have reaffirmed: {}",
                listed(&reaffirmed)
            ),
            format!("reaffirm-all would have skipped: {}", self.skipped_list()),
        ];
        if !shown.is_empty() {
            lines.push(format!(
                "reaffirm-all {label}: {}",
                shown
                    .iter()
                    .map(|(position, number)| {
                        format!("$.verified_completed_work[{position}] reaffirms #{number}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        lines
    }
}

/// The reaffirmation of one fact as the Capsule reads it after the write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The number of the claim that reaffirms the fact.
    pub number: i64,
    /// Its verdict.
    pub verdict: String,
    /// Its reason codes.
    pub reasons: Vec<String>,
}

impl Outcome {
    /// `#7: verified`, or `#8: partial (evidence_unavailable)`.
    fn shown(&self) -> String {
        if self.reasons.is_empty() {
            format!("#{}: {}", self.number, self.verdict)
        } else {
            format!(
                "#{}: {} ({})",
                self.number,
                self.verdict,
                self.reasons.join(", ")
            )
        }
    }
}

/// The statement indices `i` of every `$.verified_completed_work[i]` in a
/// Problem detail.
fn positions_in(detail: &str) -> BTreeSet<usize> {
    const PREFIX: &str = "$.verified_completed_work[";
    detail
        .match_indices(PREFIX)
        .filter_map(|(at, _)| {
            let rest = &detail[at + PREFIX.len()..];
            let digits = rest.split(']').next()?;
            (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| digits.parse().ok())
                .flatten()
        })
        .collect()
}

/// The argv of `digest` among a fact's cited commands, quoted for display.
fn shown(commands: &[Option<(Vec<String>, RunKind)>], digest: &str) -> String {
    commands
        .iter()
        .flatten()
        .find(|(argv, _)| argv_digest(argv) == digest)
        .map_or_else(
            || format!("argv sha256 {digest}"),
            |(argv, _)| command(argv.iter().map(String::as_str)),
        )
}

/// The cited commands of a fact, each once, quoted, in citation order.
fn commands_shown(commands: &[Option<(Vec<String>, RunKind)>]) -> String {
    let mut seen = Vec::new();
    for (argv, _) in commands.iter().flatten() {
        let quoted = command(argv.iter().map(String::as_str));
        if !seen.contains(&quoted) {
            seen.push(quoted);
        }
    }
    if seen.is_empty() {
        "no command".to_owned()
    } else {
        seen.join(", ")
    }
}

/// The claim IDs the command names besides `--reaffirm-all`: its
/// `--supersedes`, `--withdraw`, and `--reaffirm` targets.
fn named_claims(
    draft: &CheckpointDraft,
    resolver: &Resolver<'_>,
) -> Result<Vec<String>, ClaimError> {
    let mut named = resolver.ids(&draft.next_supersedes)?;
    named.extend(resolver.ids(&draft.withdrawals)?);
    for statement in &draft.statements {
        named.extend(resolver.ids(&statement.supersedes)?);
        if let StatementKind::Reaffirm(reference) = &statement.kind {
            named.push(resolver.id(reference)?);
        }
    }
    Ok(named)
}

/// The facts that await reaffirmation and that `named` leaves to other
/// flags, in number order.
fn awaiting<'a>(claims: &'a CapsuleClaims, named: &[String]) -> Vec<&'a Listed> {
    let mut awaiting = claims
        .claims
        .iter()
        .filter(|claim| {
            let kind = ClaimKind::parse(&claim.kind);
            let standing = Standing::parse(&claim.standing);
            let verdict = claim.verdict.as_deref().and_then(Verdict::parse);
            matches!((kind, standing, verdict), (Some(kind), Some(standing), Some(verdict))
                if awaits_reaffirmation(kind, standing, verdict))
                && !named.contains(&claim.claim_id)
        })
        .collect::<Vec<_>>();
    awaiting.sort_by_key(|claim| claim.number);
    awaiting
}

/// The context's runs of the state, as the domain matches them, named by
/// their index.
fn state_runs(draft: &CheckpointDraft) -> Vec<StateRun<String, usize>> {
    draft
        .state_runs
        .iter()
        .enumerate()
        .map(|(index, run)| StateRun {
            label: index,
            command: run.argv_sha256.clone(),
            kind: run.kind.as_deref().and_then(RunKind::of_evidence),
            passed: run.status.as_deref() == Some("PASS"),
        })
        .collect()
}

/// The label of a run of the context, or the Evidence ID as given.
fn label_of(draft: &CheckpointDraft, evidence_id: &String) -> String {
    draft
        .state_runs
        .iter()
        .find(|run| &run.evidence_id == evidence_id)
        .map_or_else(|| evidence_id.clone(), |run| run.label.clone())
}

/// Mode one for one fact: the Evidence IDs of the runs it is reaffirmed
/// on, or the reasons it is skipped, each with its code.
fn mode_one(
    draft: &CheckpointDraft,
    claim: &Listed,
    current: Result<(), StateSkip>,
    runs: &[StateRun<String, usize>],
) -> Result<Vec<String>, Vec<(SkipCode, String)>> {
    let cited = claim
        .commands
        .iter()
        .map(|cited| {
            cited
                .as_ref()
                .map(|(argv, kind)| (argv_digest(argv), *kind))
        })
        .collect::<Vec<_>>();
    current
        .map_err(|state| vec![ReaffirmSkip::State(state)])
        .and_then(|()| match_reaffirmation(&cited, runs))
        .map(|chosen| {
            chosen
                .into_iter()
                .map(|index| draft.state_runs[index].evidence_id.clone())
                .collect()
        })
        .map_err(|skips| {
            skips
                .into_iter()
                .map(|skip| {
                    (
                        SkipCode::of(&skip),
                        reason(&skip, &claim.commands, &draft.state_runs, draft),
                    )
                })
                .collect()
        })
}

/// P4-B (T18 N-a): with `--evidence`, a warning when the given runs are
/// not, as this context recorded them, runs of exactly the commands the
/// fact's Evidence ran; `None` when they are.
fn other_commands(draft: &CheckpointDraft, claim: &Listed, evidence: &[String]) -> Option<String> {
    let wanted = claim
        .commands
        .iter()
        .flatten()
        .map(|(argv, _)| Some(argv_digest(argv)))
        .collect::<BTreeSet<_>>();
    let given = evidence
        .iter()
        .map(|id| {
            draft
                .state_runs
                .iter()
                .find(|run| &run.evidence_id == id)
                .and_then(|run| run.argv_sha256.clone())
        })
        .collect::<BTreeSet<_>>();
    (given != wanted).then(|| {
        format!(
            "reaffirm-all: #{} is reaffirmed on {}, which this context does not record as runs \
             of the commands its Evidence ran ({}); it is sent as given",
            claim.number,
            evidence
                .iter()
                .map(|id| label_of(draft, id))
                .collect::<Vec<_>>()
                .join(", "),
            commands_shown(&claim.commands)
        )
    })
}

impl Draft {
    /// Replaces `--reaffirm-all` by one reaffirmation per fact that awaits
    /// one, at its place, in number order, and says what it did; `None`
    /// when the draft has no `--reaffirm-all`.
    ///
    /// # Errors
    ///
    /// A reference of the same command that the Capsule does not resolve.
    pub fn expand_reaffirm_all(
        &mut self,
        claims: &CapsuleClaims,
    ) -> Result<Option<ReaffirmAllReport>, ClaimError> {
        let Self::Checkpoint(draft) = self else {
            return Ok(None);
        };
        let Some(position) = draft
            .statements
            .iter()
            .position(|statement| statement.kind == StatementKind::ReaffirmAll)
        else {
            return Ok(None);
        };
        let named = named_claims(draft, &Resolver(Some(claims)))?;
        let awaiting = awaiting(claims, &named);
        let all = draft.statements.remove(position);
        let runs = state_runs(draft);
        // Without --evidence, only runs of the current repository state and
        // (P4-B, T18 N-c) of the Checkpoint's Workspace.
        let current = runs_are_current(
            draft
                .state_workspace
                .as_ref()
                .is_none_or(|workspace| *workspace == draft.workspace_id),
            draft.state_manifest.as_deref(),
            claims.probe_manifest.as_deref(),
        );
        let mut report = ReaffirmAllReport::default();
        let mut expanded = Vec::new();
        for claim in awaiting {
            let evidence = if all.evidence.is_empty() {
                match mode_one(draft, claim, current, &runs) {
                    Ok(evidence) => evidence,
                    Err(reasons) => {
                        let (codes, reasons) = reasons.into_iter().unzip();
                        report.skipped.push((claim.number, reasons));
                        report.skip_codes.push((claim.number, codes));
                        continue;
                    }
                }
            } else {
                if let Some(warning) = other_commands(draft, claim, &all.evidence) {
                    report.warnings.push(warning);
                    report.warned.push(claim.number);
                }
                all.evidence.clone()
            };
            report.reaffirmed.push((
                claim.number,
                evidence.iter().map(|id| label_of(draft, id)).collect(),
            ));
            expanded.push(StatementDraft {
                kind: StatementKind::Reaffirm(ClaimRef::Id(claim.claim_id.clone())),
                evidence,
                supersedes: Vec::new(),
            });
        }
        let first = draft.statements[..position]
            .iter()
            .filter(|statement| statement.is_fact())
            .count();
        report.positions = report
            .reaffirmed
            .iter()
            .enumerate()
            .map(|(index, (number, _))| (first + index, *number))
            .collect();
        draft.statements.splice(position..position, expanded);
        Ok(Some(report))
    }

    /// Whether the request still records anything besides its next action:
    /// a statement, a withdrawal, or a supersession by the next action
    /// (P4-A: otherwise `--reaffirm-all` with nothing to reaffirm sends
    /// nothing).
    #[must_use]
    pub fn records_anything(&self) -> bool {
        match self {
            Self::Checkpoint(draft) => {
                !draft.statements.is_empty()
                    || !draft.withdrawals.is_empty()
                    || !draft.next_supersedes.is_empty()
            }
            Self::Decision(_) => true,
        }
    }

    /// P4-A judgment 8, Owner supplement of P4-B: whether the request's
    /// `--next` differs from the next action in force in `claims` (none in
    /// force differs from any). A `--reaffirm-all` with nothing to
    /// reaffirm is still sent when it does.
    #[must_use]
    pub fn next_differs(&self, claims: &CapsuleClaims) -> bool {
        match self {
            Self::Checkpoint(draft) => claims.next_action.as_deref() != Some(draft.next.as_str()),
            Self::Decision(_) => true,
        }
    }
}

/// One skip reason in words.
fn reason(
    skip: &ReaffirmSkip<String, usize>,
    commands: &[Option<(Vec<String>, RunKind)>],
    runs: &[crate::context::RunLabel],
    draft: &CheckpointDraft,
) -> String {
    match skip {
        ReaffirmSkip::State(StateSkip::Unrecorded) => "this context records no run of the \
            current repository state; run the commands with `memprove run` first"
            .to_owned(),
        ReaffirmSkip::State(StateSkip::Unobserved) => "the Workspace could not be probed, so \
            whether the runs are of the current repository state is unknown"
            .to_owned(),
        ReaffirmSkip::State(StateSkip::Changed) => "the repository state changed since the \
            latest `memprove run`, so its runs are of an earlier state; run the commands again"
            .to_owned(),
        ReaffirmSkip::State(StateSkip::OtherWorkspace) => format!(
            "the runs this context holds are of Workspace {}, not of the Checkpoint's Workspace \
             {}; run the commands in that Workspace with `memprove run`",
            draft.state_workspace.as_deref().unwrap_or("unknown"),
            draft.workspace_id
        ),
        ReaffirmSkip::NoEvidence => {
            "it cites no Evidence, so there is no command to rerun".to_owned()
        }
        ReaffirmSkip::NotACommand { evidence } => format!(
            "its Evidence {} is not a command or test run, so there is no command to rerun",
            evidence + 1
        ),
        ReaffirmSkip::NotRun { command } => format!(
            "{} has no run in this repository state",
            shown(commands, command)
        ),
        ReaffirmSkip::NotPassed { command, latest } => format!(
            "the latest run {} of {} in this repository state did not pass ({})",
            runs[*latest].label,
            shown(commands, command),
            status(&runs[*latest])
        ),
        ReaffirmSkip::NoRunOfKind {
            command,
            kind: RunKind::Test,
        } => format!(
            "{} has no test run in this repository state that this context recorded, and its \
             Evidence is a test run; rerun it as a test: `memprove run --test-format cargo -- \
             <argv>`, or `memprove run --test --test-cases <N> -- <argv>`",
            shown(commands, command)
        ),
        ReaffirmSkip::NoRunOfKind {
            command,
            kind: RunKind::Command,
        } => format!(
            "{} has no command run (one without --test or --test-format) in this repository \
             state that this context recorded, and its Evidence is a command run; rerun it as \
             one: `memprove run -- <argv>`",
            shown(commands, command)
        ),
        ReaffirmSkip::KindNotPassed {
            command,
            kind,
            latest,
        } => format!(
            "the latest {} run {} of {} in this repository state did not pass ({})",
            match kind {
                RunKind::Test => "test",
                RunKind::Command => "command",
            },
            runs[*latest].label,
            shown(commands, command),
            status(&runs[*latest])
        ),
        ReaffirmSkip::NoTest => "its Evidence holds no test run, and a reaffirmed fact needs a \
            passed test run; cite one with `--reaffirm <N> --evidence <LABEL>`"
            .to_owned(),
    }
}

fn status(run: &crate::context::RunLabel) -> &str {
    run.status.as_deref().unwrap_or("no result recorded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        claims::CheckpointDraft,
        context::{RunLabel, argv_digest},
    };

    const C: &str = "018f47a0-0000-7000-8000-000000000001";

    fn argv(words: &str) -> Vec<String> {
        words.split(' ').map(str::to_owned).collect()
    }

    /// A Capsule whose facts cite runs of these commands (`-` for repository
    /// Evidence without argv).
    fn capsule(claims: &[(u64, &str, &str, &str, &[&str])]) -> String {
        let claims = claims
            .iter()
            .map(|(number, kind, standing, verdict, commands)| {
                let evidence = commands
                    .iter()
                    .map(|command| {
                        if *command == "-" {
                            r#"{"evidence_id":"G","kind":"git_head","argv":null}"#.to_owned()
                        } else {
                            let words = argv(command)
                                .iter()
                                .map(|word| format!("\"{word}\""))
                                .collect::<Vec<_>>()
                                .join(",");
                            format!(r#"{{"evidence_id":"E","kind":"test_result","argv":[{words}]}}"#)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{"number":{number},"claim_id":"{C}/verified_completed_work/{number}","kind":"{kind}",
                        "text":"t{number}","standing":"{standing}","verdict":"{verdict}","evidence":[{evidence}]}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"status":"stale","reasons":[],"claims":[{claims}],
                "bindings":{{"probe":{{"available":true,"capture_manifest_digest":"M"}}}}}}"#
        )
    }

    fn run(label: &str, command: Option<&str>, status: &str) -> RunLabel {
        kinded(label, command, "test_result", status)
    }

    fn kinded(label: &str, command: Option<&str>, kind: &str, status: &str) -> RunLabel {
        RunLabel {
            label: label.to_owned(),
            evidence_id: format!("id-{label}"),
            status: Some(status.to_owned()),
            argv_sha256: command.map(|command| argv_digest(&argv(command))),
            kind: Some(kind.to_owned()),
            argv: None,
        }
    }

    fn draft(statements: Vec<StatementDraft>, runs: Vec<RunLabel>) -> Draft {
        Draft::Checkpoint(CheckpointDraft {
            workspace_id: "W".to_owned(),
            task_id: "T".to_owned(),
            session_id: "S".to_owned(),
            snapshot_id: Ok("N".to_owned()),
            objective: "o".to_owned(),
            statements,
            next: "n".to_owned(),
            next_supersedes: Vec::new(),
            withdrawals: Vec::new(),
            withdrawal_reason: None,
            withdraw_session: None,
            state_runs: runs,
            state_manifest: Some("M".to_owned()),
            state_workspace: Some("W".to_owned()),
        })
    }

    fn statement(kind: StatementKind, evidence: &[&str]) -> StatementDraft {
        StatementDraft {
            kind,
            evidence: evidence.iter().map(|id| (*id).to_owned()).collect(),
            supersedes: Vec::new(),
        }
    }

    fn statements(draft: &Draft) -> Vec<(StatementKind, Vec<String>)> {
        let Draft::Checkpoint(draft) = draft else {
            unreachable!()
        };
        draft
            .statements
            .iter()
            .map(|statement| (statement.kind.clone(), statement.evidence.clone()))
            .collect()
    }

    fn reaffirm(number: u64, evidence: &[&str]) -> (StatementKind, Vec<String>) {
        (
            StatementKind::Reaffirm(ClaimRef::Id(format!(
                "{C}/verified_completed_work/{number}"
            ))),
            evidence.iter().map(|id| (*id).to_owned()).collect(),
        )
    }

    const TESTS: &str = "sh tests/a.sh";
    const LINT: &str = "sh tests/b.sh";
    const DOCS: &str = "sh tests/c.sh";

    /// Tests a, b, d, e of P4-A at the CLI: three stale facts on three
    /// commands, two of them rerun and passed; a pass then a failure; a
    /// fact on two commands with one rerun; a verified fact.
    #[test]
    fn without_evidence_the_latest_run_of_each_command_must_pass() {
        let body = capsule(&[
            (1, "fact", "active", "stale", &[TESTS]),
            (2, "fact", "active", "stale", &[LINT]),
            (3, "fact", "active", "stale", &[DOCS]),
            (4, "fact", "active", "partial", &["sh tests/flaky.sh"]),
            (5, "fact", "active", "stale", &[TESTS, DOCS]),
            (6, "fact", "active", "verified", &[TESTS]),
            (7, "open_item", "active", "stale", &[TESTS]),
            (8, "fact", "superseded", "stale", &[TESTS]),
            (9, "fact", "active", "stale", &["-"]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let runs = vec![
            run("r1", Some(TESTS), "PASS"),
            run("r2", Some(LINT), "PASS"),
            run("r3", Some("sh tests/flaky.sh"), "PASS"),
            run("r4", Some("sh tests/flaky.sh"), "FAIL"),
            run("r5", None, "PASS"),
        ];
        let mut all = draft(vec![statement(StatementKind::ReaffirmAll, &[])], runs);
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(
            statements(&all),
            [reaffirm(1, &["id-r1"]), reaffirm(2, &["id-r2"])]
        );
        assert_eq!(
            report.reaffirmed,
            [(1, vec!["r1".to_owned()]), (2, vec!["r2".to_owned()])]
        );
        let skipped = report
            .skipped
            .iter()
            .map(|(number, reasons)| (*number, reasons.join("; ")))
            .collect::<Vec<_>>();
        assert_eq!(
            skipped,
            [
                (
                    3,
                    "「sh tests/c.sh」 has no run in this repository state".to_owned()
                ),
                (
                    4,
                    "the latest run r4 of 「sh tests/flaky.sh」 in this repository state did not \
                     pass (FAIL)"
                        .to_owned()
                ),
                (
                    5,
                    "「sh tests/c.sh」 has no run in this repository state".to_owned()
                ),
                (
                    9,
                    "its Evidence 1 is not a command or test run, so there is no command to rerun"
                        .to_owned()
                ),
            ]
        );
        assert_eq!(
            report.lines(None),
            [
                "reaffirm-all reaffirmed: #1 (r1), #2 (r2)",
                "reaffirm-all skipped: #3 (「sh tests/c.sh」 has no run in this repository state), \
                 #4 (the latest run r4 of 「sh tests/flaky.sh」 in this repository state did not \
                 pass (FAIL)), #5 (「sh tests/c.sh」 has no run in this repository state), #9 (its \
                 Evidence 1 is not a command or test run, so there is no command to rerun)",
                "reaffirmed 2 (the verified count could not be read back), skipped 4",
            ]
        );
        assert!(all.records_anything());
    }

    /// Test f: a fact the same command already names is left to that flag;
    /// the expansion takes the place of --reaffirm-all.
    #[test]
    fn a_fact_named_elsewhere_in_the_command_is_not_reaffirmed_again() {
        let body = capsule(&[
            (1, "fact", "active", "stale", &[TESTS]),
            (2, "fact", "active", "stale", &[TESTS]),
            (3, "fact", "active", "stale", &[TESTS]),
            (4, "fact", "active", "stale", &[TESTS]),
            (5, "fact", "active", "stale", &[TESTS]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let mut superseding = statement(StatementKind::Fact("new".to_owned()), &["id-r1"]);
        superseding.supersedes = vec![ClaimRef::Number(2)];
        let mut all = draft(
            vec![
                statement(StatementKind::Reaffirm(ClaimRef::Number(1)), &["id-r1"]),
                statement(StatementKind::ReaffirmAll, &[]),
                superseding,
            ],
            vec![run("r1", Some(TESTS), "PASS")],
        );
        if let Draft::Checkpoint(inner) = &mut all {
            inner.withdrawals = vec![ClaimRef::Id(format!("{C}/verified_completed_work/3"))];
            inner.withdrawal_reason = Some("gone".to_owned());
        }
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(
            report
                .reaffirmed
                .iter()
                .map(|(number, _)| *number)
                .collect::<Vec<_>>(),
            [4, 5]
        );
        let kinds = statements(&all)
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                StatementKind::Reaffirm(ClaimRef::Number(1)),
                reaffirm(4, &[]).0,
                reaffirm(5, &[]).0,
                StatementKind::Fact("new".to_owned()),
            ]
        );
    }

    /// Mode two: --evidence after --reaffirm-all gives the same request as
    /// one --reaffirm per waiting fact with the same --evidence.
    #[test]
    fn with_evidence_it_is_one_reaffirm_per_fact_with_that_evidence() {
        let body = capsule(&[
            (1, "fact", "active", "stale", &[TESTS]),
            (2, "fact", "active", "verified", &[TESTS]),
            (3, "fact", "active", "partial", &[DOCS]),
            (4, "fact", "withdrawn", "stale", &[TESTS]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let runs = vec![
            run("r1", Some(LINT), "PASS"),
            run("r2", Some(TESTS), "FAIL"),
        ];
        let mut all = draft(
            vec![
                statement(StatementKind::Open("o".to_owned()), &[]),
                statement(StatementKind::ReaffirmAll, &["id-r1", "id-r2"]),
            ],
            runs.clone(),
        );
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        let by_hand = draft(
            vec![
                statement(StatementKind::Open("o".to_owned()), &[]),
                statement(
                    StatementKind::Reaffirm(ClaimRef::Number(1)),
                    &["id-r1", "id-r2"],
                ),
                statement(
                    StatementKind::Reaffirm(ClaimRef::Number(3)),
                    &["id-r1", "id-r2"],
                ),
            ],
            runs,
        );
        assert_eq!(
            all.render(Some(&claims)).unwrap(),
            by_hand.render(Some(&claims)).unwrap()
        );
        assert_eq!(all.reaffirmed_facts(), by_hand.reaffirmed_facts());
        assert_eq!(
            report.lines(None),
            [
                "reaffirm-all reaffirmed: #1 (r1, r2), #3 (r1, r2)",
                "reaffirm-all skipped: none",
                "reaffirmed 2 (the verified count could not be read back), skipped 0",
            ]
        );
    }

    /// Without --evidence, runs count only while the Workspace is in their
    /// state: a changed, unobserved, or unrecorded state skips every fact;
    /// with --evidence the state is not compared.
    #[test]
    fn runs_of_another_state_are_not_used() {
        let claims_in = |probe: &str| {
            let body = capsule(&[(1, "fact", "active", "stale", &[TESTS])])
                .replace(r#""capture_manifest_digest":"M""#, probe);
            CapsuleClaims::from_body(&body).unwrap()
        };
        let runs = vec![run("r1", Some(TESTS), "PASS")];
        for (probe, manifest, reason) in [
            (
                r#""capture_manifest_digest":"X""#,
                Some("M"),
                "the repository state changed since the latest `memprove run`",
            ),
            (
                r#""capture_manifest_digest":null"#,
                Some("M"),
                "the Workspace could not be probed",
            ),
            (
                r#""capture_manifest_digest":"M""#,
                None,
                "this context records no run of the current repository state",
            ),
        ] {
            let claims = claims_in(probe);
            let mut all = draft(
                vec![statement(StatementKind::ReaffirmAll, &[])],
                runs.clone(),
            );
            if let Draft::Checkpoint(inner) = &mut all {
                inner.state_manifest = manifest.map(str::to_owned);
            }
            let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
            assert!(report.reaffirmed.is_empty(), "{probe}");
            assert!(report.skipped[0].1[0].starts_with(reason), "{report:?}");
            assert!(!all.records_anything());
            let mut given = draft(
                vec![statement(StatementKind::ReaffirmAll, &["id-r1"])],
                runs.clone(),
            );
            if let Draft::Checkpoint(inner) = &mut given {
                inner.state_manifest = manifest.map(str::to_owned);
            }
            let report = given.expand_reaffirm_all(&claims).unwrap().unwrap();
            assert_eq!(report.reaffirmed, [(1, vec!["r1".to_owned()])]);
        }
        // The Capsule is read with the Checkpoint's Workspace to probe it.
        let all = draft(vec![statement(StatementKind::ReaffirmAll, &[])], runs);
        assert_eq!(all.probe_workspace(), Some("W"));
        assert_eq!(draft(Vec::new(), Vec::new()).probe_workspace(), None);
    }

    /// Nothing waits, or everything is skipped: the request records nothing
    /// unless the command holds something else.
    #[test]
    fn nothing_to_reaffirm_records_nothing_unless_something_else_is_asked() {
        let body = capsule(&[
            (1, "fact", "active", "verified", &[TESTS]),
            (2, "fact", "active", "stale", &[DOCS]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let mut all = draft(vec![statement(StatementKind::ReaffirmAll, &[])], Vec::new());
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert!(report.reaffirmed.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(!all.records_anything());
        let mut withdrawing = draft(vec![statement(StatementKind::ReaffirmAll, &[])], Vec::new());
        if let Draft::Checkpoint(inner) = &mut withdrawing {
            inner.withdrawals = vec![ClaimRef::Number(2)];
        }
        withdrawing.expand_reaffirm_all(&claims).unwrap();
        assert!(withdrawing.records_anything());
        let mut plain = draft(Vec::new(), Vec::new());
        assert_eq!(plain.expand_reaffirm_all(&claims), Ok(None));
        assert!(!plain.records_anything());
        // The placeholder alone cannot be rendered.
        let unexpanded = draft(vec![statement(StatementKind::ReaffirmAll, &[])], Vec::new());
        assert!(unexpanded.needs_capsule());
        assert!(unexpanded.render(Some(&claims)).is_err());
    }

    /// P4-B 4a (T18 SF-1): a fact on a test run is reaffirmed only on a
    /// test run. The latest run of its argv is a passed command run: with a
    /// passed test run of the argv in the state, that test run; without,
    /// the fact is skipped with how to rerun it. The other facts go on.
    #[test]
    fn p4b_a_run_answers_only_for_evidence_of_its_kind() {
        let body = capsule(&[
            (1, "fact", "active", "stale", &[TESTS]),
            (2, "fact", "active", "stale", &[LINT]),
            (3, "fact", "active", "stale", &[DOCS]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let runs = vec![
            kinded("r1", Some(TESTS), "test_result", "PASS"),
            kinded("r2", Some(TESTS), "command_result", "PASS"),
            kinded("r3", Some(LINT), "command_result", "PASS"),
            kinded("r4", Some(DOCS), "test_result", "PASS"),
        ];
        let mut all = draft(vec![statement(StatementKind::ReaffirmAll, &[])], runs);
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(
            statements(&all),
            [reaffirm(1, &["id-r1"]), reaffirm(3, &["id-r4"])]
        );
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, 2);
        assert_eq!(
            report.skipped[0].1,
            [
                "「sh tests/b.sh」 has no test run in this repository state that this context \
              recorded, and its Evidence is a test run; rerun it as a test: `memprove run \
              --test-format cargo -- <argv>`, or `memprove run --test --test-cases <N> -- \
              <argv>`"
            ]
        );
        assert_eq!(report.positions, [(0, 1), (1, 3)]);
        // A run of unknown kind (a context written before P4-B) answers for
        // nothing.
        let mut unknown = draft(
            vec![statement(StatementKind::ReaffirmAll, &[])],
            vec![RunLabel {
                kind: None,
                ..run("r1", Some(TESTS), "PASS")
            }],
        );
        let report = unknown.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert!(report.reaffirmed.is_empty());
        // Test PASS, test FAIL, command PASS (T18 TC): the latest test run
        // did not pass.
        let mut failed = draft(
            vec![statement(StatementKind::ReaffirmAll, &[])],
            vec![
                kinded("r1", Some(TESTS), "test_result", "PASS"),
                kinded("r2", Some(TESTS), "test_result", "FAIL"),
                kinded("r3", Some(TESTS), "command_result", "PASS"),
            ],
        );
        let report = failed.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert!(report.skipped[0].1[0].starts_with(
            "the latest test run r2 of 「sh tests/a.sh」 in this repository state did not pass \
             (FAIL)"
        ));
    }

    /// P4-B 4b: a refusal names what the request would have reaffirmed
    /// and skipped, says nothing was written, and maps each position the
    /// detail names to the fact it reaffirms.
    #[test]
    fn p4b_a_refusal_names_the_facts_of_the_positions() {
        let body = capsule(&[
            (1, "fact", "active", "stale", &[TESTS]),
            (2, "fact", "active", "stale", &[DOCS]),
            (3, "fact", "active", "stale", &[TESTS]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let mut all = draft(
            vec![
                statement(StatementKind::Fact("new".to_owned()), &["id-r1"]),
                statement(StatementKind::Open("o".to_owned()), &[]),
                statement(StatementKind::ReaffirmAll, &[]),
            ],
            vec![run("r1", Some(TESTS), "PASS")],
        );
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(report.positions, [(1, 1), (2, 3)]);
        let detail = "1 verified_fact citation names an overruled passed run: (1) \
                      $.verified_completed_work[2].evidence_ids[0]. (1) Evidence x passed; ...";
        assert_eq!(
            report.refused_lines(Some(detail)),
            [
                "reaffirm-all: the daemon refused the request, so nothing was written",
                "reaffirm-all would have reaffirmed: #1 (r1), #3 (r1)",
                "reaffirm-all would have skipped: #2 (「sh tests/c.sh」 has no run in this \
                 repository state)",
                "reaffirm-all positions in the detail: $.verified_completed_work[2] reaffirms #3",
            ]
        );
        assert_eq!(
            report.refused_lines(Some("$.verified_completed_work[0]: no"))[3],
            "reaffirm-all positions in the request: $.verified_completed_work[1] reaffirms #1, \
             $.verified_completed_work[2] reaffirms #3"
        );
        assert_eq!(report.refused_lines(None).len(), 4);
    }

    /// P4-B 4c (T18 N-b): after the write, each reaffirmed fact shows the
    /// claim that reaffirms it with its actual verdict, and the count says
    /// how many are `verified`.
    #[test]
    fn p4b_the_lines_read_back_the_actual_verdicts() {
        let report = ReaffirmAllReport {
            reaffirmed: vec![(1, vec!["r1".to_owned()]), (3, vec!["r2".to_owned()])],
            skipped: vec![(2, vec!["why".to_owned()])],
            warnings: Vec::new(),
            positions: vec![(0, 1), (1, 3)],
            ..ReaffirmAllReport::default()
        };
        let after = format!(
            r#"{{"claims":[
                {{"number":5,"claim_id":"{C}/verified_completed_work/0","verdict":"verified","reasons":[]}},
                {{"number":6,"claim_id":"{C}/verified_completed_work/1","verdict":"partial",
                  "reasons":["evidence_unavailable"]}}]}}"#
        );
        assert_eq!(
            report.lines(Some((&after, C))),
            [
                "reaffirm-all reaffirmed: #1 (r1) as #5: verified, #3 (r2) as #6: partial \
                 (evidence_unavailable)",
                "reaffirm-all skipped: #2 (why)",
                "reaffirmed 2 (verified 1), skipped 1",
            ]
        );
        assert_eq!(
            report.lines(Some(("not json", C)))[2],
            "reaffirmed 2 (the verified count could not be read back), skipped 1"
        );
        assert_eq!(
            ReaffirmAllReport::default().lines(None)[2],
            "reaffirmed 0 (verified 0), skipped 0"
        );
    }

    /// P4-B 4d (T18 N-a): with --evidence, a fact reaffirmed on runs of
    /// other commands than its Evidence ran is named; the request is the
    /// same.
    #[test]
    fn p4b_with_evidence_a_fact_on_other_commands_is_named() {
        let body = capsule(&[
            (1, "fact", "active", "stale", &[TESTS]),
            (2, "fact", "active", "stale", &[LINT]),
            (3, "fact", "active", "stale", &[LINT, TESTS]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let runs = vec![run("r1", Some(LINT), "PASS")];
        let mut all = draft(
            vec![statement(StatementKind::ReaffirmAll, &["id-r1"])],
            runs,
        );
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(report.reaffirmed.len(), 3);
        assert_eq!(
            report.warnings,
            [
                "reaffirm-all: #1 is reaffirmed on r1, which this context does not record as \
                 runs of the commands its Evidence ran (「sh tests/a.sh」); it is sent as given",
                "reaffirm-all: #3 is reaffirmed on r1, which this context does not record as \
                 runs of the commands its Evidence ran (「sh tests/b.sh」, 「sh tests/a.sh」); it is \
                 sent as given",
            ]
        );
        // An Evidence ID this context did not record is not known to be a
        // run of the command either.
        let mut unknown = draft(
            vec![statement(StatementKind::ReaffirmAll, &["elsewhere"])],
            Vec::new(),
        );
        let report = unknown.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(report.warnings.len(), 3);
    }

    /// P4-B 4e (T18 N-c): the context's runs and an explicit --workspace of
    /// another Workspace: the reason says so.
    #[test]
    fn p4b_runs_of_another_workspace_say_so() {
        let body = capsule(&[(1, "fact", "active", "stale", &[TESTS])]).replace(
            r#""capture_manifest_digest":"M""#,
            r#""capture_manifest_digest":"X""#,
        );
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let mut all = draft(
            vec![statement(StatementKind::ReaffirmAll, &[])],
            vec![run("r1", Some(TESTS), "PASS")],
        );
        if let Draft::Checkpoint(inner) = &mut all {
            inner.workspace_id = "W2".to_owned();
        }
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(
            report.skipped,
            [(
                1,
                vec![
                    "the runs this context holds are of Workspace W, not of the Checkpoint's \
                     Workspace W2; run the commands in that Workspace with `memprove run`"
                        .to_owned()
                ]
            )]
        );
    }

    /// P4-B 4f (P4-A judgment 8, Owner supplement): with nothing to
    /// reaffirm, a --next that differs from the next action in force is
    /// still recorded; the same text is not.
    #[test]
    fn p4b_a_next_that_differs_is_still_recorded() {
        let with_next = |next: Option<&str>| {
            let mut body = capsule(&[(1, "fact", "active", "verified", &[TESTS])]);
            if let Some(next) = next {
                body = body.replace(
                    r#""claims":["#,
                    &format!(
                        r#""claims":[{{"number":2,"claim_id":"{C}/next_action/0","kind":"next_action",
                            "text":"{next}","standing":"active","verdict":"partial","evidence":[]}},
                            {{"number":3,"claim_id":"{C}/next_action/1","kind":"next_action",
                            "text":"old","standing":"superseded","verdict":"partial","evidence":[]}},"#
                    ),
                );
            }
            CapsuleClaims::from_body(&body).unwrap()
        };
        let mut all = draft(vec![statement(StatementKind::ReaffirmAll, &[])], Vec::new());
        all.expand_reaffirm_all(&with_next(Some("n"))).unwrap();
        assert!(!all.records_anything());
        assert!(!all.next_differs(&with_next(Some("n"))));
        assert!(all.next_differs(&with_next(Some("other"))));
        assert!(all.next_differs(&with_next(None)));
        // A superseded next action with the same text is not in force.
        let superseded = with_next(Some("other"));
        let mut old = draft(vec![statement(StatementKind::ReaffirmAll, &[])], Vec::new());
        if let Draft::Checkpoint(inner) = &mut old {
            "old".clone_into(&mut inner.next);
        }
        assert!(old.next_differs(&superseded));
    }

    /// P4-G (ADR 0014 E7.5.6, E7.11.2 item 8): every clause the CLI prints
    /// for a skipped fact maps onto exactly one code of the closed set, and
    /// every code of the set is the code of exactly one clause.
    #[test]
    fn p4g_every_skip_clause_has_one_code_of_the_closed_set() {
        let draft = CheckpointDraft {
            workspace_id: "W".to_owned(),
            task_id: "T".to_owned(),
            session_id: "S".to_owned(),
            snapshot_id: Ok("N".to_owned()),
            objective: "o".to_owned(),
            statements: Vec::new(),
            next: "n".to_owned(),
            next_supersedes: Vec::new(),
            withdrawals: Vec::new(),
            withdrawal_reason: None,
            withdraw_session: None,
            state_runs: vec![run("r1", Some("sh a"), "FAIL")],
            state_manifest: Some("M".to_owned()),
            state_workspace: Some("V".to_owned()),
        };
        let digest = argv_digest(&argv("sh a"));
        let commands = vec![Some((argv("sh a"), RunKind::Test))];
        let every: [ReaffirmSkip<String, usize>; 12] = [
            ReaffirmSkip::State(StateSkip::Unrecorded),
            ReaffirmSkip::State(StateSkip::Unobserved),
            ReaffirmSkip::State(StateSkip::Changed),
            ReaffirmSkip::State(StateSkip::OtherWorkspace),
            ReaffirmSkip::NoEvidence,
            ReaffirmSkip::NotACommand { evidence: 0 },
            ReaffirmSkip::NotRun {
                command: digest.clone(),
            },
            ReaffirmSkip::NotPassed {
                command: digest.clone(),
                latest: 0,
            },
            ReaffirmSkip::NoRunOfKind {
                command: digest.clone(),
                kind: RunKind::Test,
            },
            ReaffirmSkip::NoRunOfKind {
                command: digest.clone(),
                kind: RunKind::Command,
            },
            ReaffirmSkip::KindNotPassed {
                command: digest,
                kind: RunKind::Test,
                latest: 0,
            },
            ReaffirmSkip::NoTest,
        ];
        let mut codes = Vec::new();
        let mut clauses = Vec::new();
        for skip in &every {
            let code = SkipCode::of(skip);
            let clause = reason(skip, &commands, &draft.state_runs, &draft);
            assert!(!clause.is_empty(), "{code:?}");
            codes.push(code);
            clauses.push(clause);
        }
        // One code per clause, and the set is exactly SkipCode::ALL.
        assert_eq!(codes, SkipCode::ALL);
        let mut unique = codes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), SkipCode::ALL.len());
        let mut sentences = clauses.clone();
        sentences.sort();
        sentences.dedup();
        assert_eq!(sentences.len(), clauses.len(), "{clauses:#?}");
        // The codes are plain identifiers, distinct from the warning's.
        let mut names = SkipCode::ALL
            .iter()
            .map(|code| code.as_str())
            .collect::<Vec<_>>();
        names.push(WARNING_OTHER_COMMANDS);
        for name in &names {
            assert!(
                name.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "{name}"
            );
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SkipCode::ALL.len() + 1);
    }

    /// P4-G: the report keeps one code per reason and the number of each
    /// fact a warning names.
    #[test]
    fn p4g_the_report_keeps_codes_and_warned_numbers() {
        let body = capsule(&[
            (1, "fact", "active", "stale", &["sh a"]),
            (2, "fact", "active", "stale", &["sh b"]),
        ]);
        let claims = CapsuleClaims::from_body(&body).unwrap();
        let mut all = draft(
            vec![statement(StatementKind::ReaffirmAll, &[])],
            vec![run("r1", Some("sh a"), "PASS")],
        );
        let report = all.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(report.reaffirmed.len(), 1);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skip_codes, [(2, vec![SkipCode::NoRunInState])]);
        assert_eq!(report.skipped[0].1.len(), report.skip_codes[0].1.len());
        let mut given = draft(
            vec![statement(StatementKind::ReaffirmAll, &["id-r1"])],
            vec![run("r1", Some("sh a"), "PASS")],
        );
        let report = given.expand_reaffirm_all(&claims).unwrap().unwrap();
        assert_eq!(report.warned, [2]);
        assert_eq!(report.warnings.len(), 1);
    }
}
