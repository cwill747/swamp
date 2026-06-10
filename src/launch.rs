use crate::config::{self, ConfigPaths, Harness, resolve_harness};
use crate::daemon;
use crate::daemon::socket::{ClientMsg, ServerMsg};
use crate::worktree::{Worktree, git_common_dir, is_bare, list_worktrees, resolve_git_dir};
use crate::zellij;
use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

mod layout;
use layout::{write_multi_tab_layout, write_worktree_layout};

/// Returns `true` when `running` differs from `mine` (i.e. the daemon was
/// started by a different swamp build).  Simple equality for now; unit-tested
/// so future changes don't silently regress.
pub fn version_is_stale(running: &str, mine: &str) -> bool {
    running != mine
}

/// Query the running daemon for its version.  Returns `None` if the socket is
/// absent, the daemon is unreachable, or the daemon is too old to understand
/// `GetVersion`.
fn query_daemon_version(common_dir: &Path) -> Option<String> {
    let sock = daemon::socket_path(common_dir).ok()?;
    if !sock.exists() {
        return None;
    }

    let handle = tokio::runtime::Handle::try_current().ok()?;
    tokio::task::block_in_place(|| {
        handle.block_on(async {
            use crate::daemon::socket::{read_server_msg, write_client_msg};
            use tokio::net::UnixStream;

            let mut stream = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                UnixStream::connect(&sock),
            )
            .await
            .ok() // Result<Result<UnixStream>, Elapsed> → Option<Result<UnixStream>>
            .and_then(|r| r.ok())?; // flatten inner Result → Option<UnixStream>

            write_client_msg(&mut stream, &ClientMsg::GetVersion)
                .await
                .ok()?;

            let resp = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                read_server_msg(&mut stream),
            )
            .await
            .ok() // Option<Result<Option<ServerMsg>>>
            .and_then(|r| r.ok()) // Option<Option<ServerMsg>>
            .and_then(|o| o)?; // Option<ServerMsg>

            match resp {
                ServerMsg::Version { version } => Some(version),
                _ => None,
            }
        })
    })
}

pub fn run(dir: Option<PathBuf>) -> Result<()> {
    let target = match dir {
        Some(p) => {
            std::fs::canonicalize(&p).with_context(|| format!("canonicalize {}", p.display()))?
        }
        None => std::env::current_dir()?,
    };
    let git_dir = resolve_git_dir(&target);
    let bare = is_bare(&git_dir);
    let worktrees = list_worktrees(&git_dir)?;
    if worktrees.is_empty() {
        anyhow::bail!("no worktrees found under {}", target.display());
    }

    let session = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "swamp".into());

    let cfg = config::ensure_configs()?;
    if let Ok(common) = git_common_dir(&git_dir) {
        crate::logging::init(&common, false, false, &cfg.logging);
    }

    // When launched from inside an existing zellij session, create a *nested*
    // session rather than dumping tabs into the host session. `nested` causes
    // the spawned zellij to have ZELLIJ unset so it doesn't refuse to nest.
    let nested = zellij::in_zellij();
    spawn_new_session(&target, bare, &worktrees, &session, &cfg, nested)
}

