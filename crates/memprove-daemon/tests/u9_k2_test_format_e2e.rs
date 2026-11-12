//! U9 K2 (U8 V-N5; Owner O6; ADR 0015, "Amendment (U9, S-K)") through the
//! real binaries: `memprove run --test-format cargo` and `--test-cases`
//! record the format they were made with (`cargo`, `pytest`, or `cases`),
//! and the arguments the Pack's `Last passed: ` line and the hint of `run` name
//! for a rerun are that format's, not a guess from the argv. `sh
//! tests/check.sh` is no cargo command, yet recorded with `--test-format
//! cargo` its line says `--test --test-format cargo`.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, step};
use serde_json::Value;

fn last_passed(pack: &str) -> Vec<&str> {
    pack.lines()
        .filter(|line| line.starts_with("Last passed: "))
        .collect()
}

fn completed(operator: &p4b_support::Cli, project: &str) -> Vec<Value> {
    let page = operator.ok(&["event", "list", "--project", project, "--limit", "500"]);
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event_type"] == "command.completed")
        .cloned()
        .collect()
}

/// The Events of the Task's three runs: `command.completed@2` with the member
/// for the first two, `@1` for the command run; and the ledger holds.
fn the_events_name_the_format(setup: &Setup) {
    let events = completed(&setup.operator, &setup.project_id);
    let shape = events
        .iter()
        .map(|event| {
            (
                event["payload_schema"].as_str().unwrap().to_owned(),
                event["payload"]["command"]["test_format"]
                    .as_str()
                    .unwrap_or("-")
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let expected = [
        ("command.completed@2", "cases"),
        ("command.completed@2", "cargo"),
        ("command.completed@1", "-"),
    ]
    .map(|(schema, format)| (schema.to_owned(), format.to_owned()));
    assert_eq!(shape, expected);
    assert_eq!(
        setup
            .operator
            .ok(&["ledger", "verify", "--project", &setup.project_id])["valid"],
        true
    );
}

/// A fact on the cargo run: the Pack's line, the Capsule's member, and the
/// hint of `run` all name `--test-format cargo`. Returns the 12 digits of
/// the commit.
fn the_cargo_run_is_named(setup: &Setup, cargo_label: &str) -> String {
    let agent = &setup.agent;
    agent.bytes(&[
        "checkpoint",
        "create",
        "--fact",
        "check.sh passes",
        "--evidence",
        cargo_label,
        "--next",
        "go on",
    ]);
    let head = p4b_support::git(&setup.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let short = head[..12].to_owned();
    let pack = agent.bytes(&["resume", "--pack"]);
    println!("{pack}");
    // The latest run is the command run; the cited run is the cargo one.
    assert_eq!(
        last_passed(&pack),
        [
            format!("Last passed: 「sh tests/check.sh」 ({short}; --test --test-format cargo)")
                .as_str()
        ],
        "{pack}"
    );

    step("the Capsule lists the format on the cited Evidence");
    let capsule: Value = serde_json::from_str(agent.bytes(&["resume"]).trim()).unwrap();
    let cited = &capsule["claims"][0]["evidence"][0];
    assert_eq!(cited["test_format"], "cargo", "{cited}");

    step("the same argv as a command run: the hint names the recorded format");
    let run = agent.run(&["run", "--", "sh", "tests/check.sh"], true);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let hint = run
        .stderr
        .lines()
        .find(|line| line.contains("note: "))
        .unwrap_or_else(|| panic!("no hint: {}", run.stderr));
    assert!(
        hint.ends_with("--test --test-format cargo records it as a test run"),
        "{hint}"
    );
    short
}

/// A fact on a later `cases` run: the Pack's line is `--test --test-cases
/// <N>`.
fn the_cases_run_is_named(setup: &Setup, short: &str) {
    let agent = &setup.agent;
    let (cases_label, _) =
        agent.run_command(&["--test", "--test-cases", "1"], &["sh", "tests/check.sh"]);
    agent.bytes(&[
        "checkpoint",
        "create",
        "--fact",
        "check.sh passes, counted by hand",
        "--evidence",
        &cases_label,
        "--supersedes",
        "1",
        "--next",
        "go on",
    ]);
    let pack = agent.bytes(&["resume", "--pack"]);
    println!("{pack}");
    assert_eq!(
        last_passed(&pack),
        [
            format!("Last passed: 「sh tests/check.sh」 ({short}; --test --test-cases <N>)")
                .as_str()
        ],
        "{pack}"
    );
}

#[test]
fn u9_k2_a_run_records_its_test_format_and_the_pack_and_hint_name_it() {
    let setup = Setup::new(&["check"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u9 k2",
    ]);

    step("a run with --test --test-cases records `cases`, one with --test-format cargo `cargo`");
    let (_, cases) = agent.run_command(&["--test", "--test-cases", "1"], &["sh", "tests/check.sh"]);
    assert_eq!(cases["payload"]["test_format"], "cases", "{cases}");
    let (cargo_label, cargo) =
        agent.run_command(&["--test-format", "cargo"], &["sh", "tests/check.sh"]);
    assert_eq!(cargo["payload"]["test_format"], "cargo", "{cargo}");
    let (_, command) = agent.run_command(&[], &["sh", "tests/check.sh"]);
    assert!(command["payload"].get("test_format").is_none(), "{command}");

    step("the Events: @2 with the member for the first two, @1 for the command run");
    the_events_name_the_format(&setup);

    step("a fact on the cargo run: the Pack's line names --test-format cargo");
    let short = the_cargo_run_is_named(&setup, &cargo_label);

    step("a fact on a later `cases` run: the line is --test --test-cases <N>");
    the_cases_run_is_named(&setup, &short);

    step("`--test` alone records no count and no format");
    let (_, bare) = agent.run_command(&["--test"], &["sh", "tests/check.sh"]);
    assert!(bare["payload"].get("test_format").is_none(), "{bare}");
    let events = completed(&setup.operator, &setup.project_id);
    assert_eq!(
        events.last().unwrap()["payload_schema"],
        "command.completed@1"
    );
}

/// A Task whose runs record no format has no `test_format` in its Capsule
/// or its Events, and its Pack line is inferred from the argv as before.
#[test]
fn u9_k2_a_task_without_a_recorded_format_is_unchanged() {
    let setup = Setup::new(&["check"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u9 k2 plain",
    ]);
    agent.command("tests/check.sh");
    let capsule = agent.bytes(&["resume"]);
    assert!(!capsule.contains("test_format"), "{capsule}");
    let events = completed(&setup.operator, &setup.project_id);
    assert!(
        events
            .iter()
            .all(|event| event["payload_schema"] == "command.completed@1"),
        "{events:?}"
    );
}
