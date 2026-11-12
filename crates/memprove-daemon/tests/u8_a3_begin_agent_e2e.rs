//! U8 A3 (Owner O4; ADR 0014, "Amendment (U8, S-A)") through the real
//! binaries: `begin` and `session create` declare the Session's agent with
//! `--agent-name`, `--agent-role`, and `--parent-session`, whose defaults
//! are `MEMPROVE_AGENT_NAME`, `MEMPROVE_AGENT_ROLE`, and
//! `MEMPROVE_PARENT_SESSION`; only a `begin` given none of the three
//! reuses the agent the context file remembers, whole (U8 S-F round 1,
//! B1). `context show` shows it, and the MCP `begin` tool takes the same
//! three inputs. A context file without them reads as before, and a
//! `begin` without any declares no agent.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use p4b_support::{Cli, Run, Setup, cli_binary, isolated, step, text};
use p4g_support::{Mcp, McpEnv};
use serde_json::{Value, json};

/// `memprove <words>` as `cli` runs it, with `env` set.
fn run_with(cli: &Cli, env: &[(&str, &str)], words: &[&str]) -> Run {
    let output = isolated(cli_binary())
        .current_dir(&cli.cwd)
        .arg("--data-dir")
        .arg(&cli.data_dir)
        .arg("--token-file")
        .arg(&cli.token)
        .args(words)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .envs(env.iter().copied())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "$ {env:?} ct {}\n{}{}[exit {:?}]",
        words.join(" "),
        run.stdout,
        run.stderr,
        run.code
    );
    run
}

fn ok_with(cli: &Cli, env: &[(&str, &str)], words: &[&str]) -> Value {
    let run = run_with(cli, env, words);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    serde_json::from_str(run.stdout.trim()).unwrap()
}

/// The `agent` member `listSessions` shows for `session_id`.
fn listed_agent(cli: &Cli, workspace_id: &str, session_id: &str) -> Value {
    let page = cli.ok(&["session", "list", "--workspace", workspace_id]);
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["session_id"] == session_id)
        .unwrap()
        .get("agent")
        .cloned()
        .unwrap_or(Value::Null)
}

const ENV: [(&str, &str); 2] = [
    ("MEMPROVE_AGENT_NAME", "from-env"),
    ("MEMPROVE_AGENT_ROLE", "human"),
];

#[test]
fn u8_a3_begin_declares_the_agent_options_over_environment_over_context() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;

    step("options win over the environment; the context file records them");
    let begun = ok_with(
        agent,
        &ENV,
        &[
            "begin",
            "--project",
            &setup.project_id,
            "--new-task",
            "u8 a3",
            "--agent-name",
            "lead",
            "--agent-role",
            "orchestrator",
        ],
    );
    let workspace = begun["workspace_id"].as_str().unwrap().to_owned();
    let first = begun["session_id"].as_str().unwrap().to_owned();
    assert_eq!(begun["agent_name"], "lead");
    assert_eq!(begun["agent_role"], "orchestrator");
    assert!(begun.get("parent_session_id").is_none(), "{begun}");
    assert_eq!(
        listed_agent(agent, &workspace, &first),
        json!({"name": "lead", "role": "orchestrator"})
    );
    let shown = agent.run(&["context", "show"], false);
    assert!(
        shown
            .stdout
            .contains("Agent name:     lead (role orchestrator)"),
        "{}",
        shown.stdout
    );
    agent.bytes(&["end", "--reason", "first"]);

    step("the environment wins over the context file");
    let second = ok_with(agent, &ENV, &["begin"]);
    let second_id = second["session_id"].as_str().unwrap().to_owned();
    assert_eq!(
        listed_agent(agent, &workspace, &second_id),
        json!({"name": "from-env", "role": "human"})
    );
    agent.bytes(&["end", "--reason", "second"]);

    step("neither: the context file's agent, whole");
    let unnamed = agent.ok(&["begin"]);
    assert_eq!(
        listed_agent(agent, &workspace, unnamed["session_id"].as_str().unwrap()),
        json!({"name": "from-env", "role": "human"})
    );
    agent.bytes(&["end", "--reason", "remembered"]);

    // U8 S-F round 1, B1: a parent comes with a name and a role.
    step("--parent-session names the parent of the agent the variables name");
    let third = ok_with(agent, &ENV, &["begin", "--parent-session", &first]);
    let third_id = third["session_id"].as_str().unwrap().to_owned();
    assert_eq!(third["parent_session_id"], first.as_str());
    assert_eq!(
        listed_agent(agent, &workspace, &third_id),
        json!({"name": "from-env", "role": "human", "parent_session_id": first})
    );
    let capsule = agent.capsule();
    let sessions = capsule["sessions"].as_array().unwrap();
    assert_eq!(
        sessions[3]["agent"],
        json!({"name": "from-env", "role": "human", "parent_session_id": first})
    );

    step("session create takes the same options");
    let created = agent.ok(&[
        "session",
        "create",
        "--workspace",
        &workspace,
        "--agent",
        third["agent_id"].as_str().unwrap(),
        "--agent-name",
        "raw",
        "--agent-role",
        "sub_agent",
        "--parent-session",
        &third_id,
    ]);
    assert_eq!(
        created["agent"],
        json!({"name": "raw", "role": "sub_agent", "parent_session_id": third_id})
    );
}

