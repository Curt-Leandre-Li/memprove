//! Shared helpers of the U10 S-K process-level tests of the MCP tools: one
//! `memprove mcp serve` per call, started in the fixture's repository
//! with the fixture's configuration, so a call sees the directory's context
//! and the agent variables given.

#![allow(dead_code)] // Each test file uses part of the helpers.

use std::{io::Write as _, path::Path, process::Stdio};

use memprove_protocol::strict_json::{JsonValue, parse_strict};

use crate::support::{Fixture, cli_binary, isolated, text};

/// One `tools/call` of `tool` with `arguments` on a server started in the
/// repository (the directory's context) with the agent variables `env`;
/// the tool result.
pub fn mcp_call(fixture: &Fixture, env: &[(&str, &str)], tool: &str, arguments: &str) -> JsonValue {
    mcp_call_at(fixture, &fixture.data, env, tool, arguments)
}

/// [`mcp_call`] with the data directory `data` in place of the fixture's
/// (a directory whose `memproved.sock` is another server, say a proxy).
pub fn mcp_call_at(
    fixture: &Fixture,
    data: &Path,
    env: &[(&str, &str)],
    tool: &str,
    arguments: &str,
) -> JsonValue {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", data);
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
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}},"name":"{tool}","arguments":{arguments}}}}}"#
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

pub fn structured<'a>(result: &'a JsonValue, name: &str) -> Option<&'a str> {
    result
        .get("structuredContent")
        .and_then(|content| content.get(name))
        .and_then(JsonValue::as_str)
}

/// The tool result's one fixed sentence, `content[0].text`.
pub fn sentence(result: &JsonValue) -> String {
    result
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned()
}

/// The tool's failure code and the parameter it names.
pub fn failed(result: &JsonValue) -> (Option<&str>, Option<&str>) {
    assert_eq!(
        result.get("isError").and_then(JsonValue::as_bool),
        Some(true),
        "{result:?}"
    );
    (
        structured(result, "failure"),
        structured(result, "parameter"),
    )
}

pub const PARENT: &str = "018f47a0-0000-7000-8000-0000000000aa";
