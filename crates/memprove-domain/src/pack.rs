//! The Context Pack (ADR 0014 E1, E2; P4-C, revised in P4-C2): a budgeted
//! text rendering of a Task Capsule, one item per line, for a fresh agent
//! to read.
//!
//! Nothing here does IO. [`PackCapsule::from_json`] reads the JCS bytes of
//! `getTaskCapsule`; [`select_pack`] decides, for a budget in UTF-16 code
//! units, what the Pack keeps (E1, as revised in P4-C2); [`render_pack`]
//! writes the lines; [`render_pack_claims`] writes the full lines of given
//! claim numbers, with no budget. The CLI (`resume --pack`), and later the
//! MCP server, only print the result. The same Capsule bytes and the same
//! budget give the same Pack, byte for byte: nothing in it comes from a
//! clock or from the local context file. U7 P1: the one input besides the
//! Capsule is the Session the reader calls its own
//! ([`PackCapsule::with_own_session`]), which the CLI takes from the
//! context it uses; a Session that has not ended and is not that one gets
//! the takeover hint ([`pack_takeover_hint`]) at the end of its line. U10
//! T1 (Owner O3, E1 revision 13): that line names the Session's agent like
//! every other (S-F round 2, Owner 2026-10-26, under which it named none,
//! is replaced). U10 T7: the hint names the program to run
//! ([`PackCapsule::with_takeover_program`]), the second input besides the
//! Capsule; the CLI, the hook, and the MCP server give the same one in the
//! same environment.
//!
//! The omission never makes the Task look better than the Capsule says:
//! verdicts, reason codes, and counts are never omitted, nor are the
//! Session lines (P4-H1: one for every Session that has not ended, the
//! latest that ended, and the latest three that ended abandoned; the
//! others are counted). The steps, in
//! order, stop as soon as the Pack fits: the changed files of a base commit
//! are only counted (level 1), bodies are shortened (level 2), then omitted
//! (level 3), the active `verified` facts are folded into one line (level
//! 4), then claims are no longer listed one by one, only counted by kind
//! (and, for facts, by verdict) with their numbers (level 5). The next
//! action is always listed in full. When even that does not fit, there is
//! no Pack ([`PackError::OverBudget`]).
//!
//! Text safety (E2): every line starts with one of a closed set of prefixes
//! ([`PACK_LINE_PREFIXES`]); everything an agent or a database row wrote
//! (statement text, a Decision and its rationale, a withdrawal reason, the
//! Task title, argv, changed file names) is inside `「」`, where `「`, `」`,
//! and the backslash take a backslash and every character of
//! [`PACK_ESCAPED_RANGES`] (and all of `General_Category=Cc`) is written as
//! a visible escape, so agent text can neither start a line nor leave its
//! quote. Strings the daemon writes (IDs, commits, timestamps, codes) are
//! shown only when they are plain ASCII; anything else makes the Capsule
//! malformed for the Pack.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use thiserror::Error;

use crate::{ClaimKind, ReasonCode, Standing, TestFormat, Verdict};

/// The Pack format the first line names.
pub const PACK_FORMAT: &str = "memprove-pack v1";
/// Characters (Unicode scalar values) of a body that level 2 keeps; a body
/// of at most this many characters is never shortened.
pub const PACK_BODY_KEPT: usize = 120;
/// The budget when none is given, in UTF-16 code units (E1 revision a).
pub const PACK_DEFAULT_BUDGET: usize = 9_000;
/// The smallest budget the CLI accepts.
pub const PACK_MIN_BUDGET: usize = 1_000;
/// The largest budget the CLI accepts.
pub const PACK_MAX_BUDGET: usize = 1_000_000;

// ---------------------------------------------------------------------
// The rules a Pack is made by (U16: the constants of the former query
// profiles, with their values unchanged).
// ---------------------------------------------------------------------

/// The budget of the MCP `pack` tool when none is given, in UTF-16 code
/// units (ADR 0014 E7.6): a default call stays under Claude Code's
/// 10,000-token warning line even for a Pack of CJK text.
pub const PACK_MCP_DEFAULT_BUDGET: usize = 6_000;
/// The most UTF-16 code units the `SessionStart` hook prints, the Pack and
/// what surrounds it (ADR 0014 E1 revision a, Owner condition n).
pub const PACK_HOOK_OUTPUT_LIMIT: usize = 10_000;
/// The tokenizer of the in-memory FTS5 index a query Pack is ranked by
/// (U3 G4).
pub const PACK_SEARCH_TOKENIZER: &str = "trigram";
/// The fewest characters of a query term the index matches; a shorter
/// Latin or digit term matches only as a whole word, a shorter CJK term as
/// a substring (U4 S2).
pub const PACK_SEARCH_MIN_TERM_CHARS: usize = 3;
/// The most characters (Unicode scalar values) of a query; the fewest is 1
/// (U3 G4, Owner O8).
pub const PACK_QUERY_MAX_CHARS: usize = 256;

/// A group of claims an omission step takes, lower numbers first within
/// the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimGroup {
    /// Facts judged `verified`.
    VerifiedFacts,
    /// Facts judged anything else.
    OtherFacts,
    /// Decisions.
    Decisions,
    /// Open items.
    OpenItems,
    /// U3 G4: superseded and withdrawn claims of any kind, which only a
    /// query Pack lists.
    Retired,
}

