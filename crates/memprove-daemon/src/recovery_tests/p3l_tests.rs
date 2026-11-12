//! Narrow tests of P3-L stage A (the findings of T15) through the real
//! handlers, store, and Git probes:
//!
//! - L1 (T15 S-1): the `400` detail of a `verified_fact` that cites an
//!   overruled passed run names that citation by its position in the
//!   request, with its Evidence ID and the later run, one by one.
//! - L2 (T15 N1-R7): a test Evidence also needs the latest test run of the
//!   same argv in the same state to pass; a later command run does not
//!   restore it. The Preview, the Capsule, Task completion, and the write
//!   check agree.
//! - L3 (T15 N3): `transitionTask` to `completed` first applies the Task
//!   Capsule's integrity check and refuses a `tampered` Task (`409
//!   evidence_stale`), whatever Evidence it cites, without changing the Task
//!   or writing an Event.
//! - L7 (stage B): each Session of the Task Capsule carries
//!   `last_recorded_at`, the `recorded_at` of its latest Event in the
//!   ledger; the same ledger gives the same Capsule bytes.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, completion, integrity_tests::assert_tampered_without_verified, ok, problem};
use crate::{
    api::WireResponse,
    catalog::{TAMPERED_COMPLETION, list_tasks},
};

fn id(run: &Value) -> &str {
    run["evidence_id"].as_str().unwrap()
}

fn snapshot(run: &Value) -> &str {
    run["snapshot"]["snapshot_id"].as_str().unwrap()
}

fn detail(response: &WireResponse) -> String {
    serde_json::from_str::<Value>(&response.body).unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

impl Fixture {
    /// Records a one-case test run of `argv` that exited `exit_code`, in the
    /// fixture's Task, Session, and Workspace.
    fn l_test_run(&self, argv: &[&str], exit_code: i32) -> Value {
        let mut body = self.run_body("test_run", exit_code, Some(1));
        body["metadata"]["argv"] = json!(argv);
        self.capture(&body)
    }

    /// `createCheckpoint` over `snapshot` with one `verified_fact` citing
    /// `evidence` in that order.
    fn l_fact(&self, snapshot: &str, evidence: &[&str]) -> WireResponse {
        self.checkpoint_with(
            &self.writer,
            &self.key(),
            &self.checkpoint_body(snapshot, evidence),
        )
    }
}

/// The position part of a detail: every `evidence_ids[j]` it names.
fn named_positions(detail: &str) -> Vec<usize> {
    detail
        .match_indices("$.verified_completed_work[0].evidence_ids[")
        .map(|(at, prefix)| {
            let rest = &detail[at + prefix.len()..];
            rest[..rest.find(']').unwrap()].parse().unwrap()
        })
        .collect()
}

/// L1 (T15 S-1, `repro_detail_index.py`): three passed test runs of three
/// commands in one state, then a failed run of the first command, which
/// overrules its pass. A `verified_fact` citing the overruled pass first,
/// in the middle, and last is refused with a detail that names exactly
/// that position, the overruled Evidence, and the later run. Evidence IDs
/// are time-ordered, so the request order differs from the stored
/// (ascending) order in every case but the first.
#[test]
fn an_overruled_citation_is_named_by_its_request_position() {
    let fixture = Fixture::new();
    let a = fixture.l_test_run(&["sh", "-c", "check a"], 0);
    let b = fixture.l_test_run(&["sh", "-c", "check b"], 0);
    let c = fixture.l_test_run(&["sh", "-c", "check c"], 0);
    let a_failed = fixture.l_test_run(&["sh", "-c", "check a"], 1);
    let over = snapshot(&a_failed);
    for (case, evidence, position) in [
        ("first", [id(&a), id(&b), id(&c)], 0),
        ("middle", [id(&b), id(&a), id(&c)], 1),
        ("last", [id(&c), id(&b), id(&a)], 2),
    ] {
        let refused = fixture.l_fact(over, &evidence);
        problem(
            &refused,
            OperationId::CreateCheckpoint,
            400,
            "invalid_request",
        );
        let detail = detail(&refused);
        assert_eq!(named_positions(&detail), [position], "{case}: {detail}");
        assert!(
            detail.starts_with(&format!(
                "1 verified_fact citation names an overruled passed run: (1) \
                 $.verified_completed_work[0].evidence_ids[{position}]. (1) test run Evidence \
                 {} passed;",
                id(&a)
            )),
            "{case}: {detail}"
        );
        assert!(detail.contains(id(&a_failed)), "{case}: {detail}");
        for standing in [id(&b), id(&c)] {
            assert!(!detail.contains(standing), "{case}: {detail}");
        }
    }
    // The citation without the overruled pass is accepted.
    ok(
        &fixture.l_fact(over, &[id(&c), id(&b)]),
        OperationId::CreateCheckpoint,
        201,
    );
}

/// L1: two overruled passes in one request are both named, each with its
/// own position, Evidence, and later run, in request order.
#[test]
fn two_overruled_citations_are_both_named() {
    let fixture = Fixture::new();
    let a = fixture.l_test_run(&["sh", "-c", "check a"], 0);
    let b = fixture.l_test_run(&["sh", "-c", "check b"], 0);
    let c = fixture.l_test_run(&["sh", "-c", "check c"], 0);
    let a_failed = fixture.l_test_run(&["sh", "-c", "check a"], 1);
    let c_failed = fixture.l_test_run(&["sh", "-c", "check c"], 1);
    let refused = fixture.l_fact(snapshot(&c_failed), &[id(&c), id(&b), id(&a)]);
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    let detail = detail(&refused);
    assert_eq!(named_positions(&detail), [0, 2], "{detail}");
    // P3-L3 (T16 S-1): the positions first, then each citation in full.
    assert!(
        detail.starts_with(
            "2 verified_fact citations name overruled passed runs: (1) \
             $.verified_completed_work[0].evidence_ids[0], (2) \
             $.verified_completed_work[0].evidence_ids[2]. "
        ),
        "{detail}"
    );
    let first = detail
        .find(&format!("(1) test run Evidence {} passed;", id(&c)))
        .unwrap_or_else(|| panic!("{detail}"));
    let second = detail
        .find(&format!("(2) test run Evidence {} passed;", id(&a)))
        .unwrap_or_else(|| panic!("{detail}"));
    assert!(first < second, "{detail}");
    // Each citation names its own later run.
    assert!(
        detail[first..second].contains(id(&c_failed))
            && !detail[first..second].contains(id(&a_failed)),
        "{detail}"
    );
    assert!(detail[second..].contains(id(&a_failed)), "{detail}");
    assert!(!detail.contains(id(&b)), "{detail}");
    assert!(detail.chars().count() <= 512, "{detail}");
}

/// One run of the default argv (`cargo test`) for L2: a test with
/// `cases` cases, or a command run, exiting 0.
enum Kind {
    Test(u64),
    Command,
}

impl Fixture {
    fn l_same_argv(&self, kind: &Kind) -> Value {
        let body = match kind {
            Kind::Test(cases) => self.run_body("test_run", 0, Some(*cases)),
            Kind::Command => self.run_body("command_run", 0, None),
        };
        self.capture(&body)
    }

    /// The verdict and reasons of the first fact of the Preview and of the
    /// Capsule claim of `text`.
    fn l_judged(&self, text: &str) -> (Value, Value, Value) {
        let (_, preview) = self.preview(None);
        let (_, capsule) = self.capsule(None);
        let claim = capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|claim| claim["text"] == text)
            .unwrap_or_else(|| panic!("{capsule}"))
            .clone();
        (
            preview["facts"][0]["verdict"].clone(),
            preview["facts"][0]["evidence"][0]["reasons"].clone(),
            claim["verdict"].clone(),
        )
    }
}

