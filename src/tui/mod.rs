mod icons;
mod theme;
mod view;

use crate::cli::TuiView;
use crate::daemon::resources;
use crate::daemon::socket::{ClientMsg, ServerMsg, read_server_msg, write_client_msg};
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::daemon::{self};
use crate::kill;
use crate::worktree::{BranchInfo, git_common_dir, resolve_git_dir};
use crate::zellij;
use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use std::collections::HashSet;
use std::io::stdout;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

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
enum CreateAction {
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
/// back to rows. Rebuilt every frame in [`view::render`]; panels that aren't
/// drawn this frame stay `None`.
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

    fn pin_snapshot(&mut self) {
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

pub async fn run(dir: Option<PathBuf>, view: TuiView, pin_cwd: bool) -> Result<()> {
    let cwd = match dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let start = resolve_git_dir(&cwd);
    let common = git_common_dir(&start).context("not inside a git repo")?;
    let repo_name = common
        .parent()
        .and_then(|p| p.file_name())
        .or_else(|| common.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "swamp".into());

    ensure_daemon(&start).await?;

    enable_raw_mode()?;
    let mut out = stdout();
    crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, &common, repo_name, view, cwd, pin_cwd).await;

    disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    res
}

async fn ensure_daemon(start: &std::path::Path) -> Result<()> {
    let common = git_common_dir(start)?;
    let sock = daemon::socket_path(&common);
    if sock.exists() {
        return Ok(());
    }
    let me = std::env::current_exe()?;
    std::process::Command::new(me)
        .arg("serve")
        .arg(start.display().to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    for _ in 0..40 {
        if sock.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("daemon did not start within 2s");
}

enum AppEvent {
    Snapshot(Snapshot),
    Input(Event),
    Tick,
    Resources(resources::Snapshot),
    PrStatus(PrSnapshot),
    RefreshDone(Vec<String>),
    /// The default-branch update finished; `Ok(())` clears the status line,
    /// `Err` carries a message to surface.
    UpdateDone(Result<(), String>),
    /// The daemon's reply to a ListBranches request, for the open create picker.
    Branches(Vec<BranchInfo>),
    /// A create/delete request failed; surface the message in the footer.
    ActionError(String),
    /// A non-forced delete was refused because the worktree is dirty; re-open
    /// the confirmation as a force override.
    DeleteNeedsForce(String),
}

async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    common: &std::path::Path,
    repo_name: String,
    view: TuiView,
    cwd: PathBuf,
    pin_cwd: bool,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<AppEvent>(64);

    // Daemon subscriber task.
    {
        let tx = tx.clone();
        let common = common.to_path_buf();
        tokio::spawn(async move {
            loop {
                if let Err(e) = subscribe_loop(&common, tx.clone()).await {
                    tracing::debug!("subscriber: {e:?}");
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }

    // Input pump (blocking poll on a thread).
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                if event::poll(Duration::from_millis(100)).unwrap_or(false)
                    && let Ok(evt) = event::read()
                    && tx.blocking_send(AppEvent::Input(evt)).is_err()
                {
                    return;
                }
            }
        });
    }

    // Spinner ticker.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(120));
            loop {
                iv.tick().await;
                if tx.send(AppEvent::Tick).await.is_err() {
                    return;
                }
            }
        });
    }

    let mut app = AppState {
        snapshot: Snapshot { rows: vec![] },
        selected: 0,
        spinner_frame: 0,
        repo_name,
        view,
        refreshing: false,
        pending_delete: None,
        pending_create: false,
        pre_create_names: HashSet::new(),
        input: None,
        status_msg: None,
        resources: resources::Snapshot::default(),
        pr_snapshot: PrSnapshot::default(),
        resource_scroll: 0,
        resource_viewport_height: 0,
        current_dir: cwd.canonicalize().ok(),
        pin_cwd,
        tab_env: std::env::var("ZELLIJ_TAB_NAME")
            .ok()
            .filter(|s| !s.is_empty()),
        current_tab: None,
        regions: HitRegions::default(),
        last_click: None,
    };
    app.current_tab = app.tab_env.clone();

    terminal.draw(|f| view::render(f, &mut app))?;

    while let Some(evt) = rx.recv().await {
        match evt {
            AppEvent::Snapshot(s) => {
                app.snapshot = s;
                app.pin_snapshot();
                if app.selected >= app.snapshot.rows.len() {
                    app.selected = app.snapshot.rows.len().saturating_sub(1);
                }
                if let Some(ref name) = app.pending_delete
                    && !app.snapshot.rows.iter().any(|r| &r.name == name)
                {
                    let _ = zellij::close_tab_by_name(name);
                    app.pending_delete = None;
                    app.status_msg = None;
                }
                if app.pending_create {
                    let new_rows: Vec<_> = app
                        .snapshot
                        .rows
                        .iter()
                        .filter(|r| !app.pre_create_names.contains(&r.name))
                        .collect();
                    if !new_rows.is_empty() {
                        for row in &new_rows {
                            let _ = crate::launch::open_worktree_tab(&row.path, &row.name);
                        }
                        if let Some(last) = new_rows.last() {
                            let _ = zellij::go_to_tab_name(&last.name);
                        }
                        app.pending_create = false;
                        app.status_msg = None;
                    }
                } else {
                    reconcile_tabs(&app);
                }
            }
            AppEvent::Tick => {
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
            }
            AppEvent::Resources(snap) => {
                app.resources = snap;
            }
            AppEvent::PrStatus(pr) => {
                app.pr_snapshot = pr;
            }
            AppEvent::RefreshDone(wt_names) => {
                app.refreshing = false;
                if let Ok(tabs) = zellij::list_tab_names() {
                    for tab in &tabs {
                        if tab == "dashboard" {
                            continue;
                        }
                        if !wt_names.iter().any(|n| n == tab) {
                            let _ = zellij::close_tab_by_name(tab);
                        }
                    }
                }
            }
            AppEvent::UpdateDone(res) => {
                app.status_msg = res.err();
            }
            AppEvent::Branches(branches) => {
                if let Some(InputMode::Create(p)) = app.input.as_mut() {
                    p.loading = false;
                    if p.step == CreateStep::Base {
                        p.selected = branches.iter().position(|b| b.is_default).unwrap_or(0);
                    }
                    p.branches = branches;
                }
            }
            AppEvent::ActionError(msg) => {
                app.pending_create = false;
                app.pending_delete = None;
                app.input = None;
                app.status_msg = Some(msg);
            }
            AppEvent::DeleteNeedsForce(name) => {
                // The snapshot looked clean but the daemon found uncommitted
                // work; re-prompt as a force override instead of failing.
                app.pending_delete = None;
                app.status_msg = None;
                app.input = Some(InputMode::ConfirmDelete { name, dirty: true });
            }
            AppEvent::Input(Event::Key(k)) => {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if matches!(app.input, Some(InputMode::Create(_))) {
                    handle_create_key(&mut app, k, &tx, common);
                    terminal.draw(|f| view::render(f, &mut app))?;
                    continue;
                }
                if let Some(mode) = app.input.take() {
                    handle_input_key(&mut app, mode, k, &tx, common);
                    terminal.draw(|f| view::render(f, &mut app))?;
                    continue;
                }
                match k.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        if app.view == TuiView::Resources {
                            let max = view::max_resource_scroll(
                                &app.resources,
                                app.resource_viewport_height,
                            );
                            app.resource_scroll = (app.resource_scroll + 1).min(max);
                        } else if !app.snapshot.rows.is_empty() {
                            app.selected = (app.selected + 1).min(app.snapshot.rows.len() - 1);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if app.view == TuiView::Resources {
                            app.resource_scroll = app.resource_scroll.saturating_sub(1);
                        } else {
                            app.selected = app.selected.saturating_sub(1);
                        }
                    }
                    KeyCode::Char('g') => {
                        if app.view == TuiView::Resources {
                            app.resource_scroll = 0;
                        } else {
                            app.selected = 0;
                        }
                    }
                    KeyCode::Char('G') => {
                        if app.view == TuiView::Resources {
                            let max = view::max_resource_scroll(
                                &app.resources,
                                app.resource_viewport_height,
                            );
                            app.resource_scroll = max;
                        } else if !app.snapshot.rows.is_empty() {
                            app.selected = app.snapshot.rows.len() - 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            let _ = zellij::go_to_tab_name(&row.name);
                        }
                    }
                    KeyCode::Char('c') => {
                        app.status_msg = None;
                        app.input = Some(InputMode::Create(CreatePicker {
                            step: CreateStep::Branch,
                            filter: String::new(),
                            branches: Vec::new(),
                            selected: 0,
                            scroll: 0,
                            new_branch: None,
                            loading: true,
                        }));
                        let tx = tx.clone();
                        let common = common.to_path_buf();
                        tokio::spawn(async move {
                            match request_branches(&common).await {
                                Ok(branches) => {
                                    let _ = tx.send(AppEvent::Branches(branches)).await;
                                }
                                Err(e) => {
                                    let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                                }
                            }
                        });
                    }
                    KeyCode::Char('d') => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            app.status_msg = None;
                            let dirty =
                                row.staged + row.unstaged + row.untracked > 0 || row.conflict;
                            app.input = Some(InputMode::ConfirmDelete {
                                name: row.name.clone(),
                                dirty,
                            });
                        }
                    }
                    KeyCode::Char('r') if !app.refreshing => {
                        app.refreshing = true;
                        let tx = tx.clone();
                        let common = common.to_path_buf();
                        tokio::spawn(async move {
                            if let Err(e) = send_refresh(&common, tx).await {
                                tracing::warn!("refresh: {e:?}");
                            }
                        });
                    }
                    KeyCode::Char('u') => {
                        app.status_msg = Some("Updating default branch…".into());
                        let tx = tx.clone();
                        let common = common.to_path_buf();
                        tokio::spawn(async move {
                            if let Err(e) = send_update(&common, tx.clone()).await {
                                let _ = tx.send(AppEvent::UpdateDone(Err(e.to_string()))).await;
                            }
                        });
                    }
                    KeyCode::Char('K') => {
                        return kill::run(Some(common.parent().unwrap_or(common).to_path_buf()));
                    }
                    _ => {}
                }
            }
            AppEvent::Input(Event::Mouse(m)) => handle_mouse(&mut app, m, &tx, common),
            AppEvent::Input(_) => {}
        }
        terminal.draw(|f| view::render(f, &mut app))?;
    }
    Ok(())
}

