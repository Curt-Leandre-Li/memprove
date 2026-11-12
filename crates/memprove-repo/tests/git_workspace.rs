use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use memprove_repo::{GitWorkspaceResolver, RepoError};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[derive(Debug, PartialEq, Eq)]
struct GitSnapshot {
    head: String,
    status: String,
    refs: String,
    index_sha256: Option<String>,
    tracked_file_sha256: BTreeMap<String, String>,
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
}

fn create_repository() -> (TempDir, PathBuf) {
    let directory = TempDir::new().expect("temporary root");
    let repository = directory.path().join("repository");
    fs::create_dir(&repository).expect("repository directory");
    git(&repository, &["init", "--initial-branch=main"]);
    git(&repository, &["config", "user.name", "memprove Test"]);
    git(
        &repository,
        &["config", "user.email", "memprove@example.invalid"],
    );
    fs::write(repository.join("tracked.txt"), b"tracked fixture\n").expect("tracked file");
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "-m", "fixture"]);
    (directory, repository)
}

fn snapshot(repository: &Path) -> GitSnapshot {
    let git_dir = PathBuf::from(git(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let index = git_dir.join("index");
    let index_sha256 = index
        .is_file()
        .then(|| hex_digest(&fs::read(&index).expect("index bytes")));
    let mut tracked_file_sha256 = BTreeMap::new();
    for relative in git(repository, &["ls-files"]).lines() {
        let bytes = fs::read(repository.join(relative)).expect("tracked file");
        tracked_file_sha256.insert(relative.to_owned(), hex_digest(&bytes));
    }
    GitSnapshot {
        head: git(repository, &["rev-parse", "--verify", "HEAD"]),
        status: git(
            repository,
            &["status", "--porcelain=v2", "--untracked-files=all"],
        ),
        refs: git(
            repository,
            &[
                "for-each-ref",
                "--sort=refname",
                "--format=%(refname)%00%(objectname)",
            ],
        ),
        index_sha256,
        tracked_file_sha256,
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[test]
fn resolves_normal_repository_subdirectory_and_symlink_without_mutation() {
    let (directory, repository) = create_repository();
    let subdirectory = repository.join("nested");
    fs::create_dir(&subdirectory).expect("nested directory");
    let symlink = directory.path().join("repository-link");
    std::os::unix::fs::symlink(&repository, &symlink).expect("repository symlink");
    let before = snapshot(&repository);

    let root = GitWorkspaceResolver::resolve(&repository).expect("root repository");
    let nested = GitWorkspaceResolver::resolve(&subdirectory).expect("nested path");
    let linked_path = GitWorkspaceResolver::resolve(&symlink).expect("symlink path");

    let canonical_root = repository.canonicalize().expect("canonical repository");
    assert_eq!(root.worktree_root, canonical_root);
    assert_eq!(nested, root);
    assert_eq!(linked_path, root);
    assert_eq!(root.repository_common_dir, canonical_root.join(".git"));
    assert_eq!(root.git_dir, root.repository_common_dir);
    assert_eq!(root.object_format, "sha1");
    assert_eq!(root.branch_or_detached_state, "branch:main");
    assert_eq!(root.head_commit.len(), 40);
    assert_eq!(snapshot(&repository), before);
}

#[test]
fn resolves_linked_worktree_and_detached_head_without_mutation() {
    let (directory, repository) = create_repository();
    let linked = directory.path().join("linked-worktree");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "linked-branch",
            linked.to_str().expect("UTF-8 path"),
        ],
    );
    let before_repository = snapshot(&repository);
    let before_linked = snapshot(&linked);

    let resolved = GitWorkspaceResolver::resolve(&linked).expect("linked worktree");
    assert_eq!(
        resolved.repository_common_dir,
        repository
            .canonicalize()
            .expect("canonical repository")
            .join(".git")
    );
    assert_eq!(
        resolved.worktree_root,
        linked.canonicalize().expect("canonical linked worktree")
    );
    assert!(
        resolved
            .git_dir
            .starts_with(resolved.repository_common_dir.join("worktrees"))
    );
    assert_eq!(resolved.branch_or_detached_state, "branch:linked-branch");
    assert_eq!(snapshot(&repository), before_repository);
    assert_eq!(snapshot(&linked), before_linked);

    git(&linked, &["checkout", "--detach"]);
    let before_detached = snapshot(&linked);
    let detached = GitWorkspaceResolver::resolve(&linked).expect("detached worktree");
    assert_eq!(detached.branch_or_detached_state, "detached");
    assert_eq!(snapshot(&linked), before_detached);
}

#[test]
fn rejects_missing_and_non_git_paths() {
    let directory = TempDir::new().expect("temporary root");
    let missing = directory.path().join("missing");

    assert!(matches!(
        GitWorkspaceResolver::resolve(&missing),
        Err(RepoError::PathUnavailable(_))
    ));
    assert!(matches!(
        GitWorkspaceResolver::resolve(directory.path()),
        Err(RepoError::GitCommandFailed { .. })
    ));
}
