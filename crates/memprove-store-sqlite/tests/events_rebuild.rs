//! Migration 0007 (ADR 0012 D7, P2-C): the `events` rebuild that makes
//! `session_id` and `sequence` nullable for `memproved`'s own Events.
//!
//! A database is built with the original 0001–0006 migrations, exactly as
//! the previous build left it, and filled with a chained ledger over several
//! Sessions and Event types. The tests compare every column of every row
//! before and after the upgrade, inject a failure in the middle of the
//! migration, open the outdated database from many connections at once, and
//! drive every `events` trigger with raw SQL after the rebuild.

use std::{
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

use memprove_domain::{
    AgentId, CanonicalJson, Event, EventId, EventInput, EventProducer, LedgerRecordType,
    PrivacyClass, Project, ProjectId, ProjectProvisioningState, SessionId, Sha256Digest, Task,
    TaskId, TaskStatus, UtcTimestamp, WorkspaceId,
};
use memprove_store_sqlite::{StateStore, StoreError};
use rusqlite::{Connection, types::Value};
use tempfile::TempDir;

const OLD_MIGRATIONS: [(u64, &str); 6] = [
    (1, include_str!("../migrations/state/0001_state.sql")),
    (
        2,
        include_str!("../migrations/state/0002_event_immutability.sql"),
    ),
    (
        3,
        include_str!("../migrations/state/0003_recovery_kernel.sql"),
    ),
    (
        4,
        include_str!("../migrations/state/0004_recovery_capture_consistency.sql"),
    ),
    (
        5,
        include_str!("../migrations/state/0005_task_workspace_description.sql"),
    ),
    (
        6,
        include_str!("../migrations/state/0006_event_envelope.sql"),
    ),
];
const STATE_0007: &str = include_str!("../migrations/state/0007_events_nullable_session.sql");

const EVENT_TRIGGERS: [&str; 6] = [
    "events_enforce_chain",
    "events_reject_delete",
    "events_reject_pre_envelope_ledger",
    "events_reject_replace",
    "events_reject_update",
    "events_require_envelope",
];

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("timestamp")
}

fn active_project(data_dir: &Path) -> Project {
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
        provisioning_state_internal: ProjectProvisioningState::Active,
        created_at: timestamp("2026-10-02T00:00:00Z"),
        updated_at: timestamp("2026-10-02T00:00:00Z"),
        revision: 1,
    }
}

/// One row as `INSERT INTO events` writes it, so a test can change one
/// column before writing it with raw SQL.
#[derive(Clone)]
struct Row {
    event_id: String,
    session_id: Option<String>,
    sequence: Option<u64>,
    event_type: String,
    payload: String,
    payload_sha256: String,
    recorded_at: String,
    previous_event_sha256: Option<String>,
    event_sha256: String,
    project_id: String,
    workspace_id: String,
    payload_schema: Option<String>,
    producer_name: String,
    producer_version: String,
    producer_sequence: u64,
    privacy_class: String,
    producer_occurred_at: Option<String>,
    project_sequence: u64,
    digest_version: Option<u8>,
}

impl Row {
    fn of(event: &Event) -> Self {
        Self {
            event_id: event.event_id().to_string(),
            session_id: event.session_id().map(ToString::to_string),
            sequence: event.sequence(),
            event_type: event.event_type().to_owned(),
            payload: event.payload().as_str().to_owned(),
            payload_sha256: event.payload_sha256().to_string(),
            recorded_at: event.recorded_at().to_string(),
            previous_event_sha256: event.previous_event_sha256().map(ToString::to_string),
            event_sha256: event.event_sha256().to_string(),
            project_id: event.project_id().to_string(),
            workspace_id: event.workspace_id().to_string(),
            payload_schema: Some(event.payload_schema().to_owned()),
            producer_name: event.producer().name.clone(),
            producer_version: event.producer().version.clone(),
            producer_sequence: event.producer().sequence,
            privacy_class: event.privacy_class().as_str().to_owned(),
            producer_occurred_at: event.occurred_at().map(ToString::to_string),
            project_sequence: event.project_sequence(),
            digest_version: Some(2),
        }
    }

