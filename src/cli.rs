use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "swamp", version, about = "Zellij worktree dashboard")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum TuiView {
    /// Show all panels (default for worktree tabs).
    #[default]
    All,
    /// Worktree list only.
    Worktrees,
    /// AI / Claude agent status only.
    AiStatus,
    /// Resource usage only.
    Resources,
    /// PR & CI status only.
    PrStatus,
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
        /// Which panel to render (default: all).
        #[arg(long, value_enum, default_value_t = TuiView::All)]
        view: TuiView,
        /// Pin the worktree matching this pane's cwd to the top. Set for the
        /// swamp pane inside a worktree tab; omitted on the dashboard, whose
        /// cwd is the default worktree and should stay recency-sorted.
        #[arg(long)]
        pin_cwd: bool,
    },

    /// Record an agent status update (called from Claude Code hooks).
    Hook {
        /// New status: working | waiting | idle
        status: String,
        /// Path inside the worktree (default: cwd).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Claude Code session/conversation name.
        #[arg(long)]
        session_name: Option<String>,
        /// Claude Code session id (UUID). Recorded so a restarted swamp can
        /// resume this worktree's session via `claude --resume <id>`.
        #[arg(long)]
        session_id: Option<String>,
    },

    /// Kill the swamp daemon and zellij session for this repo.
    Kill {
        /// Path inside the repo (default: cwd).
        dir: Option<PathBuf>,
    },
}
