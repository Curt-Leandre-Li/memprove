//! A strict reader of the Context Pack for its tests (P4-C, as revised in
//! P4-C2), written from ADR 0014's line formats apart from the renderer: it
//! shares no code with it, and checks every Pack against the Capsule JSON.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::{ONTOLOGY_VERSION, Sha256Digest};

/// The closed set of line prefixes; `#` stands for `#<number> `. U3 G4
/// (Owner O9): `Query:`, a query Pack's third line, makes 19. U5 P1 (Owner
/// O3): `Last passed:`, right after the Session lines, makes 20. U17 (M1
/// O3): the words are English; this reader has its own copy of them.
pub(super) const PREFIXES: [&str; 20] = [
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

const OMISSION_TAIL: &str =
    "; full text of given numbers: memprove resume --pack --claims <numbers> (such as 3,5-7)";

/// U17: whether the shape of a line says `verified` as a word of its own
/// (not `unverified`, not the code `no_verified_fact`), outside every
/// quote (the shape has them as [`QUOTE`]).
pub(super) fn says_verified(shape: &str) -> bool {
    crate::pack::says_verified(shape)
}

/// Whether ADR 0014 E2 (as this test reads it) has a character shown as an
/// escape inside 「」: all of Cc, all of Cf (Unicode 16.0.0), U+2028,
/// U+2029, the tag block, the look-alikes and blanks of P3-L O4, and the
/// characters of T16 N3.
pub(super) fn must_escape(character: char) -> bool {
    character.is_control()
        || matches!(
            u32::from(character),
            0x00ad
                | 0x0600..=0x0605
                | 0x061c
                | 0x06dd
                | 0x070f
                | 0x0890..=0x0891
                | 0x08e2
                | 0x180e
                | 0x200b..=0x200f
                | 0x202a..=0x202e
                | 0x2060..=0x2064
                | 0x2066..=0x206f
                | 0xfeff
                | 0xfff9..=0xfffb
                | 0x110bd
                | 0x110cd
                | 0x13430..=0x1343f
                | 0x1bca0..=0x1bca3
                | 0x1d173..=0x1d17a
                | 0xe0000..=0xe007f
                | 0x2028..=0x2029
                | 0x300f
                | 0xfe42
                | 0xfe44
                | 0xff63
                | 0x115f..=0x1160
                | 0x3164
                | 0xffa0
                | 0xfe00..=0xfe0f
                | 0xe0100..=0xe01ef
                | 0x180b..=0x180f
                | 0x17b4..=0x17b5
                | 0x2065
                | 0xfff0..=0xfff8
                | 0xe0080..=0xe0fff
                | 0x034f
                | 0x2518
                | 0x231f
                | 0x2e25
                | 0x251b
                | 0x2e23
                | 0x3000
                | 0x2800
        )
}

/// Stands for a quote in the shape of a line.
pub(super) const QUOTE: char = '◊';

/// A line as its template text, with [`QUOTE`] for each quote, and the
/// decoded quotes.
pub(super) fn shape(line: &str) -> Result<(String, Vec<String>), String> {
    let mut shape = String::new();
    let mut quotes = Vec::new();
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        match character {
            '「' => {
                quotes.push(quote(&mut chars)?);
                shape.push(QUOTE);
            }
            QUOTE | '」' | '\\' => return Err(format!("{character:?} outside a quote: {line}")),
            character if must_escape(character) => {
                return Err(format!("raw {character:?} outside a quote: {line}"));
            }
            character => shape.push(character),
        }
    }
    Ok((shape, quotes))
}

fn quote(chars: &mut std::str::Chars<'_>) -> Result<String, String> {
    let mut value = String::new();
    loop {
        match chars.next().ok_or("a quote runs past its line")? {
            '」' => return Ok(value),
            '「' => return Err("an unescaped 「 inside a quote".to_owned()),
            '\\' => value.push(escape(chars)?),
            character if must_escape(character) => {
                return Err(format!("raw {character:?} inside a quote"));
            }
            character => value.push(character),
        }
    }
}

fn escape(chars: &mut std::str::Chars<'_>) -> Result<char, String> {
    let character = match chars.next().ok_or("a backslash ends the line")? {
        '\\' => '\\',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        '「' => '「',
        '」' => '」',
        'u' => {
            if chars.next() != Some('{') {
                return Err("\\u without {".to_owned());
            }
            let mut hex = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(digit) if digit.is_ascii_digit() || ('a'..='f').contains(&digit) => {
                        hex.push(digit);
                    }
                    other => return Err(format!("bad \\u digit {other:?}")),
                }
            }
            if !(2..=6).contains(&hex.len()) {
                return Err(format!("bad \\u length {hex}"));
            }
            let code = u32::from_str_radix(&hex, 16).map_err(|error| error.to_string())?;
            let character = char::from_u32(code).ok_or("not a character")?;
            if !must_escape(character) || matches!(character, '\n' | '\r' | '\t') {
                return Err(format!("needless escape of {character:?}"));
            }
            character
        }
        other => return Err(format!("unknown escape \\{other}")),
    };
    Ok(character)
}

/// A little cursor over the shape of a line.
struct Reader<'a>(&'a str);

impl<'a> Reader<'a> {
    fn eat(&mut self, prefix: &str) -> Result<(), String> {
        self.0 = self
            .0
            .strip_prefix(prefix)
            .ok_or_else(|| format!("expected {prefix:?} at {:?}", self.0))?;
        Ok(())
    }

    fn try_eat(&mut self, prefix: &str) -> bool {
        self.eat(prefix).is_ok()
    }

    fn number(&mut self) -> Result<u64, String> {
        let end = self
            .0
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(self.0.len());
        let (digits, rest) = self.0.split_at(end);
        if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
            return Err(format!("expected a number at {:?}", self.0));
        }
        self.0 = rest;
        digits.parse().map_err(|_| "number too large".to_owned())
    }

    fn hex(&mut self) -> Result<String, String> {
        let end = self
            .0
            .find(|character: char| !matches!(character, '0'..='9' | 'a'..='f'))
            .unwrap_or(self.0.len());
        let (digits, rest) = self.0.split_at(end);
        if digits.is_empty() {
            return Err(format!("expected a commit at {:?}", self.0));
        }
        self.0 = rest;
        Ok(digits.to_owned())
    }

    fn until(&mut self, stop: &str) -> Result<&'a str, String> {
        let (before, after) = self
            .0
            .split_once(stop)
            .ok_or_else(|| format!("expected {stop:?} in {:?}", self.0))?;
        self.0 = after;
        Ok(before)
    }

    fn end(&self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(format!("trailing {:?}", self.0))
        }
    }
}

/// `#1–#3, #5` read back.
fn ranges(text: &str) -> Result<Vec<u64>, String> {
    let mut numbers = Vec::new();
    for part in text.split(", ") {
        let mut reader = Reader(part);
        reader.eat("#")?;
        let first = reader.number()?;
        let last = if reader.try_eat("–#") {
            reader.number()?
        } else {
            first
        };
        reader.end()?;
        if last < first {
            return Err(format!("bad range {part}"));
        }
        numbers.extend(first..=last);
    }
    Ok(numbers)
}

/// A claim body as the Pack shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Body {
    Full(String, Option<String>),
    Shortened(String, Option<String>, u64, u64),
    Omitted(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedClaim {
    pub(super) number: u64,
    pub(super) kind: &'static str,
    /// `active`, `superseded`, or `withdrawn`.
    pub(super) standing: &'static str,
    /// A fact that stands: its verdict and reasons.
    pub(super) verdict: Option<String>,
    pub(super) reasons: Vec<String>,
    pub(super) body: Body,
    /// The commits the line names, by their first digits.
    pub(super) commits: Vec<String>,
    /// Every other claim: its Evidence in words, as a whole.
    pub(super) cited: Option<String>,
    pub(super) superseded_by: Vec<u64>,
    pub(super) withdrawal_reason: Option<String>,
    /// U10 V6: the agent name after `from Session k`, if the line has one.
    pub(super) author: Option<String>,
}

/// One `Session ` line read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedSession {
    pub(super) number: u64,
    pub(super) session_id: String,
    /// `open`, `ended`, or `crashed`: the Capsule's `end_state` the words
    /// say.
    pub(super) end_state: &'static str,
    pub(super) started_at: String,
    /// An open Session's last write; `None` when the line says the ledger
    /// holds none (and for a Session that ended).
    pub(super) last_recorded_at: Option<String>,
    /// The `continues from…` part (ADR 0014 E6, P4-H1): the ledger sequence and the
    /// first digits of the declared `pack_digest`.
    pub(super) resumed: Option<(u64, String)>,
    /// U7 P1 (E1 revision 9): whether the line ends with the takeover hint
    /// of a Session that has not ended and is not the reader's own.
    pub(super) takeover: bool,
    /// U10 T7: the program the hint names, when the line has the hint.
    pub(super) program: Option<String>,
    /// U8 A4 (E1 revision 10): the `; agent「…」(…)` part: the name as
    /// written, the role, and the parent as written (`Session <j>` or
    /// `unlisted Session <id>`).
    pub(super) agent: Option<(String, String, Option<String>)>,
    /// U9 A7 (E1 revision 12): the `; ended by …` part, as written:
    /// `Session <j>`, `unlisted Session <id>`, or `agent <name> <role>`.
    pub(super) ended_by: Option<String>,
}

/// U7 P1 (E1 revision 9, Owner O3): the start of the end of the line of a
/// Session that has not ended and is not the reader's own. U10 T7 (Owner
/// O4): the rest is `<program> session end <the line's Session ID>
/// --abandoned --reason <why> (MCP: end {session:<the line's number>,
/// abandoned:true})`, as ADR 0014 "Amendment (U10, S-T)" writes it.
const TAKEOVER: &str = "; end it before taking over: ";

/// U10 T7: a program the hint may name, as this reader reads ADR 0014: the
/// word `memprove`, or an absolute path of ASCII letters, digits, `/`,
/// `.`, `_`, `-`, `+`, at most 1024 bytes.
fn takeover_program(program: &str) -> bool {
    program == "memprove"
        || (program.len() > 1
            && program.len() <= 1024
            && program.starts_with('/')
            && program
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+')))
}

/// The files of a base line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Files {
    Listed(Vec<String>, Option<u64>),
    Counted(u64),
    Word(String),
}

