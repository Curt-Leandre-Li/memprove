//! P4-E (ADR 0014 E4, E5; Owner conditions j to p) through the real
//! binaries: `memprove hook session-start` reads Claude Code's
//! `SessionStart` JSON on standard input, sends only read requests to a
//! temporary `memproved`, and prints one statement and the Context Pack
//! in a fence, or one statement alone; it always exits 0 within its time
//! limit and never moves the ledger's high-water mark. With
//! `MEMPROVE_CLIENT_SESSION` the CLI keeps one context per client session
//! in the same directory.
//!
//! Run with `--nocapture` to print the transcript.

mod p4b_support;

use memprove_domain::says_verified;
use std::{
    fs,
    io::Write as _,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use p4b_support::{Cli, Run, Setup, cli_binary, isolated, sequence, step, text};
use serde_json::Value;

/// The hook's own time limit (Owner condition o): 8 seconds since U15 T1
/// (`hook::TIME_LIMIT`; 5 until then).
const LIMIT: Duration = Duration::from_secs(8);
/// The five `SessionStart` sources.
const SOURCES: [&str; 5] = ["startup", "resume", "clear", "compact", "fork"];
/// The most UTF-16 code units the hook may print (Owner condition n).
const MAX_OUTPUT: usize = 10_000;

/// U7 P1 (Owner O3, amended 2026-10-21): `other`, read by a reader for
/// whom the open Session of `pack`'s reader is someone else's, differs
/// from `pack` only there: that Session's line ends with the takeover hint,
/// and every other line but the digest line is the same.
fn only_the_takeover_hint(other: &str, pack: &str) {
    let lines = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with("Digest: "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let (theirs, ours) = (lines(other), lines(pack));
    assert_eq!(theirs.len(), ours.len(), "{other}");
    for (their, our) in theirs.iter().zip(&ours) {
        if their != our {
            assert!(
                our.starts_with("Session ") && our.contains(" not ended ("),
                "{our}"
            );
            // U10 T7: the hint names that Session's ID and number.
            assert_eq!(*their, format!("{our}{}", p4b_support::takeover_hint(our)));
        }
    }
    assert_eq!(
        other.matches("; end it before taking over: ").count(),
        1,
        "{other}"
    );
    assert!(!pack.contains("; end it before taking over: "), "{pack}");
}

/// The agent's CLI, as the client session `client` when given.
fn as_client(cli: &Cli, client: Option<&str>, words: &[&str]) -> Run {
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
        "[client {}] $ ct {}\n{}{}[exit {:?}]",
        client.unwrap_or("-"),
        words
            .iter()
            .map(|word| if word.len() > 60 { "<long>" } else { word })
            .collect::<Vec<_>>()
            .join(" "),
        if run.stdout.len() > 4000 {
            "<long output>\n"
        } else {
            &run.stdout
        },
        run.stderr,
        run.code
    );
    run
}

fn ok_as(cli: &Cli, client: Option<&str>, words: &[&str]) -> Value {
    let run = as_client(cli, client, words);
    assert_eq!(run.code, Some(0), "{words:?}: {}{}", run.stdout, run.stderr);
    serde_json::from_str(run.stdout.trim()).unwrap()
}

/// Where a hook run looks: configuration, data directory, token file, and
/// `CLAUDE_ENV_FILE`.
#[derive(Clone)]
struct HookEnv {
    config_dir: PathBuf,
    data_dir: PathBuf,
    token_file: Option<PathBuf>,
    env_file: PathBuf,
}

impl HookEnv {
    fn of(setup: &Setup) -> Self {
        Self {
            config_dir: setup.agent.config_dir.clone(),
            data_dir: setup.agent.data_dir.clone(),
            token_file: Some(setup.agent.token.clone()),
            env_file: setup.root_path.join("claude-env.sh"),
        }
    }
}

struct HookRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    elapsed: Duration,
}