/// One omission level, in the order the levels run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmissionStep {
    /// The changed files of each base commit are only counted, one base
    /// line at a time, in the order the lines name them.
    CountChangedFiles,
    /// Bodies longer than the rules' `body_kept` characters are shortened
    /// to that many, one claim at a time, group by group.
    ShortenBodies(&'static [ClaimGroup]),
    /// Whole bodies are omitted, one claim at a time, group by group.
    OmitBodies(&'static [ClaimGroup]),
    /// The active `verified` facts are folded into one line.
    FoldVerifiedFacts,
    /// Claims are no longer listed one by one, group by group; they are
    /// counted, with their numbers, on one line.
    Unlist(&'static [ClaimGroup]),
}

/// How a Pack is cut to its budget: the kinds always listed in full, how
/// many abandoned Sessions have a line, the characters a shortened body
/// keeps, and the omission levels in order (ADR 0014 E1, as revised in
/// P4-C2 and P4-H1). When the last level does not fit, there is no Pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRules {
    /// Kinds that are always listed in full, at every level.
    pub never_omitted: &'static [ClaimKind],
    /// Of the Sessions that ended, how many of the latest that ended
    /// abandoned have a `Session ` line, besides every Session that has not
    /// ended and the latest that ended (P4-H1).
    pub abandoned_sessions_listed: usize,
    /// The characters (Unicode scalar values) a shortened body keeps.
    pub body_kept: usize,
    /// The omission levels, level 1 first.
    pub levels: &'static [OmissionStep],
}

/// The order levels 2 and 3 take bodies in (ADR 0014 E1 revision b).
const BODY_ORDER: &[ClaimGroup] = &[
    ClaimGroup::VerifiedFacts,
    ClaimGroup::OtherFacts,
    ClaimGroup::Decisions,
    ClaimGroup::OpenItems,
];

/// The rules of a whole Pack (`resume --pack`): ADR 0014 E1 as revised in
/// P4-C2 (revision 4) and P4-H1.
pub const RESUME_RULES: PackRules = PackRules {
    never_omitted: &[ClaimKind::NextAction],
    abandoned_sessions_listed: PACK_ABANDONED_SESSIONS_SHOWN,
    body_kept: PACK_BODY_KEPT,
    levels: &[
        OmissionStep::CountChangedFiles,
        OmissionStep::ShortenBodies(BODY_ORDER),
        OmissionStep::OmitBodies(BODY_ORDER),
        OmissionStep::FoldVerifiedFacts,
        // The verified facts are folded at level 4 when there are any.
        OmissionStep::Unlist(&[
            ClaimGroup::OtherFacts,
            ClaimGroup::Decisions,
            ClaimGroup::OpenItems,
        ]),
    ],
};

/// U3 G4: the order levels 2 and 3 of a query Pack take bodies in: the
/// superseded and withdrawn claims the query brought in first, then as in
/// a whole Pack.
const SEARCH_BODY_ORDER: &[ClaimGroup] = &[
    ClaimGroup::Retired,
    ClaimGroup::VerifiedFacts,
    ClaimGroup::OtherFacts,
    ClaimGroup::Decisions,
    ClaimGroup::OpenItems,
];

/// The rules of a query Pack (`resume --pack --query`, U3 G4): the same
/// levels as [`RESUME_RULES`], the superseded and withdrawn claims first.
pub const SEARCH_RULES: PackRules = PackRules {
    never_omitted: &[ClaimKind::NextAction],
    abandoned_sessions_listed: PACK_ABANDONED_SESSIONS_SHOWN,
    body_kept: PACK_BODY_KEPT,
    levels: &[
        OmissionStep::CountChangedFiles,
        OmissionStep::ShortenBodies(SEARCH_BODY_ORDER),
        OmissionStep::OmitBodies(SEARCH_BODY_ORDER),
        OmissionStep::FoldVerifiedFacts,
        OmissionStep::Unlist(&[
            ClaimGroup::Retired,
            ClaimGroup::OtherFacts,
            ClaimGroup::Decisions,
            ClaimGroup::OpenItems,
        ]),
    ],
};
/// Most changed files listed per base commit, as `resume --text` lists
/// them; the rest are counted.
pub const PACK_CHANGED_FILES_SHOWN: usize = 20;
/// Hexadecimal digits of a commit a claim line shows (P4-C2 revision 1);
/// its base line gives it in full.
pub const PACK_COMMIT_SHOWN: usize = 12;
/// Hexadecimal digits of the `pack_digest` a Session declared that its
/// `Session ` line shows (P4-H1).
pub const PACK_RESUMED_DIGEST_SHOWN: usize = 12;
/// How many of the latest Sessions that ended abandoned have a `Session `
/// line (P4-H1), besides every Session that has not ended and the latest
/// Session that ended; the others are only counted on the `Sessions:` line.
pub const PACK_ABANDONED_SESSIONS_SHOWN: usize = 3;
/// The command that shows the full lines of given claim numbers, which the
/// omission line names (P4-C2 revision 5).
pub const PACK_FULL_TEXT_COMMAND: &str = "memprove resume --pack --claims <numbers>";
/// What the first integrity line of a `tampered` Pack adds (P4-C2
/// revision 6, T19 N-4): a record that does not read takes its claims out
/// of the numbering.
pub const PACK_TAMPERED_NUMBERS: &str = "claim numbers may differ from before";
/// U10 T7 (Owner O4; D3 F24, F30): the program the takeover hint names
/// when the reader gives none ([`PackCapsule::with_takeover_program`]).
pub const PACK_TAKEOVER_PROGRAM: &str = "memprove";
/// The most bytes of a program the takeover hint names.
pub const PACK_TAKEOVER_PROGRAM_MAX: usize = 1024;

/// What the `Session ` line of a Session that has not ended adds at its end
/// when the Session is not the reader's own (U7 P1, E1 revision 9; D2 F16,
/// Owner O3): how to end it before taking over. Fixed words, the program,
/// the line's number, and the Session's ID only; the reader's own Session
/// ([`PackCapsule::with_own_session`]) has no such end. U10 T1 (Owner O3,
/// E1 revision 13): the line with the hint has the `; agent「…」(…)` of a
/// Session that declared an agent, where U8 A4 puts it; this replaces S-F
/// round 2 (Owner, 2026-10-26), under which it had none. U10 T7 (Owner
/// O4; D3 F24, F30): the hint is a command to copy as it stands,
/// `<program> session end <ID> --abandoned --reason <why>`, which needs no
/// context and no `--revision`, and the MCP form names the line's own
/// number, the `end` tool's `session`; before, it was `memprove end
/// --abandoned --reason <why> (MCP: end {session, abandoned:true})`, which
/// ends only the Session of the reader's own context. U17 (M1 O3): the
/// words are English.
#[must_use]
pub fn pack_takeover_hint(program: &str, number: usize, session_id: &str) -> String {
    format!(
        "; end it before taking over: {program} session end {session_id} --abandoned --reason \
         <why> (MCP: end {{session:{number}, abandoned:true}})"
    )
}

/// U10 T7: whether `program` may be the program of the takeover hint:
/// [`PACK_TAKEOVER_PROGRAM`], or an absolute path of at most
/// [`PACK_TAKEOVER_PROGRAM_MAX`] bytes made only of ASCII letters, digits,
/// `/`, `.`, `_`, `-`, and `+`, so that it is one plain word of a shell
/// command and plain text in the Pack (E2).
#[must_use]
pub fn is_takeover_program(program: &str) -> bool {
    program == PACK_TAKEOVER_PROGRAM
        || (program.starts_with('/')
            && program.len() > 1
            && program.len() <= PACK_TAKEOVER_PROGRAM_MAX
            && program
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+".contains(&byte)))
}

/// The `memprove run` arguments that record a run of `cargo test` as a
/// test run, counted from `cargo`'s own summary (U8 K5, E1 revision 11).
pub const RERUN_CARGO: &str = "--test --test-format cargo";
/// The same for `pytest`, counted from its summary.
pub const RERUN_PYTEST: &str = "--test --test-format pytest";
/// The same for any other command: the number of cases is the caller's,
/// `<N>` standing for it.
pub const RERUN_CASES: &str = "--test --test-cases <N>";

/// U8 K5 (E1 revision 11; U7 V-N12): the `memprove run` arguments that
/// rerun the command `argv` as a test run, after the commit on the
/// `Last passed:` line and in the sentences that tell an agent to rerun a
/// command run. Only the recorded `argv` is read, for the first word that
/// names a runner: `cargo` followed by `test` (words that start with `+`,
/// a toolchain, are skipped between them) is [`RERUN_CARGO`]; `pytest` or
/// `py.test` (by the last component of a path), or `-m` followed by
/// `pytest`, is [`RERUN_PYTEST`]; an argv that names neither is
/// [`RERUN_CASES`]. The ledger does not record which `--test-format` a run
/// was made with, so this is not a record of it but a way that works.
#[must_use]
pub fn rerun_arguments(argv: &[String]) -> &'static str {
    let last_component = |word: &str| word.rsplit('/').next().unwrap_or(word).to_owned();
    for (at, word) in argv.iter().enumerate() {
        let next = argv[at + 1..].iter().find(|next| !next.starts_with('+'));
        if last_component(word) == "cargo" && next.is_some_and(|next| next == "test") {
            return RERUN_CARGO;
        }
        if matches!(last_component(word).as_str(), "pytest" | "py.test")
            || (word == "-m" && argv.get(at + 1).is_some_and(|next| next == "pytest"))
        {
            return RERUN_PYTEST;
        }
    }
    RERUN_CASES
}

/// U9 K2 (Owner O6): [`rerun_arguments`] for a test run that may have
/// recorded the `--test-format` it was made with: the record decides when
/// there is one (`cargo` and `pytest` as their constants, `cases` as
/// [`RERUN_CASES`]), and an argv that no record speaks for (a run before U9,
/// or a run that named none) is read as [`rerun_arguments`] reads it.
#[must_use]
pub fn rerun_arguments_for(argv: &[String], format: Option<TestFormat>) -> &'static str {
    match format {
        Some(TestFormat::Cargo) => RERUN_CARGO,
        Some(TestFormat::Pytest) => RERUN_PYTEST,
        Some(TestFormat::Cases) => RERUN_CASES,
        None => rerun_arguments(argv),
    }
}

