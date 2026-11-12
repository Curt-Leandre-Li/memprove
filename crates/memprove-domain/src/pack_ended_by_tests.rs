//! U9 A7 (E1 revision 12, Owner O3; ADR 0014 "Amendment (U9, S-A)"): the
//! `Session ` line of a Session that ended abandoned and whose Capsule entry
//! has `ended_by` ends with who ended it, after everything the line had
//! (the A4 agent part and the declared Pack): `; ended by Session <j>` when the
//! ender's Session is the j-th of the Capsule's `sessions` (also with an
//! agent), `; ended by unlisted Session <id>` when it is not one of them, and `; ended by
//! agent「<name>」(<role>)` when only the agent is known. A Session
//! without `ended_by` has the line it had. Every Pack here is read back by
//! the strict parser of `pack_parse.rs`.

use serde_json::{Value, json};

use super::parse::check;
use super::{
    UNLISTED_SESSION, capsule_value, session_id, session_part, short_digest, with_sessions,
};
use crate::{PackCapsule, Sha256Digest, render_pack, select_pack};

/// Session 1 ended normally, 2–4 ended abandoned, 5 is open; 3 declared a
/// Pack. The lines are those of 2–5 (P4-H1). 2 and 3 declared an agent.
fn unended() -> Value {
    let world = with_sessions(&["ended", "crashed", "crashed", "crashed", "open"], &[3]);
    let mut value = capsule_value(&world);
    value["sessions"][1]["agent"] =
        json!({"name": "w-a", "role": "sub_agent", "parent_session_id": session_id(1)});
    value["sessions"][2]["agent"] =
        json!({"name": "lead", "role": "orchestrator", "parent_session_id": null});
    value
}

fn pack_at(value: &Value, budget: usize) -> String {
    let capsule = PackCapsule::from_json(&value.to_string()).expect("a Pack capsule");
    render_pack(&select_pack(&capsule, budget).expect("fits"))
}

/// `text` with its `Digest:` line made again for the lines before it.
fn redigest(text: &str) -> String {
    let body = &text[..=text.trim_end_matches('\n').rfind('\n').unwrap()];
    format!(
        "{body}Digest: sha256:{}\n",
        Sha256Digest::of_bytes(body.as_bytes())
    )
}

