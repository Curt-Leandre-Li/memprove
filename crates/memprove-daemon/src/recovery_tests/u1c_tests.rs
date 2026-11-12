//! Narrow tests of stage U-C of U1 (the U1 task plan,
//! section 4.1; ADR 0014, "Amendment (U1, U-C)"):
//!
//! - C4 (original #28, Owner O6): a statement body that holds a string in
//!   the format of a memprove token is refused before anything is
//!   written, with a fixed `400 invalid_request` that names the member and
//!   never repeats the text.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, ok, problem};
use crate::api::WireResponse;

fn detail(response: &WireResponse) -> String {
    serde_json::from_str::<Value>(&response.body).unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// C4: each member that carries an agent's text, with a token in it, is
/// `400` with the detail of that member and writes nothing (the key is not
/// spent either); a text that only looks like a token is accepted.
#[test]
#[allow(clippy::too_many_lines)] // Six members, then the look-alikes.
fn a_body_with_a_token_in_it_is_refused_before_anything_is_written() {
    let fixture = Fixture::new();
    let (e1, s1, _) = fixture.passed_run_with_head();
    // U5 P2: a token this daemon issued, refused by its shape (a) or by
    // its digest (b) whatever its characters; a random secret of no daemon
    // lacks a digit about once in 1,500 and would pass the shape.
    let token = fixture.writer.clone();
    assert_eq!(token.len(), 43);
    let before = fixture.events().len();
    let refuse = |response: &WireResponse, operation: OperationId, member: &str| {
        problem(response, operation, 400, "invalid_request");
        assert_eq!(
            detail(response),
            format!(
                "{member} holds a string in the format of a memprove token (43 characters of \
                 A-Z, a-z, 0-9, '-', and '_', with at least one upper-case letter, one \
                 lower-case letter, and one digit) or a token this memproved issued; nothing \
                 was recorded"
            )
        );
        assert!(
            !response.body.contains(&token),
            "the refusal repeats the text: {}",
            response.body
        );
    };

    // The three sections of a Checkpoint, each the only one with a token,
    // with the token alone and inside a sentence.
    let checkpoint = |text: &str, open: &str, next: &str| {
        let mut body = fixture.checkpoint_body(&s1, &[&e1]);
        body["verified_completed_work"][0]["text"] = json!(text);
        body["open_items"][0]["text"] = json!(open);
        body["next_action"]["text"] = json!(next);
        body
    };
    let sentence = format!("the key is {token}, rotate it");
    for (body, member) in [
        (
            checkpoint(&token, "o", "n"),
            "$.verified_completed_work[0].text",
        ),
        (checkpoint("t", &sentence, "n"), "$.open_items[0].text"),
        (checkpoint("t", "o", &sentence), "$.next_action.text"),
    ] {
        let key = fixture.key();
        let response = fixture.checkpoint_with(&fixture.writer, &key, &body);
        refuse(&response, OperationId::CreateCheckpoint, member);
        // The key was not spent: the same key with a clean body is created.
        ok(
            &fixture.checkpoint_with(&fixture.writer, &key, &checkpoint("t", "o", "n")),
            OperationId::CreateCheckpoint,
            201,
        );
    }
    let first = fixture.events().len();

    // A Decision's content and rationale.
    let mut decision = fixture.decision_body(&sentence, &[], &[]);
    let response = fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision);
    refuse(&response, OperationId::ProposeDecision, "$.content");
    decision["content"] = json!("c");
    decision["rationale"] = json!(token.clone());
    let response = fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision);
    refuse(&response, OperationId::ProposeDecision, "$.rationale");
    assert_eq!(fixture.events().len(), first, "no Decision was written");

    // A withdrawal reason (the claim exists: the Checkpoint above).
    let withdrawn = format!(
        "{}/open_items/0",
        ok(
            &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &checkpoint("t", "o", "n")),
            OperationId::CreateCheckpoint,
            201,
        )["checkpoint_id"]
            .as_str()
            .unwrap()
    );
    let after_withdrawn_claim = fixture.events().len();
    let mut withdrawing = fixture.checkpoint_body(&s1, &[&e1]);
    withdrawing["withdrawals"] = json!([withdrawn]);
    withdrawing["withdrawal_reason"] = json!(format!("leaked {token}"));
    let response = fixture.checkpoint_with(&fixture.writer, &fixture.key(), &withdrawing);
    refuse(
        &response,
        OperationId::CreateCheckpoint,
        "$.withdrawal_reason",
    );
    assert_eq!(
        fixture.events().len(),
        after_withdrawn_claim,
        "no Checkpoint and no withdrawal was written"
    );
    assert!(before < first);

    // What only looks like a token is not one: 42 characters, a run of 44,
    // 43 with a character outside the alphabet in it, a run that a
    // character outside the alphabet ends, a UUID, a SHA-256 digest.
    let forty_two = &token[..42];
    let forty_four = format!("{token}A");
    let broken = format!("{}.{}", &token[..20], &token[21..]);
    let delimited = format!("token={token}");
    let uuid = fixture.task_id.clone();
    let digest = "a".repeat(64);
    let accepted = [
        forty_two.to_owned(),
        forty_four,
        broken,
        uuid,
        digest,
        format!("{} {}", &token[..21], &token[21..]),
    ];
    for text in &accepted {
        assert!(
            !text.contains(&token) || text.len() > 43,
            "a look-alike that is a token: {text}"
        );
        let mut decision = fixture.decision_body(text, &[], &[]);
        decision["rationale"] = json!(text.clone());
        ok(
            &fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision),
            OperationId::ProposeDecision,
            201,
        );
        ok(
            &fixture.checkpoint_with(
                &fixture.writer,
                &fixture.key(),
                &checkpoint(text, text, text),
            ),
            OperationId::CreateCheckpoint,
            201,
        );
    }
    // A token with a delimiter next to it is still a token.
    let response = fixture.checkpoint_with(
        &fixture.writer,
        &fixture.key(),
        &checkpoint(&delimited, "o", "n"),
    );
    refuse(
        &response,
        OperationId::CreateCheckpoint,
        "$.verified_completed_work[0].text",
    );
}
