//! `memprove run`: runs a command in the caller's environment and submits
//! its result as Evidence (`captureEvidence`, ADR 0012). The daemon never runs
//! the command; it only captures the repository with its read-only Git
//! probes and records the result.

use std::{
    ffi::OsString,
    fmt::Write as _,
    fs::File,
    io::{self, Read},
    os::unix::process::ExitStatusExt,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use memprove_protocol::phase1::{MAX_OUTPUT_TAIL_CHARS, OperationId};
use sha2::{Digest, Sha256};

use crate::{Request, TOKEN_ENV, json_string};

/// Collector identity recorded with every command result the CLI submits.
pub const COLLECTOR_NAME: &str = "memprove-cli";

/// What `memprove run` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    /// Session that runs the command.
    pub session_id: String,
    /// Workspace the Session belongs to.
    pub workspace_id: String,
    /// Task the Session continues.
    pub task_id: String,
    /// Present when the command is a test run.
    pub test: Option<TestSpec>,
    /// Program and arguments; never empty.
    pub argv: Vec<String>,
    /// Caller-chosen `Idempotency-Key`.
    pub idempotency_key: Option<String>,
    /// Keep only the SHA-256 of standard output and error, not their text
    /// (`--digest-only`, P2-C).
    pub digest_only: bool,
}

/// The test part of a test run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSpec {
    /// Test target name.
    pub name: Option<String>,
    /// Number of test cases the caller declares. Without it, or with 0, the
    /// run never counts as passed.
    pub case_count: Option<u64>,
    /// Output format to count the cases from instead (`--test-format`).
    pub format: Option<TestFormat>,
}

impl TestSpec {
    /// U9 K2 (Owner O6): the `test_format` member of the request: the
    /// format of `--test-format`, else `cases` for a `--test-cases` count,
    /// else none (`--test` alone records no count and never passes).
    #[must_use]
    pub fn wire_format(&self) -> Option<&'static str> {
        self.format
            .map(TestFormat::name)
            .or_else(|| self.case_count.map(|_| "cases"))
    }
}

/// A test runner whose output `memprove run` can count cases from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestFormat {
    /// `cargo test` (libtest): the `test result:` line of every test binary.
    Cargo,
    /// pytest: the summary on its last line (U5 H4).
    Pytest,
}

impl TestFormat {
    /// The format named on the command line.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "cargo" => Some(Self::Cargo),
            "pytest" => Some(Self::Pytest),
            _ => None,
        }
    }

    /// The name `--test-format` takes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Pytest => "pytest",
        }
    }

    /// The case count of `executed`, or `None` when it cannot be read; a
    /// run without a count never passes.
    #[must_use]
    pub fn case_count(self, executed: &Executed) -> Option<u64> {
        self.counted(executed).map(|(count, _)| count)
    }

    /// [`TestFormat::case_count`], and where the count was read (U7 P4).
    #[must_use]
    pub fn counted(self, executed: &Executed) -> Option<(u64, CountSource)> {
        match self {
            Self::Cargo => cargo_case_count(&executed.stdout, &executed.stderr, executed.exit_code)
                .map(|count| (count, CountSource::Summary)),
            Self::Pytest => pytest_counted(&executed.stdout, &executed.stderr, executed.exit_code),
        }
    }
}

/// Where a case count was read from (U7 P4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountSource {
    /// The runner's summary: cargo's `test result:` lines, pytest's last
    /// line.
    Summary,
    /// pytest's last line of progress marks, when it printed no summary
    /// (D2 F12, Owner O6).
    ProgressLine,
}

/// U7 P4: what the run record adds after a count read from a pytest
/// progress line (Owner O6).
pub const PROGRESS_LINE_NOTE: &str = "counted from the progress line";

/// U7 P4 (D2 F12, Owner O6): what `run` adds when it cannot count the cases
/// of a pytest run.
pub const PYTEST_NO_SUMMARY: &str =
    "pytest printed no summary (`-qq`, or a `-q` in `addopts`, removes it)";

/// U6 K2 (U5 N7): the sentence for a caller's `--test-cases` that differs
/// from the count the output of `executed` gives under `format`. `None` when
/// they agree, and when the output holds no count that can be read (the
/// value given is then all there is). The value given is what is recorded.
#[must_use]
///
/// U7 P4: a count read from a pytest progress line is compared the same
/// way, and the sentence says where it was read.
pub fn case_count_warning(given: u64, format: TestFormat, executed: &Executed) -> Option<String> {
    let (counted, source) = format.counted(executed)?;
    (counted != given).then(|| match source {
        CountSource::Summary => format!(
            "--test-cases {given} differs from the {counted} test cases in the {} summary; \
             {given} is recorded",
            format.name()
        ),
        CountSource::ProgressLine => format!(
            "--test-cases {given} differs from the {counted} test cases counted from the {} \
             progress line ({PROGRESS_LINE_NOTE}); {given} is recorded",
            format.name()
        ),
    })
}

