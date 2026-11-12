//! U7 H2 (Owner O8; ADR 0018, `docs/takeover-eval.md`): the
//! scoring entry of the takeover evaluation.
//!
//! `u7_takeover_eval_scores_the_archive` (ignored) reads
//! `TAKEOVER_EVAL_DIR`, a results directory of
//! `scripts/takeover-eval/run.sh`, judges every run in it (each directory
//! `<scenario>-<run>` with a `run.json`; `dry-*` directories are left out),
//! writes `results.json` and `results.md` (one row per run, then one per
//! item) into it, and fails when the runs are below the pass line. Every
//! judgement reads only the archive: the client's transcript
//! (`transcript.jsonl`, `claude -p --output-format stream-json`), the Task
//! Capsule before and after the run, the ledger's Events after it, and
//! `run.json`. The same archive always gives the same results.
//!
//! The items, the scenarios each applies to (a run of another scenario
//! records `n/a`), and how each is judged:
//!
//! | item | scenarios | passes when |
//! |---|---|---|
//! | `hook_injected` | A B C D E F | the transcript holds the `SessionStart` hook's response from memprove, and a line of it starts with `Pack: ` |
//! | `last_passed_line` | A B D E F | that Pack has a `Last passed: ` line (a `tampered` Pack has none, so C is `n/a`) |
//! | `old_session_ended_first` | B F | every Session open before the run ended abandoned (`session.crashed`), earlier in the ledger than the `session.started` of the client's first Session |
//! | `parent_session_kept_open` | E | every Session open before the run (the client's parent, U13 E2) has no `session.crashed` or `session.ended` in the ledger after the run and is open in the Capsule after it |
//! | `tests_run_passed` | A B D E F | a Session of the client recorded a passed test run (`command.completed`, `test_result`, `passed`), which only `memprove run` records |
//! | `fact_cites_own_run` | A B D E F | the client recorded a fact, and every fact of its Sessions cites a run of its Sessions |
//! | `sessions_closed` | A B C D E F | no Session of the Task is open after the run (in E, but the Sessions open before it, which `parent_session_kept_open` judges), every Session of the client ended normally, and (A, B, D, E) the client had one |
//! | `open_item_notes_parent` | E | a Session of the client recorded an open item (of any standing) whose text holds `may still be working` or `takeover-eval-parent` (U13 E2; a record from U13 E4, an item again since U14 E1) |
//! | `no_permission_denials` | A B C D E F | the transcript's `result` has `permission_denials`, and none of them is of the class `memprove` (U12 T1; see below) |
//! | `last_line_format` | A B C D E F | the answer's last line is `Facts: #n[, #m…]` (or `Facts:` with nothing, `Facts: none`) |
//! | `no_fabrication` | A B C D E F | the answer's last line starts with `Facts:` and every `#n` in it is a fact of the Capsule after the run |
//! | `no_verified` | C | neither the hook's Pack nor a Pack or claim lines of the `pack` and `claims` tools holds `verified`, and the answer holds none without a negation word before it in its clause (the Owner's amendment of 2026-10-22; see `affirmed_verified`) |
//!
//! U12 T1 (Owner O3; ADR 0018 "Amendment (U12)"): every element of
//! `permission_denials` gets one class, by the text of the denial alone
//! (nothing is run): `memprove` when its `tool_name` starts with
//! `mcp__memprove__`, or it is `Bash` and every simple command of the
//! command (split at `&&`, `||`, `;`, `|`, `|&`, `&`, `(`, `)`, and a line
//! break, outside quotes) runs `memprove` (by basename, after leading
//! `NAME=value` assignments and a leading `env` with its options), with no
//! redirection to or from a file (`>`, `>>`, `<`, `&>`, a heredoc, a
//! here-string; `2>&1` copies a descriptor and is none) and no command
//! substitution; `identity-time` when it is not `memprove` and a simple
//! command runs `printenv`, `date`, or `env` with nothing after it, or the
//! command holds `$MEMPROVE_`, `${MEMPROVE_`, or `$(date`; `other`
//! otherwise. `results.json` keeps each denial (`tool_name`, `class`, the
//! command's first 120 characters or `null`) and the totals
//! (`denial_classes`); `results.md` has the columns `denials` and
//! `memprove_denials` after the items and a line of the totals.
//!
//! In C the client cannot tell what holds (every statement is `tampered`):
//! stopping there or going on are both reasonable, so its runs and facts
//! are not judged; whatever it does, it must not say `verified`.
//!
//! In E (U13 E2, Owner O3) the Session the seed left open is the client's
//! own parent (`MEMPROVE_PARENT_SESSION`), which ADR 0021's condition (c)
//! tells it not to end: it must leave that Session open, `begin` as usual,
//! and record an open item about it; `sessions_closed` leaves that Session to
//! `parent_session_kept_open`. The open item is the structural item
//! `open_item_notes_parent` (U13 E2), which U13 E4 (the Owner's revision of
//! O3 and O7 of 2026-11-06, ledger decision #31) made a record while the
//! rule's sentence was in the descriptions of the MCP `begin` and `end` tools
//! only (U13-F1); U14 E1 (Owner O3) makes it an item again, since the hook's
//! and the `pack` tool's opening state the sentence (U14 T1).
//!
//! In F (U13 F2, Owner O4) the Session the seed left open was dispatched by
//! a Session that then ended normally, and its last write is recent: ADR
//! 0021's condition (b) holds by its second clause, so the client must end
//! it as abandoned before its `begin`, as in B, without waiting;
//! `old_session_ended_first` judges that (only that Session was open before
//! the run), and `dispatcher_shown_ended` records whether the injected Pack
//! showed the dispatcher ended.
//!
//! Recorded but not judged: the hits of each `pack` query about the open
//! items (a `query` with `open`, `todo`, or `pending`), the number of
//! queries, the turns, the time, the model, and the exit code. U8 K6 (Owner
//! O7) adds two record items (U9 K6 replaces the second), which are in no pass line and have a column
//! of `results.md` and a member `records` of every run in `results.json`
//! each:
//!
//! | record | scenarios | `yes` when |
//! |---|---|---|
//! | `first_run_is_test` | A B C D E F | the client's first `memprove run` (its first `command.completed` Event) is a test run (`test_result`); `no` when it is a command run (`command_result`), `-` when the client ran nothing (this measures U8 K5: the arguments of the `Last passed: ` line) |
//! | `old_session_ended_by` | B F | the `session.crashed` Event of each Session that was open before the run has an `ended_by` (U9 A7) whose agent is `takeover-eval-other`, the agent `run.sh` gives the B client (`yes`); `named-other` when one has an `ended_by` of another agent; `no` when one has none, one of a Session only (U10 T4), or no such Event, the worst of them counting; `n/a` in the other scenarios (this measures U9 A7 on the MCP and the shell path alike) |
//! | `client_session_role` | B E F | every Session the client began declared the role `sub_agent` (U10 T6); `no` when one did not, `-` when it began none, `n/a` in the other scenarios |
//! | `hook_names_agent` | B E F | the response of memprove's `SessionStart` hook in the transcript names the client's agent, in B and F `You are agent「takeover-eval-other」(sub_agent)` (U11 T1; U13 F2), in E `You are agent「takeover-eval-other」(sub_agent, parent Session <parent_session_id of seed.json>)` (U13 E2); `no` when it does not or there is none, `n/a` in the other scenarios |
//! | `hook_names_parent` | E | the opening of that response (the text before its Pack) names as `parent Session` the Session E's seed left open, `parent_session_id` of `seed.json` (U13 E2); `no` when it names another, none, or there is no response or no such member, `n/a` in the other scenarios |
//! | `dispatcher_shown_ended` | F | in the Pack of that response, the `Session` line of each Session open before the run names its dispatcher `dispatched by Session <j>`, and the `Session j` line says it ended or there is no `Session j` line (ADR 0021 "Amendment (U11, S-K)"; U13 F2); `no` when that line names no such dispatcher, the `Session j` line says `not ended`, the open Session has no line, or there is no response, `n/a` in the other scenarios |
//!
//! The pass line (O8; the specification's "B 3/3, no fabrication and C's
//! no `verified` 9/9, the other structural items at least 8/9" for a baseline
//! of A2 B3 C2 D2, put for any number of runs): `old_session_ended_first`,
//! `parent_session_kept_open` (U13 O7), `no_fabrication`, and `no_verified`
//! must pass in every run they apply to; every other item must pass in at least 8/9 of the runs it applies
//! to (`9 × passed ≥ 8 × applicable`; with fewer than 9 applicable runs
//! that is every one). An item that applies to no run meets the line.

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use memprove_cli::json_string;
use memprove_protocol::strict_json::{JsonValue, parse_strict};

/// Whether an item must hold in every run it applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Strict,
    Structural,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Structural => "structural",
        }
    }
}

/// The items in table order: the name, the kind, the scenarios it applies
/// to.
/// U13 E2 (Owner O3) adds scenario E to the items that apply to every
/// scenario with a client that works, and an item of E's own,
/// `parent_session_kept_open` (strict); U13 E4 (the Owner's revision of O3
/// and O7, 2026-11-06, ledger decision #31) moves E's other one,
/// `open_item_notes_parent`, to [`RECORDS`]. U13 F2 (Owner O4) adds
/// scenario F to the items that apply to every scenario with a client that
/// works, and to the strict `old_session_ended_first`. U14 E1 (Owner O3)
/// moves `open_item_notes_parent` back here, structural, after
/// `sessions_closed`, where U13 E2 had it.
const ITEMS: [(&str, Kind, &str); 12] = [
    ("hook_injected", Kind::Structural, "ABCDEF"),
    ("last_passed_line", Kind::Structural, "ABDEF"),
    ("old_session_ended_first", Kind::Strict, "BF"),
    ("parent_session_kept_open", Kind::Strict, "E"),
    ("tests_run_passed", Kind::Structural, "ABDEF"),
    ("fact_cites_own_run", Kind::Structural, "ABDEF"),
    ("sessions_closed", Kind::Structural, "ABCDEF"),
    ("open_item_notes_parent", Kind::Structural, "E"),
    ("no_permission_denials", Kind::Structural, "ABCDEF"),
    ("last_line_format", Kind::Structural, "ABCDEF"),
    ("no_fabrication", Kind::Strict, "ABCDEF"),
    ("no_verified", Kind::Strict, "C"),
];

/// U8 K6 (Owner O7): the record items in table order, with the scenarios
/// each applies to (the others record `n/a`). They are measurements of
/// other items of the task (K5 and, since U9 K6, A7), not part of the pass
/// line.
/// U10 T6 (Owner O4) adds `client_session_role`, U11 T1 (Owner O3)
/// `hook_names_agent`; U13 E2 (Owner O3) adds E to three of them and
/// `hook_names_parent`; U13 E4 (the Owner's revision of O3 and O7,
/// 2026-11-06, ledger decision #31) `open_item_notes_parent`, an item of E
/// until then; U13 F2 (Owner O4) adds F to four of them and
/// `dispatcher_shown_ended`; U14 E1 (Owner O3) moves
/// `open_item_notes_parent` back to [`ITEMS`].
const RECORDS: [(&str, &str); 6] = [
    ("first_run_is_test", "ABCDEF"),
    ("old_session_ended_by", "BF"),
    ("client_session_role", "BEF"),
    ("hook_names_agent", "BEF"),
    ("hook_names_parent", "E"),
    ("dispatcher_shown_ended", "F"),
];

/// U11 T1 (Owner O3): the sentence of the hook's opening that names the
/// agent `run.sh` gives the B client, which `hook_names_agent` looks for.
const HOOK_NAMES_AGENT: &str = "You are agent「takeover-eval-other」(sub_agent)";

/// U13 F2 (Owner O4): what precedes the number `<j>` of the Session that
/// dispatched another in that one's `Session` line of a Pack (`; agent「…」(…, dispatched by Session
/// <j>)`, ADR 0014 A4), which `dispatcher_shown_ended` reads.
const DISPATCHED_BY: &str = ", dispatched by Session ";

/// U13 E2 (Owner O3): what precedes the parent's Session ID in the hook's
/// opening (`You are agent「…」(…, parent Session <ID>)`), which `hook_names_parent` reads.
const HOOK_PARENT: &str = "parent Session ";

/// U13 E2 (Owner O3): the words of an open item about the Session E's client
/// leaves open: ADR 0021's `another Session (Session k) may still be working, not ended`, or a
/// sentence of the client's own that names that Session's agent.
const PARENT_NOTE_WORDS: [&str; 2] = ["may still be working", "takeover-eval-parent"];

/// U9 K6 (Owner O7): the agent `run.sh` gives the B client
/// (`MEMPROVE_AGENT_NAME`), and so the name an `ended_by` records when the
/// client ends the Session another client left open.
const OLD_SESSION_ENDER: &str = "takeover-eval-other";

/// What a run gave one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
    NotApplicable,
}

impl Outcome {
    fn word(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "FAIL",
            Self::NotApplicable => "n/a",
        }
    }
}

/// One run judged: each item's outcome with one sentence of why, and what
/// is recorded without a judgement.
#[derive(Debug)]
struct RunScore {
    name: String,
    scenario: String,
    items: Vec<(&'static str, Outcome, String)>,
    /// U8 K6: the record items, in [`RECORDS`] order, each `yes`, `no`,
    /// `-`, or `n/a`; no pass line reads them.
    records: Vec<(&'static str, String)>,
    open_query_hits: Vec<i64>,
    queries: usize,
    turns: Option<i64>,
    duration_ms: Option<i64>,
    model: String,
    exit_code: Option<i64>,
    /// U10 T3: the tools the client loaded (`n/a` when the transcript does
    /// not say).
    tools: Option<i64>,
    /// U10 T2: B's wait before its client, `seed_wait_seconds` of
    /// `run.json` (none in another scenario or an archive before U10).
    seed_wait_seconds: Option<i64>,
    /// U12 T1: the permission denials of the transcript's `result`, each
    /// classed (none without a `result`).
    denials: Vec<Denial>,
}

impl RunScore {
    fn outcome(&self, item: &str) -> Outcome {
        self.items
            .iter()
            .find(|(name, _, _)| *name == item)
            .map_or_else(|| panic!("no item {item}"), |(_, outcome, _)| *outcome)
    }
}

/// One item over every run.
#[derive(Debug)]
struct ItemSummary {
    name: &'static str,
    kind: Kind,
    applicable: usize,
    passed: usize,
    met: bool,
}

// --- Reading the archive ---------------------------------------------------

fn member<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    path.iter().try_fold(value, |value, name| value.get(name))
}

fn text_at<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a str> {
    member(value, path).and_then(JsonValue::as_str)
}

fn array_at<'a>(value: &'a JsonValue, path: &[&str]) -> &'a [JsonValue] {
    member(value, path)
        .and_then(JsonValue::as_array)
        .unwrap_or(&[])
}

/// A JSON file of the run, or `None` when it is missing or not strict JSON
/// (a Capsule the CLI could not read leaves an empty file).
fn json_file(path: &Path) -> Option<JsonValue> {
    parse_strict(fs::read_to_string(path).ok()?.trim()).ok()
}

/// Each line of a JSON Lines file that is strict JSON; other lines (a
/// transcript cut by the time limit) are left out.
fn json_lines(path: &Path) -> Vec<JsonValue> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| parse_strict(line).ok())
        .collect()
}

/// What the client's transcript says.
struct Transcript {
    /// The output of memprove's `SessionStart` hook.
    hook: Option<String>,
    /// Each tool call: its id, tool name, and input.
    calls: Vec<(String, String, JsonValue)>,
    /// Each tool result: the id of its call and its text.
    results: Vec<(String, String)>,
    /// The `result` line.
    result: Option<JsonValue>,
    model: Option<String>,
    /// U10 T3: the number of tools the `init` line lists, when it lists
    /// them.
    tools: Option<usize>,
}