/// `memprove hook session-start` in `cwd` with `input` on standard input,
/// killed after 30 s if it has not ended by then.
fn hook(env: &HookEnv, cwd: &Path, input: Vec<u8>) -> HookRun {
    let mut command = isolated(cli_binary());
    command
        .args(["hook", "session-start"])
        .current_dir(cwd)
        .env("MEMPROVE_CONFIG_DIR", &env.config_dir)
        .env("MEMPROVE_DATA_DIR", &env.data_dir)
        .env("CLAUDE_ENV_FILE", &env.env_file)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(token) = &env.token_file {
        command.env("MEMPROVE_TOKEN_FILE", token);
    }
    let started = Instant::now();
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || {
        // The hook may stop reading an oversized input; that is its right.
        let _ = stdin.write_all(&input);
    });
    let done = Arc::new(AtomicBool::new(false));
    let pid = child.id();
    let watchdog = {
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        })
    };
    let output = child.wait_with_output().unwrap();
    let elapsed = started.elapsed();
    done.store(true, Ordering::SeqCst);
    watchdog.join().unwrap();
    writer.join().unwrap();
    let run = HookRun {
        code: output.status.code(),
        stdout: text(&output.stdout),
        stderr: text(&output.stderr),
        elapsed,
    };
    println!(
        "$ memprove hook session-start   [{} ms, exit {:?}]\n{}{}",
        run.elapsed.as_millis(),
        run.code,
        run.stdout,
        if run.stderr.is_empty() {
            String::new()
        } else {
            format!("[stderr] {}", run.stderr)
        }
    );
    run
}

/// Claude Code's `SessionStart` input.
fn input(session_id: &str, cwd: &Path, source: &str) -> Vec<u8> {
    serde_json::json!({
        "session_id": session_id,
        "transcript_path": "/nowhere/transcript.jsonl",
        "cwd": cwd.to_str().unwrap(),
        "hook_event_name": "SessionStart",
        "source": source,
    })
    .to_string()
    .into_bytes()
}

/// What every hook run must satisfy: exit 0 within the limit plus one
/// second, a statement first (never `{`), at most 10,000 UTF-16 code units,
/// and no token.
fn check(run: &HookRun, setup: &Setup) {
    assert_eq!(run.code, Some(0), "{}{}", run.stdout, run.stderr);
    assert!(
        run.elapsed <= LIMIT + Duration::from_secs(1),
        "took {:?}",
        run.elapsed
    );
    assert!(!run.stdout.starts_with('{'), "{}", run.stdout);
    assert!(run.stdout.starts_with("memprove: "), "{}", run.stdout);
    assert!(
        run.stdout.encode_utf16().count() <= MAX_OUTPUT,
        "{} code units",
        run.stdout.encode_utf16().count()
    );
    for token in [&setup.agent.token, &setup.operator.token] {
        let secret = fs::read_to_string(token).unwrap();
        let secret = secret.trim();
        assert!(!run.stdout.contains(secret), "a token was printed");
        assert!(!run.stderr.contains(secret), "a token was printed");
    }
}

/// The statement of a run that injected nothing: one line, no fence.
fn statement_only(run: &HookRun) -> &str {
    assert!(!run.stdout.contains("```"), "{}", run.stdout);
    assert_eq!(run.stdout.trim_end().lines().count(), 1, "{}", run.stdout);
    assert!(
        run.stdout
            .contains("no Context Pack was injected into this session"),
        "{}",
        run.stdout
    );
    run.stdout.trim_end()
}

/// The Pack inside the fence of a run that injected one.
fn fenced(run: &HookRun) -> String {
    let (statement, rest) = run.stdout.split_once('\n').unwrap();
    assert!(!statement.contains("```"), "{statement}");
    let pack = rest
        .strip_prefix("```text\n")
        .and_then(|rest| rest.strip_suffix("```\n"))
        .unwrap_or_else(|| panic!("no fenced Pack:\n{}", run.stdout));
    assert!(pack.starts_with("Pack: "), "{pack}");
    assert!(
        pack.lines().last().unwrap().starts_with("Digest: sha256:"),
        "{pack}"
    );
    pack.to_owned()
}

