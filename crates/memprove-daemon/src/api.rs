//! Local Control API routes: `healthLive`, `healthReady`, `getStatus`,
//! `getVersion`, the three client-token operations (see [`crate::tokens`]),
//! the nine Project/Workspace/Task/Session operations (see
//! [`crate::catalog`]), the three Event operations (see [`crate::events`]),
//! the four Evidence/Checkpoint operations, and the read-only Recovery
//! Preview (see [`crate::recovery`]).
//! The deferred `ingestEventBatch` and `streamEvents` paths answer like any
//! unrouted path.
//!
//! Every response body is checked against the frozen `memprove-protocol`
//! validators before it leaves the process. A body that fails validation is an
//! internal defect; it is logged and replaced by a bodiless `500`, never sent.
//!
//! The daemon runs a single-threaded async runtime. Protected operations block
//! on `SQLite` and Git subprocesses, so each one runs on Tokio's blocking pool
//! (`spawn_blocking`); the two health probes answer on the runtime thread and
//! stay responsive while protected work is in flight.

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, rejection::BytesRejection},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::Response,
    routing::{any, get, post},
};
use memprove_engine::{
    catalog::Catalog,
    events::EventLedger,
    recovery::Recovery,
    runtime::{AuthenticationError, ControlPlane, Principal},
};
use memprove_protocol::{
    phase1::{
        AccessDecision, AccessRuleKind, CapabilityId, MappingState, OperationId,
        PROBLEM_DETAIL_MAX_CHARS, ProblemCode, binding_for, validate_problem,
        validate_problem_detail, validate_response,
    },
    strict_json::parse_strict,
};
use serde_json::json;

use crate::{auth::SharedRegistry, catalog, events, recovery, tokens};

/// Routed operations, in contract order.
pub const ROUTED_OPERATIONS: [OperationId; 27] = [
    OperationId::HealthLive,
    OperationId::HealthReady,
    OperationId::GetStatus,
    OperationId::GetVersion,
    OperationId::CreateClientToken,
    OperationId::ListClientTokens,
    OperationId::RevokeClientToken,
    OperationId::RegisterProject,
    OperationId::GetProject,
    OperationId::ResolveWorkspace,
    OperationId::CreateSession,
    OperationId::ListSessions,
    OperationId::EndSession,
    OperationId::CreateTask,
    OperationId::ListTasks,
    OperationId::TransitionTask,
    OperationId::IngestEvent,
    OperationId::ListEvents,
    OperationId::GetEvent,
    OperationId::CaptureEvidence,
    OperationId::GetEvidence,
    OperationId::CreateCheckpoint,
    OperationId::GetCheckpoint,
    OperationId::GetRecoveryPreview,
    OperationId::VerifyLedger,
    OperationId::GetTaskCapsule,
    OperationId::ProposeDecision,
];

/// Largest request body read, in bytes (ADR 0012). A larger body is not
/// buffered: the extractor fails and the handler answers like any unreadable
/// body, `400 invalid_request`, after authentication and authorization.
pub const MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Shared request-handling state.
pub struct AppState {
    /// Readiness, authentication, status, and version service.
    pub plane: ControlPlane,
    /// Registry holding client tokens and Projects.
    pub registry: SharedRegistry,
    /// Project, Workspace, Task, and Session service.
    pub catalog: Catalog,
    /// Event ledger service.
    pub events: EventLedger,
    /// Evidence and Checkpoint service.
    pub recovery: Recovery,
}

const JSON: &str = "application/json";
const PROBLEM_JSON: &str = "application/problem+json";

/// A fully rendered response before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireResponse {
    /// HTTP status.
    pub status: u16,
    /// Response media type.
    pub media_type: &'static str,
    /// Serialized JSON body.
    pub body: String,
    /// Whether the RFC 6750 `WWW-Authenticate: Bearer` challenge is attached.
    pub bearer_challenge: bool,
}

