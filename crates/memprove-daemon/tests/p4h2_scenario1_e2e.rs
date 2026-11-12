//! P4-H2: acceptance scenario 1 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > A Task has more than 60 statements, about seven tenths of them superseded.
//! > A brand-new agent session starts without pasting any continuation prompt;
//! > its context already holds a Context Pack no longer than the budget.
//!
//! A Task gets 64 statements in 16 Checkpoints, 45 of them superseded
//! (70 %); its Session ends. A fresh client session then starts the way
//! Claude Code starts one: `memprove hook session-start` with only the
//! `SessionStart` JSON on standard input (source `startup`, a client
//! session no context names) and no prompt from anyone. What the hook
//! prints, all of which enters the agent's context, holds the Context Pack
//! of that Task in a fence; the whole output is at most 10,000 UTF-16 code
//! units, the Pack at most its budget of 9,000 although the full Pack is
//! longer, and the Pack is byte for byte what `resume --pack` prints. The
//! hook writes nothing to the ledger.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use std::fs;

use p4b_support::{Setup, step};
use p4h2_support::{
    BUDGET, HOOK_LIMIT, claim_line, env_file, high_water, ledger_holds, line_with, number_of,
    omission, session_start, standing, statement_and_pack, units,
};
use serde_json::Value;

/// Checkpoints of the first phase; each records four statements.
const CHECKPOINTS: u64 = 16;
/// The fresh client session Claude Code starts.
const CLIENT: &str = "fresh-client-1";

/// The text of statement `label` of Checkpoint `k`: the conclusion first,
/// then detail, `length` characters in all.
fn body(label: &str, k: u64, length: usize) -> String {
    let head = format!("{label}{k}: the {label} check holds at step {k}; ");
    let detail = "the detail of this step goes on. ".repeat(length / 33 + 1);
    format!("{head}{detail}")
        .chars()
        .take(length)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Checkpoint `k`: fact `A<k>` superseding `A<k-1>`, a new fact `B<k>`, open
/// item `O<k>` superseding `O<k-1>`, next action `N<k>` (which replaces the
/// one in force). All facts cite the context's latest run.
fn checkpoint(setup: &Setup, k: u64) {
    let agent = &setup.agent;
    let previous = (k > 1).then(|| agent.capsule());
    let supersedes = |label: &str| {
        previous
            .as_ref()
            .map(|capsule| number_of(capsule, &body(label, k - 1, 160)).to_string())
    };
    let (a, b, open, next) = (
        body("A", k, 160),
        body("B", k, 420),
        body("O", k, 160),
        body("N", k, 120),
    );
    let mut words = vec![
        "checkpoint".to_owned(),
        "create".to_owned(),
        "--fact".to_owned(),
        a,
    ];
    if let Some(number) = supersedes("A") {
        words.extend(["--supersedes".to_owned(), number]);
    }
    words.extend(["--fact".to_owned(), b, "--open".to_owned(), open]);
    if let Some(number) = supersedes("O") {
        words.extend(["--supersedes".to_owned(), number]);
    }
    words.extend(["--next".to_owned(), next]);
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    let run = agent.run(&words, true);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    println!(
        "[agent] checkpoint {k}: {}",
        run.stderr.lines().last().unwrap_or_default()
    );
}

/// The first phase: `begin`, one passing run, 16 Checkpoints, `end`.
/// Returns the Task.
fn long_task(setup: &Setup) -> String {
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 1: a long Task",
    ]);
    agent.test("tests/a.sh");
    for k in 1..=CHECKPOINTS {
        checkpoint(setup, k);
    }
    agent.ok(&["end", "--reason", "the first phase is done"]);
    begun["task_id"].as_str().unwrap().to_owned()
}

/// 64 statements, 45 superseded: about seven in ten.
fn check_the_statements(capsule: &Value) {
    let all = capsule["claims"].as_array().unwrap().len();
    let superseded = standing(capsule, "superseded").len();
    let active = standing(capsule, "active").len();
    println!("statements {all}, superseded {superseded}, active {active}");
    assert!(all >= 60, "{all} statements");
    assert_eq!((all, superseded, active), (64, 45, 19));
    // 0.65 ≤ superseded / all ≤ 0.75.
    assert!(superseded * 100 >= all * 65 && superseded * 100 <= all * 75);
}

/// The Pack names the Task's statements as the Capsule counts them, lists
/// every statement in force (in full or by its skeleton), and folds the 45
/// superseded ones into one line by number.
fn check_the_pack_content(pack: &str, capsule: &Value) {
    let task = line_with(pack, "Task「");
    assert!(
        task.starts_with("Task「scenario 1: a long Task」 (in_progress): "),
        "{task}"
    );
    assert!(task.contains("; active claims 19: "), "{task}");
    assert!(task.contains("; superseded 45; withdrawn 0"), "{task}");
    let retired = line_with(pack, "Superseded and withdrawn: ");
    assert!(
        retired.starts_with("Superseded and withdrawn: superseded 45: "),
        "{retired}"
    );
    let cut = omission(pack);
    let listed = standing(capsule, "active")
        .iter()
        .filter(|claim| claim_line(pack, claim["number"].as_u64().unwrap()).is_some())
        .count() as u64;
    assert_eq!(listed + cut.folded + cut.unlisted, 19, "{cut:?}");
    for claim in standing(capsule, "superseded") {
        let number = claim["number"].as_u64().unwrap();
        assert!(claim_line(pack, number).is_none(), "#{number} is listed");
    }
}

#[test]
fn scenario_1_a_fresh_session_starts_with_the_pack_within_its_budget() {
    let setup = Setup::new(&["a"]);
    step("scenario 1: a Task with 64 statements, 45 of them superseded");
    let task = long_task(&setup);
    let capsule = setup.agent.capsule();
    check_the_statements(&capsule);
    let full = setup
        .agent
        .bytes(&["resume", "--pack", "--budget", "1000000"]);
    assert!(units(&full) > BUDGET, "the full Pack fits anyway");
    assert!(!env_file(&setup).exists());
    let before = high_water(&setup);

    step("a fresh agent session starts: SessionStart (startup), no pasted prompt");
    let run = session_start(&setup, CLIENT, "startup");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        units(&run.stdout) <= HOOK_LIMIT,
        "{} code units",
        units(&run.stdout)
    );
    let (statement, pack) = statement_and_pack(&run.stdout);
    assert_eq!(
        statement,
        format!(
            "memprove: below is the Context Pack of Task {task} at SessionStart (startup), chosen \
             from this directory's context."
        )
    );
    assert!(units(&pack) <= BUDGET, "{} code units", units(&pack));
    let first = pack.lines().next().unwrap();
    assert!(first.starts_with("Pack: memprove-pack v1; "), "{first}");
    assert!(
        first.ends_with("; budget 9000 UTF-16 code units"),
        "{first}"
    );
    assert!(omission(&pack).level >= 1, "the full Pack had to be cut");
    assert_eq!(
        pack,
        setup.agent.bytes(&["resume", "--pack"]),
        "the injected Pack is resume --pack"
    );
    check_the_pack_content(&pack, &capsule);
    assert_eq!(high_water(&setup), before, "the hook wrote to the ledger");
    assert_eq!(
        fs::read_to_string(env_file(&setup)).unwrap(),
        format!("export MEMPROVE_CLIENT_SESSION='{CLIENT}'\n")
    );
    ledger_holds(&setup);
}
