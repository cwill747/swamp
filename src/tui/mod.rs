mod client;
mod event;
mod icons;
mod input;
mod state;
mod theme;
mod view;

pub(crate) use state::{
    AppState, CreateEntry, CreatePicker, CreateStep, HitRegions, InputMode, PrHit,
};

use crate::cli::TuiView;
use crate::daemon::{self};
use crate::worktree::{git_common_dir, resolve_git_dir};
use anyhow::{Context, Result};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

pub async fn run(dir: Option<PathBuf>, view: TuiView, pin_cwd: bool) -> Result<()> {
    let cwd = match dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let start = resolve_git_dir(&cwd);
    let common = git_common_dir(&start).context("not inside a git repo")?;
    // File-only logging (no stderr — it would corrupt the TUI). Best-effort:
    // a logging-config typo is surfaced by the daemon, not by failing the TUI.
    let log_cfg = crate::config::load_config()
        .map(|c| c.logging)
        .unwrap_or_default();
    crate::logging::init(&common, false, false, &log_cfg);
    let repo_name = common
        .parent()
        .and_then(|p| p.file_name())
        .or_else(|| common.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "swamp".into());

    ensure_daemon(&start).await?;

    // Install a panic hook that restores the terminal before printing the
    // panic message.  This ensures that a panic (e.g. from a UTF-8 slice
    // bug mid-draw) doesn't leave the user's pane stuck in raw mode /
    // alternate screen / with mouse capture active.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best-effort teardown — ignore all errors.
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        prev_hook(info);
    }));

    enable_raw_mode()?;
    let mut out = stdout();
    if let Err(e) = crossterm::execute!(out, EnterAlternateScreen, EnableMouseCapture) {
        // raw mode is active but alternate screen failed; clean up before
        // propagating the error so the caller's terminal is not corrupted.
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let res = event::event_loop(&mut terminal, &common, repo_name, view, cwd, pin_cwd).await;

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
    let sock = daemon::socket_path(&common)?;
    // Probe rather than trust the file: a daemon that died can leave its socket
    // behind, and a bare existence check would declare that stale file healthy
    // and spawn nothing, leaving the TUI's subscribe loop stuck on a connection
    // that's refused forever. `serve` itself removes a stale socket on startup.
    if daemon::probe(&sock).await.is_ok() {
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
    // Wait for a daemon that actually answers, not just for the socket file to
    // reappear — during a stale-socket restart the old file lingers until the
    // new daemon rebinds, so existence alone would return prematurely.
    for _ in 0..40 {
        if daemon::probe(&sock).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("daemon did not start within 2s");
}
