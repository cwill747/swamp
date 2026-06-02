use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "swamp", version, about = "Zellij worktree dashboard")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Launch a zellij session with a tab per worktree (default).
    Launch { dir: Option<PathBuf> },

    /// Run the per-repo daemon: state + watcher + socket server.
    Serve {
        /// Path inside the repo (default: cwd).
        dir: Option<PathBuf>,
        /// Stay in foreground (default: detach).
        #[arg(long)]
        foreground: bool,
    },

    /// Long-running TUI client; renders into the current pane.
    Tui {
        /// Path inside the repo (default: cwd).
        dir: Option<PathBuf>,
    },

    /// Record an agent status update (called from Claude Code hooks).
    Hook {
        /// New status: working | waiting | idle
        status: String,
        /// Path inside the worktree (default: cwd).
        #[arg(long)]
        dir: Option<PathBuf>,
    },

    /// Kill the swamp daemon and zellij session for this repo.
    Kill {
        /// Path inside the repo (default: cwd).
        dir: Option<PathBuf>,
    },
}
