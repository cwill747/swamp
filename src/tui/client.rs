use super::event::AppEvent;
use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::daemon::{self};
use crate::worktree::BranchInfo;
use anyhow::Result;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// Ask the daemon for the branch list (for the create picker). The connection
/// also receives periodic broadcasts (snapshots/resources), so skip any frame
/// that isn't the reply we asked for.
pub(super) async fn request_branches(common: &std::path::Path) -> Result<Vec<BranchInfo>> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::ListBranches).await?;
    loop {
        match read_server_msg(&mut stream).await? {
            Some(ServerMsg::Branches { branches }) => return Ok(branches),
            Some(ServerMsg::Err { message }) => anyhow::bail!(message),
            Some(_) => continue, // stray broadcast; keep reading
            None => return Ok(Vec::new()),
        }
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
        Some(ServerMsg::Err { message }) => {
            let _ = tx.send(AppEvent::ActionError(message)).await;
        }
        Some(ServerMsg::ErrDirty { name }) => {
            let _ = tx.send(AppEvent::DeleteNeedsForce(name)).await;
        }
        _ => {}
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
    if let Some(msg) = read_server_msg(&mut stream).await?
        && let ServerMsg::RefreshDone { worktree_names } = msg
    {
        let _ = tx.send(AppEvent::RefreshDone(worktree_names)).await;
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
    // Skip unrelated broadcasts (Snapshot/Resources/PrStatus) that may race
    // ahead of the actual reply on this subscribed connection, so we report the
    // true update outcome rather than clearing on the first frame.
    let done = loop {
        match read_server_msg(&mut stream).await? {
            Some(ServerMsg::Ok) => break Ok(()),
            Some(ServerMsg::Err { message }) => break Err(message),
            Some(_) => continue, // stray broadcast; keep reading
            None => break Ok(()),
        }
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
        match msg {
            ServerMsg::Snapshot(s) => {
                if tx.send(AppEvent::Snapshot(s)).await.is_err() {
                    break;
                }
            }
            ServerMsg::Resources(r) => {
                if tx.send(AppEvent::Resources(r)).await.is_err() {
                    break;
                }
            }
            ServerMsg::PrStatus(pr) => {
                if tx.send(AppEvent::PrStatus(pr)).await.is_err() {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
