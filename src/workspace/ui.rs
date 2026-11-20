//! Cached workspace presentation and input. No filesystem or terminal actions.
use super::remote::{Host, HostStatus};
use super::seen::{SeenState, SemanticState};
use super::selection::SharedSelection;
use super::{Connection, Destination, DestinationKey, Repository};
use crate::cli::TuiView;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use std::collections::{BTreeMap, BTreeSet};
use unicode_width::UnicodeWidthChar;

// ANSI theme tokens also work without truecolor. The canvas inherits the
// terminal background; only keyboard selection has a background accent.
struct Theme;
impl Theme {
    const ACCENT: Color = Color::Cyan;
    const WAITING: Color = Color::Yellow;
    const WORKING: Color = Color::Green;
    const DONE: Color = Color::Blue;
    const MUTED: Color = Color::DarkGray;
    const SELECTED: Color = Color::Indexed(236);
    const FAILURE: Color = Color::Red;
    const TRUNK: Color = Color::Indexed(178);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Section {
    #[default]
    Spaces,
    Agents,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Waiting,
    Working,
    Done,
    Idle,
}
impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Waiting => "Waiting",
            Self::Working => "Working",
            Self::Done => "Done",
            Self::Idle => "Idle",
        }
    }
    fn matches(self, state: SemanticState) -> bool {
        self == Self::All || self.label() == state.label()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Navigation,
    Search,
    Filters,
    Help,
    Details,
    More,
    ConfirmKill,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Activate,
    Seen,
    Quit,
    Local(char),
}

#[derive(Clone)]
pub struct Entry {
    pub repo: String,
    pub destination: Option<Destination>,
    pub key: Option<DestinationKey>,
    pub primary: String,
    pub secondary: String,
    pub state: SemanticState,
    pub child: bool,
    pub header: bool,
}
impl Entry {
    fn identity(&self) -> (String, Option<DestinationKey>) {
        (self.repo.clone(), self.key.clone())
    }
}

#[derive(Default)]
pub struct App {
    pub ascii: bool,
    /// Generated Zellij panes already have a native split gutter.
    pub embedded: bool,
    pub repositories: Vec<Repository>,
    pub hosts: Vec<Host>,
    pub seen: SeenState,
    pub section: Section,
    pub mode: Mode,
    pub filter: Filter,
    pub query: String,
    pub collapsed: BTreeSet<String>,
    pub selected: Option<(String, Option<DestinationKey>)>,
    pub index: usize,
    pub scroll: [usize; 2],
    pub detail_scroll: u16,
    pub active_path: Option<std::path::PathBuf>,
    pub status: String,
    pub connection: String,
    pub resource_lines: Vec<String>,
    pub hits: Vec<(Rect, Entry, bool, Section)>,
    pub section_hits: Vec<(Rect, Section)>,
    pub section_areas: Vec<(Rect, Section)>,
    pub detail_area: Option<Rect>,
    pub activating: bool,
}

fn state_rank(s: SemanticState) -> u8 {
    match s {
        SemanticState::Waiting => 0,
        SemanticState::Done => 1,
        SemanticState::Working => 2,
        SemanticState::Idle => 3,
        SemanticState::Unknown => 4,
    }
}
fn stale_remote(repo: &Repository) -> bool {
    repo.host.is_some() && repo.connection == Connection::Disconnected
}

fn connection_label(c: Connection) -> &'static str {
    match c {
        Connection::Loading => "Loading",
        Connection::Live => "Live",
        Connection::Disconnected => "Disconnected (stale)",
        Connection::Stopped => "Not running",
        Connection::Unidentified => "Unidentified; jump disabled",
    }
}

