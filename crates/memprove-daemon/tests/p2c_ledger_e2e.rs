//! P2-C demonstrations: the real `memprove` CLI against the real
//! `memproved` over a temporary Git repository. The operator holds the
//! bootstrap control token; every agent step uses one token that carries
//! only `mcp-read` and `mcp-write`.
//!
//! `run_digests_unavailable_runs_and_ledger_verification`: `memprove run
//! --digest-only` keeps the digests of a command's output but none of its
//! text, anywhere in the database files; a program that cannot be started
//! is recorded as `UNAVAILABLE` and backs no "tests pass"; `memprove
//! ledger verify` passes with the agent's `mcp-read` scope, and after
//! `SIGKILL` it still passes and reports the same chain head.
//!
//! `task_completes_on_verified_evidence_and_survives_sigkill`: Task →
//! Session continuing it → `memprove run` the tests → Checkpoint →
//! `task transition --to completed --evidence` → `ledger verify` passes →
//! the Event list shows every state change of the Task → `SIGKILL` →
//! restart, unchanged. A second Task whose file changed after its run is
//! refused with `409 evidence_stale`.
//!
//! `deleted_and_forged_records_are_found_through_real_binaries` (P2-K, T11
//! S-1 and S-2): with the daemon stopped, the latest Checkpoint's rows are
//! deleted with `sqlite3`; after a restart `ledger verify` answers 200 with
//! `valid = false` naming that Checkpoint and its Event, and the Preview is
//! `tampered` on that Checkpoint instead of falling back to the earlier one.
//! A failed test run is then forged as `PASS` with every digest recomputed
//! by the domain's own constructors; it reads back as `PASS`, but `ledger
//! verify` names it and completing the Task on it is `409 evidence_stale`.
//!
//! Run with `--nocapture` to print the transcript; token secrets are never
//! printed. `memprove` is taken from the same target directory as
//! `memproved`, so run this through `cargo test --workspace`.

use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_domain::{
    CanonicalJson, Evidence, EvidenceId, EvidencePayload, ProjectId, RepositorySnapshot,
};
use memprove_store_sqlite::{RegistryStore, StateStore};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);
const AGENT_ID: &str = "018f47a0-0000-7000-8000-00000000a6e1";
const TEST_SCRIPT: &str = "#!/bin/sh
# Three checks over the fixture repository.
set -e
test -f README && echo 'test readme_exists ... ok'
grep -q fixture README && echo 'test readme_content ... ok'
test \"$(git rev-parse --is-inside-work-tree)\" = true && echo 'test inside_worktree ... ok'
echo 'test result: ok. 3 passed; 0 failed'
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
    println!("\n==== {title} ====");
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    println!("$ git -C {} {}", directory.display(), arguments.join(" "));
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

