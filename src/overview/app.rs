//! State and keyboard input of the cross-repo overview.

use super::attention::{self, Item, RepoState};
use super::jump::Target;
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::discover::RepoIdentity;
use crate::fanin;
use crate::registry::RepoEntry;
use crate::util::{repo_id, repo_name_for};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const RESTART_HINT: &str = "Restart that swamp session to enable jump";

/// One live daemon.
#[derive(Debug, Default)]
struct Repo {
    identity: Option<RepoIdentity>,
    snapshot: Snapshot,
    prs: PrSnapshot,
}

/// A registered repository with no live daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdRepo {
    pub name: String,
    pub entry: RepoEntry,
}

/// What the event loop must do after a key.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    Jump(Target),
    Launch(RepoEntry),
}

/// Identity of the selected row, which keeps the selection on the same row
/// when the list changes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RowKey {
    Item { repo_id: String, worktree: String },
    Cold(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub text: String,
    pub error: bool,
}

#[derive(Debug, Default)]
pub struct App {
    repos: BTreeMap<String, Repo>,
    /// Registered repositories, each with its `repo_id`.
    registry: Vec<(String, RepoEntry)>,
    pub items: Vec<Item>,
    pub cold: Vec<ColdRepo>,
    /// Index into the items, then the cold repositories.
    pub selected: usize,
    pub status: Option<Status>,
}

impl App {
    pub fn live_count(&self) -> usize {
        self.repos.len()
    }

    /// Sockets of the live daemons whose repository is unknown.
    pub fn unidentified_count(&self) -> usize {
        self.repos
            .values()
            .filter(|repo| repo.identity.is_none())
            .count()
    }

    pub fn row_count(&self) -> usize {
        self.items.len() + self.cold.len()
    }

    /// Whether the item can be the target of a jump.
    pub fn can_jump(&self, item: &Item) -> bool {
        self.repos
            .get(&item.repo_id)
            .is_some_and(|repo| repo.identity.is_some())
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    pub fn apply(&mut self, event: fanin::Event) {
        match event {
            fanin::Event::Up { id, identity, .. } => {
                self.repos.entry(id).or_default().identity = identity;
            }
            fanin::Event::Snapshot { id, snapshot } => {
                self.repos.entry(id).or_default().snapshot = snapshot;
            }
            fanin::Event::PrStatus { id, prs } => {
                self.repos.entry(id).or_default().prs = prs;
            }
            fanin::Event::Down { id } => {
                self.repos.remove(&id);
            }
        }
        self.recompute();
    }

    pub fn set_registry(&mut self, entries: Vec<RepoEntry>) {
        self.registry = entries
            .into_iter()
            .map(|entry| (repo_id(&entry.common_dir), entry))
            .collect();
        self.recompute();
    }

    fn row_key(&self, index: usize) -> Option<RowKey> {
        if let Some(item) = self.items.get(index) {
            return Some(RowKey::Item {
                repo_id: item.repo_id.clone(),
                worktree: item.worktree.clone(),
            });
        }
        self.cold
            .get(index - self.items.len())
            .map(|cold| RowKey::Cold(cold.entry.common_dir.clone()))
    }

    fn recompute(&mut self) {
        let selected = self.row_key(self.selected);

        let names: BTreeMap<&str, String> = self
            .repos
            .iter()
            .map(|(id, repo)| {
                let name = match &repo.identity {
                    Some(identity) => repo_name_for(&identity.common_dir),
                    // The socket ID is the only name that is known.
                    None => id.clone(),
                };
                (id.as_str(), name)
            })
            .collect();
        let states: Vec<RepoState<'_>> = self
            .repos
            .iter()
            .map(|(id, repo)| RepoState {
                id,
                name: &names[id.as_str()],
                snapshot: &repo.snapshot,
                prs: &repo.prs,
            })
            .collect();
        self.items = attention::rank(&states);

        let mut cold: Vec<ColdRepo> = self
            .registry
            .iter()
            .filter(|(id, _)| !self.repos.contains_key(id))
            .map(|(_, entry)| ColdRepo {
                name: repo_name_for(&entry.common_dir),
                entry: entry.clone(),
            })
            .collect();
        cold.sort_by(|a, b| (&a.name, &a.entry.common_dir).cmp(&(&b.name, &b.entry.common_dir)));
        self.cold = cold;

        let rows = self.row_count();
        self.selected = selected
            .and_then(|key| (0..rows).find(|&i| self.row_key(i).as_ref() == Some(&key)))
            .unwrap_or(self.selected)
            .min(rows.saturating_sub(1));
    }

    fn move_selection(&mut self, delta: isize) {
        let last = self.row_count().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    fn activate(&mut self) -> Action {
        if let Some(item) = self.items.get(self.selected) {
            let identity = self
                .repos
                .get(&item.repo_id)
                .and_then(|repo| repo.identity.as_ref());
            let Some(identity) = identity else {
                self.error(RESTART_HINT);
                return Action::None;
            };
            return Action::Jump(Target {
                common_dir: identity.common_dir.clone(),
                session: identity.session_name.clone(),
                worktree: item.worktree.clone(),
                path: item.path.clone(),
            });
        }
        match self.cold.get(self.selected - self.items.len()) {
            Some(cold) => Action::Launch(cold.entry.clone()),
            None => Action::None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        self.status = None;
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
                Action::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.selected = 0;
                Action::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.selected = self.row_count().saturating_sub(1);
                Action::None
            }
            KeyCode::Enter => self.activate(),
            KeyCode::Char('r') => Action::Refresh,
            _ => Action::None,
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::daemon::state::{AgentStatus, WorktreeRow};
    use crate::discover::test_support::row;
    use std::path::Path;

    pub(crate) fn identity(name: &str) -> RepoIdentity {
        RepoIdentity {
            common_dir: PathBuf::from(format!("/repos/{name}/.bare")),
            session_name: format!("{name}-0000"),
            version: Some("9.9.9".into()),
        }
    }

    pub(crate) fn waiting(repo: &str, name: &str, ts: u64) -> WorktreeRow {
        WorktreeRow {
            agent: AgentStatus::Waiting,
            agent_ts: ts,
            ..row(name, &Path::new("/repos").join(repo).join(name))
        }
    }

    pub(crate) fn entry(name: &str) -> RepoEntry {
        RepoEntry {
            common_dir: PathBuf::from(format!("/repos/{name}/.bare")),
            session_name: format!("{name}-0000"),
            last_launched: 0,
        }
    }

    /// Add a live daemon with socket ID `id` to `app`.
    pub(crate) fn add_repo(
        app: &mut App,
        id: &str,
        identity: Option<RepoIdentity>,
        rows: Vec<WorktreeRow>,
    ) {
        app.apply(fanin::Event::Up {
            id: id.into(),
            sock: PathBuf::from(format!("/run/swamp/{id}.sock")),
            identity,
        });
        app.apply(fanin::Event::Snapshot {
            id: id.into(),
            snapshot: Snapshot {
                rows,
                default_known: true,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Two items from two live repositories and two cold repositories.
    fn app() -> App {
        let mut app = App::default();
        add_repo(
            &mut app,
            "a",
            Some(identity("alpha")),
            vec![waiting("alpha", "one", 10)],
        );
        add_repo(
            &mut app,
            "b",
            Some(identity("beta")),
            vec![waiting("beta", "two", 20)],
        );
        app.set_registry(vec![entry("zulu"), entry("yankee")]);
        app
    }

    #[test]
    fn selection_moves_from_the_items_into_the_cold_repositories() {
        let mut app = app();
        assert_eq!(app.items.len(), 2);
        assert_eq!(
            app.cold.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["yankee", "zulu"]
        );
        assert_eq!(app.selected, 0);

        assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 2);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Launch(entry("yankee"))
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 3);
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.selected, 0);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.selected, 3);
    }

    #[test]
    fn enter_on_an_item_returns_the_jump_target() {
        let mut app = app();
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Jump(Target {
                common_dir: PathBuf::from("/repos/alpha/.bare"),
                session: "alpha-0000".into(),
                worktree: "one".into(),
                path: PathBuf::from("/repos/alpha/one"),
            })
        );
    }

    #[test]
    fn quit_and_refresh_are_reported_to_the_event_loop() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Quit);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        assert_eq!(press(&mut app, KeyCode::Char('r')), Action::Refresh);
        assert_eq!(press(&mut app, KeyCode::Char('x')), Action::None);
    }

