//! U1 stage U-A (ADR 0014, "Amendment (U1, U-A)") through the real
//! binaries: the client session of `memprove mcp serve` in the hook's
//! layout. A1 (Owner O2): without `MEMPROVE_CLIENT_SESSION` the server
//! takes `CLAUDE_CODE_SESSION_ID` (which Claude Code sets for a stdio
//! server) as its client session, so the server and the shell of one Claude
//! Code session use the same context file; an explicit `session` argument
//! comes first, then `MEMPROVE_CLIENT_SESSION`, then
//! `CLAUDE_CODE_SESSION_ID`; a value that is not 1 to 128 characters of
//! `[A-Za-z0-9._:-]` is ignored with one fixed line on standard error.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::Stdio,
};

use p4b_support::{Cli, Run, Setup, cli_binary, isolated, step, text};
use p4g_support::{Called, Mcp, McpEnv, files_under, last_packs};
use serde_json::{Value, json};

/// The server as Claude Code spawns it in `setup`'s repository: the
/// agent's configuration, data directory, and token file, and
/// `CLAUDE_CODE_SESSION_ID` when given.
fn server(setup: &Setup, claude_code_session: Option<&str>) -> Mcp {
    Mcp::start(&McpEnv {
        claude_code_session: claude_code_session.map(str::to_owned),
        ..McpEnv::of(
            &setup.agent.config_dir,
            &setup.agent.data_dir,
            &setup.agent.token,
            &setup.repo,
        )
    })
}

/// The agent's CLI in the shell, as the client session `client` (the value
/// the hook exported) when given.
fn as_client(cli: &Cli, client: Option<&str>, words: &[&str]) -> Run {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&cli.cwd)
        .arg("--data-dir")
        .arg(&cli.data_dir)
        .arg("--token-file")
        .arg(&cli.token)
        .args(words)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null());
    if let Some(client) = client {
        command.env("MEMPROVE_CLIENT_SESSION", client);
    }
    let output = command.output().unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "[client {}] $ ct {}\n{}{}[exit {:?}]",
        client.unwrap_or("-"),
        words.join(" "),
        run.stdout,
        run.stderr,
        run.code
    );
    run
}

fn ok_as(cli: &Cli, client: Option<&str>, words: &[&str]) -> Value {
    let run = as_client(cli, client, words);
    assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
    serde_json::from_str(run.stdout.trim()).unwrap()
}

/// A test run in the shell as `client`; returns its label.
fn test_as(cli: &Cli, client: &str, script: &str) -> String {
    let ran = as_client(
        cli,
        Some(client),
        &["run", "--test-format", "cargo", "--", "sh", script],
    );
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    ran.stderr
        .lines()
        .last()
        .and_then(|line| line.rsplit_once(", label "))
        .unwrap_or_else(|| panic!("no label: {}", ran.stderr))
        .1
        .to_owned()
}

/// `memprove hook session-start` for the client session `client`, as
/// Claude Code runs it at `SessionStart`: it injects a Pack, records it as
/// that client session's last Pack (with the Project), and exports the
/// client session to `CLAUDE_ENV_FILE`.
fn session_start(setup: &Setup, client: &str) {
    let stdout = hook_session_start(setup, client);
    assert!(stdout.contains("```text\nPack: "), "{stdout}");
}

/// The file `client`'s `SessionStart` hook exports its client session to.
fn env_file(setup: &Setup, client: &str) -> PathBuf {
    setup.root_path.join(format!("claude-env-{client}.sh"))
}

