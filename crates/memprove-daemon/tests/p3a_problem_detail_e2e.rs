//! P3-A: every `400` Problem of the real `memproved` names the field or
//! parameter and the rule it breaks, and never repeats a token or text from
//! the request. One representative per class of validation failure, sent as
//! raw HTTP over the daemon's Unix socket; the `401` and `403` Problems carry
//! no `detail`; the real `memprove` CLI shows a `400`'s `detail` on
//! standard error.
//!
//! Every request that must not leak carries a probe string (`PROBE-…`) and
//! the control token's plaintext, as a value or as a member or parameter
//! name; the assertions look for both in the whole response, not only in
//! `detail`. `memprove` is taken from the same target directory as
//! `memproved`, so run this through `cargo test --workspace`.

use std::{
    fmt::Write as _,
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use memprove_protocol::{
    phase1::{validate_problem, validate_problem_detail},
    strict_json::{JsonValue, parse_strict},
};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(15);
/// A lowercase `UUIDv7` that names nothing.
const UNKNOWN_ID: &str = "018f47a0-0000-7000-8000-0000000000aa";
const KEY: &str = "p3a-idempotency-key-0001";
const PROBE: &str = "PROBE-4f1c-not-for-the-detail";
/// `Content-Length` limit of the daemon, plus a little.
const OVERSIZED: usize = 2_097_152 + 1024;

struct Http {
    status: u16,
    media_type: String,
    body: String,
}

impl Http {
    fn json(&self) -> JsonValue {
        parse_strict(&self.body).unwrap_or_else(|error| panic!("{error}: {}", self.body))
    }

    fn detail(&self) -> Option<String> {
        self.json()
            .get("detail")
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
    }
}

fn http(
    socket: &Path,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Http {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    let mut head =
        format!("{method} {target} HTTP/1.1\r\nHost: continuity.local\r\nConnection: close\r\n");
    for (name, value) in headers {
        write!(head, "{name}: {value}\r\n").unwrap();
    }
    if let Some(body) = body {
        write!(head, "Content-Length: {}\r\n", body.len()).unwrap();
    }
    head.push_str("\r\n");
    // The daemon may answer an oversized body before it has read all of it.
    let _ = stream.write_all(head.as_bytes());
    if let Some(body) = body {
        let _ = stream.write_all(body);
    }
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no response to {method} {target}: {raw:?}"));
    let media_type = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-type:")
                .map(|value| value.trim().to_owned())
        })
        .unwrap_or_default();
    Http {
        status: head[9..12].parse().unwrap(),
        media_type,
        body: body.to_owned(),
    }
}

/// A mutation with a valid `Idempotency-Key` and a JSON body.
fn post(socket: &Path, token: &str, target: &str, body: &Value) -> Http {
    http(
        socket,
        "POST",
        target,
        &[
            ("Authorization", &format!("Bearer {token}")),
            ("Content-Type", "application/json"),
            ("Idempotency-Key", KEY),
        ],
        Some(body.to_string().as_bytes()),
    )
}

fn get(socket: &Path, token: &str, target: &str) -> Http {
    http(
        socket,
        "GET",
        target,
        &[("Authorization", &format!("Bearer {token}"))],
        None,
    )
}

/// What a `400` must look like: a Problem of `code` whose `detail` holds
/// every `expected` fragment and repeats no secret.
fn assert_detail(label: &str, response: &Http, code: &str, expected: &[&str], secrets: &[&str]) {
    assert_eq!(response.status, 400, "{label}: {}", response.body);
    assert_eq!(response.media_type, "application/problem+json", "{label}");
    let body = response.json();
    validate_problem(400, &body).unwrap_or_else(|error| panic!("{label}: {error}"));
    validate_problem_detail(400, &body).unwrap_or_else(|error| panic!("{label}: {error}"));
    assert_eq!(
        body.get("code").and_then(JsonValue::as_str),
        Some(code),
        "{label}: {}",
        response.body
    );
    let detail = response
        .detail()
        .unwrap_or_else(|| panic!("{label}: no detail in {}", response.body));
    assert!(!detail.is_empty(), "{label}");
    for fragment in expected {
        assert!(
            detail.contains(fragment),
            "{label}: detail {detail:?} lacks {fragment:?}"
        );
    }
    for secret in secrets.iter().chain(&[PROBE]) {
        assert!(
            !response.body.contains(secret),
            "{label}: the response repeats {secret:?}: {}",
            response.body
        );
    }
    println!("{label:<44} {detail}");
}

