use super::resources;
use super::state::{PrSnapshot, Snapshot};
use super::Daemon;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Ping,
    Subscribe,
    Hook { worktree: String, status: String },
    GetVersion,
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Pong,
    Snapshot(Snapshot),
    Resources(resources::Snapshot),
    PrStatus(PrSnapshot),
    Ok,
    Err { message: String },
    Version { version: String },
    RefreshDone { worktree_names: Vec<String> },
}

pub async fn handle_client(daemon: Arc<Daemon>, mut stream: UnixStream) -> Result<()> {
    let mut rx = daemon.tx.subscribe();
    // Read initial message.
    loop {
        tokio::select! {
            res = read_msg(&mut stream) => {
                let Some(msg) = res? else { return Ok(()); };
                match msg {
                    ClientMsg::Ping => write_msg(&mut stream, &ServerMsg::Pong).await?,
                    ClientMsg::Subscribe => {
                        let snap = daemon.state.read().await.snapshot();
                        write_msg(&mut stream, &ServerMsg::Snapshot(snap)).await?;
                        let res = daemon.resources.read().await.clone();
                        write_msg(&mut stream, &ServerMsg::Resources(res)).await?;
                        let pr_snap = daemon.state.read().await.pr_snapshot();
                        write_msg(&mut stream, &ServerMsg::PrStatus(pr_snap)).await?;
                    }
                    ClientMsg::Hook { worktree, status } => {
                        match daemon.apply_hook(&worktree, &status).await {
                            Ok(()) => write_msg(&mut stream, &ServerMsg::Ok).await?,
                            Err(e) => write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?,
                        }
                    }
                    ClientMsg::GetVersion => {
                        write_msg(&mut stream, &ServerMsg::Version { version: env!("CARGO_PKG_VERSION").to_string() }).await?;
                    }
                    ClientMsg::Refresh => {
                        daemon.fetch_and_refresh().await;
                        let snap = daemon.state.read().await.snapshot();
                        let names: Vec<String> = snap.rows.iter().map(|r| r.name.clone()).collect();
                        write_msg(&mut stream, &ServerMsg::RefreshDone { worktree_names: names }).await?;
                    }
                }
            }
            ev = rx.recv() => {
                match ev {
                    Ok(m) => write_msg(&mut stream, &m).await?,
                    Err(_) => {} // lagged or closed; keep going
                }
            }
        }
    }
}

pub async fn read_msg(stream: &mut UnixStream) -> Result<Option<ClientMsg>> {
    let mut len_buf = [0u8; 4];
    if let Err(e) = stream.read_exact(&mut len_buf).await {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e.into());
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

pub async fn write_msg(stream: &mut UnixStream, msg: &ServerMsg) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read_server_msg(stream: &mut UnixStream) -> Result<Option<ServerMsg>> {
    let mut len_buf = [0u8; 4];
    if let Err(e) = stream.read_exact(&mut len_buf).await {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e.into());
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(Some(serde_json::from_slice(&buf)?))
}

pub async fn write_client_msg(stream: &mut UnixStream, msg: &ClientMsg) -> Result<()> {
    let bytes = serde_json::to_vec(msg)?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::{AgentStatus, WorktreeRow};
    use std::path::PathBuf;
    use tokio::net::UnixListener;

    fn make_snapshot(names: &[&str]) -> Snapshot {
        Snapshot {
            rows: names
                .iter()
                .map(|n| WorktreeRow {
                    name: n.to_string(),
                    path: PathBuf::from(format!("/repo/{}", n)),
                    branch: n.to_string(),
                    upstream: None,
                    ahead: 0,
                    behind: 0,
                    staged: 0,
                    unstaged: 0,
                    untracked: 0,
                    conflict: false,
                    rebase: false,
                    agent: AgentStatus::Idle,
                    agent_ts: 0,
                })
                .collect(),
        }
    }

    /// ClientMsg variants round-trip through JSON length-prefixed encoding
    /// without data loss.
    #[test]
    fn client_msg_serde_roundtrip() {
        let msgs = vec![
            ClientMsg::Ping,
            ClientMsg::Subscribe,
            ClientMsg::Hook {
                worktree: "main".into(),
                status: "working".into(),
            },
            ClientMsg::GetVersion,
            ClientMsg::Refresh,
        ];
        for msg in &msgs {
            let json = serde_json::to_vec(msg).unwrap();
            let decoded: ClientMsg = serde_json::from_slice(&json).unwrap();
            // Re-encode and compare bytes as a proxy for equality.
            assert_eq!(json, serde_json::to_vec(&decoded).unwrap());
        }
    }

    /// GetVersion / Version round-trip over a live Unix socket pair.
    #[tokio::test]
    async fn get_version_socket_roundtrip() {
        let tmp = std::env::temp_dir()
            .join(format!("swamp-test-ver-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&tmp).unwrap();

        let handle = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let msg = read_msg(&mut s).await.unwrap().unwrap();
            assert!(matches!(msg, ClientMsg::GetVersion));
            write_msg(&mut s, &ServerMsg::Version { version: "1.2.3".into() })
                .await
                .unwrap();
        });

        let mut client = tokio::net::UnixStream::connect(&tmp).await.unwrap();
        write_client_msg(&mut client, &ClientMsg::GetVersion)
            .await
            .unwrap();
        let resp = read_server_msg(&mut client).await.unwrap().unwrap();
        if let ServerMsg::Version { version } = resp {
            assert_eq!(version, "1.2.3");
        } else {
            panic!("expected Version response");
        }

        handle.await.unwrap();
        let _ = std::fs::remove_file(&tmp);
    }

    /// ServerMsg::Snapshot round-trips: rows survive encode → decode.
    #[test]
    fn server_snapshot_serde_roundtrip() {
        let snap = make_snapshot(&["alpha", "beta", "main"]);
        let msg = ServerMsg::Snapshot(snap);
        let json = serde_json::to_vec(&msg).unwrap();
        let decoded: ServerMsg = serde_json::from_slice(&json).unwrap();
        if let ServerMsg::Snapshot(s) = decoded {
            assert_eq!(s.rows.len(), 3);
            assert_eq!(s.rows[0].name, "alpha");
            assert_eq!(s.rows[2].name, "main");
        } else {
            panic!("expected Snapshot variant");
        }
    }

    /// End-to-end socket round-trip: write_client_msg / read_msg and
    /// write_msg / read_server_msg must be inverses over a Unix socket pair.
    #[tokio::test]
    async fn socket_framing_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("swamp-test-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&tmp).unwrap();

        let snap = make_snapshot(&["main", "feat"]);
        let server_snap = snap.clone();
        let handle = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            // Read client Subscribe.
            let msg = read_msg(&mut s).await.unwrap().unwrap();
            assert!(matches!(msg, ClientMsg::Subscribe));
            // Write snapshot back.
            write_msg(&mut s, &ServerMsg::Snapshot(server_snap))
                .await
                .unwrap();
        });

        let mut client = tokio::net::UnixStream::connect(&tmp).await.unwrap();
        write_client_msg(&mut client, &ClientMsg::Subscribe)
            .await
            .unwrap();
        let resp = read_server_msg(&mut client).await.unwrap().unwrap();
        if let ServerMsg::Snapshot(s) = resp {
            assert_eq!(s.rows.len(), 2);
            assert_eq!(s.rows[0].name, "main");
        } else {
            panic!("expected Snapshot");
        }

        handle.await.unwrap();
        let _ = std::fs::remove_file(&tmp);
    }
}
