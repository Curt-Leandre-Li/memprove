//! A baseline: the per-task pass counts of one run, kept in the repository
//! (`evals/baseline.json`) so later runs are compared with it.
//!
//! OWNER: workstream W3. This file is a stub with the agreed interface.

use std::path::Path;

use crate::{EvalError, RunRecord};

/// The pass counts of one task in the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineTask {
    /// The task's id.
    pub id: String,
    /// Trials that passed.
    pub passed: u32,
    /// Trials run.
    pub trials: u32,
}

/// A baseline file (`memprove.eval.baseline.v1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    /// The run it was taken from.
    pub run_id: String,
    /// The agent kind of that run.
    pub agent: String,
    /// Its model, when one was named.
    pub model: Option<String>,
    /// Every task of that run, in its order.
    pub tasks: Vec<BaselineTask>,
}

impl Baseline {
    /// The baseline of `run`.
    #[must_use]
    pub fn from_run(run: &RunRecord) -> Self {
        let _ = run;
        todo!("W3: Baseline::from_run")
    }

    /// The file's text.
    #[must_use]
    pub fn to_json(&self) -> String {
        todo!("W3: Baseline::to_json")
    }

    /// Reads a baseline file's text (from `path`, for messages).
    ///
    /// # Errors
    /// [`EvalError::Invalid`] naming the first member that is wrong.
    pub fn from_json(text: &str, path: Option<&Path>) -> Result<Self, EvalError> {
        let _ = (text, path);
        Err(EvalError::Unimplemented("baseline files (W3)"))
    }
}
