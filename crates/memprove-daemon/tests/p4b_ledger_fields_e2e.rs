//! P4-B (ADR 0014 E8) through the real binaries: the fields the Task
//! Capsule and the Recovery Preview take from rows that no check covers are
//! now read from `memproved`'s Events, so rewriting those rows behind the
//! daemon changes no byte of `resume` (JSON), `resume --text` (with and
//! without `--all`), or `recover preview` (JSON and `--text`).
//!
//! Fields: the Task's title and Workspace (`task.created@1`), and each
//! Session's start, Workspace, end, and so its number (`session.started`,
//! `session.ended`, `session.crashed`). Each row is rewritten alone on a
//! copy of the state database, then all of them at once.
//!
//! U1 B1 (T21 V1R-1): a Session row's start and Workspace are now also
//! compared with its `session.started` Event, so rewriting either makes
//! the Session a damaged record and the Task `tampered`; those rewrites
//! (and all of them at once) change the outputs, and the others still do
//! not. `ledger verify`, which does not name Sessions, still exits 0.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, step};
use serde_json::Value;

/// Everything the four reads print, in one string per read.
fn outputs(setup: &Setup) -> Vec<(&'static str, String)> {
    let agent = &setup.agent;
    vec![
        ("resume", agent.bytes(&["resume"])),
        ("resume --text", agent.bytes(&["resume", "--text"])),
        (
            "resume --text --all",
            agent.bytes(&["resume", "--text", "--all"]),
        ),
        ("recover preview", agent.bytes(&["recover", "preview"])),
        (
            "recover preview --text",
            agent.bytes(&["recover", "preview", "--text"]),
        ),
    ]
}

/// The identities the rewrites name.
struct Ids {
    task_id: String,
    workspace_id: String,
    first: String,
    second: String,
    third: String,
}

/// Session 1: a fact on a passed test, ended; Session 2: an open item,
/// abandoned; Session 3: an open item, open.
fn three_sessions(setup: &Setup) -> Ids {
    let agent = &setup.agent;
    step("Session 1: a fact on a passed test, ended; Session 2: an open item, abandoned");
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "Ledger fields",
    ]);
    let id = |value: &Value, name: &str| value[name].as_str().unwrap().to_owned();
    agent.test("tests/a.sh");
    agent.ok(&["checkpoint", "create", "--fact", "a passes", "--next", "n1"]);
    agent.ok(&["end", "--reason", "done"]);
    let second = id(&agent.ok(&["begin"]), "session_id");
    agent.test("tests/a.sh");
    agent.ok(&["checkpoint", "create", "--open", "o1", "--next", "n2"]);
    agent.ok(&["end", "--abandoned", "--reason", "left"]);
    step("Session 3: open");
    let third = id(&agent.ok(&["begin"]), "session_id");
    agent.test("tests/a.sh");
    agent.ok(&["checkpoint", "create", "--open", "o2", "--next", "n3"]);
    Ids {
        task_id: id(&begun, "task_id"),
        workspace_id: id(&begun, "workspace_id"),
        first: id(&begun, "session_id"),
        second,
        third,
    }
}

/// Each row rewrite, named, then all of them at once, with the Session
/// it makes a damaged record (U1 B1), if any.
fn rewrites(ids: &Ids) -> Vec<(&'static str, String, Option<String>)> {
    let Ids {
        task_id,
        workspace_id,
        first,
        third,
        ..
    } = ids;
    let other = "01a10000-0000-7000-8000-00000000f00d";
    let mut rewrites = vec![
        (
            "the Task's title",
            format!("UPDATE tasks SET title = 'forged title' WHERE task_id = '{task_id}';"),
            None,
        ),
        (
            "the Task's Workspace",
            format!(
                "UPDATE tasks SET workspace_id = '{workspace_id}' WHERE task_id = '{task_id}';"
            ),
            None,
        ),
        (
            "a Session's start (and so the numbering)",
            format!(
                "UPDATE sessions SET started_at = '2001-01-01T00:00:00Z' \
                 WHERE session_id = '{third}';"
            ),
            Some(third.clone()),
        ),
        (
            "an ended Session's end",
            format!(
                "UPDATE sessions SET ended_at = '2030-01-01T00:00:00Z' \
                 WHERE session_id = '{first}';"
            ),
            None,
        ),
        (
            "an open Session's end",
            format!(
                "UPDATE sessions SET ended_at = '2030-01-01T00:00:00Z' \
                 WHERE session_id = '{third}';"
            ),
            None,
        ),
        (
            "a Session's Workspace",
            format!("UPDATE sessions SET workspace_id = '{other}' WHERE session_id = '{third}';"),
            Some(third.clone()),
        ),
    ];
    let all = rewrites
        .iter()
        .map(|(_, sql, _)| sql.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    rewrites.push(("every row at once", all, Some(third.clone())));
    rewrites
}

#[test]
fn rewritten_rows_change_no_output_through_real_binaries() {
    let mut setup = Setup::new(&["a"]);
    let agent = setup.agent.clone();
    let ids = three_sessions(&setup);
    let before = outputs(&setup);
    let capsule: Value = serde_json::from_str(&before[0].1).unwrap();
    assert_eq!(capsule["task"]["title"], "Ledger fields");
    assert_eq!(capsule["task"]["workspace_id"], Value::Null);
    let states = capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| {
            (
                session["session_id"].as_str().unwrap(),
                session["end_state"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        [
            (ids.first.as_str(), "ended"),
            (ids.second.as_str(), "crashed"),
            (ids.third.as_str(), "open"),
        ]
    );
    assert!(before[1].1.contains("Session 3 "), "{}", before[1].1);
    setup.stop();
    let pristine = setup.save("pristine");
    let mut changed = Vec::new();
    for (name, sql, damaged) in rewrites(&ids) {
        step(&format!("rewrite {name}"));
        setup.restore(&pristine);
        setup.tamper(&sql);
        setup.start();
        let after = outputs(&setup);
        if let Some(session) = damaged {
            // U1 B1: the Session is a damaged record; the Task `tampered`.
            let capsule: Value = serde_json::from_str(&after[0].1).unwrap();
            assert_eq!(capsule["status"], "tampered", "{name}");
            assert!(
                capsule["damaged_records"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|record| record["record_type"] == "session"
                        && record["record_id"] == session.as_str()),
                "{name}: {capsule}"
            );
            let preview: Value = serde_json::from_str(&after[3].1).unwrap();
            assert_eq!(preview["status"], "tampered", "{name}");
        } else {
            for ((read, expected), (_, got)) in before.iter().zip(&after) {
                if got != expected {
                    println!("CHANGED: {name}: `{read}`");
                    changed.push(format!("{name}: `{read}`"));
                }
            }
        }
        let verified = agent.run(&["ledger", "verify"], true);
        assert_eq!(verified.code, Some(0), "{name}: {}", verified.stdout);
        setup.stop();
    }
    assert!(changed.is_empty(), "outputs changed: {changed:#?}");
}
