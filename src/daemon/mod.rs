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
use tokio::sync::{Mutex, RwLock, broadcast, watch};

use self::socket::{ClientMsg, ServerMsg};
use self::state::DaemonState;

pub struct Daemon {
    pub common_dir: PathBuf,
    pub session_name: String,
    pub state: Arc<RwLock<DaemonState>>,
    pub resources: Arc<RwLock<resources::Snapshot>>,
    pub repo_ops: Arc<Mutex<()>>,
    pub refresh_op: Arc<Mutex<Option<SharedOpRx>>>,
    pub fetch_op: Arc<Mutex<Option<SharedOpRx>>>,
    pub tx: broadcast::Sender<ServerMsg>,
}

type SharedOpResult = std::result::Result<(), String>;
type SharedOpRx = watch::Receiver<Option<SharedOpResult>>;

pub fn socket_path(common_dir: &Path) -> PathBuf {
    let id = repo_id(common_dir);
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
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

    // The daemon is the long-lived writer, so it truncates the per-repo log on
    // startup to bound growth. Foreground also mirrors to stderr.
    let log_cfg = crate::config::load_config()?.logging;
    crate::logging::init(&common, foreground, true, &log_cfg);

    let state = Arc::new(RwLock::new(DaemonState::load(&common).await?));
    let (tx, _) = broadcast::channel::<ServerMsg>(64);

    // Session name matches launch::run's derivation: the file_name of the dir
    // that contains the bare repo / .git. Prefer the ZELLIJ_SESSION_NAME env if
    // present (set inside any zellij pane), so the daemon agrees with zellij
    // even when started from an unusual cwd.
    let session_name = std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .unwrap_or_else(|| {
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
        repo_ops: Arc::new(Mutex::new(())),
        refresh_op: Arc::new(Mutex::new(None)),
        fetch_op: Arc::new(Mutex::new(None)),
        tx: tx.clone(),
    });

    // Bind the control socket *before* the first state scan. The TUI waits
    // only ~2s for this socket to appear; gating it behind a full worktree scan
    // (slow on a cold cache or a large monorepo) makes every dashboard pane time
    // out at once. Binding first also lets concurrent `swamp serve` spawns lose
    // immediately via EADDRINUSE instead of each grinding through a scan.
    let listener = bind_and_kickoff(&daemon, &common, &sock)?;

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
                tracing::debug!(trigger = "heartbeat", "git refresh");
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
                tracing::info!(trigger = "periodic_fetch", "running periodic git fetch");
                if let Err(e) = d.fetch_and_refresh().await {
                    tracing::warn!("periodic fetch/refresh: {e:?}");
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

    loop {
        let (stream, _) = listener.accept().await?;
        let d = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = socket::handle_client(d, stream).await {
                // A subscriber pane that goes away mid-broadcast surfaces as
                // BrokenPipe/ConnectionReset on the next write (resources alone
                // broadcast at 1Hz). That's expected client churn, not an error
                // worth logging — keep it at trace so genuine failures stand out.
                if is_disconnect(&e) {
                    tracing::trace!("client disconnected: {e:?}");
                } else {
                    tracing::debug!("client: {e:?}");
                }
            }
        });
    }
}

/// True when an error from a client connection is just a peer that went away —
/// a closed/half-closed socket rather than a real fault. These are routine
/// (every pane that closes trips one on the next broadcast) so callers downgrade
/// the log level instead of treating them as errors.
fn is_disconnect(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            use std::io::ErrorKind::*;
            matches!(
                io.kind(),
                BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof | WriteZero
            )
        })
    })
}

/// Bind the daemon's control socket, record the pid, and kick off the first
/// state scan in the background. The scan is deliberately *not* awaited here so
/// the socket is reachable the instant this returns; see the call site in
/// `serve` for why that ordering matters.
fn bind_and_kickoff(daemon: &Arc<Daemon>, common: &Path, sock: &Path) -> Result<UnixListener> {
    let listener = UnixListener::bind(sock).context("bind socket")?;
    let pid = pid_path(common);
    if let Some(parent) = pid.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(pid, std::process::id().to_string())?;
    tracing::info!("swamp daemon listening on {}", sock.display());

    let d = daemon.clone();
    tokio::spawn(async move {
        if let Err(e) = d.refresh_all().await {
            tracing::warn!("initial refresh: {e:?}");
        }
    });
    Ok(listener)
}

async fn await_shared_op(mut rx: SharedOpRx) -> Result<()> {
    loop {
        if let Some(res) = rx.borrow().clone() {
            return res.map_err(anyhow::Error::msg);
        }
        if rx.changed().await.is_err() {
            anyhow::bail!("shared operation ended without a result");
        }
    }
}