fn git_repository(path: &Path) {
    fs::create_dir_all(path.join("tests")).unwrap();
    git(path, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(path.join("README"), b"fixture\n").unwrap();
    fs::write(path.join("tests/run.sh"), TEST_SCRIPT).unwrap();
    git(path, &["add", "README", "tests/run.sh"]);
    git(
        path,
        &[
            "-c",
            "user.name=memprove Test",
            "-c",
            "user.email=memprove@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture with a test script",
        ],
    );
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct Cli {
    data_dir: PathBuf,
    token: PathBuf,
    /// Working directory of the CLI, and so of the command `run` executes.
    cwd: PathBuf,
    label: &'static str,
}

impl Cli {
    fn with_token(&self, token: &Path, label: &'static str) -> Self {
        Self {
            token: token.to_path_buf(),
            label,
            ..self.clone()
        }
    }

    fn run(&self, words: &[&str], quiet: bool) -> Run {
        let output = isolated(cli_binary())
            .current_dir(&self.cwd)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token-file")
            .arg(&self.token)
            .env_remove("MEMPROVE_DATA_DIR")
            .env_remove("MEMPROVE_TOKEN")
            .env_remove("MEMPROVE_TOKEN_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(words)
            .output()
            .unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        if !quiet {
            println!(
                "[{}] $ memprove {}\n{}{}[exit {}]",
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

    fn problem(&self, words: &[&str], status: u16, code: &str) -> Value {
        let run = self.run(words, false);
        assert_eq!(run.code, Some(1), "{words:?}: {}{}", run.stdout, run.stderr);
        assert!(
            run.stderr.contains(&format!("failed: {status} {code}")),
            "{}",
            run.stderr
        );
        serde_json::from_str(run.stdout.trim()).unwrap()
    }
}

fn str_of<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name]
        .as_str()
        .unwrap_or_else(|| panic!("{name} is not a string in {value}"))
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(cli: &Cli, log: &Path) -> Self {
        let child = isolated(env!("CARGO_BIN_EXE_memproved"))
            .arg("--data-dir")
            .arg(&cli.data_dir)
            .env_remove("MEMPROVE_DATA_DIR")
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
        cli.ok(&["health", "ready"]);
        daemon
    }

    fn kill(mut self) {
        println!("$ kill -KILL {}", self.child.id());
        let sent = Command::new("kill")
            .arg("-KILL")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = self.child.wait().unwrap();
        println!("memproved exited: {status}");
        assert_eq!(status.signal(), Some(9));
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

/// The bootstrap operator, the registered Project, and an agent token that
/// carries exactly `mcp-read` and `mcp-write`.
struct Setup {
    root: tempfile::TempDir,
    repo: PathBuf,
    operator: Cli,
    agent: Cli,
    project_id: String,
    daemon: Option<Daemon>,
}

fn setup() -> Setup {
    let root = tempfile::tempdir().unwrap();
    let repo = fs::canonicalize(root.path()).unwrap().join("repo");
    git_repository(&repo);
    let operator = Cli {
        data_dir: root.path().join("data"),
        token: root.path().join("control.token"),
        cwd: repo.clone(),
        label: "operator/control",
    };
    println!(
        "$ memproved bootstrap --data-dir <DATA> --output {}",
        operator.token.display()
    );
    let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(&operator.data_dir)
        .arg("--output")
        .arg(&operator.token)
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    println!("[exit 0] (token written to the 0600 file, not printed)");
    let daemon = Daemon::start(&operator, &root.path().join("daemon-1.log"));
    let project_id = str_of(
        &operator.ok(&["project", "register", repo.to_str().unwrap()]),
        "project_id",
    )
    .to_owned();
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
    println!(
        "[operator/control] $ memprove token create --label agent --scope mcp-read --scope \
         mcp-write\n(secret <redacted>)\n[exit 0]"
    );
    let created: Value = serde_json::from_str(created.stdout.trim()).unwrap();
    let agent_token = root.path().join("agent.token");
    fs::write(&agent_token, str_of(&created, "secret")).unwrap();
    fs::set_permissions(&agent_token, fs::Permissions::from_mode(0o600)).unwrap();
    let agent = operator.with_token(&agent_token, "agent/mcp-read+mcp-write");
    Setup {
        root,
        repo,
        operator,
        agent,
        project_id,
        daemon: Some(daemon),
    }
}

/// Every file of the Project's database family (`state.sqlite`, `-wal`,
/// `-shm`, `-journal`) that exists.
fn database_files(data_dir: &Path, project_id: &str) -> Vec<PathBuf> {
    let directory = data_dir.join("projects").join(project_id);
    fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("state.sqlite"))
        })
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn words(owned: &[String]) -> Vec<&str> {
    owned.iter().map(String::as_str).collect()
}

/// The lowercase hex SHA-256 of `bytes`, as an Evidence payload holds it.
fn wire_sha256(bytes: &[u8]) -> String {
    memprove_domain::Sha256Digest::of_bytes(bytes)
        .as_str()
        .to_owned()
}

#[test]
#[allow(clippy::too_many_lines)] // One demonstration, in order.
fn run_digests_unavailable_runs_and_ledger_verification() {
    let mut setup = setup();
    let (agent, operator) = (setup.agent.clone(), setup.operator.clone());
    let repo_arg = setup.repo.to_str().unwrap().to_owned();

    step("agent: Workspace, Task, Session");
    let workspace_id = str_of(
        &agent.ok(&[
            "workspace",
            "resolve",
            "--project",
            &setup.project_id,
            &repo_arg,
        ]),
        "workspace_id",
    )
    .to_owned();
    let task_id = str_of(
        &agent.ok(&[
            "task",
            "create",
            "--project",
            &setup.project_id,
            "--title",
            "Keep secrets out of the ledger",
            "--workspace",
            &workspace_id,
        ]),
        "task_id",
    )
    .to_owned();
    let session_id = str_of(
        &agent.ok(&[
            "session",
            "create",
            "--workspace",
            &workspace_id,
            "--agent",
            AGENT_ID,
            "--task",
            &task_id,
        ]),
        "session_id",
    )
    .to_owned();
    let target = |extra: &[&str]| {
        let mut words = vec![
            "run",
            "--session",
            session_id.as_str(),
            "--workspace",
            workspace_id.as_str(),
            "--task",
            task_id.as_str(),
        ];
        words.extend_from_slice(extra);
        words.into_iter().map(str::to_owned).collect::<Vec<_>>()
    };

    step("agent: run --digest-only keeps the digests, not the text");
    // The argv is recorded by design, so the output is assembled from
    // fragments: only the output, never the argv, holds the joined text.
    let secret_out = "stdout-secret-7f3a9c";
    let secret_err = "stderr-secret-51be20";
    let script =
        "printf '%s-%s\\n' stdout-secret 7f3a9c; printf '%s-%s\\n' stderr-secret 51be20 >&2";
    let digest_only = agent.ok(&words(&target(&[
        "--digest-only",
        "--",
        "sh",
        "-c",
        script,
    ])));
    assert_eq!(str_of(&digest_only, "status"), "PASS");
    assert_eq!(digest_only["payload"]["stdout"], "");
    assert_eq!(digest_only["payload"]["stderr"], "");
    assert_eq!(
        str_of(&digest_only["payload"], "stdout_sha256"),
        wire_sha256(format!("{secret_out}\n").as_bytes())
    );
    assert_eq!(
        str_of(&digest_only["payload"], "stderr_sha256"),
        wire_sha256(format!("{secret_err}\n").as_bytes())
    );
    let digest_only_id = str_of(&digest_only, "evidence_id").to_owned();
    // The default still keeps the tail.
    let kept = agent.ok(&words(&target(&["--", "sh", "-c", "echo visible-tail"])));
    assert_eq!(kept["payload"]["stdout"], "visible-tail\n");

    step("agent: a program that cannot be started is recorded UNAVAILABLE");
    // P2-D1: `run` exits 125 when the command could not be started; the
    // UNAVAILABLE result is still recorded and printed.
    let missing = agent.run(
        &words(&target(&[
            "--test",
            "--test-cases",
            "3",
            "--",
            "/nonexistent/memprove-tool",
        ])),
        false,
    );
    assert_eq!(missing.code, Some(125), "{}", missing.stderr);
    let missing: Value = serde_json::from_str(missing.stdout.trim()).unwrap();
    assert_eq!(str_of(&missing, "status"), "UNAVAILABLE");
    assert_eq!(missing["payload"]["exit_code"], -1);
    assert_eq!(missing["payload"]["passed"], false);
    assert!(!str_of(&missing["payload"], "launch_error").is_empty());
    let request = setup.root.path().join("checkpoint-unavailable.json");
    fs::write(
        &request,
        format!(
            r#"{{"schema_version":1,"workspace_id":"{workspace_id}","task_id":"{task_id}",
                "session_id":"{session_id}","snapshot_id":"{}",
                "objective":"Keep secrets out of the ledger",
                "verified_completed_work":[{{"classification":"verified_fact",
                    "text":"tests pass","evidence_ids":["{}"]}}],
                "open_items":[],
                "next_action":{{"classification":"unknown","text":"install the tool"}}}}"#,
            str_of(&missing["snapshot"], "snapshot_id"),
            str_of(&missing, "evidence_id")
        ),
    )
    .unwrap();
    agent.problem(
        &["checkpoint", "create", request.to_str().unwrap()],
        400,
        "invalid_request",
    );

    step("agent: ledger verify with mcp-read");
    let verified = agent.ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    assert_eq!(verified["first_failure"], Value::Null);
    // task.created, session.started, task.session_linked, and three
    // command.completed.
    assert_eq!(verified["checked"]["events"], 6);
    // Three captures of git_head, git_worktree, and one command each.
    assert_eq!(verified["checked"]["evidence"], 9);
    assert_eq!(verified["checked"]["snapshots"], 3);
    let head = str_of(&verified, "head_event_sha256").to_owned();

    step("SIGKILL; no output text in any database file; restart");
    setup.daemon.take().unwrap().kill();
    let files = database_files(&operator.data_dir, &setup.project_id);
    assert!(!files.is_empty());
    for file in &files {
        let bytes = fs::read(file).unwrap();
        for secret in [secret_out, secret_err] {
            assert!(
                !contains(&bytes, secret),
                "{} holds {secret}",
                file.display()
            );
        }
    }
    println!(
        "scanned {} database file(s): neither {secret_out} nor {secret_err} is stored",
        files.len()
    );
    let _daemon = Daemon::start(&operator, &setup.root.path().join("daemon-2.log"));
    let again = operator.ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(again["valid"], true);
    assert_eq!(str_of(&again, "head_event_sha256"), head);
    let evidence = operator.ok(&["evidence", "get", &digest_only_id]);
    assert_eq!(evidence["payload"]["stdout"], "");
    assert_eq!(
        evidence["payload"]["stdout_sha256"],
        digest_only["payload"]["stdout_sha256"]
    );
}

/// One agent Session continuing `task_id`: run the fixture tests and record
/// a Checkpoint "tests pass" citing them. Returns the Session, the Evidence,
/// and the Checkpoint.
fn tested_session(
    agent: &Cli,
    requests: &Path,
    workspace_id: &str,
    task_id: &str,
    name: &str,
) -> (String, String, String) {
    let session_id = str_of(
        &agent.ok(&[
            "session",
            "create",
            "--workspace",
            workspace_id,
            "--agent",
            AGENT_ID,
            "--task",
            task_id,
        ]),
        "session_id",
    )
    .to_owned();
    let passed = agent.ok(&[
        "run",
        "--session",
        &session_id,
        "--workspace",
        workspace_id,
        "--task",
        task_id,
        "--test",
        "--test-cases",
        "3",
        "--test-name",
        "fixture",
        "--",
        "sh",
        "tests/run.sh",
    ]);
    assert_eq!(str_of(&passed, "status"), "PASS");
    let evidence_id = str_of(&passed, "evidence_id").to_owned();
    let snapshot_id = str_of(&passed["snapshot"], "snapshot_id");
    let request = requests.join(format!("{name}.json"));
    let body = format!(
        r#"{{"schema_version":1,"workspace_id":"{workspace_id}","task_id":"{task_id}",
            "session_id":"{session_id}","snapshot_id":"{snapshot_id}",
            "objective":"Make the fixture tests pass",
            "verified_completed_work":[{{"classification":"verified_fact",
                "text":"tests pass","evidence_ids":["{evidence_id}"]}}],
            "open_items":[],
            "next_action":{{"classification":"recommendation","text":"complete the Task"}}}}"#
    );
    println!("$ cat > {} <<'EOF'\n{body}\nEOF", request.display());
    fs::write(&request, body).unwrap();
    let checkpoint = agent.ok(&["checkpoint", "create", request.to_str().unwrap()]);
    (
        session_id,
        evidence_id,
        str_of(&checkpoint, "checkpoint_id").to_owned(),
    )
}