/// True when `(col, row)` falls inside `r`.
fn point_in(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Map a click in a row region to a 0-based row index, if it lands on a row.
fn row_index(area: Rect, count: usize, col: u16, row: u16) -> Option<usize> {
    if !point_in(area, col, row) {
        return None;
    }
    let idx = (row - area.y) as usize;
    (idx < count).then_some(idx)
}

/// Detect a double-click: a left-press on the same row as the previous one
/// within 400ms. Records the click for next time.
fn is_double_click(app: &mut AppState, col: u16, row: u16) -> bool {
    let now = Instant::now();
    let dbl = matches!(
        app.last_click,
        Some((_, r, t)) if r == row && now.duration_since(t) < Duration::from_millis(400)
    );
    // Reset after a double so a third click starts a fresh pair.
    app.last_click = if dbl { None } else { Some((col, row, now)) };
    dbl
}

/// Jump the zellij session to the tab for the worktree at `idx`.
fn jump_to_worktree(app: &AppState, idx: usize) {
    if let Some(r) = app.snapshot.rows.get(idx) {
        let _ = zellij::go_to_tab_name(&r.name);
    }
}

fn handle_mouse(
    app: &mut AppState,
    m: MouseEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    // While the create picker is open it owns all mouse input.
    if matches!(app.input, Some(InputMode::Create(_))) {
        handle_create_mouse(app, m, tx, common);
        return;
    }
    let (col, row) = (m.column, m.row);
    match m.kind {
        // Scroll routes to whatever panel the cursor is over.
        MouseEventKind::ScrollDown => {
            if app.regions.resources.is_some_and(|r| point_in(r, col, row)) {
                let max = view::max_resource_scroll(&app.resources, app.resource_viewport_height);
                app.resource_scroll = (app.resource_scroll + 3).min(max);
            } else if app
                .regions
                .worktrees
                .is_some_and(|(r, _)| point_in(r, col, row))
                && !app.snapshot.rows.is_empty()
            {
                app.selected = (app.selected + 1).min(app.snapshot.rows.len() - 1);
            }
        }
        MouseEventKind::ScrollUp => {
            if app.regions.resources.is_some_and(|r| point_in(r, col, row)) {
                app.resource_scroll = app.resource_scroll.saturating_sub(3);
            } else if app
                .regions
                .worktrees
                .is_some_and(|(r, _)| point_in(r, col, row))
            {
                app.selected = app.selected.saturating_sub(1);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let dbl = is_double_click(app, col, row);

            // Worktree table: click selects, double-click jumps. Clicking the
            // PR-icon column opens the PR instead.
            if let Some((area, count)) = app.regions.worktrees
                && let Some(idx) = row_index(area, count, col, row)
            {
                // Fixed leading columns: #(3) + sp + agent(2) + sp = 7,
                // then the 1-wide PR icon.
                let pr_col = area.x + 7;
                if col == pr_col
                    && let Some(url) = app
                        .snapshot
                        .rows
                        .get(idx)
                        .and_then(|r| app.pr_snapshot.prs.get(&r.branch))
                        .and_then(|pr| pr.url.clone())
                {
                    crate::util::open_url(&url);
                    return;
                }
                app.selected = idx;
                if dbl {
                    jump_to_worktree(app, idx);
                }
                return;
            }

            // AI status: click selects the matching worktree, double-click jumps.
            let ai_target =
                app.regions.ai.as_ref().and_then(|(area, idxs)| {
                    row_index(*area, idxs.len(), col, row).map(|i| idxs[i])
                });
            if let Some(idx) = ai_target {
                app.selected = idx;
                if dbl {
                    jump_to_worktree(app, idx);
                }
                return;
            }

            // PR & CI: click opens the PR in a browser.
            let pr_url = app.regions.prs.as_ref().and_then(|(area, hits)| {
                row_index(*area, hits.len(), col, row).and_then(|i| hits[i].url.clone())
            });
            if let Some(url) = pr_url {
                crate::util::open_url(&url);
            }
        }
        _ => {}
    }
}

/// Handle a keystroke while a footer prompt is active. `app.input` was already
/// taken by the caller, so each branch re-stores it to stay open, or leaves it
/// `None` to dismiss the prompt. (The create picker is handled separately by
/// [`handle_create_key`].)
fn handle_input_key(
    app: &mut AppState,
    mode: InputMode,
    k: KeyEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    match mode {
        // The create picker keeps its state in `app.input` and is dispatched
        // before this function is reached; it never arrives here.
        InputMode::Create(picker) => {
            app.input = Some(InputMode::Create(picker));
        }
        InputMode::ConfirmDelete { name, dirty } => match k.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                app.pending_delete = Some(name.clone());
                app.status_msg = Some(format!("Deleting {name}…"));
                let tx = tx.clone();
                let common = common.to_path_buf();
                tokio::spawn(async move {
                    if let Err(e) = send_action(
                        &common,
                        ClientMsg::RemoveWorktree { name, force: dirty },
                        tx.clone(),
                    )
                    .await
                    {
                        let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
                    }
                });
            }
            _ => {} // n / Esc / anything else cancels
        },
    }
}

