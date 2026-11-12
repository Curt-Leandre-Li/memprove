//! P4-H2: acceptance scenario 7 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > With the same ledger high-water mark, probe result, and budget, the Pack
//! > is byte for byte the same, and carries its digest.
//!
//! A Task with a verified fact, a stale fact, an open item, a Decision, and
//! a next action; its Session is still open. With the ledger's high-water
//! mark, the probe's result, and the budget the same, every read gives the
//! same bytes: `resume --pack` three times and with `--budget 9000`, the
//! operator's `resume --task <id> --pack` from another context with a
//! control token, the MCP `pack` tool with `budget` 9000 (twice), the
//! `SessionStart` hook, and `resume --pack` after the daemon restarts. The
//! last line is the digest, the SHA-256 of every byte before it, and the
//! tool reports the same digest. Another budget gives another digest (the
//! budget is on the first line); the tool's default budget (6000) gives
//! what `resume --pack --budget 6000` gives. An untracked file changes the
//! probe's result, not the ledger, and the Pack and its digest change; with
//! the file gone, the Pack is the first one again, byte for byte. No read
//! writes to the ledger.
//!
//! U7 P1 (Owner O3; the Owner's decision of 2026-10-21 amends this
//! scenario): a Pack names, at the end of the line of an open Session that
//! is not the reader's own, how to end it before taking over. So the reads
//! whose context does not name the agent's open Session (the operator's,
//! and the hook of a new client session) differ from the agent's Pack only
//! in that: the line of that Session ends with the takeover hint, and every
//! other line but the digest line is the same, byte for byte. Reads in the
//! agent's context (`resume --pack`, the MCP tool) give the same bytes.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4g_support;
mod p4h2_support;

use std::fs;

use p4b_support::{Setup, commit, git, step, write};
use p4g_support::{Mcp, McpEnv};
use p4h2_support::{
    BUDGET, budget_of, high_water, ledger_holds, number_of, omission, pack_digest, sequence_of,
    session_start, statement_and_pack, units,
};
use serde_json::{Value, json};

const CLIENT: &str = "client-7";
const FA: &str = "FA the a check passes";
const FB: &str = "FB the b check passes";

/// Session 1 at X1: facts on `a` and `b`, an open item, a Decision; after a
/// commit (X2) `a` is rerun and its fact reaffirmed, so the fact on `b` is
/// stale. The Session stays open. Returns the Task.
fn record(setup: &Setup) -> String {
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 7: the same inputs, the same Pack",
    ]);
    let (a, b) = (agent.test("tests/a.sh"), agent.test("tests/b.sh"));
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        FA,
        "--evidence",
        &a,
        "--fact",
        FB,
        "--evidence",
        &b,
        "--open",
        "O1 the c check is not written yet",
        "--next",
        "N1 commit the README change",
    ]);
    agent.ok(&["decide", "D1 keep the checks as sh scripts"]);
    write(&setup.repo.join("README"), "fixture\nX2\n");
    commit(&setup.repo, "X2");
    let a = agent.test("tests/a.sh");
    let capsule = agent.capsule();
    agent.ok(&[
        "checkpoint",
        "create",
        "--reaffirm",
        &number_of(&capsule, FA).to_string(),
        "--evidence",
        &a,
        "--next",
        "N2 rerun b",
    ]);
    begun["task_id"].as_str().unwrap().to_owned()
}

/// The MCP server as Claude Code spawns it for the agent.
fn server(setup: &Setup) -> Mcp {
    Mcp::start(&McpEnv::of(
        &setup.agent.config_dir,
        &setup.agent.data_dir,
        &setup.agent.token,
        &setup.repo,
    ))
}

/// The tool's `pack` with `arguments`: its text, and its structured
/// `pack_digest`, `capsule_digest`, `project_sequence`, and `budget`.
fn tool_pack(mcp: &mut Mcp, arguments: &Value) -> (String, Value) {
    let called = mcp.ok("pack", arguments);
    let structured = &called.structured;
    (
        called.text.clone(),
        json!([
            structured["pack_digest"],
            structured["capsule_digest"],
            structured["project_sequence"],
            structured["budget"],
        ]),
    )
}

