//! Acceptance scenario 9 (ADR 0014, Owner, original text; P4-G E7.11.4,
//! Owner condition t; P4-H2 names it the scenario-9 test) through the real
//! binaries:
//!
//! > The same reads and writes can be done through the MCP tools; neither the
//! > command line nor the tool arguments hold a hand-written
//! > UUID。
//!
//! The reads and writes of a Task done through the MCP tools, with the
//! tests run by `memprove run` in a shell. First in the hook's layout
//! (the `SessionStart` hook installed in a temporary project injects the
//! Pack and exports the client session; `begin` runs in the shell; the
//! server, given the same client session as `CLAUDE_CODE_SESSION_ID` (U1,
//! A1), finds that Session in its own context), then in a second run
//! without the hook, where the tools also `begin`.
//!
//! - Reads: the `pack` tool with `budget` 9000 returns the bytes `resume
//!   --pack` prints, the `claims` tool those of `resume --pack --claims`,
//!   and `ledger_verify` the report of `ledger verify`, each read at the
//!   same ledger state.
//! - Writes: every write tool (`begin`, `checkpoint` with facts, open
//!   items, `supersedes`, `withdraw`, `reaffirm`, `reaffirm_all`, and the
//!   next action, `decide`, `complete`, `end`) changes the Task as the CLI
//!   reads it afterwards: `resume` shows each claim the result names, with
//!   its standing; after each write `resume --text --all` reads and `ledger
//!   verify` holds. (`p4g_mcp_write_e2e.rs` also runs the same writes once
//!   through the CLI and once through the tools on two ledgers and finds
//!   the two Capsules equal.)
//! - No hand-written UUID: every argv the agent typed and every `arguments`
//!   object it sent is matched against
//!   `[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}`
//!   (written out by hand: no regex crate is added) and none matches, while
//!   the Task's own ID does. Every tool result names its Task and Session
//!   first, so the agent never needs to type one. The operator's first
//!   `begin --project <id>`, before the agent starts, is not the agent's
//!   (Owner, P3-C2).
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;

use std::{
    fs,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use p4b_support::{Cli, Run, Setup, cli_binary, commit, git, isolated, step, text, write};
use p4g_support::{Called, Mcp, McpEnv};
use serde_json::{Value, json};

/// The client session the hook was given.
const CLIENT: &str = "client-9";

/// Whether `text` matches the regular expression
/// `[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}`
/// (a UUID), checked window by window.
fn has_uuid(text: &str) -> bool {
    text.as_bytes().windows(36).any(|window| {
        window.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    })
}

/// The agent's side of the run: the shell commands and tool calls, each
/// recorded as the agent wrote it.
struct Agent<'a> {
    setup: &'a Setup,
    client: Option<&'static str>,
    transcript: Vec<String>,
}

