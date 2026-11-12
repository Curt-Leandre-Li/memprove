use std::path::{Path, PathBuf};

use memprove_domain::{
    AgentId, CanonicalJson, CorrelationId, EVENT_DIGEST_VERSION, EVENT_ENVELOPE_VERSION, Event,
    EventId, EventInput, EventProducer, EvidenceId, IdempotencyContext, PrivacyClass, Project,
    ProjectId, ProjectProvisioningState, ResumedFrom, RunStatus, SESSION_CRASHED_ENDED_BY_SCHEMA,
    SESSION_STARTED_AGENT_SCHEMA, Session, SessionAgent, SessionEndedBy, SessionId,
    SessionLinkRefusal, SessionOutcome, Sha256Digest, Task, TaskId, TaskStatus, UtcTimestamp,
    Workspace, WorkspaceId, session_link_refusals, verify_event_chain,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{Idempotent, StoreError, map_sqlite_error, vfs::CapabilityRoot};

mod capsule;
mod carry_over;
mod decision;
mod ledger;
mod membership;
mod preview;
mod recovery;

pub use decision::{DecisionDraft, DecisionRow};
pub use recovery::{CheckpointDraft, EvidenceCapture, EvidenceRecord};

struct StateMigration {
    id: u64,
    name: &'static str,
    sql: &'static str,
}

const STATE_MIGRATIONS: &[StateMigration] = &[
    StateMigration {
        id: 1,
        name: "0001_state",
        sql: include_str!("../migrations/state/0001_state.sql"),
    },
    StateMigration {
        id: 2,
        name: "0002_event_immutability",
        sql: include_str!("../migrations/state/0002_event_immutability.sql"),
    },
    StateMigration {
        id: 3,
        name: "0003_recovery_kernel",
        sql: include_str!("../migrations/state/0003_recovery_kernel.sql"),
    },
    StateMigration {
        id: 4,
        name: "0004_recovery_capture_consistency",
        sql: include_str!("../migrations/state/0004_recovery_capture_consistency.sql"),
    },
    StateMigration {
        id: 5,
        name: "0005_task_workspace_description",
        sql: include_str!("../migrations/state/0005_task_workspace_description.sql"),
    },
    StateMigration {
        id: 6,
        name: "0006_event_envelope",
        sql: include_str!("../migrations/state/0006_event_envelope.sql"),
    },
    StateMigration {
        id: 7,
        name: "0007_events_nullable_session",
        sql: include_str!("../migrations/state/0007_events_nullable_session.sql"),
    },
    StateMigration {
        id: 8,
        name: "0008_session_agent",
        sql: include_str!("../migrations/state/0008_session_agent.sql"),
    },
    StateMigration {
        id: 9,
        name: "0009_session_ended_by",
        sql: include_str!("../migrations/state/0009_session_ended_by.sql"),
    },
];

/// Columns read for one Event, in [`event_from_row`] order. The 0001
/// `occurred_at` column is deliberately absent: envelope rows expose only the
/// producer-declared `producer_occurred_at` (ADR 0010 Q1 = B).
const EVENT_COLUMNS: &str = "event_id, project_id, workspace_id, session_id, sequence,
    project_sequence, event_type, payload_schema, payload, payload_sha256,
    producer_occurred_at, recorded_at, producer_name, producer_version, producer_sequence,
    correlation_id, causation_id, privacy_class, previous_event_sha256, event_sha256,
    event_digest_version, envelope_version";

/// Columns read for one Session, in [`session_from_row`] order; then the
/// agent of migration 0008 (U8 A1) and the ender of migration 0009 (U9 A7),
/// three each.
const SESSION_COLUMNS: &str = "session_id, workspace_id, agent_id, started_at, ended_at, revision,
    agent_name, agent_role, parent_session_id,
    ended_by_session_id, ended_by_agent_name, ended_by_agent_role";

/// Columns read for one Task, in [`task_from_row`] order. The completion
/// Evidence IDs are read from the Task's `task.transitioned` Event into
/// `completed` (P2-C): the Event is the only record of them. Only an Event
/// without a Session counts, which the `events` triggers allow only for
/// `memproved`'s own Events (migration 0007).
const TASK_COLUMNS: &str = "task_id, project_id, title, status, created_at, updated_at, revision,
    workspace_id, description,
    (SELECT json_extract(completion.payload, '$.completion_evidence_ids')
     FROM events AS completion
     WHERE completion.event_type = 'task.transitioned'
       AND completion.session_id IS NULL
       AND completion.producer_name = 'continuityd'
       AND json_extract(completion.payload, '$.task_id') = tasks.task_id
       AND json_extract(completion.payload, '$.to') = 'completed'
     ORDER BY completion.project_sequence DESC
     LIMIT 1)";

/// Database-bound inputs frozen before repository checkpoint capture begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointCaptureContext {
    pub(crate) project_id: memprove_domain::ProjectId,
    pub(crate) task_id: TaskId,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) repository_common_dir_sha256: Sha256Digest,
    pub(crate) worktree_root: PathBuf,
    pub(crate) git_dir: PathBuf,
    pub(crate) project_event_sequence: u64,
}

impl CheckpointCaptureContext {
    /// Returns the exact database-bound worktree root that must be captured.
    #[must_use]
    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    /// Returns the frozen count of immutable Events across every Session in the Project.
    #[must_use]
    pub const fn project_event_sequence(&self) -> u64 {
        self.project_event_sequence
    }
}

/// One keyset page read inside a single read transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePage<T> {
    /// Items in the list's stable total order.
    pub items: Vec<T>,
    /// Whether at least one further item follows the last returned item.
    pub has_more: bool,
    /// Latest Project Event sequence when the page was read; 0 without Events.
    pub snapshot_sequence: u64,
}

/// Per-project state database.
pub struct StateStore {
    connection: Connection,
    database_path: PathBuf,
    project_state: ProjectProvisioningState,
    capability_root: CapabilityRoot,
}

/// Whether opening a state database may create it (T21, V1-R-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// The provisioning saga (and tests): a missing database, or one with no
    /// object, is created, migrated, and bound to the Project.
    MayCreate,
    /// An `ACTIVE` Project's database, which provisioning created: it is
    /// opened only when it exists and holds a migration record, so a
    /// missing, empty, or object-less database is
    /// [`StoreError::StateBindingMismatch`] and nothing is written.
    Existing,
}

impl StateStore {
    /// Opens, migrates, and verifies one registry-bound state database,
    /// creating it when it is missing (the provisioning saga's step, and
    /// tests). `memproved` opens an `ACTIVE` Project's database with
    /// [`Self::open_existing`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for I/O, migration, pragma, or binding failures.
    pub fn open(data_dir: &Path, project: &Project) -> Result<Self, StoreError> {
        let capability_root = CapabilityRoot::open(data_dir)?;
        Self::open_with_root(capability_root, project)
    }

    /// T21 (V1-R-2): opens, migrates, and verifies the existing state
    /// database of a Project that provisioning created, without creating
    /// one. A missing file or directory, a file of 0 bytes, or a database
    /// with no object is [`StoreError::StateBindingMismatch`], and nothing is
    /// created or written; a database with a migration record is opened,
    /// upgraded, and checked as by [`Self::open`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for I/O, migration, pragma, or binding failures.
    pub fn open_existing(data_dir: &Path, project: &Project) -> Result<Self, StoreError> {
        let capability_root = CapabilityRoot::open(data_dir)?;
        Self::open_existing_with_root(capability_root, project)
    }

    pub(crate) fn open_with_root(
        capability_root: CapabilityRoot,
        project: &Project,
    ) -> Result<Self, StoreError> {
        Self::open_bound(capability_root, project, Opening::MayCreate)
    }

    pub(crate) fn open_existing_with_root(
        capability_root: CapabilityRoot,
        project: &Project,
    ) -> Result<Self, StoreError> {
        Self::open_bound(capability_root, project, Opening::Existing)
    }

    fn open_bound(
        capability_root: CapabilityRoot,
        project: &Project,
        opening: Opening,
    ) -> Result<Self, StoreError> {
        capability_root.verify_identity()?;
        #[cfg(test)]
        capability_root.after_identity_verification_for_test();
        let project_directory = format!("projects/{}", project.project_id.as_str());
        let relative_database_path = format!("{project_directory}/state.sqlite");
        let database_path = capability_root.absolute_path(&relative_database_path)?;
        if Path::new(&project.state_db_path) != database_path {
            return Err(StoreError::StateBindingMismatch);
        }
        let existed = capability_root.exists(&relative_database_path)?;
        if opening == Opening::Existing && !existed {
            return Err(StoreError::StateBindingMismatch);
        }
        let opened: Result<Connection, StoreError> = (|| {
            let mut connection = match opening {
                Opening::MayCreate => {
                    capability_root.ensure_dir_all(&project_directory)?;
                    capability_root.open_database(&relative_database_path)?
                }
                Opening::Existing => {
                    capability_root.open_existing_database(&relative_database_path)?
                }
            };
            configure(&connection)?;
            migrate_and_bind(&mut connection, project, opening)?;
            capability_root.verify_identity()?;
            Ok(connection)
        })();
        match opened {
            Ok(connection) => Ok(Self {
                connection,
                database_path,
                project_state: project.provisioning_state_internal,
                capability_root,
            }),
            Err(error) => {
                if !existed {
                    let _ = capability_root.cleanup_new_database(&relative_database_path);
                }
                Err(error)
            }
        }
    }

    pub(crate) fn exists_with_root(
        capability_root: &CapabilityRoot,
        project: &Project,
    ) -> Result<bool, StoreError> {
        capability_root.verify_identity()?;
        let relative_database_path =
            format!("projects/{}/state.sqlite", project.project_id.as_str());
        let expected = capability_root.absolute_path(&relative_database_path)?;
        if Path::new(&project.state_db_path) != expected {
            return Err(StoreError::StateBindingMismatch);
        }
        capability_root.exists(&relative_database_path)
    }

    pub(crate) fn cleanup_new_database_with_root(
        capability_root: &CapabilityRoot,
        project: &Project,
    ) -> Result<(), StoreError> {
        let relative_database_path =
            format!("projects/{}/state.sqlite", project.project_id.as_str());
        let expected = capability_root.absolute_path(&relative_database_path)?;
        if Path::new(&project.state_db_path) != expected {
            return Err(StoreError::StateBindingMismatch);
        }
        capability_root.cleanup_new_database(&relative_database_path)
    }

