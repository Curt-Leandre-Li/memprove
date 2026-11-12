//! U3 stage X-K, item K5 (V-N-4, original #66, Owner O4; ADR 0014,
//! "Amendment (U3, X-K)", extending C4) through the real binaries: a string
//! in the format of a memprove token is refused, before anything is
//! persisted and with C4's fixed detail naming the member, besides the
//! statement bodies also in a Task's title
//! and description (`begin --new-task`, `--description`, `task create`,
//! the `begin` tool's `new_task`), a Checkpoint's objective (`checkpoint
//! create --objective`, the `checkpoint` tool's `objective`), the reason of
//! an end (`end --reason`, the `end` tool's `reason`), and the argv of a
//! run, which `memprove run` refuses without running the command (exit
//! 2, as C2). The token is placed at the start, in the middle, and at the
//! end; a run of 42 or 44 such characters is accepted everywhere. No
//! output of the CLI, the MCP server, or memproved, and no database
//! file, holds the token afterwards.

mod p4b_support;
mod p4g_support;

use std::{fs, path::Path};

use p4b_support::{Run, Setup, step};
use p4g_support::{Called, Mcp, McpEnv};
use serde_json::json;

/// A string in the format of a token: 43 characters of `A-Z`, `a-z`,
/// `0-9`, `-`, and `_`.
const TOKEN: &str = "u3K5_tokenShaped-0123456789abcdefghijklmnop";
/// C4's fixed detail after the member (as amended in U5 P2).
const DETAIL: &str = "holds a string in the format of a memprove token (43 characters of A-Z, \
                      a-z, 0-9, '-', and '_', with at least one upper-case letter, one \
                      lower-case letter, and one digit) or a token this memproved issued; \
                      nothing was recorded";

/// The token at the start, in the middle, and at the end of a text.
fn placed() -> [String; 3] {
    [
        format!("{TOKEN} at the start"),
        format!("in the {TOKEN} middle"),
        format!("at the end={TOKEN}"),
    ]
}

/// Look-alikes that are not a token: runs of 42 and 44 such characters
/// (neither holds the token itself, so the byte search below stays exact).
fn look_alikes() -> [String; 2] {
    [
        format!("short {}", &TOKEN[..42]),
        format!("long {}YZ", &TOKEN[1..]),
    ]
}

/// The Project's ledger sequence, as `ledger verify` reports it.
fn sequence(setup: &Setup) -> u64 {
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
    verified["project_sequence"].as_u64().unwrap()
}

/// A refusal of memproved through the CLI: exit 1, the member named with
/// C4's detail, the token in neither stream.
fn refused(run: &Run, member: &str) {
    assert_eq!(run.code, Some(1), "{}{}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains(&format!("{member} {DETAIL}")),
        "{member}: {}",
        run.stderr
    );
    assert!(!run.stdout.contains(TOKEN), "{}", run.stdout);
    assert!(!run.stderr.contains(TOKEN), "{}", run.stderr);
}

/// A refusal of memproved through a tool: `problem`, `400
/// invalid_request`, the member named in the relayed detail.
fn tool_refused(called: &Called, member: &str) {
    assert!(called.is_error, "{}", called.line);
    assert_eq!(called.failure(), Some("problem"), "{}", called.line);
    assert_eq!(called.structured["status"], 400, "{}", called.line);
    assert_eq!(
        called.structured["code"], "invalid_request",
        "{}",
        called.line
    );
    assert!(
        called
            .text
            .contains(&format!("memproved says: {member} {DETAIL}")),
        "{member}: {}",
        called.text
    );
    assert!(!called.line.contains(TOKEN), "{}", called.line);
}

