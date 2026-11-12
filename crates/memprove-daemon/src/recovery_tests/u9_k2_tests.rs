//! U9 K2 (U8 V-N5; Owner O6; ADR 0015, "Amendment (U9, S-K)"): a test run
//! records the `--test-format` it was made with. `captureEvidence` takes
//! the optional `metadata.test_format` (`cargo`, `pytest`, or `cases`) of a
//! `test_run`; the Evidence payload and `command.completed@2` carry it; the
//! Task Capsule lists it on a claim's Evidence and on `cite_latest_run`.
//! Without it, a run is recorded and read as before, byte for byte.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, ok, problem};

const FORMATS: [&str; 3] = ["cargo", "pytest", "cases"];

impl Fixture {
    /// A `test_run` of `cargo test` with one case and, when given, the
    /// `test_format`.
    fn k2_body(&self, format: Option<&str>) -> Value {
        let mut body = self.run_body("test_run", 0, Some(1));
        if let Some(format) = format {
            body["metadata"]["test_format"] = json!(format);
        }
        body
    }

    /// The `command.completed` Event that recorded the Evidence `id`.
    fn k2_completed(&self, id: &str) -> Value {
        self.events()
            .into_iter()
            .rev()
            .find(|event| {
                event["event_type"] == "command.completed"
                    && event["payload"]["command"]["evidence_id"] == id
            })
            .unwrap_or_else(|| panic!("no command.completed for {id}"))
    }

    /// The Task Capsule after a Checkpoint whose one fact cites `run`.
    fn k2_cited_capsule(&self, run: &Value) -> (String, Value) {
        let snapshot = run["snapshot"]["snapshot_id"].as_str().unwrap();
        let evidence = run["evidence_id"].as_str().unwrap();
        ok(
            &self.checkpoint_with(
                &self.writer,
                &self.key(),
                &self.checkpoint_body(snapshot, &[evidence]),
            ),
            OperationId::CreateCheckpoint,
            201,
        );
        self.capsule(None)
    }
}

