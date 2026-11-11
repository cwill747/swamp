//! Find the live per-repo daemons and identify the repository behind each one.
//!
//! A daemon socket is named `<repo_id>.sock`, and `repo_id` is a one-way hash
//! of the git common directory. A current daemon answers `GetInfo`. An older
//! daemon does not, so its repository is derived from the worktree paths in
//! its snapshot and checked against the socket name.

use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::daemon::state::WorktreeRow;
use crate::registry::RepoEntry;
use crate::util::{repo_id, session_name_for};
use crate::worktree::git_common_dir;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoIdentity {
    pub common_dir: PathBuf,
    pub session_name: String,
    /// `None` for a daemon that predates `GetInfo`.
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Nothing accepts connections on the socket (a stale socket file).
    NotLive,
    /// A daemon answers, but its repository cannot be determined.
    Unidentified,
    Identified(RepoIdentity),
}

/// Every `*.sock` file in `runtime_dir`, sorted. A missing directory is empty.
pub fn socket_files(runtime_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return Vec::new();
    };
    let mut socks: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "sock"))
        .collect();
    socks.sort();
    socks
}

/// Connect to the daemon at `sock` and determine which repository it serves.
///
/// `registry` holds the registered repositories. It identifies an older
/// daemon whose snapshot has no usable worktree path.
pub async fn probe(sock: &Path, registry: &[RepoEntry]) -> Probe {
    let Some(mut stream) = connect(sock).await else {
        return Probe::NotLive;
    };
    if let Some(identity) = get_info(&mut stream).await {
        return Probe::Identified(identity);
    }
    drop(stream);

    // An older daemon closes the connection on a request that it cannot
    // decode, so the snapshot needs a new connection.
    let Some(stem) = sock.file_stem().and_then(|s| s.to_str()) else {
        return Probe::Unidentified;
    };
    let rows = first_snapshot_rows(sock).await.unwrap_or_default();
    match identity_from_rows(stem, &rows).or_else(|| identity_from_registry(stem, registry)) {
        Some(identity) => Probe::Identified(identity),
        None => Probe::Unidentified,
    }
}

/// Connect to a daemon socket. `None` means that nothing accepts connections.
pub async fn connect(sock: &Path) -> Option<UnixStream> {
    tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(sock))
        .await
        .ok()?
        .ok()
}

/// Send `GetInfo` on `stream`. `None` means that the daemon predates the
/// request. Such a daemon closes the connection, so the caller must open a
/// new one before it sends more requests.
pub async fn get_info(stream: &mut UnixStream) -> Option<RepoIdentity> {
    write_client_msg(stream, &ClientMsg::GetInfo).await.ok()?;
    match tokio::time::timeout(REPLY_TIMEOUT, read_server_msg(stream)).await {
        Ok(Ok(Some(ServerMsg::Info {
            common_dir,
            session_name,
            version,
        }))) => Some(RepoIdentity {
            common_dir,
            session_name,
            version: Some(version),
        }),
        _ => None,
    }
}

async fn first_snapshot_rows(sock: &Path) -> Option<Vec<WorktreeRow>> {
    let mut stream = connect(sock).await?;
    write_client_msg(&mut stream, &ClientMsg::Subscribe)
        .await
        .ok()?;
    let wait = async {
        loop {
            match read_server_msg(&mut stream).await {
                Ok(Some(ServerMsg::Snapshot(snap))) => return Some(snap.rows),
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return None,
            }
        }
    };
    tokio::time::timeout(SNAPSHOT_TIMEOUT, wait).await.ok()?
}

/// Derive the repository from the worktree paths that a daemon reports. A
/// candidate is accepted only when its `repo_id` equals `socket_stem`, which
/// proves that the daemon was started for that git common directory.
pub fn identity_from_rows(socket_stem: &str, rows: &[WorktreeRow]) -> Option<RepoIdentity> {
    rows.iter()
        .filter(|row| row.path.exists())
        .filter_map(|row| git_common_dir(&row.path).ok())
        .find(|common| repo_id(common) == socket_stem)
        .map(|common| RepoIdentity {
            session_name: session_name_for(&common),
            common_dir: std::fs::canonicalize(&common).unwrap_or(common),
            version: None,
        })
}

