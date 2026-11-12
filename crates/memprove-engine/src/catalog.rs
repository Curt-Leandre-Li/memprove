//! Project, Workspace, Task, and Session application service.
//!
//! The service owns the check order and the cross-record rules of the slice C
//! operations; persistence and Git probing are ports implemented by the daemon
//! over `memprove-store-sqlite` and `memprove-repo`.

use std::path::{Path, PathBuf};

use memprove_domain::{
    AgentId, CompletionBlock, CompletionEvidence, EventId, EvidenceId, IdempotencyContext, Probe,
    Project, ProjectId, ResumedFrom, RoleRule, RunHistory, Session, SessionAgent, SessionEndedBy,
    SessionId, SessionOutcome, Sha256Digest, Task, TaskId, TaskStatus, UtcTimestamp, Workspace,
    WorkspaceId, completion_blocks, is_sub_agent,
};

/// A lifecycle command guarded by optimistic concurrency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revisioned<T> {
    /// The record's revision the caller last read.
    pub expected_revision: u64,
    /// The requested step.
    pub change: T,
}

/// Why a catalog operation did not succeed. Each variant maps to exactly one
/// contract Problem code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// The request is well-formed JSON but cannot be served as asked. The
    /// text is the `detail` of the `400` Problem: the field or parameter and
    /// the rule it violates, never a value from the request.
    InvalidRequest(String),
    /// No `ACTIVE` Project has the requested identity.
    ProjectNotFound,
    /// No registered Workspace matches the request.
    WorkspaceNotResolved,
    /// The Workspace or path belongs to a different Project, or the Task is
    /// scoped to another Workspace.
    ProjectWorkspaceMismatch,
    /// The idempotency key was reused with a different request.
    IdempotencyConflict,
    /// The Session or Task does not exist.
    ResourceNotFound,
    /// The `expected_revision` is not the record's current revision.
    RevisionConflict,
    /// The lifecycle step is not allowed from the record's current state.
    InvalidTransition,
    /// The completion Evidence does not establish that the Task is complete
    /// (P2-C).
    EvidenceStale,
    /// P3-L L3: the Task Capsule's integrity check finds the Task
    /// `tampered`, so no Evidence completes it. On the wire it is
    /// `409 evidence_stale` with a detail that says so (P3-L3, T16 S-2).
    TaskTampered,
    /// P4-H1 (Owner, T20 SF-1/SF-2): a stored record the operation reads is
    /// missing or does not decode or recompute, so no retry helps. On the
    /// wire it is `409 capsule_tampered`.
    Tampered,
    /// P4-H1 (review of the first P4-H1 commit): the Project's state
    /// database is not bound to it: its binding (`state_metadata`) or its
    /// migration record (`schema_migrations`) is missing, does not decode,
    /// or differs, so none of it is read as the Project's and no retry
    /// helps. On the wire it is `409 capsule_tampered`.
    UnboundState,
    /// A backing store or the Git probe failed; the operation fails closed.
    Unavailable,
    /// U9 R1 (Owner O4; ADR 0020; SF1-A): the completion's `session_id`
    /// names a Session that declared the role `sub_agent`. On the wire it
    /// is `403 role_forbidden` with the rule's fixed `detail`.
    RoleForbidden(RoleRule),
}

/// One `transitionTask` step: the target and, for `completed`, the Evidence
/// that completes the Task (P2-C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStep {
    /// The target state.
    pub target: TaskStatus,
    /// Completion Evidence, in request order; empty unless `completed`.
    pub completion_evidence_ids: Vec<EvidenceId>,
    /// U9 SF1-A (the Owner's O8 exception to R1): the Session that asks for
    /// a completion, by whose declared role it is judged; `None` when the
    /// request names none, which is not judged by role.
    pub session_id: Option<SessionId>,
}

/// What `transitionTask` to `completed` is judged from (P2-C): the Task as
/// it stands, each cited Evidence with its Snapshot, the IDs not found in
/// the Project, and one probe of the Workspace chosen as the Recovery
/// Preview chooses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionInputs {
    /// The Task.
    pub task: Task,
    /// Cited Evidence found in the Project, in request order.
    pub evidence: Vec<CompletionEvidence>,
    /// Cited IDs that name no Evidence of the Project, or Evidence that does
    /// not recompute or whose Snapshot or Evidence disagrees with the ledger
    /// (P2-K).
    pub missing: Vec<EvidenceId>,
    /// P3-D3: the Task's runs, as the Recovery Preview and the Task Capsule
    /// read them, so that a passed run a later run overrules completes
    /// nothing.
    pub history: RunHistory,
    /// P3-L L3 (T15 N3): the Task Capsule's integrity check failed: a record
    /// of the Task is missing, does not recompute, or disagrees with the
    /// ledger, or the Project's Event chain does not verify. A `tampered`
    /// Task completes on no Evidence.
    pub tampered: bool,
    /// The probe.
    pub probe: Probe,
    /// U9 SF1-A (ADR 0020): the Session the request names, as the Task
    /// Capsule's `sessions[]` lists it; `None` when the request names none
    /// or one that is no Session of the Task.
    pub asking_session: Option<Session>,
}

/// Idempotency namespace and canonical request digest of one mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// Principal, operation, and key.
    pub context: IdempotencyContext,
    /// Digest of the canonical typed request.
    pub request_digest: Sha256Digest,
}

/// Read-only identity of a Git worktree, as resolved by the Git port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorktree {
    /// Canonical common Git directory: the Project identity.
    pub repository_common_dir: PathBuf,
    /// Canonical worktree root: the Workspace identity within the Project.
    pub worktree_root: PathBuf,
    /// Canonical per-worktree Git directory.
    pub git_dir: PathBuf,
    /// Full `HEAD` object ID.
    pub head_commit: String,
    /// `branch:<name>` or `detached`.
    pub branch_or_detached_state: String,
    /// `sha1` or `sha256`.
    pub object_format: String,
}

/// The path is missing, not inside a Git worktree, or the probe failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotResolvable(pub String);

