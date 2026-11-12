//! U6 K2 (U5 N7): `memprove run --test-format pytest --test-cases <N>` with
//! a readable pytest summary whose count differs from `<N>` writes one
//! warning line on standard error naming both numbers; the value given still
//! wins (it is what the request carries). A consistent count, a summary that
//! cannot be read, and a run with no `--test-format` write no warning.
//!
//! No daemon is needed: the warning comes after the command ran and before
//! the result is sent, and the result then fails to reach a socket nobody
//! listens on. The token file holds a made-up token; it is never sent.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

#[path = "u1e_support/mod.rs"]
mod u1e_support;

use u1e_support::{cli_binary, isolated, text};

const ID: &str = "018f47a0-0000-7000-8000-000000000001";

/// A short temporary directory (the socket path holds at most 103 bytes):
/// under `TMPDIR` when that is short, else under `/tmp`.
fn short_tempdir() -> (tempfile::TempDir, PathBuf) {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .filter(|base| base.is_absolute() && base.as_os_str().len() <= 24)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = tempfile::Builder::new()
        .prefix("u6")
        .tempdir_in(base)
        .unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

struct Place {
    _guard: tempfile::TempDir,
    root: PathBuf,
}

impl Place {
    fn new() -> Self {
        let (guard, root) = short_tempdir();
        let token = root.join("token");
        fs::write(&token, "cty_U6K2MADEUPTOKEN\n").unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _guard: guard,
            root,
        }
    }

    /// `memprove run <options> -- sh -c 'echo "<line>"'`.
    fn run(&self, options: &[&str], line: &str) -> Output {
        let mut command: Command = isolated(cli_binary());
        let data = self.root.join("nodaemon");
        let token = self.root.join("token");
        command
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .arg("--data-dir")
            .arg(&data)
            .arg("--token-file")
            .arg(&token)
            .args([
                "run",
                "--session",
                ID,
                "--workspace",
                ID,
                "--task",
                ID,
                "--test",
            ])
            .args(options)
            .args(["--", "sh", "-c", &format!("echo '{line}'")]);
        let output = command.output().unwrap();
        println!(
            "$ memprove run {options:?} -- {line}\n{}{}[exit {:?}]",
            text(&output.stdout),
            text(&output.stderr),
            output.status.code()
        );
        output
    }
}

/// The warning lines on standard error.
fn warnings(output: &Output) -> Vec<String> {
    text(&output.stderr)
        .lines()
        .filter(|line| line.starts_with("memprove: warning:"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn u6_k2_a_count_that_differs_from_the_pytest_summary_is_warned_about() {
    let place = Place::new();
    let summary = "=== 5 passed in 0.12s ===";

    // Inconsistent: one warning naming both numbers; the explicit value wins
    // (no "counted" line replaces it).
    let output = place.run(&["--test-format", "pytest", "--test-cases", "3"], summary);
    assert_eq!(
        warnings(&output),
        vec![
            "memprove: warning: --test-cases 3 differs from the 5 test cases in the pytest \
             summary; 3 is recorded"
                .to_owned()
        ]
    );
    assert!(!text(&output.stderr).contains("counted "));

    // Consistent: no warning.
    let output = place.run(&["--test-format", "pytest", "--test-cases", "5"], summary);
    assert_eq!(warnings(&output), Vec::<String>::new());

    // Failures count as cases, as they do without --test-cases.
    let output = place.run(
        &["--test-format", "pytest", "--test-cases", "5"],
        "=== 1 failed, 4 passed in 0.12s ===",
    );
    assert_eq!(warnings(&output), Vec::<String>::new());

    // No readable summary: no warning, and the value given is used as before.
    for unreadable in ["no summary here", "5 tests did a thing"] {
        let output = place.run(
            &["--test-format", "pytest", "--test-cases", "3"],
            unreadable,
        );
        assert_eq!(warnings(&output), Vec::<String>::new(), "{unreadable}");
    }

    // No --test-format: the output is not read, so there is nothing to compare.
    let output = place.run(&["--test-cases", "3"], summary);
    assert_eq!(warnings(&output), Vec::<String>::new());

    // Without --test-cases the count comes from the summary, as before.
    let output = place.run(&["--test-format", "pytest"], summary);
    assert_eq!(warnings(&output), Vec::<String>::new());
    assert!(text(&output.stderr).contains("memprove: counted 5 test cases"));
}
