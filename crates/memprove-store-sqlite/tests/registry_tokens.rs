//! Registry client-token persistence: migration chain, bootstrap lifecycle,
//! last-control-token protection, idempotency, and audit immutability.

use memprove_domain::Sha256Digest;
use memprove_store_sqlite::{
    Idempotent, NewClientToken, RegistryStore, StoreError, TokenCommand, TokenState,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

const REGISTRY_0001: &str = include_str!("../migrations/registry/0001_registry.sql");
const REGISTRY_0002: &str = include_str!("../migrations/registry/0002_client_tokens.sql");
const NOW: &str = "2026-10-02T00:00:00Z";

fn verifier(seed: &str) -> String {
    Sha256Digest::of_bytes(seed.as_bytes()).as_str().to_owned()
}

fn new_token<'a>(
    token_id: &'a str,
    scopes: &'a [&'a str],
    verifier_sha256: &'a str,
) -> NewClientToken<'a> {
    NewClientToken {
        token_id,
        client_label: "label",
        scopes,
        verifier_sha256,
        created_at: NOW,
    }
}

fn token_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn bootstrap(store: &mut RegistryStore, seed: &str) -> String {
    let id = token_id();
    let verifier = verifier(seed);
    store
        .begin_bootstrap_token(&new_token(&id, &["control"], &verifier))
        .expect("begin bootstrap");
    store.activate_bootstrap_token(&id, NOW).expect("activate");
    id
}

fn command<'a>(principal: &'a str, key: &'a str, digest: &'a str) -> TokenCommand<'a> {
    TokenCommand {
        principal_token_id: principal,
        idempotency_key: key,
        request_sha256: digest,
        now: NOW,
    }
}

fn migrations(directory: &TempDir) -> Vec<(u64, String)> {
    let connection = Connection::open(directory.path().join("registry.sqlite")).unwrap();
    connection
        .prepare("SELECT migration_id, migration_checksum FROM schema_migrations ORDER BY 1")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn a_version_1_registry_is_upgraded_in_place() {
    let directory = TempDir::new().unwrap();
    {
        let connection = Connection::open(directory.path().join("registry.sqlite")).unwrap();
        connection.execute_batch(REGISTRY_0001).unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations VALUES (1, ?1, ?2)",
                params![
                    Sha256Digest::of_bytes(REGISTRY_0001.as_bytes()).as_str(),
                    NOW
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO projects VALUES
                 ('018f47a0-0000-7000-8000-00000000000a', '/r/a', 'sha1', '/s/a', 'ACTIVE', ?1, ?1, 1),
                 ('018f47a0-0000-7000-8000-00000000000b', '/r/b', 'sha1', '/s/b', 'PROVISIONING', ?1, ?1, 1),
                 ('018f47a0-0000-7000-8000-00000000000c', '/r/c', 'sha1', '/s/c', 'FAILED', ?1, ?1, 1)",
                [NOW],
            )
            .unwrap();
    }
    let store = RegistryStore::open(directory.path()).expect("upgrade");
    assert_eq!(store.active_project_count().unwrap(), 1);
    drop(store);
    assert_eq!(
        migrations(&directory),
        vec![
            (
                1,
                Sha256Digest::of_bytes(REGISTRY_0001.as_bytes()).to_string()
            ),
            (
                2,
                Sha256Digest::of_bytes(REGISTRY_0002.as_bytes()).to_string()
            ),
        ]
    );
    RegistryStore::open(directory.path()).expect("reopen is a no-op");
    assert_eq!(migrations(&directory).len(), 2);
}

#[test]
fn drifted_or_unknown_migrations_fail_closed() {
    for tamper in [
        "UPDATE schema_migrations SET migration_checksum = 'broken' WHERE migration_id = 2",
        "INSERT INTO schema_migrations VALUES (3, 'future', '2026-10-02T00:00:00Z')",
        "DELETE FROM schema_migrations WHERE migration_id = 1",
    ] {
        let directory = TempDir::new().unwrap();
        drop(RegistryStore::open(directory.path()).unwrap());
        Connection::open(directory.path().join("registry.sqlite"))
            .unwrap()
            .execute(tamper, [])
            .unwrap();
        assert!(
            matches!(
                RegistryStore::open(directory.path()),
                Err(StoreError::MigrationDrift { .. })
            ),
            "{tamper}"
        );
    }
}

#[test]
fn bootstrap_is_two_phase_and_only_issues_the_first_control_token() {
    let directory = TempDir::new().unwrap();
    let mut store = RegistryStore::open(directory.path()).unwrap();

    let abandoned = token_id();
    let abandoned_verifier = verifier("abandoned");
    assert_eq!(
        store
            .begin_bootstrap_token(&new_token(&abandoned, &["control"], &abandoned_verifier))
            .unwrap(),
        0
    );
    assert_eq!(store.authenticate_token(&abandoned_verifier).unwrap(), None);

    let delivered = token_id();
    let delivered_verifier = verifier("delivered");
    assert_eq!(
        store
            .begin_bootstrap_token(&new_token(&delivered, &["control"], &delivered_verifier))
            .unwrap(),
        1,
        "the stale pending token is discarded"
    );
    assert_eq!(store.authenticate_token(&delivered_verifier).unwrap(), None);
    store.activate_bootstrap_token(&delivered, NOW).unwrap();
    let principal = store
        .authenticate_token(&delivered_verifier)
        .unwrap()
        .unwrap();
    assert_eq!(principal.token_id, delivered);
    assert_eq!(principal.scopes, vec!["control"]);
    assert_eq!(principal.state, TokenState::Active);

    let refused = token_id();
    let refused_verifier = verifier("refused");
    assert!(matches!(
        store.begin_bootstrap_token(&new_token(&refused, &["control"], &refused_verifier)),
        Err(StoreError::ControlTokenExists)
    ));
    assert!(matches!(
        store.activate_bootstrap_token(&abandoned, NOW),
        Err(StoreError::NotFound(_))
    ));
    let page = store.list_client_tokens(None, 10).unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "pending and discarded tokens are not listed"
    );
    assert!(!page.has_more);
}