/// Port resolving a filesystem path to its Git worktree identity.
pub trait GitResolver: Send + Sync {
    /// Resolves `path` without modifying the repository.
    ///
    /// # Errors
    ///
    /// Returns [`NotResolvable`] when `path` is not inside a usable worktree.
    fn resolve(&self, path: &Path) -> Result<ResolvedWorktree, NotResolvable>;
}

/// A registered Project with its root Workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredProject {
    /// Registry record.
    pub project: Project,
    /// First Workspace registered for the Project.
    pub root_workspace: Workspace,
}

/// A Session about to be created, with its optional Task link and the
/// identity of its daemon-produced `session.started` Event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// The Session record.
    pub session: Session,
    /// Task the Session continues, in the same Project.
    pub task_id: Option<TaskId>,
    /// Identity of the `session.started` Event.
    pub started_event_id: EventId,
    /// ADR 0014 E6 (P4-F): the Pack the Session declares it continues
    /// from; with it the store appends `capsule.recovered` in the same
    /// transaction. Only with `task_id`.
    pub resumed_from: Option<ResumedFrom>,
}

/// The end of a Session: its time, the caller's reason, and the identity of
/// the daemon-produced `session.ended` Event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEnd {
    /// When the Session ends.
    pub ended_at: UtcTimestamp,
    /// Optional free text carried in the `session.ended` payload.
    pub reason: Option<String>,
    /// `completed`, or `abandoned` for `session.crashed` (ADR 0013 D6).
    pub outcome: SessionOutcome,
    /// Identity of the `session.ended` Event.
    pub ended_event_id: EventId,
    /// U9 A7 (Owner O3): who ends the Session as abandoned, as the ender
    /// reported itself; `None` otherwise (always with `completed`).
    pub ended_by: Option<SessionEndedBy>,
}

/// One snapshot-bound page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// Items in the list's stable order.
    pub items: Vec<T>,
    /// Whether more items follow the last one.
    pub has_more: bool,
    /// Latest Project Event sequence when the page was read; 0 without Events.
    pub snapshot_sequence: u64,
}

/// Persistence port for the catalog.
pub trait CatalogStore: Send + Sync {
    /// Registers (or replays) the Project of `worktree` and its root
    /// Workspace.
    ///
    /// # Errors
    ///
    /// [`CatalogError::IdempotencyConflict`] or [`CatalogError::Unavailable`].
    fn register_project(
        &self,
        worktree: &ResolvedWorktree,
        command: &Command,
    ) -> Result<RegisteredProject, CatalogError>;

    /// Reads an `ACTIVE` Project and its root Workspace.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ProjectNotFound`] or [`CatalogError::Unavailable`].
    fn project(&self, project_id: &ProjectId) -> Result<RegisteredProject, CatalogError>;

    /// Checks that `project_id` names an `ACTIVE` Project, without reading
    /// its root Workspace (P4-H1, T20 SF-1: `listTasks` does not depend on
    /// that row). The default reads [`Self::project`].
    ///
    /// # Errors
    ///
    /// [`CatalogError::ProjectNotFound`] or the error of [`Self::project`].
    fn require_project(&self, project_id: &ProjectId) -> Result<(), CatalogError> {
        self.project(project_id).map(|_| ())
    }

