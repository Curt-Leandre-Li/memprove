use super::{
    markdown::{dollars, duration, md, thousands},
    *,
};
use crate::{
    AgentEnd, AgentOutcome, DiffStat, MemoryNote, TaskRef, Usage,
    json::{JsonValue, parse},
    model::fixtures::sample_run,
};

fn base(id: &str, passed: u32, trials: u32) -> BaselineTask {
    BaselineTask {
        id: id.to_owned(),
        passed,
        trials,
    }
}

fn baseline(tasks: Vec<BaselineTask>) -> Baseline {
    Baseline {
        run_id: "20261101T080000Z-000000".to_owned(),
        agent: "claude-code".to_owned(),
        model: Some("claude-sonnet-5-5".to_owned()),
        tasks,
    }
}

/// A trial of `task` with `outcome`, the agent having run for a second at
/// a cost of `cost`.
fn trial(task: &str, number: u32, outcome: Outcome, cost: Option<u64>) -> TrialResult {
    TrialResult {
        task: task.to_owned(),
        trial: number,
        base: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        outcome,
        reason: (outcome != Outcome::Passed).then(|| format!("{} happened", outcome.name())),
        agent: Some(AgentOutcome {
            end: AgentEnd::Exited(0),
            duration_ms: 1_000,
            usage: Usage {
                cost_microusd: cost,
                ..Usage::default()
            },
        }),
        check_exit: Some(i32::from(outcome != Outcome::Passed)),
        check_ms: 100,
        diff: DiffStat::default(),
        logs: format!("trials/{task}/{number}"),
    }
}

/// A run of the tasks `tasks`, each with the outcomes given, in order.
fn run_of(tasks: &[(&str, &[Outcome])]) -> RunRecord {
    let mut run = sample_run();
    run.memory.clear();
    run.trials_per_task = 1;
    run.tasks = tasks
        .iter()
        .map(|(id, _)| TaskRef {
            id: (*id).to_owned(),
            title: None,
            source: format!("evals/{id}.json"),
            sha256: "00".repeat(32),
        })
        .collect();
    run.results = tasks
        .iter()
        .flat_map(|(id, outcomes)| {
            outcomes
                .iter()
                .zip(1..)
                .map(|(outcome, number)| trial(id, number, *outcome, Some(10_000)))
        })
        .collect();
    run
}

use Outcome::{CheckTimeout, Error, Failed, Passed, PatchFailed, SetupFailed};

#[test]
fn classify_names_every_change() {
    let entry = |passed, trials| Some(base("t", passed, trials));
    assert_eq!(Change::classify(1, 1, None), Change::New);
    assert_eq!(Change::classify(1, 1, entry(0, 0).as_ref()), Change::New);
    assert_eq!(
        Change::classify(0, 0, entry(1, 1).as_ref()),
        Change::Missing
    );
    assert_eq!(
        Change::classify(0, 2, entry(3, 3).as_ref()),
        Change::Regressed
    );
    assert_eq!(Change::classify(1, 2, entry(3, 3).as_ref()), Change::Worse);
    assert_eq!(Change::classify(1, 3, entry(2, 3).as_ref()), Change::Worse);
    assert_eq!(Change::classify(2, 2, entry(0, 3).as_ref()), Change::Fixed);
    assert_eq!(Change::classify(1, 2, entry(0, 3).as_ref()), Change::Better);
    assert_eq!(Change::classify(2, 3, entry(1, 3).as_ref()), Change::Better);
    assert_eq!(Change::classify(1, 2, entry(2, 4).as_ref()), Change::Same);
    assert_eq!(Change::classify(0, 1, entry(0, 1).as_ref()), Change::Same);
    assert_eq!(Change::classify(3, 3, entry(1, 1).as_ref()), Change::Same);
    let names = Change::ALL.map(Change::name);
    assert_eq!(
        names,
        [
            "regressed",
            "worse",
            "fixed",
            "better",
            "same",
            "new",
            "missing"
        ]
    );
}

