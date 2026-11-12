//! Shared harness of the P4-H2 acceptance tests (ADR 0014, "Acceptance
//! scenarios (Owner, original text)" 1 to 8; scenario 9 is
//! `p4g_scenario9_e2e.rs`), on top of `p4b_support`: the `SessionStart`
//! hook as Claude Code runs it, the agent's CLI as one client session, and
//! readers of the Context Pack's lines. Token secrets are never printed.

#![allow(dead_code)] // Each scenario file uses part of the harness.

use std::{
    fs,
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

use super::p4b_support::{Cli, Run, Setup, cli_binary, isolated, text};

/// The Pack's default budget, in UTF-16 code units (ADR 0014 E1 a).
pub const BUDGET: usize = 9_000;
/// The most UTF-16 code units the hook may print (ADR 0014 E1 a, Owner
/// condition n).
pub const HOOK_LIMIT: usize = 10_000;
/// The command the Pack names for the full lines of given numbers.
pub const FULL_TEXT_COMMAND: &str = "memprove resume --pack --claims <numbers>";

/// The UTF-16 code units of `text`.
pub fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The agent's CLI as client session `client` (none: the directory's own
/// context), with the agent's data directory and token file.
pub fn as_client(cli: &Cli, client: Option<&str>, words: &[&str]) -> Run {
    let mut command = isolated(cli_binary());
    command
        .current_dir(&cli.cwd)
        .arg("--data-dir")
        .arg(&cli.data_dir)
        .arg("--token-file")
        .arg(&cli.token)
        .args(words)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null());
    if let Some(client) = client {
        command.env("MEMPROVE_CLIENT_SESSION", client);
    }
    let output = command.output().unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "[{}{}] $ memprove {}\n{}{}[exit {:?}]",
        cli.label,
        client.map_or_else(String::new, |client| format!(" as {client}")),
        words.join(" "),
        run.stdout,
        run.stderr,
        run.code
    );
    run
}

/// [`as_client`] that must exit 0; its standard output as JSON.
pub fn ok_as(cli: &Cli, client: Option<&str>, words: &[&str]) -> Value {
    let run = as_client(cli, client, words);
    assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
    serde_json::from_str(run.stdout.trim()).unwrap()
}

/// [`as_client`] that must exit 0; its standard output as is.
pub fn bytes_as(cli: &Cli, client: Option<&str>, words: &[&str]) -> String {
    let run = as_client(cli, client, words);
    assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
    run.stdout
}

/// Claude Code's `SessionStart` input for a client session that starts in
/// `cwd` from `source`: only its own fields, no prompt of anyone's.
pub fn session_start_input(client: &str, cwd: &Path, source: &str) -> Vec<u8> {
    json!({
        "session_id": client,
        "transcript_path": "/nowhere/transcript.jsonl",
        "cwd": cwd.to_str().unwrap(),
        "hook_event_name": "SessionStart",
        "source": source,
    })
    .to_string()
    .into_bytes()
}

/// The file the hook writes its `export` line to (`CLAUDE_ENV_FILE`).
pub fn env_file(setup: &Setup) -> std::path::PathBuf {
    setup.root_path.join("claude-env.sh")
}

/// `memprove hook session-start` in the repository, as Claude Code runs
/// it at the start of client session `client`: `input` on standard input,
/// the agent's configuration, data directory, and token file in the
/// environment, `CLAUDE_ENV_FILE` set, no `MEMPROVE_CLIENT_SESSION`.
pub fn session_start(setup: &Setup, client: &str, source: &str) -> Run {
    let cli = &setup.agent;
    let mut child = isolated(cli_binary())
        .args(["hook", "session-start"])
        .current_dir(&setup.repo)
        .env("MEMPROVE_CONFIG_DIR", &cli.config_dir)
        .env("MEMPROVE_DATA_DIR", &cli.data_dir)
        .env("MEMPROVE_TOKEN_FILE", &cli.token)
        .env("CLAUDE_ENV_FILE", env_file(setup))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&session_start_input(client, &setup.repo, source))
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
    };
    println!(
        "$ memprove hook session-start  (client session {client}, source {source})\n{}{}[exit {:?}]",
        run.stdout, run.stderr, run.code
    );
    for token in [&setup.agent.token, &setup.operator.token] {
        let secret = fs::read_to_string(token).unwrap();
        let secret = secret.trim();
        assert!(!run.stdout.contains(secret), "a token was printed");
        assert!(!run.stderr.contains(secret), "a token was printed");
    }
    run
}

