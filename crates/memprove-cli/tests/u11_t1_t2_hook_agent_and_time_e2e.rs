//! U11 T1, T2 (Owner O3, O4; ADR 0014 "Amendment (U11, S-T)", ADR 0021
//! "Amendment (U11)"), through the real `memprove` and `memproved`:
//!
//! - T2: the opening of the Pack the `SessionStart` hook injects ends with
//!   `The current UTC time is <time>.`, the hook's clock in the canonical form of the
//!   ledger's times (the form of the Pack's `last wrote to the ledger at <time>`),
//!   whatever the environment says; two runs differ in that sentence only.
//! - T1: with `MEMPROVE_AGENT_NAME` and `MEMPROVE_AGENT_ROLE` both set
//!   (`MEMPROVE_PARENT_SESSION` optional), the opening ends with
//!   `You are agent「<name>」(<role>[, parent Session <ID>]); when begin and end get no
//!   agent_name, agent_role, or parent_session, the server uses these three.` (a statement
//!   since U12 T3); with part of
//!   the declaration, or a value the server refuses, it names no agent and
//!   standard error has one line; with none of the three it is the opening
//!   of before plus the time sentence.
//! - Both sentences are outside the Pack: the Pack block, its digest, and
//!   the last-Pack record are the same bytes in every environment, and the
//!   same as `resume --pack` at budget 9000; a statement without a Pack has
//!   neither sentence; `hook print-config` does not change.
//! - The MCP `begin` result names the Session's agent, `agent「<name>」
//!   (<role>)`, or `no agent`.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    path::Path,
    process::{Command, Output, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use memprove_domain::UtcTimestamp;
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{Fixture, cli_binary, isolated, json, text};

const NAME: &str = "MEMPROVE_AGENT_NAME";
const ROLE: &str = "MEMPROVE_AGENT_ROLE";
const PARENT: &str = "MEMPROVE_PARENT_SESSION";
/// The rest of the identity sentence after the parentheses.
/// U12 T3 (Owner O5): a statement, no longer an imperative.
const NO_INPUT: &str = "; when begin and end get no agent_name, agent_role, or parent_session, the server uses these three.";
const FENCE: &str = "\n```text\n";

fn command(fixture: &Fixture, cwd: &Path, env: &[(&str, &str)]) -> Command {
    let mut command = isolated(cli_binary());
    command
        .current_dir(cwd)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &fixture.data);
    for (name, value) in env {
        command.env(name, value);
    }
    command
}

/// `hook session-start` for the client session `client` in `cwd`, with the
/// variables `env`: its output, which exits 0.
fn hook_in(fixture: &Fixture, cwd: &Path, client: &str, env: &[(&str, &str)]) -> Output {
    let mut child = command(fixture, cwd, env)
        .args(["hook", "session-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"{client}","source":"startup","cwd":"{}"}}"#,
        cwd.display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    output
}

/// The opening and the Pack of a response that injects one.
fn split(stdout: &str) -> (&str, &str) {
    let (opening, rest) = stdout
        .split_once(FENCE)
        .unwrap_or_else(|| panic!("no Pack: {stdout}"));
    let pack = rest
        .strip_suffix("```\n")
        .unwrap_or_else(|| panic!("no closing fence: {stdout}"));
    assert!(!opening.contains('\n'), "{opening}");
    (opening, pack)
}

/// The current Unix time in whole seconds.
fn seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// U14 T1 (Owner O2): the sentence the opening ends with, after the time
/// sentence and the identity sentence.
const OPEN_ITEM: &str = "Under the unattended takeover rule, this session records each Session it \
                         leaves open as an open item 「another Session (Session k) may still be \
                         working, not ended」 in its first checkpoint.";

/// The time the opening states, checked to be in the ledger's canonical
/// form and within `before..=after`; the opening without that sentence.
fn time_of(opening: &str, before: u64, after: u64) -> (String, String) {
    let start = opening
        .find("The current UTC time is ")
        .unwrap_or_else(|| panic!("no time sentence: {opening}"));
    let rest = &opening[start + "The current UTC time is ".len()..];
    let end = rest.find('.').unwrap();
    let time = rest[..end].to_owned();
    // The canonical form of the ledger's times, with whole seconds.
    assert_eq!(UtcTimestamp::parse(&time).unwrap().as_str(), time);
    assert_eq!(time.len(), "2026-11-03T18:40:00Z".len(), "{time}");
    let format = |seconds: u64| {
        let written = memprove_cli::run::format_utc(seconds, 0);
        written.replace(".000Z", "Z")
    };
    assert!(
        format(before) <= time && time <= format(after),
        "{time} is not between {} and {}",
        format(before),
        format(after)
    );
    let sentence = format!("The current UTC time is {time}. ");
    (time, opening.replacen(&sentence, "", 1))
}

/// Every last-Pack file under the configuration directory, by path.
fn last_packs(config: &Path) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut pending = vec![config.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.to_string_lossy().ends_with(".pack.json") {
                found.insert(
                    path.to_string_lossy().into_owned(),
                    fs::read_to_string(&path).unwrap(),
                );
            }
        }
    }
    found
}