/// Handle a keystroke while the create picker is open. Mutates the picker in
/// place via `app.input`; Enter is delegated to [`create_confirm`].
fn handle_create_key(
    app: &mut AppState,
    k: KeyEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Esc => {
            // From the Base step, Esc steps back to the Branch step (restoring
            // the typed name); from the Branch step it cancels the picker.
            if let Some(InputMode::Create(p)) = app.input.as_mut()
                && p.step == CreateStep::Base
            {
                p.step = CreateStep::Branch;
                p.filter = p.new_branch.take().unwrap_or_default();
                p.selected = 0;
                p.scroll = 0;
                return;
            }
            app.input = None;
        }
        KeyCode::Char('c') if ctrl => app.input = None,
        KeyCode::Enter => create_confirm(app, tx, common),
        KeyCode::Up => create_move_sel(app, -1),
        KeyCode::Down => create_move_sel(app, 1),
        KeyCode::Char('p') if ctrl => create_move_sel(app, -1),
        KeyCode::Char('n') if ctrl => create_move_sel(app, 1),
        KeyCode::Backspace => {
            if let Some(InputMode::Create(p)) = app.input.as_mut() {
                p.filter.pop();
                p.selected = 0;
                p.scroll = 0;
            }
        }
        KeyCode::Char(c) if !ctrl => {
            if let Some(InputMode::Create(p)) = app.input.as_mut() {
                p.filter.push(c);
                p.selected = 0;
                p.scroll = 0;
            }
        }
        _ => {}
    }
}