fn spawn_new_session(
    target: &Path,
    bare: bool,
    worktrees: &[Worktree],
    session: &str,
    cfg: &ConfigPaths,
    nested: bool,
) -> Result<()> {
    // Reuse an existing session if one already matches this repo's name —
    // but first check whether the running daemon is stale.
    if let Ok(sessions) = zellij::list_sessions()
        && sessions.iter().any(|s| s == session)
    {
        let my_version = env!("CARGO_PKG_VERSION");
        let git_dir = resolve_git_dir(target);
        let common = git_common_dir(&git_dir);

        let mut do_restart = false;
        if let Ok(common) = &common {
            if let Some(running_version) = query_daemon_version(common) {
                if version_is_stale(&running_version, my_version) {
                    if std::io::stdin().is_terminal() {
                        print!(
                            "swamp: running daemon is version {} but this binary is {} — restart session? [Y/n] ",
                            running_version, my_version
                        );
                        use std::io::Write;
                        let _ = std::io::stdout().flush();
                        let mut answer = String::new();
                        let _ = std::io::stdin().read_line(&mut answer);
                        let answer = answer.trim().to_lowercase();
                        do_restart = answer.is_empty() || answer == "y" || answer == "yes";
                    } else {
                        eprintln!(
                            "swamp: warning: running daemon is version {} but this binary is {} (non-interactive, attaching anyway)",
                            running_version, my_version
                        );
                    }
                }
            } else {
                // No version response — treat as stale (old daemon).
                if std::io::stdin().is_terminal() {
                    print!(
                        "swamp: running daemon did not report a version (likely an older build) — restart session? [Y/n] "
                    );
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    let mut answer = String::new();
                    let _ = std::io::stdin().read_line(&mut answer);
                    let answer = answer.trim().to_lowercase();
                    do_restart = answer.is_empty() || answer == "y" || answer == "yes";
                } else {
                    eprintln!(
                        "swamp: warning: running daemon did not report a version (likely an older build), attaching anyway"
                    );
                }
            }
        }

        if do_restart {
            crate::kill::run(Some(target.to_path_buf()))?;
            // Fall through to fresh launch below.
        } else {
            return zellij::attach(session, nested);
        }
    }

    let git_dir = resolve_git_dir(target);
    let layout_path = write_multi_tab_layout(bare, worktrees, session, cfg, &git_dir)?;
    let res = zellij::new_session_with_layout(&layout_path, target, session, nested);
    let _ = std::fs::remove_file(&layout_path);
    res
}

/// Open a new zellij tab for a worktree, using a freshly generated,
/// `$SHELL`-aware layout rather than an externally-installed one.
pub fn open_worktree_tab(path: &Path, name: &str) -> Result<()> {
    let cfg = config::ensure_configs()?;
    // Resolve this worktree's harness: the repo setting, plus its persisted
    // override when the setting is `choose`.
    let override_ = git_common_dir(&resolve_git_dir(path))
        .ok()
        .map(|c| load_harness_overrides(&c))
        .and_then(|m| m.get(name).copied());
    let harness = resolve_harness(cfg.harness, override_);
    let layout = write_worktree_layout(&cfg, harness)?;
    tracing::debug!(
        worktree = %name,
        layout = %layout.display(),
        ?harness,
        "wrote worktree tab layout"
    );
    let res = zellij::new_tab(&layout.to_string_lossy(), path, name);
    let _ = std::fs::remove_file(&layout);
    res
}

/// Close the worktree's tab and reopen it, so a harness swap takes effect live.
/// Reopening reads the freshly-persisted override, so the new tab's agent pane
/// comes up as the chosen harness.
///
/// Meant to run **detached** from the pane that triggered it (`swamp
/// relaunch-tab`): pressing `h` inside a worktree's own sidebar closes that very
/// tab, which would otherwise abort the reopen. Skipped when fewer than two tabs
/// exist — closing the only tab would end the session — so the swap then falls
/// back to applying on the next launch.
pub fn relaunch_worktree_tab(name: &str, path: &Path) -> Result<()> {
    // Runs as a detached `swamp relaunch-tab` process, so wire up logging here
    // too (best-effort) to capture the tab close/reopen.
    if let Ok(common) = git_common_dir(&resolve_git_dir(path)) {
        let log_cfg = config::load_config().map(|c| c.logging).unwrap_or_default();
        crate::logging::init(&common, false, false, &log_cfg);
    }
    tracing::info!(worktree = %name, "relaunching worktree tab");
    if !zellij::in_zellij() {
        return Ok(());
    }
    let Ok(tabs) = zellij::list_tab_names() else {
        return Ok(());
    };
    if !tabs.iter().any(|t| t == name) {
        // No tab to relaunch (e.g. closed); just open it fresh.
        return open_worktree_tab(path, name);
    }
    if tabs.len() < 2 {
        // Closing the sole tab would tear down the session; leave it and let the
        // persisted override apply on the next launch.
        return Ok(());
    }
    let _ = zellij::close_tab_by_name(name);
    open_worktree_tab(path, name)?;
    let _ = zellij::go_to_tab_name(name);
    Ok(())
}

