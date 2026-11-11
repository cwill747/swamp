pub mod badges;
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
    /// The latest count of waiting worktrees in other repositories, with the
    /// time it arrived. The aggregator sends it, and it lives in memory only.
    pub external_attention: std::sync::Mutex<Option<(usize, std::time::Instant)>>,
    /// Wakes the badge reconciler when the external count changes.
    pub badge_wake: Arc<Notify>,
}

/// How long an external attention count stays valid. The aggregator sends the
/// count again before this time passes, so a count from an aggregator that
/// stopped does not leave a stale badge.
pub const EXTERNAL_ATTENTION_TTL: Duration = Duration::from_secs(60);

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
pub(crate) fn flock_exclusive(path: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open lock file {}", path.display()))?;
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
            return Err(err).context("flock lock file");
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

    // Anchor the daemon's own working directory to the git common directory,
    // instead of leaving it wherever the spawning pane happened to be. The
    // daemon is detached, outlives the pane that started it, and its cwd was
    // otherwise whatever `tokio::process::Command` inherited — if that pane
    // sat inside a worktree, `worktree::remove_worktree`'s current-directory
    // check made that one worktree permanently undeletable. The common
    // directory always exists for the daemon's lifetime and is never itself
    // a worktree, so it's a safe, stable anchor.
    std::env::set_current_dir(&common)
        .with_context(|| format!("chdir to common dir {}", common.display()))?;

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
        external_attention: std::sync::Mutex::new(None),
        badge_wake: Arc::new(Notify::new()),
    });

    // Bind the control socket *before* the first state scan. The TUI waits
    // only ~2s for this socket to appear; gating it behind a full worktree scan
    // (slow on a cold cache or a large monorepo) makes every dashboard pane time
    // out at once. Binding first also lets concurrent `swamp serve` spawns lose
    // immediately via EADDRINUSE instead of each grinding through a scan.
    let listener = bind_and_kickoff(&daemon, &common, &sock)?;

    // Badge reconciler: the only writer of tab names in the repo session. It
    // does not run when the configuration turns badges off.
    if crate::config::load_config()
        .map(|c| c.badges.enabled)
        .unwrap_or(true)
    {
        tokio::spawn(badges::run(daemon.clone()));
    }
    // The aggregator feeds the cross-repo count. One instance serves every
    // daemon, and a spawn failure only means that the count stays at zero.
    crate::aggregate::spawn_detached();

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
                d.expire_subagents(crate::util::now_unix()).await;
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

/// Guarantees a worktree's "deleting" mark gets cleared, even when the
/// removal task panics or the request future is dropped mid-flight (a client
/// disconnect cancels the socket handler task that awaits it). The normal
/// completion paths in [`Daemon::remove_worktree`] call [`Self::clear`] (which
/// also broadcasts the row restored) or [`Self::disarm`] (when they already
/// broadcast their own snapshot, e.g. alongside `remove_row`); `Drop` is the
/// fallback for every path that skips both.
struct DeletingGuard {
    daemon: Arc<Daemon>,
    name: String,
    /// Set once a normal completion path has taken over clearing the mark,
    /// so `Drop` becomes a no-op instead of clearing (and broadcasting) again.
    cleared: bool,
}

impl DeletingGuard {
    fn new(daemon: Arc<Daemon>, name: String) -> Self {
        Self {
            daemon,
            name,
            cleared: false,
        }
    }

    /// Clear the mark and broadcast the row restored — the refused/failed
    /// removal case, where nothing else already broadcasts a snapshot.
    async fn clear(mut self) {
        self.cleared = true;
        let snap = {
            let mut s = self.daemon.state.write().await;
            s.clear_deleting(&self.name);
            s.snapshot()
        };
        let _ = self.daemon.tx.send(ServerMsg::Snapshot(snap));
    }

    /// Mark cleanup as already handled by the caller, so `Drop` no-ops.
    fn disarm(mut self) {
        self.cleared = true;
    }
}

