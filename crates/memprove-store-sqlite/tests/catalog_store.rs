//! Store functions behind the slice C routes: root Workspace lookup, Session
//! idempotency, keyset-paged Task and Session lists, Task Workspace binding,
//! and the active Project listing; and the T7 lifecycle writes: ending a
//! Session, moving a Task, and refusing Events for Sessions that ended.

use std::path::Path;

use memprove_domain::{
    AgentId, CanonicalJson, Event, EventId, EventInput, EventProducer, IdempotencyContext,
    IdempotencyKey, OperationId, PrincipalId, PrivacyClass, Project, ProjectId,
    ProjectProvisioningState, Session, SessionId, SessionOutcome, Sha256Digest, Task, TaskId,
    TaskStatus, UtcTimestamp, Workspace, WorkspaceId,
};
use memprove_store_sqlite::{
    Idempotent, ProjectProvisioner, RegistryStore, StateStore, StoreError,
};
use tempfile::TempDir;

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("valid timestamp")
}

fn project(data_dir: &Path, common_dir: &str, state: ProjectProvisioningState) -> Project {
    let project_id = ProjectId::new();
    Project {
        project_id: project_id.clone(),
        repository_common_dir: data_dir.join(common_dir).display().to_string(),
        object_format: "sha1".to_owned(),
        state_db_path: data_dir
            .join("projects")
            .join(project_id.as_str())
            .join("state.sqlite")
            .display()
            .to_string(),
        provisioning_state_internal: state,
        created_at: timestamp("2026-10-02T00:00:00Z"),
        updated_at: timestamp("2026-10-02T00:00:00Z"),
        revision: 1,
    }
}

fn workspace(project: &Project, path: &str, created_at: &str) -> Workspace {
    Workspace {
        workspace_id: WorkspaceId::new(),
        project_id: project.project_id.clone(),
        canonical_worktree_path: path.to_owned(),
        git_dir: format!("{path}/.git"),
        head_commit: "0123456789012345678901234567890123456789".to_owned(),
        branch_or_detached_state: "branch:main".to_owned(),
        created_at: timestamp(created_at),
        revision: 1,
    }
}

/// U1 B4 (O5): the root Workspace as `registerProject` writes it, without
/// a `workspace.registered` Event, at the main worktree, whose Git
/// directory is the Project's common directory.
fn root_workspace(project: &Project, created_at: &str) -> Workspace {
    Workspace {
        git_dir: project.repository_common_dir.clone(),
        ..workspace(project, "/repo", created_at)
    }
}

fn session(workspace: &Workspace, started_at: &str) -> Session {
    Session {
        session_id: SessionId::new(),
        workspace_id: workspace.workspace_id.clone(),
        agent_id: AgentId::new(),
        started_at: timestamp(started_at),
        ended_at: None,
        revision: 1,
        agent: None,
        ended_by: None,
    }
}

fn task(project: &Project, title: &str, created_at: &str) -> Task {
    Task {
        task_id: TaskId::new(),
        project_id: project.project_id.clone(),
        workspace_id: None,
        title: title.to_owned(),
        description: None,
        status: TaskStatus::Proposed,
        created_at: timestamp(created_at),
        updated_at: timestamp(created_at),
        revision: 1,
        completion_evidence_ids: Vec::new(),
    }
}

fn context(operation: &str, key: &str) -> IdempotencyContext {
    IdempotencyContext {
        principal_id: PrincipalId::parse("018f47a0-0000-7000-8000-000000000001").unwrap(),
        operation_id: OperationId::parse(operation).unwrap(),
        key: IdempotencyKey::parse(key).unwrap(),
    }
}

fn open_active(directory: &TempDir) -> (Project, StateStore) {
    let project = project(
        directory.path(),
        "repo.git",
        ProjectProvisioningState::Active,
    );
    let store = StateStore::open(directory.path(), &project).expect("state database");
    (project, store)
}

