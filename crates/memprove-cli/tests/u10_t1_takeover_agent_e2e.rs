//! U10 T1 (Owner O3, E1 revision 13; VN23, D3 F23, F16; ADR 0021), through
//! the real `memprove` and `memproved`: a client that declared an agent
//! leaves its Session open; another client session reads the Task. The
//! `Session ` line of that Session names its agent (`; agent「…」(…)`) and ends
//! with the takeover hint, in `resume --pack` and in the Pack the
//! `SessionStart` hook injects alike; the `Can: begin a new Session` line says to end
//! it by that line first, or to make sure it still works. The reader whose
//! own Session it is sees the agent and no hint. A Task whose Sessions all
//! ended and declared no agent has no agent part and no such words.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    io::Write as _,
    process::{Command, Output, Stdio},
};

use support::{Fixture, cli_binary, isolated, json, string, text};

const AGENT: &str = "; agent「seed-a」(sub_agent)";
const HINT_START: &str = "; end it before taking over: ";
const BEGIN: &str = "Can: begin a new Session (createSession; first end it as the line of Session 1 \
                     says, or confirm it is still working; needs mcp-write)";

fn agent_command(fixture: &Fixture, client: Option<&str>) -> Command {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &fixture.data);
    if let Some(client) = client {
        command.env("MEMPROVE_CLIENT_SESSION", client);
    }
    command
}

fn agent_as(fixture: &Fixture, client: Option<&str>, words: &[&str]) -> Output {
    let output = agent_command(fixture, client).args(words).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "memprove {}: {}",
        words.join(" "),
        text(&output.stderr)
    );
    output
}

/// `hook session-start` for the new client session `client`.
fn hook(fixture: &Fixture, client: &str) -> String {
    let mut child = agent_command(fixture, None)
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
    let stdout = text(&output.stdout);
    assert!(stdout.contains("\n```text\nPack: "), "{stdout}");
    stdout
}

/// The one `Session 1 ` line of a Pack.
fn session_one(pack: &str) -> &str {
    let lines = pack
        .lines()
        .filter(|line| line.starts_with("Session 1 "))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "{pack}");
    lines[0]
}

#[test]
fn u10_t1_the_open_session_line_names_its_agent_for_the_reader_taking_over() {
    let fixture = Fixture::new();
    // Session 1: client seed-a declares an agent, runs the check, and leaves its
    // Session open.
    let begun = json(&agent_as(
        &fixture,
        Some("seed-a"),
        &[
            "begin",
            "--project",
            &fixture.project,
            "--new-task",
            "T1 takeover",
            "--agent-name",
            "seed-a",
            "--agent-role",
            "sub_agent",
        ],
    ));
    let task = string(&begun, "task_id").to_owned();
    let session = string(&begun, "session_id").to_owned();
    agent_as(
        &fixture,
        Some("seed-a"),
        &[
            "run",
            "--test-format",
            "cargo",
            "--",
            "sh",
            "tests/check.sh",
        ],
    );

    // Another client session: the line names the agent, then the hint.
    let other = text(
        &agent_as(
            &fixture,
            Some("taker"),
            &["resume", "--task", &task, "--pack"],
        )
        .stdout,
    );
    let line = session_one(&other);
    assert!(
        line.starts_with(&format!("Session 1 {session} not ended (started ")),
        "{line}"
    );
    let (head, hint) = line.split_once(HINT_START).unwrap();
    assert!(head.ends_with(&format!("){AGENT}")), "{line}");
    assert!(!hint.contains("agent"), "{line}");
    assert!(other.lines().any(|line| line == BEGIN), "{other}");

    // The hook of a new client session injects the same line.
    let injected = hook(&fixture, "taker-hook");
    assert_eq!(session_one(&injected), line, "{injected}");
    assert!(injected.lines().any(|line| line == BEGIN), "{injected}");

    // The client whose Session it is: the agent, no hint.
    let own = text(&agent_as(&fixture, Some("seed-a"), &["resume", "--pack"]).stdout);
    assert_eq!(session_one(&own), head, "{own}");

    // Ended, the line names the agent and has no hint; a Task whose Session
    // declared no agent and ended has neither the part nor the words.
    agent_as(&fixture, Some("seed-a"), &["end"]);
    let ended = text(
        &agent_as(
            &fixture,
            Some("taker"),
            &["resume", "--task", &task, "--pack"],
        )
        .stdout,
    );
    let line = session_one(&ended);
    assert!(
        line.ends_with(AGENT) && !line.contains(HINT_START),
        "{line}"
    );
    assert!(
        ended
            .lines()
            .any(|line| line == "Can: begin a new Session (createSession; needs mcp-write)"),
        "{ended}"
    );
    let plain = fixture.task_with_fact("T1 plain", "the check passes");
    fixture.end();
    let plain = text(
        &agent_as(
            &fixture,
            Some("taker"),
            &["resume", "--task", &plain, "--pack"],
        )
        .stdout,
    );
    assert!(!plain.contains("agent「"), "{plain}");
    assert!(!plain.contains(HINT_START), "{plain}");
    assert!(
        !plain.contains("first end it as the line of Session"),
        "{plain}"
    );
    fixture.stop();
}
