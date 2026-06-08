use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
}

impl Worktree {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
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
    /// True when this local branch is already checked out in some worktree -
    /// such a branch can't be checked out into a *new* worktree.
    pub checked_out: bool,
    /// True for the repository default branch (sorted first; the base default).
    pub is_default: bool,
}

/// Registry/tab name used for a branch's worktree. Git worktree names cannot
/// contain slashes, so git-wt-style branch paths use the branch basename.
pub fn worktree_name_for_branch(branch: &str) -> &str {
    Path::new(branch)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(branch)
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

/// Returned by [`crate::worktree::remove_worktree`] when a non-forced removal
/// is refused because the worktree has uncommitted work. Callers can downcast
/// the `anyhow::Error` to this to offer a force override rather than treating
/// it as a hard failure.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_name_uses_branch_basename() {
        assert_eq!(worktree_name_for_branch("feature"), "feature");
        assert_eq!(worktree_name_for_branch("users/alice/feature"), "feature");
    }
}
