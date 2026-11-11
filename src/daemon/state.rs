use crate::config::Harness;
use crate::github::PrSummary;
use crate::util::now_unix;
use crate::worktree::{self, GitInfo, RemoveRefusedReason, Worktree};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum AgentStatus {
    Working,
    Waiting,
    #[default]
    Idle,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRecord {
    pub status: AgentStatus,
    pub ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Claude Code session id (UUID) for this worktree's active conversation.
    /// Persisted so a restarted swamp can resume the same session via
    /// `claude --resume <id>` while the worktree still exists (#33).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Per-worktree harness override, honored when the repo setting is `choose`.
    /// Set from the worktrees pane (`h`) and read at launch to build the agent
    /// pane for the right agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Harness>,
}

/// How long a done subagent stays visible before the daemon drops it.
pub const SUBAGENT_DONE_GRACE_SECS: u64 = 60;
/// How long a running subagent may go without any event before the daemon
/// treats it as gone (for example, the parent session was killed).
pub const SUBAGENT_STALE_SECS: u64 = 15 * 60;
/// How long a parent `Agent` tool description waits for its `SubagentStart`.
const PENDING_DESCRIPTION_SECS: u64 = 30;

/// What a subagent-related hook reports. Sent by `swamp hook` alongside the
/// positional status; see `crate::hook::parse_payload`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentEventKind {
    /// `SubagentStart`: a subagent began (or resumed).
    Start,
    /// `SubagentStop`: a subagent finished.
    Stop,
    /// A tool hook that fired inside a subagent.
    Activity,
    /// The parent's `Agent` tool call. Carries the task description, and the
    /// exact agent id when the tool response reports one (background runs).
    Spawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentEvent {
    pub kind: SubagentEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Running,
    Done,
}

/// One subagent of a worktree's agent, as carried in snapshots. The daemon
/// keeps these in memory only; they are never written to the status file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRow {
    pub id: String,
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: SubagentStatus,
    pub started_ts: u64,
    pub last_ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_ts: Option<u64>,
}

#[derive(Debug, Clone)]
struct PendingDescription {
    agent_type: String,
    description: String,
    ts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeRow {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
    pub upstream: Option<String>,
    #[serde(default)]
    pub upstream_gone: bool,
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflict: bool,
    pub rebase: bool,
    pub agent: AgentStatus,
    pub agent_ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default)]
    pub head_ts: u64,
    /// Effective harness override for this worktree (see [`AgentRecord::harness`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<Harness>,
    /// Whether this row's branch is the repository default branch (trunk). The
    /// TUI uses it to pin, mark, and suppress PR status for the default row.
    /// `#[serde(default)]` keeps older `.swamp-status.json` snapshots loadable
    /// (they decode as `false` and are corrected on the next scan).
    #[serde(default)]
    pub is_default: bool,
    /// Legacy protocol field retained for compatibility with version 0.7
    /// clients. Routine scans no longer compute removal verdicts, so new
    /// snapshots always set this field to `None`.
    #[serde(default)]
    pub removal_block: Option<RemoveRefusedReason>,
    /// True while a removal of this worktree is in flight, set before
    /// `repo_ops` is acquired so the whole wait is visible in every
    /// subscribed TUI. `#[serde(default)]` keeps an older peer's snapshot
    /// loadable.
    #[serde(default)]
    pub deleting: bool,
    /// Subagents of this worktree's agent, running first, then by start time.
    /// Projected from `DaemonState::subagents` by [`DaemonState::snapshot`].
    /// `#[serde(default)]` keeps an older peer's snapshot loadable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<SubagentRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: Vec<WorktreeRow>,
    /// Whether the daemon could resolve the repository default branch at all.
    /// False for a repository with no default remote `HEAD` — a locally
    /// initialized repository, or a bare clone whose remote HEAD was never
    /// fetched. Every row then decodes as `is_default == false`, so a guard
    /// written against `is_default` alone silently stops guarding. Consumers
    /// that refuse a destructive action on the default worktree MUST check
    /// this flag too and fail closed. `#[serde(default)]` decodes an older
    /// peer's snapshot as "unknown", the safe reading.
    #[serde(default)]
    pub default_known: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrSnapshot {
    pub prs: HashMap<String, PrSummary>,
    /// Unix timestamp (seconds) of the last *successful* PR fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<u64>,
    /// Set when the most recent fetch failed; cleared on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True until the first PR fetch resolves (success *or* error). Lets the TUI
    /// distinguish a never-fetched (loading) state from a fetched-but-empty one,
    /// so it shows "Loading PRs…" on first launch instead of "No PRs". A
    /// `#[serde(default)]` of `false` means an older peer's snapshot decodes as
    /// already-fetched (the safe, non-spinning interpretation).
    #[serde(default)]
    pub loading: bool,
}

pub struct DaemonState {
    pub rows: HashMap<String, WorktreeRow>,
    pub agents: HashMap<String, AgentRecord>,
    pub prs: HashMap<String, PrSummary>,
    /// Unix timestamp of the last successful PR fetch (mirrors `PrSnapshot::fetched_at`).
    pr_fetched_at: Option<u64>,
    /// Last PR fetch error, if any (mirrors `PrSnapshot::error`).
    pr_error: Option<String>,
    /// Mirrors `PrSnapshot::loading`: true until the first PR fetch resolves.
    /// Starts true (no fetch has happened yet) and is cleared by the first
    /// `update_prs` or `record_pr_error`.
    pr_loading: bool,
    /// Repository default branch name (e.g. `main`), resolved once in [`load`]
    /// from the default remote's `HEAD`. Empty when undetectable. Not
    /// serialized — it is re-resolved on every daemon start, so a mid-session
    /// default-branch change is picked up on the next restart.
    pub default_branch: String,
    /// Names of worktrees with a removal currently in flight. Kept separate
    /// from `rows` (rather than a field on the row) because a delete races
    /// with the rescan its own directory removal triggers via the fs
    /// watcher: `apply_scanned_rows` replaces `rows` wholesale, which would
    /// silently drop a field carried on the row but can't drop an entry in
    /// an independent set it doesn't touch. Projected onto
    /// `WorktreeRow::deleting` by [`Self::snapshot`].
    deleting: HashSet<String>,
    /// Subagents per worktree. In memory only: like `deleting`, it is kept
    /// apart from `rows` so a rescan cannot drop it, and apart from `agents`
    /// so `persist` never writes it.
    subagents: HashMap<String, Vec<SubagentRow>>,
    /// Parent `Agent` tool descriptions that wait for their `SubagentStart`,
    /// per worktree, oldest first.
    pending_descriptions: HashMap<String, Vec<PendingDescription>>,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            rows: HashMap::new(),
            agents: HashMap::new(),
            prs: HashMap::new(),
            pr_fetched_at: None,
            pr_error: None,
            // No PR fetch has happened yet, so a fresh daemon reports loading.
            pr_loading: true,
            default_branch: String::new(),
            deleting: HashSet::new(),
            subagents: HashMap::new(),
            pending_descriptions: HashMap::new(),
        }
    }
}

