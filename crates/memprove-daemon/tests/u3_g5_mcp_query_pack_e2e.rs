//! U3 G5 (Owner O8, O9; ADR 0016, "Query selection") through the real
//! binaries: the MCP `pack` tool's optional `query` gives, on the same
//! server and Task, the bytes `memprove resume --pack --query` prints at
//! the same budget; a query Pack is not recorded as the last Pack; a query
//! with a control character or a token is refused as `invalid_arguments`
//! naming `query`, without repeating it, and nothing is sent; `tools/list`
//! lists the ten tools, and `pack` takes `task`, `budget`, and `query`; the
//! server's `instructions` say how to use `query`.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use std::fs;

use p4b_support::{Setup, sequence, step};
use p4g_support::{Mcp, McpEnv, last_packs};
use serde_json::{Value, json};

fn agent_env(setup: &Setup) -> McpEnv {
    McpEnv::of(
        &setup.agent.config_dir,
        &setup.agent.data_dir,
        &setup.agent.token,
        &setup.repo,
    )
}

fn forget_packs(setup: &Setup) {
    for (path, _) in last_packs(&setup.agent.config_dir) {
        fs::remove_file(path).unwrap();
    }
}

/// A Task of 64 statements, one open item superseded twice in turn and
/// one withdrawn, as in the G4 test.
fn query_ledger(setup: &Setup) -> String {
    let agent = &setup.agent;
    step("a Task of 64 statements");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "U3 G5 query Pack",
    ]);
    let a = agent.test("tests/a.sh");
    let mut words = vec![
        "checkpoint".to_owned(),
        "create".to_owned(),
        "--fact".to_owned(),
        "The Pack budget counts UTF-16 code units".to_owned(),
        "--evidence".to_owned(),
        a,
    ];
    for number in 1..=57 {
        words.push("--open".to_owned());
        words.push(format!("unrelated record filler {number}"));
    }
    words.extend(
        [
            "--open",
            "query results ordered by claim number (old)",
            "--open",
            "budget budget budget",
            "--next",
            "the old next step",
        ]
        .map(str::to_owned),
    );
    agent.ok(&words.iter().map(String::as_str).collect::<Vec<_>>());
    let capsule = agent.capsule();
    let old = number_of(&capsule, "query results ordered by claim number (old)");
    let filler = number_of(&capsule, "unrelated record filler 9");
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        "query results ordered by bm25 rank",
        "--supersedes",
        &old.to_string(),
        "--withdraw",
        &filler.to_string(),
        "--next",
        "next step: implement the MCP query",
    ]);
    begun["task_id"].as_str().unwrap().to_owned()
}

fn number_of(capsule: &Value, text: &str) -> u64 {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["text"] == text)
        .and_then(|claim| claim["number"].as_u64())
        .unwrap_or_else(|| panic!("no claim {text:?}"))
}

fn modern(id: u64, method: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        }},
    })
    .to_string()
}