#[test]
fn the_report_classifies_new_and_missing_tasks() {
    let run = run_of(&[("kept", &[Passed]), ("added", &[Failed])]);
    let baseline = baseline(vec![base("kept", 1, 1), base("dropped", 1, 1)]);
    let comparison = Comparison::of(&run, Some(&baseline));
    let changes = comparison
        .lines
        .iter()
        .map(|line| (line.id, line.change))
        .collect::<Vec<_>>();
    assert_eq!(
        changes,
        [
            ("kept", Some(Change::Same)),
            ("added", Some(Change::New)),
            ("dropped", Some(Change::Missing)),
        ]
    );
    let rates = comparison.rates.expect("rates");
    assert_eq!(
        (rates.tasks, rates.before_permille(), rates.after_permille()),
        (1, Some(1000), Some(1000))
    );
}

#[test]
fn the_gate_passes_an_equal_run() {
    let run = run_of(&[("a", &[Passed]), ("b", &[Failed])]);
    let verdict = gate(&run, &Baseline::from_run(&run), &GatePolicy::default());
    assert_eq!(
        verdict,
        GateVerdict {
            passed: true,
            reasons: Vec::new(),
        }
    );
}

#[test]
fn the_gate_fails_a_regression_and_the_drop_it_causes() {
    let run = run_of(&[("a", &[Failed]), ("b", &[Passed])]);
    let baseline = baseline(vec![base("a", 1, 1), base("b", 1, 1)]);
    let verdict = gate(&run, &baseline, &GatePolicy::default());
    assert!(!verdict.passed);
    assert_eq!(
        verdict.reasons,
        [
            "Task `a` regressed: it passed 1 of 1 baseline trials and passes 0 of 1 now.",
            "The pass rate of the 2 tasks in both runs fell from 100.0% to 50.0%, 50.0 \
             percentage points; at most 0.0 are allowed.",
        ]
    );
}

#[test]
fn the_gate_allows_regressions_and_drops_when_told_to() {
    let run = run_of(&[("a", &[Failed]), ("b", &[Passed])]);
    let baseline = baseline(vec![base("a", 1, 1), base("b", 1, 1)]);
    let policy = GatePolicy {
        max_drop_permille: 500,
        fail_on_regression: false,
    };
    assert!(gate(&run, &baseline, &policy).passed);
    let policy = GatePolicy {
        max_drop_permille: 499,
        fail_on_regression: false,
    };
    let verdict = gate(&run, &baseline, &policy);
    assert_eq!(verdict.reasons.len(), 1);
    assert!(verdict.reasons[0].ends_with("at most 49.9 are allowed."));
}

#[test]
fn the_gate_fails_a_worse_task_only_by_the_drop() {
    let run = run_of(&[("a", &[Passed, Failed])]);
    let baseline = baseline(vec![base("a", 2, 2)]);
    let strict = gate(&run, &baseline, &GatePolicy::default());
    assert_eq!(strict.reasons.len(), 1);
    assert!(strict.reasons[0].starts_with("The pass rate of the 1 tasks"));
    let lenient = GatePolicy {
        max_drop_permille: 500,
        ..GatePolicy::default()
    };
    assert!(gate(&run, &baseline, &lenient).passed);
}

#[test]
fn the_gate_fails_a_missing_task_and_ignores_a_new_one() {
    let run = run_of(&[("a", &[Passed]), ("new", &[Failed])]);
    let baseline = baseline(vec![base("a", 1, 1), base("gone", 0, 1)]);
    let verdict = gate(&run, &baseline, &GatePolicy::default());
    assert_eq!(
        verdict.reasons,
        ["Task `gone` is in the baseline but this run did not run it."]
    );
}

#[test]
fn the_gate_counts_a_better_run_as_passing() {
    let run = run_of(&[("a", &[Passed]), ("b", &[Passed])]);
    let baseline = baseline(vec![base("a", 0, 1), base("b", 1, 2)]);
    assert!(gate(&run, &baseline, &GatePolicy::default()).passed);
}

