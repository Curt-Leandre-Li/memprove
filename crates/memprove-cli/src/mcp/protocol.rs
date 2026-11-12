//! The message layer (ADR 0014 E7.3, E7.4, E7.7): framing, the per-request
//! classification of both eras, the handshakes, `tools/list`, `tools/call`,
//! `ping`, `subscriptions/listen`, and the JSON-RPC errors. The only process
//! state is whether `initialize` was answered and which subscriptions are
//! open; modern requests never consult the first (M13, M18).

use std::{
    io::{self, BufRead, Write},
    panic::{self, AssertUnwindSafe},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};

use super::{
    Environment,
    failure::ToolResult,
    local::{Ignored, Local},
    tools::{self, INSTRUCTIONS},
    wire::{Line, compact, number, object, read_line, text, texts},
};

/// The modern revision (2026-07-28), served statelessly per request.
pub(super) const MODERN: &str = "2026-07-28";
/// The legacy revisions `initialize` accepts, the latest first.
pub(super) const LEGACY: [&str; 2] = ["2025-11-25", "2025-06-18"];
/// The `_meta` key that makes a request modern.
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
/// The `_meta` key every modern request carries.
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
/// The `_meta` key of the server's identity in modern results.
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";
/// The `_meta` key naming a subscription.
const SUBSCRIPTION_KEY: &str = "io.modelcontextprotocol/subscriptionId";
/// The longest line read, in bytes (the CLI's `MAX_ENVELOPE_BYTES`).
pub(super) const MAX_LINE_BYTES: usize = 1 << 20;
/// `ttlMs` of `server/discover` and `tools/list` (M30).
const TTL_MS: u64 = 3_600_000;

/// U1 (A1, Owner O2): the one fixed line written, once per process, when
/// `CLAUDE_CODE_SESSION_ID` is set to a value the server ignores.
pub(super) const IGNORED_CLAUDE_CODE_SESSION: &str = "memprove: warning: \
    CLAUDE_CODE_SESSION_ID is not 1 to 128 characters of A-Z, a-z, 0-9, ., _, :, and -; it is \
    ignored, and the context of this directory is used";

/// The methods the log names; any other is logged as `unknown`, so no
/// client text reaches standard error.
const KNOWN_METHODS: [&str; 9] = [
    "initialize",
    "ping",
    "tools/list",
    "tools/call",
    "server/discover",
    "subscriptions/listen",
    "notifications/initialized",
    "notifications/cancelled",
    "notifications/roots/list_changed",
];

/// The server: two protocol items and the environment it reads per call.
pub struct Server<'a> {
    environment: &'a dyn Fn() -> Environment,
    /// Set by `initialize`, never cleared (E7.4.1).
    legacy_initialized: bool,
    /// The ids of the open `subscriptions/listen` requests.
    subscriptions: Vec<JsonValue>,
    /// What the process keeps about its client session.
    client: ClientSessionState,
}

/// What a server process keeps between calls about its client session.
#[derive(Debug, Default)]
struct ClientSessionState {
    /// Whether the warning about an ignored `MEMPROVE_CLIENT_SESSION` was
    /// written (once per process).
    warned: bool,
    /// U1 (A1): whether the warning about an ignored
    /// `CLAUDE_CODE_SESSION_ID` was written (once per process).
    warned_claude_code: bool,
    /// U1 (U-F, SF-1): whether a call used the client session from
    /// `CLAUDE_CODE_SESSION_ID`; from then on every call does.
    claude_code_adopted: bool,
}

/// What handling one message produced, and its log line's parts.
struct Handled {
    messages: Vec<String>,
    era: &'static str,
    tool: Option<&'static str>,
    outcome: String,
}

impl Handled {
    fn new(era: &'static str, messages: Vec<String>, outcome: impl Into<String>) -> Self {
        Self {
            messages,
            era,
            tool: None,
            outcome: outcome.into(),
        }
    }
}

/// Writes the log line of `handled` on standard error (E7.3): the era, the
/// method (a known name or `unknown`), the tool, and the outcome; never an
/// argument value. Returns the messages.
fn log(handled: Handled, method: &str) -> Vec<String> {
    let tool = handled
        .tool
        .map(|tool| format!(" {tool}"))
        .unwrap_or_default();
    crate::log_line(format_args!(
        "memprove mcp: {} {method}{tool} -> {}",
        handled.era, handled.outcome
    ));
    handled.messages
}

