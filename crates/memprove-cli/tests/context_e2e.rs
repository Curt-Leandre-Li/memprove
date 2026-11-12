//! P2-D1 process-level tests: the current context, `begin`/`end`,
//! `run --test-format cargo`, the `run` exit code, `checkpoint create` from
//! flags, and standard input for `-`.
//!
//! The scenarios drive the real `memprove` and `memproved` binaries over
//! a real Cargo project in a Git repository, and never write a UUID by hand:
//! every identity comes from a command's output or the current context.
//!
//! a. In linked worktree W1: `begin` (new Task) → `run --test-format cargo`
//!    (PASS) → `checkpoint create --fact` → `end`; then in linked worktree W2:
//!    `begin --task` → `recover preview` is verified.
//! b. A failing test: `run` exits non-zero; `--fact` citing it is refused
//!    with a hint; `--open` is accepted; the Preview shows `run_status` FAIL,
//!    `evidence_not_passed` on the open item and its Evidence (P2-F), and no
//!    verified fact; `--text` names the command, the commit, and the
//!    outcome. Exit codes: the command's own, 125 for a program that cannot
//!    start and for a result that cannot be recorded.
//!
//! P2-H, in the root Workspace, where no command line but the first `begin`
//! carries a UUID (asserted):
//!
//! c. `begin --project --new-task` (the Task is now `in_progress`) → `run`
//!    (PASS) → `checkpoint create --fact` citing the latest run → commit →
//!    `recover preview` is stale → `run` again → `checkpoint create --fact`
//!    → `task complete` → `end` → `begin` is refused, naming `--new-task`.
//! d. `begin --new-task` → `task transition --to blocked` and back to
//!    `in_progress` from the context (a 400 before P2-H) → a `run` whose
//!    command prints its `MEMPROVE_*` variables records no token → no
//!    `end`: `begin` is refused, naming `end --reason` → `end` → `begin`
//!    without options continues the same Task.
//!
//! Run with `--nocapture` for the transcript; the agent token is printed as
//! `<redacted>`. `memproved` is taken from the same target directory as the
//! CLI, so run this through `cargo test --workspace`.

use std::{
    cell::RefCell,
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};

const TIMEOUT: Duration = Duration::from_secs(15);

const CARGO_TOML: &str = "[package]
name = \"demo\"
version = \"0.1.0\"
edition = \"2021\"

[dependencies]
";

const LIB_PASSING: &str = "/// Adds two numbers.
pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    #[test]
    fn adds() {
        assert_eq!(super::add(2, 2), 4);
    }

    #[test]
    fn adds_zero() {
        assert_eq!(super::add(0, 7), 7);
    }
}
";

const INTEGRATION_TEST: &str = "#[test]
fn adds_from_outside() {
    assert_eq!(demo::add(1, 2), 3);
}
";

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

fn cargo_binary() -> String {
    option_env!("CARGO").unwrap_or("cargo").to_owned()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn step(title: &str) {
    println!("\n### {title}");
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    println!("$ git -C {} {}", directory.display(), arguments.join(" "));
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    text(&output.stdout)
}

fn write(path: &Path, contents: &str) {
    println!("$ cat > {} <<'EOF'\n{contents}EOF", path.display());
    fs::write(path, contents).unwrap();
}

fn cargo_repository(path: &Path) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::create_dir_all(path.join("tests")).unwrap();
    git(path, &["init", "--quiet", "--initial-branch=main"]);
    write(&path.join("Cargo.toml"), CARGO_TOML);
    write(&path.join(".gitignore"), "/target\nCargo.lock\n");
    write(&path.join("src/lib.rs"), LIB_PASSING);
    write(&path.join("tests/outside.rs"), INTEGRATION_TEST);
    git(path, &["add", "."]);
    git(
        path,
        &["commit", "--quiet", "-m", "demo crate with three tests"],
    );
}

fn json(body: &str) -> JsonValue {
    parse_strict(body.trim()).unwrap_or_else(|error| panic!("not JSON ({error}): {body}"))
}

fn str_at<'a>(value: &'a JsonValue, path: &[&str]) -> &'a str {
    path.iter()
        .try_fold(value, |value, name| value.get(name))
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("{path:?} is not a string"))
}

fn items(value: &JsonValue, name: &str) -> Vec<JsonValue> {
    value
        .get(name)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("{name} is not an array"))
        .to_vec()
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
    cargo_target: PathBuf,
    token: PathBuf,
    cwd: PathBuf,
    label: &'static str,
}

