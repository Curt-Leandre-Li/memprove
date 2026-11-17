//! `memprove verify` is served by the memprove-verify crate before the
//! CLI's parser: its help, its exit codes on a small repository, and its
//! summary line, which `memprove run --test-format cargo` counts.

use std::{
    fs,
    io::Write as _,
    path::Path,
    process::{Command, Output, Stdio},
};

use memprove_cli::run::cargo_case_count;

fn memprove(directory: &Path, words: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memprove"))
        .args(words)
        .current_dir(directory)
        .env_remove("MEMPROVE_CONFIG_DIR")
        .output()
        .expect("memprove starts")
}

fn git(directory: &Path, words: &[&str]) {
    let status = Command::new("git")
        .args(words)
        .current_dir(directory)
        .status()
        .expect("git starts");
    assert!(status.success(), "git {words:?}");
}

/// A repository whose one commit holds a contract (`make test`, which
/// wants `answer` to say 42, with `tests/**` protected) and `answer`
/// saying 0.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "--quiet", "-b", "main"]);
    git(root, &["config", "user.email", "verify@example.invalid"]);
    git(root, &["config", "user.name", "verify"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("Makefile"), "test:\n\tsh tests/answer.sh\n").expect("write");
    fs::create_dir(root.join("tests")).expect("mkdir");
    fs::write(root.join("tests/answer.sh"), "grep -qx 42 answer\n").expect("write");
    fs::write(root.join("answer"), "0\n").expect("write");
    let init = memprove(root, &["verify", "init"]);
    assert_eq!(init.status.code(), Some(0), "{init:?}");
    let said = String::from_utf8_lossy(&init.stdout);
    assert!(said.starts_with("wrote .memprove/verify.json\n"), "{said}");
    assert!(
        said.contains("checks with `make test` (from Makefile)"),
        "{said}"
    );
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "the task"]);
    dir
}

#[test]
fn verify_help_and_usage_errors() {
    let empty = tempfile::tempdir().expect("tempdir");
    let help = memprove(empty.path(), &["verify", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.starts_with("memprove verify: "), "{text}");
    for command in ["init", "run", "show", "hook"] {
        let output = memprove(empty.path(), &["verify", command, "--help"]);
        assert_eq!(output.status.code(), Some(0), "{command}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.starts_with(&format!("memprove verify {command}: ")),
            "{text}"
        );
        assert!(text.contains("\nExit status:\n"), "{command}");
    }
    for (words, error) in [
        (&["verify"][..], "Usage:"),
        (&["verify", "check"], "unknown command \"check\""),
        (
            &["verify", "run", "--format", "xml"],
            "--format must be text or json, not \"xml\"",
        ),
        (&["verify", "run", "--strict"], "--strict needs a value"),
        (&["verify", "run", "extra"], "unexpected argument \"extra\""),
        (&["verify", "init", "--base", "x"], "unknown option --base"),
    ] {
        let output = memprove(empty.path(), words);
        assert_eq!(output.status.code(), Some(2), "{words:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(error), "{words:?}: {stderr}");
    }
    // Outside a repository there is nothing to verify: an error, not usage.
    let output = memprove(empty.path(), &["verify", "run"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let general = memprove(empty.path(), &["--help"]);
    assert!(
        String::from_utf8_lossy(&general.stdout)
            .contains("\nCompletion check (memprove verify --help):\n")
    );
}

#[test]
fn verify_run_exits_by_verdict_and_show_prints_the_last_run() {
    let repo = repository();
    let root = repo.path();

    // Not done: the check fails on the clean copy.
    let failed = memprove(root, &["verify", "run"]);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let text = String::from_utf8_lossy(&failed.stdout);
    assert!(text.starts_with("memprove verify: FAIL\n"), "{text}");
    assert!(
        text.contains("why       check `tests` (make test) exited with code 2"),
        "{text}"
    );
    let last = text.lines().last().expect("a line");
    assert!(
        last.starts_with("test result: FAILED. 0 passed; 1 failed;"),
        "{last}"
    );
    assert_eq!(cargo_case_count(text.as_bytes(), b"", 1), Some(1));

    // Passing by weakening the protected test is left to a person.
    fs::write(root.join("tests/answer.sh"), "true\n").expect("write");
    let weakened = memprove(root, &["verify", "run", "--format", "json"]);
    assert_eq!(weakened.status.code(), Some(4), "{weakened:?}");
    let receipt = String::from_utf8_lossy(&weakened.stdout);
    assert!(
        receipt.contains("\"schema\": \"memprove.verify.receipt.v1\""),
        "{receipt}"
    );
    assert!(
        receipt.contains("\"verdict\": \"needs_human\""),
        "{receipt}"
    );
    assert!(
        receipt.contains("\"kind\": \"protected_modified\""),
        "{receipt}"
    );
    git(root, &["checkout", "--quiet", "--", "tests/answer.sh"]);

    // Done.
    fs::write(root.join("answer"), "42\n").expect("write");
    let passed = memprove(root, &["verify", "run"]);
    assert_eq!(passed.status.code(), Some(0), "{passed:?}");
    let text = String::from_utf8_lossy(&passed.stdout);
    assert!(text.starts_with("memprove verify: PASS\n"), "{text}");
    assert!(text.ends_with("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"), "{text}");

    let shown = memprove(root, &["verify", "show"]);
    assert_eq!(shown.status.code(), Some(0));
    let summary = String::from_utf8_lossy(&shown.stdout);
    assert!(summary.starts_with("memprove verify: PASS\n"), "{summary}");
    assert!(
        text.starts_with(summary.as_ref()),
        "show prints what run printed"
    );
    let shown = memprove(root, &["verify", "show", "latest", "--format", "json"]);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("\"verdict\": \"pass\""));

    // Nothing was written into the worktree.
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .expect("git");
    assert_eq!(String::from_utf8_lossy(&status.stdout), " M answer\n");
}

#[test]
fn verify_hook_reads_the_event_and_always_exits_0() {
    let repo = repository();
    let root = repo.path();
    let hook = |event: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_memprove"))
            .args(["verify", "hook"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("memprove starts");
        let input = format!(r#"{{"session_id":"cli-1","hook_event_name":"{event}"}}"#);
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write");
        let output = child.wait_with_output().expect("ends");
        assert_eq!(output.status.code(), Some(0), "{event}: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    assert_eq!(hook("SessionStart"), "");
    assert_eq!(hook("Stop"), "", "the tree is the base's");
    fs::write(root.join("answer"), "41\n").expect("write");
    let held = hook("Stop");
    assert!(held.contains("\"decision\": \"block\""), "{held}");
    assert!(held.contains("stop 1 of 3"), "{held}");
    assert_eq!(hook("Notification"), "");

    let installed = memprove(root, &["verify", "init", "--hook"]);
    assert_eq!(installed.status.code(), Some(0), "{installed:?}");
    let said = String::from_utf8_lossy(&installed.stdout);
    assert!(said.contains("kept .memprove/verify.json"), "{said}");
    assert!(said.contains("wrote .claude/settings.local.json"), "{said}");
    let settings = fs::read_to_string(root.join(".claude/settings.local.json")).expect("settings");
    assert_eq!(
        settings.matches(env!("CARGO_BIN_EXE_memprove")).count(),
        2,
        "{settings}"
    );
}
