//! The lines of the Context Pack (P4-C, as revised in P4-C2).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::{
    Entry, PACK_BODY_KEPT, PACK_CHANGED_FILES_SHOWN, PACK_FORMAT, PACK_FULL_TEXT_COMMAND,
    PACK_RESUMED_DIGEST_SHOWN, PACK_TAMPERED_NUMBERS, PackAction, PackCapsule, PackClaim, PackCut,
    PackError, PackEvidence, PackSelection, PackSession, SessionEnd, Step, codes, kind_word,
    quoted, ranges,
    read::{Origin, PackEnder},
    rerun_arguments_for, short, units, word,
};
use crate::{ClaimKind, ReasonCode, Sha256Digest, Standing, Verdict};

/// One cited Evidence object in words (P3-L L6): its own result, then
/// whether the commit it is bound to is still the current state. Never a
/// verdict word of a statement and never a reason code.
fn evidence_words(evidence: &PackEvidence) -> String {
    let missing = evidence.reasons.contains(&ReasonCode::ObjectMissing);
    let status = evidence.run_status.as_deref();
    let result = match status {
        Some("PASS") => "passed",
        Some("FAIL") => "failed",
        Some("NOT_RUN") => "not run",
        Some("UNAVAILABLE") => "unavailable",
        _ if missing => "missing",
        _ if matches!(
            evidence.kind.as_deref(),
            Some("git_head" | "git_worktree" | "file_digest")
        ) =>
        {
            "repository state record"
        }
        _ => "result unknown",
    };
    let fresh = evidence.reasons.contains(&ReasonCode::EvidenceNotPassed);
    let overruled = if status == Some("PASS") && fresh {
        " (overruled by a later failed run of the same command in the same state)"
    } else {
        ""
    };
    let commit = evidence.base.as_deref().map_or_else(
        || "the bound commit".to_owned(),
        |base| format!("the bound commit {}", short(base)),
    );
    let state = match evidence.verdict {
        Verdict::Tampered => "record tampered".to_owned(),
        _ if missing => "unreadable".to_owned(),
        Verdict::Stale => format!("{commit} is no longer the current state"),
        Verdict::Verified => format!("{commit} is still the current state"),
        Verdict::Partial if fresh => format!("{commit} is still the current state"),
        Verdict::Partial => format!("whether {commit} is still the current state is unconfirmed"),
    };
    format!("{result}{overruled}, {state}")
}

