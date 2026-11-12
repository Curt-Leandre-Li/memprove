//! P4-A (Owner): `checkpoint create --reaffirm-all` through the real
//! `memprove` CLI and the real `memproved` over a temporary Git
//! repository, with an agent token holding only `mcp-read` and
//! `mcp-write`.
//!
//! Five facts at X1: F1 on `a`, F2 on `b`, F3 on `c`, F4 on `d`, F5 on `a`
//! and `c` (each a test script). Then:
//!
//! 1. X2, no run since: the runs the context holds are of X1, so every fact
//!    is skipped, nothing is sent, the ledger does not move, exit 0 (tests
//!    c and "nothing to reaffirm").
//! 2. At X2, `a` and `b` pass, `d` passes and then fails (an ignored file,
//!    so the state is the same): F1 and F2 are reaffirmed on the new runs
//!    and read `verified`; F3 is skipped for want of `c` (test a), F4
//!    because the latest `d` failed (test b), F5 because only `a` ran
//!    (test d). Each reaffirmation cites exactly the commands of the
//!    original Evidence (the property).
//! 3. `c` passes at X2 in another Workspace of the Task and in another Task
//!    of this Workspace; back in this Task and Workspace only `a` runs: F3
//!    and F5 are still skipped, the verified reaffirmations of step 2 are
//!    not touched (tests c and e), nothing is sent.
//! 4. `c` passes here: a command that also names #3 (`--supersedes`) and #5
//!    (`--reaffirm`) leaves them to those flags (test f).
//! 5. X3: `--reaffirm-all --evidence <run>` reaffirms every waiting fact on
//!    that run, as one `--reaffirm <N> --evidence <run>` per fact would.
//!
//! Run with `--nocapture` to print the transcript; token secrets are never
//! printed.

use std::{
    collections::BTreeSet,
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);
const FLAKY: &str = "#!/bin/sh
# Passes until flags/fail exists; flags/ is ignored, so the state stays.
if [ -f flags/fail ]; then
  echo 'test d ... FAILED'
  echo 'test result: FAILED. 0 passed; 1 failed'
  exit 1
fi
echo 'test d ... ok'
echo 'test result: ok. 1 passed; 0 failed'
";

fn passing(name: &str) -> String {
    format!(
        "#!/bin/sh\n# Check {name}.\necho 'test {name} ... ok'\necho 'test result: ok. 1 passed; 0 failed'\n"
    )
}

fn cli_binary() -> PathBuf {
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_memproved"));
    let cli = daemon.with_file_name("memprove");
    assert!(
        cli.exists(),
        "memprove not found next to {}; run `cargo test --workspace`",
        daemon.display()
    );
    cli
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn step(title: &str) {
    println!("\n===== {title} =====");
}

fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    command
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    println!("$ git {}", arguments.join(" "));
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    text(&output.stdout)
}

