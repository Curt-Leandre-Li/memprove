use std::{fs, path::Path, process::Command};

use memprove_domain::{
    AgentId, CanonicalJson, Checkpoint, CheckpointClassification, CheckpointId, CheckpointInput,
    CheckpointStatement, EventId, EventInput, EventProducer, Evidence, EvidenceCollector,
    EvidenceId, EvidenceInput, EvidenceKind, EvidencePayload, EvidenceSubject, IdempotencyContext,
    IdempotencyKey, OperationId, PrincipalId, PrivacyClass, Project, ProjectId,
    ProjectProvisioningState, RepositorySnapshot, RepositorySnapshotId, Session, SessionId,
    Sha256Digest, TaskId, UtcTimestamp, WorkspaceId,
};
use memprove_repo::{
    RepositoryCaptureRequest, RepositoryProbe, RepositorySnapshotCollector, SelectedFileCapture,
};
use memprove_store_sqlite::{Idempotent, StateStore, StoreError};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const STATE_0001: &str = include_str!("../migrations/state/0001_state.sql");
const STATE_0002: &str = include_str!("../migrations/state/0002_event_immutability.sql");
const STATE_0003: &str = include_str!("../migrations/state/0003_recovery_kernel.sql");
const STATE_0004: &str = include_str!("../migrations/state/0004_recovery_capture_consistency.sql");
const STATE_0005: &str = include_str!("../migrations/state/0005_task_workspace_description.sql");
const STATE_0006: &str = include_str!("../migrations/state/0006_event_envelope.sql");
const STATE_0007: &str = include_str!("../migrations/state/0007_events_nullable_session.sql");
const ORIGINAL_STATE_0003_SHA256: &str =
    "db17b70e0c302f439a8639e4d1bd18896af5dc56abe92c28fa32729c33528175";

struct UpgradeFixture {
    root: TempDir,
    project: Project,
    workspace_id: WorkspaceId,
    task_id: TaskId,
    session_id: SessionId,
    /// The Session's baseline Event. Version 2 Events need the 0006 columns,
    /// so it is appended on first open (see [`open_state`]) instead of being
    /// written into the raw 0002 database (ADR 0010 D7).
    event: EventInput,
    migration_rows: Vec<(u64, String, String)>,
}

/// A version 2 Event of `session_id` in the fixture Workspace.
fn fixture_event(
    project_id: &ProjectId,
    workspace_id: &WorkspaceId,
    session_id: &SessionId,
    event_type: &str,
    payload: &str,
    at: &str,
) -> EventInput {
    EventInput {
        event_id: EventId::new(),
        project_id: project_id.clone(),
        workspace_id: workspace_id.clone(),
        session_id: Some(session_id.clone()),
        event_type: event_type.to_owned(),
        payload_schema: format!("{event_type}@1"),
        payload: CanonicalJson::parse(payload).expect("payload"),
        occurred_at: Some(timestamp(at)),
        recorded_at: timestamp(at),
        producer: EventProducer {
            name: "recovery-kernel-test".to_owned(),
            version: "1".to_owned(),
            sequence: 0,
        },
        correlation_id: None,
        causation_id: None,
        privacy_class: PrivacyClass::Internal,
    }
}

/// Opens (and migrates) the fixture database under `data_dir`, appending the
/// baseline Event on first open.
fn open_state(data_dir: &Path, fixture: &UpgradeFixture) -> Result<StateStore, StoreError> {
    let mut state = StateStore::open(data_dir, &fixture.project)?;
    match state.get_event(&fixture.event.event_id) {
        Ok(_) => {}
        Err(StoreError::NotFound(_)) => {
            state.append_event(&fixture.event)?;
        }
        Err(error) => return Err(error),
    }
    Ok(state)
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::parse(value).expect("valid timestamp")
}

#[allow(clippy::too_many_lines)]
fn upgrade_fixture() -> UpgradeFixture {
    let root = TempDir::new().expect("approved data root");
    let repository = root.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    git(&repository, &["init", "--initial-branch=main"]);
    git(&repository, &["config", "user.name", "memprove Test"]);
    git(
        &repository,
        &["config", "user.email", "memprove@example.invalid"],
    );
    fs::write(repository.join("tracked.txt"), b"tracked fixture\n").expect("tracked file");
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "-m", "fixture"]);
    let repository_common_dir = git(
        &repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let git_dir = git(
        &repository,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    );
    let head = git(&repository, &["rev-parse", "HEAD"]);
    let canonical_repository = repository.canonicalize().expect("canonical repository");
    let project_id = ProjectId::new();
    let workspace_id = WorkspaceId::new();
    let task_id = TaskId::new();
    let session_id = SessionId::new();
    let database_path = root
        .path()
        .join("projects")
        .join(project_id.as_str())
        .join("state.sqlite");
    fs::create_dir_all(database_path.parent().expect("database parent"))
        .expect("project directory");
    let project = Project {
        project_id: project_id.clone(),
        repository_common_dir: repository_common_dir.clone(),
        object_format: "sha1".to_owned(),
        state_db_path: database_path.display().to_string(),
        provisioning_state_internal: ProjectProvisioningState::Active,
        created_at: timestamp("2026-08-16T01:00:00Z"),
        updated_at: timestamp("2026-08-16T01:00:00Z"),
        revision: 1,
    };
    let connection = Connection::open(&database_path).expect("create 0002 database");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    connection
        .execute_batch(STATE_0001)
        .expect("migration 0001");
    let repository_digest = Sha256Digest::of_bytes(project.repository_common_dir.as_bytes());
    connection
        .execute(
            "INSERT INTO state_metadata (
                metadata_key, project_id, repository_common_dir_sha256,
                schema_generation, created_at
             ) VALUES (1, ?1, ?2, 1, ?3)",
            params![
                project_id.as_str(),
                repository_digest.as_str(),
                project.created_at.as_str()
            ],
        )
        .expect("state binding");
    connection
        .execute(
            "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
             VALUES (1, ?1, ?2)",
            params![
                Sha256Digest::of_bytes(STATE_0001.as_bytes()).as_str(),
                "2026-08-16T01:00:01Z"
            ],
        )
        .expect("migration 0001 row");
    connection
        .execute_batch(STATE_0002)
        .expect("migration 0002");
    connection
        .execute(
            "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
             VALUES (2, ?1, ?2)",
            params![
                Sha256Digest::of_bytes(STATE_0002.as_bytes()).as_str(),
                "2026-08-16T01:00:02Z"
            ],
        )
        .expect("migration 0002 row");
    connection
        .execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, canonical_worktree_path, git_dir,
                head_commit, branch_or_detached_state, created_at, revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'branch:main', ?6, 1)",
            params![
                workspace_id.as_str(),
                project_id.as_str(),
                canonical_repository.display().to_string(),
                git_dir,
                head,
                "2026-08-16T01:01:00Z"
            ],
        )
        .expect("workspace");
    connection
        .execute(
            "INSERT INTO tasks (
                task_id, project_id, title, status, created_at, updated_at, revision
             ) VALUES (?1, ?2, 'Recovery task', 'in_progress', ?3, ?3, 1)",
            params![
                task_id.as_str(),
                project_id.as_str(),
                "2026-08-16T01:01:01Z"
            ],
        )
        .expect("task");
    connection
        .execute(
            "INSERT INTO sessions (
                session_id, workspace_id, agent_id, started_at, ended_at, revision
             ) VALUES (?1, ?2, ?3, ?4, NULL, 1)",
            params![
                session_id.as_str(),
                workspace_id.as_str(),
                AgentId::new().as_str(),
                "2026-08-16T01:01:02Z"
            ],
        )
        .expect("session");
    connection
        .execute(
            "INSERT INTO task_sessions (task_id, session_id, linked_at, revision)
             VALUES (?1, ?2, ?3, 1)",
            params![
                task_id.as_str(),
                session_id.as_str(),
                "2026-08-16T01:01:03Z"
            ],
        )
        .expect("task session");
    let event = fixture_event(
        &project_id,
        &workspace_id,
        &session_id,
        "session.started",
        r#"{"fixture":true}"#,
        "2026-08-16T01:01:05Z",
    );
    let migration_rows = migration_rows(&connection);
    drop(connection);
    UpgradeFixture {
        root,
        project,
        workspace_id,
        task_id,
        session_id,
        event,
        migration_rows,
    }
}

fn migration_rows(connection: &Connection) -> Vec<(u64, String, String)> {
    connection
        .prepare(
            "SELECT migration_id, migration_checksum, applied_at
             FROM schema_migrations ORDER BY migration_id",
        )
        .expect("prepare migration rows")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query migration rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("migration rows")
}

fn direct_connection(project: &Project) -> Connection {
    let connection = Connection::open(&project.state_db_path).expect("direct state connection");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    connection
}

fn canonical_subject_json(value: &EvidenceSubject) -> String {
    let raw = serde_json::to_string(value).expect("typed recovery JSON");
    CanonicalJson::parse(&raw)
        .expect("canonical typed recovery JSON")
        .to_string()
}

fn canonical_payload_json(value: &EvidencePayload) -> String {
    let raw = serde_json::to_string(value).expect("typed recovery JSON");
    CanonicalJson::parse(&raw)
        .expect("canonical typed recovery JSON")
        .to_string()
}

