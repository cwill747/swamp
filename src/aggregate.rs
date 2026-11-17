//! `swamp aggregate`: the hidden process that tells each repo daemon how many
//! worktrees wait for the user in the other repositories.
//!
//! One instance serves all daemons. It follows each daemon through the fan-in
//! module, and sends each daemon its count when the count changes and on an
//! interval. A daemon drops a count that is not sent again, so an aggregator
//! that stops cannot leave a stale badge. The process holds no state on disk
//! and exits when no daemon or dashboard client has connected for a while.

use crate::daemon::socket::ClientMsg;
use crate::daemon::state::{AgentStatus, Snapshot};
use crate::fanin;
use crate::workspace::{
    self,
    broker::StateHub,
    protocol::{ResourceState, WorkspaceState},
    remote::Remotes,
};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const LOCK_FILE: &str = "aggregator.lock";
const BROKER_LOCK_FILE: &str = "workspace.lock";

struct OwnedTask<T>(Option<tokio::task::JoinHandle<T>>);
impl<T> OwnedTask<T> {
    fn new(task: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(task))
    }
    async fn join(mut self) -> std::result::Result<T, tokio::task::JoinError> {
        let result = self.0.as_mut().expect("owned task is present").await;
        self.0.take();
        result
    }
    async fn abort_and_join(mut self) {
        if let Some(mut task) = self.0.take() {
            task.abort();
            let _ = (&mut task).await;
        }
    }
}
impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub scan_interval: Duration,
    /// Interval at which each count is sent again. It must be shorter than
    /// the lifetime of a count in the daemon.
    pub resend_interval: Duration,
    /// How long the process runs without live daemons or dashboard clients.
    pub idle_exit: Duration,
    pub request_timeout: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            scan_interval: fanin::SCAN_INTERVAL,
            resend_interval: Duration::from_secs(20),
            idle_exit: Duration::from_secs(60),
            request_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// A different instance holds the lock.
    AlreadyRunning,
    /// No daemon or dashboard client was active for the idle time.
    Idle,
}

/// Take the single-instance lock. `None` means that a different instance
/// holds it.
fn try_lock(path: &Path) -> Result<Option<std::fs::File>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    // SAFETY: the descriptor is valid for the lifetime of `file`.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let err = std::io::Error::last_os_error();
    if err.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err).with_context(|| format!("lock {}", path.display()))
    }
}

/// Number of worktrees in `snapshot` whose agent waits for the user.
fn waiting_count(snapshot: &Snapshot) -> usize {
    snapshot
        .rows
        .iter()
        .filter(|row| row.agent == AgentStatus::Waiting && !row.deleting)
        .count()
}

/// For each repository, the number of waiting worktrees in all the others.
pub fn external_counts(waiting: &BTreeMap<String, usize>) -> BTreeMap<String, usize> {
    let total: usize = waiting.values().sum();
    waiting
        .iter()
        .map(|(id, own)| (id.clone(), total - own))
        .collect()
}

/// One live daemon.
#[derive(Clone)]
struct Repo {
    sock: PathBuf,
    waiting: usize,
    /// The count that the daemon acknowledged last.
    sent: Option<usize>,
    /// False after the daemon refused the message. An older daemon does not
    /// know it, and the aggregator then stops sending to that daemon. A
    /// request that did not arrive does not change this value.
    supported: bool,
}

/// Send each daemon its count. With `all` false, only a daemon whose count
/// changed gets a message.
async fn send_counts(repos: &mut BTreeMap<String, Repo>, all: bool, timeout: Duration) {
    let waiting: BTreeMap<String, usize> = repos
        .iter()
        .map(|(id, repo)| (id.clone(), repo.waiting))
        .collect();
    for (id, count) in external_counts(&waiting) {
        let Some(repo) = repos.get_mut(&id) else {
            continue;
        };
        if !repo.supported || (!all && repo.sent == Some(count)) {
            continue;
        }
        let msg = ClientMsg::ExternalAttention { count };
        match fanin::request(&repo.sock, &msg, timeout).await {
            Ok(()) => repo.sent = Some(count),
            Err(e) if e.is_refusal() => {
                tracing::debug!(repo = %id, "daemon does not accept the attention count: {e}");
                repo.supported = false;
            }
            // The daemon did not get the count. `sent` keeps its value, so
            // the subsequent send tries again.
            Err(e) => tracing::debug!(repo = %id, "attention count not delivered: {e}"),
        }
    }
}

