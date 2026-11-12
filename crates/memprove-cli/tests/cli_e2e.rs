//! Process-level tests: the real `memprove` CLI against the real `memproved`.
//!
//! `memproved` is taken from the same target directory as the CLI binary, so
//! run these through `cargo test --workspace` (which builds both binaries in one
//! invocation) to avoid exercising a stale daemon build.

use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

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

fn cli(data_dir: &Path, words: &[&str]) -> Output {
    isolated(env!("CARGO_BIN_EXE_memprove"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(words)
        .env_remove("MEMPROVE_DATA_DIR")
        .env_remove("MEMPROVE_TOKEN")
        .output()
        .unwrap()
}

/// As [`cli`], with a made-up token in `MEMPROVE_TOKEN` (U6 K1: a command
/// that needs a token is refused locally without one). No daemon accepts it,
/// and these callers have no daemon.
fn cli_with_token(data_dir: &Path, words: &[&str]) -> Output {
    isolated(env!("CARGO_BIN_EXE_memprove"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(words)
        .env_remove("MEMPROVE_DATA_DIR")
        .env("MEMPROVE_TOKEN", "not-a-real-token")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Daemon {
    fn start(data_dir: &Path, log: &Path) -> Self {
        let child = isolated(daemon_binary())
            .arg("--data-dir")
            .arg(data_dir)
            .env_remove("MEMPROVE_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(log).unwrap())
            .spawn()
            .unwrap();
        let daemon = Self {
            child,
            log: log.to_path_buf(),
        };
        let deadline = Instant::now() + TIMEOUT;
        while !cli(data_dir, &["health", "ready"]).status.success() {
            assert!(
                Instant::now() < deadline,
                "memproved never became ready; stderr:\n{}",
                fs::read_to_string(&daemon.log).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn terminate(mut self) -> Option<i32> {
        let sent = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code();
            }
            assert!(Instant::now() < deadline, "memproved did not exit");
            thread::sleep(Duration::from_millis(20));
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

#[test]
fn cli_calls_all_four_operations_through_the_daemon() {
    let root = tempfile::tempdir().unwrap();
    let data_dir = root.path().join("data");
    let daemon = Daemon::start(&data_dir, &root.path().join("daemon.log"));

    for words in [&["health", "live"][..], &["health", "ready"][..]] {
        let output = cli(&data_dir, words);
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        assert_eq!(text(&output.stdout).trim(), r#"{"status":"ok"}"#);
    }

    // U6 K1: with no token the CLI names the places it looked and sends
    // nothing (before: the daemon's bare 401, `getStatus failed: 401
    // authentication_required`, on stdout and stderr).
    for words in [&["status"][..], &["version"][..]] {
        let output = cli(&data_dir, words);
        assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
        assert_eq!(text(&output.stdout), "");
        let stderr = text(&output.stderr);
        assert!(
            stderr.starts_with("memprove: error: no token: --token-file was not given;")
                && stderr.contains("MEMPROVE_TOKEN_FILE")
                && stderr.contains("/token does not exist")
                && stderr.contains("nothing was sent"),
            "{stderr}"
        );
        assert!(!stderr.contains("authentication_required"), "{stderr}");
    }

    let with_token = isolated(env!("CARGO_BIN_EXE_memprove"))
        .arg("status")
        .env("MEMPROVE_DATA_DIR", &data_dir)
        .env("MEMPROVE_TOKEN", "not-a-real-token")
        .output()
        .unwrap();
    assert_eq!(with_token.status.code(), Some(1));
    assert!(text(&with_token.stdout).contains(r#""code":"authentication_required""#));

    let socket = data_dir.join("memproved.sock");
    let via_socket = isolated(env!("CARGO_BIN_EXE_memprove"))
        .arg("--socket")
        .arg(&socket)
        .args(["health", "live"])
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap();
    assert_eq!(via_socket.status.code(), Some(0));

    assert_eq!(daemon.terminate(), Some(0));
    assert!(!socket.exists());
}

#[test]
fn cli_reports_an_unreachable_daemon() {
    let root = tempfile::tempdir().unwrap();
    let output = cli(root.path(), &["health", "live"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(text(&output.stderr).contains("is the daemon running?"));
}

#[test]
fn cli_rejects_unknown_commands() {
    let root = tempfile::tempdir().unwrap();
    let output = cli(root.path(), &["events"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("unknown command"));
}

/// P4-C: a `--budget` out of 1000..=1000000, a malformed `--claims`, or
/// (P4-C2) `--pack --claims` with a `--budget`, is
/// a usage error (exit 2) found before any request: with no daemon running,
/// a request would exit 3.
#[test]
fn resume_pack_and_claims_usage_errors_send_nothing() {
    let root = tempfile::tempdir().unwrap();
    let task = "018f47a0-0000-7000-8000-000000000001";
    for (words, said) in [
        (
            &["resume", "--task", task, "--pack", "--budget", "999"][..],
            "--budget must be a whole number of UTF-16 code units from 1000 through 1000000",
        ),
        (
            &["resume", "--task", task, "--pack", "--budget", "1000001"][..],
            "from 1000 through 1000000",
        ),
        (
            &["resume", "--task", task, "--budget", "2000"][..],
            "only with --pack",
        ),
        (
            &["resume", "--task", task, "--text", "--claims", "7-3"][..],
            "--claims takes",
        ),
        // P4-C2 revision 5: --pack --claims has no budget.
        (
            &[
                "resume", "--task", task, "--pack", "--claims", "3", "--budget", "2000",
            ][..],
            "used without --budget",
        ),
        (
            &["resume", "--task", task, "--pack", "--claims", "0"][..],
            "--claims takes",
        ),
    ] {
        let output = cli(root.path(), words);
        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{words:?}: {stderr}");
        assert!(stderr.contains(said), "{words:?}: {stderr}");
        // U1 (C5): no run of spaces in the sentence.
        assert!(!stderr.contains("  "), "{words:?}: {stderr:?}");
        assert!(
            stderr.ends_with("run `memprove resume --help`\n"),
            "{words:?}: {stderr}"
        );
        assert!(output.stdout.is_empty());
    }
    // Within the bounds the request is sent: no daemon, exit 3.
    for budget in ["1000", "1000000"] {
        let output = cli_with_token(
            root.path(),
            &["resume", "--task", task, "--pack", "--budget", budget],
        );
        assert_eq!(output.status.code(), Some(3), "{budget}");
    }
    let output = cli_with_token(
        root.path(),
        &["resume", "--task", task, "--pack", "--claims", "3,5-7"],
    );
    assert_eq!(output.status.code(), Some(3));
}

/// U1 (C2, original #19): an argument of the command that holds a control
/// character (Unicode Cc) is refused before the command runs, exit 2. No
/// daemon runs: an argument without one is executed first and its result
/// then fails to reach the daemon (exit 125), so the marker file below tells
/// a command that ran from one that did not.
#[test]
fn run_refuses_a_control_character_in_the_command_before_running_it() {
    let root = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let ids = [
        ("--session", "018f47a0-0000-7000-8000-000000000001"),
        ("--workspace", "018f47a0-0000-7000-8000-000000000002"),
        ("--task", "018f47a0-0000-7000-8000-000000000003"),
    ];
    let run = |command: &[&str]| {
        let mut words = vec!["run"];
        for (name, value) in ids {
            words.push(name);
            words.push(value);
        }
        words.push("--");
        words.extend_from_slice(command);
        cli_with_token(root.path(), &words)
    };
    let marker = work.path().join("ran");
    let touch = format!("touch '{}'", marker.display());
    // The control: with nothing to refuse the command runs.
    let output = run(&["sh", "-c", &touch]);
    assert_eq!(output.status.code(), Some(125), "{}", text(&output.stderr));
    assert!(
        marker.exists(),
        "an ordinary command runs before it is sent"
    );
    fs::remove_file(&marker).unwrap();
    // One Cc character, in each element, of C0, DEL, and C1.
    for control in ['\n', '\t', '\u{1}', '\u{1b}', '\u{7f}', '\u{85}', '\u{9f}'] {
        let in_script = format!("{touch}; : {control}");
        let in_program = format!("sh{control}");
        for command in [
            vec!["sh", "-c", &in_script],
            vec![&in_program, "-c", &touch],
            vec!["sh", "-c", &touch, &in_script],
        ] {
            let output = run(&command);
            let stderr = text(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(2),
                "{control:?} {command:?}: {stderr}"
            );
            assert!(stderr.contains("control character"), "{stderr}");
            assert!(
                !stderr.contains("touch"),
                "the refusal does not repeat the argument: {stderr:?}"
            );
            assert!(output.stdout.is_empty());
            assert!(!marker.exists(), "{control:?} {command:?} ran");
        }
    }
    assert_eq!(fs::read_dir(work.path()).unwrap().count(), 0);
}

/// How a CLI invocation authenticates.
#[derive(Clone, Copy)]
enum Auth<'a> {
    Anonymous,
    Env(&'a str),
    File(&'a Path),
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Collects every CLI stderr so the final leak scan covers all of them.
#[derive(Default)]
struct Transcript {
    stderr: Vec<String>,
}

impl Transcript {
    fn cli(&mut self, data_dir: &Path, auth: Auth<'_>, words: &[&str]) -> Run {
        let mut command = isolated(env!("CARGO_BIN_EXE_memprove"));
        command
            .arg("--data-dir")
            .arg(data_dir)
            .env_remove("MEMPROVE_DATA_DIR")
            .env_remove("MEMPROVE_TOKEN")
            .env_remove("MEMPROVE_TOKEN_FILE");
        match auth {
            Auth::Anonymous => {}
            Auth::Env(token) => {
                command.env("MEMPROVE_TOKEN", token);
            }
            Auth::File(path) => {
                command.arg("--token-file").arg(path);
            }
        }
        let output = command.args(words).output().unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        self.stderr.push(run.stderr.clone());
        run
    }

    fn ok(&mut self, data_dir: &Path, auth: Auth<'_>, words: &[&str]) -> serde_like::Object {
        let run = self.cli(data_dir, auth, words);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        serde_like::Object(run.stdout)
    }

    fn problem(
        &mut self,
        data_dir: &Path,
        auth: Auth<'_>,
        words: &[&str],
        status: u16,
        code: &str,
    ) {
        let run = self.cli(data_dir, auth, words);
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

/// Minimal field access over the CLI's validated JSON output.
mod serde_like {
    use memprove_protocol::strict_json::{JsonValue, parse_strict};

    pub struct Object(pub String);

    impl Object {
        pub fn value(&self) -> JsonValue {
            parse_strict(self.0.trim()).unwrap()
        }

        pub fn str(&self, name: &str) -> String {
            self.value()
                .get(name)
                .and_then(JsonValue::as_str)
                .unwrap_or_else(|| panic!("{name} missing in {}", self.0))
                .to_owned()
        }
    }
}

fn bootstrap(data_dir: &Path) -> Output {
    isolated(daemon_binary())
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(data_dir)
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap()
}

fn write_owner_only(path: &Path, contents: &str) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    writeln!(file, "{contents}").unwrap();
}

fn files_under(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.is_dir() {
            files.extend(files_under(&path));
        } else if metadata.is_file() {
            files.push(path);
        }
    }
    files
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario: bootstrap, scopes, revoke, restart, leak scan.
fn token_lifecycle_through_real_binaries_survives_restart_without_leaking_secrets() {
    let root = tempfile::tempdir().unwrap();
    let data_dir = root.path().join("data");
    let logs = root.path().join("logs");
    fs::create_dir(&logs).unwrap();
    let mut transcript = Transcript::default();

    // Offline bootstrap prints the secret exactly once, on stdout.
    let issued = bootstrap(&data_dir);
    let bootstrap_stderr = text(&issued.stderr);
    assert_eq!(issued.status.code(), Some(0), "{bootstrap_stderr}");
    let stdout = text(&issued.stdout);
    let control = stdout.trim_end().to_owned();
    assert_eq!(stdout, format!("{control}\n"));
    assert_eq!(control.len(), 43);
    assert!(bootstrap_stderr.contains("issued control token"));
    let control_file = root.path().join("control.token");
    write_owner_only(&control_file, &control);

    let again = bootstrap(&data_dir);
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).contains("an active control token already exists"));
    assert!(again.stdout.is_empty());

    let daemon = Daemon::start(&data_dir, &logs.join("first.log"));
    let while_running = bootstrap(&data_dir);
    assert_eq!(while_running.status.code(), Some(1));
    assert!(text(&while_running.stderr).contains("already running"));

    // The bootstrap token reaches status (control) and version (any token).
    let status = transcript.ok(&data_dir, Auth::File(&control_file), &["status"]);
    assert_eq!(
        status
            .value()
            .get("project_count")
            .and_then(memprove_protocol::strict_json::JsonValue::as_i64),
        Some(0)
    );
    transcript.ok(&data_dir, Auth::Env(&control), &["version"]);

    // A restricted token is forbidden from every token operation.
    let created = transcript.ok(
        &data_dir,
        Auth::File(&control_file),
        &[
            "token",
            "create",
            "--label",
            "restricted",
            "--scope",
            "mcp-read",
        ],
    );
    let restricted_id = created.str("token_id");
    let restricted = created.str("secret");
    let survivor = transcript
        .ok(
            &data_dir,
            Auth::File(&control_file),
            &[
                "token",
                "create",
                "--label",
                "survivor",
                "--scope",
                "ingest-only",
            ],
        )
        .str("secret");
    let restricted_auth = Auth::Env(&restricted);
    transcript.ok(&data_dir, restricted_auth, &["version"]);
    for words in [
        &["token", "list"][..],
        &["token", "create", "--label", "x", "--scope", "control"][..],
        &["token", "revoke", restricted_id.as_str()][..],
        &["status"][..],
    ] {
        transcript.problem(&data_dir, restricted_auth, words, 403, "policy_denied");
    }

    // The list never carries secrets; the last control token is protected.
    let listed = transcript.ok(&data_dir, Auth::File(&control_file), &["token", "list"]);
    for secret in [&control, &restricted, &survivor] {
        assert!(!listed.0.contains(secret.as_str()));
    }
    let control_id = listed
        .value()
        .get("items")
        .and_then(|items| items.as_array().map(<[_]>::to_vec))
        .unwrap()
        .into_iter()
        .find(|item| item.get("client_label").and_then(|v| v.as_str()) == Some("bootstrap-control"))
        .and_then(|item| {
            item.get("token_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .unwrap();
    transcript.problem(
        &data_dir,
        Auth::File(&control_file),
        &["token", "revoke", &control_id],
        409,
        "last_control_token",
    );

    // Revocation takes effect immediately.
    let revoked = transcript.ok(
        &data_dir,
        Auth::File(&control_file),
        &["token", "revoke", &restricted_id],
    );
    assert_eq!(revoked.str("status"), "revoked");
    transcript.problem(
        &data_dir,
        restricted_auth,
        &["version"],
        401,
        "authentication_required",
    );
    // U6 K1: no token at all is refused by the CLI before any request.
    let anonymous = transcript.cli(&data_dir, Auth::Anonymous, &["token", "list"]);
    assert_eq!(anonymous.code, Some(1), "{}", anonymous.stderr);
    assert!(anonymous.stdout.is_empty(), "{}", anonymous.stdout);
    assert!(
        anonymous
            .stderr
            .starts_with("memprove: error: no token: --token-file was not given;"),
        "{}",
        anonymous.stderr
    );

    // Tokens persist across a daemon restart; revocation does too.
    assert_eq!(daemon.terminate(), Some(0));
    let daemon = Daemon::start(&data_dir, &logs.join("second.log"));
    transcript.ok(&data_dir, Auth::File(&control_file), &["status"]);
    transcript.ok(&data_dir, Auth::Env(&survivor), &["version"]);
    transcript.problem(
        &data_dir,
        restricted_auth,
        &["version"],
        401,
        "authentication_required",
    );

    // A token file readable by others is refused before any request.
    let loose = root.path().join("loose.token");
    write_owner_only(&loose, &control);
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
    let refused = transcript.cli(&data_dir, Auth::File(&loose), &["status"]);
    assert_eq!(refused.code, Some(2));
    assert!(refused.stderr.contains("chmod 600"), "{}", refused.stderr);
    assert!(refused.stdout.is_empty());
    assert_eq!(daemon.terminate(), Some(0));

    // No plaintext secret in the data directory or in any stderr stream.
    let secrets = [&control, &restricted, &survivor];
    let data_files = files_under(&data_dir);
    let registry = data_files
        .iter()
        .find(|path| path.ends_with("registry.sqlite"))
        .expect("registry.sqlite exists");
    // Positive control: the scan reads plaintext rows (label and id are there).
    let registry_bytes = fs::read(registry).unwrap();
    assert!(contains(&registry_bytes, "restricted"));
    assert!(contains(&registry_bytes, &restricted_id));
    for path in &data_files {
        let bytes = fs::read(path).unwrap();
        for secret in secrets {
            assert!(
                !contains(&bytes, secret),
                "secret found in {}",
                path.display()
            );
        }
    }
    let mut streams = vec![
        bootstrap_stderr,
        text(&again.stderr),
        text(&while_running.stderr),
    ];
    streams.extend(
        fs::read_dir(&logs)
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap()),
    );
    streams.extend(transcript.stderr);
    for stream in &streams {
        for secret in secrets {
            assert!(
                !stream.contains(secret.as_str()),
                "secret found in stderr:\n{stream}"
            );
        }
    }
}

/// The context of `label_like_evidence_exits_two_without_a_request`: one
/// run `r1` of the latest Snapshot.
fn label_context(workspace: &std::path::Path) -> memprove_cli::context::Context {
    use memprove_cli::context::{Context, RunLabel};
    Context {
        workspace_root: workspace.to_owned(),
        project_id: "01a100d7-642a-7af2-9fd8-093f94056362".to_owned(),
        workspace_id: "01a100d7-642a-7af2-9fd8-09465e831321".to_owned(),
        agent_id: "01a100d8-7734-7aca-9e43-6fae2f229fc3".to_owned(),
        task_id: Some("01a10939-c335-7e13-8da4-b45416ac31d4".to_owned()),
        task_title: Some("label-like evidence".to_owned()),
        task_revision: Some(3),
        task_state: Some("in_progress".to_owned()),
        session_id: Some("01a10939-c336-7e13-8da4-b45416ac31d5".to_owned()),
        session_revision: Some(1),
        snapshot_id: Some("01a10942-00e3-76d0-a3f0-9d19c0bde428".to_owned()),
        snapshot_manifest: None,
        evidence_id: Some("01a10942-00e6-7b91-a31d-6a93cdaf0916".to_owned()),
        evidence_status: Some("PASS".to_owned()),
        runs: vec![RunLabel {
            label: "r1".to_owned(),
            evidence_id: "01a10942-00e6-7b91-a31d-6a93cdaf0916".to_owned(),
            status: Some("PASS".to_owned()),
            argv_sha256: None,
            kind: None,
            argv: None,
        }],
        next_run: 2,
        agent_name: None,
        agent_role: None,
        parent_session_id: None,
    }
}

/// An `--evidence` value that only looks like a run label (`r0`, `r01`,
/// `R1`) is refused by the real binary as a usage error, exit 2, before any
/// connection: the socket is a listener that nobody accepts on, so a request
/// would leave a connection in its backlog. A valid label (`r1`) does
/// connect, which shows the listener would have seen one.
#[test]
fn label_like_evidence_exits_two_without_a_request() {
    use memprove_cli::context::ContextStore;
    use std::{io::ErrorKind, os::unix::net::UnixListener};

    let root = tempfile::tempdir().unwrap();
    let workspace = fs::canonicalize(root.path()).unwrap().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let config_dir = root.path().join("config");
    let store = ContextStore::from_env(Some(config_dir.clone().into()), None, None).unwrap();
    store.save(&label_context(&workspace)).unwrap();
    let socket = root.path().join("memproved.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let command = |words: &[&str]| {
        let mut command = isolated(env!("CARGO_BIN_EXE_memprove"));
        command
            .current_dir(&workspace)
            .arg("--socket")
            .arg(&socket)
            .args(words)
            .env("MEMPROVE_TOKEN", "not-a-real-token")
            .env("MEMPROVE_CONFIG_DIR", &config_dir);
        command
    };

    for spelled in ["r0", "r01", "R1"] {
        for words in [
            &[
                "checkpoint",
                "create",
                "--fact",
                "x",
                "--evidence",
                spelled,
                "--next",
                "n",
            ][..],
            &["task", "complete", "--evidence", spelled][..],
            &["decide", "d", "--evidence", spelled][..],
        ] {
            let output = command(words).output().unwrap();
            // P3-L L9: the error, then one line naming the `--help` to read.
            println!(
                "$ memprove --socket <SOCKET> {}\n{}\n[exit {:?}]",
                words.join(" "),
                text(&output.stderr),
                output.status.code()
            );
            assert_eq!(output.status.code(), Some(2), "{words:?}");
            assert!(output.stdout.is_empty(), "{words:?}");
            assert!(
                // U3 (K4): named by its place, not repeated.
                text(&output.stderr).contains("--evidence #1 is not a run label")
                    && !text(&output.stderr).contains(spelled),
                "{words:?}: {}",
                text(&output.stderr)
            );
            let stderr = text(&output.stderr);
            assert!(
                stderr
                    .lines()
                    .last()
                    .is_some_and(|line| line.contains(" --help`")),
                "{words:?}: {stderr}"
            );
            assert!(!stderr.contains("Commands:"), "{words:?}: {stderr}");
            match listener.accept() {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                other => panic!("{words:?} reached the socket: {other:?}"),
            }
        }
    }

    // The control: a valid label is resolved and sent.
    let mut child = command(&["task", "complete", "--evidence", "r1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    listener.set_nonblocking(false).unwrap();
    let (stream, _) = listener.accept().unwrap();
    drop(stream);
    let status = child.wait().unwrap();
    assert_ne!(status.code(), Some(2));
}

/// P3-L L11 patch: `task complete` without `--evidence` is not sent while
/// the latest run of some command in the context's state did not pass. U4
/// (Y1): the only thing it sends is the read of the Session. With the
/// latest run of every command passing, or with an explicit `--evidence`,
/// the same command does connect.
#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario: a seeded context, a refusal, two controls.
fn l11_failing_run_refuses_default_completion_without_a_request() {
    use memprove_cli::context::{Context, ContextStore, RunLabel};
    use std::{io::ErrorKind, os::unix::net::UnixListener};

    let root = tempfile::tempdir().unwrap();
    let workspace = fs::canonicalize(root.path()).unwrap().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let config_dir = root.path().join("config");
    let store = ContextStore::from_env(Some(config_dir.clone().into()), None, None).unwrap();
    let evidence = |n: u8| format!("01a10942-00e6-7b91-a31d-6a93cdaf09{n:02}");
    let run = |label: &str, n: u8, status: &str, argv: &str| RunLabel {
        label: label.to_owned(),
        evidence_id: evidence(n),
        status: Some(status.to_owned()),
        argv_sha256: Some(argv.to_owned()),
        kind: None,
        argv: None,
    };
    let save = |runs: Vec<RunLabel>| {
        store
            .save(&Context {
                workspace_root: workspace.clone(),
                project_id: "01a100d7-642a-7af2-9fd8-093f94056362".to_owned(),
                workspace_id: "01a100d7-642a-7af2-9fd8-09465e831321".to_owned(),
                agent_id: "01a100d8-7734-7aca-9e43-6fae2f229fc3".to_owned(),
                task_id: Some("01a10939-c335-7e13-8da4-b45416ac31d4".to_owned()),
                task_title: Some("a failing run".to_owned()),
                task_revision: Some(3),
                task_state: Some("in_progress".to_owned()),
                session_id: Some("01a10939-c336-7e13-8da4-b45416ac31d5".to_owned()),
                session_revision: Some(1),
                snapshot_id: Some("01a10942-00e3-76d0-a3f0-9d19c0bde428".to_owned()),
                snapshot_manifest: None,
                evidence_id: Some(evidence(1)),
                evidence_status: Some("PASS".to_owned()),
                next_run: u64::try_from(runs.len()).unwrap() + 1,
                agent_name: None,
                agent_role: None,
                parent_session_id: None,
                runs,
            })
            .unwrap();
    };
    let socket = root.path().join("memproved.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let command = |words: &[&str]| {
        let mut command = isolated(env!("CARGO_BIN_EXE_memprove"));
        command
            .current_dir(&workspace)
            .arg("--socket")
            .arg(&socket)
            .args(words)
            .env("MEMPROVE_TOKEN", "not-a-real-token")
            .env("MEMPROVE_CONFIG_DIR", &config_dir);
        command
    };
    // Whether the command connects to the socket; the connection is dropped
    // at once, so the command ends with a transport error.
    let connects = |words: &[&str]| {
        let mut child = command(words)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        let connected = loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    drop(stream);
                    break true;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if child.try_wait().unwrap().is_some() {
                        break listener.accept().is_ok();
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        break false;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        let _ = child.wait();
        connected
    };

    // r1 passed (a), r2 failed (b): U4 (Y1, N-3) the Session is read first,
    // so the command connects once (here the read is dropped: exit 3, "was
    // not sent"); the refusal of the failing run needs an answer to that
    // read and is covered with a real daemon (p4g_complete_cites_as_task_
    // complete_does). Nothing else reaches the socket.
    save(vec![run("r1", 1, "PASS", "a"), run("r2", 2, "FAIL", "b")]);
    let child = command(&["task", "complete"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    listener.set_nonblocking(false).unwrap();
    let (stream, _) = listener.accept().unwrap();
    drop(stream);
    listener.set_nonblocking(true).unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = text(&output.stderr);
    println!(
        "$ memprove --socket <SOCKET> task complete\n{stderr}\n[exit {:?}]",
        output.status.code()
    );
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("task complete was not sent"), "{stderr}");
    match listener.accept() {
        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
        other => panic!("the refused completion sent a second request: {other:?}"),
    }

    // Controls: an explicit --evidence, and a later pass of r2's command.
    assert!(
        connects(&["task", "complete", "--evidence", "r1"]),
        "an explicit --evidence is sent"
    );
    save(vec![
        run("r1", 1, "PASS", "a"),
        run("r2", 2, "FAIL", "b"),
        run("r3", 3, "PASS", "b"),
    ]);
    assert!(
        connects(&["task", "complete"]),
        "the latest run of every command passed: the default is sent"
    );
}

/// A command for a binary under test with no inherited `MEMPROVE_*`
/// variable, so the test does not depend on the caller's environment
/// (P2-H); the test sets the ones it needs afterwards. U5 Z-F round 1
/// (S2): the CLI now falls back to `<configuration directory>/data-dir` and
/// `<configuration directory>/token`, so no `XDG_*` variable is inherited
/// and HOME is a path under `/dev/null`, which can be neither read nor
/// made: the default configuration directory of the developer's HOME is
/// never read, and nothing is written for HOME.
fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("MEMPROVE_") || text.starts_with("XDG_") {
            command.env_remove(name);
        }
    }
    command.env("HOME", "/dev/null/no-home");
    command
}

/// U3 (K1, Owner O3): `task complete` reads the context's Session before it
/// sends `transitionTask`. When that read fails as an exchange (here the
/// connection is closed at once, as a daemon that stops would), the
/// command reports it with the transport exit code, 3, and sends nothing
/// else: no second connection reaches the socket.
#[test]
fn u3_k1_complete_sends_nothing_when_its_session_read_fails() {
    use memprove_cli::context::{Context, ContextStore, RunLabel};
    use std::{io::ErrorKind, os::unix::net::UnixListener};

    let root = tempfile::tempdir().unwrap();
    let workspace = fs::canonicalize(root.path()).unwrap().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let config_dir = root.path().join("config");
    let store = ContextStore::from_env(Some(config_dir.clone().into()), None, None).unwrap();
    let evidence = "01a10942-00e6-7b91-a31d-6a93cdaf0901".to_owned();
    store
        .save(&Context {
            workspace_root: workspace.clone(),
            project_id: "01a100d7-642a-7af2-9fd8-093f94056362".to_owned(),
            workspace_id: "01a100d7-642a-7af2-9fd8-09465e831321".to_owned(),
            agent_id: "01a100d8-7734-7aca-9e43-6fae2f229fc3".to_owned(),
            task_id: Some("01a10939-c335-7e13-8da4-b45416ac31d4".to_owned()),
            task_title: Some("a Session read that fails".to_owned()),
            task_revision: Some(3),
            task_state: Some("in_progress".to_owned()),
            session_id: Some("01a10939-c336-7e13-8da4-b45416ac31d5".to_owned()),
            session_revision: Some(1),
            snapshot_id: Some("01a10942-00e3-76d0-a3f0-9d19c0bde428".to_owned()),
            snapshot_manifest: None,
            evidence_id: Some(evidence.clone()),
            evidence_status: Some("PASS".to_owned()),
            next_run: 2,
            agent_name: None,
            agent_role: None,
            parent_session_id: None,
            runs: vec![RunLabel {
                label: "r1".to_owned(),
                evidence_id: evidence,
                status: Some("PASS".to_owned()),
                argv_sha256: Some("a".to_owned()),
                kind: None,
                argv: None,
            }],
        })
        .unwrap();
    let socket = root.path().join("memproved.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    for words in [
        &["task", "complete"][..],
        &["task", "complete", "--evidence", "r1"][..],
    ] {
        let mut child = isolated(env!("CARGO_BIN_EXE_memprove"))
            .current_dir(&workspace)
            .arg("--socket")
            .arg(&socket)
            .args(words)
            .env("MEMPROVE_TOKEN", "not-a-real-token")
            .env("MEMPROVE_CONFIG_DIR", &config_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // The Session read: accepted, then closed without a response.
        listener.set_nonblocking(false).unwrap();
        let (stream, _) = listener.accept().unwrap();
        drop(stream);
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "{words:?} did not end");
            thread::sleep(Duration::from_millis(10));
        };
        let output = child.wait_with_output().unwrap();
        let stderr = text(&output.stderr);
        println!(
            "$ memprove --socket <SOCKET> {}\n{stderr}[exit {:?}]",
            words.join(" "),
            status.code()
        );
        assert_eq!(status.code(), Some(3), "{words:?}: {stderr}");
        assert!(
            stderr.contains("task complete was not sent"),
            "{words:?}: {stderr}"
        );
        listener.set_nonblocking(true).unwrap();
        match listener.accept() {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            other => panic!("{words:?} sent a second request: {other:?}"),
        }
    }
}
