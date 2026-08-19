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

### Requirement: Socket Protocol
Daemon clients SHALL communicate using length-prefixed JSON `ClientMsg` and `ServerMsg` frames.

#### Scenario: Client sends request
- **WHEN** a client writes a length-prefixed JSON request
- **THEN** the daemon decodes it as a client message and responds with a length-prefixed JSON server message

#### Scenario: Branch and update replies
- **WHEN** branch-list or default-branch-update clients receive unrelated broadcasts while waiting for their replies
- **THEN** the client skips unrelated messages and continues waiting

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

### Requirement: Worktree Snapshot Rows
Daemon snapshots SHALL include worktree rows with branch, upstream, ahead/behind, dirty counts, conflict/rebase state, agent status, agent timestamp, session name, head timestamp, harness override, a flag marking whether the row's branch is the repository default branch, a flag marking whether a removal is in flight for that worktree, and the worktree's removal verdict. The default-branch flag SHALL be derived from the repository's configured default branch (the default remote's `HEAD`), and SHALL be false for every row when no default branch can be determined. The removal verdict SHALL be computed during the worktree scan and SHALL name the reason a non-forced removal would be refused, or report that removal is allowed. Both new fields SHALL decode to their inert defaults when a peer omits them.

#### Scenario: Snapshot requested
- **WHEN** a client requests or subscribes to a snapshot
- **THEN** each row contains git, agent, timestamp, harness, default-branch, deleting, and removal-verdict fields needed by the TUI

#### Scenario: Snapshot ordering
- **WHEN** rows are emitted
- **THEN** they are sorted by newest head timestamp and then by name

#### Scenario: Default branch flagged
- **WHEN** the repository default branch is known and a worktree has that branch checked out
- **THEN** that worktree's row is marked as the default branch and every other row is not

#### Scenario: Default branch unknown
- **WHEN** the repository default branch cannot be determined
- **THEN** no row is marked as the default branch

#### Scenario: Removal verdict carried on the row
- **WHEN** a worktree would be refused a non-forced removal
- **THEN** its row carries the matching blocking reason

#### Scenario: Older peer snapshot
- **WHEN** a snapshot omits the deleting flag or the removal verdict
- **THEN** it still decodes, with the row reported as not deleting and with no blocking reason

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
The worktrees pane SHALL render separate failed-build, comment, and review-status columns for worktree branches with pull request summaries when the pane has enough width for the expanded layout. The default-branch row SHALL never display pull request, CI, review, or comment status in any layout.

#### Scenario: Expanded worktrees pane
- **WHEN** the worktrees pane is rendered with enough width for PR status detail columns
- **THEN** each non-default worktree row with a matching pull request summary shows failed-build count, comment count, and review status in separate columns

#### Scenario: Narrow worktrees pane
- **WHEN** the worktrees pane is rendered without enough width for PR status detail columns
- **THEN** the pane keeps a compact worktree table layout without the separate failed-build, comment, and review-status columns

#### Scenario: No pull request summary
- **WHEN** a non-default worktree branch has no matching pull request summary
- **THEN** the failed-build, comment, and review-status cells for that row render as blank when the expanded layout is active

#### Scenario: Default branch row suppresses PR status
- **WHEN** the worktrees pane renders the default-branch row in either compact or expanded layout
- **THEN** its PR state, PR number, checks, review, and comment cells render as blank
- **AND** it shows no PR-loading indicator even while PR status is still being fetched

### Requirement: Default Branch Worktree Presentation
The worktrees pane SHALL pin the default-branch row to the second position and render it with a distinct marker and color so it is immediately recognizable as the repository trunk rather than a unit of work.

#### Scenario: Default branch pinned second
- **WHEN** the worktrees pane is rendered and the current/active worktree is pinned first
- **THEN** the default-branch row appears immediately after it (second position)
- **AND** all remaining worktrees follow in their newest-activity order

#### Scenario: Default branch is the current worktree
- **WHEN** the default branch is also the current/active worktree
- **THEN** it remains pinned in the first position and is not duplicated into the second position

#### Scenario: Default branch visual marker
- **WHEN** the default-branch row is rendered in any view that shows the worktree table
- **THEN** it displays a star marker and renders its name and branch in a dedicated accent color distinct from the color used for non-default branches