    /// The Project that owns `workspace_id`, for a read (`listSessions`).
    /// P4-H1 (Owner, T20 SF-1): a Workspace whose row is missing is read as
    /// one whose row does not decode, so a store may also find it by the
    /// Project's Event chain. The default reads [`Self::workspace_project`].
    ///
    /// # Errors
    ///
    /// The errors of [`Self::workspace_project`], or
    /// [`CatalogError::UnboundState`].
    fn workspace_project_for_read(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<ProjectId, CatalogError> {
        self.workspace_project(workspace_id)
    }

    /// Finds the Project's Workspace registered at `worktree_root`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Unavailable`].
    fn workspace_at(
        &self,
        project_id: &ProjectId,
        worktree_root: &Path,
    ) -> Result<Option<Workspace>, CatalogError>;

    /// Reports whether `workspace_id` belongs to the Project.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Unavailable`].
    fn has_workspace(
        &self,
        project_id: &ProjectId,
        workspace_id: &WorkspaceId,
    ) -> Result<bool, CatalogError>;

    /// Finds the `ACTIVE` Project that owns `workspace_id`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::WorkspaceNotResolved`] or [`CatalogError::Unavailable`].
    fn workspace_project(&self, workspace_id: &WorkspaceId) -> Result<ProjectId, CatalogError>;

    /// Creates (or replays) a Session in the Project's state, links it to
    /// its Task, appends `session.started`, and (P4-F) `capsule.recovered`
    /// when it declares `resumed_from`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ResourceNotFound`] for an unknown Task,
    /// [`CatalogError::ProjectWorkspaceMismatch`] for a Task scoped to another
    /// Workspace, [`CatalogError::InvalidTransition`] for a `completed` or
    /// `cancelled` Task (P4-D, Owner A1), [`CatalogError::InvalidRequest`]
    /// for a `resumed_from` above the ledger's high-water mark,
    /// [`CatalogError::IdempotencyConflict`], or [`CatalogError::Unavailable`].
    fn create_session(
        &self,
        project_id: &ProjectId,
        session: &NewSession,
        command: &Command,
    ) -> Result<Session, CatalogError>;

    /// Reads a page of the Workspace's Sessions after `after`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::InvalidRequest`] for an unknown cursor, or
    /// [`CatalogError::Unavailable`].
    fn list_sessions(
        &self,
        project_id: &ProjectId,
        workspace_id: &WorkspaceId,
        after: Option<&SessionId>,
        limit: u32,
    ) -> Result<Page<Session>, CatalogError>;

    /// Registers (or finds) a linked worktree of the Project's repository as
    /// a Workspace. Idempotent by canonical worktree path.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ProjectNotFound`] or [`CatalogError::Unavailable`].
    fn ensure_workspace(
        &self,
        project_id: &ProjectId,
        worktree: &ResolvedWorktree,
    ) -> Result<Workspace, CatalogError>;

    /// Finds the `ACTIVE` Project whose state holds `session_id`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ResourceNotFound`] or [`CatalogError::Unavailable`].
    fn session_project(&self, session_id: &SessionId) -> Result<ProjectId, CatalogError>;

    /// Finds the `ACTIVE` Project whose state holds `task_id`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ResourceNotFound`] or [`CatalogError::Unavailable`].
    fn task_project(&self, task_id: &TaskId) -> Result<ProjectId, CatalogError>;

    /// Ends (or replays ending) an active Session and appends `session.ended`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ResourceNotFound`], [`CatalogError::RevisionConflict`],
    /// [`CatalogError::InvalidTransition`], [`CatalogError::IdempotencyConflict`],
    /// or [`CatalogError::Unavailable`].
    fn end_session(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        command: &Revisioned<SessionEnd>,
        idempotency: &Command,
    ) -> Result<Session, CatalogError>;

    /// Moves (or replays moving) a Task to `command.change` and appends its
    /// `task.transitioned` Event; for `completed` the store rechecks that the
    /// Evidence exists, is the Task's, and is `PASS`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ResourceNotFound`], [`CatalogError::RevisionConflict`],
    /// [`CatalogError::InvalidTransition`], [`CatalogError::EvidenceStale`],
    /// [`CatalogError::IdempotencyConflict`], or [`CatalogError::Unavailable`].
    fn transition_task(
        &self,
        project_id: &ProjectId,
        task_id: &TaskId,
        command: &Revisioned<TaskStep>,
        idempotency: &Command,
    ) -> Result<Task, CatalogError>;

    /// Reads what a completion of `task_id` is judged from, writing nothing:
    /// `None` when `idempotency` already succeeded, so the transition is a
    /// replay that must not be judged again. The probe runs in
    /// `workspace_id` when given, else in the Workspace of the Task's latest
    /// Checkpoint Snapshot, else in the Task's scoped Workspace (the
    /// Recovery Preview's rule); without one it is unavailable.
    ///
    /// # Errors
    ///
    /// [`CatalogError::WorkspaceNotResolved`] for an unknown `workspace_id`,
    /// [`CatalogError::ProjectWorkspaceMismatch`] for one of another Project,
    /// [`CatalogError::IdempotencyConflict`], [`CatalogError::ResourceNotFound`],
    /// or [`CatalogError::Unavailable`].
    fn completion_inputs(
        &self,
        project_id: &ProjectId,
        task_id: &TaskId,
        step: &TaskStep,
        workspace_id: Option<&WorkspaceId>,
        idempotency: &Command,
    ) -> Result<Option<CompletionInputs>, CatalogError>;

    /// Creates (or replays) a Task in its Project's state.
    ///
    /// # Errors
    ///
    /// [`CatalogError::IdempotencyConflict`] or [`CatalogError::Unavailable`].
    fn create_task(&self, task: &Task, command: &Command) -> Result<Task, CatalogError>;

    /// Reads a page of the Project's Tasks after `after`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::InvalidRequest`] for an unknown cursor, or
    /// [`CatalogError::Unavailable`].
    fn list_tasks(
        &self,
        project_id: &ProjectId,
        after: Option<&TaskId>,
        limit: u32,
    ) -> Result<Page<Task>, CatalogError>;
}

impl<T: CatalogStore + ?Sized> CatalogStore for std::sync::Arc<T> {
    fn register_project(
        &self,
        worktree: &ResolvedWorktree,
        command: &Command,
    ) -> Result<RegisteredProject, CatalogError> {
        (**self).register_project(worktree, command)
    }

    fn project(&self, project_id: &ProjectId) -> Result<RegisteredProject, CatalogError> {
        (**self).project(project_id)
    }

    fn require_project(&self, project_id: &ProjectId) -> Result<(), CatalogError> {
        (**self).require_project(project_id)
    }

    fn workspace_project_for_read(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<ProjectId, CatalogError> {
        (**self).workspace_project_for_read(workspace_id)
    }

    fn workspace_at(
        &self,
        project_id: &ProjectId,
        worktree_root: &Path,
    ) -> Result<Option<Workspace>, CatalogError> {
        (**self).workspace_at(project_id, worktree_root)
    }

    fn has_workspace(
        &self,
        project_id: &ProjectId,
        workspace_id: &WorkspaceId,
    ) -> Result<bool, CatalogError> {
        (**self).has_workspace(project_id, workspace_id)
    }

    fn workspace_project(&self, workspace_id: &WorkspaceId) -> Result<ProjectId, CatalogError> {
        (**self).workspace_project(workspace_id)
    }

    fn create_session(
        &self,
        project_id: &ProjectId,
        session: &NewSession,
        command: &Command,
    ) -> Result<Session, CatalogError> {
        (**self).create_session(project_id, session, command)
    }

    fn list_sessions(
        &self,
        project_id: &ProjectId,
        workspace_id: &WorkspaceId,
        after: Option<&SessionId>,
        limit: u32,
    ) -> Result<Page<Session>, CatalogError> {
        (**self).list_sessions(project_id, workspace_id, after, limit)
    }

    fn ensure_workspace(
        &self,
        project_id: &ProjectId,
        worktree: &ResolvedWorktree,
    ) -> Result<Workspace, CatalogError> {
        (**self).ensure_workspace(project_id, worktree)
    }

    fn session_project(&self, session_id: &SessionId) -> Result<ProjectId, CatalogError> {
        (**self).session_project(session_id)
    }

    fn task_project(&self, task_id: &TaskId) -> Result<ProjectId, CatalogError> {
        (**self).task_project(task_id)
    }

    fn end_session(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        command: &Revisioned<SessionEnd>,
        idempotency: &Command,
    ) -> Result<Session, CatalogError> {
        (**self).end_session(project_id, session_id, command, idempotency)
    }

    fn transition_task(
        &self,
        project_id: &ProjectId,
        task_id: &TaskId,
        command: &Revisioned<TaskStep>,
        idempotency: &Command,
    ) -> Result<Task, CatalogError> {
        (**self).transition_task(project_id, task_id, command, idempotency)
    }

    fn completion_inputs(
        &self,
        project_id: &ProjectId,
        task_id: &TaskId,
        step: &TaskStep,
        workspace_id: Option<&WorkspaceId>,
        idempotency: &Command,
    ) -> Result<Option<CompletionInputs>, CatalogError> {
        (**self).completion_inputs(project_id, task_id, step, workspace_id, idempotency)
    }

    fn create_task(&self, task: &Task, command: &Command) -> Result<Task, CatalogError> {
        (**self).create_task(task, command)
    }

    fn list_tasks(
        &self,
        project_id: &ProjectId,
        after: Option<&TaskId>,
        limit: u32,
    ) -> Result<Page<Task>, CatalogError> {
        (**self).list_tasks(project_id, after, limit)
    }
}

/// Inputs of `createTask` after wire validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    /// Owning Project.
    pub project_id: ProjectId,
    /// Optional Workspace of the same Project.
    pub workspace_id: Option<WorkspaceId>,
    /// Non-empty title.
    pub title: String,
    /// Optional free text.
    pub description: Option<String>,
}

/// Application service for the Project/Workspace/Task/Session operations.
pub struct Catalog {
    store: Box<dyn CatalogStore>,
    git: Box<dyn GitResolver>,
}

impl Catalog {
    /// Creates the service over its ports.
    #[must_use]
    pub fn new(store: Box<dyn CatalogStore>, git: Box<dyn GitResolver>) -> Self {
        Self { store, git }
    }

