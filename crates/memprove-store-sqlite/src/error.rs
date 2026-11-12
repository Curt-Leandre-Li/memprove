use memprove_domain::{EvidenceId, RoleRule};
use thiserror::Error;

/// P3-D3, P3-L L1: one `verified_fact` citation of a passed run that a
/// later run overrules, for the `400` detail the daemon composes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverruledCitation {
    /// Where the citation is in the request, as
    /// `$.<section>[i].evidence_ids[j]` (or `$.next_action.evidence_ids[j]`).
    pub position: String,
    /// The cited passed run.
    pub evidence_id: EvidenceId,
    /// The later run of the same argv in the same state that did not pass.
    pub later: EvidenceId,
    /// P3-L3 (T16 S-1 item 2): whether `later` is the latest test run of a
    /// cited test run (P3-L L2), so only a passed test run restores it.
    pub by_test_run: bool,
}

/// Typed persistence failures exposed to service callers.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("SQLite failure: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("capability VFS failure: {0}")]
    CapabilityVfs(String),
    #[error("migration drift in {database}: expected {expected}, found {actual}")]
    MigrationDrift {
        database: &'static str,
        expected: String,
        actual: String,
    },
    #[error("record not found: {0}")]
    NotFound(&'static str),
    #[error("idempotency key was reused with a different request digest")]
    IdempotencyConflict,
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
    #[error("the Event's Workspace is not the Workspace of its Session")]
    SessionWorkspaceMismatch,
    #[error("the Session is not active")]
    SessionNotActive,
    #[error("the Session does not continue the Task")]
    SessionNotBoundToTask,
    #[error("recovery input rejected: {0}")]
    RecoveryInputRejected(String),
    /// U9 R1 (ADR 0020): a Session that declared the role `sub_agent`
    /// wrote what that role may not write; nothing was written.
    #[error("the Session's role may not write this: {0:?}")]
    RoleForbidden(RoleRule),
    #[error("{} verified_fact citations name overruled passed runs", .0.len())]
    OverruledCitations(Vec<OverruledCitation>),
    #[error("the record's revision is not the expected revision")]
    RevisionConflict,
    #[error("the lifecycle transition is not allowed")]
    InvalidTransition,
    #[error("the completion Evidence does not complete the Task: {0}")]
    CompletionEvidenceRejected(String),
    #[error("project is not ACTIVE")]
    ProjectNotActive,
    #[error("project provisioning failed: {0}")]
    ProvisioningFailed(String),
    #[error("state database does not match its registry project")]
    StateBindingMismatch,
    #[error("invalid persisted value: {0}")]
    InvalidPersistedValue(String),
    #[error("persisted recovery-state integrity failure: {0}")]
    PersistedDataIntegrity(String),
    #[error("the last active control token cannot be revoked")]
    LastControlToken,
    #[error("an active control token already exists")]
    ControlTokenExists,
}

impl StoreError {
    /// Whether a stored value does not decode or recompute: retrying cannot
    /// help, so a read-only recovery document reports the record as damaged
    /// instead of `503` (P4-C2, T19 SF-1).
    #[must_use]
    pub fn is_undecodable(&self) -> bool {
        match self {
            Self::PersistedDataIntegrity(_) | Self::InvalidPersistedValue(_) => true,
            Self::Database(error) => is_decode_failure(error),
            _ => false,
        }
    }
}

/// Whether a row's value is not of the type or form its column holds.
pub(crate) fn is_decode_failure(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::ToSqlConversionFailure(_)
            | rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
    )
}

pub(crate) fn map_sqlite_error(error: rusqlite::Error) -> StoreError {
    match &error {
        rusqlite::Error::SqliteFailure(details, message)
            if matches!(details.code, rusqlite::ErrorCode::ConstraintViolation) =>
        {
            StoreError::ConstraintViolation(message.clone().unwrap_or_else(|| details.to_string()))
        }
        _ => StoreError::Database(error),
    }
}
