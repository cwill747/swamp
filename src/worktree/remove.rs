use super::branches;
use super::model::{GitInfo, RemovalVerdict, VerdictContext};
use super::{RemoveRefused, RemoveRefusedReason};
use crate::worktree::status::git_info;
use anyhow::{Context, Result};
use git2::{BranchType, Oid, Repository, WorktreeLockStatus, WorktreePruneOptions};
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
    pr_state: Option<String>,
    pr_head_oid: Option<Oid>,
) -> Result<()> {
    let repo = Repository::open(common_dir)
        .with_context(|| format!("open bare repo at {}", common_dir.display()))?;
    let wt = repo
        .find_worktree(name)
        .with_context(|| format!("find worktree {name}"))?;
    let wt_path = wt.path().to_path_buf();

    if let Ok(cwd) = std::env::current_dir()
        && path_contains(&wt_path, &cwd)
    {
        return Err(RemoveRefused {
            name: name.to_string(),
            reason: RemoveRefusedReason::CurrentDirectory,
        }
        .into());
    }

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
        // A status error refuses removal (fail closed) instead of assuming
        // clean: a transient libgit2 error is exactly when an automatic
        // remove_dir_all is least wanted. This can't be folded into
        // `removal_verdict` because it happens *while building* the
        // `GitInfo` the verdict is handed, not while evaluating it.
        let info = if wt_path.exists() {
            match git_info(&wt_path) {
                Err(_) => {
                    return Err(RemoveRefused {
                        name: name.to_string(),
                        reason: RemoveRefusedReason::StatusUnreadable,
                    }
                    .into());
                }
                Ok(info) => info,
            }
        } else {
            GitInfo::default()
        };

        let default_branch = branches::default_branch_name(&repo).unwrap_or_default();
        let default_branch_tip = branches::default_branch_tip_in_repo(&repo);
        let ctx = VerdictContext {
            git_info: info,
            default_branch,
            default_branch_tip,
            pr_state,
            pr_head_oid,
        };
        if let RemovalVerdict::Blocked(reason) = removal_verdict(common_dir, name, &ctx) {
            return Err(RemoveRefused {
                name: name.to_string(),
                reason,
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

/// Compute whether `name` can be removed without `force`, and why not when it
/// can't. Performs no mutation and no I/O beyond opening the repo and, when
/// the branch has no live upstream, the bounded reachability probe — the
/// caller is expected to have already read the worktree's git status into
/// `ctx.git_info`. Used both as the authoritative pre-mutation check in
/// [`remove_worktree`] and to fill `WorktreeRow::removal_block` during a scan.
pub fn removal_verdict(common_dir: &Path, name: &str, ctx: &VerdictContext) -> RemovalVerdict {
    tracing::trace!(
        worktree = name,
        branch = %ctx.git_info.branch,
        default_branch = %ctx.default_branch,
        "computing removal verdict"
    );
    let Ok(repo) = Repository::open(common_dir) else {
        return RemovalVerdict::Blocked(RemoveRefusedReason::StatusUnreadable);
    };
    let Ok(wt) = repo.find_worktree(name) else {
        return RemovalVerdict::Blocked(RemoveRefusedReason::StatusUnreadable);
    };

    // A lock status error counts as locked (fail closed).
    let lock_status = wt.is_locked().unwrap_or(WorktreeLockStatus::Locked(None));
    if lock_status != WorktreeLockStatus::Unlocked {
        return RemovalVerdict::Blocked(RemoveRefusedReason::Locked);
    }

    let info = &ctx.git_info;
    if info.is_dirty() {
        return RemovalVerdict::Blocked(RemoveRefusedReason::Dirty);
    }

    let Ok(branch) = repo.find_branch(&info.branch, BranchType::Local) else {
        // No local branch to protect (detached HEAD, unborn, or already
        // gone) — nothing more can block removal.
        return RemovalVerdict::Removable;
    };
    let Ok(tip) = branch.get().peel_to_commit() else {
        return RemovalVerdict::Removable;
    };
    let tip = tip.id();

    if branch.upstream().is_ok() {
        // Live upstream: a "clean" working tree can still hold commits the
        // upstream never saw; branch deletion would orphan them.
        return if info.ahead > 0 {
            RemovalVerdict::Blocked(RemoveRefusedReason::UnpushedCommits)
        } else {
            RemovalVerdict::Removable
        };
    }

    // No live upstream — either it was pruned (`upstream_gone`) or never
    // configured. Both cases mirror `git branch -d`: refuse unless the tip's
    // work is already merged somewhere, so it wouldn't be orphaned.
    if is_merged(&repo, &info.branch, tip, ctx) {
        RemovalVerdict::Removable
    } else {
        RemovalVerdict::Blocked(RemoveRefusedReason::UnmergedCommits)
    }
}

/// True when `tip`'s work is already present elsewhere, so deleting the
/// branch it belongs to cannot orphan it — even when the branch's own commits
/// were rewritten by a squash or rebase merge. Checked in order from cheapest
/// to most expensive: a fast-forward/merge into the default branch, a merged
/// PR whose head still matches the local tip (the only signal that survives a
/// squash merge), and finally the bounded reachability probe against every
/// other branch.
fn is_merged(repo: &Repository, branch_name: &str, tip: Oid, ctx: &VerdictContext) -> bool {
    if reachable_from_default(repo, tip, ctx.default_branch_tip) {
        return true;
    }
    if ctx.pr_state.as_deref() == Some("MERGED") && ctx.pr_head_oid == Some(tip) {
        return true;
    }
    reachable_from_other_branch(repo, branch_name, tip, ctx.default_branch_tip)
}

/// True when `tip` is reachable from the repository default branch's tip.
fn reachable_from_default(repo: &Repository, tip: Oid, default_tip: Option<Oid>) -> bool {
    default_tip.is_some_and(|default_tip| {
        tip == default_tip || repo.graph_descendant_of(default_tip, tip).unwrap_or(false)
    })
}

/// True when `tip` is reachable from a local or remote branch other than
/// `branch_name` itself, i.e. deleting that branch cannot orphan commits.
/// Checks the default branch first and returns on that first match; only
/// falls back to walking every other branch when the default branch doesn't
/// already contain the tip, so the common "already merged to main" case
/// costs one revwalk instead of one per branch in the repository.
fn reachable_from_other_branch(
    repo: &Repository,
    branch_name: &str,
    tip: Oid,
    default_tip: Option<Oid>,
) -> bool {
    if reachable_from_default(repo, tip, default_tip) {
        return true;
    }
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

fn path_contains(root: &Path, child: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let child = child.canonicalize().unwrap_or_else(|_| child.to_path_buf());
    child == root || child.starts_with(root)
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

        remove_worktree(&bare, "feature", true, false, None, None).unwrap();
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
        let err = remove_worktree(&bare, "feature", true, false, None, None).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::Dirty);
        assert!(wt.path.exists());
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        // Forcing through discards the worktree.
        remove_worktree(&bare, "feature", true, true, None, None).unwrap();
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
        let err = remove_worktree(&bare, "feature", true, false, None, None).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::UnpushedCommits);
        assert!(wt.path.exists());
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        // Force removes successfully.
        remove_worktree(&bare, "feature", true, true, None, None).unwrap();
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

        let err = remove_worktree(&bare, "feature", true, false, None, None).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::UnmergedCommits);
        assert!(wt.path.exists());

        // A worktree at the same commit as its base branch (no new commits) is
        // still removable without force.
        let wt2 = create_worktree(&bare, "feature2").unwrap();
        remove_worktree(&bare, "feature2", true, false, None, None).unwrap();
        assert!(!wt2.path.exists());

        // Force removes the unmerged one.
        remove_worktree(&bare, "feature", true, true, None, None).unwrap();
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
        let err = remove_worktree(&bare, "feature", true, false, None, None).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::Locked);
        // The directory must still exist.
        assert!(wt.path.exists(), "directory must survive a refused removal");
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        // Force removes despite the lock.
        remove_worktree(&bare, "feature", true, true, None, None).unwrap();
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

        let err = remove_worktree(&bare, "feature", true, false, None, None).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::StatusUnreadable);
        assert!(wt.path.exists());
        assert_eq!(list_worktrees(&bare).unwrap().len(), 1);

        remove_worktree(&bare, "feature", true, true, None, None).unwrap();
        assert!(!wt.path.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The bounded reachability probe finds a tip merged into the default
    /// branch without needing any other branch to exist, and still finds a
    /// tip merged only into some other branch when the default doesn't
    /// contain it.
    #[test]
    fn reachable_from_other_branch_bounds_the_probe() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let repo = Repository::open(&bare).unwrap();
        let main_tip = repo
            .find_branch("main", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap()
            .id();

        // Default-branch case: a branch whose tip *is* main's tip is
        // reachable via the default-branch check alone — no other local
        // branch exists for the walk to fall back to.
        run(&bare, &["branch", "on-main", "main"]);
        assert!(reachable_from_other_branch(
            &repo,
            "on-main",
            main_tip,
            Some(main_tip),
        ));

        // Non-default case: "topic" gains a commit main doesn't have, so the
        // default check fails; the probe must fall back to the walk and find
        // it via "mirror", a different branch at the same tip.
        let topic_wt = create_worktree(&bare, "topic").unwrap();
        std::fs::write(topic_wt.path.join("f.txt"), "x").unwrap();
        run(&topic_wt.path, &["add", "f.txt"]);
        run(&topic_wt.path, &["commit", "-m", "topic work"]);
        let topic_tip = Repository::open(&topic_wt.path)
            .unwrap()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id();
        run(&bare, &["branch", "mirror", "topic"]);

        assert!(reachable_from_other_branch(
            &repo,
            "topic",
            topic_tip,
            Some(main_tip),
        ));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A branch whose earlier commits are merged into the default branch but
    /// which has since gained a *new* local commit is still refused while it
    /// has a live upstream: merge signals only ever allow, never override a
    /// non-zero `ahead`.
    #[test]
    fn remove_refuses_post_merge_commits_with_live_upstream() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        std::fs::write(wt.path.join("work.txt"), "v1").unwrap();
        run(&wt.path, &["add", "work.txt"]);
        run(&wt.path, &["commit", "-m", "the merged work"]);

        // Fast-forward main to feature's tip — equivalent to a ff-merge —
        // via a direct ref update, since the bare repo has no working tree.
        let feature_tip = Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .args(["rev-parse", "feature"])
            .output()
            .unwrap()
            .stdout;
        let feature_tip = String::from_utf8(feature_tip).unwrap();
        run(
            &bare,
            &["update-ref", "refs/heads/main", feature_tip.trim()],
        );

        // Give feature a live upstream at its now-merged tip.
        run(&bare, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&bare, &["fetch", "origin"]);
        run(&wt.path, &["push", "--set-upstream", "origin", "feature"]);

        // A further commit, never pushed.
        std::fs::write(wt.path.join("more.txt"), "v2").unwrap();
        run(&wt.path, &["add", "more.txt"]);
        run(&wt.path, &["commit", "-m", "more work"]);

        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.ahead, 1, "the new commit must count as ahead");

        let err = remove_worktree(&bare, "feature", true, false, None, None).unwrap_err();
        let refused = err.downcast_ref::<RemoveRefused>().unwrap();
        assert_eq!(refused.reason, RemoveRefusedReason::UnpushedCommits);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A branch whose remote-tracking ref was pruned after a squash merge —
    /// so its commits are unreachable from anywhere by graph walk — is
    /// removable without force once its most recent PR is `MERGED`, and
    /// still refused without that signal.
    #[test]
    fn removal_verdict_allows_squash_merged_branch_with_merged_pr() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();
        std::fs::write(wt.path.join("work.txt"), "v1").unwrap();
        run(&wt.path, &["add", "work.txt"]);
        run(&wt.path, &["commit", "-m", "squashed elsewhere"]);

        // Simulate "pushed, squash-merged on GitHub, then pruned": an
        // upstream was configured but its remote-tracking ref is gone.
        run(&bare, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&wt.path, &["push", "--set-upstream", "origin", "feature"]);
        run(&bare, &["update-ref", "-d", "refs/remotes/origin/feature"]);

        let info = git_info(&wt.path).unwrap();
        assert!(info.upstream_gone, "upstream ref must be pruned");
        assert!(!info.is_dirty());

        // Without a PR signal, the squashed commits are unreachable anywhere
        // — still refused.
        let ctx_no_pr = VerdictContext {
            git_info: info,
            default_branch: "main".into(),
            default_branch_tip: None,
            pr_state: None,
            pr_head_oid: None,
        };
        assert_eq!(
            removal_verdict(&bare, "feature", &ctx_no_pr),
            RemovalVerdict::Blocked(RemoveRefusedReason::UnmergedCommits),
        );

        // A merged PR is the signal that unblocks it.
        let ctx_merged_pr = VerdictContext {
            pr_state: Some("MERGED".into()),
            pr_head_oid: Some(
                Repository::open(&wt.path)
                    .unwrap()
                    .head()
                    .unwrap()
                    .target()
                    .unwrap(),
            ),
            ..ctx_no_pr
        };
        assert_eq!(
            removal_verdict(&bare, "feature", &ctx_merged_pr),
            RemovalVerdict::Removable,
        );

        remove_worktree(
            &bare,
            "feature",
            true,
            false,
            ctx_merged_pr.pr_state,
            ctx_merged_pr.pr_head_oid,
        )
        .unwrap();
        assert!(!wt.path.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merged_pr_does_not_cover_a_later_local_commit() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();
        std::fs::write(wt.path.join("work.txt"), "merged").unwrap();
        run(&wt.path, &["add", "work.txt"]);
        run(&wt.path, &["commit", "-m", "merged work"]);
        let merged_pr_head = Repository::open(&wt.path)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap();

        run(&bare, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&wt.path, &["push", "--set-upstream", "origin", "feature"]);
        run(&bare, &["update-ref", "-d", "refs/remotes/origin/feature"]);

        std::fs::write(wt.path.join("later.txt"), "not merged").unwrap();
        run(&wt.path, &["add", "later.txt"]);
        run(&wt.path, &["commit", "-m", "later local work"]);

        let ctx = VerdictContext {
            git_info: git_info(&wt.path).unwrap(),
            default_branch: "main".into(),
            default_branch_tip: None,
            pr_state: Some("MERGED".into()),
            pr_head_oid: Some(merged_pr_head),
        };
        assert_eq!(
            removal_verdict(&bare, "feature", &ctx),
            RemovalVerdict::Blocked(RemoveRefusedReason::UnmergedCommits),
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
