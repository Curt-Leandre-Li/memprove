//! P3-C acceptance (ADR 0013 D1/D2): supersession, withdrawal, and
//! Decisions through the real `memprove` CLI and the real `memproved`
//! over a temporary Git repository. The operator holds the bootstrap
//! control token; the agent uses one token with only `mcp-read` and
//! `mcp-write`, and the current context of the Workspace. As in P2-H, no
//! agent command line but the first `begin --project` carries a UUID
//! (asserted): claims are named by their numbers.
//!
//! 1. Session 1 at X: `run` (PASS), `checkpoint create` with fact F1 (#1),
//!    open item O1 (#2), and a next action (#3); `decide` D1 (#4); `end`.
//! 2. Session 2: commit Y, `run` (PASS), `checkpoint create` with F2 (#5)
//!    `--supersedes 2`; `run` a failing test, `checkpoint create --open`
//!    O2 (#7) citing it; Session 2 is not ended.
//! 3. Commit Z. `resume --text`: F1 and F2 stale, with changed files and
//!    the rerun list; O1 superseded by F2; D1 in force; O2 open; Session 2
//!    not ended yet, marked as the current Session.
//! 4. `decide` D2 `--supersedes 4`: D1 is superseded.
//! 5. `ledger verify` is valid; `event list` (control) passes the response
//!    validator with `claim.superseded` in it; after SIGKILL and a restart
//!    `resume` prints the same bytes.
//! 6. One Evidence row changed with `sqlite3`: the CLI refuses to resolve
//!    claim numbers against the tampered Capsule and sends nothing.
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

    fn resume_text(&self) -> String {
        let run = self.run(&["resume", "--text", "--all"], false);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        run.stdout
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

/// `(number, kind, standing, verdict, superseded_by numbers)` of each claim.
fn claims(capsule: &Value) -> Vec<(u64, String, String, String, Vec<u64>)> {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|claim| {
            (
                claim["number"].as_u64().unwrap(),
                str_of(claim, "kind").to_owned(),
                str_of(claim, "standing").to_owned(),
                str_of(claim, "verdict").to_owned(),
                claim["superseded_by"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|by| by["number"].as_u64().unwrap())
                    .collect(),
            )
        })
        .collect()
}

fn claim(capsule: &Value, number: u64) -> &Value {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["number"] == number)
        .unwrap()
}

