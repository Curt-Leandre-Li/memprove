use super::types::{CapabilityId, OperationId, ProblemCode, SchemaId};

/// State of an operation-to-capability-to-AccessRule mapping at decision time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingState {
    /// The mapping is complete and approved by the applicable authority.
    CompleteAndApproved,
    /// The mapping is absent, incomplete, or not approved; access must fail closed.
    MissingOrUnapproved,
}

/// Result of applying the default-deny access projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessDecision {
    /// The approved mapping exists and the request satisfies its rule.
    Allowed,
    /// The mapping or request does not satisfy the frozen rule.
    Denied,
}

/// `AccessRule` discriminator copied from the frozen `OpenAPI` extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRuleKind {
    /// No credential is required after mapping approval.
    Anonymous,
    /// Any authenticated client token is sufficient.
    Authenticated,
    /// At least one listed scope is required.
    AnyOf,
}

impl AccessRuleKind {
    /// Returns the exact `OpenAPI` extension spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anonymous => "Anonymous",
            Self::Authenticated => "Authenticated",
            Self::AnyOf => "AnyOf",
        }
    }
}

/// Frozen access projection for one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessRule {
    /// `OpenAPI` `x-memprove-access.kind`.
    pub kind: AccessRuleKind,
    /// `OpenAPI` `x-memprove-access.scopes`.
    pub scopes: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SuccessBinding {
    pub status: u16,
    pub media_type: &'static str,
    pub schema: SchemaId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProblemBinding {
    pub status: u16,
    pub code: ProblemCode,
}

/// Runtime-free metadata binding one operation to its wire schemas and policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationBinding {
    /// Frozen `OpenAPI` operation identifier.
    pub operation_id: OperationId,
    /// Capability identity paired one-to-one with `operation_id`.
    pub capability_id: CapabilityId,
    /// Uppercase HTTP method.
    pub method: &'static str,
    /// Frozen route template. This is metadata and does not register a route.
    pub path: &'static str,
    /// Frozen operation `AccessRule`.
    pub access_rule: AccessRule,
    pub(crate) request_schema: Option<SchemaId>,
    pub(crate) successes: &'static [SuccessBinding],
    pub(crate) problems: &'static [ProblemBinding],
}

impl OperationBinding {
    /// Applies mapping approval and rule satisfaction with default-deny behavior.
    #[must_use]
    pub const fn authorize(
        self,
        mapping_state: MappingState,
        request_satisfies_rule: bool,
    ) -> AccessDecision {
        if matches!(mapping_state, MappingState::CompleteAndApproved) && request_satisfies_rule {
            AccessDecision::Allowed
        } else {
            AccessDecision::Denied
        }
    }

    /// Reports whether this operation requires an `application/json` body.
    #[must_use]
    pub const fn request_body_required(self) -> bool {
        self.request_schema.is_some()
    }

    /// Iterates the exact successful status/media pairs accepted by the binding.
    pub fn success_contracts(self) -> impl Iterator<Item = (u16, &'static str)> {
        self.successes
            .iter()
            .map(|success| (success.status, success.media_type))
    }

    /// Iterates the exact operation-local Problem status/code pairs.
    pub fn problem_contracts(self) -> impl Iterator<Item = (u16, ProblemCode)> {
        self.problems
            .iter()
            .map(|problem| (problem.status, problem.code))
    }
}

const CONTROL: &[&str] = &["control"];
const CONTROL_OR_MCP_READ: &[&str] = &["control", "mcp-read"];
const CONTROL_OR_MCP_WRITE: &[&str] = &["control", "mcp-write"];
const NO_SCOPES: &[&str] = &[];

