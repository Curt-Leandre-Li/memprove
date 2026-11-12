use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{
    AgentId, CanonicalJson, CorrelationId, DomainError, EventId, EvidenceId, IdempotencyKey,
    OperationId, PrincipalId, ProjectId, SessionId, Sha256Digest, TaskId, UtcTimestamp,
    WorkspaceId, canonical::MAX_SAFE_INTEGER,
};

/// Internal lifecycle used while provisioning both databases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectProvisioningState {
    Provisioning,
    Active,
    Failed,
}

impl ProjectProvisioningState {
    /// Returns the stable `SQLite` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provisioning => "PROVISIONING",
            Self::Active => "ACTIVE",
            Self::Failed => "FAILED",
        }
    }
}

/// Minimal task lifecycle aligned with the frozen contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Proposed,
    Ready,
    InProgress,
    Blocked,
    Completed,
    Cancelled,
}

impl TaskStatus {
    /// Returns the frozen wire-compatible spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Ready => "ready",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    /// P4-D (Owner A1): whether a Session may be linked to a Task in this
    /// state. A `completed` or `cancelled` Task takes no new Session:
    /// `createSession` with its `task_id` is `409 invalid_transition`. The
    /// Task Capsule's `begin_session` action reads the same function.
    #[must_use]
    pub const fn takes_sessions(self) -> bool {
        !matches!(self, Self::Completed | Self::Cancelled)
    }

    /// Parses the frozen wire spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "proposed" => Self::Proposed,
            "ready" => Self::Ready,
            "in_progress" => Self::InProgress,
            "blocked" => Self::Blocked,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    /// Checks one lifecycle step against the Task state machine
    /// (`docs/architecture/domain-model.md`):
    ///
    /// ```text
    /// proposed -> ready -> in_progress -> completed
    ///                        |     ^
    ///                        v     |
    ///                      blocked
    ///
    /// proposed | ready | blocked -> cancelled
    /// ```
    ///
    /// `completed` and `cancelled` are terminal, and staying in the same state
    /// is not a step. Entering `completed` additionally requires completion
    /// Evidence.
    ///
    /// # Errors
    ///
    /// [`TaskTransitionError::NotAllowed`] for a step outside the graph, or
    /// [`TaskTransitionError::CompletionEvidenceRequired`] for
    /// `in_progress -> completed` without completion Evidence.
    pub const fn check_transition(
        self,
        target: Self,
        has_completion_evidence: bool,
    ) -> Result<(), TaskTransitionError> {
        match (self, target) {
            (Self::Proposed, Self::Ready)
            | (Self::Ready | Self::Blocked, Self::InProgress)
            | (Self::InProgress, Self::Blocked)
            | (Self::Proposed | Self::Ready | Self::Blocked, Self::Cancelled) => Ok(()),
            (Self::InProgress, Self::Completed) if has_completion_evidence => Ok(()),
            (Self::InProgress, Self::Completed) => {
                Err(TaskTransitionError::CompletionEvidenceRequired)
            }
            _ => Err(TaskTransitionError::NotAllowed),
        }
    }
}

/// Why a Task lifecycle step was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTransitionError {
    /// The step is not an edge of the Task state machine.
    NotAllowed,
    /// `completed` requires completion Evidence.
    CompletionEvidenceRequired,
}

/// Registry record for one real repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub project_id: ProjectId,
    pub repository_common_dir: String,
    pub object_format: String,
    pub state_db_path: String,
    pub provisioning_state_internal: ProjectProvisioningState,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub revision: u64,
}

/// Per-project record for one Git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub workspace_id: WorkspaceId,
    pub project_id: ProjectId,
    pub canonical_worktree_path: String,
    pub git_dir: String,
    pub head_commit: String,
    pub branch_or_detached_state: String,
    pub created_at: UtcTimestamp,
    pub revision: u64,
}

/// One durable unit of user work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub task_id: TaskId,
    pub project_id: ProjectId,
    pub workspace_id: Option<WorkspaceId>,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub revision: u64,
    /// The Evidence that completed the Task, in request order; empty unless
    /// it is `completed` (P2-C). Recorded in its `task.transitioned` Event.
    pub completion_evidence_ids: Vec<EvidenceId>,
}

/// One Agent/client execution session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub agent_id: AgentId,
    pub started_at: UtcTimestamp,
    pub ended_at: Option<UtcTimestamp>,
    pub revision: u64,
    /// U8 A1 (Owner O4): the agent the Session declared itself to be at
    /// `createSession`, `None` for a Session that declared none (every
    /// Session before U8).
    pub agent: Option<SessionAgent>,
    /// U9 A7 (Owner O3): who ended the Session as abandoned, as the ender
    /// reported itself; `None` for a Session that has not ended, ended
    /// normally, or was ended by one that reported nothing (every Session
    /// before U9). Read from the Session's row (migration 0009), whose
    /// agent has a name and a role but no parent, or from its
    /// `session.crashed@2` Event, whose agent is whole.
    pub ended_by: Option<SessionEndedBy>,
}

/// U8 A1 (Owner O4; ADR 0010, "Amendment (U8)"): the payload schema of a
/// `session.started` Event that records a [`SessionAgent`]. Its payload is
/// that of `session.started@1` (`{agent_id, task_id}`) plus the member
/// `agent` (`{name, role, parent_session_id}`). A Session without an agent
/// is still recorded as `session.started@1`, with no `agent` member, so
/// nothing of a Session that declares none changes. The U8 task named the
/// new version `@3`; no `session.started@2` was ever written, and none is
/// read.
pub const SESSION_STARTED_AGENT_SCHEMA: &str = "session.started@3";

/// U8 A1 (Owner O4): the closed set of roles a Session's agent declares.
/// The role is a record only: `memproved` refuses nothing by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentRole {
    /// The session that dispatches the others and reviews their work.
    Orchestrator,
    /// An agent another Session dispatched.
    SubAgent,
    /// A person.
    Human,
}

impl AgentRole {
    /// Every role, in declaration order.
    pub const ALL: [Self; 3] = [Self::Orchestrator, Self::SubAgent, Self::Human];

