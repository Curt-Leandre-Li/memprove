//! P2-A demonstration (ADR 0012): the real `memprove` CLI against the real
//! `memproved` over a temporary Git repository. The operator holds the
//! bootstrap control token; every agent step uses one token that carries only
//! `mcp-read` and `mcp-write`.
//!
//! resolve the Workspace → Task → Session continuing the Task →
//! `memprove run` a real test script → a zero-case run is not a pass →
//! Checkpoint "tests pass" citing the Evidence → end the Session → `SIGKILL`
//! → restart → the Checkpoint, the Evidence, and the Events are unchanged and
//! the Event chain verifies.
//!
//! Run with `--nocapture` to print the transcript. Between daemon runs the
//! test reads `state.sqlite` through the store, which recomputes every digest;
//! the daemon is never running while it does. `memprove` is taken from the
//! same target directory as `memproved`, so run this through
//! `cargo test --workspace` (which builds both binaries).

use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_domain::{CheckpointId, EvidenceId, ProjectId};
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use memprove_store_sqlite::{RegistryStore, StateStore};

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

    fn ok(&self, words: &[&str]) -> JsonValue {
        let run = self.run(words, false);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        parse_strict(run.stdout.trim()).unwrap()
    }

    fn problem(&self, words: &[&str], status: u16, code: &str) {
        let run = self.run(words, false);
        assert_eq!(run.code, Some(1), "{words:?}: {}{}", run.stdout, run.stderr);
        assert!(run.stdout.contains(&format!(r#""code":"{code}""#)));
        assert!(run.stderr.contains(&format!("failed: {status} {code}")));
    }
}

fn get<'a>(body: &'a JsonValue, path: &[&str]) -> &'a JsonValue {
    path.iter().fold(body, |value, name| {
        value
            .get(name)
            .unwrap_or_else(|| panic!("{name} missing in {path:?}"))
    })
}

