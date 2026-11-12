//! P3-D commit 1 (ADR 0013 D3): same-state carry-over through the real
//! `memprove` CLI and the real `memproved` over a temporary Git
//! repository. The agent uses one token with only `mcp-read` and
//! `mcp-write`, and cites runs by the labels `run` prints, never by UUID
//! (asserted for every agent command line but the first `begin`).
//!
//! `same_state_runs_are_carried_over_through_real_binaries`:
//! 1. Two passed runs in one state: the second Snapshot seals both; one
//!    fact cites `r1` and `r2` and is `verified`; a commit makes it `stale`.
//! 2. A failed run in that state is carried too, but a fact on it is
//!    refused (and nothing is sent for a label of another state).
//! 3. SIGKILL and restart: the same Capsule bytes, a valid ledger, and the
//!    next run in the state carries every earlier one.
//! 4. Thirty runs in one state: membership rows and the time of the first
//!    and the thirtieth `memprove run`.
//!
//! `no_carry_over_across_states`: HEAD, index, tracked content, branch,
//! another Task, another Workspace, and an untracked file each stop it.
//!
//! `a_later_failure_overrules_an_earlier_pass_through_real_binaries`
//! (P3-D3, T14 S-1): an ignored file changes, which the capture manifest
//! does not observe, and the same command fails in the same state. `run`
//! marks the earlier pass overruled; a fact on it is refused; `resume
//! --text` and `recover preview --text` say nothing is verified; Task
//! completion with it is refused.
//!
//! Run with `--nocapture` to print the transcript; token secrets are never
//! printed.

use memprove_domain::says_verified;
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);
const PASSING: &str = "#!/bin/sh
# Three checks over the fixture repository.
set -e
test -f README && echo 'test readme_exists ... ok'
grep -q fixture README && echo 'test readme_content ... ok'
test \"$(git rev-parse --is-inside-work-tree)\" = true && echo 'test inside_worktree ... ok'
echo 'test result: ok. 3 passed; 0 failed'
";
const FAILING: &str = "#!/bin/sh
# The release notes check fails until they are written.
echo 'test readme_exists ... ok'
echo 'test readme_content ... ok'
echo 'test release_notes ... FAILED'
echo 'test result: FAILED. 2 passed; 1 failed'
exit 1
";

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