/// A JSON-RPC success.
fn response(id: &JsonValue, result: JsonValue) -> String {
    compact(&object([
        ("jsonrpc", text("2.0")),
        ("id", id.clone()),
        ("result", result),
    ]))
}

/// A JSON-RPC error.
fn error(id: &JsonValue, code: i64, message: &str, data: Option<JsonValue>) -> String {
    let mut body = vec![("code", number(code)), ("message", text(message))];
    if let Some(data) = data {
        body.push(("data", data));
    }
    compact(&object([
        ("jsonrpc", text("2.0")),
        ("id", id.clone()),
        ("error", object(body)),
    ]))
}

/// The server's identity.
fn server_info() -> JsonValue {
    object([
        ("name", text("memprove")),
        ("title", text("memprove")),
        ("version", text(env!("CARGO_PKG_VERSION"))),
    ])
}

/// `_meta` of a modern result.
fn server_meta() -> JsonValue {
    object([(SERVER_INFO_KEY, server_info())])
}

/// `data` of a legacy-shaped request that only the modern era or
/// `initialize` can serve.
fn both_ways() -> JsonValue {
    object([
        ("missing", texts([VERSION_KEY])),
        ("supported", texts([MODERN])),
        ("initialize", texts(LEGACY)),
    ])
}

/// Whether `id` is a valid request id: a string or an integer (M8: never
/// `null`).
fn valid_id(id: &JsonValue) -> bool {
    match id {
        JsonValue::String(_) => true,
        JsonValue::Number(lexical) => !lexical.contains(['.', 'e', 'E']),
        _ => false,
    }
}

fn known(method: &str) -> &'static str {
    KNOWN_METHODS
        .into_iter()
        .find(|known| *known == method)
        .unwrap_or("unknown")
}

impl<'a> Server<'a> {
    /// A server that reads `environment` once per tool call.
    pub fn new(environment: &'a dyn Fn() -> Environment) -> Self {
        Self {
            environment,
            legacy_initialized: false,
            subscriptions: Vec::new(),
            client: ClientSessionState::default(),
        }
    }

