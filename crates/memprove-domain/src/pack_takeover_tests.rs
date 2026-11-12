//! U7 P1 (E1 revision 9; D2 F16, F15, Owner O3; ADR 0014 "Amendment
//! (U7)"): the `Session ` line of a Session that has not ended and is not the
//! reader's own ends with how to end it before taking over. The reader's own
//! Session (the one the context it uses names,
//! [`PackCapsule::with_own_session`]) has no such end; a Session that ended
//! never has it. Every Pack here is read back by the strict parser of
//! `pack_parse.rs`, which knows the hint by its own copy of the words.

use super::parse::{check, check_owned, parse};
use super::{World, body, capsule_value, session_id, session_part, with_sessions};
use crate::{PackCapsule, PackError, Sha256Digest, render_pack, select_pack};

const BUDGET: usize = crate::PACK_DEFAULT_BUDGET;

/// The words of O3 before U10 T7, which no Pack has any more.
const OLD_HINT: &str = "; end it before taking over: memprove end --abandoned --reason <why> (MCP: end {session, abandoned:true})";

/// U10 T7 (Owner O4): the hint of the line of Session `number`.
fn hint(number: u64) -> String {
    super::takeover_hint(number)
}

/// Session 1 ended normally, 2 is open and declared a Pack, 3 ended abandoned,
/// 4 is open: the lines are those of 2, 3, and 4 (P4-H1).
fn four() -> World {
    with_sessions(&["ended", "open", "crashed", "open"], &[2])
}

/// The Pack of `world` at `budget` for the reader whose own Session is
/// `own`.
fn pack_of(world: &World, own: Option<&str>, budget: usize) -> Result<String, PackError> {
    let capsule = PackCapsule::from_json(&body(world))
        .unwrap()
        .with_own_session(own);
    select_pack(&capsule, budget).map(|selection| render_pack(&selection))
}

/// `text` with its `Digest:` line made again for the lines before it.
fn redigest(text: &str) -> String {
    let before = &text[..=text.trim_end_matches('\n').rfind('\n').unwrap()];
    format!(
        "{before}Digest: sha256:{}\n",
        Sha256Digest::of_bytes(before.as_bytes())
    )
}

#[test]
fn the_open_session_of_someone_else_ends_with_the_takeover_hint() {
    let world = four();
    let at = |number: u64| format!("2026-10-06T00:{number:02}:00Z");
    let two = format!(
        "Session 2 {} not ended (started {}, last wrote to the ledger at 2026-10-06T00:02:30Z); \
         continues from the Pack at ledger sequence 12 (self-reported, digest sha256:{})",
        session_id(2),
        at(2),
        super::short_digest("pack 2")
    );
    let three = format!(
        "Session 3 {} ended abandoned (started {})",
        session_id(3),
        at(3)
    );
    let four_line = format!(
        "Session 4 {} not ended (started {}, last wrote to the ledger at 2026-10-06T00:04:30Z)",
        session_id(4),
        at(4)
    );
    let summary =
        "Sessions: 4 in all, 1 ended normally, 1 ended abandoned, 2 not ended, 1 declared a \
                   resumed_from Pack"
            .to_owned();

    // Nobody's own (no context, or the hook of a new client session): both
    // open Sessions carry the hint, after the declared Pack.
    let none = pack_of(&world, None, BUDGET).unwrap();
    assert_eq!(
        session_part(&none),
        [
            summary.clone(),
            format!("{two}{}", hint(2)),
            three.clone(),
            format!("{four_line}{}", hint(4)),
        ]
    );
    // The reader's own open Session has no hint; the other one keeps it.
    let own = session_id(4);
    let mine = pack_of(&world, Some(&own), BUDGET).unwrap();
    assert_eq!(
        session_part(&mine),
        [
            summary.clone(),
            format!("{two}{}", hint(2)),
            three.clone(),
            four_line.clone()
        ]
    );
    // A Session that ended never has it, even when it is the reader's; an
    // ID the Capsule does not list is nobody's.
    for nobody in [session_id(3), session_id(9)] {
        assert_eq!(pack_of(&world, Some(&nobody), BUDGET).unwrap(), none);
    }
    // Nothing else moves: without the hint and the digest line, the Packs
    // are the same lines.
    let lines = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with("Digest: "))
            .map(|line| line.replace(&hint(2), "").replace(&hint(4), ""))
            .collect::<Vec<_>>()
    };
    assert_eq!(lines(&none), lines(&mine));
    assert_eq!(none.matches("end it before taking over").count(), 2);
    assert_eq!(mine.matches("end it before taking over").count(), 1);
    assert!(!none.contains(OLD_HINT), "{none}");
}

