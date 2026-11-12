//! The Task Capsule as the Context Pack reads it (P4-C, P4-C2).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::PackError;
use crate::{ClaimGroup, ClaimKind, ReasonCode, Sha256Digest, Standing, TestFormat, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionEnd {
    Ended,
    Crashed,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackSession {
    pub(super) session_id: String,
    pub(super) end: SessionEnd,
    pub(super) started_at: String,
    pub(super) last_recorded_at: Option<String>,
    /// ADR 0014 E6 (P4-F): the ledger sequence and `pack_digest` of the
    /// Pack the Session declared it continues from.
    pub(super) resumed_from: Option<(u64, String)>,
    /// U7 P1 (Owner O3): whether the reader called it its own
    /// ([`PackCapsule::with_own_session`]); never read from the Capsule.
    pub(super) own: bool,
    /// U8 A4 (E1 revision 10, Owner O4): the agent the Session declared.
    pub(super) agent: Option<PackAgent>,
    /// U9 A7 (E1 revision 12, Owner O3): who ended the Session as
    /// abandoned, when its Capsule entry says.
    pub(super) ended_by: Option<PackEnder>,
}

/// U9 A7 (Owner O3): who ended an abandoned Session, as the Pack names it:
/// by the ender's Session (by its number when the Capsule lists it), else
/// by its agent's name and role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PackEnder {
    /// The ender's Session.
    Session(Origin),
    /// Only the ender's agent: its name and role.
    Agent(String, &'static str),
}

/// The `ended_by` of one item of `sessions`, `None` without one; `ids` are
/// the Capsule's Session IDs in order. Only a Session that ended abandoned
/// has one (`session.crashed@2`); on any other it is malformed.
fn read_ended_by(
    session: &Value,
    ids: &[&str],
    end: SessionEnd,
) -> Result<Option<PackEnder>, PackError> {
    let ended_by = match session.get("ended_by") {
        None | Some(Value::Null) => return Ok(None),
        Some(ended_by) => ended_by,
    };
    if end != SessionEnd::Crashed {
        return Err(malformed(
            "ended_by on a Session that did not end abandoned",
        ));
    }
    if let Some(ender) = ended_by.get("session_id").filter(|id| !id.is_null()) {
        let ender = plain(ender.as_str().unwrap_or_default(), "ended_by.session_id")?;
        return Ok(Some(PackEnder::Session(
            ids.iter()
                .position(|id| *id == ender)
                .map_or(Origin::Unlisted(ender), |index| Origin::Numbered(index + 1)),
        )));
    }
    match read_agent(ended_by, ids)? {
        Some(agent) => Ok(Some(PackEnder::Agent(agent.name, agent.role))),
        None => Err(malformed("ended_by names neither a Session nor an agent")),
    }
}

/// U8 A4 (Owner O4): a Session's `agent` as the Pack shows it: its name,
/// its role (one of the closed set), and the Session that dispatched it,
/// by its number when the Capsule lists that Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackAgent {
    pub(super) name: String,
    pub(super) role: &'static str,
    pub(super) parent: Option<Origin>,
}

/// The `agent` of one item of `sessions`, `None` without one; `ids` are
/// the Capsule's Session IDs in order.
fn read_agent(session: &Value, ids: &[&str]) -> Result<Option<PackAgent>, PackError> {
    let agent = match session.get("agent") {
        None | Some(Value::Null) => return Ok(None),
        Some(agent) => agent,
    };
    let role = string(agent, "role")?;
    let role = ["orchestrator", "sub_agent", "human"]
        .into_iter()
        .find(|known| *known == role)
        .ok_or_else(|| malformed("an agent role outside the closed set"))?;
    let parent = match field(agent, "parent_session_id")? {
        Value::Null => None,
        parent => {
            let parent = plain(parent.as_str().unwrap_or_default(), "parent_session_id")?;
            Some(
                ids.iter()
                    .position(|id| *id == parent)
                    .map_or(Origin::Unlisted(parent), |index| {
                        Origin::Numbered(index + 1)
                    }),
            )
        }
    };
    Ok(Some(PackAgent {
        name: string(agent, "name")?.to_owned(),
        role,
        parent,
    }))
}

/// One action of the Capsule (ADR 0014 E3, P4-D): only fixed words,
/// numbers, and codes reach the Pack (Owner condition e).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackAction {
    /// `complete_task`, `begin_session`, or `cite_latest_run`.
    pub(super) action: &'static str,
    /// The Problem codes it is refused with; empty when it is available.
    pub(super) reasons: Vec<String>,
    /// U5 H5 (Owner O7): `cite_latest_run` refused with `invalid_request`:
    /// the sentence the daemon's Problem `detail` has for the cause the
    /// Capsule shows. A fixed sentence, never agent text; none when the
    /// Task has no run (S3).
    pub(super) invalid_request_detail: Option<&'static str>,
    /// The scope its write needs.
    pub(super) scope: String,
    /// `complete_task`: how many runs it cites.
    pub(super) cited: usize,
    /// `begin_session`: the numbers of the Sessions that have not ended.
    pub(super) open_sessions: Vec<usize>,
}

/// Where a claim came from: `Session N`, or a Session the Capsule does not
/// list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Origin {
    Numbered(usize),
    Unlisted(String),
}

