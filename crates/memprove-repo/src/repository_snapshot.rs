use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Component, Path, PathBuf},
    process::Output,
};

use memprove_domain::{
    Evidence, EvidenceCollector, EvidenceId, EvidenceInput, EvidenceKind, EvidencePayload,
    EvidenceSubject, ProjectId, RepositorySnapshot, RepositorySnapshotId, RepositorySnapshotInput,
    Sha256Digest, TaskId, UtcTimestamp, WorkspaceId, porcelain_v2_paths,
    repository_capture_manifest_digest,
};
use sha2::{Digest, Sha256};

use crate::{GitWorkspaceResolver, RepoError, ResolvedGitWorkspace, git_workspace::hermetic_git};

/// One caller-selected `repo://` file and its stable Evidence identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedFileCapture {
    pub evidence_id: EvidenceId,
    pub path: String,
}

/// Stable IDs, times, and caller-provided Evidence used to materialize a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryCaptureRequest {
    pub project_id: ProjectId,
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub snapshot_id: RepositorySnapshotId,
    pub git_head_evidence_id: EvidenceId,
    pub git_worktree_evidence_id: EvidenceId,
    pub selected_files: Vec<SelectedFileCapture>,
    pub command_evidence: Vec<Evidence>,
    pub test_evidence: Vec<Evidence>,
    pub observed_at: UtcTimestamp,
    pub captured_at: UtcTimestamp,
    pub collector: EvidenceCollector,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedFileState {
    path: String,
    kind: &'static str,
    content_digest: Sha256Digest,
}

/// Exact read-only probe retained for the short precommit recheck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryProbe {
    repository_common_dir: PathBuf,
    worktree_root: PathBuf,
    git_dir: PathBuf,
    head_oid: String,
    branch: Option<String>,
    status_porcelain_v2: String,
    repository_identity: Sha256Digest,
    index_digest: Sha256Digest,
    tracked_worktree_digest: Sha256Digest,
    selected_files: Vec<SelectedFileState>,
    capture_manifest_digest: Sha256Digest,
}

impl RepositoryProbe {
    /// Re-runs the complete capture manifest, including Git/index/tracked state and selected files.
    /// This boundary comparison fails closed on observed drift; it does not claim detection of an
    /// arbitrary transient A-to-B-to-A change that begins and ends between observation points.
    ///
    /// # Errors
    ///
    /// Returns [`RepoError::RepositoryChanged`] for any captured-state drift.
    pub fn verify_unchanged(&self) -> Result<(), RepoError> {
        let selected_paths = self
            .selected_files
            .iter()
            .map(|selected| selected.path.clone())
            .collect::<Vec<_>>();
        let current = probe(&self.worktree_root, &selected_paths)?;
        if current == *self {
            Ok(())
        } else {
            Err(RepoError::RepositoryChanged)
        }
    }

    #[must_use]
    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    #[must_use]
    pub fn repository_common_dir(&self) -> &Path {
        &self.repository_common_dir
    }

    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    #[must_use]
    pub fn repository_identity(&self) -> &Sha256Digest {
        &self.repository_identity
    }

    #[must_use]
    pub fn head_oid(&self) -> &str {
        &self.head_oid
    }

    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    #[must_use]
    pub fn status_porcelain_v2(&self) -> &str {
        &self.status_porcelain_v2
    }

    #[must_use]
    pub fn index_digest(&self) -> &Sha256Digest {
        &self.index_digest
    }

    #[must_use]
    pub fn tracked_worktree_digest(&self) -> &Sha256Digest {
        &self.tracked_worktree_digest
    }

    #[must_use]
    pub fn capture_manifest_digest(&self) -> &Sha256Digest {
        &self.capture_manifest_digest
    }
}

/// Materialized capture ready for an atomic store call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRepositorySnapshot {
    pub evidence: Vec<Evidence>,
    pub snapshot: RepositorySnapshot,
    pub probe: RepositoryProbe,
}

/// Minimal read-only Git and selected-file collector.
pub struct RepositorySnapshotCollector;

impl RepositorySnapshotCollector {
    /// Captures exact Git state and only the explicitly supplied `repo://` files.
    ///
    /// Long-running file reads occur here, outside any `SQLite` transaction. The returned probe
    /// must be rechecked immediately before commit.
    ///
    /// # Errors
    ///
    /// Returns [`RepoError`] for Git, UTF-8, path-policy, file, or domain failures.
    #[allow(clippy::needless_pass_by_value)] // Preserve the existing one-shot public capture API.
    pub fn capture(
        repository: &Path,
        request: RepositoryCaptureRequest,
    ) -> Result<CapturedRepositorySnapshot, RepoError> {
        capture_with_hook(repository, &request, |_, _| Ok(()))
    }
}

const MAX_CAPTURE_ATTEMPTS: usize = 3;

