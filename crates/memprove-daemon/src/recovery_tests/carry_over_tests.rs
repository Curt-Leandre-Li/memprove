//! Narrow tests of same-state carry-over (ADR 0013 D3, P3-D) through the
//! real handlers, store, and Git probes: `captureEvidence` seals the command
//! and test Evidence of earlier Snapshots of the same Task, Workspace, and
//! repository state into its new Snapshot, never from a Snapshot with an
//! untracked file or a record that no longer checks out, and a replay
//! returns the recorded result without carrying anything again.

use memprove_domain::EvidencePayload;
use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, ok, problem};

/// The command and test Evidence a capture response's Snapshot seals.
fn sealed(run: &Value) -> Vec<String> {
    ["command_evidence_ids", "test_evidence_ids"]
        .iter()
        .flat_map(|name| run["snapshot"][name].as_array().unwrap().clone())
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

fn id(run: &Value) -> &str {
    run["evidence_id"].as_str().unwrap()
}

fn snapshot(run: &Value) -> &str {
    run["snapshot"]["snapshot_id"].as_str().unwrap()
}

fn sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids
}

/// `(verdict, reasons)` of `evidence_id` inside claim `number` of a Capsule.
fn judged(capsule: &Value, number: u64, evidence_id: &str) -> (Value, Value, Value) {
    let claim = capsule["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|claim| claim["number"] == number)
        .unwrap();
    let item = claim["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["evidence_id"] == evidence_id)
        .unwrap();
    (
        item["verdict"].clone(),
        item["reasons"].clone(),
        item["snapshot_id"].clone(),
    )
}

#[test]
fn same_state_runs_are_carried_over_and_a_replay_carries_nothing_again() {
    let fixture = Fixture::new();
    let first_body = fixture.run_body("test_run", 0, Some(3));
    let first_key = fixture.key();
    let first = ok(
        &fixture.capture_with(&fixture.writer, &first_key, &first_body),
        OperationId::CaptureEvidence,
        201,
    );
    assert_eq!(sealed(&first), [id(&first)]);
    let failed = fixture.capture(&fixture.run_body("command_run", 2, None));
    assert_eq!(failed["status"], "FAIL");
    assert_eq!(
        sorted(sealed(&failed)),
        sorted(vec![id(&first).to_owned(), id(&failed).to_owned()])
    );
    let third = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    assert_eq!(
        sorted(sealed(&third)),
        sorted(vec![
            id(&first).to_owned(),
            id(&failed).to_owned(),
            id(&third).to_owned()
        ])
    );
    let events = fixture.events().len();

    // A replay returns the recorded result: its own Snapshot, carrying
    // nothing, although two later runs share its state now; nothing is
    // appended.
    let replayed = ok(
        &fixture.capture_with(&fixture.writer, &first_key, &first_body),
        OperationId::CaptureEvidence,
        201,
    );
    assert_eq!(replayed["snapshot"], first["snapshot"]);
    assert_eq!(sealed(&replayed), [id(&first)]);
    assert_eq!(fixture.events().len(), events);

    // The command.completed Event of the third run records every Evidence
    // object its Snapshot seals, carried ones included, in the payload
    // structure it always had.
    let recorded = fixture
        .events()
        .into_iter()
        .rfind(|event| event["event_type"] == "command.completed")
        .unwrap();
    let listed = recorded["payload"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["evidence_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    for carried in [id(&first), id(&failed)] {
        assert!(listed.iter().any(|listed| listed == carried));
    }
    assert_eq!(
        recorded["payload"]["command"]["evidence_id"],
        third["evidence_id"]
    );
    assert!(fixture.ledger_valid());

    // A Checkpoint over the third Snapshot cites both passed runs.
    let both = fixture.checkpoint_body(snapshot(&third), &[id(&first), id(&third)]);
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &both),
        OperationId::CreateCheckpoint,
        201,
    );
    let (_, preview) = fixture.preview(None);
    assert_eq!(preview["status"], "verified", "{preview}");
    assert_eq!(preview["facts"][0]["evidence"].as_array().unwrap().len(), 2);
    assert!(fixture.ledger_valid());
}

#[test]
fn a_carried_run_that_did_not_pass_still_supports_no_verified_fact() {
    let fixture = Fixture::new();
    let failed = fixture.capture(&fixture.run_body("test_run", 101, Some(3)));
    assert_eq!(failed["status"], "FAIL");
    let passed = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    assert!(sealed(&passed).iter().any(|carried| carried == id(&failed)));
    for evidence in [vec![id(&failed)], vec![id(&passed), id(&failed)]] {
        let body = fixture.checkpoint_body(snapshot(&passed), &evidence);
        let refused = fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body);
        problem(
            &refused,
            OperationId::CreateCheckpoint,
            400,
            "invalid_request",
        );
    }
    // As an open item, the carried failure is recorded and judged as a
    // fresh run that did not pass.
    let mut body = fixture.checkpoint_body(snapshot(&passed), &[id(&passed)]);
    body["open_items"] = json!([
        { "classification": "unknown", "text": "the slow suite fails", "evidence_ids": [id(&failed)] },
    ]);
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
        OperationId::CreateCheckpoint,
        201,
    );
    let (_, capsule) = fixture.capsule(None);
    let (verdict, reasons, _) = judged(&capsule, 2, id(&failed));
    assert_eq!(
        (verdict, reasons),
        (json!("partial"), json!(["evidence_not_passed"]))
    );
}

