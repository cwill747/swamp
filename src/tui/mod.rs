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
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind, EnableMouseCapture, DisableMouseCapture};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub struct AppState {
    pub snapshot: Snapshot,
    pub selected: usize,
    pub spinner_frame: usize,
    pub repo_name: String,
    pub view: TuiView,
    pub refreshing: bool,
    pub pending_delete: Option<String>,
    pub resources: resources::Snapshot,
    pub pr_snapshot: PrSnapshot,
    pub resource_scroll: u16,
    pub resource_viewport_height: u16,
}

pub async fn run(dir: Option<PathBuf>, view: TuiView) -> Result<()> {
    let start = resolve_git_dir(&dir.unwrap_or(std::env::current_dir()?));
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

    let res = event_loop(&mut terminal, &common, repo_name, view).await;

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
        resources: resources::Snapshot::default(),
        pr_snapshot: PrSnapshot::default(),
        resource_scroll: 0,
        resource_viewport_height: 0,
    };

    terminal.draw(|f| view::render(f, &mut app))?;

    while let Some(evt) = rx.recv().await {
        match evt {
            AppEvent::Snapshot(s) => {
                app.snapshot = s;
                if app.selected >= app.snapshot.rows.len() {
                    app.selected = app.snapshot.rows.len().saturating_sub(1);
                }
                if let Some(ref name) = app.pending_delete {
                    if !app.snapshot.rows.iter().any(|r| &r.name == name) {
                        let _ = zellij::close_tab_by_name(name);
                        app.pending_delete = None;
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
                    KeyCode::Char('d') => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            let name = row.name.clone();
                            app.pending_delete = Some(name.clone());
                            let _ = zellij::run_floating(
                                "git",
                                &["wt", "remove", &name],
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
            AppEvent::Input(Event::Mouse(m)) => match m.kind {
                MouseEventKind::ScrollDown => {
                    let max = view::max_resource_scroll(&app.resources, app.resource_viewport_height);
                    app.resource_scroll = (app.resource_scroll + 3).min(max);
                }
                MouseEventKind::ScrollUp => {
                    app.resource_scroll = app.resource_scroll.saturating_sub(3);
                }
                _ => {}
            },
            AppEvent::Input(_) => {}
        }
        terminal.draw(|f| view::render(f, &mut app))?;
    }
    Ok(())
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
