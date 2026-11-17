//! `memprove verify`: is the work an agent says is done really done? A
//! contract in the repository (`.memprove/verify.json`) names the commands
//! that must exit 0 and the paths the work should not need to change (its
//! tests). A verification writes the files being verified to a new
//! directory that holds nothing else (no untracked or ignored file, no
//! build output), runs the commands there, and their exit codes alone say
//! whether they passed. When the work changed a protected path, the
//! commands run a second time with every protected path as it is at the
//! base commit: work that passes only with its own changes to the tests is
//! left to a person. The contract is read from the base commit, so the work
//! being verified cannot weaken it. Every verification writes a receipt.
//!
//! The crate depends on `memprove-eval` alone, for its isolated working
//! copies, its command runner and its JSON reader; the `memprove` CLI
//! embeds it as `memprove verify`.

#![forbid(unsafe_code)]

pub mod cli;
pub mod contract;
pub mod hook;
pub mod init;
mod markers;
pub mod receipt;
pub mod run;
pub mod snapshot;

use std::path::{Path, PathBuf};

pub use contract::{CONTRACT_PATH, CONTRACT_SCHEMA, Check, Contract};
pub use memprove_eval::EvalError as VerifyError;
pub use receipt::{CommandResult, Finding, FindingKind, RECEIPT_SCHEMA, Receipt, Stage, Verdict};
pub use run::{Options, Plan, Planned, verify};
pub use snapshot::{Base, BaseSource};

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

/// Where `memprove verify` keeps what it writes for the repository of
/// `repo_root`: `memprove-verify` in its Git common directory, so that
/// receipts and logs are never untracked files of any worktree.
///
/// # Errors
/// When `repo_root` is not in a Git worktree.
pub fn state_dir(repo_root: &Path) -> Result<PathBuf, VerifyError> {
    let common = memprove_eval::git::git(repo_root, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(common.trim());
    let common = if common.is_absolute() {
        common
    } else {
        repo_root.join(common)
    };
    Ok(common.join("memprove-verify"))
}