impl Drop for DeletingGuard {
    fn drop(&mut self) {
        if self.cleared {
            return;
        }
        tracing::warn!(
            worktree = %self.name,
            "removal task ended without clearing its deleting mark; clearing from the guard"
        );
        let daemon = self.daemon.clone();
        let name = self.name.clone();
        tokio::spawn(async move {
            let snap = {
                let mut s = daemon.state.write().await;
                s.clear_deleting(&name);
                s.snapshot()
            };
            let _ = daemon.tx.send(ServerMsg::Snapshot(snap));
        });
    }
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
    /// Store the count of waiting worktrees in other repositories.
    pub fn set_external_attention(&self, count: usize) {
        *self.external_attention.lock().unwrap() = Some((count, std::time::Instant::now()));
        self.badge_wake.notify_one();
    }

    /// The count of waiting worktrees in other repositories, or zero when no
    /// count arrived within [`EXTERNAL_ATTENTION_TTL`].
    pub fn external_attention(&self) -> usize {
        self.external_attention_at(std::time::Instant::now())
    }

    fn external_attention_at(&self, now: std::time::Instant) -> usize {
        match *self.external_attention.lock().unwrap() {
            Some((count, at)) if now.saturating_duration_since(at) < EXTERNAL_ATTENTION_TTL => {
                count
            }
            _ => 0,
        }
    }

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
        // Clone out the agents map and default branch under a read lock so the git
        // scan (which can take seconds on large repos) runs completely off the
        // runtime via spawn_blocking, without blocking any async task or
        // holding any lock.
        let (agents, default_branch) = {
            let s = self.state.read().await;
            (s.agents.clone(), s.default_branch.clone())
        };

        // `scan_worktrees` fans the per-worktree git status out across a bounded
        // pool of scoped threads internally, so a single `spawn_blocking` keeps
        // the whole concurrent scan off the async runtime. No async lock is held
        // across this await — `agents` was cloned out above under a read
        // lock.
        let common = self.common_dir.clone();
        let (new_rows, titles) = tokio::task::spawn_blocking(move || {
            let rows = crate::daemon::state::scan_worktrees(&common, &agents, &default_branch)?;
            anyhow::Ok((rows, crate::slot_service::user_titles(&common)))
        })
        .await
        .context("git scan task")??;