/// U8 K5: `word` as a shell would take it whole: left as it is when it is
/// plain, else in single quotes.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-./=:,@%+".contains(character));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// U8 K5: `argv` as one command line, each element quoted when it needs it.
#[must_use]
pub fn shell_line(argv: &[String]) -> String {
    argv.iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// U8 K5 (Owner O5): the one reason a fact may not cite the run `label`, a
/// command run (`command_result`) of `argv`: `checkpoint create --fact` and
/// the MCP `checkpoint` tool say it before they send anything. The
/// arguments are those of [`memprove_domain::rerun_arguments`].
#[must_use]
pub fn command_run_cannot_support_fact(label: &str, argv: &[String]) -> String {
    format!(
        "{label} is a command run and cannot support a fact; a rerun with memprove run {} -- {} \
         can be cited",
        memprove_domain::rerun_arguments(argv),
        shell_line(argv)
    )
}

/// U8 K5 (Owner O5): the line `run` adds, without `--test` and
/// `--test-format`, when its `argv` is the Task's last passed test run.
///
/// U9 K2 (Owner O6): the arguments are those of the passed run's recorded
/// `test_format` when it has one, else the guess from its argv.
#[must_use]
pub fn same_command_hint(argv: &[String], format: Option<memprove_domain::TestFormat>) -> String {
    format!(
        "note: this command is the Task's last passed test run, but without --test or --test-format \
         it is recorded as a command run, which cannot support a fact; {} records it as a test run",
        memprove_domain::rerun_arguments_for(argv, format)
    )
}

/// U8 K5 (Owner O5): the hint that `spec`, a run without `--test` and
/// `--test-format`, ran the argv of the Task's last passed test run, and which
/// arguments to add. U8 F1 (SF-1): the Task Capsule is read before the run is
/// recorded (the command run would be the Task's latest run, and a passed test
/// run that no statement cites is placed on the Pack only while it is the
/// latest), and the run is the one the Capsule shows as the last passed test
/// run ([`memprove_domain::PackCapsule::last_passed_test_argv`]), whether or
/// not the Pack prints its `Last passed: ` line. One read; best effort: a Capsule
/// that cannot be read gives no hint.
pub fn last_passed_hint<D: crate::flow::Daemon>(daemon: &D, spec: &RunSpec) -> Option<String> {
    let request = Request::bare(
        OperationId::GetTaskCapsule,
        format!(
            "/v1/tasks/{}/recovery-capsule?workspace_id={}",
            spec.task_id, spec.workspace_id
        ),
    );
    let reply = daemon.read(&request).ok()?;
    let capsule = memprove_domain::PackCapsule::from_json(&reply.body).ok()?;
    let (argv, format) = capsule.last_passed_test()?;
    (argv == spec.argv.as_slice()).then(|| same_command_hint(&spec.argv, format))
}

/// The cases pytest reports (U5 H4, Owner O6): passed + failed + errors of
/// the summary on the last non-empty line of standard output (of standard
/// error when standard output has none), `=== 1 failed, 4 passed, 2 skipped
/// in 0.30s ===` with or without the `=` rule (`-q`). Skipped, xfailed,
/// xpassed, deselected, warnings and reruns are not cases; `no tests ran`
/// is zero cases.
///
/// `None` when the last line is not a summary (a word or a time it does not
/// know, a count given twice, text after the time), or when it reports
/// failures or errors although the command exited 0: an unreadable count is
/// never a pass (the run is then recorded without a count, as with
/// `cargo`). Parsing output text cannot be fully reliable; a plugin that
/// prints after the summary is unreadable. The run passes when the daemon
/// finds the exit code 0 and the count above 0, which is when failed and
/// errors are 0 and something passed.
///
/// U7 P4 (D2 F12, Owner O6): `pytest -qq` (or `-q` with another `-q` in
/// `addopts`) prints no summary, only progress marks. When the last line is
/// not a summary but a line of marks ([`pytest_progress`]), it is counted
/// ([`pytest_counted`]); U8 K3: skipped, xfailed, and xpassed marks are not
/// cases there either.
#[must_use]
pub fn pytest_case_count(stdout: &[u8], stderr: &[u8], exit_code: i32) -> Option<u64> {
    pytest_counted(stdout, stderr, exit_code).map(|(count, _)| count)
}

/// [`pytest_case_count`], and whether the count came from the summary or
/// (U7 P4) from a last line of progress marks: then the cases are the marks
/// `.`, `F`, and `E`, and the failures its `F` and `E`; failures under exit
/// code 0 contradict each other and give `None`. U8 K3 (U7 V-N5): `s`, `x`,
/// and `X` (skipped, xfailed, xpassed) are not cases, as in a summary, so
/// O6's "cases = characters" reads as the characters that are cases; a line of
/// only such marks is zero cases and never passes (the daemon wants a count
/// above 0), as `4 skipped` is. Only the last line is read: a progress
/// display that pytest wrapped over several lines is counted by its last
/// line alone.
#[must_use]
pub fn pytest_counted(stdout: &[u8], stderr: &[u8], exit_code: i32) -> Option<(u64, CountSource)> {
    let last_line = |stream: &[u8]| {
        strip_ansi(&String::from_utf8_lossy(stream))
            .lines()
            .rev()
            .map(|line| line.trim().to_owned())
            .find(|line| !line.is_empty())
    };
    let line = last_line(stdout).or_else(|| last_line(stderr))?;
    let (count, failures, source) = if let Some((passed, failed, errors)) = pytest_summary(&line) {
        (
            passed.checked_add(failed)?.checked_add(errors)?,
            failed.checked_add(errors)?,
            CountSource::Summary,
        )
    } else {
        let (marks, failures) = pytest_progress(&line)?;
        (marks, failures, CountSource::ProgressLine)
    };
    if failures > 0 && exit_code == 0 {
        return None;
    }
    (count <= 9_007_199_254_740_991).then_some((count, source))
}

/// U7 P4 (Owner O6): `(cases, failures)` of a line of pytest progress marks,
/// `^[.FEsxX]+( +\[ *\d+%\])?$`: how many of its marks are cases (U8 K3: `.`,
/// `F`, and `E`; not `s`, `x`, or `X`), and how many of them are `F` or `E`;
/// `None` for any other line.
fn pytest_progress(line: &str) -> Option<(u64, u64)> {
    let end = line
        .find(|mark| !matches!(mark, '.' | 'F' | 'E' | 's' | 'x' | 'X'))
        .unwrap_or(line.len());
    let (marks, rest) = line.split_at(end);
    if marks.is_empty() {
        return None;
    }
    if !rest.is_empty() {
        let percent = rest
            .trim_start_matches(' ')
            .strip_prefix('[')?
            .trim_start_matches(' ')
            .strip_suffix("%]")?;
        if !rest.starts_with(' ')
            || percent.is_empty()
            || !percent.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
    }
    let cases = marks
        .chars()
        .filter(|mark| matches!(mark, '.' | 'F' | 'E'))
        .count();
    let failures = marks
        .chars()
        .filter(|mark| matches!(mark, 'F' | 'E'))
        .count();
    Some((u64::try_from(cases).ok()?, u64::try_from(failures).ok()?))
}

/// `(passed, failed, errors)` of one pytest summary line, or `None`.
fn pytest_summary(line: &str) -> Option<(u64, u64, u64)> {
    let line = line.trim_matches('=').trim();
    let (items, time) = line.rsplit_once(" in ")?;
    let (seconds, rest) = time.split_once(' ').unwrap_or((time, ""));
    let seconds = seconds.strip_suffix('s')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
    if !digits(whole) || !digits(fraction) {
        return None;
    }
    // `(0:01:01)`, the clock form pytest appends to the seconds.
    if !rest.is_empty() {
        let clock = rest.strip_prefix('(')?.strip_suffix(')')?;
        let parts = clock.split(':').collect::<Vec<_>>();
        if parts.len() != 3 || !parts.iter().all(|part| digits(part)) {
            return None;
        }
    }
    if items == "no tests ran" {
        return Some((0, 0, 0));
    }
    let (mut passed, mut failed, mut errors) = (None, None, None);
    for item in items.split(", ") {
        let (number, word) = item.split_once(' ')?;
        let number = number.parse::<u64>().ok()?;
        let slot = match word {
            "passed" => &mut passed,
            "failed" => &mut failed,
            "error" | "errors" => &mut errors,
            "skipped" | "xfailed" | "xpassed" | "deselected" | "warning" | "warnings" | "rerun"
            | "reruns" => continue,
            _ => return None,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(number);
    }
    Some((
        passed.unwrap_or(0),
        failed.unwrap_or(0),
        errors.unwrap_or(0),
    ))
}

/// The cases `cargo test` reports: the passed and failed counts summed over
/// every `test result:` line (one per test binary and doc-test run).
///
/// Only lines that start with `test result: ` count, and none inside the
/// captured output libtest prints for a failed test: from a
/// `---- <name> stdout ----` header up to the `failures:` list after it
/// (or the `successes:` list, for `--show-output`). That output is the
/// test's own, and may hold the summary lines of an inner `cargo test`
/// (P2-J). Parsing output text cannot be fully reliable (Owner decision,
/// P2-J); with `--nocapture` the inner lines are not fenced off and do
/// count.
///
/// `None` when no such line is found, when one cannot be parsed, or when a
/// line reports failures although the command exited 0: an unreadable
/// count is never a pass. Ignored and filtered-out tests are not cases.
#[must_use]
pub fn cargo_case_count(stdout: &[u8], stderr: &[u8], exit_code: i32) -> Option<u64> {
    let mut total: Option<u64> = None;
    for stream in [stdout, stderr] {
        let mut captured = false;
        for line in strip_ansi(&String::from_utf8_lossy(stream)).lines() {
            if line.starts_with("---- ") && line.ends_with(" stdout ----") {
                captured = true;
                continue;
            }
            if captured {
                captured = !matches!(line, "failures:" | "successes:");
                continue;
            }
            let Some(rest) = line.strip_prefix("test result: ") else {
                continue;
            };
            let (passed, failed) = libtest_counts(rest)?;
            if failed > 0 && exit_code == 0 {
                return None;
            }
            total = Some(
                total
                    .unwrap_or(0)
                    .checked_add(passed.checked_add(failed)?)?,
            );
        }
    }
    total.filter(|count| *count <= 9_007_199_254_740_991)
}

/// `(passed, failed)` from the remainder of a libtest summary line,
/// `ok. 3 passed; 0 failed; 1 ignored; ...` or `FAILED. 1 passed; 2 failed; ...`.
fn libtest_counts(rest: &str) -> Option<(u64, u64)> {
    let counts = rest
        .strip_prefix("ok. ")
        .or_else(|| rest.strip_prefix("FAILED. "))?;
    let (mut passed, mut failed) = (None, None);
    for part in counts.split("; ") {
        let mut words = part.split(' ');
        let (Some(number), Some(label)) = (words.next(), words.next()) else {
            continue;
        };
        let slot = match label {
            "passed" => &mut passed,
            "failed" => &mut failed,
            _ => continue,
        };
        if slot.is_some() {
            return None;
        }
        *slot = Some(number.parse::<u64>().ok()?);
    }
    Some((passed?, failed?))
}

/// `text` without ANSI CSI escape sequences (`--color always`).
fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            if characters.next() == Some('[') {
                for inner in characters.by_ref() {
                    if ('@'..='~').contains(&inner) {
                        break;
                    }
                }
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

/// One finished command, or one that could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executed {
    /// Exit code; `128 + signal` when a signal ended the process, and
    /// [`LAUNCH_FAILED_EXIT_CODE`] when it never started.
    pub exit_code: i32,
    /// Start time, RFC 3339 UTC with milliseconds.
    pub started_at: String,
    /// End time, RFC 3339 UTC with milliseconds.
    pub finished_at: String,
    /// Complete standard output.
    pub stdout: Vec<u8>,
    /// Complete standard error.
    pub stderr: Vec<u8>,
    /// Why the program could not be started; the result is then
    /// `UNAVAILABLE` (PRD FR-203) and supports no claim that it passed.
    pub launch_error: Option<String>,
}

/// The exit code recorded for a program that never started; a started
/// process never reports it.
pub const LAUNCH_FAILED_EXIT_CODE: i32 = -1;

impl Executed {
    /// The result of a program that could not be started at `at`: no
    /// output, the launch error text with control characters replaced.
    #[must_use]
    pub fn unavailable(error: &io::Error, at: String) -> Self {
        let reason = error
            .to_string()
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect::<String>();
        let reason = if reason.trim().is_empty() {
            "the program could not be started".to_owned()
        } else {
            reason
        };
        Self {
            exit_code: LAUNCH_FAILED_EXIT_CODE,
            started_at: at.clone(),
            finished_at: at,
            stdout: Vec::new(),
            stderr: Vec::new(),
            launch_error: Some(reason),
        }
    }
}

/// The child process of `run`: `argv` with the caller's environment except
/// `MEMPROVE_TOKEN` (Owner decision P2-H: the bearer token is not passed
/// to the command, whose output becomes Evidence; every other variable,
/// `MEMPROVE_*` included, is inherited), working directory, and standard
/// input, capturing standard output and error.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an empty `argv`.
pub fn child_command(argv: &[String]) -> io::Result<Command> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no command given"))?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_remove(TOKEN_ENV)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command)
}

/// Runs `argv` as [`child_command`] describes.
///
/// # Errors
///
/// Returns the I/O error when the program cannot be started or awaited.
pub fn execute(argv: &[String]) -> io::Result<Executed> {
    let mut command = child_command(argv)?;
    let started_at = utc_now();
    let output = command.output()?;
    let finished_at = utc_now();
    let exit_code = output
        .status
        .code()
        .or_else(|| output.status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1);
    Ok(Executed {
        exit_code,
        started_at,
        finished_at,
        stdout: output.stdout,
        stderr: output.stderr,
        launch_error: None,
    })
}

/// The `captureEvidence` request for `executed` under `spec`, sealing it into
/// the new Snapshot `snapshot_id`. With `spec.digest_only` the output tails
/// are sent empty; their digests always cover the complete output.
#[must_use]
pub fn capture_request(spec: &RunSpec, executed: &Executed, snapshot_id: &str) -> Request {
    let kept = |bytes: &[u8]| {
        if spec.digest_only {
            String::new()
        } else {
            tail(bytes)
        }
    };
    let argv = spec
        .argv
        .iter()
        .map(|argument| json_string(argument))
        .collect::<Vec<_>>()
        .join(",");
    let mut metadata = format!(
        r#"{{"argv":[{argv}],"exit_code":{},"started_at":{},"finished_at":{},"stdout_tail":{},"stderr_tail":{},"stdout_sha256":{},"stderr_sha256":{},"collector":{{"name":{},"version":{}}}"#,
        executed.exit_code,
        json_string(&executed.started_at),
        json_string(&executed.finished_at),
        json_string(&kept(&executed.stdout)),
        json_string(&kept(&executed.stderr)),
        json_string(&sha256(&executed.stdout)),
        json_string(&sha256(&executed.stderr)),
        json_string(COLLECTOR_NAME),
        json_string(env!("CARGO_PKG_VERSION")),
    );
    if let Some(launch_error) = &executed.launch_error {
        write!(metadata, r#","launch_error":{}"#, json_string(launch_error))
            .expect("writing to a String cannot fail");
    }
    if let Some(test) = &spec.test {
        if let Some(name) = &test.name {
            write!(metadata, r#","test_name":{}"#, json_string(name))
                .expect("writing to a String cannot fail");
        }
        if let Some(count) = test.case_count {
            write!(metadata, r#","test_case_count":{count}"#)
                .expect("writing to a String cannot fail");
        }
        // U9 K2 (Owner O6): how the run was made, so a rerun is named from
        // the record: the format when `--test-format` was given, `cases`
        // for a count the caller declared.
        if let Some(format) = test.wire_format() {
            write!(metadata, r#","test_format":{}"#, json_string(format))
                .expect("writing to a String cannot fail");
        }
    }
    metadata.push('}');
    let body = format!(
        r#"{{"schema_version":1,"workspace_id":{},"task_id":{},"session_id":{},"artifact_type":{},"snapshot_id":{},"metadata":{metadata}}}"#,
        json_string(&spec.workspace_id),
        json_string(&spec.task_id),
        json_string(&spec.session_id),
        json_string(if spec.test.is_some() {
            "test_run"
        } else {
            "command_run"
        }),
        json_string(snapshot_id),
    );
    Request {
        operation: OperationId::CaptureEvidence,
        target: "/v1/evidence/capture".to_owned(),
        body: Some(body),
        idempotency_key: spec.idempotency_key.clone(),
        summary: false,
        all_items: false,
        capsule_view: crate::CapsuleView::Plain,
        pending: None,
    }
}

/// The last [`MAX_OUTPUT_TAIL_CHARS`] characters of `bytes`, decoded as UTF-8
/// with replacement characters.
#[must_use]
pub fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(MAX_OUTPUT_TAIL_CHARS))
        .collect()
}

/// `sha256:` and the lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::from("sha256:"), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        })
}

/// A fresh `UUIDv7` (RFC 9562): the Unix time in milliseconds and 74 random
/// bits from `/dev/urandom`.
///
/// # Errors
///
/// Returns the I/O error when the random device cannot be read.
pub fn uuid_v7() -> io::Result<String> {
    let mut random = [0_u8; 10];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let millis = u64::try_from(millis).unwrap_or(u64::MAX).to_be_bytes();
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&millis[2..]);
    bytes[6] = 0x70 | (random[0] & 0x0f);
    bytes[7] = random[1];
    bytes[8] = 0x80 | (random[2] & 0x3f);
    bytes[9..].copy_from_slice(&random[3..]);
    let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
        hex
    });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// The current time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