/// The closed set of line prefixes (E2, as revised in P4-C2; U17: in
/// English). `#` stands for `#<number> `, the line of one claim. `Can:`
/// and `Cannot:` are the actions of E3 (P4-D), one line each, after the
/// Session lines; they are never omitted. `Query:` (U3 G4, Owner O9) is
/// the query of a query Pack, right after the `Ontology:` line, and only
/// there. `Last passed:` (U5 P1, Owner O3) names the passed test run the
/// Capsule shows, right after the Session lines, when there is no `Rerun:`
/// line; never omitted. `Session ` is the line of one Session; `Sessions:`
/// counts them.
pub const PACK_LINE_PREFIXES: [&str; 20] = [
    "Pack: ",
    "Ontology: ",
    "Query: ",
    "Task「",
    "Integrity: ",
    "Sessions: ",
    "Session ",
    "Last passed: ",
    "Base: ",
    "#",
    "Rerun: ",
    "No rerun needed: ",
    "Folded: ",
    "Not listed: ",
    "Can: ",
    "Cannot: ",
    "Superseded and withdrawn: ",
    "Gaps: ",
    "Omitted: ",
    "Digest: ",
];

/// The Unicode version whose `General_Category` the ranges below follow.
pub const PACK_ESCAPES_UNICODE_VERSION: &str = "16.0.0";