const ANONYMOUS: AccessRule = AccessRule {
    kind: AccessRuleKind::Anonymous,
    scopes: NO_SCOPES,
};
const AUTHENTICATED: AccessRule = AccessRule {
    kind: AccessRuleKind::Authenticated,
    scopes: NO_SCOPES,
};
const CONTROL_ACCESS: AccessRule = AccessRule {
    kind: AccessRuleKind::AnyOf,
    scopes: CONTROL,
};
/// ADR 0012: agent reads of Task, Session, and Project metadata.
const READ_ACCESS: AccessRule = AccessRule {
    kind: AccessRuleKind::AnyOf,
    scopes: CONTROL_OR_MCP_READ,
};
/// ADR 0012: agent writes.
const WRITE_ACCESS: AccessRule = AccessRule {
    kind: AccessRuleKind::AnyOf,
    scopes: CONTROL_OR_MCP_WRITE,
};

const fn success(status: u16, media_type: &'static str, schema: SchemaId) -> SuccessBinding {
    SuccessBinding {
        status,
        media_type,
        schema,
    }
}

const fn problem(status: u16, code: ProblemCode) -> ProblemBinding {
    ProblemBinding { status, code }
}

const HEALTH_SUCCESS: &[SuccessBinding] = &[success(200, "application/json", SchemaId::Health)];
const STATUS_SUCCESS: &[SuccessBinding] = &[success(200, "application/json", SchemaId::Status)];
const VERSION_SUCCESS: &[SuccessBinding] = &[success(200, "application/json", SchemaId::Version)];
const CREATE_TOKEN_SUCCESS: &[SuccessBinding] = &[
    success(200, "application/json", SchemaId::TokenSummary),
    success(201, "application/json", SchemaId::TokenCreated),
];
const TOKEN_PAGE_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::TokenSummaryPage)];
const TOKEN_SUMMARY_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::TokenSummary)];
const PROJECT_CREATED_SUCCESS: &[SuccessBinding] =
    &[success(201, "application/json", SchemaId::Project)];
const PROJECT_SUCCESS: &[SuccessBinding] = &[success(200, "application/json", SchemaId::Project)];
const WORKSPACE_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::Workspace)];
const SESSION_CREATED_SUCCESS: &[SuccessBinding] =
    &[success(201, "application/json", SchemaId::Session)];
const SESSION_PAGE_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::SessionPage)];
const SESSION_SUCCESS: &[SuccessBinding] = &[success(200, "application/json", SchemaId::Session)];
const ACCEPTED_SUCCESS: &[SuccessBinding] = &[success(202, "application/json", SchemaId::Accepted)];
const EVENT_PAGE_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::EventPage)];
const EVENT_STREAM_SUCCESS: &[SuccessBinding] =
    &[success(200, "text/event-stream", SchemaId::EventStream)];
const EVENT_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::EventEnvelope)];
const TASK_CREATED_SUCCESS: &[SuccessBinding] = &[success(201, "application/json", SchemaId::Task)];
const TASK_PAGE_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::TaskPage)];
const TASK_SUCCESS: &[SuccessBinding] = &[success(200, "application/json", SchemaId::Task)];
const RESOURCE_CREATED_SUCCESS: &[SuccessBinding] =
    &[success(201, "application/json", SchemaId::Resource)];
const RESOURCE_SUCCESS: &[SuccessBinding] = &[success(200, "application/json", SchemaId::Resource)];
const RECOVERY_PREVIEW_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::RecoveryPreview)];
const TASK_CAPSULE_SUCCESS: &[SuccessBinding] =
    &[success(200, "application/json", SchemaId::TaskCapsule)];
const LEDGER_VERIFICATION_SUCCESS: &[SuccessBinding] = &[success(
    200,
    "application/json",
    SchemaId::LedgerVerification,
)];