fn capture_with_hook<F>(
    repository: &Path,
    request: &RepositoryCaptureRequest,
    mut after_initial_probe: F,
) -> Result<CapturedRepositorySnapshot, RepoError>
where
    F: FnMut(usize, &Path) -> Result<(), RepoError>,
{
    validate_request(request)?;
    let selected_paths = request
        .selected_files
        .iter()
        .map(|selected| selected.path.clone())
        .collect::<Vec<_>>();
    for attempt in 0..MAX_CAPTURE_ATTEMPTS {
        let captured_probe = probe(repository, &selected_paths)?;
        after_initial_probe(attempt, &captured_probe.worktree_root)?;
        let captured = capture_at_probe(request, &captured_probe)?;
        if probe(repository, &selected_paths)? == captured_probe {
            return Ok(captured);
        }
    }
    Err(RepoError::RepositoryChanged)
}

#[allow(clippy::too_many_lines)] // One typed Evidence-to-Snapshot materialization boundary.
fn capture_at_probe(
    request: &RepositoryCaptureRequest,
    probe: &RepositoryProbe,
) -> Result<CapturedRepositorySnapshot, RepoError> {
    let repository_identity = probe.repository_identity.clone();
    let mut evidence = vec![
        Evidence::new(EvidenceInput {
            evidence_id: request.git_head_evidence_id.clone(),
            task_id: request.task_id.clone(),
            workspace_id: request.workspace_id.clone(),
            kind: EvidenceKind::GitHead,
            subject: EvidenceSubject::GitHead {
                repository_identity: repository_identity.clone(),
            },
            observed_at: request.observed_at.clone(),
            collector: request.collector.clone(),
            payload: EvidencePayload::GitHead {
                head_oid: probe.head_oid.clone(),
                branch: probe.branch.clone(),
            },
        })
        .map_err(domain_error)?,
        Evidence::new(EvidenceInput {
            evidence_id: request.git_worktree_evidence_id.clone(),
            task_id: request.task_id.clone(),
            workspace_id: request.workspace_id.clone(),
            kind: EvidenceKind::GitWorktree,
            subject: EvidenceSubject::GitWorktree {
                repository_identity,
            },
            observed_at: request.observed_at.clone(),
            collector: request.collector.clone(),
            payload: EvidencePayload::GitWorktree {
                status_porcelain_v2: probe.status_porcelain_v2.clone(),
            },
        })
        .map_err(domain_error)?,
    ];
    for selected in &request.selected_files {
        let selected_state = probe
            .selected_files
            .iter()
            .find(|state| state.path == selected.path)
            .ok_or_else(|| {
                RepoError::RecoveryDomain(
                    "selected file is missing from the capture manifest".to_owned(),
                )
            })?;
        evidence.push(
            Evidence::new(EvidenceInput {
                evidence_id: selected.evidence_id.clone(),
                task_id: request.task_id.clone(),
                workspace_id: request.workspace_id.clone(),
                kind: EvidenceKind::FileDigest,
                subject: EvidenceSubject::FileDigest {
                    path: selected.path.clone(),
                },
                observed_at: request.observed_at.clone(),
                collector: request.collector.clone(),
                payload: EvidencePayload::FileDigest {
                    sha256: selected_state.content_digest.clone(),
                },
            })
            .map_err(domain_error)?,
        );
    }
    evidence.extend(request.command_evidence.iter().cloned());
    evidence.extend(request.test_evidence.iter().cloned());

    let file_evidence_ids = request
        .selected_files
        .iter()
        .map(|selected| selected.evidence_id.clone())
        .collect();
    let command_evidence_ids = request
        .command_evidence
        .iter()
        .map(|item| item.evidence_id().clone())
        .collect();
    let test_evidence_ids = request
        .test_evidence
        .iter()
        .map(|item| item.evidence_id().clone())
        .collect();
    let snapshot = RepositorySnapshot::new(
        RepositorySnapshotInput {
            snapshot_id: request.snapshot_id.clone(),
            project_id: request.project_id.clone(),
            task_id: request.task_id.clone(),
            workspace_id: request.workspace_id.clone(),
            head_oid: probe.head_oid.clone(),
            branch: probe.branch.clone(),
            worktree_status_porcelain_v2: probe.status_porcelain_v2.clone(),
            index_digest: probe.index_digest.clone(),
            tracked_worktree_digest: probe.tracked_worktree_digest.clone(),
            capture_manifest_digest: probe.capture_manifest_digest.clone(),
            git_head_evidence_id: request.git_head_evidence_id.clone(),
            git_worktree_evidence_id: request.git_worktree_evidence_id.clone(),
            file_evidence_ids,
            command_evidence_ids,
            test_evidence_ids,
            captured_at: request.captured_at.clone(),
        },
        &evidence,
    )
    .map_err(domain_error)?;
    Ok(CapturedRepositorySnapshot {
        evidence,
        snapshot,
        probe: probe.clone(),
    })
}