#### Scenario: No default branch present
- **WHEN** no worktree is marked as the default branch
- **THEN** no row is pinned second on that basis and no star marker or accent color is applied

### Requirement: TUI Input Workflows
The TUI SHALL support keyboard and mouse workflows for selection movement, tab switching, worktree creation, worktree deletion, current-tab worktree deletion, harness selection, refresh, default-branch update, session kill, and quit.

#### Scenario: Navigation
- **WHEN** the user presses movement keys or clicks selectable regions
- **THEN** the TUI updates selection consistently with the active panel

#### Scenario: Create workflow
- **WHEN** the user starts worktree creation
- **THEN** the TUI provides branch-name input followed by base branch selection for new branches

#### Scenario: Dirty delete workflow
- **WHEN** the snapshot reports that a worktree is blocked from removal
- **THEN** the first delete confirmation already states the reason and offers a force delete

#### Scenario: Late refusal
- **WHEN** the daemon refuses a deletion the snapshot did not predict
- **THEN** the TUI reopens the delete confirmation as a force-delete prompt

#### Scenario: Current-tab delete
- **WHEN** the user presses the current-tab delete key
- **THEN** the TUI targets the worktree the pane lives in and closes its tab after removal

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

### Requirement: Shared Deletion State
The daemon SHALL record which worktrees have a removal in flight and SHALL expose that state on the snapshot rows it broadcasts, so every subscribed TUI shows the same deletion state. The daemon SHALL mark a worktree as deleting and broadcast a snapshot **before** it acquires the repository operation lock, and SHALL clear the mark and broadcast again when the removal succeeds, is refused, or fails. The mark SHALL survive a full worktree rescan that replaces the row.

#### Scenario: Delete starts
- **WHEN** the daemon accepts a removal request for a worktree
- **THEN** it marks that worktree as deleting and broadcasts a snapshot before starting any git work

#### Scenario: Non-initiating TUI sees the delete
- **WHEN** one TUI requests a removal and other TUIs are subscribed
- **THEN** every subscribed TUI receives the snapshot marking that row as deleting

#### Scenario: Delete blocked behind another repository operation
- **WHEN** a removal request waits on a repository operation such as an in-flight fetch
- **THEN** the row is already marked as deleting for the whole wait

#### Scenario: Delete refused
- **WHEN** a removal is refused
- **THEN** the daemon clears the deleting mark and broadcasts a snapshot with the row restored

#### Scenario: Delete succeeds
- **WHEN** a removal succeeds
- **THEN** the daemon drops the row and clears its deleting mark

#### Scenario: Rescan during a delete
- **WHEN** a worktree rescan replaces the rows while a removal is in flight
- **THEN** the affected row is still marked as deleting in the resulting snapshot

### Requirement: Deleting Row Presentation
The TUI SHALL render a row marked as deleting distinctly from a normal row, with an active indicator, and SHALL reject a delete request for a row already marked as deleting.

#### Scenario: Row shown as deleting
- **WHEN** a snapshot marks a worktree as deleting
- **THEN** the TUI renders that row with a deletion indicator

#### Scenario: Duplicate delete rejected
- **WHEN** the user requests deletion of a row already marked as deleting
- **THEN** the TUI does not send a second removal request

### Requirement: Up-Front Removal Reason
The TUI SHALL take the removal reason for the selected worktree from the daemon snapshot and SHALL present it in the first confirmation, without a preliminary request to the daemon. The daemon-side check SHALL remain authoritative: when it refuses a removal the snapshot reason did not predict, the TUI SHALL fall back to the force-confirmation flow.

#### Scenario: Blocked worktree confirmation
- **WHEN** the user requests deletion of a worktree whose snapshot reports a blocking reason
- **THEN** the first confirmation already states that reason and offers a force delete
- **AND** no round-trip to the daemon happens before the confirmation appears

#### Scenario: Removable worktree confirmation
- **WHEN** the user requests deletion of a worktree whose snapshot reports no blocking reason
- **THEN** the first confirmation is a plain delete confirmation

