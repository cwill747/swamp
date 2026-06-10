# Repo Session Launch Specification

## Purpose

Describe how swamp exposes its CLI surface and launches or attaches to Zellij
sessions for git repositories and their worktrees.

## Requirements

### Requirement: CLI Command Surface
The CLI SHALL expose `launch`, `serve`, `tui`, `hook`, `kill`, `init`, and `completions` as public commands, while keeping `codex-notify` and `relaunch-tab` hidden from normal help output.

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

### Requirement: Existing Session Attachment
When a matching Zellij session already exists, launch SHALL attach to it unless a stale daemon version is detected and an interactive restart is accepted. When launch is running **inside** an existing Zellij session (nested), it SHALL switch the live client to the matching session rather than spawning a process that leaves the originating client idle.

#### Scenario: Current session exists
- **WHEN** a matching Zellij session exists, launch is not nested, and no accepted restart is required
- **THEN** swamp attaches to that session

#### Scenario: Current session exists while nested
- **WHEN** a matching Zellij session exists and launch is running inside another Zellij session
- **THEN** swamp switches the current client to the matching session
- **AND** the originating client is not left idle in the host session

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
When launch is running inside an existing Zellij session and no matching repo session exists yet, swamp SHALL create the repo session from the generated layout AND switch the current client to it in a single operation, so the user is moved into the new session rather than being left in the host session. Launch SHALL NOT spawn the new session as a blocking child that the host client never attaches to.

#### Scenario: New session created while nested
- **WHEN** launch runs inside an existing Zellij session and no matching repo session exists
- **THEN** swamp creates the repo session using the generated layout
- **AND** switches the current client to that new session

#### Scenario: Not nested
- **WHEN** launch runs outside any Zellij session and no matching repo session exists
- **THEN** swamp starts the new session in the foreground as before, without switching an existing client

### Requirement: Originating Tab Cleanup
After a nested launch switches the client to the repo session, swamp SHALL make a best-effort attempt to close the originating tab in the host session, so the user is not left with a stale swamp tab. Swamp SHALL NOT close the originating tab when it is the host session's only tab, because doing so would tear down the host session.

#### Scenario: Host has multiple tabs
- **WHEN** a nested launch switches to the repo session and the host session has more than one tab
- **THEN** swamp closes the originating tab in the host session

#### Scenario: Host has a single tab
- **WHEN** a nested launch switches to the repo session and the originating tab is the host session's only tab
- **THEN** swamp leaves the originating tab in place and drops back to the shell it was in before

#### Scenario: Tab close fails
- **WHEN** the best-effort close of the originating tab fails
- **THEN** the switch to the repo session still succeeds and launch does not error