/// The same Evidence object judged through the Snapshot it was recorded
/// with and through a later Snapshot that carried it over gets the same
/// verdict and reasons, now and after each kind of change.
#[test]
fn carried_evidence_is_judged_as_through_its_own_snapshot() {
    let fixture = Fixture::new();
    let first = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    let body = fixture.checkpoint_body(snapshot(&first), &[id(&first)]);
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
        OperationId::CreateCheckpoint,
        201,
    );
    let second = fixture.capture(&fixture.run_body("command_run", 0, None));
    let body = fixture.checkpoint_body(snapshot(&second), &[id(&first)]);
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
        OperationId::CreateCheckpoint,
        201,
    );
    // Claims: #1 fact over the first Snapshot, #2 open, #3 next, #4 fact
    // over the second Snapshot, #5 open, #6 next.
    let compare = |case: &str, expected: &str| {
        let (_, capsule) = fixture.capsule(None);
        let own = judged(&capsule, 1, id(&first));
        let carried = judged(&capsule, 4, id(&first));
        assert_eq!(own.2, snapshot(&first), "{case}");
        assert_eq!(carried.2, snapshot(&second), "{case}");
        assert_eq!((&own.0, &own.1), (&carried.0, &carried.1), "{case}");
        assert_eq!(own.0, expected, "{case}: {capsule}");
    };
    compare("unchanged", "verified");
    std::fs::write(fixture.repo.join("README"), b"fixture\nedited\n").unwrap();
    compare("tracked file edited", "stale");
    std::fs::write(fixture.repo.join("README"), b"fixture\n").unwrap();
    compare("edit undone", "verified");
    std::fs::write(fixture.repo.join("scratch.txt"), b"new\n").unwrap();
    compare("untracked file", "stale");
}

/// Owner decision P3-D: no carry-over when either state lists an
/// untracked file, and (ADR 0013 D3) only Evidence that checks out now.
#[test]
fn untracked_files_and_records_that_do_not_check_out_are_not_carried() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.join("scratch.txt"), b"new\n").unwrap();
    let first = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    let second = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    assert_eq!(
        first["snapshot"]["capture_manifest_digest"],
        second["snapshot"]["capture_manifest_digest"]
    );
    assert_eq!(sealed(&second), [id(&second)]);
    std::fs::remove_file(fixture.repo.join("scratch.txt")).unwrap();

    // A row that no longer recomputes is not carried.
    let changed = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    fixture.tamper(&format!(
        "UPDATE evidence SET payload = replace(payload, '\"exit_code\":0', '\"exit_code\":7') \
         WHERE evidence_id = '{}';",
        id(&changed)
    ));
    let after_change = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    assert_eq!(sealed(&after_change), [id(&after_change)]);

    // Nor one that recomputes but disagrees with memproved's Events: the
    // payload and every digest over it rewritten.
    let fixture = Fixture::new();
    let rewritten = fixture.capture(&fixture.run_body("test_run", 101, Some(3)));
    let checkpoint = fixture.checkpoint_body(snapshot(&rewritten), &[]);
    let mut checkpoint = checkpoint;
    checkpoint["verified_completed_work"] = json!([]);
    let created = ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &checkpoint),
        OperationId::CreateCheckpoint,
        201,
    );
    let EvidencePayload::TestResult {
        argv,
        stdout,
        stderr,
        test_case_count,
        test_format,
        started_at,
        finished_at,
        stdout_sha256,
        stderr_sha256,
        launch_error,
        ..
    } = fixture
        .store()
        .get_evidence(&memprove_domain::EvidenceId::parse(id(&rewritten)).unwrap())
        .unwrap()
        .payload()
        .clone()
    else {
        panic!("not a test result");
    };
    let forged = EvidencePayload::TestResult {
        argv,
        exit_code: 0,
        stdout,
        stderr,
        passed: true,
        test_case_count,
        test_format,
        started_at,
        finished_at,
        stdout_sha256,
        stderr_sha256,
        launch_error,
    };
    fixture.rewrite_payloads(&[(
        id(&rewritten),
        snapshot(&rewritten),
        created["checkpoint_id"].as_str().unwrap(),
        forged,
    )]);
    assert!(!fixture.ledger_valid());
    let next = fixture.capture(&fixture.run_body("test_run", 0, Some(3)));
    assert_eq!(sealed(&next), [id(&next)]);
}

/// Thirty runs in one state: the n-th Snapshot seals all n runs. Prints the
/// membership rows and the time of the first and the thirtieth
/// `captureEvidence` (the handler: request checks, the Git probe, and the
/// write transaction with its carry-over).
#[test]
fn thirty_runs_in_one_state_carry_every_earlier_run() {
    let fixture = Fixture::new();
    let mut elapsed = Vec::new();
    let mut last = Value::Null;
    for index in 0..30 {
        let body = fixture.run_body("command_run", 0, None);
        let key = fixture.key();
        let started = std::time::Instant::now();
        let response = fixture.capture_with(&fixture.writer, &key, &body);
        elapsed.push(started.elapsed());
        last = ok(&response, OperationId::CaptureEvidence, 201);
        assert_eq!(sealed(&last).len(), index + 1);
    }
    let rows = |sql: &str| {
        let output = std::process::Command::new("sqlite3")
            .arg(fixture.database())
            .arg(sql)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let total = rows("SELECT count(*) FROM repository_snapshot_evidence;");
    let latest = rows(&format!(
        "SELECT count(*) FROM repository_snapshot_evidence WHERE snapshot_id = '{}';",
        snapshot(&last)
    ));
    println!(
        "30 captures in one state: {total} membership rows (30th Snapshot: {latest}); \
         captureEvidence took {:?} (1st) and {:?} (30th)",
        elapsed[0], elapsed[29]
    );
    // Two repository rows per Snapshot plus n runs in the n-th.
    assert_eq!(total, (2 * 30 + (1..=30).sum::<usize>()).to_string());
    assert_eq!(latest, "32");
    assert!(fixture.ledger_valid());
}
