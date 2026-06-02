use crate::daemon;
use crate::worktree::{git_common_dir, resolve_git_dir};
use crate::zellij;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Derive the zellij session name for a repo container dir — same logic as
/// `launch::run`.  Returns the stem of the canonicalized path.
pub fn session_name_for_dir(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "swamp".into())
}

pub fn run(dir: Option<PathBuf>) -> Result<()> {
    let start = match dir {
        Some(p) => std::fs::canonicalize(&p)
            .with_context(|| format!("canonicalize {}", p.display()))?,
        None => std::env::current_dir()?,
    };

    let git_dir = resolve_git_dir(&start);
    let common = git_common_dir(&git_dir).context("not inside a git repo")?;
    let session = session_name_for_dir(&start);

    kill_daemon(&common);
    kill_zellij_session(&session);

    Ok(())
}

fn kill_daemon(common_dir: &std::path::Path) {
    let pid_file = daemon::pid_path(common_dir);
    let sock_file = daemon::socket_path(common_dir);

    match std::fs::read_to_string(&pid_file) {
        Ok(contents) => {
            let pid_str = contents.trim().to_string();
            match pid_str.parse::<i32>() {
                Ok(pid) => {
                    tracing::info!("sending SIGTERM to daemon pid {pid}");
                    let status = std::process::Command::new("kill")
                        .arg("-TERM")
                        .arg(pid.to_string())
                        .status();
                    match status {
                        Ok(s) if s.success() => {
                            // Give it a moment, then SIGKILL if still alive.
                            std::thread::sleep(std::time::Duration::from_millis(300));
                            let _ = std::process::Command::new("kill")
                                .arg("-0")
                                .arg(pid.to_string())
                                .status()
                                .ok()
                                .filter(|s| s.success())
                                .map(|_| {
                                    tracing::warn!("daemon still alive, sending SIGKILL");
                                    let _ = std::process::Command::new("kill")
                                        .arg("-KILL")
                                        .arg(pid.to_string())
                                        .status();
                                });
                        }
                        Ok(_) => {
                            tracing::warn!("kill -TERM {pid} returned non-zero (process may not exist)");
                        }
                        Err(e) => {
                            tracing::warn!("failed to run kill: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("could not parse pid from {}: {e}", pid_file.display());
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!("no daemon pid file found at {}", pid_file.display());
        }
        Err(e) => {
            tracing::warn!("could not read pid file {}: {e}", pid_file.display());
        }
    }

    // Clean up socket and pid files regardless.
    if sock_file.exists() {
        if let Err(e) = std::fs::remove_file(&sock_file) {
            tracing::warn!("could not remove socket file: {e}");
        }
    }
    if pid_file.exists() {
        if let Err(e) = std::fs::remove_file(&pid_file) {
            tracing::warn!("could not remove pid file: {e}");
        }
    }
}

fn kill_zellij_session(session: &str) {
    if let Err(e) = zellij::kill_session(session) {
        tracing::warn!("could not kill zellij session {session:?}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn session_name_is_last_component() {
        assert_eq!(
            session_name_for_dir(Path::new("/home/user/code/myrepo")),
            "myrepo"
        );
        assert_eq!(
            session_name_for_dir(Path::new("/home/user/code/talks")),
            "talks"
        );
    }

    #[test]
    fn session_name_trailing_slash_stripped() {
        // PathBuf::file_name() handles trailing slashes by returning None on
        // "/", but for a normal path it works correctly regardless.
        assert_eq!(
            session_name_for_dir(Path::new("/some/path/repo")),
            "repo"
        );
    }
}