#[test]
fn u9_a7_abandoned_lines_name_who_ended_them_after_the_agent_part() {
    let plain = unended();
    let mut value = plain.clone();
    // Session 2: by Session 1 (its Session, and an agent besides).
    value["sessions"][1]["ended_by"] = json!({
        "session_id": session_id(1),
        "agent": {"name": "taker", "role": "sub_agent", "parent_session_id": null},
    });
    // Session 3: by a Session the Capsule does not list.
    value["sessions"][2]["ended_by"] = json!({"session_id": UNLISTED_SESSION});
    // Session 4: by an agent only.
    value["sessions"][3]["ended_by"] = json!({
        "agent": {"name": "taker", "role": "sub_agent", "parent_session_id": session_id(5)},
    });
    let at = |number: u64| format!("2026-10-06T00:{number:02}:00Z");
    let crashed = |number: u64| {
        format!(
            "Session {number} {} ended abandoned (started {})",
            session_id(number),
            at(number)
        )
    };
    let resumed = format!(
        "; continues from the Pack at ledger sequence 13 (self-reported, digest sha256:{})",
        short_digest("pack 3")
    );
    let before = [
        format!(
            "{}; agent「w-a」(sub_agent, dispatched by Session 1)",
            crashed(2)
        ),
        format!("{}; agent「lead」(orchestrator){resumed}", crashed(3)),
        crashed(4),
    ];
    let after = [
        format!("{}; ended by Session 1", before[0]),
        format!(
            "{}; ended by unlisted Session {UNLISTED_SESSION}",
            before[1]
        ),
        format!("{}; ended by agent「taker」(sub_agent)", before[2]),
    ];
    for budget in [4_000, 9_000, 100_000] {
        let text = pack_at(&value, budget);
        check(&value, &text, budget).unwrap_or_else(|error| panic!("{budget}: {error}\n{text}"));
        let lines = session_part(&text);
        assert_eq!(lines[1..4], after, "budget {budget}");
        // Without `ended_by`, each line is the line it was, byte for byte,
        // and (where nothing is cut) so is every other line of the Pack but
        // its digests.
        let unchanged = pack_at(&plain, budget);
        check(&plain, &unchanged, budget).unwrap();
        assert_eq!(session_part(&unchanged)[1..4], before, "budget {budget}");
        let others = |text: &str| {
            text.lines()
                .filter(|line| !line.starts_with("Session ") && !line.starts_with("Digest: "))
                .skip(1)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if budget == 100_000 {
            assert_eq!(others(&text), others(&unchanged));
        }
        // An ender part the Capsule does not have, or a different one, is
        // not the Pack's, and the strict reader refuses it.
        let open = lines[4].clone();
        for (line, forged) in [
            (&before[2], format!("{}; ended by Session 2", before[2])),
            (&after[0], format!("{}; ended by Session 3", before[0])),
            (&after[1], format!("{}; ended by Session 1", before[1])),
            (
                &after[2],
                format!("{}; ended by agent「other」(sub_agent)", before[2]),
            ),
            (&open, format!("{open}; ended by Session 1")),
        ] {
            let source = if line == &before[2] {
                &unchanged
            } else {
                &text
            };
            let changed = redigest(&source.replacen(line.as_str(), &forged, 1));
            assert_ne!(&changed, source, "budget {budget}: {forged}");
            let capsule = if line == &before[2] { &plain } else { &value };
            assert!(
                check(capsule, &changed, budget).is_err(),
                "budget {budget}: {forged}"
            );
        }
    }
}

/// A Session whose `ended_by` names the Session of Session 1 and no agent
/// writes the same `; ended by Session 1`; one that ended normally never has an
/// ender part, whatever its entry says (only `session.crashed@2` records
/// one).
#[test]
fn u9_a7_the_session_form_wins_and_only_abandoned_lines_have_it() {
    let mut value = unended();
    value["sessions"][1]["ended_by"] = json!({"session_id": session_id(1)});
    let text = pack_at(&value, 100_000);
    check(&value, &text, 100_000).unwrap();
    assert!(
        session_part(&text)[1]
            .ends_with("(sub_agent, dispatched by Session 1); ended by Session 1"),
        "{text}"
    );
    let mut value = capsule_value(&with_sessions(&["crashed", "ended"], &[]));
    value["sessions"][1]["ended_by"] = json!({"session_id": session_id(1)});
    assert!(PackCapsule::from_json(&value.to_string()).is_err());
}

/// U10 SF-1 (S-V dimension 1): Session 2 declared an agent and was ended by
/// an agent only (the shape the takeover evaluation's B leaves its seed
/// with); Session 3 declared one and was ended by Session 1; Session 4 declared one
/// and was ended by Session 5 with an agent besides (the Session wins); Session 5
/// is open and declared one.
fn agents_and_enders() -> Value {
    let mut value = unended();
    let taker = json!({"name": "taker", "role": "sub_agent", "parent_session_id": null});
    value["sessions"][1]["ended_by"] = json!({"agent": taker});
    value["sessions"][2]["ended_by"] = json!({"session_id": session_id(1)});
    value["sessions"][3]["agent"] =
        json!({"name": "w-d", "role": "sub_agent", "parent_session_id": null});
    value["sessions"][3]["ended_by"] = json!({"session_id": session_id(5), "agent": taker});
    value["sessions"][4]["agent"] =
        json!({"name": "w-e", "role": "sub_agent", "parent_session_id": session_id(1)});
    value
}

/// The start of the line of Session `number`, which ended abandoned.
fn crashed(number: u64) -> String {
    format!(
        "Session {number} {} ended abandoned (started 2026-10-06T00:{number:02}:00Z)",
        session_id(number)
    )
}

/// The start of the line of Session 5, which has not ended.
fn open_five() -> String {
    format!(
        "Session 5 {} not ended (started 2026-10-06T00:05:00Z, nothing of it in the ledger yet)",
        session_id(5)
    )
}

/// The lines of Session 2–5 of [`agents_and_enders`].
fn agents_and_enders_lines() -> [String; 4] {
    let resumed = format!(
        "; continues from the Pack at ledger sequence 13 (self-reported, digest sha256:{})",
        short_digest("pack 3")
    );
    [
        format!(
            "{}; agent「w-a」(sub_agent, dispatched by Session 1); ended by agent「taker」(sub_agent)",
            crashed(2)
        ),
        format!(
            "{}; agent「lead」(orchestrator){resumed}; ended by Session 1",
            crashed(3)
        ),
        format!(
            "{}; agent「w-d」(sub_agent); ended by Session 5",
            crashed(4)
        ),
        format!(
            "{}; agent「w-e」(sub_agent, dispatched by Session 1){}",
            open_five(),
            super::takeover_hint(5)
        ),
    ]
}

/// U10 SF-1: a line may have two quotes, its Session's agent and an ender
/// known only by its agent, and the strict reader reads it back like any
/// other (U10 T1's `pack_parse` round trip): the count of a line's quotes
/// is what `session_line` says it must be, one for the agent part and one
/// for an ender by agent. Each line read back has the number, agent, and
/// ender of the Capsule.
#[test]
fn u10_sf1_lines_with_an_agent_and_an_ender_read_back() {
    let value = agents_and_enders();
    let agent = |name: &str, role: &str, parent: Option<&str>| {
        Some((name.to_owned(), role.to_owned(), parent.map(str::to_owned)))
    };
    let read_back = [
        (
            2,
            agent("w-a", "sub_agent", Some("Session 1")),
            Some("agent taker sub_agent"),
        ),
        (3, agent("lead", "orchestrator", None), Some("Session 1")),
        (4, agent("w-d", "sub_agent", None), Some("Session 5")),
        (5, agent("w-e", "sub_agent", Some("Session 1")), None),
    ];
    for budget in [4_000, 9_000, 100_000] {
        let text = pack_at(&value, budget);
        assert_eq!(
            session_part(&text)[1..5],
            agents_and_enders_lines(),
            "budget {budget}"
        );
        // Rendered, read back strictly, and the same as the Capsule (`check`
        // compares each line's number, agent, and ender with it).
        let parsed = check(&value, &text, budget)
            .unwrap_or_else(|error| panic!("{budget}: {error}\n{text}"));
        let lines = parsed
            .session_lines
            .iter()
            .map(|line| (line.number, line.agent.clone(), line.ended_by.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(lines, read_back, "budget {budget}");
    }
}

/// U10 SF-1: what the reader still refuses once it counts two quotes: a
/// line whose quotes are not one for the agent part and one for an ender
/// by agent, a named ender on a line that did not end abandoned, and an
/// agent part after the takeover hint or after the ender. Two names
/// swapped are read, and are not the Capsule's.
#[test]
fn u10_sf1_the_reader_still_refuses_other_quotes_and_places() {
    let value = agents_and_enders();
    let text = pack_at(&value, 100_000);
    let [line2, line3, _, line5] = agents_and_enders_lines();
    let agent2 = "; agent「w-a」(sub_agent, dispatched by Session 1)";
    let ender2 = "; ended by agent「taker」(sub_agent)";
    let agent5 = "; agent「w-e」(sub_agent, dispatched by Session 1)";
    let hint5 = super::takeover_hint(5);
    let quotes = "agent text on a Session line";
    let not_abandoned = "an ender on a Session line that did not end abandoned";
    for (line, forged, reason) in [
        // A third quote, in the ender's role or in the agent's.
        (
            &line2,
            line2.replace("(sub_agent)", "(sub_agent「x」)"),
            quotes,
        ),
        (
            &line2,
            line2.replace(
                "(sub_agent, dispatched by Session 1)",
                "(sub_agent「x」, dispatched by Session 1)",
            ),
            quotes,
        ),
        // Two quotes and no ender by agent: none at all, or one by Session.
        (
            &line2,
            format!(
                "{}; agent「w-a」(sub_agent「x」, dispatched by Session 1)",
                crashed(2)
            ),
            quotes,
        ),
        (
            &line3,
            line3.replace("(orchestrator)", "(orchestrator「x」)"),
            quotes,
        ),
        // A named ender on a line that ended normally, or that has not ended.
        (
            &line2,
            line2.replace("ended abandoned", "ended normally"),
            not_abandoned,
        ),
        (&line5, format!("{line5}{ender2}"), not_abandoned),
        // The agent part after the takeover hint, or after the ender.
        (&line5, format!("{}{hint5}{agent5}", open_five()), quotes),
        (&line2, format!("{}{ender2}{agent2}", crashed(2)), quotes),
    ] {
        let changed = redigest(&text.replacen(line.as_str(), &forged, 1));
        assert_ne!(changed, text, "{forged}");
        let error = super::parse::parse(&changed).expect_err(&forged);
        assert!(error.contains(reason), "{forged}: {error}");
        assert!(check(&value, &changed, 100_000).is_err(), "{forged}");
    }
    let swapped = line2
        .replace("「w-a」", "「tmp」")
        .replace("「taker」", "「w-a」")
        .replace("「tmp」", "「taker」");
    let changed = redigest(&text.replacen(line2.as_str(), &swapped, 1));
    super::parse::parse(&changed).unwrap();
    assert!(check(&value, &changed, 100_000).is_err(), "{swapped}");
}
