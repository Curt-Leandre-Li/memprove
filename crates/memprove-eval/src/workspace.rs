//! One trial's isolated worktree: created detached at the task's base
//! commit, diffed after the agent, patched for the check, removed at the
//! end. Used by the runner and by `mine --verify`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{DiffStat, EvalError, git::git};

/// A detached worktree of a repository, removed by [`Worktree::remove`].
#[derive(Debug)]
pub struct Worktree {
    path: PathBuf,
    repo_root: PathBuf,
}

impl Worktree {
    /// Adds a detached worktree of `repo_root` at `commit`, at
    /// `parent_dir/name` (which must not exist; `parent_dir` is created).
    ///
    /// # Errors
    /// When `git worktree add` fails.
    pub fn create(
        repo_root: &Path,
        parent_dir: &Path,
        name: &str,
        commit: &str,
    ) -> Result<Self, EvalError> {
        fs::create_dir_all(parent_dir).map_err(|error| EvalError::io(parent_dir, error))?;
        let path = parent_dir.join(name);
        let target = path.to_string_lossy().into_owned();
        git(
            repo_root,
            &["worktree", "add", "--detach", "--quiet", &target, commit],
        )?;
        Ok(Self {
            path,
            repo_root: repo_root.to_path_buf(),
        })
    }

    /// The worktree's root.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes everything the worktree holds that differs from `base`
    /// (commits the agent made, staged and unstaged changes, new files) as
    /// a binary diff to `out`, and counts it. The index is left with every
    /// change staged.
    ///
    /// # Errors
    /// When `git` fails or `out` cannot be written.
    pub fn diff(&self, base: &str, out: &Path) -> Result<DiffStat, EvalError> {
        git(&self.path, &["add", "--all"])?;
        let patch = git(&self.path, &["diff", "--cached", "--binary", base])?;
        fs::write(out, &patch).map_err(|error| EvalError::io(out, error))?;
        let numstat = git(&self.path, &["diff", "--cached", "--numstat", base])?;
        let mut stat = DiffStat {
            sha256: crate::hex(&Sha256::digest(patch.as_bytes())),
            ..DiffStat::default()
        };
        for line in numstat.lines() {
            let mut fields = line.split('\t');
            let insertions = fields.next().and_then(|field| field.parse::<u64>().ok());
            let deletions = fields.next().and_then(|field| field.parse::<u64>().ok());
            stat.files += 1;
            stat.insertions += insertions.unwrap_or(0);
            stat.deletions += deletions.unwrap_or(0);
        }
        Ok(stat)
    }

    /// Applies `patch` (a file outside the worktree) to the worktree's
    /// files; a patch that does not apply cleanly changes nothing.
    ///
    /// # Errors
    /// [`EvalError::Git`] when it does not apply.
    pub fn apply(&self, patch: &Path) -> Result<(), EvalError> {
        let patch = patch.to_string_lossy().into_owned();
        git(
            &self.path,
            &["apply", "--binary", "--whitespace=nowarn", &patch],
        )
        .map(|_| ())
    }

    /// Removes the worktree and its administrative files.
    ///
    /// # Errors
    /// When `git worktree remove` fails.
    pub fn remove(self) -> Result<(), EvalError> {
        let target = self.path.to_string_lossy().into_owned();
        git(&self.repo_root, &["worktree", "remove", "--force", &target]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "user.email", "eval@example.invalid"],
            &["config", "user.name", "eval"],
        ] {
            git(root, args).expect("git");
        }
        fs::write(root.join("a.txt"), "one\n").expect("write");
        git(root, &["add", "a.txt"]).expect("add");
        git(root, &["commit", "--quiet", "-m", "one"]).expect("commit");
        dir
    }

    #[test]
    fn diffs_commits_and_new_files_then_applies_and_removes() {
        let repo = repo();
        let base = crate::git::commit(repo.path(), "HEAD").expect("head");
        let parent = repo.path().join(".git").join("u23-test");
        let tree = Worktree::create(repo.path(), &parent, "t1", &base).expect("worktree");
        fs::write(tree.path().join("a.txt"), "one\ntwo\n").expect("edit");
        git(tree.path(), &["commit", "--quiet", "-am", "agent commit"]).expect("agent commits");
        fs::write(tree.path().join("b.txt"), "new\n").expect("new file");
        let out = repo.path().join("agent.diff");
        let stat = tree.diff(&base, &out).expect("diff");
        assert_eq!((stat.files, stat.insertions, stat.deletions), (2, 2, 0));
        assert_eq!(stat.sha256.len(), 64);

        let check = repo.path().join("check.patch");
        fs::write(
            &check,
            "diff --git a/c.txt b/c.txt\nnew file mode 100644\n--- /dev/null\n+++ b/c.txt\n@@ -0,0 +1 @@\n+check\n",
        )
        .expect("patch");
        tree.apply(&check).expect("applies");
        assert!(tree.path().join("c.txt").exists());
        assert!(tree.apply(&check).is_err(), "a second apply conflicts");

        let path = tree.path().to_path_buf();
        tree.remove().expect("removed");
        assert!(!path.exists());
    }
}
