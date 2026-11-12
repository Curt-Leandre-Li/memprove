//! `registerProject`, `getProject`, `resolveWorkspace`, `createSession`,
//! `listSessions`, `endSession`, `createTask`, `listTasks`, and
//! `transitionTask`.
//!
//! Check order follows the contract: readiness, authentication, authorization,
//! then request validation, then the engine catalog service. Paths in request
//! bodies must be absolute so they never depend on the daemon's working
//! directory.

use std::path::Path;

use memprove_domain::{
    AgentId, EvidenceId, IdempotencyContext, IdempotencyKey, OperationId as DomainOperationId,
    PrincipalId, ProjectId, ResumedFrom, Session, SessionAgent, SessionEndedBy, SessionId,
    SessionOutcome, Task, TaskId, TaskStatus, Workspace, WorkspaceId,
};
use memprove_engine::{
    catalog::{CatalogError, Command, NewTask, Page, RegisteredProject, TaskStep},
    runtime::Principal,
};
use memprove_protocol::{
    phase1::{OperationId, ProblemCode, validate_request},
    strict_json::JsonValue,
};
use serde_json::json;

use crate::{
    api::{AppState, WireResponse, guard},
    events::safe_integer,
    recovery::refuse_token_shape,
    tokens::{
        TokenRequest, body_json, idempotency_key, is_uuid_v7, parse_list_query_for, request_digest,
    },
};

const PROJECT_PREFIX: &str = "/v1/projects/";
const SESSION_PREFIX: &str = "/v1/sessions/";
const SESSION_END_SUFFIX: &str = "/end";
const TASK_PREFIX: &str = "/v1/tasks/";
const TASK_TRANSITION_SUFFIX: &str = "/transition";

pub(crate) fn problem(code: ProblemCode) -> WireResponse {
    WireResponse::problem(code)
}

/// P3-L3 (Owner, T16 S-2): the detail of `409 evidence_stale` when the Task
/// Capsule's integrity check finds the Task `tampered` (P3-L L3).
pub(crate) const TAMPERED_COMPLETION: &str = "the integrity check of the Task failed: a \
     record of the Task is missing, does not recompute, or disagrees with the ledger, so no \
     Evidence can complete it; a rerun does not help; run `memprove ledger verify` to see \
     which record";

/// P4-H1 (Owner, T20 SF-1/SF-2): the detail of `409 capsule_tampered` when
/// a stored record a read needs is missing or does not decode or
/// recompute. `503 service_not_ready` is left for what a retry can cure.
pub(crate) const TAMPERED_RECORD: &str = "a stored record this request reads is missing, does \
     not decode, or does not recompute; a retry does not help; run `memprove ledger verify` \
     to see which record";

/// `409 capsule_tampered` with [`TAMPERED_RECORD`] (P4-H1).
pub(crate) fn tampered() -> WireResponse {
    WireResponse::with_detail(ProblemCode::CapsuleTampered, TAMPERED_RECORD)
}

/// P4-H1 (review of the first P4-H1 commit): the detail of
/// `409 capsule_tampered` when the Project's state database is not bound to
/// it, so none of it is read as the Project's. U1 B5: also when the
/// Project's registry record does not decode.
pub(crate) const UNBOUND_STATE: &str = "the Project's state database is not bound to it: its \
     state_metadata or schema_migrations row is missing, does not decode, or differs from what \
     this memproved expects, or the Project's registry record does not decode; nothing of it \
     is read as the Project's ledger and a retry does not help";

/// `409 capsule_tampered` with [`UNBOUND_STATE`] (P4-H1).
pub(crate) fn unbound_state() -> WireResponse {
    WireResponse::with_detail(ProblemCode::CapsuleTampered, UNBOUND_STATE)
}

fn catalog_problem(error: CatalogError) -> WireResponse {
    problem(match error {
        CatalogError::TaskTampered => {
            return WireResponse::with_detail(ProblemCode::EvidenceStale, TAMPERED_COMPLETION);
        }
        CatalogError::Tampered => return tampered(),
        CatalogError::UnboundState => return unbound_state(),
        CatalogError::InvalidRequest(detail) => return WireResponse::invalid(detail),
        CatalogError::ProjectNotFound => ProblemCode::ProjectNotFound,
        CatalogError::WorkspaceNotResolved => ProblemCode::WorkspaceNotResolved,
        CatalogError::ProjectWorkspaceMismatch => ProblemCode::ProjectWorkspaceMismatch,
        CatalogError::IdempotencyConflict => ProblemCode::IdempotencyConflict,
        CatalogError::ResourceNotFound => ProblemCode::ResourceNotFound,
        CatalogError::RevisionConflict => ProblemCode::RevisionConflict,
        CatalogError::InvalidTransition => ProblemCode::InvalidTransition,
        CatalogError::EvidenceStale => ProblemCode::EvidenceStale,
        CatalogError::Unavailable => ProblemCode::ServiceNotReady,
        CatalogError::RoleForbidden(rule) => return WireResponse::role_forbidden(rule),
    })
}

/// Strictly decodes a JSON body and validates it against the operation's
/// request schema.
fn decode(operation: OperationId, request: &TokenRequest<'_>) -> Result<JsonValue, WireResponse> {
    let body = body_json(request)?;
    validate_request(operation, Some(&body))
        .map_err(|error| WireResponse::invalid(error.detail()))?;
    Ok(body)
}

/// `400` for a member that is not a lowercase `UUIDv7`; `at` is its location.
pub(crate) fn invalid_id(at: &str) -> WireResponse {
    WireResponse::invalid(format!("{at} must be a lowercase UUIDv7"))
}

/// A required `UUIDv7` member of a validated body.
pub(crate) fn id_member<T, E>(
    body: &JsonValue,
    name: &str,
    parse: impl Fn(&str) -> Result<T, E>,
) -> Result<T, WireResponse> {
    text(body, name)
        .and_then(|id| parse_id(id, parse))
        .ok_or_else(|| invalid_id(&format!("$.{name}")))
}

/// An optional nullable `UUIDv7` member: absent and `null` are the same.
pub(crate) fn optional_id_member<T, E>(
    body: &JsonValue,
    name: &str,
    parse: impl Fn(&str) -> Result<T, E>,
) -> Result<Option<T>, WireResponse> {
    match body.get(name) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .and_then(|id| parse_id(id, parse))
            .map(Some)
            .ok_or_else(|| invalid_id(&format!("$.{name}"))),
    }
}

/// A `UUIDv7` path parameter.
pub(crate) fn path_id<T, E>(
    value: &str,
    name: &str,
    parse: impl Fn(&str) -> Result<T, E>,
) -> Result<T, WireResponse> {
    parse_id(value, parse).ok_or_else(|| invalid_id(&format!("path parameter {name}")))
}

/// An absolute path member without NUL.
fn absolute_member<'a>(body: &'a JsonValue, name: &str) -> Result<&'a Path, WireResponse> {
    text(body, name).and_then(absolute).ok_or_else(|| {
        WireResponse::invalid(format!("$.{name} must be an absolute path without NUL"))
    })
}

pub(crate) fn text<'a>(body: &'a JsonValue, name: &str) -> Option<&'a str> {
    body.get(name).and_then(JsonValue::as_str)
}

fn absolute(value: &str) -> Option<&Path> {
    let path = Path::new(value);
    (path.is_absolute() && !value.contains('\0')).then_some(path)
}

/// Builds the idempotency namespace for a validated mutation.
pub(crate) fn command(
    principal: &Principal,
    operation: OperationId,
    key: &str,
    canonical: &serde_json::Value,
) -> Result<Command, WireResponse> {
    let invalid = |detail: &'static str| move |_| WireResponse::invalid(detail);
    Ok(Command {
        context: IdempotencyContext {
            principal_id: PrincipalId::parse(principal.id())
                .map_err(invalid("the bearer token's identity is not usable here"))?,
            operation_id: DomainOperationId::parse(operation.as_str())
                .map_err(invalid("the operation identity is not usable here"))?,
            key: IdempotencyKey::parse(key).map_err(invalid(
                "the Idempotency-Key header must be text without control characters",
            ))?,
        },
        request_digest: request_digest(canonical),
    })
}

/// Common mutation preamble: guard, `Idempotency-Key`, and body decoding.
pub(crate) fn mutation(
    state: &AppState,
    operation: OperationId,
    request: &TokenRequest<'_>,
) -> Result<(Principal, String, JsonValue), WireResponse> {
    let principal = guard(&state.plane, operation, request.bearer)?;
    let key = idempotency_key(request.idempotency_key)
        .map_err(WireResponse::invalid)?
        .to_owned();
    let body = decode(operation, request)?;
    Ok((principal, key, body))
}

fn project_body(registered: &RegisteredProject) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "project_id": registered.project.project_id,
        "root_workspace_id": registered.root_workspace.workspace_id,
        "kind": "git",
        "revision": registered.project.revision,
    })
}