impl Cli {
    fn at(&self, cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            ..self.clone()
        }
    }

    fn command(&self, words: &[&str]) -> Command {
        let mut command = isolated(env!("CARGO_BIN_EXE_memprove"));
        command
            .current_dir(&self.cwd)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token-file")
            .arg(&self.token)
            .args(words)
            .env_remove("MEMPROVE_DATA_DIR")
            .env_remove("MEMPROVE_TOKEN")
            .env_remove("MEMPROVE_TOKEN_FILE")
            .env("MEMPROVE_CONFIG_DIR", &self.config_dir)
            .env("CARGO_TARGET_DIR", &self.cargo_target)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        command
    }

    fn run_quiet(&self, words: &[&str]) -> Run {
        let output = self.command(words).output().unwrap();
        Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        }
    }

    fn run(&self, words: &[&str]) -> Run {
        let run = self.run_quiet(words);
        let shown = words
            .iter()
            .map(|word| {
                if word.contains(' ') {
                    format!("{word:?}")
                } else {
                    (*word).to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "[{}] (cwd {}) $ memprove {shown}\n{}{}[exit {}]",
            self.label,
            self.cwd.display(),
            run.stdout,
            run.stderr,
            run.code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string())
        );
        run
    }

    /// As [`Cli::run`], with the token in `MEMPROVE_TOKEN` instead of
    /// `--token-file`.
    fn run_with_env_token(&self, words: &[&str], secret: &str) -> Run {
        let mut command = isolated(env!("CARGO_BIN_EXE_memprove"));
        command
            .current_dir(&self.cwd)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .args(words)
            .env("MEMPROVE_TOKEN", secret)
            .env("MEMPROVE_CONFIG_DIR", &self.config_dir)
            .env("CARGO_TARGET_DIR", &self.cargo_target)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        let output = command.output().unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        println!(
            "[{}] (cwd {}) $ MEMPROVE_TOKEN=<redacted> memprove {}\n{}{}[exit {}]",
            self.label,
            self.cwd.display(),
            words
                .iter()
                .map(|word| if word.contains(' ') {
                    format!("{word:?}")
                } else {
                    (*word).to_owned()
                })
                .collect::<Vec<_>>()
                .join(" "),
            run.stdout.replace(secret, "<redacted>"),
            run.stderr.replace(secret, "<redacted>"),
            run.code
                .map_or_else(|| "signal".to_owned(), |code| code.to_string())
        );
        run
    }

    fn ok(&self, words: &[&str]) -> JsonValue {
        let run = self.run(words);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        json(&run.stdout)
    }
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
        println!(
            "$ memproved --data-dir <DATA> &   # pid {}",
            daemon.child.id()
        );
        let deadline = Instant::now() + TIMEOUT;
        while cli.run_quiet(&["health", "ready"]).code != Some(0) {
            assert!(
                Instant::now() < deadline,
                "memproved never became ready:\n{}",
                fs::read_to_string(log).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(self.child.id().to_string())
                .status();
            let deadline = Instant::now() + TIMEOUT;
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Setup {
    _root: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    operator: Cli,
    agent: Cli,
    project_id: String,
    _daemon: Daemon,
}

/// A Cargo project registered as a Project, a running daemon, the operator's
/// control token, and an agent token with only `mcp-read` and `mcp-write`.
fn setup() -> Setup {
    let root_dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root_dir.path()).unwrap();
    let repo = root.join("repo");
    step("setup: Cargo project, daemon, operator, agent token");
    cargo_repository(&repo);
    let operator = Cli {
        data_dir: root.join("data"),
        config_dir: root.join("config"),
        cargo_target: root.join("cargo-target"),
        token: root.join("control.token"),
        cwd: repo.clone(),
        label: "operator/control",
    };
    println!(
        "$ memproved bootstrap --data-dir <DATA> --output {}",
        operator.token.display()
    );
    let issued = isolated(daemon_binary())
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
    let daemon = Daemon::start(&operator, &root.join("daemon.log"));
    let project = operator.ok(&["project", "register", repo.to_str().unwrap()]);
    let project_id = str_at(&project, &["project_id"]).to_owned();
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
    let created = operator.run_quiet(&words);
    assert_eq!(created.code, Some(0), "{}", created.stderr);
    let secret = str_at(&json(&created.stdout), &["secret"]).to_owned();
    println!(
        "[operator/control] $ memprove {}\n{}\n[exit 0]",
        words.join(" "),
        created.stdout.trim().replace(&secret, "<redacted>")
    );
    let agent_token = root.join("agent.token");
    fs::write(&agent_token, &secret).unwrap();
    fs::set_permissions(&agent_token, fs::Permissions::from_mode(0o600)).unwrap();
    let agent = Cli {
        token: agent_token,
        label: "agent/mcp-read+mcp-write",
        ..operator.clone()
    };
    Setup {
        _root: root_dir,
        root,
        repo,
        operator,
        agent,
        project_id,
        _daemon: daemon,
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// The single context file under the configuration directory.
fn context_files(config_dir: &Path) -> Vec<PathBuf> {
    let directory = config_dir.join("contexts");
    let mut files = fs::read_dir(&directory)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    files
}

#[test]
#[allow(clippy::too_many_lines)] // Scenarios a and b, in order.
fn begin_run_checkpoint_end_and_recover_through_real_binaries() {
    let setup = setup();
    let (agent, project_id) = (&setup.agent, setup.project_id.as_str());
    let (w1, w2) = (setup.root.join("w1"), setup.root.join("w2"));
    git(
        &setup.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "work",
            w1.to_str().unwrap(),
        ],
    );
    git(
        &setup.repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "other",
            w2.to_str().unwrap(),
        ],
    );
    let elsewhere = setup.root.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();

    step("errors outside any Workspace");
    let outside = agent.at(&elsewhere).run(&["run", "--", "true"]);
    assert_eq!(outside.code, Some(2));
    assert!(
        outside
            .stderr
            .contains("is not inside a Workspace with a current context"),
        "{}",
        outside.stderr
    );
    let outside =
        agent
            .at(&elsewhere)
            .run(&["begin", "--project", project_id, "--new-task", "never"]);
    assert_eq!(outside.code, Some(1));
    assert!(
        outside.stderr.contains("404 workspace_not_resolved"),
        "{}",
        outside.stderr
    );
    assert!(
        outside
            .stderr
            .contains("is not inside a Workspace of Project")
    );
    assert!(context_files(&agent.config_dir).is_empty());

    step("a. W1: begin with a new Task");
    let a1 = agent.at(&w1);
    let begun = a1.ok(&[
        "begin",
        "--project",
        project_id,
        "--new-task",
        "Make the demo tests pass",
    ]);
    let task_id = str_at(&begun, &["task_id"]).to_owned();
    let session_a = str_at(&begun, &["session_id"]).to_owned();
    assert_eq!(str_at(&begun, &["workspace_root"]), w1.to_str().unwrap());
    let files = context_files(&agent.config_dir);
    assert_eq!(files.len(), 1);
    let context_file = files[0].clone();
    assert!(!context_file.starts_with(&w1));
    assert_eq!(mode(&context_file), 0o600);
    let stored = fs::read_to_string(&context_file).unwrap();
    println!("$ cat {}\n{stored}", context_file.display());
    let secret = fs::read_to_string(&agent.token).unwrap();
    assert!(!stored.contains(secret.trim()));
    assert_eq!(
        git(&w1, &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );

    step("a. W1: run the tests through cargo; the case count comes from the output");
    let cargo = cargo_binary();
    let passed = a1.run(&[
        "run",
        "--test-format",
        "cargo",
        "--",
        &cargo,
        "test",
        "--offline",
    ]);
    assert_eq!(passed.code, Some(0), "{}", passed.stderr);
    assert!(
        passed.stderr.contains("counted 3 test cases"),
        "{}",
        passed.stderr
    );
    let passed = json(&passed.stdout);
    assert_eq!(str_at(&passed, &["status"]), "PASS");
    assert_eq!(
        passed
            .get("payload")
            .and_then(|payload| payload.get("test_case_count"))
            .and_then(JsonValue::as_i64),
        Some(3)
    );
    let evidence_pass = str_at(&passed, &["evidence_id"]).to_owned();
    assert!(
        fs::read_to_string(&context_file)
            .unwrap()
            .contains(str_at(&passed, &["snapshot", "snapshot_id"]))
    );
    assert_eq!(
        git(&w1, &["status", "--porcelain", "--untracked-files=all"]),
        ""
    );

    step("a. W1: Checkpoint from flags, then end");
    let checkpoint = a1.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "the demo tests pass",
        "--evidence",
        &evidence_pass,
        "--next",
        "rerun cargo test after the next change",
    ]);
    assert_eq!(str_at(&checkpoint, &["session_id"]), session_a);
    let ended = a1.ok(&["end", "--reason", "tests pass; checkpoint recorded"]);
    assert_eq!(str_at(&ended, &["state"]), "ended");
    let again = a1.run(&["end"]);
    assert_eq!(again.code, Some(2));
    assert!(again.stderr.contains("has none"), "{}", again.stderr);

    step("a. W2: a new Session continues the Task; recover preview first");
    let a2 = agent.at(&w2);
    let begun_w2 = a2.ok(&["begin", "--project", project_id, "--task", &task_id]);
    assert_eq!(str_at(&begun_w2, &["task_id"]), task_id);
    assert_eq!(
        str_at(&begun_w2, &["task_title"]),
        "Make the demo tests pass"
    );
    assert_ne!(
        str_at(&begun_w2, &["workspace_id"]),
        str_at(&begun, &["workspace_id"])
    );
    let preview = a2.ok(&["recover", "preview"]);
    assert_eq!(str_at(&preview, &["status"]), "verified");
    assert_eq!(
        str_at(&preview, &["bindings", "probe", "workspace_id"]),
        str_at(&begun_w2, &["workspace_id"])
    );
    let facts = items(&preview, "facts");
    assert_eq!(str_at(&facts[0], &["verdict"]), "verified");
    assert_eq!(
        str_at(&items(&facts[0], "evidence")[0], &["run_status"]),
        "PASS"
    );
    let summary = a2.run(&["recover", "preview", "--text"]);
    assert_eq!(summary.code, Some(0));
    assert!(
        summary.stdout.starts_with("verified: "),
        "{}",
        summary.stdout
    );
    a2.ok(&["end", "--reason", "previewed only"]);

    step("b. W1: a new Task; a test is broken and committed");
    let begun_b = a1.ok(&["begin", "--new-task", "Keep the demo tests green"]);
    let session_b = str_at(&begun_b, &["session_id"]).to_owned();
    assert_eq!(str_at(&begun_b, &["project_id"]), project_id);
    write(
        &w1.join("src/lib.rs"),
        &LIB_PASSING.replace("super::add(2, 2), 4", "super::add(2, 2), 5"),
    );
    git(&w1, &["commit", "--quiet", "-am", "break adds"]);
    let broken_head = git(&w1, &["rev-parse", "HEAD"]).trim().to_owned();

    step("b. W1: the failing run exits with cargo's code and is recorded FAIL");
    let failed = a1.run(&[
        "run",
        "--test-format",
        "cargo",
        "--",
        &cargo,
        "test",
        "--offline",
    ]);
    assert_eq!(failed.code, Some(101), "{}", failed.stderr);
    let failed = json(&failed.stdout);
    assert_eq!(str_at(&failed, &["status"]), "FAIL");
    assert_eq!(
        failed
            .get("payload")
            .and_then(|payload| payload.get("test_case_count"))
            .and_then(JsonValue::as_i64),
        Some(2)
    );
    let evidence_fail = str_at(&failed, &["evidence_id"]).to_owned();

    step("b. W1: --fact citing the failed run is refused with a hint");
    let refused = a1.run(&[
        "checkpoint",
        "create",
        "--fact",
        "the demo tests pass",
        "--evidence",
        &evidence_fail,
        "--next",
        "fix adds",
    ]);
    assert_eq!(refused.code, Some(1));
    assert!(
        refused
            .stderr
            .contains("createCheckpoint failed: 400 invalid_request"),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("use --open"), "{}", refused.stderr);

    step("b. W1: the failure recorded with --open");
    a1.ok(&[
        "checkpoint",
        "create",
        "--open",
        "test adds fails: add(2, 2) is not 5",
        "--evidence",
        &evidence_fail,
        "--next",
        "fix the expectation in adds and rerun cargo test",
    ]);
    // P2-F: the real Preview carries evidence_not_passed and passes the
    // daemon's own response validation (a 200, not a 500).
    let preview = a1.ok(&["recover", "preview"]);
    let reasons = |value: &JsonValue| {
        items(value, "reasons")
            .iter()
            .map(|reason| reason.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(str_at(&preview, &["status"]), "partial");
    // No verified fact: the Checkpoint itself cannot be confirmed.
    assert_eq!(reasons(&preview), ["evidence_unavailable"]);
    let facts = items(&preview, "facts");
    let open = facts
        .iter()
        .find(|fact| str_at(fact, &["claim", "section"]) == "open_items")
        .expect("an open item");
    assert_eq!(str_at(open, &["classification"]), "unknown");
    assert_eq!(str_at(open, &["verdict"]), "partial");
    assert_eq!(reasons(open), ["evidence_not_passed"]);
    let evidence = items(open, "evidence");
    assert_eq!(str_at(&evidence[0], &["evidence_id"]), evidence_fail);
    assert_eq!(str_at(&evidence[0], &["run_status"]), "FAIL");
    assert_eq!(str_at(&evidence[0], &["verdict"]), "partial");
    assert_eq!(reasons(&evidence[0]), ["evidence_not_passed"]);
    assert_eq!(
        items(&evidence[0], "argv")
            .iter()
            .filter_map(JsonValue::as_str)
            .collect::<Vec<_>>(),
        [cargo.as_str(), "test", "--offline"]
    );
    for fact in &facts {
        assert_ne!(str_at(fact, &["verdict"]), "verified", "{fact:?}");
    }
    let summary = a1.run(&["recover", "preview", "--text"]);
    assert_eq!(summary.code, Some(0), "{}", summary.stderr);
    assert_eq!(
        summary.stdout.trim_end(),
        format!(
            "unconfirmed: the latest Checkpoint records no verified_fact (evidence bound to commit \
             {broken_head} (the same as the current HEAD))\n\
             command 「{cargo} test --offline」 on commit {broken_head} resulted in FAIL; the \
             evidence is still fresh, but the result is not PASS (reason: evidence_not_passed)"
        )
    );

    step("b. W1: run exit codes");
    let seven = a1.run(&["run", "--", "sh", "-c", "exit 7"]);
    assert_eq!(seven.code, Some(7));
    assert_eq!(str_at(&json(&seven.stdout), &["status"]), "FAIL");
    let missing = a1.run(&["run", "--", "/nonexistent/memprove-missing-program"]);
    assert_eq!(missing.code, Some(125));
    assert!(
        missing.stderr.contains("could not be started"),
        "{}",
        missing.stderr
    );
    assert_eq!(str_at(&json(&missing.stdout), &["status"]), "UNAVAILABLE");
    a1.ok(&["end", "--reason", "failure recorded"]);
    let no_session = a1.run(&["run", "--", "true"]);
    assert_eq!(no_session.code, Some(2));
    assert!(
        no_session.stderr.contains("has none"),
        "{}",
        no_session.stderr
    );
    let unrecorded = a1.run(&["run", "--session", &session_b, "--", "true"]);
    assert_eq!(unrecorded.code, Some(125));
    assert!(
        unrecorded.stderr.contains("409 invalid_transition"),
        "{}",
        unrecorded.stderr
    );
    assert!(
        unrecorded.stderr.contains("was not recorded"),
        "{}",
        unrecorded.stderr
    );

    step("ledger still verifies");
    let ledger = setup
        .operator
        .ok(&["ledger", "verify", "--project", project_id]);
    assert_eq!(ledger.get("valid").and_then(JsonValue::as_bool), Some(true));
}

/// The UUIDs written in `text` (8-4-4-4-12 hexadecimal digits).
fn uuids_in(text: &str) -> Vec<String> {
    let is_uuid = |window: &str| {
        window.char_indices().all(|(index, character)| {
            if [8, 13, 18, 23].contains(&index) {
                character == '-'
            } else {
                character.is_ascii_hexdigit()
            }
        })
    };
    (0..text.len())
        .filter_map(|start| text.get(start..start + 36))
        .filter(|window| is_uuid(window))
        .map(str::to_owned)
        .collect()
}

/// The demo crate with a `sub` function and its test: four test cases.
fn lib_with_sub() -> String {
    LIB_PASSING.replace(
        "#[cfg(test)]\nmod tests {",
        "/// Subtracts.\npub fn sub(left: u64, right: u64) -> u64 {\n    left - right\n}\n\n\
         #[cfg(test)]\nmod tests {\n    #[test]\n    fn subs() {\n        \
         assert_eq!(super::sub(5, 2), 3);\n    }\n",
    )
}

#[test]
#[allow(clippy::too_many_lines)] // Scenarios c and d, in order.
fn p2h_hand_over_complete_and_resume_without_ids() {
    let setup = setup();
    let agent = &setup.agent;
    let project_id = setup.project_id.as_str();
    let cargo = cargo_binary();
    let secret = fs::read_to_string(&agent.token).unwrap().trim().to_owned();
    // Every memprove command line of the scenarios, for the UUID check.
    let lines = RefCell::new(Vec::<String>::new());
    let run = |words: &[&str]| {
        lines.borrow_mut().push(words.join(" "));
        agent.run(words)
    };
    let ok = |words: &[&str]| {
        let done = run(words);
        assert_eq!(
            done.code,
            Some(0),
            "{words:?}: {}{}",
            done.stdout,
            done.stderr
        );
        json(&done.stdout)
    };
    let cargo_test = [
        "run",
        "--test-format",
        "cargo",
        "--",
        &cargo,
        "test",
        "--offline",
    ];

    step("c. begin a new Task: it is moved to in_progress");
    let begun = ok(&[
        "begin",
        "--project",
        project_id,
        "--new-task",
        "Add subtraction",
    ]);
    let task_c = str_at(&begun, &["task_id"]).to_owned();
    assert_eq!(str_at(&begun, &["task_state"]), "in_progress");
    // proposed (1) -> ready (2) -> in_progress (3).
    assert_eq!(
        begun.get("task_revision").and_then(JsonValue::as_i64),
        Some(3)
    );
    let shown = run(&["context", "show"]);
    assert_eq!(shown.code, Some(0), "{}", shown.stderr);
    for expected in [
        format!("Project:        {project_id}"),
        format!("Task:           {task_c} \"Add subtraction\" (state in_progress, revision 3)"),
        "Evidence:       none".to_owned(),
    ] {
        assert!(
            shown.stdout.contains(&expected),
            "{expected}\n{}",
            shown.stdout
        );
    }
    assert!(!shown.stdout.contains(&secret));

    step("c. run the tests (PASS), then a --fact citing the latest run");
    let passed = run(&cargo_test);
    assert_eq!(passed.code, Some(0), "{}", passed.stderr);
    let passed = json(&passed.stdout);
    assert_eq!(str_at(&passed, &["status"]), "PASS");
    let evidence_first = str_at(&passed, &["evidence_id"]).to_owned();
    let checkpoint = run(&[
        "checkpoint",
        "create",
        "--fact",
        "the demo tests pass",
        "--next",
        "add sub and its test",
    ]);
    assert_eq!(checkpoint.code, Some(0), "{}", checkpoint.stderr);
    assert!(
        checkpoint.stdout.contains(&evidence_first),
        "{}",
        checkpoint.stdout
    );

    step("c. commit a change: the Evidence no longer holds");
    write(&setup.repo.join("src/lib.rs"), &lib_with_sub());
    git(&setup.repo, &["commit", "--quiet", "-am", "add sub"]);
    let stale = ok(&["recover", "preview"]);
    assert_eq!(str_at(&stale, &["status"]), "stale");

    step("c. run again after the commit, Checkpoint, complete the Task, end");
    let rerun = run(&cargo_test);
    assert_eq!(rerun.code, Some(0), "{}", rerun.stderr);
    assert!(
        rerun.stderr.contains("counted 4 test cases"),
        "{}",
        rerun.stderr
    );
    let evidence_second = str_at(&json(&rerun.stdout), &["evidence_id"]).to_owned();
    let checkpoint = run(&[
        "checkpoint",
        "create",
        "--fact",
        "sub is implemented and the demo tests pass",
        "--next",
        "nothing left for this Task",
    ]);
    assert_eq!(checkpoint.code, Some(0), "{}", checkpoint.stderr);
    assert!(checkpoint.stdout.contains(&evidence_second));
    let completed = ok(&["task", "complete"]);
    assert_eq!(str_at(&completed, &["state"]), "completed");
    assert_eq!(
        items(&completed, "completion_evidence_ids")
            .iter()
            .filter_map(JsonValue::as_str)
            .collect::<Vec<_>>(),
        [evidence_second.as_str()]
    );
    ok(&["end", "--reason", "sub added; Task completed"]);
    let refused = run(&["begin"]);
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("is completed"),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("--new-task"), "{}", refused.stderr);

    step("d. begin a new Task; move it from the context (P2-H item 1)");
    let begun_d = ok(&["begin", "--new-task", "Document add and sub"]);
    let task_d = str_at(&begun_d, &["task_id"]).to_owned();
    assert_ne!(task_d, task_c);
    let blocked = ok(&["task", "transition", "--to", "blocked"]);
    assert_eq!(str_at(&blocked, &["state"]), "blocked");
    let resumed = ok(&["task", "transition", "--to", "in_progress"]);
    assert_eq!(str_at(&resumed, &["state"]), "in_progress");
    assert_eq!(resumed.get("revision").and_then(JsonValue::as_i64), Some(5));

    step("d. the command run gets no token, other MEMPROVE_* variables stay");
    let words = ["run", "--", "sh", "-c", "env | grep '^MEMPROVE_' | sort"];
    lines.borrow_mut().push(words.join(" "));
    let printed = agent.run_with_env_token(&words, &secret);
    assert_eq!(printed.code, Some(0), "{}", printed.stderr);
    // stderr shows the command's complete output; stdout is the Evidence.
    assert!(
        printed.stderr.contains("MEMPROVE_CONFIG_DIR="),
        "{}",
        printed.stderr
    );
    for shown in [&printed.stdout, &printed.stderr] {
        assert!(!shown.contains(&secret));
        assert!(!shown.contains("MEMPROVE_TOKEN="), "{shown}");
    }
    let tail = str_at(&json(&printed.stdout), &["payload", "stdout"]).to_owned();
    assert!(tail.contains("MEMPROVE_CONFIG_DIR="), "{tail}");
    assert!(!tail.contains(&secret) && !tail.contains("MEMPROVE_TOKEN="));

    step("d. no end: begin is refused while the Session is active");
    let session_d = str_at(&begun_d, &["session_id"]).to_owned();
    for words in [
        &["begin"][..],
        &["begin", "--new-task", "Something else"][..],
    ] {
        let refused = run(words);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert!(
            refused.stderr.contains("still active"),
            "{}",
            refused.stderr
        );
        assert!(
            refused.stderr.contains("memprove end --reason"),
            "{}",
            refused.stderr
        );
    }
    let shown = run(&["context", "show"]);
    assert!(shown.stdout.contains(&session_d), "{}", shown.stdout);

    step("d. end, then begin without options continues the same Task");
    ok(&["end", "--reason", "pausing the documentation"]);
    let continued = ok(&["begin"]);
    assert_eq!(str_at(&continued, &["task_id"]), task_d);
    assert_eq!(str_at(&continued, &["task_state"]), "in_progress");
    assert_ne!(str_at(&continued, &["session_id"]), session_d);
    run(&["context", "show"]);
    ok(&["end", "--reason", "scenario d done"]);

    step("no command line but the first begin carries a UUID");
    let lines = lines.into_inner();
    println!("{} command lines checked", lines.len());
    for (index, line) in lines.iter().enumerate() {
        let found = uuids_in(line);
        if index == 0 {
            assert_eq!(found, [project_id], "{line}");
        } else {
            assert!(found.is_empty(), "{line}");
        }
    }
    assert!(uuids_in(&task_c).len() == 1, "the detector recognises IDs");
}

/// A one-shot server on `socket` that records the request body and answers
/// `400 invalid_request`.
fn record_one_request(socket: &Path) -> thread::JoinHandle<String> {
    let listener = UnixListener::bind(socket).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let mut buffer = [0_u8; 4096];
        let body_start = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "the request ended early");
            raw.extend_from_slice(&buffer[..read]);
            if let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                break split + 4;
            }
        };
        let head = text(&raw[..body_start]);
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        while raw.len() < body_start + length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "the body ended early");
            raw.extend_from_slice(&buffer[..read]);
        }
        let problem = r#"{"type":"https://continuity.local/problems/invalid-request","title":"Invalid request","status":400,"code":"invalid_request","request_id":"018f47a0-0000-7000-8000-000000000001","retryable":false}"#;
        write!(
            stream,
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/problem+json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{problem}",
            problem.len()
        )
        .unwrap();
        text(&raw[body_start..body_start + length])
    })
}