    /// Returns the wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Orchestrator => "orchestrator",
            Self::SubAgent => "sub_agent",
            Self::Human => "human",
        }
    }

    /// Parses the wire spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == value)
    }
}

/// U8 A1 (Owner O4): who a Session's agent is, as the Session declared it:
/// a short name, a role, and the Session that dispatched it, if any. All
/// three are the client's own report; `memproved` checks their form and
/// that the parent Session exists in the Project (in any state), and
/// records them in `session.started@3` and the Session's row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionAgent {
    /// `^[A-Za-z0-9_-]{1,64}$`.
    pub name: String,
    pub role: AgentRole,
    pub parent_session_id: Option<SessionId>,
}

/// Why an `agent` is refused (`400 invalid_request`); the text is the
/// `detail`, which names the member and the rule, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRefusal(pub String);

/// The rule of [`SessionAgent::name`], as the refusal states it.
pub const AGENT_NAME_RULE: &str = "must be 1 to 64 ASCII letters, digits, _ or -";

impl SessionAgent {
    /// Whether `name` is an agent name: 1 to 64 ASCII letters, digits, `_`,
    /// or `-`.
    #[must_use]
    pub fn is_name(name: &str) -> bool {
        (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }

    /// An agent from its three members as text; `parent_session_id` absent
    /// is `None`.
    ///
    /// # Errors
    ///
    /// [`AgentRefusal`] naming the first member that does not hold.
    pub fn new(
        name: &str,
        role: &str,
        parent_session_id: Option<&str>,
    ) -> Result<Self, AgentRefusal> {
        if !Self::is_name(name) {
            return Err(AgentRefusal(format!("$.agent.name {AGENT_NAME_RULE}")));
        }
        let Some(role) = AgentRole::parse(role) else {
            return Err(AgentRefusal(
                "$.agent.role must be orchestrator, sub_agent, or human".to_owned(),
            ));
        };
        let parent_session_id = parent_session_id
            .map(|id| {
                SessionId::parse(id).map_err(|_| {
                    AgentRefusal("$.agent.parent_session_id must be a lowercase UUIDv7".to_owned())
                })
            })
            .transpose()?;
        Ok(Self {
            name: name.to_owned(),
            role,
            parent_session_id,
        })
    }

    /// Reads the `agent` member of a `createSession` body: an object with
    /// `name`, `role`, and optionally `parent_session_id` (absent and `null`
    /// are the same).
    ///
    /// # Errors
    ///
    /// [`AgentRefusal`] naming the first member that does not hold.
    pub fn from_request(value: &Value) -> Result<Self, AgentRefusal> {
        let Some(object) = value.as_object() else {
            return Err(AgentRefusal("$.agent must be an object".to_owned()));
        };
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "name" | "role" | "parent_session_id"))
        {
            return Err(AgentRefusal(
                "$.agent has a member other than name, role, and parent_session_id".to_owned(),
            ));
        }
        let text = |name: &str| object.get(name).and_then(Value::as_str);
        let parent = match object.get("parent_session_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => Some(id.as_str()),
            Some(_) => {
                return Err(AgentRefusal(
                    "$.agent.parent_session_id must be a lowercase UUIDv7".to_owned(),
                ));
            }
        };
        Self::new(
            text("name").unwrap_or_default(),
            text("role").unwrap_or_default(),
            parent,
        )
    }

    /// The `agent` member of a wire `Session`: `{name, role}`, with
    /// `parent_session_id` only when there is one.
    #[must_use]
    pub fn wire(&self) -> Value {
        let mut wire = json!({ "name": self.name, "role": self.role.as_str() });
        if let Some(parent) = &self.parent_session_id {
            wire["parent_session_id"] = json!(parent);
        }
        wire
    }

    /// The `agent` member as `session.started@3` records it and the Task
    /// Capsule shows it: `{name, role, parent_session_id}`, the last `null`
    /// when there is none.
    #[must_use]
    pub fn recorded(&self) -> Value {
        json!({
            "name": self.name,
            "role": self.role.as_str(),
            "parent_session_id": self.parent_session_id,
        })
    }

    /// Reads [`Self::recorded`] back: exactly its three members, each of
    /// its form; `None` otherwise.
    #[must_use]
    pub fn from_recorded(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 3 {
            return None;
        }
        let parent = match object.get("parent_session_id")? {
            Value::Null => None,
            Value::String(id) => Some(id.as_str()),
            _ => return None,
        };
        Self::new(
            object.get("name")?.as_str()?,
            object.get("role")?.as_str()?,
            parent,
        )
        .ok()
    }
}

/// U9 A7 (Owner O3; ADR 0010, "Amendment (U9)"): the payload schema of a
/// `session.crashed` Event that records who ended the Session. Its payload
/// is that of `session.crashed@1` (`{reason, outcome}`) plus the member
/// `ended_by` ([`SessionEndedBy::recorded`]). An abandoned end that names
/// no ender is still recorded as `session.crashed@1`, with no `ended_by`
/// member, so its bytes are those of before U9; `session.ended` does not
/// change.
pub const SESSION_CRASHED_ENDED_BY_SCHEMA: &str = "session.crashed@2";

/// U9 A7 (Owner O3): who ended a Session as abandoned, as the ender
/// reported itself: the Session its own context names (never the Session
/// it ends) and the agent it declares, either or both. `memproved`
/// checks their form, that the Session and the agent's parent are Sessions
/// of the Project, and records them in `session.crashed@2` and three
/// columns of the ended Session's row (migration 0009: the Session, the
/// agent's name, and its role).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionEndedBy {
    /// The ender's Session.
    pub session_id: Option<SessionId>,
    /// The agent the ender declares.
    pub agent: Option<SessionAgent>,
}