/// Whether `value` holds a UUID-shaped run of characters.
fn has_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(36).any(|window| {
        window.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    })
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
    /// Agent steps assert that their command line carries no UUID.
    no_uuid: bool,
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
            let shown = words
                .iter()
                .map(|word| {
                    if word.contains(' ') || word.starts_with('#') {
                        format!("{word:?}")
                    } else {
                        (*word).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            if self.no_uuid {
                assert!(
                    !has_uuid(&shown),
                    "an agent command line carries a UUID: {shown}"
                );
            }
            println!(
                "[{}] $ ct {shown}\n{}{}[exit {}]",
                self.label,
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

    fn resume(&self) -> (String, Value) {
        let run = self.run(&["resume"], true);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        (
            run.stdout.clone(),
            serde_json::from_str(run.stdout.trim()).unwrap(),
        )
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
        println!(
            "$ memproved --data-dir <DATA> &   # pid {}",
            daemon.child.id()
        );
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

    fn signal(mut self, name: &str) {
        println!("$ kill -{name} {}", self.child.id());
        let sent = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = self.child.wait().unwrap();
        println!("memproved exited: {status}");
        if name == "KILL" {
            assert_eq!(status.signal(), Some(9));
        } else {
            assert_eq!(status.code(), Some(0));
        }
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

fn str_of<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name]
        .as_str()
        .unwrap_or_else(|| panic!("{name} is not a string in {value}"))
}

fn sealed(run: &Value) -> Vec<String> {
    ["command_evidence_ids", "test_evidence_ids"]
        .iter()
        .flat_map(|name| run["snapshot"][name].as_array().unwrap().clone())
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// The label `run` printed on its last line of standard error.
fn label_of(stderr: &str) -> String {
    let last = stderr.lines().last().unwrap();
    last.rsplit_once(", label ").map_or_else(
        || panic!("no label in {last:?}"),
        |(_, label)| label.trim().to_owned(),
    )
}

/// Everything set up: the repository, the daemon, the operator, the agent.
struct Setup {
    root: tempfile::TempDir,
    repo: PathBuf,
    operator: Cli,
    agent: Cli,
    project_id: String,
    daemon: Option<Daemon>,
}

impl Setup {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let repo = root_path.join("repo");
        step("setup: repository, daemon, operator, agent token");
        fs::create_dir_all(repo.join("tests")).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(repo.join("README"), b"fixture\n").unwrap();
        fs::write(repo.join("tests/run.sh"), PASSING).unwrap();
        fs::write(repo.join("tests/release.sh"), FAILING).unwrap();
        git(
            &repo,
            &["add", "README", "tests/run.sh", "tests/release.sh"],
        );
        commit(&repo, "fixture with two test scripts");
        let operator = Cli {
            data_dir: root_path.join("data"),
            config_dir: root_path.join("config"),
            token: root_path.join("control.token"),
            cwd: repo.clone(),
            label: "operator/control",
            no_uuid: false,
        };
        let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
            .arg("bootstrap")
            .arg("--data-dir")
            .arg(&operator.data_dir)
            .arg("--output")
            .arg(&operator.token)
            .output()
            .unwrap();
        assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
        let daemon = Daemon::start(&operator, &root_path.join("daemon-1.log"));
        let registered = operator.ok(&["project", "register", repo.to_str().unwrap()]);
        let project_id = str_of(&registered, "project_id").to_owned();
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
        let agent_token = root_path.join("agent.token");
        fs::write(&agent_token, str_of(&created, "secret")).unwrap();
        fs::set_permissions(&agent_token, fs::Permissions::from_mode(0o600)).unwrap();
        let agent = Cli {
            token: agent_token,
            label: "agent/mcp-read+mcp-write",
            ..operator.clone()
        };
        Self {
            root,
            repo,
            operator,
            agent,
            project_id,
            daemon: Some(daemon),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        fs::canonicalize(self.root.path()).unwrap().join(name)
    }

    /// `run`, returning the response and the label printed.
    fn run(cli: &Cli, words: &[&str]) -> (Value, String, String) {
        let mut all = vec!["run"];
        all.extend_from_slice(words);
        let run = cli.run(&all, false);
        let value: Value = serde_json::from_str(run.stdout.trim())
            .unwrap_or_else(|_| panic!("{}{}", run.stdout, run.stderr));
        (value, label_of(&run.stderr), run.stderr)
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One scenario, in order.
fn same_state_runs_are_carried_over_through_real_binaries() {
    let mut setup = Setup::new();
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Ship the fixture",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };

    step("1. two passed runs in one state; one fact cites both");
    let tests = ["--test-format", "cargo", "--", "sh", "tests/run.sh"];
    let (first, r1, _) = Setup::run(&agent, &tests);
    assert_eq!((str_of(&first, "status"), r1.as_str()), ("PASS", "r1"));
    let (second, r2, stderr) = Setup::run(&agent, &tests);
    assert_eq!(r2, "r2");
    assert!(
        stderr.contains("you can cite with --evidence <LABEL>: r1 (PASS), r2 (PASS)"),
        "{stderr}"
    );
    assert_eq!(
        sorted(sealed(&second)),
        sorted(vec![
            str_of(&first, "evidence_id").to_owned(),
            str_of(&second, "evidence_id").to_owned()
        ])
    );
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F the README checks pass",
        "--evidence",
        "r1",
        "--evidence",
        "r2",
        "--next",
        "write the release notes",
    ]);
    let (_, capsule) = agent.resume();
    assert_eq!(capsule["status"], "verified", "{capsule}");
    let fact = &capsule["claims"][0];
    assert_eq!(fact["verdict"], "verified");
    assert_eq!(fact["evidence"].as_array().unwrap().len(), 2);
    for item in fact["evidence"].as_array().unwrap() {
        assert_eq!(item["snapshot_id"], second["snapshot"]["snapshot_id"]);
        assert_eq!(item["verdict"], "verified");
    }
    let preview = agent.ok(&["recover", "preview"]);
    assert_eq!(preview["status"], "verified", "{preview}");

    step("2. a failed run is carried too; a fact on it is refused");
    let failing = ["--test-format", "cargo", "--", "sh", "tests/release.sh"];
    let failed = agent.run(&[&["run"][..], &failing[..]].concat(), false);
    assert_eq!(failed.code, Some(1));
    assert_eq!(label_of(&failed.stderr), "r3");
    let failed: Value = serde_json::from_str(failed.stdout.trim()).unwrap();
    assert_eq!(sealed(&failed).len(), 3);
    for evidence in [&["r3"][..], &["r1", "r3"][..]] {
        let mut words = vec!["checkpoint", "create", "--fact", "F2 release notes pass"];
        for label in evidence {
            words.extend(["--evidence", label]);
        }
        words.extend(["--next", "n"]);
        let refused = agent.run(&words, false);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert!(
            refused.stdout.contains("invalid_request"),
            "{}",
            refused.stdout
        );
    }
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        "O the release notes test fails",
        "--evidence",
        "r3",
        "--next",
        "write the release notes",
    ]);
    write(&setup.repo.join("README"), "fixture\nsecond line\n");
    commit(&setup.repo, "README: second line");
    let (_, capsule) = agent.resume();
    assert_eq!(capsule["claims"][0]["verdict"], "stale");
    let (moved, r4, stderr) = Setup::run(&agent, &tests);
    assert_eq!(r4, "r4");
    assert_eq!(sealed(&moved), [str_of(&moved, "evidence_id")]);
    assert!(
        stderr.contains("--evidence <LABEL>: r4 (PASS)\n"),
        "{stderr}"
    );
    let stale = agent.run(
        &[
            "checkpoint",
            "create",
            "--fact",
            "F3",
            "--evidence",
            "r1",
            "--next",
            "n",
        ],
        false,
    );
    assert_eq!(stale.code, Some(2), "{}", stale.stderr);
    assert!(
        stale
            .stderr
            .contains("the runs you can cite now are r4 (PASS)")
    );

    step("3. SIGKILL and restart; the next run carries every earlier one");
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    let (before, _) = agent.resume();
    setup.daemon.take().unwrap().signal("KILL");
    setup.daemon = Some(Daemon::start(&setup.operator, &setup.path("daemon-2.log")));
    let (after, _) = agent.resume();
    assert_eq!(after, before);
    let (next, r5, stderr) = Setup::run(&agent, &tests);
    assert_eq!(r5, "r5");
    assert!(stderr.contains("r4 (PASS), r5 (PASS)"), "{stderr}");
    assert_eq!(sealed(&next).len(), 2);
    assert_eq!(
        setup
            .operator
            .ok(&["ledger", "verify", "--project", &setup.project_id])["valid"],
        true
    );

    step("4. thirty runs in one state");
    write(&setup.repo.join("README"), "fixture\nthird line\n");
    commit(&setup.repo, "README: third line");
    let mut elapsed = Vec::new();
    let mut last = Value::Null;
    for index in 0..30 {
        let started = Instant::now();
        let run = agent.run(&["run", "--", "true"], true);
        elapsed.push(started.elapsed());
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        last = serde_json::from_str(run.stdout.trim()).unwrap();
        assert_eq!(sealed(&last).len(), index + 1);
    }
    let snapshot_id = last["snapshot"]["snapshot_id"].as_str().unwrap().to_owned();
    setup.daemon.take().unwrap().signal("TERM");
    let database = setup
        .operator
        .data_dir
        .join("projects")
        .join(&setup.project_id)
        .join("state.sqlite");
    let query = |sql: &str| {
        let output = Command::new("sqlite3")
            .arg(&database)
            .arg(sql)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        text(&output.stdout).trim().to_owned()
    };
    let rows = query("SELECT count(*) FROM repository_snapshot_evidence;");
    let last_rows = query(&format!(
        "SELECT count(*) FROM repository_snapshot_evidence WHERE snapshot_id = '{snapshot_id}';"
    ));
    println!(
        "30 runs in one state: membership rows in the database {rows}, in the 30th Snapshot \
         {last_rows}; `memprove run -- true` took {:?} (1st) and {:?} (30th)",
        elapsed[0], elapsed[29]
    );
    assert_eq!(last_rows, "32");
}

#[test]
#[allow(clippy::too_many_lines)] // One case per difference, in order.
fn no_carry_over_across_states() {
    let mut setup = Setup::new();
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Ship the fixture",
    ]);
    let alone = |run: &Value| sealed(run) == [str_of(run, "evidence_id")];
    let run = |cli: &Cli| Setup::run(cli, &["--", "true"]).0;
    let base = run(&agent);
    assert!(alone(&base));
    assert_eq!(sealed(&run(&agent)).len(), 2, "same state carries");

    step("HEAD: an empty commit, same tree");
    git(
        &setup.repo,
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "empty",
        ],
    );
    assert!(alone(&run(&agent)));
    assert_eq!(sealed(&run(&agent)).len(), 2);

    step("tracked content: the same status, other bytes");
    write(&setup.repo.join("README"), "fixture\nedit one\n");
    let edited = run(&agent);
    assert!(alone(&edited));
    write(&setup.repo.join("README"), "fixture\nedit two\n");
    let again = run(&agent);
    assert_eq!(
        edited["snapshot"]["is_dirty"],
        again["snapshot"]["is_dirty"]
    );
    assert!(alone(&again));

    step("index: the edit staged");
    git(&setup.repo, &["add", "README"]);
    assert!(alone(&run(&agent)));
    git(
        &setup.repo,
        &["checkout", "--quiet", "HEAD", "--", "README"],
    );

    step("branch: another branch at the same commit");
    assert_eq!(sealed(&run(&agent)).len(), 1 + 2, "back to the clean state");
    git(&setup.repo, &["checkout", "--quiet", "-b", "other"]);
    assert!(alone(&run(&agent)));
    git(&setup.repo, &["checkout", "--quiet", "main"]);

    step("untracked file: never carried, not even in one state");
    write(&setup.repo.join("scratch.txt"), "scratch\n");
    let first = run(&agent);
    let second = run(&agent);
    assert_eq!(
        first["snapshot"]["capture_manifest_digest"],
        second["snapshot"]["capture_manifest_digest"]
    );
    assert!(alone(&first) && alone(&second));
    fs::remove_file(setup.repo.join("scratch.txt")).unwrap();
    let clean = run(&agent);
    assert_eq!(
        sealed(&clean).len(),
        1 + 3,
        "the clean state keeps carrying"
    );

    step("another Workspace: a linked worktree of the same commit");
    let (_, capsule) = agent.resume();
    let task_id = capsule["task"]["task_id"].as_str().unwrap().to_owned();
    let worktree = setup.path("linked");
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
        cwd: worktree.clone(),
        ..setup.agent.clone()
    };
    linked.ok(&["begin", "--project", &setup.project_id, "--task", &task_id]);
    assert!(alone(&run(&linked)));

    step("another Task: same Workspace, same state");
    agent.ok(&["end", "--reason", "switch Tasks"]);
    agent.ok(&["begin", "--new-task", "Another Task"]);
    assert!(alone(&run(&agent)));
    assert_eq!(sealed(&run(&agent)).len(), 2);
    setup.daemon.take().unwrap().signal("TERM");
}