    #[test]
    fn keys_on_an_empty_list_do_nothing() {
        let mut app = App::default();
        for code in [
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Char('G'),
            KeyCode::Enter,
        ] {
            assert_eq!(press(&mut app, code), Action::None);
            assert_eq!(app.selected, 0);
        }
    }

    #[test]
    fn an_unidentified_repo_shows_its_items_and_refuses_a_jump() {
        let mut app = App::default();
        add_repo(
            &mut app,
            "feedfacefeedface",
            None,
            vec![waiting("x", "one", 1)],
        );
        assert_eq!(app.unidentified_count(), 1);
        assert_eq!(app.items[0].repo, "feedfacefeedface");
        assert!(!app.can_jump(&app.items[0]));

        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(app.status.as_ref().unwrap().text, RESTART_HINT);

        // A later identification enables the jump.
        app.apply(fanin::Event::Up {
            id: "feedfacefeedface".into(),
            sock: PathBuf::new(),
            identity: Some(identity("alpha")),
        });
        assert_eq!(app.items[0].repo, "alpha");
        assert!(matches!(press(&mut app, KeyCode::Enter), Action::Jump(_)));
    }

    #[test]
    fn a_daemon_that_stops_becomes_a_cold_repo_when_it_is_registered() {
        let mut app = App::default();
        let alpha = entry("alpha");
        let id = repo_id(&alpha.common_dir);
        app.set_registry(vec![alpha.clone()]);
        assert_eq!(app.cold.len(), 1);

        add_repo(
            &mut app,
            &id,
            Some(identity("alpha")),
            vec![waiting("alpha", "one", 1)],
        );
        assert_eq!(app.items.len(), 1);
        assert!(app.cold.is_empty());

        app.apply(fanin::Event::Down { id });
        assert!(app.items.is_empty());
        assert_eq!(app.cold[0].entry, alpha);
        assert_eq!(app.live_count(), 0);
    }

    #[test]
    fn the_selection_follows_its_row_when_the_list_changes() {
        let mut app = app();
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.items[app.selected].worktree, "two");

        // A new item sorts in front of the selected one.
        add_repo(
            &mut app,
            "c",
            Some(identity("gamma")),
            vec![waiting("gamma", "zero", 1)],
        );
        assert_eq!(app.items[app.selected].worktree, "two");
        assert_eq!(app.selected, 2);

        // The selected item goes away, and the selection stays in range.
        app.apply(fanin::Event::Down { id: "b".into() });
        app.apply(fanin::Event::Down { id: "a".into() });
        app.apply(fanin::Event::Down { id: "c".into() });
        app.set_registry(Vec::new());
        assert_eq!(app.row_count(), 0);
        assert_eq!(app.selected, 0);
    }
}
