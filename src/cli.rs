use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::fmt;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "swamp",
    version,
    about = "Zellij worktree dashboard",
    long_about = "swamp launches a Zellij session for a git repo, with one tab per worktree plus panes for lazygit, an agent harness, a shell, and live repo status."
)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum HookStatus {
    /// Agent is actively processing a prompt or tool call.
    Working,
    /// Agent is blocked on user input or a permission prompt.
    Waiting,
    /// Agent finished its turn.
    Idle,
}

impl fmt::Display for HookStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HookStatus::Working => "working",
            HookStatus::Waiting => "waiting",
            HookStatus::Idle => "idle",
        })
    }
}

#[derive(Subcommand)]
pub enum Cmd {
    #[command(
        about = "Launch or attach to a repo session",
        long_about = "Launch or attach to a Zellij session for a git repo. With no DIR, swamp uses the current directory. This is also the default command when `swamp` is run with no subcommand."
    )]
    Launch(LaunchArgs),

    #[command(
        about = "Run the repo status daemon",
        long_about = "Run the per-repo daemon that scans worktrees, watches git state, stores agent status, and serves TUI clients over a Unix socket. Most users do not need to run this directly."
    )]
    Serve(ServeArgs),

    #[command(
        about = "Render the status TUI",
        long_about = "Render the long-running status TUI in the current terminal pane. swamp launch embeds this in generated Zellij layouts, but it can also be run directly."
    )]
    Tui(TuiArgs),

    #[command(
        about = "Record an agent status update",
        long_about = "Record an agent status update for the current worktree. This is intended for Claude Code hooks and other automation; `swamp init` installs the recommended hooks."
    )]
    Hook(HookArgs),

    #[command(
        hide = true,
        about = "Forward a Codex notify event",
        long_about = "Forward a Codex `notify` event to swamp. `swamp init` configures Codex to call this command with the JSON payload Codex appends."
    )]
    CodexNotify(CodexNotifyArgs),

    #[command(
        hide = true,
        about = "Relaunch a worktree tab",
        long_about = "Close and reopen one worktree tab so a harness swap takes effect live. The TUI spawns this command detached; it is not intended for interactive use."
    )]
    RelaunchTab(RelaunchTabArgs),

    #[command(
        about = "Stop the repo session",
        long_about = "Stop the per-repo daemon, kill the matching Zellij session, and remove swamp's runtime socket and PID files."
    )]
    Kill(KillArgs),

    #[command(
        about = "Show the repo's diagnostic log",
        long_about = "Print swamp's per-repository diagnostic log (tab additions, git refreshes, hook updates). With no DIR, scopes output to the worktree containing the current directory; pass --all for the whole repo, or -f to follow new output."
    )]
    Logs(LogsArgs),

    #[command(
        about = "Install user config and agent hooks",
        long_about = "Write swamp's config file if it is missing, refresh managed config files, install or update Claude Code hooks, and configure Codex notify."
    )]
    Init,

    #[command(
        about = "Generate shell completions",
        long_about = "Generate shell completions for swamp. Packagers should use this command to install completions from the exact binary they ship."
    )]
    Completions(CompletionsArgs),
}

#[derive(Args)]
pub struct LaunchArgs {
    /// Path inside the repo to launch (default: current directory).
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct ServeArgs {
    /// Path inside the repo to serve (default: current directory).
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Stay attached and log to stderr instead of detaching.
    #[arg(long)]
    pub foreground: bool,
}

#[derive(Args)]
pub struct TuiArgs {
    /// Path inside the repo to inspect (default: current directory).
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Panel to render.
    #[arg(long, value_enum, default_value_t = TuiView::All)]
    pub view: TuiView,
    /// Pin the worktree matching this pane's cwd to the top.
    #[arg(long)]
    pub pin_cwd: bool,
}

#[derive(Args)]
pub struct HookArgs {
    /// New status to record: working, waiting, or idle.
    #[arg(value_enum, value_name = "STATUS")]
    pub status: HookStatus,
    /// Path inside the worktree to update (default: current directory).
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Claude Code session or conversation name to show in the TUI.
    #[arg(long, value_name = "NAME")]
    pub session_name: Option<String>,
    /// Claude Code session UUID to resume on a later launch.
    #[arg(long, value_name = "ID")]
    pub session_id: Option<String>,
}

#[derive(Args)]
pub struct CodexNotifyArgs {
    /// JSON payload Codex passes to notify.
    #[arg(value_name = "JSON", trailing_var_arg = true)]
    pub payload: Vec<String>,
}

#[derive(Args)]
pub struct RelaunchTabArgs {
    /// Worktree tab name to relaunch.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Worktree path for the relaunched tab.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,
}

#[derive(Args)]
pub struct KillArgs {
    /// Path inside the repo session to stop (default: current directory).
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct LogsArgs {
    /// Path inside the repo to inspect (default: current directory). When it
    /// falls inside a worktree, output is scoped to that worktree.
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Follow the log, printing new output as it is appended.
    #[arg(short, long)]
    pub follow: bool,
    /// Show the whole-repo log instead of scoping to the current worktree.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for.
    #[arg(value_enum, value_name = "SHELL")]
    pub shell: Shell,
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::{CommandFactory, Parser};

    fn top_level_help() -> String {
        Cli::command().render_long_help().to_string()
    }

    #[test]
    fn top_level_help_hides_internal_commands() {
        let help = top_level_help();

        assert!(help.contains("launch"));
        assert!(help.contains("init"));
        assert!(help.contains("completions"));
        assert!(!help.contains("codex-notify"));
        assert!(!help.contains("relaunch-tab"));
    }

    #[test]
    fn launch_help_documents_dir_argument() {
        let mut cmd = Cli::command();
        let launch = cmd.find_subcommand_mut("launch").unwrap();
        let help = launch.render_long_help().to_string();

        assert!(help.contains("[DIR]"));
        assert!(help.contains("Path inside the repo to launch"));
    }

    #[test]
    fn completions_help_lists_supported_shells() {
        let mut cmd = Cli::command();
        let completions = cmd.find_subcommand_mut("completions").unwrap();
        let help = completions.render_long_help().to_string();

        assert!(help.contains("bash"));
        assert!(help.contains("fish"));
        assert!(help.contains("zsh"));
    }

    #[test]
    fn hook_rejects_unknown_status() {
        let err = match Cli::try_parse_from(["swamp", "hook", "blocked"]) {
            Ok(_) => panic!("unknown hook status should be rejected"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }
}
