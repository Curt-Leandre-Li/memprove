//! U11 N4 (U10 VN14; Owner O6; ADR 0014 "Amendment (U11, S-K)"), through
//! the real `memprove` and `memproved`: `task list` without `--project`
//! takes the Project of the context in the order the other commands take it
//! (the client session's context when `MEMPROVE_CLIENT_SESSION` names one
//! that covers the directory, else the directory's) and, when that gives
//! none, the Project every context of this Workspace root records. A shell
//! that has no client session, in a Workspace root that only a client
//! session began, therefore lists; outside a Workspace root with a context
//! it is still the usage error (exit 2) and nothing is sent.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::process::{Command, Output};

use memprove_protocol::strict_json::JsonValue;
use support::{Fixture, cli_binary, isolated, json, text};

/// The CLI in `cwd` with the fixture's agent token and configuration
/// directory, as client session `client` when one is given.
fn command(fixture: &Fixture, cwd: &std::path::Path, client: Option<&str>) -> Command {
    let mut command = isolated(cli_binary());
    command
        .current_dir(cwd)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .arg("--data-dir")
        .arg(&fixture.data)
        .arg("--token-file")
        .arg(&fixture.agent);
    if let Some(client) = client {
        command.env("MEMPROVE_CLIENT_SESSION", client);
    }
    command
}

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

fn listed(output: &Output) -> Vec<String> {
    assert_eq!(
        output.status.code(),
        Some(0),
        "task list: {}",
        text(&output.stderr)
    );
    titles(&json(output))
}

#[test]
fn u11_n4_task_list_uses_the_client_sessions_context_and_then_the_directorys() {
    let fixture = Fixture::new();
    // Only a client session began here: the root has a context file of
    // client session `lead`, and none of its own.
    let begun = command(&fixture, &fixture.repo, Some("lead"))
        .args([
            "begin",
            "--project",
            &fixture.project,
            "--new-task",
            "N4 lead",
        ])
        .output()
        .unwrap();
    assert_eq!(begun.status.code(), Some(0), "{}", text(&begun.stderr));

    // The client session's own context: as before.
    let own = listed(
        &command(&fixture, &fixture.repo, Some("lead"))
            .args(["task", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(own, ["N4 lead"]);

    // A shell with no client session: the directory has no context, but the
    // root's only context names the Project (this was exit 2).
    let shell = listed(
        &command(&fixture, &fixture.repo, None)
            .args(["task", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(shell, own);

    // Another client session with no context of its own here: the same.
    let other = listed(
        &command(&fixture, &fixture.repo, Some("other"))
            .args(["task", "list", "--limit", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(other, own);

    // An explicit Project still wins: a UUIDv7 that names no Project is the
    // daemon's answer (404), not the context's.
    let elsewhere = "018f47a0-0000-7000-8000-0000000000aa";
    let explicit = command(&fixture, &fixture.repo, None)
        .args(["task", "list", "--project", elsewhere])
        .output()
        .unwrap();
    assert_eq!(
        explicit.status.code(),
        Some(1),
        "{}",
        text(&explicit.stderr)
    );

    // Outside a Workspace root with a context: the usage error, nothing sent.
    for client in [None, Some("lead")] {
        let outside = command(&fixture, &fixture.root, client)
            .args(["task", "list"])
            .output()
            .unwrap();
        assert_eq!(outside.status.code(), Some(2), "{}", text(&outside.stderr));
        assert!(
            text(&outside.stderr).contains("task list requires --project"),
            "{}",
            text(&outside.stderr)
        );
    }
    fixture.stop();
}