#[test]
fn bootstrap_discard_leaves_a_non_authenticating_token() {
    let directory = TempDir::new().unwrap();
    let mut store = RegistryStore::open(directory.path()).unwrap();
    let id = token_id();
    let verifier = verifier("lost");
    store
        .begin_bootstrap_token(&new_token(&id, &["control"], &verifier))
        .unwrap();
    store.discard_bootstrap_token(&id, NOW).unwrap();
    assert_eq!(store.authenticate_token(&verifier).unwrap(), None);
    assert!(matches!(
        store.begin_bootstrap_token(&new_token(&id, &["mcp-read"], &verifier)),
        Err(StoreError::ConstraintViolation(_))
    ));
}

#[test]
fn revocation_protects_the_last_control_token_atomically() {
    let directory = TempDir::new().unwrap();
    let mut store = RegistryStore::open(directory.path()).unwrap();
    let control = bootstrap(&mut store, "control");

    assert!(matches!(
        store.revoke_client_token(&command(&control, "key-0000000000001", "d1"), &control),
        Err(StoreError::LastControlToken)
    ));

    let second = token_id();
    let second_verifier = verifier("second");
    let created = store
        .create_client_token(
            &command(&control, "key-0000000000002", "d2"),
            &new_token(&second, &["control", "mcp-read"], &second_verifier),
        )
        .unwrap();
    assert!(matches!(created, Idempotent::Created(_)));

    let revoked = store
        .revoke_client_token(&command(&second, "key-0000000000003", "d3"), &control)
        .unwrap();
    let Idempotent::Created(revoked) = revoked else {
        panic!("expected a new revocation");
    };
    assert_eq!(revoked.state, TokenState::Revoked);
    assert_eq!(revoked.revoked_at.as_deref(), Some(NOW));
    assert_eq!(
        store.authenticate_token(&verifier("control")).unwrap(),
        None
    );
    assert!(matches!(
        store.revoke_client_token(&command(&second, "key-0000000000004", "d4"), &second),
        Err(StoreError::LastControlToken)
    ));
}

#[test]
fn idempotency_records_replay_and_conflict() {
    let directory = TempDir::new().unwrap();
    let mut store = RegistryStore::open(directory.path()).unwrap();
    let control = bootstrap(&mut store, "control");
    let first_id = token_id();
    let first_verifier = verifier("first");
    let key = "key-0000000000001";

    let first = store
        .create_client_token(
            &command(&control, key, "digest-a"),
            &new_token(&first_id, &["mcp-read"], &first_verifier),
        )
        .unwrap();
    assert!(matches!(first, Idempotent::Created(_)));

    let other_id = token_id();
    let other_verifier = verifier("other");
    let replay = store
        .create_client_token(
            &command(&control, key, "digest-a"),
            &new_token(&other_id, &["mcp-read"], &other_verifier),
        )
        .unwrap();
    let Idempotent::Replayed(replayed) = replay else {
        panic!("expected a replay");
    };
    assert_eq!(replayed.token_id, first_id);
    assert_eq!(store.authenticate_token(&other_verifier).unwrap(), None);

    assert!(matches!(
        store.create_client_token(
            &command(&control, key, "digest-b"),
            &new_token(&other_id, &["mcp-read"], &other_verifier),
        ),
        Err(StoreError::IdempotencyConflict)
    ));
    assert!(matches!(
        store.revoke_client_token(&command(&control, "key-0000000000002", "r"), &other_id),
        Err(StoreError::NotFound(_))
    ));
}

#[test]
fn persisted_rows_hold_only_verifiers_and_audit_is_append_only() {
    let directory = TempDir::new().unwrap();
    let mut store = RegistryStore::open(directory.path()).unwrap();
    let control = bootstrap(&mut store, "control-secret-value");
    let reader = token_id();
    let reader_verifier = verifier("reader-secret-value");
    store
        .create_client_token(
            &command(&control, "key-0000000000001", "d"),
            &new_token(&reader, &["mcp-read"], &reader_verifier),
        )
        .unwrap();
    store
        .revoke_client_token(&command(&control, "key-0000000000002", "r"), &reader)
        .unwrap();
    drop(store);

    let bytes = std::fs::read(directory.path().join("registry.sqlite")).unwrap();
    for secret in ["control-secret-value", "reader-secret-value"] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "plaintext {secret} found in registry.sqlite"
        );
    }

    let connection = Connection::open(directory.path().join("registry.sqlite")).unwrap();
    let actions: Vec<String> = connection
        .prepare("SELECT action FROM token_audit_events ORDER BY audit_seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        actions,
        [
            "bootstrap_issued",
            "bootstrap_activated",
            "created",
            "revoked"
        ]
    );
    for statement in [
        "UPDATE token_audit_events SET action = 'created'",
        "DELETE FROM token_audit_events",
    ] {
        let error = connection.execute(statement, []).unwrap_err();
        assert!(error.to_string().contains("append-only"), "{error}");
    }
    let error = connection
        .execute(
            "INSERT INTO client_token_scopes (token_id, scope) VALUES (?1, 'admin')",
            [&reader],
        )
        .unwrap_err();
    assert!(error.to_string().contains("CHECK"), "{error}");
}
