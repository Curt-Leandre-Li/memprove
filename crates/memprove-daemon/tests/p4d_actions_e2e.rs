//! P4-D (ADR 0014 E3; Owner decisions A1, B3, C1) through the real
//! binaries: every action the Task Capsule lists is executed at once with
//! the `memprove` CLI, from the same ledger (the Project's data is saved
//! before each read and put back after each write). An action read as
//! available is not refused; one read as unavailable is refused with a
//! Problem code it lists (Owner condition b). Each action is seen available
//! and unavailable in several ledger states.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use std::{fs, path::PathBuf};

use p4b_support::{Cli, Setup, commit, step, write};
use serde_json::Value;

/// A test script that fails and prints a cargo-style summary.
const FAILING: &str =
    "#!/bin/sh\necho 'test f ... FAILED'\necho 'test result: FAILED. 0 passed; 1 failed'\nexit 1\n";

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect()
}

fn action<'a>(capsule: &'a Value, name: &str) -> &'a Value {
    capsule["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["action"] == name)
        .unwrap()
}

/// The Problem `memprove` reported on standard error: `<status> <code>`.
fn refusal(stderr: &str) -> Option<(String, String)> {
    stderr.lines().find_map(|line| {
        let (_, rest) = line.split_once(" failed: ")?;
        let (status, code) = rest.split_once(' ')?;
        let code = code.split([':', ' ']).next()?;
        Some((status.to_owned(), code.to_owned()))
    })
}

struct Ledger<'a> {
    setup: &'a mut Setup,
    task: String,
    workspace: String,
    agent_id: String,
    /// The Session `cite_latest_run` writes with: the one the context
    /// began, also once it has ended (P4-H1, T20 N-1).
    session: Option<String>,
}

