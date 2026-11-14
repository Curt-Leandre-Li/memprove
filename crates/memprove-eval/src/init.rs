//! `memprove eval init`: the self-serve setup. It writes what a repository
//! needs to start evaluating (an `evals/` directory with a guide addressed
//! to the coding agent and a first task, a CI workflow, a short note in
//! `AGENTS.md` / `CLAUDE.md`), so that the developer's own agent does the
//! rest.
//!
//! OWNER: workstream W4. This file is a stub with the agreed interface.

use std::path::{Path, PathBuf};

use crate::{AgentKind, EvalError};

/// What `init` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitSpec {
    /// The worktree root.
    pub repo_root: PathBuf,
    /// The agent the workflow and the first task are written for.
    pub agent: AgentKind,
    /// Also write `.github/workflows/memprove-eval.yml`.
    pub workflow: bool,
    /// Print what would be written; write nothing.
    pub dry_run: bool,
}

/// What `init` did, for its output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitReport {
    /// Files written or changed, relative to the root.
    pub written: Vec<PathBuf>,
    /// Files that already said the same, relative to the root.
    pub unchanged: Vec<PathBuf>,
    /// Sentences for the person (for example the secret the workflow
    /// needs).
    pub notes: Vec<String>,
}

/// Sets up `spec.repo_root` for `memprove eval`. Running it again changes
/// nothing.
///
/// # Errors
/// When a file cannot be read or written.
pub fn init(spec: &InitSpec) -> Result<InitReport, EvalError> {
    let _ = spec;
    Err(EvalError::Unimplemented("eval init (W4)"))
}

/// The repository's test command, guessed from its files (`Cargo.toml`,
/// `package.json`, `pyproject.toml`, `go.mod`, ...); `None` when there is
/// no guess.
#[must_use]
pub fn detect_check(repo_root: &Path) -> Option<Vec<String>> {
    let _ = repo_root;
    None
}