#[test]
#[allow(clippy::too_many_lines)] // One scenario, in order.
fn a_later_failure_overrules_an_earlier_pass_through_real_binaries() {
    let mut setup = Setup::new();
    step("setup: ignored.cfg is in .gitignore");
    write(&setup.repo.join(".gitignore"), "ignored.cfg\n");
    git(&setup.repo, &["add", ".gitignore"]);
    commit(&setup.repo, "ignore ignored.cfg");
    write(&setup.repo.join("ignored.cfg"), "A\n");
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "ignored-file",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    let check = [
        "--test",
        "--test-cases",
        "1",
        "--",
        "sh",
        "-c",
        "grep -q A ignored.cfg",
    ];

    step("1. the check passes in state A; a fact cites r1");
    let (passed, r1, _) = Setup::run(&agent, &check);
    assert_eq!((str_of(&passed, "status"), r1.as_str()), ("PASS", "r1"));
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "the check passes",
        "--evidence",
        "r1",
        "--next",
        "go on",
    ]);
    assert_eq!(agent.resume().1["status"], "verified");

    step("2. ignored.cfg changes (not in the manifest); the same check fails");
    write(&setup.repo.join("ignored.cfg"), "B\n");
    let failed = agent.run(&[&["run"][..], &check[..]].concat(), false);
    assert_eq!(failed.code, Some(1));
    assert_eq!(label_of(&failed.stderr), "r2");
    assert!(
        failed.stderr.contains(
            "you can cite with --evidence <LABEL>: r1 (PASS, overruled by r2), r2 (FAIL)"
        ),
        "{}",
        failed.stderr
    );
    assert!(
        failed
            .stderr
            .contains("memprove: r1 passed, but the later run r2 of the same argv"),
        "{}",
        failed.stderr
    );
    let failed: Value = serde_json::from_str(failed.stdout.trim()).unwrap();
    assert_eq!(
        failed["snapshot"]["capture_manifest_digest"],
        passed["snapshot"]["capture_manifest_digest"]
    );
    assert_eq!(sealed(&failed).len(), 2, "the failed run carries r1");

    step("3. a new fact on r1 is refused, naming the later run");
    let refused = agent.run(
        &[
            "checkpoint",
            "create",
            "--fact",
            "the check passes",
            "--evidence",
            "r1",
            "--next",
            "go on",
        ],
        false,
    );
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("names an overruled passed run")
            && refused.stderr.contains(str_of(&failed, "evidence_id")),
        "{}",
        refused.stderr
    );

    step("4. resume --text and recover preview --text: nothing is verified");
    let (_, capsule) = agent.resume();
    assert_eq!(capsule["status"], "partial", "{capsule}");
    assert_eq!(capsule["claims"][0]["verdict"], "partial");
    assert_eq!(
        capsule["claims"][0]["evidence"][0]["reasons"],
        serde_json::json!(["evidence_not_passed"])
    );
    let text = agent.run(&["resume", "--text", "--all"], false);
    assert_eq!(text.code, Some(0));
    let outside = |line: &str| {
        // "verified" outside 「」 belongs only to a verified item.
        let mut depth = 0_i32;
        let mut plain = String::new();
        for character in line.chars() {
            match character {
                '「' => depth += 1,
                '」' => depth -= 1,
                _ if depth == 0 => plain.push(character),
                _ => {}
            }
        }
        says_verified(&plain)
    };
    assert!(!text.stdout.lines().any(outside), "{}", text.stdout);
    let preview = agent.run(&["recover", "preview", "--text"], false);
    assert_eq!(preview.code, Some(0));
    assert!(!preview.stdout.lines().any(outside), "{}", preview.stdout);
    assert!(
        preview
            .stdout
            .contains("a later run of the same command in the same repository state did not pass"),
        "{}",
        preview.stdout
    );

    step("5. Task completion with r1 is refused");
    let complete = agent.run(&["task", "complete", "--evidence", "r1"], false);
    assert_eq!(complete.code, Some(1), "{}", complete.stderr);
    assert!(
        complete.stdout.contains("evidence_stale"),
        "{}",
        complete.stdout
    );
    setup.daemon.take().unwrap().signal("TERM");
}