fn validate_request(request: &RepositoryCaptureRequest) -> Result<(), RepoError> {
    let mut ids = BTreeSet::new();
    for id in std::iter::once(&request.git_head_evidence_id)
        .chain(std::iter::once(&request.git_worktree_evidence_id))
        .chain(
            request
                .selected_files
                .iter()
                .map(|selected| &selected.evidence_id),
        )
        .chain(request.command_evidence.iter().map(Evidence::evidence_id))
        .chain(request.test_evidence.iter().map(Evidence::evidence_id))
    {
        if !ids.insert(id) {
            return Err(RepoError::RecoveryDomain(
                "capture Evidence IDs must be unique".to_owned(),
            ));
        }
    }
    let mut paths = BTreeSet::new();
    for selected in &request.selected_files {
        if !paths.insert(&selected.path) {
            return Err(RepoError::InvalidRepoPath(selected.path.clone()));
        }
    }
    if request
        .command_evidence
        .iter()
        .any(|item| item.kind() != EvidenceKind::CommandResult)
        || request
            .test_evidence
            .iter()
            .any(|item| item.kind() != EvidenceKind::TestResult)
    {
        return Err(RepoError::RecoveryDomain(
            "caller Evidence role does not match kind".to_owned(),
        ));
    }
    Ok(())
}

fn probe(repository: &Path, selected_paths: &[String]) -> Result<RepositoryProbe, RepoError> {
    for _ in 0..MAX_CAPTURE_ATTEMPTS {
        let first = probe_once(repository, selected_paths)?;
        let second = probe_once(repository, selected_paths)?;
        if first == second {
            return Ok(second);
        }
    }
    Err(RepoError::RepositoryChanged)
}

fn probe_once(repository: &Path, selected_paths: &[String]) -> Result<RepositoryProbe, RepoError> {
    let resolved = GitWorkspaceResolver::resolve(repository)?;
    let branch = branch(&resolved);
    let status = run_git_exact(
        &resolved.worktree_root,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    )?;
    let index_state = run_git_bytes(&resolved.worktree_root, &["ls-files", "--stage", "-z"])?;
    let repository_identity = repository_identity(&resolved);
    let index_digest = Sha256Digest::of_bytes(&index_state);
    let tracked_worktree_digest = tracked_worktree_digest(&resolved.worktree_root, &index_state)?;
    let mut selected_files = selected_paths
        .iter()
        .map(|path| read_selected_file_state(&resolved.worktree_root, path))
        .collect::<Result<Vec<_>, _>>()?;
    selected_files.sort_by(|left, right| left.path.cmp(&right.path));
    let capture_manifest_digest = capture_manifest_digest(
        &repository_identity,
        &resolved.head_commit,
        branch.as_deref(),
        &status,
        &index_digest,
        &tracked_worktree_digest,
        &selected_files,
    );
    Ok(RepositoryProbe {
        repository_identity,
        index_digest,
        tracked_worktree_digest,
        selected_files,
        capture_manifest_digest,
        repository_common_dir: resolved.repository_common_dir,
        worktree_root: resolved.worktree_root,
        git_dir: resolved.git_dir,
        head_oid: resolved.head_commit,
        branch,
        status_porcelain_v2: status,
    })
}

fn branch(resolved: &ResolvedGitWorkspace) -> Option<String> {
    resolved
        .branch_or_detached_state
        .strip_prefix("branch:")
        .map(str::to_owned)
}

fn run_git_exact(directory: &Path, arguments: &[&str]) -> Result<String, RepoError> {
    exact_utf8(
        run_git_bytes(directory, arguments)?,
        argv(directory, arguments),
    )
}