        // Swap the freshly computed rows in under the write lock.
        let snap = {
            let mut s = self.state.write().await;
            s.apply_scanned_rows(new_rows);
            s.branch_titles = titles;
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
    /// then broadcast the refreshed snapshot.
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
        // bare layout the default branch often has no upstream
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

    /// The TUI create action: put `branch` into a free slot, or an overflow
    /// worktree when the pool is full.
    pub async fn create_worktree(&self, branch: &str) -> Result<()> {
        self.slot_alloc(branch, None, false, false)
            .await
            .map(|_| ())
    }

    /// The TUI create action for a new `branch` cut from `base`.
    pub async fn create_worktree_from_base(&self, branch: &str, base: &str) -> Result<()> {
        self.slot_alloc(branch, Some(base.to_string()), false, false)
            .await
            .map(|_| ())
    }

    /// Allocate a worktree for `branch` (see [`crate::slot_service::alloc`]),
    /// apply the agent-record changes, and broadcast the refreshed snapshot.
    pub async fn slot_alloc(
        &self,
        branch: &str,
        base: Option<String>,
        offline: bool,
        no_hooks: bool,
    ) -> Result<crate::worktree::slots::Allocation> {
        let _repo = self.repo_ops.lock().await;
        let common = self.common_dir.clone();
        let branch = branch.to_string();
        let (allocation, changes) = tokio::task::spawn_blocking(move || {
            let cfg = crate::config::slots_config(&common)?;
            let opts = crate::worktree::slots::AllocOptions {
                pool_size: cfg.pool_size,
                fetch: !offline,
                base,
            };
            let hooks = if no_hooks {
                &[][..]
            } else {
                &cfg.hooks.post_alloc[..]
            };
            crate::slot_service::alloc(&common, &branch, &opts, hooks)
        })
        .await
        .context("slot alloc task")??;
        tracing::info!(
            worktree = %allocation.name,
            branch = %allocation.branch,
            existing = allocation.existing,
            "allocated worktree"
        );
        self.apply_agent_changes(&changes).await?;
        self.refresh_all_unlocked().await?;
        Ok(allocation)
    }

    /// Release a slot (see [`crate::slot_service::release`]). When the client
    /// sent no PR facts, the daemon uses its own PR map, and looks the PR up
    /// when the map has no entry for the branch (the map stays empty while no
    /// TUI subscribes).
    pub async fn slot_release(
        &self,
        target: Option<String>,
        cwd: PathBuf,
        force: bool,
        offline: bool,
        no_hooks: bool,
        pr: Option<crate::worktree::slots::PrFacts>,
    ) -> Result<crate::worktree::slots::Release> {
        let _repo = self.repo_ops.lock().await;
        let (agents, prs) = {
            let s = self.state.read().await;
            (s.agents.clone(), s.pr_state_for_verdicts().cloned())
        };
        let common = self.common_dir.clone();
        let (release, changes) = tokio::task::spawn_blocking(move || {
            let pr = pr.or_else(|| {
                let wt = crate::worktree::slots::resolve_target(&common, target.as_deref(), &cwd)
                    .ok()?;
                let branch = crate::worktree::slots::checked_out_branch(&wt.path)?;
                let summary = prs?.remove(&branch)?;
                Some(crate::worktree::slots::PrFacts {
                    branch,
                    state: summary.state,
                    head_oid: summary.head_oid.and_then(|o| o.parse().ok()),
                })
            });
            let pr = match pr {
                None if !offline => crate::slot_service::pr_facts(&common, target.as_deref(), &cwd),
                pr => pr,
            };
            let opts = crate::worktree::slots::ReleaseOptions {
                force,
                fetch: !offline,
                pr,
            };
            let cfg = crate::config::slots_config(&common)?;
            let hooks = if no_hooks {
                &[][..]
            } else {
                &cfg.hooks.pre_release[..]
            };
            crate::slot_service::release(&common, target.as_deref(), &cwd, &opts, &agents, hooks)
        })
        .await
        .context("slot release task")??;
        tracing::info!(worktree = %release.name, outcome = ?release.outcome, "released worktree");
        self.apply_agent_changes(&changes).await?;
        self.refresh_all_unlocked().await?;
        Ok(release)
    }

    /// Set or clear the user title of `branch` and broadcast the new snapshot.
    pub async fn set_title(&self, branch: &str, title: Option<String>) -> Result<()> {
        let common = self.common_dir.clone();
        let b = branch.to_string();
        let titles = tokio::task::spawn_blocking(move || {
            crate::slot_service::set_title(&common, &b, title.as_deref())?;
            anyhow::Ok(crate::slot_service::user_titles(&common))
        })
        .await
        .context("set title task")??;
        let snap = {
            let mut s = self.state.write().await;
            s.branch_titles = titles;
            s.snapshot()
        };
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        tracing::info!(%branch, "set work title");
        Ok(())
    }

    /// Apply agent-record edits from a slot operation and persist them.
    async fn apply_agent_changes(&self, changes: &crate::slot_service::AgentChanges) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut s = self.state.write().await;
        changes.apply(&mut s.agents);
        s.persist(&self.common_dir).await
    }