#[allow(clippy::too_many_lines)] // Explicitly seeds every original-0003 recovery relation.
fn seed_parent_snapshot(connection: &Connection, bundle: &MaterializedBundle) -> Sha256Digest {
    for item in &bundle.evidence {
        connection
            .execute(
                "INSERT INTO evidence (
                    evidence_id, task_id, workspace_id, kind, subject, observed_at,
                    collector_name, collector_version, payload, payload_digest, canonical_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    item.evidence_id().as_str(),
                    item.task_id().as_str(),
                    item.workspace_id().as_str(),
                    item.kind().as_str(),
                    canonical_subject_json(item.subject()),
                    item.observed_at().as_str(),
                    item.collector().name,
                    item.collector().version,
                    canonical_payload_json(item.payload()),
                    item.payload_digest().as_str(),
                    item.canonical_digest().as_str(),
                ],
            )
            .expect("parent Evidence row");
    }
    let evidence_reference = |id: &EvidenceId| {
        let item = bundle
            .evidence
            .iter()
            .find(|item| item.evidence_id() == id)
            .expect("snapshot Evidence");
        serde_json::json!({
            "evidence_id": item.evidence_id(),
            "canonical_digest": item.canonical_digest(),
        })
    };
    let references = |ids: &[EvidenceId]| ids.iter().map(evidence_reference).collect::<Vec<_>>();
    let legacy_state = serde_json::json!({
        "schema_version": 1,
        "object_type": "repository_snapshot",
        "snapshot_id": bundle.snapshot.snapshot_id(),
        "project_id": bundle.snapshot.project_id(),
        "task_id": bundle.snapshot.task_id(),
        "workspace_id": bundle.snapshot.workspace_id(),
        "head_oid": bundle.snapshot.head_oid(),
        "branch": bundle.snapshot.branch(),
        "worktree_status_porcelain_v2": bundle.snapshot.worktree_status_porcelain_v2(),
        "is_dirty": bundle.snapshot.is_dirty(),
        "git_head_evidence": evidence_reference(bundle.snapshot.git_head_evidence_id()),
        "git_worktree_evidence": evidence_reference(bundle.snapshot.git_worktree_evidence_id()),
        "file_evidence": references(bundle.snapshot.file_evidence_ids()),
        "command_evidence": references(bundle.snapshot.command_evidence_ids()),
        "test_evidence": references(bundle.snapshot.test_evidence_ids()),
        "captured_at": bundle.snapshot.captured_at(),
    });
    let legacy_digest =
        CanonicalJson::parse(&serde_json::to_string(&legacy_state).expect("legacy Snapshot JSON"))
            .expect("canonical legacy Snapshot")
            .sha256();
    connection
        .execute(
            "INSERT INTO repository_snapshots (
                snapshot_id, project_id, task_id, workspace_id, head_oid, branch,
                worktree_status_porcelain_v2, is_dirty, captured_at, canonical_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                bundle.snapshot.snapshot_id().as_str(),
                bundle.snapshot.project_id().as_str(),
                bundle.snapshot.task_id().as_str(),
                bundle.snapshot.workspace_id().as_str(),
                bundle.snapshot.head_oid(),
                bundle.snapshot.branch(),
                bundle.snapshot.worktree_status_porcelain_v2(),
                bundle.snapshot.is_dirty(),
                bundle.snapshot.captured_at().as_str(),
                legacy_digest.as_str(),
            ],
        )
        .expect("parent RepositorySnapshot row");
    for (role, ids) in [
        (
            "git_head",
            std::slice::from_ref(bundle.snapshot.git_head_evidence_id()),
        ),
        (
            "git_worktree",
            std::slice::from_ref(bundle.snapshot.git_worktree_evidence_id()),
        ),
        ("file", bundle.snapshot.file_evidence_ids()),
        ("command", bundle.snapshot.command_evidence_ids()),
        ("test", bundle.snapshot.test_evidence_ids()),
    ] {
        for (ordinal, id) in ids.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO repository_snapshot_evidence (
                        snapshot_id, evidence_id, role, ordinal
                     ) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        bundle.snapshot.snapshot_id().as_str(),
                        id.as_str(),
                        role,
                        ordinal,
                    ],
                )
                .expect("parent Snapshot Evidence ref");
        }
    }
    connection
        .execute(
            "INSERT INTO checkpoints (
                checkpoint_id, task_id, session_id, snapshot_id, objective, created_at, digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                bundle.checkpoint.checkpoint_id().as_str(),
                bundle.checkpoint.task_id().as_str(),
                bundle.checkpoint.session_id().as_str(),
                bundle.checkpoint.snapshot_id().as_str(),
                bundle.checkpoint.objective(),
                bundle.checkpoint.created_at().as_str(),
                bundle.checkpoint.digest().as_str(),
            ],
        )
        .expect("parent Checkpoint row");
    for (section, statements) in [
        (
            "verified_completed_work",
            bundle.checkpoint.verified_completed_work(),
        ),
        ("open_items", bundle.checkpoint.open_items()),
        (
            "next_action",
            std::slice::from_ref(bundle.checkpoint.next_action()),
        ),
    ] {
        for (item_ordinal, statement) in statements.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO checkpoint_items (
                        checkpoint_id, section, ordinal, classification, text
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        bundle.checkpoint.checkpoint_id().as_str(),
                        section,
                        item_ordinal,
                        statement.classification().as_str(),
                        statement.text(),
                    ],
                )
                .expect("parent Checkpoint item");
            for (evidence_ordinal, evidence_id) in statement.evidence_ids().iter().enumerate() {
                connection
                    .execute(
                        "INSERT INTO checkpoint_item_evidence (
                            checkpoint_id, section, item_ordinal, evidence_id, evidence_ordinal
                         ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            bundle.checkpoint.checkpoint_id().as_str(),
                            section,
                            item_ordinal,
                            evidence_id.as_str(),
                            evidence_ordinal,
                        ],
                    )
                    .expect("parent Checkpoint Evidence ref");
            }
        }
    }
    legacy_digest
}

#[test]
fn migration_0002_to_latest_preserves_existing_state_and_is_restart_stable() {
    let fixture = upgrade_fixture();

    let state = open_state(fixture.root.path(), &fixture).expect("upgrade to latest");
    let first_event = state
        .get_event(&fixture.event.event_id)
        .expect("baseline Event");
    let before_event_bytes = (
        first_event.payload().as_str().to_owned(),
        first_event.payload_sha256().to_string(),
        first_event.event_sha256().to_string(),
    );
    assert_eq!(
        state
            .get_workspace(&fixture.workspace_id)
            .expect("workspace")
            .workspace_id,
        fixture.workspace_id
    );
    assert_eq!(
        state.get_task(&fixture.task_id).expect("task").task_id,
        fixture.task_id
    );
    assert_eq!(
        state
            .get_session(&fixture.session_id)
            .expect("session")
            .session_id,
        fixture.session_id
    );
    drop(state);
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let reopened_event = state.get_event(&fixture.event.event_id).expect("event");
    assert_eq!(
        (
            reopened_event.payload().as_str().to_owned(),
            reopened_event.payload_sha256().to_string(),
            reopened_event.event_sha256().to_string(),
        ),
        before_event_bytes
    );
    drop(state);

    let connection = direct_connection(&fixture.project);
    let after_first_open = migration_rows(&connection);
    // U8 A1: migration 0008; U9 A7: 0009.
    assert_eq!(after_first_open.len(), 9);
    assert_eq!(&after_first_open[..2], fixture.migration_rows.as_slice());
    let migration_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations/state/0003_recovery_kernel.sql");
    let migration_bytes = fs::read(migration_path).expect("0003 migration bytes");
    assert_eq!(
        after_first_open[2].1,
        Sha256Digest::of_bytes(&migration_bytes).to_string()
    );
    assert_eq!(
        after_first_open[3].1,
        Sha256Digest::of_bytes(STATE_0004.as_bytes()).to_string()
    );
    assert_eq!(
        after_first_open[4].1,
        Sha256Digest::of_bytes(STATE_0005.as_bytes()).to_string()
    );
    assert_eq!(
        after_first_open[5].1,
        Sha256Digest::of_bytes(STATE_0006.as_bytes()).to_string()
    );
    assert_eq!(
        after_first_open[6].1,
        Sha256Digest::of_bytes(STATE_0007.as_bytes()).to_string()
    );
    drop(connection);

    drop(open_state(fixture.root.path(), &fixture).expect("restart"));
    let connection = direct_connection(&fixture.project);
    assert_eq!(migration_rows(&connection), after_first_open);
}

#[test]
fn parent_database_with_original_0003_checksum_upgrades_to_latest() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch(STATE_0003)
        .expect("parent migration 0003 schema");
    connection
        .execute(
            "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
             VALUES (3, ?1, ?2)",
            params![ORIGINAL_STATE_0003_SHA256, "2026-08-16T01:00:03Z"],
        )
        .expect("parent migration 0003 row");
    let legacy_digest = seed_parent_snapshot(&connection, &bundle);
    drop(connection);

    let state = open_state(fixture.root.path(), &fixture)
        .expect("open parent-version database and apply forward migration");
    let reopened = state
        .get_repository_snapshot(bundle.snapshot.snapshot_id())
        .expect("legacy Snapshot readback after forward migration");
    assert_eq!(reopened.canonical_digest(), &legacy_digest);
    assert_eq!(reopened.head_oid(), bundle.snapshot.head_oid());
    assert_eq!(
        reopened.file_evidence_ids(),
        bundle.snapshot.file_evidence_ids()
    );
    let reopened_checkpoint = state
        .get_checkpoint(bundle.checkpoint.checkpoint_id())
        .expect("legacy Checkpoint readback after forward migration");
    assert_eq!(reopened_checkpoint.digest(), bundle.checkpoint.digest());
    assert_eq!(reopened_checkpoint.project_event_sequence(), 0);
    drop(state);
    let connection = direct_connection(&fixture.project);
    let rows = migration_rows(&connection);
    // U8 A1: migration 0008; U9 A7: 0009.
    assert_eq!(rows.len(), 9);
    assert_eq!(rows[2].0, 3);
    assert_eq!(rows[2].1, ORIGINAL_STATE_0003_SHA256);
    assert_eq!(
        rows[3].1,
        Sha256Digest::of_bytes(STATE_0004.as_bytes()).as_str()
    );
    assert_eq!(
        rows[4].1,
        Sha256Digest::of_bytes(STATE_0005.as_bytes()).as_str()
    );
    assert_eq!(
        Sha256Digest::of_bytes(STATE_0003.as_bytes()).as_str(),
        ORIGINAL_STATE_0003_SHA256
    );
}