#### Scenario: Stale snapshot
- **WHEN** the snapshot reported no blocking reason but the daemon refuses the removal
- **THEN** the TUI reopens the confirmation as a force-delete prompt carrying the daemon's reason

### Requirement: Floating Confirmation Pane
When a deletion is blocked and swamp is running inside Zellij, the TUI SHALL open a Zellij floating pane that shows the blocking reason and the work at risk — the short working-tree status and a diff summary — and offers force delete or cancel. A deletion that is not blocked SHALL keep the existing single-line footer confirmation and SHALL NOT open a pane. When swamp is not inside Zellij, or the pane cannot be opened, the TUI SHALL fall back to the footer force-confirmation prompt.

The pane SHALL report the blocking reason it was given when it was opened and SHALL NOT re-compute the removal verdict. The working-tree status and diff summary it shows SHALL be read from the worktree when the pane renders, so the evidence the user decides on is current even though the reason label is not re-checked.

#### Scenario: Blocked deletion inside Zellij
- **WHEN** the user confirms deletion of a blocked worktree inside Zellij
- **THEN** a floating pane opens showing the blocking reason, the short status, and a diff summary

#### Scenario: Force accepted in the pane
- **WHEN** the user chooses force delete in the floating pane
- **THEN** the removal proceeds with force and the pane closes

#### Scenario: Cancelled in the pane
- **WHEN** the user cancels in the floating pane
- **THEN** the pane closes and the worktree is untouched

#### Scenario: Clean deletion opens no pane
- **WHEN** the user deletes a worktree that is not blocked
- **THEN** the footer confirmation is used and no floating pane is opened

#### Scenario: Outside Zellij
- **WHEN** a blocked deletion is confirmed outside Zellij
- **THEN** the TUI shows the footer force-confirmation prompt instead

#### Scenario: Pane cannot be opened
- **WHEN** opening the floating pane fails
- **THEN** the TUI shows the footer force-confirmation prompt instead

#### Scenario: Reason is not re-checked
- **WHEN** the floating pane opens
- **THEN** it reports the blocking reason it was opened with
- **AND** it does not acquire the repository operation lock to re-compute the verdict

#### Scenario: Evidence is read at render time
- **WHEN** the floating pane renders the work at risk
- **THEN** the short status and diff summary are read from the worktree at that moment

### Requirement: Delete Current Tab Worktree
The TUI SHALL provide a key that deletes the worktree the pane itself lives in and then closes that worktree's Zellij tab. The key SHALL target the pane's own worktree regardless of which row is selected. The key SHALL do nothing when the pane's worktree resolves to the repository default branch's worktree, and SHALL say so, so that a single keystroke on the dashboard cannot delete the trunk worktree. The removal and the tab close SHALL be performed by a detached helper process whose working directory is outside the target worktree, so the work completes after the tab and its panes are gone. The helper SHALL close the tab before removing the directory, so no pane holds a working directory inside it during removal.

#### Scenario: Delete the current worktree
- **WHEN** the user presses the current-tab delete key in a worktree tab's sidebar
- **THEN** swamp deletes that pane's worktree and closes its tab

#### Scenario: Selection is ignored
- **WHEN** the user presses the current-tab delete key while a different row is selected
- **THEN** the pane's own worktree is the target, not the selected row

#### Scenario: Helper outlives the tab
- **WHEN** the tab is closed as part of the current-tab delete
- **THEN** the removal still completes because the helper runs detached outside the target worktree

#### Scenario: Blocked current-tab delete
- **WHEN** the pane's own worktree is blocked from removal
- **THEN** the floating confirmation pane is shown before anything is closed or removed

#### Scenario: No current worktree
- **WHEN** the pane's working directory does not resolve to a worktree
- **THEN** the key does nothing

#### Scenario: Pane resolves to the default-branch worktree
- **WHEN** the user presses the current-tab delete key and the pane's worktree is the default branch's worktree
- **THEN** nothing is deleted and no tab is closed
- **AND** the TUI reports why the key did nothing

#### Scenario: Default-branch worktree is still deletable deliberately
- **WHEN** the user selects the default branch's worktree and presses the selected-row delete key
- **THEN** the normal delete confirmation flow runs
