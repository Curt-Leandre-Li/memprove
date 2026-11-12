//! U10 T7 (Owner O4; D3 F24, F30; ADR 0014 "Amendment (U10, S-T)", ADR 0013
//! "Amendment (U10, S-T)"), through the real `memprove` and
//! `memproved`: the takeover hint of a Pack is a command another client
//! session copies as it stands. It names the Session's ID and `session end
//! … --abandoned`, which needs no `--revision` (the CLI reads the Session's
//! current one; an explicit one is still sent as given), and the program is
//! `memprove` when that name finds this executable on `PATH`, else its
//! absolute path, in `resume --pack` and in the hook's Pack alike. The end
//! records the copying client as the ender. `end` in a context that names
//! no Session points at `session end <SESSION_ID>`.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    io::Write as _,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

use memprove_protocol::strict_json::JsonValue;
use support::{Fixture, cli_binary, isolated, json, string, text};

const HINT_START: &str = "; end it before taking over: ";

/// The CLI in the repository as client session `client`, with `path` as
/// `PATH` and the agent `agent` declared by its environment variables.
fn command(fixture: &Fixture, client: &str, path: &str, agent: Option<&str>) -> Command {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("PATH", path)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &fixture.data)
        .env("MEMPROVE_CLIENT_SESSION", client);
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

