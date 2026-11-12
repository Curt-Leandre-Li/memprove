//! U5 P1 (D1 F10, Owner O3; ADR 0014, "Amendment (U5, Z-P)"): through the
//! real binaries, a Pack with no `Rerun: ` line names the Task's last passed
//! test run, `Last passed: 「<argv>」 (<12 digits>; …)`, right after the Session
//! lines: in `resume --pack`, in a query Pack, and in what the `SessionStart`
//! hook injects (still at most 10,000 UTF-16 code units). A Task with no
//! passed test run has no such line, and neither has a Pack whose facts
//! need a rerun. The `ContextManifest`'s `pack_digest` is still the Pack's
//! last line; the Capsule JSON holds no such text.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use p4b_support::{Setup, commit, step, write};
use p4h2_support::{
    HOOK_LIMIT, lines_with, pack_digest, session_start, short, statement_and_pack, units,
};

fn last_passed(pack: &str) -> Vec<&str> {
    lines_with(pack, "Last passed: ")
}

/// The line stands right after the last `Session ` line (or the `Sessions: ` line), before every action, base, and claim line.
fn assert_in_place(pack: &str, line: &str) {
    let lines = pack.lines().collect::<Vec<_>>();
    let at = lines.iter().position(|seen| *seen == line).unwrap();
    assert!(
        lines[at - 1].starts_with("Session ") || lines[at - 1].starts_with("Sessions: "),
        "{pack}"
    );
    assert!(
        lines[at + 1].starts_with("Can: ") || lines[at + 1].starts_with("Cannot: "),
        "{pack}"
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One Task, step by step.
fn u5_p1_the_pack_names_the_last_passed_test_run_when_no_rerun_is_due() {
    let setup = Setup::new(&["a", "b"]);
    let agent = &setup.agent;
    agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u5 p1",
    ]);

    step("no run yet, then a command run only: no line");
    assert!(last_passed(&agent.bytes(&["resume", "--pack"])).is_empty());
    agent.command("tests/b.sh");
    assert!(last_passed(&agent.bytes(&["resume", "--pack"])).is_empty());

    step("a passed test run, a fact on it, then a command run (D1's order)");
    let label = agent.test("tests/a.sh");
    agent.bytes(&[
        "checkpoint",
        "create",
        "--fact",
        "tests/a.sh passes",
        "--evidence",
        &label,
        "--next",
        "go on",
    ]);
    agent.command("tests/b.sh");
    let head = p4b_support::git(&setup.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let expected = format!(
        "Last passed: 「sh tests/a.sh」 ({}; --test --test-format cargo)",
        short(&head)
    );
    let pack = agent.bytes(&["resume", "--pack"]);
    println!("{pack}");
    assert!(pack.contains("): verified; "), "{pack}");
    assert_eq!(last_passed(&pack), [expected.as_str()]);
    assert_in_place(&pack, &expected);
    assert!(lines_with(&pack, "Rerun: ").is_empty());

    step("the Pack's last line is its digest");
    assert_eq!(
        pack.lines().last().unwrap(),
        format!("Digest: {}", pack_digest(&pack))
    );

    step("a query Pack has the same line");
    let query = agent.bytes(&["resume", "--pack", "--query", "passes"]);
    println!("{query}");
    assert!(query.contains("Query: 「passes」; 1 matches"), "{query}");
    assert_eq!(last_passed(&query), [expected.as_str()]);
    assert_in_place(&query, &expected);

    step("the hook injects it, within 10,000 UTF-16 code units");
    let hook = session_start(&setup, "u5-p1-client", "startup");
    assert_eq!(hook.code, Some(0));
    assert!(units(&hook.stdout) <= HOOK_LIMIT, "{}", units(&hook.stdout));
    let (_, injected) = statement_and_pack(&hook.stdout);
    assert_eq!(last_passed(&injected), [expected.as_str()]);
    assert_in_place(&injected, &expected);

    step("the Capsule JSON holds no such text");
    let capsule = agent.bytes(&["resume"]);
    assert!(!capsule.contains("Last passed"), "{capsule}");

    step("a new commit makes the fact stale: the rerun line names the argv instead");
    write(&setup.repo.join("README"), "fixture, changed\n");
    commit(&setup.repo, "change the README");
    let pack = agent.bytes(&["resume", "--pack"]);
    println!("{pack}");
    assert_eq!(lines_with(&pack, "Rerun: ").len(), 1, "{pack}");
    assert!(last_passed(&pack).is_empty(), "{pack}");
    let query = agent.bytes(&["resume", "--pack", "--query", "passes"]);
    assert!(last_passed(&query).is_empty(), "{query}");
    let hook = session_start(&setup, "u5-p1-client-2", "startup");
    let (_, injected) = statement_and_pack(&hook.stdout);
    assert!(last_passed(&injected).is_empty(), "{injected}");
}