#[derive(Debug, Default, Clone)]
pub(super) struct Parsed {
    pub(super) budget: u64,
    pub(super) sequence: u64,
    pub(super) capsule_digest: String,
    pub(super) title: String,
    pub(super) task_word: String,
    pub(super) task_reasons: Vec<String>,
    pub(super) counts: Vec<u64>,
    pub(super) breakdown: BTreeMap<String, u64>,
    pub(super) integrity: Vec<String>,
    /// The `Sessions:` counts: all, ended normally, ended abandoned,
    /// open, and (P4-H1) declared a continuation source.
    pub(super) sessions: Vec<u64>,
    /// Each `Session ` line, in order.
    pub(super) session_lines: Vec<ParsedSession>,
    /// Each action line, `Can` or `Cannot` and the rest of its text (E3).
    pub(super) actions: Vec<(bool, String)>,
    pub(super) bases: Vec<(String, Files)>,
    pub(super) claims: Vec<ParsedClaim>,
    pub(super) fold: Vec<u64>,
    pub(super) unlisted_facts: BTreeMap<String, Vec<u64>>,
    pub(super) unlisted_other: BTreeMap<&'static str, Vec<u64>>,
    pub(super) superseded: Vec<u64>,
    pub(super) withdrawn: Vec<u64>,
    pub(super) reruns: Vec<String>,
    pub(super) gaps: Vec<String>,
    pub(super) omission: Vec<u64>,
    /// Each line whose template text says "verified" as a word, and what it is.
    pub(super) verified: Vec<(&'static str, u64)>,
    /// How many characters of each active claim's body are shown (set by
    /// [`check`]).
    pub(super) shown: BTreeMap<u64, u64>,
    /// U3 G4: the `Query:` line: the query, the count of matches, and their
    /// numbers, best first.
    pub(super) query: Option<(String, u64, Vec<u64>)>,
    /// U3 G4: the superseded and withdrawn claims a query Pack's level 5
    /// no longer lists.
    pub(super) unlisted_retired: Vec<u64>,
    /// U5 P1: the `Last passed:` line: the argv and the commit's 12 digits;
    /// U8 K5 (E1 revision 11): and the `memprove run` arguments that rerun
    /// it as a test run.
    pub(super) last_passed: Option<(String, String, String)>,
}

/// The index in the 18 prefixes before U3 G4 and U5 P1 of the prefix at
/// `prefix` of [`PREFIXES`]; `None` for `Query:` and `Last passed:`.
fn before_query(prefix: usize) -> Option<usize> {
    match prefix {
        0 | 1 => Some(prefix),
        2 | 7 => None,
        3..=6 => Some(prefix - 1),
        _ => Some(prefix - 2),
    }
}

/// The rank of each prefix in the order of the lines: the action lines
/// (`Can:`, `Cannot:`, ADR 0014 E3) come after the Session lines.
/// `prefix` is the index among the 18 prefixes before U3 G4.
fn rank(prefix: usize) -> u8 {
    match prefix {
        0..=5 => u8::try_from(prefix).unwrap(),
        12 | 13 => 6,
        6 => 7,
        7..=10 => 8,
        11 => 9,
        14 => 10,
        15 => 11,
        16 => 12,
        _ => 13,
    }
}

/// Rank of a base line, where `--claims` output starts.
const BASE_RANK: u8 = 7;

/// Reads a whole Pack strictly; every line must be in place.
pub(super) fn parse(text: &str) -> Result<Parsed, String> {
    let body = text.strip_suffix('\n').ok_or("no final line break")?;
    let (before_last, last) = body.rsplit_once('\n').ok_or("one line")?;
    let digest = last
        .strip_prefix("Digest: sha256:")
        .ok_or("no digest line")?;
    let before = &text[..=before_last.len()];
    if digest != Sha256Digest::of_bytes(before.as_bytes()).as_str() {
        return Err("pack_digest does not match".to_owned());
    }
    let parsed = parse_lines(before, false)?;
    if parsed.omission.is_empty() {
        return Err("no omission line".to_owned());
    }
    Ok(parsed)
}

/// Reads lines strictly: a whole Pack but its digest line, or (`fragment`)
/// the output of `--claims`, base and claim lines only.
pub(super) fn parse_lines(text: &str, fragment: bool) -> Result<Parsed, String> {
    let body = text.strip_suffix('\n').ok_or("no final line break")?;
    let mut parsed = Parsed::default();
    let mut last_rank = if fragment { BASE_RANK } else { 0 };
    for (index, line) in body.split('\n').enumerate() {
        let position = PREFIXES
            .iter()
            .position(|prefix| line.starts_with(prefix))
            .ok_or_else(|| format!("line {index} has no prefix: {line}"))?;
        let (shape, quotes) = shape(line)?;
        // U3 G4 (Owner O9): `Query:` only as the third line of a whole Pack.
        // U5 P1 (Owner O3): `Last passed:` once, right after the `Sessions:`
        // line and the `Session ` lines (ranks 4 and 5), before everything
        // else; a `Session ` line after it is out of place.
        let Some(prefix) = before_query(position) else {
            if position == 7 {
                if fragment || parsed.last_passed.is_some() || !matches!(last_rank, 4 | 5) {
                    return Err(format!("line {index} is out of place: {line}"));
                }
                parsed.last_passed = Some(
                    last_passed_line(&shape, &quotes)
                        .map_err(|error| format!("{error} in line {index}: {line}"))?,
                );
                last_rank = 6;
                continue;
            }
            if fragment || index != 2 {
                return Err(format!("line {index} is out of place: {line}"));
            }
            parsed.query = Some(query_line(&shape, &quotes)?);
            continue;
        };
        let line_rank = rank(prefix);
        // The lines after a `Query:` line are where they would be without it.
        let index = index - usize::from(parsed.query.is_some());
        let in_place = if fragment {
            matches!(prefix, 6 | 7) && line_rank >= last_rank
        } else if index < 3 {
            usize::from(line_rank) == index
        } else {
            line_rank >= last_rank && line_rank > 2 && line_rank < 13
        };
        if !in_place {
            return Err(format!("line {index} is out of place: {line}"));
        }
        last_rank = line_rank;
        if says_verified(&shape) {
            parsed.verified.push(match prefix {
                2 => ("task", 0),
                7 => ("claim", claim_number(&shape)?),
                10 => ("fold", 0),
                _ => ("other", 0),
            });
        }
        parse_line(prefix, &shape, &quotes, &mut parsed)
            .map_err(|error| format!("{error} in line {index}: {line}"))?;
    }
    Ok(parsed)
}

/// `Query: 「<query>」; <n> matches[, by rank: #a, #b, …]` (U3 G4, Owner O9).
fn query_line(shape: &str, quotes: &[String]) -> Result<(String, u64, Vec<u64>), String> {
    let mut reader = Reader(shape);
    reader.eat("Query: ◊; ")?;
    let [query] = quotes else {
        return Err("a query line without one quote".to_owned());
    };
    let count = reader.number()?;
    reader.eat(" matches")?;
    let mut numbers = Vec::new();
    if reader.try_eat(", by rank: ") {
        for part in reader.0.split(", ") {
            let mut item = Reader(part);
            item.eat("#")?;
            numbers.push(item.number()?);
            item.end()?;
        }
        reader.0 = "";
    }
    reader.end()?;
    if u64::try_from(numbers.len()).unwrap() != count {
        return Err("the query's match count".to_owned());
    }
    Ok((query.clone(), count, numbers))
}

/// The three spellings of the arguments that rerun a passed run as a test
/// run (U8 K5, E1 revision 11): the only text a `Last passed:` line may hold
/// after its commit, as this reader has them (not taken from the code that
/// writes the line).
const RERUN_ARGUMENTS: [&str; 3] = [
    "--test --test-format cargo",
    "--test --test-format pytest",
    "--test --test-cases <N>",
];

/// `Last passed: 「<argv>」 (<12 hexadecimal digits>; <rerun arguments>)` (U5
/// P1, Owner O3; U8 K5, E1 revision 11).
fn last_passed_line(shape: &str, quotes: &[String]) -> Result<(String, String, String), String> {
    let mut reader = Reader(shape);
    reader.eat("Last passed: ◊ (")?;
    let [argv] = quotes else {
        return Err("a last-passed line without one quote".to_owned());
    };
    let commit = reader.hex()?;
    if commit.len() != 12 {
        return Err("a last-passed commit of other than 12 digits".to_owned());
    }
    reader.eat("; ")?;
    let rerun = reader.until(")")?;
    if !RERUN_ARGUMENTS.contains(&rerun) {
        return Err(format!("last-passed rerun arguments {rerun:?}"));
    }
    reader.end()?;
    Ok((argv.clone(), commit, rerun.to_owned()))
}

/// U8 K5 (E1 revision 11), as this test reads it: the arguments that rerun
/// `argv` (the Capsule's, an array of strings) as a test run. The first
/// word that names a runner decides: `cargo` followed by `test` (words that
/// start with `+`, a toolchain, between them are skipped) is cargo's;
/// `pytest` or `py.test` (by the last component of a path), or `-m` followed
/// by `pytest`, is pytest's; no runner named, the count is the caller's.
fn expected_rerun(argv: &Value) -> &'static str {
    let words: Vec<&str> = argv
        .as_array()
        .unwrap()
        .iter()
        .map(|word| word.as_str().unwrap())
        .collect();
    let base = |word: &str| word.rsplit('/').next().unwrap().to_owned();
    for (at, word) in words.iter().enumerate() {
        let next = words[at + 1..].iter().find(|next| !next.starts_with('+'));
        if base(word) == "cargo" && next == Some(&"test") {
            return "--test --test-format cargo";
        }
        if matches!(base(word).as_str(), "pytest" | "py.test")
            || (*word == "-m" && words.get(at + 1) == Some(&"pytest"))
        {
            return "--test --test-format pytest";
        }
    }
    "--test --test-cases <N>"
}

/// U9 K2 (Owner O6), as this test reads it: the rerun arguments of a run
/// whose Capsule item may carry the `test_format` it was recorded with. The
/// record decides when there is one; without it the argv does, as
/// [`expected_rerun`] reads it.
fn expected_rerun_of(item: &Value) -> &'static str {
    match item.get("test_format").and_then(Value::as_str) {
        Some("cargo") => "--test --test-format cargo",
        Some("pytest") => "--test --test-format pytest",
        Some("cases") => "--test --test-cases <N>",
        Some(other) => panic!("unknown test_format {other:?}"),
        None => expected_rerun(&item["argv"]),
    }
}

