//! P3-D commit 2 (ADR 0013 P3-7): the acceptance scenario through the real
//! `memprove` CLI and the real `memproved` over a temporary Git
//! repository. The agents hold tokens with only `mcp-read` and `mcp-write`;
//! no agent command line but the first `begin --project` carries a UUID
//! (asserted).
//!
//! 1. Session 1 at X1: F1 on a passed test run, Decision D1, open item O1;
//!    ended normally.
//! 2. Session 2 at X2: O1 done; F2 cites two runs of one state (labels r1,
//!    r2) and supersedes O1; a failed test recorded as open item O2; not
//!    ended.
//! 3. The code moves to X3, which also adds files named "verified.txt" and
//!    "#1 verified:x" (T13 S-1). A fresh agent runs `resume` and `resume
//!    --text`: F1 and F2 stale with their commits, HEAD X3, and the changed
//!    files, each in 「」;
//!    each rerun command once; D1 in force; O1 superseded by F2; O2 open;
//!    Session 2 not ended; every item with its number, verdict, and Session;
//!    the same bytes and `capsule_digest` twice; no "verified" outside 「」
//!    for an item that is not verified.
//! 4. The fresh agent ends Session 2 with `end --abandoned`, begins, reruns
//!    the listed commands, and reaffirms both facts: the Task is verified,
//!    and Session 2 reads as ended abandoned.
//! 5. P3-D2: `--reaffirm` of a superseded fact and `--withdraw` of a fact
//!    without `--withdraw-reason` send nothing that is recorded; with a
//!    reason the fact is withdrawn, the first line counts it, and its line
//!    shows the reason.
//!
//! On every `resume --text` and `recover preview --text`, a line that shows
//! "verified" outside 「」 belongs to an item whose verdict is `verified`.
//!
//! Run with `--nocapture` to print the transcript and the `resume --text`
//! the fresh agent sees; token secrets are never printed.

use memprove_domain::says_verified;
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(15);
const PASSING: &str = "#!/bin/sh
# Three checks over the fixture repository.
set -e
test -f README && echo 'test readme_exists ... ok'
grep -q fixture README && echo 'test readme_content ... ok'
test \"$(git rev-parse --is-inside-work-tree)\" = true && echo 'test inside_worktree ... ok'
echo 'test result: ok. 3 passed; 0 failed'
";
const FAILING: &str = "#!/bin/sh
# The release notes check fails until they are written.
echo 'test readme_exists ... ok'
echo 'test readme_content ... ok'
echo 'test release_notes ... FAILED'
echo 'test result: FAILED. 2 passed; 1 failed'
exit 1
";
const LINT: &str = "#!/bin/sh
# Two style checks over the fixture repository.
set -e
grep -q fixture README && echo 'test readme_names_the_fixture ... ok'
! grep -q '	' README && echo 'test readme_has_no_tabs ... ok'
echo 'test result: ok. 2 passed; 0 failed'
";

fn cli_binary() -> PathBuf {
    let daemon = PathBuf::from(env!("CARGO_BIN_EXE_memproved"));
    let cli = daemon.with_file_name("memprove");
    assert!(
        cli.exists(),
        "memprove not found next to {}; run `cargo test --workspace`",
        daemon.display()
    );
    cli
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn step(title: &str) {
    println!("\n===== {title} =====");
}

fn isolated(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("MEMPROVE_") {
            command.env_remove(name);
        }
    }
    command
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    println!("$ git {}", arguments.join(" "));
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    text(&output.stdout)
}

fn commit(directory: &Path, message: &str) -> String {
    git(
        directory,
        &[
            "-c",
            "user.name=memprove Test",
            "-c",
            "user.email=memprove@example.invalid",
            "commit",
            "--quiet",
            "-am",
            message,
        ],
    );
    git(directory, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn write(path: &Path, contents: &str) {
    println!("$ printf {contents:?} > {}", path.display());
    fs::write(path, contents).unwrap();
}

/// Whether `value` holds a UUID-shaped run of characters.
fn has_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(36).any(|window| {
        window.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    })
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Clone)]
struct Cli {
    data_dir: PathBuf,
    config_dir: PathBuf,
    token: PathBuf,
    cwd: PathBuf,
    label: &'static str,
    /// Agent steps assert that their command line carries no UUID.
    no_uuid: bool,
}

