// Git worktree + status operations backed by libgit2 (`git2`).
//
// The worktree creation/deletion logic here is adapted from git-workon-lib
// (MIT, Ahmed El Gabri) — https://github.com/git-workon/git-workon-lib —
// specifically `add_worktree`/`workon_root` and the binary's `prune_worktree`.
// It has been slimmed to the cases swamp needs and ported to `anyhow`.
//
// Everything is read-only/local except the daemon's periodic `git fetch`, which
// deliberately stays a subprocess (it needs the user's credential/SSH setup).

use anyhow::{Context, Result};
use git2::{
    BranchType, Repository, RepositoryState, Status, StatusOptions, WorktreeAddOptions,
    WorktreePruneOptions,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
}

/// Whether a [`BranchInfo`] names a local branch or a remote-tracking one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchKind {
    Local,
    Remote,
}

/// A branch the create picker can offer: either to spin a worktree from
/// directly, or to use as the base for a brand-new branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    /// Short name (e.g. `feature/login`); for a remote branch the `<remote>/`
    /// prefix is stripped so it reads like a local branch.
    pub name: String,
    pub kind: BranchKind,
    /// The owning remote (e.g. `origin`) for [`BranchKind::Remote`].
    pub remote: Option<String>,
    /// True when this local branch is already checked out in some worktree —
    /// such a branch can't be checked out into a *new* worktree.
    pub checked_out: bool,
    /// True for the repository default branch (sorted first; the base default).
    pub is_default: bool,
}

impl Worktree {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(Debug, Clone, Default)]
pub struct GitInfo {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflict: bool,
    pub rebase: bool,
    pub head_ts: u64,
}

impl GitInfo {
    /// Whether the working tree holds changes that a removal would discard:
    /// staged, unstaged, untracked, or an in-progress conflict.
    pub fn is_dirty(&self) -> bool {
        self.staged + self.unstaged + self.untracked > 0 || self.conflict
    }
}

/// Returned by [`remove_worktree`] when a non-forced removal is refused because
/// the worktree has uncommitted work. Callers can downcast the `anyhow::Error`
/// to this to offer a force override rather than treating it as a hard failure.
#[derive(Debug)]
pub struct DirtyWorktree {
    pub name: String,
}

impl std::fmt::Display for DirtyWorktree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "worktree '{}' has uncommitted changes; refusing to remove",
            self.name
        )
    }
}

impl std::error::Error for DirtyWorktree {}

/// Resolve the effective directory for git operations.
///
/// The git-wt bare-clone pattern creates a container dir with `.git → .bare`
/// that libgit2 won't treat as a normal repo dir. When a `.bare/` subdirectory
/// exists, prefer it so we open the bare repo directly.
pub fn resolve_git_dir(dir: &Path) -> PathBuf {
    let bare = dir.join(".bare");
    if bare.is_dir() {
        return bare;
    }
    dir.to_path_buf()
}

/// Open the repo for `dir`, tolerating either an exact repo dir or any path
/// inside a working tree (discover walks upward).
fn open_lenient(dir: &Path) -> Result<Repository> {
    Repository::open(dir)
        .or_else(|_| Repository::discover(dir))
        .with_context(|| format!("open git repo at {}", dir.display()))
}

pub fn git_common_dir(dir: &Path) -> Result<PathBuf> {
    let repo = open_lenient(dir)?;
    Ok(repo.commondir().to_path_buf())
}

pub fn is_bare(dir: &Path) -> bool {
    open_lenient(dir).map(|r| r.is_bare()).unwrap_or(false)
}

