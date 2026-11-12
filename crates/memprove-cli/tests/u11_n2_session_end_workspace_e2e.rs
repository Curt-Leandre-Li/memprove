//! U11 N2 (U10 VN10; Owner O6; ADR 0021 "Amendment (U11)"), through the real
//! `memprove` and `memproved`: `session end <SESSION_ID> --abandoned`
//! reads the Session's current revision from the Workspace's Sessions. In a
//! configuration directory that holds only the daemon's location and a
//! token (no context of the Workspace root), the Workspace is not known, so
//! the command takes `--workspace <WORKSPACE_ID>`; without it, nothing is
//! sent and the error says both ways out. In the configuration directory
//! that has a context of the Workspace root the Pack's command is copied as
//! it stands, as before. `session end --help` states the premise.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

use memprove_protocol::strict_json::JsonValue;
use support::{Fixture, cli_binary, isolated, json, string, text};

const BARE_PATH: &str = "/usr/bin:/bin";

/// The CLI in the repository as client session `client`, with the
/// configuration directory `config`; `agent` declares its agent.
fn command(fixture: &Fixture, config: &PathBuf, client: &str, agent: Option<&str>) -> Command {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("PATH", BARE_PATH)
        .env("MEMPROVE_CONFIG_DIR", config)
        .env("MEMPROVE_CLIENT_SESSION", client);
    if config == &fixture.config {
        command.env("MEMPROVE_DATA_DIR", &fixture.data);
    }
    if let Some(agent) = agent {
        command
            .env("MEMPROVE_AGENT_NAME", agent)
            .env("MEMPROVE_AGENT_ROLE", "sub_agent");
    }
    command
}

fn ok(output: Output, what: &str) -> Output {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what}: {}",
        text(&output.stderr)
    );
    output
}

