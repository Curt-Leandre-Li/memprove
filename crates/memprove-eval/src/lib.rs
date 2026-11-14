//! `memprove eval`: does an agent setup still do this repository's real
//! work? A task names a starting commit, a prompt and an acceptance
//! command; a run gives each task to a coding agent (Claude Code, Codex,
//! or any program) in its own Git worktree, and the acceptance command's
//! exit code alone says whether it passed. Runs are compared with a
//! baseline, gated in CI, and reported with the memory the agent was given
//! that memprove no longer holds as verified.
//!
//! The crate depends on no other memprove crate but the strict JSON
//! parser, so that it can become a product of its own; the `memprove` CLI
//! embeds it as `memprove eval` and gives it a [`MemoryProbe`].

#![forbid(unsafe_code)]

pub mod agent;
pub mod baseline;
pub mod cli;
mod error;
pub mod git;
pub mod init;
pub mod json;
mod memory;
pub mod mine;
mod model;
pub mod process;
pub mod report;
pub mod runner;
pub mod task;
pub mod time;
pub mod workspace;

/// `bytes` as lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            write!(text, "{byte:02x}").expect("writing to a String cannot fail");
            text
        })
}

pub use agent::{Agent, AgentRequest, agent_for};
pub use baseline::{Baseline, BaselineTask};
pub use error::EvalError;
pub use memory::MemoryProbe;
pub use model::{
    AgentEnd, AgentKind, AgentOutcome, AgentSpec, BASELINE_SCHEMA, CommandSpec, DiffStat,
    MemoryNote, Outcome, RUN_SCHEMA, RepositoryInfo, RunRecord, Summary, TASK_SCHEMA, Task,
    TaskRef, TrialResult, Usage,
};