impl App {
    fn matches(&self, repo: &Repository, d: &Destination, child: Option<&str>) -> bool {
        let state = self.seen.derive(&repo.id, &d.row);
        self.matches_in_state(repo, d, child, state)
    }
    fn child_matches(
        &self,
        repo: &Repository,
        d: &Destination,
        child: &crate::daemon::state::SubagentRow,
    ) -> bool {
        let state = match child.status {
            crate::daemon::state::SubagentStatus::Running => SemanticState::Working,
            crate::daemon::state::SubagentStatus::Done => SemanticState::Done,
        };
        self.matches_in_state(
            repo,
            d,
            child.description.as_deref().or(Some(&child.agent_type)),
            state,
        )
    }
    fn matches_in_state(
        &self,
        repo: &Repository,
        d: &Destination,
        child: Option<&str>,
        state: SemanticState,
    ) -> bool {
        let text = format!(
            "{} {} {} {} {} {}",
            d.title,
            repo.label,
            d.row.branch,
            d.row.path.display(),
            d.row.harness.map(|h| h.label()).unwrap_or(""),
            child.unwrap_or("")
        )
        .to_lowercase();
        self.filter.matches(state) && text.contains(&self.query.to_lowercase())
    }
    fn destination_entry(&self, repo: &Repository, d: Destination) -> Entry {
        let state = self.seen.derive(&repo.id, &d.row);
        let secondary = if matches!(
            d.row.slot_state,
            Some(crate::worktree::SlotState::Free | crate::worktree::SlotState::Pinned)
        ) {
            format!("{} / {}", repo.label, d.row.name)
        } else {
            format!(
                "{} / {}{}{}",
                repo.label,
                d.row.branch,
                d.row
                    .harness
                    .map(|h| format!(" / {}", h.label()))
                    .unwrap_or_default(),
                if d.row.is_default { " / trunk" } else { "" }
            )
        };
        Entry {
            repo: repo.id.clone(),
            key: Some(d.key.clone()),
            primary: d.title.clone(),
            secondary: if stale_remote(repo) {
                format!("{secondary} / stale")
            } else {
                secondary
            },
            destination: Some(d),
            state,
            child: false,
            header: false,
        }
    }
    pub fn entries(&self, section: Section) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut remote_rows = BTreeMap::<&str, Vec<Entry>>::new();
        for repo in &self.repositories {
            let out = match (section, repo.host.as_deref()) {
                (Section::Spaces, Some(host)) => remote_rows.entry(host).or_default(),
                _ => &mut out,
            };
            let destinations = repo.destinations();
            if section == Section::Spaces {
                let matching: Vec<_> = destinations
                    .into_iter()
                    .filter(|d| {
                        self.matches(repo, d, None)
                            || d.row
                                .subagents
                                .iter()
                                .any(|s| self.child_matches(repo, d, s))
                    })
                    .collect();
                let repo_match = self.filter == Filter::All
                    && repo
                        .label
                        .to_lowercase()
                        .contains(&self.query.to_lowercase());
                if matching.is_empty() && !repo_match {
                    continue;
                }
                let waiting = matching
                    .iter()
                    .filter(|d| self.seen.derive(&repo.id, &d.row) == SemanticState::Waiting)
                    .count();
                out.push(Entry {
                    repo: repo.id.clone(),
                    destination: None,
                    key: None,
                    primary: format!(
                        "{}{}",
                        repo.label,
                        if waiting > 0 {
                            format!("  !{waiting}")
                        } else {
                            String::new()
                        }
                    ),
                    secondary: connection_label(repo.connection).into(),
                    state: SemanticState::Unknown,
                    child: false,
                    header: true,
                });
                if !self.collapsed.contains(&repo.id) || !self.query.is_empty() {
                    out.extend(matching.into_iter().map(|d| {
                        let mut entry = self.destination_entry(repo, d);
                        let row = &entry.destination.as_ref().unwrap().row;
                        if !matches!(
                            row.slot_state,
                            Some(
                                crate::worktree::SlotState::Free
                                    | crate::worktree::SlotState::Pinned
                            )
                        ) {
                            let slot = if row.kind == crate::worktree::WorktreeKind::Slot {
                                format!("{} / ", row.name)
                            } else {
                                String::new()
                            };
                            let dirty = if row.staged + row.unstaged + row.untracked > 0 {
                                format!(" +{} ~{} ?{}", row.staged, row.unstaged, row.untracked)
                            } else {
                                String::new()
                            };
                            let pr = if row.is_default {
                                String::new()
                            } else {
                                repo.prs
                                    .as_ref()
                                    .and_then(|p| p.prs.get(&row.branch))
                                    .map(|p| format!(" #{}", p.number))
                                    .unwrap_or_default()
                            };
                            let stale = if stale_remote(repo) { " / stale" } else { "" };
                            entry.secondary = format!(
                                "{slot}{}{}{dirty}{pr}{stale}",
                                row.branch,
                                if row.is_default { " / trunk" } else { "" }
                            );
                        }
                        entry
                    }));
                }
            } else {
                let mut groups = Vec::new();
                for d in destinations.into_iter().filter(Destination::reported_agent) {
                    let parent_match = self.matches(repo, &d, None);
                    let children: Vec<_> = d
                        .row
                        .subagents
                        .iter()
                        .filter(|s| parent_match || self.child_matches(repo, &d, s))
                        .cloned()
                        .collect();
                    if !parent_match && children.is_empty() {
                        continue;
                    }
                    let parent = self.destination_entry(repo, d);
                    let mut group = vec![parent.clone()];
                    for child in children {
                        let mut entry = parent.clone();
                        entry.key = parent.key.as_ref().map(|k| DestinationKey {
                            subagent: Some(child.id.clone()),
                            ..k.clone()
                        });
                        entry.primary = child.description.unwrap_or(child.agent_type);
                        entry.secondary = format!(
                            "{} / subagent {} parent{}",
                            repo.label,
                            if self.ascii { "->" } else { "→" },
                            if stale_remote(repo) { " / stale" } else { "" },
                        );
                        entry.state = match child.status {
                            crate::daemon::state::SubagentStatus::Running => SemanticState::Working,
                            crate::daemon::state::SubagentStatus::Done => SemanticState::Done,
                        };
                        entry.child = true;
                        group.push(entry);
                    }
                    groups.push(group);
                }
                out.extend(groups.into_iter().flatten());
            }
        }
        if section == Section::Agents {
            let mut groups: Vec<Vec<Entry>> = Vec::new();
            for entry in out {
                if entry.child {
                    if let Some(group) = groups.last_mut() {
                        group.push(entry);
                    }
                } else {
                    groups.push(vec![entry]);
                }
            }
            groups.sort_by(|a, b| {
                (state_rank(a[0].state), &a[0].repo, &a[0].primary, &a[0].key).cmp(&(
                    state_rank(b[0].state),
                    &b[0].repo,
                    &b[0].primary,
                    &b[0].key,
                ))
            });
            return groups.into_iter().flatten().collect();
        }
        let query = self.query.to_lowercase();
        for host in &self.hosts {
            let matching = remote_rows.remove(host.id.as_str()).unwrap_or_default();
            if matching.is_empty()
                && (self.filter != Filter::All
                    || !(host.id.to_lowercase().contains(&query)
                        || host.config.target.to_lowercase().contains(&query)))
            {
                continue;
            }
            let key = host.key();
            let expanded = !self.collapsed.contains(&key) || !self.query.is_empty();
            out.push(Entry {
                repo: key,
                destination: None,
                key: None,
                primary: format!("{} · {}", host.id, host.status.label()),
                secondary: host.config.target.clone(),
                state: SemanticState::Unknown,
                child: false,
                header: true,
            });
            if expanded {
                out.extend(matching);
            }
        }
        out
    }
    pub fn host(&self, key: &str) -> Option<&Host> {
        self.hosts.iter().find(|host| host.key() == key)
    }

    pub fn selected_entry(&self) -> Option<Entry> {
        self.entries(self.section)
            .into_iter()
            .find(|e| Some(e.identity()) == self.selected)
    }
    pub fn reconcile(&mut self) {
        let entries = self.entries(self.section);
        self.index = self
            .selected
            .as_ref()
            .and_then(|key| entries.iter().position(|e| &e.identity() == key))
            .unwrap_or(self.index)
            .min(entries.len().saturating_sub(1));
        if self.mode == Mode::Search
            && !self.query.is_empty()
            && entries.get(self.index).is_some_and(|e| e.header)
            && let Some(index) = entries.iter().position(|e| !e.header)
        {
            self.index = index;
        }
        self.selected = entries.get(self.index).map(Entry::identity);
    }
    pub fn move_selection(&mut self, delta: i32) {
        let entries = self.entries(self.section);
        self.index =
            (self.index as i32 + delta).clamp(0, entries.len().saturating_sub(1) as i32) as usize;
        self.selected = entries.get(self.index).map(Entry::identity);
        self.detail_scroll = 0;
    }
    pub fn shared(&self) -> SharedSelection {
        SharedSelection {
            destination: self.selected.as_ref().and_then(|s| s.1.clone()),
            repository: self.selected.as_ref().map(|s| s.0.clone()),
            agents: self.section == Section::Agents,
            collapsed: self.collapsed.clone(),
            query: self.query.clone(),
            filter: self.filter.label().into(),
        }
    }
    pub fn apply_shared(&mut self, state: SharedSelection) {
        self.section = if state.agents {
            Section::Agents
        } else {
            Section::Spaces
        };
        self.collapsed = state.collapsed;
        self.query = state.query;
        self.filter = match state.filter.as_str() {
            "Waiting" => Filter::Waiting,
            "Working" => Filter::Working,
            "Done" => Filter::Done,
            "Idle" => Filter::Idle,
            _ => Filter::All,
        };
        self.selected = state.repository.map(|repo| (repo, state.destination));
        self.reconcile();
    }
    pub fn toggle(&mut self, expand: Option<bool>) {
        if let Some((repo, _)) = &self.selected {
            let collapse = expand.map(|v| !v).unwrap_or(!self.collapsed.contains(repo));
            if collapse {
                self.collapsed.insert(repo.clone());
            } else {
                self.collapsed.remove(repo);
            }
            self.reconcile();
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.mode == Mode::Search {
            match key.code {
                KeyCode::Esc => {
                    self.query.clear();
                    self.mode = Mode::Navigation;
                }
                KeyCode::Enter => {
                    self.mode = Mode::Navigation;
                    return if self.selected_entry().is_some() {
                        Action::Activate
                    } else {
                        Action::None
                    };
                }
                KeyCode::Tab => self.mode = Mode::Filters,
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char('u') if ctrl => self.query.clear(),
                KeyCode::Char(c) if !ctrl && self.query.len() < 1024 => self.query.push(c),
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                _ => {}
            }
            self.reconcile();
            return Action::None;
        }
        if self.mode == Mode::Filters {
            match key.code {
                KeyCode::Char('a') => self.filter = Filter::All,
                KeyCode::Char('b') => self.filter = Filter::Waiting,
                KeyCode::Char('w') => self.filter = Filter::Working,
                KeyCode::Char('d') => self.filter = Filter::Done,
                KeyCode::Char('i') => self.filter = Filter::Idle,
                KeyCode::Tab | KeyCode::Char('/') => self.mode = Mode::Search,
                KeyCode::Esc | KeyCode::Enter => self.mode = Mode::Navigation,
                _ => {}
            }
            self.reconcile();
            return Action::None;
        }
        if self.mode == Mode::Help {
            self.mode = Mode::Navigation;
            return Action::None;
        }
        if self.mode == Mode::ConfirmKill {
            self.mode = Mode::Navigation;
            return if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                Action::Local('K')
            } else {
                Action::None
            };
        }
        if self.mode == Mode::More {
            self.mode = Mode::Navigation;
            return match key.code {
                KeyCode::Char(c @ ('c' | 'd' | 'D' | 't' | 'h' | 'r' | 'u' | 'O')) => {
                    Action::Local(c)
                }
                KeyCode::Char('K') => {
                    self.mode = Mode::ConfirmKill;
                    Action::None
                }
                _ => Action::None,
            };
        }
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Navigation;
                self.query.clear();
                self.filter = Filter::All;
                self.reconcile();
            }
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('c') if ctrl => return Action::Quit,
            KeyCode::Down | KeyCode::Char('j') => {
                if self.mode == Mode::Details {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                } else {
                    self.move_selection(1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.mode == Mode::Details {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                } else {
                    self.move_selection(-1);
                }
            }
            KeyCode::Tab => {
                self.section = if self.section == Section::Spaces {
                    Section::Agents
                } else {
                    Section::Spaces
                };
                self.index = 0;
                self.selected = None;
                self.reconcile();
            }
            KeyCode::Left => self.toggle(Some(false)),
            KeyCode::Right => self.toggle(Some(true)),
            KeyCode::Char('/' | 'g') => {
                self.mode = Mode::Search;
                self.collapsed.clear();
            }
            KeyCode::Char('f') => self.mode = Mode::Filters,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('m') => self.mode = Mode::More,
            KeyCode::Char('l') => self.mode = Mode::Details,
            KeyCode::Enter => return Action::Activate,
            KeyCode::Char('v') => return Action::Seen,
            KeyCode::Char(']') if self.next_attention(1) => return Action::Activate,
            KeyCode::Char('[') if self.next_attention(-1) => return Action::Activate,
            KeyCode::Char('K') => self.mode = Mode::ConfirmKill,
            KeyCode::Char(c @ ('c' | 'd' | 'D' | 't' | 'h' | 'r' | 'u' | 'O')) => {
                return Action::Local(c);
            }
            _ => {}
        }
        Action::None
    }
    pub fn attention(&self) -> Vec<DestinationKey> {
        use crate::overview::attention::{RepoState, rank};
        let empty = crate::daemon::state::PrSnapshot::default();
        let states: Vec<_> = self
            .repositories
            .iter()
            .filter_map(|r| {
                Some(RepoState {
                    id: &r.id,
                    name: &r.label,
                    snapshot: r.snapshot.as_ref()?,
                    prs: r.prs.as_ref().unwrap_or(&empty),
                })
            })
            .collect();
        let ranked = rank(&states);
        let mut out = Vec::new();
        for repo in &self.repositories {
            if stale_remote(repo) {
                continue;
            }
            for d in repo.destinations() {
                if d.row.deleting {
                    continue;
                }
                let reason = ranked
                    .iter()
                    .find(|i| i.repo_id == repo.id && i.path == d.row.path);
                let tier = reason.map(|r| r.reason.tier());
                let done = self.seen.derive(&repo.id, &d.row) == SemanticState::Done;
                if let Some(tier) = tier
                    .map(|t| if t == 5 { 6 } else { t })
                    .or(done.then_some(5))
                {
                    let tier = if done { tier.min(5) } else { tier };
                    out.push((
                        tier,
                        reason.and_then(|r| r.since),
                        repo.label.clone(),
                        d.title.clone(),
                        d.key,
                    ));
                }
            }
        }
        out.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| {
                    if a.0 == 1 {
                        a.1.cmp(&b.1)
                    } else if a.0 == 6 {
                        b.1.cmp(&a.1)
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then((&a.2, &a.3, &a.4).cmp(&(&b.2, &b.3, &b.4)))
        });
        out.into_iter().map(|v| v.4).collect()
    }
    pub fn next_attention(&mut self, delta: i32) -> bool {
        let keys = self.attention();
        if keys.is_empty() {
            self.status = "No destinations need attention".into();
            return false;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|s| s.1.as_ref())
            .map(DestinationKey::parent);
        let i = current
            .and_then(|k| keys.iter().position(|v| *v == k))
            .map(|i| (i as i32 + delta).rem_euclid(keys.len() as i32) as usize)
            .unwrap_or(if delta > 0 { 0 } else { keys.len() - 1 });
        self.section = Section::Spaces;
        self.query.clear();
        self.filter = Filter::All;
        self.collapsed.remove(&keys[i].repo);
        if let Some(host) = self
            .repositories
            .iter()
            .find(|repo| repo.id == keys[i].repo)
            .and_then(|repo| repo.host.as_ref())
        {
            self.collapsed.remove(&format!("ssh:{host}"));
        }
        self.selected = Some((keys[i].repo.clone(), Some(keys[i].clone())));
        self.reconcile();
        true
    }
}

