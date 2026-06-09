use crate::worktree::branches::default_branch_name;
use crate::worktree::model::{Worktree, worktree_name_for_branch};
use anyhow::{Context, Result};
use git2::{BranchType, Repository, WorktreeAddOptions};
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve the directory that holds all worktrees for `repo`.
///
/// For the bare-repo layout (`project/.bare`, `project/main`, ...) this is the
/// `project/` directory - the common ancestor of the git dir and any workdir.
/// Adapted from git-workon-lib's `workon_root`.
fn workon_root(repo: &Repository) -> Result<PathBuf> {
    let path = repo.path();
    if let Some(workdir) = repo.workdir()
        && workdir != path
    {
        let repo_ancestors: Vec<_> = path.ancestors().collect();
        if let Some(common) = workdir.ancestors().find(|a| repo_ancestors.contains(a)) {
            return Ok(common.to_path_buf());
        }
    }
    path.parent()
        .map(Path::to_path_buf)
        .context("git dir has no parent")
}

/// Find a remote-tracking branch `<remote>/<branch_name>` and return its
/// `(remote, oid)`. Adapted from git-workon-lib.
fn find_remote_tracking_branch(
    repo: &Repository,
    branch_name: &str,
) -> Option<(String, git2::Oid)> {
    for entry in repo.branches(Some(BranchType::Remote)).ok()?.flatten() {
        let (branch, _) = entry;
        if let Ok(Some(full)) = branch.name()
            && let Some((remote, name)) = full.split_once('/')
            && name == branch_name
            && let Some(oid) = branch.get().target()
        {
            return Some((remote.to_string(), oid));
        }
    }
    None
}

/// Create a worktree for `branch` under the repo's worktree root.
///
/// Reuses an existing local branch; otherwise creates it from the matching
/// remote-tracking branch, the default branch, or HEAD. The worktree dir is a
/// sibling of `.bare` named after the branch (git-wt layout). `common_dir` must
/// point at the bare/common git dir. Adapted from git-workon-lib's
/// `add_worktree` (Normal-branch path only).
pub fn create_worktree(common_dir: &Path, branch: &str) -> Result<Worktree> {
    let repo = Repository::open(common_dir)
        .with_context(|| format!("open bare repo at {}", common_dir.display()))?;

    let (reference, remote) = match repo.find_branch(branch, BranchType::Local) {
        // Existing local branch: pull LFS from whatever remote it tracks.
        Ok(b) => {
            let remote = branch_remote(&repo, branch);
            (b.into_reference(), remote)
        }
        Err(_) => {
            if let Some((remote, oid)) = find_remote_tracking_branch(&repo, branch) {
                let commit = repo.find_commit(oid)?;
                let mut local = repo.branch(branch, &commit, false)?;
                let _ = local.set_upstream(Some(&format!("{remote}/{branch}")));
                (local.into_reference(), Some(remote))
            } else {
                let name = default_branch_name(&repo).unwrap_or_else(|| "main".into());
                let base = repo
                    .find_branch(&name, BranchType::Local)
                    .map(git2::Branch::into_reference)
                    .or_else(|_| {
                        repo.find_branch(&format!("origin/{name}"), BranchType::Remote)
                            .map(git2::Branch::into_reference)
                    })
                    .or_else(|_| repo.head())?
                    .peel_to_commit()?;
                (repo.branch(branch, &base, false)?.into_reference(), None)
            }
        }
    };

    add_worktree(&repo, branch, &reference, remote.as_deref())
}

/// Create a worktree for a brand-new `new_branch` cut from `base`.
///
/// `base` is resolved to a commit via, in order: a local branch, the
/// `origin/<base>` remote-tracking branch, then a generic revparse (so a tag or
/// raw sha works too). The new branch carries no upstream - it's local-only
/// until pushed. `common_dir` must point at the bare/common git dir.
pub fn create_worktree_from_base(
    common_dir: &Path,
    new_branch: &str,
    base: &str,
) -> Result<Worktree> {
    let repo = Repository::open(common_dir)
        .with_context(|| format!("open bare repo at {}", common_dir.display()))?;

    let base_commit = repo
        .find_branch(base, BranchType::Local)
        .map(git2::Branch::into_reference)
        .or_else(|_| {
            repo.find_branch(&format!("origin/{base}"), BranchType::Remote)
                .map(git2::Branch::into_reference)
        })
        .and_then(|r| r.peel_to_commit())
        .or_else(|_| repo.revparse_single(base).and_then(|o| o.peel_to_commit()))
        .with_context(|| format!("resolve base branch {base}"))?;

    let reference = repo
        .branch(new_branch, &base_commit, false)
        .with_context(|| format!("create branch {new_branch} from {base}"))?
        .into_reference();

    // The new branch has no upstream, so resolve the remote the *base* tracks
    // (which may not be `origin`) for the LFS pull. Falls back to None ->
    // git-lfs's own default when the base is a tag/sha or tracks nothing.
    let remote = base_remote(&repo, base);

    add_worktree(&repo, new_branch, &reference, remote.as_deref())
}

