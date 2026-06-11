## MODIFIED Requirements

### Requirement: Daemon Startup
The daemon SHALL remove stale socket files, bind its Unix socket, write its PID file, scan local git state, and broadcast an initial snapshot. The initial scan SHALL gather per-worktree git status concurrently across worktrees and SHALL NOT block on network operations.

#### Scenario: Stale socket
- **WHEN** a previous socket file exists but no daemon responds
- **THEN** the new daemon removes the stale socket and starts

#### Scenario: First scan
- **WHEN** the daemon starts successfully
- **THEN** clients can connect after socket bind and receive state after the initial refresh

#### Scenario: Concurrent local scan
- **WHEN** the daemon performs its initial worktree scan over multiple worktrees
- **THEN** per-worktree git status is gathered concurrently rather than strictly one worktree at a time

### Requirement: Snapshot Broadcasts
Subscribers SHALL receive the current worktree snapshot, resource snapshot, and PR status immediately after subscribing, followed by future broadcasts. The worktree snapshot SHALL be computed entirely from local git state and SHALL be delivered as soon as the local scan completes, independent of and never waiting for network PR/CI status.

#### Scenario: New subscriber
- **WHEN** a TUI client subscribes to the daemon
- **THEN** it receives the current snapshots without waiting for the next polling interval

#### Scenario: Worktree snapshot not gated on network
- **WHEN** a TUI client subscribes while network PR status has not yet been fetched
- **THEN** it still receives the worktree snapshot built from local git state without waiting for PR data

#### Scenario: State changes
- **WHEN** daemon state changes after refresh, hook, resource, or PR updates
- **THEN** subscribed clients receive updated messages