#[test]
fn migration_0003_has_only_the_frozen_recovery_schema_objects() {
    let fixture = upgrade_fixture();
    drop(open_state(fixture.root.path(), &fixture).expect("migrate"));
    let connection = direct_connection(&fixture.project);
    let tables = schema_names(&connection, "table");
    for expected in [
        "evidence",
        "repository_snapshots",
        "repository_snapshot_evidence",
        "checkpoints",
        "checkpoint_items",
        "checkpoint_item_evidence",
    ] {
        assert!(
            tables.contains(&expected.to_owned()),
            "missing table {expected}"
        );
    }
    let recovery_indexes = schema_names_like(
        &connection,
        "index",
        &[
            "evidence",
            "repository_snapshots",
            "repository_snapshot_evidence",
            "checkpoints",
            "checkpoint_items",
            "checkpoint_item_evidence",
        ],
    );
    assert_eq!(
        recovery_indexes,
        vec![
            "checkpoint_item_evidence_order_idx",
            "checkpoint_items_order_idx",
            "checkpoints_task_order_idx",
            "evidence_task_workspace_kind_idx",
            "repository_snapshot_evidence_role_idx",
            "repository_snapshots_task_order_idx",
        ]
    );
    let recovery_triggers = schema_names_like(
        &connection,
        "trigger",
        &[
            "evidence",
            "repository_snapshots",
            "repository_snapshot_evidence",
            "checkpoints",
            "checkpoint_items",
            "checkpoint_item_evidence",
        ],
    );
    assert_eq!(recovery_triggers.len(), 26);
}

fn schema_names(connection: &Connection, object_type: &str) -> Vec<String> {
    connection
        .prepare("SELECT name FROM sqlite_master WHERE type = ?1 ORDER BY name")
        .expect("prepare schema names")
        .query_map([object_type], |row| row.get(0))
        .expect("query schema names")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema names")
}

fn schema_names_like(connection: &Connection, object_type: &str, tables: &[&str]) -> Vec<String> {
    let mut names = Vec::new();
    for table in tables {
        names.extend(
            connection
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = ?1 AND tbl_name = ?2 AND sql IS NOT NULL ORDER BY name",
                )
                .expect("prepare recovery schema names")
                .query_map([object_type, *table], |row| row.get(0))
                .expect("query recovery schema names")
                .collect::<Result<Vec<String>, _>>()
                .expect("recovery schema names"),
        );
    }
    names.sort();
    names.dedup();
    names
}

#[test]
fn all_recovery_rows_are_append_only_even_with_recursive_triggers_off() {
    let fixture = upgrade_fixture();
    drop(open_state(fixture.root.path(), &fixture).expect("migrate"));
    let connection = direct_connection(&fixture.project);
    seed_recovery_rows(&connection, &fixture);
    connection
        .pragma_update(None, "recursive_triggers", "OFF")
        .expect("disable recursive triggers");

    for (table, column) in [
        ("evidence", "collector_name"),
        ("repository_snapshots", "head_oid"),
        ("repository_snapshot_evidence", "role"),
        ("checkpoints", "objective"),
        ("checkpoint_items", "text"),
        ("checkpoint_item_evidence", "evidence_ordinal"),
    ] {
        let update = format!("UPDATE {table} SET {column} = {column}");
        assert!(
            connection.execute(&update, []).is_err(),
            "UPDATE mutated {table}"
        );
        let replace = format!("INSERT OR REPLACE INTO {table} SELECT * FROM {table} LIMIT 1");
        assert!(
            connection.execute(&replace, []).is_err(),
            "REPLACE overwrote {table}"
        );
        let delete = format!("DELETE FROM {table}");
        assert!(
            connection.execute(&delete, []).is_err(),
            "DELETE mutated {table}"
        );
    }
}

#[allow(clippy::too_many_lines)]
fn seed_recovery_rows(connection: &Connection, fixture: &UpgradeFixture) {
    let head_id = memprove_domain::EvidenceId::new();
    let worktree_id = memprove_domain::EvidenceId::new();
    let snapshot_id = memprove_domain::RepositorySnapshotId::new();
    let checkpoint_id = memprove_domain::CheckpointId::new();
    for (id, kind, subject, payload, payload_digest, canonical_digest) in [
        (
            head_id.as_str(),
            "git_head",
            r#"{"repository_identity":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","type":"git_head"}"#,
            r#"{"branch":"main","head_oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","type":"git_head"}"#,
            "1111111111111111111111111111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222222222222222222222222222",
        ),
        (
            worktree_id.as_str(),
            "git_worktree",
            r#"{"repository_identity":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","type":"git_worktree"}"#,
            r#"{"status_porcelain_v2":"","type":"git_worktree"}"#,
            "3333333333333333333333333333333333333333333333333333333333333333",
            "4444444444444444444444444444444444444444444444444444444444444444",
        ),
    ] {
        connection
            .execute(
                "INSERT INTO evidence (
                    evidence_id, task_id, workspace_id, kind, subject, observed_at,
                    collector_name, collector_version, payload, payload_digest, canonical_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'test', '1', ?7, ?8, ?9)",
                params![
                    id,
                    fixture.task_id.as_str(),
                    fixture.workspace_id.as_str(),
                    kind,
                    subject,
                    "2026-08-16T01:02:03Z",
                    payload,
                    payload_digest,
                    canonical_digest
                ],
            )
            .expect("Evidence row");
    }
    connection
        .execute(
            "INSERT INTO repository_snapshots (
                snapshot_id, project_id, task_id, workspace_id, head_oid, branch,
                worktree_status_porcelain_v2, capture_format_version, index_digest,
                tracked_worktree_digest, capture_manifest_digest, is_dirty, captured_at,
                canonical_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'main', '', 2, ?6, ?7, ?8, 0, ?9, ?10)",
            params![
                snapshot_id.as_str(),
                fixture.project.project_id.as_str(),
                fixture.task_id.as_str(),
                fixture.workspace_id.as_str(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "7777777777777777777777777777777777777777777777777777777777777777",
                "8888888888888888888888888888888888888888888888888888888888888888",
                "9999999999999999999999999999999999999999999999999999999999999999",
                "2026-08-16T01:02:04Z",
                "5555555555555555555555555555555555555555555555555555555555555555",
            ],
        )
        .expect("snapshot row");
    for (id, role, ordinal) in [
        (head_id.as_str(), "git_head", 0),
        (worktree_id.as_str(), "git_worktree", 0),
    ] {
        connection
            .execute(
                "INSERT INTO repository_snapshot_evidence (
                    snapshot_id, evidence_id, role, ordinal
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![snapshot_id.as_str(), id, role, ordinal],
            )
            .expect("snapshot Evidence ref");
    }
    connection
        .execute(
            "INSERT INTO checkpoints (
                checkpoint_id, task_id, session_id, snapshot_id, project_event_sequence,
                objective, created_at, digest
             ) VALUES (?1, ?2, ?3, ?4, 1, 'capture', ?5, ?6)",
            params![
                checkpoint_id.as_str(),
                fixture.task_id.as_str(),
                fixture.session_id.as_str(),
                snapshot_id.as_str(),
                "2026-08-16T01:02:05Z",
                "6666666666666666666666666666666666666666666666666666666666666666"
            ],
        )
        .expect("checkpoint row");
    connection
        .execute(
            "INSERT INTO checkpoint_items (
                checkpoint_id, section, ordinal, classification, text
             ) VALUES (?1, 'verified_completed_work', 0, 'verified_fact', 'HEAD observed')",
            [checkpoint_id.as_str()],
        )
        .expect("checkpoint item");
    connection
        .execute(
            "INSERT INTO checkpoint_item_evidence (
                checkpoint_id, section, item_ordinal, evidence_id, evidence_ordinal
             ) VALUES (?1, 'verified_completed_work', 0, ?2, 0)",
            params![checkpoint_id.as_str(), head_id.as_str()],
        )
        .expect("checkpoint item Evidence ref");
}

struct MaterializedBundle {
    evidence: Vec<Evidence>,
    snapshot: RepositorySnapshot,
    probe: RepositoryProbe,
    checkpoint: Checkpoint,
    event_id: EventId,
    recorded_at: UtcTimestamp,
    idempotency: IdempotencyContext,
    request_digest: Sha256Digest,
}