impl SessionEndedBy {
    /// Reads the `ended_by` member of an `endSession` body: an object with
    /// `session_id` (a `UUIDv7`) and `agent` (as `createSession`'s, U8 O4),
    /// either or both (absent and `null` are the same), and nothing else.
    ///
    /// # Errors
    ///
    /// [`AgentRefusal`] naming the first member that does not hold; never
    /// a value.
    pub fn from_request(value: &Value) -> Result<Self, AgentRefusal> {
        let Some(object) = value.as_object() else {
            return Err(AgentRefusal("$.ended_by must be an object".to_owned()));
        };
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "session_id" | "agent"))
        {
            return Err(AgentRefusal(
                "$.ended_by has a member other than session_id and agent".to_owned(),
            ));
        }
        let session_id = match object.get("session_id") {
            None | Some(Value::Null) => None,
            Some(id) => Some(
                id.as_str()
                    .and_then(|id| SessionId::parse(id).ok())
                    .ok_or_else(|| {
                        AgentRefusal("$.ended_by.session_id must be a lowercase UUIDv7".to_owned())
                    })?,
            ),
        };
        let agent = match object.get("agent") {
            None | Some(Value::Null) => None,
            Some(agent) => Some(SessionAgent::from_request(agent).map_err(|refusal| {
                AgentRefusal(refusal.0.replacen("$.agent", "$.ended_by.agent", 1))
            })?),
        };
        if session_id.is_none() && agent.is_none() {
            return Err(AgentRefusal(
                "$.ended_by must name a session_id, an agent, or both".to_owned(),
            ));
        }
        Ok(Self { session_id, agent })
    }

    /// The `ended_by` member as `session.crashed@2` records it and the
    /// Task Capsule shows it: `session_id` and `agent`
    /// ([`SessionAgent::recorded`]), each only when there is one.
    #[must_use]
    pub fn recorded(&self) -> Value {
        let mut recorded = json!({});
        if let Some(session_id) = &self.session_id {
            recorded["session_id"] = json!(session_id);
        }
        if let Some(agent) = &self.agent {
            recorded["agent"] = agent.recorded();
        }
        recorded
    }

    /// Reads [`Self::recorded`] back: one or both of its members, each of
    /// its form, and nothing else; `None` otherwise.
    #[must_use]
    pub fn from_recorded(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.is_empty()
            || object
                .keys()
                .any(|key| !matches!(key.as_str(), "session_id" | "agent"))
        {
            return None;
        }
        let session_id = match object.get("session_id") {
            None => None,
            Some(id) => Some(SessionId::parse(id.as_str()?).ok()?),
        };
        let agent = match object.get("agent") {
            None => None,
            Some(agent) => Some(SessionAgent::from_recorded(agent)?),
        };
        Some(Self { session_id, agent })
    }

    /// The `ended_by` member of a wire `Session`, from the row's three
    /// columns: `session_id` and `agent` (`{name, role}`: the row keeps no
    /// parent), each only when there is one.
    #[must_use]
    pub fn wire(&self) -> Value {
        let mut wire = json!({});
        if let Some(session_id) = &self.session_id {
            wire["session_id"] = json!(session_id);
        }
        if let Some(agent) = &self.agent {
            wire["agent"] = json!({ "name": agent.name, "role": agent.role.as_str() });
        }
        wire
    }

    /// The values of the three columns of migration 0009:
    /// `ended_by_session_id`, `ended_by_agent_name`, `ended_by_agent_role`.
    #[must_use]
    pub fn columns(ended_by: Option<&Self>) -> [Option<String>; 3] {
        let agent = ended_by.and_then(|ended_by| ended_by.agent.as_ref());
        [
            ended_by
                .and_then(|ended_by| ended_by.session_id.as_ref())
                .map(ToString::to_string),
            agent.map(|agent| agent.name.clone()),
            agent.map(|agent| agent.role.as_str().to_owned()),
        ]
    }

    /// Reads the three columns of migration 0009 back: all NULL is none;
    /// otherwise a Session, an agent's name and role (both or neither), or
    /// both.
    ///
    /// # Errors
    ///
    /// [`AgentRefusal`] when a value is not of its form or a name comes
    /// without a role or a role without a name.
    pub fn from_columns(
        session_id: Option<&str>,
        name: Option<&str>,
        role: Option<&str>,
    ) -> Result<Option<Self>, AgentRefusal> {
        let session_id = session_id
            .map(|id| {
                SessionId::parse(id).map_err(|_| {
                    AgentRefusal("ended_by_session_id is not a lowercase UUIDv7".to_owned())
                })
            })
            .transpose()?;
        let agent = match (name, role) {
            (None, None) => None,
            (Some(name), Some(role)) => Some(SessionAgent::new(name, role, None)?),
            _ => {
                return Err(AgentRefusal(
                    "ended_by_agent_name and ended_by_agent_role are both set or both NULL"
                        .to_owned(),
                ));
            }
        };
        Ok((session_id.is_some() || agent.is_some()).then_some(Self { session_id, agent }))
    }
}

impl Session {
    /// Whether the Session still accepts Events: it has not ended.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// How `endSession` ends a Session (ADR 0013 D6, P3-D): `completed` (the
/// default) appends `session.ended`; `abandoned`, for a Session another
/// agent left unfinished, appends `session.crashed` in its place. Either
/// way the Session ends and then accepts no Event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionOutcome {
    /// The agent finished its Session.
    #[default]
    Completed,
    /// The Session was left unfinished and is ended by someone else.
    Abandoned,
}

impl SessionOutcome {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
        }
    }

    /// Parses the wire spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "completed" => Some(Self::Completed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// The Event type `memproved` appends for this outcome.
    #[must_use]
    pub const fn event_type(self) -> &'static str {
        match self {
            Self::Completed => "session.ended",
            Self::Abandoned => "session.crashed",
        }
    }
}

/// Normalized Task-to-Session relationship.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSessionLink {
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub linked_at: UtcTimestamp,
    pub revision: u64,
}

/// Typed namespace for one idempotent operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyContext {
    pub principal_id: PrincipalId,
    pub operation_id: OperationId,
    pub key: IdempotencyKey,
}

/// Durable idempotency lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyState {
    InProgress,
    Succeeded,
    Failed,
}

