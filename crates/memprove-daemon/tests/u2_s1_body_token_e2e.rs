//! U2 stage W-B, item S1 (N-3, original #129, Owner O4; ADR 0014,
//! "Amendment (U2, W-B)") through the real binaries: a statement body that
//! holds a string in the format of a memprove token, sent through the MCP
//! server's `checkpoint` and `decide` and through the CLI.
//!
//! - The MCP tool error relays the daemon Problem's `detail`, which names
//!   the member, and its fixed sentence says that a body must not hold a
//!   token.
//! - The CLI's local usage error names a body by its option and ordinal
//!   (`--fact #1`), never by its text.
//! - Nothing the binaries write (the server's standard output and error,
//!   the CLI's standard output and error, the daemon's log) holds the token.
//!
//! The token-shaped string is made up here; it is never a real secret.

mod p4b_support;
mod p4g_support;

use std::fs;

use p4b_support::{Run, Setup, step};
use p4g_support::{Mcp, McpEnv};
use serde_json::json;

/// 43 characters of `A-Z`, `a-z`, `0-9`, `-`, and `_`: the format of a
/// memprove token (C4).
const TOKEN: &str = "u2S1-tokenShaped_0123456789abcdefghijklmnop";

fn no_token(what: &str, text: &str) {
    assert!(!text.contains(TOKEN), "{what} holds the token:\n{text}");
}

/// The CLI run quietly (the test transcript would hold the token).
fn quiet(setup: &Setup, words: &[&str]) -> Run {
    let run = setup.agent.run(words, true);
    println!(
        "[agent] $ ct {} (body withheld)\n{}{}[exit {:?}]",
        words.first().copied().unwrap_or_default(),
        run.stdout,
        run.stderr,
        run.code
    );
    no_token("the CLI's standard output", &run.stdout);
    no_token("the CLI's standard error", &run.stderr);
    run
}

#[test]
#[allow(clippy::too_many_lines)] // Both paths, five members, then the logs.
fn u2_s1_a_body_with_a_token_is_refused_by_member_and_never_shown() {
    assert_eq!(TOKEN.len(), 43);
    let mut setup = Setup::new(&["a"]);
    let sentence = format!("the key is {TOKEN}, rotate it");

    step("CLI: the local usage error names the body by option and ordinal");
    setup.agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u2 s1",
    ]);
    // The context records no run yet, so a --fact without --evidence is a
    // usage error before anything is sent.
    let usage = quiet(
        &setup,
        &[
            "checkpoint",
            "create",
            "--open",
            "o",
            "--fact",
            &sentence,
            "--next",
            "n",
        ],
    );
    assert_eq!(usage.code, Some(2));
    assert!(
        usage
            .stderr
            .contains("--fact #1 needs at least one --evidence"),
        "{}",
        usage.stderr
    );
    let r1 = setup.agent.test("tests/a.sh");

    step("CLI: the daemon's 400 names the member");
    for (words, member) in [
        (
            vec![
                "checkpoint",
                "create",
                "--fact",
                "a passes",
                "--fact",
                sentence.as_str(),
                "--evidence",
                r1.as_str(),
                "--next",
                "n",
            ],
            "$.verified_completed_work[1].text",
        ),
        (
            vec!["checkpoint", "create", "--next", sentence.as_str()],
            "$.next_action.text",
        ),
        (
            vec!["decide", "plan a", "--rationale", sentence.as_str()],
            "$.rationale",
        ),
    ] {
        let refused = quiet(&setup, &words);
        assert_eq!(refused.code, Some(1), "{}", refused.stderr);
        assert!(
            refused.stderr.contains(&format!(
                "{member} holds a string in the format of a memprove token"
            )),
            "{member}: {}",
            refused.stderr
        );
    }

    step("MCP: the tool error relays the detail and says a body holds no token");
    let mut mcp = Mcp::start(&McpEnv::of(
        &setup.agent.config_dir,
        &setup.agent.data_dir,
        &setup.agent.token,
        &setup.repo,
    ));
    for (tool, arguments, member) in [
        (
            "checkpoint",
            json!({"facts": [{"text": sentence, "evidence": [r1]}], "next": {"text": "n"}}),
            "$.verified_completed_work[0].text",
        ),
        (
            "checkpoint",
            json!({"open_items": [{"text": "o"}, {"text": sentence}], "next": {"text": "n"}}),
            "$.open_items[1].text",
        ),
        (
            "checkpoint",
            json!({"next": {"text": TOKEN}}),
            "$.next_action.text",
        ),
        ("decide", json!({"text": sentence}), "$.content"),
        (
            "decide",
            json!({"text": "plan a", "rationale": TOKEN}),
            "$.rationale",
        ),
    ] {
        let called = mcp.call(tool, &arguments);
        assert!(called.is_error, "{}", called.line);
        assert_eq!(called.failure(), Some("problem"), "{}", called.line);
        assert_eq!(called.structured["status"], 400);
        assert_eq!(called.structured["code"], "invalid_request");
        no_token("the tool error", &called.line);
        assert!(
            called.text.contains("a body holds no token"),
            "{}",
            called.text
        );
        assert!(
            called.text.contains(&format!(
                "memproved says: {member} holds a string in the format of a memprove token"
            )),
            "{tool} {member}: {}",
            called.text
        );
    }
    let (stdout, stderr) = mcp.finish();
    no_token("the server's standard output", &stdout);
    no_token("the server's standard error", &stderr);

    step("nothing was recorded, and the daemon's log holds no token");
    let capsule = setup.agent.capsule();
    assert!(
        capsule["claims"].as_array().unwrap().is_empty(),
        "{capsule}"
    );
    setup.stop();
    for entry in fs::read_dir(&setup.root_path).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "log") {
            no_token(
                &path.display().to_string(),
                &fs::read_to_string(&path).unwrap(),
            );
        }
    }
}
