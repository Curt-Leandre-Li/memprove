//! U8 K8 (U7 report, section 14, r20): the wait of the test harness for a
//! daemon to become ready (`p4b_support::Daemon::start`) is 90 seconds, not
//! 15, unless `MEMPROVE_TEST_READY_TIMEOUT_SECS` says otherwise (whole
//! seconds, at least 1; anything else is ignored), and a daemon that never
//! becomes ready is reported with the last 20 lines of its log. A heavily
//! loaded machine made the 15 seconds fail now and then.
//!
//! Test helper only: no product code changes with it.

mod p4b_support;

use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    process::Command,
    time::Duration,
};

use p4b_support::{
    Cli, Daemon, READY_TIMEOUT_VARIABLE, log_tail, ready_failure_message, ready_timeout,
    ready_timeout_from,
};

#[test]
fn u8_k8_the_wait_is_ninety_seconds_unless_the_variable_says_otherwise() {
    assert_eq!(ready_timeout_from(None), Duration::from_secs(90));
    assert_eq!(ready_timeout_from(Some("1")), Duration::from_secs(1));
    assert_eq!(ready_timeout_from(Some(" 300 ")), Duration::from_secs(300));
    // Not a whole number of seconds of at least 1: ignored.
    for ignored in ["", "0", "-5", "1.5", "ten", "1s"] {
        assert_eq!(
            ready_timeout_from(Some(ignored)),
            Duration::from_secs(90),
            "{ignored:?}"
        );
    }
}

/// `ready_timeout` reads the variable of its own process. A test cannot set
/// it for itself, so this runs the inner test below in a child process with
/// the variable set to 1, and in another without it.
#[test]
fn u8_k8_the_process_reads_the_variable() {
    let inner = |value: Option<&str>| {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "u8_k8_inner_reports_the_wait",
                "--ignored",
                "--nocapture",
            ])
            .env_remove(READY_TIMEOUT_VARIABLE);
        if let Some(value) = value {
            command.env(READY_TIMEOUT_VARIABLE, value);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(inner(Some("1")).contains("ready wait: 1 s"));
    assert!(inner(None).contains("ready wait: 90 s"));
}

#[test]
#[ignore = "run by u8_k8_the_process_reads_the_variable in a child process"]
fn u8_k8_inner_reports_the_wait() {
    println!("ready wait: {} s", ready_timeout().as_secs());
}

#[test]
fn u8_k8_the_message_carries_the_last_twenty_lines_of_the_log() {
    let log = (1..=30)
        .map(|n| format!("line {n}\n"))
        .collect::<Vec<_>>()
        .concat();
    let tail = log_tail(&log, 20);
    assert_eq!(tail.lines().count(), 20);
    assert!(tail.starts_with("line 11\n"), "{tail}");
    assert!(tail.ends_with("line 30\n"), "{tail}");
    // A shorter log is whole; no final line break is added or lost.
    assert_eq!(log_tail("a\nb\n", 20), "a\nb\n");
    assert_eq!(log_tail("a\nb", 20), "a\nb");
    assert_eq!(log_tail("", 20), "");
    let message = ready_failure_message(&log);
    assert!(message.starts_with("memproved never became ready:\n"));
    assert!(message.contains("line 30\n") && !message.contains("line 10\n"));
}

/// A daemon that cannot start (its data directory is a file) never answers
/// `health ready`; with a wait of one second the harness fails with that
/// message and the log of the daemon in it.
#[test]
fn u8_k8_a_daemon_that_never_becomes_ready_is_reported_with_its_log() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let blocker = root.join("not-a-directory");
    fs::write(&blocker, "a file\n").unwrap();
    let cli = Cli {
        data_dir: blocker.join("data"),
        config_dir: root.join("config"),
        token: root.join("token"),
        cwd: root.clone(),
        label: "k8",
    };
    let log = root.join("daemon.log");
    let panic = catch_unwind(AssertUnwindSafe(|| {
        Daemon::start_within(&cli, &log, Duration::from_secs(1));
    }))
    .expect_err("a daemon that cannot start is not ready");
    let message = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
        .unwrap();
    let written = fs::read_to_string(&log).unwrap();
    assert!(!written.is_empty(), "the daemon wrote nothing to its log");
    assert!(
        message.starts_with("memproved never became ready:\n"),
        "{message}"
    );
    assert!(
        message.ends_with(log_tail(&written, 20).as_str()),
        "{message}"
    );
}