/// Characters a quote shows as `\u{..}`, besides all of
/// `General_Category=Cc` (E2): all of Cf (Unicode 16.0.0), U+2028, U+2029,
/// the whole tag block, the look-alikes of `」` and the blank letters of
/// P3-L O4, and T16 N3's characters: the variation selectors
/// (U+FE00–U+FE0F, U+E0100–U+E01EF), the Mongolian free variation
/// selectors (U+180B–U+180F), Khmer U+17B4 and U+17B5, the unassigned
/// U+2065, U+FFF0–U+FFF8, and U+E0080–U+E0FFF, the combining grapheme
/// joiner U+034F, the look-alikes of a closing delimiter (┘ U+2518, ⌟
/// U+231F, ⸥ U+2E25, ┛ U+251B, ⸣ U+2E23), and the wide blanks U+3000 and
/// U+2800. Adjacent ranges are merged.
pub const PACK_ESCAPED_RANGES: [(char, char); 36] = [
    ('\u{00ad}', '\u{00ad}'),
    ('\u{034f}', '\u{034f}'),
    ('\u{0600}', '\u{0605}'),
    ('\u{061c}', '\u{061c}'),
    ('\u{06dd}', '\u{06dd}'),
    ('\u{070f}', '\u{070f}'),
    ('\u{0890}', '\u{0891}'),
    ('\u{08e2}', '\u{08e2}'),
    ('\u{115f}', '\u{1160}'),
    ('\u{17b4}', '\u{17b5}'),
    ('\u{180b}', '\u{180f}'),
    ('\u{200b}', '\u{200f}'),
    ('\u{2028}', '\u{202e}'),
    ('\u{2060}', '\u{206f}'),
    ('\u{231f}', '\u{231f}'),
    ('\u{2518}', '\u{2518}'),
    ('\u{251b}', '\u{251b}'),
    ('\u{2800}', '\u{2800}'),
    ('\u{2e23}', '\u{2e23}'),
    ('\u{2e25}', '\u{2e25}'),
    ('\u{3000}', '\u{3000}'),
    ('\u{300f}', '\u{300f}'),
    ('\u{3164}', '\u{3164}'),
    ('\u{fe00}', '\u{fe0f}'),
    ('\u{fe42}', '\u{fe42}'),
    ('\u{fe44}', '\u{fe44}'),
    ('\u{feff}', '\u{feff}'),
    ('\u{ff63}', '\u{ff63}'),
    ('\u{ffa0}', '\u{ffa0}'),
    ('\u{fff0}', '\u{fffb}'),
    ('\u{110bd}', '\u{110bd}'),
    ('\u{110cd}', '\u{110cd}'),
    ('\u{13430}', '\u{1343f}'),
    ('\u{1bca0}', '\u{1bca3}'),
    ('\u{1d173}', '\u{1d17a}'),
    ('\u{e0000}', '\u{e0fff}'),
];

/// Why no Pack could be made.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PackError {
    /// The Capsule is not one the Pack can render.
    #[error("the Task Capsule cannot be rendered as a Pack: {0}")]
    Malformed(String),
    /// Even level 5 is longer than the budget.
    #[error("the Pack needs {needed} UTF-16 code units even at level 5; the budget is {budget}")]
    OverBudget {
        /// The budget asked for.
        budget: usize,
        /// The least budget at which level 5 fits: the length of the
        /// level-5 Pack, which names its budget on the first line, at that
        /// budget.
        needed: usize,
    },
    /// U3 G4: the query's matches name a claim the Capsule does not list,
    /// or one twice.
    #[error("the query's matches do not fit the Task Capsule: {0}")]
    BadMatches(String),
    /// A claim number the Capsule does not list (`--claims`).
    #[error("names #{number}, which the Task Capsule does not list; it lists {listed}")]
    NoSuchClaim {
        /// The first number named that the Capsule does not list.
        number: u64,
        /// The numbers it lists, `#1–#N` or `no claim`.
        listed: String,
    },
}

