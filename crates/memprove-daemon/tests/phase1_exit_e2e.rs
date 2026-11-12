//! Phase 1 exit demonstration: the real `memprove` CLI against the real
//! `memproved` over a temporary Git repository with a linked worktree made
//! by `git worktree add`.
//!
//! register → resolve the root and the linked worktree → Task → Session →
//! Events → move the Task → end the Session → a further Event is refused →
//! `SIGKILL` → restart → every identity, state, replayed result, and the Event
//! chain is unchanged.
//!
//! Run with `--nocapture` to print the transcript. Between daemon runs the
//! test reads `state.sqlite` through the store, which recomputes every digest;
//! the daemon is never running while it does. `memprove` is taken from the
//! same target directory as `memproved`, so run this through
//! `cargo test --workspace` (which builds both binaries).

use std::{
    fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_domain::{Event, ProjectId};
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use memprove_store_sqlite::{RegistryStore, StateStore};

const TIMEOUT: Duration = Duration::from_secs(15);
const AGENT_ID: &str = "018f47a0-0000-7000-8000-00000000a6e1";

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

fn git(directory: &Path, arguments: &[&str]) {
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
}

fn git_repository(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(path.join("README"), b"fixture\n").unwrap();
    git(path, &["add", "README"]);
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
            "fixture",
        ],
    );
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

struct Cli {
    data_dir: PathBuf,
    token: PathBuf,
}

