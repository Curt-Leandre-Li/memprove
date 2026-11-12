use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Barrier},
    thread,
};

use memprove_domain::{
    AgentId, CanonicalJson, Event, EventId, EventInput, EventProducer, IdempotencyContext,
    IdempotencyKey, OperationId, PrincipalId, PrivacyClass, Project, ProjectId,
    ProjectProvisioningState, Session, SessionId, Sha256Digest, Task, TaskId, TaskStatus,
    UtcTimestamp, Workspace, WorkspaceId,
};
use memprove_repo::GitWorkspaceResolver;
use memprove_store_sqlite::{
    Idempotent, ProjectProvisioner, RegistryStore, StateStore, StoreError,
};
use rusqlite::{Connection, types::ValueRef};
use tempfile::TempDir;

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("valid timestamp")
}

fn project(data_dir: &Path) -> Project {
    let project_id = ProjectId::new();
    Project {
        project_id: project_id.clone(),
        repository_common_dir: data_dir.join("git-common").display().to_string(),
        object_format: "sha1".to_owned(),
        state_db_path: data_dir
            .join("projects")
            .join(project_id.as_str())
            .join("state.sqlite")
            .display()
            .to_string(),
        provisioning_state_internal: ProjectProvisioningState::Provisioning,
        created_at: timestamp("2026-07-31T01:00:00Z"),
        updated_at: timestamp("2026-07-31T01:00:00Z"),
        revision: 1,
    }
}

fn context(principal: &str, key: &str) -> IdempotencyContext {
    IdempotencyContext {
        principal_id: PrincipalId::parse(principal).expect("principal"),
        operation_id: OperationId::parse("registerProject").expect("operation"),
        key: IdempotencyKey::parse(key).expect("key"),
    }
}

fn request_digest(root: &str) -> Sha256Digest {
    CanonicalJson::parse(&format!(r#"{{"root":"{root}"}}"#))
        .expect("request")
        .sha256()
}

fn assert_no_state_database_family(root: &Path, project_id: &ProjectId) {
    let state_path = root
        .join("projects")
        .join(project_id.as_str())
        .join("state.sqlite");
    for suffix in ["", "-journal", "-wal", "-shm"] {
        assert!(
            !PathBuf::from(format!("{}{suffix}", state_path.display())).exists(),
            "unexpected state database residue: {}{suffix}",
            state_path.display()
        );
    }
    assert!(
        !root.join("projects").join(project_id.as_str()).exists(),
        "unexpected partial project directory below {}",
        root.display()
    );
}

fn assert_failed_provision_has_no_registry_state(
    provisioner: &ProjectProvisioner,
    idempotency: &IdempotencyContext,
) {
    assert_eq!(provisioner.project_count().expect("Project row count"), 0);
    assert!(matches!(
        provisioner.idempotency_state(idempotency),
        Err(StoreError::NotFound("registry idempotency record"))
    ));
}

fn active_project(data_dir: &Path) -> Project {
    let mut value = project(data_dir);
    value.provisioning_state_internal = ProjectProvisioningState::Active;
    value
}

fn workspace(project: &Project, suffix: &str) -> Workspace {
    Workspace {
        workspace_id: WorkspaceId::new(),
        project_id: project.project_id.clone(),
        canonical_worktree_path: format!("{}/worktree-{suffix}", project.repository_common_dir),
        git_dir: format!("{}/worktrees/{suffix}", project.repository_common_dir),
        head_commit: "0123456789012345678901234567890123456789".to_owned(),
        branch_or_detached_state: "branch:main".to_owned(),
        created_at: timestamp("2026-07-31T01:01:00Z"),
        revision: 1,
    }
}

fn task_fixture(project: &Project, title: &str, created_at: &str) -> Task {
    Task {
        task_id: TaskId::new(),
        project_id: project.project_id.clone(),
        workspace_id: None,
        title: title.to_owned(),
        description: None,
        status: TaskStatus::Ready,
        created_at: timestamp(created_at),
        updated_at: timestamp(created_at),
        revision: 1,
        completion_evidence_ids: Vec::new(),
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

fn event_input(
    project_id: &ProjectId,
    session: &Session,
    timestamp_suffix: u64,
    event_type: &str,
    payload: &str,
) -> EventInput {
    EventInput {
        event_id: EventId::new(),
        project_id: project_id.clone(),
        workspace_id: session.workspace_id.clone(),
        session_id: Some(session.session_id.clone()),
        event_type: event_type.to_owned(),
        payload_schema: format!("{event_type}@1"),
        payload: CanonicalJson::parse(payload).expect("payload"),
        occurred_at: Some(timestamp(&format!(
            "2026-07-31T01:02:{timestamp_suffix:02}Z"
        ))),
        recorded_at: timestamp(&format!("2026-07-31T01:03:{timestamp_suffix:02}Z")),
        producer: EventProducer {
            name: "golden-path-test".to_owned(),
            version: "1".to_owned(),
            sequence: timestamp_suffix,
        },
        correlation_id: None,
        causation_id: None,
        privacy_class: PrivacyClass::Internal,
    }
}

/// A pre-envelope (ADR 0010) Event row as the predecessor schema stored it,
/// with its version 1 digest.
#[derive(Debug, Clone)]
struct PredecessorEvent {
    event_id: EventId,
    event_sha256: Sha256Digest,
}

impl PredecessorEvent {
    fn event_id(&self) -> &EventId {
        &self.event_id
    }

    fn event_sha256(&self) -> &Sha256Digest {
        &self.event_sha256
    }
}

fn state_context(principal: &str, operation: &str, key: &str) -> IdempotencyContext {
    IdempotencyContext {
        principal_id: PrincipalId::parse(principal).expect("principal"),
        operation_id: OperationId::parse(operation).expect("operation"),
        key: IdempotencyKey::parse(key).expect("key"),
    }
}

fn table_names(path: &Path) -> Vec<String> {
    let connection = Connection::open(path).expect("open database for inspection");
    let mut statement = connection
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("prepare table inventory");
    statement
        .query_map([], |row| row.get(0))
        .expect("query table inventory")
        .collect::<Result<_, _>>()
        .expect("read table inventory")
}

type StoredEventRow = (
    String,
    String,
    u64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
);

fn stored_event_rows(connection: &Connection) -> Vec<StoredEventRow> {
    connection
        .prepare(
            "SELECT event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
             FROM events ORDER BY event_id",
        )
        .expect("prepare Event byte snapshot")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .expect("query Event byte snapshot")
        .collect::<Result<Vec<_>, _>>()
        .expect("read Event byte snapshot")
}

fn table_row_bytes(connection: &Connection, table: &str, order_by: &str) -> Vec<Vec<Vec<u8>>> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY {order_by}"))
        .expect("prepare byte-equivalent row snapshot");
    let column_count = statement.column_count();
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|index| {
                    let mut encoded = Vec::new();
                    match row.get_ref(index)? {
                        ValueRef::Null => encoded.push(0),
                        ValueRef::Integer(value) => {
                            encoded.push(1);
                            encoded.extend_from_slice(&value.to_be_bytes());
                        }
                        ValueRef::Real(value) => {
                            encoded.push(2);
                            encoded.extend_from_slice(&value.to_bits().to_be_bytes());
                        }
                        ValueRef::Text(value) => {
                            encoded.push(3);
                            encoded.extend_from_slice(value);
                        }
                        ValueRef::Blob(value) => {
                            encoded.push(4);
                            encoded.extend_from_slice(value);
                        }
                    }
                    Ok(encoded)
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("query byte-equivalent row snapshot")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read byte-equivalent row snapshot")
}

fn predecessor_domain_rows(connection: &Connection) -> BTreeMap<&'static str, Vec<Vec<Vec<u8>>>> {
    [
        ("state_metadata", "metadata_key"),
        ("workspaces", "workspace_id"),
        ("tasks", "task_id"),
        ("sessions", "session_id"),
        ("events", "event_id"),
    ]
    .into_iter()
    .map(|(table, order_by)| (table, table_row_bytes(connection, table, order_by)))
    .collect()
}

/// Strips the trailing `workspace_id` and `description` columns that migration
/// 0005 appends to `tasks`, asserting that they are NULL on upgraded rows.
fn without_columns_added_by_0005_and_0006(
    mut rows: BTreeMap<&'static str, Vec<Vec<Vec<u8>>>>,
) -> BTreeMap<&'static str, Vec<Vec<Vec<u8>>>> {
    for row in rows.get_mut("tasks").expect("tasks rows") {
        let added = row.split_off(row.len() - 2);
        assert_eq!(added, vec![vec![0], vec![0]], "0005 columns must be NULL");
    }
    for row in rows.get_mut("events").expect("events rows") {
        let added = row.split_off(10);
        assert_eq!(added, vec![vec![0]; 13], "0006 columns must be NULL");
    }
    // U8 A1: so are the three agent columns of 0008; U9 A7: and the three
    // ender columns of 0009.
    for row in rows.get_mut("sessions").expect("sessions rows") {
        let added = row.split_off(row.len() - 6);
        assert_eq!(
            added,
            vec![vec![0]; 6],
            "0008 and 0009 columns must be NULL"
        );
    }
    rows
}

fn state_mutation_rows(connection: &Connection) -> BTreeMap<&'static str, Vec<Vec<Vec<u8>>>> {
    [
        ("state_metadata", "metadata_key"),
        ("workspaces", "workspace_id"),
        ("tasks", "task_id"),
        ("sessions", "session_id"),
        ("task_sessions", "task_id, session_id"),
        ("events", "event_id"),
        (
            "state_idempotency_records",
            "principal_id, operation_id, idempotency_key",
        ),
    ]
    .into_iter()
    .map(|(table, order_by)| (table, table_row_bytes(connection, table, order_by)))
    .collect()
}

fn assert_root_identity_failure<T>(result: Result<T, StoreError>) {
    match result {
        Err(StoreError::CapabilityVfs(message)) => assert!(
            message.contains("approved data root"),
            "unclear root identity error: {message}"
        ),
        Err(error) => panic!("unexpected mutation error: {error}"),
        Ok(_) => panic!("mutation accepted a replaced approved data root"),
    }
}

fn migration_records(connection: &Connection) -> Vec<(u64, String)> {
    connection
        .prepare(
            "SELECT migration_id, migration_checksum FROM schema_migrations ORDER BY migration_id",
        )
        .expect("prepare migration inventory")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query migration inventory")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read migration inventory")
}

