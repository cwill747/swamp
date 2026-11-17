//! Asynchronous workspace broker client. Connection and retry work never runs
//! on the TUI render/input path.
use super::protocol::{self, ClientMessage, ServerMessage, WorkspaceState};
use anyhow::Result;
use std::{path::PathBuf, time::Duration};
use tokio::{
    net::UnixStream,
    sync::{mpsc, watch},
    task::JoinHandle,
};

const AGGREGATOR_RESTART_BACKOFF: Duration = Duration::from_secs(5);

fn restart_aggregator_if_due(last_attempt: &mut tokio::time::Instant, restart: &mut impl FnMut()) {
    if last_attempt.elapsed() >= AGGREGATOR_RESTART_BACKOFF {
        restart();
        *last_attempt = tokio::time::Instant::now();
    }
}

#[derive(Debug, Clone)]
pub enum ClientStatus {
    Connecting,
    Connected,
    Unavailable(String),
}

pub struct WorkspaceClient {
    state: watch::Receiver<Option<WorkspaceState>>,
    status: watch::Receiver<ClientStatus>,
    select_tx: watch::Sender<Option<String>>,
    cancel_tx: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

struct AbortOnDrop(Option<JoinHandle<()>>);
impl AbortOnDrop {
    async fn abort_and_join(mut self) {
        if let Some(mut task) = self.0.take() {
            task.abort();
            let _ = (&mut task).await;
        }
    }
}
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

impl WorkspaceClient {
    pub fn start(runtime_dir: PathBuf, selected_repo: Option<String>) -> Self {
        Self::start_with_aggregator(runtime_dir, selected_repo, crate::aggregate::spawn_detached)
    }

