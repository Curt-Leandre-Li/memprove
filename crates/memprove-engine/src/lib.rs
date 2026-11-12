//! Application services and infrastructure ports will live in this crate.
//!
//! The runtime health/status/version service with its authentication,
//! Project-count, and writer-queue ports, the Project/Workspace/Task/Session
//! catalog service with its store and Git ports, the Event ledger service
//! with its store port, and the Evidence/Checkpoint recovery service with its
//! capture-and-store port live here; no verification or context-assembly
//! behavior is implemented yet.

#![forbid(unsafe_code)]

/// Human-readable boundary description used by workspace smoke tests.
pub const CRATE_PURPOSE: &str = "domain orchestration and infrastructure ports";

/// Confirms the intended inward dependency without adding runtime behavior.
pub const DOMAIN_BOUNDARY: &str = memprove_domain::CRATE_PURPOSE;

pub mod catalog;
pub mod events;
pub mod recovery;
pub mod runtime;
