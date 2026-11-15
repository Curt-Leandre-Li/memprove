//! What memprove holds for the repository when a run starts, as memprove
//! judges it. The crate does not talk to memprove's daemon: the program
//! that embeds it (the `memprove` CLI) passes a [`MemoryProbe`], and a
//! standalone build passes none. The statements are always recorded in
//! `run.json`; the Pack is given to the agent only with `--memory pack`
//! (see [`crate::runner::RunOptions::give_pack`]), since a trial worktree
//! has no memprove context of its own.

use std::path::Path;

use crate::MemoryNote;

/// What memprove would give an agent that starts in a repository now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    /// The Pack's statements, in its order, each with its verdict.
    pub notes: Vec<MemoryNote>,
    /// The Context Pack's text, as the `SessionStart` hook would inject it
    /// into a new session (without the hook's opening sentence).
    pub pack: String,
}

/// Reads what memprove would give an agent that starts in `repo_root` now.
pub trait MemoryProbe: Sync {
    /// The statements and the Pack, read without writing anything.
    ///
    /// # Errors
    /// One sentence saying why they could not be read (the repository has
    /// no memprove context, no Task is in progress, no daemon, no token);
    /// a run without `--memory pack` goes on without them and records the
    /// sentence as `memory_unread`.
    fn read(&self, repo_root: &Path) -> Result<Memory, String>;
}
