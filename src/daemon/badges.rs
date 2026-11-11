//! Keep the Zellij tab names of the repo session in step with the attention
//! state: a badge on the tab of each worktree whose agent waits, and a count
//! on the dashboard tab for waiting worktrees in other repositories.
//!
//! The daemon is the only writer of tab names in its session. It resolves tab
//! IDs again at each reconcile, because a session resurrection assigns new
//! IDs.

use super::Daemon;
use super::state::{AgentStatus, Snapshot};
use crate::badge::{self, Inputs};
use crate::zellij::{self, SessionState, TabInfo};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Interval of the reconcile that runs with no state change. It repairs tab
/// names after a session resurrection or a manual rename.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

/// The Zellij operations that a reconcile needs. Tests replace them.
pub trait TabOps {
    /// The tabs of the session, or `None` when the session does not run.
    fn list_tabs(&self) -> Result<Option<Vec<TabInfo>>>;
    fn rename_tab(&self, tab_id: u32, name: &str) -> Result<()>;
}

struct SessionTabs {
    session: String,
}

impl TabOps for SessionTabs {
    fn list_tabs(&self) -> Result<Option<Vec<TabInfo>>> {
        // Zellij reports success for an action against a session that does
        // not exist, so check the session first.
        if zellij::session_state(&self.session)? != Some(SessionState::Live) {
            return Ok(None);
        }
        zellij::list_tabs(&self.session).map(Some)
    }
    fn rename_tab(&self, tab_id: u32, name: &str) -> Result<()> {
        zellij::rename_tab_by_id(&self.session, tab_id, name)
    }
}

/// The badge inputs of a snapshot. A worktree with a removal in flight does
/// not count as waiting.
pub fn inputs_from(snapshot: &Snapshot, external: usize) -> Inputs {
    Inputs {
        worktrees: snapshot.rows.iter().map(|row| row.name.clone()).collect(),
        waiting: snapshot
            .rows
            .iter()
            .filter(|row| row.agent == AgentStatus::Waiting && !row.deleting)
            .map(|row| row.name.clone())
            .collect(),
        external,
    }
}

/// Rename each tab whose name does not agree with `inputs`. Returns the
/// number of tabs that were renamed.
///
/// A failure is logged and otherwise ignored: a daemon can run with no Zellij
/// session, and a tab can close between the list and the rename.
pub fn reconcile(ops: &dyn TabOps, inputs: &Inputs) -> usize {
    let tabs = match ops.list_tabs() {
        Ok(Some(tabs)) => tabs,
        Ok(None) => return 0,
        Err(e) => {
            tracing::debug!("badges: list tabs failed: {e:#}");
            return 0;
        }
    };
    let mut renamed = 0;
    for (tab_id, name) in badge::plan_renames(&tabs, inputs, badge::glyph()) {
        match ops.rename_tab(tab_id, &name) {
            Ok(()) => renamed += 1,
            Err(e) => tracing::debug!(tab_id, name, "badges: rename failed: {e:#}"),
        }
    }
    renamed
}

async fn reconcile_session(session: &str, inputs: Inputs) {
    let ops = SessionTabs {
        session: session.to_string(),
    };
    let _ = tokio::task::spawn_blocking(move || reconcile(&ops, &inputs)).await;
}

/// Run the reconciler until the daemon stops.
///
/// A reconcile runs when the badge inputs change and on each
/// [`RECONCILE_INTERVAL`]. The daemon does not start this task when the
/// configuration turns badges off, so that swamp then renames no tab.
pub async fn run(daemon: Arc<Daemon>) {
    let mut broadcasts = daemon.tx.subscribe();
    let mut applied: Option<Inputs> = None;
    let mut timer = tokio::time::interval(RECONCILE_INTERVAL);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let timer_fired = tokio::select! {
            _ = timer.tick() => true,
            _ = daemon.badge_wake.notified() => false,
            received = broadcasts.recv() => match received {
                Ok(_) | Err(RecvError::Lagged(_)) => false,
                Err(RecvError::Closed) => return,
            },
        };
        let snapshot = daemon.state.read().await.snapshot();
        let inputs = inputs_from(&snapshot, daemon.external_attention());
        if timer_fired || applied.as_ref() != Some(&inputs) {
            reconcile_session(&daemon.session_name, inputs.clone()).await;
            applied = Some(inputs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::WorktreeRow;
    use crate::discover::test_support::row;
    use std::cell::RefCell;
    use std::path::Path;

    struct Fake {
        tabs: Option<Vec<TabInfo>>,
        fail_rename_of: Option<u32>,
        renames: RefCell<Vec<(u32, String)>>,
    }

    impl Fake {
        fn new(names: &[&str]) -> Self {
            Fake {
                tabs: Some(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, name)| TabInfo {
                            tab_id: i as u32,
                            position: i,
                            name: name.to_string(),
                        })
                        .collect(),
                ),
                fail_rename_of: None,
                renames: RefCell::default(),
            }
        }
    }

    impl TabOps for Fake {
        fn list_tabs(&self) -> Result<Option<Vec<TabInfo>>> {
            Ok(self.tabs.clone())
        }
        fn rename_tab(&self, tab_id: u32, name: &str) -> Result<()> {
            if self.fail_rename_of == Some(tab_id) {
                anyhow::bail!("rename failed");
            }
            self.renames.borrow_mut().push((tab_id, name.to_string()));
            Ok(())
        }
    }

    fn wt(name: &str, agent: AgentStatus) -> WorktreeRow {
        WorktreeRow {
            agent,
            ..row(name, Path::new("/repo").join(name).as_path())
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            rows: vec![
                wt("main", AgentStatus::Idle),
                wt("feature", AgentStatus::Waiting),
                wt("busy", AgentStatus::Working),
                WorktreeRow {
                    deleting: true,
                    ..wt("going", AgentStatus::Waiting)
                },
            ],
            default_known: true,
        }
    }

    #[test]
    fn inputs_count_only_waiting_worktrees_that_stay() {
        let inputs = inputs_from(&snapshot(), 2);
        assert_eq!(inputs.worktrees.len(), 4);
        assert_eq!(
            inputs
                .waiting
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["feature"]
        );
        assert_eq!(inputs.external, 2);
    }

    #[test]
    fn reconcile_renames_by_tab_id() {
        let ops = Fake::new(&["dashboard", "main", "feature", "● busy"]);
        let renamed = reconcile(&ops, &inputs_from(&snapshot(), 1));
        assert_eq!(renamed, 3);
        let glyph = badge::glyph();
        assert_eq!(
            *ops.renames.borrow(),
            [
                (0, format!("dashboard {glyph}1")),
                (2, format!("{glyph} feature")),
                (3, "busy".to_string()),
            ]
        );
    }

    #[test]
    fn a_rename_failure_does_not_stop_the_other_renames() {
        let ops = Fake {
            fail_rename_of: Some(0),
            ..Fake::new(&["dashboard", "feature"])
        };
        let renamed = reconcile(&ops, &inputs_from(&snapshot(), 1));
        assert_eq!(renamed, 1);
        assert_eq!(ops.renames.borrow().len(), 1);
        assert_eq!(ops.renames.borrow()[0].0, 1);
    }

    #[test]
    fn a_daemon_with_no_session_renames_nothing() {
        let ops = Fake {
            tabs: None,
            ..Fake::new(&[])
        };
        assert_eq!(reconcile(&ops, &inputs_from(&snapshot(), 1)), 0);
        assert!(ops.renames.borrow().is_empty());
    }
}
