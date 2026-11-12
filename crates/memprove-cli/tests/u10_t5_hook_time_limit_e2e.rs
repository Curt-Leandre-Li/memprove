//! U10 T5 (Owner O4; VN21, the hook's part; ADR 0018 "Amendment (U10)"
//! Part 1), through the real `memprove`: the `SessionStart` hook reads the
//! Pack within its own limit (`hook::TIME_LIMIT`, Owner condition o): 8
//! seconds since U15 T1 (Owner O2; U14-N1; ADR 0014 "Amendment (U15)"), 5
//! until then. Against a daemon that accepts the connection and never
//! answers, it prints only `memprove: the Context Pack was not read within 8 seconds; no
//! Context Pack was injected into this session; running memprove resume --pack in this
//! session fetches it.`, exits 0 after
//! about 8 seconds (within the 10 seconds of the handler's `timeout` that
//! `hook install` writes, 2 of which stay for printing and exiting), and
//! writes no token. The guide says so, and how to reproduce it.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::{
    io::Write as _,
    os::unix::net::UnixListener,
    process::Stdio,
    time::{Duration, Instant},
};

use support::{Fixture, cli_binary, isolated, text};

const STATEMENT: &str = "memprove: the Context Pack was not read within 8 seconds; no Context Pack was \
                         injected into this session; running memprove resume --pack in this \
                         session fetches it.\n";

#[test]
fn u10_t5_a_silent_daemon_gives_the_time_limit_statement_within_the_limit() {
    let fixture = Fixture::new();
    fixture.task_with_fact("T5 hook limit", "the check passes");
    // A data directory whose socket accepts and never answers.
    let silent = fixture.root.join("m");
    std::fs::create_dir(&silent).unwrap();
    let listener = UnixListener::bind(silent.join("memproved.sock")).unwrap();
    let mut child = isolated(cli_binary())
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.root)
        .env("MEMPROVE_CONFIG_DIR", &fixture.config)
        .env("MEMPROVE_DATA_DIR", &silent)
        .args(["hook", "session-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let input = format!(
        r#"{{"hook_event_name":"SessionStart","session_id":"t5-client","source":"startup","cwd":"{}"}}"#,
        fixture.repo.display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let elapsed = started.elapsed();
    drop(listener);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(text(&output.stdout), STATEMENT);
    // U15 T1: the limit is 8 seconds; the handler's `timeout` is 10.
    assert!(
        elapsed >= Duration::from_millis(7_900) && elapsed < Duration::from_secs(10),
        "{elapsed:?}"
    );
    assert!(!text(&output.stderr).contains(&fixture.agent_secret));
    assert!(!text(&output.stdout).contains(&fixture.agent_secret));

    // The guide states the limit, what the client sees, and how to
    // reproduce it.
    let guide = include_str!("../../../docs/guide.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for words in [
        "**The hook's time limit.**",
        "memprove: the Context Pack was not read within 8 seconds; no Context Pack was injected into this session; running memprove resume --pack in this session fetches it.",
        "the handler's own `timeout` of 10 seconds",
        "about 8 seconds after it started",
        "memprove resume --pack",
        "MEMPROVE_DATA_DIR=/tmp/mute memprove hook session-start",
    ] {
        assert!(guide.contains(words), "the guide lacks {words:?}");
    }
    fixture.stop();
}