impl Daemon {
    pub async fn refresh_all(&self) -> Result<()> {
        if let Some(rx) = self.fetch_op.lock().await.as_ref().cloned() {
            if let Err(e) = await_shared_op(rx).await {
                self.refresh_all_exclusive().await?;
                return Err(e);
            }
            return Ok(());
        }
        let tx = {
            let mut refresh = self.refresh_op.lock().await;
            if let Some(rx) = refresh.as_ref().cloned() {
                drop(refresh);
                return await_shared_op(rx).await;
            }
            let (tx, rx) = watch::channel(None);
            *refresh = Some(rx);
            tx
        };

        let res = self
            .refresh_all_exclusive()
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Some(res.clone()));
        *self.refresh_op.lock().await = None;
        res.map_err(anyhow::Error::msg)
    }

    async fn refresh_all_exclusive(&self) -> Result<()> {
        let _repo = self.repo_ops.lock().await;
        self.refresh_all_unlocked().await
    }

    async fn refresh_all_unlocked(&self) -> Result<()> {
        let mut s = self.state.write().await;
        s.refresh_git(&self.common_dir)?;
        let snap = s.snapshot();
        drop(s);
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        Ok(())
    }

    pub async fn fetch_and_refresh(&self) -> Result<()> {
        let tx = {
            let mut fetch = self.fetch_op.lock().await;
            if let Some(rx) = fetch.as_ref().cloned() {
                drop(fetch);
                return await_shared_op(rx).await;
            }
            let (tx, rx) = watch::channel(None);
            *fetch = Some(rx);
            tx
        };

        let res = self
            .fetch_and_refresh_exclusive()
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Some(res.clone()));
        *self.fetch_op.lock().await = None;
        res.map_err(anyhow::Error::msg)
    }

    async fn fetch_and_refresh_exclusive(&self) -> Result<()> {
        let _repo = self.repo_ops.lock().await;
        let status = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.common_dir)
            .args(["fetch", "--all", "--prune"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .context("git fetch")?;
        if !status.success() {
            anyhow::bail!("git fetch exited {status}");
        }
        self.refresh_all_unlocked().await
    }

    /// Fetch all remotes and fast-forward the default branch in its worktree,
    /// the equivalent of the old `git wt update`, then broadcast the refreshed
    /// snapshot.
    pub async fn update_default(&self) -> Result<()> {
        let _repo = self.repo_ops.lock().await;
        tracing::info!(
            trigger = "update_default",
            "fetching all remotes and fast-forwarding default branch"
        );
        let fetch = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.common_dir)
            .args(["fetch", "--all", "--prune"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .context("git fetch")?;
        if !fetch.success() {
            anyhow::bail!("git fetch exited {fetch}");
        }

        // Fast-forward the default branch in its worktree (if it's checked out).
        // Merge the remote-tracking ref explicitly rather than `@{u}`: in a
        // bare/git-wt layout the default branch often has no upstream
        // configured, which would make `@{u}` fail with "no upstream
        // configured".
        let common = self.common_dir.clone();
        let (wt, branch) = tokio::task::spawn_blocking(move || {
            (
                crate::worktree::default_worktree_path(&common),
                crate::worktree::default_branch(&common),
            )
        })
        .await
        .context("locate default worktree")?;
        if let Some(path) = wt {
            let remote_ref = format!("origin/{branch}");
            let out = tokio::process::Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["merge", "--ff-only", &remote_ref])
                .output()
                .await
                .context("git merge --ff-only")?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                anyhow::bail!("fast-forward failed: {}", stderr.trim());
            }
            let wt_name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            tracing::info!(worktree = %wt_name, branch = %branch, "fast-forwarded default branch");
        } else {
            tracing::debug!(branch = %branch, "default branch not checked out; nothing to fast-forward");
        }

        self.refresh_all_unlocked().await
    }

    /// Create a worktree for `branch` (git2, off the async thread) and
    /// broadcast the refreshed snapshot.
    pub async fn create_worktree(&self, branch: &str) -> Result<()> {
        let _repo = self.repo_ops.lock().await;
        let common = self.common_dir.clone();
        let branch = branch.to_string();
        tokio::task::spawn_blocking(move || crate::worktree::create_worktree(&common, &branch))
            .await
            .context("create worktree task")??;
        self.refresh_all_unlocked().await
    }

    /// Create a worktree for a new `branch` cut from `base` (git2, off the
    /// async thread) and broadcast the refreshed snapshot.
    pub async fn create_worktree_from_base(&self, branch: &str, base: &str) -> Result<()> {
        let _repo = self.repo_ops.lock().await;
        let common = self.common_dir.clone();
        let branch = branch.to_string();
        let base = base.to_string();
        tokio::task::spawn_blocking(move || {
            crate::worktree::create_worktree_from_base(&common, &branch, &base)
        })
        .await
        .context("create worktree task")??;
        self.refresh_all_unlocked().await
    }

    /// Remove worktree `name` and its local branch (git2, off the async thread),
    /// then broadcast the refreshed snapshot.
    pub async fn remove_worktree(&self, name: &str, force: bool) -> Result<()> {
        let _repo = self.repo_ops.lock().await;
        let common = self.common_dir.clone();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            crate::worktree::remove_worktree(&common, &name, true, force)
        })
        .await
        .context("remove worktree task")??;
        self.refresh_all_unlocked().await
    }

    pub async fn list_branches(&self) -> Result<Vec<crate::worktree::BranchInfo>> {
        let _repo = self.repo_ops.lock().await;
        let common = self.common_dir.clone();
        tokio::task::spawn_blocking(move || crate::worktree::list_branches(&common))
            .await
            .context("list branches task")?
    }

    pub async fn apply_hook(
        &self,
        wt_name: &str,
        status: &str,
        session_name: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<()> {
        tracing::info!(worktree = %wt_name, status, "applied agent hook");
        let mut s = self.state.write().await;
        s.apply_hook(wt_name, status, session_name, session_id)?;
        s.persist(&self.common_dir).await?;
        let snap = s.snapshot();
        drop(s);
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        Ok(())
    }

    /// Record the per-worktree harness override, persist it, and broadcast the
    /// refreshed snapshot so the indicator updates.
    pub async fn set_harness(&self, wt_name: &str, harness: crate::config::Harness) -> Result<()> {
        let mut s = self.state.write().await;
        s.set_harness(wt_name, harness);
        s.persist(&self.common_dir).await?;
        let snap = s.snapshot();
        drop(s);
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        Ok(())
    }
}

