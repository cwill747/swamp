use crate::cli::TuiView;
use crate::daemon::resources;
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::worktree::BranchInfo;
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::time::Instant;

/// An active prompt that captures keystrokes instead of the normal navigation
/// keys.
pub enum InputMode {
    /// The git-wt-style create picker (centered modal overlay).
    Create(CreatePicker),
    /// Confirming deletion of the named worktree. When `force_reason` is
    /// `Some`, the daemon already refused a non-forced attempt and this prompt
    /// asks whether to retry with `force: true`; the string is the
    /// human-readable reason (e.g. "has uncommitted changes").
    ConfirmDelete {
        name: String,
        force_reason: Option<String>,
    },
    /// Choosing the harness (Claude/Codex) for the named worktree. Applies on
    /// the next launch of that worktree's tab; only honored when the repo's
    /// harness setting is `choose`.
    PickHarness { name: String },
}

/// Which step of the [`CreatePicker`] is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateStep {
    /// Choosing an existing branch, or typing a new branch name.
    Branch,
    /// Choosing the base branch for the new branch named in `new_branch`.
    Base,
}

/// State for the two-step "create worktree" modal: pick an existing branch to
/// spin a worktree from, or type a new branch name and then pick its base.
pub struct CreatePicker {
    pub step: CreateStep,
    /// Current filter text (and, in the Branch step, the new-branch name).
    pub filter: String,
    /// All branches reported by the daemon (empty until the reply lands).
    pub branches: Vec<BranchInfo>,
    /// Selected index into the *filtered* entry list (see [`CreatePicker::entries`]).
    pub selected: usize,
    /// First visible entry index (scroll offset).
    pub scroll: usize,
    /// In the Base step, the new branch name chosen in the Branch step.
    pub new_branch: Option<String>,
    /// True while waiting for the daemon's branch list.
    pub loading: bool,
}

/// One row in the create picker's filtered list.
pub enum CreateEntry<'a> {
    /// Synthetic "create a new branch" row carrying the typed name.
    New(&'a str),
    /// An existing branch.
    Branch(&'a BranchInfo),
}

/// The action a confirmed selection resolves to (owned, so the picker borrow
/// can be released before we act).
pub(crate) enum CreateAction {
    New(String),
    Branch(String),
}

impl CreatePicker {
    /// The visible, filtered entries for the current step. The Branch step
    /// excludes already-checked-out branches and prepends a synthetic "new
    /// branch" row when the filter is non-empty and matches no branch; the Base
    /// step lists every branch (any branch is a valid base).
    pub fn entries(&self) -> Vec<CreateEntry<'_>> {
        let needle = self.filter.trim().to_lowercase();
        let mut out: Vec<CreateEntry<'_>> = Vec::new();
        match self.step {
            CreateStep::Branch => {
                let trimmed = self.filter.trim();
                let exact = self.branches.iter().any(|b| b.name == trimmed);
                if !trimmed.is_empty() && !exact {
                    out.push(CreateEntry::New(trimmed));
                }
                out.extend(
                    self.branches
                        .iter()
                        .filter(|b| !b.checked_out && b.name.to_lowercase().contains(&needle))
                        .map(CreateEntry::Branch),
                );
            }
            CreateStep::Base => {
                out.extend(
                    self.branches
                        .iter()
                        .filter(|b| b.name.to_lowercase().contains(&needle))
                        .map(CreateEntry::Branch),
                );
            }
        }
        out
    }
}

/// A clickable PR row: enough to open it in a browser.
#[derive(Clone)]
pub struct PrHit {
    pub url: Option<String>,
}

/// Screen regions captured during the last render, used to map mouse clicks
/// back to rows. Rebuilt every frame in [`super::view::render`]; panels that
/// aren't drawn this frame stay `None`.
#[derive(Default)]
pub struct HitRegions {
    /// Worktree table: (row area, visible row count, scroll offset).
    pub worktrees: Option<(Rect, usize, usize)>,
    /// AI status table: (row area, snapshot index per visible row).
    pub ai: Option<(Rect, Vec<usize>)>,
    /// PR & CI table: (row area, PR per visible row).
    pub prs: Option<(Rect, Vec<PrHit>)>,
    /// Full Resources panel area (for scroll routing).
    pub resources: Option<Rect>,
    /// Create-picker list rows area: clicks/scroll map to filtered entries.
    pub create_list: Option<Rect>,
}

