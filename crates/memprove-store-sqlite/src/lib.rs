//! The `SQLite` `StateStore`, `LedgerStore`, and Blob metadata adapters will live
//! in this crate.
//!
//! Phase 0 contains no database dependency, migration, or persistence behavior.

#![forbid(unsafe_code)]

mod error;
mod provisioning;
mod registry;
mod search;
mod search_v2;
mod state;
mod vfs;

pub use error::{OverruledCitation, StoreError};
pub use provisioning::ProjectProvisioner;
pub use registry::{
    NewClientToken, RegistryStore, StoredToken, TokenCommand, TokenPage, TokenState,
    rebind_restored_state_paths,
};
pub use search::{SearchError, sqlite_version};
pub use search_v2::{SearchEntry, rank_statements_v2};
pub use state::{
    CheckpointDraft, DecisionDraft, DecisionRow, EvidenceCapture, EvidenceRecord, StatePage,
    StateStore,
};

/// Human-readable boundary description used by workspace smoke tests.
pub const CRATE_PURPOSE: &str = "SQLite ledger, projection, outbox, and backup adapters";

/// Confirms the adapter points toward the engine port boundary.
pub const ENGINE_BOUNDARY: &str = memprove_engine::CRATE_PURPOSE;

/// Confirms persisted objects use the shared domain contract.
pub const DOMAIN_BOUNDARY: &str = memprove_domain::CRATE_PURPOSE;

/// Outcome of an idempotent create operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Idempotent<T> {
    Created(T),
    Replayed(T),
}

use error::map_sqlite_error;
