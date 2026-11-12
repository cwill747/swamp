## MODIFIED Requirements

### Requirement: Worktree Snapshot Rows
Daemon snapshots SHALL include worktree rows with branch, upstream, ahead/behind, dirty counts, conflict/rebase state, agent status, agent timestamp, session name, head timestamp, harness override, a flag marking whether the row's branch is the repository default branch, a flag marking whether a removal is in flight for that worktree, the worktree kind (`default`, `slot`, or `overflow`), the slot state (`free`, `busy`, or `pinned`) for slot rows, the pin reason, and the resolved title with its source. The default-branch flag SHALL be derived from the repository's configured default branch (the default remote's `HEAD`), and SHALL be false for every row when no default branch can be determined. The deleting, kind, slot state, pin reason, and title fields SHALL decode to inert defaults when a peer omits them. A legacy removal-verdict field MAY remain present for protocol compatibility, but routine scans SHALL leave it empty.

#### Scenario: Snapshot requested
- **WHEN** a client requests or subscribes to a snapshot
- **THEN** each row contains the git, agent, timestamp, harness, default-branch, deleting, kind, slot state, and title fields needed by the TUI

#### Scenario: Snapshot ordering
- **WHEN** rows are emitted
- **THEN** busy slot and overflow rows are sorted by newest head timestamp and then by name, and free and pinned slot rows follow, sorted by slot number

#### Scenario: Default branch flagged
- **WHEN** the repository default branch is known and a worktree has that branch checked out
- **THEN** that worktree's row is marked as the default branch and every other row is not

#### Scenario: Default branch unknown
- **WHEN** the repository default branch cannot be determined
- **THEN** no row is marked as the default branch

#### Scenario: Removal verdict omitted from routine scan
- **WHEN** a worktree would be refused a non-forced removal
- **THEN** its routine snapshot does not compute or carry the blocking reason

#### Scenario: Older peer snapshot
- **WHEN** a snapshot omits the deleting, kind, slot state, or title fields, or includes the legacy removal-verdict field
- **THEN** it still decodes without requiring a background removal check

### Requirement: On-Demand Worktree Tab Opening
The dashboard TUI SHALL open a worktree's Zellij tab only in response to explicit user activation of that worktree, and SHALL NOT open worktree tabs in response to daemon snapshot updates. When the worktree already has a tab, activation SHALL switch to the existing tab rather than open a duplicate. A slot allocation is an explicit action and opens its own tab as the Slot Tab Lifecycle requirement describes. Activation of a free slot row SHALL NOT open a tab.

#### Scenario: User activates a worktree without a tab
- **WHEN** the user activates a worktree in the dashboard while running inside Zellij
- **AND** no tab currently exists for that worktree
- **THEN** the TUI opens a worktree tab for it and switches focus to it

#### Scenario: User activates a worktree that already has a tab
- **WHEN** the user activates a worktree whose tab already exists
- **THEN** the TUI switches to the existing tab instead of opening a duplicate

#### Scenario: New worktree appears without user action
- **WHEN** a new worktree appears in daemon snapshots and no allocation or user activation requested it
- **THEN** the TUI does not open a tab for it

#### Scenario: Free slot activated
- **WHEN** the user activates a free slot row
- **THEN** the TUI does not open a tab and offers to allocate a branch into the slot

#### Scenario: Outside Zellij
- **WHEN** the TUI is not running inside Zellij
- **THEN** worktree activation does not attempt to open Zellij tabs

## ADDED Requirements

### Requirement: Slot Tab Lifecycle
When the repository's Zellij session exists, a slot allocation from the CLI SHALL open a tab for the allocated worktree when it has none, and SHALL give focus back to the tab that had it. An existing tab SHALL stay as it is. An allocation from the Claude `WorktreeCreate` hook, or with `--no-tab`, SHALL NOT open a tab, because Claude runs in that worktree itself. A slot release SHALL close the slot's tab unless the user passes `--no-tab`. When the release runs inside the tab it closes, swamp SHALL complete the release in a process that survives the tab close.

#### Scenario: Allocation with a session
- **WHEN** an agent allocates `parser-fix` into `work2` and the repository session exists
- **THEN** a `work2` tab opens with the agent, lazygit, and shell panes

#### Scenario: Agent allocation keeps the user's view
- **WHEN** an agent allocates a slot while the user looks at the dashboard tab
- **THEN** the new slot tab opens and the dashboard tab keeps focus

#### Scenario: Claude worktree hook
- **WHEN** the `WorktreeCreate` hook allocates a slot
- **THEN** swamp opens no tab

#### Scenario: Allocation without a session
- **WHEN** no repository Zellij session exists
- **THEN** the allocation succeeds and no tab is opened

#### Scenario: Release from inside the slot tab
- **WHEN** the user releases `work2` from a pane in the `work2` tab
- **THEN** swamp closes the `work2` tab and the release still completes

### Requirement: Work Item Presentation
The worktrees pane SHALL show each row's title in place of the worktree name, and SHALL show the slot name (`work<N>`) in a short label column for slot rows. The label SHALL be blank for other rows, whose name repeats the branch. Free and pinned slot rows SHALL be dimmed and SHALL show no branch. Pinned slot rows SHALL show the pin reason as their title. When a peer sends no title, the pane SHALL show the worktree name. When the pane is narrow, the slot label SHALL join the title cell, and the title and branch columns SHALL keep a minimum width ahead of the status columns.

#### Scenario: Busy slot
- **WHEN** `work1` holds a branch whose PR is titled `Fix flaky CI retry logic`
- **THEN** the row shows `Fix flaky CI retry logic` and the label `work1`

#### Scenario: Free slot
- **WHEN** `work3` is free
- **THEN** the row shows the title `free` in the muted style

#### Scenario: Narrow pane
- **WHEN** the worktrees pane is 40 columns wide
- **THEN** each row still shows its title and branch