/// The state migration of the predecessor schema, as commit
/// `c24f9eb40a754d5d46cd577c2d89f8cdf0f552bc` held it at
/// `crates/continuity-store-sqlite/migrations/state/0001_state.sql`. U22: a
/// copy in `tests/fixtures/`, checked by its digest, since a repository
/// without that history (the published one) cannot `git show` it.
fn predecessor_state_migration() -> String {
    const EXPECTED_SHA256: &str =
        "7d43fed4e6d350cdd645bdb405c0b25bb03f01f2b3f31294e4a51e1c8114ed4e";
    const MIGRATION: &str = include_str!("fixtures/predecessor-state-0001.sql");
    assert_eq!(
        Sha256Digest::of_bytes(MIGRATION.as_bytes()).as_str(),
        EXPECTED_SHA256
    );
    MIGRATION.to_owned()
}

fn initialize_predecessor_state(
    project: &Project,
) -> (Workspace, Task, Session, Vec<PredecessorEvent>) {
    let state_path = Path::new(&project.state_db_path);
    std::fs::create_dir_all(state_path.parent().expect("state parent"))
        .expect("create predecessor state directory");
    let migration = predecessor_state_migration();
    let connection = Connection::open(state_path).expect("create predecessor database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("predecessor foreign keys");
    connection
        .execute_batch(&migration)
        .expect("apply predecessor 0001");
    connection
        .execute(
            "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
             VALUES (1, ?1, ?2)",
            rusqlite::params![
                Sha256Digest::of_bytes(migration.as_bytes()).as_str(),
                "2026-07-31T01:00:00Z"
            ],
        )
        .expect("bind predecessor 0001");
    connection
        .execute(
            "INSERT INTO state_metadata (
                metadata_key, project_id, repository_common_dir_sha256,
                schema_generation, created_at
             ) VALUES (1, ?1, ?2, 1, ?3)",
            rusqlite::params![
                project.project_id.as_str(),
                Sha256Digest::of_bytes(project.repository_common_dir.as_bytes()).as_str(),
                "2026-07-31T01:00:00Z"
            ],
        )
        .expect("predecessor state binding");

    let (workspace, task, session) = insert_predecessor_domain_rows(&connection, project);
    let events = insert_predecessor_events(&connection, &session);
    (workspace, task, session, events)
}

fn insert_predecessor_domain_rows(
    connection: &Connection,
    project: &Project,
) -> (Workspace, Task, Session) {
    let workspace = workspace(project, "predecessor");
    let task = task_fixture(project, "Predecessor task", "2026-07-31T01:01:01Z");
    let session = session(&workspace, "2026-07-31T01:01:02Z");
    connection
        .execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, canonical_worktree_path, git_dir,
                head_commit, branch_or_detached_state, created_at, revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                workspace.workspace_id.as_str(),
                workspace.project_id.as_str(),
                workspace.canonical_worktree_path,
                workspace.git_dir,
                workspace.head_commit,
                workspace.branch_or_detached_state,
                workspace.created_at.as_str(),
                workspace.revision,
            ],
        )
        .expect("predecessor Workspace");
    connection
        .execute(
            "INSERT INTO tasks (
                task_id, project_id, title, status, created_at, updated_at, revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                task.task_id.as_str(),
                task.project_id.as_str(),
                task.title,
                task.status.as_str(),
                task.created_at.as_str(),
                task.updated_at.as_str(),
                task.revision,
            ],
        )
        .expect("predecessor Task");
    connection
        .execute(
            "INSERT INTO sessions (
                session_id, workspace_id, agent_id, started_at, ended_at, revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                session.session_id.as_str(),
                session.workspace_id.as_str(),
                session.agent_id.as_str(),
                session.started_at.as_str(),
                session.ended_at.as_ref().map(UtcTimestamp::as_str),
                session.revision,
            ],
        )
        .expect("predecessor Session");
    (workspace, task, session)
}

fn insert_predecessor_events(connection: &Connection, session: &Session) -> Vec<PredecessorEvent> {
    let mut previous: Option<Sha256Digest> = None;
    let mut events = Vec::new();
    for sequence in 1..=3_u64 {
        let event_id = EventId::new();
        let payload = CanonicalJson::parse(&format!(r#"{{"sequence":{sequence}}}"#))
            .expect("predecessor payload");
        let occurred_at = format!("2026-07-31T01:02:{sequence:02}Z");
        let recorded_at = format!("2026-07-31T01:03:{sequence:02}Z");
        // The version 1 digest the predecessor computed.
        let event_sha256 = CanonicalJson::parse(
            &serde_json::json!({
                "event_id": event_id.as_str(),
                "session_id": session.session_id.as_str(),
                "sequence": sequence,
                "event_type": "predecessor.recorded",
                "payload_sha256": payload.sha256().as_str(),
                "occurred_at": occurred_at,
                "recorded_at": recorded_at,
                "previous_event_sha256": previous.as_ref().map(Sha256Digest::as_str),
            })
            .to_string(),
        )
        .expect("version 1 digest input")
        .sha256();
        connection
            .execute(
                "INSERT INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    event_id.as_str(),
                    session.session_id.as_str(),
                    sequence,
                    "predecessor.recorded",
                    payload.as_str(),
                    payload.sha256().as_str(),
                    occurred_at,
                    recorded_at,
                    previous.as_ref().map(Sha256Digest::as_str),
                    event_sha256.as_str(),
                ],
            )
            .expect("insert predecessor Event");
        previous = Some(event_sha256.clone());
        events.push(PredecessorEvent {
            event_id,
            event_sha256,
        });
    }
    events
}

/// Event triggers after migration 0006.
const EVENT_TRIGGERS: [&str; 6] = [
    "events_enforce_chain",
    "events_reject_delete",
    "events_reject_pre_envelope_ledger",
    "events_reject_replace",
    "events_reject_update",
    "events_require_envelope",
];

fn event_trigger_names(connection: &Connection) -> Vec<String> {
    connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name = 'events' ORDER BY name",
        )
        .expect("prepare Event trigger inventory")
        .query_map([], |row| row.get(0))
        .expect("query Event trigger inventory")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read Event trigger inventory")
}

