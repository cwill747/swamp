use crate::config::Harness;
use crate::github::PrSummary;
use crate::util::now_unix;
use crate::worktree::{
    self, GitInfo, RemovalVerdict, RemoveRefusedReason, VerdictContext, Worktree,
};
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
    /// The reason a non-forced removal of this worktree would be refused, or
    /// `None` when it is removable. Computed during the scan (see
    /// `scan_worktrees`) so the TUI can show the reason in the first delete
    /// confirmation instead of waiting on a daemon round-trip.
    /// `#[serde(default)]` keeps an older peer's snapshot loadable — it
    /// decodes as `None`, the safe "don't know, assume removable" reading.
    #[serde(default)]
    pub removal_block: Option<RemoveRefusedReason>,
    /// True while a removal of this worktree is in flight, set before
    /// `repo_ops` is acquired so the whole wait is visible in every
    /// subscribed TUI. `#[serde(default)]` keeps an older peer's snapshot
    /// loadable.
    #[serde(default)]
    pub deleting: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: Vec<WorktreeRow>,
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
        let removed: Vec<&str> = self
            .rows
            .keys()
            .filter(|k| !new_rows.contains_key(*k))
            .map(String::as_str)
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
                row
            })
            .collect();
        rows.sort_by(|a, b| b.head_ts.cmp(&a.head_ts).then(a.name.cmp(&b.name)));
        Snapshot { rows }
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

    /// The PR map [`scan_worktrees`] should trust for the merged-branch
    /// removal signal: `None` while the first fetch hasn't resolved yet or
    /// the most recent one failed, even though `self.prs` may still hold a
    /// stale-but-valid map for display. A cached "was merged" shouldn't
    /// unlock a destructive squash-merge deletion during an outage — see
    /// design decision 3 in the `better-worktree-deletion` change.
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
    prs: Option<&HashMap<String, PrSummary>>,
) -> Result<HashMap<String, WorktreeRow>> {
    let wts = worktree::list_worktrees(common_dir)?;
    if wts.is_empty() {
        return Ok(HashMap::new());
    }

    // Resolved once per scan (not once per worktree): it's the same tip
    // regardless of which worktree's removal verdict is being computed.
    let default_branch_tip = worktree::default_branch_tip(common_dir);

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
                    let (info, status_error) = match worktree::git_info(&wt.path) {
                        Ok(info) => (info, false),
                        Err(error) => {
                            tracing::warn!(
                                worktree = %wt.name(),
                                %error,
                                "worktree status read failed"
                            );
                            (GitInfo::default(), true)
                        }
                    };
                    let name = wt.name();
                    let agent = agents.get(&name).cloned().unwrap_or_default();
                    let pr = prs.and_then(|m| m.get(&info.branch));
                    let pr_state = pr.map(|pr| pr.state.clone());
                    let pr_head_oid = pr
                        .and_then(|pr| pr.head_oid.as_deref())
                        .and_then(|oid| oid.parse().ok());
                    let ctx = VerdictContext {
                        git_info: info.clone(),
                        default_branch: default_branch.to_string(),
                        default_branch_tip,
                        pr_state,
                        pr_head_oid,
                    };
                    let verdict = if status_error {
                        RemovalVerdict::Blocked(RemoveRefusedReason::StatusUnreadable)
                    } else {
                        worktree::removal_verdict(common_dir, &name, &ctx)
                    };
                    let removal_block = verdict.blocking_reason().cloned();
                    let row = build_row(wt, &info, &agent, default_branch, removal_block);
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
    removal_block: Option<RemoveRefusedReason>,
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
        removal_block,
        // Projected from `DaemonState::deleting` by `snapshot()`, not known
        // at scan time — a scan and a delete can race, and the scan must not
        // clobber an in-flight delete's mark.
        deleting: false,
    }
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
        let main = build_row(&worktree("main", "main"), &info_main, &agent, "main", None);
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
            None,
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
        let row = build_row(&worktree("main", "main"), &info, &agent, "", None);
        assert!(
            !row.is_default,
            "no row may be flagged when the default branch is unknown"
        );
    }

    /// `scan_worktrees` computes a per-row removal verdict during the scan: a
    /// dirty worktree's row carries the dirty blocking reason; a clean
    /// worktree whose branch matches another branch's tip (nothing to
    /// orphan) carries none.
    #[test]
    fn scan_worktrees_reports_removal_block() {
        use crate::worktree::test_support::{git_available, setup};
        use crate::worktree::{create_worktree, create_worktree_from_base};

        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let dirty = create_worktree(&bare, "feature").unwrap();
        std::fs::write(dirty.path.join("scratch.txt"), "wip").unwrap();
        // Cut with no further commits, so its tip matches "main"'s — reachable
        // from another branch, nothing would be orphaned by deleting it.
        create_worktree_from_base(&bare, "clean", "main").unwrap();

        let rows = scan_worktrees(&bare, &HashMap::new(), "", None).unwrap();

        assert_eq!(
            rows.get("feature").unwrap().removal_block,
            Some(RemoveRefusedReason::Dirty),
            "a dirty worktree's row must carry the dirty reason"
        );
        assert_eq!(
            rows.get("clean").unwrap().removal_block,
            None,
            "a clean, already-merged worktree's row must carry no reason"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_worktrees_reports_unreadable_status() {
        use crate::worktree::create_worktree;
        use crate::worktree::test_support::{git_available, setup};
        use std::process::Command;

        if !git_available() {
            return;
        }
        let (root, bare) = setup();
        let wt = create_worktree(&bare, "feature").unwrap();
        let output = Command::new("git")
            .arg("-C")
            .arg(&wt.path)
            .args(["rev-parse", "--git-dir"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let git_dir = String::from_utf8(output.stdout).unwrap();
        std::fs::write(wt.path.join(git_dir.trim()).join("index"), "not an index").unwrap();

        let rows = scan_worktrees(&bare, &HashMap::new(), "main", None).unwrap();
        assert_eq!(
            rows.get("feature").unwrap().removal_block,
            Some(RemoveRefusedReason::StatusUnreadable),
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
}
