//! Local broker transport and per-client coalesced writers.
use super::protocol::{self, ClientMessage, ResourceState, ServerMessage, WorkspaceState};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{RwLock, oneshot, watch},
    task::JoinSet,
};

pub const WRITE_DEADLINE: Duration = Duration::from_secs(3);
const MAX_REPO_ID: usize = 255;

pub struct EndpointGuard {
    path: Option<PathBuf>,
    identity: Option<(u64, u64)>,
}
impl EndpointGuard {
    pub fn capture(path: &Path) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            path: Some(path.to_path_buf()),
            identity: std::fs::symlink_metadata(path)
                .ok()
                .map(|meta| (meta.dev(), meta.ino())),
        }
    }
}
impl Drop for EndpointGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if let (Some(path), Some((dev, ino))) = (&self.path, self.identity)
            && let Ok(meta) = std::fs::symlink_metadata(path)
            && meta.dev() == dev
            && meta.ino() == ino
            && meta.file_type().is_socket()
            && meta.uid() == unsafe { libc::getuid() }
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct AbortOnDrop(Option<JoinHandle<()>>);
impl AbortOnDrop {
    async fn stop(mut self) {
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

#[derive(Clone)]
pub struct StateHub {
    inventory: watch::Sender<WorkspaceState>,
    resources: Arc<RwLock<HashMap<String, watch::Sender<ResourceState>>>>,
}

impl StateHub {
    pub fn new(initial: WorkspaceState) -> Self {
        let (inventory, _) = watch::channel(initial);
        Self {
            inventory,
            resources: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    /// Replaces the complete inventory and coalesces pending client updates.
    pub fn publish(&self, mut state: WorkspaceState) {
        state.revision = self.inventory.borrow().revision.wrapping_add(1);
        self.inventory.send_replace(state);
    }
    pub fn subscribe_inventory(&self) -> watch::Receiver<WorkspaceState> {
        self.inventory.subscribe()
    }
    pub fn has_clients(&self) -> bool {
        self.inventory.receiver_count() > 0
    }

    pub async fn publish_resource(&self, repo_id: String, mut state: ResourceState) -> bool {
        let (state_repo, generation) = match &state {
            ResourceState::Loading {
                repo_id,
                generation,
                ..
            }
            | ResourceState::Unavailable {
                repo_id,
                generation,
                ..
            }
            | ResourceState::Disconnected {
                repo_id,
                generation,
                ..
            }
            | ResourceState::Ready {
                repo_id,
                generation,
                ..
            } => (repo_id, *generation),
        };
        if state_repo != &repo_id {
            return false;
        }
        let mut resources = self.resources.write().await;
        let sender = resources.entry(repo_id.clone()).or_insert_with(|| {
            let (sender, _) = watch::channel(ResourceState::Loading {
                repo_id: repo_id.clone(),
                generation: 0,
                snapshot: None,
            });
            sender
        });
        let previous = sender.borrow().clone();
        if resource_generation(&previous) > generation {
            return false;
        }
        if let Some(stale) = stale_resource_snapshot(&previous) {
            match &mut state {
                ResourceState::Loading { snapshot, .. }
                | ResourceState::Unavailable { snapshot, .. }
                | ResourceState::Disconnected { snapshot, .. }
                    if snapshot.is_none() =>
                {
                    *snapshot = Some(stale);
                }
                _ => {}
            }
        }
        sender.send_replace(state);
        true
    }

    async fn subscribe_resource(&self, repo_id: &str) -> Option<watch::Receiver<ResourceState>> {
        let repo = self
            .inventory
            .borrow()
            .repositories
            .iter()
            .find(|repo| repo.id == repo_id && repo.host.is_none())
            .cloned()?;
        let mut resources = self.resources.write().await;
        Some(
            resources
                .entry(repo_id.to_owned())
                .or_insert_with(|| {
                    let initial = match repo.connection {
                        super::Connection::Disconnected => ResourceState::Disconnected {
                            repo_id: repo_id.to_owned(),
                            generation: 0,
                            snapshot: None,
                        },
                        super::Connection::Stopped | super::Connection::Unidentified => {
                            ResourceState::Unavailable {
                                repo_id: repo_id.to_owned(),
                                generation: 0,
                                snapshot: None,
                            }
                        }
                        super::Connection::Loading | super::Connection::Live => {
                            ResourceState::Loading {
                                repo_id: repo_id.to_owned(),
                                generation: 0,
                                snapshot: None,
                            }
                        }
                    };
                    let (sender, _) = watch::channel(initial);
                    sender
                })
                .subscribe(),
        )
    }
}

fn resource_generation(state: &ResourceState) -> u64 {
    match state {
        ResourceState::Loading { generation, .. }
        | ResourceState::Unavailable { generation, .. }
        | ResourceState::Disconnected { generation, .. }
        | ResourceState::Ready { generation, .. } => *generation,
    }
}

fn stale_resource_snapshot(state: &ResourceState) -> Option<crate::daemon::resources::Snapshot> {
    match state {
        ResourceState::Ready { snapshot, .. } => Some(snapshot.clone()),
        ResourceState::Loading { snapshot, .. }
        | ResourceState::Unavailable { snapshot, .. }
        | ResourceState::Disconnected { snapshot, .. } => snapshot.clone(),
    }
}

/// Bind only after the caller owns the aggregator singleton lock. Existing
/// endpoints can then only be stale; never unlink an endpoint on a lock miss.
pub fn bind_owned(runtime_dir: &Path) -> Result<UnixListener> {
    use std::os::unix::fs::MetadataExt;
    let dir = std::fs::symlink_metadata(runtime_dir)
        .with_context(|| format!("inspect runtime directory {}", runtime_dir.display()))?;
    if !dir.is_dir() || dir.uid() != unsafe { libc::getuid() } || dir.mode() & 0o077 != 0 {
        bail!(
            "unsafe workspace runtime directory {}",
            runtime_dir.display()
        );
    }
    let path = protocol::socket_path(runtime_dir);
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::getuid() } {
            bail!(
                "refusing to replace unsafe workspace endpoint {}",
                path.display()
            );
        }
        std::fs::remove_file(&path).with_context(|| format!("remove stale {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serve the singleton endpoint. The receiver is the cancellation boundary;
/// all client connection tasks are owned and joined before returning.
pub async fn serve(
    listener: UnixListener,
    hub: StateHub,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    serve_with_limits(listener, hub, shutdown, WRITE_DEADLINE, None).await
}

async fn serve_with_limits(
    listener: UnixListener,
    hub: StateHub,
    mut shutdown: watch::Receiver<bool>,
    write_deadline: Duration,
    send_buffer: Option<libc::c_int>,
) -> Result<()> {
    let endpoint = listener
        .local_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(Path::to_path_buf));
    let _endpoint_guard = endpoint.as_ref().map(|path| EndpointGuard::capture(path));
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } }
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept workspace client")?;
                if let Some(size) = send_buffer {
                    use std::os::fd::AsRawFd;
                    // SAFETY: stream owns a live descriptor and `size` is a c_int.
                    let _ = unsafe { libc::setsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF, &size as *const _ as *const libc::c_void, std::mem::size_of_val(&size) as libc::socklen_t) };
                }
                let hub = hub.clone();
                clients.spawn(async move { let _ = serve_client(stream, hub, write_deadline).await; });
            }
            joined = clients.join_next(), if !clients.is_empty() => { let _ = joined; }
        }
    }
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    drop(listener);
    Ok(())
}