#[test]
fn the_gate_on_a_run_of_no_trials_names_every_baseline_task() {
    let run = run_of(&[]);
    let baseline = baseline(vec![base("a", 1, 1)]);
    let verdict = gate(&run, &baseline, &GatePolicy::default());
    assert_eq!(
        verdict.reasons,
        ["Task `a` is in the baseline but this run did not run it."]
    );
    assert!(gate(&run, &Baseline::from_run(&run), &GatePolicy::default()).passed);
}

const SAMPLE_MARKDOWN: &str = "## memprove eval

**1/2 trials passed (50.0%)** · run `20261114T221323Z-a1b2c3` · agent claude-code, model claude-sonnet-5-5 · HEAD `0123456` · cost $0.41 · tokens 2,400 in / 680 out · agent time 2m 02s

| Task | This run | Agent time | Cost | Diff |
|---|---|---|---|---|
| `empty-input`: parser accepts empty input | 1/1: passed | 1m 01s | $0.41 | +14 -3 in 2 files |
| `truncated-header` | 0/1: failed | 1m 01s | n/a | none |

### Trials that did not pass

- `truncated-header` trial 1: **failed**: the check exited with code 101 (logs: `trials/truncated-header/1`)

### Statements memprove held that it no longer judges verified

Read from memprove when the run started. The agent did not get them: a trial worktree has no memprove context.

| Statement | Kind | Verdict | Text | Commit |
|---|---|---|---|---|
| #4 | fact | stale | the parser rejects a truncated header | `89abcde` |
";

#[test]
fn markdown_without_a_baseline() {
    assert_eq!(
        render(&sample_run(), None, Format::Markdown),
        SAMPLE_MARKDOWN
    );
}

#[test]
fn markdown_with_a_baseline() {
    let run = sample_run();
    let baseline = baseline(vec![
        base("empty-input", 1, 1),
        base("truncated-header", 1, 1),
        base("gone", 1, 2),
    ]);
    let text = render(&run, Some(&baseline), Format::Markdown);
    let expected = "Compared with baseline `20261101T080000Z-000000` (claude-code, model \
                    claude-sonnet-5-5): **100.0% → 50.0% (-50.0 pp)** on the 2 tasks in both · \
                    1 regressed · 0 fixed · 1 missing\n";
    assert!(text.contains(expected), "{text}");
    assert!(text.contains(
        "| Task | This run | Baseline | Change | Agent time | Cost | Diff |\n\
         |---|---|---|---|---|---|---|\n\
         | `empty-input`: parser accepts empty input | 1/1: passed | 1/1 | same | 1m 01s | $0.41 | +14 -3 in 2 files |\n\
         | `truncated-header` | 0/1: failed | 1/1 | **regressed** | 1m 01s | n/a | none |\n\
         | `gone` | not run | 1/2 | **missing** | – | – | – |\n"
    ));
}

#[test]
fn markdown_says_when_no_task_is_shared() {
    let run = run_of(&[("a", &[Passed])]);
    let text = render(
        &run,
        Some(&baseline(vec![base("b", 1, 1)])),
        Format::Markdown,
    );
    assert!(text.contains(
        "(claude-code, model claude-sonnet-5-5): no task is in both runs · 1 new · 1 missing\n"
    ));
    assert!(text.contains("| `a` | 1/1: passed | – | new |"));
}

#[test]
fn markdown_of_a_run_of_no_trials() {
    let mut run = run_of(&[]);
    run.agent.model = None;
    let text = render(&run, None, Format::Markdown);
    assert!(text.contains(
        "**0/0 trials passed (n/a)** · run `20261114T221323Z-a1b2c3` · agent claude-code, \
         default model · HEAD `0123456` · cost n/a · tokens 0 in / 0 out · agent time 0.0s\n"
    ));
    assert!(text.ends_with("\nNo tasks ran.\n"));
    assert!(!text.contains("Trials that did not pass"));
}