impl Cli {
    fn run(&self, words: &[&str], quiet: bool) -> Run {
        let output = isolated(cli_binary())
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token-file")
            .arg(&self.token)
            .env_remove("MEMPROVE_DATA_DIR")
            .env_remove("MEMPROVE_TOKEN")
            .env_remove("MEMPROVE_TOKEN_FILE")
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
                "$ memprove {}\n{}{}[exit {}]",
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

fn str_of(body: &JsonValue, name: &str) -> String {
    body.get(name)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("{name} missing"))
        .to_owned()
}

fn int_of(body: &JsonValue, name: &str) -> i64 {
    body.get(name)
        .and_then(JsonValue::as_i64)
        .unwrap_or_else(|| panic!("{name} missing"))
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
            assert_eq!(status.code(), Some(0), "graceful stop");
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

/// The Project's Events read straight from `state.sqlite` with every digest
/// recomputed, after the full chain verifies. The daemon must be stopped.
fn verified_ledger(data_dir: &Path, project_id: &str) -> Vec<Event> {
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
    let events = state.list_events_page(None, 500).unwrap().items;
    println!(
        "store: verify_event_chain OK over {} Events; head {}",
        events.len(),
        events
            .last()
            .map_or_else(String::new, |event| event.event_sha256().to_string())
    );
    events
}

fn envelope(
    directory: &Path,
    project_id: &str,
    workspace_id: &str,
    session_id: &str,
    turn: u64,
) -> PathBuf {
    let event_id = uuid::Uuid::now_v7().to_string();
    let body = serde_json::json!({
        "envelope_version": "1.0",
        "event_id": event_id,
        "project_id": project_id,
        "workspace_id": workspace_id,
        "session_id": session_id,
        "event_type": "turn.started",
        "payload_schema": "turn.started@1",
        "producer": { "name": "exit-demo", "version": "1.0.0", "sequence": turn },
        "privacy_class": "internal",
        "payload": { "turn": turn },
    });
    let path = directory.join(format!("turn-{turn}.json"));
    fs::write(&path, body.to_string()).unwrap();
    path
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario: the Phase 1 exit demonstration.
fn phase1_exit_demonstration_through_real_binaries() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    git_repository(&repo);
    let linked = root.path().join("repo-feature");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    let linked_nested = linked.join("src/deep");
    fs::create_dir_all(&linked_nested).unwrap();
    let envelopes = root.path().join("envelopes");
    fs::create_dir(&envelopes).unwrap();
    let cli = Cli {
        data_dir: root.path().join("data"),
        token: root.path().join("control.token"),
    };

    println!(
        "$ memproved bootstrap --data-dir <DATA> --output {}",
        cli.token.display()
    );
    let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(&cli.data_dir)
        .arg("--output")
        .arg(&cli.token)
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    println!("[exit 0] (token written to the 0600 file, not printed)");

    let daemon = Daemon::start(&cli, &root.path().join("daemon-1.log"));

    // Register, then resolve the root and the linked worktree.
    let repo_arg = repo.to_str().unwrap();
    let project = cli.ok(&["project", "register", repo_arg]);
    let project_id = str_of(&project, "project_id");
    let root_id = str_of(&project, "root_workspace_id");
    let resolve = |path: &Path| {
        cli.ok(&[
            "workspace",
            "resolve",
            "--project",
            &project_id,
            path.to_str().unwrap(),
        ])
    };
    assert_eq!(str_of(&resolve(&repo), "workspace_id"), root_id);
    let linked_workspace = resolve(&linked);
    let linked_id = str_of(&linked_workspace, "workspace_id");
    assert_ne!(linked_id, root_id);
    assert_eq!(resolve(&linked), linked_workspace, "repeat resolution");
    assert_eq!(resolve(&linked_nested), linked_workspace, "subdirectory");

    // Task and Session (in the linked worktree's Workspace).
    let task = cli.ok(&[
        "task",
        "create",
        "--project",
        &project_id,
        "--title",
        "Phase 1 exit",
        "--workspace",
        &linked_id,
    ]);
    let task_id = str_of(&task, "task_id");
    assert_eq!(str_of(&task, "state"), "proposed");
    let session = cli.ok(&[
        "session",
        "create",
        "--workspace",
        &linked_id,
        "--agent",
        AGENT_ID,
    ]);
    let session_id = str_of(&session, "session_id");
    assert_eq!(str_of(&session, "state"), "active");

    // Events.
    let mut event_ids = Vec::new();
    for turn in 1..=2 {
        let path = envelope(&envelopes, &project_id, &linked_id, &session_id, turn);
        let accepted = cli.ok(&["event", "ingest", path.to_str().unwrap()]);
        event_ids.push(str_of(&accepted, "resource_id"));
    }

    // Move the Task: proposed -> ready -> in_progress.
    let ready_key = "exit-demo-transition-ready-0001";
    let ready = cli.ok(&[
        "task",
        "transition",
        &task_id,
        "--to",
        "ready",
        "--revision",
        "1",
        "--idempotency-key",
        ready_key,
    ]);
    assert_eq!(
        (str_of(&ready, "state"), int_of(&ready, "revision")),
        ("ready".to_owned(), 2)
    );
    let started = cli.ok(&[
        "task",
        "transition",
        &task_id,
        "--to",
        "in_progress",
        "--revision",
        "2",
    ]);
    assert_eq!(int_of(&started, "revision"), 3);
    cli.problem(
        &[
            "task",
            "transition",
            &task_id,
            "--to",
            "completed",
            "--revision",
            "3",
        ],
        409,
        // P2-C: completed without completion Evidence does not hold.
        "evidence_stale",
    );

    // End the Session; a further Event is refused.
    let end_key = "exit-demo-end-session-000001";
    let ended = cli.ok(&[
        "session",
        "end",
        &session_id,
        "--revision",
        "1",
        "--reason",
        "demo complete",
        "--idempotency-key",
        end_key,
    ]);
    assert_eq!(
        (str_of(&ended, "state"), int_of(&ended, "revision")),
        ("ended".to_owned(), 2)
    );
    cli.problem(
        &["session", "end", &session_id, "--revision", "2"],
        409,
        "invalid_transition",
    );
    let late = envelope(&envelopes, &project_id, &linked_id, &session_id, 3);
    cli.problem(
        &["event", "ingest", late.to_str().unwrap()],
        409,
        "invalid_transition",
    );

    let tasks = cli.ok(&["task", "list", "--project", &project_id]);
    let sessions = cli.ok(&["session", "list", "--workspace", &linked_id]);
    let events = cli.ok(&["event", "list", "--project", &project_id]);
    // workspace.registered, task.created, session.started, two ingested
    // Events, two task.transitioned (P2-C), and session.ended (ADR 0012).
    assert_eq!(int_of(&events, "snapshot_sequence"), 8);

    // SIGKILL, then verify the ledger offline.
    daemon.signal("KILL");
    let before = verified_ledger(&cli.data_dir, &project_id);
    assert_eq!(
        before.iter().map(Event::event_type).collect::<Vec<_>>(),
        [
            "workspace.registered",
            "task.created",
            "session.started",
            "turn.started",
            "turn.started",
            "task.transitioned",
            "task.transitioned",
            "session.ended"
        ]
    );
    // The Workspace and Task records have no Session (ADR 0012 D7).
    assert_eq!(
        before
            .iter()
            .filter(|event| event.session_id().is_none())
            .map(Event::event_type)
            .collect::<Vec<_>>(),
        [
            "workspace.registered",
            "task.created",
            "task.transitioned",
            "task.transitioned"
        ]
    );
    assert_eq!(
        before[7].payload().as_str(),
        r#"{"reason":"demo complete"}"#
    );

    // Restart: every identity, state, and replayed result is unchanged.
    let daemon = Daemon::start(&cli, &root.path().join("daemon-2.log"));
    assert_eq!(cli.ok(&["project", "get", &project_id]), project);
    assert_eq!(cli.ok(&["project", "register", repo_arg]), project);
    assert_eq!(str_of(&resolve(&repo), "workspace_id"), root_id);
    assert_eq!(resolve(&linked), linked_workspace);
    assert_eq!(resolve(&linked_nested), linked_workspace);
    assert_eq!(cli.ok(&["task", "list", "--project", &project_id]), tasks);
    assert_eq!(
        cli.ok(&["session", "list", "--workspace", &linked_id]),
        sessions
    );
    assert_eq!(cli.ok(&["event", "list", "--project", &project_id]), events);
    for event_id in &event_ids {
        assert_eq!(
            str_of(&cli.ok(&["event", "get", event_id]), "event_id"),
            *event_id
        );
    }
    // Idempotent replays after the restart return the first results.
    assert_eq!(
        cli.ok(&[
            "task",
            "transition",
            &task_id,
            "--to",
            "ready",
            "--revision",
            "1",
            "--idempotency-key",
            ready_key,
        ]),
        ready
    );
    assert_eq!(
        cli.ok(&[
            "session",
            "end",
            &session_id,
            "--revision",
            "1",
            "--reason",
            "demo complete",
            "--idempotency-key",
            end_key,
        ]),
        ended
    );
    cli.problem(
        &["event", "ingest", late.to_str().unwrap()],
        409,
        "invalid_transition",
    );
    let status = cli.ok(&["status"]);
    assert_eq!(int_of(&status, "project_count"), 1);

    // Graceful stop, then the chain verifies again, byte for byte the same.
    daemon.signal("TERM");
    let after = verified_ledger(&cli.data_dir, &project_id);
    assert_eq!(after, before);
    assert!(!cli.data_dir.join("memproved.sock").exists());
    println!("PHASE 1 EXIT DEMONSTRATION: OK");
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