    /// Handles one line of standard input; returns the lines to write.
    pub(super) fn handle(&mut self, line: Line) -> Vec<String> {
        let Line::Text(bytes) = line else {
            return log(Handled::new("-", vec![parse_error()], "-32700"), "-");
        };
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(&bytes);
        if bytes
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
        {
            return Vec::new();
        }
        let parsed = std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| parse_strict(text).ok());
        match parsed {
            Some(value) => self.message(&value),
            None => log(Handled::new("-", vec![parse_error()], "-32700"), "-"),
        }
    }

    /// One parsed message (E7.4.1 steps 2 to 6).
    fn message(&mut self, value: &JsonValue) -> Vec<String> {
        let invalid = |id: JsonValue| {
            Handled::new(
                "-",
                vec![error(&id, -32600, "Invalid Request", None)],
                "-32600",
            )
        };
        if value.as_object().is_none() {
            return log(invalid(JsonValue::Null), "-");
        }
        let id = value.get("id");
        let echo = id
            .filter(|id| valid_id(id))
            .cloned()
            .unwrap_or(JsonValue::Null);
        let method = value.get("method");
        let method_name = method.and_then(JsonValue::as_str);
        let malformed = value.get("jsonrpc").and_then(JsonValue::as_str) != Some("2.0")
            || (method.is_some() && method_name.is_none())
            || id.is_some_and(|id| !valid_id(id));
        if malformed {
            return log(invalid(echo), method_name.map_or("-", known));
        }
        let params = value.get("params");
        match (method_name, id) {
            (Some(method), None) => {
                let handled = self.notification(method, params);
                log(handled, known(method))
            }
            (None, _) if value.get("result").is_some() || value.get("error").is_some() => {
                log(Handled::new("-", Vec::new(), "ignored (a response)"), "-")
            }
            (None, _) => log(invalid(echo), "-"),
            (Some(method), Some(id)) => {
                let handled = if params.is_some_and(|params| params.as_object().is_none()) {
                    Handled::new(
                        "-",
                        vec![error(id, -32602, "Invalid params", None)],
                        "-32602",
                    )
                } else {
                    // A panic is answered `-32603` and the loop goes on
                    // (E7.3); the process never exits on one message.
                    panic::catch_unwind(AssertUnwindSafe(|| self.request(method, id, params)))
                        .unwrap_or_else(|_| {
                            Handled::new(
                                "-",
                                vec![error(id, -32603, "Internal error", None)],
                                "-32603 (panic)",
                            )
                        })
                };
                log(handled, known(method))
            }
        }
    }

    /// A notification: never answered; a `notifications/cancelled` that
    /// names an open subscription closes it with its completion result
    /// (E7.4.6).
    fn notification(&mut self, method: &str, params: Option<&JsonValue>) -> Handled {
        let mut messages = Vec::new();
        if method == "notifications/cancelled"
            && let Some(request) = params.and_then(|params| params.get("requestId"))
        {
            let named = compact(request);
            if let Some(index) = self
                .subscriptions
                .iter()
                .position(|open| compact(open) == named)
            {
                let id = self.subscriptions.remove(index);
                messages.push(completion(&id));
            }
        }
        Handled::new("-", messages, "ok")
    }

    /// The completion results of every open subscription, at the end of
    /// input (M28, graceful closure).
    pub(super) fn close(&mut self) -> Vec<String> {
        self.subscriptions
            .drain(..)
            .map(|id| completion(&id))
            .collect()
    }

    /// A request (E7.4.1 steps 7 to 9).
    fn request(&mut self, method: &str, id: &JsonValue, params: Option<&JsonValue>) -> Handled {
        #[cfg(test)]
        assert!(method != "test/panic", "injected");
        if method == "initialize" {
            return self.initialize(id, params);
        }
        let meta = params
            .and_then(|params| params.get("_meta"))
            .filter(|meta| meta.as_object().is_some());
        match meta.and_then(|meta| meta.get(VERSION_KEY).map(|version| (meta, version))) {
            Some((meta, version)) => self.modern(method, id, params, meta, version),
            None => self.legacy(method, id, params),
        }
    }

    /// `initialize` (E7.4.2): the legacy handshake, whatever `_meta` says.
    fn initialize(&mut self, id: &JsonValue, params: Option<&JsonValue>) -> Handled {
        let Some(requested) = params
            .and_then(|params| params.get("protocolVersion"))
            .and_then(JsonValue::as_str)
        else {
            let data = object([
                ("missing", texts(["protocolVersion"])),
                ("supported", texts(LEGACY)),
            ]);
            return Handled::new(
                "legacy",
                vec![error(id, -32602, "Invalid params", Some(data))],
                "-32602",
            );
        };
        let version = LEGACY
            .into_iter()
            .find(|legacy| *legacy == requested)
            .unwrap_or(LEGACY[0]);
        self.legacy_initialized = true;
        let result = object([
            ("protocolVersion", text(version)),
            ("capabilities", object([("tools", object([]))])),
            ("serverInfo", server_info()),
            ("instructions", text(INSTRUCTIONS)),
        ]);
        Handled::new("legacy", vec![response(id, result)], "ok")
    }

    /// A modern request (E7.4.1 step 8).
    fn modern(
        &mut self,
        method: &str,
        id: &JsonValue,
        params: Option<&JsonValue>,
        meta: &JsonValue,
        version: &JsonValue,
    ) -> Handled {
        let reply = |messages, outcome: &str| Handled::new("modern", messages, outcome);
        let Some(version) = version.as_str() else {
            let data = object([("missing", texts([VERSION_KEY]))]);
            return reply(
                vec![error(id, -32602, "Invalid params", Some(data))],
                "-32602",
            );
        };
        if version != MODERN {
            let data = object([("supported", texts([MODERN])), ("requested", text(version))]);
            return reply(
                vec![error(
                    id,
                    -32022,
                    "Unsupported protocol version",
                    Some(data),
                )],
                "-32022",
            );
        }
        if meta
            .get(CAPABILITIES_KEY)
            .and_then(JsonValue::as_object)
            .is_none()
        {
            let data = object([("missing", texts([CAPABILITIES_KEY]))]);
            return reply(
                vec![error(id, -32602, "Invalid params", Some(data))],
                "-32602",
            );
        }
        let complete = |members: Vec<(&'static str, JsonValue)>| {
            let mut all = vec![("resultType", text("complete"))];
            all.extend(members);
            all.push(("_meta", server_meta()));
            object(all)
        };
        match method {
            "server/discover" => {
                let result = complete(vec![
                    ("supportedVersions", texts([MODERN])),
                    ("capabilities", object([("tools", object([]))])),
                    ("instructions", text(INSTRUCTIONS)),
                    ("ttlMs", number(TTL_MS)),
                    ("cacheScope", text("public")),
                ]);
                reply(vec![response(id, result)], "ok")
            }
            "tools/list" if params.is_some_and(|params| params.get("cursor").is_some()) => {
                reply(vec![error(id, -32602, "Invalid params", None)], "-32602")
            }
            "tools/list" => {
                let result = complete(vec![
                    ("tools", tools::list().clone()),
                    ("ttlMs", number(TTL_MS)),
                    ("cacheScope", text("public")),
                ]);
                reply(vec![response(id, result)], "ok")
            }
            "tools/call" => self.tools_call(true, id, params),
            "ping" => reply(vec![response(id, complete(Vec::new()))], "ok"),
            "subscriptions/listen" => {
                let acknowledged = compact(&object([
                    ("jsonrpc", text("2.0")),
                    ("method", text("notifications/subscriptions/acknowledged")),
                    (
                        "params",
                        object([
                            ("_meta", object([(SUBSCRIPTION_KEY, id.clone())])),
                            ("notifications", object([])),
                        ]),
                    ),
                ]));
                let named = compact(id);
                if !self.subscriptions.iter().any(|open| compact(open) == named) {
                    self.subscriptions.push(id.clone());
                }
                reply(vec![acknowledged], "ok (subscription open)")
            }
            _ => reply(vec![error(id, -32601, "Method not found", None)], "-32601"),
        }
    }

    /// A request without modern `_meta` (E7.4.1 step 9).
    fn legacy(&mut self, method: &str, id: &JsonValue, params: Option<&JsonValue>) -> Handled {
        let reply = |messages, outcome: &str| Handled::new("legacy", messages, outcome);
        let both = || {
            reply(
                vec![error(id, -32602, "Invalid params", Some(both_ways()))],
                "-32602",
            )
        };
        match method {
            "ping" => reply(vec![response(id, object([]))], "ok"),
            "tools/list" | "tools/call" if !self.legacy_initialized => both(),
            "server/discover" | "subscriptions/listen" => both(),
            "tools/list" if params.is_some_and(|params| params.get("cursor").is_some()) => {
                reply(vec![error(id, -32602, "Invalid params", None)], "-32602")
            }
            "tools/list" => {
                let result = object([("tools", tools::list().clone())]);
                reply(vec![response(id, result)], "ok")
            }
            "tools/call" => self.tools_call(false, id, params),
            _ => reply(vec![error(id, -32601, "Method not found", None)], "-32601"),
        }
    }

    /// `tools/call` in either era (E7.4.4).
    fn tools_call(&mut self, modern: bool, id: &JsonValue, params: Option<&JsonValue>) -> Handled {
        let era = if modern { "modern" } else { "legacy" };
        let invalid =
            |message: &str| Handled::new(era, vec![error(id, -32602, message, None)], "-32602");
        let Some(name) = params
            .and_then(|params| params.get("name"))
            .and_then(JsonValue::as_str)
        else {
            return invalid("Invalid params");
        };
        let arguments = params.and_then(|params| params.get("arguments"));
        if arguments.is_some_and(|arguments| arguments.as_object().is_none()) {
            return invalid("Invalid params");
        }
        let Some(tool) = tools::find(name) else {
            return invalid("Unknown tool");
        };
        let ClientSessionState {
            warned,
            warned_claude_code,
            claude_code_adopted,
        } = &mut self.client;
        let local = Local::resolve((self.environment)(), claude_code_adopted, &mut |ignored| {
            match ignored {
                Ignored::ClientSession if !*warned => {
                    *warned = true;
                    crate::log_line(format_args!(
                        "memprove: warning: MEMPROVE_CLIENT_SESSION is not 1 to 128 characters of \
                     A-Z, a-z, 0-9, _, and -; it is ignored, and the context of this directory is \
                     used"
                    ));
                }
                Ignored::ClaudeCodeSession if !*warned_claude_code => {
                    *warned_claude_code = true;
                    crate::log_line(format_args!("{IGNORED_CLAUDE_CODE_SESSION}"));
                }
                _ => {}
            }
        });
        let mut handled = match tools::call(tool, arguments, &local) {
            Ok(result) => {
                let outcome = result.outcome;
                Handled::new(
                    era,
                    vec![response(id, tool_result(modern, result))],
                    outcome,
                )
            }
            Err(_) => Handled::new(
                era,
                vec![error(id, -32603, "Internal error", None)],
                "-32603",
            ),
        };
        handled.tool = Some(tool.name);
        handled
    }
}

