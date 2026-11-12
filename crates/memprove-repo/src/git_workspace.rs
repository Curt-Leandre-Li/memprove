use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use thiserror::Error;

use memprove_domain::{
    Project, ProjectId, ProjectProvisioningState, UtcTimestamp, Workspace, WorkspaceId,
};

/// Read-only Git resolution failures.
#[derive(Debug, Error)]
pub enum RepoError {
    #[error("path is unavailable: {0}")]
    PathUnavailable(PathBuf),
    #[error("git command failed ({status:?}) for {argv:?}: {stderr}")]
    GitCommandFailed {
        argv: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
    #[error("git command returned non-UTF-8 output for {0:?}")]
    NonUtf8Output(Vec<String>),
    #[error("resolved Git directories do not share the expected common directory")]
    InvalidWorktreeRelationship,
    #[error("unsupported Git object format: {0}")]
    UnsupportedObjectFormat(String),
    #[error("HEAD object ID does not match the repository object format")]
    InvalidHeadObjectId,
    #[error("invalid explicit repo path: {0}")]
    InvalidRepoPath(String),
    #[error("selected repo path contains a symlink: {0}")]
    SymlinkNotAllowed(PathBuf),
    #[error("selected repo path is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("repository changed after capture")]
    RepositoryChanged,
    #[error("repository capture I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("repository capture domain validation failed: {0}")]
    RecoveryDomain(String),
}

/// Exact read-only identity resolved from a real Git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGitWorkspace {
    pub repository_common_dir: PathBuf,
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub head_commit: String,
    pub branch_or_detached_state: String,
    pub object_format: String,
}

/// Domain records derived from one resolved real Git root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProjectRegistration {
    pub project: Project,
    pub root_workspace: Workspace,
}

impl ResolvedGitWorkspace {
    /// Creates validated initial domain records without modifying the Git repository.
    #[must_use]
    pub fn registration_records(
        &self,
        data_dir: &Path,
        created_at: &UtcTimestamp,
    ) -> ResolvedProjectRegistration {
        let project_id = ProjectId::new();
        let state_db_path = data_dir
            .join("projects")
            .join(project_id.as_str())
            .join("state.sqlite");
        let project = Project {
            project_id: project_id.clone(),
            repository_common_dir: self.repository_common_dir.display().to_string(),
            object_format: self.object_format.clone(),
            state_db_path: state_db_path.display().to_string(),
            provisioning_state_internal: ProjectProvisioningState::Provisioning,
            created_at: created_at.clone(),
            updated_at: created_at.clone(),
            revision: 1,
        };
        let root_workspace = Workspace {
            workspace_id: WorkspaceId::new(),
            project_id,
            canonical_worktree_path: self.worktree_root.display().to_string(),
            git_dir: self.git_dir.display().to_string(),
            head_commit: self.head_commit.clone(),
            branch_or_detached_state: self.branch_or_detached_state.clone(),
            created_at: created_at.clone(),
            revision: 1,
        };
        ResolvedProjectRegistration {
            project,
            root_workspace,
        }
    }
}

/// Fixed-argv, read-only Git repository and worktree resolver.
pub struct GitWorkspaceResolver;