pub struct AppState {
    pub snapshot: Snapshot,
    /// Selected worktree name. Resolved against the latest snapshot before use
    /// because snapshots reorder rows and the current tab can be pinned to row 0.
    pub selected: Option<String>,
    pub worktree_scroll: usize,
    pub spinner_frame: usize,
    pub repo_name: String,
    pub view: TuiView,
    pub refreshing: bool,
    pub pending_delete: Option<String>,
    pub pending_create: Option<String>,
    pub connected: bool,
    /// Active footer prompt (create/delete), if any.
    pub input: Option<InputMode>,
    /// Transient one-line status/error shown in the footer.
    pub status_msg: Option<String>,
    /// Self-dismissing footer confirmation (message, ticks remaining). Used for
    /// brief success notices like "URL copied" that should fade on their own.
    pub toast: Option<(String, u16)>,
    pub resources: resources::Snapshot,
    pub pr_snapshot: PrSnapshot,
    pub resource_scroll: u16,
    pub resource_viewport_height: u16,
    /// Canonicalized working directory of this swamp pane — the worktree it
    /// was launched in. Used to identify the active worktree row.
    pub current_dir: Option<PathBuf>,
    /// When true, resolve the active worktree from `current_dir`. Set on the
    /// worktree-tab pane; false on the dashboard (whose cwd is the default
    /// worktree, which must not be pinned).
    pub pin_cwd: bool,
    /// Tab name from `ZELLIJ_TAB_NAME`, used as a fallback when the working
    /// directory does not match a known worktree.
    pub tab_env: Option<String>,
    /// Resolved name of the active worktree (for highlighting / pinning).
    pub current_tab: Option<String>,
    /// Clickable regions from the last render (see [`HitRegions`]).
    pub regions: HitRegions,
    /// Last left-click (column, row, time), for double-click detection.
    pub last_click: Option<(u16, u16, Instant)>,
}

impl AppState {
    pub(crate) fn selected_index(&self) -> Option<usize> {
        let name = self.selected.as_ref()?;
        self.snapshot.rows.iter().position(|r| &r.name == name)
    }

    pub(crate) fn selected_row(&self) -> Option<&crate::daemon::state::WorktreeRow> {
        self.selected_index()
            .and_then(|idx| self.snapshot.rows.get(idx))
    }

    pub(crate) fn select_index(&mut self, idx: usize) {
        self.selected = self.snapshot.rows.get(idx).map(|r| r.name.clone());
    }

    pub(crate) fn move_selection(&mut self, delta: i32) {
        if self.snapshot.rows.is_empty() {
            self.selected = None;
            return;
        }
        let idx = self.selected_index().unwrap_or(0) as i32;
        let max = self.snapshot.rows.len() as i32 - 1;
        self.select_index((idx + delta).clamp(0, max) as usize);
    }

    pub(crate) fn select_first(&mut self) {
        if self.snapshot.rows.is_empty() {
            self.selected = None;
        } else {
            self.select_index(0);
        }
    }

    pub(crate) fn select_last(&mut self) {
        if self.snapshot.rows.is_empty() {
            self.selected = None;
        } else {
            self.select_index(self.snapshot.rows.len() - 1);
        }
    }

    pub(crate) fn reconcile_selection(&mut self) {
        if self.snapshot.rows.is_empty() {
            self.selected = None;
            self.worktree_scroll = 0;
            return;
        }
        if self.selected_index().is_none() {
            self.select_index(0);
        }
        if self.worktree_scroll >= self.snapshot.rows.len() {
            self.worktree_scroll = self.snapshot.rows.len().saturating_sub(1);
        }
    }

    /// Identify the active worktree row: prefer the one whose path contains
    /// this pane's working directory, falling back to the zellij tab name.
    fn resolve_current_tab(&self) -> Option<String> {
        if self.pin_cwd
            && let Some(ref dir) = self.current_dir
            && let Some(row) = self
                .snapshot
                .rows
                .iter()
                .find(|r| path_matches_worktree(dir, &r.path))
        {
            return Some(row.name.clone());
        }
        self.tab_env.clone()
    }

