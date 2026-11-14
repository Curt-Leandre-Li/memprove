//! Reports and the CI gate: what a run did, compared with a baseline.
//!
//! Everything here is a pure function of a [`RunRecord`] and a
//! [`Baseline`]: the same input gives the same bytes.
//!
//! Each task of a run is compared with its baseline entry and gets one
//! [`Change`]. The overall pass rate the gate checks is taken over the
//! tasks both have (neither `new` nor `missing`), in tenths of a percent,
//! rounded down as [`crate::Summary::pass_permille`] rounds.

mod json;
mod markdown;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;

use crate::{Baseline, BaselineTask, MemoryNote, Outcome, RunRecord, TrialResult};

/// The `schema` of a report in [`Format::Json`].
pub const REPORT_SCHEMA: &str = "memprove.eval.report.v1";

/// How a report is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// For people and pull-request comments.
    Markdown,
    /// For programs.
    Json,
}

/// When the gate fails a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatePolicy {
    /// The largest drop of the overall pass rate allowed, in tenths of a
    /// percentage point.
    pub max_drop_permille: u64,
    /// Fail when any task that passed every baseline trial passes none now.
    pub fail_on_regression: bool,
}

impl Default for GatePolicy {
    fn default() -> Self {
        Self {
            max_drop_permille: 0,
            fail_on_regression: true,
        }
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateVerdict {
    /// Whether the run may pass.
    pub passed: bool,
    /// One sentence per reason it failed; empty when it passed.
    pub reasons: Vec<String>,
}

/// How one task of a run compares with its baseline entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// It passed every baseline trial and passes none now.
    Regressed,
    /// It passes a smaller share of its trials than in the baseline.
    Worse,
    /// It passed no baseline trial and passes every trial now.
    Fixed,
    /// It passes a larger share of its trials than in the baseline.
    Better,
    /// It passes the same share of its trials.
    Same,
    /// The baseline does not have it (or ran no trial of it).
    New,
    /// The baseline has it and this run ran no trial of it.
    Missing,
}

impl Change {
    /// Every change, in the order reports count them.
    pub const ALL: [Self; 7] = [
        Self::Regressed,
        Self::Worse,
        Self::Fixed,
        Self::Better,
        Self::Same,
        Self::New,
        Self::Missing,
    ];

    /// The name in reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Regressed => "regressed",
            Self::Worse => "worse",
            Self::Fixed => "fixed",
            Self::Better => "better",
            Self::Same => "same",
            Self::New => "new",
            Self::Missing => "missing",
        }
    }

    /// Whether the task is in both the run and the baseline, so that it
    /// counts in the compared pass rate.
    #[must_use]
    pub const fn is_shared(self) -> bool {
        !matches!(self, Self::New | Self::Missing)
    }

    /// The change of a task that passed `passed` of `trials` trials now,
    /// against its baseline entry `baseline`.
    #[must_use]
    pub fn classify(passed: u32, trials: u32, baseline: Option<&BaselineTask>) -> Self {
        let Some(base) = baseline else {
            return Self::New;
        };
        if trials == 0 {
            return Self::Missing;
        }
        if base.trials == 0 {
            return Self::New;
        }
        if base.passed == base.trials && passed == 0 {
            return Self::Regressed;
        }
        if base.passed == 0 && passed == trials {
            return Self::Fixed;
        }
        let now = u64::from(passed) * u64::from(base.trials);
        let then = u64::from(base.passed) * u64::from(trials);
        match now.cmp(&then) {
            Ordering::Less => Self::Worse,
            Ordering::Greater => Self::Better,
            Ordering::Equal => Self::Same,
        }
    }
}

/// The report of `run`, compared with `baseline` when there is one.
#[must_use]
pub fn render(run: &RunRecord, baseline: Option<&Baseline>, format: Format) -> String {
    let comparison = Comparison::of(run, baseline);
    match format {
        Format::Markdown => markdown::render(&comparison),
        Format::Json => json::render(&comparison),
    }
}