/// Move the picker selection by `delta`, clamped to the filtered entry list.
fn create_move_sel(app: &mut AppState, delta: i32) {
    if let Some(InputMode::Create(p)) = app.input.as_mut() {
        let n = p.entries().len();
        if n == 0 {
            p.selected = 0;
            return;
        }
        let next = p.selected as i32 + delta;
        p.selected = next.clamp(0, n as i32 - 1) as usize;
    }
}

/// Act on the currently-selected picker entry: advance to the Base step for a
/// new branch, or fire the create request for an existing branch / chosen base.
fn create_confirm(app: &mut AppState, tx: &mpsc::Sender<AppEvent>, common: &std::path::Path) {
    let Some(InputMode::Create(mut picker)) = app.input.take() else {
        return;
    };
    let action = {
        let entries = picker.entries();
        entries.get(picker.selected).map(|e| match e {
            CreateEntry::New(name) => CreateAction::New(name.to_string()),
            CreateEntry::Branch(b) => CreateAction::Branch(b.name.clone()),
        })
    };
    match (picker.step, action) {
        (CreateStep::Branch, Some(CreateAction::New(name))) => {
            picker.step = CreateStep::Base;
            picker.new_branch = Some(name);
            picker.filter.clear();
            picker.selected = picker
                .branches
                .iter()
                .position(|b| b.is_default)
                .unwrap_or(0);
            picker.scroll = 0;
            app.input = Some(InputMode::Create(picker));
        }
        (CreateStep::Branch, Some(CreateAction::Branch(branch))) => {
            start_create(app, tx, common, ClientMsg::CreateWorktree { branch });
        }
        (CreateStep::Base, Some(CreateAction::Branch(base))) => {
            if let Some(branch) = picker.new_branch.clone() {
                start_create(
                    app,
                    tx,
                    common,
                    ClientMsg::CreateWorktreeFromBase { branch, base },
                );
            }
        }
        // Nothing selectable, or an impossible combo: reopen unchanged.
        _ => app.input = Some(InputMode::Create(picker)),
    }
}

