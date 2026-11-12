//! Catalog ports implemented over `memprove-store-sqlite` and
//! `memprove-repo`. No SQL is written here; every database access goes
//! through a store function.
//!
//! Each call opens the Project's `state.sqlite` through the store's
//! provisioning-bound path. Registrations are serialized by the provisioner
//! lock, which also covers the root Workspace insert, so two concurrent
//! registrations of one repository cannot create two roots; the root is
//! written before the Project becomes `ACTIVE`. Every other state mutation,
//! including a linked worktree registered by `resolveWorkspace`, runs in its
//! Project's writer lane (see [`crate::writer`]).

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use memprove_domain::{
    CapsuleProbe, CheckpointId, CheckpointView, Event, EventId, EventInput, EvidenceCollector,
    EvidenceId, EvidenceKind, EvidenceSubject, LedgerVerification, Probe, ProbeObservation,
    Project, ProjectId, ProjectProvisioningState, RepositoryObservation, RepositorySnapshot,
    Session, SessionId, Task, TaskCapsuleView, TaskId, TaskLedgerView, UtcTimestamp, Workspace,
    WorkspaceId, capsule_bases, capsule_default_workspace, capsule_selected_paths, run_history,
};
use memprove_engine::{
    catalog::{
        CatalogError, CatalogStore, Command, CompletionInputs, GitResolver, NewSession,
        NotResolvable, Page, RegisteredProject, ResolvedWorktree, Revisioned, SessionEnd, TaskStep,
    },
    events::{EventError, EventStore},
    recovery::{
        CapsuleInputs, CheckpointSubmission, DecisionSubmission, EvidenceSubmission, PreviewInputs,
        RecordedCheckpoint, RecordedDecision, RecordedEvidence, RecoveryError, RecoveryStore,
    },
};
use memprove_repo::{
    GitWorkspaceResolver, RepositoryCaptureRequest, RepositoryProbe, RepositorySnapshotCollector,
    ResolvedGitWorkspace, observe_workspace, observe_workspace_bases,
    workspace_repository_identity,
};
use memprove_store_sqlite::{
    CheckpointDraft, DecisionDraft, EvidenceCapture, Idempotent, ProjectProvisioner, StatePage,
    StateStore, StoreError,
};

use crate::{
    auth::{SharedRegistry, lock},
    writer::{Lane, WriterQueue},
};

/// Resolves worktrees with the fixed-argv, read-only Git probes.
pub struct RepoGit;

impl GitResolver for RepoGit {
    fn resolve(&self, path: &Path) -> Result<ResolvedWorktree, NotResolvable> {
        GitWorkspaceResolver::resolve(path)
            .map(|resolved| ResolvedWorktree {
                repository_common_dir: resolved.repository_common_dir,
                worktree_root: resolved.worktree_root,
                git_dir: resolved.git_dir,
                head_commit: resolved.head_commit,
                branch_or_detached_state: resolved.branch_or_detached_state,
                object_format: resolved.object_format,
            })
            .map_err(|error| NotResolvable(error.to_string()))
    }
}

/// Catalog persistence over the registry and the per-Project state databases.
pub struct StoreCatalog {
    data_dir: PathBuf,
    registry: SharedRegistry,
    provisioner: Mutex<ProjectProvisioner>,
    writers: Arc<WriterQueue>,
}