#[allow(clippy::too_many_lines)]
fn materialized_bundle(fixture: &UpgradeFixture) -> MaterializedBundle {
    let command_evidence = typed_evidence(
        fixture,
        EvidenceKind::CommandResult,
        EvidenceSubject::CommandResult {
            command: "git status".to_owned(),
        },
        EvidencePayload::CommandResult {
            argv: vec!["git".to_owned(), "status".to_owned()],
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            started_at: None,
            finished_at: None,
            stdout_sha256: None,
            stderr_sha256: None,
            launch_error: None,
        },
    );
    let test_evidence = typed_evidence(
        fixture,
        EvidenceKind::TestResult,
        EvidenceSubject::TestResult {
            test_name: "recovery kernel".to_owned(),
        },
        EvidencePayload::TestResult {
            argv: vec!["cargo".to_owned(), "test".to_owned()],
            exit_code: 0,
            stdout: "ok".to_owned(),
            stderr: String::new(),
            passed: true,
            test_case_count: Some(1),
            test_format: None,
            started_at: None,
            finished_at: None,
            stdout_sha256: None,
            stderr_sha256: None,
            launch_error: None,
        },
    );
    let captured = RepositorySnapshotCollector::capture(
        &fixture.root.path().join("repository"),
        RepositoryCaptureRequest {
            snapshot_id: RepositorySnapshotId::new(),
            project_id: fixture.project.project_id.clone(),
            task_id: fixture.task_id.clone(),
            workspace_id: fixture.workspace_id.clone(),
            git_head_evidence_id: EvidenceId::new(),
            git_worktree_evidence_id: EvidenceId::new(),
            selected_files: vec![SelectedFileCapture {
                evidence_id: EvidenceId::new(),
                path: "repo://tracked.txt".to_owned(),
            }],
            command_evidence: vec![command_evidence],
            test_evidence: vec![test_evidence],
            observed_at: timestamp("2026-08-16T01:02:59Z"),
            captured_at: timestamp("2026-08-16T01:03:00Z"),
            collector: EvidenceCollector {
                name: "memprove-repo".to_owned(),
                version: "0.1.0".to_owned(),
            },
        },
    )
    .expect("repository capture");
    let evidence = captured.evidence;
    let snapshot = captured.snapshot;
    let probe = captured.probe;
    let checkpoint = Checkpoint::new(
        CheckpointInput {
            checkpoint_id: CheckpointId::new(),
            task_id: fixture.task_id.clone(),
            session_id: fixture.session_id.clone(),
            snapshot_id: snapshot.snapshot_id().clone(),
            project_event_sequence: 1,
            objective: "persist recovery state".to_owned(),
            verified_completed_work: vec![
                CheckpointStatement::new(
                    CheckpointClassification::VerifiedFact,
                    "HEAD and tests observed".to_owned(),
                    vec![
                        evidence[4].evidence_id().clone(),
                        evidence[0].evidence_id().clone(),
                    ],
                )
                .expect("verified statement"),
            ],
            open_items: vec![
                CheckpointStatement::new(
                    CheckpointClassification::Unknown,
                    "independent review pending".to_owned(),
                    vec![],
                )
                .expect("open item"),
            ],
            next_action: CheckpointStatement::new(
                CheckpointClassification::Recommendation,
                "request independent review".to_owned(),
                vec![],
            )
            .expect("next action"),
            created_at: timestamp("2026-08-16T01:03:01Z"),
        },
        &snapshot,
        &evidence,
    )
    .expect("checkpoint");
    MaterializedBundle {
        evidence,
        snapshot,
        probe,
        checkpoint,
        event_id: EventId::new(),
        recorded_at: timestamp("2026-08-16T01:03:02Z"),
        idempotency: IdempotencyContext {
            principal_id: PrincipalId::parse("test-principal").expect("principal"),
            operation_id: OperationId::parse("commitCheckpointBundle").expect("operation"),
            key: IdempotencyKey::parse("recovery-bundle-1").expect("key"),
        },
        request_digest: Sha256Digest::of_bytes(b"materialized recovery bundle request"),
    }
}

fn typed_evidence(
    fixture: &UpgradeFixture,
    kind: EvidenceKind,
    subject: EvidenceSubject,
    payload: EvidencePayload,
) -> Evidence {
    Evidence::new(EvidenceInput {
        evidence_id: EvidenceId::new(),
        task_id: fixture.task_id.clone(),
        workspace_id: fixture.workspace_id.clone(),
        kind,
        subject,
        observed_at: timestamp("2026-08-16T01:02:59Z"),
        collector: EvidenceCollector {
            name: "memprove-test".to_owned(),
            version: "1".to_owned(),
        },
        payload,
    })
    .expect("typed Evidence")
}

#[test]
fn full_bundle_is_atomic_idempotent_and_identical_after_restart() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    let capture_context = state
        .begin_checkpoint_capture(&fixture.task_id, &fixture.workspace_id)
        .expect("capture context");

    let created = state
        .commit_checkpoint_bundle(
            &capture_context,
            &bundle.evidence,
            &bundle.snapshot,
            &bundle.probe,
            &bundle.checkpoint,
            &bundle.event_id,
            &bundle.recorded_at,
            &bundle.idempotency,
            &bundle.request_digest,
        )
        .expect("commit bundle");
    assert_eq!(created, Idempotent::Created(bundle.checkpoint.clone()));
    for expected in &bundle.evidence {
        assert_eq!(
            state
                .get_evidence(expected.evidence_id())
                .expect("Evidence readback"),
            *expected
        );
    }
    assert_eq!(
        state
            .get_repository_snapshot(bundle.snapshot.snapshot_id())
            .expect("snapshot readback"),
        bundle.snapshot
    );
    assert_eq!(
        state
            .get_checkpoint(bundle.checkpoint.checkpoint_id())
            .expect("checkpoint readback"),
        bundle.checkpoint
    );
    let events = state.list_events(&fixture.session_id).expect("events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].event_id(), &bundle.event_id);
    assert_eq!(events[1].event_type(), "checkpoint.created");
    let event_payload = serde_json::from_str::<serde_json::Value>(events[1].payload().as_str())
        .expect("checkpoint.created payload");
    assert_eq!(
        event_payload["checkpoint"]["checkpoint_id"],
        bundle.checkpoint.checkpoint_id().as_str()
    );
    drop(state);

    let mut reopened = open_state(fixture.root.path(), &fixture).expect("restart");
    assert_eq!(
        reopened
            .get_checkpoint(bundle.checkpoint.checkpoint_id())
            .expect("checkpoint after restart"),
        bundle.checkpoint
    );
    let replayed = reopened
        .commit_checkpoint_bundle(
            &capture_context,
            &bundle.evidence,
            &bundle.snapshot,
            &bundle.probe,
            &bundle.checkpoint,
            &bundle.event_id,
            &bundle.recorded_at,
            &bundle.idempotency,
            &bundle.request_digest,
        )
        .expect("idempotent replay");
    assert_eq!(replayed, Idempotent::Replayed(bundle.checkpoint.clone()));
    assert_eq!(
        reopened
            .list_events(&fixture.session_id)
            .expect("events")
            .len(),
        2
    );
    assert!(matches!(
        reopened.commit_checkpoint_bundle(
            &capture_context,
            &bundle.evidence,
            &bundle.snapshot,
            &bundle.probe,
            &bundle.checkpoint,
            &bundle.event_id,
            &bundle.recorded_at,
            &bundle.idempotency,
            &Sha256Digest::of_bytes(b"different request"),
        ),
        Err(StoreError::IdempotencyConflict)
    ));
}

#[test]
fn distinct_snapshot_instances_with_the_same_state_digest_persist_independently() {
    let fixture = upgrade_fixture();
    let first = materialized_bundle(&fixture);
    let mut second = materialized_bundle(&fixture);
    assert_ne!(first.snapshot.snapshot_id(), second.snapshot.snapshot_id());
    assert_eq!(
        first.snapshot.canonical_digest(),
        second.snapshot.canonical_digest()
    );

    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    assert!(matches!(
        commit_bundle(&mut state, &first),
        Ok(Idempotent::Created(_))
    ));
    let mut checkpoint_input = second.checkpoint.to_input();
    checkpoint_input.project_event_sequence = 2;
    second.checkpoint = Checkpoint::new(checkpoint_input, &second.snapshot, &second.evidence)
        .expect("second checkpoint");
    second.idempotency.key = IdempotencyKey::parse("recovery-bundle-2").expect("key");
    second.request_digest = Sha256Digest::of_bytes(b"second materialized recovery bundle request");
    assert!(matches!(
        commit_bundle(&mut state, &second),
        Ok(Idempotent::Created(_))
    ));
    drop(state);

    let connection = direct_connection(&fixture.project);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM repository_snapshots", [], |row| row
                .get::<_, u64>(
                0
            ))
            .expect("snapshot count"),
        2
    );
}

fn commit_bundle(
    state: &mut StateStore,
    bundle: &MaterializedBundle,
) -> Result<Idempotent<Checkpoint>, StoreError> {
    let capture = state
        .begin_checkpoint_capture(bundle.snapshot.task_id(), bundle.snapshot.workspace_id())?;
    state.commit_checkpoint_bundle(
        &capture,
        &bundle.evidence,
        &bundle.snapshot,
        &bundle.probe,
        &bundle.checkpoint,
        &bundle.event_id,
        &bundle.recorded_at,
        &bundle.idempotency,
        &bundle.request_digest,
    )
}

fn durable_counts(project: &Project) -> Vec<u64> {
    let connection = direct_connection(project);
    [
        "evidence",
        "repository_snapshots",
        "repository_snapshot_evidence",
        "checkpoints",
        "checkpoint_items",
        "checkpoint_item_evidence",
        "state_idempotency_records",
        "events",
    ]
    .iter()
    .map(|table| {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count durable rows")
    })
    .collect()
}