#[test]
fn the_strict_reader_reads_the_hint_back_and_holds_it_to_the_own_session() {
    let world = four();
    let value = capsule_value(&world);
    let owns = [None, Some(session_id(2)), Some(session_id(4))];
    for own in &owns {
        let own = own.as_deref();
        let Err(PackError::OverBudget { needed, .. }) = pack_of(&world, own, 1_000) else {
            panic!("four Sessions and the fixture's claims fit 1000 units");
        };
        for budget in [needed, needed + 200, 3_000, BUDGET, 100_000] {
            let text = pack_of(&world, own, budget).unwrap();
            check_owned(&value, &text, budget, own)
                .unwrap_or_else(|error| panic!("{own:?} {budget}: {error}\n{text}"));
            // Read back: the hint is on the open lines that are not `own`.
            let parsed = parse(&text).unwrap();
            let flags = parsed
                .session_lines
                .iter()
                .map(|line| (line.number, line.takeover))
                .collect::<Vec<_>>();
            let expected = [2_u64, 3, 4]
                .map(|number| {
                    (
                        number,
                        number != 3 && own != Some(session_id(number).as_str()),
                    )
                })
                .to_vec();
            assert_eq!(flags, expected, "{own:?} {budget}");
            // Held to the reader: another reader's Pack is refused.
            for other in &owns {
                if other.as_deref() != own {
                    assert!(
                        check_owned(&value, &text, budget, other.as_deref()).is_err(),
                        "{own:?} read as {other:?}"
                    );
                }
            }
        }
    }
    // `check` is the reader with no own Session.
    let text = pack_of(&world, None, BUDGET).unwrap();
    check(&value, &text, BUDGET).unwrap();
    // The hint on a Session that ended is refused, and so is a hint in
    // other words.
    let crashed = " (started 2026-10-06T00:03:00Z)";
    let moved = redigest(&text.replacen(crashed, &format!("{crashed}{}", hint(3)), 1));
    assert!(parse(&moved).is_err());
    let reworded = redigest(&text.replacen(&hint(2), "; end it before you take over", 1));
    assert!(parse(&reworded).is_err());
    // U10 T7: the words before T7, a hint naming another line's Session or
    // number, and a program that is not one plain word are refused too.
    for forged in [
        OLD_HINT.to_owned(),
        hint(4),
        hint(2).replace("session:2", "session:4"),
        hint(2).replace("memprove session end", "memprove --x session end"),
        hint(2).replace("memprove session end", "/tmp/a b/memprove session end"),
    ] {
        let forged = redigest(&text.replacen(&hint(2), &forged, 1));
        assert!(check(&value, &forged, BUDGET).is_err(), "{forged}");
    }
}

/// O3's other way a Session is the reader's own: it declared it continues
/// from the Pack the reader's last-Pack record names (`resumed_from`, by
/// ledger sequence and `pack_digest`). Another sequence or digest is not
/// that Pack.
#[test]
fn a_session_that_declared_the_readers_recorded_pack_is_its_own() {
    let world = four();
    let value = capsule_value(&world);
    let digest = super::digest_of("pack 2");
    let read = |resumed: Option<(u64, &str)>| {
        let capsule = PackCapsule::from_json(&body(&world))
            .unwrap()
            .with_own_session(None)
            .with_own_resumed(resumed);
        render_pack(&select_pack(&capsule, BUDGET).unwrap())
    };
    let declared = read(Some((12, &digest)));
    check_owned(&value, &declared, BUDGET, Some(&session_id(2))).unwrap();
    assert_eq!(
        declared,
        pack_of(&world, Some(&session_id(2)), BUDGET).unwrap()
    );
    let nobody = pack_of(&world, None, BUDGET).unwrap();
    let another = super::digest_of("pack 3");
    for other in [
        Some((13, digest.as_str())),
        Some((12, another.as_str())),
        None,
    ] {
        assert_eq!(read(other), nobody, "{other:?}");
    }
    // Both ways together: the context's Session and the declaring one.
    let both = PackCapsule::from_json(&body(&world))
        .unwrap()
        .with_own_session(Some(&session_id(4)))
        .with_own_resumed(Some((12, &digest)));
    let text = render_pack(&select_pack(&both, BUDGET).unwrap());
    assert!(!text.contains("end it before taking over"), "{text}");
}