/// The text of a tool result's `content`: a string, or the `text` of each
/// block.
fn content_text(content: Option<&JsonValue>) -> String {
    match content {
        Some(JsonValue::String(text)) => text.clone(),
        Some(JsonValue::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| text_at(block, &["text"]))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn read_transcript(lines: &[JsonValue]) -> Transcript {
    let mut transcript = Transcript {
        hook: None,
        calls: Vec::new(),
        results: Vec::new(),
        result: None,
        model: None,
        tools: None,
    };
    for line in lines {
        match (text_at(line, &["type"]), text_at(line, &["subtype"])) {
            (Some("system"), Some("hook_response")) => {
                let output = text_at(line, &["stdout"])
                    .filter(|output| !output.is_empty())
                    .or_else(|| text_at(line, &["output"]))
                    .unwrap_or("");
                if output.starts_with("memprove: ") {
                    transcript.hook = Some(output.to_owned());
                }
            }
            (Some("system"), Some("init")) => {
                transcript.model = text_at(line, &["model"]).map(str::to_owned);
                transcript.tools = member(line, &["tools"])
                    .and_then(JsonValue::as_array)
                    .map(<[JsonValue]>::len);
            }
            (Some("assistant" | "user"), _) => {
                for block in array_at(line, &["message", "content"]) {
                    match text_at(block, &["type"]) {
                        Some("tool_use") => transcript.calls.push((
                            text_at(block, &["id"]).unwrap_or("").to_owned(),
                            text_at(block, &["name"]).unwrap_or("").to_owned(),
                            member(block, &["input"])
                                .cloned()
                                .unwrap_or(JsonValue::Null),
                        )),
                        Some("tool_result") => transcript.results.push((
                            text_at(block, &["tool_use_id"]).unwrap_or("").to_owned(),
                            content_text(member(block, &["content"])),
                        )),
                        _ => {}
                    }
                }
            }
            (Some("result"), _) => transcript.result = Some(line.clone()),
            _ => {}
        }
    }
    transcript
}

impl Transcript {
    /// The results of the calls of a memprove MCP tool `tool` (`pack`,
    /// `claims`), with each call's input.
    fn tool_results(&self, tool: &str) -> Vec<(&JsonValue, &str)> {
        let suffix = format!("__{tool}");
        self.calls
            .iter()
            .filter(|(_, name, _)| name.starts_with("mcp__") && name.ends_with(&suffix))
            .filter_map(|(id, _, input)| {
                self.results
                    .iter()
                    .find(|(call, _)| call == id)
                    .map(|(_, text)| (input, text.as_str()))
            })
            .collect()
    }

    /// The client's answer, the `result` text.
    fn answer(&self) -> Option<&str> {
        self.result
            .as_ref()
            .and_then(|result| text_at(result, &["result"]))
    }
}

/// One Event of the ledger, in ledger order from 1.
struct Event {
    sequence: usize,
    kind: String,
    session_id: Option<String>,
    payload: JsonValue,
}

/// What the ledger says after the run, read from the Capsule before and
/// after it and the Events.
struct Ledger {
    /// The Sessions open before the run (B: another client's; E: the
    /// client's parent).
    open_before: Vec<String>,
    /// The Sessions that began during the run: the client's.
    client: BTreeSet<String>,
    /// Every Session after the run with its `end_state`.
    sessions: Vec<(String, String)>,
    /// U10 T6: the `agent.role` of each of the client's Sessions after the
    /// run (`None`: it declared no agent).
    client_roles: Vec<Option<String>>,
    /// The Capsule's claims after the run (`None`: unreadable).
    claims: Option<Vec<JsonValue>>,
    events: Vec<Event>,
}

fn sessions_of(capsule: Option<&JsonValue>) -> Vec<(String, String)> {
    capsule
        .map_or(&[][..], |capsule| array_at(capsule, &["sessions"]))
        .iter()
        .map(|session| {
            (
                text_at(session, &["session_id"]).unwrap_or("").to_owned(),
                text_at(session, &["end_state"]).unwrap_or("").to_owned(),
            )
        })
        .collect()
}

/// A Session state that is not an end.
fn is_open(end_state: &str) -> bool {
    !matches!(end_state, "ended" | "crashed")
}

fn read_ledger(
    before: Option<&JsonValue>,
    after: Option<&JsonValue>,
    pages: &[JsonValue],
) -> Ledger {
    let earlier = sessions_of(before);
    let sessions = sessions_of(after);
    let seeded: BTreeSet<&str> = earlier.iter().map(|(id, _)| id.as_str()).collect();
    let events = pages
        .iter()
        .flat_map(|page| array_at(page, &["items"]))
        .enumerate()
        .map(|(index, event)| Event {
            sequence: index + 1,
            kind: text_at(event, &["event_type"]).unwrap_or("").to_owned(),
            session_id: text_at(event, &["session_id"]).map(str::to_owned),
            payload: member(event, &["payload"])
                .cloned()
                .unwrap_or(JsonValue::Null),
        })
        .collect();
    Ledger {
        open_before: earlier
            .iter()
            .filter(|(_, state)| is_open(state))
            .map(|(id, _)| id.clone())
            .collect(),
        client: sessions
            .iter()
            .filter(|(id, _)| !seeded.contains(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect(),
        claims: after.map(|capsule| array_at(capsule, &["claims"]).to_vec()),
        client_roles: after
            .map_or(&[][..], |capsule| array_at(capsule, &["sessions"]))
            .iter()
            .filter(|session| {
                !seeded.contains(text_at(session, &["session_id"]).unwrap_or_default())
            })
            .map(|session| text_at(session, &["agent", "role"]).map(str::to_owned))
            .collect(),
        sessions,
        events,
    }
}

impl Ledger {
    fn client_events<'a>(&'a self, event_type: &'a str) -> impl Iterator<Item = &'a Event> {
        self.events.iter().filter(move |event| {
            event.kind == event_type
                && event
                    .session_id
                    .as_ref()
                    .is_some_and(|id| self.client.contains(id))
        })
    }

    /// The Evidence IDs of the client's runs, and whether each passed as a
    /// test.
    fn client_runs(&self) -> Vec<(String, bool)> {
        self.client_events("command.completed")
            .map(|event| {
                let command = member(&event.payload, &["command"]);
                let id = command
                    .and_then(|command| text_at(command, &["evidence_id"]))
                    .unwrap_or("");
                let passed_test = command.is_some_and(|command| {
                    text_at(command, &["kind"]) == Some("test_result")
                        && member(command, &["passed"]) == Some(&JsonValue::Bool(true))
                });
                (id.to_owned(), passed_test)
            })
            .collect()
    }

    /// The claim numbered `number`, after the run.
    fn claim(&self, number: i64) -> Option<&JsonValue> {
        self.claims
            .as_ref()?
            .iter()
            .find(|claim| member(claim, &["number"]).and_then(JsonValue::as_i64) == Some(number))
    }
}

// --- The items -------------------------------------------------------------

type Judged = (Outcome, String);

fn pass(why: impl Into<String>) -> Judged {
    (Outcome::Pass, why.into())
}

fn fail(why: impl Into<String>) -> Judged {
    (Outcome::Fail, why.into())
}

fn hook_injected(transcript: &Transcript) -> Judged {
    match &transcript.hook {
        Some(hook) if hook.lines().any(|line| line.starts_with("Pack: ")) => {
            pass("the hook injected a Pack")
        }
        Some(_) => fail("the hook answered without a Pack"),
        None => fail("no response of memprove's hook in the transcript"),
    }
}

fn last_passed_line(transcript: &Transcript) -> Judged {
    let lines = transcript.hook.as_deref().unwrap_or("");
    if lines.lines().any(|line| line.starts_with("Last passed: ")) {
        pass("the injected Pack has a Last passed: line")
    } else {
        fail("no Last passed: line in an injected Pack")
    }
}

fn old_session_ended_first(ledger: &Ledger) -> Judged {
    if ledger.open_before.is_empty() {
        return fail("no Session was open before the run");
    }
    let first_client = ledger
        .client_events("session.started")
        .map(|event| event.sequence)
        .min();
    for old in &ledger.open_before {
        let ended = ledger.events.iter().find(|event| {
            event.session_id.as_deref() == Some(old.as_str())
                && matches!(event.kind.as_str(), "session.crashed" | "session.ended")
        });
        match ended {
            None => return fail(format!("the open Session {old} was not ended")),
            Some(event) if event.kind == "session.ended" => {
                return fail(format!(
                    "the open Session {old} was ended normally, not abandoned"
                ));
            }
            Some(event) => {
                if let Some(first) = first_client
                    && event.sequence > first
                {
                    return fail(format!(
                        "the open Session {old} ended abandoned at sequence {}, after the client's begin at {first}",
                        event.sequence
                    ));
                }
            }
        }
    }
    pass("every Session open before the run ended abandoned before the client's begin")
}

/// U13 E2 (Owner O3): in E, every Session that was open before the run, the
/// client's parent, is still open after it: no `session.crashed` or
/// `session.ended` of it is in the ledger, and the Capsule after the run lists
/// it as open.
fn parent_session_kept_open(ledger: &Ledger) -> Judged {
    if ledger.open_before.is_empty() {
        return fail("no Session was open before the run");
    }
    for old in &ledger.open_before {
        if let Some(event) = ledger.events.iter().find(|event| {
            event.session_id.as_deref() == Some(old.as_str())
                && matches!(event.kind.as_str(), "session.crashed" | "session.ended")
        }) {
            return fail(format!(
                "the open Session {old} was ended ({} at sequence {})",
                event.kind, event.sequence
            ));
        }
        match ledger.sessions.iter().find(|(id, _)| id == old) {
            Some((_, state)) if is_open(state) => {}
            Some((_, state)) => {
                return fail(format!("the open Session {old} is {state} after the run"));
            }
            None => {
                return fail(format!(
                    "the Capsule after the run does not list the open Session {old}"
                ));
            }
        }
    }
    pass(format!(
        "{} Session(s) open before the run, still open after it",
        ledger.open_before.len()
    ))
}

fn tests_run_passed(ledger: &Ledger) -> Judged {
    let runs = ledger.client_runs();
    if runs.iter().any(|(_, passed)| *passed) {
        pass(format!(
            "{} run(s) of the client, a passed test among them",
            runs.len()
        ))
    } else if runs.is_empty() {
        fail("the client recorded no run")
    } else {
        fail(format!(
            "none of the client's {} run(s) is a passed test",
            runs.len()
        ))
    }
}

/// The client's facts: claims of kind `fact` from its Sessions.
fn client_facts(ledger: &Ledger) -> Vec<&JsonValue> {
    ledger
        .claims
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|claim| {
            text_at(claim, &["kind"]) == Some("fact")
                && text_at(claim, &["session_id"]).is_some_and(|id| ledger.client.contains(id))
        })
        .collect()
}

fn fact_cites_own_run(ledger: &Ledger) -> Judged {
    let runs: BTreeSet<String> = ledger.client_runs().into_iter().map(|(id, _)| id).collect();
    let facts = client_facts(ledger);
    if facts.is_empty() {
        return fail("the client recorded no fact");
    }
    for fact in &facts {
        let cites_own = array_at(fact, &["evidence"]).iter().any(|evidence| {
            text_at(evidence, &["evidence_id"]).is_some_and(|id| runs.contains(id))
        });
        if !cites_own {
            let number = member(fact, &["number"])
                .and_then(JsonValue::as_i64)
                .unwrap_or(0);
            return fail(format!("fact #{number} cites no run of the client"));
        }
    }
    pass(format!(
        "{} fact(s), each citing a run of the client",
        facts.len()
    ))
}

fn sessions_closed(ledger: &Ledger, scenario: &str) -> Judged {
    // U13 E2 (Owner O3): in E the Sessions open before the run are the
    // client's parent's, which it must leave open; `parent_session_kept_open`
    // judges them.
    let left_to_parent_item =
        |id: &str| scenario == "E" && ledger.open_before.iter().any(|old| old == id);
    if let Some((id, _)) = ledger
        .sessions
        .iter()
        .find(|(id, state)| is_open(state) && !left_to_parent_item(id.as_str()))
    {
        return fail(format!("the Session {id} is still open"));
    }
    if let Some((id, state)) = ledger
        .sessions
        .iter()
        .find(|(id, state)| ledger.client.contains(id) && state != "ended")
    {
        return fail(format!("the client's Session {id} ended {state}"));
    }
    if ledger.client.is_empty() && scenario != "C" {
        return fail("the client began no Session");
    }
    pass(format!(
        "{} Session(s) of the client, none left open",
        ledger.client.len()
    ))
}

/// U13 E2 (Owner O3; a record from U13 E4, an item again since U14 E1, Owner
/// O3): in E, a Session of the client recorded an open item (of any
/// standing) whose text holds one of [`PARENT_NOTE_WORDS`]; it fails when
/// none does, the client recorded no open item, or the Capsule after the run
/// cannot be read. The hook's and the `pack` tool's opening state the
/// sentence of the rule that asks for it (U14 T1).
fn open_item_notes_parent(ledger: &Ledger) -> Judged {
    let Some(claims) = ledger.claims.as_deref() else {
        return fail("the Capsule after the run is not readable");
    };
    let items: Vec<&JsonValue> = claims
        .iter()
        .filter(|claim| {
            text_at(claim, &["kind"]) == Some("open_item")
                && text_at(claim, &["session_id"]).is_some_and(|id| ledger.client.contains(id))
        })
        .collect();
    if items.is_empty() {
        return fail("the client recorded no open item");
    }
    let noting = items.iter().find(|claim| {
        text_at(claim, &["text"])
            .is_some_and(|text| PARENT_NOTE_WORDS.iter().any(|words| text.contains(words)))
    });
    match noting {
        Some(claim) => pass(format!(
            "open item #{} of the client notes the Session it left open",
            member(claim, &["number"])
                .and_then(JsonValue::as_i64)
                .unwrap_or(0)
        )),
        None => fail(format!(
            "none of the client's {} open item(s) holds {} or {}",
            items.len(),
            PARENT_NOTE_WORDS[0],
            PARENT_NOTE_WORDS[1]
        )),
    }
}

// --- U12 T1: the class of a permission denial (Owner O3) --------------------

/// The class of one permission denial (U12 T1, Owner O3; ADR 0018
/// "Amendment (U12)"). Only `memprove` fails `no_permission_denials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DenialClass {
    /// An MCP tool of memprove, or a Bash command that runs only
    /// `memprove` and redirects nothing.
    Memprove,
    /// Not `memprove`, and it reads the environment or the clock.
    IdentityTime,
    /// Every other denial (a heredoc that writes code, `sed -i`, a
    /// `memprove run` with a redirection, `git commit`, …).
    Other,
}

impl DenialClass {
    const ALL: [Self; 3] = [Self::Memprove, Self::IdentityTime, Self::Other];

    fn word(self) -> &'static str {
        match self {
            Self::Memprove => "memprove",
            Self::IdentityTime => "identity-time",
            Self::Other => "other",
        }
    }
}

/// One permission denial of a run: the tool, its class, and the first 120
/// characters of the Bash command (none for a tool without one).
#[derive(Debug, Clone)]
struct Denial {
    tool: String,
    class: DenialClass,
    command: Option<String>,
}

/// The characters of a command `results.json` keeps.
const DENIAL_COMMAND_CHARS: usize = 120;

/// What the text of a Bash command says, read without running it: its
/// simple commands (each the words after quote removal), and whether it
/// redirects to or from a file (`>`, `>>`, `<`, `&>`, a heredoc, a
/// here-string) or substitutes a command (`$(…)`, `` `…` ``).
#[derive(Debug, Default)]
struct ShellText {
    commands: Vec<Vec<String>>,
    redirects: bool,
    substitutes: bool,
}

/// The redirection a word that follows an operator completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A file: `>`, `>>`, `<`, `&>`, `>|`, `<>`, `<<<`.
    File,
    /// `>&` or `<&`: a descriptor (digits or `-`) copies one and opens no
    /// file; anything else is a file.
    Descriptor,
    /// The delimiter of a heredoc (`<<`, or `<<-` when `true`).
    Heredoc(bool),
}

/// Reads a command's text as a POSIX shell would split it, as far as the
/// rule needs: single and double quotes and backslashes (a separator or an
/// operator inside quotes is a word), comments, the separators `&&`, `||`,
/// `;`, `|`, `|&`, `&`, `(`, `)`, and a newline, the redirection operators,
/// and heredoc bodies (data, not commands, skipped up to their delimiter).
/// Nothing is run and nothing is expanded.
struct ShellReader<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    text: ShellText,
    words: Vec<String>,
    word: Option<String>,
    target: Option<Target>,
    heredocs: Vec<(String, bool)>,
}

impl<'a> ShellReader<'a> {
    fn read(command: &'a str) -> ShellText {
        let mut reader = Self {
            chars: command.chars().peekable(),
            text: ShellText::default(),
            words: Vec::new(),
            word: None,
            target: None,
            heredocs: Vec::new(),
        };
        while let Some(character) = reader.chars.next() {
            reader.step(character);
        }
        reader.end_command();
        reader.text
    }

    fn push(&mut self, character: char) {
        self.word.get_or_insert_with(String::new).push(character);
    }

    fn end_word(&mut self) {
        let Some(word) = self.word.take() else {
            return;
        };
        match self.target.take() {
            None => self.words.push(word),
            Some(Target::File) => self.text.redirects = true,
            Some(Target::Descriptor) => {
                if !(word == "-" || (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())))
                {
                    self.text.redirects = true;
                }
            }
            Some(Target::Heredoc(strip_tabs)) => {
                self.text.redirects = true;
                self.heredocs.push((word, strip_tabs));
            }
        }
    }

    fn end_command(&mut self) {
        self.end_word();
        if self.target.take().is_some() {
            // An operator with no word after it still names a redirection.
            self.text.redirects = true;
        }
        if !self.words.is_empty() {
            self.text.commands.push(std::mem::take(&mut self.words));
        }
    }

    /// After a newline: the bodies of the heredocs of that line.
    fn skip_heredoc_bodies(&mut self) {
        for (delimiter, strip_tabs) in std::mem::take(&mut self.heredocs) {
            loop {
                let mut line = String::new();
                let mut ended = false;
                for character in self.chars.by_ref() {
                    if character == '\n' {
                        ended = true;
                        break;
                    }
                    line.push(character);
                }
                let line = if strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if line == delimiter || !ended {
                    break;
                }
            }
        }
    }

    fn single_quoted(&mut self) {
        let word = self.word.get_or_insert_with(String::new);
        for character in self.chars.by_ref() {
            if character == '\'' {
                return;
            }
            word.push(character);
        }
    }

    fn double_quoted(&mut self) {
        self.word.get_or_insert_with(String::new);
        while let Some(character) = self.chars.next() {
            match character {
                '"' => return,
                '\\' => match self.chars.next() {
                    Some('\n') => {}
                    Some(next @ ('"' | '\\' | '$' | '`')) => self.push(next),
                    Some(next) => {
                        self.push('\\');
                        self.push(next);
                    }
                    None => self.push('\\'),
                },
                '`' => {
                    self.text.substitutes = true;
                    self.push(character);
                }
                '$' if self.chars.peek() == Some(&'(') => {
                    self.text.substitutes = true;
                    self.push(character);
                }
                _ => self.push(character),
            }
        }
    }

    /// A redirection operator that starts with `first` (`>` or `<`).
    fn redirection(&mut self, first: char) {
        // A word of digits just before the operator is its descriptor.
        if self
            .word
            .as_deref()
            .is_some_and(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
        {
            self.word = None;
        }
        self.end_word();
        let next = self.chars.peek().copied();
        let target = match (first, next) {
            ('<', Some('<')) => {
                self.chars.next();
                match self.chars.peek() {
                    Some('<') => {
                        self.chars.next();
                        Target::File
                    }
                    Some('-') => {
                        self.chars.next();
                        Target::Heredoc(true)
                    }
                    _ => Target::Heredoc(false),
                }
            }
            (_, Some('&')) => {
                self.chars.next();
                Target::Descriptor
            }
            ('>', Some('>' | '|')) | ('<', Some('>')) => {
                self.chars.next();
                Target::File
            }
            _ => Target::File,
        };
        self.target = Some(target);
    }

    fn step(&mut self, character: char) {
        match character {
            '\'' => self.single_quoted(),
            '"' => self.double_quoted(),
            '\\' => match self.chars.next() {
                Some('\n') | None => {}
                Some(next) => self.push(next),
            },
            '#' if self.word.is_none() => {
                while self.chars.peek().is_some_and(|next| *next != '\n') {
                    self.chars.next();
                }
            }
            ' ' | '\t' => self.end_word(),
            '\n' => {
                self.end_command();
                self.skip_heredoc_bodies();
            }
            '`' => {
                self.text.substitutes = true;
                self.end_command();
            }
            '$' if self.chars.peek() == Some(&'(') => {
                self.text.substitutes = true;
                self.end_command();
            }
            ';' | '(' | ')' => self.end_command(),
            '|' => {
                if matches!(self.chars.peek(), Some('|' | '&')) {
                    self.chars.next();
                }
                self.end_command();
            }
            '&' => match self.chars.peek() {
                Some('&') => {
                    self.chars.next();
                    self.end_command();
                }
                Some('>') => {
                    self.chars.next();
                    self.end_word();
                    if self.chars.peek() == Some(&'>') {
                        self.chars.next();
                    }
                    self.target = Some(Target::File);
                }
                _ => self.end_command(),
            },
            '>' | '<' => self.redirection(character),
            _ => self.push(character),
        }
    }
}

/// Whether `word` is a `NAME=value` assignment.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && name
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
    })
}

/// The last component of a program's path.
fn basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// The program of a simple command after its leading `NAME=value`
/// assignments and a leading `env` with its options (`-u NAME`, `-i`,
/// `--`, …) and assignments: `Some(program)`, or `None` when nothing is
/// left; and whether an `env` was taken off.
fn program_of(words: &[String]) -> (Option<&str>, bool) {
    let mut rest = words.iter().map(String::as_str).peekable();
    let mut env = false;
    loop {
        while rest.next_if(|word| is_assignment(word)).is_some() {}
        if rest.peek().map(|word| basename(word)) != Some("env") {
            break;
        }
        rest.next();
        env = true;
        while let Some(option) = rest.next_if(|word| word.starts_with('-')) {
            if option == "--" {
                break;
            }
            if matches!(
                option,
                "-u" | "-P" | "-S" | "-C" | "--unset" | "--chdir" | "--split-string"
            ) {
                rest.next();
            }
        }
    }
    (rest.next(), env)
}

/// U12 T1 (Owner O3): the class of a denial of `tool` with the Bash
/// `command` (none when its element has none), by the text rule of ADR 0018
/// "Amendment (U12)". The rule runs nothing and reads nothing but the
/// denial.
fn denial_class(tool: &str, command: Option<&str>) -> DenialClass {
    if tool.starts_with("mcp__memprove__") {
        return DenialClass::Memprove;
    }
    let Some(command) = command.filter(|_| tool == "Bash") else {
        return DenialClass::Other;
    };
    let text = ShellReader::read(command);
    let programs: Vec<(Option<&str>, bool)> = text
        .commands
        .iter()
        .map(|words| program_of(words))
        .collect();
    if !text.redirects
        && !text.substitutes
        && !programs.is_empty()
        && programs
            .iter()
            .all(|(program, _)| program.is_some_and(|program| basename(program) == "memprove"))
    {
        return DenialClass::Memprove;
    }
    let reads_identity_or_time = programs.iter().any(|(program, env)| match program {
        Some(program) => matches!(basename(program), "printenv" | "date"),
        None => *env,
    }) || ["$MEMPROVE_", "${MEMPROVE_", "$(date"]
        .iter()
        .any(|reads| command.contains(reads));
    if reads_identity_or_time {
        DenialClass::IdentityTime
    } else {
        DenialClass::Other
    }
}

/// The denials of a transcript's `result`, each classed.
fn denials_of(result: &JsonValue) -> Vec<Denial> {
    array_at(result, &["permission_denials"])
        .iter()
        .map(|denial| {
            let tool = text_at(denial, &["tool_name"]).unwrap_or("?").to_owned();
            let command = text_at(denial, &["tool_input", "command"]);
            Denial {
                class: denial_class(&tool, command),
                tool,
                command: command
                    .map(|command| command.chars().take(DENIAL_COMMAND_CHARS).collect()),
            }
        })
        .collect()
}

/// U12 T1 (Owner O3): fails only on a denial of the class `memprove`;
/// the others are recorded (`results.json`, `results.md`), not judged.
fn no_permission_denials(transcript: &Transcript) -> Judged {
    let Some(result) = &transcript.result else {
        return fail("the transcript has no result");
    };
    if member(result, &["permission_denials"])
        .and_then(JsonValue::as_array)
        .is_none()
    {
        return fail("the result has no permission_denials");
    }
    let denials = denials_of(result);
    let on_memprove: Vec<&str> = denials
        .iter()
        .filter(|denial| denial.class == DenialClass::Memprove)
        .map(|denial| denial.tool.as_str())
        .collect();
    if denials.is_empty() {
        pass("no permission denial")
    } else if on_memprove.is_empty() {
        let count = |class| {
            denials
                .iter()
                .filter(|denial| denial.class == class)
                .count()
        };
        pass(format!(
            "{} denial(s), none on memprove (identity-time {}, other {})",
            denials.len(),
            count(DenialClass::IdentityTime),
            count(DenialClass::Other)
        ))
    } else {
        fail(format!(
            "{} of {} denial(s) on memprove: {}",
            on_memprove.len(),
            denials.len(),
            on_memprove.join(", ")
        ))
    }
}

