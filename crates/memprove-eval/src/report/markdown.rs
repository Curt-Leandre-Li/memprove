//! The Markdown report: for people, a pull-request comment, or
//! `$GITHUB_STEP_SUMMARY`.

use std::fmt::Write as _;

use super::{Change, Comparison, Line, percent, points};
use crate::{DiffStat, MemoryNote, TrialResult};

/// Statement texts longer than this many characters are cut.
const TEXT_LIMIT: usize = 200;

pub(super) fn render(comparison: &Comparison<'_>) -> String {
    let mut text = String::from("## memprove eval\n\n");
    text.push_str(&overview(comparison));
    text.push('\n');
    if let Some(line) = against_baseline(comparison) {
        text.push('\n');
        text.push_str(&line);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&table(comparison));
    let failures = comparison.failures().collect::<Vec<_>>();
    if !failures.is_empty() {
        text.push_str("\n### Trials that did not pass\n\n");
        for result in failures {
            text.push_str(&failure(result));
            text.push('\n');
        }
    }
    let memory = comparison.listed_memory();
    if !memory.is_empty() {
        text.push_str(
            "\n### Memory the agent was given that memprove no longer holds as verified\n\n\
             | Statement | Kind | Verdict | Text | Commit |\n|---|---|---|---|---|\n",
        );
        for note in memory {
            text.push_str(&memory_row(note));
            text.push('\n');
        }
    }
    text
}

fn overview(comparison: &Comparison<'_>) -> String {
    let run = comparison.run;
    let summary = run.summary();
    let rate = summary
        .pass_permille()
        .map_or_else(|| "n/a".to_owned(), percent);
    let model = run.agent.model.as_deref().map_or_else(
        || "default model".to_owned(),
        |model| format!("model {}", md(model)),
    );
    let errors = match summary.errors {
        0 => String::new(),
        1 => " (1 infrastructure error)".to_owned(),
        count => format!(" ({count} infrastructure errors)"),
    };
    let cost = summary
        .cost_microusd
        .map_or_else(|| "n/a".to_owned(), dollars);
    format!(
        "**{}/{} trials passed ({rate})**{errors} · run {} · agent {}, {model} · HEAD {} · \
         cost {cost} · tokens {} in / {} out · agent time {}",
        summary.passed,
        summary.trials,
        code(&run.run_id),
        run.agent.kind.name(),
        code(short(&run.repository.head)),
        thousands(summary.input_tokens),
        thousands(summary.output_tokens),
        duration(summary.agent_ms),
    )
}

fn against_baseline(comparison: &Comparison<'_>) -> Option<String> {
    let baseline = comparison.baseline?;
    let rates = comparison.rates.unwrap_or_default();
    let model = baseline.model.as_deref().map_or_else(
        || "default model".to_owned(),
        |model| format!("model {}", md(model)),
    );
    let mut line = format!(
        "Compared with baseline {} ({}, {model}): ",
        code(&baseline.run_id),
        md(&baseline.agent)
    );
    match (
        rates.before_permille(),
        rates.after_permille(),
        rates.delta_permille(),
    ) {
        (Some(before), Some(after), Some(delta)) => {
            let tasks = if rates.tasks == 1 { "task" } else { "tasks" };
            let _ = write!(
                line,
                "**{} → {} ({})** on the {} {tasks} in both · {} regressed · {} fixed",
                percent(before),
                percent(after),
                points(delta),
                rates.tasks,
                comparison.count(Change::Regressed),
                comparison.count(Change::Fixed),
            );
        }
        _ => line.push_str("no task is in both runs"),
    }
    for change in [Change::New, Change::Missing] {
        let count = comparison.count(change);
        if count > 0 {
            let _ = write!(line, " · {count} {}", change.name());
        }
    }
    Some(line)
}