/// L2, first sequence (T15 N1-R7): a passed test, then a test with no case
/// (`NOT_RUN`), then a passed command run, all of one argv in one state.
/// The latest run passed, but the latest test run did not: the first test
/// is no longer `verified` (the Preview, the Capsule, Task completion), and
/// a new `verified_fact` on it is refused, naming the zero-case run.
#[test]
fn a_later_test_without_cases_overrules_a_test_pass_despite_a_later_command_pass() {
    let fixture = Fixture::new();
    fixture.step(&fixture.task_id, 1, "ready");
    fixture.step(&fixture.task_id, 2, "in_progress");
    let passed = fixture.l_same_argv(&Kind::Test(1));
    assert_eq!(passed["status"], "PASS");
    ok(
        &fixture.l_fact(snapshot(&passed), &[id(&passed)]),
        OperationId::CreateCheckpoint,
        201,
    );
    assert_eq!(
        fixture.l_judged("tests pass"),
        (json!("verified"), json!([]), json!("verified"))
    );
    let empty = fixture.l_same_argv(&Kind::Test(0));
    assert_eq!(empty["status"], "NOT_RUN");
    let command = fixture.l_same_argv(&Kind::Command);
    assert_eq!(command["status"], "PASS");
    assert_eq!(
        fixture.l_judged("tests pass"),
        (
            json!("partial"),
            json!(["evidence_not_passed"]),
            json!("partial")
        )
    );
    let refused = fixture.l_fact(snapshot(&command), &[id(&passed)]);
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    let detail = detail(&refused);
    // P3-L3 (T16 S-1 item 2): the zero-case test run, not the command run,
    // and a passed test run is what it needs.
    assert!(
        detail.contains(&format!(
            "(1) test run Evidence {} passed; the latest test run {} did not pass: it needs \
             a later passed test run.",
            id(&passed),
            id(&empty)
        )) && !detail.contains(id(&command)),
        "{detail}"
    );
    problem(
        &fixture.transition_with(
            &fixture.task_id,
            &fixture.key(),
            &completion(3, &[id(&passed)]),
        ),
        OperationId::TransitionTask,
        409,
        "evidence_stale",
    );
}