/// UTF-16 code units.
fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// One environment of the hook: its variables, the identity sentence the
/// opening ends with, and the words of the one warning line on standard
/// error.
struct Case {
    name: &'static str,
    env: Vec<(&'static str, String)>,
    identity: Option<String>,
    warning: Option<&'static str>,
}

impl Case {
    fn new(name: &'static str, env: &[(&'static str, &str)]) -> Self {
        Self {
            name,
            env: env
                .iter()
                .map(|(variable, value)| (*variable, (*value).to_owned()))
                .collect(),
            identity: None,
            warning: None,
        }
    }

    fn naming(mut self, identity: String) -> Self {
        self.identity = Some(identity);
        self
    }

    fn warning(mut self, words: &'static str) -> Self {
        self.warning = Some(words);
        self
    }
}

/// The environments, in order; the first has none of the variables.
fn cases(parent: &str) -> Vec<Case> {
    let longest = "w".repeat(64);
    vec![
        Case::new("none", &[]),
        Case::new(
            "name and role",
            &[(NAME, "takeover-eval-other"), (ROLE, "sub_agent")],
        )
        .naming(format!(
            "You are agent「takeover-eval-other」(sub_agent){NO_INPUT} "
        )),
        Case::new(
            "name, role, and parent",
            &[(NAME, "u11-s-t"), (ROLE, "sub_agent"), (PARENT, parent)],
        )
        .naming(format!(
            "You are agent「u11-s-t」(sub_agent, parent Session {parent}){NO_INPUT} "
        )),
        Case::new(
            "the longest name, orchestrator, and a parent",
            &[(NAME, &longest), (ROLE, "orchestrator"), (PARENT, parent)],
        )
        .naming(format!(
            "You are agent「{longest}」(orchestrator, parent Session {parent}){NO_INPUT} "
        )),
        Case::new("the name only", &[(NAME, "takeover-eval-other")]).warning("are one declaration"),
        Case::new("the role only", &[(ROLE, "sub_agent")]).warning("are one declaration"),
        Case::new("the parent only", &[(PARENT, parent)]).warning("are one declaration"),
        Case::new(
            "a name the server refuses",
            &[(NAME, "bad name!"), (ROLE, "sub_agent")],
        )
        .warning("does not hold its rule"),
        Case::new(
            "a role the server refuses",
            &[(NAME, "takeover-eval-other"), (ROLE, "boss")],
        )
        .warning("does not hold its rule"),
        Case::new("empty variables", &[(NAME, ""), (ROLE, ""), (PARENT, "")]),
    ]
}

/// What one environment's response gave: the response without the time
/// sentence, the Pack, and the last-Pack records.
struct Seen {
    without_time: String,
    pack: String,
    records: BTreeMap<String, String>,
}

