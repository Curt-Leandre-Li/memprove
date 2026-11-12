//! `Git`, filesystem snapshot, hashing, path policy, and fixed-argv runner
//! adapters will live in this crate.
//!
//! Phase 0 contains no repository inspection or subprocess execution.

#![forbid(unsafe_code)]

mod git_workspace;
mod repository_snapshot;

pub use git_workspace::{
    GitWorkspaceResolver, RepoError, ResolvedGitWorkspace, ResolvedProjectRegistration,
};
pub use repository_snapshot::{
    CapturedRepositorySnapshot, RepositoryCaptureRequest, RepositoryProbe,
    RepositorySnapshotCollector, SelectedFileCapture, WorkspaceBasesObservation,
    WorkspaceObservation, observe_workspace, observe_workspace_bases,
    workspace_repository_identity,
};

/// Human-readable boundary description used by workspace smoke tests.
pub const CRATE_PURPOSE: &str = "repository probes, snapshots, path policy, and command runners";

/// Confirms the adapter points toward the engine port boundary.
pub const ENGINE_BOUNDARY: &str = memprove_engine::CRATE_PURPOSE;