#[test]
fn dash_reads_the_request_from_standard_input() {
    let root = tempfile::tempdir().unwrap();
    for (words, sent) in [
        (
            &["event", "ingest", "-"][..],
            "{\"envelope_version\":\"1.0\",\n \"payload\":{\"n\":1.50}}\n",
        ),
        (
            &["checkpoint", "create", "-"][..],
            "{\"schema_version\":1,\"objective\":\"from stdin\"}",
        ),
    ] {
        let socket = root.path().join(format!("{}.sock", words[0]));
        let server = record_one_request(&socket);
        let mut child = isolated(env!("CARGO_BIN_EXE_memprove"))
            .arg("--socket")
            .arg(&socket)
            .args(words)
            .env_remove("MEMPROVE_DATA_DIR")
            .env_remove("MEMPROVE_TOKEN_FILE")
            .env("MEMPROVE_TOKEN", "not-a-real-token")
            .env("MEMPROVE_CONFIG_DIR", root.path().join("config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(sent.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        println!(
            "$ printf %s {sent:?} | memprove --socket <SOCKET> {}\n{}{}[exit {:?}]",
            words.join(" "),
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        assert_eq!(server.join().unwrap(), sent, "{words:?}");
        assert_eq!(output.status.code(), Some(1));
        assert!(text(&output.stderr).contains("failed: 400 invalid_request"));
    }
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

/// The last line of standard error of `memprove run` sums up what was
/// recorded: the result status, the case count of a test run, and the
/// `evidence_id`. The JSON on standard output is unchanged.
#[test]
fn run_ends_standard_error_with_a_summary_line() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Summary line",
    ]);
    let last_line = |stderr: &str| {
        stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned()
    };

    step("a test run: status, case count, and evidence_id on the last line");
    let cargo = cargo_binary();
    let tested = agent.run(&[
        "run",
        "--test-format",
        "cargo",
        "--",
        &cargo,
        "test",
        "--offline",
    ]);
    assert_eq!(tested.code, Some(0), "{}", tested.stderr);
    let recorded = json(&tested.stdout);
    let line = last_line(&tested.stderr);
    assert!(line.starts_with("memprove: "), "{line}");
    assert!(line.contains(str_at(&recorded, &["status"])), "{line}");
    assert!(line.contains("3 test cases"), "{line}");
    assert!(line.contains(str_at(&recorded, &["evidence_id"])), "{line}");

    step("a command run: status and evidence_id, no case count");
    let failed = agent.run(&["run", "--", "false"]);
    assert_eq!(failed.code, Some(1), "{}", failed.stderr);
    let recorded = json(&failed.stdout);
    let line = last_line(&failed.stderr);
    assert!(line.starts_with("memprove: "), "{line}");
    assert!(line.contains(str_at(&recorded, &["status"])), "{line}");
    assert!(!line.contains("test cases"), "{line}");
    assert!(line.contains(str_at(&recorded, &["evidence_id"])), "{line}");
}