/// Runs the hook in the environment `case` and checks its opening: the
/// opening of before (`base`), the time sentence, and the case's identity
/// sentence; the warning line; the length.
fn check_case(fixture: &Fixture, index: usize, case: &Case, base: &str) -> Seen {
    let env: Vec<(&str, &str)> = case
        .env
        .iter()
        .map(|(variable, value)| (*variable, value.as_str()))
        .collect();
    let before = seconds();
    let output = hook_in(fixture, &fixture.repo, &format!("u11-client-{index}"), &env);
    let after = seconds();
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    let (opening, pack) = split(&stdout);
    let (time, without_time) = time_of(opening, before, after);
    let identity = case.identity.as_deref().unwrap_or("");
    assert_eq!(
        opening,
        format!("{base} The current UTC time is {time}. {identity}{OPEN_ITEM}"),
        "{}",
        case.name
    );
    if identity.is_empty() {
        assert_eq!(without_time, format!("{base} {OPEN_ITEM}"), "{}", case.name);
    }
    // U12 T4 (Owner O5; U11-VN1): both sentences together within
    // `OPENING_SENTENCES_MAX_UNITS` in every case, the longest declaration
    // the server takes included; the whole output within the hook's limit.
    // U14 T1 (Owner O2): with the open-item sentence, within the bound (480
    // since U17, when the sentences became English). U21: the open-item
    // sentence names who records the item, 25 units longer (454 before).
    let added = units(&format!(
        "The current UTC time is {time}. {identity}{OPEN_ITEM}"
    ));
    if case.name.starts_with("the longest") {
        println!("the longest time, identity and open-item sentences: {added} UTF-16 code units");
        assert_eq!(added, 479, "{}", case.name);
    }
    assert!(
        added <= memprove_cli::hook::OPENING_SENTENCES_MAX_UNITS,
        "{}: {added}",
        case.name
    );
    assert!(units(&stdout) <= memprove_cli::hook::MAX_OUTPUT_UNITS);
    match case.warning {
        Some(words) => {
            assert_eq!(stderr.lines().count(), 1, "{}: {stderr}", case.name);
            assert!(stderr.starts_with("memprove: warning: "), "{stderr}");
            assert!(stderr.contains(words), "{}: {stderr}", case.name);
            assert!(!stderr.contains("bad name!") && !stderr.contains("boss"));
        }
        None => assert!(
            !stderr.contains("MEMPROVE_AGENT"),
            "{}: {stderr}",
            case.name
        ),
    }
    let records = last_packs(&fixture.config);
    assert!(!records.is_empty(), "{}: no last-Pack record", case.name);
    Seen {
        without_time: stdout.replacen(&format!("The current UTC time is {time}. "), "", 1),
        pack: pack.to_owned(),
        records,
    }
}

/// A Task with a passed run and a fact, its Session ended (the directory's
/// context names the Task): the Task and the ended Session.
fn seeded(fixture: &Fixture, title: &str) -> (String, String) {
    let task = fixture.task_with_fact(title, "check passes");
    let capsule = json(&fixture.agent_ok(&["resume"]));
    let session = capsule
        .get("sessions")
        .and_then(JsonValue::as_array)
        .and_then(|sessions| sessions.first())
        .and_then(|session| session.get("session_id"))
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned();
    fixture.end();
    (task, session)
}

#[test]
fn u11_t1_t2_the_opening_states_the_time_and_names_the_declared_agent() {
    let fixture = Fixture::new();
    let (task, seed_session) = seeded(&fixture, "U11 T1 T2");
    let base = format!(
        "memprove: below is the Context Pack of Task {task} at SessionStart (startup), chosen from this directory's context."
    );
    let mut first: Option<Seen> = None;
    for (index, case) in cases(&seed_session).iter().enumerate() {
        let seen = check_case(&fixture, index, case, &base);
        let Some(first) = &first else {
            first = Some(seen);
            continue;
        };
        // The Pack, its digest, and its record are the same bytes.
        assert_eq!(seen.pack, first.pack, "{}", case.name);
        assert_eq!(
            seen.pack.lines().last(),
            first.pack.lines().last(),
            "{}: the digest",
            case.name
        );
        assert_eq!(seen.records, first.records, "{}: the record", case.name);
        // Without a declared agent the response differs from the first in
        // the time sentence only.
        if case.identity.is_none() {
            assert_eq!(seen.without_time, first.without_time, "{}", case.name);
        }
    }
    let pack = first.unwrap().pack;
    assert!(
        pack.lines().last().unwrap().starts_with("Digest: sha256:"),
        "{pack}"
    );
    // The Pack is the one `resume --pack` prints at the hook's budget.
    let resumed = fixture.agent_ok(&["resume", "--task", &task, "--pack", "--budget", "9000"]);
    assert_eq!(text(&resumed.stdout), pack);
}