/// The Task with `task_id` in a `task list` page.
fn listed(page: &Value, task_id: &str) -> Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == task_id)
        .unwrap_or_else(|| panic!("{task_id} not listed in {page}"))
        .clone()
}

#[test]
#[allow(clippy::too_many_lines)] // One end-to-end demonstration, in order.
fn task_completes_on_verified_evidence_and_survives_sigkill() {
    let mut setup = setup();
    let (agent, operator) = (setup.agent.clone(), setup.operator.clone());
    let project_id = setup.project_id.clone();
    let requests = setup.root.path().join("requests");
    fs::create_dir(&requests).unwrap();
    let repo_arg = setup.repo.to_str().unwrap().to_owned();

    step("agent: Workspace and Task");
    let workspace_id = str_of(
        &agent.ok(&["workspace", "resolve", "--project", &project_id, &repo_arg]),
        "workspace_id",
    )
    .to_owned();
    let task_id = str_of(
        &agent.ok(&[
            "task",
            "create",
            "--project",
            &project_id,
            "--title",
            "Make the fixture tests pass",
            "--workspace",
            &workspace_id,
        ]),
        "task_id",
    )
    .to_owned();
    let transition = |revision: &str, target: &str, extra: &[&str]| {
        let mut words = vec![
            "task",
            "transition",
            task_id.as_str(),
            "--to",
            target,
            "--revision",
            revision,
        ];
        words.extend_from_slice(extra);
        agent.ok(&words)
    };
    transition("1", "ready", &[]);
    transition("2", "in_progress", &[]);

    step("agent: Session continuing the Task, tests, Checkpoint");
    let (_, evidence_id, checkpoint_id) =
        tested_session(&agent, &requests, &workspace_id, &task_id, "checkpoint-1");
    let preview = agent.ok(&["recover", "preview", "--task", &task_id]);
    assert_eq!(preview["status"], "verified");

    step("agent: complete the Task on the Evidence");
    let completed = transition("3", "completed", &["--evidence", &evidence_id]);
    assert_eq!(str_of(&completed, "state"), "completed");
    assert_eq!(completed["revision"], 4);
    assert_eq!(
        completed["completion_evidence_ids"],
        serde_json::json!([evidence_id])
    );

    step("agent: ledger verify");
    let verified = agent.ok(&["ledger", "verify", "--project", &project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    assert_eq!(verified["checked"]["checkpoints"], 1);
    let head = str_of(&verified, "head_event_sha256").to_owned();

    step("operator: the Event list holds every change of the Task");
    let events = operator.ok(&["event", "list", "--project", &project_id, "--limit", "500"]);
    let items = events["items"].as_array().unwrap();
    let of_task = items
        .iter()
        .filter(|event| {
            str_of(event, "event_type").starts_with("task.")
                && event["payload"]["task_id"] == task_id
        })
        .map(|event| {
            (
                str_of(event, "event_type").to_owned(),
                event["payload"]["to"].as_str().map(str::to_owned),
                event["session_id"].is_null(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        of_task,
        [
            ("task.created".to_owned(), None, true),
            (
                "task.transitioned".to_owned(),
                Some("ready".to_owned()),
                true
            ),
            (
                "task.transitioned".to_owned(),
                Some("in_progress".to_owned()),
                true
            ),
            ("task.session_linked".to_owned(), None, false),
            (
                "task.transitioned".to_owned(),
                Some("completed".to_owned()),
                true
            ),
        ]
    );
    let last = items.last().unwrap();
    assert_eq!(str_of(last, "event_type"), "task.transitioned");
    assert_eq!(
        last["payload"]["completion_evidence_ids"],
        serde_json::json!([evidence_id])
    );
    assert_eq!(str_of(&last["producer"], "name"), "continuityd");
    let tasks = agent.ok(&["task", "list", "--project", &project_id]);
    assert_eq!(listed(&tasks, &task_id), completed);

    step("SIGKILL and restart: unchanged");
    setup.daemon.take().unwrap().kill();
    let daemon = Daemon::start(&operator, &setup.root.path().join("daemon-2.log"));
    assert_eq!(agent.ok(&["task", "list", "--project", &project_id]), tasks);
    assert_eq!(
        operator.ok(&["event", "list", "--project", &project_id, "--limit", "500"]),
        events
    );
    let again = agent.ok(&["ledger", "verify", "--project", &project_id]);
    assert_eq!(again, verified);
    assert_eq!(str_of(&again, "head_event_sha256"), head);
    assert_eq!(
        agent.ok(&["checkpoint", "get", &checkpoint_id])["checkpoint_id"],
        checkpoint_id.as_str()
    );

    step("agent: a second Task whose file changed after its run is refused");
    let second = str_of(
        &agent.ok(&[
            "task",
            "create",
            "--project",
            &project_id,
            "--title",
            "Change the README",
            "--workspace",
            &workspace_id,
        ]),
        "task_id",
    )
    .to_owned();
    for (revision, target) in [("1", "ready"), ("2", "in_progress")] {
        agent.ok(&[
            "task",
            "transition",
            &second,
            "--to",
            target,
            "--revision",
            revision,
        ]);
    }
    let (_, second_evidence, _) =
        tested_session(&agent, &requests, &workspace_id, &second, "checkpoint-2");
    println!("$ printf 'fixture, edited\\n' > README");
    fs::write(setup.repo.join("README"), "fixture, edited\n").unwrap();
    let refused = agent.problem(
        &[
            "task",
            "transition",
            &second,
            "--to",
            "completed",
            "--revision",
            "3",
            "--evidence",
            &second_evidence,
        ],
        409,
        "evidence_stale",
    );
    assert_eq!(refused["code"], "evidence_stale");
    let stale = agent.ok(&["recover", "preview", "--task", &second]);
    assert_eq!(stale["status"], "stale");
    let tasks = agent.ok(&["task", "list", "--project", &project_id]);
    assert_eq!(listed(&tasks, &second)["state"], "in_progress");
    assert_eq!(listed(&tasks, &task_id), completed);
    let final_check = agent.ok(&["ledger", "verify", "--project", &project_id]);
    assert_eq!(final_check["valid"], true);
    drop(daemon);
}

/// One failed test run of `task_id` in `session_id`: three cases, exit 1.
/// Returns the Evidence and its Snapshot.
fn failed_run(
    agent: &Cli,
    session_id: &str,
    workspace_id: &str,
    task_id: &str,
) -> (String, String) {
    let run = agent.run(
        &[
            "run",
            "--session",
            session_id,
            "--workspace",
            workspace_id,
            "--task",
            task_id,
            "--test",
            "--test-cases",
            "3",
            "--test-name",
            "fixture",
            "--",
            "sh",
            "-c",
            "echo 'test result: FAILED. 0 passed; 3 failed'; exit 1",
        ],
        false,
    );
    let failed: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(str_of(&failed, "status"), "FAIL", "{}", run.stderr);
    (
        str_of(&failed, "evidence_id").to_owned(),
        str_of(&failed["snapshot"], "snapshot_id").to_owned(),
    )
}

fn sqlite(database: &Path, sql: &str) -> String {
    println!("$ sqlite3 {} {sql:?}", database.display());
    let output = Command::new("sqlite3")
        .arg(database)
        .arg(sql)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    text(&output.stdout)
}

/// An SQL string literal.
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The SQL a writer who bypasses the daemon would run to turn a failed test
/// run into `PASS`: its payload and digests, and its Snapshot's digest,
/// recomputed with the domain's own constructors; no Event is touched.
fn forge_pass(data_dir: &Path, project_id: &str, evidence_id: &str) -> String {
    let data_dir = &fs::canonicalize(data_dir).unwrap();
    let registry = RegistryStore::open(data_dir).unwrap();
    let project = registry
        .get_project(&ProjectId::parse(project_id).unwrap())
        .unwrap();
    drop(registry);
    let state = StateStore::open(data_dir, &project).unwrap();
    let id = EvidenceId::parse(evidence_id).unwrap();
    let record = state.get_evidence_record(&id).unwrap();
    let snapshot = record.snapshots.into_iter().next().unwrap();
    let mut input = record.evidence.to_input();
    input.payload = match input.payload {
        EvidencePayload::TestResult {
            argv,
            stdout,
            stderr,
            test_case_count,
            started_at,
            finished_at,
            stdout_sha256,
            stderr_sha256,
            ..
        } => EvidencePayload::TestResult {
            argv,
            exit_code: 0,
            stdout,
            stderr,
            passed: true,
            test_case_count,
            test_format: None,
            started_at,
            finished_at,
            stdout_sha256,
            stderr_sha256,
            launch_error: None,
        },
        other => panic!("not a test run: {other:?}"),
    };
    let forged = Evidence::new(input).unwrap();
    let sealed = snapshot
        .all_evidence_ids()
        .map(|item| {
            if *item == id {
                forged.clone()
            } else {
                state.get_evidence(item).unwrap()
            }
        })
        .collect::<Vec<_>>();
    let resealed = RepositorySnapshot::new(snapshot.to_input(), &sealed).unwrap();
    let payload = CanonicalJson::parse(&serde_json::to_string(forged.payload()).unwrap()).unwrap();
    format!(
        "DROP TRIGGER evidence_reject_update; DROP TRIGGER repository_snapshots_reject_update; \
         UPDATE evidence SET payload = {}, payload_digest = {}, canonical_digest = {} \
         WHERE evidence_id = {}; \
         UPDATE repository_snapshots SET canonical_digest = {} WHERE snapshot_id = {}; \
         SELECT total_changes();",
        quoted(payload.as_str()),
        quoted(forged.payload_digest().as_str()),
        quoted(forged.canonical_digest().as_str()),
        quoted(evidence_id),
        quoted(resealed.canonical_digest().as_str()),
        quoted(snapshot.snapshot_id().as_str()),
    )
}

/// The `ledger verify` result of a ledger that does not verify: exit 5,
/// the `200 LedgerVerification` on stdout.
fn invalid_ledger(agent: &Cli, project_id: &str) -> Value {
    let run = agent.run(&["ledger", "verify", "--project", project_id], false);
    assert_eq!(run.code, Some(5), "{}{}", run.stdout, run.stderr);
    let verification: Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(verification["valid"], false, "{verification}");
    verification
}

#[test]
#[allow(clippy::too_many_lines)] // One demonstration, in order.
fn deleted_and_forged_records_are_found_through_real_binaries() {
    let mut setup = setup();
    let (agent, operator) = (setup.agent.clone(), setup.operator.clone());
    let project_id = setup.project_id.clone();
    let requests = setup.root.path().join("requests");
    fs::create_dir(&requests).unwrap();
    let repo_arg = setup.repo.to_str().unwrap().to_owned();
    let database = operator
        .data_dir
        .join("projects")
        .join(&project_id)
        .join("state.sqlite");
    let workspace_id = str_of(
        &agent.ok(&["workspace", "resolve", "--project", &project_id, &repo_arg]),
        "workspace_id",
    )
    .to_owned();
    let new_task = |title: &str| {
        let task_id = str_of(
            &agent.ok(&[
                "task",
                "create",
                "--project",
                &project_id,
                "--title",
                title,
                "--workspace",
                &workspace_id,
            ]),
            "task_id",
        )
        .to_owned();
        for (revision, target) in [("1", "ready"), ("2", "in_progress")] {
            agent.ok(&[
                "task",
                "transition",
                &task_id,
                "--to",
                target,
                "--revision",
                revision,
            ]);
        }
        task_id
    };

    step("agent: CP1 says tests pass; CP2, later, records a failed run");
    let task_a = new_task("Keep the fixture tests passing");
    let (session_id, _, checkpoint_1) =
        tested_session(&agent, &requests, &workspace_id, &task_a, "checkpoint-1");
    let (failed_a, snapshot_a) = failed_run(&agent, &session_id, &workspace_id, &task_a);
    let request = requests.join("checkpoint-2.json");
    fs::write(
        &request,
        format!(
            r#"{{"schema_version":1,"workspace_id":"{workspace_id}","task_id":"{task_a}",
                "session_id":"{session_id}","snapshot_id":"{snapshot_a}",
                "objective":"Keep the fixture tests passing",
                "verified_completed_work":[],
                "open_items":[{{"classification":"unknown","text":"the tests now fail",
                    "evidence_ids":["{failed_a}"]}}],
                "next_action":{{"classification":"recommendation","text":"fix the tests"}}}}"#
        ),
    )
    .unwrap();
    let checkpoint_2 = str_of(
        &agent.ok(&["checkpoint", "create", request.to_str().unwrap()]),
        "checkpoint_id",
    )
    .to_owned();
    let before = agent.ok(&["recover", "preview", "--task", &task_a]);
    assert_eq!(
        before["bindings"]["checkpoint"]["checkpoint_id"],
        checkpoint_2.as_str()
    );
    assert_eq!(before["status"], "partial");
    let announced = before["bindings"]["checkpoint"]["project_event_sequence"]
        .as_u64()
        .unwrap()
        + 1;
    assert_eq!(
        agent.ok(&["ledger", "verify", "--project", &project_id])["valid"],
        true
    );

    step("daemon stopped: CP2's rows deleted, its checkpoint.created Event kept");
    setup.daemon.take().unwrap().kill();
    let deleted = sqlite(
        &database,
        &format!(
            "DROP TRIGGER checkpoint_item_evidence_reject_delete; \
             DROP TRIGGER checkpoint_items_reject_delete; DROP TRIGGER checkpoints_reject_delete; \
             DELETE FROM checkpoint_item_evidence WHERE checkpoint_id = '{checkpoint_2}'; \
             DELETE FROM checkpoint_items WHERE checkpoint_id = '{checkpoint_2}'; \
             DELETE FROM checkpoints WHERE checkpoint_id = '{checkpoint_2}'; \
             SELECT count(*) FROM checkpoints;"
        ),
    );
    assert_eq!(deleted.trim(), "1", "CP1 remains");
    let daemon = Daemon::start(&operator, &setup.root.path().join("daemon-2.log"));

    step("agent: ledger verify names CP2; the Preview is tampered on CP2, not CP1");
    let verification = invalid_ledger(&agent, &project_id);
    assert_eq!(
        verification["first_failure"],
        serde_json::json!({
            "record_type": "checkpoint",
            "record_id": checkpoint_2,
            "project_sequence": announced,
            "reason": format!(
                "record is missing; the checkpoint.created Event at Project sequence \
                 {announced} records it"
            ),
        })
    );
    let after = agent.ok(&["recover", "preview", "--task", &task_a]);
    assert_eq!(after["status"], "tampered", "{after}");
    assert_eq!(
        after["bindings"]["checkpoint"]["checkpoint_id"],
        checkpoint_2.as_str()
    );
    assert_eq!(after["bindings"]["checkpoint"]["intact"], false);
    assert!(
        after["gaps"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("record_integrity")),
        "{after}"
    );
    assert!(
        after["facts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fact| fact["claim"]["checkpoint_id"] != checkpoint_1.as_str()),
        "CP1's statements do not come back: {after}"
    );

    step("daemon stopped: a failed run of Task B forged as PASS, digests recomputed");
    let task_b = new_task("Make the fixture tests pass again");
    let session_b = str_of(
        &agent.ok(&[
            "session",
            "create",
            "--workspace",
            &workspace_id,
            "--agent",
            AGENT_ID,
            "--task",
            &task_b,
        ]),
        "session_id",
    )
    .to_owned();
    let (failed_b, _) = failed_run(&agent, &session_b, &workspace_id, &task_b);
    daemon.kill();
    let forgery = forge_pass(&operator.data_dir, &project_id, &failed_b);
    assert_eq!(sqlite(&database, &forgery).trim(), "2");
    let _daemon = Daemon::start(&operator, &setup.root.path().join("daemon-3.log"));

    step("the forged run reads back as PASS; ledger verify names it; completion is 409");
    assert_eq!(
        str_of(&operator.ok(&["evidence", "get", &failed_b]), "status"),
        "PASS"
    );
    let verification = invalid_ledger(&agent, &project_id);
    // Evidence is checked before Checkpoints, so the forgery is reported
    // ahead of CP2's deletion.
    let failure = &verification["first_failure"];
    assert_eq!(failure["record_type"], "evidence", "{failure}");
    assert_eq!(failure["record_id"], failed_b.as_str(), "{failure}");
    assert!(
        str_of(failure, "reason").contains("records digest"),
        "{failure}"
    );
    let refused = agent.problem(
        &[
            "task",
            "transition",
            &task_b,
            "--to",
            "completed",
            "--revision",
            "3",
            "--evidence",
            &failed_b,
        ],
        409,
        "evidence_stale",
    );
    assert_eq!(refused["code"], "evidence_stale");
    let tasks = agent.ok(&["task", "list", "--project", &project_id]);
    assert_eq!(listed(&tasks, &task_b)["state"], "in_progress");
}

/// A command for a binary under test with no inherited `MEMPROVE_*`
/// variable, so the test does not depend on the caller's environment
/// (P2-H); the test sets the ones it needs afterwards.
fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    command
}
