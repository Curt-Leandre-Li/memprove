//! Shared harness of the P4-B process-level tests: the real `memprove`
//! CLI and the real `memproved` over a temporary Git repository, with an
//! agent token holding only `mcp-read` and `mcp-write`. Adapted from
//! `p4a_reaffirm_all_e2e.rs`. Token secrets are never printed.

#![allow(dead_code)] // Each test file uses part of the harness.

use std::{
    fmt::Write as _,
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

/// U8 K8 (U7 report, section 14, r20): how long a test waits for a daemon to
/// answer `health ready` when `MEMPROVE_TEST_READY_TIMEOUT_SECS` does not
/// say: 90 seconds. The 15 seconds this was before failed now and then on a
/// heavily loaded machine.
const DEFAULT_READY_TIMEOUT_SECS: u64 = 90;

/// The variable that sets the wait, in whole seconds of at least 1.
pub const READY_TIMEOUT_VARIABLE: &str = "MEMPROVE_TEST_READY_TIMEOUT_SECS";

/// How many lines of the daemon's log the failure message carries.
const READY_LOG_LINES: usize = 20;

/// The wait of this process: the variable, else the default.
pub fn ready_timeout() -> Duration {
    ready_timeout_from(std::env::var(READY_TIMEOUT_VARIABLE).ok().as_deref())
}

/// The wait for the value of the variable: whole seconds of at least 1,
/// else (not set, empty, zero, negative, not a number) the default.
pub fn ready_timeout_from(value: Option<&str>) -> Duration {
    let seconds = value
        .and_then(|text| text.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_READY_TIMEOUT_SECS);
    Duration::from_secs(seconds)
}

/// The last `lines` lines of `log`, with the line breaks it had.
pub fn log_tail(log: &str, lines: usize) -> String {
    let mut starts = log
        .match_indices('\n')
        .map(|(at, _)| at + 1)
        .collect::<Vec<_>>();
    // A final line without a line break is a line too.
    if !log.is_empty() && !log.ends_with('\n') {
        starts.push(log.len());
    }
    let total = starts.len();
    if total <= lines {
        return log.to_owned();
    }
    // `starts[n]` is where line `n + 1` begins (the end of line `n`).
    log[starts[total - lines - 1]..].to_owned()
}

/// The message of a daemon that never became ready: the last
/// [`READY_LOG_LINES`] lines of its log.
pub fn ready_failure_message(log: &str) -> String {
    format!(
        "memproved never became ready:\n{}",
        log_tail(log, READY_LOG_LINES)
    )
}

/// A test script that passes and prints a cargo-style summary.
pub fn passing(name: &str) -> String {
    format!(
        "#!/bin/sh\n# Check {name}.\necho 'test {name} ... ok'\necho 'test result: ok. 1 passed; 0 failed'\n"
    )
}

pub fn cli_binary() -> PathBuf {
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_memproved"));
    let cli = daemon.with_file_name("memprove");
    assert!(
        cli.exists(),
        "memprove not found next to {}; run `cargo test --workspace`",
        daemon.display()
    );
    cli
}

/// U10 T7 (Owner O4; ADR 0014 "Amendment (U10, S-T)"): the takeover hint
/// at the end of the `Session` line `line` (by its number and Session ID), as
/// the CLI under test writes it in this test's environment: the program is
/// `memprove` when the CLI's canonical directory is on `PATH`, else the
/// CLI's canonical path.
#[allow(dead_code)] // Each test file uses part of the support.
pub fn takeover_hint(line: &str) -> String {
    let words = line.split(' ').collect::<Vec<_>>();
    let (number, session) = (words[1], words[2]);
    let cli = std::fs::canonicalize(cli_binary()).unwrap();
    let dir = cli.parent().unwrap();
    let on_path = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|entry| std::fs::canonicalize(&entry).ok().as_deref() == Some(dir))
    });
    let program = if on_path {
        "memprove".to_owned()
    } else {
        cli.to_str().unwrap().to_owned()
    };
    format!(
        "; end it before taking over: {program} session end {session} --abandoned --reason <why> \
         (MCP: end {{session:{number}, abandoned:true}})"
    )
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

pub fn step(title: &str) {
    println!("\n===== {title} =====");
}

pub fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    // U1 (U-A, A1): `memprove mcp serve` takes `CLAUDE_CODE_SESSION_ID`
    // as its client session; a test sets it only on purpose.
    command.env_remove("CLAUDE_CODE_SESSION_ID");
    command
}