impl WireResponse {
    pub(crate) fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            status,
            media_type: JSON,
            body: body.to_string(),
            bearer_challenge: false,
        }
    }

    /// Renders the Problem for `code` with a fresh `UUIDv7` request id.
    ///
    /// A `400` needs [`WireResponse::invalid`], which carries the `detail`
    /// every `400` must have.
    #[must_use]
    pub fn problem(code: ProblemCode) -> Self {
        debug_assert_ne!(code.status(), 400, "a 400 Problem needs a detail");
        Self::render(code, None)
    }

    /// Renders `400 invalid_request` whose `detail` names the field or
    /// parameter and the rule it violates (P3-A).
    ///
    /// `detail` is built from static rule text and schema field names. It
    /// must not repeat a token, a header value, or any text from the request
    /// body; `memprove_protocol::strict_json::echoable_name` is the one way a
    /// caller-chosen name gets in.
    #[must_use]
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::render(ProblemCode::InvalidRequest, Some(detail.into()))
    }

    /// Renders the Problem for `code`, which is neither `400`, `401`, nor
    /// `403`, with a `detail` of fixed text (P3-L3, T16 S-2).
    #[must_use]
    pub fn with_detail(code: ProblemCode, detail: &str) -> Self {
        debug_assert!(
            !matches!(code.status(), 400 | 401 | 403),
            "a 400 uses invalid, and a 401 or 403 carries no detail"
        );
        Self::render(code, Some(detail.to_owned()))
    }

    /// U9 R1 (Owner O4; ADR 0020): `403 role_forbidden` with the rule's
    /// fixed `detail`, the one `403` that carries one.
    #[must_use]
    pub fn role_forbidden(rule: memprove_domain::RoleRule) -> Self {
        Self::render(ProblemCode::RoleForbidden, Some(rule.detail().to_owned()))
    }

    /// Renders `400 unsupported_scope` with its `detail`.
    #[must_use]
    pub fn unsupported_scope(detail: impl Into<String>) -> Self {
        Self::render(ProblemCode::UnsupportedScope, Some(detail.into()))
    }

    fn render(code: ProblemCode, detail: Option<String>) -> Self {
        let status = code.status();
        let mut body = json!({
            "type": format!("https://continuity.local/problems/{}", code.as_str().replace('_', "-")),
            "title": problem_title(code),
            "status": status,
            "code": code.as_str(),
            "request_id": uuid::Uuid::now_v7().to_string(),
            "retryable": matches!(code, ProblemCode::ServiceNotReady),
        });
        if let Some(detail) = detail {
            let detail: String = detail.chars().take(PROBLEM_DETAIL_MAX_CHARS).collect();
            body["detail"] = json!(detail);
        }
        Self {
            status,
            media_type: PROBLEM_JSON,
            body: body.to_string(),
            bearer_challenge: matches!(code, ProblemCode::AuthenticationRequired),
        }
    }

    /// Checks the response against the operation binding, or against the
    /// global Problem closure when no operation matched the route.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic when the body is not strict JSON or violates the
    /// frozen contract.
    pub fn validate(&self, operation: Option<OperationId>) -> Result<(), String> {
        let body = parse_strict(&self.body).map_err(|error| error.to_string())?;
        match operation {
            Some(operation) => validate_response(operation, self.status, self.media_type, &body),
            None if self.media_type == PROBLEM_JSON => validate_problem(self.status, &body),
            None => return Err(format!("unrouted response has media {}", self.media_type)),
        }
        .map_err(|error| error.to_string())?;
        if self.media_type == PROBLEM_JSON {
            // P3-A: what `memproved` emits is held to the `detail` rule: a
            // `400` always says what was wrong, a `401` or `403` never does.
            validate_problem_detail(self.status, &body).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

fn problem_title(code: ProblemCode) -> &'static str {
    match code {
        ProblemCode::InvalidRequest => "Invalid request",
        ProblemCode::UnsupportedScope => "Unsupported scope",
        ProblemCode::AuthenticationRequired => "Authentication required",
        ProblemCode::PolicyDenied => "Policy denied",
        ProblemCode::ProjectNotFound => "Project not found",
        ProblemCode::WorkspaceNotResolved => "Workspace not resolved",
        ProblemCode::ProjectWorkspaceMismatch => "Project and Workspace mismatch",
        ProblemCode::ResourceNotFound => "Resource not found",
        ProblemCode::IdempotencyConflict => "Idempotency conflict",
        ProblemCode::RevisionConflict => "Revision conflict",
        ProblemCode::InvalidTransition => "Invalid transition",
        ProblemCode::LastControlToken => "Last control token",
        ProblemCode::ServiceNotReady => "Service not ready",
        ProblemCode::RoleForbidden => "Role forbidden",
        _ => "Request failed",
    }
}

/// `healthLive`: the process is up and serving.
#[must_use]
pub fn health_live() -> WireResponse {
    WireResponse::json(200, &json!({ "status": "ok" }))
}

/// `healthReady`: ready to serve protected operations.
#[must_use]
pub fn health_ready(plane: &ControlPlane) -> WireResponse {
    if plane.readiness().is_ready() {
        WireResponse::json(200, &json!({ "status": "ok" }))
    } else {
        WireResponse::problem(ProblemCode::ServiceNotReady)
    }
}

/// `getStatus`, after readiness, authentication, and authorization.
#[must_use]
pub fn get_status(plane: &ControlPlane, bearer: Option<&str>) -> WireResponse {
    if let Err(denied) = guard(plane, OperationId::GetStatus, bearer) {
        return denied;
    }
    let Ok(status) = plane.status() else {
        return WireResponse::problem(ProblemCode::ServiceNotReady);
    };
    WireResponse::json(
        200,
        &json!({
            "schema_version": 1,
            "ready": status.ready,
            "writer_queue_depth": status.writer_queue_depth,
            "project_count": status.project_count,
        }),
    )
}

/// `getVersion`, after readiness and authentication.
#[must_use]
pub fn get_version(plane: &ControlPlane, bearer: Option<&str>) -> WireResponse {
    if let Err(denied) = guard(plane, OperationId::GetVersion, bearer) {
        return denied;
    }
    let version = plane.version();
    let mut body = json!({
        "schema_version": 1,
        "application_version": version.application_version,
        "api_major": version.api_major,
        "capabilities": version.capabilities,
    });
    // U1 (E1, Owner O8): the one optional member, after the others.
    if let (Some(build), Some(members)) = (version.build, body.as_object_mut()) {
        members.insert("build".to_owned(), json!(build));
    }
    WireResponse::json(200, &body)
}

/// Response for any route outside the contract, including a known path with
/// an unsupported method: the Problem closure has no `405`, so both map to
/// `404 resource_not_found`.
#[must_use]
pub fn unknown_route() -> WireResponse {
    WireResponse::problem(ProblemCode::ResourceNotFound)
}

/// Protected-operation preamble in the contract's check order: readiness
/// (`503`) before authentication (`401`) before authorization (`403`). An
/// unreadable credential store fails closed with `503`.
///
/// # Errors
///
/// Returns the Problem response that ends the request.
pub(crate) fn guard(
    plane: &ControlPlane,
    operation: OperationId,
    bearer: Option<&str>,
) -> Result<Principal, WireResponse> {
    if !plane.readiness().is_ready() {
        return Err(WireResponse::problem(ProblemCode::ServiceNotReady));
    }
    let principal = plane.authenticate(bearer).map_err(|error| match error {
        AuthenticationError::Rejected => WireResponse::problem(ProblemCode::AuthenticationRequired),
        AuthenticationError::Unavailable => WireResponse::problem(ProblemCode::ServiceNotReady),
    })?;
    let binding = binding_for(operation.as_str()).expect("routed operation has a binding");
    // Routed operations use the AccessRules frozen in OpenAPI v1, which the
    // D6 mapping mirrors; the Owner approved them for slices A and B
    // (2026-10-02). Unrouted operations never reach this point.
    let decision = binding.authorize(
        MappingState::CompleteAndApproved,
        satisfies(
            binding.access_rule.kind,
            binding.access_rule.scopes,
            &principal,
        ),
    );
    match decision {
        AccessDecision::Allowed => Ok(principal),
        AccessDecision::Denied => Err(WireResponse::problem(ProblemCode::PolicyDenied)),
    }
}

fn satisfies(kind: AccessRuleKind, scopes: &[&str], principal: &Principal) -> bool {
    match kind {
        AccessRuleKind::Anonymous | AccessRuleKind::Authenticated => true,
        AccessRuleKind::AnyOf => scopes.iter().any(|scope| principal.has_scope(scope)),
    }
}

/// Extracts the bearer credential from `Authorization: Bearer <token>`.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
        .filter(|token| !token.is_empty())
}

