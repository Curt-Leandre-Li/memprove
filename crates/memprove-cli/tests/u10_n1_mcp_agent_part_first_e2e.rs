//! U10 N1 (U9 VN6; `docs/history/api/mcp-contract.md`, ADR 0014 "Amendment (U10,
//! S-K)"), through the real `memprove` and `memproved`: the MCP `begin`
//! and `end` tools. An agent declaration that is only a part (a name
//! without a role, a role without a name, a parent alone), in the inputs or
//! in the server's variables, is `invalid_arguments` before the tool says
//! there is no Project, no Task, or no context, whether or not a context
//! exists.

#[path = "u10_mcp_support/mod.rs"]
mod mcp;
#[path = "u6_ui_support/mod.rs"]
mod support;

use mcp::{PARENT, failed, mcp_call};
use support::Fixture;

#[test]
fn u10_n1_a_part_of_an_agent_is_refused_before_the_missing_project() {
    let fixture = Fixture::new();

    // No context at all: without agent inputs `begin` says there is no
    // Project; with a whole declaration it still does.
    for arguments in [r"{}", r#"{"agent_name":"a","agent_role":"sub_agent"}"#] {
        let result = mcp_call(&fixture, &[], "begin", arguments);
        assert_eq!(
            failed(&result),
            (Some("no_project"), None),
            "{arguments}: {result:?}"
        );
    }
    // A part alone is the answer, whichever part.
    for (arguments, parameter) in [
        (r#"{"agent_name":"a"}"#, "agent_role"),
        (r#"{"agent_role":"sub_agent"}"#, "agent_name"),
        (
            &format!(r#"{{"parent_session":"{PARENT}"}}"#) as &str,
            "agent_name",
        ),
        (
            &format!(r#"{{"agent_name":"a","parent_session":"{PARENT}"}}"#) as &str,
            "agent_role",
        ),
    ] {
        let result = mcp_call(&fixture, &[], "begin", arguments);
        assert_eq!(
            failed(&result),
            (Some("invalid_arguments"), Some(parameter)),
            "{arguments}: {result:?}"
        );
    }
    // The server's variables are a source like the inputs (T6): a name
    // without a role there is refused first too.
    let result = mcp_call(
        &fixture,
        &[("MEMPROVE_AGENT_NAME", "env-agent")],
        "begin",
        r"{}",
    );
    assert_eq!(
        failed(&result),
        (Some("invalid_arguments"), Some("agent_role")),
        "{result:?}"
    );
    // The inputs win over the variables, whole: a whole input is not
    // refused for the variables' part, so the missing Project is the answer.
    let result = mcp_call(
        &fixture,
        &[("MEMPROVE_AGENT_NAME", "env-agent")],
        "begin",
        r#"{"agent_name":"a","agent_role":"sub_agent"}"#,
    );
    assert_eq!(failed(&result), (Some("no_project"), None), "{result:?}");
    fixture.stop();
}

#[test]
fn u10_n1_a_part_of_an_agent_is_refused_before_the_missing_context_of_end() {
    let fixture = Fixture::new();
    let abandoned = |extra: &str| format!(r#"{{"session":1,"abandoned":true{extra}}}"#);

    // No context: `end` says so; a part of an agent is told first.
    let result = mcp_call(&fixture, &[], "end", &abandoned(""));
    assert_eq!(failed(&result), (Some("no_context"), None), "{result:?}");
    for (extra, parameter) in [
        (r#","agent_name":"a""#, "agent_role"),
        (r#","agent_role":"sub_agent""#, "agent_name"),
        (
            &format!(r#","parent_session":"{PARENT}""#) as &str,
            "agent_name",
        ),
    ] {
        let result = mcp_call(&fixture, &[], "end", &abandoned(extra));
        assert_eq!(
            failed(&result),
            (Some("invalid_arguments"), Some(parameter)),
            "{extra}: {result:?}"
        );
    }
    // The server's variables too.
    let result = mcp_call(
        &fixture,
        &[("MEMPROVE_AGENT_ROLE", "sub_agent")],
        "end",
        &abandoned(""),
    );
    assert_eq!(
        failed(&result),
        (Some("invalid_arguments"), Some("agent_name")),
        "{result:?}"
    );
    fixture.stop();
}