impl DaemonState {
    pub async fn load(common_dir: &Path) -> Result<Self> {
        // Hydrate the agent records persisted by a prior run. `persist` rewrites
        // the whole `agents` map, so without this an empty in-memory map would
        // clobber other worktrees' session ids / harness overrides the first
        // time any record changes (a hook ping or `set_harness`).
        let agents = load_agents(common_dir).await;
        // Resolve the default branch once per daemon lifetime. It is read from
        // the default remote's HEAD and effectively never changes within a
        // session, so re-resolving it on every scan would be wasted work.
        let default_branch = worktree::default_branch(common_dir);
        Ok(Self {
            agents,
            default_branch,
            ..Default::default()
        })
    }

    pub async fn persist(&self, common_dir: &Path) -> Result<()> {
        let path = common_dir.join(".swamp-status.json");
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.agents)?;
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    /// Swap freshly computed rows (produced by [`scan_worktrees`]) into the
    /// state, logging worktree-set changes at info level.  Callers that want
    /// to run the git scan off the async runtime (see `refresh_all_unlocked`)
    /// call `scan_worktrees` in a `spawn_blocking` block and then call this
    /// under the write lock to do just the in-memory swap.
    pub fn apply_scanned_rows(&mut self, new_rows: HashMap<String, WorktreeRow>) {
        let added: Vec<&str> = new_rows
            .keys()
            .filter(|k| !self.rows.contains_key(*k))
            .map(String::as_str)
            .collect();
        let removed: Vec<String> = self
            .rows
            .keys()
            .filter(|k| !new_rows.contains_key(*k))
            .cloned()
            .collect();
        if added.is_empty() && removed.is_empty() {
            tracing::debug!(worktrees = new_rows.len(), "git state refreshed");
        } else {
            tracing::info!(
                worktrees = new_rows.len(),
                ?added,
                ?removed,
                "worktree set changed"
            );
        }
        for name in &removed {
            self.drop_subagents(name);
        }
        self.rows = new_rows;
    }