/// Resolve the branch label for the worktree rooted at `path`. A detached HEAD
/// yields a `detached@<sha>` label, matching the prior porcelain output.
fn worktree_branch(path: &Path) -> String {
    let Ok(repo) = Repository::open(path) else {
        return String::new();
    };
    if !repo.head_detached().unwrap_or(false) {
        if let Some(name) = repo.head().ok().and_then(|h| h.shorthand().map(String::from)) {
            return name;
        }
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

    if !repo.is_bare() {
        if let Some(workdir) = repo.workdir() {
            let path = workdir.to_path_buf();
            let branch = worktree_branch(&path);
            wts.push(Worktree { path, branch });
        }
    }

    Ok(wts)
}

/// Resolve the default branch *name* (e.g. "main") from
/// `refs/remotes/origin/HEAD`.
fn default_branch_name(repo: &Repository) -> Option<String> {
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

/// Collect git status for a single worktree at `dir`.
pub fn git_info(dir: &Path) -> Result<GitInfo> {
    let repo = Repository::open(dir)?;
    let mut info = GitInfo::default();

    let detached = repo.head_detached().unwrap_or(false);
    let head = repo.head().ok();

    info.branch = if detached {
        "(detached)".into()
    } else {
        head.as_ref()
            .and_then(|h| h.shorthand())
            .unwrap_or("(detached)")
            .to_string()
    };

    if let Some(commit) = head.as_ref().and_then(|h| h.peel_to_commit().ok()) {
        info.head_ts = commit.time().seconds().max(0) as u64;
    }

    // Upstream tracking + ahead/behind.
    if !detached {
        if let (Some(local_oid), Ok(branch)) = (
            head.as_ref().and_then(|h| h.target()),
            repo.find_branch(&info.branch, BranchType::Local),
        ) {
            if let Ok(upstream) = branch.upstream() {
                if let Ok(Some(name)) = upstream.name() {
                    info.upstream = Some(name.to_string());
                }
                if let Some(up_oid) = upstream.get().target() {
                    if let Ok((ahead, behind)) = repo.graph_ahead_behind(local_oid, up_oid) {
                        info.ahead = ahead as u32;
                        info.behind = behind as u32;
                    }
                }
            }
        }
    }

    // Working-tree status counts.
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .include_ignored(false);
    if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
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

/// Resolve the directory that holds all worktrees for `repo`.
///
/// For the bare-repo layout (`project/.bare`, `project/main`, …) this is the
/// `project/` directory — the common ancestor of the git dir and any workdir.
/// Adapted from git-workon-lib's `workon_root`.
fn workon_root(repo: &Repository) -> Result<PathBuf> {
    let path = repo.path();
    if let Some(workdir) = repo.workdir() {
        if workdir != path {
            let repo_ancestors: Vec<_> = path.ancestors().collect();
            if let Some(common) = workdir
                .ancestors()
                .find(|a| repo_ancestors.contains(a))
            {
                return Ok(common.to_path_buf());
            }
        }
    }
    path.parent()
        .map(Path::to_path_buf)
        .context("git dir has no parent")
}

/// Find a remote-tracking branch `<remote>/<branch_name>` and return its
/// `(remote, oid)`. Adapted from git-workon-lib.
fn find_remote_tracking_branch(repo: &Repository, branch_name: &str) -> Option<(String, git2::Oid)> {
    for entry in repo.branches(Some(BranchType::Remote)).ok()?.flatten() {
        let (branch, _) = entry;
        if let Ok(Some(full)) = branch.name() {
            if let Some((remote, name)) = full.split_once('/') {
                if name == branch_name {
                    if let Some(oid) = branch.get().target() {
                        return Some((remote.to_string(), oid));
                    }
                }
            }
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

    let reference = match repo.find_branch(branch, BranchType::Local) {
        Ok(b) => b.into_reference(),
        Err(_) => {
            if let Some((remote, oid)) = find_remote_tracking_branch(&repo, branch) {
                let commit = repo.find_commit(oid)?;
                let mut local = repo.branch(branch, &commit, false)?;
                let _ = local.set_upstream(Some(&format!("{}/{}", remote, branch)));
                local.into_reference()
            } else {
                let name = default_branch_name(&repo).unwrap_or_else(|| "main".into());
                let base = repo
                    .find_branch(&name, BranchType::Local)
                    .map(git2::Branch::into_reference)
                    .or_else(|_| {
                        repo.find_branch(&format!("origin/{}", name), BranchType::Remote)
                            .map(git2::Branch::into_reference)
                    })
                    .or_else(|_| repo.head())?
                    .peel_to_commit()?;
                repo.branch(branch, &base, false)?.into_reference()
            }
        }
    };

    add_worktree(&repo, branch, &reference)
}

/// Create a worktree for a brand-new `new_branch` cut from `base`.
///
/// `base` is resolved to a commit via, in order: a local branch, the
/// `origin/<base>` remote-tracking branch, then a generic revparse (so a tag or
/// raw sha works too). The new branch carries no upstream — it's local-only
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
            repo.find_branch(&format!("origin/{}", base), BranchType::Remote)
                .map(git2::Branch::into_reference)
        })
        .and_then(|r| r.peel_to_commit())
        .or_else(|_| {
            repo.revparse_single(base)
                .and_then(|o| o.peel_to_commit())
        })
        .with_context(|| format!("resolve base branch {}", base))?;

    let reference = repo
        .branch(new_branch, &base_commit, false)
        .with_context(|| format!("create branch {} from {}", new_branch, base))?
        .into_reference();

    add_worktree(&repo, new_branch, &reference)
}

/// Materialize the worktree directory for `branch` pointed at `reference`. The
/// worktree dir is a sibling of `.bare` named after the branch (git-wt layout);
/// since git can't name a worktree with slashes, the registry name uses the
/// branch basename.
fn add_worktree(repo: &Repository, branch: &str, reference: &git2::Reference) -> Result<Worktree> {
    let root = workon_root(repo)?;
    let wt_name = Path::new(branch)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(branch);
    let wt_path = root.join(branch);
    if let Some(parent) = wt_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(reference));
    let wt = repo
        .worktree(wt_name, &wt_path, Some(&opts))
        .with_context(|| format!("create worktree {} at {}", wt_name, wt_path.display()))?;

    Ok(Worktree {
        path: wt.path().to_path_buf(),
        branch: branch.to_string(),
    })
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
    let checked_out: std::collections::HashSet<String> = list_worktrees(common_dir)
        .unwrap_or_default()
        .into_iter()
        .map(|w| w.branch)
        .collect();

    let mut locals: Vec<BranchInfo> = Vec::new();
    let mut local_names: std::collections::HashSet<String> = std::collections::HashSet::new();
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

/// Remove the worktree named `name`: delete its directory, prune the git
/// metadata, and (when `delete_branch`) delete its local branch. Mirrors
/// `git wt remove`. Adapted from git-workon's `prune_worktree`.
///
/// Unless `force` is set, a worktree with uncommitted or untracked changes is
/// refused (returning a [`DirtyWorktree`] error) so `remove_dir_all` can't
/// silently discard local work — the caller is expected to surface this and
/// let the user opt into a forced removal.
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
        .with_context(|| format!("find worktree {}", name))?;
    let wt_path = wt.path().to_path_buf();

    // Guard against destroying uncommitted work. A failure to read status is
    // treated as "assume clean" so a removal is never blocked by a transient
    // libgit2 error; the force path skips the check entirely.
    if !force && wt_path.exists() {
        if let Ok(info) = git_info(&wt_path) {
            if info.is_dirty() {
                return Err(DirtyWorktree {
                    name: name.to_string(),
                }
                .into());
            }
        }
    }

    // Capture the branch before we tear the worktree down.
    let branch = if delete_branch {
        Repository::open(&wt_path)
            .ok()
            .filter(|r| !r.head_detached().unwrap_or(true))
            .and_then(|r| r.head().ok().and_then(|h| h.shorthand().map(String::from)))
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
        .with_context(|| format!("prune worktree {}", name))?;

    if let Some(branch) = branch {
        if let Ok(mut b) = repo.find_branch(&branch, BranchType::Local) {
            let _ = b.delete();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    /// Build a git-wt style layout: `root/.bare` (bare repo) with branch `main`
    /// committed, and `root/.git` linking to it. Returns `(root, bare_dir)`.
    fn setup() -> (PathBuf, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir()
            .join(format!("swamp-wt-test-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&root).unwrap();
        run(&root, &["init", "-q"]);
        run(&root, &["config", "user.email", "t@example.com"]);
        run(&root, &["config", "user.name", "t"]);
        run(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
        run(&root, &["branch", "-M", "main"]);
        // Convert to the bare-worktree layout.
        std::fs::rename(root.join(".git"), root.join(".bare")).unwrap();
        std::fs::write(root.join(".git"), "gitdir: ./.bare\n").unwrap();
        let bare = root.join(".bare");
        let ok = Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .args(["config", "core.bare", "true"])
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok);
        (root, bare)
    }

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

        let info = git_info(&wt.path).unwrap();
        assert_eq!(info.branch, "feature");
        assert_eq!(info.untracked, 0);
        assert_eq!(info.staged, 0);
        assert!(!info.conflict && !info.rebase);

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
        assert!(names.contains(&"feature"), "feature should be listed: {names:?}");
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

    #[test]
    fn helpers_resolve_bare_layout() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        // resolve_git_dir prefers the `.bare` subdir of the container.
        assert_eq!(resolve_git_dir(&root), bare);
        assert!(is_bare(&bare));
        let common = git_common_dir(&bare).unwrap();
        assert_eq!(
            common.canonicalize().unwrap(),
            bare.canonicalize().unwrap()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