#[test]
fn markdown_without_cost_or_agent() {
    let mut run = run_of(&[("a", &[SetupFailed]), ("b", &[Passed])]);
    run.results[0].agent = None;
    run.results[1]
        .agent
        .as_mut()
        .expect("agent")
        .usage
        .cost_microusd = None;
    let text = render(&run, None, Format::Markdown);
    assert!(text.contains("cost n/a"));
    assert!(text.contains("| `a` | 0/1: setup_failed | – | n/a | none |"));
    assert!(text.contains("| `b` | 1/1: passed | 1.0s | n/a | none |"));
}

#[test]
fn markdown_lists_every_outcome_and_counts_errors() {
    let run = run_of(&[(
        "a",
        &[
            Passed,
            Failed,
            CheckTimeout,
            SetupFailed,
            PatchFailed,
            Error,
        ],
    )]);
    let text = render(&run, None, Format::Markdown);
    assert!(text.contains("**1/6 trials passed (16.6%)** (1 infrastructure error) ·"));
    assert!(text.contains(
        "| `a` | 1/6: passed, failed, check_timeout, setup_failed, patch_failed, error |"
    ));
    for (number, outcome) in [(2, "failed"), (3, "check_timeout"), (6, "error")] {
        assert!(text.contains(&format!(
            "- `a` trial {number}: **{outcome}**: {} happened (logs: `trials/a/{number}`)\n",
            md(outcome)
        )));
    }
}

#[test]
fn memory_is_listed_only_on_failure_and_only_when_not_verified() {
    let note = |label: &str, kind: &str, verdict: &str| MemoryNote {
        label: label.to_owned(),
        kind: kind.to_owned(),
        verdict: verdict.to_owned(),
        text: format!("statement {label}"),
        commit: None,
    };
    let mut run = run_of(&[("a", &[Passed])]);
    run.memory = vec![
        note("#1", "fact", "verified"),
        note("#2", "fact", "stale"),
        note("#3", "fact", "unconfirmed"),
        note("#4", "fact", "tampered"),
        note("#5", "decision", "unconfirmed"),
        note("#6", "decision", "stale"),
        note("#7", "open", "unconfirmed"),
        note("#8", "next", "verified"),
    ];
    let passing = render(&run, None, Format::Markdown);
    assert!(!passing.contains("Statements memprove held"));
    run.results[0].outcome = Failed;
    let failing = render(&run, None, Format::Markdown);
    let listed = failing
        .lines()
        .filter(|line| line.starts_with("| #"))
        .map(|line| line.split(" | ").next().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(listed, ["| #2", "| #3", "| #4", "| #6"]);
    assert!(failing.contains("| #2 | fact | stale | statement #2 | – |"));
    run.memory.truncate(1);
    assert!(!render(&run, None, Format::Markdown).contains("Statements memprove held"));
}

#[test]
fn markdown_escapes_what_the_run_did_not_write() {
    let mut run = sample_run();
    run.results[1].reason = Some("<script>|a|\n`b` *c* [d](e) & ~f~".to_owned());
    run.memory[0].text = "x".repeat(250);
    run.tasks[0].title = Some("pipes | in | title".to_owned());
    let text = render(&run, None, Format::Markdown);
    assert!(text.contains(
        "**failed**: &lt;script&gt;\\|a\\| \\`b\\` \\*c\\* \\[d\\](e) &amp; \\~f\\~ (logs:"
    ));
    assert!(text.contains(&format!("| {}… |", "x".repeat(200))));
    assert!(text.contains("`empty-input`: pipes \\| in \\| title |"));
    assert_eq!(md("a_b"), "a\\_b");
}

#[test]
fn numbers_are_formatted_for_people() {
    assert_eq!(dollars(412_000), "$0.41");
    assert_eq!(dollars(415_000), "$0.42");
    assert_eq!(dollars(12_345_678), "$12.35");
    assert_eq!(dollars(0), "$0.00");
    assert_eq!(dollars(4_999), "<$0.01");
    assert_eq!(dollars(5_000), "$0.01");
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000), "1,000");
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(duration(0), "0.0s");
    assert_eq!(duration(4_250), "4.2s");
    assert_eq!(duration(59_999), "59.9s");
    assert_eq!(duration(60_000), "1m 00s");
    assert_eq!(duration(3_599_999), "59m 59s");
    assert_eq!(duration(3_780_000), "1h 03m");
    assert_eq!(percent(875), "87.5%");
    assert_eq!(points(-125), "-12.5 pp");
    assert_eq!(points(0), "0.0 pp");
    assert_eq!(points(5), "+0.5 pp");
}