/// What happened to the body (or the whole line) of one active claim that
/// is not a next action (a next action is always shown in full).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PackCut {
    /// Shown in full.
    Full,
    /// Level 2: its first [`PACK_BODY_KEPT`] characters, and how many were
    /// left out.
    Shortened,
    /// Level 3: left out; its length is said.
    Omitted,
    /// Level 4: a `verified` fact counted and numbered on the fold line.
    Folded,
    /// Level 5: not listed; counted and numbered on the `Not listed:` line.
    Unlisted,
}

/// Why a query text is refused (U3 G4, Owner O8). The text is never
/// repeated: each refusal is fixed words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum QueryRefusal {
    /// Empty, or longer than [`PACK_QUERY_MAX_CHARS`] characters.
    #[error("the query takes 1 through 256 characters (Unicode scalar values)")]
    Length,
    /// A control character (Unicode category Cc), as C2 refuses one.
    #[error("the query holds a control character (Unicode category Cc)")]
    Control,
    /// A string in the format of a memprove token, as C4 refuses one
    /// (U5 P2: [`crate::holds_token_shape`]).
    #[error(
        "the query holds a string in the format of a memprove token (43 characters of A-Z, \
         a-z, 0-9, '-', and '_', with at least one upper-case letter, one lower-case letter, and \
         one digit)"
    )]
    TokenFormat,
}

/// U3 G4 (Owner O8): whether `text` can be a query: 1 through
/// [`PACK_QUERY_MAX_CHARS`] characters, no control character (C2), and no
/// string in the format of a memprove token (C4's format; U5 P2: a
/// maximal run of exactly 43 characters of `A-Z`, `a-z`, `0-9`, `-`, and
/// `_` with an upper-case letter, a lower-case letter, and a digit,
/// [`crate::holds_token_shape`]).
///
/// # Errors
///
/// The first [`QueryRefusal`] that applies.
pub fn check_pack_query(text: &str) -> Result<(), QueryRefusal> {
    if !(1..=PACK_QUERY_MAX_CHARS).contains(&text.chars().count()) {
        return Err(QueryRefusal::Length);
    }
    if text.chars().any(char::is_control) {
        return Err(QueryRefusal::Control);
    }
    if crate::holds_token_shape(text) {
        return Err(QueryRefusal::TokenFormat);
    }
    Ok(())
}

/// A query and what it matched (U3 G4): the claim numbers, best first, as
/// the in-memory FTS5 index ranked them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackQuery {
    text: String,
    matches: Vec<u64>,
    sqlite_version: String,
}

impl PackQuery {
    /// A query `text` and its `matches`, best first, as ranked by an
    /// in-memory FTS5 index of the `SQLite` `sqlite_version` (U4 S2).
    ///
    /// # Errors
    ///
    /// The [`QueryRefusal`] of [`check_pack_query`].
    pub fn new(text: &str, matches: Vec<u64>, sqlite_version: &str) -> Result<Self, QueryRefusal> {
        check_pack_query(text)?;
        Ok(Self {
            text: text.to_owned(),
            matches,
            sqlite_version: sqlite_version.to_owned(),
        })
    }

    /// U4 S2 (Owner O8): the version of the `SQLite` whose FTS5 index
    /// ranked the matches.
    #[must_use]
    pub fn sqlite_version(&self) -> &str {
        &self.sqlite_version
    }

    /// The query text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The claim numbers it matched, best first.
    #[must_use]
    pub fn matches(&self) -> &[u64] {
        &self.matches
    }
}

/// Why a query Pack lists a claim (U3 G4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Selected {
    /// The query matched it.
    Match,
    /// It supersedes, in turn, a claim the query matched.
    Supersession,
    /// The active next action, listed when anything matched.
    NextAction,
}

impl Selected {
    /// The spelling the tests and the readers use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Supersession => "supersession_chain",
            Self::NextAction => "next_action",
        }
    }
}

