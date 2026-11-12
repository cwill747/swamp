//! `swamp clone`: create a repository in swamp's bare layout.
//!
//! ```text
//! app/
//! ├── .bare/   git data (bare repo)
//! ├── .git     "gitdir: ./.bare"
//! └── main/    worktree for the default branch
//! ```
//!
//! Network steps (`fetch`, `remote set-head`) run as `git` subprocesses so they
//! use the user's credential and SSH setup, the same as the daemon's periodic
//! fetch. Their output goes to stderr so that stdout carries only the path.

use crate::worktree::branches::default_branch_name;
use crate::worktree::create::create_worktree;
use anyhow::{Context, Result, bail};
use git2::Repository;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Clone `url` into `dir` (default: `./<repo-name>`) in the bare layout and
/// return the path of the default branch worktree.
///
/// Refuses a target that exists and is not empty. On a failure after swamp
/// created the target dir, swamp removes that dir again.
pub fn clone_repo(url: &str, dir: Option<&Path>) -> Result<PathBuf> {
    let target = match dir {
        Some(d) => d.to_path_buf(),
        None => std::env::current_dir()?.join(repo_name_from_url(url)?),
    };
    if target.exists() {
        let empty = std::fs::read_dir(&target)
            .with_context(|| format!("read {}", target.display()))?
            .next()
            .is_none();
        if !empty {
            bail!("{} already exists and is not empty", target.display());
        }
    }
    let created = !target.exists();
    std::fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;

    let url = local_source(url, &std::env::current_dir()?);
    match clone_into(&url, &target) {
        Ok(path) => Ok(path),
        Err(err) => {
            if created {
                let _ = std::fs::remove_dir_all(&target);
            } else {
                // The dir was empty before; put it back the way it was.
                let _ = std::fs::remove_dir_all(target.join(".bare"));
                let _ = std::fs::remove_file(target.join(".git"));
            }
            Err(err)
        }
    }
}

fn clone_into(url: &str, target: &Path) -> Result<PathBuf> {
    let bare = target.join(".bare");
    git(target, &["init", "--bare", "--quiet", ".bare"])?;
    std::fs::write(target.join(".git"), "gitdir: ./.bare\n")
        .with_context(|| format!("write {}", target.join(".git").display()))?;
    // `remote add` writes the standard `+refs/heads/*:refs/remotes/origin/*`
    // refspec, so every remote branch gets a remote-tracking ref.
    git_dir(&bare, &["remote", "add", "origin", url])?;
    git_dir(&bare, &["fetch", "origin"])?;
    git_dir(&bare, &["remote", "set-head", "origin", "--auto"])?;

    let repo = Repository::open(&bare).with_context(|| format!("open {}", bare.display()))?;
    let default = default_branch_name(&repo)
        .context("remote has no default branch (is the repository empty?)")?;
    drop(repo);
    let wt = create_worktree(&bare, &default)?;
    Ok(wt.path)
}

/// `url` with a relative local path made absolute against `cwd`, as
/// `git clone` does. Later fetches run from other dirs (a worktree, or the
/// daemon with `-C <common dir>`), where a relative path would not resolve.
/// URLs with a scheme and scp-like `host:path` sources stay as they are.
fn local_source(url: &str, cwd: &Path) -> String {
    let scp_like = url
        .split_once(':')
        .is_some_and(|(host, _)| !host.is_empty() && !host.contains('/'));
    if url.contains("://") || scp_like || Path::new(url).is_absolute() {
        return url.to_string();
    }
    let abs = cwd.join(url);
    abs.canonicalize()
        .unwrap_or(abs)
        .to_string_lossy()
        .into_owned()
}

/// The directory name `git clone` would pick: the last path segment of the URL
/// without a trailing `.git`.
fn repo_name_from_url(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches(".git");
    if last.is_empty() || last == "." || last == ".." {
        bail!("cannot derive a directory name from {url}; pass a DIR");
    }
    Ok(last.to_string())
}