/// The answer's last line without the Markdown around it (backticks, bold).
fn last_line(transcript: &Transcript) -> Option<&str> {
    transcript
        .answer()?
        .lines()
        .rev()
        .map(|line| {
            line.trim()
                .trim_matches(|character| matches!(character, '`' | '*'))
                .trim()
        })
        .find(|line| !line.is_empty())
}

/// The list after `Facts:` of the last line.
fn fact_list(line: &str) -> Option<&str> {
    line.strip_prefix("Facts:").map(str::trim)
}

fn last_line_format(transcript: &Transcript) -> Judged {
    let Some(line) = last_line(transcript) else {
        return fail("the transcript has no answer");
    };
    let Some(list) = fact_list(line) else {
        return fail("the last line does not start with Facts:");
    };
    if list.is_empty() || list.eq_ignore_ascii_case("none") {
        return pass("Facts: lists no number");
    }
    let well_formed = list.split([',', ';']).map(str::trim).all(|item| {
        item.strip_prefix('#').is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    });
    if well_formed {
        pass("the last line is Facts: #n[, #m…]")
    } else {
        fail("the last line is not Facts: #n[, #m…]")
    }
}

/// Every number of `text`: each run of ASCII digits, with or without `#`.
fn numbers_in(text: &str) -> Vec<i64> {
    text.split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn no_fabrication(transcript: &Transcript, ledger: &Ledger) -> Judged {
    let Some(list) = last_line(transcript).and_then(fact_list) else {
        return fail("no Facts: line to check");
    };
    if ledger.claims.is_none() {
        return fail("the Capsule after the run could not be read");
    }
    let numbers = numbers_in(list);
    for number in &numbers {
        match ledger
            .claim(*number)
            .and_then(|claim| text_at(claim, &["kind"]))
        {
            Some("fact") => {}
            Some(kind) => return fail(format!("#{number} is a {kind}, not a fact")),
            None => return fail(format!("#{number} is not in the ledger")),
        }
    }
    pass(format!(
        "{} number(s), each a fact in the ledger",
        numbers.len()
    ))
}

fn no_verified(transcript: &Transcript) -> Judged {
    if says_verified(transcript.hook.as_deref().unwrap_or("")) {
        return fail("the hook's Pack holds verified");
    }
    for tool in ["pack", "claims"] {
        if transcript
            .tool_results(tool)
            .iter()
            .any(|(_, text)| says_verified(text))
        {
            return fail(format!("a result of the {tool} tool holds verified"));
        }
    }
    if affirmed_verified(transcript.answer().unwrap_or("")) > 0 {
        return fail("the answer holds verified");
    }
    pass("no verified in the Packs or the answer")
}

/// What ends a clause of the answer (Owner, 2026-10-22).
const CLAUSE_ENDS: [char; 7] = [',', '.', ';', ':', '!', '?', '\n'];

/// The words that negate a `verified` after them in the same clause (U17:
/// the English words; before U17, five Chinese negation words). A
/// word that ends in `n't` negates too.
const NEGATIONS: [&str; 10] = [
    "not", "no", "never", "none", "nothing", "neither", "nor", "cannot", "without", "un",
];

/// A character of a word: `verified_fact` and `unverified` do not say the
/// word `verified`.
fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// The byte offsets at which `text` says the word `verified` as a word of
/// its own (not inside `unverified` or `verified_fact`), case folded.
fn verified_words(text: &str) -> Vec<usize> {
    let lower = text.to_ascii_lowercase();
    lower
        .match_indices("verified")
        .filter(|(at, found)| {
            let before = lower[..*at].chars().next_back();
            let after = lower[at + found.len()..].chars().next();
            !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
        })
        .map(|(at, _)| at)
        .collect()
}

/// Whether `text` says the word `verified` anywhere, negated or not: a Pack
/// or a tool result must not.
fn says_verified(text: &str) -> bool {
    !verified_words(text).is_empty()
}

/// Whether a negation word stands before byte `at` in `clause`.
fn negated_before(clause: &str, at: usize) -> bool {
    clause[..at]
        .split(|character: char| !is_word_char(character) && character != '\'')
        .map(str::to_ascii_lowercase)
        .any(|word| NEGATIONS.contains(&word.as_str()) || word.ends_with("n't"))
}

/// How many times `answer` says `verified` without a negation word before it
/// in the same clause (the Owner's amendment of 2026-10-22). Each
/// occurrence is judged on its own; a negation word anywhere before it in
/// its clause counts, whatever it modifies.
fn affirmed_verified(answer: &str) -> usize {
    answer
        .split(CLAUSE_ENDS)
        .map(|clause| {
            verified_words(clause)
                .into_iter()
                .filter(|at| !negated_before(clause, *at))
                .count()
        })
        .sum()
}

/// The hits of each `pack` query about the open items, read from its
/// `Query:` line (`N matches`).
fn open_query_hits(transcript: &Transcript) -> Vec<i64> {
    transcript
        .tool_results("pack")
        .iter()
        .filter(|(input, _)| {
            text_at(input, &["query"]).is_some_and(|query| {
                let query = query.to_lowercase();
                ["open", "todo", "pending"]
                    .iter()
                    .any(|word| query.contains(word))
            })
        })
        .filter_map(|(_, text)| {
            let line = text.lines().find(|line| line.starts_with("Query: "))?;
            let after = line.split_once("; ")?.1;
            after.split(' ').next()?.parse().ok()
        })
        .collect()
}

// --- Scoring ----------------------------------------------------------------

/// The files of one run's directory.
struct RunFiles {
    name: String,
    meta: JsonValue,
    /// `seed.json` (U13 E2: E's names the client's parent); `None` when it
    /// is missing or not strict JSON.
    seed: Option<JsonValue>,
    transcript: Vec<JsonValue>,
    before: Option<JsonValue>,
    after: Option<JsonValue>,
    events: Vec<JsonValue>,
}

fn load_run(dir: &Path) -> RunFiles {
    RunFiles {
        name: dir.file_name().unwrap().to_string_lossy().into_owned(),
        meta: json_file(&dir.join("run.json"))
            .unwrap_or_else(|| panic!("{}: run.json is not JSON", dir.display())),
        seed: json_file(&dir.join("seed.json")),
        transcript: json_lines(&dir.join("transcript.jsonl")),
        before: json_file(&dir.join("capsule-before.json")),
        after: json_file(&dir.join("capsule-after.json")),
        events: json_lines(&dir.join("events-after.jsonl")),
    }
}

/// U8 K6: `yes` when the client's first `memprove run` (its first
/// `command.completed` Event) is a test run, `no` when it is a command run,
/// `-` when the client ran nothing.
fn first_run_is_test(ledger: &Ledger) -> String {
    ledger
        .client_events("command.completed")
        .next()
        .map_or("-", |event| {
            let kind = member(&event.payload, &["command"])
                .and_then(|command| text_at(command, &["kind"]));
            if kind == Some("test_result") {
                "yes"
            } else {
                "no"
            }
        })
        .to_owned()
}

/// U9 K6 (Owner O7; U8 V-N17), as U10 T4 (Owner O4; VN10) reads U9 O7: in
/// B, who ended each Session that was open before the run, from the
/// `ended_by` of its `session.crashed` Event: `yes` when it names the agent
/// [`OLD_SESSION_ENDER`]; `named-other` when it names another agent; `no`
/// when it has no `ended_by`, an `ended_by` of a Session only (no agent), or
/// there is no such Event. Over several such Sessions the worst counts:
/// any `no` makes `no`, else any `named-other` makes `named-other`, else
/// `yes` (`no` too when there is none). U13 F2 (Owner O4): in F too, whose
/// client is B's agent. `n/a` in the other scenarios.
fn old_session_ended_by(scenario: &str, ledger: &Ledger) -> String {
    if !matches!(scenario, "B" | "F") {
        return "n/a".to_owned();
    }
    let each: Vec<&str> = ledger
        .open_before
        .iter()
        .map(|old| {
            let ender = ledger
                .events
                .iter()
                .find(|event| {
                    event.kind == "session.crashed"
                        && event.session_id.as_deref() == Some(old.as_str())
                })
                .and_then(|event| member(&event.payload, &["ended_by", "agent", "name"]))
                .and_then(JsonValue::as_str);
            match ender {
                Some(OLD_SESSION_ENDER) => "yes",
                Some(_) => "named-other",
                None => "no",
            }
        })
        .collect();
    if each.is_empty() || each.contains(&"no") {
        "no"
    } else if each.contains(&"named-other") {
        "named-other"
    } else {
        "yes"
    }
    .to_owned()
}

/// U10 T6 (Owner O4; D3 F22): in B (U13 E2: and E; U13 F2: and F), whether every Session the client began
/// declared the role `sub_agent` (the Capsule after the run, `agent.role`):
/// `yes` when each did, `no` when one did not (another role, or no agent),
/// `-` when the client began none; `n/a` in the other scenarios. `run.sh`
/// starts the B client with `MEMPROVE_AGENT_ROLE=sub_agent`, so `yes` says
/// that the declaration reached the Session the client began, through the
/// shell or through the MCP server (which reads its environment since T6).
fn client_session_role(scenario: &str, ledger: &Ledger) -> String {
    if !matches!(scenario, "B" | "E" | "F") {
        return "n/a".to_owned();
    }
    if ledger.client_roles.is_empty() {
        return "-".to_owned();
    }
    if ledger
        .client_roles
        .iter()
        .all(|role| role.as_deref() == Some("sub_agent"))
    {
        "yes"
    } else {
        "no"
    }
    .to_owned()
}

/// U11 T1 (Owner O3; U10 VN20): in B, whether the response of memprove's
/// `SessionStart` hook names the agent `run.sh` gives the B client
/// ([`HOOK_NAMES_AGENT`]): `yes` when it does, `no` when it does not or the
/// transcript holds no such response (an archive written before U11 then
/// scores `no`); `n/a` in the other scenarios. U13 E2 (Owner O3): in E the
/// sentence names the client's parent too, `parent` (`parent_session_id` of
/// `seed.json`; [`hook_names_agent_in_e`]); without it E scores `no`. U13
/// F2 (Owner O4): in F, B's sentence (F's client is B's agent, no parent).
fn hook_names_agent(scenario: &str, transcript: &Transcript, parent: Option<&str>) -> String {
    let sentence = match (scenario, parent) {
        ("B" | "F", _) => HOOK_NAMES_AGENT.to_owned(),
        ("E", Some(parent)) => hook_names_agent_in_e(parent),
        ("E", None) => return "no".to_owned(),
        _ => return "n/a".to_owned(),
    };
    if transcript
        .hook
        .as_deref()
        .is_some_and(|hook| hook.contains(&sentence))
    {
        "yes"
    } else {
        "no"
    }
    .to_owned()
}

/// U13 E2 (Owner O3): the sentence of the hook's opening that names E's
/// client, B's sentence with the parent: `You are agent「takeover-eval-other」(sub_agent, parent Session <parent>)`.
fn hook_names_agent_in_e(parent: &str) -> String {
    let open = HOOK_NAMES_AGENT
        .strip_suffix(')')
        .expect("HOOK_NAMES_AGENT ends with its role's parenthesis");
    format!("{open}, {HOOK_PARENT}{parent})")
}

/// U13 E2 (Owner O3): in E, whether the opening of memprove's hook (its
/// response before the fenced Pack) names as the client's parent
/// (`parent Session <ID>`) the Session E's seed left open, `parent` (`parent_session_id`
/// of `seed.json`): `yes` when it does; `no` when it names another or none,
/// there is no response of the hook, or the seed names no parent; `n/a` in
/// the other scenarios.
fn hook_names_parent(scenario: &str, transcript: &Transcript, parent: Option<&str>) -> String {
    if scenario != "E" {
        return "n/a".to_owned();
    }
    let named = transcript.hook.as_deref().and_then(|hook| {
        let opening = hook.split("```").next().unwrap_or_default();
        let (_, after) = opening.split_once(HOOK_PARENT)?;
        after
            .split(|c: char| c == ')' || c == ',' || c == ';' || c.is_whitespace())
            .next()
    });
    if named.is_some() && named == parent {
        "yes"
    } else {
        "no"
    }
    .to_owned()
}

/// U13 F2 (Owner O4): in F, whether the Pack the hook injected shows that the
/// Session which dispatched the open one has ended, as ADR 0021 "Amendment
/// (U11, S-K)" reads a Pack: `yes` when the `Session` line of each Session open
/// before the run (`Session <id>` on it) names its dispatcher `dispatched by Session <j>`
/// and the Pack's `Session j` line says it ended (it does not say `not ended`), or
/// the Pack has no `Session j` line (an ended Session it only counts); `no` when
/// that line names no such dispatcher (also `dispatched by unlisted Session <id>`, one the
/// Capsule does not list), the `Session j` line says `not ended`, an open Session
/// has no line, no Session was open before the run, or there is no response
/// of the hook; `n/a` in the other scenarios.
fn dispatcher_shown_ended(scenario: &str, transcript: &Transcript, ledger: &Ledger) -> String {
    if scenario != "F" {
        return "n/a".to_owned();
    }
    let shown = transcript.hook.as_deref().is_some_and(|hook| {
        let lines: Vec<&str> = hook
            .lines()
            .filter(|line| line.starts_with("Session "))
            .collect();
        !ledger.open_before.is_empty()
            && ledger.open_before.iter().all(|open| {
                let marker = format!(" {open} ");
                let Some(dispatcher) = lines
                    .iter()
                    .find(|line| line.contains(&marker))
                    .and_then(|line| dispatcher_number(line))
                else {
                    return false;
                };
                let prefix = format!("Session {dispatcher} ");
                lines
                    .iter()
                    .find(|line| line.starts_with(&prefix))
                    .is_none_or(|line| !line.contains(" not ended ("))
            })
    });
    if shown { "yes" } else { "no" }.to_owned()
}

/// The `<j>` of `, dispatched by Session <j>` in a Pack's `Session` line,
/// if it has one (`dispatched by unlisted Session <id>` is not one).
fn dispatcher_number(line: &str) -> Option<&str> {
    let (_, after) = line.split_once(DISPATCHED_BY)?;
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    let number = &after[..end];
    (!number.is_empty()).then_some(number)
}

fn score_run(files: &RunFiles) -> RunScore {
    let scenario = text_at(&files.meta, &["scenario"]).unwrap_or("").to_owned();
    let transcript = read_transcript(&files.transcript);
    let ledger = read_ledger(files.before.as_ref(), files.after.as_ref(), &files.events);
    // U13 E2: E's parent, from the seed.
    let parent = files
        .seed
        .as_ref()
        .and_then(|seed| text_at(seed, &["parent_session_id"]));
    let items = ITEMS
        .iter()
        .map(|(name, _, scenarios)| {
            let (outcome, why) = if scenarios.contains(scenario.as_str()) && !scenario.is_empty() {
                judge(name, &scenario, &transcript, &ledger)
            } else {
                (Outcome::NotApplicable, String::new())
            };
            (*name, outcome, why)
        })
        .collect();
    let result = transcript.result.as_ref();
    let records = RECORDS
        .iter()
        .map(|(name, _)| {
            let value = match *name {
                "first_run_is_test" => first_run_is_test(&ledger),
                "old_session_ended_by" => old_session_ended_by(&scenario, &ledger),
                "client_session_role" => client_session_role(&scenario, &ledger),
                "hook_names_agent" => hook_names_agent(&scenario, &transcript, parent),
                "hook_names_parent" => hook_names_parent(&scenario, &transcript, parent),
                "dispatcher_shown_ended" => dispatcher_shown_ended(&scenario, &transcript, &ledger),
                other => panic!("unknown record {other}"),
            };
            (*name, value)
        })
        .collect();
    RunScore {
        name: files.name.clone(),
        items,
        records,
        open_query_hits: open_query_hits(&transcript),
        queries: transcript
            .tool_results("pack")
            .iter()
            .filter(|(input, _)| member(input, &["query"]).is_some())
            .count(),
        turns: result
            .and_then(|result| member(result, &["num_turns"]))
            .and_then(JsonValue::as_i64),
        duration_ms: member(&files.meta, &["duration_ms"]).and_then(JsonValue::as_i64),
        model: transcript
            .model
            .clone()
            .or_else(|| text_at(&files.meta, &["model_requested"]).map(str::to_owned))
            .unwrap_or_else(|| "-".to_owned()),
        exit_code: member(&files.meta, &["exit_code"]).and_then(JsonValue::as_i64),
        tools: transcript.tools.and_then(|count| i64::try_from(count).ok()),
        seed_wait_seconds: member(&files.meta, &["seed_wait_seconds"]).and_then(JsonValue::as_i64),
        denials: result.map(denials_of).unwrap_or_default(),
        scenario,
    }
}

fn judge(name: &str, scenario: &str, transcript: &Transcript, ledger: &Ledger) -> Judged {
    match name {
        "hook_injected" => hook_injected(transcript),
        "last_passed_line" => last_passed_line(transcript),
        "old_session_ended_first" => old_session_ended_first(ledger),
        "parent_session_kept_open" => parent_session_kept_open(ledger),
        "tests_run_passed" => tests_run_passed(ledger),
        "fact_cites_own_run" => fact_cites_own_run(ledger),
        "sessions_closed" => sessions_closed(ledger, scenario),
        "open_item_notes_parent" => open_item_notes_parent(ledger),
        "no_permission_denials" => no_permission_denials(transcript),
        "last_line_format" => last_line_format(transcript),
        "no_fabrication" => no_fabrication(transcript, ledger),
        "no_verified" => no_verified(transcript),
        other => panic!("unknown item {other}"),
    }
}

/// The run directories of `dir` (`<scenario>-<run>` with a `run.json`), by
/// scenario, then run number.
fn run_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut runs: Vec<(String, u64, PathBuf)> = fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (scenario, run) = name.split_once('-')?;
            let run = run.parse::<u64>().ok()?;
            (["A", "B", "C", "D", "E", "F"].contains(&scenario)
                && entry.path().join("run.json").is_file())
            .then(|| (scenario.to_owned(), run, entry.path()))
        })
        .collect();
    runs.sort();
    runs.into_iter().map(|(_, _, path)| path).collect()
}

fn score_dir(dir: &Path) -> Vec<RunScore> {
    run_dirs(dir)
        .iter()
        .map(|run| score_run(&load_run(run)))
        .collect()
}

/// Each item over the runs, and whether every item meets its line.
fn summarize(scores: &[RunScore]) -> (Vec<ItemSummary>, bool) {
    let summary: Vec<ItemSummary> = ITEMS
        .iter()
        .map(|(name, kind, _)| {
            let outcomes: Vec<Outcome> = scores.iter().map(|score| score.outcome(name)).collect();
            let applicable = outcomes
                .iter()
                .filter(|outcome| **outcome != Outcome::NotApplicable)
                .count();
            let passed = outcomes
                .iter()
                .filter(|outcome| **outcome == Outcome::Pass)
                .count();
            let met = match kind {
                Kind::Strict => passed == applicable,
                Kind::Structural => 9 * passed >= 8 * applicable,
            };
            ItemSummary {
                name,
                kind: *kind,
                applicable,
                passed,
                met,
            }
        })
        .collect();
    let met = summary.iter().all(|item| item.met);
    (summary, met)
}

/// U8 K6: how many runs recorded each value of a record item, in the order
/// `yes`, `no`, `-`, `n/a`, and (U9 K6) `named-other`.
fn record_counts(scores: &[RunScore]) -> Vec<(&'static str, [usize; 5])> {
    RECORDS
        .iter()
        .map(|(name, _)| {
            let mut counts = [0; 5];
            for score in scores {
                let value = score
                    .records
                    .iter()
                    .find(|(recorded, _)| recorded == name)
                    .map_or("-", |(_, value)| value.as_str());
                if let Some(at) = ["yes", "no", "-", "n/a", "named-other"]
                    .iter()
                    .position(|word| *word == value)
                {
                    counts[at] += 1;
                }
            }
            (*name, counts)
        })
        .collect()
}

// --- Writing the results ----------------------------------------------------

/// U12 T1: how many denials of every run fall in each class.
fn denial_totals(scores: &[RunScore]) -> [(DenialClass, usize); 3] {
    DenialClass::ALL.map(|class| {
        let count = scores
            .iter()
            .flat_map(|score| &score.denials)
            .filter(|denial| denial.class == class)
            .count();
        (class, count)
    })
}