/// L2, second sequence: a passed test, then a passed command run of the same
/// argv. The test is still the latest test run, so a fact on it is
/// `verified` and completes the Task.
#[test]
fn a_later_command_pass_keeps_a_test_pass_verified() {
    let fixture = Fixture::new();
    fixture.step(&fixture.task_id, 1, "ready");
    fixture.step(&fixture.task_id, 2, "in_progress");
    let passed = fixture.l_same_argv(&Kind::Test(1));
    let command = fixture.l_same_argv(&Kind::Command);
    assert_eq!(command["status"], "PASS");
    ok(
        &fixture.l_fact(snapshot(&command), &[id(&passed)]),
        OperationId::CreateCheckpoint,
        201,
    );
    assert_eq!(
        fixture.l_judged("tests pass"),
        (json!("verified"), json!([]), json!("verified"))
    );
    let done = ok(
        &fixture.transition_with(
            &fixture.task_id,
            &fixture.key(),
            &completion(3, &[id(&passed)]),
        ),
        OperationId::TransitionTask,
        200,
    );
    assert_eq!(done["state"], "completed");
}

/// L2, third sequence: a test with no case, then a passed test of the same
/// argv. The latest test run passed, so a fact on it is `verified` and
/// completes the Task.
#[test]
fn a_test_pass_after_a_test_without_cases_is_verified() {
    let fixture = Fixture::new();
    fixture.step(&fixture.task_id, 1, "ready");
    fixture.step(&fixture.task_id, 2, "in_progress");
    let empty = fixture.l_same_argv(&Kind::Test(0));
    assert_eq!(empty["status"], "NOT_RUN");
    let passed = fixture.l_same_argv(&Kind::Test(1));
    ok(
        &fixture.l_fact(snapshot(&passed), &[id(&passed)]),
        OperationId::CreateCheckpoint,
        201,
    );
    assert_eq!(
        fixture.l_judged("tests pass"),
        (json!("verified"), json!([]), json!("verified"))
    );
    let done = ok(
        &fixture.transition_with(
            &fixture.task_id,
            &fixture.key(),
            &completion(3, &[id(&passed)]),
        ),
        OperationId::TransitionTask,
        200,
    );
    assert_eq!(done["state"], "completed");
}

impl Fixture {
    /// The fixture Task's state and revision, read with `listTasks`.
    fn l_task_row(&self) -> (Value, Value) {
        let listed = ok(
            &list_tasks(
                self.state(),
                Some(&self.reader),
                Some(&format!("project_id={}", self.project_id)),
            ),
            OperationId::ListTasks,
            200,
        );
        let row = listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["task_id"] == self.task_id.as_str())
            .unwrap()
            .clone();
        (row["state"].clone(), row["revision"].clone())
    }

    /// `transitionTask` to `completed` citing `evidence` on a `tampered`
    /// Task: `409 evidence_stale`, and neither the Task nor the ledger
    /// changes.
    fn l_completion_refused(&self, evidence: &[&str], case: &str) {
        let before = self.events().len();
        let refused = self.transition_with(&self.task_id, &self.key(), &completion(3, evidence));
        problem(&refused, OperationId::TransitionTask, 409, "evidence_stale");
        // P3-L3 (T16 S-2): the detail says why, and that a rerun is no use.
        assert_eq!(detail(&refused), TAMPERED_COMPLETION, "{case}");
        assert_eq!(
            self.l_task_row(),
            (json!("in_progress"), json!(3)),
            "{case}"
        );
        assert_eq!(self.events().len(), before, "{case}: an Event was written");
    }
}