fn assert_all_event_replace_axes_are_immutable(
    connection: &Connection,
    events: &[PredecessorEvent],
    session: &Session,
) {
    connection
        .pragma_update(None, "recursive_triggers", "OFF")
        .expect("disable recursive triggers");
    assert_eq!(
        connection
            .pragma_query_value::<u8, _>(None, "recursive_triggers", |row| row.get(0))
            .expect("recursive trigger state"),
        0
    );
    let baseline = stored_event_rows(connection);
    let first = &events[0];
    let second = &events[1];
    let third = &events[2];

    assert!(
        connection
            .execute(
                "INSERT OR REPLACE INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) SELECT event_id, session_id, sequence, 'replace.id', payload, payload_sha256,
                          occurred_at, recorded_at, previous_event_sha256, ?2
                   FROM events WHERE event_id = ?1",
                rusqlite::params![
                    first.event_id().as_str(),
                    Sha256Digest::of_bytes(b"predecessor replacement id").as_str(),
                ],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(connection), baseline);
    assert!(
        connection
            .execute(
                "INSERT OR REPLACE INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) SELECT ?2, session_id, sequence, 'replace.sequence', payload, payload_sha256,
                          occurred_at, recorded_at, previous_event_sha256, ?3
                   FROM events WHERE event_id = ?1",
                rusqlite::params![
                    second.event_id().as_str(),
                    EventId::new().as_str(),
                    Sha256Digest::of_bytes(b"predecessor replacement sequence").as_str(),
                ],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(connection), baseline);
    assert!(
        connection
            .execute(
                "INSERT OR REPLACE INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) SELECT ?2, ?3, 4, 'replace.digest', payload, payload_sha256,
                          occurred_at, recorded_at, ?4, event_sha256
                   FROM events WHERE event_id = ?1",
                rusqlite::params![
                    first.event_id().as_str(),
                    EventId::new().as_str(),
                    session.session_id.as_str(),
                    third.event_sha256().as_str(),
                ],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(connection), baseline);
    assert!(
        connection
            .execute(
                "UPDATE events SET event_type = 'tampered' WHERE event_id = ?1",
                [first.event_id().as_str()],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM events WHERE event_id = ?1",
                [first.event_id().as_str()],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(connection), baseline);
}

#[derive(Debug, PartialEq, Eq)]
struct RepositorySnapshot {
    head: String,
    status: String,
    refs: String,
    index_sha256: Option<Sha256Digest>,
    tracked_sha256: BTreeMap<String, Sha256Digest>,
}

fn git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
}

fn real_git_repository(root: &Path) -> PathBuf {
    let repository = root.join("real-project");
    std::fs::create_dir(&repository).expect("repository directory");
    git(&repository, &["init", "--initial-branch=main"]);
    git(&repository, &["config", "user.name", "memprove Test"]);
    git(
        &repository,
        &["config", "user.email", "memprove@example.invalid"],
    );
    std::fs::write(repository.join("tracked.txt"), b"tracked fixture\n").expect("tracked file");
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "-m", "fixture"]);
    repository
}

fn repository_snapshot(repository: &Path) -> RepositorySnapshot {
    let git_dir = PathBuf::from(git(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let index_path = git_dir.join("index");
    let index_sha256 = index_path
        .is_file()
        .then(|| Sha256Digest::of_bytes(&std::fs::read(index_path).expect("index bytes")));
    let tracked_sha256 = git(repository, &["ls-files"])
        .lines()
        .map(|relative| {
            (
                relative.to_owned(),
                Sha256Digest::of_bytes(
                    &std::fs::read(repository.join(relative)).expect("tracked file"),
                ),
            )
        })
        .collect();
    RepositorySnapshot {
        head: git(repository, &["rev-parse", "--verify", "HEAD"]),
        status: git(
            repository,
            &["status", "--porcelain=v2", "--untracked-files=all"],
        ),
        refs: git(
            repository,
            &[
                "for-each-ref",
                "--sort=refname",
                "--format=%(refname)%00%(objectname)",
            ],
        ),
        index_sha256,
        tracked_sha256,
    }
}

#[test]
fn registry_migration_is_isolated_idempotent_and_checksum_bound() {
    let directory = TempDir::new().expect("temporary data directory");
    let database_path = directory.path().join("registry.sqlite");

    let store = RegistryStore::open(directory.path()).expect("fresh registry");
    assert_eq!(store.database_path(), database_path);
    assert_eq!(
        table_names(store.database_path()),
        vec![
            "client_token_scopes",
            "client_tokens",
            "projects",
            "registry_idempotency_records",
            "schema_migrations",
            "token_audit_events",
            "token_idempotency_records"
        ]
    );
    drop(store);

    RegistryStore::open(directory.path()).expect("migration reopens cleanly");
    let connection = Connection::open(&database_path).expect("open database");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get::<_, u64>(0)
            })
            .expect("migration count"),
        2
    );
    connection
        .execute(
            "UPDATE schema_migrations SET migration_checksum = 'broken'",
            [],
        )
        .expect("inject checksum drift");
    drop(connection);

    assert!(matches!(
        RegistryStore::open(directory.path()),
        Err(StoreError::MigrationDrift { .. })
    ));
}

#[test]
fn registry_begin_is_atomic_and_subject_scoped() {
    let directory = TempDir::new().expect("temporary data directory");
    let mut store = RegistryStore::open(directory.path()).expect("registry");
    let project = project(directory.path());
    let digest = request_digest(&project.repository_common_dir);
    let first_context = context("test-principal-a", "same-key");

    assert!(matches!(
        store
            .register_project_begin(&project, &first_context, &digest)
            .expect("first registration"),
        Idempotent::Created(_)
    ));
    assert!(matches!(
        store
            .register_project_begin(&project, &first_context, &digest)
            .expect("same request replay"),
        Idempotent::Replayed(_)
    ));
    assert!(matches!(
        store.register_project_begin(
            &project,
            &first_context,
            &Sha256Digest::of_bytes(b"different")
        ),
        Err(StoreError::IdempotencyConflict)
    ));

    let independent_context = context("test-principal-b", "same-key");
    assert!(matches!(
        store
            .register_project_begin(&project, &independent_context, &digest)
            .expect("independent namespace"),
        Idempotent::Replayed(_)
    ));

    let connection = Connection::open(store.database_path()).expect("open database");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM registry_idempotency_records",
                [],
                |row| row.get::<_, u64>(0)
            )
            .expect("idempotency count"),
        2
    );
    assert_eq!(
        store
            .get_project(&project.project_id)
            .expect("persisted project"),
        project
    );
}

#[test]
fn failed_registry_transaction_does_not_leave_idempotency_success() {
    let directory = TempDir::new().expect("temporary data directory");
    let mut store = RegistryStore::open(directory.path()).expect("registry");
    let mut invalid = project(directory.path());
    invalid.revision = 0;
    let idempotency = context("test-principal-a", "invalid-project");

    assert!(matches!(
        store.register_project_begin(
            &invalid,
            &idempotency,
            &request_digest(&invalid.repository_common_dir)
        ),
        Err(StoreError::ConstraintViolation(_))
    ));

    let connection = Connection::open(store.database_path()).expect("open database");
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM registry_idempotency_records",
                [],
                |row| row.get::<_, u64>(0)
            )
            .expect("idempotency count"),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM projects", [], |row| {
                row.get::<_, u64>(0)
            })
            .expect("project count"),
        0
    );
}

#[test]
fn state_migration_is_isolated_reopenable_and_project_bound() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());

    let store = StateStore::open(directory.path(), &project).expect("fresh state database");
    assert_eq!(
        table_names(store.database_path()),
        vec![
            "checkpoint_item_evidence",
            "checkpoint_items",
            "checkpoints",
            "events",
            "evidence",
            "repository_snapshot_evidence",
            "repository_snapshots",
            "schema_migrations",
            "sessions",
            "state_idempotency_records",
            "state_metadata",
            "task_sessions",
            "tasks",
            "workspaces"
        ]
    );
    let state_path = store.database_path().to_owned();
    drop(store);

    StateStore::open(directory.path(), &project).expect("state reopens cleanly");
    let connection = Connection::open(&state_path).expect("inspect migration chain");
    assert_eq!(
        migration_records(&connection)
            .into_iter()
            .map(|record| record.0)
            .collect::<Vec<_>>(),
        // U8 A1: migration 0008; U9 A7: 0009.
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
    drop(connection);
    let mut wrong_project = project.clone();
    wrong_project.project_id = ProjectId::new();
    assert!(matches!(
        StateStore::open(directory.path(), &wrong_project),
        Err(StoreError::StateBindingMismatch)
    ));

    let connection = Connection::open(&state_path).expect("open state database");
    connection
        .execute(
            "UPDATE schema_migrations SET migration_checksum = 'broken'",
            [],
        )
        .expect("inject checksum drift");
    drop(connection);
    assert!(matches!(
        StateStore::open(directory.path(), &project),
        Err(StoreError::MigrationDrift { .. })
    ));
}