/// The gate's verdict on `run` against `baseline`: a reason for each task
/// that regressed (when `policy` says so), for each baseline task the run
/// did not run, and for a drop of the compared pass rate larger than
/// `policy` allows. New tasks do not count.
#[must_use]
pub fn gate(run: &RunRecord, baseline: &Baseline, policy: &GatePolicy) -> GateVerdict {
    let comparison = Comparison::of(run, Some(baseline));
    let mut reasons = Vec::new();
    for line in &comparison.lines {
        let (Some(change), Some(base)) = (line.change, line.baseline) else {
            continue;
        };
        match change {
            Change::Regressed if policy.fail_on_regression => reasons.push(format!(
                "Task `{}` regressed: it passed {} of {} baseline trials and passes {} of {} now.",
                line.id,
                base.passed,
                base.trials,
                line.passed(),
                line.trials()
            )),
            Change::Missing => reasons.push(format!(
                "Task `{}` is in the baseline but this run did not run it.",
                line.id
            )),
            _ => {}
        }
    }
    if let Some(rates) = comparison.rates
        && let (Some(before), Some(after)) = (rates.before_permille(), rates.after_permille())
        && before.saturating_sub(after) > policy.max_drop_permille
    {
        reasons.push(format!(
            "The pass rate of the {} tasks in both runs fell from {} to {}, {} percentage points; \
             at most {} are allowed.",
            rates.tasks,
            percent(before),
            percent(after),
            tenths(before - after),
            tenths(policy.max_drop_permille)
        ));
    }
    GateVerdict {
        passed: reasons.is_empty(),
        reasons,
    }
}

/// One task of the report: this run's trials of it and its baseline entry.
#[derive(Debug)]
struct Line<'a> {
    id: &'a str,
    title: Option<&'a str>,
    results: Vec<&'a TrialResult>,
    baseline: Option<&'a BaselineTask>,
    /// `None` without a baseline.
    change: Option<Change>,
}

impl Line<'_> {
    fn trials(&self) -> u32 {
        u32::try_from(self.results.len()).unwrap_or(u32::MAX)
    }

    fn passed(&self) -> u32 {
        let passed = self
            .results
            .iter()
            .filter(|result| result.outcome == Outcome::Passed)
            .count();
        u32::try_from(passed).unwrap_or(u32::MAX)
    }

    /// The agent's milliseconds over the trials it ran in; `None` when it
    /// ran in none.
    fn agent_ms(&self) -> Option<u64> {
        self.results
            .iter()
            .filter_map(|result| result.agent.as_ref())
            .map(|agent| agent.duration_ms)
            .reduce(u64::saturating_add)
    }

    /// The cost over the trials whose agent reported one; `None` when none
    /// did.
    fn cost_microusd(&self) -> Option<u64> {
        self.results
            .iter()
            .filter_map(|result| result.agent.as_ref()?.usage.cost_microusd)
            .reduce(u64::saturating_add)
    }

    /// Files, insertions and deletions summed over the trials.
    fn diff_totals(&self) -> (u64, u64, u64) {
        self.results.iter().fold((0, 0, 0), |(f, i, d), result| {
            (
                f.saturating_add(result.diff.files),
                i.saturating_add(result.diff.insertions),
                d.saturating_add(result.diff.deletions),
            )
        })
    }
}

/// Pass counts of the tasks in both the run and the baseline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Rates {
    tasks: u64,
    before_passed: u64,
    before_trials: u64,
    after_passed: u64,
    after_trials: u64,
}

impl Rates {
    const fn before_permille(&self) -> Option<u64> {
        (self.before_passed * 1000).checked_div(self.before_trials)
    }

    const fn after_permille(&self) -> Option<u64> {
        (self.after_passed * 1000).checked_div(self.after_trials)
    }

    /// After minus before, in tenths of a percentage point.
    fn delta_permille(&self) -> Option<i64> {
        let before = i64::try_from(self.before_permille()?).ok()?;
        let after = i64::try_from(self.after_permille()?).ok()?;
        Some(after - before)
    }
}

