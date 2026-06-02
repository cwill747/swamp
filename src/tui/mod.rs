mod icons;
mod theme;
mod view;

use crate::cli::TuiView;
use crate::daemon::socket::{read_server_msg, write_client_msg, ClientMsg, ServerMsg};
use crate::kill;
use crate::daemon::state::Snapshot;
use crate::daemon::{self};
use crate::worktree::{git_common_dir, resolve_git_dir};
use crate::zellij;
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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
    crossterm::execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, &common, repo_name, view).await;

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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
    };

    terminal.draw(|f| view::render(f, &app))?;

    while let Some(evt) = rx.recv().await {
        match evt {
            AppEvent::Snapshot(s) => {
                app.snapshot = s;
                if app.selected >= app.snapshot.rows.len() {
                    app.selected = app.snapshot.rows.len().saturating_sub(1);
                }
            }
            AppEvent::Tick => {
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
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
                        if !app.snapshot.rows.is_empty() {
                            app.selected = (app.selected + 1).min(app.snapshot.rows.len() - 1);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.selected = app.selected.saturating_sub(1);
                    }
                    KeyCode::Char('g') => app.selected = 0,
                    KeyCode::Char('G') => {
                        if !app.snapshot.rows.is_empty() {
                            app.selected = app.snapshot.rows.len() - 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(row) = app.snapshot.rows.get(app.selected) {
                            let _ = zellij::go_to_tab_name(&row.name);
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
            AppEvent::Input(_) => {}
        }
        terminal.draw(|f| view::render(f, &app))?;
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
        if let ServerMsg::Snapshot(s) = msg {
            if tx.send(AppEvent::Snapshot(s)).await.is_err() {
                break;
            }
        }
    }
    Ok(())
}
