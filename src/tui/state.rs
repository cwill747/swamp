use crate::cli::TuiView;
use crate::daemon::resources;
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::worktree::BranchInfo;
use ratatui::layout::Rect;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

/// An active prompt that captures keystrokes instead of the normal navigation
/// keys.
pub enum InputMode {
    /// The git-wt-style create picker (centered modal overlay).
    Create(CreatePicker),
    /// Confirming deletion of the named worktree. `dirty` is true when the
    /// worktree has uncommitted work, which turns the prompt into a force
    /// override (deletion proceeds with `force: true`).
    ConfirmDelete { name: String, dirty: bool },
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
    /// Worktree table: (row area, visible row count). Display row index equals
    /// the snapshot index, so a hit row maps straight to `selected`.
    pub worktrees: Option<(Rect, usize)>,
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
    pub selected: usize,
    pub spinner_frame: usize,
    pub repo_name: String,
    pub view: TuiView,
    pub refreshing: bool,
    pub pending_delete: Option<String>,
    pub pending_create: bool,
    pub pre_create_names: HashSet<String>,
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