async fn attention_worker(
    mut updates: tokio::sync::watch::Receiver<BTreeMap<String, Repo>>,
    interval: Duration,
    timeout: Duration,
    lock_path: PathBuf,
) {
    let mut resend = tokio::time::interval(interval);
    let mut repos = BTreeMap::new();
    let mut attention_lock = None;
    loop {
        tokio::select! {
            _ = resend.tick() => {
                acquire_attention_lock(&mut attention_lock, &lock_path);
                if attention_lock.is_some() {
                    send_counts(&mut repos, true, timeout).await;
                }
            },
            changed = updates.changed() => {
                if changed.is_err() { return; }
                let incoming = updates.borrow_and_update().clone();
                repos.retain(|id, _| incoming.contains_key(id));
                for (id, fresh) in incoming {
                    if let Some(current) = repos.get_mut(&id) {
                        current.sock = fresh.sock;
                        current.waiting = fresh.waiting;
                    } else {
                        repos.insert(id, fresh);
                    }
                }
                acquire_attention_lock(&mut attention_lock, &lock_path);
                if attention_lock.is_some() {
                    send_counts(&mut repos, false, timeout).await;
                }
            }
        }
    }
}

fn acquire_attention_lock(lock: &mut Option<std::fs::File>, path: &Path) {
    if lock.is_none() {
        match try_lock(path) {
            Ok(Some(file)) => *lock = Some(file),
            Ok(None) => {}
            Err(error) => tracing::warn!("could not acquire attention lock: {error:#}"),
        }
    }
}

