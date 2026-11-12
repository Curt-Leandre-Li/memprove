//! P4-G (ADR 0014 E7.11.1, Owner conditions q, r, x, y) through the real
//! binary and no daemon: `memprove mcp serve` driven by a minimal client
//! written here (piped standard streams, one JSON line per message, one
//! line read per expected answer within 15 s, standard error kept whole).
//! Both handshakes, the per-request classification, malformed and
//! oversized input, notifications and subscriptions, standard-output
//! purity, startup and shutdown, statements only, and the tool names.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use memprove_cli::context::{Context, ContextStore};
use memprove_protocol::strict_json::{JsonValue, parse_strict};

/// The longest a test waits for one answer.
const DEADLINE: Duration = Duration::from_secs(15);
/// The tools, in the order of `tools/list`.
const TOOLS: [&str; 10] = [
    "pack",
    "claims",
    "context",
    "tasks",
    "ledger_verify",
    "begin",
    "checkpoint",
    "decide",
    "end",
    "complete",
];
const SERVER_INFO: &str = r#"{"name":"memprove","title":"memprove","version":"0.1.0"}"#;

/// A running `memprove mcp serve` and its streams.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Vec<u8>>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    readers: Vec<thread::JoinHandle<()>>,
}

/// The binary with no `MEMPROVE_*`, no `CLAUDE_PROJECT_DIR`, and no
/// `CLAUDE_CODE_SESSION_ID` of the test's own environment, at home in `home`.
fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memprove"));
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        // U1 (A1): the server takes `CLAUDE_CODE_SESSION_ID` as its client
        // session; a test sets it only on purpose.
        if name_text.starts_with("MEMPROVE_")
            || name_text == "CLAUDE_PROJECT_DIR"
            || name_text == "CLAUDE_CODE_SESSION_ID"
        {
            command.env_remove(name);
        }
    }
    command
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

impl Mcp {
    fn start(home: &Path, env: &[(&str, OsString)]) -> Self {
        Self::spawn(home, env, true)
    }

    /// As [`Mcp::start`]; with `read_stderr` false the read end of the
    /// server's standard error is closed at once, so a write to it fails
    /// with a broken pipe.
    fn spawn(home: &Path, env: &[(&str, OsString)], read_stderr: bool) -> Self {
        let mut command = command(home);
        command
            .args(["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let (sender, lines) = mpsc::channel();
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let all = Arc::clone(&stdout);
        let reader = thread::spawn(move || {
            loop {
                let mut line = Vec::new();
                match out.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        all.lock().unwrap().extend_from_slice(&line);
                        if sender.send(line).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut err = child.stderr.take().unwrap();
        let log = Arc::clone(&stderr);
        let mut readers = vec![reader];
        if read_stderr {
            readers.push(thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = err.read_to_end(&mut bytes);
                log.lock().unwrap().extend_from_slice(&bytes);
            }));
        } else {
            drop(err);
        }
        Self {
            child,
            stdin,
            lines,
            stdout,
            stderr,
            readers,
        }
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn send(&mut self, line: &str) {
        self.send_bytes(format!("{line}\n").as_bytes());
    }

    /// The next line of standard output, without its line feed.
    fn recv(&self) -> String {
        let line = self
            .lines
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| panic!("no answer within {DEADLINE:?}"));
        let line = line.strip_suffix(b"\n").expect("a line ends in \\n");
        String::from_utf8(line.to_vec()).unwrap()
    }

    /// `line` sent, its one answer read.
    fn ask(&mut self, line: &str) -> String {
        self.send(line);
        self.recv()
    }

    /// Asserts that nothing is written within `wait`.
    fn quiet(&self, wait: Duration) {
        if let Ok(line) = self.lines.recv_timeout(wait) {
            panic!("unexpected output: {}", String::from_utf8_lossy(&line));
        }
    }

    /// Closes standard input and waits for the exit; returns the status,
    /// the lines still unread, all of standard output, and standard error.
    fn finish(mut self) -> (ExitStatus, Vec<String>, Vec<u8>, String) {
        drop(self.stdin.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < DEADLINE, "the server did not exit");
            thread::sleep(Duration::from_millis(10));
        };
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        let rest = self
            .lines
            .try_iter()
            .map(|line| String::from_utf8(line).unwrap().trim_end().to_owned())
            .collect();
        let stdout = self.stdout.lock().unwrap().clone();
        let stderr = String::from_utf8(self.stderr.lock().unwrap().clone()).unwrap();
        (status, rest, stdout, stderr)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn meta() -> &'static str {
    r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}"#
}

/// A modern request.
fn modern(id: &str, method: &str, rest: &str) -> String {
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!(",{rest}")
    };
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{{}{rest}}}}}"#,
        meta()
    )
}

/// A legacy request.
fn legacy(id: &str, method: &str, params: Option<&str>) -> String {
    match params {
        Some(params) => {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
        }
        None => format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"}}"#),
    }
}

fn initialize(id: &str, version: &str) -> String {
    legacy(
        id,
        "initialize",
        Some(&format!(
            r#"{{"protocolVersion":"{version}","capabilities":{{"roots":{{"listChanged":true}},"sampling":{{}}}},"clientInfo":{{"name":"claude-code","version":"2.1.290"}}}}"#
        )),
    )
}

fn json(text: &str) -> JsonValue {
    parse_strict(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

fn member<'a>(value: &'a JsonValue, path: &[&str]) -> &'a JsonValue {
    path.iter()
        .try_fold(value, |value, name| value.get(name))
        .unwrap_or_else(|| panic!("no {path:?}"))
}

fn names(value: &JsonValue) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect()
}

