//! Process-level lifecycle tests against the real `memproved` binary.

use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixStream,
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_protocol::{
    phase1::{OperationId, validate_problem, validate_response},
    strict_json::parse_strict,
};

const TIMEOUT: Duration = Duration::from_secs(15);

struct Daemon {
    child: Child,
    log: PathBuf,
}

impl Daemon {
    fn spawn(data_dir: &Path, log: &Path) -> Self {
        let child = isolated(env!("CARGO_BIN_EXE_memproved"))
            .arg("--data-dir")
            .arg(data_dir)
            .env_remove("MEMPROVE_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(log).unwrap())
            .spawn()
            .unwrap();
        Self {
            child,
            log: log.to_path_buf(),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn stderr(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.pid().to_string())
            .status()
            .unwrap();
        assert!(status.success(), "kill -{name} failed");
    }

    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "memproved did not exit; stderr:\n{}",
                self.stderr()
            );
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

struct Http {
    status: u16,
    media_type: String,
    headers: String,
    body: String,
}

fn request(socket: &Path, method: &str, path: &str) -> std::io::Result<Http> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: continuity.local\r\nConnection: close\r\n\r\n"
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let (head, body) = raw.split_once("\r\n\r\n").expect("header terminator");
    let status = head[9..12].parse().unwrap();
    let media_type = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-type:")
                .map(|value| value.trim().to_owned())
        })
        .unwrap_or_default();
    Ok(Http {
        status,
        media_type,
        headers: head.to_ascii_lowercase(),
        body: body.to_owned(),
    })
}

fn wait_ready(daemon: &Daemon, socket: &Path) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Ok(response) = request(socket, "GET", "/health/ready")
            && response.status == 200
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "memproved never became ready; stderr:\n{}",
            daemon.stderr()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    data_dir: PathBuf,
    socket: PathBuf,
    logs: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let data_dir = root.path().join("data");
    let logs = root.path().join("logs");
    fs::create_dir(&logs).unwrap();
    Fixture {
        socket: data_dir.join("memproved.sock"),
        data_dir,
        logs,
        _root: root,
    }
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn serves_over_owner_only_socket_and_cleans_up_on_sigterm() {
    let fx = fixture();
    let mut daemon = Daemon::spawn(&fx.data_dir, &fx.logs.join("a.log"));
    wait_ready(&daemon, &fx.socket);

    let metadata = fs::symlink_metadata(&fx.socket).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(mode(&fx.socket), 0o600);
    assert_eq!(mode(&fx.data_dir), 0o700);

    for (path, operation) in [
        ("/health/live", OperationId::HealthLive),
        ("/health/ready", OperationId::HealthReady),
    ] {
        let response = request(&fx.socket, "GET", path).unwrap();
        assert_eq!(response.status, 200);
        let body = parse_strict(&response.body).unwrap();
        validate_response(operation, response.status, &response.media_type, &body).unwrap();
    }
    for (path, operation) in [
        ("/v1/status", OperationId::GetStatus),
        ("/v1/version", OperationId::GetVersion),
    ] {
        let response = request(&fx.socket, "GET", path).unwrap();
        assert_eq!(response.status, 401);
        assert!(response.headers.contains("www-authenticate: bearer"));
        let body = parse_strict(&response.body).unwrap();
        validate_response(operation, response.status, &response.media_type, &body).unwrap();
    }
    for (method, path) in [
        ("GET", "/v1/projects/register"),
        ("GET", "/nope"),
        ("POST", "/health/live"),
        ("DELETE", "/v1/status"),
    ] {
        let response = request(&fx.socket, method, path).unwrap();
        assert_eq!(response.status, 404, "{method} {path}");
        assert_eq!(response.media_type, "application/problem+json");
        let body = parse_strict(&response.body).unwrap();
        validate_problem(404, &body).unwrap();
        assert!(response.body.contains(r#""code":"resource_not_found""#));
    }

    daemon.signal("TERM");
    let status = daemon.wait();
    assert_eq!(status.code(), Some(0), "stderr:\n{}", daemon.stderr());
    assert!(!fx.socket.exists(), "socket must be removed on SIGTERM");
    assert!(daemon.stderr().contains("received SIGTERM"));
}

#[test]
fn sigint_also_shuts_down_gracefully() {
    let fx = fixture();
    let mut daemon = Daemon::spawn(&fx.data_dir, &fx.logs.join("a.log"));
    wait_ready(&daemon, &fx.socket);
    daemon.signal("INT");
    assert_eq!(
        daemon.wait().code(),
        Some(0),
        "stderr:\n{}",
        daemon.stderr()
    );
    assert!(!fx.socket.exists());
}

#[test]
fn second_instance_is_rejected_without_disturbing_the_first() {
    let fx = fixture();
    let mut first = Daemon::spawn(&fx.data_dir, &fx.logs.join("first.log"));
    wait_ready(&first, &fx.socket);

    let mut second = Daemon::spawn(&fx.data_dir, &fx.logs.join("second.log"));
    let status = second.wait();
    assert_eq!(status.code(), Some(1));
    let message = second.stderr();
    assert!(
        message.contains("another memproved instance is already running"),
        "{message}"
    );
    assert!(
        message.contains(&format!("lock held by pid {}", first.pid())),
        "{message}"
    );

    assert!(
        fs::symlink_metadata(&fx.socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert_eq!(
        request(&fx.socket, "GET", "/health/live").unwrap().status,
        200
    );

    first.signal("TERM");
    assert_eq!(first.wait().code(), Some(0));
}

#[test]
fn restarts_after_sigkill_by_replacing_the_stale_socket() {
    let fx = fixture();
    let mut killed = Daemon::spawn(&fx.data_dir, &fx.logs.join("killed.log"));
    wait_ready(&killed, &fx.socket);
    killed.child.kill().unwrap();
    let status = killed.wait();
    assert_eq!(status.signal(), Some(9));

    assert!(
        fs::symlink_metadata(&fx.socket)
            .unwrap()
            .file_type()
            .is_socket(),
        "SIGKILL leaves the socket file behind"
    );
    assert!(UnixStream::connect(&fx.socket).is_err());

    let mut restarted = Daemon::spawn(&fx.data_dir, &fx.logs.join("restarted.log"));
    wait_ready(&restarted, &fx.socket);
    assert!(restarted.stderr().contains("removed stale socket"));
    assert_eq!(mode(&fx.socket), 0o600);
    assert_eq!(
        request(&fx.socket, "GET", "/health/live").unwrap().status,
        200
    );

    restarted.signal("TERM");
    assert_eq!(restarted.wait().code(), Some(0));
    assert!(!fx.socket.exists());
}

#[test]
fn missing_data_dir_is_a_usage_error() {
    let output = isolated(env!("CARGO_BIN_EXE_memproved"))
        .env_remove("MEMPROVE_DATA_DIR")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("no data directory configured"), "{stderr}");
}

#[test]
fn data_dir_from_environment_is_used() {
    let fx = fixture();
    let child = isolated(env!("CARGO_BIN_EXE_memproved"))
        .env("MEMPROVE_DATA_DIR", &fx.data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(fx.logs.join("env.log")).unwrap())
        .spawn()
        .unwrap();
    let mut daemon = Daemon {
        child,
        log: fx.logs.join("env.log"),
    };
    wait_ready(&daemon, &fx.socket);
    daemon.signal("TERM");
    assert_eq!(daemon.wait().code(), Some(0));
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