fn run_git_bytes(directory: &Path, arguments: &[&str]) -> Result<Vec<u8>, RepoError> {
    let output = fixed_git(directory, arguments)?;
    if !output.status.success() {
        return Err(RepoError::GitCommandFailed {
            argv: argv(directory, arguments),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
}

fn exact_utf8(bytes: Vec<u8>, command_argv: Vec<String>) -> Result<String, RepoError> {
    String::from_utf8(bytes).map_err(|_| RepoError::NonUtf8Output(command_argv))
}

fn fixed_git(directory: &Path, arguments: &[&str]) -> Result<Output, RepoError> {
    hermetic_git(directory, arguments).map_err(|error| RepoError::GitCommandFailed {
        argv: argv(directory, arguments),
        status: None,
        stderr: error.to_string(),
    })
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

fn repository_identity(resolved: &ResolvedGitWorkspace) -> Sha256Digest {
    workspace_repository_identity(
        &resolved.repository_common_dir,
        &resolved.worktree_root,
        &resolved.git_dir,
        &resolved.object_format,
    )
}

/// The repository identity recorded in `git_head` and `git_worktree`
/// Evidence subjects, for a worktree at these canonical paths.
#[must_use]
pub fn workspace_repository_identity(
    repository_common_dir: &Path,
    worktree_root: &Path,
    git_dir: &Path,
    object_format: &str,
) -> Sha256Digest {
    let mut identity = Vec::new();
    append_field(&mut identity, repository_common_dir.as_os_str().as_bytes());
    append_field(&mut identity, worktree_root.as_os_str().as_bytes());
    append_field(&mut identity, git_dir.as_os_str().as_bytes());
    append_field(&mut identity, object_format.as_bytes());
    Sha256Digest::of_bytes(&identity)
}

/// One read-only observation of a worktree for a Recovery Preview
/// (ADR 0012 P1 and D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceObservation {
    /// Git, index, and tracked-content state, as a capture would record it.
    pub probe: RepositoryProbe,
    /// Content digest of each requested `repo://` file, `None` when it is no
    /// longer a readable regular file inside the worktree.
    pub selected_files: BTreeMap<String, Option<Sha256Digest>>,
    /// Untracked, not ignored paths, relative to the worktree root.
    pub untracked_paths: BTreeSet<String>,
    /// Paths that may differ between `base_commit` and the worktree: the
    /// paths `git diff-tree` reports between `base_commit` and HEAD, unioned
    /// with every path the status reports (a superset of `git diff
    /// base_commit`), or `None` when that commit is not in the repository.
    pub changed_since_base: Option<BTreeSet<String>>,
}

/// Observes the worktree at `path` with fixed-argv, read-only Git commands:
/// the capture probe, the requested `repo://` files, the untracked paths,
/// and the paths changed since `base_commit`. The capture probe is repeated
/// after the other reads, and the whole observation is retried until it is
/// stable.
///
/// Nothing here writes to the repository (P2-E, T9 S2). `git status` under
/// `--no-optional-locks` refreshes stat information in memory only, so a
/// file whose stat data is merely out of date is not reported.
/// `git diff <base>` would refresh and rewrite `.git/index` even so; the
/// changed paths are therefore `git diff-tree` between two commits, which
/// never reads the index or the worktree, unioned with the paths the
/// status already reports.
///
/// # Errors
///
/// [`RepoError`] when the path is not a Git worktree, a Git command fails,
/// or the worktree keeps changing.
pub fn observe_workspace(
    path: &Path,
    selected_paths: &[String],
    base_commit: &str,
) -> Result<WorkspaceObservation, RepoError> {
    let mut observed = observe_workspace_bases(path, selected_paths, &[base_commit.to_owned()])?;
    Ok(WorkspaceObservation {
        probe: observed.probe,
        selected_files: observed.selected_files,
        untracked_paths: observed.untracked_paths,
        changed_since_base: observed.changed_since.remove(base_commit).flatten(),
    })
}

/// One read-only observation of a worktree for a Task Capsule (ADR 0013
/// D4): [`observe_workspace`] with the changed paths since each of several
/// base commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBasesObservation {
    /// Git, index, and tracked-content state, as a capture would record it.
    pub probe: RepositoryProbe,
    /// Content digest of each requested `repo://` file, `None` when it is no
    /// longer a readable regular file inside the worktree.
    pub selected_files: BTreeMap<String, Option<Sha256Digest>>,
    /// Untracked, not ignored paths, relative to the worktree root.
    pub untracked_paths: BTreeSet<String>,
    /// For each requested base commit, the paths `git diff-tree` reports
    /// between it and HEAD unioned with every tracked path the status
    /// reports, or `None` when that commit is not in the repository.
    pub changed_since: BTreeMap<String, Option<BTreeSet<String>>>,
}

/// [`observe_workspace`] for several base commits at once: one `git
/// diff-tree` per distinct base, inside the same stable observation. The
/// caller bounds the number of bases (ADR 0013: at most 16).
///
/// # Errors
///
/// [`RepoError`] when the path is not a Git worktree, a Git command fails,
/// or the worktree keeps changing.
pub fn observe_workspace_bases(
    path: &Path,
    selected_paths: &[String],
    base_commits: &[String],
) -> Result<WorkspaceBasesObservation, RepoError> {
    // T21 (V3-F-1): one probe before the other reads and one after; the
    // observation holds only when the two are equal, which is the check
    // [`probe`]'s own repetition would make again. A read probes the
    // Workspace once per Task Capsule, so each Git process counts.
    for _ in 0..MAX_CAPTURE_ATTEMPTS {
        let before = probe_once(path, &[])?;
        let root = before.worktree_root.clone();
        let mut selected_files = BTreeMap::new();
        let mut changed = false;
        for selected in selected_paths {
            let digest = match read_selected_file_state(&root, selected) {
                Ok(state) => Some(state.content_digest),
                Err(RepoError::RepositoryChanged) => {
                    changed = true;
                    None
                }
                Err(_) => None,
            };
            selected_files.insert(selected.clone(), digest);
        }
        let untracked_paths = z_paths(&run_git_bytes(
            &root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?)?;
        // Tracked paths only: untracked ones are reported on their own.
        let status = porcelain_v2_paths(before.status_porcelain_v2());
        let tracked = status
            .all
            .difference(&status.untracked)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut changed_since = BTreeMap::new();
        for base_commit in base_commits {
            if changed_since.contains_key(base_commit) {
                continue;
            }
            let since = if commit_exists(&root, base_commit)? {
                let mut paths = z_paths(&run_git_bytes(
                    &root,
                    &[
                        "diff-tree",
                        "-r",
                        "--name-only",
                        "--no-renames",
                        "-z",
                        base_commit,
                        before.head_oid(),
                        "--",
                    ],
                )?)?;
                paths.extend(tracked.iter().cloned());
                Some(paths)
            } else {
                None
            };
            changed_since.insert(base_commit.clone(), since);
        }
        if !changed && probe_once(path, &[])? == before {
            return Ok(WorkspaceBasesObservation {
                probe: before,
                selected_files,
                untracked_paths,
                changed_since,
            });
        }
    }
    Err(RepoError::RepositoryChanged)
}

/// Whether `oid` names a commit in the repository (`git cat-file -e`).
fn commit_exists(root: &Path, oid: &str) -> Result<bool, RepoError> {
    if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(false);
    }
    let object = format!("{oid}^{{commit}}");
    Ok(fixed_git(root, &["cat-file", "-e", &object])?
        .status
        .success())
}

/// NUL-separated UTF-8 paths.
fn z_paths(bytes: &[u8]) -> Result<BTreeSet<String>, RepoError> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            String::from_utf8(path.to_vec())
                .map_err(|_| RepoError::NonUtf8Output(vec!["git".to_owned(), "-z".to_owned()]))
        })
        .collect()
}

