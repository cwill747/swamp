## MODIFIED Requirements

### Requirement: PR Snapshot Broadcast
The daemon SHALL refresh PR status for known worktree branches and broadcast PR snapshots to subscribed TUI clients. Each PR snapshot SHALL indicate whether a PR fetch has ever completed, so that a never-fetched (loading) state is distinguishable from a completed-but-empty result. The daemon SHALL trigger a PR fetch when a subscriber first connects rather than only on the periodic poll interval.

#### Scenario: PR refresh completes
- **WHEN** the daemon refreshes PR status
- **THEN** subscribers receive an updated PR snapshot marked as fetched

#### Scenario: Fetch triggered on subscribe
- **WHEN** a TUI client subscribes and no successful PR fetch has happened yet
- **THEN** the daemon initiates a PR fetch without waiting for the next poll interval

#### Scenario: Loading state before first fetch
- **WHEN** a PR snapshot is broadcast before any PR fetch has completed
- **THEN** the snapshot indicates a pending/loading state rather than an empty result

#### Scenario: TUI subscriber connects after fetch
- **WHEN** a TUI client subscribes after PR status has already been collected
- **THEN** it receives the current PR snapshot immediately, marked as fetched

### Requirement: PR Status Display
The PR status TUI SHALL display PR and CI state from daemon PR snapshots for worktree branches, and SHALL distinguish a loading state from a completed-but-empty result.

#### Scenario: PR summary exists
- **WHEN** the active snapshot contains a PR summary for a worktree branch
- **THEN** the PR panel renders the PR identity, state, review decision, and check status

#### Scenario: Loading before first fetch
- **WHEN** no PR fetch has completed yet and no PR summaries are present
- **THEN** the PR panel indicates that PR status is loading rather than that there are no PRs

#### Scenario: Fetched and empty
- **WHEN** a PR fetch has completed and no worktree branch has a matching PR summary
- **THEN** the PR panel indicates that there are no PRs for any worktree branch

#### Scenario: PR summary missing for a branch
- **WHEN** a fetch has completed and no PR summary exists for a specific worktree branch
- **THEN** the PR panel indicates that no PR status is available for that branch

### Requirement: Worktrees Pane PR Status Consumption
The worktrees pane SHALL consume existing PR summaries to display CI and review status for matching worktree branches, and SHALL not fabricate status for branches whose PR data is still loading or absent. While PR data is loading, the pane SHALL render a dedicated loading indicator for affected rows that is visually distinct from the CI build-running (pending checks) indicator.

#### Scenario: Matching worktree branch
- **WHEN** a worktree branch has a matching pull request summary
- **THEN** the worktrees pane can derive failed-build count, comment count, and review status from that summary

#### Scenario: PR data loading
- **WHEN** no PR fetch has completed yet for the repository
- **THEN** the worktrees pane does not fabricate failed-build, comment, or review-status values and renders a loading indicator for rows lacking a summary

#### Scenario: Loading indicator distinct from build-running
- **WHEN** the worktrees pane renders the PR-data loading indicator
- **THEN** that indicator is visually distinct from the indicator used for a pull request with checks still running (CI pending)

#### Scenario: PR refresh unavailable
- **WHEN** PR status refresh is unavailable or has not returned data for a branch
- **THEN** the worktrees pane does not fabricate failed-build, comment, or review-status values for that branch