/// U5 P1 (Owner O3), as this test reads it from the Capsule JSON: the argv
/// (joined by spaces), the commit's first 12 digits, and (U8 K5) the rerun
/// arguments of the passed test
/// run the `Last passed:` line names, or `None` when there is no such line: a
/// `tampered` Capsule, one with a `rerun` item (class 5), or one that shows
/// no passed test run it can place. The run is the latest run of the Task
/// (`cite_latest_run`) when that is a passed test run whose commit the
/// Capsule gives (a claim's Evidence of that ID, else the probed HEAD when
/// `complete_task` cites it); else the last run `complete_task` cites that
/// a claim's Evidence shows as a passed test run.
pub(super) fn expected_last_passed(capsule: &Value) -> Option<(String, String, String)> {
    if capsule["status"] == "tampered"
        || capsule["continuation"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["class"] == 5 && item["template"] == "rerun")
    {
        return None;
    }
    let mut claims = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .collect::<Vec<_>>();
    claims.sort_by_key(|claim| claim["number"].as_u64().unwrap());
    let joined = |argv: &Value| {
        argv.as_array().map(|argv| {
            argv.iter()
                .map(|argument| argument.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .map(|argv| argv.join(" "))
        })?
    };
    let passed_test = |item: &Value| item["kind"] == "test_result" && item["run_status"] == "PASS";
    // The first Evidence object of each ID, in claim-number order.
    let mut first: Vec<(&str, &Value)> = Vec::new();
    for claim in claims {
        for item in claim["evidence"].as_array().unwrap() {
            if let Some(id) = item["evidence_id"].as_str()
                && !first.iter().any(|(seen, _)| *seen == id)
            {
                first.push((id, item));
            }
        }
    }
    let cited = |id: &str| {
        let (_, item) = first.iter().find(|(seen, _)| *seen == id)?;
        if !passed_test(item) {
            return None;
        }
        Some((
            joined(&item["argv"])?,
            item["base_commit"].as_str()?.to_owned(),
            expected_rerun_of(item),
        ))
    };
    let args = |name: &str| {
        capsule["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["action"] == name)
            .map_or(Value::Null, |action| action["args"].clone())
    };
    let completion = strings(&args("complete_task")["completion_evidence_ids"]);
    let latest = args("cite_latest_run");
    let probe = &capsule["bindings"]["probe"];
    let head = (probe["available"] == true)
        .then(|| probe["head_oid"].as_str().map(str::to_owned))
        .flatten();
    let twelve = |commit: String| commit.chars().take(12).collect::<String>();
    if let (Some(id), Some(argv)) = (latest["evidence_id"].as_str(), joined(&latest["argv"]))
        && passed_test(&latest)
    {
        let commit = cited(id).map(|(_, base, _)| base).or_else(|| {
            head.clone()
                .filter(|_| completion.iter().any(|seen| seen == id))
        });
        if let Some(commit) = commit {
            return Some((argv, twelve(commit), expected_rerun_of(&latest).to_owned()));
        }
    }
    completion
        .iter()
        .rev()
        .find_map(|id| cited(id))
        .map(|(argv, base, rerun)| (argv, twelve(base), rerun.to_owned()))
}

fn claim_number(shape: &str) -> Result<u64, String> {
    let mut reader = Reader(shape);
    reader.eat("#")?;
    reader.number()
}

fn parse_line(
    prefix: usize,
    shape: &str,
    quotes: &[String],
    parsed: &mut Parsed,
) -> Result<(), String> {
    let mut reader = Reader(shape);
    match prefix {
        0 => header_line(&mut reader, parsed)?,
        1 => {
            reader.eat("Ontology: ")?;
            reader.eat(ONTOLOGY_VERSION)?;
            reader.eat(" (the version of the verdict rules)")?;
        }
        2 => task_line(&mut reader, quotes, parsed)?,
        3 => {
            parsed.integrity.push(shape.to_owned());
            return Ok(());
        }
        4 => sessions_line(&mut reader, parsed)?,
        5 => {
            // U8 A4, U9 A7: a Session line's quotes are its agent's name and
            // an ender's by agent; `session_line` checks how many it has
            // (U10 SF-1: two, when it has both).
            parsed
                .session_lines
                .push(session_line(&mut reader, quotes)?);
        }
        12 | 13 => {
            if !quotes.is_empty() {
                return Err("agent text on an action line".to_owned());
            }
            let available = prefix == 12;
            reader.eat(if available { "Can: " } else { "Cannot: " })?;
            parsed.actions.push((available, reader.0.to_owned()));
            return Ok(());
        }
        6 => base_line(&mut reader, quotes, parsed)?,
        7 => {
            parsed.claims.push(claim_line(shape, quotes)?);
            return Ok(());
        }
        8 | 9 => {
            reader.eat(if prefix == 8 {
                "Rerun: "
            } else {
                "No rerun needed: "
            })?;
            reader.eat("◊ ")?;
            if prefix == 8 {
                reader.eat("on the current HEAD (for ")?;
                ranges(reader.until(")")?)?;
            }
            parsed
                .reruns
                .push(quotes.first().cloned().ok_or("no argv")?);
            return Ok(());
        }
        10 => {
            reader.eat("Folded: verified active facts ")?;
            let count = reader.number()?;
            reader.eat(": ")?;
            parsed.fold =
                ranges(reader.until(" (bodies and evidence bindings not listed one by one)")?)?;
            if u64::try_from(parsed.fold.len()).unwrap() != count {
                return Err("fold count".to_owned());
            }
        }
        11 => unlisted_line(&mut reader, parsed)?,
        14 => folded_line(&mut reader, parsed)?,
        15 => {
            reader.eat("Gaps: ")?;
            parsed.gaps = reader.0.split(", ").map(str::to_owned).collect();
            return Ok(());
        }
        _ => omission_line(&mut reader, parsed)?,
    }
    reader.end()
}

fn header_line(reader: &mut Reader<'_>, parsed: &mut Parsed) -> Result<(), String> {
    reader.eat("Pack: memprove-pack v1; capsule_digest ")?;
    parsed.capsule_digest = reader.until("; ledger sequence ")?.to_owned();
    parsed.sequence = reader.number()?;
    if !reader.try_eat("; HEAD not probed; ") {
        reader.eat("; probed HEAD ")?;
        reader.hex()?;
        reader.eat("; ")?;
    }
    reader.eat("budget ")?;
    parsed.budget = reader.number()?;
    reader.eat(" UTF-16 code units")
}

fn sessions_line(reader: &mut Reader<'_>, parsed: &mut Parsed) -> Result<(), String> {
    reader.eat("Sessions: ")?;
    let all = reader.number()?;
    reader.eat(" in all, ")?;
    let ended = reader.number()?;
    reader.eat(" ended normally, ")?;
    let crashed = reader.number()?;
    reader.eat(" ended abandoned, ")?;
    let open = reader.number()?;
    reader.eat(" not ended, ")?;
    let declared = reader.number()?;
    reader.eat(" declared a resumed_from Pack")?;
    parsed.sessions = vec![all, ended, crashed, open, declared];
    Ok(())
}

/// A plain timestamp the daemon wrote, up to `stop`.
fn timestamp<'a>(reader: &mut Reader<'a>, stop: &str) -> Result<&'a str, String> {
    let at = reader.until(stop)?;
    if at.is_empty() || !at.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(format!("a time that is not plain: {at:?}"));
    }
    Ok(at)
}

/// `Session <k> <id> <state> (started <t>[, <last write>])[; agent…]
/// [; continues from…]` (P4-C2, E6, P4-H1; U8 A4), with the takeover hint
/// (U7 P1, only after `not ended`) at the end, `…[; continues from…]; end
/// it before taking over: …`; U10 T1 (Owner O3): a line with the hint has
/// the agent part too, where A4 puts it (S-F round 2's rule, no agent
/// there, is gone); U9 A7: after `ended abandoned`, at the end, `[; ended
/// by …]`.
fn session_line(reader: &mut Reader<'_>, quotes: &[String]) -> Result<ParsedSession, String> {
    reader.eat("Session ")?;
    let number = reader.number()?;
    reader.eat(" ")?;
    let session_id = reader.until(" ")?.to_owned();
    if session_id.is_empty() || !session_id.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("a Session ID that is not plain".to_owned());
    }
    let (end_state, started_at, last_recorded_at) = if reader.try_eat("not ended (started ") {
        let started = timestamp(reader, ", ")?.to_owned();
        let last = if reader.try_eat("nothing of it in the ledger yet)") {
            None
        } else {
            reader.eat("last wrote to the ledger at ")?;
            Some(timestamp(reader, ")")?.to_owned())
        };
        ("open", started, last)
    } else if reader.try_eat("ended abandoned (started ") {
        ("crashed", timestamp(reader, ")")?.to_owned(), None)
    } else {
        reader.eat("ended normally (started ")?;
        ("ended", timestamp(reader, ")")?.to_owned(), None)
    };
    let agent = agent_part(reader, quotes)?;
    let resumed = if reader.try_eat("; continues from the Pack at ledger sequence ") {
        let sequence = reader.number()?;
        reader.eat(" (self-reported, digest sha256:")?;
        let digest = reader.hex()?;
        if digest.len() != 12 {
            return Err(format!("a declared digest of {} digits", digest.len()));
        }
        reader.eat(")")?;
        Some((sequence, digest))
    } else {
        None
    };
    let program = if end_state == "open" && reader.try_eat(TAKEOVER) {
        let program = reader.until(" session end ")?.to_owned();
        if !takeover_program(&program) {
            return Err(format!("a takeover program that is not plain: {program:?}"));
        }
        reader.eat(&session_id)?;
        reader.eat(" --abandoned --reason <why> (MCP: end {session:")?;
        if reader.number()? != number {
            return Err("a takeover hint naming another Session number".to_owned());
        }
        reader.eat(", abandoned:true})")?;
        Some(program)
    } else {
        None
    };
    let takeover = program.is_some();
    let named = usize::from(agent.is_some());
    let ended_by = ender_part(reader, quotes.get(named..).unwrap_or_default())?;
    if ended_by.is_some() && end_state != "crashed" {
        return Err("an ender on a Session line that did not end abandoned".to_owned());
    }
    let enders = usize::from(ended_by.as_ref().is_some_and(|by| by.starts_with("agent ")));
    if quotes.len() != named + enders {
        return Err("agent text on a Session line".to_owned());
    }
    reader.end()?;
    Ok(ParsedSession {
        number,
        session_id,
        end_state,
        started_at,
        last_recorded_at,
        resumed,
        takeover,
        program,
        agent,
        ended_by,
    })
}