/// U10 T1 (Owner O3, E1 revision 13; VN23, D3 F23, F16): the line of a
/// Session that has not ended and is not the reader's own names its agent
/// too, where U8 A4 puts it (after the parentheses, before the declared
/// Pack), and the hint stays at the end; this replaces S-F round 2 (Owner,
/// 2026-10-26), under which that line named no agent. A line of a Session
/// that declared no agent, and a Pack with no open Session, are the bytes
/// they were. The `Can:` line of `begin_session` no longer only counts the
/// open Sessions: it says to end them by their lines first, or to make sure
/// they still work (D3 F16: the same Pack said both "end it first" and "you
/// can begin"). The strict reader reads every such Pack back.
#[test]
fn u10_t1_the_takeover_line_names_the_agent_and_begin_points_at_the_lines() {
    use serde_json::{Value, json};

    let world = four();
    let unnamed = capsule_value(&world);
    let mut value = unnamed.clone();
    value["sessions"][1]["agent"] =
        json!({"name": "seed-b", "role": "sub_agent", "parent_session_id": session_id(1)});
    value["sessions"][3]["agent"] =
        json!({"name": "takeover-eval-other", "role": "sub_agent", "parent_session_id": null});
    let render = |value: &Value, own: Option<&str>, budget: usize| {
        let capsule = PackCapsule::from_json(&value.to_string())
            .unwrap()
            .with_own_session(own);
        render_pack(&select_pack(&capsule, budget).unwrap())
    };
    let at = |number: u64| format!("2026-10-06T00:{number:02}:00Z");
    let two = format!(
        "Session 2 {} not ended (started {}, last wrote to the ledger at 2026-10-06T00:02:30Z)\
         ; agent「seed-b」(sub_agent, dispatched by Session 1)\
         ; continues from the Pack at ledger sequence 12 (self-reported, digest sha256:{})",
        session_id(2),
        at(2),
        super::short_digest("pack 2")
    );
    let four_line = format!(
        "Session 4 {} not ended (started {}, last wrote to the ledger at 2026-10-06T00:04:30Z)\
         ; agent「takeover-eval-other」(sub_agent)",
        session_id(4),
        at(4)
    );
    let begin = "Can: begin a new Session (createSession; first end them as the lines of Sessions 2, 4 \
                 say, or confirm they are still working; needs mcp-write)";
    for budget in [3_000, BUDGET, 100_000] {
        let none = render(&value, None, budget);
        check(&value, &none, budget).unwrap_or_else(|error| panic!("{budget}: {error}\n{none}"));
        let lines = session_part(&none);
        assert_eq!(lines[1], format!("{two}{}", hint(2)), "{budget}");
        assert_eq!(lines[3], format!("{four_line}{}", hint(4)), "{budget}");
        assert!(none.lines().any(|line| line == begin), "{none}");
        let parsed = parse(&none).unwrap();
        let named = parsed
            .session_lines
            .iter()
            .map(|line| (line.number, line.takeover, line.agent.is_some()))
            .collect::<Vec<_>>();
        assert_eq!(named, [(2, true, true), (3, false, false), (4, true, true)]);
        // The reader's own open Session keeps its agent and has no hint.
        let own = session_id(4);
        let mine = render(&value, Some(&own), budget);
        check_owned(&value, &mine, budget, Some(&own))
            .unwrap_or_else(|error| panic!("{budget}: {error}\n{mine}"));
        assert_eq!(session_part(&mine)[3], four_line, "{budget}");
        // The same Pack with the agent left out of a hint line is refused.
        let forged = none.replacen("; agent「takeover-eval-other」(sub_agent)", "", 1);
        assert!(check(&value, &redigest(&forged), budget).is_err());
        // Without agents, the lines are those of the same Sessions without
        // the fragment.
        let plain = render(&unnamed, None, budget);
        check(&unnamed, &plain, budget).unwrap();
        let stripped = |line: &str| match line.split_once("; agent「") {
            Some((head, tail)) => format!("{head}{}", tail.split_once(')').unwrap().1),
            None => line.to_owned(),
        };
        assert_eq!(
            session_part(&plain),
            session_part(&none)
                .iter()
                .map(|line| stripped(line))
                .collect::<Vec<_>>()
        );
    }
}

