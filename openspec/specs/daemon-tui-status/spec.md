# Daemon TUI Status Specification

## Purpose

Describe swamp's per-repository daemon, client protocol, live TUI behavior,
resource reporting, on-demand worktree tab opening, and session shutdown behavior.

## Requirements

### Requirement: Daemon Runtime Location
The daemon SHALL resolve the git common directory and use per-repository runtime socket and PID files under `$XDG_RUNTIME_DIR/swamp`, falling back to a temp runtime directory when needed.

#### Scenario: Runtime dir available
- **WHEN** `$XDG_RUNTIME_DIR` is usable
- **THEN** daemon socket and PID files are placed under `$XDG_RUNTIME_DIR/swamp`

#### Scenario: Runtime dir unavailable
- **WHEN** `$XDG_RUNTIME_DIR` is unavailable
- **THEN** daemon socket and PID files are placed under swamp's temp runtime fallback

### Requirement: Daemon Startup
The daemon SHALL remove stale socket files, bind its Unix socket, write its PID file, scan git state, and broadcast an initial snapshot.

#### Scenario: Stale socket
- **WHEN** a previous socket file exists but no daemon responds
- **THEN** the new daemon removes the stale socket and starts

#### Scenario: First scan
- **WHEN** the daemon starts successfully
- **THEN** clients can connect after socket bind and receive state after the initial refresh

### Requirement: Socket Protocol
Daemon clients SHALL communicate using length-prefixed JSON `ClientMsg` and `ServerMsg` frames.

#### Scenario: Client sends request
- **WHEN** a client writes a length-prefixed JSON request
- **THEN** the daemon decodes it as a client message and responds with a length-prefixed JSON server message

#### Scenario: Branch and update replies
- **WHEN** branch-list or default-branch-update clients receive unrelated broadcasts while waiting for their replies
- **THEN** the client skips unrelated messages and continues waiting

### Requirement: Snapshot Broadcasts
Subscribers SHALL receive the current worktree snapshot, resource snapshot, and PR status immediately after subscribing, followed by future broadcasts.

#### Scenario: New subscriber
- **WHEN** a TUI client subscribes to the daemon
- **THEN** it receives the current snapshots without waiting for the next polling interval

#### Scenario: State changes
- **WHEN** daemon state changes after refresh, hook, resource, or PR updates
- **THEN** subscribed clients receive updated messages

### Requirement: Worktree Snapshot Rows
Daemon snapshots SHALL include worktree rows with branch, upstream, ahead/behind, dirty counts, conflict/rebase state, agent status, agent timestamp, session name, head timestamp, and harness override.

#### Scenario: Snapshot requested
- **WHEN** a client requests or subscribes to a snapshot
- **THEN** each row contains git, agent, timestamp, and harness fields needed by the TUI

#### Scenario: Snapshot ordering
- **WHEN** rows are emitted
- **THEN** they are sorted by newest head timestamp and then by name

### Requirement: TUI Daemon Startup
The TUI SHALL start or probe the daemon on demand and fail if the daemon cannot answer within its startup timeout.

#### Scenario: Daemon already running
- **WHEN** `swamp tui` starts and a daemon answers
- **THEN** the TUI connects to the existing daemon

#### Scenario: Daemon not running
- **WHEN** `swamp tui` starts and no daemon answers
- **THEN** the TUI starts a daemon and waits for it to become responsive

#### Scenario: Daemon unavailable
- **WHEN** no daemon answers before the timeout
- **THEN** the TUI fails instead of drawing stale state

### Requirement: TUI Views
The TUI SHALL render worktree, AI status, resource, and PR status panels, with `all`, `worktrees`, `ai-status`, `resources`, and `pr-status` view modes.

#### Scenario: All view
- **WHEN** the TUI is run with the default view
- **THEN** all status panels are rendered together

#### Scenario: Single-panel view
- **WHEN** the TUI is run with a specific view mode
- **THEN** only that panel's status view is rendered

### Requirement: Worktrees Pane PR Status Columns
The worktrees pane SHALL render separate failed-build, comment, and review-status columns for worktree branches with pull request summaries when the pane has enough width for the expanded layout.

#### Scenario: Expanded worktrees pane
- **WHEN** the worktrees pane is rendered with enough width for PR status detail columns
- **THEN** each worktree row with a matching pull request summary shows failed-build count, comment count, and review status in separate columns

#### Scenario: Narrow worktrees pane
- **WHEN** the worktrees pane is rendered without enough width for PR status detail columns
- **THEN** the pane keeps a compact worktree table layout without the separate failed-build, comment, and review-status columns

#### Scenario: No pull request summary
- **WHEN** a worktree branch has no matching pull request summary
- **THEN** the failed-build, comment, and review-status cells for that row render as blank when the expanded layout is active

### Requirement: TUI Input Workflows
The TUI SHALL support keyboard and mouse workflows for selection movement, tab switching, worktree creation, worktree deletion, harness selection, refresh, default-branch update, session kill, and quit.

#### Scenario: Navigation
- **WHEN** the user presses movement keys or clicks selectable regions
- **THEN** the TUI updates selection consistently with the active panel

#### Scenario: Create workflow
- **WHEN** the user starts worktree creation
- **THEN** the TUI provides branch-name input followed by base branch selection for new branches

#### Scenario: Dirty delete workflow
- **WHEN** the daemon refuses deletion because a worktree is dirty
- **THEN** the TUI reopens the delete confirmation as a force-delete prompt

### Requirement: On-Demand Worktree Tab Opening
The dashboard TUI SHALL open a worktree's Zellij tab only in response to explicit user activation of that worktree, and SHALL NOT open worktree tabs in response to daemon snapshot updates. When the worktree already has a tab, activation SHALL switch to the existing tab rather than open a duplicate.

#### Scenario: User activates a worktree without a tab
- **WHEN** the user activates a worktree in the dashboard while running inside Zellij
- **AND** no tab currently exists for that worktree
- **THEN** the TUI opens a worktree tab for it and switches focus to it

#### Scenario: User activates a worktree that already has a tab
- **WHEN** the user activates a worktree whose tab already exists
- **THEN** the TUI switches to the existing tab instead of opening a duplicate

#### Scenario: New worktree appears without user action
- **WHEN** a new worktree appears in daemon snapshots and the user has not activated it
- **THEN** the TUI does not open a tab for it

#### Scenario: Outside Zellij
- **WHEN** the TUI is not running inside Zellij
- **THEN** worktree activation does not attempt to open Zellij tabs

### Requirement: Resource Reporting
The daemon SHALL sample Zellij-session process descendants, aggregate CPU, RSS, elapsed time, system load, and memory, and broadcast resource snapshots.

#### Scenario: Resource polling interval
- **WHEN** the daemon is running
- **THEN** resource snapshots are refreshed and broadcast periodically

#### Scenario: Session process missing
- **WHEN** the Zellij session process cannot be found
- **THEN** resource reporting emits fallback resource data

### Requirement: Session Shutdown
`swamp kill` SHALL resolve the target repo, terminate the daemon PID when present, remove runtime socket and PID files, and kill/delete the matching Zellij session.

#### Scenario: PID exists
- **WHEN** `swamp kill` finds a daemon PID file
- **THEN** it attempts to terminate that daemon and cleans runtime files

#### Scenario: PID missing or invalid
- **WHEN** `swamp kill` cannot read a usable daemon PID
- **THEN** it still removes runtime files and attempts Zellij session cleanup

#### Scenario: Zellij session exists
- **WHEN** the matching Zellij session exists
- **THEN** `swamp kill` kills and deletes that session