/// U9 A7 (E1 revision 12): `; ended by Session <j>`, `; ended by unlisted
/// Session <id>`, or `; ended by agent「<name>」(<role>)`, if the line has it
/// here, at its end; the name is the first of `quotes` (the line's quotes
/// after the agent part's).
fn ender_part(reader: &mut Reader<'_>, quotes: &[String]) -> Result<Option<String>, String> {
    if !reader.try_eat("; ended by ") {
        return Ok(None);
    }
    if reader.try_eat("unlisted Session ") {
        let session_id = reader.0;
        if session_id.is_empty() || !session_id.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("an ender Session ID that is not plain".to_owned());
        }
        reader.0 = "";
        return Ok(Some(format!("unlisted Session {session_id}")));
    }
    if reader.try_eat("Session ") {
        return Ok(Some(format!("Session {}", reader.number()?)));
    }
    reader.eat("agent◊(")?;
    let name = quotes.first().ok_or("an ender without a name")?;
    let role = reader.until(")")?;
    Ok(Some(format!("agent {name} {role}")))
}

/// U8 A4 (E1 revision 10): `; agent「<name>」(<role>[, dispatched by
/// <parent>])`, if the line has it here; the name is the line's first
/// quote (an ender's by agent is the other, and `session_line` refuses any
/// more).
fn agent_part(
    reader: &mut Reader<'_>,
    quotes: &[String],
) -> Result<Option<(String, String, Option<String>)>, String> {
    Ok(if reader.try_eat("; agent◊(") {
        let name = quotes.first().ok_or("an agent without a name")?.clone();
        let inside = reader.until(")")?;
        let (role, parent) = match inside.split_once(", dispatched by ") {
            Some((role, parent)) => (role, Some(parent.to_owned())),
            None => (inside, None),
        };
        Some((name, role.to_owned(), parent))
    } else {
        None
    })
}

fn base_line(
    reader: &mut Reader<'_>,
    quotes: &[String],
    parsed: &mut Parsed,
) -> Result<(), String> {
    reader.eat("Base: ")?;
    let base = reader.hex()?;
    reader.eat("; files changed since: ")?;
    let files = if reader.0.starts_with(QUOTE) {
        let listed = reader.0.matches(QUOTE).count();
        for index in 0..listed {
            if index > 0 {
                reader.eat(", ")?;
            }
            reader.eat("◊")?;
        }
        let total = if reader.try_eat(" (") {
            let total = reader.number()?;
            reader.eat(" in all, the first 20 listed)")?;
            Some(total)
        } else {
            None
        };
        if listed != quotes.len() {
            return Err("base line quotes".to_owned());
        }
        Files::Listed(quotes.to_vec(), total)
    } else if let Ok(count) = reader.number() {
        reader.eat(" (not listed one by one)")?;
        Files::Counted(count)
    } else {
        let word = reader.0;
        if ![
            "none",
            "not compared",
            "not compared (more than 16 base commits)",
            "cannot be listed (the base commit is not in the repository, or the current state \
             cannot be read)",
        ]
        .contains(&word)
        {
            return Err(format!("unknown files {word}"));
        }
        reader.0 = "";
        Files::Word(word.to_owned())
    };
    if parsed.bases.iter().any(|(seen, _)| *seen == base) {
        return Err("a base line twice".to_owned());
    }
    parsed.bases.push((base, files));
    Ok(())
}

fn unlisted_line(reader: &mut Reader<'_>, parsed: &mut Parsed) -> Result<(), String> {
    reader.eat("Not listed: ")?;
    let parts = reader.until(" (bodies and evidence bindings not listed one by one)")?;
    for part in parts.split("; ") {
        let mut part_reader = Reader(part);
        if part_reader.try_eat("facts ") {
            let count = part_reader.number()?;
            part_reader.eat(" (")?;
            let inner = part_reader
                .0
                .strip_suffix(')')
                .ok_or("an unended fact count")?;
            let mut seen = 0;
            for group in inner.split(" / ") {
                let (word, rest) = group.split_once(' ').ok_or("a verdict group")?;
                let (number, list) = rest.split_once(": ").ok_or("a verdict group")?;
                let numbers = ranges(list)?;
                if number.parse::<u64>().map_err(|_| "count")? != numbers.len() as u64 {
                    return Err("a verdict count".to_owned());
                }
                seen += numbers.len() as u64;
                parsed.unlisted_facts.insert(word.to_owned(), numbers);
            }
            if seen != count {
                return Err("the fact count".to_owned());
            }
            continue;
        }
        if part_reader.try_eat("superseded or withdrawn ") {
            let count = part_reader.number()?;
            part_reader.eat(": ")?;
            parsed.unlisted_retired = ranges(part_reader.0)?;
            if parsed.unlisted_retired.len() as u64 != count {
                return Err("an unlisted retired count".to_owned());
            }
            continue;
        }
        let kind = if part_reader.try_eat("decisions ") {
            "decision"
        } else {
            part_reader.eat("open items ")?;
            "open_item"
        };
        let count = part_reader.number()?;
        part_reader.eat(": ")?;
        let numbers = ranges(part_reader.0)?;
        if numbers.len() as u64 != count {
            return Err("an unlisted count".to_owned());
        }
        parsed.unlisted_other.insert(kind, numbers);
    }
    Ok(())
}

fn folded_line(reader: &mut Reader<'_>, parsed: &mut Parsed) -> Result<(), String> {
    let part = |reader: &mut Reader<'_>, label: &str| -> Result<Vec<u64>, String> {
        reader.eat(label)?;
        let count = reader.number()?;
        let numbers = if reader.try_eat(": ") {
            let end = [reader.0.find("; "), reader.0.find(" (")]
                .into_iter()
                .flatten()
                .min()
                .ok_or("an unended list")?;
            let (list, rest) = reader.0.split_at(end);
            reader.0 = rest;
            ranges(list)?
        } else {
            Vec::new()
        };
        if u64::try_from(numbers.len()).unwrap() != count {
            return Err("folded count".to_owned());
        }
        Ok(numbers)
    };
    reader.eat("Superseded and withdrawn: ")?;
    parsed.superseded = part(reader, "superseded ")?;
    reader.eat("; ")?;
    parsed.withdrawn = part(reader, "withdrawn ")?;
    reader.eat(" (not listed one by one)")
}

fn omission_line(reader: &mut Reader<'_>, parsed: &mut Parsed) -> Result<(), String> {
    reader.eat("Omitted: level ")?;
    let level = reader.number()?;
    reader.eat("; bases with files only counted ")?;
    let counted = reader.number()?;
    reader.eat(", bodies shortened ")?;
    let shortened = reader.number()?;
    reader.eat(", bodies omitted ")?;
    let omitted = reader.number()?;
    reader.eat(", folded into one line ")?;
    let folded = reader.number()?;
    reader.eat(", not listed ")?;
    let unlisted = reader.number()?;
    reader.eat(OMISSION_TAIL)?;
    parsed.omission = vec![level, counted, shortened, omitted, folded, unlisted];
    Ok(())
}

fn task_line(
    reader: &mut Reader<'_>,
    quotes: &[String],
    parsed: &mut Parsed,
) -> Result<(), String> {
    reader.eat("Task◊ (")?;
    parsed.title = quotes.first().cloned().ok_or("no title")?;
    reader.until("): ")?;
    let head = reader.until("; active claims ")?;
    match head.split_once(" (reasons: ") {
        Some((word, reasons)) => {
            parsed.task_word = word.to_owned();
            parsed.task_reasons = reasons
                .strip_suffix(')')
                .ok_or("reasons")?
                .split(", ")
                .map(str::to_owned)
                .collect();
        }
        None => parsed.task_word = head.to_owned(),
    }
    let active = reader.number()?;
    reader.eat(": facts ")?;
    let facts = reader.number()?;
    if reader.try_eat(" (") {
        for part in reader.until(")")?.split(" / ") {
            let (word, count) = part.split_once(' ').ok_or("breakdown")?;
            parsed
                .breakdown
                .insert(word.to_owned(), count.parse().map_err(|_| "count")?);
        }
    }
    let mut counts = vec![active, facts];
    for label in [
        ", open items ",
        ", decisions ",
        ", next actions ",
        "; superseded ",
        "; withdrawn ",
    ] {
        reader.eat(label)?;
        counts.push(reader.number()?);
    }
    reader.eat(" (facts ")?;
    counts.push(reader.number()?);
    reader.eat(")")?;
    parsed.counts = counts;
    Ok(())
}

fn kind_of(word: &str) -> Result<&'static str, String> {
    Ok(match word {
        "fact" => "fact",
        "open item" => "open_item",
        "decision" => "decision",
        "next action" => "next_action",
        other => return Err(format!("unknown kind {other}")),
    })
}

/// `verified`, `fact stale`, ...: a fact that stands and its verdict.
fn fact_label(label: &str) -> Option<&'static str> {
    Some(match label {
        "verified" => "verified",
        "fact stale" => "stale",
        "fact unconfirmed" => "partial",
        "fact tampered" => "tampered",
        _ => return None,
    })
}

fn claim_body(reader: &mut Reader<'_>, quotes: &[String]) -> Result<(Body, usize), String> {
    if reader.try_eat("(body omitted, ") {
        let total = reader.number()?;
        reader.eat(" chars)")?;
        return Ok((Body::Omitted(total), 0));
    }
    reader.eat("◊")?;
    let text = quotes.first().cloned().ok_or("no body")?;
    let (rationale, used) = if reader.try_eat(", rationale ◊") {
        (Some(quotes.get(1).cloned().ok_or("no rationale")?), 2)
    } else {
        (None, 1)
    };
    if reader.try_eat("… (body ") {
        let total = reader.number()?;
        reader.eat(" chars, ")?;
        let dropped = reader.number()?;
        reader.eat(" omitted)")?;
        return Ok((Body::Shortened(text, rationale, total, dropped), used));
    }
    Ok((Body::Full(text, rationale), used))
}

/// The commits an Evidence word or a binding names, by their first digits.
fn commits_in(text: &str) -> Vec<String> {
    let mut commits = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("commit ") {
        rest = &rest[at + "commit ".len()..];
        let end = rest
            .find(|character: char| !matches!(character, '0'..='9' | 'a'..='f'))
            .unwrap_or(rest.len());
        if end > 0 {
            commits.push(rest[..end].to_owned());
        }
        rest = &rest[end..];
    }
    commits
}