#[must_use]
pub fn utc_now() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_utc(elapsed.as_secs(), elapsed.subsec_millis())
}

/// Formats Unix seconds and milliseconds as an RFC 3339 UTC timestamp, using
/// the proleptic Gregorian civil-from-days conversion.
#[must_use]
pub fn format_utc(seconds: u64, millis: u32) -> String {
    let days = seconds / 86_400;
    let of_day = seconds % 86_400;
    // Days since 0000-03-01, so a year ends with February.
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    )
}

/// `args` after `--`, as UTF-8 strings.
///
/// # Errors
///
/// Returns the first argument that is not valid UTF-8.
pub fn utf8_argv(args: Vec<OsString>) -> Result<Vec<String>, OsString> {
    args.into_iter().map(OsString::into_string).collect()
}

#[cfg(test)]
mod tests {
    use memprove_protocol::{
        phase1::validate_request,
        strict_json::{JsonValue, parse_strict},
    };

    use super::*;

    /// U8 K5 (Owner O5): the two sentences that tell an agent to run a
    /// command as a test run, and the quoting of the argv in them.
    #[test]
    fn the_rerun_sentences_name_the_arguments_and_quote_the_argv() {
        let argv = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            command_run_cannot_support_fact("r3", &argv(&["sh", "tests/check.sh"])),
            "r3 is a command run and cannot support a fact; a rerun with memprove run --test \
             --test-cases <N> -- sh tests/check.sh can be cited"
        );
        assert_eq!(
            command_run_cannot_support_fact(
                "r12",
                &argv(&["env", "A=1", "cargo", "test", "-p", "x"])
            ),
            "r12 is a command run and cannot support a fact; a rerun with memprove run --test \
             --test-format cargo -- env A=1 cargo test -p x can be cited"
        );
        // An element a shell would split or expand is quoted; a quote in it
        // is closed and reopened; an empty element is two quotes.
        assert_eq!(
            shell_line(&argv(&[
                "pytest",
                "-k",
                "a b",
                "it's",
                "",
                "$HOME",
                "x=y:z,1@2%3+4"
            ])),
            "pytest -k 'a b' 'it'\\''s' '' '$HOME' x=y:z,1@2%3+4"
        );
        assert_eq!(
            same_command_hint(&argv(&["python3", "-m", "pytest"]), None),
            "note: this command is the Task's last passed test run, but without --test or \
             --test-format it is recorded as a command run, which cannot support a fact; --test \
             --test-format pytest records it as a test run"
        );
    }

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(format_utc(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_utc(951_782_400, 7), "2000-02-29T00:00:00.007Z");
        assert_eq!(format_utc(1_790_942_400, 250), "2026-10-02T12:00:00.250Z");
        assert_eq!(format_utc(4_102_444_799, 999), "2099-12-31T23:59:59.999Z");
    }

    #[test]
    fn uuid_v7_has_the_version_and_variant_bits() {
        let first = uuid_v7().unwrap();
        let second = uuid_v7().unwrap();
        assert_ne!(first, second);
        let bytes = first.as_bytes();
        assert_eq!(first.len(), 36);
        assert_eq!(bytes[14], b'7');
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'));
    }

    #[test]
    fn digests_and_tails_are_bounded_and_exact() {
        assert_eq!(
            sha256(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let long = "é".repeat(MAX_OUTPUT_TAIL_CHARS + 10);
        let kept = tail(long.as_bytes());
        assert_eq!(kept.chars().count(), MAX_OUTPUT_TAIL_CHARS);
        assert_eq!(tail(b"ok\n"), "ok\n");
    }

    #[test]
    fn capture_requests_satisfy_the_contract() {
        let id = "018f47a0-0000-7000-8000-000000000001";
        let executed = execute(&[
            "sh".to_owned(),
            "-c".to_owned(),
            "echo out; echo err >&2; exit 3".to_owned(),
        ])
        .unwrap();
        assert_eq!(executed.exit_code, 3);
        assert_eq!(executed.stdout, b"out\n");
        assert_eq!(executed.stderr, b"err\n");
        for test in [
            None,
            Some(TestSpec {
                name: Some("unit \"quoted\"".to_owned()),
                case_count: Some(0),
                format: None,
            }),
        ] {
            let spec = RunSpec {
                session_id: id.to_owned(),
                workspace_id: id.to_owned(),
                task_id: id.to_owned(),
                test,
                argv: vec!["sh".to_owned(), "-c".to_owned(), "exit 3".to_owned()],
                idempotency_key: None,
                digest_only: false,
            };
            let request = capture_request(&spec, &executed, id);
            let body = parse_strict(request.body.as_deref().unwrap()).unwrap();
            validate_request(OperationId::CaptureEvidence, Some(&body)).unwrap();
        }
    }

    fn spec(digest_only: bool) -> RunSpec {
        let id = "018f47a0-0000-7000-8000-000000000001";
        RunSpec {
            session_id: id.to_owned(),
            workspace_id: id.to_owned(),
            task_id: id.to_owned(),
            test: Some(TestSpec {
                name: None,
                case_count: Some(3),
                format: None,
            }),
            argv: vec!["/nonexistent/memprove-test-program".to_owned()],
            idempotency_key: None,
            digest_only,
        }
    }

    fn metadata(request: &Request) -> JsonValue {
        parse_strict(request.body.as_deref().unwrap())
            .unwrap()
            .get("metadata")
            .unwrap()
            .clone()
    }

    fn text<'a>(value: &'a JsonValue, name: &str) -> &'a str {
        value.get(name).and_then(JsonValue::as_str).unwrap()
    }

    /// P2-H item 6: the child loses `MEMPROVE_TOKEN` and only it.
    #[test]
    fn the_child_command_does_not_inherit_the_token() {
        let command = child_command(&["env".to_owned()]).unwrap();
        let changes = command
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<Vec<_>>();
        assert_eq!(changes, [(OsString::from(TOKEN_ENV), None)]);
        assert!(child_command(&[]).is_err());
    }

    #[test]
    fn a_missing_program_is_recorded_as_unavailable() {
        let error = execute(&["/nonexistent/memprove-test-program".to_owned()]).unwrap_err();
        let executed = Executed::unavailable(&error, utc_now());
        assert_eq!(executed.exit_code, LAUNCH_FAILED_EXIT_CODE);
        let request = capture_request(
            &spec(false),
            &executed,
            "018f47a0-0000-7000-8000-0000000000aa",
        );
        let body = parse_strict(request.body.as_deref().unwrap()).unwrap();
        validate_request(OperationId::CaptureEvidence, Some(&body)).unwrap();
        let recorded = metadata(&request);
        assert_eq!(
            recorded.get("exit_code").and_then(JsonValue::as_i64),
            Some(i64::from(LAUNCH_FAILED_EXIT_CODE))
        );
        assert!(!text(&recorded, "launch_error").is_empty());
        // A started program sends no launch_error.
        let started = execute(&["true".to_owned()]).unwrap();
        let request = capture_request(
            &spec(false),
            &started,
            "018f47a0-0000-7000-8000-0000000000ab",
        );
        assert!(metadata(&request).get("launch_error").is_none());
    }

    const CARGO_OK: &str = "\
running 2 tests
test a ... ok
test b ... ok