impl Cli {
    fn run(&self, words: &[&str], quiet: bool) -> Run {
        let output = isolated(cli_binary())
            .current_dir(&self.cwd)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token-file")
            .arg(&self.token)
            .args(words)
            .env("MEMPROVE_CONFIG_DIR", &self.config_dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let run = Run {
            code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
        };
        if !quiet {
            let shown = words
                .iter()
                .map(|word| {
                    if word.contains(' ') || word.starts_with('#') {
                        format!("{word:?}")
                    } else {
                        (*word).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            if self.no_uuid {
                assert!(
                    !has_uuid(&shown),
                    "an agent command line carries a UUID: {shown}"
                );
            }
            println!(
                "[{}] $ ct {shown}\n{}{}[exit {}]",
                self.label,
                run.stdout,
                run.stderr,
                run.code
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string())
            );
        }
        run
    }

    fn ok(&self, words: &[&str]) -> Value {
        let run = self.run(words, false);
        assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
        serde_json::from_str(run.stdout.trim()).unwrap()
    }

    fn resume_text(&self) -> String {
        let run = self.run(&["resume", "--text", "--all"], false);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        run.stdout
    }

    /// P4-A: `resume --text` without `--all`, superseded and withdrawn
    /// claims folded into one line.
    fn resume_folded(&self) -> String {
        let run = self.run(&["resume", "--text"], false);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        run.stdout
    }

    /// P4-C: `resume --pack`, with `--budget` when given.
    fn resume_pack(&self, budget: Option<&str>) -> Run {
        let mut words = vec!["resume", "--pack"];
        words.extend(budget.iter().flat_map(|budget| ["--budget", budget]));
        self.run(&words, true)
    }

    fn resume(&self) -> (String, Value) {
        println!("[{}] $ ct resume   # JSON not shown", self.label);
        let run = self.run(&["resume"], true);
        assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
        (
            run.stdout.clone(),
            serde_json::from_str(run.stdout.trim()).unwrap(),
        )
    }
}

struct Daemon {
    child: Child,
}

impl Daemon {
    fn start(cli: &Cli, log: &Path) -> Self {
        let child = isolated(env!("CARGO_BIN_EXE_memproved"))
            .arg("--data-dir")
            .arg(&cli.data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(log).unwrap())
            .spawn()
            .unwrap();
        // U1 (C6): the guard exists before anything below can panic.
        let daemon = Self { child };
        println!(
            "$ memproved --data-dir <DATA> &   # pid {}",
            daemon.child.id()
        );
        let deadline = Instant::now() + TIMEOUT;
        while cli.run(&["health", "ready"], true).code != Some(0) {
            assert!(
                Instant::now() < deadline,
                "memproved never became ready:\n{}",
                fs::read_to_string(log).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
        daemon
    }

    fn signal(mut self, name: &str) {
        println!("$ kill -{name} {}", self.child.id());
        let sent = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success());
        let status = self.child.wait().unwrap();
        println!("memproved exited: {status}");
        if name == "KILL" {
            assert_eq!(status.signal(), Some(9));
        } else {
            assert_eq!(status.code(), Some(0));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn str_of<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name]
        .as_str()
        .unwrap_or_else(|| panic!("{name} is not a string in {value}"))
}

/// The lines of `resume --text` with every 「…」 quotation removed (a `」`
/// inside one is written `\」`).
fn outside_quotes(line: &str) -> String {
    let mut outside = String::new();
    let mut depth = 0_u32;
    let mut escaped = false;
    for character in line.chars() {
        if depth > 0 {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '」' {
                depth -= 1;
            }
            continue;
        }
        if character == '「' {
            depth += 1;
        } else {
            outside.push(character);
        }
    }
    outside
}

/// P4-C: the line prefixes of ADR 0014 E2 as P4-C fixed them, with P4-C2's
/// `Base: ` and `Not listed: `.
const PACK_PREFIXES: [&str; 19] = [
    "Pack: ",
    "Ontology: ",
    "Task「",
    "Integrity: ",
    "Sessions: ",
    "Session ",
    // U5 P1 (Owner O3): the last passed test run, when no rerun is due.
    "Last passed: ",
    "Base: ",
    "#",
    "Rerun: ",
    "No rerun needed: ",
    "Folded: ",
    "Not listed: ",
    // ADR 0014 E3 (P4-D): one action line per action, never left out.
    "Can: ",
    "Cannot: ",
    "Superseded and withdrawn: ",
    "Gaps: ",
    "Omitted: ",
    "Digest: ",
];

fn utf16(text: &str) -> usize {
    text.encode_utf16().count()
}

/// P4-C (ADR 0014 E1, E2, scenarios 1, 2, 7, 8): the fresh agent's Context
/// Pack through the real CLI. The same ledger, probe, and budget give the
/// same bytes; the Pack is within its budget, every line starts with a
/// prefix of the closed set, "verified" outside 「」 appears only on the Task
/// line, an active verified fact, or the fold line; the last line is the
/// SHA-256 of the lines before it; a smaller budget gives another digest
/// and never a better verdict; `--text --claims` prints the full lines of
/// the numbers named. Returns the default Pack.
fn assert_pack(cli: &Cli, capsule: &Value, title: &str) -> String {
    let first = cli.resume_pack(None);
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let again = cli.resume_pack(Some("9000"));
    assert_eq!(first.stdout, again.stdout, "the default budget is 9000");
    let pack = first.stdout;
    println!(
        "\n----- P4-C resume --pack ({title}), {} UTF-16 code units, {} bytes, {} lines -----\n\
         {pack}----- end -----",
        utf16(&pack),
        pack.len(),
        pack.lines().count()
    );
    assert!(utf16(&pack) <= 9_000);
    let (body, digest) = pack.trim_end().rsplit_once('\n').unwrap();
    assert_eq!(
        digest,
        format!(
            "Digest: sha256:{}",
            memprove_domain::Sha256Digest::of_bytes(format!("{body}\n").as_bytes())
        )
    );
    // P4-D: the action section, one line per action of the Capsule, with
    // fixed words, numbers, and codes only (no 「」).
    let actions = pack
        .lines()
        .filter(|line| line.starts_with("Can: ") || line.starts_with("Cannot: "))
        .collect::<Vec<_>>();
    assert_eq!(actions.len(), 3, "{pack}");
    for (line, action) in actions.iter().zip(capsule["actions"].as_array().unwrap()) {
        assert_eq!(
            line.starts_with("Can: "),
            action["available"].as_bool().unwrap(),
            "{line}"
        );
        assert!(!line.contains('「'), "{line}");
        for reason in action["reasons"].as_array().unwrap() {
            assert!(line.contains(reason.as_str().unwrap()), "{line}");
        }
    }
    let header = pack.lines().next().unwrap();
    assert!(
        header.contains(str_of(capsule, "capsule_digest")),
        "{header}"
    );
    assert!(
        header.contains(&format!(
            "ledger sequence {}; ",
            capsule["bindings"]["ledger"]["project_sequence"]
        )),
        "{header}"
    );
    let verified = |number: &str| {
        number.parse::<u64>().is_ok_and(|number| {
            let claim = claim(capsule, number);
            str_of(claim, "kind") == "fact"
                && str_of(claim, "standing") == "active"
                && str_of(claim, "verdict") == "verified"
        })
    };
    for line in pack.lines() {
        assert!(
            PACK_PREFIXES.iter().any(|prefix| line.starts_with(prefix)),
            "{line}"
        );
        let outside = outside_quotes(line);
        if says_verified(&outside) {
            let number = outside
                .strip_prefix('#')
                .and_then(|rest| rest.split_once(" verified: "))
                .map(|(number, _)| number);
            assert!(
                capsule["status"] != "tampered"
                    && (line.starts_with("Task「")
                        || line.starts_with("Folded: ")
                        || number.is_some_and(verified)),
                "{line}"
            );
        }
    }
    assert_small_pack(cli, &pack, title);
    assert_claims_option(cli);
    pack
}

/// P4-C: a budget of 1000 (or the length the CLI says level 5 needs) keeps
/// the Task line as it is and gives another digest.
fn assert_small_pack(cli: &Cli, pack: &str, title: &str) {
    let task_word = pack.lines().nth(2).unwrap();
    let small = cli.resume_pack(Some("1000"));
    let small = if small.code == Some(1) {
        let needed = small
            .stderr
            .split("needs ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .unwrap()
            .to_owned();
        let retry = cli.resume_pack(Some(&needed));
        assert_eq!(retry.code, Some(0), "{}", retry.stderr);
        assert!(utf16(&retry.stdout) <= needed.parse().unwrap());
        retry.stdout
    } else {
        assert_eq!(small.code, Some(0), "{}", small.stderr);
        assert!(utf16(&small.stdout) <= 1_000);
        small.stdout
    };
    assert_eq!(
        small.lines().nth(2),
        Some(task_word),
        "the Task line is never cut"
    );
    assert_ne!(small.lines().last(), pack.lines().last());
    println!(
        "\n----- P4-C resume --pack with a small budget ({title}), {} UTF-16 code units -----\n\
         {small}----- end -----",
        utf16(&small)
    );
}

/// P4-C: `--text --claims` prints the `--text --all` lines of the numbers
/// named; a number the Capsule does not list exits 2.
fn assert_claims_option(cli: &Cli) {
    let all = cli.resume_text();
    let claims = cli.run(&["resume", "--text", "--claims", "1-2"], false);
    assert_eq!(claims.code, Some(0), "{}", claims.stderr);
    let mut expected = String::new();
    for line in all
        .lines()
        .filter(|line| line.starts_with("#1 ") || line.starts_with("#2 "))
    {
        expected.push_str(line);
        expected.push('\n');
    }
    assert_eq!(claims.stdout, expected);
    let missing = cli.run(&["resume", "--text", "--claims", "999"], false);
    assert_eq!(missing.code, Some(2), "{}", missing.stderr);
    assert!(missing.stderr.contains("names #999"), "{}", missing.stderr);
    // P4-C2 revision 5: `--pack --claims` prints the full Pack lines of the
    // numbers named, after the base lines they name.
    let pack = cli.run(&["resume", "--pack", "--claims", "1-2"], false);
    assert_eq!(pack.code, Some(0), "{}", pack.stderr);
    let numbered = pack
        .stdout
        .lines()
        .filter(|line| !line.starts_with("Base: "))
        .collect::<Vec<_>>();
    assert_eq!(numbered.len(), 2, "{}", pack.stdout);
    assert!(
        numbered
            .iter()
            .all(|line| line.starts_with("#1 ") || line.starts_with("#2 ")),
        "{}",
        pack.stdout
    );
    let missing = cli.run(&["resume", "--pack", "--claims", "999"], false);
    assert_eq!(missing.code, Some(2), "{}", missing.stderr);
    assert!(missing.stderr.contains("names #999"), "{}", missing.stderr);
    let budget = cli.run(
        &["resume", "--pack", "--claims", "1", "--budget", "2000"],
        false,
    );
    assert_eq!(budget.code, Some(2), "{}", budget.stderr);
}

/// "verified" outside 「」 only on lines whose continuation item is
/// `verified`; the lines are the continuation items in order, then gaps.
/// P3-D2: the lines are exactly the items (and one line of gaps), so the
/// line-to-item mapping holds.
fn assert_verified_only_where_verified(text: &str, capsule: &Value) {
    let items = capsule["continuation"].as_array().unwrap();
    let gaps = usize::from(!capsule["gaps"].as_array().unwrap().is_empty());
    // P3-L L7: one Session summary line, after the Task line.
    let summary = text
        .lines()
        .filter(|line| line.starts_with("Sessions: "))
        .count();
    assert_eq!(summary, 1, "{text}");
    assert_eq!(text.lines().count(), items.len() + gaps + 1, "{text}");
    let lines = text
        .lines()
        .filter(|line| !line.starts_with("Sessions: "))
        .collect::<Vec<_>>();
    for (index, line) in lines.into_iter().enumerate() {
        if says_verified(&outside_quotes(line)) {
            assert_eq!(
                items.get(index).map(|item| item["verdict"].clone()),
                Some(Value::String("verified".to_owned())),
                "{line}"
            );
            // P3-L L6 (Owner O5): and only on the header or a fact; P3-L3
            // (T16 N2): a fact that stands, not a superseded or withdrawn one.
            let item = &items[index];
            let kind = capsule["claims"]
                .as_array()
                .unwrap()
                .iter()
                .find(|claim| claim["claim_id"] == item["object_id"])
                .map(|claim| claim["kind"].as_str().unwrap());
            assert!(
                item["template"] == "header"
                    || (item["template"] == "fact" && kind == Some("fact")),
                "{line}"
            );
        }
    }
}

/// `recover preview --text`: "verified" outside 「」 only when the Preview
/// itself is `verified`, and then only on its first line.
fn assert_preview_text_verified_only_if_verified(cli: &Cli) {
    let preview = cli.run(&["recover", "preview"], true);
    assert_eq!(preview.code, Some(0), "{}", preview.stderr);
    let preview: Value = serde_json::from_str(preview.stdout.trim()).unwrap();
    let summary = cli.run(&["recover", "preview", "--text"], false);
    assert_eq!(summary.code, Some(0), "{}", summary.stderr);
    for (index, line) in summary.stdout.lines().enumerate() {
        assert_eq!(
            says_verified(&outside_quotes(line)),
            index == 0 && preview["status"] == "verified",
            "{line}"
        );
    }
}

/// `#1–#3, #5`: ascending numbers, consecutive ones as a range.
fn ranges(numbers: &[u64]) -> String {
    let mut parts = Vec::new();
    let mut index = 0;
    while index < numbers.len() {
        let mut last = index;
        while last + 1 < numbers.len() && numbers[last + 1] == numbers[last] + 1 {
            last += 1;
        }
        parts.push(if last == index {
            format!("#{}", numbers[index])
        } else {
            format!("#{}–#{}", numbers[index], numbers[last])
        });
        index = last + 1;
    }
    parts.join(", ")
}

/// P4-A: the default `resume --text` (`folded`) is the `--all` text
/// (`all`) without the line of each superseded or withdrawn claim, plus one
/// line, after the other statements and before the gaps, that counts both
/// kinds and names their numbers as the JSON's `standing` does; none when
/// there are none. "verified" outside 「」 stays on the Task line and the
/// verified facts that stand.
fn assert_folded(folded: &str, all: &str, capsule: &Value) {
    let items = capsule["continuation"].as_array().unwrap();
    let gaps = !capsule["gaps"].as_array().unwrap().is_empty();
    let numbers = |standing: &str| {
        let mut numbers = capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|claim| claim["standing"] == standing)
            .map(|claim| claim["number"].as_u64().unwrap())
            .collect::<Vec<_>>();
        numbers.sort_unstable();
        numbers
    };
    let part = |label: &str, numbers: &[u64]| {
        if numbers.is_empty() {
            format!("{label} 0")
        } else {
            format!("{label} {}: {}", numbers.len(), ranges(numbers))
        }
    };
    let (superseded, withdrawn) = (numbers("superseded"), numbers("withdrawn"));
    // The lines of `--all` are the items in order (the Session summary
    // aside), then the gaps.
    let mut kept = Vec::new();
    let mut item = 0;
    let mut gaps_line = None;
    for line in all.lines() {
        if line.starts_with("Sessions: ") {
            kept.push(line.to_owned());
            continue;
        }
        match items.get(item) {
            Some(it) => {
                if it["template"] != "superseded" && it["template"] != "withdrawn" {
                    kept.push(line.to_owned());
                }
                item += 1;
            }
            None => gaps_line = Some(line.to_owned()),
        }
    }
    assert_eq!(item, items.len(), "{all}");
    assert_eq!(gaps_line.is_some(), gaps, "{all}");
    if !superseded.is_empty() || !withdrawn.is_empty() {
        kept.push(format!(
            "{}; {} (not listed one by one; resume --text --all lists them all)",
            part("superseded", &superseded),
            part("withdrawn", &withdrawn)
        ));
    }
    kept.extend(gaps_line);
    assert_eq!(folded.lines().collect::<Vec<_>>(), kept, "{folded}");
    for line in folded.lines() {
        if says_verified(&outside_quotes(line)) {
            let verified_fact = line.split_once(' ').is_some_and(|(number, rest)| {
                rest.starts_with("verified: ")
                    && number
                        .strip_prefix('#')
                        .and_then(|number| number.parse::<u64>().ok())
                        .is_some_and(|number| {
                            let claim = claim(capsule, number);
                            claim["kind"] == "fact"
                                && claim["standing"] == "active"
                                && claim["verdict"] == "verified"
                        })
            });
            assert!(
                (line.starts_with("Task「") && capsule["status"] == "verified") || verified_fact,
                "{line}"
            );
        }
    }
}

fn claim(capsule: &Value, number: u64) -> &Value {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["number"] == number)
        .unwrap()
}

/// `(kind, standing, verdict, superseded_by numbers)` of each claim.
fn claims(capsule: &Value) -> Vec<(u64, String, String, String, Vec<u64>)> {
    capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|claim| {
            (
                claim["number"].as_u64().unwrap(),
                str_of(claim, "kind").to_owned(),
                str_of(claim, "standing").to_owned(),
                str_of(claim, "verdict").to_owned(),
                claim["superseded_by"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|by| by["number"].as_u64().unwrap())
                    .collect(),
            )
        })
        .collect()
}

fn issue(operator: &Cli, label: &str, path: &Path) {
    let created = operator.run(
        &[
            "token",
            "create",
            "--label",
            label,
            "--scope",
            "mcp-read",
            "--scope",
            "mcp-write",
        ],
        true,
    );
    assert_eq!(created.code, Some(0), "{}", created.stderr);
    let created: Value = serde_json::from_str(created.stdout.trim()).unwrap();
    let mut shown = created.clone();
    shown["secret"] = Value::String("<redacted>".to_owned());
    println!(
        "[operator/control] $ ct token create --label {label} --scope mcp-read --scope mcp-write\n{shown}\n[exit 0]"
    );
    fs::write(path, str_of(&created, "secret")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)] // The acceptance scenario, in order.
fn acceptance_scenario_through_real_binaries() {
    let root = tempfile::tempdir().unwrap();
    let root_path = fs::canonicalize(root.path()).unwrap();
    let repo = root_path.join("repo");
    step("setup: repository at X1, daemon, operator, two agent tokens");
    fs::create_dir_all(repo.join("tests")).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join("README"), b"fixture\n").unwrap();
    fs::write(repo.join("tests/run.sh"), PASSING).unwrap();
    fs::write(repo.join("tests/lint.sh"), LINT).unwrap();
    fs::write(repo.join("tests/release.sh"), FAILING).unwrap();
    git(
        &repo,
        &[
            "add",
            "README",
            "tests/run.sh",
            "tests/lint.sh",
            "tests/release.sh",
        ],
    );
    let x1 = commit(&repo, "fixture with three test scripts");
    let operator = Cli {
        data_dir: root_path.join("data"),
        config_dir: root_path.join("config"),
        token: root_path.join("control.token"),
        cwd: repo.clone(),
        label: "operator/control",
        no_uuid: false,
    };
    let issued = isolated(env!("CARGO_BIN_EXE_memproved"))
        .arg("bootstrap")
        .arg("--data-dir")
        .arg(&operator.data_dir)
        .arg("--output")
        .arg(&operator.token)
        .output()
        .unwrap();
    assert_eq!(issued.status.code(), Some(0), "{}", text(&issued.stderr));
    println!("$ memproved bootstrap --data-dir <DATA> --output <CONTROL_TOKEN_FILE>\n[exit 0]");
    let daemon = Daemon::start(&operator, &root_path.join("daemon.log"));
    let registered = operator.ok(&["project", "register", repo.to_str().unwrap()]);
    let project_id = str_of(&registered, "project_id").to_owned();
    issue(&operator, "agent", &root_path.join("agent.token"));
    issue(&operator, "takeover", &root_path.join("takeover.token"));
    let agent = Cli {
        token: root_path.join("agent.token"),
        label: "agent/mcp-read+mcp-write",
        ..operator.clone()
    };
    let tests = ["--test-format", "cargo", "--", "sh", "tests/run.sh"];
    let lint = ["--test-format", "cargo", "--", "sh", "tests/lint.sh"];
    let release = ["--test-format", "cargo", "--", "sh", "tests/release.sh"];
    let run = |cli: &Cli, words: &[&str]| {
        let run = cli.run(&[&["run"][..], words].concat(), false);
        let value: Value = serde_json::from_str(run.stdout.trim()).unwrap();
        (run.code, value)
    };

    step("1. Session 1 at X1: F1 (tests pass), D1, O1; ended normally");
    agent.ok(&[
        "begin",
        "--project",
        &project_id,
        "--new-task",
        "Ship the fixture",
    ]);
    let agent = Cli {
        no_uuid: true,
        ..agent
    };
    let (code, passed) = run(&agent, &tests);
    assert_eq!((code, str_of(&passed, "status")), (Some(0), "PASS"));
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F1 README checks pass",
        "--open",
        "O1 README lacks a second line",
        // P3-L L5: an --open cites a run only when told to.
        "--evidence",
        "last",
        "--next",
        "add the second README line",
    ]);
    agent.ok(&[
        "decide",
        "D1 keep the README plain text",
        "--rationale",
        "no renderer is assumed",
    ]);
    agent.ok(&["end", "--reason", "session 1 done"]);