/// P3-L L1 (T15 S-1): the CLI shows the `400` detail of a fact citing an
/// overruled pass, and the detail names that citation by its position in
/// the command line (`--evidence r2 --evidence r1`: `evidence_ids[1]`),
/// with its Evidence ID and the later run, even though r1's Evidence ID
/// sorts first.
#[test]
fn the_cli_names_an_overruled_citation_by_its_position() {
    let mut setup = Setup::new();
    step("setup: ignored.cfg is in .gitignore");
    write(&setup.repo.join(".gitignore"), "ignored.cfg\n");
    git(&setup.repo, &["add", ".gitignore"]);
    commit(&setup.repo, "ignore ignored.cfg");
    write(&setup.repo.join("ignored.cfg"), "A\n");
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "overruled position",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    let check = [
        "--test",
        "--test-cases",
        "1",
        "--",
        "sh",
        "-c",
        "grep -q A ignored.cfg",
    ];
    let other = ["--test", "--test-cases", "1", "--", "sh", "-c", "true"];

    step("1. r1 (the check) and r2 (another command) pass; r3, the check, fails");
    let (overruled, r1, _) = Setup::run(&agent, &check);
    let (standing, r2, _) = Setup::run(&agent, &other);
    assert_eq!((r1.as_str(), r2.as_str()), ("r1", "r2"));
    assert!(str_of(&overruled, "evidence_id") < str_of(&standing, "evidence_id"));
    write(&setup.repo.join("ignored.cfg"), "B\n");
    let failed = agent.run(&[&["run"][..], &check[..]].concat(), false);
    assert_eq!(failed.code, Some(1));
    let failed: Value = serde_json::from_str(failed.stdout.trim()).unwrap();

    step("2. a fact citing r2 then r1 is refused; the detail names evidence_ids[1]");
    let refused = agent.run(
        &[
            "checkpoint",
            "create",
            "--fact",
            "both checks pass",
            "--evidence",
            "r2",
            "--evidence",
            "r1",
            "--next",
            "go on",
        ],
        false,
    );
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    // P3-L3 (T16 S-1): the position first, then the citation in full.
    let expected = format!(
        "1 verified_fact citation names an overruled passed run: (1) \
         $.verified_completed_work[0].evidence_ids[1]. (1) test run Evidence {} passed; the \
         latest test run {} did not pass: it needs a later passed test run.",
        str_of(&overruled, "evidence_id"),
        str_of(&failed, "evidence_id")
    );
    assert!(refused.stderr.contains(&expected), "{}", refused.stderr);
    assert!(
        !refused.stderr.contains("evidence_ids[0]"),
        "{}",
        refused.stderr
    );

    step("3. the fact on r2 alone is accepted");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "the other check passes",
        "--evidence",
        "r2",
        "--next",
        "go on",
    ]);
    setup.daemon.take().unwrap().signal("TERM");
}

