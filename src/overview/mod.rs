//! `swamp overview`: the cross-repo TUI that lists the worktrees that need
//! the user and moves the user to the correct repo session and worktree tab.

mod app;
mod attention;
mod jump;
mod view;

use crate::daemon::socket::{ClientMsg, read_server_msg, write_client_msg};
use crate::fanin;
use crate::registry::RepoEntry;
use crate::util::now_unix;
use crate::zellij;
use anyhow::{Context, Result};
use app::{Action, App};
use crossterm::event::{Event, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::collections::BTreeMap;
use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

/// Name of the dedicated Zellij session that holds the overview TUI.
pub const SESSION: &str = "swamp-overview";

/// Seconds between two reads of the registry file.
const REGISTRY_RELOAD_SECS: u64 = 5;

pub async fn run(no_session: bool) -> Result<()> {
    if no_session {
        run_tui().await
    } else {
        tokio::task::spawn_blocking(open_session).await?
    }
}

/// Move the user to the overview session. Create the session when it does
/// not exist. Inside Zellij the client switches to it. Outside Zellij this
/// process attaches to it.
pub fn open_session() -> Result<()> {
    if std::env::var("ZELLIJ_SESSION_NAME").as_deref() == Ok(SESSION) {
        return Ok(());
    }
    crate::launch::ensure_overview_session(SESSION)?;
    if zellij::in_zellij() {
        zellij::switch_session(SESSION, None)
    } else {
        zellij::attach(SESSION)
    }
}

enum LoopEvent {
    FanIn(fanin::Event),
    Input(Event),
    Tick,
    /// A jump or a launch finished.
    Done(Result<Finished, String>),
}

enum Finished {
    /// The client moved. The text, if any, goes to the footer.
    Moved(Option<String>),
    /// The session is ready, and this process must attach to it.
    Attach {
        session: String,
        /// The worktree tab to bring into focus after the attach.
        focus: Option<jump::Target>,
    },
}

fn run_jump(target: jump::Target, inside_zellij: bool) -> Result<Finished> {
    if !inside_zellij {
        jump::ensure_session(&jump::Zellij, &target)?;
        return Ok(Finished::Attach {
            session: target.session.clone(),
            focus: Some(target),
        });
    }
    Ok(Finished::Moved(match jump::jump(&jump::Zellij, &target)? {
        jump::Outcome::Tab => None,
        jump::Outcome::SessionOnly { reason } => Some(format!(
            "Moved to {}, but not to the tab of {}: {reason}",
            target.session, target.worktree
        )),
    }))
}

fn run_launch(entry: RepoEntry, inside_zellij: bool) -> Result<Finished> {
    let session = crate::launch::ensure_background_session(&entry.common_dir)?;
    if !inside_zellij {
        return Ok(Finished::Attach {
            session,
            focus: None,
        });
    }
    zellij::switch_session(&session, None)?;
    Ok(Finished::Moved(None))
}

/// Ask one daemon to scan its worktrees again. The result arrives as a
/// snapshot on the subscription, so the reply is not used.
async fn request_refresh(sock: PathBuf) {
    let request = async {
        let mut stream = crate::discover::connect(&sock).await?;
        write_client_msg(&mut stream, &ClientMsg::Refresh)
            .await
            .ok()?;
        read_server_msg(&mut stream).await.ok()
    };
    let _ = tokio::time::timeout(Duration::from_secs(60), request).await;
}

fn load_registry(path: Option<&PathBuf>) -> Vec<RepoEntry> {
    path.map(|p| crate::registry::load_at(p))
        .unwrap_or_default()
}

async fn run_tui() -> Result<()> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(stdout(), LeaveAlternateScreen);
        prev_hook(info);
    }));

    enable_raw_mode()?;
    let mut out = stdout();
    if let Err(e) = crossterm::execute!(out, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let res = event_loop(&mut terminal).await;

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    match res? {
        // Outside Zellij, the jump ends with an attach in this terminal.
        Some(attach) => attach_outside(attach),
        None => Ok(()),
    }
}

/// A session to attach to from a terminal that is outside Zellij.
struct Attach {
    session: String,
    focus: Option<jump::Target>,
}

/// Attach to the session in this terminal. The worktree tab gets the focus
/// after the client is attached, because Zellij cannot open a tab in a
/// session that has no client.
fn attach_outside(attach: Attach) -> Result<()> {
    if let Some(target) = attach.focus {
        std::thread::spawn(move || {
            if let Err(e) = jump::focus_tab(&jump::Zellij, &target) {
                tracing::debug!("focus worktree tab after attach: {e:#}");
            }
        });
    }
    // The input thread of the TUI stops within one poll interval. Wait for
    // that, so that it does not take a key that belongs to Zellij.
    std::thread::sleep(Duration::from_millis(150));
    zellij::attach_and_wait(&attach.session)
}

