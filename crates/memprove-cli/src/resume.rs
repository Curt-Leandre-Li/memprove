//! The lines `resume --text` prints: one per continuation item of a Task
//! Capsule (ADR 0013 D5), each from a fixed template, with the Task-local
//! claim numbers. The lines are display only: the Capsule JSON is the
//! judgement.
//!
//! P3-C2 (T12 S-1): every item is exactly one line. P3-D2 (T13 S-1, N1,
//! N3): everything an agent wrote (statement text, a Decision's content and
//! rationale, a withdrawal reason, the Task title, argv, each changed file)
//! is quoted in `「」`, with line breaks, control characters, U+2028,
//! U+2029, and invisible format characters written as visible escapes
//! ([`crate::display`]). The verdict words are the template's own, outside
//! those quotes: a line that shows "verified" outside `「」` is the Task line
//! or the line of a `verified` fact that stands (P3-L3, T16 N2). The
//! changed files since a base commit are listed once, at the first fact
//! bound to it, at most 20 with the total. The first line says how many
//! facts were withdrawn, when any were (P3-D2, T13 N2).

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use memprove_domain::{PackCapsule, PackError, render_pack, render_pack_claims, select_pack};
use memprove_protocol::strict_json::{JsonValue, parse_strict};

use crate::{
    context::{CONFIG_DIR_ENV, ContextStore, workspace_root},
    display::{changed_files, command, inline, quoted},
};

fn at<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    path.iter().try_fold(value, |value, name| value.get(name))
}

fn text<'a>(value: &'a JsonValue, path: &[&str]) -> &'a str {
    at(value, path).and_then(JsonValue::as_str).unwrap_or("")
}

fn strings(value: Option<&JsonValue>) -> Vec<&str> {
    value
        .and_then(JsonValue::as_array)
        .map(|items| items.iter().filter_map(JsonValue::as_str).collect())
        .unwrap_or_default()
}

fn numbers(value: Option<&JsonValue>) -> String {
    value
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_i64)
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn number(item: &JsonValue) -> String {
    at(item, &["args", "number"])
        .and_then(JsonValue::as_i64)
        .map_or_else(|| "#?".to_owned(), |number| format!("#{number}"))
}

/// The verdict in words. Only `verified` reads "verified".
fn verdict_word(verdict: &str) -> &'static str {
    match verdict {
        "verified" => "verified",
        "stale" => "stale",
        "partial" => "unconfirmed",
        "tampered" => "tampered",
        _ => "no verdict",
    }
}

/// ` (reasons: …)` for an item that is not verified, empty otherwise.
fn because(item: &JsonValue) -> String {
    let reasons = strings(item.get("reasons"));
    if text(item, &["verdict"]) == "verified" || reasons.is_empty() {
        String::new()
    } else {
        format!(" (reasons: {})", reasons.join(", "))
    }
}

fn argv(item: &JsonValue) -> String {
    command(strings(at(item, &["args", "argv"])))
}

/// The changed files since `base`, as the Capsule lists them.
fn changed_since(capsule: &JsonValue, base: &str) -> String {
    let entry = capsule
        .get("changed_paths_by_base")
        .and_then(JsonValue::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| text(entry, &["base_commit"]) == base)
        });
    let Some(entry) = entry else {
        return "not compared".to_owned();
    };
    if at(entry, &["probed"]).and_then(JsonValue::as_bool) != Some(true) {
        return "not compared (more than 16 base commits)".to_owned();
    }
    if at(entry, &["complete"]).and_then(JsonValue::as_bool) != Some(true) {
        return "cannot be listed (the base commit is not in the repository, or the current state cannot be read)"
            .to_owned();
    }
    let paths = strings(entry.get("paths"));
    if paths.is_empty() {
        "none".to_owned()
    } else {
        changed_files(&paths)
    }
}

/// The 1-based place of a Session among the Capsule's `sessions` (started
/// first, numbered first; P3-D), when it is listed there.
fn session_number(capsule: &JsonValue, session_id: &str) -> Option<usize> {
    capsule
        .get("sessions")
        .and_then(JsonValue::as_array)
        .and_then(|sessions| {
            sessions
                .iter()
                .position(|session| text(session, &["session_id"]) == session_id)
        })
        .map(|index| index + 1)
}

/// The Session a claim came from: `Session N`, or `unlisted Session <id>`
/// when the Capsule does not list it.
fn session_label(capsule: &JsonValue, session_id: &str) -> String {
    session_number(capsule, session_id).map_or_else(
        || format!("unlisted Session {}", inline(session_id)),
        |number| format!("Session {number}"),
    )
}

/// U10 V6 (E1 revision 13, Owner O5): the author of a statement line:
/// [`session_label`], and `「<name>」` after `Session N` when that Session
/// declared an agent, as the Pack writes it.
fn author_label(capsule: &JsonValue, session_id: &str) -> String {
    let mut label = session_label(capsule, session_id);
    if session_number(capsule, session_id).is_some()
        && let Some(name) = session_entry(capsule, session_id)
            .and_then(|session| session.get("agent"))
            .and_then(|agent| agent.get("name"))
            .and_then(JsonValue::as_str)
    {
        label.push_str(&crate::display::quoted(name));
    }
    label
}

/// `verdict: …; from Session N`: the verdict and the Session a claim came from,
/// which every claim line carries (P3-D).
fn judged_from(capsule: &JsonValue, item: &JsonValue) -> String {
    let verdict = verdict_word(text(item, &["verdict"]));
    match text(item, &["session_id"]) {
        "" => format!("verdict: {verdict}"),
        session_id => format!(
            "verdict: {verdict}; from {}",
            author_label(capsule, session_id)
        ),
    }
}

/// `from Session N`: the Session a claim came from, when it names one.
fn from_session(capsule: &JsonValue, item: &JsonValue) -> Option<String> {
    match text(item, &["session_id"]) {
        "" => None,
        session_id => Some(format!("from {}", author_label(capsule, session_id))),
    }
}

/// The claim of the Capsule's `claims` that a continuation item stands for.
fn claim_of<'a>(capsule: &'a JsonValue, item: &JsonValue) -> Option<&'a JsonValue> {
    let claim_id = text(item, &["object_id"]);
    capsule
        .get("claims")
        .and_then(JsonValue::as_array)?
        .iter()
        .find(|claim| text(claim, &["claim_id"]) == claim_id)
}

/// One cited Evidence object in words (P3-L L6): its own result, then
/// whether the commit it is bound to is still the current state. Never a
/// verdict word of a statement and never a reason code.
fn evidence_words(evidence: &JsonValue) -> String {
    let reasons = strings(evidence.get("reasons"));
    let missing = reasons.contains(&"object_missing");
    let status = text(evidence, &["run_status"]);
    let result = match status {
        "PASS" => "passed",
        "FAIL" => "failed",
        "NOT_RUN" => "not run",
        "UNAVAILABLE" => "unavailable",
        _ if missing => "missing",
        _ if matches!(
            text(evidence, &["kind"]),
            "git_head" | "git_worktree" | "file_digest"
        ) =>
        {
            "repository state record"
        }
        _ => "result unknown",
    };
    let fresh = reasons.contains(&"evidence_not_passed");
    let overruled = if status == "PASS" && fresh {
        " (overruled by a later failed run of the same command in the same state)"
    } else {
        ""
    };
    let commit = match text(evidence, &["base_commit"]) {
        "" => "the bound commit".to_owned(),
        base => format!("the bound commit {}", inline(base)),
    };
    let state = match text(evidence, &["verdict"]) {
        "tampered" => "record tampered".to_owned(),
        _ if missing => "unreadable".to_owned(),
        "stale" => format!("{commit} is no longer the current state"),
        "verified" => format!("{commit} is still the current state"),
        "partial" if fresh => format!("{commit} is still the current state"),
        _ => format!("whether {commit} is still the current state is unconfirmed"),
    };
    format!("{result}{overruled}, {state}")
}

/// What an open item, a next action, or a Decision shows instead of a
/// verdict (P3-L L6, Owner O5): `no evidence cited`, or each cited Evidence
/// in words; `tampered; ` first when the Capsule is tampered (D7 unchanged).
fn cited_evidence(capsule: &JsonValue, item: &JsonValue) -> String {
    let evidence = claim_of(capsule, item)
        .and_then(|claim| claim.get("evidence"))
        .and_then(JsonValue::as_array)
        .unwrap_or_default();
    let words = match evidence {
        [] => "no evidence cited".to_owned(),
        [only] => format!("evidence: {}", evidence_words(only)),
        several => several
            .iter()
            .enumerate()
            .map(|(index, evidence)| {
                format!("evidence {}: {}", index + 1, evidence_words(evidence))
            })
            .collect::<Vec<_>>()
            .join("; "),
    };
    let tampered = if text(item, &["verdict"]) == "tampered" {
        "tampered; "
    } else {
        ""
    };
    match from_session(capsule, item) {
        Some(session) => format!("{tampered}{words}; {session}"),
        None => format!("{tampered}{words}"),
    }
}

/// What a superseded or withdrawn claim shows: its Evidence in words, of
/// every kind, a fact included (P3-L3, Owner, T16 N2): no verdict and no
/// reason code, so "verified" stays on the facts that stand. A claim the
/// Capsule does not list shows only the Session it came from, if any. The
/// ` (…)` is empty, and left out, when there is nothing to show.
fn cited_or_from(capsule: &JsonValue, item: &JsonValue) -> Option<String> {
    if claim_of(capsule, item).is_some() {
        Some(cited_evidence(capsule, item))
    } else {
        from_session(capsule, item)
    }
}

/// ` (parts; …)`, or nothing when there is no part.
fn parenthesized(parts: impl IntoIterator<Item = Option<String>>) -> String {
    let parts = parts.into_iter().flatten().collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join("; "))
    }
}

fn head(item: &JsonValue) -> String {
    match text(item, &["args", "head_oid"]) {
        "" => "(unreadable)".to_owned(),
        head => inline(head),
    }
}

fn bases(item: &JsonValue) -> Vec<String> {
    strings(at(item, &["args", "base_commits"]))
        .into_iter()
        .map(inline)
        .collect()
}

/// The Capsule's entry of `session_id` in `sessions`.
fn session_entry<'a>(capsule: &'a JsonValue, session_id: &str) -> Option<&'a JsonValue> {
    capsule
        .get("sessions")
        .and_then(JsonValue::as_array)?
        .iter()
        .find(|session| text(session, &["session_id"]) == session_id)
}

/// P3-L L7: when the ledger last recorded an Event of `session_id`, from
/// the Capsule (the ledger's time, never the clock).
fn last_write(capsule: &JsonValue, session_id: &str) -> String {
    match session_entry(capsule, session_id)
        .and_then(|session| session.get("last_recorded_at"))
        .and_then(JsonValue::as_str)
    {
        Some(at) => format!("last wrote to the ledger at {}", inline(at)),
        None => "nothing of it in the ledger yet".to_owned(),
    }
}

/// U8 A6 (Owner O4): ` (withdrawn by Session j)` (or ` (withdrawn by unlisted
/// Session <id>)` for a Session the Capsule does not list) after a withdrawn
/// claim's line, from the `withdrawn_by` of its Capsule claim; ` (withdrawer
/// unreadable)` when it is
/// `null`, the Checkpoint that recorded the edge not readable (U8 S-F round
/// 1, B2; the projection's words); empty without the member.
fn withdrawn_by(capsule: &JsonValue, item: &JsonValue) -> String {
    let number = at(item, &["args", "number"]).and_then(JsonValue::as_i64);
    let by = capsule
        .get("claims")
        .and_then(JsonValue::as_array)
        .and_then(|claims| {
            claims
                .iter()
                .find(|claim| claim.get("number").and_then(JsonValue::as_i64) == number)
        })
        .and_then(|claim| claim.get("withdrawn_by"));
    match by {
        None => String::new(),
        Some(JsonValue::Null) => " (withdrawer unreadable)".to_owned(),
        Some(by) => by
            .get("session_id")
            .and_then(JsonValue::as_str)
            .map_or_else(String::new, |session_id| {
                format!(" (withdrawn by {})", session_label(capsule, session_id))
            }),
    }
}