fn claim_line(shape: &str, quotes: &[String]) -> Result<ParsedClaim, String> {
    let mut reader = Reader(shape);
    reader.eat("#")?;
    let number = reader.number()?;
    reader.eat(" ")?;
    let label = reader.until(": ")?;
    let (kind, standing, verdict, superseded_by) = if let Some(verdict) = fact_label(label) {
        ("fact", "active", Some(verdict.to_owned()), Vec::new())
    } else if let Some((kind, rest)) = label.split_once(", ") {
        if rest == "withdrawn" {
            (kind_of(kind)?, "withdrawn", None, Vec::new())
        } else {
            let by = rest
                .strip_prefix("superseded by ")
                .ok_or("a superseded label")?;
            (kind_of(kind)?, "superseded", None, ranges(by)?)
        }
    } else {
        let kind = kind_of(label)?;
        if kind == "fact" {
            return Err("a fact without a verdict".to_owned());
        }
        (kind, "active", None, Vec::new())
    };
    let (body, used) = claim_body(&mut reader, quotes)?;
    let inner = reader
        .0
        .strip_prefix(" (")
        .and_then(|inner| inner.strip_suffix(')'))
        .ok_or("no skeleton")?;
    let (head, from) = inner.rsplit_once("; ").ok_or("no Session")?;
    let named = author_named(from)?;
    let mut claim = ParsedClaim {
        number,
        kind,
        standing,
        verdict,
        reasons: Vec::new(),
        body,
        commits: commits_in(head),
        cited: None,
        superseded_by,
        withdrawal_reason: None,
        author: None,
    };
    let mut quotes_used = used;
    if claim.verdict.is_some() {
        let rest = match head.strip_prefix("reasons: ") {
            Some(rest) => {
                let (codes, rest) = rest.split_once("; ").ok_or("reasons")?;
                claim.reasons = codes.split(", ").map(str::to_owned).collect();
                rest
            }
            None => head,
        };
        let mut binding = Reader(rest);
        claim.commits = Vec::new();
        if !(binding.try_eat("no evidence cited")
            || binding.try_eat("the commit the evidence is bound to is unreadable"))
        {
            binding.eat("evidence bound to commit ")?;
            claim.commits.push(binding.hex()?);
            while binding.try_eat(", ") {
                claim.commits.push(binding.hex()?);
            }
        }
        binding.end()?;
    } else {
        let cited = match head.strip_prefix("withdrawal reason ◊; ") {
            Some(cited) if standing == "withdrawn" => {
                claim.withdrawal_reason =
                    Some(quotes.get(used).cloned().ok_or("no withdrawal reason")?);
                quotes_used += 1;
                cited
            }
            _ => head,
        };
        check_cited(cited)?;
        claim.cited = Some(cited.to_owned());
    }
    if named {
        claim.author = Some(quotes.get(quotes_used).cloned().ok_or("no agent name")?);
        quotes_used += 1;
    }
    if quotes_used != quotes.len() {
        return Err("unexpected quotes".to_owned());
    }
    Ok(claim)
}

/// The author part of a claim line: `from Session k`, `from Session k◊`
/// (U10 V6: the last quote of the line is the agent's name), or `from
/// unlisted Session <id>`; whether it has the name.
fn author_named(from: &str) -> Result<bool, String> {
    if from.starts_with("from unlisted Session ") {
        return Ok(false);
    }
    let rest = from.strip_prefix("from Session ").ok_or("no Session")?;
    let digits = rest.trim_end_matches(QUOTE);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("a Session number {rest:?}"));
    }
    match rest.len() - digits.len() {
        0 => Ok(false),
        length if length == QUOTE.len_utf8() => Ok(true),
        _ => Err("two names".to_owned()),
    }
}

