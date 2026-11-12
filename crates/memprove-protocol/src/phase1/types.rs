use std::{fmt, str::FromStr};

use crate::strict_json::JsonValue;

/// Error returned when a frozen wire enum does not contain a supplied value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseEnumError {
    enum_name: &'static str,
    value: String,
}

impl ParseEnumError {
    pub(crate) fn new(enum_name: &'static str, value: &str) -> Self {
        Self {
            enum_name,
            value: value.to_owned(),
        }
    }
}

impl fmt::Display for ParseEnumError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} does not contain variant {:?}",
            self.enum_name, self.value
        )
    }
}

impl std::error::Error for ParseEnumError {}

macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($variant:ident => $wire:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name {
            $(
                #[doc = concat!("Wire value `", $wire, "`.")]
                $variant,
            )+
        }

        impl $name {
            /// Every frozen variant, in contract order.
            pub const ALL: [Self; wire_enum!(@count $($variant),+)] = [$(Self::$variant),+];

            /// Returns the exact wire spelling.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(ParseEnumError::new(stringify!($name), value)),
                }
            }
        }
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(wire_enum!(@unit $variant)),+])
    };
    (@unit $variant:ident) => { () };
}

wire_enum! {
    /// The 23 frozen Phase 1 `operationId` values, then the four Phase 2
    /// operations routed by ADR 0012 (P2-A), `getRecoveryPreview`
    /// (ADR 0012 D6, P2-B), `verifyLedger` (P2-C), `getTaskCapsule`
    /// (ADR 0013 D4, P3-B), and `proposeDecision` (ADR 0013 D2, P3-C).
    pub enum OperationId {
        HealthLive => "healthLive",
        HealthReady => "healthReady",
        GetStatus => "getStatus",
        GetVersion => "getVersion",
        CreateClientToken => "createClientToken",
        ListClientTokens => "listClientTokens",
        RevokeClientToken => "revokeClientToken",
        RegisterProject => "registerProject",
        GetProject => "getProject",
        ResolveWorkspace => "resolveWorkspace",
        CreateSession => "createSession",
        ListSessions => "listSessions",
        EndSession => "endSession",
        IngestEvent => "ingestEvent",
        ListEvents => "listEvents",
        IngestEventBatch => "ingestEventBatch",
        StreamEvents => "streamEvents",
        GetEvent => "getEvent",
        CreateTask => "createTask",
        ListTasks => "listTasks",
        GetNextTask => "getNextTask",
        TransitionTask => "transitionTask",
        AddTaskDependency => "addTaskDependency",
        CaptureEvidence => "captureEvidence",
        GetEvidence => "getEvidence",
        CreateCheckpoint => "createCheckpoint",
        GetCheckpoint => "getCheckpoint",
        GetRecoveryPreview => "getRecoveryPreview",
        VerifyLedger => "verifyLedger",
        GetTaskCapsule => "getTaskCapsule",
        ProposeDecision => "proposeDecision"
    }
}

/// A capability identity bound one-to-one to a Phase 1 operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityId(OperationId);

impl CapabilityId {
    /// Creates the exact capability paired with an operation.
    #[must_use]
    pub const fn for_operation(operation_id: OperationId) -> Self {
        Self(operation_id)
    }

