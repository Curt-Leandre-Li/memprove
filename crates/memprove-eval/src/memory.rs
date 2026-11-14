//! What memprove holds for the repository when a run starts, as memprove
//! judges it. The crate does not talk to memprove's daemon: the program
//! that embeds it (the `memprove` CLI) passes a [`MemoryProbe`], and a
//! standalone build passes none. v0 records these statements in `run.json`
//! but does not give them to the agent: a trial worktree has no memprove
//! context of its own.

use std::path::Path;

use crate::MemoryNote;

/// Reads the statements memprove would give an agent that starts in
/// `repo_root` now, each with its verdict.
pub trait MemoryProbe: Sync {
    /// The statements, in the Pack's order.
    ///
    /// # Errors
    /// One sentence saying why they could not be read (the repository has
    /// no memprove context, no Task is in progress, no daemon, no token);
    /// the run goes on without them and records the sentence as
    /// `memory_unread`.
    fn notes(&self, repo_root: &Path) -> Result<Vec<MemoryNote>, String>;
}
