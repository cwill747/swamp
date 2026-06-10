use super::{RemoveRefused, RemoveRefusedReason};
use crate::worktree::status::git_info_strict;
use anyhow::{Context, Result};
use git2::{BranchType, Repository, WorktreeLockStatus, WorktreePruneOptions};
use std::fs;
use std::path::Path;

/// Remove the worktree named `name`: delete its directory, prune the git
/// metadata, and (when `delete_branch`) delete its local branch. Mirrors
/// `git wt remove`. Adapted from git-workon's `prune_worktree`.
///
/// Unless `force` is set, removal is refused (returning a [`RemoveRefused`]
/// error) when any of the following are true:
///
/// - Status lookup fails (corrupt index, permission error, …).
/// - The worktree has uncommitted / untracked changes.
/// - The branch has commits not yet pushed to its upstream.
/// - The branch has no upstream and its tip is not reachable from any other
///   branch (deleting it would orphan those commits).
/// - The worktree is locked.
///
/// All checks run **before** any filesystem mutation so that `remove_dir_all`
/// never discards local work silently.
///
/// When `force` is true every check is skipped and locked worktrees are pruned
/// with libgit2's `valid + locked` prune options.
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

    // Capture the branch before we tear the worktree down (also needed by the
    // pre-removal safety checks below).
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

    if !force {
        // A lock status error counts as locked (fail closed).
        let lock_status = wt.is_locked().unwrap_or(WorktreeLockStatus::Locked(None));
        if lock_status != WorktreeLockStatus::Unlocked {
            return Err(RemoveRefused {
                name: name.to_string(),
                reason: RemoveRefusedReason::Locked,
            }
            .into());
        }

        if wt_path.exists() {
            // A status error refuses removal (fail closed) instead of assuming
            // clean: a transient libgit2 error is exactly when an automatic
            // remove_dir_all is least wanted.
            match git_info_strict(&wt_path) {
                Err(_) => {
                    return Err(RemoveRefused {
                        name: name.to_string(),
                        reason: RemoveRefusedReason::StatusUnreadable,
                    }
                    .into());
                }
                Ok(info) if info.is_dirty() => {
                    return Err(RemoveRefused {
                        name: name.to_string(),
                        reason: RemoveRefusedReason::Dirty,
                    }
                    .into());
                }
                // A "clean" working tree can still hold commits the upstream
                // never saw; branch deletion would orphan them.
                Ok(info) if info.ahead > 0 => {
                    return Err(RemoveRefused {
                        name: name.to_string(),
                        reason: RemoveRefusedReason::UnpushedCommits,
                    }
                    .into());
                }
                Ok(_) => {}
            }
        }

        // A branch with no upstream always reports `ahead == 0`, yet deleting
        // it can still orphan commits — the common case for agent branches
        // that were committed but never pushed. Mirror `git branch -d`: refuse
        // unless the tip is reachable from some other branch.
        if let Some(branch_name) = branch.as_deref()
            && let Ok(b) = repo.find_branch(branch_name, BranchType::Local)
            && b.upstream().is_err()
            && let Ok(tip) = b.get().peel_to_commit()
            && !reachable_from_other_branch(&repo, branch_name, tip.id())
        {
            return Err(RemoveRefused {
                name: name.to_string(),
                reason: RemoveRefusedReason::UnmergedCommits,
            }
            .into());
        }
    }

    if wt_path.exists() {
        fs::remove_dir_all(&wt_path)
            .with_context(|| format!("remove worktree dir {}", wt_path.display()))?;
    }

    let mut opts = WorktreePruneOptions::new();
    opts.valid(true);
    // Forced removals may target locked worktrees; without this the prune
    // fails and leaves stale metadata behind after remove_dir_all.
    if force {
        opts.locked(true);
    }
    wt.prune(Some(&mut opts))
        .with_context(|| format!("prune worktree {name}"))?;

    if let Some(branch) = branch
        && let Ok(mut b) = repo.find_branch(&branch, BranchType::Local)
    {
        let _ = b.delete();
    }

    Ok(())
}