/// U5 H4 (D1 F3, O6): `run --test-format pytest` reads the case count from
/// pytest's summary line. The commands are `sh -c` scripts that print what
/// pytest prints (a real pytest is not needed): all passed, a failed case,
/// an error, skipped cases, no summary, and an explicit `--test-cases`.
#[test]
fn u5_h4_run_test_format_pytest_counts_cases_from_the_summary_line() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "pytest summaries",
    ]);
    // `(script, exit code, status, "counted N test cases" or none)`.
    let scripts: [(&str, i32, &str, Option<&str>); 6] = [
        (
            "printf '...\\n===== 5 passed in 0.12s =====\\n'",
            0,
            "PASS",
            Some("counted 5 test cases"),
        ),
        (
            "printf '=== 1 failed, 4 passed in 0.30s ===\\n'; exit 1",
            1,
            "FAIL",
            Some("counted 5 test cases"),
        ),
        (
            "printf '=== 3 passed, 1 error in 0.20s ===\\n'; exit 1",
            1,
            "FAIL",
            Some("counted 4 test cases"),
        ),
        (
            "printf '=== 3 passed, 2 skipped in 0.20s ===\\n'",
            0,
            "PASS",
            Some("counted 3 test cases"),
        ),
        (
            "printf '=== 5 skipped in 0.01s ===\\n'",
            0,
            "NOT_RUN",
            Some("counted 0 test cases"),
        ),
        ("echo hello", 0, "NOT_RUN", None),
    ];
    for (script, code, status, counted) in scripts {
        step(script);
        let run = agent.run(&["run", "--test-format", "pytest", "--", "sh", "-c", script]);
        assert_eq!(run.code, Some(code), "{}", run.stderr);
        assert_eq!(str_at(&json(&run.stdout), &["status"]), status, "{script}");
        match counted {
            Some(line) => assert!(
                run.stderr.contains(&format!("memprove: {line}\n")),
                "{}",
                run.stderr
            ),
            None => assert!(
                run.stderr.contains("could not count the test cases"),
                "{}",
                run.stderr
            ),
        }
    }

    step("an explicit --test-cases wins over the summary");
    let run = agent.run(&[
        "run",
        "--test-format",
        "pytest",
        "--test-cases",
        "9",
        "--",
        "sh",
        "-c",
        "echo no summary here",
    ]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(str_at(&json(&run.stdout), &["status"]), "PASS");
    assert!(!run.stderr.contains("counted"), "{}", run.stderr);
    let evidence = setup.operator.ok(&[
        "evidence",
        "get",
        str_at(&json(&run.stdout), &["evidence_id"]),
    ]);
    let count = evidence
        .get("payload")
        .and_then(|payload| payload.get("test_case_count"))
        .and_then(JsonValue::as_i64);
    assert_eq!(count, Some(9));
}