const NO_PROBLEMS: &[ProblemBinding] = &[];
const READY_PROBLEMS: &[ProblemBinding] = &[problem(503, ProblemCode::ServiceNotReady)];
const STATUS_PROBLEMS: &[ProblemBinding] = &[
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(503, ProblemCode::ServiceNotReady),
];
const VERSION_PROBLEMS: &[ProblemBinding] = &[
    problem(401, ProblemCode::AuthenticationRequired),
    problem(503, ProblemCode::ServiceNotReady),
];
const CREATE_TOKEN_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(400, ProblemCode::UnsupportedScope),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(503, ProblemCode::ServiceNotReady),
];
const LIST_TOKEN_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(503, ProblemCode::ServiceNotReady),
];
const REVOKE_TOKEN_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::LastControlToken),
    problem(503, ProblemCode::ServiceNotReady),
];
const REGISTER_PROJECT_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1 (Owner, T20 SF-1/SF-2) adds `409 capsule_tampered` to every
/// operation that reads a stored record which can be missing or not decode:
/// no retry cures it, so it is not `503 service_not_ready`. Here: the root
/// Workspace row.
const GET_PROJECT_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1: the root Workspace row (`409 capsule_tampered`).
const RESOLVE_WORKSPACE_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
/// ADR 0012 adds `404 resource_not_found` (unknown `task_id`) and
/// `409 project_workspace_mismatch` (Task scoped to another Workspace).
const CREATE_SESSION_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    // P4-D (Owner A1): a Task that is completed or cancelled.
    problem(409, ProblemCode::InvalidTransition),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1: a Session row of the page that does not decode
/// (`409 capsule_tampered`).
const LIST_SESSION_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
const END_SESSION_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::InvalidTransition),
    problem(409, ProblemCode::RevisionConflict),
    problem(503, ProblemCode::ServiceNotReady),
];
/// ADR 0010 Q3 adds `404 resource_not_found` (unknown Session) and T7 adds
/// `409 invalid_transition` (Session not active) to `ingestEvent` only; the
/// deferred batch keeps the frozen set.
const INGEST_EVENT_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(409, ProblemCode::InvalidTransition),
    problem(503, ProblemCode::ServiceNotReady),
];
const INGEST_EVENT_BATCH_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1: an Event row of the page that does not decode or recompute
/// (`409 capsule_tampered`). The deferred `streamEvents` keeps the frozen
/// set.
const LIST_EVENT_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
const STREAM_EVENT_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1: the Event row does not decode or recompute
/// (`409 capsule_tampered`).
const GET_EVENT_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1: the root Workspace row (`409 capsule_tampered`).
const CREATE_TASK_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1: a Task row of the page that does not decode
/// (`409 capsule_tampered`); the root Workspace is not read.
const LIST_TASK_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
const NEXT_TASK_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ResourceNotFound),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P2-C adds the Recovery Preview's Workspace refusals for the optional
/// `workspace_id` of a completion; U9 R1 (ADR 0020) `403 role_forbidden`
/// for a completion whose `session_id` names a Session that declared the
/// role `sub_agent`, whatever runs it cites (S-F round 1, SF1-A).
const TRANSITION_TASK_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(403, ProblemCode::RoleForbidden),
    problem(404, ProblemCode::ResourceNotFound),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(409, ProblemCode::EvidenceStale),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::InvalidTransition),
    problem(409, ProblemCode::RevisionConflict),
    problem(503, ProblemCode::ServiceNotReady),
];
const DEPENDENCY_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::DependencyCycle),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::RevisionConflict),
    problem(503, ProblemCode::ServiceNotReady),
];