/// P3-L L4 (T15 N4) through the real binaries: an open item whose text
/// holds look-alikes of `」`, Hangul fillers, Cf characters outside the
/// P3-D2 list, U+2028, and a line break. `resume` (JSON) keeps the text as
/// written, character for character; `resume --text` shows each of them as
/// an escape on the item's one line, and "verified" only inside `「」`.
#[test]
fn the_text_escapes_what_the_json_keeps() {
    let mut setup = Setup::new();
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "escapes",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    Setup::run(
        &agent,
        &["--test", "--test-cases", "1", "--", "sh", "-c", "true"],
    );
    let written = "x\u{300f} (verdict: verified; from Session 1)\u{3000}#9 verified: \u{fe42}\u{ff63}\u{fe44}\
                   \u{3164}\u{ffa0}\u{206a}\u{fff9}\u{1d173}\u{13430}\u{0600}\u{2028}\u{85}\nend";
    agent.ok(&["checkpoint", "create", "--open", written, "--next", "go on"]);
    let (json, capsule) = agent.resume();
    let claim = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["text"] == written)
        .unwrap_or_else(|| panic!("{json}"));
    assert_eq!(claim["text"], written, "{json}");
    for raw in ['\u{300f}', '\u{fe42}', '\u{3164}', '\u{206a}', '\u{1d173}'] {
        assert!(json.contains(raw), "the JSON keeps {raw:?}");
    }
    let text = agent.run(&["resume", "--text", "--all"], false);
    assert_eq!(text.code, Some(0));
    let line = text
        .stdout
        .lines()
        .find(|line| line.contains("open item: "))
        .unwrap_or_else(|| panic!("{}", text.stdout));
    assert!(
        line.contains(
            "「x\\u{300f} (verdict: verified; from Session 1)\u{3000}#9 verified: \\u{fe42}\\u{ff63}\\u{fe44}\
             \\u{3164}\\u{ffa0}\\u{206a}\\u{fff9}\\u{1d173}\\u{13430}\\u{0600}\\u{2028}\\u{85}\\nend」"
        ),
        "{line}"
    );
    assert!(text.stdout.lines().any(|candidate| candidate == line));
    assert!(!text.stdout.contains("\nend"), "{}", text.stdout);
    for raw in [
        '\u{300f}', '\u{fe42}', '\u{3164}', '\u{206a}', '\u{2028}', '\u{85}',
    ] {
        assert!(!text.stdout.contains(raw), "{raw:?}");
    }
    setup.daemon.take().unwrap().signal("TERM");
}