/// P3-L L10: inside a Workspace with a current context `ledger verify` needs
/// no `--project`; an explicit `--project` wins; outside any context it is a
/// usage error that names both ways to give the Project.
#[test]
fn l10_ledger_verify_takes_the_project_from_the_context_through_real_binaries() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Ledger verify from the context",
    ]);

    step("no --project: the context's Project is verified");
    let implicit = agent.run(&["ledger", "verify"]);
    assert_eq!(implicit.code, Some(0), "{}", implicit.stderr);
    let report = json(&implicit.stdout);
    assert_eq!(str_at(&report, &["project_id"]), setup.project_id);
    assert_eq!(report.get("valid").and_then(JsonValue::as_bool), Some(true));

    step("an explicit --project is the same Project here");
    let explicit = agent.run(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(explicit.code, Some(0), "{}", explicit.stderr);
    assert_eq!(json(&explicit.stdout), report);

    step("an explicit --project wins over the context's");
    let unknown = "01a10000-0000-7000-8000-000000000001";
    let other = agent.run(&["ledger", "verify", "--project", unknown]);
    assert_eq!(other.code, Some(1), "{}{}", other.stdout, other.stderr);
    assert!(other.stdout.contains(r#""code":"#), "{}", other.stdout);

    step("outside any context: a usage error naming both ways");
    let elsewhere = setup.root.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let outside = agent.at(&elsewhere).run(&["ledger", "verify"]);
    assert_eq!(outside.code, Some(2), "{}", outside.stderr);
    assert!(outside.stdout.is_empty());
    assert!(
        outside.stderr.contains("--project <PROJECT_ID>")
            && outside.stderr.contains("current context")
            && outside.stderr.contains("`memprove begin`"),
        "{}",
        outside.stderr
    );
}

/// P3-L L11 (O3): `task complete` without `--evidence` cites every citable
/// passed run of the current state and says which labels it cited on standard
/// error; a pass that a later failing run of its argv overruled is cited
/// again once that argv passes (before that the default is refused, see
/// `l11_task_complete_refuses_while_a_run_fails_through_real_binaries`);
/// `--evidence` is not widened.
#[test]
#[allow(clippy::too_many_lines)] // Scenarios a, b, c, in order.
fn l11_task_complete_cites_all_citable_passed_runs_through_real_binaries() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    let evidence_of = |run: &Run| str_at(&json(&run.stdout), &["evidence_id"]).to_owned();
    let ids_of = |completed: &JsonValue| {
        items(completed, "completion_evidence_ids")
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let last_line = |stderr: &str| {
        stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned()
    };

    step("a. three passed runs of three commands: all three are cited");
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Complete citing all passed runs",
    ]);
    let first = agent.run(&["run", "--test", "--test-cases", "1", "--", "echo", "one"]);
    let second = agent.run(&["run", "--", "echo", "two"]);
    let third = agent.run(&["run", "--", "echo", "three"]);
    for run in [&first, &second, &third] {
        assert_eq!(run.code, Some(0), "{}", run.stderr);
    }
    let all = [&first, &second, &third].map(evidence_of);
    let completed = agent.run(&["task", "complete"]);
    assert_eq!(completed.code, Some(0), "{}", completed.stderr);
    assert_eq!(ids_of(&json(&completed.stdout)), all);
    assert!(
        completed.stderr.contains("citing the runs r1, r2, r3"),
        "{}",
        completed.stderr
    );
    agent.ok(&["end", "--reason", "completed with three runs"]);

    step(
        "b. a pass a later failing run of its argv overrules: refused until the argv passes again",
    );
    agent.ok(&[
        "begin",
        "--new-task",
        "Complete after the overruling run passes",
    ]);
    let flag = setup.root.join("flag");
    fs::write(&flag, "x").unwrap();
    let probe = format!("test -e {}", flag.display());
    let passing = agent.run(&["run", "--", "sh", "-c", &probe]);
    assert_eq!(passing.code, Some(0), "{}", passing.stderr);
    fs::remove_file(&flag).unwrap();
    let failing = agent.run(&["run", "--", "sh", "-c", &probe]);
    assert_eq!(failing.code, Some(1), "{}", failing.stderr);
    assert!(
        last_line(&failing.stderr).contains("label r2"),
        "{}",
        failing.stderr
    );
    let other = agent.run(&["run", "--", "echo", "three"]);
    assert_eq!(other.code, Some(0), "{}", other.stderr);
    assert!(
        other.stderr.contains("r1 (PASS, overruled by r2)"),
        "{}",
        other.stderr
    );
    let refused = agent.run(&["task", "complete"]);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(refused.stderr.contains("r2 (FAIL)"), "{}", refused.stderr);
    fs::write(&flag, "x").unwrap();
    let again = agent.run(&["run", "--", "sh", "-c", &probe]);
    assert_eq!(again.code, Some(0), "{}", again.stderr);
    let completed = agent.run(&["task", "complete"]);
    assert_eq!(completed.code, Some(0), "{}", completed.stderr);
    assert_eq!(
        ids_of(&json(&completed.stdout)),
        [
            evidence_of(&passing),
            evidence_of(&other),
            evidence_of(&again)
        ],
        "the restored pass, the other command, and the rerun; not the failed run"
    );
    assert!(
        completed.stderr.contains("citing the runs r1, r3, r4"),
        "{}",
        completed.stderr
    );
    agent.ok(&["end", "--reason", "completed after the rerun"]);

    step("c. an explicit --evidence is cited as given");
    agent.ok(&["begin", "--new-task", "Complete citing one run"]);
    let one = agent.run(&["run", "--", "echo", "one"]);
    let two = agent.run(&["run", "--", "echo", "two"]);
    assert_eq!(one.code.max(two.code), Some(0));
    let completed = agent.run(&["task", "complete", "--evidence", "r1"]);
    assert_eq!(completed.code, Some(0), "{}", completed.stderr);
    assert_eq!(ids_of(&json(&completed.stdout)), [evidence_of(&one)]);
    assert!(
        !completed.stderr.contains("citing the runs"),
        "{}",
        completed.stderr
    );
}

