//! U8 A4 (E1 revision 10, Owner O4; ADR 0014, "Amendment (U8, S-A)")
//! through the real binaries: the `Session ` lines of `resume --pack` and
//! `resume --text` name each Session's agent after the parentheses,
//! `; agent「<name>」(<role>)`, with the Session that dispatched it,
//! `, dispatched by Session <j>`; the line of a Session that has not ended and is not
//! the reader's own, which carries the takeover hint, names its agent too,
//! before the hint (U10 T1, Owner O3, which replaces S-F round 2 of
//! 2026-10-26, under which it named none); a Session without an agent keeps
//! its line, and claim lines still say `from Session k`.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use p4b_support::{Setup, step};

fn session_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("Session "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn u8_a4_the_session_lines_of_pack_and_text_name_the_agent() {
    let setup = Setup::new(&["a"]);
    let lead = &setup.agent;
    step("Session 1: the orchestrator's Session, ended");
    let begun = lead.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--new-task",
        "u8 a4",
        "--agent-name",
        "lead",
        "--agent-role",
        "orchestrator",
    ]);
    let first = begun["session_id"].as_str().unwrap().to_owned();
    let task = begun["task_id"].as_str().unwrap().to_owned();
    lead.bytes(&["end", "--reason", "dispatched"]);

    step("Session 2: a sub-agent it dispatched, still open, in its own context");
    let child = lead.elsewhere(setup.root_path.join("config-child"), "child");
    let begun = child.ok(&[
        "begin",
        "--project",
        &setup.project_id,
        "--task",
        &task,
        "--agent-name",
        "child",
        "--agent-role",
        "sub_agent",
        "--parent-session",
        &first,
    ]);
    let second = begun["session_id"].as_str().unwrap().to_owned();

    step("Session 3: the orchestrator context's next Session, declared human, open");
    let third = lead.ok(&["begin", "--agent-name", "lead", "--agent-role", "human"]);
    let third_id = third["session_id"].as_str().unwrap().to_owned();

    step("the Pack and resume --text, as the orchestrator reads them");
    let pack = lead.bytes(&["resume", "--pack"]);
    let lines = session_lines(&pack);
    // U10 T7: the hint names that Session's ID and number.
    let hint = p4b_support::takeover_hint(&lines[1]);
    assert_eq!(lines.len(), 3, "{pack}");
    assert!(
        lines[0].starts_with(&format!("Session 1 {first} ended normally (started "))
            && lines[0].ends_with("); agent「lead」(orchestrator)"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].starts_with(&format!("Session 2 {second} not ended (started "))
            && lines[1].ends_with(&format!(
                "); agent「child」(sub_agent, dispatched by Session 1){hint}"
            )),
        "{}",
        lines[1]
    );
    // The reader's own open Session has no takeover hint.
    assert!(
        lines[2].starts_with(&format!("Session 3 {third_id} not ended (started "))
            && lines[2].ends_with("); agent「lead」(human)"),
        "{}",
        lines[2]
    );
    let text = lead.bytes(&["resume", "--text"]);
    let lines = session_lines(&text);
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].ends_with("); agent「child」(sub_agent, dispatched by Session 1)"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("(the Session this directory's context file records)")
            && lines[1].ends_with("); agent「lead」(human)"),
        "{}",
        lines[1]
    );
}
