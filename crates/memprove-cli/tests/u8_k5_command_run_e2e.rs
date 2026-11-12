//! U8 K5 (U7 V-N12, Owner O5; ADR 0018 and ADR 0014 "Amendment (U8,
//! S-K)"), through the real `memprove` and `memproved`:
//!
//! 1. `memprove run` without `--test` or `--test-format`, of the argv of the
//!    Task's last passed test run, says on standard error which arguments to
//!    add (one more line, before the line that sums the run up); the run is
//!    still recorded as a command run, and a run with the arguments, a
//!    run of another argv, and a Task with no passed test run get no such
//!    line.
//! 2. `checkpoint create --fact` all of whose citations are runs that this
//!    context recorded as command runs sends nothing and gives the one reason
//!    (exit 2, as every refusal before the request); the MCP `checkpoint`
//!    tool gives the same sentence as an error. A fact on a test run, a fact
//!    that cites a test run besides a command run (the daemon allows it), and
//!    an open item, are not held back.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    fs,
    io::Write as _,
    os::unix::fs::PermissionsExt,
    process::{Output, Stdio},
};

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{Fixture, cli_binary, git, isolated, text};

/// A runner named `pytest` that prints a summary pytest would.
const PYTEST_SCRIPT: &str = "#!/bin/sh\necho '1 passed in 0.01s'\n";

/// A Task in the fixture with a passed test run of `./tests/pytest` (r1,
/// counted as pytest), a fact on it, and the Session open.
fn task_with_a_passed_pytest_run(fixture: &Fixture) {
    let script = fixture.repo.join("tests/pytest");
    fs::write(&script, PYTEST_SCRIPT).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    // Committed, so that no untracked file keeps the runs from being carried
    // into one another's Snapshot.
    git(&fixture.repo, &["add", "tests/pytest"]);
    git(
        &fixture.repo,
        &["commit", "--quiet", "-m", "add the pytest runner"],
    );
    fixture.agent_ok(&[
        "begin",
        "--project",
        &fixture.project,
        "--new-task",
        "K5 command runs",
    ]);
    fixture.agent_ok(&["run", "--test-format", "pytest", "--", "./tests/pytest"]);
    fixture.agent_ok(&[
        "checkpoint",
        "create",
        "--fact",
        "the pytest check passes",
        "--evidence",
        "r1",
        "--next",
        "carry on",
    ]);
}

fn shown(words: &[&str], output: &Output) {
    println!(
        "$ memprove {}\n{}{}[exit {:?}]",
        words.join(" "),
        text(&output.stdout),
        text(&output.stderr),
        output.status.code()
    );
}

fn run(fixture: &Fixture, words: &[&str]) -> Output {
    let output = fixture.agent(words);
    shown(words, &output);
    output
}

const HINT_PYTEST: &str = "memprove: note: this command is the Task's last passed test run, but without --test or --test-format it is recorded as a command run, which cannot support a fact; --test --test-format pytest records it as a test run";