    /// Record an agent status ping. Returns `true` when the record changed in
    /// a way subscribers can observe (status, session name/id) — repeated pings
    /// of the same status only refresh the in-memory timestamp and return
    /// `false`, so the daemon can skip the persist + snapshot broadcast. Active
    /// agents ping on every tool call; broadcasting each one made every hook a
    /// tab-reconcile trigger in the TUI.
    pub fn apply_hook(
        &mut self,
        wt_name: &str,
        status: &str,
        session_name: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<bool> {
        let agent_status = match status.to_lowercase().as_str() {
            "working" => AgentStatus::Working,
            "waiting" => AgentStatus::Waiting,
            "idle" | "done" | "stop" => AgentStatus::Idle,
            other => anyhow::bail!("unknown status: {}", other),
        };
        let existing = self.agents.get(wt_name);
        let session = session_name
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| existing.and_then(|r| r.session_name.clone()));
        // Like session_name, a missing/empty session id preserves the previously
        // recorded one rather than clearing it — most hooks don't carry it.
        let sid = session_id
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| existing.and_then(|r| r.session_id.clone()));
        // Preserve any per-worktree harness override across status pings.
        let harness = existing.and_then(|r| r.harness);
        // The timestamp always moves; "changed" means anything else did. The
        // harness is carried over verbatim, so it can never be a diff source.
        let changed = match existing {
            None => true,
            Some(prev) => {
                prev.status != agent_status
                    || prev.session_name != session
                    || prev.session_id != sid
            }
        };
        let rec = AgentRecord {
            status: agent_status,
            ts: now_unix(),
            session_name: session,
            session_id: sid,
            harness,
        };
        self.agents.insert(wt_name.to_string(), rec.clone());
        if let Some(row) = self.rows.get_mut(wt_name) {
            row.agent = rec.status;
            row.agent_ts = rec.ts;
            row.session_name = rec.session_name;
        }
        Ok(changed)
    }

    /// Record a hook that may carry a subagent event. Returns `true` when subscribers can observe the change, with the
    /// same meaning as [`Self::apply_hook`].
    ///
    /// A `start`, `stop`, or `activity` event updates only the subagent: the
    /// parent record is not touched, so a background subagent cannot flip an
    /// idle parent back to working. A `spawn` event (the parent's own `Agent`
    /// tool call) records the description and then applies the parent status.
    pub fn apply_hook_event(
        &mut self,
        wt_name: &str,
        status: &str,
        session_name: Option<&str>,
        session_id: Option<&str>,
        subagent: Option<&SubagentEvent>,
    ) -> Result<bool> {
        self.apply_hook_event_at(
            wt_name,
            status,
            session_name,
            session_id,
            subagent,
            now_unix(),
        )
    }

    /// [`Self::apply_hook_event`] with an explicit clock for the subagent
    /// timestamps, so tests can step time.
    pub fn apply_hook_event_at(
        &mut self,
        wt_name: &str,
        status: &str,
        session_name: Option<&str>,
        session_id: Option<&str>,
        subagent: Option<&SubagentEvent>,
        now: u64,
    ) -> Result<bool> {
        let mut changed = false;
        if let Some(ev) = subagent {
            if ev.kind != SubagentEventKind::Spawn {
                return Ok(self.apply_subagent_event(wt_name, ev, now));
            }
            changed |= self.record_spawn(wt_name, ev, now);
        }
        changed |= self.apply_hook(wt_name, status, session_name, session_id)?;
        Ok(changed)
    }

    /// Apply a `start`, `stop`, or `activity` event. Events without an agent
    /// id or with an empty agent type (Claude's internal agents) are ignored.
    fn apply_subagent_event(&mut self, wt_name: &str, ev: &SubagentEvent, now: u64) -> bool {
        let Some(id) = ev.agent_id.as_deref().filter(|s| !s.is_empty()) else {
            return false;
        };
        if ev.agent_type.is_empty() {
            return false;
        }
        let list = self.subagents.entry(wt_name.to_string()).or_default();
        if let Some(rec) = list.iter_mut().find(|r| r.id == id) {
            rec.last_ts = now;
            return match ev.kind {
                SubagentEventKind::Start if rec.status == SubagentStatus::Done => {
                    rec.status = SubagentStatus::Running;
                    rec.finished_ts = None;
                    true
                }
                SubagentEventKind::Stop if rec.status == SubagentStatus::Running => {
                    rec.status = SubagentStatus::Done;
                    rec.finished_ts = Some(now);
                    true
                }
                _ => false,
            };
        }
        let done = ev.kind == SubagentEventKind::Stop;
        let description = take_pending_description(
            self.pending_descriptions.get_mut(wt_name),
            &ev.agent_type,
            now,
        );
        let list = self.subagents.entry(wt_name.to_string()).or_default();
        list.push(SubagentRow {
            id: id.to_string(),
            agent_type: ev.agent_type.clone(),
            description,
            status: if done {
                SubagentStatus::Done
            } else {
                SubagentStatus::Running
            },
            started_ts: now,
            last_ts: now,
            finished_ts: done.then_some(now),
        });
        true
    }

    /// Record the description of a parent `Agent` tool call. Without an agent
    /// id, queue it for the next `SubagentStart` of the same type. With an id
    /// (a background run's tool response), set it on that exact subagent.
    fn record_spawn(&mut self, wt_name: &str, ev: &SubagentEvent, now: u64) -> bool {
        let Some(description) = ev.description.as_deref().filter(|s| !s.is_empty()) else {
            return false;
        };
        let pending = self
            .pending_descriptions
            .entry(wt_name.to_string())
            .or_default();
        pending.retain(|p| now.saturating_sub(p.ts) <= PENDING_DESCRIPTION_SECS);
        let Some(id) = ev.agent_id.as_deref().filter(|s| !s.is_empty()) else {
            pending.push(PendingDescription {
                agent_type: ev.agent_type.clone(),
                description: description.to_string(),
                ts: now,
            });
            return false;
        };
        // The PreToolUse for this call queued the same description. Drop it so
        // it cannot attach to a later subagent of the same type.
        if let Some(pos) = pending
            .iter()
            .position(|p| p.agent_type == ev.agent_type && p.description == description)
        {
            pending.remove(pos);
        }
        let list = self.subagents.entry(wt_name.to_string()).or_default();
        match list.iter_mut().find(|r| r.id == id) {
            Some(rec) if rec.description.as_deref() == Some(description) => false,
            Some(rec) => {
                rec.description = Some(description.to_string());
                true
            }
            None => {
                list.push(SubagentRow {
                    id: id.to_string(),
                    agent_type: ev.agent_type.clone(),
                    description: Some(description.to_string()),
                    status: SubagentStatus::Running,
                    started_ts: now,
                    last_ts: now,
                    finished_ts: None,
                });
                true
            }
        }
    }

    /// Drop done subagents past their grace period and running subagents that
    /// have gone stale. Returns whether any subagent was dropped.
    pub fn expire_subagents(&mut self, now: u64) -> bool {
        let mut changed = false;
        for list in self.subagents.values_mut() {
            let before = list.len();
            list.retain(|r| match r.status {
                SubagentStatus::Done => {
                    now.saturating_sub(r.finished_ts.unwrap_or(r.last_ts))
                        <= SUBAGENT_DONE_GRACE_SECS
                }
                SubagentStatus::Running => now.saturating_sub(r.last_ts) <= SUBAGENT_STALE_SECS,
            });
            changed |= list.len() != before;
        }
        self.subagents.retain(|_, list| !list.is_empty());
        for pending in self.pending_descriptions.values_mut() {
            pending.retain(|p| now.saturating_sub(p.ts) <= PENDING_DESCRIPTION_SECS);
        }
        self.pending_descriptions.retain(|_, p| !p.is_empty());
        changed
    }

    fn drop_subagents(&mut self, wt_name: &str) {
        self.subagents.remove(wt_name);
        self.pending_descriptions.remove(wt_name);
    }

    /// Record the per-worktree harness override (worktrees pane `h`). Preserves
    /// the rest of the agent record so an existing session/status isn't lost.
    pub fn set_harness(&mut self, wt_name: &str, harness: Harness) {
        let rec = self.agents.entry(wt_name.to_string()).or_default();
        rec.harness = Some(harness);
        if let Some(row) = self.rows.get_mut(wt_name) {
            row.harness = Some(harness);
        }
    }

    /// Optimistically drop a single worktree row after a delete, without the
    /// full rescan `apply_scanned_rows` performs. Lets the daemon broadcast the
    /// removal immediately; a background refresh reconciles the rest. Returns
    /// whether a row was actually present.
    pub fn remove_row(&mut self, name: &str) -> bool {
        let removed = self.rows.remove(name).is_some();
        // Drop the now-orphaned agent record so a later worktree reusing the
        // name can't inherit stale status.
        self.agents.remove(name);
        self.drop_subagents(name);
        removed
    }

    /// Mark `name` as having a removal in flight. Call **before** acquiring
    /// `repo_ops` and broadcast the resulting snapshot immediately, so the
    /// whole wait for the lock — a queued fetch can hold it for up to 60s —
    /// is visible in every subscribed TUI, not just the removal itself.
    pub fn mark_deleting(&mut self, name: &str) {
        self.deleting.insert(name.to_string());
    }

    /// Clear the in-flight mark set by [`Self::mark_deleting`]. Idempotent —
    /// safe to call on success, refusal, failure, or from the cleanup guard
    /// that catches a panicked or cancelled removal task.
    pub fn clear_deleting(&mut self, name: &str) {
        self.deleting.remove(name);
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut rows: Vec<WorktreeRow> = self
            .rows
            .values()
            .cloned()
            .map(|mut row| {
                row.deleting = self.deleting.contains(&row.name);
                row.subagents = self.subagents.get(&row.name).cloned().unwrap_or_default();
                row.subagents.sort_by(|a, b| {
                    (a.status != SubagentStatus::Running)
                        .cmp(&(b.status != SubagentStatus::Running))
                        .then(a.started_ts.cmp(&b.started_ts))
                        .then(a.id.cmp(&b.id))
                });
                row
            })
            .collect();
        rows.sort_by(|a, b| b.head_ts.cmp(&a.head_ts).then(a.name.cmp(&b.name)));
        Snapshot {
            rows,
            default_known: !self.default_branch.is_empty(),
        }
    }

    /// Record a successful PR fetch.
    ///
    /// Replaces the PR map, records `fetched_at`, and clears any previous
    /// error.  The daemon's PR poller calls this on the `Ok(Ok(prs))` arm.
    pub fn update_prs(&mut self, prs: HashMap<String, PrSummary>) {
        self.prs = prs;
        self.pr_fetched_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs());
        self.pr_error = None;
        // The first fetch has resolved; clear the loading state.
        self.pr_loading = false;
    }

    /// Record a PR fetch failure.
    ///
    /// Keeps the existing `self.prs` map so the TUI continues to display
    /// stale-but-valid data.  Sets `pr_error` for display in the PR view.
    /// The daemon's PR poller calls this on the `Ok(Err(e))` arm.
    pub fn record_pr_error(&mut self, error: String) {
        // Intentionally does NOT clear `self.prs`.
        tracing::warn!(error = %error, "github PR fetch failed; keeping previous state");
        self.pr_error = Some(error);
        // A fetch resolved (with an error); stop reporting loading so a repo with
        // no `gh`/network settles into the "github unreachable" path rather than
        // spinning "Loading…" forever.
        self.pr_loading = false;
    }

    pub fn pr_snapshot(&self) -> PrSnapshot {
        PrSnapshot {
            prs: self.prs.clone(),
            fetched_at: self.pr_fetched_at,
            error: self.pr_error.clone(),
            loading: self.pr_loading,
        }
    }

    /// The PR map an authoritative removal check should trust for the
    /// merged-branch signal. Returns `None` while the first fetch hasn't
    /// resolved or the most recent fetch failed.
    pub fn pr_state_for_verdicts(&self) -> Option<&HashMap<String, PrSummary>> {
        if self.pr_loading || self.pr_error.is_some() {
            None
        } else {
            Some(&self.prs)
        }
    }
}