/// Create zellij tabs for any worktrees in the snapshot that don't have one.
///
/// Swamp opens a tab itself when *it* creates a worktree (the `pending_create`
/// path), but a worktree born outside swamp — `git worktree add` in another
/// terminal, an agent spinning one up — only shows up in the daemon snapshot. It
/// lists in the dashboard, yet double-clicking it can't focus anything because
/// no tab exists. Reconcile fills that gap.
///
/// Only the dashboard's worktrees pane runs this: it's the single instance with
/// `view == Worktrees && !pin_cwd`, so the several swamp panes (one per worktree
/// tab, plus the dashboard's other views) don't race to create duplicate tabs.
/// `query-tab-names` is the dedupe — a worktree that already has a tab is
/// skipped, which also makes the first post-launch snapshot a no-op.
fn reconcile_tabs(app: &AppState) {
    if app.view != TuiView::Worktrees || app.pin_cwd {
        return;
    }
    let Ok(tabs) = zellij::list_tab_names() else {
        return;
    };
    for row in &app.snapshot.rows {
        if !tabs.iter().any(|t| t == &row.name) {
            let _ = crate::launch::open_worktree_tab(&row.path, &row.name);
        }
    }
}

/// Fire a worktree-create request and arm the pending-create tracking so the
/// new tab opens when the next snapshot arrives. Leaves `app.input` closed.
fn start_create(
    app: &mut AppState,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
    msg: ClientMsg,
) {
    let label = match &msg {
        ClientMsg::CreateWorktree { branch } | ClientMsg::CreateWorktreeFromBase { branch, .. } => {
            branch.clone()
        }
        _ => String::new(),
    };
    app.pre_create_names = app.snapshot.rows.iter().map(|r| r.name.clone()).collect();
    app.pending_create = true;
    app.status_msg = Some(format!("Creating {label}…"));
    let tx = tx.clone();
    let common = common.to_path_buf();
    tokio::spawn(async move {
        if let Err(e) = send_action(&common, msg, tx.clone()).await {
            let _ = tx.send(AppEvent::ActionError(e.to_string())).await;
        }
    });
}

/// Route a mouse event to the open create picker: scroll/click select an entry,
/// double-click confirms it.
fn handle_create_mouse(
    app: &mut AppState,
    m: MouseEvent,
    tx: &mpsc::Sender<AppEvent>,
    common: &std::path::Path,
) {
    match m.kind {
        MouseEventKind::ScrollDown => create_move_sel(app, 1),
        MouseEventKind::ScrollUp => create_move_sel(app, -1),
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(area) = app.regions.create_list else {
                return;
            };
            let dbl = is_double_click(app, m.column, m.row);
            if let Some(InputMode::Create(p)) = app.input.as_mut() {
                let n = p.entries().len();
                let visible = n.saturating_sub(p.scroll).min(area.height as usize);
                if let Some(idx) = row_index(area, visible, m.column, m.row) {
                    p.selected = (p.scroll + idx).min(n.saturating_sub(1));
                }
            }
            if dbl {
                create_confirm(app, tx, common);
            }
        }
        _ => {}
    }
}