/// `memprove hook session-start` for the client session `client`, as
/// Claude Code runs it at `SessionStart`; returns what it printed.
fn hook_session_start(setup: &Setup, client: &str) -> String {
    let cli = &setup.agent;
    let input = json!({
        "session_id": client,
        "transcript_path": "/nowhere/transcript.jsonl",
        "cwd": setup.repo.to_str().unwrap(),
        "hook_event_name": "SessionStart",
        "source": "startup",
    })
    .to_string();
    let mut child = isolated(cli_binary())
        .args(["hook", "session-start"])
        .current_dir(&setup.repo)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .env("MEMPROVE_DATA_DIR", &cli.data_dir)
        .env("MEMPROVE_TOKEN_FILE", &cli.token)
        .env("CLAUDE_ENV_FILE", env_file(setup, client))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    println!(
        "$ memprove hook session-start ({client})\n{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(0));
    text(&output.stdout)
}

/// U1 U-F (SF-1): the hook's last-Pack record for `client`, a value of
/// A1's wider set that the hook does not take: the hook records a Pack for
/// a stand-in client session, and that file is renamed to `client`'s (the
/// files are named by digests).
fn record_as(setup: &Setup, client: &str) {
    let digest = |bytes: &[u8]| memprove_domain::Sha256Digest::of_bytes(bytes).to_string();
    let stand_in = "cc-stand-in";
    session_start(setup, stand_in);
    let root = std::fs::canonicalize(&setup.repo).unwrap();
    let clients = setup
        .agent
        .config_dir
        .join("contexts")
        .join(digest(root.as_os_str().as_encoded_bytes()));
    std::fs::rename(
        clients.join(format!("{}.pack.json", digest(stand_in.as_bytes()))),
        clients.join(format!("{}.pack.json", digest(client.as_bytes()))),
    )
    .unwrap();
}

/// Every context file (not a last-Pack file) under `config_dir`, with its
/// bytes.
fn context_files(config_dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    files_under(&config_dir.join("contexts"))
        .into_iter()
        .filter(|(path, _)| !path.to_string_lossy().ends_with(".pack.json"))
        .collect()
}

/// The context files a step created, changed, or removed.
fn touched(before: &[(PathBuf, Vec<u8>)], after: &[(PathBuf, Vec<u8>)]) -> Vec<PathBuf> {
    let mut paths = before
        .iter()
        .filter(|file| !after.contains(file))
        .chain(after.iter().filter(|file| !before.contains(file)))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

/// One tool call that must succeed; returns the result and the context
/// files it touched.
fn call_touching(
    mcp: &mut Mcp,
    config_dir: &Path,
    tool: &str,
    arguments: &Value,
) -> (Called, Vec<PathBuf>) {
    let before = context_files(config_dir);
    let called = mcp.call(tool, arguments);
    assert!(!called.is_error, "{}", called.line);
    (called, touched(&before, &context_files(config_dir)))
}

/// The operator's first `begin --project` and `end`: the directory's
/// context names the Project, the Workspace, and the Task. Returns the
/// Task.
fn first_task(setup: &Setup, title: &str) -> String {
    let begun = setup
        .agent
        .ok(&["begin", "--project", &setup.project_id, "--new-task", title]);
    setup.agent.bytes(&["end", "--reason", "operator"]);
    begun["task_id"].as_str().unwrap().to_owned()
}

/// A1, process level (Owner O2): one Workspace in the hook's layout, two
/// Claude Code sessions, each with its own `mcp serve` given its own
/// `CLAUDE_CODE_SESSION_ID`. Each `begin`s through MCP, runs a test in its
/// shell (as the hook exported it), `complete`s, and `end`s; each call
/// touches that client session's context file only, and it is the file
/// the shell of the same client session uses.
#[test]
fn u1_a1_two_claude_code_sessions_each_keep_their_own_context_file() {
    let setup = Setup::new(&["a", "b"]);
    let agent = &setup.agent;
    let config = &agent.config_dir;
    let task_x = first_task(&setup, "u1 a1 x");
    let (x, y) = ("cc-session-x", "cc-session-y");
    step("SessionStart of two Claude Code sessions in one Workspace");
    session_start(&setup, x);
    session_start(&setup, y);
    let mut mcp_x = server(&setup, Some(x));
    let mut mcp_y = server(&setup, Some(y));

    step("x begins through MCP: only x's context file is written");
    let (begun_x, files) = call_touching(&mut mcp_x, config, "begin", &json!({}));
    assert_eq!(begun_x.structured["context_file"], "client");
    assert_eq!(begun_x.structured["task_id"], task_x.as_str());
    assert_eq!(files.len(), 1, "{files:?}");
    let file_x = files[0].clone();
    let session_x = begun_x.structured["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let shown = as_client(agent, Some(x), &["context", "show"]);
    assert!(shown.stdout.contains(&session_x), "{}", shown.stdout);

    step("y begins a new Task through MCP: only y's context file is written");
    let (begun_y, files) = call_touching(
        &mut mcp_y,
        config,
        "begin",
        &json!({"new_task": {"title": "u1 a1 y"}}),
    );
    assert_eq!(begun_y.structured["context_file"], "client");
    assert_eq!(files.len(), 1, "{files:?}");
    let file_y = files[0].clone();
    assert_ne!(file_x, file_y);
    let task_y = begun_y.structured["task_id"].as_str().unwrap().to_owned();
    let session_y = begun_y.structured["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(task_x, task_y);
    let shown = as_client(agent, Some(y), &["context", "show"]);
    assert!(shown.stdout.contains(&session_y), "{}", shown.stdout);

    step("each shell runs its test in its own Session");
    test_as(agent, x, "tests/a.sh");
    test_as(agent, y, "tests/b.sh");

    step("each completes its Task: only its own context file changes");
    for (mcp, file, task, session) in [
        (&mut mcp_x, &file_x, &task_x, &session_x),
        (&mut mcp_y, &file_y, &task_y, &session_y),
    ] {
        let (completed, files) = call_touching(mcp, config, "complete", &json!({}));
        assert_eq!(completed.structured["task_id"], task.as_str());
        assert_eq!(completed.structured["session_id"], session.as_str());
        assert_eq!(completed.structured["state"], "completed");
        assert_eq!(files, vec![file.clone()]);
    }

    step("each ends its Session: only its own context file changes");
    for (mcp, file, session) in [
        (&mut mcp_x, &file_x, &session_x),
        (&mut mcp_y, &file_y, &session_y),
    ] {
        let (ended, files) = call_touching(mcp, config, "end", &json!({"reason": "done"}));
        assert_eq!(ended.structured["session_id"], session.as_str());
        assert_eq!(ended.structured["context_cleared"], true);
        assert_eq!(files, vec![file.clone()]);
    }
    for (task, session) in [(&task_x, &session_x), (&task_y, &session_y)] {
        let capsule = agent.ok(&["resume", "--task", task]);
        assert_eq!(capsule["task"]["status"], "completed");
        let ended = capsule["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["session_id"] == session.as_str())
            .unwrap()
            .clone();
        assert_eq!(ended["end_state"], "ended");
    }
    let (_, stderr_x) = mcp_x.finish();
    let (_, stderr_y) = mcp_y.finish();
    for stderr in [stderr_x, stderr_y] {
        assert!(!stderr.contains("warning"), "{stderr}");
    }
}

/// The `session_id` the `context` tool reports for a server started with
/// `client` (`MEMPROVE_CLIENT_SESSION`) and `claude_code`
/// (`CLAUDE_CODE_SESSION_ID`), the file kind it names, and its standard
/// error.
fn context_of(setup: &Setup, client: Option<&str>, claude_code: Option<&str>) -> (Value, String) {
    let mut mcp = Mcp::start(&McpEnv {
        client: client.map(str::to_owned),
        claude_code_session: claude_code.map(str::to_owned),
        ..McpEnv::of(
            &setup.agent.config_dir,
            &setup.agent.data_dir,
            &setup.agent.token,
            &setup.repo,
        )
    });
    let shown = mcp.ok("context", &json!({}));
    let (_, stderr) = mcp.finish();
    (shown.structured, stderr)
}

/// The fixed line about an ignored `CLAUDE_CODE_SESSION_ID`.
const IGNORED_CLAUDE_CODE: &str = "memprove: warning: CLAUDE_CODE_SESSION_ID is not 1 to 128 \
     characters of A-Z, a-z, 0-9, ., _, :, and -; it is ignored, and the context of this \
     directory is used";

/// A1 (Owner O2): the three orders of precedence (an explicit `session`
/// before `MEMPROVE_CLIENT_SESSION` before `CLAUDE_CODE_SESSION_ID`), a
/// value in the wider character set, and values that are ignored with one
/// fixed line on standard error.
#[test]
fn u1_a1_precedence_and_ignored_values() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let task = first_task(&setup, "u1 a1 precedence");
    step("client-a and client-b begin in the shell on the same Task");
    let a = ok_as(
        agent,
        Some("client-a"),
        &["begin", "--project", &setup.project_id, "--task", &task],
    );
    let session_a = a["session_id"].as_str().unwrap().to_owned();
    let b = ok_as(
        agent,
        Some("client-b"),
        &["begin", "--project", &setup.project_id, "--task", &task],
    );
    let session_b = b["session_id"].as_str().unwrap().to_owned();

    step("CLAUDE_CODE_SESSION_ID alone names the client session");
    let (shown, stderr) = context_of(&setup, None, Some("client-a"));
    assert_eq!(shown["context_file"], "client");
    assert_eq!(shown["session_id"], session_a.as_str());
    assert!(!stderr.contains("warning"), "{stderr}");

    step("MEMPROVE_CLIENT_SESSION comes before CLAUDE_CODE_SESSION_ID");
    let (shown, _) = context_of(&setup, Some("client-b"), Some("client-a"));
    assert_eq!(shown["context_file"], "client");
    assert_eq!(shown["session_id"], session_b.as_str());
    let (shown, _) = context_of(&setup, Some("client-c"), Some("client-a"));
    assert_eq!(shown["context_file"], "client");
    assert_eq!(shown["session_id"], Value::Null);

    step("an explicit session comes before both");
    let mut mcp = Mcp::start(&McpEnv {
        client: Some("client-a".to_owned()),
        claude_code_session: Some("client-a".to_owned()),
        ..McpEnv::of(
            &agent.config_dir,
            &agent.data_dir,
            &agent.token,
            &setup.repo,
        )
    });
    let own = mcp.ok("decide", &json!({"text": "own session"}));
    assert_eq!(own.structured["session_id"], session_a.as_str());
    // Session 1 is the operator's; client-a's is 2, client-b's 3.
    let explicit = mcp.ok("decide", &json!({"session": 3, "text": "explicit session"}));
    assert_eq!(explicit.structured["session_id"], session_b.as_str());
    mcp.finish();

    step("a value of the wider character set is a client session of its own");
    let wide = "cc.1:x-Y_z";
    // U1 U-F (SF-1): only once that client session has a record; before,
    // the directory's context, with no warning (the value is not refused).
    let mut mcp = server(&setup, Some(wide));
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "directory");
    record_as(&setup, wide);
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "client");
    assert_eq!(shown.structured["session_id"], Value::Null);
    let (_, stderr) = mcp.finish();
    assert!(!stderr.contains("warning"), "{stderr}");

    step("ignored values: the directory's context, one fixed line each");
    let long = "x".repeat(129);
    for value in ["not/valid", "has space", "", long.as_str(), "é"] {
        let mut mcp = server(&setup, Some(value));
        let first = mcp.ok("context", &json!({}));
        assert_eq!(first.structured["context_file"], "directory", "{value:?}");
        mcp.ok("context", &json!({}));
        let (_, stderr) = mcp.finish();
        assert_eq!(
            stderr
                .lines()
                .filter(|line| *line == IGNORED_CLAUDE_CODE)
                .count(),
            1,
            "{value:?}: {stderr}"
        );
        if value.len() > 1 {
            assert!(!stderr.contains(value), "{stderr}");
        }
    }
    step("an ignored MEMPROVE_CLIENT_SESSION is not replaced by CLAUDE_CODE_SESSION_ID");
    let (shown, stderr) = context_of(&setup, Some("not/valid"), Some("client-a"));
    assert_eq!(shown["context_file"], "directory");
    assert!(
        stderr.contains("MEMPROVE_CLIENT_SESSION is not 1 to 128"),
        "{stderr}"
    );
    assert!(!stderr.contains("CLAUDE_CODE_SESSION_ID"), "{stderr}");
}

/// A4 (V5-N1): the last-Pack record the `pack` tool writes names the
/// Project, so a server with a client session `begin`s right after `pack`,
/// with no context of its own: after the hook (whose record `pack`
/// replaces) and with a client session in the wider set of A1, whose files
/// are named by their digest.
#[test]
fn u1_a4_a_client_session_begins_right_after_pack() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let task = first_task(&setup, "u1 a4");
    for (client, hook) in [("cc-a4", true), ("cc.a4:wide", false)] {
        step(&format!(
            "{client}: pack, then begin, with no context of its own"
        ));
        // U1 U-F (SF-1): the client session needs a record of its own;
        // the hook does not take a value of the wider set.
        if hook {
            session_start(&setup, client);
        } else {
            record_as(&setup, client);
        }
        let mut mcp = server(&setup, Some(client));
        let pack = mcp.ok("pack", &json!({}));
        assert_eq!(pack.structured["task_id"], task.as_str());
        let shown = mcp.ok("context", &json!({}));
        assert_eq!(shown.structured["context_file"], "client");
        assert_eq!(shown.structured["context"], Value::Null);
        assert_eq!(
            shown.structured["last_pack"]["project_id"],
            setup.project_id.as_str()
        );
        let before = context_files(&agent.config_dir);
        let begun = mcp.ok("begin", &json!({}));
        assert_eq!(begun.structured["task_id"], task.as_str());
        assert_eq!(begun.structured["context_file"], "client");
        assert_eq!(
            begun.structured["resumed_from"]["pack_digest"],
            pack.structured["pack_digest"]
        );
        let written = touched(&before, &context_files(&agent.config_dir));
        assert_eq!(written.len(), 1, "{written:?}");
        let name = written[0].file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(name.len(), 64 + ".json".len(), "{name}");
        assert!(
            name[..64].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{name}"
        );
        let ended = mcp.ok("end", &json!({}));
        assert_eq!(ended.structured["context_cleared"], true);
        mcp.finish();
    }
}

/// The fixed sentence of `no_client_session`.
const NO_CLIENT_SESSION: &str = "this directory has a client session context that records a Session, \
     but this server has no client session (neither MEMPROVE_CLIENT_SESSION nor \
     CLAUDE_CODE_SESSION_ID has a usable value), so the context file to write back is undetermined; \
     nothing was sent; memprove end or memprove task complete in that client session's shell \
     does it";

/// The ledger high-water mark of `task`.
fn high_water(setup: &Setup, task: &str) -> u64 {
    p4b_support::sequence(&setup.agent.ok(&["resume", "--task", task]))
}

/// A2, process level (Owner O3): under rule D, `end` and `complete` write
/// back only this client session's context file. In the hook's layout a
/// server with no client session sends nothing and answers
/// `no_client_session`; a server whose own context names no Session does
/// not take another client session's (`no_session_context`); two servers,
/// each with its own `CLAUDE_CODE_SESSION_ID`, each `begin`, `complete`,
/// and `end` on their own file; with one context file per Workspace root
/// (no hook) the directory's file is taken as before.
#[test]
fn u1_a2_end_and_complete_write_back_only_this_client_sessions_file() {
    let setup = Setup::new(&["a", "b", "c"]);
    let agent = &setup.agent;
    let config = &agent.config_dir;
    let task_x = first_task(&setup, "u1 a2 x");
    let (x, y) = ("cc-a2-x", "cc-a2-y");
    session_start(&setup, x);
    session_start(&setup, y);
    step("y begins a new Task in its shell and runs a test");
    let begun_y = ok_as(
        agent,
        Some(y),
        &[
            "begin",
            "--project",
            &setup.project_id,
            "--new-task",
            "u1 a2 y",
        ],
    );
    let task_y = begun_y["task_id"].as_str().unwrap().to_owned();
    let session_y = begun_y["session_id"].as_str().unwrap().to_owned();
    test_as(agent, y, "tests/b.sh");

    step("a server with no client session: end and complete send nothing");
    let mut anonymous = server(&setup, None);
    let before = context_files(config);
    let sequence = high_water(&setup, &task_y);
    for tool in ["complete", "end"] {
        let refused = anonymous.call(tool, &json!({}));
        assert_eq!(
            refused.failure(),
            Some("no_client_session"),
            "{}",
            refused.line
        );
        assert_eq!(refused.text, NO_CLIENT_SESSION);
    }
    assert_eq!(context_files(config), before);
    assert_eq!(high_water(&setup, &task_y), sequence);
    step("checkpoint and decide still find y's Session by rule D");
    let decided = anonymous.call("decide", &json!({"text": "rule d still reads"}));
    assert!(!decided.is_error, "{}", decided.line);
    assert_eq!(decided.structured["session_id"], session_y.as_str());
    anonymous.finish();

    step("x's server, whose own context names no Session, leaves y's alone");
    let mut mcp_x = server(&setup, Some(x));
    let before = context_files(config);
    let sequence = high_water(&setup, &task_y);
    for tool in ["complete", "end"] {
        let refused = mcp_x.call(tool, &json!({}));
        assert_eq!(
            refused.failure(),
            Some("no_session_context"),
            "{}",
            refused.line
        );
    }
    assert_eq!(context_files(config), before);
    assert_eq!(high_water(&setup, &task_y), sequence);
    assert_eq!(
        agent.ok(&["resume", "--task", &task_y])["task"]["status"],
        "in_progress"
    );

    step("x begins, completes, and ends on its own file");
    let (begun_x, files) = call_touching(&mut mcp_x, config, "begin", &json!({}));
    assert_eq!(begun_x.structured["task_id"], task_x.as_str());
    assert_eq!(files.len(), 1, "{files:?}");
    let file_x = files[0].clone();
    test_as(agent, x, "tests/a.sh");
    let (completed, files) = call_touching(&mut mcp_x, config, "complete", &json!({}));
    assert_eq!(completed.structured["task_id"], task_x.as_str());
    assert_eq!(files, vec![file_x.clone()]);
    let (ended, files) = call_touching(&mut mcp_x, config, "end", &json!({}));
    assert_eq!(ended.structured["context_cleared"], true);
    assert_eq!(files, vec![file_x.clone()]);

    step("y's server completes and ends y's Task and Session on y's file");
    let mut mcp_y = server(&setup, Some(y));
    let (completed, files_complete) = call_touching(&mut mcp_y, config, "complete", &json!({}));
    assert_eq!(completed.structured["task_id"], task_y.as_str());
    let (ended, files_end) = call_touching(&mut mcp_y, config, "end", &json!({}));
    assert_eq!(ended.structured["session_id"], session_y.as_str());
    assert_eq!(files_complete, files_end);
    assert_eq!(files_complete.len(), 1);
    assert_ne!(files_complete[0], file_x);
    mcp_y.finish();

    step("one context file per root (no client session's file names a Session)");
    let begun = agent.ok(&["begin", "--new-task", "u1 a2 directory"]);
    let task_d = begun["task_id"].as_str().unwrap().to_owned();
    agent.test("tests/c.sh");
    let (completed, files) = call_touching(&mut mcp_x, config, "complete", &json!({}));
    assert_eq!(completed.structured["task_id"], task_d.as_str());
    assert_eq!(files.len(), 1);
    assert!(!files.contains(&file_x));
    let (ended, again) = call_touching(&mut mcp_x, config, "end", &json!({}));
    assert_eq!(ended.structured["session_id"], begun["session_id"]);
    assert_eq!(again, files);
    mcp_x.finish();
}

/// A3 (original #34): rule D does not count a context file that names a
/// Session which has ended (here ended with `session end` from another
/// context, which leaves that file as it was), so it no longer makes the
/// choice `ambiguous_session_context`.
#[test]
fn u1_a3_rule_d_does_not_count_a_context_of_an_ended_session() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let task = first_task(&setup, "u1 a3");
    step("client-a and client-b begin in the shell on the same Task");
    let mut sessions = Vec::new();
    for client in ["client-a", "client-b"] {
        let begun = ok_as(
            agent,
            Some(client),
            &["begin", "--project", &setup.project_id, "--task", &task],
        );
        sessions.push((
            begun["session_id"].as_str().unwrap().to_owned(),
            begun["workspace_id"].as_str().unwrap().to_owned(),
        ));
    }
    let (session_a, _) = sessions[0].clone();
    let (session_b, workspace) = sessions[1].clone();
    let mut mcp = server(&setup, None);
    let ambiguous = mcp.call("decide", &json!({"text": "two live sessions"}));
    assert_eq!(ambiguous.failure(), Some("ambiguous_session_context"));
    assert_eq!(ambiguous.structured["count"], 2);

    step("client-a ends client-b's Session by its ID; client-b's file still names it");
    let listed = agent.ok(&["session", "list", "--workspace", &workspace]);
    let revision = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["session_id"] == session_b.as_str())
        .unwrap()["revision"]
        .to_string();
    let before = context_files(&agent.config_dir);
    ok_as(
        agent,
        Some("client-a"),
        &[
            "session",
            "end",
            &session_b,
            "--revision",
            &revision,
            "--abandoned",
        ],
    );
    assert_eq!(context_files(&agent.config_dir), before);
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["contexts_with_session"], 2);

    step("rule D counts only client-a's live Session");
    let decided = mcp.call("decide", &json!({"text": "one live session"}));
    assert!(!decided.is_error, "{}", decided.line);
    assert_eq!(decided.structured["session_id"], session_a.as_str());
    let refused = mcp.call("end", &json!({}));
    assert_eq!(
        refused.failure(),
        Some("no_client_session"),
        "{}",
        refused.line
    );
    mcp.finish();

    step("with client-a's Session ended too, rule D finds none");
    let ended = as_client(agent, Some("client-a"), &["end"]);
    assert_eq!(ended.code, Some(0), "{}", ended.stderr);
    let mut mcp = server(&setup, None);
    for tool in ["decide", "end", "complete"] {
        let arguments = if tool == "decide" {
            json!({"text": "none live"})
        } else {
            json!({})
        };
        let none = mcp.call(tool, &arguments);
        assert_eq!(none.failure(), Some("no_session_context"), "{}", none.line);
    }
    mcp.finish();
}