    /// Returns the exact project state database path.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Persists one root or linked worktree.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project is inactive or a constraint fails.
    pub fn create_workspace(&mut self, workspace: &Workspace) -> Result<(), StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction
            .execute(
                "INSERT INTO workspaces (
                    workspace_id, project_id, canonical_worktree_path, git_dir,
                    head_commit, branch_or_detached_state, created_at, revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    workspace.workspace_id.as_str(),
                    workspace.project_id.as_str(),
                    workspace.canonical_worktree_path,
                    workspace.git_dir,
                    workspace.head_commit,
                    workspace.branch_or_detached_state,
                    workspace.created_at.as_str(),
                    workspace.revision
                ],
            )
            .map_err(map_sqlite_error)?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(())
    }

    /// Reads one workspace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the Workspace is absent or cannot be decoded.
    pub fn get_workspace(&self, workspace_id: &WorkspaceId) -> Result<Workspace, StoreError> {
        self.connection
            .query_row(
                "SELECT workspace_id, project_id, canonical_worktree_path, git_dir,
                        head_commit, branch_or_detached_state, created_at, revision
                 FROM workspaces WHERE workspace_id = ?1",
                [workspace_id.as_str()],
                workspace_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound("Workspace"))
    }

    /// Atomically creates or replays one Task in a subject-scoped namespace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for inactive projects, conflicts, constraints, or database failures.
    pub fn create_task(
        &mut self,
        proposed: &Task,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Task>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(record) = task_idempotency(&transaction, context)? {
            let outcome =
                recover_task_idempotency(&transaction, proposed, context, request_digest, record)?;
            capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(outcome);
        }

        let existing = task_by_id(&transaction, &proposed.task_id)?;
        let outcome = if let Some(task) = existing {
            Idempotent::Replayed(task)
        } else {
            insert_task(&transaction, proposed)?;
            Idempotent::Created(proposed.clone())
        };
        transaction
            .execute(
                "INSERT INTO state_idempotency_records (
                    principal_id, operation_id, idempotency_key,
                    canonical_request_sha256, state, result_record_type,
                    result_record_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'SUCCEEDED', 'Task', ?5, ?6, ?6)",
                params![
                    context.principal_id.as_str(),
                    context.operation_id.as_str(),
                    context.key.as_str(),
                    request_digest.as_str(),
                    proposed.task_id.as_str(),
                    proposed.created_at.as_str()
                ],
            )
            .map_err(map_sqlite_error)?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Reads one Task.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the Task is absent or cannot be decoded.
    pub fn get_task(&self, task_id: &TaskId) -> Result<Task, StoreError> {
        task_by_id_connection(&self.connection, task_id)?.ok_or(StoreError::NotFound("Task"))
    }

    /// Whether this state holds a Task row for `task_id`, without decoding
    /// it (P4-C2: the read-only recovery documents find the Task's Project
    /// even when its row does not decode, and report that row as damaged).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query fails.
    pub fn has_task(&self, task_id: &TaskId) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE task_id = ?1)",
            [task_id.as_str()],
            |row| row.get(0),
        )?)
    }

    /// Lists Tasks by an explicit stable total order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query or row decoding fails.
    pub fn list_tasks(&self) -> Result<Vec<Task>, StoreError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks ORDER BY created_at, task_id"
        ))?;
        statement
            .query_map([], task_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Persists a Session with direct `workspace_id` and `agent_id` fields.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project is inactive or a constraint fails.
    pub fn create_session(&mut self, session: &Session) -> Result<(), StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_session(&transaction, session)?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(())
    }

    /// Reads one Session.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the Session is absent or cannot be decoded.
    pub fn get_session(&self, session_id: &SessionId) -> Result<Session, StoreError> {
        self.connection
            .query_row(
                "SELECT session_id, workspace_id, agent_id, started_at, ended_at, revision,
                agent_name, agent_role, parent_session_id,
                ended_by_session_id, ended_by_agent_name, ended_by_agent_role
                 FROM sessions WHERE session_id = ?1",
                [session_id.as_str()],
                session_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound("Session"))
    }

    /// Lists Sessions by an explicit stable total order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query or row decoding fails.
    pub fn list_sessions(&self) -> Result<Vec<Session>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT session_id, workspace_id, agent_id, started_at, ended_at, revision,
                agent_name, agent_role, parent_session_id,
                ended_by_session_id, ended_by_agent_name, ended_by_agent_role
             FROM sessions ORDER BY started_at, session_id",
        )?;
        statement
            .query_map([], session_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Links one Session to at most one Task and appends its
    /// `task.session_linked` Event on the Session (P2-C).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project is inactive, the Task or
    /// Session is unknown, the Session has ended, the Task is `completed` or
    /// `cancelled` ([`StoreError::InvalidTransition`], P4-D), or a
    /// constraint fails.
    pub fn link_session_to_task(
        &mut self,
        task_id: &TaskId,
        session_id: &SessionId,
        linked_at: &UtcTimestamp,
    ) -> Result<(), StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let task = task_by_id(&transaction, task_id)?.ok_or(StoreError::NotFound("Task"))?;
        let session =
            session_by_id(&transaction, session_id)?.ok_or(StoreError::NotFound("Session"))?;
        // P4-D (Owner A1): a finished Task takes no Session, linked later
        // either.
        if let Some(refusal) = session_link_refusals(&task, None).into_iter().next() {
            return Err(link_refusal(refusal));
        }
        insert_session_link(&transaction, &task, &session, linked_at)?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(())
    }

    /// Lists the Sessions linked to a Task in link order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query or ID decoding fails.
    pub fn list_task_sessions(&self, task_id: &TaskId) -> Result<Vec<SessionId>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT session_id FROM task_sessions
             WHERE task_id = ?1 ORDER BY linked_at, session_id",
        )?;
        statement
            .query_map([task_id.as_str()], |row| {
                SessionId::parse(&row.get::<_, String>(0)?)
                    .map_err(|error| invalid_value(0, error.to_string()))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Allocates and appends one immutable, chain-linked Event in its own
    /// write transaction. See [`append_event_in`], the only Event insert.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project is inactive, the ledger holds
    /// pre-envelope rows, or the Event violates a constraint.
    pub fn append_event(&mut self, input: &EventInput) -> Result<Event, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event = append_event_in(&transaction, input)?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(event)
    }

    /// Atomically appends or replays one ingested Event in the Project's
    /// idempotency namespace. The digest is always computed here; callers
    /// cannot supply one.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::IdempotencyConflict`] for a reused key with a
    /// different request or an `event_id` already recorded,
    /// [`StoreError::NotFound`] for an unknown Session,
    /// [`StoreError::SessionWorkspaceMismatch`] when the Workspace is not the
    /// Session's, or another [`StoreError`].
    pub fn ingest_event(
        &mut self,
        input: &EventInput,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Event>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((digest, state, record_id)) = task_idempotency(&transaction, context)? {
            if digest != request_digest.as_str() {
                return Err(StoreError::IdempotencyConflict);
            }
            if state != "SUCCEEDED" {
                return Err(StoreError::InvalidPersistedValue(format!(
                    "Event idempotency record is {state}"
                )));
            }
            let event_id = record_id
                .as_deref()
                .map(EventId::parse)
                .transpose()
                .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))?
                .ok_or_else(|| {
                    StoreError::InvalidPersistedValue(
                        "SUCCEEDED Event idempotency record has no result ID".to_owned(),
                    )
                })?;
            let existing = event_by_id(&transaction, &event_id)?
                .ok_or(StoreError::NotFound("Event for idempotency record"))?;
            capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(existing));
        }
        // A client Event always names its Session (ADR 0010 D1); only
        // `memproved` writes Events without one.
        let session_id = input.session_id.as_ref().ok_or_else(|| {
            StoreError::ConstraintViolation("an ingested Event must name its Session".to_owned())
        })?;
        let session_workspace = transaction
            .query_row(
                "SELECT workspace_id FROM sessions WHERE session_id = ?1",
                [session_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("Session"))?;
        if session_workspace != input.workspace_id.as_str() {
            return Err(StoreError::SessionWorkspaceMismatch);
        }
        let known: bool = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE event_id = ?1)",
            [input.event_id.as_str()],
            |row| row.get(0),
        )?;
        if known {
            return Err(StoreError::IdempotencyConflict);
        }
        let event = append_event_in(&transaction, input)?;
        transaction
            .execute(
                "INSERT INTO state_idempotency_records (
                    principal_id, operation_id, idempotency_key,
                    canonical_request_sha256, state, result_record_type,
                    result_record_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'SUCCEEDED', 'Event', ?5, ?6, ?6)",
                params![
                    context.principal_id.as_str(),
                    context.operation_id.as_str(),
                    context.key.as_str(),
                    request_digest.as_str(),
                    event.event_id().as_str(),
                    event.recorded_at().as_str()
                ],
            )
            .map_err(map_sqlite_error)?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(event))
    }

    /// Reads one Event, recomputing its digests.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the Event is absent, predates the envelope,
    /// or fails digest validation.
    pub fn get_event(&self, event_id: &EventId) -> Result<Event, StoreError> {
        event_by_id(&self.connection, event_id)?.ok_or(StoreError::NotFound("Event"))
    }

    /// Lists one Session's Events in Session sequence order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query or Event validation fails.
    pub fn list_events(&self, session_id: &SessionId) -> Result<Vec<Event>, StoreError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM events WHERE session_id = ?1 ORDER BY sequence, event_id"
        ))?;
        statement
            .query_map([session_id.as_str()], event_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Reads one page of the Project's Events after `after` in
    /// `project_sequence` order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] when `after` names no Event, or another
    /// [`StoreError`] when the read or Event validation fails.
    pub fn list_events_page(
        &self,
        after: Option<&EventId>,
        limit: u32,
    ) -> Result<StatePage<Event>, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let after_sequence = match after {
            Some(after) => Some(
                transaction
                    .query_row(
                        "SELECT project_sequence FROM events WHERE event_id = ?1",
                        [after.as_str()],
                        |row| row.get::<_, Option<u64>>(0),
                    )
                    .optional()?
                    .ok_or(StoreError::NotFound("Event cursor"))?
                    .ok_or_else(pre_envelope_rows)?,
            ),
            None => None,
        };
        let mut statement = transaction.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM events
             WHERE ?1 IS NULL OR project_sequence > ?1
             ORDER BY project_sequence
             LIMIT ?2"
        ))?;
        let items = statement
            .query_map(
                params![after_sequence, i64::from(limit) + 1],
                event_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let page = state_page(items, limit, latest_project_sequence(&transaction)?);
        transaction.commit()?;
        Ok(page)
    }

    /// Recomputes and verifies the Project's complete Event chain.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidPersistedValue`] when the ledger holds
    /// pre-envelope rows or any sequence, link, or digest is invalid.
    pub fn verify_event_chain(&self) -> Result<(), StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        reject_pre_envelope_rows(&transaction)?;
        let mut statement = transaction.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM events ORDER BY project_sequence"
        ))?;
        let events = statement
            .query_map([], event_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        transaction.commit()?;
        verify_event_chain(events.iter())
            .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))
    }

    /// Inserts `workspace` unless a Workspace with the same canonical worktree
    /// path exists, in which case that Workspace is returned unchanged. A new
    /// Workspace gets its `workspace.registered` Event, without a Session,
    /// in the same transaction (ADR 0012 D7, P2-C).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project is inactive or a constraint fails.
    pub fn ensure_workspace(
        &mut self,
        workspace: &Workspace,
    ) -> Result<Idempotent<Workspace>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = workspace_by_path(&transaction, &workspace.canonical_worktree_path)?
        {
            capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(existing));
        }
        insert_workspace(&transaction, workspace)?;
        append_daemon_event_in(
            &transaction,
            &DaemonEvent {
                event_id: &EventId::new(),
                workspace_id: &workspace.workspace_id,
                session_id: None,
                event_type: "workspace.registered",
                payload: &serde_json::json!({
                    "workspace_id": workspace.workspace_id,
                    "canonical_worktree_path": workspace.canonical_worktree_path,
                    "head_commit": workspace.head_commit,
                    "branch_or_detached_state": workspace.branch_or_detached_state,
                }),
                occurred_at: &workspace.created_at,
                recorded_at: &workspace.created_at,
            },
        )?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(workspace.clone()))
    }

    /// Finds the Workspace registered for an exact canonical worktree path.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query or row decoding fails.
    pub fn find_workspace_by_path(
        &self,
        canonical_worktree_path: &str,
    ) -> Result<Option<Workspace>, StoreError> {
        workspace_by_path(&self.connection, canonical_worktree_path)
    }

    /// Returns the Project's root Workspace ([`root_workspace_in`]); `None`
    /// when no row is the root.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query or row decoding fails.
    pub fn root_workspace(&self) -> Result<Option<Workspace>, StoreError> {
        root_workspace_in(&self.connection)
    }

    /// Reports whether `workspace_id` is registered in this Project.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query fails.
    pub fn has_workspace(&self, workspace_id: &WorkspaceId) -> Result<bool, StoreError> {
        self.connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM workspaces WHERE workspace_id = ?1)",
                [workspace_id.as_str()],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    /// Whether an Event of this Project's chain was recorded in
    /// `workspace_id` (P4-H1, T20 SF-1): a Workspace of the Project whose
    /// row is missing is still named by its Events.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query fails.
    pub fn ledger_names_workspace(&self, workspace_id: &WorkspaceId) -> Result<bool, StoreError> {
        self.connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE workspace_id = ?1)",
                [workspace_id.as_str()],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    /// Atomically creates or replays one Session in a subject-scoped namespace.
    /// A new Session is linked to `task_id` when one is given and gets its
    /// daemon-produced `session.started` Event in the same transaction
    /// (ADR 0012); a replay writes nothing. See
    /// [`Self::create_session_resumed`].
    ///
    /// # Errors
    ///
    /// As [`Self::create_session_resumed`].
    pub fn create_session_idempotent(
        &mut self,
        proposed: &Session,
        task_id: Option<&TaskId>,
        started_event_id: &EventId,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Session>, StoreError> {
        self.create_session_resumed(
            proposed,
            task_id,
            None,
            started_event_id,
            context,
            request_digest,
        )
    }

    /// [`Self::create_session_idempotent`] with, ADR 0014 E6 (P4-F), the
    /// Pack the Session declares it continues from: its ledger sequence must
    /// be at most the Project's latest one, and after `session.started` and
    /// `task.session_linked` `memproved` appends `capsule.recovered@1`
    /// (`{task_id, capsule_digest, project_sequence, pack_digest, budget,
    /// declared: true}`) on the Session, in the same transaction. P4-D
    /// (Owner A1): a `completed` or `cancelled` Task takes no new Session.
    ///
    /// # Errors
    ///
    /// [`StoreError::RecoveryInputRejected`] for a `resumed_from` above the
    /// high-water mark or without `task_id`, [`StoreError::NotFound`] for an
    /// unknown Task, [`StoreError::SessionWorkspaceMismatch`] when the Task
    /// is scoped to another Workspace, [`StoreError::InvalidTransition`] for
    /// a finished Task, or a [`StoreError`] for inactive projects, conflicts,
    /// constraints, or database failures; nothing is written then.
    pub fn create_session_resumed(
        &mut self,
        proposed: &Session,
        task_id: Option<&TaskId>,
        resumed_from: Option<&ResumedFrom>,
        started_event_id: &EventId,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Session>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = replayed_session(&transaction, context, request_digest)? {
            capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(existing));
        }
        check_resumed_from(&transaction, task_id, resumed_from)?;
        check_parent_session(&transaction, proposed)?;
        insert_session(&transaction, proposed)?;
        // U8 A1 (Owner O4): a Session that declares an agent records it in
        // `session.started@3`; one that declares none is recorded exactly
        // as before, as `session.started@1` without an `agent` member.
        let mut payload = serde_json::json!({
            "agent_id": proposed.agent_id,
            "task_id": task_id,
        });
        if let Some(agent) = &proposed.agent {
            payload["agent"] = agent.recorded();
        }
        append_daemon_event_versioned(
            &transaction,
            &DaemonEvent {
                event_id: started_event_id,
                workspace_id: &proposed.workspace_id,
                session_id: Some(&proposed.session_id),
                event_type: "session.started",
                payload: &payload,
                occurred_at: &proposed.started_at,
                recorded_at: &proposed.started_at,
            },
            if proposed.agent.is_some() {
                SESSION_STARTED_AGENT_SCHEMA
            } else {
                "session.started@1"
            },
        )?;
        if let Some(task_id) = task_id {
            link_new_session(&transaction, task_id, proposed)?;
            if let Some(from) = resumed_from {
                append_capsule_recovered(&transaction, task_id, proposed, from)?;
            }
        }
        transaction
            .execute(
                "INSERT INTO state_idempotency_records (
                    principal_id, operation_id, idempotency_key,
                    canonical_request_sha256, state, result_record_type,
                    result_record_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, 'SUCCEEDED', 'Session', ?5, ?6, ?6)",
                params![
                    context.principal_id.as_str(),
                    context.operation_id.as_str(),
                    context.key.as_str(),
                    request_digest.as_str(),
                    proposed.session_id.as_str(),
                    proposed.started_at.as_str()
                ],
            )
            .map_err(map_sqlite_error)?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(proposed.clone()))
    }

    /// Atomically ends one active Session, or replays the result of the same
    /// idempotent request. The revision is checked before the state, so a
    /// stale writer always learns that it is stale.
    ///
    /// The daemon-produced `session.ended` Event, whose payload carries
    /// `reason`, is appended in the same transaction before `ended_at` is
    /// written: an ended Session accepts no Event (ADR 0012). With
    /// [`SessionOutcome::Abandoned`] (ADR 0013 D6, P3-D) `session.crashed`
    /// (`{"reason", "outcome": "abandoned"}`) takes its place: one Event per
    /// change either way. A replay writes nothing.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdempotencyConflict`], [`StoreError::NotFound`] for an
    /// unknown Session, [`StoreError::RevisionConflict`],
    /// [`StoreError::InvalidTransition`] for a Session that already ended, or
    /// another [`StoreError`].
    #[allow(clippy::too_many_arguments)]
    pub fn end_session(
        &mut self,
        session_id: &SessionId,
        expected_revision: u64,
        ended_at: &UtcTimestamp,
        reason: Option<&str>,
        outcome: SessionOutcome,
        ended_event_id: &EventId,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Session>, StoreError> {
        self.end_session_by(
            session_id,
            expected_revision,
            ended_at,
            reason,
            (outcome, None),
            ended_event_id,
            context,
            request_digest,
        )
    }

    /// [`Self::end_session`] with (U9 A7, Owner O3; ADR 0010, "Amendment
    /// (U9)") who ends the Session as abandoned. With an `ended_by` (only
    /// with [`SessionOutcome::Abandoned`]; the caller refuses it otherwise)
    /// the Event is `session.crashed@2`, whose payload adds `ended_by`, and
    /// the row's three columns of migration 0009 hold the ender's Session,
    /// agent name, and role; without one, nothing differs from
    /// [`Self::end_session`]. The ender's Session and its agent's parent
    /// must be Sessions of this Project (in any state), checked in the
    /// write transaction.
    ///
    /// # Errors
    ///
    /// As [`Self::end_session`], and [`StoreError::RecoveryInputRejected`]
    /// with the rule for an ender Session or parent that names no Session
    /// of this Project.
    #[allow(clippy::too_many_arguments)]
    pub fn end_session_by(
        &mut self,
        session_id: &SessionId,
        expected_revision: u64,
        ended_at: &UtcTimestamp,
        reason: Option<&str>,
        (outcome, ended_by): (SessionOutcome, Option<&SessionEndedBy>),
        ended_event_id: &EventId,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Session>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(record_id) = succeeded_result(&transaction, context, request_digest)? {
            let recorded = SessionId::parse(&record_id)
                .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))?;
            // An ended Session never changes again, so the current row is the
            // recorded result.
            let existing = session_by_id(&transaction, &recorded)?
                .ok_or(StoreError::NotFound("Session for idempotency record"))?;
            capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(existing));
        }
        let current =
            session_by_id(&transaction, session_id)?.ok_or(StoreError::NotFound("Session"))?;
        let ended_by = ended_by.filter(|_| outcome == SessionOutcome::Abandoned);
        check_ender(&transaction, ended_by)?;
        if current.revision != expected_revision {
            return Err(StoreError::RevisionConflict);
        }
        if !current.is_active() {
            return Err(StoreError::InvalidTransition);
        }
        let mut payload = match outcome {
            SessionOutcome::Completed => serde_json::json!({ "reason": reason }),
            SessionOutcome::Abandoned => serde_json::json!({
                "reason": reason,
                "outcome": outcome.as_str(),
            }),
        };
        // U9 A7: an end that names its ender is `session.crashed@2`; one
        // that names none is recorded exactly as before.
        let payload_schema = match ended_by {
            Some(ended_by) => {
                payload["ended_by"] = ended_by.recorded();
                SESSION_CRASHED_ENDED_BY_SCHEMA.to_owned()
            }
            None => format!("{}@1", outcome.event_type()),
        };
        append_daemon_event_versioned(
            &transaction,
            &DaemonEvent {
                event_id: ended_event_id,
                workspace_id: &current.workspace_id,
                session_id: Some(session_id),
                event_type: outcome.event_type(),
                payload: &payload,
                occurred_at: ended_at,
                recorded_at: ended_at,
            },
            &payload_schema,
        )?;
        let ended = Session {
            ended_at: Some(ended_at.clone()),
            revision: next_revision(current.revision)?,
            ended_by: ended_by.cloned(),
            ..current
        };
        let [ender_session, ender_name, ender_role] = SessionEndedBy::columns(ended_by);
        transaction.execute(
            "UPDATE sessions SET ended_at = ?2, revision = ?3,
                ended_by_session_id = ?5, ended_by_agent_name = ?6, ended_by_agent_role = ?7
             WHERE session_id = ?1 AND revision = ?4 AND ended_at IS NULL",
            params![
                session_id.as_str(),
                ended_at.as_str(),
                ended.revision,
                expected_revision,
                ender_session,
                ender_name,
                ender_role,
            ],
        )?;
        record_success(
            &transaction,
            context,
            request_digest,
            "Session",
            session_id.as_str(),
            ended_at,
        )?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(ended))
    }

    /// Atomically moves one Task along the domain state machine
    /// ([`TaskStatus::check_transition`]) and appends its `task.transitioned`
    /// Event without a Session (ADR 0012 D7), or replays the result of the
    /// same idempotent request. The revision is checked before the state.
    ///
    /// `completed` needs `completion_evidence_ids` (P2-C): the caller has
    /// judged them `verified` against a probe; here, inside the write
    /// transaction, each must exist, belong to the Task, and be `PASS`. They
    /// are recorded in the Event, and other targets take none.
    ///
    /// A replay returns the Task as the first request left it: the same
    /// request names the same `target`, `expected_revision`, and Evidence,
    /// and no other wire field of a Task changes after creation.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdempotencyConflict`], [`StoreError::NotFound`] for an
    /// unknown Task, [`StoreError::RevisionConflict`],
    /// [`StoreError::InvalidTransition`],
    /// [`StoreError::CompletionEvidenceRejected`], or another [`StoreError`].
    #[allow(clippy::too_many_arguments)]
    pub fn transition_task(
        &mut self,
        task_id: &TaskId,
        expected_revision: u64,
        target: TaskStatus,
        completion_evidence_ids: &[EvidenceId],
        updated_at: &UtcTimestamp,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Task>, StoreError> {
        let capability_root = self.verified_mutation_root()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(record_id) = succeeded_result(&transaction, context, request_digest)? {
            let recorded = TaskId::parse(&record_id)
                .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))?;
            let current = task_by_id(&transaction, &recorded)?
                .ok_or(StoreError::NotFound("Task for idempotency record"))?;
            capability_root.verify_identity()?;
            transaction.commit()?;
            // Only a completion carries completion Evidence; `completed` is
            // terminal, so the current Task's Evidence is the recorded one.
            let completion_evidence_ids = if target == TaskStatus::Completed {
                current.completion_evidence_ids.clone()
            } else {
                Vec::new()
            };
            return Ok(Idempotent::Replayed(Task {
                status: target,
                revision: next_revision(expected_revision)?,
                completion_evidence_ids,
                ..current
            }));
        }
        let current = task_by_id(&transaction, task_id)?.ok_or(StoreError::NotFound("Task"))?;
        if current.revision != expected_revision {
            return Err(StoreError::RevisionConflict);
        }
        current
            .status
            .check_transition(target, !completion_evidence_ids.is_empty())
            .map_err(|_| StoreError::InvalidTransition)?;
        if target == TaskStatus::Completed {
            check_completion_evidence(&transaction, task_id, completion_evidence_ids)?;
        } else if !completion_evidence_ids.is_empty() {
            return Err(StoreError::CompletionEvidenceRejected(
                "completion Evidence applies only to completed".to_owned(),
            ));
        }
        let from = current.status;
        let moved = Task {
            status: target,
            updated_at: updated_at.clone(),
            revision: next_revision(current.revision)?,
            completion_evidence_ids: completion_evidence_ids.to_vec(),
            ..current
        };
        transaction.execute(
            "UPDATE tasks SET status = ?2, updated_at = ?3, revision = ?4
             WHERE task_id = ?1 AND revision = ?5",
            params![
                task_id.as_str(),
                target.as_str(),
                updated_at.as_str(),
                moved.revision,
                expected_revision
            ],
        )?;
        append_task_event(
            &transaction,
            &moved,
            None,
            "task.transitioned",
            &serde_json::json!({
                "task_id": task_id,
                "from": from.as_str(),
                "to": target.as_str(),
                "revision": moved.revision,
                "completion_evidence_ids": completion_evidence_ids,
            }),
            updated_at,
        )?;
        record_success(
            &transaction,
            context,
            request_digest,
            "Task",
            task_id.as_str(),
            updated_at,
        )?;
        capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(Idempotent::Created(moved))
    }

    /// Writes the root Workspace during provisioning, before the Project is
    /// `ACTIVE`, unless a root already exists; returns the root.
    pub(crate) fn ensure_root_workspace(
        &mut self,
        root: &Workspace,
    ) -> Result<Workspace, StoreError> {
        self.capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // U1 B4 (O5): the root is found by the rule of `root_workspace_in`.
        // Only a database without any Workspace row gets one; one whose root
        // row is missing while other rows remain is not given a new root
        // (before: the first row by `created_at` was taken as the root).
        let root = if let Some(existing) = root_workspace_in(&transaction)? {
            existing
        } else if transaction.query_row("SELECT EXISTS (SELECT 1 FROM workspaces)", [], |row| {
            row.get::<_, bool>(0)
        })? {
            return Err(StoreError::PersistedDataIntegrity(
                "the root Workspace row is missing".to_owned(),
            ));
        } else {
            insert_workspace(&transaction, root)?;
            root.clone()
        };
        self.capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(root)
    }

    /// Reads one page of Tasks after `after` in `(created_at, task_id)` order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] when `after` names no Task, or another
    /// [`StoreError`] when the read fails.
    pub fn list_tasks_page(
        &self,
        after: Option<&TaskId>,
        limit: u32,
    ) -> Result<StatePage<Task>, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        // P4-H1: the cursor row only has to exist; a page row that does not
        // decode is the error.
        if let Some(after) = after
            && !transaction.query_row(
                "SELECT EXISTS (SELECT 1 FROM tasks WHERE task_id = ?1)",
                [after.as_str()],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Err(StoreError::NotFound("Task cursor"));
        }
        let mut statement = transaction.prepare(&format!(
            "SELECT {TASK_COLUMNS}
             FROM tasks
             WHERE ?1 IS NULL
                OR (created_at, task_id) > (SELECT created_at, task_id FROM tasks WHERE task_id = ?1)
             ORDER BY created_at, task_id
             LIMIT ?2"
        ))?;
        let items = statement
            .query_map(
                params![after.map(TaskId::as_str), i64::from(limit) + 1],
                task_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let page = state_page(items, limit, latest_project_sequence(&transaction)?);
        transaction.commit()?;
        Ok(page)
    }

    /// Reads one page of a Workspace's Sessions after `after` in
    /// `(started_at, session_id)` order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] when `after` names no Session of the
    /// Workspace, or another [`StoreError`] when the read fails.
    pub fn list_workspace_sessions_page(
        &self,
        workspace_id: &WorkspaceId,
        after: Option<&SessionId>,
        limit: u32,
    ) -> Result<StatePage<Session>, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        if let Some(after) = after {
            let known: bool = transaction.query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM sessions WHERE session_id = ?1 AND workspace_id = ?2
                 )",
                params![after.as_str(), workspace_id.as_str()],
                |row| row.get(0),
            )?;
            if !known {
                return Err(StoreError::NotFound("Session cursor"));
            }
        }
        let mut statement = transaction.prepare(
            "SELECT session_id, workspace_id, agent_id, started_at, ended_at, revision,
                agent_name, agent_role, parent_session_id,
                ended_by_session_id, ended_by_agent_name, ended_by_agent_role
             FROM sessions
             WHERE workspace_id = ?1
               AND (?2 IS NULL
                    OR (started_at, session_id)
                       > (SELECT started_at, session_id FROM sessions WHERE session_id = ?2))
             ORDER BY started_at, session_id
             LIMIT ?3",
        )?;
        let items = statement
            .query_map(
                params![
                    workspace_id.as_str(),
                    after.map(SessionId::as_str),
                    i64::from(limit) + 1
                ],
                session_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let page = state_page(items, limit, latest_project_sequence(&transaction)?);
        transaction.commit()?;
        Ok(page)
    }

    /// The result record ID of `context` when that request already
    /// succeeded, `None` when the key is unused; writes nothing. Lets a
    /// caller tell a replay from a new request before judging it (P2-C).
    ///
    /// # Errors
    ///
    /// [`StoreError::IdempotencyConflict`] when the key was used with another
    /// request, or another [`StoreError`].
    pub fn succeeded_record(
        &self,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Option<String>, StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        let recorded = succeeded_result(&transaction, context, request_digest)?;
        transaction.commit()?;
        Ok(recorded)
    }

    /// Returns the number of state idempotency rows for behavioral verification.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the query fails.
    pub fn idempotency_record_count(&self) -> Result<u64, StoreError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM state_idempotency_records",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    fn require_active(&self) -> Result<(), StoreError> {
        if self.project_state == ProjectProvisioningState::Active {
            Ok(())
        } else {
            Err(StoreError::ProjectNotActive)
        }
    }

    fn verified_mutation_root(&self) -> Result<CapabilityRoot, StoreError> {
        self.require_active()?;
        self.capability_root.verify_identity()?;
        Ok(self.capability_root.clone())
    }
}