/// A run and its baseline, task by task.
#[derive(Debug)]
struct Comparison<'a> {
    run: &'a RunRecord,
    baseline: Option<&'a Baseline>,
    /// The run's tasks in run order, then the baseline tasks it did not
    /// run, in baseline order.
    lines: Vec<Line<'a>>,
    /// `None` without a baseline.
    rates: Option<Rates>,
}

impl<'a> Comparison<'a> {
    fn of(run: &'a RunRecord, baseline: Option<&'a Baseline>) -> Self {
        let mut ids: Vec<(&str, Option<&str>)> = run
            .tasks
            .iter()
            .map(|task| (task.id.as_str(), task.title.as_deref()))
            .collect();
        for result in &run.results {
            if !ids.iter().any(|(id, _)| *id == result.task) {
                ids.push((result.task.as_str(), None));
            }
        }
        let mut lines: Vec<Line<'a>> = ids
            .into_iter()
            .map(|(id, title)| {
                let results = run
                    .results
                    .iter()
                    .filter(|result| result.task == id)
                    .collect();
                let entry = baseline.and_then(|baseline| baseline.task(id));
                let mut line = Line {
                    id,
                    title,
                    results,
                    baseline: entry,
                    change: None,
                };
                line.change =
                    baseline.map(|_| Change::classify(line.passed(), line.trials(), entry));
                line
            })
            .collect();
        if let Some(baseline) = baseline {
            for task in &baseline.tasks {
                if !lines.iter().any(|line| line.id == task.id) {
                    lines.push(Line {
                        id: &task.id,
                        title: None,
                        results: Vec::new(),
                        baseline: Some(task),
                        change: Some(Change::Missing),
                    });
                }
            }
        }
        let rates = baseline.map(|_| {
            let mut rates = Rates::default();
            for line in &lines {
                if let (Some(change), Some(base)) = (line.change, line.baseline)
                    && change.is_shared()
                {
                    rates.tasks += 1;
                    rates.before_passed += u64::from(base.passed);
                    rates.before_trials += u64::from(base.trials);
                    rates.after_passed += u64::from(line.passed());
                    rates.after_trials += u64::from(line.trials());
                }
            }
            rates
        });
        Self {
            run,
            baseline,
            lines,
            rates,
        }
    }

    /// How many tasks have `change`.
    fn count(&self, change: Change) -> usize {
        self.lines
            .iter()
            .filter(|line| line.change == Some(change))
            .count()
    }

    /// The trials that did not pass, in run order.
    fn failures(&self) -> impl Iterator<Item = &'a TrialResult> + use<'a> {
        self.run
            .results
            .iter()
            .filter(|result| result.outcome != Outcome::Passed)
    }

    /// The memory notes a report lists: none when every trial passed;
    /// otherwise each note memprove does not hold as verified. A fact is
    /// listed for any verdict but `verified`; another kind of statement
    /// (an open item, a decision, a next action), which memprove never
    /// verifies unless it cites Evidence, only when its Evidence is
    /// `stale` or `tampered`.
    fn listed_memory(&self) -> Vec<&'a MemoryNote> {
        if self.failures().next().is_none() {
            return Vec::new();
        }
        self.run
            .memory
            .iter()
            .filter(|note| no_longer_verified(note))
            .collect()
    }
}

/// Whether a report lists `note` as memory memprove no longer holds as
/// verified (see [`Comparison::listed_memory`]).
fn no_longer_verified(note: &MemoryNote) -> bool {
    match note.verdict.as_str() {
        "verified" => false,
        "stale" | "tampered" => true,
        _ => note.kind == "fact",
    }
}

/// `875` as `87.5%`.
fn percent(permille: u64) -> String {
    format!("{}%", tenths(permille))
}

/// `875` as `87.5`.
fn tenths(value: u64) -> String {
    format!("{}.{}", value / 10, value % 10)
}

/// `-125` as `-12.5 pp`, `0` as `0.0 pp`.
fn points(delta: i64) -> String {
    let sign = match delta.cmp(&0) {
        Ordering::Less => "-",
        Ordering::Greater => "+",
        Ordering::Equal => "",
    };
    format!("{sign}{} pp", tenths(delta.unsigned_abs()))
}