    /// Run the authoritative non-forced removal checks without mutating the
    /// worktree. The current-tab deletion flow uses this before closing its
    /// own tab; ordinary deletion checks inside `remove_worktree`.
    pub async fn check_worktree_removal(&self, name: &str) -> Result<()> {
        let _repo = self.repo_ops.lock().await;
        let (pr_state, pr_head_oid) = {
            let s = self.state.read().await;
            let pr = s
                .rows
                .get(name)
                .and_then(|row| s.pr_state_for_verdicts()?.get(&row.branch));
            (
                pr.map(|pr| pr.state.clone()),
                pr.and_then(|pr| pr.head_oid.as_deref())
                    .and_then(|oid| oid.parse().ok()),
            )
        };
        let common = self.common_dir.clone();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            crate::worktree::check_worktree_removal(&common, &name, pr_state, pr_head_oid)
        })
        .await
        .context("check worktree removal task")?
    }

    /// Remove worktree `name` and its local branch (git2, off the async thread).
    ///
    /// The worktree is marked "deleting" and a snapshot broadcast **before**
    /// `repo_ops` is acquired, so a removal queued behind another repository
    /// operation (a `git fetch --all` can hold the lock for up to 60s) shows
    /// as deleting in every subscribed TUI for the whole wait, not just once
    /// the removal itself starts. The mark is cleared and a fresh snapshot
    /// broadcast on every exit path — success, refusal, or failure — via
    /// `guard`, whose `Drop` also covers a panicked or cancelled removal task
    /// so a stranded mark can't make a row permanently undeletable.
    ///
    /// The reply is *not* gated on a full rescan: once the removal succeeds the
    /// daemon optimistically drops the deleted row and broadcasts the new
    /// snapshot, so the TUI reflects the deletion immediately. A full
    /// `refresh_all` then reconciles the rest of the worktree set in the
    /// background. The old behavior — `refresh_all_unlocked().await` inline —
    /// made the client wait out a status scan of *every* remaining worktree
    /// (which scales with worktree count) before the row could disappear.
    pub async fn remove_worktree(self: &Arc<Self>, name: &str, force: bool) -> Result<()> {
        let snap = {
            let mut s = self.state.write().await;
            s.mark_deleting(name);
            s.snapshot()
        };
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        let guard = DeletingGuard::new(Arc::clone(self), name.to_string());

        let result = {
            let _repo = self.repo_ops.lock().await;
            let (pr_state, pr_head_oid) = {
                let s = self.state.read().await;
                let pr = s
                    .rows
                    .get(name)
                    .and_then(|row| s.pr_state_for_verdicts()?.get(&row.branch));
                (
                    pr.map(|pr| pr.state.clone()),
                    pr.and_then(|pr| pr.head_oid.as_deref())
                        .and_then(|oid| oid.parse().ok()),
                )
            };
            let common = self.common_dir.clone();
            let name = name.to_string();
            tokio::task::spawn_blocking(move || {
                // Serialize with slot operations from the CLI and other clients.
                let _slots = crate::worktree::slots::lock(&common)?;
                crate::worktree::remove_worktree(&common, &name, true, force, pr_state, pr_head_oid)
            })
            .await
            .context("remove worktree task")?
        };

        if let Err(e) = result {
            // Refused or failed: clear the mark and broadcast the row
            // restored, exactly as it was before the request.
            guard.clear().await;
            return Err(e);
        }

        // Optimistic removal: drop just the deleted row, clear the deleting
        // mark, and broadcast now, in the same snapshot.
        let snap = {
            let mut s = self.state.write().await;
            s.clear_deleting(name);
            s.remove_row(name);
            s.snapshot()
        };
        let _ = self.tx.send(ServerMsg::Snapshot(snap));
        guard.disarm();

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
        subagent: Option<&state::SubagentEvent>,
    ) -> Result<()> {
        let mut s = self.state.write().await;
        let changed = s.apply_hook_event(wt_name, status, session_name, session_id, subagent)?;
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
    /// Drop finished and stale subagents, and broadcast a snapshot when any
    /// were dropped. Subagents are in memory only, so there is no persist.
    pub async fn expire_subagents(&self, now: u64) {
        let mut s = self.state.write().await;
        if s.expire_subagents(now) {
            tracing::debug!("expired subagents");
            let snap = s.snapshot();
            drop(s);
            let _ = self.tx.send(ServerMsg::Snapshot(snap));
        }
    }

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
            external_attention: std::sync::Mutex::new(None),
            badge_wake: Arc::new(Notify::new()),
        })
    }

    /// A TUI create (through the daemon) and a CLI alloc (in-process) race for
    /// the same free slot; the shared slot lock gives them different ones.
    // Holds the env lock across awaits: `slot_alloc` reads the user config,
    // which the config tests point at temporary files.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn daemon_create_and_cli_alloc_get_different_worktrees() {
        let _env = crate::util::test_env_lock();
        // Other tests leave XDG_CONFIG_HOME pointing at their own (sometimes
        // malformed) config; read defaults from an empty dir instead.
        let xdg = std::env::temp_dir().join(format!("swamp-daemon-xdg-{}", std::process::id()));
        // SAFETY: the crate test env lock serializes env changes.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &xdg) };
        if !crate::worktree::test_support::git_available() {
            return;
        }
        let (root, bare) = crate::worktree::test_support::setup();
        let daemon = build_daemon(&bare).await;
        let cli_common = bare.clone();

        let cli = tokio::task::spawn_blocking(move || {
            let opts = crate::worktree::slots::AllocOptions::pool(5);
            crate::slot_service::alloc(&cli_common, "from-cli", &opts, &[])
                .unwrap()
                .0
                .path
        });
        daemon.create_worktree("from-tui").await.unwrap();
        let cli_path = cli.await.unwrap();

        let tui_path = crate::worktree::list_worktrees(&bare)
            .unwrap()
            .into_iter()
            .find(|w| w.branch == "from-tui")
            .unwrap()
            .path;
        assert_ne!(cli_path, tui_path);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Releasing through the daemon clears the slot's agent record in memory
    /// and remembers its session for the branch.
    // Holds the env lock across awaits: `slot_alloc` reads the user config,
    // which the config tests point at temporary files.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn daemon_release_moves_session_to_branch_memory() {
        let _env = crate::util::test_env_lock();
        // Other tests leave XDG_CONFIG_HOME pointing at their own (sometimes
        // malformed) config; read defaults from an empty dir instead.
        let xdg = std::env::temp_dir().join(format!("swamp-daemon-xdg-{}", std::process::id()));
        // SAFETY: the crate test env lock serializes env changes.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &xdg) };
        if !crate::worktree::test_support::git_available() {
            return;
        }
        let (root, bare) = crate::worktree::test_support::setup();
        let daemon = build_daemon(&bare).await;
        let a = daemon
            .slot_alloc("remember-me", None, true, false)
            .await
            .unwrap();
        daemon
            .apply_hook(&a.name, "idle", Some("Remember me"), Some("sess-1"), None)
            .await
            .unwrap();

        daemon
            .slot_release(Some(a.name.clone()), "/".into(), false, true, false, None)
            .await
            .unwrap();

        assert!(!daemon.state.read().await.agents.contains_key(&a.name));
        let mem = crate::slot_service::load_branches(&bare);
        assert_eq!(mem["remember-me"].session_id.as_deref(), Some("sess-1"));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn make_row(name: &str) -> crate::daemon::state::WorktreeRow {
        crate::daemon::state::WorktreeRow {
            name: name.to_string(),
            path: PathBuf::from(format!("/repo/{name}")),
            branch: name.to_string(),
            upstream: None,
            upstream_gone: false,
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflict: false,
            rebase: false,
            agent: crate::daemon::state::AgentStatus::Idle,
            agent_ts: 0,
            session_name: None,
            head_ts: 0,
            harness: None,
            is_default: false,
            removal_block: None,
            deleting: false,
            subagents: Vec::new(),
            kind: Default::default(),
            slot_state: None,
            pin: None,
            title: String::new(),
            title_source: None,
            head_subject: None,
            openspec_change: None,
        }
    }

    /// Write one protocol frame with an arbitrary JSON payload, so a test can
    /// send a request type that `ClientMsg` does not define.
    async fn write_raw_request(stream: &mut tokio::net::UnixStream, json: &str) {
        use tokio::io::AsyncWriteExt;
        let len = (4 + json.len()) as u32;
        stream.write_all(&len.to_be_bytes()).await.unwrap();
        stream.write_all(b"SWP1").await.unwrap();
        stream.write_all(json.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
    }

    /// The external count is acknowledged, lives in memory only, and expires.
    #[tokio::test]
    async fn external_attention_is_acknowledged_expires_and_is_not_in_the_snapshot() {
        if !git_available() {
            return;
        }
        let root = git_init_repo();
        let daemon = build_daemon(&root.join(".git")).await;
        assert_eq!(daemon.external_attention(), 0);

        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let task = tokio::spawn(socket::handle_client(daemon.clone(), server));
        socket::write_client_msg(&mut client, &ClientMsg::ExternalAttention { count: 3 })
            .await
            .unwrap();
        let reply = socket::read_server_msg(&mut client).await.unwrap();
        assert!(matches!(reply, Some(ServerMsg::Ok)), "got {reply:?}");
        assert_eq!(daemon.external_attention(), 3);

        // The count is not part of the snapshot that clients receive.
        let snapshot = serde_json::to_string(&daemon.state.read().await.snapshot()).unwrap();
        assert!(!snapshot.contains("external"), "{snapshot}");

        // The count is valid for less than the full lifetime.
        let now = std::time::Instant::now();
        assert_eq!(
            daemon.external_attention_at(now + EXTERNAL_ATTENTION_TTL - Duration::from_secs(1)),
            3
        );
        assert_eq!(
            daemon.external_attention_at(now + EXTERNAL_ATTENTION_TTL),
            0
        );

        // A new message replaces the count.
        socket::write_client_msg(&mut client, &ClientMsg::ExternalAttention { count: 0 })
            .await
            .unwrap();
        let _ = socket::read_server_msg(&mut client).await.unwrap();
        assert_eq!(daemon.external_attention(), 0);

        drop(client);
        let _ = task.await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `GetInfo` needs no subscription, and a later `Subscribe` on the same
    /// connection still delivers the three initial snapshots.
    #[tokio::test]
    async fn get_info_then_subscribe_on_one_connection() {
        use crate::daemon::socket::{ClientMsg, read_server_msg, write_client_msg};

        if !git_available() {
            return;
        }
        let repo = git_init_repo();
        let common = repo.join(".git");
        let daemon = build_daemon(&common).await;
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let task = tokio::spawn(socket::handle_client(daemon.clone(), server));

        write_client_msg(&mut client, &ClientMsg::GetInfo)
            .await
            .unwrap();
        match read_server_msg(&mut client).await.unwrap().unwrap() {
            ServerMsg::Info {
                common_dir,
                session_name,
                version,
            } => {
                assert_eq!(common_dir, common);
                assert_eq!(session_name, "test");
                assert_eq!(version, env!("CARGO_PKG_VERSION"));
            }
            other => panic!("expected Info, got {other:?}"),
        }

        write_client_msg(&mut client, &ClientMsg::Subscribe)
            .await
            .unwrap();
        assert!(matches!(
            read_server_msg(&mut client).await.unwrap().unwrap(),
            ServerMsg::Snapshot(_)
        ));
        assert!(matches!(
            read_server_msg(&mut client).await.unwrap().unwrap(),
            ServerMsg::Resources(_)
        ));
        assert!(matches!(
            read_server_msg(&mut client).await.unwrap().unwrap(),
            ServerMsg::PrStatus(_)
        ));

        drop(client);
        task.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A request type from a newer client gets an error reply. The connection
    /// stays usable, and a second client is not affected.
    #[tokio::test]
    async fn unknown_request_gets_error_and_daemon_keeps_serving() {
        use crate::daemon::socket::{ClientMsg, read_server_msg, write_client_msg};

        if !git_available() {
            return;
        }
        let repo = git_init_repo();
        let daemon = build_daemon(&repo.join(".git")).await;
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let task = tokio::spawn(socket::handle_client(daemon.clone(), server));

        write_raw_request(&mut client, r#"{"type":"from_the_future","x":1}"#).await;
        assert!(matches!(
            read_server_msg(&mut client).await.unwrap().unwrap(),
            ServerMsg::Err { .. }
        ));
        write_client_msg(&mut client, &ClientMsg::Ping)
            .await
            .unwrap();
        assert!(matches!(
            read_server_msg(&mut client).await.unwrap().unwrap(),
            ServerMsg::Pong
        ));

        let (mut second, server2) = tokio::net::UnixStream::pair().unwrap();
        let task2 = tokio::spawn(socket::handle_client(daemon.clone(), server2));
        write_client_msg(&mut second, &ClientMsg::Ping)
            .await
            .unwrap();
        assert!(matches!(
            read_server_msg(&mut second).await.unwrap().unwrap(),
            ServerMsg::Pong
        ));

        drop(client);
        drop(second);
        task.await.unwrap().unwrap();
        task2.await.unwrap().unwrap();
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Dropping a `DeletingGuard` without calling `clear`/`disarm` — the shape
    /// of a panicked or cancelled removal task — still clears the mark. The
    /// guard's `Drop` cannot `.await` directly, so it spawns the cleanup;
    /// this proves that spawned task actually runs and reaches the state.
    #[tokio::test]
    async fn deleting_guard_clears_mark_on_drop() {
        if !git_available() {
            eprintln!("skipping deleting_guard_clears_mark_on_drop: git not on PATH");
            return;
        }
        let repo = git_init_repo();
        let common = git_common_dir(&repo).unwrap();
        let daemon = build_daemon(&common).await;
        {
            let mut s = daemon.state.write().await;
            s.rows.insert("feature".into(), make_row("feature"));
            s.mark_deleting("feature");
        }
        assert!(
            daemon.state.read().await.snapshot().rows[0].deleting,
            "the mark must be visible before the guard drops"
        );

        drop(DeletingGuard::new(daemon.clone(), "feature".to_string()));

        let mut cleared = false;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if !daemon.state.read().await.snapshot().rows[0].deleting {
                cleared = true;
                break;
            }
        }
        assert!(cleared, "dropping the guard must clear the deleting mark");

        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Subagent start, stop, and expiry broadcast a snapshot. An activity ping
    /// for a known subagent does not.
    #[tokio::test]
    async fn subagent_changes_broadcast_and_pings_do_not() {
        use crate::daemon::state::{SUBAGENT_DONE_GRACE_SECS, SubagentEvent, SubagentEventKind};
        if !git_available() {
            eprintln!("skipping subagent_changes_broadcast_and_pings_do_not: git not on PATH");
            return;
        }
        let repo = git_init_repo();
        let common = git_common_dir(&repo).unwrap();
        let daemon = build_daemon(&common).await;
        daemon
            .state
            .write()
            .await
            .rows
            .insert("feature".into(), make_row("feature"));
        let mut rx = daemon.tx.subscribe();
        let ev = |kind| SubagentEvent {
            kind,
            agent_id: Some("a1".into()),
            agent_type: "Explore".into(),
            description: None,
        };
        let hook = |kind| {
            let daemon = daemon.clone();
            async move {
                daemon
                    .apply_hook("feature", "working", None, None, Some(&ev(kind)))
                    .await
                    .unwrap();
            }
        };
        let snapshots = |rx: &mut broadcast::Receiver<ServerMsg>| {
            let mut n = 0;
            while let Ok(msg) = rx.try_recv() {
                if matches!(msg, ServerMsg::Snapshot(_)) {
                    n += 1;
                }
            }
            n
        };

        hook(SubagentEventKind::Start).await;
        assert_eq!(snapshots(&mut rx), 1, "start broadcasts");
        hook(SubagentEventKind::Activity).await;
        assert_eq!(snapshots(&mut rx), 0, "activity ping does not broadcast");
        hook(SubagentEventKind::Stop).await;
        assert_eq!(snapshots(&mut rx), 1, "stop broadcasts");

        let now = crate::util::now_unix();
        daemon.expire_subagents(now).await;
        assert_eq!(snapshots(&mut rx), 0, "nothing to expire yet");
        daemon
            .expire_subagents(now + SUBAGENT_DONE_GRACE_SECS + 5)
            .await;
        assert_eq!(snapshots(&mut rx), 1, "expiry broadcasts");
        assert!(
            daemon.state.read().await.snapshot().rows[0]
                .subagents
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[tokio::test]
    async fn merged_pr_refreshes_verdict_and_allows_authoritative_removal() {
        use crate::github::PrSummary;
        use crate::worktree::test_support::{git_available, run, setup};
        use crate::worktree::{RemoveRefusedReason, create_worktree};

        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();
        std::fs::write(wt.path.join("work.txt"), "merged").unwrap();
        run(&wt.path, &["add", "work.txt"]);
        run(&wt.path, &["commit", "-m", "merged work"]);
        let head_oid = git2::Repository::open(&wt.path)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap();
        run(&bare, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&wt.path, &["push", "--set-upstream", "origin", "feature"]);
        run(&bare, &["update-ref", "-d", "refs/remotes/origin/feature"]);

        let daemon = build_daemon(&bare).await;
        daemon.refresh_all().await.unwrap();
        let error = daemon.check_worktree_removal("feature").await.unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::worktree::RemoveRefused>()
                .unwrap()
                .reason,
            RemoveRefusedReason::UnmergedCommits,
        );

        let mut prs = std::collections::HashMap::new();
        prs.insert(
            "feature".into(),
            PrSummary {
                number: 1,
                title: "Merged work".into(),
                state: "MERGED".into(),
                head_oid: Some(head_oid.to_string()),
                is_draft: false,
                checks: None,
                check_meta: None,
                url: None,
                comment_count: 0,
                review: None,
                reviews_partial: false,
            },
        );
        daemon.state.write().await.update_prs(prs);
        daemon.check_worktree_removal("feature").await.unwrap();

        daemon.remove_worktree("feature", false).await.unwrap();
        assert!(!wt.path.exists());

        let _ = std::fs::remove_dir_all(&root);
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