impl IdempotencyState {
    /// Returns the stable `SQLite` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "IN_PROGRESS",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
        }
    }
}

/// Persistent idempotency record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    pub context: IdempotencyContext,
    pub canonical_request_sha256: Sha256Digest,
    pub state: IdempotencyState,
    pub result_record_type: Option<String>,
    pub result_record_id: Option<String>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

/// Applied migration identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRecord {
    pub migration_id: u64,
    pub migration_checksum: Sha256Digest,
    pub applied_at: UtcTimestamp,
}

/// Envelope version accepted and persisted by this build.
pub const EVENT_ENVELOPE_VERSION: &str = "1.0";

/// Event digest version computed by this build (ADR 0010).
pub const EVENT_DIGEST_VERSION: u8 = 2;

/// Producer name of every Event the daemon writes itself (`continuityd`, the
/// name the ledger has carried since P2-E and keeps after the rename to memprove), and the
/// collector-name prefix of its own Evidence (`continuityd-git`).
pub const DAEMON_PRODUCER_NAME: &str = "continuityd";

/// Every Event type `memproved` produces itself (ADR 0012 D7, P2-A, P2-C;
/// ADR 0013 P3-C: `claim.superseded` and `decision.recorded`; P3-D:
/// `session.crashed`; ADR 0014 E6, P4-F: `capsule.recovered`). This is the
/// list of what it writes now: `ingestEvent` refuses all of them (P2-E), and
/// a test drives every daemon write and checks each type it records is here.
pub const DAEMON_EVENT_TYPES: [&str; 12] = [
    "session.started",
    "session.ended",
    "command.completed",
    "checkpoint.created",
    "task.created",
    "task.transitioned",
    "task.session_linked",
    "workspace.registered",
    "claim.superseded",
    "decision.recorded",
    "session.crashed",
    "capsule.recovered",
];

/// The Event types ADR 0013 D8 reserves to `memproved`:
/// `claim.superseded` (an edge between two claims), `decision.recorded` (a
/// Decision), and `session.crashed` (a Session that ended abandoned). The
/// read paths take them only from `memproved`, and `ingestEvent` refuses
/// them as it refuses [`DAEMON_EVENT_TYPES`]. `memproved` writes the
/// first two since P3-C and `session.crashed` since P3-D (`endSession` with
/// `outcome` `abandoned`), so those three are in both lists. P3-C2 (Owner, T12 N6):
/// `capsule.recovered` is reserved now, before any writer exists, so no
/// client-written row of it can ever be mistaken for one of `memproved`;
/// `memproved` writes it since P4-F (`createSession` with `resumed_from`,
/// ADR 0014 E6), so all four are in both lists.
pub const DAEMON_RESERVED_EVENT_TYPES: [&str; 4] = [
    "claim.superseded",
    "decision.recorded",
    "session.crashed",
    "capsule.recovered",
];

/// Whether only `memproved` may write `event_type`: it is in
/// [`DAEMON_EVENT_TYPES`] or [`DAEMON_RESERVED_EVENT_TYPES`].
#[must_use]
pub fn is_daemon_event_type(event_type: &str) -> bool {
    DAEMON_EVENT_TYPES.contains(&event_type) || DAEMON_RESERVED_EVENT_TYPES.contains(&event_type)
}

/// Whether a client-supplied producer or collector name claims to be
/// the daemon (P2-E): any name that starts with [`DAEMON_PRODUCER_NAME`],
/// in any ASCII case (P2-K, T11 S-4). The wire admits only ASCII letters,
/// digits, `.`, `_`, and `-` in these names, so no other look-alike exists.
#[must_use]
pub fn is_reserved_producer_name(name: &str) -> bool {
    name.get(..DAEMON_PRODUCER_NAME.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(DAEMON_PRODUCER_NAME))
}

/// Producer-declared privacy class of an Event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyClass {
    Public,
    Internal,
    Private,
    Secret,
}

impl PrivacyClass {
    /// Returns the contract spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Internal => "internal",
            Self::Private => "private",
            Self::Secret => "secret",
        }
    }

    /// Parses the contract spelling.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidEventChain`] for any other value.
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "public" => Ok(Self::Public),
            "internal" => Ok(Self::Internal),
            "private" => Ok(Self::Private),
            "secret" => Ok(Self::Secret),
            other => Err(DomainError::InvalidEventChain(format!(
                "unknown privacy class {other:?}"
            ))),
        }
    }
}

/// Producer identity and producer-declared sequence. The sequence is stored
/// and digested as given; it is not used for deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventProducer {
    pub name: String,
    pub version: String,
    pub sequence: u64,
}

/// Validated input used to compute one Event: the envelope fields plus the
/// daemon's receipt time. Ledger positions and digests are assigned by
/// [`Event::new`]. `session_id` is `None` only for an Event `memproved`
/// produces for a change without a Session (ADR 0012 D7, P2-C); a client
/// Event always names its Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventInput {
    pub event_id: EventId,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub session_id: Option<SessionId>,
    pub event_type: String,
    pub payload_schema: String,
    pub payload: CanonicalJson,
    /// Producer-declared occurrence time; `None` when the producer gave none.
    pub occurred_at: Option<UtcTimestamp>,
    /// Daemon receipt time.
    pub recorded_at: UtcTimestamp,
    pub producer: EventProducer,
    pub correlation_id: Option<CorrelationId>,
    pub causation_id: Option<EventId>,
    pub privacy_class: PrivacyClass,
}

/// Immutable event-ledger row with a version 2 digest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_field_names)] // Exact persisted Event contract names are intentional.
pub struct Event {
    input: EventInput,
    sequence: Option<u64>,
    project_sequence: u64,
    payload_sha256: Sha256Digest,
    previous_event_sha256: Option<Sha256Digest>,
    event_sha256: Sha256Digest,
}