impl Agent<'_> {
    /// `memprove <words>` in the shell, as the client session when the
    /// hook exported one.
    fn shell(&mut self, words: &[&str]) -> Run {
        self.transcript
            .push(format!("$ memprove {}", words.join(" ")));
        let cli = &self.setup.agent;
        let mut command = isolated(cli_binary());
        command
            .current_dir(&cli.cwd)
            .args(words)
            .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
            .env("MEMPROVE_DATA_DIR", &cli.data_dir)
            .env("MEMPROVE_TOKEN_FILE", &cli.token)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null());
        if let Some(client) = self.client {
            command.env("MEMPROVE_CLIENT_SESSION", client);
        }
        let output = command.output().unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        println!(
            "[agent] $ memprove {}\n{}{}[exit {:?}]",
            words.join(" "),
            if run.stdout.len() > 3000 {
                "<long output>\n"
            } else {
                &run.stdout
            },
            run.stderr,
            run.code
        );
        run
    }

    /// A test run in the shell; returns its label.
    fn test(&mut self, script: &str) -> String {
        let run = self.shell(&["run", "--test-format", "cargo", "--", "sh", script]);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        let last = run.stderr.lines().last().unwrap_or_default();
        last.rsplit_once(", label ")
            .unwrap_or_else(|| panic!("no label: {}", run.stderr))
            .1
            .to_owned()
    }

    /// A tool call; every result begins with its Task and Session.
    fn call(&mut self, mcp: &mut Mcp, tool: &str, arguments: &Value) -> Called {
        self.transcript.push(format!("tool {tool} {arguments}"));
        let called = mcp.call(tool, arguments);
        assert!(!called.is_error, "{}", called.line);
        // T21 (V6-F1): the object of a Pack result is its second text
        // block; every other result's is its `structuredContent`.
        let (object, marker) = if matches!(tool, "pack" | "claims") {
            let line: Value = serde_json::from_str(&called.line).unwrap();
            (
                // U12 T2 (the Owner's O8 exception of 2026-11-05): the object
                // is the last text block (a `pack` without `query` has its
                // opening first).
                line["result"]["content"]
                    .as_array()
                    .and_then(|blocks| blocks.last())
                    .and_then(|block| block["text"].as_str())
                    .unwrap()
                    .to_owned(),
                r#"{"task_id":"#,
            )
        } else {
            (called.line.clone(), r#""structuredContent":{"task_id":"#)
        };
        assert!(object.contains(marker), "{}", called.line);
        let after_task = object.split(marker).nth(1).unwrap();
        let session_at = after_task.find(r#","session_id":"#).unwrap();
        assert!(
            session_at <= 38,
            "session_id is not second: {}",
            called.line
        );
        called
    }

    /// The Task Capsule as the CLI reads it (`resume`).
    fn capsule(&mut self) -> Value {
        let run = self.shell(&["resume"]);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }

    /// The standard output of a shell command that must succeed.
    fn bytes(&mut self, words: &[&str]) -> String {
        let run = self.shell(words);
        assert_eq!(run.code, Some(0), "{words:?}: {}", run.stderr);
        run.stdout
    }

    /// The same reads through the tools and through the CLI, at the same
    /// ledger state: `pack` (budget 9000) is `resume --pack`, `claims` of
    /// every number is `resume --pack --claims`, `ledger_verify` is `ledger
    /// verify`.
    fn reads_match(&mut self, mcp: &mut Mcp) {
        let pack = self.call(mcp, "pack", &json!({"budget": 9000}));
        assert_eq!(pack.text, self.bytes(&["resume", "--pack"]));
        let last = self.capsule()["claims"].as_array().unwrap().len();
        let numbers = (1..=last).collect::<Vec<_>>();
        let claims = self.call(mcp, "claims", &json!({"claims": numbers}));
        assert_eq!(
            claims.text,
            self.bytes(&["resume", "--pack", "--claims", &format!("1-{last}")])
        );
        let verified = self.call(mcp, "ledger_verify", &json!({}));
        let report: Value = serde_json::from_str(self.bytes(&["ledger", "verify"]).trim()).unwrap();
        assert_eq!(verified.structured["report"], report);
        assert_eq!(report["valid"], true);
    }

    /// After a write: `resume --text --all` reads and the ledger verifies.
    fn check(&mut self) {
        let shown = self.shell(&["resume", "--text", "--all"]);
        assert_eq!(shown.code, Some(0), "{}", shown.stderr);
        assert!(shown.stdout.contains("Task「"), "{}", shown.stdout);
        let verified = self.shell(&["ledger", "verify"]);
        assert_eq!(verified.code, Some(0), "{}", verified.stderr);
        let report: Value = serde_json::from_str(verified.stdout.trim()).unwrap();
        assert_eq!(report["valid"], true);
    }
}

/// The server as Claude Code spawns it: the project directory, the agent's
/// configuration, data directory, and token file, and (U1, A1) the client
/// session as `CLAUDE_CODE_SESSION_ID` when the hook was given one.
fn server(setup: &Setup, client: Option<&str>) -> Mcp {
    Mcp::start(&McpEnv {
        claude_code_session: client.map(str::to_owned),
        ..McpEnv::of(
            &setup.agent.config_dir,
            &setup.agent.data_dir,
            &setup.agent.token,
            &setup.repo,
        )
    })
}

/// `memprove hook <words>` in `cwd`, with `input` on standard input.
fn hook(setup: &Setup, cwd: &Path, words: &[&str], input: &[u8]) -> Run {
    let cli = &setup.agent;
    let mut child = isolated(cli_binary())
        .arg("hook")
        .args(words)
        .current_dir(cwd)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .env("MEMPROVE_DATA_DIR", &cli.data_dir)
        .env("MEMPROVE_TOKEN_FILE", &cli.token)
        .env("CLAUDE_ENV_FILE", setup.root_path.join("claude-env.sh"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "$ memprove hook {}\n{}{}[exit {:?}]",
        words.join(" "),
        run.stdout,
        run.stderr,
        run.code
    );
    run
}

/// The claim `number` of `capsule`.
fn claim(capsule: &Value, number: &Value) -> Value {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["number"] == *number)
        .unwrap_or_else(|| panic!("no claim #{number}"))
        .clone()
}

/// Statements through the tools: facts on two runs and open items; a
/// Decision that supersedes an open item; a fact that supersedes the first
/// and a withdrawal. The CLI reads what each write did. Returns the results
/// and the number of the fact in force.
fn statements(agent: &mut Agent<'_>, mcp: &mut Mcp) -> (Vec<Called>, Value) {
    let r1 = agent.test("tests/a.sh");
    let r2 = agent.test("tests/b.sh");
    let written = agent.call(
        mcp,
        "checkpoint",
        &json!({
            "facts": [{"text": "a and b pass", "evidence": [r1, r2]}],
            "open_items": [{"text": "c is unknown"}, {"text": "d is unknown"}],
            "next": {"text": "decide the plan"},
        }),
    );
    assert_eq!(written.structured["sent"], true);
    assert_eq!(written.structured["report"], "read");
    agent.check();
    agent.reads_match(mcp);
    let decided = agent.call(
        mcp,
        "decide",
        &json!({"text": "use plan x", "rationale": "simpler", "supersedes": [2]}),
    );
    let decision = decided.structured["number"].clone();
    assert!(decision.is_u64(), "{}", decided.line);
    let capsule = agent.capsule();
    assert_eq!(claim(&capsule, &decision)["kind"], "decision");
    assert_eq!(
        claim(&capsule, &json!(2))["superseded_by"][0]["number"],
        decision
    );
    agent.check();
    let replaced = agent.call(
        mcp,
        "checkpoint",
        &json!({
            "facts": [{"text": "a and b pass in this state", "evidence": [r1, r2], "supersedes": [1]}],
            "withdraw": [3],
            "withdraw_reason": "d is out of scope",
            "next": {"text": "commit, then reaffirm"},
        }),
    );
    assert_eq!(replaced.structured["sent"], true);
    let capsule = agent.capsule();
    let fact = claim(&capsule, &json!(1))["superseded_by"][0]["number"].clone();
    assert_eq!(claim(&capsule, &fact)["text"], "a and b pass in this state");
    assert_eq!(claim(&capsule, &fact)["verdict"], "verified");
    assert_eq!(claim(&capsule, &json!(3))["standing"], "withdrawn");
    assert_eq!(
        claim(&capsule, &json!(3))["withdrawal_reason"],
        "d is out of scope"
    );
    agent.check();
    agent.reads_match(mcp);
    (vec![written, decided, replaced], fact)
}

/// After a commit, the fact `fact` reaffirmed by number on the reruns;
/// after another, `reaffirm_all` reaffirms it. Returns the results.
fn reaffirmations(agent: &mut Agent<'_>, mcp: &mut Mcp, state: &str, fact: &Value) -> Vec<Called> {
    write(&agent.setup.repo.join("README"), state);
    commit(&agent.setup.repo, "a new state");
    let ra = agent.test("tests/a.sh");
    let rb = agent.test("tests/b.sh");
    let one = agent.call(
        mcp,
        "checkpoint",
        &json!({
            "reaffirm": [{"claim": fact, "evidence": [ra, rb]}],
            "next": {"text": "commit again, then reaffirm all"},
        }),
    );
    assert_eq!(one.structured["sent"], true);
    let capsule = agent.capsule();
    let fact = claim(&capsule, fact)["superseded_by"][0]["number"].clone();
    assert_eq!(claim(&capsule, &fact)["text"], "a and b pass in this state");
    assert_eq!(claim(&capsule, &fact)["verdict"], "verified");
    agent.check();
    write(&agent.setup.repo.join("README"), &format!("{state}again\n"));
    commit(&agent.setup.repo, "another state");
    agent.test("tests/a.sh");
    agent.test("tests/b.sh");
    let all = agent.call(
        mcp,
        "checkpoint",
        &json!({"reaffirm_all": {}, "next": {"text": "complete the Task"}}),
    );
    let reaffirmed = all.structured["reaffirm_all"]["reaffirmed"]
        .as_array()
        .unwrap();
    assert_eq!(reaffirmed.len(), 1, "{}", all.line);
    assert_eq!(reaffirmed[0]["verdict"], "verified");
    assert_eq!(all.structured["reaffirm_all"]["skipped"], json!([]));
    agent.check();
    agent.reads_match(mcp);
    vec![one, all]
}

/// The Task's work through the tools in the Session the agent is in:
/// [`statements`], [`reaffirmations`], completion, end. The CLI reads what
/// each write did. Returns the Task and the Session every result named.
fn work_through_tools(agent: &mut Agent<'_>, mcp: &mut Mcp, state: &str) -> (Value, Value) {
    let (mut results, fact) = statements(agent, mcp);
    let ids = (
        results[0].structured["task_id"].clone(),
        results[0].structured["session_id"].clone(),
    );
    results.extend(reaffirmations(agent, mcp, state, &fact));
    let completed = agent.call(mcp, "complete", &json!({}));
    assert_eq!(completed.structured["state"], "completed");
    assert_eq!(agent.capsule()["task"]["status"], "completed");
    agent.check();
    let ended = agent.call(mcp, "end", &json!({"reason": "scenario 9 done"}));
    assert_eq!(ended.structured["context_cleared"], true);
    let capsule = agent.capsule();
    let session = capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == ids.1)
        .unwrap()
        .clone();
    assert_eq!(session["end_state"], "ended");
    agent.check();
    results.extend([completed, ended]);
    assert_eq!(results.len(), 7);
    for called in &results {
        assert_eq!(
            (
                called.structured["task_id"].clone(),
                called.structured["session_id"].clone()
            ),
            ids,
            "{}",
            called.line
        );
    }
    ids
}

