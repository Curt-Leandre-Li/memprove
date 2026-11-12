//! Migration 0009 (U9 A7, Owner O3; ADR 0010, "Amendment (U9)"): three
//! nullable `sessions` columns, `ended_by_session_id`,
//! `ended_by_agent_name`, and `ended_by_agent_role`, appended with `ADD
//! COLUMN` only.
//!
//! A database is built with the 0001–0008 migrations, exactly as the
//! previous build left it, holding three Sessions whose `session.started@1`
//! Events of `memproved` carry no agent, one of them ended as abandoned
//! by a `session.crashed@1` Event, which names no ender. The tests compare
//! every row before and after the upgrade, open the outdated database from
//! many connections at once, and change each new column past `memproved`
//! to see `verifyLedger` name the Session.

use std::{
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

use memprove_domain::{
    AgentId, CanonicalJson, Event, EventId, EventInput, EventProducer, LedgerRecordType,
    PrivacyClass, Project, ProjectId, ProjectProvisioningState, SessionId, Sha256Digest,
    UtcTimestamp, WorkspaceId,
};
use memprove_store_sqlite::StateStore;
use rusqlite::{Connection, types::Value};
use tempfile::TempDir;

const OLD_MIGRATIONS: [(u64, &str); 8] = [
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
    (
        7,
        include_str!("../migrations/state/0007_events_nullable_session.sql"),
    ),
    (
        8,
        include_str!("../migrations/state/0008_session_agent.sql"),
    ),
];

/// The time the third Session ends at.
const CRASHED_AT: &str = "2026-10-02T00:03:00Z";

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

/// A database at migration 0008 with one Workspace and three Sessions, each
/// with its `session.started@1` Event of `memproved`; the third ended by
/// `session.crashed@1`.
struct Outdated {
    directory: Arc<TempDir>,
    project: Project,
    sessions: Vec<SessionId>,
    started: Vec<Event>,
    crashed: Event,
}

fn insert_event(connection: &Connection, event: &Event) {
    connection
        .execute(
            "INSERT INTO events (
                event_id, session_id, sequence, event_type, payload, payload_sha256,
                occurred_at, recorded_at, previous_event_sha256, event_sha256,
                envelope_version, project_id, workspace_id, payload_schema,
                producer_name, producer_version, producer_sequence, correlation_id,
                causation_id, privacy_class, producer_occurred_at, project_sequence,
                event_digest_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, '1.0', ?10, ?11, ?12, ?13,
                       ?14, ?15, NULL, NULL, ?16, ?17, ?18, 2)",
            rusqlite::params![
                event.event_id().as_str(),
                event.session_id().map(SessionId::as_str),
                event.sequence(),
                event.event_type(),
                event.payload().as_str(),
                event.payload_sha256().as_str(),
                event.recorded_at().as_str(),
                event.previous_event_sha256().map(Sha256Digest::as_str),
                event.event_sha256().as_str(),
                event.project_id().as_str(),
                event.workspace_id().as_str(),
                event.payload_schema(),
                event.producer().name,
                event.producer().version,
                event.producer().sequence,
                event.privacy_class().as_str(),
                event.occurred_at().map(UtcTimestamp::as_str),
                event.project_sequence(),
            ],
        )
        .expect("0008 insert");
}

/// Applies 0001–0008 and records them, as the previous build did.
fn apply_old_migrations(connection: &Connection, project: &Project) {
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
}

/// One Event of `memproved` on `session_id`: `(event_type, payload)`,
/// at `at`, with `(sequence, project_sequence)`.
fn daemon_event(
    project: &Project,
    workspace_id: &WorkspaceId,
    session_id: &SessionId,
    (event_type, payload): (&str, &str),
    at: &str,
    sequences: (u64, u64),
    previous: Option<&Event>,
) -> Event {
    Event::new(
        EventInput {
            event_id: EventId::new(),
            project_id: project.project_id.clone(),
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            event_type: event_type.to_owned(),
            payload_schema: format!("{event_type}@1"),
            payload: CanonicalJson::parse(payload).expect("payload"),
            occurred_at: Some(timestamp(at)),
            recorded_at: timestamp(at),
            producer: EventProducer {
                name: "continuityd".to_owned(),
                version: "0.1.0".to_owned(),
                sequence: sequences.1,
            },
            correlation_id: None,
            causation_id: None,
            privacy_class: PrivacyClass::Internal,
        },
        Some(sequences.0),
        sequences.1,
        previous.map(|event| event.event_sha256().clone()),
    )
    .expect("event")
}

