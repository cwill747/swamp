use super::Daemon;
use anyhow::Result;
use notify::{RecursiveMode, Watcher};
use std::sync::Arc;
use std::time::Duration;

pub async fn run(daemon: Arc<Daemon>) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    // Watch the common dir (covers HEAD, worktrees/, index changes for the bare repo).
    watcher.watch(&daemon.common_dir, RecursiveMode::Recursive)?;
    // Also watch each worktree's index/HEAD.
    {
        let s = daemon.state.read().await;
        for row in s.rows.values() {
            let _ = watcher.watch(&row.path.join(".git"), RecursiveMode::NonRecursive);
        }
    }

    // Debounce: collect bursts, then refresh once.
    loop {
        let mut got_event = false;
        tokio::select! {
            ev = rx.recv() => {
                if ev.is_some() {
                    got_event = true;
                } else {
                    return Ok(());
                }
            }
        }
        if got_event {
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
            if let Err(e) = daemon.refresh_all().await {
                tracing::warn!("watcher refresh: {e:?}");
            }
        }
    }
}
