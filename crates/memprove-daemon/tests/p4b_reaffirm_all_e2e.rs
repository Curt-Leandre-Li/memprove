//! P4-B (Owner, T18) `checkpoint create --reaffirm-all` corrections through
//! the real `memprove` CLI and the real `memproved` over a temporary
//! Git repository, with an agent token holding only `mcp-read` and
//! `mcp-write`:
//!
//! - 4a (SF-1): a fact on a test run is reaffirmed only on a test run;
//! - 4b: a refusal names the facts it would have reaffirmed, says nothing
//!   was written, and maps the detail's positions to fact numbers;
//! - 4c (N-b): the actual verdict of each reaffirmation is read back;
//! - 4d (N-a): with `--evidence`, a fact on another command is named;
//! - 4e (N-c): runs of another Workspace say so;
//! - 4f (P4-A judgment 8, supplement): with nothing to reaffirm, a `--next`
//!   that differs from the next action in force is still recorded.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use std::fs;

use p4b_support::{Cli, Setup, checkpoint, claim, commit, git, last_lines, sequence, step, write};
use serde_json::Value;

/// A test script that fails while `flags/fail-<name>` exists; `flags/` is
/// ignored, so the repository state stays the same.
fn flaky(name: &str) -> String {
    format!(
        "#!/bin/sh\nif [ -f flags/fail-{name} ]; then\n  echo 'test {name} ... FAILED'\n  \
         echo 'test result: FAILED. 0 passed; 1 failed'\n  exit 1\nfi\necho 'test {name} ... ok'\n\
         echo 'test result: ok. 1 passed; 0 failed'\n"
    )
}

/// A Setup whose `tests/a.sh` can fail on a flag, with F1 on `a` and F2 on
/// `b` recorded at X1 (`verified`), and X2 committed with no run since.
fn two_facts() -> Setup {
    let setup = Setup::new(&["a", "b"]);
    write(&setup.repo.join("tests/a.sh"), &flaky("a"));
    commit(&setup.repo, "a can fail on a flag");
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Reaffirm all, corrected",
    ]);
    let a = agent.test("tests/a.sh");
    let b = agent.test("tests/b.sh");
    checkpoint(
        agent,
        &[
            "--fact",
            "F1 a passes",
            "--evidence",
            &a,
            "--fact",
            "F2 b passes",
            "--evidence",
            &b,
            "--next",
            "n1",
        ],
        0,
    );
    let capsule = agent.capsule();
    for number in [1, 2] {
        assert_eq!(claim(&capsule, number)["verdict"], "verified", "#{number}");
    }
    write(&setup.repo.join("README"), "fixture\nX2\n");
    commit(&setup.repo, "X2");
    setup
}

