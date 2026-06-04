use crate::github::PrSummary;
use crate::util::now_unix;
use crate::worktree::{self, GitInfo, Worktree};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Working,
    Waiting,
    Idle,
}

impl Default for AgentStatus {
    fn default() -> Self {
        AgentStatus::Idle
    }
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
    pub async fn load(_common_dir: &Path) -> Result<Self> {
        Ok(Self {
            rows: HashMap::new(),
            agents: HashMap::new(),
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
            let agent = self.agents.get(&wt.name()).cloned().unwrap_or_default();
            new_rows.insert(wt.name(), build_row(&wt, &info, &agent));
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
        let rec = AgentRecord {
            status: agent_status,
            ts: now_unix(),
            session_name: session,
            session_id: sid,
        };
        self.agents.insert(wt_name.to_string(), rec.clone());
        if let Some(row) = self.rows.get_mut(wt_name) {
            row.agent = rec.status;
            row.agent_ts = rec.ts;
            row.session_name = rec.session_name;
        }
        Ok(())
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
        state.rows.insert("old".into(), make_row_with_ts("old", 100));
        state.rows.insert("newest".into(), make_row_with_ts("newest", 300));
        state.rows.insert("middle".into(), make_row_with_ts("middle", 200));

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
    }
}