async fn serve_client(stream: UnixStream, hub: StateHub, write_deadline: Duration) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let first_message = tokio::time::timeout(Duration::from_secs(2), protocol::read(&mut reader))
        .await
        .context("workspace subscribe timed out")??;
    let Some(ClientMessage::Subscribe {
        version,
        selected_repo,
        include_remote,
    }) = first_message
    else {
        bail!("workspace client did not subscribe");
    };
    if version != protocol::VERSION {
        tokio::time::timeout(
            WRITE_DEADLINE,
            protocol::write(
                &mut writer,
                &ServerMessage::Error {
                    message: format!("unsupported protocol version {version}"),
                },
            ),
        )
        .await
        .context("workspace version response timed out")??;
        return Ok(());
    }
    validate_repo(selected_repo.as_deref())?;
    let mut revisions = hub.inventory.subscribe();
    let (select_tx, mut selections) = watch::channel(selected_repo.clone());
    let resource_hub = hub.clone();
    let (writer_done_tx, mut writer_done_rx) = oneshot::channel();
    let writer_task = AbortOnDrop(Some(tokio::spawn(async move {
        let mut resource_id = selected_repo.clone();
        let mut selected_resource = if let Some(id) = resource_id.as_deref() {
            Some(resource_hub.subscribe_resource(id).await)
        } else {
            None
        };
        let mut last_revision = 0_u64;
        loop {
            let mut state = revisions.borrow_and_update().clone();
            let selected = selections.borrow_and_update().clone();
            if !include_remote {
                state.repositories.retain(|repo| repo.host.is_none());
                state.hosts = None;
            }
            let selected_present = selected.as_ref().is_some_and(|id| {
                state
                    .repositories
                    .iter()
                    .any(|repo| &repo.id == id && repo.host.is_none())
            });
            if selected != resource_id || selected_resource.as_ref().is_some_and(Option::is_none) {
                resource_id = selected.clone();
                selected_resource = if selected_present {
                    let id = resource_id.as_deref().expect("selected repository exists");
                    Some(resource_hub.subscribe_resource(id).await)
                } else if resource_id.is_some() {
                    Some(None)
                } else {
                    None
                };
            }
            if !selected_present && selected.is_some() {
                selected_resource = Some(None);
            }
            state.selected_repo = selected.clone();
            state.resource = if let Some(Some(resource)) = &selected_resource {
                resource.borrow().clone()
            } else if let Some(id) = selected.as_ref() {
                if selected_present {
                    ResourceState::Loading {
                        repo_id: id.clone(),
                        generation: 0,
                        snapshot: None,
                    }
                } else {
                    ResourceState::Unavailable {
                        repo_id: id.clone(),
                        generation: 0,
                        snapshot: None,
                    }
                }
            } else {
                // Selection-free clients do not receive unrelated resource data.
                state.resource = ResourceState::Loading {
                    repo_id: String::new(),
                    generation: 0,
                    snapshot: None,
                };
                state.resource
            };
            last_revision = last_revision.wrapping_add(1).max(state.revision);
            state.revision = last_revision;
            if !matches!(
                tokio::time::timeout(
                    write_deadline,
                    protocol::write(&mut writer, &ServerMessage::State(state))
                )
                .await,
                Ok(Ok(()))
            ) {
                break;
            }
            tokio::select! {
                changed = revisions.changed() => if changed.is_err() { break; },
                changed = selections.changed() => if changed.is_err() { break; },
                changed = async { if let Some(Some(resource)) = &mut selected_resource { resource.changed().await } else { std::future::pending().await } } => if changed.is_err() { break; },
            }
        }
        let _ = writer_done_tx.send(());
    })));
    let mut client_error = None;
    loop {
        let incoming = tokio::select! {
            result = protocol::read::<ClientMessage, _>(&mut reader) => result,
            _ = &mut writer_done_rx => break,
        };
        match incoming {
            Err(error) => {
                client_error = Some(error);
                break;
            }
            Ok(Some(ClientMessage::SelectRepo { repo_id })) => {
                if let Err(error) = validate_repo(repo_id.as_deref()) {
                    client_error = Some(error);
                    break;
                }
                let _ = select_tx.send(repo_id);
            }
            Ok(Some(ClientMessage::Subscribe { .. })) => {
                client_error = Some(anyhow::anyhow!("duplicate workspace subscribe"));
                break;
            }
            Ok(None) => break,
        }
    }
    writer_task.stop().await;
    if let Some(error) = client_error {
        return Err(error);
    }
    Ok(())
}