/// One cited Evidence object, as P3-L L6 describes it in words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackEvidence {
    /// `PASS`, `FAIL`, `NOT_RUN`, `UNAVAILABLE`, or none.
    pub(super) run_status: Option<String>,
    /// None when its record could not be read: `getTaskCapsule` writes a
    /// null `kind` then (P4-H2, scenario 8).
    pub(super) kind: Option<String>,
    pub(super) reasons: Vec<ReasonCode>,
    pub(super) base: Option<String>,
    pub(super) verdict: Verdict,
    /// U5 P1 (Owner O3): its `evidence_id` and `argv`, when the Capsule
    /// gives them; they name a passed test run on the `Last passed:` line.
    pub(super) evidence_id: Option<String>,
    pub(super) argv: Option<Vec<String>>,
    /// U9 K2 (Owner O6): the `--test-format` of a test run that recorded
    /// one; it decides the rerun arguments of the `Last passed:` line.
    pub(super) test_format: Option<TestFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackClaim {
    pub(super) number: u64,
    pub(super) kind: ClaimKind,
    pub(super) standing: Standing,
    pub(super) verdict: Verdict,
    pub(super) reasons: Vec<ReasonCode>,
    pub(super) text: String,
    pub(super) rationale: Option<String>,
    pub(super) withdrawal_reason: Option<String>,
    pub(super) superseded_by: Vec<u64>,
    pub(super) origin: Origin,
    /// U10 V6 (E1 revision 13): the agent name its Session declared, when
    /// the Capsule lists that Session and it declared one.
    pub(super) author: Option<String>,
    pub(super) evidence: Vec<PackEvidence>,
    /// The distinct base commits of its Evidence, in order.
    pub(super) bases: Vec<String>,
}

impl PackClaim {
    pub(super) fn body_chars(&self) -> usize {
        self.text.chars().count() + self.rationale.as_deref().map_or(0, |r| r.chars().count())
    }

    pub(super) fn is_verified_fact(&self) -> bool {
        self.kind == ClaimKind::Fact && self.verdict == Verdict::Verified
    }

    /// Whether the claim belongs to `group` of a query profile (U3 G2).
    /// The four groups of `resume.v1` hold active claims only; U3 G4's
    /// `Retired` holds the superseded and withdrawn ones.
    pub(super) fn in_group(&self, group: ClaimGroup) -> bool {
        let active = self.standing == Standing::Active;
        match group {
            ClaimGroup::VerifiedFacts => active && self.is_verified_fact(),
            ClaimGroup::OtherFacts => {
                active && self.kind == ClaimKind::Fact && self.verdict != Verdict::Verified
            }
            ClaimGroup::Decisions => active && self.kind == ClaimKind::Decision,
            ClaimGroup::OpenItems => active && self.kind == ClaimKind::OpenItem,
            ClaimGroup::Retired => !active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Changes {
    pub(super) probed: bool,
    pub(super) complete: bool,
    pub(super) paths: Vec<String>,
}

/// One line-bearing item of the continuation, in its order (ADR 0013 D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Entry {
    ChainInvalid,
    Damaged {
        record_type: String,
        record_id: String,
        reason: ReasonCode,
    },
    Session(usize),
    /// An active claim (classes 3, 4, 6, 7, 8).
    Claim(u64),
    Rerun {
        argv: Vec<String>,
        numbers: Vec<u64>,
    },
    Reaffirm {
        argv: Vec<String>,
        evidence_id: String,
        snapshot_id: String,
        numbers: Vec<u64>,
    },
    /// A superseded or withdrawn claim (class 9): never a line of the Pack,
    /// only of `--claims`.
    Retired(u64),
}

/// The passed test run of the `Last passed:` line: its argv, the commit it is
/// bound to, and (U9 K2) the format it recorded, when it did.
pub(super) type LastPassed = (Vec<String>, String, Option<TestFormat>);

/// A Task Capsule read for the Pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackCapsule {
    pub(super) capsule_digest: String,
    pub(super) ontology_version: String,
    pub(super) title: String,
    pub(super) task_status: String,
    pub(super) verdict: Verdict,
    pub(super) reasons: Vec<ReasonCode>,
    pub(super) project_sequence: u64,
    pub(super) head: Option<String>,
    pub(super) sessions: Vec<PackSession>,
    pub(super) actions: Vec<PackAction>,
    pub(super) entries: Vec<Entry>,
    pub(super) claims: BTreeMap<u64, PackClaim>,
    pub(super) changes: BTreeMap<String, Changes>,
    pub(super) gaps: Vec<String>,
    /// U5 P1 (Owner O3): the argv and bound commit of the `Last passed:` line,
    /// when it has one ([`last_passed`]).
    /// U9 K2: with the format that run recorded, when it did.
    pub(super) last_passed: Option<LastPassed>,
    /// U8 F1 (SF-1): the argv of the Task's latest run, when that run is a
    /// passed test run ([`latest_passed_test_argv`]), whether or not the
    /// Pack places it; U9 K2: with the format it recorded, when it did.
    pub(super) latest_passed_test: Option<(Vec<String>, Option<TestFormat>)>,
    /// U10 T7 (Owner O4): the program the takeover hint names
    /// ([`PackCapsule::with_takeover_program`]); never read from the
    /// Capsule.
    pub(super) takeover_program: String,
}

fn malformed(what: impl Into<String>) -> PackError {
    PackError::Malformed(what.into())
}

fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, PackError> {
    value
        .get(name)
        .ok_or_else(|| malformed(format!("no member {name}")))
}

fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str, PackError> {
    field(value, name)?
        .as_str()
        .ok_or_else(|| malformed(format!("{name} is not a string")))
}

fn nullable_string(value: &Value, name: &str) -> Result<Option<String>, PackError> {
    match field(value, name)? {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(malformed(format!("{name} is not a string or null"))),
    }
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], PackError> {
    field(value, name)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| malformed(format!("{name} is not an array")))
}