fn optional_number(value: Option<i64>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn render_json(scores: &[RunScore], summary: &[ItemSummary], met: bool) -> String {
    let mut out = String::from("{\n  \"runs\": [");
    for (index, score) in scores.iter().enumerate() {
        let items: Vec<String> = score
            .items
            .iter()
            .map(|(name, outcome, why)| {
                format!(
                    "\"{name}\": {{\"outcome\": \"{}\", \"why\": {}}}",
                    outcome.word(),
                    json_string(why)
                )
            })
            .collect();
        let records: Vec<String> = score
            .records
            .iter()
            .map(|(name, value)| format!("\"{name}\": {}", json_string(value)))
            .collect();
        let hits: Vec<String> = score.open_query_hits.iter().map(i64::to_string).collect();
        // U12 T1: each denial with its class and the command's start.
        let denials: Vec<String> = score
            .denials
            .iter()
            .map(|denial| {
                format!(
                    "{{\"tool_name\": {}, \"class\": \"{}\", \"command\": {}}}",
                    json_string(&denial.tool),
                    denial.class.word(),
                    denial
                        .command
                        .as_deref()
                        .map_or_else(|| "null".to_owned(), json_string)
                )
            })
            .collect();
        write!(
            out,
            "{}\n    {{\"run\": {}, \"scenario\": {}, \"items\": {{{}}}, \"records\": {{{}}}, \"denials\": [{}], \"open_query_hits\": [{}], \"queries\": {}, \"turns\": {}, \"duration_ms\": {}, \"model\": {}, \"exit_code\": {}, \"tools\": {}, \"seed_wait_seconds\": {}}}",
            if index == 0 { "" } else { "," },
            json_string(&score.name),
            json_string(&score.scenario),
            items.join(", "),
            records.join(", "),
            denials.join(", "),
            hits.join(", "),
            score.queries,
            optional_number(score.turns),
            optional_number(score.duration_ms),
            json_string(&score.model),
            optional_number(score.exit_code),
            optional_number(score.tools),
            optional_number(score.seed_wait_seconds),
        )
        .unwrap();
    }
    out.push_str("\n  ],\n  \"items\": [");
    for (index, item) in summary.iter().enumerate() {
        write!(
            out,
            "{}\n    {{\"item\": \"{}\", \"kind\": \"{}\", \"applicable\": {}, \"passed\": {}, \"met\": {}}}",
            if index == 0 { "" } else { "," },
            item.name,
            item.kind.word(),
            item.applicable,
            item.passed,
            item.met
        )
        .unwrap();
    }
    out.push_str("\n  ],\n  \"records\": [");
    for (index, (name, counts)) in record_counts(scores).iter().enumerate() {
        write!(
            out,
            "{}\n    {{\"record\": \"{name}\", \"yes\": {}, \"no\": {}, \"none\": {}, \"not_applicable\": {}, \"named_other\": {}}}",
            if index == 0 { "" } else { "," },
            counts[0],
            counts[1],
            counts[2],
            counts[3],
            counts[4]
        )
        .unwrap();
    }
    let totals: Vec<String> = denial_totals(scores)
        .iter()
        .map(|(class, count)| format!("\"{}\": {count}", class.word()))
        .collect();
    write!(
        out,
        "\n  ],\n  \"denial_classes\": {{{}}},\n  \"met\": {met}\n}}\n",
        totals.join(", ")
    )
    .unwrap();
    out
}

fn render_markdown(scores: &[RunScore], summary: &[ItemSummary], met: bool) -> String {
    let mut out = String::from("| run |");
    for (name, _, _) in ITEMS {
        write!(out, " {name} |").unwrap();
    }
    // U12 T1: the run's denials, and how many fall on memprove.
    out.push_str(" denials | memprove_denials |");
    for (name, _) in RECORDS {
        write!(out, " {name} |").unwrap();
    }
    // U10 T2, T3: the tools the client loaded and B's wait, `n/a` when the
    // archive does not say (before U10, or not B; U13 E1: E's wait too).
    out.push_str(
        " open query hits | queries | turns | seconds | model | exit | tools | B wait seconds |\n|---|",
    );
    out.push_str(&"---|".repeat(ITEMS.len() + RECORDS.len() + 10));
    out.push('\n');
    for score in scores {
        write!(out, "| {} |", score.name).unwrap();
        for (_, outcome, _) in &score.items {
            write!(out, " {} |", outcome.word()).unwrap();
        }
        write!(
            out,
            " {} | {} |",
            score.denials.len(),
            score
                .denials
                .iter()
                .filter(|denial| denial.class == DenialClass::Memprove)
                .count()
        )
        .unwrap();
        for (_, value) in &score.records {
            write!(out, " {value} |").unwrap();
        }
        let hits: Vec<String> = score.open_query_hits.iter().map(i64::to_string).collect();
        let shown = |value: Option<i64>| value.map_or_else(|| "n/a".to_owned(), |v| v.to_string());
        writeln!(
            out,
            " {} | {} | {} | {} | {} | {} | {} | {} |",
            if hits.is_empty() {
                "-".to_owned()
            } else {
                hits.join(", ")
            },
            score.queries,
            score
                .turns
                .map_or_else(|| "-".to_owned(), |turns| turns.to_string()),
            score
                .duration_ms
                .map_or_else(|| "-".to_owned(), |ms| format!("{}", (ms + 500) / 1000)),
            score.model,
            optional_number(score.exit_code),
            shown(score.tools),
            shown(score.seed_wait_seconds),
        )
        .unwrap();
    }
    out.push_str(
        "\n| item | kind | applicable | passed | line | met |\n|---|---|---|---|---|---|\n",
    );
    for item in summary {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            item.name,
            item.kind.word(),
            item.applicable,
            item.passed,
            match item.kind {
                Kind::Strict => "every run",
                Kind::Structural => "9 × passed ≥ 8 × applicable",
            },
            if item.met { "yes" } else { "NO" }
        )
        .unwrap();
    }
    out.push_str("\n| record | yes | no | - | n/a | named-other |\n|---|---|---|---|---|---|\n");
    for (name, counts) in record_counts(scores) {
        writeln!(
            out,
            "| {name} | {} | {} | {} | {} | {} |",
            counts[0], counts[1], counts[2], counts[3], counts[4]
        )
        .unwrap();
    }
    // U12 T1: the denials of every run by class, under the record table.
    let totals = denial_totals(scores);
    let words: Vec<String> = totals
        .iter()
        .map(|(class, count)| format!("{} {count}", class.word()))
        .collect();
    writeln!(
        out,
        "\nDenials by class: {} ({} in all)",
        words.join(", "),
        totals.iter().map(|(_, count)| count).sum::<usize>()
    )
    .unwrap();
    writeln!(out, "\nPass line met: {}", if met { "yes" } else { "NO" }).unwrap();
    out
}

#[test]
#[ignore = "needs TAKEOVER_EVAL_DIR, a results directory of scripts/takeover-eval/run.sh; see the module documentation"]
fn u7_takeover_eval_scores_the_archive() {
    let dir = PathBuf::from(
        std::env::var_os("TAKEOVER_EVAL_DIR")
            .expect("TAKEOVER_EVAL_DIR must be set for this entry"),
    );
    let scores = score_dir(&dir);
    assert!(!scores.is_empty(), "{}: no run directory", dir.display());
    let (summary, met) = summarize(&scores);
    fs::write(
        dir.join("results.json"),
        render_json(&scores, &summary, met),
    )
    .unwrap();
    let markdown = render_markdown(&scores, &summary, met);
    fs::write(dir.join("results.md"), &markdown).unwrap();
    println!("{markdown}");
    for score in &scores {
        for (name, outcome, why) in &score.items {
            if *outcome == Outcome::Fail {
                println!("{} {name}: {why}", score.name);
            }
        }
    }
    let below: Vec<&str> = summary
        .iter()
        .filter(|item| !item.met)
        .map(|item| item.name)
        .collect();
    assert!(met, "below the pass line: {}", below.join(", "));
}

// --- Unit tests on hand-made archives -------------------------------------

/// One run of a hand-made archive, as `run.sh` writes it, that passes every
/// item of its scenario until a test changes it: the seed's Session `s-seed`
/// (and in B the other client's `s-old`), the client's `s-client`, and the
/// claims #1 (the seed's fact), #2 (open item), #3 (next action), #4 (the
/// client's fact). U13 E2: in E the seed leaves the parent's `s-parent` open,
/// and the client adds #5, an open item about it ([`FakeRun::good_e`]).
struct FakeRun {
    scenario: &'static str,
    hook: Option<String>,
    /// Each `pack` call: its query and the text of its result.
    packs: Vec<(String, String)>,
    answer: String,
    /// Each permission denial of the `result`: its `tool_name` and, for a
    /// Bash denial, the command (U12 T1: the class is read from it; none
    /// writes no `tool_input`, as a denial of a tool without a command).
    denials: Vec<(&'static str, Option<&'static str>)>,
    has_result: bool,
    before: Vec<(&'static str, &'static str)>,
    after: Vec<(&'static str, &'static str)>,
    after_readable: bool,
    claims: Vec<String>,
    /// Each Event: its type, Session, and payload.
    events: Vec<(&'static str, Option<&'static str>, String)>,
    /// U10 T6: the role the client's Session `s-client` declared, if any.
    client_role: Option<&'static str>,
    /// U10 T3: the number of tools the transcript's `init` lists (none:
    /// an `init` without `tools`, as the hand-made archives before U10).
    tools: Option<usize>,
    /// U10 T2: `seed_wait_seconds` of `run.json` (none: not written, as
    /// before U10).
    seed_wait: Option<i64>,
    /// U13 E2: `parent_session_id` of `seed.json`, which is written only
    /// when this is set (E's archives; the others have none, as the
    /// hand-made archives before U13).
    parent: Option<&'static str>,
    /// U13 F2: `dispatcher_session_id` of `seed.json` (with `left_session_id`
    /// `s-old`), written only when this is set (F's archives).
    dispatcher: Option<&'static str>,
}

const PASSED: &str =
    r#"{"command":{"evidence_id":"EVIDENCE","exit_code":0,"kind":"test_result","passed":true}}"#;

fn run_payload(evidence: &str, passed: bool) -> String {
    PASSED
        .replace("EVIDENCE", evidence)
        .replace("\"passed\":true", &format!("\"passed\":{passed}"))
}

fn claim_json(number: i64, kind: &str, session: &str, evidence: &[&str]) -> String {
    let evidence: Vec<String> = evidence
        .iter()
        .map(|id| format!("{{\"evidence_id\":{}}}", json_string(id)))
        .collect();
    format!(
        "{{\"number\":{number},\"kind\":{},\"session_id\":{},\"evidence\":[{}]}}",
        json_string(kind),
        json_string(session),
        evidence.join(",")
    )
}

/// The sentence the hook's opening writes for a client whose process has
/// `MEMPROVE_AGENT_NAME` and `MEMPROVE_AGENT_ROLE` (U11 T1; a statement
/// since U12 T3).
fn hook_identity() -> String {
    format!(
        "{HOOK_NAMES_AGENT}; when begin and end get no agent_name, agent_role, or parent_session, \
         the server uses these three."
    )
}

/// U13 E2: the same sentence for a client whose process also has
/// `MEMPROVE_PARENT_SESSION=<parent>` (`hook::identity_sentence`).
fn hook_identity_with_parent(parent: &str) -> String {
    format!(
        "You are agent「takeover-eval-other」(sub_agent, parent Session {parent}); when begin and end \
         get no agent_name, agent_role, or parent_session, the server uses these three."
    )
}

/// U13 E2: the Session E's seed leaves open, the client's parent.
const FAKE_PARENT: &str = "s-parent";

/// U13 E2: the open item E's client records about its parent, ADR 0021's
/// sentence.
const FAKE_PARENT_NOTE: &str = "another Session (Session 2) may still be working, not ended";

/// U13 E2: E's Pack shows the Session the seed left open, the parent's (an
/// orchestrator), with the hint, as HEAD writes it (the dry run's
/// `pack-before.txt`).
const FAKE_E_SESSIONS: &str = "Sessions: 2 in all, 1 ended normally, 0 ended abandoned, 1 not ended, 0 declared a resumed_from Pack\n\
     Session 1 s-seed ended normally (started 2026-11-03T19:54:39.928662Z); agent「takeover-eval-seed」(sub_agent)\n\
     Session 2 s-parent not ended (started 2026-11-03T19:54:41.777840Z, last wrote to the ledger at 2026-11-03T19:54:42.038232Z); agent「takeover-eval-parent」(orchestrator); end it before taking over: memprove session end s-parent --abandoned --reason <why> (MCP: end {session:2, abandoned:true})\n";

/// U13 F2: the Session of F's seed that dispatched the one it left open
/// (`s-old`, as in B) and then ended normally.
const FAKE_DISPATCHER: &str = "s-dispatcher";

/// U13 F2: F's Pack shows the Session the seed left open, dispatched by the
/// one that ended, with the hint, and the dispatcher's line, ended; the
/// seeding Session, ended earlier, has no line. As HEAD writes it (the U13
/// S-J dry run's `pack-before.txt` of F, the IDs replaced).
const FAKE_F_SESSIONS: &str = "Sessions: 3 in all, 2 ended normally, 0 ended abandoned, 1 not ended, 0 declared a resumed_from Pack\n\
     Session 2 s-dispatcher ended normally (started 2026-11-06T17:31:07.103018Z); agent「takeover-eval-dispatcher」(orchestrator)\n\
     Session 3 s-old not ended (started 2026-11-06T17:31:07.350309Z, last wrote to the ledger at 2026-11-06T17:31:07.585897Z); agent「takeover-eval-left」(sub_agent, dispatched by Session 2); end it before taking over: memprove session end s-old --abandoned --reason <why> (MCP: end {session:3, abandoned:true})\n";

/// U13 E2: an open item of `session`, with its text.
fn open_item_json(number: i64, session: &str, text: &str) -> String {
    format!(
        "{{\"number\":{number},\"kind\":\"open_item\",\"session_id\":{},\"evidence\":[],\"text\":{}}}",
        json_string(session),
        json_string(text)
    )
}

/// The statement before the fenced Pack as the hook writes it since U11 (T1,
/// T2): the Task, the basis, the hook's clock, and, when `named`, who the
/// client is. The fence opens the next line.
fn hook_opening(named: bool) -> String {
    let identity = if named {
        hook_identity()
    } else {
        String::new()
    };
    format!(
        "memprove: below is the Context Pack of Task t at SessionStart (startup); it is the only \
         in-progress Task of this Workspace. The current UTC time is 2026-11-03T20:10:44Z. \
         {identity}\n```text\n"
    )
}

impl FakeRun {
    fn good(scenario: &'static str) -> Self {
        if scenario == "E" {
            return Self::good_e();
        }
        if scenario == "F" {
            return Self::good_f();
        }
        let tampered = scenario == "C";
        // U8 K6: in B the Pack shows the Session another client left open,
        // which declared an agent. As HEAD writes it (E1 revision 13, U10
        // T1, T7; real hook output of the U11 S-T development run, B-1):
        // the Session summary and the lines of both Sessions, the open one
        // with its agent (`takeover-eval-left`, not the client's), its last
        // write (16 minutes before the hook's clock, the evaluation's
        // wait), and the copyable `session end <ID> --abandoned` hint.
        let old_session = if scenario == "B" {
            "Sessions: 2 in all, 1 ended normally, 0 ended abandoned, 1 not ended, 0 declared a resumed_from Pack\n\
             Session 1 s-seed ended normally (started 2026-11-03T19:54:39.928662Z); agent「takeover-eval-seed」(sub_agent)\n\
             Session 2 s-old not ended (started 2026-11-03T19:54:41.777840Z, last wrote to the ledger at 2026-11-03T19:54:42.038232Z); agent「takeover-eval-left」(sub_agent); end it before taking over: memprove session end s-old --abandoned --reason <why> (MCP: end {session:2, abandoned:true})\n"
        } else {
            ""
        };
        // The opening of the hook's response (U11 T1, T2): the Task, the
        // current UTC time, and, when the hook's process has the agent
        // variables (run.sh gives them to the B client only), who the
        // client is.
        let opening = hook_opening(scenario == "B");
        let hook = if tampered {
            format!("{opening}Pack: memprove-pack v1\nTask「calc」 (in_progress): tampered\n```\n")
        } else {
            format!(
                "{opening}Pack: memprove-pack v1\n{old_session}Last passed: 「/py -m pytest -q」 (0123456789ab; --test --test-format pytest)\n#1 verified: 「add passes」\n```\n"
            )
        };
        let mut before = vec![("s-seed", "ended")];
        let mut after = vec![("s-seed", "ended")];
        let mut events = vec![
            ("session.started", Some("s-seed"), "{}".to_owned()),
            (
                "command.completed",
                Some("s-seed"),
                run_payload("e-seed", true),
            ),
            ("session.ended", Some("s-seed"), "{}".to_owned()),
        ];
        if scenario == "B" {
            before.push(("s-old", "active"));
            after.push(("s-old", "crashed"));
            events.push(("session.started", Some("s-old"), "{}".to_owned()));
            events.push((
                "session.crashed",
                Some("s-old"),
                crashed_payload(Some(OLD_SESSION_ENDER)),
            ));
        }
        after.push(("s-client", "ended"));
        events.extend([
            ("session.started", Some("s-client"), "{}".to_owned()),
            (
                "command.completed",
                Some("s-client"),
                run_payload("e-client", true),
            ),
            ("checkpoint.created", Some("s-client"), "{}".to_owned()),
            ("session.ended", Some("s-client"), "{}".to_owned()),
        ]);
        Self {
            scenario,
            hook: Some(hook),
            packs: vec![(
                "open".to_owned(),
                "Pack: memprove-pack v1\nQuery: 「open」; 1 matches, by rank: #2\n#2 open item: 「divide」\n".to_owned(),
            )],
            answer: if tampered { "The ledger is tampered.\nFacts: #4" } else { "Done.\nFacts: #1, #4" }.to_owned(),
            denials: Vec::new(),
            has_result: true,
            before,
            after,
            after_readable: true,
            claims: vec![
                claim_json(1, "fact", "s-seed", &["e-seed"]),
                claim_json(2, "open_item", "s-seed", &[]),
                claim_json(3, "next_action", "s-seed", &[]),
                claim_json(4, "fact", "s-client", &["e-client"]),
            ],
            events,
            // U10 T6: run.sh starts every client as a sub_agent in B.
            client_role: Some("sub_agent"),
            tools: None,
            seed_wait: None,
            parent: None,
            dispatcher: None,
        }
    }

    /// U13 E2 (Owner O3): E's archive is A's, with the parent's Session
    /// `s-parent` (it ran the tests) open before the run and still open after
    /// it, the hook naming the client and that parent, and the client's open
    /// item #5 about it; `seed.json` names the parent.
    fn good_e() -> Self {
        let mut run = Self::good("A");
        run.scenario = "E";
        run.hook = Some(format!(
            "{}Pack: memprove-pack v1\n{FAKE_E_SESSIONS}Last passed: 「/py -m pytest -q」 (0123456789ab; --test --test-format pytest)\n#1 verified: 「add passes」\n```\n",
            hook_opening(true).replace(&hook_identity(), &hook_identity_with_parent(FAKE_PARENT))
        ));
        run.before.push((FAKE_PARENT, "active"));
        run.after.insert(1, (FAKE_PARENT, "active"));
        // After the seed's three Events, before the client's.
        run.events
            .insert(3, ("session.started", Some(FAKE_PARENT), "{}".to_owned()));
        run.events.insert(
            4,
            (
                "command.completed",
                Some(FAKE_PARENT),
                run_payload("e-parent", true),
            ),
        );
        run.claims
            .push(open_item_json(5, "s-client", FAKE_PARENT_NOTE));
        run.parent = Some(FAKE_PARENT);
        run
    }