pub(crate) fn finish(operation: Option<OperationId>, response: &WireResponse) -> Response {
    if let Err(diagnostic) = response.validate(operation) {
        let name = operation.map_or("unrouted", OperationId::as_str);
        eprintln!("memproved: internal contract violation in {name} response: {diagnostic}");
        let mut failure = Response::new(Body::empty());
        *failure.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        return failure;
    }
    let mut rendered = Response::new(Body::from(response.body.clone()));
    *rendered.status_mut() =
        StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = rendered.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(response.media_type),
    );
    if response.bearer_challenge {
        headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    rendered
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Owned transport inputs of a mutation, moved onto the blocking pool.
struct OwnedRequest {
    headers: HeaderMap,
    body: Option<Bytes>,
}

impl OwnedRequest {
    fn new(headers: HeaderMap, body: Result<Bytes, BytesRejection>) -> Self {
        Self {
            headers,
            body: body.ok(),
        }
    }

    fn borrow(&self) -> tokens::TokenRequest<'_> {
        tokens::TokenRequest {
            bearer: bearer(&self.headers),
            idempotency_key: header_text(&self.headers, "idempotency-key"),
            content_type: header_text(&self.headers, "content-type"),
            body: self.body.as_deref(),
        }
    }
}

/// Runs a protected handler on the blocking pool and finishes its response.
async fn blocking(
    operation: OperationId,
    work: impl FnOnce() -> WireResponse + Send + 'static,
) -> Response {
    let response = tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| {
            eprintln!("memproved: {operation} handler failed: {error}");
            WireResponse::problem(ProblemCode::ServiceNotReady)
        });
    finish(Some(operation), &response)
}

