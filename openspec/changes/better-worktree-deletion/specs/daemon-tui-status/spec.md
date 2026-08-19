## ADDED Requirements

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

## MODIFIED Requirements

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