fn validate_repo(id: Option<&str>) -> Result<()> {
    if let Some(id) = id
        && (id.is_empty() || id.len() > MAX_REPO_ID || id.chars().any(char::is_control))
    {
        bail!("invalid workspace repository identity");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::protocol::ResourceState;
    use tokio::net::UnixStream;

    fn state(revision: u64) -> WorkspaceState {
        WorkspaceState {
            version: protocol::VERSION,
            revision,
            repositories: Vec::new(),
            hosts: None,
            selected_repo: None,
            resource: ResourceState::Loading {
                repo_id: String::new(),
                generation: 0,
                snapshot: None,
            },
        }
    }

    #[tokio::test]
    async fn legacy_clients_receive_only_local_repositories() {
        let mut initial = state(0);
        initial.hosts = Some(vec![]);
        for (id, host) in [("local", None), ("ssh:dev:local", Some("dev".into()))] {
            initial.repositories.push(crate::workspace::Repository {
                id: id.into(),
                label: id.into(),
                host,
                identity: None,
                socket: None,
                connection: super::super::Connection::Stopped,
                snapshot: None,
                prs: None,
            });
        }
        let hub = StateHub::new(initial);
        for include_remote in [false, true] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let serving = tokio::spawn(serve_client(server, hub.clone(), WRITE_DEADLINE));
            if include_remote {
                protocol::write(
                    &mut client,
                    &ClientMessage::Subscribe {
                        version: protocol::VERSION,
                        selected_repo: None,
                        include_remote: true,
                    },
                )
                .await
                .unwrap();
            } else {
                // The old wire format has no capability field.
                protocol::write(
                    &mut client,
                    &serde_json::json!({
                        "type": "subscribe", "version": protocol::VERSION, "selected_repo": null
                    }),
                )
                .await
                .unwrap();
            }
            let Some(ServerMessage::State(received)) = protocol::read(&mut client).await.unwrap()
            else {
                panic!("missing workspace state");
            };
            assert_eq!(received.hosts.is_some(), include_remote);
            assert_eq!(
                received.repositories.len(),
                if include_remote { 2 } else { 1 }
            );
            assert_eq!(received.repositories[0].id, "local");
            drop(client);
            serving.await.unwrap().unwrap();
        }
    }

    #[test]
    fn pending_inventory_is_coalesced_to_the_latest_revision() {
        let hub = StateHub::new(state(1));
        let receiver = hub.inventory.subscribe();
        hub.publish(state(2));
        hub.publish(state(3));
        assert_eq!(receiver.borrow().revision, 3);
    }

    #[test]
    fn invalid_repo_interests_are_rejected() {
        assert!(validate_repo(Some("valid-id")).is_ok());
        assert!(validate_repo(Some("bad\nidentity")).is_err());
        assert!(validate_repo(Some(&"x".repeat(MAX_REPO_ID + 1))).is_err());
        assert!(validate_repo(Some("")).is_err());
    }

    #[tokio::test]
    async fn endpoint_replaces_only_owned_stale_sockets_and_is_private() {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let dir = crate::discover::test_support::temp_dir("broker-endpoint");
        let path = protocol::socket_path(&dir);
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        let listener = bind_owned(&dir).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.mode() & 0o777, 0o600);
        drop(listener);
        let _ = std::fs::remove_file(&path);

        std::fs::write(&path, "keep this file").unwrap();
        assert!(bind_owned(&dir).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep this file");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn resource_cache_rejects_wrong_repo_and_old_generation() {
        let hub = StateHub::new(state(0));
        assert!(
            hub.publish_resource(
                "alpha".into(),
                ResourceState::Loading {
                    repo_id: "alpha".into(),
                    generation: 3,
                    snapshot: None
                }
            )
            .await
        );
        assert!(
            !hub.publish_resource(
                "alpha".into(),
                ResourceState::Ready {
                    repo_id: "beta".into(),
                    generation: 4,
                    snapshot: crate::daemon::resources::Snapshot::default()
                }
            )
            .await
        );
        assert!(
            hub.publish_resource(
                "alpha".into(),
                ResourceState::Ready {
                    repo_id: "alpha".into(),
                    generation: 3,
                    snapshot: crate::daemon::resources::Snapshot {
                        total_cpu: 4.0,
                        ..Default::default()
                    },
                }
            )
            .await
        );
        assert!(
            hub.publish_resource(
                "alpha".into(),
                ResourceState::Disconnected {
                    repo_id: "alpha".into(),
                    generation: 3,
                    snapshot: None
                }
            )
            .await
        );
        assert!(
            !hub.publish_resource(
                "alpha".into(),
                ResourceState::Disconnected {
                    repo_id: "alpha".into(),
                    generation: 2,
                    snapshot: None
                }
            )
            .await
        );
        let resource = hub
            .resources
            .read()
            .await
            .get("alpha")
            .unwrap()
            .borrow()
            .clone();
        assert!(
            matches!(resource, ResourceState::Disconnected { generation: 3, snapshot: Some(snapshot), .. } if snapshot.total_cpu == 4.0)
        );
    }

    #[tokio::test]
    async fn multiple_clients_keep_independent_resource_interests() {
        let path =
            std::env::temp_dir().join(format!("swamp-broker-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let mut initial = state(0);
        for (id, connection) in [
            ("alpha", crate::workspace::Connection::Loading),
            ("beta", crate::workspace::Connection::Loading),
            ("cold", crate::workspace::Connection::Stopped),
        ] {
            initial.repositories.push(crate::workspace::Repository {
                id: id.into(),
                label: id.into(),
                host: None,
                identity: None,
                socket: None,
                connection,
                snapshot: None,
                prs: None,
            });
        }
        let hub = StateHub::new(initial);
        for (id, generation) in [("alpha", 11), ("beta", 22)] {
            hub.publish_resource(
                id.into(),
                ResourceState::Ready {
                    repo_id: id.into(),
                    generation,
                    snapshot: crate::daemon::resources::Snapshot::default(),
                },
            )
            .await;
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(serve(listener, hub.clone(), shutdown_rx));
        let mut alpha = UnixStream::connect(&path).await.unwrap();
        let mut beta = UnixStream::connect(&path).await.unwrap();
        let mut cold = UnixStream::connect(&path).await.unwrap();
        for (stream, id) in [(&mut alpha, "alpha"), (&mut beta, "beta")] {
            protocol::write(
                stream,
                &ClientMessage::Subscribe {
                    version: protocol::VERSION,
                    selected_repo: Some(id.into()),
                    include_remote: false,
                },
            )
            .await
            .unwrap();
        }
        protocol::write(
            &mut cold,
            &ClientMessage::Subscribe {
                version: protocol::VERSION,
                selected_repo: Some("cold".into()),
                include_remote: false,
            },
        )
        .await
        .unwrap();
        let alpha_state: ServerMessage =
            tokio::time::timeout(WRITE_DEADLINE, protocol::read(&mut alpha))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        let beta_state: ServerMessage =
            tokio::time::timeout(WRITE_DEADLINE, protocol::read(&mut beta))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(
            matches!(alpha_state, ServerMessage::State(WorkspaceState { resource: ResourceState::Ready { repo_id, generation: 11, .. }, .. }) if repo_id == "alpha")
        );
        assert!(
            matches!(beta_state, ServerMessage::State(WorkspaceState { resource: ResourceState::Ready { repo_id, generation: 22, .. }, .. }) if repo_id == "beta")
        );
        let cold_state: ServerMessage =
            tokio::time::timeout(WRITE_DEADLINE, protocol::read(&mut cold))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(
            matches!(cold_state, ServerMessage::State(WorkspaceState { resource: ResourceState::Unavailable { repo_id, .. }, .. }) if repo_id == "cold")
        );

        hub.publish_resource(
            "cold".into(),
            ResourceState::Loading {
                repo_id: "cold".into(),
                generation: 1,
                snapshot: None,
            },
        )
        .await;
        let mut current = hub.inventory.borrow().clone();
        current
            .repositories
            .iter_mut()
            .find(|repo| repo.id == "cold")
            .unwrap()
            .connection = crate::workspace::Connection::Loading;
        hub.publish(current);
        let cold_live: ServerMessage =
            tokio::time::timeout(WRITE_DEADLINE, protocol::read(&mut cold))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(
            matches!(cold_live, ServerMessage::State(WorkspaceState { resource: ResourceState::Loading { repo_id, generation: 1, .. }, .. }) if repo_id == "cold")
        );
        let _: ServerMessage = protocol::read(&mut alpha).await.unwrap().unwrap();
        let _: ServerMessage = protocol::read(&mut beta).await.unwrap().unwrap();

        for generation in 12..50 {
            hub.publish_resource(
                "alpha".into(),
                ResourceState::Ready {
                    repo_id: "alpha".into(),
                    generation,
                    snapshot: crate::daemon::resources::Snapshot::default(),
                },
            )
            .await;
        }
        let mut final_generation = 0;
        while final_generation < 49 {
            let update: ServerMessage =
                tokio::time::timeout(WRITE_DEADLINE, protocol::read(&mut alpha))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            if let ServerMessage::State(WorkspaceState {
                resource:
                    ResourceState::Ready {
                        repo_id,
                        generation,
                        ..
                    },
                ..
            }) = update
            {
                assert_eq!(repo_id, "alpha");
                final_generation = generation;
            }
        }
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                protocol::read::<ServerMessage, _>(&mut beta)
            )
            .await
            .is_err(),
            "alpha resource ticks do not wake beta"
        );

        protocol::write(
            &mut alpha,
            &ClientMessage::SelectRepo {
                repo_id: Some("beta".into()),
            },
        )
        .await
        .unwrap();
        let changed: ServerMessage =
            tokio::time::timeout(WRITE_DEADLINE, protocol::read(&mut alpha))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(
            matches!(changed, ServerMessage::State(WorkspaceState { selected_repo: Some(repo_id), resource: ResourceState::Ready { generation: 22, .. }, .. }) if repo_id == "beta")
        );
        drop((alpha, beta, cold));
        shutdown_tx.send_replace(true);
        server.await.unwrap().unwrap();
        assert!(
            !path.exists(),
            "broker removes only its own endpoint on shutdown"
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn stalled_client_writer_does_not_block_a_responsive_client() {
        let path =
            std::env::temp_dir().join(format!("swamp-broker-slow-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let mut large = state(0);
        large.repositories.push(crate::workspace::Repository {
            id: "large".into(),
            label: "x".repeat(15 * 1024 * 1024),
            host: None,
            identity: None,
            socket: None,
            connection: crate::workspace::Connection::Loading,
            snapshot: None,
            prs: None,
        });
        let hub = StateHub::new(large);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let server = tokio::spawn(serve_with_limits(
            listener,
            hub.clone(),
            shutdown_rx,
            WRITE_DEADLINE,
            Some(1024),
        ));
        let mut stalled = UnixStream::connect(&path).await.unwrap();
        use std::os::fd::AsRawFd;
        let receive_buffer: libc::c_int = 1024;
        // SAFETY: this uses the valid socket descriptor and a correctly sized
        // integer option value to keep the test peer's receive buffer tiny.
        let result = unsafe {
            libc::setsockopt(
                stalled.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &receive_buffer as *const _ as *const libc::c_void,
                std::mem::size_of_val(&receive_buffer) as libc::socklen_t,
            )
        };
        assert_eq!(result, 0);
        protocol::write(
            &mut stalled,
            &ClientMessage::Subscribe {
                version: protocol::VERSION,
                selected_repo: None,
                include_remote: false,
            },
        )
        .await
        .unwrap();
        // Keep this peer's receive buffer full while another pane connects.
        tokio::time::sleep(Duration::from_millis(30)).await;
        let mut responsive = UnixStream::connect(&path).await.unwrap();
        protocol::write(
            &mut responsive,
            &ClientMessage::Subscribe {
                version: protocol::VERSION,
                selected_repo: None,
                include_remote: false,
            },
        )
        .await
        .unwrap();
        let response: ServerMessage =
            tokio::time::timeout(Duration::from_secs(2), protocol::read(&mut responsive))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(
            matches!(response, ServerMessage::State(WorkspaceState { repositories, .. }) if repositories[0].label.len() == 15 * 1024 * 1024)
        );
        hub.publish(state(1));
        let latest: ServerMessage =
            tokio::time::timeout(Duration::from_secs(2), protocol::read(&mut responsive))
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert!(matches!(
            latest,
            ServerMessage::State(WorkspaceState { revision, repositories, .. }) if revision >= 2 && repositories.is_empty()
        ));
        tokio::time::sleep(WRITE_DEADLINE + Duration::from_millis(100)).await;
        let stalled_result = tokio::time::timeout(
            WRITE_DEADLINE + Duration::from_secs(1),
            protocol::read::<ServerMessage, _>(&mut stalled),
        )
        .await
        .expect("stalled writer deadline completes");
        assert!(
            matches!(stalled_result, Err(_) | Ok(None)),
            "writer deadline closes only the stalled connection"
        );
        drop(responsive);
        shutdown_tx.send_replace(true);
        server.await.unwrap().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn large_workspace_inventory_stays_compact_for_multiple_panes() {
        use crate::{
            daemon::state::Snapshot,
            discover::{RepoIdentity, test_support::row},
            fanin::Event,
        };
        use std::path::Path;
        let mut model = super::super::Workspace::default();
        for repo in 0..20 {
            let id = format!("repo-{repo:02}");
            let root = format!("/repos/project-{repo:02}");
            model.apply(Event::Up {
                id: id.clone(),
                sock: format!("/runtime/{id}.sock").into(),
                identity: Some(RepoIdentity {
                    common_dir: format!("{root}/.bare").into(),
                    session_name: format!("{id}-session"),
                    version: Some("test".into()),
                }),
            });
            let rows = (0..10)
                .map(|worktree| {
                    let name = format!("worktree-{worktree:02}");
                    row(&name, &Path::new(&root).join(&name))
                })
                .collect();
            model.apply(Event::Snapshot {
                id,
                snapshot: Snapshot {
                    rows,
                    default_known: true,
                },
            });
        }
        let repositories = model.repositories();
        assert_eq!(repositories.len(), 20);
        assert_eq!(
            repositories
                .iter()
                .map(|repo| repo.destinations().len())
                .sum::<usize>(),
            200
        );

        let hub = StateHub::new(state(0));
        let clients: Vec<_> = (0..4).map(|_| hub.inventory.subscribe()).collect();
        for revision in 1..=10 {
            hub.publish(WorkspaceState {
                version: protocol::VERSION,
                revision,
                repositories: repositories.clone(),
                hosts: None,
                selected_repo: None,
                resource: ResourceState::Loading {
                    repo_id: String::new(),
                    generation: 0,
                    snapshot: None,
                },
            });
        }
        let encoded = serde_json::to_vec(&hub.inventory.borrow().clone()).unwrap();
        assert!(encoded.len() < protocol::MAX_FRAME_LEN);
        assert!(clients.iter().all(|client| client.borrow().revision == 10));
    }
}
