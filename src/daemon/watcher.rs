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
                Some(res) if !is_own_status_event(&res) => break,
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

/// True when every path in the event is one of swamp's own status files
/// (`.swamp-status.json` or its rename-source `.tmp`), i.e. the event was
/// caused by a hook persist rather than a git state change. Watch errors and
/// events with no paths are treated as relevant so a real change is never
/// dropped.
fn is_own_status_event(res: &notify::Result<notify::Event>) -> bool {
    let Ok(ev) = res else {
        return false;
    };
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
    use std::path::PathBuf;

    fn event_with_paths(paths: &[&str]) -> notify::Result<Event> {
        Ok(Event {
            paths: paths.iter().map(PathBuf::from).collect(),
            ..Default::default()
        })
    }

    /// Hook persists (status file + its tmp rename-source) are swamp's own
    /// writes and must not trigger a rescan; anything touching other files —
    /// even in the same burst — must.
    #[test]
    fn status_file_events_are_ignored_others_are_not() {
        assert!(is_own_status_event(&event_with_paths(&[
            "/repo/.git/.swamp-status.json"
        ])));
        assert!(is_own_status_event(&event_with_paths(&[
            "/repo/.git/.swamp-status.json.tmp",
            "/repo/.git/.swamp-status.json",
        ])));
        // A git change is relevant.
        assert!(!is_own_status_event(&event_with_paths(&[
            "/repo/.git/HEAD"
        ])));
        // Mixed events stay relevant.
        assert!(!is_own_status_event(&event_with_paths(&[
            "/repo/.git/.swamp-status.json",
            "/repo/.git/packed-refs",
        ])));
        // No paths / errors: treated as relevant so changes are never dropped.
        assert!(!is_own_status_event(&event_with_paths(&[])));
        assert!(!is_own_status_event(&Err(notify::Error::generic("boom"))));
    }
}