/// U1 B4 (original #32, O5): the root is the Workspace of the Project's
/// repository that no `workspace.registered` Event announces (the one
/// `registerProject` writes), not the first by `created_at`; before, this
/// test registered the root with `ensure_workspace` and found it by
/// `created_at`.
#[test]
fn ensure_workspace_is_idempotent_by_canonical_path_and_registration_writes_the_root() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    assert_eq!(store.root_workspace().unwrap(), None);

    let root = root_workspace(&project, "2026-10-02T00:00:01Z");
    store.create_workspace(&root).unwrap();
    let mut retry = workspace(&project, "/repo", "2026-10-02T00:00:09Z");
    retry.workspace_id = WorkspaceId::new();
    assert_eq!(
        store.ensure_workspace(&retry).unwrap(),
        Idempotent::Replayed(root.clone())
    );
    // Registered by `resolveWorkspace`, earlier by `created_at`, and its Git
    // directory the common directory too: still not the root.
    let linked = Workspace {
        git_dir: format!("{}/worktrees/linked", project.repository_common_dir),
        ..workspace(&project, "/repo-linked", "2026-10-02T00:00:00Z")
    };
    store.ensure_workspace(&linked).unwrap();

    assert_eq!(store.root_workspace().unwrap(), Some(root.clone()));
    assert_eq!(store.find_workspace_by_path("/repo").unwrap(), Some(root));
    assert_eq!(store.find_workspace_by_path("/elsewhere").unwrap(), None);
    assert!(store.has_workspace(&linked.workspace_id).unwrap());
    assert!(!store.has_workspace(&WorkspaceId::new()).unwrap());
}

#[test]
fn session_creation_replays_and_conflicts_by_idempotency_key() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    let root = workspace(&project, "/repo", "2026-10-02T00:00:01Z");
    store.ensure_workspace(&root).unwrap();

    let key = context("createSession", "session-key-0001");
    let digest = Sha256Digest::of_bytes(b"request");
    let first = session(&root, "2026-10-02T00:00:02Z");
    assert_eq!(
        store
            .create_session_idempotent(&first, None, &EventId::new(), &key, &digest)
            .unwrap(),
        Idempotent::Created(first.clone())
    );
    // A retry proposes fresh identifiers but replays the original Session.
    let retry = session(&root, "2026-10-02T00:00:03Z");
    assert_eq!(
        store
            .create_session_idempotent(&retry, None, &EventId::new(), &key, &digest)
            .unwrap(),
        Idempotent::Replayed(first.clone())
    );
    assert!(matches!(
        store.create_session_idempotent(
            &retry,
            None,
            &EventId::new(),
            &key,
            &Sha256Digest::of_bytes(b"other")
        ),
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(store.list_sessions().unwrap(), vec![first]);
    assert_eq!(store.idempotency_record_count().unwrap(), 1);

    let unknown_workspace = Session {
        workspace_id: WorkspaceId::new(),
        ..session(&root, "2026-10-02T00:00:04Z")
    };
    assert!(matches!(
        store.create_session_idempotent(
            &unknown_workspace,
            None,
            &EventId::new(),
            &context("createSession", "session-key-0002"),
            &digest
        ),
        Err(StoreError::ConstraintViolation(_))
    ));
    assert_eq!(store.idempotency_record_count().unwrap(), 1);
}

#[test]
fn session_creation_links_its_task_and_appends_session_started() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    let root = workspace(&project, "/repo", "2026-10-02T00:00:01Z");
    let linked = workspace(&project, "/repo-linked", "2026-10-02T00:00:01Z");
    store.ensure_workspace(&root).unwrap();
    store.ensure_workspace(&linked).unwrap();
    let mut scoped = task(&project, "scoped", "2026-10-02T00:00:02Z");
    scoped.workspace_id = Some(root.workspace_id.clone());
    store
        .create_task(
            &scoped,
            &context("createTask", "link-task-key-0001"),
            &Sha256Digest::of_bytes(b"scoped"),
        )
        .unwrap();
    let digest = Sha256Digest::of_bytes(b"request");
    // Two workspace.registered and one task.created Event (P2-C).
    let before = store.list_events_page(None, 10).unwrap().items;
    assert_eq!(
        before.iter().map(Event::event_type).collect::<Vec<_>>(),
        [
            "workspace.registered",
            "workspace.registered",
            "task.created"
        ]
    );
    assert!(before.iter().all(|event| event.session_id().is_none()));

    // An unknown Task and a Task scoped to another Workspace write nothing.
    assert!(matches!(
        store.create_session_idempotent(
            &session(&root, "2026-10-02T00:00:03Z"),
            Some(&TaskId::new()),
            &EventId::new(),
            &context("createSession", "link-session-0001"),
            &digest,
        ),
        Err(StoreError::NotFound("Task"))
    ));
    assert!(matches!(
        store.create_session_idempotent(
            &session(&linked, "2026-10-02T00:00:03Z"),
            Some(&scoped.task_id),
            &EventId::new(),
            &context("createSession", "link-session-0002"),
            &digest,
        ),
        Err(StoreError::SessionWorkspaceMismatch)
    ));
    assert!(store.list_sessions().unwrap().is_empty());
    assert_eq!(store.list_events_page(None, 10).unwrap().items, before);

    let open = session(&root, "2026-10-02T00:00:04Z");
    let started_id = EventId::new();
    let key = context("createSession", "link-session-0003");
    store
        .create_session_idempotent(&open, Some(&scoped.task_id), &started_id, &key, &digest)
        .unwrap();
    assert_eq!(
        store.list_task_sessions(&scoped.task_id).unwrap(),
        vec![open.session_id.clone()]
    );
    let events = store.list_events(&open.session_id).unwrap();
    // session.started, then the link's task.session_linked (P2-C).
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].event_type(), "task.session_linked");
    assert_eq!(events[1].sequence(), Some(2));
    let started = &events[0];
    assert_eq!(started.event_id(), &started_id);
    assert_eq!(started.event_type(), "session.started");
    assert_eq!(started.payload_schema(), "session.started@1");
    assert_eq!(
        started.payload().as_str(),
        format!(
            r#"{{"agent_id":"{}","task_id":"{}"}}"#,
            open.agent_id, scoped.task_id
        )
    );
    assert_eq!(started.producer().name, "continuityd");
    assert_eq!(started.producer().sequence, started.project_sequence());
    assert_eq!(started.occurred_at(), Some(&open.started_at));

    // A replay links nothing new and appends nothing.
    store
        .create_session_idempotent(
            &session(&root, "2026-10-02T00:00:05Z"),
            Some(&scoped.task_id),
            &EventId::new(),
            &key,
            &digest,
        )
        .unwrap();
    assert_eq!(store.list_events_page(None, 10).unwrap().items.len(), 5);
    store.verify_event_chain().unwrap();
}

