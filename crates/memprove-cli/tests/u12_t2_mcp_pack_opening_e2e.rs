//! U12 T2 (Owner O4; U11-VN7, FN2; ADR 0014 E7.5.3 and E7.5.5, "Corrected
//! U12 T2"), through the real `memprove` and `memproved`: the MCP `pack`
//! tool's result, without `query`, has three text blocks,
//! the opening, the Pack, and the object. The opening comes from the hook's
//! function without its `SessionStart` words:
//! `memprove: below is the Context Pack of Task <ID><why>. The current UTC time is <time>.`, then,
//! when the server's process has `MEMPROVE_AGENT_NAME` and
//! `MEMPROVE_AGENT_ROLE`, the identity sentence (with `, parent Session <ID>` when
//! `MEMPROVE_PARENT_SESSION` is set); with only part of them, none. The
//! Pack block is the bytes of `resume --pack --budget 6000` in every
//! environment, its `pack_digest` and its last-Pack record the same; a query
//! Pack has no opening.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use memprove_domain::UtcTimestamp;
use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{Fixture, cli_binary, isolated, json, text};

const NAME: &str = "MEMPROVE_AGENT_NAME";
const ROLE: &str = "MEMPROVE_AGENT_ROLE";
const PARENT: &str = "MEMPROVE_PARENT_SESSION";
const NO_INPUT: &str = "; when begin and end get no agent_name, agent_role, or parent_session, the server uses these three.";
/// U14 T1 (Owner O2): the sentence the opening ends with, after the time
/// sentence and the identity sentence.
const OPEN_ITEM: &str = "Under the unattended takeover rule, this session records each Session it \
                         leaves open as an open item 「another Session (Session k) may still be \
                         working, not ended」 in its first checkpoint.";

fn seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn utc(seconds: u64) -> String {
    memprove_cli::run::format_utc(seconds, 0).replace(".000Z", "Z")
}

/// One `tools/call` of `pack` with `arguments` on a server started in the
/// repository with the variables `env`: the texts of the result's content
/// blocks and the server's standard error.
fn pack_call(fixture: &Fixture, env: &[(&str, &str)], arguments: &str) -> (Vec<String>, String) {
    let mut command: Command = isolated(cli_binary());
    command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &fixture.data);
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}},"name":"pack","arguments":{arguments}}}}}"#
    );
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stdout = text(&output.stdout);
    let reply = parse_strict(stdout.lines().next().unwrap()).unwrap();
    let result = reply.get("result").unwrap_or_else(|| panic!("{stdout}"));
    assert_eq!(
        result.get("isError").and_then(JsonValue::as_bool),
        Some(false),
        "{stdout}"
    );
    assert!(result.get("structuredContent").is_none(), "{stdout}");
    let blocks = result
        .get("content")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|block| {
            assert_eq!(block.get("type").and_then(JsonValue::as_str), Some("text"));
            block
                .get("text")
                .and_then(JsonValue::as_str)
                .unwrap()
                .to_owned()
        })
        .collect();
    (blocks, text(&output.stderr))
}