fn commit(directory: &Path, message: &str) -> String {
    git(
        directory,
        &[
            "-c",
            "user.name=memprove Test",
            "-c",
            "user.email=memprove@example.invalid",
            "commit",
            "--quiet",
            "-am",
            message,
        ],
    );
    git(directory, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn write(path: &Path, contents: &str) {
    println!("$ printf {contents:?} > {}", path.display());
    fs::write(path, contents).unwrap();
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct Cli {
    data_dir: PathBuf,
    config_dir: PathBuf,
    token: PathBuf,
    cwd: PathBuf,
    label: &'static str,
}

impl Cli {
    fn run(&self, words: &[&str], quiet: bool) -> Run {
        let output = isolated(cli_binary())
            .current_dir(&self.cwd)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token-file")
            .arg(&self.token)
            .args(words)
            .env("MEMPROVE_CONFIG_DIR", &self.config_dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        if !quiet {
            println!(
                "[{}] $ ct {}\n{}{}[exit {}]",
                self.label,
                words.join(" "),
                run.stdout,
                run.stderr,
                run.code
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string())
            );
        }
        run
    }

    fn ok(&self, words: &[&str]) -> Value {
        let run = self.run(words, false);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }

    /// `memprove run` of a test script; returns the label it printed.
    fn test(&self, script: &str) -> String {
        let run = self.run(&["run", "--test-format", "cargo", "--", "sh", script], true);
        let last = run.stderr.lines().last().unwrap_or_default().to_owned();
        println!(
            "[{}] $ ct run --test-format cargo -- sh {script}\n{last}",
            self.label
        );
        let (_, label) = last
            .rsplit_once(", label ")
            .unwrap_or_else(|| panic!("no label: {}", run.stderr));
        label.to_owned()
    }

    fn capsule(&self) -> Value {
        let run = self.run(&["resume"], true);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(cli: &Cli, log: &Path) -> Self {
        let child = isolated(env!("CARGO_BIN_EXE_memproved"))
            .arg("--data-dir")
            .arg(&cli.data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(log).unwrap())
            .spawn()
            .unwrap();
        // U1 (C6): the guard exists before anything below can panic.
        let daemon = Self { child };
        let deadline = Instant::now() + TIMEOUT;
        while cli.run(&["health", "ready"], true).code != Some(0) {
            assert!(
                Instant::now() < deadline,
                "memproved never became ready:\n{}",
                fs::read_to_string(log).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn stop(mut self) {
        let sent = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = self.child.wait().unwrap();
        assert_eq!((status.code(), status.signal()), (Some(0), None));
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn claim(capsule: &Value, number: u64) -> &Value {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["number"] == number)
        .unwrap_or_else(|| panic!("no claim #{number}"))
}

fn sequence(capsule: &Value) -> u64 {
    capsule["bindings"]["ledger"]["project_sequence"]
        .as_u64()
        .unwrap()
}

/// The argv of every Evidence a claim cites, as a set.
fn commands(claim: &Value) -> BTreeSet<Vec<String>> {
    claim["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|evidence| {
            evidence["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|word| word.as_str().unwrap().to_owned())
                .collect()
        })
        .collect()
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

/// The property of P4-A: each claim `by` that superseded `target` by a
/// reaffirmation cites exactly the commands `target` cited.
fn assert_same_commands(capsule: &Value, pairs: &[(u64, u64)]) {
    for (target, by) in pairs {
        let old = claim(capsule, *target);
        let new = claim(capsule, *by);
        assert_eq!(new["text"], old["text"], "#{by} reaffirms #{target}");
        assert_eq!(
            old["superseded_by"][0]["number"], *by,
            "#{target} is superseded by #{by}"
        );
        assert_eq!(commands(new), commands(old), "#{target} → #{by}");
    }
}

struct Setup {
    root: tempfile::TempDir,
    repo: PathBuf,
    project_id: String,
    agent: Cli,
    daemon: Option<Daemon>,
}

impl Setup {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let repo = root_path.join("repo");
        fs::create_dir_all(repo.join("tests")).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(repo.join("README"), b"fixture\n").unwrap();
        fs::write(repo.join(".gitignore"), b"flags/\n").unwrap();
        for name in ["a", "b", "c"] {
            fs::write(repo.join(format!("tests/{name}.sh")), passing(name)).unwrap();
        }
        fs::write(repo.join("tests/d.sh"), FLAKY).unwrap();
        git(&repo, &["add", "."]);
        commit(&repo, "fixture with four test scripts");
        let operator = Cli {
            data_dir: root_path.join("data"),
            config_dir: root_path.join("config"),
            token: root_path.join("control.token"),
            cwd: repo.clone(),
            label: "operator",
        };
        let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
            .args(["bootstrap", "--data-dir"])
            .arg(&operator.data_dir)
            .arg("--output")
            .arg(&operator.token)
            .output()
            .unwrap();
        assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
        let daemon = Daemon::start(&operator, &root_path.join("daemon.log"));
        let registered = operator.ok(&["project", "register", repo.to_str().unwrap()]);
        let project_id = registered["project_id"].as_str().unwrap().to_owned();
        let created = operator.run(
            &[
                "token",
                "create",
                "--label",
                "agent",
                "--scope",
                "mcp-read",
                "--scope",
                "mcp-write",
            ],
            true,
        );
        assert_eq!(created.code, Some(0), "{}", created.stderr);
        let created: Value = serde_json::from_str(created.stdout.trim()).unwrap();
        let token = root_path.join("agent.token");
        fs::write(&token, created["secret"].as_str().unwrap()).unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        let agent = Cli {
            token,
            label: "agent/mcp-read+mcp-write",
            ..operator
        };
        Self {
            root,
            repo,
            project_id,
            agent,
            daemon: Some(daemon),
        }
    }
}

/// `checkpoint create` with `words`; asserts the exit code and returns the
/// run.
fn checkpoint(agent: &Cli, words: &[&str], code: i32) -> Run {
    let run = agent.run(&[&["checkpoint", "create"][..], words].concat(), false);
    assert_eq!(run.code, Some(code), "{}{}", run.stdout, run.stderr);
    run
}

/// Five facts at X1 on four commands; all `verified`.
fn five_facts(setup: &Setup) {
    step("X1: F1 on a, F2 on b, F3 on c, F4 on d, F5 on a and c");
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Reaffirm all",
    ]);
    let labels = ["a", "b", "c", "d"].map(|name| agent.test(&format!("tests/{name}.sh")));
    assert_eq!(labels, ["r1", "r2", "r3", "r4"]);
    checkpoint(
        agent,
        &[
            "--fact",
            "F1 a passes",
            "--evidence",
            "r1",
            "--fact",
            "F2 b passes",
            "--evidence",
            "r2",
            "--fact",
            "F3 c passes",
            "--evidence",
            "r3",
            "--fact",
            "F4 d passes",
            "--evidence",
            "r4",
            "--fact",
            "F5 a and c pass",
            "--evidence",
            "r1",
            "--evidence",
            "r3",
            "--next",
            "n1",
        ],
        0,
    );
    let capsule = agent.capsule();
    for number in 1..=5 {
        assert_eq!(claim(&capsule, number)["verdict"], "verified", "#{number}");
    }
}

/// Step 1: the state changed and nothing ran since: every fact is skipped
/// and nothing is sent.
fn earlier_state_runs_are_not_used(setup: &Setup) {
    step("1. X2 without a run: the context's runs are of X1");
    write(&setup.repo.join("README"), "fixture\nsecond line\n");
    commit(&setup.repo, "README: second line");
    let before = setup.agent.capsule();
    assert!(
        facts(&before)
            .iter()
            .all(|(_, _, verdict)| verdict == "stale")
    );
    // P4-B: the next action in force, so that nothing else is recorded.
    let run = checkpoint(&setup.agent, &["--reaffirm-all", "--next", "n1"], 0);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    assert!(run.stderr.contains("nothing was sent"), "{}", run.stderr);
    for number in 1..=5 {
        assert!(
            run.stderr.contains(&format!(
                "#{number} (the repository state changed since the latest `memprove run`"
            )),
            "{}",
            run.stderr
        );
    }
    assert_eq!(
        run.stderr.lines().last(),
        Some("memprove: reaffirmed 0 (verified 0), skipped 5")
    );
    let after = setup.agent.capsule();
    assert_eq!(
        sequence(&after),
        sequence(&before),
        "the ledger did not move"
    );
    assert_eq!(after["claims"], before["claims"]);
}

/// Step 2: tests a, b, d and the property. Returns the stderr of the
/// command.
fn two_of_three_rerun(setup: &Setup) -> String {
    step("2. X2: a and b pass; d passes, then fails in the same state; c is not run");
    let agent = &setup.agent;
    let a = agent.test("tests/a.sh");
    let b = agent.test("tests/b.sh");
    agent.test("tests/d.sh");
    fs::create_dir_all(setup.repo.join("flags")).unwrap();
    write(&setup.repo.join("flags/fail"), "1\n");
    let failed = agent.test("tests/d.sh");
    let run = checkpoint(agent, &["--reaffirm-all", "--next", "n2"], 0);
    let stderr = run.stderr.clone();
    let lines = stderr.lines().collect::<Vec<_>>();
    assert_eq!(
        lines[lines.len() - 3..],
        [
            format!(
                "memprove: reaffirm-all reaffirmed: #1 ({a}) as #7: verified, #2 ({b}) as #8: \
                 verified"
            ),
            format!(
                "memprove: reaffirm-all skipped: #3 (「sh tests/c.sh」 has no run in this \
                 repository state), #4 (the latest run {failed} of 「sh tests/d.sh」 in this \
                 repository state did not pass (FAIL)), #5 (「sh tests/c.sh」 has no run in \
                 this repository state)"
            ),
            "memprove: reaffirmed 2 (verified 2), skipped 3".to_owned(),
        ]
    );
    assert!(
        stderr.contains("memprove: reaffirmed: #1 as #7, #2 as #8"),
        "{stderr}"
    );
    let capsule = agent.capsule();
    assert_eq!(
        facts(&capsule),
        [
            (1, "superseded".to_owned(), "stale".to_owned()),
            (2, "superseded".to_owned(), "stale".to_owned()),
            (3, "active".to_owned(), "stale".to_owned()),
            (4, "active".to_owned(), "stale".to_owned()),
            (5, "active".to_owned(), "stale".to_owned()),
            (7, "active".to_owned(), "verified".to_owned()),
            (8, "active".to_owned(), "verified".to_owned()),
        ]
    );
    assert_same_commands(&capsule, &[(1, 7), (2, 8)]);
    fs::remove_file(setup.repo.join("flags/fail")).unwrap();
    stderr
}

/// Step 3: runs of another Workspace and of another Task are not used; a
/// fact on two commands with one rerun is skipped; verified facts are not
/// reaffirmed again.
fn other_task_and_workspace_runs_are_not_used(setup: &Setup) -> String {
    step("3. c passes in another Workspace and in another Task; here only a runs");
    let agent = &setup.agent;
    let task_id = agent.capsule()["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let worktree = setup.root.path().canonicalize().unwrap().join("linked");
    git(
        &setup.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
        ],
    );
    let linked = Cli {
        cwd: worktree,
        label: "agent in linked worktree",
        ..agent.clone()
    };
    linked.ok(&["begin", "--project", &setup.project_id, "--task", &task_id]);
    linked.test("tests/c.sh");
    linked.ok(&["end", "--reason", "c passed in another Workspace"]);
    agent.ok(&["end", "--reason", "switch Tasks"]);
    agent.ok(&["begin", "--new-task", "Another Task"]);
    agent.test("tests/c.sh");
    agent.ok(&["end", "--reason", "c passed in another Task"]);
    agent.ok(&["begin", "--task", &task_id]);
    let label = agent.test("tests/a.sh");
    let before = agent.capsule();
    // P4-B: the next action in force, so that nothing else is recorded.
    let run = checkpoint(agent, &["--reaffirm-all", "--next", "n2"], 0);
    assert!(run.stdout.is_empty(), "nothing was sent: {}", run.stdout);
    assert_eq!(
        run.stderr.lines().rev().take(3).collect::<Vec<_>>(),
        [
            "memprove: reaffirmed 0 (verified 0), skipped 3",
            "memprove: reaffirm-all skipped: #3 (「sh tests/c.sh」 has no run in this \
             repository state), #4 (「sh tests/d.sh」 has no run in this repository state), #5 \
             (「sh tests/c.sh」 has no run in this repository state)",
            "memprove: reaffirm-all reaffirmed: none",
        ],
        "{label}"
    );
    assert_eq!(sequence(&agent.capsule()), sequence(&before));
    label
}

/// Step 4: test f.
fn named_facts_are_left_to_their_flags(setup: &Setup, a: &str) {
    step("4. c passes here; #3 and #5 are named by --supersedes and --reaffirm");
    let agent = &setup.agent;
    let c = agent.test("tests/c.sh");
    let run = checkpoint(
        agent,
        &[
            "--reaffirm-all",
            "--reaffirm",
            "5",
            "--evidence",
            a,
            "--evidence",
            &c,
            "--fact",
            "F3b c passes again",
            "--evidence",
            &c,
            "--supersedes",
            "3",
            "--next",
            "n4",
        ],
        0,
    );
    let lines = run.stderr.lines().collect::<Vec<_>>();
    assert_eq!(
        &lines[lines.len() - 3..],
        [
            "memprove: reaffirm-all reaffirmed: none",
            "memprove: reaffirm-all skipped: #4 (「sh tests/d.sh」 has no run in this \
             repository state)",
            "memprove: reaffirmed 0 (verified 0), skipped 1",
        ]
    );
    let capsule = agent.capsule();
    let new = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|claim| claim["number"].as_u64().unwrap() > 9)
        .map(|claim| claim["text"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(new, ["F5 a and c pass", "F3b c passes again", "n4"]);
}

/// P4-B (T18 N-b): the reaffirmed line's items for `waiting`, each with
/// the claim that reaffirms it in `after` and that claim's verdict, and how
/// many of those are `verified`.
fn reaffirmed_as(after: &Value, waiting: &[Value], label: &str) -> (String, usize) {
    let mut verified = 0;
    let numbers = waiting
        .iter()
        .map(|waited| {
            let old = claim(after, waited["number"].as_u64().unwrap());
            let by = old["superseded_by"][0]["number"].as_u64().unwrap();
            let new = claim(after, by);
            let verdict = new["verdict"].as_str().unwrap();
            if verdict == "verified" {
                verified += 1;
            }
            let reasons = new["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .map(|reason| reason.as_str().unwrap())
                .collect::<Vec<_>>();
            let shown = if reasons.is_empty() {
                verdict.to_owned()
            } else {
                format!("{verdict} ({})", reasons.join(", "))
            };
            format!("#{} ({label}) as #{by}: {shown}", old["number"])
        })
        .collect::<Vec<_>>()
        .join(", ");
    (numbers, verified)
}

/// Step 5: mode two at X3.
fn evidence_applies_to_every_waiting_fact(setup: &Setup) {
    step("5. X3: --reaffirm-all --evidence <run>");
    let agent = &setup.agent;
    write(
        &setup.repo.join("README"),
        "fixture\nsecond line\nthird line\n",
    );
    commit(&setup.repo, "README: third line");
    let label = agent.test("tests/a.sh");
    let before = agent.capsule();
    let waiting = before["claims"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|claim| {
            claim["kind"] == "fact"
                && claim["standing"] == "active"
                && claim["verdict"] != "verified"
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(waiting.len(), 5, "{waiting:?}");
    let run = checkpoint(
        agent,
        &["--reaffirm-all", "--evidence", &label, "--next", "n5"],
        0,
    );
    let checkpoint: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    let recorded = agent.ok(&[
        "checkpoint",
        "get",
        checkpoint["checkpoint_id"].as_str().unwrap(),
    ]);
    // What one `--reaffirm <N> --evidence <label>` per waiting fact sends:
    // in number order, each with the fact's text and the one run, and each
    // superseding its fact (the edges are read from the Capsule).
    let cited = recorded["verified_completed_work"][0]["evidence_ids"].clone();
    assert_eq!(cited.as_array().map(Vec::len), Some(1));
    let expected = waiting
        .iter()
        .map(|claim| {
            serde_json::json!({
                "classification": "verified_fact",
                "text": claim["text"],
                "evidence_ids": cited,
            })
        })
        .collect::<Vec<_>>();
    let statements = recorded["verified_completed_work"]
        .as_array()
        .unwrap()
        .iter()
        .map(|statement| {
            serde_json::json!({
                "classification": statement["classification"],
                "text": statement["text"],
                "evidence_ids": statement["evidence_ids"],
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(statements, expected);
    let after = agent.capsule();
    let prefix = format!(
        "{}/verified_completed_work/",
        checkpoint["checkpoint_id"].as_str().unwrap()
    );
    for (ordinal, old) in waiting.iter().enumerate() {
        let old = claim(&after, old["number"].as_u64().unwrap());
        assert_eq!(old["standing"], "superseded");
        let by = old["superseded_by"][0]["claim_id"].as_str().unwrap();
        assert_eq!(by, format!("{prefix}{ordinal}"));
    }
    // P4-B (T18 N-b): each with the claim that reaffirms it and its
    // verdict, read back from the Capsule.
    let (numbers, verified) = reaffirmed_as(&after, &waiting, &label);
    let lines = run.stderr.lines().collect::<Vec<_>>();
    assert_eq!(
        &lines[lines.len() - 3..],
        [
            format!("memprove: reaffirm-all reaffirmed: {numbers}").as_str(),
            "memprove: reaffirm-all skipped: none",
            format!("memprove: reaffirmed 5 (verified {verified}), skipped 0").as_str(),
        ]
    );
}

#[test]
fn reaffirm_all_through_real_binaries() {
    let mut setup = Setup::new();
    five_facts(&setup);
    earlier_state_runs_are_not_used(&setup);
    let stderr = two_of_three_rerun(&setup);
    println!(
        "\n----- P4-A test a: stderr of checkpoint create --reaffirm-all -----\n{stderr}----- end -----"
    );
    let a = other_task_and_workspace_runs_are_not_used(&setup);
    named_facts_are_left_to_their_flags(&setup, &a);
    evidence_applies_to_every_waiting_fact(&setup);
    assert_eq!(setup.agent.run(&["ledger", "verify"], true).code, Some(0));
    setup.daemon.take().unwrap().stop();
}
