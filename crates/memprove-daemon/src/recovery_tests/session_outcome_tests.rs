//! Narrow tests of ADR 0013 D6 (P3-D): `endSession`'s optional `outcome`.
//! `abandoned` ends the Session with `session.crashed` in place of
//! `session.ended`, one Event either way, and every reader of a Session's
//! end takes both.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, ok, problem};
use crate::{api::WireResponse, catalog::end_session, test_support::json_request};

fn end(fixture: &Fixture, session_id: &str, key: &str, body: &Value) -> WireResponse {
    end_session(
        fixture.state(),
        &json_request(Some(&fixture.writer), Some(key), &body.to_string()),
        session_id,
    )
}

/// The end Events of `session_id`, as `(event_type, payload_schema,
/// producer, payload)`.
fn ends(fixture: &Fixture, session_id: &str) -> Vec<(Value, Value, Value, Value)> {
    fixture
        .events()
        .into_iter()
        .filter(|event| event["session_id"] == session_id)
        .filter(|event| {
            matches!(
                event["event_type"].as_str(),
                Some("session.ended" | "session.crashed")
            )
        })
        .map(|event| {
            (
                event["event_type"].clone(),
                event["payload_schema"].clone(),
                event["producer"]["name"].clone(),
                event["payload"].clone(),
            )
        })
        .collect()
}

#[test]
fn an_abandoned_session_ends_with_session_crashed_alone() {
    let fixture = Fixture::new();
    let body = json!({
        "schema_version": 1, "expected_revision": 1, "reason": "taken over",
        "outcome": "abandoned",
    });
    let key = fixture.key();
    let ended = ok(
        &end(&fixture, &fixture.session_id, &key, &body),
        OperationId::EndSession,
        200,
    );
    assert_eq!(
        (ended["state"].as_str(), ended["revision"].as_u64()),
        (Some("ended"), Some(2))
    );
    assert_eq!(
        ends(&fixture, &fixture.session_id),
        [(
            json!("session.crashed"),
            json!("session.crashed@1"),
            json!("continuityd"),
            json!({ "reason": "taken over", "outcome": "abandoned" }),
        )]
    );
    let events = fixture.events().len();

    // A replay returns the result and appends nothing; the same key with a
    // completed end is another request.
    let replayed = ok(
        &end(&fixture, &fixture.session_id, &key, &body),
        OperationId::EndSession,
        200,
    );
    assert_eq!(replayed, ended);
    let completed = json!({ "schema_version": 1, "expected_revision": 1, "reason": "taken over" });
    problem(
        &end(&fixture, &fixture.session_id, &key, &completed),
        OperationId::EndSession,
        409,
        "idempotency_conflict",
    );
    assert_eq!(fixture.events().len(), events);

    // The Session has ended: no Event, no run, no second end.
    let run = fixture.capture_with(
        &fixture.writer,
        &fixture.key(),
        &fixture.run_body("test_run", 0, Some(3)),
    );
    problem(
        &run,
        OperationId::CaptureEvidence,
        409,
        "invalid_transition",
    );
    problem(
        &end(
            &fixture,
            &fixture.session_id,
            &fixture.key(),
            &json!({ "schema_version": 1, "expected_revision": 2 }),
        ),
        OperationId::EndSession,
        409,
        "invalid_transition",
    );

    // The Capsule reads it as crashed, and the ledger holds.
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(capsule["sessions"][0]["end_state"], "crashed");
    assert!(
        capsule["continuation"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["template"] == "session_crashed"
                && item["object_id"] == fixture.session_id.as_str())
    );
    assert!(fixture.ledger_valid());
}

#[test]
fn outcome_defaults_to_completed_and_is_checked() {
    let fixture = Fixture::new();
    for outcome in [json!("crashed"), json!("Abandoned"), json!(1), Value::Null] {
        let body = json!({ "schema_version": 1, "expected_revision": 1, "outcome": outcome });
        let refused = end(&fixture, &fixture.session_id, &fixture.key(), &body);
        problem(&refused, OperationId::EndSession, 400, "invalid_request");
        assert!(refused.body.contains("$.outcome"), "{}", refused.body);
    }
    // An explicit completed is the default: the same request digest.
    let key = fixture.key();
    let ended = ok(
        &end(
            &fixture,
            &fixture.session_id,
            &key,
            &json!({ "schema_version": 1, "expected_revision": 1 }),
        ),
        OperationId::EndSession,
        200,
    );
    let replayed = ok(
        &end(
            &fixture,
            &fixture.session_id,
            &key,
            &json!({ "schema_version": 1, "expected_revision": 1, "outcome": "completed" }),
        ),
        OperationId::EndSession,
        200,
    );
    assert_eq!(replayed, ended);
    assert_eq!(
        ends(&fixture, &fixture.session_id),
        [(
            json!("session.ended"),
            json!("session.ended@1"),
            json!("continuityd"),
            json!({ "reason": null }),
        )]
    );
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(capsule["sessions"][0]["end_state"], "ended");
}
