use crate::config::Harness;
use crate::github::PrSummary;
use crate::util::now_unix;
use crate::worktree::{self, GitInfo, Worktree};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: Vec<WorktreeRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrSnapshot {
    pub prs: HashMap<String, PrSummary>,
}

pub struct DaemonState {
    pub rows: HashMap<String, WorktreeRow>,
    pub agents: HashMap<String, AgentRecord>,
    pub prs: HashMap<String, PrSummary>,
}

impl DaemonState {
    pub async fn load(common_dir: &Path) -> Result<Self> {
        // Hydrate the agent records persisted by a prior run. `persist` rewrites
        // the whole `agents` map, so without this an empty in-memory map would
        // clobber other worktrees' session ids / harness overrides the first
        // time any record changes (a hook ping or `set_harness`).
        let agents = load_agents(common_dir).await;
        Ok(Self {
            rows: HashMap::new(),
            agents,
            prs: HashMap::new(),
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

    pub fn refresh_git(&mut self, common_dir: &Path) -> Result<()> {
        let wts = worktree::list_worktrees(common_dir)?;
        let mut new_rows = HashMap::new();
        for wt in wts {
            let info = worktree::git_info(&wt.path).unwrap_or_default();
            let name = wt.name();
            let agent = self.agents.get(&name).cloned().unwrap_or_default();
            let row = build_row(&wt, &info, &agent);
            tracing::trace!(
                worktree = %name,
                branch = %row.branch,
                ahead = row.ahead,
                behind = row.behind,
                dirty = row.staged + row.unstaged + row.untracked,
                "scanned worktree"
            );
            new_rows.insert(name, row);
        }
        // Report worktree set changes (the "why did a tab appear?" signal) at
        // info; a no-change refresh is just debug noise.
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
        Ok(())
    }

    pub fn apply_hook(
        &mut self,
        wt_name: &str,
        status: &str,
        session_name: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<()> {
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
        Ok(())
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

    pub fn snapshot(&self) -> Snapshot {
        let mut rows: Vec<WorktreeRow> = self.rows.values().cloned().collect();
        rows.sort_by(|a, b| b.head_ts.cmp(&a.head_ts).then(a.name.cmp(&b.name)));
        Snapshot { rows }
    }

    pub fn update_prs(&mut self, prs: HashMap<String, PrSummary>) {
        self.prs = prs;
    }

    pub fn pr_snapshot(&self) -> PrSnapshot {
        PrSnapshot {
            prs: self.prs.clone(),
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

fn build_row(wt: &Worktree, info: &GitInfo, agent: &AgentRecord) -> WorktreeRow {
    let branch = if info.branch.is_empty() || info.branch == "(detached)" {
        wt.branch.clone()
    } else {
        info.branch.clone()
    };
    WorktreeRow {
        name: wt.name(),
        path: wt.path.clone(),
        branch,
        upstream: info.upstream.clone(),
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
        }
    }

    /// With equal head_ts, snapshot falls back to alphabetical name order.
    #[test]
    fn snapshot_rows_sorted_by_name_when_same_ts() {
        let mut state = DaemonState {
            rows: HashMap::new(),
            agents: HashMap::new(),
            prs: HashMap::new(),
        };
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
        let mut state = DaemonState {
            rows: HashMap::new(),
            agents: HashMap::new(),
            prs: HashMap::new(),
        };
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
        let mut state = DaemonState {
            rows: HashMap::new(),
            agents: HashMap::new(),
            prs: HashMap::new(),
        };
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
        let mut state = DaemonState {
            rows: HashMap::new(),
            agents: HashMap::new(),
            prs: HashMap::new(),
        };
        state.rows.insert("main".into(), make_row("main"));

        // "ghost" does not exist in rows; apply_hook must not crash.
        state.apply_hook("ghost", "working", None, None).unwrap();
        let snap = state.snapshot();
        // "main" row is untouched; no new row for "ghost".
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.rows[0].name, "main");
        assert_eq!(snap.rows[0].agent, AgentStatus::Idle);
    }

    /// A session id is recorded on the agent record and preserved across a
    /// later hook that omits it — so later `working`/`idle` pings don't wipe
    /// the id we need to resume the session (#33).
    #[test]
    fn apply_hook_records_and_preserves_session_id() {
        let mut state = DaemonState {
            rows: HashMap::new(),
            agents: HashMap::new(),
            prs: HashMap::new(),
        };

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
        let mut state = DaemonState {
            rows: HashMap::new(),
            agents: HashMap::new(),
            prs: HashMap::new(),
        };
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
}