fn git(cwd: &Path, args: &[&str]) -> Result<()> {
    run(Command::new("git").arg("-C").arg(cwd).args(args), args)
}

fn git_dir(git_dir: &Path, args: &[&str]) -> Result<()> {
    run(
        Command::new("git").arg("--git-dir").arg(git_dir).args(args),
        args,
    )
}

fn run(cmd: &mut Command, args: &[&str]) -> Result<()> {
    // Send git's stdout to our stderr: stdout is reserved for the final path.
    let status = cmd
        .stdin(Stdio::null())
        .stdout(std::io::stderr())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !status.success() {
        bail!("git {} exited {status}", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::test_support::{git_available, run as git_run};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d =
            std::env::temp_dir().join(format!("swamp-clone-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A normal (non-bare) origin repo with `main` and a second branch.
    fn origin() -> PathBuf {
        let o = temp_dir("origin");
        git_run(&o, &["init", "-q", "-b", "main"]);
        git_run(&o, &["config", "user.email", "t@example.com"]);
        git_run(&o, &["config", "user.name", "t"]);
        git_run(&o, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git_run(&o, &["branch", "feature"]);
        o
    }

    #[test]
    fn repo_name_from_common_urls() {
        assert_eq!(
            repo_name_from_url("https://example.com/org/app.git").unwrap(),
            "app"
        );
        assert_eq!(
            repo_name_from_url("git@github.com:org/app.git").unwrap(),
            "app"
        );
        assert_eq!(repo_name_from_url("file:///tmp/x/app/").unwrap(), "app");
        assert!(repo_name_from_url("/").is_err());
    }

    #[test]
    fn local_source_makes_relative_paths_absolute() {
        let cwd = temp_dir("cwd");
        std::fs::create_dir_all(cwd.join("origin")).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        assert_eq!(
            local_source("./origin", &cwd),
            cwd.join("origin").to_string_lossy()
        );
        for url in [
            "https://example.com/org/app.git",
            "git@github.com:org/app.git",
            "file:///tmp/app",
            "/abs/app",
        ] {
            assert_eq!(local_source(url, &cwd), url);
        }
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn clone_creates_bare_layout() {
        if !git_available() {
            return;
        }
        let o = origin();
        let parent = temp_dir("parent");
        let target = parent.join("app");
        let url = format!("file://{}", o.display());

        let path = clone_repo(&url, Some(&target)).unwrap();

        assert_eq!(path, target.join("main"));
        assert!(target.join(".bare").is_dir());
        assert_eq!(
            std::fs::read_to_string(target.join(".git")).unwrap(),
            "gitdir: ./.bare\n"
        );
        assert!(path.join(".git").is_file(), "main is a linked worktree");
        let repo = Repository::open(target.join(".bare")).unwrap();
        assert!(repo.is_bare());
        assert!(repo.find_reference("refs/remotes/origin/main").is_ok());
        assert!(repo.find_reference("refs/remotes/origin/feature").is_ok());
        // Only the default branch exists locally.
        assert!(repo.find_branch("main", git2::BranchType::Local).is_ok());
        assert!(
            repo.find_branch("feature", git2::BranchType::Local)
                .is_err()
        );

        let _ = std::fs::remove_dir_all(&o);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn clone_refuses_non_empty_target() {
        if !git_available() {
            return;
        }
        let target = temp_dir("busy");
        std::fs::write(target.join("keep.txt"), "x").unwrap();

        let err = clone_repo("file:///nonexistent", Some(&target)).unwrap_err();

        assert!(err.to_string().contains("not empty"), "{err}");
        assert!(target.join("keep.txt").exists());
        assert!(!target.join(".bare").exists());
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn failed_clone_removes_created_dir() {
        if !git_available() {
            return;
        }
        let parent = temp_dir("fail");
        let target = parent.join("app");

        let missing = parent.join("no-such-origin");
        assert!(clone_repo(&format!("file://{}", missing.display()), Some(&target)).is_err());

        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(&parent);
    }
}