/// U7 P1 (Owner O3, amended 2026-10-21): `other`, read by a reader for
/// whom the agent's open Session is someone else's, differs from the
/// agent's `pack` only there: that Session's line ends with the takeover
/// hint, and every other line but the digest line is the same.
fn only_the_takeover_hint(other: &str, pack: &str) {
    let lines = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with("Digest: "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let (theirs, ours) = (lines(other), lines(pack));
    assert_eq!(theirs.len(), ours.len(), "{other}");
    for (their, our) in theirs.iter().zip(&ours) {
        if their != our {
            assert!(
                our.starts_with("Session ") && our.contains(" not ended ("),
                "{our}"
            );
            // U10 T7: the hint names that Session's ID and number.
            assert_eq!(*their, format!("{our}{}", p4b_support::takeover_hint(our)));
        }
    }
    assert_eq!(
        other.matches("; end it before taking over: ").count(),
        1,
        "{other}"
    );
    assert!(!pack.contains("; end it before taking over: "), "{pack}");
}

/// The lines of `pack` between the first line and the digest line.
fn middle(pack: &str) -> Vec<&str> {
    let lines = pack.lines().collect::<Vec<_>>();
    lines[1..lines.len() - 1].to_vec()
}

#[test]
fn scenario_7_the_same_ledger_probe_and_budget_give_the_same_pack_and_digest() {
    let mut setup = Setup::new(&["a", "b"]);
    let agent = setup.agent.clone();
    step("scenario 7: a verified and a stale fact, a Decision, its Session open");
    let task = record(&setup);
    let capsule = agent.capsule();
    let n = high_water(&setup);
    let head = git(&setup.repo, &["rev-parse", "HEAD"]).trim().to_owned();

    step("resume --pack, three times, and with --budget 9000");
    let pack = agent.bytes(&["resume", "--pack"]);
    println!("{pack}");
    for _ in 0..2 {
        assert_eq!(agent.bytes(&["resume", "--pack"]), pack);
    }
    assert_eq!(agent.bytes(&["resume", "--pack", "--budget", "9000"]), pack);
    let digest = pack_digest(&pack);
    assert_eq!(sequence_of(&pack), n);
    assert_eq!(budget_of(&pack), BUDGET);
    assert!(
        pack.starts_with(&format!(
            "Pack: memprove-pack v1; capsule_digest {}; ledger sequence {n}; probed HEAD {head}; ",
            capsule["capsule_digest"].as_str().unwrap()
        )),
        "{pack}"
    );
    assert!(units(&pack) <= BUDGET);
    assert!(pack.contains("verified: 「FA "), "{pack}");
    assert!(pack.contains("fact stale: 「FB "), "{pack}");

    step("the operator, from another context, with a control token");
    let operator = setup.operator.clone();
    // U7 P1 (amended 2026-10-21): the token does not change the Pack; the
    // other context changes only the takeover hint on the agent's open
    // Session.
    only_the_takeover_hint(
        &operator.bytes(&["resume", "--task", &task, "--pack"]),
        &pack,
    );

    step("the MCP pack tool with budget 9000, twice");
    let mut mcp = server(&setup);
    let expected = json!([digest, capsule["capsule_digest"], n, 9000]);
    for _ in 0..2 {
        let (text, said) = tool_pack(&mut mcp, &json!({"budget": 9000}));
        assert_eq!(text, pack, "the tool returns resume --pack");
        assert_eq!(said, expected);
    }
    step("the tool's default budget, 6000: another budget, another digest");
    let (other, said) = tool_pack(&mut mcp, &json!({}));
    assert_eq!(said[3], 6000);
    assert_eq!(
        other,
        agent.bytes(&["resume", "--pack", "--budget", "6000"])
    );
    assert_ne!(pack_digest(&other), digest);
    assert_eq!(said[0], pack_digest(&other).as_str());
    mcp.finish();

    step("the SessionStart hook");
    let run = session_start(&setup, CLIENT, "startup");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let (_, injected) = statement_and_pack(&run.stdout);
    // U7 P1 (amended 2026-10-21): a new client session's Pack.
    only_the_takeover_hint(&injected, &pack);

    step("after the daemon restarts");
    setup.stop();
    setup.start();
    assert_eq!(agent.bytes(&["resume", "--pack"]), pack);

    step("other budgets: other digests, the same claims at level 0");
    let digests = [9_001, 1_000_000]
        .map(|budget| {
            let other = agent.bytes(&["resume", "--pack", "--budget", &budget.to_string()]);
            assert_eq!(omission(&other).level, 0);
            assert_eq!(middle(&other), middle(&pack), "only the first line differs");
            pack_digest(&other)
        })
        .to_vec();
    let shorter = agent.bytes(&[
        "resume",
        "--pack",
        "--budget",
        &(units(&pack) - 1).to_string(),
    ]);
    assert!(omission(&shorter).level > 0);
    let mut all = digests.clone();
    all.extend([digest.clone(), pack_digest(&shorter)]);
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 4, "four budgets, four digests");

    step("the same ledger, another probe result: an untracked file");
    let untracked = setup.repo.join("notes.txt");
    fs::write(&untracked, "a note\n").unwrap();
    let probed = agent.bytes(&["resume", "--pack"]);
    assert_eq!(sequence_of(&probed), n);
    assert_ne!(pack_digest(&probed), digest);
    assert_ne!(agent.capsule()["capsule_digest"], capsule["capsule_digest"]);
    fs::remove_file(&untracked).unwrap();
    assert_eq!(
        agent.bytes(&["resume", "--pack"]),
        pack,
        "the same probe result again: the same bytes"
    );

    assert_eq!(high_water(&setup), n, "a read wrote to the ledger");
    ledger_holds(&setup);
    setup.stop();
}