test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

   Doc-tests demo

running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
";

    #[test]
    fn cargo_counts_sum_every_test_binary() {
        assert_eq!(cargo_case_count(CARGO_OK.as_bytes(), b"", 0), Some(6));
        // Summary lines on standard error count too, with colour codes.
        let coloured = "test result: \u{1b}[32mok\u{1b}[0m. 4 passed; 0 failed; 0 ignored";
        assert_eq!(cargo_case_count(b"", coloured.as_bytes(), 0), Some(4));
    }

    /// P2-J: the one failed outer test captured an inner `cargo test`,
    /// whose summary lines appear in its captured output; an indented
    /// summary line does not count either.
    #[test]
    fn cargo_skips_the_captured_output_of_failed_tests() {
        let nested = "\
running 1 test
test nested_cargo ... FAILED

failures:

---- nested_cargo stdout ----
running 2 tests
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
   Doc-tests inner
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
thread 'nested_cargo' panicked at tests/nested.rs:9:5:
inner run left a file behind

failures:
    nested_cargo

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.50s
";
        assert_eq!(cargo_case_count(nested.as_bytes(), b"", 101), Some(1));
        let indented = "    test result: ok. 9 passed; 0 failed; 0 ignored\n\
                        test result: ok. 2 passed; 0 failed; 0 ignored\n";
        assert_eq!(cargo_case_count(indented.as_bytes(), b"", 0), Some(2));
        // `--show-output` prints passed tests' output under `successes:`.
        let shown = "\
successes:

---- nested_cargo stdout ----
test result: ok. 5 passed; 0 failed; 0 ignored

successes:
    nested_cargo

test result: ok. 1 passed; 0 failed; 0 ignored
";
        assert_eq!(cargo_case_count(shown.as_bytes(), b"", 0), Some(1));
    }

    /// U6 K2: a given count is compared with a readable pytest summary only.
    #[test]
    fn a_given_case_count_is_compared_with_a_readable_pytest_summary_only() {
        let executed = |stdout: &str, exit_code: i32| Executed {
            exit_code,
            started_at: String::new(),
            finished_at: String::new(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
            launch_error: None,
        };
        let five = executed("=== 5 passed in 0.1s ===\n", 0);
        assert_eq!(case_count_warning(5, TestFormat::Pytest, &five), None);
        assert_eq!(
            case_count_warning(3, TestFormat::Pytest, &five).as_deref(),
            Some(
                "--test-cases 3 differs from the 5 test cases in the pytest summary; 3 is \
                 recorded"
            )
        );
        // Unreadable: nothing to compare. So is a contradictory summary.
        for unreadable in [
            executed("nothing\n", 0),
            executed("=== 1 failed, 4 passed in 0.1s ===\n", 0),
        ] {
            assert_eq!(case_count_warning(3, TestFormat::Pytest, &unreadable), None);
        }
    }

    #[test]
    fn cargo_failures_are_cases_of_a_failed_run() {
        let failed = "test result: ok. 2 passed; 0 failed; 0 ignored\n\
                      test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured\n";
        assert_eq!(cargo_case_count(failed.as_bytes(), b"", 101), Some(4));
        // Failures under exit code 0 contradict each other: no count.
        assert_eq!(cargo_case_count(failed.as_bytes(), b"", 0), None);
    }

    #[test]
    fn cargo_zero_cases_and_unreadable_output_never_pass() {
        let empty = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored\n";
        assert_eq!(cargo_case_count(empty.as_bytes(), b"", 0), Some(0));
        for unreadable in [
            "",
            "all good\n",
            "test result: ok. many passed; 0 failed\n",
            "test result: ok. 3 passed\n",
            "test result: maybe. 3 passed; 0 failed\n",
            "test result: ok. 3 passed; 1 passed; 0 failed\n",
        ] {
            assert_eq!(
                cargo_case_count(unreadable.as_bytes(), b"", 0),
                None,
                "{unreadable:?}"
            );
        }
        let executed = Executed {
            exit_code: 0,
            started_at: utc_now(),
            finished_at: utc_now(),
            stdout: b"done\n".to_vec(),
            stderr: Vec::new(),
            launch_error: None,
        };
        assert_eq!(TestFormat::Cargo.case_count(&executed), None);
        assert_eq!(TestFormat::parse("cargo"), Some(TestFormat::Cargo));
        assert_eq!(TestFormat::parse("pytest"), Some(TestFormat::Pytest));
        assert_eq!(TestFormat::parse("junit"), None);
    }

    fn pytest(stdout: &str, exit_code: i32) -> Option<u64> {
        pytest_case_count(stdout.as_bytes(), b"", exit_code)
    }

    /// U5 H4 (O6): the cases are passed + failed + errors of the last
    /// line's summary; skipped and the other words are not cases.
    #[test]
    fn pytest_counts_passed_failed_and_error_cases_from_the_last_line() {
        // All passed, with and without the `=` rule (`-q`).
        assert_eq!(
            pytest("collected 5 items\n\n===== 5 passed in 0.12s =====\n", 0),
            Some(5)
        );
        assert_eq!(pytest(".....\n5 passed in 0.12s\n", 0), Some(5));
        // Failed cases are cases of a failed run.
        assert_eq!(
            pytest(
                "FAILED t.py::a - assert 0\n=== 1 failed, 4 passed in 0.3s ===\n",
                1
            ),
            Some(5)
        );
        // Errors (fixtures, collection) are cases too.
        assert_eq!(pytest("=== 3 passed, 2 errors in 0.2s ===\n", 1), Some(5));
        assert_eq!(pytest("=== 1 error in 0.2s ===\n", 2), Some(1));
        assert_eq!(pytest("=== 1 warning, 2 errors in 0.2s ===\n", 2), Some(2));
        assert_eq!(
            pytest("=== 1 failed, 2 passed, 1 error in 0.4s (0:00:00) ===\n", 1),
            Some(4)
        );
        // Skipped, xfailed, xpassed, deselected, warnings and reruns are not cases.
        assert_eq!(
            pytest(
                "=== 3 passed, 2 skipped, 1 xfailed, 1 xpassed, 4 deselected, 2 warnings, 1 rerun in 1.00s ===\n",
                0
            ),
            Some(3)
        );
        assert_eq!(pytest("=== 4 skipped in 0.01s ===\n", 0), Some(0));
        assert_eq!(pytest("=== no tests ran in 0.01s ===\n", 5), Some(0));
        // Colour codes, trailing blank lines, the long-run time form.
        assert_eq!(
            pytest(
                "\u{1b}[32m===== \u{1b}[1m\u{1b}[32m7 passed\u{1b}[0m\u{1b}[32m in 61.2s (0:01:01)\u{1b}[0m\u{1b}[32m =====\u{1b}[0m\n\n",
                0
            ),
            Some(7)
        );
        // The summary on standard error counts when standard output has none.
        assert_eq!(
            pytest_case_count(b"", b"=== 2 passed in 0.1s ===\n", 0),
            Some(2)
        );
    }

    fn counted(stdout: &str, exit_code: i32) -> Option<(u64, CountSource)> {
        TestFormat::Pytest.counted(&Executed {
            exit_code,
            started_at: String::new(),
            finished_at: String::new(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
            launch_error: None,
        })
    }

    /// U7 P4 (D2 F12, Owner O6): with no summary (`-qq`, or `-q` in
    /// `addopts` on top of `-q`), a last line of progress marks
    /// (`^[.FEsxX]+( +\[ *\d+%\])?$`) is counted: the cases are its `.`, `F`,
    /// and `E` (U8 K3: `s`, `x`, and `X` are not cases, as in a summary); the
    /// failures are its `F` and `E`, and failures under exit code 0
    /// contradict each other. A summary, when there is one, is still what
    /// counts.
    #[test]
    fn pytest_counts_a_progress_line_when_there_is_no_summary() {
        let progress = Some(CountSource::ProgressLine);
        // All dots, with and without the percentage, D2's `-qq` output.
        assert_eq!(counted(".....\n", 0), Some((5, CountSource::ProgressLine)));
        assert_eq!(
            counted("..... [100%]\n", 0),
            Some((5, CountSource::ProgressLine))
        );
        assert_eq!(counted("....    [ 80%]\n\n", 0).map(|(n, _)| n), Some(4));
        // F and E are failures: a case of a failed run; under exit code 0
        // they contradict the exit code, and nothing is counted.
        assert_eq!(
            counted("..F.. [100%]\n", 1),
            Some((5, CountSource::ProgressLine))
        );
        assert_eq!(counted("..E\n", 1).map(|(n, _)| n), Some(3));
        assert_eq!(counted("..F..\n", 0), None);
        assert_eq!(counted("E\n", 0), None);
        // U8 K3 (U7 V-N5): s, x, X are not cases, as in a summary; only `.`,
        // `F`, and `E` are.
        assert_eq!(
            counted("..s.x.X\n", 0),
            Some((4, CountSource::ProgressLine))
        );
        assert_eq!(counted(".s.F\n", 1), Some((3, CountSource::ProgressLine)));
        assert_eq!(counted(".s.F [100%]\n", 0), None);
        // A line of only skipped marks is zero cases, which never passes
        // (the daemon wants a count above 0), as `4 skipped` is.
        assert_eq!(
            counted("ss [100%]\n", 0).map(|(n, source)| (n, Some(source))),
            Some((0, progress))
        );
        assert_eq!(counted("sxX\n", 0).map(|(n, _)| n), Some(0));
        assert_eq!(counted("s\n", 5).map(|(n, _)| n), Some(0));
        // On standard error when standard output has none.
        assert_eq!(pytest_case_count(b"", b"... [100%]\n", 0), Some(3));
        // A summary wins, and a summary line is not read as progress.
        assert_eq!(
            counted("..... [100%]\n5 passed in 0.1s\n", 0),
            Some((5, CountSource::Summary))
        );
        assert_eq!(counted("=== 1 failed, 4 passed in 0.3s ===\n", 0), None);
        // Not progress lines: they record no count (NOT_RUN).
        for unreadable in [
            ".... [100%] done\n",
            "..a..\n",
            ". . .\n",
            "[100%]\n",
            "..... [100]\n",
            "..... [%]\n",
            "..... [1 00%]\n",
            ".....[100%]\n",
            "..... [100%]\ncollected 5 items\n",
        ] {
            assert_eq!(counted(unreadable, 0), None, "{unreadable:?}");
        }
    }

    /// U7 P4: `--test-cases` still wins over a progress line, and the U5 K2
    /// warning names a count read from it.
    #[test]
    fn a_given_case_count_is_compared_with_a_progress_line_too() {
        let executed = Executed {
            exit_code: 0,
            started_at: String::new(),
            finished_at: String::new(),
            stdout: b"..... [100%]\n".to_vec(),
            stderr: Vec::new(),
            launch_error: None,
        };
        assert_eq!(case_count_warning(5, TestFormat::Pytest, &executed), None);
        assert_eq!(
            case_count_warning(3, TestFormat::Pytest, &executed).as_deref(),
            Some(
                "--test-cases 3 differs from the 5 test cases counted from the pytest progress \
                 line (counted from the progress line); 3 is recorded"
            )
        );
    }

    /// U5 H4: only the last line is the summary; failures under exit code 0
    /// contradict each other; anything else is unreadable and records no
    /// count, which never passes.
    #[test]
    fn pytest_output_it_cannot_read_records_no_count() {
        for unreadable in [
            "",
            "\n\n",
            "all good\n",
            // A summary-looking line that is not the last one (a test's output).
            "=== 5 passed in 0.1s ===\nthen something else\n",
            "=== many passed in 0.1s ===\n",
            "=== 3 passed ===\n",
            "=== 3 passed in soon ===\n",
            "=== 3 passed, 1 passed in 0.1s ===\n",
            "=== 3 surprised in 0.1s ===\n",
            "3 passed in 0.1s and more\n",
        ] {
            assert_eq!(pytest(unreadable, 0), None, "{unreadable:?}");
        }
        assert_eq!(pytest("=== 1 failed, 4 passed in 0.3s ===\n", 0), None);
        assert_eq!(pytest("=== 1 error in 0.3s ===\n", 0), None);
        // A failing exit code with an all-passed summary keeps the count;
        // the daemon fails the run on the exit code.
        assert_eq!(pytest("=== 5 passed in 0.3s ===\n", 1), Some(5));
    }

    #[test]
    fn digest_only_sends_digests_without_output_text() {
        let executed = execute(&[
            "sh".to_owned(),
            "-c".to_owned(),
            "echo secret-out; echo secret-err >&2".to_owned(),
        ])
        .unwrap();
        let kept = metadata(&capture_request(
            &spec(false),
            &executed,
            "018f47a0-0000-7000-8000-0000000000ac",
        ));
        let dropped = metadata(&capture_request(
            &spec(true),
            &executed,
            "018f47a0-0000-7000-8000-0000000000ad",
        ));
        assert_eq!(text(&kept, "stdout_tail"), "secret-out\n");
        assert_eq!(text(&dropped, "stdout_tail"), "");
        assert_eq!(text(&dropped, "stderr_tail"), "");
        assert_eq!(text(&dropped, "stdout_sha256"), sha256(b"secret-out\n"));
        assert_eq!(text(&dropped, "stderr_sha256"), sha256(b"secret-err\n"));
        assert_eq!(
            text(&dropped, "stdout_sha256"),
            text(&kept, "stdout_sha256")
        );
    }
}
