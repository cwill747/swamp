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
use clap::{CommandFactory, Parser};

#[tokio::main]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();

    match args.command {
        None => launch::run(None),
        Some(cli::Cmd::Launch(args)) => launch::run(args.dir),
        Some(cli::Cmd::Serve(args)) => daemon::serve(args.dir, args.foreground).await,
        Some(cli::Cmd::Tui(args)) => tui::run(args.dir, args.view, args.pin_cwd).await,
        Some(cli::Cmd::Hook(cli::HookArgs {
            status,
            dir,
            session_name,
            session_id,
        })) => hook::run(status.to_string(), dir, session_name, session_id).await,
        Some(cli::Cmd::CodexNotify(args)) => codex_notify::run(args.payload).await,
        Some(cli::Cmd::RelaunchTab(args)) => launch::relaunch_worktree_tab(&args.name, &args.dir),
        Some(cli::Cmd::Kill(args)) => kill::run(args.dir),
        Some(cli::Cmd::Init) => config::init(),
        Some(cli::Cmd::Completions(args)) => {
            clap_complete::generate(
                args.shell,
                &mut cli::Cli::command(),
                "swamp",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    }
}