pub(crate) async fn probe(sock: &Path) -> Result<()> {
    // Connect + send Ping; if it succeeds someone's home.
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;
    let mut s = UnixStream::connect(sock).await?;
    let msg = serde_json::to_vec(&ClientMsg::Ping)?;
    s.write_all(&(msg.len() as u32).to_be_bytes()).await?;
    s.write_all(&msg).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::process::Command as StdCommand;

    fn git_available() -> bool {
        StdCommand::new("git").arg("--version").output().is_ok()
    }

    fn git_init_repo() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("swamp-bind-test-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let ok = StdCommand::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        run(&["commit", "-q", "--allow-empty", "-m", "init"]);
        dir
    }

    async fn build_daemon(common: &Path) -> Arc<Daemon> {
        Arc::new(Daemon {
            common_dir: common.to_path_buf(),
            session_name: "test".into(),
            state: Arc::new(RwLock::new(DaemonState::load(common).await.unwrap())),
            resources: Arc::new(RwLock::new(resources::Snapshot::default())),
            repo_ops: Arc::new(Mutex::new(())),
            refresh_op: Arc::new(Mutex::new(None)),
            fetch_op: Arc::new(Mutex::new(None)),
            tx: broadcast::channel(64).0,
        })
    }

    /// The control socket must come up before the initial git scan runs, so the
    /// TUI's ~2s readiness wait can't trip on a slow scan. We prove the scan is
    /// deferred (rows are still empty the instant the socket is bound) and that
    /// it later populates in the background. A current-thread runtime guarantees
    /// the spawned scan can't have run before the synchronous `try_read`.
    #[tokio::test(flavor = "current_thread")]
    async fn bind_happens_before_initial_scan() {
        if !git_available() {
            eprintln!("skipping bind_happens_before_initial_scan: git not on PATH");
            return;
        }
        let repo = git_init_repo();
        let common = git_common_dir(&repo).unwrap();
        let daemon = build_daemon(&common).await;
        let sock = repo.join("test.sock");

        let listener = bind_and_kickoff(&daemon, &common, &sock).unwrap();

        assert!(sock.exists(), "socket should be bound immediately");
        assert!(
            daemon.state.try_read().unwrap().rows.is_empty(),
            "initial scan must be deferred, not awaited before bind"
        );
        // A racing second `serve` loses fast instead of running its own scan.
        assert!(UnixListener::bind(&sock).is_err(), "second bind must fail");

        let mut populated = false;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if !daemon.state.read().await.rows.is_empty() {
                populated = true;
                break;
            }
        }
        assert!(
            populated,
            "background scan should populate the worktree row"
        );

        drop(listener);
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_file(pid_path(&common));
        let _ = std::fs::remove_dir_all(&repo);
    }
}