/// UTF-16 code units of `text`.
fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// A5 (original #26, V5-N3): the default output of `claims` and `begin` is
/// at most 6000 UTF-16 code units, the Pack's default budget. Lines that
/// fit are the bytes `resume --pack --claims` prints; longer ones are cut
/// the Pack's way, with an `Omitted: ` line that says what was cut. `begin`
/// cuts a long Task title to its first 120 characters and says its length.
#[test]
fn u1_a5_claims_and_begin_stay_within_the_pack_budget() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    first_task(&setup, "u1 a5 first");
    let mut mcp = server(&setup, None);

    step("begin a Task whose title is 4096 characters outside the BMP");
    let title = "😀".repeat(4096);
    let begun = mcp.ok("begin", &json!({"new_task": {"title": title}}));
    assert!(units(&begun.text) <= 6000, "{}", units(&begun.text));
    let shown_title = begun.structured["context"]["task_title"].as_str().unwrap();
    assert_eq!(shown_title.chars().count(), 120);
    assert_eq!(begun.structured["context"]["task_title_chars"], 4096);
    let task = begun.structured["task_id"].as_str().unwrap().to_owned();
    let context = agent.run(&["context", "show"], true);
    assert!(
        context.stdout.contains(&title),
        "the context file keeps the title"
    );

    step("a short claim: the bytes resume --pack --claims prints");
    agent.test("tests/a.sh");
    mcp.ok(
        "checkpoint",
        &json!({"facts": [{"text": "a passes"}], "next": {"text": "next"}}),
    );
    let short = mcp.ok("claims", &json!({"claims": [1]}));
    let cli = agent.bytes(&["resume", "--task", &task, "--pack", "--claims", "1"]);
    assert_eq!(short.text, cli);
    assert!(!short.text.contains("Omitted: "));

    step("a Decision of 4096 + 4096 non-ASCII characters is cut the Pack's way");
    let text = "é".repeat(4096);
    let rationale = "ü".repeat(4096);
    let decided = mcp.ok("decide", &json!({"text": text, "rationale": rationale}));
    let number = decided.structured["number"].as_u64().unwrap();
    let cli = agent.bytes(&[
        "resume",
        "--task",
        &task,
        "--pack",
        "--claims",
        &number.to_string(),
    ]);
    assert!(units(&cli) > 6000, "{}", units(&cli));
    let long = mcp.ok("claims", &json!({"claims": [number]}));
    assert!(units(&long.text) <= 6000, "{}", units(&long.text));
    assert!(
        long.text.contains("… (body 8192 chars, 8072 omitted)"),
        "{}",
        long.text
    );
    let last = long.text.lines().last().unwrap();
    assert!(
        last.starts_with(
            "Omitted: level 2; bases with files only counted 0, bodies shortened 1, bodies omitted 0, not listed 0; "
        ),
        "{last}"
    );
    assert!(
        last.ends_with("memprove resume --pack --claims <numbers> (such as 3,5-7)"),
        "{last}"
    );

    step("many long claims: the ones that do not fit are numbered, not listed");
    for index in 0..3 {
        mcp.ok(
            "decide",
            &json!({"text": format!("{index}{}", "€".repeat(4000))}),
        );
    }
    let all = mcp.ok("claims", &json!({"claims": [1, 2, 3, 4, 5, 6]}));
    assert!(units(&all.text) <= 6000, "{}", units(&all.text));
    assert!(
        all.text
            .lines()
            .last()
            .unwrap()
            .starts_with("Omitted: level "),
        "{}",
        all.text
    );
    mcp.finish();
}