fn escaped(value: &str) -> String {
    let mut text = String::with_capacity(value.len() + 2);
    for character in value.chars() {
        match character {
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '「' | '」' => {
                text.push('\\');
                text.push(character);
            }
            character if character.is_control() => {
                write!(text, "\\u{{{:02x}}}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character
                if PACK_ESCAPED_RANGES
                    .iter()
                    .any(|(first, last)| (*first..=*last).contains(&character)) =>
            {
                write!(text, "\\u{{{:04x}}}", u32::from(character))
                    .expect("writing to a String cannot fail");
            }
            character => text.push(character),
        }
    }
    text
}

/// A string an agent or a database row wrote, quoted in `「」` (E2).
fn quoted(value: &str) -> String {
    format!("「{}」", escaped(value))
}

fn units(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// A line's length with its line break.
fn line_units(line: Option<&str>) -> usize {
    line.map_or(0, |line| units(line) + 1)
}

/// U17: whether `text` says `verified` as a word of its own: not inside
/// `unverified`, not the reason or gap codes `no_verified_fact` and
/// `verified_fact`. This is the English reading of the rule that a
/// tampered Task's Pack, `resume --text`, Preview and tool results say
/// "verified" nowhere (ADR 0013, ADR 0014); the Chinese text had one
/// three-character verdict word for it. Quotes are not special here: a
/// caller that wants only the fixed text strips 「」 first.
#[must_use]
pub fn says_verified(text: &str) -> bool {
    let bytes = text.as_bytes();
    let word =
        |byte: Option<u8>| byte.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    let mut from = 0;
    while let Some(at) = text[from..].find("verified") {
        let start = from + at;
        let end = start + "verified".len();
        if !word(start.checked_sub(1).map(|index| bytes[index])) && !word(bytes.get(end).copied()) {
            return true;
        }
        from = end;
    }
    false
}

/// The word a Pack line gives a verdict (`verified`, `stale`,
/// `unconfirmed`, `tampered`); `resume --text` uses the same words.
#[must_use]
pub fn word(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Verified => "verified",
        Verdict::Stale => "stale",
        Verdict::Partial => "unconfirmed",
        Verdict::Tampered => "tampered",
    }
}

/// The label of a claim's kind as a query term of `search.v2` (U4 S2 rule
/// f): one word, so that a whitespace-separated query can hold it, in the
/// names the Capsule JSON gives the kinds: `fact`, `open_item`, `decision`,
/// `next_action`. (The claim lines write the kinds in words,
/// [`kind_word`]: `fact`, `open item`, `decision`, `next action`.)
#[must_use]
pub const fn pack_kind_label(kind: ClaimKind) -> &'static str {
    match kind {
        ClaimKind::Fact => "fact",
        ClaimKind::OpenItem => "open_item",
        ClaimKind::Decision => "decision",
        ClaimKind::NextAction => "next_action",
    }
}

const fn kind_word(kind: ClaimKind) -> &'static str {
    match kind {
        ClaimKind::Fact => "fact",
        ClaimKind::OpenItem => "open item",
        ClaimKind::Decision => "decision",
        ClaimKind::NextAction => "next action",
    }
}