/// The `result` of a `tools/call` (E7.5.3): a modern result adds
/// `resultType` first and `_meta` last, also on `isError: true` (M25).
/// T21 (V6-F1): the result of `pack` and `claims` has two text blocks, the
/// Pack's bytes and the compact object, and no `structuredContent`. U12 T2
/// (Owner O4): a `pack` result without `query` has three, its opening first.
fn tool_result(modern: bool, result: ToolResult) -> JsonValue {
    let mut members = Vec::new();
    if modern {
        members.push(("resultType", text("complete")));
    }
    let block =
        |value: String| object([("type", text("text")), ("text", JsonValue::String(value))]);
    // U12 T2 (Owner O4): a whole Pack of `pack` comes after its opening.
    let mut content: Vec<JsonValue> = result.opening.into_iter().map(block).collect();
    content.push(block(result.text));
    if result.structured_as_text {
        content.push(block(compact(&result.structured)));
    }
    members.push(("content", JsonValue::Array(content)));
    if !result.structured_as_text {
        members.push(("structuredContent", result.structured));
    }
    members.push(("isError", JsonValue::Bool(result.is_error)));
    if modern {
        members.push(("_meta", server_meta()));
    }
    object(members)
}

/// `-32700` for a line the parser refuses.
fn parse_error() -> String {
    error(&JsonValue::Null, -32700, "Parse error", None)
}

