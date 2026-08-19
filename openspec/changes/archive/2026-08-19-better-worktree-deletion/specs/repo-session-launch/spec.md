## ADDED Requirements

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

## MODIFIED Requirements

### Requirement: CLI Command Surface
The CLI SHALL expose `launch`, `serve`, `tui`, `hook`, `kill`, `init`, and `completions` as public commands, while keeping `codex-notify`, `relaunch-tab`, `delete-tab`, and `confirm-delete` hidden from normal help output.

#### Scenario: Help hides internal commands
- **WHEN** a user renders top-level help
- **THEN** public commands are listed
- **AND** internal commands are not shown

#### Scenario: Unknown hook status is rejected
- **WHEN** a user runs `swamp hook` with a status other than `working`, `waiting`, or `idle`
- **THEN** argument parsing fails before recording a status update
