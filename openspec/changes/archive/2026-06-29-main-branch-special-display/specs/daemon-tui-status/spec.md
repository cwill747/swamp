## MODIFIED Requirements

### Requirement: Worktree Snapshot Rows
Daemon snapshots SHALL include worktree rows with branch, upstream, ahead/behind, dirty counts, conflict/rebase state, agent status, agent timestamp, session name, head timestamp, harness override, and a flag marking whether the row's branch is the repository default branch. The default-branch flag SHALL be derived from the repository's configured default branch (the default remote's `HEAD`), and SHALL be false for every row when no default branch can be determined.

#### Scenario: Snapshot requested
- **WHEN** a client requests or subscribes to a snapshot
- **THEN** each row contains git, agent, timestamp, harness, and default-branch fields needed by the TUI

#### Scenario: Snapshot ordering
- **WHEN** rows are emitted
- **THEN** they are sorted by newest head timestamp and then by name

#### Scenario: Default branch flagged
- **WHEN** the repository default branch is known and a worktree has that branch checked out
- **THEN** that worktree's row is marked as the default branch and every other row is not

#### Scenario: Default branch unknown
- **WHEN** the repository default branch cannot be determined
- **THEN** no row is marked as the default branch

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

## ADDED Requirements

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
