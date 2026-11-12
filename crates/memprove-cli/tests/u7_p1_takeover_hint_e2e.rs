//! U7 P1 (E1 revision 9; D2 F16, F15, Owner O3; ADR 0014 "Amendment
//! (U7)"), through the real `memprove` and `memproved`: the `Session `
//! line of a Session that has not ended ends with how to end it before
//! taking over, unless it is the reader's own, the Session the context the
//! reader uses names:
//!
//! - `resume --pack` in the directory's context, and in a client session's
//!   (`MEMPROVE_CLIENT_SESSION`);
//! - the `SessionStart` hook: a new client session has no own Session; a
//!   client session that began one (`compact`) has;
//! - the MCP `pack` tool: the server's own context (rule D, step 1);
//! - `begin` refused while the context's Session is active names
//!   `memprove end --abandoned --reason <why>`.
//!
//! The Capsule JSON holds no such text, and the `Sessions: ` line is the
//! same for every reader.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    io::Write as _,
    process::{Command, Output, Stdio},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{Fixture, cli_binary, isolated, text};

const HINT: &str = "; end it before taking over: ";

/// U10 T7 (Owner O4): the hint at the end of the `Session` line `line`, by its
/// number and Session ID: `memprove` when the CLI's canonical directory
/// is on `PATH`, else the CLI's canonical path.
fn hint_of(line: &str) -> String {
    let words = line.split(' ').collect::<Vec<_>>();
    let cli = std::fs::canonicalize(cli_binary()).unwrap();
    let dir = cli.parent().unwrap();
    let on_path = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .any(|entry| std::fs::canonicalize(&entry).ok().as_deref() == Some(dir))
    });
    let program = if on_path {
        "memprove".to_owned()
    } else {
        cli.to_str().unwrap().to_owned()
    };
    format!(
        "{HINT}{program} session end {} --abandoned --reason <why> (MCP: end {{session:{}, \
         abandoned:true}})",
        words[2], words[1]
    )
}

/// The agent's CLI in the repository, as client session `client` (none:
/// the directory's context).
fn agent_as(fixture: &Fixture, client: Option<&str>, words: &[&str]) -> Output {
    let mut command = agent_command(fixture, client);
    command
        .arg("--data-dir")
        .arg(&fixture.data)
        .arg("--token-file")
        .arg(&fixture.agent)
        .args(words);
    command.output().unwrap()
}

fn agent_command(fixture: &Fixture, client: Option<&str>) -> Command {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config);
    if let Some(client) = client {
        command.env("MEMPROVE_CLIENT_SESSION", client);
    }
    command
}

/// The `Session ` lines of a Pack, keyed by their number, and whether each
/// ends with the hint; the `Sessions: ` line.
fn sessions(pack: &str) -> (String, Vec<(String, bool)>) {
    let summary = pack
        .lines()
        .find(|line| line.starts_with("Sessions: "))
        .unwrap()
        .to_owned();
    let lines = pack
        .lines()
        .filter(|line| line.starts_with("Session "))
        .map(|line| {
            let number = line.split(' ').nth(1).unwrap().to_owned();
            (number, line.ends_with(&hint_of(line)))
        })
        .collect();
    (summary, lines)
}

fn hinted(pack: &str, expected: &[(&str, bool)]) -> String {
    let (summary, lines) = sessions(pack);
    let expected = expected
        .iter()
        .map(|(number, hint)| ((*number).to_owned(), *hint))
        .collect::<Vec<_>>();
    assert_eq!(lines, expected, "{pack}");
    // Only the Session lines may hold the hint.
    assert_eq!(
        pack.matches(HINT).count(),
        expected.iter().filter(|(_, hint)| *hint).count(),
        "{pack}"
    );
    summary
}