    fn insert(&self, connection: &Connection) -> rusqlite::Result<usize> {
        connection.execute(
            "INSERT INTO events (
                event_id, session_id, sequence, event_type, payload, payload_sha256,
                occurred_at, recorded_at, previous_event_sha256, event_sha256,
                envelope_version, project_id, workspace_id, payload_schema,
                producer_name, producer_version, producer_sequence, correlation_id,
                causation_id, privacy_class, producer_occurred_at, project_sequence,
                event_digest_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, '1.0', ?10, ?11, ?12, ?13,
                       ?14, ?15, NULL, NULL, ?16, ?17, ?18, ?19)",
            rusqlite::params![
                self.event_id,
                self.session_id,
                self.sequence,
                self.event_type,
                self.payload,
                self.payload_sha256,
                self.recorded_at,
                self.previous_event_sha256,
                self.event_sha256,
                self.project_id,
                self.workspace_id,
                self.payload_schema,
                self.producer_name,
                self.producer_version,
                self.producer_sequence,
                self.privacy_class,
                self.producer_occurred_at,
                self.project_sequence,
                self.digest_version,
            ],
        )
    }
}

/// A database at migration 0006 holding one Workspace, three Sessions (one
/// ended), and a chained ledger of eight Events of six types.
struct Outdated {
    directory: Arc<TempDir>,
    project: Project,
    workspace_id: WorkspaceId,
    sessions: Vec<SessionId>,
    events: Vec<Event>,
}

/// The input of one client Event of `session` at `minute`.
fn client_input(
    project: &Project,
    workspace_id: &WorkspaceId,
    session: Option<&SessionId>,
    event_type: &str,
    payload: &str,
    minute: u32,
) -> EventInput {
    EventInput {
        event_id: EventId::new(),
        project_id: project.project_id.clone(),
        workspace_id: workspace_id.clone(),
        session_id: session.cloned(),
        event_type: event_type.to_owned(),
        payload_schema: format!("{event_type}@1"),
        payload: CanonicalJson::parse(payload).expect("payload"),
        occurred_at: minute
            .is_multiple_of(2)
            .then(|| timestamp(&format!("2026-10-02T01:{minute:02}:00Z"))),
        recorded_at: timestamp(&format!("2026-10-02T01:{minute:02}:30Z")),
        producer: EventProducer {
            name: "rebuild-test".to_owned(),
            version: "1".to_owned(),
            sequence: u64::from(minute),
        },
        correlation_id: None,
        causation_id: None,
        privacy_class: if minute.is_multiple_of(3) {
            PrivacyClass::Private
        } else {
            PrivacyClass::Internal
        },
    }
}