fn events(operator: &Cli, project_id: &str) -> Vec<Value> {
    let page = operator.run(
        &["event", "list", "--project", project_id, "--limit", "500"],
        true,
    );
    assert_eq!(page.code, Some(0), "{}{}", page.stdout, page.stderr);
    serde_json::from_str::<Value>(page.stdout.trim()).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
#[allow(clippy::too_many_lines)] // One acceptance scenario, 1 through 6, in order.
fn claim_edges_and_decisions_through_real_binaries() {
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
    let x = commit(&repo, "fixture with two test scripts");
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
    println!("$ memproved bootstrap --data-dir <DATA> --output <CONTROL_TOKEN_FILE>\n[exit 0]");
    let daemon = Daemon::start(&operator, &root_path.join("daemon-1.log"));
    let registered = operator.ok(&["project", "register", repo.to_str().unwrap()]);
    let project_id = str_of(&registered, "project_id").to_owned();
    let words = [
        "token",
        "create",
        "--label",
        "agent",
        "--scope",
        "mcp-read",
        "--scope",
        "mcp-write",
    ];
    let created = operator.run(&words, true);
    assert_eq!(created.code, Some(0), "{}", created.stderr);
    let created: Value = serde_json::from_str(created.stdout.trim()).unwrap();
    let secret = str_of(&created, "secret").to_owned();
    let mut shown = created.clone();
    shown["secret"] = Value::String("<redacted>".to_owned());
    println!(
        "[operator/control] $ ct {}\n{shown}\n[exit 0]",
        words.join(" ")
    );
    let agent_token = root_path.join("agent.token");
    fs::write(&agent_token, &secret).unwrap();
    fs::set_permissions(&agent_token, fs::Permissions::from_mode(0o600)).unwrap();
    let agent = Cli {
        token: agent_token,
        label: "agent/mcp-read+mcp-write",
        ..operator.clone()
    };

    step("1. Session 1 at X: run, F1 + O1, decide D1, end");
    // P2-H: the first `begin` in a Workspace names the Project; no later
    // agent command line carries a UUID.
    agent.ok(&[
        "begin",
        "--project",
        &project_id,
        "--new-task",
        "Ship the fixture",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    let run_1 = agent.ok(&["run", "--test-format", "cargo", "--", "sh", "tests/run.sh"]);
    assert_eq!(str_of(&run_1, "status"), "PASS");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F1 README check passes",
        "--open",
        "O1 README lacks a second line",
        // P3-L L5: an --open cites a run only when told to.
        "--evidence",
        "last",
        "--next",
        "add the second README line",
    ]);
    let d1 = agent.ok(&[
        "decide",
        "D1 keep the README plain text",
        "--rationale",
        "no renderer is assumed",
    ]);
    assert_eq!(str_of(&d1, "content"), "D1 keep the README plain text");
    agent.ok(&["end", "--reason", "session 1 done"]);

    step("2. Session 2: commit Y, run, F2 supersedes #2; failing run, O2; no end");
    agent.ok(&["begin"]);
    write(&repo.join("README"), "fixture\nsecond line\n");
    let y = commit(&repo, "README: second line");
    let run_2 = agent.ok(&["run", "--test-format", "cargo", "--", "sh", "tests/run.sh"]);
    assert_eq!(str_of(&run_2, "status"), "PASS");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F2 README has its second line",
        "--supersedes",
        "2",
        "--next",
        "write the release notes",
    ]);
    let failed = agent.run(
        &[
            "run",
            "--test-format",
            "cargo",
            "--",
            "sh",
            "tests/release.sh",
        ],
        false,
    );
    assert_eq!(failed.code, Some(1), "{}", failed.stderr);
    let failed: Value = serde_json::from_str(failed.stdout.trim()).unwrap();
    assert_eq!(str_of(&failed, "status"), "FAIL");
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        "O2 release notes test fails",
        "--evidence",
        "last",
        "--next",
        "write the release notes",
    ]);

    step("3. commit Z; resume");
    write(&repo.join("README"), "fixture\nsecond line\nthird line\n");
    let z = commit(&repo, "README: third line");
    let (_, capsule) = agent.resume();
    let s = |value: &str| value.to_owned();
    assert_eq!(
        claims(&capsule),
        [
            (1, s("fact"), s("active"), s("stale"), vec![]),
            // O1 cites run 1 (PASS at X): stale, like F1.
            (2, s("open_item"), s("superseded"), s("stale"), vec![5]),
            (3, s("next_action"), s("superseded"), s("partial"), vec![6]),
            (4, s("decision"), s("active"), s("partial"), vec![]),
            (5, s("fact"), s("active"), s("stale"), vec![]),
            (6, s("next_action"), s("superseded"), s("partial"), vec![8]),
            // O2 cites the failed run at Y: not fresh, so stale (P2-F).
            (7, s("open_item"), s("active"), s("stale"), vec![]),
            (8, s("next_action"), s("active"), s("partial"), vec![]),
        ]
    );
    assert_eq!(claim(&capsule, 1)["evidence"][0]["base_commit"], x.as_str());
    assert_eq!(claim(&capsule, 5)["evidence"][0]["base_commit"], y.as_str());
    assert_eq!(capsule["status"], "stale");
    assert_eq!(capsule["bindings"]["probe"]["head_oid"], z.as_str());
    let rerun = capsule["rerun"].as_array().unwrap();
    assert_eq!(rerun.len(), 1);
    assert_eq!(rerun[0]["argv"], serde_json::json!(["sh", "tests/run.sh"]));
    assert_eq!(rerun[0]["claims"], serde_json::json!([1, 5]));
    let sessions = capsule["sessions"].as_array().unwrap();
    assert_eq!(
        sessions
            .iter()
            .map(|session| str_of(session, "end_state"))
            .collect::<Vec<_>>(),
        ["ended", "open"]
    );
    let first_text = agent.resume_text();
    // P3-C2: statement and Decision texts are quoted in 「」.
    for expected in [
        "#1 fact stale: 「F1 README check passes」",
        "#5 fact stale: 「F2 README has its second line」",
        "changed files",
        "README",
        "Rerun: 「sh tests/run.sh」",
        "#2 superseded by #5: 「O1 README lacks a second line」",
        "#4 decision: 「D1 keep the README plain text」 (active, not superseded",
        "#7 open item: 「O2 release notes test fails」",
        // P3-L L7: a neutral label for the context's Session.
        "(the Session this directory's context file records) not ended",
    ] {
        assert!(first_text.contains(expected), "{expected}\n{first_text}");
    }
    assert!(!says_verified(&first_text));
    assert!(!first_text.contains("did not end normally"));

    step("4. decide D2 --supersedes 4: D1 is superseded");
    let d2 = agent.ok(&[
        "decide",
        "D2 render the README as Markdown",
        "--supersedes",
        "4",
    ]);
    assert_eq!(
        d2["supersedes"],
        serde_json::json!([format!("decision/{}", str_of(&d1, "decision_id"))])
    );
    let (_, capsule) = agent.resume();
    assert_eq!(
        claims(&capsule)[3],
        (4, s("decision"), s("superseded"), s("partial"), vec![9])
    );
    assert_eq!(
        claims(&capsule)[8],
        (9, s("decision"), s("active"), s("partial"), vec![])
    );
    let second_text = agent.resume_text();
    assert!(
        second_text.contains("#4 superseded by #9: 「D1 keep the README plain text」"),
        "{second_text}"
    );
    assert!(
        second_text
            .contains("#9 decision: 「D2 render the README as Markdown」 (active, not superseded")
    );

    step("5. ledger verify, event list (control), SIGKILL and restart");
    let verified = operator.ok(&["ledger", "verify", "--project", &project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    let ledger = events(&operator, &project_id);
    let edges = ledger
        .iter()
        .filter(|event| event["event_type"] == "claim.superseded")
        .collect::<Vec<_>>();
    let decisions = ledger
        .iter()
        .filter(|event| event["event_type"] == "decision.recorded")
        .collect::<Vec<_>>();
    assert_eq!((edges.len(), decisions.len()), (2, 2));
    for event in edges.iter().chain(&decisions) {
        assert_eq!(event["producer"]["name"], "continuityd");
    }
    println!(
        "claim.superseded payload: {}\ndecision.recorded payload: {}",
        edges[0]["payload"], decisions[0]["payload"]
    );
    let (bytes_before, _) = agent.resume();
    let text_before = agent.resume_text();
    daemon.signal("KILL");
    let daemon = Daemon::start(&operator, &root_path.join("daemon-2.log"));
    let (bytes_after, _) = agent.resume();
    assert_eq!(bytes_after, bytes_before);
    assert_eq!(agent.resume_text(), text_before);
    assert_eq!(
        operator.ok(&["ledger", "verify", "--project", &project_id])["valid"],
        true
    );

    step("6. one Evidence row changed: numbers are not resolved, nothing is sent");
    daemon.signal("TERM");
    let database = operator
        .data_dir
        .join("projects")
        .join(&project_id)
        .join("state.sqlite");
    let evidence_1 = str_of(&run_1, "evidence_id");
    let tamper = format!(
        "DROP TRIGGER evidence_reject_update; \
         UPDATE evidence SET payload = replace(payload, '3 passed', '9 passed') \
         WHERE evidence_id = '{evidence_1}'; SELECT changes();"
    );
    println!("$ sqlite3 <STATE_DB> {tamper:?}");
    let output = Command::new("sqlite3")
        .arg(&database)
        .arg(&tamper)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout).trim(), "1");
    let daemon = Daemon::start(&operator, &root_path.join("daemon-3.log"));
    let before = events(&operator, &project_id).len();
    let (_, tampered) = agent.resume();
    assert_eq!(tampered["status"], "tampered");
    for words in [
        &["decide", "D3 anything", "--supersedes", "9"][..],
        &[
            "checkpoint",
            "create",
            "--open",
            "O3",
            "--supersedes",
            "7",
            "--next",
            "n",
        ][..],
        &["checkpoint", "create", "--withdraw", "#7", "--next", "n"][..],
    ] {
        let refused = agent.run(words, false);
        assert_eq!(refused.code, Some(1), "{words:?}");
        assert!(
            refused.stderr.contains("the Task Capsule is tampered"),
            "{}",
            refused.stderr
        );
        assert!(refused.stdout.trim().is_empty(), "{}", refused.stdout);
    }
    assert_eq!(events(&operator, &project_id).len(), before);
    daemon.signal("TERM");
}
