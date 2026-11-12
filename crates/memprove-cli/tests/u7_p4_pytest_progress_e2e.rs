//! U7 P4 (D2 F12, Owner O6; ADR 0015 "Amendment (U7, W-P)"), through the
//! real `memprove` and `memproved`: `run --test-format pytest` of a
//! pytest that printed no summary (`-qq`, or `-q` in `addopts` on top of
//! `-q`) counts the last line of progress marks, says so in the run's
//! record on standard error, and passes when no mark is a failure and the
//! exit code is 0; a last line that is neither a summary nor progress marks
//! records no count (`NOT_RUN`) and says why pytest printed none.
//! `--test-cases` still wins, with U5 K2's warning.

#[path = "u6_ui_support/mod.rs"]
mod support;

use std::process::Output;

use memprove_protocol::strict_json::{JsonValue, parse_strict};
use support::{Fixture, text};

/// `memprove run --test-format pytest <options> -- sh -c '<script>'`.
fn run(fixture: &Fixture, options: &[&str], script: &str) -> Output {
    let mut words = vec!["run", "--test-format", "pytest"];
    words.extend_from_slice(options);
    words.extend(["--", "sh", "-c", script]);
    let output = fixture.agent(&words);
    println!(
        "$ memprove {}\n{}[exit {:?}]",
        words.join(" "),
        text(&output.stderr),
        output.status.code()
    );
    output
}

/// The record's status (whatever the command's exit code) and the last
/// line of standard error.
fn recorded(output: &Output) -> (String, String) {
    let record = parse_strict(text(&output.stdout).trim()).unwrap();
    let status = record
        .get("status")
        .and_then(JsonValue::as_str)
        .unwrap()
        .to_owned();
    let stderr = text(&output.stderr);
    (status, stderr.lines().last().unwrap().to_owned())
}

#[test]
fn u7_p4_a_pytest_progress_line_is_counted_when_there_is_no_summary() {
    let fixture = Fixture::new();
    fixture.agent_ok(&[
        "begin",
        "--project",
        &fixture.project,
        "--new-task",
        "P4 pytest -qq",
    ]);

    // D2's case: only dots, with the percentage.
    let output = run(&fixture, &[], "echo '..... [100%]'");
    assert_eq!(output.status.code(), Some(0));
    let (status, last) = recorded(&output);
    assert_eq!(status, "PASS");
    assert!(
        last.contains("PASS, 5 test cases (counted from the progress line), evidence_id "),
        "{last}"
    );
    assert!(
        text(&output.stderr).contains(
            "memprove: counted 5 test cases from the pytest progress line (counted from the progress line)"
        )
    );

    // A failure mark, and pytest's exit code 1: a failed run of 5 cases.
    let output = run(&fixture, &[], "echo '..F.. [100%]'; exit 1");
    assert_eq!(output.status.code(), Some(1));
    let (status, last) = recorded(&output);
    assert_eq!(status, "FAIL");
    assert!(
        last.contains(", 5 test cases (counted from the progress line), "),
        "{last}"
    );

    // U8 K3 (U7 V-N5): skipped and xfail marks are not cases, as in a
    // summary: `..s.x` is three cases.
    let output = run(&fixture, &[], "echo '..s.x'");
    assert_eq!(recorded(&output).0, "PASS");
    assert!(
        recorded(&output)
            .1
            .contains(", 3 test cases (counted from the progress line), ")
    );
    assert!(
        text(&output.stderr).contains(
            "memprove: counted 3 test cases from the pytest progress line (counted from the progress line)"
        )
    );

    // A line of only skipped marks is zero cases and does not pass, as
    // `2 skipped` does not; a failure among skips is a failed run of the
    // cases that ran.
    let output = run(&fixture, &[], "echo 'ss [100%]'");
    let (status, last) = recorded(&output);
    assert_ne!(status, "PASS", "{last}");
    assert!(
        last.contains(", 0 test cases (counted from the progress line), "),
        "{last}"
    );
    let output = run(&fixture, &[], "echo '.s.F'; exit 1");
    let (status, last) = recorded(&output);
    assert_eq!(status, "FAIL");
    assert!(
        last.contains(", 3 test cases (counted from the progress line), "),
        "{last}"
    );

    // No summary and no progress line: no count, and why.
    let output = run(&fixture, &[], "echo 'all good'");
    let (status, last) = recorded(&output);
    assert_ne!(status, "PASS");
    assert!(last.contains(", no test case count, "), "{last}");
    assert!(
        text(&output.stderr).contains(
            "memprove: could not count the test cases in the output; pytest printed no summary \
             (`-qq`, or a `-q` in `addopts`, removes it); the run is recorded without a case count \
             and does not pass"
        ),
        "{}",
        text(&output.stderr)
    );

    // A summary is read as before, with no note.
    let output = run(
        &fixture,
        &[],
        "echo '..... [100%]'; echo '5 passed in 0.1s'",
    );
    let (status, last) = recorded(&output);
    assert_eq!(status, "PASS");
    assert!(last.contains(", 5 test cases, evidence_id "), "{last}");
    assert!(text(&output.stderr).contains("memprove: counted 5 test cases\n"));

    // --test-cases wins; U5 K2's warning names the progress line.
    let output = run(
        &fixture,
        &["--test", "--test-cases", "3"],
        "echo '..... [100%]'",
    );
    assert!(
        text(&output.stderr).contains(
            "memprove: warning: --test-cases 3 differs from the 5 test cases counted from the \
             pytest progress line (counted from the progress line); 3 is recorded"
        ),
        "{}",
        text(&output.stderr)
    );
    let (status, last) = recorded(&output);
    assert_eq!(status, "PASS");
    assert!(last.contains(", 3 test cases, "), "{last}");
    fixture.end();
    fixture.stop();
}
