use super::event::AppEvent;
use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::daemon::{self};
use crate::worktree::BranchInfo;
use anyhow::Result;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// Ask the daemon for the branch list (for the create picker).
pub(super) async fn request_branches(common: &std::path::Path) -> Result<Vec<BranchInfo>> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::ListBranches).await?;
    match read_server_msg(&mut stream).await? {
        Some(ServerMsg::Branches { branches }) => Ok(branches),
        Some(ServerMsg::Err { message }) => anyhow::bail!(message),
        Some(other) => anyhow::bail!("unexpected branch-list reply: {other:?}"),
        None => anyhow::bail!("daemon closed before branch-list reply"),
    }
}

/// Send a create/remove request to the daemon and forward any error message
/// back to the UI. Success is observed via the broadcast snapshot.
pub(super) async fn send_action(
    common: &std::path::Path,
    msg: ClientMsg,
    tx: mpsc::Sender<AppEvent>,
) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &msg).await?;
    match read_server_msg(&mut stream).await? {
        Some(ServerMsg::Ok) => {}
        Some(ServerMsg::Err { message }) => {
            let _ = tx.send(AppEvent::ActionError(message)).await;
        }
        Some(ServerMsg::ErrDirty { name, reason }) => {
            let _ = tx.send(AppEvent::DeleteNeedsForce(name, reason)).await;
        }
        Some(other) => anyhow::bail!("unexpected action reply: {other:?}"),
        None => anyhow::bail!("daemon closed before action reply"),
    }
    Ok(())
}

pub(super) async fn send_refresh(
    common: &std::path::Path,
    tx: mpsc::Sender<AppEvent>,
) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::Refresh).await?;
    match read_server_msg(&mut stream).await? {
        Some(ServerMsg::RefreshDone { worktree_names }) => {
            let _ = tx.send(AppEvent::RefreshDone(Ok(worktree_names))).await;
        }
        Some(ServerMsg::Err { message }) => {
            let _ = tx.send(AppEvent::RefreshDone(Err(message))).await;
        }
        Some(other) => {
            let _ = tx
                .send(AppEvent::RefreshDone(Err(format!(
                    "unexpected refresh reply: {other:?}"
                ))))
                .await;
        }
        None => {
            let _ = tx
                .send(AppEvent::RefreshDone(Err(
                    "daemon closed before refresh reply".into(),
                )))
                .await;
        }
    }
    Ok(())
}

/// Ask the daemon to fetch and fast-forward the default branch, then report the
/// outcome so the footer status line can clear (or show an error).
pub(super) async fn send_update(
    common: &std::path::Path,
    tx: mpsc::Sender<AppEvent>,
) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::UpdateDefault).await?;
    let done = match read_server_msg(&mut stream).await? {
        Some(ServerMsg::Ok) => Ok(()),
        Some(ServerMsg::Err { message }) => Err(message),
        Some(other) => Err(format!("unexpected update reply: {other:?}")),
        None => Err("daemon closed before update reply".into()),
    };
    let _ = tx.send(AppEvent::UpdateDone(done)).await;
    Ok(())
}

pub(super) async fn subscribe_loop(
    common: &std::path::Path,
    tx: mpsc::Sender<AppEvent>,
) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::Subscribe).await?;
    while let Some(msg) = read_server_msg(&mut stream).await? {
        let event = match msg {
            ServerMsg::Snapshot(s) => AppEvent::Snapshot(s),
            ServerMsg::Resources(r) => AppEvent::Resources(r),
            ServerMsg::PrStatus(pr) => AppEvent::PrStatus(pr),
            _ => continue,
        };
        if tx.send(event).await.is_err() {
            break;
        }
    }
    Ok(())
}