fn insert_workspace(
    transaction: &Transaction<'_>,
    workspace: &Workspace,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, canonical_worktree_path, git_dir,
                head_commit, branch_or_detached_state, created_at, revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workspace.workspace_id.as_str(),
                workspace.project_id.as_str(),
                workspace.canonical_worktree_path,
                workspace.git_dir,
                workspace.head_commit,
                workspace.branch_or_detached_state,
                workspace.created_at.as_str(),
                workspace.revision
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn session_by_id(
    connection: &Connection,
    session_id: &SessionId,
) -> Result<Option<Session>, StoreError> {
    connection
        .query_row(
            "SELECT session_id, workspace_id, agent_id, started_at, ended_at, revision,
                agent_name, agent_role, parent_session_id,
                ended_by_session_id, ended_by_agent_name, ended_by_agent_role
             FROM sessions WHERE session_id = ?1",
            [session_id.as_str()],
            session_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn next_revision(revision: u64) -> Result<u64, StoreError> {
    revision
        .checked_add(1)
        .filter(|next| i64::try_from(*next).is_ok())
        .ok_or_else(|| StoreError::ConstraintViolation("revision overflow".to_owned()))
}

/// The result record ID of a `SUCCEEDED` idempotency record in `context`,
/// `None` when the key is unused.
///
/// # Errors
///
/// [`StoreError::IdempotencyConflict`] when the key was used with another
/// request, or [`StoreError::InvalidPersistedValue`] for an unfinished or
/// incomplete record.
fn succeeded_result(
    transaction: &Transaction<'_>,
    context: &IdempotencyContext,
    request_digest: &Sha256Digest,
) -> Result<Option<String>, StoreError> {
    let Some((digest, state, record_id)) = task_idempotency(transaction, context)? else {
        return Ok(None);
    };
    if digest != request_digest.as_str() {
        return Err(StoreError::IdempotencyConflict);
    }
    if state != "SUCCEEDED" {
        return Err(StoreError::InvalidPersistedValue(format!(
            "{} idempotency record is {state}",
            context.operation_id.as_str()
        )));
    }
    record_id.map(Some).ok_or_else(|| {
        StoreError::InvalidPersistedValue(
            "SUCCEEDED idempotency record has no result ID".to_owned(),
        )
    })
}

fn record_success(
    transaction: &Transaction<'_>,
    context: &IdempotencyContext,
    request_digest: &Sha256Digest,
    record_type: &str,
    record_id: &str,
    at: &UtcTimestamp,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO state_idempotency_records (
                principal_id, operation_id, idempotency_key,
                canonical_request_sha256, state, result_record_type,
                result_record_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'SUCCEEDED', ?5, ?6, ?7, ?7)",
            params![
                context.principal_id.as_str(),
                context.operation_id.as_str(),
                context.key.as_str(),
                request_digest.as_str(),
                record_type,
                record_id,
                at.as_str()
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn workspace_by_path(
    connection: &Connection,
    canonical_worktree_path: &str,
) -> Result<Option<Workspace>, StoreError> {
    connection
        .query_row(
            "SELECT workspace_id, project_id, canonical_worktree_path, git_dir,
                    head_commit, branch_or_detached_state, created_at, revision
             FROM workspaces WHERE canonical_worktree_path = ?1",
            [canonical_worktree_path],
            workspace_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

/// Latest Project Event sequence of this per-Project database, 0 without
/// Events. P4-H1: a sequence below 1, which no row the store wrote holds
/// (CHECK `BETWEEN 1 AND 2^53 - 1`), is not one; the chain check reports
/// such a row.
fn latest_project_sequence(connection: &Connection) -> Result<u64, StoreError> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(project_sequence), 0) FROM events WHERE project_sequence >= 1",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::from)
}

fn pre_envelope_rows() -> StoreError {
    StoreError::InvalidPersistedValue(
        "the Event ledger holds pre-envelope rows (event_digest_version IS NULL); \
         this build neither appends to nor verifies such a ledger"
            .to_owned(),
    )
}

fn reject_pre_envelope_rows(connection: &Connection) -> Result<(), StoreError> {
    let present: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE event_digest_version IS NULL)",
        [],
        |row| row.get(0),
    )?;
    if present {
        Err(pre_envelope_rows())
    } else {
        Ok(())
    }
}

/// The only `INSERT INTO events` (ADR 0010 D6): allocates the next Session and
/// Project positions inside the caller's write transaction, computes both
/// digests, and appends the row. Concurrent writers are serialized by the
/// `IMMEDIATE` transaction; the triggers and the unique Project sequence
/// index reject any fork that bypasses this function. An Event without a
/// Session (a daemon Event, ADR 0012 D7) takes no Session sequence.
fn append_event_in(transaction: &Transaction<'_>, input: &EventInput) -> Result<Event, StoreError> {
    reject_pre_envelope_rows(transaction)?;
    let overflow = || StoreError::ConstraintViolation("Event sequence overflow".to_owned());
    let sequence = match &input.session_id {
        None => None,
        Some(session_id) => {
            // A Session that is not active accepts no Event from any writer
            // (Owner decision, T7). An unknown Session is left to the
            // foreign key.
            let active: Option<bool> = transaction
                .query_row(
                    "SELECT ended_at IS NULL FROM sessions WHERE session_id = ?1",
                    [session_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            if active == Some(false) {
                return Err(StoreError::SessionNotActive);
            }
            // T21 (V1-N-5): nor one whose end the chain records.
            if session_end_recorded(transaction, session_id)? {
                return Err(StoreError::SessionNotActive);
            }
            let session_head: Option<u64> = transaction.query_row(
                "SELECT MAX(sequence) FROM events WHERE session_id = ?1",
                [session_id.as_str()],
                |row| row.get(0),
            )?;
            Some(
                session_head
                    .map_or(Some(1), |head| head.checked_add(1))
                    .ok_or_else(overflow)?,
            )
        }
    };
    let (project_sequence, previous_event_sha256) = match project_head(transaction)? {
        Some((head, digest)) => (head.checked_add(1).ok_or_else(overflow)?, Some(digest)),
        None => (1, None),
    };
    let event = Event::new(
        input.clone(),
        sequence,
        project_sequence,
        previous_event_sha256,
    )
    .map_err(|error| StoreError::ConstraintViolation(error.to_string()))?;
    event
        .validate()
        .map_err(|error| StoreError::ConstraintViolation(error.to_string()))?;
    transaction
        .execute(
            "INSERT INTO events (
                event_id, session_id, sequence, event_type, payload, payload_sha256,
                occurred_at, recorded_at, previous_event_sha256, event_sha256,
                envelope_version, project_id, workspace_id, payload_schema,
                producer_name, producer_version, producer_sequence, correlation_id,
                causation_id, privacy_class, producer_occurred_at, project_sequence,
                event_digest_version
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21, ?22
             )",
            params![
                event.event_id().as_str(),
                event.session_id().map(SessionId::as_str),
                event.sequence(),
                event.event_type(),
                event.payload().as_str(),
                event.payload_sha256().as_str(),
                // ?7 fills both the 0001 `occurred_at` (never read back) and
                // `recorded_at` with the receipt time.
                event.recorded_at().as_str(),
                event.previous_event_sha256().map(Sha256Digest::as_str),
                event.event_sha256().as_str(),
                event.envelope_version(),
                event.project_id().as_str(),
                event.workspace_id().as_str(),
                event.payload_schema(),
                event.producer().name,
                event.producer().version,
                event.producer().sequence,
                event.correlation_id().map(CorrelationId::as_str),
                event.causation_id().map(EventId::as_str),
                event.privacy_class().as_str(),
                event.occurred_at().map(UtcTimestamp::as_str),
                event.project_sequence(),
                EVENT_DIGEST_VERSION,
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(event)
}

/// Inside the write transaction, the completion Evidence of a Task must be
/// non-empty, exist, belong to the Task, and be `PASS` (P2-C). Freshness was
/// judged by the caller against its probe.
fn check_completion_evidence(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    evidence_ids: &[EvidenceId],
) -> Result<(), StoreError> {
    let rejected = |reason: String| Err(StoreError::CompletionEvidenceRejected(reason));
    if evidence_ids.is_empty() {
        return rejected("no completion Evidence".to_owned());
    }
    for evidence_id in evidence_ids {
        let Some(evidence) = recovery::evidence_by_id(transaction, evidence_id)? else {
            return rejected(format!("Evidence {evidence_id} does not exist"));
        };
        if evidence.task_id() != task_id {
            return rejected(format!("Evidence {evidence_id} belongs to another Task"));
        }
        if evidence.payload().run_status() != Some(RunStatus::Pass) {
            return rejected(format!("Evidence {evidence_id} is not PASS"));
        }
    }
    Ok(())
}

/// Links a Session created in `transaction` to its Task. Any error rolls the
/// whole creation back.
fn link_new_session(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    session: &Session,
) -> Result<(), StoreError> {
    let task = task_by_id(transaction, task_id)?.ok_or(StoreError::NotFound("Task"))?;
    // T21 (V1-N-5): the Workspace scope the Task's `task.created@1` Event
    // recorded, as the Task Capsule reads it (`task_from_ledger`); no digest
    // covers the row's `workspace_id`.
    let task = capsule::task_from_ledger(transaction, task)?;
    // The Workspace scope, then (P4-D, Owner A1) the Task's state: the
    // function the Task Capsule's `begin_session` action reads.
    if let Some(refusal) = session_link_refusals(&task, Some(&session.workspace_id))
        .into_iter()
        .next()
    {
        return Err(link_refusal(refusal));
    }
    insert_session_link(transaction, &task, session, &session.started_at)
}

/// The store error of a refused Session link.
const fn link_refusal(refusal: SessionLinkRefusal) -> StoreError {
    match refusal {
        SessionLinkRefusal::WorkspaceMismatch => StoreError::SessionWorkspaceMismatch,
        SessionLinkRefusal::TaskFinished => StoreError::InvalidTransition,
    }
}

/// The Session an earlier `createSession` with the same idempotency key
/// created, `None` for a new request.
fn replayed_session(
    transaction: &Transaction<'_>,
    context: &IdempotencyContext,
    request_digest: &Sha256Digest,
) -> Result<Option<Session>, StoreError> {
    if let Some((digest, state, record_id)) = task_idempotency(transaction, context)? {
        if digest != request_digest.as_str() {
            return Err(StoreError::IdempotencyConflict);
        }
        if state != "SUCCEEDED" {
            return Err(StoreError::InvalidPersistedValue(format!(
                "Session idempotency record is {state}"
            )));
        }
        let session_id = record_id
            .as_deref()
            .map(SessionId::parse)
            .transpose()
            .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))?
            .ok_or_else(|| {
                StoreError::InvalidPersistedValue(
                    "SUCCEEDED Session idempotency record has no result ID".to_owned(),
                )
            })?;
        let existing = transaction
            .query_row(
                "SELECT session_id, workspace_id, agent_id, started_at, ended_at, revision,
                agent_name, agent_role, parent_session_id,
                ended_by_session_id, ended_by_agent_name, ended_by_agent_role
                 FROM sessions WHERE session_id = ?1",
                [session_id.as_str()],
                session_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound("Session for idempotency record"))?;
        return Ok(Some(existing));
    }
    Ok(None)
}

/// ADR 0014 E6 (P4-F, Owner condition f): `resumed_from` only with a Task,
/// and its ledger sequence at most the Project's latest.
fn check_resumed_from(
    transaction: &Transaction<'_>,
    task_id: Option<&TaskId>,
    resumed_from: Option<&ResumedFrom>,
) -> Result<(), StoreError> {
    let Some(from) = resumed_from else {
        return Ok(());
    };
    if task_id.is_none() {
        return Err(StoreError::RecoveryInputRejected(
            "$.resumed_from applies only to a Session with a $.task_id (ADR 0014 E6)".to_owned(),
        ));
    }
    from.check_sequence(latest_project_sequence(transaction)?)
        .map_err(|refusal| StoreError::RecoveryInputRejected(refusal.0))
}

/// ADR 0014 E6 (P4-F, Owner condition h): `capsule.recovered@1` of
/// `memproved` on the new Session, in its creating transaction.
fn append_capsule_recovered(
    transaction: &Transaction<'_>,
    task_id: &TaskId,
    session: &Session,
    from: &ResumedFrom,
) -> Result<(), StoreError> {
    append_daemon_event_in(
        transaction,
        &DaemonEvent {
            event_id: &EventId::new(),
            workspace_id: &session.workspace_id,
            session_id: Some(&session.session_id),
            event_type: "capsule.recovered",
            payload: &from.payload(task_id),
            occurred_at: &session.started_at,
            recorded_at: &session.started_at,
        },
    )
    .map(|_| ())
}

/// Writes the Task–Session link and its `task.session_linked` Event, on the
/// Session (P2-C).
fn insert_session_link(
    transaction: &Transaction<'_>,
    task: &Task,
    session: &Session,
    linked_at: &UtcTimestamp,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO task_sessions (task_id, session_id, linked_at, revision)
             VALUES (?1, ?2, ?3, 1)",
            params![
                task.task_id.as_str(),
                session.session_id.as_str(),
                linked_at.as_str()
            ],
        )
        .map_err(map_sqlite_error)?;
    append_task_event(
        transaction,
        task,
        Some(session),
        "task.session_linked",
        &serde_json::json!({
            "task_id": task.task_id,
            "session_id": session.session_id,
        }),
        linked_at,
    )?;
    Ok(())
}

/// One Event produced by `memproved` itself, appended with the state change
/// it records. `session_id` is `None` for a change no Session made
/// (ADR 0012 D7).
pub(crate) struct DaemonEvent<'a> {
    pub(crate) event_id: &'a EventId,
    pub(crate) workspace_id: &'a WorkspaceId,
    pub(crate) session_id: Option<&'a SessionId>,
    pub(crate) event_type: &'static str,
    pub(crate) payload: &'a serde_json::Value,
    pub(crate) occurred_at: &'a UtcTimestamp,
    pub(crate) recorded_at: &'a UtcTimestamp,
}

/// Appends a daemon-produced Event through [`append_event_in`] (ADR 0010 D6):
/// producer `memproved` at the crate version, producer sequence equal to the
/// Project sequence the Event takes, `privacy_class = internal`, and payload
/// schema `<event_type>@1`.
pub(crate) fn append_daemon_event_in(
    transaction: &Transaction<'_>,
    event: &DaemonEvent<'_>,
) -> Result<Event, StoreError> {
    append_daemon_event_versioned(transaction, event, &format!("{}@1", event.event_type))
}

/// [`append_daemon_event_in`] with an explicit `payload_schema` (U8 A1:
/// `session.started@3` for a Session that declares an agent).
fn append_daemon_event_versioned(
    transaction: &Transaction<'_>,
    event: &DaemonEvent<'_>,
    payload_schema: &str,
) -> Result<Event, StoreError> {
    let payload = CanonicalJson::parse(&event.payload.to_string())
        .map_err(|error| StoreError::ConstraintViolation(error.to_string()))?;
    let project_id = transaction.query_row(
        "SELECT project_id FROM state_metadata WHERE metadata_key = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    let input = EventInput {
        event_id: event.event_id.clone(),
        project_id: ProjectId::parse(&project_id)
            .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))?,
        workspace_id: event.workspace_id.clone(),
        session_id: event.session_id.cloned(),
        event_type: event.event_type.to_owned(),
        payload_schema: payload_schema.to_owned(),
        payload,
        occurred_at: Some(event.occurred_at.clone()),
        recorded_at: event.recorded_at.clone(),
        producer: EventProducer {
            name: memprove_domain::DAEMON_PRODUCER_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            sequence: next_project_sequence(transaction)?,
        },
        correlation_id: None,
        causation_id: None,
        privacy_class: PrivacyClass::Internal,
    };
    append_event_in(transaction, &input)
}