/// U10 T1 (Owner O3; D3 F16): the `Can: begin a new Session` line for one open
/// Session; a refused begin and a Task without open Sessions keep their
/// words.
#[test]
fn u10_t1_begin_points_at_the_line_of_the_one_open_session() {
    // One open Session: "that line", "it"; a refused begin keeps its words.
    let one = with_sessions(&["ended", "open"], &[]);
    let text = pack_of(&one, None, BUDGET).unwrap();
    check(&capsule_value(&one), &text, BUDGET).unwrap();
    assert!(
        text.lines().any(|line| line
            == "Can: begin a new Session (createSession; first end it as the line of Session 2 says, or \
                confirm it is still working; needs mcp-write)"),
        "{text}"
    );
    let mut refused = one.clone();
    let open = vec![session_id(2)];
    refused.actions = super::actions(&["evidence_stale"], 0, &["invalid_transition"], &open, &[]);
    let text = pack_of(&refused, None, BUDGET).unwrap();
    check(&capsule_value(&refused), &text, BUDGET).unwrap();
    assert!(
        text.lines().any(|line| line
            == "Cannot: begin a new Session (createSession; reasons: invalid_transition; open Sessions 2; \
                needs mcp-write)"),
        "{text}"
    );
    // No open Session: the line it always was.
    let closed = with_sessions(&["ended", "crashed"], &[]);
    let text = pack_of(&closed, None, BUDGET).unwrap();
    assert!(
        text.lines()
            .any(|line| line == "Can: begin a new Session (createSession; needs mcp-write)"),
        "{text}"
    );
}

/// U10 T7 (Owner O4; D3 F24, F30; ADR 0014 E1 revision 13): the takeover
/// hint is a command to copy: `memprove session end <the Session's ID>
/// --abandoned --reason <why>`, with the MCP form `end {session:<k>,
/// abandoned:true}` naming the line's own number (the `end` tool's
/// `session` is the Pack's Session number). Each open line has its own.
#[test]
fn u10_t7_the_hint_is_the_command_that_ends_that_session() {
    let world = four();
    let text = pack_of(&world, None, BUDGET).unwrap();
    for number in [2_u64, 4] {
        let id = session_id(number);
        let hint = format!(
            "; end it before taking over: memprove session end {id} --abandoned --reason <why> \
             (MCP: end {{session:{number}, abandoned:true}})"
        );
        let line = session_part(&text)
            .into_iter()
            .find(|line| line.starts_with(&format!("Session {number} ")))
            .unwrap();
        assert!(line.ends_with(&hint), "{line}");
        assert_eq!(
            line.matches("end it before taking over").count(),
            1,
            "{line}"
        );
    }
    assert!(!text.contains("memprove end --abandoned"), "{text}");
    check(&capsule_value(&world), &text, BUDGET).unwrap();
}

/// U10 T7 (Owner O4; D3 F30): when `memprove` does not find the reader's
/// executable, the reader gives its absolute path, and the hint names it;
/// the strict reader reads it as the same hint. A program that is not one
/// plain shell word is not taken, and one Pack never names two programs.
#[test]
fn u10_t7_the_hint_names_the_absolute_path_the_reader_gives() {
    let world = four();
    let value = capsule_value(&world);
    let read = |program: &str| {
        let capsule = PackCapsule::from_json(&body(&world))
            .unwrap()
            .with_takeover_program(program);
        render_pack(&select_pack(&capsule, BUDGET).unwrap())
    };
    let plain = pack_of(&world, None, BUDGET).unwrap();
    assert_eq!(read("memprove"), plain);
    let path = "/opt/memprove-1.2/bin/memprove";
    let absolute = read(path);
    check(&value, &absolute, BUDGET).unwrap();
    for number in [2, 4] {
        let expected =
            hint(number).replace(": memprove session end", &format!(": {path} session end"));
        assert!(absolute.contains(&expected), "{absolute}");
    }
    let parsed = parse(&absolute).unwrap();
    let programs = parsed
        .session_lines
        .iter()
        .map(|line| line.program.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(programs, [Some(path), None, Some(path)]);
    // Only the program differs.
    assert_eq!(
        absolute
            .lines()
            .filter(|line| !line.starts_with("Digest: "))
            .map(|line| line.replace(path, "memprove"))
            .collect::<Vec<_>>(),
        plain
            .lines()
            .filter(|line| !line.starts_with("Digest: "))
            .collect::<Vec<_>>()
    );
    // Not one plain word, relative, or empty: the default stays.
    for refused in [
        "",
        "bin/memprove",
        "/a b/memprove",
        "/a/「x」",
        "/a/c;rm",
        "/",
    ] {
        assert_eq!(read(refused), plain, "{refused:?}");
    }
    // Two programs in one Pack are not one reader's.
    let mixed = redigest(&absolute.replacen(&format!(": {path} "), ": memprove ", 1));
    assert!(check(&value, &mixed, BUDGET).is_err(), "{mixed}");
}