/// Read the persisted `name → AgentRecord` map from `.swamp-status.json`.
/// A missing or malformed file yields an empty map, so a fresh repo (or a typo)
/// simply starts with no recorded agents rather than failing the daemon.
async fn load_agents(common_dir: &Path) -> HashMap<String, AgentRecord> {
    let path = common_dir.join(".swamp-status.json");
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return HashMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Run the full git scan (list_worktrees + per-worktree git_info) and return
/// the computed row map.  This function is CPU/IO-bound and is meant to be
/// called from `tokio::task::spawn_blocking`; it must NOT be called while
/// holding any async lock.
///
/// `agents` is a snapshot cloned out from `DaemonState::agents` under a read
/// lock *before* this call; the caller swaps the result in under the write lock
/// with [`DaemonState::apply_scanned_rows`]. `prs` is the PR map to trust for
/// the merged-branch removal signal — pass `None` while a PR fetch is loading
/// or has most recently failed, since a stale-but-cached map shouldn't unlock
/// a squash-merge deletion during an outage (see
/// [`DaemonState::pr_state_for_verdicts`]).
pub fn scan_worktrees(
    common_dir: &Path,
    agents: &HashMap<String, AgentRecord>,
    default_branch: &str,
) -> Result<HashMap<String, WorktreeRow>> {
    let wts = worktree::list_worktrees(common_dir)?;
    if wts.is_empty() {
        return Ok(HashMap::new());
    }

    // Gather per-worktree git status concurrently. Each `git_info` shells out to
    // `git status` / `git rev-list`, so a sequential loop made first-launch
    // latency scale with worktree count. We're already on a `spawn_blocking`
    // thread, so fanning the work across a small pool of scoped OS threads is
    // safe; `MAX_SCAN_CONCURRENCY` caps the number of simultaneous `git`
    // subprocesses so a repo with dozens of worktrees can't stampede the box.
    const MAX_SCAN_CONCURRENCY: usize = 8;
    let workers = MAX_SCAN_CONCURRENCY.min(wts.len());
    let next = AtomicUsize::new(0);
    let new_rows: Mutex<HashMap<String, WorktreeRow>> = Mutex::new(HashMap::new());

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(wt) = wts.get(i) else { break };
                    let info = match worktree::git_info(&wt.path) {
                        Ok(info) => info,
                        Err(error) => {
                            tracing::warn!(
                                worktree = %wt.name(),
                                %error,
                                "worktree status read failed"
                            );
                            GitInfo::default()
                        }
                    };
                    let name = wt.name();
                    let agent = agents.get(&name).cloned().unwrap_or_default();
                    let row = build_row(wt, &info, &agent, default_branch);
                    tracing::trace!(
                        worktree = %name,
                        branch = %row.branch,
                        ahead = row.ahead,
                        behind = row.behind,
                        dirty = row.staged + row.unstaged + row.untracked,
                        "scanned worktree"
                    );
                    new_rows.lock().unwrap().insert(name, row);
                }
            });
        }
    });

    Ok(new_rows.into_inner().unwrap())
}

fn build_row(
    wt: &Worktree,
    info: &GitInfo,
    agent: &AgentRecord,
    default_branch: &str,
) -> WorktreeRow {
    let branch = if info.branch.is_empty() || info.branch == "(detached)" {
        wt.branch.clone()
    } else {
        info.branch.clone()
    };
    let is_default = !default_branch.is_empty() && branch == default_branch;
    WorktreeRow {
        name: wt.name(),
        path: wt.path.clone(),
        branch,
        upstream: info.upstream.clone(),
        upstream_gone: info.upstream_gone,
        ahead: info.ahead,
        behind: info.behind,
        staged: info.staged,
        unstaged: info.unstaged,
        untracked: info.untracked,
        conflict: info.conflict,
        rebase: info.rebase,
        agent: agent.status,
        agent_ts: agent.ts,
        session_name: agent.session_name.clone(),
        head_ts: info.head_ts,
        harness: agent.harness,
        is_default,
        removal_block: None,
        // Projected from `DaemonState::deleting` by `snapshot()`, not known
        // at scan time — a scan and a delete can race, and the scan must not
        // clobber an in-flight delete's mark.
        deleting: false,
        // Projected from `DaemonState::subagents` by `snapshot()`.
        subagents: Vec::new(),
    }
}

