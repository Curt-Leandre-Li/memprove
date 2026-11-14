//! The memory the agent is given, as memprove judges it. The crate does
//! not talk to memprove's daemon: the program that embeds it (the
//! `memprove` CLI) passes a [`MemoryProbe`], and a standalone build passes
//! none.

use std::path::Path;

use crate::MemoryNote;

/// Reads the statements memprove would give an agent that starts in
/// `repo_root` now, each with its verdict.
pub trait MemoryProbe: Sync {
    /// The statements, in the Pack's order; empty when memprove holds no
    /// Task for this repository.
    ///
    /// # Errors
    /// One sentence saying why memory could not be read (no daemon, no
    /// token); the run goes on without memory and records the sentence.
    fn notes(&self, repo_root: &Path) -> Result<Vec<MemoryNote>, String>;
}
