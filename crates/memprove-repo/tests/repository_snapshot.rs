use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::ffi::OsStringExt,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use memprove_domain::{
    EvidenceCollector, EvidenceKind, EvidencePayload, ProjectId, RepositorySnapshotId, TaskId,
    UtcTimestamp, WorkspaceId,
};
use memprove_repo::{
    RepoError, RepositoryCaptureRequest, RepositorySnapshotCollector, SelectedFileCapture,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[derive(Debug, PartialEq, Eq)]
struct RepositoryMutationSnapshot {
    head: Vec<u8>,
    status: Vec<u8>,
    refs: Vec<u8>,
    index: Option<Vec<u8>>,
    files: BTreeMap<OsString, Vec<u8>>,
}

fn git_output(repository: &Path, arguments: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .env("LC_ALL", "C")
        .output()
        .expect("Git fixture command")
}

fn git(repository: &Path, arguments: &[&str]) -> String {
    let output = git_output(repository, arguments);
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 fixture output")
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

fn create_repository_with_submodule() -> (TempDir, PathBuf, PathBuf) {
    let (directory, repository) = create_repository();
    let origin = directory.path().join("submodule-origin");
    fs::create_dir(&origin).expect("submodule origin");
    git(&origin, &["init", "--initial-branch=main"]);
    git(&origin, &["config", "user.name", "memprove Test"]);
    git(
        &origin,
        &["config", "user.email", "memprove@example.invalid"],
    );
    fs::write(origin.join("nested.txt"), b"initial\n").expect("submodule file");
    git(&origin, &["add", "nested.txt"]);
    git(&origin, &["commit", "-m", "submodule initial"]);
    let origin_path = origin.display().to_string();
    git(
        &repository,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &origin_path,
            "nested",
        ],
    );
    git(&repository, &["commit", "-m", "add submodule"]);
    let submodule = repository.join("nested");
    git(&submodule, &["config", "user.name", "memprove Test"]);
    git(
        &submodule,
        &["config", "user.email", "memprove@example.invalid"],
    );
    (directory, repository, submodule)
}

fn request(selected_files: Vec<SelectedFileCapture>) -> RepositoryCaptureRequest {
    RepositoryCaptureRequest {
        project_id: ProjectId::new(),
        task_id: TaskId::new(),
        workspace_id: WorkspaceId::new(),
        snapshot_id: RepositorySnapshotId::new(),
        git_head_evidence_id: memprove_domain::EvidenceId::new(),
        git_worktree_evidence_id: memprove_domain::EvidenceId::new(),
        selected_files,
        command_evidence: vec![],
        test_evidence: vec![],
        observed_at: UtcTimestamp::parse("2026-08-16T02:00:00Z").expect("observed_at"),
        captured_at: UtcTimestamp::parse("2026-08-16T02:00:01Z").expect("captured_at"),
        collector: EvidenceCollector {
            name: "memprove-repo".to_owned(),
            version: "0.1.0".to_owned(),
        },
    }
}

fn mutation_snapshot(repository: &Path) -> RepositoryMutationSnapshot {
    let git_dir = PathBuf::from(git(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let index = git_dir.join("index");
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(repository).expect("repository entries") {
        let entry = entry.expect("repository entry");
        if entry.file_name() != ".git" && entry.path().is_file() {
            files.insert(
                entry.file_name(),
                fs::read(entry.path()).expect("file bytes"),
            );
        }
    }
    RepositoryMutationSnapshot {
        head: git_output(repository, &["rev-parse", "--verify", "HEAD"]).stdout,
        status: git_output(
            repository,
            &["status", "--porcelain=v2", "--untracked-files=all"],
        )
        .stdout,
        refs: git_output(
            repository,
            &[
                "for-each-ref",
                "--sort=refname",
                "--format=%(refname)%00%(objectname)",
            ],
        )
        .stdout,
        index: index
            .is_file()
            .then(|| fs::read(index).expect("index bytes")),
        files,
    }
}

#[test]
fn captures_exact_git_state_and_selected_file_without_repository_mutation() {
    let (_directory, repository) = create_repository();
    fs::write(repository.join("tracked.txt"), b"modified fixture\n").expect("modify tracked");
    fs::write(repository.join("untracked.txt"), b"untracked fixture\n").expect("untracked");
    let before = mutation_snapshot(&repository);
    let expected_status = String::from_utf8(before.status.clone()).expect("UTF-8 status");
    let selected = SelectedFileCapture {
        evidence_id: memprove_domain::EvidenceId::new(),
        path: "repo://tracked.txt".to_owned(),
    };

    let captured =
        RepositorySnapshotCollector::capture(&repository, request(vec![selected.clone()]))
            .expect("capture");

    assert_eq!(
        captured.snapshot.head_oid(),
        git(&repository, &["rev-parse", "HEAD"])
    );
    assert_eq!(captured.snapshot.branch(), Some("main"));
    assert_eq!(
        captured.snapshot.worktree_status_porcelain_v2(),
        expected_status
    );
    assert!(captured.snapshot.is_dirty());
    assert_eq!(captured.evidence.len(), 3);
    assert_eq!(captured.evidence[0].kind(), EvidenceKind::GitHead);
    assert_eq!(captured.evidence[1].kind(), EvidenceKind::GitWorktree);
    assert_eq!(captured.evidence[2].kind(), EvidenceKind::FileDigest);
    assert_eq!(captured.evidence[2].evidence_id(), &selected.evidence_id);
    assert!(matches!(
        captured.evidence[2].payload(),
        EvidencePayload::FileDigest { sha256 }
            if sha256.as_str() == hex_digest(b"modified fixture\n")
    ));
    assert_eq!(mutation_snapshot(&repository), before);
}

#[test]
fn repository_controlled_fsmonitor_never_executes_or_mutates_the_project() {
    let (directory, repository) = create_repository();
    let marker = repository.join("fsmonitor-executed");
    let fsmonitor = directory.path().join("malicious-fsmonitor");
    fs::write(
        &fsmonitor,
        format!("#!/bin/sh\n: > '{}'\n", marker.display()),
    )
    .expect("fsmonitor script");
    let mut permissions = fs::metadata(&fsmonitor)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fsmonitor, permissions).expect("executable fsmonitor");
    let before = mutation_snapshot(&repository);
    git(
        &repository,
        &["config", "core.fsmonitor", &fsmonitor.display().to_string()],
    );

    RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("capture");

    assert!(!marker.exists(), "repository-controlled fsmonitor executed");
    git(&repository, &["config", "--unset", "core.fsmonitor"]);
    assert_eq!(mutation_snapshot(&repository), before);
}

#[test]
fn repository_controlled_filter_helper_never_executes_during_capture() {
    let (directory, repository) = create_repository();
    let marker = repository.join("filter-executed");
    let filter = directory.path().join("malicious-filter");
    fs::write(
        &filter,
        format!("#!/bin/sh\n: > '{}'\ncat\n", marker.display()),
    )
    .expect("filter script");
    let mut permissions = fs::metadata(&filter)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&filter, permissions).expect("executable filter");
    fs::write(
        repository.join(".gitattributes"),
        b"tracked.txt filter=evil\n",
    )
    .expect("attributes");
    git(&repository, &["add", ".gitattributes"]);
    git(&repository, &["commit", "-m", "add attributes"]);
    git(
        &repository,
        &["config", "filter.evil.clean", &filter.display().to_string()],
    );
    git(&repository, &["config", "filter.evil.required", "true"]);
    fs::write(repository.join("tracked.txt"), b"changed bytes\n").expect("modify tracked file");

    RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("capture");

    assert!(!marker.exists(), "repository-controlled filter executed");
}

#[test]
fn explicit_file_capture_rejects_escape_and_every_symlink_semantic() {
    let (directory, repository) = create_repository();
    let outside = directory.path().join("outside.txt");
    fs::write(&outside, b"outside").expect("outside file");
    std::os::unix::fs::symlink(&outside, repository.join("link.txt")).expect("file symlink");
    fs::create_dir(repository.join("real-dir")).expect("real dir");
    fs::write(repository.join("real-dir/nested.txt"), b"nested").expect("nested file");
    std::os::unix::fs::symlink(repository.join("real-dir"), repository.join("dir-link"))
        .expect("directory symlink");

    for path in [
        "repo://../outside.txt",
        "repo:///absolute",
        "repo://link.txt",
        "repo://dir-link/nested.txt",
    ] {
        let result = RepositorySnapshotCollector::capture(
            &repository,
            request(vec![SelectedFileCapture {
                evidence_id: memprove_domain::EvidenceId::new(),
                path: path.to_owned(),
            }]),
        );
        assert!(
            matches!(
                result,
                Err(RepoError::InvalidRepoPath(_) | RepoError::SymlinkNotAllowed(_))
            ),
            "accepted {path}: {result:?}"
        );
    }
}

#[test]
fn second_probe_rejects_head_branch_or_status_drift() {
    let (_directory, repository) = create_repository();
    let captured = RepositorySnapshotCollector::capture(&repository, request(vec![]))
        .expect("capture candidate");
    captured.probe.verify_unchanged().expect("unchanged probe");

    fs::write(repository.join("new.txt"), b"drift\n").expect("status drift");
    assert!(matches!(
        captured.probe.verify_unchanged(),
        Err(RepoError::RepositoryChanged)
    ));
}

#[test]
fn selected_untracked_content_change_invalidates_final_revalidation() {
    let (_directory, repository) = create_repository();
    fs::write(repository.join("selected.txt"), b"first\n").expect("selected file");
    let captured = RepositorySnapshotCollector::capture(
        &repository,
        request(vec![SelectedFileCapture {
            evidence_id: memprove_domain::EvidenceId::new(),
            path: "repo://selected.txt".to_owned(),
        }]),
    )
    .expect("capture selected untracked file");

    fs::write(repository.join("selected.txt"), b"other\n").expect("mutate selected file");

    assert!(matches!(
        captured.probe.verify_unchanged(),
        Err(RepoError::RepositoryChanged)
    ));
}

#[test]
fn selected_tracked_content_change_invalidates_final_revalidation() {
    let (_directory, repository) = create_repository();
    let captured = RepositorySnapshotCollector::capture(
        &repository,
        request(vec![SelectedFileCapture {
            evidence_id: memprove_domain::EvidenceId::new(),
            path: "repo://tracked.txt".to_owned(),
        }]),
    )
    .expect("capture selected tracked file");

    fs::write(repository.join("tracked.txt"), b"other fixture\n").expect("mutate tracked file");

    assert!(matches!(
        captured.probe.verify_unchanged(),
        Err(RepoError::RepositoryChanged)
    ));
}

#[test]
fn selected_file_a_b_a_schedule_rejects_the_intermediate_capture() {
    let (_directory, repository) = create_repository();
    let path = repository.join("selected.txt");
    let selected = SelectedFileCapture {
        evidence_id: memprove_domain::EvidenceId::new(),
        path: "repo://selected.txt".to_owned(),
    };
    fs::write(&path, b"A\n").expect("state A");
    let before = RepositorySnapshotCollector::capture(&repository, request(vec![selected.clone()]))
        .expect("capture state A");
    fs::write(&path, b"B\n").expect("state B");
    let intermediate = RepositorySnapshotCollector::capture(&repository, request(vec![selected]))
        .expect("capture state B");
    fs::write(&path, b"A\n").expect("restore state A");

    before.probe.verify_unchanged().expect("state A restored");
    assert!(matches!(
        intermediate.probe.verify_unchanged(),
        Err(RepoError::RepositoryChanged)
    ));
}

#[test]
fn different_dirty_submodule_heads_have_distinct_canonical_state_digests() {
    let (_directory, repository, submodule) = create_repository_with_submodule();
    fs::write(submodule.join("nested.txt"), b"dirty one\n").expect("first dirty commit");
    git(&submodule, &["add", "nested.txt"]);
    git(&submodule, &["commit", "-m", "dirty one"]);
    let first =
        RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("first capture");

    fs::write(submodule.join("nested.txt"), b"dirty two\n").expect("second dirty commit");
    git(&submodule, &["add", "nested.txt"]);
    git(&submodule, &["commit", "-m", "dirty two"]);
    let second =
        RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("second capture");

    assert_eq!(
        first.snapshot.index_digest(),
        second.snapshot.index_digest()
    );
    assert_eq!(
        first.snapshot.worktree_status_porcelain_v2(),
        second.snapshot.worktree_status_porcelain_v2()
    );
    assert_ne!(
        first.snapshot.tracked_worktree_digest(),
        second.snapshot.tracked_worktree_digest()
    );
    assert_ne!(
        first.snapshot.canonical_digest(),
        second.snapshot.canonical_digest()
    );
}

#[test]
fn different_gitlink_index_oids_have_distinct_canonical_state_digests() {
    let (_directory, repository, submodule) = create_repository_with_submodule();
    fs::write(submodule.join("nested.txt"), b"index one\n").expect("first gitlink commit");
    git(&submodule, &["add", "nested.txt"]);
    git(&submodule, &["commit", "-m", "index one"]);
    git(&repository, &["add", "nested"]);
    let first =
        RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("first capture");

    fs::write(submodule.join("nested.txt"), b"index two\n").expect("second gitlink commit");
    git(&submodule, &["add", "nested.txt"]);
    git(&submodule, &["commit", "-m", "index two"]);
    git(&repository, &["add", "nested"]);
    let second =
        RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("second capture");

    assert_ne!(
        first.snapshot.index_digest(),
        second.snapshot.index_digest()
    );
    assert_ne!(
        first.snapshot.canonical_digest(),
        second.snapshot.canonical_digest()
    );
}

#[test]
fn selected_input_order_does_not_change_canonical_state_digest() {
    let (_directory, repository) = create_repository();
    fs::write(repository.join("a.txt"), b"A\n").expect("selected A");
    fs::write(repository.join("b.txt"), b"B\n").expect("selected B");
    let selected = |path: &str| SelectedFileCapture {
        evidence_id: memprove_domain::EvidenceId::new(),
        path: path.to_owned(),
    };
    let first = RepositorySnapshotCollector::capture(
        &repository,
        request(vec![selected("repo://b.txt"), selected("repo://a.txt")]),
    )
    .expect("first capture");
    let second = RepositorySnapshotCollector::capture(
        &repository,
        request(vec![selected("repo://a.txt"), selected("repo://b.txt")]),
    )
    .expect("second capture");

    assert_eq!(
        first.snapshot.canonical_digest(),
        second.snapshot.canonical_digest()
    );
}

#[test]
fn equivalent_porcelain_shape_with_distinct_tracked_bytes_has_distinct_state_digest() {
    let (_directory, repository) = create_repository();
    fs::write(repository.join("tracked.txt"), b"first content\n").expect("first content");
    let first =
        RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("first capture");
    let status = first.snapshot.worktree_status_porcelain_v2().to_owned();

    fs::write(repository.join("tracked.txt"), b"other content\n").expect("other content");
    let second =
        RepositorySnapshotCollector::capture(&repository, request(vec![])).expect("second capture");

    assert_eq!(second.snapshot.worktree_status_porcelain_v2(), status);
    assert_eq!(
        first.snapshot.index_digest(),
        second.snapshot.index_digest()
    );
    assert_ne!(
        first.snapshot.tracked_worktree_digest(),
        second.snapshot.tracked_worktree_digest()
    );
    assert_ne!(
        first.snapshot.canonical_digest(),
        second.snapshot.canonical_digest()
    );
    assert!(matches!(
        first.probe.verify_unchanged(),
        Err(RepoError::RepositoryChanged)
    ));
}

#[test]
fn detached_capture_uses_explicit_null_branch() {
    let (_directory, repository) = create_repository();
    git(&repository, &["checkout", "--detach"]);

    let captured = RepositorySnapshotCollector::capture(&repository, request(vec![]))
        .expect("detached capture");

    assert_eq!(captured.snapshot.branch(), None);
    assert!(matches!(
        captured.evidence[0].payload(),
        EvidencePayload::GitHead { branch: None, .. }
    ));
    captured
        .probe
        .verify_unchanged()
        .expect("detached unchanged");
}

#[test]
fn non_utf8_porcelain_output_is_rejected_without_lossy_conversion() {
    let (_directory, repository) = create_repository();
    git(&repository, &["config", "core.quotePath", "false"]);
    let invalid_name = OsString::from_vec(vec![b'n', b'o', b'n', b'-', 0x80]);
    if let Err(error) = fs::write(repository.join(invalid_name), b"invalid name\n") {
        assert_eq!(
            error.raw_os_error(),
            Some(92),
            "unexpected filename error: {error}"
        );
        return;
    }

    let result = RepositorySnapshotCollector::capture(&repository, request(vec![]));
    assert!(matches!(result, Err(RepoError::NonUtf8Output(_))));
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("String write");
    }
    encoded
}