    /// `registerProject`: the root must be inside a Git worktree; its common
    /// Git directory is the Project identity, so registering any path of an
    /// already registered repository returns that Project.
    ///
    /// # Errors
    ///
    /// [`CatalogError::InvalidRequest`] when `root` is not a Git worktree, or
    /// the store's error.
    pub fn register_project(
        &self,
        root: &Path,
        command: &Command,
    ) -> Result<RegisteredProject, CatalogError> {
        let worktree = self.git.resolve(root).map_err(|_| {
            CatalogError::InvalidRequest("$.root is not inside a Git worktree".to_owned())
        })?;
        self.store.register_project(&worktree, command)
    }

    /// `getProject`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ProjectNotFound`] or [`CatalogError::Unavailable`].
    pub fn get_project(&self, project_id: &ProjectId) -> Result<RegisteredProject, CatalogError> {
        self.store.project(project_id)
    }

    /// `resolveWorkspace`: finds the Workspace of the worktree containing
    /// `path`. The Project must exist; a path outside any Git worktree is not
    /// resolved; a worktree of a different repository is a mismatch. A linked
    /// worktree of the Project's own repository (same common Git directory)
    /// that is not yet registered is registered as a new Workspace, once
    /// (ADR 0011).
    ///
    /// # Errors
    ///
    /// [`CatalogError::ProjectNotFound`], [`CatalogError::WorkspaceNotResolved`],
    /// [`CatalogError::ProjectWorkspaceMismatch`], or
    /// [`CatalogError::Unavailable`].
    pub fn resolve_workspace(
        &self,
        project_id: &ProjectId,
        path: &Path,
    ) -> Result<Workspace, CatalogError> {
        let registered = self.store.project(project_id)?;
        let worktree = self
            .git
            .resolve(path)
            .map_err(|_| CatalogError::WorkspaceNotResolved)?;
        if Path::new(&registered.project.repository_common_dir) != worktree.repository_common_dir {
            return Err(CatalogError::ProjectWorkspaceMismatch);
        }
        match self
            .store
            .workspace_at(project_id, &worktree.worktree_root)?
        {
            Some(workspace) => Ok(workspace),
            None => self.store.ensure_workspace(project_id, &worktree),
        }
    }

    /// `createSession` in the Project that owns `workspace_id`, optionally
    /// continuing `task_id` of the same Project; the store appends
    /// `session.started` with the Session (ADR 0012) and, with
    /// `resumed_from` (ADR 0014 E6), `capsule.recovered`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::InvalidRequest`] for `resumed_from` without
    /// `task_id` or above the ledger's high-water mark,
    /// [`CatalogError::WorkspaceNotResolved`], [`CatalogError::ResourceNotFound`]
    /// for an unknown Task, [`CatalogError::ProjectWorkspaceMismatch`] for a
    /// Task scoped to another Workspace, [`CatalogError::InvalidTransition`]
    /// for a `completed` or `cancelled` Task (P4-D, Owner A1),
    /// [`CatalogError::InvalidRequest`] for an `agent` whose parent Session
    /// is not one of the Project (U8 A1, Owner O4),
    /// [`CatalogError::IdempotencyConflict`], or [`CatalogError::Unavailable`].
    pub fn create_session(
        &self,
        workspace_id: &WorkspaceId,
        agent_id: &AgentId,
        agent: Option<SessionAgent>,
        task_id: Option<&TaskId>,
        resumed_from: Option<ResumedFrom>,
        command: &Command,
    ) -> Result<Session, CatalogError> {
        if resumed_from.is_some() && task_id.is_none() {
            return Err(CatalogError::InvalidRequest(
                "$.resumed_from applies only to a Session with a $.task_id (ADR 0014 E6)"
                    .to_owned(),
            ));
        }
        let project_id = self.store.workspace_project(workspace_id)?;
        let session = NewSession {
            session: Session {
                session_id: SessionId::new(),
                workspace_id: workspace_id.clone(),
                agent_id: agent_id.clone(),
                started_at: UtcTimestamp::now(),
                ended_at: None,
                revision: 1,
                agent,
                ended_by: None,
            },
            task_id: task_id.cloned(),
            started_event_id: EventId::new(),
            resumed_from,
        };
        self.store.create_session(&project_id, &session, command)
    }

    /// `listSessions` of one Workspace. P4-H1 (Owner, T20 SF-1): the
    /// Workspace is found as a read finds it
    /// ([`CatalogStore::workspace_project_for_read`]).
    ///
    /// # Errors
    ///
    /// [`CatalogError::WorkspaceNotResolved`], [`CatalogError::InvalidRequest`]
    /// for an unknown cursor, [`CatalogError::Tampered`],
    /// [`CatalogError::UnboundState`], or [`CatalogError::Unavailable`].
    pub fn list_sessions(
        &self,
        workspace_id: &WorkspaceId,
        after: Option<&SessionId>,
        limit: u32,
    ) -> Result<Page<Session>, CatalogError> {
        let project_id = self.store.workspace_project_for_read(workspace_id)?;
        self.store
            .list_sessions(&project_id, workspace_id, after, limit)
    }