/// Run the aggregator for the daemons in `runtime_dir`.
async fn run_at(runtime_dir: PathBuf, timing: Timing) -> Result<Exit> {
    std::fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("create {}", runtime_dir.display()))?;
    // Older Swamp versions hold `aggregator.lock` while serving attention
    // counts but do not own a workspace broker. Keep this broker lock separate
    // so the new workspace endpoint starts during upgrades; the attention
    // worker acquires the legacy lock when it becomes available.
    let Some(_broker_lock) = try_lock(&runtime_dir.join(BROKER_LOCK_FILE))? else {
        return Ok(Exit::AlreadyRunning);
    };

    // Establish the endpoint before discovery starts. The broker lock is held
    // above, so an existing workspace socket is stale and safe to replace.
    let listener = workspace::broker::bind_owned(&runtime_dir)?;
    let _endpoint_guard =
        workspace::broker::EndpointGuard::capture(&workspace::protocol::socket_path(&runtime_dir));
    let mut model = workspace::Workspace::default();
    let mut remotes = Remotes::default();
    let hub = StateHub::new(inventory_state(&model, &remotes));

    let (config_tx, mut config_rx) = tokio::sync::watch::channel(BTreeMap::new());
    let load_ssh_config = !cfg!(test);
    let config_task = load_ssh_config.then(|| {
        OwnedTask::new(tokio::spawn(async move {
            let mut refresh = tokio::time::interval(Duration::from_secs(2));
            loop {
                refresh.tick().await;
                match tokio::task::spawn_blocking(crate::config::load_config).await {
                    Ok(Ok(config)) => {
                        config_tx.send_if_modified(|hosts| {
                            if *hosts == config.ssh_hosts {
                                return false;
                            }
                            *hosts = config.ssh_hosts;
                            true
                        });
                    }
                    Ok(Err(error)) => tracing::debug!("could not reload SSH hosts: {error:#}"),
                    Err(error) => tracing::warn!("SSH host config task: {error}"),
                }
            }
        }))
    });
    let (remote_tx, mut remote_rx) = mpsc::channel(64);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let broker = OwnedTask::new(tokio::spawn(workspace::broker::serve(
        listener,
        hub.clone(),
        shutdown_rx,
    )));

    // Registry I/O is kept off the broker/event loop. Each refresh is bounded
    // to one blocking task and the latest complete result is coalesced.
    let registry_path = crate::registry::registry_path().ok();
    let registry_refresh_path = registry_path.clone();
    let (registry_tx, mut registry_rx) = tokio::sync::watch::channel(Vec::new());
    let registry_task = OwnedTask::new(tokio::spawn(async move {
        let mut refresh = tokio::time::interval(Duration::from_secs(5));
        loop {
            refresh.tick().await;
            if let Some(path) = registry_refresh_path.clone()
                && let Ok(entries) =
                    tokio::task::spawn_blocking(move || crate::registry::load_at(&path)).await
            {
                registry_tx.send_replace(entries);
            }
        }
    }));

    let (tx, mut rx) = mpsc::channel(256);
    let scan = OwnedTask::new(fanin::spawn(
        fanin::Config {
            runtime_dir: runtime_dir.clone(),
            // The aggregator needs no repository identity.
            registry: registry_path.clone(),
            scan_interval: timing.scan_interval,
        },
        tx.clone(),
    ));
    let mut repos: BTreeMap<String, Repo> = BTreeMap::new();
    let (attention_tx, attention_rx) = tokio::sync::watch::channel(BTreeMap::new());
    let attention = OwnedTask::new(tokio::spawn(attention_worker(
        attention_rx,
        timing.resend_interval,
        timing.request_timeout,
        runtime_dir.join(LOCK_FILE),
    )));
    let mut generations = BTreeMap::<String, u64>::new();
    let mut resource_deadlines = BTreeMap::<String, Instant>::new();
    let mut idle_since = Instant::now();
    let mut resource_check = tokio::time::interval(Duration::from_millis(500));
    let exit = loop {
        let changed = tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break Exit::Idle };
                match &event {
                    fanin::Event::Up { id, sock, .. } => {
                        let id = id.clone();
                        let sock = sock.clone();
                        if !repos.contains_key(&id) {
                            let generation = generations.entry(id.clone()).or_default();
                            *generation = generation.wrapping_add(1);
                            let state = ResourceState::Loading { repo_id: id.clone(), generation: *generation, snapshot: None };
                            resource_deadlines.insert(id.clone(), Instant::now() + Duration::from_secs(3));
                            hub.publish_resource(id.clone(), state).await;
                        }
                        repos.entry(id).and_modify(|repo| repo.sock = sock.clone()).or_insert(Repo {
                            sock,
                            waiting: 0,
                            sent: None,
                            supported: true,
                        });
                    }
                    fanin::Event::Snapshot { id, snapshot } => {
                        if let Some(repo) = repos.get_mut(id) {
                            repo.waiting = waiting_count(snapshot);
                        }
                    }
                    fanin::Event::Resources { id, resources } => {
                        if repos.contains_key(id) {
                            let generation = *generations.get(id).unwrap_or(&0);
                            let state = ResourceState::Ready { repo_id: id.clone(), generation, snapshot: resources.clone() };
                            resource_deadlines.remove(id);
                            hub.publish_resource(id.clone(), state).await;
                        }
                    }
                    fanin::Event::Down { id } => {
                        repos.remove(id);
                        let generation = *generations.get(id).unwrap_or(&0);
                        let state = ResourceState::Disconnected { repo_id: id.clone(), generation, snapshot: None };
                        resource_deadlines.remove(id);
                        hub.publish_resource(id.clone(), state).await;
                    }
                    fanin::Event::PrStatus { .. } => {}
                }
                model.apply(event);
                let waiting = repos.iter().map(|(id, repo)| (id.clone(), Repo { sock: repo.sock.clone(), waiting: repo.waiting, sent: None, supported: true })).collect();
                attention_tx.send_replace(waiting);
                true
            }
            changed = registry_rx.changed() => {
                changed.is_ok() && model.set_registry(registry_rx.borrow_and_update().clone())
            }
            update = remote_rx.recv() => update.is_some_and(|update| remotes.apply(update)),
            changed = config_rx.changed(), if load_ssh_config => {
                changed.is_ok() && remotes.configure(&config_rx.borrow_and_update(), hub.has_clients(), &remote_tx)
            }
            _ = resource_check.tick() => {
                let expired: Vec<_> = resource_deadlines.iter().filter(|(_, deadline)| **deadline <= Instant::now()).map(|(id, _)| id.clone()).collect();
                for id in expired {
                    resource_deadlines.remove(&id);
                    let generation = *generations.get(&id).unwrap_or(&0);
                    let state = ResourceState::Unavailable { repo_id: id.clone(), generation, snapshot: None };
                    hub.publish_resource(id, state).await;
                }
                remotes.configure(&config_rx.borrow(), hub.has_clients(), &remote_tx)
            }
        };
        if changed {
            hub.publish(inventory_state(&model, &remotes));
        }
        if !repos.is_empty() || hub.has_clients() {
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= timing.idle_exit {
            break Exit::Idle;
        }
    };
    drop(remotes);
    if let Some(task) = config_task {
        task.abort_and_join().await;
    }
    attention.abort_and_join().await;
    registry_task.abort_and_join().await;
    drop(tx);
    drop(rx);
    scan.join().await.context("fan-in scanner task")?;
    shutdown_tx.send_replace(true);
    broker.join().await.context("workspace broker task")??;
    Ok(exit)
}

