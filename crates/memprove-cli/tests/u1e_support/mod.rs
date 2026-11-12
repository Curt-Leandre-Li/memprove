//! Shared harness of the U1 stage U-E process-level tests: the real
//! `memprove` and `memproved` in short temporary directories under
//! `/tmp` (the socket path holds at most 103 bytes), with no `MEMPROVE_*`
//! variable and no `CLAUDE_CODE_SESSION_ID` inherited. Token secrets are
//! never printed.

#![allow(dead_code)] // Each test file uses part of the harness.

use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};

pub const TIMEOUT: Duration = Duration::from_secs(20);

pub fn cli_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_memprove"))
}

pub fn daemon_binary() -> PathBuf {
    let daemon = cli_binary().with_file_name("memproved");
    assert!(
        daemon.exists(),
        "memproved not found next to the CLI; run `cargo test --workspace`"
    );
    daemon
}

pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A command with no `MEMPROVE_*` variable, no `CLAUDE_CODE_SESSION_ID`,
/// and Git's global and system configuration left out.
pub fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("MEMPROVE_") || name_text.starts_with("XDG_") {
            command.env_remove(name);
        }
    }
    command
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A new short temporary directory under `/tmp`, canonical.
pub fn short_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("ue")
        .tempdir_in("/tmp")
        .unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

pub fn json(output: &Output) -> JsonValue {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    parse_strict(text(&output.stdout).trim()).unwrap()
}

pub fn string<'a>(value: &'a JsonValue, name: &str) -> &'a str {
    value
        .get(name)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string member {name}"))
}

pub fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

pub fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = isolated("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=memprove Test"])
        .args(["-c", "user.email=memprove@example.invalid"])
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    text(&output.stdout)
}

/// A Git repository with one committed file and one passing test script.
pub fn repository(path: &Path) {
    fs::create_dir_all(path.join("tests")).unwrap();
    git(path, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(path.join("README"), "fixture\n").unwrap();
    let script = path.join("tests/check.sh");
    fs::write(
        &script,
        "#!/bin/sh\necho 'test check ... ok'\necho 'test result: ok. 1 passed; 0 failed'\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(path, &["add", "."]);
    git(path, &["commit", "--quiet", "-m", "fixture"]);
}

/// A daemon this test started; stopped with SIGTERM, or killed when the
/// test ends first.
pub struct Daemon {
    child: Child,
}

impl Daemon {
    pub fn start(data_dir: &Path, log: &Path) -> Self {
        let daemon = Self {
            child: isolated(daemon_binary())
                .arg("--data-dir")
                .arg(data_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(File::create(log).unwrap())
                .spawn()
                .unwrap(),
        };
        let deadline = Instant::now() + TIMEOUT;
        while !isolated(cli_binary())
            .arg("--data-dir")
            .arg(data_dir)
            .args(["health", "ready"])
            .output()
            .unwrap()
            .status
            .success()
        {
            assert!(
                Instant::now() < deadline,
                "memproved never became ready: {}",
                fs::read_to_string(log).unwrap_or_default()
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
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().unwrap().is_none() {
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

/// Issues the first control token of `data_dir` into `token`.
pub fn bootstrap(data_dir: &Path, token: &Path) {
    let output = isolated(daemon_binary())
        .args(["bootstrap", "--data-dir"])
        .arg(data_dir)
        .arg("--output")
        .arg(token)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
}

/// `memprove --data-dir <data_dir> --token-file <token> <words>` in `cwd`.
pub fn cli_in(cwd: &Path, data_dir: &Path, token: &Path, words: &[&str]) -> Output {
    isolated(cli_binary())
        .current_dir(cwd)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--token-file")
        .arg(token)
        .args(words)
        .output()
        .unwrap()
}

/// The `ledger verify` result of `project`: `valid` and the chain-head
/// digest.
pub fn ledger(data_dir: &Path, token: &Path, project: &str) -> (bool, String) {
    let verified = json(&cli_in(
        Path::new("/"),
        data_dir,
        token,
        &["ledger", "verify", "--project", project],
    ));
    let valid = verified.get("valid") == Some(&JsonValue::Bool(true));
    let head = match verified.get("head_event_sha256") {
        Some(JsonValue::String(head)) => head.clone(),
        other => format!("{other:?}"),
    };
    (valid, head)
}

/// A copy of this CLI whose compiled-in build identity reads differently.
pub fn other_build(root: &Path) -> PathBuf {
    let version = text(
        &isolated(cli_binary())
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    );
    let identity = version
        .trim()
        .rsplit_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap()
        .to_owned();
    let commit = identity.strip_suffix("-dirty").unwrap_or(&identity);
    assert_eq!(commit.len(), 12, "{version}");
    let replacement = if commit == "0123456789ab" {
        "ba9876543210"
    } else {
        "0123456789ab"
    };
    let mut bytes = fs::read(cli_binary()).unwrap();
    let mut replaced = 0;
    let mut index = 0;
    while let Some(found) = bytes[index..]
        .windows(commit.len())
        .position(|window| window == commit.as_bytes())
    {
        let at = index + found;
        bytes[at..at + commit.len()].copy_from_slice(replacement.as_bytes());
        replaced += 1;
        index = at + commit.len();
    }
    assert!(replaced > 0, "the build identity is not in the binary");
    let copy = root.join("memprove-other");
    fs::write(&copy, bytes).unwrap();
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o700)).unwrap();
    if cfg!(target_os = "macos") {
        let signed = Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&copy)
            .output()
            .unwrap();
        assert!(signed.status.success(), "{}", text(&signed.stderr));
    }
    let patched = text(&isolated(&copy).arg("--version").output().unwrap().stdout);
    assert!(patched.contains(replacement), "{patched}");
    copy
}

/// Stops the daemon whose pid the lock file of `data_dir` records, when it
/// is a running `memproved` (one this test started through
/// `ensure-daemon.sh`), and waits until it has exited.
pub fn stop_recorded_daemon(data_dir: &Path) {
    let Ok(pid) = fs::read_to_string(data_dir.join("memproved.lock")) else {
        return;
    };
    let pid = pid.trim().to_owned();
    if pid.is_empty() || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return;
    }
    let Ok(command) = Command::new("ps")
        .args(["-p", &pid, "-o", "comm="])
        .output()
    else {
        return;
    };
    if !text(&command.stdout).contains("memproved") {
        return;
    }
    let _ = Command::new("kill").args(["-TERM", &pid]).status();
    let deadline = Instant::now() + TIMEOUT;
    while Command::new("kill")
        .args(["-0", &pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(Instant::now() < deadline, "memproved {pid} did not exit");
        thread::sleep(Duration::from_millis(50));
    }
}