fn capture_manifest_digest(
    repository_identity: &Sha256Digest,
    head_oid: &str,
    branch: Option<&str>,
    status_porcelain_v2: &str,
    index_digest: &Sha256Digest,
    tracked_worktree_digest: &Sha256Digest,
    selected_files: &[SelectedFileState],
) -> Sha256Digest {
    let selected_files = selected_files
        .iter()
        .map(|selected| (selected.path.as_str(), &selected.content_digest))
        .collect::<Vec<_>>();
    repository_capture_manifest_digest(
        repository_identity,
        head_oid,
        branch,
        status_porcelain_v2,
        index_digest,
        tracked_worktree_digest,
        &selected_files,
    )
}

fn tracked_worktree_digest(
    worktree_root: &Path,
    index_state: &[u8],
) -> Result<Sha256Digest, RepoError> {
    let mut manifest = Vec::new();
    append_field(&mut manifest, b"continuity.tracked-worktree-manifest.v2");
    for record in index_state
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| {
                RepoError::RecoveryDomain("invalid git ls-files --stage record".to_owned())
            })?;
        let metadata = &record[..separator];
        let raw_path = &record[separator + 1..];
        let fields = metadata
            .split(|byte| *byte == b' ')
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let [mode, object_id, stage] = fields.as_slice() else {
            return Err(RepoError::RecoveryDomain(
                "invalid git ls-files --stage metadata".to_owned(),
            ));
        };
        append_field(&mut manifest, raw_path);
        append_field(&mut manifest, mode);
        append_field(&mut manifest, object_id);
        append_field(&mut manifest, stage);
        let path = worktree_root.join(std::ffi::OsString::from_vec(raw_path.to_vec()));
        if *mode == b"160000" {
            append_submodule_state(&mut manifest, &path, object_id)?;
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                manifest.push(1);
                manifest.extend_from_slice(&(metadata.mode() & 0o111).to_be_bytes());
                let bytes = fs::read(&path)?;
                let after = fs::symlink_metadata(&path)?;
                if !same_file_state(&metadata, &after) {
                    return Err(RepoError::RepositoryChanged);
                }
                append_field(&mut manifest, &Sha256::digest(bytes));
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                manifest.push(2);
                let target = fs::read_link(&path)?;
                let after = fs::symlink_metadata(&path)?;
                if !same_file_state(&metadata, &after) {
                    return Err(RepoError::RepositoryChanged);
                }
                append_field(&mut manifest, target.as_os_str().as_bytes());
            }
            Ok(metadata) if metadata.file_type().is_dir() => manifest.push(3),
            Ok(_) => manifest.push(4),
            Err(error) if error.kind() == ErrorKind::NotFound => manifest.push(0),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Sha256Digest::of_bytes(&manifest))
}

fn append_submodule_state(
    manifest: &mut Vec<u8>,
    path: &Path,
    index_object_id: &[u8],
) -> Result<(), RepoError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            manifest.push(0);
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() {
        return Err(RepoError::NotRegularFile(path.to_owned()));
    }
    match fs::symlink_metadata(path.join(".git")) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            manifest.push(0);
            return Ok(());
        }
        Err(error) => return Err(error.into()),
        Ok(git_metadata) if git_metadata.file_type().is_symlink() => {
            return Err(RepoError::SymlinkNotAllowed(path.join(".git")));
        }
        Ok(_) => {}
    }
    let resolved = GitWorkspaceResolver::resolve(path)?;
    if resolved.worktree_root != path.canonicalize()? {
        return Err(RepoError::RepositoryChanged);
    }
    let status = run_git_exact(
        &resolved.worktree_root,
        &["status", "--porcelain=v2", "--untracked-files=all"],
    )?;
    manifest.push(1);
    append_field(manifest, index_object_id);
    append_field(manifest, resolved.head_commit.as_bytes());
    append_field(manifest, resolved.branch_or_detached_state.as_bytes());
    append_field(manifest, resolved.object_format.as_bytes());
    append_field(manifest, status.as_bytes());
    manifest.push(u8::from(
        resolved.head_commit.as_bytes() != index_object_id || !status.is_empty(),
    ));
    Ok(())
}

