//! Narrow tests of ADR 0013 P3-C: the `claim.superseded` edges of
//! `createCheckpoint` (`supersedes`, `withdrawals`) and `proposeDecision`,
//! through the real handlers, store, and Git probes.

use memprove_protocol::phase1::OperationId;
use serde_json::{Value, json};

use super::{Fixture, ok, problem};
use crate::{
    api::WireResponse, events::ingest_event, recovery::propose_decision, test_support::json_request,
};

impl Fixture {
    pub(super) fn decision_with(
        &self,
        bearer: Option<&str>,
        key: &str,
        body: &Value,
    ) -> WireResponse {
        propose_decision(
            self.state(),
            &json_request(bearer, Some(key), &body.to_string()),
        )
    }

    /// A `proposeDecision` body on the fixture's Task and Session.
    pub(super) fn decision_body(
        &self,
        content: &str,
        evidence: &[&str],
        supersedes: &[&str],
    ) -> Value {
        json!({
            "schema_version": 1,
            "task_id": self.task_id,
            "session_id": self.session_id,
            "content": content,
            "rationale": "simplest that holds",
            "evidence_ids": evidence,
            "supersedes": supersedes,
        })
    }

    pub(super) fn decide(&self, content: &str, evidence: &[&str], supersedes: &[&str]) -> Value {
        ok(
            &self.decision_with(
                Some(&self.writer),
                &self.key(),
                &self.decision_body(content, evidence, supersedes),
            ),
            OperationId::ProposeDecision,
            201,
        )
    }

