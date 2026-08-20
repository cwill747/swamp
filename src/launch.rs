use crate::config::{self, ConfigPaths, Harness, resolve_harness};
use crate::daemon;
use crate::daemon::socket::{ClientMsg, ServerMsg};
use crate::util::session_name_for;
use crate::worktree::{Worktree, git_common_dir, list_worktrees, resolve_git_dir};
use crate::zellij;
use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::Duration;

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
    let worktrees = list_worktrees(&git_dir)?;
    if worktrees.is_empty() {
        anyhow::bail!("no worktrees found under {}", target.display());
    }

    let cfg = config::ensure_configs()?;
    let common = git_common_dir(&git_dir);
    if let Ok(ref c) = common {
        crate::logging::init(c, false, false, &cfg.logging);
    }
    let session = common
        .as_deref()
        .map(session_name_for)
        .unwrap_or_else(|_| "swamp".into());

    // Zellij 0.45 handles a client launched inside another Zellij session as a
    // native nested session. Keep the foreground launch path so the repo session
    // stays in this pane and Zellij can offer its nesting controls.
    spawn_new_session(&target, &worktrees, &session, &cfg)
}

fn spawn_new_session(
    target: &Path,
    worktrees: &[Worktree],
    session: &str,
    cfg: &ConfigPaths,
) -> Result<()> {
    let git_dir = resolve_git_dir(target);
    let common = git_common_dir(&git_dir);
    let launch_lock = match &common {
        Ok(c) => Some(acquire_launch_lock(c)?),
        Err(_) => None,
    };

    // Reuse an existing session if one already matches this repo's name —
    // but first check whether the running daemon is stale.
    let sessions = zellij::list_sessions()?;
    if sessions.iter().any(|s| s == session) {
        let my_version = env!("CARGO_PKG_VERSION");

        let mut do_restart = false;
        if let Ok(common) = &common {
            if let Some(running_version) = query_daemon_version(common) {
                if version_is_stale(&running_version, my_version) {
                    if std::io::stdin().is_terminal() {
                        do_restart = prompt_restart(&format!(
                            "swamp: running daemon is version {} but this binary is {} - restart session? [y/N] ",
                            running_version, my_version
                        ));
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
                    do_restart = prompt_restart(
                        "swamp: running daemon did not report a version (likely an older build) - restart session? [y/N] ",
                    );
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
        } else if std::env::var("ZELLIJ_SESSION_NAME").as_deref() == Ok(session) {
            // The repo session is already active. Attaching it inside itself
            // would create a recursive client instead of a useful nested session.
            return Ok(());
        } else {
            return zellij::attach(session);
        }
    }

    let layout_path = write_multi_tab_layout(worktrees, session, cfg, &git_dir)?;
    let mut child = zellij::spawn_new_session_with_layout(&layout_path, target, session)?;
    wait_for_session_registration(&mut child, session)?;
    drop(launch_lock);
    zellij::wait_for_session_exit(child)
}

/// Keep the launch lock until the new session is visible to other swamp
/// processes. Once registered, concurrent launches can acquire the lock and
/// attach instead of timing out while this foreground client remains open.
fn wait_for_session_registration(child: &mut Child, session: &str) -> Result<()> {
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    const MAX_ATTEMPTS: usize = 100;

    for _ in 0..MAX_ATTEMPTS {
        if zellij::list_sessions()
            .is_ok_and(|sessions| sessions.iter().any(|candidate| candidate == session))
        {
            return Ok(());
        }
        if child
            .try_wait()
            .context("check new zellij session process")?
            .is_some()
        {
            return Ok(());
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    tracing::warn!(
        session,
        "zellij session was not registered within 5 seconds"
    );
    Ok(())
}

fn prompt_restart(prompt: &str) -> bool {
    print!("{prompt}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

fn launch_lock_path(common_dir: &Path) -> Result<PathBuf> {
    let id = crate::util::repo_id(common_dir);
    Ok(crate::util::runtime_base_dir()?.join(format!("{id}.launch.lock")))
}

fn acquire_launch_lock(common_dir: &Path) -> Result<std::fs::File> {
    let path = launch_lock_path(common_dir)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open launch lock {}", path.display()))?;
    let fd = file.as_raw_fd();
    let mut waited_ms = 0u64;
    loop {
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            return Ok(file);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock || waited_ms >= 5_000 {
            return Err(err).context("flock launch lock");
        }
        std::thread::sleep(Duration::from_millis(50));
        waited_ms += 50;
    }
}

/// Open a new zellij tab for a worktree, using a freshly generated,
/// `$SHELL`-aware layout rather than an externally-installed one.
pub fn open_worktree_tab(path: &Path, name: &str) -> Result<()> {
    let cfg = config::ensure_configs()?;
    let common = git_common_dir(&resolve_git_dir(path)).ok();
    // Resolve this worktree's harness: the repo setting, plus its persisted
    // override when the setting is `choose`.
    let override_ = common
        .as_deref()
        .map(load_harness_overrides)
        .and_then(|m| m.get(name).copied());
    let harness = resolve_harness(cfg.harness, override_);
    // Resume the worktree's recorded Claude session so an on-demand tab picks
    // the conversation back up, matching what launch used to do per worktree.
    let resume = common
        .as_deref()
        .map(load_session_ids)
        .and_then(|m| m.get(name).cloned());
    let layout = write_worktree_layout(&cfg, harness, resume.as_deref())?;
    tracing::debug!(
        worktree = %name,
        layout = %layout.display(),
        ?harness,
        resume = resume.is_some(),
        "wrote worktree tab layout"
    );
    zellij::new_tab(&layout.to_string_lossy(), path, name)
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

/// Spawn a detached `swamp delete-tab` to close worktree `name`'s tab and
/// remove it, in its own process group with a cwd outside the target — the
/// same pattern [`relaunch_worktree_tab`]'s caller (`spawn_relaunch_tab`)
/// uses for the harness swap, which has the identical "this pane is about to
/// close its own tab" problem. Shared by the `D` key and by
/// `confirm-delete`'s force path when `--close-tab` was set, so the detached
/// process is spawned identically from both.
pub fn spawn_delete_tab(name: &str, path: &Path, common_dir: &Path, force: bool) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("delete-tab").arg(name).arg(path);
    if force {
        cmd.arg("--force");
    }
    let _ = cmd
        .current_dir(common_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

/// Close worktree `name`'s Zellij tab, then remove the worktree. Runs as the
/// detached `swamp delete-tab` process spawned by [`spawn_delete_tab`], so it
/// has no terminal to print to — failures go to the repository diagnostic log
/// instead.
///
/// Closes the tab **before** removing the directory: `remove_dir_all` on a
/// directory that is the working directory of live processes succeeds on
/// Linux but leaves lazygit and the agent writing into an unlinked tree, and
/// an agent mid-write can recreate paths under a directory being removed.
/// Closing the tab terminates those processes first.
pub async fn delete_tab(name: &str, path: &Path, force: bool) -> Result<()> {
    if let Ok(common) = git_common_dir(&resolve_git_dir(path)) {
        let log_cfg = config::load_config().map(|c| c.logging).unwrap_or_default();
        crate::logging::init(&common, false, false, &log_cfg);
    }
    tracing::info!(worktree = %name, "delete-tab: closing tab and removing worktree");

    if zellij::in_zellij()
        && let Err(e) = zellij::close_tab_by_name(name)
    {
        tracing::warn!(worktree = %name, "delete-tab: close tab failed: {e:?}");
    }

    if let Err(e) = send_remove_worktree(name, path, force).await {
        tracing::warn!(worktree = %name, "delete-tab: remove worktree failed: {e:?}");
    }
    Ok(())
}

/// Send `RemoveWorktree` for `name` (whose common dir is resolved from
/// `path`) to the daemon and wait for the reply.
async fn send_remove_worktree(name: &str, path: &Path, force: bool) -> Result<()> {
    let common = git_common_dir(&resolve_git_dir(path))?;
    send_remove_worktree_at(&common, name, force).await
}

async fn send_remove_worktree_at(common_dir: &Path, name: &str, force: bool) -> Result<()> {
    use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
    use tokio::net::UnixStream;

    let sock = daemon::socket_path(common_dir)?;
    let mut stream = UnixStream::connect(&sock)
        .await
        .context("connect to daemon")?;
    write_client_msg(
        &mut stream,
        &ClientMsg::RemoveWorktree {
            name: name.to_string(),
            force,
        },
    )
    .await?;
    match read_server_msg(&mut stream).await? {
        Some(ServerMsg::Ok) => Ok(()),
        Some(ServerMsg::Err { message }) => Err(anyhow::anyhow!(message)),
        Some(ServerMsg::ErrDirty { reason, .. }) => Err(anyhow::anyhow!(reason)),
        Some(other) => Err(anyhow::anyhow!("unexpected reply: {other:?}")),
        None => Err(anyhow::anyhow!("daemon closed before replying")),
    }
}

/// Render the blocked-deletion confirmation for worktree `name` at `path`:
/// the reason it was given — never re-computed, see design decision 8 in the
/// `better-worktree-deletion` change — plus its current status and diffstat
/// read live, then wait for `f` (force) or `n`/Esc. Runs interactively in a
/// Zellij floating pane spawned by the TUI, so unlike [`delete_tab`] it has a
/// real terminal to render into.
///
/// No daemon round-trip happens before the prompt appears: everything shown
/// is read directly from the worktree, not fetched from the daemon.
pub async fn confirm_delete(name: &str, path: &Path, reason: &str, close_tab: bool) -> Result<()> {
    let common = git_common_dir(&resolve_git_dir(path))
        .with_context(|| format!("resolve git common dir for {}", path.display()))?;
    let log_cfg = config::load_config().map(|c| c.logging).unwrap_or_default();
    crate::logging::init(&common, false, false, &log_cfg);

    println!("Worktree '{name}' {reason}.\n");
    print_git_status(path);
    println!();
    print!("Force delete? (f = force, n/Esc = cancel) ");
    {
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }

    let force = wait_for_force_key()?;
    println!();
    if !force {
        println!("Cancelled.");
        return Ok(());
    }

    if close_tab {
        spawn_delete_tab(name, path, &common, true);
    } else if let Err(e) = send_remove_worktree_at(&common, name, true).await {
        tracing::warn!(worktree = %name, "confirm-delete: force remove failed: {e:?}");
        println!("Delete failed: {e}");
    }
    Ok(())
}

/// Print `git status --short` and `git diff --stat HEAD` for `path` — the
/// work at risk, read fresh at render time rather than from any cached state.
fn print_git_status(path: &Path) {
    println!("Status:");
    print_git_output(path, &["status", "--short"]);
    println!("\nDiff summary:");
    print_git_output(path, &["diff", "--stat", "HEAD"]);
}

/// Run one `git` command in `path` and print its stdout under the current
/// heading. A command that starts but exits nonzero — a corrupt index, an
/// unreadable object store, no `HEAD` — still yields `Ok`, with empty stdout.
/// Printing that as `(none)` would tell the user there is no work at risk
/// exactly when git could not look, which is the wrong answer to act on right
/// before a force delete. So the exit status is checked, and a failure reports
/// the status and git's own stderr instead.
fn print_git_output(path: &Path, args: &[&str]) {
    match std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
    {
        Ok(out) if !out.status.success() => {
            println!("  (git {} failed: {})", args.join(" "), out.status);
            let err = String::from_utf8_lossy(&out.stderr);
            for line in err.lines().filter(|l| !l.trim().is_empty()) {
                println!("  {line}");
            }
        }
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.trim().is_empty() {
                println!("  (none)");
            } else {
                print!("{text}");
            }
        }
        Err(e) => println!("  (unavailable: {e})"),
    }
}

/// Block waiting for a single `f`/`n`/Esc keypress in raw mode, returning
/// `true` for force and `false` for cancel.
fn wait_for_force_key() -> Result<bool> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, read};
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

    enable_raw_mode().context("enable raw mode")?;
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('f') | KeyCode::Char('F') => break Ok(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => break Ok(false),
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e).context("read key event"),
        }
    };
    let _ = disable_raw_mode();
    result
}

/// Load the worktree → Claude session id map from the persisted
/// `.swamp-status.json` in the git common dir. `swamp kill` leaves this file in
/// place, so on the next launch we can resume each worktree's session. Ids that
/// fail `is_safe_session_id` are dropped — we interpolate the id straight into a
/// shell command line, so anything outside the expected UUID charset is refused
/// rather than escaped.
pub(super) fn load_session_ids(common_dir: &Path) -> std::collections::HashMap<String, String> {
    let Some(map) = load_status_values(common_dir) else {
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
    let Some(map) = load_status_values(common_dir) else {
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

fn load_status_values(
    common_dir: &Path,
) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    let path = common_dir.join(".swamp-status.json");
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(map) => Some(map),
        Err(e) => {
            let corrupt = corrupt_status_path(&path);
            match std::fs::rename(&path, &corrupt) {
                Ok(()) => tracing::warn!(
                    path = %path.display(),
                    corrupt = %corrupt.display(),
                    "renamed corrupt swamp status file: {e}"
                ),
                Err(rename_err) => tracing::warn!(
                    path = %path.display(),
                    "could not rename corrupt swamp status file ({e}): {rename_err}"
                ),
            }
            None
        }
    }
}

fn corrupt_status_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.corrupt", crate::util::now_unix()));
    PathBuf::from(name)
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
