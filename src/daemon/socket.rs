use super::Daemon;
use super::resources;
use super::state::{PrSnapshot, Snapshot};
use crate::config::Harness;
use crate::worktree::BranchInfo;
use anyhow::Result;
use git2::Reference;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Ping,
    Subscribe,
    Hook {
        worktree: String,
        status: String,
        session_name: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
    GetVersion,
    Refresh,
    /// Fetch all remotes and fast-forward the default branch in its worktree —
    /// the equivalent of the old `git wt update`.
    UpdateDefault,
    ListBranches,
    CreateWorktree {
        branch: String,
    },
    CreateWorktreeFromBase {
        branch: String,
        base: String,
    },
    RemoveWorktree {
        name: String,
        #[serde(default)]
        force: bool,
    },
    /// Set the per-worktree harness override (worktrees pane `h`); applies the
    /// next time swamp builds that worktree's tab.
    SetHarness {
        worktree: String,
        harness: Harness,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Pong,
    Snapshot(Snapshot),
    Resources(resources::Snapshot),
    PrStatus(PrSnapshot),
    Ok,
    Err {
        message: String,
    },
    /// A non-forced `RemoveWorktree` was refused. The client may retry with
    /// `force: true`. `reason` is a short human-readable description of why
    /// the removal was refused (e.g. "has uncommitted changes").
    ErrDirty {
        name: String,
        reason: String,
    },
    Version {
        version: String,
    },
    RefreshDone {
        worktree_names: Vec<String>,
    },
    Branches {
        branches: Vec<BranchInfo>,
    },
}

pub async fn handle_client(daemon: Arc<Daemon>, mut stream: UnixStream) -> Result<()> {
    let mut rx = daemon.tx.subscribe();
    // Only clients that send `Subscribe` want the broadcast stream. Short-lived
    // clients (the liveness `probe`, `GetVersion`, `Hook`, one-shot `Refresh`)
    // connect, exchange one request/reply, and close. Keep the broadcast arm
    // disabled before subscription so resource ticks cannot cancel and restart
    // the pre-subscribe read timeout.
    let mut subscription: Option<PrSubscription> = None;
    loop {
        tokio::select! {
            res = read_client_msg(&mut stream, subscription.is_some()) => {
                let Some(msg) = res? else { return Ok(()); };
                match msg {
                    ClientMsg::Ping => write_msg(&mut stream, &ServerMsg::Pong).await?,
                    ClientMsg::Subscribe => {
                        if subscription.is_none() {
                            subscription = Some(PrSubscription::new(daemon.clone()));
                        }
                        let snap = daemon.state.read().await.snapshot();
                        write_msg(&mut stream, &ServerMsg::Snapshot(snap)).await?;
                        let res = daemon.resources.read().await.clone();
                        write_msg(&mut stream, &ServerMsg::Resources(res)).await?;
                        let pr_snap = daemon.state.read().await.pr_snapshot();
                        // Kick the poller to fetch now (rather than on its next
                        // cadence) when no fetch has resolved yet. `Notify`
                        // coalesces concurrent subscribers into a single wake, and
                        // the poller serializes fetches, so this can't fan out.
                        if pr_snap.loading {
                            daemon.pr_wake.notify_one();
                        }
                        write_msg(&mut stream, &ServerMsg::PrStatus(pr_snap)).await?;
                    }
                    ClientMsg::Hook { worktree, status, session_name, session_id } => {
                        if let Err(e) = validate_hook(&worktree, session_name.as_deref(), session_id.as_deref()) {
                            write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?;
                            continue;
                        }
                        match daemon.apply_hook(&worktree, &status, session_name.as_deref(), session_id.as_deref()).await {
                            Ok(()) => write_msg(&mut stream, &ServerMsg::Ok).await?,
                            Err(e) => write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?,
                        }
                    }
                    ClientMsg::GetVersion => {
                        write_msg(&mut stream, &ServerMsg::Version { version: env!("CARGO_PKG_VERSION").to_string() }).await?;
                    }
                    ClientMsg::Refresh => {
                        tracing::info!(trigger = "tui_refresh", "client requested refresh");
                        match daemon.fetch_and_refresh().await {
                            Ok(()) => {
                                let snap = daemon.state.read().await.snapshot();
                                let names: Vec<String> = snap.rows.iter().map(|r| r.name.clone()).collect();
                                write_msg(&mut stream, &ServerMsg::RefreshDone { worktree_names: names }).await?;
                            }
                            Err(e) => write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?,
                        }
                    }
                    ClientMsg::UpdateDefault => {
                        tracing::info!(trigger = "update_default", "client requested default-branch update");
                        match daemon.update_default().await {
                            Ok(()) => write_msg(&mut stream, &ServerMsg::Ok).await?,
                            Err(e) => write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?,
                        }
                    }
                    ClientMsg::ListBranches => {
                        match daemon.list_branches().await {
                            Ok(branches) => {
                                write_msg(&mut stream, &ServerMsg::Branches { branches }).await?
                            }
                            Err(e) => {
                                write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?
                            }
                        }
                    }
                    ClientMsg::CreateWorktree { branch } => {
                        if let Err(e) = validate_branch_name(&branch) {
                            write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?;
                            continue;
                        }
                        match daemon.create_worktree(&branch).await {
                            Ok(()) => write_msg(&mut stream, &ServerMsg::Ok).await?,
                            Err(e) => write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?,
                        }
                    }
                    ClientMsg::CreateWorktreeFromBase { branch, base } => {
                        if let Err(e) = validate_branch_name(&branch).and_then(|_| validate_refish("base", &base)) {
                            write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?;
                            continue;
                        }
                        match daemon.create_worktree_from_base(&branch, &base).await {
                            Ok(()) => write_msg(&mut stream, &ServerMsg::Ok).await?,
                            Err(e) => write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?,
                        }
                    }
                    ClientMsg::SetHarness { worktree, harness } => {
                        if let Err(e) = validate_worktree_name(&worktree) {
                            write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?;
                            continue;
                        }
                        match daemon.set_harness(&worktree, harness).await {
                            Ok(()) => write_msg(&mut stream, &ServerMsg::Ok).await?,
                            Err(e) => write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?,
                        }
                    }
                    ClientMsg::RemoveWorktree { name, force } => {
                        if let Err(e) = validate_worktree_name(&name) {
                            write_msg(&mut stream, &ServerMsg::Err { message: e.to_string() }).await?;
                            continue;
                        }
                        match daemon.remove_worktree(&name, force).await {
                            Ok(()) => write_msg(&mut stream, &ServerMsg::Ok).await?,
                            Err(e) => {
                                // Surface a refused-removal distinctly so the TUI
                                // can offer a force override.
                                let reply = match e.downcast_ref::<crate::worktree::RemoveRefused>() {
                                    Some(r) => ServerMsg::ErrDirty {
                                        name: r.name.clone(),
                                        reason: r.reason.description().to_string(),
                                    },
                                    None => ServerMsg::Err { message: e.to_string() },
                                };
                                write_msg(&mut stream, &reply).await?
                            }
                        }
                    }
                }
            }
            ev = rx.recv(), if subscription.is_some() => {
                // Err means lagged or closed; keep going.
                if let Ok(m) = ev {
                    write_msg(&mut stream, &m).await?;
                }
            }
        }
    }
}

struct PrSubscription {
    daemon: Arc<Daemon>,
}

impl PrSubscription {
    fn new(daemon: Arc<Daemon>) -> Self {
        daemon.pr_subscribers.fetch_add(1, Ordering::Relaxed);
        Self { daemon }
    }
}

impl Drop for PrSubscription {
    fn drop(&mut self) {
        self.daemon.pr_subscribers.fetch_sub(1, Ordering::Relaxed);
    }
}

const PROTOCOL_MAGIC: &[u8; 4] = b"SWP1";
const CLIENT_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLIENT_FRAME_LEN: usize = 256 * 1024;
const MAX_SERVER_FRAME_LEN: usize = 16 * 1024 * 1024;
const MAX_NAME_LEN: usize = 255;
const MAX_SESSION_FIELD_LEN: usize = 512;

async fn read_client_msg(stream: &mut UnixStream, subscribed: bool) -> Result<Option<ClientMsg>> {
    if subscribed {
        read_msg(stream).await
    } else {
        match tokio::time::timeout(CLIENT_READ_TIMEOUT, read_msg(stream)).await {
            Ok(res) => res,
            Err(_) => anyhow::bail!("timed out waiting for client message"),
        }
    }
}

pub async fn read_msg(stream: &mut UnixStream) -> Result<Option<ClientMsg>> {
    read_framed_json(stream, MAX_CLIENT_FRAME_LEN).await
}

pub async fn write_msg(stream: &mut UnixStream, msg: &ServerMsg) -> Result<()> {
    write_framed_json(stream, msg).await
}

pub async fn read_server_msg(stream: &mut UnixStream) -> Result<Option<ServerMsg>> {
    read_framed_json(stream, MAX_SERVER_FRAME_LEN).await
}

pub async fn write_client_msg(stream: &mut UnixStream, msg: &ClientMsg) -> Result<()> {
    write_framed_json(stream, msg).await
}

async fn read_framed_json<T: for<'de> Deserialize<'de>>(
    stream: &mut UnixStream,
    max_frame_len: usize,
) -> Result<Option<T>> {
    let mut len_buf = [0u8; 4];
    if let Err(e) = stream.read_exact(&mut len_buf).await {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e.into());
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max_frame_len {
        anyhow::bail!(
            "incoming frame length {len} exceeds maximum {max_frame_len}; dropping connection"
        );
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let payload = buf.strip_prefix(PROTOCOL_MAGIC).unwrap_or(&buf);
    Ok(Some(serde_json::from_slice(payload)?))
}

async fn write_framed_json<T: Serialize>(stream: &mut UnixStream, msg: &T) -> Result<()> {
    let payload = serde_json::to_vec(msg)?;
    let len = PROTOCOL_MAGIC.len() + payload.len();
    stream.write_all(&(len as u32).to_be_bytes()).await?;
    stream.write_all(PROTOCOL_MAGIC).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

fn validate_branch_name(branch: &str) -> Result<()> {
    validate_refish("branch", branch)?;
    if branch.starts_with('-') {
        anyhow::bail!("branch must not start with '-'");
    }
    let refname = format!("refs/heads/{branch}");
    if !Reference::is_valid_name(&refname) {
        anyhow::bail!("invalid branch name: {branch}");
    }
    Ok(())
}

fn validate_hook(
    worktree: &str,
    session_name: Option<&str>,
    session_id: Option<&str>,
) -> Result<()> {
    validate_worktree_name(worktree)?;
    validate_optional_field("session_name", session_name, MAX_SESSION_FIELD_LEN)?;
    validate_optional_field("session_id", session_id, MAX_SESSION_FIELD_LEN)
}

fn validate_refish(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("{field} must not be empty");
    }
    if value.len() > MAX_NAME_LEN {
        anyhow::bail!("{field} exceeds {MAX_NAME_LEN} bytes");
    }
    if value.chars().any(|c| c.is_control()) {
        anyhow::bail!("{field} contains a control character");
    }
    Ok(())
}

fn validate_worktree_name(name: &str) -> Result<()> {
    validate_refish("worktree", name)?;
    if name == "." || name == ".." {
        anyhow::bail!("worktree name must not be '.' or '..'");
    }
    if name.contains('/') || name.contains('\\') {
        anyhow::bail!("worktree name must not contain path separators");
    }
    Ok(())
}

fn validate_optional_field(field: &str, value: Option<&str>, max_len: usize) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > max_len {
        anyhow::bail!("{field} exceeds {max_len} bytes");
    }
    if value.chars().any(|c| c.is_control()) {
        anyhow::bail!("{field} contains a control character");
    }
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
                    upstream_gone: false,
                    ahead: 0,
                    behind: 0,
                    staged: 0,
                    unstaged: 0,
                    untracked: 0,
                    conflict: false,
                    rebase: false,
                    agent: AgentStatus::Idle,
                    agent_ts: 0,
                    session_name: None,
                    head_ts: 0,
                    harness: None,
                    is_default: false,
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
                session_name: None,
                session_id: None,
            },
            ClientMsg::GetVersion,
            ClientMsg::Refresh,
            ClientMsg::UpdateDefault,
            ClientMsg::ListBranches,
            ClientMsg::CreateWorktree {
                branch: "feature/x".into(),
            },
            ClientMsg::CreateWorktreeFromBase {
                branch: "feature/x".into(),
                base: "main".into(),
            },
            ClientMsg::RemoveWorktree {
                name: "feature-x".into(),
                force: false,
            },
            ClientMsg::SetHarness {
                worktree: "feature-x".into(),
                harness: Harness::Codex,
            },
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
        let tmp = std::env::temp_dir().join(format!("swamp-test-ver-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&tmp).unwrap();

        let handle = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let msg = read_msg(&mut s).await.unwrap().unwrap();
            assert!(matches!(msg, ClientMsg::GetVersion));
            write_msg(
                &mut s,
                &ServerMsg::Version {
                    version: "1.2.3".into(),
                },
            )
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

    #[tokio::test]
    async fn write_client_msg_prefixes_protocol_magic() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write_client_msg(&mut client, &ClientMsg::Ping)
            .await
            .unwrap();

        let mut len_buf = [0u8; 4];
        server.read_exact(&mut len_buf).await.unwrap();
        let len = u32::from_be_bytes(len_buf) as usize;
        assert!(len > PROTOCOL_MAGIC.len());

        let mut magic = [0u8; 4];
        server.read_exact(&mut magic).await.unwrap();
        assert_eq!(&magic, PROTOCOL_MAGIC);
    }

    #[tokio::test]
    async fn read_msg_accepts_legacy_json_frame_without_magic() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let payload = serde_json::to_vec(&ClientMsg::Ping).unwrap();
        writer
            .write_all(&(payload.len() as u32).to_be_bytes())
            .await
            .unwrap();
        writer.write_all(&payload).await.unwrap();

        assert!(matches!(
            read_msg(&mut reader).await.unwrap(),
            Some(ClientMsg::Ping)
        ));
    }

    /// A 4-byte length header of 0xFFFFFFFF (4 GiB) must be rejected by
    /// read_msg before any allocation attempt, instead of OOM-killing the
    /// daemon.
    #[tokio::test]
    async fn read_msg_rejects_oversized_frame() {
        let tmp =
            std::env::temp_dir().join(format!("swamp-test-oversized-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&tmp).unwrap();

        let handle = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            // Write a header claiming 4 GiB of payload.
            use tokio::io::AsyncWriteExt;
            s.write_all(&0xFFFF_FFFFu32.to_be_bytes()).await.unwrap();
        });

        let mut server_side = tokio::net::UnixStream::connect(&tmp).await.unwrap();
        let err = read_msg(&mut server_side).await.unwrap_err();
        assert!(
            err.to_string().contains("exceeds maximum"),
            "expected cap error, got: {err}"
        );

        handle.await.unwrap();
        let _ = std::fs::remove_file(&tmp);
    }

    /// Same guard for read_server_msg: a huge length prefix is rejected.
    #[tokio::test]
    async fn read_server_msg_rejects_oversized_frame() {
        let tmp = std::env::temp_dir().join(format!(
            "swamp-test-oversized-srv-{}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&tmp).unwrap();

        let handle = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncWriteExt;
            s.write_all(&0xFFFF_FFFFu32.to_be_bytes()).await.unwrap();
        });

        let mut client = tokio::net::UnixStream::connect(&tmp).await.unwrap();
        let err = read_server_msg(&mut client).await.unwrap_err();
        assert!(
            err.to_string().contains("exceeds maximum"),
            "expected cap error, got: {err}"
        );

        handle.await.unwrap();
        let _ = std::fs::remove_file(&tmp);
    }

    /// A client frame one byte over the cap must be rejected before allocation.
    #[tokio::test]
    async fn read_msg_rejects_one_byte_over_client_cap() {
        // Just above cap — no socket needed, just check the arithmetic.
        let tmp_over =
            std::env::temp_dir().join(format!("swamp-test-over-cap-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&tmp_over).unwrap();

        let over_len = (MAX_CLIENT_FRAME_LEN + 1) as u32;
        let handle = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncWriteExt;
            s.write_all(&over_len.to_be_bytes()).await.unwrap();
        });

        let mut client = tokio::net::UnixStream::connect(&tmp_over).await.unwrap();
        let err = read_msg(&mut client).await.unwrap_err();
        assert!(
            err.to_string().contains("exceeds maximum"),
            "expected cap error for over-cap frame, got: {err}"
        );

        handle.await.unwrap();
        let _ = std::fs::remove_file(&tmp_over);
    }

    #[test]
    fn validates_branch_names_at_daemon_boundary() {
        assert!(validate_branch_name("feature/login").is_ok());
        assert!(validate_branch_name("").is_err());
        assert!(validate_branch_name("-feature").is_err());
        assert!(validate_branch_name("feature..login").is_err());
        assert!(validate_branch_name("feature\nlogin").is_err());
    }

    #[test]
    fn validates_worktree_names_at_daemon_boundary() {
        assert!(validate_worktree_name("feature-login").is_ok());
        assert!(validate_worktree_name("").is_err());
        assert!(validate_worktree_name(".").is_err());
        assert!(validate_worktree_name("..").is_err());
        assert!(validate_worktree_name("feature/login").is_err());
        assert!(validate_worktree_name("feature\\login").is_err());
    }

    #[test]
    fn validates_session_fields_at_daemon_boundary() {
        assert!(validate_hook("main", Some("session"), Some("id")).is_ok());
        assert!(validate_hook("main", Some("bad\nsession"), None).is_err());
        assert!(validate_hook("main", Some(&"x".repeat(MAX_SESSION_FIELD_LEN + 1)), None).is_err());
    }
}