/// Strip CSI/OSC/DCS and C0/C1 controls before width clipping. Raw identity is
/// retained separately and never reconstructed from this display string.
pub fn clean(text: &str) -> String {
    let mut chars = text.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']' | 'P' | '_' | '^') => {
                    let mut esc = false;
                    for c in chars.by_ref() {
                        if c == '\u{7}' || (esc && c == '\\') {
                            break;
                        }
                        esc = c == '\u{1b}';
                    }
                }
                _ => {}
            }
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}
pub fn clip(text: &str, width: u16) -> String {
    let mut used = 0;
    clean(text)
        .chars()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= width as usize
        })
        .collect()
}

fn color(state: SemanticState) -> Color {
    match state {
        SemanticState::Waiting => Theme::WAITING,
        SemanticState::Working => Theme::WORKING,
        SemanticState::Done => Theme::DONE,
        SemanticState::Idle | SemanticState::Unknown => Theme::MUTED,
    }
}
fn mark(state: SemanticState) -> &'static str {
    match state {
        SemanticState::Waiting => "!",
        SemanticState::Working => "~",
        SemanticState::Done => "+",
        SemanticState::Idle => "-",
        SemanticState::Unknown => "?",
    }
}
fn line(f: &mut Frame, area: Rect, y: u16, text: &str, style: Style) {
    if y < area.height {
        f.render_widget(
            Paragraph::new(clip(text, area.width)).style(style),
            Rect::new(area.x, area.y + y, area.width, 1),
        );
    }
}