fn str_of(body: &JsonValue, name: &str) -> String {
    get(body, &[name])
        .as_str()
        .unwrap_or_else(|| panic!("{name} is not a string"))
        .to_owned()
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
            "$ memproved --data-dir <DATA>   # pid {}",
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

/// `memprove run <target> <extra>`.
fn run_words<'a>(target: &[&'a str], extra: &[&'a str]) -> Vec<&'a str> {
    let mut words = vec!["run"];
    words.extend_from_slice(target);
    words.extend_from_slice(extra);
    words
}

/// Opens the Project's state through the store after the full Event chain
/// verifies. The daemon must be stopped.
fn verified_state(data_dir: &Path, project_id: &str) -> StateStore {
    let data_dir = fs::canonicalize(data_dir).unwrap();
    let registry = RegistryStore::open(&data_dir).unwrap();
    let project = registry
        .get_project(&ProjectId::parse(project_id).unwrap())
        .unwrap();
    drop(registry);
    let state = StateStore::open(&data_dir, &project).unwrap();
    state
        .verify_event_chain()
        .expect("Project Event chain verifies");
    state
}

#[test]
#[allow(clippy::too_many_lines)] // One end-to-end demonstration, in order.
fn agent_evidence_and_checkpoint_survive_sigkill_through_real_binaries() {
    let root = tempfile::tempdir().unwrap();
    let repo = fs::canonicalize(root.path()).unwrap().join("repo");
    git_repository(&repo);
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let requests = root.path().join("requests");
    fs::create_dir(&requests).unwrap();
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

    // The operator registers the repository and issues the agent one token
    // carrying exactly mcp-read and mcp-write.
    let repo_arg = repo.to_str().unwrap();
    let project_id = str_of(
        &operator.ok(&["project", "register", repo_arg]),
        "project_id",
    );
    let created = operator.ok(&[
        "token",
        "create",
        "--label",
        "agent",
        "--scope",
        "mcp-read",
        "--scope",
        "mcp-write",
    ]);
    assert_eq!(
        get(&created, &["scopes"]),
        &parse_strict(r#"["mcp-read","mcp-write"]"#).unwrap()
    );
    let agent_token = root.path().join("agent.token");
    fs::write(&agent_token, str_of(&created, "secret")).unwrap();
    fs::set_permissions(&agent_token, fs::Permissions::from_mode(0o600)).unwrap();
    let agent = operator.with_token(&agent_token, "agent/mcp-read+mcp-write");

    // Agent: resolve the Workspace, create the Task, start a Session on it.
    let workspace_id = str_of(
        &agent.ok(&["workspace", "resolve", "--project", &project_id, repo_arg]),
        "workspace_id",
    );
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
    );
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
    );
    let target = [
        "--session",
        &session_id,
        "--workspace",
        &workspace_id,
        "--task",
        &task_id,
    ];

    // Agent: run the real test script here; memproved records the result
    // pinned to a snapshot of the repository at HEAD.
    let passed = agent.ok(&run_words(
        &target,
        &[
            "--test",
            "--test-cases",
            "3",
            "--test-name",
            "fixture",
            "--",
            "sh",
            "tests/run.sh",
        ],
    ));
    assert_eq!(str_of(&passed, "status"), "PASS");
    assert_eq!(get(&passed, &["payload", "exit_code"]).as_i64(), Some(0));
    assert_eq!(
        get(&passed, &["payload", "test_case_count"]).as_i64(),
        Some(3)
    );
    assert!(
        get(&passed, &["payload", "stdout"])
            .as_str()
            .unwrap()
            .contains("3 passed")
    );
    assert_eq!(str_of(get(&passed, &["snapshot"]), "head_oid"), head);
    assert_eq!(
        get(&passed, &["snapshot", "is_dirty"]).as_bool(),
        Some(false)
    );
    let evidence_id = str_of(&passed, "evidence_id");
    let snapshot_id = str_of(get(&passed, &["snapshot"]), "snapshot_id");

    // Agent: a test run with zero declared cases is not a pass. P3-D3: it
    // is another command; a later run of `sh tests/run.sh` itself in this
    // state that did not pass would overrule the passed run.
    let empty = agent.ok(&run_words(
        &target,
        &["--test", "--test-cases", "0", "--", "true"],
    ));
    assert_eq!(str_of(&empty, "status"), "NOT_RUN");
    assert_eq!(get(&empty, &["payload", "passed"]).as_bool(), Some(false));

    // Agent: a Checkpoint that claims "tests pass" citing the passed run.
    let checkpoint_request = |snapshot: &str, evidence: &str| {
        format!(
            r#"{{"schema_version":1,"workspace_id":"{workspace_id}","task_id":"{task_id}",
                "session_id":"{session_id}","snapshot_id":"{snapshot}",
                "objective":"Make the fixture tests pass",
                "verified_completed_work":[{{"classification":"verified_fact",
                    "text":"tests pass","evidence_ids":["{evidence}"]}}],
                "open_items":[],
                "next_action":{{"classification":"recommendation",
                    "text":"rerun tests/run.sh after the next change"}}}}"#
        )
    };
    let refused = requests.join("checkpoint-zero-cases.json");
    fs::write(
        &refused,
        checkpoint_request(
            &str_of(get(&empty, &["snapshot"]), "snapshot_id"),
            &str_of(&empty, "evidence_id"),
        ),
    )
    .unwrap();
    agent.problem(
        &["checkpoint", "create", refused.to_str().unwrap()],
        400,
        "invalid_request",
    );
    let request = requests.join("checkpoint.json");
    fs::write(&request, checkpoint_request(&snapshot_id, &evidence_id)).unwrap();
    let checkpoint = agent.ok(&["checkpoint", "create", request.to_str().unwrap()]);
    let checkpoint_id = str_of(&checkpoint, "checkpoint_id");
    assert_eq!(
        str_of(
            get(&checkpoint, &["verified_completed_work"])
                .as_array()
                .unwrap()
                .first()
                .unwrap(),
            "text"
        ),
        "tests pass"
    );
    assert_eq!(str_of(&checkpoint, "snapshot_id"), snapshot_id);

    // Agent: end the Session; it then accepts no further Evidence.
    let ended = agent.ok(&[
        "session",
        "end",
        &session_id,
        "--revision",
        "1",
        "--reason",
        "tests pass; checkpoint recorded",
    ]);
    assert_eq!(str_of(&ended, "state"), "ended");
    let late = agent.run(&run_words(&target, &["--", "true"]), false);
    // P2-D1: a result that cannot be recorded exits 125.
    assert_eq!(late.code, Some(125));
    assert!(late.stderr.contains("failed: 409 invalid_transition"));

    // The agent token cannot reach control-only operations.
    agent.problem(&["status"], 403, "policy_denied");
    agent.problem(
        &["event", "list", "--project", &project_id],
        403,
        "policy_denied",
    );
    agent.problem(&["evidence", "get", &evidence_id], 403, "policy_denied");
    agent.problem(&["project", "register", repo_arg], 403, "policy_denied");

    // Before the kill: what the agent and the operator read.
    let agent_checkpoint = agent.ok(&["checkpoint", "get", &checkpoint_id]);
    assert_eq!(agent_checkpoint, checkpoint);
    let evidence = operator.ok(&["evidence", "get", &evidence_id]);
    // P3-D: the zero-case run shares the passed run's repository state and
    // carries it over (ADR 0013 D3), so a second Snapshot seals it; the
    // first is still the one it was recorded with.
    let mut expected = passed.clone();
    if let JsonValue::Object(members) = &mut expected {
        for (name, value) in members.iter_mut() {
            if name == "snapshot_ids" {
                *value = JsonValue::Array(vec![
                    JsonValue::String(snapshot_id.clone()),
                    JsonValue::String(str_of(get(&empty, &["snapshot"]), "snapshot_id")),
                ]);
            }
        }
    }
    assert_eq!(evidence, expected);
    let events = operator.ok(&["event", "list", "--project", &project_id]);
    let types = get(&events, &["items"])
        .as_array()
        .unwrap()
        .iter()
        .map(|event| str_of(event, "event_type"))
        .collect::<Vec<_>>();
    // task.created and task.session_linked are P2-C's.
    assert_eq!(
        types,
        [
            "task.created",
            "session.started",
            "task.session_linked",
            "command.completed",
            "command.completed",
            "checkpoint.created",
            "session.ended"
        ]
    );
    let items = get(&events, &["items"]).as_array().unwrap();
    assert_eq!(
        str_of(get(&items[3], &["payload", "command"]), "evidence_id"),
        evidence_id
    );
    assert_eq!(
        str_of(get(&items[5], &["payload", "checkpoint"]), "checkpoint_id"),
        checkpoint_id
    );
    assert_eq!(
        get(&items[6], &["payload", "reason"]).as_str(),
        Some("tests pass; checkpoint recorded")
    );

    // SIGKILL, then verify offline through the store.
    daemon.kill();
    {
        let state = verified_state(&operator.data_dir, &project_id);
        let stored = state
            .get_checkpoint(&CheckpointId::parse(&checkpoint_id).unwrap())
            .unwrap();
        assert_eq!(
            stored.digest().to_string(),
            str_of(&checkpoint, "digest")["sha256:".len()..]
        );
        let record = state
            .get_evidence_record(&EvidenceId::parse(&evidence_id).unwrap())
            .unwrap();
        assert_eq!(
            record.evidence.canonical_digest().to_string(),
            str_of(&evidence, "canonical_digest")["sha256:".len()..]
        );
        println!(
            "store: verify_event_chain OK; Checkpoint {checkpoint_id} digest {} and Evidence \
             {evidence_id} digest {} recomputed",
            stored.digest(),
            record.evidence.canonical_digest()
        );
    }

    // Restart: the Checkpoint, the Evidence, and the Events are unchanged.
    let daemon = Daemon::start(&operator, &root.path().join("daemon-2.log"));
    assert_eq!(agent.ok(&["checkpoint", "get", &checkpoint_id]), checkpoint);
    assert_eq!(
        operator.ok(&["checkpoint", "get", &checkpoint_id]),
        checkpoint
    );
    assert_eq!(operator.ok(&["evidence", "get", &evidence_id]), evidence);
    assert_eq!(
        operator.ok(&["event", "list", "--project", &project_id]),
        events
    );
    drop(daemon);
    let state = verified_state(&operator.data_dir, &project_id);
    assert_eq!(state.list_events_page(None, 500).unwrap().items.len(), 7);
    println!("restart: Checkpoint, Evidence, and 7 Events unchanged; chain verifies");
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
