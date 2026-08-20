# Repo Session Launch Specification

## Purpose

Describe how swamp exposes its CLI surface and launches or attaches to Zellij
sessions for git repositories and their worktrees.

## Requirements

### Requirement: CLI Command Surface
The CLI SHALL expose `launch`, `serve`, `tui`, `hook`, `kill`, `init`, and `completions` as public commands, while keeping `codex-notify`, `relaunch-tab`, `delete-tab`, and `confirm-delete` hidden from normal help output.

#### Scenario: Help hides internal commands
- **WHEN** a user renders top-level help
- **THEN** public commands are listed
- **AND** internal commands are not shown

#### Scenario: Unknown hook status is rejected
- **WHEN** a user runs `swamp hook` with a status other than `working`, `waiting`, or `idle`
- **THEN** argument parsing fails before recording a status update

### Requirement: Default Launch Command
Running `swamp` without a subcommand SHALL behave as `swamp launch` using the current directory.

#### Scenario: No subcommand
- **WHEN** a user runs `swamp` from inside a git repository
- **THEN** swamp launches or attaches to the repo session for that repository

#### Scenario: Explicit launch directory
- **WHEN** a user runs `swamp launch DIR`
- **THEN** swamp uses `DIR` as the repository location to inspect and launch

### Requirement: Repo Session Startup
Launch SHALL resolve the target git repository, discover its worktrees, reject repositories with no worktrees, and name the Zellij session from the target directory basename.

#### Scenario: Worktrees exist
- **WHEN** launch finds one or more worktrees
- **THEN** swamp builds a Zellij layout for those worktrees and starts or attaches to the named session

#### Scenario: No worktrees
- **WHEN** launch cannot discover any worktrees for the target repository
- **THEN** launch fails instead of starting an empty session

### Requirement: Zellij Version Compatibility
Swamp SHALL require Zellij 0.45 or later for repo session launch.

#### Scenario: Supported Zellij version
- **WHEN** the user launches swamp with Zellij 0.45 or later on `PATH`
- **THEN** swamp can use Zellij's native nested-session controls

### Requirement: Existing Session Attachment
When a matching Zellij session already exists, launch SHALL attach to it unless a stale daemon version is detected and an interactive restart is accepted. When launch is running inside an existing Zellij session, it SHALL attach to the matching repo session as a child in the current pane so Zellij can provide native nested-session controls.

#### Scenario: Current session exists
- **WHEN** a matching Zellij session exists, launch is not nested, and no accepted restart is required
- **THEN** swamp attaches to that session

#### Scenario: Current session exists while nested
- **WHEN** a matching Zellij session exists and launch is running inside another Zellij session
- **THEN** swamp attaches to the matching session inside the current pane
- **AND** Zellij offers its native nested-session controls

#### Scenario: Repo session is already active
- **WHEN** launch runs from inside the matching repo session
- **THEN** swamp leaves the current session active
- **AND** does not attach the session recursively

#### Scenario: Stale daemon in interactive terminal
- **WHEN** a matching session has a daemon version mismatch and the user accepts restart
- **THEN** swamp kills the old session before starting a fresh one

#### Scenario: Stale daemon in non-interactive terminal
- **WHEN** a matching session has a daemon version mismatch and no interactive prompt is available
- **THEN** swamp warns and attaches without restarting

### Requirement: Zellij Layout Generation
Launch SHALL generate a layout consisting of a single focused dashboard tab and SHALL NOT pre-create worktree tabs. The number of tabs in the generated layout SHALL NOT depend on the number of discovered worktrees.

#### Scenario: Bare repository layout
- **WHEN** the target repository is bare or uses a bare worktree layout
- **THEN** the generated session starts with a single focused dashboard tab
- **AND** no worktree tabs are pre-created

#### Scenario: Normal repository layout
- **WHEN** the target repository is not bare
- **THEN** the generated session starts with a single focused dashboard tab
- **AND** no worktree tabs are pre-created

#### Scenario: Many worktrees
- **WHEN** the repository has many discovered worktrees
- **THEN** the generated layout still contains only the focused dashboard tab

### Requirement: Dashboard Panes
The dashboard tab SHALL include worktree, resource, AI status, PR status, and shell panes sized according to dashboard config.

#### Scenario: Dashboard config present
- **WHEN** dashboard column percentages are configured
- **THEN** those percentages are used for the generated dashboard panes