    pub(crate) fn pin_snapshot(&mut self) {
        self.current_tab = self.resolve_current_tab();
        if self.view != TuiView::Worktrees {
            return;
        }
        if let Some(ref tab) = self.current_tab
            && let Some(pos) = self.snapshot.rows.iter().position(|r| &r.name == tab)
        {
            let row = self.snapshot.rows.remove(pos);
            self.snapshot.rows.insert(0, row);
        }
        // Pin the default branch directly below the current worktree (second
        // slot). If it is the resolved current worktree, leave it first rather
        // than duplicating it down a row.
        if let Some(pos) = self.snapshot.rows.iter().position(|r| r.is_default) {
            let default_is_current = self
                .current_tab
                .as_ref()
                .is_some_and(|tab| tab == &self.snapshot.rows[pos].name);
            if default_is_current {
                return;
            }
            let row = self.snapshot.rows.remove(pos);
            let target = 1.min(self.snapshot.rows.len());
            self.snapshot.rows.insert(target, row);
        }
    }
}

/// True when `dir` is the worktree at `wt_path` (or a subdirectory of it).
fn path_matches_worktree(dir: &std::path::Path, wt_path: &std::path::Path) -> bool {
    let wt = wt_path
        .canonicalize()
        .unwrap_or_else(|_| wt_path.to_path_buf());
    dir == wt || dir.starts_with(&wt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::{AgentStatus, WorktreeRow};

    fn row(name: &str, is_default: bool) -> WorktreeRow {
        WorktreeRow {
            name: name.to_string(),
            path: PathBuf::from(format!("/repo/{name}")),
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
            head_ts: 0,
            harness: None,
            is_default,
        }
    }

    /// Build a worktrees-view AppState whose active tab resolves (via
    /// `tab_env`) to `current`, with the given rows.
    fn app_with(rows: Vec<WorktreeRow>, current: &str) -> AppState {
        AppState {
            snapshot: Snapshot { rows },
            selected: None,
            worktree_scroll: 0,
            spinner_frame: 0,
            repo_name: "repo".into(),
            view: TuiView::Worktrees,
            refreshing: false,
            pending_delete: None,
            pending_create: None,
            connected: true,
            input: None,
            status_msg: None,
            toast: None,
            resources: resources::Snapshot::default(),
            pr_snapshot: PrSnapshot::default(),
            resource_scroll: 0,
            resource_viewport_height: 0,
            current_dir: None,
            pin_cwd: false,
            tab_env: Some(current.to_string()),
            current_tab: None,
            regions: HitRegions::default(),
            last_click: None,
        }
    }

    /// The default-branch row is pinned to index 1, directly below the pinned
    /// current worktree at index 0.
    #[test]
    fn pin_snapshot_puts_default_branch_second() {
        let rows = vec![
            row("feat-a", false),
            row("main", true),
            row("feat-b", false),
        ];
        let mut app = app_with(rows, "feat-a");
        app.pin_snapshot();
        let names: Vec<&str> = app.snapshot.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names[0], "feat-a", "current worktree stays pinned first");
        assert_eq!(names[1], "main", "default branch is pinned second");
    }

    /// When the default branch *is* the current worktree it stays at index 0
    /// and is not duplicated into the second slot.
    #[test]
    fn pin_snapshot_default_branch_is_current_stays_first() {
        let rows = vec![
            row("feat-a", false),
            row("main", true),
            row("feat-b", false),
        ];
        let mut app = app_with(rows, "main");
        app.pin_snapshot();
        let names: Vec<&str> = app.snapshot.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names[0], "main", "default+current worktree stays first");
        assert_eq!(
            names.iter().filter(|n| **n == "main").count(),
            1,
            "the default row must not be duplicated"
        );
    }

    #[test]
    fn pin_snapshot_moves_default_branch_from_first_when_current_unresolved() {
        let rows = vec![
            row("main", true),
            row("feat-a", false),
            row("feat-b", false),
        ];
        let mut app = app_with(rows, "dashboard");
        app.pin_snapshot();
        let names: Vec<&str> = app.snapshot.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["feat-a", "main", "feat-b"],
            "default branch is pinned second even when no current worktree matched"
        );
    }

    #[test]
    fn pane_cwd_matches_its_worktree() {
        let wt = std::path::Path::new("/repo/feature-x");
        assert!(path_matches_worktree(wt, wt));
        // A subdirectory of the worktree still resolves to it.
        assert!(path_matches_worktree(
            std::path::Path::new("/repo/feature-x/src"),
            wt,
        ));
    }

    #[test]
    fn pane_cwd_does_not_match_other_worktrees() {
        let cwd = std::path::Path::new("/repo/feature-x");
        assert!(!path_matches_worktree(
            cwd,
            std::path::Path::new("/repo/main")
        ));
        // A worktree whose name is a prefix must not match (no false positive).
        assert!(!path_matches_worktree(
            cwd,
            std::path::Path::new("/repo/feature")
        ));
    }
}