/// `hook session-start` for client session `client` at `source`.
fn hook(fixture: &Fixture, client: &str, source: &str) -> String {
    let mut child = isolated(cli_binary())
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &fixture.data)
        .args(["hook", "session-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"{client}","source":"{source}","cwd":"{}"}}"#,
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

/// The text of the MCP `pack` tool, one modern request, as client session
/// `client` (none: the directory's context).
fn mcp_pack(fixture: &Fixture, client: Option<&str>) -> String {
    let mut command = agent_command(fixture, client);
    let mut child = command
        .env("MEMPROVE_DATA_DIR", &fixture.data)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}},"name":"pack","arguments":{}}}"#;
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let line = text(&output.stdout);
    let reply = parse_strict(line.lines().next().unwrap()).unwrap();
    let content = reply
        .get("result")
        .and_then(|result| result.get("content"))
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("{line}"));
    // U12 T2 (Owner O4): the opening first, then the Pack.
    assert_eq!(content.len(), 3, "{line}");
    assert!(
        content[0]
            .get("text")
            .and_then(JsonValue::as_str)
            .is_some_and(
                |opening| opening.starts_with("memprove: below is the Context Pack of Task ")
            ),
        "{line}"
    );
    content[1]
        .get("text")
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned()
}

fn ok(output: &Output, what: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what}: {}",
        text(&output.stderr)
    );
}

#[test]
fn u7_p1_only_an_open_session_that_is_not_the_readers_own_gets_the_takeover_hint() {
    let fixture = Fixture::new();
    // Session 1: the directory's context, left open.
    let task = fixture.task_with_fact("P1 takeover", "the check passes");
    // Session 2: client session client-a begins its own and leaves it open.
    let begun = agent_as(
        &fixture,
        Some("client-a"),
        &["begin", "--project", &fixture.project, "--task", &task],
    );
    ok(&begun, "begin as client-a");

    // resume --pack: the directory's context owns Session 1, client-a owns
    // Session 2, client-b owns none.
    let directory = text(&agent_as(&fixture, None, &["resume", "--pack"]).stdout);
    let summary = hinted(&directory, &[("1", false), ("2", true)]);
    let a = text(&agent_as(&fixture, Some("client-a"), &["resume", "--pack"]).stdout);
    assert_eq!(hinted(&a, &[("1", true), ("2", false)]), summary);
    let b = agent_as(
        &fixture,
        Some("client-b"),
        &["resume", "--task", &task, "--pack"],
    );
    ok(&b, "resume as client-b");
    assert_eq!(
        hinted(&text(&b.stdout), &[("1", true), ("2", true)]),
        summary
    );
    // The query Pack follows the same reader.
    let query = text(
        &agent_as(
            &fixture,
            Some("client-a"),
            &["resume", "--pack", "--query", "check"],
        )
        .stdout,
    );
    hinted(&query, &[("1", true), ("2", false)]);
    // The Capsule JSON holds no such text.
    let capsule = text(&agent_as(&fixture, None, &["resume"]).stdout);
    assert!(!capsule.contains("end it before taking over"), "{capsule}");

    // The hook: a new client session owns no Session; client-a, after its
    // context was compacted, owns Session 2.
    let startup = hook(&fixture, "client-c", "startup");
    hinted(&startup, &[("1", true), ("2", true)]);
    let compact = hook(&fixture, "client-a", "compact");
    hinted(&compact, &[("1", true), ("2", false)]);
    let fork = hook(&fixture, "client-d", "fork");
    hinted(&fork, &[("1", true), ("2", true)]);
    assert!(
        fork.contains("while the parent session is still running, the guidance at the end of that line does not apply to it."),
        "{fork}"
    );

    // The MCP pack tool: the server's own context.
    let served = mcp_pack(&fixture, Some("client-a"));
    hinted(&served, &[("1", true), ("2", false)]);
    let served = mcp_pack(&fixture, None);
    hinted(&served, &[("1", false), ("2", true)]);

    // begin while the directory context's Session is active names the
    // abandoned end for a Session someone else left.
    let refused = agent_as(&fixture, None, &["begin"]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr));
    let stderr = text(&refused.stderr);
    assert!(stderr.contains("still active"), "{stderr}");
    assert!(
        stderr.contains(
            "a Session someone else left open is ended with `memprove end --abandoned \
             --reason <why>`"
        ),
        "{stderr}"
    );

    // Once Session 1 ended, no line names it as open, and Session 2 is the only
    // one a reader other than client-a is told to end.
    ok(&agent_as(&fixture, None, &["end"]), "end Session 1");
    let after = text(&agent_as(&fixture, None, &["resume", "--task", &task, "--pack"]).stdout);
    hinted(&after, &[("1", false), ("2", true)]);
    fixture.stop();
}