#### Scenario: Dashboard config absent
- **WHEN** dashboard column percentages are missing
- **THEN** swamp uses its built-in dashboard defaults

### Requirement: Worktree Tab Panes
Each worktree tab SHALL include a lazygit pane using swamp's managed lazygit config, a pinned worktree status pane, a suspended agent pane, and a shell pane.

#### Scenario: Worktree tab starts
- **WHEN** a worktree tab is opened from the generated layout
- **THEN** lazygit, status, agent, and shell panes are configured for that worktree

#### Scenario: Agent pane selected harness
- **WHEN** the worktree has an effective Claude or Codex harness
- **THEN** the generated agent pane starts that harness for the worktree

### Requirement: Shell and Nix Startup
Generated panes SHALL use the user's login shell when available, fall back to `/bin/bash`, and enter `nix develop` only when an executable `nix` is on `PATH`.

#### Scenario: Fish shell
- **WHEN** the configured shell is fish
- **THEN** launch emits fish-compatible startup commands

#### Scenario: POSIX shell
- **WHEN** the configured shell is not fish
- **THEN** launch emits POSIX-compatible startup commands

#### Scenario: Nix unavailable
- **WHEN** `nix` is not executable on `PATH`
- **THEN** generated panes do not attempt to enter `nix develop`

### Requirement: Worktree Tab Relaunch
The internal relaunch command SHALL reopen a worktree tab so harness changes can take effect without requiring a full session restart.

#### Scenario: Outside Zellij
- **WHEN** relaunch is invoked outside Zellij
- **THEN** the command exits without changing tabs

#### Scenario: Current tab is not the only tab
- **WHEN** the target tab exists and another tab can remain open
- **THEN** relaunch opens a replacement worktree tab and closes the old tab

#### Scenario: Target tab is missing
- **WHEN** the target tab does not exist
- **THEN** relaunch opens a fresh worktree tab

#### Scenario: Existing tabs cannot be queried
- **WHEN** relaunch cannot query the current Zellij tab names
- **THEN** relaunch exits without changing tabs

### Requirement: Nested Session Launch
When launch is running inside an existing Zellij session and no matching repo session exists, swamp SHALL start the repo session from the generated layout as a child in the current pane. Swamp SHALL preserve the outer session and originating tab so Zellij 0.45 or later can provide native nested-session zoom, focus, and navigation controls.

#### Scenario: New session created while nested
- **WHEN** launch runs inside an existing Zellij session and no matching repo session exists
- **THEN** swamp creates the repo session using the generated layout inside the current pane
- **AND** Zellij offers its native nested-session controls

#### Scenario: Not nested
- **WHEN** launch runs outside any Zellij session and no matching repo session exists
- **THEN** swamp starts the new session in the foreground as before

### Requirement: Daemon Working Directory
The daemon SHALL set its own working directory to the repository git common directory at startup, rather than inheriting the working directory of whichever process started it. The daemon therefore never holds a working directory inside a worktree it may be asked to remove.

#### Scenario: Daemon started from a worktree pane
- **WHEN** a TUI pane running inside a worktree starts the daemon
- **THEN** the daemon's working directory is the git common directory, not that worktree

#### Scenario: Removing the worktree that started the daemon
- **WHEN** removal is requested for the worktree whose pane first started the daemon
- **THEN** the daemon does not refuse the removal for containing its current directory

### Requirement: Worktree Tab Delete
The internal delete-tab command SHALL remove one worktree and close its Zellij tab, running detached from the tab it closes. It SHALL close the tab first and remove the worktree afterwards, so no pane holds a working directory inside the target during removal. Failures SHALL be recorded in the repository diagnostic log rather than surfaced to a terminal that may already be gone.

#### Scenario: Tab exists
- **WHEN** delete-tab runs for a worktree whose tab is open
- **THEN** it closes that tab and then removes the worktree

#### Scenario: Tab is missing
- **WHEN** delete-tab runs for a worktree with no open tab
- **THEN** it removes the worktree without closing any tab

#### Scenario: Outside Zellij
- **WHEN** delete-tab runs outside Zellij
- **THEN** it removes the worktree and skips the tab close

#### Scenario: Removal fails
- **WHEN** the removal fails after the tab was closed
- **THEN** the failure is written to the repository diagnostic log

#### Scenario: Command survives its tab
- **WHEN** the tab that launched delete-tab is closed
- **THEN** the command continues to completion because it runs in its own process group with a working directory outside the target