/// The completion result of the subscription `id` (E7.4.6).
fn completion(id: &JsonValue) -> String {
    response(
        id,
        object([
            ("resultType", text("complete")),
            (
                "_meta",
                object([
                    (SUBSCRIPTION_KEY, id.clone()),
                    (SERVER_INFO_KEY, server_info()),
                ]),
            ),
        ]),
    )
}

/// Writes one message and its line feed with one `write_all`, then flushes.
fn send(output: &mut dyn Write, message: &str) -> io::Result<()> {
    let mut line = Vec::with_capacity(message.len() + 1);
    line.extend_from_slice(message.as_bytes());
    line.push(b'\n');
    output.write_all(&line)?;
    output.flush()
}

/// Serves `input` until its end, one message at a time, strictly in order
/// (E7.3): each line is read, handled, and answered before the next is
/// read. At the end of input every open subscription gets its completion
/// result. A failed write (the client went away) ends the loop silently.
pub fn serve(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    environment: &dyn Fn() -> Environment,
) {
    let mut server = Server::new(environment);
    while let Ok(Some(line)) = read_line(input, MAX_LINE_BYTES) {
        for message in server.handle(line) {
            if send(output, &message).is_err() {
                return;
            }
        }
    }
    for message in server.close() {
        if send(output, &message).is_err() {
            return;
        }
    }
}

/// `memprove mcp serve` on this process's standard streams. Nothing is
/// done at startup; every tool call reads the environment anew. A panic
/// prints nothing of its own: the message is answered `-32603` and logged
/// in one line.
pub fn run_stdio() {
    run_stdio_with(None);
}

