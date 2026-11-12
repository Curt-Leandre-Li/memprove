//! U5 P2 (D1 F2, Owner O4; ADR 0014 C4, "Amendment (U5, Z-P)"): through
//! the real binaries, the token rule of C4 and K5 no longer refuses D1's
//! test name `test_calculate_metrics_rejects_empty_series` (43 characters,
//! no upper-case letter, no digit) in a fact, an open item, a Task title, or
//! an argv; a run of 43 with an upper-case letter, a lower-case letter, and a
//! digit is still refused, and so is the agent's own token, issued by this
//! daemon. The CLI's hint after the refusal says the rule and not
//! `--evidence`; `--query` and the MCP `pack` tool's `query` follow the same
//! shape, with the fixed sentences that say it.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use p4b_support::{Setup, sequence, step};
use p4g_support::{Mcp, McpEnv};
use serde_json::json;

const D1_TEST_NAME: &str = "test_calculate_metrics_rejects_empty_series";
const MIXED: &str = "Ab3_cDe-4fGh5iJk6lMn7oPq8rSt9uVw0xYz1AbCd2E";
const HINT: &str = "memprove: hint: the text named above holds a string memproved takes \
                    for a memprove token: a run of exactly 43 characters of A-Z, a-z, 0-9, \
                    '-', and '_' (nothing else of them next to it) with at least one \
                    upper-case letter, one lower-case letter, and one digit, or a token this \
                    memproved issued, whatever its characters. Write that part another way (a \
                    test name such as test_calculate_metrics_rejects_empty_series is not one) \
                    and send it again; nothing was recorded.";

#[test]
#[allow(clippy::too_many_lines)] // Each path, accepted and refused.
fn u5_p2_a_test_name_of_43_characters_is_accepted_and_a_token_is_not() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    assert_eq!(D1_TEST_NAME.len(), 43);
    assert_eq!(MIXED.len(), 43);

    step("D1's test name in a Task title, an argv, a fact, and an open item");
    let title = format!("fix {D1_TEST_NAME}");
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        &title,
    ]);
    let (label, evidence) = agent.run_command(
        &["--test-format", "cargo"],
        &["sh", "tests/a.sh", D1_TEST_NAME],
    );
    assert_eq!(evidence["payload"]["argv"][2], D1_TEST_NAME, "{evidence}");
    let fact = format!("{D1_TEST_NAME} passes");
    let open = format!("{D1_TEST_NAME} needs a docstring");
    agent.bytes(&[
        "checkpoint",
        "create",
        "--fact",
        &fact,
        "--evidence",
        &label,
        "--open",
        &open,
        "--next",
        "go on",
    ]);
    let capsule = agent.capsule();
    let texts = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|claim| claim["text"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(texts.contains(&fact) && texts.contains(&open), "{texts:?}");
    assert_eq!(capsule["task"]["title"], title.as_str());
    let pack = agent.bytes(&["resume", "--pack", "--query", D1_TEST_NAME]);
    assert!(pack.contains("; 2 matches"), "{pack}");

    step("a run of the three classes in a fact is refused, with the rule as the hint");
    let before = sequence(&agent.capsule());
    let refused = agent.run(
        &[
            "checkpoint",
            "create",
            "--fact",
            &format!("{MIXED} passes"),
            "--evidence",
            &label,
            "--next",
            "go on",
        ],
        false,
    );
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    assert!(
        refused.stderr.contains(
            "400 invalid_request: $.verified_completed_work[0].text holds a string in the format \
             of a memprove token (43 characters of A-Z, a-z, 0-9, '-', and '_', with at least \
             one upper-case letter, one lower-case letter, and one digit) or a token this \
             memproved issued; nothing was recorded"
        ),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains(HINT), "{}", refused.stderr);
    assert!(!refused.stderr.contains("--evidence"), "{}", refused.stderr);
    assert!(!refused.stderr.contains(MIXED) && !refused.stdout.contains(MIXED));
    assert_eq!(sequence(&agent.capsule()), before, "nothing was recorded");

    step("the agent's own token, which this daemon issued, is refused in an open item");
    let token = std::fs::read_to_string(&agent.token).unwrap();
    let token = token.trim();
    assert_eq!(token.len(), 43);
    let refused = agent.run(
        &[
            "checkpoint",
            "create",
            "--open",
            &format!("the key is {token}"),
            "--next",
            "go on",
        ],
        true,
    );
    assert_eq!(refused.code, Some(1));
    assert!(
        refused
            .stderr
            .contains("$.open_items[0].text holds a string in the format of a memprove token"),
        "the refusal"
    );
    assert!(refused.stderr.contains(HINT));
    assert!(!refused.stderr.contains(token) && !refused.stdout.contains(token));
    assert_eq!(sequence(&agent.capsule()), before, "nothing was recorded");

    step("run refuses a run of the three classes before it runs; --query too");
    let refused = agent.run(&["run", "--", "sh", "tests/a.sh", MIXED], false);
    assert_eq!(refused.code, Some(2));
    assert!(
        refused.stderr.contains(
            "run refuses a command with a string in the format of a memprove token (43 \
             characters of A-Z, a-z, 0-9, '-', and '_', with at least one upper-case letter, \
             one lower-case letter, and one digit) in the program or in any argument; the \
             command was not run and nothing was sent"
        ),
        "{}",
        refused.stderr
    );
    let refused = agent.run(&["resume", "--pack", "--query", MIXED], false);
    assert_eq!(refused.code, Some(2));
    assert!(
        refused.stderr.contains(
            "--query refuses a query with a string in the format of a memprove token (43 \
             characters of A-Z, a-z, 0-9, '-', and '_', with at least one upper-case letter, \
             one lower-case letter, and one digit)"
        ),
        "{}",
        refused.stderr
    );

    step("the MCP pack tool: D1's test name is a query, a run of three classes is not");
    let mut mcp = Mcp::start(&McpEnv::of(
        &agent.config_dir,
        &agent.data_dir,
        &agent.token,
        &setup.repo,
    ));
    let answered = mcp.call("pack", &json!({"query": D1_TEST_NAME}));
    assert!(!answered.is_error, "{}", answered.line);
    assert!(answered.text.contains("; 2 matches"), "{}", answered.text);
    let refused = mcp.call("pack", &json!({"query": format!("why {MIXED}")}));
    assert_eq!(
        refused.failure(),
        Some("invalid_arguments"),
        "{}",
        refused.line
    );
    assert_eq!(
        refused.text,
        "the argument query holds a control character (Cc) or a string in the format of a memprove \
         token (43 characters of A-Z, a-z, 0-9, -, _ with at least one upper-case letter, one \
         lower-case letter, and one digit); the query is not repeated; nothing was sent"
    );
    assert!(!refused.line.contains(MIXED));
    let (_, stderr) = mcp.finish();
    assert!(!stderr.contains(MIXED) && !stderr.contains(token));
}
