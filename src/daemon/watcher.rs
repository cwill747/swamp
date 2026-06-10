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
        if rx.recv().await.is_none() {
            return Ok(());
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
