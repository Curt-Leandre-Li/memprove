//! Process-level Event scenario (ADR 0010): the real `memprove` CLI against
//! the real `memproved` over a temporary Git repository. Events survive a
//! `SIGKILL` with identical sequences and digests, a `SIGKILL` in the middle of
//! concurrent ingests leaves no partial row, and new Events extend the chain.
//!
//! Between daemon runs the test reads `state.sqlite` through the store, which
//! recomputes every digest; the daemon is never running while it does.
//! `memprove` is taken from the same target directory as `memproved`, so
//! run this through `cargo test --workspace` (which builds both binaries).

use std::{
    fs,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use memprove_domain::{Event, ProjectId};
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use memprove_store_sqlite::{RegistryStore, StateStore};

const TIMEOUT: Duration = Duration::from_secs(15);
/// Concurrent CLI writers in the crash phase, and Events each one submits.
const WRITERS: usize = 6;
const PER_WRITER: u64 = 40;

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

#[derive(Clone)]
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
            println!("$ memprove {}\n{}", words.join(" "), run.stdout.trim_end());
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

    fn signal(mut self, name: &str, number: i32) {
        let sent = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = self.child.wait().unwrap();
        if number == 9 {
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
fn ledger(data_dir: &Path, project_id: &str) -> Vec<Event> {
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
    let page = state.list_events_page(None, 500).unwrap();
    assert!(!page.has_more);
    assert_eq!(
        page.snapshot_sequence,
        u64::try_from(page.items.len()).unwrap()
    );
    page.items
}

/// `(event_id, session sequence, project sequence, previous digest, digest)`.
type Fingerprint = (String, Option<u64>, u64, Option<String>, String);

fn fingerprint(events: &[Event]) -> Vec<Fingerprint> {
    events
        .iter()
        .map(|event| {
            (
                event.event_id().to_string(),
                event.sequence(),
                event.project_sequence(),
                event.previous_event_sha256().map(ToString::to_string),
                event.event_sha256().to_string(),
            )
        })
        .collect()
}

struct Envelopes {
    directory: PathBuf,
    project_id: String,
    workspace_id: String,
}

impl Envelopes {
    /// Writes one envelope for `session_id` and returns `(event_id, path)`.
    fn write(&self, session_id: &str, turn: u64, occurred_at: Option<&str>) -> (String, PathBuf) {
        let event_id = uuid::Uuid::now_v7().to_string();
        let mut body = serde_json::json!({
            "envelope_version": "1.0",
            "event_id": event_id,
            "project_id": self.project_id,
            "workspace_id": self.workspace_id,
            "session_id": session_id,
            "event_type": "turn.started",
            "payload_schema": "turn.started@1",
            "producer": { "name": "e2e-hook", "version": "1.0.0", "sequence": turn },
            "privacy_class": "internal",
            "payload": { "turn": turn },
        });
        if let Some(occurred_at) = occurred_at {
            body["occurred_at"] = serde_json::json!(occurred_at);
        }
        let path = self.directory.join(format!("{event_id}.json"));
        fs::write(&path, body.to_string()).unwrap();
        (event_id, path)
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario across three daemon runs.
fn events_survive_sigkill_and_extend_one_chain_through_real_binaries() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    git_repository(&repo);
    let data_dir = root.path().join("data");
    let token = root.path().join("control.token");
    let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--output")
        .arg(&token)
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    let cli = Cli {
        data_dir: data_dir.clone(),
        token,
    };

    let daemon = Daemon::start(&cli, &root.path().join("daemon-1.log"));
    let project = cli.ok(&["project", "register", repo.to_str().unwrap()]);
    let project_id = str_of(&project, "project_id");
    let workspace_id = str_of(&project, "root_workspace_id");
    let task = cli.ok(&[
        "task",
        "create",
        "--project",
        &project_id,
        "--title",
        "Record events",
        "--workspace",
        &workspace_id,
    ]);
    assert_eq!(str_of(&task, "state"), "proposed");
    let mut sessions = Vec::new();
    for agent in [
        "018f47a0-0000-7000-8000-00000000a6e1",
        "018f47a0-0000-7000-8000-00000000a6e2",
    ] {
        let session = cli.ok(&[
            "session",
            "create",
            "--workspace",
            &workspace_id,
            "--agent",
            agent,
        ]);
        sessions.push(str_of(&session, "session_id"));
    }
    let envelopes = Envelopes {
        directory: root.path().join("envelopes"),
        project_id: project_id.clone(),
        workspace_id: workspace_id.clone(),
    };
    fs::create_dir(&envelopes.directory).unwrap();

    // Five Events across two Sessions; one declares its occurrence time.
    let mut written = Vec::new();
    for turn in 0..5_u64 {
        let occurred_at = (turn == 0).then_some("2026-10-02T08:00:00.250000Z");
        let session = &sessions[usize::try_from(turn % 2).unwrap()];
        let (event_id, path) = envelopes.write(session, turn, occurred_at);
        let accepted = cli.ok(&[
            "event",
            "ingest",
            path.to_str().unwrap(),
            "--idempotency-key",
            &format!("e2e-first-run-{turn:04}"),
        ]);
        assert_eq!(str_of(&accepted, "resource_id"), event_id);
        written.push(event_id);
    }
    let first = cli.ok(&["event", "get", &written[0]]);
    assert_eq!(str_of(&first, "occurred_at"), "2026-10-02T08:00:00.250Z");
    let second = cli.ok(&["event", "get", &written[1]]);
    assert_eq!(second.get("occurred_at"), Some(&JsonValue::Null));
    // The Task's `task.created` (P2-C) and each Session's `session.started`
    // (ADR 0012) precede the five Events.
    let page = cli.ok(&["event", "list", "--project", &project_id]);
    assert_eq!(
        page.get("snapshot_sequence").and_then(JsonValue::as_i64),
        Some(8)
    );
    // The contract Problems through the real binaries.
    let mut orphan = envelopes.write(&sessions[0], 99, None);
    let orphan_text = fs::read_to_string(&orphan.1)
        .unwrap()
        .replace(&sessions[0], "018f47a0-0000-7000-8000-000000000001");
    orphan.1 = root.path().join("orphan.json");
    fs::write(&orphan.1, orphan_text).unwrap();
    cli.problem(
        &["event", "ingest", orphan.1.to_str().unwrap()],
        404,
        "resource_not_found",
    );
    cli.problem(
        &["event", "get", "018f47a0-0000-7000-8000-000000000001"],
        404,
        "resource_not_found",
    );

    daemon.signal("KILL", 9);
    let before_restart = ledger(&data_dir, &project_id);
    assert_eq!(
        before_restart
            .iter()
            .map(memprove_domain::Event::event_type)
            .collect::<Vec<_>>(),
        [
            "task.created",
            "session.started",
            "session.started",
            "turn.started",
            "turn.started",
            "turn.started",
            "turn.started",
            "turn.started"
        ]
    );
    assert_eq!(
        before_restart[3..]
            .iter()
            .map(|event| event.event_id().to_string())
            .collect::<Vec<_>>(),
        written
    );
    assert_eq!(
        before_restart
            .iter()
            .map(|event| (event.sequence(), event.project_sequence()))
            .collect::<Vec<_>>(),
        // The Task's task.created has no Session (P2-C).
        std::iter::once((None, 1))
            .chain(
                [(1, 2), (1, 3), (2, 4), (2, 5), (3, 6), (3, 7), (4, 8)]
                    .map(|(session, project)| (Some(session), project)),
            )
            .collect::<Vec<_>>()
    );

    // Restart: the same Events, sequences, and digests; replay is idempotent;
    // a new Event takes the chain tail.
    let daemon = Daemon::start(&cli, &root.path().join("daemon-2.log"));
    assert_eq!(cli.ok(&["event", "list", "--project", &project_id]), page);
    assert_eq!(cli.ok(&["event", "get", &written[0]]), first);
    let replayed = cli.ok(&[
        "event",
        "ingest",
        envelopes
            .directory
            .join(format!("{}.json", written[0]))
            .to_str()
            .unwrap(),
        "--idempotency-key",
        "e2e-first-run-0000",
    ]);
    assert_eq!(str_of(&replayed, "resource_id"), written[0]);
    let (tail_id, tail_path) = envelopes.write(&sessions[1], 5, None);
    cli.ok(&["event", "ingest", tail_path.to_str().unwrap()]);
    daemon.signal("TERM", 15);
    let after_restart = ledger(&data_dir, &project_id);
    assert_eq!(
        fingerprint(&after_restart[..8]),
        fingerprint(&before_restart)
    );
    let tail = &after_restart[8];
    assert_eq!(tail.event_id().as_str(), tail_id);
    assert_eq!((tail.sequence(), tail.project_sequence()), (Some(4), 9));
    assert_eq!(
        tail.previous_event_sha256(),
        Some(before_restart[7].event_sha256())
    );

    // SIGKILL while concurrent ingests are in flight.
    let daemon = Daemon::start(&cli, &root.path().join("daemon-3.log"));
    let jobs = (0..WRITERS)
        .map(|writer| {
            (0..PER_WRITER)
                .map(|turn| {
                    let session = &sessions[writer % 2];
                    let (_, path) = envelopes.write(session, 100 + turn, None);
                    (format!("e2e-kill-{writer:02}-{turn:04}"), path)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(WRITERS + 1));
    let handles = jobs
        .clone()
        .into_iter()
        .map(|jobs| {
            let cli = cli.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                jobs.into_iter()
                    .map(|(key, path)| {
                        let run = cli.run(
                            &[
                                "event",
                                "ingest",
                                path.to_str().unwrap(),
                                "--idempotency-key",
                                &key,
                            ],
                            true,
                        );
                        // 0 accepted, 3 daemon gone; nothing else is allowed.
                        assert!(
                            matches!(run.code, Some(0 | 3)),
                            "{key}: {}{}",
                            run.stdout,
                            run.stderr
                        );
                        run.code == Some(0)
                    })
                    .filter(|accepted| *accepted)
                    .count()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    // Kill once the ledger has visibly advanced, while writers are mid-flight.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let run = cli.run(
            &["event", "list", "--project", &project_id, "--limit", "1"],
            true,
        );
        let progressed = run.code == Some(0)
            && parse_strict(run.stdout.trim())
                .unwrap()
                .get("snapshot_sequence")
                .and_then(JsonValue::as_i64)
                .is_some_and(|sequence| sequence >= 9 + 20);
        if progressed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "concurrent ingests made no progress"
        );
        thread::sleep(Duration::from_millis(10));
    }
    daemon.signal("KILL", 9);
    let acknowledged: usize = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .sum();
    let after_kill = ledger(&data_dir, &project_id);
    let committed = after_kill.len() - 9;
    println!(
        "SIGKILL during {WRITERS} concurrent writers: {acknowledged} acknowledged, \
         {committed} committed of {}",
        WRITERS * usize::try_from(PER_WRITER).unwrap()
    );
    assert!(committed >= acknowledged, "an acknowledged Event was lost");
    assert!(
        committed < WRITERS * usize::try_from(PER_WRITER).unwrap(),
        "the kill landed after every write; the crash window was not exercised"
    );
    assert_eq!(fingerprint(&after_kill[..9]), fingerprint(&after_restart));

    // Restart and resubmit every job with its own key: committed ones replay,
    // the rest are appended; no Event is duplicated.
    let daemon = Daemon::start(&cli, &root.path().join("daemon-4.log"));
    for (key, path) in jobs.iter().flatten() {
        let run = cli.run(
            &[
                "event",
                "ingest",
                path.to_str().unwrap(),
                "--idempotency-key",
                key,
            ],
            true,
        );
        assert_eq!(run.code, Some(0), "{key}: {}{}", run.stdout, run.stderr);
    }
    let status = cli.ok(&["status"]);
    assert_eq!(
        status.get("writer_queue_depth").and_then(JsonValue::as_i64),
        Some(0)
    );
    daemon.signal("TERM", 15);
    let settled = ledger(&data_dir, &project_id);
    assert_eq!(
        settled.len(),
        9 + WRITERS * usize::try_from(PER_WRITER).unwrap()
    );
    assert_eq!(
        fingerprint(&settled[..after_kill.len()]),
        fingerprint(&after_kill)
    );
    let mut ids = settled
        .iter()
        .map(|event| event.event_id().to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), settled.len());
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