/// The hook's statement (its first line) and the Pack in its fence. U11
/// T2 (Owner O4): the statement ends with `The current UTC time is <time>.`, the hook's
/// clock, which is checked here (whole seconds, `YYYY-MM-DDTHH:MM:SSZ`) and
/// taken off, so the callers compare the statement of before.
pub fn statement_and_pack(stdout: &str) -> (String, String) {
    let (statement, rest) = stdout
        .split_once('\n')
        .unwrap_or_else(|| panic!("one line only:\n{stdout}"));
    assert!(!statement.starts_with('{'), "{statement}");
    assert!(!statement.contains("```"), "{statement}");
    let pack = rest
        .strip_prefix("```text\n")
        .and_then(|rest| rest.strip_suffix("```\n"))
        .unwrap_or_else(|| panic!("no fenced Pack:\n{stdout}"));
    (without_time(statement), pack.to_owned())
}

/// U14 T1 (Owner O2): the sentence the hook's opening ends with, after the
/// time sentence (and the identity sentence, which these tests do not set).
pub const OPEN_ITEM_SENTENCE: &str = "Under the unattended takeover rule, this session records each \
                                      Session it leaves open as an open item 「another Session \
                                      (Session k) may still be working, not ended」 in its first \
                                      checkpoint.";

/// U11 T2 (Owner O4): `statement` without the time sentence it ends with,
/// `The current UTC time is <time>.`, checked to be there once and in the form of the
/// ledger's times with whole seconds. U14 T1 (Owner O2): the open-item
/// sentence after it is checked to be there once and taken off first.
pub fn without_time(statement: &str) -> String {
    assert_eq!(
        statement.matches(OPEN_ITEM_SENTENCE).count(),
        1,
        "{statement}"
    );
    let statement = statement
        .strip_suffix(OPEN_ITEM_SENTENCE)
        .unwrap_or_else(|| panic!("the statement does not end with the open item: {statement}"));
    let body = statement
        .strip_suffix(". ")
        .unwrap_or_else(|| panic!("the statement does not end with a period: {statement}"));
    let (before, time) = body
        .rsplit_once("The current UTC time is ")
        .unwrap_or_else(|| panic!("no time sentence: {statement}"));
    assert_eq!(time.len(), "2026-11-03T18:40:00Z".len(), "{statement}");
    assert!(
        time.bytes().enumerate().all(|(at, byte)| match at {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            19 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        }),
        "{statement}"
    );
    assert_eq!(
        statement.matches("The current UTC time is ").count(),
        1,
        "{statement}"
    );
    before.trim_end().to_owned()
}

/// The ledger's high-water mark of the Project, read by the operator.
pub fn high_water(setup: &Setup) -> u64 {
    setup.operator.ok(&[
        "event",
        "list",
        "--project",
        &setup.project_id,
        "--limit",
        "1",
    ])["snapshot_sequence"]
        .as_u64()
        .unwrap()
}

/// The lines of `pack` that start with `prefix`.
pub fn lines_with<'a>(pack: &'a str, prefix: &str) -> Vec<&'a str> {
    pack.lines()
        .filter(|line| line.starts_with(prefix))
        .collect()
}

/// The one line of `pack` that starts with `prefix`.
pub fn line_with<'a>(pack: &'a str, prefix: &str) -> &'a str {
    let lines = lines_with(pack, prefix);
    assert_eq!(lines.len(), 1, "{prefix}: {lines:?}\n{pack}");
    lines[0]
}

/// The line of claim `number` in `pack`, when it has one.
pub fn claim_line(pack: &str, number: u64) -> Option<&str> {
    let prefix = format!("#{number} ");
    pack.lines().find(|line| line.starts_with(&prefix))
}

/// The word the Pack gives a verdict (`Verdict` of the domain).
pub fn verdict_word(verdict: &str) -> &'static str {
    match verdict {
        "verified" => "verified",
        "stale" => "stale",
        "partial" => "unconfirmed",
        "tampered" => "tampered",
        other => panic!("unknown verdict {other}"),
    }
}