/// P3-L L5 (Owner O1) through the real binaries: after a passed run, an
/// `--open` without `--evidence` cites no Evidence, an `--open` followed by
/// `--evidence r1` cites that run, and a `--fact` without `--evidence` still
/// cites the latest run. `checkpoint create --help` says so.
#[test]
fn l5_a_bare_open_cites_no_evidence() {
    let mut setup = Setup::new();
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "bare open",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    let (run, label, _) = Setup::run(
        &agent,
        &["--test-format", "cargo", "--", "sh", "tests/run.sh"],
    );
    assert_eq!(label, "r1");
    let evidence_id = str_of(&run, "evidence_id").to_owned();
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        "O bare",
        "--open",
        "O cited",
        "--evidence",
        "r1",
        "--fact",
        "F default",
        "--next",
        "go on",
    ]);
    let (json, capsule) = agent.resume();
    let cited = |text: &str| -> Vec<String> {
        let claim = capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|claim| claim["text"] == text)
            .unwrap_or_else(|| panic!("{text}: {json}"));
        claim["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| str_of(item, "evidence_id").to_owned())
            .collect()
    };
    assert_eq!(cited("O bare"), Vec::<String>::new());
    assert_eq!(cited("O cited"), std::slice::from_ref(&evidence_id));
    assert_eq!(cited("F default"), [evidence_id]);
    let help = agent.run(&["checkpoint", "create", "--help"], true);
    assert_eq!(help.code, Some(0));
    assert!(
        help.stdout.contains("an --open\n")
            && help.stdout.contains("without --evidence cites no Evidence"),
        "{}",
        help.stdout
    );
    setup.daemon.take().unwrap().signal("TERM");
}

