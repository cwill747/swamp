//! Move the user to the Zellij session of a repository, with the tab of one
//! worktree in focus.

use crate::zellij::{self, SessionState, TabInfo};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub common_dir: PathBuf,
    pub session: String,
    pub worktree: String,
    pub path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The worktree tab has the focus in the target session.
    Tab,
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
    ensure_session(ops, target)?;
    let open_tab = ops
        .list_tabs(&target.session)
        .ok()
        .and_then(|tabs| zellij::find_tab(&tabs, &target.worktree).cloned());
    if let Some(tab) = open_tab {
        return match ops.switch_session(&target.session, Some(tab.position)) {
            Ok(()) => Ok(Outcome::Tab),
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
        Ok(()) => Outcome::Tab,
        Err(e) => Outcome::SessionOnly {
            reason: format!("{e:#}"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        live: RefCell<bool>,
        clients: RefCell<usize>,
        tabs: RefCell<Vec<TabInfo>>,
        list_fails: bool,
        open_fails: bool,
        positioned_switch_fails: bool,
        calls: RefCell<Vec<String>>,
    }

    impl Fake {
        fn live(names: &[&str]) -> Self {
            Fake {
                live: RefCell::new(true),
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
        fn settle(&self) {}
    }

    fn target() -> Target {
        Target {
            common_dir: PathBuf::from("/repos/demo/.bare"),
            session: "demo-1a2b".into(),
            worktree: "feature".into(),
            path: PathBuf::from("/repos/demo/feature"),
        }
    }

    #[test]
    fn an_open_tab_gets_the_focus_by_position() {
        let ops = Fake::live(&["dashboard", "main", "feature"]);
        assert_eq!(jump(&ops, &target()).unwrap(), Outcome::Tab);
        assert_eq!(ops.calls(), ["switch demo-1a2b Some(2)"]);
    }

    #[test]
    fn a_missing_tab_is_opened_after_the_client_moves() {
        let ops = Fake::live(&["dashboard"]);
        assert_eq!(jump(&ops, &target()).unwrap(), Outcome::Tab);
        assert_eq!(
            ops.calls(),
            [
                "switch demo-1a2b None",
                "open demo-1a2b /repos/demo/feature feature",
                "focus demo-1a2b 11",
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
    fn a_session_that_does_not_run_is_started_before_the_jump() {
        let ops = Fake {
            live: RefCell::new(false),
            ..Fake::live(&["dashboard", "feature"])
        };
        assert_eq!(jump(&ops, &target()).unwrap(), Outcome::Tab);
        assert_eq!(
            ops.calls(),
            ["start /repos/demo/.bare", "switch demo-1a2b Some(1)"]
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