/// The label a fact line starts with after its number: `verified`, or
/// `fact <verdict word>`.
pub fn fact_label(verdict: &str) -> String {
    if verdict == "verified" {
        "verified".to_owned()
    } else {
        format!("fact {}", verdict_word(verdict))
    }
}

/// The word the Pack gives a kind of claim.
pub fn kind_word(kind: &str) -> &'static str {
    match kind {
        "fact" => "fact",
        "open_item" => "open item",
        "decision" => "decision",
        "next_action" => "next action",
        other => panic!("unknown kind {other}"),
    }
}

/// The reason codes of `value` (a claim, an action, or the Capsule),
/// joined as the Pack joins them.
pub fn codes(value: &Value) -> String {
    value["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|reason| reason.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The claims of `capsule` that are `standing`.
pub fn standing<'a>(capsule: &'a Value, standing: &str) -> Vec<&'a Value> {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|claim| claim["standing"] == standing)
        .collect()
}

/// The number of the claim of `capsule` whose text is `text`.
pub fn number_of(capsule: &Value, text: &str) -> u64 {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["text"] == text)
        .unwrap_or_else(|| panic!("no claim 「{text}」"))["number"]
        .as_u64()
        .unwrap()
}

/// What the `Omitted: ` line of a Pack says: the level, then the counts of
/// base lines counted, bodies shortened, bodies omitted, facts folded, and
/// claims not listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Omission {
    pub level: u64,
    pub bases_counted: u64,
    pub shortened: u64,
    pub omitted: u64,
    pub folded: u64,
    pub unlisted: u64,
}

/// Reads the `Omitted: ` line of `pack`; it names the command for the full
/// lines.
pub fn omission(pack: &str) -> Omission {
    let line = line_with(pack, "Omitted: ");
    assert!(
        line.ends_with(&format!(
            "full text of given numbers: {FULL_TEXT_COMMAND} (such as 3,5-7)"
        )),
        "{line}"
    );
    let numbers = line
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    // level L; bases B, shortened S, omitted O, folded F, not listed U; … 3,5-7
    Omission {
        level: numbers[0],
        bases_counted: numbers[1],
        shortened: numbers[2],
        omitted: numbers[3],
        folded: numbers[4],
        unlisted: numbers[5],
    }
}

/// The ledger verifies for the operator.
pub fn ledger_holds(setup: &Setup) {
    let verified = setup
        .operator
        .ok(&["ledger", "verify", "--project", &setup.project_id]);
    assert_eq!(verified["valid"], true, "{verified}");
}

/// Whether `git status --porcelain` of the repository is empty.
pub fn clean(repo: &Path) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    text(&output.stdout).trim().is_empty()
}

/// The verdicts in the order the Pack counts them (`Verdict::ALL`).
pub const VERDICTS: [&str; 4] = ["verified", "stale", "partial", "tampered"];

/// Claim numbers as the Pack writes them: runs of consecutive numbers as
/// `#a–#b`, joined by `, `.
pub fn ranges(numbers: &[u64]) -> String {
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

/// A commit as a claim line names it: its first 12 hexadecimal digits.
pub fn short(commit: &str) -> &str {
    &commit[..12]
}

/// The Pack's number of the Session `session_id` of `capsule` (by start,
/// from 1).
pub fn session_number(capsule: &Value, session_id: &Value) -> usize {
    capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .position(|session| session["session_id"] == *session_id)
        .unwrap_or_else(|| panic!("no Session {session_id}"))
        + 1
}

/// The claim `number` of `capsule`.
pub fn claim(capsule: &Value, number: u64) -> &Value {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["number"] == number)
        .unwrap_or_else(|| panic!("no claim #{number}"))
}

/// The numbers of the active claims of `capsule` of `kind`.
pub fn active_numbers(capsule: &Value, kind: &str) -> Vec<u64> {
    standing(capsule, "active")
        .iter()
        .filter(|claim| claim["kind"] == kind)
        .map(|claim| claim["number"].as_u64().unwrap())
        .collect()
}

