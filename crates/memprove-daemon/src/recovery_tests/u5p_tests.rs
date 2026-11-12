//! Narrow tests of U5 P2 (the U5 task plan,
//! section 4.2; D1 F2, Owner O4; ADR 0014 C4, "Amendment (U5, Z-P)"): the
//! token rule of C4 and K5 is (a) a maximal run of 43 characters of `A-Z`,
//! `a-z`, `0-9`, `-`, and `_` with at least one upper-case letter, one
//! lower-case letter, and one digit, and (b) any such run of 43, whatever
//! its characters, whose SHA-256 is the verifier of a token this daemon
//! issued. D1's test name, 43 characters without an upper-case letter or a
//! digit, is no longer refused in any member C4 and K5 cover.

use memprove_protocol::phase1::OperationId;
use memprove_store_sqlite::{NewClientToken, TokenCommand};
use serde_json::{Value, json};

use super::{Fixture, ok, problem};
use crate::{
    api::WireResponse,
    auth::verifier,
    catalog::{create_task, end_session},
    test_support::json_request,
};

/// D1's test name (F2): 43 characters, no upper-case letter, no digit.
const D1_TEST_NAME: &str = "test_calculate_metrics_rejects_empty_series";
/// 43 characters with all three classes: refused by its shape (a).
const MIXED: &str = "Ab3_cDe-4fGh5iJk6lMn7oPq8rSt9uVw0xYz1AbCd2E";
/// 43 lower-case letters and digits: not token-shaped; refused only when
/// this daemon issued it (b).
const LOWER_AND_DIGITS: &str = "u5p2issued0token0of0lower0letters0and0digit";
/// The same characters, one changed: never issued.
const NOT_ISSUED: &str = "u5p2issued0token0of0lower0letters0and0digiz";

fn detail(response: &WireResponse) -> String {
    serde_json::from_str::<Value>(&response.body).unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The detail of every refusal: the member, and the rule in words.
fn refusal(member: &str) -> String {
    format!(
        "{member} holds a string in the format of a memprove token (43 characters of A-Z, \
         a-z, 0-9, '-', and '_', with at least one upper-case letter, one lower-case letter, \
         and one digit) or a token this memproved issued; nothing was recorded"
    )
}

/// Issues a token whose secret is `secret` through the registry, as
/// `createClientToken` stores one (only its verifier).
fn issue(fixture: &Fixture, secret: &str) {
    let principal = fixture.harness.id_of(&fixture.control);
    let token_id = uuid::Uuid::now_v7().to_string();
    let verifier = verifier(secret);
    let key = fixture.key();
    let request = verifier.clone();
    let mut registry = fixture.state().registry.lock().unwrap();
    registry
        .create_client_token(
            &TokenCommand {
                principal_token_id: &principal,
                idempotency_key: &key,
                request_sha256: &request,
                now: "2026-10-18T00:00:00Z",
            },
            &NewClientToken {
                token_id: &token_id,
                client_label: "u5-p2",
                scopes: &["mcp-read"],
                verifier_sha256: &verifier,
                created_at: "2026-10-18T00:00:00Z",
            },
        )
        .unwrap();
}

/// What each member answers with `text` in it (inside a sentence), in
/// order: the statement texts, the objective, a Decision's content and
/// rationale, a Task's title and description, an argv element, and an end
/// reason. `(member, response, operation)`.
fn answers(fixture: &Fixture, text: &str) -> Vec<(&'static str, WireResponse, OperationId)> {
    let (evidence, snapshot, _) = fixture.passed_run_with_head();
    let sentence = format!("{text} passes");
    let checkpoint = |at: &str| {
        let mut body = fixture.checkpoint_body(&snapshot, &[&evidence]);
        match at {
            "fact" => body["verified_completed_work"][0]["text"] = json!(sentence),
            "open" => body["open_items"][0]["text"] = json!(sentence),
            "next" => body["next_action"]["text"] = json!(sentence),
            _ => body["objective"] = json!(sentence),
        }
        fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body)
    };
    let mut decision = fixture.decision_body(&sentence, &[], &[]);
    let content = fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision);
    decision["content"] = json!("c");
    decision["rationale"] = json!(sentence);
    let rationale = fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision);
    let task = |title: &str, description: &str| {
        let body = json!({
            "schema_version": 1, "project_id": fixture.project_id, "title": title,
            "description": description,
        });
        create_task(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &body.to_string(),
            ),
        )
    };
    let mut run = fixture.run_body("test_run", 0, Some(1));
    run["metadata"]["argv"] = json!(["python3", "-m", "pytest", "-k", text]);
    let argv = fixture.capture_with(&fixture.writer, &fixture.key(), &run);
    let session = fixture.session(&fixture.workspace_id, Some(&fixture.task_id));
    let body = json!({ "schema_version": 1, "expected_revision": 1, "reason": sentence });
    let end = end_session(
        fixture.state(),
        &json_request(
            Some(&fixture.writer),
            Some(&fixture.key()),
            &body.to_string(),
        ),
        &session,
    );
    vec![
        (
            "$.verified_completed_work[0].text",
            checkpoint("fact"),
            OperationId::CreateCheckpoint,
        ),
        (
            "$.open_items[0].text",
            checkpoint("open"),
            OperationId::CreateCheckpoint,
        ),
        (
            "$.next_action.text",
            checkpoint("next"),
            OperationId::CreateCheckpoint,
        ),
        (
            "$.objective",
            checkpoint("objective"),
            OperationId::CreateCheckpoint,
        ),
        ("$.content", content, OperationId::ProposeDecision),
        ("$.rationale", rationale, OperationId::ProposeDecision),
        ("$.title", task(&sentence, "d"), OperationId::CreateTask),
        (
            "$.description",
            task("t", &sentence),
            OperationId::CreateTask,
        ),
        ("$.metadata.argv[4]", argv, OperationId::CaptureEvidence),
        ("$.reason", end, OperationId::EndSession),
    ]
}

