use super::Daemon;
use anyhow::Result;
use notify::{RecursiveMode, Watcher};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub async fn run(daemon: Arc<Daemon>) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    // Watch the common dir itself for direct files like HEAD, packed-refs, and
    // .swamp-status.json, but avoid recursively subscribing to objects/ and logs/.
    watcher.watch(&daemon.common_dir, RecursiveMode::NonRecursive)?;
    let mut refs_watched = ensure_watch_if_exists(
        &mut watcher,
        &daemon.common_dir.join("refs"),
        RecursiveMode::Recursive,
        false,
    )?;
    let mut worktrees_watched = ensure_watch_if_exists(
        &mut watcher,
        &daemon.common_dir.join("worktrees"),
        RecursiveMode::Recursive,
        false,
    )?;

    // Debounce: collect bursts, then refresh once.
    loop {
        // Wait for an event that actually warrants a rescan. Swamp's own
        // status-file writes land in the watched common dir (every hook ping
        // persists `.swamp-status.json` via a tmp+rename), and treating them as
        // git changes echoed each hook into a full worktree rescan + broadcast.
        loop {
            match rx.recv().await {
                None => return Ok(()),
                Some(res) if warrants_rescan(&res) => break,
                Some(_) => continue,
            }
        }
        // Drain any further events within a short window.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                ev = rx.recv() => {
                    if ev.is_none() { return Ok(()); }
                }
            }
        }
        tracing::debug!(trigger = "watcher", "filesystem change; git refresh");
        refs_watched = ensure_watch_if_exists(
            &mut watcher,
            &daemon.common_dir.join("refs"),
            RecursiveMode::Recursive,
            refs_watched,
        )?;
        worktrees_watched = ensure_watch_if_exists(
            &mut watcher,
            &daemon.common_dir.join("worktrees"),
            RecursiveMode::Recursive,
            worktrees_watched,
        )?;
        if let Err(e) = daemon.refresh_all().await {
            tracing::warn!("watcher refresh: {e:?}");
        }
    }
}

/// Whether an event should trigger a git rescan. Watch errors and events with
/// no paths are treated as relevant so a real change is never dropped, but two
/// classes of event are filtered out:
///
/// - `Access` (open/read) events: reads never change git state, and the daemon
///   itself opens `config`, `refs/`, and per-worktree files on every refresh.
///   libgit2's own reads emitting `Access(Open)` events otherwise feed straight
///   back into the watcher, spinning `refresh_all` ~5x/sec forever.
/// - Swamp's own status-file writes (`.swamp-status.json` / its `.tmp`
///   rename-source), which every hook ping persists into the watched common
///   dir; treating them as git changes echoed each hook into a full rescan.
fn warrants_rescan(res: &notify::Result<notify::Event>) -> bool {
    let Ok(ev) = res else {
        return true;
    };
    if matches!(ev.kind, notify::EventKind::Access(_)) {
        return false;
    }
    !is_own_status_event(ev)
}

/// True when every path in the event is one of swamp's own status files
/// (`.swamp-status.json` or its rename-source `.tmp`). An event with no paths
/// is not treated as own-status, so it still warrants a rescan.
fn is_own_status_event(ev: &notify::Event) -> bool {
    !ev.paths.is_empty()
        && ev.paths.iter().all(|p| {
            p.file_name()
                .is_some_and(|n| n == ".swamp-status.json" || n == ".swamp-status.json.tmp")
        })
}

fn ensure_watch_if_exists(
    watcher: &mut notify::RecommendedWatcher,
    path: &Path,
    mode: RecursiveMode,
    watched: bool,
) -> notify::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    if !watched {
        watcher.watch(path, mode)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::Event;
    use notify::event::{AccessKind, AccessMode, EventKind, ModifyKind};
    use std::path::PathBuf;

    fn modify_event(paths: &[&str]) -> notify::Result<Event> {
        Ok(Event {
            kind: EventKind::Modify(ModifyKind::Any),
            paths: paths.iter().map(PathBuf::from).collect(),
            ..Default::default()
        })
    }

    fn access_event(paths: &[&str]) -> notify::Result<Event> {
        Ok(Event {
            kind: EventKind::Access(AccessKind::Open(AccessMode::Any)),
            paths: paths.iter().map(PathBuf::from).collect(),
            ..Default::default()
        })
    }

    /// Hook persists (status file + its tmp rename-source) are swamp's own
    /// writes and must not trigger a rescan; anything touching other files —
    /// even in the same burst — must.
    #[test]
    fn status_file_writes_do_not_warrant_rescan_others_do() {
        assert!(!warrants_rescan(&modify_event(&[
            "/repo/.git/.swamp-status.json"
        ])));
        assert!(!warrants_rescan(&modify_event(&[
            "/repo/.git/.swamp-status.json.tmp",
            "/repo/.git/.swamp-status.json",
        ])));
        // A git change is relevant.
        assert!(warrants_rescan(&modify_event(&["/repo/.git/HEAD"])));
        // Mixed events stay relevant.
        assert!(warrants_rescan(&modify_event(&[
            "/repo/.git/.swamp-status.json",
            "/repo/.git/packed-refs",
        ])));
        // No paths / errors: treated as relevant so changes are never dropped.
        assert!(warrants_rescan(&modify_event(&[])));
        assert!(warrants_rescan(&Err(notify::Error::generic("boom"))));
    }

    /// Read/open (`Access`) events never warrant a rescan: reads don't change
    /// git state, and the daemon opens `config`/`refs/` on every refresh, so
    /// honoring them spins `refresh_all` in a tight loop.
    #[test]
    fn access_events_do_not_warrant_rescan() {
        assert!(!warrants_rescan(&access_event(&["/repo/.git/config"])));
        assert!(!warrants_rescan(&access_event(&["/repo/.git/refs"])));
        // Even an access touching a "real" git path must be ignored.
        assert!(!warrants_rescan(&access_event(&["/repo/.git/HEAD"])));
    }
}