fn boolean(value: &Value, name: &str) -> Result<bool, PackError> {
    field(value, name)?
        .as_bool()
        .ok_or_else(|| malformed(format!("{name} is not a boolean")))
}

fn natural(value: &Value, name: &str) -> Result<u64, PackError> {
    field(value, name)?
        .as_u64()
        .ok_or_else(|| malformed(format!("{name} is not a natural number")))
}

/// A string the daemon writes (an ID, a timestamp, a version), shown
/// outside `「」`: plain printable ASCII only.
fn plain(value: &str, name: &str) -> Result<String, PackError> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_graphic()) {
        Ok(value.to_owned())
    } else {
        Err(malformed(format!("{name} is not plain ASCII")))
    }
}

/// A code (a gap, a record type, a Task status, a kind): `[a-z0-9_]+`.
fn code(value: &str, name: &str) -> Result<String, PackError> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        Ok(value.to_owned())
    } else {
        Err(malformed(format!("{name} is not a code")))
    }
}

/// A commit: lower-case hexadecimal, at most 64 digits.
fn commit(value: &str) -> Result<String, PackError> {
    if (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(value.to_owned())
    } else {
        Err(malformed("a commit is not hexadecimal"))
    }
}

fn verdict_of(value: &str) -> Result<Verdict, PackError> {
    Verdict::ALL
        .into_iter()
        .find(|verdict| verdict.as_str() == value)
        .ok_or_else(|| malformed(format!("unknown verdict {value:?}")))
}

fn reasons_of(value: &Value, name: &str) -> Result<Vec<ReasonCode>, PackError> {
    array(value, name)?
        .iter()
        .map(|reason| {
            let reason = reason.as_str().unwrap_or_default();
            ReasonCode::ALL
                .into_iter()
                .find(|code| code.as_str() == reason)
                .ok_or_else(|| malformed(format!("unknown reason code {reason:?}")))
        })
        .collect()
}

fn numbers_of(value: &Value, name: &str) -> Result<Vec<u64>, PackError> {
    array(value, name)?
        .iter()
        .map(|number| {
            number
                .as_u64()
                .ok_or_else(|| malformed(format!("{name} holds a non-number")))
        })
        .collect()
}

fn strings_of(value: &Value, name: &str) -> Result<Vec<String>, PackError> {
    array(value, name)?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| malformed(format!("{name} holds a non-string")))
        })
        .collect()
}

fn read_sessions(value: &Value) -> Result<Vec<PackSession>, PackError> {
    let sessions = array(value, "sessions")?;
    let ids = sessions
        .iter()
        .map(|session| string(session, "session_id"))
        .collect::<Result<Vec<_>, _>>()?;
    sessions
        .iter()
        .map(|session| {
            let end = match string(session, "end_state")? {
                "ended" => SessionEnd::Ended,
                "crashed" => SessionEnd::Crashed,
                "open" => SessionEnd::Open,
                other => return Err(malformed(format!("unknown end_state {other:?}"))),
            };
            Ok(PackSession {
                session_id: plain(string(session, "session_id")?, "session_id")?,
                end,
                started_at: plain(string(session, "started_at")?, "started_at")?,
                last_recorded_at: match field(session, "last_recorded_at")? {
                    Value::Null => None,
                    other => Some(plain(
                        other.as_str().unwrap_or_default(),
                        "last_recorded_at",
                    )?),
                },
                resumed_from: match field(session, "resumed_from")? {
                    Value::Null => None,
                    from => {
                        let pack_digest = string(from, "pack_digest")?;
                        pack_digest
                            .strip_prefix("sha256:")
                            .and_then(|hex| Sha256Digest::parse(hex).ok())
                            .ok_or_else(|| malformed("resumed_from.pack_digest"))?;
                        if !boolean(from, "declared")? {
                            return Err(malformed("resumed_from is not declared"));
                        }
                        Some((natural(from, "project_sequence")?, pack_digest.to_owned()))
                    }
                },
                own: false,
                agent: read_agent(session, &ids)?,
                ended_by: read_ended_by(session, &ids, end)?,
            })
        })
        .collect()
}