/// No file under `root` holds the token (databases, logs, contexts).
fn nowhere_under(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !bytes
                        .windows(TOKEN.len())
                        .any(|window| window == TOKEN.as_bytes()),
                    "{} holds the token",
                    path.display()
                );
            }
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered scenario: each field, then the controls.
fn u3_k5_the_cli_refuses_a_token_in_each_free_text_field() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u3 k5",
    ]);
    let r1 = agent.test("tests/a.sh");
    let marker = setup.root_path.join("ran");
    let touch = format!("touch {}", marker.display());
    let before = sequence(&setup);

    step("while the Session is active: objective, end reason, run argv, task create");
    for text in placed() {
        refused(
            &agent.run(
                &[
                    "checkpoint",
                    "create",
                    "--objective",
                    &text,
                    "--fact",
                    "a passes",
                    "--evidence",
                    &r1,
                    "--next",
                    "n",
                ],
                false,
            ),
            "$.objective",
        );
        refused(&agent.run(&["end", "--reason", &text], false), "$.reason");
        refused(
            &agent.run(
                &[
                    "task",
                    "create",
                    "--project",
                    &setup.project_id,
                    "--title",
                    &text,
                ],
                false,
            ),
            "$.title",
        );
        refused(
            &agent.run(
                &[
                    "task",
                    "create",
                    "--project",
                    &setup.project_id,
                    "--title",
                    "t",
                    "--description",
                    &text,
                ],
                false,
            ),
            "$.description",
        );
        let run = agent.run(&["run", "--", "sh", "-c", &touch, "sh", &text], false);
        assert_eq!(run.code, Some(2), "{}", run.stderr);
        assert!(
            run.stderr
                .contains("run refuses a command with a string in the format of a memprove token"),
            "{}",
            run.stderr
        );
        assert!(!run.stdout.contains(TOKEN) && !run.stderr.contains(TOKEN));
        assert!(!marker.exists(), "the command was run");
    }
    assert_eq!(sequence(&setup), before, "nothing was written");

    step("with no active Session: begin --new-task and --description");
    agent.ok(&["end", "--reason", "done"]);
    let before = sequence(&setup);
    for text in placed() {
        refused(
            &agent.run(&["begin", "--new-task", &text], false),
            "$.title",
        );
        refused(
            &agent.run(&["begin", "--new-task", "t", "--description", &text], false),
            "$.description",
        );
    }
    assert_eq!(sequence(&setup), before, "nothing was written");
    let tasks = setup
        .operator
        .ok(&["task", "list", "--project", &setup.project_id]);
    assert_eq!(tasks["items"].as_array().unwrap().len(), 1, "{tasks}");

    step("look-alikes of 42 and 44 characters are accepted in every field");
    for text in look_alikes() {
        agent.ok(&["begin", "--new-task", &text, "--description", &text]);
        let run = agent.run(&["run", "--", "sh", "-c", "true", "sh", &text], false);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        let r = agent.test("tests/a.sh");
        agent.ok(&[
            "checkpoint",
            "create",
            "--objective",
            &text,
            "--fact",
            "a passes",
            "--evidence",
            &r,
            "--next",
            "n",
        ]);
        agent.ok(&["end", "--reason", &text]);
    }

    step("no file (databases, contexts, the daemon's log) holds the token");
    nowhere_under(&setup.root_path);
}

#[test]
fn u3_k5_the_tools_refuse_a_token_in_each_free_text_field() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u3 k5 tools",
    ]);
    agent.ok(&["end", "--reason", "done"]);
    let mut mcp = Mcp::start(&McpEnv::of(
        &agent.config_dir,
        &agent.data_dir,
        &agent.token,
        &setup.repo,
    ));
    let before = sequence(&setup);

    step("the begin tool: new_task's title and description");
    for text in placed() {
        let called = mcp.call("begin", &json!({"new_task": {"title": text}}));
        tool_refused(&called, "$.title");
        let called = mcp.call(
            "begin",
            &json!({"new_task": {"title": "t", "description": text}}),
        );
        tool_refused(&called, "$.description");
    }
    assert_eq!(sequence(&setup), before, "nothing was written");

    step("the checkpoint tool's objective and the end tool's reason");
    mcp.ok("begin", &json!({"task": 1}));
    let r2 = agent.test("tests/a.sh");
    let before = sequence(&setup);
    for text in placed() {
        let called = mcp.call(
            "checkpoint",
            &json!({
                "objective": text,
                "facts": [{"text": "a passes", "evidence": [r2]}],
                "next": {"text": "n"},
            }),
        );
        tool_refused(&called, "$.objective");
        let called = mcp.call("end", &json!({"reason": text}));
        tool_refused(&called, "$.reason");
    }
    assert_eq!(sequence(&setup), before, "nothing was written");

    step("look-alikes are accepted");
    let [short, long] = look_alikes();
    mcp.ok(
        "checkpoint",
        &json!({
            "objective": short,
            "facts": [{"text": "a passes", "evidence": [r2]}],
            "next": {"text": "n"},
        }),
    );
    mcp.ok("end", &json!({"reason": long}));
    let (stdout, stderr) = mcp.finish();
    assert!(!stdout.contains(TOKEN), "the server's standard output");
    assert!(!stderr.contains(TOKEN), "the server's standard error");
    nowhere_under(&setup.root_path);
}