    /// Returns the frozen capability identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0.as_str()
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

wire_enum! {
    /// Canonical Local-First V1 task lifecycle.
    pub enum TaskState {
        Proposed => "proposed",
        Ready => "ready",
        InProgress => "in_progress",
        Blocked => "blocked",
        Completed => "completed",
        Cancelled => "cancelled"
    }
}

wire_enum! {
    /// Frozen client-token scopes.
    pub enum ClientScope {
        Control => "control",
        IngestOnly => "ingest-only",
        McpRead => "mcp-read",
        McpWrite => "mcp-write",
        ProxyClient => "proxy-client"
    }
}

wire_enum! {
    /// Frozen token lifecycle values.
    pub enum TokenStatus {
        Active => "active",
        Revoked => "revoked"
    }
}

wire_enum! {
    /// Frozen project kinds.
    pub enum ProjectKind {
        Git => "git",
        Directory => "directory"
    }
}

wire_enum! {
    /// Frozen session lifecycle values.
    pub enum SessionState {
        Active => "active",
        Ended => "ended",
        Crashed => "crashed"
    }
}

wire_enum! {
    /// Event privacy classification.
    pub enum PrivacyClass {
        Public => "public",
        Internal => "internal",
        Private => "private",
        Secret => "secret"
    }
}

wire_enum! {
    /// Frozen Phase 1 event kinds, then the four `memproved` records of
    /// Task and Workspace changes (ADR 0012 D7, P2-C), then `memproved`'s
    /// edges between claims (ADR 0013 D1, P3-C).
    pub enum EventType {
        SessionStarted => "session.started",
        SessionEnded => "session.ended",
        SessionCrashed => "session.crashed",
        TurnStarted => "turn.started",
        TurnEnded => "turn.ended",
        ModelRequested => "model.requested",
        ModelResponded => "model.responded",
        ModelFailed => "model.failed",
        ToolStarted => "tool.started",
        ToolCompleted => "tool.completed",
        ToolFailed => "tool.failed",
        CommandStarted => "command.started",
        CommandCompleted => "command.completed",
        FileObserved => "file.observed",
        FileWritten => "file.written",
        FileDeleted => "file.deleted",
        GitHeadChanged => "git.head.changed",
        GitStatusObserved => "git.status.observed",
        GitDiffCaptured => "git.diff.captured",
        GitCommitCreated => "git.commit.created",
        DecisionRecorded => "decision.recorded",
        CheckpointCreated => "checkpoint.created",
        ContextAssembled => "context.assembled",
        ContextInjected => "context.injected",
        CapsuleCreated => "capsule.created",
        CapsuleVerified => "capsule.verified",
        CapsuleRecovered => "capsule.recovered",
        CaptureRedacted => "capture.redacted",
        CaptureDropped => "capture.dropped",
        ProxyRouteSelected => "proxy.route.selected",
        TaskCreated => "task.created",
        TaskTransitioned => "task.transitioned",
        TaskSessionLinked => "task.session_linked",
        WorkspaceRegistered => "workspace.registered",
        ClaimSuperseded => "claim.superseded"
    }
}

wire_enum! {
    /// Stable machine-readable problem codes.
    pub enum ProblemCode {
        InvalidRequest => "invalid_request",
        UnsupportedScope => "unsupported_scope",
        AuthenticationRequired => "authentication_required",
        PolicyDenied => "policy_denied",
        ProjectNotFound => "project_not_found",
        WorkspaceNotResolved => "workspace_not_resolved",
        ProjectWorkspaceMismatch => "project_workspace_mismatch",
        ResourceNotFound => "resource_not_found",
        RevisionConflict => "revision_conflict",
        InvalidTransition => "invalid_transition",
        DependencyCycle => "dependency_cycle",
        EvidenceStale => "evidence_stale",
        CapsuleStale => "capsule_stale",
        CapsuleTampered => "capsule_tampered",
        ContextUnavailable => "context_unavailable",
        CommandTimeout => "command_timeout",
        StoreCorrupt => "store_corrupt",
        IdempotencyMismatch => "idempotency_mismatch",
        IdempotencyConflict => "idempotency_conflict",
        LastControlToken => "last_control_token",
        ServiceNotReady => "service_not_ready",
        ProviderUnavailable => "provider_unavailable",
        // U9 R1 (Owner O4; ADR 0020): a Session that declared the role
        // `sub_agent` wrote what that role may not write.
        RoleForbidden => "role_forbidden"
    }
}

impl ProblemCode {
    /// Returns the only legal HTTP status for this frozen problem code.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::InvalidRequest | Self::UnsupportedScope => 400,
            Self::AuthenticationRequired => 401,
            Self::PolicyDenied | Self::RoleForbidden => 403,
            Self::ProjectNotFound | Self::WorkspaceNotResolved | Self::ResourceNotFound => 404,
            Self::ProjectWorkspaceMismatch
            | Self::RevisionConflict
            | Self::InvalidTransition
            | Self::DependencyCycle
            | Self::EvidenceStale
            | Self::CapsuleStale
            | Self::CapsuleTampered
            | Self::IdempotencyMismatch
            | Self::IdempotencyConflict
            | Self::LastControlToken => 409,
            Self::StoreCorrupt => 500,
            Self::ContextUnavailable | Self::ServiceNotReady | Self::ProviderUnavailable => 503,
            Self::CommandTimeout => 504,
        }
    }
}

/// Identifies one validation schema behind a typed binding.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaId {
    NoBody,
    Health,
    Status,
    Version,
    CreateClientTokenRequest,
    TokenCreated,
    TokenSummary,
    TokenSummaryPage,
    RegisterProjectRequest,
    Project,
    ResolveWorkspaceRequest,
    Workspace,
    CreateSessionRequest,
    Session,
    SessionPage,
    ExpectedRevisionCommand,
    EventEnvelope,
    EventBatchRequest,
    Accepted,
    EventPage,
    EventStream,
    CreateTaskRequest,
    Task,
    TaskPage,
    TaskTransitionRequest,
    TaskDependencyRequest,
    CreateClientTokenSuccess,
    Problem,
    CaptureEvidenceRequest,
    CreateCheckpointRequest,
    Resource,
    RecoveryPreview,
    ProjectCommand,
    LedgerVerification,
    TaskCapsule,
    DecisionProposal,
}

