// Git worktree + status operations backed by libgit2 (`git2`).
//
// The worktree creation/deletion logic here is adapted from git-workon-lib
// (MIT, Ahmed El Gabri) - https://github.com/git-workon/git-workon-lib -
// specifically `add_worktree`/`workon_root` and the binary's `prune_worktree`.
// It has been slimmed to the cases swamp needs and ported to `anyhow`.
//
// Everything is read-only/local except the daemon's periodic `git fetch`, which
// deliberately stays a subprocess (it needs the user's credential/SSH setup).

mod branches;
mod create;
mod model;
mod remove;
mod repo;
mod status;

pub use branches::{
    default_branch, default_worktree_path, find_default_worktree, list_branches, list_worktrees,
};
pub use create::{create_worktree, create_worktree_from_base};
pub use model::{
    BranchInfo, BranchKind, DirtyWorktree, GitInfo, Worktree, worktree_name_for_branch,
};
pub use remove::remove_worktree;
pub use repo::{git_common_dir, is_bare, resolve_git_dir};
pub use status::git_info;

#[cfg(test)]
mod test_support {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(super) fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    pub(super) fn run(dir: &Path, args: &[&str]) {
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
    pub(super) fn setup() -> (PathBuf, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("swamp-wt-test-{}-{}", std::process::id(), nanos));
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
}