/// The directory of the binaries under test, and a `PATH` without it.
fn bin_dir() -> PathBuf {
    std::fs::canonicalize(cli_binary())
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

const BARE_PATH: &str = "/usr/bin:/bin";

/// A client session that begins the Task with an agent and leaves its
/// Session open; returns the Session's ID.
fn open_session(fixture: &Fixture, task: &str, client: &str) -> String {
    let begun = json(&ok(
        command(fixture, client, BARE_PATH, Some(client))
            .args(["begin", "--project", &fixture.project, "--task", task])
            .output()
            .unwrap(),
        "begin",
    ));
    string(&begun, "session_id").to_owned()
}

/// The `Session <number> ` line of a Pack.
fn line_of(pack: &str, number: u64) -> String {
    pack.lines()
        .find(|line| line.starts_with(&format!("Session {number} ")))
        .unwrap_or_else(|| panic!("no Session {number}:\n{pack}"))
        .to_owned()
}

/// The command of a hint, with `<why>` filled in, as words.
fn hint_command(line: &str, why: &str) -> Vec<String> {
    let hint = line.split_once(HINT_START).unwrap().1;
    let (command, _) = hint.split_once(" (MCP: ").unwrap();
    command
        .split(' ')
        .map(|word| {
            if word == "<why>" {
                why.to_owned()
            } else {
                word.to_owned()
            }
        })
        .collect()
}

/// `hook session-start` of a new client session with `path` as `PATH`.
fn hook(fixture: &Fixture, client: &str, path: &str) -> String {
    let mut child = isolated(cli_binary())
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("PATH", path)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &fixture.data)
        .args(["hook", "session-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"{client}","source":"startup","cwd":"{}"}}"#,
        fixture.repo.display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    text(&output.stdout)
}

/// The Capsule's entry of Session `session`.
fn capsule_session(fixture: &Fixture, task: &str, session: &str) -> JsonValue {
    let capsule = json(&ok(
        command(fixture, "reader", BARE_PATH, None)
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

#[test]
fn u10_t7_the_hint_is_a_command_another_client_copies() {
    let fixture = Fixture::new();
    let task = fixture.task_with_fact("T7 takeover", "the check passes");
    fixture.end();
    let first = open_session(&fixture, &task, "seed-a");
    let program = std::fs::canonicalize(cli_binary()).unwrap();
    let program = program.to_str().unwrap();

    // Without the binaries on PATH: the absolute path, the ID, the number.
    let pack = text(
        &ok(
            command(&fixture, "taker", BARE_PATH, None)
                .args(["resume", "--task", &task, "--pack"])
                .output()
                .unwrap(),
            "resume --pack",
        )
        .stdout,
    );
    let line = line_of(&pack, 2);
    assert!(
        line.ends_with(&format!(
            "{HINT_START}{program} session end {first} --abandoned --reason <why> (MCP: end \
             {{session:2, abandoned:true}})"
        )),
        "{line}"
    );
    // The hook's Pack in the same environment has the same line.
    let injected = hook(&fixture, "taker-hook", BARE_PATH);
    assert_eq!(line_of(&injected, 2), line, "{injected}");
    // With them on PATH, `memprove` finds this executable.
    let on_path = format!("{}:{BARE_PATH}", bin_dir().display());
    let named = text(
        &ok(
            command(&fixture, "taker", &on_path, None)
                .args(["resume", "--task", &task, "--pack"])
                .output()
                .unwrap(),
            "resume --pack on PATH",
        )
        .stdout,
    );
    assert_eq!(
        line_of(&named, 2),
        line.replace(&format!(": {program} "), ": memprove "),
        "{named}"
    );
    assert_eq!(
        line_of(&hook(&fixture, "taker-hook2", &on_path), 2),
        line_of(&named, 2)
    );

    // Copied as it stands by another client session (no context, no
    // --revision): the Session ends abandoned, ended by that client's agent.
    let words = hint_command(&line, "taken over");
    assert_eq!(words[0], program);
    let ended = command(&fixture, "taker", BARE_PATH, Some("taker"))
        .args(&words[1..])
        .output()
        .unwrap();
    ok(ended, "the copied command");
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

    explicit_revision_and_the_end_message(&fixture, &task);
    fixture.stop();
}

/// An explicit `--revision` is sent as given: a wrong one is refused, the
/// right one ends the Session; `end` in a context that names no Session
/// points at `session end`.
fn explicit_revision_and_the_end_message(fixture: &Fixture, task: &str) {
    // An explicit --revision is sent as given: a wrong one is refused, the
    // right one ends the Session.
    let second = open_session(fixture, task, "seed-b");
    let wrong = command(fixture, "taker", BARE_PATH, None)
        .args(["session", "end", &second, "--abandoned", "--revision", "7"])
        .args(["--reason", "taken over"])
        .output()
        .unwrap();
    assert_eq!(wrong.status.code(), Some(1), "{}", text(&wrong.stderr));
    assert!(
        text(&wrong.stderr).contains("revision_conflict"),
        "{}",
        text(&wrong.stderr)
    );
    ok(
        command(fixture, "taker", BARE_PATH, None)
            .args(["session", "end", &second, "--abandoned", "--revision", "1"])
            .args(["--reason", "taken over"])
            .output()
            .unwrap(),
        "session end --revision 1",
    );
    assert_eq!(
        capsule_session(fixture, task, &second)
            .get("end_state")
            .and_then(JsonValue::as_str),
        Some("crashed")
    );

    // `end` in a context that names no Session points at `session end`.
    let refused = command(fixture, "taker", BARE_PATH, None)
        .args(["end", "--abandoned", "--reason", "x"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr));
    let stderr = text(&refused.stderr);
    assert!(
        stderr.contains("memprove session end <SESSION_ID> --abandoned --reason <why>"),
        "{stderr}"
    );
    assert!(!stderr.contains("pass <SESSION_ID>"), "{stderr}");
}

/// U10 T7 (Owner O4, the three `--help` pages and the guide in step): `resume --help`, whose `--pack`
/// description and notes speak of the takeover, names the hint's command,
/// `memprove session end <SESSION_ID> --abandoned --reason <why>`, and no
/// longer the `end --abandoned` the hint named before.
#[test]
fn u10_t7_resume_help_names_the_hints_command() {
    let output = isolated(cli_binary())
        .args(["resume", "--help"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let help = text(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        help.contains("session end <SESSION_ID> --abandoned"),
        "{help}"
    );
    assert!(help.contains("end {session:<k>, abandoned:true}"), "{help}");
    for old in [
        "memprove end --abandoned --reason <why>",
        "End a Session someone else left with `memprove end --abandoned`",
    ] {
        assert!(!help.contains(old), "{old}: {help}");
    }
}
