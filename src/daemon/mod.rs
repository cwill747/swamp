pub mod resources;
pub mod socket;
pub mod state;
pub mod watcher;

use crate::util::{repo_id, session_name_for};
use crate::worktree::{git_common_dir, resolve_git_dir};
use anyhow::{Context, Result};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::sync::{Mutex, Notify, RwLock, broadcast, watch};

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
    pub pr_subscribers: Arc<AtomicUsize>,
    /// Wakes the PR poller so it can fetch immediately instead of sleeping out
    /// its interval. Poked on subscribe when no successful fetch has happened
    /// yet; `Notify` coalesces concurrent pokes into a single wake.
    pub pr_wake: Arc<Notify>,
}

type SharedOpResult = std::result::Result<(), String>;
type SharedOpRx = watch::Receiver<Option<SharedOpResult>>;

pub fn socket_path(common_dir: &Path) -> Result<PathBuf> {
    let id = repo_id(common_dir);
    let base = crate::util::runtime_base_dir()?;
    Ok(base.join(format!("{}.sock", id)))
}

pub fn pid_path(common_dir: &Path) -> Result<PathBuf> {
    Ok(socket_path(common_dir)?.with_extension("pid"))
}

fn lock_path(common_dir: &Path) -> Result<PathBuf> {
    Ok(socket_path(common_dir)?.with_extension("lock"))
}

/// Acquire an exclusive advisory lock on `<runtime>/<id>.lock`.
///
/// Opens (or creates) the lock file and calls `flock(2)` with `LOCK_EX`.
/// Returns the open `File` whose lifetime keeps the lock held — drop it to
/// release.  The lock is NOT inherited by child processes spawned after this
/// call (close-on-exec is set automatically on Linux for files opened with
/// `O_CLOEXEC` / the standard `File::create` path).
fn acquire_startup_lock(common_dir: &Path) -> Result<std::fs::File> {
    flock_exclusive(&lock_path(common_dir)?)
}

/// Open (or create) `path` and take an exclusive `flock(2)` on it, retrying
/// briefly on contention. The returned `File` holds the lock until dropped.
fn flock_exclusive(path: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open startup lock {}", path.display()))?;
    // LOCK_EX | LOCK_NB: fail immediately if another process holds it, then
    // retry in a short loop so we tolerate brief contention without spinning.
    // We cap retries so a truly stuck holder doesn't park us forever.
    let fd = file.as_raw_fd();
    let mut waited_ms = 0u64;
    loop {
        // SAFETY: fd is valid for the lifetime of `file`.
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            return Ok(file);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock || waited_ms >= 5_000 {
            return Err(err).context("flock startup lock");
        }
        std::thread::sleep(Duration::from_millis(50));
        waited_ms += 50;
    }
}