fn codes(reasons: &[ReasonCode]) -> String {
    reasons
        .iter()
        .map(|reason| reason.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A commit as a claim line shows it: its first [`PACK_COMMIT_SHOWN`]
/// digits (the daemon writes it as hexadecimal ASCII).
fn short(commit: &str) -> &str {
    &commit[..commit.len().min(PACK_COMMIT_SHOWN)]
}

/// `#1–#3, #5`: ascending numbers, consecutive ones as a range.
fn ranges(numbers: &[u64]) -> String {
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

#[path = "pack_read.rs"]
mod read;

use read::{Entry, PackAction, PackClaim, PackEvidence, PackSession, SessionEnd};
pub use read::{PackCapsule, PackStatement};

#[path = "pack_lines.rs"]
mod lines;

pub use lines::{render_pack, render_pack_claims, render_pack_claims_within};

/// Adds the claims that supersede claim `number`, in turn, depth first, in
/// the order its `superseded_by` names them, unless listed already.
fn chain(
    capsule: &PackCapsule,
    number: u64,
    order: &mut Vec<u64>,
    why: &mut BTreeMap<u64, Selected>,
) {
    for by in &capsule.claims[&number].superseded_by {
        if capsule.claims.contains_key(by) && !why.contains_key(by) {
            why.insert(*by, Selected::Supersession);
            order.push(*by);
            chain(capsule, *by, order, why);
        }
    }
}

/// The claims of a query Pack in the order of their lines, and why each is
/// there (U3 G4): each match, best first, followed by the claims that
/// supersede it, in turn (depth first, in the order `superseded_by` names
/// them), unless listed already; then, when anything matched, every active
/// next action not listed yet.
fn query_order(
    capsule: &PackCapsule,
    query: &PackQuery,
) -> Result<(Vec<u64>, BTreeMap<u64, Selected>), PackError> {
    let mut seen = BTreeSet::new();
    for number in query.matches() {
        if !capsule.claims.contains_key(number) {
            return Err(PackError::BadMatches(format!(
                "#{number} is not a claim of the Task"
            )));
        }
        if !seen.insert(*number) {
            return Err(PackError::BadMatches(format!("#{number} matched twice")));
        }
    }
    let mut order = Vec::new();
    let mut why = BTreeMap::new();
    for number in query.matches() {
        if why.insert(*number, Selected::Match).is_none() {
            order.push(*number);
        }
        chain(capsule, *number, &mut order, &mut why);
    }
    if !order.is_empty() {
        for claim in capsule.active_in_order() {
            if claim.kind == ClaimKind::NextAction && !why.contains_key(&claim.number) {
                why.insert(claim.number, Selected::NextAction);
                order.push(claim.number);
            }
        }
    }
    Ok((order, why))
}

// ---------------------------------------------------------------------
// The selection.
// ---------------------------------------------------------------------

/// What the Pack keeps: the level reached, each active claim's cut, and
/// the base commits whose changed files are only counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSelection<'a> {
    capsule: &'a PackCapsule,
    rules: &'static PackRules,
    budget: usize,
    level: u8,
    cuts: BTreeMap<u64, PackCut>,
    counted: BTreeSet<String>,
    /// U3 G4: the claims that can have a line, in the order of the lines:
    /// the active claims in continuation order, or a query Pack's claims.
    order: Vec<u64>,
    /// U3 G4: the query of a query Pack, and why each of its claims is
    /// listed.
    query: Option<(&'a PackQuery, BTreeMap<u64, Selected>)>,
}

impl PackSelection<'_> {
    /// The level reached, 0 (nothing left out) to 5.
    #[must_use]
    pub const fn level(&self) -> u8 {
        self.level
    }

    /// The budget, in UTF-16 code units.
    #[must_use]
    pub const fn budget(&self) -> usize {
        self.budget
    }

    /// The rules that made the selection.
    #[must_use]
    pub const fn rules(&self) -> &'static PackRules {
        self.rules
    }

    /// What happened to active claim `number`; `None` for a next action
    /// (always in full) and for a claim that is not active. U3 G4: in a
    /// query Pack, for each claim it lists but an active next action, and
    /// `None` for one it does not list.
    #[must_use]
    pub fn cut(&self, number: u64) -> Option<PackCut> {
        self.cuts.get(&number).copied()
    }

    /// U3 G4: the query of a query Pack.
    #[must_use]
    pub fn query(&self) -> Option<&PackQuery> {
        self.query.as_ref().map(|(query, _)| *query)
    }

    /// U3 G4: why a query Pack lists claim `number`; `None` for one it
    /// does not list, and in a Pack made without a query.
    #[must_use]
    pub fn selected(&self, number: u64) -> Option<Selected> {
        self.query
            .as_ref()
            .and_then(|(_, why)| why.get(&number).copied())
    }

    /// The claims that can have a line, in the order of the lines (U3 G4).
    #[must_use]
    pub fn claim_order(&self) -> &[u64] {
        &self.order
    }

    /// The base commits of the claims `listed` keeps, in the order of
    /// their first line.
    pub(super) fn base_order(&self, listed: impl Fn(u64) -> bool) -> Vec<String> {
        self.capsule.base_order_in(&self.order, listed)
    }

    /// The base commits whose changed files level 1 only counts.
    #[must_use]
    pub const fn counted_bases(&self) -> &BTreeSet<String> {
        &self.counted
    }

    fn count(&self, cut: PackCut) -> usize {
        self.cuts.values().filter(|seen| **seen == cut).count()
    }

    /// Whether claim `number` has a line of its own.
    fn listed(&self, number: u64) -> bool {
        !matches!(self.cut(number), Some(PackCut::Folded | PackCut::Unlisted))
    }
}

/// The length of a selection's Pack, kept up to date as cuts change: every
/// line but a claim's, a base line, the fold, unlisted, and omission lines
/// is fixed.
struct Measure {
    fixed: usize,
    claims: BTreeMap<u64, usize>,
}

impl Measure {
    fn of(selection: &PackSelection<'_>) -> Self {
        let claims = selection
            .order
            .iter()
            .map(|number| {
                (
                    *number,
                    line_units(selection.claim_line(*number).as_deref()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut measure = Self { fixed: 0, claims };
        let all = units(&render_pack(selection));
        measure.fixed = all - measure.variable(selection);
        measure
    }

    fn variable(&self, selection: &PackSelection<'_>) -> usize {
        self.claims.values().sum::<usize>()
            + selection
                .base_lines()
                .iter()
                .map(|line| line_units(Some(line)))
                .sum::<usize>()
            + line_units(selection.fold_line().as_deref())
            + line_units(selection.unlisted_line().as_deref())
            + line_units(Some(&selection.omission_line()))
    }

    fn total(&self, selection: &PackSelection<'_>) -> usize {
        self.fixed + self.variable(selection)
    }

    fn set(&mut self, selection: &mut PackSelection<'_>, number: u64, cut: PackCut) {
        selection.cuts.insert(number, cut);
        self.claims
            .insert(number, line_units(selection.claim_line(number).as_deref()));
    }
}

/// One step of the omission, in order.
#[derive(Debug, Clone)]
enum Step {
    Count(String),
    Cut(u64, PackCut),
    Fold,
}

/// Every step of the omission, level by level, in order: the levels of
/// the selection's rules (for a whole Pack, E1 as revised in P4-C2
/// revision 4).
///
/// U3 G4: the steps take only the claims of `selection` (its
/// `claim_order`): for a whole Pack the active claims, as before.
fn steps(selection: &PackSelection<'_>) -> Vec<(u8, Step)> {
    let capsule = selection.capsule;
    let rules = selection.rules;
    let members = selection.order.iter().copied().collect::<BTreeSet<_>>();
    let mut steps = Vec::new();
    for (step, level) in rules.levels.iter().zip(1_u8..) {
        match *step {
            OmissionStep::CountChangedFiles => {
                for base in selection.base_order(|_| true) {
                    if capsule.listable(&base) {
                        steps.push((level, Step::Count(base)));
                    }
                }
            }
            OmissionStep::ShortenBodies(groups) => {
                for number in capsule.group_order(groups, &members) {
                    if capsule.claims[&number].body_chars() > rules.body_kept {
                        steps.push((level, Step::Cut(number, PackCut::Shortened)));
                    }
                }
            }
            OmissionStep::OmitBodies(groups) => {
                for number in capsule.group_order(groups, &members) {
                    steps.push((level, Step::Cut(number, PackCut::Omitted)));
                }
            }
            OmissionStep::FoldVerifiedFacts => {
                if !capsule
                    .group_order(&[ClaimGroup::VerifiedFacts], &members)
                    .is_empty()
                {
                    steps.push((level, Step::Fold));
                }
            }
            OmissionStep::Unlist(groups) => {
                for number in capsule.group_order(groups, &members) {
                    steps.push((level, Step::Cut(number, PackCut::Unlisted)));
                }
            }
        }
    }
    steps
}

/// Chooses what the Pack of `capsule` keeps within `budget` UTF-16 code
/// units (ADR 0014 E1, as revised in P4-C2). The steps run in order and
/// stop as soon as the Pack fits:
///
/// 1. the changed files of each base commit, one base line at a time, are
///    only counted;
/// 2. bodies longer than [`PACK_BODY_KEPT`] characters are shortened to
///    their first [`PACK_BODY_KEPT`], one claim at a time, in the order of
///    the active `verified` facts, the other active facts, the Decisions,
///    the open items (lower numbers first);
/// 3. whole bodies are omitted, in the same order;
/// 4. the active `verified` facts are folded into one line;
/// 5. claims are no longer listed, one at a time: the facts, then the
///    Decisions, then the open items, lower numbers first; the next action
///    always is.
///
/// # Errors
///
/// [`PackError::OverBudget`], with the least budget at which the level-5
/// Pack fits (U1 C1), when even level 5 does not fit.
pub fn select_pack(capsule: &PackCapsule, budget: usize) -> Result<PackSelection<'_>, PackError> {
    select_pack_with(capsule, &RESUME_RULES, budget)
}

/// [`select_pack`] by `rules`: the kinds they never omit and their
/// omission levels, in order. With [`RESUME_RULES`] it is [`select_pack`].
///
/// # Errors
///
/// [`PackError::OverBudget`] as [`select_pack`].
pub fn select_pack_with<'a>(
    capsule: &'a PackCapsule,
    rules: &'static PackRules,
    budget: usize,
) -> Result<PackSelection<'a>, PackError> {
    let order = capsule
        .active_in_order()
        .map(|claim| claim.number)
        .collect();
    select(capsule, rules, budget, order, None)
}

/// U3 G4 (Owner O8, O9): the query Pack of `capsule` for `query` within
/// `budget` UTF-16 code units, by [`SEARCH_RULES`]: the claims the query
/// matched, best first (as `query` ranks them), each followed by the
/// claims that supersede it, in turn, then (when anything matched) the
/// active next action; a `Query:` line after the `Ontology:` line; cut by the
/// levels of [`select_pack`], the superseded and withdrawn claims first.
/// With no match there is no claim line, and the Pack is still whole.
///
/// # Errors
///
/// [`PackError::BadMatches`] when the matches name a claim the Capsule
/// does not list, or one twice; [`PackError::OverBudget`] as
/// [`select_pack`].
pub fn select_query_pack<'a>(
    capsule: &'a PackCapsule,
    query: &'a PackQuery,
    budget: usize,
) -> Result<PackSelection<'a>, PackError> {
    let (order, why) = query_order(capsule, query)?;
    select(capsule, &SEARCH_RULES, budget, order, Some((query, why)))
}

