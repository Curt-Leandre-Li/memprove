//! The JSON report (`memprove.eval.report.v1`): the Markdown report's
//! content for programs, with the numbers the gate uses.
//!
//! Members, in order: `schema`, `run_id`, `started_at`, `finished_at`,
//! `head`, `agent` (`kind`, `model`, `version`), `summary` (as in
//! `run.json`), `baseline` (`null` without one; else its `run_id`,
//! `agent`, `model`, the compared pass counts `shared_tasks`, `before`,
//! `after` and `delta_permille`, and how many tasks have each change),
//! `tasks` (one per report row: `id`, `title`, `passed`, `trials`,
//! `outcomes`, `baseline`, `change`, `agent_ms`, `cost_microusd`, `diff`),
//! `failures` (each trial that did not pass: `task`, `trial`, `outcome`,
//! `reason`, `logs`), `memory_not_verified` (the statements the Markdown
//! report lists, empty when it lists none), and `memory_unread` (the
//! run's sentence for why memprove's statements were not read, or `null`).

use super::{Change, Comparison, Line, REPORT_SCHEMA};
use crate::{MemoryNote, TrialResult, json::Out};

pub(super) fn render(comparison: &Comparison<'_>) -> String {
    let run = comparison.run;
    let summary = run.summary();
    Out::Object(vec![
        ("schema", Out::str(REPORT_SCHEMA)),
        ("run_id", Out::str(&run.run_id)),
        ("started_at", Out::str(&run.started_at)),
        ("finished_at", Out::str(&run.finished_at)),
        ("head", Out::str(&run.repository.head)),
        (
            "agent",
            Out::Object(vec![
                ("kind", Out::str(run.agent.kind.name())),
                ("model", Out::opt(run.agent.model.as_deref(), Out::str)),
                ("version", Out::opt(run.agent_version.as_deref(), Out::str)),
            ]),
        ),
        (
            "summary",
            Out::Object(vec![
                ("tasks", Out::uint(summary.tasks)),
                ("trials", Out::uint(summary.trials)),
                ("passed", Out::uint(summary.passed)),
                ("errors", Out::uint(summary.errors)),
                (
                    "pass_permille",
                    Out::opt(summary.pass_permille(), Out::uint),
                ),
                ("cost_microusd", Out::opt(summary.cost_microusd, Out::uint)),
                ("input_tokens", Out::uint(summary.input_tokens)),
                ("output_tokens", Out::uint(summary.output_tokens)),
                ("agent_ms", Out::uint(summary.agent_ms)),
                ("check_ms", Out::uint(summary.check_ms)),
            ]),
        ),
        ("baseline", baseline_out(comparison)),
        (
            "tasks",
            Out::Array(comparison.lines.iter().map(line_out).collect()),
        ),
        (
            "failures",
            Out::Array(comparison.failures().map(failure_out).collect()),
        ),
        (
            "memory_not_verified",
            Out::Array(
                comparison
                    .listed_memory()
                    .into_iter()
                    .map(memory_out)
                    .collect(),
            ),
        ),
        (
            "memory_unread",
            Out::opt(run.memory_unread.as_deref(), Out::str),
        ),
    ])
    .render()
}

fn baseline_out(comparison: &Comparison<'_>) -> Out {
    let (Some(baseline), Some(rates)) = (comparison.baseline, comparison.rates) else {
        return Out::null();
    };
    let counts = |passed, trials, permille: Option<u64>| {
        Out::Object(vec![
            ("passed", Out::uint(passed)),
            ("trials", Out::uint(trials)),
            ("permille", Out::opt(permille, Out::uint)),
        ])
    };
    let mut members = vec![
        ("run_id", Out::str(&baseline.run_id)),
        ("agent", Out::str(&baseline.agent)),
        ("model", Out::opt(baseline.model.as_deref(), Out::str)),
        ("shared_tasks", Out::uint(rates.tasks)),
        (
            "before",
            counts(
                rates.before_passed,
                rates.before_trials,
                rates.before_permille(),
            ),
        ),
        (
            "after",
            counts(
                rates.after_passed,
                rates.after_trials,
                rates.after_permille(),
            ),
        ),
        ("delta_permille", Out::opt(rates.delta_permille(), Out::int)),
    ];
    for change in Change::ALL {
        let count = u64::try_from(comparison.count(change)).unwrap_or(u64::MAX);
        members.push((change.name(), Out::uint(count)));
    }
    Out::Object(members)
}

fn line_out(line: &Line<'_>) -> Out {
    let (files, insertions, deletions) = line.diff_totals();
    Out::Object(vec![
        ("id", Out::str(line.id)),
        ("title", Out::opt(line.title, Out::str)),
        ("passed", Out::uint(u64::from(line.passed()))),
        ("trials", Out::uint(u64::from(line.trials()))),
        (
            "outcomes",
            Out::Array(
                line.results
                    .iter()
                    .map(|result| Out::str(result.outcome.name()))
                    .collect(),
            ),
        ),
        (
            "baseline",
            Out::opt(line.baseline, |base| {
                Out::Object(vec![
                    ("passed", Out::uint(u64::from(base.passed))),
                    ("trials", Out::uint(u64::from(base.trials))),
                ])
            }),
        ),
        (
            "change",
            Out::opt(line.change, |change| Out::str(change.name())),
        ),
        ("agent_ms", Out::opt(line.agent_ms(), Out::uint)),
        ("cost_microusd", Out::opt(line.cost_microusd(), Out::uint)),
        (
            "diff",
            Out::Object(vec![
                ("files", Out::uint(files)),
                ("insertions", Out::uint(insertions)),
                ("deletions", Out::uint(deletions)),
            ]),
        ),
    ])
}

fn failure_out(result: &TrialResult) -> Out {
    Out::Object(vec![
        ("task", Out::str(&result.task)),
        ("trial", Out::uint(u64::from(result.trial))),
        ("outcome", Out::str(result.outcome.name())),
        ("reason", Out::opt(result.reason.as_deref(), Out::str)),
        ("logs", Out::str(&result.logs)),
    ])
}

fn memory_out(note: &MemoryNote) -> Out {
    Out::Object(vec![
        ("label", Out::str(&note.label)),
        ("kind", Out::str(&note.kind)),
        ("verdict", Out::str(&note.verdict)),
        ("text", Out::str(&note.text)),
        ("commit", Out::opt(note.commit.as_deref(), Out::str)),
    ])
}
