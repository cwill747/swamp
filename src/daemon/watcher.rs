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
    watch_if_exists(
        &mut watcher,
        &daemon.common_dir.join("refs"),
        RecursiveMode::Recursive,
    )?;
    watch_if_exists(
        &mut watcher,
        &daemon.common_dir.join("worktrees"),
        RecursiveMode::Recursive,
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
        if let Err(e) = daemon.refresh_all().await {
            tracing::warn!("watcher refresh: {e:?}");
        }
    }
}

fn watch_if_exists(
    watcher: &mut notify::RecommendedWatcher,
    path: &Path,
    mode: RecursiveMode,
) -> notify::Result<()> {
    if path.exists() {
        watcher.watch(path, mode)?;
    }
    Ok(())
}