/// The `tools` array of a `tools/list` answer, as the bytes it was sent in:
/// up to `,"ttlMs"` (modern) or the closing `}}` (legacy).
fn tools_bytes(line: &str) -> &str {
    let start = line.find(r#""tools":["#).unwrap() + r#""tools":"#.len();
    let end = line.find(r#","ttlMs""#).unwrap_or(line.len() - 2);
    &line[start..end]
}

/// Every property name anywhere in `schema`.
fn property_names(schema: &JsonValue) -> Vec<String> {
    let mut found = Vec::new();
    if let Some(properties) = schema.get("properties").and_then(JsonValue::as_object) {
        found.extend(properties.iter().map(|(name, _)| name.clone()));
    }
    match schema {
        JsonValue::Object(members) => {
            for (_, value) in members {
                found.extend(property_names(value));
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                found.extend(property_names(item));
            }
        }
        _ => {}
    }
    found
}

/// Every object in `schema` has `"additionalProperties": false`.
fn every_object_closed(schema: &JsonValue) -> bool {
    match schema {
        JsonValue::Object(members) => {
            let closed = schema.get("type").and_then(JsonValue::as_str) != Some("object")
                || schema.get("additionalProperties") == Some(&JsonValue::Bool(false));
            closed && members.iter().all(|(_, value)| every_object_closed(value))
        }
        JsonValue::Array(items) => items.iter().all(every_object_closed),
        _ => true,
    }
}

#[test]
fn modern_handshake_lists_the_ten_tools_in_order() {
    let home = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::start(home.path(), &[]);
    let discover = mcp.ask(&modern(r#""discover-1""#, "server/discover", ""));
    let value = json(&discover);
    assert_eq!(names(&value), ["jsonrpc", "id", "result"]);
    let result = member(&value, &["result"]);
    assert_eq!(
        names(result),
        [
            "resultType",
            "supportedVersions",
            "capabilities",
            "instructions",
            "ttlMs",
            "cacheScope",
            "_meta"
        ]
    );
    assert!(discover.starts_with(r#"{"jsonrpc":"2.0","id":"discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"instructions":"memprove keeps"#));
    assert!(discover.ends_with(&format!(
        r#","ttlMs":3600000,"cacheScope":"public","_meta":{{"io.modelcontextprotocol/serverInfo":{SERVER_INFO}}}}}}}"#
    )));

    let list = mcp.ask(&modern("2", "tools/list", ""));
    let value = json(&list);
    let result = member(&value, &["result"]);
    assert_eq!(
        names(result),
        ["resultType", "tools", "ttlMs", "cacheScope", "_meta"]
    );
    assert_eq!(member(result, &["resultType"]).as_str(), Some("complete"));
    assert!(member(result, &["ttlMs"]).as_i64().unwrap() >= 0);
    let tools = member(result, &["tools"]).as_array().unwrap();
    let listed: Vec<&str> = tools
        .iter()
        .map(|tool| member(tool, &["name"]).as_str().unwrap())
        .collect();
    assert_eq!(listed, TOOLS);
    for tool in tools {
        assert_eq!(
            names(tool),
            ["name", "description", "inputSchema", "annotations"]
        );
        let schema = member(tool, &["inputSchema"]);
        assert_eq!(member(schema, &["type"]).as_str(), Some("object"));
        assert!(every_object_closed(schema), "{}", list);
        assert!(schema.get("$schema").is_none());
        let read_only = member(tool, &["annotations", "readOnlyHint"]);
        let name = member(tool, &["name"]).as_str().unwrap();
        assert_eq!(
            read_only,
            &JsonValue::Bool(TOOLS[..5].contains(&name)),
            "{name}"
        );
    }
    let again = mcp.ask(&modern("2", "tools/list", ""));
    assert_eq!(again, list, "two tools/list replies differ");

    let context = mcp.ask(&modern("3", "tools/call", r#""name":"context""#));
    assert!(
        context.starts_with(r#"{"jsonrpc":"2.0","id":3,"result":{"resultType":"complete","content":[{"type":"text","text":"#),
        "{context}"
    );
    let (status, rest, _, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(rest.is_empty(), "{rest:?}");
}

#[test]
fn legacy_handshake_in_both_versions() {
    let home = tempfile::tempdir().unwrap();
    let modern_tools = {
        let mut mcp = Mcp::start(home.path(), &[]);
        let list = mcp.ask(&modern("1", "tools/list", ""));
        tools_bytes(&list).to_owned()
    };
    for (asked, answered) in [
        ("2025-11-25", "2025-11-25"),
        ("2025-06-18", "2025-06-18"),
        ("2024-11-05", "2025-11-25"),
        ("x", "2025-11-25"),
    ] {
        let mut mcp = Mcp::start(home.path(), &[]);
        assert_eq!(
            mcp.ask(&legacy("0", "ping", None)),
            r#"{"jsonrpc":"2.0","id":0,"result":{}}"#
        );
        let answer = mcp.ask(&initialize("1", asked));
        assert!(
            answer.starts_with(&format!(
                r#"{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{answered}","capabilities":{{"tools":{{}}}},"serverInfo":{SERVER_INFO},"instructions":"memprove keeps"#
            )),
            "{answer}"
        );
        assert!(!answer.contains("resultType") && !answer.contains("_meta"));
        mcp.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        mcp.quiet(Duration::from_millis(200));
        assert_eq!(
            mcp.ask(&legacy("\"p\"", "ping", None)),
            r#"{"jsonrpc":"2.0","id":"p","result":{}}"#
        );
        let list = mcp.ask(&legacy("2", "tools/list", None));
        assert!(list.starts_with(r#"{"jsonrpc":"2.0","id":2,"result":{"tools":["#));
        for absent in ["resultType", "ttlMs", "_meta"] {
            assert!(!list.contains(absent), "{absent}");
        }
        assert_eq!(tools_bytes(&list), modern_tools, "the lists differ");
        let call = mcp.ask(&legacy(
            "3",
            "tools/call",
            Some(r#"{"name":"context","arguments":{}}"#),
        ));
        assert!(
            call.starts_with(
                r#"{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"#
            ),
            "{call}"
        );
        for absent in ["resultType", "_meta"] {
            assert!(!call.contains(absent), "{absent}");
        }
        let (status, _, _, _) = mcp.finish();
        assert_eq!(status.code(), Some(0));
    }
}

#[test]
fn each_request_is_classified_on_its_own() {
    let home = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::start(home.path(), &[]);
    let both = r#""data":{"missing":["io.modelcontextprotocol/protocolVersion"],"supported":["2026-07-28"],"initialize":["2025-11-25","2025-06-18"]}"#;
    assert!(
        mcp.ask(&modern("1", "tools/list", ""))
            .contains(r#""result":{"resultType":"complete","tools":["#)
    );
    for method in [
        "tools/list",
        "tools/call",
        "server/discover",
        "subscriptions/listen",
    ] {
        assert_eq!(
            mcp.ask(&legacy("2", method, None)),
            format!(
                r#"{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"Invalid params",{both}}}}}"#
            ),
            "{method}"
        );
    }
    let init = mcp.ask(&legacy(
        "3",
        "initialize",
        Some(&format!(r#"{{"protocolVersion":"2025-06-18",{}}}"#, meta())),
    ));
    assert!(init.contains(r#""protocolVersion":"2025-06-18""#), "{init}");
    assert!(!init.contains("resultType"));
    // Both eras in one process.
    assert!(
        mcp.ask(&modern("4", "server/discover", ""))
            .contains(r#""resultType":"complete""#)
    );
    assert!(
        mcp.ask(&legacy("5", "tools/call", Some(r#"{"name":"context"}"#)))
            .starts_with(r#"{"jsonrpc":"2.0","id":5,"result":{"content""#)
    );
    for (version, id) in [("2025-11-25", "6"), ("1900-01-01", "7")] {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/list","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"{version}","io.modelcontextprotocol/clientCapabilities":{{}}}}}}}}"#
        );
        assert_eq!(
            mcp.ask(&line),
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32022,"message":"Unsupported protocol version","data":{{"supported":["2026-07-28"],"requested":"{version}"}}}}}}"#
            )
        );
    }
    assert_eq!(
        mcp.ask(r#"{"jsonrpc":"2.0","id":8,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":20260728,"io.modelcontextprotocol/clientCapabilities":{}}}}"#),
        r#"{"jsonrpc":"2.0","id":8,"error":{"code":-32602,"message":"Invalid params","data":{"missing":["io.modelcontextprotocol/protocolVersion"]}}}"#
    );
    assert_eq!(
        mcp.ask(r#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#),
        r#"{"jsonrpc":"2.0","id":9,"error":{"code":-32602,"message":"Invalid params","data":{"missing":["io.modelcontextprotocol/clientCapabilities"]}}}"#
    );
    assert_eq!(
        mcp.ask(&modern("10", "ping", "")),
        format!(
            r#"{{"jsonrpc":"2.0","id":10,"result":{{"resultType":"complete","_meta":{{"io.modelcontextprotocol/serverInfo":{SERVER_INFO}}}}}}}"#
        )
    );
    assert_eq!(
        mcp.ask(&modern("11", "tools/list", r#""cursor":"x""#)),
        r#"{"jsonrpc":"2.0","id":11,"error":{"code":-32602,"message":"Invalid params"}}"#
    );
    for method in ["resources/list", "prompts/list", "logging/setLevel"] {
        let not_found = r#""error":{"code":-32601,"message":"Method not found"}}"#;
        assert!(
            mcp.ask(&modern("12", method, ""))
                .ends_with(&format!(r#""id":12,{not_found}"#))
        );
        assert!(
            mcp.ask(&legacy("13", method, None))
                .ends_with(&format!(r#""id":13,{not_found}"#))
        );
    }
    let (status, rest, _, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0));
    assert!(rest.is_empty());
    // One log line per message, naming no method the server does not know.
    assert!(
        stderr.contains("memprove mcp: legacy initialize -> ok"),
        "{stderr}"
    );
    assert!(
        stderr.contains("memprove mcp: modern unknown -> -32601"),
        "{stderr}"
    );
    assert!(!stderr.contains("resources/list"), "{stderr}");
}

#[test]
fn malformed_and_oversized_input_is_answered_and_the_next_request_served() {
    let home = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::start(home.path(), &[]);
    let probe = |id: u32| legacy(&id.to_string(), "ping", None);
    let answer = |id: u32| format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#);
    let parse_error =
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#;
    let mut id = 0;
    let mut bad = |mcp: &mut Mcp, bytes: Vec<u8>, expected: &str| {
        mcp.send_bytes(&bytes);
        mcp.send_bytes(b"\n");
        assert_eq!(
            mcp.recv(),
            expected,
            "{}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(80)])
        );
        id += 1;
        assert_eq!(mcp.ask(&probe(id)), answer(id));
    };
    let mut over = vec![b'x'; 1_048_577];
    over[0] = b'{';
    let nested = format!("{}{}", "[".repeat(129), "]".repeat(129));
    for bytes in [
        b"not json".to_vec(),
        vec![b'{', 0xff, 0xfe, b'}'],
        over,
        vec![b' '; 2 << 20],
        nested.into_bytes(),
        br#"{"jsonrpc":"2.0","id":1,"id":2,"method":"ping"}"#.to_vec(),
    ] {
        bad(&mut mcp, bytes, parse_error);
    }
    let invalid = |id: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32600,"message":"Invalid Request"}}}}"#
        )
    };
    for (line, echoed) in [
        ("[]", "null"),
        (r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#, "null"),
        (r#""x""#, "null"),
        ("null", "null"),
        ("{}", "null"),
        (r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#, "1"),
        (r#"{"jsonrpc":"2.0","id":"a","method":5}"#, r#""a""#),
        (r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#, "null"),
        (r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#, "null"),
        (r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#, "null"),
    ] {
        bad(&mut mcp, line.as_bytes().to_vec(), &invalid(echoed));
    }
    // A response sent by the client, empty and blank lines: no output.
    mcp.send(r#"{"jsonrpc":"2.0","id":5,"result":{}}"#);
    mcp.send("");
    mcp.send(" \t ");
    mcp.send_bytes(b"{\"jsonrpc\":\"2.0\",\"id\":77,\"method\":\"ping\"}\r\n");
    assert_eq!(mcp.recv(), answer(77));
    for id in [r#""discover-1""#, "12345678901234567890"] {
        assert_eq!(
            mcp.ask(&modern(id, "nope/nothing", "")),
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"Method not found"}}}}"#
            )
        );
    }
    let (status, rest, _, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(rest.is_empty(), "{rest:?}");
}

/// A tool name or an argument value never appears in any output; a bad
/// argument is a tool error naming the parameter only.
#[test]
fn bad_tool_calls_never_echo_what_was_sent() {
    let home = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::start(home.path(), &[]);
    let sentinel = "SENTINEL-7f3a";
    assert_eq!(
        mcp.ask(&modern(
            "20",
            "tools/call",
            &format!(r#""name":"nope-{sentinel}""#)
        )),
        r#"{"jsonrpc":"2.0","id":20,"error":{"code":-32602,"message":"Unknown tool"}}"#
    );
    assert_eq!(
        mcp.ask(&modern(
            "21",
            "tools/call",
            r#""name":"pack","arguments":[1]"#
        )),
        r#"{"jsonrpc":"2.0","id":21,"error":{"code":-32602,"message":"Invalid params"}}"#
    );
    for (tool, arguments, parameter) in [
        ("pack", format!(r#"{{"{sentinel}":1}}"#), "null"),
        (
            "pack",
            format!(r#"{{"budget":"{sentinel}"}}"#),
            r#""budget""#,
        ),
        (
            "complete",
            format!(r#"{{"evidence":["r0"],"x{sentinel}":1}}"#),
            "null",
        ),
        (
            "complete",
            r#"{"evidence":["r0"]}"#.to_owned(),
            r#""evidence""#,
        ),
        (
            "claims",
            r#"{"claims":[1],"task":"018f47a0-0000-7000-8000-000000000004"}"#.to_owned(),
            r#""task""#,
        ),
        (
            "decide",
            format!(r#"{{"text":"{sentinel}","evidence":["{sentinel}"]}}"#),
            r#""evidence""#,
        ),
    ] {
        let answer = mcp.ask(&modern(
            "22",
            "tools/call",
            &format!(r#""name":"{tool}","arguments":{arguments}"#),
        ));
        assert!(
            answer.contains(&format!(
                r#""structuredContent":{{"task_id":null,"session_id":null,"failure":"invalid_arguments","parameter":{parameter}}},"isError":true"#
            )),
            "{answer}"
        );
    }
    let (status, rest, stdout, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(rest.is_empty(), "{rest:?}");
    assert!(!String::from_utf8_lossy(&stdout).contains(sentinel));
    assert!(!stderr.contains(sentinel), "{stderr}");
}

#[test]
fn notifications_get_no_answer_and_subscriptions_complete() {
    let home = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::start(home.path(), &[]);
    for method in [
        "notifications/initialized",
        "notifications/roots/list_changed",
        "notifications/progress",
        "notifications/message",
        "notifications/whatever",
    ] {
        mcp.send(&format!(r#"{{"jsonrpc":"2.0","method":"{method}"}}"#));
    }
    mcp.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":42,"reason":"gone"}}"#);
    mcp.quiet(Duration::from_millis(300));
    let listen = |id: u32| {
        modern(
            &id.to_string(),
            "subscriptions/listen",
            r#""notifications":{"toolsListChanged":true}"#,
        )
    };
    let ack = |id: u32| {
        format!(
            r#"{{"jsonrpc":"2.0","method":"notifications/subscriptions/acknowledged","params":{{"_meta":{{"io.modelcontextprotocol/subscriptionId":{id}}},"notifications":{{}}}}}}"#
        )
    };
    let done = |id: u32| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"result":{{"resultType":"complete","_meta":{{"io.modelcontextprotocol/subscriptionId":{id},"io.modelcontextprotocol/serverInfo":{SERVER_INFO}}}}}}}"#
        )
    };
    assert_eq!(mcp.ask(&listen(7)), ack(7));
    mcp.quiet(Duration::from_millis(200));
    mcp.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#);
    assert_eq!(mcp.recv(), done(7));
    assert_eq!(mcp.ask(&listen(8)), ack(8));
    let (status, rest, _, _) = mcp.finish();
    assert_eq!(status.code(), Some(0));
    assert_eq!(rest, [done(8)]);
}

/// A context of `workspace` (its own Workspace root) in `config`, with
/// made-up identities: the tools then reach for the daemon.
fn fake_context(workspace: &Path, config: &Path) {
    let id = |n: u32| format!("018f47a0-0000-7000-8000-{n:012}");
    let context = Context {
        workspace_root: std::fs::canonicalize(workspace).unwrap(),
        project_id: id(1),
        workspace_id: id(2),
        agent_id: id(3),
        task_id: Some(id(4)),
        task_title: Some("t".to_owned()),
        task_revision: Some(2),
        task_state: Some("in_progress".to_owned()),
        session_id: Some(id(5)),
        session_revision: Some(1),
        snapshot_id: None,
        snapshot_manifest: None,
        evidence_id: None,
        evidence_status: None,
        runs: Vec::new(),
        next_run: 1,
        agent_name: None,
        agent_role: None,
        parent_session_id: None,
    };
    ContextStore::from_env(Some(config.as_os_str().to_owned()), None, None)
        .unwrap()
        .save(&context)
        .unwrap();
}

/// Every byte of standard output is one JSON-RPC message per line, as many
/// lines as requests, whatever goes wrong behind the tools: no socket, then
/// a socket and a token file other users can read, with `RUST_BACKTRACE`.
#[test]
fn standard_output_holds_protocol_messages_only() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let data = home.path().join("data");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    fake_context(&workspace, &config);
    std::fs::write(config.join("data-dir"), format!("{}\n", data.display())).unwrap();
    let token = config.join("token");
    std::fs::write(&token, "not-a-real-token-0644\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
    for (env, expected) in [
        (
            vec![
                ("MEMPROVE_CONFIG_DIR", config.clone().into_os_string()),
                ("CLAUDE_PROJECT_DIR", workspace.clone().into_os_string()),
            ],
            "daemon_not_running",
        ),
        (
            vec![
                ("MEMPROVE_CONFIG_DIR", config.clone().into_os_string()),
                ("CLAUDE_PROJECT_DIR", workspace.clone().into_os_string()),
                ("RUST_BACKTRACE", OsString::from("1")),
            ],
            "token_unusable",
        ),
    ] {
        if expected == "token_unusable" {
            std::fs::write(data.join("memproved.sock"), b"").unwrap();
        }
        let mut mcp = Mcp::start(home.path(), &env);
        let requests = [
            modern("1", "server/discover", ""),
            modern("2", "tools/list", ""),
            modern("3", "tools/call", r#""name":"pack""#),
            modern("4", "tools/call", r#""name":"tasks""#),
            modern("5", "tools/call", r#""name":"ledger_verify""#),
            modern(
                "6",
                "tools/call",
                r#""name":"claims","arguments":{"claims":[1]}"#,
            ),
            modern("7", "tools/call", r#""name":"begin""#),
            modern("8", "tools/call", r#""name":"context""#),
            initialize("9", "2025-11-25"),
            legacy(
                "10",
                "tools/call",
                Some(r#"{"name":"pack","arguments":{"task":1}}"#),
            ),
        ];
        for request in &requests {
            mcp.send(request);
        }
        let (status, rest, stdout, stderr) = mcp.finish();
        assert_eq!(status.code(), Some(0), "{stderr}");
        assert_eq!(rest.len(), requests.len(), "{rest:?}");
        let text = String::from_utf8(stdout).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), requests.len());
        for line in text.lines() {
            let value = json(line);
            assert_eq!(member(&value, &["jsonrpc"]).as_str(), Some("2.0"), "{line}");
            assert!(value.get("result").is_some() || value.get("error").is_some());
        }
        // The four read tools that ask the daemon, the legacy `pack`, and
        // (P4-G part G-2) `begin`, which reaches it too.
        assert_eq!(
            text.matches(&format!(r#""failure":"{expected}""#)).count(),
            6,
            "{text}"
        );
        assert!(text.contains(r#""task_id":"018f47a0-0000-7000-8000-000000000004","session_id":"018f47a0-0000-7000-8000-000000000005""#));
        assert!(!text.contains("not-a-real-token") && !stderr.contains("not-a-real-token"));
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.starts_with("memprove mcp: "))
                .count(),
            requests.len(),
            "{stderr}"
        );
    }
}

#[test]
fn startup_does_nothing_and_shutdown_is_prompt() {
    let home = tempfile::tempdir().unwrap();
    // Nothing before the first line.
    let mcp = Mcp::start(home.path(), &[]);
    mcp.quiet(Duration::from_millis(300));
    let (status, rest, stdout, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0));
    assert!(
        rest.is_empty() && stdout.is_empty() && stderr.is_empty(),
        "{stderr}"
    );
    assert!(
        std::fs::read_dir(home.path()).unwrap().next().is_none(),
        "a file was written"
    );

    // One `server/discover`, then the end of input: one line, exit 0.
    let mut command = command(home.path());
    let mut child = command
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(format!("{}\n", modern("1", "server/discover", "")).as_bytes())
        .unwrap();
    drop(stdin);
    let started = Instant::now();
    let output = child.wait_with_output().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);

    // SIGTERM ends it.
    let mcp = Mcp::start(home.path(), &[]);
    let killed = Command::new("kill")
        .args(["-TERM", &mcp.child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let (status, _, _, _) = mcp.finish();
    assert!(
        status.signal() == Some(15) || status.code() == Some(0),
        "{status:?}"
    );

    // A closed standard output: exit 0, no panic message.
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(format!("{}\n", legacy("1", "ping", None)).as_bytes());
    let _ = stdin.write_all(format!("{}\n", legacy("2", "ping", None)).as_bytes());
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// The warning about an ignored `MEMPROVE_CLIENT_SESSION` (ADR 0014
/// E7.5.2, Owner condition j) is written once per process, at the first
/// tool call, and never at startup (E7.3: no context is read before a tool
/// call).
#[test]
fn an_ignored_client_session_is_warned_about_once_and_not_at_startup() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    fake_context(&workspace, &config);
    let env = [
        ("MEMPROVE_CONFIG_DIR", config.into_os_string()),
        ("CLAUDE_PROJECT_DIR", workspace.into_os_string()),
        ("MEMPROVE_CLIENT_SESSION", OsString::from("bad value!")),
    ];
    let warning = "memprove: warning: MEMPROVE_CLIENT_SESSION is not 1 to 128 characters";

    // Started and given no message: nothing on either stream.
    let mcp = Mcp::start(home.path(), &env);
    mcp.quiet(Duration::from_millis(300));
    let (status, rest, stdout, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0));
    assert!(
        rest.is_empty() && stdout.is_empty() && stderr.is_empty(),
        "{stderr}"
    );

    // A handshake, then two tool calls: one warning, after the handshake's
    // log line and before the first tool call's.
    let mut mcp = Mcp::start(home.path(), &env);
    mcp.ask(&modern("1", "server/discover", ""));
    mcp.ask(&modern("2", "tools/call", r#""name":"context""#));
    mcp.ask(&modern("3", "tools/call", r#""name":"context""#));
    let (status, rest, _, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0));
    assert!(rest.is_empty(), "{rest:?}");
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with(warning))
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(lines.len(), 4, "{stderr}");
    assert!(
        lines[0].starts_with("memprove mcp: modern server/discover"),
        "{stderr}"
    );
    assert!(lines[1].starts_with(warning), "{stderr}");
    assert!(
        lines[2].starts_with("memprove mcp: modern tools/call context"),
        "{stderr}"
    );
}

/// U1 (C3, original #22): when whatever reads the server's standard error
/// is gone, a write to it fails with a broken pipe. The server ignores
/// that: every log line and warning is best effort, and the requests are
/// still answered. The read end is closed before the first message; the
/// ignored client session makes the server warn as well as log.
#[test]
fn a_standard_error_nobody_reads_does_not_stop_the_server() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    fake_context(&workspace, &config);
    let env = [
        ("MEMPROVE_CONFIG_DIR", config.into_os_string()),
        ("CLAUDE_PROJECT_DIR", workspace.into_os_string()),
        ("MEMPROVE_CLIENT_SESSION", OsString::from("bad value!")),
    ];
    let mut mcp = Mcp::spawn(home.path(), &env, false);
    let discovered = mcp.ask(&modern("1", "server/discover", ""));
    assert!(discovered.contains(r#""id":1,"result""#), "{discovered}");
    for id in ["2", "3"] {
        let answer = mcp.ask(&modern(id, "tools/call", r#""name":"context""#));
        assert!(
            answer.contains(&format!(r#""id":{id},"result""#))
                && answer.contains(r#""isError":false"#),
            "{answer}"
        );
    }
    let (status, rest, _, _) = mcp.finish();
    assert_eq!(status.code(), Some(0));
    assert!(rest.is_empty(), "{rest:?}");
}

/// E7.11.1 item 8: the help of `mcp`, `mcp serve`, and `mcp print-config`
/// without a daemon, and usage errors (exit 2, nothing on standard output)
/// for an option or an argument either command does not take.
#[test]
fn mcp_help_and_usage_errors() {
    let home = tempfile::tempdir().unwrap();
    for (words, heading) in [
        (&["mcp", "--help"][..], "memprove mcp serve:"),
        (&["mcp", "serve", "--help"][..], "memprove mcp serve:"),
        (&["mcp", "serve", "-h"][..], "memprove mcp serve:"),
        (
            &["mcp", "print-config", "--help"][..],
            "memprove mcp print-config:",
        ),
        (
            &["mcp", "print-config", "-h"][..],
            "memprove mcp print-config:",
        ),
    ] {
        let output = command(home.path()).args(words).output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(0), "{words:?}");
        assert!(output.stderr.is_empty(), "{words:?}");
        assert!(text.starts_with(heading), "{text}");
        assert!(text.contains("\nExit status:\n"), "{text}");
        assert!(!text.contains("Global options:"), "{text}");
    }
    // The group's help holds both commands, in the general usage's order.
    let group = command(home.path())
        .args(["mcp", "--help"])
        .output()
        .unwrap();
    let group = String::from_utf8(group.stdout).unwrap();
    let serve = group.find("memprove mcp serve:").unwrap();
    let print = group
        .find("memprove mcp print-config:")
        .unwrap_or_else(|| panic!("{group}"));
    assert!(serve < print, "{group}");
    for (words, hint) in [
        (&["mcp", "serve", "--data-dir", "x"][..], "mcp serve"),
        (&["--token-file", "x", "mcp", "serve"][..], "mcp serve"),
        (&["mcp", "serve", "extra"][..], "mcp serve"),
        (
            &["mcp", "print-config", "--data-dir", "x"][..],
            "mcp print-config",
        ),
        (
            &["--socket", "x", "mcp", "print-config"][..],
            "mcp print-config",
        ),
        (
            &["mcp", "print-config", "--token-file", "x"][..],
            "mcp print-config",
        ),
        (&["mcp", "print-config", "extra"][..], "mcp print-config"),
        (&["mcp", "print-config", "--", "x"][..], "mcp print-config"),
        (&["mcp"][..], "mcp"),
        (&["mcp", "bogus"][..], "mcp"),
    ] {
        let output = command(home.path()).args(words).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{words:?}");
        assert!(output.stdout.is_empty(), "{words:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.starts_with("memprove: error: "), "{stderr}");
        assert!(
            stderr
                .lines()
                .last()
                .is_some_and(|line| line.contains(&format!("`memprove {hint} --help`"))),
            "{words:?}: {stderr}"
        );
    }
    // `mcp` alone names both commands.
    let output = command(home.path()).arg("mcp").output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("memprove: error: mcp takes serve or print-config"),
        "{stderr}"
    );
    // Nothing was written in the home directory.
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

/// Every file under `root` and its bytes, and every directory, so that a
/// later call can tell whether anything changed.
fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.insert(path.clone(), None);
                pending.push(path);
            } else {
                found.insert(path.clone(), Some(fs::read(&path).unwrap()));
            }
        }
    }
    found
}

/// `path` as one shell word: as it is when it holds only characters a
/// shell takes literally, else in single quotes.
fn shell_word(path: &str) -> String {
    if path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/._+-@%:,=".contains(&byte))
    {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// E7.11.1 item 8 (ADR 0014 E7.9, Owner condition y): `mcp print-config`
/// prints the `.mcp.json` fragment, exactly, with the canonical path of the
/// binary, `args` `mcp serve`, no `env` (U5 P3: the configuration
/// directory's; U5 Z-F round 1: after the settings part), and no token;
/// standard error holds the three statements; no file under the Workspace or the configuration
/// directory changes, and no `.mcp.json` is created.
#[test]
fn print_config_prints_the_fragment_and_writes_nothing() {
    const SENTINEL: &str = "cty_PRINTCONFIGSENTINEL0123456789abcdef";
    let home = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(home.path()).unwrap();
    let project = base.join("project");
    fs::create_dir_all(project.join("src/deep")).unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&project)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    let config = base.join("config");
    fs::create_dir_all(&config).unwrap();
    let token = config.join("token");
    fs::write(&token, format!("{SENTINEL}\n")).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        config.join("data-dir"),
        format!("{}\n", base.join("data").display()),
    )
    .unwrap();
    let before = tree(&base);

    let output = command(&base)
        .current_dir(project.join("src/deep"))
        .env("MEMPROVE_CONFIG_DIR", &config)
        .env("MEMPROVE_TOKEN", SENTINEL)
        .env("MEMPROVE_TOKEN_FILE", &token)
        .args(["mcp", "print-config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");

    let executable = fs::canonicalize(env!("CARGO_BIN_EXE_memprove")).unwrap();
    let executable = executable.to_str().unwrap();
    // U5 P3 (Owner O5): the whole settings fragment; before, the
    // `.mcp.json` entry alone (`{"mcpServers": {"memprove": {"type":
    // "stdio", "command": …, "args": ["mcp", "serve"]}}}`, no env). U5 Z-F
    // round 1 (S1): two parts, each under its `==> <file> <==` line: the
    // settings part (hooks, env, permissions) and the `.mcp.json` part
    // (mcpServers); before, one object with all four.
    let (settings_part, mcp_part) = stdout
        .strip_prefix("==> .claude/settings.local.json <==\n")
        .and_then(|rest| rest.split_once("\n==> .mcp.json <==\n"))
        .unwrap_or_else(|| panic!("not the two parts: {stdout}"));
    assert_eq!(names(&json(settings_part)), ["hooks", "env", "permissions"]);
    let fragment = json(mcp_part);
    assert_eq!(names(&fragment), ["mcpServers"]);
    assert_eq!(names(member(&fragment, &["mcpServers"])), ["memprove"]);
    let entry = member(&fragment, &["mcpServers", "memprove"]);
    assert_eq!(names(entry), ["type", "command", "args", "env"]);
    assert_eq!(member(entry, &["command"]).as_str(), Some(executable));
    assert_eq!(
        member(entry, &["env", "MEMPROVE_CONFIG_DIR"]).as_str(),
        config.to_str()
    );
    let args: Vec<&str> = member(entry, &["args"])
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    // U7 P2 (Owner O4): the directory as an argument too.
    assert_eq!(
        args,
        ["mcp", "serve", "--config-dir", config.to_str().unwrap()]
    );
    assert!(!stdout.contains(SENTINEL));

    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines,
        [
            // U5 Z-F round 1 (S1): the two statements name the two files.
            format!(
                "memprove: the part under .claude/settings.local.json belongs in \
                 {0}/.claude/settings.local.json and the part under .mcp.json in {0}/.mcp.json, \
                 the file Claude Code reads the project's MCP servers from (it does not read \
                 mcpServers from a settings file); `memprove hook install` writes the hooks \
                 and env members into the first, its permissions.allow too with --allow-tools, \
                 and the mcpServers member into the second; nothing was written",
                project.display()
            ),
            format!(
                "memprove: the .mcp.json part is also a file for claude --mcp-config, and its \
                 entry also fits ~/.claude.json (local or user scope); the same entry from the \
                 command line: claude mcp add --transport stdio --scope project --env {} \
                 memprove -- {} mcp serve --config-dir {}",
                shell_word(&format!("MEMPROVE_CONFIG_DIR={}", config.display())),
                shell_word(executable),
                // U7 P2 (Owner O4): the directory as an argument too.
                shell_word(config.to_str().unwrap())
            ),
            // U5 Z-F round 1 (S2): "the hook, the server, and the CLI" (before:
            // "the hook and the server").
            format!(
                "memprove: env.MEMPROVE_CONFIG_DIR is the configuration directory this \
                 command uses ({}, from MEMPROVE_CONFIG_DIR); the hook, the server, and the \
                 CLI read the data directory and the token through <config>/data-dir and \
                 <config>/token unless MEMPROVE_DATA_DIR and MEMPROVE_TOKEN_FILE are set; permissions.allow \
                 lets an unattended claude -p use the memprove tools and `memprove run`; a \
                 token never goes into these files",
                config.display()
            ),
        ]
    );
    // No token, not in either stream.
    assert!(!stdout.contains(SENTINEL) && !stderr.contains(SENTINEL));
    // Nothing was written: no file under the Workspace or the configuration
    // directory changed, and no `.mcp.json` exists.
    assert_eq!(tree(&base), before);
    assert!(!project.join(".mcp.json").exists());
}

/// Sentence-initial words that would make a sentence an instruction.
const IMPERATIVES: [&str; 18] = [
    "Use", "Call", "Run", "Do", "Don't", "Please", "Pass", "Make", "Ensure", "Avoid", "Remember",
    "Try", "Never", "Always", "Set", "Provide", "Give", "Send",
];

/// Whether `text` is statements only (ADR 0014 E7.10): no question or
/// exclamation mark, and no sentence starting with a bare imperative.
fn statements_only(text: &str) -> Result<(), String> {
    if text.contains(['?', '？', '!', '！']) {
        return Err(format!("a question or exclamation mark: {text}"));
    }
    for sentence in text.split(". ") {
        let first = sentence.split_whitespace().next().unwrap_or_default();
        if IMPERATIVES.contains(&first) {
            return Err(format!("an imperative: {sentence}"));
        }
    }
    Ok(())
}

#[test]
fn descriptions_and_instructions_are_statements_and_names_are_valid() {
    let home = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::start(home.path(), &[]);
    let discover = json(&mcp.ask(&modern("1", "server/discover", "")));
    let initialize = json(&mcp.ask(&initialize("2", "2025-11-25")));
    let list = json(&mcp.ask(&modern("3", "tools/list", "")));
    let instructions = member(&discover, &["result", "instructions"])
        .as_str()
        .unwrap();
    assert_eq!(
        member(&initialize, &["result", "instructions"]).as_str(),
        Some(instructions)
    );
    statements_only(instructions).unwrap();
    for tool in member(&list, &["result", "tools"]).as_array().unwrap() {
        let name = member(tool, &["name"]).as_str().unwrap();
        assert!(
            (1..=128).contains(&name.len())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte)),
            "{name}"
        );
        statements_only(member(tool, &["description"]).as_str().unwrap())
            .unwrap_or_else(|why| panic!("{name}: {why}"));
        assert!(tool.get("title").is_none(), "{name}");
        // No parameter is a token, a Project, or a client session.
        let properties = property_names(member(tool, &["inputSchema"]));
        for forbidden in ["token", "project_id", "client_session"] {
            assert!(!properties.iter().any(|name| name == forbidden), "{name}");
        }
    }
    assert!(statements_only("Run the tests.").is_err());
    assert!(statements_only("Is it done?").is_err());
    let (status, _, _, _) = mcp.finish();
    assert_eq!(status.code(), Some(0));
}

/// U6 K3 (U5 N4): a `pack` query that holds the whole token the server
/// itself holds is refused as a query in the token format is (U5 C4, the
/// fixed `invalid_arguments` result naming `query`), before anything is sent
/// and without repeating the query or the token; nothing is written. The
/// token has no upper-case letter, so only the new check can see it. Any
/// other query goes on to the daemon (here a made-up socket).
#[test]
fn u6_k3_a_pack_query_with_the_servers_own_token_is_refused_before_anything_is_sent() {
    const TOKEN: &str = "cty_u6k3-held-token-0123456789-abcdefghijkl";
    const SHAPED: &str = "Ab3_cDe-4fGh5iJk6lMn7oPq8rSt9uVw0xYz1AbCd2E";
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let data = home.path().join("data");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    fake_context(&workspace, &config);
    std::fs::write(config.join("data-dir"), format!("{}\n", data.display())).unwrap();
    let token = config.join("token");
    std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    // A file where the socket would be: the server finds the daemon's
    // socket, so it can hold its token; a request sent to it would fail.
    std::fs::write(data.join("memproved.sock"), b"").unwrap();
    let env = [
        ("MEMPROVE_CONFIG_DIR", config.clone().into_os_string()),
        ("CLAUDE_PROJECT_DIR", workspace.clone().into_os_string()),
    ];
    let call = |query: &str| {
        modern(
            "1",
            "tools/call",
            &format!(r#""name":"pack","arguments":{{"query":"{query}"}}"#),
        )
    };
    let before = tree(home.path());

    let mut mcp = Mcp::start(home.path(), &env);
    let shaped = mcp.ask(&call(SHAPED));
    assert!(
        shaped.contains(r#""failure":"invalid_arguments","parameter":"query""#)
            && shaped.contains(r#""isError":true"#),
        "{shaped}"
    );
    for query in [
        TOKEN.to_owned(),
        format!("{TOKEN} and more words"),
        format!("words in front {TOKEN} and behind"),
        format!("ends with the token {TOKEN}"),
    ] {
        let answer = mcp.ask(&call(&query));
        assert_eq!(answer, shaped, "{query}");
        assert!(!answer.contains(TOKEN));
    }
    for query in ["a harmless query", &TOKEN[..20], &TOKEN[4..]] {
        let answer = mcp.ask(&call(query));
        assert!(
            answer.contains(r#""isError":true"#)
                && !answer.contains(r#""failure":"invalid_arguments""#),
            "{answer}"
        );
    }
    let (status, rest, _, stderr) = mcp.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(rest.is_empty(), "{rest:?}");
    assert!(!stderr.contains(TOKEN), "{stderr}");
    assert_eq!(tree(home.path()), before, "the server wrote nothing");
}