fn detail(body: &str) -> String {
    serde_json::from_str::<Value>(body).unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Each format is recorded in the Evidence payload and in the Event, which
/// moves to `command.completed@2`; a run without one stays as it was: no
/// member anywhere and `command.completed@1`.
#[test]
fn a_test_run_records_its_format_and_one_without_keeps_its_bytes() {
    let fixture = Fixture::new();
    for format in FORMATS {
        let run = fixture.capture(&fixture.k2_body(Some(format)));
        assert_eq!(run["payload"]["test_format"], format, "{run}");
        let event = fixture.k2_completed(run["evidence_id"].as_str().unwrap());
        assert_eq!(event["payload_schema"], "command.completed@2", "{event}");
        assert_eq!(event["payload"]["command"]["test_format"], format);
    }
    let run = fixture.capture(&fixture.k2_body(None));
    assert!(run["payload"].get("test_format").is_none(), "{run}");
    let event = fixture.k2_completed(run["evidence_id"].as_str().unwrap());
    assert_eq!(event["payload_schema"], "command.completed@1", "{event}");
    assert!(
        event["payload"]["command"].get("test_format").is_none(),
        "{event}"
    );
    let command = fixture.capture(&fixture.run_body("command_run", 0, None));
    let event = fixture.k2_completed(command["evidence_id"].as_str().unwrap());
    assert_eq!(event["payload_schema"], "command.completed@1", "{event}");
    assert!(fixture.ledger_valid());
}

/// A format outside the closed set, one that is not a string, and one on a
/// `command_run` are refused with nothing recorded.
#[test]
fn a_test_format_outside_the_set_or_on_a_command_run_is_refused() {
    let fixture = Fixture::new();
    let before = fixture.events().len();
    let refuse = |body: &Value| {
        let response = fixture.capture_with(&fixture.writer, &fixture.key(), body);
        problem(
            &response,
            OperationId::CaptureEvidence,
            400,
            "invalid_request",
        );
        detail(&response.body)
    };
    for bad in [
        json!("junit"),
        json!(""),
        json!(7),
        json!(null),
        json!(["cargo"]),
    ] {
        let mut body = fixture.k2_body(None);
        body["metadata"]["test_format"] = bad.clone();
        assert!(refuse(&body).starts_with("$.metadata.test_format"), "{bad}");
    }
    let mut body = fixture.run_body("command_run", 0, None);
    body["metadata"]["test_format"] = json!("cargo");
    assert!(
        refuse(&body).contains("test_format") && refuse(&body).contains("test_run"),
        "{}",
        refuse(&body)
    );
    assert_eq!(fixture.events().len(), before);
}

/// The format enters the idempotency digest only when given: the same key
/// and body replays, and the same key with another format conflicts.
#[test]
fn the_test_format_belongs_to_the_request_digest_only_when_given() {
    let fixture = Fixture::new();
    let key = fixture.key();
    let body = fixture.k2_body(Some("cargo"));
    let first = fixture.capture_with(&fixture.writer, &key, &body);
    assert_eq!(first.status, 201, "{}", first.body);
    let replay = fixture.capture_with(&fixture.writer, &key, &body);
    assert_eq!(replay.status, 201, "{}", replay.body);
    assert_eq!(first.body, replay.body);
    let mut other = body.clone();
    other["metadata"]["test_format"] = json!("pytest");
    let conflict = fixture.capture_with(&fixture.writer, &key, &other);
    assert_eq!(conflict.status, 409, "{}", conflict.body);
    let mut without = body;
    without["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("test_format");
    let conflict = fixture.capture_with(&fixture.writer, &key, &without);
    assert_eq!(conflict.status, 409, "{}", conflict.body);
}

/// The Capsule lists the format on the Evidence a claim cites and on
/// `cite_latest_run`; a Task whose runs name none, and a Task with only a
/// command run, have no `test_format` anywhere in the Capsule.
#[test]
fn the_capsule_lists_the_format_only_for_a_run_that_recorded_one() {
    for format in FORMATS {
        let fixture = Fixture::new();
        let run = fixture.capture(&fixture.k2_body(Some(format)));
        let (_, capsule) = fixture.k2_cited_capsule(&run);
        let claim = capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|claim| claim["kind"] == "fact")
            .unwrap();
        assert_eq!(claim["evidence"][0]["test_format"], format, "{claim}");
        let latest = capsule["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["action"] == "cite_latest_run")
            .unwrap();
        assert_eq!(latest["args"]["test_format"], format, "{latest}");
    }
    let fixture = Fixture::new();
    let run = fixture.capture(&fixture.k2_body(None));
    let (bytes, _) = fixture.k2_cited_capsule(&run);
    assert!(!bytes.contains("test_format"), "{bytes}");
    let fixture = Fixture::new();
    fixture.capture(&fixture.run_body("command_run", 0, None));
    let (bytes, _) = fixture.capsule(None);
    assert!(!bytes.contains("test_format"), "{bytes}");
}

/// The format is part of the Evidence object's canonical digest and of the
/// Event chain: changing it in the row, digests left as they were, makes
/// `verifyLedger` and the Task Capsule find the Task tampered.
#[test]
fn a_changed_test_format_is_found_by_the_ledger_and_the_capsule() {
    let fixture = Fixture::new();
    let run = fixture.capture(&fixture.k2_body(Some("cargo")));
    fixture.k2_cited_capsule(&run);
    assert!(fixture.ledger_valid());
    fixture.tamper(&format!(
        "UPDATE evidence SET payload = replace(payload, '\"test_format\":\"cargo\"', \
         '\"test_format\":\"pytest\"') WHERE evidence_id = '{}';",
        run["evidence_id"].as_str().unwrap()
    ));
    super::integrity_tests::assert_tampered_without_verified(&fixture, "test_format");
}
