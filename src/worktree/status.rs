use crate::worktree::model::GitInfo;
use anyhow::Result;
use git2::{BranchType, Repository, RepositoryState, Status, StatusOptions};
use std::path::Path;

/// Collect git status for a single worktree at `dir`.
pub fn git_info(dir: &Path) -> Result<GitInfo> {
    git_info_with_status_mode(dir, StatusMode::Tolerant)
}

/// Collect git status for a single worktree, returning an error if working-tree
/// status cannot be read.
pub(super) fn git_info_strict(dir: &Path) -> Result<GitInfo> {
    git_info_with_status_mode(dir, StatusMode::Strict)
}

enum StatusMode {
    Tolerant,
    Strict,
}

fn git_info_with_status_mode(dir: &Path, status_mode: StatusMode) -> Result<GitInfo> {
    let repo = Repository::open(dir)?;
    let mut info = GitInfo::default();

    let detached = repo.head_detached().unwrap_or(false);
    let head = repo.head().ok();

    info.branch = if head.is_none() {
        "(unborn)".into()
    } else if detached {
        "(detached)".into()
    } else {
        head.as_ref()
            .and_then(|h| h.shorthand().ok())
            .unwrap_or("(detached)")
            .to_string()
    };

    if let Some(commit) = head.as_ref().and_then(|h| h.peel_to_commit().ok()) {
        info.head_ts = commit.time().seconds().max(0) as u64;
    }

    // Upstream tracking + ahead/behind.
    if !detached
        && info.branch != "(unborn)"
        && let (Some(local_oid), Ok(branch)) = (
            head.as_ref().and_then(|h| h.target()),
            repo.find_branch(&info.branch, BranchType::Local),
        )
    {
        let refname = format!("refs/heads/{}", info.branch);
        let upstream_name = repo
            .branch_upstream_name(&refname)
            .ok()
            .and_then(|buf| buf.as_str().ok().map(short_upstream_name));
        if let Ok(upstream) = branch.upstream() {
            info.upstream = upstream
                .name()
                .ok()
                .flatten()
                .map(String::from)
                .or(upstream_name);
            if let Some(up_oid) = upstream.get().target()
                && let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, up_oid)
            {
                info.ahead = ahead as u32;
                info.behind = behind as u32;
            }
        } else if let Some(name) = upstream_name {
            info.upstream = Some(name);
            info.upstream_gone = true;
        }
    }

    fn short_upstream_name(name: &str) -> String {
        name.strip_prefix("refs/remotes/")
            .unwrap_or(name)
            .to_string()
    }

    // Working-tree status counts.
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .exclude_submodules(true)
        .include_ignored(false);
    let statuses = match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => Some(statuses),
        Err(err) if matches!(status_mode, StatusMode::Tolerant) => {
            let _ = err;
            None
        }
        Err(err) => return Err(err.into()),
    };
    if let Some(statuses) = statuses {
        for entry in statuses.iter() {
            let s = entry.status();
            if s.intersects(
                Status::INDEX_NEW
                    | Status::INDEX_MODIFIED
                    | Status::INDEX_DELETED
                    | Status::INDEX_RENAMED
                    | Status::INDEX_TYPECHANGE,
            ) {
                info.staged += 1;
            }
            if s.intersects(
                Status::WT_MODIFIED
                    | Status::WT_DELETED
                    | Status::WT_TYPECHANGE
                    | Status::WT_RENAMED,
            ) {
                info.unstaged += 1;
            }
            if s.contains(Status::WT_NEW) {
                info.untracked += 1;
            }
            if s.contains(Status::CONFLICTED) {
                info.conflict = true;
            }
        }
    }

    info.rebase = matches!(
        repo.state(),
        RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge
    );

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::create_worktree;
    use crate::worktree::test_support::{git_available, run, setup};

    #[test]
    fn git_info_reports_clean_worktree() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.branch, "feature");
        assert_eq!(info.untracked, 0);
        assert_eq!(info.staged, 0);
        assert!(!info.conflict && !info.rebase);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_info_counts_untracked_then_staged() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "work").unwrap();

        std::fs::write(wt.path.join("a.txt"), "hi").unwrap();
        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.untracked, 1);
        assert_eq!(info.staged, 0);

        run(&wt.path, &["add", "a.txt"]);
        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.staged, 1);
        assert_eq!(info.untracked, 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn git_info_distinguishes_deleted_upstream() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();

        run(&bare, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&wt.path, &["push", "--set-upstream", "origin", "feature"]);
        run(&bare, &["update-ref", "-d", "refs/remotes/origin/feature"]);

        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.upstream.as_deref(), Some("origin/feature"));
        assert!(info.upstream_gone);
        assert_eq!(info.ahead, 0);
        assert_eq!(info.behind, 0);

        let _ = std::fs::remove_dir_all(&root);
    }
}