    step("2. Session 2 at X2: O1 done, F2 on two runs of one state; O2 failing; not ended");
    agent.ok(&["begin"]);
    write(&repo.join("README"), "fixture\nsecond line\n");
    let x2 = commit(&repo, "README: second line");
    let (_, first) = run(&agent, &tests);
    let (_, second) = run(&agent, &lint);
    assert_eq!(str_of(&second, "status"), "PASS");
    // The second run's Snapshot seals the first (ADR 0013 D3).
    let sealed = ["command_evidence_ids", "test_evidence_ids"]
        .iter()
        .flat_map(|name| second["snapshot"][name].as_array().unwrap().clone())
        .collect::<Vec<_>>();
    assert!(sealed.contains(&first["evidence_id"]), "{second}");
    agent.ok(&[
        "checkpoint",
        "create",
        "--fact",
        "F2 README has its second line",
        "--evidence",
        "r1",
        "--evidence",
        "r2",
        "--supersedes",
        "2",
        "--next",
        "write the release notes",
    ]);
    let (code, failed) = run(&agent, &release);
    assert_eq!((code, str_of(&failed, "status")), (Some(1), "FAIL"));
    agent.ok(&[
        "checkpoint",
        "create",
        "--open",
        "O2 release notes test fails, not verified",
        "--evidence",
        "last",
        "--next",
        "write the release notes",
    ]);