fn invalid(label: &str, response: &Http, expected: &[&str], secrets: &[&str]) {
    assert_detail(label, response, "invalid_request", expected, secrets);
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

fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    command
}

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

struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn start_daemon(data_dir: &Path, log: &Path) -> Daemon {
    let child = isolated(env!("CARGO_BIN_EXE_memproved"))
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(fs::File::create(log).unwrap())
        .spawn()
        .unwrap();
    let daemon = Daemon { child };
    let socket = data_dir.join("memproved.sock");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if socket.exists()
            && UnixStream::connect(&socket).is_ok()
            && http(&socket, "GET", "/health/ready", &[], None).status == 200
        {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "memproved never became ready:\n{}",
            fs::read_to_string(log).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn str_of(value: &Value, name: &str) -> String {
    value[name]
        .as_str()
        .unwrap_or_else(|| panic!("{name} missing in {value}"))
        .to_owned()
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

#[allow(clippy::too_many_lines)] // One ordered scenario: a daemon, then one case per class.
#[test]
fn every_400_names_its_field_and_never_repeats_a_secret() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    git_repository(&repo);
    let not_git = root.path().join("plain");
    fs::create_dir(&not_git).unwrap();
    let data_dir = root.path().join("data");
    let token_file = root.path().join("control.token");
    let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--output")
        .arg(&token_file)
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    let control = fs::read_to_string(&token_file).unwrap().trim().to_owned();
    assert!(
        control.len() >= 40,
        "the control token is read from its file"
    );
    let _daemon = start_daemon(&data_dir, &root.path().join("daemon.log"));
    let socket = data_dir.join("memproved.sock");
    let secrets = [control.as_str()];

    // State the failing requests refer to: a Project, a Task, a Session.
    let project = post(
        &socket,
        &control,
        "/v1/projects/register",
        &json!({ "schema_version": 1, "root": repo }),
    );
    assert_eq!(project.status, 201, "{}", project.body);
    let project: Value = serde_json::from_str(&project.body).unwrap();
    let project_id = str_of(&project, "project_id");
    let workspace_id = str_of(&project, "root_workspace_id");
    let task = post(
        &socket,
        &control,
        "/v1/tasks",
        &json!({ "schema_version": 1, "project_id": project_id, "title": "detail" }),
    );
    assert_eq!(task.status, 201, "{}", task.body);
    let task: Value = serde_json::from_str(&task.body).unwrap();
    let (task_id, task_revision) = (str_of(&task, "task_id"), task["revision"].clone());
    let session = post(
        &socket,
        &control,
        "/v1/sessions",
        &json!({
            "schema_version": 1,
            "workspace_id": workspace_id,
            "agent_id": "018f47a0-0000-7000-8000-00000000a6e1",
            "task_id": task_id,
        }),
    );
    assert_eq!(session.status, 201, "{}", session.body);
    let session: Value = serde_json::from_str(&session.body).unwrap();
    let session_id = str_of(&session, "session_id");
    println!("\n==== 400 details ====");

    // --- headers -------------------------------------------------------
    let token_body = json!({ "schema_version": 1, "client_label": "x", "scopes": ["mcp-read"] });
    let bearer = format!("Bearer {control}");
    invalid(
        "createClientToken: no Idempotency-Key",
        &http(
            &socket,
            "POST",
            "/v1/auth/tokens",
            &[
                ("Authorization", &bearer),
                ("Content-Type", "application/json"),
            ],
            Some(token_body.to_string().as_bytes()),
        ),
        &["Idempotency-Key"],
        &secrets,
    );
    invalid(
        "createClientToken: short Idempotency-Key",
        &http(
            &socket,
            "POST",
            "/v1/auth/tokens",
            &[
                ("Authorization", &bearer),
                ("Content-Type", "application/json"),
                ("Idempotency-Key", PROBE.split_at(10).0),
            ],
            Some(token_body.to_string().as_bytes()),
        ),
        &["Idempotency-Key", "16 through 200"],
        &[control.as_str(), "PROBE-4f1c"],
    );
    invalid(
        "createClientToken: wrong Content-Type",
        &http(
            &socket,
            "POST",
            "/v1/auth/tokens",
            &[
                ("Authorization", &bearer),
                ("Content-Type", "text/plain"),
                ("Idempotency-Key", KEY),
            ],
            Some(token_body.to_string().as_bytes()),
        ),
        &["Content-Type", "application/json"],
        &secrets,
    );

    // --- body: JSON syntax ----------------------------------------------
    let with = |body: &str| {
        http(
            &socket,
            "POST",
            "/v1/auth/tokens",
            &[
                ("Authorization", &bearer),
                ("Content-Type", "application/json"),
                ("Idempotency-Key", KEY),
            ],
            Some(body.as_bytes()),
        )
    };
    invalid(
        "malformed JSON",
        &with(&format!(r#"{{"schema_version":1,"client_label":"{PROBE}"#)),
        &["not valid JSON", "byte"],
        &secrets,
    );
    invalid(
        "duplicate member",
        &with(r#"{"schema_version":1,"schema_version":1}"#),
        &["not valid JSON", "schema_version appears more than once"],
        &secrets,
    );
    invalid(
        "duplicate member named like a token",
        &with(&format!(r#"{{"{control}":1,"{control}":2}}"#)),
        &["not valid JSON", "a member appears more than once"],
        &secrets,
    );
    invalid(
        "trailing text after the JSON value",
        &with(&format!("{{}} {PROBE}")),
        &["not valid JSON"],
        &secrets,
    );

    // --- body: schema ----------------------------------------------------
    invalid(
        "unknown member",
        &with(r#"{"schema_version":1,"client_label":"x","scopes":["mcp-read"],"extra":1}"#),
        &["the request body has an unknown member extra"],
        &secrets,
    );
    invalid(
        "unknown member named like a token",
        &with(&format!(
            r#"{{"schema_version":1,"client_label":"x","scopes":["mcp-read"],"{control}":"{PROBE}"}}"#
        )),
        &["the request body has an unknown member"],
        &secrets,
    );
    invalid(
        "missing member",
        &with(r#"{"schema_version":1,"scopes":["mcp-read"]}"#),
        &["$.client_label is required"],
        &secrets,
    );
    invalid(
        "wrong type",
        &with(&format!(
            r#"{{"schema_version":1,"client_label":"x","scopes":"{PROBE}"}}"#
        )),
        &["$.scopes must be"],
        &secrets,
    );
    invalid(
        "value out of range",
        &with(r#"{"schema_version":1,"client_label":"","scopes":["mcp-read"]}"#),
        &["$.client_label", "1 through 128"],
        &secrets,
    );
    assert_detail(
        "unsupported_scope",
        &with(&format!(
            r#"{{"schema_version":1,"client_label":"x","scopes":["{PROBE}"]}}"#
        )),
        "unsupported_scope",
        &["$.scopes[]"],
        &secrets,
    );
    invalid(
        "schema: not a UUID",
        &post(
            &socket,
            &control,
            "/v1/sessions",
            &json!({
                "schema_version": 1,
                "workspace_id": PROBE,
                "agent_id": "018f47a0-0000-7000-8000-00000000a6e1",
            }),
        ),
        &["$.workspace_id"],
        &secrets,
    );
    invalid(
        "an oversized body",
        &http(
            &socket,
            "POST",
            "/v1/auth/tokens",
            &[
                ("Authorization", &bearer),
                ("Content-Type", "application/json"),
                ("Idempotency-Key", KEY),
            ],
            Some(&vec![b' '; OVERSIZED]),
        ),
        &["request body", "2097152"],
        &secrets,
    );

    // --- body: rules the handler checks ---------------------------------
    invalid(
        "registerProject: relative root",
        &post(
            &socket,
            &control,
            "/v1/projects/register",
            &json!({ "schema_version": 1, "root": "relative/path" }),
        ),
        &["$.root must be an absolute path"],
        &secrets,
    );
    invalid(
        "registerProject: not a Git worktree",
        &post(
            &socket,
            &control,
            "/v1/projects/register",
            &json!({ "schema_version": 1, "root": not_git }),
        ),
        &["$.root is not inside a Git worktree"],
        &secrets,
    );
    invalid(
        "resolveWorkspace: relative path",
        &post(
            &socket,
            &control,
            "/v1/workspaces/resolve",
            &json!({ "schema_version": 1, "project_id": project_id, "path": "rel" }),
        ),
        &["$.path must be an absolute path"],
        &secrets,
    );
    invalid(
        "transitionTask: force_reason",
        &post(
            &socket,
            &control,
            &format!("/v1/tasks/{task_id}/transition"),
            &json!({
                "schema_version": 1,
                "expected_revision": task_revision,
                "target_state": "ready",
                "force_reason": PROBE,
            }),
        ),
        &["$.force_reason"],
        &secrets,
    );
    invalid(
        "transitionTask: evidence for another target",
        &post(
            &socket,
            &control,
            &format!("/v1/tasks/{task_id}/transition"),
            &json!({
                "schema_version": 1,
                "expected_revision": task_revision,
                "target_state": "ready",
                "completion_evidence_ids": [UNKNOWN_ID],
            }),
        ),
        &["$.completion_evidence_ids", "target_state completed"],
        &secrets,
    );
    let event = |event_type: &str, producer: &str, sequence: Value| {
        post(
            &socket,
            &control,
            "/v1/events",
            &json!({
                "envelope_version": "1.0",
                "event_id": "018f47a0-0000-7000-8000-0000000000e1",
                "project_id": project_id,
                "workspace_id": workspace_id,
                "session_id": session_id,
                "event_type": event_type,
                "payload_schema": "turn.started@1",
                "producer": { "name": producer, "version": "1.0.0", "sequence": sequence },
                "privacy_class": "internal",
                "payload": { "note": PROBE },
            }),
        )
    };
    invalid(
        "ingestEvent: an Event type only memproved writes",
        &event("session.started", "e2e-hook", json!(1)),
        &["$.event_type"],
        &secrets,
    );
    invalid(
        "ingestEvent: a producer named like continuityd",
        &event("turn.started", "continuityd-fake", json!(1)),
        &["$.producer.name"],
        &secrets,
    );
    invalid(
        "ingestEvent: producer.sequence out of range",
        &event("turn.started", "e2e-hook", json!(9_007_199_254_740_992_u64)),
        &["$.producer.sequence"],
        &secrets,
    );
    let capture = |edit: &dyn Fn(&mut Value)| {
        let mut body = json!({
            "schema_version": 1,
            "workspace_id": workspace_id,
            "task_id": task_id,
            "session_id": session_id,
            "artifact_type": "command_run",
            "snapshot_id": "018f47a0-0000-7000-8000-0000000000c1",
            "metadata": {
                "argv": ["true"],
                "exit_code": 0,
                "started_at": "2026-10-02T10:00:00Z",
                "finished_at": "2026-10-02T10:00:01Z",
                "stdout_tail": "",
                "stderr_tail": "",
                "stdout_sha256": digest('a'),
                "stderr_sha256": digest('b'),
                "collector": { "name": "memprove-cli", "version": "0.1.0" },
            },
        });
        edit(&mut body);
        post(&socket, &control, "/v1/evidence/capture", &body)
    };
    invalid(
        "captureEvidence: test fields on a command_run",
        &capture(&|body| body["metadata"]["test_name"] = json!(PROBE)),
        &["$.metadata.test_name", "test_run"],
        &secrets,
    );
    invalid(
        "captureEvidence: a collector named like continuityd",
        &capture(&|body| body["metadata"]["collector"]["name"] = json!("continuityd")),
        &["$.metadata.collector.name"],
        &secrets,
    );
    invalid(
        "captureEvidence: a run that ends before it starts",
        &capture(&|body| {
            body["metadata"]["started_at"] = json!("2026-10-02T11:00:00Z");
        }),
        &["$.metadata", "run interval"],
        &secrets,
    );
    invalid(
        "createCheckpoint: a fact without Evidence",
        &post(
            &socket,
            &control,
            "/v1/checkpoints",
            &json!({
                "schema_version": 1,
                "workspace_id": workspace_id,
                "task_id": task_id,
                "session_id": session_id,
                "snapshot_id": "018f47a0-0000-7000-8000-0000000000c2",
                "objective": "detail",
                "verified_completed_work": [
                    { "classification": "verified_fact", "text": PROBE },
                ],
                "open_items": [],
                "next_action": { "classification": "recommendation", "text": "go" },
            }),
        ),
        &["$.verified_completed_work[0]", "at least one Evidence"],
        &secrets,
    );
    invalid(
        "verifyLedger: not a UUID",
        &post(
            &socket,
            &control,
            "/v1/verification/ledger",
            &json!({ "schema_version": 1, "project_id": PROBE }),
        ),
        &["$.project_id"],
        &secrets,
    );

    // --- path parameters -------------------------------------------------
    for (label, target, parameter) in [
        ("getProject", "/v1/projects/", "project_id"),
        ("getEvent", "/v1/events/", "event_id"),
        ("getEvidence", "/v1/evidence/", "evidence_id"),
        ("getCheckpoint", "/v1/checkpoints/", "checkpoint_id"),
    ] {
        invalid(
            &format!("{label}: path parameter"),
            &get(&socket, &control, &format!("{target}{PROBE}")),
            &[&format!(
                "path parameter {parameter} must be a lowercase UUIDv7"
            )],
            &secrets,
        );
    }
    invalid(
        "getRecoveryPreview: path parameter",
        &get(
            &socket,
            &control,
            &format!("/v1/tasks/{PROBE}/recovery-preview"),
        ),
        &["path parameter task_id"],
        &secrets,
    );
    invalid(
        "endSession: path parameter",
        &post(
            &socket,
            &control,
            &format!("/v1/sessions/{PROBE}/end"),
            &json!({ "schema_version": 1, "expected_revision": 1 }),
        ),
        &["path parameter session_id"],
        &secrets,
    );
    invalid(
        "revokeClientToken: path parameter",
        &http(
            &socket,
            "POST",
            &format!("/v1/auth/tokens/{PROBE}/revoke"),
            &[("Authorization", &bearer), ("Idempotency-Key", KEY)],
            Some(b""),
        ),
        &["path parameter token_id"],
        &secrets,
    );
    invalid(
        "revokeClientToken: a body",
        &http(
            &socket,
            "POST",
            &format!("/v1/auth/tokens/{UNKNOWN_ID}/revoke"),
            &[
                ("Authorization", &bearer),
                ("Content-Type", "application/json"),
                ("Idempotency-Key", KEY),
            ],
            Some(PROBE.as_bytes()),
        ),
        &["no request body"],
        &secrets,
    );

    // --- query parameters ------------------------------------------------
    let tasks = |query: &str| get(&socket, &control, &format!("/v1/tasks?{query}"));
    invalid(
        "listTasks: limit",
        &tasks(&format!("project_id={project_id}&limit=0")),
        &["query parameter limit", "1 through 500"],
        &secrets,
    );
    invalid(
        "listTasks: limit that is not a number",
        &tasks(&format!("project_id={project_id}&limit={PROBE}")),
        &["query parameter limit"],
        &secrets,
    );
    invalid(
        "listTasks: cursor",
        &tasks(&format!("project_id={project_id}&cursor={PROBE}")),
        &["query parameter cursor must be a lowercase UUIDv7"],
        &secrets,
    );
    invalid(
        "listTasks: a cursor that names no record",
        &tasks(&format!("project_id={project_id}&cursor={UNKNOWN_ID}")),
        &["query parameter cursor does not name an existing record"],
        &secrets,
    );
    invalid(
        "listTasks: missing project_id",
        &tasks("limit=1"),
        &["query parameter project_id is required"],
        &secrets,
    );
    invalid(
        "listTasks: unknown parameter",
        &tasks(&format!("project_id={project_id}&extra=1")),
        &["unknown query parameter extra"],
        &secrets,
    );
    invalid(
        "listTasks: a parameter named like a token",
        &tasks(&format!("project_id={project_id}&{control}=1")),
        &["unknown query parameter"],
        &secrets,
    );
    invalid(
        "listTasks: repeated parameter",
        &tasks(&format!("project_id={project_id}&project_id={project_id}")),
        &["query parameter project_id was given more than once"],
        &secrets,
    );
    invalid(
        "listSessions: workspace_id",
        &get(
            &socket,
            &control,
            &format!("/v1/sessions?workspace_id={PROBE}"),
        ),
        &["query parameter workspace_id"],
        &secrets,
    );
    invalid(
        "listEvents: project_id",
        &get(&socket, &control, &format!("/v1/events?project_id={PROBE}")),
        &["query parameter project_id"],
        &secrets,
    );
    invalid(
        "listClientTokens: limit",
        &get(&socket, &control, "/v1/auth/tokens?limit=501"),
        &["query parameter limit"],
        &secrets,
    );
    invalid(
        "getRecoveryPreview: workspace_id",
        &get(
            &socket,
            &control,
            &format!("/v1/tasks/{task_id}/recovery-preview?workspace_id={PROBE}"),
        ),
        &["query parameter workspace_id"],
        &secrets,
    );
    invalid(
        "getRecoveryPreview: unknown parameter",
        &get(
            &socket,
            &control,
            &format!("/v1/tasks/{task_id}/recovery-preview?{control}=1"),
        ),
        &["unknown query parameter"],
        &secrets,
    );

    // --- 401 and 403 carry no detail -------------------------------------
    let anonymous = http(&socket, "GET", "/v1/status", &[], None);
    assert_eq!(anonymous.status, 401);
    validate_problem_detail(401, &anonymous.json()).unwrap();
    assert!(anonymous.detail().is_none(), "{}", anonymous.body);
    let reader = post(&socket, &control, "/v1/auth/tokens", &token_body);
    assert_eq!(reader.status, 201, "{}", reader.body);
    let reader: Value = serde_json::from_str(&reader.body).unwrap();
    let denied = get(&socket, &str_of(&reader, "secret"), "/v1/auth/tokens");
    assert_eq!(denied.status, 403, "{}", denied.body);
    validate_problem_detail(403, &denied.json()).unwrap();
    assert!(denied.detail().is_none(), "{}", denied.body);

    // --- the CLI shows the detail on standard error ---------------------
    let run = isolated(cli_binary())
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--token-file")
        .arg(&token_file)
        .args(["task", "list", "--project", &project_id, "--limit", "0"])
        .output()
        .unwrap();
    let (stdout, stderr) = (text(&run.stdout), text(&run.stderr));
    assert_eq!(run.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains(
            "listTasks failed: 400 invalid_request: query parameter limit must be an integer \
             from 1 through 500"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains(&control), "{stderr}");
    println!("[cli] {}", stderr.trim());
}