/// U8 A4 (E1 revision 10, Owner O4): the agent of `session_id` as its
/// Session line ends, as the Pack writes it: `; agent「<name>」(<role>)`,
/// with `, dispatched by Session <j>` (or `, dispatched by unlisted Session
/// <id>` for a parent the Capsule does not list) inside the parentheses;
/// empty for a Session that
/// declared no agent.
fn agent_words(capsule: &JsonValue, session_id: &str) -> String {
    let Some(agent) = session_entry(capsule, session_id)
        .and_then(|session| session.get("agent"))
        .filter(|agent| !matches!(agent, JsonValue::Null))
    else {
        return String::new();
    };
    let parent = match agent.get("parent_session_id").and_then(JsonValue::as_str) {
        None => String::new(),
        Some(parent) => match session_number(capsule, parent) {
            Some(number) => format!(", dispatched by Session {number}"),
            None => format!(", dispatched by unlisted Session {}", inline(parent)),
        },
    };
    format!(
        "; agent{}({}{parent})",
        crate::display::quoted(text(agent, &["name"])),
        inline(text(agent, &["role"])),
    )
}

/// U9 A7 (E1 revision 12, Owner O3): who ended the abandoned Session
/// `session_id`, as its line ends, as the Pack writes it: `; ended by Session
/// <j>` (or `; ended by unlisted Session <id>` for an ender Session the
/// Capsule does not list), else `; ended by agent「<name>」(<role>)`; empty
/// when the Capsule
/// names no ender.
fn ender_words(capsule: &JsonValue, session_id: &str) -> String {
    let Some(ended_by) = session_entry(capsule, session_id)
        .and_then(|session| session.get("ended_by"))
        .filter(|ended_by| !matches!(ended_by, JsonValue::Null))
    else {
        return String::new();
    };
    if let Some(ender) = ended_by.get("session_id").and_then(JsonValue::as_str) {
        return format!("; ended by {}", session_label(capsule, ender));
    }
    match ended_by.get("agent") {
        Some(agent) => format!(
            "; ended by agent{}({})",
            crate::display::quoted(text(agent, &["name"])),
            inline(text(agent, &["role"])),
        ),
        None => String::new(),
    }
}

/// P3-L L7: one line counting the Task's Sessions by how they ended, when
/// the Capsule lists them.
fn session_summary(capsule: &JsonValue) -> Option<String> {
    let sessions = capsule.get("sessions")?.as_array()?;
    let count = |state: &str| {
        sessions
            .iter()
            .filter(|session| text(session, &["end_state"]) == state)
            .count()
    };
    Some(format!(
        "Sessions: {} in all, {} ended normally, {} ended abandoned, {} not ended",
        sessions.len(),
        count("ended"),
        count("crashed"),
        count("open"),
    ))
}

/// The Session the CLI context names, and which context file named it
/// (P4-E, ADR 0014 E5): this directory's, or this client session's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrentSession<'a> {
    /// The Session.
    pub session_id: &'a str,
    /// Whether the context file is the client session's
    /// (`MEMPROVE_CLIENT_SESSION`), not the directory's.
    pub client: bool,
}

impl CurrentSession<'_> {
    /// What a `not ended` line of this Session says after its ID.
    fn label(self) -> &'static str {
        if self.client {
            " (the Session this client session's context file records)"
        } else {
            " (the Session this directory's context file records)"
        }
    }
}

