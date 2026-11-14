//! `memprove eval mine`: task files from the repository's own history. A
//! commit that changed tests and code becomes a task: start from its
//! parent, ask for what its message says, judge by its tests (applied as
//! the check patch, so the agent never sees them).
//!
//! OWNER: workstream W4. This file is a stub with the agreed interface.

use std::path::PathBuf;

use crate::EvalError;

/// What `mine` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MineSpec {
    /// The worktree root.
    pub repo_root: PathBuf,
    /// At most this many tasks.
    pub limit: usize,
    /// The commits to look at (`git log` syntax); `None` is the history of
    /// HEAD.
    pub range: Option<String>,
    /// Where the task files and patches go, relative to the root.
    pub out_dir: PathBuf,
    /// Print what would be written; write nothing.
    pub dry_run: bool,
}

/// One task `mine` wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinedTask {
    /// The task's id.
    pub id: String,
    /// The commit it was made from.
    pub commit: String,
    /// The task file, relative to the root.
    pub path: PathBuf,
}

/// Writes tasks mined from history.
///
/// # Errors
/// When `git` fails or a file cannot be written.
pub fn mine(spec: &MineSpec) -> Result<Vec<MinedTask>, EvalError> {
    let _ = spec;
    Err(EvalError::Unimplemented("eval mine (W4)"))
}