    step("3. the code moves to X3 (with files named verified); a fresh agent resumes");
    write(&repo.join("README"), "fixture\nsecond line\nthird line\n");
    // T13 S-1: changed files whose names hold the verdict word.
    write(&repo.join("verified.txt"), "a file name, not a verdict\n");
    write(&repo.join("#1 verified:x"), "a file name, not a line\n");
    git(&repo, &["add", "verified.txt", "#1 verified:x"]);
    let x3 = commit(&repo, "README: third line");
    let takeover = Cli {
        token: root_path.join("takeover.token"),
        label: "takeover/mcp-read+mcp-write",
        no_uuid: true,
        ..agent.clone()
    };
    let (bytes, capsule) = takeover.resume();
    let text_1 = takeover.resume_text();
    let (bytes_again, capsule_again) = takeover.resume();
    let text_2 = takeover.resume_text();
    assert_eq!(bytes, bytes_again, "same ledger and probe, same bytes");
    assert_eq!(text_1, text_2);
    assert_eq!(capsule["capsule_digest"], capsule_again["capsule_digest"]);
    println!("\n----- resume --text as the fresh agent sees it -----\n{text_1}----- end -----");
    let pack = assert_pack(&takeover, &capsule, "the fresh agent at X3");
    assert!(pack.contains("Rerun: 「sh tests/run.sh」"), "{pack}");
    // P4-A: the default folds; the same ledger and probe give the same bytes.
    let folded_1 = takeover.resume_folded();
    assert_eq!(takeover.resume_folded(), folded_1);
    assert_folded(&folded_1, &text_1, &capsule);
    println!(
        "\n----- P4-A resume --text (default), {} bytes, {} lines -----\n{folded_1}----- end -----",
        folded_1.len(),
        folded_1.lines().count()
    );
    println!(
        "\n----- P4-A resume --text --all, {} bytes, {} lines -----\n{text_1}----- end -----",
        text_1.len(),
        text_1.lines().count()
    );
    let alone = takeover.run(&["resume", "--all"], false);
    assert_eq!(alone.code, Some(2), "{}", alone.stderr);
    assert!(alone.stdout.is_empty());

