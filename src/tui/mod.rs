mod icons;
mod theme;
mod view;

use crate::cli::TuiView;
use crate::daemon::socket::{read_server_msg, write_client_msg, ClientMsg, ServerMsg};
use crate::kill;
use crate::daemon::resources;
use crate::daemon::state::{PrSnapshot, Snapshot};
use crate::daemon::{self};
use crate::worktree::{git_common_dir, resolve_git_dir};
use crate::zellij;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use std::io::stdout;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

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
    pub create_input_received: bool,
    pub pre_create_names: HashSet<String>,
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
        if self.pin_cwd {
            if let Some(ref dir) = self.current_dir {
                if let Some(row) = self
                    .snapshot
                    .rows
                    .iter()
                    .find(|r| path_matches_worktree(dir, &r.path))
                {
                    return Some(row.name.clone());
                }
            }
        }
        self.tab_env.clone()
    }

    fn pin_snapshot(&mut self) {
        self.current_tab = self.resolve_current_tab();
        if self.view != TuiView::Worktrees {
            return;
        }
        if let Some(ref tab) = self.current_tab {
            if let Some(pos) = self.snapshot.rows.iter().position(|r| &r.name == tab) {
                let row = self.snapshot.rows.remove(pos);
                self.snapshot.rows.insert(0, row);
            }
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
    crossterm::execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
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
        std::thread::spawn(move || loop {
            if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(evt) = event::read() {
                    if tx.blocking_send(AppEvent::Input(evt)).is_err() {
                        return;
                    }
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
        create_input_received: false,
        pre_create_names: HashSet::new(),
        resources: resources::Snapshot::default(),
        pr_snapshot: PrSnapshot::default(),
        resource_scroll: 0,
        resource_viewport_height: 0,
        current_dir: cwd.canonicalize().ok(),
        pin_cwd,
        tab_env: std::env::var("ZELLIJ_TAB_NAME").ok().filter(|s| !s.is_empty()),
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
                if let Some(ref name) = app.pending_delete {
                    if !app.snapshot.rows.iter().any(|r| &r.name == name) {
                        let _ = zellij::close_tab_by_name(name);
                        app.pending_delete = None;
                    }
                }
                if app.pending_create {
                    let new_rows: Vec<_> = app.snapshot.rows.iter()
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
                    } else if app.create_input_received {
                        app.pending_create = false;
                    }
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
            AppEvent::Input(Event::Key(k)) => {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if app.pending_create && !app.create_input_received {
                    app.create_input_received = true;
                    let tx = tx.clone();
                    let common = common.to_path_buf();
                    tokio::spawn(async move {
                        let _ = send_refresh(&common, tx).await;
                    });
                }
                match k.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        if app.view == TuiView::Resources {
                            let max = view::max_resource_scroll(&app.resources, app.resource_viewport_height);
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
                            let max = view::max_resource_scroll(&app.resources, app.resource_viewport_height);
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
                        app.pre_create_names = app.snapshot.rows.iter()
                            .map(|r| r.name.clone())
                            .collect();
                        app.pending_create = true;
                        app.create_input_received = false;
                        let cwd = app.snapshot.rows.first()
                            .map(|r| r.path.clone())
                            .unwrap_or_else(|| common.parent().unwrap_or(common).to_path_buf());
                        let _ = zellij::run_floating(
                            "git",
                            &["wt", "add"],
                            &cwd,
                            "60%", "40%",
                        );
                    }
                    KeyCode::Char('d') => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            let name = row.name.clone();
                            app.pending_delete = Some(name.clone());
                            // Run from a *different* worktree — removing the one
                            // we're cd'd into would yank the shell's cwd.
                            let cwd = app.snapshot.rows.iter()
                                .find(|r| r.name != name)
                                .map(|r| r.path.clone())
                                .unwrap_or_else(|| common.parent().unwrap_or(common).to_path_buf());
                            let _ = zellij::run_floating(
                                "git",
                                &["wt", "remove", &name],
                                &cwd,
                                "60%", "40%",
                            );
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
                    KeyCode::Char('K') => {
                        return kill::run(Some(
                            common.parent().unwrap_or(common).to_path_buf(),
                        ));
                    }
                    _ => {}
                }
            }
            AppEvent::Input(Event::Mouse(m)) => handle_mouse(&mut app, m),
            AppEvent::Input(_) => {}
        }
        terminal.draw(|f| view::render(f, &mut app))?;
    }
    Ok(())
}

/// True when `(col, row)` falls inside `r`.
fn point_in(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x.saturating_add(r.width) && row >= r.y && row < r.y.saturating_add(r.height)
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

fn handle_mouse(app: &mut AppState, m: MouseEvent) {
    let (col, row) = (m.column, m.row);
    match m.kind {
        // Scroll routes to whatever panel the cursor is over.
        MouseEventKind::ScrollDown => {
            if app.regions.resources.map_or(false, |r| point_in(r, col, row)) {
                let max = view::max_resource_scroll(&app.resources, app.resource_viewport_height);
                app.resource_scroll = (app.resource_scroll + 3).min(max);
            } else if app.regions.worktrees.map_or(false, |(r, _)| point_in(r, col, row)) {
                if !app.snapshot.rows.is_empty() {
                    app.selected = (app.selected + 1).min(app.snapshot.rows.len() - 1);
                }
            }
        }
        MouseEventKind::ScrollUp => {
            if app.regions.resources.map_or(false, |r| point_in(r, col, row)) {
                app.resource_scroll = app.resource_scroll.saturating_sub(3);
            } else if app.regions.worktrees.map_or(false, |(r, _)| point_in(r, col, row)) {
                app.selected = app.selected.saturating_sub(1);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let dbl = is_double_click(app, col, row);

            // Worktree table: click selects, double-click jumps. Clicking the
            // PR-icon column opens the PR instead.
            if let Some((area, count)) = app.regions.worktrees {
                if let Some(idx) = row_index(area, count, col, row) {
                    // Fixed leading columns: #(3) + sp + agent(2) + sp = 7,
                    // then the 1-wide PR icon.
                    let pr_col = area.x + 7;
                    if col == pr_col {
                        if let Some(url) = app
                            .snapshot
                            .rows
                            .get(idx)
                            .and_then(|r| app.pr_snapshot.prs.get(&r.branch))
                            .and_then(|pr| pr.url.clone())
                        {
                            crate::util::open_url(&url);
                            return;
                        }
                    }
                    app.selected = idx;
                    if dbl {
                        jump_to_worktree(app, idx);
                    }
                    return;
                }
            }

            // AI status: click selects the matching worktree, double-click jumps.
            let ai_target = app.regions.ai.as_ref().and_then(|(area, idxs)| {
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

async fn send_refresh(common: &std::path::Path, tx: mpsc::Sender<AppEvent>) -> Result<()> {
    let sock = daemon::socket_path(common);
    let mut stream = UnixStream::connect(&sock).await?;
    write_client_msg(&mut stream, &ClientMsg::Refresh).await?;
    if let Some(msg) = read_server_msg(&mut stream).await? {
        if let ServerMsg::RefreshDone { worktree_names } = msg {
            let _ = tx.send(AppEvent::RefreshDone(worktree_names)).await;
        }
    }
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
        assert!(!path_matches_worktree(cwd, std::path::Path::new("/repo/main")));
        // A worktree whose name is a prefix must not match (no false positive).
        assert!(!path_matches_worktree(cwd, std::path::Path::new("/repo/feature")));
    }

    #[test]
    fn point_in_respects_bounds() {
        let r = Rect { x: 2, y: 3, width: 4, height: 2 };
        assert!(point_in(r, 2, 3)); // top-left corner
        assert!(point_in(r, 5, 4)); // bottom-right inclusive
        assert!(!point_in(r, 6, 4)); // one past width
        assert!(!point_in(r, 5, 5)); // one past height
        assert!(!point_in(r, 1, 3)); // left of region
    }

    #[test]
    fn row_index_maps_click_to_row() {
        // Rows region with three visible rows starting at y=3.
        let area = Rect { x: 0, y: 3, width: 10, height: 5 };
        assert_eq!(row_index(area, 3, 0, 3), Some(0));
        assert_eq!(row_index(area, 3, 9, 5), Some(2));
        // Inside the rect but past the populated rows.
        assert_eq!(row_index(area, 3, 0, 6), None);
        // Outside the rect entirely.
        assert_eq!(row_index(area, 3, 0, 2), None);
    }
}