/// The hook's layout: the hook installed in this temporary project and
/// `SessionStart`; `pack` through MCP and `begin` in the shell as the client
/// session the hook exported; then the work through the tools.
fn in_the_hook_layout(agent: &mut Agent<'_>, task: &str) {
    let setup = agent.setup;
    step("the hook installed in this temporary project, then SessionStart");
    let installed = hook(setup, &setup.repo, &["install"], b"");
    assert_eq!(installed.code, Some(0), "{}", installed.stderr);
    assert!(setup.repo.join(".claude/settings.local.json").is_file());
    let input = json!({
        "session_id": CLIENT,
        "transcript_path": "/nowhere/transcript.jsonl",
        "cwd": setup.repo.to_str().unwrap(),
        "hook_event_name": "SessionStart",
        "source": "startup",
    })
    .to_string();
    agent.transcript.push(format!("hook session-start {input}"));
    let started = hook(setup, &setup.repo, &["session-start"], input.as_bytes());
    assert_eq!(started.code, Some(0));
    assert!(
        started.stdout.contains("```text\nPack: "),
        "{}",
        started.stdout
    );
    let exported = fs::read_to_string(setup.root_path.join("claude-env.sh")).unwrap();
    assert!(exported.contains(&format!("MEMPROVE_CLIENT_SESSION='{CLIENT}'")));
    agent.client = Some(CLIENT);

    step("the hook's layout: pack through MCP, begin in the shell");
    // U1 (A1, A2): Claude Code gives the server the hook's session id as
    // CLAUDE_CODE_SESSION_ID; `complete` and `end` write back that client
    // session's context file, the one the shell's `begin` wrote.
    let mut mcp = server(setup, Some(CLIENT));
    let pack = agent.call(&mut mcp, "pack", &json!({}));
    assert_eq!(pack.structured["task_id"], task);
    assert!(pack.text.starts_with("Pack: memprove-pack v1"));
    let begun = agent.shell(&["begin"]);
    assert_eq!(begun.code, Some(0), "{}", begun.stderr);
    assert!(
        begun
            .stderr
            .contains("declares that it resumes from the Pack"),
        "{}",
        begun.stderr
    );
    let begun: Value = serde_json::from_str(begun.stdout.trim()).unwrap();
    assert_eq!(begun["task_id"], task);
    let context = agent.call(&mut mcp, "context", &json!({}));
    assert_eq!(context.structured["contexts_with_session"], 1);
    agent.check();
    let ids = work_through_tools(agent, &mut mcp, "fixture, first run\n");
    assert_eq!(ids, (json!(task), begun["session_id"].clone()));
    mcp.finish();
}

