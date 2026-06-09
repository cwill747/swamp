use crate::daemon::socket::{ClientMsg, write_client_msg};
use crate::daemon::{self};
use crate::util::now_unix;
use crate::worktree::git_common_dir;
use anyhow::{Context, Result};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;

pub async fn run(
    status: String,
    dir: Option<PathBuf>,
    session_name: Option<String>,
    session_id: Option<String>,
) -> Result<()> {
    let start = dir.unwrap_or(std::env::current_dir()?);
    let common = git_common_dir(&start).context("not inside a git repo")?;
    let wt_name = start
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());

    let sock = daemon::socket_path(&common);
    // Try daemon first.
    if sock.exists()
        && let Ok(Ok(mut s)) =
            tokio::time::timeout(Duration::from_millis(200), UnixStream::connect(&sock)).await
    {
        let _ = write_client_msg(
            &mut s,
            &ClientMsg::Hook {
                worktree: wt_name.clone(),
                status: status.clone(),
                session_name: session_name.clone(),
                session_id: session_id.clone(),
            },
        )
        .await;
        tracing::debug!(worktree = %wt_name, status = %status, "forwarded hook to daemon");
        return Ok(());
    }

    tracing::debug!(worktree = %wt_name, status = %status, "daemon down; writing hook to status file");
    // Fallback: mutate .swamp-status.json directly.
    let path = common.join(".swamp-status.json");
    let mut map: serde_json::Map<String, serde_json::Value> = if path.exists() {
        serde_json::from_slice(&tokio::fs::read(&path).await?).unwrap_or_default()
    } else {
        Default::default()
    };
    // Carry forward prior session_name / session_id / harness when this hook
    // omits them, so transient `working`/`idle` pings don't erase data we need
    // later: the session id to resume Claude (#33), and the per-worktree harness
    // override the user picked with `h`. Mirrors DaemonState::apply_hook.
    let prior = map.get(&wt_name).cloned();
    let prior_field = |key: &str| {
        prior
            .as_ref()
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    };
    let session_name = session_name
        .filter(|s| !s.is_empty())
        .or_else(|| prior_field("session_name"));
    let session_id = session_id
        .filter(|s| !s.is_empty())
        .or_else(|| prior_field("session_id"));
    // A hook never carries the harness override; always preserve the saved one,
    // otherwise this fallback silently reverts `choose` repos to Claude.
    let harness = prior_field("harness");

    let mut entry = serde_json::Map::new();
    entry.insert("status".into(), json!(status.to_lowercase()));
    entry.insert("ts".into(), json!(now_unix()));
    if let Some(ref name) = session_name {
        entry.insert("session_name".into(), json!(name));
    }
    if let Some(ref id) = session_id {
        entry.insert("session_id".into(), json!(id));
    }
    if let Some(ref h) = harness {
        entry.insert("harness".into(), json!(h));
    }
    map.insert(wt_name, json!(entry));
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(&map)?).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}