/// P3-L L12: the `workspace_id` of a Task response (`task complete`
/// included) is the Task's own Workspace, the one `task create --workspace`
/// scoped it to, and `null` for the Task `begin --new-task` creates, which
/// has none; it is not the context's Workspace. The `workspace_id` the CLI
/// sends with `task complete` is where the Evidence is judged (the
/// `TaskTransitionRequest` schema of the `OpenAPI` document). The guide says so.
#[test]
fn l12_task_response_workspace_id_is_the_tasks_own_workspace() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Unscoped Task",
    ]);
    let workspace_id = str_at(&begun, &["workspace_id"]).to_owned();

    step("a task complete of the unscoped begin Task answers workspace_id null");
    let passed = agent.run(&["run", "--", "echo", "done"]);
    assert_eq!(passed.code, Some(0), "{}", passed.stderr);
    let completed = agent.ok(&["task", "complete"]);
    assert_eq!(str_at(&completed, &["state"]), "completed");
    assert_eq!(completed.get("workspace_id"), Some(&JsonValue::Null));
    agent.ok(&["end", "--reason", "unscoped Task completed"]);

    step("a Task created with --workspace keeps it in every Task response");
    let created = agent.ok(&[
        "task",
        "create",
        "--project",
        &setup.project_id,
        "--title",
        "Scoped Task",
        "--workspace",
        &workspace_id,
    ]);
    assert_eq!(str_at(&created, &["workspace_id"]), workspace_id);
    let task_id = str_at(&created, &["task_id"]).to_owned();
    let moved = agent.ok(&[
        "task",
        "transition",
        &task_id,
        "--to",
        "ready",
        "--revision",
        "1",
    ]);
    assert_eq!(str_at(&moved, &["state"]), "ready");
    assert_eq!(str_at(&moved, &["workspace_id"]), workspace_id);
    let listed = agent.ok(&["task", "list", "--project", &setup.project_id]);
    let scoped = items(&listed, "items")
        .into_iter()
        .find(|task| str_at(task, &["task_id"]) == task_id)
        .expect("the scoped Task is listed");
    assert_eq!(str_at(&scoped, &["workspace_id"]), workspace_id);
}

/// P3-L L11 patch: `task complete` without `--evidence` is a usage error
/// (exit 2, nothing sent, the Task untouched) while the latest run of some
/// command of the current state did not pass, and it names that run. An
/// explicit `--evidence` is cited as given; once the failing command passes
/// again the default completes, citing the passed runs.
#[test]
fn l11_task_complete_refuses_while_a_run_fails_through_real_binaries() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    let evidence_of = |run: &Run| str_at(&json(&run.stdout), &["evidence_id"]).to_owned();
    let ids_of = |completed: &JsonValue| {
        items(completed, "completion_evidence_ids")
            .iter()
            .filter_map(JsonValue::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let task_line = |shown: &Run| {
        shown
            .stdout
            .lines()
            .find(|line| line.starts_with("Task:"))
            .unwrap_or_default()
            .to_owned()
    };

    step("a. r1 (a test) passes, r2 (another command) fails: the default is refused");
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Refuse while a run fails",
    ]);
    let first = agent.run(&["run", "--test", "--test-cases", "1", "--", "echo", "one"]);
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let failing = agent.run(&["run", "--", "false"]);
    assert_eq!(failing.code, Some(1), "{}", failing.stderr);
    let before = task_line(&agent.run(&["context", "show"]));
    let refused = agent.run(&["task", "complete"]);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(refused.stdout.is_empty(), "{}", refused.stdout);
    assert!(refused.stderr.contains("r2 (FAIL)"), "{}", refused.stderr);
    assert!(!refused.stderr.contains("r1 (FAIL)"), "{}", refused.stderr);
    assert!(refused.stderr.contains("--evidence"), "{}", refused.stderr);
    assert!(
        !refused.stderr.contains("failed: "),
        "no request was answered: {}",
        refused.stderr
    );
    let after = task_line(&agent.run(&["context", "show"]));
    assert_eq!(before, after, "the Task is untouched");
    assert!(after.contains("state in_progress"), "{after}");

    step("a. an explicit --evidence r1 is cited as given");
    let completed = agent.run(&["task", "complete", "--evidence", "r1"]);
    assert_eq!(completed.code, Some(0), "{}", completed.stderr);
    assert_eq!(ids_of(&json(&completed.stdout)), [evidence_of(&first)]);
    assert!(
        !completed.stderr.contains("citing the runs"),
        "{}",
        completed.stderr
    );
    agent.ok(&["end", "--reason", "completed citing r1 explicitly"]);

    step("b. r1 passes, r2 fails, r3 is r2's command and passes: the default completes");
    agent.ok(&[
        "begin",
        "--new-task",
        "Complete once the failing run passes",
    ]);
    let flag = setup.root.join("flag-l11-patch");
    let probe = format!("test -e {}", flag.display());
    let one = agent.run(&["run", "--test", "--test-cases", "1", "--", "echo", "one"]);
    assert_eq!(one.code, Some(0), "{}", one.stderr);
    let missing = agent.run(&["run", "--", "sh", "-c", &probe]);
    assert_eq!(missing.code, Some(1), "{}", missing.stderr);
    let refused = agent.run(&["task", "complete"]);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(refused.stderr.contains("r2 (FAIL)"), "{}", refused.stderr);
    fs::write(&flag, "x").unwrap();
    let present = agent.run(&["run", "--", "sh", "-c", &probe]);
    assert_eq!(present.code, Some(0), "{}", present.stderr);
    let completed = agent.run(&["task", "complete"]);
    assert_eq!(completed.code, Some(0), "{}", completed.stderr);
    assert_eq!(
        ids_of(&json(&completed.stdout)),
        [evidence_of(&one), evidence_of(&present)],
        "r1 and r3 are cited, not the failed r2"
    );
    assert!(
        completed.stderr.contains("citing the runs r1, r3"),
        "{}",
        completed.stderr
    );
    agent.ok(&["end", "--reason", "completed after the rerun passed"]);
}