pub fn render(f: &mut Frame, app: &mut App, view: TuiView) {
    let area = f.area();
    app.hits.clear();
    app.section_hits.clear();
    app.section_areas.clear();
    app.detail_area = None;
    if area.width < 8 || area.height < 3 {
        line(f, area, 0, "Resize; q", Style::default());
        return;
    }
    let body = Rect::new(area.x, area.y, area.width, area.height - 1);
    if view == TuiView::Details || app.mode == Mode::Details {
        render_details(f, app, body);
    } else if view == TuiView::Workspace && area.width >= 60 {
        let width = if area.width >= 100 { 32 } else { 24 };
        render_rail(f, app, Rect::new(body.x, body.y, width, body.height));
        render_details(
            f,
            app,
            Rect::new(
                body.x + width + 1,
                body.y,
                body.width - width - 1,
                body.height,
            ),
        );
    } else if view == TuiView::Workspace {
        render_section(f, app, body, app.section);
    } else {
        render_rail(f, app, body);
    }
    let footer = match app.mode {
        Mode::Search => format!("Go To: {}_  Enter open  Tab filters  Esc clear",app.query),
        Mode::Filters => format!("[{}] a All b Waiting w Working d Done i Idle; Tab type",app.filter.label()),
        Mode::ConfirmKill => "Stop this repository session and its agents? y/N".into(),
        Mode::More => "c create d delete D delete current t title h harness r refresh u update O overview K kill".into(),
        _ if !app.status.is_empty() => app.status.clone(),
        _ if !app.connection.is_empty() => app.connection.clone(),
        _ if !app.query.is_empty() || app.filter != Filter::All => format!("{}: {:?}  / search f filter Esc clear",app.filter.label(),app.query),
        _ if area.width < 16 => "/ ? Tab q".into(),
        _ => "/ search  g Go To  l details  ? help".into(),
    };
    line(
        f,
        area,
        area.height - 1,
        &footer,
        Style::default().fg(Theme::ACCENT),
    );
    if app.mode == Mode::Help {
        let text = "Swamp workspace\nj/k arrows: select (never activate)\nTab: Spaces / Agents; left/right: collapse/expand\nEnter or single click: open agent; subagents open parent\n/ or g: Go To; Tab in search: state filters\nf: filters; a All b Waiting w Working d Done i Idle\n[ / ]: previous / next attention destination\nv: Mark Seen (shared locally, exact completion)\nNative Zellij tab changes do not mark Done seen\nl: details / drill-in; Esc: back / clear\nc create; d delete; D delete current tab; t title\nh harness; r refresh; u update; O overview; m More\nK: confirm stop repo session and agents\nq: standalone exit / return to terminal in sidebar\nUse native Zellij focus, fullscreen, split and detach\nKeys apply only while this Swamp pane has focus\nAny key closes help";
        f.render_widget(ratatui::widgets::Clear, body);
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), body);
    }
}

fn render_rail(f: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 {
        return;
    }
    let content = Rect::new(
        area.x,
        area.y,
        area.width - u16::from(!app.embedded),
        area.height,
    );
    let divider = if app.ascii { "|" } else { "│" };
    if !app.embedded {
        for y in 0..area.height {
            line(
                f,
                area,
                y,
                &format!("{}{}", " ".repeat((area.width - 1) as usize), divider),
                Style::default().fg(Theme::MUTED),
            );
        }
    }
    if area.height < 10 || (area.width < 60 && app.mode == Mode::Search) {
        render_section(f, app, content, app.section);
        return;
    }
    let spaces = app.entries(Section::Spaces);
    let agents = app.entries(Section::Agents);
    let row_height = if area.width < 28 { 1 } else { 2 };
    let space_height = ((area.height as f32 * 0.45) as u16)
        .max(3)
        .min((spaces.len() as u16 * row_height + 1).max(3))
        .min(area.height.saturating_sub(3));
    let space_height = if agents.is_empty() {
        area.height.saturating_sub(2)
    } else {
        space_height
    };
    render_section(
        f,
        app,
        Rect::new(content.x, content.y, content.width, space_height),
        Section::Spaces,
    );
    render_section(
        f,
        app,
        Rect::new(
            content.x,
            content.y + space_height,
            content.width,
            content.height - space_height,
        ),
        Section::Agents,
    );
}