/// Load the worktree → Claude session id map from the persisted
/// `.swamp-status.json` in the git common dir. `swamp kill` leaves this file in
/// place, so on the next launch we can resume each worktree's session. Ids that
/// fail `is_safe_session_id` are dropped — we interpolate the id straight into a
/// shell command line, so anything outside the expected UUID charset is refused
/// rather than escaped.
pub(super) fn load_session_ids(common_dir: &Path) -> std::collections::HashMap<String, String> {
    let path = common_dir.join(".swamp-status.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Default::default();
    };
    let Ok(map) =
        serde_json::from_slice::<std::collections::HashMap<String, serde_json::Value>>(&bytes)
    else {
        return Default::default();
    };
    map.into_iter()
        .filter_map(|(name, v)| {
            v.get("session_id")
                .and_then(|s| s.as_str())
                .filter(|s| is_safe_session_id(s))
                .map(|s| (name, s.to_string()))
        })
        .collect()
}

/// Load the worktree → harness override map from `.swamp-status.json`. Only
/// consulted when the repo setting is `choose`; an unrecognized value is
/// dropped so a hand-edited file can't pick a non-existent agent.
pub(super) fn load_harness_overrides(
    common_dir: &Path,
) -> std::collections::HashMap<String, Harness> {
    let path = common_dir.join(".swamp-status.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Default::default();
    };
    let Ok(map) =
        serde_json::from_slice::<std::collections::HashMap<String, serde_json::Value>>(&bytes)
    else {
        return Default::default();
    };
    map.into_iter()
        .filter_map(|(name, v)| {
            let h = match v.get("harness").and_then(|s| s.as_str()) {
                Some("claude") => Harness::Claude,
                Some("codex") => Harness::Codex,
                _ => return None,
            };
            Some((name, h))
        })
        .collect()
}

/// A session id is safe to splice into a shell command only if it's a plain
/// token — Claude session ids are UUIDs, so restrict to `[A-Za-z0-9_-]`.
pub(super) fn is_safe_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_stale_same_version() {
        assert!(!version_is_stale("0.1.0", "0.1.0"));
    }

    #[test]
    fn version_is_stale_different_version() {
        assert!(version_is_stale("0.1.0", "0.2.0"));
    }

    #[test]
    fn version_is_stale_empty_running() {
        // Old daemons that don't respond should be treated as stale by callers,
        // but an empty string is still different from any real version.
        assert!(version_is_stale("", "0.1.0"));
    }

    #[test]
    fn safe_session_id_accepts_uuid_rejects_shell_metachars() {
        assert!(is_safe_session_id("3f9c1e2a-7b40-4d8e-9a1f-2c3d4e5f6a7b"));
        assert!(is_safe_session_id("abc_123-DEF"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("id; rm -rf /"));
        assert!(!is_safe_session_id("$(whoami)"));
        assert!(!is_safe_session_id("a b"));
    }

    /// `load_session_ids` reads worktree → session id pairs from a persisted
    /// status file and drops entries whose id is unsafe or absent.
    #[test]
    fn load_session_ids_reads_safe_entries_only() {
        let dir = std::env::temp_dir().join(format!("swamp-sid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "feat": { "status": "idle", "ts": 1, "session_id": "good-id-1" },
            "bare": { "status": "working", "ts": 2, "session_id": "rm -rf" },
            "none": { "status": "idle", "ts": 3 }
        }"#;
        std::fs::write(dir.join(".swamp-status.json"), json).unwrap();

        let map = load_session_ids(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(map.get("feat").map(String::as_str), Some("good-id-1"));
        assert!(!map.contains_key("bare"), "unsafe id must be dropped");
        assert!(!map.contains_key("none"), "missing id must be absent");
    }

    #[test]
    fn load_session_ids_missing_file_is_empty() {
        let dir = std::env::temp_dir().join("swamp-definitely-missing-dir-xyz");
        assert!(load_session_ids(&dir).is_empty());
    }
}