#[test]
fn u8_k5_run_names_the_arguments_to_add_for_the_last_passed_test_run() {
    let fixture = Fixture::new();
    task_with_a_passed_pytest_run(&fixture);

    // The same argv as a command run: the hint is the line before the one
    // that sums the run up, and the run is a command run all the same.
    let output = run(&fixture, &["run", "--", "./tests/pytest"]);
    assert_eq!(output.status.code(), Some(0));
    let stderr = text(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    let last = lines.len() - 1;
    assert!(lines[last].starts_with("memprove: recorded PASS, evidence_id "));
    assert!(!lines[last].contains("test cases"), "{stderr}");
    assert_eq!(lines[last - 1], HINT_PYTEST, "{stderr}");
    assert_eq!(stderr.matches("note: ").count(), 1);
    let record = parse_strict(text(&output.stdout).trim()).unwrap();
    assert_eq!(
        record.get("kind").and_then(JsonValue::as_str),
        Some("command_result")
    );

    // With the arguments, with another argv, and with --test only (a test
    // run, so no hint): nothing more is said.
    for words in [
        &["run", "--test-format", "pytest", "--", "./tests/pytest"][..],
        &["run", "--test", "--test-cases", "1", "--", "./tests/pytest"][..],
        &["run", "--", "sh", "tests/check.sh"][..],
        &["run", "--", "./tests/pytest", "-q"][..],
    ] {
        let output = run(&fixture, words);
        assert_eq!(output.status.code(), Some(0), "{words:?}");
        assert!(!text(&output.stderr).contains("note: "), "{words:?}");
    }
    fixture.end();
    fixture.stop();
}

/// A Task in the fixture whose only run is a passed test run of
/// `./tests/pytest` (r1) that no statement cites, and the Session open.
fn task_with_an_uncited_passed_pytest_run(fixture: &Fixture) {
    let script = fixture.repo.join("tests/pytest");
    fs::write(&script, PYTEST_SCRIPT).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    git(&fixture.repo, &["add", "tests/pytest"]);
    git(
        &fixture.repo,
        &["commit", "--quiet", "-m", "add the pytest runner"],
    );
    fixture.agent_ok(&[
        "begin",
        "--project",
        &fixture.project,
        "--new-task",
        "K5 uncited test run",
    ]);
    fixture.agent_ok(&["run", "--test-format", "pytest", "--", "./tests/pytest"]);
}

/// U8 F1 SF-1 (S-V): the hint depends on the Task's latest passed test run,
/// not on whether the Pack still shows its `Last passed: ` line, which a run
/// that no statement cites loses as soon as a command run is recorded after
/// it.
#[test]
fn u8_k5_run_hints_when_the_last_passed_test_run_is_cited_by_no_statement() {
    let fixture = Fixture::new();
    task_with_an_uncited_passed_pytest_run(&fixture);
    let pack = text(&fixture.agent_ok(&["resume", "--pack"]).stdout);
    assert!(pack.contains("Last passed: 「./tests/pytest」"), "{pack}");

    let output = run(&fixture, &["run", "--", "./tests/pytest"]);
    assert_eq!(output.status.code(), Some(0));
    let stderr = text(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    let last = lines.len() - 1;
    assert!(lines[last].starts_with("memprove: recorded PASS, evidence_id "));
    assert_eq!(lines[last - 1], HINT_PYTEST, "{stderr}");
    assert_eq!(stderr.matches("note: ").count(), 1);
    // Still a command run.
    let record = parse_strict(text(&output.stdout).trim()).unwrap();
    assert_eq!(
        record.get("kind").and_then(JsonValue::as_str),
        Some("command_result")
    );
    fixture.end();
    fixture.stop();
}

/// The same when the test run was made on an earlier commit than the one the
/// repository is at now: the Pack shows no `Last passed: ` line then, and the
/// Task's latest run is still a passed test run of the same argv.
#[test]
fn u8_k5_run_hints_when_the_last_passed_test_run_is_of_an_earlier_commit() {
    let fixture = Fixture::new();
    task_with_an_uncited_passed_pytest_run(&fixture);
    fs::write(fixture.repo.join("tests/other.txt"), "later\n").unwrap();
    git(&fixture.repo, &["add", "tests/other.txt"]);
    git(&fixture.repo, &["commit", "--quiet", "-m", "move on"]);
    let pack = text(&fixture.agent_ok(&["resume", "--pack"]).stdout);
    println!("{pack}");

    let output = run(&fixture, &["run", "--", "./tests/pytest"]);
    assert_eq!(output.status.code(), Some(0));
    let stderr = text(&output.stderr);
    assert_eq!(stderr.matches(HINT_PYTEST).count(), 1, "{stderr}");
    fixture.end();
    fixture.stop();
}

#[test]
fn u8_k5_run_says_nothing_when_the_task_has_no_passed_test_run() {
    let fixture = Fixture::new();
    fixture.agent_ok(&[
        "begin",
        "--project",
        &fixture.project,
        "--new-task",
        "K5 no passed run",
    ]);
    let output = run(&fixture, &["run", "--", "sh", "tests/check.sh"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(!text(&output.stderr).contains("note: "));
    // A failed test run is not a passed one.
    let output = run(
        &fixture,
        &[
            "run",
            "--test",
            "--test-cases",
            "1",
            "--",
            "sh",
            "-c",
            "exit 1",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let output = run(&fixture, &["run", "--", "sh", "-c", "exit 1"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(!text(&output.stderr).contains("note: "));
    fixture.end();
    fixture.stop();
}

const REASON_CHECK: &str = "r2 is a command run and cannot support a fact; a rerun with memprove run --test --test-cases <N> -- sh tests/check.sh can be cited";

#[test]
fn u8_k5_checkpoint_fact_on_a_command_run_sends_nothing_and_gives_one_reason() {
    let fixture = Fixture::new();
    task_with_a_passed_pytest_run(&fixture);
    // r2 is a command run of another command than the test run r1.
    assert_eq!(
        run(&fixture, &["run", "--", "sh", "tests/check.sh"])
            .status
            .code(),
        Some(0)
    );
    let before = fixture.ledger();

    let refused = |words: &[&str], reason: &str| {
        let output = run(&fixture, words);
        assert_eq!(output.status.code(), Some(2), "{words:?}");
        assert_eq!(text(&output.stdout), "", "{words:?}");
        assert_eq!(
            text(&output.stderr),
            format!(
                "memprove: error: {reason}\nmemprove: for the usage of this command, run \
                 `memprove checkpoint create --help`\n"
            ),
            "{words:?}"
        );
    };
    // By label, by `last`, and by default (the latest run is r2).
    refused(
        &[
            "checkpoint",
            "create",
            "--fact",
            "it passes",
            "--evidence",
            "r2",
            "--next",
            "n",
        ],
        REASON_CHECK,
    );
    refused(
        &[
            "checkpoint",
            "create",
            "--fact",
            "it passes",
            "--evidence",
            "last",
            "--next",
            "n",
        ],
        REASON_CHECK,
    );
    refused(
        &["checkpoint", "create", "--fact", "it passes", "--next", "n"],
        REASON_CHECK,
    );
    // The second fact is the one that cites it.
    refused(
        &[
            "checkpoint",
            "create",
            "--fact",
            "a",
            "--evidence",
            "r1",
            "--fact",
            "b",
            "--evidence",
            "r2",
            "--next",
            "n",
        ],
        REASON_CHECK,
    );
    assert_eq!(fixture.ledger(), before, "nothing was sent");

    fixture.end();
    fixture.stop();
}

#[test]
fn u8_k5_the_reason_quotes_the_argv_and_names_the_runner_and_other_facts_go() {
    let fixture = Fixture::new();
    task_with_a_passed_pytest_run(&fixture);
    let refused = |words: &[&str], reason: &str| {
        let output = run(&fixture, words);
        assert_eq!(output.status.code(), Some(2), "{words:?}");
        assert_eq!(text(&output.stdout), "", "{words:?}");
        assert!(
            text(&output.stderr).starts_with(&format!("memprove: error: {reason}\n")),
            "{words:?}: {}",
            text(&output.stderr)
        );
    };
    // The pytest runner as a command run names the pytest format, and its
    // argv is quoted where a shell would need it.
    assert_eq!(
        run(&fixture, &["run", "--", "./tests/pytest", "-k", "a b"])
            .status
            .code(),
        Some(0)
    );
    let before = fixture.ledger();
    refused(
        &[
            "checkpoint",
            "create",
            "--fact",
            "it passes",
            "--evidence",
            "r2",
            "--next",
            "n",
        ],
        "r2 is a command run and cannot support a fact; a rerun with memprove run --test --test-format pytest -- ./tests/pytest -k 'a b' can be cited",
    );
    assert_eq!(fixture.ledger(), before, "nothing was sent");

    // A fact on the test run, a fact that cites the command run besides the
    // test run (the daemon allows extra citations), and an open item on the
    // command run, go.
    let output = run(
        &fixture,
        &[
            "checkpoint",
            "create",
            "--fact",
            "r1 stands",
            "--evidence",
            "r1",
            "--next",
            "n",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let output = run(
        &fixture,
        &[
            "checkpoint",
            "create",
            "--fact",
            "r1 stands, and r2 ran too",
            "--evidence",
            "r2",
            "--evidence",
            "r1",
            "--next",
            "n",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let output = run(
        &fixture,
        &[
            "checkpoint",
            "create",
            "--open",
            "look at it",
            "--evidence",
            "r2",
            "--next",
            "n",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    fixture.end();
    fixture.stop();
}

/// The text of the first content item of the MCP `checkpoint` tool's reply
/// and whether it is an error.
fn mcp_checkpoint(fixture: &Fixture, arguments: &str) -> (String, bool, JsonValue) {
    let request = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{{\"_meta\":{{\"io.modelcontextprotocol/protocolVersion\":\"2026-07-28\",\"io.modelcontextprotocol/clientCapabilities\":{{}}}},\"name\":\"checkpoint\",\"arguments\":{arguments}}}}}\n"
    );
    let mut child = isolated(cli_binary())
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &fixture.data)
        .args(["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = text(&output.stdout);
    println!("mcp checkpoint {arguments}\n{stdout}");
    let reply = parse_strict(stdout.lines().next().unwrap()).unwrap();
    let result = reply.get("result").unwrap_or_else(|| panic!("{stdout}"));
    let first = result
        .get("content")
        .and_then(JsonValue::as_array)
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned();
    let error = result.get("isError") == Some(&JsonValue::Bool(true));
    let structured = result.get("structuredContent").cloned().unwrap();
    (first, error, structured)
}

#[test]
fn u8_k5_mcp_checkpoint_gives_the_same_sentence_as_an_error() {
    let fixture = Fixture::new();
    task_with_a_passed_pytest_run(&fixture);
    assert_eq!(
        run(&fixture, &["run", "--", "sh", "tests/check.sh"])
            .status
            .code(),
        Some(0)
    );
    let before = fixture.ledger();

    let (reason, error, structured) = mcp_checkpoint(
        &fixture,
        r#"{"facts":[{"text":"it passes","evidence":["r2"]}],"next":{"text":"n"}}"#,
    );
    assert!(error);
    assert_eq!(reason, REASON_CHECK);
    assert_eq!(
        structured.get("failure").and_then(JsonValue::as_str),
        Some("command_run_cited")
    );
    // By `last` and by default.
    for arguments in [
        r#"{"facts":[{"text":"it passes","evidence":["last"]}],"next":{"text":"n"}}"#,
        r#"{"facts":[{"text":"it passes"}],"next":{"text":"n"}}"#,
    ] {
        let (reason, error, _) = mcp_checkpoint(&fixture, arguments);
        assert!(error, "{arguments}");
        assert_eq!(reason, REASON_CHECK, "{arguments}");
    }
    assert_eq!(fixture.ledger(), before, "nothing was sent");

    // A fact that cites the command run besides the test run, and a fact on
    // the test run alone, are recorded.
    let (text, error, _) = mcp_checkpoint(
        &fixture,
        r#"{"facts":[{"text":"r1 stands, r2 ran too","evidence":["r2","r1"]}],"next":{"text":"n"}}"#,
    );
    assert!(!error, "{text}");
    let (text, error, _) = mcp_checkpoint(
        &fixture,
        r#"{"facts":[{"text":"r1 stands","evidence":["r1"]}],"next":{"text":"n"}}"#,
    );
    assert!(!error, "{text}");
    assert_ne!(fixture.ledger(), before);
    fixture.end();
    fixture.stop();
}
