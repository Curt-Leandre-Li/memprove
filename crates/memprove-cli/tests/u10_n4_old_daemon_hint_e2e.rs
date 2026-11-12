//! U10 N4 (U9 VN12; Owner O6; ADR 0014 "Amendment (U10, S-K)"): a `memprove`
//! that sends the completion's `session_id` to a `memproved` that does not
//! know the member (one built before U9 SF1-A answers `400 invalid_request`,
//! "the request body has an unknown member `session_id`") says that the daemon
//! is older and which build to upgrade to, in the CLI's `task complete` and
//! `task transition --to completed` and in the MCP `complete`.
//!
//! The older daemon is a proxy: a Unix socket in a directory of its own
//! that forwards every request to the real `memproved` of the fixture, and
//! answers the transition that carries a `session_id` as the older daemon
//! did. Nothing under `/tmp/u8-*` or any other installed daemon is used.

#[path = "u10_mcp_support/mod.rs"]
mod mcp;
#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use mcp::{failed, mcp_call_at, sentence, structured};
use memprove_protocol::strict_json::JsonValue;
use support::{Fixture, cli_binary, isolated, json, text};

/// What the older daemon answers a body that holds `session_id`.
const REFUSAL: &str = r#"{"type":"https://continuity.local/problems/invalid-request","title":"Invalid request","status":400,"code":"invalid_request","detail":"the request body has an unknown member session_id","request_id":"018f47a0-0000-7000-8000-0000000000bb","retryable":false}"#;

/// A socket that forwards to `real`, and, when `older`, refuses a
/// `transitionTask` whose body holds `session_id` as an older daemon did.
struct Proxy {
    dir: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Proxy {
    fn start(real: PathBuf, dir: PathBuf, older: bool) -> Self {
        fs::create_dir(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("memproved.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (flag, log) = (Arc::clone(&stop), Arc::clone(&seen));
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((client, _)) => {
                        client.set_nonblocking(false).unwrap();
                        serve(client, &real, older, &log);
                    }
                    Err(_) => thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Self {
            dir,
            stop,
            thread: Some(thread),
            seen,
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("memproved.sock")
    }

    /// Stops the proxy; the request lines it saw.
    fn finish(mut self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
        self.seen.lock().unwrap().clone()
    }
}

fn serve(mut client: UnixStream, real: &Path, older: bool, seen: &Mutex<Vec<String>>) {
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 4096];
    let head_end = loop {
        if let Some(at) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
            break at;
        }
        let got = client.read(&mut buffer).unwrap();
        assert!(got > 0, "the client closed inside the head");
        raw.extend_from_slice(&buffer[..got]);
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while raw.len() < head_end + 4 + length {
        let got = client.read(&mut buffer).unwrap();
        assert!(got > 0, "the client closed inside the body");
        raw.extend_from_slice(&buffer[..got]);
    }
    let request_line = head.lines().next().unwrap().to_owned();
    seen.lock().unwrap().push(request_line.clone());
    let body = &raw[head_end + 4..];
    let carries_session = body.windows(12).any(|window| window == b"\"session_id\"");
    if older
        && request_line.starts_with("POST /v1/tasks/")
        && request_line.contains("/transition")
        && carries_session
    {
        let reply = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/problem+json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{REFUSAL}",
            REFUSAL.len()
        );
        client.write_all(reply.as_bytes()).unwrap();
        return;
    }
    let mut upstream = UnixStream::connect(real).unwrap();
    upstream.write_all(&raw).unwrap();
    let mut reply = Vec::new();
    upstream.read_to_end(&mut reply).unwrap();
    client.write_all(&reply).unwrap();
}

/// `memprove <words>` in the repository, through `socket`.
fn cli(fixture: &Fixture, socket: &Path, words: &[&str]) -> std::process::Output {
    isolated(cli_binary())
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .arg("--socket")
        .arg(socket)
        .arg("--token-file")
        .arg(&fixture.agent)
        .args(words)
        .output()
        .unwrap()
}

fn task_state(fixture: &Fixture) -> String {
    json(&fixture.agent_ok(&["resume"]))
        .get("task")
        .and_then(|task| task.get("status"))
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned()
}

fn hint() -> String {
    format!(
        "the daemon is older than this build (it does not know session_id); memproved {} knows it",
        env!("MEMPROVE_BUILD_ID")
    )
}

#[test]
fn u10_n4_the_cli_says_an_older_daemon_must_be_upgraded() {
    let fixture = Fixture::new();
    fixture.task_with_fact("N4 cli", "the check passes");
    let older = Proxy::start(
        fixture.data.join("memproved.sock"),
        fixture.root.join("p1"),
        true,
    );

    // `task complete`, then `task transition --to completed`: refused by
    // the older daemon; the failure line and the hint, exit 1, and the Task
    // is still in progress.
    for words in [
        &["task", "complete"][..],
        &["task", "transition", "--to", "completed"][..],
    ] {
        let output = cli(&fixture, &older.socket(), words);
        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{words:?}: {stderr}");
        assert!(
            stderr.contains(
                "transitionTask failed: 400 invalid_request: the request body has an unknown \
                 member session_id"
            ),
            "{words:?}: {stderr}"
        );
        assert!(stderr.contains(&hint()), "{words:?}: {stderr}");
        assert_eq!(task_state(&fixture), "in_progress");
    }
    let seen = older.finish();
    assert_eq!(
        seen.iter()
            .filter(|line| line.contains("/transition"))
            .count(),
        2,
        "{seen:?}"
    );

    // A daemon that knows `session_id` (the proxy passes everything on):
    // the same command completes the Task, with no hint.
    let current = Proxy::start(
        fixture.data.join("memproved.sock"),
        fixture.root.join("p2"),
        false,
    );
    let output = cli(&fixture, &current.socket(), &["task", "complete"]);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("the daemon is older"), "{stderr}");
    current.finish();
    assert_eq!(task_state(&fixture), "completed");
    fixture.stop();
}

#[test]
fn u10_n4_the_mcp_complete_says_an_older_daemon_must_be_upgraded() {
    let fixture = Fixture::new();
    fixture.task_with_fact("N4 mcp", "the check passes");
    let older = Proxy::start(
        fixture.data.join("memproved.sock"),
        fixture.root.join("p1"),
        true,
    );
    let result = mcp_call_at(&fixture, &older.dir, &[], "complete", "{}");
    older.finish();
    let (failure, _) = failed(&result);
    assert_eq!(failure, Some("problem"), "{result:?}");
    assert_eq!(structured(&result, "code"), Some("invalid_request"));
    let sentence = sentence(&result);
    assert!(
        sentence.contains("the request body has an unknown member session_id"),
        "{sentence}"
    );
    assert!(sentence.ends_with(&hint()), "{sentence}");
    assert_eq!(task_state(&fixture), "in_progress");

    // The same server against a daemon that knows the member: completed.
    let current = Proxy::start(
        fixture.data.join("memproved.sock"),
        fixture.root.join("p2"),
        false,
    );
    let result = mcp_call_at(&fixture, &current.dir, &[], "complete", "{}");
    current.finish();
    assert_eq!(
        result.get("isError").and_then(JsonValue::as_bool),
        Some(false),
        "{result:?}"
    );
    assert_eq!(task_state(&fixture), "completed");
    fixture.stop();
}
