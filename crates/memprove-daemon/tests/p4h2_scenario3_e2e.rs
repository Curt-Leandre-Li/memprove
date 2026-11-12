//! P4-H2: acceptance scenario 3 (ADR 0014, Owner, original text) through
//! the real binaries:
//!
//! > The new session's Session records which Pack it continues from (its
//! > digest and the ledger high-water mark), and later Packs show it.
//!
//! Session 1 records a fact and ends. The agent reads `resume --pack` and
//! runs `begin`: Session 2 declares that Pack as `resumed_from` (its
//! `capsule_digest`, the ledger high-water mark N it names, its
//! `pack_digest`, and its budget), and `memproved` records
//! `capsule.recovered` with the Session. A later Pack shows on Session 2's
//! line the ledger sequence and the first 12 digits of the declared digest,
//! and counts the declaring Sessions on its summary line. Session 3 starts
//! the way Claude Code starts one (the `SessionStart` hook injects the Pack,
//! then `begin` as that client session) and declares the injected Pack; the
//! Pack after it counts two declaring Sessions, the listed one with its own
//! digest.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;
mod p4h2_support;

use p4b_support::{Setup, sequence, step};
use p4h2_support::{
    as_client, ledger_holds, line_with, ok_as, session_line, session_part, session_start,
    statement_and_pack,
};
use serde_json::{Value, json};

/// The client session Claude Code starts for Session 3.
const CLIENT: &str = "client-3";

/// What a Pack names of itself: the `capsule_digest` and ledger sequence
/// of its first line, the `pack_digest` of its last.
struct Named {
    capsule_digest: String,
    project_sequence: u64,
    pack_digest: String,
}

fn named(pack: &str) -> Named {
    let first = pack.lines().next().unwrap();
    let field = |label: &str| {
        first
            .split("; ")
            .find_map(|part| part.strip_prefix(label))
            .unwrap_or_else(|| panic!("no {label} in {first}"))
            .to_owned()
    };
    Named {
        capsule_digest: field("capsule_digest "),
        project_sequence: field("ledger sequence ").parse().unwrap(),
        pack_digest: pack
            .lines()
            .last()
            .unwrap()
            .strip_prefix("Digest: ")
            .unwrap()
            .to_owned(),
    }
}

/// `resumed_from` of Session `number` (from 1) in `capsule`.
fn resumed_from(capsule: &Value, number: usize) -> &Value {
    &capsule["sessions"][number - 1]["resumed_from"]
}

/// The declaration of `pack`, as the Capsule and the Event carry it.
fn declaration(pack: &Named) -> Value {
    json!({
        "capsule_digest": pack.capsule_digest,
        "project_sequence": pack.project_sequence,
        "pack_digest": pack.pack_digest,
        "budget": 9000,
        "declared": true,
    })
}

/// The `capsule.recovered` Events of the Project, in ledger order.
fn recovered(setup: &Setup) -> Vec<Value> {
    let events = setup.operator.ok(&[
        "event",
        "list",
        "--project",
        &setup.project_id,
        "--limit",
        "500",
    ]);
    events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event_type"] == "capsule.recovered")
        .cloned()
        .collect()
}

/// The Event `memproved` wrote for `session` declaring `pack` of `task`.
fn check_event(event: &Value, task: &str, session: &Value, pack: &Named) {
    assert_eq!(event["session_id"], *session);
    assert_eq!(event["producer"]["name"], "continuityd");
    assert_eq!(event["payload_schema"], "capsule.recovered@1");
    let mut payload = declaration(pack);
    payload["task_id"] = json!(task);
    assert_eq!(event["payload"], payload);
}

/// Session 1: one run, one fact, end. Returns the Task.
fn first_session(setup: &Setup) -> String {
    let agent = &setup.agent;
    let begun = agent.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "scenario 3",
    ]);
    agent.test("tests/a.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F1 a passes",
        "--next",
        "N1",
    ]);
    agent.ok(&["end", "--reason", "Session 1 is done"]);
    begun["task_id"].as_str().unwrap().to_owned()
}

/// `resume --pack`, then `begin`: Session 2 declares that Pack. Returns
/// the Pack's names and Session 2.
fn second_session(setup: &Setup, task: &str) -> (Named, Value) {
    let agent = &setup.agent;
    let capsule = agent.capsule();
    let pack = agent.bytes(&["resume", "--pack"]);
    let read = named(&pack);
    assert_eq!(
        read.capsule_digest,
        capsule["capsule_digest"].as_str().unwrap()
    );
    assert_eq!(read.project_sequence, sequence(&capsule));
    let begun = agent.run(&["begin"], false);
    assert_eq!(begun.code, Some(0), "{}", begun.stderr);
    assert!(
        begun.stderr.contains(&format!(
            "memprove: the Session declares that it resumes from the Pack of ledger sequence {} \
             (pack_digest {})",
            read.project_sequence, read.pack_digest
        )),
        "{}",
        begun.stderr
    );
    let begun: Value = serde_json::from_str(begun.stdout.trim()).unwrap();
    assert_eq!(begun["task_id"], task);
    let session = begun["session_id"].clone();
    let after = agent.capsule();
    assert_eq!(after["sessions"][1]["session_id"], session);
    assert_eq!(*resumed_from(&after, 2), declaration(&read));
    assert!(resumed_from(&after, 1).is_null());
    let events = recovered(setup);
    assert_eq!(events.len(), 1);
    check_event(&events[0], task, &session, &read);
    (read, session)
}