fn workspace_body(workspace: &Workspace) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "workspace_id": workspace.workspace_id,
        "project_id": workspace.project_id,
        "revision": workspace.revision,
    })
}

/// A wire `Session`. U8 A1 (Owner O4): `agent` only for a Session that
/// declared one, so the body of every other Session is as before; U9 A7
/// (Owner O3): `ended_by` likewise.
fn session_body(session: &Session) -> serde_json::Value {
    let mut body = json!({
        "schema_version": 1,
        "session_id": session.session_id,
        "workspace_id": session.workspace_id,
        "agent_id": session.agent_id,
        "state": if session.ended_at.is_some() { "ended" } else { "active" },
        "revision": session.revision,
    });
    if let Some(agent) = &session.agent {
        body["agent"] = agent.wire();
    }
    // U9 A7 (Owner O3): who ended it as abandoned, from its row.
    if let Some(ended_by) = &session.ended_by {
        body["ended_by"] = ended_by.wire();
    }
    body
}

fn task_body(task: &Task) -> serde_json::Value {
    json!({
        "schema_version": 1,
        "task_id": task.task_id,
        "project_id": task.project_id,
        "workspace_id": task.workspace_id,
        "title": task.title,
        "state": task.status.as_str(),
        "revision": task.revision,
        "dependency_ids": [],
        "completion_evidence_ids": task.completion_evidence_ids,
    })
}

pub(crate) fn page_body<T>(
    page: &Page<T>,
    item: impl Fn(&T) -> serde_json::Value,
    id: impl Fn(&T) -> String,
) -> serde_json::Value {
    let next_cursor = page.has_more.then(|| page.items.last().map(&id)).flatten();
    json!({
        "schema_version": 1,
        "snapshot_sequence": page.snapshot_sequence,
        "items": page.items.iter().map(item).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    })
}

/// `POST /v1/projects/register`.
#[must_use]
pub fn register_project(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::RegisterProject;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let root = match absolute_member(&body, "root") {
        Ok(root) => root,
        Err(invalid) => return invalid,
    };
    let canonical = json!({ "operation_id": operation.as_str(), "root": root });
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    match state.catalog.register_project(root, &command) {
        Ok(registered) => WireResponse::json(201, &project_body(&registered)),
        Err(error) => catalog_problem(error),
    }
}

/// Extracts `{project_id}` from a `/v1/projects/{project_id}` path.
#[must_use]
pub fn project_target(path: &str) -> &str {
    path.strip_prefix(PROJECT_PREFIX).unwrap_or_default()
}

/// `GET /v1/projects/{project_id}`.
#[must_use]
pub fn get_project(state: &AppState, bearer: Option<&str>, project_id: &str) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::GetProject, bearer) {
        return denied;
    }
    let project_id = match path_id(project_id, "project_id", ProjectId::parse) {
        Ok(project_id) => project_id,
        Err(invalid) => return invalid,
    };
    match state.catalog.get_project(&project_id) {
        Ok(registered) => WireResponse::json(200, &project_body(&registered)),
        Err(error) => catalog_problem(error),
    }
}

/// `POST /v1/workspaces/resolve`. It has no `Idempotency-Key`.
#[must_use]
pub fn resolve_workspace(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::ResolveWorkspace;
    if let Err(denied) = guard(&state.plane, operation, request.bearer) {
        return denied;
    }
    let body = match decode(operation, request) {
        Ok(body) => body,
        Err(invalid) => return invalid,
    };
    let project_id = match id_member(&body, "project_id", ProjectId::parse) {
        Ok(project_id) => project_id,
        Err(invalid) => return invalid,
    };
    let path = match absolute_member(&body, "path") {
        Ok(path) => path,
        Err(invalid) => return invalid,
    };
    match state.catalog.resolve_workspace(&project_id, path) {
        Ok(workspace) => WireResponse::json(200, &workspace_body(&workspace)),
        Err(error) => catalog_problem(error),
    }
}

/// `POST /v1/sessions`. The optional `task_id` links the new Session to a
/// Task of the same Project (ADR 0012); the Session's `session.started` Event
/// is written with it. U8 A1 (Owner O4): the optional `agent` is recorded
/// in `session.started@3` and the Session's row.
#[must_use]
pub fn create_session(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::CreateSession;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let workspace_id = match id_member(&body, "workspace_id", WorkspaceId::parse) {
        Ok(workspace_id) => workspace_id,
        Err(invalid) => return invalid,
    };
    let agent_id = match id_member(&body, "agent_id", AgentId::parse) {
        Ok(agent_id) => agent_id,
        Err(invalid) => return invalid,
    };
    let task_id = match optional_id_member(&body, "task_id", TaskId::parse) {
        Ok(task_id) => task_id,
        Err(invalid) => return invalid,
    };
    // ADR 0014 E6 (P4-F): the Pack the Session declares it continues from.
    let resumed_from = match body.get("resumed_from") {
        None | Some(JsonValue::Null) => None,
        Some(value) => match ResumedFrom::from_request(&serde_value(value)) {
            Ok(resumed_from) => Some(resumed_from),
            Err(refusal) => return WireResponse::invalid(refusal.0),
        },
    };
    // U8 A1 (Owner O4): the agent the Session declares itself to be.
    let agent = match body.get("agent") {
        None | Some(JsonValue::Null) => None,
        Some(value) => match SessionAgent::from_request(&serde_value(value)) {
            Ok(agent) => Some(agent),
            Err(refusal) => return WireResponse::invalid(refusal.0),
        },
    };
    let mut canonical = json!({
        "operation_id": operation.as_str(),
        "workspace_id": workspace_id,
        "agent_id": agent_id,
        "task_id": task_id,
    });
    // Present only when given, so the digest of every earlier request stays
    // the same.
    if let Some(resumed_from) = &resumed_from {
        canonical["resumed_from"] = resumed_from.wire(false);
    }
    if let Some(agent) = &agent {
        canonical["agent"] = agent.recorded();
    }
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    match state.catalog.create_session(
        &workspace_id,
        &agent_id,
        agent,
        task_id.as_ref(),
        resumed_from,
        &command,
    ) {
        Ok(session) => WireResponse::json(201, &session_body(&session)),
        Err(error) => catalog_problem(error),
    }
}

/// A validated strict JSON value as a `serde_json` value; a number keeps
/// its lexical form when it is an integer `serde_json` reads exactly.
fn serde_value(value: &JsonValue) -> serde_json::Value {
    match value {
        JsonValue::Null => serde_json::Value::Null,
        JsonValue::Bool(flag) => serde_json::Value::Bool(*flag),
        JsonValue::Number(number) => {
            serde_json::from_str(number).unwrap_or(serde_json::Value::Null)
        }
        JsonValue::String(text) => serde_json::Value::String(text.clone()),
        JsonValue::Array(items) => items.iter().map(serde_value).collect(),
        JsonValue::Object(members) => members
            .iter()
            .map(|(name, member)| (name.clone(), serde_value(member)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
    }
}

/// `GET /v1/sessions?workspace_id=…`.
#[must_use]
pub fn list_sessions(state: &AppState, bearer: Option<&str>, query: Option<&str>) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::ListSessions, bearer) {
        return denied;
    }
    let list = match parse_list_query_for(query, Some("workspace_id")) {
        Ok(list) => list,
        Err(detail) => return WireResponse::invalid(detail),
    };
    let Some(workspace_id) = list.subject.and_then(|id| parse_id(id, WorkspaceId::parse)) else {
        return invalid_id("query parameter workspace_id");
    };
    let cursor = match list.cursor {
        None => None,
        Some(id) => match parse_id(id, SessionId::parse) {
            Some(cursor) => Some(cursor),
            None => return invalid_id("query parameter cursor"),
        },
    };
    match state
        .catalog
        .list_sessions(&workspace_id, cursor.as_ref(), list.limit)
    {
        Ok(page) => WireResponse::json(
            200,
            &page_body(&page, session_body, |session| {
                session.session_id.to_string()
            }),
        ),
        Err(error) => catalog_problem(error),
    }
}

/// `expected_revision` as a `u64`. A schema-valid revision above 2^53 − 1
/// names no stored revision, so it is carried as `u64::MAX`, which never
/// matches and yields `revision_conflict`.
fn expected_revision(body: &JsonValue) -> u64 {
    body.get("expected_revision")
        .and_then(safe_integer)
        .unwrap_or(u64::MAX)
}

/// Extracts `{session_id}` from a `/v1/sessions/{session_id}/end` path.
#[must_use]
pub fn session_end_target(path: &str) -> &str {
    path.strip_prefix(SESSION_PREFIX)
        .and_then(|rest| rest.strip_suffix(SESSION_END_SUFFIX))
        .unwrap_or_default()
}