/// Find the registered repository whose `repo_id` equals `socket_stem`. This
/// identifies an older daemon whose worktree paths are all gone.
pub fn identity_from_registry(socket_stem: &str, entries: &[RepoEntry]) -> Option<RepoIdentity> {
    entries
        .iter()
        .find(|entry| repo_id(&entry.common_dir) == socket_stem)
        .map(|entry| RepoIdentity {
            common_dir: entry.common_dir.clone(),
            session_name: entry.session_name.clone(),
            version: None,
        })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::daemon::socket::{read_msg, write_msg};
    use crate::daemon::state::{AgentStatus, PrSnapshot, Snapshot};
    use tokio::net::UnixListener;

    pub(crate) fn row(name: &str, path: &Path) -> WorktreeRow {
        WorktreeRow {
            name: name.to_string(),
            path: path.to_path_buf(),
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
            agent: AgentStatus::Idle,
            agent_ts: 0,
            session_name: None,
            head_ts: 0,
            harness: None,
            is_default: false,
            removal_block: None,
            deleting: false,
            subagents: Vec::new(),
        }
    }

    /// Make a new temporary directory. `_tag` names the caller for the
    /// reader only.
    ///
    /// The directory name is short because tests bind sockets in it, and
    /// macOS limits a socket path to 104 bytes.
    pub(crate) fn temp_dir(_tag: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sw-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Create a socket file that nothing listens on, as a daemon that died
    /// leaves behind.
    ///
    /// The socket is a datagram socket, so a stream connection always fails.
    /// A listener that is bound and then dropped is not sufficient: a child
    /// process that a parallel test forks can hold a copy of the listener for
    /// a moment, and a connection in that moment succeeds.
    pub(crate) fn stale_socket(path: &Path) {
        drop(std::os::unix::net::UnixDatagram::bind(path).unwrap());
    }

    /// How a fake daemon answers `GetInfo`.
    #[derive(Clone)]
    pub(crate) enum InfoReply {
        /// Close the connection, as a daemon that predates `GetInfo` does.
        Reject,
        Answer(RepoIdentity),
    }

    /// Serve `sock` like a daemon: answer `GetInfo` per `info`, and answer
    /// `Subscribe` with one snapshot of `rows`, then hold the connection open.
    /// An abort of the returned task also closes each open connection.
    pub(crate) fn spawn_fake_daemon(
        sock: &Path,
        info: InfoReply,
        rows: Vec<WorktreeRow>,
    ) -> tokio::task::JoinHandle<()> {
        spawn_fake_daemon_with(sock, info, rows, None, ServerMsg::Ok)
    }

    /// Like [`spawn_fake_daemon`], with a PR snapshot that follows the
    /// worktree snapshot and a fixed `reply` to each other request.
    pub(crate) fn spawn_fake_daemon_with(
        sock: &Path,
        info: InfoReply,
        rows: Vec<WorktreeRow>,
        prs: Option<PrSnapshot>,
        reply: ServerMsg,
    ) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(sock).unwrap();
        tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let info = info.clone();
                let rows = rows.clone();
                let prs = prs.clone();
                let reply = reply.clone();
                connections.spawn(async move {
                    while let Ok(Some(msg)) = read_msg(&mut stream).await {
                        match (msg, &info) {
                            (ClientMsg::GetInfo, InfoReply::Reject) => return,
                            (ClientMsg::GetInfo, InfoReply::Answer(id)) => {
                                let reply = ServerMsg::Info {
                                    common_dir: id.common_dir.clone(),
                                    session_name: id.session_name.clone(),
                                    version: id.version.clone().unwrap_or_default(),
                                };
                                let _ = write_msg(&mut stream, &reply).await;
                            }
                            (ClientMsg::Subscribe, _) => {
                                let snap = Snapshot {
                                    rows: rows.clone(),
                                    default_known: true,
                                };
                                let _ = write_msg(&mut stream, &ServerMsg::Snapshot(snap)).await;
                                if let Some(prs) = &prs {
                                    let _ =
                                        write_msg(&mut stream, &ServerMsg::PrStatus(prs.clone()))
                                            .await;
                                }
                            }
                            _ => {
                                let _ = write_msg(&mut stream, &reply).await;
                            }
                        }
                    }
                });
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::worktree::create_worktree;
    use crate::worktree::test_support::{git_available, setup};

    #[test]
    fn socket_files_lists_only_sockets() {
        let dir = temp_dir("socks");
        for name in ["b.sock", "a.sock", "a.pid", "a.lock", "layout.kdl"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let names: Vec<String> = socket_files(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.sock", "b.sock"]);
        assert!(socket_files(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identity_from_rows_accepts_only_a_matching_hash() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();
        let stem = repo_id(&bare);
        let rows = vec![
            row("gone", &root.join("no-such-worktree")),
            row("feature", &wt.path),
        ];

        let identity = identity_from_rows(&stem, &rows).expect("identified");
        assert_eq!(identity.common_dir, bare.canonicalize().unwrap());
        assert_eq!(identity.session_name, session_name_for(&bare));
        assert_eq!(identity.version, None);

        // The paths resolve to a repository, but not the one this socket names.
        assert_eq!(identity_from_rows("0000000000000000", &rows), None);
        // No reported path exists any more.
        assert_eq!(identity_from_rows(&stem, &rows[..1]), None);
        assert_eq!(identity_from_rows(&stem, &[]), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn probe_uses_get_info_when_the_daemon_supports_it() {
        let dir = temp_dir("probe-new");
        let sock = dir.join("feedfacefeedface.sock");
        let identity = RepoIdentity {
            common_dir: PathBuf::from("/repos/demo/.bare"),
            session_name: "demo-feed".into(),
            version: Some("9.9.9".into()),
        };
        let daemon = spawn_fake_daemon(&sock, InfoReply::Answer(identity.clone()), Vec::new());

        assert_eq!(probe(&sock, &[]).await, Probe::Identified(identity));
        daemon.abort();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn probe_falls_back_to_snapshot_paths_for_an_old_daemon() {
        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();
        let dir = temp_dir("probe-old");

        let sock = dir.join(format!("{}.sock", repo_id(&bare)));
        let daemon = spawn_fake_daemon(&sock, InfoReply::Reject, vec![row("feature", &wt.path)]);
        match probe(&sock, &[]).await {
            Probe::Identified(id) => {
                assert_eq!(id.common_dir, bare.canonicalize().unwrap());
                assert_eq!(id.version, None);
            }
            other => panic!("expected Identified, got {other:?}"),
        }
        daemon.abort();

        // Same rows behind a socket whose name does not match the repository.
        let wrong = dir.join("0000000000000000.sock");
        let daemon = spawn_fake_daemon(&wrong, InfoReply::Reject, vec![row("feature", &wt.path)]);
        assert_eq!(probe(&wrong, &[]).await, Probe::Unidentified);
        daemon.abort();

        // A daemon whose worktree paths are all gone.
        let gone = dir.join("1111111111111111.sock");
        let daemon = spawn_fake_daemon(
            &gone,
            InfoReply::Reject,
            vec![row("gone", &root.join("no-such-worktree"))],
        );
        assert_eq!(probe(&gone, &[]).await, Probe::Unidentified);
        daemon.abort();

        // The registry identifies an old daemon that reports no usable path.
        let empty = dir.join(format!("{}.sock", repo_id(&bare)));
        let _ = std::fs::remove_file(&empty);
        let daemon = spawn_fake_daemon(&empty, InfoReply::Reject, vec![]);
        let entry = RepoEntry {
            common_dir: bare.clone(),
            session_name: "demo-1a2b".into(),
            last_launched: 0,
        };
        assert_eq!(probe(&empty, &[]).await, Probe::Unidentified);
        assert_eq!(
            probe(&empty, std::slice::from_ref(&entry)).await,
            Probe::Identified(RepoIdentity {
                common_dir: bare.clone(),
                session_name: "demo-1a2b".into(),
                version: None,
            })
        );
        // An entry of a different repository does not match the socket.
        let other = RepoEntry {
            common_dir: root.join("other/.bare"),
            ..entry
        };
        assert_eq!(probe(&empty, &[other]).await, Probe::Unidentified);
        daemon.abort();

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn probe_reports_a_stale_socket_as_not_live() {
        let dir = temp_dir("probe-stale");
        let sock = dir.join("2222222222222222.sock");
        stale_socket(&sock);
        assert!(sock.exists());
        assert_eq!(probe(&sock, &[]).await, Probe::NotLive);
        assert_eq!(probe(&dir.join("missing.sock"), &[]).await, Probe::NotLive);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