    fn start_with_aggregator(
        runtime_dir: PathBuf,
        selected_repo: Option<String>,
        mut restart_aggregator: impl FnMut() + Send + 'static,
    ) -> Self {
        let (state_tx, state) = watch::channel(None);
        let (status_tx, status) = watch::channel(ClientStatus::Connecting);
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let (select_tx, mut select_rx) = watch::channel(selected_repo.clone());
        let task = tokio::spawn(async move {
            let mut selected = selected_repo;
            let mut delay = Duration::from_millis(100);
            let mut last_aggregator_restart =
                tokio::time::Instant::now() - AGGREGATOR_RESTART_BACKOFF;
            loop {
                let path = protocol::socket_path(&runtime_dir);
                let connected = tokio::select! {
                    changed = cancel_rx.changed() => { if changed.is_err() || *cancel_rx.borrow() { return; } continue; }
                    result = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(&path)) => result,
                };
                let stream = match connected {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(e)) => {
                        restart_aggregator_if_due(
                            &mut last_aggregator_restart,
                            &mut restart_aggregator,
                        );
                        let _ = status_tx.send(ClientStatus::Unavailable(format!(
                            "{}: {e}",
                            path.display()
                        )));
                        tokio::select! { _ = tokio::time::sleep(delay) => {}, changed = select_rx.changed() => if changed.is_ok() { selected = select_rx.borrow_and_update().clone(); }, canceled = cancel_rx.changed() => if canceled.is_err() || *cancel_rx.borrow() { return; } }
                        delay = (delay * 2).min(Duration::from_secs(2));
                        continue;
                    }
                    Err(_) => {
                        restart_aggregator_if_due(
                            &mut last_aggregator_restart,
                            &mut restart_aggregator,
                        );
                        let _ = status_tx.send(ClientStatus::Unavailable(
                            "workspace broker connection timed out".into(),
                        ));
                        tokio::select! { _ = tokio::time::sleep(delay) => {}, changed = cancel_rx.changed() => if changed.is_err() || *cancel_rx.borrow() { return; } }
                        delay = (delay * 2).min(Duration::from_secs(2));
                        continue;
                    }
                };
                delay = Duration::from_millis(100);
                let (mut reader, mut writer) = stream.into_split();
                let subscribe = tokio::time::timeout(
                    Duration::from_secs(2),
                    protocol::write(
                        &mut writer,
                        &ClientMessage::Subscribe {
                            version: protocol::VERSION,
                            selected_repo: selected.clone(),
                            include_remote: true,
                        },
                    ),
                )
                .await;
                if !matches!(subscribe, Ok(Ok(()))) {
                    restart_aggregator_if_due(
                        &mut last_aggregator_restart,
                        &mut restart_aggregator,
                    );
                    tokio::select! { _ = tokio::time::sleep(delay) => {}, changed = cancel_rx.changed() => if changed.is_err() || *cancel_rx.borrow() { return; } }
                    continue;
                }
                let (incoming_tx, mut incoming_rx) = mpsc::channel(1);
                let reader_task = AbortOnDrop(Some(tokio::spawn(async move {
                    loop {
                        let message = protocol::read::<ServerMessage, _>(&mut reader).await;
                        if incoming_tx.send(message).await.is_err() {
                            break;
                        }
                    }
                })));
                let mut received_state = false;
                let handshake_timeout = tokio::time::sleep(Duration::from_secs(2));
                tokio::pin!(handshake_timeout);
                loop {
                    tokio::select! {
                        changed = cancel_rx.changed() => { if changed.is_err() || *cancel_rx.borrow() { break; } }
                        incoming = incoming_rx.recv() => match incoming {
                            Some(Ok(Some(ServerMessage::State(s)))) if s.version == protocol::VERSION => {
                                received_state = true;
                                let _ = status_tx.send(ClientStatus::Connected);
                                let _ = state_tx.send(Some(s));
                            }
                            Some(Ok(Some(ServerMessage::Error { message }))) => { let _ = status_tx.send(ClientStatus::Unavailable(message)); break; }
                            Some(Ok(Some(ServerMessage::State(_)))) => { let _ = status_tx.send(ClientStatus::Unavailable("workspace broker protocol version mismatch".into())); break; }
                            _ => { let _ = status_tx.send(ClientStatus::Connecting); break; }
                        },
                        _ = &mut handshake_timeout, if !received_state => {
                            let _ = status_tx.send(ClientStatus::Unavailable("workspace broker did not provide a compatible state".into()));
                            break;
                        }
                        changed = select_rx.changed() => {
                            if changed.is_err() { break; }
                            selected = select_rx.borrow_and_update().clone();
                            if !matches!(tokio::time::timeout(Duration::from_secs(2), protocol::write(&mut writer, &ClientMessage::SelectRepo { repo_id: selected.clone() })).await, Ok(Ok(()))) { break; }
                        }
                    }
                }
                reader_task.abort_and_join().await;
                if *cancel_rx.borrow() {
                    break;
                }
                restart_aggregator_if_due(&mut last_aggregator_restart, &mut restart_aggregator);
                tokio::select! { _ = tokio::time::sleep(delay) => {}, changed = select_rx.changed() => if changed.is_ok() { selected = select_rx.borrow_and_update().clone(); }, canceled = cancel_rx.changed() => if canceled.is_err() || *cancel_rx.borrow() { break; } }
                delay = (delay * 2).min(Duration::from_secs(2));
            }
        });
        Self {
            state,
            status,
            select_tx,
            cancel_tx,
            task: Some(task),
        }
    }

    pub fn state(&self) -> watch::Receiver<Option<WorkspaceState>> {
        self.state.clone()
    }
    pub fn status(&self) -> watch::Receiver<ClientStatus> {
        self.status.clone()
    }
    pub async fn select_repo(&self, repo_id: Option<String>) -> Result<()> {
        self.select_tx.send_replace(repo_id);
        Ok(())
    }
    pub async fn close(mut self) {
        self.cancel_tx.send_replace(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for WorkspaceClient {
    fn drop(&mut self) {
        self.cancel_tx.send_replace(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn close_joins_reader_and_releases_broker_connection() {
        let runtime =
            std::env::temp_dir().join(format!("swamp-client-test-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let path = protocol::socket_path(&runtime);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let subscribe: Option<ClientMessage> = protocol::read(&mut stream).await.unwrap();
            assert!(matches!(subscribe, Some(ClientMessage::Subscribe { .. })));
            let _ = connected_tx.send(());
            let closed: Option<ClientMessage> = protocol::read(&mut stream).await.unwrap();
            assert!(closed.is_none());
        });
        let client = WorkspaceClient::start(runtime.clone(), None);
        tokio::time::timeout(Duration::from_secs(3), connected_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), client.close())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .unwrap()
            .unwrap();
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(runtime);
    }

    #[tokio::test]
    async fn silent_broker_does_not_look_like_an_empty_connected_workspace() {
        let runtime =
            std::env::temp_dir().join(format!("swamp-client-silent-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let path = protocol::socket_path(&runtime);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: Option<ClientMessage> = protocol::read(&mut stream).await.unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
        });
        let client = WorkspaceClient::start(runtime.clone(), None);
        let mut status = client.status();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(&*status.borrow(), ClientStatus::Unavailable(message) if message.contains("did not provide")) {
                    break;
                }
                status.changed().await.unwrap();
            }
        }).await.unwrap();
        assert!(client.state().borrow().is_none());
        client.close().await;
        peer.abort();
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(runtime);
    }

    fn empty_state(revision: u64) -> WorkspaceState {
        WorkspaceState {
            version: protocol::VERSION,
            revision,
            repositories: Vec::new(),
            hosts: None,
            selected_repo: None,
            resource: super::super::protocol::ResourceState::Loading {
                repo_id: String::new(),
                generation: 0,
                snapshot: None,
            },
        }
    }

    #[tokio::test]
    async fn client_reconnects_and_receives_state_after_broker_restart() {
        let runtime =
            std::env::temp_dir().join(format!("swamp-client-restart-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let path = protocol::socket_path(&runtime);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let peer = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let _: Option<ClientMessage> = protocol::read(&mut first).await.unwrap();
            protocol::write(&mut first, &ServerMessage::State(empty_state(1)))
                .await
                .unwrap();
            drop(first);
            let (mut restarted, _) = listener.accept().await.unwrap();
            let subscribe: Option<ClientMessage> = protocol::read(&mut restarted).await.unwrap();
            assert!(matches!(subscribe, Some(ClientMessage::Subscribe { .. })));
            protocol::write(&mut restarted, &ServerMessage::State(empty_state(2)))
                .await
                .unwrap();
            let _: Option<ClientMessage> = protocol::read(&mut restarted).await.unwrap();
        });
        let restart_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let restart_count_in_task = restart_count.clone();
        let client = WorkspaceClient::start_with_aggregator(runtime.clone(), None, move || {
            restart_count_in_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let mut state = client.state();
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if state
                    .borrow()
                    .as_ref()
                    .is_some_and(|state| state.revision == 2)
                {
                    break;
                }
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(restart_count.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        client.close().await;
        tokio::time::timeout(Duration::from_secs(2), peer)
            .await
            .unwrap()
            .unwrap();
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(runtime);
    }

    #[tokio::test]
    async fn incompatible_broker_protocol_is_reported() {
        let runtime =
            std::env::temp_dir().join(format!("swamp-client-version-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let path = protocol::socket_path(&runtime);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: Option<ClientMessage> = protocol::read(&mut stream).await.unwrap();
            protocol::write(
                &mut stream,
                &ServerMessage::Error {
                    message: "unsupported protocol version 99".into(),
                },
            )
            .await
            .unwrap();
        });
        let client = WorkspaceClient::start(runtime.clone(), None);
        let mut status = client.status();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(&*status.borrow(), ClientStatus::Unavailable(message) if message.contains("version 99")) { break; }
                status.changed().await.unwrap();
            }
        }).await.unwrap();
        client.close().await;
        peer.await.unwrap();
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(runtime);
    }

    #[tokio::test]
    async fn rapid_repo_selection_is_nonblocking_and_coalesces_to_latest() {
        let runtime =
            std::env::temp_dir().join(format!("swamp-client-select-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let path = protocol::socket_path(&runtime);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let (selected_tx, selected_rx) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: Option<ClientMessage> = protocol::read(&mut stream).await.unwrap();
            let selection: Option<ClientMessage> = protocol::read(&mut stream).await.unwrap();
            let _ = selected_tx.send(selection);
            let _: Option<ClientMessage> = protocol::read(&mut stream).await.unwrap();
        });
        let client = WorkspaceClient::start(runtime.clone(), None);
        for index in 0..100 {
            client
                .select_repo(Some(format!("repo-{index}")))
                .await
                .unwrap();
        }
        let selected = tokio::time::timeout(Duration::from_secs(2), selected_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(selected, Some(ClientMessage::SelectRepo { repo_id: Some(id) }) if id == "repo-99")
        );
        client.close().await;
        tokio::time::timeout(Duration::from_secs(2), peer)
            .await
            .unwrap()
            .unwrap();
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(runtime);
    }
}