/// `POST /v1/sessions/{session_id}/end`. The optional `reason` is digested
/// for idempotency and recorded in the `session.ended` Event's payload
/// (ADR 0012); the Session record itself has no reason. The optional
/// `outcome` (ADR 0013 D6, P3-D) is `completed` (the default) or
/// `abandoned`, which records `session.crashed` instead; it enters the
/// request digest only when `abandoned`, so every earlier request and an
/// explicit `completed` keep their digest. U9 A7 (Owner O3): the optional
/// `ended_by`, only with `abandoned`, names who ends the Session; it is
/// recorded in `session.crashed@2` and the Session's row, and enters the
/// digest only when present.
#[must_use]
pub fn end_session(state: &AppState, request: &TokenRequest<'_>, session_id: &str) -> WireResponse {
    let operation = OperationId::EndSession;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let session_id = match path_id(session_id, "session_id", SessionId::parse) {
        Ok(session_id) => session_id,
        Err(invalid) => return invalid,
    };
    let expected_revision = expected_revision(&body);
    // U3 (K5, Owner O4): C4's refusal, before anything is written.
    if let Some(reason) = text(&body, "reason")
        && let Err(refused) = refuse_token_shape(&state.registry, reason, "$.reason")
    {
        return refused;
    }
    let outcome = match body.get("outcome") {
        None => SessionOutcome::Completed,
        Some(value) => match value.as_str().and_then(SessionOutcome::parse) {
            Some(outcome) => outcome,
            None => return WireResponse::invalid("$.outcome must be completed or abandoned"),
        },
    };
    // U9 A7 (Owner O3): who ends it, as the ender reports itself; only
    // with `abandoned`, and never the Session being ended.
    let ended_by = match body.get("ended_by") {
        None | Some(JsonValue::Null) => None,
        Some(_) if outcome != SessionOutcome::Abandoned => {
            return WireResponse::invalid("$.ended_by is accepted only with outcome abandoned");
        }
        Some(value) => match SessionEndedBy::from_request(&serde_value(value)) {
            Ok(ended_by) if ended_by.session_id.as_ref() == Some(&session_id) => {
                return WireResponse::invalid(
                    "$.ended_by.session_id must not be the Session being ended",
                );
            }
            Ok(ended_by) => Some(ended_by),
            Err(refusal) => return WireResponse::invalid(refusal.0),
        },
    };
    let mut canonical = json!({
        "operation_id": operation.as_str(),
        "session_id": session_id,
        "expected_revision": expected_revision,
        "reason": text(&body, "reason"),
    });
    if outcome == SessionOutcome::Abandoned {
        canonical["outcome"] = json!(outcome.as_str());
    }
    if let Some(ended_by) = &ended_by {
        canonical["ended_by"] = ended_by.recorded();
    }
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    match state.catalog.end_session_by(
        &session_id,
        expected_revision,
        text(&body, "reason"),
        (outcome, ended_by),
        &command,
    ) {
        Ok(session) => WireResponse::json(200, &session_body(&session)),
        Err(error) => catalog_problem(error),
    }
}

/// Extracts `{task_id}` from a `/v1/tasks/{task_id}/transition` path.
#[must_use]
pub fn task_transition_target(path: &str) -> &str {
    path.strip_prefix(TASK_PREFIX)
        .and_then(|rest| rest.strip_suffix(TASK_TRANSITION_SUFFIX))
        .unwrap_or_default()
}

/// `POST /v1/tasks/{task_id}/transition`. `completed` (P2-C) needs a
/// non-empty `completion_evidence_ids`, each Evidence of the Task, `PASS`,
/// and `verified` now against the Workspace the Recovery Preview would
/// probe, or the optional `workspace_id`; otherwise `409 evidence_stale`.
/// Other targets take neither (`400 invalid_request`), and any
/// `force_reason` is still `400 invalid_request` in Phase 2. U9 SF1-A (the
/// Owner's O8 exception to R1): an optional `session_id`, the asking
/// Session, with `completed` only; a `sub_agent`'s is `403 role_forbidden`.
#[must_use]
pub fn transition_task(
    state: &AppState,
    request: &TokenRequest<'_>,
    task_id: &str,
) -> WireResponse {
    let operation = OperationId::TransitionTask;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let task_id = match path_id(task_id, "task_id", TaskId::parse) {
        Ok(task_id) => task_id,
        Err(invalid) => return invalid,
    };
    let Some(target) = text(&body, "target_state").and_then(TaskStatus::parse) else {
        return WireResponse::invalid(
            "$.target_state must be one of proposed, ready, in_progress, blocked, completed, cancelled",
        );
    };
    let evidence = match body.get("completion_evidence_ids") {
        None => Vec::new(),
        Some(ids) => {
            let parsed = ids.as_array().and_then(|ids| {
                ids.iter()
                    .map(|id| id.as_str().and_then(|id| parse_id(id, EvidenceId::parse)))
                    .collect::<Option<Vec<_>>>()
            });
            match parsed {
                Some(evidence) => evidence,
                None => return invalid_id("every $.completion_evidence_ids item"),
            }
        }
    };
    let workspace_id = match body.get("workspace_id") {
        None => None,
        Some(value) => match value
            .as_str()
            .and_then(|id| parse_id(id, WorkspaceId::parse))
        {
            Some(workspace_id) => Some(workspace_id),
            None => return invalid_id("$.workspace_id"),
        },
    };
    // U9 SF1-A (the Owner's O8 exception to R1): the Session that asks,
    // by whose declared role a completion is judged.
    let session_id = match optional_id_member(&body, "session_id", SessionId::parse) {
        Ok(session_id) => session_id,
        Err(invalid) => return invalid,
    };
    if body.get("force_reason").is_some() {
        return WireResponse::invalid("$.force_reason is not accepted in Phase 2");
    }
    let expected_revision = expected_revision(&body);
    let mut canonical = json!({
        "operation_id": operation.as_str(),
        "task_id": task_id,
        "expected_revision": expected_revision,
        "target_state": target.as_str(),
    });
    // Present only when given, so the digest of every earlier request stays
    // the same and replays across the upgrade still match.
    if !evidence.is_empty() {
        canonical["completion_evidence_ids"] = json!(evidence);
    }
    if let Some(workspace_id) = &workspace_id {
        canonical["workspace_id"] = json!(workspace_id);
    }
    if let Some(session_id) = &session_id {
        canonical["session_id"] = json!(session_id);
    }
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    let step = TaskStep {
        target,
        completion_evidence_ids: evidence,
        session_id,
    };
    match state.catalog.transition_task(
        &task_id,
        expected_revision,
        step,
        workspace_id.as_ref(),
        &command,
    ) {
        Ok(task) => WireResponse::json(200, &task_body(&task)),
        Err(error) => catalog_problem(error),
    }
}

/// `POST /v1/tasks`.
#[must_use]
pub fn create_task(state: &AppState, request: &TokenRequest<'_>) -> WireResponse {
    let operation = OperationId::CreateTask;
    let (principal, key, body) = match mutation(state, operation, request) {
        Ok(decoded) => decoded,
        Err(denied) => return denied,
    };
    let project_id = match id_member(&body, "project_id", ProjectId::parse) {
        Ok(project_id) => project_id,
        Err(invalid) => return invalid,
    };
    let workspace_id = match optional_id_member(&body, "workspace_id", WorkspaceId::parse) {
        Ok(workspace_id) => workspace_id,
        Err(invalid) => return invalid,
    };
    let Some(title) = text(&body, "title").map(str::to_owned) else {
        return WireResponse::invalid("$.title must be a string");
    };
    let description = text(&body, "description").map(str::to_owned);
    // U3 (K5, Owner O4): C4's refusal, before anything is written.
    if let Err(refused) = refuse_token_shape(&state.registry, &title, "$.title") {
        return refused;
    }
    if let Some(description) = &description
        && let Err(refused) = refuse_token_shape(&state.registry, description, "$.description")
    {
        return refused;
    }
    let canonical = json!({
        "operation_id": operation.as_str(),
        "project_id": project_id,
        "workspace_id": workspace_id,
        "title": title,
        "description": description,
    });
    let command = match command(&principal, operation, &key, &canonical) {
        Ok(command) => command,
        Err(invalid) => return invalid,
    };
    let new = NewTask {
        project_id,
        workspace_id,
        title,
        description,
    };
    match state.catalog.create_task(new, &command) {
        Ok(task) => WireResponse::json(201, &task_body(&task)),
        Err(error) => catalog_problem(error),
    }
}

/// `GET /v1/tasks?project_id=…`.
#[must_use]
pub fn list_tasks(state: &AppState, bearer: Option<&str>, query: Option<&str>) -> WireResponse {
    if let Err(denied) = guard(&state.plane, OperationId::ListTasks, bearer) {
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
        Some(id) => match parse_id(id, TaskId::parse) {
            Some(cursor) => Some(cursor),
            None => return invalid_id("query parameter cursor"),
        },
    };
    match state
        .catalog
        .list_tasks(&project_id, cursor.as_ref(), list.limit)
    {
        Ok(page) => WireResponse::json(
            200,
            &page_body(&page, task_body, |task| task.task_id.to_string()),
        ),
        Err(error) => catalog_problem(error),
    }
}

