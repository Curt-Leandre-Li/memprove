//! Agent adapters: how `claude -p`, `codex exec` and any other program are
//! run on one task in its worktree, and what they reported.
//!
//! OWNER: workstream W2. This file is a stub with the agreed interface.

use std::{path::Path, time::Duration};

use crate::{AgentOutcome, AgentSpec, EvalError};

/// One agent run.
#[derive(Debug, Clone, Copy)]
pub struct AgentRequest<'a> {
    /// The task's worktree; the agent's working directory.
    pub workdir: &'a Path,
    /// The task's prompt.
    pub prompt: &'a str,
    /// The task's time limit for the agent.
    pub timeout: Duration,
    /// The trial's directory: the adapter writes `agent.out` and
    /// `agent.err` (and may write more files) here.
    pub log_dir: &'a Path,
}

/// An agent that can run tasks; one value runs every trial of a run,
/// possibly from several threads.
pub trait Agent: Sync {
    /// What the agent says its version is, for `run.json`.
    fn version(&self) -> Option<String>;

    /// Runs the agent once and reports how it ended and what it used. It
    /// never judges the work.
    fn run(&self, request: &AgentRequest<'_>) -> AgentOutcome;
}

/// The adapter for `spec`.
///
/// # Errors
/// [`EvalError::Usage`] when `spec` cannot be run (a `command` agent
/// without a command).
pub fn agent_for(spec: &AgentSpec) -> Result<Box<dyn Agent>, EvalError> {
    let _ = spec;
    Err(EvalError::Unimplemented("agent adapters (W2)"))
}