/// A6 (original #27, V5-N5/V7-N1): when the directory's context names a
/// finished Task and no other Task is in progress, `pack` and `claims`
/// without `task` answer `task_finished` with a sentence about the Task
/// and its state, not about Sessions; `pack {task: n}` still reads it, and
/// `begin` keeps its own sentence.
#[test]
fn u1_a6_a_finished_task_is_named_with_its_state() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u1 a6",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    agent.test("tests/a.sh");
    checkpoint_fact(agent);
    agent.bytes(&["task", "complete"]);
    agent.bytes(&["end", "--reason", "done"]);
    let mut mcp = server(&setup, None);
    let sentence = "the Task recorded by this directory's context is completed, and this Workspace has \
                    no in-progress Task; pack and claims read another Task through the task argument, \
                    and begin starts a new Task through new_task";
    for (tool, arguments) in [("pack", json!({})), ("claims", json!({"claims": [1]}))] {
        let finished = mcp.call(tool, &arguments);
        assert_eq!(
            finished.failure(),
            Some("task_finished"),
            "{}",
            finished.line
        );
        assert_eq!(finished.structured["task_id"], task.as_str());
        assert_eq!(finished.structured["state"], "completed");
        assert_eq!(finished.text, sentence);
        assert!(!finished.text.contains("Session"));
    }
    let read = mcp.ok("pack", &json!({"task": 1}));
    assert_eq!(read.structured["task_id"], task.as_str());
    let refused = mcp.call("begin", &json!({}));
    assert_eq!(refused.failure(), Some("task_finished"));
    assert_eq!(
        refused.text,
        "the Task is completed and takes no new Session; new_task starts new work"
    );
    mcp.finish();
}