fn assert_zero_partial_bundle(project: &Project, expected_event_count: u64) {
    assert_eq!(
        durable_counts(project),
        vec![0, 0, 0, 0, 0, 0, 0, expected_event_count]
    );
}

#[test]
fn missing_evidence_and_session_mismatch_fail_before_any_bundle_row() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    let capture = state
        .begin_checkpoint_capture(&fixture.task_id, &fixture.workspace_id)
        .expect("capture context");
    let missing = state.commit_checkpoint_bundle(
        &capture,
        &bundle.evidence[..4],
        &bundle.snapshot,
        &bundle.probe,
        &bundle.checkpoint,
        &bundle.event_id,
        &bundle.recorded_at,
        &bundle.idempotency,
        &bundle.request_digest,
    );
    assert!(matches!(missing, Err(StoreError::ConstraintViolation(_))));
    drop(state);
    assert_zero_partial_bundle(&fixture.project, 1);

    let mut mismatched_bundle = materialized_bundle(&fixture);
    mismatched_bundle.checkpoint = Checkpoint::new(
        CheckpointInput {
            session_id: SessionId::new(),
            ..mismatched_bundle.checkpoint.to_input()
        },
        &mismatched_bundle.snapshot,
        &mismatched_bundle.evidence,
    )
    .expect("domain permits storage-owned Session association validation");
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    let mismatch = commit_bundle(&mut state, &mismatched_bundle);
    assert!(matches!(
        mismatch,
        Err(StoreError::NotFound("Session for Checkpoint"))
    ));
    drop(state);
    assert_zero_partial_bundle(&fixture.project, 1);
}

#[test]
fn failures_at_final_item_and_event_roll_back_everything() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    drop(open_state(fixture.root.path(), &fixture).expect("migrate"));
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch(
            "CREATE TRIGGER inject_final_item_failure
             BEFORE INSERT ON checkpoint_items
             WHEN NEW.section = 'open_items'
             BEGIN SELECT RAISE(ABORT, 'injected final item failure'); END;",
        )
        .expect("failure trigger");
    drop(connection);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    assert!(matches!(
        commit_bundle(&mut state, &bundle),
        Err(StoreError::ConstraintViolation(_))
    ));
    drop(state);
    assert_zero_partial_bundle(&fixture.project, 1);

    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch("DROP TRIGGER inject_final_item_failure;")
        .expect("drop failure trigger");
    drop(connection);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    state
        .append_event(&EventInput {
            event_id: bundle.event_id.clone(),
            ..fixture_event(
                &fixture.project.project_id,
                &fixture.workspace_id,
                &fixture.session_id,
                "collision",
                r#"{"collision":true}"#,
                "2026-08-16T01:02:58Z",
            )
        })
        .expect("colliding Event identity");
    assert!(matches!(
        commit_bundle(&mut state, &bundle),
        Err(StoreError::ConstraintViolation(_))
    ));
    drop(state);
    assert_zero_partial_bundle(&fixture.project, 2);
}

#[test]
fn capability_root_replacement_at_precommit_fails_closed_and_rolls_back() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let approved = fixture.root.path().to_owned();
    let original = approved.with_extension("original-root");
    let outside = TempDir::new().expect("outside root");
    let mut state = open_state(&approved, &fixture).expect("state");
    let capture = state
        .begin_checkpoint_capture(&fixture.task_id, &fixture.workspace_id)
        .expect("capture context");
    fs::rename(&approved, &original).expect("replace approved root");
    std::os::unix::fs::symlink(outside.path(), &approved).expect("replacement symlink");
    let failure = state.commit_checkpoint_bundle(
        &capture,
        &bundle.evidence,
        &bundle.snapshot,
        &bundle.probe,
        &bundle.checkpoint,
        &bundle.event_id,
        &bundle.recorded_at,
        &bundle.idempotency,
        &bundle.request_digest,
    );
    assert!(matches!(failure, Err(StoreError::CapabilityVfs(_))));
    drop(state);
    fs::remove_file(&approved).expect("remove replacement symlink");
    fs::rename(&original, &approved).expect("restore approved root");
    assert_zero_partial_bundle(&fixture.project, 1);
}

#[test]
fn sqlite_fk_and_role_kind_constraints_fail_closed() {
    let fixture = upgrade_fixture();
    drop(open_state(fixture.root.path(), &fixture).expect("migrate"));
    let connection = direct_connection(&fixture.project);
    let missing = EvidenceId::new();
    assert!(
        connection
            .execute(
                "INSERT INTO evidence (
                    evidence_id, task_id, workspace_id, kind, subject, observed_at,
                    collector_name, collector_version, payload, payload_digest, canonical_digest
                 ) VALUES (?1, ?2, ?3, 'git_head', '{}', ?4, 'test', '1', '{}', ?5, ?6)",
                params![
                    missing.as_str(),
                    TaskId::new().as_str(),
                    fixture.workspace_id.as_str(),
                    "2026-08-16T01:02:03Z",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ],
            )
            .is_err()
    );

    let bundle = materialized_bundle(&fixture);
    drop(connection);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("bundle");
    drop(state);
    let connection = direct_connection(&fixture.project);
    let extra = EvidenceId::new();
    connection
        .execute(
            "INSERT INTO evidence (
                evidence_id, task_id, workspace_id, kind, subject, observed_at,
                collector_name, collector_version, payload, payload_digest, canonical_digest
             ) VALUES (?1, ?2, ?3, 'command_result', ?4, ?5, 'test', '1', ?6, ?7, ?8)",
            params![
                extra.as_str(),
                fixture.task_id.as_str(),
                fixture.workspace_id.as_str(),
                r#"{"command":"x","type":"command_result"}"#,
                "2026-08-16T01:02:03Z",
                r#"{"argv":[],"exit_code":0,"stderr":"","stdout":"","type":"command_result"}"#,
                "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            ],
        )
        .expect("extra Evidence");
    assert!(
        connection
            .execute(
                "INSERT INTO repository_snapshot_evidence (
                    snapshot_id, evidence_id, role, ordinal
                 ) VALUES (?1, ?2, 'file', 1)",
                params![bundle.snapshot.snapshot_id().as_str(), extra.as_str()],
            )
            .is_err()
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn readback_rejects_every_tampered_digest_and_impossible_reference() {
    for (table, trigger, column, id_column, target) in [
        (
            "evidence",
            "evidence_reject_update",
            "payload_digest",
            "evidence_id",
            "evidence",
        ),
        (
            "evidence",
            "evidence_reject_update",
            "canonical_digest",
            "evidence_id",
            "evidence",
        ),
        (
            "repository_snapshots",
            "repository_snapshots_reject_update",
            "canonical_digest",
            "snapshot_id",
            "snapshot",
        ),
        (
            "checkpoints",
            "checkpoints_reject_update",
            "digest",
            "checkpoint_id",
            "checkpoint",
        ),
    ] {
        let fixture = upgrade_fixture();
        let bundle = materialized_bundle(&fixture);
        let mut state = open_state(fixture.root.path(), &fixture).expect("state");
        commit_bundle(&mut state, &bundle).expect("bundle");
        drop(state);
        let connection = direct_connection(&fixture.project);
        connection
            .execute_batch(&format!("DROP TRIGGER {trigger};"))
            .expect("drop append trigger for corruption probe");
        let id = match id_column {
            "evidence_id" => bundle.evidence[0].evidence_id().as_str(),
            "snapshot_id" => bundle.snapshot.snapshot_id().as_str(),
            "checkpoint_id" => bundle.checkpoint.checkpoint_id().as_str(),
            _ => unreachable!(),
        };
        connection
            .execute(
                &format!("UPDATE {table} SET {column} = ?1 WHERE {id_column} = ?2"),
                params![
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    id
                ],
            )
            .expect("tamper digest");
        drop(connection);
        let state = open_state(fixture.root.path(), &fixture).expect("reopen");
        let result = match target {
            "evidence" => state
                .get_evidence(bundle.evidence[0].evidence_id())
                .map(|_| ()),
            "snapshot" => state
                .get_repository_snapshot(bundle.snapshot.snapshot_id())
                .map(|_| ()),
            "checkpoint" => state
                .get_checkpoint(bundle.checkpoint.checkpoint_id())
                .map(|_| ()),
            _ => unreachable!(),
        };
        assert!(matches!(result, Err(StoreError::PersistedDataIntegrity(_))));
    }

    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("bundle");
    drop(state);
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch(
            "DROP TRIGGER repository_snapshot_evidence_reject_update;
             UPDATE repository_snapshot_evidence SET role = 'file', ordinal = 99
             WHERE role = 'git_head';",
        )
        .expect("tamper role");
    drop(connection);
    let state = open_state(fixture.root.path(), &fixture).expect("reopen");
    assert!(matches!(
        state.get_repository_snapshot(bundle.snapshot.snapshot_id()),
        Err(StoreError::PersistedDataIntegrity(_))
    ));

    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("bundle");
    drop(state);
    let connection = direct_connection(&fixture.project);
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for corruption probe");
    connection
        .execute_batch("DROP TRIGGER evidence_reject_delete;")
        .expect("drop Evidence delete trigger");
    connection
        .execute(
            "DELETE FROM evidence WHERE evidence_id = ?1",
            [bundle.evidence[0].evidence_id().as_str()],
        )
        .expect("delete referenced Evidence");
    drop(connection);
    let state = open_state(fixture.root.path(), &fixture).expect("reopen");
    assert!(matches!(
        state.get_repository_snapshot(bundle.snapshot.snapshot_id()),
        Err(StoreError::PersistedDataIntegrity(_))
    ));
}