    /// `endSession`: an active Session ends now, recording `reason` in its
    /// `session.ended` Event, or (`abandoned`, ADR 0013 D6) in its
    /// `session.crashed` Event; it then accepts no Event.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ResourceNotFound`], [`CatalogError::RevisionConflict`],
    /// [`CatalogError::InvalidTransition`] when the Session already ended,
    /// [`CatalogError::IdempotencyConflict`], or [`CatalogError::Unavailable`].
    pub fn end_session(
        &self,
        session_id: &SessionId,
        expected_revision: u64,
        reason: Option<&str>,
        outcome: SessionOutcome,
        command: &Command,
    ) -> Result<Session, CatalogError> {
        self.end_session_by(
            session_id,
            expected_revision,
            reason,
            (outcome, None),
            command,
        )
    }

    /// [`Self::end_session`] with (U9 A7, Owner O3) who ends it as
    /// abandoned: `ended_by` goes with [`SessionOutcome::Abandoned`] only
    /// (the caller refuses it otherwise), and the store checks that its
    /// Sessions are of the Project.
    ///
    /// # Errors
    ///
    /// As [`Self::end_session`], and [`CatalogError::InvalidRequest`] for an
    /// `ended_by` whose Session or agent parent is not one of the Project.
    pub fn end_session_by(
        &self,
        session_id: &SessionId,
        expected_revision: u64,
        reason: Option<&str>,
        (outcome, ended_by): (SessionOutcome, Option<SessionEndedBy>),
        command: &Command,
    ) -> Result<Session, CatalogError> {
        let project_id = self.store.session_project(session_id)?;
        let end = Revisioned {
            expected_revision,
            change: SessionEnd {
                ended_at: UtcTimestamp::now(),
                reason: reason.map(str::to_owned),
                outcome,
                ended_event_id: EventId::new(),
                ended_by,
            },
        };
        self.store
            .end_session(&project_id, session_id, &end, command)
    }

    /// `transitionTask` along the domain Task state machine. `completed`
    /// (P2-C) needs `completion_evidence_ids`: each Evidence of the Task,
    /// `PASS`, and `verified` now by [`memprove_domain::judge_completion`], the Recovery
    /// Preview's evidence judgement, against one probe of the Workspace the
    /// Preview would choose (`workspace_id` when given). P3-L L3: a Task the
    /// Task Capsule's integrity check finds `tampered` is not completed,
    /// whatever it cites. A replay of a recorded transition is not judged
    /// again. Other targets take no Evidence, no Workspace, and no Session.
    /// U9 R1 (Owner O4; ADR 0020; SF1-A, the Owner's O8 exception): a
    /// completion whose `session_id` names a Session that declared the role
    /// `sub_agent` is refused, whatever it cites; a `session_id` must be one
    /// of the Task's Sessions; a request without one is not judged by role.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ResourceNotFound`], [`CatalogError::RevisionConflict`],
    /// [`CatalogError::InvalidTransition`], [`CatalogError::EvidenceStale`],
    /// [`CatalogError::TaskTampered`], [`CatalogError::WorkspaceNotResolved`],
    /// [`CatalogError::ProjectWorkspaceMismatch`],
    /// [`CatalogError::IdempotencyConflict`], [`CatalogError::InvalidRequest`]
    /// for a `session_id` that is no Session of the Task or comes with
    /// another target, [`CatalogError::RoleForbidden`] (U9 R1: before every
    /// other check of a completion but that one), or
    /// [`CatalogError::Unavailable`].
    pub fn transition_task(
        &self,
        task_id: &TaskId,
        expected_revision: u64,
        step: TaskStep,
        workspace_id: Option<&WorkspaceId>,
        command: &Command,
    ) -> Result<Task, CatalogError> {
        let project_id = self.store.task_project(task_id)?;
        if step.target == TaskStatus::Completed {
            if let Some(inputs) =
                self.store
                    .completion_inputs(&project_id, task_id, &step, workspace_id, command)?
            {
                // U9 R1 (Owner O4; ADR 0020; SF1-A, the Owner's O8
                // exception): the asking Session is one of the Task's, and a
                // `sub_agent` does not complete a Task, whatever runs it
                // cites. A request that names no Session is not judged.
                if step.session_id.is_some() && inputs.asking_session.is_none() {
                    return Err(CatalogError::InvalidRequest(
                        "$.session_id must name a Session of the Task".to_owned(),
                    ));
                }
                if inputs.asking_session.as_ref().is_some_and(is_sub_agent) {
                    return Err(CatalogError::RoleForbidden(RoleRule::Completion));
                }
                // The cheap refusals first, so a stale or impossible request
                // is told so before its Evidence is judged.
                if inputs.task.revision != expected_revision {
                    return Err(CatalogError::RevisionConflict);
                }
                // The state graph, then (P3-L L3) the integrity check of the
                // Task Capsule before any cited Evidence is looked at, then
                // the Evidence. ADR 0014 E3 (P4-D): the Task Capsule's
                // `complete_task` action reads the same function.
                if let Some(block) = completion_blocks(
                    task_id,
                    inputs.task.status,
                    inputs.tampered,
                    &inputs.missing,
                    &inputs.evidence,
                    &inputs.history,
                    &inputs.probe,
                )
                .into_iter()
                .next()
                {
                    return Err(match block {
                        CompletionBlock::InvalidTransition => CatalogError::InvalidTransition,
                        CompletionBlock::Tampered => CatalogError::TaskTampered,
                        CompletionBlock::Missing | CompletionBlock::NotCompleted(_) => {
                            CatalogError::EvidenceStale
                        }
                    });
                }
            }
        } else if !step.completion_evidence_ids.is_empty() || workspace_id.is_some() {
            return Err(CatalogError::InvalidRequest(
                "$.completion_evidence_ids and $.workspace_id apply only to target_state completed"
                    .to_owned(),
            ));
        } else if step.session_id.is_some() {
            return Err(CatalogError::InvalidRequest(
                "$.session_id applies only to target_state completed".to_owned(),
            ));
        }
        let step = Revisioned {
            expected_revision,
            change: step,
        };
        self.store
            .transition_task(&project_id, task_id, &step, command)
    }

