//! Private, versioned workspace broker protocol.
use super::Repository;
use crate::daemon::resources;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u16 = 1;
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;
pub const SOCKET_NAME: &str = "workspace.sock";
const MAGIC: &[u8; 4] = b"WSK1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Subscribe {
        version: u16,
        selected_repo: Option<String>,
    },
    SelectRepo {
        repo_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    State(WorkspaceState),
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub version: u16,
    pub revision: u64,
    pub repositories: Vec<Repository>,
    pub selected_repo: Option<String>,
    pub resource: ResourceState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResourceState {
    Loading {
        repo_id: String,
        generation: u64,
        #[serde(default)]
        snapshot: Option<resources::Snapshot>,
    },
    Unavailable {
        repo_id: String,
        generation: u64,
        #[serde(default)]
        snapshot: Option<resources::Snapshot>,
    },
    Disconnected {
        repo_id: String,
        generation: u64,
        #[serde(default)]
        snapshot: Option<resources::Snapshot>,
    },
    Ready {
        repo_id: String,
        generation: u64,
        snapshot: resources::Snapshot,
    },
}

pub async fn write<S: AsyncWrite + Unpin, T: Serialize>(stream: &mut S, value: &T) -> Result<()> {
    let body = serde_json::to_vec(value).context("encode workspace protocol message")?;
    if body.len() > MAX_FRAME_LEN {
        bail!("workspace frame exceeds {MAX_FRAME_LEN} bytes");
    }
    stream.write_all(MAGIC).await?;
    stream.write_all(&(body.len() as u32).to_be_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read<T: DeserializeOwned, S: AsyncRead + Unpin>(stream: &mut S) -> Result<Option<T>> {
    let mut magic = [0; 4];
    match stream.read_exact(&mut magic).await {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    if &magic != MAGIC {
        bail!("unsupported workspace protocol framing");
    }
    let len = stream.read_u32().await? as usize;
    if len == 0 || len > MAX_FRAME_LEN {
        bail!("invalid workspace frame length {len}");
    }
    let mut body = vec![0; len];
    stream.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .context("decode workspace protocol message")
        .map(Some)
}

pub fn socket_path(runtime_dir: &std::path::Path) -> PathBuf {
    runtime_dir.join(SOCKET_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn framed_messages_round_trip_and_reject_oversized_frames() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let message = ClientMessage::Subscribe {
            version: VERSION,
            selected_repo: Some("repo-id".into()),
        };
        write(&mut a, &message).await.unwrap();
        let decoded: ClientMessage = read(&mut b).await.unwrap().unwrap();
        assert!(
            matches!(decoded, ClientMessage::Subscribe { version: VERSION, selected_repo: Some(id) } if id == "repo-id")
        );

        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(MAGIC).await.unwrap();
        a.write_all(&((MAX_FRAME_LEN as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(read::<ClientMessage, _>(&mut b).await.is_err());
    }
}