#[test]
fn state_migration_upgrades_real_predecessor_database_without_rewriting_rows() {
    const PREDECESSOR_SHA256: &str =
        "7d43fed4e6d350cdd645bdb405c0b25bb03f01f2b3f31294e4a51e1c8114ed4e";

    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let (workspace, task, session, events) = initialize_predecessor_state(&project);
    let state_path = Path::new(&project.state_db_path);
    let connection = Connection::open(state_path).expect("inspect predecessor database");
    let before = predecessor_domain_rows(&connection);
    assert_eq!(
        migration_records(&connection),
        vec![(1, PREDECESSOR_SHA256.to_owned())]
    );
    drop(connection);

    let upgraded = StateStore::open(directory.path(), &project).expect("upgrade predecessor");
    assert_eq!(
        upgraded
            .get_workspace(&workspace.workspace_id)
            .expect("preserved Workspace"),
        workspace
    );
    assert_eq!(
        upgraded.get_task(&task.task_id).expect("preserved Task"),
        task
    );
    assert_eq!(
        upgraded
            .get_session(&session.session_id)
            .expect("preserved Session"),
        session
    );
    // ADR 0010 D7: pre-envelope rows are preserved byte for byte, but this
    // build neither reads, verifies, nor appends to a ledger holding them.
    assert!(matches!(
        upgraded.list_events(&session.session_id),
        Err(StoreError::Database(_))
    ));
    assert!(matches!(
        upgraded.get_event(events[0].event_id()),
        Err(StoreError::Database(_))
    ));
    assert!(matches!(
        upgraded.verify_event_chain(),
        Err(StoreError::InvalidPersistedValue(_))
    ));
    let mut upgraded = upgraded;
    assert!(matches!(
        upgraded.append_event(&event_input(
            &project.project_id,
            &session,
            9,
            "turn.started",
            "{}",
        )),
        Err(StoreError::InvalidPersistedValue(_))
    ));
    drop(upgraded);

    let connection = Connection::open(state_path).expect("inspect upgraded database");
    assert_eq!(
        without_columns_added_by_0005_and_0006(predecessor_domain_rows(&connection)),
        before
    );
    let records = migration_records(&connection);
    // U8 A1: migration 0008; U9 A7: 0009.
    assert_eq!(records.len(), 9);
    assert_eq!(records[0], (1, PREDECESSOR_SHA256.to_owned()));
    assert_eq!(records[1].0, 2);
    assert_eq!(event_trigger_names(&connection), EVENT_TRIGGERS);
    // A raw pre-envelope insert is rejected by the 0006 triggers.
    assert!(
        connection
            .execute(
                "INSERT INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) VALUES (?1, ?2, 4, 'late.v1', '{}', ?3, ?4, ?4, ?5, ?6)",
                rusqlite::params![
                    EventId::new().as_str(),
                    session.session_id.as_str(),
                    Sha256Digest::of_bytes(b"{}").as_str(),
                    "2026-07-31T01:04:00Z",
                    events[2].event_sha256().as_str(),
                    Sha256Digest::of_bytes(b"late v1 row").as_str(),
                ],
            )
            .is_err()
    );
    assert_all_event_replace_axes_are_immutable(&connection, &events, &session);
    drop(connection);

    StateStore::open(directory.path(), &project).expect("idempotent migration reopen");
    let connection = Connection::open(state_path).expect("inspect idempotent reopen");
    assert_eq!(migration_records(&connection), records);
    assert_eq!(
        without_columns_added_by_0005_and_0006(predecessor_domain_rows(&connection)),
        before
    );
}

#[test]
fn state_migration_failure_rolls_back_version_two_and_reruns_once() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    initialize_predecessor_state(&project);
    let state_path = Path::new(&project.state_db_path);
    let connection = Connection::open(state_path).expect("prepare partial predecessor state");
    connection
        .execute_batch("ALTER TABLE events RENAME TO events_partial")
        .expect("inject missing Event table during migration");
    drop(connection);

    assert!(StateStore::open(directory.path(), &project).is_err());
    let connection = Connection::open(state_path).expect("inspect failed migration");
    assert_eq!(migration_records(&connection).len(), 1);
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'trigger' AND name IN (
                    'events_enforce_chain', 'events_reject_delete',
                    'events_reject_replace', 'events_reject_update'
                 )",
                [],
                |row| row.get::<_, u64>(0),
            )
            .expect("rolled-back Event trigger count"),
        4
    );
    connection
        .execute_batch("ALTER TABLE events_partial RENAME TO events")
        .expect("restore Event table for migration rerun");
    drop(connection);

    StateStore::open(directory.path(), &project).expect("rerun version two");
    StateStore::open(directory.path(), &project).expect("reopen after version two");
    let connection = Connection::open(state_path).expect("inspect rerun");
    // U8 A1: migration 0008; U9 A7: 0009.
    assert_eq!(migration_records(&connection).len(), 9);
    assert_eq!(event_trigger_names(&connection), EVENT_TRIGGERS);
}

#[test]
fn state_migration_inventory_rejects_unknown_missing_and_out_of_order_records() {
    for mutation in ["unknown-checksum", "missing-version", "out-of-order"] {
        let directory = TempDir::new().expect("temporary data directory");
        let project = active_project(directory.path());
        let store = StateStore::open(directory.path(), &project).expect("fresh migration chain");
        let state_path = store.database_path().to_owned();
        drop(store);
        let connection = Connection::open(&state_path).expect("mutate migration inventory");
        // U8 A1: migration 0008; U9 A7: 0009.
        assert_eq!(migration_records(&connection).len(), 9);
        match mutation {
            "unknown-checksum" => {
                connection
                    .execute(
                        "UPDATE schema_migrations SET migration_checksum = 'broken'
                         WHERE migration_id = 1",
                        [],
                    )
                    .expect("inject unknown checksum");
            }
            "missing-version" => {
                connection
                    .execute("DELETE FROM schema_migrations WHERE migration_id = 1", [])
                    .expect("remove migration record");
            }
            "out-of-order" => {
                connection
                    .execute(
                        // U8 A1: 8 is now taken (migration 0008), so 9; U9 A7:
                        // and 9 (migration 0009), so 10.
                        "UPDATE schema_migrations SET migration_id = 10 WHERE migration_id = 2",
                        [],
                    )
                    .expect("inject out-of-order version");
            }
            _ => unreachable!(),
        }
        drop(connection);
        assert!(
            matches!(
                StateStore::open(directory.path(), &project),
                Err(StoreError::MigrationDrift { .. })
            ),
            "mutation {mutation:?} did not fail closed"
        );
    }
}

