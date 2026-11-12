//! `ingestEvent`, `listEvents`, and `getEvent` (ADR 0010), and the
//! read-only `verifyLedger` (P2-C). `memproved`'s own Events without a
//! Session (ADR 0012 D7) are listed with `session_id` null; a client cannot
//! ingest their four types.
//!
//! Check order follows the contract: readiness, authentication,
//! authorization, `Idempotency-Key`, request validation, then the engine
//! Event ledger. Beyond the frozen schema, ingest requires a non-null
//! `session_id` (D1), normalizes `occurred_at` to the canonical timestamp
//! form, and rejects payload integers RFC 8785 would round (both `400
//! invalid_request`). The digest is always computed by the store. An Event
//! for a Session that is not active is `409 invalid_transition`.

use memprove_domain::{
    CanonicalJson, CorrelationId, Event, EventId, EventProducer, LedgerVerification,
    MAX_SAFE_INTEGER, PrivacyClass, ProjectId, SessionId, UtcTimestamp, WorkspaceId,
    is_daemon_event_type, is_reserved_producer_name,
};
use memprove_engine::events::{EventError, NewEvent};
use memprove_protocol::{
    phase1::{OperationId, ProblemCode},
    strict_json::JsonValue,
};
use serde_json::json;

use crate::{
    api::{AppState, WireResponse, guard},
    catalog::{
        command, id_member, invalid_id, mutation, optional_id_member, page_body, parse_id, path_id,
        problem, tampered, text, unbound_state,
    },
    tokens::{TokenRequest, parse_list_query_for},
};

const EVENT_PREFIX: &str = "/v1/events/";

fn event_problem(error: EventError) -> WireResponse {
    problem(match error {
        EventError::InvalidRequest(detail) => return WireResponse::invalid(detail),
        EventError::ProjectNotFound => ProblemCode::ProjectNotFound,
        EventError::ResourceNotFound => ProblemCode::ResourceNotFound,
        EventError::ProjectWorkspaceMismatch => ProblemCode::ProjectWorkspaceMismatch,
        EventError::IdempotencyConflict => ProblemCode::IdempotencyConflict,
        EventError::InvalidTransition => ProblemCode::InvalidTransition,
        EventError::Tampered => return tampered(),
        EventError::UnboundState => return unbound_state(),
        EventError::Unavailable => ProblemCode::ServiceNotReady,
    })
}

/// Writes `value` back to JSON text, keeping every number lexeme exactly as
/// received so [`CanonicalJson::parse_exact_integers`] sees what the client
/// sent.
fn write_json(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        JsonValue::Number(lexeme) => out.push_str(lexeme),
        JsonValue::String(value) => {
            out.push_str(&serde_json::to_string(value).expect("a string always serializes"));
        }
        JsonValue::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(members) => {
            out.push('{');
            for (index, (name, item)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(name).expect("a string always serializes"));
                out.push(':');
                write_json(item, out);
            }
            out.push('}');
        }
    }
}

/// A schema-valid integer lexeme as an exact `u64`, at most 2^53 − 1.
pub(crate) fn safe_integer(value: &JsonValue) -> Option<u64> {
    let JsonValue::Number(lexeme) = value else {
        return None;
    };
    if lexeme.bytes().all(|byte| byte.is_ascii_digit()) {
        return lexeme
            .parse()
            .ok()
            .filter(|value| *value <= MAX_SAFE_INTEGER);
    }
    // Schema validation already proved the value integral (for example
    // `1.0` or `1e2`); below 2^53 an IEEE-754 double holds it exactly.
    #[allow(clippy::cast_precision_loss)]
    let bound = MAX_SAFE_INTEGER as f64;
    let parsed = lexeme.parse::<f64>().ok()?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (parsed.is_finite() && parsed >= 0.0 && parsed <= bound && parsed.fract() == 0.0)
        .then_some(parsed as u64)
}