impl GitWorkspaceResolver {
    /// Resolves a repository root, linked worktree, symlink, or nested path.
    ///
    /// # Errors
    ///
    /// Returns [`RepoError`] when the path or fixed read-only Git probes cannot be validated.
    pub fn resolve(input: &Path) -> Result<ResolvedGitWorkspace, RepoError> {
        let canonical_input = input
            .canonicalize()
            .map_err(|_| RepoError::PathUnavailable(input.to_owned()))?;
        let worktree_root = canonical_git_path(&run_git(
            &canonical_input,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        )?)?;
        let repository_common_dir = canonical_git_path(&run_git(
            &canonical_input,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)?;
        let git_dir = canonical_git_path(&run_git(
            &canonical_input,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        )?)?;
        let head_commit = run_git(&canonical_input, &["rev-parse", "--verify", "HEAD"])?;
        let object_format = run_git(&canonical_input, &["rev-parse", "--show-object-format"])?;
        if !matches!(object_format.as_str(), "sha1" | "sha256") {
            return Err(RepoError::UnsupportedObjectFormat(object_format));
        }
        let expected_length = if object_format == "sha1" { 40 } else { 64 };
        if head_commit.len() != expected_length
            || !head_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(RepoError::InvalidHeadObjectId);
        }
        let branch = run_git_allow_status(
            &canonical_input,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )?;
        let branch_or_detached_state = if branch.status.success() {
            format!("branch:{}", utf8_stdout(&branch, &["symbolic-ref"])?)
        } else {
            "detached".to_owned()
        };
        let linked_worktree_parent = repository_common_dir.join("worktrees");
        if git_dir != repository_common_dir && !git_dir.starts_with(linked_worktree_parent) {
            return Err(RepoError::InvalidWorktreeRelationship);
        }
        if !canonical_input.starts_with(&worktree_root) {
            return Err(RepoError::InvalidWorktreeRelationship);
        }
        Ok(ResolvedGitWorkspace {
            repository_common_dir,
            worktree_root,
            git_dir,
            head_commit,
            branch_or_detached_state,
            object_format,
        })
    }
}

fn run_git(directory: &Path, arguments: &[&str]) -> Result<String, RepoError> {
    let output = run_git_allow_status(directory, arguments)?;
    if !output.status.success() {
        return Err(command_error(directory, arguments, &output));
    }
    utf8_stdout(&output, arguments)
}

fn run_git_allow_status(directory: &Path, arguments: &[&str]) -> Result<Output, RepoError> {
    hermetic_git(directory, arguments).map_err(|error| RepoError::GitCommandFailed {
        argv: argv(directory, arguments),
        status: None,
        stderr: error.to_string(),
    })
}

#[cfg(test)]
thread_local! {
    /// The Git processes this thread started (T21, V3-F-1).
    static GIT_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The number of Git processes [`hermetic_git`] started on this thread.
#[cfg(test)]
pub(crate) fn git_runs() -> usize {
    GIT_RUNS.with(std::cell::Cell::get)
}

/// Runs one fixed Git probe without loading system, user, or repository-controlled config.
pub(crate) fn hermetic_git(directory: &Path, arguments: &[&str]) -> std::io::Result<Output> {
    #[cfg(test)]
    GIT_RUNS.with(|runs| runs.set(runs.get() + 1));
    Command::new("git")
        .arg("--no-optional-locks")
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.untrackedCache=false"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "maintenance.auto=false"])
        .args(["-c", "gc.auto=0"])
        .args(["-c", "credential.helper="])
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG", "/dev/null")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
}

fn utf8_stdout(output: &Output, arguments: &[&str]) -> Result<String, RepoError> {
    String::from_utf8(output.stdout.clone())
        .map(|value| value.trim().to_owned())
        .map_err(|_| {
            RepoError::NonUtf8Output(
                std::iter::once("git".to_owned())
                    .chain(arguments.iter().map(|argument| (*argument).to_owned()))
                    .collect(),
            )
        })
}

fn command_error(directory: &Path, arguments: &[&str], output: &Output) -> RepoError {
    RepoError::GitCommandFailed {
        argv: argv(directory, arguments),
        status: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

fn argv(directory: &Path, arguments: &[&str]) -> Vec<String> {
    [
        "git".to_owned(),
        "-C".to_owned(),
        directory.display().to_string(),
    ]
    .into_iter()
    .chain(arguments.iter().map(|argument| (*argument).to_owned()))
    .collect()
}

fn canonical_git_path(value: &str) -> Result<PathBuf, RepoError> {
    let path = PathBuf::from(value);
    path.canonicalize()
        .map_err(|_| RepoError::PathUnavailable(path))
}
