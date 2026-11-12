//! U1 (E1, Owner O8): `memprove --version` and `memproved --version`
//! print `<name> <workspace version> (<12-character commit>[-dirty])`, and
//! the version response of the real daemon carries the same identity in its
//! optional `build` member. The real binaries are run with no
//! `MEMPROVE_*` variable; the daemon's data directory is a short
//! temporary directory under `/tmp` (the socket path has a length limit).

use std::{
    fs::File,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};

const TIMEOUT: Duration = Duration::from_secs(15);

fn cli_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_memprove"))
}

fn daemon_binary() -> PathBuf {
    let daemon = cli_binary().with_file_name("memproved");
    assert!(
        daemon.exists(),
        "memproved not found next to the CLI; run `cargo test --workspace`"
    );
    daemon
}

fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    command.env_remove("CLAUDE_CODE_SESSION_ID");
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The first 12 characters of the commit the workspace is checked out at.
fn head_prefix() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace_root())
        .output()
        .unwrap();
    assert!(output.status.success());
    text(&output.stdout).trim()[..12].to_owned()
}

/// The build identity in a `--version` line of `name`, after checking the
/// line's shape: `<name> <version> (<12 hex>[-dirty])` and a line break.
fn identity_of(name: &str, output: &Output) -> String {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr));
    let line = text(&output.stdout);
    let prefix = format!("{name} {} (", env!("CARGO_PKG_VERSION"));
    let identity = line
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(")\n"))
        .unwrap_or_else(|| panic!("unexpected --version line {line:?}"));
    let commit = identity.strip_suffix("-dirty").unwrap_or(identity);
    assert_eq!(commit.len(), 12, "{line:?}");
    assert!(
        commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{line:?}"
    );
    identity.to_owned()
}

#[test]
fn both_binaries_print_the_commit_they_were_built_from() {
    let head = head_prefix();
    let mut identities = Vec::new();
    for flag in ["--version", "-V"] {
        let cli = isolated(cli_binary()).arg(flag).output().unwrap();
        let daemon = isolated(daemon_binary()).arg(flag).output().unwrap();
        let cli = identity_of("memprove", &cli);
        let daemon = identity_of("memproved", &daemon);
        assert!(cli.starts_with(&head), "{cli} is not HEAD {head}");
        assert_eq!(cli, daemon, "one build, one identity");
        identities.push(cli);
    }
    assert_eq!(identities[0], identities[1]);
}

#[test]
fn version_takes_no_other_argument() {
    let output = isolated(cli_binary())
        .args(["--version", "status"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout));
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_version_response_carries_the_daemon_build() {
    let root = tempfile::Builder::new()
        .prefix("u1e")
        .tempdir_in("/tmp")
        .unwrap();
    let data_dir = root.path().join("d");
    let token = root.path().join("t");
    let issued = isolated(daemon_binary())
        .args(["bootstrap", "--data-dir"])
        .arg(&data_dir)
        .arg("--output")
        .arg(&token)
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    let _daemon = Daemon(
        isolated(daemon_binary())
            .arg("--data-dir")
            .arg(&data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(root.path().join("log")).unwrap())
            .spawn()
            .unwrap(),
    );
    let cli = |words: &[&str]| {
        isolated(cli_binary())
            .arg("--data-dir")
            .arg(&data_dir)
            .arg("--token-file")
            .arg(&token)
            .args(words)
            .output()
            .unwrap()
    };
    let deadline = Instant::now() + TIMEOUT;
    while !cli(&["health", "ready"]).status.success() {
        assert!(Instant::now() < deadline, "memproved never became ready");
        thread::sleep(Duration::from_millis(20));
    }
    let version = cli(&["version"]);
    assert_eq!(version.status.code(), Some(0), "{}", text(&version.stderr));
    let body = parse_strict(text(&version.stdout).trim()).unwrap();
    let own = identity_of(
        "memprove",
        &isolated(cli_binary()).arg("--version").output().unwrap(),
    );
    assert_eq!(body.get("build").and_then(JsonValue::as_str), Some(&*own));
    assert_eq!(
        body.get("application_version").and_then(JsonValue::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );
}