#[allow(clippy::too_many_lines)] // The 0001–0006 database, row by row.
fn outdated() -> Outdated {
    let directory = Arc::new(TempDir::new().expect("data directory"));
    let project = active_project(directory.path());
    std::fs::create_dir_all(Path::new(&project.state_db_path).parent().unwrap())
        .expect("project directory");
    let connection = Connection::open(&project.state_db_path).expect("raw database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    for (id, sql) in OLD_MIGRATIONS {
        connection.execute_batch(sql).expect("old migration");
        if id == 1 {
            connection
                .execute(
                    "INSERT INTO state_metadata (
                        metadata_key, project_id, repository_common_dir_sha256,
                        schema_generation, created_at
                     ) VALUES (1, ?1, ?2, 1, '2026-10-02T00:00:00Z')",
                    [
                        project.project_id.as_str(),
                        Sha256Digest::of_bytes(project.repository_common_dir.as_bytes()).as_str(),
                    ],
                )
                .expect("binding");
        }
        connection
            .execute(
                "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
                 VALUES (?1, ?2, '2026-10-02T00:00:00Z')",
                rusqlite::params![id, Sha256Digest::of_bytes(sql.as_bytes()).as_str()],
            )
            .expect("migration record");
    }
    // The root Workspace as `registerProject` writes it: no Event, its Git
    // directory the Project's common directory (U1 B4).
    let workspace_id = WorkspaceId::new();
    connection
        .execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, canonical_worktree_path, git_dir, head_commit,
                branch_or_detached_state, created_at, revision
             ) VALUES (?1, ?2, '/repos/a', ?4, ?3, 'branch:main',
                       '2026-10-02T00:00:01Z', 1)",
            [
                workspace_id.as_str(),
                project.project_id.as_str(),
                "0123456789012345678901234567890123456789",
                project.repository_common_dir.as_str(),
            ],
        )
        .expect("workspace");
    let sessions = (0..3).map(|_| SessionId::new()).collect::<Vec<_>>();
    for (index, session) in sessions.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO sessions (
                    session_id, workspace_id, agent_id, started_at, ended_at, revision
                 ) VALUES (?1, ?2, ?3, '2026-10-02T00:00:02Z', ?4, ?5)",
                rusqlite::params![
                    session.as_str(),
                    workspace_id.as_str(),
                    AgentId::new().as_str(),
                    (index == 2).then_some("2026-10-02T02:00:00Z"),
                    if index == 2 { 2 } else { 1 },
                ],
            )
            .expect("session");
    }
    // Eight Events interleaving the three Sessions.
    let plan: [(usize, &str, &str); 8] = [
        (0, "session.started", r#"{"agent":"a"}"#),
        (1, "session.started", r#"{"agent":"b"}"#),
        (0, "turn.started", r#"{"turn":1}"#),
        (2, "session.started", r#"{"agent":"c"}"#),
        (1, "tool.completed", r#"{"tool":"grep","ok":true}"#),
        (
            0,
            "command.completed",
            r#"{"argv":["cargo","test"],"exit":0}"#,
        ),
        (2, "file.written", r#"{"path":"README"}"#),
        (2, "session.ended", r#"{"reason":"done"}"#),
    ];
    let mut events: Vec<Event> = Vec::new();
    let mut heads = [0_u64; 3];
    for (minute, (session, event_type, payload)) in plan.into_iter().enumerate() {
        heads[session] += 1;
        let event = Event::new(
            client_input(
                &project,
                &workspace_id,
                Some(&sessions[session]),
                event_type,
                payload,
                u32::try_from(minute).unwrap(),
            ),
            Some(heads[session]),
            u64::try_from(events.len()).unwrap() + 1,
            events.last().map(|event| event.event_sha256().clone()),
        )
        .expect("event");
        Row::of(&event).insert(&connection).expect("0006 insert");
        events.push(event);
    }
    drop(connection);
    Outdated {
        directory,
        project,
        workspace_id,
        sessions,
        events,
    }
}

/// Every column of every `events` row in Project order.
fn rows(connection: &Connection) -> Vec<Vec<Value>> {
    let mut statement = connection
        .prepare("SELECT * FROM events ORDER BY project_sequence")
        .expect("select events");
    let width = statement.column_count();
    statement
        .query_map([], |row| {
            (0..width).map(|index| row.get::<_, Value>(index)).collect()
        })
        .expect("query events")
        .collect::<Result<Vec<_>, _>>()
        .expect("events")
}

/// `(name, type, notnull, default, pk)` of every `events` column.
fn columns(connection: &Connection) -> Vec<(String, String, bool, Option<String>, u8)> {
    connection
        .prepare("PRAGMA table_info(events)")
        .expect("table_info")
        .query_map([], |row| {
            Ok((
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("query table_info")
        .collect::<Result<Vec<_>, _>>()
        .expect("columns")
}

fn names(connection: &Connection, kind: &str) -> Vec<String> {
    connection
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = ?1 AND tbl_name = 'events' AND name NOT LIKE 'sqlite_autoindex%'
             ORDER BY name",
        )
        .expect("schema names")
        .query_map([kind], |row| row.get(0))
        .expect("query schema names")
        .collect::<Result<Vec<_>, _>>()
        .expect("names")
}

fn migration_ids(connection: &Connection) -> Vec<(u64, String)> {
    connection
        .prepare(
            "SELECT migration_id, migration_checksum FROM schema_migrations ORDER BY migration_id",
        )
        .expect("migrations")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query migrations")
        .collect::<Result<Vec<_>, _>>()
        .expect("migration ids")
}

fn raw(project: &Project) -> Connection {
    let connection = Connection::open(&project.state_db_path).expect("raw connection");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    connection
}

#[test]
#[allow(clippy::too_many_lines)] // Every column and digest before and after.
fn migration_0007_preserves_every_event_row_value_and_digest() {
    let outdated = outdated();
    let connection = raw(&outdated.project);
    let before_rows = rows(&connection);
    let before_columns = columns(&connection);
    assert_eq!(before_rows.len(), 8);
    assert_eq!(migration_ids(&connection).len(), 6);
    assert_eq!(names(&connection, "trigger"), EVENT_TRIGGERS);
    drop(connection);

    let mut state =
        StateStore::open(outdated.directory.path(), &outdated.project).expect("upgrade to 0007");
    let connection = raw(&outdated.project);
    // Every value of every row, digests included, is unchanged.
    assert_eq!(rows(&connection), before_rows);
    // The same columns in the same order; only the two NOT NULLs are gone.
    let after_columns = columns(&connection);
    assert_eq!(after_columns.len(), before_columns.len());
    for (before, after) in before_columns.iter().zip(&after_columns) {
        let relaxed = matches!(before.0.as_str(), "session_id" | "sequence");
        assert_eq!(
            (&before.0, &before.1, &before.3, before.4),
            (&after.0, &after.1, &after.3, after.4)
        );
        assert_eq!(after.2, before.2 && !relaxed, "{}", before.0);
        if relaxed {
            assert!(before.2, "{} was NOT NULL before 0007", before.0);
        }
    }
    let migrations = migration_ids(&connection);
    // U8 A1: the open applies 0008 after 0007; U9 A7: and 0009.
    assert_eq!(migrations.len(), 9);
    assert_eq!(
        migrations[6],
        (7, Sha256Digest::of_bytes(STATE_0007.as_bytes()).to_string())
    );
    assert_eq!(names(&connection, "trigger"), EVENT_TRIGGERS);
    assert_eq!(
        names(&connection, "index"),
        ["events_order_idx", "events_project_sequence_idx"]
    );
    assert!(
        !connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'events_0007')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    );
    let foreign_keys = connection
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .count();
    assert_eq!(foreign_keys, 0);
    drop(connection);

    // The store reads back the same Events, recomputing both digests, and
    // the chain and the whole ledger verify.
    let page = state.list_events_page(None, 100).expect("events");
    assert_eq!(page.items, outdated.events);
    for event in &outdated.events {
        assert_eq!(state.get_event(event.event_id()).unwrap(), *event);
    }
    state.verify_event_chain().expect("chain after 0007");
    let ledger = state.verify_ledger().expect("ledger");
    assert!(ledger.is_valid(), "{ledger:?}");
    assert_eq!(ledger.events, 8);
    assert_eq!(
        ledger.head_event_sha256.as_ref(),
        Some(outdated.events[7].event_sha256())
    );

    // The rebuilt table takes a daemon Event without a Session and the next
    // client Event of a Session after it.
    state
        .create_task(
            &Task {
                task_id: TaskId::new(),
                project_id: outdated.project.project_id.clone(),
                workspace_id: Some(outdated.workspace_id.clone()),
                title: "after the rebuild".to_owned(),
                description: None,
                status: TaskStatus::Proposed,
                created_at: timestamp("2026-10-02T03:00:00Z"),
                updated_at: timestamp("2026-10-02T03:00:00Z"),
                revision: 1,
                completion_evidence_ids: Vec::new(),
            },
            &memprove_domain::IdempotencyContext {
                principal_id: memprove_domain::PrincipalId::parse("principal").unwrap(),
                operation_id: memprove_domain::OperationId::parse("createTask").unwrap(),
                key: memprove_domain::IdempotencyKey::parse("rebuild-task-0001").unwrap(),
            },
            &Sha256Digest::of_bytes(b"task"),
        )
        .expect("task.created after 0007");
    let next = state
        .append_event(&client_input(
            &outdated.project,
            &outdated.workspace_id,
            Some(&outdated.sessions[0]),
            "turn.ended",
            r#"{"turn":1}"#,
            50,
        ))
        .expect("client Event after 0007");
    assert_eq!((next.sequence(), next.project_sequence()), (Some(4), 10));
    let tail = state.list_events_page(None, 100).unwrap().items;
    assert_eq!(tail[8].event_type(), "task.created");
    assert_eq!((tail[8].session_id(), tail[8].sequence()), (None, None));
    assert_eq!(&tail[..8], outdated.events.as_slice());
    state
        .verify_event_chain()
        .expect("chain with a daemon Event");
    assert!(state.verify_ledger().unwrap().is_valid());

    // Reopening applies nothing again.
    drop(state);
    let reopened = StateStore::open(outdated.directory.path(), &outdated.project).expect("reopen");
    assert_eq!(migration_ids(&raw(&outdated.project)), migrations);
    reopened.verify_event_chain().expect("chain after reopen");
}

#[test]
fn migration_0007_failure_rolls_back_completely_and_reruns_once() {
    let outdated = outdated();
    let connection = Connection::open(&outdated.project.state_db_path).expect("raw");
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("foreign keys off");
    // A dangling Session reference, written with foreign keys off, makes the
    // copy into the rebuilt table fail halfway through the migration.
    let removed: (String, String, String, String, Option<String>, i64) = connection
        .query_row(
            "SELECT session_id, workspace_id, agent_id, started_at, ended_at, revision
             FROM sessions WHERE session_id = ?1",
            [outdated.sessions[1].as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            [outdated.sessions[1].as_str()],
        )
        .expect("dangle a Session reference");
    let before_rows = rows(&connection);
    let before_columns = columns(&connection);
    let before_sql = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'events'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    drop(connection);

    let failed = StateStore::open(outdated.directory.path(), &outdated.project);
    assert!(
        matches!(
            &failed,
            Err(StoreError::Database(_) | StoreError::ConstraintViolation(_))
        ),
        "{:?}",
        failed.err()
    );
    let connection = raw(&outdated.project);
    // Nothing of 0007 remains: no record, no new table, the old schema, the
    // old triggers and indexes, and every row as it was.
    assert_eq!(migration_ids(&connection).len(), 6);
    assert_eq!(rows(&connection), before_rows);
    assert_eq!(columns(&connection), before_columns);
    assert_eq!(
        connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'events'",
                [],
                |row| { row.get::<_, String>(0) }
            )
            .unwrap(),
        before_sql
    );
    assert_eq!(names(&connection, "trigger"), EVENT_TRIGGERS);
    assert_eq!(
        names(&connection, "index"),
        ["events_order_idx", "events_project_sequence_idx"]
    );
    assert!(
        !connection
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'events_0007')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    );

    // Repair the reference: the next open applies 0007 once.
    connection
        .execute(
            "INSERT INTO sessions (
                session_id, workspace_id, agent_id, started_at, ended_at, revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                removed.0, removed.1, removed.2, removed.3, removed.4, removed.5
            ],
        )
        .unwrap();
    drop(connection);
    let state = StateStore::open(outdated.directory.path(), &outdated.project).expect("rerun");
    let connection = raw(&outdated.project);
    // U8 A1: and 0008 after it; U9 A7: and 0009.
    assert_eq!(migration_ids(&connection).len(), 9);
    assert_eq!(rows(&connection), before_rows);
    state.verify_event_chain().expect("chain after rerun");
}

#[test]
fn concurrent_opens_of_a_0006_database_apply_0007_once() {
    const OPENERS: usize = 8;
    let outdated = outdated();
    let before_rows = rows(&raw(&outdated.project));
    let barrier = Arc::new(Barrier::new(OPENERS));
    let handles = (0..OPENERS)
        .map(|_| {
            let directory = Arc::clone(&outdated.directory);
            let project = outdated.project.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                StateStore::open(directory.path(), &project).map(drop)
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().expect("opener").expect("concurrent open");
    }
    let connection = raw(&outdated.project);
    let migrations = migration_ids(&connection);
    assert_eq!(
        migrations.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        // U8 A1: 0008 is applied once too; U9 A7: and 0009.
        [1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
    assert_eq!(rows(&connection), before_rows);
    assert_eq!(names(&connection, "trigger"), EVENT_TRIGGERS);
    StateStore::open(outdated.directory.path(), &outdated.project)
        .expect("open")
        .verify_event_chain()
        .expect("chain");
}

/// A rebuilt ledger holding the eight client Events, plus the next valid
/// daemon Event without a Session (not written).
fn rebuilt() -> (Outdated, StateStore, Event) {
    let outdated = outdated();
    let state = StateStore::open(outdated.directory.path(), &outdated.project).expect("rebuild");
    let head = outdated.events.last().unwrap();
    let daemon = Event::new(
        EventInput {
            producer: EventProducer {
                name: "continuityd".to_owned(),
                version: "0.1.0".to_owned(),
                sequence: 9,
            },
            privacy_class: PrivacyClass::Internal,
            ..client_input(
                &outdated.project,
                &outdated.workspace_id,
                None,
                "task.created",
                r#"{"task_id":"x"}"#,
                40,
            )
        },
        None,
        9,
        Some(head.event_sha256().clone()),
    )
    .expect("daemon Event");
    (outdated, state, daemon)
}

fn refused(connection: &Connection, row: &Row, message: &str) {
    let error = row.insert(connection).expect_err(message).to_string();
    assert!(error.contains(message), "{error} does not name {message:?}");
}

#[test]
#[allow(clippy::too_many_lines)] // Every rule of every trigger, by raw SQL.
fn every_events_constraint_still_holds_after_the_rebuild() {
    let (outdated, state, daemon) = rebuilt();
    let connection = raw(&outdated.project);
    let append_only = "events are append-only";
    let chain = "event sequence or hash chain is invalid";
    let envelope = "event envelope is incomplete or unbound";

    // Append-only: no update, no delete, no replacement.
    assert!(
        connection
            .execute(
                "UPDATE events SET payload = '{}' WHERE project_sequence = 1",
                []
            )
            .unwrap_err()
            .to_string()
            .contains(append_only)
    );
    assert!(
        connection
            .execute("DELETE FROM events WHERE project_sequence = 8", [])
            .unwrap_err()
            .to_string()
            .contains(append_only)
    );
    // Each copy is otherwise the valid next row, so only replacement fails.
    let first = Row::of(&outdated.events[0]);
    let mut same_id = Row::of(&daemon);
    same_id.event_id = first.event_id.clone();
    refused(&connection, &same_id, append_only);
    let mut same_digest = Row::of(&daemon);
    same_digest.event_sha256 = first.event_sha256.clone();
    refused(&connection, &same_digest, append_only);
    // A taken Session position also breaks that Session's sequence.
    let mut same_position = Row::of(&daemon);
    same_position.session_id = Some(outdated.sessions[0].to_string());
    same_position.sequence = Some(1);
    same_position.producer_name = "rebuild-test".to_owned();
    let error = same_position.insert(&connection).unwrap_err().to_string();
    assert!(
        error.contains(append_only) || error.contains(chain),
        "{error}"
    );
    let error = first.insert(&connection).unwrap_err().to_string();
    assert!(
        error.contains(append_only) || error.contains(chain),
        "{error}"
    );

    // The chain: the next Project sequence, linked to the head.
    let mut gap = Row::of(&daemon);
    gap.project_sequence = 10;
    gap.producer_sequence = 10;
    refused(&connection, &gap, chain);
    let mut unlinked = Row::of(&daemon);
    unlinked.previous_event_sha256 = Some(Sha256Digest::of_bytes(b"elsewhere").to_string());
    refused(&connection, &unlinked, chain);
    let mut unrooted = Row::of(&daemon);
    unrooted.previous_event_sha256 = None;
    refused(&connection, &unrooted, chain);
    // The Session sequence stays contiguous for rows with a Session.
    let mut skipped = Row::of(&daemon);
    skipped.session_id = Some(outdated.sessions[0].to_string());
    skipped.sequence = Some(5);
    skipped.producer_name = "rebuild-test".to_owned();
    refused(&connection, &skipped, chain);

    // The envelope: complete, digest version 2, bound to its Project and
    // Session Workspace.
    for (column, row) in [
        ("payload_schema", {
            let mut row = Row::of(&daemon);
            row.payload_schema = None;
            row
        }),
        ("event_digest_version", {
            let mut row = Row::of(&daemon);
            row.digest_version = None;
            row
        }),
        ("project_id", {
            let mut row = Row::of(&daemon);
            row.project_id = ProjectId::new().to_string();
            row
        }),
    ] {
        let error = row.insert(&connection).expect_err(column).to_string();
        assert!(error.contains(envelope), "{column}: {error}");
    }
    let other_workspace = WorkspaceId::new();
    connection
        .execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, canonical_worktree_path, git_dir, head_commit,
                branch_or_detached_state, created_at, revision
             ) VALUES (?1, ?2, '/repos/b', '/repos/b/.git', ?3, 'branch:b',
                       '2026-10-02T00:00:01Z', 1)",
            [
                other_workspace.as_str(),
                outdated.project.project_id.as_str(),
                "0123456789012345678901234567890123456789",
            ],
        )
        .unwrap();
    let mut elsewhere = Row::of(&daemon);
    elsewhere.session_id = Some(outdated.sessions[0].to_string());
    elsewhere.sequence = Some(4);
    elsewhere.producer_name = "rebuild-test".to_owned();
    elsewhere.workspace_id = other_workspace.to_string();
    refused(&connection, &elsewhere, envelope);

    // Without a Session: no Session sequence, and only memproved's own
    // Event (producer, internal privacy, producer sequence = Project
    // sequence).
    let mut half = Row::of(&daemon);
    half.sequence = Some(1);
    refused(&connection, &half, envelope);
    // A Session without its sequence also breaks that Session's sequence.
    let mut other_half = Row::of(&daemon);
    other_half.session_id = Some(outdated.sessions[0].to_string());
    let error = other_half.insert(&connection).unwrap_err().to_string();
    assert!(error.contains(envelope) || error.contains(chain), "{error}");
    let mut client = Row::of(&daemon);
    client.producer_name = "cli".to_owned();
    refused(&connection, &client, envelope);
    let mut private = Row::of(&daemon);
    private.privacy_class = "private".to_owned();
    refused(&connection, &private, envelope);
    let mut counted = Row::of(&daemon);
    counted.producer_sequence = 3;
    refused(&connection, &counted, envelope);

    // A pre-envelope row still blocks every later insert.
    // (Exercised by golden_path_store; here the valid daemon row goes in.)
    Row::of(&daemon)
        .insert(&connection)
        .expect("memproved's Event without a Session");
    drop(connection);
    state
        .verify_event_chain()
        .expect("chain with the raw daemon row");
    assert_eq!(state.get_event(daemon.event_id()).unwrap(), daemon);
    let ledger = state.verify_ledger().unwrap();
    assert_eq!(ledger.events, 9);
    // T21 (V1-S-2): the chain holds, and the raw `task.created` names a
    // Task no row holds, which the ledger check reports as missing.
    let failure = ledger.first_failure.expect("the Task the raw Event names");
    assert_eq!(
        (
            failure.record_type,
            failure.record_id.as_str(),
            failure.project_sequence
        ),
        (LedgerRecordType::Task, "x", Some(9))
    );
    assert!(
        failure.reason.starts_with("record is missing"),
        "{failure:?}"
    );
}

#[test]
fn daemon_events_may_omit_the_session_and_client_events_may_not() {
    let (outdated, mut state, _) = rebuilt();
    // A client Event without a Session is refused before any row is written.
    let without = client_input(
        &outdated.project,
        &outdated.workspace_id,
        None,
        "turn.started",
        "{}",
        41,
    );
    assert!(matches!(
        state.ingest_event(
            &without,
            &memprove_domain::IdempotencyContext {
                principal_id: memprove_domain::PrincipalId::parse("principal").unwrap(),
                operation_id: memprove_domain::OperationId::parse("ingestEvent").unwrap(),
                key: memprove_domain::IdempotencyKey::parse("no-session-key-01").unwrap(),
            },
            &Sha256Digest::of_bytes(b"no session"),
        ),
        Err(StoreError::ConstraintViolation(_))
    ));
    // Through the single write path it reaches the triggers, which refuse a
    // client producer without a Session.
    assert!(state.append_event(&without).is_err());
    assert_eq!(state.list_events_page(None, 100).unwrap().items.len(), 8);
    state.verify_event_chain().expect("nothing was written");
}