/// The `Session ` line the Pack gives Session `number` of `capsule` (ADR 0014
/// E6, P4-H1): how it ended or that it is still open, and the Pack it
/// declared it continues from, by the first 12 digits of its digest.
pub fn session_line(capsule: &Value, number: usize) -> String {
    let session = &capsule["sessions"][number - 1];
    let id = session["session_id"].as_str().unwrap();
    let started = session["started_at"].as_str().unwrap();
    let state = match session["end_state"].as_str().unwrap() {
        "ended" => format!("ended normally (started {started})"),
        "crashed" => format!("ended abandoned (started {started})"),
        _ => format!(
            "not ended (started {started}, {})",
            session["last_recorded_at"].as_str().map_or_else(
                || "nothing of it in the ledger yet".to_owned(),
                |at| format!("last wrote to the ledger at {at}")
            )
        ),
    };
    let from = &session["resumed_from"];
    let resumed = if from.is_null() {
        String::new()
    } else {
        format!(
            "; continues from the Pack at ledger sequence {} (self-reported, digest {})",
            from["project_sequence"],
            &from["pack_digest"].as_str().unwrap()[.."sha256:".len() + 12]
        )
    };
    format!("Session {number} {id} {state}{resumed}")
}

/// The `Sessions: ` line and the `Session ` lines of `pack`.
pub fn session_part(pack: &str) -> Vec<&str> {
    pack.lines()
        .filter(|line| line.starts_with("Sessions: ") || line.starts_with("Session "))
        .collect()
}

/// What `line` says outside `「」` (ADR 0014 E2): every quoted part, in
/// which `「`, `」`, and the backslash take a backslash, left out. A quote
/// that does not close fails the test.
pub fn outside_quotes(line: &str) -> String {
    let mut outside = String::new();
    let mut inside = false;
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        if inside {
            match character {
                '\\' => {
                    characters.next();
                }
                '」' => inside = false,
                _ => {}
            }
        } else if character == '「' {
            inside = true;
        } else {
            outside.push(character);
        }
    }
    assert!(!inside, "a quote does not close: {line}");
    outside
}

/// The budget the first line of `pack` names.
pub fn budget_of(pack: &str) -> usize {
    let first = pack.lines().next().unwrap();
    first
        .rsplit_once("; budget ")
        .and_then(|(_, rest)| rest.strip_suffix(" UTF-16 code units"))
        .unwrap_or_else(|| panic!("no budget on {first}"))
        .parse()
        .unwrap()
}

/// The ledger sequence the first line of `pack` names (`ledger sequence N`).
pub fn sequence_of(pack: &str) -> u64 {
    let first = pack.lines().next().unwrap();
    first
        .split_once("; ledger sequence ")
        .and_then(|(_, rest)| rest.split(';').next())
        .unwrap_or_else(|| panic!("no ledger sequence on {first}"))
        .parse()
        .unwrap()
}

/// The smallest budget at which `cli`'s `resume --pack` fits: what it names
/// when the budget of 1000 is too small, else 1000.
pub fn smallest_budget(cli: &Cli) -> usize {
    let run = cli.run(&["resume", "--pack", "--budget", "1000"], true);
    if run.code == Some(0) {
        return 1000;
    }
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    run.stderr
        .split_once("needs ")
        .and_then(|(_, rest)| rest.split(' ').next())
        .unwrap_or_else(|| panic!("no length in {}", run.stderr))
        .parse()
        .unwrap()
}

/// The claim numbers `text` names, `#a–#b` ranges expanded, in order.
pub fn numbers_in(text: &str) -> Vec<u64> {
    let mut numbers = Vec::new();
    let mut rest = text;
    let mut range_from = None;
    while let Some(at) = rest.find('#') {
        let after = &rest[at + 1..];
        let digits = after
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        rest = &after[digits.len()..];
        let Ok(number) = digits.parse::<u64>() else {
            continue;
        };
        if let Some(first) = range_from.take() {
            numbers.extend(first..=number);
        } else if rest.starts_with('–') {
            range_from = Some(number);
        } else {
            numbers.push(number);
        }
    }
    numbers
}

/// The Pack's last line, `Digest: sha256:<hex>`, is the SHA-256 of every byte
/// before it; returns `sha256:<hex>`.
pub fn pack_digest(pack: &str) -> String {
    let body = pack.strip_suffix('\n').unwrap();
    let (before, last) = body.rsplit_once('\n').unwrap();
    let digest = last
        .strip_prefix("Digest: ")
        .unwrap_or_else(|| panic!("the last line is not the digest: {last}"));
    let computed =
        memprove_domain::Sha256Digest::of_bytes(format!("{before}\n").as_bytes()).to_string();
    assert_eq!(digest, format!("sha256:{computed}"), "the Pack's digest");
    digest.to_owned()
}