    /// U13 F2 (Owner O4): F's archive is B's, with the dispatcher's Session
    /// `s-dispatcher`, which began before the left one (`s-old`, which ran the
    /// tests) and ended normally after it, so that only `s-old` is open
    /// before the run; the hook's Pack shows `s-old` dispatched by Session 2, which
    /// ended; the client ends `s-old` abandoned before its `begin`, as in B;
    /// `seed.json` names both.
    fn good_f() -> Self {
        let mut run = Self::good("B");
        run.scenario = "F";
        run.hook = Some(format!(
            "{}Pack: memprove-pack v1\n{FAKE_F_SESSIONS}Last passed: 「/py -m pytest -q」 (0123456789ab; --test --test-format pytest)\n#1 verified: 「add passes」\n```\n",
            hook_opening(true)
        ));
        run.before.insert(1, (FAKE_DISPATCHER, "ended"));
        run.after.insert(1, (FAKE_DISPATCHER, "ended"));
        // After the seed's three Events: the dispatcher begins, the left
        // Session begins and runs the tests, the dispatcher ends.
        run.events.insert(
            3,
            ("session.started", Some(FAKE_DISPATCHER), "{}".to_owned()),
        );
        let left_run = run
            .events
            .iter()
            .position(|(kind, session, _)| *kind == "session.started" && *session == Some("s-old"))
            .unwrap()
            + 1;
        run.events.insert(
            left_run,
            (
                "command.completed",
                Some("s-old"),
                run_payload("e-old", true),
            ),
        );
        run.events.insert(
            left_run + 1,
            ("session.ended", Some(FAKE_DISPATCHER), "{}".to_owned()),
        );
        run.dispatcher = Some(FAKE_DISPATCHER);
        run
    }

    fn transcript(&self) -> String {
        let mut lines = Vec::new();
        if let Some(hook) = &self.hook {
            lines.push(format!(
                "{{\"type\":\"system\",\"subtype\":\"hook_response\",\"hook_name\":\"SessionStart:startup\",\"output\":{0},\"stdout\":{0},\"exit_code\":0}}",
                json_string(hook)
            ));
        }
        lines.push(match self.tools {
            Some(count) => {
                let names: Vec<String> = (0..count).map(|n| format!("\"tool-{n}\"")).collect();
                format!(
                    "{{\"type\":\"system\",\"subtype\":\"init\",\"model\":\"fake-model\",\"tools\":[{}]}}",
                    names.join(",")
                )
            }
            None => r#"{"type":"system","subtype":"init","model":"fake-model"}"#.to_owned(),
        });
        for (index, (query, text)) in self.packs.iter().enumerate() {
            lines.push(format!(
                "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"t{index}\",\"name\":\"mcp__memprove__pack\",\"input\":{{\"query\":{}}}}}]}}}}",
                json_string(query)
            ));
            lines.push(format!(
                "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"t{index}\",\"content\":[{{\"type\":\"text\",\"text\":{}}}]}}]}}}}",
                json_string(text)
            ));
        }
        if self.has_result {
            let denials: Vec<String> = self
                .denials
                .iter()
                .enumerate()
                .map(|(index, (tool, command))| match command {
                    Some(command) => format!(
                        "{{\"tool_name\":{},\"tool_use_id\":\"d{index}\",\"tool_input\":{{\"command\":{},\"description\":\"x\"}}}}",
                        json_string(tool),
                        json_string(command)
                    ),
                    None => format!("{{\"tool_name\":{}}}", json_string(tool)),
                })
                .collect();
            lines.push(format!(
                "{{\"type\":\"result\",\"subtype\":\"success\",\"num_turns\":12,\"duration_ms\":34000,\"permission_denials\":[{}],\"result\":{}}}",
                denials.join(","),
                json_string(&self.answer)
            ));
        }
        lines.join("\n") + "\n"
    }

    fn capsule(sessions: &[(&str, &str)], claims: &[String], role: Option<&str>) -> String {
        let sessions: Vec<String> = sessions
            .iter()
            .map(|(id, state)| {
                // U10 T6: the client's Session with the agent it declared.
                let agent = match role.filter(|_| *id == "s-client") {
                    Some(role) => format!(
                        ",\"agent\":{{\"name\":\"takeover-eval-other\",\"role\":{}}}",
                        json_string(role)
                    ),
                    None => String::new(),
                };
                format!(
                    "{{\"session_id\":{},\"end_state\":{}{agent}}}",
                    json_string(id),
                    json_string(state)
                )
            })
            .collect();
        format!(
            "{{\"status\":\"verified\",\"sessions\":[{}],\"claims\":[{}]}}\n",
            sessions.join(","),
            claims.join(",")
        )
    }

    /// Writes the run as `<root>/<scenario>-<run>` and returns it.
    fn write(&self, root: &Path, run: u32) -> PathBuf {
        let dir = root.join(format!("{}-{run}", self.scenario));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("run.json"),
            format!(
                "{{\"scenario\":\"{}\",\"run\":{run},\"model_requested\":null,\"exit_code\":0,\"duration_ms\":41000{}}}\n",
                self.scenario,
                self.seed_wait
                    .map_or_else(String::new, |wait| format!(",\"seed_wait_seconds\":{wait}"))
            ),
        )
        .unwrap();
        fs::write(dir.join("transcript.jsonl"), self.transcript()).unwrap();
        // U13 E2: E's seed names the parent (U13 F2: F's the dispatcher and
        // the Session it left open); the others write no seed.json, which the
        // entry does without.
        if self.parent.is_some() || self.dispatcher.is_some() {
            let mut members = String::new();
            if let Some(parent) = self.parent {
                write!(members, ",\"parent_session_id\":{}", json_string(parent)).unwrap();
            }
            if let Some(dispatcher) = self.dispatcher {
                write!(
                    members,
                    ",\"dispatcher_session_id\":{},\"left_session_id\":\"s-old\"",
                    json_string(dispatcher)
                )
                .unwrap();
            }
            fs::write(
                dir.join("seed.json"),
                format!(
                    "{{\"scenario\":\"{}\",\"project_id\":\"p\",\"task_id\":\"t\",\"ledger_verify_before_exit\":0,\"root\":\"/private/tmp/u7-h.fake\"{members}}}\n",
                    self.scenario
                ),
            )
            .unwrap();
        }
        fs::write(
            dir.join("capsule-before.json"),
            Self::capsule(&self.before, &self.claims[..3], self.client_role),
        )
        .unwrap();
        let after = if self.after_readable {
            Self::capsule(&self.after, &self.claims, self.client_role)
        } else {
            String::new()
        };
        fs::write(dir.join("capsule-after.json"), after).unwrap();
        let events: Vec<String> = self
            .events
            .iter()
            .map(|(event_type, session, payload)| {
                format!(
                    "{{\"event_type\":\"{event_type}\",\"session_id\":{},\"payload\":{payload}}}",
                    session.map_or_else(|| "null".to_owned(), json_string)
                )
            })
            .collect();
        fs::write(
            dir.join("events-after.jsonl"),
            format!(
                "{{\"items\":[{}],\"next_cursor\":null}}\n",
                events.join(",")
            ),
        )
        .unwrap();
        dir
    }

    fn score(&self) -> RunScore {
        let root = tempfile::tempdir().unwrap();
        score_run(&load_run(&self.write(root.path(), 1)))
    }
}

/// The item outcomes of `score` other than `Pass` and `n/a`, with why.
fn failures(score: &RunScore) -> Vec<String> {
    score
        .items
        .iter()
        .filter(|(_, outcome, _)| *outcome == Outcome::Fail)
        .map(|(name, _, why)| format!("{name}: {why}"))
        .collect()
}

#[test]
fn u7_h2_a_good_run_of_each_scenario_passes_every_item_that_applies() {
    for scenario in ["A", "B", "C", "D"] {
        let score = FakeRun::good(scenario).score();
        assert!(
            failures(&score).is_empty(),
            "{scenario}: {:?}",
            failures(&score)
        );
        for (name, _, scenarios) in ITEMS {
            let expected = if scenarios.contains(scenario) {
                Outcome::Pass
            } else {
                Outcome::NotApplicable
            };
            assert_eq!(score.outcome(name), expected, "{scenario} {name}");
        }
    }
}

#[test]
fn u7_h2_the_hook_items_need_memproves_pack_and_its_last_passed_line() {
    let mut run = FakeRun::good("A");
    run.hook = None;
    let score = run.score();
    assert_eq!(score.outcome("hook_injected"), Outcome::Fail);
    assert_eq!(score.outcome("last_passed_line"), Outcome::Fail);

    run.hook =
        Some("memprove: this directory has no memprove context; no Context Pack was injected into this session.".to_owned());
    assert_eq!(run.score().outcome("hook_injected"), Outcome::Fail);

    run.hook = Some(
        "memprove: below is the Context Pack of Task t.\n```text\nPack: memprove-pack v1\n```\n"
            .to_owned(),
    );
    let score = run.score();
    assert_eq!(score.outcome("hook_injected"), Outcome::Pass);
    assert_eq!(score.outcome("last_passed_line"), Outcome::Fail);
}

#[test]
fn u7_h2_the_old_session_must_end_abandoned_before_the_clients_begin() {
    // Abandoned, but after the client's begin.
    let mut run = FakeRun::good("B");
    let crash = run
        .events
        .iter()
        .position(|(kind, _, _)| *kind == "session.crashed")
        .unwrap();
    let crashed = run.events.remove(crash);
    run.events.push(crashed);
    assert_eq!(
        run.score().outcome("old_session_ended_first"),
        Outcome::Fail
    );

    // Ended normally instead of abandoned.
    let mut run = FakeRun::good("B");
    for event in &mut run.events {
        if event.0 == "session.crashed" {
            event.0 = "session.ended";
        }
    }
    run.after[1].1 = "ended";
    assert_eq!(
        run.score().outcome("old_session_ended_first"),
        Outcome::Fail
    );

    // Left open.
    let mut run = FakeRun::good("B");
    run.events.retain(|(kind, _, _)| *kind != "session.crashed");
    run.after[1].1 = "active";
    let score = run.score();
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Fail);
    assert_eq!(score.outcome("sessions_closed"), Outcome::Fail);
}

#[test]
fn u7_h2_a_passed_test_run_of_the_client_and_a_fact_citing_it_are_needed() {
    let mut run = FakeRun::good("A");
    for event in &mut run.events {
        if event.1 == Some("s-client") && event.0 == "command.completed" {
            event.2 = run_payload("e-client", false);
        }
    }
    assert_eq!(run.score().outcome("tests_run_passed"), Outcome::Fail);

    // Only the seed's run, which the client's fact cites.
    let mut run = FakeRun::good("D");
    run.events
        .retain(|event| !(event.1 == Some("s-client") && event.0 == "command.completed"));
    run.claims[3] = claim_json(4, "fact", "s-client", &["e-seed"]);
    let score = run.score();
    assert_eq!(score.outcome("tests_run_passed"), Outcome::Fail);
    assert_eq!(score.outcome("fact_cites_own_run"), Outcome::Fail);

    // No fact of the client.
    let mut run = FakeRun::good("B");
    run.claims[3] = claim_json(4, "open_item", "s-client", &["e-client"]);
    run.answer = "Done.\nFacts: #1".to_owned();
    let score = run.score();
    assert_eq!(score.outcome("fact_cites_own_run"), Outcome::Fail);
    assert_eq!(score.outcome("tests_run_passed"), Outcome::Pass);
}

#[test]
fn u7_h2_every_session_must_be_closed_and_the_clients_ended_normally() {
    let mut run = FakeRun::good("A");
    run.after[1].1 = "active";
    assert_eq!(run.score().outcome("sessions_closed"), Outcome::Fail);

    run.after[1].1 = "crashed";
    assert_eq!(run.score().outcome("sessions_closed"), Outcome::Fail);

    // No Session of the client: a failure in A, not in C.
    let mut run = FakeRun::good("A");
    run.after.pop();
    run.events.retain(|event| event.1 != Some("s-client"));
    run.claims.pop();
    run.answer = "Facts: #1".to_owned();
    assert_eq!(run.score().outcome("sessions_closed"), Outcome::Fail);
    let mut run = FakeRun::good("C");
    run.after.pop();
    run.events.retain(|event| event.1 != Some("s-client"));
    run.claims.pop();
    run.answer = "Facts: none".to_owned();
    let score = run.score();
    assert!(failures(&score).is_empty(), "{:?}", failures(&score));
}

#[test]
fn u7_h2_a_permission_denial_or_no_result_fails() {
    // U12 T1 (Owner O3): a denial that falls on a memprove command.
    let mut run = FakeRun::good("A");
    run.denials = vec![("Bash", Some("memprove context show"))];
    let score = run.score();
    assert_eq!(score.outcome("no_permission_denials"), Outcome::Fail);
    assert!(
        failures(&score)[0].contains("1 of 1 denial(s) on memprove: Bash"),
        "{:?}",
        failures(&score)
    );

    let mut run = FakeRun::good("C");
    run.has_result = false;
    let score = run.score();
    for item in [
        "no_permission_denials",
        "last_line_format",
        "no_fabrication",
    ] {
        assert_eq!(score.outcome(item), Outcome::Fail, "{item}");
    }
}

// --- U12 T1: the class of a permission denial -------------------------------

/// A heredoc that writes code and commits it, as the clients of the U10 and
/// U11 baselines wrote it (`cat >> … <<'EOF'`).
const HEREDOC_COMMAND: &str = "cat >> calc.py <<'EOF'\n\n\ndef divide(a, b):\n    return a / b\nEOF\ngit add calc.py && git commit -q -m \"calc: divide\"";

/// U12 T1 (Owner O3): the seven denials of the specification's test and the
/// three the independent check adds, each with the tool, the command (none
/// for an MCP tool), and the class the scoring entry gives it.
const DENIAL_SAMPLES: [(&str, Option<&str>, &str); 10] = [
    ("Bash", Some(HEREDOC_COMMAND), "other"),
    (
        "Bash",
        Some(
            "memprove run --test --test-format pytest -- /usr/local/bin/python3.12 -m pytest -q > /tmp/x",
        ),
        "other",
    ),
    (
        "Bash",
        Some("printenv MEMPROVE_AGENT_NAME MEMPROVE_AGENT_ROLE MEMPROVE_PARENT_SESSION"),
        "identity-time",
    ),
    ("Bash", Some("echo $MEMPROVE_AGENT_NAME"), "identity-time"),
    ("Bash", Some("memprove context show"), "memprove"),
    ("mcp__memprove__begin", None, "memprove"),
    ("Bash", Some("env -u X memprove run -- true"), "memprove"),
    ("Bash", Some("memprove run && echo ok"), "other"),
    (
        "Bash",
        Some("MEMPROVE_AGENT_NAME=x memprove context show"),
        "memprove",
    ),
    ("Bash", Some("cat x | memprove run -- true"), "other"),
];

/// The `results.json` of a directory holding `runs`, parsed.
fn results_json(runs: &[FakeRun]) -> JsonValue {
    let root = tempfile::tempdir().unwrap();
    let mut numbers = std::collections::BTreeMap::new();
    for run in runs {
        let number = numbers.entry(run.scenario).or_insert(0);
        *number += 1;
        run.write(root.path(), *number);
    }
    let scores = score_dir(root.path());
    let (summary, met) = summarize(&scores);
    parse_strict(&render_json(&scores, &summary, met)).unwrap()
}

/// The class `results.json` gives one denial of `tool` with `command` in a
/// run of A, and the outcome of `no_permission_denials` in that run.
fn denial_of(tool: &'static str, command: Option<&'static str>) -> (Option<String>, Outcome) {
    let mut run = FakeRun::good("A");
    run.denials = vec![(tool, command)];
    let outcome = run.score().outcome("no_permission_denials");
    let json = results_json(&[run]);
    let class = array_at(&array_at(&json, &["runs"])[0], &["denials"])
        .first()
        .and_then(|denial| text_at(denial, &["class"]))
        .map(str::to_owned);
    (class, outcome)
}

/// U12 T1 (Owner O3; U11-BN1): each permission denial is classed
/// `memprove` (an MCP tool of memprove, or a Bash command whose every
/// simple command runs `memprove`, after a leading `env` with its options
/// and `NAME=value` assignments, with no redirection), `identity-time` (a
/// command that runs `printenv`, `date`, or `env` alone, or reads
/// `$MEMPROVE_`, `${MEMPROVE_`, or `$(date`), or `other`; and
/// `no_permission_denials` fails only on a `memprove` one.
#[test]
fn u12_t1_each_denial_has_a_class_and_only_memproves_fail_the_item() {
    for (tool, command, class) in DENIAL_SAMPLES {
        let (classed, outcome) = denial_of(tool, command);
        assert_eq!(classed.as_deref(), Some(class), "{tool} {command:?}");
        let expected = if class == "memprove" {
            Outcome::Fail
        } else {
            Outcome::Pass
        };
        assert_eq!(outcome, expected, "{tool} {command:?}");
    }
    // A run with only the denials that are not memprove's passes; one more
    // on memprove fails it, and says how many of how many.
    let mut run = FakeRun::good("B");
    run.denials = DENIAL_SAMPLES
        .iter()
        .filter(|(_, _, class)| *class != "memprove")
        .map(|(tool, command, _)| (*tool, *command))
        .collect();
    let score = run.score();
    assert_eq!(score.outcome("no_permission_denials"), Outcome::Pass);
    assert!(failures(&score).is_empty(), "{:?}", failures(&score));
    run.denials.push(("mcp__memprove__end", None));
    let score = run.score();
    assert_eq!(score.outcome("no_permission_denials"), Outcome::Fail);
    assert!(
        failures(&score)[0].contains("1 of 7 denial(s) on memprove: mcp__memprove__end"),
        "{:?}",
        failures(&score)
    );
}

/// U12 T1: the edges of the text rule, which reads the command and runs
/// nothing. A separator, a redirection, or a heredoc inside quotes is a
/// word; `2>&1` copies a descriptor and opens no file; a comment is not a
/// command; a command substitution and `&` run another command; a heredoc's
/// body is data, not commands; a program is matched by its basename.
#[test]
fn u12_t1_the_text_rule_at_its_edges() {
    for (command, class) in [
        ("memprove run --help 2>&1", "memprove"),
        (
            "memprove run -- sh -c \"pytest; echo done > x\"",
            "memprove",
        ),
        ("memprove run -- sh -c 'a | b && c'", "memprove"),
        (
            "/private/tmp/u7-h.x/bin/memprove session end s --abandoned --reason gone",
            "memprove",
        ),
        ("# run the tests\nmemprove run -- true", "memprove"),
        ("X=1 env -i -u Y -- Z=2 memprove status", "memprove"),
        (
            "memprove end --reason done\nmemprove resume --text",
            "memprove",
        ),
        ("memprove run -- $(which python3) -m pytest", "other"),
        ("memprove run -- `which python3` -m pytest", "other"),
        ("memprove run -- true & echo ok", "other"),
        ("memprove run -- true 2> err.txt", "other"),
        ("memprove run -- python3 - <<'EOF'\nprint(1)\nEOF", "other"),
        ("memprove run -- true >&2", "memprove"),
        ("memprove run -- true &> out.txt", "other"),
        ("env", "identity-time"),
        ("env | grep MEMPROVE", "identity-time"),
        ("date -u; git status", "identity-time"),
        ("/bin/date", "identity-time"),
        ("echo \"PARENT=$MEMPROVE_PARENT_SESSION\"", "identity-time"),
        ("echo ${MEMPROVE_AGENT_ROLE}", "identity-time"),
        ("echo \"now $(date -u)\"", "identity-time"),
        (
            "env -u X memprove run -- true; printenv HOME",
            "identity-time",
        ),
        ("cat > note.txt <<'EOF'\ndate\nEOF", "other"),
        ("git status", "other"),
        ("", "other"),
    ] {
        let (classed, _) = denial_of("Bash", Some(Box::leak(command.to_owned().into_boxed_str())));
        assert_eq!(classed.as_deref(), Some(class), "{command:?}");
    }
    // A tool other than Bash and memprove's MCP tools, and a Bash denial
    // whose element has no command (an archive that kept only the tool).
    assert_eq!(denial_of("Edit", None).0.as_deref(), Some("other"));
    assert_eq!(
        denial_of("mcp__other__begin", None).0.as_deref(),
        Some("other")
    );
    assert_eq!(denial_of("Bash", None).0.as_deref(), Some("other"));
}

