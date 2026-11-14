//! Reports and the CI gate: what a run did, compared with a baseline.
//!
//! OWNER: workstream W3. This file is a stub with the agreed interface.

use crate::{Baseline, RunRecord};

/// How a report is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// For people and pull-request comments.
    Markdown,
    /// For programs.
    Json,
}

/// When the gate fails a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatePolicy {
    /// The largest drop of the overall pass rate allowed, in tenths of a
    /// percentage point.
    pub max_drop_permille: u64,
    /// Fail when any task that passed every baseline trial passes none now.
    pub fail_on_regression: bool,
}

impl Default for GatePolicy {
    fn default() -> Self {
        Self {
            max_drop_permille: 0,
            fail_on_regression: true,
        }
    }
}

/// The gate's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateVerdict {
    /// Whether the run may pass.
    pub passed: bool,
    /// One sentence per reason it failed; empty when it passed.
    pub reasons: Vec<String>,
}

/// The report of `run`, compared with `baseline` when there is one.
#[must_use]
pub fn render(run: &RunRecord, baseline: Option<&Baseline>, format: Format) -> String {
    let _ = (run, baseline, format);
    todo!("W3: report::render")
}

/// The gate's verdict on `run` against `baseline`.
#[must_use]
pub fn gate(run: &RunRecord, baseline: &Baseline, policy: &GatePolicy) -> GateVerdict {
    let _ = (run, baseline, policy);
    todo!("W3: report::gate")
}