/// L3, T15 N3 (`repro_completion_tampered.py`): a passed run, then a failed
/// run of the same argv that overrules it; then the failed run's Snapshot
/// and Evidence rows are deleted. verifyLedger is invalid and both documents
/// are `tampered`, so completion citing the passed run is refused, although
/// with the failed run gone the pass is no longer overruled.
#[test]
fn completion_is_refused_on_a_task_whose_overruling_run_was_deleted() {
    let fixture = Fixture::new();
    fixture.step(&fixture.task_id, 1, "ready");
    fixture.step(&fixture.task_id, 2, "in_progress");
    let passed = fixture.l_same_argv(&Kind::Test(1));
    let failed = fixture.capture(&fixture.run_body("test_run", 1, Some(1)));
    assert_eq!(failed["status"], "FAIL");
    let removed = snapshot(&failed);
    fixture.tamper(&format!(
        "DELETE FROM evidence WHERE evidence_id IN (SELECT evidence_id FROM \
         repository_snapshot_evidence WHERE snapshot_id = '{removed}') AND evidence_id != '{}';
         DELETE FROM repository_snapshot_evidence WHERE snapshot_id = '{removed}';
         DELETE FROM repository_snapshots WHERE snapshot_id = '{removed}';",
        id(&passed)
    ));
    assert_tampered_without_verified(&fixture, "overruling run deleted");
    fixture.l_completion_refused(&[id(&passed)], "overruling run deleted");
}

/// L3: a record the cited Evidence does not rest on (an open item's text of
/// an earlier Checkpoint) is rewritten. The cited run is a fresh, passed,
/// unoverruled test, yet the `tampered` Task is not completed. Without the
/// rewrite the same request completes the Task.
#[test]
fn completion_is_refused_on_a_tampered_task_whatever_evidence_it_cites() {
    for tampered in [true, false] {
        let fixture = Fixture::new();
        fixture.step(&fixture.task_id, 1, "ready");
        fixture.step(&fixture.task_id, 2, "in_progress");
        let passed = fixture.l_same_argv(&Kind::Test(1));
        let checkpoint = ok(
            &fixture.l_fact(snapshot(&passed), &[id(&passed)]),
            OperationId::CreateCheckpoint,
            201,
        )["checkpoint_id"]
            .as_str()
            .unwrap()
            .to_owned();
        if !tampered {
            let done = ok(
                &fixture.transition_with(
                    &fixture.task_id,
                    &fixture.key(),
                    &completion(3, &[id(&passed)]),
                ),
                OperationId::TransitionTask,
                200,
            );
            assert_eq!(done["state"], "completed");
            continue;
        }
        fixture.tamper(&format!(
            "UPDATE checkpoint_items SET text = 'rewritten' \
             WHERE checkpoint_id = '{checkpoint}' AND section = 'open_items' AND ordinal = 0;"
        ));
        assert_tampered_without_verified(&fixture, "open item rewritten");
        fixture.l_completion_refused(&[id(&passed)], "open item rewritten");
    }
}

/// The `recorded_at` of the latest Event of `session_id`, read from the
/// ledger table itself (the Event envelope does not carry it).
fn last_recorded_at(fixture: &Fixture, session_id: &str) -> Value {
    let output = std::process::Command::new("sqlite3")
        .arg(fixture.database())
        .arg(format!(
            "SELECT recorded_at FROM events WHERE session_id = '{session_id}' \
             ORDER BY sequence DESC LIMIT 1;"
        ))
        .output()
        .unwrap();
    assert!(output.status.success());
    json!(String::from_utf8(output.stdout).unwrap().trim())
}

/// L7: a Capsule Session's `last_recorded_at` is the time its latest Event
/// was recorded, read from the ledger: `session.started` at first, then the
/// run's `command.completed`, then `session.ended`. Reading twice at the
/// same ledger high-water mark gives the same bytes.
#[test]
fn l7_capsule_sessions_carry_their_last_ledger_write_time() {
    let fixture = Fixture::new();
    let session = fixture.session_id.clone();
    let (first, capsule) = fixture.capsule(None);
    assert_eq!(
        capsule["sessions"][0]["last_recorded_at"],
        last_recorded_at(&fixture, &session),
        "{first}"
    );
    let (again, _) = fixture.capsule(None);
    assert_eq!(first, again);
    fixture.l_test_run(&["sh", "-c", "check"], 0);
    let (_, capsule) = fixture.capsule(None);
    let written = last_recorded_at(&fixture, &session);
    assert_eq!(capsule["sessions"][0]["last_recorded_at"], written);
    assert_eq!(capsule["sessions"][0]["end_state"], "open");
    let (once, _) = fixture.capsule(None);
    let (twice, _) = fixture.capsule(None);
    assert_eq!(once, twice);
    ok(
        &crate::catalog::end_session(
            fixture.state(),
            &crate::test_support::json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &json!({ "schema_version": 1, "expected_revision": 1 }).to_string(),
            ),
            &session,
        ),
        OperationId::EndSession,
        200,
    );
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(capsule["sessions"][0]["end_state"], "ended");
    let ended = last_recorded_at(&fixture, &session);
    assert_ne!(ended, written);
    assert_eq!(capsule["sessions"][0]["last_recorded_at"], ended);
}