impl StoreCatalog {
    /// Opens the provisioning saga over `data_dir`, next to the shared
    /// registry connection.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be opened.
    pub fn open(
        data_dir: &Path,
        registry: SharedRegistry,
        writers: Arc<WriterQueue>,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            registry,
            provisioner: Mutex::new(ProjectProvisioner::open(data_dir)?),
            writers,
        })
    }

    fn in_lane<T>(&self, project: &Project, work: impl FnOnce() -> T) -> T {
        self.writers
            .run(Lane::Project(project.project_id.to_string()), work)
    }

    fn register(
        &self,
        worktree: &ResolvedWorktree,
        command: &Command,
    ) -> Result<RegisteredProject, CatalogError> {
        let mut provisioner = self
            .provisioner
            .lock()
            .map_err(|_| CatalogError::Unavailable)?;
        let resolved = ResolvedGitWorkspace {
            repository_common_dir: worktree.repository_common_dir.clone(),
            worktree_root: worktree.worktree_root.clone(),
            git_dir: worktree.git_dir.clone(),
            head_commit: worktree.head_commit.clone(),
            branch_or_detached_state: worktree.branch_or_detached_state.clone(),
            object_format: worktree.object_format.clone(),
        };
        let records = resolved.registration_records(provisioner.data_dir(), &UtcTimestamp::now());
        // The root Workspace is written before the Project becomes ACTIVE;
        // repeating the registration completes any step a crash skipped.
        let (project, root_workspace) = provisioner
            .provision_with_root(
                &records.project,
                &records.root_workspace,
                &command.context,
                &command.request_digest,
            )
            .map_err(|error| mutation_error("provision a Project", &error))?;
        Ok(RegisteredProject {
            project,
            root_workspace,
        })
    }

    /// The `ACTIVE` Project `project_id`, for a write: a registry record
    /// that cannot be read is `Unavailable` (`503`, O7).
    fn active_project(&self, project_id: &ProjectId) -> Result<Project, CatalogError> {
        self.active_project_as(project_id, false)
    }

    /// U1 B5 (T21 V1-N-3): [`Self::active_project`] for a read. A registry
    /// record that does not decode (a revision below 1, text that is not
    /// UTF-8 or not of its form) is [`CatalogError::UnboundState`], as a
    /// state database not bound to its Project is: none of the Project is
    /// read as its ledger and no retry cures it. Before, `getProject` of a
    /// record with revision 0 answered a `500` without a body, and the
    /// other reads `503`.
    fn readable_project(&self, project_id: &ProjectId) -> Result<Project, CatalogError> {
        self.active_project_as(project_id, true)
    }

    fn active_project_as(
        &self,
        project_id: &ProjectId,
        read: bool,
    ) -> Result<Project, CatalogError> {
        let registry = lock(&self.registry).map_err(|_| CatalogError::Unavailable)?;
        match registry.get_project(project_id) {
            Ok(project)
                if project.provisioning_state_internal == ProjectProvisioningState::Active =>
            {
                Ok(project)
            }
            Ok(_) | Err(StoreError::NotFound(_)) => Err(CatalogError::ProjectNotFound),
            Err(error) if read && error.is_undecodable() => {
                eprintln!(
                    "memproved: the registry record of Project {project_id} does not decode: {error}"
                );
                Err(CatalogError::UnboundState)
            }
            Err(error) => Err(unavailable("read a Project", &error)),
        }
    }

    /// The Project's state database, opened for a write. T21 (V1-R-2): it
    /// is never created here, only by provisioning; a database that is
    /// missing, empty, or not bound to the Project is `Unavailable`.
    fn state(&self, project: &Project) -> Result<StateStore, CatalogError> {
        StateStore::open_existing(&self.data_dir, project)
            .map_err(|error| unavailable("open a Project state database", &error))
    }

    /// The Project's state database, opened for a read. P4-H1 (review of
    /// the first P4-H1 commit): a database whose binding (`state_metadata`)
    /// or migration record (`schema_migrations`) is missing, does not
    /// decode, or differs is [`CatalogError::UnboundState`], which no retry
    /// cures; any other failure is `Unavailable`. A write keeps
    /// [`Self::state`]. T21 (V1-R-2): a read creates no database; one that
    /// is missing (its file or its directory), of 0 bytes, or without any
    /// object is not bound to the Project either.
    fn read_state(&self, project: &Project) -> Result<StateStore, CatalogError> {
        StateStore::open_existing(&self.data_dir, project).map_err(|error| {
            if matches!(
                error,
                StoreError::StateBindingMismatch | StoreError::MigrationDrift { .. }
            ) || error.is_undecodable()
            {
                eprintln!(
                    "memproved: the state database of Project {} is not bound to it: {error}",
                    project.project_id
                );
                CatalogError::UnboundState
            } else {
                unavailable("open a Project state database", &error)
            }
        })
    }

    /// P4-H1: `read` on the state database of each `ACTIVE` Project, opened
    /// for a read, until one finds the record (`Some`). A Project whose
    /// state database is not bound to it is passed over; when no other
    /// Project finds the record, the answer is
    /// [`CatalogError::UnboundState`], since that one may hold it.
    fn find_for_read<T>(
        &self,
        read: impl Fn(&Project, &StateStore) -> Result<Option<T>, CatalogError>,
    ) -> Result<Option<T>, CatalogError> {
        // U1 B5: a registry record that does not decode is passed over as a
        // Project whose state database is not bound to it (before, it made
        // every lookup `503`).
        let records = {
            let registry = lock(&self.registry).map_err(|_| CatalogError::Unavailable)?;
            registry
                .list_active_project_ids()
                .map_err(|error| unavailable("list Projects", &error))?
                .into_iter()
                .map(|id| match ProjectId::parse(&id) {
                    Ok(project_id) => match registry.get_project(&project_id) {
                        Ok(project) => Ok(Some(project)),
                        Err(error) if error.is_undecodable() => Ok(None),
                        Err(error) => Err(unavailable("read a Project", &error)),
                    },
                    Err(_) => Ok(None),
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut unbound = false;
        for record in records {
            let Some(project) = record else {
                unbound = true;
                continue;
            };
            let state = match self.read_state(&project) {
                Ok(state) => state,
                Err(CatalogError::UnboundState) => {
                    unbound = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if let Some(found) = read(&project, &state)? {
                return Ok(Some(found));
            }
        }
        if unbound {
            Err(CatalogError::UnboundState)
        } else {
            Ok(None)
        }
    }

    /// The `ACTIVE` Project that holds `task_id`, for a read.
    fn task_project_for_read(&self, task_id: &TaskId) -> Result<Project, CatalogError> {
        self.find_for_read(|project, state| {
            Ok(state
                .has_task(task_id)
                .map_err(|error| unavailable("look up a Task", &error))?
                .then(|| project.clone()))
        })?
        .ok_or(CatalogError::ResourceNotFound)
    }

    /// The `ACTIVE` Project whose state database satisfies `holds`, for a
    /// write. T21 (V1-R-2): a Project whose state database is not bound to
    /// it (now also one that is missing or empty, which is no longer
    /// created) is passed over, as [`Self::find_for_read`] passes over it;
    /// when no other Project holds the record, the answer is `Unavailable`
    /// (`503` for a write), since that one may hold it.
    fn owning_project(
        &self,
        what: &str,
        holds: impl Fn(&StateStore) -> Result<bool, StoreError>,
    ) -> Result<Project, CatalogError> {
        match self.find_for_read(|project, state| {
            Ok(holds(state)
                .map_err(|error| unavailable(what, &error))?
                .then(|| project.clone()))
        }) {
            Ok(Some(project)) => Ok(project),
            Ok(None) => Err(CatalogError::ResourceNotFound),
            Err(CatalogError::UnboundState) => Err(CatalogError::Unavailable),
            Err(error) => Err(error),
        }
    }

    /// The Project with its root Workspace. P4-H1 (Owner, T20 SF-1): a root
    /// Workspace row that is missing or does not decode is a damaged record
    /// a retry cannot cure (`Tampered`), not `Unavailable`; the root is
    /// written before the Project becomes `ACTIVE`.
    fn registered(&self, project: Project) -> Result<RegisteredProject, CatalogError> {
        let root_workspace = self
            .read_state(&project)?
            .root_workspace()
            .map_err(|error| catalog_read_error("read the root Workspace", &error))?
            .ok_or_else(|| {
                eprintln!(
                    "memproved: Project {} has no root Workspace row",
                    project.project_id
                );
                CatalogError::Tampered
            })?;
        Ok(RegisteredProject {
            project,
            root_workspace,
        })
    }
}

impl StoreCatalog {
    /// The Workspace a Recovery Preview or a completion probes (ADR 0012
    /// D6): `workspace_id` when given (a Workspace of this Project), else
    /// the Workspace of the latest Checkpoint's Snapshot, else the Task's
    /// scoped Workspace; `None` when there is none.
    /// The Workspace a Preview or Capsule probes: `workspace_id` when
    /// given (it must be a Workspace of this Project), else `default`.
    fn asker_workspace(
        &self,
        state: &StateStore,
        default: Option<WorkspaceId>,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<Option<WorkspaceId>, CatalogError> {
        match workspace_id {
            Some(workspace_id) => {
                // P4-H1 (Owner, T20 SF-1): a Workspace the Project's Event
                // chain names whose row is missing is read as one whose row
                // does not decode: the asker's, with the probe unavailable.
                let found = match state.has_workspace(workspace_id) {
                    Ok(false) => state.ledger_names_workspace(workspace_id),
                    found => found,
                }
                .map_err(|error| unavailable("look up a Workspace", &error))?;
                if !found {
                    // Registered in another Project, or not at all.
                    return Err(
                        match CatalogStore::workspace_project_for_read(self, workspace_id) {
                            Ok(_) => CatalogError::ProjectWorkspaceMismatch,
                            Err(error) => error,
                        },
                    );
                }
                Ok(Some(workspace_id.clone()))
            }
            None => Ok(default),
        }
    }
}

/// The Workspace a Preview probes when the asker names none: the latest
/// Checkpoint's Snapshot Workspace, else the Task's scope.
fn preview_default(view: &TaskLedgerView) -> Option<WorkspaceId> {
    match &view.checkpoint {
        CheckpointView::Intact { snapshot, .. } => Some(snapshot.workspace_id().clone()),
        _ => view.task.workspace_id.clone(),
    }
}

/// The `repo://` paths of a Snapshot's selected files.
fn selected_paths(
    snapshot: &RepositorySnapshot,
    evidence: &[memprove_domain::Evidence],
) -> Vec<String> {
    evidence
        .iter()
        .filter(|item| snapshot.file_evidence_ids().contains(item.evidence_id()))
        .filter_map(|item| match item.subject() {
            EvidenceSubject::FileDigest { path } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

fn unavailable(action: &str, error: &StoreError) -> CatalogError {
    eprintln!("memproved: cannot {action}: {error}");
    CatalogError::Unavailable
}

fn mutation_error(action: &str, error: &StoreError) -> CatalogError {
    match error {
        StoreError::IdempotencyConflict => CatalogError::IdempotencyConflict,
        StoreError::RevisionConflict => CatalogError::RevisionConflict,
        // T21 (V1-N-5): `endSession` of a Session whose end the chain
        // already records, though its row has none, is refused like the end
        // of an ended Session.
        StoreError::InvalidTransition | StoreError::SessionNotActive => {
            CatalogError::InvalidTransition
        }
        StoreError::CompletionEvidenceRejected(_) => CatalogError::EvidenceStale,
        StoreError::NotFound("Session" | "Task") => CatalogError::ResourceNotFound,
        StoreError::SessionWorkspaceMismatch => CatalogError::ProjectWorkspaceMismatch,
        other => unavailable(action, other),
    }
}

/// `detail` of a list page whose `cursor` is well-formed but names no record.
const UNKNOWN_CURSOR: &str = "query parameter cursor does not name an existing record";

fn exists<T>(found: Result<T, StoreError>) -> Result<bool, StoreError> {
    match found {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

fn list_error(action: &str, error: &StoreError) -> CatalogError {
    match error {
        StoreError::NotFound(_) => CatalogError::InvalidRequest(UNKNOWN_CURSOR.to_owned()),
        other => catalog_read_error(action, other),
    }
}

/// P4-H1 (Owner, T20 SF-1/SF-2): a stored value a read needs that does not
/// decode or recompute is `Tampered`, which no retry cures; any other
/// failure is `Unavailable`.
fn catalog_read_error(action: &str, error: &StoreError) -> CatalogError {
    if error.is_undecodable() {
        eprintln!("memproved: cannot {action}: a stored record does not decode: {error}");
        CatalogError::Tampered
    } else {
        unavailable(action, error)
    }
}

/// The Workspace a probe reads; `None` when its row is missing or does not
/// decode (P4-C2, T19 SF-1; P4-H1, T20 SF-1): the row is not a record of the
/// Task, so the probe is unavailable, never `503`.
fn readable_workspace(
    state: &StateStore,
    workspace_id: &WorkspaceId,
) -> Result<Option<Workspace>, StoreError> {
    match state.get_workspace(workspace_id) {
        Ok(workspace) => Ok(Some(workspace)),
        Err(error @ StoreError::NotFound(_)) => {
            eprintln!("memproved: the Workspace {workspace_id} has no row: {error}");
            Ok(None)
        }
        Err(error) if error.is_undecodable() => {
            eprintln!("memproved: the Workspace {workspace_id} does not decode: {error}");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn page<T>(page: StatePage<T>) -> Page<T> {
    Page {
        items: page.items,
        has_more: page.has_more,
        snapshot_sequence: page.snapshot_sequence,
    }
}

fn outcome<T>(value: Idempotent<T>) -> T {
    match value {
        Idempotent::Created(value) | Idempotent::Replayed(value) => value,
    }
}

impl CatalogStore for StoreCatalog {
    fn register_project(
        &self,
        worktree: &ResolvedWorktree,
        command: &Command,
    ) -> Result<RegisteredProject, CatalogError> {
        self.writers
            .run(Lane::Registration, || self.register(worktree, command))
    }

    fn project(&self, project_id: &ProjectId) -> Result<RegisteredProject, CatalogError> {
        let project = self.readable_project(project_id)?;
        self.registered(project)
    }

    fn require_project(&self, project_id: &ProjectId) -> Result<(), CatalogError> {
        self.readable_project(project_id).map(|_| ())
    }

    /// P4-H1 (Owner, T20 SF-1): a Workspace this Project's Event chain names
    /// is the Project's even when its row is missing, as when its row does
    /// not decode.
    fn workspace_project_for_read(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<ProjectId, CatalogError> {
        self.find_for_read(|project, state| {
            let found = match state.has_workspace(workspace_id) {
                Ok(false) => state.ledger_names_workspace(workspace_id),
                found => found,
            }
            .map_err(|error| unavailable("look up a Workspace", &error))?;
            Ok(found.then(|| project.project_id.clone()))
        })?
        .ok_or(CatalogError::WorkspaceNotResolved)
    }

    /// P4-H1 (review of the first P4-H1 commit): the row at the path that
    /// does not decode is `Tampered`, which no retry cures.
    fn workspace_at(
        &self,
        project_id: &ProjectId,
        worktree_root: &Path,
    ) -> Result<Option<Workspace>, CatalogError> {
        let project = self.active_project(project_id)?;
        self.read_state(&project)?
            .find_workspace_by_path(&worktree_root.display().to_string())
            .map_err(|error| catalog_read_error("look up a Workspace", &error))
    }

    fn has_workspace(
        &self,
        project_id: &ProjectId,
        workspace_id: &WorkspaceId,
    ) -> Result<bool, CatalogError> {
        let project = self.active_project(project_id)?;
        self.state(&project)?
            .has_workspace(workspace_id)
            .map_err(|error| unavailable("look up a Workspace", &error))
    }

    /// The `ACTIVE` Project of a Workspace, for a write. T21 (V1-R-2): a
    /// Project whose state database is not bound to it is passed over, as
    /// [`Self::owning_project`] passes over it.
    fn workspace_project(&self, workspace_id: &WorkspaceId) -> Result<ProjectId, CatalogError> {
        match self.owning_project("look up a Workspace", |state| {
            state.has_workspace(workspace_id)
        }) {
            Ok(project) => Ok(project.project_id),
            Err(CatalogError::ResourceNotFound) => Err(CatalogError::WorkspaceNotResolved),
            Err(error) => Err(error),
        }
    }

    fn create_session(
        &self,
        project_id: &ProjectId,
        session: &NewSession,
        command: &Command,
    ) -> Result<Session, CatalogError> {
        let project = self.active_project(project_id)?;
        self.in_lane(&project, || {
            self.state(&project)?
                .create_session_resumed(
                    &session.session,
                    session.task_id.as_ref(),
                    session.resumed_from.as_ref(),
                    &session.started_event_id,
                    &command.context,
                    &command.request_digest,
                )
                .map(outcome)
                .map_err(|error| match error {
                    // ADR 0014 E6: a `resumed_from` the ledger refuses; the
                    // text is a fixed rule of the domain.
                    StoreError::RecoveryInputRejected(rule) => CatalogError::InvalidRequest(rule),
                    other => mutation_error("create a Session", &other),
                })
        })
    }

    fn list_sessions(
        &self,
        project_id: &ProjectId,
        workspace_id: &WorkspaceId,
        after: Option<&SessionId>,
        limit: u32,
    ) -> Result<Page<Session>, CatalogError> {
        let project = self.readable_project(project_id)?;
        self.read_state(&project)?
            .list_workspace_sessions_page(workspace_id, after, limit)
            .map(page)
            .map_err(|error| list_error("list Sessions", &error))
    }

    fn ensure_workspace(
        &self,
        project_id: &ProjectId,
        worktree: &ResolvedWorktree,
    ) -> Result<Workspace, CatalogError> {
        let project = self.active_project(project_id)?;
        let workspace = Workspace {
            workspace_id: WorkspaceId::new(),
            project_id: project.project_id.clone(),
            canonical_worktree_path: worktree.worktree_root.display().to_string(),
            git_dir: worktree.git_dir.display().to_string(),
            head_commit: worktree.head_commit.clone(),
            branch_or_detached_state: worktree.branch_or_detached_state.clone(),
            created_at: UtcTimestamp::now(),
            revision: 1,
        };
        self.in_lane(&project, || {
            self.state(&project)?
                .ensure_workspace(&workspace)
                .map(outcome)
                .map_err(|error| unavailable("register a linked worktree Workspace", &error))
        })
    }

    fn session_project(&self, session_id: &SessionId) -> Result<ProjectId, CatalogError> {
        self.owning_project("look up a Session", |state| {
            exists(state.get_session(session_id))
        })
        .map(|project| project.project_id)
    }

    fn task_project(&self, task_id: &TaskId) -> Result<ProjectId, CatalogError> {
        self.owning_project("look up a Task", |state| state.has_task(task_id))
            .map(|project| project.project_id)
    }

    fn end_session(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        command: &Revisioned<SessionEnd>,
        idempotency: &Command,
    ) -> Result<Session, CatalogError> {
        let project = self.active_project(project_id)?;
        self.in_lane(&project, || {
            self.state(&project)?
                .end_session_by(
                    session_id,
                    command.expected_revision,
                    &command.change.ended_at,
                    command.change.reason.as_deref(),
                    (command.change.outcome, command.change.ended_by.as_ref()),
                    &command.change.ended_event_id,
                    &idempotency.context,
                    &idempotency.request_digest,
                )
                .map(outcome)
                .map_err(|error| match error {
                    // U9 A7 (Owner O3): an `ended_by` whose Session or agent
                    // parent is not one of the Project; the text is a fixed
                    // rule.
                    StoreError::RecoveryInputRejected(rule) => CatalogError::InvalidRequest(rule),
                    other => mutation_error("end a Session", &other),
                })
        })
    }

    fn transition_task(
        &self,
        project_id: &ProjectId,
        task_id: &TaskId,
        command: &Revisioned<TaskStep>,
        idempotency: &Command,
    ) -> Result<Task, CatalogError> {
        let project = self.active_project(project_id)?;
        self.in_lane(&project, || {
            self.state(&project)?
                .transition_task(
                    task_id,
                    command.expected_revision,
                    command.change.target,
                    &command.change.completion_evidence_ids,
                    &UtcTimestamp::now(),
                    &idempotency.context,
                    &idempotency.request_digest,
                )
                .map(outcome)
                .map_err(|error| mutation_error("transition a Task", &error))
        })
    }

    /// Reads the Task's ledger view, the cited Evidence, and the asking
    /// Session, then probes the Workspace the Recovery Preview would
    /// choose, once, outside the writer lane. Writes nothing; the store
    /// rechecks inside its transaction.
    fn completion_inputs(
        &self,
        project_id: &ProjectId,
        task_id: &TaskId,
        step: &TaskStep,
        workspace_id: Option<&WorkspaceId>,
        idempotency: &Command,
    ) -> Result<Option<CompletionInputs>, CatalogError> {
        let project = self.active_project(project_id)?;
        let state = self.state(&project)?;
        if state
            .succeeded_record(&idempotency.context, &idempotency.request_digest)
            .map_err(|error| mutation_error("read a Task transition record", &error))?
            .is_some()
        {
            return Ok(None);
        }
        let view = state
            .task_ledger_view(task_id)
            .map_err(|error| mutation_error("read a Task ledger view", &error))?;
        // A write: a state database of another Project that is not bound
        // to it stays `Unavailable` here.
        let asker = self
            .asker_workspace(&state, preview_default(&view), workspace_id)
            .map_err(|error| match error {
                CatalogError::UnboundState => CatalogError::Unavailable,
                other => other,
            })?;
        let mut evidence = Vec::new();
        let mut missing = Vec::new();
        for evidence_id in &step.completion_evidence_ids {
            match state.completion_evidence(evidence_id) {
                Ok(item) => evidence.push(item),
                // Absent, not recomputing, or disagreeing with the ledger
                // (P2-K, T11 S-2): it completes nothing.
                Err(
                    StoreError::NotFound(_)
                    | StoreError::PersistedDataIntegrity(_)
                    | StoreError::InvalidPersistedValue(_),
                ) => missing.push(evidence_id.clone()),
                Err(error) => return Err(unavailable("read completion Evidence", &error)),
            }
        }
        let snapshots = evidence
            .iter()
            .filter_map(|item| {
                item.snapshot
                    .as_ref()
                    .map(|snapshot| (snapshot, &item.snapshot_evidence))
            })
            .collect::<Vec<_>>();
        let selected = snapshots
            .iter()
            .flat_map(|(snapshot, items)| selected_paths(snapshot, items))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let base = snapshots
            .first()
            .map_or_else(String::new, |(snapshot, _)| snapshot.head_oid().to_owned());
        // P4-H1: the Workspace row as the Recovery Preview reads it.
        let observation = match &asker {
            None => ProbeObservation::Unavailable,
            Some(workspace_id) => match readable_workspace(&state, workspace_id)
                .map_err(|error| unavailable("read a Workspace", &error))?
            {
                Some(workspace) => observe_paths(&project, &workspace, &selected, &base),
                None => ProbeObservation::Unavailable,
            },
        };
        // U9 SF1-A (ADR 0020; the Owner's O8 exception to R1): the asking
        // Session as the Task Capsule's `sessions[]` lists it. The ledger
        // view holds the latest Sessions; only when it leaves some out is
        // the Capsule's ledger view read for the rest.
        let asking_session = match &step.session_id {
            None => None,
            Some(session_id) => {
                let named = |session: &&Session| &session.session_id == session_id;
                match view.sessions.iter().find(named) {
                    Some(session) => Some(session.clone()),
                    None if view.sessions_truncated => state
                        .task_capsule_view(task_id)
                        .map_err(|error| mutation_error("read a Task Capsule view", &error))?
                        .sessions
                        .into_iter()
                        .map(|record| record.session)
                        .find(|session| named(&session)),
                    None => None,
                }
            }
        };
        Ok(Some(CompletionInputs {
            asking_session,
            history: run_history(&view.runs),
            // P3-L L3: the Task Capsule's integrity check, as the Recovery
            // Preview applies it (`task_integrity` and the Event chain).
            tampered: !view.damaged_records.is_empty() || !view.ledger_chain_valid,
            task: view.task,
            evidence,
            missing,
            probe: Probe {
                workspace_id: asker,
                observation,
            },
        }))
    }

    fn create_task(&self, task: &Task, command: &Command) -> Result<Task, CatalogError> {
        let project = self.active_project(&task.project_id)?;
        self.in_lane(&project, || {
            self.state(&project)?
                .create_task(task, &command.context, &command.request_digest)
                .map(outcome)
                .map_err(|error| mutation_error("create a Task", &error))
        })
    }

    fn list_tasks(
        &self,
        project_id: &ProjectId,
        after: Option<&TaskId>,
        limit: u32,
    ) -> Result<Page<Task>, CatalogError> {
        let project = self.readable_project(project_id)?;
        self.read_state(&project)?
            .list_tasks_page(after, limit)
            .map(page)
            .map_err(|error| list_error("list Tasks", &error))
    }
}

#[allow(clippy::needless_pass_by_value)] // A `map_err` callback receives the error by value.
fn event_error(error: CatalogError) -> EventError {
    match error {
        CatalogError::ProjectNotFound => EventError::ProjectNotFound,
        CatalogError::Tampered => EventError::Tampered,
        CatalogError::UnboundState => EventError::UnboundState,
        _ => EventError::Unavailable,
    }
}

/// P4-H1 (Owner, T20 SF-1/SF-2): an Event row a read needs that does not
/// decode or recompute is `Tampered`; any other failure is `Unavailable`.
fn event_read_error(action: &str, error: &StoreError) -> EventError {
    match catalog_read_error(action, error) {
        CatalogError::Tampered => EventError::Tampered,
        _ => EventError::Unavailable,
    }
}

impl EventStore for StoreCatalog {
    fn ingest(&self, input: &EventInput, command: &Command) -> Result<Event, EventError> {
        let project = self
            .active_project(&input.project_id)
            .map_err(event_error)?;
        self.in_lane(&project, || {
            self.state(&project)
                .map_err(event_error)?
                .ingest_event(input, &command.context, &command.request_digest)
                .map(outcome)
                .map_err(|error| match error {
                    StoreError::IdempotencyConflict => EventError::IdempotencyConflict,
                    StoreError::NotFound("Session") => EventError::ResourceNotFound,
                    StoreError::SessionWorkspaceMismatch => EventError::ProjectWorkspaceMismatch,
                    StoreError::SessionNotActive => EventError::InvalidTransition,
                    other => event_error(unavailable("ingest an Event", &other)),
                })
        })
    }

    fn event(&self, event_id: &EventId) -> Result<Event, EventError> {
        self.find_for_read(|_, state| match state.get_event(event_id) {
            Ok(event) => Ok(Some(event)),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(error) => Err(catalog_read_error("read an Event", &error)),
        })
        .map_err(event_error)?
        .ok_or(EventError::ResourceNotFound)
    }

    fn list(
        &self,
        project_id: &ProjectId,
        after: Option<&EventId>,
        limit: u32,
    ) -> Result<Page<Event>, EventError> {
        let project = self.readable_project(project_id).map_err(event_error)?;
        self.read_state(&project)
            .map_err(event_error)?
            .list_events_page(after, limit)
            .map(page)
            .map_err(|error| match error {
                StoreError::NotFound(_) => EventError::InvalidRequest(UNKNOWN_CURSOR.to_owned()),
                other => event_read_error("list Events", &other),
            })
    }

    fn verify_ledger(&self, project_id: &ProjectId) -> Result<LedgerVerification, EventError> {
        let project = self.readable_project(project_id).map_err(event_error)?;
        self.read_state(&project)
            .map_err(event_error)?
            .verify_ledger()
            .map_err(|error| event_error(unavailable("verify the ledger", &error)))
    }
}

/// Collector identity of the Evidence `memproved` itself observes: the
/// `git_head` and `git_worktree` Evidence of a capture.
fn git_collector() -> EvidenceCollector {
    EvidenceCollector {
        name: "continuityd-git".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn recovery_error(action: &str, error: &StoreError) -> RecoveryError {
    match error {
        StoreError::IdempotencyConflict => RecoveryError::IdempotencyConflict,
        StoreError::NotFound(
            "Session"
            | "Task"
            | "Task for checkpoint capture"
            | "RepositorySnapshot"
            | "Evidence"
            | "Checkpoint",
        ) => RecoveryError::ResourceNotFound,
        StoreError::NotFound("Workspace for checkpoint capture") => {
            RecoveryError::WorkspaceNotResolved
        }
        StoreError::SessionWorkspaceMismatch => RecoveryError::ProjectWorkspaceMismatch,
        StoreError::SessionNotBoundToTask => RecoveryError::InvalidRequest(
            "$.session_id names a Session that does not continue the Task $.task_id".to_owned(),
        ),
        // The text is a fixed rule of the store or a domain rule message.
        StoreError::RecoveryInputRejected(rule) => RecoveryError::InvalidRequest(rule.clone()),
        // P3-L3 (T16 S-1): every overruled citation, within the limit.
        StoreError::OverruledCitations(citations) => {
            RecoveryError::InvalidRequest(crate::overruled::overruled_detail(citations))
        }
        StoreError::SessionNotActive => RecoveryError::InvalidTransition,
        // U9 R1 (ADR 0020): the rule's fixed detail, nothing of the request.
        StoreError::RoleForbidden(rule) => RecoveryError::RoleForbidden(*rule),
        other => {
            eprintln!("memproved: cannot {action}: {other}");
            RecoveryError::Unavailable
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // A `map_err` callback receives the error by value.
fn recovery_unavailable(error: CatalogError) -> RecoveryError {
    match error {
        CatalogError::WorkspaceNotResolved => RecoveryError::WorkspaceNotResolved,
        CatalogError::ProjectNotFound | CatalogError::ResourceNotFound => {
            RecoveryError::ResourceNotFound
        }
        CatalogError::Tampered => RecoveryError::Tampered,
        CatalogError::UnboundState => RecoveryError::UnboundState,
        _ => RecoveryError::Unavailable,
    }
}

/// P4-H1 (Owner, T20 SF-1/SF-2): a record a read needs that does not decode
/// or recompute is `Tampered`, which no retry cures; any other failure maps
/// as [`recovery_error`] maps it.
fn recovery_read_error(action: &str, error: &StoreError) -> RecoveryError {
    if error.is_undecodable() {
        eprintln!("memproved: cannot {action}: a stored record does not decode: {error}");
        RecoveryError::Tampered
    } else {
        recovery_error(action, error)
    }
}

impl StoreCatalog {
    /// Reads from the first `ACTIVE` Project whose state holds the record
    /// ([`Self::find_for_read`]).
    fn find_in_projects<T>(
        &self,
        action: &str,
        read: impl Fn(&StateStore) -> Result<T, StoreError>,
    ) -> Result<T, RecoveryError> {
        self.find_for_read(|_, state| match read(state) {
            Ok(found) => Ok(Some(found)),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(error) => Err(catalog_read_error(action, &error)),
        })
        .map_err(recovery_unavailable)?
        .ok_or(RecoveryError::ResourceNotFound)
    }
}

impl RecoveryStore for StoreCatalog {
    fn workspace_project(&self, workspace_id: &WorkspaceId) -> Result<ProjectId, RecoveryError> {
        CatalogStore::workspace_project(self, workspace_id).map_err(recovery_unavailable)
    }

    /// Captures outside the writer lane with the read-only Git probes, then
    /// commits inside it; the store rechecks the repository before writing.
    fn capture_evidence(
        &self,
        submission: &EvidenceSubmission,
        command: &Command,
    ) -> Result<RecordedEvidence, RecoveryError> {
        let project = self
            .active_project(&submission.project_id)
            .map_err(recovery_unavailable)?;
        let state = self.state(&project).map_err(recovery_unavailable)?;
        let context = state
            .begin_checkpoint_capture(&submission.task_id, &submission.workspace_id)
            .map_err(|error| match error {
                // A Task of another Project is not found in this one.
                StoreError::ConstraintViolation(_) => RecoveryError::ResourceNotFound,
                other => recovery_error("begin an Evidence capture", &other),
            })?;
        let (command_evidence, test_evidence) = match submission.command.kind() {
            EvidenceKind::TestResult => (Vec::new(), vec![submission.command.clone()]),
            _ => (vec![submission.command.clone()], Vec::new()),
        };
        let captured = RepositorySnapshotCollector::capture(
            context.worktree_root(),
            RepositoryCaptureRequest {
                project_id: submission.project_id.clone(),
                task_id: submission.task_id.clone(),
                workspace_id: submission.workspace_id.clone(),
                snapshot_id: submission.snapshot_id.clone(),
                git_head_evidence_id: submission.git_head_evidence_id.clone(),
                git_worktree_evidence_id: submission.git_worktree_evidence_id.clone(),
                selected_files: Vec::new(),
                command_evidence,
                test_evidence,
                observed_at: submission.captured_at.clone(),
                captured_at: submission.captured_at.clone(),
                collector: git_collector(),
            },
        )
        .map_err(|error| {
            eprintln!("memproved: cannot capture the Workspace repository: {error}");
            RecoveryError::Unavailable
        })?;
        self.in_lane(&project, || {
            let mut state = self.state(&project).map_err(recovery_unavailable)?;
            state
                .commit_evidence_capture(
                    &EvidenceCapture {
                        context: &context,
                        evidence: &captured.evidence,
                        snapshot: &captured.snapshot,
                        probe: &captured.probe,
                        subject_evidence_id: submission.command.evidence_id(),
                        session_id: &submission.session_id,
                        event_id: &submission.event_id,
                        recorded_at: &submission.captured_at,
                    },
                    &command.context,
                    &command.request_digest,
                )
                .map(|record| {
                    let record = outcome(record);
                    RecordedEvidence {
                        evidence: record.evidence,
                        snapshots: record.snapshots,
                    }
                })
                .map_err(|error| recovery_error("record captured Evidence", &error))
        })
    }

    fn evidence(&self, evidence_id: &EvidenceId) -> Result<RecordedEvidence, RecoveryError> {
        self.find_in_projects("read Evidence", |state| {
            state
                .get_evidence_record(evidence_id)
                .map(|record| RecordedEvidence {
                    evidence: record.evidence,
                    snapshots: record.snapshots,
                })
        })
    }

    fn create_checkpoint(
        &self,
        submission: &CheckpointSubmission,
        command: &Command,
    ) -> Result<RecordedCheckpoint, RecoveryError> {
        let project = self
            .active_project(&submission.project_id)
            .map_err(recovery_unavailable)?;
        let draft = CheckpointDraft {
            checkpoint_id: submission.checkpoint_id.clone(),
            task_id: submission.task_id.clone(),
            session_id: submission.session_id.clone(),
            workspace_id: submission.workspace_id.clone(),
            snapshot_id: submission.snapshot_id.clone(),
            objective: submission.objective.clone(),
            verified_completed_work: submission.verified_completed_work.clone(),
            open_items: submission.open_items.clone(),
            next_action: submission.next_action.clone(),
            created_at: submission.created_at.clone(),
            edges: submission.edges.clone(),
            cited: submission.cited.clone(),
        };
        self.in_lane(&project, || {
            let mut state = self.state(&project).map_err(recovery_unavailable)?;
            let checkpoint = state
                .create_checkpoint(
                    &draft,
                    &submission.event_id,
                    &submission.created_at,
                    &command.context,
                    &command.request_digest,
                )
                .map(outcome)
                .map_err(|error| recovery_error("create a Checkpoint", &error))?;
            let snapshot = state
                .get_repository_snapshot(checkpoint.snapshot_id())
                .map_err(|error| recovery_error("read a Checkpoint Snapshot", &error))?;
            Ok(RecordedCheckpoint {
                checkpoint,
                snapshot,
            })
        })
    }

    fn checkpoint(
        &self,
        checkpoint_id: &CheckpointId,
    ) -> Result<RecordedCheckpoint, RecoveryError> {
        self.find_in_projects("read a Checkpoint", |state| {
            let checkpoint = state.get_checkpoint(checkpoint_id)?;
            let snapshot = state.get_repository_snapshot(checkpoint.snapshot_id())?;
            Ok(RecordedCheckpoint {
                checkpoint,
                snapshot,
            })
        })
    }

    /// Records the Decision in the Project that owns the Task, inside its
    /// writer lane (ADR 0013 D2, P3-C).
    fn record_decision(
        &self,
        submission: &DecisionSubmission,
        command: &Command,
    ) -> Result<RecordedDecision, RecoveryError> {
        let project = self
            .owning_project("look up a Task", |state| {
                exists(state.get_task(&submission.task_id))
            })
            .map_err(recovery_unavailable)?;
        let draft = DecisionDraft {
            decision_id: submission.decision_id.clone(),
            task_id: submission.task_id.clone(),
            session_id: submission.session_id.clone(),
            payload: submission.payload.clone(),
            edges: submission.edges.clone(),
            recorded_at: submission.recorded_at.clone(),
        };
        self.in_lane(&project, || {
            let mut state = self.state(&project).map_err(recovery_unavailable)?;
            state
                .record_decision(&draft, &command.context, &command.request_digest)
                .map(|row| RecordedDecision {
                    decision_id: row.decision_id,
                    project_sequence: row.project_sequence,
                    task_id: row.task_id,
                    session_id: row.session_id,
                    payload: row.payload,
                    supersedes: row.supersedes,
                    recorded_at: row.recorded_at,
                })
                .map_err(|error| recovery_error("record a Decision", &error))
        })
    }

    /// Reads the ledger view first (freezing N), then probes the asker's
    /// Workspace once with the read-only Git commands. Nothing is written.
    fn preview_inputs(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<PreviewInputs, RecoveryError> {
        let project = self
            .task_project_for_read(task_id)
            .map_err(recovery_unavailable)?;
        let state = self.read_state(&project).map_err(recovery_unavailable)?;
        let view = state
            .task_ledger_view(task_id)
            .map_err(|error| recovery_read_error("read a Recovery Preview ledger view", &error))?;
        let asker = self
            .asker_workspace(&state, preview_default(&view), workspace_id)
            .map_err(|error| match error {
                CatalogError::ProjectWorkspaceMismatch => RecoveryError::ProjectWorkspaceMismatch,
                other => recovery_unavailable(other),
            })?;
        let observation = match &asker {
            None => ProbeObservation::Unavailable,
            Some(workspace_id) => match probed_workspace(&state, workspace_id)? {
                Some(workspace) => observe(&project, &workspace, &view),
                None => ProbeObservation::Unavailable,
            },
        };
        Ok(PreviewInputs {
            view,
            probe: Probe {
                workspace_id: asker,
                observation,
            },
        })
    }

    /// Reads the whole ledger view first (freezing N), then probes the
    /// asker's Workspace once: the selected files of every Snapshot a claim
    /// is bound to, and at most 16 base commits. Nothing is written.
    fn capsule_inputs(
        &self,
        task_id: &TaskId,
        workspace_id: Option<&WorkspaceId>,
    ) -> Result<CapsuleInputs, RecoveryError> {
        let project = self
            .task_project_for_read(task_id)
            .map_err(recovery_unavailable)?;
        let state = self.read_state(&project).map_err(recovery_unavailable)?;
        let view = state
            .task_capsule_view(task_id)
            .map_err(|error| recovery_read_error("read a Task Capsule ledger view", &error))?;
        let asker = self
            .asker_workspace(&state, capsule_default_workspace(&view), workspace_id)
            .map_err(|error| match error {
                CatalogError::ProjectWorkspaceMismatch => RecoveryError::ProjectWorkspaceMismatch,
                other => recovery_unavailable(other),
            })?;
        let probe = match &asker {
            None => CapsuleProbe {
                probe: Probe {
                    workspace_id: None,
                    observation: ProbeObservation::Unavailable,
                },
                changed_since: std::collections::BTreeMap::new(),
            },
            Some(workspace_id) => match probed_workspace(&state, workspace_id)? {
                Some(workspace) => observe_capsule(&project, &workspace, &view),
                None => CapsuleProbe {
                    probe: Probe {
                        workspace_id: Some(workspace_id.clone()),
                        observation: ProbeObservation::Unavailable,
                    },
                    changed_since: std::collections::BTreeMap::new(),
                },
            },
        };
        Ok(CapsuleInputs { view, probe })
    }
}

/// The Workspace a recovery document probes; `None` when its row is missing
/// or does not decode ([`readable_workspace`]).
fn probed_workspace(
    state: &StateStore,
    workspace_id: &WorkspaceId,
) -> Result<Option<Workspace>, RecoveryError> {
    readable_workspace(state, workspace_id)
        .map_err(|error| recovery_error("read a Workspace", &error))
}

/// The probe P of a Recovery Preview: the Snapshot's selected files, the
/// changes since its commit, and whether the resolved repository and
/// worktree are the registered ones. A probe that cannot run is
/// `Unavailable`, never an error.
fn observe(project: &Project, workspace: &Workspace, view: &TaskLedgerView) -> ProbeObservation {
    let (selected, base) = match &view.checkpoint {
        CheckpointView::Intact {
            snapshot, evidence, ..
        } => (
            selected_paths(snapshot, evidence),
            snapshot.head_oid().to_owned(),
        ),
        _ => (Vec::new(), String::new()),
    };
    observe_paths(project, workspace, &selected, &base)
}

/// The probe P of a Task Capsule (ADR 0013 D4): the selected files of every
/// Snapshot a claim is bound to and the changes since each probed base
/// commit, at most 16. A probe that cannot run is `Unavailable`.
fn observe_capsule(
    project: &Project,
    workspace: &Workspace,
    view: &TaskCapsuleView,
) -> CapsuleProbe {
    let selected = capsule_selected_paths(view);
    let bases = capsule_bases(view).probed;
    let root = Path::new(&workspace.canonical_worktree_path);
    let unavailable = |error: &dyn std::fmt::Display| {
        eprintln!(
            "memproved: Task Capsule probe of Workspace {} is unavailable: {error}",
            workspace.workspace_id
        );
        CapsuleProbe {
            probe: Probe {
                workspace_id: Some(workspace.workspace_id.clone()),
                observation: ProbeObservation::Unavailable,
            },
            changed_since: std::collections::BTreeMap::new(),
        }
    };
    let observed = match observe_workspace_bases(root, &selected, &bases) {
        Ok(observed) => observed,
        Err(error) => return unavailable(&error),
    };
    CapsuleProbe {
        probe: Probe {
            workspace_id: Some(workspace.workspace_id.clone()),
            observation: ProbeObservation::Observed(repository_observation(
                project,
                workspace,
                &observed.probe,
                observed.selected_files,
                observed.untracked_paths,
                None,
            )),
        },
        changed_since: observed.changed_since,
    }
}

/// What the probe saw, compared with the registered Project and Workspace.
fn repository_observation(
    project: &Project,
    workspace: &Workspace,
    probe: &RepositoryProbe,
    selected_files: std::collections::BTreeMap<String, Option<memprove_domain::Sha256Digest>>,
    untracked_paths: std::collections::BTreeSet<String>,
    changed_since_base: Option<std::collections::BTreeSet<String>>,
) -> RepositoryObservation {
    let root = Path::new(&workspace.canonical_worktree_path);
    let common_dir = Path::new(&project.repository_common_dir);
    let expected = workspace_repository_identity(
        common_dir,
        root,
        Path::new(&workspace.git_dir),
        &project.object_format,
    );
    RepositoryObservation {
        same_repository: probe.repository_common_dir() == common_dir,
        same_workspace: probe.repository_identity() == &expected,
        repository_identity: probe.repository_identity().clone(),
        head_oid: probe.head_oid().to_owned(),
        branch: probe.branch().map(str::to_owned),
        status_porcelain_v2: probe.status_porcelain_v2().to_owned(),
        index_digest: probe.index_digest().clone(),
        tracked_worktree_digest: probe.tracked_worktree_digest().clone(),
        capture_manifest_digest: probe.capture_manifest_digest().clone(),
        selected_files,
        untracked_paths,
        changed_since_base,
    }
}

/// One read-only probe of `workspace`: the `selected` `repo://` files and
/// the changes since `base`. A probe that cannot run is `Unavailable`.
fn observe_paths(
    project: &Project,
    workspace: &Workspace,
    selected: &[String],
    base: &str,
) -> ProbeObservation {
    let root = Path::new(&workspace.canonical_worktree_path);
    let observed = match observe_workspace(root, selected, base) {
        Ok(observed) => observed,
        Err(error) => {
            eprintln!(
                "memproved: Recovery Preview probe of Workspace {} is unavailable: {error}",
                workspace.workspace_id
            );
            return ProbeObservation::Unavailable;
        }
    };
    ProbeObservation::Observed(repository_observation(
        project,
        workspace,
        &observed.probe,
        observed.selected_files,
        observed.untracked_paths,
        observed.changed_since_base,
    ))
}