/// The schema and the migration record as `sqlite_master` and
/// `schema_migrations` hold them, `applied_at` included.
fn schema_and_record(connection: &Connection) -> (Vec<(String, String, Option<String>)>, String) {
    let schema = connection
        .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
        .expect("schema")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("schema rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema values");
    let record = connection
        .query_row(
            "SELECT coalesce(group_concat(migration_id || ':' || applied_at, ','), '')
             FROM (SELECT * FROM schema_migrations ORDER BY migration_id)",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    (schema, record)
}

/// T21 (V1-S-1): a migration record whose last rows are missing, or which
/// is missing altogether, is drift when the schema already shows those
/// migrations applied; opening the database applies nothing and changes
/// nothing (before: a missing row 7 re-applied 0007 and inserted a new row
/// on a read). A genuine 0006 database still upgrades
/// (`events_rebuild.rs`).
#[test]
fn state_migration_inventory_rejects_trailing_records_missing_from_an_applied_schema() {
    for mutation in [
        "DELETE FROM schema_migrations WHERE migration_id = 7",
        "DELETE FROM schema_migrations WHERE migration_id >= 6",
        "DELETE FROM schema_migrations WHERE migration_id >= 2",
        "DROP TABLE schema_migrations",
    ] {
        let directory = TempDir::new().expect("temporary data directory");
        let project = active_project(directory.path());
        let store = StateStore::open(directory.path(), &project).expect("fresh migration chain");
        let state_path = store.database_path().to_owned();
        drop(store);
        let connection = Connection::open(&state_path).expect("mutate migration inventory");
        connection.execute_batch(mutation).expect("mutation");
        let before = schema_and_record(&connection);
        drop(connection);
        let opened = StateStore::open(directory.path(), &project);
        assert!(
            matches!(opened, Err(StoreError::MigrationDrift { .. })),
            "mutation {mutation:?} did not fail closed: {:?}",
            opened.map(|_| ())
        );
        let connection = Connection::open(&state_path).expect("inspect after the open");
        assert_eq!(
            schema_and_record(&connection),
            before,
            "mutation {mutation:?}: the open changed the database"
        );
    }
}

/// A change made to a state database file at `path`.
type ChangeState = fn(&Path);

/// T21 (V1-R-2): `open_existing`, with which `memproved` opens an ACTIVE
/// Project's state database, never creates one. A missing file or
/// directory, a file of 0 bytes, and a database without any object are
/// `StateBindingMismatch`, and the data directory is unchanged (before:
/// the open created and migrated an empty ledger bound to the Project); a
/// database whose migration table alone was dropped stays drift; an intact
/// one opens. `open`, the provisioning saga's step, still creates.
#[test]
fn opening_an_existing_state_database_never_creates_one() {
    let files = |path: &Path| -> Option<Vec<(PathBuf, Vec<u8>)>> {
        let mut found = fs::read_dir(path.parent()?)
            .ok()?
            .map(|entry| {
                let path = entry.expect("entry").path();
                let bytes = fs::read(&path).expect("read");
                (path, bytes)
            })
            .collect::<Vec<_>>();
        found.sort();
        Some(found)
    };
    let cases: [(&str, ChangeState); 5] = [
        ("never created", |path| {
            fs::remove_dir_all(path.parent().expect("directory")).expect("remove");
        }),
        ("deleted", |path| fs::remove_file(path).expect("remove")),
        ("0 bytes", |path| fs::write(path, b"").expect("truncate")),
        ("every object dropped", |path| {
            let connection = Connection::open(path).expect("open");
            let tables = connection
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
                .expect("prepare")
                .query_map([], |row| row.get::<_, String>(0))
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("tables");
            for table in tables {
                connection
                    .execute_batch(&format!("DROP TABLE IF EXISTS \"{table}\""))
                    .expect("drop");
            }
        }),
        ("intact", |_| {}),
    ];
    for (case, change) in cases {
        let directory = TempDir::new().expect("temporary data directory");
        let project = active_project(directory.path());
        let state_path = StateStore::open(directory.path(), &project)
            .expect("provisioned database")
            .database_path()
            .to_owned();
        change(&state_path);
        let before = files(&state_path);
        let opened = StateStore::open_existing(directory.path(), &project);
        if case == "intact" {
            assert!(opened.is_ok(), "{case}: {:?}", opened.map(|_| ()));
            continue;
        }
        assert!(
            matches!(opened, Err(StoreError::StateBindingMismatch)),
            "{case}: {:?}",
            opened.map(|_| ())
        );
        assert_eq!(files(&state_path), before, "{case}: the open wrote a file");
        if case == "never created" {
            assert_no_state_database_family(directory.path(), &project.project_id);
        }
    }

    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let state_path = StateStore::open(directory.path(), &project)
        .expect("provisioned database")
        .database_path()
        .to_owned();
    Connection::open(&state_path)
        .expect("open")
        .execute_batch("DROP TABLE schema_migrations")
        .expect("drop");
    let before = files(&state_path);
    assert!(matches!(
        StateStore::open_existing(directory.path(), &project),
        Err(StoreError::MigrationDrift { .. })
    ));
    assert_eq!(files(&state_path), before);
}

#[test]
fn state_records_preserve_session_semantics_and_task_links() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let workspace = workspace(&project, "root");
    let task = task_fixture(&project, "Primary task", "2026-07-31T01:02:00Z");
    let other_task = task_fixture(&project, "Other task", "2026-07-31T01:02:01Z");
    let first_session = session(&workspace, "2026-07-31T01:03:00Z");
    let second_session = session(&workspace, "2026-07-31T01:03:01Z");
    let mut store = StateStore::open(directory.path(), &project).expect("state database");

    store.create_workspace(&workspace).expect("workspace");
    store
        .create_task(
            &task,
            &state_context("test-principal-a", "createTask", "task-one"),
            &CanonicalJson::parse(r#"{"title":"Primary task"}"#)
                .expect("request")
                .sha256(),
        )
        .expect("task");
    store
        .create_task(
            &other_task,
            &state_context("test-principal-a", "createTask", "task-two"),
            &CanonicalJson::parse(r#"{"title":"Other task"}"#)
                .expect("request")
                .sha256(),
        )
        .expect("other task");
    store.create_session(&first_session).expect("first session");
    store
        .create_session(&second_session)
        .expect("second session");
    store
        .link_session_to_task(
            &task.task_id,
            &first_session.session_id,
            &timestamp("2026-07-31T01:04:00Z"),
        )
        .expect("first link");
    store
        .link_session_to_task(
            &task.task_id,
            &second_session.session_id,
            &timestamp("2026-07-31T01:04:01Z"),
        )
        .expect("second link");

    assert!(matches!(
        store.link_session_to_task(
            &other_task.task_id,
            &first_session.session_id,
            &timestamp("2026-07-31T01:04:02Z")
        ),
        Err(StoreError::ConstraintViolation(_))
    ));
    assert_eq!(
        store.list_sessions().expect("ordered sessions"),
        vec![first_session.clone(), second_session.clone()]
    );
    assert_eq!(
        store
            .list_task_sessions(&task.task_id)
            .expect("task sessions"),
        vec![
            first_session.session_id.clone(),
            second_session.session_id.clone()
        ]
    );

    let connection = Connection::open(store.database_path()).expect("open database");
    let columns = connection
        .prepare("PRAGMA table_info(sessions)")
        .expect("prepare columns")
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query columns")
        .collect::<Result<Vec<_>, _>>()
        .expect("session columns");
    assert_eq!(
        columns,
        vec![
            "session_id",
            "workspace_id",
            "agent_id",
            "started_at",
            "ended_at",
            "revision",
            // U8 A1 (migration 0008).
            "agent_name",
            "agent_role",
            "parent_session_id",
            // U9 A7 (migration 0009).
            "ended_by_session_id",
            "ended_by_agent_name",
            "ended_by_agent_role"
        ]
    );
    assert!(!columns.iter().any(|column| column == "task_id"));
    assert!(!columns.iter().any(|column| column == "client_kind"));
}

#[test]
fn state_idempotency_is_subject_scoped_and_rolls_back_failures() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let task = task_fixture(&project, "Idempotent task", "2026-07-31T01:02:00Z");
    let digest = CanonicalJson::parse(r#"{"title":"Idempotent task"}"#)
        .expect("request")
        .sha256();
    let mut store = StateStore::open(directory.path(), &project).expect("state database");
    // task.created is recorded under the root Workspace (P2-C).
    store
        .create_workspace(&workspace(&project, "root"))
        .expect("root Workspace");
    let first = state_context("test-principal-a", "createTask", "same-key");

    assert!(matches!(
        store.create_task(&task, &first, &digest).expect("create"),
        Idempotent::Created(_)
    ));
    assert!(matches!(
        store.create_task(&task, &first, &digest).expect("replay"),
        Idempotent::Replayed(_)
    ));
    assert!(matches!(
        store.create_task(&task, &first, &Sha256Digest::of_bytes(b"different")),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(matches!(
        store
            .create_task(
                &task,
                &state_context("test-principal-b", "createTask", "same-key"),
                &digest
            )
            .expect("independent namespace"),
        Idempotent::Replayed(_)
    ));

    let mut invalid = task_fixture(&project, "Invalid", "2026-07-31T01:02:02Z");
    invalid.revision = 0;
    assert!(matches!(
        store.create_task(
            &invalid,
            &state_context("test-principal-a", "createTask", "invalid"),
            &Sha256Digest::of_bytes(b"invalid")
        ),
        Err(StoreError::ConstraintViolation(_))
    ));
    assert_eq!(
        store.idempotency_record_count().expect("idempotency count"),
        2
    );
}

#[test]
fn in_progress_state_idempotency_retries_deterministically() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let task = task_fixture(&project, "Recovered task", "2026-07-31T01:02:00Z");
    let request = CanonicalJson::parse(r#"{"title":"Recovered task"}"#)
        .expect("request")
        .sha256();
    let idempotency = state_context("test-principal-a", "createTask", "in-progress");
    let mut store = StateStore::open(directory.path(), &project).expect("state database");
    // task.created is recorded under the root Workspace (P2-C).
    store
        .create_workspace(&workspace(&project, "root"))
        .expect("root Workspace");
    let connection = Connection::open(store.database_path()).expect("open database");
    connection
        .execute(
            "INSERT INTO state_idempotency_records (
                principal_id, operation_id, idempotency_key, canonical_request_sha256,
                state, result_record_type, result_record_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'IN_PROGRESS', NULL, NULL, ?5, ?5)",
            rusqlite::params![
                idempotency.principal_id.as_str(),
                idempotency.operation_id.as_str(),
                idempotency.key.as_str(),
                request.as_str(),
                task.created_at.as_str()
            ],
        )
        .expect("inject recoverable in-progress row");
    drop(connection);

    assert!(matches!(
        store
            .create_task(&task, &idempotency, &request)
            .expect("deterministic retry"),
        Idempotent::Created(_)
    ));
    assert_eq!(store.get_task(&task.task_id).expect("task"), task);
    let connection = Connection::open(store.database_path()).expect("open database");
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM state_idempotency_records
                 WHERE principal_id = ?1 AND operation_id = ?2 AND idempotency_key = ?3",
                rusqlite::params![
                    idempotency.principal_id.as_str(),
                    idempotency.operation_id.as_str(),
                    idempotency.key.as_str()
                ],
                |row| row.get::<_, String>(0)
            )
            .expect("idempotency state"),
        "SUCCEEDED"
    );
}

#[test]
fn events_are_append_only_ordered_and_persist_across_reopen() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let workspace = workspace(&project, "root");
    let session = session(&workspace, "2026-07-31T01:03:00Z");
    let mut store = StateStore::open(directory.path(), &project).expect("state database");
    store.create_workspace(&workspace).expect("workspace");
    store.create_session(&session).expect("session");
    let first = store
        .append_event(&event_input(
            &project.project_id,
            &session,
            1,
            "session.started",
            r#"{"b":2,"a":1}"#,
        ))
        .expect("first event");
    let second = store
        .append_event(&event_input(
            &project.project_id,
            &session,
            2,
            "turn.started",
            r#"{"turn":1}"#,
        ))
        .expect("second event");
    assert_eq!(
        store.list_events(&session.session_id).expect("events"),
        vec![first.clone(), second.clone()]
    );
    store.verify_event_chain().expect("valid chain");

    let connection = Connection::open(store.database_path()).expect("open database");
    connection
        .pragma_update(None, "recursive_triggers", "ON")
        .expect("recursive triggers");
    assert!(
        connection
            .execute(
                "UPDATE events SET event_type = 'tampered' WHERE event_id = ?1",
                [first.event_id().as_str()]
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM events WHERE event_id = ?1",
                [first.event_id().as_str()]
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "INSERT OR REPLACE INTO events (
                event_id, session_id, sequence, event_type, payload, payload_sha256,
                occurred_at, recorded_at, previous_event_sha256, event_sha256
             ) SELECT event_id, session_id, sequence, event_type, payload, payload_sha256,
                      occurred_at, recorded_at, previous_event_sha256, event_sha256
               FROM events WHERE event_id = ?1",
                [first.event_id().as_str()]
            )
            .is_err()
    );
    drop(connection);
    drop(store);

    let reopened = StateStore::open(directory.path(), &project).expect("reopen");
    assert_eq!(
        reopened
            .list_events(&session.session_id)
            .expect("persisted events"),
        vec![first, second]
    );
    reopened.verify_event_chain().expect("persisted chain");
}

