//! Process-level slice C scenario: the real `memprove` CLI against the real
//! `memproved`, over a temporary Git repository, across a `SIGKILL` restart.
//!
//! `memproved` is taken from the same target directory as the CLI binary, so
//! run this through `cargo test --workspace` (which builds both binaries).

use std::{
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};

const TIMEOUT: Duration = Duration::from_secs(15);

fn daemon_binary() -> PathBuf {
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_memprove"));
    let daemon = cli.with_file_name("memproved");
    assert!(
        daemon.exists(),
        "memproved not found next to {}; run `cargo test --workspace`",
        cli.display()
    );
    daemon
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

struct Cli {
    data_dir: PathBuf,
}

impl Cli {
    fn run(&self, token_file: Option<&Path>, words: &[&str]) -> Run {
        let mut command = isolated(env!("CARGO_BIN_EXE_memprove"));
        command
            .arg("--data-dir")
            .arg(&self.data_dir)
            .env_remove("MEMPROVE_DATA_DIR")
            .env_remove("MEMPROVE_TOKEN")
            .env_remove("MEMPROVE_TOKEN_FILE");
        if let Some(path) = token_file {
            command.arg("--token-file").arg(path);
        }
        let output = command.args(words).output().unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        println!("$ memprove {}\n{}", words.join(" "), run.stdout.trim_end());
        run
    }

    fn ok(&self, token_file: &Path, words: &[&str]) -> JsonValue {
        let run = self.run(Some(token_file), words);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        parse_strict(run.stdout.trim()).unwrap()
    }

    fn problem(&self, token_file: &Path, words: &[&str], status: u16, code: &str) {
        let run = self.run(Some(token_file), words);
        assert_eq!(run.code, Some(1), "{words:?}: {}{}", run.stdout, run.stderr);
        assert!(
            run.stdout.contains(&format!(r#""code":"{code}""#)),
            "{words:?}: {}",
            run.stdout
        );
        assert!(
            run.stderr.contains(&format!("failed: {status} {code}")),
            "{words:?}: {}",
            run.stderr
        );
    }
}

fn str_of(body: &JsonValue, name: &str) -> String {
    body.get(name)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("{name} missing"))
        .to_owned()
}

fn ids(page: &JsonValue, name: &str) -> Vec<String> {
    page.get("items")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|item| str_of(item, name))
        .collect()
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(cli: &Cli, log: &Path) -> Self {
        let child = isolated(daemon_binary())
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
        while cli.run(None, &["health", "ready"]).code != Some(0) {
            assert!(
                Instant::now() < deadline,
                "memproved never became ready:\n{}",
                fs::read_to_string(log).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn kill(mut self) {
        let sent = Command::new("kill")
            .arg("-KILL")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = self.child.wait().unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(9)
        );
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

fn write_owner_only(path: &Path, contents: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    writeln!(file, "{contents}").unwrap();
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario: register, use, SIGKILL, restart, re-read.
fn catalog_survives_sigkill_and_enforces_scopes_through_real_binaries() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    git_repository(&repo);
    let nested = repo.join("src");
    fs::create_dir(&nested).unwrap();
    let plain = root.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let cli = Cli {
        data_dir: root.path().join("data"),
    };

    let control = root.path().join("control.token");
    let issued = isolated(daemon_binary())
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(&cli.data_dir)
        .arg("--output")
        .arg(&control)
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));

    let daemon = Daemon::start(&cli, &root.path().join("daemon-1.log"));

    let reader = root.path().join("reader.token");
    let created = cli.ok(
        &control,
        &[
            "token", "create", "--label", "reader", "--scope", "mcp-read",
        ],
    );
    write_owner_only(&reader, &str_of(&created, "secret"));

    let repo_arg = repo.to_str().unwrap();
    let project = cli.ok(&control, &["project", "register", repo_arg]);
    let project_id = str_of(&project, "project_id");
    let root_workspace_id = str_of(&project, "root_workspace_id");
    assert_eq!(str_of(&project, "kind"), "git");

    assert_eq!(cli.ok(&control, &["project", "get", &project_id]), project);
    let workspace = cli.ok(
        &control,
        &[
            "workspace",
            "resolve",
            "--project",
            &project_id,
            nested.to_str().unwrap(),
        ],
    );
    assert_eq!(str_of(&workspace, "workspace_id"), root_workspace_id);

    let task = cli.ok(
        &control,
        &[
            "task",
            "create",
            "--project",
            &project_id,
            "--title",
            "Survive a SIGKILL",
            "--workspace",
            &root_workspace_id,
        ],
    );
    let task_id = str_of(&task, "task_id");
    let tasks = cli.ok(&control, &["task", "list", "--project", &project_id]);
    assert_eq!(ids(&tasks, "task_id"), vec![task_id.clone()]);

    let agent_id = "018f47a0-0000-7000-8000-00000000a6e1";
    let session = cli.ok(
        &control,
        &[
            "session",
            "create",
            "--workspace",
            &root_workspace_id,
            "--agent",
            agent_id,
        ],
    );
    let session_id = str_of(&session, "session_id");
    assert_eq!(str_of(&session, "state"), "active");
    let sessions = cli.ok(
        &control,
        &["session", "list", "--workspace", &root_workspace_id],
    );
    assert_eq!(ids(&sessions, "session_id"), vec![session_id.clone()]);
    // The Task's `task.created` (P2-C) and the Session's `session.started`
    // (ADR 0012) Events advance the snapshot every later list is bound to.
    assert_eq!(
        sessions
            .get("snapshot_sequence")
            .and_then(JsonValue::as_i64),
        Some(2)
    );
    let tasks = cli.ok(&control, &["task", "list", "--project", &project_id]);

    let status = cli.ok(&control, &["status"]);
    assert_eq!(
        status.get("project_count").and_then(JsonValue::as_i64),
        Some(1)
    );

    // An mcp-read token reads Project, Task, and Session metadata but writes
    // nothing, and never registers a Project (ADR 0012).
    assert_eq!(cli.ok(&reader, &["project", "get", &project_id]), project);
    assert_eq!(
        cli.ok(&reader, &["task", "list", "--project", &project_id]),
        tasks
    );
    cli.problem(
        &reader,
        &[
            "task",
            "create",
            "--project",
            &project_id,
            "--title",
            "nope",
        ],
        403,
        "policy_denied",
    );
    cli.problem(
        &reader,
        &["project", "register", repo_arg],
        403,
        "policy_denied",
    );

    // A directory outside any Git worktree is not resolved.
    cli.problem(
        &control,
        &[
            "workspace",
            "resolve",
            "--project",
            &project_id,
            plain.to_str().unwrap(),
        ],
        404,
        "workspace_not_resolved",
    );

    daemon.kill();
    assert!(
        cli.data_dir.join("memproved.sock").exists(),
        "SIGKILL leaves the socket behind"
    );
    let daemon = Daemon::start(&cli, &root.path().join("daemon-2.log"));

    assert_eq!(cli.ok(&control, &["project", "get", &project_id]), project);
    assert_eq!(
        cli.ok(&control, &["task", "list", "--project", &project_id]),
        tasks
    );
    assert_eq!(
        cli.ok(
            &control,
            &["session", "list", "--workspace", &root_workspace_id]
        ),
        sessions
    );
    assert_eq!(
        cli.ok(
            &control,
            &["workspace", "resolve", "--project", &project_id, repo_arg]
        ),
        workspace
    );
    let status = cli.ok(&control, &["status"]);
    assert_eq!(
        status.get("project_count").and_then(JsonValue::as_i64),
        Some(1)
    );
    // Registering again after the restart returns the same identities.
    assert_eq!(
        cli.ok(&control, &["project", "register", repo_arg]),
        project
    );
    assert_eq!(cli.ok(&reader, &["project", "get", &project_id]), project);
    cli.problem(&reader, &["status"], 403, "policy_denied");
    drop(daemon);
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
