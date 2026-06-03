pub mod resources;
pub mod socket;
pub mod state;
pub mod watcher;

use crate::util::repo_id;
use crate::worktree::{git_common_dir, resolve_git_dir};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::sync::{broadcast, RwLock};

use self::socket::{ClientMsg, ServerMsg};
use self::state::DaemonState;

pub struct Daemon {
    pub common_dir: PathBuf,
    pub session_name: String,
    pub state: Arc<RwLock<DaemonState>>,
    pub resources: Arc<RwLock<resources::Snapshot>>,
    pub tx: broadcast::Sender<ServerMsg>,
}

pub fn socket_path(common_dir: &Path) -> PathBuf {
    let id = repo_id(common_dir);
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("swamp").join(format!("{}.sock", id))
}

pub fn pid_path(common_dir: &Path) -> PathBuf {
    socket_path(common_dir).with_extension("pid")
}

pub async fn serve(dir: Option<PathBuf>, foreground: bool) -> Result<()> {
    let start = match dir {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let start = resolve_git_dir(&start);
    let common = git_common_dir(&start).context("not inside a git repo")?;
    let sock = socket_path(&common);
    if let Some(parent) = sock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Remove a stale socket if the previous daemon died.
    if sock.exists() {
        if probe(&sock).await.is_ok() {
            anyhow::bail!("swamp serve already running for {}", common.display());
        }
        let _ = std::fs::remove_file(&sock);
    }

    if !foreground {
        // crude double-fork via spawning ourselves.
        let me = std::env::current_exe()?;
        std::process::Command::new(me)
            .arg("serve")
            .arg("--foreground")
            .arg(start.display().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("spawn detached daemon")?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let state = Arc::new(RwLock::new(DaemonState::load(&common).await?));
    let (tx, _) = broadcast::channel::<ServerMsg>(64);

    // Session name matches launch::run's derivation: the file_name of the dir
    // that contains the bare repo / .git. Prefer the ZELLIJ_SESSION_NAME env if
    // present (set inside any zellij pane), so the daemon agrees with zellij
    // even when started from an unusual cwd.
    let session_name = std::env::var("ZELLIJ_SESSION_NAME").ok().unwrap_or_else(|| {
        common
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "swamp".into())
    });

    let daemon = Arc::new(Daemon {
        common_dir: common.clone(),
        session_name,
        state: state.clone(),
        resources: Arc::new(RwLock::new(resources::Snapshot::default())),
        tx: tx.clone(),
    });

    // Initial scan.
    daemon.refresh_all().await?;

    // Watcher task.
    {
        let d = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = watcher::run(d).await {
                tracing::error!("watcher exited: {e:?}");
            }
        });
    }

    // Resource sampler (1Hz). Uses spawn_blocking to run ps/sysctl shell-outs
    // off the async runtime and stores results back in the daemon's cache,
    // broadcasting a Resources message to subscribers.
    {
        let d = daemon.clone();
        tokio::spawn(async move {
            let mut roots: Vec<u32> = Vec::new();
            loop {
                let session = d.session_name.clone();
                let roots_in = roots.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let mut r = roots_in;
                    let snap = resources::sample(&session, &mut r);
                    (snap, r)
                })
                .await;
                match result {
                    Ok((Ok(snap), new_roots)) => {
                        roots = new_roots;
                        *d.resources.write().await = snap.clone();
                        let _ = d.tx.send(ServerMsg::Resources(snap));
                    }
                    Ok((Err(e), _)) => tracing::debug!("resource sample: {e:?}"),
                    Err(e) => tracing::warn!("resource sampler join: {e:?}"),
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    }

    // Heartbeat refresher.
    {
        let d = daemon.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tick.tick().await;
                if let Err(e) = d.refresh_all().await {
                    tracing::warn!("heartbeat refresh: {e:?}");
                }
            }
        });
    }

    // Periodic git fetch (replaces lazygit autoFetch to avoid 1Password SSH prompts).
    {
        let d = daemon.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(300));
            tick.tick().await; // skip immediate first tick
            loop {
                tick.tick().await;
                tracing::debug!("running periodic git fetch");
                let result = tokio::process::Command::new("git")
                    .arg("-C")
                    .arg(&d.common_dir)
                    .args(["fetch", "--all", "--prune"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
                match result {
                    Ok(s) if s.success() => {
                        if let Err(e) = d.refresh_all().await {
                            tracing::warn!("post-fetch refresh: {e:?}");
                        }
                    }
                    Ok(s) => tracing::warn!("git fetch exited {s}"),
                    Err(e) => tracing::warn!("git fetch failed: {e}"),
                }
            }
        });
    }

    // PR status poller (60s interval).
    {
        let d = daemon.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            loop {
                let common_dir = d.common_dir.clone();
                let branches: Vec<String> = {
                    let s = d.state.read().await;
                    s.rows.values().map(|r| r.branch.clone()).collect()
                };
                let result = tokio::task::spawn_blocking(move || {
                    crate::github::list_prs_for_branches(&common_dir, &branches)
                })
                .await;
                match result {
                    Ok(Ok(prs)) => {
                        let mut s = d.state.write().await;
                        s.update_prs(prs);
                        let pr_snap = s.pr_snapshot();
                        drop(s);
                        let _ = d.tx.send(ServerMsg::PrStatus(pr_snap));
                    }
                    Ok(Err(e)) => tracing::debug!("pr status poll: {e:?}"),
                    Err(e) => tracing::warn!("pr status poll join: {e:?}"),
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    let listener = UnixListener::bind(&sock).context("bind socket")?;
    std::fs::write(pid_path(&common), std::process::id().to_string())?;
    tracing::info!("swamp daemon listening on {}", sock.display());

    loop {
        let (stream, _) = listener.accept().await?;
        let d = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = socket::handle_client(d, stream).await {
                tracing::debug!("client: {e:?}");
            }
        });
    }
}

impl Daemon {
    pub async fn refresh_all(&self) -> Result<()> {
        let mut s = self.state.write().await;
        s.refresh_git(&self.common_dir)?;
        let snap = s.snapshot();
        drop(s);
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        Ok(())
    }

    pub async fn fetch_and_refresh(&self) {
        let result = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.common_dir)
            .args(["fetch", "--all", "--prune"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
        match result {
            Ok(s) if s.success() => {
                if let Err(e) = self.refresh_all().await {
                    tracing::warn!("post-fetch refresh: {e:?}");
                }
            }
            Ok(s) => tracing::warn!("git fetch exited {s}"),
            Err(e) => tracing::warn!("git fetch failed: {e}"),
        }
    }

    pub async fn apply_hook(&self, wt_name: &str, status: &str) -> Result<()> {
        let mut s = self.state.write().await;
        s.apply_hook(wt_name, status)?;
        s.persist(&self.common_dir).await?;
        let snap = s.snapshot();
        drop(s);
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        Ok(())
    }
}

async fn probe(sock: &Path) -> Result<()> {
    // Connect + send Ping; if it succeeds someone's home.
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;
    let mut s = UnixStream::connect(sock).await?;
    let msg = serde_json::to_vec(&ClientMsg::Ping)?;
    s.write_all(&(msg.len() as u32).to_be_bytes()).await?;
    s.write_all(&msg).await?;
    Ok(())
}