/// A later Pack: Session 2's line shows ledger sequence N and the first 12
/// digits of the declared digest; the summary counts one declaring
/// Session; the whole digest is not written.
fn check_later_pack(setup: &Setup, first: &Named) {
    let agent = &setup.agent;
    agent.test("tests/a.sh");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F2 a still passes",
        "--next",
        "N2",
    ]);
    let capsule = agent.capsule();
    let pack = agent.bytes(&["resume", "--pack"]);
    print!("{pack}");
    let line = session_line(&capsule, 2);
    assert!(line.contains(" not ended (started "), "{line}");
    assert!(
        line.ends_with(&format!(
            "; continues from the Pack at ledger sequence {} (self-reported, digest {})",
            first.project_sequence,
            &first.pack_digest[.."sha256:".len() + 12]
        )),
        "{line}"
    );
    assert_eq!(
        session_part(&pack),
        [
            "Sessions: 2 in all, 1 ended normally, 0 ended abandoned, 1 not ended, 1 declared a resumed_from Pack"
                .to_owned(),
            session_line(&capsule, 1),
            line,
        ]
    );
    assert!(
        !pack.contains(&first.pack_digest),
        "the whole declared digest"
    );
    agent.ok(&["end", "--reason", "Session 2 is done"]);
}

/// Session 3 as Claude Code starts it: the hook injects the Pack (which
/// shows Session 2's declaration), `begin` as the client session declares
/// it. Returns the injected Pack's names.
fn third_session(setup: &Setup, task: &str, first: &Named) -> Named {
    let run = session_start(setup, CLIENT, "startup");
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let (statement, pack) = statement_and_pack(&run.stdout);
    assert!(statement.contains(&format!("Task {task} ")), "{statement}");
    // The injected Pack shows Session 2's declaration too.
    assert!(
        pack.contains(&format!(
            " ended normally (started {}); continues from the Pack at ledger sequence {} (self-reported, digest {})\n",
            setup.agent.capsule()["sessions"][1]["started_at"]
                .as_str()
                .unwrap(),
            first.project_sequence,
            &first.pack_digest[.."sha256:".len() + 12]
        )),
        "{pack}"
    );
    let injected = named(&pack);
    let begun = as_client(&setup.agent, Some(CLIENT), &["begin"]);
    assert_eq!(begun.code, Some(0), "{}", begun.stderr);
    assert!(
        begun.stderr.contains(&format!(
            "resumes from the Pack of ledger sequence {} (pack_digest {})",
            injected.project_sequence, injected.pack_digest
        )),
        "{}",
        begun.stderr
    );
    let begun: Value = serde_json::from_str(begun.stdout.trim()).unwrap();
    assert_eq!(begun["task_id"], task);
    let capsule = ok_as(&setup.agent, Some(CLIENT), &["resume"]);
    assert_eq!(capsule["sessions"][2]["session_id"], begun["session_id"]);
    assert_eq!(*resumed_from(&capsule, 3), declaration(&injected));
    let events = recovered(setup);
    assert_eq!(events.len(), 2);
    check_event(&events[1], task, &begun["session_id"], &injected);
    ok_as(
        &setup.agent,
        Some(CLIENT),
        &["end", "--reason", "Session 3 is done"],
    );
    injected
}

#[test]
fn scenario_3_a_session_records_the_pack_it_continues_and_later_packs_show_it() {
    let setup = Setup::new(&["a"]);
    step("Session 1 records a fact and ends");
    let task = first_session(&setup);

    step("resume --pack, then begin: Session 2 declares that Pack");
    let (first, _) = second_session(&setup, &task);

    step("a later Pack shows it");
    check_later_pack(&setup, &first);

    step("Session 3: the hook injects the Pack, begin declares it");
    let injected = third_session(&setup, &task, &first);
    assert_ne!(injected.pack_digest, first.pack_digest);

    step("the Pack after Session 3 counts both declaring Sessions");
    let capsule = setup.agent.capsule();
    let pack = setup.agent.bytes(&["resume", "--task", &task, "--pack"]);
    print!("{pack}");
    assert_eq!(
        line_with(&pack, "Sessions: "),
        "Sessions: 3 in all, 3 ended normally, 0 ended abandoned, 0 not ended, 2 declared a resumed_from Pack"
    );
    // Only the latest that ended is listed (P4-H1); Session 2 is counted.
    let line = session_line(&capsule, 3);
    assert!(
        line.ends_with(&format!(
            "; continues from the Pack at ledger sequence {} (self-reported, digest {})",
            injected.project_sequence,
            &injected.pack_digest[.."sha256:".len() + 12]
        )),
        "{line}"
    );
    assert_eq!(session_part(&pack)[1..], [line]);
    ledger_holds(&setup);
}