/// Associates a Rust wire type with its frozen validation schema.
pub trait WireType {
    /// Schema used by the binding validator.
    const SCHEMA: SchemaId;
}

macro_rules! wire_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident $body:tt => $schema:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        $vis struct $name $body

        impl WireType for $name {
            const SCHEMA: SchemaId = SchemaId::$schema;
        }
    };
}

/// A validated `UUIDv7` string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UuidV7(pub String);

/// An opaque non-empty pagination cursor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor(pub String);

/// An exact JSON number token whose mathematical value is an integer.
///
/// The frozen schemas do not impose an `i64` or `u64` maximum, so the lexical
/// representation is retained without narrowing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JsonInteger(pub String);

/// A positive optimistic-concurrency revision represented without narrowing.
///
/// Validation at the wire ingress establishes the positive-integer invariant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExpectedRevision(pub JsonInteger);

/// An RFC 3339 timestamp. Numeric offsets and `Z`/`z` are allowed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rfc3339DateTime(pub String);

/// An RFC 3339 UTC timestamp carried on the wire.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rfc3339Utc(pub String);

wire_struct! {
    /// Marker for operations whose HTTP request has no body.
    pub struct NoBody; => NoBody
}

wire_struct! {
    /// Process health response.
    pub struct Health {
        /// Frozen health marker; only `ok` is valid.
        pub status: String,
    } => Health
}

wire_struct! {
    /// Local daemon status projection.
    pub struct DaemonStatus {
        pub schema_version: u8,
        pub ready: bool,
        pub writer_queue_depth: JsonInteger,
        pub project_count: JsonInteger,
    } => Status
}

wire_struct! {
    /// Local application and API version projection.
    pub struct VersionInfo {
        pub schema_version: u8,
        pub application_version: String,
        pub api_major: u8,
        pub capabilities: Vec<String>,
        /// U1 (E1, Owner O8): `<12 hex>[-dirty]` or `unknown`; optional.
        pub build: Option<String>,
    } => Version
}

wire_struct! {
    /// Request to create an offline client token.
    pub struct CreateClientTokenRequest {
        pub schema_version: u8,
        pub client_label: String,
        pub scopes: Vec<ClientScope>,
    } => CreateClientTokenRequest
}

wire_struct! {
    /// One-time token creation response.
    pub struct TokenCreated {
        pub schema_version: u8,
        pub token_id: UuidV7,
        pub client_label: String,
        pub scopes: Vec<ClientScope>,
        pub secret: String,
    } => TokenCreated
}

wire_struct! {
    /// Non-sensitive token lifecycle projection.
    pub struct TokenSummary {
        pub schema_version: u8,
        pub token_id: UuidV7,
        pub client_label: String,
        pub scopes: Vec<ClientScope>,
        pub status: TokenStatus,
        pub created_at: Rfc3339DateTime,
        pub revoked_at: Option<Rfc3339DateTime>,
    } => TokenSummary
}

wire_struct! {
    /// Bounded token page.
    pub struct TokenSummaryPage {
        pub schema_version: u8,
        pub items: Vec<TokenSummary>,
        pub next_cursor: Option<Cursor>,
    } => TokenSummaryPage
}

wire_struct! {
    /// Request to register a repository or directory root.
    pub struct RegisterProjectRequest {
        pub schema_version: u8,
        pub root: String,
    } => RegisterProjectRequest
}

wire_struct! {
    /// Registered project projection.
    pub struct WireProject {
        pub schema_version: u8,
        pub project_id: UuidV7,
        pub root_workspace_id: UuidV7,
        pub kind: ProjectKind,
        pub revision: JsonInteger,
    } => Project
}

/// Frozen `Project` response type.
pub type Project = WireProject;

wire_struct! {
    /// Request to resolve a workspace path inside a project.
    pub struct ResolveWorkspaceRequest {
        pub schema_version: u8,
        pub project_id: UuidV7,
        pub path: String,
    } => ResolveWorkspaceRequest
}

wire_struct! {
    /// Resolved workspace projection.
    pub struct WireWorkspace {
        pub schema_version: u8,
        pub workspace_id: UuidV7,
        pub project_id: UuidV7,
        pub revision: JsonInteger,
    } => Workspace
}