/// Options, environment, and the option the usage error names.
type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str);

#[test]
fn u8_a3_a_mistaken_agent_is_a_usage_error_and_sends_nothing() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let begin = ["begin", "--project", &setup.project_id, "--new-task", "x"];
    let cases: [Case; 5] = [
        (&["--agent-role", "human"], &[], "--agent-name"),
        (&["--agent-name", "a"], &[], "--agent-role"),
        (
            &["--agent-name", "a b", "--agent-role", "human"],
            &[],
            "--agent-name",
        ),
        (
            &["--agent-name", "a", "--agent-role", "admin"],
            &[],
            "--agent-role",
        ),
        (
            &["--parent-session", "not-an-id"],
            &[
                ("MEMPROVE_AGENT_NAME", "a"),
                ("MEMPROVE_AGENT_ROLE", "human"),
            ],
            "--parent-session",
        ),
    ];
    for (options, env, named) in cases {
        let words = [&begin[..], options].concat();
        let run = run_with(agent, env, &words);
        assert_eq!(run.code, Some(2), "{}", run.stderr);
        assert!(run.stderr.contains(named), "{}", run.stderr);
    }
    let listed = agent.ok(&["task", "list", "--project", &setup.project_id]);
    assert!(listed["items"].as_array().unwrap().is_empty(), "{listed}");
}

#[test]
fn u8_a3_an_unnamed_context_reads_as_before_and_mcp_begin_declares_an_agent() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    step("a begin without any agent writes the context as before");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u8 a3 mcp",
    ]);
    for member in ["agent_name", "agent_role", "parent_session_id"] {
        assert!(begun.get(member).is_none(), "{begun}");
    }
    let workspace = begun["workspace_id"].as_str().unwrap().to_owned();
    let first = begun["session_id"].as_str().unwrap().to_owned();
    assert_eq!(listed_agent(agent, &workspace, &first), Value::Null);
    let shown = agent.run(&["context", "show"], false);
    assert!(!shown.stdout.contains("Agent name:"), "{}", shown.stdout);
    agent.bytes(&["end", "--reason", "first"]);

    step("MCP begin takes agent_name, agent_role, and parent_session");
    let mut mcp = Mcp::start(&McpEnv::of(
        &agent.config_dir,
        &agent.data_dir,
        &agent.token,
        &setup.repo,
    ));
    let refused = mcp.call(
        "begin",
        &json!({"agent_name": "a b", "agent_role": "sub_agent"}),
    );
    assert_eq!(
        refused.failure(),
        Some("invalid_arguments"),
        "{}",
        refused.line
    );
    assert_eq!(refused.structured["parameter"], "agent_name");
    let begun = mcp.call(
        "begin",
        &json!({"agent_name": "mcp-child", "agent_role": "sub_agent", "parent_session": first}),
    );
    assert!(!begun.is_error, "{}", begun.line);
    let session = begun.structured["session_id"].as_str().unwrap().to_owned();
    mcp.finish();
    assert_eq!(
        listed_agent(agent, &workspace, &session),
        json!({"name": "mcp-child", "role": "sub_agent", "parent_session_id": first})
    );
}

/// The Sessions `session list` shows for `workspace_id`.
fn session_count(cli: &Cli, workspace_id: &str) -> usize {
    cli.ok(&["session", "list", "--workspace", workspace_id])["items"]
        .as_array()
        .unwrap()
        .len()
}

/// U8 S-F round 1, B1: begins a Task whose first Session is `optname`, a
/// sub-agent, and returns the Workspace, that Session, and how to begin
/// (and end) a Session of `child`, a sub-agent it dispatched, which the
/// context then remembers.
fn dispatched_child(setup: &Setup) -> (String, String, Value) {
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u8 f1 b1",
        "--agent-name",
        "optname",
        "--agent-role",
        "sub_agent",
    ]);
    let workspace = begun["workspace_id"].as_str().unwrap().to_owned();
    let first = begun["session_id"].as_str().unwrap().to_owned();
    agent.bytes(&["end", "--reason", "first"]);
    let dispatched = json!({"name": "child", "role": "sub_agent", "parent_session_id": first});
    begin_child(setup, &workspace, &first, &dispatched);
    (workspace, first, dispatched)
}

/// A Session of `child`, dispatched by `first`, begun and ended.
fn begin_child(setup: &Setup, workspace: &str, first: &str, dispatched: &Value) {
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--agent-name",
        "child",
        "--agent-role",
        "sub_agent",
        "--parent-session",
        first,
    ]);
    assert_eq!(
        listed_agent(agent, workspace, begun["session_id"].as_str().unwrap()),
        *dispatched
    );
    agent.bytes(&["end", "--reason", "child"]);
}

