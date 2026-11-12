use std::path::{Path, PathBuf};

use memprove_domain::{
    IdempotencyContext, Project, ProjectId, ProjectProvisioningState, Sha256Digest, UtcTimestamp,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{Idempotent, StoreError, map_sqlite_error, vfs::CapabilityRoot};

mod tokens;

pub use tokens::{NewClientToken, StoredToken, TokenCommand, TokenPage, TokenState};

struct RegistryMigration {
    id: u64,
    sql: &'static str,
}

/// Ordered, append-only registry migration chain.
const REGISTRY_MIGRATIONS: [RegistryMigration; 2] = [
    RegistryMigration {
        id: 1,
        sql: include_str!("../migrations/registry/0001_registry.sql"),
    },
    RegistryMigration {
        id: 2,
        sql: include_str!("../migrations/registry/0002_client_tokens.sql"),
    },
];

/// Global project registry and provisioning lifecycle store.
pub struct RegistryStore {
    connection: Connection,
    database_path: PathBuf,
    capability_root: CapabilityRoot,
}

impl RegistryStore {
    /// Opens `<data_dir>/registry.sqlite` and applies the registry-only migration.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for directory, database, migration, or pragma failures.
    pub fn open(data_dir: &Path) -> Result<Self, StoreError> {
        let capability_root = CapabilityRoot::open(data_dir)?;
        Self::open_with_root(capability_root)
    }

    pub(crate) fn open_with_root(capability_root: CapabilityRoot) -> Result<Self, StoreError> {
        capability_root.verify_identity()?;
        let database_path = capability_root.absolute_path("registry.sqlite")?;
        let existed = capability_root.exists("registry.sqlite")?;
        let opened = (|| {
            let mut connection = capability_root.open_database("registry.sqlite")?;
            configure(&connection)?;
            migrate(&mut connection)?;
            capability_root.verify_identity()?;
            Ok(connection)
        })();
        match opened {
            Ok(connection) => Ok(Self {
                connection,
                database_path,
                capability_root,
            }),
            Err(error) => {
                if !existed {
                    let _ = capability_root.cleanup_new_database("registry.sqlite");
                }
                Err(error)
            }
        }
    }

    /// Returns the exact registry database path.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Atomically records Step A of project provisioning.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for conflicts, constraints, or transaction failures.
    pub fn register_project_begin(
        &mut self,
        proposed: &Project,
        context: &IdempotencyContext,
        request_digest: &Sha256Digest,
    ) -> Result<Idempotent<Project>, StoreError> {
        self.capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(existing_digest) = transaction
            .query_row(
                "SELECT canonical_request_sha256
                 FROM registry_idempotency_records
                 WHERE principal_id = ?1 AND operation_id = ?2 AND idempotency_key = ?3",
                params![
                    context.principal_id.as_str(),
                    context.operation_id.as_str(),
                    context.key.as_str()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if existing_digest != request_digest.as_str() {
                return Err(StoreError::IdempotencyConflict);
            }
            let existing = find_matching_project(&transaction, proposed)?;
            self.capability_root.verify_identity()?;
            transaction.commit()?;
            return Ok(Idempotent::Replayed(existing));
        }

        let existing = find_project_by_identity(&transaction, proposed)?;
        let outcome = if let Some(project) = existing {
            Idempotent::Replayed(project)
        } else {
            transaction
                .execute(
                    "INSERT INTO projects (
                        project_id, repository_common_dir, object_format, state_db_path,
                        provisioning_state, created_at, updated_at, revision
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        proposed.project_id.as_str(),
                        proposed.repository_common_dir,
                        proposed.object_format,
                        proposed.state_db_path,
                        proposed.provisioning_state_internal.as_str(),
                        proposed.created_at.as_str(),
                        proposed.updated_at.as_str(),
                        proposed.revision
                    ],
                )
                .map_err(map_sqlite_error)?;
            Idempotent::Created(proposed.clone())
        };
        let project = match &outcome {
            Idempotent::Created(project) | Idempotent::Replayed(project) => project,
        };
        let (idempotency_state, result_record_type, result_record_id) =
            match project.provisioning_state_internal {
                ProjectProvisioningState::Provisioning => ("IN_PROGRESS", None, None),
                ProjectProvisioningState::Active => (
                    "SUCCEEDED",
                    Some("Project"),
                    Some(project.project_id.as_str()),
                ),
                ProjectProvisioningState::Failed => ("FAILED", None, None),
            };
        transaction
            .execute(
                "INSERT INTO registry_idempotency_records (
                    principal_id, operation_id, idempotency_key,
                    canonical_request_sha256, state, provisioning_record_id,
                    result_record_type, result_record_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                params![
                    context.principal_id.as_str(),
                    context.operation_id.as_str(),
                    context.key.as_str(),
                    request_digest.as_str(),
                    idempotency_state,
                    project.project_id.as_str(),
                    result_record_type,
                    result_record_id,
                    proposed.created_at.as_str()
                ],
            )
            .map_err(map_sqlite_error)?;
        self.capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(outcome)
    }

    pub(crate) fn idempotency_state(
        &self,
        context: &IdempotencyContext,
    ) -> Result<String, StoreError> {
        self.connection
            .query_row(
                "SELECT state FROM registry_idempotency_records
                 WHERE principal_id = ?1 AND operation_id = ?2 AND idempotency_key = ?3",
                params![
                    context.principal_id.as_str(),
                    context.operation_id.as_str(),
                    context.key.as_str()
                ],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("registry idempotency record"))
    }

    /// Reads one project by stable ID.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the project is absent or cannot be decoded.
    pub fn get_project(&self, project_id: &ProjectId) -> Result<Project, StoreError> {
        self.connection
            .query_row(
                "SELECT project_id, repository_common_dir, object_format, state_db_path,
                        provisioning_state, created_at, updated_at, revision
                 FROM projects WHERE project_id = ?1",
                [project_id.as_str()],
                project_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound("Project"))
    }

    /// Finalizes Step C after the state binding has been verified.
    pub(crate) fn finalize_project(
        &mut self,
        project_id: &ProjectId,
    ) -> Result<Project, StoreError> {
        self.capability_root.verify_identity()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction
            .query_row(
                "SELECT project_id, repository_common_dir, object_format, state_db_path,
                        provisioning_state, created_at, updated_at, revision
                 FROM projects WHERE project_id = ?1",
                [project_id.as_str()],
                project_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound("Project"))?;
        match current.provisioning_state_internal {
            ProjectProvisioningState::Active => {
                self.capability_root.verify_identity()?;
                transaction.commit()?;
                return Ok(current);
            }
            ProjectProvisioningState::Failed => {
                return Err(StoreError::ProvisioningFailed(
                    "registry Project is already FAILED".to_owned(),
                ));
            }
            ProjectProvisioningState::Provisioning => {}
        }
        let now = UtcTimestamp::now();
        transaction.execute(
            "UPDATE projects
             SET provisioning_state = 'ACTIVE', updated_at = ?2, revision = revision + 1
             WHERE project_id = ?1 AND provisioning_state = 'PROVISIONING'",
            params![project_id.as_str(), now.as_str()],
        )?;
        transaction.execute(
            "UPDATE registry_idempotency_records
             SET state = 'SUCCEEDED', result_record_type = 'Project',
                 result_record_id = ?1, updated_at = ?2
             WHERE provisioning_record_id = ?1 AND state = 'IN_PROGRESS'",
            params![project_id.as_str(), now.as_str()],
        )?;
        let active = transaction.query_row(
            "SELECT project_id, repository_common_dir, object_format, state_db_path,
                    provisioning_state, created_at, updated_at, revision
             FROM projects WHERE project_id = ?1",
            [project_id.as_str()],
            project_from_row,
        )?;
        self.capability_root.verify_identity()?;
        transaction.commit()?;
        Ok(active)
    }

    pub(crate) fn rollback_project_creation(
        &mut self,
        project_id: &ProjectId,
    ) -> Result<(), StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM registry_idempotency_records
             WHERE provisioning_record_id = ?1 AND state = 'IN_PROGRESS'",
            [project_id.as_str()],
        )?;
        let removed = transaction.execute(
            "DELETE FROM projects
             WHERE project_id = ?1 AND provisioning_state IN ('PROVISIONING', 'ACTIVE')",
            [project_id.as_str()],
        )?;
        if removed != 1 {
            return Err(StoreError::ConstraintViolation(
                "cannot roll back a Project outside its creation saga".to_owned(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    /// Counts Projects whose provisioning finished (`ACTIVE`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be read.
    pub fn active_project_count(&self) -> Result<u64, StoreError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE provisioning_state = 'ACTIVE'",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    /// Lists Projects whose provisioning finished (`ACTIVE`), by `project_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be read or decoded.
    pub fn list_active_projects(&self) -> Result<Vec<Project>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT project_id, repository_common_dir, object_format, state_db_path,
                    provisioning_state, created_at, updated_at, revision
             FROM projects WHERE provisioning_state = 'ACTIVE' ORDER BY project_id",
        )?;
        statement
            .query_map([], project_from_row)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// U1 B5: the identities of the `ACTIVE` Projects, by `project_id`,
    /// without decoding their records (text that is not UTF-8 reads
    /// lossily), so a caller can read each record on its own.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be read.
    pub fn list_active_project_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT project_id FROM projects WHERE provisioning_state = 'ACTIVE'
             ORDER BY project_id",
        )?;
        statement
            .query_map([], |row| match row.get_ref(0)? {
                rusqlite::types::ValueRef::Text(bytes) => {
                    Ok(String::from_utf8_lossy(bytes).into_owned())
                }
                _ => row.get(0),
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn project_count(&self) -> Result<u64, StoreError> {
        self.connection
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .map_err(StoreError::from)
    }

    pub(crate) fn idempotency_state_for_project(
        &self,
        project_id: &ProjectId,
    ) -> Result<String, StoreError> {
        self.connection
            .query_row(
                "SELECT state FROM registry_idempotency_records
                 WHERE provisioning_record_id = ?1 ORDER BY created_at LIMIT 1",
                [project_id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::NotFound("registry idempotency record"))
    }
}

/// U1 U-F (SF-2, Owner ruling 2026-10-14): binds the registry of a data
/// directory restored from a backup of `source` to `data_dir`, its new
/// place. Each `projects` row whose `state_db_path` is exactly
/// `<source>/projects/<project_id>/state.sqlite` is set to the path the
/// state database is opened at below `data_dir` (the check of
/// `StateStore`'s binding); no other column or row changes, and nothing
/// else is migrated or written. A row that names another path was not
/// bound at `source` either and is left as it is. Run offline, with the
/// instance lock of `data_dir` held, on an absolute canonical `data_dir`.
/// Returns how many rows were rewritten.
///
/// # Errors
///
/// [`StoreError`] when the registry cannot be opened or written, a
/// Project ID is not a valid path component, or a rewritten path is
/// already recorded for another Project.
pub fn rebind_restored_state_paths(data_dir: &Path, source: &Path) -> Result<usize, StoreError> {
    let capability_root = CapabilityRoot::open(data_dir)?;
    let mut connection = capability_root.open_existing_database("registry.sqlite")?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let rows = {
        let mut statement = transaction
            .prepare("SELECT project_id, state_db_path FROM projects ORDER BY project_id")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut rebound = 0;
    for (project_id, recorded) in rows {
        let relative = format!("projects/{project_id}/state.sqlite");
        if Path::new(&recorded) != source.join(&relative) {
            continue;
        }
        let bound = capability_root.absolute_path(&relative)?;
        if Path::new(&recorded) == bound {
            continue;
        }
        rebound += transaction.execute(
            "UPDATE projects SET state_db_path = ?1 WHERE project_id = ?2",
            params![bound.display().to_string(), project_id],
        )?;
    }
    capability_root.verify_identity()?;
    transaction.commit()?;
    Ok(rebound)
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let enabled: u8 = connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    if enabled != 1 {
        return Err(StoreError::InvalidPersistedValue(
            "PRAGMA foreign_keys is not enabled".to_owned(),
        ));
    }
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let has_table: bool = connection.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get(0),
    )?;
    let applied = if has_table {
        let records = connection
            .prepare(
                "SELECT migration_id, migration_checksum
                 FROM schema_migrations ORDER BY migration_id",
            )?
            .query_map([], |row| {
                Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if records.is_empty() || records.len() > REGISTRY_MIGRATIONS.len() {
            return Err(registry_migration_drift(&records));
        }
        for (record, expected) in records.iter().zip(&REGISTRY_MIGRATIONS) {
            let expected_checksum = Sha256Digest::of_bytes(expected.sql.as_bytes());
            if record.0 != expected.id || record.1 != expected_checksum.as_str() {
                return Err(registry_migration_drift(&records));
            }
        }
        records.len()
    } else {
        0
    };

    for migration in &REGISTRY_MIGRATIONS[applied..] {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations (migration_id, migration_checksum, applied_at)
             VALUES (?1, ?2, ?3)",
            params![
                migration.id,
                Sha256Digest::of_bytes(migration.sql.as_bytes()).as_str(),
                UtcTimestamp::now().as_str()
            ],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

fn registry_migration_drift(records: &[(u64, String)]) -> StoreError {
    let expected = REGISTRY_MIGRATIONS
        .iter()
        .map(|migration| {
            format!(
                "{}:{}",
                migration.id,
                Sha256Digest::of_bytes(migration.sql.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    StoreError::MigrationDrift {
        database: "registry.sqlite",
        expected,
        actual: format!("{records:?}"),
    }
}

fn find_matching_project(
    transaction: &Transaction<'_>,
    proposed: &Project,
) -> Result<Project, StoreError> {
    find_project_by_identity(transaction, proposed)?
        .ok_or(StoreError::NotFound("Project for idempotency record"))
}

fn find_project_by_identity(
    transaction: &Transaction<'_>,
    proposed: &Project,
) -> Result<Option<Project>, StoreError> {
    transaction
        .query_row(
            "SELECT project_id, repository_common_dir, object_format, state_db_path,
                    provisioning_state, created_at, updated_at, revision
             FROM projects
             WHERE project_id = ?1 OR repository_common_dir = ?2",
            params![proposed.project_id.as_str(), proposed.repository_common_dir],
            project_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let state = match row.get::<_, String>(4)?.as_str() {
        "PROVISIONING" => ProjectProvisioningState::Provisioning,
        "ACTIVE" => ProjectProvisioningState::Active,
        "FAILED" => ProjectProvisioningState::Failed,
        value => return Err(invalid_value(4, value)),
    };
    Ok(Project {
        project_id: ProjectId::parse(&row.get::<_, String>(0)?)
            .map_err(|error| invalid_value(0, error.to_string()))?,
        repository_common_dir: row.get(1)?,
        object_format: row.get(2)?,
        state_db_path: row.get(3)?,
        provisioning_state_internal: state,
        created_at: UtcTimestamp::parse(&row.get::<_, String>(5)?)
            .map_err(|error| invalid_value(5, error.to_string()))?,
        updated_at: UtcTimestamp::parse(&row.get::<_, String>(6)?)
            .map_err(|error| invalid_value(6, error.to_string()))?,
        revision: stored_revision(row)?,
    })
}

/// U1 B5 (T21 V1-N-3): a Project's revision; one below 1, which the
/// column's CHECK refuses, does not decode (before: it decoded, and
/// `getProject`'s response failed its own contract, a `500` without a body).
fn stored_revision(row: &rusqlite::Row<'_>) -> rusqlite::Result<u64> {
    let revision = row.get::<_, u64>(7)?;
    if revision == 0 {
        return Err(invalid_value(7, "revision must be at least 1"));
    }
    Ok(revision)
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