/// Parses a contract `UUIDv7` into a typed domain identity.
pub(crate) fn parse_id<T, E>(value: &str, parse: impl Fn(&str) -> Result<T, E>) -> Option<T> {
    if is_uuid_v7(value) {
        parse(value).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, Instant},
    };

    use memprove_engine::catalog::{GitResolver, NotResolvable, ResolvedWorktree};
    use memprove_protocol::strict_json::parse_strict;

    use super::*;
    use crate::{
        api::{get_status, router},
        test_support::{Harness, field, git_repository, git_worktree, json_request},
    };

    struct Fixture {
        harness: Harness,
        control: String,
        /// `mcp-read` only (ADR 0012 agent reads).
        reader: String,
        /// `mcp-write` only (ADR 0012 agent writes).
        writer: String,
        repos: tempfile::TempDir,
        repo: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let harness = Harness::new(true);
            let control = harness.bootstrap();
            let (_, reader) = harness.issue(&control, "reader", &["mcp-read"]);
            let (_, writer) = harness.issue(&control, "writer", &["mcp-write"]);
            let repos = tempfile::tempdir().unwrap();
            let repo = git_repository(repos.path(), "repo");
            Self {
                harness,
                control,
                reader,
                writer,
                repos,
                repo,
            }
        }

        fn state(&self) -> &AppState {
            &self.harness.state
        }

        fn key(&self) -> String {
            self.harness.fresh_key()
        }

        fn register_with(&self, bearer: &str, key: Option<&str>, root: &Path) -> WireResponse {
            let body = json!({ "schema_version": 1, "root": root }).to_string();
            register_project(self.state(), &json_request(Some(bearer), key, &body))
        }

        /// Registers the fixture repository and returns `(project_id,
        /// root_workspace_id)`.
        fn register(&self) -> (String, String) {
            let response = self.register_with(&self.control, Some(&self.key()), &self.repo);
            assert_eq!(response.status, 201, "{}", response.body);
            let body = parse_strict(&response.body).unwrap();
            (
                field(&body, "project_id"),
                field(&body, "root_workspace_id"),
            )
        }

        fn resolve(&self, bearer: &str, project_id: &str, path: &str) -> WireResponse {
            let body =
                json!({ "schema_version": 1, "project_id": project_id, "path": path }).to_string();
            resolve_workspace(self.state(), &json_request(Some(bearer), None, &body))
        }

        fn session_with(&self, bearer: &str, key: &str, body: &serde_json::Value) -> WireResponse {
            create_session(
                self.state(),
                &json_request(Some(bearer), Some(key), &body.to_string()),
            )
        }

        fn task_with(&self, bearer: &str, key: &str, body: &serde_json::Value) -> WireResponse {
            create_task(
                self.state(),
                &json_request(Some(bearer), Some(key), &body.to_string()),
            )
        }

        /// Creates a Session in `workspace_id` and returns its id.
        fn session(&self, workspace_id: &str) -> String {
            let body = json!({
                "schema_version": 1,
                "workspace_id": workspace_id,
                "agent_id": "018f47a0-0000-7000-8000-00000000a6e1",
            });
            let response = self.session_with(&self.control, &self.key(), &body);
            assert_eq!(response.status, 201, "{}", response.body);
            field(&parse_strict(&response.body).unwrap(), "session_id")
        }

        /// Creates a Task in `project_id` and returns its id.
        fn task(&self, project_id: &str) -> String {
            let body = json!({ "schema_version": 1, "project_id": project_id, "title": "t" });
            let response = self.task_with(&self.control, &self.key(), &body);
            assert_eq!(response.status, 201, "{}", response.body);
            field(&parse_strict(&response.body).unwrap(), "task_id")
        }

        fn end(
            &self,
            bearer: Option<&str>,
            key: Option<&str>,
            id: &str,
            body: &str,
        ) -> WireResponse {
            end_session(self.state(), &json_request(bearer, key, body), id)
        }

        fn transition(
            &self,
            bearer: Option<&str>,
            key: Option<&str>,
            id: &str,
            body: &str,
        ) -> WireResponse {
            transition_task(self.state(), &json_request(bearer, key, body), id)
        }
    }

    fn revision_body(revision: u64) -> String {
        json!({ "schema_version": 1, "expected_revision": revision }).to_string()
    }

    fn step(revision: u64, target: &str) -> String {
        json!({ "schema_version": 1, "expected_revision": revision, "target_state": target })
            .to_string()
    }

    fn code(response: &WireResponse) -> String {
        field(&parse_strict(&response.body).unwrap(), "code")
    }

    fn assert_problem(
        response: &WireResponse,
        operation: OperationId,
        status: u16,
        expected: &str,
    ) {
        assert_eq!(response.status, status, "{}", response.body);
        assert_eq!(code(response), expected);
        response.validate(Some(operation)).unwrap();
    }

    fn assert_ok(response: &WireResponse, operation: OperationId, status: u16) -> JsonValue {
        assert_eq!(response.status, status, "{}", response.body);
        response.validate(Some(operation)).unwrap();
        parse_strict(&response.body).unwrap()
    }

    fn items(body: &JsonValue, id: &str) -> Vec<String> {
        body.get("items")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .map(|item| field(item, id))
            .collect()
    }

    fn project_count(fixture: &Fixture) -> i64 {
        let status = get_status(&fixture.state().plane, Some(&fixture.control));
        parse_strict(&status.body)
            .unwrap()
            .get("project_count")
            .and_then(JsonValue::as_i64)
            .unwrap()
    }

    #[test]
    fn register_project_is_idempotent_and_keyed_by_repository() {
        let fixture = Fixture::new();
        let op = OperationId::RegisterProject;
        assert_eq!(project_count(&fixture), 0);
        let key = fixture.key();
        let first = assert_ok(
            &fixture.register_with(&fixture.control, Some(&key), &fixture.repo),
            op,
            201,
        );
        assert_eq!(field(&first, "kind"), "git");
        assert_eq!(project_count(&fixture), 1);

        // Same key, same request: the same Project.
        let replay = assert_ok(
            &fixture.register_with(&fixture.control, Some(&key), &fixture.repo),
            op,
            201,
        );
        assert_eq!(replay, first);
        // A new key on another path of the same repository: still the same
        // Project and root Workspace.
        let subdir = fixture.repo.join("src");
        std::fs::create_dir(&subdir).unwrap();
        let other_path = assert_ok(
            &fixture.register_with(&fixture.control, Some(&fixture.key()), &subdir),
            op,
            201,
        );
        assert_eq!(other_path, first);
        assert_eq!(project_count(&fixture), 1);

        // Same key, different request.
        let other_repo = git_repository(fixture.repos.path(), "other");
        assert_problem(
            &fixture.register_with(&fixture.control, Some(&key), &other_repo),
            op,
            409,
            "idempotency_conflict",
        );
        assert_eq!(project_count(&fixture), 1);
    }

    #[test]
    fn register_project_rejects_invalid_input_and_insufficient_scope() {
        let fixture = Fixture::new();
        let op = OperationId::RegisterProject;
        let plain = fixture.repos.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        for root in [
            plain.as_path(),
            Path::new("relative/path"),
            Path::new("/does/not/exist"),
        ] {
            assert_problem(
                &fixture.register_with(&fixture.control, Some(&fixture.key()), root),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &fixture.register_with(&fixture.control, None, &fixture.repo),
            op,
            400,
            "invalid_request",
        );
        for body in [
            r#"{"schema_version":1}"#.to_owned(),
            r#"{"schema_version":1,"root":""}"#.to_owned(),
            json!({ "schema_version": 1, "root": fixture.repo, "extra": 1 }).to_string(),
            json!({ "schema_version": 2, "root": fixture.repo }).to_string(),
        ] {
            assert_problem(
                &register_project(
                    fixture.state(),
                    &json_request(Some(&fixture.control), Some(&fixture.key()), &body),
                ),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &fixture.register_with(&fixture.reader, Some(&fixture.key()), &fixture.repo),
            op,
            403,
            "policy_denied",
        );
        let anonymous = register_project(
            fixture.state(),
            &json_request(
                None,
                Some(&fixture.key()),
                r#"{"schema_version":1,"root":"/"}"#,
            ),
        );
        assert_problem(&anonymous, op, 401, "authentication_required");
        assert_eq!(project_count(&fixture), 0);
    }

    #[test]
    fn get_project_reads_back_the_registration() {
        let fixture = Fixture::new();
        let op = OperationId::GetProject;
        let (project_id, root) = fixture.register();
        let body = assert_ok(
            &get_project(fixture.state(), Some(&fixture.control), &project_id),
            op,
            200,
        );
        assert_eq!(field(&body, "project_id"), project_id);
        assert_eq!(field(&body, "root_workspace_id"), root);
        assert_problem(
            &get_project(
                fixture.state(),
                Some(&fixture.control),
                "018f47a0-0000-7000-8000-000000000001",
            ),
            op,
            404,
            "project_not_found",
        );
        for bad in [
            "not-a-uuid",
            "018f47a0-0000-4000-8000-000000000001",
            "018f47a0-0000-7000-c000-000000000001",
        ] {
            assert_problem(
                &get_project(fixture.state(), Some(&fixture.control), bad),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &get_project(fixture.state(), Some(&fixture.writer), &project_id),
            op,
            403,
            "policy_denied",
        );
    }

    #[test]
    fn resolve_workspace_maps_every_outcome_to_the_contract() {
        let fixture = Fixture::new();
        let op = OperationId::ResolveWorkspace;
        let (project_id, root) = fixture.register();
        let subdir = fixture.repo.join("nested");
        std::fs::create_dir(&subdir).unwrap();
        for path in [&fixture.repo, &subdir] {
            let body = assert_ok(
                &fixture.resolve(&fixture.control, &project_id, path.to_str().unwrap()),
                op,
                200,
            );
            assert_eq!(field(&body, "workspace_id"), root);
            assert_eq!(field(&body, "project_id"), project_id);
        }
        let plain = fixture.repos.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_problem(
            &fixture.resolve(&fixture.control, &project_id, plain.to_str().unwrap()),
            op,
            404,
            "workspace_not_resolved",
        );
        let other = git_repository(fixture.repos.path(), "other");
        assert_problem(
            &fixture.resolve(&fixture.control, &project_id, other.to_str().unwrap()),
            op,
            409,
            "project_workspace_mismatch",
        );
        assert_problem(
            &fixture.resolve(
                &fixture.control,
                "018f47a0-0000-7000-8000-000000000001",
                fixture.repo.to_str().unwrap(),
            ),
            op,
            404,
            "project_not_found",
        );
        assert_problem(
            &fixture.resolve(&fixture.control, &project_id, "relative"),
            op,
            400,
            "invalid_request",
        );
        assert_problem(
            &fixture.resolve(&fixture.reader, &project_id, fixture.repo.to_str().unwrap()),
            op,
            403,
            "policy_denied",
        );
        // Resolution is pure: it registered nothing.
        assert_eq!(project_count(&fixture), 1);
    }

    #[test]
    fn create_session_is_idempotent_and_requires_a_registered_workspace() {
        let fixture = Fixture::new();
        let op = OperationId::CreateSession;
        let (_, root) = fixture.register();
        let agent = "018f47a0-0000-7000-8000-00000000a6e1";
        let request = json!({ "schema_version": 1, "workspace_id": root, "agent_id": agent });
        let key = fixture.key();
        let first = assert_ok(
            &fixture.session_with(&fixture.control, &key, &request),
            op,
            201,
        );
        assert_eq!(field(&first, "state"), "active");
        assert_eq!(field(&first, "agent_id"), agent);
        let replay = assert_ok(
            &fixture.session_with(&fixture.control, &key, &request),
            op,
            201,
        );
        assert_eq!(replay, first);
        let other_agent = json!({
            "schema_version": 1,
            "workspace_id": root,
            "agent_id": "018f47a0-0000-7000-8000-00000000a6e2",
        });
        assert_problem(
            &fixture.session_with(&fixture.control, &key, &other_agent),
            op,
            409,
            "idempotency_conflict",
        );
        assert_problem(
            &fixture.session_with(
                &fixture.control,
                &fixture.key(),
                &json!({
                    "schema_version": 1,
                    "workspace_id": "018f47a0-0000-7000-8000-000000000001",
                    "agent_id": agent,
                }),
            ),
            op,
            404,
            "workspace_not_resolved",
        );
        for bad in [
            json!({ "schema_version": 1, "workspace_id": root }),
            json!({ "schema_version": 1, "workspace_id": root, "agent_id": "agent-1" }),
            json!({ "schema_version": 1, "workspace_id": root, "agent_id": agent, "x": 1 }),
        ] {
            assert_problem(
                &fixture.session_with(&fixture.control, &fixture.key(), &bad),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &create_session(
                fixture.state(),
                &json_request(Some(&fixture.control), None, &request.to_string()),
            ),
            op,
            400,
            "invalid_request",
        );
        assert_problem(
            &fixture.session_with(&fixture.reader, &fixture.key(), &request),
            op,
            403,
            "policy_denied",
        );
    }

    #[test]
    fn list_sessions_pages_one_workspace() {
        let fixture = Fixture::new();
        let op = OperationId::ListSessions;
        let (_, root) = fixture.register();
        let mut created = Vec::new();
        for index in 0..3 {
            let request = json!({
                "schema_version": 1,
                "workspace_id": root,
                "agent_id": format!("018f47a0-0000-7000-8000-00000000a6e{index}"),
            });
            let body = assert_ok(
                &fixture.session_with(&fixture.control, &fixture.key(), &request),
                OperationId::CreateSession,
                201,
            );
            created.push(field(&body, "session_id"));
        }
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let query = match &cursor {
                Some(cursor) => format!("workspace_id={root}&limit=2&cursor={cursor}"),
                None => format!("workspace_id={root}&limit=2"),
            };
            let page = assert_ok(
                &list_sessions(fixture.state(), Some(&fixture.control), Some(&query)),
                op,
                200,
            );
            seen.extend(items(&page, "session_id"));
            match page.get("next_cursor").and_then(JsonValue::as_str) {
                Some(next) => cursor = Some(next.to_owned()),
                None => break,
            }
        }
        let mut expected = created.clone();
        expected.sort();
        let mut sorted_seen = seen.clone();
        sorted_seen.sort();
        assert_eq!(sorted_seen, expected);
        assert_eq!(seen.len(), 3);

        for query in [
            None,
            Some("limit=2".to_owned()),
            Some(format!("workspace_id={root}&limit=0")),
            Some(format!(
                "workspace_id={root}&cursor=018f47a0-0000-7000-8000-000000000001"
            )),
        ] {
            assert_problem(
                &list_sessions(fixture.state(), Some(&fixture.control), query.as_deref()),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &list_sessions(
                fixture.state(),
                Some(&fixture.control),
                Some("workspace_id=018f47a0-0000-7000-8000-000000000001"),
            ),
            op,
            404,
            "workspace_not_resolved",
        );
        assert_problem(
            &list_sessions(
                fixture.state(),
                Some(&fixture.writer),
                Some(&format!("workspace_id={root}")),
            ),
            op,
            403,
            "policy_denied",
        );
    }

    #[test]
    fn create_task_is_idempotent_and_checks_its_workspace() {
        let fixture = Fixture::new();
        let op = OperationId::CreateTask;
        let (project_id, root) = fixture.register();
        let request = json!({
            "schema_version": 1,
            "project_id": project_id,
            "workspace_id": root,
            "title": "Write the slice C report",
            "description": "with transcripts",
        });
        let key = fixture.key();
        let first = assert_ok(
            &fixture.task_with(&fixture.control, &key, &request),
            op,
            201,
        );
        assert_eq!(field(&first, "state"), "proposed");
        assert_eq!(field(&first, "workspace_id"), root);
        let replay = assert_ok(
            &fixture.task_with(&fixture.control, &key, &request),
            op,
            201,
        );
        assert_eq!(replay, first);
        let retitled = json!({ "schema_version": 1, "project_id": project_id, "title": "x" });
        assert_problem(
            &fixture.task_with(&fixture.control, &key, &retitled),
            op,
            409,
            "idempotency_conflict",
        );
        let unbound = assert_ok(
            &fixture.task_with(&fixture.control, &fixture.key(), &retitled),
            op,
            201,
        );
        assert_eq!(unbound.get("workspace_id"), Some(&JsonValue::Null));

        // A Workspace of another Project is a mismatch.
        let other = git_repository(fixture.repos.path(), "other");
        let other_root = parse_strict(
            &fixture
                .register_with(&fixture.control, Some(&fixture.key()), &other)
                .body,
        )
        .unwrap();
        assert_problem(
            &fixture.task_with(
                &fixture.control,
                &fixture.key(),
                &json!({
                    "schema_version": 1,
                    "project_id": project_id,
                    "workspace_id": field(&other_root, "root_workspace_id"),
                    "title": "t",
                }),
            ),
            op,
            409,
            "project_workspace_mismatch",
        );
        assert_problem(
            &fixture.task_with(
                &fixture.control,
                &fixture.key(),
                &json!({
                    "schema_version": 1,
                    "project_id": "018f47a0-0000-7000-8000-000000000001",
                    "title": "t",
                }),
            ),
            op,
            404,
            "project_not_found",
        );
        for bad in [
            json!({ "schema_version": 1, "project_id": project_id, "title": "" }),
            json!({ "schema_version": 1, "project_id": project_id }),
            json!({ "schema_version": 1, "project_id": project_id, "title": "t", "workspace_id": "x" }),
            json!({ "schema_version": 1, "project_id": project_id, "title": "t", "state": "ready" }),
        ] {
            assert_problem(
                &fixture.task_with(&fixture.control, &fixture.key(), &bad),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &fixture.task_with(&fixture.reader, &fixture.key(), &request),
            op,
            403,
            "policy_denied",
        );
    }

    #[test]
    fn list_tasks_pages_one_project() {
        let fixture = Fixture::new();
        let op = OperationId::ListTasks;
        let (project_id, _) = fixture.register();
        let mut created = Vec::new();
        for index in 0..3 {
            let request = json!({ "schema_version": 1, "project_id": project_id, "title": format!("t{index}") });
            let body = assert_ok(
                &fixture.task_with(&fixture.control, &fixture.key(), &request),
                OperationId::CreateTask,
                201,
            );
            created.push(field(&body, "task_id"));
        }
        let first = assert_ok(
            &list_tasks(
                fixture.state(),
                Some(&fixture.control),
                Some(&format!("project_id={project_id}&limit=2")),
            ),
            op,
            200,
        );
        let cursor = field(&first, "next_cursor");
        let second = assert_ok(
            &list_tasks(
                fixture.state(),
                Some(&fixture.control),
                Some(&format!("project_id={project_id}&limit=2&cursor={cursor}")),
            ),
            op,
            200,
        );
        assert_eq!(second.get("next_cursor"), Some(&JsonValue::Null));
        let mut seen = items(&first, "task_id");
        seen.extend(items(&second, "task_id"));
        assert_eq!(seen, created);

        assert_problem(
            &list_tasks(
                fixture.state(),
                Some(&fixture.control),
                Some("project_id=018f47a0-0000-7000-8000-000000000001"),
            ),
            op,
            404,
            "project_not_found",
        );
        for query in [None, Some("project_id=nope".to_owned())] {
            assert_problem(
                &list_tasks(fixture.state(), Some(&fixture.control), query.as_deref()),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &list_tasks(
                fixture.state(),
                Some(&fixture.writer),
                Some(&format!("project_id={project_id}")),
            ),
            op,
            403,
            "policy_denied",
        );
    }

    #[test]
    fn concurrent_mutations_sharing_one_key_record_one_resource() {
        const CALLERS: usize = 8;
        let fixture = Arc::new(Fixture::new());
        let (project_id, root) = fixture.register();
        let task = json!({ "schema_version": 1, "project_id": project_id, "title": "once" });
        let session = json!({
            "schema_version": 1,
            "workspace_id": root,
            "agent_id": "018f47a0-0000-7000-8000-00000000a6e1",
        });
        let barrier = Arc::new(std::sync::Barrier::new(CALLERS * 2));
        let handles = (0..CALLERS)
            .flat_map(|_| {
                [
                    (
                        OperationId::CreateTask,
                        "shared-task-key-0001",
                        task.clone(),
                    ),
                    (
                        OperationId::CreateSession,
                        "shared-session-key-01",
                        session.clone(),
                    ),
                ]
            })
            .map(|(operation, key, body)| {
                let fixture = Arc::clone(&fixture);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let response = if operation == OperationId::CreateTask {
                        fixture.task_with(&fixture.control, key, &body)
                    } else {
                        fixture.session_with(&fixture.control, key, &body)
                    };
                    let body = assert_ok(&response, operation, 201);
                    let id = if operation == OperationId::CreateTask {
                        "task_id"
                    } else {
                        "session_id"
                    };
                    (operation, field(&body, id))
                })
            })
            .collect::<Vec<_>>();
        let mut task_ids = std::collections::BTreeSet::new();
        let mut session_ids = std::collections::BTreeSet::new();
        for handle in handles {
            match handle.join().unwrap() {
                (OperationId::CreateTask, id) => task_ids.insert(id),
                (_, id) => session_ids.insert(id),
            };
        }
        assert_eq!((task_ids.len(), session_ids.len()), (1, 1));
        let tasks = assert_ok(
            &list_tasks(
                fixture.state(),
                Some(&fixture.control),
                Some(&format!("project_id={project_id}")),
            ),
            OperationId::ListTasks,
            200,
        );
        assert_eq!(
            items(&tasks, "task_id"),
            task_ids.into_iter().collect::<Vec<_>>()
        );
        let sessions = assert_ok(
            &list_sessions(
                fixture.state(),
                Some(&fixture.control),
                Some(&format!("workspace_id={root}")),
            ),
            OperationId::ListSessions,
            200,
        );
        assert_eq!(
            items(&sessions, "session_id"),
            session_ids.into_iter().collect::<Vec<_>>()
        );
    }

    /// A Git probe that takes far longer than a health check may.
    struct SlowGit(Duration);

    impl GitResolver for SlowGit {
        fn resolve(&self, _path: &Path) -> Result<ResolvedWorktree, NotResolvable> {
            std::thread::sleep(self.0);
            Err(NotResolvable("slow fixture".to_owned()))
        }
    }

    fn exchange(socket: &Path, request: &str) -> String {
        let mut stream = UnixStream::connect(socket).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn health_answers_while_a_protected_operation_blocks() {
        let blocked_for = Duration::from_secs(2);
        let harness = Harness::with_git(true, Box::new(SlowGit(blocked_for)));
        let control = harness.bootstrap();
        let (dir, state) = harness.into_parts();
        let socket = dir.path().join("probe.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        // The same single-threaded runtime shape as `memproved`.
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                    axum::serve(listener, router(Arc::new(state))).await
                })
        });

        let body = r#"{"schema_version":1,"root":"/slow"}"#;
        let register = format!(
            "POST /v1/projects/register HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
             Authorization: Bearer {control}\r\nIdempotency-Key: slow-key-00000001\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let slow_socket = socket.clone();
        let started = Instant::now();
        let slow = std::thread::spawn(move || exchange(&slow_socket, &register));
        std::thread::sleep(Duration::from_millis(300));

        for path in ["/health/live", "/health/ready"] {
            let probe = Instant::now();
            let response = exchange(
                &socket,
                &format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"),
            );
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(
                probe.elapsed() < Duration::from_millis(500),
                "{path} took {:?} while registerProject was blocked",
                probe.elapsed()
            );
        }
        assert!(!slow.is_finished(), "the blocking call ended too early");
        let response = slow.join().unwrap();
        assert!(started.elapsed() >= blocked_for);
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Every endSession outcome against one Session.
    fn end_session_is_idempotent_and_refuses_a_second_end() {
        let fixture = Fixture::new();
        let op = OperationId::EndSession;
        let (_, root) = fixture.register();
        let session = fixture.session(&root);
        let control = Some(fixture.control.as_str());
        let key = fixture.key();

        let ended = assert_ok(
            &fixture.end(control, Some(&key), &session, &revision_body(1)),
            op,
            200,
        );
        assert_eq!(field(&ended, "state"), "ended");
        assert_eq!(ended.get("revision").and_then(JsonValue::as_i64), Some(2));
        // Replay: same key, same request, same result.
        let replay = assert_ok(
            &fixture.end(control, Some(&key), &session, &revision_body(1)),
            op,
            200,
        );
        assert_eq!(replay, ended);
        // Same key, different request.
        let with_reason =
            json!({ "schema_version": 1, "expected_revision": 1, "reason": "x" }).to_string();
        assert_problem(
            &fixture.end(control, Some(&key), &session, &with_reason),
            op,
            409,
            "idempotency_conflict",
        );
        // Ending again: the current revision is an invalid transition; a stale
        // one is a revision conflict.
        assert_problem(
            &fixture.end(control, Some(&fixture.key()), &session, &revision_body(2)),
            op,
            409,
            "invalid_transition",
        );
        assert_problem(
            &fixture.end(control, Some(&fixture.key()), &session, &revision_body(1)),
            op,
            409,
            "revision_conflict",
        );
        let listed = list_sessions(
            fixture.state(),
            control,
            Some(&format!("workspace_id={root}")),
        );
        assert_ok(&listed, OperationId::ListSessions, 200);
        assert!(listed.body.contains(r#""state":"ended""#));

        // A fresh Session with a reason ends; the reason is not stored.
        let other = fixture.session(&root);
        assert_ok(
            &fixture.end(control, Some(&fixture.key()), &other, &with_reason),
            op,
            200,
        );
        assert_problem(
            &fixture.end(
                control,
                Some(&fixture.key()),
                "018f47a0-0000-7000-8000-000000000001",
                &revision_body(1),
            ),
            op,
            404,
            "resource_not_found",
        );
        assert_problem(
            &fixture.end(
                control,
                Some(&fixture.key()),
                "not-a-uuid",
                &revision_body(1),
            ),
            op,
            400,
            "invalid_request",
        );
        for body in [
            r#"{"schema_version":1}"#,
            r#"{"schema_version":1,"expected_revision":0}"#,
            r#"{"schema_version":1,"expected_revision":"1"}"#,
            r#"{"schema_version":2,"expected_revision":1}"#,
            r#"{"schema_version":1,"expected_revision":1,"reason":""}"#,
            r#"{"schema_version":1,"expected_revision":1,"extra":true}"#,
        ] {
            assert_problem(
                &fixture.end(control, Some(&fixture.key()), &other, body),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &fixture.end(control, None, &other, &revision_body(2)),
            op,
            400,
            "invalid_request",
        );
        // A revision beyond any stored one is a conflict, not a crash.
        assert_problem(
            &fixture.end(
                control,
                Some(&fixture.key()),
                &other,
                r#"{"schema_version":1,"expected_revision":99999999999999999999}"#,
            ),
            op,
            409,
            "revision_conflict",
        );
        assert_problem(
            &fixture.end(
                Some(&fixture.reader),
                Some(&fixture.key()),
                &other,
                &revision_body(2),
            ),
            op,
            403,
            "policy_denied",
        );
        assert_problem(
            &fixture.end(None, Some(&fixture.key()), &other, &revision_body(2)),
            op,
            401,
            "authentication_required",
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Every transitionTask outcome along one Task's life.
    fn transition_task_follows_the_domain_state_machine() {
        let fixture = Fixture::new();
        let op = OperationId::TransitionTask;
        let (project_id, _) = fixture.register();
        let task = fixture.task(&project_id);
        let control = Some(fixture.control.as_str());
        let key = fixture.key();

        // proposed -> in_progress is not an edge.
        assert_problem(
            &fixture.transition(
                control,
                Some(&fixture.key()),
                &task,
                &step(1, "in_progress"),
            ),
            op,
            409,
            "invalid_transition",
        );
        let ready = assert_ok(
            &fixture.transition(control, Some(&key), &task, &step(1, "ready")),
            op,
            200,
        );
        assert_eq!(field(&ready, "state"), "ready");
        assert_eq!(ready.get("revision").and_then(JsonValue::as_i64), Some(2));
        assert_problem(
            &fixture.transition(control, Some(&key), &task, &step(1, "cancelled")),
            op,
            409,
            "idempotency_conflict",
        );
        assert_problem(
            &fixture.transition(
                control,
                Some(&fixture.key()),
                &task,
                &step(1, "in_progress"),
            ),
            op,
            409,
            "revision_conflict",
        );
        let with_empty_evidence = json!({
            "schema_version": 1,
            "expected_revision": 2,
            "target_state": "in_progress",
            "completion_evidence_ids": [],
        })
        .to_string();
        let started = assert_ok(
            &fixture.transition(control, Some(&fixture.key()), &task, &with_empty_evidence),
            op,
            200,
        );
        assert_eq!(field(&started, "state"), "in_progress");
        // A replay after later changes still returns the first result.
        assert_eq!(
            assert_ok(
                &fixture.transition(control, Some(&key), &task, &step(1, "ready")),
                op,
                200
            ),
            ready
        );
        // P2-C: completed without completion Evidence does not hold.
        assert_problem(
            &fixture.transition(control, Some(&fixture.key()), &task, &step(3, "completed")),
            op,
            409,
            "evidence_stale",
        );
        let blocked = assert_ok(
            &fixture.transition(control, Some(&fixture.key()), &task, &step(3, "blocked")),
            op,
            200,
        );
        assert_eq!(field(&blocked, "state"), "blocked");
        let cancelled = assert_ok(
            &fixture.transition(control, Some(&fixture.key()), &task, &step(4, "cancelled")),
            op,
            200,
        );
        assert_eq!(
            cancelled.get("revision").and_then(JsonValue::as_i64),
            Some(5)
        );
        // cancelled is terminal.
        for target in [
            "proposed",
            "ready",
            "in_progress",
            "blocked",
            "completed",
            "cancelled",
        ] {
            assert_problem(
                &fixture.transition(control, Some(&fixture.key()), &task, &step(5, target)),
                op,
                409,
                "invalid_transition",
            );
        }

        let other = fixture.task(&project_id);
        for body in [
            json!({ "schema_version": 1, "expected_revision": 1, "target_state": "done" })
                .to_string(),
            json!({ "schema_version": 1, "expected_revision": 1 }).to_string(),
            json!({
                "schema_version": 1, "expected_revision": 1, "target_state": "ready",
                "completion_evidence_ids": ["018f47a0-0000-7000-8000-000000000001"],
            })
            .to_string(),
            json!({
                "schema_version": 1, "expected_revision": 1, "target_state": "ready",
                "force_reason": "because",
            })
            .to_string(),
        ] {
            assert_problem(
                &fixture.transition(control, Some(&fixture.key()), &other, &body),
                op,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &fixture.transition(control, None, &other, &step(1, "ready")),
            op,
            400,
            "invalid_request",
        );
        assert_problem(
            &fixture.transition(
                control,
                Some(&fixture.key()),
                "018f47a0-0000-7000-8000-000000000001",
                &step(1, "ready"),
            ),
            op,
            404,
            "resource_not_found",
        );
        assert_problem(
            &fixture.transition(
                Some(&fixture.reader),
                Some(&fixture.key()),
                &other,
                &step(1, "ready"),
            ),
            op,
            403,
            "policy_denied",
        );
        assert_problem(
            &fixture.transition(None, Some(&fixture.key()), &other, &step(1, "ready")),
            op,
            401,
            "authentication_required",
        );
        let listed = list_tasks(
            fixture.state(),
            control,
            Some(&format!("project_id={project_id}")),
        );
        assert_ok(&listed, OperationId::ListTasks, 200);
        assert!(listed.body.contains(r#""state":"cancelled""#));
    }

    #[test]
    fn concurrent_transitions_of_one_task_admit_exactly_one() {
        let fixture = Fixture::new();
        let (project_id, _) = fixture.register();
        let task = fixture.task(&project_id);
        let targets = [
            "ready",
            "cancelled",
            "ready",
            "cancelled",
            "ready",
            "cancelled",
        ];
        let barrier = std::sync::Barrier::new(targets.len());
        let outcomes: Vec<WireResponse> = std::thread::scope(|scope| {
            let handles: Vec<_> = targets
                .iter()
                .map(|target| {
                    let key = fixture.key();
                    let (fixture, task, barrier) = (&fixture, &task, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        fixture.transition(
                            Some(&fixture.control),
                            Some(&key),
                            task,
                            &step(1, target),
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let won: Vec<_> = outcomes.iter().filter(|r| r.status == 200).collect();
        assert_eq!(won.len(), 1, "{outcomes:?}");
        for lost in outcomes.iter().filter(|r| r.status != 200) {
            assert_problem(lost, OperationId::TransitionTask, 409, "revision_conflict");
        }
        let winner = parse_strict(&won[0].body).unwrap();
        assert_eq!(winner.get("revision").and_then(JsonValue::as_i64), Some(2));
        let listed = list_tasks(
            fixture.state(),
            Some(&fixture.control),
            Some(&format!("project_id={project_id}")),
        );
        assert!(
            listed
                .body
                .contains(&format!(r#""state":"{}""#, field(&winner, "state")))
        );
        assert!(listed.body.contains(r#""revision":2"#));
    }

    /// Events of the Project, oldest first, as `(event_type, payload)`.
    fn ledger(fixture: &Fixture, project_id: &str) -> Vec<(String, serde_json::Value)> {
        let page = crate::events::list_events(
            fixture.state(),
            Some(&fixture.control),
            Some(&format!("project_id={project_id}&limit=500")),
        );
        let page: serde_json::Value = serde_json::from_str(&page.body).unwrap();
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["event_type"].as_str().unwrap().to_owned(),
                    item["payload"].clone(),
                )
            })
            .collect()
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One Session lifecycle, every ADR 0012 outcome.
    fn session_lifecycle_records_started_and_ended_events_and_links_its_task() {
        let fixture = Fixture::new();
        let op = OperationId::CreateSession;
        let (project_id, root) = fixture.register();
        let task_body = json!({
            "schema_version": 1, "project_id": project_id, "workspace_id": root, "title": "t",
        });
        let task = field(
            &assert_ok(
                &fixture.task_with(&fixture.control, &fixture.key(), &task_body),
                OperationId::CreateTask,
                201,
            ),
            "task_id",
        );
        let agent = "018f47a0-0000-7000-8000-00000000a6e1";
        let request = |workspace: &str, task_id: &serde_json::Value| {
            json!({
                "schema_version": 1, "workspace_id": workspace, "agent_id": agent,
                "task_id": task_id,
            })
        };

        // Invalid, unknown, and cross-Workspace Tasks write nothing.
        assert_problem(
            &fixture.session_with(
                &fixture.control,
                &fixture.key(),
                &request(&root, &json!("x")),
            ),
            op,
            400,
            "invalid_request",
        );
        assert_problem(
            &fixture.session_with(
                &fixture.control,
                &fixture.key(),
                &request(&root, &json!("018f47a0-0000-7000-8000-000000000001")),
            ),
            op,
            404,
            "resource_not_found",
        );
        let linked = git_worktree(
            &fixture.repo,
            &fixture.repos.path().join("repo-linked"),
            "feature",
        );
        let linked_id = field(
            &assert_ok(
                &fixture.resolve(&fixture.control, &project_id, linked.to_str().unwrap()),
                OperationId::ResolveWorkspace,
                200,
            ),
            "workspace_id",
        );
        assert_problem(
            &fixture.session_with(
                &fixture.control,
                &fixture.key(),
                &request(&linked_id, &json!(task)),
            ),
            op,
            409,
            "project_workspace_mismatch",
        );
        // Only the Task's task.created and the linked worktree's
        // workspace.registered (P2-C): the refused Sessions wrote nothing.
        let base = ledger(&fixture, &project_id);
        assert_eq!(
            base.iter()
                .map(|(kind, _)| kind.as_str())
                .collect::<Vec<_>>(),
            ["task.created", "workspace.registered"]
        );
        let sessions = |fixture: &Fixture| ledger(fixture, &project_id)[base.len()..].to_vec();

        // An agent's mcp-write token starts the Session; a replay adds nothing.
        let key = fixture.key();
        let created = assert_ok(
            &fixture.session_with(&fixture.writer, &key, &request(&root, &json!(task))),
            op,
            201,
        );
        let session = field(&created, "session_id");
        let replayed = assert_ok(
            &fixture.session_with(&fixture.writer, &key, &request(&root, &json!(task))),
            op,
            201,
        );
        assert_eq!(replayed, created);
        assert_problem(
            &fixture.session_with(&fixture.writer, &key, &request(&root, &json!(null))),
            op,
            409,
            "idempotency_conflict",
        );
        assert_eq!(
            sessions(&fixture),
            vec![
                (
                    "session.started".to_owned(),
                    json!({ "agent_id": agent, "task_id": task }),
                ),
                (
                    "task.session_linked".to_owned(),
                    json!({ "session_id": session, "task_id": task }),
                ),
            ]
        );
        // Without a Task, the payload names none and nothing is linked.
        fixture.session(&root);
        assert_eq!(
            sessions(&fixture)[2],
            (
                "session.started".to_owned(),
                json!({ "agent_id": agent, "task_id": null }),
            )
        );

        // Ending records the reason; a replay adds nothing.
        let end_key = fixture.key();
        let end_body = r#"{"schema_version":1,"expected_revision":1,"reason":"verify done"}"#;
        assert_ok(
            &fixture.end(Some(&fixture.writer), Some(&end_key), &session, end_body),
            OperationId::EndSession,
            200,
        );
        assert_ok(
            &fixture.end(Some(&fixture.writer), Some(&end_key), &session, end_body),
            OperationId::EndSession,
            200,
        );
        let events = sessions(&fixture);
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[3],
            (
                "session.ended".to_owned(),
                json!({ "reason": "verify done" }),
            )
        );

        // The ended Session accepts no further Event.
        let late = json!({
            "envelope_version": "1.0",
            "event_id": uuid::Uuid::now_v7().to_string(),
            "project_id": project_id,
            "workspace_id": root,
            "session_id": session,
            "event_type": "turn.started",
            "payload_schema": "turn.started@1",
            "producer": { "name": "test", "version": "1", "sequence": 0 },
            "privacy_class": "internal",
            "payload": {},
        });
        assert_problem(
            &crate::events::ingest_event(
                fixture.state(),
                &json_request(
                    Some(&fixture.writer),
                    Some(&fixture.key()),
                    &late.to_string(),
                ),
            ),
            OperationId::IngestEvent,
            409,
            "invalid_transition",
        );
        assert_eq!(sessions(&fixture).len(), 4);

        // The stored chain verifies offline.
        let data_dir = std::fs::canonicalize(fixture.harness.dir.path()).unwrap();
        let registry = memprove_store_sqlite::RegistryStore::open(&data_dir).unwrap();
        let project = registry
            .get_project(&ProjectId::parse(&project_id).unwrap())
            .unwrap();
        let store = memprove_store_sqlite::StateStore::open(&data_dir, &project).unwrap();
        store.verify_event_chain().unwrap();
        assert_eq!(
            store
                .list_task_sessions(&TaskId::parse(&task).unwrap())
                .unwrap(),
            vec![SessionId::parse(&session).unwrap()]
        );
    }

    #[test]
    fn linked_worktrees_of_the_project_repository_register_once() {
        let fixture = Fixture::new();
        let op = OperationId::ResolveWorkspace;
        let (project_id, root) = fixture.register();
        let linked = git_worktree(
            &fixture.repo,
            &fixture.repos.path().join("repo-linked"),
            "feature",
        );
        let nested = linked.join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();

        let first = assert_ok(
            &fixture.resolve(&fixture.control, &project_id, linked.to_str().unwrap()),
            op,
            200,
        );
        let linked_id = field(&first, "workspace_id");
        assert_ne!(linked_id, root);
        assert_eq!(field(&first, "project_id"), project_id);
        for path in [&linked, &nested] {
            let again = assert_ok(
                &fixture.resolve(&fixture.control, &project_id, path.to_str().unwrap()),
                op,
                200,
            );
            assert_eq!(again, first);
        }
        // The root still resolves to the root, and getProject is unchanged.
        let root_body = assert_ok(
            &fixture.resolve(
                &fixture.control,
                &project_id,
                fixture.repo.to_str().unwrap(),
            ),
            op,
            200,
        );
        assert_eq!(field(&root_body, "workspace_id"), root);
        let project = assert_ok(
            &get_project(fixture.state(), Some(&fixture.control), &project_id),
            OperationId::GetProject,
            200,
        );
        assert_eq!(field(&project, "root_workspace_id"), root);
        // The linked Workspace is a real Workspace: it takes Sessions.
        fixture.session(&linked_id);

        // Concurrent first resolutions of another linked worktree agree.
        let second = git_worktree(
            &fixture.repo,
            &fixture.repos.path().join("repo-second"),
            "second",
        );
        let ids: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    let (fixture, project_id, second) = (&fixture, &project_id, &second);
                    scope.spawn(move || {
                        let response =
                            fixture.resolve(&fixture.control, project_id, second.to_str().unwrap());
                        assert_eq!(response.status, 200, "{}", response.body);
                        field(&parse_strict(&response.body).unwrap(), "workspace_id")
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        assert!(ids.iter().all(|id| id == &ids[0]), "{ids:?}");
        assert_ne!(ids[0], linked_id);

        // Unrelated repositories (and their linked worktrees) and non-Git
        // directories are never registered.
        let other = git_repository(fixture.repos.path(), "other");
        let other_linked = git_worktree(&other, &fixture.repos.path().join("other-linked"), "o");
        for path in [&other, &other_linked] {
            assert_problem(
                &fixture.resolve(&fixture.control, &project_id, path.to_str().unwrap()),
                op,
                409,
                "project_workspace_mismatch",
            );
        }
        let plain = fixture.repos.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_problem(
            &fixture.resolve(&fixture.control, &project_id, plain.to_str().unwrap()),
            op,
            404,
            "workspace_not_resolved",
        );
        assert_problem(
            &fixture.resolve(&fixture.reader, &project_id, linked.to_str().unwrap()),
            op,
            403,
            "policy_denied",
        );
        assert_eq!(project_count(&fixture), 1);
    }

    #[test]
    fn registering_again_completes_a_project_that_crashed_before_activation() {
        let fixture = Fixture::new();
        let data_dir = std::fs::canonicalize(fixture.harness.dir.path()).unwrap();
        // A registration that died after the registry row and the state
        // database, before the root Workspace and activation.
        let records = memprove_repo::GitWorkspaceResolver::resolve(&fixture.repo)
            .unwrap()
            .registration_records(&data_dir, &memprove_domain::UtcTimestamp::now());
        let context = IdempotencyContext {
            principal_id: PrincipalId::parse("crashed-principal").unwrap(),
            operation_id: DomainOperationId::parse("registerProject").unwrap(),
            key: IdempotencyKey::parse("crashed-registration").unwrap(),
        };
        memprove_store_sqlite::RegistryStore::open(&data_dir)
            .unwrap()
            .register_project_begin(
                &records.project,
                &context,
                &memprove_domain::Sha256Digest::of_bytes(b"crashed"),
            )
            .unwrap();
        drop(memprove_store_sqlite::StateStore::open(&data_dir, &records.project).unwrap());
        let crashed = records.project.project_id.to_string();
        // Not ACTIVE: invisible to readers.
        assert_problem(
            &get_project(fixture.state(), Some(&fixture.control), &crashed),
            OperationId::GetProject,
            404,
            "project_not_found",
        );

        let (project_id, root) = fixture.register();
        assert_eq!(project_id, crashed, "the crashed registration is resumed");
        let body = assert_ok(
            &get_project(fixture.state(), Some(&fixture.control), &project_id),
            OperationId::GetProject,
            200,
        );
        assert_eq!(field(&body, "root_workspace_id"), root);
        assert_eq!(project_count(&fixture), 1);
    }
}