#[test]
fn cross_workspace_evidence_corruption_is_rejected_on_snapshot_readback() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("bundle");
    drop(state);
    let other_workspace = WorkspaceId::new();
    let connection = direct_connection(&fixture.project);
    connection
        .execute(
            "INSERT INTO workspaces (
                workspace_id, project_id, canonical_worktree_path, git_dir,
                head_commit, branch_or_detached_state, created_at, revision
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'branch:other', ?6, 1)",
            params![
                other_workspace.as_str(),
                fixture.project.project_id.as_str(),
                fixture
                    .root
                    .path()
                    .join("other-worktree")
                    .display()
                    .to_string(),
                fixture
                    .root
                    .path()
                    .join("other-git-dir")
                    .display()
                    .to_string(),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "2026-08-16T01:04:00Z",
            ],
        )
        .expect("other Workspace");
    connection
        .execute_batch("DROP TRIGGER evidence_reject_update;")
        .expect("drop Evidence update trigger");
    connection
        .execute(
            "UPDATE evidence SET workspace_id = ?1 WHERE evidence_id = ?2",
            params![
                other_workspace.as_str(),
                bundle.evidence[0].evidence_id().as_str()
            ],
        )
        .expect("cross-workspace corruption");
    drop(connection);
    let state = open_state(fixture.root.path(), &fixture).expect("reopen");
    assert!(matches!(
        state.get_repository_snapshot(bundle.snapshot.snapshot_id()),
        Err(StoreError::PersistedDataIntegrity(_))
    ));
}

fn git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("Git fixture command");
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

fn bind_fixture_to_real_repository(fixture: &UpgradeFixture) -> std::path::PathBuf {
    fixture.root.path().join("repository")
}

fn captured_bundle(
    fixture: &UpgradeFixture,
    repository: &Path,
) -> (
    memprove_repo::CapturedRepositorySnapshot,
    Checkpoint,
    EventId,
    IdempotencyContext,
) {
    let captured = RepositorySnapshotCollector::capture(
        repository,
        RepositoryCaptureRequest {
            project_id: fixture.project.project_id.clone(),
            task_id: fixture.task_id.clone(),
            workspace_id: fixture.workspace_id.clone(),
            snapshot_id: RepositorySnapshotId::new(),
            git_head_evidence_id: EvidenceId::new(),
            git_worktree_evidence_id: EvidenceId::new(),
            selected_files: vec![SelectedFileCapture {
                evidence_id: EvidenceId::new(),
                path: "repo://tracked.txt".to_owned(),
            }],
            command_evidence: vec![],
            test_evidence: vec![typed_evidence(
                fixture,
                EvidenceKind::TestResult,
                EvidenceSubject::TestResult {
                    test_name: "real repository capture".to_owned(),
                },
                EvidencePayload::TestResult {
                    argv: vec!["cargo".to_owned(), "test".to_owned()],
                    exit_code: 0,
                    stdout: "ok".to_owned(),
                    stderr: String::new(),
                    passed: true,
                    test_case_count: Some(1),
                    test_format: None,
                    started_at: None,
                    finished_at: None,
                    stdout_sha256: None,
                    stderr_sha256: None,
                    launch_error: None,
                },
            )],
            observed_at: timestamp("2026-08-16T02:00:00Z"),
            captured_at: timestamp("2026-08-16T02:00:01Z"),
            collector: EvidenceCollector {
                name: "memprove-repo".to_owned(),
                version: "0.1.0".to_owned(),
            },
        },
    )
    .expect("real repository capture");
    let checkpoint = Checkpoint::new(
        CheckpointInput {
            checkpoint_id: CheckpointId::new(),
            task_id: fixture.task_id.clone(),
            session_id: fixture.session_id.clone(),
            snapshot_id: captured.snapshot.snapshot_id().clone(),
            project_event_sequence: 1,
            objective: "capture real repository".to_owned(),
            verified_completed_work: vec![
                CheckpointStatement::new(
                    CheckpointClassification::VerifiedFact,
                    "selected file observed".to_owned(),
                    vec![
                        captured.evidence[2].evidence_id().clone(),
                        captured.evidence[3].evidence_id().clone(),
                    ],
                )
                .expect("verified statement"),
            ],
            open_items: vec![],
            next_action: CheckpointStatement::new(
                CheckpointClassification::Recommendation,
                "continue".to_owned(),
                vec![],
            )
            .expect("next action"),
            created_at: timestamp("2026-08-16T02:00:02Z"),
        },
        &captured.snapshot,
        &captured.evidence,
    )
    .expect("Checkpoint");
    (
        captured,
        checkpoint,
        EventId::new(),
        IdempotencyContext {
            principal_id: PrincipalId::parse("capture-test").expect("principal"),
            operation_id: OperationId::parse("commitCheckpointBundle").expect("operation"),
            key: IdempotencyKey::parse("capture-1").expect("key"),
        },
    )
}

