use crate::worktree::model::{BranchInfo, BranchKind, Worktree};
use crate::worktree::repo::open_lenient;
use anyhow::Result;
use git2::{BranchType, Repository};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Resolve the branch label for the worktree rooted at `path`. A detached HEAD
/// yields a `detached@<sha>` label, matching the prior porcelain output.
fn worktree_branch(path: &Path) -> String {
    let Ok(repo) = Repository::open(path) else {
        return String::new();
    };
    if !repo.head_detached().unwrap_or(false)
        && let Some(name) = repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from))
    {
        return name;
    }
    let head = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string())
        .unwrap_or_default();
    if head.is_empty() {
        return String::new();
    }
    let sha = if head.len() >= 7 { &head[..7] } else { &head };
    format!("detached@{}", sha)
}

/// List the worktrees for the repository at `dir`.
///
/// Linked worktrees come from libgit2's worktree registry; the bare repo itself
/// is excluded (it has no working tree). For a non-bare repo the main working
/// tree is included for parity with `git worktree list`.
pub fn list_worktrees(dir: &Path) -> Result<Vec<Worktree>> {
    let repo = open_lenient(dir)?;
    let mut wts = Vec::new();

    for name in repo.worktrees()?.iter().flatten() {
        let Ok(wt) = repo.find_worktree(name) else {
            continue;
        };
        let path = wt.path().to_path_buf();
        if !path.exists() {
            continue;
        }
        let branch = worktree_branch(&path);
        wts.push(Worktree { path, branch });
    }

    if !repo.is_bare()
        && let Some(workdir) = repo.workdir()
    {
        let path = workdir.to_path_buf();
        let branch = worktree_branch(&path);
        wts.push(Worktree { path, branch });
    }

    Ok(wts)
}

/// Resolve the default branch *name* (e.g. "main") from
/// `refs/remotes/origin/HEAD`.
pub(super) fn default_branch_name(repo: &Repository) -> Option<String> {
    let r = repo.find_reference("refs/remotes/origin/HEAD").ok()?;
    let target = r.symbolic_target()?;
    target
        .strip_prefix("refs/remotes/origin/")
        .map(String::from)
}

/// Detect the default branch name, falling back to "main".
pub fn default_branch(dir: &Path) -> String {
    open_lenient(dir)
        .ok()
        .and_then(|r| default_branch_name(&r))
        .unwrap_or_else(|| "main".into())
}

/// Find the worktree tracking the default branch (the one `git wt update`
/// syncs). Falls back to the first worktree if no match.
pub fn find_default_worktree<'a>(worktrees: &'a [Worktree], dir: &Path) -> Option<&'a Worktree> {
    let default = default_branch(dir);
    worktrees
        .iter()
        .find(|w| w.branch == default)
        .or_else(|| worktrees.first())
}

/// Path of the worktree that has the default branch checked out, if any. Unlike
/// [`find_default_worktree`] this never falls back to an unrelated worktree - it
/// returns `None` when the default branch isn't checked out anywhere, so callers
/// (e.g. the "update" action) don't fast-forward the wrong tree.
pub fn default_worktree_path(common_dir: &Path) -> Option<PathBuf> {
    let default = default_branch(common_dir);
    list_worktrees(common_dir)
        .ok()?
        .into_iter()
        .find(|w| w.branch == default)
        .map(|w| w.path)
}

/// List local + remote-tracking branches for the create picker.
///
/// Local branches come first (with `checked_out`/`is_default` marked); remote
/// branches follow, with the `<remote>/` prefix stripped and `HEAD` skipped.
/// A remote branch whose short name already exists locally is dropped (the
/// local entry covers it). Sorted: default first, then local, then remote, each
/// group alphabetical.
pub fn list_branches(common_dir: &Path) -> Result<Vec<BranchInfo>> {
    let repo = open_lenient(common_dir)?;
    let default = default_branch_name(&repo);

    // Branch names currently checked out in some worktree.
    let checked_out: HashSet<String> = list_worktrees(common_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|w| w.branch)
        .collect();

    let mut locals: Vec<BranchInfo> = Vec::new();
    let mut local_names: HashSet<String> = HashSet::new();
    for entry in repo.branches(Some(BranchType::Local))?.flatten() {
        let (branch, _) = entry;
        if let Ok(Some(name)) = branch.name() {
            local_names.insert(name.to_string());
            locals.push(BranchInfo {
                name: name.to_string(),
                kind: BranchKind::Local,
                remote: None,
                checked_out: checked_out.contains(name),
                is_default: default.as_deref() == Some(name),
            });
        }
    }

    let mut remotes: Vec<BranchInfo> = Vec::new();
    for entry in repo.branches(Some(BranchType::Remote))?.flatten() {
        let (branch, _) = entry;
        if let Ok(Some(full)) = branch.name() {
            let Some((remote, short)) = full.split_once('/') else {
                continue;
            };
            if short == "HEAD" || local_names.contains(short) {
                continue;
            }
            remotes.push(BranchInfo {
                name: short.to_string(),
                kind: BranchKind::Remote,
                remote: Some(remote.to_string()),
                checked_out: false,
                is_default: false,
            });
        }
    }

    let sort_key = |b: &BranchInfo| (!b.is_default, b.name.clone());
    locals.sort_by_key(sort_key);
    remotes.sort_by_key(sort_key);
    locals.extend(remotes);
    Ok(locals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::create_worktree;
    use crate::worktree::test_support::{git_available, run, setup};

    #[test]
    fn create_lists_and_removes_worktree() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();

        assert!(list_worktrees(&bare).unwrap().is_empty());

        let wt = create_worktree(&bare, "feature").unwrap();
        assert_eq!(wt.branch, "feature");
        assert!(wt.path.ends_with("feature"));
        assert!(wt.path.is_dir());

        let listed = list_worktrees(&bare).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name(), "feature");
        assert_eq!(listed[0].branch, "feature");

        let _ = crate::worktree::remove_worktree(&bare, "feature", true, false);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_branches_reports_local_and_checked_out() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        // Add a second local branch in the bare repo.
        run(&bare, &["branch", "feature", "main"]);

        let branches = list_branches(&bare).unwrap();
        let names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main"), "main should be listed: {names:?}");
        assert!(
            names.contains(&"feature"),
            "feature should be listed: {names:?}"
        );
        assert!(
            branches.iter().all(|b| b.kind == BranchKind::Local),
            "no remotes configured, all should be local"
        );
        assert!(
            branches.iter().all(|b| !b.checked_out),
            "nothing checked out yet"
        );

        // After checking out `feature` into a worktree it must be flagged.
        create_worktree(&bare, "feature").unwrap();
        let branches = list_branches(&bare).unwrap();
        let feature = branches.iter().find(|b| b.name == "feature").unwrap();
        assert!(feature.checked_out, "feature is now in a worktree");
        let main = branches.iter().find(|b| b.name == "main").unwrap();
        assert!(!main.checked_out, "main is still free");

        let _ = std::fs::remove_dir_all(&root);
    }
}