fn select<'a>(
    capsule: &'a PackCapsule,
    rules: &'static PackRules,
    budget: usize,
    order: Vec<u64>,
    query: Option<(&'a PackQuery, BTreeMap<u64, Selected>)>,
) -> Result<PackSelection<'a>, PackError> {
    let cuts = order
        .iter()
        .map(|number| &capsule.claims[number])
        .filter(|claim| {
            !(claim.standing == Standing::Active && rules.never_omitted.contains(&claim.kind))
        })
        .map(|claim| (claim.number, PackCut::Full))
        .collect();
    let mut selection = PackSelection {
        capsule,
        rules,
        budget,
        level: 0,
        cuts,
        counted: BTreeSet::new(),
        order,
        query,
    };
    let mut measure = Measure::of(&selection);
    if measure.total(&selection) <= budget {
        return Ok(selection);
    }
    let members = selection.order.iter().copied().collect::<BTreeSet<_>>();
    for (level, step) in steps(&selection) {
        selection.level = level;
        match step {
            Step::Count(base) => {
                selection.counted.insert(base);
            }
            Step::Cut(number, cut) => measure.set(&mut selection, number, cut),
            Step::Fold => {
                for number in capsule.group_order(&[ClaimGroup::VerifiedFacts], &members) {
                    measure.set(&mut selection, number, PackCut::Folded);
                }
            }
        }
        if measure.total(&selection) <= budget {
            return Ok(checked(selection, &measure));
        }
    }
    let mut needed = measure.total(&selection);
    debug_assert_eq!(units(&render_pack(&selection)), needed);
    // U1 (C1): the first line names the budget, so the level-5 Pack is
    // one code unit longer for each digit of it. `needed` is the length
    // at the digits of `budget`; at a budget of `needed` the Pack is
    // measured again, until it fits. The result is the least budget the
    // Pack fits at, so retrying with it always succeeds.
    loop {
        selection.budget = needed;
        let at_needed = units(&render_pack(&selection));
        if at_needed <= needed {
            break;
        }
        needed = at_needed;
    }
    Err(PackError::OverBudget { budget, needed })
}

fn checked<'a>(selection: PackSelection<'a>, measure: &Measure) -> PackSelection<'a> {
    debug_assert_eq!(
        units(&render_pack(&selection)),
        measure.total(&selection),
        "the measured length is the rendered length"
    );
    selection
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
