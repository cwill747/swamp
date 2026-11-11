//! Subscribe to each live per-repo daemon and merge their broadcasts into one
//! stream of events.
//!
//! The module scans the runtime directory for daemon sockets on an interval.
//! It follows each daemon on its own connection, and drops a connection that
//! fails. The next scan connects again.

use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::discover::{self, RepoIdentity};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub const SCAN_INTERVAL: Duration = Duration::from_secs(2);

/// One change in the set of live daemons or in the state of one daemon. `id`
/// is the socket file stem, which is the `repo_id` of the repository.
#[derive(Debug, Clone)]
pub enum Event {
    /// A daemon is connected. The event repeats when a later snapshot
    /// identifies a repository that was unknown before.
    Up {
        id: String,
        sock: PathBuf,
        identity: Option<RepoIdentity>,
    },
    Snapshot {
        id: String,
        snapshot: Snapshot,
    },
    PrStatus {
        id: String,
        prs: PrSnapshot,
    },
    /// The connection to the daemon closed.
    Down {
        id: String,
    },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub runtime_dir: PathBuf,
    /// Registry file for the identification of a daemon that predates
    /// `GetInfo` and whose worktree paths are gone.
    pub registry: Option<PathBuf>,
    pub scan_interval: Duration,
}

/// Start the scan loop. The task stops when the receiver of `tx` is dropped.
pub fn spawn(cfg: Config, tx: mpsc::Sender<Event>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let active: Arc<Mutex<HashSet<String>>> = Arc::default();
        let mut interval = tokio::time::interval(cfg.scan_interval);
        loop {
            interval.tick().await;
            if tx.is_closed() {
                return;
            }
            for sock in discover::socket_files(&cfg.runtime_dir) {
                let Some(id) = sock.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                    continue;
                };
                if !active.lock().unwrap().insert(id.clone()) {
                    continue;
                }
                let active = active.clone();
                let registry = cfg.registry.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    follow(&sock, &id, registry.as_deref(), &tx).await;
                    active.lock().unwrap().remove(&id);
                });
            }
        }
    })
}

/// Follow one daemon until its connection closes.
async fn follow(sock: &Path, id: &str, registry: Option<&Path>, tx: &mpsc::Sender<Event>) {
    let Some(mut stream) = discover::connect(sock).await else {
        return;
    };
    let mut identity = discover::get_info(&mut stream).await;
    if identity.is_none() {
        // A daemon that predates `GetInfo` closes the connection.
        let Some(fresh) = discover::connect(sock).await else {
            return;
        };
        stream = fresh;
    }
    if write_client_msg(&mut stream, &ClientMsg::Subscribe)
        .await
        .is_err()
    {
        return;
    }

    let up = |identity: &Option<RepoIdentity>| Event::Up {
        id: id.to_string(),
        sock: sock.to_path_buf(),
        identity: identity.clone(),
    };
    let mut announced = false;
    if identity.is_some() {
        announced = true;
        if tx.send(up(&identity)).await.is_err() {
            return;
        }
    }

    while let Ok(Some(msg)) = read_server_msg(&mut stream).await {
        let event = match msg {
            ServerMsg::Snapshot(snapshot) => {
                if identity.is_none() {
                    identity = discover::identity_from_rows(id, &snapshot.rows).or_else(|| {
                        let entries = crate::registry::load_at(registry?);
                        discover::identity_from_registry(id, &entries)
                    });
                    if identity.is_some() || !announced {
                        announced = true;
                        if tx.send(up(&identity)).await.is_err() {
                            return;
                        }
                    }
                }
                Event::Snapshot {
                    id: id.to_string(),
                    snapshot,
                }
            }
            ServerMsg::PrStatus(prs) => Event::PrStatus {
                id: id.to_string(),
                prs,
            },
            _ => continue,
        };
        if tx.send(event).await.is_err() {
            return;
        }
    }
    if announced {
        let _ = tx.send(Event::Down { id: id.to_string() }).await;
    }
}

/// How long a daemon can take to answer a one-shot request. A default-branch
/// update fetches from the remote, so the limit is generous.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Send one request to the daemon at `sock` on a new connection and wait for
/// the result. Messages that are not a result, such as a broadcast, are
/// skipped.
pub async fn request(sock: &Path, msg: &ClientMsg, timeout: Duration) -> Result<(), String> {
    let exchange = async {
        let mut stream = discover::connect(sock)
            .await
            .ok_or_else(|| "the daemon does not answer".to_string())?;
        write_client_msg(&mut stream, msg)
            .await
            .map_err(|e| format!("{e:#}"))?;
        loop {
            match read_server_msg(&mut stream).await {
                Ok(Some(ServerMsg::Ok | ServerMsg::RefreshDone { .. })) => return Ok(()),
                Ok(Some(ServerMsg::Err { message })) => return Err(message),
                Ok(Some(ServerMsg::ErrDirty { reason, .. })) => return Err(reason),
                Ok(Some(_)) => continue,
                Ok(None) => return Err("the daemon closed the connection".to_string()),
                Err(e) => return Err(format!("{e:#}")),
            }
        }
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result,
        Err(_) => Err(format!("no answer in {} seconds", timeout.as_secs())),
    }
}

