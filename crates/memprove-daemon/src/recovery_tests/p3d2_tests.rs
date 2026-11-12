//! Narrow tests of P3-D2 (T13): a withdrawn fact needs a reason, recorded
//! in its `claim.superseded@1` payload and shown by the Capsule; a Decision
//! is judged by the rule of a `verified_fact`; agent text is limited to
//! 4096 characters on write. Through the real handlers, store, and Git
//! probes.

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

impl Fixture {
    /// A passed test run: its Evidence, its Snapshot, and the Snapshot's
    /// `git_head` Evidence.
    pub(super) fn passed_run_with_head(&self) -> (String, String, String) {
        let passed = self.capture(&self.run_body("test_run", 0, Some(3)));
        let text = |path: &[&str]| {
            path.iter()
                .fold(&passed, |value, name| &value[*name])
                .as_str()
                .unwrap()
                .to_owned()
        };
        (
            text(&["evidence_id"]),
            text(&["snapshot", "snapshot_id"]),
            text(&["snapshot", "git_head_evidence_id"]),
        )
    }

    fn claim_by_id<'a>(capsule: &'a Value, claim_id: &str) -> &'a Value {
        capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|claim| claim["claim_id"] == claim_id)
            .unwrap_or_else(|| panic!("{claim_id} is not listed"))
    }
}