/// Builds the router over `state`.
#[allow(clippy::too_many_lines)] // One route table keeps the contract surface reviewable.
pub fn router(state: Arc<AppState>) -> Router {
    let ready = Arc::clone(&state);
    let status = Arc::clone(&state);
    let version = Arc::clone(&state);
    let create_token = Arc::clone(&state);
    let list_tokens = Arc::clone(&state);
    let revoke_token = Arc::clone(&state);
    let register = Arc::clone(&state);
    let project = Arc::clone(&state);
    let resolve = Arc::clone(&state);
    let create_session = Arc::clone(&state);
    let list_sessions = Arc::clone(&state);
    let end_session = Arc::clone(&state);
    let create_task = Arc::clone(&state);
    let list_tasks = Arc::clone(&state);
    let transition_task = Arc::clone(&state);
    let ingest_event = Arc::clone(&state);
    let list_events = Arc::clone(&state);
    let get_event = Arc::clone(&state);
    let capture = Arc::clone(&state);
    let evidence = Arc::clone(&state);
    let create_checkpoint = Arc::clone(&state);
    let checkpoint = Arc::clone(&state);
    let verify_ledger = Arc::clone(&state);
    let capsule = Arc::clone(&state);
    let decision = Arc::clone(&state);
    let preview = state;
    Router::new()
        .route(
            "/health/live",
            get(|| async { finish(Some(OperationId::HealthLive), &health_live()) }),
        )
        .route(
            "/health/ready",
            get(move || async move {
                finish(Some(OperationId::HealthReady), &health_ready(&ready.plane))
            }),
        )
        .route(
            "/v1/status",
            get(move |headers: HeaderMap| {
                blocking(OperationId::GetStatus, move || {
                    get_status(&status.plane, bearer(&headers))
                })
            }),
        )
        .route(
            "/v1/version",
            get(move |headers: HeaderMap| {
                blocking(OperationId::GetVersion, move || {
                    get_version(&version.plane, bearer(&headers))
                })
            }),
        )
        .route(
            "/v1/auth/tokens",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::CreateClientToken, move || {
                        tokens::create_client_token(&create_token, &request.borrow())
                    })
                },
            )
            .get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::ListClientTokens, move || {
                    tokens::list_client_tokens(&list_tokens, bearer(&headers), uri.query())
                })
            }),
        )
        .route(
            "/v1/auth/tokens/{token_id}/revoke",
            post(
                move |headers: HeaderMap, uri: Uri, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::RevokeClientToken, move || {
                        let target = tokens::revoke_target(uri.path());
                        tokens::revoke_client_token(&revoke_token, &request.borrow(), target)
                    })
                },
            ),
        )
        .route(
            "/v1/projects/register",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::RegisterProject, move || {
                        catalog::register_project(&register, &request.borrow())
                    })
                },
            ),
        )
        .route(
            "/v1/projects/{project_id}",
            get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::GetProject, move || {
                    let target = catalog::project_target(uri.path());
                    catalog::get_project(&project, bearer(&headers), target)
                })
            }),
        )
        .route(
            "/v1/workspaces/resolve",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::ResolveWorkspace, move || {
                        catalog::resolve_workspace(&resolve, &request.borrow())
                    })
                },
            ),
        )
        .route(
            "/v1/sessions",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::CreateSession, move || {
                        catalog::create_session(&create_session, &request.borrow())
                    })
                },
            )
            .get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::ListSessions, move || {
                    catalog::list_sessions(&list_sessions, bearer(&headers), uri.query())
                })
            }),
        )
        .route(
            "/v1/sessions/{session_id}/end",
            post(
                move |headers: HeaderMap, uri: Uri, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::EndSession, move || {
                        let target = catalog::session_end_target(uri.path());
                        catalog::end_session(&end_session, &request.borrow(), target)
                    })
                },
            ),
        )
        .route(
            "/v1/tasks",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::CreateTask, move || {
                        catalog::create_task(&create_task, &request.borrow())
                    })
                },
            )
            .get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::ListTasks, move || {
                    catalog::list_tasks(&list_tasks, bearer(&headers), uri.query())
                })
            }),
        )
        .route(
            "/v1/tasks/{task_id}/transition",
            post(
                move |headers: HeaderMap, uri: Uri, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::TransitionTask, move || {
                        let target = catalog::task_transition_target(uri.path());
                        catalog::transition_task(&transition_task, &request.borrow(), target)
                    })
                },
            ),
        )
        .route(
            "/v1/tasks/{task_id}/recovery-preview",
            get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::GetRecoveryPreview, move || {
                    let target = recovery::recovery_preview_target(uri.path());
                    recovery::get_recovery_preview(&preview, bearer(&headers), target, uri.query())
                })
            }),
        )
        .route(
            "/v1/tasks/{task_id}/recovery-capsule",
            get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::GetTaskCapsule, move || {
                    let target = recovery::task_capsule_target(uri.path());
                    recovery::get_task_capsule(&capsule, bearer(&headers), target, uri.query())
                })
            }),
        )
        .route(
            "/v1/events",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::IngestEvent, move || {
                        events::ingest_event(&ingest_event, &request.borrow())
                    })
                },
            )
            .get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::ListEvents, move || {
                    events::list_events(&list_events, bearer(&headers), uri.query())
                })
            }),
        )
        // Deferred operations: static segments win over `{event_id}`.
        .route(
            "/v1/events/batch",
            any(|| async { finish(None, &unknown_route()) }),
        )
        .route(
            "/v1/events/stream",
            any(|| async { finish(None, &unknown_route()) }),
        )
        .route(
            "/v1/events/{event_id}",
            get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::GetEvent, move || {
                    let target = events::event_target(uri.path());
                    events::get_event(&get_event, bearer(&headers), target)
                })
            }),
        )
        .route(
            "/v1/evidence/capture",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::CaptureEvidence, move || {
                        recovery::capture_evidence(&capture, &request.borrow())
                    })
                },
            ),
        )
        .route(
            "/v1/evidence/{evidence_id}",
            get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::GetEvidence, move || {
                    let target = recovery::evidence_target(uri.path());
                    recovery::get_evidence(&evidence, bearer(&headers), target)
                })
            }),
        )
        .route(
            "/v1/checkpoints",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::CreateCheckpoint, move || {
                        recovery::create_checkpoint(&create_checkpoint, &request.borrow())
                    })
                },
            ),
        )
        .route(
            "/v1/checkpoints/{checkpoint_id}",
            get(move |headers: HeaderMap, uri: Uri| {
                blocking(OperationId::GetCheckpoint, move || {
                    let target = recovery::checkpoint_target(uri.path());
                    recovery::get_checkpoint(&checkpoint, bearer(&headers), target)
                })
            }),
        )
        .route(
            "/v1/decisions",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::ProposeDecision, move || {
                        recovery::propose_decision(&decision, &request.borrow())
                    })
                },
            ),
        )
        .route(
            "/v1/verification/ledger",
            post(
                move |headers: HeaderMap, body: Result<Bytes, BytesRejection>| {
                    let request = OwnedRequest::new(headers, body);
                    blocking(OperationId::VerifyLedger, move || {
                        events::verify_ledger(&verify_ledger, &request.borrow())
                    })
                },
            ),
        )
        .fallback(|| async { finish(None, &unknown_route()) })
        .method_not_allowed_fallback(|| async { finish(None, &unknown_route()) })
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
}

