//! Narrow tests of P3-D3 (the findings of T14) through the real handlers,
//! store, and Git probes:
//!
//! - S-1: a passed run is overruled when the latest run of the same argv
//!   in the same Task, Workspace, and repository state did not pass, with
//!   or without carry-over: the Preview, the Capsule, and Task completion
//!   judge it alike, and `createCheckpoint` refuses a `verified_fact` on it.
//! - S-2: a Snapshot or Evidence row the ledger announces on a Session of
//!   the Task, moved to another Task, makes both documents `tampered`.
//! - S-3: the Capsule reads `session.crashed` only from a payload that
//!   decodes as `session.crashed@1`.
//! - N10: a run is carried over only from the first Snapshot that seals it.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, completion, integrity_tests::assert_tampered_without_verified, ok, problem};
use crate::{
    api::WireResponse,
    catalog::resolve_workspace,
    test_support::{git_worktree, json_request},
};

/// The command T14's ignored-file case runs.
const CHECK: [&str; 3] = ["sh", "-c", "grep -q A ignored.cfg"];

fn git(directory: &std::path::Path, arguments: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn id(run: &Value) -> &str {
    run["evidence_id"].as_str().unwrap()
}

fn snapshot(run: &Value) -> &str {
    run["snapshot"]["snapshot_id"].as_str().unwrap()
}

/// The command and test Evidence a capture response's Snapshot seals.
fn sealed(run: &Value) -> Vec<String> {
    ["command_evidence_ids", "test_evidence_ids"]
        .iter()
        .flat_map(|name| run["snapshot"][name].as_array().unwrap().clone())
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

/// Whether any member anywhere in `value` is the string `verified`.
fn holds_verified(value: &Value) -> bool {
    match value {
        Value::String(text) => text == "verified",
        Value::Array(items) => items.iter().any(holds_verified),
        Value::Object(members) => members.values().any(holds_verified),
        _ => false,
    }
}

impl Fixture {
    /// Records a one-case test run of `argv` that exited `exit_code`, for
    /// the fixture's Task and Session unless `task` names another Task, its
    /// Session, and Workspace.
    fn run_of(&self, argv: &[&str], exit_code: i32, task: Option<(&str, &str, &str)>) -> Value {
        let mut body = self.run_body("test_run", exit_code, Some(1));
        body["metadata"]["argv"] = json!(argv);
        if let Some((task_id, session_id, workspace_id)) = task {
            body["task_id"] = json!(task_id);
            body["session_id"] = json!(session_id);
            body["workspace_id"] = json!(workspace_id);
        }
        self.capture(&body)
    }

    fn fact_on(&self, snapshot: &str, evidence: &[&str]) -> WireResponse {
        self.checkpoint_with(
            &self.writer,
            &self.key(),
            &self.checkpoint_body(snapshot, evidence),
        )
    }

    /// Moves the fixture's Task to `in_progress`; its revision is then 3.
    fn in_progress(&self) {
        self.step(&self.task_id, 1, "ready");
        self.step(&self.task_id, 2, "in_progress");
    }
}

/// T14 S-1 (`repro_ignored_carry.py`): a passed run, a Checkpoint whose
/// fact cites it, then a change the capture manifest does not observe and a
/// failed run of the same argv in the same state. The failed run carries
/// the passed one over, and the fact still cites the earlier pass: neither
/// the Preview nor the Capsule holds `verified`, Task completion with it is
/// `409`, and a new `verified_fact` on it over the later Snapshot is `400`
/// naming the later run.
fn a_later_failure_overrules(case: &str, setup: impl Fn(&Fixture), change: impl Fn(&Fixture)) {
    let fixture = Fixture::new();
    setup(&fixture);
    fixture.in_progress();
    let passed = fixture.run_of(&CHECK, 0, None);
    assert_eq!(passed["status"], "PASS");
    ok(
        &fixture.fact_on(snapshot(&passed), &[id(&passed)]),
        OperationId::CreateCheckpoint,
        201,
    );
    assert_eq!(fixture.preview(None).1["status"], "verified", "{case}");
    change(&fixture);
    let failed = fixture.run_of(&CHECK, 1, None);
    assert_eq!(failed["status"], "FAIL");
    assert_eq!(
        failed["snapshot"]["capture_manifest_digest"],
        passed["snapshot"]["capture_manifest_digest"],
        "{case}: the manifest does not observe the change"
    );
    assert!(
        sealed(&failed).iter().any(|carried| carried == id(&passed)),
        "{case}: the failed run carries the passed one over"
    );

    let (preview_bytes, preview) = fixture.preview(None);
    assert_eq!(preview["status"], "partial", "{case}: {preview_bytes}");
    assert_eq!(preview["reasons"], json!(["evidence_not_passed"]), "{case}");
    let item = &preview["facts"][0]["evidence"][0];
    assert_eq!(item["run_status"], "PASS", "{case}");
    assert_eq!(
        (&item["verdict"], &item["reasons"]),
        (&json!("partial"), &json!(["evidence_not_passed"])),
        "{case}"
    );
    assert!(!holds_verified(&preview), "{case}: {preview_bytes}");
    let (capsule_bytes, capsule) = fixture.capsule(None);
    assert_eq!(capsule["status"], "partial", "{case}: {capsule_bytes}");
    assert!(!holds_verified(&capsule), "{case}: {capsule_bytes}");
    // The fact's run is offered for a rerun, never for reaffirmation.
    assert_eq!(capsule["rerun"][0]["argv"], json!(CHECK), "{case}");

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
    let refused = fixture.fact_on(snapshot(&failed), &[id(&passed)]);
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    let detail = serde_json::from_str::<Value>(&refused.body).unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        detail.starts_with(
            "1 verified_fact citation names an overruled passed run: (1) \
             $.verified_completed_work[0].evidence_ids[0]. (1) "
        ) && detail.contains(&format!("Evidence {} passed;", id(&passed)))
            && detail.contains(id(&failed)),
        "{case}: {detail}"
    );
    // The failed run itself still cannot back a fact, and nothing changed.
    assert!(fixture.ledger_valid());
}

/// T14 S-1, the ignored-file case: `ignored.cfg` is in `.gitignore`.
#[test]
fn an_ignored_file_change_and_a_later_failure_overrule_the_pass() {
    a_later_failure_overrules(
        "ignored file",
        |fixture| {
            std::fs::write(fixture.repo.join(".gitignore"), b"ignored.cfg\n").unwrap();
            git(&fixture.repo, &["add", ".gitignore"]);
            fixture.commit("ignore ignored.cfg");
            std::fs::write(fixture.repo.join("ignored.cfg"), b"A\n").unwrap();
        },
        |fixture| std::fs::write(fixture.repo.join("ignored.cfg"), b"B\n").unwrap(),
    );
}

/// T14 S-1, the write-bit case: a tracked file loses its write bit.
#[test]
fn a_write_bit_change_and_a_later_failure_overrule_the_pass() {
    use std::os::unix::fs::PermissionsExt;
    a_later_failure_overrules(
        "write bit",
        |_| {},
        |fixture| {
            std::fs::set_permissions(
                fixture.repo.join("README"),
                std::fs::Permissions::from_mode(0o444),
            )
            .unwrap();
        },
    );
}

/// P3-D3: the rule does not depend on carry-over. Both states list the same
/// untracked file, so their manifests are equal and nothing is carried; the
/// later failure still overrules the earlier pass: a fact on it is refused
/// on write, and an earlier fact on it is neither `verified` nor completes
/// the Task.
#[test]
fn a_later_failure_overrules_the_pass_without_carry_over() {
    let fixture = Fixture::new();
    fixture.in_progress();
    std::fs::write(fixture.repo.join("scratch.txt"), b"same\n").unwrap();
    let passed = fixture.run_of(&CHECK, 0, None);
    ok(
        &fixture.fact_on(snapshot(&passed), &[id(&passed)]),
        OperationId::CreateCheckpoint,
        201,
    );
    let failed = fixture.run_of(&CHECK, 1, None);
    assert_eq!(
        failed["snapshot"]["capture_manifest_digest"],
        passed["snapshot"]["capture_manifest_digest"]
    );
    assert_eq!(sealed(&failed), [id(&failed)], "nothing is carried");
    let refused = fixture.fact_on(snapshot(&passed), &[id(&passed)]);
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    assert!(
        refused.body.contains("names an overruled passed run"),
        "{}",
        refused.body
    );
    assert!(refused.body.contains(id(&failed)), "{}", refused.body);
    let (preview_bytes, preview) = fixture.preview(None);
    assert!(!holds_verified(&preview), "{preview_bytes}");
    let (capsule_bytes, capsule) = fixture.capsule(None);
    assert!(!holds_verified(&capsule), "{capsule_bytes}");
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

/// P3-D3: pass, fail, pass in one state. Each run is judged by its own
/// outcome, since the latest run passed: facts on the first and on the last
/// pass are `verified`, an open item on the failure is `partial` with
/// `evidence_not_passed`, and the Task completes on the last run.
#[test]
fn pass_fail_pass_judges_each_run_by_its_own_outcome() {
    let fixture = Fixture::new();
    fixture.in_progress();
    let first = fixture.run_of(&CHECK, 0, None);
    let failed = fixture.run_of(&CHECK, 1, None);
    // Between the two passes, the first one is overruled.
    let refused = fixture.fact_on(snapshot(&failed), &[id(&first)]);
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    let last = fixture.run_of(&CHECK, 0, None);
    let mut body = fixture.checkpoint_body(snapshot(&last), &[id(&first)]);
    body["verified_completed_work"] = json!([
        { "classification": "verified_fact", "text": "first pass", "evidence_ids": [id(&first)] },
        { "classification": "verified_fact", "text": "last pass", "evidence_ids": [id(&last)] },
    ]);
    body["open_items"] = json!([
        { "classification": "unknown", "text": "it failed once", "evidence_ids": [id(&failed)] },
    ]);
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
        OperationId::CreateCheckpoint,
        201,
    );
    let (_, preview) = fixture.preview(None);
    assert_eq!(preview["status"], "verified", "{preview}");
    let verdicts = preview["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| {
            (
                fact["text"].as_str().unwrap().to_owned(),
                fact["verdict"].clone(),
                fact["evidence"][0]["reasons"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        verdicts,
        [
            ("first pass".to_owned(), json!("verified"), json!([])),
            ("last pass".to_owned(), json!("verified"), json!([])),
            (
                "it failed once".to_owned(),
                json!("partial"),
                json!(["evidence_not_passed"])
            ),
            (
                "open a pull request".to_owned(),
                json!("partial"),
                Value::Null
            ),
        ]
    );
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(capsule["status"], "verified");
    let done = ok(
        &fixture.transition_with(
            &fixture.task_id,
            &fixture.key(),
            &completion(3, &[id(&last)]),
        ),
        OperationId::TransitionTask,
        200,
    );
    assert_eq!(done["state"], "completed");
}

/// P3-D3: failures of another argv, of another Task, and of another
/// Workspace overrule nothing.
#[test]
fn failures_of_another_argv_task_or_workspace_overrule_nothing() {
    let fixture = Fixture::new();
    // A Task with no Workspace scope, continued in the root Workspace and
    // in a linked worktree at the same commit.
    let task = fixture.task(None);
    let root = fixture.workspace_id.clone();
    let session = fixture.session(&root, Some(&task));
    let linked_path = git_worktree(&fixture.repo, &fixture.repos.path().join("side"), "side");
    let linked = ok(
        &resolve_workspace(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                None,
                &json!({
                    "schema_version": 1, "project_id": fixture.project_id, "path": linked_path,
                })
                .to_string(),
            ),
        ),
        OperationId::ResolveWorkspace,
        200,
    )["workspace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let linked_session = fixture.session(&linked, Some(&task));
    let passed = fixture.run_of(&CHECK, 0, Some((&task, &session, &root)));
    let mut body = fixture.checkpoint_body(snapshot(&passed), &[id(&passed)]);
    body["task_id"] = json!(task);
    body["session_id"] = json!(session);
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
        OperationId::CreateCheckpoint,
        201,
    );
    let judged = || {
        let response = fixture.preview_with(Some(&fixture.reader), &task, None);
        ok(&response, OperationId::GetRecoveryPreview, 200)["status"].clone()
    };
    assert_eq!(judged(), "verified");
    // Another argv, same Task, Workspace, and state.
    let other = fixture.run_of(&["cargo", "test"], 1, Some((&task, &session, &root)));
    assert!(sealed(&other).iter().any(|carried| carried == id(&passed)));
    assert_eq!(judged(), "verified", "another argv");
    // The same argv in another Task (the fixture's), same Workspace and
    // state.
    fixture.run_of(&CHECK, 1, None);
    assert_eq!(judged(), "verified", "another Task");
    // The same argv and Task in another Workspace at the same commit.
    fixture.run_of(&CHECK, 1, Some((&task, &linked_session, &linked)));
    assert_eq!(judged(), "verified", "another Workspace");
}

/// The ledger of T14's `repro_moved_snapshot.py`: Task T with a Session,
/// another Task T2, and S1 sealing run E1.
struct Moved {
    fixture: Fixture,
    other_task: String,
    s1: String,
    e1: String,
}

/// `carried`: E1 is carried into S2, whose Checkpoint cites E1 (S1 is the
/// first sealer of the cited, carried run). Otherwise S1 is a run no
/// Checkpoint uses: a commit, then S2 with its own run E2, which the
/// Checkpoint cites.
fn moved(carried: bool) -> Moved {
    let fixture = Fixture::new();
    let other_task = fixture.task(Some(&fixture.workspace_id.clone()));
    let first = fixture.run_of(&CHECK, 0, None);
    if !carried {
        std::fs::write(fixture.repo.join("README"), b"fixture\nsecond\n").unwrap();
        fixture.commit("Y");
    }
    let second = fixture.run_of(&CHECK, 0, None);
    assert_eq!(
        sealed(&second).iter().any(|item| item == id(&first)),
        carried
    );
    let cited = if carried { id(&first) } else { id(&second) };
    ok(
        &fixture.fact_on(snapshot(&second), &[cited]),
        OperationId::CreateCheckpoint,
        201,
    );
    let (_, preview) = fixture.preview(None);
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(
        (&preview["status"], &capsule["status"]),
        (&json!("verified"), &json!("verified"))
    );
    Moved {
        s1: snapshot(&first).to_owned(),
        e1: id(&first).to_owned(),
        fixture,
        other_task,
    }
}

impl Moved {
    fn damaged(&self) -> Vec<(String, String, String)> {
        let (_, capsule) = self.fixture.capsule(None);
        let (_, preview) = self.fixture.preview(None);
        assert!(
            preview["gaps"]
                .as_array()
                .unwrap()
                .contains(&json!("record_integrity"))
        );
        capsule["damaged_records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| {
                (
                    record["record_type"].as_str().unwrap().to_owned(),
                    record["record_id"].as_str().unwrap().to_owned(),
                    record["reason"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

/// T14 S-2, `aa5e712-carried`: the first sealer of the cited, carried run
/// moved to another Task.
#[test]
fn a_moved_first_sealer_of_a_cited_carried_run_makes_both_tampered() {
    let ledger = moved(true);
    ledger.fixture.tamper(&format!(
        "UPDATE repository_snapshots SET task_id = '{}' WHERE snapshot_id = '{}';",
        ledger.other_task, ledger.s1
    ));
    assert_tampered_without_verified(&ledger.fixture, "carried");
    assert!(ledger.damaged().contains(&(
        "snapshot".to_owned(),
        ledger.s1.clone(),
        "object_digest_mismatch".to_owned()
    )));
}

/// T14 S-2, `aa5e712-plain` and `87fd1a6-plain` (the same ledger, one
/// without carry-over): a Snapshot no Checkpoint uses moved to another
/// Task.
#[test]
fn a_moved_snapshot_no_checkpoint_uses_makes_both_tampered() {
    let ledger = moved(false);
    ledger.fixture.tamper(&format!(
        "UPDATE repository_snapshots SET task_id = '{}' WHERE snapshot_id = '{}';",
        ledger.other_task, ledger.s1
    ));
    assert_tampered_without_verified(&ledger.fixture, "plain");
    assert_eq!(
        ledger.damaged(),
        [(
            "snapshot".to_owned(),
            ledger.s1.clone(),
            "object_digest_mismatch".to_owned()
        )]
    );
}

/// T14 S-2, the third shape of the plain ledger: the whole run, its
/// Snapshot and its Evidence rows, moved to another Task, so that every
/// row reads as a consistent run of that Task and only the ledger's
/// announcement on Task T's Session still names it.
#[test]
fn a_whole_run_moved_to_another_task_makes_both_tampered() {
    let ledger = moved(false);
    ledger.fixture.tamper(&format!(
        "UPDATE repository_snapshots SET task_id = '{task}' WHERE snapshot_id = '{s1}';
         UPDATE evidence SET task_id = '{task}' WHERE evidence_id IN
           (SELECT evidence_id FROM repository_snapshot_evidence WHERE snapshot_id = '{s1}');",
        task = ledger.other_task,
        s1 = ledger.s1
    ));
    assert_tampered_without_verified(&ledger.fixture, "whole run");
    let damaged = ledger.damaged();
    for expected in [("snapshot", &ledger.s1), ("evidence", &ledger.e1)] {
        assert!(
            damaged.contains(&(
                expected.0.to_owned(),
                expected.1.clone(),
                "object_digest_mismatch".to_owned()
            )),
            "{damaged:?}"
        );
    }
}

/// T14 S-3 (`repro_crashed_payload_mutant.py`): chain-consistent
/// `session.crashed@1` Events of `memproved` read as a crash only when
/// the payload decodes: exactly `reason` (null or a string) and `outcome`
/// `abandoned`. A missing `outcome`, `outcome` `completed`, and an empty
/// payload read as `open`.
#[test]
fn session_crashed_counts_only_with_a_payload_that_decodes() {
    let fixture = Fixture::new();
    let cases = [
        (
            "valid",
            json!({ "reason": null, "outcome": "abandoned" }),
            "crashed",
        ),
        (
            "valid, with a reason",
            json!({ "reason": "lost", "outcome": "abandoned" }),
            "crashed",
        ),
        ("no outcome", json!({ "reason": null }), "open"),
        (
            "outcome completed",
            json!({ "reason": null, "outcome": "completed" }),
            "open",
        ),
        ("empty", json!({}), "open"),
    ];
    let mut sessions = Vec::new();
    for (_, payload, _) in &cases {
        let session_id = fixture.session(&fixture.workspace_id.clone(), Some(&fixture.task_id));
        fixture.append_on(&session_id, "session.crashed", "session.crashed@1", payload);
        sessions.push(session_id);
    }
    assert!(fixture.ledger_valid());
    let (_, capsule) = fixture.capsule(None);
    for ((case, _, expected), session_id) in cases.iter().zip(&sessions) {
        let listed = capsule["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|session| session["session_id"] == session_id.as_str())
            .unwrap();
        assert_eq!(listed["end_state"], *expected, "{case}");
    }
}

/// T14 N10 (`repro_first_sealer_mutant.py`): a run is carried over only
/// from the first Snapshot that seals it. S1 (E1's first sealer) leaves
/// the candidates, by its manifest column or its Task; S2, which carried
/// E1, is intact; the next run of the state carries S2's own run but not
/// E1.
#[test]
fn a_run_is_carried_only_from_its_first_sealer() {
    for column in ["capture_manifest_digest", "task_id"] {
        let fixture = Fixture::new();
        let other_task = fixture.task(Some(&fixture.workspace_id.clone()));
        let first = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
        let second = fixture.capture(&fixture.run_body("command_run", 0, None));
        assert!(sealed(&second).iter().any(|item| item == id(&first)));
        let value = if column == "task_id" {
            other_task
        } else {
            "0".repeat(64)
        };
        fixture.tamper(&format!(
            "UPDATE repository_snapshots SET {column} = '{value}' WHERE snapshot_id = '{}';",
            snapshot(&first)
        ));
        let third = fixture.capture(&fixture.run_body("command_run", 0, None));
        let carried = sealed(&third);
        assert!(carried.iter().any(|item| item == id(&second)), "{column}");
        assert!(!carried.iter().any(|item| item == id(&first)), "{column}");
        assert!(!fixture.ledger_valid(), "{column}");
        let (_, capsule) = fixture.capsule(None);
        assert_eq!(capsule["status"], "tampered", "{column}");
    }
}