/// Run until the user quits (`None`) or a jump outside Zellij needs this
/// process to attach to a session (`Some`).
async fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
) -> Result<Option<Attach>>
where
    B::Error: Send + Sync + 'static,
{
    let runtime_dir = crate::util::runtime_base_dir()?;
    let registry_path = crate::registry::registry_path().ok();
    let inside_zellij = zellij::in_zellij();
    let (tx, mut rx) = mpsc::channel::<LoopEvent>(256);

    // Daemon fan-in.
    {
        let (fan_tx, mut fan_rx) = mpsc::channel(256);
        fanin::spawn(
            fanin::Config {
                runtime_dir,
                registry: registry_path.clone(),
                scan_interval: fanin::SCAN_INTERVAL,
            },
            fan_tx,
        );
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(event) = fan_rx.recv().await {
                if tx.send(LoopEvent::FanIn(event)).await.is_err() {
                    return;
                }
            }
        });
    }

    // Input pump (blocking poll on a thread).
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                match crossterm::event::poll(Duration::from_millis(100)) {
                    Ok(true) => match crossterm::event::read() {
                        Ok(event) => {
                            if tx.blocking_send(LoopEvent::Input(event)).is_err() {
                                return;
                            }
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(250)),
                    },
                    Ok(false) => {
                        if tx.is_closed() {
                            return;
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(250)),
                }
            }
        });
    }

    // The ticker keeps the ages current and reloads the registry.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                if tx.send(LoopEvent::Tick).await.is_err() {
                    return;
                }
            }
        });
    }

    let mut app = App::default();
    app.set_registry(load_registry(registry_path.as_ref()));
    let mut sockets: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut busy = false;
    let mut ticks = 0u64;
    terminal
        .draw(|f| view::render(f, &app, now_unix()))
        .context("draw overview")?;

    while let Some(event) = rx.recv().await {
        match event {
            LoopEvent::FanIn(event) => {
                match &event {
                    fanin::Event::Up { id, sock, .. } => {
                        sockets.insert(id.clone(), sock.clone());
                    }
                    fanin::Event::Down { id } => {
                        sockets.remove(id);
                    }
                    _ => {}
                }
                app.apply(event);
            }
            LoopEvent::Tick => {
                ticks += 1;
                if ticks.is_multiple_of(REGISTRY_RELOAD_SECS) {
                    app.set_registry(load_registry(registry_path.as_ref()));
                }
            }
            LoopEvent::Done(result) => {
                busy = false;
                match result {
                    Ok(Finished::Attach { session, focus }) => {
                        return Ok(Some(Attach { session, focus }));
                    }
                    Ok(Finished::Moved(None)) => app.status = None,
                    Ok(Finished::Moved(Some(text))) => app.error(text),
                    Err(text) => app.error(text),
                }
            }
            LoopEvent::Input(Event::Key(key)) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match app.handle_key(key) {
                    Action::None => {}
                    Action::Quit => return Ok(None),
                    Action::Refresh => {
                        app.set_registry(load_registry(registry_path.as_ref()));
                        for sock in sockets.values() {
                            tokio::spawn(request_refresh(sock.clone()));
                        }
                        app.info(format!("Refresh requested from {} repos", sockets.len()));
                    }
                    // One jump or launch at a time.
                    Action::Jump(_) | Action::Launch(_) if busy => {
                        app.info("Still busy with the last request");
                    }
                    Action::Jump(target) => {
                        busy = true;
                        app.info(format!(
                            "Jumping to {} in {}…",
                            target.worktree, target.session
                        ));
                        let tx = tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let result = run_jump(target, inside_zellij)
                                .map_err(|e| format!("Jump failed: {e:#}"));
                            let _ = tx.blocking_send(LoopEvent::Done(result));
                        });
                    }
                    Action::Launch(entry) => {
                        busy = true;
                        app.info(format!("Launching {}…", entry.session_name));
                        let tx = tx.clone();
                        tokio::task::spawn_blocking(move || {
                            let result = run_launch(entry, inside_zellij)
                                .map_err(|e| format!("Launch failed: {e:#}"));
                            let _ = tx.blocking_send(LoopEvent::Done(result));
                        });
                    }
                }
            }
            LoopEvent::Input(_) => {}
        }
        terminal
            .draw(|f| view::render(f, &app, now_unix()))
            .context("draw overview")?;
    }
    Ok(None)
}