/// The ledger's high-water mark of the Project, read by the operator.
fn high_water(setup: &Setup) -> u64 {
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

/// Every path under `root`, except the daemon's data directory.
fn tree(root: &Path, data_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.starts_with(data_dir) {
                continue;
            }
            if path.is_dir() && !path.is_symlink() {
                pending.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

/// The `MEMPROVE_CLIENT_SESSION` lines of the env file.
fn env_lines(env: &HookEnv) -> Vec<String> {
    fs::read_to_string(&env.env_file)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn session<'a>(capsule: &'a Value, session_id: &str) -> &'a Value {
    capsule["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["session_id"] == session_id)
        .unwrap()
}

/// A Task begun by client `client`, with one passed run and a verified
/// fact; its Session stays open.
fn verified_task(setup: &Setup, client: &str, title: &str) -> (String, String) {
    let begun = ok_as(
        &setup.agent,
        Some(client),
        &["begin", "--project", &setup.project_id, "--new-task", title],
    );
    let ran = as_client(
        &setup.agent,
        Some(client),
        &["run", "--test-format", "cargo", "--", "sh", "tests/a.sh"],
    );
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    ok_as(
        &setup.agent,
        Some(client),
        &["checkpoint", "create", "--fact", "a passes", "--next", "n1"],
    );
    (
        begun["task_id"].as_str().unwrap().to_owned(),
        begun["session_id"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn p4e_each_source_injects_the_pack_and_reads_only() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    step("client A begins a Task with a verified fact");
    let (task, _) = verified_task(&setup, "client-a", "P4-E hook");
    let expected = as_client(&setup.agent, Some("client-a"), &["resume", "--pack"]);
    assert_eq!(expected.code, Some(0), "{}", expected.stderr);
    let before = high_water(&setup);

    for source in SOURCES {
        step(&format!("SessionStart {source}"));
        let run = hook(&env, &setup.repo, input("client-a", &setup.repo, source));
        check(&run, &setup);
        let statement = run.stdout.lines().next().unwrap();
        assert!(statement.contains(&format!("Task {task}")), "{statement}");
        assert!(statement.contains(&format!("({source})")), "{statement}");
        assert!(
            statement.contains("this client session's context"),
            "{statement}"
        );
        assert_eq!(
            statement.contains("forked"),
            source == "fork",
            "only fork says it was forked: {statement}"
        );
        // The same ledger, probe, and budget give the same Pack.
        assert_eq!(fenced(&run), expected.stdout);
        assert!(says_verified(&fenced(&run)));
        assert_eq!(high_water(&setup), before, "the hook wrote to the ledger");
    }
    step("the client session is exported once");
    assert_eq!(
        env_lines(&env),
        ["export MEMPROVE_CLIENT_SESSION='client-a'"]
    );
}

/// One hook run of client `client` in the repository (source `startup`)
/// that must print one statement holding `needle` and leave the ledger's
/// high-water mark where it was.
fn statement_of(setup: &Setup, env: &HookEnv, client: &str, needle: &str) -> HookRun {
    let before = high_water(setup);
    let run = hook(env, &setup.repo, input(client, &setup.repo, "startup"));
    check(&run, setup);
    assert!(statement_only(&run).contains(needle), "{}", run.stdout);
    assert_eq!(high_water(setup), before, "the hook wrote to the ledger");
    run
}

/// The CLI of the agent with its contexts in `config_dir`.
fn agent_in(setup: &Setup, config_dir: &Path) -> Cli {
    Cli {
        config_dir: config_dir.to_path_buf(),
        ..setup.agent.clone()
    }
}

/// `begin --new-task <title>` as client `client`; returns the Task.
fn new_task(agent: &Cli, setup: &Setup, client: &str, title: &str) -> String {
    let begun = ok_as(
        agent,
        Some(client),
        &["begin", "--project", &setup.project_id, "--new-task", title],
    );
    begun["task_id"].as_str().unwrap().to_owned()
}

#[test]
fn p4e_no_context_no_task_or_several_tasks() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);

    step("no context and no Task: an empty configuration directory");
    let empty = HookEnv {
        config_dir: setup.root_path.join("config-empty"),
        ..env.clone()
    };
    statement_of(
        &setup,
        &empty,
        "fresh",
        "this directory has no memprove context",
    );

    step("a context of this directory, but no Task in progress");
    let other = HookEnv {
        config_dir: setup.root_path.join("config-other"),
        ..env.clone()
    };
    let agent = agent_in(&setup, &other.config_dir);
    let cancelled = new_task(&agent, &setup, "client-x", "cancelled");
    ok_as(&agent, Some("client-x"), &["end", "--reason", "stop"]);
    for state in ["blocked", "cancelled"] {
        let words = ["task", "transition", &cancelled, "--to", state];
        ok_as(&agent, Some("client-x"), &words);
    }
    statement_of(&setup, &other, "client-y", "no in-progress Task");

    step("several Tasks in progress and no context naming one");
    let first = new_task(&agent, &setup, "client-x2", "one");
    let second = new_task(&agent, &setup, "client-y2", "two");
    let run = statement_of(&setup, &other, "client-z", "2 in-progress Tasks");
    for task in [&first, &second] {
        assert!(run.stdout.contains(task.as_str()), "{}", run.stdout);
    }
}

#[test]
fn p4e_no_token_no_answer_or_no_daemon() {
    let mut setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    // Every step reads the context of client A.
    verified_task(&setup, "client-a", "P4-E failures");

    step("no token");
    let tokenless = HookEnv {
        token_file: None,
        ..env.clone()
    };
    statement_of(&setup, &tokenless, "client-a", "no usable token");

    step("a daemon that does not answer: the time limit");
    let silent = setup.root_path.join("silent");
    fs::create_dir_all(&silent).unwrap();
    let listener = UnixListener::bind(silent.join("memproved.sock")).unwrap();
    let mute = HookEnv {
        data_dir: silent.clone(),
        ..env.clone()
    };
    let run = statement_of(&setup, &mute, "client-a", "not read within 8 seconds");
    assert!(run.elapsed >= Duration::from_secs(4), "{:?}", run.elapsed);
    drop(listener);

    step("no daemon: the socket does not exist");
    let before = high_water(&setup);
    setup.stop();
    let run = hook(&env, &setup.repo, input("client-a", &setup.repo, "startup"));
    check(&run, &setup);
    assert!(
        statement_only(&run).contains("memproved is not running"),
        "{}",
        run.stdout
    );
    setup.start();
    assert_eq!(high_water(&setup), before);
}

#[test]
fn p4e_a_long_task_is_shortened_within_the_limit() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    let (long, _) = verified_task(&setup, "client-long", "long");
    let mut words = vec!["checkpoint".to_owned(), "create".to_owned()];
    for number in 0..8 {
        words.push("--fact".to_owned());
        words.push(format!("fact {number} holds: {}", "x".repeat(3000)));
    }
    words.extend(["--next".to_owned(), "n2".to_owned()]);
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    ok_as(&setup.agent, Some("client-long"), &words);
    let before = high_water(&setup);
    let run = hook(
        &env,
        &setup.repo,
        input("client-long", &setup.repo, "startup"),
    );
    check(&run, &setup);
    let pack = fenced(&run);
    assert!(run.stdout.contains(&format!("Task {long}")));
    assert!(
        pack.lines().any(|line| line.starts_with("Omitted: ")),
        "{pack}"
    );
    assert_eq!(high_water(&setup), before);
}

#[test]
fn p4e_a_pack_that_does_not_fit_is_one_statement_with_the_command() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    let huge = new_task(&setup.agent, &setup, "client-huge", &"T".repeat(12_000));
    let run = statement_of(&setup, &env, "client-huge", "does not fit");
    let statement = run.stdout.trim_end();
    let command = format!("memprove resume --task {huge} --pack --budget ");
    let needed = statement
        .split_once(&command)
        .and_then(|(_, rest)| {
            rest.split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .unwrap_or_else(|| panic!("no command in {statement}"));
    let full = as_client(
        &setup.agent,
        Some("client-huge"),
        &["resume", "--task", &huge, "--pack", "--budget", needed],
    );
    assert_eq!(full.code, Some(0), "{}", full.stderr);
}

#[test]
fn p4e_a_tampered_task_injects_no_verified() {
    let mut setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    let (task, _) = verified_task(&setup, "client-t", "tampered");
    let run = hook(&env, &setup.repo, input("client-t", &setup.repo, "startup"));
    assert!(says_verified(&fenced(&run)));
    setup.stop();
    setup.tamper(&format!(
        "UPDATE tasks SET revision = revision + 7 WHERE task_id = '{task}';"
    ));
    setup.start();
    let before = high_water(&setup);
    let run = hook(&env, &setup.repo, input("client-t", &setup.repo, "startup"));
    check(&run, &setup);
    let pack = fenced(&run);
    assert!(pack.contains("tampered"), "{pack}");
    assert!(!says_verified(&run.stdout), "{}", run.stdout);
    assert_eq!(high_water(&setup), before);
}

#[test]
fn p4e_standard_input_is_untrusted() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    verified_task(&setup, "client-a", "untrusted input");
    let before = high_water(&setup);
    let files = tree(&setup.root_path, &setup.agent.data_dir);
    let repo = setup.repo.to_str().unwrap();
    let marker = setup.root_path.join("pwned");
    let mut inputs: Vec<(String, Vec<u8>)> = [
        "a\"b",
        "a`touch pwned`b",
        "$(touch pwned)",
        "a\nb",
        "../../escape",
        "a/b",
        "",
        &"a".repeat(129),
    ]
    .iter()
    .map(|id| {
        (
            format!("session_id {id:?}"),
            input(id, &setup.repo, "startup"),
        )
    })
    .collect();
    inputs.extend([
        (
            "session_id of another type".to_owned(),
            format!(r#"{{"session_id":7,"cwd":"{repo}","source":"startup"}}"#).into_bytes(),
        ),
        (
            "no session_id".to_owned(),
            format!(r#"{{"cwd":"{repo}","source":"startup"}}"#).into_bytes(),
        ),
        (
            "cwd that does not exist".to_owned(),
            input("client-a", Path::new("/nonexistent/p4e"), "startup"),
        ),
        (
            "an unknown source".to_owned(),
            input("client-a", &setup.repo, "reboot"),
        ),
        ("not JSON".to_owned(), b"not json at all".to_vec()),
        ("deep nesting".to_owned(), "[".repeat(100_000).into_bytes()),
        ("one megabyte".to_owned(), {
            let mut big = br#"{"session_id":"client-a","pad":""#.to_vec();
            big.extend(std::iter::repeat_n(b'x', 1 << 20));
            big.extend(br#"","cwd":"/","source":"startup"}"#);
            big
        }),
    ]);
    for (label, bytes) in inputs {
        step(&label);
        let run = hook(&env, &setup.repo, bytes);
        check(&run, &setup);
        assert!(
            statement_only(&run).starts_with("memprove: the SessionStart input"),
            "{label}: {}",
            run.stdout
        );
        assert!(!env.env_file.exists(), "{label}: CLAUDE_ENV_FILE written");
        assert!(!marker.exists(), "{label}: a command ran");
        assert_eq!(
            tree(&setup.root_path, &setup.agent.data_dir),
            files,
            "{label}: a file was created"
        );
    }
    assert_eq!(high_water(&setup), before);
}

/// The line of `resume --text` (as `client`) that names `session`.
fn session_line(agent: &Cli, client: &str, session: &str) -> String {
    let lines = as_client(agent, Some(client), &["resume", "--text"]);
    assert_eq!(lines.code, Some(0), "{}", lines.stderr);
    lines
        .stdout
        .lines()
        .find(|line| line.contains(session))
        .unwrap_or_else(|| panic!("{session} not in {}", lines.stdout))
        .to_owned()
}

/// Two client sessions of one directory on one Task: each `begin` gets its
/// own Session in its own context file, and each `resume --text` labels its
/// own Session.
fn two_clients(setup: &Setup) -> (String, String, String) {
    let agent = &setup.agent;
    let (task, session_a) = verified_task(setup, "client-a", "shared");
    let begun_b = ok_as(
        agent,
        Some("client-b"),
        &["begin", "--project", &setup.project_id, "--task", &task],
    );
    let session_b = begun_b["session_id"].as_str().unwrap().to_owned();
    assert_ne!(session_a, session_b);
    (task, session_a, session_b)
}

#[test]
fn p4e_client_sessions_keep_their_own_context() {
    let setup = Setup::new(&["a"]);
    let agent = &setup.agent;
    step("two client sessions begin in the same directory");
    let (_, session_a, session_b) = two_clients(&setup);
    for (client, own, other) in [
        ("client-a", &session_a, &session_b),
        ("client-b", &session_b, &session_a),
    ] {
        let shown = as_client(agent, Some(client), &["context", "show"]);
        assert!(shown.stdout.contains(own.as_str()), "{}", shown.stdout);
        assert!(
            session_line(agent, client, own)
                .contains("(the Session this client session's context file records)")
        );
        assert!(!session_line(agent, client, other).contains("context file records"));
    }
    // Neither wrote the directory's own context file.
    let plain = fs::read_dir(agent.config_dir.join("contexts"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert!(plain.is_empty(), "{plain:?}");
    let shown = as_client(agent, None, &["context", "show"]);
    assert_eq!(shown.code, Some(2), "{}", shown.stdout);

    step("an invalid MEMPROVE_CLIENT_SESSION is ignored, with a warning");
    let invalid = as_client(agent, Some("not/valid"), &["context", "show"]);
    assert_eq!(invalid.code, Some(2));
    assert!(
        invalid.stderr.contains("MEMPROVE_CLIENT_SESSION"),
        "{}",
        invalid.stderr
    );
    assert!(!invalid.stderr.contains("not/valid"), "{}", invalid.stderr);
}

#[test]
fn p4e_claude_env_file_selects_the_client_context() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    let (_, session_a, session_b) = two_clients(&setup);
    let source = |expected: &str| {
        let output = isolated("bash")
            .arg("-c")
            .arg(r#". "$1" && exec "$2" context show"#)
            .arg("bash")
            .arg(&env.env_file)
            .arg(cli_binary())
            .current_dir(&setup.repo)
            .env("MEMPROVE_CONFIG_DIR", &setup.agent.config_dir)
            .output()
            .unwrap();
        let shown = text(&output.stdout);
        println!("$ bash -c '. $CLAUDE_ENV_FILE && memprove context show'\n{shown}");
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        assert!(shown.contains(expected), "{shown}");
    };
    for _ in 0..2 {
        let run = hook(&env, &setup.repo, input("client-a", &setup.repo, "resume"));
        check(&run, &setup);
    }
    assert_eq!(
        env_lines(&env),
        ["export MEMPROVE_CLIENT_SESSION='client-a'"]
    );
    source(&session_a);
    let run = hook(&env, &setup.repo, input("client-b", &setup.repo, "clear"));
    check(&run, &setup);
    source(&session_b);
}

#[test]
fn p4e_begin_after_the_hook_declares_the_injected_pack() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    let agent = &setup.agent;
    let (task, _, _) = two_clients(&setup);
    ok_as(agent, Some("client-a"), &["end", "--reason", "a done"]);
    ok_as(agent, Some("client-b"), &["end", "--reason", "b done"]);
    let before = high_water(&setup);
    let run = hook(&env, &setup.repo, input("client-c", &setup.repo, "startup"));
    check(&run, &setup);
    assert!(
        run.stdout
            .lines()
            .next()
            .unwrap()
            .contains("the only in-progress Task"),
        "{}",
        run.stdout
    );
    let pack = fenced(&run);
    assert_eq!(high_water(&setup), before);
    let begun = as_client(agent, Some("client-c"), &["begin"]);
    assert_eq!(begun.code, Some(0), "{}", begun.stderr);
    assert!(
        begun
            .stderr
            .contains("declares that it resumes from the Pack of ledger sequence"),
        "{}",
        begun.stderr
    );
    let begun: Value = serde_json::from_str(begun.stdout.trim()).unwrap();
    assert_eq!(begun["task_id"], task.as_str());
    let session_c = begun["session_id"].as_str().unwrap().to_owned();
    let capsule = ok_as(agent, Some("client-c"), &["resume"]);
    let declared = &session(&capsule, &session_c)["resumed_from"];
    let digest = pack
        .lines()
        .last()
        .unwrap()
        .strip_prefix("Digest: ")
        .unwrap();
    assert_eq!(declared["pack_digest"], digest);
    assert_eq!(declared["project_sequence"], before);
    assert_eq!(declared["budget"], 9000);
    let first = pack.lines().next().unwrap();
    assert!(
        first.contains(&format!(
            "capsule_digest {}; ledger sequence {before}; ",
            declared["capsule_digest"].as_str().unwrap()
        )),
        "{first}"
    );
    assert!(sequence(&capsule) > before);
}

/// P4-H1: every last-Pack file (`*.pack.json`) under the contexts of
/// `config_dir`, with its contents.
fn pack_files(config_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    let mut pending = vec![config_dir.join("contexts")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.to_string_lossy().ends_with(".pack.json") {
                let contents = fs::read_to_string(&path).unwrap();
                files.push((path, contents));
            }
        }
    }
    files.sort();
    files
}

/// U1 U-F (SF-1R): the last-Pack files `after` holds beyond `before`,
/// which must be unchanged: each new one a record of no Pack naming
/// `project` (the hook exported a client session it gave no Pack).
fn no_pack_records(
    before: &[(PathBuf, String)],
    after: &[(PathBuf, String)],
    project: &str,
) -> usize {
    assert!(
        before.iter().all(|file| after.contains(file)),
        "a Pack was recorded"
    );
    let added = after
        .iter()
        .filter(|file| !before.contains(file))
        .collect::<Vec<_>>();
    for (path, contents) in &added {
        assert_eq!(
            contents,
            &format!("{{\"no_pack\":true,\"project_id\":\"{project}\"}}\n"),
            "{}",
            path.display()
        );
    }
    added.len()
}

/// P4-H1: a Task begun in the context of `client` (the directory's own when
/// `None`) and finished as `state`: `completed` citing its passed run, or
/// `cancelled` through `blocked`. Its Session is ended. Returns the Task.
fn finished_task(setup: &Setup, client: Option<&str>, state: &str) -> String {
    let agent = &setup.agent;
    let title = format!("{state} Task");
    let begun = ok_as(
        agent,
        client,
        &[
            "begin",
            "--project",
            &setup.project_id,
            "--new-task",
            &title,
        ],
    );
    let task = begun["task_id"].as_str().unwrap().to_owned();
    if state == "completed" {
        let ran = as_client(
            agent,
            client,
            &["run", "--test-format", "cargo", "--", "sh", "tests/a.sh"],
        );
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        ok_as(agent, client, &["task", "complete"]);
        ok_as(agent, client, &["end", "--reason", "done"]);
    } else {
        ok_as(agent, client, &["end", "--reason", "stop"]);
        for to in ["blocked", "cancelled"] {
            ok_as(agent, client, &["task", "transition", &task, "--to", to]);
        }
    }
    let capsule = ok_as(agent, client, &["resume"]);
    assert_eq!(capsule["task"]["status"], state);
    task
}

/// P4-H1 (Owner decision): the directory's context names a Task that is
/// completed or cancelled. The hook injects no Pack of it and goes on with
/// the only Task in progress in this Workspace: none or several give one
/// statement naming the finished Task and its state; exactly one gets its
/// Pack, and the statement says why. Only a given Pack is recorded.
#[test]
fn p4h1_a_finished_task_of_the_directory_context_gives_way() {
    for state in ["completed", "cancelled"] {
        let setup = Setup::new(&["a"]);
        let env = HookEnv::of(&setup);
        let agent = &setup.agent;
        step(&format!("the directory's context names a {state} Task"));
        let finished = finished_task(&setup, None, state);
        let named = format!("the Task {finished} recorded by this directory's context is {state}");

        step("no Task in progress in this Workspace");
        let packs = pack_files(&agent.config_dir);
        let run = statement_of(&setup, &env, "fresh-0", &named);
        assert!(
            statement_only(&run).contains("this Workspace has no in-progress Task"),
            "{}",
            run.stdout
        );
        // U1 U-F (SF-1R): no Pack is recorded, but the client session the
        // hook exported gets a record of no Pack.
        let recorded = pack_files(&agent.config_dir);
        assert_eq!(no_pack_records(&packs, &recorded, &setup.project_id), 1);
        assert_eq!(
            env_lines(&env).last().map(String::as_str),
            Some("export MEMPROVE_CLIENT_SESSION='fresh-0'")
        );

        step("one Task in progress in this Workspace: its Pack");
        let one = new_task(agent, &setup, "client-x", "one");
        let expected = as_client(agent, Some("client-x"), &["resume", "--pack"]);
        assert_eq!(expected.code, Some(0), "{}", expected.stderr);
        let before = high_water(&setup);
        let packs = pack_files(&agent.config_dir);
        let run = hook(&env, &setup.repo, input("fresh-1", &setup.repo, "startup"));
        check(&run, &setup);
        let statement = run.stdout.lines().next().unwrap();
        assert!(statement.contains(&format!("Task {one}")), "{statement}");
        assert!(
            statement.contains("the only in-progress Task"),
            "{statement}"
        );
        assert!(statement.contains(&named), "{statement}");
        // U7 P1 (Owner O3, amended 2026-10-21): the hook's Pack is a new
        // client session's, for which client-x's open Session is someone
        // else's: it differs from client-x's own Pack only in the takeover
        // hint on that Session's line (before: the same bytes).
        only_the_takeover_hint(&fenced(&run), &expected.stdout);
        assert_eq!(high_water(&setup), before, "the hook wrote to the ledger");
        assert_eq!(
            pack_files(&agent.config_dir).len(),
            packs.len() + 1,
            "the given Pack was not recorded"
        );

        step("two Tasks in progress in this Workspace");
        let two = new_task(agent, &setup, "client-y", "two");
        let packs = pack_files(&agent.config_dir);
        let run = statement_of(&setup, &env, "fresh-2", &named);
        let statement = statement_only(&run);
        assert!(statement.contains("2 in-progress Tasks"), "{statement}");
        for task in [&one, &two] {
            assert!(statement.contains(task.as_str()), "{statement}");
        }
        let recorded = pack_files(&agent.config_dir);
        assert_eq!(no_pack_records(&packs, &recorded, &setup.project_id), 1);
    }
}

/// T21 (V3-F-1, fix round 2): the directory's context names a completed
/// Task and six Tasks are in progress in this Workspace, each begun by its
/// own client session (the re-verification's repro). The hook reads the
/// seven Capsules at once and prints, within its limit, the statement that
/// names the finished Task and lists the six (before: one Capsule after
/// another; under load it printed its timeout statement instead).
#[test]
fn t21_six_tasks_in_progress_after_a_completed_one_are_read_at_once() {
    let setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    let agent = &setup.agent;
    let finished = finished_task(&setup, None, "completed");
    let named = format!("the Task {finished} recorded by this directory's context is completed");
    let tasks = (1..=6)
        .map(|index| {
            new_task(
                agent,
                &setup,
                &format!("client-{index}"),
                &format!("T{index}"),
            )
        })
        .collect::<Vec<_>>();
    let run = statement_of(&setup, &env, "fresh", &named);
    let statement = statement_only(&run);
    assert!(statement.contains("6 in-progress Tasks"), "{statement}");
    for task in &tasks {
        assert!(statement.contains(task.as_str()), "{statement}");
    }
}

/// P4-H1: a client session's own context names a Task that is completed
/// or cancelled. The hook injects no Pack: one statement names the Task and
/// its state, even with another Task in progress in this Workspace.
#[test]
fn p4h1_a_finished_task_of_the_client_context_is_one_statement() {
    for state in ["completed", "cancelled"] {
        let setup = Setup::new(&["a"]);
        let env = HookEnv::of(&setup);
        let agent = &setup.agent;
        step(&format!("client A's context names a {state} Task"));
        let finished = finished_task(&setup, Some("client-a"), state);
        let other = new_task(agent, &setup, "client-b", "in progress");
        let packs = pack_files(&agent.config_dir);
        for source in SOURCES {
            step(&format!("SessionStart {source}"));
            let before = high_water(&setup);
            let run = hook(&env, &setup.repo, input("client-a", &setup.repo, source));
            check(&run, &setup);
            let statement = statement_only(&run);
            assert_eq!(
                statement,
                format!(
                    "memprove: the Task {finished} recorded by this client session's context is \
                     {state}; no Context Pack was injected into this session."
                )
            );
            assert!(!statement.contains(other.as_str()), "{statement}");
            assert_eq!(high_water(&setup), before, "the hook wrote to the ledger");
            assert_eq!(pack_files(&agent.config_dir), packs, "a Pack was recorded");
        }
        assert_eq!(
            env_lines(&env),
            ["export MEMPROVE_CLIENT_SESSION='client-a'"]
        );
    }
}

/// P4-H1: a Task whose row was rewritten to `completed` outside
/// `memproved` has a tampered Capsule. The hook does not trust that state:
/// it injects the Pack, which says tampered and never verified, as before.
#[test]
fn p4h1_a_tampered_task_that_reads_as_completed_keeps_its_pack() {
    let mut setup = Setup::new(&["a"]);
    let env = HookEnv::of(&setup);
    let (task, _) = verified_task(&setup, "client-t", "tampered as completed");
    setup.stop();
    setup.tamper(&format!(
        "UPDATE tasks SET status = 'completed' WHERE task_id = '{task}';"
    ));
    setup.start();
    let capsule = ok_as(&setup.agent, Some("client-t"), &["resume"]);
    assert_eq!(
        (&capsule["status"], &capsule["task"]["status"]),
        (&Value::from("tampered"), &Value::from("completed"))
    );
    let before = high_water(&setup);
    let run = hook(&env, &setup.repo, input("client-t", &setup.repo, "startup"));
    check(&run, &setup);
    let statement = run.stdout.lines().next().unwrap();
    assert!(statement.contains(&format!("Task {task}")), "{statement}");
    assert!(
        statement.contains("this client session's context"),
        "{statement}"
    );
    assert!(!statement.contains("context is "), "{statement}");
    let pack = fenced(&run);
    assert!(pack.contains("tampered"), "{pack}");
    assert!(!says_verified(&run.stdout), "{}", run.stdout);
    assert_eq!(high_water(&setup), before);
}
