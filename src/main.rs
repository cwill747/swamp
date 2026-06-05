mod cli;
mod codex_notify;
mod config;
mod daemon;
mod github;
mod hook;
mod kill;
mod launch;
mod tui;
mod util;
mod worktree;
mod zellij;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();

    match args.command {
        None => launch::run(None),
        Some(cli::Cmd::Launch { dir }) => launch::run(dir),
        Some(cli::Cmd::Serve { dir, foreground }) => daemon::serve(dir, foreground).await,
        Some(cli::Cmd::Tui { dir, view, pin_cwd }) => tui::run(dir, view, pin_cwd).await,
        Some(cli::Cmd::Hook {
            status,
            dir,
            session_name,
            session_id,
        }) => hook::run(status, dir, session_name, session_id).await,
        Some(cli::Cmd::CodexNotify { payload }) => codex_notify::run(payload).await,
        Some(cli::Cmd::RelaunchTab { name, dir }) => launch::relaunch_worktree_tab(&name, &dir),
        Some(cli::Cmd::Kill { dir }) => kill::run(dir),
        Some(cli::Cmd::Init) => config::init(),
    }
}