fn same_file_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.mode() == after.mode()
        && before.size() == after.size()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn read_selected_file_state(
    worktree_root: &Path,
    repo_uri: &str,
) -> Result<SelectedFileState, RepoError> {
    let raw = repo_uri
        .strip_prefix("repo://")
        .ok_or_else(|| RepoError::InvalidRepoPath(repo_uri.to_owned()))?;
    if raw.is_empty() || raw.starts_with('/') {
        return Err(RepoError::InvalidRepoPath(repo_uri.to_owned()));
    }
    let relative = Path::new(raw);
    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => {
                return Err(RepoError::InvalidRepoPath(repo_uri.to_owned()));
            }
        }
    }
    if normalized.as_os_str() != relative.as_os_str() {
        return Err(RepoError::InvalidRepoPath(repo_uri.to_owned()));
    }
    let mut current = worktree_root.to_owned();
    let component_count = normalized.components().count();
    for (index, component) in normalized.components().enumerate() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() {
            return Err(RepoError::SymlinkNotAllowed(current));
        }
        if index + 1 < component_count && !metadata.is_dir() {
            return Err(RepoError::InvalidRepoPath(repo_uri.to_owned()));
        }
        if index + 1 == component_count && !metadata.is_file() {
            return Err(RepoError::NotRegularFile(current));
        }
    }
    let canonical_before = current.canonicalize()?;
    let metadata_before = fs::symlink_metadata(&current)?;
    if !canonical_before.starts_with(worktree_root) {
        return Err(RepoError::InvalidRepoPath(repo_uri.to_owned()));
    }
    let bytes = fs::read(&current)?;
    let canonical_after = current.canonicalize()?;
    let metadata_after = fs::symlink_metadata(&current)?;
    if canonical_before != canonical_after
        || !same_file_state(&metadata_before, &metadata_after)
        || metadata_after.file_type().is_symlink()
        || !metadata_after.is_file()
    {
        return Err(RepoError::RepositoryChanged);
    }
    Ok(SelectedFileState {
        path: repo_uri.to_owned(),
        kind: "regular_file",
        content_digest: Sha256Digest::of_bytes(&bytes),
    })
}