/// U8 S-F round 1, B1: a declaration is taken as a whole. A `begin` that
/// names an agent by option or variable mixes in nothing of the agent the
/// context remembers (not its parent); only a `begin` that gives none of
/// the three (an empty variable is unset) reuses the remembered agent,
/// whole.
#[test]
fn u8_f1_b1_a_declaration_is_taken_as_a_whole() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    step("a sub-agent dispatched by the first Session is remembered");
    let (workspace, first, dispatched) = dispatched_child(&setup);

    step("a new name and role by option: not the remembered parent");
    let lead = agent.ok(&[
        "begin",
        "--agent-name",
        "lead",
        "--agent-role",
        "orchestrator",
    ]);
    assert!(lead.get("parent_session_id").is_none(), "{lead}");
    assert_eq!(
        listed_agent(agent, &workspace, lead["session_id"].as_str().unwrap()),
        json!({"name": "lead", "role": "orchestrator"})
    );
    let shown = agent.run(&["context", "show"], false);
    assert!(
        shown
            .stdout
            .contains("Agent name:     lead (role orchestrator)\n"),
        "{}",
        shown.stdout
    );
    agent.bytes(&["end", "--reason", "lead"]);

    step("by variable, with an empty MEMPROVE_PARENT_SESSION: no parent");
    begin_child(&setup, &workspace, &first, &dispatched);
    let other = ok_with(
        agent,
        &[
            ("MEMPROVE_AGENT_NAME", "other"),
            ("MEMPROVE_AGENT_ROLE", "human"),
            ("MEMPROVE_PARENT_SESSION", ""),
        ],
        &["begin"],
    );
    assert_eq!(
        listed_agent(agent, &workspace, other["session_id"].as_str().unwrap()),
        json!({"name": "other", "role": "human"})
    );
    agent.bytes(&["end", "--reason", "other"]);

    step("none of the three, and empty variables: the remembered agent, whole");
    begin_child(&setup, &workspace, &first, &dispatched);
    let again = ok_with(
        agent,
        &[
            ("MEMPROVE_AGENT_NAME", ""),
            ("MEMPROVE_AGENT_ROLE", ""),
            ("MEMPROVE_PARENT_SESSION", ""),
        ],
        &["begin"],
    );
    assert_eq!(
        listed_agent(agent, &workspace, again["session_id"].as_str().unwrap()),
        dispatched
    );
}

/// U8 S-F round 1, B1: half a declaration is a usage error that sends
/// nothing, also when the context remembers the other half; the MCP
/// `begin` refuses it as `invalid_arguments` and, given a name and a role,
/// mixes in nothing of the remembered agent.
#[test]
fn u8_f1_b1_half_a_declaration_is_refused_by_begin_and_mcp_begin() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    let (workspace, first, _) = dispatched_child(&setup);

    step("half a declaration is a usage error and sends nothing");
    let sessions = session_count(agent, &workspace);
    let cases: [Case; 4] = [
        (
            &["--agent-name", "x"],
            &[],
            "--agent-name needs --agent-role",
        ),
        (
            &[],
            &[("MEMPROVE_AGENT_NAME", "envonly")],
            "--agent-name needs --agent-role",
        ),
        (
            &[],
            &[("MEMPROVE_AGENT_ROLE", "human")],
            "--agent-role needs --agent-name",
        ),
        (
            &["--parent-session", &first],
            &[],
            "--parent-session needs --agent-name and --agent-role",
        ),
    ];
    for (options, env, said) in cases {
        let words = [&["begin"][..], options].concat();
        let run = run_with(agent, env, &words);
        assert_eq!(run.code, Some(2), "{}", run.stderr);
        assert!(run.stderr.contains(said), "{}", run.stderr);
    }
    assert_eq!(session_count(agent, &workspace), sessions);

    step("MCP begin: half a declaration is refused; a whole one mixes in nothing");
    let mut mcp = Mcp::start(&McpEnv::of(
        &agent.config_dir,
        &agent.data_dir,
        &agent.token,
        &setup.repo,
    ));
    for (arguments, parameter) in [
        (json!({"agent_name": "x"}), "agent_role"),
        (json!({"agent_role": "human"}), "agent_name"),
        (json!({"parent_session": first}), "agent_name"),
    ] {
        let refused = mcp.call("begin", &arguments);
        assert_eq!(
            refused.failure(),
            Some("invalid_arguments"),
            "{}",
            refused.line
        );
        assert_eq!(refused.structured["parameter"], parameter);
    }
    assert_eq!(session_count(agent, &workspace), sessions);
    let begun = mcp.call(
        "begin",
        &json!({"agent_name": "mcp-lead", "agent_role": "orchestrator"}),
    );
    assert!(!begun.is_error, "{}", begun.line);
    let session = begun.structured["session_id"].as_str().unwrap().to_owned();
    mcp.finish();
    assert_eq!(
        listed_agent(agent, &workspace, &session),
        json!({"name": "mcp-lead", "role": "orchestrator"})
    );
}