/// P3-D2 (Owner, T13 N2): withdrawing a fact without a reason is `400` and
/// writes nothing; with a reason the `claim.superseded` payload, the
/// Capsule's claim, the withdrawn item, and the header count carry it;
/// withdrawing an open item needs none; a reason without withdrawals is
/// `400`; a replay with the reason appends nothing, and the same key with
/// another reason conflicts.
#[test]
#[allow(clippy::too_many_lines)] // Refusal, acceptance, document, replay.
fn withdrawing_a_fact_needs_a_reason_that_the_capsule_shows() {
    let fixture = Fixture::new();
    let (e1, s1, _) = fixture.passed_run_with_head();
    let cp1 = ok(
        &fixture.checkpoint_with(
            &fixture.writer,
            &fixture.key(),
            &fixture.checkpoint_body(&s1, &[&e1]),
        ),
        OperationId::CreateCheckpoint,
        201,
    )["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let f1 = format!("{cp1}/verified_completed_work/0");
    let o1 = format!("{cp1}/open_items/0");
    let (e2, s2, _) = fixture.passed_run_with_head();
    let withdrawing = |withdrawals: &[&str], reason: Option<&str>| {
        let mut body = fixture.checkpoint_body(&s2, &[&e2]);
        body["verified_completed_work"][0]["text"] = json!("tests still pass");
        body["withdrawals"] = json!(withdrawals);
        if let Some(reason) = reason {
            body["withdrawal_reason"] = json!(reason);
        }
        body
    };

    let before = fixture.events().len();
    let refused = fixture.checkpoint_with(
        &fixture.writer,
        &fixture.key(),
        &withdrawing(&[&o1, &f1], None),
    );
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    assert_eq!(
        detail(&refused),
        "$.withdrawals[1] names a fact; withdrawing a fact needs $.withdrawal_reason, which \
         says why it no longer holds"
    );
    let mut lonely = fixture.checkpoint_body(&s2, &[&e2]);
    lonely["withdrawal_reason"] = json!("nothing to withdraw");
    let refused = fixture.checkpoint_with(&fixture.writer, &fixture.key(), &lonely);
    problem(
        &refused,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    assert!(
        detail(&refused).starts_with("$.withdrawal_reason is given without $.withdrawals"),
        "{}",
        detail(&refused)
    );
    let mut empty = withdrawing(&[&f1], None);
    empty["withdrawal_reason"] = json!("");
    problem(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &empty),
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    assert_eq!(fixture.events().len(), before, "nothing was written");

    // An open item needs no reason.
    let (e3, s3, _) = fixture.passed_run_with_head();
    let mut open_only = fixture.checkpoint_body(&s3, &[&e3]);
    open_only["withdrawals"] = json!([o1]);
    let cp_open = ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &open_only),
        OperationId::CreateCheckpoint,
        201,
    )["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let events = fixture.events();
    let open_edge = events.last().unwrap();
    assert_eq!(open_edge["event_type"], "claim.superseded");
    assert_eq!(
        open_edge["payload"],
        json!({ "task_id": fixture.task_id, "relation": "withdraws", "target": o1, "by": null })
    );

    // A fact with a reason.
    let reason = "the test only passed against a stale fixture";
    let key = fixture.key();
    let mut with_reason = fixture.checkpoint_body(&s3, &[&e3]);
    with_reason["verified_completed_work"][0]["text"] = json!("tests pass again");
    with_reason["withdrawals"] = json!([f1]);
    with_reason["withdrawal_reason"] = json!(reason);
    ok(
        &fixture.checkpoint_with(&fixture.writer, &key, &with_reason),
        OperationId::CreateCheckpoint,
        201,
    );
    let events = fixture.events();
    let edge = events.last().unwrap();
    assert_eq!(edge["event_type"], "claim.superseded");
    assert_eq!(edge["payload_schema"], "claim.superseded@1");
    assert_eq!(
        edge["payload"],
        json!({
            "task_id": fixture.task_id, "relation": "withdraws", "target": f1, "by": null,
            "reason": reason,
        })
    );

    let (_, capsule) = fixture.capsule(None);
    let withdrawn = Fixture::claim_by_id(&capsule, &f1);
    assert_eq!(withdrawn["standing"], "withdrawn");
    assert_eq!(withdrawn["withdrawal_reason"], reason);
    assert_eq!(
        withdrawn["verdict"], "verified",
        "withdrawal changes no verdict"
    );
    let open = Fixture::claim_by_id(&capsule, &o1);
    assert_eq!(open["standing"], "withdrawn");
    assert_eq!(open["withdrawal_reason"], Value::Null);
    let active = Fixture::claim_by_id(&capsule, &format!("{cp_open}/verified_completed_work/0"));
    assert_eq!(active["withdrawal_reason"], Value::Null);
    let continuation = capsule["continuation"].as_array().unwrap();
    assert_eq!(continuation[0]["args"]["withdrawn_facts"], 1);
    let item = continuation
        .iter()
        .find(|item| item["template"] == "withdrawn" && item["object_id"] == f1.as_str())
        .unwrap();
    assert_eq!(item["args"]["withdrawal_reason"], reason);

    // A replay appends nothing; the same key with another reason, or
    // without one, conflicts.
    let before = fixture.events().len();
    ok(
        &fixture.checkpoint_with(&fixture.writer, &key, &with_reason),
        OperationId::CreateCheckpoint,
        201,
    );
    assert_eq!(fixture.events().len(), before);
    let mut other = with_reason.clone();
    other["withdrawal_reason"] = json!("another reason");
    problem(
        &fixture.checkpoint_with(&fixture.writer, &key, &other),
        OperationId::CreateCheckpoint,
        409,
        "idempotency_conflict",
    );
}

/// P3-D2 (Owner, T13 N9): a Decision resting only on repository Evidence is
/// at best `partial`; one citing a fresh, passed test is `verified`.
#[test]
fn a_decision_is_verified_only_on_a_fresh_passed_test() {
    let fixture = Fixture::new();
    let (test, _, git_head) = fixture.passed_run_with_head();
    let git_only = fixture.decide("use JCS", &[&git_head], &[]);
    let tested = fixture.decide("keep the lexer", &[&test], &[]);
    let both = fixture.decide("ship it", &[&git_head, &test], &[]);
    let (_, capsule) = fixture.capsule(None);
    let verdict = |decision: &Value| {
        let claim = Fixture::claim_by_id(&capsule, decision["claim_id"].as_str().unwrap());
        assert_eq!(claim["kind"], "decision");
        (
            claim["verdict"].as_str().unwrap().to_owned(),
            claim["reasons"].clone(),
            claim["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["verdict"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        verdict(&git_only),
        (
            "partial".to_owned(),
            json!(["evidence_unavailable"]),
            vec!["verified".to_owned()]
        ),
        "the git_head Evidence itself is fresh"
    );
    assert_eq!(
        verdict(&tested),
        (
            "verified".to_owned(),
            json!([]),
            vec!["verified".to_owned()]
        )
    );
    assert_eq!(verdict(&both).0, "verified");
}

/// P3-D2 (Owner): a statement text, a Decision's content and rationale,
/// and a withdrawal reason hold at most 4096 characters; 4097 is `400`
/// naming the member, 4096 is accepted. Characters, not bytes: 4096
/// three-byte characters pass.
#[test]
#[allow(clippy::too_many_lines)] // Four members, each at and over the limit.
fn agent_text_holds_at_most_4096_characters() {
    let fixture = Fixture::new();
    let (e1, s1, _) = fixture.passed_run_with_head();
    let at_limit = "é".repeat(4096);
    let over = "x".repeat(4097);
    let checkpoint = |text: &str, open: &str, next: &str| {
        let mut body = fixture.checkpoint_body(&s1, &[&e1]);
        body["verified_completed_work"][0]["text"] = json!(text);
        body["open_items"][0]["text"] = json!(open);
        body["next_action"]["text"] = json!(next);
        body
    };
    let cases = [
        (
            checkpoint(&over, "o", "n"),
            "$.verified_completed_work[0].text",
        ),
        (checkpoint("t", &over, "n"), "$.open_items[0].text"),
        (checkpoint("t", "o", &over), "$.next_action.text"),
    ];
    let before = fixture.events().len();
    for (body, member) in cases {
        let response = fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body);
        problem(
            &response,
            OperationId::CreateCheckpoint,
            400,
            "invalid_request",
        );
        assert_eq!(
            detail(&response),
            format!("{member}: expected at most 4096 characters (P3-D2 limit for agent text)")
        );
    }
    let mut decision = fixture.decision_body(&over, &[], &[]);
    let response = fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision);
    problem(
        &response,
        OperationId::ProposeDecision,
        400,
        "invalid_request",
    );
    assert!(detail(&response).starts_with("$.content: expected at most 4096"));
    decision["content"] = json!("c");
    decision["rationale"] = json!(over);
    let response = fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision);
    problem(
        &response,
        OperationId::ProposeDecision,
        400,
        "invalid_request",
    );
    assert!(detail(&response).starts_with("$.rationale: expected at most 4096"));
    assert_eq!(fixture.events().len(), before, "nothing was written");

    // At the limit: every member is accepted.
    let cp1 = ok(
        &fixture.checkpoint_with(
            &fixture.writer,
            &fixture.key(),
            &checkpoint(&at_limit, &at_limit, &at_limit),
        ),
        OperationId::CreateCheckpoint,
        201,
    )["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut decision = fixture.decision_body(&at_limit, &[], &[]);
    decision["rationale"] = json!(at_limit);
    ok(
        &fixture.decision_with(Some(&fixture.writer), &fixture.key(), &decision),
        OperationId::ProposeDecision,
        201,
    );

    // The withdrawal reason.
    let (e2, s2, _) = fixture.passed_run_with_head();
    let withdrawal = |reason: &str| {
        let mut body = fixture.checkpoint_body(&s2, &[&e2]);
        body["withdrawals"] = json!([format!("{cp1}/verified_completed_work/0")]);
        body["withdrawal_reason"] = json!(reason);
        body
    };
    let response = fixture.checkpoint_with(&fixture.writer, &fixture.key(), &withdrawal(&over));
    problem(
        &response,
        OperationId::CreateCheckpoint,
        400,
        "invalid_request",
    );
    assert_eq!(
        detail(&response),
        "$.withdrawal_reason: expected at most 4096 characters (P3-D2 limit for agent text)"
    );
    ok(
        &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &withdrawal(&at_limit)),
        OperationId::CreateCheckpoint,
        201,
    );
    let (_, capsule) = fixture.capsule(None);
    assert_eq!(
        Fixture::claim_by_id(&capsule, &format!("{cp1}/verified_completed_work/0"))["withdrawal_reason"],
        at_limit.as_str()
    );
}