/// Frozen `Workspace` response type.
pub type Workspace = WireWorkspace;

wire_struct! {
    /// Request to create an agent session.
    pub struct CreateSessionRequest {
        pub schema_version: u8,
        pub workspace_id: UuidV7,
        pub agent_id: UuidV7,
    } => CreateSessionRequest
}

wire_struct! {
    /// Agent session projection.
    pub struct WireSession {
        pub schema_version: u8,
        pub session_id: UuidV7,
        pub workspace_id: UuidV7,
        pub agent_id: UuidV7,
        pub state: SessionState,
        pub revision: JsonInteger,
    } => Session
}

/// Frozen `Session` response type.
pub type Session = WireSession;

wire_struct! {
    /// Snapshot-bound session page.
    pub struct SessionPage {
        pub schema_version: u8,
        pub snapshot_sequence: JsonInteger,
        pub items: Vec<Session>,
        pub next_cursor: Option<Cursor>,
    } => SessionPage
}

wire_struct! {
    /// Positive-revision command body; bound to `endSession`, whose
    /// contract schema `EndSessionRequest` adds `outcome` (`completed` or
    /// `abandoned`, ADR 0013 D6, P3-D).
    pub struct ExpectedRevisionCommand {
        pub schema_version: u8,
        pub expected_revision: ExpectedRevision,
        pub reason: Option<String>,
        pub outcome: Option<String>,
    } => ExpectedRevisionCommand
}

/// Producer identity and per-producer event sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventProducer {
    pub name: String,
    pub version: String,
    pub sequence: JsonInteger,
}

wire_struct! {
    /// Frozen event envelope shared by ingest and retrieval.
    pub struct EventEnvelope {
        pub envelope_version: String,
        pub event_id: UuidV7,
        pub project_id: UuidV7,
        pub workspace_id: UuidV7,
        pub session_id: Option<UuidV7>,
        pub event_type: EventType,
        pub payload_schema: String,
        pub occurred_at: Option<Rfc3339Utc>,
        pub producer: EventProducer,
        pub correlation_id: Option<UuidV7>,
        pub causation_id: Option<UuidV7>,
        pub privacy_class: PrivacyClass,
        pub payload: JsonValue,
    } => EventEnvelope
}

wire_struct! {
    /// Bounded event-ingest batch.
    pub struct EventBatchRequest {
        pub events: Vec<EventEnvelope>,
    } => EventBatchRequest
}

wire_struct! {
    /// Durable or asynchronous command acceptance projection.
    pub struct Accepted {
        pub schema_version: u8,
        pub request_id: UuidV7,
        pub status: String,
        pub resource_id: Option<UuidV7>,
    } => Accepted
}

wire_struct! {
    /// Snapshot-bound event page.
    pub struct EventPage {
        pub schema_version: u8,
        pub snapshot_sequence: JsonInteger,
        pub items: Vec<EventEnvelope>,
        pub next_cursor: Option<Cursor>,
    } => EventPage
}

/// One ordered server-sent event frame sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventStream(pub String);

impl WireType for EventStream {
    const SCHEMA: SchemaId = SchemaId::EventStream;
}

wire_struct! {
    /// Request to create a task.
    pub struct CreateTaskRequest {
        pub schema_version: u8,
        pub project_id: UuidV7,
        pub workspace_id: Option<UuidV7>,
        pub title: String,
        pub description: Option<String>,
    } => CreateTaskRequest
}

wire_struct! {
    /// Current task projection.
    pub struct WireTask {
        pub schema_version: u8,
        pub task_id: UuidV7,
        pub project_id: UuidV7,
        pub workspace_id: Option<UuidV7>,
        pub title: String,
        pub state: TaskState,
        pub revision: JsonInteger,
        pub dependency_ids: Vec<UuidV7>,
        pub completion_evidence_ids: Vec<UuidV7>,
    } => Task
}

/// Frozen `Task` response type.
pub type Task = WireTask;

wire_struct! {
    /// Snapshot-bound task page.
    pub struct TaskPage {
        pub schema_version: u8,
        pub snapshot_sequence: JsonInteger,
        pub items: Vec<Task>,
        pub next_cursor: Option<Cursor>,
    } => TaskPage
}

wire_struct! {
    /// Request to transition a task through the frozen lifecycle.
    pub struct TaskTransitionRequest {
        pub schema_version: u8,
        pub expected_revision: ExpectedRevision,
        pub target_state: TaskState,
        pub completion_evidence_ids: Option<Vec<UuidV7>>,
        pub force_reason: Option<String>,
        pub workspace_id: Option<UuidV7>,
        /// U9 SF1-A (the Owner's O8 exception to R1): the Session that asks
        /// for the transition, by which a completion is judged; only with
        /// `target_state` `completed`, and one of the Task's Sessions.
        pub session_id: Option<UuidV7>,
    } => TaskTransitionRequest
}