/// Pop the oldest live pending description for `agent_type`, discarding
/// expired entries on the way.
fn take_pending_description(
    pending: Option<&mut Vec<PendingDescription>>,
    agent_type: &str,
    now: u64,
) -> Option<String> {
    let pending = pending?;
    pending.retain(|p| now.saturating_sub(p.ts) <= PENDING_DESCRIPTION_SECS);
    let pos = pending.iter().position(|p| p.agent_type == agent_type)?;
    Some(pending.remove(pos).description)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_row(name: &str) -> WorktreeRow {
        make_row_with_ts(name, 0)
    }

    fn make_row_with_ts(name: &str, head_ts: u64) -> WorktreeRow {
        WorktreeRow {
            name: name.to_string(),
            path: PathBuf::from(format!("/repo/{}", name)),
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
            head_ts,
            harness: None,
            is_default: false,
            removal_block: None,
            deleting: false,
            subagents: Vec::new(),
        }
    }

    fn worktree(name: &str, branch: &str) -> Worktree {
        Worktree {
            path: PathBuf::from(format!("/repo/{name}")),
            branch: branch.to_string(),
        }
    }

    /// `build_row` flags only the worktree on the repo default branch, and
    /// leaves every other row unflagged.
    #[test]
    fn build_row_flags_default_branch() {
        let agent = AgentRecord::default();

        let info_main = GitInfo {
            branch: "main".into(),
            ..Default::default()
        };
        let main = build_row(&worktree("main", "main"), &info_main, &agent, "main");
        assert!(main.is_default, "the default branch row must be flagged");

        let info_feat = GitInfo {
            branch: "feature/x".into(),
            ..Default::default()
        };
        let feat = build_row(
            &worktree("feature-x", "feature/x"),
            &info_feat,
            &agent,
            "main",
        );
        assert!(!feat.is_default, "a non-default row must not be flagged");
    }

    /// When the default branch is undetectable (empty), no row is flagged —
    /// even a worktree literally named/branched `main`.
    #[test]
    fn build_row_no_default_when_undetectable() {
        let agent = AgentRecord::default();
        let info = GitInfo {
            branch: "main".into(),
            ..Default::default()
        };
        let row = build_row(&worktree("main", "main"), &info, &agent, "");
        assert!(
            !row.is_default,
            "no row may be flagged when the default branch is unknown"
        );
    }

    /// The snapshot carries whether the default branch was resolvable at all,
    /// so a consumer can tell "this row is not trunk" apart from "no row can
    /// be trunk because detection failed". `is_default` alone cannot: it
    /// reads `false` in both cases.
    #[test]
    fn snapshot_reports_whether_the_default_branch_is_known() {
        let mut state = DaemonState::default();
        assert!(
            !state.snapshot().default_known,
            "an unresolved default branch must report as unknown"
        );

        state.default_branch = "main".to_string();
        assert!(
            state.snapshot().default_known,
            "a resolved default branch must report as known"
        );
    }

    /// Routine scans leave removal checks to the deletion path.
    #[test]
    fn scan_worktrees_does_not_compute_removal_block() {
        use crate::worktree::create_worktree;
        use crate::worktree::test_support::{git_available, setup};

        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let dirty = create_worktree(&bare, "feature").unwrap();
        std::fs::write(dirty.path.join("scratch.txt"), "wip").unwrap();

        let rows = scan_worktrees(&bare, &HashMap::new(), "").unwrap();

        assert_eq!(
            rows.get("feature").unwrap().removal_block,
            None,
            "routine scans must not compute removal verdicts"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A row snapshot from an older peer that omits `removal_block` and
    /// `deleting` still decodes — the safe "not deleting, no blocking
    /// reason" reading, not a decode failure.
    #[test]
    fn worktree_row_decodes_without_removal_fields() {
        let json = serde_json::json!({
            "name": "feature",
            "path": "/repo/feature",
            "branch": "feature",
            "upstream": null,
            "ahead": 0,
            "behind": 0,
            "staged": 0,
            "unstaged": 0,
            "untracked": 0,
            "conflict": false,
            "rebase": false,
            "agent": "idle",
            "agent_ts": 0,
        });
        let row: WorktreeRow = serde_json::from_value(json).unwrap();
        assert_eq!(row.removal_block, None);
        assert!(!row.deleting);
    }

    /// The deleting mark survives `apply_scanned_rows`, which replaces `rows`
    /// wholesale — a delete races with exactly the rescan its own directory
    /// removal triggers via the fs watcher, and the mark must not be dropped
    /// mid-delete.
    #[test]
    fn deleting_mark_survives_apply_scanned_rows() {
        let mut state = DaemonState::default();
        state.rows.insert("feature".into(), make_row("feature"));
        state.mark_deleting("feature");

        let mut new_rows = HashMap::new();
        new_rows.insert("feature".into(), make_row("feature"));
        state.apply_scanned_rows(new_rows);

        assert!(
            state.snapshot().rows[0].deleting,
            "the mark must survive a rescan that replaces the row"
        );
    }

    /// `clear_deleting` restores a marked row to `deleting: false` on the next
    /// snapshot — the shape of a refused or failed removal.
    #[test]
    fn clear_deleting_restores_the_row() {
        let mut state = DaemonState::default();
        state.rows.insert("feature".into(), make_row("feature"));
        state.mark_deleting("feature");
        assert!(state.snapshot().rows[0].deleting);

        state.clear_deleting("feature");
        assert!(!state.snapshot().rows[0].deleting);
    }

    /// With equal head_ts, snapshot falls back to alphabetical name order.
    #[test]
    fn snapshot_rows_sorted_by_name_when_same_ts() {
        let mut state = DaemonState::default();
        state.rows.insert("zebra".into(), make_row("zebra"));
        state.rows.insert("alpha".into(), make_row("alpha"));
        state.rows.insert("main".into(), make_row("main"));
        state.rows.insert("beta".into(), make_row("beta"));

        let snap = state.snapshot();
        let names: Vec<&str> = snap.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "main", "zebra"]);
    }

    /// snapshot() sorts by head_ts descending (most recently updated first).
    #[test]
    fn snapshot_rows_sorted_by_head_ts_descending() {
        let mut state = DaemonState::default();
        state
            .rows
            .insert("old".into(), make_row_with_ts("old", 100));
        state
            .rows
            .insert("newest".into(), make_row_with_ts("newest", 300));
        state
            .rows
            .insert("middle".into(), make_row_with_ts("middle", 200));

        let snap = state.snapshot();
        let names: Vec<&str> = snap.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["newest", "middle", "old"]);
    }

    /// `apply_hook` must update an existing row's agent status in-place so the
    /// next snapshot reflects it — the row must not disappear from the snapshot.
    #[test]
    fn apply_hook_updates_existing_row() {
        let mut state = DaemonState::default();
        state.rows.insert("main".into(), make_row("main"));

        state.apply_hook("main", "working", None, None).unwrap();
        let snap = state.snapshot();
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].agent, AgentStatus::Working);
    }

    /// `apply_hook` with an unknown worktree name must still succeed (the agent
    /// record is stored) but the snapshot rows must remain unchanged.
    #[test]
    fn apply_hook_unknown_worktree_is_ignored_in_rows() {
        let mut state = DaemonState::default();
        state.rows.insert("main".into(), make_row("main"));

        // "ghost" does not exist in rows; apply_hook must not crash.
        state.apply_hook("ghost", "working", None, None).unwrap();
        let snap = state.snapshot();
        // "main" row is untouched; no new row for "ghost".
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].name, "main");
        assert_eq!(snap.rows[0].agent, AgentStatus::Idle);
    }

    /// `apply_hook` reports a change only when the observable record moved:
    /// the first ping, a status transition, or a new session id. Repeated
    /// same-status pings (one per tool call from an active agent) return
    /// `false` so the daemon doesn't persist + broadcast a snapshot per ping.
    #[test]
    fn apply_hook_reports_observable_changes_only() {
        let mut state = DaemonState::default();

        // First ping for an unknown agent is a change.
        assert!(state.apply_hook("main", "working", None, None).unwrap());
        // Same status again: timestamp-only, not a change.
        assert!(!state.apply_hook("main", "working", None, None).unwrap());
        // Status transition is a change.
        assert!(state.apply_hook("main", "idle", None, None).unwrap());
        // A session id appearing is a change…
        assert!(state.apply_hook("main", "idle", None, Some("abc")).unwrap());
        // …but repeating it (or omitting it, which preserves it) is not.
        assert!(!state.apply_hook("main", "idle", None, Some("abc")).unwrap());
        assert!(!state.apply_hook("main", "idle", None, None).unwrap());
        // The in-memory timestamp still refreshes on a no-change ping.
        let before = state.agents.get("main").unwrap().ts;
        assert!(!state.apply_hook("main", "idle", None, None).unwrap());
        assert!(state.agents.get("main").unwrap().ts >= before);
    }

    /// A session id is recorded on the agent record and preserved across a
    /// later hook that omits it — so later `working`/`idle` pings don't wipe
    /// the id we need to resume the session (#33).
    #[test]
    fn apply_hook_records_and_preserves_session_id() {
        let mut state = DaemonState::default();

        state
            .apply_hook("main", "working", None, Some("abc-123"))
            .unwrap();
        assert_eq!(
            state.agents.get("main").unwrap().session_id.as_deref(),
            Some("abc-123")
        );

        // A subsequent hook without a session id keeps the recorded one.
        state.apply_hook("main", "idle", None, None).unwrap();
        assert_eq!(
            state.agents.get("main").unwrap().session_id.as_deref(),
            Some("abc-123")
        );

        // An empty session id is treated as "not provided".
        state.apply_hook("main", "working", None, Some("")).unwrap();
        assert_eq!(
            state.agents.get("main").unwrap().session_id.as_deref(),
            Some("abc-123")
        );
    }

    /// A daemon hydrates persisted agent records on load, so changing one
    /// worktree's harness and re-persisting must not clobber another worktree's
    /// recorded Claude `session_id` (needed to resume on the next launch).
    #[tokio::test]
    async fn set_harness_persist_preserves_other_session_ids() {
        let dir = std::env::temp_dir().join(format!(
            "swamp-state-hydrate-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let status = dir.join(".swamp-status.json");
        // A prior run recorded a session id for `feat`.
        std::fs::write(
            &status,
            r#"{"feat":{"status":"idle","ts":1,"session_id":"keep-me"}}"#,
        )
        .unwrap();

        let mut state = DaemonState::load(&dir).await.unwrap();
        assert_eq!(
            state.agents.get("feat").unwrap().session_id.as_deref(),
            Some("keep-me"),
            "load must hydrate existing records"
        );

        // Pick a harness for a *different* worktree, then persist.
        state.set_harness("main", Harness::Codex);
        state.persist(&dir).await.unwrap();

        // Re-read from disk: feat's session id survives, main's harness is saved.
        let reread = load_agents(&dir).await;
        assert_eq!(
            reread.get("feat").unwrap().session_id.as_deref(),
            Some("keep-me")
        );
        assert_eq!(reread.get("main").unwrap().harness, Some(Harness::Codex));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `set_harness` records the override, updates the row, and survives a later
    /// status hook that doesn't mention the harness.
    #[test]
    fn set_harness_records_and_survives_hooks() {
        let mut state = DaemonState::default();
        state.rows.insert("main".into(), make_row("main"));

        state.set_harness("main", Harness::Codex);
        assert_eq!(
            state.agents.get("main").unwrap().harness,
            Some(Harness::Codex)
        );
        assert_eq!(
            state.rows.get("main").unwrap().harness,
            Some(Harness::Codex)
        );

        // A later status ping must not wipe the override.
        state.apply_hook("main", "working", None, None).unwrap();
        assert_eq!(
            state.agents.get("main").unwrap().harness,
            Some(Harness::Codex)
        );
    }

    fn make_pr(number: u32) -> PrSummary {
        PrSummary {
            number,
            title: format!("PR {number}"),
            state: "OPEN".into(),
            head_oid: None,
            is_draft: false,
            checks: None,
            check_meta: None,
            url: None,
            comment_count: 0,
            review: None,
            reviews_partial: false,
        }
    }

    /// A successful fetch replaces the PR map and clears any prior error.
    #[test]
    fn update_prs_success_replaces_and_clears_error() {
        let mut state = DaemonState::default();
        // Seed an error state by calling record_pr_error directly.
        state.record_pr_error("old error".into());

        let mut prs = HashMap::new();
        prs.insert("feat".into(), make_pr(42));
        state.update_prs(prs);

        assert_eq!(state.prs.len(), 1);
        assert!(state.pr_error.is_none());
        assert!(state.pr_fetched_at.is_some());

        let snap = state.pr_snapshot();
        assert_eq!(snap.prs.len(), 1);
        assert!(snap.error.is_none());
        assert!(snap.fetched_at.is_some());
    }

    /// A PR fetch error keeps the previous map and records the error message.
    #[test]
    fn record_pr_error_preserves_previous_map() {
        let mut state = DaemonState::default();

        // Seed a successful fetch first.
        let mut prs = HashMap::new();
        prs.insert("feat".into(), make_pr(7));
        state.update_prs(prs);
        assert!(state.pr_fetched_at.is_some());

        // Now a transient failure must NOT wipe the map.
        state.record_pr_error("network timeout".into());
        assert_eq!(state.prs.len(), 1, "previous PR map must be preserved");
        assert_eq!(state.pr_error.as_deref(), Some("network timeout"));

        let snap = state.pr_snapshot();
        assert_eq!(snap.prs.len(), 1);
        assert_eq!(snap.error.as_deref(), Some("network timeout"));
        // fetched_at from the prior success is still present.
        assert!(snap.fetched_at.is_some());
    }

    /// A failure before any successful fetch records an empty map and an error.
    #[test]
    fn record_pr_error_on_empty_state() {
        let mut state = DaemonState::default();
        state.record_pr_error("gh not found".into());

        assert!(state.prs.is_empty());
        assert_eq!(state.pr_error.as_deref(), Some("gh not found"));
        assert!(state.pr_fetched_at.is_none());
    }

    /// A successful fetch after a recorded error clears the error.
    #[test]
    fn update_prs_after_error_clears_it() {
        let mut state = DaemonState::default();
        state.record_pr_error("transient".into());
        assert!(state.pr_error.is_some());

        let mut prs = HashMap::new();
        prs.insert("main".into(), make_pr(1));
        state.update_prs(prs);
        assert!(state.pr_error.is_none());
    }

    /// A fresh daemon (no fetch yet) reports `loading = true` so the TUI shows
    /// "Loading PRs…" rather than "No PRs" before the first fetch resolves.
    #[test]
    fn pr_snapshot_loading_by_default() {
        let state = DaemonState::default();
        assert!(
            state.pr_snapshot().loading,
            "fresh state must report loading"
        );
    }

    /// The first successful fetch clears `loading`.
    #[test]
    fn update_prs_clears_loading() {
        let mut state = DaemonState::default();
        assert!(state.pr_snapshot().loading);

        let mut prs = HashMap::new();
        prs.insert("feat".into(), make_pr(9));
        state.update_prs(prs);

        assert!(
            !state.pr_snapshot().loading,
            "a resolved fetch must clear loading"
        );
    }

    /// The first fetch *error* also clears `loading` — loading means "no fetch
    /// has resolved yet", so a repo with no `gh`/network must not spin forever.
    #[test]
    fn record_pr_error_clears_loading() {
        let mut state = DaemonState::default();
        assert!(state.pr_snapshot().loading);

        state.record_pr_error("gh not found".into());

        assert!(
            !state.pr_snapshot().loading,
            "an errored fetch must clear loading"
        );
    }

    /// A fetch error before any success clears loading while preserving the
    /// (empty) map — the snapshot reflects errored-and-empty, not loading.
    #[test]
    fn record_pr_error_clears_loading_and_keeps_map() {
        let mut state = DaemonState::default();

        // Seed a prior success so we can confirm the map is preserved on error.
        let mut prs = HashMap::new();
        prs.insert("feat".into(), make_pr(3));
        state.update_prs(prs);
        assert!(!state.pr_snapshot().loading);

        state.record_pr_error("network timeout".into());
        let snap = state.pr_snapshot();
        assert!(!snap.loading);
        assert_eq!(snap.prs.len(), 1, "error must preserve the previous map");
        assert_eq!(snap.error.as_deref(), Some("network timeout"));
    }

    // ── subagents ────────────────────────────────────────────────────────────

    fn sub(kind: SubagentEventKind, id: Option<&str>, ty: &str) -> SubagentEvent {
        SubagentEvent {
            kind,
            agent_id: id.map(str::to_string),
            agent_type: ty.to_string(),
            description: None,
        }
    }

    fn spawn(ty: &str, desc: &str, id: Option<&str>) -> SubagentEvent {
        SubagentEvent {
            description: Some(desc.to_string()),
            ..sub(SubagentEventKind::Spawn, id, ty)
        }
    }

    fn apply(state: &mut DaemonState, status: &str, ev: Option<&SubagentEvent>, now: u64) -> bool {
        state
            .apply_hook_event_at("main", status, None, None, ev, now)
            .unwrap()
    }

    fn subs(state: &DaemonState) -> Vec<SubagentRow> {
        state
            .snapshot()
            .rows
            .into_iter()
            .find(|r| r.name == "main")
            .unwrap()
            .subagents
    }

    fn state_with_main() -> DaemonState {
        let mut state = DaemonState::default();
        state.rows.insert("main".into(), make_row("main"));
        state
    }

    #[test]
    fn subagent_start_and_stop() {
        let mut state = state_with_main();
        let start = sub(SubagentEventKind::Start, Some("a1"), "Explore");
        assert!(apply(&mut state, "working", Some(&start), 100));
        let s = subs(&state);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].status, SubagentStatus::Running);
        assert_eq!(s[0].agent_type, "Explore");
        assert_eq!(s[0].started_ts, 100);

        let stop = sub(SubagentEventKind::Stop, Some("a1"), "Explore");
        assert!(apply(&mut state, "idle", Some(&stop), 130));
        let s = subs(&state);
        assert_eq!(s[0].status, SubagentStatus::Done);
        assert_eq!(s[0].finished_ts, Some(130));
        // A repeated stop is not an observable change.
        assert!(!apply(&mut state, "idle", Some(&stop), 131));
    }

    #[test]
    fn subagent_events_leave_parent_untouched() {
        let mut state = state_with_main();
        state.apply_hook("main", "idle", None, None).unwrap();
        let before = state.agents["main"].clone();
        let start = sub(SubagentEventKind::Start, Some("a1"), "general-purpose");
        apply(&mut state, "working", Some(&start), 100);
        let act = sub(SubagentEventKind::Activity, Some("a1"), "general-purpose");
        apply(&mut state, "working", Some(&act), 101);
        let stop = sub(SubagentEventKind::Stop, Some("a1"), "general-purpose");
        apply(&mut state, "idle", Some(&stop), 102);
        assert_eq!(state.agents["main"].status, AgentStatus::Idle);
        assert_eq!(state.agents["main"].ts, before.ts);
        assert_eq!(state.snapshot().rows[0].agent, AgentStatus::Idle);
    }

    #[test]
    fn subagent_revive_keeps_start_time() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("a1"), "Plan")),
            100,
        );
        apply(
            &mut state,
            "idle",
            Some(&sub(SubagentEventKind::Stop, Some("a1"), "Plan")),
            110,
        );
        assert!(apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("a1"), "Plan")),
            120
        ));
        let s = subs(&state);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].status, SubagentStatus::Running);
        assert_eq!(s[0].started_ts, 100);
        assert_eq!(s[0].finished_ts, None);
    }

    #[test]
    fn subagent_unknown_stop_and_activity_are_added() {
        let mut state = state_with_main();
        assert!(apply(
            &mut state,
            "idle",
            Some(&sub(SubagentEventKind::Stop, Some("a1"), "Plan")),
            100
        ));
        assert!(apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Activity, Some("a2"), "Explore")),
            100
        ));
        let s = subs(&state);
        assert_eq!(s.len(), 2);
        // Running sorts first.
        assert_eq!(s[0].id, "a2");
        assert_eq!(s[0].status, SubagentStatus::Running);
        assert_eq!(s[1].id, "a1");
        assert_eq!(s[1].status, SubagentStatus::Done);
        // Activity for a known subagent only moves its time: not a broadcast.
        assert!(!apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Activity, Some("a2"), "Explore")),
            150
        ));
        assert_eq!(subs(&state)[0].last_ts, 150);
    }

    #[test]
    fn subagent_empty_type_or_missing_id_is_ignored() {
        let mut state = state_with_main();
        assert!(!apply(
            &mut state,
            "idle",
            Some(&sub(SubagentEventKind::Stop, Some("a1"), "")),
            100
        ));
        assert!(!apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, None, "Plan")),
            100
        ));
        assert!(subs(&state).is_empty());
        assert!(!state.agents.contains_key("main"));
    }

    #[test]
    fn subagent_description_fifo_by_type() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "working",
            Some(&spawn("Explore", "find hooks", None)),
            100,
        );
        apply(
            &mut state,
            "working",
            Some(&spawn("Plan", "plan it", None)),
            100,
        );
        apply(
            &mut state,
            "working",
            Some(&spawn("Explore", "find tests", None)),
            100,
        );
        // The parent `Agent` tool call still applies the parent status.
        assert_eq!(state.agents["main"].status, AgentStatus::Working);
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("p"), "Plan")),
            101,
        );
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("e1"), "Explore")),
            101,
        );
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("e2"), "Explore")),
            102,
        );
        let by_id = |id: &str| {
            subs(&state)
                .into_iter()
                .find(|r| r.id == id)
                .unwrap()
                .description
        };
        assert_eq!(by_id("p").as_deref(), Some("plan it"));
        assert_eq!(by_id("e1").as_deref(), Some("find hooks"));
        assert_eq!(by_id("e2").as_deref(), Some("find tests"));
    }

    #[test]
    fn subagent_stale_pending_description_is_dropped() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "working",
            Some(&spawn("Explore", "old", None)),
            100,
        );
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("e1"), "Explore")),
            100 + PENDING_DESCRIPTION_SECS + 1,
        );
        assert_eq!(subs(&state)[0].description, None);
    }

    #[test]
    fn subagent_description_override_by_agent_id() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "working",
            Some(&spawn("Explore", "first", None)),
            100,
        );
        apply(
            &mut state,
            "working",
            Some(&spawn("Explore", "second", None)),
            100,
        );
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("e2"), "Explore")),
            101,
        );
        // e2 took "first" by FIFO; the background tool response corrects it.
        assert!(apply(
            &mut state,
            "working",
            Some(&spawn("Explore", "second", Some("e2"))),
            102
        ));
        assert_eq!(subs(&state)[0].description.as_deref(), Some("second"));
        // The matching pending entry is gone, so the next start gets nothing.
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("e3"), "Explore")),
            103,
        );
        let e3 = subs(&state).into_iter().find(|r| r.id == "e3").unwrap();
        assert_eq!(e3.description, None);
    }

    /// A background subagent's finish starts a new parent turn, which fires
    /// `UserPromptSubmit`. That parent hook must not cut the done grace period.
    #[test]
    fn parent_hook_keeps_done_subagents() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "idle",
            Some(&sub(SubagentEventKind::Stop, Some("d"), "Plan")),
            100,
        );
        apply(&mut state, "working", None, 101);
        let s2 = subs(&state);
        assert_eq!(s2.len(), 1);
        assert_eq!(s2[0].status, SubagentStatus::Done);
    }

    #[test]
    fn expire_subagents_at_boundaries() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "idle",
            Some(&sub(SubagentEventKind::Stop, Some("d"), "Plan")),
            1000,
        );
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("r"), "Plan")),
            1000,
        );
        assert!(!state.expire_subagents(1000 + SUBAGENT_DONE_GRACE_SECS));
        assert_eq!(subs(&state).len(), 2);
        assert!(state.expire_subagents(1000 + SUBAGENT_DONE_GRACE_SECS + 1));
        assert_eq!(subs(&state).len(), 1);
        assert!(!state.expire_subagents(1000 + SUBAGENT_STALE_SECS));
        assert!(state.expire_subagents(1000 + SUBAGENT_STALE_SECS + 1));
        assert!(subs(&state).is_empty());
    }

    #[test]
    fn remove_row_drops_subagents() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("r"), "Plan")),
            100,
        );
        state.remove_row("main");
        state.rows.insert("main".into(), make_row("main"));
        assert!(subs(&state).is_empty());
    }

    #[test]
    fn scan_removal_drops_subagents() {
        let mut state = state_with_main();
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("r"), "Plan")),
            100,
        );
        state.apply_scanned_rows(HashMap::new());
        state.apply_scanned_rows(HashMap::from([("main".to_string(), make_row("main"))]));
        assert!(subs(&state).is_empty());
    }

    #[tokio::test]
    async fn persist_never_writes_subagents() {
        let dir =
            std::env::temp_dir().join(format!("swamp-subagent-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = state_with_main();
        state.apply_hook("main", "working", None, None).unwrap();
        apply(
            &mut state,
            "working",
            Some(&spawn("Plan", "secret plan", None)),
            100,
        );
        apply(
            &mut state,
            "working",
            Some(&sub(SubagentEventKind::Start, Some("agent-xyz"), "Plan")),
            100,
        );
        state.persist(&dir).await.unwrap();
        let text = std::fs::read_to_string(dir.join(".swamp-status.json")).unwrap();
        assert!(!text.contains("agent-xyz"));
        assert!(!text.contains("secret plan"));
        assert!(!text.contains("subagent"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worktree_row_subagents_serde() {
        // An older peer's row without the field still decodes.
        let mut v = serde_json::to_value(make_row("main")).unwrap();
        assert!(v.get("subagents").is_none(), "empty list is skipped");
        v.as_object_mut().unwrap().remove("subagents");
        let row: WorktreeRow = serde_json::from_value(v).unwrap();
        assert!(row.subagents.is_empty());

        let mut row = make_row("main");
        row.subagents.push(SubagentRow {
            id: "a1".into(),
            agent_type: "Explore".into(),
            description: Some("look".into()),
            status: SubagentStatus::Done,
            started_ts: 1,
            last_ts: 2,
            finished_ts: Some(2),
        });
        let back: WorktreeRow =
            serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        assert_eq!(back.subagents, row.subagents);
    }
}
