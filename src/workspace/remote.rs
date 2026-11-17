//! Read-only workspace discovery over a system SSH subprocess.
use super::{Connection, Repository, Workspace, protocol};
use crate::{config::SshHostConfig, fanin};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

const EXPORT_VERSION: u16 = 1;
const HEARTBEAT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub config: SshHostConfig,
    pub status: HostStatus,
}

impl Host {
    pub fn key(&self) -> String {
        format!("ssh:{}", self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostStatus {
    Connecting,
    Connected,
    Offline(String),
}

impl HostStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Offline(_) => "Offline",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ExportMessage {
    Inventory {
        version: u16,
        repositories: Vec<Repository>,
    },
    Heartbeat,
}

/// Discover only this machine's daemons and registry, even if its own
/// dashboard also has remote hosts configured.
pub async fn export() -> Result<()> {
    let mut stdout = tokio::io::stdout();
    let registry = crate::registry::registry_path().ok();
    let (tx, mut rx) = mpsc::channel(256);
    let scanner = Task(fanin::spawn(
        fanin::Config {
            runtime_dir: crate::util::runtime_base_dir()?,
            registry: registry.clone(),
            scan_interval: fanin::SCAN_INTERVAL,
        },
        tx,
    ));
    let mut model = Workspace::default();
    let mut refresh = tokio::time::interval(HEARTBEAT);
    let mut publish = tokio::time::interval(Duration::from_millis(250));
    publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut dirty = true;
    let mut last_sent = None;
    loop {
        tokio::select! {
            biased;
            _ = refresh.tick() => {
                if let Some(path) = registry.clone() {
                    let entries = tokio::task::spawn_blocking(move || crate::registry::load_at(&path)).await?;
                    dirty |= model.set_registry(entries);
                }
            }
            _ = publish.tick() => {
                let message = if dirty {
                    ExportMessage::Inventory {
                        version: EXPORT_VERSION,
                        repositories: model.repositories(),
                    }
                } else if last_sent.is_some_and(|at: tokio::time::Instant| at.elapsed() >= HEARTBEAT) {
                    ExportMessage::Heartbeat
                } else {
                    continue;
                };
                if let Err(error) = protocol::write(&mut stdout, &message).await {
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
                    {
                        break;
                    }
                    return Err(error);
                }
                dirty = false;
                last_sent = Some(tokio::time::Instant::now());
            }
            event = rx.recv() => match event {
                Some(fanin::Event::Resources { .. }) => {}
                Some(event) => {
                    model.apply(event);
                    dirty = true;
                }
                None => bail!("local inventory scanner stopped"),
            }
        }
    }
    drop(scanner);
    Ok(())
}

struct Task(JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Default)]
pub struct Remotes {
    hosts: BTreeMap<String, Peer>,
    generation: u64,
}

struct Peer {
    host: Host,
    repositories: Vec<Repository>,
    generation: u64,
    _task: Task,
}

pub struct Update {
    id: String,
    generation: u64,
    result: Result<Vec<Repository>, String>,
}

impl Remotes {
    pub fn configure(
        &mut self,
        config: &BTreeMap<String, SshHostConfig>,
        active: bool,
        updates: &mpsc::Sender<Update>,
    ) -> bool {
        let before = self.hosts.len();
        self.hosts
            .retain(|id, peer| active && config.get(id) == Some(&peer.host.config));
        let mut changed = before != self.hosts.len();
        if active {
            for (id, config) in config {
                if self.hosts.contains_key(id) {
                    continue;
                }
                changed = true;
                self.generation = self.generation.wrapping_add(1);
                let host = Host {
                    id: id.clone(),
                    config: config.clone(),
                    status: HostStatus::Connecting,
                };
                let task = tokio::spawn(follow(host.clone(), self.generation, updates.clone()));
                self.hosts.insert(
                    id.clone(),
                    Peer {
                        host,
                        repositories: Vec::new(),
                        generation: self.generation,
                        _task: Task(task),
                    },
                );
            }
        }
        changed
    }

    pub fn apply(&mut self, update: Update) -> bool {
        let Some(peer) = self.hosts.get_mut(&update.id) else {
            return false;
        };
        if peer.generation != update.generation {
            return false;
        }
        match update.result {
            Ok(repos) => {
                peer.host.status = HostStatus::Connected;
                peer.repositories = repos;
            }
            Err(error) => {
                peer.host.status = HostStatus::Offline(error);
                for repo in &mut peer.repositories {
                    repo.connection = Connection::Disconnected;
                }
            }
        }
        true
    }

    pub fn inventory(&self) -> (Vec<Host>, Vec<Repository>) {
        (
            self.hosts.values().map(|peer| peer.host.clone()).collect(),
            self.hosts
                .values()
                .flat_map(|peer| peer.repositories.iter().cloned())
                .collect(),
        )
    }
}

async fn follow(host: Host, generation: u64, updates: mpsc::Sender<Update>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let mut received = false;
        let result = connect(&host, generation, &updates, &mut received).await;
        let Err(error) = result else {
            return;
        };
        if updates
            .send(Update {
                id: host.id.clone(),
                generation,
                result: Err(format!("{error:#}")),
            })
            .await
            .is_err()
        {
            return;
        }
        if received {
            delay = Duration::from_secs(1);
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(READ_TIMEOUT);
    }
}

async fn connect(
    host: &Host,
    generation: u64,
    updates: &mpsc::Sender<Update>,
    received: &mut bool,
) -> Result<()> {
    let command = remote_command(&host.config, &["ssh-inventory"]);
    let mut child = tokio::process::Command::new("ssh")
        .args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=8",
            "-o",
            "ServerAliveInterval=10",
            "-o",
            "ServerAliveCountMax=2",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ForwardAgent=no",
            "--",
            &host.config.target,
            &command,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("start OpenSSH")?;
    let mut reader = child.stdout.take().context("open SSH stdout")?;
    let mut stderr = child.stderr.take().context("open SSH stderr")?;
    let mut errors = JoinSet::new();
    errors.spawn(async move {
        let mut first = Vec::new();
        let mut buf = [0; 1024];
        while let Ok(n @ 1..) = stderr.read(&mut buf).await {
            first.extend_from_slice(&buf[..n.min(2048 - first.len())]);
        }
        String::from_utf8_lossy(&first).trim().to_owned()
    });

    loop {
        let message = tokio::time::timeout(READ_TIMEOUT, protocol::read(&mut reader))
            .await
            .context("SSH inventory stopped responding")?
            .context("invalid SSH inventory (check Swamp version and remote shell stdout)")?;
        match message {
            Some(ExportMessage::Inventory {
                version,
                repositories,
            }) => {
                ensure!(
                    version == EXPORT_VERSION,
                    "unsupported remote inventory version {version}"
                );
                let repositories = qualify(host, repositories)?;
                *received = true;
                if updates
                    .send(Update {
                        id: host.id.clone(),
                        generation,
                        result: Ok(repositories),
                    })
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            Some(ExportMessage::Heartbeat) if *received => {}
            Some(ExportMessage::Heartbeat) => bail!("remote did not send its inventory"),
            None => {
                if let Ok(Some(Ok(error))) =
                    tokio::time::timeout(Duration::from_millis(500), errors.join_next()).await
                    && !error.is_empty()
                {
                    bail!("{error}");
                }
                bail!("SSH inventory connection closed");
            }
        }
    }
}

fn qualify(host: &Host, mut repos: Vec<Repository>) -> Result<Vec<Repository>> {
    let mut ids = BTreeSet::new();
    for repo in &mut repos {
        // Keep repository IDs in the local per-repo daemon namespace on the
        // exporter. Only the receiving broker can assign host identities.
        ensure!(
            repo.host.is_none()
                && repo.id.len() <= 64
                && !repo.id.is_empty()
                && repo
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                && ids.insert(repo.id.clone()),
            "invalid repository identity in SSH inventory"
        );
        repo.id = format!("{}:{}", host.key(), repo.id);
        repo.label = format!("{} [{}]", repo.label, host.id);
        repo.host = Some(host.id.clone());
        repo.socket = None;
    }
    Ok(repos)
}

// OpenSSH passes a command string to the remote shell. Quote each argument
// independently, including paths received from the remote inventory.
fn remote_command(config: &SshHostConfig, args: &[&str]) -> String {
    std::iter::once(config.swamp_path.as_deref().unwrap_or("swamp"))
        .chain(args.iter().copied())
        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn open(host: &Host, common_dir: &Path, label: &str) -> Result<()> {
    let path = common_dir
        .to_str()
        .context("remote repository path is not UTF-8")?;
    ensure!(
        common_dir.is_absolute() && !path.contains('\0'),
        "invalid remote repository path"
    );
    let command = remote_command(&host.config, &["ssh-attach", "--", path]);
    crate::zellij::new_ssh_pane(
        &format!("SSH · {label}"),
        &[
            "ssh",
            "-t",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ClearAllForwardings=yes",
            "--",
            &host.config.target,
            &command,
        ],
    )
}

pub fn attach(common_dir: &Path) -> Result<()> {
    let common_dir = common_dir
        .canonicalize()
        .context("locate remote repository")?;
    ensure!(
        crate::worktree::git_common_dir(&common_dir)?.canonicalize()? == common_dir,
        "expected a Git common directory"
    );
    let session = crate::launch::ensure_background_session(&common_dir)?;
    crate::zellij::attach_and_wait(&session)
}