/// `(number, standing, verdict)` of every fact.
fn facts(capsule: &Value) -> Vec<(u64, String, String)> {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|claim| claim["kind"] == "fact")
        .map(|claim| {
            (
                claim["number"].as_u64().unwrap(),
                claim["standing"].as_str().unwrap().to_owned(),
                claim["verdict"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn fact(number: u64, standing: &str, verdict: &str) -> (u64, String, String) {
    (number, standing.to_owned(), verdict.to_owned())
}

/// The kinds of the Evidence a claim cites.
fn kinds(claim: &Value) -> Vec<String> {
    claim["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// 4a: F1's Evidence is a test run of `a`. At X2 the latest run of `a` is
/// a passed command run, after a passed test run: F1 is reaffirmed on the
/// test run, F2 on `b`, both `verified`. At X3 `a` runs only as a command:
/// F1 is skipped with how to rerun it, F2 is reaffirmed and written.
#[test]
fn p4b_4a_a_test_fact_is_reaffirmed_only_on_a_test_run() {
    let setup = two_facts();
    let agent = &setup.agent;
    step("4a, X2: test a, then command a (both pass), test b");
    let test_a = agent.test("tests/a.sh");
    agent.command("tests/a.sh");
    let test_b = agent.test("tests/b.sh");
    let run = checkpoint(agent, &["--reaffirm-all", "--next", "n2"], 0);
    let lines = last_lines(&run.stderr, 3);
    assert_eq!(
        lines[0],
        format!(
            "memprove: reaffirm-all reaffirmed: #1 ({test_a}) as #4: verified, #2 ({test_b}) \
             as #5: verified"
        )
    );
    assert_eq!(lines[2], "memprove: reaffirmed 2 (verified 2), skipped 0");
    let capsule = agent.capsule();
    assert_eq!(kinds(claim(&capsule, 4)), ["test_result"]);
    step("4a, X3: command a only, test b");
    write(&setup.repo.join("README"), "fixture\nX3\n");
    commit(&setup.repo, "X3");
    let command_a = agent.command("tests/a.sh");
    let test_b = agent.test("tests/b.sh");
    let run = checkpoint(agent, &["--reaffirm-all", "--next", "n3"], 0);
    let lines = last_lines(&run.stderr, 3);
    assert_eq!(
        lines[0],
        format!("memprove: reaffirm-all reaffirmed: #5 ({test_b}) as #7: verified")
    );
    assert_eq!(
        lines[1],
        "memprove: reaffirm-all skipped: #4 (「sh tests/a.sh」 has no test run in this \
         repository state that this context recorded, and its Evidence is a test run; rerun it \
         as a test: `memprove run --test-format cargo -- <argv>`, or `memprove run --test \
         --test-cases <N> -- <argv>`)"
    );
    assert_eq!(lines[2], "memprove: reaffirmed 1 (verified 1), skipped 1");
    assert!(
        !run.stderr.contains(&format!("({command_a})")),
        "{}",
        run.stderr
    );
    let capsule = agent.capsule();
    assert_eq!(
        facts(&capsule),
        [
            fact(1, "superseded", "stale"),
            fact(2, "superseded", "stale"),
            fact(4, "active", "stale"),
            fact(5, "superseded", "stale"),
            fact(7, "active", "verified"),
        ]
    );
}

/// 4b: another context's failed run of `a` in the same state overrules
/// this context's pass; the daemon refuses the request. Standard error
/// names the facts, says nothing was written, and maps the detail's
/// position to #1. Nothing is retried.
#[test]
fn p4b_4b_a_refusal_names_the_fact_and_writes_nothing() {
    let setup = two_facts();
    let agent = &setup.agent;
    step("4b, X2: this context passes a and b; another context fails a");
    let test_a = agent.test("tests/a.sh");
    let test_b = agent.test("tests/b.sh");
    let task = agent.capsule()["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = agent.elsewhere(setup.root_path.join("config-other"), "other agent");
    other.ok(&["begin", "--project", &setup.project_id, "--task", &task]);
    fs::create_dir_all(setup.repo.join("flags")).unwrap();
    write(&setup.repo.join("flags/fail-a"), "1\n");
    other.test("tests/a.sh");
    let before = sequence(&agent.capsule());
    let run = checkpoint(agent, &["--reaffirm-all", "--next", "n2"], 1);
    assert!(!run.stdout.contains("checkpoint_id"), "{}", run.stdout);
    let lines = last_lines(&run.stderr, 4);
    assert_eq!(
        lines,
        [
            "memprove: reaffirm-all: the daemon refused the request, so nothing was written"
                .to_owned(),
            format!("memprove: reaffirm-all would have reaffirmed: #1 ({test_a}), #2 ({test_b})"),
            "memprove: reaffirm-all would have skipped: none".to_owned(),
            "memprove: reaffirm-all positions in the detail: $.verified_completed_work[0] \
             reaffirms #1"
                .to_owned(),
        ]
    );
    assert!(run.stderr.contains("overruled"), "{}", run.stderr);
    assert_eq!(sequence(&agent.capsule()), before, "nothing was written");
}

/// 4c (N-b): an untracked file exists when the run is recorded: the fact
/// is reaffirmed, and the line shows its actual verdict, not `verified`.
#[test]
fn p4b_4c_the_actual_verdict_is_read_back() {
    let setup = two_facts();
    let agent = &setup.agent;
    step("4c, X2: an untracked file, then test a");
    write(&setup.repo.join("scratch.txt"), "not added\n");
    let test_a = agent.test("tests/a.sh");
    let run = checkpoint(agent, &["--reaffirm-all", "--next", "n2"], 0);
    let lines = last_lines(&run.stderr, 3);
    assert!(
        lines[0].starts_with(&format!(
            "memprove: reaffirm-all reaffirmed: #1 ({test_a}) as #4: partial ("
        )),
        "{}",
        lines[0]
    );
    assert!(!lines[0].contains(": verified"), "{}", lines[0]);
    assert_eq!(lines[2], "memprove: reaffirmed 1 (verified 0), skipped 1");
    let capsule = agent.capsule();
    assert_eq!(claim(&capsule, 4)["verdict"], "partial");
}

/// 4d (N-a): with `--evidence`, F1 (on `a`) reaffirmed on a run of `b` is
/// named on standard error; both are reaffirmed as before.
#[test]
fn p4b_4d_with_evidence_a_fact_on_another_command_is_named() {
    let setup = two_facts();
    let agent = &setup.agent;
    step("4d, X2: test b only; --reaffirm-all --evidence <it>");
    let test_b = agent.test("tests/b.sh");
    let run = checkpoint(
        agent,
        &["--reaffirm-all", "--evidence", &test_b, "--next", "n2"],
        0,
    );
    let warnings = run
        .stderr
        .lines()
        .filter(|line| line.starts_with("memprove: warning: reaffirm-all:"))
        .collect::<Vec<_>>();
    assert_eq!(
        warnings,
        [format!(
            "memprove: warning: reaffirm-all: #1 is reaffirmed on {test_b}, which this context \
             does not record as runs of the commands its Evidence ran (「sh tests/a.sh」); it is \
             sent as given"
        )]
    );
    let lines = last_lines(&run.stderr, 3);
    assert_eq!(lines[2], "memprove: reaffirmed 2 (verified 2), skipped 0");
}

/// 4e (N-c): an explicit --workspace of another Workspace: the reason says
/// the runs are of another Workspace.
#[test]
fn p4b_4e_runs_of_another_workspace_say_so() {
    let setup = two_facts();
    let agent = &setup.agent;
    step("4e: a linked worktree registered as a second Workspace");
    let worktree = setup.root_path.join("wt2");
    git(
        &setup.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt2",
            worktree.to_str().unwrap(),
        ],
    );
    let task = agent.capsule()["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let elsewhere = Cli {
        cwd: worktree,
        ..agent.clone()
    };
    let other = elsewhere.ok(&["begin", "--project", &setup.project_id, "--task", &task]);
    let other_workspace = other["workspace_id"].as_str().unwrap().to_owned();
    let here = agent.capsule()["bindings"]["probe"]["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    agent.test("tests/a.sh");
    let run = checkpoint(
        agent,
        &[
            "--reaffirm-all",
            "--workspace",
            &other_workspace,
            "--next",
            "n1",
        ],
        0,
    );
    let reason = format!(
        "the runs this context holds are of Workspace {here}, not of the Checkpoint's Workspace \
         {other_workspace}; run the commands in that Workspace with `memprove run`"
    );
    assert!(
        run.stderr.contains(&format!("#1 ({reason})"))
            && run.stderr.contains(&format!("#2 ({reason})")),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("nothing was sent"), "{}", run.stderr);
}

/// 4f: X2 with no run: nothing to reaffirm. `--next` equal to the next
/// action in force sends nothing; a different one is recorded.
#[test]
fn p4b_4f_a_next_that_differs_is_still_recorded() {
    let setup = two_facts();
    let agent = &setup.agent;
    step("4f, X2, no run: --next n1 (in force), then --next n2");
    let before = sequence(&agent.capsule());
    let same = checkpoint(agent, &["--reaffirm-all", "--next", "n1"], 0);
    assert!(same.stdout.is_empty(), "{}", same.stdout);
    assert!(
        same.stderr
            .contains("(its --next is the next action in force); nothing was sent"),
        "{}",
        same.stderr
    );
    assert_eq!(sequence(&agent.capsule()), before);
    let differs = checkpoint(agent, &["--reaffirm-all", "--next", "n2"], 0);
    let recorded: Value = serde_json::from_str(differs.stdout.trim()).unwrap();
    assert_eq!(recorded["next_action"]["text"], "n2");
    assert!(
        differs
            .stderr
            .contains("memprove: new statements: #4 next_action"),
        "{}",
        differs.stderr
    );
    assert_eq!(
        last_lines(&differs.stderr, 1)[0],
        "memprove: reaffirmed 0 (verified 0), skipped 2"
    );
    let capsule = agent.capsule();
    assert!(sequence(&capsule) > before);
    assert_eq!(claim(&capsule, 3)["standing"], "superseded");
    assert_eq!(claim(&capsule, 4)["standing"], "active");
}