fn assert_accepted(fixture: &Fixture, text: &str) {
    for (member, response, operation) in answers(fixture, text) {
        assert!(
            matches!(response.status, 200 | 201),
            "{member} with {text:?}: {}",
            response.body
        );
        response.validate(Some(operation)).unwrap();
    }
}

fn assert_refused(fixture: &Fixture, text: &str) {
    let before = fixture.events().len();
    for (member, response, operation) in answers(fixture, text) {
        problem(&response, operation, 400, "invalid_request");
        assert_eq!(detail(&response), refusal(member));
        assert!(!response.body.contains(text), "{}", response.body);
    }
    // Only the runs and the Session the cases need were written.
    let written = fixture.events().len() - before;
    assert!(written <= 4, "{written} Events for refused writes");
}

/// D1's test name is accepted in every member C4 and K5 cover; so is a
/// 43-character run of lower-case letters and digits nobody issued.
#[test]
fn p2_an_identifier_of_43_characters_without_the_three_classes_is_accepted() {
    let fixture = Fixture::new();
    assert_eq!(D1_TEST_NAME.len(), 43);
    assert_accepted(&fixture, D1_TEST_NAME);
    issue(&fixture, LOWER_AND_DIGITS);
    assert_eq!(NOT_ISSUED.len(), 43);
    assert_accepted(&fixture, NOT_ISSUED);
}

/// (a) A run of 43 with an upper-case letter, a lower-case letter, and a
/// digit is refused in every member, by its shape alone.
#[test]
fn p2_a_run_of_the_three_classes_is_refused_by_its_shape() {
    let fixture = Fixture::new();
    assert_eq!(MIXED.len(), 43);
    assert_refused(&fixture, MIXED);
}

/// (b) A token this daemon issued is refused whatever its characters: one
/// of lower-case letters and digits only, which the shape lets through, and
/// the fixture's own random tokens.
#[test]
fn p2_a_token_this_daemon_issued_is_refused_whatever_its_characters() {
    let fixture = Fixture::new();
    assert_eq!(LOWER_AND_DIGITS.len(), 43);
    assert!(!memprove_domain::holds_token_shape(LOWER_AND_DIGITS));
    assert_accepted(&fixture, LOWER_AND_DIGITS);
    issue(&fixture, LOWER_AND_DIGITS);
    assert_refused(&fixture, LOWER_AND_DIGITS);
    let writer = fixture.writer.clone();
    assert_refused(&fixture, &writer);
    let reader = fixture.reader.clone();
    assert_refused(&fixture, &reader);
    // A revoked token is still one this daemon issued.
    let target = fixture.harness.id_of(&fixture.reader);
    ok(
        &fixture.harness.revoke(&fixture.control, &target),
        OperationId::RevokeClientToken,
        200,
    );
    assert_refused(&fixture, &reader);
}