    /// A passed run: its Evidence and Snapshot.
    fn passed_run(&self) -> (String, String) {
        let passed = self.capture(&self.run_body("test_run", 0, Some(3)));
        (
            passed["evidence_id"].as_str().unwrap().to_owned(),
            passed["snapshot"]["snapshot_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    }

    /// A Checkpoint over `snapshot` with `facts` and `open` (each a text and
    /// its `supersedes`), and `withdrawals` with a reason (P3-D2: a
    /// withdrawn fact needs one).
    fn edged_body(
        &self,
        snapshot: &str,
        evidence: &str,
        facts: &[(&str, &[&str])],
        open: &[(&str, &[&str])],
        withdrawals: &[&str],
    ) -> Value {
        let statement = |classification: &str, (text, supersedes): &(&str, &[&str])| {
            let mut item = json!({
                "classification": classification, "text": text, "evidence_ids": [evidence],
            });
            if !supersedes.is_empty() {
                item["supersedes"] = json!(supersedes);
            }
            item
        };
        let mut body = json!({
            "schema_version": 1,
            "workspace_id": self.workspace_id,
            "task_id": self.task_id,
            "session_id": self.session_id,
            "snapshot_id": snapshot,
            "objective": "make the tests pass",
            "verified_completed_work": facts.iter().map(|fact| statement("verified_fact", fact)).collect::<Vec<_>>(),
            "open_items": open.iter().map(|item| statement("unknown", item)).collect::<Vec<_>>(),
            "next_action": { "classification": "recommendation", "text": "carry on" },
        });
        if !withdrawals.is_empty() {
            body["withdrawals"] = json!(withdrawals);
            body["withdrawal_reason"] = json!("no longer holds");
        }
        body
    }

    fn edged(&self, body: &Value) -> String {
        ok(
            &self.checkpoint_with(&self.writer, &self.key(), body),
            OperationId::CreateCheckpoint,
            201,
        )["checkpoint_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn claim<'a>(capsule: &'a Value, claim_id: &str) -> &'a Value {
        capsule["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|claim| claim["claim_id"] == claim_id)
            .unwrap_or_else(|| panic!("{claim_id} is not listed"))
    }
}

fn of_type<'a>(events: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["event_type"] == event_type)
        .collect()
}

fn detail(response: &WireResponse) -> String {
    serde_json::from_str::<Value>(&response.body).unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// P3-C: each `supersedes` and `withdrawals` item is one `claim.superseded`
/// Event of memproved, right after `checkpoint.created`, on the Session;
/// the Capsule applies them; a replay appends nothing; the same key with
/// other edges conflicts.
#[test]
#[allow(clippy::too_many_lines)] // Two Checkpoints, their Events, replay, conflict.
fn checkpoint_edges_are_one_event_each_in_the_checkpoint_transaction() {
    let fixture = Fixture::new();
    let (e1, s1) = fixture.passed_run();
    let cp1 = fixture.edged(&fixture.edged_body(
        &s1,
        &e1,
        &[("F1 tests pass", &[])],
        &[("O1 docs", &[]), ("O3 benchmarks", &[])],
        &[],
    ));
    let o1 = format!("{cp1}/open_items/0");
    let o3 = format!("{cp1}/open_items/1");
    let (e2, s2) = fixture.passed_run();
    let body = fixture.edged_body(&s2, &e2, &[("F2 docs written", &[&o1])], &[], &[&o3]);
    let key = fixture.key();
    let created = ok(
        &fixture.checkpoint_with(&fixture.writer, &key, &body),
        OperationId::CreateCheckpoint,
        201,
    );
    let cp2 = created["checkpoint_id"].as_str().unwrap().to_owned();
    let events = fixture.events();
    let announced = events
        .iter()
        .position(|event| {
            event["event_type"] == "checkpoint.created"
                && event["payload"]["checkpoint"]["checkpoint_id"] == cp2.as_str()
        })
        .unwrap();
    let edges = &events[announced + 1..];
    assert_eq!(edges.len(), 2);
    // memproved's producer sequence is the Project sequence.
    let sequence = events[announced]["producer"]["sequence"].as_u64().unwrap();
    for (offset, edge) in edges.iter().enumerate() {
        assert_eq!(edge["event_type"], "claim.superseded");
        assert_eq!(edge["payload_schema"], "claim.superseded@1");
        assert_eq!(edge["producer"]["name"], "continuityd");
        assert_eq!(edge["session_id"], fixture.session_id.as_str());
        assert_eq!(edge["workspace_id"], fixture.workspace_id.as_str());
        assert_eq!(
            edge["producer"]["sequence"].as_u64().unwrap(),
            sequence + 1 + offset as u64
        );
    }
    assert_eq!(
        edges[0]["payload"],
        json!({
            "task_id": fixture.task_id, "relation": "supersedes", "target": o1,
            "by": format!("{cp2}/verified_completed_work/0"),
        })
    );
    assert_eq!(
        edges[1]["payload"],
        json!({
            "task_id": fixture.task_id, "relation": "withdraws", "target": o3, "by": null,
            "reason": "no longer holds",
        })
    );

    let (_, capsule) = fixture.capsule(None);
    let f2 = Fixture::claim(&capsule, &format!("{cp2}/verified_completed_work/0"));
    let first = Fixture::claim(&capsule, &o1);
    assert_eq!(first["standing"], "superseded");
    assert_eq!(first["superseded_by"][0]["number"], f2["number"]);
    assert_eq!(Fixture::claim(&capsule, &o3)["standing"], "withdrawn");
    assert_eq!(f2["standing"], "active");

    // A replay returns the same Checkpoint and appends nothing.
    let before = fixture.events().len();
    let replayed = ok(
        &fixture.checkpoint_with(&fixture.writer, &key, &body),
        OperationId::CreateCheckpoint,
        201,
    );
    assert_eq!(replayed["checkpoint_id"], cp2.as_str());
    assert_eq!(fixture.events().len(), before);
    // The same key with other edges, or none, conflicts.
    let mut other = body.clone();
    other["withdrawals"] = json!([format!("{cp1}/verified_completed_work/0")]);
    let mut none = body.clone();
    none.as_object_mut().unwrap().remove("withdrawals");
    none.as_object_mut().unwrap().remove("withdrawal_reason");
    none["verified_completed_work"][0]
        .as_object_mut()
        .unwrap()
        .remove("supersedes");
    for changed in [other, none] {
        problem(
            &fixture.checkpoint_with(&fixture.writer, &key, &changed),
            OperationId::CreateCheckpoint,
            409,
            "idempotency_conflict",
        );
    }
    assert_eq!(fixture.events().len(), before);
}

/// P3-C: a target must be an earlier readable claim of the same Task, named
/// once; otherwise `400` names the location and the rule, and nothing is
/// written.
#[test]
#[allow(clippy::too_many_lines)] // Every refusal of an edge target.
fn checkpoint_edges_refuse_targets_outside_the_task_unknown_or_repeated() {
    let fixture = Fixture::new();
    let (e1, s1) = fixture.passed_run();
    let cp1 = fixture.edged(&fixture.edged_body(&s1, &e1, &[("F1", &[])], &[("O1", &[])], &[]));
    let o1 = format!("{cp1}/open_items/0");

    // A Checkpoint and a Decision of another Task of the same Project.
    let other_task = fixture.task(Some(&fixture.workspace_id.clone()));
    let other_session = fixture.session(&fixture.workspace_id.clone(), Some(&other_task));
    let mut run = fixture.run_body("test_run", 0, Some(3));
    run["task_id"] = json!(other_task);
    run["session_id"] = json!(other_session);
    let other_run = fixture.capture(&run);
    let mut other_body = fixture.checkpoint_body(
        other_run["snapshot"]["snapshot_id"].as_str().unwrap(),
        &[other_run["evidence_id"].as_str().unwrap()],
    );
    other_body["task_id"] = json!(other_task);
    other_body["session_id"] = json!(other_session);
    let other_cp = fixture.edged(&other_body);
    let mut other_decision = fixture.decision_body("other", &[], &[]);
    other_decision["task_id"] = json!(other_task);
    other_decision["session_id"] = json!(other_session);
    let other_decision = ok(
        &fixture.decision_with(Some(&fixture.writer), &fixture.key(), &other_decision),
        OperationId::ProposeDecision,
        201,
    );

    let (e2, s2) = fixture.passed_run();
    let unknown_checkpoint = uuid::Uuid::now_v7();
    let unknown_event = uuid::Uuid::now_v7();
    let cases: Vec<(Value, &str)> = vec![
        (
            fixture.edged_body(
                &s2,
                &e2,
                &[("F2", &[&format!("{other_cp}/open_items/0")])],
                &[],
                &[],
            ),
            "$.verified_completed_work[0].supersedes[0] names a claim of another Task",
        ),
        (
            fixture.edged_body(
                &s2,
                &e2,
                &[],
                &[],
                &[other_decision["claim_id"].as_str().unwrap()],
            ),
            "$.withdrawals[0] names a claim of another Task",
        ),
        (
            fixture.edged_body(
                &s2,
                &e2,
                &[],
                &[("O2", &[&format!("{unknown_checkpoint}/open_items/0")])],
                &[],
            ),
            "$.open_items[0].supersedes[0] names no recorded claim",
        ),
        (
            fixture.edged_body(&s2, &e2, &[], &[], &[&format!("decision/{unknown_event}")]),
            "$.withdrawals[0] names no recorded claim",
        ),
        (
            fixture.edged_body(&s2, &e2, &[], &[], &[&format!("{cp1}/open_items/7")]),
            "$.withdrawals[0] names no statement of that Checkpoint",
        ),
        (
            fixture.edged_body(&s2, &e2, &[("F2", &[&o1]), ("F3", &[&o1])], &[], &[]),
            "$.verified_completed_work[1].supersedes[0] names a claim this request already names",
        ),
        (
            fixture.edged_body(&s2, &e2, &[("F2", &[&o1])], &[], &[&o1]),
            "$.withdrawals[0] names a claim this request already names",
        ),
    ];
    let before = fixture.events().len();
    for (body, expected) in cases {
        let response = fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body);
        problem(
            &response,
            OperationId::CreateCheckpoint,
            400,
            "invalid_request",
        );
        assert!(detail(&response).starts_with(expected), "{}", response.body);
    }
    // A malformed claim ID, or one named twice in one array, is refused by
    // the request schema.
    for withdrawals in [
        json!(["nope"]),
        json!([o1, o1]),
        json!([format!("{cp1}/open_items/01")]),
    ] {
        let mut body = fixture.edged_body(&s2, &e2, &[], &[], &[]);
        body["withdrawals"] = withdrawals;
        problem(
            &fixture.checkpoint_with(&fixture.writer, &fixture.key(), &body),
            OperationId::CreateCheckpoint,
            400,
            "invalid_request",
        );
    }
    assert_eq!(fixture.events().len(), before);
}

/// P3-C: a Checkpoint cannot name one of its own statements (itself, or a
/// sibling, which is not earlier in the ledger). The wire cannot name a
/// Checkpoint before it exists, so the store is called with the identity.
#[test]
fn checkpoint_edges_refuse_the_claim_itself_and_its_siblings() {
    use memprove_domain::{
        CheckpointClassification, CheckpointId, CheckpointStatement, ClaimId, ClaimRef,
        EdgeRelation, EventId, EvidenceId, IdempotencyContext, IdempotencyKey, OperationId as Op,
        PrincipalId, ProposedEdge, RepositorySnapshotId, SessionId, Sha256Digest, StatementSection,
        TaskId, UtcTimestamp, WorkspaceId,
    };
    use memprove_store_sqlite::{CheckpointDraft, StoreError};

    let fixture = Fixture::new();
    let (evidence, snapshot) = fixture.passed_run();
    let checkpoint_id = CheckpointId::new();
    let claim = |section, ordinal| {
        ClaimId::Statement(ClaimRef {
            checkpoint_id: checkpoint_id.clone(),
            section,
            ordinal,
        })
    };
    let fact = claim(StatementSection::VerifiedCompletedWork, 0);
    let open = claim(StatementSection::OpenItems, 0);
    let statement = |classification, text: &str, evidence: Vec<EvidenceId>| {
        CheckpointStatement::new(classification, text.to_owned(), evidence).unwrap()
    };
    for (target, by, expected) in [
        (
            open.clone(),
            open.clone(),
            "$.x names the claim that declares it",
        ),
        (
            fact.clone(),
            open.clone(),
            "$.x names a claim recorded by this same request",
        ),
    ] {
        let draft = CheckpointDraft {
            checkpoint_id: checkpoint_id.clone(),
            task_id: TaskId::parse(&fixture.task_id).unwrap(),
            session_id: SessionId::parse(&fixture.session_id).unwrap(),
            workspace_id: WorkspaceId::parse(&fixture.workspace_id).unwrap(),
            snapshot_id: RepositorySnapshotId::parse(&snapshot).unwrap(),
            objective: "o".to_owned(),
            verified_completed_work: vec![statement(
                CheckpointClassification::VerifiedFact,
                "F",
                vec![EvidenceId::parse(&evidence).unwrap()],
            )],
            open_items: vec![statement(
                CheckpointClassification::Unknown,
                "O",
                Vec::new(),
            )],
            next_action: statement(CheckpointClassification::Recommendation, "N", Vec::new()),
            created_at: UtcTimestamp::now(),
            edges: vec![ProposedEdge {
                relation: EdgeRelation::Supersedes,
                target,
                by: Some(by),
                reason: None,
                at: "$.x".to_owned(),
            }],
            cited: memprove_engine::recovery::RequestedCitations::default(),
        };
        let refused = fixture
            .store()
            .create_checkpoint(
                &draft,
                &EventId::new(),
                &UtcTimestamp::now(),
                &IdempotencyContext {
                    principal_id: PrincipalId::parse("p").unwrap(),
                    operation_id: Op::parse("createCheckpoint").unwrap(),
                    key: IdempotencyKey::parse(&fixture.key()).unwrap(),
                },
                &Sha256Digest::of_bytes(b"request"),
            )
            .unwrap_err();
        match refused {
            StoreError::RecoveryInputRejected(detail) => {
                assert!(detail.starts_with(expected), "{detail}");
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(of_type(&fixture.events(), "checkpoint.created").is_empty());
}

/// ADR 0013 D2 (P3-C): `proposeDecision` appends `decision.recorded` and one
/// `claim.superseded` per `supersedes` item in one transaction; the Capsule
/// lists the Decision; a replay appends nothing; the Session must be active
/// and continue the Task; Evidence must be the Task's.
#[test]
#[allow(clippy::too_many_lines)] // Every proposeDecision outcome.
fn propose_decision_records_the_decision_and_its_edges() {
    let fixture = Fixture::new();
    let op = OperationId::ProposeDecision;
    let (evidence, snapshot) = fixture.passed_run();
    let cp = fixture.edged(&fixture.edged_body(
        &snapshot,
        &evidence,
        &[("F1", &[])],
        &[("O1", &[])],
        &[],
    ));
    let key = fixture.key();
    let body = fixture.decision_body("use JCS", &[&evidence], &[]);
    let d1 = ok(
        &fixture.decision_with(Some(&fixture.writer), &key, &body),
        op,
        201,
    );
    let d1_id = d1["decision_id"].as_str().unwrap().to_owned();
    assert_eq!(d1["id"], d1_id.as_str());
    assert_eq!(d1["claim_id"], format!("decision/{d1_id}"));
    assert_eq!(d1["content"], "use JCS");
    assert_eq!(d1["rationale"], "simplest that holds");
    assert_eq!(d1["evidence_ids"], json!([evidence]));
    assert_eq!(d1["supersedes"], json!([]));
    let events = fixture.events();
    let recorded = of_type(&events, "decision.recorded");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["event_id"], d1_id.as_str());
    assert_eq!(recorded[0]["producer"]["name"], "continuityd");
    assert_eq!(recorded[0]["session_id"], fixture.session_id.as_str());
    assert_eq!(recorded[0]["producer"]["sequence"], d1["project_sequence"]);
    assert_eq!(
        recorded[0]["payload"],
        json!({
            "task_id": fixture.task_id, "text": "use JCS", "rationale": "simplest that holds",
            "evidence_ids": [evidence],
        })
    );
    // Replay: same Decision, nothing appended; other content conflicts.
    let before = fixture.events().len();
    let replayed = ok(
        &fixture.decision_with(Some(&fixture.writer), &key, &body),
        op,
        201,
    );
    assert_eq!(replayed, d1);
    assert_eq!(fixture.events().len(), before);
    problem(
        &fixture.decision_with(
            Some(&fixture.writer),
            &key,
            &fixture.decision_body("use CBOR", &[&evidence], &[]),
        ),
        op,
        409,
        "idempotency_conflict",
    );

    // D2 supersedes D1 and the open item: one edge each, after it.
    let o1 = format!("{cp}/open_items/0");
    let d1_claim = format!("decision/{d1_id}");
    let d2 = fixture.decide("use JCS everywhere", &[], &[&d1_claim, &o1]);
    let d2_claim = d2["claim_id"].as_str().unwrap().to_owned();
    assert_eq!(d2["supersedes"], json!([d1_claim, o1]));
    let events = fixture.events();
    let edges = of_type(&events, "claim.superseded");
    assert_eq!(
        edges
            .iter()
            .map(|edge| edge["payload"].clone())
            .collect::<Vec<_>>(),
        [
            json!({ "task_id": fixture.task_id, "relation": "supersedes", "target": d1_claim, "by": d2_claim }),
            json!({ "task_id": fixture.task_id, "relation": "supersedes", "target": o1, "by": d2_claim }),
        ]
    );
    assert_eq!(
        edges[0]["producer"]["sequence"].as_u64().unwrap(),
        d2["project_sequence"].as_u64().unwrap() + 1
    );
    let (_, capsule) = fixture.capsule(None);
    let first = Fixture::claim(&capsule, &d1_claim);
    let second = Fixture::claim(&capsule, &d2_claim);
    assert_eq!(first["kind"], "decision");
    assert_eq!(first["standing"], "superseded");
    assert_eq!(first["superseded_by"][0]["number"], second["number"]);
    assert_eq!(second["standing"], "active");
    assert_eq!(second["rationale"], "simplest that holds");
    assert_eq!(Fixture::claim(&capsule, &o1)["standing"], "superseded");
    // A replay of D2 reads its edges back from the ledger.
    let replay_key = fixture.key();
    let again = fixture.decision_body("again", &[], &[&o1]);
    let first_reply = ok(
        &fixture.decision_with(Some(&fixture.writer), &replay_key, &again),
        op,
        201,
    );
    let second_reply = ok(
        &fixture.decision_with(Some(&fixture.writer), &replay_key, &again),
        op,
        201,
    );
    assert_eq!(first_reply, second_reply);
    assert_eq!(second_reply["supersedes"], json!([o1]));

    // Refusals write nothing.
    let before = fixture.events().len();
    let other_task = fixture.task(Some(&fixture.workspace_id.clone()));
    let other_session = fixture.session(&fixture.workspace_id.clone(), Some(&other_task));
    let mut run = fixture.run_body("test_run", 0, Some(1));
    run["task_id"] = json!(other_task);
    run["session_id"] = json!(other_session);
    let foreign = fixture.capture(&run)["evidence_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let unknown = uuid::Uuid::now_v7().to_string();
    let before_refusals = fixture.events().len();
    assert!(before_refusals > before);
    for (body, expected) in [
        (
            fixture.decision_body("x", &[&foreign], &[]),
            "$.evidence_ids[0] names Evidence of another Task",
        ),
        (
            fixture.decision_body("x", &[&unknown], &[]),
            "$.evidence_ids[0] names no recorded Evidence",
        ),
        (fixture.decision_body("x", &[], &[&d2_claim, &d2_claim]), ""),
        (
            fixture.decision_body("x", &[], &[&format!("decision/{unknown}")]),
            "$.supersedes[0] names no recorded claim",
        ),
    ] {
        let response = fixture.decision_with(Some(&fixture.writer), &fixture.key(), &body);
        problem(&response, op, 400, "invalid_request");
        assert!(detail(&response).starts_with(expected), "{}", response.body);
    }
    let mut not_continued = fixture.decision_body("x", &[], &[]);
    not_continued["session_id"] = json!(other_session);
    problem(
        &fixture.decision_with(Some(&fixture.writer), &fixture.key(), &not_continued),
        op,
        400,
        "invalid_request",
    );
    for member in ["task_id", "session_id"] {
        let mut body = fixture.decision_body("x", &[], &[]);
        body[member] = json!(unknown);
        problem(
            &fixture.decision_with(Some(&fixture.writer), &fixture.key(), &body),
            op,
            404,
            "resource_not_found",
        );
    }
    let mut empty = fixture.decision_body("x", &[], &[]);
    empty["content"] = json!("");
    problem(
        &fixture.decision_with(Some(&fixture.writer), &fixture.key(), &empty),
        op,
        400,
        "invalid_request",
    );
    assert_eq!(fixture.events().len(), before_refusals);

    // An ended Session records no Decision.
    ok(
        &crate::catalog::end_session(
            fixture.state(),
            &json_request(
                Some(&fixture.writer),
                Some(&fixture.key()),
                r#"{"schema_version":1,"expected_revision":1}"#,
            ),
            &fixture.session_id,
        ),
        OperationId::EndSession,
        200,
    );
    problem(
        &fixture.decision_with(
            Some(&fixture.writer),
            &fixture.key(),
            &fixture.decision_body("late", &[], &[]),
        ),
        op,
        409,
        "invalid_transition",
    );
}

/// P3-C: `proposeDecision` takes `control` or `mcp-write`; `mcp-read` is
/// `403` and no token `401`. Clients still cannot write the three reserved
/// Event types.
#[test]
fn propose_decision_scopes_and_reserved_types() {
    let fixture = Fixture::new();
    let op = OperationId::ProposeDecision;
    let body = fixture.decision_body("d", &[], &[]);
    for bearer in [&fixture.writer, &fixture.control] {
        ok(
            &fixture.decision_with(Some(bearer), &fixture.key(), &body),
            op,
            201,
        );
    }
    problem(
        &fixture.decision_with(Some(&fixture.reader), &fixture.key(), &body),
        op,
        403,
        "policy_denied",
    );
    problem(
        &fixture.decision_with(None, &fixture.key(), &body),
        op,
        401,
        "authentication_required",
    );
    for reserved in ["claim.superseded", "decision.recorded", "session.crashed"] {
        let envelope = json!({
            "envelope_version": "1.0",
            "event_id": uuid::Uuid::now_v7().to_string(),
            "project_id": fixture.project_id,
            "workspace_id": fixture.workspace_id,
            "session_id": fixture.session_id,
            "event_type": reserved,
            "payload_schema": format!("{reserved}@1"),
            "producer": { "name": "agent", "version": "1", "sequence": 1 },
            "privacy_class": "internal",
            "payload": { "task_id": fixture.task_id },
        });
        for bearer in [&fixture.writer, &fixture.control] {
            problem(
                &ingest_event(
                    fixture.state(),
                    &json_request(Some(bearer), Some(&fixture.key()), &envelope.to_string()),
                ),
                OperationId::IngestEvent,
                400,
                "invalid_request",
            );
        }
    }
}

/// ADR 0013 D1/D4 (P3-C): superseding or withdrawing a stale fact leaves its
/// own verdict as it was, and it stays listed; the Task verdict comes only
/// from the active facts. Withdrawal wins over supersession.
#[test]
#[allow(clippy::too_many_lines)] // One stale fact, then each edge.
fn edges_change_no_verdict_and_withdrawal_wins() {
    let fixture = Fixture::new();
    let (e1, s1) = fixture.passed_run();
    let cp1 = fixture.edged(&fixture.edged_body(
        &s1,
        &e1,
        &[("F1 parser done", &[]), ("Fx lexer done", &[])],
        &[],
        &[],
    ));
    let f1 = format!("{cp1}/verified_completed_work/0");
    let fx = format!("{cp1}/verified_completed_work/1");
    std::fs::write(fixture.repo.join("README"), b"edited\n").unwrap();
    fixture.commit("change after the run");
    let (_, stale) = fixture.capsule(None);
    assert_eq!(stale["status"], "stale");
    let before_f1 = Fixture::claim(&stale, &f1).clone();
    let before_lexer = Fixture::claim(&stale, &fx).clone();
    assert_eq!(before_f1["verdict"], "stale");

    let (e2, s2) = fixture.passed_run();
    let cp2 = fixture.edged(&fixture.edged_body(
        &s2,
        &e2,
        &[("F2 parser done again", &[&f1])],
        &[],
        &[&fx],
    ));
    let (_, after) = fixture.capsule(None);
    let f1_after = Fixture::claim(&after, &f1);
    let lexer_after = Fixture::claim(&after, &fx);
    for (before, now, standing) in [
        (&before_f1, f1_after, "superseded"),
        (&before_lexer, lexer_after, "withdrawn"),
    ] {
        assert_eq!(now["standing"], standing);
        for member in [
            "verdict",
            "reasons",
            "evidence",
            "grounding_digest",
            "number",
        ] {
            assert_eq!(now[member], before[member], "{member}");
        }
    }
    let f2 = Fixture::claim(&after, &format!("{cp2}/verified_completed_work/0"));
    assert_eq!(f2["verdict"], "verified");
    // Only the active fact counts: the Task is verified again.
    assert_eq!(after["status"], "verified");
    let continuation = after["continuation"].as_array().unwrap();
    for (claim, template) in [(&f1, "superseded"), (&fx, "withdrawn")] {
        let item = continuation
            .iter()
            .find(|item| item["object_id"] == claim.as_str())
            .unwrap();
        assert_eq!(item["template"], template);
        assert_eq!(item["verdict"], "stale");
    }

    // Withdrawing the superseded F1 as well: withdrawn wins.
    let (e3, s3) = fixture.passed_run();
    fixture.edged(&fixture.edged_body(&s3, &e3, &[], &[], &[&f1]));
    let (_, last) = fixture.capsule(None);
    let f1_last = Fixture::claim(&last, &f1);
    assert_eq!(f1_last["standing"], "withdrawn");
    assert_eq!(f1_last["verdict"], before_f1["verdict"]);
    assert_eq!(last["status"], "verified");
}