/// The `detail` of the Problem a refused command printed on standard output.
fn problem_detail(refused: &Run) -> String {
    str_at(&json(&refused.stdout), &["detail"]).to_owned()
}

/// Each `(statement, evidence index)` the first sentence of an overruled
/// detail names, as `$.verified_completed_work[i].evidence_ids[j]`.
fn overruled_positions(detail: &str) -> Vec<(usize, usize)> {
    let first = &detail[..detail.find("]. ").map_or(detail.len(), |at| at + 2)];
    let prefix = "$.verified_completed_work[";
    first
        .match_indices(prefix)
        .map(|(at, _)| {
            let rest = &first[at + prefix.len()..];
            let statement = rest[..rest.find(']').unwrap()].parse().unwrap();
            let rest = &rest[rest.find("evidence_ids[").unwrap() + "evidence_ids[".len()..];
            (statement, rest[..rest.find(']').unwrap()].parse().unwrap())
        })
        .collect()
}

/// `count` passed command runs of their own argv (r1..r<count>), a passed
/// test run that stands (r<count+1>), then a failed run of each of the
/// first `count` argv: the passed runs' Evidence IDs and the failed ones'.
fn overruled_runs(setup: &Setup, agent: &Cli, count: usize) -> (Vec<String>, Vec<String>) {
    let probes = (0..count)
        .map(|index| {
            let flag = setup.root.join(format!("flag-s1-{count}-{index}"));
            fs::write(&flag, "x").unwrap();
            (flag.clone(), format!("test -e {}", flag.display()))
        })
        .collect::<Vec<_>>();
    let mut passed = Vec::new();
    for (_, probe) in &probes {
        let run = agent.run(&["run", "--", "sh", "-c", probe]);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        passed.push(str_at(&json(&run.stdout), &["evidence_id"]).to_owned());
    }
    let standing = agent.run(&["run", "--test", "--test-cases", "1", "--", "echo", "ok"]);
    assert_eq!(standing.code, Some(0), "{}", standing.stderr);
    let mut failed = Vec::new();
    for (flag, probe) in &probes {
        fs::remove_file(flag).unwrap();
        let run = agent.run(&["run", "--", "sh", "-c", probe]);
        assert_eq!(run.code, Some(1), "{}", run.stderr);
        failed.push(str_at(&json(&run.stdout), &["evidence_id"]).to_owned());
    }
    (passed, failed)
}

/// `checkpoint create` with one `--fact` per entry of `cited`, citing its
/// labels in order.
fn checkpoint_citing(agent: &Cli, cited: &[Vec<String>]) -> Run {
    let mut words = vec!["checkpoint".to_owned(), "create".to_owned()];
    for (fact, labels) in cited.iter().enumerate() {
        words.extend(["--fact".to_owned(), format!("checks of fact {fact} pass")]);
        for label in labels {
            words.extend(["--evidence".to_owned(), label.clone()]);
        }
    }
    words.extend(["--next".to_owned(), "go on".to_owned()]);
    agent.run(&words.iter().map(String::as_str).collect::<Vec<_>>())
}

/// The descriptions of an overruled detail are whole, in order, and the
/// detail ends with the last one or with how many it left out; the number
/// described.
fn assert_whole_descriptions(detail: &str, count: usize) -> usize {
    let described = (1..=count)
        .filter(|number| detail.contains(&format!(" ({number}) Evidence ")))
        .count();
    for number in 1..=described {
        let at = detail.find(&format!(" ({number}) Evidence ")).unwrap();
        let item = &detail[at..];
        let end = item.find(" passes.").unwrap_or_else(|| panic!("{detail}"));
        let next = item[1..].find(" (").map_or(item.len(), |at| at + 1);
        assert!(end < next, "description {number} is cut: {detail}");
    }
    if described < count {
        assert!(
            detail.ends_with(&format!(" {} more not described.", count - described)),
            "{detail}"
        );
    } else {
        assert!(detail.ends_with(" rerun until it passes."), "{detail}");
    }
    described
}

