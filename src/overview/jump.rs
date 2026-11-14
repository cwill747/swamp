//! Move the user to the Zellij session of a repository, with the tab of one
//! worktree in focus.

use crate::zellij::{self, GeneratedPaneRole, SessionState, TabInfo};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub common_dir: PathBuf,
    pub session: String,
    pub worktree: String,
    pub path: PathBuf,
    /// Captured branch identity from the selected snapshot. Slot paths can be
    /// reused while activation is waiting for Zellij, so validate it again.
    pub expected_branch: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The target tab and its generated agent terminal have focus.
    AgentFocused,
    /// The target tab is active, but its agent pane was unavailable or unsafe
    /// to resolve. Callers must not treat this as a confirmed agent visit.
    TabOnly { reason: String },
    /// The session is the target, but the tab could not be found or opened.
    SessionOnly { reason: String },
}

/// The Zellij operations that a jump needs. Tests replace them.
pub trait Ops {
    fn session_state(&self, session: &str) -> Result<Option<SessionState>>;
    /// Start the session of the repository at `common_dir` with no client.
    fn start_session(&self, common_dir: &Path) -> Result<()>;
    fn list_tabs(&self, session: &str) -> Result<Vec<TabInfo>>;
    /// Number of clients that are attached to `session`.
    fn client_count(&self, session: &str) -> Result<usize>;
    fn open_tab(&self, session: &str, path: &Path, name: &str) -> Result<()>;
    /// Move the client of this pane to `session`.
    fn switch_session(&self, session: &str, tab_position: Option<usize>) -> Result<()>;
    fn go_to_tab(&self, session: &str, tab_id: u32) -> Result<()>;
    /// Focus the generated agent terminal in `tab_id` for this client. False
    /// means discovery could not identify exactly one safe target.
    fn focus_agent(&self, session: &str, tab_id: u32) -> Result<bool>;
    /// Check that the selected path still belongs to this repository/branch.
    /// Implementations may use a captured snapshot in tests; production checks
    /// git only during an explicit activation.
    fn validate_target(&self, _target: &Target) -> Result<bool> {
        Ok(true)
    }
    /// Wait for Zellij to apply a change. Tests do not wait.
    fn settle(&self) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub struct Zellij;

impl Ops for Zellij {
    fn session_state(&self, session: &str) -> Result<Option<SessionState>> {
        zellij::session_state(session)
    }
    fn start_session(&self, common_dir: &Path) -> Result<()> {
        crate::launch::ensure_background_session(common_dir).map(drop)
    }
    fn list_tabs(&self, session: &str) -> Result<Vec<TabInfo>> {
        zellij::list_tabs(session)
    }
    fn client_count(&self, session: &str) -> Result<usize> {
        zellij::client_count(session)
    }
    fn open_tab(&self, session: &str, path: &Path, name: &str) -> Result<()> {
        crate::launch::open_worktree_tab_in(Some(session), path, name)
    }
    fn switch_session(&self, session: &str, tab_position: Option<usize>) -> Result<()> {
        zellij::switch_session(session, tab_position)
    }
    fn go_to_tab(&self, session: &str, tab_id: u32) -> Result<()> {
        zellij::go_to_tab_by_id(session, tab_id)
    }
    fn focus_agent(&self, session: &str, tab_id: u32) -> Result<bool> {
        let Some(pane) = zellij::resolve_generated_pane(session, tab_id, GeneratedPaneRole::Agent)?
        else {
            return Ok(false);
        };
        let origin = std::env::var("ZELLIJ_SESSION_NAME").ok();
        match zellij::pane_focus_target(origin.as_deref(), session, pane.pane_id()) {
            zellij::PaneFocusTarget::CurrentClient(pane_id) => {
                zellij::focus_pane_here(pane_id)?;
                Ok(true)
            }
            zellij::PaneFocusTarget::TargetSession { session, .. }
                if zellij::client_count(&session)? != 1 =>
            {
                Ok(false)
            }
            zellij::PaneFocusTarget::TargetSession { session, .. } => {
                zellij::focus_role_in_session(&session, tab_id, GeneratedPaneRole::Agent)?;
                Ok(true)
            }
        }
    }
    fn validate_target(&self, target: &Target) -> Result<bool> {
        let expected_common = target
            .common_dir
            .canonicalize()
            .unwrap_or_else(|_| target.common_dir.clone());
        let worktrees = match crate::worktree::list_worktrees(&target.common_dir) {
            Ok(worktrees) => worktrees,
            Err(_) => return Ok(false),
        };
        let expected_path = target
            .path
            .canonicalize()
            .unwrap_or_else(|_| target.path.clone());
        Ok(worktrees.into_iter().any(|worktree| {
            let path = worktree
                .path
                .canonicalize()
                .unwrap_or_else(|_| worktree.path.clone());
            path == expected_path
                && worktree.branch
                    == target
                        .expected_branch
                        .as_deref()
                        .unwrap_or(&worktree.branch)
                && crate::worktree::git_common_dir(&worktree.path)
                    .ok()
                    .map(|dir| dir.canonicalize().unwrap_or(dir))
                    .as_ref()
                    == Some(&expected_common)
        }))
    }
}

/// Bring the worktree tab into focus for the client that is attached to the
/// target session. Open the tab when it is absent.
///
/// Zellij cannot lay out a new tab in a session that has no client, because
/// such a session has no display size. This function therefore waits for a
/// client before it opens a tab.
pub fn focus_tab(ops: &dyn Ops, target: &Target) -> Result<()> {
    let mut attached = false;
    for _ in 0..50 {
        if ops.client_count(&target.session)? > 0 {
            attached = true;
            break;
        }
        ops.settle();
    }
    if !attached {
        anyhow::bail!("no client is attached to {}", target.session);
    }
    if !ops.validate_target(target)? {
        anyhow::bail!("selected worktree changed before activation");
    }

    let tabs = ops.list_tabs(&target.session)?;
    let mut tab = zellij::find_tab(&tabs, &target.worktree).cloned();
    if tab.is_none() {
        ops.open_tab(&target.session, &target.path, &target.worktree)?;
        for _ in 0..20 {
            let tabs = ops.list_tabs(&target.session)?;
            tab = zellij::find_tab(&tabs, &target.worktree).cloned();
            if tab.is_some() {
                break;
            }
            ops.settle();
        }
    }
    let Some(tab) = tab else {
        anyhow::bail!("the tab of {} did not open", target.worktree);
    };
    if !ops.validate_target(target)? {
        anyhow::bail!("selected worktree changed before tab focus");
    }
    ops.go_to_tab(&target.session, tab.tab_id)
}

/// Start the target session when it does not run.
///
/// Zellij reports success for an action against a session that does not
/// exist, so each jump checks the session first.
pub fn ensure_session(ops: &dyn Ops, target: &Target) -> Result<()> {
    if ops.session_state(&target.session)? != Some(SessionState::Live) {
        ops.start_session(&target.common_dir)?;
    }
    Ok(())
}

/// Move the client of this pane to the target session, with the worktree tab
/// in focus.
///
/// When the tab is open, one `switch-session` call with the tab position does
/// both steps. When the tab is absent, the client moves first and the tab
/// opens second (see [`focus_tab`]).
pub fn jump(ops: &dyn Ops, target: &Target) -> Result<Outcome> {
    if !ops.validate_target(target)? {
        return Ok(Outcome::SessionOnly {
            reason: "selected worktree changed before activation".into(),
        });
    }
    ensure_session(ops, target)?;
    let open_tab = ops
        .list_tabs(&target.session)
        .ok()
        .and_then(|tabs| zellij::find_tab(&tabs, &target.worktree).cloned());
    if let Some(tab) = open_tab {
        return match ops.switch_session(&target.session, Some(tab.position)) {
            Ok(()) => Ok(focus_agent_after_tab(ops, target, tab.tab_id)),
            Err(e) => {
                ops.switch_session(&target.session, None)?;
                Ok(Outcome::SessionOnly {
                    reason: format!("{e:#}"),
                })
            }
        };
    }
    ops.switch_session(&target.session, None)?;
    Ok(match focus_tab(ops, target) {
        Ok(()) => focus_target(ops, target),
        Err(e) => Outcome::SessionOnly {
            reason: format!("{e:#}"),
        },
    })
}

pub fn focus_target(ops: &dyn Ops, target: &Target) -> Outcome {
    match ops
        .list_tabs(&target.session)
        .ok()
        .and_then(|tabs| zellij::find_tab(&tabs, &target.worktree).map(|tab| tab.tab_id))
    {
        Some(tab_id) => focus_agent_after_tab(ops, target, tab_id),
        None => Outcome::TabOnly {
            reason: "target tab could not be resolved".into(),
        },
    }
}

pub fn focus_target_in_session(target: &Target) -> Result<Outcome> {
    if !Zellij.validate_target(target)? {
        return Ok(Outcome::TabOnly {
            reason: "selected worktree changed after attaching".into(),
        });
    }
    let tabs = zellij::list_tabs(&target.session)?;
    let tab = zellij::find_tab(&tabs, &target.worktree)
        .ok_or_else(|| anyhow::anyhow!("target tab could not be resolved"))?;
    match zellij::focus_role_in_session(&target.session, tab.tab_id, GeneratedPaneRole::Agent) {
        Ok(()) if Zellij.validate_target(target)? => Ok(Outcome::TabOnly {
            reason: "session-targeted focus cannot confirm which attached client received focus"
                .into(),
        }),
        Ok(()) => Ok(Outcome::TabOnly {
            reason: "selected worktree changed during agent focus".into(),
        }),
        Err(error) => Ok(Outcome::TabOnly {
            reason: format!("could not focus agent pane: {error:#}"),
        }),
    }
}

fn focus_agent_after_tab(ops: &dyn Ops, target: &Target, tab_id: u32) -> Outcome {
    match ops.validate_target(target) {
        Ok(true) => {}
        Ok(false) => {
            return Outcome::TabOnly {
                reason: "selected worktree changed before agent focus".into(),
            };
        }
        Err(error) => {
            return Outcome::TabOnly {
                reason: format!("could not verify selected worktree: {error:#}"),
            };
        }
    }
    match ops.focus_agent(&target.session, tab_id) {
        Ok(true) => match ops.validate_target(target) {
            Ok(true) => Outcome::AgentFocused,
            Ok(false) => Outcome::TabOnly {
                reason: "selected worktree changed during agent focus".into(),
            },
            Err(error) => Outcome::TabOnly {
                reason: format!("could not verify selected worktree after focus: {error:#}"),
            },
        },
        Ok(false) => Outcome::TabOnly {
            reason: "agent pane is missing, exited, or ambiguous".into(),
        },
        Err(error) => Outcome::TabOnly {
            reason: format!("could not focus agent pane: {error:#}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct Fake {
        live: RefCell<bool>,
        clients: RefCell<usize>,
        tabs: RefCell<Vec<TabInfo>>,
        list_fails: bool,
        open_fails: bool,
        agent_missing: bool,
        positioned_switch_fails: bool,
        branch_valid: Cell<bool>,
        invalidate_after_focus: bool,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn live(names: &[&str]) -> Self {
            Fake {
                live: RefCell::new(true),
                branch_valid: Cell::new(true),
                tabs: RefCell::new(
                    names
                        .iter()
                        .enumerate()
                        .map(|(i, name)| TabInfo {
                            tab_id: 10 + i as u32,
                            position: i,
                            name: name.to_string(),
                        })
                        .collect(),
                ),
                ..Fake::default()
            }
        }

        /// Each call that changes Zellij state, in order.
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Ops for Fake {
        fn session_state(&self, _: &str) -> Result<Option<SessionState>> {
            Ok(self.live.borrow().then_some(SessionState::Live))
        }
        fn start_session(&self, common_dir: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("start {}", common_dir.display()));
            *self.live.borrow_mut() = true;
            Ok(())
        }
        fn list_tabs(&self, _: &str) -> Result<Vec<TabInfo>> {
            if self.list_fails {
                anyhow::bail!("list failed");
            }
            Ok(self.tabs.borrow().clone())
        }
        fn client_count(&self, _: &str) -> Result<usize> {
            Ok(*self.clients.borrow())
        }
        fn open_tab(&self, session: &str, path: &Path, name: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("open {session} {} {name}", path.display()));
            // Zellij 0.45 cannot lay out a new tab in a session with no client.
            assert!(*self.clients.borrow() > 0, "tab opened with no client");
            if self.open_fails {
                anyhow::bail!("open failed");
            }
            let mut tabs = self.tabs.borrow_mut();
            let position = tabs.len();
            tabs.push(TabInfo {
                tab_id: 10 + position as u32,
                position,
                name: name.to_string(),
            });
            Ok(())
        }
        fn switch_session(&self, session: &str, tab_position: Option<usize>) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("switch {session} {tab_position:?}"));
            if self.positioned_switch_fails && tab_position.is_some() {
                anyhow::bail!("switch failed");
            }
            *self.clients.borrow_mut() = 1;
            Ok(())
        }
        fn go_to_tab(&self, session: &str, tab_id: u32) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("focus {session} {tab_id}"));
            Ok(())
        }
        fn focus_agent(&self, session: &str, tab_id: u32) -> Result<bool> {
            self.calls
                .borrow_mut()
                .push(format!("agent {session} {tab_id}"));
            if self.invalidate_after_focus {
                self.branch_valid.set(false);
            }
            Ok(!self.agent_missing)
        }
        fn validate_target(&self, _: &Target) -> Result<bool> {
            Ok(self.branch_valid.get())
        }
        fn settle(&self) {}
    }

    fn target() -> Target {
        Target {
            common_dir: PathBuf::from("/repos/demo/.bare"),
            session: "demo-1a2b".into(),
            worktree: "feature".into(),
            path: PathBuf::from("/repos/demo/feature"),
            expected_branch: Some("feature".into()),
        }
    }

    #[test]
    fn an_open_tab_gets_the_focus_by_position() {
        let ops = Fake::live(&["dashboard", "main", "feature"]);
        assert_eq!(jump(&ops, &target()).unwrap(), Outcome::AgentFocused);
        assert_eq!(
            ops.calls(),
            ["switch demo-1a2b Some(2)", "agent demo-1a2b 12"]
        );
    }

    #[test]
    fn a_slot_reassigned_before_activation_is_not_opened_or_acknowledged() {
        let ops = Fake::live(&["dashboard", "feature"]);
        ops.branch_valid.set(false);
        assert!(matches!(
            jump(&ops, &target()).unwrap(),
            Outcome::SessionOnly { .. }
        ));
        assert!(ops.calls().is_empty());
    }

    #[test]
    fn a_slot_reassigned_during_focus_is_a_partial_visit() {
        let ops = Fake {
            invalidate_after_focus: true,
            ..Fake::live(&["dashboard", "feature"])
        };
        assert!(matches!(
            jump(&ops, &target()).unwrap(),
            Outcome::TabOnly { .. }
        ));
    }

    #[test]
    fn a_missing_tab_is_opened_after_the_client_moves() {
        let ops = Fake::live(&["dashboard"]);
        assert_eq!(jump(&ops, &target()).unwrap(), Outcome::AgentFocused);
        assert_eq!(
            ops.calls(),
            [
                "switch demo-1a2b None",
                "open demo-1a2b /repos/demo/feature feature",
                "focus demo-1a2b 11",
                "agent demo-1a2b 11",
            ]
        );
    }

    #[test]
    fn a_tab_that_cannot_open_still_moves_the_client_to_the_session() {
        let ops = Fake {
            open_fails: true,
            ..Fake::live(&["dashboard"])
        };
        let outcome = jump(&ops, &target()).unwrap();
        assert!(matches!(outcome, Outcome::SessionOnly { .. }));
        assert_eq!(ops.calls()[0], "switch demo-1a2b None");

        let ops = Fake {
            list_fails: true,
            ..Fake::live(&[])
        };
        let outcome = jump(&ops, &target()).unwrap();
        assert!(matches!(outcome, Outcome::SessionOnly { .. }));
        assert_eq!(ops.calls(), ["switch demo-1a2b None"]);
    }

    #[test]
    fn a_failed_switch_to_a_position_falls_back_to_the_session() {
        let ops = Fake {
            positioned_switch_fails: true,
            ..Fake::live(&["dashboard", "feature"])
        };
        let outcome = jump(&ops, &target()).unwrap();
        assert!(matches!(outcome, Outcome::SessionOnly { .. }));
        assert_eq!(
            ops.calls(),
            ["switch demo-1a2b Some(1)", "switch demo-1a2b None"]
        );
    }

    #[test]
    fn renamed_or_ambiguous_agent_role_is_tab_only_not_a_confirmed_visit() {
        let ops = Fake {
            agent_missing: true,
            ..Fake::live(&["dashboard", "feature"])
        };
        assert!(matches!(
            jump(&ops, &target()).unwrap(),
            Outcome::TabOnly { .. }
        ));
        assert_eq!(
            ops.calls(),
            ["switch demo-1a2b Some(1)", "agent demo-1a2b 11"]
        );
    }

    #[test]
    fn a_session_that_does_not_run_is_started_before_the_jump() {
        let ops = Fake {
            live: RefCell::new(false),
            ..Fake::live(&["dashboard", "feature"])
        };
        assert_eq!(jump(&ops, &target()).unwrap(), Outcome::AgentFocused);
        assert_eq!(
            ops.calls(),
            [
                "start /repos/demo/.bare",
                "switch demo-1a2b Some(1)",
                "agent demo-1a2b 11"
            ]
        );
    }

    #[test]
    fn focus_tab_waits_for_a_client_and_focuses_by_id() {
        // The caller attaches a client. No client moves through Zellij.
        let ops = Fake {
            clients: RefCell::new(1),
            ..Fake::live(&["dashboard", "feature"])
        };
        focus_tab(&ops, &target()).unwrap();
        assert_eq!(ops.calls(), ["focus demo-1a2b 11"]);

        let ops = Fake {
            clients: RefCell::new(1),
            ..Fake::live(&["dashboard"])
        };
        focus_tab(&ops, &target()).unwrap();
        assert_eq!(
            ops.calls(),
            [
                "open demo-1a2b /repos/demo/feature feature",
                "focus demo-1a2b 11"
            ]
        );

        // No client attaches: nothing is opened.
        let ops = Fake::live(&["dashboard"]);
        assert!(focus_tab(&ops, &target()).is_err());
        assert!(ops.calls().is_empty());
    }
}
