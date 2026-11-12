//! Event envelope ledger (ADR 0010): idempotent ingest, Project-level chain,
//! paging, F01 tamper detection, the 0006 write-boundary triggers, and
//! concurrent writers.

use std::{
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

use memprove_domain::{
    AgentId, CanonicalJson, Event, EventId, EventInput, EventProducer, IdempotencyContext,
    IdempotencyKey, OperationId, PrincipalId, PrivacyClass, Project, ProjectId,
    ProjectProvisioningState, Session, SessionId, Sha256Digest, UtcTimestamp, Workspace,
    WorkspaceId,
};
use memprove_store_sqlite::{Idempotent, StateStore, StoreError};
use rusqlite::Connection;
use tempfile::TempDir;

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("valid timestamp")
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

fn workspace(project: &Project, suffix: &str) -> Workspace {
    Workspace {
        workspace_id: WorkspaceId::new(),
        project_id: project.project_id.clone(),
        canonical_worktree_path: format!("/repos/{suffix}"),
        git_dir: format!("/repos/{suffix}/.git"),
        head_commit: "0123456789012345678901234567890123456789".to_owned(),
        branch_or_detached_state: "branch:main".to_owned(),
        created_at: timestamp("2026-10-02T00:00:01Z"),
        revision: 1,
    }
}

fn session(workspace: &Workspace) -> Session {
    Session {
        session_id: SessionId::new(),
        workspace_id: workspace.workspace_id.clone(),
        agent_id: AgentId::new(),
        started_at: timestamp("2026-10-02T00:00:02Z"),
        ended_at: None,
        revision: 1,
        agent: None,
        ended_by: None,
    }
}

fn input(project: &Project, session: &Session, producer_sequence: u64) -> EventInput {
    EventInput {
        event_id: EventId::new(),
        project_id: project.project_id.clone(),
        workspace_id: session.workspace_id.clone(),
        session_id: Some(session.session_id.clone()),
        event_type: "turn.started".to_owned(),
        payload_schema: "turn.started@1".to_owned(),
        payload: CanonicalJson::parse(&format!(r#"{{"turn":{producer_sequence}}}"#))
            .expect("payload"),
        occurred_at: None,
        recorded_at: timestamp("2026-10-02T00:01:00Z"),
        producer: EventProducer {
            name: "event-ledger-test".to_owned(),
            version: "1".to_owned(),
            sequence: producer_sequence,
        },
        correlation_id: None,
        causation_id: None,
        privacy_class: PrivacyClass::Private,
    }
}

fn context(key: &str) -> IdempotencyContext {
    IdempotencyContext {
        principal_id: PrincipalId::parse("principal-a").expect("principal"),
        operation_id: OperationId::parse("ingestEvent").expect("operation"),
        key: IdempotencyKey::parse(key).expect("key"),
    }
}

fn digest(label: &str) -> Sha256Digest {
    Sha256Digest::of_bytes(label.as_bytes())
}

struct Ledger {
    directory: TempDir,
    project: Project,
    workspace: Workspace,
    sessions: [Session; 2],
}

impl Ledger {
    fn new() -> Self {
        let directory = TempDir::new().expect("data directory");
        let project = active_project(directory.path());
        let workspace = workspace(&project, "root");
        let sessions = [session(&workspace), session(&workspace)];
        let mut store = StateStore::open(directory.path(), &project).expect("state");
        store.create_workspace(&workspace).expect("workspace");
        for session in &sessions {
            store.create_session(session).expect("session");
        }
        Self {
            directory,
            project,
            workspace,
            sessions,
        }
    }

    fn open(&self) -> StateStore {
        StateStore::open(self.directory.path(), &self.project).expect("state")
    }

    fn raw(&self) -> Connection {
        Connection::open(&self.project.state_db_path).expect("raw connection")
    }
}

fn event_count(connection: &Connection) -> u64 {
    connection
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .expect("event count")
}

/// Inserts `event` through raw SQL with every 0006 column filled, bypassing
/// the store's digest computation.
fn raw_insert(
    connection: &Connection,
    event: &Event,
    payload: &str,
    event_sha256: &str,
    digest_version: Option<u8>,
) -> rusqlite::Result<usize> {
    connection.execute(
        "INSERT INTO events (
            event_id, session_id, sequence, event_type, payload, payload_sha256,
            occurred_at, recorded_at, previous_event_sha256, event_sha256,
            envelope_version, project_id, workspace_id, payload_schema,
            producer_name, producer_version, producer_sequence, correlation_id,
            causation_id, privacy_class, producer_occurred_at, project_sequence,
            event_digest_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, '1.0', ?10, ?11, ?12, ?13, ?14,
                   ?15, NULL, NULL, ?16, NULL, ?17, ?18)",
        rusqlite::params![
            event.event_id().as_str(),
            event.session_id().map(SessionId::as_str),
            event.sequence(),
            event.event_type(),
            payload,
            event.payload_sha256().as_str(),
            event.recorded_at().as_str(),
            event.previous_event_sha256().map(Sha256Digest::as_str),
            event_sha256,
            event.project_id().as_str(),
            event.workspace_id().as_str(),
            event.payload_schema(),
            event.producer().name,
            event.producer().version,
            event.producer().sequence,
            event.privacy_class().as_str(),
            event.project_sequence(),
            digest_version,
        ],
    )
}

#[test]
fn ingest_is_idempotent_and_binds_the_session_workspace() {
    let ledger = Ledger::new();
    let mut store = ledger.open();
    let [first_session, _] = &ledger.sessions;
    let proposed = input(&ledger.project, first_session, 0);

    let created = match store
        .ingest_event(&proposed, &context("key-0000000000000001"), &digest("a"))
        .expect("ingest")
    {
        Idempotent::Created(event) => event,
        Idempotent::Replayed(_) => panic!("first ingest replayed"),
    };
    assert_eq!(
        (created.sequence(), created.project_sequence()),
        (Some(1), 1)
    );
    assert_eq!(created.occurred_at(), None);

    // Same key and request: the same Event, even with a fresh receipt time.
    let mut retried = proposed.clone();
    retried.recorded_at = timestamp("2026-10-02T00:02:00Z");
    assert_eq!(
        store
            .ingest_event(&retried, &context("key-0000000000000001"), &digest("a"))
            .expect("replay"),
        Idempotent::Replayed(created.clone())
    );
    // Same key, different request.
    assert!(matches!(
        store.ingest_event(&proposed, &context("key-0000000000000001"), &digest("b")),
        Err(StoreError::IdempotencyConflict)
    ));
    // A recorded event_id under a new key.
    assert!(matches!(
        store.ingest_event(&proposed, &context("key-0000000000000002"), &digest("a")),
        Err(StoreError::IdempotencyConflict)
    ));
    // Unknown Session.
    let mut orphan = input(&ledger.project, first_session, 1);
    orphan.session_id = Some(SessionId::new());
    assert!(matches!(
        store.ingest_event(&orphan, &context("key-0000000000000003"), &digest("c")),
        Err(StoreError::NotFound("Session"))
    ));
    // Workspace other than the Session's.
    let mut elsewhere = input(&ledger.project, first_session, 1);
    elsewhere.workspace_id = WorkspaceId::new();
    assert!(matches!(
        store.ingest_event(&elsewhere, &context("key-0000000000000004"), &digest("d")),
        Err(StoreError::SessionWorkspaceMismatch)
    ));
    assert_eq!(event_count(&ledger.raw()), 1);
    assert_eq!(store.idempotency_record_count().expect("records"), 1);
    assert_eq!(store.get_event(created.event_id()).expect("read"), created);
}

#[test]
fn project_chain_spans_sessions_and_pages_by_project_sequence() {
    let ledger = Ledger::new();
    let mut store = ledger.open();
    let empty = store.list_events_page(None, 10).expect("empty page");
    assert_eq!((empty.items.len(), empty.snapshot_sequence), (0, 0));

    let mut appended = Vec::new();
    for index in 0..5_u64 {
        let session = &ledger.sessions[usize::try_from(index % 2).unwrap()];
        appended.push(
            store
                .append_event(&input(&ledger.project, session, index))
                .expect("append"),
        );
    }
    assert_eq!(
        appended
            .iter()
            .map(|event| (event.sequence(), event.project_sequence()))
            .collect::<Vec<_>>(),
        vec![
            (Some(1), 1),
            (Some(1), 2),
            (Some(2), 3),
            (Some(2), 4),
            (Some(3), 5)
        ]
    );
    for pair in appended.windows(2) {
        assert_eq!(
            pair[1].previous_event_sha256(),
            Some(pair[0].event_sha256())
        );
    }
    store.verify_event_chain().expect("valid Project chain");

    let first = store.list_events_page(None, 2).expect("first page");
    assert!(first.has_more);
    assert_eq!(first.snapshot_sequence, 5);
    let second = store
        .list_events_page(Some(first.items[1].event_id()), 10)
        .expect("second page");
    assert!(!second.has_more);
    let paged = first
        .items
        .iter()
        .chain(&second.items)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(paged, appended);
    assert!(matches!(
        store.list_events_page(Some(&EventId::new()), 10),
        Err(StoreError::NotFound("Event cursor"))
    ));
    // Task and Session pages carry the same snapshot sequence.
    assert_eq!(
        store.list_tasks_page(None, 10).unwrap().snapshot_sequence,
        5
    );
    assert_eq!(
        store
            .list_workspace_sessions_page(&ledger.workspace.workspace_id, None, 10)
            .unwrap()
            .snapshot_sequence,
        5
    );
}

#[test]
fn f01_rows_with_wrong_digests_are_rejected_on_read_and_by_chain_verification() {
    for (case, tamper_payload) in [("event digest", false), ("payload bytes", true)] {
        let ledger = Ledger::new();
        let mut store = ledger.open();
        let [first_session, _] = &ledger.sessions;
        let genesis = store
            .append_event(&input(&ledger.project, first_session, 0))
            .expect("genesis");
        let honest = Event::new(
            input(&ledger.project, first_session, 1),
            Some(2),
            2,
            Some(genesis.event_sha256().clone()),
        )
        .expect("well-formed second Event");
        let forged = digest("forged");
        let (payload, event_sha256) = if tamper_payload {
            (r#"{"turn":999}"#, honest.event_sha256().as_str())
        } else {
            (honest.payload().as_str(), forged.as_str())
        };
        // Bypassing the daemon is possible (F01 scope): the triggers check
        // shape and links, not digests.
        raw_insert(&ledger.raw(), &honest, payload, event_sha256, Some(2))
            .expect("raw insert bypassing the store");

        assert!(
            matches!(
                store.get_event(honest.event_id()),
                Err(StoreError::Database(_))
            ),
            "{case}: read accepted a forged row"
        );
        assert!(
            store.list_events_page(None, 10).is_err(),
            "{case}: page accepted a forged row"
        );
        assert!(
            matches!(
                store.verify_event_chain(),
                Err(StoreError::Database(_) | StoreError::InvalidPersistedValue(_))
            ),
            "{case}: chain verification accepted a forged row"
        );
        assert!(store.get_event(genesis.event_id()).is_ok());
    }
}

#[test]
fn triggers_reject_unversioned_incomplete_and_forked_inserts() {
    let ledger = Ledger::new();
    let mut store = ledger.open();
    let [first_session, second_session] = &ledger.sessions;
    let genesis = store
        .append_event(&input(&ledger.project, first_session, 0))
        .expect("genesis");
    let next = Event::new(
        input(&ledger.project, second_session, 1),
        Some(1),
        2,
        Some(genesis.event_sha256().clone()),
    )
    .expect("well-formed next Event");
    let raw = ledger.raw();

    // event_digest_version NULL.
    let error = raw_insert(
        &raw,
        &next,
        next.payload().as_str(),
        next.event_sha256().as_str(),
        None,
    )
    .expect_err("unversioned insert accepted");
    assert!(
        error.to_string().contains("event envelope is incomplete"),
        "{error}"
    );
    // A fork: a second Event claiming project_sequence 1.
    let fork = Event::new(input(&ledger.project, second_session, 2), Some(1), 1, None)
        .expect("well-formed fork");
    assert!(
        raw_insert(
            &raw,
            &fork,
            fork.payload().as_str(),
            fork.event_sha256().as_str(),
            Some(2)
        )
        .is_err()
    );
    // A Workspace other than the Session's.
    let mut foreign = input(&ledger.project, second_session, 3);
    foreign.workspace_id = WorkspaceId::new();
    let foreign = Event::new(foreign, Some(1), 2, Some(genesis.event_sha256().clone())).unwrap();
    assert!(
        raw_insert(
            &raw,
            &foreign,
            foreign.payload().as_str(),
            foreign.event_sha256().as_str(),
            Some(2)
        )
        .is_err()
    );
    assert_eq!(event_count(&raw), 1);
    // The honest row still goes in.
    raw_insert(
        &raw,
        &next,
        next.payload().as_str(),
        next.event_sha256().as_str(),
        Some(2),
    )
    .expect("complete, linked insert");
    store.verify_event_chain().expect("chain");
}

#[test]
fn concurrent_ingests_allocate_one_contiguous_project_chain() {
    const WRITERS: usize = 16;
    let ledger = Arc::new(Ledger::new());
    let barrier = Arc::new(Barrier::new(WRITERS));
    let handles = (0..WRITERS)
        .map(|index| {
            let ledger = Arc::clone(&ledger);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut store = ledger.open();
                let session = &ledger.sessions[index % 2];
                let proposed = input(&ledger.project, session, index as u64);
                barrier.wait();
                store.ingest_event(
                    &proposed,
                    &context(&format!("concurrent-key-{index:04}")),
                    &digest(&index.to_string()),
                )
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        assert!(matches!(
            handle.join().expect("writer"),
            Ok(Idempotent::Created(_))
        ));
    }
    let store = ledger.open();
    let page = store.list_events_page(None, 500).expect("page");
    assert_eq!(
        page.items
            .iter()
            .map(Event::project_sequence)
            .collect::<Vec<_>>(),
        (1..=WRITERS as u64).collect::<Vec<_>>()
    );
    assert_eq!(page.snapshot_sequence, WRITERS as u64);
    store.verify_event_chain().expect("contiguous chain");
}

#[test]
fn concurrent_submissions_of_one_idempotency_key_record_one_event() {
    let ledger = Arc::new(Ledger::new());
    let proposed = input(&ledger.project, &ledger.sessions[0], 7);
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let ledger = Arc::clone(&ledger);
            let barrier = Arc::clone(&barrier);
            let proposed = proposed.clone();
            thread::spawn(move || {
                let mut store = ledger.open();
                barrier.wait();
                store.ingest_event(&proposed, &context("shared-key-000000001"), &digest("same"))
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("writer").expect("ingest"))
        .collect::<Vec<_>>();
    let created = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Idempotent::Created(_)))
        .count();
    assert_eq!(created, 1, "{outcomes:?}");
    let events = outcomes
        .into_iter()
        .map(|(Idempotent::Created(event) | Idempotent::Replayed(event))| event)
        .collect::<Vec<_>>();
    assert_eq!(events[0], events[1]);
    assert_eq!(event_count(&ledger.raw()), 1);
}

#[test]
fn concurrent_opens_of_an_outdated_database_apply_each_migration_once() {
    const OPENERS: usize = 8;
    const STATE_0001: &str = include_str!("../migrations/state/0001_state.sql");
    let directory = Arc::new(TempDir::new().expect("data directory"));
    let project = active_project(directory.path());
    // A database left at migration 0001, as an older build would leave it.
    std::fs::create_dir_all(Path::new(&project.state_db_path).parent().unwrap())
        .expect("project directory");
    let raw = Connection::open(&project.state_db_path).expect("raw");
    raw.execute_batch(STATE_0001).expect("0001");
    raw.execute(
        "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
         VALUES (1, ?1, '2026-10-02T00:00:00Z')",
        [Sha256Digest::of_bytes(STATE_0001.as_bytes()).as_str()],
    )
    .expect("0001 record");
    raw.execute(
        "INSERT INTO state_metadata (
            metadata_key, project_id, repository_common_dir_sha256, schema_generation, created_at
         ) VALUES (1, ?1, ?2, 1, '2026-10-02T00:00:00Z')",
        [
            project.project_id.as_str(),
            Sha256Digest::of_bytes(project.repository_common_dir.as_bytes()).as_str(),
        ],
    )
    .expect("binding");
    drop(raw);

    let barrier = Arc::new(Barrier::new(OPENERS));
    let handles = (0..OPENERS)
        .map(|_| {
            let directory = Arc::clone(&directory);
            let project = project.clone();
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
    let migrations: Vec<u64> = Connection::open(&project.state_db_path)
        .expect("raw")
        .prepare("SELECT migration_id FROM schema_migrations ORDER BY migration_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // U8 A1: migration 0008 is the eighth; U9 A7: 0009 the ninth.
    assert_eq!(migrations, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
}