/// `400` for an envelope member that is present but unusable; `at` is its
/// location and `rule` the rule it violates.
fn bad(at: &str, rule: &str) -> WireResponse {
    WireResponse::invalid(format!("{at} {rule}"))
}

/// A required string member of a validated envelope.
fn string_member<'a>(body: &'a JsonValue, name: &str) -> Result<&'a str, WireResponse> {
    text(body, name).ok_or_else(|| bad(&format!("$.{name}"), "must be a string"))
}

/// Typed envelope plus its idempotency canonical form.
fn envelope(body: &JsonValue) -> Result<(NewEvent, serde_json::Value), WireResponse> {
    let event_id = id_member(body, "event_id", EventId::parse)?;
    let project_id = id_member(body, "project_id", ProjectId::parse)?;
    let workspace_id = id_member(body, "workspace_id", WorkspaceId::parse)?;
    // ADR 0010 D1: a new Event must name its Session.
    let session_id = match body.get("session_id") {
        None | Some(JsonValue::Null) => {
            return Err(bad("$.session_id", "is required and must not be null"));
        }
        Some(_) => id_member(body, "session_id", SessionId::parse)?,
    };
    let occurred_at = match body.get("occurred_at") {
        None | Some(JsonValue::Null) => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| bad("$.occurred_at", "must be a string"))?;
            Some(
                UtcTimestamp::normalize(text)
                    .map_err(|_| bad("$.occurred_at", "must be an RFC 3339 UTC timestamp"))?,
            )
        }
    };
    let producer = body
        .get("producer")
        .ok_or_else(|| bad("$.producer", "is required"))?;
    let producer = EventProducer {
        name: string_member(producer, "name")
            .map_err(|_| bad("$.producer.name", "must be a string"))?
            .to_owned(),
        version: string_member(producer, "version")
            .map_err(|_| bad("$.producer.version", "must be a string"))?
            .to_owned(),
        sequence: producer
            .get("sequence")
            .and_then(safe_integer)
            .ok_or_else(|| {
                bad(
                    "$.producer.sequence",
                    "must be an integer from 0 through 9007199254740991",
                )
            })?,
    };
    let mut payload_text = String::new();
    write_json(
        body.get("payload")
            .ok_or_else(|| bad("$.payload", "is required"))?,
        &mut payload_text,
    );
    let payload = CanonicalJson::parse_exact_integers(&payload_text).map_err(|_| {
        bad(
            "$.payload",
            "holds an integer outside the range of exact JSON numbers (RFC 8785)",
        )
    })?;
    let event_type = string_member(body, "event_type")?;
    // P2-E: a client can neither write an Event type memproved produces
    // nor claim to be the daemon (`continuityd`). ADR 0013 D8: nor one reserved to it
    // (`claim.superseded`, `decision.recorded`, `session.crashed`).
    if is_daemon_event_type(event_type) {
        return Err(bad(
            "$.event_type",
            "names an Event type that only memproved writes",
        ));
    }
    if is_reserved_producer_name(&producer.name) {
        return Err(bad("$.producer.name", "is a name reserved for continuityd"));
    }
    let privacy_class = PrivacyClass::parse(string_member(body, "privacy_class")?)
        .map_err(|_| bad("$.privacy_class", "is not one of the allowed values"))?;
    let new = NewEvent {
        event_id,
        project_id,
        workspace_id,
        session_id,
        event_type: event_type.to_owned(),
        payload_schema: string_member(body, "payload_schema")?.to_owned(),
        payload,
        occurred_at,
        producer,
        correlation_id: optional_id_member(body, "correlation_id", CorrelationId::parse)?,
        causation_id: optional_id_member(body, "causation_id", EventId::parse)?,
        privacy_class,
    };
    let canonical = json!({
        "operation_id": OperationId::IngestEvent.as_str(),
        "envelope_version": "1.0",
        "event_id": new.event_id,
        "project_id": new.project_id,
        "workspace_id": new.workspace_id,
        "session_id": new.session_id,
        "event_type": new.event_type,
        "payload_schema": new.payload_schema,
        "occurred_at": new.occurred_at,
        "producer": {
            "name": new.producer.name,
            "version": new.producer.version,
            "sequence": new.producer.sequence,
        },
        "correlation_id": new.correlation_id,
        "causation_id": new.causation_id,
        "privacy_class": new.privacy_class.as_str(),
        "payload_sha256": new.payload.sha256(),
    });
    Ok((new, canonical))
}