pub async fn serve(dir: Option<PathBuf>, foreground: bool) -> Result<()> {
    let start = match dir {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let start = resolve_git_dir(&start);
    let common = git_common_dir(&start).context("not inside a git repo")?;

    if !foreground {
        // The non-foreground parent only spawns a --foreground child and exits.
        // It does NOT hold the startup lock — doing so would deadlock the child
        // (on Linux, flock locks are per open-file-description, not per-fd, but
        // the child is a separate process and would still block on LOCK_EX).
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

    // ── Foreground path: serialise the check→remove→bind→pid critical section ──
    //
    // Multiple --foreground processes (e.g. a previous parent + this one) may
    // race here.  An exclusive flock on the lock file ensures only one of them
    // completes the bind; the other sees a live socket via `probe` and exits.
    // The lock file remains open (and therefore locked) for the daemon's
    // lifetime, giving `swamp kill` a reliable liveness signal.
    let _startup_lock = acquire_startup_lock(&common)?;

    let sock = socket_path(&common)?;
    // Remove a stale socket if the previous daemon died.
    if std::fs::symlink_metadata(&sock).is_ok() {
        if probe(&sock).await.is_ok() {
            anyhow::bail!("swamp serve already running for {}", common.display());
        }
        remove_stale_socket(&sock)?;
    }

    // The daemon is the long-lived writer, so it truncates the per-repo log on
    // startup to bound growth. Foreground also mirrors to stderr.
    let log_cfg = crate::config::load_config()?.logging;
    crate::logging::init(&common, foreground, true, &log_cfg);

    let state = Arc::new(RwLock::new(DaemonState::load(&common).await?));
    let (tx, _) = broadcast::channel::<ServerMsg>(64);

    // Session name matches launch::run's derivation via session_name_for.
    // Prefer the ZELLIJ_SESSION_NAME env if present (set inside any zellij
    // pane), so the daemon agrees with zellij even when started from an
    // unusual cwd.
    let session_name = std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .unwrap_or_else(|| session_name_for(&common));

    let daemon = Arc::new(Daemon {
        common_dir: common.clone(),
        session_name,
        state: state.clone(),
        resources: Arc::new(RwLock::new(resources::Snapshot::default())),
        repo_ops: Arc::new(Mutex::new(())),
        refresh_op: Arc::new(Mutex::new(None)),
        fetch_op: Arc::new(Mutex::new(None)),
        tx: tx.clone(),
        pr_subscribers: Arc::new(AtomicUsize::new(0)),
        pr_wake: Arc::new(Notify::new()),
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
            // Settle briefly on startup, but wake immediately if a subscriber
            // connects first so the first fetch isn't gated on this delay.
            tokio::select! {
                _ = d.pr_wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
            let mut delay = Duration::from_secs(60);
            loop {
                if d.pr_subscribers.load(Ordering::Relaxed) == 0 {
                    // No subscribers: idle until one connects (which wakes us) or
                    // re-check shortly in case a subscriber raced the gate.
                    tokio::select! {
                        _ = d.pr_wake.notified() => {}
                        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    }
                    continue;
                }

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
                        delay = Duration::from_secs(60);
                    }
                    Ok(Err(e)) => {
                        tracing::debug!("pr status poll: {e:?}");
                        // Keep the previous PR map; tell subscribers the data
                        // is stale rather than blanking their badges.
                        let mut s = d.state.write().await;
                        s.record_pr_error(format!("{e:#}"));
                        let pr_snap = s.pr_snapshot();
                        drop(s);
                        let _ = d.tx.send(ServerMsg::PrStatus(pr_snap));
                        delay = (delay * 2).min(Duration::from_secs(600));
                    }
                    Err(e) => tracing::warn!("pr status poll join: {e:?}"),
                }
                // Sleep out the steady-state cadence/backoff, but cut it short if
                // a new subscriber pokes us (e.g. another pane connecting).
                tokio::select! {
                    _ = d.pr_wake.notified() => {}
                    _ = tokio::time::sleep(delay) => {}
                }
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
    set_socket_permissions(sock)?;
    let pid = pid_path(common)?;
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

fn remove_stale_socket(sock: &Path) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let meta = std::fs::symlink_metadata(sock)
        .with_context(|| format!("stat stale socket {}", sock.display()))?;
    if meta.file_type().is_symlink() {
        anyhow::bail!("refusing to remove stale socket symlink {}", sock.display());
    }
    if !meta.file_type().is_socket() {
        anyhow::bail!(
            "refusing to remove stale socket path {}: not a socket",
            sock.display()
        );
    }
    let our_uid = unsafe { libc::getuid() };
    if meta.uid() != our_uid {
        anyhow::bail!(
            "refusing to remove stale socket {} owned by uid {}",
            sock.display(),
            meta.uid()
        );
    }
    std::fs::remove_file(sock).with_context(|| format!("remove stale socket {}", sock.display()))
}

fn set_socket_permissions(sock: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set permissions on socket {}", sock.display()))
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
        // Clone out the agents map under a *read* lock so the git scan (which
        // can take seconds on large repos) runs completely off the runtime via
        // spawn_blocking, without blocking any async task or holding any lock.
        let (agents, default_branch) = {
            let s = self.state.read().await;
            (s.agents.clone(), s.default_branch.clone())
        };

        // `scan_worktrees` fans the per-worktree git status out across a bounded
        // pool of scoped threads internally, so a single `spawn_blocking` keeps
        // the whole concurrent scan off the async runtime. No async lock is held
        // across this await — `agents` was cloned out above under a read lock.
        let common = self.common_dir.clone();
        let new_rows = tokio::task::spawn_blocking(move || {
            crate::daemon::state::scan_worktrees(&common, &agents, &default_branch)
        })
        .await
        .context("git scan task")??;

        // Swap the freshly computed rows in under the write lock.
        let snap = {
            let mut s = self.state.write().await;
            s.apply_scanned_rows(new_rows);
            s.snapshot()
        };
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
        let mut child = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.common_dir)
            .args(["fetch", "--all", "--prune"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("git fetch")?;
        let status = tokio::time::timeout(Duration::from_secs(60), child.wait())
            .await
            .map_err(|_| anyhow::anyhow!("git fetch timed out after 60s"))?
            .context("git fetch wait")?;
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
        let mut fetch_child = tokio::process::Command::new("git")
            .arg("-C")
            .arg(&self.common_dir)
            .args(["fetch", "--all", "--prune"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("git fetch")?;
        let fetch_status = tokio::time::timeout(Duration::from_secs(60), fetch_child.wait())
            .await
            .map_err(|_| anyhow::anyhow!("git fetch timed out after 60s"))?
            .context("git fetch wait")?;
        if !fetch_status.success() {
            anyhow::bail!("git fetch exited {fetch_status}");
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
            let merge_child = tokio::process::Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["merge", "--ff-only", &remote_ref])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .context("git merge --ff-only")?;
            let out = tokio::time::timeout(Duration::from_secs(30), merge_child.wait_with_output())
                .await
                .map_err(|_| anyhow::anyhow!("git merge --ff-only timed out after 30s"))?
                .context("git merge --ff-only wait")?;
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

    /// Remove worktree `name` and its local branch (git2, off the async thread).
    ///
    /// The reply is *not* gated on a full rescan: once the removal succeeds the
    /// daemon optimistically drops the deleted row and broadcasts the new
    /// snapshot, so the TUI reflects the deletion immediately. A full
    /// `refresh_all` then reconciles the rest of the worktree set in the
    /// background. The old behavior — `refresh_all_unlocked().await` inline —
    /// made the client wait out a status scan of *every* remaining worktree
    /// (which scales with worktree count) before the row could disappear.
    pub async fn remove_worktree(self: &Arc<Self>, name: &str, force: bool) -> Result<()> {
        {
            let _repo = self.repo_ops.lock().await;
            let common = self.common_dir.clone();
            let name = name.to_string();
            tokio::task::spawn_blocking(move || {
                crate::worktree::remove_worktree(&common, &name, true, force)
            })
            .await
            .context("remove worktree task")??;
        }

        // Optimistic removal: drop just the deleted row and broadcast now.
        let snap = {
            let mut s = self.state.write().await;
            s.remove_row(name);
            s.snapshot()
        };
        let _ = self.tx.send(ServerMsg::Snapshot(snap));

        // Reconcile the remaining worktrees in the background. `refresh_all`
        // coalesces with the watcher-triggered refresh that the directory
        // removal also kicks off, so this can't stack a redundant scan.
        let daemon = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(e) = daemon.refresh_all().await {
                tracing::warn!("post-delete refresh: {e:?}");
            }
        });
        Ok(())
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
        let mut s = self.state.write().await;
        let changed = s.apply_hook(wt_name, status, session_name, session_id)?;
        if !changed {
            // Timestamp-only ping (active agents send one per tool call). The
            // in-memory record is fresh; skip the persist — whose status-file
            // write would otherwise echo through the fs watcher as a pointless
            // git rescan — and the snapshot broadcast, which drove the TUI's
            // tab reconciler on every ping.
            tracing::debug!(worktree = %wt_name, status, "agent hook unchanged; not broadcasting");
            return Ok(());
        }
        tracing::info!(worktree = %wt_name, status, "applied agent hook");
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
    use tokio::net::UnixStream;
    let mut s = UnixStream::connect(sock).await?;
    socket::write_client_msg(&mut s, &ClientMsg::Ping).await?;
    match tokio::time::timeout(Duration::from_secs(2), socket::read_server_msg(&mut s)).await {
        Ok(Ok(Some(ServerMsg::Pong))) => Ok(()),
        Ok(Ok(Some(other))) => anyhow::bail!("unexpected probe response: {other:?}"),
        Ok(Ok(None)) => anyhow::bail!("daemon closed probe socket"),
        Ok(Err(e)) => Err(e),
        Err(_) => anyhow::bail!("daemon probe timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::process::Command as StdCommand;

    /// Acquire → drop → re-acquire the lock on the same path to verify that
    /// releasing the flock (by dropping the File) allows a second caller to
    /// succeed. Exercises the flock primitive directly so the test needs no
    /// git repo, env vars, or runtime dir (it must pass in the Nix sandbox).
    #[test]
    fn startup_lock_acquire_release_reacquire() {
        let dir = std::env::temp_dir().join(format!(
            "swamp-lock-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.lock");

        // First acquisition must succeed.
        let lock1 = flock_exclusive(&path).expect("first acquire");
        // Dropping releases the flock.
        drop(lock1);
        // Second acquisition must also succeed (not permanently locked).
        let lock2 = flock_exclusive(&path).expect("re-acquire after release");
        drop(lock2);

        let _ = std::fs::remove_dir_all(&dir);
    }

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
            pr_subscribers: Arc::new(AtomicUsize::new(0)),
            pr_wake: Arc::new(Notify::new()),
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
        if let Ok(p) = pid_path(&common) {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir_all(&repo);
    }
}
