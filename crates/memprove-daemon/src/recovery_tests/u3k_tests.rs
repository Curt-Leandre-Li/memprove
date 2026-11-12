//! Narrow tests of stage X-K of U3 (the U3 task plan,
//! section 4.1; ADR 0014, "Amendment (U3, X-K)"):
//!
//! - K5 (V-N-4, original #66, Owner O4): C4's refusal covers, besides the
//!   statement bodies, a Task's `title` and `description`, a Checkpoint's
//!   `objective`, an end's `reason`, and each element of an Evidence's
//!   `argv`. Each is refused
//!   with C4's fixed detail naming the member, before anything is written,
//!   and the Idempotency-Key is not spent; a look-alike is accepted.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, ok, problem};
use crate::{
    api::WireResponse,
    catalog::{create_task, end_session},
    test_support::json_request,
};

fn detail(response: &WireResponse) -> String {
    serde_json::from_str::<Value>(&response.body).unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The token at the start, in the middle, and at the end of a text.
fn placed(token: &str) -> [String; 3] {
    [
        format!("{token} first"),
        format!("in the {token} middle"),
        format!("last:{token}"),
    ]
}

#[test]
#[allow(clippy::too_many_lines)] // Five members, each refused and then accepted.
fn k5_a_token_in_a_title_description_objective_reason_or_argv_is_refused() {
    let fixture = Fixture::new();
    // U5 P2: a token this daemon issued, refused by its shape (a) or by
    // its digest (b) whatever its characters; a random secret of no daemon
    // lacks a digit about once in 1,500 and would pass the shape.
    let token = fixture.writer.clone();
    assert_eq!(token.len(), 43);
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
        assert!(!response.body.contains(&token), "{}", response.body);
    };
    let look_alikes = [token[..42].to_owned(), format!("{token}A")];
    let before = fixture.events().len();

    // createTask: title and description.
    let task_body = |title: &str, description: Option<&str>| {
        let mut body = json!({
            "schema_version": 1, "project_id": fixture.project_id, "title": title,
        });
        if let Some(description) = description {
            body["description"] = json!(description);
        }
        body
    };
    let task_with = |key: &str, body: &Value| {
        create_task(
            fixture.state(),
            &json_request(Some(&fixture.writer), Some(key), &body.to_string()),
        )
    };
    for text in placed(&token) {
        for (body, member) in [
            (task_body(&text, None), "$.title"),
            (task_body("t", Some(&text)), "$.description"),
        ] {
            let key = fixture.key();
            refuse(&task_with(&key, &body), OperationId::CreateTask, member);
            // The key was not spent: the same key with a clean body works.
            ok(
                &task_with(&key, &task_body("t", None)),
                OperationId::CreateTask,
                201,
            );
        }
    }
    let tasks = fixture.events().len();
    assert_eq!(tasks, before + 6, "only the six clean Tasks were written");

    // captureEvidence: each element of argv.
    for text in placed(&token) {
        let mut body = fixture.run_body("command_run", 0, None);
        body["metadata"]["argv"] = json!(["sh", "-c", "true", text]);
        let key = fixture.key();
        refuse(
            &fixture.capture_with(&fixture.writer, &key, &body),
            OperationId::CaptureEvidence,
            "$.metadata.argv[3]",
        );
        let mut first = fixture.run_body("command_run", 0, None);
        first["metadata"]["argv"] = json!([text, "x"]);
        refuse(
            &fixture.capture_with(&fixture.writer, &fixture.key(), &first),
            OperationId::CaptureEvidence,
            "$.metadata.argv[0]",
        );
        ok(
            &fixture.capture_with(
                &fixture.writer,
                &key,
                &fixture.run_body("command_run", 0, None),
            ),
            OperationId::CaptureEvidence,
            201,
        );
    }
    let (evidence, snapshot, _) = fixture.passed_run_with_head();

    // createCheckpoint: objective.
    let after_runs = fixture.events().len();
    for text in placed(&token) {
        let mut body = fixture.checkpoint_body(&snapshot, &[&evidence]);
        body["objective"] = json!(text);
        refuse(
            &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
            OperationId::CreateCheckpoint,
            "$.objective",
        );
    }
    assert_eq!(
        fixture.events().len(),
        after_runs,
        "no Checkpoint was written"
    );

    // endSession: reason.
    let end_with = |reason: &str| {
        let body = json!({ "schema_version": 1, "expected_revision": 1, "reason": reason });
        end_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                &body.to_string(),
            ),
            &fixture.session_id,
        )
    };
    for text in placed(&token) {
        refuse(&end_with(&text), OperationId::EndSession, "$.reason");
    }
    assert_eq!(
        fixture.events().len(),
        after_runs,
        "the Session did not end"
    );

    // Look-alikes are accepted in every member.
    for text in &look_alikes {
        ok(
            &task_with(&fixture.key(), &task_body(text, Some(text))),
            OperationId::CreateTask,
            201,
        );
        let mut run = fixture.run_body("command_run", 0, None);
        run["metadata"]["argv"] = json!(["echo", text]);
        fixture.capture(&run);
        let mut body = fixture.checkpoint_body(&snapshot, &[&evidence]);
        body["objective"] = json!(text);
        ok(
            &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
            OperationId::CreateCheckpoint,
            201,
        );
    }
    ok(&end_with(&look_alikes[0]), OperationId::EndSession, 200);
}