fn domain_error(error: impl std::fmt::Display) -> RepoError {
    RepoError::RecoveryDomain(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, process::Command};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn exact_git_output_decoder_rejects_non_utf8_without_lossy_conversion() {
        let result = exact_utf8(
            vec![b'o', b'k', 0x80],
            vec!["git".to_owned(), "status".to_owned()],
        );

        assert!(matches!(result, Err(RepoError::NonUtf8Output(argv)) if argv == ["git", "status"]));
    }

    #[test]
    fn tracked_file_change_during_capture_retries_to_one_coherent_state() {
        let directory = TempDir::new().expect("temporary root");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).expect("repository directory");
        fixture_git(&repository, &["init", "--initial-branch=main"]);
        fs::write(repository.join("tracked.txt"), b"before\n").expect("tracked file");
        fixture_git(&repository, &["add", "tracked.txt"]);
        fixture_git(
            &repository,
            &[
                "-c",
                "user.name=memprove Test",
                "-c",
                "user.email=memprove@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        );
        let selected_id = EvidenceId::new();
        let changed = Cell::new(false);
        let captured = capture_with_hook(
            &repository,
            &RepositoryCaptureRequest {
                project_id: ProjectId::new(),
                task_id: TaskId::new(),
                workspace_id: WorkspaceId::new(),
                snapshot_id: RepositorySnapshotId::new(),
                git_head_evidence_id: EvidenceId::new(),
                git_worktree_evidence_id: EvidenceId::new(),
                selected_files: vec![SelectedFileCapture {
                    evidence_id: selected_id.clone(),
                    path: "repo://tracked.txt".to_owned(),
                }],
                command_evidence: vec![],
                test_evidence: vec![],
                observed_at: UtcTimestamp::parse("2026-08-16T02:00:00Z").expect("observed_at"),
                captured_at: UtcTimestamp::parse("2026-08-16T02:00:01Z").expect("captured_at"),
                collector: EvidenceCollector {
                    name: "memprove-repo".to_owned(),
                    version: "0.1.0".to_owned(),
                },
            },
            |attempt, worktree| {
                if attempt == 0 {
                    fs::write(worktree.join("tracked.txt"), b"after\n")?;
                    changed.set(true);
                }
                Ok(())
            },
        )
        .expect("capture retries");

        assert!(changed.get());
        assert!(matches!(
            captured
                .evidence
                .iter()
                .find(|item| item.evidence_id() == &selected_id)
                .expect("selected Evidence")
                .payload(),
            EvidencePayload::FileDigest { sha256 }
                if *sha256 == Sha256Digest::of_bytes(b"after\n")
        ));
        captured.probe.verify_unchanged().expect("coherent state");
    }

    #[test]
    fn index_and_head_changes_during_capture_retry_to_one_coherent_state() {
        let directory = TempDir::new().expect("temporary root");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).expect("repository directory");
        fixture_git(&repository, &["init", "--initial-branch=main"]);
        fs::write(repository.join("tracked.txt"), b"before\n").expect("tracked file");
        fixture_git(&repository, &["add", "tracked.txt"]);
        fixture_git(
            &repository,
            &[
                "-c",
                "user.name=memprove Test",
                "-c",
                "user.email=memprove@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        );
        let attempts = Cell::new(0_usize);
        let captured = capture_with_hook(
            &repository,
            &RepositoryCaptureRequest {
                project_id: ProjectId::new(),
                task_id: TaskId::new(),
                workspace_id: WorkspaceId::new(),
                snapshot_id: RepositorySnapshotId::new(),
                git_head_evidence_id: EvidenceId::new(),
                git_worktree_evidence_id: EvidenceId::new(),
                selected_files: vec![],
                command_evidence: vec![],
                test_evidence: vec![],
                observed_at: UtcTimestamp::parse("2026-08-16T02:00:00Z").expect("observed_at"),
                captured_at: UtcTimestamp::parse("2026-08-16T02:00:01Z").expect("captured_at"),
                collector: EvidenceCollector {
                    name: "memprove-repo".to_owned(),
                    version: "0.1.0".to_owned(),
                },
            },
            |attempt, worktree| {
                attempts.set(attempts.get() + 1);
                if attempt == 0 {
                    fs::write(worktree.join("tracked.txt"), b"staged\n")?;
                    fixture_git(worktree, &["add", "tracked.txt"]);
                } else if attempt == 1 {
                    fixture_git(
                        worktree,
                        &[
                            "-c",
                            "user.name=memprove Test",
                            "-c",
                            "user.email=memprove@example.invalid",
                            "commit",
                            "-m",
                            "capture drift",
                        ],
                    );
                }
                Ok(())
            },
        )
        .expect("capture retries after index and HEAD drift");

        assert_eq!(attempts.get(), 3);
        let current_head = run_git_exact(&repository, &["rev-parse", "--verify", "HEAD"])
            .expect("current HEAD")
            .trim()
            .to_owned();
        assert_eq!(captured.snapshot.head_oid(), current_head);
        captured.probe.verify_unchanged().expect("coherent state");
    }

    #[test]
    fn selected_untracked_change_exhausts_bounded_capture_retries() {
        let directory = TempDir::new().expect("temporary root");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).expect("repository directory");
        fixture_git(&repository, &["init", "--initial-branch=main"]);
        fs::write(repository.join("tracked.txt"), b"tracked\n").expect("tracked file");
        fixture_git(&repository, &["add", "tracked.txt"]);
        fixture_git(
            &repository,
            &[
                "-c",
                "user.name=memprove Test",
                "-c",
                "user.email=memprove@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        );
        fs::write(repository.join("selected.txt"), b"A\n").expect("selected file");
        let attempts = Cell::new(0_usize);
        let result = capture_with_hook(
            &repository,
            &RepositoryCaptureRequest {
                project_id: ProjectId::new(),
                task_id: TaskId::new(),
                workspace_id: WorkspaceId::new(),
                snapshot_id: RepositorySnapshotId::new(),
                git_head_evidence_id: EvidenceId::new(),
                git_worktree_evidence_id: EvidenceId::new(),
                selected_files: vec![SelectedFileCapture {
                    evidence_id: EvidenceId::new(),
                    path: "repo://selected.txt".to_owned(),
                }],
                command_evidence: vec![],
                test_evidence: vec![],
                observed_at: UtcTimestamp::parse("2026-08-16T02:00:00Z").expect("observed_at"),
                captured_at: UtcTimestamp::parse("2026-08-16T02:00:01Z").expect("captured_at"),
                collector: EvidenceCollector {
                    name: "memprove-repo".to_owned(),
                    version: "0.1.0".to_owned(),
                },
            },
            |attempt, worktree| {
                attempts.set(attempts.get() + 1);
                let bytes: &[u8] = if attempt % 2 == 0 { b"B\n" } else { b"A\n" };
                fs::write(worktree.join("selected.txt"), bytes)?;
                Ok(())
            },
        );

        assert!(matches!(result, Err(RepoError::RepositoryChanged)));
        assert_eq!(attempts.get(), MAX_CAPTURE_ATTEMPTS);
    }

    /// Every file under `.git` with its bytes and modification time.
    fn git_dir_state(repository: &Path) -> BTreeMap<PathBuf, (Vec<u8>, i64, i64)> {
        let mut state = BTreeMap::new();
        let mut pending = vec![repository.join(".git")];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                let metadata = fs::symlink_metadata(&path).unwrap();
                if metadata.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = fs::read(&path).unwrap();
                    state.insert(path, (bytes, metadata.mtime(), metadata.mtime_nsec()));
                }
            }
        }
        state
    }

    fn commit(repository: &Path, message: &str) -> String {
        fixture_git(
            repository,
            &[
                "-c",
                "user.name=memprove Test",
                "-c",
                "user.email=memprove@example.invalid",
                "commit",
                "--quiet",
                "-am",
                message,
            ],
        );
        run_git_exact(repository, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned()
    }

    #[test]
    fn workspace_observation_is_read_only_and_reports_d5_paths() {
        let directory = TempDir::new().expect("temporary root");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fixture_git(&repository, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(repository.join("README"), b"one\n").unwrap();
        fs::write(repository.join("kept.txt"), b"kept\n").unwrap();
        fixture_git(&repository, &["add", "README", "kept.txt"]);
        let base = commit(&repository, "base");
        let selected = ["repo://README".to_owned(), "repo://gone.txt".to_owned()];

        let unchanged = observe_workspace(&repository, &selected, &base).unwrap();
        assert_eq!(unchanged.probe.head_oid(), base);
        assert_eq!(unchanged.changed_since_base, Some(BTreeSet::new()));
        assert!(unchanged.untracked_paths.is_empty());
        assert_eq!(
            unchanged.selected_files,
            BTreeMap::from([
                (
                    "repo://README".to_owned(),
                    Some(Sha256Digest::of_bytes(b"one\n"))
                ),
                ("repo://gone.txt".to_owned(), None),
            ])
        );

        fs::write(repository.join("README"), b"two\n").unwrap();
        let head = commit(&repository, "change README");
        fs::write(repository.join("kept.txt"), b"edited\n").unwrap();
        fs::write(repository.join("new file.txt"), b"new\n").unwrap();
        // Make the index stat-dirty so a refreshing command would rewrite it.
        fs::write(repository.join("README"), b"two\n").unwrap();
        let before = git_dir_state(&repository);
        let observed = observe_workspace(&repository, &selected, &base).unwrap();
        assert_eq!(
            git_dir_state(&repository),
            before,
            "the probe wrote to .git"
        );
        assert_eq!(observed.probe.head_oid(), head);
        assert_eq!(
            observed.changed_since_base,
            Some(BTreeSet::from(["README".to_owned(), "kept.txt".to_owned()]))
        );
        assert_eq!(
            observed.untracked_paths,
            BTreeSet::from(["new file.txt".to_owned()])
        );

        let unknown = "c".repeat(base.len());
        let collected = observe_workspace(&repository, &selected, &unknown).unwrap();
        assert_eq!(collected.changed_since_base, None);
        assert!(observe_workspace(&directory.path().join("missing"), &[], &base).is_err());
    }

    /// T21 (V3-F-1): a read-only observation runs one probe before its
    /// other reads and one after, not two of each (before: 35 Git processes
    /// for one base commit, now 19).
    #[test]
    fn workspace_observation_probes_once_on_each_side() {
        let directory = TempDir::new().expect("temporary root");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fixture_git(&repository, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(repository.join("README"), b"one\n").unwrap();
        fixture_git(&repository, &["add", "README"]);
        let base = commit(&repository, "base");
        let start = crate::git_workspace::git_runs();
        probe_once(&repository, &[]).unwrap();
        let one_probe = crate::git_workspace::git_runs() - start;
        let start = crate::git_workspace::git_runs();
        observe_workspace(&repository, &[], &base).unwrap();
        let observed = crate::git_workspace::git_runs() - start;
        // A probe on each side, `ls-files --others`, and `cat-file -e` and
        // `diff-tree` for the base commit.
        assert_eq!(observed, 2 * one_probe + 3, "one probe is {one_probe}");
    }

    #[test]
    fn workspace_identity_matches_the_capture_identity() {
        let directory = TempDir::new().expect("temporary root");
        let repository = directory.path().join("repository");
        fs::create_dir(&repository).unwrap();
        fixture_git(&repository, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(repository.join("README"), b"one\n").unwrap();
        fixture_git(&repository, &["add", "README"]);
        let base = commit(&repository, "base");
        let observed = observe_workspace(&repository, &[], &base).unwrap();
        let resolved = GitWorkspaceResolver::resolve(&repository).unwrap();
        assert_eq!(
            workspace_repository_identity(
                &resolved.repository_common_dir,
                &resolved.worktree_root,
                &resolved.git_dir,
                &resolved.object_format,
            ),
            *observed.probe.repository_identity()
        );
    }

    fn fixture_git(repository: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("fixture Git command");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
