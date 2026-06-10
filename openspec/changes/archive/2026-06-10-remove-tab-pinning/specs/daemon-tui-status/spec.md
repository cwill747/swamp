## REMOVED Requirements

### Requirement: Tab Reconciliation
**Reason**: Tab pinning is being removed. Tying the set of open tabs to the set of worktrees auto-clutters the tab bar, slows perceived startup, and removes tab control from the user. Worktree tabs are now opened only on explicit user action.
**Migration**: Worktrees no longer auto-open tabs. To get a worktree's tab, activate it from the dashboard worktree pane (see the new "On-Demand Worktree Tab Opening" requirement). Tabs already opened in a live session remain part of Zellij session state and persist across detach/reattach.

## ADDED Requirements

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