fn inventory_state(model: &workspace::Workspace, remotes: &Remotes) -> WorkspaceState {
    let mut repositories = model.repositories();
    let (hosts, remote_repositories) = remotes.inventory();
    repositories.extend(remote_repositories);
    WorkspaceState {
        version: workspace::protocol::VERSION,
        revision: 0,
        repositories,
        hosts: Some(hosts),
        selected_repo: None,
        resource: ResourceState::Loading {
            repo_id: String::new(),
            generation: 0,
            snapshot: None,
        },
    }
}

/// Entry point of the hidden `swamp aggregate` command.
pub async fn run() -> Result<()> {
    run_at(crate::util::runtime_base_dir()?, Timing::default())
        .await
        .map(drop)
}

/// Start `exe aggregate` as a detached process. Returns whether the process
/// started. A failure is not an error for the caller: without an aggregator,
/// the cross-repo count stays at zero.
pub fn spawn_detached_from(exe: &Path) -> bool {
    use std::os::unix::process::CommandExt;
    let spawned = std::process::Command::new(exe)
        .arg("aggregate")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
    match spawned {
        Ok(mut child) => {
            // Reap the child when it exits, so that it does not stay as a
            // zombie of a caller that runs for a long time.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(e) => {
            tracing::debug!("could not start the aggregator: {e}");
            false
        }
    }
}