pub fn git(directory: &Path, arguments: &[&str]) -> String {
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

pub fn commit(directory: &Path, message: &str) -> String {
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

pub fn write(path: &Path, contents: &str) {
    println!("$ printf {contents:?} > {}", path.display());
    fs::write(path, contents).unwrap();
}

pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone)]
pub struct Cli {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub token: PathBuf,
    pub cwd: PathBuf,
    pub label: &'static str,
}

impl Cli {
    pub fn run(&self, words: &[&str], quiet: bool) -> Run {
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

    pub fn ok(&self, words: &[&str]) -> Value {
        let run = self.run(words, false);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }

    /// `memprove run` of `words` after `--`, with `flags` before it;
    /// returns the label it printed and the recorded Evidence.
    pub fn run_command(&self, flags: &[&str], words: &[&str]) -> (String, Value) {
        let all = [&["run"][..], flags, &["--"][..], words].concat();
        let run = self.run(&all, true);
        let last = run.stderr.lines().last().unwrap_or_default().to_owned();
        println!("[{}] $ ct {}\n{last}", self.label, all.join(" "));
        let (_, label) = last
            .rsplit_once(", label ")
            .unwrap_or_else(|| panic!("no label: {}", run.stderr));
        (
            label.to_owned(),
            serde_json::from_str(run.stdout.trim()).unwrap(),
        )
    }

    /// `memprove run --test-format cargo -- sh <script>`: a test run.
    pub fn test(&self, script: &str) -> String {
        self.run_command(&["--test-format", "cargo"], &["sh", script])
            .0
    }

    /// `memprove run -- sh <script>`: a command run, not a test run.
    pub fn command(&self, script: &str) -> String {
        self.run_command(&[], &["sh", script]).0
    }

    pub fn capsule(&self) -> Value {
        let run = self.run(&["resume"], true);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }

    /// The standard output of a command that must succeed, as is.
    pub fn bytes(&self, words: &[&str]) -> String {
        let run = self.run(words, true);
        assert_eq!(run.code, Some(0), "{words:?}: {}", run.stderr);
        run.stdout
    }

    /// The same CLI in another configuration directory: another context.
    pub fn elsewhere(&self, config_dir: PathBuf, label: &'static str) -> Self {
        Self {
            config_dir,
            label,
            ..self.clone()
        }
    }
}

pub struct Daemon {
    child: Child,
}

impl Daemon {
    pub fn start(cli: &Cli, log: &Path) -> Self {
        Self::start_within(cli, log, ready_timeout())
    }

