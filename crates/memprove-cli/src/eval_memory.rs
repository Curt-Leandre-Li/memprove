//! `memprove eval`'s memory probe: the statements of the Pack memprove
//! would give an agent that starts in a repository, each with its verdict,
//! read from the daemon without writing anything.
//!
//! OWNER: workstream W3. This file is a stub with the agreed interface.

use std::path::Path;

use memprove_eval::{MemoryNote, MemoryProbe};

/// Reads memory through the daemon of the configuration in use.
#[derive(Debug, Default, Clone, Copy)]
pub struct DaemonMemory;

impl MemoryProbe for DaemonMemory {
    fn notes(&self, repo_root: &Path) -> Result<Vec<MemoryNote>, String> {
        let _ = repo_root;
        Err("reading memory is not implemented yet".to_owned())
    }
}
