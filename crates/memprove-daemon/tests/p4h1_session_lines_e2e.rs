//! P4-H1 (Owner, T20) through the real binaries: which Sessions the
//! Context Pack lists. Nine Sessions of one Task, numbered by start: ended
//! normally (1, 4, 8), ended abandoned (2, 3, 5, 6), and open (7, created
//! outside the context, and 9); 4 and 9 declared the Pack they resume
//! from. `resume --pack` lists the open ones, the latest that ended (8),
//! and the latest three that ended abandoned (6, 5, 3); 1, 2, and 4 are
//! only counted on the `Sessions: ` line, which also counts the two
//! Sessions that declared a Pack. A declared digest shows its first 12
//! digits, and the lines are the same at a budget that shortens bodies.
//! U7 P1 (Owner O3, amended by the Owner's decision of 2026-10-21): 7 is
//! open and not the Session the CLI's context names (9 is), so its line
//! ends with the takeover hint; 9's does not.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, step};
use serde_json::Value;

/// The `Sessions: ` and `Session ` lines of a Pack.
fn session_part(pack: &str) -> Vec<String> {
    pack.lines()
        .filter(|line| line.starts_with("Sessions: ") || line.starts_with("Session "))
        .map(str::to_owned)
        .collect()
}

/// The level the `Omitted: ` line names.
fn level(pack: &str) -> u32 {
    let line = pack
        .lines()
        .find(|line| line.starts_with("Omitted: level "))
        .unwrap();
    line["Omitted: level ".len()..]
        .split([' ', ';'])
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// The line the Pack gives Session `number` of `capsule`, by the formats of
/// P4-C2, E6, and P4-H1.
fn expected_line(capsule: &Value, number: usize) -> String {
    let session = &capsule["sessions"][number - 1];
    let id = session["session_id"].as_str().unwrap();
    let started = session["started_at"].as_str().unwrap();
    let state = match session["end_state"].as_str().unwrap() {
        "ended" => format!("ended normally (started {started})"),
        "crashed" => format!("ended abandoned (started {started})"),
        _ => format!(
            "not ended (started {started}, {})",
            session["last_recorded_at"].as_str().map_or_else(
                || "nothing of it in the ledger yet".to_owned(),
                |at| format!("last wrote to the ledger at {at}")
            )
        ),
    };
    let from = &session["resumed_from"];
    let resumed = if from.is_null() {
        String::new()
    } else {
        format!(
            "; continues from the Pack at ledger sequence {} (self-reported, digest {})",
            from["project_sequence"],
            &from["pack_digest"].as_str().unwrap()[.."sha256:".len() + 12]
        )
    };
    format!("Session {number} {id} {state}{resumed}")
}

/// Starts and ends the nine Sessions with the agent's CLI, and records a
/// run and a fact with a long body in Session 9; returns the Session IDs in
/// the order they started.
fn nine_sessions(setup: &Setup) -> Vec<String> {
    let agent = setup.agent.clone();
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "many Sessions",
    ]);
    let task = begun["task_id"].as_str().unwrap().to_owned();
    let workspace = begun["workspace_id"].as_str().unwrap().to_owned();
    let agent_id = begun["agent_id"].as_str().unwrap().to_owned();
    let mut ids = vec![begun["session_id"].as_str().unwrap().to_owned()];
    let session_of = |begun: &Value| begun["session_id"].as_str().unwrap().to_owned();
    // `abandoned` ends the context's Session with session.crashed.
    let end = |abandoned: bool| {
        let words: &[&str] = if abandoned {
            &["end", "--abandoned", "--reason", "left unfinished"]
        } else {
            &["end", "--reason", "done"]
        };
        agent.bytes(words);
    };
    step("Session 1 ends normally, 2 and 3 abandoned");
    end(false);
    for _ in 0..2 {
        ids.push(session_of(&agent.ok(&["begin"])));
        end(true);
    }
    step("Session 4 declares the Pack it read and ends normally");
    agent.bytes(&["resume", "--pack"]);
    ids.push(session_of(&agent.ok(&["begin"])));
    end(false);
    step("Session 5 and 6 end abandoned; 7 starts outside the context");
    for _ in 0..2 {
        ids.push(session_of(&agent.ok(&["begin"])));
        end(true);
    }
    ids.push(session_of(&agent.ok(&[
        "session",
        "create",
        "--workspace",
        &workspace,
        "--agent",
        &agent_id,
        "--task",
        &task,
    ])));
    step("Session 8 ends normally; 9 declares the Pack it read and stays open");
    ids.push(session_of(&agent.ok(&["begin"])));
    end(false);
    agent.bytes(&["resume", "--pack"]);
    ids.push(session_of(&agent.ok(&["begin"])));
    agent.test("tests/a.sh");
    let long = format!("the a check passes; {}", "the detail goes on. ".repeat(20));
    agent.bytes(&["checkpoint", "create", "--fact", &long, "--next", "go on"]);
    ids
}

#[test]
fn the_pack_lists_open_sessions_the_latest_ended_and_three_latest_abandoned() {
    let setup = Setup::new(&["a"]);
    let agent = setup.agent.clone();
    let ids = nine_sessions(&setup);
    let capsule = agent.capsule();
    let sessions = capsule["sessions"].as_array().unwrap();
    assert_eq!(
        sessions
            .iter()
            .map(|session| session["session_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        sessions
            .iter()
            .map(|session| session["end_state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ended", "crashed", "crashed", "ended", "crashed", "crashed", "open", "ended", "open"
        ]
    );
    let declared = sessions
        .iter()
        .zip(1..)
        .filter(|(session, _)| !session["resumed_from"].is_null())
        .map(|(_, number)| number)
        .collect::<Vec<_>>();
    assert_eq!(declared, [4, 9]);

    step("resume --pack");
    let pack = agent.bytes(&["resume", "--pack"]);
    print!("{pack}");
    let mut expected = vec![
        "Sessions: 9 in all, 3 ended normally, 4 ended abandoned, 2 not ended, 2 declared a resumed_from Pack"
            .to_owned(),
    ];
    // U7 P1: Session 7, open outside the context, ends with the takeover hint.
    expected.extend([3, 5, 6, 7, 8, 9].map(|number| {
        let line = expected_line(&capsule, number);
        if number == 7 {
            format!("{line}{}", p4b_support::takeover_hint(&line))
        } else {
            line
        }
    }));
    assert_eq!(session_part(&pack), expected);
    assert_eq!(level(&pack), 0);
    for (number, id) in ids.iter().enumerate().take(4) {
        if number != 2 {
            assert!(
                !pack.contains(id.as_str()),
                "Session {} is listed",
                number + 1
            );
        }
    }
    let full = sessions[8]["resumed_from"]["pack_digest"].as_str().unwrap();
    assert!(!pack.contains(full), "the whole declared digest");

    step("a budget that shortens the fact keeps every Session line");
    let length = pack.encode_utf16().count();
    let budget = (length - 40).to_string();
    let shorter = agent.bytes(&["resume", "--pack", "--budget", &budget]);
    assert!(level(&shorter) >= 2, "{shorter}");
    assert_eq!(session_part(&shorter), expected);

    let project = setup.project_id.clone();
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &project]);
    assert_eq!(verified["valid"], true);
}