/// The time an opening states, within `before..=after`, and the opening
/// with it written as `<T>`.
fn time_of(opening: &str, before: u64, after: u64) -> String {
    let start = opening
        .find("The current UTC time is ")
        .unwrap_or_else(|| panic!("no time sentence: {opening}"));
    let rest = &opening[start + "The current UTC time is ".len()..];
    let time = &rest[..rest.find('.').unwrap()];
    assert_eq!(UtcTimestamp::parse(time).unwrap().as_str(), time);
    assert!(
        utc(before).as_str() <= time && time <= utc(after).as_str(),
        "{time}"
    );
    opening.replacen(
        &format!("The current UTC time is {time}."),
        "The current UTC time is <T>.",
        1,
    )
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

/// A Task with a passed run and a fact, its Session ended, named by the
/// directory's context: the Task and its Session.
fn seeded(fixture: &Fixture) -> (String, String) {
    let task = fixture.task_with_fact("U12 T2", "check passes");
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

/// One environment's call: its opening (time written `<T>`), Pack, object,
/// standard error, and the last-Pack records after it.
struct Seen {
    opening: String,
    pack: String,
    object: JsonValue,
    stderr: String,
    records: BTreeMap<String, String>,
}

fn call(fixture: &Fixture, env: &[(&str, &str)]) -> Seen {
    let before = seconds();
    let (blocks, stderr) = pack_call(fixture, env, "{}");
    let after = seconds();
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert!(!blocks[0].contains('\n'), "{}", blocks[0]);
    Seen {
        opening: time_of(&blocks[0], before, after),
        pack: blocks[1].clone(),
        object: parse_strict(&blocks[2]).unwrap(),
        stderr,
        records: last_packs(&fixture.config),
    }
}

#[test]
fn u12_t2_the_pack_result_opens_with_the_hooks_opening() {
    let fixture = Fixture::new();
    let (task, seed_session) = seeded(&fixture);
    let base = format!(
        "memprove: below is the Context Pack of Task {task}, chosen from this directory's context. The current UTC time is <T>. "
    );
    let resumed = text(
        &fixture
            .agent_ok(&["resume", "--task", &task, "--pack", "--budget", "6000"])
            .stdout,
    );

    // The three variables: the agent with its parent.
    let all = call(
        &fixture,
        &[
            (NAME, "u12-s-t"),
            (ROLE, "sub_agent"),
            (PARENT, &seed_session),
        ],
    );
    assert_eq!(
        all.opening,
        format!(
            "{base}You are agent「u12-s-t」(sub_agent, parent Session {seed_session}){NO_INPUT} {OPEN_ITEM}"
        )
    );
    // None of them: the time only.
    let none = call(&fixture, &[]);
    assert_eq!(none.opening, format!("{base}{OPEN_ITEM}"));
    assert!(!none.stderr.contains("MEMPROVE_AGENT"), "{}", none.stderr);
    // The name and the role: the agent without a parent.
    let named = call(
        &fixture,
        &[(NAME, "takeover-eval-other"), (ROLE, "sub_agent")],
    );
    assert_eq!(
        named.opening,
        format!("{base}You are agent「takeover-eval-other」(sub_agent){NO_INPUT} {OPEN_ITEM}")
    );
    // Part of the declaration: no agent.
    let part = call(&fixture, &[(NAME, "takeover-eval-other")]);
    assert_eq!(part.opening, format!("{base}{OPEN_ITEM}"));

    // The Pack block, its digest, and its record are the same bytes in every
    // environment, and the Pack is `resume --pack --budget 6000`'s.
    for seen in [&all, &none, &named, &part] {
        assert_eq!(seen.pack, resumed);
        assert_eq!(
            seen.object
                .get("pack_digest")
                .and_then(JsonValue::as_str)
                .map(|digest| format!("Digest: {digest}")),
            resumed.lines().last().map(str::to_owned)
        );
        assert_eq!(
            seen.object.get("task_id").and_then(JsonValue::as_str),
            Some(task.as_str())
        );
        assert_eq!(seen.records, all.records);
        // The sentences after the first stay within the bound of U12 T4
        // (U14 T1: 320, with the open-item sentence).
        let sentences = seen.opening.split_once(". ").unwrap().1;
        assert!(
            sentences.encode_utf16().count() <= memprove_cli::hook::OPENING_SENTENCES_MAX_UNITS,
            "{sentences}"
        );
    }
}

#[test]
fn u12_t2_a_query_has_no_opening_and_a_number_says_so() {
    let fixture = Fixture::new();
    let (task, _) = seeded(&fixture);
    let env = [(NAME, "u12-s-t"), (ROLE, "sub_agent")];
    // A query Pack: two blocks, as before U12.
    let arguments = r#"{"query":"check"}"#;
    let (blocks, _) = pack_call(&fixture, &env, arguments);
    assert_eq!(blocks.len(), 2, "{arguments}: {blocks:?}");
    assert!(!blocks[0].starts_with("memprove: "), "{arguments}");
    assert!(parse_strict(&blocks[1]).is_ok(), "{arguments}");
    // A Task named by number: the opening says so.
    let before = seconds();
    let (blocks, _) = pack_call(&fixture, &[], r#"{"task":1}"#);
    let after = seconds();
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert_eq!(
        time_of(&blocks[0], before, after),
        format!(
            "memprove: below is the Context Pack of Task {task}, chosen by the call's task argument \
             (Task number 1). The current UTC time is <T>. {OPEN_ITEM}"
        )
    );
    assert!(blocks[1].starts_with("Pack: "), "{}", blocks[1]);
}