/// Evidence in words (P3-L L6): no verdict word or code but `tampered`.
fn check_cited(cited: &str) -> Result<(), String> {
    let words = cited.strip_prefix("tampered; ").unwrap_or(cited);
    if words == "no evidence cited" {
        return Ok(());
    }
    let items = if let Some(only) = words.strip_prefix("evidence: ") {
        vec![only]
    } else {
        words
            .split("; ")
            .enumerate()
            .map(|(index, item)| {
                item.strip_prefix(&format!("evidence {}: ", index + 1))
                    .ok_or_else(|| format!("bad Evidence item {item}"))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    for item in items {
        let (result, state) = item.split_once(", ").ok_or("an Evidence item")?;
        let result = result
            .strip_suffix(
                " (overruled by a later failed run of the same command in the same state)",
            )
            .unwrap_or(result);
        if ![
            "passed",
            "failed",
            "not run",
            "unavailable",
            "missing",
            "repository state record",
            "result unknown",
        ]
        .contains(&result)
        {
            return Err(format!("bad result {result}"));
        }
        let commit = |text: &str| {
            let mut reader = Reader(text);
            reader.eat("the bound commit")?;
            if reader.try_eat(" ") {
                reader.hex()?;
            }
            reader.end()
        };
        let valid = matches!(state, "record tampered" | "unreadable")
            || state
                .strip_suffix(" is no longer the current state")
                .is_some_and(|text| commit(text).is_ok())
            || state
                .strip_suffix(" is still the current state")
                .is_some_and(|text| commit(text).is_ok())
            || state
                .strip_prefix("whether ")
                .and_then(|text| text.strip_suffix(" is still the current state is unconfirmed"))
                .is_some_and(|text| commit(text).is_ok());
        if !valid {
            return Err(format!("bad state {state}"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// The properties, against the Capsule JSON.
// ---------------------------------------------------------------------

/// One claim as the Capsule JSON has it.
#[derive(Debug, Clone)]
struct Expected {
    number: u64,
    kind: String,
    standing: String,
    verdict: String,
    reasons: Vec<String>,
    text: String,
    rationale: Option<String>,
    bases: Vec<String>,
    evidence: Vec<Value>,
    superseded_by: Vec<u64>,
    withdrawal_reason: Option<String>,
    /// U10 V6: the agent name of its Session, when `sessions[]` lists the
    /// Session and it declared one.
    author: Option<String>,
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn expected_claims(capsule: &Value) -> BTreeMap<u64, Expected> {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|claim| {
            let number = claim["number"].as_u64().unwrap();
            let evidence = claim["evidence"].as_array().unwrap().clone();
            let mut bases: Vec<String> = Vec::new();
            for base in evidence
                .iter()
                .filter_map(|item| item["base_commit"].as_str())
            {
                if !bases.iter().any(|seen| seen == base) {
                    bases.push(base.to_owned());
                }
            }
            (
                number,
                Expected {
                    number,
                    kind: claim["kind"].as_str().unwrap().to_owned(),
                    standing: claim["standing"].as_str().unwrap().to_owned(),
                    verdict: claim["verdict"].as_str().unwrap().to_owned(),
                    reasons: strings(&claim["reasons"]),
                    text: claim["text"].as_str().unwrap().to_owned(),
                    rationale: claim["rationale"].as_str().map(str::to_owned),
                    bases,
                    evidence,
                    superseded_by: claim["superseded_by"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|by| by["number"].as_u64().unwrap())
                        .collect(),
                    withdrawal_reason: claim["withdrawal_reason"].as_str().map(str::to_owned),
                    author: capsule["sessions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|session| session["session_id"] == claim["session_id"])
                        .and_then(|session| session["agent"]["name"].as_str())
                        .map(str::to_owned),
                },
            )
        })
        .collect()
}

fn word(verdict: &str) -> &'static str {
    match verdict {
        "verified" => "verified",
        "stale" => "stale",
        "partial" => "unconfirmed",
        _ => "tampered",
    }
}

/// What P3-L L6 says of one Evidence object, as this test reads it.
fn expected_words(evidence: &Value) -> String {
    let reasons = strings(&evidence["reasons"]);
    let has = |code: &str| reasons.iter().any(|reason| reason == code);
    let status = evidence["run_status"].as_str();
    let result = match status {
        Some("PASS") => "passed",
        Some("FAIL") => "failed",
        Some("NOT_RUN") => "not run",
        Some("UNAVAILABLE") => "unavailable",
        _ if has("object_missing") => "missing",
        _ if matches!(
            evidence["kind"].as_str(),
            Some("git_head" | "git_worktree" | "file_digest")
        ) =>
        {
            "repository state record"
        }
        _ => "result unknown",
    };
    let overruled = if status == Some("PASS") && has("evidence_not_passed") {
        " (overruled by a later failed run of the same command in the same state)"
    } else {
        ""
    };
    let commit = match evidence["base_commit"].as_str() {
        Some(base) => format!("the bound commit {}", &base[..base.len().min(12)]),
        None => "the bound commit".to_owned(),
    };
    let state = match evidence["verdict"].as_str().unwrap() {
        "tampered" => "record tampered".to_owned(),
        _ if has("object_missing") => "unreadable".to_owned(),
        "stale" => format!("{commit} is no longer the current state"),
        "verified" => format!("{commit} is still the current state"),
        "partial" if has("evidence_not_passed") => format!("{commit} is still the current state"),
        _ => format!("whether {commit} is still the current state is unconfirmed"),
    };
    format!("{result}{overruled}, {state}")
}

fn expected_cited(claim: &Expected) -> String {
    let words = match &claim.evidence[..] {
        [] => "no evidence cited".to_owned(),
        [only] => format!("evidence: {}", expected_words(only)),
        several => several
            .iter()
            .enumerate()
            .map(|(index, item)| format!("evidence {}: {}", index + 1, expected_words(item)))
            .collect::<Vec<_>>()
            .join("; "),
    };
    if claim.verdict == "tampered" {
        format!("tampered; {words}")
    } else {
        words
    }
}

/// The body of `claim` as the Pack must show it in `body`; how many of its
/// characters are shown.
fn check_body(claim: &Expected, body: &Body) -> Result<u64, String> {
    let text_count = claim.text.chars().count();
    let total = text_count + claim.rationale.as_deref().map_or(0, |r| r.chars().count());
    let total = u64::try_from(total).unwrap();
    match body {
        Body::Full(text, shown) => {
            if *text != claim.text || shown != &claim.rationale {
                return Err(format!("#{} body differs", claim.number));
            }
            Ok(total)
        }
        Body::Shortened(text, shown, said, dropped) => {
            // The first 120 characters of the text, then of the rationale.
            let kept_text = claim.text.chars().take(120).collect::<String>();
            let kept_rationale = (text_count < 120)
                .then(|| {
                    claim
                        .rationale
                        .as_deref()
                        .map(|r| r.chars().take(120 - text_count).collect::<String>())
                })
                .flatten();
            if total <= 120
                || *said != total
                || *dropped != total - 120
                || *text != kept_text
                || *shown != kept_rationale
            {
                return Err(format!("#{} shortened wrongly: {body:?}", claim.number));
            }
            Ok(120)
        }
        Body::Omitted(said) => {
            if *said != total {
                return Err(format!("#{} omitted length", claim.number));
            }
            Ok(0)
        }
    }
}

/// One claim line against its claim: the skeleton of a fact (number,
/// verdict, reasons), the kind and Evidence words of any other, the
/// commits by their first digits.
fn check_claim_line(claim: &ParsedClaim, expected: &Expected) -> Result<(), String> {
    let number = claim.number;
    if claim.kind != expected.kind || claim.standing != expected.standing {
        return Err(format!("#{number}: {claim:?} != {expected:?}"));
    }
    if claim.author != expected.author {
        return Err(format!("#{number}: author {:?}", claim.author));
    }
    if let Some(verdict) = &claim.verdict {
        if *verdict != expected.verdict || claim.reasons != expected.reasons {
            return Err(format!("#{number}: verdict or reasons"));
        }
        let shorts = expected
            .bases
            .iter()
            .map(|base| base[..base.len().min(12)].to_owned())
            .collect::<Vec<_>>();
        if claim.commits != shorts {
            return Err(format!("#{number}: binding {:?}", claim.commits));
        }
    } else {
        if claim.cited.as_deref() != Some(expected_cited(expected).as_str()) {
            return Err(format!("#{number}: Evidence words {:?}", claim.cited));
        }
        if claim.standing == "superseded" && claim.superseded_by != expected.superseded_by {
            return Err(format!("#{number}: superseded by"));
        }
        if claim.withdrawal_reason != expected.withdrawal_reason && claim.standing == "withdrawn" {
            return Err(format!("#{number}: withdrawal reason"));
        }
    }
    if claim.commits.iter().any(|commit| commit.len() > 12) {
        return Err(format!("#{number}: a commit longer than 12 digits"));
    }
    Ok(())
}

/// The base lines are those of the bases the listed claims name, in the
/// order of their first line, each with its changed files.
fn check_bases(
    capsule: &Value,
    claims: &BTreeMap<u64, Expected>,
    parsed: &Parsed,
    counting: bool,
) -> Result<u64, String> {
    let mut expected: Vec<&String> = Vec::new();
    for claim in &parsed.claims {
        for base in &claims[&claim.number].bases {
            if !expected.contains(&base) {
                expected.push(base);
            }
        }
    }
    let seen = parsed
        .bases
        .iter()
        .map(|(base, _)| base)
        .collect::<Vec<_>>();
    if seen != expected {
        return Err(format!("base lines {seen:?} != {expected:?}"));
    }
    let mut counted = 0;
    for (base, files) in &parsed.bases {
        let changes = capsule["changed_paths_by_base"]
            .as_array()
            .unwrap()
            .iter()
            .find(|change| change["base_commit"] == base.as_str());
        let paths = changes
            .map(|change| {
                strings(&change["paths"])
                    .into_iter()
                    .map(|path| path.strip_prefix("repo://").unwrap_or(&path).to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let word = match changes {
            None => Some("not compared"),
            Some(change) if change["probed"] == false => {
                Some("not compared (more than 16 base commits)")
            }
            Some(change) if change["complete"] == false => Some(
                "cannot be listed (the base commit is not in the repository, or the current \
                 state cannot be read)",
            ),
            Some(_) if paths.is_empty() => Some("none"),
            Some(_) => None,
        };
        let total = u64::try_from(paths.len()).unwrap();
        let fits = match (files, word) {
            (Files::Word(said), Some(word)) => said == word,
            (Files::Counted(count), None) => {
                counted += 1;
                counting && *count == total
            }
            (Files::Listed(listed, over), None) => {
                *listed == paths.iter().take(20).cloned().collect::<Vec<_>>()
                    && *over == (total > 20).then_some(total)
            }
            _ => false,
        };
        if !fits {
            return Err(format!("base {base}: {files:?}"));
        }
    }
    Ok(counted)
}

/// Every property of one Pack of `capsule` at `budget`: the length, the
/// skeleton of each listed claim, the unlisted counts, the base lines, the
/// counts, "verified", and the digest. Returns the parsed Pack, with how many
/// characters of each active claim's body are shown.
pub(super) fn check(capsule: &Value, text: &str, budget: usize) -> Result<Parsed, String> {
    check_owned(capsule, text, budget, None)
}

/// U7 P1: [`check`] of a Pack made by the reader whose own Session is
/// `own`: the `Session ` line of a Session that has not ended ends with the
/// takeover hint exactly when it is not `own`.
pub(super) fn check_owned(
    capsule: &Value,
    text: &str,
    budget: usize,
    own: Option<&str>,
) -> Result<Parsed, String> {
    if text.encode_utf16().count() > budget {
        return Err(format!("over the budget {budget}"));
    }
    let mut parsed = parse(text)?;
    if usize::try_from(parsed.budget).unwrap() != budget {
        return Err("budget".to_owned());
    }
    if parsed.capsule_digest != capsule["capsule_digest"].as_str().unwrap()
        || parsed.sequence
            != capsule["bindings"]["ledger"]["project_sequence"]
                .as_u64()
                .unwrap()
        || parsed.title != capsule["task"]["title"].as_str().unwrap()
        || parsed.task_word != word(capsule["status"].as_str().unwrap())
        || parsed.task_reasons != strings(&capsule["reasons"])
    {
        return Err("header or Task line".to_owned());
    }
    if parsed.query.is_some() {
        return Err("a query line in a Pack made without a query".to_owned());
    }
    if parsed.last_passed != expected_last_passed(capsule) {
        return Err(format!("last-passed line {:?}", parsed.last_passed));
    }
    let claims = expected_claims(capsule);
    check_counts(&claims, &parsed)?;
    check_retired(&claims, &parsed)?;
    let level = parsed.omission[0];
    let counted = check_bases(capsule, &claims, &parsed, level >= 1)?;
    if counted != parsed.omission[1] {
        return Err("counted base lines".to_owned());
    }
    parsed.shown = check_claims(&claims, &parsed)?;
    check_verified(capsule, &claims, &parsed)?;
    check_integrity(capsule, &parsed, true, own)?;
    check_actions(capsule, &parsed)?;
    Ok(parsed)
}

/// U3 G4: the claims a query Pack of `matches` (best first) must list, in
/// order, as this test reads O8: each match, then the claims that
/// supersede it, in turn, depth first, unless already listed; then, when
/// anything matched, each active next action in continuation order.
pub(super) fn expected_query_order(capsule: &Value, matches: &[u64]) -> Vec<u64> {
    fn after(claims: &BTreeMap<u64, Expected>, number: u64, order: &mut Vec<u64>) {
        for by in &claims[&number].superseded_by {
            if !order.contains(by) {
                order.push(*by);
                after(claims, *by, order);
            }
        }
    }
    let claims = expected_claims(capsule);
    let mut order: Vec<u64> = Vec::new();
    for number in matches {
        if !order.contains(number) {
            order.push(*number);
        }
        after(&claims, *number, &mut order);
    }
    if !order.is_empty() {
        for item in capsule["continuation"].as_array().unwrap() {
            if item["class"] == 8 {
                let number = item["args"]["number"].as_u64().unwrap();
                if !order.contains(&number) {
                    order.push(number);
                }
            }
        }
    }
    order
}

/// U3 G4 (Owner O8, O9): every property of a query Pack of `capsule` at
/// `budget` for `query` and its `matches`: the `Query:` line, the header,
/// Task, integrity, Session, and action lines as in [`check`], no rerun
/// and no `Superseded and withdrawn:` line, and the claims of [`expected_query_order`]
/// each listed in that order with its skeleton, folded (an active verified
/// fact), or no longer listed (level 5) and counted, the next action always
/// in full.
#[allow(clippy::too_many_lines)] // Every property, checked in one place.
pub(super) fn check_query(
    capsule: &Value,
    text: &str,
    budget: usize,
    query: &str,
    matches: &[u64],
) -> Result<Parsed, String> {
    if text.encode_utf16().count() > budget {
        return Err(format!("over the budget {budget}"));
    }
    let mut parsed = parse(text)?;
    let expected_query = (
        query.to_owned(),
        u64::try_from(matches.len()).unwrap(),
        matches.to_vec(),
    );
    if parsed.query.as_ref() != Some(&expected_query) {
        return Err(format!("query line {:?}", parsed.query));
    }
    if parsed.last_passed != expected_last_passed(capsule) {
        return Err(format!("last-passed line {:?}", parsed.last_passed));
    }
    if usize::try_from(parsed.budget).unwrap() != budget
        || parsed.capsule_digest != capsule["capsule_digest"].as_str().unwrap()
        || parsed.title != capsule["task"]["title"].as_str().unwrap()
        || parsed.task_word != word(capsule["status"].as_str().unwrap())
    {
        return Err("header or Task line".to_owned());
    }
    if !parsed.superseded.is_empty() || !parsed.withdrawn.is_empty() {
        return Err("a Superseded and withdrawn line in a query Pack".to_owned());
    }
    let claims = expected_claims(capsule);
    check_counts(&claims, &parsed)?;
    let order = expected_query_order(capsule, matches);
    let level = parsed.omission[0];
    let counted = check_bases(capsule, &claims, &parsed, level >= 1)?;
    if counted != parsed.omission[1] {
        return Err("counted base lines".to_owned());
    }
    // The listed claims, in the expected order.
    let listed = parsed
        .claims
        .iter()
        .map(|claim| claim.number)
        .collect::<Vec<_>>();
    let in_order = order
        .iter()
        .copied()
        .filter(|number| listed.contains(number))
        .collect::<Vec<_>>();
    if listed != in_order {
        return Err(format!("claim lines {listed:?} not in the order {order:?}"));
    }
    let (mut shortened, mut omitted) = (0, 0);
    for claim in &parsed.claims {
        let expected = &claims[&claim.number];
        check_claim_line(claim, expected)?;
        match claim.body {
            Body::Shortened(..) => shortened += 1,
            Body::Omitted(_) => omitted += 1,
            Body::Full(..) => {}
        }
        if expected.kind == "next_action"
            && expected.standing == "active"
            && !matches!(claim.body, Body::Full(..))
        {
            return Err("a next action lost its body".to_owned());
        }
        parsed
            .shown
            .insert(claim.number, check_body(expected, &claim.body)?);
    }
    let mut unlisted_facts: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut unlisted_other: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut unlisted_retired = Vec::new();
    for number in order.iter().filter(|number| !listed.contains(number)) {
        let expected = &claims[number];
        parsed.shown.insert(*number, 0);
        if parsed.fold.contains(number) {
            if expected.kind != "fact"
                || expected.verdict != "verified"
                || expected.standing != "active"
                || level < 4
            {
                return Err(format!("#{number} folded wrongly"));
            }
        } else if level == 5 && expected.standing != "active" {
            unlisted_retired.push(*number);
        } else if level == 5 && expected.kind != "next_action" {
            if expected.kind == "fact" {
                unlisted_facts
                    .entry(word(&expected.verdict).to_owned())
                    .or_default()
                    .push(*number);
            } else {
                unlisted_other
                    .entry(if expected.kind == "decision" {
                        "decision"
                    } else {
                        "open_item"
                    })
                    .or_default()
                    .push(*number);
            }
        } else {
            return Err(format!("#{number} is missing"));
        }
    }
    for numbers in unlisted_facts
        .values_mut()
        .chain(unlisted_other.values_mut())
    {
        numbers.sort_unstable();
    }
    unlisted_retired.sort_unstable();
    if unlisted_facts != parsed.unlisted_facts
        || unlisted_other != parsed.unlisted_other
        || unlisted_retired != parsed.unlisted_retired
    {
        return Err(format!(
            "unlisted {:?} {:?} {:?}",
            parsed.unlisted_facts, parsed.unlisted_other, parsed.unlisted_retired
        ));
    }
    if parsed.fold.iter().any(|number| !order.contains(number)) {
        return Err("a fold of a claim not selected".to_owned());
    }
    let folded = u64::try_from(parsed.fold.len()).unwrap();
    let unlisted = (unlisted_facts
        .values()
        .chain(unlisted_other.values())
        .map(Vec::len)
        .sum::<usize>()
        + unlisted_retired.len()) as u64;
    if parsed.omission[2..] != [shortened, omitted, folded, unlisted] {
        return Err(format!("omission counts {:?}", parsed.omission));
    }
    check_verified(capsule, &claims, &parsed)?;
    check_integrity(capsule, &parsed, false, None)?;
    check_actions(capsule, &parsed)?;
    Ok(parsed)
}

/// ADR 0014 E3 (P4-D): one action line per action, in order, at every
/// level, with what the Capsule says (condition e: only fixed words,
/// numbers, and codes).
fn check_actions(capsule: &Value, parsed: &Parsed) -> Result<(), String> {
    use std::fmt::Write as _;
    let sessions = capsule["sessions"].as_array().unwrap();
    let mut expected = Vec::new();
    for action in capsule["actions"].as_array().unwrap() {
        let reasons = strings(&action["reasons"]);
        let args = &action["args"];
        let (what, operation) = match action["action"].as_str().unwrap() {
            "complete_task" => ("complete the Task", "transitionTask"),
            "begin_session" => ("begin a new Session", "createSession"),
            "cite_latest_run" => ("record a fact citing the latest run", "createCheckpoint"),
            other => return Err(format!("unknown action {other}")),
        };
        let mut text = format!("{what} ({operation}");
        if reasons.is_empty() {
            if let Some(cited) = args["completion_evidence_ids"].as_array() {
                write!(
                    text,
                    "; cites {} passed runs of the current repository state",
                    cited.len()
                )
                .unwrap();
            }
        } else {
            // U5 H5 (Owner O7): the sentence of the daemon's `detail` after
            // `invalid_request` of citing the latest run, by what `args`
            // show; none without a run (S3: it depends on the request).
            let detail = if action["action"] == "cite_latest_run" {
                if args["evidence_id"].is_null() {
                    ""
                } else if args["kind"] == "test_result"
                    && args["run_status"] == "PASS"
                    && !args["overruled_by"].is_null()
                {
                    "1 verified_fact citation names an overruled passed run"
                } else {
                    "verified_fact requires successful executed test Evidence"
                }
            } else {
                ""
            };
            let shown = reasons
                .iter()
                .map(|reason| {
                    if reason == "invalid_request" && !detail.is_empty() {
                        format!("{reason} ({detail})")
                    } else {
                        reason.clone()
                    }
                })
                .collect::<Vec<_>>();
            write!(text, "; reasons: {}", shown.join(", ")).unwrap();
        }
        if let Some(open) = args["open_session_ids"]
            .as_array()
            .filter(|open| !open.is_empty())
        {
            let numbers = open
                .iter()
                .map(|id| {
                    (sessions
                        .iter()
                        .position(|session| session["session_id"] == *id)
                        .unwrap()
                        + 1)
                    .to_string()
                })
                .collect::<Vec<_>>();
            // U10 T1 (Owner O3; D3 F16): an available begin says to end
            // them by their lines first, or to make sure they still work.
            let numbers = numbers.join(", ");
            match (reasons.is_empty(), open.len()) {
                (false, _) => write!(text, "; open Sessions {numbers}"),
                (true, 1) => write!(
                    text,
                    "; first end it as the line of Session {numbers} says, or confirm it is still \
                     working"
                ),
                (true, _) => write!(
                    text,
                    "; first end them as the lines of Sessions {numbers} say, or confirm they are \
                     still working"
                ),
            }
            .unwrap();
        }
        write!(text, "; needs {})", action["scope"].as_str().unwrap()).unwrap();
        expected.push((reasons.is_empty(), text));
    }
    if parsed.actions != expected {
        return Err(format!("actions {:?} != {expected:?}", parsed.actions));
    }
    Ok(())
}

/// P4-H1 (Owner, T20): the numbers of the Sessions that have a line, as
/// this test reads the rule: every open one; of those that ended (normally
/// or abandoned), the last one; and the last three that ended abandoned.
/// "Last" is the order of `sessions[]`, the Session numbering.
pub(super) fn expected_session_numbers(sessions: &[Value]) -> BTreeSet<u64> {
    let numbered = sessions
        .iter()
        .zip(1_u64..)
        .map(|(session, number)| (number, session["end_state"].as_str().unwrap()))
        .collect::<Vec<_>>();
    let mut expected = numbered
        .iter()
        .filter(|(_, end)| *end == "open")
        .map(|(number, _)| *number)
        .collect::<BTreeSet<_>>();
    let ended = numbered
        .iter()
        .filter(|(_, end)| *end != "open")
        .collect::<Vec<_>>();
    expected.extend(ended.last().map(|(number, _)| *number));
    expected.extend(
        ended
            .iter()
            .rev()
            .filter(|(_, end)| *end == "crashed")
            .take(3)
            .map(|(number, _)| *number),
    );
    expected
}

/// The `Sessions:` counts (with the declared ones, P4-H1) and each
/// `Session ` line against the Capsule's `sessions[]`: exactly the Sessions
/// [`expected_session_numbers`] names, in order, each with its ID, end,
/// start, last write, and its declared continuation source by the first
/// 12 digits of the `pack_digest` (ADR 0014 E6, P4-H1), and (U7 P1) the
/// takeover hint on the line of each Session that has not ended and is not
/// `own`, and on no other.
fn check_sessions(capsule: &Value, parsed: &Parsed, own: Option<&str>) -> Result<(), String> {
    let sessions = capsule["sessions"].as_array().unwrap();
    let count = |state: &str| {
        u64::try_from(
            sessions
                .iter()
                .filter(|session| session["end_state"] == state)
                .count(),
        )
        .unwrap()
    };
    let declared = u64::try_from(
        sessions
            .iter()
            .filter(|session| !session["resumed_from"].is_null())
            .count(),
    )
    .unwrap();
    let all = u64::try_from(sessions.len()).unwrap();
    let summary = [
        all,
        count("ended"),
        count("crashed"),
        count("open"),
        declared,
    ];
    if parsed.sessions != summary {
        return Err(format!(
            "Session summary {:?} != {summary:?}",
            parsed.sessions
        ));
    }
    let listed = parsed
        .session_lines
        .iter()
        .map(|line| line.number)
        .collect::<Vec<_>>();
    let expected = expected_session_numbers(sessions)
        .into_iter()
        .collect::<Vec<_>>();
    if listed != expected {
        return Err(format!("Session lines {listed:?} != {expected:?}"));
    }
    // U10 T7: one reader, one program on every hint of its Pack.
    let programs = parsed
        .session_lines
        .iter()
        .filter_map(|line| line.program.as_deref())
        .collect::<BTreeSet<_>>();
    if programs.len() > 1 {
        return Err(format!("takeover hints of several programs: {programs:?}"));
    }
    for line in &parsed.session_lines {
        let session = &sessions[usize::try_from(line.number - 1).unwrap()];
        let last = if line.end_state == "open" {
            session["last_recorded_at"].as_str().map(str::to_owned)
        } else {
            None
        };
        let from = &session["resumed_from"];
        let resumed = (!from.is_null()).then(|| {
            let digest = from["pack_digest"].as_str().unwrap();
            (
                from["project_sequence"].as_u64().unwrap(),
                digest["sha256:".len().."sha256:".len() + 12].to_owned(),
            )
        });
        if session["session_id"] != line.session_id.as_str()
            || session["end_state"] != line.end_state
            || session["started_at"] != line.started_at.as_str()
            || line.last_recorded_at != last
            || line.resumed != resumed
            || line.takeover != (line.end_state == "open" && own != Some(line.session_id.as_str()))
            // U10 T1 (Owner O3): every line names its Session's agent, the
            // line with the takeover hint too.
            || line.agent != expected_agent(sessions, session)
            // U9 A7: who ended it, on the line of an abandoned Session.
            || line.ended_by != expected_ender(sessions, session)
        {
            return Err(format!("Session line {} disagrees: {line:?}", line.number));
        }
    }
    Ok(())
}

/// U9 A7: the `; ended by …` part a Session's line must have, from the
/// Capsule's `ended_by`: `Session <j>` for an ender Session `sessions`
/// lists, `unlisted Session <id>` for one it does not, else `agent <name>
/// <role>`; none without `ended_by`.
fn expected_ender(sessions: &[Value], session: &Value) -> Option<String> {
    let ended_by = session.get("ended_by").filter(|by| !by.is_null())?;
    if let Some(ender) = ended_by["session_id"].as_str() {
        return Some(
            sessions
                .iter()
                .position(|other| other["session_id"] == ender)
                .map_or_else(
                    || format!("unlisted Session {ender}"),
                    |index| format!("Session {}", index + 1),
                ),
        );
    }
    let agent = &ended_by["agent"];
    Some(format!(
        "agent {} {}",
        agent["name"].as_str().unwrap(),
        agent["role"].as_str().unwrap()
    ))
}

/// U8 A4: the `agent` part a Session's line must have, from the Capsule:
/// its name, role, and parent (`Session <j>` for one `sessions` lists,
/// `unlisted Session <id>` otherwise); none without an `agent`.
fn expected_agent(sessions: &[Value], session: &Value) -> Option<(String, String, Option<String>)> {
    let agent = session.get("agent").filter(|agent| !agent.is_null())?;
    let parent = agent["parent_session_id"].as_str().map(|parent| {
        sessions
            .iter()
            .position(|other| other["session_id"] == parent)
            .map_or_else(
                || format!("unlisted Session {parent}"),
                |index| format!("Session {}", index + 1),
            )
    });
    Some((
        agent["name"].as_str().unwrap().to_owned(),
        agent["role"].as_str().unwrap().to_owned(),
        parent,
    ))
}

fn check_counts(claims: &BTreeMap<u64, Expected>, parsed: &Parsed) -> Result<(), String> {
    let count = |kind: Option<&str>, standing: &str| {
        u64::try_from(
            claims
                .values()
                .filter(|claim| claim.standing == standing && kind.is_none_or(|k| claim.kind == k))
                .count(),
        )
        .unwrap()
    };
    let expected = vec![
        count(None, "active"),
        count(Some("fact"), "active"),
        count(Some("open_item"), "active"),
        count(Some("decision"), "active"),
        count(Some("next_action"), "active"),
        count(None, "superseded"),
        count(None, "withdrawn"),
        count(Some("fact"), "withdrawn"),
    ];
    if parsed.counts != expected {
        return Err(format!("counts {:?} != {expected:?}", parsed.counts));
    }
    let mut breakdown = BTreeMap::new();
    for claim in claims.values() {
        if claim.kind == "fact" && claim.standing == "active" {
            *breakdown
                .entry(word(&claim.verdict).to_owned())
                .or_insert(0) += 1;
        }
    }
    if parsed.breakdown != breakdown {
        return Err("fact breakdown".to_owned());
    }
    Ok(())
}

/// The `Superseded and withdrawn:` line of a Pack made without a query.
fn check_retired(claims: &BTreeMap<u64, Expected>, parsed: &Parsed) -> Result<(), String> {
    let numbers = |standing: &str| {
        claims
            .values()
            .filter(|claim| claim.standing == standing)
            .map(|claim| claim.number)
            .collect::<Vec<_>>()
    };
    if parsed.superseded != numbers("superseded") || parsed.withdrawn != numbers("withdrawn") {
        return Err("superseded or withdrawn numbers".to_owned());
    }
    Ok(())
}

/// Property b (as revised in P4-C2): every active claim is listed with its
/// skeleton, folded (a `verified` fact at level 4 or 5), or unlisted (level
/// 5) and counted there, a fact by its verdict, another by its kind; the
/// omission line counts them; a next action is always listed in full.
fn check_claims(
    claims: &BTreeMap<u64, Expected>,
    parsed: &Parsed,
) -> Result<BTreeMap<u64, u64>, String> {
    let level = parsed.omission[0];
    let mut shown = BTreeMap::new();
    let mut listed = BTreeSet::new();
    let (mut shortened, mut omitted) = (0, 0);
    for claim in &parsed.claims {
        let expected = claims
            .get(&claim.number)
            .filter(|expected| expected.standing == "active")
            .ok_or_else(|| format!("#{} is not an active claim", claim.number))?;
        check_claim_line(claim, expected)?;
        if !listed.insert(claim.number) {
            return Err(format!("#{} twice", claim.number));
        }
        match claim.body {
            Body::Shortened(..) => shortened += 1,
            Body::Omitted(_) => omitted += 1,
            Body::Full(..) => {}
        }
        if expected.kind == "next_action" && !matches!(claim.body, Body::Full(..)) {
            return Err("a next action lost its body".to_owned());
        }
        shown.insert(claim.number, check_body(expected, &claim.body)?);
    }
    let mut unlisted_facts: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut unlisted_other: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    for expected in claims.values().filter(|claim| claim.standing == "active") {
        if listed.contains(&expected.number) {
            continue;
        }
        shown.insert(expected.number, 0);
        if parsed.fold.contains(&expected.number) {
            if expected.kind != "fact" || expected.verdict != "verified" || level < 4 {
                return Err(format!("#{} folded wrongly", expected.number));
            }
        } else if level == 5 && expected.kind != "next_action" {
            if expected.kind == "fact" {
                unlisted_facts
                    .entry(word(&expected.verdict).to_owned())
                    .or_default()
                    .push(expected.number);
            } else {
                let kind = if expected.kind == "decision" {
                    "decision"
                } else {
                    "open_item"
                };
                unlisted_other
                    .entry(kind)
                    .or_default()
                    .push(expected.number);
            }
        } else {
            return Err(format!("#{} is missing", expected.number));
        }
    }
    if unlisted_facts != parsed.unlisted_facts || unlisted_other != parsed.unlisted_other {
        return Err(format!(
            "unlisted {:?} {:?}",
            parsed.unlisted_facts, parsed.unlisted_other
        ));
    }
    if unlisted_facts.contains_key("verified") {
        return Err("a verified fact unlisted".to_owned());
    }
    let folded = u64::try_from(parsed.fold.len()).unwrap();
    let unlisted = unlisted_facts
        .values()
        .chain(unlisted_other.values())
        .map(Vec::len)
        .sum::<usize>() as u64;
    if parsed.omission[2..] != [shortened, omitted, folded, unlisted] {
        return Err(format!("omission counts {:?}", parsed.omission));
    }
    Ok(shown)
}

/// "verified" (as a word) outside 「」 only on the Task line, an active
/// `verified` fact, and the fold line; nowhere when the Capsule is
/// `tampered`.
fn check_verified(
    capsule: &Value,
    claims: &BTreeMap<u64, Expected>,
    parsed: &Parsed,
) -> Result<(), String> {
    let tampered = capsule["status"] == "tampered";
    for (kind, number) in &parsed.verified {
        let allowed = match *kind {
            "task" | "fold" => !tampered,
            "claim" => claims.get(number).is_some_and(|claim| {
                claim.kind == "fact" && claim.standing == "active" && claim.verdict == "verified"
            }),
            _ => false,
        };
        if !allowed {
            return Err(format!("verified on a {kind} line #{number}"));
        }
    }
    Ok(())
}

/// The integrity, Session, rerun (`reruns`: a Pack made without a query
/// lists them; a query Pack does not), and gap lines.
fn check_integrity(
    capsule: &Value,
    parsed: &Parsed,
    reruns: bool,
    own: Option<&str>,
) -> Result<(), String> {
    let damaged = capsule["damaged_records"].as_array().unwrap();
    let chain = !capsule["bindings"]["ledger"]["chain_valid"]
        .as_bool()
        .unwrap();
    let expected = damaged.len() + usize::from(chain);
    if parsed.integrity.len() != expected.max(1) {
        return Err("integrity lines".to_owned());
    }
    for record in damaged {
        let id = record["record_id"].as_str().unwrap();
        let record_type = record["record_type"].as_str().unwrap();
        if !parsed
            .integrity
            .iter()
            .any(|line| line.starts_with(&format!("Integrity: {record_type} {id} ")))
        {
            return Err(format!("damaged record {id} not listed"));
        }
    }
    // P4-C2 revision 6: the first integrity line of a tampered Pack.
    let note = "; claim numbers may differ from before";
    let tampered = capsule["status"] == "tampered";
    if parsed.integrity[0].ends_with(note) != tampered
        || parsed.integrity[1..].iter().any(|line| line.contains(note))
    {
        return Err("the tampered note".to_owned());
    }
    check_sessions(capsule, parsed, own)?;
    let reruns = capsule["continuation"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| reruns && item["class"] == 5)
        .map(|item| strings(&item["args"]["argv"]).join(" "))
        .collect::<Vec<_>>();
    if parsed.reruns != reruns || parsed.gaps != strings(&capsule["gaps"]) {
        return Err("reruns or gaps".to_owned());
    }
    Ok(())
}

/// Property c: a larger budget (`later`) lists every claim and base the
/// smaller one (`earlier`) listed, shows no less of any body, and counts no
/// base whose files the smaller one listed.
pub(super) fn assert_no_less(earlier: &Parsed, later: &Parsed, label: &str) {
    let listed = |parsed: &Parsed| {
        parsed
            .claims
            .iter()
            .map(|claim| claim.number)
            .collect::<BTreeSet<_>>()
    };
    assert!(listed(earlier).is_subset(&listed(later)), "{label}: claims");
    for (number, kept) in &earlier.shown {
        assert!(later.shown[number] >= *kept, "{label}: #{number}");
    }
    for (base, files) in &earlier.bases {
        let after = later
            .bases
            .iter()
            .find(|(seen, _)| seen == base)
            .unwrap_or_else(|| panic!("{label}: base {base}"));
        assert!(
            !matches!(after.1, Files::Counted(_)) || matches!(files, Files::Counted(_)),
            "{label}: base {base} counted"
        );
    }
}

/// `--claims` output (P4-C2 revision 5): base lines, then the full line of
/// each claim named; returns the numbers in the order listed.
pub(super) fn check_claims_output(capsule: &Value, text: &str) -> Result<Vec<u64>, String> {
    let parsed = parse_lines(text, true)?;
    let claims = expected_claims(capsule);
    let mut numbers = Vec::new();
    for claim in &parsed.claims {
        let expected = claims
            .get(&claim.number)
            .ok_or_else(|| format!("#{} is no claim", claim.number))?;
        check_claim_line(claim, expected)?;
        if !matches!(claim.body, Body::Full(..)) {
            return Err(format!("#{} is not in full", claim.number));
        }
        check_body(expected, &claim.body)?;
        numbers.push(claim.number);
    }
    check_bases(capsule, &claims, &parsed, false)?;
    Ok(numbers)
}
