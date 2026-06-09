//! Diagnostic logging: a per-repository log file plus the `swamp logs`
//! inspection command.
//!
//! Every swamp process for a repo writes `tracing` events to one shared file,
//! `$XDG_RUNTIME_DIR/swamp/<repo_id>.log` (the temp-dir fallback mirrors the
//! daemon socket path). The daemon truncates the file when it starts; other
//! short-lived processes (TUI panes, `swamp relaunch-tab`) append. Worktree-
//! relevant events carry a `worktree=<name>` field so `swamp logs` can scope
//! output to a single worktree by path.

use crate::config::LoggingConfig;
use crate::util::repo_id;
use crate::worktree::{git_common_dir, list_worktrees, resolve_git_dir};
use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Per-repository log file path, alongside the daemon's socket/PID files.
pub fn log_path(common_dir: &Path) -> PathBuf {
    let id = repo_id(common_dir);
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("swamp").join(format!("{id}.log"))
}

/// A cloneable writer over a shared append handle. `&File` implements `Write`,
/// so each clone borrows the same descriptor; `O_APPEND` keeps concurrent
/// writes from different swamp processes from clobbering one another.
#[derive(Clone)]
struct FileWriter(Arc<std::fs::File>);

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

/// Build the active `EnvFilter`: `RUST_LOG` wins, then the configured `filter`
/// directive, then the bare `level`. The `filter` string is validated when the
/// config is loaded, so the fallbacks here are defensive.
fn env_filter(cfg: &LoggingConfig) -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    if std::env::var_os("RUST_LOG").is_some() {
        return EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(cfg.level.as_str()));
    }
    if let Some(filter) = &cfg.filter
        && let Ok(f) = EnvFilter::try_new(filter)
    {
        return f;
    }
    EnvFilter::new(cfg.level.as_str())
}

/// Open the per-repo log file, creating its directory. `fresh` truncates
/// (daemon startup, to bound growth); otherwise the file is opened for append.
fn open_log_file(common_dir: &Path, fresh: bool) -> Option<std::fs::File> {
    let path = log_path(common_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(fresh)
        .append(!fresh)
        .open(&path)
        .ok()
}

/// Install the tracing subscriber for the current process: always a file layer
/// (per-repo log), plus a stderr layer when `foreground`. `fresh` truncates the
/// file first (daemon only). Best-effort and idempotent — `try_init` means a
/// second call in the same process is a harmless no-op.
pub fn init(common_dir: &Path, foreground: bool, fresh: bool, cfg: &LoggingConfig) {
    use tracing_subscriber::fmt;
    use tracing_subscriber::prelude::*;

    let file_layer = open_log_file(common_dir, fresh).map(|file| {
        let writer = FileWriter(Arc::new(file));
        fmt::layer()
            .with_ansi(false)
            .with_writer(move || writer.clone())
    });
    let stderr_layer = foreground.then(|| fmt::layer().with_writer(std::io::stderr));

    let _ = tracing_subscriber::registry()
        .with(env_filter(cfg))
        .with(file_layer)
        .with(stderr_layer)
        .try_init();
}

/// True when `line` carries a `worktree=<name>` field. Matches the whole
/// space-delimited field token, so a name that is a substring of another
/// worktree's name (e.g. `feat` vs `feature`) does not leak across filters.
fn line_matches_worktree(line: &str, name: &str) -> bool {
    let token = format!("worktree={name}");
    line.split_whitespace().any(|t| t == token)
}

/// The worktree whose path contains `target`, by the same `path.file_name()`
/// name the rest of swamp uses. `None` when `target` is the repo/bare root or
/// sits outside every worktree — the caller then prints the whole-repo log.
/// The most specific (longest path) match wins for nested worktrees.
fn current_worktree_name(common_dir: &Path, target: &Path) -> Option<String> {
    let target = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let wts = list_worktrees(common_dir).ok()?;
    wts.into_iter()
        .filter_map(|wt| {
            let canon = std::fs::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
            target.starts_with(&canon).then(|| (canon, wt.name()))
        })
        .max_by_key(|(p, _)| p.components().count())
        .map(|(_, name)| name)
}

/// `swamp logs`: print (and optionally follow) the active repo's log file,
/// scoped to the worktree that `dir` falls in unless `all` is set.
pub async fn show(dir: Option<PathBuf>, follow: bool, all: bool) -> Result<()> {
    let target = match dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let git_dir = resolve_git_dir(&target);
    let common = git_common_dir(&git_dir).context("not inside a git repo")?;
    let path = log_path(&common);

    let scope = if all {
        None
    } else {
        current_worktree_name(&common, &target)
    };

    if !path.exists() {
        println!(
            "swamp: no logs yet for this repository ({})",
            path.display()
        );
        return Ok(());
    }

    let keep = |line: &str| match &scope {
        None => true,
        Some(name) => line_matches_worktree(line, name),
    };

    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("open {}", path.display()))?;

    let mut buf = String::new();
    file.read_to_string(&mut buf).await?;
    let mut out = std::io::stdout();
    for line in buf.lines() {
        if keep(line) {
            let _ = writeln!(out, "{line}");
        }
    }
    if !follow {
        return Ok(());
    }

    // Tail: poll for appended bytes, re-reading from the last offset. A shorter
    // file means it was truncated (a new daemon started), so rewind to 0.
    let mut offset = file.stream_position().await?;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let len = match tokio::fs::metadata(&path).await {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if len < offset {
            offset = 0;
            file = tokio::fs::File::open(&path).await?;
        }
        if len > offset {
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut chunk = String::new();
            file.read_to_string(&mut chunk).await?;
            offset = len;
            for line in chunk.lines() {
                if keep(line) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_field_matches_exact_token_only() {
        let line = r#"2024-01-01 INFO swamp::launch: opening tab worktree=feat layout=/tmp/x.kdl"#;
        assert!(line_matches_worktree(line, "feat"));
        // A substring of the real worktree name must not match.
        assert!(!line_matches_worktree(line, "fea"));
        // A superstring must not match either.
        assert!(!line_matches_worktree(line, "feature"));
    }

    #[test]
    fn worktree_field_distinguishes_similar_names() {
        let feature = r#"INFO swamp: refreshed worktree=feature total=2"#;
        assert!(line_matches_worktree(feature, "feature"));
        assert!(!line_matches_worktree(feature, "feat"));
    }

    #[test]
    fn repo_wide_line_has_no_worktree_field() {
        let line = r#"INFO swamp::daemon: swamp daemon listening on /run/swamp/x.sock"#;
        assert!(!line_matches_worktree(line, "main"));
    }

    #[test]
    fn log_path_is_under_runtime_dir() {
        // SAFETY: single-threaded test; no other thread reads the environment.
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        let p = log_path(Path::new("/repo/.bare"));
        assert!(p.starts_with("/run/user/1000/swamp"));
        assert!(p.extension().is_some_and(|e| e == "log"));
    }
}