impl Event {
    /// Computes the payload and version 2 Event digests. `sequence` is the
    /// position in the Session (`None` exactly when the Event has no
    /// Session), `project_sequence` the position in the Project chain, and
    /// `previous_event_sha256` the digest of the Event at
    /// `project_sequence - 1`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidEventChain`] when the input cannot form a valid Event.
    pub fn new(
        input: EventInput,
        sequence: Option<u64>,
        project_sequence: u64,
        previous_event_sha256: Option<Sha256Digest>,
    ) -> Result<Self, DomainError> {
        check_event_fields(
            &input,
            sequence,
            project_sequence,
            previous_event_sha256.as_ref(),
        )?;
        let payload_sha256 = input.payload.sha256();
        let event_sha256 = event_digest(
            &input,
            sequence,
            project_sequence,
            &payload_sha256,
            previous_event_sha256.as_ref(),
        );
        Ok(Self {
            input,
            sequence,
            project_sequence,
            payload_sha256,
            previous_event_sha256,
            event_sha256,
        })
    }

    /// Rehydrates an Event only when persisted digest bytes match recomputation.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidEventChain`] for invalid input or either digest mismatch.
    pub fn from_stored(
        input: EventInput,
        sequence: Option<u64>,
        project_sequence: u64,
        previous_event_sha256: Option<Sha256Digest>,
        stored_payload_sha256: &Sha256Digest,
        stored_event_sha256: &Sha256Digest,
    ) -> Result<Self, DomainError> {
        let event = Self::new(input, sequence, project_sequence, previous_event_sha256)?;
        if &event.payload_sha256 != stored_payload_sha256
            || &event.event_sha256 != stored_event_sha256
        {
            return Err(DomainError::InvalidEventChain(
                "stored Event digest mismatch".to_owned(),
            ));
        }
        Ok(event)
    }

    /// Recomputes and verifies this Event's payload and event digests.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidEventChain`] for any invariant violation.
    pub fn validate(&self) -> Result<(), DomainError> {
        check_event_fields(
            &self.input,
            self.sequence,
            self.project_sequence,
            self.previous_event_sha256.as_ref(),
        )?;
        if self.payload_sha256 != self.input.payload.sha256()
            || self.event_sha256 != self.recomputed_sha256()
        {
            return Err(DomainError::InvalidEventChain(
                "event digest mismatch".to_owned(),
            ));
        }
        Ok(())
    }

    /// Returns the envelope fields and receipt time.
    #[must_use]
    pub fn input(&self) -> &EventInput {
        &self.input
    }

    /// Returns the envelope version, always [`EVENT_ENVELOPE_VERSION`].
    #[must_use]
    pub const fn envelope_version(&self) -> &'static str {
        EVENT_ENVELOPE_VERSION
    }

    /// Returns the immutable Event identity.
    #[must_use]
    pub fn event_id(&self) -> &EventId {
        &self.input.event_id
    }

    /// Returns the owning Project identity.
    #[must_use]
    pub fn project_id(&self) -> &ProjectId {
        &self.input.project_id
    }

    /// Returns the Workspace of the owning Session.
    #[must_use]
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.input.workspace_id
    }

    /// Returns the owning Session identity; `None` for a daemon Event
    /// without a Session.
    #[must_use]
    pub fn session_id(&self) -> Option<&SessionId> {
        self.input.session_id.as_ref()
    }

    /// Returns the position in the Session, starting at one; `None` exactly
    /// when the Event has no Session.
    #[must_use]
    pub const fn sequence(&self) -> Option<u64> {
        self.sequence
    }

    /// Returns the position in the Project chain, starting at one.
    #[must_use]
    pub const fn project_sequence(&self) -> u64 {
        self.project_sequence
    }

    /// Returns the Event type.
    #[must_use]
    pub fn event_type(&self) -> &str {
        &self.input.event_type
    }

    /// Returns the payload schema identifier.
    #[must_use]
    pub fn payload_schema(&self) -> &str {
        &self.input.payload_schema
    }

    /// Returns the canonical Event payload.
    #[must_use]
    pub fn payload(&self) -> &CanonicalJson {
        &self.input.payload
    }

    /// Returns the recomputed payload digest.
    #[must_use]
    pub fn payload_sha256(&self) -> &Sha256Digest {
        &self.payload_sha256
    }

    /// Returns the producer-declared occurrence time, if any.
    #[must_use]
    pub fn occurred_at(&self) -> Option<&UtcTimestamp> {
        self.input.occurred_at.as_ref()
    }

    /// Returns the daemon receipt time.
    #[must_use]
    pub fn recorded_at(&self) -> &UtcTimestamp {
        &self.input.recorded_at
    }

    /// Returns the producer identity and sequence.
    #[must_use]
    pub fn producer(&self) -> &EventProducer {
        &self.input.producer
    }

    /// Returns the correlation identity, if any.
    #[must_use]
    pub fn correlation_id(&self) -> Option<&CorrelationId> {
        self.input.correlation_id.as_ref()
    }

    /// Returns the causing Event identity, if any.
    #[must_use]
    pub fn causation_id(&self) -> Option<&EventId> {
        self.input.causation_id.as_ref()
    }

    /// Returns the privacy class.
    #[must_use]
    pub const fn privacy_class(&self) -> PrivacyClass {
        self.input.privacy_class
    }

    /// Returns the preceding Project Event digest, if any.
    #[must_use]
    pub fn previous_event_sha256(&self) -> Option<&Sha256Digest> {
        self.previous_event_sha256.as_ref()
    }

    /// Returns the recomputed Event digest.
    #[must_use]
    pub fn event_sha256(&self) -> &Sha256Digest {
        &self.event_sha256
    }

    fn recomputed_sha256(&self) -> Sha256Digest {
        event_digest(
            &self.input,
            self.sequence,
            self.project_sequence,
            &self.payload_sha256,
            self.previous_event_sha256.as_ref(),
        )
    }
}

/// Verifies one Project's Event chain given in `project_sequence` order: every
/// digest, the contiguous Project sequence from one, each link to the
/// preceding Project Event, and the contiguous per-Session sequence of the
/// Events that have a Session.
///
/// # Errors
///
/// Returns [`DomainError::InvalidEventChain`] for any digest, sequence, or link break.
pub fn verify_event_chain<'a>(
    events: impl IntoIterator<Item = &'a Event>,
) -> Result<(), DomainError> {
    let mut chain = EventChainVerifier::default();
    for event in events {
        chain.push(event)?;
    }
    Ok(())
}