/// Acceptance scenario 9 (E7.11.4) end to end, in the hook's layout and
/// without it.
#[test]
fn scenario_9_the_same_reads_and_writes_go_through_the_tools_with_no_hand_written_uuid() {
    let setup = Setup::new(&["a", "b"]);
    // U5 Z-F round 1 (S1): `hook install` also writes .mcp.json.
    write(
        &setup.repo.join(".gitignore"),
        "flags/\nout/\n.claude/\n.mcp.json\n",
    );
    git(&setup.repo, &["add", "."]);
    commit(&setup.repo, "ignore the local settings of Claude Code");
    step("the operator's first begin --project, then end (not the agent's)");
    let operator: &Cli = &setup.agent;
    let first = operator.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 9",
    ]);
    let task = first["task_id"].as_str().unwrap().to_owned();
    operator.bytes(&["end", "--reason", "operator"]);

    let mut agent = Agent {
        setup: &setup,
        client: None,
        transcript: Vec::new(),
    };
    in_the_hook_layout(&mut agent, &task);

    step("a second run without the hook: begin through MCP too");
    agent.client = None;
    let mut mcp = server(&setup, None);
    let begun = agent.call(
        &mut mcp,
        "begin",
        &json!({"new_task": {"title": "scenario 9, second run"}}),
    );
    assert_eq!(begun.structured["context_file"], "directory");
    assert_eq!(begun.structured["task_moved"], true);
    assert_ne!(begun.structured["task_id"], task.as_str());
    agent.check();
    let ids = work_through_tools(&mut agent, &mut mcp, "fixture, second run\n");
    assert_eq!(
        ids,
        (
            begun.structured["task_id"].clone(),
            begun.structured["session_id"].clone()
        )
    );
    let after = agent.call(&mut mcp, "context", &json!({}));
    assert_eq!(after.structured["session_id"], Value::Null);
    let (stdout, _) = mcp.finish();
    assert!(
        stdout
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).is_ok())
    );

    println!("transcript of the agent:\n{}", agent.transcript.join("\n"));
    assert!(agent.transcript.len() > 30);
    assert!(has_uuid(&task), "the matcher finds a UUID");
    let tools = agent
        .transcript
        .iter()
        .filter(|entry| entry.starts_with("tool "))
        .count();
    assert!(tools > 20, "{tools} tool calls");
    for entry in &agent.transcript {
        assert!(!has_uuid(entry), "a UUID in {entry}");
    }
    let hook_removed = Command::new("git")
        .arg("-C")
        .arg(&setup.repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(
        text(&hook_removed.stdout).trim().is_empty(),
        "the tree is dirty"
    );
}