wire_struct! {
    /// Request to add a dependency to a task.
    pub struct TaskDependencyRequest {
        pub schema_version: u8,
        pub expected_revision: ExpectedRevision,
        pub dependency_task_id: UuidV7,
    } => TaskDependencyRequest
}

wire_struct! {
    /// Stable machine-readable failure response.
    pub struct Problem {
        pub type_uri: String,
        pub title: String,
        pub status: u16,
        pub detail: Option<String>,
        pub code: ProblemCode,
        pub request_id: UuidV7,
        pub retryable: bool,
    } => Problem
}

/// Either replayed token metadata or a newly created one-time secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateClientTokenSuccess {
    /// Idempotent replay without secret disclosure.
    Existing(TokenSummary),
    /// First creation with the one-time secret.
    Created(TokenCreated),
}

impl WireType for CreateClientTokenSuccess {
    const SCHEMA: SchemaId = SchemaId::CreateClientTokenSuccess;
}

/// Compile-time closure for one frozen Phase 1 operation.
pub trait WireOperation {
    /// Request body type, or [`NoBody`].
    type Request: WireType;
    /// Successful response type.
    type Success: WireType;
    /// Stable problem response type.
    type Error: WireType;

    /// Frozen operation identity.
    const OPERATION_ID: OperationId;
}

/// Unique zero-sized operation markers.
pub mod operations {
    use super::{
        Accepted, CreateClientTokenRequest, CreateClientTokenSuccess, CreateSessionRequest,
        CreateTaskRequest, DaemonStatus, EventBatchRequest, EventEnvelope, EventPage, EventStream,
        ExpectedRevisionCommand, Health, NoBody, OperationId, Problem, Project,
        RegisterProjectRequest, ResolveWorkspaceRequest, Session, SessionPage, Task,
        TaskDependencyRequest, TaskPage, TaskTransitionRequest, TokenSummary, TokenSummaryPage,
        VersionInfo, WireOperation, Workspace,
    };

    macro_rules! operation {
        ($name:ident, $id:ident, $request:ty, $success:ty) => {
            #[doc = concat!("Binding marker for `", stringify!($id), "`.")]
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $name;

            impl WireOperation for $name {
                type Request = $request;
                type Success = $success;
                type Error = Problem;

                const OPERATION_ID: OperationId = OperationId::$id;
            }
        };
    }

    operation!(HealthLive, HealthLive, NoBody, Health);
    operation!(HealthReady, HealthReady, NoBody, Health);
    operation!(GetStatus, GetStatus, NoBody, DaemonStatus);
    operation!(GetVersion, GetVersion, NoBody, VersionInfo);
    operation!(
        CreateClientToken,
        CreateClientToken,
        CreateClientTokenRequest,
        CreateClientTokenSuccess
    );
    operation!(ListClientTokens, ListClientTokens, NoBody, TokenSummaryPage);
    operation!(RevokeClientToken, RevokeClientToken, NoBody, TokenSummary);
    operation!(
        RegisterProject,
        RegisterProject,
        RegisterProjectRequest,
        Project
    );
    operation!(GetProject, GetProject, NoBody, Project);
    operation!(
        ResolveWorkspace,
        ResolveWorkspace,
        ResolveWorkspaceRequest,
        Workspace
    );
    operation!(CreateSession, CreateSession, CreateSessionRequest, Session);
    operation!(ListSessions, ListSessions, NoBody, SessionPage);
    operation!(EndSession, EndSession, ExpectedRevisionCommand, Session);
    operation!(IngestEvent, IngestEvent, EventEnvelope, Accepted);
    operation!(ListEvents, ListEvents, NoBody, EventPage);
    operation!(
        IngestEventBatch,
        IngestEventBatch,
        EventBatchRequest,
        Accepted
    );
    operation!(StreamEvents, StreamEvents, NoBody, EventStream);
    operation!(GetEvent, GetEvent, NoBody, EventEnvelope);
    operation!(CreateTask, CreateTask, CreateTaskRequest, Task);
    operation!(ListTasks, ListTasks, NoBody, TaskPage);
    operation!(GetNextTask, GetNextTask, NoBody, Task);
    operation!(TransitionTask, TransitionTask, TaskTransitionRequest, Task);
    operation!(
        AddTaskDependency,
        AddTaskDependency,
        TaskDependencyRequest,
        Task
    );
}
