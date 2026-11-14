//! Task files (`memprove.eval.task.v1`): reading and checking them.
//!
//! OWNER: workstream W1.

use std::path::{Path, PathBuf};

use crate::{EvalError, Task, TaskRef};

/// Reads one task file; `file` is absolute or relative to `repo_root`.
///
/// # Errors
/// [`EvalError::Io`] or [`EvalError::Invalid`].
pub fn load_task(repo_root: &Path, file: &Path) -> Result<(Task, TaskRef), EvalError> {
    let _ = (repo_root, file);
    Err(EvalError::Unimplemented("task files (W1)"))
}

/// Reads the tasks named by `paths`: files as they are, directories as
/// their `*.json` files other than `baseline.json`, sorted by name.
/// Duplicate ids are refused.
///
/// # Errors
/// As [`load_task`], and [`EvalError::Invalid`] for a duplicate id.
pub fn load_suite(repo_root: &Path, paths: &[PathBuf]) -> Result<Vec<(Task, TaskRef)>, EvalError> {
    let _ = (repo_root, paths);
    Err(EvalError::Unimplemented("task files (W1)"))
}