/// [`run_stdio`] with (U7 P2, D2 F13, Owner O4) `--config-dir <DIR>`: every
/// tool call takes `config_dir`, when given, as the configuration
/// directory, whatever `MEMPROVE_CONFIG_DIR` says.
pub fn run_stdio_with(config_dir: Option<std::ffi::OsString>) {
    panic::set_hook(Box::new(|_| {}));
    let stdin = io::stdin();
    let stdout = io::stdout();
    let environment = move || {
        let mut environment = Environment::from_process();
        environment.process = environment.process.with_config_dir(config_dir.clone());
        environment
    };
    serve(&mut stdin.lock(), &mut stdout.lock(), &environment);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern_meta() -> &'static str {
        r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}"#
    }

    /// Runs `lines` through a server with an empty environment; returns
    /// what it wrote.
    fn run(lines: &[&str]) -> Vec<String> {
        let input = lines.join("\n");
        let mut output = Vec::new();
        serve(&mut input.as_bytes(), &mut output, &Environment::default);
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn requests_are_classified_one_by_one() {
        let meta = modern_meta();
        let discover = format!(
            r#"{{"jsonrpc":"2.0","id":"discover-1","method":"server/discover","params":{{{meta}}}}}"#
        );
        let out = run(&[
            &discover,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2025-11-25","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":7}}}"#,
            r#"{"jsonrpc":"2.0","id":7,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
            r#"{"jsonrpc":"2.0","id":12345678901234567890,"method":"resources/list"}"#,
        ]);
        assert_eq!(out.len(), 8, "{out:#?}");
        assert!(out[0].starts_with(r#"{"jsonrpc":"2.0","id":"discover-1","result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"instructions":"#), "{}", out[0]);
        assert!(out[0].ends_with(r#""ttlMs":3600000,"cacheScope":"public","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"memprove","title":"memprove","version":"0.1.0"}}}}"#), "{}", out[0]);
        assert_eq!(
            out[1],
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"Invalid params","data":{"missing":["io.modelcontextprotocol/protocolVersion"],"supported":["2026-07-28"],"initialize":["2025-11-25","2025-06-18"]}}}"#
        );
        assert!(out[2].contains(r#""result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"memprove","title":"memprove","version":"0.1.0"},"instructions":"#), "{}", out[2]);
        assert!(!out[2].contains("resultType") && !out[2].contains("_meta"));
        assert!(
            out[3].starts_with(r#"{"jsonrpc":"2.0","id":4,"result":{"tools":[{"name":"pack""#),
            "{}",
            out[3]
        );
        assert_eq!(
            out[4],
            r#"{"jsonrpc":"2.0","id":5,"error":{"code":-32022,"message":"Unsupported protocol version","data":{"supported":["2026-07-28"],"requested":"2025-11-25"}}}"#
        );
        assert_eq!(
            out[5],
            r#"{"jsonrpc":"2.0","id":6,"error":{"code":-32602,"message":"Invalid params","data":{"missing":["io.modelcontextprotocol/protocolVersion"]}}}"#
        );
        assert_eq!(
            out[6],
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32602,"message":"Invalid params","data":{"missing":["io.modelcontextprotocol/clientCapabilities"]}}}"#
        );
        assert_eq!(
            out[7],
            r#"{"jsonrpc":"2.0","id":12345678901234567890,"error":{"code":-32601,"message":"Method not found"}}"#
        );
    }

    #[test]
    fn malformed_messages_get_the_fixed_errors_and_the_loop_goes_on() {
        let nested = format!("{}{}", "[".repeat(129), "]".repeat(129));
        let out = run(&[
            "not json",
            r#"{"a":1,"a":2}"#,
            &nested,
            "[]",
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            r#""x""#,
            "null",
            "{}",
            r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":5}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":9,"result":{}}"#,
            "   ",
            "",
            r#"{"jsonrpc":"2.0","id":10,"method":"ping","params":[]}"#,
            "{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"ping\"}\r",
        ]);
        let parse =
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#;
        let invalid = |id: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32600,"message":"Invalid Request"}}}}"#
            )
        };
        let expected = [
            parse.to_owned(),
            parse.to_owned(),
            parse.to_owned(),
            invalid("null"),
            invalid("null"),
            invalid("null"),
            invalid("null"),
            invalid("null"),
            invalid("1"),
            invalid("1"),
            invalid("null"),
            invalid("null"),
            invalid("null"),
            r#"{"jsonrpc":"2.0","id":10,"error":{"code":-32602,"message":"Invalid params"}}"#
                .to_owned(),
            r#"{"jsonrpc":"2.0","id":11,"result":{}}"#.to_owned(),
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn a_panic_is_answered_and_the_server_keeps_serving() {
        let out = run(&[
            r#"{"jsonrpc":"2.0","id":"p","method":"test/panic"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        ]);
        assert_eq!(
            out,
            [
                r#"{"jsonrpc":"2.0","id":"p","error":{"code":-32603,"message":"Internal error"}}"#,
                r#"{"jsonrpc":"2.0","id":2,"result":{}}"#,
            ]
        );
    }

    #[test]
    fn subscriptions_are_acknowledged_and_closed() {
        let meta = modern_meta();
        let listen = |id: u32| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"subscriptions/listen","params":{{{meta},"notifications":{{"toolsListChanged":true}}}}}}"#
            )
        };
        let out = run(&[
            &listen(7),
            &listen(8),
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/unknown"}"#,
        ]);
        let ack = |id: u32| {
            format!(
                r#"{{"jsonrpc":"2.0","method":"notifications/subscriptions/acknowledged","params":{{"_meta":{{"io.modelcontextprotocol/subscriptionId":{id}}},"notifications":{{}}}}}}"#
            )
        };
        let done = |id: u32| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"result":{{"resultType":"complete","_meta":{{"io.modelcontextprotocol/subscriptionId":{id},"io.modelcontextprotocol/serverInfo":{{"name":"memprove","title":"memprove","version":"0.1.0"}}}}}}}}"#
            )
        };
        assert_eq!(out, [ack(7), ack(8), done(7), done(8)]);
    }

    #[test]
    fn tool_calls_check_the_name_and_the_arguments_first() {
        let meta = modern_meta();
        let call = |id: u32, rest: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{{meta},{rest}}}}}"#
            )
        };
        let out = run(&[
            &call(1, r#""name":"nope-SENTINEL""#),
            &call(2, r#""name":"pack","arguments":[]"#),
            &call(3, r#""name":5"#),
            &call(4, r#""name":"pack","arguments":{"budget":"SENTINEL"}"#),
            &call(
                5,
                r#""name":"checkpoint","arguments":{"SENTINEL":1,"next":{"text":"n"}}"#,
            ),
            &call(6, r#""name":"context""#),
        ]);
        assert_eq!(
            out[0],
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Unknown tool"}}"#
        );
        assert_eq!(
            out[1],
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"Invalid params"}}"#
        );
        assert_eq!(
            out[2],
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Invalid params"}}"#
        );
        assert_eq!(
            out[3],
            r#"{"jsonrpc":"2.0","id":4,"result":{"resultType":"complete","content":[{"type":"text","text":"the argument budget does not match the tool's inputSchema; nothing was sent"}],"structuredContent":{"task_id":null,"session_id":null,"failure":"invalid_arguments","parameter":"budget"},"isError":true,"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"memprove","title":"memprove","version":"0.1.0"}}}}"#
        );
        assert!(
            out[4].contains(r#""failure":"invalid_arguments","parameter":null"#),
            "{}",
            out[4]
        );
        // No configuration directory in the empty environment.
        assert!(
            out[5].contains(r#""failure":"no_config_dir""#),
            "{}",
            out[5]
        );
        assert!(out.iter().all(|line| !line.contains("SENTINEL")));
    }

    /// T21 (V6-F1): a `pack` or `claims` success carries the Pack's bytes
    /// and then its object as two text blocks, with no `structuredContent`,
    /// in both eras (Claude Code 2.1.290 hands the model only
    /// `structuredContent` when there is one); every other result keeps one
    /// text block and `structuredContent`.
    #[test]
    fn a_pack_result_has_two_text_blocks_and_no_structured_content() {
        let ids = super::super::failure::Ids {
            task_id: Some("018f47a0-0000-7000-8000-000000000004".to_owned()),
            session_id: None,
        };
        let pack = super::super::failure::succeeded(
            &ids,
            vec![("budget", number(6000_usize))],
            Some("Task line\nDigest: sha256:00\n".to_owned()),
        );
        assert_eq!(
            compact(&tool_result(false, pack.clone())),
            r#"{"content":[{"type":"text","text":"Task line\u000aDigest: sha256:00\u000a"},{"type":"text","text":"{\"task_id\":\"018f47a0-0000-7000-8000-000000000004\",\"session_id\":null,\"budget\":6000}"}],"isError":false}"#
        );
        let modern = compact(&tool_result(true, pack));
        assert!(
            modern.starts_with(
                r#"{"resultType":"complete","content":[{"type":"text","text":"Task line"#
            ),
            "{modern}"
        );
        assert!(!modern.contains("structuredContent"), "{modern}");
        let other =
            super::super::failure::succeeded(&ids, vec![("budget", number(6000_usize))], None);
        assert_eq!(
            compact(&tool_result(false, other)),
            r#"{"content":[{"type":"text","text":"{\"task_id\":\"018f47a0-0000-7000-8000-000000000004\",\"session_id\":null,\"budget\":6000}"}],"structuredContent":{"task_id":"018f47a0-0000-7000-8000-000000000004","session_id":null,"budget":6000},"isError":false}"#
        );
    }
}
