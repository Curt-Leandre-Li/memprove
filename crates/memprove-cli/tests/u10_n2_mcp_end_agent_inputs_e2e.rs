//! U10 N2 (U9 VN7; `docs/history/api/mcp-contract.md`, ADR 0014 "Amendment (U10,
//! S-K)"), through the real `memprove` and `memproved`: the MCP `end`
//! tool without `abandoned` that is given an agent input is
//! `invalid_arguments` (the tool did not say it ignored them), as the wire
//! refuses an `ended_by` on a Session that is not abandoned; nothing is
//! sent, and the Session stays open. `end` without inputs still ends it.

#[path = "u10_mcp_support/mod.rs"]
mod mcp;
#[path = "u6_ui_support/mod.rs"]
mod support;

use mcp::{PARENT, failed, mcp_call, structured};
use memprove_protocol::strict_json::JsonValue;
use support::{Fixture, json};

/// The `end_state` of each of the Task's Sessions, oldest first.
fn session_states(fixture: &Fixture) -> Vec<String> {
    json(&fixture.agent_ok(&["resume"]))
        .get("sessions")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|entry| {
            entry
                .get("end_state")
                .and_then(JsonValue::as_str)
                .unwrap_or("?")
                .to_owned()
        })
        .collect()
}

#[test]
fn u10_n2_end_without_abandoned_refuses_agent_inputs_and_ends_without_them() {
    let fixture = Fixture::new();
    fixture.task_with_fact("N2 end", "the check passes");
    assert_eq!(session_states(&fixture), ["open"]);

    // Each agent input, alone or whole, is refused when the Session is not
    // abandoned, and the Session stays open.
    for (arguments, parameter) in [
        (
            r#"{"agent_name":"a","agent_role":"sub_agent"}"#,
            "agent_name",
        ),
        (r#"{"agent_name":"a"}"#, "agent_name"),
        (r#"{"agent_role":"sub_agent"}"#, "agent_role"),
        (
            &format!(r#"{{"reason":"done","parent_session":"{PARENT}"}}"#) as &str,
            "parent_session",
        ),
    ] {
        let result = mcp_call(&fixture, &[], "end", arguments);
        assert_eq!(
            failed(&result),
            (Some("invalid_arguments"), Some(parameter)),
            "{arguments}: {result:?}"
        );
        assert_eq!(session_states(&fixture), ["open"], "{arguments}");
    }
    // Without inputs it ends the Session; the server's variables alone are
    // not inputs and are not refused.
    let result = mcp_call(
        &fixture,
        &[
            ("MEMPROVE_AGENT_NAME", "env-agent"),
            ("MEMPROVE_AGENT_ROLE", "sub_agent"),
        ],
        "end",
        r#"{"reason":"done"}"#,
    );
    assert_eq!(
        result.get("isError").and_then(JsonValue::as_bool),
        Some(false),
        "{result:?}"
    );
    assert_eq!(structured(&result, "outcome"), Some("completed"));
    assert_eq!(session_states(&fixture), ["ended"]);
    fixture.stop();
}