fn table(comparison: &Comparison<'_>) -> String {
    if comparison.lines.is_empty() {
        return "No tasks ran.\n".to_owned();
    }
    let compared = comparison.baseline.is_some();
    let mut text = if compared {
        "| Task | This run | Baseline | Change | Agent time | Cost | Diff |\n\
         |---|---|---|---|---|---|---|\n"
    } else {
        "| Task | This run | Agent time | Cost | Diff |\n|---|---|---|---|---|\n"
    }
    .to_owned();
    for line in &comparison.lines {
        let mut cells = vec![task_cell(line), this_run(line)];
        if compared {
            cells.push(line.baseline.filter(|base| base.trials > 0).map_or_else(
                || "–".to_owned(),
                |base| format!("{}/{}", base.passed, base.trials),
            ));
            cells.push(line.change.map_or_else(String::new, |change| match change {
                Change::Regressed | Change::Missing => format!("**{}**", change.name()),
                other => other.name().to_owned(),
            }));
        }
        let ran = !line.results.is_empty();
        cells.push(line.agent_ms().map_or_else(|| "–".to_owned(), duration));
        cells.push(match line.cost_microusd() {
            Some(cost) => dollars(cost),
            None if ran => "n/a".to_owned(),
            None => "–".to_owned(),
        });
        cells.push(if ran {
            line.results
                .iter()
                .map(|result| diff(&result.diff))
                .collect::<Vec<_>>()
                .join("; ")
        } else {
            "–".to_owned()
        });
        text.push_str("| ");
        text.push_str(&cells.join(" | "));
        text.push_str(" |\n");
    }
    text
}

fn task_cell(line: &Line<'_>) -> String {
    match line.title {
        Some(title) => format!("{}: {}", code(line.id), md(&clip(title))),
        None => code(line.id),
    }
}

fn this_run(line: &Line<'_>) -> String {
    if line.results.is_empty() {
        return "not run".to_owned();
    }
    let outcomes = line
        .results
        .iter()
        .map(|result| result.outcome.name())
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}/{}: {outcomes}", line.passed(), line.trials())
}

fn failure(result: &TrialResult) -> String {
    let reason = result
        .reason
        .as_deref()
        .map_or_else(|| "no reason was recorded".to_owned(), md);
    format!(
        "- {} trial {}: **{}**: {reason} (logs: {})",
        code(&result.task),
        result.trial,
        result.outcome.name(),
        code(&result.logs)
    )
}

fn memory_row(note: &MemoryNote) -> String {
    format!(
        "| {} | {} | {} | {} | {} |",
        md(&note.label),
        md(&note.kind),
        md(&note.verdict),
        md(&clip(&note.text)),
        note.commit
            .as_deref()
            .map_or_else(|| "–".to_owned(), |commit| code(short(commit)))
    )
}

fn diff(diff: &DiffStat) -> String {
    if diff.files == 0 && diff.insertions == 0 && diff.deletions == 0 {
        return "none".to_owned();
    }
    let files = if diff.files == 1 { "file" } else { "files" };
    format!(
        "+{} -{} in {} {files}",
        diff.insertions, diff.deletions, diff.files
    )
}

/// `412000` millionths of a dollar as `$0.41`, rounded to the cent; a cost
/// under half a cent as `<$0.01`.
pub(super) fn dollars(microusd: u64) -> String {
    let cents = microusd.saturating_add(5_000) / 10_000;
    if cents == 0 && microusd > 0 {
        return "<$0.01".to_owned();
    }
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// `1234567` as `1,234,567`.
pub(super) fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// Milliseconds as `4.2s`, `2m 05s`, or `1h 03m`.
pub(super) fn duration(ms: u64) -> String {
    if ms < 60_000 {
        return format!("{}.{}s", ms / 1000, ms % 1000 / 100);
    }
    let seconds = ms / 1000;
    if seconds < 3600 {
        return format!("{}m {:02}s", seconds / 60, seconds % 60);
    }
    let minutes = seconds / 60;
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}

/// The first seven characters of a commit.
fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// `text` cut to [`TEXT_LIMIT`] characters, with `…` when cut.
fn clip(text: &str) -> String {
    match text.char_indices().nth(TEXT_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// `text` as a code span, or escaped text when a code span cannot hold it
/// inside a table cell.
fn code(text: &str) -> String {
    if text.is_empty() || text.chars().any(|c| c == '`' || c == '|' || c.is_control()) {
        md(text)
    } else {
        format!("`{text}`")
    }
}

/// `text` as literal Markdown on one line, safe in a table cell and in a
/// pull-request comment: line breaks become spaces, Markdown punctuation is
/// escaped, and `<`, `>`, `&` are entities, so no HTML gets through.
pub(super) fn md(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' | '`' | '*' | '_' | '[' | ']' | '|' | '~' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            control if control.is_control() => escaped.push(' '),
            other => escaped.push(other),
        }
    }
    escaped
}