/// A configuration directory that holds the daemon's location and the
/// agent's token, and nothing else: no context of any Workspace root.
fn token_only_config(fixture: &Fixture) -> PathBuf {
    let config = fixture.root.join("cfg2");
    fs::create_dir(&config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    for (name, content) in [
        ("token", fs::read_to_string(&fixture.agent).unwrap()),
        ("data-dir", format!("{}\n", fixture.data.display())),
    ] {
        let path = config.join(name);
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    config
}

/// A client session that begins the Task with an agent and leaves its
/// Session open; returns the Session's ID and the Workspace's.
fn open_session(fixture: &Fixture, task: &str, client: &str) -> (String, String) {
    let begun = json(&ok(
        command(fixture, &fixture.config, client, Some(client))
            .args(["begin", "--project", &fixture.project, "--task", task])
            .output()
            .unwrap(),
        "begin",
    ));
    (
        string(&begun, "session_id").to_owned(),
        string(&begun, "workspace_id").to_owned(),
    )
}

/// The Capsule's entry of Session `session`.
fn capsule_session(fixture: &Fixture, task: &str, session: &str) -> JsonValue {
    let capsule = json(&ok(
        command(fixture, &fixture.config, "reader", None)
            .args(["resume", "--task", task])
            .output()
            .unwrap(),
        "resume",
    ));
    capsule
        .get("sessions")
        .and_then(JsonValue::as_array)
        .unwrap()
        .iter()
        .find(|entry| entry.get("session_id").and_then(JsonValue::as_str) == Some(session))
        .unwrap()
        .clone()
}

fn end_state(fixture: &Fixture, task: &str, session: &str) -> String {
    capsule_session(fixture, task, session)
        .get("end_state")
        .and_then(JsonValue::as_str)
        .unwrap_or("none")
        .to_owned()
}

/// `session end <session> --abandoned --reason x` as client session `taker`
/// (agent `taker`) in configuration directory `config`, with `extra` after it.
fn end_as_taker(fixture: &Fixture, config: &PathBuf, session: &str, extra: &[&str]) -> Output {
    command(fixture, config, "taker", Some("taker"))
        .args(["session", "end", session, "--abandoned", "--reason", "x"])
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn u11_n2_session_end_takes_the_workspace_where_no_context_names_it() {
    let fixture = Fixture::new();
    let task = fixture.task_with_fact("N2 takeover", "the check passes");
    fixture.end();
    let (first, workspace) = open_session(&fixture, &task, "seed-a");
    let (second, _) = open_session(&fixture, &task, "seed-b");
    let (third, _) = open_session(&fixture, &task, "seed-c");
    let other = token_only_config(&fixture);

    // Another configuration directory, no --workspace: nothing is sent, and
    // the error says the ways out (exit 2).
    let refused = end_as_taker(&fixture, &other, &first, &[]);
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr));
    let stderr = text(&refused.stderr)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for words in [
        "no context of this configuration directory names its Workspace",
        "run the command in the Workspace's directory where this configuration directory has a \
         context",
        "pass --workspace <WORKSPACE_ID>",
        "memprove workspace resolve --project <PROJECT_ID> <PATH>",
        "pass --revision <N>",
        "session end was not sent",
    ] {
        assert!(stderr.contains(words), "{words:?} missing:\n{stderr}");
    }
    assert_eq!(end_state(&fixture, &task, &first), "open");

    // With --workspace the Session ends abandoned, ended by that client's
    // agent.
    ok(
        end_as_taker(&fixture, &other, &first, &["--workspace", &workspace]),
        "session end --workspace",
    );
    let entry = capsule_session(&fixture, &task, &first);
    assert_eq!(
        entry.get("end_state").and_then(JsonValue::as_str),
        Some("crashed")
    );
    assert_eq!(
        entry
            .get("ended_by")
            .and_then(|by| by.get("agent"))
            .and_then(|agent| agent.get("name"))
            .and_then(JsonValue::as_str),
        Some("taker")
    );

    // A Workspace that is not the Session's ends nothing, whatever the
    // daemon says of it: the command fails and says nothing was sent.
    let elsewhere = "018f47a0-0000-7000-8000-0000000000aa";
    let unlisted = end_as_taker(&fixture, &other, &second, &["--workspace", elsewhere]);
    assert_ne!(unlisted.status.code(), Some(0));
    assert!(text(&unlisted.stderr).contains("session end was not sent"));
    assert_eq!(end_state(&fixture, &task, &second), "open");

    // A malformed value is a usage error and sends nothing.
    let malformed = end_as_taker(&fixture, &other, &second, &["--workspace", "a&b=c"]);
    assert_eq!(malformed.status.code(), Some(2));
    assert_eq!(end_state(&fixture, &task, &second), "open");

    // The configuration directory that knows the Workspace root: the Pack's
    // command, copied as it stands, still ends the Session; an explicit
    // --workspace wins there too.
    let wrong = end_as_taker(
        &fixture,
        &fixture.config,
        &third,
        &["--workspace", elsewhere],
    );
    assert_ne!(wrong.status.code(), Some(0), "{}", text(&wrong.stderr));
    assert!(text(&wrong.stderr).contains("session end was not sent"));
    assert_eq!(end_state(&fixture, &task, &third), "open");
    ok(
        end_as_taker(&fixture, &fixture.config, &second, &[]),
        "session end as copied",
    );
    assert_eq!(end_state(&fixture, &task, &second), "crashed");
    fixture.stop();
}

/// `session end --help` states the premise: where the Workspace comes from,
/// and `--workspace`.
#[test]
fn u11_n2_session_end_help_states_the_premise() {
    let output = isolated(cli_binary())
        .args(["session", "end", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let help = text(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for words in [
        "[--workspace <WORKSPACE_ID>]",
        "--workspace <WORKSPACE_ID>",
        "the Workspace whose Sessions are read for the revision",
        "a configuration directory that holds no context of this Workspace root",
        "memprove session end <SESSION_ID> --abandoned --reason <why> --workspace <WORKSPACE_ID>",
    ] {
        assert!(help.contains(words), "{words:?} missing:\n{help}");
    }
}