/// ADR 0012 (P2-A): the Problem sets of the four routed Phase 2 operations,
/// which the frozen contract left unannotated.
const CAPTURE_EVIDENCE_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(409, ProblemCode::InvalidTransition),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P4-H1: the record, or one it is read with, does not decode or recompute
/// (`409 capsule_tampered`).
const GET_RECOVERY_RECORD_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
/// The Problems of `captureEvidence`, and U9 R1 (ADR 0020) `403
/// role_forbidden` for an edge a `sub_agent` Session may not declare.
const CREATE_CHECKPOINT_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(403, ProblemCode::RoleForbidden),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(409, ProblemCode::InvalidTransition),
    problem(503, ProblemCode::ServiceNotReady),
];
/// ADR 0012 D6 (P2-B): an unknown Task, an unknown `workspace_id`, or a
/// `workspace_id` of another Project. P4-H1: `409 capsule_tampered` when a
/// stored value the view reads does not decode where the view cannot name
/// it as a damaged record (none is known; the documents report damage as
/// `tampered`).
const RECOVERY_PREVIEW_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ResourceNotFound),
    problem(404, ProblemCode::WorkspaceNotResolved),
    problem(409, ProblemCode::ProjectWorkspaceMismatch),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];
/// ADR 0013 D4 (P3-B): the Problems of the Recovery Preview.
const TASK_CAPSULE_PROBLEMS: &[ProblemBinding] = RECOVERY_PREVIEW_PROBLEMS;
/// ADR 0013 D2 (P3-C): an unknown Task or Session, a reused key, or a
/// Session that is not active; U9 R1 (ADR 0020): a `sub_agent` Session.
const PROPOSE_DECISION_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(403, ProblemCode::RoleForbidden),
    problem(404, ProblemCode::ResourceNotFound),
    problem(409, ProblemCode::IdempotencyConflict),
    problem(409, ProblemCode::InvalidTransition),
    problem(503, ProblemCode::ServiceNotReady),
];
/// P2-C: an unknown Project. P4-H1 (review of the first P4-H1 commit): a
/// state database that is not bound to the Project (`409 capsule_tampered`);
/// a row of it that is missing or does not decode is a `first_failure`.
const VERIFY_LEDGER_PROBLEMS: &[ProblemBinding] = &[
    problem(400, ProblemCode::InvalidRequest),
    problem(401, ProblemCode::AuthenticationRequired),
    problem(403, ProblemCode::PolicyDenied),
    problem(404, ProblemCode::ProjectNotFound),
    problem(409, ProblemCode::CapsuleTampered),
    problem(503, ProblemCode::ServiceNotReady),
];

macro_rules! binding {
    ($id:ident, $method:literal, $path:literal, $request:expr, $success:ident, $access:ident, $problems:ident) => {
        OperationBinding {
            operation_id: OperationId::$id,
            capability_id: CapabilityId::for_operation(OperationId::$id),
            method: $method,
            path: $path,
            access_rule: $access,
            request_schema: $request,
            successes: $success,
            problems: $problems,
        }
    };
}