#[test]
fn real_capture_precommit_recheck_commits_unchanged_and_rolls_back_stale_candidate() {
    let fixture = upgrade_fixture();
    let repository = bind_fixture_to_real_repository(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    let capture_context = state
        .begin_checkpoint_capture(&fixture.task_id, &fixture.workspace_id)
        .expect("capture context");
    let (captured, checkpoint, event_id, idempotency) = captured_bundle(&fixture, &repository);
    let request_digest = Sha256Digest::of_bytes(b"real capture request");
    let committed = state
        .commit_checkpoint_bundle(
            &capture_context,
            &captured.evidence,
            &captured.snapshot,
            &captured.probe,
            &checkpoint,
            &event_id,
            &timestamp("2026-08-16T02:00:03Z"),
            &idempotency,
            &request_digest,
        )
        .expect("unchanged capture commit");
    assert_eq!(committed, Idempotent::Created(checkpoint));
    drop(state);

    let stale_fixture = upgrade_fixture();
    let stale_repository = bind_fixture_to_real_repository(&stale_fixture);
    let mut state = open_state(stale_fixture.root.path(), &stale_fixture).expect("stale state");
    let capture_context = state
        .begin_checkpoint_capture(&stale_fixture.task_id, &stale_fixture.workspace_id)
        .expect("capture context");
    let (stale_capture, checkpoint, event_id, idempotency) =
        captured_bundle(&stale_fixture, &stale_repository);
    fs::write(stale_repository.join("drift.txt"), b"drift\n").expect("status drift");
    let result = state.commit_checkpoint_bundle(
        &capture_context,
        &stale_capture.evidence,
        &stale_capture.snapshot,
        &stale_capture.probe,
        &checkpoint,
        &event_id,
        &timestamp("2026-08-16T02:00:03Z"),
        &idempotency,
        &Sha256Digest::of_bytes(b"stale capture request"),
    );
    assert!(matches!(result, Err(StoreError::ConstraintViolation(_))));
    drop(state);
    assert_zero_partial_bundle(&stale_fixture.project, 1);
}

#[test]
fn capture_from_a_different_repository_is_rejected_before_any_bundle_row() {
    let fixture = upgrade_fixture();
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    let capture_context = state
        .begin_checkpoint_capture(&fixture.task_id, &fixture.workspace_id)
        .expect("capture context");

    let other_root = TempDir::new().expect("other repository root");
    let other_repository = other_root.path().join("repository");
    fs::create_dir(&other_repository).expect("other repository");
    git(&other_repository, &["init", "--initial-branch=main"]);
    git(&other_repository, &["config", "user.name", "memprove Test"]);
    git(
        &other_repository,
        &["config", "user.email", "memprove@example.invalid"],
    );
    fs::write(other_repository.join("tracked.txt"), b"tracked fixture\n")
        .expect("other tracked file");
    git(&other_repository, &["add", "tracked.txt"]);
    git(&other_repository, &["commit", "-m", "other fixture"]);
    let (foreign, checkpoint, event_id, idempotency) = captured_bundle(&fixture, &other_repository);

    let result = state.commit_checkpoint_bundle(
        &capture_context,
        &foreign.evidence,
        &foreign.snapshot,
        &foreign.probe,
        &checkpoint,
        &event_id,
        &timestamp("2026-08-16T02:00:03Z"),
        &idempotency,
        &Sha256Digest::of_bytes(b"foreign repository request"),
    );
    assert!(matches!(result, Err(StoreError::StateBindingMismatch)));
    drop(state);
    assert_zero_partial_bundle(&fixture.project, 1);

    let repository = bind_fixture_to_real_repository(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    let (captured, checkpoint, event_id, idempotency) = captured_bundle(&fixture, &repository);
    assert!(matches!(
        state.commit_checkpoint_bundle(
            &capture_context,
            &captured.evidence,
            &captured.snapshot,
            &captured.probe,
            &checkpoint,
            &event_id,
            &timestamp("2026-08-16T02:00:03Z"),
            &idempotency,
            &Sha256Digest::of_bytes(b"bound repository request"),
        ),
        Ok(Idempotent::Created(_))
    ));
}

#[test]
fn project_event_append_after_capture_freeze_rejects_checkpoint_without_partial_rows() {
    let fixture = upgrade_fixture();
    let repository = bind_fixture_to_real_repository(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    let capture_context = state
        .begin_checkpoint_capture(&fixture.task_id, &fixture.workspace_id)
        .expect("freeze project event sequence");
    assert_eq!(capture_context.project_event_sequence(), 1);
    let (captured, checkpoint, event_id, idempotency) = captured_bundle(&fixture, &repository);
    let other_session = Session {
        session_id: SessionId::new(),
        workspace_id: fixture.workspace_id.clone(),
        agent_id: AgentId::new(),
        started_at: timestamp("2026-08-16T02:00:02Z"),
        ended_at: None,
        revision: 1,
        agent: None,
        ended_by: None,
    };
    state.create_session(&other_session).expect("other Session");
    state
        .link_session_to_task(
            &fixture.task_id,
            &other_session.session_id,
            &timestamp("2026-08-16T02:00:03Z"),
        )
        .expect("link other Session");
    state
        .append_event(&fixture_event(
            &fixture.project.project_id,
            &fixture.workspace_id,
            &other_session.session_id,
            "external.progress",
            r#"{"source":"other-session"}"#,
            "2026-08-16T02:00:04Z",
        ))
        .expect("other Session Event");

    let result = state.commit_checkpoint_bundle(
        &capture_context,
        &captured.evidence,
        &captured.snapshot,
        &captured.probe,
        &checkpoint,
        &event_id,
        &timestamp("2026-08-16T02:00:05Z"),
        &idempotency,
        &Sha256Digest::of_bytes(b"project event freeze request"),
    );

    assert!(matches!(result, Err(StoreError::ConstraintViolation(_))));
    drop(state);
    // The baseline Event, the link's task.session_linked (P2-C), and the
    // other Session's Event.
    assert_zero_partial_bundle(&fixture.project, 3);
}

/// U1 B2 (T21 V1R-3): the fixture's Task row is written into the raw 0002
/// database, so no `task.created` Event names it; `verifyLedger` reports it,
/// after every other record (a ledger from before P2-E is not supported,
/// Owner P3-C2). Everything else holds when it is the first failure.
fn assert_only_the_pre_0007_task_fails(
    verification: &memprove_domain::LedgerVerification,
    fixture: &UpgradeFixture,
) {
    let failure = verification
        .first_failure
        .as_ref()
        .unwrap_or_else(|| panic!("{verification:?}"));
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Task,
        "{verification:?}"
    );
    assert_eq!(failure.record_id, fixture.task_id.as_str());
    assert_eq!(
        failure.reason,
        "no task.created or task.transitioned Event of memproved records this Task"
    );
}

/// `verifyLedger` (P2-C): an intact ledger verifies with every record
/// counted and the chain head named; a tampered Event row and a tampered
/// Evidence row are each located, with nothing written by the check.
#[test]
fn verify_ledger_counts_every_record_and_locates_tampering() {
    let intact = |fixture: &UpgradeFixture| {
        let bundle = materialized_bundle(fixture);
        let mut state = open_state(fixture.root.path(), fixture).expect("state");
        commit_bundle(&mut state, &bundle).expect("commit bundle");
        let events = state.list_events(&fixture.session_id).expect("events");
        let verification = state.verify_ledger().expect("verify");
        assert_only_the_pre_0007_task_fails(&verification, fixture);
        assert_eq!(verification.project_sequence, 2);
        assert_eq!(verification.events, 2);
        assert_eq!(
            verification.evidence,
            u64::try_from(bundle.evidence.len()).unwrap()
        );
        assert_eq!((verification.snapshots, verification.checkpoints), (1, 1));
        assert_eq!(
            verification.head_event_sha256.as_ref(),
            Some(events[1].event_sha256())
        );
        let before = durable_counts(&fixture.project);
        state.verify_ledger().expect("verify again");
        assert_eq!(durable_counts(&fixture.project), before, "the check wrote");
        (bundle, events)
    };

    // An Event row edited past the triggers.
    let fixture = upgrade_fixture();
    let (_, events) = intact(&fixture);
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch(
            "DROP TRIGGER events_reject_update;
             UPDATE events SET payload = '{\"forged\":true}' WHERE project_sequence = 1;",
        )
        .expect("tamper with an Event row");
    drop(connection);
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let verification = state.verify_ledger().expect("verify tampered Events");
    assert!(!verification.is_valid());
    assert_eq!(verification.events, 2, "every row is still counted");
    let failure = verification.first_failure.expect("located");
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Event
    );
    assert_eq!(failure.record_id, events[0].event_id().as_str());
    assert_eq!(failure.project_sequence, Some(1));
    assert!(failure.reason.contains("digest"), "{}", failure.reason);

    // An Evidence row edited past the triggers; the Event chain still holds.
    let fixture = upgrade_fixture();
    let (bundle, _) = intact(&fixture);
    let tampered = bundle
        .evidence
        .last()
        .expect("Evidence")
        .evidence_id()
        .clone();
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch("DROP TRIGGER evidence_reject_update;")
        .expect("drop trigger");
    connection
        .execute(
            "UPDATE evidence SET observed_at = '2026-01-01T00:00:00Z' WHERE evidence_id = ?1",
            [tampered.as_str()],
        )
        .expect("tamper with an Evidence row");
    drop(connection);
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let verification = state.verify_ledger().expect("verify tampered Evidence");
    assert!(!verification.is_valid());
    let failure = verification.first_failure.expect("located");
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Evidence
    );
    assert_eq!(failure.record_id, tampered.as_str());
    assert_eq!(failure.project_sequence, None);
    assert_eq!(verification.events, 2);
    assert_eq!((verification.snapshots, verification.checkpoints), (1, 1));
}

/// Deletes the rows `deletes` names from the fixture database past the
/// append-only triggers `triggers` (the daemon is stopped; the same OS user).
fn delete_rows(fixture: &UpgradeFixture, triggers: &[&str], deletes: &[(&str, &str)]) {
    let connection = direct_connection(&fixture.project);
    for trigger in triggers {
        connection
            .execute_batch(&format!("DROP TRIGGER {trigger};"))
            .expect("drop trigger");
    }
    for (sql, id) in deletes {
        assert!(
            connection.execute(sql, [id]).expect("delete rows") > 0,
            "{sql}"
        );
    }
}

/// P2-K (T11 S-8): a record that no `memproved` Event records does not
/// verify. Deleting the chain tail that holds its `checkpoint.created`
/// leaves a shorter chain that verifies on its own (the known boundary),
/// but the Evidence, Snapshot, and Checkpoint it announced are now recorded
/// by no Event: `verifyLedger` names the first of them and the Preview
/// reads the Checkpoint as damaged.
#[test]
fn verify_ledger_and_the_preview_refuse_a_record_no_event_records() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("commit bundle");
    assert_only_the_pre_0007_task_fails(&state.verify_ledger().expect("verify"), &fixture);
    drop(state);
    delete_rows(
        &fixture,
        &["events_reject_delete"],
        &[(
            "DELETE FROM events WHERE event_type = 'checkpoint.created' AND event_id = ?1",
            bundle.event_id.as_str(),
        )],
    );
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let verification = state.verify_ledger().expect("verify the shorter chain");
    assert!(!verification.is_valid());
    assert_eq!(verification.events, 1, "the remaining chain is complete");
    let failure = verification.first_failure.expect("located");
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Evidence
    );
    let first = bundle
        .evidence
        .iter()
        .map(|item| item.evidence_id().as_str())
        .min()
        .expect("Evidence");
    assert_eq!(failure.record_id, first);
    assert_eq!(failure.project_sequence, None);
    assert_eq!(
        failure.reason,
        "no command.completed or checkpoint.created Event of memproved records it"
    );
    drop(state);
    match preview_checkpoint(&fixture) {
        memprove_domain::CheckpointView::Damaged {
            checkpoint_id,
            missing_object,
            ..
        } => {
            assert_eq!(&checkpoint_id, bundle.checkpoint.checkpoint_id());
            assert!(!missing_object);
        }
        other => panic!("{other:?}"),
    }
}

