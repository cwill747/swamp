use anyhow::{Context, Result};
use git2::Repository;
use std::path::{Path, PathBuf};

/// Resolve the effective directory for git operations.
///
/// The git-wt bare-clone pattern creates a container dir with `.git -> .bare`
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
pub(super) fn open_lenient(dir: &Path) -> Result<Repository> {
    Repository::open(dir)
        .or_else(|_| Repository::discover(dir))
        .with_context(|| format!("open git repo at {}", dir.display()))
}

pub fn git_common_dir(dir: &Path) -> Result<PathBuf> {
    let repo = open_lenient(dir)?;
    Ok(repo.commondir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::test_support::{git_available, setup};

    #[test]
    fn helpers_resolve_bare_layout() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        // resolve_git_dir prefers the `.bare` subdir of the container.
        assert_eq!(resolve_git_dir(&root), bare);
        let common = git_common_dir(&bare).unwrap();
        assert_eq!(common.canonicalize().unwrap(), bare.canonicalize().unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }
}