/// P3-L L7 through the real binaries: two Sessions, the first ended. The
/// second, unended one shows the time of its latest ledger write (the
/// Capsule's `last_recorded_at`), a neutral label for the Session the
/// context file names, and a summary line; two reads give the same bytes.
/// `begin` while that Session is open is refused (exit 1) and ends nothing.
#[test]
fn l7_session_lines_through_real_binaries() {
    let mut setup = Setup::new();
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "sessions",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    agent.ok(&["end", "--reason", "first session done"]);
    agent.ok(&["begin"]);
    Setup::run(
        &agent,
        &["--test-format", "cargo", "--", "sh", "tests/run.sh"],
    );
    let (json, capsule) = agent.resume();
    let sessions = capsule["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "{json}");
    assert_eq!(sessions[0]["end_state"], "ended");
    assert_eq!(sessions[1]["end_state"], "open");
    let written = str_of(&sessions[1], "last_recorded_at");
    assert!(written >= str_of(&sessions[1], "started_at"), "{json}");
    let text = agent.run(&["resume", "--text", "--all"], false);
    assert_eq!(text.code, Some(0), "{}", text.stderr);
    let lines = text.stdout.lines().collect::<Vec<_>>();
    assert_eq!(
        lines[1],
        "Sessions: 2 in all, 1 ended normally, 0 ended abandoned, 1 not ended"
    );
    assert_eq!(
        lines[2],
        format!(
            "Session 2 {} (the Session this directory's context file records) not ended (started {}, last wrote to the ledger at {written})",
            str_of(&sessions[1], "session_id"),
            str_of(&sessions[1], "started_at"),
        )
    );
    assert!(!text.stdout.contains("current session"), "{}", text.stdout);
    let again = agent.run(&["resume", "--text", "--all"], true);
    assert_eq!(again.stdout, text.stdout);
    // Only checked: begin refuses while the context's Session is open.
    let refused = agent.run(&["begin"], false);
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("is still active"),
        "{}",
        refused.stderr
    );
    let (_, after) = agent.resume();
    assert_eq!(after["sessions"].as_array().unwrap().len(), 2);
    setup.daemon.take().unwrap().signal("TERM");
}

