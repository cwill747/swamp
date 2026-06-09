use crate::worktree::model::DirtyWorktree;
use crate::worktree::status::git_info;
use anyhow::{Context, Result};
use git2::{BranchType, Repository, WorktreePruneOptions};
use std::fs;
use std::path::Path;

/// Remove the worktree named `name`: delete its directory, prune the git
/// metadata, and (when `delete_branch`) delete its local branch. Mirrors
/// `git wt remove`. Adapted from git-workon's `prune_worktree`.
///
/// Unless `force` is set, a worktree with uncommitted or untracked changes is
/// refused (returning a [`DirtyWorktree`] error) so `remove_dir_all` can't
/// silently discard local work - the caller is expected to surface this and let
/// the user opt into a forced removal.
pub fn remove_worktree(
    common_dir: &Path,
    name: &str,
    delete_branch: bool,
    force: bool,
) -> Result<()> {
    let repo = Repository::open(common_dir)
        .with_context(|| format!("open bare repo at {}", common_dir.display()))?;
    let wt = repo
        .find_worktree(name)
        .with_context(|| format!("find worktree {name}"))?;
    let wt_path = wt.path().to_path_buf();

    // Guard against destroying uncommitted work. A failure to read status is
    // treated as "assume clean" so a removal is never blocked by a transient
    // libgit2 error; the force path skips the check entirely.
    if !force
        && wt_path.exists()
        && let Ok(info) = git_info(&wt_path)
        && info.is_dirty()
    {
        return Err(DirtyWorktree {
            name: name.to_string(),
        }
        .into());
    }

    // Capture the branch before we tear the worktree down.
    let branch = if delete_branch {
        Repository::open(&wt_path)
            .ok()
            .filter(|r| !r.head_detached().unwrap_or(true))
            .and_then(|r| {
                r.head()
                    .ok()
                    .and_then(|h| h.shorthand().ok().map(String::from))
            })
    } else {
        None
    };

    if wt_path.exists() {
        fs::remove_dir_all(&wt_path)
            .with_context(|| format!("remove worktree dir {}", wt_path.display()))?;
    }

    let mut opts = WorktreePruneOptions::new();
    opts.valid(true);
    wt.prune(Some(&mut opts))
        .with_context(|| format!("prune worktree {name}"))?;

    if let Some(branch) = branch
        && let Ok(mut b) = repo.find_branch(&branch, BranchType::Local)
    {
        let _ = b.delete();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::test_support::{git_available, setup};
    use crate::worktree::{create_worktree, git_info, list_worktrees};
    use std::process::Command;

    #[test]
    fn removes_worktree_and_branch() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        remove_worktree(&bare, "feature", true, false).unwrap();
        assert!(list_worktrees(&bare).unwrap().is_empty());
        assert!(!wt.path.exists());
        let branch_exists = Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .args(["rev-parse", "--verify", "-q", "refs/heads/feature"])
            .output()
            .unwrap()
            .status
            .success();
        assert!(!branch_exists, "branch feature should be deleted");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_refuses_dirty_without_force() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        // Leave an untracked file so the worktree is dirty.
        std::fs::write(wt.path.join("scratch.txt"), "wip").unwrap();
        assert!(git_info(&wt.path).unwrap().is_dirty());

        // Non-forced removal is refused and leaves everything in place.
        let err = remove_worktree(&bare, "feature", true, false).unwrap_err();
        assert!(err.downcast_ref::<DirtyWorktree>().is_some());
        assert!(wt.path.exists());
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        // Forcing through discards the worktree.
        remove_worktree(&bare, "feature", true, true).unwrap();
        assert!(!wt.path.exists());
        assert!(list_worktrees(&bare).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }
}