#[test]
fn tasks_persist_their_workspace_and_description() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    let root = workspace(&project, "/repo", "2026-10-02T00:00:01Z");
    store.ensure_workspace(&root).unwrap();

    let mut bound = task(&project, "bound", "2026-10-02T00:00:02Z");
    bound.workspace_id = Some(root.workspace_id.clone());
    bound.description = Some(String::new());
    store
        .create_task(
            &bound,
            &context("createTask", "task-key-00000001"),
            &Sha256Digest::of_bytes(b"bound"),
        )
        .unwrap();
    assert_eq!(store.get_task(&bound.task_id).unwrap(), bound);

    let mut dangling = task(&project, "dangling", "2026-10-02T00:00:03Z");
    dangling.workspace_id = Some(WorkspaceId::new());
    assert!(matches!(
        store.create_task(
            &dangling,
            &context("createTask", "task-key-00000002"),
            &Sha256Digest::of_bytes(b"dangling"),
        ),
        Err(StoreError::ConstraintViolation(_))
    ));
}

#[test]
fn task_pages_follow_a_stable_keyset_order() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    // An unscoped Task's task.created is recorded under the root Workspace.
    store
        .create_workspace(&root_workspace(&project, "2026-10-02T00:00:00Z"))
        .unwrap();
    let mut created = Vec::new();
    for (index, at) in [
        "2026-10-02T00:00:03Z",
        "2026-10-02T00:00:01Z",
        "2026-10-02T00:00:02Z",
    ]
    .into_iter()
    .enumerate()
    {
        let value = task(&project, &format!("t{index}"), at);
        store
            .create_task(
                &value,
                &context("createTask", &format!("task-page-key-{index:04}")),
                &Sha256Digest::of_bytes(at.as_bytes()),
            )
            .unwrap();
        created.push(value);
    }
    created.sort_by(|a, b| (&a.created_at, &a.task_id).cmp(&(&b.created_at, &b.task_id)));

    let first = store.list_tasks_page(None, 2).unwrap();
    assert_eq!(first.items, created[..2]);
    assert!(first.has_more);
    // Three task.created Events (P2-C); the root, written as
    // `registerProject` writes it, has no Event (U1 B4).
    assert_eq!(first.snapshot_sequence, 3);
    let second = store
        .list_tasks_page(Some(&first.items[1].task_id), 2)
        .unwrap();
    assert_eq!(second.items, created[2..]);
    assert!(!second.has_more);
    let all = store.list_tasks_page(None, 500).unwrap();
    assert_eq!(all.items, created);
    assert!(!all.has_more);
    assert!(matches!(
        store.list_tasks_page(Some(&TaskId::new()), 2),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn session_pages_are_scoped_to_one_workspace() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    let root = workspace(&project, "/repo", "2026-10-02T00:00:01Z");
    let other = workspace(&project, "/repo-linked", "2026-10-02T00:00:02Z");
    store.ensure_workspace(&root).unwrap();
    store.ensure_workspace(&other).unwrap();
    let digest = Sha256Digest::of_bytes(b"request");
    let mut in_root = Vec::new();
    for index in 0..3 {
        let value = session(&root, &format!("2026-10-02T00:01:0{index}Z"));
        store
            .create_session_idempotent(
                &value,
                None,
                &EventId::new(),
                &context("createSession", &format!("root-session-{index:04}")),
                &digest,
            )
            .unwrap();
        in_root.push(value);
    }
    let foreign = session(&other, "2026-10-02T00:01:05Z");
    store
        .create_session_idempotent(
            &foreign,
            None,
            &EventId::new(),
            &context("createSession", "other-session-0001"),
            &digest,
        )
        .unwrap();

    let first = store
        .list_workspace_sessions_page(&root.workspace_id, None, 2)
        .unwrap();
    assert_eq!(first.items, in_root[..2]);
    assert!(first.has_more);
    let second = store
        .list_workspace_sessions_page(&root.workspace_id, Some(&first.items[1].session_id), 2)
        .unwrap();
    assert_eq!(second.items, in_root[2..]);
    assert!(!second.has_more);
    assert!(matches!(
        store.list_workspace_sessions_page(&root.workspace_id, Some(&foreign.session_id), 2),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store
            .list_workspace_sessions_page(&other.workspace_id, None, 100)
            .unwrap()
            .items,
        vec![foreign]
    );
}

#[test]
fn only_active_projects_are_listed() {
    let directory = TempDir::new().unwrap();
    let mut provisioner = ProjectProvisioner::open(directory.path()).unwrap();
    let first = provisioner
        .provision(
            &project(
                directory.path(),
                "a.git",
                ProjectProvisioningState::Provisioning,
            ),
            &context("registerProject", "register-key-0001"),
            &Sha256Digest::of_bytes(b"a"),
        )
        .unwrap();
    let registry = RegistryStore::open(directory.path()).unwrap();
    assert_eq!(registry.list_active_projects().unwrap(), vec![first]);
    assert_eq!(registry.active_project_count().unwrap(), 1);
}

fn event(project: &Project, session: &Session) -> EventInput {
    EventInput {
        event_id: EventId::new(),
        project_id: project.project_id.clone(),
        workspace_id: session.workspace_id.clone(),
        session_id: Some(session.session_id.clone()),
        event_type: "turn.started".to_owned(),
        payload_schema: "turn.started@1".to_owned(),
        payload: CanonicalJson::parse(r#"{"turn":1}"#).unwrap(),
        occurred_at: None,
        recorded_at: timestamp("2026-10-02T00:01:00Z"),
        producer: EventProducer {
            name: "catalog-store-test".to_owned(),
            version: "1".to_owned(),
            sequence: 1,
        },
        correlation_id: None,
        causation_id: None,
        privacy_class: PrivacyClass::Internal,
    }
}

#[test]
#[allow(clippy::too_many_lines)] // Every endSession outcome and the Event it leaves.
fn ending_a_session_is_idempotent_and_closes_it_to_every_event_writer() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    let root = workspace(&project, "/repo", "2026-10-02T00:00:01Z");
    store.ensure_workspace(&root).unwrap();
    let open = session(&root, "2026-10-02T00:00:02Z");
    store.create_session(&open).unwrap();
    store.append_event(&event(&project, &open)).unwrap();

    let key = context("endSession", "end-session-key-01");
    let digest = Sha256Digest::of_bytes(b"end");
    let at = timestamp("2026-10-02T00:02:00Z");
    assert!(matches!(
        store.end_session(
            &open.session_id,
            2,
            &at,
            None,
            SessionOutcome::Completed,
            &EventId::new(),
            &key,
            &digest
        ),
        Err(StoreError::RevisionConflict)
    ));
    let Idempotent::Created(ended) = store
        .end_session(
            &open.session_id,
            1,
            &at,
            Some("verify done"),
            SessionOutcome::Completed,
            &EventId::new(),
            &key,
            &digest,
        )
        .unwrap()
    else {
        panic!("first end must create");
    };
    assert_eq!((ended.ended_at.as_ref(), ended.revision), (Some(&at), 2));
    assert_eq!(
        store
            .end_session(
                &open.session_id,
                1,
                &at,
                None,
                SessionOutcome::Completed,
                &EventId::new(),
                &key,
                &digest
            )
            .unwrap(),
        Idempotent::Replayed(ended.clone())
    );
    assert!(matches!(
        store.end_session(
            &open.session_id,
            1,
            &at,
            None,
            SessionOutcome::Completed,
            &EventId::new(),
            &key,
            &Sha256Digest::of_bytes(b"x")
        ),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(matches!(
        store.end_session(
            &open.session_id,
            2,
            &at,
            None,
            SessionOutcome::Completed,
            &EventId::new(),
            &context("endSession", "end-session-key-02"),
            &digest
        ),
        Err(StoreError::InvalidTransition)
    ));
    assert!(matches!(
        store.end_session(
            &SessionId::new(),
            1,
            &at,
            None,
            SessionOutcome::Completed,
            &EventId::new(),
            &context("endSession", "end-session-key-03"),
            &digest
        ),
        Err(StoreError::NotFound("Session"))
    ));

    // The non-idempotent append path (used by the checkpoint bundle) and
    // ingest both refuse the ended Session; nothing is written.
    assert!(matches!(
        store.append_event(&event(&project, &open)),
        Err(StoreError::SessionNotActive)
    ));
    assert!(matches!(
        store.ingest_event(
            &event(&project, &open),
            &context("ingestEvent", "ingest-after-end-1"),
            &digest
        ),
        Err(StoreError::SessionNotActive)
    ));
    // ADR 0012: the end itself appended exactly one daemon Event, before the
    // Session closed; the replay and the refused writes appended nothing.
    let events = store.list_events(&open.session_id).unwrap();
    assert_eq!(events.len(), 2);
    let ended_event = &events[1];
    assert_eq!(ended_event.event_type(), "session.ended");
    assert_eq!(ended_event.payload_schema(), "session.ended@1");
    assert_eq!(
        ended_event.payload().as_str(),
        r#"{"reason":"verify done"}"#
    );
    assert_eq!(ended_event.producer().name, "continuityd");
    assert_eq!(ended_event.occurred_at(), Some(&at));
    store.verify_event_chain().unwrap();

    drop(store);
    let reopened = StateStore::open(directory.path(), &project).unwrap();
    assert_eq!(reopened.get_session(&open.session_id).unwrap(), ended);
}

#[test]
fn task_transitions_follow_the_state_machine_and_persist() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    store
        .create_workspace(&root_workspace(&project, "2026-10-02T00:00:00Z"))
        .unwrap();
    let proposed = task(&project, "t", "2026-10-02T00:00:01Z");
    store
        .create_task(
            &proposed,
            &context("createTask", "create-task-key-01"),
            &Sha256Digest::of_bytes(b"create"),
        )
        .unwrap();
    let at = timestamp("2026-10-02T00:03:00Z");
    let step = |store: &mut StateStore, key: &str, revision, target| {
        store.transition_task(
            &proposed.task_id,
            revision,
            target,
            &[],
            &at,
            &context("transitionTask", key),
            &Sha256Digest::of_bytes(format!("{revision}{target:?}").as_bytes()),
        )
    };
    assert!(matches!(
        step(&mut store, "transition-key-0001", 1, TaskStatus::InProgress),
        Err(StoreError::InvalidTransition)
    ));
    let Idempotent::Created(ready) =
        step(&mut store, "transition-key-0002", 1, TaskStatus::Ready).unwrap()
    else {
        panic!("first transition must create");
    };
    assert_eq!((ready.status, ready.revision), (TaskStatus::Ready, 2));
    step(&mut store, "transition-key-0003", 2, TaskStatus::InProgress).unwrap();
    assert!(matches!(
        step(&mut store, "transition-key-0004", 3, TaskStatus::Completed),
        Err(StoreError::InvalidTransition)
    ));
    assert!(matches!(
        step(&mut store, "transition-key-0005", 2, TaskStatus::Blocked),
        Err(StoreError::RevisionConflict)
    ));
    // The replay reproduces the first result, not the current row.
    assert_eq!(
        step(&mut store, "transition-key-0002", 1, TaskStatus::Ready).unwrap(),
        Idempotent::Replayed(ready)
    );
    assert!(matches!(
        store.transition_task(
            &TaskId::new(),
            1,
            TaskStatus::Ready,
            &[],
            &at,
            &context("transitionTask", "transition-key-0006"),
            &Sha256Digest::of_bytes(b"unknown"),
        ),
        Err(StoreError::NotFound("Task"))
    ));

    drop(store);
    let reopened = StateStore::open(directory.path(), &project).unwrap();
    let stored = reopened.get_task(&proposed.task_id).unwrap();
    assert_eq!(
        (stored.status, stored.revision),
        (TaskStatus::InProgress, 3)
    );
    assert_eq!(stored.updated_at, at);
}

/// P2-E (T9 S3/N1): a Task row must agree with its last Task Event; a status
/// edited past the store is located by `verifyLedger`.
#[test]
fn verify_ledger_compares_each_task_with_its_last_task_event() {
    let directory = TempDir::new().unwrap();
    let (project, mut store) = open_active(&directory);
    store
        .create_workspace(&root_workspace(&project, "2026-10-02T00:00:00Z"))
        .unwrap();
    let proposed = task(&project, "t", "2026-10-02T00:00:01Z");
    store
        .create_task(
            &proposed,
            &context("createTask", "create-task-key-01"),
            &Sha256Digest::of_bytes(b"create"),
        )
        .unwrap();
    store
        .transition_task(
            &proposed.task_id,
            1,
            TaskStatus::Ready,
            &[],
            &timestamp("2026-10-02T00:03:00Z"),
            &context("transitionTask", "transition-key-0001"),
            &Sha256Digest::of_bytes(b"ready"),
        )
        .unwrap();
    let verification = store.verify_ledger().unwrap();
    assert!(verification.is_valid(), "{verification:?}");
    assert_eq!(verification.tasks, 1);
    drop(store);

    let connection = rusqlite::Connection::open(&project.state_db_path).unwrap();
    connection
        .execute(
            "UPDATE tasks SET status = 'completed', revision = revision + 1 WHERE task_id = ?1",
            [proposed.task_id.as_str()],
        )
        .unwrap();
    drop(connection);
    let store = StateStore::open(directory.path(), &project).unwrap();
    let verification = store.verify_ledger().unwrap();
    let failure = verification.first_failure.expect("located");
    assert_eq!(failure.record_type, memprove_domain::LedgerRecordType::Task);
    assert_eq!(failure.record_id, proposed.task_id.as_str());
    assert!(failure.reason.contains("completed"), "{}", failure.reason);
}