/// P2-K (T11 S-1): every record a `memproved` Event records must exist.
/// A deleted Checkpoint, and a deleted run (Checkpoint, Snapshot, and
/// Evidence), are each located with the Project sequence of the Event that
/// records them, counted, and read by the Preview as a missing Checkpoint.
#[test]
#[allow(clippy::too_many_lines)] // Two deletions, each through verifyLedger and the Preview.
fn verify_ledger_and_the_preview_find_a_record_an_event_records_missing() {
    let checkpoint_tables = [
        "DELETE FROM checkpoint_item_evidence WHERE checkpoint_id = ?1",
        "DELETE FROM checkpoint_items WHERE checkpoint_id = ?1",
        "DELETE FROM checkpoints WHERE checkpoint_id = ?1",
    ];
    let checkpoint_triggers = [
        "checkpoint_item_evidence_reject_delete",
        "checkpoint_items_reject_delete",
        "checkpoints_reject_delete",
    ];
    let missing =
        |fixture: &UpgradeFixture, bundle: &MaterializedBundle| match preview_checkpoint(fixture) {
            memprove_domain::CheckpointView::Damaged {
                checkpoint_id,
                stored_digest,
                missing_object,
                invalid_evidence_ids,
            } => {
                assert_eq!(&checkpoint_id, bundle.checkpoint.checkpoint_id());
                assert_eq!(stored_digest, None);
                assert!(missing_object);
                assert!(invalid_evidence_ids.is_empty());
            }
            other => panic!("{other:?}"),
        };

    // The Checkpoint alone.
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("commit bundle");
    drop(state);
    let checkpoint_id = bundle.checkpoint.checkpoint_id().as_str();
    delete_rows(
        &fixture,
        &checkpoint_triggers,
        &checkpoint_tables.map(|sql| (sql, checkpoint_id)),
    );
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let verification = state.verify_ledger().expect("verify a missing Checkpoint");
    assert!(!verification.is_valid());
    assert_eq!(
        (
            verification.events,
            verification.evidence,
            verification.snapshots,
            verification.checkpoints
        ),
        (2, bundle.evidence.len() as u64, 1, 1),
        "the missing Checkpoint is counted"
    );
    let failure = verification.first_failure.expect("located");
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Checkpoint
    );
    assert_eq!(failure.record_id, checkpoint_id);
    assert_eq!(failure.project_sequence, Some(2));
    assert_eq!(
        failure.reason,
        "record is missing; the checkpoint.created Event at Project sequence 2 records it"
    );
    drop(state);
    missing(&fixture, &bundle);

    // The whole run: Checkpoint, Snapshot, and every Evidence object.
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("commit bundle");
    drop(state);
    let checkpoint_id = bundle.checkpoint.checkpoint_id().as_str();
    let snapshot_id = bundle.snapshot.snapshot_id().as_str();
    let mut deletes = checkpoint_tables.map(|sql| (sql, checkpoint_id)).to_vec();
    deletes.push((
        "DELETE FROM repository_snapshot_evidence WHERE snapshot_id = ?1",
        snapshot_id,
    ));
    deletes.push((
        "DELETE FROM repository_snapshots WHERE snapshot_id = ?1",
        snapshot_id,
    ));
    for item in &bundle.evidence {
        deletes.push((
            "DELETE FROM evidence WHERE evidence_id = ?1",
            item.evidence_id().as_str(),
        ));
    }
    let mut triggers = checkpoint_triggers.to_vec();
    triggers.extend([
        "repository_snapshot_evidence_reject_delete",
        "repository_snapshots_reject_delete",
        "evidence_reject_delete",
    ]);
    delete_rows(&fixture, &triggers, &deletes);
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let verification = state.verify_ledger().expect("verify a missing run");
    assert!(!verification.is_valid());
    assert_eq!(
        (
            verification.evidence,
            verification.snapshots,
            verification.checkpoints
        ),
        (bundle.evidence.len() as u64, 1, 1),
        "every missing record is counted"
    );
    let failure = verification.first_failure.expect("located");
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Evidence
    );
    let first = bundle
        .evidence
        .iter()
        .map(|item| item.evidence_id().as_str())
        .min()
        .expect("Evidence");
    assert_eq!(failure.record_id, first);
    assert_eq!(failure.project_sequence, Some(2));
    assert_eq!(
        failure.reason,
        "record is missing; the command.completed or checkpoint.created Event at Project \
         sequence 2 records it"
    );
    drop(state);
    missing(&fixture, &bundle);
}

/// P2-K (T11 S-2): Task completion reads each cited Evidence object through
/// the ledger agreement the Preview applies. Evidence rewritten with a
/// self-consistent digest still recomputes, but its `checkpoint.created`
/// Event names the old digest, so neither it nor any other Evidence of its
/// Snapshot completes a Task.
#[test]
fn completion_evidence_agrees_with_the_ledger() {
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("commit bundle");
    let target = bundle.evidence.last().expect("Evidence");
    let read = state
        .completion_evidence(target.evidence_id())
        .expect("intact completion Evidence");
    assert_eq!(&read.evidence, target);
    assert_eq!(read.snapshot.as_ref(), Some(&bundle.snapshot));
    assert_eq!(read.snapshot_evidence.len(), bundle.evidence.len());
    drop(state);
    let mut input = target.to_input();
    input.observed_at = timestamp("2026-01-01T00:00:00Z");
    let rewritten = Evidence::new(input).expect("self-consistent Evidence");
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch("DROP TRIGGER evidence_reject_update;")
        .expect("drop trigger");
    connection
        .execute(
            "UPDATE evidence SET observed_at = ?1, canonical_digest = ?2 WHERE evidence_id = ?3",
            params![
                rewritten.observed_at().as_str(),
                rewritten.canonical_digest().as_str(),
                target.evidence_id().as_str()
            ],
        )
        .expect("rewrite Evidence");
    drop(connection);
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    assert_eq!(
        state
            .get_evidence(target.evidence_id())
            .expect("the rewritten Evidence recomputes"),
        rewritten
    );
    for item in &bundle.evidence {
        match state.completion_evidence(item.evidence_id()) {
            Err(StoreError::PersistedDataIntegrity(reason)) => {
                assert!(reason.contains("disagrees with the ledger"), "{reason}");
                assert!(reason.contains(target.evidence_id().as_str()), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }
}

/// The latest Checkpoint as the Recovery Preview reads it.
fn preview_checkpoint(fixture: &UpgradeFixture) -> memprove_domain::CheckpointView {
    StateStore::open(fixture.root.path(), &fixture.project)
        .expect("reopen")
        .task_ledger_view(&fixture.task_id)
        .expect("ledger view")
        .checkpoint
}

/// P2-E (T9 S3): a record that recomputes must also agree with what
/// `memproved`'s own Events recorded. An Evidence row rewritten with
/// self-consistent digests, and a Checkpoint whose
/// `project_event_sequence` (outside its digest) was edited, are each
/// located by `verifyLedger` and read as damaged by the Preview.
#[test]
#[allow(clippy::too_many_lines)] // Two tamperings, each through verifyLedger and the Preview.
fn verify_ledger_and_the_preview_compare_records_with_their_events() {
    // Evidence rewritten with a recomputed canonical digest; the
    // checkpoint.created Event still names the old one.
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("commit bundle");
    assert_only_the_pre_0007_task_fails(&state.verify_ledger().expect("verify"), &fixture);
    drop(state);
    assert!(matches!(
        preview_checkpoint(&fixture),
        memprove_domain::CheckpointView::Intact { .. }
    ));
    let target = bundle.evidence.last().expect("Evidence");
    let mut input = target.to_input();
    input.observed_at = timestamp("2026-01-01T00:00:00Z");
    let rewritten = Evidence::new(input).expect("self-consistent Evidence");
    assert_ne!(rewritten.canonical_digest(), target.canonical_digest());
    // The Checkpoint digest covers the Evidence digests it cites; it is
    // recomputed too, so every record recomputes and only the Events differ.
    let evidence = bundle
        .evidence
        .iter()
        .map(|item| {
            if item.evidence_id() == target.evidence_id() {
                rewritten.clone()
            } else {
                item.clone()
            }
        })
        .collect::<Vec<_>>();
    let checkpoint = Checkpoint::new(bundle.checkpoint.to_input(), &bundle.snapshot, &evidence)
        .expect("self-consistent Checkpoint");
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch(
            "DROP TRIGGER evidence_reject_update;
             DROP TRIGGER checkpoints_reject_update;",
        )
        .expect("drop triggers");
    connection
        .execute(
            "UPDATE checkpoints SET digest = ?1 WHERE checkpoint_id = ?2",
            params![
                checkpoint.digest().as_str(),
                checkpoint.checkpoint_id().as_str()
            ],
        )
        .expect("rewrite Checkpoint digest");
    connection
        .execute(
            "UPDATE evidence SET observed_at = ?1, canonical_digest = ?2 WHERE evidence_id = ?3",
            params![
                rewritten.observed_at().as_str(),
                rewritten.canonical_digest().as_str(),
                target.evidence_id().as_str()
            ],
        )
        .expect("rewrite Evidence");
    drop(connection);
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let verification = state.verify_ledger().expect("verify rewritten Evidence");
    let failure = verification.first_failure.expect("located");
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Evidence
    );
    assert_eq!(failure.record_id, target.evidence_id().as_str());
    assert!(
        failure.reason.contains("Project sequence"),
        "{}",
        failure.reason
    );
    drop(state);
    match preview_checkpoint(&fixture) {
        memprove_domain::CheckpointView::Damaged {
            missing_object,
            invalid_evidence_ids,
            ..
        } => {
            assert!(!missing_object);
            assert_eq!(invalid_evidence_ids, [target.evidence_id().clone()]);
        }
        other => panic!("{other:?}"),
    }

    // A Checkpoint's project_event_sequence edited: it no longer precedes
    // its checkpoint.created Event.
    let fixture = upgrade_fixture();
    let bundle = materialized_bundle(&fixture);
    let mut state = open_state(fixture.root.path(), &fixture).expect("state");
    commit_bundle(&mut state, &bundle).expect("commit bundle");
    drop(state);
    let connection = direct_connection(&fixture.project);
    connection
        .execute_batch(
            "DROP TRIGGER checkpoints_reject_update;
             UPDATE checkpoints SET project_event_sequence = 999;",
        )
        .expect("edit project_event_sequence");
    drop(connection);
    let state = StateStore::open(fixture.root.path(), &fixture.project).expect("reopen");
    let verification = state.verify_ledger().expect("verify edited Checkpoint");
    let failure = verification.first_failure.expect("located");
    assert_eq!(
        failure.record_type,
        memprove_domain::LedgerRecordType::Checkpoint
    );
    assert_eq!(
        failure.record_id,
        bundle.checkpoint.checkpoint_id().as_str()
    );
    assert!(
        failure.reason.contains("project_event_sequence 999"),
        "{}",
        failure.reason
    );
    drop(state);
    assert!(matches!(
        preview_checkpoint(&fixture),
        memprove_domain::CheckpointView::Damaged { .. }
    ));
}