/// Make sure that an aggregator runs. A second instance exits at once, so
/// each launch and each daemon start can call this.
pub fn spawn_detached() {
    // A test binary is not `swamp`, so it must not start itself.
    if cfg!(test) {
        return;
    }
    if let Ok(exe) = std::env::current_exe() {
        spawn_detached_from(&exe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::socket::ServerMsg;
    use crate::daemon::state::WorktreeRow;
    use crate::discover::test_support::{
        InfoReply, RequestLog, row, spawn_recording_daemon, temp_dir,
    };

    fn timing() -> Timing {
        Timing {
            scan_interval: Duration::from_millis(40),
            resend_interval: Duration::from_millis(150),
            idle_exit: Duration::from_millis(400),
            request_timeout: Duration::from_secs(2),
        }
    }

    fn rows(waiting: usize, idle: usize) -> Vec<WorktreeRow> {
        let wait = (0..waiting).map(|i| WorktreeRow {
            agent: AgentStatus::Waiting,
            ..row(&format!("w{i}"), Path::new("/x"))
        });
        let idle = (0..idle).map(|i| row(&format!("i{i}"), Path::new("/x")));
        wait.chain(idle).collect()
    }

    /// The counts in the `ExternalAttention` messages that `log` holds.
    fn counts(log: &RequestLog) -> Vec<usize> {
        log.lock()
            .unwrap()
            .iter()
            .filter_map(|msg| match msg {
                ClientMsg::ExternalAttention { count } => Some(*count),
                _ => None,
            })
            .collect()
    }

    async fn wait_for(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out: {what}");
    }

    #[test]
    fn external_counts_exclude_the_own_repository() {
        let waiting: BTreeMap<String, usize> = [
            ("a".to_string(), 2),
            ("b".to_string(), 1),
            ("c".to_string(), 0),
        ]
        .into();
        let counts = external_counts(&waiting);
        assert_eq!(counts["a"], 1);
        assert_eq!(counts["b"], 2);
        assert_eq!(counts["c"], 3);
        assert!(external_counts(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn waiting_count_skips_a_removal_in_flight() {
        let mut all = rows(2, 3);
        all[0].deleting = true;
        let snapshot = Snapshot {
            rows: all,
            default_known: true,
        };
        assert_eq!(waiting_count(&snapshot), 1);
    }

    #[tokio::test]
    async fn each_daemon_receives_the_waiting_count_of_the_others() {
        let dir = temp_dir("aggregate-two");
        let (log_a, log_b, log_old) = (
            RequestLog::default(),
            RequestLog::default(),
            RequestLog::default(),
        );
        let spawn = |name: &str, rows, reply, log: &RequestLog| {
            spawn_recording_daemon(
                &dir.join(format!("{name}.sock")),
                InfoReply::Reject,
                rows,
                None,
                reply,
                log.clone(),
            )
        };
        // Repository a has 2 waiting worktrees, and b has 1.
        let a = spawn("aaaaaaaaaaaaaaaa", rows(2, 1), Some(ServerMsg::Ok), &log_a);
        let _b = spawn("bbbbbbbbbbbbbbbb", rows(1, 0), Some(ServerMsg::Ok), &log_b);
        // An older daemon closes the connection on the unknown message.
        let _old = spawn("cccccccccccccccc", rows(0, 1), None, &log_old);

        let aggregator = tokio::spawn(run_at(dir.clone(), timing()));

        wait_for("a receives the count of b", || {
            counts(&log_a).last() == Some(&1)
        })
        .await;
        wait_for("b receives the count of a", || {
            counts(&log_b).last() == Some(&2)
        })
        .await;
        // The count is sent again on the interval.
        let sent = counts(&log_b).len();
        wait_for("b receives the count again", || counts(&log_b).len() > sent).await;

        // The older daemon got the message one time and then no more.
        wait_for("the old daemon is tried", || !counts(&log_old).is_empty()).await;
        let tried = counts(&log_old).len();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(counts(&log_old).len(), tried);
        assert_eq!(tried, 1);

        // Repository a stops: b now has no waiting worktree elsewhere.
        a.abort();
        wait_for("b receives zero", || counts(&log_b).last() == Some(&0)).await;

        aggregator.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_count_that_does_not_arrive_is_sent_again() {
        let dir = temp_dir("agg-retry");
        let sock = dir.join("aaaaaaaaaaaaaaaa.sock");
        let repo = |sock: &Path| Repo {
            sock: sock.to_path_buf(),
            waiting: 0,
            sent: None,
            supported: true,
        };
        let mut repos = BTreeMap::from([("a".to_string(), repo(&sock))]);
        let wait = Duration::from_secs(5);

        // Nothing listens: the request does not arrive.
        send_counts(&mut repos, false, wait).await;
        assert!(repos["a"].supported);
        assert_eq!(repos["a"].sent, None);

        // The daemon answers now, and the subsequent send delivers the count.
        let log = RequestLog::default();
        let daemon = spawn_recording_daemon(
            &sock,
            InfoReply::Reject,
            vec![],
            None,
            Some(ServerMsg::Ok),
            log.clone(),
        );
        send_counts(&mut repos, false, wait).await;
        assert_eq!(repos["a"].sent, Some(0));
        assert_eq!(counts(&log), [0]);
        daemon.abort();

        // A daemon that refuses the message gets no more messages.
        for (name, reply) in [
            ("bbbbbbbbbbbbbbbb.sock", None),
            (
                "cccccccccccccccc.sock",
                Some(ServerMsg::Err {
                    message: "unsupported request".into(),
                }),
            ),
        ] {
            let sock = dir.join(name);
            let log = RequestLog::default();
            let daemon =
                spawn_recording_daemon(&sock, InfoReply::Reject, vec![], None, reply, log.clone());
            let mut repos = BTreeMap::from([("a".to_string(), repo(&sock))]);
            send_counts(&mut repos, true, wait).await;
            send_counts(&mut repos, true, wait).await;
            assert!(!repos["a"].supported, "{name}");
            assert_eq!(counts(&log), [0], "{name}");
            daemon.abort();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stalled_attention_request_does_not_block_inventory_publication() {
        let dir = temp_dir("agg-attention-stall");
        let sock = dir.join("slow.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let daemon = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = crate::daemon::socket::read_msg(&mut stream).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let repo = Repo {
            sock,
            waiting: 0,
            sent: None,
            supported: true,
        };
        let repos = BTreeMap::from([("slow".to_string(), repo)]);
        let (updates_tx, updates_rx) = tokio::sync::watch::channel(BTreeMap::new());
        let worker = tokio::spawn(attention_worker(
            updates_rx,
            Duration::from_secs(30),
            Duration::from_millis(300),
            dir.join(LOCK_FILE),
        ));
        updates_tx.send_replace(repos);

        let hub = crate::workspace::broker::StateHub::new(WorkspaceState {
            version: crate::workspace::protocol::VERSION,
            revision: 0,
            repositories: Vec::new(),
            hosts: None,
            selected_repo: None,
            resource: ResourceState::Loading {
                repo_id: String::new(),
                generation: 0,
                snapshot: None,
            },
        });
        let mut client = hub.subscribe_inventory();
        let start = Instant::now();
        hub.publish(WorkspaceState {
            version: crate::workspace::protocol::VERSION,
            revision: 1,
            repositories: Vec::new(),
            hosts: None,
            selected_repo: None,
            resource: ResourceState::Loading {
                repo_id: String::new(),
                generation: 0,
                snapshot: None,
            },
        });
        assert!(client.changed().await.is_ok());
        assert!(start.elapsed() < Duration::from_millis(100));

        drop(updates_tx);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        daemon.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn attention_takes_over_the_legacy_lock_after_an_old_aggregator_exits() {
        let dir = temp_dir("agg-attention-upgrade");
        let legacy_lock = dir.join(LOCK_FILE);
        let held = try_lock(&legacy_lock)
            .unwrap()
            .expect("legacy lock is free");
        let sock = dir.join("aaaaaaaaaaaaaaaa.sock");
        let log = RequestLog::default();
        let daemon = spawn_recording_daemon(
            &sock,
            InfoReply::Reject,
            rows(0, 1),
            None,
            Some(ServerMsg::Ok),
            log.clone(),
        );
        let repo = Repo {
            sock,
            waiting: 0,
            sent: None,
            supported: true,
        };
        let (updates_tx, updates_rx) = tokio::sync::watch::channel(BTreeMap::new());
        let worker = tokio::spawn(attention_worker(
            updates_rx,
            Duration::from_millis(30),
            Duration::from_secs(2),
            legacy_lock,
        ));
        updates_tx.send_replace(BTreeMap::from([("repo".into(), repo)]));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(counts(&log).is_empty(), "do not compete with the old owner");

        drop(held);
        wait_for("attention lock handoff", || !counts(&log).is_empty()).await;
        worker.abort();
        daemon.abort();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn legacy_attention_lock_does_not_block_the_workspace_broker() {
        let dir = temp_dir("aggregate-upgrade-broker");
        let legacy = try_lock(&dir.join(LOCK_FILE))
            .unwrap()
            .expect("legacy attention lock is free");
        let endpoint = workspace::protocol::socket_path(&dir);
        let task = tokio::spawn(run_at(
            dir.clone(),
            Timing {
                idle_exit: Duration::from_secs(30),
                ..timing()
            },
        ));
        wait_for("workspace broker beside legacy aggregator", || {
            endpoint.exists()
        })
        .await;
        assert!(!task.is_finished());
        task.abort();
        let _ = task.await;
        drop(legacy);
        assert!(!endpoint.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn aborting_aggregator_closes_follower_and_removes_only_its_broker_socket() {
        let dir = temp_dir("aggregate-cancel");
        let daemon_path = dir.join("aaaaaaaaaaaaaaaa.sock");
        let listener = tokio::net::UnixListener::bind(&daemon_path).unwrap();
        let (subscribed_tx, subscribed_rx) = tokio::sync::oneshot::channel();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let daemon = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            assert!(matches!(
                crate::daemon::socket::read_msg(&mut stream).await.unwrap(),
                Some(ClientMsg::GetInfo)
            ));
            crate::daemon::socket::write_msg(
                &mut stream,
                &crate::daemon::socket::ServerMsg::Info {
                    common_dir: "/repo/.bare".into(),
                    session_name: "repo-session".into(),
                    version: "test".into(),
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                crate::daemon::socket::read_msg(&mut stream).await.unwrap(),
                Some(ClientMsg::Subscribe)
            ));
            crate::daemon::socket::write_msg(
                &mut stream,
                &crate::daemon::socket::ServerMsg::Snapshot(Snapshot::default()),
            )
            .await
            .unwrap();
            let _ = subscribed_tx.send(());
            let mut byte = [0];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut byte).await;
            let _ = closed_tx.send(());
        });
        let aggregator = tokio::spawn(run_at(
            dir.clone(),
            Timing {
                idle_exit: Duration::from_secs(30),
                ..timing()
            },
        ));
        tokio::time::timeout(Duration::from_secs(3), subscribed_rx)
            .await
            .unwrap()
            .unwrap();
        let endpoint = workspace::protocol::socket_path(&dir);
        assert!(endpoint.exists());
        aggregator.abort();
        let _ = aggregator.await;
        tokio::time::timeout(Duration::from_secs(3), closed_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!endpoint.exists());
        daemon.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn multiple_workspace_clients_share_one_upstream_subscription() {
        let dir = temp_dir("aggregate-shared-sub");
        let log = RequestLog::default();
        let daemon = spawn_recording_daemon(
            &dir.join("aaaaaaaaaaaaaaaa.sock"),
            InfoReply::Answer(crate::discover::RepoIdentity {
                common_dir: "/repo/.bare".into(),
                session_name: "repo-session".into(),
                version: Some("test".into()),
            }),
            rows(1, 1),
            None,
            Some(ServerMsg::Ok),
            log.clone(),
        );
        let aggregator = tokio::spawn(run_at(dir.clone(), timing()));
        let clients: Vec<_> = (0..4)
            .map(|_| crate::workspace::client::WorkspaceClient::start(dir.clone(), None))
            .collect();
        for client in &clients {
            let mut state = client.state();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if state.borrow().as_ref().is_some_and(|state| {
                        state
                            .repositories
                            .iter()
                            .any(|repo| repo.snapshot.is_some())
                    }) {
                        break;
                    }
                    state.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(
            log.lock()
                .unwrap()
                .iter()
                .filter(|msg| matches!(msg, ClientMsg::Subscribe))
                .count(),
            1
        );
        for client in clients {
            client.close().await;
        }
        aggregator.abort();
        let _ = aggregator.await;
        daemon.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_second_instance_exits_with_success_and_an_idle_one_exits() {
        let dir = temp_dir("aggregate-lock");
        let lock = dir.join(BROKER_LOCK_FILE);

        // The test holds the lock, as a first instance does. A probe of the
        // lock of a real first instance can take the lock before it does.
        let held = try_lock(&lock).unwrap().expect("the lock is free");
        let second = run_at(dir.clone(), timing()).await.unwrap();
        assert_eq!(second, Exit::AlreadyRunning);
        drop(held);

        // A child process that a parallel test forks can hold a copy of the
        // lock file for a moment, so each step that needs the free lock
        // waits for it.
        let free = || matches!(try_lock(&lock), Ok(Some(_)));

        // No daemon is live, so an instance with the lock exits after the
        // idle time.
        let mut exit = Exit::AlreadyRunning;
        for _ in 0..50 {
            wait_for("the lock is free", free).await;
            exit = tokio::time::timeout(Duration::from_secs(5), run_at(dir.clone(), timing()))
                .await
                .expect("the idle aggregator exits")
                .unwrap();
            if exit != Exit::AlreadyRunning {
                break;
            }
        }
        assert_eq!(exit, Exit::Idle);
        wait_for("the lock is free again", free).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_spawn_failure_is_reported_and_does_not_panic() {
        assert!(!spawn_detached_from(Path::new("/nonexistent/swamp-binary")));
    }
}
