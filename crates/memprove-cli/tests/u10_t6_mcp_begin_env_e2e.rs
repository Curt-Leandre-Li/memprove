//! U10 T6 (Owner O4; D3 F22; ADR 0018 "Amendment (U10)" Part 2,
//! `docs/history/api/mcp-contract.md`), through the real `memprove` and
//! `memproved`: the MCP `begin` tool, given none of `agent_name`,
//! `agent_role`, and `parent_session`, declares the agent of the server's
//! own `MEMPROVE_AGENT_NAME`, `MEMPROVE_AGENT_ROLE`, and
//! `MEMPROVE_PARENT_SESSION`, before the context's agent, as `end` and
//! `memprove begin` do; inputs win over the variables; a name without a
//! role in the variables is `invalid_arguments` and nothing is sent. A
//! client that starts the server with its own environment (Claude Code
//! does) so makes its Session the role it declared, which R1 then checks.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{io::Write as _, process::Stdio};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{Fixture, cli_binary, isolated, json, text};

/// One `tools/call` of `begin` with `arguments` on a server started in the
/// repository (the directory's context) with the agent variables `env`;
/// the tool result.
fn mcp_begin(fixture: &Fixture, env: &[(&str, &str)], arguments: &str) -> JsonValue {
    let mut command = isolated(cli_binary());
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
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}},"name":"begin","arguments":{arguments}}}}}"#
    );
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stdout = text(&output.stdout);
    let reply = parse_strict(stdout.lines().next().unwrap()).unwrap();
    reply
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("{stdout}"))
}

fn is_error(result: &JsonValue) -> bool {
    result.get("isError").and_then(JsonValue::as_bool) == Some(true)
}

fn structured<'a>(result: &'a JsonValue, name: &str) -> Option<&'a str> {
    result
        .get("structuredContent")
        .and_then(|content| content.get(name))
        .and_then(JsonValue::as_str)
}

/// The agent (name, role) of the Task's Session `session` in its Capsule.
fn agent_of(fixture: &Fixture, session: &str) -> Option<(String, String)> {
    let capsule = json(&fixture.agent_ok(&["resume"]));
    let entry = capsule
        .get("sessions")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .find(|entry| entry.get("session_id").and_then(JsonValue::as_str) == Some(session))
        .unwrap()
        .clone();
    let agent = entry.get("agent")?;
    let field = |name: &str| {
        agent
            .get(name)
            .and_then(JsonValue::as_str)
            .unwrap()
            .to_owned()
    };
    Some((field("name"), field("role")))
}

fn session_count(fixture: &Fixture) -> usize {
    json(&fixture.agent_ok(&["resume"]))
        .get("sessions")
        .and_then(JsonValue::as_array)
        .unwrap()
        .len()
}

/// The Session a successful `begin` result names, after ending it from the
/// command line (the directory's context) so the next case can begin.
fn begun(fixture: &Fixture, result: &JsonValue) -> String {
    assert!(!is_error(result), "{result:?}");
    let session = structured(result, "session_id").unwrap().to_owned();
    fixture.end();
    session
}

#[test]
fn u10_t6_mcp_begin_declares_the_servers_agent_variables() {
    let fixture = Fixture::new();
    fixture.task_with_fact("T6 begin", "the check passes");
    fixture.end();
    let variables = [
        ("MEMPROVE_AGENT_NAME", "env-agent"),
        ("MEMPROVE_AGENT_ROLE", "sub_agent"),
    ];

    // Only the variables: the Session is theirs.
    let session = begun(&fixture, &mcp_begin(&fixture, &variables, "{}"));
    assert_eq!(
        agent_of(&fixture, &session),
        Some(("env-agent".to_owned(), "sub_agent".to_owned()))
    );

    // Inputs and variables: the inputs, whole.
    let session = begun(
        &fixture,
        &mcp_begin(
            &fixture,
            &variables,
            r#"{"agent_name":"input-agent","agent_role":"human"}"#,
        ),
    );
    assert_eq!(
        agent_of(&fixture, &session),
        Some(("input-agent".to_owned(), "human".to_owned()))
    );

    // Neither: the context's agent (the one the last begin recorded).
    let session = begun(&fixture, &mcp_begin(&fixture, &[], "{}"));
    assert_eq!(
        agent_of(&fixture, &session),
        Some(("input-agent".to_owned(), "human".to_owned()))
    );

    // A name without a role in the variables: refused, nothing sent.
    let before = session_count(&fixture);
    let refused = mcp_begin(&fixture, &[("MEMPROVE_AGENT_NAME", "env-agent")], "{}");
    assert!(is_error(&refused), "{refused:?}");
    assert_eq!(structured(&refused, "failure"), Some("invalid_arguments"));
    assert_eq!(structured(&refused, "parameter"), Some("agent_role"));
    assert_eq!(session_count(&fixture), before);
    fixture.stop();
}