/// The next Project Event sequence inside the caller's write transaction.
fn next_project_sequence(transaction: &Transaction<'_>) -> Result<u64, StoreError> {
    match project_head(transaction)? {
        Some((head, _)) => head
            .checked_add(1)
            .ok_or_else(|| StoreError::ConstraintViolation("Event sequence overflow".to_owned())),
        None => Ok(1),
    }
}

fn project_head(connection: &Connection) -> Result<Option<(u64, Sha256Digest)>, StoreError> {
    connection
        .query_row(
            "SELECT project_sequence, event_sha256 FROM events
             ORDER BY project_sequence DESC LIMIT 1",
            [],
            |row| Ok((row.get::<_, Option<u64>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(head, digest)| {
            let head = head.ok_or_else(pre_envelope_rows)?;
            let digest = Sha256Digest::parse(&digest)
                .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))?;
            Ok((head, digest))
        })
        .transpose()
}

fn event_by_id(connection: &Connection, event_id: &EventId) -> Result<Option<Event>, StoreError> {
    connection
        .query_row(
            &format!("SELECT {EVENT_COLUMNS} FROM events WHERE event_id = ?1"),
            [event_id.as_str()],
            event_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn state_page<T>(mut items: Vec<T>, limit: u32, snapshot_sequence: u64) -> StatePage<T> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = items.len() > limit;
    items.truncate(limit);
    StatePage {
        items,
        has_more,
        snapshot_sequence,
    }
}

/// Inserts a new Task and its daemon-produced `task.created` Event, which
/// has no Session (ADR 0012 D7, P2-C).
fn insert_task(transaction: &Transaction<'_>, task: &Task) -> Result<(), StoreError> {
    insert_task_row(transaction, task)?;
    append_task_event(
        transaction,
        task,
        None,
        "task.created",
        &serde_json::json!({
            "task_id": task.task_id,
            "title": task.title,
            "workspace_id": task.workspace_id,
            "status": task.status.as_str(),
            "revision": task.revision,
        }),
        &task.created_at,
    )?;
    Ok(())
}

/// The Workspace a Task's Events are recorded under: the Workspace it is
/// scoped to, else the Project's root Workspace ([`root_workspace_in`]).
fn task_event_workspace(connection: &Connection, task: &Task) -> Result<WorkspaceId, StoreError> {
    if let Some(workspace_id) = &task.workspace_id {
        return Ok(workspace_id.clone());
    }
    Ok(root_workspace_in(connection)?
        .ok_or(StoreError::NotFound("root Workspace"))?
        .workspace_id)
}

/// U1 B4 (original #32, Owner O5): the identity of the Project's root
/// Workspace, without reading its row; `None` when no row is the root.
///
/// The registry records the Project's repository by its common Git
/// directory, and the state database is bound to it by that path's digest
/// (`state_metadata.repository_common_dir_sha256`, checked at every open).
/// `registerProject` writes the root Workspace, the worktree it was given,
/// without an Event; every later Workspace is written by `resolveWorkspace`
/// with its `workspace.registered` Event of `memproved` in the same
/// transaction. The root is therefore the Workspace row of that repository
/// (its Git directory is the common directory, or a linked worktree's
/// `<common>/worktrees/<name>`) that no `workspace.registered` Event
/// announces: the main worktree's first, then the least identity. Before,
/// it was the first row by `created_at`, so with the root's row deleted
/// another row became the root.
pub(super) fn root_workspace_id(connection: &Connection) -> Result<Option<String>, StoreError> {
    let Some(digest) = connection
        .query_row(
            "SELECT repository_common_dir_sha256 FROM state_metadata WHERE metadata_key = 1",
            [],
            |row| lossy_text(row, 0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let announced = connection
        .prepare(
            "SELECT payload FROM events
             WHERE producer_name = ?1 AND event_type = 'workspace.registered'",
        )?
        .query_map([memprove_domain::DAEMON_PRODUCER_NAME], |row| {
            stored_json(row, 0)
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .filter_map(|payload| {
            payload
                .get("workspace_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let of_repository = |path: Option<&Path>| {
        path.and_then(Path::to_str)
            .is_some_and(|path| Sha256Digest::of_bytes(path.as_bytes()).as_str() == digest)
    };
    let mut candidates = connection
        .prepare("SELECT workspace_id, git_dir FROM workspaces")?
        .query_map([], |row| Ok((lossy_text(row, 0)?, lossy_text(row, 1)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(workspace_id, _)| !announced.contains(workspace_id))
        .filter_map(|(workspace_id, git_dir)| {
            let git_dir = Path::new(&git_dir);
            if of_repository(Some(git_dir)) {
                return Some((0_u8, workspace_id));
            }
            let parent = git_dir.parent();
            let linked = parent.and_then(Path::file_name) == Some("worktrees".as_ref())
                && of_repository(parent.and_then(Path::parent));
            linked.then_some((1_u8, workspace_id))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    Ok(candidates
        .into_iter()
        .next()
        .map(|(_, workspace_id)| workspace_id))
}

/// The Project's root Workspace ([`root_workspace_id`]), its row decoded;
/// `None` when no row is the root.
pub(super) fn root_workspace_in(connection: &Connection) -> Result<Option<Workspace>, StoreError> {
    let Some(workspace_id) = root_workspace_id(connection)? else {
        return Ok(None);
    };
    connection
        .query_row(
            "SELECT workspace_id, project_id, canonical_worktree_path, git_dir,
                    head_commit, branch_or_detached_state, created_at, revision
             FROM workspaces WHERE workspace_id = ?1",
            [workspace_id],
            workspace_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

/// Appends one daemon-produced Event about `task` in the caller's write
/// transaction: without a Session for Task changes, with one for a link.
fn append_task_event(
    transaction: &Transaction<'_>,
    task: &Task,
    session: Option<&Session>,
    event_type: &'static str,
    payload: &serde_json::Value,
    at: &UtcTimestamp,
) -> Result<Event, StoreError> {
    let workspace_id = match session {
        Some(session) => session.workspace_id.clone(),
        None => task_event_workspace(transaction, task)?,
    };
    append_daemon_event_in(
        transaction,
        &DaemonEvent {
            event_id: &EventId::new(),
            workspace_id: &workspace_id,
            session_id: session.map(|session| &session.session_id),
            event_type,
            payload,
            occurred_at: at,
            recorded_at: at,
        },
    )
}

fn insert_task_row(transaction: &Transaction<'_>, task: &Task) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO tasks (
                task_id, project_id, title, status, created_at, updated_at, revision,
                workspace_id, description
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                task.task_id.as_str(),
                task.project_id.as_str(),
                task.title,
                task.status.as_str(),
                task.created_at.as_str(),
                task.updated_at.as_str(),
                task.revision,
                task.workspace_id.as_ref().map(WorkspaceId::as_str),
                task.description
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

type StoredTaskIdempotency = (String, String, Option<String>);

fn task_idempotency(
    transaction: &Transaction<'_>,
    context: &IdempotencyContext,
) -> Result<Option<StoredTaskIdempotency>, StoreError> {
    transaction
        .query_row(
            "SELECT canonical_request_sha256, state, result_record_id
             FROM state_idempotency_records
             WHERE principal_id = ?1 AND operation_id = ?2 AND idempotency_key = ?3",
            params![
                context.principal_id.as_str(),
                context.operation_id.as_str(),
                context.key.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(StoreError::from)
}

fn recover_task_idempotency(
    transaction: &Transaction<'_>,
    proposed: &Task,
    context: &IdempotencyContext,
    request_digest: &Sha256Digest,
    record: StoredTaskIdempotency,
) -> Result<Idempotent<Task>, StoreError> {
    let (digest, state, record_id) = record;
    if digest != request_digest.as_str() {
        return Err(StoreError::IdempotencyConflict);
    }
    match state.as_str() {
        "SUCCEEDED" => {
            let record_id = record_id.ok_or_else(|| {
                StoreError::InvalidPersistedValue(
                    "SUCCEEDED Task idempotency record has no result ID".to_owned(),
                )
            })?;
            let task_id = TaskId::parse(&record_id)
                .map_err(|error| StoreError::InvalidPersistedValue(error.to_string()))?;
            let existing = task_by_id(transaction, &task_id)?
                .ok_or(StoreError::NotFound("Task for idempotency record"))?;
            Ok(Idempotent::Replayed(existing))
        }
        "IN_PROGRESS" => {
            let outcome = if let Some(task) = task_by_id(transaction, &proposed.task_id)? {
                Idempotent::Replayed(task)
            } else {
                insert_task(transaction, proposed)?;
                Idempotent::Created(proposed.clone())
            };
            transaction.execute(
                "UPDATE state_idempotency_records
                 SET state = 'SUCCEEDED', result_record_type = 'Task',
                     result_record_id = ?4, updated_at = ?5
                 WHERE principal_id = ?1 AND operation_id = ?2
                   AND idempotency_key = ?3 AND state = 'IN_PROGRESS'",
                params![
                    context.principal_id.as_str(),
                    context.operation_id.as_str(),
                    context.key.as_str(),
                    proposed.task_id.as_str(),
                    proposed.updated_at.as_str()
                ],
            )?;
            Ok(outcome)
        }
        "FAILED" => Err(StoreError::ConstraintViolation(
            "idempotent operation previously failed".to_owned(),
        )),
        value => Err(StoreError::InvalidPersistedValue(format!(
            "unknown idempotency state {value}"
        ))),
    }
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "recursive_triggers", "ON")?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let enabled: u8 = connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    if enabled != 1 {
        return Err(StoreError::InvalidPersistedValue(
            "PRAGMA foreign_keys is not enabled".to_owned(),
        ));
    }
    Ok(())
}

fn migrate_and_bind(
    connection: &mut Connection,
    project: &Project,
    opening: Opening,
) -> Result<(), StoreError> {
    let repository_digest = Sha256Digest::of_bytes(project.repository_common_dir.as_bytes());
    let records = recorded_migrations(connection)?;
    let applied = records.len();
    // T21 (V1-R-2): only provisioning creates a ledger. An existing database
    // without a migration record is never upgraded from nothing: with no
    // object it is not bound to the Project; with objects it is drift, as
    // the comparison below would find. Nothing is written either way.
    if opening == Opening::Existing && applied == 0 {
        return Err(if schema_shape(connection)?.is_empty() {
            StoreError::StateBindingMismatch
        } else {
            state_migration_drift(&records)
        });
    }
    // T21 (V1-S-1): the schema the recorded migrations produce, built in
    // memory, only when a migration is pending.
    let expected = if applied < STATE_MIGRATIONS.len() {
        let expected = Connection::open_in_memory()?;
        for migration in &STATE_MIGRATIONS[..applied] {
            expected.execute_batch(migration.sql)?;
        }
        Some(expected)
    } else {
        None
    };

    for migration in &STATE_MIGRATIONS[applied..] {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let checksum = Sha256Digest::of_bytes(migration.sql.as_bytes());
        // Another connection may have applied this migration after the
        // inventory read above; re-check under the write lock.
        if let Some(recorded) = recorded_migration_checksum(&transaction, migration.id)? {
            if recorded != checksum.as_str() {
                return Err(state_migration_drift(&[(migration.id, recorded)]));
            }
            transaction.commit()?;
            if let Some(expected) = &expected {
                expected.execute_batch(migration.sql)?;
            }
            continue;
        }
        // T21 (V1-S-1): a pending migration is applied only to the schema
        // the recorded ones produce. A record whose last rows are missing
        // (or which is missing altogether) under a schema that already shows
        // those migrations is drift, not an upgrade: nothing is applied and
        // nothing is written.
        if let Some(expected) = &expected {
            if schema_shape(&transaction)? != schema_shape(expected)? {
                return Err(state_migration_drift(&records));
            }
            expected.execute_batch(migration.sql)?;
        }
        transaction.execute_batch(migration.sql)?;
        let now = UtcTimestamp::now();
        if migration.id == 1 {
            transaction.execute(
                "INSERT INTO state_metadata (
                    metadata_key, project_id, repository_common_dir_sha256,
                    schema_generation, created_at
                 ) VALUES (1, ?1, ?2, 1, ?3)",
                params![
                    project.project_id.as_str(),
                    repository_digest.as_str(),
                    now.as_str()
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
             VALUES (?1, ?2, ?3)",
            params![migration.id, checksum.as_str(), now.as_str()],
        )?;
        transaction.commit()?;
    }

    let binding = connection
        .query_row(
            "SELECT project_id, repository_common_dir_sha256, schema_generation
             FROM state_metadata WHERE metadata_key = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            },
        )
        .optional()?;
    if binding
        != Some((
            project.project_id.to_string(),
            repository_digest.to_string(),
            1,
        ))
    {
        return Err(StoreError::StateBindingMismatch);
    }
    Ok(())
}

/// The migration record, checked against the migrations this build knows:
/// empty without a `schema_migrations` table; drift when the table is
/// empty, longer than the list, or differs from its start.
fn recorded_migrations(connection: &Connection) -> Result<Vec<(u64, String)>, StoreError> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(Vec::new());
    }
    let records = connection
        .prepare(
            "SELECT migration_id, migration_checksum
             FROM schema_migrations ORDER BY migration_id",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if records.is_empty() || records.len() > STATE_MIGRATIONS.len() {
        return Err(state_migration_drift(&records));
    }
    for (record, expected) in records.iter().zip(STATE_MIGRATIONS) {
        let expected_checksum = Sha256Digest::of_bytes(expected.sql.as_bytes());
        if record.0 != expected.id || record.1 != expected_checksum.as_str() {
            return Err(state_migration_drift(&records));
        }
    }
    Ok(records)
}

/// One object of a schema: its type, name, and table, and for a table its
/// columns (name, declared type, `NOT NULL`, default, primary-key
/// position).
type SchemaObject = (
    String,
    String,
    String,
    Vec<(String, String, i64, Option<String>, i64)>,
);

/// T21 (V1-S-1): the shape of a database's schema, every object
/// `sqlite_master` names but `SQLite`'s own, in type and name order. It tells
/// a schema a pending migration has not touched from one it has, migration
/// 0007 included (`events.session_id` and `sequence` lose `NOT NULL`).
fn schema_shape(connection: &Connection) -> Result<Vec<SchemaObject>, StoreError> {
    let objects = connection
        .prepare(
            "SELECT type, name, tbl_name FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut columns = connection.prepare(
        "SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_xinfo(?1) ORDER BY cid",
    )?;
    objects
        .into_iter()
        .map(|(kind, name, table)| {
            let shape = if kind == "table" {
                columns
                    .query_map([&name], |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                Vec::new()
            };
            Ok((kind, name, table, shape))
        })
        .collect()
}

fn recorded_migration_checksum(
    connection: &Connection,
    migration_id: u64,
) -> Result<Option<String>, StoreError> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    connection
        .query_row(
            "SELECT migration_checksum FROM schema_migrations WHERE migration_id = ?1",
            [migration_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::from)
}

fn state_migration_drift(records: &[(u64, String)]) -> StoreError {
    let expected = STATE_MIGRATIONS
        .iter()
        .map(|migration| {
            format!(
                "{}:{}:{}",
                migration.id,
                migration.name,
                Sha256Digest::of_bytes(migration.sql.as_bytes()).as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    StoreError::MigrationDrift {
        database: "state.sqlite",
        expected,
        actual: format!("{records:?}"),
    }
}

fn workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        workspace_id: WorkspaceId::parse(&row.get::<_, String>(0)?)
            .map_err(|error| invalid_value(0, error.to_string()))?,
        project_id: memprove_domain::ProjectId::parse(&row.get::<_, String>(1)?)
            .map_err(|error| invalid_value(1, error.to_string()))?,
        canonical_worktree_path: row.get(2)?,
        git_dir: row.get(3)?,
        head_commit: row.get(4)?,
        branch_or_detached_state: row.get(5)?,
        created_at: parse_timestamp(row, 6)?,
        revision: stored_revision(row, 7)?,
    })
}

fn task_by_id(transaction: &Transaction<'_>, task_id: &TaskId) -> Result<Option<Task>, StoreError> {
    task_by_id_connection(transaction, task_id)
}

fn task_by_id_connection(
    connection: &Connection,
    task_id: &TaskId,
) -> Result<Option<Task>, StoreError> {
    connection
        .query_row(
            &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE task_id = ?1"),
            [task_id.as_str()],
            task_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let status = match row.get::<_, String>(3)?.as_str() {
        "proposed" => TaskStatus::Proposed,
        "ready" => TaskStatus::Ready,
        "in_progress" => TaskStatus::InProgress,
        "blocked" => TaskStatus::Blocked,
        "completed" => TaskStatus::Completed,
        "cancelled" => TaskStatus::Cancelled,
        value => return Err(invalid_value(3, value)),
    };
    let workspace_id = row
        .get::<_, Option<String>>(7)?
        .map(|value| WorkspaceId::parse(&value))
        .transpose()
        .map_err(|error| invalid_value(7, error.to_string()))?;
    let completion_evidence_ids = match row.get::<_, Option<String>>(9)? {
        None => Vec::new(),
        Some(ids) => serde_json::from_str::<Vec<String>>(&ids)
            .map_err(|error| invalid_value(9, error.to_string()))?
            .iter()
            .map(|id| EvidenceId::parse(id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| invalid_value(9, error.to_string()))?,
    };
    // P4-H1 (review of the first P4-H1 commit): an empty title, which no
    // Task the store wrote holds (createTask requires one), does not decode.
    let title = row.get::<_, String>(2)?;
    if title.is_empty() {
        return Err(invalid_value(2, "title must not be empty"));
    }
    Ok(Task {
        task_id: TaskId::parse(&row.get::<_, String>(0)?)
            .map_err(|error| invalid_value(0, error.to_string()))?,
        project_id: memprove_domain::ProjectId::parse(&row.get::<_, String>(1)?)
            .map_err(|error| invalid_value(1, error.to_string()))?,
        workspace_id,
        title,
        description: row.get(8)?,
        status,
        created_at: parse_timestamp(row, 4)?,
        updated_at: parse_timestamp(row, 5)?,
        revision: stored_revision(row, 6)?,
        completion_evidence_ids,
    })
}

/// Writes one `sessions` row, the three agent columns of migration 0008
/// (U8 A1) from `session.agent`, NULL for a Session that declares none.
fn insert_session(transaction: &Transaction<'_>, session: &Session) -> Result<(), StoreError> {
    let agent = session.agent.as_ref();
    transaction
        .execute(
            "INSERT INTO sessions (
                session_id, workspace_id, agent_id, started_at, ended_at, revision,
                agent_name, agent_role, parent_session_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                session.session_id.as_str(),
                session.workspace_id.as_str(),
                session.agent_id.as_str(),
                session.started_at.as_str(),
                session.ended_at.as_ref().map(UtcTimestamp::as_str),
                session.revision,
                agent.map(|agent| agent.name.as_str()),
                agent.map(|agent| agent.role.as_str()),
                agent
                    .and_then(|agent| agent.parent_session_id.as_ref())
                    .map(SessionId::as_str),
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

/// Whether the chain records the end of `session_id` (T21 V1-N-5), so it
/// accepts no Event though its row may say otherwise.
fn session_end_recorded(
    transaction: &Transaction<'_>,
    session_id: &SessionId,
) -> Result<bool, StoreError> {
    // T21 (V1-N-5): a Session whose end the chain records, read as
    // every reader of the ledger reads it (`session_from_ledger`,
    // the Task Capsule's Session ends and `actions`): `memproved`'s
    // `session.ended`, or its `session.crashed` at `@1` (P3-C2).
    // No digest covers the row's `ended_at`; on a ledger only
    // `memproved` wrote, the two agree.
    let ended: bool = transaction.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM events
            WHERE session_id = ?1 AND producer_name = ?2
              AND (event_type = 'session.ended'
                   OR (event_type = 'session.crashed'
                       AND payload_schema = 'session.crashed@1'))
         )",
        params![session_id.as_str(), memprove_domain::DAEMON_PRODUCER_NAME],
        |row| row.get(0),
    )?;
    // U9 A7: or its `session.crashed@2`, which names who ended it,
    // when its payload decodes as the Task Capsule decodes it (one
    // that does not is not an end there either).
    let crashed_by = transaction
        .prepare(
            "SELECT payload FROM events
             WHERE session_id = ?1 AND producer_name = ?2
               AND event_type = 'session.crashed' AND payload_schema = ?3",
        )?
        .query_map(
            params![
                session_id.as_str(),
                memprove_domain::DAEMON_PRODUCER_NAME,
                SESSION_CRASHED_ENDED_BY_SCHEMA
            ],
            |row| stored_json(row, 0),
        )?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .any(|payload| capsule::crashed_payload(SESSION_CRASHED_ENDED_BY_SCHEMA, &payload));
    Ok(ended || crashed_by)
}

/// U9 A7 (Owner O3): the Session an `ended_by` names, and its agent's
/// parent, must be Sessions of this Project, in any state; otherwise `400
/// invalid_request` and nothing is written. That the ender is not the
/// Session being ended is the caller's check.
fn check_ender(
    transaction: &Transaction<'_>,
    ended_by: Option<&SessionEndedBy>,
) -> Result<(), StoreError> {
    let Some(ended_by) = ended_by else {
        return Ok(());
    };
    let parent = ended_by
        .agent
        .as_ref()
        .and_then(|agent| agent.parent_session_id.as_ref());
    for (session_id, rule) in [
        (
            ended_by.session_id.as_ref(),
            "$.ended_by.session_id names no Session of this Project",
        ),
        (
            parent,
            "$.ended_by.agent.parent_session_id names no Session of this Project",
        ),
    ] {
        let Some(session_id) = session_id else {
            continue;
        };
        let exists: bool = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE session_id = ?1)",
            [session_id.as_str()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(StoreError::RecoveryInputRejected(rule.to_owned()));
        }
    }
    Ok(())
}

/// U8 A1 (Owner O4): the parent Session an agent names must be a Session
/// of this Project, in any state; otherwise `400 invalid_request` and
/// nothing is written.
fn check_parent_session(
    transaction: &Transaction<'_>,
    proposed: &Session,
) -> Result<(), StoreError> {
    let Some(parent) = proposed
        .agent
        .as_ref()
        .and_then(|agent| agent.parent_session_id.as_ref())
    else {
        return Ok(());
    };
    let exists: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE session_id = ?1)",
        [parent.as_str()],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::RecoveryInputRejected(
            "$.agent.parent_session_id names no Session of this Project".to_owned(),
        ))
    }
}

/// The agent of a `sessions` row (columns 6 to 8, migration 0008): `None`
/// when all three are NULL; a name and a role, with an optional parent,
/// otherwise. Any other combination, or a value not of its form, does not
/// decode.
fn session_agent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<SessionAgent>> {
    let name = row.get::<_, Option<String>>(6)?;
    let role = row.get::<_, Option<String>>(7)?;
    let parent = row.get::<_, Option<String>>(8)?;
    match (name, role) {
        (None, None) if parent.is_none() => Ok(None),
        (Some(name), Some(role)) => SessionAgent::new(&name, &role, parent.as_deref())
            .map(Some)
            .map_err(|refusal| invalid_value(6, refusal.0)),
        _ => Err(invalid_value(
            6,
            "agent_name and agent_role are both set or both NULL, with no parent_session_id \
             without them",
        )),
    }
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let ended_at = row
        .get::<_, Option<String>>(4)?
        .map(|value| UtcTimestamp::parse(&value))
        .transpose()
        .map_err(|error| invalid_value(4, error.to_string()))?;
    let agent = session_agent_from_row(row)?;
    // U9 A7 (migration 0009): the ender, columns 9 to 11.
    let ended_by = SessionEndedBy::from_columns(
        row.get::<_, Option<String>>(9)?.as_deref(),
        row.get::<_, Option<String>>(10)?.as_deref(),
        row.get::<_, Option<String>>(11)?.as_deref(),
    )
    .map_err(|refusal| invalid_value(9, refusal.0))?;
    Ok(Session {
        session_id: SessionId::parse(&row.get::<_, String>(0)?)
            .map_err(|error| invalid_value(0, error.to_string()))?,
        workspace_id: WorkspaceId::parse(&row.get::<_, String>(1)?)
            .map_err(|error| invalid_value(1, error.to_string()))?,
        agent_id: AgentId::parse(&row.get::<_, String>(2)?)
            .map_err(|error| invalid_value(2, error.to_string()))?,
        started_at: parse_timestamp(row, 3)?,
        ended_at,
        revision: stored_revision(row, 5)?,
        agent,
        ended_by,
    })
}

/// Decodes one [`EVENT_COLUMNS`] row and rehydrates it only when both stored
/// digests match recomputation.
fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    if row.get::<_, Option<u8>>(20)? != Some(EVENT_DIGEST_VERSION)
        || row.get::<_, Option<String>>(21)?.as_deref() != Some(EVENT_ENVELOPE_VERSION)
    {
        return Err(invalid_value(20, "pre-envelope Event row"));
    }
    let text = |index: usize| row.get::<_, String>(index);
    let optional_text = |index: usize| row.get::<_, Option<String>>(index);
    let input = EventInput {
        event_id: decoded(0, EventId::parse(&text(0)?))?,
        project_id: decoded(1, ProjectId::parse(&text(1)?))?,
        workspace_id: decoded(2, WorkspaceId::parse(&text(2)?))?,
        session_id: optional_text(3)?
            .map(|value| decoded(3, SessionId::parse(&value)))
            .transpose()?,
        event_type: text(6)?,
        payload_schema: text(7)?,
        payload: decoded(8, CanonicalJson::parse(&text(8)?))?,
        occurred_at: optional_text(10)?
            .map(|value| decoded(10, UtcTimestamp::parse(&value)))
            .transpose()?,
        recorded_at: parse_timestamp(row, 11)?,
        producer: EventProducer {
            name: text(12)?,
            version: text(13)?,
            sequence: row.get(14)?,
        },
        correlation_id: optional_text(15)?
            .map(|value| decoded(15, CorrelationId::parse(&value)))
            .transpose()?,
        causation_id: optional_text(16)?
            .map(|value| decoded(16, EventId::parse(&value)))
            .transpose()?,
        privacy_class: decoded(17, PrivacyClass::parse(&text(17)?))?,
    };
    let previous = optional_text(18)?
        .map(|value| decoded(18, Sha256Digest::parse(&value)))
        .transpose()?;
    let stored_payload = decoded(9, Sha256Digest::parse(&text(9)?))?;
    let stored_event = decoded(19, Sha256Digest::parse(&text(19)?))?;
    Event::from_stored(
        input,
        row.get(4)?,
        row.get(5)?,
        previous,
        &stored_payload,
        &stored_event,
    )
    .map_err(|error| invalid_value(19, error.to_string()))
}

fn decoded<T>(
    index: usize,
    result: Result<T, memprove_domain::DomainError>,
) -> rusqlite::Result<T> {
    result.map_err(|error| invalid_value(index, error.to_string()))
}

fn parse_timestamp(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<UtcTimestamp> {
    UtcTimestamp::parse(&row.get::<_, String>(index)?)
        .map_err(|error| invalid_value(index, error.to_string()))
}

/// A stored revision. P4-H1 (review of the first P4-H1 commit): one below
/// 1, which no row the store wrote holds (CHECK `revision >= 1`), does not
/// decode, so it is never rendered.
fn stored_revision(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let revision = row.get::<_, u64>(index)?;
    if revision == 0 {
        return Err(invalid_value(index, "revision must be at least 1"));
    }
    Ok(revision)
}

/// P4-H1 (review of the first P4-H1 commit): a TEXT value that a read
/// walking many rows uses as an identity, a type, a time, or a digest.
/// Bytes that are not UTF-8 read as U+FFFD, which no identity, type, time,
/// or digest holds, so the value names nothing and does not parse; the
/// check of its own row reports it. One such row thus never fails the read.
pub(super) fn lossy_text(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<String> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Text(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        _ => row.get(index),
    }
}

/// [`lossy_text`] of a column that may be NULL.
pub(super) fn lossy_optional_text(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<String>> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Null => Ok(None),
        rusqlite::types::ValueRef::Text(bytes) => {
            Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
        }
        _ => row.get(index),
    }
}

/// P4-H1 (review of the first P4-H1 commit): a stored JSON payload that a
/// read walking many rows parses; `None` when it is not UTF-8 JSON, so it
/// names nothing (the chain check reports the Event).
pub(super) fn stored_json(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<serde_json::Value>> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Text(bytes) => Ok(serde_json::from_slice(bytes).ok()),
        _ => row
            .get::<_, String>(index)
            .map(|text| serde_json::from_str(&text).ok()),
    }
}

fn invalid_value(index: usize, value: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            value.into(),
        )),
    )
}