const BINDINGS: [OperationBinding; 31] = [
    binding!(
        HealthLive,
        "GET",
        "/health/live",
        None,
        HEALTH_SUCCESS,
        ANONYMOUS,
        NO_PROBLEMS
    ),
    binding!(
        HealthReady,
        "GET",
        "/health/ready",
        None,
        HEALTH_SUCCESS,
        ANONYMOUS,
        READY_PROBLEMS
    ),
    binding!(
        GetStatus,
        "GET",
        "/v1/status",
        None,
        STATUS_SUCCESS,
        CONTROL_ACCESS,
        STATUS_PROBLEMS
    ),
    binding!(
        GetVersion,
        "GET",
        "/v1/version",
        None,
        VERSION_SUCCESS,
        AUTHENTICATED,
        VERSION_PROBLEMS
    ),
    binding!(
        CreateClientToken,
        "POST",
        "/v1/auth/tokens",
        Some(SchemaId::CreateClientTokenRequest),
        CREATE_TOKEN_SUCCESS,
        CONTROL_ACCESS,
        CREATE_TOKEN_PROBLEMS
    ),
    binding!(
        ListClientTokens,
        "GET",
        "/v1/auth/tokens",
        None,
        TOKEN_PAGE_SUCCESS,
        CONTROL_ACCESS,
        LIST_TOKEN_PROBLEMS
    ),
    binding!(
        RevokeClientToken,
        "POST",
        "/v1/auth/tokens/{token_id}/revoke",
        None,
        TOKEN_SUMMARY_SUCCESS,
        CONTROL_ACCESS,
        REVOKE_TOKEN_PROBLEMS
    ),
    binding!(
        RegisterProject,
        "POST",
        "/v1/projects/register",
        Some(SchemaId::RegisterProjectRequest),
        PROJECT_CREATED_SUCCESS,
        CONTROL_ACCESS,
        REGISTER_PROJECT_PROBLEMS
    ),
    binding!(
        GetProject,
        "GET",
        "/v1/projects/{project_id}",
        None,
        PROJECT_SUCCESS,
        READ_ACCESS,
        GET_PROJECT_PROBLEMS
    ),
    binding!(
        ResolveWorkspace,
        "POST",
        "/v1/workspaces/resolve",
        Some(SchemaId::ResolveWorkspaceRequest),
        WORKSPACE_SUCCESS,
        WRITE_ACCESS,
        RESOLVE_WORKSPACE_PROBLEMS
    ),
    binding!(
        CreateSession,
        "POST",
        "/v1/sessions",
        Some(SchemaId::CreateSessionRequest),
        SESSION_CREATED_SUCCESS,
        WRITE_ACCESS,
        CREATE_SESSION_PROBLEMS
    ),
    binding!(
        ListSessions,
        "GET",
        "/v1/sessions",
        None,
        SESSION_PAGE_SUCCESS,
        READ_ACCESS,
        LIST_SESSION_PROBLEMS
    ),
    binding!(
        EndSession,
        "POST",
        "/v1/sessions/{session_id}/end",
        Some(SchemaId::ExpectedRevisionCommand),
        SESSION_SUCCESS,
        WRITE_ACCESS,
        END_SESSION_PROBLEMS
    ),
    binding!(
        IngestEvent,
        "POST",
        "/v1/events",
        Some(SchemaId::EventEnvelope),
        ACCEPTED_SUCCESS,
        WRITE_ACCESS,
        INGEST_EVENT_PROBLEMS
    ),
    binding!(
        ListEvents,
        "GET",
        "/v1/events",
        None,
        EVENT_PAGE_SUCCESS,
        CONTROL_ACCESS,
        LIST_EVENT_PROBLEMS
    ),
    binding!(
        IngestEventBatch,
        "POST",
        "/v1/events/batch",
        Some(SchemaId::EventBatchRequest),
        ACCEPTED_SUCCESS,
        CONTROL_ACCESS,
        INGEST_EVENT_BATCH_PROBLEMS
    ),
    binding!(
        StreamEvents,
        "GET",
        "/v1/events/stream",
        None,
        EVENT_STREAM_SUCCESS,
        CONTROL_ACCESS,
        STREAM_EVENT_PROBLEMS
    ),
    binding!(
        GetEvent,
        "GET",
        "/v1/events/{event_id}",
        None,
        EVENT_SUCCESS,
        CONTROL_ACCESS,
        GET_EVENT_PROBLEMS
    ),
    binding!(
        CreateTask,
        "POST",
        "/v1/tasks",
        Some(SchemaId::CreateTaskRequest),
        TASK_CREATED_SUCCESS,
        WRITE_ACCESS,
        CREATE_TASK_PROBLEMS
    ),
    binding!(
        ListTasks,
        "GET",
        "/v1/tasks",
        None,
        TASK_PAGE_SUCCESS,
        READ_ACCESS,
        LIST_TASK_PROBLEMS
    ),
    binding!(
        GetNextTask,
        "GET",
        "/v1/tasks/next",
        None,
        TASK_SUCCESS,
        CONTROL_ACCESS,
        NEXT_TASK_PROBLEMS
    ),
    binding!(
        TransitionTask,
        "POST",
        "/v1/tasks/{task_id}/transition",
        Some(SchemaId::TaskTransitionRequest),
        TASK_SUCCESS,
        WRITE_ACCESS,
        TRANSITION_TASK_PROBLEMS
    ),
    binding!(
        AddTaskDependency,
        "POST",
        "/v1/tasks/{task_id}/dependencies",
        Some(SchemaId::TaskDependencyRequest),
        TASK_SUCCESS,
        CONTROL_ACCESS,
        DEPENDENCY_PROBLEMS
    ),
    binding!(
        CaptureEvidence,
        "POST",
        "/v1/evidence/capture",
        Some(SchemaId::CaptureEvidenceRequest),
        RESOURCE_CREATED_SUCCESS,
        WRITE_ACCESS,
        CAPTURE_EVIDENCE_PROBLEMS
    ),
    binding!(
        GetEvidence,
        "GET",
        "/v1/evidence/{evidence_id}",
        None,
        RESOURCE_SUCCESS,
        CONTROL_ACCESS,
        GET_RECOVERY_RECORD_PROBLEMS
    ),
    binding!(
        CreateCheckpoint,
        "POST",
        "/v1/checkpoints",
        Some(SchemaId::CreateCheckpointRequest),
        RESOURCE_CREATED_SUCCESS,
        WRITE_ACCESS,
        CREATE_CHECKPOINT_PROBLEMS
    ),
    binding!(
        GetCheckpoint,
        "GET",
        "/v1/checkpoints/{checkpoint_id}",
        None,
        RESOURCE_SUCCESS,
        READ_ACCESS,
        GET_RECOVERY_RECORD_PROBLEMS
    ),
    binding!(
        GetRecoveryPreview,
        "GET",
        "/v1/tasks/{task_id}/recovery-preview",
        None,
        RECOVERY_PREVIEW_SUCCESS,
        READ_ACCESS,
        RECOVERY_PREVIEW_PROBLEMS
    ),
    binding!(
        VerifyLedger,
        "POST",
        "/v1/verification/ledger",
        Some(SchemaId::ProjectCommand),
        LEDGER_VERIFICATION_SUCCESS,
        READ_ACCESS,
        VERIFY_LEDGER_PROBLEMS
    ),
    binding!(
        GetTaskCapsule,
        "GET",
        "/v1/tasks/{task_id}/recovery-capsule",
        None,
        TASK_CAPSULE_SUCCESS,
        READ_ACCESS,
        TASK_CAPSULE_PROBLEMS
    ),
    binding!(
        ProposeDecision,
        "POST",
        "/v1/decisions",
        Some(SchemaId::DecisionProposal),
        RESOURCE_CREATED_SUCCESS,
        WRITE_ACCESS,
        PROPOSE_DECISION_PROBLEMS
    ),
];

/// Number of frozen Phase 1 bindings; the Phase 2 bindings follow them.
pub const PHASE1_BINDING_COUNT: usize = 23;

/// Returns the 23 frozen Phase 1 bindings followed by the four Phase 2
/// bindings routed by ADR 0012 (P2-A), `getRecoveryPreview` (D6, P2-B),
/// `verifyLedger` (P2-C), `getTaskCapsule` (ADR 0013 D4, P3-B), and
/// `proposeDecision` (ADR 0013 D2, P3-C).
#[must_use]
pub const fn all_bindings() -> &'static [OperationBinding; 31] {
    &BINDINGS
}

/// Looks up a frozen Phase 1 binding by exact `operationId`.
#[must_use]
pub fn binding_for(operation_id: &str) -> Option<&'static OperationBinding> {
    let operation_id = operation_id.parse::<OperationId>().ok()?;
    BINDINGS
        .iter()
        .find(|binding| binding.operation_id == operation_id)
}

pub(crate) fn binding_for_id(operation_id: OperationId) -> &'static OperationBinding {
    &BINDINGS[operation_id_index(operation_id)]
}

const fn operation_id_index(operation_id: OperationId) -> usize {
    operation_id as usize
}