/// Ask the daemon for the branch list (for the create picker). The connection
/// also receives periodic broadcasts (snapshots/resources), so skip any frame
/// that isn't the reply we asked for.
async fn request_branches(common: &std::path::Path) -> Result<Vec<BranchInfo>> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::ListBranches).await?;
    loop {
        match read_server_msg(&mut stream).await? {
            Some(ServerMsg::Branches { branches }) => return Ok(branches),
            Some(ServerMsg::Err { message }) => anyhow::bail!(message),
            Some(_) => continue, // stray broadcast; keep reading
            None => return Ok(Vec::new()),
        }
    }
}

/// Send a create/remove request to the daemon and forward any error message
/// back to the UI. Success is observed via the broadcast snapshot.
async fn send_action(
    common: &std::path::Path,
    msg: ClientMsg,
    tx: mpsc::Sender<AppEvent>,
) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &msg).await?;
    match read_server_msg(&mut stream).await? {
        Some(ServerMsg::Err { message }) => {
            let _ = tx.send(AppEvent::ActionError(message)).await;
        }
        Some(ServerMsg::ErrDirty { name }) => {
            let _ = tx.send(AppEvent::DeleteNeedsForce(name)).await;
        }
        _ => {}
    }
    Ok(())
}

async fn send_refresh(common: &std::path::Path, tx: mpsc::Sender<AppEvent>) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::Refresh).await?;
    if let Some(msg) = read_server_msg(&mut stream).await?
        && let ServerMsg::RefreshDone { worktree_names } = msg
    {
        let _ = tx.send(AppEvent::RefreshDone(worktree_names)).await;
    }
    Ok(())
}

/// Ask the daemon to fetch and fast-forward the default branch, then report the
/// outcome so the footer status line can clear (or show an error).
async fn send_update(common: &std::path::Path, tx: mpsc::Sender<AppEvent>) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::UpdateDefault).await?;
    // Skip unrelated broadcasts (Snapshot/Resources/PrStatus) that may race
    // ahead of the actual reply on this subscribed connection, so we report the
    // true update outcome rather than clearing on the first frame.
    let done = loop {
        match read_server_msg(&mut stream).await? {
            Some(ServerMsg::Ok) => break Ok(()),
            Some(ServerMsg::Err { message }) => break Err(message),
            Some(_) => continue, // stray broadcast; keep reading
            None => break Ok(()),
        }
    };
    let _ = tx.send(AppEvent::UpdateDone(done)).await;
    Ok(())
}

async fn subscribe_loop(common: &std::path::Path, tx: mpsc::Sender<AppEvent>) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::Subscribe).await?;
    while let Some(msg) = read_server_msg(&mut stream).await? {
        match msg {
            ServerMsg::Snapshot(s) => {
                if tx.send(AppEvent::Snapshot(s)).await.is_err() {
                    break;
                }
            }
            ServerMsg::Resources(r) => {
                if tx.send(AppEvent::Resources(r)).await.is_err() {
                    break;
                }
            }
            ServerMsg::PrStatus(pr) => {
                if tx.send(AppEvent::PrStatus(pr)).await.is_err() {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok(())
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

    #[test]
    fn point_in_respects_bounds() {
        let r = Rect {
            x: 2,
            y: 3,
            width: 4,
            height: 2,
        };
        assert!(point_in(r, 2, 3)); // top-left corner
        assert!(point_in(r, 5, 4)); // bottom-right inclusive
        assert!(!point_in(r, 6, 4)); // one past width
        assert!(!point_in(r, 5, 5)); // one past height
        assert!(!point_in(r, 1, 3)); // left of region
    }

    #[test]
    fn row_index_maps_click_to_row() {
        // Rows region with three visible rows starting at y=3.
        let area = Rect {
            x: 0,
            y: 3,
            width: 10,
            height: 5,
        };
        assert_eq!(row_index(area, 3, 0, 3), Some(0));
        assert_eq!(row_index(area, 3, 9, 5), Some(2));
        // Inside the rect but past the populated rows.
        assert_eq!(row_index(area, 3, 0, 6), None);
        // Outside the rect entirely.
        assert_eq!(row_index(area, 3, 0, 2), None);
    }
}