    /// `createTask`: a new Task starts `proposed`; an optional Workspace must
    /// belong to the same Project.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ProjectNotFound`], [`CatalogError::ProjectWorkspaceMismatch`],
    /// [`CatalogError::IdempotencyConflict`], or [`CatalogError::Unavailable`].
    pub fn create_task(&self, new: NewTask, command: &Command) -> Result<Task, CatalogError> {
        self.store.project(&new.project_id)?;
        if let Some(workspace_id) = &new.workspace_id
            && !self.store.has_workspace(&new.project_id, workspace_id)?
        {
            return Err(CatalogError::ProjectWorkspaceMismatch);
        }
        let now = UtcTimestamp::now();
        let task = Task {
            task_id: TaskId::new(),
            project_id: new.project_id,
            workspace_id: new.workspace_id,
            title: new.title,
            description: new.description,
            status: TaskStatus::Proposed,
            created_at: now.clone(),
            updated_at: now,
            revision: 1,
            completion_evidence_ids: Vec::new(),
        };
        self.store.create_task(&task, command)
    }

    /// `listTasks` of one Project.
    ///
    /// # Errors
    ///
    /// [`CatalogError::ProjectNotFound`], [`CatalogError::InvalidRequest`] for an
    /// unknown cursor, [`CatalogError::Tampered`] for a Task row that does
    /// not decode, or [`CatalogError::Unavailable`]. The root Workspace is
    /// not read (P4-H1, T20 SF-1).
    pub fn list_tasks(
        &self,
        project_id: &ProjectId,
        after: Option<&TaskId>,
        limit: u32,
    ) -> Result<Page<Task>, CatalogError> {
        self.store.require_project(project_id)?;
        self.store.list_tasks(project_id, after, limit)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use memprove_domain::{IdempotencyKey, OperationId, PrincipalId, ProjectProvisioningState};

    use super::*;

    const COMMON: &str = "/repos/a/.git";

    fn step(target: TaskStatus) -> TaskStep {
        TaskStep {
            target,
            completion_evidence_ids: Vec::new(),
            session_id: None,
        }
    }

    fn worktree(common: &str, root: &str) -> ResolvedWorktree {
        ResolvedWorktree {
            repository_common_dir: PathBuf::from(common),
            worktree_root: PathBuf::from(root),
            git_dir: PathBuf::from(common),
            head_commit: "0".repeat(40),
            branch_or_detached_state: "branch:main".to_owned(),
            object_format: "sha1".to_owned(),
        }
    }

    struct FakeGit;

    impl GitResolver for FakeGit {
        fn resolve(&self, path: &Path) -> Result<ResolvedWorktree, NotResolvable> {
            match path.to_str() {
                Some("/repos/a" | "/repos/a/src") => Ok(worktree(COMMON, "/repos/a")),
                Some("/repos/a-linked") => Ok(worktree(COMMON, "/repos/a-linked")),
                Some("/repos/b") => Ok(worktree("/repos/b/.git", "/repos/b")),
                _ => Err(NotResolvable("not a worktree".to_owned())),
            }
        }
    }

    #[derive(Default)]
    struct FakeStore {
        registered: Mutex<Option<RegisteredProject>>,
        linked: Mutex<Vec<Workspace>>,
        tasks: Mutex<Vec<Task>>,
        sessions: Mutex<Vec<Session>>,
    }

    fn registered(store: &FakeStore) -> Option<RegisteredProject> {
        store.registered.lock().unwrap().clone()
    }

    impl CatalogStore for FakeStore {
        fn register_project(
            &self,
            worktree: &ResolvedWorktree,
            _command: &Command,
        ) -> Result<RegisteredProject, CatalogError> {
            let now = UtcTimestamp::now();
            let project_id = ProjectId::new();
            let value = RegisteredProject {
                project: Project {
                    project_id: project_id.clone(),
                    repository_common_dir: worktree.repository_common_dir.display().to_string(),
                    object_format: worktree.object_format.clone(),
                    state_db_path: "/data/state.sqlite".to_owned(),
                    provisioning_state_internal: ProjectProvisioningState::Active,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    revision: 2,
                },
                root_workspace: Workspace {
                    workspace_id: WorkspaceId::new(),
                    project_id,
                    canonical_worktree_path: worktree.worktree_root.display().to_string(),
                    git_dir: worktree.git_dir.display().to_string(),
                    head_commit: worktree.head_commit.clone(),
                    branch_or_detached_state: worktree.branch_or_detached_state.clone(),
                    created_at: now,
                    revision: 1,
                },
            };
            *self.registered.lock().unwrap() = Some(value.clone());
            Ok(value)
        }

        fn project(&self, project_id: &ProjectId) -> Result<RegisteredProject, CatalogError> {
            registered(self)
                .filter(|value| &value.project.project_id == project_id)
                .ok_or(CatalogError::ProjectNotFound)
        }

        fn workspace_at(
            &self,
            project_id: &ProjectId,
            worktree_root: &Path,
        ) -> Result<Option<Workspace>, CatalogError> {
            let root = self.project(project_id)?.root_workspace;
            Ok(std::iter::once(root)
                .chain(self.linked.lock().unwrap().iter().cloned())
                .find(|workspace| Path::new(&workspace.canonical_worktree_path) == worktree_root))
        }

        fn ensure_workspace(
            &self,
            project_id: &ProjectId,
            worktree: &ResolvedWorktree,
        ) -> Result<Workspace, CatalogError> {
            if let Some(found) = self.workspace_at(project_id, &worktree.worktree_root)? {
                return Ok(found);
            }
            let workspace = Workspace {
                workspace_id: WorkspaceId::new(),
                project_id: project_id.clone(),
                canonical_worktree_path: worktree.worktree_root.display().to_string(),
                git_dir: worktree.git_dir.display().to_string(),
                head_commit: worktree.head_commit.clone(),
                branch_or_detached_state: worktree.branch_or_detached_state.clone(),
                created_at: UtcTimestamp::now(),
                revision: 1,
            };
            self.linked.lock().unwrap().push(workspace.clone());
            Ok(workspace)
        }

        fn session_project(&self, session_id: &SessionId) -> Result<ProjectId, CatalogError> {
            let known = self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .any(|session| &session.session_id == session_id);
            registered(self)
                .filter(|_| known)
                .map(|value| value.project.project_id)
                .ok_or(CatalogError::ResourceNotFound)
        }

        fn task_project(&self, task_id: &TaskId) -> Result<ProjectId, CatalogError> {
            self.tasks
                .lock()
                .unwrap()
                .iter()
                .find(|task| &task.task_id == task_id)
                .map(|task| task.project_id.clone())
                .ok_or(CatalogError::ResourceNotFound)
        }

        fn end_session(
            &self,
            _project_id: &ProjectId,
            session_id: &SessionId,
            command: &Revisioned<SessionEnd>,
            _idempotency: &Command,
        ) -> Result<Session, CatalogError> {
            let mut sessions = self.sessions.lock().unwrap();
            let session = sessions
                .iter_mut()
                .find(|session| &session.session_id == session_id)
                .ok_or(CatalogError::ResourceNotFound)?;
            if session.revision != command.expected_revision {
                return Err(CatalogError::RevisionConflict);
            }
            if !session.is_active() {
                return Err(CatalogError::InvalidTransition);
            }
            session.ended_at = Some(command.change.ended_at.clone());
            session.ended_by.clone_from(&command.change.ended_by);
            session.revision += 1;
            Ok(session.clone())
        }

        fn transition_task(
            &self,
            _project_id: &ProjectId,
            task_id: &TaskId,
            command: &Revisioned<TaskStep>,
            _idempotency: &Command,
        ) -> Result<Task, CatalogError> {
            let mut tasks = self.tasks.lock().unwrap();
            let task = tasks
                .iter_mut()
                .find(|task| &task.task_id == task_id)
                .ok_or(CatalogError::ResourceNotFound)?;
            if task.revision != command.expected_revision {
                return Err(CatalogError::RevisionConflict);
            }
            task.status
                .check_transition(
                    command.change.target,
                    !command.change.completion_evidence_ids.is_empty(),
                )
                .map_err(|_| CatalogError::InvalidTransition)?;
            task.status = command.change.target;
            task.revision += 1;
            Ok(task.clone())
        }

        fn completion_inputs(
            &self,
            _project_id: &ProjectId,
            _task_id: &TaskId,
            _step: &TaskStep,
            _workspace_id: Option<&WorkspaceId>,
            _idempotency: &Command,
        ) -> Result<Option<CompletionInputs>, CatalogError> {
            Err(CatalogError::Unavailable)
        }

        fn has_workspace(
            &self,
            project_id: &ProjectId,
            workspace_id: &WorkspaceId,
        ) -> Result<bool, CatalogError> {
            Ok(self.project(project_id)?.root_workspace.workspace_id == *workspace_id)
        }

        fn workspace_project(&self, workspace_id: &WorkspaceId) -> Result<ProjectId, CatalogError> {
            registered(self)
                .filter(|value| value.root_workspace.workspace_id == *workspace_id)
                .map(|value| value.project.project_id)
                .ok_or(CatalogError::WorkspaceNotResolved)
        }

        fn create_session(
            &self,
            _project_id: &ProjectId,
            session: &NewSession,
            _command: &Command,
        ) -> Result<Session, CatalogError> {
            self.sessions.lock().unwrap().push(session.session.clone());
            Ok(session.session.clone())
        }

        fn list_sessions(
            &self,
            _project_id: &ProjectId,
            workspace_id: &WorkspaceId,
            _after: Option<&SessionId>,
            _limit: u32,
        ) -> Result<Page<Session>, CatalogError> {
            let items = self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .filter(|session| &session.workspace_id == workspace_id)
                .cloned()
                .collect();
            Ok(Page {
                items,
                has_more: false,
                snapshot_sequence: 0,
            })
        }

        fn create_task(&self, task: &Task, _command: &Command) -> Result<Task, CatalogError> {
            self.tasks.lock().unwrap().push(task.clone());
            Ok(task.clone())
        }

        fn list_tasks(
            &self,
            _project_id: &ProjectId,
            _after: Option<&TaskId>,
            _limit: u32,
        ) -> Result<Page<Task>, CatalogError> {
            Ok(Page {
                items: self.tasks.lock().unwrap().clone(),
                has_more: false,
                snapshot_sequence: 0,
            })
        }
    }

    fn command() -> Command {
        Command {
            context: IdempotencyContext {
                principal_id: PrincipalId::parse("principal").unwrap(),
                operation_id: OperationId::parse("registerProject").unwrap(),
                key: IdempotencyKey::parse("key-0000000000001").unwrap(),
            },
            request_digest: Sha256Digest::of_bytes(b"request"),
        }
    }

    fn catalog() -> Catalog {
        Catalog::new(Box::<FakeStore>::default(), Box::new(FakeGit))
    }

    #[test]
    fn registration_requires_a_git_worktree() {
        let catalog = catalog();
        assert_eq!(
            catalog.register_project(Path::new("/not-git"), &command()),
            Err(CatalogError::InvalidRequest(
                "$.root is not inside a Git worktree".to_owned()
            ))
        );
        let registered = catalog
            .register_project(Path::new("/repos/a/src"), &command())
            .unwrap();
        assert_eq!(
            registered.root_workspace.canonical_worktree_path,
            "/repos/a"
        );
        assert_eq!(
            catalog.get_project(&registered.project.project_id),
            Ok(registered)
        );
        assert_eq!(
            catalog.get_project(&ProjectId::new()),
            Err(CatalogError::ProjectNotFound)
        );
    }

    #[test]
    fn workspace_resolution_distinguishes_every_failure() {
        let catalog = catalog();
        let registered = catalog
            .register_project(Path::new("/repos/a"), &command())
            .unwrap();
        let project_id = &registered.project.project_id;
        assert_eq!(
            catalog.resolve_workspace(project_id, Path::new("/repos/a/src")),
            Ok(registered.root_workspace.clone())
        );
        assert_eq!(
            catalog.resolve_workspace(&ProjectId::new(), Path::new("/repos/a")),
            Err(CatalogError::ProjectNotFound)
        );
        assert_eq!(
            catalog.resolve_workspace(project_id, Path::new("/tmp/plain")),
            Err(CatalogError::WorkspaceNotResolved)
        );
        // A linked worktree of the same repository is registered once.
        let linked = catalog
            .resolve_workspace(project_id, Path::new("/repos/a-linked"))
            .unwrap();
        assert_ne!(linked.workspace_id, registered.root_workspace.workspace_id);
        assert_eq!(linked.canonical_worktree_path, "/repos/a-linked");
        assert_eq!(
            catalog.resolve_workspace(project_id, Path::new("/repos/a-linked")),
            Ok(linked)
        );
        assert_eq!(
            catalog.resolve_workspace(project_id, Path::new("/repos/b")),
            Err(CatalogError::ProjectWorkspaceMismatch)
        );
    }

    #[test]
    fn tasks_start_proposed_and_bind_only_same_project_workspaces() {
        let catalog = catalog();
        let registered = catalog
            .register_project(Path::new("/repos/a"), &command())
            .unwrap();
        let project_id = registered.project.project_id.clone();
        let new = |workspace_id| NewTask {
            project_id: project_id.clone(),
            workspace_id,
            title: "t".to_owned(),
            description: Some("d".to_owned()),
        };
        let task = catalog
            .create_task(
                new(Some(registered.root_workspace.workspace_id.clone())),
                &command(),
            )
            .unwrap();
        assert_eq!(task.status, TaskStatus::Proposed);
        assert_eq!(task.revision, 1);
        assert_eq!(task.description.as_deref(), Some("d"));
        assert_eq!(
            catalog.create_task(new(Some(WorkspaceId::new())), &command()),
            Err(CatalogError::ProjectWorkspaceMismatch)
        );
        assert_eq!(
            catalog.create_task(
                NewTask {
                    project_id: ProjectId::new(),
                    ..new(None)
                },
                &command()
            ),
            Err(CatalogError::ProjectNotFound)
        );
        assert_eq!(
            catalog.list_tasks(&project_id, None, 10).unwrap().items,
            vec![task]
        );
        assert_eq!(
            catalog.list_tasks(&ProjectId::new(), None, 10),
            Err(CatalogError::ProjectNotFound)
        );
    }

    #[test]
    fn sessions_require_a_registered_workspace() {
        let catalog = catalog();
        let registered = catalog
            .register_project(Path::new("/repos/a"), &command())
            .unwrap();
        let workspace_id = registered.root_workspace.workspace_id;
        let agent = AgentId::new();
        let session = catalog
            .create_session(&workspace_id, &agent, None, None, None, &command())
            .unwrap();
        assert_eq!(session.agent_id, agent);
        assert_eq!(session.ended_at, None);
        assert_eq!(
            catalog.create_session(&WorkspaceId::new(), &agent, None, None, None, &command()),
            Err(CatalogError::WorkspaceNotResolved)
        );
        assert_eq!(
            catalog
                .list_sessions(&workspace_id, None, 10)
                .unwrap()
                .items,
            vec![session]
        );
        assert_eq!(
            catalog.list_sessions(&WorkspaceId::new(), None, 10),
            Err(CatalogError::WorkspaceNotResolved)
        );
    }

    #[test]
    fn session_end_and_task_transition_check_revision_then_state() {
        let catalog = catalog();
        let registered = catalog
            .register_project(Path::new("/repos/a"), &command())
            .unwrap();
        let session = catalog
            .create_session(
                &registered.root_workspace.workspace_id,
                &AgentId::new(),
                None,
                None,
                None,
                &command(),
            )
            .unwrap();
        assert_eq!(
            catalog.end_session(
                &SessionId::new(),
                1,
                None,
                SessionOutcome::Completed,
                &command()
            ),
            Err(CatalogError::ResourceNotFound)
        );
        assert_eq!(
            catalog.end_session(
                &session.session_id,
                2,
                None,
                SessionOutcome::Completed,
                &command()
            ),
            Err(CatalogError::RevisionConflict)
        );
        let ended = catalog
            .end_session(
                &session.session_id,
                1,
                None,
                SessionOutcome::Completed,
                &command(),
            )
            .unwrap();
        assert!(!ended.is_active());
        assert_eq!(ended.revision, 2);
        assert_eq!(
            catalog.end_session(
                &session.session_id,
                2,
                None,
                SessionOutcome::Completed,
                &command()
            ),
            Err(CatalogError::InvalidTransition)
        );

        let task = catalog
            .create_task(
                NewTask {
                    project_id: registered.project.project_id.clone(),
                    workspace_id: None,
                    title: "t".to_owned(),
                    description: None,
                },
                &command(),
            )
            .unwrap();
        assert_eq!(
            catalog.transition_task(&TaskId::new(), 1, step(TaskStatus::Ready), None, &command()),
            Err(CatalogError::ResourceNotFound)
        );
        assert_eq!(
            catalog.transition_task(
                &task.task_id,
                1,
                step(TaskStatus::InProgress),
                None,
                &command()
            ),
            Err(CatalogError::InvalidTransition)
        );
        let ready = catalog
            .transition_task(&task.task_id, 1, step(TaskStatus::Ready), None, &command())
            .unwrap();
        assert_eq!((ready.status, ready.revision), (TaskStatus::Ready, 2));
        assert_eq!(
            catalog.transition_task(
                &task.task_id,
                1,
                step(TaskStatus::InProgress),
                None,
                &command()
            ),
            Err(CatalogError::RevisionConflict)
        );
    }
}