fn outdated() -> Outdated {
    let directory = Arc::new(TempDir::new().expect("data directory"));
    let project = active_project(directory.path());
    std::fs::create_dir_all(Path::new(&project.state_db_path).parent().unwrap())
        .expect("project directory");
    let connection = Connection::open(&project.state_db_path).expect("raw database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    apply_old_migrations(&connection, &project);
    // The root Workspace as `registerProject` writes it (U1 B4).
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
    let mut sessions = Vec::new();
    let mut started: Vec<Event> = Vec::new();
    for index in 0..3_u64 {
        let session_id = SessionId::new();
        let agent_id = AgentId::new();
        let at = format!("2026-10-02T00:0{index}:02Z");
        connection
            .execute(
                "INSERT INTO sessions (
                    session_id, workspace_id, agent_id, started_at, ended_at, revision
                 ) VALUES (?1, ?2, ?3, ?4, NULL, 1)",
                [
                    session_id.as_str(),
                    workspace_id.as_str(),
                    agent_id.as_str(),
                    at.as_str(),
                ],
            )
            .expect("session");
        let event = daemon_event(
            &project,
            &workspace_id,
            &session_id,
            (
                "session.started",
                &format!(r#"{{"agent_id":"{agent_id}","task_id":null}}"#),
            ),
            &at,
            (1, index + 1),
            started.last(),
        );
        insert_event(&connection, &event);
        sessions.push(session_id);
        started.push(event);
    }
    // The third Session ends as abandoned, as `endSession` wrote it before
    // U9: `session.crashed@1`, and the row's end.
    let crashed = daemon_event(
        &project,
        &workspace_id,
        &sessions[2],
        (
            "session.crashed",
            r#"{"outcome":"abandoned","reason":"taken over"}"#,
        ),
        CRASHED_AT,
        (2, 4),
        started.last(),
    );
    insert_event(&connection, &crashed);
    connection
        .execute(
            "UPDATE sessions SET ended_at = ?1, revision = 2 WHERE session_id = ?2",
            [CRASHED_AT, sessions[2].as_str()],
        )
        .expect("end the third Session");
    drop(connection);
    Outdated {
        directory,
        project,
        sessions,
        started,
        crashed,
    }
}

fn raw(project: &Project) -> Connection {
    let connection = Connection::open(&project.state_db_path).expect("raw connection");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    connection
}

/// Every column of every row of `table`, in rowid order.
fn rows(connection: &Connection, table: &str) -> Vec<Vec<Value>> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .expect("select");
    let width = statement.column_count();
    statement
        .query_map([], |row| {
            (0..width).map(|index| row.get::<_, Value>(index)).collect()
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

/// `(name, type, notnull, default)` of every `sessions` column.
fn columns(connection: &Connection) -> Vec<(String, String, bool, Option<String>)> {
    connection
        .prepare("PRAGMA table_info(sessions)")
        .expect("table_info")
        .query_map([], |row| {
            Ok((row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })
        .expect("query table_info")
        .collect::<Result<Vec<_>, _>>()
        .expect("columns")
}

fn migration_ids(connection: &Connection) -> Vec<u64> {
    connection
        .prepare("SELECT migration_id FROM schema_migrations ORDER BY migration_id")
        .expect("migrations")
        .query_map([], |row| row.get(0))
        .expect("query migrations")
        .collect::<Result<Vec<_>, _>>()
        .expect("migration ids")
}

const NEW_COLUMNS: [&str; 3] = [
    "ended_by_session_id",
    "ended_by_agent_name",
    "ended_by_agent_role",
];

#[test]
fn migration_0009_appends_three_nullable_columns_and_keeps_every_row() {
    let outdated = outdated();
    let connection = raw(&outdated.project);
    let before_sessions = rows(&connection, "sessions");
    let before_events = rows(&connection, "events");
    let before_columns = columns(&connection);
    assert_eq!(migration_ids(&connection), [1, 2, 3, 4, 5, 6, 7, 8]);
    drop(connection);

    let state = StateStore::open(outdated.directory.path(), &outdated.project).expect("0009");
    let connection = raw(&outdated.project);
    assert_eq!(migration_ids(&connection), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let after_columns = columns(&connection);
    assert_eq!(&after_columns[..before_columns.len()], before_columns);
    assert_eq!(
        after_columns[before_columns.len()..],
        NEW_COLUMNS.map(|name| (name.to_owned(), "TEXT".to_owned(), false, None))
    );
    // Every Event row is as it was; every Session row has its old values
    // and NULL in the three new columns.
    assert_eq!(rows(&connection, "events"), before_events);
    let after_sessions = rows(&connection, "sessions");
    assert_eq!(after_sessions.len(), before_sessions.len());
    for (before, after) in before_sessions.iter().zip(&after_sessions) {
        assert_eq!(&after[..before.len()], before.as_slice());
        assert_eq!(
            after[before.len()..],
            [Value::Null, Value::Null, Value::Null]
        );
    }
    // The `session.crashed@1` Event, which names no ender, verifies as
    // before.
    let ledger = state.verify_ledger().expect("ledger");
    assert!(ledger.is_valid(), "{ledger:?}");
    assert_eq!(ledger.events, 4);
    assert_eq!(ledger.project_sequence, 4);
    drop(state);

    // Reopening applies nothing again.
    let reopened = StateStore::open(outdated.directory.path(), &outdated.project).expect("reopen");
    assert_eq!(
        migration_ids(&raw(&outdated.project)),
        [1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
    assert_eq!(rows(&raw(&outdated.project), "sessions"), after_sessions);
    assert_eq!(rows(&raw(&outdated.project), "events"), before_events);
    assert!(reopened.verify_ledger().unwrap().is_valid());
}

#[test]
fn concurrent_opens_of_a_0008_database_apply_0009_once() {
    const OPENERS: usize = 8;
    let outdated = outdated();
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
    assert_eq!(migration_ids(&connection), [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(columns(&connection).len(), 12);
}

/// A change to any of the three columns past `memproved` makes
/// `verifyLedger` name the Session: on its `session.crashed` Event for the
/// Session that ended, on its `session.started` Event for one the ledger
/// records no end of.
#[test]
fn a_changed_ended_by_column_makes_verify_ledger_name_the_session() {
    let outdated = outdated();
    drop(StateStore::open(outdated.directory.path(), &outdated.project).expect("0009"));
    let cases = NEW_COLUMNS
        .iter()
        .flat_map(|column| [(column, 2_usize), (column, 0_usize)]);
    for (column, index) in cases {
        let session = &outdated.sessions[index];
        let value = match *column {
            "ended_by_session_id" => outdated.sessions[1].to_string(),
            "ended_by_agent_role" => "human".to_owned(),
            _ => "forged".to_owned(),
        };
        let connection = raw(&outdated.project);
        connection
            .execute(
                &format!("UPDATE sessions SET {column} = ?1 WHERE session_id = ?2"),
                [value.as_str(), session.as_str()],
            )
            .expect("change the column");
        drop(connection);
        let state = StateStore::open(outdated.directory.path(), &outdated.project).expect("open");
        let ledger = state.verify_ledger().expect("ledger");
        let failure = ledger.first_failure.expect("a failure");
        let event = if index == 2 {
            &outdated.crashed
        } else {
            &outdated.started[index]
        };
        assert_eq!(failure.record_type, LedgerRecordType::Event, "{column}");
        assert_eq!(failure.record_id, event.event_id().as_str(), "{column}");
        assert_eq!(failure.project_sequence, Some(event.project_sequence()));
        assert!(
            failure.reason.contains(session.as_str()) && failure.reason.contains(column),
            "{column}: {}",
            failure.reason
        );
        drop(state);
        raw(&outdated.project)
            .execute(
                &format!("UPDATE sessions SET {column} = NULL WHERE session_id = ?1"),
                [session.as_str()],
            )
            .expect("put the column back");
        let state = StateStore::open(outdated.directory.path(), &outdated.project).expect("open");
        assert!(state.verify_ledger().unwrap().is_valid(), "{column}");
    }
}
