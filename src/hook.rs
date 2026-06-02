use crate::daemon::socket::{write_client_msg, ClientMsg};
use crate::daemon::{self};
use crate::util::now_unix;
use crate::worktree::git_common_dir;
use anyhow::{Context, Result};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;

pub async fn run(status: String, dir: Option<PathBuf>) -> Result<()> {
    let start = dir.unwrap_or(std::env::current_dir()?);
    let common = git_common_dir(&start).context("not inside a git repo")?;
    let wt_name = start
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".into());

    let sock = daemon::socket_path(&common);
    // Try daemon first.
    if sock.exists() {
        if let Ok(Ok(mut s)) =
            tokio::time::timeout(Duration::from_millis(200), UnixStream::connect(&sock)).await
        {
            let _ = write_client_msg(
                &mut s,
                &ClientMsg::Hook {
                    worktree: wt_name.clone(),
                    status: status.clone(),
                },
            )
            .await;
            return Ok(());
        }
    }

    // Fallback: mutate .swamp-status.json directly.
    let path = common.join(".swamp-status.json");
    let mut map: serde_json::Map<String, serde_json::Value> = if path.exists() {
        serde_json::from_slice(&tokio::fs::read(&path).await?).unwrap_or_default()
    } else {
        Default::default()
    };
    map.insert(
        wt_name,
        json!({ "status": status.to_lowercase(), "ts": now_unix() }),
    );
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec_pretty(&map)?).await?;
    tokio::fs::rename(&tmp, &path).await?;
    Ok(())
}