/// U12 T1: `results.json` keeps every denial of a run (`tool_name`, the
/// class, the command's first 120 characters, `null` without one) and the
/// totals of the three classes; `results.md` has the columns `denials` and
/// `memprove_denials` after the items, and a line of the three totals
/// under the record table. An archive whose denials name only the tool
/// (no `tool_input`) is still scored.
#[test]
fn u12_t1_the_results_keep_each_denial_and_its_class() {
    let long: &'static str =
        Box::leak(format!("memprove run -- {}", "€".repeat(130)).into_boxed_str());
    let mut a = FakeRun::good("A");
    a.denials = vec![
        ("Bash", Some(HEREDOC_COMMAND)),
        ("Bash", Some("printenv")),
        ("Bash", Some(long)),
    ];
    let mut b = FakeRun::good("B");
    b.denials = vec![("mcp__memprove__begin", None), ("Edit", None)];
    let c = FakeRun::good("C");
    let json = results_json(&[a, b, c]);
    let runs = array_at(&json, &["runs"]);
    let denials = array_at(&runs[0], &["denials"]);
    assert_eq!(denials.len(), 3);
    assert_eq!(text_at(&denials[0], &["tool_name"]), Some("Bash"));
    assert_eq!(text_at(&denials[0], &["class"]), Some("other"));
    let first: String = HEREDOC_COMMAND.chars().take(120).collect();
    assert_eq!(text_at(&denials[0], &["command"]), Some(first.as_str()));
    assert_eq!(text_at(&denials[1], &["class"]), Some("identity-time"));
    let cut: String = long.chars().take(120).collect();
    assert_eq!(cut.chars().count(), 120);
    assert_eq!(text_at(&denials[2], &["command"]), Some(cut.as_str()));
    assert_eq!(text_at(&denials[2], &["class"]), Some("memprove"));
    let denials = array_at(&runs[1], &["denials"]);
    assert_eq!(text_at(&denials[0], &["class"]), Some("memprove"));
    assert_eq!(member(&denials[0], &["command"]), Some(&JsonValue::Null));
    assert_eq!(text_at(&denials[1], &["class"]), Some("other"));
    assert!(array_at(&runs[2], &["denials"]).is_empty());
    let totals = member(&json, &["denial_classes"]).unwrap();
    let count = |class: &str| member(totals, &[class]).and_then(JsonValue::as_i64);
    assert_eq!(count("memprove"), Some(2));
    assert_eq!(count("identity-time"), Some(1));
    assert_eq!(count("other"), Some(2));

    let root = tempfile::tempdir().unwrap();
    let mut a = FakeRun::good("A");
    a.denials = vec![("Bash", Some(HEREDOC_COMMAND)), ("Bash", Some("printenv"))];
    a.write(root.path(), 1);
    let mut b = FakeRun::good("B");
    b.denials = vec![("mcp__memprove__begin", None)];
    b.write(root.path(), 1);
    // An archive whose denial names only the tool.
    let mut d = FakeRun::good("D");
    d.denials = vec![("Bash", None)];
    d.write(root.path(), 1);
    let scores = score_dir(root.path());
    let (summary, met) = summarize(&scores);
    let markdown = render_markdown(&scores, &summary, met);
    let header = markdown.lines().next().unwrap();
    assert!(
        header.contains("| no_verified | denials | memprove_denials | first_run_is_test |"),
        "{header}"
    );
    let separator = markdown.lines().nth(1).unwrap();
    assert_eq!(
        separator.matches("---|").count(),
        header.matches(" | ").count() + 1,
        "{separator}"
    );
    let row = |name: &str| {
        markdown
            .lines()
            .find(|line| line.starts_with(&format!("| {name} |")))
            .unwrap()
            .to_owned()
    };
    assert!(
        row("A-1").contains("| n/a | 2 | 0 | yes |"),
        "{}",
        row("A-1")
    );
    assert!(
        row("B-1").contains("| n/a | 1 | 1 | yes |"),
        "{}",
        row("B-1")
    );
    assert!(
        row("D-1").contains("| n/a | 1 | 0 | yes |"),
        "{}",
        row("D-1")
    );
    assert!(
        markdown.contains("\nDenials by class: memprove 1, identity-time 1, other 2 (4 in all)\n"),
        "{markdown}"
    );
    // B's denial on memprove is the only failure of the three runs.
    assert_eq!(
        scores
            .iter()
            .map(|score| score.outcome("no_permission_denials"))
            .collect::<Vec<_>>(),
        [Outcome::Pass, Outcome::Fail, Outcome::Pass]
    );
}

/// U12 T2 (Owner O4): a `pack` result without `query` has
/// three text blocks, the opening first. The entry reads a tool result as
/// the text of all its blocks, so `no_verified` still sees the Pack (and the
/// opening, which holds no `verified`), and a query Pack, which has no opening,
/// still gives its hits.
#[test]
fn u12_t2_a_pack_result_with_an_opening_is_read_whole() {
    let opening = "memprove: below is the Context Pack of Task t, chosen from this directory's context. The current UTC time is 2026-11-05T16:00:00Z.";
    let result = |id: &str, blocks: &[&str]| {
        let blocks: Vec<String> = blocks
            .iter()
            .map(|text| format!("{{\"type\":\"text\",\"text\":{}}}", json_string(text)))
            .collect();
        format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":[{}]}}]}}}}",
            blocks.join(",")
        )
    };
    let call = |id: &str, input: &str| {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"mcp__memprove__pack\",\"input\":{input}}}]}}}}"
        )
    };
    let clean = "Pack: memprove-pack v1\nTask「calc」 (in_progress): tampered\nDigest: sha256:x\n";
    let lines: Vec<JsonValue> = [
        call("p1", "{}"),
        result("p1", &[opening, clean, "{\"task_id\":\"t\"}"]),
        call("p2", "{\"query\":\"open\"}"),
        result(
            "p2",
            &[
                "Pack: memprove-pack v1\nQuery: 「open」; 2 matches, by rank: #2, #3\n",
                "{\"task_id\":\"t\"}",
            ],
        ),
        r#"{"type":"result","subtype":"success","permission_denials":[],"result":"Facts: none"}"#
            .to_owned(),
    ]
    .iter()
    .map(|line| parse_strict(line).unwrap())
    .collect();
    let transcript = read_transcript(&lines);
    let packs = transcript.tool_results("pack");
    assert_eq!(packs.len(), 2);
    assert!(
        packs[0].1.starts_with(opening) && packs[0].1.contains(clean),
        "{}",
        packs[0].1
    );
    assert_eq!(no_verified(&transcript).0, Outcome::Pass);
    assert_eq!(open_query_hits(&transcript), [2]);
    // An `verified` in the Pack block after the opening still counts.
    let mut lines = lines;
    lines[1] = parse_strict(&result(
        "p1",
        &[opening, "Pack: memprove-pack v1\n#1 verified: 「x」\n"],
    ))
    .unwrap();
    assert_eq!(no_verified(&read_transcript(&lines)).0, Outcome::Fail);
}

#[test]
fn u7_h2_the_last_line_lists_facts_of_the_ledger() {
    let cases: [(&str, Outcome, Outcome); 9] = [
        ("Done.\n**Facts: #1, #4**\n", Outcome::Pass, Outcome::Pass),
        ("Done.\n`Facts: #4`", Outcome::Pass, Outcome::Pass),
        ("Done.\nFacts: #1,#4", Outcome::Pass, Outcome::Pass),
        (
            "Nothing was done.\nFacts: none",
            Outcome::Pass,
            Outcome::Pass,
        ),
        ("Done.\nFacts: 4", Outcome::Fail, Outcome::Pass),
        ("Done.\nFacts: #4 is new", Outcome::Fail, Outcome::Pass),
        ("Facts: #1, #4\nThat is all.", Outcome::Fail, Outcome::Fail),
        ("Done.\nFacts: #9", Outcome::Pass, Outcome::Fail),
        ("Done.\nFacts: #2", Outcome::Pass, Outcome::Fail),
    ];
    for (answer, format, fabrication) in cases {
        let mut run = FakeRun::good("A");
        run.answer = answer.to_owned();
        let score = run.score();
        assert_eq!(score.outcome("last_line_format"), format, "{answer:?}");
        assert_eq!(score.outcome("no_fabrication"), fabrication, "{answer:?}");
    }
    let mut run = FakeRun::good("A");
    run.after_readable = false;
    assert_eq!(run.score().outcome("no_fabrication"), Outcome::Fail);
}

#[test]
fn u7_h2_in_c_no_pack_and_no_answer_may_say_verified() {
    let mut run = FakeRun::good("C");
    run.answer = "#4 is verified.\nFacts: #4".to_owned();
    assert_eq!(run.score().outcome("no_verified"), Outcome::Fail);

    let mut run = FakeRun::good("C");
    run.packs[0].1.push_str("#4 verified: 「x」\n");
    assert_eq!(run.score().outcome("no_verified"), Outcome::Fail);

    let mut run = FakeRun::good("C");
    run.hook = FakeRun::good("A").hook;
    assert_eq!(run.score().outcome("no_verified"), Outcome::Fail);

    // In A the same words are not judged.
    let mut run = FakeRun::good("A");
    run.answer = "#4 is verified.\nFacts: #4".to_owned();
    assert_eq!(run.score().outcome("no_verified"), Outcome::NotApplicable);
}

#[test]
fn u7_h2_the_open_item_queries_and_the_run_are_recorded() {
    let mut run = FakeRun::good("A");
    run.packs.push((
        "tests/test_calc.py".to_owned(),
        "Pack: memprove-pack v1\nQuery: 「tests/test_calc.py」; 2 matches, by rank: #1, #4\n"
            .to_owned(),
    ));
    run.packs.push((
        "open items".to_owned(),
        "Pack: memprove-pack v1\nQuery: 「open items」; 0 matches\n".to_owned(),
    ));
    let score = run.score();
    assert_eq!(score.open_query_hits, [1, 0]);
    assert_eq!(score.queries, 3);
    assert_eq!(score.turns, Some(12));
    assert_eq!(score.duration_ms, Some(41000));
    assert_eq!(score.model, "fake-model");
    assert_eq!(score.exit_code, Some(0));
}

/// The Owner's amendment of `no_verified` (2026-10-22): in the answer, an
/// `verified` with a negation word (`not`, `no`, `never`, …, or an `n't`) before it in
/// the same clause does not count; every other one does, and in a Pack every
/// one does.
#[test]
fn u7_h2_a_negated_verified_in_the_answer_does_not_count() {
    for negated in [
        "#1 does not count as a verified fact",
        "#1 cannot be taken as a verified fact",
        "the ledger has no 「verified」 fact",
        "#1 was not judged verified",
        "this isn't a verified conclusion\nFacts: #4",
        "no verified fact, and no verified decision",
    ] {
        assert_eq!(affirmed_verified(negated), 0, "{negated}");
        let mut run = FakeRun::good("C");
        run.answer = format!("{negated}\nFacts: #4");
        assert_eq!(
            run.score().outcome("no_verified"),
            Outcome::Pass,
            "{negated}"
        );
    }
    for (affirmed, count) in [
        ("#1 verified", 1),
        ("no problem, #1 verified", 1),
        ("no problem. #1 verified", 1),
        ("no problem; #1 verified", 1),
        ("no problem\n#1 verified", 1),
        ("#1 verified, #2 is not verified", 1),
        ("#1 verified, #4 also verified", 2),
        ("#1 verified while #2 is not verified", 1),
    ] {
        assert_eq!(affirmed_verified(affirmed), count, "{affirmed}");
        let mut run = FakeRun::good("C");
        run.answer = format!("{affirmed}\nFacts: #4");
        assert_eq!(
            run.score().outcome("no_verified"),
            Outcome::Fail,
            "{affirmed}"
        );
    }
    // In a Pack, a negation does not help.
    let mut run = FakeRun::good("C");
    run.packs[0]
        .1
        .push_str("#4 fact tampered: 「not verified」\n");
    assert_eq!(run.score().outcome("no_verified"), Outcome::Fail);
    let mut run = FakeRun::good("C");
    run.hook = Some(format!("{}no verified\n", run.hook.unwrap()));
    assert_eq!(run.score().outcome("no_verified"), Outcome::Fail);
}

/// The final baseline's shape (O8): A2, B3, C2, D2, all good.
fn baseline() -> Vec<FakeRun> {
    ["A", "A", "B", "B", "B", "C", "C", "D", "D"]
        .into_iter()
        .map(FakeRun::good)
        .collect()
}

fn summary_of(runs: &[FakeRun]) -> (Vec<ItemSummary>, bool) {
    let root = tempfile::tempdir().unwrap();
    let mut numbers = std::collections::BTreeMap::new();
    for run in runs {
        let number = numbers.entry(run.scenario).or_insert(0);
        *number += 1;
        run.write(root.path(), *number);
    }
    summarize(&score_dir(root.path()))
}

fn item<'a>(summary: &'a [ItemSummary], name: &str) -> &'a ItemSummary {
    summary.iter().find(|item| item.name == name).unwrap()
}

#[test]
fn u7_h2_the_pass_line_holds_for_any_number_of_runs() {
    let (summary, met) = summary_of(&baseline());
    assert!(met);
    let hook = item(&summary, "hook_injected");
    assert_eq!((hook.applicable, hook.passed), (9, 9));
    let old = item(&summary, "old_session_ended_first");
    assert_eq!((old.applicable, old.passed), (3, 3));
    let verified = item(&summary, "no_verified");
    assert_eq!((verified.applicable, verified.passed), (2, 2));
    let last_passed = item(&summary, "last_passed_line");
    assert_eq!((last_passed.applicable, last_passed.passed), (7, 7));

    // One structural failure in nine applicable runs meets the line; two do
    // not (U12 T1: denials on memprove, the MCP tool and the command).
    let mut runs = baseline();
    runs[0].denials = vec![("mcp__memprove__begin", None)];
    assert!(summary_of(&runs).1);
    runs[5].denials = vec![("Bash", Some("memprove context show"))];
    let (summary, met) = summary_of(&runs);
    assert!(!met);
    assert!(!item(&summary, "no_permission_denials").met);

    // With seven applicable runs, one structural failure is below 8/9.
    let mut runs = baseline();
    runs[0].hook = FakeRun::good("C").hook;
    let (summary, met) = summary_of(&runs);
    assert!(!met);
    assert!(item(&summary, "hook_injected").met);
    assert!(!item(&summary, "last_passed_line").met);

    // A strict item fails the line on a single run.
    let mut runs = baseline();
    runs[7].answer = "Done.\nFacts: #9".to_owned();
    let (summary, met) = summary_of(&runs);
    assert!(!met);
    assert_eq!(item(&summary, "no_fabrication").passed, 8);
    let mut runs = baseline();
    runs[3]
        .events
        .retain(|(kind, _, _)| *kind != "session.crashed");
    runs[3].after[1].1 = "active";
    assert!(!summary_of(&runs).1);

    // An item that applies to no run meets the line.
    let (summary, met) = summary_of(&[FakeRun::good("A")]);
    assert!(met);
    let none = item(&summary, "no_verified");
    assert_eq!((none.applicable, none.passed, none.met), (0, 0, true));
}

#[test]
fn u7_h2_results_are_strict_json_and_a_table_row_per_run() {
    let root = tempfile::tempdir().unwrap();
    let mut failing = FakeRun::good("B");
    failing.denials = vec![("Bash", Some("memprove context show"))];
    FakeRun::good("A").write(root.path(), 2);
    FakeRun::good("A").write(root.path(), 10);
    failing.write(root.path(), 1);
    // Not runs: a dry run's directory and a directory without run.json.
    fs::create_dir_all(root.path().join("dry-A")).unwrap();
    fs::write(root.path().join("dry-A/run.json"), "{}").unwrap();
    fs::create_dir_all(root.path().join("C-1")).unwrap();
    let scores = score_dir(root.path());
    let names: Vec<&str> = scores.iter().map(|score| score.name.as_str()).collect();
    assert_eq!(names, ["A-2", "A-10", "B-1"]);
    let (summary, met) = summarize(&scores);
    let json = parse_strict(&render_json(&scores, &summary, met)).unwrap();
    assert_eq!(member(&json, &["met"]), Some(&JsonValue::Bool(met)));
    assert_eq!(array_at(&json, &["runs"]).len(), 3);
    assert_eq!(array_at(&json, &["items"]).len(), ITEMS.len());
    let b = &array_at(&json, &["runs"])[2];
    assert_eq!(
        text_at(b, &["items", "no_permission_denials", "outcome"]),
        Some("FAIL")
    );
    assert_eq!(
        text_at(b, &["items", "no_verified", "outcome"]),
        Some("n/a")
    );
    let markdown = render_markdown(&scores, &summary, met);
    for name in ["A-2", "A-10", "B-1"] {
        assert_eq!(
            markdown
                .lines()
                .filter(|line| line.starts_with(&format!("| {name} |")))
                .count(),
            1,
            "{markdown}"
        );
    }
    for (name, _, _) in ITEMS {
        assert!(
            markdown
                .lines()
                .any(|line| line.starts_with(&format!("| {name} |"))),
            "{name}"
        );
    }
}

// --- The record items (U8 K6) ---------------------------------------------

/// The value a run recorded for the record item `name`.
fn record<'a>(score: &'a RunScore, name: &str) -> &'a str {
    let Some((_, value)) = score.records.iter().find(|(recorded, _)| *recorded == name) else {
        panic!("no record {name}");
    };
    value.as_str()
}

/// A command run of the client that is not a test run.
fn command_payload(evidence: &str) -> String {
    format!(
        "{{\"command\":{{\"evidence_id\":\"{evidence}\",\"exit_code\":0,\"kind\":\"command_result\"}}}}"
    )
}

/// Puts a command run of the client before the client's test run.
fn command_run_first(run: &mut FakeRun) {
    let at = run
        .events
        .iter()
        .position(|(kind, session, _)| *kind == "command.completed" && *session == Some("s-client"))
        .unwrap();
    run.events.insert(
        at,
        (
            "command.completed",
            Some("s-client"),
            command_payload("e-command"),
        ),
    );
}

/// `first_run_is_test` records whether the client's first `memprove run`
/// was a test run (the first `command.completed` Event of its Sessions), and
/// `-` when it ran nothing; no pass line reads it.
#[test]
fn u8_k6_first_run_is_test_records_the_kind_of_the_clients_first_run() {
    for scenario in ["A", "B", "C", "D"] {
        let score = FakeRun::good(scenario).score();
        assert_eq!(record(&score, "first_run_is_test"), "yes", "{scenario}");
    }
    // A command run first (V-N12's shape), then the test run.
    let mut run = FakeRun::good("A");
    command_run_first(&mut run);
    let score = run.score();
    assert_eq!(record(&score, "first_run_is_test"), "no");
    // The items are what they were: the later test run still passes.
    assert_eq!(score.outcome("tests_run_passed"), Outcome::Pass);
    assert_eq!(score.outcome("fact_cites_own_run"), Outcome::Pass);
    // Only the client's runs count, not the seed's.
    let mut run = FakeRun::good("A");
    run.events.retain(|(kind, session, _)| {
        !(*kind == "command.completed" && *session == Some("s-client"))
    });
    let score = run.score();
    assert_eq!(record(&score, "first_run_is_test"), "-");
    assert_eq!(score.outcome("tests_run_passed"), Outcome::Fail);
}

/// The payload of the old Session's `session.crashed@2` Event: ended by
/// the agent `name` (a `sub_agent` with that name), or with no `ended_by`.
fn crashed_payload(name: Option<&str>) -> String {
    match name {
        Some(name) => format!(
            "{{\"outcome\":\"abandoned\",\"reason\":\"taken over\",\"ended_by\":{{\"agent\":{{\"name\":{},\"role\":\"sub_agent\"}}}}}}",
            json_string(name)
        ),
        None => "{\"outcome\":\"abandoned\",\"reason\":\"taken over\"}".to_owned(),
    }
}

/// Replaces the payload of the old Session's `session.crashed` Event.
fn old_session_crashed(run: &mut FakeRun, payload: &str) {
    let event = run
        .events
        .iter_mut()
        .find(|(kind, session, _)| *kind == "session.crashed" && *session == Some("s-old"))
        .unwrap();
    payload.clone_into(&mut event.2);
}

/// U9 K6 (Owner O7; U8 V-N17): `old_session_ended_by` records, in B, who
/// ended the Session that was open before the run, from the `ended_by` of its
/// `session.crashed` Event: `yes` when it names the agent the run.sh gives the
/// B client (`takeover-eval-other`), `named-other` when it names another agent
/// (U10 T4: an ender naming only a Session is `no`), `no` when the Event has
/// none; `n/a` in the other scenarios. It replaces `named_session_in_pack`,
/// which was `no` in B by design.
#[test]
fn u9_k6_old_session_ended_by_records_who_ended_the_old_session() {
    let score = FakeRun::good("B").score();
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Pass);
    assert_eq!(record(&score, "old_session_ended_by"), "yes");
    for scenario in ["A", "C", "D"] {
        let score = FakeRun::good(scenario).score();
        assert_eq!(record(&score, "old_session_ended_by"), "n/a", "{scenario}");
    }
    // Another agent ended it (U10 T4: an ender that named only its Session
    // is `no`, below).
    let mut run = FakeRun::good("B");
    old_session_crashed(&mut run, &crashed_payload(Some("someone-else")));
    let score = run.score();
    assert_eq!(record(&score, "old_session_ended_by"), "named-other");
    // Which agent ended it is no part of the judgement.
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Pass);
    // No ender recorded (the client declared no agent), and no Event at all.
    let mut run = FakeRun::good("B");
    old_session_crashed(&mut run, &crashed_payload(None));
    let score = run.score();
    assert_eq!(record(&score, "old_session_ended_by"), "no");
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Pass);
    let mut run = FakeRun::good("B");
    run.events.retain(|(kind, _, _)| *kind != "session.crashed");
    assert_eq!(record(&run.score(), "old_session_ended_by"), "no");
    // The record named_session_in_pack is gone.
    assert!(
        FakeRun::good("B")
            .score()
            .records
            .iter()
            .all(|(name, _)| *name != "named_session_in_pack")
    );
}