/// P3-L3 (Owner, T16 S-1): with 2, 3, and 8 overruled citations in one
/// `checkpoint create` (8 across two facts), the `400` detail names every
/// overruled position in its first sentence, stays within 512 characters,
/// describes whole citations only and ends by saying how many it left
/// undescribed; dropping exactly the named positions makes the retry
/// succeed. Each overruled citation is a passed command run of its own argv
/// whose later run failed; each fact also cites one passed test run that
/// stands.
#[test]
fn l3_s1_every_overruled_citation_is_named_and_the_retry_succeeds() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    for (count, facts) in [(2_usize, 1_usize), (3, 1), (8, 2)] {
        step(&format!("{count} overruled citations in {facts} fact(s)"));
        let mut begin = vec!["begin"];
        if count == 2 {
            begin.extend(["--project", &setup.project_id]);
        }
        let title = format!("Overruled citations: {count}");
        begin.extend(["--new-task", &title]);
        agent.ok(&begin);
        let (passed, failed) = overruled_runs(&setup, &agent, count);
        // Labels: r1..r<count> passed and are overruled, r<count+1> is the
        // standing test. Each fact cites its share of the overruled runs in
        // reverse order with the standing test second.
        let standing = format!("r{}", count + 1);
        let per_fact = count / facts;
        let cited = (0..facts)
            .map(|fact| {
                let mut labels = (fact * per_fact..(fact + 1) * per_fact)
                    .rev()
                    .map(|index| format!("r{}", index + 1))
                    .collect::<Vec<_>>();
                labels.insert(1, standing.clone());
                labels
            })
            .collect::<Vec<_>>();
        let refused = checkpoint_citing(&agent, &cited);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert_eq!(str_at(&json(&refused.stdout), &["code"]), "invalid_request");
        let detail = problem_detail(&refused);
        assert!(refused.stderr.contains(&detail), "{}", refused.stderr);
        assert!(detail.chars().count() <= 512, "{detail}");

        // The first sentence names exactly the overruled positions, in
        // request order.
        let expected = cited
            .iter()
            .enumerate()
            .flat_map(|(fact, labels)| {
                labels
                    .iter()
                    .enumerate()
                    .filter(|(_, label)| **label != standing)
                    .map(move |(index, _)| (fact, index))
            })
            .collect::<Vec<_>>();
        assert_eq!(expected.len(), count);
        let named = overruled_positions(&detail);
        assert_eq!(named, expected, "{detail}");

        // Whole descriptions only, then how many are left.
        let described = assert_whole_descriptions(&detail, count);
        if count == 2 {
            assert_eq!(described, 2, "{detail}");
            for id in passed.iter().chain(&failed) {
                assert!(detail.contains(id.as_str()), "{detail}");
            }
        }

        step("drop exactly the named positions and retry");
        let retried = cited
            .iter()
            .enumerate()
            .map(|(fact, labels)| {
                labels
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !named.contains(&(fact, *index)))
                    .map(|(_, label)| label.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let accepted = checkpoint_citing(&agent, &retried);
        assert_eq!(accepted.code, Some(0), "{}", accepted.stderr);
        agent.ok(&["end", "--reason", "overruled citations dropped"]);
    }
}

/// Runs `sql` with `sqlite3` on the Project's state database, with the
/// append-only trigger `trigger` dropped: a writer that bypasses
/// `memproved`.
fn tamper(setup: &Setup, trigger: &str, sql: &str) {
    let database = setup
        .operator
        .data_dir
        .join("projects")
        .join(&setup.project_id)
        .join("state.sqlite");
    let script = format!("PRAGMA foreign_keys = OFF; DROP TRIGGER IF EXISTS {trigger}; {sql}");
    println!("$ sqlite3 {} {script:?}", database.display());
    let output = Command::new("sqlite3")
        .arg(&database)
        .arg(&script)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
}

/// P3-L3 (Owner, T16 S-2): completing a `tampered` Task is `409
/// evidence_stale` with a detail that says the integrity check failed, a
/// rerun does not help, and `ledger verify` shows the record; `task
/// complete` shows it and no longer advises a rerun, with or without
/// `--evidence`. The Task stays `in_progress`.
#[test]
fn l3_s2_completing_a_tampered_task_says_why_and_advises_no_rerun() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Complete a tampered Task",
    ]);
    let passed = agent.run(&["run", "--test", "--test-cases", "1", "--", "echo", "ok"]);
    assert_eq!(passed.code, Some(0), "{}", passed.stderr);
    let checkpoint = agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "the check passes",
        "--evidence",
        "r1",
        "--open",
        "docs",
        "--next",
        "complete the Task",
    ]);
    let checkpoint_id = str_at(&checkpoint, &["checkpoint_id"]).to_owned();

    step("an open item of the Checkpoint is rewritten behind the daemon's back");
    tamper(
        &setup,
        "checkpoint_items_reject_update",
        &format!(
            "UPDATE checkpoint_items SET text = 'rewritten' WHERE checkpoint_id = \
             '{checkpoint_id}' AND section = 'open_items' AND ordinal = 0;"
        ),
    );
    let verified = agent.run(&["ledger", "verify"]);
    assert_eq!(verified.code, Some(5), "{}", verified.stderr);

    for words in [
        &["task", "complete"][..],
        &["task", "complete", "--evidence", "r1"][..],
    ] {
        step(&format!("{}: refused with the reason", words.join(" ")));
        let refused = agent.run(words);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert_eq!(str_at(&json(&refused.stdout), &["code"]), "evidence_stale");
        let detail = problem_detail(&refused);
        for phrase in [
            "integrity check",
            "a rerun does not help",
            "`memprove ledger verify`",
        ] {
            assert!(detail.contains(phrase), "{phrase:?} not in {detail}");
        }
        assert!(
            refused
                .stderr
                .contains(&format!("409 evidence_stale: {detail}")),
            "{}",
            refused.stderr
        );
        for advice in ["Commit first", "rerun the tests", "try again"] {
            assert!(
                !refused.stderr.contains(advice),
                "{advice:?}: {}",
                refused.stderr
            );
        }
    }
    let shown = agent.run(&["context", "show"]);
    assert!(
        shown.stdout.contains("state in_progress"),
        "{}",
        shown.stdout
    );
}

/// P3-L3 (Owner, T16 N7): the second rule of L2 through the real binaries.
/// A test of one argv passes (r1) and a fact cites it; then a test run of the
/// same argv runs no case (r2, `NOT_RUN`) and a command run of it passes
/// (r3). The fact is no longer verified, a new fact citing r1 is refused
/// with a detail that names r2 and asks for a passed test run, and the Task
/// is not completed on r1, nor by default.
#[test]
fn l3_n7_a_test_without_cases_overrules_a_test_pass_despite_a_command_pass() {
    let setup = setup();
    let agent = setup.agent.at(&setup.repo);
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Test, empty test, command",
    ]);
    let argv = ["sh", "-c", "echo n7"];
    let run_as = |prefix: &[&str]| {
        let mut words = vec!["run"];
        words.extend(prefix);
        words.push("--");
        words.extend(argv);
        let run = agent.run(&words);
        json(&run.stdout)
    };
    let first = run_as(&["--test", "--test-cases", "1"]);
    assert_eq!(str_at(&first, &["status"]), "PASS");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "the n7 check passes",
        "--evidence",
        "r1",
        "--next",
        "go on",
    ]);
    let capsule = agent.ok(&["resume"]);
    assert_eq!(
        str_at(&items(&capsule, "claims")[0], &["verdict"]),
        "verified"
    );

    step("a test run with no case, then a passed command run of the same argv");
    let empty = run_as(&["--test", "--test-cases", "0"]);
    assert_eq!(str_at(&empty, &["status"]), "NOT_RUN");
    let command = run_as(&[]);
    assert_eq!(str_at(&command, &["status"]), "PASS");

    step("the fact is no longer verified");
    let capsule = agent.ok(&["resume"]);
    let fact = &items(&capsule, "claims")[0];
    assert_eq!(str_at(fact, &["kind"]), "fact");
    assert_eq!(str_at(fact, &["verdict"]), "partial");
    assert_ne!(str_at(&capsule, &["status"]), "verified");
    let preview = agent.ok(&["recover", "preview"]);
    assert_eq!(
        str_at(&items(&preview, "facts")[0], &["verdict"]),
        "partial"
    );

    step("a new fact citing r1 is refused, naming the empty test run");
    let refused = agent.run(&[
        "checkpoint",
        "create",
        "--fact",
        "the n7 check still passes",
        "--evidence",
        "r1",
        "--next",
        "go on",
    ]);
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    let detail = problem_detail(&refused);
    let empty_id = str_at(&empty, &["evidence_id"]);
    assert!(
        detail.contains(&format!(
            "(1) test run Evidence {} passed; the latest test run {empty_id} did not pass",
            str_at(&first, &["evidence_id"])
        )),
        "{detail}"
    );
    assert!(detail.contains("needs a later passed test run"), "{detail}");
    assert!(
        !detail.contains(str_at(&command, &["evidence_id"])),
        "{detail}"
    );

    step("the Task is not completed on r1, nor by default");
    for words in [
        &["task", "complete", "--evidence", "r1"][..],
        &["task", "complete"][..],
    ] {
        let refused = agent.run(words);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert_eq!(str_at(&json(&refused.stdout), &["code"]), "evidence_stale");
    }
    let shown = agent.run(&["context", "show"]);
    assert!(
        shown.stdout.contains("state in_progress"),
        "{}",
        shown.stdout
    );
}
