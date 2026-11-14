//! Running a suite: every trial of every task, in isolated worktrees, the
//! agent then the check, and `run.json` in the run directory.
//!
//! OWNER: workstream W1.

use std::path::PathBuf;

use crate::{Agent, AgentSpec, EvalError, MemoryProbe, RunRecord, Task, TaskRef};

/// How a suite is run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    /// The worktree root the run starts from.
    pub repo_root: PathBuf,
    /// The agent.
    pub agent: AgentSpec,
    /// Trials of each task, at least 1.
    pub trials: u32,
    /// Trials run at the same time, at least 1.
    pub jobs: u32,
    /// Where run directories go; each run gets `<out_root>/<run_id>/`.
    pub out_root: PathBuf,
    /// Keep each trial's worktree instead of removing it.
    pub keep_worktrees: bool,
}

/// Runs `tasks` with `agent` and writes `<out_root>/<run_id>/run.json`.
///
/// # Errors
/// When the run directory cannot be written or the repository cannot be
/// read; a failing trial is a result, not an error.
pub fn run_suite(
    options: &RunOptions,
    tasks: &[(Task, TaskRef)],
    agent: &dyn Agent,
    memory: Option<&dyn MemoryProbe>,
) -> Result<(RunRecord, PathBuf), EvalError> {
    let _ = (options, tasks, agent, memory);
    Err(EvalError::Unimplemented("the runner (W1)"))
}

/// A line in the format of libtest's `test result:` line, counting one
/// case per trial, so that `memprove run --test-format cargo -- memprove
/// eval run ...` records the run as test Evidence.
#[must_use]
pub fn libtest_summary(run: &RunRecord) -> String {
    let summary = run.summary();
    let failed = summary.trials - summary.passed;
    let verdict = if failed == 0 { "ok" } else { "FAILED" };
    let seconds = (summary.agent_ms + summary.check_ms) / 1000;
    format!(
        "test result: {verdict}. {} passed; {failed} failed; 0 ignored; 0 measured; 0 filtered out; finished in {seconds}.00s",
        summary.passed
    )
}