/// `checkpoint create` with one fact citing the latest run.
fn checkpoint_fact(agent: &Cli) {
    let run = agent.run(
        &[
            "checkpoint",
            "create",
            "--fact",
            "a passes",
            "--next",
            "done",
        ],
        true,
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
}

/// The client sessions' files (context and last-Pack files) of every
/// Workspace root under `config_dir`: those in a `contexts/<root>/`
/// directory, not the directory's own files.
fn client_files(config_dir: &Path) -> Vec<PathBuf> {
    let contexts = config_dir.join("contexts");
    files_under(&contexts)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| path.parent() != Some(contexts.as_path()))
        .collect()
}

/// U1 U-F (SF-1, the reviewer's sequence): without the hook, a server
/// whose `CLAUDE_CODE_SESSION_ID` has no context file and no last-Pack
/// record uses the directory's context, as at 52847bf: `begin {}` starts a
/// Session on the directory's context (from the operator's
/// `begin --project … --new-task …` in the shell), and `memprove run` in
/// the shell, with no `MEMPROVE_CLIENT_SESSION`, records its Evidence in
/// that Session, which `complete` then cites. No client session's file is
/// written. The check is made on every call: once the hook records a Pack
/// for that client session, the same server uses the client session's
/// files, and keeps using them after the record is gone.
#[test]
fn u1_f_sf1_without_the_hook_a_claude_code_session_uses_the_directory_context() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let config = &agent.config_dir;
    let task = first_task(&setup, "u1 f sf1");
    let client = "cc-no-hook";
    let mut mcp = server(&setup, Some(client));

    step("no record of the client session: context names the directory's file");
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "directory");
    assert_eq!(shown.structured["lookup"], "found");

    step("begin {} starts a Session on the directory's context");
    let (begun, files) = call_touching(&mut mcp, config, "begin", &json!({}));
    assert_eq!(begun.structured["context_file"], "directory");
    assert_eq!(begun.structured["task_id"], task.as_str());
    let session = begun.structured["session_id"].as_str().unwrap().to_owned();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].parent(), Some(config.join("contexts").as_path()));
    assert_eq!(client_files(config), Vec::<PathBuf>::new());

    step("the shell, with no MEMPROVE_CLIENT_SESSION, runs a test in that Session");
    let shown = as_client(agent, None, &["context", "show"]);
    assert!(shown.stdout.contains(&session), "{}", shown.stdout);
    let ran = as_client(
        agent,
        None,
        &["run", "--test-format", "cargo", "--", "sh", "tests/a.sh"],
    );
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);

    step("complete cites that run; end clears the directory's context");
    let (completed, files_complete) = call_touching(&mut mcp, config, "complete", &json!({}));
    assert_eq!(completed.structured["task_id"], task.as_str());
    assert_eq!(completed.structured["session_id"], session.as_str());
    assert_eq!(completed.structured["state"], "completed");
    let (ended, files_end) = call_touching(&mut mcp, config, "end", &json!({}));
    assert_eq!(ended.structured["session_id"], session.as_str());
    assert_eq!(ended.structured["context_cleared"], true);
    assert_eq!(files_complete, files);
    assert_eq!(files_end, files);
    assert_eq!(client_files(config), Vec::<PathBuf>::new());
    let capsule = agent.ok(&["resume", "--task", &task]);
    assert_eq!(capsule["task"]["status"], "completed");

    step("the hook records a Pack for the client session: the next call uses it");
    agent.ok(&["begin", "--new-task", "u1 f sf1 hook"]);
    agent.bytes(&["end", "--reason", "operator"]);
    session_start(&setup, client);
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "client");
    assert_eq!(shown.structured["lookup"], "unbegun");

    step("once used, the client session's files stay this server's");
    for (path, _) in last_packs(config) {
        std::fs::remove_file(path).unwrap();
    }
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "client");
    assert_eq!(shown.structured["lookup"], "absent");
    let (_, stderr) = mcp.finish();
    assert!(!stderr.contains("warning"), "{stderr}");

    step("a new server of that client session, with no record left: the directory's");
    let mut again = server(&setup, Some(client));
    let shown = again.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "directory");
    again.finish();
}