/// Capability identifiers of the routed operations.
#[must_use]
pub fn routed_capabilities() -> Vec<&'static str> {
    ROUTED_OPERATIONS
        .iter()
        .map(|operation| CapabilityId::for_operation(*operation).as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        auth::generate_secret,
        test_support::{Harness, field},
        tokens::{TokenRequest, create_client_token, revoke_client_token, revoke_target},
    };
    use memprove_protocol::strict_json::JsonValue;
    use memprove_store_sqlite::NewClientToken;

    fn problem_code(response: &WireResponse) -> String {
        let body = parse_strict(&response.body).unwrap();
        field(&body, "code")
    }

    fn assert_problem(response: &WireResponse, operation: OperationId, status: u16, code: &str) {
        assert_eq!(response.status, status, "{}", response.body);
        assert_eq!(problem_code(response), code);
        assert_eq!(response.bearer_challenge, status == 401);
        response.validate(Some(operation)).unwrap();
    }

    /// Calls every protected operation with `bearer` and returns
    /// `(operation, response)` pairs.
    fn protected_calls(
        harness: &Harness,
        bearer: Option<&str>,
    ) -> Vec<(OperationId, WireResponse)> {
        let target = "018f47a0-0000-7000-8000-000000000001";
        let body = r#"{"schema_version":1,"client_label":"x","scopes":["mcp-read"]}"#;
        vec![
            (
                OperationId::GetStatus,
                get_status(&harness.state.plane, bearer),
            ),
            (
                OperationId::GetVersion,
                get_version(&harness.state.plane, bearer),
            ),
            (
                OperationId::CreateClientToken,
                harness.create_with(bearer, Some(&harness.fresh_key()), body),
            ),
            (OperationId::ListClientTokens, harness.list(bearer, None)),
            (
                OperationId::RevokeClientToken,
                harness.revoke_with(bearer, Some(&harness.fresh_key()), target),
            ),
        ]
    }

    #[test]
    fn health_is_anonymous_and_follows_readiness() {
        let harness = Harness::new(true);
        let live = health_live();
        assert_eq!(live.status, 200);
        live.validate(Some(OperationId::HealthLive)).unwrap();
        let ready = health_ready(&harness.state.plane);
        assert_eq!(ready.status, 200);
        ready.validate(Some(OperationId::HealthReady)).unwrap();

        let not_ready = health_ready(&Harness::new(false).state.plane);
        assert_problem(
            &not_ready,
            OperationId::HealthReady,
            503,
            "service_not_ready",
        );
    }

    #[test]
    fn anonymous_requests_are_rejected_with_a_bearer_challenge() {
        let harness = Harness::new(true);
        harness.bootstrap();
        for (operation, response) in protected_calls(&harness, None) {
            assert_problem(&response, operation, 401, "authentication_required");
        }
    }

    #[test]
    fn unknown_and_malformed_tokens_are_rejected() {
        let harness = Harness::new(true);
        harness.bootstrap();
        let unknown = generate_secret().unwrap();
        for bearer in [unknown.expose(), "not-a-token", "", &"a".repeat(44)] {
            for (operation, response) in protected_calls(&harness, Some(bearer)) {
                assert_problem(&response, operation, 401, "authentication_required");
            }
        }
    }

    #[test]
    fn revoked_tokens_are_rejected_immediately() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let (reader_id, reader) = harness.issue(&control, "reader", &["mcp-read"]);
        assert_eq!(get_version(&harness.state.plane, Some(&reader)).status, 200);

        let revoked = harness.revoke(&control, &reader_id);
        assert_eq!(revoked.status, 200, "{}", revoked.body);
        revoked
            .validate(Some(OperationId::RevokeClientToken))
            .unwrap();
        assert!(revoked.body.contains(r#""status":"revoked""#));

        for (operation, response) in protected_calls(&harness, Some(&reader)) {
            assert_problem(&response, operation, 401, "authentication_required");
        }
    }

    #[test]
    fn insufficient_scope_is_forbidden_except_for_authenticated_operations() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let (_, limited) = harness.issue(&control, "limited", &["mcp-read", "ingest-only"]);
        for (operation, response) in protected_calls(&harness, Some(&limited)) {
            if operation == OperationId::GetVersion {
                assert_eq!(response.status, 200);
                response.validate(Some(operation)).unwrap();
            } else {
                assert_problem(&response, operation, 403, "policy_denied");
            }
        }
    }

    #[test]
    fn control_token_reaches_every_operation() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();

        let status = get_status(&harness.state.plane, Some(&control));
        assert_eq!(status.status, 200);
        status.validate(Some(OperationId::GetStatus)).unwrap();
        let body = parse_strict(&status.body).unwrap();
        assert_eq!(body.get("ready"), Some(&JsonValue::Bool(true)));
        assert_eq!(
            body.get("project_count").and_then(JsonValue::as_i64),
            Some(0)
        );
        assert_eq!(
            body.get("writer_queue_depth").and_then(JsonValue::as_i64),
            Some(0)
        );

        let version = get_version(&harness.state.plane, Some(&control));
        assert_eq!(version.status, 200);
        version.validate(Some(OperationId::GetVersion)).unwrap();
        assert!(version.body.contains(
            r#""capabilities":["healthLive","healthReady","getStatus","getVersion","createClientToken","listClientTokens","revokeClientToken","registerProject","getProject","resolveWorkspace","createSession","listSessions","endSession","createTask","listTasks","transitionTask","ingestEvent","listEvents","getEvent","captureEvidence","getEvidence","createCheckpoint","getCheckpoint","getRecoveryPreview","verifyLedger","getTaskCapsule","proposeDecision"]"#
        ));

        let created = harness.create(&control, "agent", &["mcp-write", "mcp-read"]);
        assert_eq!(created.status, 201);
        created
            .validate(Some(OperationId::CreateClientToken))
            .unwrap();
        let created_body = parse_strict(&created.body).unwrap();
        assert!(
            created
                .body
                .contains(r#""scopes":["mcp-read","mcp-write"]"#)
        );
        let secret = field(&created_body, "secret");
        assert_eq!(secret.len(), 43);
        assert_eq!(get_version(&harness.state.plane, Some(&secret)).status, 200);

        let list = harness.list(Some(&control), None);
        assert_eq!(list.status, 200);
        list.validate(Some(OperationId::ListClientTokens)).unwrap();
        let revoked = harness.revoke(&control, &field(&created_body, "token_id"));
        assert_eq!(revoked.status, 200);
    }

    #[test]
    fn the_last_control_token_cannot_be_revoked() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let control_id = harness.id_of(&control);
        let (_, reader) = harness.issue(&control, "reader", &["mcp-read"]);

        let refused = harness.revoke(&control, &control_id);
        assert_problem(
            &refused,
            OperationId::RevokeClientToken,
            409,
            "last_control_token",
        );
        assert_eq!(get_status(&harness.state.plane, Some(&control)).status, 200);
        // A non-control token cannot even attempt it.
        let forbidden = harness.revoke(&reader, &control_id);
        assert_problem(
            &forbidden,
            OperationId::RevokeClientToken,
            403,
            "policy_denied",
        );
    }

    #[test]
    fn self_revocation_succeeds_only_while_another_control_token_remains() {
        let harness = Harness::new(true);
        let first = harness.bootstrap();
        let first_id = harness.id_of(&first);
        let (second_id, second) = harness.issue(&first, "second-control", &["control"]);

        let key = harness.fresh_key();
        let revoked = harness.revoke_with(Some(&first), Some(&key), &first_id);
        assert_eq!(revoked.status, 200, "{}", revoked.body);
        // The revoked caller is rejected before any idempotency replay.
        let replay = harness.revoke_with(Some(&first), Some(&key), &first_id);
        assert_problem(
            &replay,
            OperationId::RevokeClientToken,
            401,
            "authentication_required",
        );

        let last = harness.revoke(&second, &second_id);
        assert_problem(
            &last,
            OperationId::RevokeClientToken,
            409,
            "last_control_token",
        );
    }

    #[test]
    fn create_is_idempotent_and_never_replays_the_secret() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let key = harness.fresh_key();
        let body =
            r#"{"schema_version":1,"client_label":"ci","scopes":["proxy-client","mcp-read"]}"#;
        let reordered =
            r#"{"scopes":["mcp-read","proxy-client"],"client_label":"ci","schema_version":1}"#;

        let first = harness.create_with(Some(&control), Some(&key), body);
        assert_eq!(first.status, 201);
        let first_body = parse_strict(&first.body).unwrap();
        let secret = field(&first_body, "secret");

        let replay = harness.create_with(Some(&control), Some(&key), reordered);
        assert_eq!(replay.status, 200, "{}", replay.body);
        replay
            .validate(Some(OperationId::CreateClientToken))
            .unwrap();
        let replay_body = parse_strict(&replay.body).unwrap();
        assert_eq!(
            field(&replay_body, "token_id"),
            field(&first_body, "token_id")
        );
        assert!(replay_body.get("secret").is_none());
        assert!(!replay.body.contains(&secret));
        assert!(!replay.body.contains(&crate::auth::verifier(&secret)));

        let conflict = harness.create_with(
            Some(&control),
            Some(&key),
            r#"{"schema_version":1,"client_label":"other","scopes":["mcp-read"]}"#,
        );
        assert_problem(
            &conflict,
            OperationId::CreateClientToken,
            409,
            "idempotency_conflict",
        );
    }

    #[test]
    fn revoke_is_idempotent_and_conflicts_on_a_different_target() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let (a, _) = harness.issue(&control, "a", &["mcp-read"]);
        let (b, _) = harness.issue(&control, "b", &["mcp-read"]);
        let key = harness.fresh_key();

        let first = harness.revoke_with(Some(&control), Some(&key), &a);
        let replay = harness.revoke_with(Some(&control), Some(&key), &a);
        assert_eq!(first.status, 200);
        assert_eq!(replay.status, 200);
        let first_body = parse_strict(&first.body).unwrap();
        let replay_body = parse_strict(&replay.body).unwrap();
        assert_eq!(first_body.get("revoked_at"), replay_body.get("revoked_at"));

        let again = harness.revoke(&control, &a);
        assert_eq!(again.status, 200);
        assert!(again.body.contains(r#""status":"revoked""#));

        let conflict = harness.revoke_with(Some(&control), Some(&key), &b);
        assert_problem(
            &conflict,
            OperationId::RevokeClientToken,
            409,
            "idempotency_conflict",
        );
    }

    #[test]
    fn malformed_requests_are_invalid() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let key = harness.fresh_key();
        let create = OperationId::CreateClientToken;
        let valid = r#"{"schema_version":1,"client_label":"x","scopes":["mcp-read"]}"#;

        assert_problem(
            &harness.create_with(Some(&control), None, valid),
            create,
            400,
            "invalid_request",
        );
        assert_problem(
            &harness.create_with(Some(&control), Some("too-short"), valid),
            create,
            400,
            "invalid_request",
        );
        for body in [
            r#"{"schema_version":1,"client_label":"x","scopes":["mcp-read"],"extra":1}"#,
            r#"{"schema_version":1,"client_label":"x","client_label":"y","scopes":["mcp-read"]}"#,
            r#"{"schema_version":2,"client_label":"x","scopes":["mcp-read"]}"#,
            r#"{"schema_version":1,"client_label":"","scopes":["mcp-read"]}"#,
            r#"{"schema_version":1,"client_label":"x","scopes":[]}"#,
            r#"{"schema_version":1,"client_label":"x","scopes":["mcp-read","mcp-read"]}"#,
            "not json",
        ] {
            assert_problem(
                &harness.create_with(Some(&control), Some(&key), body),
                create,
                400,
                "invalid_request",
            );
        }
        assert_problem(
            &harness.create_with(
                Some(&control),
                Some(&key),
                r#"{"schema_version":1,"client_label":"x","scopes":["admin"]}"#,
            ),
            create,
            400,
            "unsupported_scope",
        );
        let wrong_media = create_client_token(
            &harness.state,
            &TokenRequest {
                bearer: Some(&control),
                idempotency_key: Some(&key),
                content_type: Some("text/plain"),
                body: Some(valid.as_bytes()),
            },
        );
        assert_problem(&wrong_media, create, 400, "invalid_request");

        let revoke = OperationId::RevokeClientToken;
        assert_problem(
            &harness.revoke(&control, "not-a-uuid"),
            revoke,
            400,
            "invalid_request",
        );
        assert_problem(
            &harness.revoke_with(Some(&control), None, "018f47a0-0000-7000-8000-000000000001"),
            revoke,
            400,
            "invalid_request",
        );
        let with_body = revoke_client_token(
            &harness.state,
            &TokenRequest {
                bearer: Some(&control),
                idempotency_key: Some(&key),
                content_type: Some("application/json"),
                body: Some(b"{}"),
            },
            "018f47a0-0000-7000-8000-000000000001",
        );
        assert_problem(&with_body, revoke, 400, "invalid_request");
        assert_problem(
            &harness.list(Some(&control), Some("limit=0")),
            OperationId::ListClientTokens,
            400,
            "invalid_request",
        );
    }

    #[test]
    fn unknown_targets_are_resource_not_found() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let unknown = harness.revoke(&control, "018f47a0-0000-7000-8000-000000000001");
        assert_problem(
            &unknown,
            OperationId::RevokeClientToken,
            404,
            "resource_not_found",
        );
    }

    #[test]
    fn pending_bootstrap_tokens_do_not_authenticate_and_are_not_listed() {
        let harness = Harness::new(true);
        let secret = generate_secret().unwrap();
        let pending_id = uuid::Uuid::now_v7().to_string();
        let verifier = secret.verifier();
        harness
            .state
            .registry
            .lock()
            .unwrap()
            .begin_bootstrap_token(&NewClientToken {
                token_id: &pending_id,
                client_label: "pending",
                scopes: &["control"],
                verifier_sha256: &verifier,
                created_at: "2026-10-02T00:00:00Z",
            })
            .unwrap();
        for (operation, response) in protected_calls(&harness, Some(secret.expose())) {
            assert_problem(&response, operation, 401, "authentication_required");
        }
        let control = harness.bootstrap();
        let list = harness.list(Some(&control), None);
        assert!(!list.body.contains(&pending_id), "{}", list.body);
        let pending = harness.revoke(&control, &pending_id);
        assert_problem(
            &pending,
            OperationId::RevokeClientToken,
            404,
            "resource_not_found",
        );
    }

    #[test]
    fn list_pages_without_secrets_or_verifiers() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let mut secrets = vec![control.clone()];
        for label in ["a", "b", "c"] {
            secrets.push(harness.issue(&control, label, &["mcp-read"]).1);
        }
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let query = match &cursor {
                Some(cursor) => format!("limit=1&cursor={cursor}"),
                None => "limit=1".to_owned(),
            };
            let page = harness.list(Some(&control), Some(&query));
            assert_eq!(page.status, 200, "{}", page.body);
            page.validate(Some(OperationId::ListClientTokens)).unwrap();
            for secret in &secrets {
                assert!(!page.body.contains(secret.as_str()));
                assert!(!page.body.contains(&crate::auth::verifier(secret)));
            }
            let body = parse_strict(&page.body).unwrap();
            let items = body.get("items").and_then(JsonValue::as_array).unwrap();
            assert_eq!(items.len(), 1);
            seen.push(field(&items[0], "token_id"));
            match body.get("next_cursor").and_then(JsonValue::as_str) {
                Some(next) => cursor = Some(next.to_owned()),
                None => break,
            }
        }
        assert_eq!(seen.len(), 4);
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(seen, sorted);
    }

    #[test]
    fn readiness_is_checked_before_authentication() {
        let harness = Harness::new(false);
        let control = harness.bootstrap();
        for bearer in [None, Some(control.as_str())] {
            for (operation, response) in protected_calls(&harness, bearer) {
                assert_problem(&response, operation, 503, "service_not_ready");
            }
        }
    }

    #[test]
    fn an_unusable_registry_fails_closed() {
        let harness = Harness::new(true);
        let control = harness.bootstrap();
        let registry = Arc::clone(&harness.state.registry);
        let poisoned = std::thread::spawn(move || {
            let _guard = registry.lock().unwrap();
            panic!("poison the registry lock");
        })
        .join();
        assert!(poisoned.is_err());
        for (operation, response) in protected_calls(&harness, Some(&control)) {
            assert_problem(&response, operation, 503, "service_not_ready");
        }
    }

    #[test]
    fn unknown_route_is_a_contract_problem() {
        let response = unknown_route();
        assert_eq!(response.status, 404);
        assert_eq!(problem_code(&response), "resource_not_found");
        response.validate(None).unwrap();
        assert_eq!(revoke_target("/v1/auth/tokens/x/revoke"), "x");
    }

    #[test]
    fn invalid_bodies_are_caught_by_validation() {
        let bogus = WireResponse::json(200, &json!({ "status": "degraded" }));
        assert!(bogus.validate(Some(OperationId::HealthLive)).is_err());
        let wrong_status = WireResponse::json(201, &json!({ "status": "ok" }));
        assert!(
            wrong_status
                .validate(Some(OperationId::HealthLive))
                .is_err()
        );
        let foreign_problem = WireResponse::problem(ProblemCode::PolicyDenied);
        assert!(
            foreign_problem
                .validate(Some(OperationId::GetVersion))
                .is_err()
        );
        let finished = finish(Some(OperationId::HealthLive), &bogus);
        assert_eq!(finished.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn bearer_extraction() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer(&headers), None);
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc"),
        );
        assert_eq!(bearer(&headers), Some("abc"));
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert_eq!(bearer(&headers), None);
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer "));
        assert_eq!(bearer(&headers), None);
    }
}
