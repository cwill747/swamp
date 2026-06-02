use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    pub head: String,
    pub bare: bool,
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
}

/// Resolve the effective directory for git commands.
///
/// The git-wt bare-clone pattern creates a container dir with `.git → .bare`
/// that git itself does not recognize as a valid worktree.  When git fails for
/// `dir` but a `.bare/` subdirectory exists, return that instead.
pub fn resolve_git_dir(dir: &Path) -> PathBuf {
    if git(dir, &["rev-parse", "--git-dir"]).is_ok() {
        return dir.to_path_buf();
    }
    let bare = dir.join(".bare");
    if bare.is_dir() {
        return bare;
    }
    dir.to_path_buf()
}

/// Run `git -C dir <args...>` and return stdout.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("git {:?}", args))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn git_common_dir(dir: &Path) -> Result<PathBuf> {
    let s = git(dir, &["rev-parse", "--git-common-dir"])?
        .trim()
        .to_string();
    let p = PathBuf::from(s);
    if p.is_absolute() {
        Ok(p)
    } else {
        Ok(dir.join(p))
    }
}

pub fn is_bare(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--is-bare-repository"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// Parse `git worktree list --porcelain` output.
pub fn list_worktrees(dir: &Path) -> Result<Vec<Worktree>> {
    let out = git(dir, &["worktree", "list", "--porcelain"])?;
    let mut wts = Vec::new();
    let mut cur = Worktree {
        path: PathBuf::new(),
        branch: String::new(),
        head: String::new(),
        bare: false,
    };
    let mut have_path = false;
    let flush = |cur: &mut Worktree, have_path: &mut bool, wts: &mut Vec<Worktree>| {
        if *have_path && !cur.bare {
            if cur.branch.is_empty() {
                let sha = if cur.head.len() >= 7 { &cur.head[..7] } else { &cur.head };
                cur.branch = format!("detached@{}", sha);
            }
            wts.push(cur.clone());
        }
        *cur = Worktree {
            path: PathBuf::new(),
            branch: String::new(),
            head: String::new(),
            bare: false,
        };
        *have_path = false;
    };
    for line in out.lines() {
        if line.is_empty() {
            flush(&mut cur, &mut have_path, &mut wts);
        } else if let Some(p) = line.strip_prefix("worktree ") {
            cur.path = PathBuf::from(p);
            have_path = true;
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            cur.branch = b.to_string();
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            cur.head = h.to_string();
        } else if line == "bare" {
            cur.bare = true;
        }
    }
    flush(&mut cur, &mut have_path, &mut wts);
    Ok(wts)
}

/// Detect the default branch name (e.g. "main", "master") by inspecting
/// `refs/remotes/origin/HEAD`.  Falls back to "main".
pub fn default_branch(dir: &Path) -> String {
    git(dir, &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .ok()
        .and_then(|s| s.trim().strip_prefix("refs/remotes/origin/").map(String::from))
        .unwrap_or_else(|| "main".into())
}

/// Find the worktree tracking the default branch (the one `git wt update`
/// syncs).  Falls back to the first worktree if no match.
pub fn find_default_worktree<'a>(worktrees: &'a [Worktree], dir: &Path) -> Option<&'a Worktree> {
    let default = default_branch(dir);
    worktrees
        .iter()
        .find(|w| w.branch == default)
        .or_else(|| worktrees.first())
}

/// Parse `git status --porcelain=v2 -b` for a single worktree.
pub fn git_info(dir: &Path) -> Result<GitInfo> {
    let out = git(dir, &["status", "--porcelain=v2", "-b", "--untracked-files=normal"])?;
    let mut info = GitInfo::default();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            info.branch = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            info.upstream = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            let mut it = rest.split_whitespace();
            if let Some(a) = it.next() {
                info.ahead = a.trim_start_matches('+').parse().unwrap_or(0);
            }
            if let Some(b) = it.next() {
                info.behind = b.trim_start_matches('-').parse().unwrap_or(0);
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            // "<X><Y> ..." — index then worktree status chars
            let bytes = line.as_bytes();
            if bytes.len() > 3 {
                let x = bytes[2] as char;
                let y = bytes[3] as char;
                if x != '.' {
                    info.staged += 1;
                }
                if y != '.' {
                    info.unstaged += 1;
                }
            }
        } else if line.starts_with("? ") {
            info.untracked += 1;
        } else if line.starts_with("u ") {
            info.conflict = true;
        }
    }
    // rebase detection
    if let Ok(common) = git_common_dir(dir) {
        if common.join("rebase-merge").exists() || common.join("rebase-apply").exists() {
            info.rebase = true;
        }
    }
    if info.branch.is_empty() {
        info.branch = "(detached)".into();
    }
    Ok(info)
}