#[test]
#[allow(clippy::too_many_lines)] // One scenario, step by step.
fn u3g5_the_pack_tool_takes_a_query_with_the_clis_bytes() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let task = query_ledger(&setup);
    let capsule = agent.capsule();
    assert!(capsule["claims"].as_array().unwrap().len() >= 60);
    let before = sequence(&capsule);
    let mut mcp = Mcp::start(&agent_env(&setup));

    for (arguments, words) in [
        (
            json!({"query": "budget"}),
            vec!["--query", "budget", "--budget", "6000"],
        ),
        (
            json!({"query": "ordered", "budget": 9000}),
            vec!["--query", "ordered", "--budget", "9000"],
        ),
        (
            json!({"query": "filler", "budget": 2400}),
            vec!["--query", "filler", "--budget", "2400"],
        ),
        (
            json!({"query": "nothing like this"}),
            vec!["--query", "nothing like this", "--budget", "6000"],
        ),
    ] {
        step(&format!("pack {arguments} against resume --pack {words:?}"));
        forget_packs(&setup);
        let mut all = vec!["resume", "--pack"];
        all.extend(words.iter().copied());
        let cli = agent.bytes(&all);
        let called = mcp.ok("pack", &arguments);
        assert_eq!(called.text, cli, "the same bytes for {arguments}");
        assert!(
            last_packs(&agent.config_dir).is_empty(),
            "a query Pack is not the last Pack"
        );
        assert_eq!(called.structured["task_id"], task.as_str());
        let budget = arguments["budget"].as_u64().unwrap_or(6000);
        assert_eq!(called.structured["budget"], budget);
        assert!(cli.lines().nth(2).unwrap().starts_with("Query: "), "{cli}");
        let pack = cli.clone();
        // The object names the query Pack's digest.
        assert_eq!(
            format!(
                "Digest: {}",
                called.structured["pack_digest"].as_str().unwrap()
            ),
            pack.lines().last().unwrap()
        );
    }

    step("over budget: the length the query Pack needs, which then fits");
    let small = mcp.call("pack", &json!({"query": "filler", "budget": 1000}));
    let cli = agent.run(
        &["resume", "--pack", "--query", "filler", "--budget", "1000"],
        true,
    );
    if small.is_error {
        assert_eq!(small.failure(), Some("over_budget"), "{}", small.line);
        assert_eq!(cli.code, Some(1));
        let needed = small.structured["needed"].as_u64().unwrap();
        assert!(
            cli.stderr.contains(&format!("--budget {needed} ")),
            "{}",
            cli.stderr
        );
        let fits = mcp.ok("pack", &json!({"query": "filler", "budget": needed}));
        let needed = needed.to_string();
        assert_eq!(
            fits.text,
            agent.bytes(&["resume", "--pack", "--query", "filler", "--budget", &needed])
        );
    } else {
        assert_eq!(small.text, cli.stdout);
    }

    step("refused: a control character, a token, a length; nothing sent");
    let token = "u3G5_tokenShaped-0123456789abcdefghijklmnop";
    assert_eq!(token.len(), 43);
    for query in [
        "secret\u{7}query".to_owned(),
        "secret\nquery".to_owned(),
        format!("secret {token}"),
        "s".repeat(257),
        String::new(),
    ] {
        let refused = mcp.call("pack", &json!({"query": query}));
        assert_eq!(refused.failure(), Some("invalid_arguments"), "{query:?}");
        assert_eq!(refused.structured["parameter"], "query");
        assert!(!refused.line.contains("secret") && !refused.line.contains(token));
    }
    let refused = mcp.call("pack", &json!({"query": 3}));
    assert_eq!(refused.failure(), Some("invalid_arguments"));

    step("tools/list: the ten tools in order, and pack takes task, budget, and query");
    let list: Value = serde_json::from_str(&mcp.ask(&modern(900, "tools/list"))).unwrap();
    let tools = list["result"]["tools"].as_array().unwrap();
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "pack",
            "claims",
            "context",
            "tasks",
            "ledger_verify",
            "begin",
            "checkpoint",
            "decide",
            "end",
            "complete"
        ]
    );
    let properties = tools[0]["inputSchema"]["properties"].as_object().unwrap();
    let mut listed = properties.keys().map(String::as_str).collect::<Vec<_>>();
    listed.sort_unstable();
    assert_eq!(listed, ["budget", "query", "task"]);
    let query = &properties["query"];
    assert_eq!(query["type"], "string");
    assert_eq!(query["minLength"], 1);
    assert_eq!(query["maxLength"], 256);

    step("the instructions say how to use query");
    let discover: Value = serde_json::from_str(&mcp.ask(&modern(901, "server/discover"))).unwrap();
    let instructions = discover["result"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains(
            "The pack tool's `query` returns a Pack of only the statements that match it"
        ),
        "{instructions}"
    );

    let (_, stderr) = mcp.finish();
    assert!(
        !stderr.contains("secret") && !stderr.contains(token),
        "{stderr}"
    );
    assert_eq!(sequence(&agent.capsule()), before, "reads write nothing");
}