/// The stored Event as the frozen wire envelope. Absent optional members are
/// rendered as explicit `null`; `occurred_at` is the producer-declared time.
pub(crate) fn envelope_body(event: &Event) -> serde_json::Value {
    let payload: serde_json::Value =
        serde_json::from_str(event.payload().as_str()).expect("a stored canonical payload is JSON");
    json!({
        "envelope_version": event.envelope_version(),
        "event_id": event.event_id(),
        "project_id": event.project_id(),
        "workspace_id": event.workspace_id(),
        "session_id": event.session_id(),
        "event_type": event.event_type(),
        "payload_schema": event.payload_schema(),
        "occurred_at": event.occurred_at(),
        "producer": {
            "name": event.producer().name,
            "version": event.producer().version,
            "sequence": event.producer().sequence,
        },
        "correlation_id": event.correlation_id(),
        "causation_id": event.causation_id(),
        "privacy_class": event.privacy_class().as_str(),
        "payload": payload,
    })
}

/// `POST /v1/events`.
#[must_use]
pub fn ingest_event(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::IngestEvent;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let (new, canonical) = match envelope(&body) {
        Ok(typed) => typed,
        Err(invalid) => return invalid,
    };
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    match state.events.ingest(new, &command) {
        Ok(event) => WireResponse::json(
            202,
            &json!({
                "schema_version": 1,
                "request_id": uuid::Uuid::now_v7().to_string(),
                "status": "accepted",
                "resource_id": event.event_id(),
            }),
        ),
        Err(error) => event_problem(error),
    }
}

/// `GET /v1/events?project_id=…`.
#[must_use]
pub fn list_events(state: &AppState, bearer: Option<&str>, query: Option<&str>) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::ListEvents, bearer) {
        return denied;
    }
    let list = match parse_list_query_for(query, Some("project_id")) {
        Ok(list) => list,
        Err(detail) => return WireResponse::invalid(detail),
    };
    let Some(project_id) = list.subject.and_then(|id| parse_id(id, ProjectId::parse)) else {
        return invalid_id("query parameter project_id");
    };
    let cursor = match list.cursor {
        None => None,
        Some(id) => match parse_id(id, EventId::parse) {
            Some(cursor) => Some(cursor),
            None => return invalid_id("query parameter cursor"),
        },
    };
    match state
        .events
        .list_events(&project_id, cursor.as_ref(), list.limit)
    {
        Ok(page) => WireResponse::json(
            200,
            &page_body(&page, envelope_body, |event| event.event_id().to_string()),
        ),
        Err(error) => event_problem(error),
    }
}

fn ledger_body(project_id: &ProjectId, verification: &LedgerVerification) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "project_id": project_id,
        "project_sequence": verification.project_sequence,
        "valid": verification.is_valid(),
        "checked": {
            "events": verification.events,
            "evidence": verification.evidence,
            "snapshots": verification.snapshots,
            "checkpoints": verification.checkpoints,
            "tasks": verification.tasks,
        },
        "head_event_sha256": verification
            .head_event_sha256
            .as_ref()
            .map(|digest| format!("sha256:{digest}")),
        "first_failure": verification.first_failure.as_ref().map(|failure| json!({
            "record_type": failure.record_type.as_str(),
            "record_id": failure.record_id,
            "project_sequence": failure.project_sequence,
            "reason": failure.reason,
        })),
    })
}