#[test]
fn json_has_the_same_content_and_the_gate_numbers() {
    let run = sample_run();
    let baseline = baseline(vec![
        base("empty-input", 1, 1),
        base("truncated-header", 1, 1),
        base("gone", 1, 2),
    ]);
    let text = render(&run, Some(&baseline), Format::Json);
    assert_eq!(text, render(&run, Some(&baseline), Format::Json));
    let value = parse(&text, None).expect("strict JSON");
    let get = |path: &[&str]| {
        path.iter()
            .try_fold(&value, |value, name| value.get(name))
            .cloned()
    };
    let int = |path: &[&str]| get(path).and_then(|value| value.as_i64());
    let text_at = |path: &[&str]| get(path).and_then(|value| value.as_str().map(str::to_owned));
    assert_eq!(text_at(&["schema"]).as_deref(), Some(REPORT_SCHEMA));
    assert_eq!(int(&["summary", "pass_permille"]), Some(500));
    assert_eq!(int(&["baseline", "shared_tasks"]), Some(2));
    assert_eq!(int(&["baseline", "before", "permille"]), Some(1000));
    assert_eq!(int(&["baseline", "after", "permille"]), Some(500));
    assert_eq!(int(&["baseline", "delta_permille"]), Some(-500));
    assert_eq!(int(&["baseline", "regressed"]), Some(1));
    assert_eq!(int(&["baseline", "missing"]), Some(1));
    assert_eq!(int(&["baseline", "new"]), Some(0));
    let tasks = get(&["tasks"]).expect("tasks");
    let tasks = tasks.as_array().expect("array");
    assert_eq!(tasks.len(), 3);
    assert_eq!(
        tasks[1].get("change").and_then(|value| value.as_str()),
        Some("regressed")
    );
    assert!(
        tasks[2]
            .get("agent_ms")
            .is_some_and(|value| matches!(value, JsonValue::Null))
    );
    let failures = get(&["failures"]).expect("failures");
    assert_eq!(failures.as_array().map(<[_]>::len), Some(1));
    let memory = get(&["memory_not_verified"]).expect("memory");
    assert_eq!(
        memory
            .as_array()
            .and_then(|notes| notes[0].get("verdict"))
            .and_then(|value| value.as_str()),
        Some("stale")
    );
}

#[test]
fn json_without_a_baseline_or_trials() {
    let text = render(&run_of(&[]), None, Format::Json);
    let value = parse(&text, None).expect("strict JSON");
    assert!(
        value
            .get("baseline")
            .is_some_and(|value| matches!(value, JsonValue::Null))
    );
    assert!(
        value
            .get("summary")
            .and_then(|summary| summary.get("pass_permille"))
            .is_some_and(|value| matches!(value, JsonValue::Null))
    );
    assert!(text.contains(
        "\"tasks\": [],\n  \"failures\": [],\n  \"memory_not_verified\": [],\n  \"memory_unread\": null\n}\n"
    ));
}

#[test]
fn the_reason_memory_was_not_read_is_shown_with_failures() {
    let mut run = run_of(&[("a", &[Passed])]);
    run.memory_unread = Some("no memprove context for /work/repo".to_owned());
    let line = "\nmemprove's memory was not read: no memprove context for /work/repo\n";
    assert!(!render(&run, None, Format::Markdown).contains(line));
    run.results[0].outcome = Failed;
    assert!(render(&run, None, Format::Markdown).ends_with(line));
    let value = parse(&render(&run, None, Format::Json), None).expect("strict JSON");
    assert_eq!(
        value.get("memory_unread").and_then(|value| value.as_str()),
        Some("no memprove context for /work/repo")
    );
}