/// Checks a Project's Event chain one Event at a time, in `project_sequence`
/// order, so a caller can tell where it first breaks (`verifyLedger`, P2-C).
/// [`verify_event_chain`] is this check over a whole chain.
#[derive(Debug, Clone, Default)]
pub struct EventChainVerifier {
    previous: Option<(ProjectId, u64, Sha256Digest)>,
    session_heads: BTreeMap<SessionId, u64>,
}

impl EventChainVerifier {
    /// Checks the next Event: its digests, the next Project sequence, the
    /// link to the preceding Event, the same Project, and the next sequence
    /// of its Session.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidEventChain`] naming the first broken
    /// rule; the verifier is unchanged.
    pub fn push(&mut self, event: &Event) -> Result<(), DomainError> {
        event.validate()?;
        let expected_project_sequence = self
            .previous
            .as_ref()
            .map_or(1, |(_, sequence, _)| sequence + 1);
        if event.project_sequence != expected_project_sequence
            || event.previous_event_sha256.as_ref()
                != self.previous.as_ref().map(|(_, _, digest)| digest)
            || self
                .previous
                .as_ref()
                .is_some_and(|(project_id, _, _)| project_id != event.project_id())
        {
            return Err(DomainError::InvalidEventChain(
                "Project sequence or previous digest is broken".to_owned(),
            ));
        }
        if let (Some(session_id), Some(sequence)) = (event.session_id(), event.sequence) {
            let head = self.session_heads.get(session_id).copied().unwrap_or(0);
            if sequence != head + 1 {
                return Err(DomainError::InvalidEventChain(
                    "Session sequence is broken".to_owned(),
                ));
            }
            self.session_heads.insert(session_id.clone(), sequence);
        }
        self.previous = Some((
            event.project_id().clone(),
            event.project_sequence,
            event.event_sha256.clone(),
        ));
        Ok(())
    }
}

/// The kind of ledger record `verifyLedger` checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LedgerRecordType {
    /// One row of the Event chain.
    Event,
    /// One Evidence object.
    Evidence,
    /// One `RepositorySnapshot`.
    Snapshot,
    /// One Checkpoint.
    Checkpoint,
    /// One Task row, compared with its last Task Event (P2-E).
    Task,
    /// P4-C2 (Owner, ontology v4): one Session of a Task, its `sessions`
    /// row or its `task_sessions` row, missing, not decoding, or
    /// disagreeing with the chain; the record ID is the `session_id`.
    /// Only the Task Capsule names it; `verifyLedger` does not.
    Session,
    /// U1 B4 (Owner O5): one `workspaces` row; the record ID is the
    /// `workspace_id`. Only `verifyLedger` names it: the Project's root
    /// Workspace whose row is missing. A read of a Workspace row that is
    /// missing or does not decode leaves the probe unavailable and names no
    /// damaged record (P4-H1, T20 SF-1), so the Task Capsule does not.
    Workspace,
}

impl LedgerRecordType {
    /// Every record type, in declaration order (U3 G1).
    pub const ALL: [Self; 7] = [
        Self::Event,
        Self::Evidence,
        Self::Snapshot,
        Self::Checkpoint,
        Self::Task,
        Self::Session,
        Self::Workspace,
    ];

    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Evidence => "evidence",
            Self::Snapshot => "snapshot",
            Self::Checkpoint => "checkpoint",
            Self::Task => "task",
            Self::Session => "session",
            Self::Workspace => "workspace",
        }
    }
}

/// Where a ledger first fails to recompute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerFailure {
    /// The kind of record.
    pub record_type: LedgerRecordType,
    /// Its identity as stored (it may not parse).
    pub record_id: String,
    /// Its Project sequence, for an Event row that stores one.
    pub project_sequence: Option<u64>,
    /// What does not hold.
    pub reason: String,
}

/// The result of recomputing a Project's ledger (`verifyLedger`, P2-C): the
/// Event chain, then every Evidence object, `RepositorySnapshot`, and
/// Checkpoint, each with its digests recomputed and (P2-E) compared with
/// the digests `memproved`'s own Events recorded for it, then every Task
/// row compared with its last Task Event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerVerification {
    /// The Project's latest Event sequence, 0 without Events.
    pub project_sequence: u64,
    /// Event rows checked.
    pub events: u64,
    /// Evidence objects checked.
    pub evidence: u64,
    /// `RepositorySnapshot`s checked.
    pub snapshots: u64,
    /// Checkpoints checked.
    pub checkpoints: u64,
    /// Task rows checked (P2-E).
    pub tasks: u64,
    /// The stored digest of the Event at `project_sequence`, the chain head.
    pub head_event_sha256: Option<Sha256Digest>,
    /// The first record that does not hold, in the order Events, Evidence,
    /// Snapshots, Checkpoints, Tasks; `None` when everything holds.
    pub first_failure: Option<LedgerFailure>,
}

impl LedgerVerification {
    /// Whether every record recomputed and the chain is unbroken.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.first_failure.is_none()
    }
}

/// Computes a request digest without idempotency namespace values.
#[must_use]
pub fn canonical_request_sha256(body: &CanonicalJson) -> Sha256Digest {
    body.sha256()
}

fn check_event_fields(
    input: &EventInput,
    sequence: Option<u64>,
    project_sequence: u64,
    previous_event_sha256: Option<&Sha256Digest>,
) -> Result<(), DomainError> {
    let invalid = |reason: &str| Err(DomainError::InvalidEventChain(reason.to_owned()));
    if input.session_id.is_some() != sequence.is_some() {
        return invalid("an Event has a Session sequence exactly when it has a Session");
    }
    if sequence == Some(0) || project_sequence == 0 {
        return invalid("event sequences must start at one");
    }
    if sequence.is_some_and(|sequence| sequence > MAX_SAFE_INTEGER)
        || project_sequence > MAX_SAFE_INTEGER
        || input.producer.sequence > MAX_SAFE_INTEGER
    {
        return invalid("event sequences must not exceed 2^53 - 1");
    }
    if (project_sequence == 1) != previous_event_sha256.is_none() {
        return invalid("only the first Project Event has no previous digest");
    }
    if input.event_type.is_empty()
        || input.payload_schema.is_empty()
        || input.producer.name.is_empty()
        || input.producer.version.is_empty()
    {
        return invalid("event type, payload schema, and producer must be non-empty");
    }
    if !input.payload.as_str().starts_with('{') {
        return invalid("event payload must be a JSON object");
    }
    Ok(())
}