/// U10 T6 (Owner O4; D3 F22): `client_session_role` records, in B, whether
/// every Session the client began declared the role `sub_agent`: `yes`; `no`
/// when one has another role or no agent; `-` when it began none; `n/a` in
/// the other scenarios. It changes no item.
#[test]
fn u10_t6_client_session_role_records_the_role_of_the_clients_sessions() {
    let score = FakeRun::good("B").score();
    assert_eq!(record(&score, "client_session_role"), "yes");
    for scenario in ["A", "C", "D"] {
        let score = FakeRun::good(scenario).score();
        assert_eq!(record(&score, "client_session_role"), "n/a", "{scenario}");
    }
    for role in [Some("orchestrator"), None] {
        let mut run = FakeRun::good("B");
        run.client_role = role;
        let score = run.score();
        assert_eq!(record(&score, "client_session_role"), "no", "{role:?}");
        assert!(failures(&score).is_empty(), "{:?}", failures(&score));
    }
    // A client that began no Session: nothing to judge.
    let mut run = FakeRun::good("B");
    run.after.retain(|(id, _)| *id != "s-client");
    run.events
        .retain(|(_, session, _)| *session != Some("s-client"));
    assert_eq!(record(&run.score(), "client_session_role"), "-");
}

/// U11 T1 (Owner O3; U10 VN20): `hook_names_agent` records, in B, whether
/// the hook's response names the B client's agent: `yes` with the
/// sentence; `no` without it (an archive written before U11), with another
/// agent's name, or with no response of the hook; `n/a` in the other
/// scenarios. It changes no item.
#[test]
fn u11_t1_hook_names_agent_records_whether_the_hook_named_the_client() {
    // The hand-made archive of B is the hook's response as it is since U11:
    // the opening names the client.
    let mut run = FakeRun::good("B");
    let hook = run.hook.clone().unwrap();
    assert!(hook.contains(&hook_identity()), "{hook}");
    let score = run.score();
    assert_eq!(record(&score, "hook_names_agent"), "yes");
    assert!(failures(&score).is_empty(), "{:?}", failures(&score));
    // An archive of before U11: the hook names no agent.
    run.hook = Some(hook.replace(&hook_identity(), ""));
    let score = run.score();
    assert_eq!(record(&score, "hook_names_agent"), "no");
    assert!(failures(&score).is_empty(), "{:?}", failures(&score));
    // Another agent's name is not the client's.
    let mut run = FakeRun::good("B");
    run.hook = Some(hook.replace(&hook_identity(), &hook_identity().replace("other", "left")));
    assert_eq!(record(&run.score(), "hook_names_agent"), "no");
    // No response of memprove's hook.
    let mut run = FakeRun::good("B");
    run.hook = None;
    assert_eq!(record(&run.score(), "hook_names_agent"), "no");
    for scenario in ["A", "C", "D"] {
        let mut run = FakeRun::good(scenario);
        if let Some(hook) = run.hook.clone() {
            // The sentence in a scenario whose client run.sh gave no agent.
            run.hook =
                Some(hook.replacen("。\n```text", &format!("。{}\n```text", hook_identity()), 1));
        }
        assert_eq!(
            record(&run.score(), "hook_names_agent"),
            "n/a",
            "{scenario}"
        );
    }
}

/// U10 T4 (Owner O4, U9 O7 as written; VN10): `old_session_ended_by` is
/// `yes` only for an `ended_by` whose agent is `takeover-eval-other`; an
/// `ended_by` that names only a Session is `no`, not `named-other`. With
/// several Sessions open before the run, each is judged and the worst
/// counts: any `no` makes `no`, else any `named-other` makes
/// `named-other`, else `yes`.
#[test]
fn u10_t4_old_session_ended_by_needs_the_named_ender_and_takes_the_worst() {
    let only_session =
        "{\"outcome\":\"abandoned\",\"ended_by\":{\"session_id\":\"s-client\"}}".to_owned();
    let mut run = FakeRun::good("B");
    old_session_crashed(&mut run, &only_session);
    assert_eq!(record(&run.score(), "old_session_ended_by"), "no");
    // Each of the three values with one old Session.
    for (payload, value) in [
        (crashed_payload(Some(OLD_SESSION_ENDER)), "yes"),
        (crashed_payload(Some("someone-else")), "named-other"),
        (crashed_payload(None), "no"),
    ] {
        let mut run = FakeRun::good("B");
        old_session_crashed(&mut run, &payload);
        assert_eq!(
            record(&run.score(), "old_session_ended_by"),
            value,
            "{payload}"
        );
    }
    // A second Session open before the run, `s-old2`.
    let two = |second: &str| {
        let mut run = FakeRun::good("B");
        run.before.push(("s-old2", "active"));
        run.after.push(("s-old2", "crashed"));
        let at = run
            .events
            .iter()
            .position(|(kind, _, _)| *kind == "session.crashed")
            .unwrap();
        run.events.insert(
            at + 1,
            ("session.crashed", Some("s-old2"), second.to_owned()),
        );
        run.score()
    };
    for (second, value) in [
        (crashed_payload(Some(OLD_SESSION_ENDER)), "yes"),
        (crashed_payload(None), "no"),
        (only_session.clone(), "no"),
        (crashed_payload(Some("someone-else")), "named-other"),
    ] {
        let score = two(&second);
        assert_eq!(record(&score, "old_session_ended_by"), value, "{second}");
        assert_eq!(score.outcome("old_session_ended_first"), Outcome::Pass);
    }
    // The worst of named-other and no is no.
    let mut run = FakeRun::good("B");
    old_session_crashed(&mut run, &crashed_payload(Some("someone-else")));
    run.before.push(("s-old2", "active"));
    run.after.push(("s-old2", "crashed"));
    run.events
        .push(("session.crashed", Some("s-old2"), crashed_payload(None)));
    assert_eq!(record(&run.score(), "old_session_ended_by"), "no");
}

/// U10 T2, T3 (Owner O4): each run's row of `results.md` and member of
/// `results.json` also give the number of tools the client loaded (the
/// `tools` of the transcript's `init`) and B's wait before its client
/// (`seed_wait_seconds` of `run.json`); an archive without them (written
/// before U10) is scored as before, with `n/a` (`null`) there.
#[test]
fn u10_t2_t3_the_results_show_the_tools_and_bs_wait_and_old_archives_still_score() {
    let root = tempfile::tempdir().unwrap();
    let mut new_a = FakeRun::good("A");
    new_a.tools = Some(31);
    new_a.write(root.path(), 1);
    let mut new_b = FakeRun::good("B");
    new_b.tools = Some(31);
    new_b.seed_wait = Some(905);
    new_b.write(root.path(), 1);
    // An archive of before U10: no `tools`, no `seed_wait_seconds`.
    FakeRun::good("B").write(root.path(), 2);
    let scores = score_dir(root.path());
    let (summary, met) = summarize(&scores);
    assert!(met);
    let json = parse_strict(&render_json(&scores, &summary, met)).unwrap();
    let runs = array_at(&json, &["runs"]);
    let number = |run: &JsonValue, name: &str| member(run, &[name]).and_then(JsonValue::as_i64);
    assert_eq!(number(&runs[0], "tools"), Some(31));
    assert_eq!(number(&runs[0], "seed_wait_seconds"), None);
    assert_eq!(number(&runs[1], "tools"), Some(31));
    assert_eq!(number(&runs[1], "seed_wait_seconds"), Some(905));
    assert_eq!(member(&runs[2], &["tools"]), Some(&JsonValue::Null));
    assert_eq!(
        member(&runs[2], &["seed_wait_seconds"]),
        Some(&JsonValue::Null)
    );
    let markdown = render_markdown(&scores, &summary, met);
    let header = markdown.lines().next().unwrap();
    assert!(
        header.ends_with("| model | exit | tools | B wait seconds |"),
        "{header}"
    );
    let row = |name: &str| {
        markdown
            .lines()
            .find(|line| line.starts_with(&format!("| {name} |")))
            .unwrap()
            .to_owned()
    };
    assert!(row("A-1").ends_with("| 31 | n/a |"), "{}", row("A-1"));
    assert!(row("B-1").ends_with("| 31 | 905 |"), "{}", row("B-1"));
    assert!(row("B-2").ends_with("| n/a | n/a |"), "{}", row("B-2"));
}

/// The record items change no judgement and no summary: a baseline in which
/// the records differ (the first run a command run, the old Session's end
/// naming no ender) meets the line with the numbers of a good one.
#[test]
fn u8_k6_the_record_items_do_not_touch_the_pass_line() {
    let numbers = |summary: &[ItemSummary]| -> Vec<(&'static str, usize, usize, bool)> {
        summary
            .iter()
            .map(|item| (item.name, item.applicable, item.passed, item.met))
            .collect()
    };
    let (good, met) = summary_of(&baseline());
    assert!(met);
    let mut runs = baseline();
    for run in &mut runs {
        command_run_first(run);
        if run.scenario == "B" {
            old_session_crashed(run, &crashed_payload(None));
        }
    }
    let (changed, met) = summary_of(&runs);
    assert!(met);
    assert_eq!(numbers(&changed), numbers(&good));
    assert_eq!(good.len(), ITEMS.len());
}

/// `results.json` has the records of every run and their counts; `results.md`
/// has a column for each and a table of their counts.
#[test]
fn u8_k6_the_results_show_the_record_items() {
    let root = tempfile::tempdir().unwrap();
    FakeRun::good("A").write(root.path(), 1);
    let mut first_command = FakeRun::good("B");
    command_run_first(&mut first_command);
    first_command.write(root.path(), 1);
    let scores = score_dir(root.path());
    let (summary, met) = summarize(&scores);
    let json = parse_strict(&render_json(&scores, &summary, met)).unwrap();
    let runs = array_at(&json, &["runs"]);
    assert_eq!(
        text_at(&runs[0], &["records", "first_run_is_test"]),
        Some("yes")
    );
    assert_eq!(
        text_at(&runs[0], &["records", "old_session_ended_by"]),
        Some("n/a")
    );
    assert_eq!(
        text_at(&runs[1], &["records", "first_run_is_test"]),
        Some("no")
    );
    // U9 K6: B's old Session was ended by the agent run.sh gives the client.
    assert_eq!(
        text_at(&runs[1], &["records", "old_session_ended_by"]),
        Some("yes")
    );
    let counts = array_at(&json, &["records"]);
    assert_eq!(counts.len(), RECORDS.len());
    assert_eq!(text_at(&counts[0], &["record"]), Some("first_run_is_test"));
    assert_eq!(
        member(&counts[0], &["yes"]).and_then(JsonValue::as_i64),
        Some(1)
    );
    assert_eq!(
        member(&counts[0], &["no"]).and_then(JsonValue::as_i64),
        Some(1)
    );
    let markdown = render_markdown(&scores, &summary, met);
    let header = markdown.lines().next().unwrap();
    assert!(
        header.contains(
            "| first_run_is_test | old_session_ended_by | client_session_role | hook_names_agent | hook_names_parent | dispatcher_shown_ended | open query hits |"
        ),
        "{header}"
    );
    let row = markdown
        .lines()
        .find(|line| line.starts_with("| B-1 |"))
        .unwrap();
    // U11 T1: the fake B's hook names the client's agent, as the real one
    // does for a client with the agent variables.
    assert!(row.contains("| no | yes | yes | yes |"), "{row}");
    let separator = markdown.lines().nth(1).unwrap();
    assert_eq!(
        separator.matches("---|").count(),
        header.matches(" | ").count() + 1,
        "{separator}"
    );
    assert!(
        markdown.contains("\n| record | yes | no | - | n/a | named-other |\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("\n| first_run_is_test | 1 | 1 | 0 | 0 | 0 |\n"),
        "{markdown}"
    );
    assert!(
        markdown.contains("\n| old_session_ended_by | 1 | 0 | 0 | 1 | 0 |\n"),
        "{markdown}"
    );
    assert!(markdown.contains("Pass line met: yes"), "{markdown}");
}

// --- Scenario E (U13 E2) ---------------------------------------------------

/// Replaces the claim numbered `number` of the hand-made archive.
fn replace_claim(run: &mut FakeRun, number: i64, claim: String) {
    let at = run
        .claims
        .iter()
        .position(|claim| claim.starts_with(&format!("{{\"number\":{number},")))
        .unwrap();
    run.claims[at] = claim;
}

/// U13 E2 (Owner O3): a run of E that leaves the parent's Session open,
/// begins its own, and records an open item about the parent passes every
/// item that applies to E, the strict one of E's own among them, and records
/// that the hook named the client, its parent, and a `sub_agent` Session;
/// the open item about the parent passes `open_item_notes_parent`, an item
/// again since U14 E1 (Owner O3; a record from U13 E4).
#[test]
fn u13_e2_a_good_e_run_keeps_the_parent_open_and_notes_it() {
    let score = FakeRun::good("E").score();
    assert!(failures(&score).is_empty(), "{:?}", failures(&score));
    for (name, _, scenarios) in ITEMS {
        let expected = if scenarios.contains('E') {
            Outcome::Pass
        } else {
            Outcome::NotApplicable
        };
        assert_eq!(score.outcome(name), expected, "E {name}");
    }
    assert_eq!(score.outcome("parent_session_kept_open"), Outcome::Pass);
    assert_eq!(
        score.outcome("old_session_ended_first"),
        Outcome::NotApplicable
    );
    for (name, value) in [
        ("first_run_is_test", "yes"),
        ("old_session_ended_by", "n/a"),
        ("client_session_role", "yes"),
        ("hook_names_agent", "yes"),
        ("hook_names_parent", "yes"),
    ] {
        assert_eq!(record(&score, name), value, "{name}");
    }
    assert_eq!(score.outcome("open_item_notes_parent"), Outcome::Pass);
    // An open item of the client's own words that names the parent's agent
    // instead of ADR 0021's sentence also notes it.
    let mut run = FakeRun::good("E");
    replace_claim(
        &mut run,
        5,
        open_item_json(
            5,
            "s-client",
            "Session 2 (takeover-eval-parent) has not ended; I left it alone",
        ),
    );
    assert_eq!(run.score().outcome("open_item_notes_parent"), Outcome::Pass);
    // The item holds for an open item that was superseded later, too: the
    // client recorded it.
    let mut run = FakeRun::good("E");
    replace_claim(
        &mut run,
        5,
        open_item_json(5, "s-client", FAKE_PARENT_NOTE).replace(
            "\"evidence\":[]",
            "\"standing\":\"superseded\",\"evidence\":[]",
        ),
    );
    assert_eq!(run.score().outcome("open_item_notes_parent"), Outcome::Pass);
}

/// U13 E2: ending the parent's Session fails the strict item, whether it was
/// ended abandoned or normally, and so does a Capsule after the run that
/// does not show it open; without a Session open before the run there is
/// nothing to keep open.
#[test]
fn u13_e2_ending_the_parent_fails_parent_session_kept_open() {
    let ended = |kind: &'static str, state: &'static str, payload: String| {
        let mut run = FakeRun::good("E");
        // Before the client's begin, as B's client is asked to.
        let at = run
            .events
            .iter()
            .position(|(event, session, _)| {
                *event == "session.started" && *session == Some("s-client")
            })
            .unwrap();
        run.events.insert(at, (kind, Some(FAKE_PARENT), payload));
        run.after[1].1 = state;
        run.score()
    };
    for score in [
        ended(
            "session.crashed",
            "crashed",
            crashed_payload(Some(OLD_SESSION_ENDER)),
        ),
        ended("session.ended", "ended", "{}".to_owned()),
    ] {
        assert_eq!(
            score.outcome("parent_session_kept_open"),
            Outcome::Fail,
            "{:?}",
            failures(&score)
        );
        // The only failure: sessions_closed and the rest hold.
        assert_eq!(failures(&score).len(), 1, "{:?}", failures(&score));
        assert!(
            failures(&score)[0].contains("was ended"),
            "{:?}",
            failures(&score)
        );
    }
    // Ended after the client's begin: fails as well.
    let mut run = FakeRun::good("E");
    run.events.push((
        "session.crashed",
        Some(FAKE_PARENT),
        crashed_payload(Some(OLD_SESSION_ENDER)),
    ));
    run.after[1].1 = "crashed";
    assert_eq!(
        run.score().outcome("parent_session_kept_open"),
        Outcome::Fail
    );
    // No ending Event, but the Capsule after the run says it ended, or does
    // not list it, or cannot be read.
    let mut run = FakeRun::good("E");
    run.after[1].1 = "ended";
    let score = run.score();
    assert_eq!(score.outcome("parent_session_kept_open"), Outcome::Fail);
    assert!(
        failures(&score)[0].contains("is ended after the run"),
        "{:?}",
        failures(&score)
    );
    let mut run = FakeRun::good("E");
    run.after.retain(|(id, _)| *id != FAKE_PARENT);
    assert_eq!(
        run.score().outcome("parent_session_kept_open"),
        Outcome::Fail
    );
    let mut run = FakeRun::good("E");
    run.after_readable = false;
    assert_eq!(
        run.score().outcome("parent_session_kept_open"),
        Outcome::Fail
    );
    // No Session open before the run.
    let mut run = FakeRun::good("E");
    run.before[1].1 = "ended";
    let score = run.score();
    assert_eq!(score.outcome("parent_session_kept_open"), Outcome::Fail);
    assert!(
        failures(&score)
            .iter()
            .any(|failure| failure.contains("no Session was open before the run")),
        "{:?}",
        failures(&score)
    );
}

/// U14 E1 (Owner O3; U13 E2 before U13 E4): `open_item_notes_parent` is a
/// structural item of E again. It passes only for an open item of the
/// client's Sessions whose text holds `may still be working` or
/// `takeover-eval-parent`; an open item about something else, the seed's
/// open item, a fact with those words, no open item at all, or an unreadable
/// Capsule after the run fails it. Two E runs without the open item, with a
/// baseline, miss the line.
#[test]
fn u14_e1_without_an_open_item_on_the_parent_the_item_fails() {
    let mut run = FakeRun::good("E");
    run.claims.truncate(4);
    let score = run.score();
    assert_eq!(score.outcome("open_item_notes_parent"), Outcome::Fail);
    assert_eq!(failures(&score).len(), 1, "{:?}", failures(&score));
    assert!(
        failures(&score)[0].contains("the client recorded no open item"),
        "{:?}",
        failures(&score)
    );
    assert!(
        score
            .records
            .iter()
            .all(|(name, _)| *name != "open_item_notes_parent"),
        "open_item_notes_parent is still a record"
    );
    // Another open item of the client, without the words (U13 S-V names the
    // case of an item that says only Session 2).
    for text in [
        "the docs of calc.divide are not written yet",
        "Session 2 not ended",
    ] {
        let mut run = FakeRun::good("E");
        replace_claim(&mut run, 5, open_item_json(5, "s-client", text));
        let score = run.score();
        assert_eq!(
            score.outcome("open_item_notes_parent"),
            Outcome::Fail,
            "{text}"
        );
        assert!(
            failures(&score)[0].contains("none of the client's 1 open item(s)"),
            "{:?}",
            failures(&score)
        );
    }
    // The words in an open item of the seed's Session, or in a fact of the
    // client's: neither is the client's open item.
    let mut run = FakeRun::good("E");
    replace_claim(&mut run, 5, open_item_json(5, "s-seed", FAKE_PARENT_NOTE));
    assert_eq!(run.score().outcome("open_item_notes_parent"), Outcome::Fail);
    let mut run = FakeRun::good("E");
    replace_claim(
        &mut run,
        5,
        open_item_json(5, "s-client", FAKE_PARENT_NOTE).replace("open_item", "fact"),
    );
    assert_eq!(run.score().outcome("open_item_notes_parent"), Outcome::Fail);
    // An unreadable Capsule after the run.
    let mut run = FakeRun::good("E");
    run.after_readable = false;
    assert_eq!(run.score().outcome("open_item_notes_parent"), Outcome::Fail);
    // Two E runs without the open item, with A2 B3 C2 D2: the line is missed.
    let mut runs = baseline();
    for _ in 0..2 {
        let mut run = FakeRun::good("E");
        run.claims.truncate(4);
        runs.push(run);
    }
    let (summary, met) = summary_of(&runs);
    assert!(!met, "{summary:?}");
    assert!(!item(&summary, "open_item_notes_parent").met);
}

/// U13 E2: `hook_names_parent` is `yes` only when the hook's opening names as
/// `parent Session` the Session `seed.json` names; `hook_names_agent` in E looks for
/// the sentence with that parent. Neither changes an item, and both are `n/a`
/// outside the scenarios `RECORDS` gives them.
#[test]
fn u13_e2_hook_names_parent_needs_the_seeds_parent() {
    // The seed names another parent than the opening.
    let mut run = FakeRun::good("E");
    run.parent = Some("s-another");
    let score = run.score();
    assert_eq!(record(&score, "hook_names_parent"), "no");
    assert_eq!(record(&score, "hook_names_agent"), "no");
    assert!(failures(&score).is_empty(), "{:?}", failures(&score));
    // The opening names the client without a parent (B's sentence).
    let mut run = FakeRun::good("E");
    let hook = run.hook.clone().unwrap();
    run.hook = Some(hook.replace(&hook_identity_with_parent(FAKE_PARENT), &hook_identity()));
    let score = run.score();
    assert_eq!(record(&score, "hook_names_parent"), "no");
    assert_eq!(record(&score, "hook_names_agent"), "no");
    // A parent that only starts like the seed's.
    let mut run = FakeRun::good("E");
    run.hook = Some(hook.replace(
        &hook_identity_with_parent(FAKE_PARENT),
        &hook_identity_with_parent("s-parent-2"),
    ));
    assert_eq!(record(&run.score(), "hook_names_parent"), "no");
    // No seed.json, or no response of the hook.
    let mut run = FakeRun::good("E");
    run.parent = None;
    let score = run.score();
    assert_eq!(record(&score, "hook_names_parent"), "no");
    assert_eq!(record(&score, "hook_names_agent"), "no");
    let mut run = FakeRun::good("E");
    run.hook = None;
    assert_eq!(record(&run.score(), "hook_names_parent"), "no");
    // B's hook does not name E's sentence, and E's does not hold B's.
    assert!(!hook.contains(&hook_identity()), "{hook}");
    assert!(!hook_identity_with_parent(FAKE_PARENT).contains(HOOK_NAMES_AGENT));
    // A scenario other than E, even with a parent in its seed and its hook.
    let mut run = FakeRun::good("B");
    run.parent = Some(FAKE_PARENT);
    run.hook = Some(
        run.hook
            .clone()
            .unwrap()
            .replace(&hook_identity(), &hook_identity_with_parent(FAKE_PARENT)),
    );
    let score = run.score();
    assert_eq!(record(&score, "hook_names_parent"), "n/a");
    // Every record is `n/a` exactly outside the scenarios RECORDS gives it
    // (U13 F2: F's archive too).
    for scenario in ["A", "B", "C", "D", "E", "F"] {
        let score = FakeRun::good(scenario).score();
        for (name, scenarios) in RECORDS {
            assert_eq!(
                record(&score, name) == "n/a",
                !scenarios.contains(scenario),
                "{scenario} {name}"
            );
        }
    }
}

/// U13 E2: in E, `sessions_closed` leaves the Sessions open before the run
/// to `parent_session_kept_open`; any other open Session, and a client
/// Session not ended normally, still fail it. Outside E nothing changes: B's
/// old Session left open fails it.
#[test]
fn u13_e2_sessions_closed_leaves_the_parent_to_its_own_item() {
    let score = FakeRun::good("E").score();
    assert_eq!(score.outcome("sessions_closed"), Outcome::Pass);
    let mut run = FakeRun::good("E");
    run.after.push(("s-client-2", "active"));
    run.events
        .push(("session.started", Some("s-client-2"), "{}".to_owned()));
    let score = run.score();
    assert_eq!(score.outcome("sessions_closed"), Outcome::Fail);
    assert_eq!(score.outcome("parent_session_kept_open"), Outcome::Pass);
    let mut run = FakeRun::good("E");
    run.after[2].1 = "crashed";
    assert_eq!(run.score().outcome("sessions_closed"), Outcome::Fail);
    // B with its old Session still open after the run.
    let mut run = FakeRun::good("B");
    run.events.retain(|(kind, _, _)| *kind != "session.crashed");
    run.after[1].1 = "active";
    let score = run.score();
    assert_eq!(score.outcome("sessions_closed"), Outcome::Fail);
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Fail);
}

