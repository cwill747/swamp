use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::daemon::{self};
use crate::util::now_unix;
use crate::worktree::{git_common_dir, list_worktrees};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;
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
    let wt_name = resolve_worktree_name(&common, &start).unwrap_or_else(|| {
        start
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into())
    });

    let sock = daemon::socket_path(&common)?;
    // Try daemon first.
    if sock.exists() {
        match forward_to_daemon(
            &sock,
            ClientMsg::Hook {
                worktree: wt_name.clone(),
                status: status.clone(),
                session_name: session_name.clone(),
                session_id: session_id.clone(),
            },
        )
        .await
        {
            Ok(()) => {
                tracing::debug!(worktree = %wt_name, status = %status, "forwarded hook to daemon");
                return Ok(());
            }
            Err(e) => {
                tracing::debug!(worktree = %wt_name, status = %status, "daemon hook failed; falling back to status file: {e:?}");
            }
        }
    }

    tracing::debug!(worktree = %wt_name, status = %status, "daemon down; writing hook to status file");
    // Fallback: mutate .swamp-status.json directly.
    let path = common.join(".swamp-status.json");
    let mut map = read_status_map(&path).await?;
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

fn resolve_worktree_name(common: &Path, start: &Path) -> Option<String> {
    let target = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    list_worktrees(common)
        .ok()?
        .into_iter()
        .filter_map(|wt| {
            let path = std::fs::canonicalize(&wt.path).unwrap_or_else(|_| wt.path.clone());
            target.starts_with(&path).then(|| (path, wt.name()))
        })
        .max_by_key(|(path, _)| path.components().count())
        .map(|(_, name)| name)
}

async fn read_status_map(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>> {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return Ok(Default::default());
    };
    match serde_json::from_slice(&bytes) {
        Ok(map) => Ok(map),
        Err(e) => {
            let corrupt = corrupt_path(path);
            match tokio::fs::rename(path, &corrupt).await {
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
            Ok(Default::default())
        }
    }
}

fn corrupt_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.corrupt", now_unix()));
    PathBuf::from(name)
}

async fn forward_to_daemon(sock: &std::path::Path, msg: ClientMsg) -> Result<()> {
    let mut s = tokio::time::timeout(Duration::from_millis(200), UnixStream::connect(sock))
        .await
        .context("connect to daemon timed out")?
        .context("connect to daemon")?;
    tokio::time::timeout(Duration::from_millis(500), write_client_msg(&mut s, &msg))
        .await
        .context("write hook to daemon timed out")?
        .context("write hook to daemon")?;
    match tokio::time::timeout(Duration::from_millis(500), read_server_msg(&mut s))
        .await
        .context("read hook ack timed out")?
        .context("read hook ack")?
    {
        Some(ServerMsg::Ok) => Ok(()),
        Some(ServerMsg::Err { message }) => anyhow::bail!(message),
        Some(other) => anyhow::bail!("unexpected hook reply: {other:?}"),
        None => anyhow::bail!("daemon closed before hook ack"),
    }
}