/// One line for one continuation item; `current_session` is the Session of
/// the CLI context, if any. `listed` holds each base commit whose changed
/// files an earlier line listed, with that line's number.
#[allow(clippy::too_many_lines)] // Every template in one place.
fn line(
    capsule: &JsonValue,
    item: &JsonValue,
    current_session: Option<CurrentSession<'_>>,
    listed: &mut BTreeMap<String, String>,
) -> String {
    let verdict = text(item, &["verdict"]);
    let object = inline(text(item, &["object_id"]));
    let quoted = quoted(text(item, &["args", "text"]));
    let judged = judged_from(capsule, item);
    let session = session_number(capsule, text(item, &["object_id"])).map_or_else(
        || "Session ".to_owned(),
        |number| format!("Session {number} "),
    );
    match text(item, &["template"]) {
        "header" => format!(
            "Task{} ({}): {} (ledger sequence {}, current HEAD {}{}){}",
            crate::display::quoted(text(item, &["args", "title"])),
            text(item, &["args", "task_status"]),
            verdict_word(verdict),
            at(item, &["args", "project_sequence"])
                .and_then(JsonValue::as_i64)
                .unwrap_or_default(),
            head(item),
            // P3-D2 (Owner, T13 N2): how many facts were withdrawn.
            match at(item, &["args", "withdrawn_facts"]).and_then(JsonValue::as_i64) {
                Some(count) if count > 0 => format!(", withdrawn facts {count}"),
                _ => String::new(),
            },
            because(item),
        ),
        "ledger_chain_invalid" => {
            "Integrity: the event chain does not verify; every conclusion is treated as tampered"
                .to_owned()
        }
        "damaged_record" => format!(
            "Integrity: {} {object} is missing, does not recompute, or disagrees with the ledger; \
             every conclusion is treated as tampered{}",
            text(item, &["args", "record_type"]),
            because(item),
        ),
        // P3-C (Owner): a Session without an end Event has not ended yet.
        // P3-L L7: it shows when the ledger last recorded one of its
        // Events, and the Session the context file of this directory names
        // is labelled as only that.
        "session_open" => format!(
            "{session}{object}{} not ended (started {}, {}){}",
            match current_session {
                Some(current) if current.session_id == text(item, &["object_id"]) =>
                    current.label(),
                _ => "",
            },
            inline(text(item, &["args", "started_at"])),
            last_write(capsule, text(item, &["object_id"])),
            agent_words(capsule, text(item, &["object_id"])),
        ),
        // U9 A7 (E1 revision 12): and who ended it, after the agent.
        "session_crashed" => format!(
            "{session}{object} ended abandoned (session.crashed; started {}){}{}",
            inline(text(item, &["args", "started_at"])),
            agent_words(capsule, text(item, &["object_id"])),
            ender_words(capsule, text(item, &["object_id"])),
        ),
        // P3-L L6 (Owner O5): no verdict and no reason code on an open
        // item, a next action, or a Decision; their Evidence in words.
        "open_item" => format!(
            "{} open item: {quoted} ({})",
            number(item),
            cited_evidence(capsule, item),
        ),
        "fact" if verdict == "verified" => format!(
            "{} verified: {quoted} (evidence bound to commit {}, current HEAD {}; {judged})",
            number(item),
            bases(item).join(", "),
            head(item),
        ),
        "fact" => {
            let bases = bases(item);
            let changed = strings(at(item, &["args", "base_commits"]))
                .into_iter()
                .map(|base| {
                    let since = if let Some(first) = listed.get(base) {
                        format!("same as {first}")
                    } else {
                        listed.insert(base.to_owned(), number(item));
                        changed_since(capsule, base)
                    };
                    format!("since {}: {since}", inline(base))
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "{} fact {}: {quoted} (evidence bound to commit {}, current HEAD {}; changed files \
                 {changed}; {judged}){}",
                number(item),
                verdict_word(verdict),
                bases.join(", "),
                head(item),
                because(item),
            )
        }
        "rerun" => format!(
            "Rerun: {} on the current HEAD {} (for {})",
            argv(item),
            head(item),
            numbers(at(item, &["args", "numbers"])),
        ),
        "reaffirm" => format!(
            "No rerun needed: {} passed in a later run that matches the current state (evidence \
             {}, snapshot {}); a new Checkpoint on that snapshot can cite it and reaffirm {}",
            argv(item),
            inline(text(item, &["args", "verified_evidence_id"])),
            inline(text(item, &["args", "verified_snapshot_id"])),
            numbers(at(item, &["args", "numbers"])),
        ),
        "decision" => {
            let rationale = match at(item, &["args", "rationale"]).and_then(JsonValue::as_str) {
                None => String::new(),
                Some(rationale) => format!("; rationale {}", crate::display::quoted(rationale)),
            };
            format!(
                "{} decision: {quoted} (active, not superseded; {}{rationale})",
                number(item),
                cited_evidence(capsule, item),
            )
        }
        "next_action" => format!(
            "{} next action: {quoted} ({})",
            number(item),
            cited_evidence(capsule, item)
        ),
        "superseded" => format!(
            "{} superseded by {}: {quoted}{}",
            number(item),
            numbers(at(item, &["args", "superseded_by"])),
            parenthesized([cited_or_from(capsule, item)]),
        ),
        // P3-D2: the reason the withdrawal gave, when it gave one. U8 A6:
        // and the Session that withdrew it, when the Capsule names it.
        "withdrawn" => format!(
            "{} withdrawn: {quoted}{}{}",
            number(item),
            parenthesized([
                cited_or_from(capsule, item),
                at(item, &["args", "withdrawal_reason"])
                    .and_then(JsonValue::as_str)
                    .map(|reason| format!("withdrawal reason {}", crate::display::quoted(reason))),
            ]),
            withdrawn_by(capsule, item),
        ),
        other => format!("[{}] {object}", inline(other)),
    }
}

/// Whether a continuation item stands for a superseded or withdrawn claim
/// (class 9, ADR 0013 D5), which `resume --text` folds into one line
/// unless `--all` is given (P4-A).
fn folded(item: &JsonValue) -> bool {
    matches!(text(item, &["template"]), "superseded" | "withdrawn")
}

/// `#1–#3, #5`: ascending numbers, each run of consecutive numbers as a
/// range.
fn ranges(numbers: &[i64]) -> String {
    let mut numbers = numbers.to_vec();
    numbers.sort_unstable();
    numbers.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut rest = numbers.iter().copied().peekable();
    while let Some(first) = rest.next() {
        let mut last = first;
        while rest.next_if_eq(&(last + 1)).is_some() {
            last += 1;
        }
        parts.push(if first == last {
            format!("#{first}")
        } else {
            format!("#{first}–#{last}")
        });
    }
    parts.join(", ")
}

/// P4-A (Owner): the one line that stands for the superseded and withdrawn
/// claims `resume --text` does not list one by one: how many of each, and
/// their numbers. `None` when there are none. It holds no verdict word.
fn folded_summary(items: &[JsonValue]) -> Option<String> {
    let numbers_of = |template: &str| {
        items
            .iter()
            .filter(|item| text(item, &["template"]) == template)
            .map(|item| at(item, &["args", "number"]).and_then(JsonValue::as_i64))
            .collect::<Vec<_>>()
    };
    let part = |label: &str, numbers: &[Option<i64>]| {
        let known = numbers.iter().flatten().copied().collect::<Vec<_>>();
        if known.is_empty() {
            format!("{label} {}", numbers.len())
        } else {
            format!("{label} {}: {}", numbers.len(), ranges(&known))
        }
    };
    let superseded = numbers_of("superseded");
    let withdrawn = numbers_of("withdrawn");
    (!superseded.is_empty() || !withdrawn.is_empty()).then(|| {
        format!(
            "{}; {} (not listed one by one; resume --text --all lists them all)",
            part("superseded", &superseded),
            part("withdrawn", &withdrawn),
        )
    })
}

/// One line per continuation item of a Task Capsule body, then one line of
/// gaps when there are any; `None` when the body is not a Task Capsule.
/// `current_session` is the Session of the CLI context, marked as the
/// current one. Unless `all` (`--all`), the superseded and withdrawn claims
/// are not listed one by one: one line at the end of the statements counts
/// them and names their numbers (P4-A); every other line is the same.
#[must_use]
pub fn render(body: &str, current_session: Option<&str>, all: bool) -> Option<String> {
    render_as(
        body,
        current_session.map(|session_id| CurrentSession {
            session_id,
            client: false,
        }),
        all,
    )
}

/// [`render`], with the current Session labelled by the context file that
/// names it (P4-E): this client session's or this directory's.
#[must_use]
pub fn render_as(
    body: &str,
    current_session: Option<CurrentSession<'_>>,
    all: bool,
) -> Option<String> {
    let capsule = parse_strict(body).ok()?;
    let every = capsule.get("continuation")?.as_array()?;
    Some(render_items(&capsule, every, current_session, all))
}

/// U8 A5 (Owner O4): `resume --text --session <k>`: the lines of
/// [`render_as`] with, of the Session lines and the statements, only those
/// of `Session k` (the k-th of the Capsule's `sessions`, as the Pack and
/// `resume --text` number them); the Task line, the integrity lines, and the
/// Session summary stay. A local filter of the Capsule the daemon sent.
///
/// # Errors
///
/// The usage error (exit 2) when the body is not a Task Capsule, `k` names
/// no Session of it, or `Session k` wrote no statement of the Task.
pub fn render_session(
    body: &str,
    current_session: Option<CurrentSession<'_>>,
    all: bool,
    number: u64,
) -> Result<String, String> {
    let not_capsule = || "the reply is not a Task Capsule; nothing is shown".to_owned();
    let capsule = parse_strict(body).map_err(|_| not_capsule())?;
    let sessions = capsule
        .get("sessions")
        .and_then(JsonValue::as_array)
        .ok_or_else(not_capsule)?;
    let every = capsule
        .get("continuation")
        .and_then(JsonValue::as_array)
        .ok_or_else(not_capsule)?;
    let session_id = usize::try_from(number)
        .ok()
        .and_then(|number| number.checked_sub(1))
        .and_then(|index| sessions.get(index))
        .map(|session| text(session, &["session_id"]).to_owned())
        .ok_or_else(|| {
            format!(
                "--session {number} names no Session of this Task, which has {} (Session 1 to \
                 Session {})",
                sessions.len(),
                sessions.len()
            )
        })?;
    let wrote = capsule
        .get("claims")
        .and_then(JsonValue::as_array)
        .is_some_and(|claims| {
            claims
                .iter()
                .any(|claim| text(claim, &["session_id"]) == session_id)
        });
    if !wrote {
        return Err(format!("Session {number} wrote no statement of this Task"));
    }
    let kept = every
        .iter()
        .filter(|item| {
            at(item, &["class"])
                .and_then(JsonValue::as_i64)
                .is_some_and(|class| class <= 1)
                || text(item, &["session_id"]) == session_id
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(render_items(&capsule, &kept, current_session, all))
}

/// The lines of `every` continuation item of `capsule` (see [`render_as`]).
fn render_items(
    capsule: &JsonValue,
    every: &[JsonValue],
    current_session: Option<CurrentSession<'_>>,
    all: bool,
) -> String {
    let items = every
        .iter()
        .filter(|item| all || !folded(item))
        .collect::<Vec<_>>();
    let mut listed = BTreeMap::new();
    let mut lines = items
        .iter()
        .map(|item| line(capsule, item, current_session, &mut listed))
        .collect::<Vec<_>>();
    // P3-L L7: the Session summary follows the Task line and the integrity
    // lines, before the Sessions that did not end normally.
    if let Some(summary) = session_summary(capsule) {
        let at = items
            .iter()
            .position(|item| {
                at(item, &["class"])
                    .and_then(JsonValue::as_i64)
                    .is_none_or(|class| class > 1)
            })
            .unwrap_or(items.len());
        lines.insert(at, summary);
    }
    if !all && let Some(summary) = folded_summary(every) {
        lines.push(summary);
    }
    let gaps = strings(capsule.get("gaps"));
    if !gaps.is_empty() {
        lines.push(format!(
            "Gaps: {}",
            gaps.into_iter().map(inline).collect::<Vec<_>>().join(", ")
        ));
    }
    lines.join("\n")
}

/// P4-C: the Context Pack of a Task Capsule body within `budget` UTF-16
/// code units (ADR 0014 E1, E2). The selection and the lines are the
/// domain's ([`memprove_domain::select_pack`]); this only reads the body.
///
/// # Errors
///
/// U7 P1 (E1 revision 9, Owner O3): `own` names the reader's own
/// Sessions, whose `Session ` line, while they have not ended, has no takeover
/// hint; every other Session that has not ended has it.
///
/// # Errors
///
/// [`PackError::Malformed`] when the body is not a Task Capsule the Pack can
/// render; [`PackError::OverBudget`] when even level 5 does not fit.
pub fn pack(body: &str, budget: usize, own: Own<'_>) -> Result<String, PackError> {
    let capsule = own.read(body)?;
    let selection = select_pack(&capsule, budget)?;
    Ok(render_pack(&selection))
}

/// U7 P1 (E1 revision 9; D2 F16, Owner O3): which Sessions a reader of a
/// Pack calls its own, as O3 names them (the `resumed_from` one, or the one
/// this client's context records):
/// the Session the context the reader uses names, and every Session that
/// declared it continues from the Pack the reader's last-Pack record names.
/// The default is a reader with neither: every open Session is someone
/// else's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Own<'a> {
    /// The Session the reader's context file names.
    pub session_id: Option<&'a str>,
    /// The ledger sequence and `pack_digest` (`sha256:<hex>`) of the Pack
    /// the reader's last-Pack record names.
    pub resumed_from: Option<(u64, &'a str)>,
    /// U10 T7 (Owner O4): the program the takeover hint names
    /// ([`takeover_program`] for this process); `None` names
    /// `memprove`.
    pub program: Option<&'a str>,
}

impl<'a> Own<'a> {
    /// The reader whose context names `session_id` and whose last-Pack
    /// record names the Pack `recorded` ([`recorded_pack`]), running this
    /// executable ([`takeover_program`]).
    #[must_use]
    pub fn of(session_id: Option<&'a str>, recorded: Option<&'a (u64, String)>) -> Self {
        Self {
            session_id,
            resumed_from: recorded.map(|(sequence, digest)| (*sequence, digest.as_str())),
            program: Some(takeover_program()),
        }
    }

    /// The Capsule of `body` as this reader reads it
    /// ([`PackCapsule::with_own_session`],
    /// [`PackCapsule::with_own_resumed`],
    /// [`PackCapsule::with_takeover_program`]).
    fn read(self, body: &str) -> Result<PackCapsule, PackError> {
        let capsule = PackCapsule::from_json(body)?
            .with_own_session(self.session_id)
            .with_own_resumed(self.resumed_from);
        Ok(match self.program {
            Some(program) => capsule.with_takeover_program(program),
            None => capsule,
        })
    }
}

/// U10 T7 (Owner O4; D3 F30): the program the takeover hint of this
/// process's Packs names, decided once per process by [`program_for`] from
/// this executable and `PATH`. `resume --pack`, the hook, and
/// the MCP server are this one executable, so in one environment they name
/// the same program and give the same Pack bytes (ADR 0014 E7 (u)) and the
/// same `pack_digest` (the last-Pack record, E6 (i)).
#[must_use]
pub fn takeover_program() -> &'static str {
    static PROGRAM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PROGRAM.get_or_init(|| {
        program_for(
            crate::hook::executable().ok().as_deref().map(Path::new),
            std::env::var_os("PATH").as_deref(),
        )
    })
}

/// U10 T7: [`memprove_domain::PACK_TAKEOVER_PROGRAM`] when the canonical
/// `executable` is named `memprove` and its directory is one of `path`'s
/// (the `path` check of `doctor`, so that the word runs this executable),
/// else the executable's absolute path when the Pack can show it
/// ([`memprove_domain::is_takeover_program`]), else the word anyway.
#[must_use]
pub fn program_for(executable: Option<&Path>, path: Option<&std::ffi::OsStr>) -> String {
    let word = memprove_domain::PACK_TAKEOVER_PROGRAM;
    let Some(executable) = executable else {
        return word.to_owned();
    };
    let found = executable.file_name().is_some_and(|name| name == word)
        && executable
            .parent()
            .is_some_and(|dir| crate::doctor::dir_on_path(dir, path));
    match executable.to_str() {
        Some(absolute) if !found && memprove_domain::is_takeover_program(absolute) => {
            absolute.to_owned()
        }
        _ => word.to_owned(),
    }
}

/// U7 P1 (Owner O3): the ledger sequence and `pack_digest` of the Pack the
/// last-Pack record of `store` at the Workspace root of `cwd` names; `None`
/// when there is no such record or it cannot be read.
#[must_use]
pub fn recorded_pack(store: Option<&ContextStore>, cwd: Option<&Path>) -> Option<(u64, String)> {
    let root = workspace_root(cwd?).ok()?;
    let pack = store?.last_pack(&root).ok()??;
    Some((
        pack.resumed_from.project_sequence,
        format!("sha256:{}", pack.resumed_from.pack_digest.as_str()),
    ))
}

/// Why a query Pack could not be made (U3 G4).
#[derive(Debug)]
pub enum QueryPackFailure {
    /// As [`pack`]: the Capsule cannot be rendered, or even level 5 does
    /// not fit.
    Pack(PackError),
    /// The query is refused (the CLI checks it before sending anything).
    Refused(memprove_domain::QueryRefusal),
    /// The in-memory FTS5 index could not be built or read.
    Index(String),
}

/// U3 G4 (Owner O8, O9): the query Pack of a Task Capsule body for `query`
/// within `budget`. The statements of the Capsule (superseded and withdrawn
/// ones too) are ranked by an in-memory `SQLite` FTS5 index built here and
/// dropped on return ([`memprove_store_sqlite::rank_statements_v2`]: the
/// matching of U4 S2); nothing is persisted or sent. The selection and the
/// lines are the domain's ([`memprove_domain::select_query_pack`]). `own`
/// is as in [`pack`].
///
/// # Errors
///
/// [`QueryPackFailure`].
pub fn query_pack(
    body: &str,
    query: &str,
    budget: usize,
    own: Own<'_>,
) -> Result<String, QueryPackFailure> {
    use memprove_domain::{Standing, pack_kind_label};
    let capsule = own.read(body).map_err(QueryPackFailure::Pack)?;
    memprove_domain::check_pack_query(query).map_err(QueryPackFailure::Refused)?;
    let entries = capsule
        .search_statements()
        .into_iter()
        .map(|statement| memprove_store_sqlite::SearchEntry {
            number: statement.number,
            text: statement.text,
            rationale: statement.rationale,
            kind_label: pack_kind_label(statement.kind),
            active: statement.standing == Standing::Active,
        })
        .collect::<Vec<_>>();
    let matches = memprove_store_sqlite::rank_statements_v2(&entries, query)
        .map_err(|error| QueryPackFailure::Index(error.to_string()))?;
    let query =
        memprove_domain::PackQuery::new(query, matches, memprove_store_sqlite::sqlite_version())
            .map_err(QueryPackFailure::Refused)?;
    let selection = memprove_domain::select_query_pack(&capsule, &query, budget)
        .map_err(QueryPackFailure::Pack)?;
    Ok(render_pack(&selection))
}

/// ADR 0014 E6 (P4-F, Owner condition i): what `begin` later declares as
/// `resumed_from` for the full Pack `text` that `resume --pack` printed for
/// the Capsule `body` at `budget`: the Task, the `capsule_digest`, the
/// ledger sequence, the `pack_digest` (its `Digest: ` line), and the budget.
///
/// # Errors
///
/// What could not be read.
pub fn last_pack(
    body: &str,
    text: &str,
    budget: usize,
) -> Result<crate::context::LastPack, String> {
    let capsule = parse_strict(body).map_err(|error| error.to_string())?;
    let task_id = text_at(&capsule, &["task", "task_id"]).ok_or("no task_id")?;
    let pack_digest = text
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("Digest: "))
        .ok_or("no Digest: line")?;
    let resumed_from = memprove_domain::ResumedFrom::new(
        text_at(&capsule, &["capsule_digest"]),
        at(&capsule, &["bindings", "ledger", "project_sequence"])
            .and_then(JsonValue::as_i64)
            .and_then(|sequence| u64::try_from(sequence).ok()),
        Some(pack_digest),
        u64::try_from(budget).ok(),
    )
    .map_err(|refusal| refusal.0)?;
    Ok(crate::context::LastPack {
        task_id: task_id.to_owned(),
        resumed_from,
        project_id: None,
    })
}

/// ADR 0014 E6 (P4-F, Owner condition i): after `resume --pack` printed a
/// full Pack, records its Task, `capsule_digest`, ledger sequence,
/// `pack_digest`, and budget next to the context file of the Workspace root
/// of `cwd` in `store`, for `begin` to declare. A failure is a note on
/// standard error only; the output and the exit code do not change. P4-G
/// (ADR 0014 E7.5.5): the MCP `pack` tool records its Pack the same way.
/// U1 (A4): `project_id`, when the caller knows the Task's Project (the MCP
/// `pack` tool does), is recorded with it, as the `SessionStart` hook
/// records it, so that `begin` in a client session with no context of its
/// own needs no Project; `resume --pack` passes `None`, as before.
pub fn remember_pack(
    store: Option<&ContextStore>,
    cwd: io::Result<PathBuf>,
    body: &str,
    text: &str,
    budget: usize,
    project_id: Option<&str>,
) {
    let warn = |why: String| {
        crate::log_line(format_args!(
            "memprove: warning: the Pack was not recorded for `memprove begin` ({why}); \
             a Session begun now does not declare it as resumed_from"
        ));
    };
    let Some(store) = store else {
        return warn(format!(
            "no configuration directory; set {CONFIG_DIR_ENV} or HOME"
        ));
    };
    let root = match cwd.and_then(|cwd| workspace_root(&cwd)) {
        Ok(root) => root,
        Err(error) => return warn(format!("cannot resolve the current directory: {error}")),
    };
    let mut pack = match last_pack(body, text, budget) {
        Ok(pack) => pack,
        Err(error) => return warn(error),
    };
    pack.project_id = project_id
        .filter(|project| crate::claims::is_uuid_v7(project))
        .map(str::to_owned);
    if let Err(error) = store.save_last_pack(&root, &pack) {
        warn(error.to_string());
    }
}

fn text_at<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a str> {
    at(value, path).and_then(JsonValue::as_str)
}

/// P4-C2 revision 5: the full Pack lines of the claims `ranges` names
/// (`resume --pack --claims`), after the base lines they name; no budget.
///
/// # Errors
///
/// [`PackError::Malformed`] when the body is not a Task Capsule the Pack can
/// render; [`PackError::NoSuchClaim`] for a number it does not list.
pub fn pack_claims(body: &str, ranges: &[(u64, u64)]) -> Result<String, PackError> {
    render_pack_claims(&PackCapsule::from_json(body)?, ranges)
}

/// U1 (A5): [`pack_claims`] within `budget` UTF-16 code units, cut the
/// Pack's way when the whole lines do not fit (the MCP `claims` tool).
///
/// # Errors
///
/// As [`pack_claims`], and [`PackError::OverBudget`] when even the
/// omission line does not fit.
pub fn pack_claims_within(
    body: &str,
    ranges: &[(u64, u64)],
    budget: usize,
) -> Result<String, PackError> {
    memprove_domain::render_pack_claims_within(&PackCapsule::from_json(body)?, ranges, budget)
}

/// The claim number of a continuation item, when it is one of a claim.
fn claim_number(item: &JsonValue) -> Option<u64> {
    at(item, &["args", "number"])
        .and_then(JsonValue::as_i64)
        .and_then(|number| u64::try_from(number).ok())
}

/// P4-C: the `resume --text --all` line of each claim whose number one of
/// `ranges` (inclusive) holds, superseded and withdrawn ones too, in the
/// order `resume --text --all` lists them. The changed files since a base
/// commit are listed at the first of these lines bound to it.
///
/// # Errors
///
/// Says which number the Capsule does not list (a usage error), or that the
/// body is not a Task Capsule.
pub fn render_claims(
    body: &str,
    current_session: Option<CurrentSession<'_>>,
    ranges: &[(u64, u64)],
) -> Result<String, String> {
    let not_a_capsule = || "the response is not a Task Capsule".to_owned();
    let capsule = parse_strict(body).map_err(|_| not_a_capsule())?;
    let every = capsule
        .get("continuation")
        .and_then(JsonValue::as_array)
        .ok_or_else(not_a_capsule)?;
    let listed = every
        .iter()
        .filter_map(claim_number)
        .collect::<std::collections::BTreeSet<_>>();
    for (first, last) in ranges {
        let mut expected = Some(*first);
        for number in listed.range(first..=last) {
            if expected != Some(*number) {
                break;
            }
            expected = number.checked_add(1);
        }
        if let Some(missing) = expected.filter(|missing| missing <= last) {
            return Err(format!(
                "--claims names #{missing}, which the Task Capsule does not list; it lists {}",
                match (listed.first(), listed.last()) {
                    (Some(low), Some(high)) => format!("#{low}–#{high}"),
                    _ => "no claim".to_owned(),
                }
            ));
        }
    }
    let wanted = |item: &&JsonValue| {
        claim_number(item).is_some_and(|number| {
            ranges
                .iter()
                .any(|(first, last)| (*first..=*last).contains(&number))
        })
    };
    let mut changed = BTreeMap::new();
    Ok(every
        .iter()
        .filter(wanted)
        .map(|item| line(&capsule, item, current_session, &mut changed))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::display::outside_quotes;

    fn item(class: u8, template: &str, verdict: &str, args: &str) -> String {
        format!(
            r#"{{"class":{class},"template":"{template}","object_id":"O","session_id":null,
                "verdict":{verdict},"reasons":["head_changed","evidence_stale"],"standing":null,
                "args":{args}}}"#
        )
    }

    fn capsule(items: &[String]) -> String {
        format!(
            r#"{{"continuation":[{}],"gaps":["base_commits_over_limit"],
                "changed_paths_by_base":[
                    {{"base_commit":"X","probed":true,"complete":true,"paths":["repo://README"]}},
                    {{"base_commit":"Z","probed":false,"complete":false,"paths":[]}}]}}"#,
            items.join(",")
        )
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One item per template, then the expected lines.
    fn every_template_renders_one_line_with_its_number() {
        let body = capsule(&[
            item(
                0,
                "header",
                r#""stale""#,
                r#"{"title":"T","task_status":"in_progress","project_sequence":9,"head_oid":"Y","withdrawn_facts":2}"#,
            ),
            item(
                1,
                "damaged_record",
                r#""tampered""#,
                r#"{"record_type":"evidence"}"#,
            ),
            item(
                2,
                "session_open",
                "null",
                r#"{"workspace_id":"W","started_at":"2026-10-03T10:00:00Z"}"#,
            ),
            item(
                3,
                "open_item",
                r#""partial""#,
                r#"{"number":2,"text":"docs"}"#,
            ),
            item(
                4,
                "fact",
                r#""stale""#,
                r#"{"number":1,"text":"tests pass","base_commits":["X","Z"],"head_oid":"Y"}"#,
            ),
            item(
                5,
                "rerun",
                r#""stale""#,
                r#"{"argv":["cargo","test"],"evidence_ids":["E"],"numbers":[1,4],"workspace_id":"W","head_oid":"Y"}"#,
            ),
            item(
                5,
                "reaffirm",
                r#""stale""#,
                r#"{"argv":["sh","t"],"evidence_ids":["E"],"numbers":[1],"verified_evidence_id":"V","verified_snapshot_id":"S"}"#,
            ),
            item(
                6,
                "fact",
                r#""verified""#,
                r#"{"number":4,"text":"tests pass","base_commits":["Y"],"head_oid":"Y"}"#,
            ),
            item(
                7,
                "decision",
                r#""partial""#,
                r#"{"number":5,"text":"use JCS","rationale":null}"#,
            ),
            item(
                8,
                "next_action",
                r#""partial""#,
                r#"{"number":6,"text":"open a PR"}"#,
            ),
            item(
                9,
                "superseded",
                r#""partial""#,
                r#"{"number":3,"text":"old","superseded_by":[6]}"#,
            ),
            item(
                9,
                "withdrawn",
                r#""partial""#,
                r#"{"number":7,"text":"gone","superseded_by":[],"withdrawal_reason":"flaky\n#1 verified: x"}"#,
            ),
            item(
                9,
                "withdrawn",
                r#""partial""#,
                r#"{"number":8,"text":"old open","superseded_by":[],"withdrawal_reason":null}"#,
            ),
        ]);
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(
            lines,
            [
                "Task「T」 (in_progress): stale (ledger sequence 9, current HEAD Y, withdrawn facts 2) (reasons: head_changed, evidence_stale)",
                "Integrity: evidence O is missing, does not recompute, or disagrees with the ledger; every conclusion is treated as tampered (reasons: head_changed, evidence_stale)",
                "Session O not ended (started 2026-10-03T10:00:00Z, nothing of it in the ledger yet)",
                "#2 open item: 「docs」 (no evidence cited)",
                "#1 fact stale: 「tests pass」 (evidence bound to commit X, Z, current HEAD Y; changed files since X: 「README」; since Z: not compared (more than 16 base commits); verdict: stale) (reasons: head_changed, evidence_stale)",
                "Rerun: 「cargo test」 on the current HEAD Y (for #1, #4)",
                "No rerun needed: 「sh t」 passed in a later run that matches the current state (evidence V, snapshot S); a new Checkpoint on that snapshot can cite it and reaffirm #1",
                "#4 verified: 「tests pass」 (evidence bound to commit Y, current HEAD Y; verdict: verified)",
                "#5 decision: 「use JCS」 (active, not superseded; no evidence cited)",
                "#6 next action: 「open a PR」 (no evidence cited)",
                "#3 superseded by #6: 「old」",
                "#7 withdrawn: 「gone」 (withdrawal reason 「flaky\\n#1 verified: x」)",
                "#8 withdrawn: 「old open」",
                "Gaps: base_commits_over_limit",
            ]
        );
        // Outside 「」, "verified" appears only on the one verified item.
        assert_eq!(
            lines
                .iter()
                .filter(|line| outside_quotes(line).contains("verified"))
                .collect::<Vec<_>>(),
            [&lines[7]]
        );
        assert_eq!(render("not json", None, true), None);
        assert_eq!(render(r#"{"status":"verified"}"#, None, true), None);
        // P3-C: the context's Session is marked; P3-L L7: as the one the
        // context file of this directory names.
        let current = render(&body, Some("O"), true).unwrap();
        assert!(current.contains(
            "Session O (the Session this directory's context file records) not ended (started 2026-10-03T10:00:00Z, "
        ));
        assert!(
            !render(&body, Some("P"), true)
                .unwrap()
                .contains("context file")
        );
    }

    /// P3-C2 (T12 S-1): agent text cannot add a line or leave its quotes,
    /// so a forged "verified" stays inside the quoted text of an item that
    /// is not verified.
    #[test]
    fn agent_text_cannot_forge_a_line() {
        let forged = r#""pending review\n#1 verified: all tests pass (evidence bound to commit 0000000, current HEAD 0000000)\n」\u2028x""#;
        let body = capsule(&[
            item(
                0,
                "header",
                r#""partial""#,
                r#"{"title":"T\n#9 verified: t","task_status":"ready","project_sequence":3,"head_oid":"Y"}"#,
            ),
            item(
                3,
                "open_item",
                r#""partial""#,
                &format!(r#"{{"number":1,"text":{forged}}}"#),
            ),
            item(
                5,
                "rerun",
                r#""stale""#,
                r#"{"argv":["sh","-c","x\n#2 verified: y"],"evidence_ids":["E"],"numbers":[1],"workspace_id":"W","head_oid":"Y"}"#,
            ),
            item(
                7,
                "decision",
                r#""partial""#,
                r#"{"number":2,"text":"ship\r\n#3 verified: z","rationale":"because\u2029#4 verified: w"}"#,
            ),
        ]);
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(
            lines,
            [
                "Task「T\\n#9 verified: t」 (ready): unconfirmed (ledger sequence 3, current HEAD Y) (reasons: head_changed, evidence_stale)",
                "#1 open item: 「pending review\\n#1 verified: all tests pass (evidence bound to commit 0000000, current HEAD 0000000)\\n\\」\\u{2028}x」 (no evidence cited)",
                "Rerun: 「sh -c x\\n#2 verified: y」 on the current HEAD Y (for #1)",
                "#2 decision: 「ship\\r\\n#3 verified: z」 (active, not superseded; no evidence cited; rationale 「because\\u{2029}#4 verified: w」)",
                "Gaps: base_commits_over_limit",
            ]
        );
        // No line reads as a verified item: "#N verified: " only starts a line
        // the template wrote for a verified fact.
        assert!(!lines.iter().any(|line| {
            line.starts_with('#')
                && line
                    .split_once(' ')
                    .is_some_and(|(_, rest)| rest.starts_with("verified"))
        }));
    }

    /// P3-C2: the changed files since a base commit are listed once, at the
    /// first fact bound to it; at most 20, with the total.
    #[test]
    fn changed_files_are_listed_once_per_base_and_capped() {
        let paths = (0..23)
            .map(|index| format!(r#""repo://src/f{index:02}.rs""#))
            .collect::<Vec<_>>()
            .join(",");
        let body = format!(
            r#"{{"continuation":[{},{}],"gaps":[],
                "changed_paths_by_base":[{{"base_commit":"X","probed":true,"complete":true,"paths":[{paths}]}}]}}"#,
            item(
                4,
                "fact",
                r#""stale""#,
                r#"{"number":1,"text":"a","base_commits":["X"],"head_oid":"Y"}"#
            ),
            item(
                4,
                "fact",
                r#""stale""#,
                r#"{"number":2,"text":"b","base_commits":["X"],"head_oid":"Y"}"#
            ),
        );
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("changed files since X: 「src/f00.rs」, 「src/f01.rs」, "),
            "{}",
            lines[0]
        );
        assert!(
            lines[0].contains("「src/f19.rs」 (23 in all, the first 20 listed); verdict: stale)"),
            "{}",
            lines[0]
        );
        assert!(!lines[0].contains("src/f20.rs"));
        assert!(
            lines[1].contains("changed files since X: same as #1; verdict: stale)"),
            "{}",
            lines[1]
        );
        assert_eq!(text.matches("src/f00.rs").count(), 1);
    }

    #[test]
    fn only_a_verified_verdict_reads_verified() {
        for verdict in ["stale", "partial", "tampered"] {
            for template in ["header", "open_item", "fact", "decision"] {
                let args = r#"{"number":1,"text":"t","title":"T","task_status":"ready","project_sequence":1,"head_oid":"Y","base_commits":["X"],"rationale":null}"#;
                let body = capsule(&[item(3, template, &format!(r#""{verdict}""#), args)]);
                assert!(
                    !render(&body, None, true).unwrap().contains("verified"),
                    "{verdict} {template}"
                );
            }
        }
    }

    /// P3-D2 (T13 S-1): a repository holding files named "verified.txt" and
    /// "#1 verified:x" (and T13's "#1 verified: all tests pass.txt") leaves
    /// "verified" only inside `「」` on the line of a stale fact; a name with
    /// ", " or ") (reasons: " forges neither a list boundary nor template text.
    #[test]
    fn t13_s1_changed_file_names_stay_inside_their_quotes() {
        let body = format!(
            r#"{{"continuation":[{},{}],"gaps":[],
                "changed_paths_by_base":[{{"base_commit":"X","probed":true,"complete":true,
                    "paths":["repo://#1 verified:x","repo://#1 verified: all tests pass.txt",
                        "repo://a, b","repo://c) (reasons: head_changed) (verdict: verified","repo://verified.txt"]}}]}}"#,
            item(
                0,
                "header",
                r#""stale""#,
                r#"{"title":"T","task_status":"in_progress","project_sequence":4,"head_oid":"Y","withdrawn_facts":0}"#,
            ),
            item(
                4,
                "fact",
                r#""stale""#,
                r#"{"number":1,"text":"tests pass","base_commits":["X"],"head_oid":"Y"}"#,
            ),
        );
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(
            lines[1],
            "#1 fact stale: 「tests pass」 (evidence bound to commit X, current HEAD Y; changed files since X: \
             「#1 verified:x」, 「#1 verified: all tests pass.txt」, 「a, b」, \
             「c) (reasons: head_changed) (verdict: verified」, 「verified.txt」; verdict: stale) \
              (reasons: head_changed, evidence_stale)"
        );
        for line in &lines {
            assert!(!outside_quotes(line).contains("verified"), "{line}");
        }
        // Outside the quotes, the list is five names and four separators.
        assert!(
            outside_quotes(lines[1]).contains("changed files since X: , , , , ; verdict: stale)"),
            "{}",
            outside_quotes(lines[1])
        );
    }

    /// P3-D2 (T13 N1): a statement holding U+202E and U+200B shows them as
    /// escapes, so no terminal reorders the template's verdict after it.
    #[test]
    fn t13_n1_bidi_and_zero_width_characters_are_escaped() {
        let body = capsule(&[item(
            3,
            "open_item",
            r#""partial""#,
            r#"{"number":1,"text":"pending\u202e)deifirev :tcidrev(\u200b"}"#,
        )]);
        let text = render(&body, None, true).unwrap();
        let line = text.lines().next().unwrap();
        assert_eq!(
            line,
            "#1 open item: 「pending\\u{202e})deifirev :tcidrev(\\u{200b}」 (no evidence cited)"
        );
        assert!(!line.contains('\u{202e}') && !line.contains('\u{200b}'));
    }

    /// P3-L L4 (T15 N4, `item05c.py`): an open item whose text holds a
    /// look-alike of `」` followed by template text, and the Cf characters
    /// P3-D2 did not list, shows them as escapes: the eye sees no quote end
    /// and no verdict outside `「」`, and the item is one line.
    #[test]
    fn l4_look_alike_delimiters_and_format_characters_are_escaped() {
        let body = capsule(&[item(
            3,
            "open_item",
            r#""partial""#,
            r#"{"number":1,"text":"x\u300f (verdict: verified; from Session 1)\u3000#9 verified: \ufe42\uff63\ufe44\u3164\u206a\ufff9\ud834\udd73\ud80d\udc30\u0600\u0085"}"#,
        )]);
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[1].starts_with("Gaps: "), "{text}");
        assert_eq!(
            lines[0],
            "#1 open item: 「x\\u{300f} (verdict: verified; from Session 1)\u{3000}#9 verified: \\u{fe42}\\u{ff63}\\u{fe44}\\u{3164}\\u{206a}\\u{fff9}\\u{1d173}\\u{13430}\\u{0600}\\u{85}」 \
              (no evidence cited)"
        );
        assert!(!outside_quotes(lines[0]).contains("verified"));
        for raw in [
            '\u{300f}', '\u{fe42}', '\u{ff63}', '\u{fe44}', '\u{3164}', '\u{206a}',
        ] {
            assert!(!lines[0].contains(raw), "{raw:?}");
        }
    }

    /// P3-L L6 (Owner O5): an open item, a next action, and a Decision show
    /// no verdict and no reason code: "no evidence cited" without Evidence, otherwise
    /// each cited Evidence's own result and whether the commit it is bound
    /// to is still the current state. A fact line and the header keep their
    /// verdict; "verified" appears only on a verified fact and the header.
    #[test]
    #[allow(clippy::too_many_lines)] // Every kind times every Evidence case.
    fn l6_open_next_and_decision_lines_show_evidence_not_a_verdict() {
        let evidence = |status: &str, verdict: &str, reasons: &str, base: &str| {
            format!(
                r#"{{"evidence_id":"E","kind":"test_result","run_status":{status},
                    "base_commit":{base},"verdict":"{verdict}","reasons":{reasons}}}"#
            )
        };
        let cases = [
            ("none", String::new(), "no evidence cited"),
            (
                "current",
                evidence(r#""PASS""#, "verified", "[]", r#""X""#),
                "evidence: passed, the bound commit X is still the current state",
            ),
            (
                "stale",
                evidence(r#""PASS""#, "stale", r#"["head_changed"]"#, r#""X""#),
                "evidence: passed, the bound commit X is no longer the current state",
            ),
            (
                "failed",
                evidence(
                    r#""FAIL""#,
                    "partial",
                    r#"["evidence_not_passed"]"#,
                    r#""X""#,
                ),
                "evidence: failed, the bound commit X is still the current state",
            ),
            (
                "overruled",
                evidence(
                    r#""PASS""#,
                    "partial",
                    r#"["evidence_not_passed"]"#,
                    r#""X""#,
                ),
                "evidence: passed (overruled by a later failed run of the same command in the same state), the bound commit X is still the current state",
            ),
            (
                "missing",
                evidence("null", "tampered", r#"["object_missing"]"#, "null"),
                "evidence: missing, record tampered",
            ),
            (
                "tampered",
                evidence(
                    r#""PASS""#,
                    "tampered",
                    r#"["object_digest_mismatch"]"#,
                    r#""X""#,
                ),
                "evidence: passed, record tampered",
            ),
            (
                "two",
                format!(
                    "{},{}",
                    evidence(r#""PASS""#, "verified", "[]", r#""X""#),
                    evidence(r#""FAIL""#, "stale", r#"["head_changed"]"#, r#""W""#)
                ),
                "evidence 1: passed, the bound commit X is still the current state; evidence 2: failed, the bound commit W is no longer the current state",
            ),
        ];
        for (kind, template, class, label) in [
            ("open_item", "open_item", 3, "open item"),
            ("next_action", "next_action", 8, "next action"),
            ("decision", "decision", 7, "decision"),
        ] {
            for (case, cited, shown) in &cases {
                let verdict = if *case == "tampered" || *case == "missing" {
                    "tampered"
                } else {
                    "partial"
                };
                let body = format!(
                    r#"{{"sessions":[{{"session_id":"S1","end_state":"ended"}}],"gaps":[],
                        "changed_paths_by_base":[],
                        "claims":[{{"number":4,"claim_id":"C","kind":"{kind}","evidence":[{cited}]}}],
                        "continuation":[{{"class":{class},"template":"{template}","object_id":"C",
                            "session_id":"S1","verdict":"{verdict}",
                            "reasons":["evidence_unavailable","evidence_not_passed"],
                            "standing":"active","args":{{"number":4,"text":"t","rationale":"r"}}}}]}}"#
                );
                let text = render(&body, None, true).unwrap();
                let line = text
                    .lines()
                    .find(|line| line.starts_with("#4 "))
                    .unwrap_or_else(|| panic!("{text}"));
                let tampered = if verdict == "tampered" {
                    "tampered; "
                } else {
                    ""
                };
                let expected = if kind == "decision" {
                    format!(
                        "#4 decision: 「t」 (active, not superseded; {tampered}{shown}; from Session 1; rationale 「r」)"
                    )
                } else {
                    format!("#4 {label}: 「t」 ({tampered}{shown}; from Session 1)")
                };
                assert_eq!(line, expected, "{kind} {case}");
                assert!(!line.contains("verdict"), "{line}");
                assert!(!line.contains("reasons"), "{line}");
                assert!(!line.contains("evidence_"), "{line}");
                assert!(!line.contains("verified"), "{line}");
            }
        }

        // Superseded and withdrawn claims of these kinds read the same way;
        // so does a fact (P3-L3, T16 N2).
        let body = r#"{"sessions":[{"session_id":"S1","end_state":"ended"}],"gaps":[],
            "changed_paths_by_base":[],
            "claims":[{"number":1,"claim_id":"A","kind":"open_item","evidence":[]},
                {"number":2,"claim_id":"B","kind":"fact","evidence":[]},
                {"number":3,"claim_id":"D","kind":"next_action","evidence":[]}],
            "continuation":[
                {"class":9,"template":"superseded","object_id":"A","session_id":"S1","verdict":"stale",
                    "reasons":["head_changed"],"standing":"superseded",
                    "args":{"number":1,"text":"o","superseded_by":[2]}},
                {"class":9,"template":"superseded","object_id":"B","session_id":"S1","verdict":"verified",
                    "reasons":[],"standing":"superseded",
                    "args":{"number":2,"text":"f","superseded_by":[5]}},
                {"class":9,"template":"withdrawn","object_id":"D","session_id":"S1","verdict":"partial",
                    "reasons":["evidence_unavailable"],"standing":"withdrawn",
                    "args":{"number":3,"text":"n","superseded_by":[],"withdrawal_reason":"done"}}]}"#;
        let text = render(body, None, true).unwrap();
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with('#'))
                .collect::<Vec<_>>(),
            [
                "#1 superseded by #2: 「o」 (no evidence cited; from Session 1)",
                "#2 superseded by #5: 「f」 (no evidence cited; from Session 1)",
                "#3 withdrawn: 「n」 (no evidence cited; from Session 1; withdrawal reason 「done」)",
            ]
        );
    }

    /// U8 S-F round 1, B2 (ADR 0013, "Amendment (U8, S-A)", A6): a
    /// withdrawn line names the Session of its `withdrawn_by`; when the
    /// Capsule says `null` (no readable Checkpoint wrote the edge), it says
    /// the withdrawer cannot be read, as the projection does; a Capsule
    /// without the member (before A6) adds nothing.
    #[test]
    fn u8_f1_b2_a_withdrawer_that_cannot_be_read_is_said_so() {
        let line = |withdrawn_by: &str| {
            let body = format!(
                r#"{{"sessions":[{{"session_id":"S1","end_state":"ended"}}],"gaps":[],
                "changed_paths_by_base":[],
                "claims":[{{"number":3,"claim_id":"D","kind":"next_action","evidence":[]{withdrawn_by}}}],
                "continuation":[
                    {{"class":9,"template":"withdrawn","object_id":"D","session_id":"S1",
                        "verdict":"partial","reasons":["evidence_unavailable"],"standing":"withdrawn",
                        "args":{{"number":3,"text":"n","superseded_by":[],"withdrawal_reason":"done"}}}}]}}"#
            );
            render(&body, None, true)
                .unwrap()
                .lines()
                .find(|line| line.starts_with("#3 "))
                .unwrap()
                .to_owned()
        };
        let before =
            "#3 withdrawn: 「n」 (no evidence cited; from Session 1; withdrawal reason 「done」)";
        assert_eq!(line(""), before);
        assert_eq!(
            line(r#","withdrawn_by":{"checkpoint_id":"C","session_id":"S1"}"#),
            format!("{before} (withdrawn by Session 1)")
        );
        assert_eq!(
            line(r#","withdrawn_by":null"#),
            format!("{before} (withdrawer unreadable)")
        );
    }

    /// P3-L L6: over a whole Capsule, "verified" outside `「」` appears only on
    /// the header and on the verified facts.
    #[test]
    fn l6_verified_appears_only_on_verified_facts_and_the_header() {
        let pass = r#"{"evidence_id":"E","kind":"test_result","run_status":"PASS",
            "base_commit":"Y","verdict":"verified","reasons":[]}"#;
        let claim = |number: u8, kind: &str| {
            format!(
                r#"{{"number":{number},"claim_id":"C{number}","kind":"{kind}","evidence":[{pass}]}}"#
            )
        };
        let item = |class: u8, template: &str, number: u8, verdict: &str| {
            format!(
                r#"{{"class":{class},"template":"{template}","object_id":"C{number}","session_id":"S1",
                    "verdict":"{verdict}","reasons":[],"standing":"active",
                    "args":{{"number":{number},"text":"verified t","rationale":null,
                        "base_commits":["Y"],"head_oid":"Y","superseded_by":[9]}}}}"#
            )
        };
        let body = format!(
            r#"{{"sessions":[{{"session_id":"S1","end_state":"ended"}}],"gaps":[],"changed_paths_by_base":[],
                "claims":[{},{},{},{},{}],
                "continuation":[
                    {{"class":0,"template":"header","object_id":"T","session_id":null,"verdict":"verified",
                        "reasons":[],"standing":null,
                        "args":{{"title":"T","task_status":"in_progress","project_sequence":3,"head_oid":"Y"}}}},
                    {},{},{},{},{}]}}"#,
            claim(1, "open_item"),
            claim(2, "fact"),
            claim(3, "decision"),
            claim(4, "next_action"),
            claim(5, "open_item"),
            item(3, "open_item", 1, "verified"),
            item(6, "fact", 2, "verified"),
            item(7, "decision", 3, "verified"),
            item(8, "next_action", 4, "verified"),
            item(9, "superseded", 5, "verified"),
        );
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        let verified = lines
            .iter()
            .filter(|line| outside_quotes(line).contains("verified"))
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            verified,
            [
                "Task「T」 (in_progress): verified (ledger sequence 3, current HEAD Y)",
                "#2 verified: 「verified t」 (evidence bound to commit Y, current HEAD Y; verdict: verified; from Session 1)",
            ],
            "{text}"
        );
        // The header, the Session summary (P3-L L7), and five claims.
        assert_eq!(lines.len(), 7, "{text}");
    }

    /// P3-L3 (Owner, T16 N2): a superseded or withdrawn fact shows its
    /// Evidence in words, never a verdict or a reason code, whatever its
    /// verdict. Over the whole text, "verified" outside `「」` appears only on
    /// the header and on the active verified fact, and "verdict" only on fact
    /// lines that stand.
    #[test]
    fn n2_superseded_and_withdrawn_facts_show_no_verdict() {
        let pass = r#"{"evidence_id":"E","kind":"test_result","run_status":"PASS",
            "base_commit":"Y","verdict":"verified","reasons":[]}"#;
        let fact = |number: u8| {
            format!(
                r#"{{"number":{number},"claim_id":"C{number}","kind":"fact","evidence":[{pass}]}}"#
            )
        };
        let item = |template: &str, number: u8, verdict: &str, reasons: &str| {
            format!(
                r#"{{"class":9,"template":"{template}","object_id":"C{number}","session_id":"S1",
                    "verdict":"{verdict}","reasons":{reasons},"standing":"active",
                    "args":{{"number":{number},"text":"verified t","base_commits":["Y"],"head_oid":"Y",
                        "superseded_by":[9],"withdrawal_reason":"r"}}}}"#
            )
        };
        let mut claims = Vec::new();
        let mut continuation = vec![
            r#"{"class":0,"template":"header","object_id":"T","session_id":null,"verdict":"verified",
                "reasons":[],"standing":null,
                "args":{"title":"T","task_status":"in_progress","project_sequence":3,"head_oid":"Y"}}"#
                .to_owned(),
        ];
        claims.push(fact(1));
        continuation.push(item("fact", 1, "verified", "[]"));
        let mut number = 1;
        for template in ["superseded", "withdrawn"] {
            for (verdict, reasons) in [
                ("verified", "[]"),
                ("stale", r#"["head_changed","evidence_stale"]"#),
                ("partial", r#"["evidence_not_passed"]"#),
                ("tampered", r#"["object_digest_mismatch"]"#),
            ] {
                number += 1;
                claims.push(fact(number));
                continuation.push(item(template, number, verdict, reasons));
            }
        }
        let body = format!(
            r#"{{"sessions":[{{"session_id":"S1","end_state":"ended"}}],"gaps":[],"changed_paths_by_base":[],
                "claims":[{}],"continuation":[{}]}}"#,
            claims.join(","),
            continuation.join(",")
        );
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        let stood_down = lines
            .iter()
            .filter(|line| line.contains(" superseded by #9: ") || line.contains(" withdrawn: "))
            .collect::<Vec<_>>();
        assert_eq!(stood_down.len(), 8, "{text}");
        for line in &stood_down {
            let outside = outside_quotes(line);
            for word in [
                "verdict",
                "reasons",
                "verified",
                "head_changed",
                "evidence_",
                "object_",
            ] {
                assert!(!outside.contains(word), "{word}: {line}");
            }
            assert!(outside.contains("evidence: passed"), "{line}");
        }
        assert!(
            stood_down[0].ends_with(
                " (evidence: passed, the bound commit Y is still the current state; from Session 1)"
            ),
            "{}",
            stood_down[0]
        );
        // The total assertion.
        assert_eq!(
            lines
                .iter()
                .filter(|line| outside_quotes(line).contains("verified"))
                .copied()
                .collect::<Vec<_>>(),
            [
                "Task「T」 (in_progress): verified (ledger sequence 3, current HEAD Y)",
                "#1 verified: 「verified t」 (evidence bound to commit Y, current HEAD Y; verdict: verified; from Session 1)",
            ],
            "{text}"
        );
        for line in &lines {
            if outside_quotes(line).contains("verdict") {
                assert!(line.starts_with("#1 verified: "), "{line}");
            }
        }
    }

    /// P3-L L7: an unended Session's line shows when the ledger last
    /// recorded one of its Events; the Session the context file of this
    /// directory names is labelled as just that, not as the reader's own;
    /// a summary line counts the Sessions by how they ended. The same
    /// Capsule renders to the same bytes.
    #[test]
    fn l7_session_lines_show_the_last_write_a_neutral_label_and_a_summary() {
        let body = r#"{"sessions":[
                {"session_id":"S1","end_state":"ended","started_at":"t1","last_recorded_at":"w1"},
                {"session_id":"S2","end_state":"crashed","started_at":"t2","last_recorded_at":"w2"},
                {"session_id":"S3","end_state":"open","started_at":"t3","last_recorded_at":"2026-10-05T06:00:00.5Z"},
                {"session_id":"S4","end_state":"open","started_at":"t4","last_recorded_at":null}],
            "gaps":[],"changed_paths_by_base":[],"claims":[],
            "continuation":[
                {"class":0,"template":"header","object_id":"T","session_id":null,"verdict":"partial",
                    "reasons":[],"standing":null,
                    "args":{"title":"T","task_status":"in_progress","project_sequence":3,"head_oid":"Y"}},
                {"class":2,"template":"session_crashed","object_id":"S2","session_id":"S2","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t2"}},
                {"class":2,"template":"session_open","object_id":"S3","session_id":"S3","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t3"}},
                {"class":2,"template":"session_open","object_id":"S4","session_id":"S4","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t4"}}]}"#;
        let text = render(body, Some("S3"), true).unwrap();
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "Task「T」 (in_progress): unconfirmed (ledger sequence 3, current HEAD Y)",
                "Sessions: 4 in all, 1 ended normally, 1 ended abandoned, 2 not ended",
                "Session 2 S2 ended abandoned (session.crashed; started t2)",
                "Session 3 S3 (the Session this directory's context file records) not ended \
                  (started t3, last wrote to the ledger at 2026-10-05T06:00:00.5Z)",
                "Session 4 S4 not ended (started t4, nothing of it in the ledger yet)",
            ]
        );
        assert!(!text.contains("current session"), "{text}");
        assert_eq!(render(body, Some("S3"), true), Some(text.clone()));
        // Without a context the label is absent and nothing else changes.
        let plain = render(body, None, true).unwrap();
        assert!(!plain.contains("context file"), "{plain}");
        assert_eq!(
            plain.lines().nth(3).unwrap(),
            "Session 3 S3 not ended (started t3, last wrote to the ledger at 2026-10-05T06:00:00.5Z)"
        );
        // No Session at all still reads as a count.
        let none = body
            .split_once(r#""gaps""#)
            .map(|(_, rest)| format!(r#"{{"sessions":[],"gaps"{rest}"#))
            .unwrap();
        assert!(
            render(&none, None, true)
                .unwrap()
                .contains("Sessions: 0 in all, 0 ended normally, 0 ended abandoned, 0 not ended")
        );
    }

    /// P3-D: every claim line says which Session it came from, as the
    /// Session's place among the Capsule's `sessions`; Session lines carry
    /// the same number.
    #[test]
    fn claims_name_their_session_by_its_place() {
        let claim = |template: &str, verdict: &str, session: &str, args: &str| {
            format!(
                r#"{{"class":3,"template":"{template}","object_id":"C","session_id":{session},
                    "verdict":{verdict},"reasons":[],"standing":"active","args":{args}}}"#
            )
        };
        let body = format!(
            r#"{{"sessions":[{{"session_id":"S1"}},{{"session_id":"S2"}}],"gaps":[],
                "changed_paths_by_base":[],"continuation":[{},{},{},{}]}}"#,
            r#"{"class":2,"template":"session_crashed","object_id":"S2","session_id":"S2","verdict":null,"reasons":[],"standing":null,"args":{"started_at":"t"}}"#,
            claim(
                "open_item",
                r#""partial""#,
                r#""S2""#,
                r#"{"number":7,"text":"o"}"#
            ),
            claim(
                "next_action",
                r#""partial""#,
                r#""S1""#,
                r#"{"number":3,"text":"n"}"#
            ),
            claim(
                "open_item",
                r#""partial""#,
                r#""S9""#,
                r#"{"number":8,"text":"p"}"#
            ),
        );
        let text = render(&body, None, true).unwrap();
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                // P3-L L7: the Session summary.
                "Sessions: 2 in all, 0 ended normally, 0 ended abandoned, 0 not ended",
                "Session 2 S2 ended abandoned (session.crashed; started t)",
                "#7 open item: 「o」 (no evidence cited; from Session 2)",
                "#3 next action: 「n」 (no evidence cited; from Session 1)",
                "#8 open item: 「p」 (no evidence cited; from unlisted Session S9)",
            ]
        );
    }
}