    let s = |value: &str| value.to_owned();
    assert_eq!(
        claims(&capsule),
        [
            (1, s("fact"), s("active"), s("stale"), vec![]),
            (2, s("open_item"), s("superseded"), s("stale"), vec![5]),
            (3, s("next_action"), s("superseded"), s("partial"), vec![6]),
            (4, s("decision"), s("active"), s("partial"), vec![]),
            (5, s("fact"), s("active"), s("stale"), vec![]),
            (6, s("next_action"), s("superseded"), s("partial"), vec![8]),
            (7, s("open_item"), s("active"), s("stale"), vec![]),
            (8, s("next_action"), s("active"), s("partial"), vec![]),
        ]
    );
    assert_eq!(capsule["status"], "stale");
    assert_eq!(capsule["bindings"]["probe"]["head_oid"], x3.as_str());
    let sessions = capsule["sessions"].as_array().unwrap();
    assert_eq!(
        sessions
            .iter()
            .map(|session| str_of(session, "end_state"))
            .collect::<Vec<_>>(),
        ["ended", "open"]
    );
    for claim in capsule["claims"].as_array().unwrap() {
        let number = claim["number"].as_u64().unwrap();
        let expected = if number <= 4 {
            &sessions[0]
        } else {
            &sessions[1]
        };
        assert_eq!(claim["session_id"], expected["session_id"], "#{number}");
        assert!(claim["verdict"].is_string(), "#{number}");
    }
    for item in claim(&capsule, 1)["evidence"].as_array().unwrap() {
        assert_eq!(item["base_commit"], x1.as_str());
    }
    let f2_evidence = claim(&capsule, 5)["evidence"].as_array().unwrap();
    assert_eq!(f2_evidence.len(), 2);
    for item in f2_evidence {
        assert_eq!(item["base_commit"], x2.as_str());
        assert_eq!(item["snapshot_id"], second["snapshot"]["snapshot_id"]);
    }
    let reruns = capsule["rerun"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rerun| (rerun["argv"].clone(), rerun["claims"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        reruns,
        [
            (
                serde_json::json!(["sh", "tests/run.sh"]),
                serde_json::json!([1, 5])
            ),
            (
                serde_json::json!(["sh", "tests/lint.sh"]),
                serde_json::json!([5])
            ),
        ]
    );
    for changed in capsule["changed_paths_by_base"].as_array().unwrap() {
        assert!(
            changed["paths"]
                .as_array()
                .unwrap()
                .contains(&Value::String("repo://README".to_owned())),
            "{changed}"
        );
    }
    let short = |commit: &str| commit.to_owned();
    for expected in [
        format!(
            "#1 fact stale: 「F1 README checks pass」 (evidence bound to commit {}, current HEAD {}; changed files since {}: 「#1 verified:x」, 「README」, 「verified.txt」; verdict: stale; from Session 1)",
            short(&x1),
            short(&x3),
            short(&x1)
        ),
        format!(
            "#5 fact stale: 「F2 README has its second line」 (evidence bound to commit {}, current HEAD {}; changed files since {}: 「#1 verified:x」, 「README」, 「verified.txt」; verdict: stale; from Session 2)",
            short(&x2),
            short(&x3),
            short(&x2)
        ),
        // P3-L L6: open items, next actions, and Decisions show their
        // Evidence in words, not a verdict.
        "#4 decision: 「D1 keep the README plain text」 (active, not superseded; no evidence cited; from Session 1; rationale 「no renderer is assumed」)".to_owned(),
        format!(
            "#2 superseded by #5: 「O1 README lacks a second line」 (evidence: passed, the bound commit {x1} is no longer the current state; from Session 1)"
        ),
        format!(
            "#7 open item: 「O2 release notes test fails, not verified」 (evidence: failed, the bound commit {x2} is no longer the current state; from Session 2)"
        ),
        format!(
            "Session 2 {} (the Session this directory's context file records) not ended",
            str_of(&sessions[1], "session_id")
        ),
    ] {
        assert!(text_1.contains(&expected), "{expected}\n{text_1}");
    }
    assert_eq!(text_1.matches("「sh tests/run.sh」").count(), 1, "{text_1}");
    assert_eq!(
        text_1.matches("「sh tests/lint.sh」").count(),
        1,
        "{text_1}"
    );
    // P3-L L6: only a fact line shows a verdict.
    for line in text_1.lines().filter(|line| line.starts_with('#')) {
        let number = line[1..].split(' ').next().unwrap().parse::<u64>().unwrap();
        assert_eq!(
            line.contains("verdict: "),
            str_of(claim(&capsule, number), "kind") == "fact",
            "{line}"
        );
        assert!(line.contains("from Session "), "{line}");
    }
    assert_verified_only_where_verified(&text_1, &capsule);
    // T13 S-1: the file names hold "verified", but only inside 「」.
    assert!(text_1.contains("「verified.txt」"), "{text_1}");
    assert!(
        text_1
            .lines()
            .all(|line| !says_verified(&outside_quotes(line)))
    );
    assert_preview_text_verified_only_if_verified(&takeover);

    step("4. the fresh agent ends Session 2 as abandoned, reruns, reaffirms");
    takeover.ok(&[
        "end",
        "--abandoned",
        "--reason",
        "taken over: session 2 was left unfinished",
    ]);
    takeover.ok(&["begin"]);
    let (code, _) = run(&takeover, &tests);
    assert_eq!(code, Some(0));
    let (code, _) = run(&takeover, &lint);
    assert_eq!(code, Some(0));
    let before_reaffirm = takeover.resume_text();
    assert!(
        before_reaffirm.contains("No rerun needed: 「sh tests/run.sh」"),
        "{before_reaffirm}"
    );
    takeover.ok(&[
        "checkpoint",
        "create",
        "--reaffirm",
        "1",
        "--evidence",
        "r1",
        "--reaffirm",
        "5",
        "--evidence",
        "r1",
        "--evidence",
        "r2",
        "--next",
        "write the release notes",
    ]);
    let (_, capsule) = takeover.resume();
    let final_text = takeover.resume_text();
    println!("\n----- resume --text after the takeover -----\n{final_text}----- end -----");
    assert_eq!(capsule["status"], "verified", "{capsule}");
    assert_eq!(
        capsule["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| str_of(session, "end_state"))
            .collect::<Vec<_>>(),
        ["ended", "crashed", "open"]
    );
    let crashed = str_of(&capsule["sessions"][1], "session_id");
    assert!(
        final_text.contains(&format!(
            "Session 2 {crashed} ended abandoned (session.crashed; "
        )),
        "{final_text}"
    );
    assert_eq!(
        (
            str_of(claim(&capsule, 1), "standing"),
            str_of(claim(&capsule, 5), "standing")
        ),
        ("superseded", "superseded")
    );
    for number in [9, 10] {
        assert_eq!(str_of(claim(&capsule, number), "verdict"), "verified");
        assert_eq!(str_of(claim(&capsule, number), "kind"), "fact");
    }
    assert_verified_only_where_verified(&final_text, &capsule);
    let pack = assert_pack(&takeover, &capsule, "after the takeover");
    assert!(
        pack.lines().nth(2).unwrap().contains("): verified"),
        "{pack}"
    );
    let folded_final = takeover.resume_folded();
    assert_folded(&folded_final, &final_text, &capsule);
    println!(
        "\n----- P4-A resume --text (default) after the takeover -----\n{folded_final}----- end -----"
    );
    assert!(
        final_text
            .lines()
            .any(|line| says_verified(&outside_quotes(line))),
        "{final_text}"
    );
    assert_preview_text_verified_only_if_verified(&takeover);

    step("5. P3-D2: reaffirm only an active fact; withdraw a fact only with a reason");
    let refused = takeover.run(
        &[
            "checkpoint",
            "create",
            "--reaffirm",
            "5",
            "--next",
            "write the release notes",
        ],
        false,
    );
    assert_eq!(refused.code, Some(1), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("--reaffirm #5 names a fact that is superseded"),
        "{}",
        refused.stderr
    );
    let unexplained = takeover.run(
        &[
            "checkpoint",
            "create",
            "--withdraw",
            "10",
            "--next",
            "write the release notes",
        ],
        false,
    );
    assert_eq!(unexplained.code, Some(1), "{}", unexplained.stderr);
    assert!(
        unexplained.stderr.contains("withdrawing a fact needs"),
        "{}",
        unexplained.stderr
    );
    assert_eq!(
        takeover.resume().1["claims"],
        capsule["claims"],
        "nothing was recorded"
    );
    takeover.ok(&[
        "checkpoint",
        "create",
        "--withdraw",
        "10",
        "--withdraw-reason",
        "lint is not a README check; F2 rests on #9's run",
        "--next",
        "write the release notes",
    ]);
    let (_, withdrawn) = takeover.resume();
    let withdrawn_text = takeover.resume_text();
    println!("\n----- resume --text after a withdrawal -----\n{withdrawn_text}----- end -----");
    assert_eq!(str_of(claim(&withdrawn, 10), "standing"), "withdrawn");
    assert_eq!(
        str_of(claim(&withdrawn, 10), "withdrawal_reason"),
        "lint is not a README check; F2 rests on #9's run"
    );
    assert_eq!(withdrawn["continuation"][0]["args"]["withdrawn_facts"], 1);
    let first_line = withdrawn_text.lines().next().unwrap();
    assert!(first_line.contains(", withdrawn facts 1)"), "{first_line}");
    // P3-L3 (T16 N2): a withdrawn fact shows its Evidence, not a verdict.
    let withdrawn_line = withdrawn_text
        .lines()
        .find(|line| line.starts_with("#10 withdrawn: "))
        .unwrap_or_else(|| panic!("{withdrawn_text}"));
    assert!(
        withdrawn_line.starts_with("#10 withdrawn: 「F2 README has its second line」 (evidence 1: passed, ")
            // U8 A6 (Owner O4): and the Session that withdrew it.
            && withdrawn_line.ends_with(
                "; from Session 3; withdrawal reason 「lint is not a README check; F2 rests on #9's run」) \
                 (withdrawn by Session 3)"
            )
            && !withdrawn_line.contains("verdict"),
        "{withdrawn_line}"
    );
    assert_verified_only_where_verified(&withdrawn_text, &withdrawn);
    let folded_withdrawn = takeover.resume_folded();
    assert_folded(&folded_withdrawn, &withdrawn_text, &withdrawn);
    assert!(
        !folded_withdrawn.contains("#10 withdrawn: "),
        "{folded_withdrawn}"
    );
    let events = operator.run(
        &["event", "list", "--project", &project_id, "--limit", "500"],
        true,
    );
    assert_eq!(events.code, Some(0), "{}", events.stderr);
    let events: Value = serde_json::from_str(events.stdout.trim()).unwrap();
    let crashed_events = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["event_type"] == "session.crashed")
        .collect::<Vec<_>>();
    assert_eq!(crashed_events.len(), 1);
    assert_eq!(crashed_events[0]["producer"]["name"], "continuityd");
    assert_eq!(crashed_events[0]["payload_schema"], "session.crashed@1");
    assert_eq!(crashed_events[0]["session_id"], crashed);
    println!("session.crashed payload: {}", crashed_events[0]["payload"]);
    assert_eq!(
        operator.ok(&["ledger", "verify", "--project", &project_id])["valid"],
        true
    );
    daemon.signal("TERM");
}