/// Version 2 Event digest: SHA-256 over the RFC 8785 canonical bytes of an
/// object with exactly these 18 members, absent values as `null` (ADR 0010).
fn event_digest(
    input: &EventInput,
    sequence: Option<u64>,
    project_sequence: u64,
    payload_sha256: &Sha256Digest,
    previous_event_sha256: Option<&Sha256Digest>,
) -> Sha256Digest {
    let canonical = CanonicalJson::from_trusted_value(&json!({
        "digest_version": EVENT_DIGEST_VERSION,
        "envelope_version": EVENT_ENVELOPE_VERSION,
        "event_id": input.event_id.as_str(),
        "project_id": input.project_id.as_str(),
        "workspace_id": input.workspace_id.as_str(),
        "session_id": input.session_id.as_ref().map(SessionId::as_str),
        "session_sequence": sequence,
        "project_sequence": project_sequence,
        "event_type": input.event_type,
        "payload_schema": input.payload_schema,
        "occurred_at": input.occurred_at.as_ref().map(UtcTimestamp::as_str),
        "recorded_at": input.recorded_at.as_str(),
        "producer": {
            "name": input.producer.name,
            "version": input.producer.version,
            "sequence": input.producer.sequence,
        },
        "correlation_id": input.correlation_id.as_ref().map(CorrelationId::as_str),
        "causation_id": input.causation_id.as_ref().map(EventId::as_str),
        "privacy_class": input.privacy_class.as_str(),
        "payload_sha256": payload_sha256.as_str(),
        "previous_event_sha256": previous_event_sha256.map(Sha256Digest::as_str),
    }))
    .expect("typed Event fields always serialize to safe JSON");
    canonical.sha256()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2-K (T11 S-4): the `continuityd` prefix is reserved in any ASCII
    /// case; other names, including ones that merely contain it, are not.
    #[test]
    fn the_continuityd_prefix_is_reserved_in_any_ascii_case() {
        for reserved in [
            "continuityd",
            "Continuityd",
            "CONTINUITYD",
            "continuityD",
            "continuityd-git",
            "CONTINUITYD.cli",
            "continuityd2",
        ] {
            assert!(is_reserved_producer_name(reserved), "{reserved}");
        }
        for free in [
            "memprove",
            "memprove-cli",
            "continuit",
            "my-continuityd",
            "hook",
            "",
            "\u{e9}continuityd",
        ] {
            assert!(!is_reserved_producer_name(free), "{free}");
        }
    }

    #[test]
    fn task_state_machine_allows_exactly_the_documented_edges() {
        use TaskStatus::{Blocked, Cancelled, Completed, InProgress, Proposed, Ready};
        let all = [Proposed, Ready, InProgress, Blocked, Completed, Cancelled];
        let allowed = [
            (Proposed, Ready),
            (Ready, InProgress),
            (InProgress, Blocked),
            (Blocked, InProgress),
            (Proposed, Cancelled),
            (Ready, Cancelled),
            (Blocked, Cancelled),
        ];
        for from in all {
            assert_eq!(TaskStatus::parse(from.as_str()), Some(from));
            for to in all {
                let expected = if allowed.contains(&(from, to)) {
                    Ok(())
                } else if (from, to) == (InProgress, Completed) {
                    Err(TaskTransitionError::CompletionEvidenceRequired)
                } else {
                    Err(TaskTransitionError::NotAllowed)
                };
                assert_eq!(
                    from.check_transition(to, false),
                    expected,
                    "{from:?} -> {to:?}"
                );
            }
        }
        assert_eq!(InProgress.check_transition(Completed, true), Ok(()));
        assert_eq!(
            Ready.check_transition(Completed, true),
            Err(TaskTransitionError::NotAllowed)
        );
        assert_eq!(TaskStatus::parse("done"), None);
    }

    fn input() -> EventInput {
        EventInput {
            event_id: EventId::parse("01920000-0000-7000-8000-00000000e001").unwrap(),
            project_id: ProjectId::parse("01920000-0000-7000-8000-00000000a001").unwrap(),
            workspace_id: WorkspaceId::parse("01920000-0000-7000-8000-00000000b001").unwrap(),
            session_id: Some(SessionId::parse("01920000-0000-7000-8000-00000000c001").unwrap()),
            event_type: "session.started".to_owned(),
            payload_schema: "session.started@1".to_owned(),
            payload: CanonicalJson::parse(r#"{"ok":true}"#).unwrap(),
            occurred_at: None,
            recorded_at: UtcTimestamp::parse("2026-10-02T08:00:00Z").unwrap(),
            producer: EventProducer {
                name: "cli".to_owned(),
                version: "0.1.0".to_owned(),
                sequence: 0,
            },
            correlation_id: None,
            causation_id: None,
            privacy_class: PrivacyClass::Internal,
        }
    }

    #[test]
    fn digest_matches_the_adr_0010_test_vector() {
        let event = Event::new(input(), Some(1), 1, None).expect("valid Event");
        assert_eq!(
            event.payload_sha256().as_str(),
            "4062edaf750fb8074e7e83e0c9028c94e32468a8b6f1614774328ef045150f93"
        );
        assert_eq!(
            event.event_sha256().as_str(),
            "e58ddc30c8ec7397e3116f389594149cb1f6bd401caa09d11eac0968f706721f"
        );
    }

    #[test]
    fn every_envelope_field_is_covered_by_the_digest() {
        let base = Event::new(input(), Some(1), 1, None).unwrap();
        let mut variants = Vec::new();
        let mut changed = input();
        changed.workspace_id = WorkspaceId::new();
        variants.push(changed);
        let mut changed = input();
        changed.payload_schema = "session.started@2".to_owned();
        variants.push(changed);
        let mut changed = input();
        changed.occurred_at = Some(UtcTimestamp::parse("2026-10-02T07:59:59Z").unwrap());
        variants.push(changed);
        let mut changed = input();
        changed.producer.sequence = 1;
        variants.push(changed);
        let mut changed = input();
        changed.producer.name = "hook".to_owned();
        variants.push(changed);
        let mut changed = input();
        changed.correlation_id = Some(CorrelationId::new());
        variants.push(changed);
        let mut changed = input();
        changed.causation_id = Some(EventId::new());
        variants.push(changed);
        let mut changed = input();
        changed.privacy_class = PrivacyClass::Secret;
        variants.push(changed);
        for variant in variants {
            let event = Event::new(variant, Some(1), 1, None).unwrap();
            assert_ne!(event.event_sha256(), base.event_sha256());
        }
        assert_ne!(
            Event::new(input(), Some(2), 1, None)
                .unwrap()
                .event_sha256(),
            base.event_sha256()
        );
    }

    #[test]
    fn event_validation_rejects_private_digest_corruption() {
        let mut payload_corrupted = Event::new(input(), Some(1), 1, None).unwrap();
        payload_corrupted.payload_sha256 = Sha256Digest::of_bytes(b"forged payload digest");
        assert!(payload_corrupted.validate().is_err());

        let mut event_corrupted = Event::new(input(), Some(1), 1, None).unwrap();
        event_corrupted.event_sha256 = Sha256Digest::of_bytes(b"forged event digest");
        assert!(event_corrupted.validate().is_err());
    }

    #[test]
    fn construction_enforces_genesis_links_and_safe_sequences() {
        let digest = Sha256Digest::of_bytes(b"previous");
        assert!(Event::new(input(), Some(1), 1, Some(digest.clone())).is_err());
        assert!(Event::new(input(), Some(1), 2, None).is_err());
        assert!(Event::new(input(), Some(0), 1, None).is_err());
        assert!(Event::new(input(), Some(1), MAX_SAFE_INTEGER + 1, Some(digest)).is_err());
        let mut unsafe_producer = input();
        unsafe_producer.producer.sequence = MAX_SAFE_INTEGER + 1;
        assert!(Event::new(unsafe_producer, Some(1), 1, None).is_err());
        let mut array_payload = input();
        array_payload.payload = CanonicalJson::parse("[1]").unwrap();
        assert!(Event::new(array_payload, Some(1), 1, None).is_err());
    }

    #[test]
    fn project_chain_links_sessions_and_rejects_breaks() {
        let first = Event::new(input(), Some(1), 1, None).unwrap();
        let mut other_session = input();
        other_session.event_id = EventId::new();
        other_session.session_id = Some(SessionId::new());
        let second = Event::new(
            other_session,
            Some(1),
            2,
            Some(first.event_sha256().clone()),
        )
        .unwrap();
        let mut same_session = input();
        same_session.event_id = EventId::new();
        let third = Event::new(
            same_session.clone(),
            Some(2),
            3,
            Some(second.event_sha256().clone()),
        )
        .unwrap();
        verify_event_chain([&first, &second, &third]).expect("valid Project chain");

        assert!(
            verify_event_chain([&second]).is_err(),
            "genesis must be first"
        );
        assert!(
            verify_event_chain([&first, &third]).is_err(),
            "gap in Project chain"
        );
        let skipped = Event::new(
            same_session,
            Some(3),
            3,
            Some(second.event_sha256().clone()),
        )
        .unwrap();
        assert!(
            verify_event_chain([&first, &second, &skipped]).is_err(),
            "gap in Session sequence"
        );
        let mut foreign = input();
        foreign.event_id = EventId::new();
        foreign.project_id = ProjectId::new();
        let foreign = Event::new(foreign, Some(2), 2, Some(first.event_sha256().clone())).unwrap();
        assert!(verify_event_chain([&first, &foreign]).is_err());
    }

    /// ADR 0012 D7 (P2-C): a daemon Event without a Session digests its
    /// Session members as `null`, takes no Session sequence, and chains like
    /// any other Event without disturbing the Session sequences around it.
    #[test]
    fn an_event_without_a_session_digests_nulls_and_chains() {
        let mut daemon = input();
        daemon.event_id = EventId::new();
        daemon.session_id = None;
        daemon.event_type = "task.created".to_owned();
        daemon.payload_schema = "task.created@1".to_owned();
        assert!(Event::new(daemon.clone(), Some(1), 1, None).is_err());
        let mut sessionful = input();
        assert!(Event::new(sessionful.clone(), None, 1, None).is_err());

        let first = Event::new(daemon.clone(), None, 1, None).unwrap();
        assert_eq!((first.session_id(), first.sequence()), (None, None));
        let mut with_session = daemon.clone();
        with_session.session_id = Some(SessionId::new());
        assert_ne!(
            Event::new(with_session, Some(1), 1, None)
                .unwrap()
                .event_sha256(),
            first.event_sha256(),
            "a null Session is digested"
        );
        sessionful.event_id = EventId::new();
        let second = Event::new(
            sessionful.clone(),
            Some(1),
            2,
            Some(first.event_sha256().clone()),
        )
        .unwrap();
        daemon.event_id = EventId::new();
        let third = Event::new(daemon, None, 3, Some(second.event_sha256().clone())).unwrap();
        sessionful.event_id = EventId::new();
        let fourth =
            Event::new(sessionful, Some(2), 4, Some(third.event_sha256().clone())).unwrap();
        verify_event_chain([&first, &second, &third, &fourth]).expect("mixed chain");
        let stored = Event::from_stored(
            first.input().clone(),
            None,
            1,
            None,
            first.payload_sha256(),
            first.event_sha256(),
        )
        .unwrap();
        assert_eq!(stored, first);
    }
}