/// True when `tip` is reachable from a local or remote branch other than
/// `branch_name` itself, i.e. deleting that branch cannot orphan commits.
fn reachable_from_other_branch(repo: &Repository, branch_name: &str, tip: git2::Oid) -> bool {
    let Ok(branches) = repo.branches(None) else {
        return false;
    };
    for (other, kind) in branches.flatten() {
        if kind == BranchType::Local && other.name().ok().flatten() == Some(branch_name) {
            continue;
        }
        let Ok(commit) = other.get().peel_to_commit() else {
            continue;
        };
        if commit.id() == tip || repo.graph_descendant_of(commit.id(), tip).unwrap_or(false) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::test_support::{git_available, run, setup};
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
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::Dirty);
        assert!(wt.path.exists());
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        // Forcing through discards the worktree.
        remove_worktree(&bare, "feature", true, true).unwrap();
        assert!(!wt.path.exists());
        assert!(list_worktrees(&bare).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A worktree whose branch has unpushed commits is refused without force
    /// and removed cleanly with force.
    #[test]
    fn remove_refuses_ahead_without_force() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        // Commit something in the worktree — it has no upstream, so `ahead` is
        // zero. We need an upstream to get a non-zero ahead count.
        // Set up origin in the bare repo pointing at itself, then push the
        // feature branch so there's an upstream to be ahead of.
        Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .args(["remote", "add", "origin", bare.to_str().unwrap()])
            .output()
            .unwrap();
        Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .args(["fetch", "origin"])
            .output()
            .unwrap();
        // Push feature so it has an upstream at origin/feature.
        run(&wt.path, &["push", "--set-upstream", "origin", "feature"]);

        // Now make a commit on the worktree branch — making it 1 ahead.
        std::fs::write(wt.path.join("new.txt"), "content").unwrap();
        run(&wt.path, &["add", "new.txt"]);
        run(&wt.path, &["commit", "-m", "wip"]);

        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.ahead, 1, "should be 1 ahead of origin/feature");
        assert!(!info.is_dirty(), "working tree should be clean");

        // Non-forced removal must be refused.
        let err = remove_worktree(&bare, "feature", true, false).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::UnpushedCommits);
        assert!(wt.path.exists());
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        // Force removes successfully.
        remove_worktree(&bare, "feature", true, true).unwrap();
        assert!(!wt.path.exists());
        assert!(list_worktrees(&bare).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A branch with no upstream and commits reachable from no other branch is
    /// refused without force: `ahead` is 0 by definition, but deleting the
    /// branch would orphan the commits.
    #[test]
    fn remove_refuses_unmerged_no_upstream_without_force() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        // Commit on the branch without ever pushing — no upstream, ahead == 0.
        std::fs::write(wt.path.join("new.txt"), "content").unwrap();
        run(&wt.path, &["add", "new.txt"]);
        run(&wt.path, &["commit", "-m", "wip"]);

        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.ahead, 0, "no upstream, so ahead must be 0");
        assert!(!info.is_dirty());

        let err = remove_worktree(&bare, "feature", true, false).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::UnmergedCommits);
        assert!(wt.path.exists());

        // A worktree at the same commit as its base branch (no new commits) is
        // still removable without force.
        let wt2 = create_worktree(&bare, "feature2").unwrap();
        remove_worktree(&bare, "feature2", true, false).unwrap();
        assert!(!wt2.path.exists());

        // Force removes the unmerged one.
        remove_worktree(&bare, "feature", true, true).unwrap();
        assert!(!wt.path.exists());
        assert!(list_worktrees(&bare).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A locked worktree is refused without force; its directory must survive.
    /// With force it is removed despite the lock.
    #[test]
    fn remove_refuses_locked_without_force() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        // Lock the worktree via git CLI.
        let locked = Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .args(["worktree", "lock", wt.path.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success();
        assert!(locked, "git worktree lock should succeed");

        // Non-forced removal must be refused.
        let err = remove_worktree(&bare, "feature", true, false).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::Locked);
        // The directory must still exist.
        assert!(wt.path.exists(), "directory must survive a refused removal");
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        // Force removes despite the lock.
        remove_worktree(&bare, "feature", true, true).unwrap();
        assert!(!wt.path.exists());
        assert!(list_worktrees(&bare).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_refuses_unreadable_status_without_force() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        let output = Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .args(["rev-parse", "--git-dir"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let git_dir = String::from_utf8(output.stdout).unwrap();
        let git_dir = wt.path.join(git_dir.trim());
        std::fs::write(git_dir.join("index"), "not a git index").unwrap();

        let err = remove_worktree(&bare, "feature", true, false).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::StatusUnreadable);
        assert!(wt.path.exists());
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        remove_worktree(&bare, "feature", true, true).unwrap();
        assert!(!wt.path.exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