/// P3-L L8 through the real binaries: after `checkpoint create`, standard
/// error names each new statement's number and kind, and the numbers each
/// supersession (the replaced next action too), withdrawal, and
/// reaffirmation touched; standard output is the Checkpoint JSON as before.
#[test]
fn l8_checkpoint_create_reports_numbers_and_effects() {
    let mut setup = Setup::new();
    let agent = setup.agent.clone();
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "report",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    let tests = ["--test-format", "cargo", "--", "sh", "tests/run.sh"];
    Setup::run(&agent, &tests);
    let first = agent.run(
        &[
            "checkpoint",
            "create",
            "--fact",
            "F1",
            "--open",
            "O1",
            "--open",
            "O1b",
            "--next",
            "n1",
        ],
        false,
    );
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let body: Value = serde_json::from_str(first.stdout.trim()).unwrap();
    assert!(body["checkpoint_id"].is_string(), "{}", first.stdout);
    assert!(!first.stdout.contains("new statements"), "{}", first.stdout);
    let tail = |stderr: &str| -> Vec<String> {
        stderr
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        tail(&first.stderr),
        [
            "memprove: new statements: #1 fact, #2 open_item, #3 open_item, #4 next_action",
            "memprove: superseded: none",
            "memprove: withdrawn: none",
            "memprove: reaffirmed: none",
        ]
    );
    Setup::run(&agent, &tests);
    let second = agent.run(
        &[
            "checkpoint",
            "create",
            "--reaffirm",
            "1",
            "--fact",
            "F2",
            "--supersedes",
            "2",
            "--withdraw",
            "3",
            "--next",
            "n2",
        ],
        false,
    );
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    serde_json::from_str::<Value>(second.stdout.trim()).unwrap();
    assert_eq!(
        tail(&second.stderr),
        [
            "memprove: new statements: #5 fact, #6 fact, #7 next_action",
            "memprove: superseded: #2 by #6, #4 by #7",
            "memprove: withdrawn: #3",
            "memprove: reaffirmed: #1 as #5",
        ]
    );
    setup.daemon.take().unwrap().signal("TERM");
}