/// What an open item, a Decision, a next action, or a superseded or
/// withdrawn claim shows instead of a verdict (P3-L L6, P4-C2 revision 3):
/// `no evidence cited`, or each cited Evidence in words; `tampered; `
/// first in a `tampered` Capsule.
fn cited(claim: &PackClaim) -> String {
    let words = match &claim.evidence[..] {
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
    if claim.verdict == Verdict::Tampered {
        format!("tampered; {words}")
    } else {
        words
    }
}

/// A fact's Evidence binding: the commits by their first digits (P4-C2
/// revision 1); the current HEAD is on the first line only.
fn binding(claim: &PackClaim) -> String {
    if claim.evidence.is_empty() {
        "no evidence cited".to_owned()
    } else if claim.bases.is_empty() {
        "the commit the evidence is bound to is unreadable".to_owned()
    } else {
        format!(
            "evidence bound to commit {}",
            claim
                .bases
                .iter()
                .map(|base| short(base))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn body(claim: &PackClaim, cut: PackCut) -> String {
    let total = claim.body_chars();
    match cut {
        PackCut::Full => match &claim.rationale {
            Some(rationale) => format!("{}, rationale {}", quoted(&claim.text), quoted(rationale)),
            None => quoted(&claim.text),
        },
        PackCut::Shortened => {
            let text_chars = claim.text.chars().count();
            let text = claim.text.chars().take(PACK_BODY_KEPT).collect::<String>();
            let rationale = claim
                .rationale
                .as_deref()
                .filter(|_| text_chars < PACK_BODY_KEPT)
                .map(|rationale| {
                    rationale
                        .chars()
                        .take(PACK_BODY_KEPT - text_chars)
                        .collect::<String>()
                });
            let shown = match rationale {
                Some(rationale) => format!("{}, rationale {}", quoted(&text), quoted(&rationale)),
                None => quoted(&text),
            };
            format!(
                "{shown}… (body {total} chars, {} omitted)",
                total - PACK_BODY_KEPT
            )
        }
        _ => format!("(body omitted, {total} chars)"),
    }
}

/// `from Session k`, with U10 V6 (E1 revision 13) `「<name>」` when Session
/// k declared an agent; `from unlisted Session <id>` for a Session the
/// Capsule does not list.
fn origin(claim: &PackClaim) -> String {
    match &claim.origin {
        Origin::Numbered(session) => format!(
            "from Session {session}{}",
            claim.author.as_deref().map_or_else(String::new, quoted)
        ),
        Origin::Unlisted(session_id) => format!("from unlisted Session {session_id}"),
    }
}

/// The line of one claim with its body `cut`: an active one as the Pack
/// lists it, a superseded or withdrawn one as `--claims` shows it.
fn claim_line(claim: &PackClaim, cut: PackCut) -> String {
    let number = claim.number;
    let body = body(claim, cut);
    let from = origin(claim);
    match claim.standing {
        Standing::Superseded => {
            let by = claim
                .superseded_by
                .iter()
                .map(|by| format!("#{by}"))
                .collect::<Vec<_>>()
                .join(", ");
            return format!(
                "#{number} {}, superseded by {by}: {body} ({}; {from})",
                kind_word(claim.kind),
                cited(claim)
            );
        }
        Standing::Withdrawn => {
            let reason = claim
                .withdrawal_reason
                .as_deref()
                .map(|reason| format!("withdrawal reason {}; ", quoted(reason)))
                .unwrap_or_default();
            return format!(
                "#{number} {}, withdrawn: {body} ({reason}{}; {from})",
                kind_word(claim.kind),
                cited(claim)
            );
        }
        Standing::Active => {}
    }
    if claim.kind == ClaimKind::Fact {
        let label = if claim.verdict == Verdict::Verified {
            "verified".to_owned()
        } else {
            format!("fact {}", word(claim.verdict))
        };
        let reasons = if claim.reasons.is_empty() {
            String::new()
        } else {
            format!("reasons: {}; ", codes(&claim.reasons))
        };
        format!(
            "#{number} {label}: {body} ({reasons}{}; {from})",
            binding(claim)
        )
    } else {
        format!(
            "#{number} {}: {body} ({}; {from})",
            kind_word(claim.kind),
            cited(claim)
        )
    }
}

/// The line of one base commit (P4-C2 revision 2): its full name and the
/// files changed since, or (level 1) their count.
fn base_line(capsule: &PackCapsule, base: &str, counted: bool) -> String {
    let files = match capsule.changes.get(base) {
        None => "not compared".to_owned(),
        Some(changes) if !changes.probed => "not compared (more than 16 base commits)".to_owned(),
        Some(changes) if !changes.complete => {
            "cannot be listed (the base commit is not in the repository, or the current state \
             cannot be read)"
                .to_owned()
        }
        Some(changes) if changes.paths.is_empty() => "none".to_owned(),
        Some(changes) if counted => format!("{} (not listed one by one)", changes.paths.len()),
        Some(changes) => {
            let shown = changes
                .paths
                .iter()
                .take(PACK_CHANGED_FILES_SHOWN)
                .map(|path| quoted(path))
                .collect::<Vec<_>>()
                .join(", ");
            if changes.paths.len() > PACK_CHANGED_FILES_SHOWN {
                format!(
                    "{shown} ({} in all, the first {PACK_CHANGED_FILES_SHOWN} listed)",
                    changes.paths.len()
                )
            } else {
                shown
            }
        }
    };
    format!("Base: {base}; files changed since: {files}")
}

fn numbers(of: &[u64]) -> String {
    of.iter()
        .map(|number| format!("#{number}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The line of a continuation entry that is neither a claim nor a Session.
fn other_line(entry: &Entry) -> Option<String> {
    match entry {
        Entry::ChainInvalid => Some(
            "Integrity: the event chain does not verify; every conclusion is treated as tampered"
                .to_owned(),
        ),
        Entry::Damaged {
            record_type,
            record_id,
            reason,
        } => Some(format!(
            "Integrity: {record_type} {record_id} is missing, does not recompute, or disagrees \
             with the ledger ({}); every conclusion is treated as tampered",
            reason.as_str()
        )),
        Entry::Rerun { argv, numbers: of } => Some(format!(
            "Rerun: {} on the current HEAD (for {})",
            quoted(&argv.join(" ")),
            numbers(of)
        )),
        Entry::Reaffirm {
            argv,
            evidence_id,
            snapshot_id,
            numbers: of,
        } => Some(format!(
            "No rerun needed: {} passed in a later run that matches the current state (evidence \
             {evidence_id}, snapshot {snapshot_id}); a new Checkpoint on that snapshot can cite it \
             and reaffirm {}",
            quoted(&argv.join(" ")),
            numbers(of)
        )),
        Entry::Session(_) | Entry::Claim(_) | Entry::Retired(_) => None,
    }
}

/// ADR 0014 E6 (P4-F, Owner condition g): the Pack a Session declared it
/// continues from; the digests are the Session's own word. P4-H1: the
/// `pack_digest` by its first [`PACK_RESUMED_DIGEST_SHOWN`] digits.
fn resumed(session: &PackSession) -> String {
    session
        .resumed_from
        .as_ref()
        .map_or_else(String::new, |(sequence, digest)| {
            let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
            format!(
                "; continues from the Pack at ledger sequence {sequence} (self-reported, digest \
                 sha256:{})",
                &hex[..hex.len().min(PACK_RESUMED_DIGEST_SHOWN)]
            )
        })
}

/// P4-H1 (Owner, T20): the indexes in `sessions` (the Session numbering,
/// by start) of the Sessions with a `Session ` line: every Session that has
/// not ended; of those that ended, normally or abandoned, the latest one
/// and the latest `abandoned_shown` that ended abandoned (the Pack's
/// rules; [`super::PACK_ABANDONED_SESSIONS_SHOWN`] for a whole Pack). The
/// others are only counted on the `Sessions:` line.
fn listed_sessions(sessions: &[PackSession], abandoned_shown: usize) -> BTreeSet<usize> {
    let mut listed = BTreeSet::new();
    let mut latest_ended = true;
    let mut abandoned = 0;
    for (index, session) in sessions.iter().enumerate().rev() {
        let keep = match session.end {
            SessionEnd::Open => true,
            SessionEnd::Ended | SessionEnd::Crashed => {
                let latest = std::mem::replace(&mut latest_ended, false);
                let shown = session.end == SessionEnd::Crashed && abandoned < abandoned_shown;
                if shown {
                    abandoned += 1;
                }
                latest || shown
            }
        };
        if keep {
            listed.insert(index);
        }
    }
    listed
}

/// U8 A4 (E1 revision 10, Owner O4): `; agent「<name>」(<role>)`, with the
/// parent `, dispatched by Session <j>` or, for one the Capsule does not
/// list, `, dispatched by unlisted Session <id>` inside the parentheses;
/// empty for a Session that declared no agent.
fn agent(session: &PackSession) -> String {
    session.agent.as_ref().map_or_else(String::new, |agent| {
        let parent = match &agent.parent {
            None => String::new(),
            Some(Origin::Numbered(number)) => format!(", dispatched by Session {number}"),
            Some(Origin::Unlisted(session_id)) => {
                format!(", dispatched by unlisted Session {session_id}")
            }
        };
        format!("; agent{}({}{parent})", quoted(&agent.name), agent.role)
    })
}

/// U9 A7 (E1 revision 12, Owner O3): who ended an abandoned Session:
/// `; ended by Session <j>` when the ender's Session is the j-th of the
/// Capsule's `sessions` (whether or not it also named an agent; its agent
/// is on its own line), `; ended by unlisted Session <id>` when the Capsule
/// does not list it, and `; ended by agent「<name>」(<role>)` when only the
/// agent is known; empty for a Session the Capsule names no ender of.
fn ender(session: &PackSession) -> String {
    match &session.ended_by {
        None => String::new(),
        Some(PackEnder::Session(Origin::Numbered(number))) => {
            format!("; ended by Session {number}")
        }
        Some(PackEnder::Session(Origin::Unlisted(session_id))) => {
            format!("; ended by unlisted Session {session_id}")
        }
        Some(PackEnder::Agent(name, role)) => format!("; ended by agent{}({role})", quoted(name)),
    }
}

/// The line of a listed Session ([`listed_sessions`]), with (U8 A4, E1
/// revision 10) its agent right after the parentheses, (P4-F) the Pack it
/// declared it continues from, and (U7 P1, E1 revision 9) at the end, for
/// a Session that has not ended and is not the reader's own, the takeover
/// hint ([`super::pack_takeover_hint`], U10 T7: of `program`, this line's
/// number, and the Session's ID). U10 T1 (Owner O3, E1 revision 13): the line with
/// the hint names the agent too, in the same place, so that a reader taking
/// over sees whose Session it is about to end (D3 F23); this replaces S-F
/// round 2 (Owner, 2026-10-26), under which that line named none. A
/// Session that declared no agent has the line it had. U9 A7 (E1 revision
/// 12): the line of a Session that ended abandoned ends with who ended it
/// ([`ender`]), after everything else; it never has the hint.
fn session_line(number: usize, session: &PackSession, program: &str) -> String {
    let line = match session.end {
        SessionEnd::Crashed => format!(
            "Session {number} {} ended abandoned (started {})",
            session.session_id, session.started_at
        ),
        SessionEnd::Open => format!(
            "Session {number} {} not ended (started {}, {})",
            session.session_id,
            session.started_at,
            session.last_recorded_at.as_ref().map_or_else(
                || "nothing of it in the ledger yet".to_owned(),
                |at| format!("last wrote to the ledger at {at}")
            ),
        ),
        SessionEnd::Ended => format!(
            "Session {number} {} ended normally (started {})",
            session.session_id, session.started_at
        ),
    };
    let end = if session.end == SessionEnd::Open && !session.own {
        super::pack_takeover_hint(program, number, &session.session_id)
    } else {
        ender(session)
    };
    format!("{line}{}{}{end}", agent(session), resumed(session))
}

/// The line of one action (ADR 0014 E3, P4-D): fixed words, numbers, and
/// codes only (Owner condition e); never omitted.
fn action_line(action: &PackAction) -> String {
    let (what, operation) = match action.action {
        "complete_task" => ("complete the Task", "transitionTask"),
        "begin_session" => ("begin a new Session", "createSession"),
        _ => ("record a fact citing the latest run", "createCheckpoint"),
    };
    let mut parts = vec![operation.to_owned()];
    if action.reasons.is_empty() {
        if action.action == "complete_task" {
            parts.push(format!(
                "cites {} passed runs of the current repository state",
                action.cited
            ));
        }
    } else {
        // U5 H5 (Owner O7): `invalid_request` of citing the latest run says
        // why, in the daemon's own words.
        let reasons = action
            .reasons
            .iter()
            .map(
                |reason| match (reason.as_str(), action.invalid_request_detail) {
                    ("invalid_request", Some(detail)) => format!("{reason} ({detail})"),
                    _ => reason.clone(),
                },
            )
            .collect::<Vec<_>>();
        parts.push(format!("reasons: {}", reasons.join(", ")));
    }
    if !action.open_sessions.is_empty() {
        let numbers = action
            .open_sessions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        // U10 T1 (Owner O3; D3 F16): when a new Session can begin, the line
        // says what to do with the open ones first, by their `Session `
        // lines; a refused begin only names them, as before.
        parts.push(
            match (action.reasons.is_empty(), action.open_sessions.len()) {
                (false, _) => format!("open Sessions {numbers}"),
                (true, 1) => format!(
                    "first end it as the line of Session {numbers} says, or confirm it is still \
                     working"
                ),
                (true, _) => format!(
                    "first end them as the lines of Sessions {numbers} say, or confirm they are \
                     still working"
                ),
            },
        );
    }
    parts.push(format!("needs {}", action.scope));
    format!(
        "{}{what} ({})",
        if action.reasons.is_empty() {
            "Can: "
        } else {
            "Cannot: "
        },
        parts.join("; ")
    )
}

impl PackSelection<'_> {
    fn header_line(&self) -> String {
        let capsule = self.capsule;
        format!(
            "Pack: {PACK_FORMAT}; capsule_digest {}; ledger sequence {}; {}; budget {} UTF-16 \
             code units",
            capsule.capsule_digest,
            capsule.project_sequence,
            capsule.head.as_ref().map_or_else(
                || "HEAD not probed".to_owned(),
                |head| format!("probed HEAD {head}")
            ),
            self.budget,
        )
    }

    /// The Task's verdict and the count of each kind, by the Capsule
    /// (never omitted).
    fn task_line(&self) -> String {
        let capsule = self.capsule;
        let active = capsule.active().collect::<Vec<_>>();
        let count = |kind: ClaimKind| active.iter().filter(|claim| claim.kind == kind).count();
        let breakdown = Verdict::ALL
            .into_iter()
            .filter_map(|verdict| {
                let facts = active
                    .iter()
                    .filter(|claim| claim.kind == ClaimKind::Fact && claim.verdict == verdict)
                    .count();
                (facts > 0).then(|| format!("{} {facts}", word(verdict)))
            })
            .collect::<Vec<_>>();
        let standing = |standing: Standing, kind: Option<ClaimKind>| {
            capsule
                .claims
                .values()
                .filter(|claim| claim.standing == standing && kind.is_none_or(|k| claim.kind == k))
                .count()
        };
        format!(
            "Task{} ({}): {}{}; active claims {}: facts {}{}, open items {}, decisions {}, next \
             actions {}; superseded {}; withdrawn {} (facts {})",
            quoted(&capsule.title),
            capsule.task_status,
            word(capsule.verdict),
            if capsule.reasons.is_empty() {
                String::new()
            } else {
                format!(" (reasons: {})", codes(&capsule.reasons))
            },
            active.len(),
            count(ClaimKind::Fact),
            if breakdown.is_empty() {
                String::new()
            } else {
                format!(" ({})", breakdown.join(" / "))
            },
            count(ClaimKind::OpenItem),
            count(ClaimKind::Decision),
            count(ClaimKind::NextAction),
            standing(Standing::Superseded, None),
            standing(Standing::Withdrawn, None),
            standing(Standing::Withdrawn, Some(ClaimKind::Fact)),
        )
    }

    /// Every integrity line; the first adds, in a `tampered` Pack, that the
    /// numbers may differ from before (P4-C2 revision 6).
    fn integrity_lines(&self) -> Vec<String> {
        let mut lines = self
            .capsule
            .entries
            .iter()
            .filter(|entry| matches!(entry, Entry::ChainInvalid | Entry::Damaged { .. }))
            .filter_map(other_line)
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(
                "Integrity: no inconsistency found (the event chain is valid, and every record of \
                 this Task recomputes and agrees with the ledger)"
                    .to_owned(),
            );
        }
        if self.capsule.verdict == Verdict::Tampered {
            write!(lines[0], "; {PACK_TAMPERED_NUMBERS}").expect("writing to a String cannot fail");
        }
        lines
    }

    /// Every Session counted by how it ended, and (P4-H1) how many
    /// declared the Pack they continue from (never omitted).
    fn session_summary(&self) -> String {
        let sessions = &self.capsule.sessions;
        let count = |end: SessionEnd| sessions.iter().filter(|session| session.end == end).count();
        format!(
            "Sessions: {} in all, {} ended normally, {} ended abandoned, {} not ended, {} \
             declared a resumed_from Pack",
            sessions.len(),
            count(SessionEnd::Ended),
            count(SessionEnd::Crashed),
            count(SessionEnd::Open),
            sessions
                .iter()
                .filter(|session| session.resumed_from.is_some())
                .count(),
        )
    }

    /// The base lines of the claims that still have a line.
    pub(super) fn base_lines(&self) -> Vec<String> {
        self.base_order(|number| self.listed(number))
            .iter()
            .map(|base| base_line(self.capsule, base, self.counted.contains(base)))
            .collect()
    }

    /// One active claim's line; `None` when it is folded or unlisted.
    pub(super) fn claim_line(&self, number: u64) -> Option<String> {
        let cut = self.cut(number).unwrap_or(PackCut::Full);
        if matches!(cut, PackCut::Folded | PackCut::Unlisted) {
            return None;
        }
        Some(claim_line(&self.capsule.claims[&number], cut))
    }

    pub(super) fn fold_line(&self) -> Option<String> {
        let folded = self.numbers_cut(PackCut::Folded);
        (!folded.is_empty()).then(|| {
            format!(
                "Folded: verified active facts {}: {} (bodies and evidence bindings not listed one \
                 by one)",
                folded.len(),
                ranges(&folded)
            )
        })
    }

    fn numbers_cut(&self, cut: PackCut) -> Vec<u64> {
        self.cuts
            .iter()
            .filter(|(_, seen)| **seen == cut)
            .map(|(number, _)| *number)
            .collect()
    }

    /// Level 5: the claims no longer listed, the facts by verdict, the
    /// Decisions and open items by kind only (P4-C2 revision 4, decision
    /// two), with their numbers.
    pub(super) fn unlisted_line(&self) -> Option<String> {
        let unlisted = self.numbers_cut(PackCut::Unlisted);
        if unlisted.is_empty() {
            return None;
        }
        let (claims, retired): (Vec<&PackClaim>, Vec<&PackClaim>) = unlisted
            .iter()
            .map(|number| &self.capsule.claims[number])
            .partition(|claim| claim.standing == Standing::Active);
        let of_kind = |kind: ClaimKind| {
            claims
                .iter()
                .filter(|claim| claim.kind == kind)
                .copied()
                .collect::<Vec<_>>()
        };
        let numbers_of = |claims: &[&PackClaim]| {
            ranges(&claims.iter().map(|claim| claim.number).collect::<Vec<_>>())
        };
        let mut parts = Vec::new();
        let facts = of_kind(ClaimKind::Fact);
        if !facts.is_empty() {
            let by_verdict = Verdict::ALL
                .into_iter()
                .filter_map(|verdict| {
                    let of = facts
                        .iter()
                        .filter(|claim| claim.verdict == verdict)
                        .copied()
                        .collect::<Vec<_>>();
                    (!of.is_empty())
                        .then(|| format!("{} {}: {}", word(verdict), of.len(), numbers_of(&of)))
                })
                .collect::<Vec<_>>();
            parts.push(format!(
                "facts {} ({})",
                facts.len(),
                by_verdict.join(" / ")
            ));
        }
        for kind in [ClaimKind::Decision, ClaimKind::OpenItem] {
            let of = of_kind(kind);
            if !of.is_empty() {
                parts.push(format!(
                    "{}s {}: {}",
                    kind_word(kind),
                    of.len(),
                    numbers_of(&of)
                ));
            }
        }
        // U3 G4: a query Pack's superseded and withdrawn claims, by number
        // only (their lines show no verdict).
        if !retired.is_empty() {
            parts.push(format!(
                "superseded or withdrawn {}: {}",
                retired.len(),
                numbers_of(&retired)
            ));
        }
        Some(format!(
            "Not listed: {} (bodies and evidence bindings not listed one by one)",
            parts.join("; ")
        ))
    }

    pub(super) fn omission_line(&self) -> String {
        format!(
            "Omitted: level {}; bases with files only counted {}, bodies shortened {}, bodies \
             omitted {}, folded into one line {}, not listed {}; full text of given numbers: \
             {PACK_FULL_TEXT_COMMAND} (such as 3,5-7)",
            self.level,
            self.base_order(|number| self.listed(number))
                .iter()
                .filter(|base| self.counted.contains(*base))
                .count(),
            self.count(PackCut::Shortened),
            self.count(PackCut::Omitted),
            self.count(PackCut::Folded),
            self.count(PackCut::Unlisted),
        )
    }

    fn retired_line(&self) -> Option<String> {
        let numbers = |standing: Standing| {
            self.capsule
                .claims
                .values()
                .filter(|claim| claim.standing == standing)
                .map(|claim| claim.number)
                .collect::<Vec<_>>()
        };
        let part = |label: &str, numbers: &[u64]| {
            if numbers.is_empty() {
                format!("{label} 0")
            } else {
                format!("{label} {}: {}", numbers.len(), ranges(numbers))
            }
        };
        let superseded = numbers(Standing::Superseded);
        let withdrawn = numbers(Standing::Withdrawn);
        (!superseded.is_empty() || !withdrawn.is_empty()).then(|| {
            format!(
                "Superseded and withdrawn: {}; {} (not listed one by one)",
                part("superseded", &superseded),
                part("withdrawn", &withdrawn)
            )
        })
    }

    /// U5 P1 (Owner O3, D1 F10): `Last passed: 「<argv>」(<12 digits>)`, the
    /// passed test run the Capsule names (`PackCapsule::last_passed`), right
    /// after the Session lines; never omitted, in a query Pack too. U8 K5
    /// (E1 revision 11): the parenthesis ends with `;` and the `memprove
    /// run` arguments that rerun that run as a test run
    /// ([`rerun_arguments_for`]: U9 K2, the run's own recorded format first).
    fn last_passed_line(&self) -> Option<String> {
        self.capsule
            .last_passed
            .as_ref()
            .map(|(argv, commit, format)| {
                format!(
                    "Last passed: {} ({}; {})",
                    quoted(&argv.join(" ")),
                    short(commit),
                    rerun_arguments_for(argv, *format)
                )
            })
    }

    /// U3 G4 (Owner O9): the `Query:` line of a query Pack: the query in
    /// `「」`, how many claims it matched, and their numbers, best first.
    fn query_line(&self) -> Option<String> {
        let query = self.query()?;
        let matches = query.matches();
        let mut line = format!("Query: {}; {} matches", quoted(query.text()), matches.len());
        if !matches.is_empty() {
            write!(
                line,
                ", by rank: {}",
                matches
                    .iter()
                    .map(|number| format!("#{number}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .expect("writing to a String cannot fail");
        }
        Some(line)
    }

    /// Every line but the digest line, in order.
    fn lines(&self) -> Vec<String> {
        let capsule = self.capsule;
        let mut lines = vec![
            self.header_line(),
            format!(
                "Ontology: {} (the version of the verdict rules)",
                capsule.ontology_version
            ),
        ];
        lines.extend(self.query_line());
        lines.push(self.task_line());
        lines.extend(self.integrity_lines());
        lines.push(self.session_summary());
        let listed = listed_sessions(&capsule.sessions, self.rules.abandoned_sessions_listed);
        for (index, session) in capsule.sessions.iter().enumerate() {
            if listed.contains(&index) {
                lines.push(session_line(index + 1, session, &capsule.takeover_program));
            }
        }
        lines.extend(self.last_passed_line());
        lines.extend(capsule.actions.iter().map(action_line));
        lines.extend(self.base_lines());
        let mut folded = false;
        if self.query.is_some() {
            // U3 G4: the claims of a query Pack in their order; no reruns,
            // and no `Superseded and withdrawn:` line (it would say its claims are not
            // listed one by one).
            for number in &self.order {
                if self.cut(*number) == Some(PackCut::Folded) {
                    if !folded {
                        lines.extend(self.fold_line());
                        folded = true;
                    }
                } else {
                    lines.extend(self.claim_line(*number));
                }
            }
            lines.extend(self.unlisted_line());
        } else {
            for entry in &capsule.entries {
                match entry {
                    Entry::Claim(number) if self.cut(*number) == Some(PackCut::Folded) => {
                        if !folded {
                            lines.extend(self.fold_line());
                            folded = true;
                        }
                    }
                    Entry::Claim(number) => lines.extend(self.claim_line(*number)),
                    Entry::Rerun { .. } | Entry::Reaffirm { .. } => {
                        lines.extend(other_line(entry));
                    }
                    _ => {}
                }
            }
            lines.extend(self.unlisted_line());
            lines.extend(self.retired_line());
        }
        if !capsule.gaps.is_empty() {
            lines.push(format!("Gaps: {}", capsule.gaps.join(", ")));
        }
        lines.push(self.omission_line());
        lines
    }
}

/// The Pack of a selection: its lines, each ending in a line break, then
/// `Digest: sha256:<pack_digest>`, the SHA-256 of the UTF-8 bytes of every
/// line before it (the budget is on the first line, so it is part of the
/// digest).
#[must_use]
pub fn render_pack(selection: &PackSelection<'_>) -> String {
    let mut text = String::new();
    for line in selection.lines() {
        text.push_str(&line);
        text.push('\n');
    }
    let digest = Sha256Digest::of_bytes(text.as_bytes());
    writeln!(text, "Digest: sha256:{digest}").expect("writing to a String cannot fail");
    text
}

/// The claims `ranges` names, in continuation order, superseded and
/// withdrawn ones too.
///
/// # Errors
///
/// [`PackError::NoSuchClaim`] for the first number the Capsule does not
/// list.
fn claims_named<'a>(
    capsule: &'a PackCapsule,
    ranges: &[(u64, u64)],
) -> Result<Vec<&'a PackClaim>, PackError> {
    let listed = capsule.claims.keys().copied().collect::<BTreeSet<_>>();
    for (first, last) in ranges {
        let mut expected = Some(*first);
        for number in listed.range(first..=last) {
            if expected != Some(*number) {
                break;
            }
            expected = number.checked_add(1);
        }
        if let Some(number) = expected.filter(|missing| missing <= last) {
            return Err(PackError::NoSuchClaim {
                number,
                listed: match (listed.first(), listed.last()) {
                    (Some(low), Some(high)) => format!("#{low}–#{high}"),
                    _ => "no claim".to_owned(),
                },
            });
        }
    }
    Ok(capsule
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Claim(number) | Entry::Retired(number) => Some(*number),
            _ => None,
        })
        .filter(|number| {
            ranges
                .iter()
                .any(|(first, last)| (*first..=*last).contains(number))
        })
        .map(|number| &capsule.claims[&number])
        .collect())
}

/// The lines of `wanted` with their cuts: the base line of each base commit
/// the listed claims name (its changed files only counted when it is in
/// `counted`), then each claim not [`PackCut::Unlisted`].
fn claims_text(
    capsule: &PackCapsule,
    wanted: &[&PackClaim],
    cuts: &BTreeMap<u64, PackCut>,
    counted: &BTreeSet<String>,
) -> String {
    let cut = |claim: &PackClaim| cuts.get(&claim.number).copied().unwrap_or(PackCut::Full);
    let listed = wanted
        .iter()
        .filter(|claim| cut(claim) != PackCut::Unlisted)
        .collect::<Vec<_>>();
    let mut bases: Vec<&String> = Vec::new();
    for base in listed.iter().flat_map(|claim| &claim.bases) {
        if !bases.contains(&base) {
            bases.push(base);
        }
    }
    let mut text = String::new();
    for line in bases
        .iter()
        .map(|base| base_line(capsule, base, counted.contains(*base)))
        .chain(listed.iter().map(|claim| claim_line(claim, cut(claim))))
    {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// P4-C2 revision 5 (`resume --pack --claims`): the full line of each claim
/// whose number one of `ranges` (inclusive) holds, superseded and withdrawn
/// ones too, in continuation order, after the base line of each base
/// commit they name; in the Pack's line formats and escapes, with no
/// budget. Each line ends in a line break; there is no first or digest
/// line.
///
/// # Errors
///
/// [`PackError::NoSuchClaim`] for the first number the Capsule does not
/// list.
pub fn render_pack_claims(
    capsule: &PackCapsule,
    ranges: &[(u64, u64)],
) -> Result<String, PackError> {
    let wanted = claims_named(capsule, ranges)?;
    Ok(claims_text(
        capsule,
        &wanted,
        &BTreeMap::new(),
        &BTreeSet::new(),
    ))
}

/// U1 (A5, original #26): [`render_pack_claims`] within `budget` UTF-16
/// code units, for the MCP `claims` tool. When the full lines fit they are
/// returned as they are. Otherwise the Pack's steps run in order until the
/// text fits, and a last `Omitted:` line says what was cut and how to read the
/// whole lines:
///
/// 1. the changed files of each base commit, one base line at a time, are
///    only counted;
/// 2. bodies longer than [`PACK_BODY_KEPT`] characters are shortened to
///    their first [`PACK_BODY_KEPT`], one claim at a time, in the order of
///    the lines (an active next action's body is never cut, as in the
///    Pack);
/// 3. whole bodies are omitted, in the same order;
/// 5. claims are no longer listed, one at a time, in the order of the
///    lines, an active next action last; the omission line numbers them.
///
/// There is no level 4: the claims named are listed one by one, never
/// folded.
///
/// # Errors
///
/// [`PackError::NoSuchClaim`] as [`render_pack_claims`];
/// [`PackError::OverBudget`] when even the omission line alone does not
/// fit.
pub fn render_pack_claims_within(
    capsule: &PackCapsule,
    ranges: &[(u64, u64)],
    budget: usize,
) -> Result<String, PackError> {
    let wanted = claims_named(capsule, ranges)?;
    let mut cuts = BTreeMap::new();
    let mut counted = BTreeSet::new();
    let full = claims_text(capsule, &wanted, &cuts, &counted);
    if units(&full) <= budget {
        return Ok(full);
    }
    let next_action = |claim: &PackClaim| {
        claim.kind == ClaimKind::NextAction && claim.standing == Standing::Active
    };
    let mut steps: Vec<(u8, Step)> = Vec::new();
    for claim in &wanted {
        for base in &claim.bases {
            if !steps
                .iter()
                .any(|(_, step)| matches!(step, Step::Count(seen) if seen == base))
            {
                steps.push((1, Step::Count(base.clone())));
            }
        }
    }
    for claim in wanted.iter().filter(|claim| !next_action(claim)) {
        if claim.body_chars() > PACK_BODY_KEPT {
            steps.push((2, Step::Cut(claim.number, PackCut::Shortened)));
        }
    }
    for claim in wanted.iter().filter(|claim| !next_action(claim)) {
        steps.push((3, Step::Cut(claim.number, PackCut::Omitted)));
    }
    for claim in wanted
        .iter()
        .filter(|claim| !next_action(claim))
        .chain(wanted.iter().filter(|claim| next_action(claim)))
    {
        steps.push((5, Step::Cut(claim.number, PackCut::Unlisted)));
    }
    let mut text = full;
    for (level, step) in steps {
        match step {
            Step::Count(base) => {
                counted.insert(base);
            }
            Step::Cut(number, cut) => {
                cuts.insert(number, cut);
            }
            Step::Fold => {}
        }
        text = claims_text(capsule, &wanted, &cuts, &counted);
        text.push_str(&claims_omission_line(level, &wanted, &cuts, &counted));
        text.push('\n');
        if units(&text) <= budget {
            return Ok(text);
        }
    }
    Err(PackError::OverBudget {
        budget,
        needed: units(&text),
    })
}

/// The `Omitted:` line of [`render_pack_claims_within`]: the level reached,
/// how many base lines only count their files, how many bodies were
/// shortened and omitted, and the numbers of the claims not listed.
fn claims_omission_line(
    level: u8,
    wanted: &[&PackClaim],
    cuts: &BTreeMap<u64, PackCut>,
    counted: &BTreeSet<String>,
) -> String {
    let count = |cut: PackCut| cuts.values().filter(|seen| **seen == cut).count();
    let unlisted = wanted
        .iter()
        .map(|claim| claim.number)
        .filter(|number| cuts.get(number) == Some(&PackCut::Unlisted))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let unlisted_numbers = if unlisted.is_empty() {
        String::new()
    } else {
        format!(" ({})", ranges(&unlisted))
    };
    format!(
        "Omitted: level {level}; bases with files only counted {}, bodies shortened {}, bodies \
         omitted {}, not listed {}{unlisted_numbers}; full text of given numbers: \
         {PACK_FULL_TEXT_COMMAND} (such as 3,5-7)",
        counted.len(),
        count(PackCut::Shortened),
        count(PackCut::Omitted),
        unlisted.len(),
    )
}