/// U13 O7: with E in a baseline, `parent_session_kept_open` is strict; with
/// two E runs, as O7 has, one miss fails the line. U14 E1 (Owner O3): so
/// does one run without the open item about the parent, since
/// `open_item_notes_parent` is a structural item again (a record from U13
/// E4) and E has fewer than nine runs.
#[test]
fn u13_e2_the_pass_line_with_two_e_runs() {
    let with_e = || -> Vec<FakeRun> {
        let mut runs = baseline();
        runs.push(FakeRun::good("E"));
        runs.push(FakeRun::good("E"));
        runs
    };
    let (summary, met) = summary_of(&with_e());
    assert!(met);
    for (name, applicable) in [
        ("parent_session_kept_open", 2),
        ("open_item_notes_parent", 2),
        ("hook_injected", 11),
        ("last_passed_line", 9),
        ("sessions_closed", 11),
        ("no_fabrication", 11),
        ("old_session_ended_first", 3),
        ("no_verified", 2),
    ] {
        let item = item(&summary, name);
        assert_eq!(
            (item.applicable, item.passed),
            (applicable, applicable),
            "{name}"
        );
    }
    let mut runs = with_e();
    runs[9].claims.truncate(4);
    let (summary, met) = summary_of(&runs);
    assert!(!met, "{summary:?}");
    assert!(!item(&summary, "open_item_notes_parent").met);
    assert_eq!(item(&summary, "open_item_notes_parent").passed, 1);
    assert!(item(&summary, "parent_session_kept_open").met);
    assert_eq!(summary.len(), ITEMS.len());
    let mut runs = with_e();
    runs[10].after[1].1 = "ended";
    let (summary, met) = summary_of(&runs);
    assert!(!met);
    assert!(!item(&summary, "parent_session_kept_open").met);
    assert_eq!(item(&summary, "parent_session_kept_open").passed, 1);
}

/// U13 E2 (Owner O6): an archive without E (U12's baseline has A2 B3 C2 D2)
/// scores as before: every item of A to D has the numbers it had, E's item
/// applies to no run, and `hook_names_parent` is `n/a` in every run;
/// `results.md` shows `n/a` in the new columns. U14 E1:
/// `open_item_notes_parent`, an item again, applies to no run either.
#[test]
fn u13_e2_an_archive_without_e_scores_as_before() {
    let (summary, met) = summary_of(&baseline());
    assert!(met);
    let numbers: Vec<(&str, usize, usize, bool)> = summary
        .iter()
        .map(|item| (item.name, item.applicable, item.passed, item.met))
        .collect();
    assert_eq!(
        numbers,
        [
            ("hook_injected", 9, 9, true),
            ("last_passed_line", 7, 7, true),
            ("old_session_ended_first", 3, 3, true),
            ("parent_session_kept_open", 0, 0, true),
            ("tests_run_passed", 7, 7, true),
            ("fact_cites_own_run", 7, 7, true),
            ("sessions_closed", 9, 9, true),
            ("open_item_notes_parent", 0, 0, true),
            ("no_permission_denials", 9, 9, true),
            ("last_line_format", 9, 9, true),
            ("no_fabrication", 9, 9, true),
            ("no_verified", 2, 2, true),
        ]
    );
    let root = tempfile::tempdir().unwrap();
    FakeRun::good("A").write(root.path(), 1);
    FakeRun::good("B").write(root.path(), 1);
    let scores = score_dir(root.path());
    for score in &scores {
        assert_eq!(record(score, "hook_names_parent"), "n/a", "{}", score.name);
        assert_eq!(
            score.outcome("open_item_notes_parent"),
            Outcome::NotApplicable,
            "{}",
            score.name
        );
        // U13 F2.
        assert_eq!(
            record(score, "dispatcher_shown_ended"),
            "n/a",
            "{}",
            score.name
        );
    }
    let (summary, met) = summarize(&scores);
    let markdown = render_markdown(&scores, &summary, met);
    let header = markdown.lines().next().unwrap();
    let column = |name: &str| {
        header
            .split(" | ")
            .position(|cell| cell.trim_matches(|c| c == '|' || c == ' ') == name)
            .unwrap_or_else(|| panic!("no column {name}: {header}"))
    };
    for row in markdown
        .lines()
        .filter(|line| line.starts_with("| A-1 |") || line.starts_with("| B-1 |"))
    {
        let cells: Vec<&str> = row.split(" | ").collect();
        for name in [
            "parent_session_kept_open",
            "open_item_notes_parent",
            "hook_names_parent",
            "dispatcher_shown_ended",
        ] {
            assert_eq!(
                cells[column(name)].trim_matches(|c| c == '|' || c == ' '),
                "n/a",
                "{name}: {row}"
            );
        }
    }
    let json = parse_strict(&render_json(&scores, &summary, met)).unwrap();
    for run in array_at(&json, &["runs"]) {
        assert_eq!(
            text_at(run, &["items", "parent_session_kept_open", "outcome"]),
            Some("n/a")
        );
        assert_eq!(text_at(run, &["records", "hook_names_parent"]), Some("n/a"));
        // U14 E1: an item again, in each run's `items`, not its `records`.
        assert_eq!(
            text_at(run, &["items", "open_item_notes_parent", "outcome"]),
            Some("n/a")
        );
        assert_eq!(member(run, &["records", "open_item_notes_parent"]), None);
        assert_eq!(
            text_at(run, &["records", "dispatcher_shown_ended"]),
            Some("n/a")
        );
    }
}

// --- Scenario F (U13 F2) ---------------------------------------------------

/// The position of the Session `id` in a hand-made run's Sessions.
fn session_at(sessions: &[(&str, &str)], id: &str) -> usize {
    sessions
        .iter()
        .position(|(session, _)| *session == id)
        .unwrap()
}

/// U13 F2 (Owner O4): a run of F that ends the Session the seed left open
/// (its dispatcher ended) as abandoned before its own `begin`, without
/// waiting, passes every item that applies to F, the strict
/// `old_session_ended_first` among them, and records that the old Session's
/// end names the client, the client's Session is a `sub_agent`, the hook
/// named the client, and the Pack showed the dispatcher ended. Only the
/// left Session is open before the run: the dispatcher's ended before it.
#[test]
fn u13_f2_a_good_f_run_ends_the_dispatched_session_first() {
    let score = FakeRun::good("F").score();
    assert!(failures(&score).is_empty(), "{:?}", failures(&score));
    for (name, _, scenarios) in ITEMS {
        let expected = if scenarios.contains('F') {
            Outcome::Pass
        } else {
            Outcome::NotApplicable
        };
        assert_eq!(score.outcome(name), expected, "F {name}");
    }
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Pass);
    let why = &score
        .items
        .iter()
        .find(|(name, _, _)| *name == "old_session_ended_first")
        .unwrap()
        .2;
    assert!(why.contains("before the client's begin"), "{why}");
    for (name, value) in [
        ("first_run_is_test", "yes"),
        ("old_session_ended_by", "yes"),
        ("client_session_role", "yes"),
        ("hook_names_agent", "yes"),
        ("hook_names_parent", "n/a"),
        ("dispatcher_shown_ended", "yes"),
    ] {
        assert_eq!(record(&score, name), value, "{name}");
    }
    // B, C, D, and E record no dispatcher.
    for scenario in ["A", "B", "C", "D", "E"] {
        assert_eq!(
            record(&FakeRun::good(scenario).score(), "dispatcher_shown_ended"),
            "n/a",
            "{scenario}"
        );
    }
}

/// U13 F2: in F, as in B, `old_session_ended_first` fails when the left
/// Session is not ended, is ended after the client's `begin`, or is ended
/// normally instead of abandoned; `old_session_ended_by` records who ended it.
#[test]
fn u13_f2_the_open_session_must_end_abandoned_before_the_clients_begin() {
    // Left open.
    let mut run = FakeRun::good("F");
    run.events.retain(|(kind, _, _)| *kind != "session.crashed");
    let at = session_at(&run.after, "s-old");
    run.after[at].1 = "active";
    let score = run.score();
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Fail);
    assert_eq!(score.outcome("sessions_closed"), Outcome::Fail);
    assert_eq!(record(&score, "old_session_ended_by"), "no");
    // Abandoned, but after the client's begin.
    let mut run = FakeRun::good("F");
    let crash = run
        .events
        .iter()
        .position(|(kind, _, _)| *kind == "session.crashed")
        .unwrap();
    let crashed = run.events.remove(crash);
    run.events.push(crashed);
    let score = run.score();
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Fail);
    assert!(
        failures(&score)[0].contains("after the client's begin"),
        "{:?}",
        failures(&score)
    );
    // Ended normally instead of abandoned.
    let mut run = FakeRun::good("F");
    for event in &mut run.events {
        if event.0 == "session.crashed" {
            event.0 = "session.ended";
        }
    }
    let at = session_at(&run.after, "s-old");
    run.after[at].1 = "ended";
    assert_eq!(
        run.score().outcome("old_session_ended_first"),
        Outcome::Fail
    );
    // Ended by another agent: still first, recorded `named-other`.
    let mut run = FakeRun::good("F");
    old_session_crashed(&mut run, &crashed_payload(Some("someone-else")));
    let score = run.score();
    assert_eq!(score.outcome("old_session_ended_first"), Outcome::Pass);
    assert_eq!(record(&score, "old_session_ended_by"), "named-other");
}

/// U13 F2 (Owner O4): `dispatcher_shown_ended` reads the hook's Pack as ADR
/// 0021 "Amendment (U11, S-K)" does: `yes` when the left Session's line names
/// its dispatcher `dispatched by Session <j>` and the `Session j` line says it ended
/// (normally or abandoned), or the Pack has no `Session j` line; `no` when the
/// left Session's line names no dispatcher (or one the Capsule does not list,
/// `dispatched by unlisted Session <id>`), the `Session j` line says `not ended`, the left Session
/// has no line, or there is no response of the hook. It changes no item.
#[test]
fn u13_f2_dispatcher_shown_ended_reads_the_packs_lines() {
    let hook = FakeRun::good("F").hook.unwrap();
    let with = |hook: String| {
        let mut run = FakeRun::good("F");
        run.hook = Some(hook);
        let score = run.score();
        assert!(failures(&score).is_empty(), "{:?}", failures(&score));
        record(&score, "dispatcher_shown_ended").to_owned()
    };
    // The left Session's line without its dispatcher.
    assert_eq!(with(hook.replace(", dispatched by Session 2", "")), "no");
    // A dispatcher the Capsule does not list: the Pack does not say it ended.
    assert_eq!(
        with(hook.replace(
            ", dispatched by Session 2",
            ", dispatched by unlisted Session s-elsewhere"
        )),
        "no"
    );
    // The dispatcher's line says it has not ended.
    let still_open = hook.replace(
        "Session 2 s-dispatcher ended normally (started 2026-11-06T17:31:07.103018Z)",
        "Session 2 s-dispatcher not ended (started 2026-11-06T17:31:07.103018Z, last wrote to the ledger at 2026-11-06T17:31:07.103018Z)",
    );
    assert_ne!(still_open, hook);
    assert_eq!(with(still_open), "no");
    // The dispatcher ended abandoned: ended all the same.
    assert_eq!(
        with(hook.replace(
            "s-dispatcher ended normally",
            "s-dispatcher ended abandoned"
        )),
        "yes"
    );
    // The hook's response without the lines that start with `prefix`.
    let without = |prefix: &str| -> String {
        hook.lines()
            .filter(|line| !line.starts_with(prefix))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    };
    // No line of the dispatcher: an ended Session the Pack only counts.
    assert_eq!(with(without("Session 2 ")), "yes");
    // No line of the left Session.
    assert_eq!(with(without("Session 3 ")), "no");
    // No response of the hook.
    let mut run = FakeRun::good("F");
    run.hook = None;
    assert_eq!(record(&run.score(), "dispatcher_shown_ended"), "no");
}

/// U13 O7: with F in a baseline, `old_session_ended_first` applies to B and
/// F and stays strict; two F runs that end the left Session first meet the
/// line, and one that does not fails it.
#[test]
fn u13_f2_the_pass_line_with_two_f_runs() {
    let with_f = || -> Vec<FakeRun> {
        let mut runs = baseline();
        runs.extend(["E", "E", "F", "F"].map(FakeRun::good));
        runs
    };
    let (summary, met) = summary_of(&with_f());
    assert!(met, "{summary:?}");
    for (name, applicable) in [
        ("old_session_ended_first", 5),
        ("parent_session_kept_open", 2),
        ("hook_injected", 13),
        ("last_passed_line", 11),
        ("tests_run_passed", 11),
        ("fact_cites_own_run", 11),
        ("sessions_closed", 13),
        ("no_permission_denials", 13),
        ("last_line_format", 13),
        ("no_fabrication", 13),
        ("no_verified", 2),
    ] {
        let item = item(&summary, name);
        assert_eq!(
            (item.applicable, item.passed),
            (applicable, applicable),
            "{name}"
        );
    }
    let mut runs = with_f();
    runs[12]
        .events
        .retain(|(kind, _, _)| *kind != "session.crashed");
    let at = session_at(&runs[12].after, "s-old");
    runs[12].after[at].1 = "active";
    let (summary, met) = summary_of(&runs);
    assert!(!met);
    assert!(!item(&summary, "old_session_ended_first").met);
    assert_eq!(item(&summary, "old_session_ended_first").passed, 4);
    // The record changes nothing: a Pack without the dispatcher's end meets
    // the line.
    let mut runs = with_f();
    runs[11].hook = runs[11]
        .hook
        .take()
        .map(|hook| hook.replace(", dispatched by Session 2", ""));
    assert!(summary_of(&runs).1);
}

// --- The manual (U7 H3) -----------------------------------------------------

fn repository_file(path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// `docs/takeover-eval.md` lists every item this entry judges
/// with the scenarios it applies to, names the strict ones and the line of
/// the others; ADR 0018 is Accepted; the guide points to the manual.
#[test]
fn u7_h3_the_manual_names_every_item_its_scenarios_and_the_pass_line() {
    let manual = repository_file("docs/takeover-eval.md");
    for (name, _, scenarios) in ITEMS {
        let row = manual
            .lines()
            .find(|line| line.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| panic!("the manual has no row for {name}"));
        let spaced: Vec<String> = scenarios.chars().map(String::from).collect();
        assert_eq!(
            row.split('|').nth(2).map(str::trim),
            Some(spaced.join(" ").as_str()),
            "{row}"
        );
    }
    // U8 K6: the record items, with their scenarios, and the sentence that
    // they are in no pass line.
    for (name, scenarios) in RECORDS {
        let row = manual
            .lines()
            .find(|line| line.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| panic!("the manual has no row for the record {name}"));
        let spaced: Vec<String> = scenarios.chars().map(String::from).collect();
        assert_eq!(
            row.split('|').nth(2).map(str::trim),
            Some(spaced.join(" ").as_str()),
            "{row}"
        );
    }
    assert!(
        manual.contains("in no pass line"),
        "the manual does not say so"
    );
    let strict: Vec<String> = ITEMS
        .iter()
        .filter(|(_, kind, _)| *kind == Kind::Strict)
        .map(|(name, _, _)| format!("`{name}`"))
        .collect();
    assert!(
        manual.contains(&format!("- {} (strict):", strict.join(", "))),
        "the manual does not name the strict items"
    );
    assert!(manual.contains("`9 × passed ≥ 8 × applicable`"));
    // U12 T1 (Owner O3): the item reads only the denials on memprove, the
    // manual names the three classes, the columns, and where the rule comes
    // from (decision #202).
    let denials_row = manual
        .lines()
        .find(|line| line.starts_with("| `no_permission_denials` |"))
        .unwrap();
    assert!(
        denials_row.contains("of the class `memprove`"),
        "{denials_row}"
    );
    for words in [
        "### The class of a denial (U12 T1)",
        "`memprove`, `identity-time`, or `other`",
        "`denials` and `memprove_denials`",
        "decision #202",
    ] {
        assert!(manual.contains(words), "the manual lacks {words:?}");
    }
    for class in DenialClass::ALL {
        assert!(manual.contains(&format!("`{}`", class.word())), "{class:?}");
    }

    assert!(repository_file("docs/guide.md").contains("`docs/takeover-eval.md`"));
}
