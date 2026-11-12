//! U10 N10 (D3 F29; ADR 0014 "Amendment (U10, S-K)"), through the real
//! `memprove` and `memproved`: `task list` without `--project` lists the
//! Project of the context of the directory it runs in; an explicit
//! `--project` is used as before; in a directory with no context it is the
//! usage error (exit 2) it was, and nothing is sent.

#[path = "u6_ui_support/mod.rs"]
mod support;

use memprove_protocol::strict_json::JsonValue;
use support::{Fixture, json, text};

/// The titles listed in `list`'s `items`, in order.
fn titles(list: &JsonValue) -> Vec<String> {
    list.get("items")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .map(|item| {
            item.get("title")
                .and_then(JsonValue::as_str)
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn u10_n10_task_list_without_project_uses_the_directorys_context() {
    let fixture = Fixture::new();
    fixture.task_with_fact("N10 first", "the check passes");
    fixture.end();

    // In the repository, whose context names the Project.
    let from_context = json(&fixture.agent_ok(&["task", "list"]));
    let explicit = json(&fixture.agent_ok(&["task", "list", "--project", &fixture.project]));
    assert_eq!(titles(&from_context), ["N10 first"]);
    assert_eq!(titles(&explicit), titles(&from_context));
    let limited = json(&fixture.agent_ok(&["task", "list", "--limit", "1"]));
    assert_eq!(titles(&limited), ["N10 first"]);

    // An explicit Project wins over the context's: another Project's list.
    // (A UUIDv7 that names no Project: the daemon's answer, not the
    // context's.)
    let other = "018f47a0-0000-7000-8000-0000000000aa";
    let output = fixture.agent(&["task", "list", "--project", other]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    assert!(
        text(&output.stderr).contains("listTasks failed: 404"),
        "{}",
        text(&output.stderr)
    );

    // Outside a Workspace with a context: the usage error, nothing sent.
    let output = fixture.agent_in(&fixture.root, &["task", "list"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    assert!(
        text(&output.stderr).contains("task list requires --project"),
        "{}",
        text(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    fixture.stop();
}