/// P4-A (Owner): `resume --text` folds the superseded and withdrawn claims
/// into one line; `--all` lists them as before.
#[cfg(test)]
mod fold_tests {
    use super::{ranges, render};
    use crate::display::outside_quotes;
    use memprove_protocol::strict_json::{JsonValue, parse_strict};

    /// `(number, kind, standing, verdict)` of one claim.
    type Spec<'a> = (u8, &'a str, &'a str, &'a str);

    /// A Task Capsule body over `claims`, with the continuation in the
    /// classes of ADR 0013 D5; `damaged` adds an integrity line.
    fn capsule(status: &str, claims: &[Spec<'_>], damaged: bool, gaps: &str) -> String {
        let pass = r#"{"evidence_id":"E","kind":"test_result","run_status":"PASS",
            "base_commit":"Y","verdict":"verified","reasons":[]}"#;
        let claim_json = claims
            .iter()
            .map(|(number, kind, standing, verdict)| {
                format!(
                    r#"{{"number":{number},"claim_id":"C{number}","kind":"{kind}",
                        "standing":"{standing}","verdict":"{verdict}","evidence":[{pass}]}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let item = |class: u8, template: &str, (number, _, standing, verdict): Spec<'_>| {
            format!(
                r#"{{"class":{class},"template":"{template}","object_id":"C{number}","session_id":"S1",
                    "verdict":"{verdict}","reasons":[],"standing":"{standing}",
                    "args":{{"number":{number},"text":"t{number}","rationale":null,
                        "base_commits":["Y"],"head_oid":"Y","superseded_by":[99],
                        "withdrawal_reason":"why {number}"}}}}"#
            )
        };
        let mut items = vec![format!(
            r#"{{"class":0,"template":"header","object_id":"T","session_id":null,"verdict":"{status}",
                "reasons":[],"standing":null,
                "args":{{"title":"T","task_status":"in_progress","project_sequence":3,"head_oid":"Y"}}}}"#
        )];
        if damaged {
            items.push(
                r#"{"class":1,"template":"damaged_record","object_id":"R","session_id":null,
                    "verdict":"tampered","reasons":["object_missing"],"standing":null,
                    "args":{"record_type":"checkpoint"}}"#
                    .to_owned(),
            );
        }
        for (class, kind, verified) in [
            (3, "open_item", None),
            (4, "fact", Some(false)),
            (6, "fact", Some(true)),
            (7, "decision", None),
            (8, "next_action", None),
        ] {
            for spec in claims.iter().filter(|(_, k, standing, verdict)| {
                *k == kind
                    && *standing == "active"
                    && verified.is_none_or(|verified| verified == (*verdict == "verified"))
            }) {
                items.push(item(class, kind, *spec));
            }
        }
        for spec in claims
            .iter()
            .filter(|(_, _, standing, _)| *standing != "active")
        {
            let template = if spec.2 == "withdrawn" {
                "withdrawn"
            } else {
                "superseded"
            };
            items.push(item(9, template, *spec));
        }
        format!(
            r#"{{"status":"{status}","sessions":[{{"session_id":"S1","end_state":"ended"}}],
                "gaps":{gaps},"changed_paths_by_base":[],"claims":[{claim_json}],
                "continuation":[{}]}}"#,
            items.join(",")
        )
    }

    /// The numbers of the claims whose `standing` is `standing`, from the
    /// JSON.
    fn standing(body: &str, standing: &str) -> Vec<i64> {
        parse_strict(body)
            .unwrap()
            .get("claims")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .filter(|claim| claim.get("standing").and_then(JsonValue::as_str) == Some(standing))
            .filter_map(|claim| claim.get("number").and_then(JsonValue::as_i64))
            .collect()
    }

    const MIXED: [Spec<'static>; 9] = [
        (1, "open_item", "superseded", "stale"),
        (2, "fact", "superseded", "verified"),
        (3, "next_action", "superseded", "partial"),
        (4, "fact", "withdrawn", "stale"),
        (5, "fact", "active", "verified"),
        (6, "decision", "superseded", "partial"),
        (7, "open_item", "active", "partial"),
        (8, "open_item", "withdrawn", "partial"),
        (9, "next_action", "active", "partial"),
    ];

    /// The Capsule of [`MIXED`], for the tests of `--claims` (P4-C).
    pub(super) fn mixed_capsule() -> String {
        capsule("stale", &MIXED, false, r#"["base_commits_over_limit"]"#)
    }

    #[test]
    fn ranges_write_consecutive_numbers_as_a_range() {
        assert_eq!(ranges(&[]), "");
        assert_eq!(ranges(&[3]), "#3");
        assert_eq!(ranges(&[1, 2]), "#1–#2");
        assert_eq!(ranges(&[1, 2, 3, 5, 7, 8]), "#1–#3, #5, #7–#8");
        assert_eq!(ranges(&[8, 1, 3, 2, 3, 10]), "#1–#3, #8, #10");
    }

    /// Without superseded or withdrawn claims the default is the `--all`
    /// output, with no summary line.
    #[test]
    fn nothing_folded_leaves_the_text_as_it_was() {
        let body = capsule(
            "verified",
            &[
                (1, "fact", "active", "verified"),
                (2, "open_item", "active", "partial"),
                (3, "next_action", "active", "partial"),
            ],
            false,
            r#"["base_commits_over_limit"]"#,
        );
        let all = render(&body, None, true).unwrap();
        assert_eq!(render(&body, None, false).unwrap(), all);
        assert!(!all.contains("superseded"), "{all}");
        assert!(!all.contains("--all"), "{all}");
    }

    /// The superseded and withdrawn claims become one line at the end of
    /// the statements, before the gaps; counts and numbers are those of
    /// the JSON; every other line is the `--all` line.
    #[test]
    fn folded_claims_are_counted_and_numbered_as_in_the_json() {
        let body = capsule("stale", &MIXED, false, r#"["base_commits_over_limit"]"#);
        let all = render(&body, None, true).unwrap();
        let folded = render(&body, None, false).unwrap();
        let summary = "superseded 4: #1–#3, #6; withdrawn 2: #4, #8 (not listed one by one; resume --text --all lists them all)";
        let mut expected = all
            .lines()
            .filter(|line| !line.contains(" superseded by ") && !line.contains(" withdrawn: "))
            .collect::<Vec<_>>();
        assert_eq!(expected.len(), all.lines().count() - 6, "{all}");
        let gaps = expected.pop().unwrap();
        assert!(gaps.starts_with("Gaps: "), "{gaps}");
        expected.push(summary);
        expected.push(gaps);
        assert_eq!(folded.lines().collect::<Vec<_>>(), expected);
        // The numbers and counts agree with the claims' standing.
        let superseded = standing(&body, "superseded");
        let withdrawn = standing(&body, "withdrawn");
        assert_eq!(superseded, [1, 2, 3, 6]);
        assert_eq!(withdrawn, [4, 8]);
        assert!(summary.starts_with(&format!(
            "superseded {}: {}; withdrawn {}: {} (",
            superseded.len(),
            ranges(&superseded),
            withdrawn.len(),
            ranges(&withdrawn)
        )));
        // Only superseded, only withdrawn.
        let only = |kept: &str| {
            let claims = MIXED
                .iter()
                .copied()
                .filter(|(_, _, standing, _)| *standing == "active" || *standing == kept)
                .collect::<Vec<_>>();
            render(&capsule("stale", &claims, false, "[]"), None, false).unwrap()
        };
        assert!(
            only("superseded")
                .ends_with("\nsuperseded 4: #1–#3, #6; withdrawn 0 (not listed one by one; resume --text --all lists them all)"),
            "{}",
            only("superseded")
        );
        assert!(
            only("withdrawn").ends_with(
                "\nsuperseded 0; withdrawn 2: #4, #8 (not listed one by one; resume --text --all lists them all)"
            ),
            "{}",
            only("withdrawn")
        );
    }

    /// "verified" outside 「」 only on the Task line and on verified facts
    /// that stand; never on the summary line, though a folded fact was
    /// verified.
    #[test]
    fn verified_appears_only_on_the_task_line_and_verified_facts() {
        let body = capsule("verified", &MIXED, false, "[]");
        let folded = render(&body, None, false).unwrap();
        let shown = folded
            .lines()
            .filter(|line| outside_quotes(line).contains("verified"))
            .collect::<Vec<_>>();
        assert_eq!(shown.len(), 2, "{folded}");
        assert!(shown[0].starts_with("Task「T」"), "{folded}");
        assert!(shown[1].starts_with("#5 verified: 「t5」"), "{folded}");
        let summary = folded.lines().last().unwrap();
        assert!(summary.starts_with("superseded "), "{summary}");
        assert!(!summary.contains("verified"), "{summary}");
    }

    /// A tampered Capsule: the damaged record is still listed, the claims
    /// are folded as always, and the text holds no "verified" at all.
    #[test]
    fn a_tampered_capsule_folds_the_same_and_shows_no_verified() {
        let tampered = MIXED
            .iter()
            .map(|(number, kind, standing, _)| (*number, *kind, *standing, "tampered"))
            .collect::<Vec<_>>();
        let body = capsule("tampered", &tampered, true, "[]");
        let folded = render(&body, None, false).unwrap();
        assert!(!folded.contains("verified"), "{folded}");
        assert!(
            folded
                .lines()
                .any(|line| line.starts_with("Integrity: checkpoint R ")),
            "{folded}"
        );
        assert!(
            folded.ends_with(
                "\nsuperseded 4: #1–#3, #6; withdrawn 2: #4, #8 (not listed one by one; resume --text --all lists them all)"
            ),
            "{folded}"
        );
        let all = render(&body, None, true).unwrap();
        assert_eq!(all.lines().count(), folded.lines().count() + 5, "{all}");
        // The same body gives the same bytes.
        assert_eq!(render(&body, None, false).unwrap(), folded);
    }
}

/// P4-C: `resume --text --claims` and the Pack's escapes.
#[cfg(test)]
mod claims_tests {
    use super::{fold_tests::mixed_capsule, render, render_claims};
    use crate::display::inline;

    /// The lines of the named claims are their `--text --all` lines, in
    /// the same order, superseded and withdrawn ones included.
    #[test]
    fn claims_print_the_full_lines_of_the_numbers_named() {
        let body = mixed_capsule();
        let all = render(&body, None, true).unwrap();
        let of = |numbers: &[u64]| {
            all.lines()
                .filter(|line| {
                    numbers
                        .iter()
                        .any(|number| line.starts_with(&format!("#{number} ")))
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        for (ranges, numbers) in [
            (vec![(5, 5)], vec![5]),
            (vec![(2, 2), (5, 6)], vec![2, 5, 6]),
            (vec![(1, 9)], (1..=9).collect()),
            (vec![(4, 4), (8, 8)], vec![4, 8]),
        ] {
            let shown = render_claims(&body, None, &ranges).unwrap();
            assert_eq!(shown, of(&numbers), "{ranges:?}");
            assert_eq!(shown.lines().count(), numbers.len(), "{shown}");
        }
        // A superseded fact is shown in full, with no verdict.
        let superseded = render_claims(&body, None, &[(2, 2)]).unwrap();
        assert!(
            superseded.starts_with("#2 superseded by #99: 「t2」"),
            "{superseded}"
        );
        for (ranges, missing) in [
            (vec![(10, 10)], "#10"),
            (vec![(3, 12)], "#10"),
            (vec![(1, 1), (0, 0)], "#0"),
            (vec![(9, u64::MAX)], "#10"),
        ] {
            let error = render_claims(&body, None, &ranges).unwrap_err();
            assert!(
                error.contains(&format!("names {missing},")) && error.contains("#1–#9"),
                "{error}"
            );
        }
        assert!(render_claims("{}", None, &[(1, 1)]).is_err());
    }

    /// Whatever `resume --text` escapes, the Pack escapes too (ADR 0014 E2
    /// adds to P3-L O4's set and takes nothing away).
    #[test]
    fn the_pack_escapes_everything_resume_text_escapes() {
        for code in 0..=0x0010_ffff_u32 {
            let Some(character) = char::from_u32(code) else {
                continue;
            };
            if character == '\\' || inline(&character.to_string()) == character.to_string() {
                continue;
            }
            assert!(
                character.is_control()
                    || memprove_domain::PACK_ESCAPED_RANGES
                        .iter()
                        .any(|(first, last)| (*first..=*last).contains(&character)),
                "{code:x}"
            );
        }
    }

    /// U8 A4 (E1 revision 10, Owner O4): `resume --text` names the agent of
    /// each Session line as the Pack does, after the parentheses: its name,
    /// its role, and the Session that dispatched it (`Session j` when the
    /// Capsule lists it, else `Session <id>`); a Session without an agent
    /// keeps its line.
    #[test]
    fn u8_a4_session_lines_name_the_agent() {
        let body = r#"{"sessions":[
                {"session_id":"S1","end_state":"ended","started_at":"t1","last_recorded_at":"w1"},
                {"session_id":"S2","end_state":"crashed","started_at":"t2","last_recorded_at":"w2",
                    "agent":{"name":"w-a","role":"sub_agent","parent_session_id":"S1"}},
                {"session_id":"S3","end_state":"open","started_at":"t3","last_recorded_at":"w3",
                    "agent":{"name":"lead","role":"orchestrator","parent_session_id":null}},
                {"session_id":"S4","end_state":"open","started_at":"t4","last_recorded_at":null,
                    "agent":{"name":"x","role":"human","parent_session_id":"S9"}},
                {"session_id":"S5","end_state":"open","started_at":"t5","last_recorded_at":null}],
            "gaps":[],"changed_paths_by_base":[],"claims":[],
            "continuation":[
                {"class":0,"template":"header","object_id":"T","session_id":null,"verdict":"partial",
                    "reasons":[],"standing":null,
                    "args":{"title":"T","task_status":"in_progress","project_sequence":3,"head_oid":"Y"}},
                {"class":2,"template":"session_crashed","object_id":"S2","session_id":"S2","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t2"}},
                {"class":2,"template":"session_open","object_id":"S3","session_id":"S3","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t3"}},
                {"class":2,"template":"session_open","object_id":"S4","session_id":"S4","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t4"}},
                {"class":2,"template":"session_open","object_id":"S5","session_id":"S5","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t5"}}]}"#;
        let text = render(body, Some("S3"), true).unwrap();
        assert_eq!(
            text.lines().skip(2).collect::<Vec<_>>(),
            [
                "Session 2 S2 ended abandoned (session.crashed; started t2)\
                 ; agent「w-a」(sub_agent, dispatched by Session 1)",
                "Session 3 S3 (the Session this directory's context file records) not ended \
                  (started t3, last wrote to the ledger at w3); agent「lead」(orchestrator)",
                "Session 4 S4 not ended (started t4, nothing of it in the ledger yet)\
                 ; agent「x」(human, dispatched by unlisted Session S9)",
                "Session 5 S5 not ended (started t5, nothing of it in the ledger yet)",
            ]
        );
    }

    /// U10 V6 (E1 revision 13, Owner O5): a statement line names the agent
    /// its Session declared, `from Session k「<name>」`, as the
    /// Pack do, on a fact (`verdict: …; from …`) and on any other statement, in
    /// `--text` and `--text --all`; a Session without an agent and one the
    /// Capsule does not list keep their words, and so do `withdrawn by Session k` and
    /// the Session lines.
    #[test]
    fn u10_v6_statement_lines_name_the_agent_of_their_session() {
        let pass = r#"{"evidence_id":"E","kind":"test_result","run_status":"PASS",
            "base_commit":"Y","verdict":"verified","reasons":[]}"#;
        let item = |template: &str, number: u8, session: &str, standing: &str| {
            format!(
                r#"{{"class":6,"template":"{template}","object_id":"C{number}","session_id":"{session}",
                    "verdict":"verified","reasons":[],"standing":"{standing}",
                    "args":{{"number":{number},"text":"t{number}","rationale":null,
                        "base_commits":["Y"],"head_oid":"Y","superseded_by":[]}}}}"#
            )
        };
        let claim = |number: u8, kind: &str, session: &str, standing: &str| {
            format!(
                r#"{{"number":{number},"claim_id":"C{number}","kind":"{kind}","session_id":"{session}",
                    "standing":"{standing}","evidence":[{pass}],
                    "withdrawn_by":{{"checkpoint_id":"K","session_id":"S2"}}}}"#
            )
        };
        let body = format!(
            r#"{{"sessions":[{{"session_id":"S1","end_state":"ended"}},
                    {{"session_id":"S2","end_state":"ended",
                      "agent":{{"name":"u10-「s」","role":"sub_agent","parent_session_id":"S1"}}}}],
                "gaps":[],"changed_paths_by_base":[],
                "claims":[{},{},{},{},{}],
                "continuation":[
                    {{"class":0,"template":"header","object_id":"T","session_id":null,"verdict":"verified",
                        "reasons":[],"standing":null,
                        "args":{{"title":"T","task_status":"in_progress","project_sequence":3,"head_oid":"Y"}}}},
                    {},{},{},{},{}]}}"#,
            claim(1, "fact", "S2", "active"),
            claim(2, "fact", "S1", "active"),
            claim(3, "open_item", "S2", "active"),
            claim(4, "open_item", "S9", "active"),
            claim(5, "open_item", "S2", "withdrawn"),
            item("fact", 1, "S2", "active"),
            item("fact", 2, "S1", "active"),
            item("open_item", 3, "S2", "active"),
            item("open_item", 4, "S9", "active"),
            item("withdrawn", 5, "S2", "withdrawn"),
        );
        // `resume --text` escapes `」` in an agent name, as on its Session
        // lines (U8 A4).
        let named = "u10-「s\\」";
        let text = render(&body, None, true).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        for (number, ending) in [
            (
                1,
                format!("; verdict: verified; from Session 2「{named}」)"),
            ),
            (2, "; verdict: verified; from Session 1)".to_owned()),
            (3, format!("; from Session 2「{named}」)")),
            (4, "; from unlisted Session S9)".to_owned()),
        ] {
            let line = lines
                .iter()
                .find(|line| line.starts_with(&format!("#{number} ")))
                .unwrap_or_else(|| panic!("#{number}: {text}"));
            assert!(line.ends_with(&ending), "{line} lacks {ending}");
        }
        let withdrawn = lines
            .iter()
            .find(|line| line.starts_with("#5 "))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(
            withdrawn.contains(&format!("from Session 2「{named}」")),
            "{withdrawn}"
        );
        assert!(
            withdrawn.ends_with(" (withdrawn by Session 2)"),
            "{withdrawn}"
        );
        // Without `--all` the withdrawn one is folded; the others are as above.
        let folded = render(&body, None, false).unwrap();
        assert!(
            folded.contains(&format!("#1 verified: 「t1」 (evidence bound to commit Y, current HEAD Y; verdict: verified; from Session 2「{named}」)")),
            "{folded}"
        );
    }

    /// U9 A7 (E1 revision 12, Owner O3): an abandoned Session's line ends
    /// with who ended it, after the agent part, as the Pack writes it:
    /// `; ended by Session <j>` (the ender's Session listed, with or without
    /// an agent), `; ended by unlisted Session <id>` (not listed), `; ended by
    /// agent「<name>」(<role>)` (an agent only); without `ended_by` the line is
    /// as
    /// before.
    #[test]
    fn u9_a7_abandoned_session_lines_name_who_ended_them() {
        let body = r#"{"sessions":[
                {"session_id":"S1","end_state":"open","started_at":"t1","last_recorded_at":"w1"},
                {"session_id":"S2","end_state":"crashed","started_at":"t2","last_recorded_at":"w2",
                    "agent":{"name":"w-a","role":"sub_agent","parent_session_id":"S1"},
                    "ended_by":{"session_id":"S1",
                        "agent":{"name":"lead","role":"orchestrator","parent_session_id":null}}},
                {"session_id":"S3","end_state":"crashed","started_at":"t3","last_recorded_at":"w3",
                    "ended_by":{"session_id":"S9"}},
                {"session_id":"S4","end_state":"crashed","started_at":"t4","last_recorded_at":"w4",
                    "agent":{"name":"x","role":"human","parent_session_id":null},
                    "ended_by":{"agent":{"name":"taker","role":"sub_agent","parent_session_id":"S1"}}},
                {"session_id":"S5","end_state":"crashed","started_at":"t5","last_recorded_at":"w5"}],
            "gaps":[],"changed_paths_by_base":[],"claims":[],
            "continuation":[
                {"class":0,"template":"header","object_id":"T","session_id":null,"verdict":"partial",
                    "reasons":[],"standing":null,
                    "args":{"title":"T","task_status":"in_progress","project_sequence":3,"head_oid":"Y"}},
                {"class":2,"template":"session_crashed","object_id":"S2","session_id":"S2","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t2"}},
                {"class":2,"template":"session_crashed","object_id":"S3","session_id":"S3","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t3"}},
                {"class":2,"template":"session_crashed","object_id":"S4","session_id":"S4","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t4"}},
                {"class":2,"template":"session_crashed","object_id":"S5","session_id":"S5","verdict":null,
                    "reasons":[],"standing":null,"args":{"workspace_id":"W","started_at":"t5"}}]}"#;
        let text = render(body, None, true).unwrap();
        assert_eq!(
            text.lines().skip(2).collect::<Vec<_>>(),
            [
                "Session 2 S2 ended abandoned (session.crashed; started t2)\
                 ; agent「w-a」(sub_agent, dispatched by Session 1); ended by Session 1",
                "Session 3 S3 ended abandoned (session.crashed; started t3); ended by unlisted Session S9",
                "Session 4 S4 ended abandoned (session.crashed; started t4)\
                 ; agent「x」(human); ended by agent「taker」(sub_agent)",
                "Session 5 S5 ended abandoned (session.crashed; started t5)",
            ]
        );
    }
}
