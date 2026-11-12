//! Versioned wire-contract types and dependency-free contract validation.
//!
//! The frozen `OpenAPI` document and repository `JSON` Schemas remain the wire
//! authorities. This crate supplies the Phase 1 bindings that compile against
//! those contracts without introducing a production dependency.

#![forbid(unsafe_code)]

/// Human-readable boundary description used by workspace smoke tests.
pub const CRATE_PURPOSE: &str = "versioned API, event, MCP, and canonical wire contracts";

pub mod phase1;
pub mod strict_json;