/// The actions, in their fixed order (ADR 0014 E3 as implemented in P4-D).
fn read_actions(value: &Value, sessions: &[PackSession]) -> Result<Vec<PackAction>, PackError> {
    const ORDER: [&str; 3] = ["complete_task", "begin_session", "cite_latest_run"];
    const CODES: [&str; 4] = [
        "invalid_request",
        "project_workspace_mismatch",
        "invalid_transition",
        "evidence_stale",
    ];
    let items = array(value, "actions")?;
    if items.len() != ORDER.len() {
        return Err(malformed("actions is not the three actions"));
    }
    items
        .iter()
        .zip(ORDER)
        .map(|(item, action)| {
            if string(item, "action")? != action {
                return Err(malformed("actions are out of order"));
            }
            let reasons = strings_of(item, "reasons")?;
            if reasons
                .iter()
                .any(|reason| !CODES.contains(&reason.as_str()))
            {
                return Err(malformed(
                    "an action reason is not a Problem code it can have",
                ));
            }
            if boolean(item, "available")? != reasons.is_empty() {
                return Err(malformed(
                    "an action's availability disagrees with its reasons",
                ));
            }
            let args = field(item, "args")?;
            let open_sessions = if action == "begin_session" {
                array(args, "open_session_ids")?
                    .iter()
                    .map(|id| {
                        let id = id.as_str().unwrap_or_default();
                        sessions
                            .iter()
                            .position(|session| session.session_id == id)
                            .map(|index| index + 1)
                            .ok_or_else(|| malformed("an open Session the Capsule does not list"))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            let invalid_request_detail = (action == "cite_latest_run"
                && reasons.iter().any(|reason| reason == "invalid_request"))
            .then(|| cite_latest_run_detail(args))
            .flatten();
            Ok(PackAction {
                action,
                reasons,
                invalid_request_detail,
                scope: match string(item, "scope")? {
                    scope @ ("mcp-write" | "control") => scope.to_owned(),
                    _ => return Err(malformed("an action scope is not a write scope")),
                },
                cited: if action == "complete_task" {
                    array(args, "completion_evidence_ids")?.len()
                } else {
                    0
                },
                open_sessions,
            })
        })
        .collect()
}

/// U5 H5 (Owner O7): why a `createCheckpoint` citing the Task's latest run
/// is `400 invalid_request`, in the words of the daemon's Problem `detail`:
/// a latest run that is not a passed test run (the Evidence rule of
/// `verified_fact`), or a passed run a later one overrules (the opening of
/// the daemon's detail, which then lists positions of the request). The
/// Evidence rule is also what is left when the Capsule shows no other cause.
///
/// U5 Z-F round 1 (S3): no run gives none. There is no Snapshot to put in
/// the request, and the daemon's `detail` is the first of its members it
/// finds missing or null (`$.workspace_id is required`, `$.snapshot_id is
/// required`, `$.snapshot_id must be string, not null`, …), which depends on
/// the request the client builds and not on the Capsule.
fn cite_latest_run_detail(args: &Value) -> Option<&'static str> {
    if args["evidence_id"].is_null() {
        None
    } else if args["kind"] == "test_result"
        && args["run_status"] == "PASS"
        && !args["overruled_by"].is_null()
    {
        Some("1 verified_fact citation names an overruled passed run")
    } else {
        Some("verified_fact requires successful executed test Evidence")
    }
}

/// U5 P1 (Owner O3, D1 F10): the argv and bound commit of the passed test
/// run the `Last passed:` line names, by what the Capsule shows. None when the
/// Capsule has a `rerun` item (its `Rerun:` line gives the argv), when it is
/// `tampered`, or when it shows no passed test run it can place:
///
/// 1. the Task's latest run (`cite_latest_run`'s `args`), when it is a test
///    run (`test_result`) that passed (`PASS`) and the Capsule gives its
///    commit: the base commit of a claim's Evidence with its `evidence_id`,
///    else, when it is one of the runs `complete_task` cites (a passed run
///    that is `verified` now, so bound to the HEAD the probe read), that
///    HEAD;
/// 2. else the last of the runs `complete_task` cites, in their (ledger)
///    order, that a claim's Evidence shows as a passed test run, with that
///    Evidence's argv and base commit.
///
/// A claim's Evidence counts in claim-number order, then in its order; the
/// first one with an `evidence_id` gives that run's argv and commit.
fn last_passed(value: &Value, capsule: &PackCapsule) -> Result<Option<LastPassed>, PackError> {
    if capsule.verdict == Verdict::Tampered
        || capsule
            .entries
            .iter()
            .any(|entry| matches!(entry, Entry::Rerun { .. }))
    {
        return Ok(None);
    }
    let passed_test = |kind: Option<&str>, status: Option<&str>| {
        kind == Some("test_result") && status == Some("PASS")
    };
    let cited = |id: &str| {
        capsule
            .claims
            .values()
            .flat_map(|claim| &claim.evidence)
            .find(|evidence| evidence.evidence_id.as_deref() == Some(id))
            .filter(|evidence| {
                passed_test(evidence.kind.as_deref(), evidence.run_status.as_deref())
            })
            .and_then(|evidence| {
                Some((
                    evidence.argv.clone()?,
                    evidence.base.clone()?,
                    evidence.test_format,
                ))
            })
    };
    let args = |action: &str| {
        array(value, "actions").map(|actions| {
            actions
                .iter()
                .find(|item| item["action"] == action)
                .map_or(&Value::Null, |item| &item["args"])
        })
    };
    let completion = args("complete_task")?["completion_evidence_ids"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let latest = args("cite_latest_run")?;
    if let (Some(id), Some(argv)) = (latest["evidence_id"].as_str(), argv_in(latest))
        && passed_test(latest["kind"].as_str(), latest["run_status"].as_str())
    {
        let commit = cited(id).map(|(_, base, _)| base).or_else(|| {
            completion
                .contains(&id)
                .then(|| capsule.head.clone())
                .flatten()
        });
        if let Some(commit) = commit {
            return Ok(Some((argv, commit, test_format_in(latest)?)));
        }
    }
    Ok(completion.iter().rev().find_map(|id| cited(id)))
}

/// U8 F1 (SF-1): the argv of the Task's latest run (`cite_latest_run`'s
/// `args`) when it is a test run (`test_result`) that passed (`PASS`); `None`
/// when it is not, when there is no run, or when the Capsule is `tampered`.
/// Unlike [`last_passed`] it does not ask where the run is bound: a passed
/// test run that no statement cites, or that was made on an earlier commit,
/// is still the Task's latest passed test run.
fn latest_passed_test_argv(
    value: &Value,
    capsule: &PackCapsule,
) -> Option<(Vec<String>, Option<TestFormat>)> {
    if capsule.verdict == Verdict::Tampered {
        return None;
    }
    let latest = value
        .get("actions")?
        .as_array()?
        .iter()
        .find(|item| item["action"] == "cite_latest_run")?
        .get("args")?;
    if latest["kind"] == "test_result" && latest["run_status"] == "PASS" {
        Some((argv_in(latest)?, test_format_in(latest).ok()?))
    } else {
        None
    }
}

fn kind_of(value: &str) -> Result<ClaimKind, PackError> {
    [
        ClaimKind::Fact,
        ClaimKind::OpenItem,
        ClaimKind::NextAction,
        ClaimKind::Decision,
    ]
    .into_iter()
    .find(|kind| kind.as_str() == value)
    .ok_or_else(|| malformed(format!("unknown claim kind {value:?}")))
}

fn standing_of(value: &str) -> Result<Standing, PackError> {
    [Standing::Active, Standing::Superseded, Standing::Withdrawn]
        .into_iter()
        .find(|standing| standing.as_str() == value)
        .ok_or_else(|| malformed(format!("unknown standing {value:?}")))
}

fn read_evidence(item: &Value) -> Result<PackEvidence, PackError> {
    let run_status = nullable_string(item, "run_status")?;
    if let Some(status) = &run_status
        && !matches!(status.as_str(), "PASS" | "FAIL" | "NOT_RUN" | "UNAVAILABLE")
    {
        return Err(malformed(format!("unknown run_status {status:?}")));
    }
    let kind = nullable_string(item, "kind")?
        .as_deref()
        .map(|kind| code(kind, "Evidence kind"))
        .transpose()?;
    let verdict = verdict_of(string(item, "verdict")?)?;
    if kind.is_none() && verdict == Verdict::Verified {
        return Err(malformed("Evidence that could not be read is verified"));
    }
    Ok(PackEvidence {
        run_status,
        kind,
        reasons: reasons_of(item, "reasons")?,
        base: nullable_string(item, "base_commit")?
            .as_deref()
            .map(commit)
            .transpose()?,
        verdict,
        evidence_id: item
            .get("evidence_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        argv: argv_in(item),
        test_format: test_format_in(item)?,
    })
}

/// U9 K2 (Owner O6): the `test_format` member of `value`, absent for a run
/// that recorded none; a value outside the closed set is malformed.
fn test_format_in(value: &Value) -> Result<Option<TestFormat>, PackError> {
    value
        .get("test_format")
        .map(|format| {
            format
                .as_str()
                .and_then(TestFormat::parse)
                .ok_or_else(|| malformed("test_format"))
        })
        .transpose()
}

/// U5 P1: the `argv` member of `value`, when it is an array of strings.
fn argv_in(value: &Value) -> Option<Vec<String>> {
    value
        .get("argv")?
        .as_array()?
        .iter()
        .map(|argument| argument.as_str().map(str::to_owned))
        .collect()
}

fn read_claim(claim: &Value, sessions: &[PackSession]) -> Result<PackClaim, PackError> {
    let session_id = plain(string(claim, "session_id")?, "session_id")?;
    let listed = sessions
        .iter()
        .position(|session| session.session_id == session_id);
    let author = listed
        .and_then(|index| sessions[index].agent.as_ref())
        .map(|agent| agent.name.clone());
    let origin = listed.map_or(Origin::Unlisted(session_id), |index| {
        Origin::Numbered(index + 1)
    });
    let evidence = array(claim, "evidence")?
        .iter()
        .map(read_evidence)
        .collect::<Result<Vec<_>, _>>()?;
    let mut bases: Vec<String> = Vec::new();
    for base in evidence.iter().filter_map(|item| item.base.clone()) {
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    let number = natural(claim, "number")?;
    if number == 0 {
        return Err(malformed("claim number 0"));
    }
    let superseded_by = array(claim, "superseded_by")?
        .iter()
        .map(|by| natural(by, "number"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PackClaim {
        number,
        kind: kind_of(string(claim, "kind")?)?,
        standing: standing_of(string(claim, "standing")?)?,
        verdict: verdict_of(string(claim, "verdict")?)?,
        reasons: reasons_of(claim, "reasons")?,
        text: string(claim, "text")?.to_owned(),
        rationale: field(claim, "rationale")?.as_str().map(str::to_owned),
        withdrawal_reason: field(claim, "withdrawal_reason")?
            .as_str()
            .map(str::to_owned),
        superseded_by,
        origin,
        author,
        evidence,
        bases,
    })
}

/// The claims by number, and each `claim_id`'s number.
type ReadClaims = (BTreeMap<u64, PackClaim>, BTreeMap<String, u64>);

fn read_claims(value: &Value, sessions: &[PackSession]) -> Result<ReadClaims, PackError> {
    let mut claims = BTreeMap::new();
    let mut ids = BTreeMap::new();
    for claim in array(value, "claims")? {
        let read = read_claim(claim, sessions)?;
        let id = plain(string(claim, "claim_id")?, "claim_id")?;
        if ids.insert(id, read.number).is_some() {
            return Err(malformed("a claim_id appears twice"));
        }
        if let Some(earlier) = claims.insert(read.number, read) {
            return Err(malformed(format!(
                "claim #{} appears twice",
                earlier.number
            )));
        }
    }
    Ok((claims, ids))
}

fn read_changes(value: &Value) -> Result<BTreeMap<String, Changes>, PackError> {
    let mut changes = BTreeMap::new();
    for change in array(value, "changed_paths_by_base")? {
        let paths = strings_of(change, "paths")?
            .into_iter()
            .map(|path| {
                path.strip_prefix("repo://")
                    .map_or(path.clone(), str::to_owned)
            })
            .collect();
        changes.insert(
            commit(string(change, "base_commit")?)?,
            Changes {
                probed: boolean(change, "probed")?,
                complete: boolean(change, "complete")?,
                paths,
            },
        );
    }
    Ok(changes)
}

/// The template of a claim item, and the class it belongs in.
fn claim_class(claim: &PackClaim) -> (u64, &'static str) {
    match claim.standing {
        Standing::Withdrawn => return (9, "withdrawn"),
        Standing::Superseded => return (9, "superseded"),
        Standing::Active => {}
    }
    match claim.kind {
        ClaimKind::OpenItem => (3, "open_item"),
        ClaimKind::Fact if claim.verdict == Verdict::Verified => (6, "fact"),
        ClaimKind::Fact => (4, "fact"),
        ClaimKind::Decision => (7, "decision"),
        ClaimKind::NextAction => (8, "next_action"),
    }
}

/// One continuation item that is not a claim's, as an entry.
fn plain_entry(
    class: u64,
    template: &str,
    item: &Value,
    capsule: &PackCapsule,
) -> Result<Option<Entry>, PackError> {
    let object = string(item, "object_id")?;
    let args = field(item, "args")?;
    Ok(Some(match (class, template) {
        (0, "header") => return Ok(None),
        (1, "ledger_chain_invalid") => Entry::ChainInvalid,
        (1, "damaged_record") => {
            let [reason] = reasons_of(item, "reasons")?[..] else {
                return Err(malformed("a damaged record without one reason"));
            };
            Entry::Damaged {
                record_type: code(string(args, "record_type")?, "record_type")?,
                record_id: plain(object, "record_id")?,
                reason,
            }
        }
        (2, "session_open" | "session_crashed") => {
            let index = capsule
                .sessions
                .iter()
                .position(|session| session.session_id == object)
                .ok_or_else(|| malformed("a Session item names no listed Session"))?;
            let end = if template == "session_open" {
                SessionEnd::Open
            } else {
                SessionEnd::Crashed
            };
            if capsule.sessions[index].end != end {
                return Err(malformed("a Session item disagrees with its Session"));
            }
            Entry::Session(index)
        }
        (5, "rerun") => Entry::Rerun {
            argv: strings_of(args, "argv")?,
            numbers: numbers_of(args, "numbers")?,
        },
        (5, "reaffirm") => Entry::Reaffirm {
            argv: strings_of(args, "argv")?,
            evidence_id: plain(string(args, "verified_evidence_id")?, "evidence id")?,
            snapshot_id: plain(string(args, "verified_snapshot_id")?, "snapshot id")?,
            numbers: numbers_of(args, "numbers")?,
        },
        _ => return Err(malformed(format!("unknown item {class} {template}"))),
    }))
}

/// The continuation, as entries; checks that every claim is listed once in
/// its class, in class order.
fn read_entries(
    value: &Value,
    capsule: &PackCapsule,
    ids: &BTreeMap<String, u64>,
) -> Result<Vec<Entry>, PackError> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    let mut last_class = 0;
    for (index, item) in array(value, "continuation")?.iter().enumerate() {
        let class = natural(item, "class")?;
        let template = string(item, "template")?;
        if class < last_class || (index == 0) != (class == 0 && template == "header") {
            return Err(malformed("the continuation is out of order"));
        }
        last_class = class;
        if !matches!(class, 3 | 4 | 6..=9) {
            entries.extend(plain_entry(class, template, item, capsule)?);
            continue;
        }
        let number = *ids
            .get(string(item, "object_id")?)
            .ok_or_else(|| malformed("a claim item names no claim"))?;
        let claim = &capsule.claims[&number];
        if claim_class(claim) != (class, template) || !seen.insert(number) {
            return Err(malformed(format!("claim #{number} is listed wrongly")));
        }
        entries.push(if class == 9 {
            Entry::Retired(number)
        } else {
            Entry::Claim(number)
        });
    }
    if seen.len() != capsule.claims.len() {
        return Err(malformed("a claim is missing from the continuation"));
    }
    Ok(entries)
}

impl PackCapsule {
    /// U8 K5 (E1 revision 11): the argv of the passed test run the Pack's
    /// `Last passed:` line names, `None` when the Pack has no such line.
    #[must_use]
    pub fn last_passed_argv(&self) -> Option<&[String]> {
        self.last_passed
            .as_ref()
            .map(|(argv, _, _)| argv.as_slice())
    }

    /// U8 F1 (SF-1): the argv of the Task's last passed test run, as the
    /// Capsule shows it: its latest run when that is a passed test run (bound
    /// to a commit or not), else the run the Pack's `Last passed:` line names.
    /// `None` when the Capsule shows neither. It does not decide what the
    /// Pack prints.
    #[must_use]
    pub fn last_passed_test_argv(&self) -> Option<&[String]> {
        self.last_passed_test().map(|(argv, _)| argv)
    }

    /// U9 K2 (Owner O6): [`Self::last_passed_test_argv`] with the format
    /// that run recorded, `None` for a run recorded before U9 or without
    /// one.
    #[must_use]
    pub fn last_passed_test(&self) -> Option<(&[String], Option<TestFormat>)> {
        self.latest_passed_test
            .as_ref()
            .map(|(argv, format)| (argv.as_slice(), *format))
            .or_else(|| {
                self.last_passed
                    .as_ref()
                    .map(|(argv, _, format)| (argv.as_slice(), *format))
            })
    }

    /// Reads the JCS bytes of a Task Capsule (`getTaskCapsule`).
    ///
    /// # Errors
    ///
    /// [`PackError::Malformed`] when it is not a Task Capsule the Pack can
    /// render: a member is missing or of the wrong type; a string the daemon
    /// writes (an ID, a commit, a timestamp, a code) is not plain ASCII; the
    /// continuation does not list each claim once in its class; the Capsule
    /// is `tampered` and still holds a `verified` claim; or an Evidence
    /// object whose record could not be read (a null `kind`) is `verified`.
    pub fn from_json(body: &str) -> Result<Self, PackError> {
        let value: Value =
            serde_json::from_str(body).map_err(|error| malformed(format!("not JSON: {error}")))?;
        let digest = string(&value, "capsule_digest")?;
        digest
            .strip_prefix("sha256:")
            .and_then(|hex| Sha256Digest::parse(hex).ok())
            .ok_or_else(|| malformed("capsule_digest"))?;
        let task = field(&value, "task")?;
        let bindings = field(&value, "bindings")?;
        let probe = field(bindings, "probe")?;
        let sessions = read_sessions(&value)?;
        let actions = read_actions(&value, &sessions)?;
        let (claims, ids) = read_claims(&value, &sessions)?;
        let mut capsule = Self {
            capsule_digest: digest.to_owned(),
            ontology_version: plain(string(&value, "ontology_version")?, "ontology_version")?,
            title: string(task, "title")?.to_owned(),
            task_status: code(string(task, "status")?, "task status")?,
            verdict: verdict_of(string(&value, "status")?)?,
            reasons: reasons_of(&value, "reasons")?,
            project_sequence: natural(field(bindings, "ledger")?, "project_sequence")?,
            head: if boolean(probe, "available")? {
                Some(commit(string(probe, "head_oid")?)?)
            } else {
                None
            },
            sessions,
            actions,
            entries: Vec::new(),
            claims,
            changes: read_changes(&value)?,
            gaps: strings_of(&value, "gaps")?
                .iter()
                .map(|gap| code(gap, "gap"))
                .collect::<Result<_, _>>()?,
            last_passed: None,
            latest_passed_test: None,
            takeover_program: crate::PACK_TAKEOVER_PROGRAM.to_owned(),
        };
        capsule.entries = read_entries(&value, &capsule, &ids)?;
        capsule.check(&value)?;
        capsule.last_passed = last_passed(&value, &capsule)?;
        capsule.latest_passed_test = latest_passed_test_argv(&value, &capsule);
        Ok(capsule)
    }

    /// U7 P1 (E1 revision 9; D2 F16, Owner O3): the Capsule as read by the
    /// reader whose own Session is `session_id`, the Session named by the
    /// context the reader uses: its `Session ` line, when it has not ended,
    /// has no takeover hint ([`crate::pack_takeover_hint`]); every other
    /// Session that has
    /// not ended has it. `None`, or an ID the Capsule does not list, leaves
    /// every such Session someone else's, as [`PackCapsule::from_json`]
    /// reads them.
    #[must_use]
    pub fn with_own_session(mut self, session_id: Option<&str>) -> Self {
        for session in &mut self.sessions {
            session.own = session_id == Some(session.session_id.as_str());
        }
        self
    }

    /// U10 T7 (Owner O4; D3 F24, F30): the program the takeover hint names,
    /// as the reader runs it: [`crate::PACK_TAKEOVER_PROGRAM`] when that
    /// name finds it, else its absolute path. A program
    /// [`crate::is_takeover_program`] does not accept leaves the default,
    /// so the hint stays one plain shell word. The CLI gives the same
    /// program to `resume --pack`, the hook, and the MCP server, so the
    /// three give the same Pack, and the same `pack_digest`, in one
    /// environment.
    #[must_use]
    pub fn with_takeover_program(mut self, program: &str) -> Self {
        if crate::is_takeover_program(program) {
            program.clone_into(&mut self.takeover_program);
        }
        self
    }

    /// U7 P1 (Owner O3, "`resumed_from`, or recorded by this client's context"): also the
    /// reader's own, every Session that declared it continues from the Pack
    /// of `resumed_from` (its ledger sequence and `pack_digest`,
    /// `sha256:<hex>`), the Pack the reader's last-Pack record names. Adds
    /// to [`PackCapsule::with_own_session`], which it follows; `None` adds
    /// nothing.
    #[must_use]
    pub fn with_own_resumed(mut self, resumed_from: Option<(u64, &str)>) -> Self {
        if let Some((sequence, digest)) = resumed_from {
            for session in &mut self.sessions {
                if session
                    .resumed_from
                    .as_ref()
                    .is_some_and(|(declared, declared_digest)| {
                        *declared == sequence && declared_digest == digest
                    })
                {
                    session.own = true;
                }
            }
        }
        self
    }

    fn check(&self, value: &Value) -> Result<(), PackError> {
        let chain_valid = boolean(field(field(value, "bindings")?, "ledger")?, "chain_valid")?;
        if chain_valid == self.entries.contains(&Entry::ChainInvalid) {
            return Err(malformed("chain_valid disagrees with the continuation"));
        }
        let damaged = self
            .entries
            .iter()
            .filter(|entry| matches!(entry, Entry::Damaged { .. }))
            .count();
        if damaged != array(value, "damaged_records")?.len() {
            return Err(malformed("damaged_records disagrees with the continuation"));
        }
        if self.verdict == Verdict::Tampered
            && self
                .claims
                .values()
                .any(|claim| claim.verdict == Verdict::Verified)
        {
            return Err(malformed("a tampered Capsule holds a verified claim"));
        }
        Ok(())
    }

    pub(super) fn active(&self) -> impl Iterator<Item = &PackClaim> {
        self.claims
            .values()
            .filter(|claim| claim.standing == Standing::Active)
    }

    /// The active claims in continuation order.
    pub(super) fn active_in_order(&self) -> impl Iterator<Item = &PackClaim> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Claim(number) => Some(&self.claims[number]),
            _ => None,
        })
    }

    /// The base commits of the active claims `listed` keeps, in the order
    /// their first line names them (P4-C2 revision 2: each has a line).
    #[cfg(test)]
    pub(super) fn base_order(&self, listed: impl Fn(u64) -> bool) -> Vec<String> {
        let order = self
            .active_in_order()
            .map(|claim| claim.number)
            .collect::<Vec<_>>();
        self.base_order_in(&order, listed)
    }

    /// The base commits of the claims of `order` that `listed` keeps, in
    /// the order their first line names them (U3 G4: `order` is the order
    /// of a Pack's claim lines; for `resume.v1` the active claims in
    /// continuation order).
    pub(super) fn base_order_in(&self, order: &[u64], listed: impl Fn(u64) -> bool) -> Vec<String> {
        let mut bases: Vec<String> = Vec::new();
        for claim in order
            .iter()
            .filter(|number| listed(**number))
            .map(|number| &self.claims[number])
        {
            for base in &claim.bases {
                if !bases.contains(base) {
                    bases.push(base.clone());
                }
            }
        }
        bases
    }

    /// Whether a base line lists file names level 1 can count instead.
    pub(super) fn listable(&self, base: &str) -> bool {
        self.changes
            .get(base)
            .is_some_and(|changes| changes.probed && changes.complete && !changes.paths.is_empty())
    }

    /// The active claims of `groups`, group by group, lower numbers first
    /// within each (U3 G2: the order of an omission step of a query
    /// profile; for `resume.v1`, E1 revision b and P4-C2 revision 4).
    ///
    /// U3 G4: only the claims of `members` (a Pack's claims: the active
    /// ones for `resume.v1`, the selected ones for `search.v1`).
    pub(super) fn group_order(&self, groups: &[ClaimGroup], members: &BTreeSet<u64>) -> Vec<u64> {
        groups
            .iter()
            .flat_map(|group| {
                members
                    .iter()
                    .map(|number| &self.claims[number])
                    .filter(move |claim| claim.in_group(*group))
                    .map(|claim| claim.number)
            })
            .collect()
    }

    /// U3 G4: what a query can match, one item per claim of the Task,
    /// superseded and withdrawn ones too, by number: the claim number, its
    /// text, and a Decision's rationale.
    #[must_use]
    pub fn statements(&self) -> Vec<(u64, &str, Option<&str>)> {
        self.claims
            .values()
            .map(|claim| {
                (
                    claim.number,
                    claim.text.as_str(),
                    claim.rationale.as_deref(),
                )
            })
            .collect()
    }

    /// U4 S2: what `search.v2` can match, one item per claim of the Task,
    /// superseded and withdrawn ones too, by number: as
    /// [`Self::statements`], with each claim's kind and standing.
    #[must_use]
    pub fn search_statements(&self) -> Vec<PackStatement<'_>> {
        self.claims
            .values()
            .map(|claim| PackStatement {
                number: claim.number,
                text: claim.text.as_str(),
                rationale: claim.rationale.as_deref(),
                kind: claim.kind,
                standing: claim.standing,
            })
            .collect()
    }
}

/// U4 S2: one claim as a query can match it ([`PackCapsule::search_statements`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackStatement<'a> {
    /// The claim number (`#n`).
    pub number: u64,
    /// The statement's text.
    pub text: &'a str,
    /// A Decision's rationale.
    pub rationale: Option<&'a str>,
    /// Its kind.
    pub kind: ClaimKind,
    /// Its standing.
    pub standing: Standing,
}
