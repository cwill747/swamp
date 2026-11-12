//! `swamp aggregate`: the hidden process that tells each repo daemon how many
//! worktrees wait for the user in the other repositories.
//!
//! One instance serves all daemons. It follows each daemon through the fan-in
//! module, and sends each daemon its count when the count changes and on an
//! interval. A daemon drops a count that is not sent again, so an aggregator
//! that stops cannot leave a stale badge. The process holds no state on disk
//! and exits when no daemon has run for a while.

use crate::daemon::socket::ClientMsg;
use crate::daemon::state::{AgentStatus, Snapshot};
use crate::fanin;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const LOCK_FILE: &str = "aggregator.lock";

#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub scan_interval: Duration,
    /// Interval at which each count is sent again. It must be shorter than
    /// the lifetime of a count in the daemon.
    pub resend_interval: Duration,
    /// How long the process runs with no live daemon before it exits.
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
    /// No daemon was live for the idle time.
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

/// Run the aggregator for the daemons in `runtime_dir`.
pub async fn run_at(runtime_dir: PathBuf, timing: Timing) -> Result<Exit> {
    std::fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("create {}", runtime_dir.display()))?;
    let Some(_lock) = try_lock(&runtime_dir.join(LOCK_FILE))? else {
        return Ok(Exit::AlreadyRunning);
    };

    let (tx, mut rx) = mpsc::channel(256);
    let scan = fanin::spawn(
        fanin::Config {
            runtime_dir,
            // The aggregator needs no repository identity.
            registry: None,
            scan_interval: timing.scan_interval,
        },
        tx,
    );
    let mut repos: BTreeMap<String, Repo> = BTreeMap::new();
    let mut resend = tokio::time::interval(timing.resend_interval);
    let mut idle_since = Instant::now();
    let exit = loop {
        tokio::select! {
            event = rx.recv() => {
                let Some(event) = event else { break Exit::Idle };
                match event {
                    fanin::Event::Up { id, sock, .. } => {
                        repos.entry(id).or_insert(Repo {
                            sock,
                            waiting: 0,
                            sent: None,
                            supported: true,
                        });
                    }
                    fanin::Event::Snapshot { id, snapshot } => {
                        if let Some(repo) = repos.get_mut(&id) {
                            repo.waiting = waiting_count(&snapshot);
                        }
                    }
                    fanin::Event::Down { id } => {
                        repos.remove(&id);
                    }
                    fanin::Event::PrStatus { .. } => continue,
                }
                send_counts(&mut repos, false, timing.request_timeout).await;
            }
            _ = resend.tick() => {
                send_counts(&mut repos, true, timing.request_timeout).await;
            }
        }
        if !repos.is_empty() {
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= timing.idle_exit {
            break Exit::Idle;
        }
    };
    scan.abort();
    Ok(exit)
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
    async fn a_second_instance_exits_with_success_and_an_idle_one_exits() {
        let dir = temp_dir("aggregate-lock");
        let lock = dir.join(LOCK_FILE);

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