/// `POST /v1/verification/ledger` (P2-C): recomputes the Project's Event
/// chain and every Evidence, Snapshot, and Checkpoint digest. The frozen
/// `Idempotency-Key` is required and checked, but nothing is recorded:
/// the operation writes nothing, so a repeat simply verifies again.
#[must_use]
pub fn verify_ledger(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::VerifyLedger;
    let (_, _, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let project_id = match id_member(&body, "project_id", ProjectId::parse) {
        Ok(project_id) => project_id,
        Err(invalid) => return invalid,
    };
    match state.events.verify_ledger(&project_id) {
        Ok(verification) => WireResponse::json(200, &ledger_body(&project_id, &verification)),
        Err(error) => event_problem(error),
    }
}

/// Extracts `{event_id}` from a `/v1/events/{event_id}` path.
#[must_use]
pub fn event_target(path: &str) -> &str {
    path.strip_prefix(EVENT_PREFIX).unwrap_or_default()
}

/// `GET /v1/events/{event_id}`.
#[must_use]
pub fn get_event(state: &AppState, bearer: Option<&str>, event_id: &str) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::GetEvent, bearer) {
        return denied;
    }
    let event_id = match path_id(event_id, "event_id", EventId::parse) {
        Ok(event_id) => event_id,
        Err(invalid) => return invalid,
    };
    match state.events.get_event(&event_id) {
        Ok(event) => WireResponse::json(200, &envelope_body(&event)),
        Err(error) => event_problem(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Barrier},
        thread,
    };

    use memprove_domain::{DAEMON_EVENT_TYPES, DAEMON_RESERVED_EVENT_TYPES};
    use memprove_protocol::strict_json::parse_strict;

    use super::*;
    use crate::{
        catalog::{create_session, end_session, register_project},
        test_support::{Harness, field, git_repository, json_request},
    };

    struct Fixture {
        harness: Harness,
        control: String,
        reader: String,
        _repos: tempfile::TempDir,
        _repo: PathBuf,
        project_id: String,
        workspace_id: String,
        session_id: String,
    }

    impl Fixture {
        fn new() -> Self {
            let harness = Harness::new(true);
            let control = harness.bootstrap();
            let (_, reader) = harness.issue(&control, "reader", &["mcp-read"]);
            let repos = tempfile::tempdir().unwrap();
            let repo = git_repository(repos.path(), "repo");
            let body = json!({ "schema_version": 1, "root": repo }).to_string();
            let registered = register_project(
                &harness.state,
                &json_request(Some(&control), Some(&harness.fresh_key()), &body),
            );
            assert_eq!(registered.status, 201, "{}", registered.body);
            let registered = parse_strict(&registered.body).unwrap();
            let project_id = field(&registered, "project_id");
            let workspace_id = field(&registered, "root_workspace_id");
            let body = json!({
                "schema_version": 1,
                "workspace_id": workspace_id,
                "agent_id": "018f47a0-0000-7000-8000-00000000a6e1",
            })
            .to_string();
            let session = create_session(
                &harness.state,
                &json_request(Some(&control), Some(&harness.fresh_key()), &body),
            );
            assert_eq!(session.status, 201, "{}", session.body);
            let session_id = field(&parse_strict(&session.body).unwrap(), "session_id");
            Self {
                harness,
                control,
                reader,
                _repos: repos,
                _repo: repo,
                project_id,
                workspace_id,
                session_id,
            }
        }

        fn envelope(&self) -> serde_json::Value {
            json!({
                "envelope_version": "1.0",
                "event_id": uuid::Uuid::now_v7().to_string(),
                "project_id": self.project_id,
                "workspace_id": self.workspace_id,
                "session_id": self.session_id,
                "event_type": "turn.started",
                "payload_schema": "turn.started@1",
                "producer": { "name": "hook", "version": "1.0.0", "sequence": 1 },
                "privacy_class": "internal",
                "payload": { "turn": 1 },
            })
        }

        fn ingest_with(&self, bearer: &str, key: Option<&str>, body: &str) -> WireResponse {
            ingest_event(&self.harness.state, &json_request(Some(bearer), key, body))
        }

        fn ingest(&self, body: &serde_json::Value) -> WireResponse {
            self.ingest_with(
                &self.control,
                Some(&self.harness.fresh_key()),
                &body.to_string(),
            )
        }
    }

    fn assert_problem(response: &WireResponse, operation: OperationId, status: u16, code: &str) {
        assert_eq!(response.status, status, "{}", response.body);
        assert_eq!(field(&parse_strict(&response.body).unwrap(), "code"), code);
        response.validate(Some(operation)).unwrap();
    }

    fn assert_ok(response: &WireResponse, operation: OperationId, status: u16) -> JsonValue {
        assert_eq!(response.status, status, "{}", response.body);
        response.validate(Some(operation)).unwrap();
        parse_strict(&response.body).unwrap()
    }

    #[test]
    fn ingest_accepts_reads_back_and_replays_idempotently() {
        let fixture = Fixture::new();
        let op = OperationId::IngestEvent;
        let mut body = fixture.envelope();
        body["occurred_at"] = json!("2026-10-02T08:00:00.500000Z");
        body["correlation_id"] = json!(uuid::Uuid::now_v7().to_string());
        body["payload"] =
            json!({ "b": [1, 2.5, "x"], "a": null, "big": 9_007_199_254_740_991_u64 });
        let key = fixture.harness.fresh_key();
        let accepted = assert_ok(
            &fixture.ingest_with(&fixture.control, Some(&key), &body.to_string()),
            op,
            202,
        );
        let event_id = body["event_id"].as_str().unwrap().to_owned();
        assert_eq!(field(&accepted, "resource_id"), event_id);
        assert_eq!(field(&accepted, "status"), "accepted");

        // Replay: same key, same request, same Event.
        let replay = assert_ok(
            &fixture.ingest_with(&fixture.control, Some(&key), &body.to_string()),
            op,
            202,
        );
        assert_eq!(field(&replay, "resource_id"), event_id);
        assert_ne!(field(&replay, "request_id"), field(&accepted, "request_id"));
        // Same key, different request.
        let mut changed = body.clone();
        changed["payload"] = json!({ "turn": 2 });
        assert_problem(
            &fixture.ingest_with(&fixture.control, Some(&key), &changed.to_string()),
            op,
            409,
            "idempotency_conflict",
        );
        // The same event_id under a new key.
        assert_problem(&fixture.ingest(&changed), op, 409, "idempotency_conflict");

        let read = assert_ok(
            &get_event(&fixture.harness.state, Some(&fixture.control), &event_id),
            OperationId::GetEvent,
            200,
        );
        assert_eq!(field(&read, "occurred_at"), "2026-10-02T08:00:00.500Z");
        assert_eq!(field(&read, "session_id"), fixture.session_id);
        assert_eq!(read.get("causation_id"), Some(&JsonValue::Null));
        let mut expected_payload = String::new();
        write_json(read.get("payload").unwrap(), &mut expected_payload);
        assert_eq!(
            CanonicalJson::parse(&expected_payload).unwrap().as_str(),
            r#"{"a":null,"b":[1,2.5,"x"],"big":9007199254740991}"#
        );
    }

    #[test]
    fn an_event_without_occurred_at_reads_back_null() {
        let fixture = Fixture::new();
        let body = fixture.envelope();
        assert_ok(&fixture.ingest(&body), OperationId::IngestEvent, 202);
        let read = assert_ok(
            &get_event(
                &fixture.harness.state,
                Some(&fixture.control),
                body["event_id"].as_str().unwrap(),
            ),
            OperationId::GetEvent,
            200,
        );
        assert_eq!(read.get("occurred_at"), Some(&JsonValue::Null));
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One table of every rejected envelope and reference.
    fn ingest_rejects_invalid_envelopes_unknown_references_and_scope() {
        let fixture = Fixture::new();
        let op = OperationId::IngestEvent;
        let mut invalid = Vec::new();
        for (member, value) in [
            ("session_id", serde_json::Value::Null),
            ("event_type", json!("not.a.type")),
            ("occurred_at", json!("2026-10-02T08:00:00+00:00")),
            ("occurred_at", json!("2026-10-02T08:00:00.1234567891Z")),
            ("privacy_class", json!("confidential")),
            ("event_id", json!("018f47a0-0000-4000-8000-000000000001")),
            (
                "producer",
                json!({ "name": "hook", "version": "1", "sequence": 9_007_199_254_740_992_u64 }),
            ),
            (
                "producer",
                json!({ "name": "hook", "version": "1", "sequence": -1 }),
            ),
            ("payload", json!({ "n": 9_007_199_254_740_993_u64 })),
            ("payload", json!([1])),
            // P2-E: a client cannot claim to be memproved.
            (
                "producer",
                json!({ "name": "continuityd", "version": "0.1.0", "sequence": 1 }),
            ),
            (
                "producer",
                json!({ "name": "continuityd-git", "version": "1", "sequence": 1 }),
            ),
            (
                "producer",
                json!({ "name": "continuityd2", "version": "1", "sequence": 1 }),
            ),
        ]
        .into_iter()
        .chain(
            // P2-K (T11 S-4): look-alikes, by case or by character.
            [
                "Continuityd",
                "CONTINUITYD",
                " continuityd",
                "\u{a0}continuityd",
                "c\u{43e}ntinuityd",
                "\u{200b}continuityd",
                "agent x",
            ]
            .map(|name| {
                (
                    "producer",
                    json!({ "name": name, "version": "1", "sequence": 1 }),
                )
            }),
        ) {
            let mut body = fixture.envelope();
            body[member] = value;
            invalid.push(body.to_string());
        }
        let mut no_session = fixture.envelope();
        no_session.as_object_mut().unwrap().remove("session_id");
        invalid.push(no_session.to_string());
        // P2-C, P2-E: no Event type memproved produces can be ingested,
        // with or without a Session; ADR 0013 D8: nor one reserved to it.
        for daemon_only in DAEMON_EVENT_TYPES
            .iter()
            .chain(&DAEMON_RESERVED_EVENT_TYPES)
        {
            let mut body = fixture.envelope();
            body["event_type"] = json!(daemon_only);
            body["payload_schema"] = json!(format!("{daemon_only}@1"));
            invalid.push(body.to_string());
            body["session_id"] = serde_json::Value::Null;
            invalid.push(body.to_string());
        }
        let mut extra = fixture.envelope();
        extra["sequence"] = json!(1);
        invalid.push(extra.to_string());
        invalid.push("{".to_owned());
        for body in invalid {
            assert_problem(
                &fixture.ingest_with(&fixture.control, Some(&fixture.harness.fresh_key()), &body),
                op,
                400,
                "invalid_request",
            );
        }
        // Missing Idempotency-Key.
        assert_problem(
            &fixture.ingest_with(&fixture.control, None, &fixture.envelope().to_string()),
            op,
            400,
            "invalid_request",
        );
        let mut unknown_session = fixture.envelope();
        unknown_session["session_id"] = json!("018f47a0-0000-7000-8000-000000000001");
        assert_problem(
            &fixture.ingest(&unknown_session),
            op,
            404,
            "resource_not_found",
        );
        let mut unknown_project = fixture.envelope();
        unknown_project["project_id"] = json!("018f47a0-0000-7000-8000-000000000001");
        assert_problem(
            &fixture.ingest(&unknown_project),
            op,
            404,
            "project_not_found",
        );
        let mut foreign_workspace = fixture.envelope();
        foreign_workspace["workspace_id"] = json!("018f47a0-0000-7000-8000-000000000001");
        assert_problem(
            &fixture.ingest(&foreign_workspace),
            op,
            409,
            "project_workspace_mismatch",
        );
        assert_problem(
            &fixture.ingest_with(
                &fixture.reader,
                Some(&fixture.harness.fresh_key()),
                &fixture.envelope().to_string(),
            ),
            op,
            403,
            "policy_denied",
        );
        let anonymous = ingest_event(
            &fixture.harness.state,
            &json_request(None, Some(&fixture.harness.fresh_key()), "{}"),
        );
        assert_problem(&anonymous, op, 401, "authentication_required");
        let page = assert_ok(
            &list_events(
                &fixture.harness.state,
                Some(&fixture.control),
                Some(&format!("project_id={}", fixture.project_id)),
            ),
            OperationId::ListEvents,
            200,
        );
        // Only the fixture Session's `session.started` (ADR 0012) is recorded.
        let items = page.get("items").and_then(JsonValue::as_array).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(field(&items[0], "event_type"), "session.started");
        assert_eq!(
            page.get("snapshot_sequence").and_then(JsonValue::as_i64),
            Some(1)
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Every listEvents and getEvent outcome.
    fn list_pages_in_project_order_and_get_maps_every_outcome() {
        let fixture = Fixture::new();
        let state = &fixture.harness.state;
        let mut ids = Vec::new();
        for turn in 0..3 {
            let mut body = fixture.envelope();
            body["payload"] = json!({ "turn": turn });
            assert_ok(&fixture.ingest(&body), OperationId::IngestEvent, 202);
            ids.push(body["event_id"].as_str().unwrap().to_owned());
        }
        let op = OperationId::ListEvents;
        let query = |extra: &str| format!("project_id={}&limit=2{extra}", fixture.project_id);
        let first = assert_ok(
            &list_events(state, Some(&fixture.control), Some(&query(""))),
            op,
            200,
        );
        // The fixture Session's `session.started` (ADR 0012) comes first.
        assert_eq!(
            first.get("snapshot_sequence").and_then(JsonValue::as_i64),
            Some(4)
        );
        let cursor = field(&first, "next_cursor");
        let second = assert_ok(
            &list_events(
                state,
                Some(&fixture.control),
                Some(&query(&format!("&cursor={cursor}"))),
            ),
            op,
            200,
        );
        assert_eq!(second.get("next_cursor"), Some(&JsonValue::Null));
        let seen = [&first, &second]
            .iter()
            .flat_map(|page| page.get("items").and_then(JsonValue::as_array).unwrap())
            .map(|item| (field(item, "event_type"), field(item, "event_id")))
            .collect::<Vec<_>>();
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[0].0, "session.started");
        assert_eq!(
            seen[1..]
                .iter()
                .map(|(_, id)| id.clone())
                .collect::<Vec<_>>(),
            ids
        );

        for bad in [
            None,
            Some("limit=2".to_owned()),
            Some(query("&cursor=018f47a0-0000-7000-8000-000000000001")),
            Some("project_id=nope".to_owned()),
        ] {
            assert_problem(
                &list_events(state, Some(&fixture.control), bad.as_deref()),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &list_events(
                state,
                Some(&fixture.control),
                Some("project_id=018f47a0-0000-7000-8000-000000000001"),
            ),
            op,
            404,
            "project_not_found",
        );
        assert_problem(
            &list_events(state, Some(&fixture.reader), Some(&query(""))),
            op,
            403,
            "policy_denied",
        );

        let op = OperationId::GetEvent;
        assert_problem(
            &get_event(
                state,
                Some(&fixture.control),
                "018f47a0-0000-7000-8000-000000000001",
            ),
            op,
            404,
            "resource_not_found",
        );
        for bad in ["stream", "018f47a0-0000-7000-c000-000000000001"] {
            assert_problem(
                &get_event(state, Some(&fixture.control), bad),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &get_event(state, Some(&fixture.reader), &ids[0]),
            op,
            403,
            "policy_denied",
        );
        assert_problem(
            &get_event(state, None, &ids[0]),
            op,
            401,
            "authentication_required",
        );
    }

    #[test]
    fn get_status_reports_mutations_inside_a_writer_lane() {
        let fixture = Fixture::new();
        let depth = |fixture: &Fixture| {
            parse_strict(
                &crate::api::get_status(&fixture.harness.state.plane, Some(&fixture.control)).body,
            )
            .unwrap()
            .get("writer_queue_depth")
            .and_then(JsonValue::as_i64)
        };
        assert_eq!(depth(&fixture), Some(0));
        let inside = fixture.harness.writers.run(
            crate::writer::Lane::Project(fixture.project_id.clone()),
            || depth(&fixture),
        );
        assert_eq!(inside, Some(1));
        assert_eq!(depth(&fixture), Some(0));
    }

    #[test]
    fn concurrent_ingests_through_the_handler_form_one_chain() {
        const WRITERS: usize = 12;
        let fixture = Arc::new(Fixture::new());
        let barrier = Arc::new(Barrier::new(WRITERS));
        let bodies = (0..WRITERS).map(|_| fixture.envelope()).collect::<Vec<_>>();
        let handles = bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                let fixture = Arc::clone(&fixture);
                let barrier = Arc::clone(&barrier);
                let body = body.to_string();
                thread::spawn(move || {
                    barrier.wait();
                    fixture.ingest_with(
                        &fixture.control,
                        Some(&format!("concurrent-handler-{index:04}")),
                        &body,
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            assert_eq!(handle.join().unwrap().status, 202);
        }
        // One key submitted twice at once records one Event.
        let shared = fixture.envelope().to_string();
        let twice = (0..2)
            .map(|_| {
                let fixture = Arc::clone(&fixture);
                let shared = shared.clone();
                thread::spawn(move || {
                    fixture.ingest_with(&fixture.control, Some("shared-handler-key-01"), &shared)
                })
            })
            .collect::<Vec<_>>();
        for handle in twice {
            assert_eq!(handle.join().unwrap().status, 202);
        }
        let page = parse_strict(
            &list_events(
                &fixture.harness.state,
                Some(&fixture.control),
                Some(&format!("project_id={}&limit=500", fixture.project_id)),
            )
            .body,
        )
        .unwrap();
        // Plus the fixture Session's `session.started` (ADR 0012).
        assert_eq!(
            page.get("snapshot_sequence").and_then(JsonValue::as_i64),
            Some(i64::try_from(WRITERS).unwrap() + 2)
        );
        assert_eq!(
            page.get("items")
                .and_then(JsonValue::as_array)
                .unwrap()
                .len(),
            WRITERS + 2
        );
    }

    #[test]
    fn a_session_that_is_not_active_accepts_no_event() {
        let fixture = Fixture::new();
        let op = OperationId::IngestEvent;
        let before = fixture.envelope();
        let key = fixture.harness.fresh_key();
        let accepted = fixture.ingest_with(&fixture.control, Some(&key), &before.to_string());
        let accepted = assert_ok(&accepted, op, 202);

        let ended = end_session(
            &fixture.harness.state,
            &json_request(
                Some(&fixture.control),
                Some(&fixture.harness.fresh_key()),
                r#"{"schema_version":1,"expected_revision":1}"#,
            ),
            &fixture.session_id,
        );
        assert_eq!(ended.status, 200, "{}", ended.body);

        assert_problem(
            &fixture.ingest(&fixture.envelope()),
            op,
            409,
            "invalid_transition",
        );
        // A replay of an Event recorded while the Session was active is not a
        // new write: it still returns the recorded result.
        let replay = fixture.ingest_with(&fixture.control, Some(&key), &before.to_string());
        assert_eq!(
            field(&assert_ok(&replay, op, 202), "resource_id"),
            field(&accepted, "resource_id")
        );
        let page = list_events(
            &fixture.harness.state,
            Some(&fixture.control),
            Some(&format!("project_id={}", fixture.project_id)),
        );
        let page = assert_ok(&page, OperationId::ListEvents, 200);
        // `session.started`, the accepted Event, and `session.ended`, which
        // was appended before the Session closed (ADR 0012).
        let types = page
            .get("items")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .map(|item| field(item, "event_type"))
            .collect::<Vec<_>>();
        assert_eq!(types, ["session.started", "turn.started", "session.ended"]);
    }
}