/// A statement without a Pack has neither sentence, and `hook print-config`
/// is the same with and without the agent variables.
#[test]
fn u11_t1_t2_a_statement_and_print_config_have_neither_sentence() {
    let fixture = Fixture::new();
    seeded(&fixture, "U11 T1 T2 statement");
    let outside = fixture.root.join("elsewhere");
    fs::create_dir(&outside).unwrap();
    let statement = text(
        &hook_in(
            &fixture,
            &outside,
            "u11-statement",
            &[(NAME, "takeover-eval-other"), (ROLE, "sub_agent")],
        )
        .stdout,
    );
    assert!(
        statement.ends_with("no Context Pack was injected into this session.\n"),
        "{statement}"
    );
    assert_eq!(statement.lines().count(), 1, "{statement}");
    assert!(
        !statement.contains("The current UTC time") && !statement.contains("You are agent"),
        "{statement}"
    );
    // U14 T1 (Owner O2): nor the open-item sentence, which only an opening
    // before a Pack has.
    assert!(!statement.contains(OPEN_ITEM), "{statement}");

    // `hook print-config` is the same with and without the variables.
    let print = |env: &[(&str, &str)]| {
        let output = command(&fixture, &fixture.repo, env)
            .args(["hook", "print-config"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        (output.stdout, output.stderr)
    };
    assert_eq!(
        print(&[]),
        print(&[(NAME, "takeover-eval-other"), (ROLE, "sub_agent")])
    );
}

/// One `tools/call` of `name` with `arguments` on a server started in the
/// repository with the variables `env`; the tool result.
fn mcp_call(fixture: &Fixture, env: &[(&str, &str)], name: &str, arguments: &str) -> JsonValue {
    let mut child = command(fixture, &fixture.repo, env)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}},"name":"{name}","arguments":{arguments}}}}}"#
    );
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stdout = text(&output.stdout);
    let reply = parse_strict(stdout.lines().next().unwrap()).unwrap();
    let result = reply
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("{stdout}"));
    assert_ne!(
        result.get("isError").and_then(JsonValue::as_bool),
        Some(true),
        "{name}: {stdout}"
    );
    result
}

/// The text of a tool result's first content block.
fn result_text(result: &JsonValue) -> String {
    result
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned()
}

#[test]
fn u11_t1_the_mcp_begin_result_names_the_sessions_agent() {
    let fixture = Fixture::new();
    fixture.task_with_fact("U11 T1 MCP", "check passes");
    fixture.end();

    // No variable and a context without an agent: no agent.
    let begun = mcp_call(&fixture, &[], "begin", "{}");
    let agent = begun
        .get("structuredContent")
        .and_then(|content| content.get("agent"))
        .and_then(JsonValue::as_str)
        .unwrap();
    assert_eq!(agent, "no agent");
    assert!(
        result_text(&begun).contains(r#""agent":"no agent""#),
        "{}",
        result_text(&begun)
    );
    mcp_call(&fixture, &[], "end", "{}");

    // The server's variables: the agent they declare.
    let env = [(NAME, "takeover-eval-other"), (ROLE, "sub_agent")];
    let begun = mcp_call(&fixture, &env, "begin", "{}");
    let structured = begun.get("structuredContent").unwrap();
    assert_eq!(
        structured.get("agent").and_then(JsonValue::as_str),
        Some("agent「takeover-eval-other」(sub_agent)")
    );
    assert!(
        result_text(&begun).contains(r#""agent":"agent「takeover-eval-other」(sub_agent)""#),
        "{}",
        result_text(&begun)
    );
    // The same agent the Capsule's Session declares.
    let session = structured
        .get("session_id")
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned();
    let capsule = json(&fixture.agent_ok(&["resume"]));
    let entry = capsule
        .get("sessions")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .find(|entry| entry.get("session_id").and_then(JsonValue::as_str) == Some(&session))
        .unwrap()
        .clone();
    assert_eq!(
        entry
            .get("agent")
            .and_then(|agent| agent.get("name"))
            .and_then(JsonValue::as_str),
        Some("takeover-eval-other")
    );
    mcp_call(&fixture, &env, "end", "{}");

    // Inputs: the agent they declare.
    let begun = mcp_call(
        &fixture,
        &env,
        "begin",
        r#"{"agent_name":"input-agent","agent_role":"human"}"#,
    );
    assert_eq!(
        begun
            .get("structuredContent")
            .and_then(|content| content.get("agent"))
            .and_then(JsonValue::as_str),
        Some("agent「input-agent」(human)")
    );
}