#[test]
fn every_state_mutation_fails_closed_after_open_root_is_replaced() {
    let container = TempDir::new().expect("container");
    let approved = container.path().join("approved");
    let original = container.path().join("approved-original");
    fs::create_dir(&approved).expect("approved root");
    let outside = TempDir::new().expect("replacement target");
    let project = active_project(&approved);
    let existing_workspace = workspace(&project, "existing");
    let existing_task = task_fixture(&project, "Existing task", "2026-07-31T01:02:00Z");
    let existing_session = session(&existing_workspace, "2026-07-31T01:03:00Z");
    let mut store = StateStore::open(&approved, &project).expect("state database");
    store
        .create_workspace(&existing_workspace)
        .expect("existing workspace");
    store
        .create_task(
            &existing_task,
            &state_context("test-principal", "createTask", "existing-task"),
            &CanonicalJson::parse(r#"{"title":"Existing task"}"#)
                .expect("request")
                .sha256(),
        )
        .expect("existing task");
    store
        .create_session(&existing_session)
        .expect("existing session");
    let first_event = store
        .append_event(&event_input(
            &project.project_id,
            &existing_session,
            1,
            "session.started",
            r#"{"turn":0}"#,
        ))
        .expect("initial event");
    let baseline_connection = Connection::open(store.database_path()).expect("baseline database");
    let baseline_rows = state_mutation_rows(&baseline_connection);
    drop(baseline_connection);

    fs::rename(&approved, &original).expect("move approved root");
    symlink(outside.path(), &approved).expect("replace root with symlink");

    let proposed_workspace = workspace(&project, "rejected");
    let proposed_task = task_fixture(&project, "Rejected task", "2026-07-31T01:02:02Z");
    let proposed_session = session(&existing_workspace, "2026-07-31T01:03:02Z");
    let outcomes = [
        store.create_workspace(&proposed_workspace),
        store
            .create_task(
                &proposed_task,
                &state_context("test-principal", "createTask", "rejected-task"),
                &CanonicalJson::parse(r#"{"title":"Rejected task"}"#)
                    .expect("request")
                    .sha256(),
            )
            .map(|_| ()),
        store.create_session(&proposed_session),
        store.link_session_to_task(
            &existing_task.task_id,
            &existing_session.session_id,
            &timestamp("2026-07-31T01:04:00Z"),
        ),
        store
            .append_event(&event_input(
                &project.project_id,
                &existing_session,
                2,
                "turn.started",
                r#"{"turn":1}"#,
            ))
            .map(|_| ()),
    ];
    for outcome in outcomes {
        assert_root_identity_failure(outcome);
    }

    assert_eq!(
        store
            .list_events(&existing_session.session_id)
            .expect("unchanged event chain"),
        vec![first_event]
    );
    store
        .verify_event_chain()
        .expect("unchanged chain remains valid");
    drop(store);

    let original_database = original
        .join("projects")
        .join(project.project_id.as_str())
        .join("state.sqlite");
    let connection = Connection::open(original_database).expect("original opened database");
    assert_eq!(state_mutation_rows(&connection), baseline_rows);
    assert!(
        fs::read_dir(outside.path())
            .expect("replacement target")
            .next()
            .is_none(),
        "replacement root received memprove writes"
    );
}

#[test]
fn event_append_repeatedly_fails_without_residue_for_every_root_replacement_kind() {
    for replacement in ["directory", "symlink", "file", "missing"] {
        let container = TempDir::new().expect("container");
        let approved = container.path().join("approved");
        let original = container.path().join("approved-original");
        fs::create_dir(&approved).expect("approved root");
        let outside = TempDir::new().expect("symlink target");
        let project = active_project(&approved);
        let workspace = workspace(&project, "root");
        let session = session(&workspace, "2026-07-31T01:03:00Z");
        let mut store = StateStore::open(&approved, &project).expect("state database");
        store.create_workspace(&workspace).expect("workspace");
        store.create_session(&session).expect("session");
        let first_event = store
            .append_event(&event_input(
                &project.project_id,
                &session,
                1,
                "session.started",
                r#"{"turn":0}"#,
            ))
            .expect("initial event");
        let baseline_connection =
            Connection::open(store.database_path()).expect("baseline database");
        let baseline_rows = stored_event_rows(&baseline_connection);
        drop(baseline_connection);

        fs::rename(&approved, &original).expect("move approved root");
        match replacement {
            "directory" => fs::create_dir(&approved).expect("replacement directory"),
            "symlink" => symlink(outside.path(), &approved).expect("replacement symlink"),
            "file" => fs::write(&approved, b"replacement-file").expect("replacement file"),
            "missing" => {}
            _ => unreachable!(),
        }

        for timestamp_suffix in [2, 3] {
            assert_root_identity_failure(store.append_event(&event_input(
                &project.project_id,
                &session,
                timestamp_suffix,
                "turn.started",
                r#"{"turn":1}"#,
            )));
        }
        assert_eq!(
            store
                .list_events(&session.session_id)
                .expect("unchanged event chain"),
            vec![first_event]
        );
        store
            .verify_event_chain()
            .expect("unchanged chain remains valid");
        drop(store);

        let original_database = original
            .join("projects")
            .join(project.project_id.as_str())
            .join("state.sqlite");
        let connection = Connection::open(original_database).expect("original opened database");
        assert_eq!(stored_event_rows(&connection), baseline_rows);
        match replacement {
            "directory" => assert!(
                fs::read_dir(&approved)
                    .expect("replacement directory")
                    .next()
                    .is_none(),
                "replacement directory received memprove writes"
            ),
            "symlink" => assert!(
                fs::read_dir(outside.path())
                    .expect("replacement target")
                    .next()
                    .is_none(),
                "replacement symlink target received memprove writes"
            ),
            "file" => assert_eq!(
                fs::read(&approved).expect("replacement file"),
                b"replacement-file"
            ),
            "missing" => assert!(!approved.exists()),
            _ => unreachable!(),
        }
    }
}

#[test]
fn forged_event_digest_is_rejected_before_any_row_is_written() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let workspace = workspace(&project, "root");
    let session = session(&workspace, "2026-07-31T01:03:00Z");
    let mut store = StateStore::open(directory.path(), &project).expect("state database");
    store.create_workspace(&workspace).expect("workspace");
    store.create_session(&session).expect("session");
    let input = event_input(
        &project.project_id,
        &session,
        1,
        "session.started",
        r#"{"ok":true}"#,
    );
    let valid = Event::new(input.clone(), Some(1), 1, None).expect("valid Event");
    let forged = Event::from_stored(
        input,
        Some(1),
        1,
        None,
        &Sha256Digest::of_bytes(b"forged"),
        valid.event_sha256(),
    );

    assert!(forged.is_err());
    let connection = Connection::open(store.database_path()).expect("inspect database");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, u64>(0))
            .expect("event row count"),
        0
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One atomic invariant test snapshots every REPLACE conflict axis.
fn append_only_triggers_reject_all_replace_axes_with_recursive_triggers_disabled() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let workspace = workspace(&project, "root");
    let primary_session = session(&workspace, "2026-07-31T01:03:00Z");
    let other_session = session(&workspace, "2026-07-31T01:03:01Z");
    let mut store = StateStore::open(directory.path(), &project).expect("state database");
    store.create_workspace(&workspace).expect("workspace");
    store.create_session(&primary_session).expect("session");
    store.create_session(&other_session).expect("other session");
    let first = store
        .append_event(&event_input(
            &project.project_id,
            &primary_session,
            1,
            "session.started",
            r#"{"turn":0}"#,
        ))
        .expect("first Event");
    let second = store
        .append_event(&event_input(
            &project.project_id,
            &primary_session,
            2,
            "turn.started",
            r#"{"turn":1}"#,
        ))
        .expect("second Event");

    let connection = Connection::open(store.database_path()).expect("independent connection");
    connection
        .pragma_update(None, "recursive_triggers", "OFF")
        .expect("disable recursive triggers");
    assert_eq!(
        connection
            .pragma_query_value::<u8, _>(None, "recursive_triggers", |row| row.get(0))
            .expect("recursive trigger state"),
        0
    );
    let baseline = stored_event_rows(&connection);

    assert!(
        connection
            .execute(
                "UPDATE events SET event_type = 'tampered' WHERE event_id = ?1",
                [first.event_id().as_str()],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(&connection), baseline);
    assert!(
        connection
            .execute(
                "DELETE FROM events WHERE event_id = ?1",
                [first.event_id().as_str()],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(&connection), baseline);

    assert!(
        connection
            .execute(
                "INSERT OR REPLACE INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) SELECT event_id, ?2, 1, 'replaced.id', payload, payload_sha256,
                          occurred_at, recorded_at, NULL, ?3
                   FROM events WHERE event_id = ?1",
                rusqlite::params![
                    first.event_id().as_str(),
                    other_session.session_id.as_str(),
                    Sha256Digest::of_bytes(b"replacement event-id axis").as_str(),
                ],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(&connection), baseline);

    assert!(
        connection
            .execute(
                "INSERT OR REPLACE INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) SELECT ?2, session_id, sequence, 'replaced.sequence', payload, payload_sha256,
                          occurred_at, recorded_at, previous_event_sha256, ?3
                   FROM events WHERE event_id = ?1",
                rusqlite::params![
                    second.event_id().as_str(),
                    EventId::new().as_str(),
                    Sha256Digest::of_bytes(b"replacement sequence axis").as_str(),
                ],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(&connection), baseline);

    assert!(
        connection
            .execute(
                "INSERT OR REPLACE INTO events (
                    event_id, session_id, sequence, event_type, payload, payload_sha256,
                    occurred_at, recorded_at, previous_event_sha256, event_sha256
                 ) SELECT ?2, ?3, 1, 'replaced.digest', payload, payload_sha256,
                          occurred_at, recorded_at, NULL, event_sha256
                   FROM events WHERE event_id = ?1",
                rusqlite::params![
                    second.event_id().as_str(),
                    EventId::new().as_str(),
                    other_session.session_id.as_str(),
                ],
            )
            .is_err()
    );
    assert_eq!(stored_event_rows(&connection), baseline);
    assert_eq!(baseline.len(), 2);
    drop(connection);

    store
        .verify_event_chain()
        .expect("original chain unchanged");
    assert_eq!(
        store
            .list_events(&primary_session.session_id)
            .expect("Events")
            .len(),
        2
    );
}

#[test]
fn concurrent_event_appends_allocate_one_contiguous_chain_in_storage() {
    const APPEND_COUNT: usize = 8;

    let directory = TempDir::new().expect("temporary data directory");
    let project = active_project(directory.path());
    let workspace = workspace(&project, "root");
    let session = session(&workspace, "2026-07-31T01:03:00Z");
    let mut setup = StateStore::open(directory.path(), &project).expect("state database");
    setup.create_workspace(&workspace).expect("workspace");
    setup.create_session(&session).expect("session");
    drop(setup);

    let barrier = Arc::new(Barrier::new(APPEND_COUNT));
    let data_dir = directory.path().to_owned();
    let handles = (0..APPEND_COUNT)
        .map(|index| {
            let barrier = Arc::clone(&barrier);
            let data_dir = data_dir.clone();
            let project = project.clone();
            let session = session.clone();
            thread::spawn(move || {
                let mut store =
                    StateStore::open(&data_dir, &project).expect("concurrent state database");
                let proposed = event_input(
                    &project.project_id,
                    &session,
                    index as u64 + 1,
                    "turn.recorded",
                    &format!(r#"{{"worker":{index}}}"#),
                );
                barrier.wait();
                store.append_event(&proposed)
            })
        })
        .collect::<Vec<_>>();
    let appended = handles
        .into_iter()
        .map(|handle| handle.join().expect("append thread"))
        .collect::<Result<Vec<_>, _>>()
        .expect("all concurrent appends succeed");

    assert_eq!(appended.len(), APPEND_COUNT);
    let reopened = StateStore::open(directory.path(), &project).expect("reopen state database");
    let events = reopened
        .list_events(&session.session_id)
        .expect("event chain");
    assert_eq!(events.len(), APPEND_COUNT);
    assert_eq!(
        events.iter().map(Event::sequence).collect::<Vec<_>>(),
        (1..=APPEND_COUNT as u64).map(Some).collect::<Vec<_>>()
    );
    reopened
        .verify_event_chain()
        .expect("contiguous event chain");
}

#[test]
fn non_active_project_rejects_state_domain_operations() {
    let directory = TempDir::new().expect("temporary data directory");
    let project = project(directory.path());
    let workspace = workspace(&project, "root");
    let mut store =
        StateStore::open(directory.path(), &project).expect("initialize state database");

    assert!(matches!(
        store.create_workspace(&workspace),
        Err(StoreError::ProjectNotActive)
    ));
}

#[test]
fn provisioning_saga_completes_and_retry_returns_the_same_active_project() {
    let directory = TempDir::new().expect("temporary data directory");
    let proposed = project(directory.path());
    let idempotency = context("test-principal-a", "provision-project");
    let digest = request_digest(&proposed.repository_common_dir);
    let mut provisioner = ProjectProvisioner::open(directory.path()).expect("provisioner");

    let active = provisioner
        .provision(&proposed, &idempotency, &digest)
        .expect("successful saga");
    assert_eq!(
        active.provisioning_state_internal,
        ProjectProvisioningState::Active
    );
    assert_eq!(active.revision, 2);
    StateStore::open(directory.path(), &active).expect("final state binding");

    let replay = provisioner
        .provision(&proposed, &idempotency, &digest)
        .expect("deterministic retry");
    assert_eq!(replay, active);

    let independent = context("test-principal-b", "provision-project");
    let independent_replay = provisioner
        .provision(&proposed, &independent, &digest)
        .expect("independent subject retry");
    assert_eq!(independent_replay, active);
    assert_eq!(
        provisioner
            .idempotency_state(&independent)
            .expect("independent idempotency state"),
        "SUCCEEDED"
    );
    assert_eq!(provisioner.project_count().expect("project count"), 1);
}

#[test]
fn state_initialization_failure_leaves_no_registry_or_partial_project() {
    let directory = TempDir::new().expect("temporary data directory");
    let blocker = directory.path().join("not-a-directory");
    std::fs::write(&blocker, b"file").expect("create path blocker");
    let mut proposed = project(directory.path());
    proposed.state_db_path = blocker.join("state.sqlite").display().to_string();
    let mut provisioner = ProjectProvisioner::open(directory.path()).expect("provisioner");

    assert!(matches!(
        provisioner.provision(
            &proposed,
            &context("test-principal-a", "failed-project"),
            &request_digest(&proposed.repository_common_dir)
        ),
        Err(StoreError::ProvisioningFailed(_))
    ));
    assert_eq!(provisioner.project_count().expect("project count"), 0);
    assert!(matches!(
        provisioner.get_project(&proposed.project_id),
        Err(StoreError::NotFound("Project"))
    ));
    assert!(matches!(
        provisioner.idempotency_state_for_project(&proposed.project_id),
        Err(StoreError::NotFound("registry idempotency record"))
    ));
    assert_eq!(std::fs::read(&blocker).expect("blocker bytes"), b"file");
}

#[test]
fn approved_root_replacement_cannot_redirect_state_outside_original_capability() {
    let container = TempDir::new().expect("container");
    let approved = container.path().join("approved");
    let original = container.path().join("approved-original");
    fs::create_dir(&approved).expect("approved root");
    let outside = TempDir::new().expect("outside root");
    let proposed = project(&approved);
    let idempotency = context("root-replacement", "symlink-escape");
    let mut provisioner = ProjectProvisioner::open(&approved).expect("provisioner binds root");

    fs::rename(&approved, &original).expect("move approved root");
    symlink(outside.path(), &approved).expect("redirect approved path");

    let result = provisioner.provision(
        &proposed,
        &idempotency,
        &Sha256Digest::of_bytes(b"root replacement"),
    );
    assert!(
        matches!(result, Err(StoreError::ProvisioningFailed(_))),
        "replaced data-root path was accepted: {result:?}"
    );
    assert_failed_provision_has_no_registry_state(&provisioner, &idempotency);
    assert_no_state_database_family(outside.path(), &proposed.project_id);
    assert_no_state_database_family(&original, &proposed.project_id);
}

#[test]
fn approved_root_directory_file_and_missing_entry_replacements_fail_without_residue() {
    for replacement in ["directory", "file", "missing"] {
        let container = TempDir::new().expect("container");
        let approved = container.path().join("approved");
        let original = container.path().join("approved-original");
        fs::create_dir(&approved).expect("approved root");
        let proposed = project(&approved);
        let idempotency = context("root-replacement", replacement);
        let mut provisioner = ProjectProvisioner::open(&approved).expect("provisioner binds root");

        fs::rename(&approved, &original).expect("move approved root");
        if replacement == "directory" {
            fs::create_dir(&approved).expect("replacement directory");
        } else if replacement == "file" {
            fs::write(&approved, b"replacement file").expect("replacement file");
        }

        let result = provisioner.provision(
            &proposed,
            &idempotency,
            &Sha256Digest::of_bytes(replacement.as_bytes()),
        );
        assert!(
            matches!(result, Err(StoreError::ProvisioningFailed(_))),
            "{replacement} root replacement was accepted: {result:?}"
        );
        assert_failed_provision_has_no_registry_state(&provisioner, &idempotency);
        assert_no_state_database_family(&approved, &proposed.project_id);
        assert_no_state_database_family(&original, &proposed.project_id);
        drop(provisioner);

        let reopen = ProjectProvisioner::open(&approved);
        assert!(
            reopen.is_err(),
            "{replacement} root replacement was rebound during daemon reopen"
        );
        assert!(
            !approved.join("registry.sqlite").exists(),
            "{replacement} daemon reopen created a replacement registry"
        );
    }
}

#[test]
fn root_replacement_fails_closed_for_concurrent_provision_and_daemon_reopen() {
    const PROVISIONER_COUNT: usize = 4;

    let container = TempDir::new().expect("container");
    let approved = container.path().join("approved");
    let original = container.path().join("approved-original");
    fs::create_dir(&approved).expect("approved root");
    let outside = TempDir::new().expect("outside root");
    let provisioners = (0..PROVISIONER_COUNT)
        .map(|_| ProjectProvisioner::open(&approved).expect("shared approved root"))
        .collect::<Vec<_>>();
    let ready = Arc::new(Barrier::new(PROVISIONER_COUNT + 1));
    let go = Arc::new(Barrier::new(PROVISIONER_COUNT + 1));
    let handles = provisioners
        .into_iter()
        .enumerate()
        .map(|(index, mut provisioner)| {
            let approved = approved.clone();
            let ready = Arc::clone(&ready);
            let go = Arc::clone(&go);
            thread::spawn(move || {
                let proposed = project(&approved);
                let idempotency =
                    context("root-replacement-concurrent", &format!("worker-{index}"));
                ready.wait();
                go.wait();
                let result = provisioner.provision(
                    &proposed,
                    &idempotency,
                    &Sha256Digest::of_bytes(format!("worker-{index}").as_bytes()),
                );
                (provisioner, proposed, idempotency, result)
            })
        })
        .collect::<Vec<_>>();

    ready.wait();
    fs::rename(&approved, &original).expect("move approved root");
    symlink(outside.path(), &approved).expect("redirect approved path");
    go.wait();

    for handle in handles {
        let (provisioner, proposed, idempotency, result) = handle.join().expect("worker");
        assert!(
            matches!(result, Err(StoreError::ProvisioningFailed(_))),
            "concurrent provision accepted replacement: {result:?}"
        );
        assert_failed_provision_has_no_registry_state(&provisioner, &idempotency);
        assert_no_state_database_family(outside.path(), &proposed.project_id);
        assert_no_state_database_family(&original, &proposed.project_id);
    }

    let reopen = ProjectProvisioner::open(&approved);
    assert!(
        reopen.is_err(),
        "daemon-lifetime reopen rebound to replacement root"
    );
    assert!(
        !outside.path().join("registry.sqlite").exists(),
        "daemon reopen created a registry outside the original capability"
    );
}

#[test]
fn restart_after_registry_begin_rolls_back_the_incomplete_project_without_residue() {
    let directory = TempDir::new().expect("temporary data directory");
    let proposed = project(directory.path());
    let idempotency = context("test-principal-a", "crash-after-begin");
    let digest = request_digest(&proposed.repository_common_dir);
    let mut registry = RegistryStore::open(directory.path()).expect("registry");
    registry
        .register_project_begin(&proposed, &idempotency, &digest)
        .expect("Step A");
    drop(registry);

    let mut provisioner = ProjectProvisioner::open(directory.path()).expect("restart");
    assert!(matches!(
        provisioner.resume(&proposed.project_id),
        Err(StoreError::ProvisioningFailed(_))
    ));
    assert_eq!(provisioner.project_count().expect("project count"), 0);
    assert!(matches!(
        provisioner.get_project(&proposed.project_id),
        Err(StoreError::NotFound("Project"))
    ));
    assert!(matches!(
        provisioner.idempotency_state_for_project(&proposed.project_id),
        Err(StoreError::NotFound("registry idempotency record"))
    ));
    assert_no_state_database_family(directory.path(), &proposed.project_id);
}

#[test]
fn restart_after_state_migration_finalizes_existing_project() {
    let directory = TempDir::new().expect("temporary data directory");
    let proposed = project(directory.path());
    let idempotency = context("test-principal-a", "crash-after-state");
    let digest = request_digest(&proposed.repository_common_dir);
    let mut registry = RegistryStore::open(directory.path()).expect("registry");
    registry
        .register_project_begin(&proposed, &idempotency, &digest)
        .expect("Step A");
    StateStore::open(directory.path(), &proposed).expect("Step B");
    drop(registry);

    let mut provisioner = ProjectProvisioner::open(directory.path()).expect("restart");
    let active = provisioner
        .resume(&proposed.project_id)
        .expect("finalize Step C");
    assert_eq!(
        active.provisioning_state_internal,
        ProjectProvisioningState::Active
    );
    assert_eq!(provisioner.project_count().expect("project count"), 1);
    assert_eq!(
        provisioner
            .idempotency_state_for_project(&proposed.project_id)
            .expect("idempotency state"),
        "SUCCEEDED"
    );
}

fn assert_reopened_golden_path(
    data_dir: &Path,
    active_project: &Project,
    root_workspace: &Workspace,
    task: &Task,
    session: &Session,
    events: &[Event],
) {
    let reopened_provisioner = ProjectProvisioner::open(data_dir).expect("reopen registry");
    let reopened_project = reopened_provisioner
        .get_project(&active_project.project_id)
        .expect("persisted Project");
    assert_eq!(&reopened_project, active_project);
    let reopened_state = StateStore::open(data_dir, &reopened_project).expect("reopen state");
    assert_eq!(
        reopened_state
            .get_workspace(&root_workspace.workspace_id)
            .expect("persisted Workspace"),
        *root_workspace
    );
    assert_eq!(
        reopened_state
            .get_task(&task.task_id)
            .expect("persisted Task"),
        *task
    );
    assert_eq!(
        reopened_state
            .get_session(&session.session_id)
            .expect("persisted Session"),
        *session
    );
    assert_eq!(
        reopened_state
            .list_task_sessions(&task.task_id)
            .expect("persisted TaskSessionLink"),
        vec![session.session_id.clone()]
    );
    let persisted = reopened_state
        .list_events(&session.session_id)
        .expect("persisted Events");
    // The Task link's own task.session_linked Event comes first (P2-C).
    assert_eq!(persisted[0].event_type(), "task.session_linked");
    assert_eq!(&persisted[1..], events);
    reopened_state
        .verify_event_chain()
        .expect("reopened Event chain");
}

#[test]
fn integrated_real_git_to_reopened_state_golden_path() {
    let directory = TempDir::new().expect("temporary root");
    let repository = real_git_repository(directory.path());
    let nested = repository.join("nested");
    std::fs::create_dir(&nested).expect("nested input");
    let before = repository_snapshot(&repository);
    let data_dir = directory.path().join("memprove-data");
    let resolved = GitWorkspaceResolver::resolve(&nested).expect("resolve real Git identity");
    let registration = resolved.registration_records(&data_dir, &timestamp("2026-07-31T02:00:00Z"));
    assert_eq!(
        registration.project.repository_common_dir,
        repository
            .canonicalize()
            .expect("canonical repository")
            .join(".git")
            .display()
            .to_string()
    );

    let idempotency = context("test-principal-a", "integrated-project");
    let project_digest = request_digest(&registration.project.repository_common_dir);
    let mut provisioner = ProjectProvisioner::open(&data_dir).expect("provisioner");
    let active_project = provisioner
        .provision(&registration.project, &idempotency, &project_digest)
        .expect("active project");
    let mut state = StateStore::open(&data_dir, &active_project).expect("active state");
    state
        .create_workspace(&registration.root_workspace)
        .expect("root workspace");
    let task = task_fixture(
        &active_project,
        "Integrated Golden Path",
        "2026-07-31T02:01:00Z",
    );
    state
        .create_task(
            &task,
            &state_context("test-principal-a", "createTask", "integrated-task"),
            &CanonicalJson::parse(r#"{"title":"Integrated Golden Path"}"#)
                .expect("request")
                .sha256(),
        )
        .expect("task");
    let session = session(&registration.root_workspace, "2026-07-31T02:02:00Z");
    state.create_session(&session).expect("session");
    state
        .link_session_to_task(
            &task.task_id,
            &session.session_id,
            &timestamp("2026-07-31T02:02:01Z"),
        )
        .expect("TaskSessionLink");
    let first = state
        .append_event(&event_input(
            &active_project.project_id,
            &session,
            1,
            "session.started",
            r#"{"agent":"test","state":"active"}"#,
        ))
        .expect("first event");
    let second = state
        .append_event(&event_input(
            &active_project.project_id,
            &session,
            2,
            "turn.started",
            r#"{"turn":1}"#,
        ))
        .expect("second event");
    drop(state);
    drop(provisioner);

    assert_reopened_golden_path(
        &data_dir,
        &active_project,
        &registration.root_workspace,
        &task,
        &session,
        &[first, second],
    );
    assert_eq!(repository_snapshot(&repository), before);
}