/// Materialize the worktree directory for `branch` pointed at `reference`. The
/// worktree dir is a sibling of `.bare` named after the branch (git-wt layout);
/// since git can't name a worktree with slashes, the registry name uses the
/// branch basename.
fn add_worktree(
    repo: &Repository,
    branch: &str,
    reference: &git2::Reference,
    remote: Option<&str>,
) -> Result<Worktree> {
    let root = workon_root(repo)?;
    let wt_name = worktree_name_for_branch(branch);
    let wt_path = root.join(branch);
    if let Some(parent) = wt_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(reference));
    let wt = repo
        .worktree(wt_name, &wt_path, Some(&opts))
        .with_context(|| format!("create worktree {wt_name} at {}", wt_path.display()))?;

    let path = wt.path().to_path_buf();
    inflate_lfs(&path, remote);

    Ok(Worktree {
        path,
        branch: branch.to_string(),
    })
}

/// The remote that local `branch` tracks (e.g. `origin`, `upstream`), if any.
fn branch_remote(repo: &Repository, branch: &str) -> Option<String> {
    let refname = format!("refs/heads/{branch}");
    repo.branch_upstream_remote(&refname)
        .ok()
        .and_then(|buf| buf.as_str().ok().map(String::from))
}

/// The remote that a `base` (for a brand-new branch) draws its objects from:
/// the remote a local base branch tracks, else the `<remote>/` of a matching
/// remote-tracking branch. `None` for a tag/sha or an untracked local branch.
fn base_remote(repo: &Repository, base: &str) -> Option<String> {
    if repo.find_branch(base, BranchType::Local).is_ok()
        && let Some(remote) = branch_remote(repo, base)
    {
        return Some(remote);
    }
    find_remote_tracking_branch(repo, base).map(|(remote, _)| remote)
}

/// Inflate any Git LFS pointer files in the freshly-created worktree at `path`.
///
/// libgit2's checkout doesn't run Git's clean/smudge filters, so LFS-tracked
/// files land as pointer files rather than their real contents. We shell out to
/// `git lfs pull` (fetch missing objects + smudge) to materialize them - the
/// same subprocess-for-credentials rationale as the daemon's periodic fetch.
///
/// `remote` pins the pull to the remote the branch/base actually tracks; a new
/// branch has no upstream, so without this `git lfs` would silently default to
/// `origin` and miss objects that live only on a non-`origin` remote. `None`
/// falls back to git-lfs's own default.
///
/// Best-effort: a repo that doesn't use LFS, or a missing `git-lfs` install, is
/// not an error - worktree creation must still succeed, so failures are only
/// logged.
fn inflate_lfs(path: &Path, remote: Option<&str>) {
    if !uses_lfs(path) {
        return;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(path).args(["lfs", "pull"]);
    if let Some(remote) = remote {
        cmd.arg(remote);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => {}
        Ok(o) => tracing::warn!(
            "git lfs pull in {} exited {}: {}",
            path.display(),
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => tracing::warn!("git lfs pull in {} failed: {e}", path.display()),
    }
}

/// Whether the worktree at `path` has LFS-tracked files at HEAD.
///
/// `git lfs ls-files` lists exactly the pointer files that need inflating; an
/// empty list (non-LFS repo) or a non-zero exit (`git-lfs` not installed) both
/// mean there's nothing for us to do.
fn uses_lfs(path: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["lfs", "ls-files", "-n"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::list_branches;
    use crate::worktree::test_support::{git_available, setup};
    use std::process::Command;

    #[test]
    fn create_worktree_from_base_cuts_new_branch() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();

        let wt = create_worktree_from_base(&bare, "feature/new", "main").unwrap();
        assert_eq!(wt.branch, "feature/new");
        assert!(wt.path.is_dir());

        // The new branch must point at main's commit.
        let head_of = |dir: &Path, rev: &str| -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["rev-parse", rev])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let base_oid = head_of(&bare, "main");
        let new_oid = head_of(&wt.path, "HEAD");
        assert_eq!(new_oid, base_oid, "new branch should be cut from main");

        // And the local branch exists in the repo.
        let branches = list_branches(&bare).unwrap();
        assert!(branches.iter().any(|b| b.name == "feature/new"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