/// Result of [`update_default_all`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UpdateReport {
    /// Number of repositories that completed the update.
    pub updated: usize,
    /// Name and error text of each repository that failed, sorted by name.
    pub failed: Vec<(String, String)>,
}

impl UpdateReport {
    /// One line for a status bar.
    pub fn summary(&self) -> String {
        let total = self.updated + self.failed.len();
        if total == 0 {
            return "No repo runs, so there is nothing to update".to_string();
        }
        let repos = if total == 1 { "repo" } else { "repos" };
        if self.failed.is_empty() {
            return format!("Updated {total} {repos}");
        }
        let failures: Vec<String> = self
            .failed
            .iter()
            .map(|(name, error)| format!("{name} ({error})"))
            .collect();
        format!(
            "Updated {} of {total} {repos}. Failed: {}",
            self.updated,
            failures.join(", ")
        )
    }
}

/// Ask each daemon in `targets` (repository name and socket) to update its
/// default branch. The requests run concurrently, and one failure does not
/// stop the others.
pub async fn update_default_all(
    targets: Vec<(String, PathBuf)>,
    timeout: Duration,
) -> UpdateReport {
    let mut requests = tokio::task::JoinSet::new();
    for (name, sock) in targets {
        requests.spawn(async move {
            let result = request(&sock, &ClientMsg::UpdateDefault, timeout).await;
            (name, result)
        });
    }
    let mut report = UpdateReport::default();
    while let Some(joined) = requests.join_next().await {
        match joined {
            Ok((_, Ok(()))) => report.updated += 1,
            Ok((name, Err(error))) => report.failed.push((name, error)),
            Err(e) => report.failed.push(("?".to_string(), e.to_string())),
        }
    }
    report.failed.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::test_support::{
        InfoReply, row, spawn_fake_daemon, stale_socket, temp_dir,
    };
    use crate::util::{repo_id, session_name_for};
    use crate::worktree::create_worktree;
    use crate::worktree::test_support::{git_available, setup};

    const WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn update_all_reaches_each_daemon_and_names_the_one_that_fails() {
        use crate::discover::test_support::spawn_fake_daemon_with;
        let dir = temp_dir("fanin-update");
        let good = dir.join("aaaaaaaaaaaaaaaa.sock");
        let bad = dir.join("bbbbbbbbbbbbbbbb.sock");
        let gone = dir.join("cccccccccccccccc.sock");
        let _good =
            spawn_fake_daemon_with(&good, InfoReply::Reject, Vec::new(), None, ServerMsg::Ok);
        let _bad = spawn_fake_daemon_with(
            &bad,
            InfoReply::Reject,
            Vec::new(),
            None,
            ServerMsg::Err {
                message: "no default branch".into(),
            },
        );
        stale_socket(&gone);

        let report = update_default_all(
            vec![
                ("alpha".into(), good.clone()),
                ("beta".into(), bad),
                ("gamma".into(), gone),
            ],
            WAIT,
        )
        .await;
        assert_eq!(report.updated, 1);
        assert_eq!(
            report.failed,
            [
                ("beta".to_string(), "no default branch".to_string()),
                (
                    "gamma".to_string(),
                    "the daemon does not answer".to_string()
                ),
            ]
        );
        assert_eq!(
            report.summary(),
            "Updated 1 of 3 repos. Failed: beta (no default branch), gamma (the daemon does not answer)"
        );

        let report = update_default_all(vec![("alpha".into(), good)], WAIT).await;
        assert_eq!(report.summary(), "Updated 1 repo");
        assert_eq!(
            update_default_all(Vec::new(), WAIT).await.summary(),
            "No repo runs, so there is nothing to update"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn request_skips_messages_that_are_not_a_result() {
        use crate::daemon::socket::{read_msg, write_msg};
        let dir = temp_dir("fanin-request");
        let sock = dir.join("aaaaaaaaaaaaaaaa.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let daemon = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let got: Option<ClientMsg> = read_msg(&mut stream).await.unwrap();
            assert!(matches!(got, Some(ClientMsg::UpdateDefault)));
            let broadcast = ServerMsg::Snapshot(Snapshot::default());
            write_msg(&mut stream, &broadcast).await.unwrap();
            write_msg(&mut stream, &ServerMsg::Ok).await.unwrap();
        });
        assert_eq!(
            request(&sock, &ClientMsg::UpdateDefault, WAIT).await,
            Ok(())
        );
        daemon.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn config(dir: &Path, registry: Option<PathBuf>) -> Config {
        Config {
            runtime_dir: dir.to_path_buf(),
            registry,
            scan_interval: Duration::from_millis(50),
        }
    }

    fn identity(name: &str) -> RepoIdentity {
        RepoIdentity {
            common_dir: PathBuf::from(format!("/repos/{name}/.bare")),
            session_name: format!("{name}-0000"),
            version: Some("9.9.9".into()),
        }
    }

    async fn next(rx: &mut mpsc::Receiver<Event>) -> Event {
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("an event before the timeout")
            .expect("an open channel")
    }

    /// Wait for the `Up` event of `id` and return its identity.
    async fn wait_up(rx: &mut mpsc::Receiver<Event>, id: &str) -> Option<RepoIdentity> {
        loop {
            if let Event::Up {
                id: got, identity, ..
            } = next(rx).await
                && got == id
            {
                return identity;
            }
        }
    }

    async fn wait_down(rx: &mut mpsc::Receiver<Event>, id: &str) {
        loop {
            if let Event::Down { id: got } = next(rx).await
                && got == id
            {
                return;
            }
        }
    }

    #[tokio::test]
    async fn follows_two_daemons_and_reports_a_stop_and_a_late_start() {
        let dir = temp_dir("fanin-two");
        let a = spawn_fake_daemon(
            &dir.join("aaaaaaaaaaaaaaaa.sock"),
            InfoReply::Answer(identity("a")),
            vec![row("feature", Path::new("/repos/a/feature"))],
        );
        let _b = spawn_fake_daemon(
            &dir.join("bbbbbbbbbbbbbbbb.sock"),
            InfoReply::Answer(identity("b")),
            Vec::new(),
        );
        // A stale socket file must not block the scan.
        stale_socket(&dir.join("dddddddddddddddd.sock"));

        let (tx, mut rx) = mpsc::channel(64);
        let task = spawn(config(&dir, None), tx);

        let mut up = HashSet::new();
        let mut snapshots = HashSet::new();
        while up.len() < 2 || snapshots.len() < 2 {
            match next(&mut rx).await {
                Event::Up { id, identity, .. } => {
                    assert!(identity.is_some());
                    up.insert(id);
                }
                Event::Snapshot { id, .. } => {
                    snapshots.insert(id);
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert!(up.contains("aaaaaaaaaaaaaaaa") && up.contains("bbbbbbbbbbbbbbbb"));

        a.abort();
        wait_down(&mut rx, "aaaaaaaaaaaaaaaa").await;

        // A daemon that starts after the first scan is found by a later scan.
        let _c = spawn_fake_daemon(
            &dir.join("cccccccccccccccc.sock"),
            InfoReply::Answer(identity("c")),
            Vec::new(),
        );
        assert_eq!(
            wait_up(&mut rx, "cccccccccccccccc").await,
            Some(identity("c"))
        );

        task.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn identifies_an_old_daemon_from_row_paths_then_the_registry() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();
        let dir = temp_dir("fanin-old");
        let id = repo_id(&bare);
        let sock = dir.join(format!("{id}.sock"));
        let gone = vec![row("gone", &root.join("no-such-worktree"))];

        // Usable row paths.
        let daemon = spawn_fake_daemon(&sock, InfoReply::Reject, vec![row("feature", &wt.path)]);
        let (tx, mut rx) = mpsc::channel(64);
        let task = spawn(config(&dir, None), tx);
        let found = wait_up(&mut rx, &id).await.expect("identified");
        assert_eq!(found.common_dir, bare.canonicalize().unwrap());
        assert_eq!(found.version, None);
        task.abort();
        daemon.abort();
        let _ = std::fs::remove_file(&sock);

        // Unusable row paths, and the repository is registered.
        let registry = dir.join("repos.json");
        crate::registry::upsert_at(&registry, &bare, &session_name_for(&bare), 1).unwrap();
        let daemon = spawn_fake_daemon(&sock, InfoReply::Reject, gone.clone());
        let (tx, mut rx) = mpsc::channel(64);
        let task = spawn(config(&dir, Some(registry.clone())), tx);
        let found = wait_up(&mut rx, &id).await.expect("identified");
        assert_eq!(found.session_name, session_name_for(&bare));
        task.abort();
        daemon.abort();
        let _ = std::fs::remove_file(&sock);

        // Unusable row paths, and the repository is not registered. The
        // daemon is still reported, and its snapshot still arrives.
        crate::registry::remove_at(&registry, &bare).unwrap();
        let daemon = spawn_fake_daemon(&sock, InfoReply::Reject, gone);
        let (tx, mut rx) = mpsc::channel(64);
        let task = spawn(config(&dir, Some(registry)), tx);
        assert_eq!(wait_up(&mut rx, &id).await, None);
        assert!(matches!(next(&mut rx).await, Event::Snapshot { .. }));
        task.abort();
        daemon.abort();

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
    }
}