    /// [`Daemon::start`], waiting at most `timeout` for `health ready`.
    pub fn start_within(cli: &Cli, log: &Path, timeout: Duration) -> Self {
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
        let deadline = Instant::now() + timeout;
        while cli.run(&["health", "ready"], true).code != Some(0) {
            assert!(
                Instant::now() < deadline,
                "{}",
                ready_failure_message(&fs::read_to_string(log).unwrap_or_default())
            );
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    pub fn stop(mut self) {
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

impl Daemon {
    /// Sends SIGKILL: no graceful shutdown, the socket file is left behind.
    pub fn kill(mut self) {
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
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

pub fn claim(capsule: &Value, number: u64) -> &Value {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["number"] == number)
        .unwrap_or_else(|| panic!("no claim #{number}"))
}

pub fn sequence(capsule: &Value) -> u64 {
    capsule["bindings"]["ledger"]["project_sequence"]
        .as_u64()
        .unwrap()
}

/// The append-only triggers of the recovery tables and the Event ledger.
const REJECT_TRIGGERS: [&str; 21] = [
    "evidence_reject_update",
    "evidence_reject_delete",
    "evidence_reject_replace",
    "repository_snapshots_reject_update",
    "repository_snapshots_reject_delete",
    "repository_snapshots_reject_replace",
    "repository_snapshot_evidence_reject_update",
    "repository_snapshot_evidence_reject_delete",
    "repository_snapshot_evidence_reject_replace",
    "checkpoints_reject_update",
    "checkpoints_reject_delete",
    "checkpoints_reject_replace",
    "checkpoint_items_reject_update",
    "checkpoint_items_reject_delete",
    "checkpoint_items_reject_replace",
    "checkpoint_item_evidence_reject_update",
    "checkpoint_item_evidence_reject_delete",
    "checkpoint_item_evidence_reject_replace",
    "events_reject_update",
    "events_reject_delete",
    "events_reject_replace",
];

pub struct Setup {
    pub root: tempfile::TempDir,
    pub root_path: PathBuf,
    pub repo: PathBuf,
    pub project_id: String,
    pub operator: Cli,
    pub agent: Cli,
    pub daemon: Option<Daemon>,
    starts: u32,
}

impl Setup {
    /// A repository with `README`, `.gitignore` (`flags/`, `out/`), and one
    /// passing test script per name in `tests/`, committed; the daemon
    /// running, the Project registered, an agent token issued.
    pub fn new(scripts: &[&str]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let repo = root_path.join("repo");
        fs::create_dir_all(repo.join("tests")).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(repo.join("README"), b"fixture\n").unwrap();
        fs::write(repo.join(".gitignore"), b"flags/\nout/\n").unwrap();
        for name in scripts {
            fs::write(repo.join(format!("tests/{name}.sh")), passing(name)).unwrap();
        }
        git(&repo, &["add", "."]);
        commit(&repo, "fixture");
        let operator = Cli {
            data_dir: root_path.join("data"),
            config_dir: root_path.join("config-operator"),
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
        let daemon = Daemon::start(&operator, &root_path.join("daemon-0.log"));
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
            config_dir: root_path.join("config"),
            label: "agent/mcp-read+mcp-write",
            ..operator.clone()
        };
        Self {
            root,
            root_path,
            repo,
            project_id,
            operator,
            agent,
            daemon: Some(daemon),
            starts: 0,
        }
    }

    pub fn stop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.stop();
        }
    }

    /// Kills the daemon with SIGKILL (P4-F: what survives a crash).
    pub fn kill(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.kill();
        }
    }

    pub fn start(&mut self) {
        assert!(self.daemon.is_none());
        self.starts += 1;
        let log = self.root_path.join(format!("daemon-{}.log", self.starts));
        self.daemon = Some(Daemon::start(&self.operator, &log));
    }

    /// The Project's state database.
    pub fn database(&self) -> PathBuf {
        self.operator
            .data_dir
            .join("projects")
            .join(&self.project_id)
            .join("state.sqlite")
    }

    /// Runs `sql` with `sqlite3` on the state database, the append-only
    /// triggers dropped: a writer that bypasses `memproved`. The daemon
    /// must be stopped.
    pub fn tamper(&self, sql: &str) {
        assert!(self.daemon.is_none(), "stop the daemon before tampering");
        let mut drops = String::new();
        for name in REJECT_TRIGGERS {
            write!(drops, "DROP TRIGGER IF EXISTS {name};").unwrap();
        }
        println!("$ sqlite3 state.sqlite {sql:?}");
        let output = Command::new("sqlite3")
            .arg(self.database())
            .arg(format!("PRAGMA foreign_keys = OFF; {drops} {sql}"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
    }

    /// One value `sqlite3` reads from the state database.
    pub fn query(&self, sql: &str) -> String {
        let output = Command::new("sqlite3")
            .arg(self.database())
            .arg(sql)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        text(&output.stdout).trim().to_owned()
    }

    /// A copy of the Project's data (`*.sqlite*` of its directory), to put
    /// back with [`Setup::restore`]. The daemon must be stopped.
    pub fn save(&self, name: &str) -> PathBuf {
        assert!(self.daemon.is_none());
        let copy = self.root_path.join(name);
        fs::create_dir_all(&copy).unwrap();
        for entry in fs::read_dir(self.database().parent().unwrap()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                fs::copy(&path, copy.join(path.file_name().unwrap())).unwrap();
            }
        }
        copy
    }

    /// Puts back a copy made by [`Setup::save`]. The daemon must be stopped.
    pub fn restore(&self, copy: &Path) {
        assert!(self.daemon.is_none());
        let directory = self.database().parent().unwrap().to_owned();
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() && !copy.join(path.file_name().unwrap()).exists() {
                fs::remove_file(path).unwrap();
            }
        }
        for entry in fs::read_dir(copy).unwrap() {
            let path = entry.unwrap().path();
            fs::copy(&path, directory.join(path.file_name().unwrap())).unwrap();
        }
    }
}

/// `checkpoint create` with `words`; asserts the exit code and returns the
/// run.
pub fn checkpoint(agent: &Cli, words: &[&str], code: i32) -> Run {
    let run = agent.run(&[&["checkpoint", "create"][..], words].concat(), false);
    assert_eq!(run.code, Some(code), "{}{}", run.stdout, run.stderr);
    run
}

/// The last `n` lines of `stderr`.
pub fn last_lines(stderr: &str, n: usize) -> Vec<String> {
    let lines = stderr.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}