/// U1 U-F (SF-1R, the re-check's N-1): in the hook's layout, the hook of a
/// client session injects no Pack (the directory's context names a
/// completed Task and no other Task is in progress) but still exports the
/// client session to the shell. It writes that client session's last-Pack
/// record as a record of no Pack, with the Project, so the server given
/// the same `CLAUDE_CODE_SESSION_ID` uses the client session's files as
/// the shell does: `begin {new_task}` writes the client session's context
/// file, declares no Pack, and the shell's `memprove run` as that client
/// session records its Evidence in that Session, which `complete` cites.
/// The directory's context file is left as it was.
#[test]
fn u1_f_sf1r_a_hook_that_injects_no_pack_still_records_the_client_session() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let config = &agent.config_dir;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u1 f sf1r done",
    ]);
    let finished = begun["task_id"].as_str().unwrap().to_owned();
    agent.test("tests/a.sh");
    checkpoint_fact(agent);
    agent.bytes(&["task", "complete"]);
    agent.bytes(&["end", "--reason", "done"]);
    let client = "cc-hook-no-pack";

    step("the hook names the completed Task, injects no Pack, exports the client session");
    let stdout = hook_session_start(&setup, client);
    assert!(stdout.contains("is completed"), "{stdout}");
    assert!(!stdout.contains("```text"), "{stdout}");
    let exported = std::fs::read_to_string(env_file(&setup, client)).unwrap();
    assert_eq!(
        exported.lines().last(),
        Some(format!("export MEMPROVE_CLIENT_SESSION='{client}'").as_str())
    );
    let directory_files = context_files(config);

    step("the server of that client session uses the client session's files");
    let mut mcp = server(&setup, Some(client));
    let shown = mcp.ok("context", &json!({}));
    assert_eq!(shown.structured["context_file"], "client", "{}", shown.line);
    assert_eq!(shown.structured["lookup"], "absent");
    assert_eq!(shown.structured["last_pack"], Value::Null);

    step("begin {new_task} writes the client session's context file, declaring no Pack");
    let (begun, files) = call_touching(
        &mut mcp,
        config,
        "begin",
        &json!({"new_task": {"title": "u1 f sf1r next"}}),
    );
    assert_eq!(begun.structured["context_file"], "client");
    assert_eq!(begun.structured["resumed_from"], Value::Null);
    let session = begun.structured["session_id"].as_str().unwrap().to_owned();
    let task = begun.structured["task_id"].as_str().unwrap().to_owned();
    assert_ne!(task, finished);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(client_files(config).contains(&files[0]), "{files:?}");

    step("the shell, as that client session, runs a test in that Session");
    let shown = as_client(agent, Some(client), &["context", "show"]);
    assert!(shown.stdout.contains(&session), "{}", shown.stdout);
    let label = test_as(agent, client, "tests/a.sh");

    step("complete cites that run in that Session; the directory's file is unchanged");
    let completed = mcp.ok("complete", &json!({}));
    assert_eq!(completed.structured["task_id"], task.as_str());
    assert_eq!(completed.structured["session_id"], session.as_str());
    assert_eq!(completed.structured["cited"], json!([label]));
    let ended = mcp.ok("end", &json!({}));
    assert_eq!(ended.structured["session_id"], session.as_str());
    let (_, stderr) = mcp.finish();
    assert!(!stderr.contains("warning"), "{stderr}");
    let directory_now = context_files(config)
        .into_iter()
        .filter(|(path, _)| path.parent() == Some(config.join("contexts").as_path()))
        .collect::<Vec<_>>();
    assert_eq!(directory_now, directory_files);
}