impl Ledger<'_> {
    /// `task transition` of the Task to `target` at the revision the
    /// Capsule reads (the context's may be ahead of a restored ledger).
    fn transition(&self, target: &str) {
        let revision = self.capsule()["task"]["revision"].to_string();
        self.setup.agent.ok(&[
            "task",
            "transition",
            &self.task,
            "--to",
            target,
            "--revision",
            &revision,
        ]);
    }

    fn agent(&self) -> Cli {
        self.setup.agent.clone()
    }

    fn capsule(&self) -> Value {
        let run = self.setup.agent.run(
            &[
                "resume",
                "--task",
                &self.task,
                "--workspace",
                &self.workspace,
            ],
            true,
        );
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }

    /// Executes `action` as its `args` say, with the CLI; `None` when no
    /// request can be formed (no run to cite), else the exit code and the
    /// refusal.
    fn execute(&self, action: &Value) -> Option<(i32, Option<(String, String)>)> {
        let args = &action["args"];
        // U8 K5: a CLI whose context recorded a run as a command run refuses
        // a fact that cites only command runs before it asks the daemon.
        // This test asks what the daemon answers, so `cite_latest_run` goes
        // through a configuration directory with no context (it names the
        // Workspace, Task, Session, and Snapshot itself).
        let agent = if action["action"] == "cite_latest_run" {
            self.agent().elsewhere(
                self.setup.root_path.join("config-without-context"),
                "no context",
            )
        } else {
            self.agent()
        };
        let mut words: Vec<String> = Vec::new();
        match action["action"].as_str().unwrap() {
            "complete_task" => {
                words.extend(
                    [
                        "task",
                        "transition",
                        &self.task,
                        "--to",
                        "completed",
                        "--revision",
                    ]
                    .map(str::to_owned),
                );
                words.push(args["expected_revision"].to_string());
                for id in strings(&args["completion_evidence_ids"]) {
                    words.extend(["--evidence".to_owned(), id.to_owned()]);
                }
                words.extend([
                    "--workspace".to_owned(),
                    args["workspace_id"].as_str().unwrap().to_owned(),
                ]);
            }
            "begin_session" => {
                words.extend(["session", "create", "--workspace"].map(str::to_owned));
                words.push(args["workspace_id"].as_str().unwrap().to_owned());
                words.extend([
                    "--agent".to_owned(),
                    self.agent_id.clone(),
                    "--task".to_owned(),
                    self.task.clone(),
                ]);
            }
            "cite_latest_run" => {
                let evidence = args["evidence_id"].as_str()?;
                words.extend(
                    [
                        "checkpoint",
                        "create",
                        "--fact",
                        "the latest run passed",
                        "--evidence",
                        evidence,
                        "--snapshot",
                        args["snapshot_id"].as_str().unwrap(),
                        "--workspace",
                        args["workspace_id"].as_str().unwrap(),
                        "--task",
                        &self.task,
                        // The Task's title would come from the context.
                        "--objective",
                        "P4-D actions",
                        "--next",
                        "go on",
                    ]
                    .map(str::to_owned),
                );
                if let Some(session) = &self.session {
                    words.extend(["--session".to_owned(), session.clone()]);
                }
            }
            other => panic!("unknown action {other}"),
        }
        let words = words.iter().map(String::as_str).collect::<Vec<_>>();
        let run = agent.run(&words, false);
        Some((run.code.unwrap(), refusal(&run.stderr)))
    }

    /// Reads every action, executes each at once from the same saved
    /// ledger, and checks the outcome against the reading; `expected` is
    /// each action's availability in this state.
    fn check(&mut self, label: &str, expected: [(&str, bool); 3]) {
        step(&format!("state {label}"));
        self.setup.stop();
        let copy: PathBuf = self.setup.save(&format!("ledger-{label}"));
        self.setup.start();
        for (name, available) in expected {
            let capsule = self.capsule();
            let read = action(&capsule, name).clone();
            println!("{name}: {read}");
            assert_eq!(read["available"], available, "{label}: {read}");
            assert_eq!(read["scope"], "mcp-write");
            match self.execute(&read) {
                None => {
                    assert!(!available, "{label}: {name} available without a request");
                }
                Some((code, refused)) => {
                    if available {
                        assert_eq!(code, 0, "{label}: {name} was available: {refused:?}");
                    } else {
                        let (status, problem) =
                            refused.unwrap_or_else(|| panic!("{label}: {name}: no Problem"));
                        // The reasons are in check order: the write answers
                        // the first.
                        assert_eq!(
                            strings(&read["reasons"]).first(),
                            Some(&problem.as_str()),
                            "{label}: {name} unavailable for {} but refused {status} {problem}",
                            read["reasons"]
                        );
                    }
                }
            }
            self.setup.stop();
            self.setup.restore(&copy);
            self.setup.start();
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One ledger, state by state.
fn every_action_reads_as_its_write_answers() {
    let mut setup = Setup::new(&["a"]);
    write(&setup.repo.join("tests/f.sh"), FAILING);
    commit_all(&setup);
    let agent = setup.agent.clone();
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "P4-D actions",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let workspace = begun["workspace_id"].as_str().unwrap().to_owned();
    let mut ledger = Ledger {
        setup: &mut setup,
        task,
        workspace: workspace.clone(),
        agent_id: begun["agent_id"].as_str().unwrap().to_owned(),
        session: Some(begun["session_id"].as_str().unwrap().to_owned()),
    };
    ledger.check(
        "no run",
        [
            ("complete_task", false),
            ("begin_session", true),
            ("cite_latest_run", false),
        ],
    );
    agent.test("tests/a.sh");
    ledger.check(
        "a passed",
        [
            ("complete_task", true),
            ("begin_session", true),
            ("cite_latest_run", true),
        ],
    );
    agent.test("tests/f.sh");
    ledger.check(
        "f failed last",
        [
            ("complete_task", true),
            ("begin_session", true),
            ("cite_latest_run", false),
        ],
    );
    let readme = ledger.setup.repo.join("README");
    write(&readme, "changed\n");
    commit(&ledger.setup.repo, "change README");
    ledger.check(
        "after a commit",
        [
            ("complete_task", false),
            ("begin_session", true),
            ("cite_latest_run", false),
        ],
    );
    agent.command("tests/a.sh");
    ledger.check(
        "a passed as a command",
        [
            ("complete_task", true),
            ("begin_session", true),
            ("cite_latest_run", false),
        ],
    );
    ledger.transition("blocked");
    ledger.check(
        "blocked",
        [
            ("complete_task", false),
            ("begin_session", true),
            ("cite_latest_run", false),
        ],
    );
    let capsule = ledger.capsule();
    assert_eq!(
        strings(&action(&capsule, "complete_task")["reasons"]),
        ["invalid_transition"]
    );
    ledger.transition("in_progress");
    agent.test("tests/a.sh");

    step("Owner condition d: a tampered Task does not complete");
    ledger.setup.stop();
    let intact = ledger.setup.save("ledger-intact");
    ledger.setup.tamper(&format!(
        "UPDATE tasks SET revision = revision + 7 WHERE task_id = '{}';",
        ledger.task
    ));
    ledger.setup.start();
    let capsule = ledger.capsule();
    assert_eq!(capsule["status"], "tampered");
    ledger.check(
        "tampered",
        [
            ("complete_task", false),
            ("begin_session", true),
            ("cite_latest_run", true),
        ],
    );
    let capsule = ledger.capsule();
    assert_eq!(
        strings(&action(&capsule, "complete_task")["reasons"]),
        ["evidence_stale"]
    );
    ledger.setup.stop();
    ledger.setup.restore(&intact);
    ledger.setup.start();

    step("completing the Task as complete_task says");
    let capsule = ledger.capsule();
    let complete = action(&capsule, "complete_task").clone();
    assert_eq!(complete["available"], true);
    let (code, _) = ledger.execute(&complete).unwrap();
    assert_eq!(code, 0);
    ledger.check(
        "completed",
        [
            ("complete_task", false),
            ("begin_session", false),
            ("cite_latest_run", true),
        ],
    );
    let capsule = ledger.capsule();
    assert_eq!(
        strings(&action(&capsule, "begin_session")["reasons"]),
        ["invalid_transition"]
    );
    // `begin` refuses it before any request, as before.
    let refused = agent.run(&["begin"], false);
    assert_eq!(refused.code, Some(1));

    step("P4-H1 (T20 N-1): no open Session of a completed Task, nothing to cite with");
    agent.ok(&["end", "--reason", "done"]);
    ledger.check(
        "completed, Session ended",
        [
            ("complete_task", false),
            ("begin_session", false),
            ("cite_latest_run", false),
        ],
    );
    let capsule = ledger.capsule();
    assert_eq!(
        strings(&action(&capsule, "cite_latest_run")["reasons"]),
        ["invalid_transition"]
    );

    step("a cancelled Task takes no new Session either (Owner A1)");
    let operator = ledger.setup.operator.clone();
    let created = operator.ok(&[
        "task",
        "create",
        "--project",
        &ledger.setup.project_id,
        "--title",
        "to be cancelled",
    ]);
    let cancelled = created["task_id"].as_str().unwrap().to_owned();
    operator.ok(&[
        "task",
        "transition",
        &cancelled,
        "--to",
        "cancelled",
        "--revision",
        "1",
    ]);
    let mut other = Ledger {
        setup: ledger.setup,
        task: cancelled,
        workspace,
        agent_id: begun["agent_id"].as_str().unwrap().to_owned(),
        session: None,
    };
    other.check(
        "cancelled",
        [
            ("complete_task", false),
            ("begin_session", false),
            ("cite_latest_run", false),
        ],
    );
    let capsule = other.capsule();
    assert_eq!(
        strings(&action(&capsule, "cite_latest_run")["reasons"]),
        ["invalid_request", "invalid_transition"]
    );
    let project = other.setup.project_id.clone();
    let ledger_verified = other
        .setup
        .operator
        .ok(&["ledger", "verify", "--project", &project]);
    assert_eq!(ledger_verified["valid"], true);
}

/// P4-H1 (Owner, T20 N-1) through the real binaries: a cancelled Task's
/// latest run is citable while its Session is open, and not once that
/// Session ended abandoned (`invalid_transition`); both readings are
/// executed with `memprove checkpoint create --session`.
#[test]
fn n1_a_cancelled_task_cites_only_through_an_open_session() {
    let mut setup = Setup::new(&["a"]);
    let agent = setup.agent.clone();
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "N-1 cancelled",
    ]);
    let mut ledger = Ledger {
        setup: &mut setup,
        task: begun["task_id"].as_str().unwrap().to_owned(),
        workspace: begun["workspace_id"].as_str().unwrap().to_owned(),
        agent_id: begun["agent_id"].as_str().unwrap().to_owned(),
        session: Some(begun["session_id"].as_str().unwrap().to_owned()),
    };
    agent.test("tests/a.sh");
    ledger.transition("blocked");
    ledger.transition("cancelled");
    ledger.check(
        "cancelled, Session open",
        [
            ("complete_task", false),
            ("begin_session", false),
            ("cite_latest_run", true),
        ],
    );
    agent.ok(&["end", "--abandoned", "--reason", "cancelled"]);
    ledger.check(
        "cancelled, Session abandoned",
        [
            ("complete_task", false),
            ("begin_session", false),
            ("cite_latest_run", false),
        ],
    );
    let capsule = ledger.capsule();
    assert_eq!(capsule["sessions"][0]["end_state"], "crashed");
    assert_eq!(
        strings(&action(&capsule, "cite_latest_run")["reasons"]),
        ["invalid_transition"]
    );
    let project = ledger.setup.project_id.clone();
    let verified = ledger
        .setup
        .operator
        .ok(&["ledger", "verify", "--project", &project]);
    assert_eq!(verified["valid"], true);
}

fn commit_all(setup: &Setup) {
    p4b_support::git(&setup.repo, &["add", "."]);
    commit(&setup.repo, "a failing test");
    assert!(fs::metadata(setup.repo.join("tests/f.sh")).is_ok());
}