fn render_section(f: &mut Frame, app: &mut App, area: Rect, section: Section) {
    if area.height == 0 {
        return;
    }
    app.section_areas.push((area, section));
    let entries = app.entries(section);
    let waiting = entries
        .iter()
        .filter(|e| !e.child && !e.header && e.state == SemanticState::Waiting)
        .count();
    let count = |state| {
        entries
            .iter()
            .filter(|e| !e.child && !e.header && e.state == state)
            .count()
    };
    let counts = format!(
        "!{waiting} ~{} +{} -{}",
        count(SemanticState::Working),
        count(SemanticState::Done),
        count(SemanticState::Idle)
    );
    let heading = match section {
        Section::Spaces => "Spaces",
        Section::Agents => "Agents",
    };
    line(
        f,
        area,
        0,
        &format!(
            "{heading}{}  {counts}",
            if app.section == section { " >" } else { "" }
        ),
        Style::default()
            .fg(Theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    );
    app.section_hits
        .push((Rect::new(area.x, area.y, area.width, 1), section));
    let compact = area.width < 27;
    let height = if compact { 1 } else { 2 };
    let capacity = area.height.saturating_sub(1) as usize / height;
    let which = if section == Section::Spaces { 0 } else { 1 };
    let selected = entries
        .iter()
        .position(|e| Some(e.identity()) == app.selected && app.section == section);
    if let Some(i) = selected {
        if i < app.scroll[which] {
            app.scroll[which] = i;
        } else if i >= app.scroll[which] + capacity {
            app.scroll[which] = i.saturating_sub(capacity.saturating_sub(1));
        }
    }
    app.scroll[which] = app.scroll[which].min(entries.len().saturating_sub(capacity));
    if entries.is_empty() {
        line(
            f,
            area,
            1,
            if app.query.is_empty() && app.filter == Filter::All {
                if section == Section::Spaces {
                    "No repositories; reconnecting"
                } else {
                    "No reported agents"
                }
            } else {
                "No matches; Esc clears"
            },
            Style::default().fg(Theme::MUTED),
        );
    }
    for (index, entry) in entries
        .iter()
        .enumerate()
        .skip(app.scroll[which])
        .take(capacity)
    {
        let y = 1 + ((index - app.scroll[which]) * height) as u16;
        let selected = selected == Some(index);
        let active = !entry.repo.starts_with("ssh:")
            && entry
                .destination
                .as_ref()
                .is_some_and(|d| app.active_path.as_ref().is_some_and(|p| p == &d.row.path));
        let prefix = if selected { ">" } else { " " };
        let symbol = if entry.header {
            if app.collapsed.contains(&entry.repo) {
                ">"
            } else {
                "v"
            }
        } else {
            mark(entry.state)
        };
        let location = if active {
            "@ "
        } else if entry.destination.as_ref().is_some_and(|d| d.row.is_default) {
            "* "
        } else {
            ""
        };
        let indent = if entry.child { "  " } else { "" };
        let state_label = if entry.header {
            String::new()
        } else if compact {
            format!(" {}", entry.state.label())
        } else {
            String::new()
        };
        let text = format!(
            "{prefix}{symbol} {location}{indent}{}{state_label}",
            entry.primary
        );
        let style = Style::default().fg(if entry.header {
            Theme::ACCENT
        } else {
            color(entry.state)
        });
        let style = if selected {
            style.bg(Theme::SELECTED).add_modifier(Modifier::BOLD)
        } else {
            style
        };
        line(f, area, y, &text, style);
        if !location.is_empty() && area.width > 3 {
            f.render_widget(
                Paragraph::new(location.trim()).style(style.fg(if active {
                    Theme::ACCENT
                } else {
                    Theme::TRUNK
                })),
                Rect::new(area.x + 3, area.y + y, 1, 1),
            );
        }
        if !compact {
            let failed = entry.destination.as_ref().is_some_and(|d| {
                d.row.conflict
                    || d.row.rebase
                    || (!d.row.is_default
                        && app
                            .repositories
                            .iter()
                            .find(|r| r.id == entry.repo)
                            .and_then(|r| r.prs.as_ref())
                            .and_then(|p| p.prs.get(&d.row.branch))
                            .is_some_and(|p| {
                                p.state == "OPEN"
                                    && matches!(
                                        p.checks,
                                        Some(crate::github::CheckState::Failure { .. })
                                    )
                            }))
            });
            line(
                f,
                area,
                y + 1,
                &format!(
                    "   {indent}{}{}",
                    entry.secondary,
                    if entry.header {
                        String::new()
                    } else {
                        format!(" / {}", entry.state.label())
                    }
                ),
                Style::default().fg(if failed { Theme::FAILURE } else { Theme::MUTED }),
            );
        }
        let hit = Rect::new(area.x, area.y + y, area.width, height as u16);
        app.hits.push((hit, entry.clone(), false, section));
        if entry.header {
            app.hits.push((
                Rect::new(area.x + 1, area.y + y, 1, 1),
                entry.clone(),
                true,
                section,
            ));
        }
    }
}

fn render_details(f: &mut Frame, app: &mut App, area: Rect) {
    app.detail_area = Some(area);
    let Some(entry) = app.selected_entry() else {
        line(
            f,
            area,
            0,
            "Select a destination; / Go To",
            Style::default().fg(Theme::MUTED),
        );
        return;
    };
    if let Some(host) = app.host(&entry.repo) {
        let mut lines = vec![
            Line::from(clean(&entry.primary)),
            Line::from(clean(&format!("SSH target    {}", host.config.target))),
        ];
        match &host.status {
            HostStatus::Offline(error) => {
                lines.push(Line::from(clean(error)));
                lines.push(Line::from(
                    "Retrying automatically. Cached repositories are stale.",
                ));
            }
            HostStatus::Connected
                if !app
                    .repositories
                    .iter()
                    .any(|repo| repo.host.as_ref() == Some(&host.id)) =>
            {
                lines.push(Line::from(
                    "No Swamp repositories on this host. Run swamp register there to add one.",
                ));
            }
            _ => {}
        }
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((app.detail_scroll, 0)),
            area,
        );
        return;
    }
    let Some(repo) = app.repositories.iter().find(|r| r.id == entry.repo) else {
        return;
    };
    let mut lines = vec![
        Line::from(Span::styled(
            clean(&entry.primary),
            Style::default()
                .fg(color(entry.state))
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(clean(&format!(
            "{} / {}",
            repo.label,
            connection_label(repo.connection)
        ))),
    ];
    if let Some(d) = &entry.destination {
        let row = &d.row;
        let slot = match row.slot_state {
            Some(state) => format!("{} {state:?}", row.name),
            None => "overflow".into(),
        };
        let mut text = vec![
            format!("Path     {}", row.path.display()),
            format!(
                "Branch   {}{}",
                row.branch,
                if row.is_default { " (trunk)" } else { "" }
            ),
            format!("Slot     {slot}"),
            format!("Title    {:?}", row.title_source),
            format!(
                "Git      {} staged / {} unstaged / {} untracked",
                row.staged, row.unstaged, row.untracked
            ),
            format!(
                "         +{} ahead / -{} behind{}{}",
                row.ahead,
                row.behind,
                if row.conflict { " / CONFLICT" } else { "" },
                if row.rebase { " / REBASE" } else { "" }
            ),
            format!(
                "Agent    {} / {}{}",
                row.harness.map(|h| h.label()).unwrap_or("unknown harness"),
                entry.state.label(),
                if row.agent_ts > 0 {
                    format!(
                        " / status {}s ago",
                        crate::util::now_unix().saturating_sub(row.agent_ts)
                    )
                } else {
                    String::new()
                }
            ),
        ];
        if !row.is_default {
            match &repo.prs {
                None => text.push("PR       Loading / unavailable from peer".into()),
                Some(prs) if prs.loading => text.push("PR       Loading".into()),
                Some(prs) if prs.error.is_some() => text.push(format!(
                    "PR       Unavailable: {}",
                    prs.error.as_deref().unwrap_or("")
                )),
                Some(prs) => match prs.prs.get(&row.branch) {
                    Some(pr) => {
                        text.push(format!("PR       #{} {} {}", pr.number, pr.state, pr.title));
                        text.push(format!("CI       {:?}", pr.checks));
                        text.push(format!(
                            "Review   {:?} / {} comments",
                            pr.review, pr.comment_count
                        ));
                    }
                    None => text.push("PR       No pull request".into()),
                },
            }
        }
        for child in &row.subagents {
            text.push(format!(
                "  Subagent {}: {:?} / {} (opens parent)",
                child.agent_type,
                child.status,
                child.description.as_deref().unwrap_or(&child.id)
            ));
        }
        lines.extend(text.into_iter().map(|s| Line::from(clean(&s))));
    } else if let Some(identity) = &repo.identity {
        lines.push(Line::from(clean(
            &identity.common_dir.display().to_string(),
        )));
    }
    lines.push(Line::from(""));
    if repo.host.is_some() {
        lines.push(Line::from("Enter Open remote session   g Go To"));
    } else {
        lines.push(Line::from("Resources (selected repository)"));
        lines.extend(app.resource_lines.iter().map(|s| Line::from(clean(s))));
        lines.push(Line::from(""));
        lines.push(Line::from("Enter Open agent   g Go To   c Create   m More"));
    }
    if !crate::zellij::in_zellij() {
        lines.push(Line::from(
            "Outside Zellij: paths available; terminal opening disabled",
        ));
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        area,
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        daemon::state::{AgentStatus, Snapshot, SubagentRow, SubagentStatus},
        discover::{RepoIdentity, test_support::row},
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::Path;
    use std::time::Duration;

    pub(crate) fn fixture() -> App {
        let mut main = row("main", Path::new("/repos/swamp/main"));
        main.is_default = true;
        let mut working = row("herdr-ify", Path::new("/repos/swamp/herdr-ify"));
        working.title = "Herdr-style dashboard".into();
        working.agent = AgentStatus::Working;
        working.agent_ts = 1;
        working.staged = 3;
        working.unstaged = 2;
        working.subagents.push(SubagentRow {
            id: "reviewer".into(),
            agent_type: "review".into(),
            description: Some("Review parser".into()),
            status: SubagentStatus::Running,
            started_ts: 1,
            last_ts: 1,
            finished_ts: None,
        });
        let mut waiting = row("rss-fix", Path::new("/repos/papr/rss-fix"));
        waiting.title = "Fix flaky tests".into();
        waiting.agent = AgentStatus::Waiting;
        waiting.agent_ts = 1;
        let mut done = row("parser", Path::new("/repos/swamp/parser"));
        done.title = "Review parser".into();
        done.agent_ts = 1;
        done.activity_id = Some("turn-1".into());
        done.completion_id = Some("completed-1".into());
        let repo = |id: &str, label: &str, rows| Repository {
            id: id.into(),
            label: label.into(),
            host: None,
            identity: Some(RepoIdentity {
                common_dir: format!("/repos/{label}/.bare").into(),
                session_name: format!("{label}-session"),
                version: None,
            }),
            socket: None,
            connection: Connection::Live,
            snapshot: Some(Snapshot {
                rows,
                default_known: true,
            }),
            prs: None,
        };
        let mut app = App {
            repositories: vec![
                repo("swamp", "swamp", vec![main, working, done]),
                repo("papr", "papr", vec![waiting]),
            ],
            active_path: Some("/repos/swamp/herdr-ify".into()),
            ..Default::default()
        };
        app.reconcile();
        app
    }
    pub(crate) fn screen(app: &mut App, width: u16, height: u16, view: TuiView) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render(f, app, view)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|r| {
                r.iter()
                    .map(|c| c.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
                    + "\n"
            })
            .collect()
    }
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    #[test]
    fn search_text_cannot_invoke_actions_and_filters_are_explicit() {
        let mut app = fixture();
        assert_eq!(app.key(key('/')), Action::None);
        for c in "dDKwbiv".chars() {
            assert_eq!(app.key(key(c)), Action::None);
        }
        assert_eq!(app.query, "dDKwbiv");
        assert_eq!(app.filter, Filter::All);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.query.is_empty());
        app.key(key('f'));
        assert_eq!(app.key(key('d')), Action::None);
        assert_eq!(app.filter, Filter::Done);
    }
    #[test]
    fn go_to_enter_opens_only_a_matching_destination() {
        let mut app = fixture();
        app.key(key('g'));
        for c in "rss-fix".chars() {
            assert_eq!(app.key(key(c)), Action::None);
        }
        assert_eq!(
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Activate
        );
        assert_eq!(app.selected_entry().unwrap().key.unwrap().branch, "rss-fix");
        app.key(key('/'));
        app.query = "no-result".into();
        app.reconcile();
        assert_eq!(
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::None
        );
    }
    #[test]
    fn search_subagent_retains_parent_and_selection_survives_broadcast() {
        let mut app = fixture();
        app.section = Section::Agents;
        app.query = "review parser".into();
        app.reconcile();
        let entries = app.entries(Section::Agents);
        assert!(entries.iter().any(|e| e.primary == "Herdr-style dashboard"));
        assert!(
            entries
                .iter()
                .any(|e| e.child && e.primary == "Review parser")
        );
        let selected = app.selected.clone();
        app.repositories.reverse();
        app.reconcile();
        assert_eq!(app.selected, selected);
        app.repositories.clear();
        app.reconcile();
        assert!(app.selected.is_none());
    }
    #[test]
    fn working_subagent_filter_retains_unknown_parent_context() {
        let mut app = fixture();
        let parent = &mut app.repositories[0].snapshot.as_mut().unwrap().rows[1];
        parent.agent_ts = 0;
        parent.agent = AgentStatus::Idle;
        app.filter = Filter::Working;
        app.query = "review parser".into();
        let entries = app.entries(Section::Agents);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].state, SemanticState::Unknown);
        assert!(entries[1].child);
        assert_eq!(entries[1].state, SemanticState::Working);
    }
    #[test]
    fn groups_sort_waiting_done_working_and_keep_children() {
        let app = fixture();
        let entries = app.entries(Section::Agents);
        assert_eq!(
            entries
                .iter()
                .filter(|e| !e.child)
                .map(|e| e.state)
                .collect::<Vec<_>>(),
            [
                SemanticState::Waiting,
                SemanticState::Done,
                SemanticState::Working
            ]
        );
        assert!(entries.last().unwrap().child);
    }
    #[test]
    fn attention_is_unique_prioritized_and_empty_queue_never_activates() {
        let mut app = fixture();
        app.repositories[1].snapshot.as_mut().unwrap().rows[0].conflict = true;
        app.repositories[0].snapshot.as_mut().unwrap().rows[0].unstaged = 1;
        let keys = app.attention();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].repo, "papr");
        assert_eq!(keys[1].branch, "parser");
        app.repositories.clear();
        assert_eq!(app.key(key(']')), Action::None);
    }
    #[test]
    fn attention_reveals_remote_worktree_and_preserves_selection() {
        for direction in ['[', ']'] {
            for section in [Section::Spaces, Section::Agents] {
                for collapse_repo in [false, true] {
                    let mut app = fixture();
                    let host = Host {
                        id: "dev".into(),
                        config: crate::config::SshHostConfig {
                            target: "user@dev".into(),
                            swamp_path: None,
                        },
                        status: HostStatus::Connected,
                    };
                    let host_key = host.key();
                    app.hosts.push(host);
                    let remote = &mut app.repositories[1];
                    remote.id = "ssh:dev:papr".into();
                    remote.host = Some("dev".into());
                    let target = remote.destinations()[0].key.clone();
                    app.collapsed.insert(host_key.clone());
                    if collapse_repo {
                        app.collapsed.insert(target.repo.clone());
                    }
                    // The other attention target is local. Both directions must
                    // move from it to the waiting remote worktree.
                    let local = app.repositories[0]
                        .destinations()
                        .into_iter()
                        .find(|d| d.key.branch == "parser")
                        .unwrap()
                        .key;
                    app.section = section;
                    app.selected = Some((local.repo.clone(), Some(local.clone())));
                    app.reconcile();
                    assert_eq!(app.selected_entry().unwrap().key, Some(local));
                    app.collapsed.insert("ssh:unrelated".into());

                    assert_eq!(app.key(key(direction)), Action::Activate);
                    assert_eq!(app.section, Section::Spaces);
                    assert!(!app.collapsed.contains(&host_key));
                    assert!(!app.collapsed.contains(&target.repo));
                    assert!(app.collapsed.contains("ssh:unrelated"));
                    assert_eq!(app.selected_entry().unwrap().key, Some(target.clone()));
                    assert_eq!(app.shared().destination, Some(target.clone()));
                    assert_eq!(
                        app.entries(Section::Spaces)[app.index].key,
                        Some(target.clone())
                    );

                    app.repositories.reverse();
                    app.reconcile();
                    assert_eq!(app.selected_entry().unwrap().key, Some(target));
                }
            }
        }
    }
    #[test]
    fn clipping_respects_columns_and_strips_terminal_sequences() {
        assert_eq!(clip("ab界cd", 4), "ab界");
        assert_eq!(clip("e\u{301}界x", 3), "e\u{301}界");
        assert_eq!(clean("a\x1b[31mRED\x1b[0m\x1b]0;evil\x07b\n\rc"), "aREDbc");
        assert_eq!(clip("a", 0), "");
    }
    #[test]
    fn ascii_and_ansi_theme_preserve_state_without_truecolor() {
        let mut app = fixture();
        app.ascii = true;
        let text = screen(&mut app, 32, 30, TuiView::Sidebar);
        assert!(text.contains('|'));
        assert!(!text.contains('│'));
        assert!(!text.contains('→'));
        for (state, symbol) in [
            (SemanticState::Waiting, "!"),
            (SemanticState::Working, "~"),
            (SemanticState::Done, "+"),
            (SemanticState::Idle, "-"),
        ] {
            assert_eq!(mark(state), symbol);
            assert!(!matches!(color(state), Color::Rgb(_, _, _)));
        }
        assert!(
            text.lines().any(|line| line.contains("~ @ Herdr-style")),
            "active location must retain its state without color"
        );
        let mut terminal = Terminal::new(TestBackend::new(32, 30)).unwrap();
        terminal
            .draw(|f| render(f, &mut app, TuiView::Sidebar))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 3)].fg, Theme::WORKING);
        assert_eq!(buffer[(3, 3)].fg, Theme::ACCENT);
    }
    #[test]
    fn generated_sidebar_uses_native_gutter_without_drawing_a_second_divider() {
        let mut app = fixture();
        let standalone = screen(&mut app, 32, 30, TuiView::Sidebar);
        assert!(standalone.contains('│'));
        app.embedded = true;
        // A 32-cell Zellij pane gives Swamp 31 cells beside its split gutter.
        let embedded = screen(&mut app, 31, 30, TuiView::Sidebar);
        assert!(!embedded.contains('│'));
        assert!(embedded.contains("Spaces") && embedded.contains("Agents"));
        assert!(embedded.contains("~ @ Herdr-style dashboard"));
    }
    #[test]
    fn render_sizes_hierarchy_and_current_selection_distinction() {
        let mut app = fixture();
        let rail = screen(&mut app, 32, 30, TuiView::Sidebar);
        assert!(rail.find("Spaces").unwrap() < rail.find("Agents").unwrap());
        assert!(rail.contains("@ Herdr-style dashboard"));
        assert!(rail.contains(">v swamp"));
        assert!(rail.contains("+ Review parser"));
        assert_eq!(app.hits.iter().filter(|(_, e, _, _)| e.child).count(), 1);
        for (w, h, view) in [
            (24, 16, TuiView::Sidebar),
            (12, 8, TuiView::Sidebar),
            (80, 24, TuiView::Workspace),
            (48, 16, TuiView::Workspace),
            (120, 36, TuiView::Workspace),
            (160, 48, TuiView::Workspace),
            (1, 1, TuiView::Workspace),
        ] {
            let text = screen(&mut app, w, h, view);
            assert!(!text.is_empty());
            for (r, _, _, _) in &app.hits {
                assert!(r.right() <= w && r.bottom() <= h);
            }
        }
        let compact = screen(&mut app, 24, 16, TuiView::Sidebar);
        assert!(compact.contains("Spaces"));
        app.mode = Mode::Details;
        let details = screen(&mut app, 48, 16, TuiView::Workspace);
        assert!(details.contains("Resources"));
        assert!(!details.contains("Spaces"));
    }
    #[test]
    fn trunk_details_suppress_pr_and_unknown_is_not_idle() {
        let mut app = fixture();
        app.move_selection(2);
        assert_eq!(app.selected_entry().unwrap().state, SemanticState::Unknown);
        let text = screen(&mut app, 80, 24, TuiView::Details);
        assert!(text.contains("(trunk)"));
        assert!(!text.contains("PR       "));
    }
    #[test]
    fn details_distinguish_loading_error_empty_and_disconnect() {
        let mut app = fixture();
        app.move_selection(1);
        app.repositories[0].prs = Some(crate::daemon::state::PrSnapshot {
            loading: true,
            ..Default::default()
        });
        assert!(screen(&mut app, 100, 30, TuiView::Details).contains("PR       Loading"));
        app.repositories[0].prs = Some(crate::daemon::state::PrSnapshot {
            error: Some("remote refused".into()),
            ..Default::default()
        });
        assert!(
            screen(&mut app, 100, 30, TuiView::Details).contains("Unavailable: remote refused")
        );
        app.repositories[0].prs = Some(Default::default());
        assert!(screen(&mut app, 100, 30, TuiView::Details).contains("No pull request"));
        app.repositories[0].connection = Connection::Disconnected;
        assert!(screen(&mut app, 100, 30, TuiView::Details).contains("Disconnected (stale)"));
        app.filter = Filter::Waiting;
        app.reconcile();
        assert_eq!(
            app.entries(Section::Spaces)
                .iter()
                .filter(|e| e.header)
                .count(),
            1
        );
        app.query = "no such title".into();
        app.reconcile();
        assert!(screen(&mut app, 32, 30, TuiView::Sidebar).contains("No matches; Esc clears"));
    }
    #[test]
    fn fixture_snapshot_artifacts() {
        let mut app = fixture();
        // Strip unpredictable age text by selecting the repository header.
        let cases = [
            ("rail-32x30", 32, 30, TuiView::Sidebar),
            ("rail-24x16", 24, 16, TuiView::Sidebar),
            ("standalone-80x24", 80, 24, TuiView::Workspace),
            ("standalone-48x16", 48, 16, TuiView::Workspace),
            ("extreme-7x3", 7, 3, TuiView::Workspace),
        ];
        for (name, w, h, view) in cases {
            let rendered = screen(&mut app, w, h, view).trim_end().to_owned() + "\n";
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/workspace")
                .join(format!("{name}.txt"));
            if std::env::var_os("SWAMP_UPDATE_SNAPSHOTS").is_some() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, &rendered).unwrap();
            }
            assert_eq!(std::fs::read_to_string(path).unwrap(), rendered);
        }
    }

    #[test]
    fn twenty_repo_two_hundred_worktree_four_pane_render_benchmark() {
        let mut app = fixture();
        let template = app.repositories[0].clone();
        app.repositories.clear();
        for i in 0..20 {
            let mut repo = template.clone();
            repo.id = format!("repo-{i}");
            repo.label = format!("project-{i}");
            repo.snapshot.as_mut().unwrap().rows = (0..10)
                .map(|j| {
                    let mut row = template.snapshot.as_ref().unwrap().rows[1].clone();
                    row.name = format!("work-{j}");
                    row.branch = format!("branch-{j}");
                    row.path = format!("/repos/project-{i}/work-{j}").into();
                    row
                })
                .collect();
            app.repositories.push(repo);
        }
        app.reconcile();
        let started = std::time::Instant::now();
        let mut worst = Duration::ZERO;
        for frame in 0..40 {
            let before = std::time::Instant::now();
            app.move_selection(1);
            for _ in 0..4 {
                let _ = screen(&mut app, 32, 30, TuiView::Sidebar);
            }
            if frame == 20 {
                app.query = "branch-1".into();
                app.reconcile();
            }
            worst = worst.max(before.elapsed());
        }
        eprintln!(
            "20 repos / 200 worktrees / four panes: 40 input+render frames {:?}; slowest {:?}",
            started.elapsed(),
            worst
        );
        assert!(
            worst < Duration::from_secs(1),
            "input/rendering stalled: {worst:?}"
        );
    }
}
