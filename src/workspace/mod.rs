//! Shared, side-effect-free workspace inventory. Socket events update cached
//! data; projecting a rail must never query git, Zellij, or the network.

pub mod broker;
pub mod client;
pub mod protocol;
pub mod seen;
pub mod selection;
pub mod ui;

use crate::daemon::state::{AgentStatus, PrSnapshot, Snapshot, SubagentRow, WorktreeRow};
use crate::discover::RepoIdentity;
use crate::fanin::Event;
use crate::registry::RepoEntry;
use crate::util::{repo_id, repo_name_for};
use crate::worktree::SlotState;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Display names and row positions are deliberately absent from identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DestinationKey {
    pub repo: String,
    pub path: PathBuf,
    pub branch: String,
    pub session: Option<String>,
    pub subagent: Option<String>,
}

impl DestinationKey {
    pub fn parent(&self) -> Self {
        Self {
            subagent: None,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Connection {
    Loading,
    Live,
    Disconnected,
    Stopped,
    Unidentified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: String,
    pub label: String,
    pub identity: Option<RepoIdentity>,
    pub socket: Option<PathBuf>,
    pub connection: Connection,
    pub snapshot: Option<Snapshot>,
    pub prs: Option<PrSnapshot>,
}

impl Repository {
    pub fn can_activate(&self) -> bool {
        self.identity.is_some() && matches!(self.connection, Connection::Live | Connection::Stopped)
    }

    pub fn destinations(&self) -> Vec<Destination> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut rows = snapshot.rows.clone();
        rows.sort_by(|a, b| {
            idle_slot(a)
                .cmp(&idle_slot(b))
                .then(b.head_ts.cmp(&a.head_ts))
                .then(a.name.cmp(&b.name))
                .then(a.path.cmp(&b.path))
        });
        rows.into_iter()
            .map(|row| {
                let title = match row.slot_state {
                    Some(SlotState::Free) => "free".into(),
                    Some(SlotState::Pinned) => row.pin.clone().unwrap_or_else(|| "pinned".into()),
                    _ if !row.title.trim().is_empty() => row.title.clone(),
                    _ if row
                        .session_name
                        .as_ref()
                        .is_some_and(|s| !s.trim().is_empty()) =>
                    {
                        row.session_name.clone().unwrap_or_default()
                    }
                    _ if !row.branch.is_empty() => row.branch.clone(),
                    _ => row.name.clone(),
                };
                Destination {
                    key: DestinationKey {
                        repo: self.id.clone(),
                        path: row.path.clone(),
                        branch: row.branch.clone(),
                        session: row.session_name.clone(),
                        subagent: None,
                    },
                    title,
                    row,
                }
            })
            .collect()
    }
}

fn idle_slot(row: &WorktreeRow) -> (bool, u32) {
    let idle = matches!(row.slot_state, Some(SlotState::Free | SlotState::Pinned));
    (
        idle,
        if idle {
            row.name
                .strip_prefix("work")
                .and_then(|s| s.parse().ok())
                .unwrap_or(u32::MAX)
        } else {
            0
        },
    )
}

#[derive(Debug, Clone)]
pub struct Destination {
    pub key: DestinationKey,
    pub title: String,
    /// Keep the complete snapshot row, including trunk, slot, and title source.
    pub row: WorktreeRow,
}
impl Destination {
    pub fn reported_agent(&self) -> bool {
        (self.row.agent_ts > 0 || !self.row.subagents.is_empty())
            && !matches!(
                self.row.slot_state,
                Some(SlotState::Free | SlotState::Pinned)
            )
    }
}

#[derive(Debug, Clone)]
pub struct AgentGroup {
    pub parent: Destination,
    pub children: Vec<(DestinationKey, SubagentRow)>,
}

#[derive(Debug, Default)]
pub struct Workspace {
    peers: BTreeMap<String, Repository>,
    registry: BTreeMap<String, RepoEntry>,
}

impl Workspace {
    pub fn set_registry(&mut self, entries: Vec<RepoEntry>) {
        self.registry = entries
            .into_iter()
            .map(|e| (repo_id(&e.common_dir), e))
            .collect();
        // A registry entry is the durable identity for a launched repo. If
        // its live follower already ended, expose it as Stopped so explicit
        // activation can relaunch it; do not preserve its old rows as live.
        let registry = &self.registry;
        self.peers.retain(|id, peer| {
            peer.connection != Connection::Disconnected || !registry.contains_key(id)
        });
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Up { id, sock, identity } => {
                let was_disconnected = self
                    .peers
                    .get(&id)
                    .is_some_and(|peer| peer.connection == Connection::Disconnected);
                let peer = self.peers.entry(id.clone()).or_insert_with(|| Repository {
                    id,
                    label: String::new(),
                    identity: None,
                    socket: None,
                    connection: Connection::Loading,
                    snapshot: None,
                    prs: None,
                });
                // Retain cached rows and remote status as stale through a
                // reconnect, but never label them as a fresh live snapshot.
                peer.socket = Some(sock);
                peer.identity = identity;
                peer.connection = if peer.identity.is_none() {
                    Connection::Unidentified
                } else if !was_disconnected && peer.snapshot.is_some() {
                    Connection::Live
                } else {
                    Connection::Loading
                };
            }
            Event::Snapshot { id, snapshot } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.snapshot = Some(snapshot);
                    peer.connection = if peer.identity.is_some() {
                        Connection::Live
                    } else {
                        Connection::Unidentified
                    };
                }
            }
            Event::PrStatus { id, prs } => {
                if let Some(peer) = self.peers.get_mut(&id) {
                    peer.prs = Some(prs);
                }
            }
            Event::Down { id } => {
                if self.registry.contains_key(&id) {
                    // The registry will project this repository as Stopped
                    // and activatable, without surfacing cached rows as live.
                    self.peers.remove(&id);
                } else if let Some(peer) = self.peers.get_mut(&id) {
                    peer.connection = Connection::Disconnected;
                }
            }
            Event::Resources { .. } => {}
        }
    }

    /// Identified live peers first, unidentified peers next, then stopped repos.
    /// A disconnected snapshot is retained and labeled stale, not a live empty repo.
    pub fn repositories(&self) -> Vec<Repository> {
        let mut repos: Vec<_> = self.peers.values().cloned().collect();
        for (id, entry) in &self.registry {
            if self.peers.contains_key(id) {
                continue;
            }
            repos.push(Repository {
                id: id.clone(),
                label: String::new(),
                identity: Some(RepoIdentity {
                    common_dir: entry.common_dir.clone(),
                    session_name: entry.session_name.clone(),
                    version: None,
                }),
                socket: None,
                connection: Connection::Stopped,
                snapshot: None,
                prs: None,
            });
        }
        let mut counts = BTreeMap::<String, usize>::new();
        for repo in &repos {
            if let Some(identity) = &repo.identity {
                *counts
                    .entry(repo_name_for(&identity.common_dir))
                    .or_default() += 1;
            }
        }
        for repo in &mut repos {
            repo.label = match &repo.identity {
                Some(identity) => {
                    let name = repo_name_for(&identity.common_dir);
                    if counts[&name] > 1 {
                        let path = identity.common_dir.parent().unwrap_or(&identity.common_dir);
                        format!("{name} ({})", path.display())
                    } else {
                        name
                    }
                }
                None => format!(
                    "Unidentified ({})",
                    repo.socket
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| repo.id.clone())
                ),
            };
        }
        repos.sort_by(|a, b| {
            let tier = |r: &Repository| match r.connection {
                Connection::Stopped => 2,
                _ if r.identity.is_none() => 1,
                _ => 0,
            };
            (tier(a), &a.label, &a.id).cmp(&(tier(b), &b.label, &b.id))
        });
        repos
    }

    pub fn agents(&self) -> Vec<AgentGroup> {
        let mut groups: Vec<_> = self
            .repositories()
            .iter()
            .flat_map(|repo| {
                repo.destinations()
                    .into_iter()
                    .filter(Destination::reported_agent)
                    .map(|parent| {
                        let mut children: Vec<_> = parent
                            .row
                            .subagents
                            .iter()
                            .cloned()
                            .map(|child| {
                                (
                                    DestinationKey {
                                        subagent: Some(child.id.clone()),
                                        ..parent.key.clone()
                                    },
                                    child,
                                )
                            })
                            .collect();
                        children.sort_by(|a, b| {
                            use crate::daemon::state::SubagentStatus;
                            (
                                a.1.status != SubagentStatus::Running,
                                a.1.started_ts,
                                &a.1.id,
                            )
                                .cmp(&(
                                    b.1.status != SubagentStatus::Running,
                                    b.1.started_ts,
                                    &b.1.id,
                                ))
                        });
                        (repo.label.clone(), AgentGroup { parent, children })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        groups.sort_by(|(a_repo, a), (b_repo, b)| {
            let tier = |d: &Destination| {
                if d.row.agent_ts == 0 {
                    4
                } else {
                    match d.row.agent {
                        AgentStatus::Waiting => 0,
                        AgentStatus::Working => 2,
                        AgentStatus::Idle => 3,
                    }
                }
            };
            (tier(&a.parent), a_repo, &a.parent.title, &a.parent.key).cmp(&(
                tier(&b.parent),
                b_repo,
                &b.parent.title,
                &b.parent.key,
            ))
        });
        groups.into_iter().map(|(_, group)| group).collect()
    }
}

/// Selection follows identity on reorder, or the nearest surviving neighbor on
/// removal. A slot reassigned to another branch is a different destination.
#[derive(Debug, Default)]
pub struct Selection {
    pub key: Option<DestinationKey>,
    index: usize,
}
impl Selection {
    pub fn select(&mut self, keys: &[DestinationKey], index: usize) {
        self.index = index.min(keys.len().saturating_sub(1));
        self.key = keys.get(self.index).cloned();
    }
    pub fn reconcile(&mut self, keys: &[DestinationKey]) {
        let index = self
            .key
            .as_ref()
            .and_then(|key| keys.iter().position(|k| k == key))
            .unwrap_or(self.index);
        self.select(keys, index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::{SubagentRow, SubagentStatus};
    use crate::discover::test_support::row;
    use std::path::Path;

    fn up(model: &mut Workspace, id: &str, root: Option<&str>) {
        model.apply(Event::Up {
            id: id.into(),
            sock: format!("/runtime/{id}.sock").into(),
            identity: root.map(|root| RepoIdentity {
                common_dir: format!("{root}/.bare").into(),
                session_name: format!("{id}-session"),
                version: None,
            }),
        });
    }
    fn snapshot(model: &mut Workspace, id: &str, rows: Vec<WorktreeRow>) {
        model.apply(Event::Snapshot {
            id: id.into(),
            snapshot: Snapshot {
                rows,
                default_known: true,
            },
        });
    }
    fn wt(name: &str) -> WorktreeRow {
        row(name, &Path::new("/repo").join(name))
    }

    #[test]
    fn inventory_keeps_non_attention_rows_cold_and_unidentified_repos() {
        let mut model = Workspace::default();
        model.set_registry(vec![RepoEntry {
            common_dir: "/stopped/quiet/.bare".into(),
            session_name: "quiet-session".into(),
            last_launched: 1,
        }]);
        up(&mut model, "one", Some("/a/repo"));
        up(&mut model, "two", Some("/b/repo"));
        up(&mut model, "old", None);
        snapshot(&mut model, "one", vec![wt("main"), wt("clean")]);
        snapshot(&mut model, "two", vec![]);
        snapshot(&mut model, "old", vec![wt("unknown")]);
        let repos = model.repositories();
        assert_eq!(repos.len(), 4);
        assert_eq!(repos[0].label, "repo (/a/repo)");
        assert_eq!(repos[1].label, "repo (/b/repo)");
        assert_eq!(repos[0].destinations().len(), 2);
        assert!(repos[1].destinations().is_empty());
        assert!(repos[2].label.contains("/runtime/old.sock"));
        assert!(!repos[2].can_activate());
        assert_eq!(repos[3].connection, Connection::Stopped);
        assert!(repos[3].can_activate());
        assert!(model.agents().is_empty());
    }

    #[test]
    fn disconnect_retains_stale_rows_reconnect_is_loading_until_snapshot() {
        let mut model = Workspace::default();
        up(&mut model, "a", Some("/repo"));
        snapshot(&mut model, "a", vec![wt("feature")]);
        model.apply(Event::Down { id: "a".into() });
        let repos = model.repositories();
        let repo = repos.first().unwrap();
        assert_eq!(repo.connection, Connection::Disconnected);
        assert_eq!(repo.destinations().len(), 1);
        assert!(!repo.can_activate());
        up(&mut model, "a", Some("/repo"));
        assert_eq!(model.repositories()[0].connection, Connection::Loading);
        assert_eq!(model.repositories()[0].destinations().len(), 1);
        assert_eq!(
            model.repositories()[0].snapshot.as_ref().unwrap().rows[0].name,
            "feature"
        );
        snapshot(&mut model, "a", vec![]);
        assert_eq!(model.repositories()[0].connection, Connection::Live);
    }

    #[test]
    fn registered_disconnected_repo_reconciles_to_stopped_without_cached_rows() {
        let entry = RepoEntry {
            common_dir: "/repo/.git".into(),
            session_name: "repo-session".into(),
            last_launched: 1,
        };
        let id = repo_id(&entry.common_dir);
        let mut model = Workspace::default();
        model.set_registry(vec![entry]);
        up(&mut model, &id, Some("/repo"));
        snapshot(&mut model, &id, vec![wt("feature")]);
        model.apply(Event::Down { id: id.clone() });

        let repos = model.repositories();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].connection, Connection::Stopped);
        assert!(repos[0].can_activate());
        assert!(repos[0].destinations().is_empty());
    }

    #[test]
    fn late_registry_refresh_reconciles_an_already_disconnected_repo() {
        let entry = RepoEntry {
            common_dir: "/repo/.git".into(),
            session_name: "repo-session".into(),
            last_launched: 1,
        };
        let id = repo_id(&entry.common_dir);
        let mut model = Workspace::default();
        up(&mut model, &id, Some("/repo"));
        snapshot(&mut model, &id, vec![wt("stale-feature")]);
        model.apply(Event::Down { id: id.clone() });
        model.set_registry(vec![entry]);

        let repos = model.repositories();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].connection, Connection::Stopped);
        assert!(repos[0].can_activate());
        assert!(repos[0].destinations().is_empty());
    }

    #[test]
    fn registry_entry_does_not_duplicate_connected_peer_and_late_identity_is_safe() {
        let entry = RepoEntry {
            common_dir: "/repo/.bare".into(),
            session_name: "repo-session".into(),
            last_launched: 0,
        };
        let id = repo_id(&entry.common_dir);
        let mut model = Workspace::default();
        model.set_registry(vec![entry]);
        up(&mut model, &id, None);
        snapshot(&mut model, &id, vec![wt("feature")]);
        assert_eq!(model.repositories().len(), 1);
        up(&mut model, &id, Some("/repo"));
        assert_eq!(model.repositories()[0].connection, Connection::Live);
        assert_eq!(model.repositories()[0].destinations().len(), 1);
    }

    #[test]
    fn titles_slots_trunk_and_parent_groups_are_preserved() {
        let mut model = Workspace::default();
        up(&mut model, "a", Some("/repo"));
        let mut main = wt("main");
        main.is_default = true;
        let mut working = wt("feature");
        working.agent = AgentStatus::Working;
        working.agent_ts = 10;
        working.title = "Implement parser".into();
        working.subagents = vec![SubagentRow {
            id: "reviewer".into(),
            agent_type: "review".into(),
            description: Some("Review parser".into()),
            status: SubagentStatus::Running,
            started_ts: 11,
            last_ts: 12,
            finished_ts: None,
        }];
        let mut waiting = wt("waiting");
        waiting.agent = AgentStatus::Waiting;
        waiting.agent_ts = 10;
        let mut free = wt("work2");
        free.slot_state = Some(SlotState::Free);
        free.agent_ts = 10;
        let mut pinned = wt("work10");
        pinned.slot_state = Some(SlotState::Pinned);
        pinned.pin = Some("reserved".into());
        snapshot(&mut model, "a", vec![pinned, free, working, main, waiting]);
        let destinations = model.repositories()[0].destinations();
        assert_eq!(
            destinations
                .iter()
                .map(|d| d.row.name.as_str())
                .collect::<Vec<_>>(),
            ["feature", "main", "waiting", "work2", "work10"]
        );
        assert_eq!(destinations[0].title, "Implement parser");
        assert!(destinations[1].row.is_default);
        assert_eq!(destinations[1].title, "main");
        assert_eq!(destinations[3].title, "free");
        assert_eq!(destinations[4].title, "reserved");
        let groups = model.agents();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].parent.row.name, "waiting");
        assert_eq!(groups[1].children[0].0.parent(), groups[1].parent.key);
    }

    #[test]
    fn selection_survives_reorder_removal_and_rejects_slot_branch_reuse() {
        let key = |branch: &str| DestinationKey {
            repo: "a".into(),
            path: "/repo/work1".into(),
            branch: branch.into(),
            session: None,
            subagent: None,
        };
        let (a, b, c) = (key("a"), key("b"), key("c"));
        let mut selection = Selection::default();
        selection.select(&[a.clone(), b.clone(), c.clone()], 1);
        selection.reconcile(&[c.clone(), a.clone(), b.clone()]);
        assert_eq!(selection.key, Some(b.clone()));
        selection.reconcile(&[c.clone(), a.clone()]);
        assert_eq!(selection.key, Some(a));
        selection.reconcile(std::slice::from_ref(&c));
        assert_eq!(selection.key, Some(c));
        assert_ne!(selection.key, Some(b));
        selection.reconcile(&[]);
        assert!(selection.key.is_none());
    }
}
