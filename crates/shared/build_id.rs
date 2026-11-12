// U1 (E1): the build identity of `memprove` and `memproved`, worked out
// by each crate's build script and compiled in as `MEMPROVE_BUILD_ID`.
//
// The identity is the first 12 characters of the commit the workspace is
// checked out at, with `-dirty` when `git status` lists any change (an
// untracked file included), or `unknown` when the workspace is not the top
// of a Git worktree or `git` cannot answer. Only local `git` commands run;
// nothing touches the network and nothing is written: `--no-optional-locks`
// keeps `git status` from refreshing the index, and the identity reaches the
// compiler as an environment variable, not a file.
//
// This file is `include!`d by `crates/memprove-cli/build.rs`,
// `crates/memprove-daemon/build.rs`, and the CLI's unit tests.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The identity printed when no commit can be determined.
const UNKNOWN_BUILD: &str = "unknown";

/// Runs `git` in `root` with the repository taken from `root` alone (the
/// variables that redirect Git elsewhere are removed); its trimmed standard
/// output when it succeeds.
fn git_output(root: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .args(arguments)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim_end_matches(['\n', '\r']).to_owned())
}

/// Whether `root` is the top of a Git worktree: a workspace unpacked inside
/// some other repository is not built from that repository's commit.
fn is_worktree_top(root: &Path) -> bool {
    let Some(top) = git_output(root, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    match (Path::new(&top).canonicalize(), root.canonicalize()) {
        (Ok(top), Ok(root)) => top == root,
        _ => false,
    }
}

/// The build identity of the workspace at `root`: `<12 hex>`,
/// `<12 hex>-dirty`, or `unknown`.
fn build_id_at(root: &Path) -> String {
    if !is_worktree_top(root) {
        return UNKNOWN_BUILD.to_owned();
    }
    let Some(commit) = git_output(root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
    else {
        return UNKNOWN_BUILD.to_owned();
    };
    if commit.len() < 12 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return UNKNOWN_BUILD.to_owned();
    }
    let short = commit[..12].to_ascii_lowercase();
    match git_output(root, &["status", "--porcelain", "--untracked-files=normal"]) {
        Some(status) if status.is_empty() => short,
        Some(_) => format!("{short}-dirty"),
        None => UNKNOWN_BUILD.to_owned(),
    }
}

/// The files whose change can change the identity, as absolute paths that
/// exist: the worktree's `HEAD` and index, the branch `HEAD` names, the
/// packed refs, and the `.git` entry of the worktree (a file in a linked
/// worktree). Paths come from `git rev-parse --git-path`, which knows where
/// a linked worktree keeps each of them.
fn watched_git_paths(root: &Path) -> Vec<PathBuf> {
    let mut names = vec![
        "HEAD".to_owned(),
        "index".to_owned(),
        "packed-refs".to_owned(),
    ];
    if let Some(reference) = git_output(root, &["symbolic-ref", "--quiet", "HEAD"]) {
        names.push(reference);
    }
    let mut paths = vec![root.join(".git")];
    for name in names {
        if let Some(path) = git_output(root, &["rev-parse", "--git-path", &name]) {
            paths.push(root.join(path));
        }
    }
    paths.retain(|path| path.exists());
    paths
}

/// What a build script prints: the identity, and when to work it out
/// again. Besides the Git files, the sources of every crate and the
/// workspace manifest and lock file are watched, so an edit there refreshes
/// `-dirty`; a change elsewhere in the worktree (documentation, say) does
/// not until one of those changes too.
#[allow(dead_code)] // The unit tests include this file without calling it.
fn emit_build_id() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let root = manifest_dir.join("../..");
    let root = root.canonicalize().unwrap_or(root);
    println!("cargo:rustc-env=MEMPROVE_BUILD_ID={}", build_id_at(&root));
    if is_worktree_top(&root) {
        for path in watched_git_paths(&root) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    for relative in ["crates", "Cargo.toml", "Cargo.lock"] {
        let path = root.join(relative);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=memprove Test"])
            .args(["-c", "user.email=memprove@example.invalid"])
            .args(arguments)
            .current_dir(directory)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn repository(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(repo.join("README"), "fixture\n").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "--quiet", "-m", "fixture"]);
        repo
    }

    fn head(repo: &Path) -> String {
        git_output(repo, &["rev-parse", "HEAD"]).unwrap()[..12].to_owned()
    }

    #[test]
    fn a_directory_outside_git_is_unknown() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(build_id_at(root.path()), "unknown");
        assert!(watched_git_paths(root.path()).is_empty());
    }

    #[test]
    fn a_clean_worktree_is_its_commit_and_a_change_makes_it_dirty() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        let commit = head(&repo);
        assert_eq!(build_id_at(&repo), commit);
        fs::write(repo.join("README"), "changed\n").unwrap();
        assert_eq!(build_id_at(&repo), format!("{commit}-dirty"));
        git(&repo, &["checkout", "--quiet", "--", "README"]);
        fs::write(repo.join("untracked"), "new\n").unwrap();
        assert_eq!(build_id_at(&repo), format!("{commit}-dirty"));
    }

    #[test]
    fn a_directory_below_the_top_of_a_worktree_is_unknown() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        let nested = repo.join("nested");
        fs::create_dir(&nested).unwrap();
        assert_eq!(build_id_at(&nested), "unknown");
    }

    #[test]
    fn a_repository_without_a_commit_is_unknown() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--quiet"]);
        assert_eq!(build_id_at(root.path()), "unknown");
    }

    #[test]
    fn a_linked_worktree_watches_its_own_head_and_index_and_the_shared_branch() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        let linked = root.path().join("linked");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "side",
                linked.to_str().unwrap(),
            ],
        );
        assert!(linked.join(".git").is_file());
        assert_eq!(build_id_at(&linked), head(&repo));
        let watched = watched_git_paths(&linked);
        let common = repo.join(".git").canonicalize().unwrap();
        let canonical: Vec<PathBuf> = watched
            .iter()
            .map(|path| path.canonicalize().unwrap())
            .collect();
        assert!(canonical.contains(&linked.join(".git").canonicalize().unwrap()));
        assert!(
            canonical.contains(&common.join("worktrees/linked/HEAD")),
            "{watched:?}"
        );
        assert!(
            canonical.contains(&common.join("worktrees/linked/index")),
            "{watched:?}"
        );
        assert!(
            canonical.contains(&common.join("refs/heads/side")),
            "{watched:?}"
        );
        assert!(!canonical.contains(&common.join("HEAD")), "{watched:?}");
    }
}
