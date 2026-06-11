# GitHub PR CI Status Specification

## Purpose

Describe how swamp discovers GitHub pull request and check status for worktree
branches and exposes that status to the daemon and TUI.

## Requirements

### Requirement: GitHub CLI Integration
Swamp SHALL query GitHub pull request status through the `gh` CLI for the current repository's worktree branches.

#### Scenario: GitHub CLI available
- **WHEN** PR status refresh runs and `gh` can query the repository
- **THEN** swamp collects PR summaries for matching branches

#### Scenario: GitHub CLI unavailable or query fails
- **WHEN** `gh` cannot provide PR data
- **THEN** swamp treats PR refresh as unavailable

### Requirement: GraphQL Preferred Query
Swamp SHALL prefer batched GraphQL PR queries and fall back to per-branch REST queries when GraphQL cannot provide results.

#### Scenario: GraphQL succeeds
- **WHEN** the batched GraphQL query succeeds
- **THEN** swamp uses its PR and check-rollup results

#### Scenario: GraphQL fails
- **WHEN** the batched GraphQL query fails
- **THEN** swamp attempts REST lookup for individual branches

### Requirement: Pull Request Summary Fields
PR summaries SHALL expose branch name, PR number, title, state, draft flag, URL, check state, check metadata, comment count, and review decision.

#### Scenario: Open pull request found
- **WHEN** a worktree branch has a matching pull request
- **THEN** the PR snapshot includes the pull request's display fields and status fields

#### Scenario: No pull request found
- **WHEN** a worktree branch has no matching pull request
- **THEN** the PR snapshot omits a PR summary for that branch

#### Scenario: Pull request has comments
- **WHEN** a matching pull request has comments available from GitHub
- **THEN** the PR summary includes the pull request comment count

### Requirement: Check Status Aggregation
Swamp SHALL aggregate GitHub check rollups into success, pending, or failure status with passed and total counts where applicable.

#### Scenario: All relevant checks pass
- **WHEN** every non-skipped check succeeds
- **THEN** the aggregate check status is success

#### Scenario: Some checks pending
- **WHEN** no relevant check has failed and at least one relevant check is pending
- **THEN** the aggregate check status is pending with passed and total counts

#### Scenario: A check fails
- **WHEN** any relevant check has failed
- **THEN** the aggregate check status is failure with passed and total counts

### Requirement: Skipped Check Handling
Swamp SHALL exclude skipped checks from totals and treat an all-skipped rollup as success.

#### Scenario: Mixed skipped and passing checks
- **WHEN** a rollup contains skipped checks and passing checks
- **THEN** skipped checks are excluded from aggregate totals

#### Scenario: All checks skipped
- **WHEN** every check in a rollup is skipped
- **THEN** the aggregate check status is success

### Requirement: PR Snapshot Broadcast
The daemon SHALL periodically refresh PR status for known worktree branches and broadcast PR snapshots to subscribed TUI clients.

#### Scenario: PR refresh completes
- **WHEN** the daemon refreshes PR status
- **THEN** subscribers receive an updated PR snapshot

#### Scenario: TUI subscriber connects
- **WHEN** a TUI client subscribes after PR status has already been collected
- **THEN** it receives the current PR snapshot immediately

### Requirement: PR Status Display
The PR status TUI SHALL display PR and CI state from daemon PR snapshots for worktree branches.

#### Scenario: PR summary exists
- **WHEN** the active snapshot contains a PR summary for a worktree branch
- **THEN** the PR panel renders the PR identity, state, review decision, and check status

#### Scenario: PR summary missing
- **WHEN** no PR summary exists for a worktree branch
- **THEN** the PR panel indicates that no PR status is available for that branch

### Requirement: Worktrees Pane PR Status Consumption
The worktrees pane SHALL consume existing PR summaries to display CI and review status for matching worktree branches.

#### Scenario: Matching worktree branch
- **WHEN** a worktree branch has a matching pull request summary
- **THEN** the worktrees pane can derive failed-build count, comment count, and review status from that summary

#### Scenario: PR refresh unavailable
- **WHEN** PR status refresh is unavailable or has not returned data for a branch
- **THEN** the worktrees pane does not fabricate failed-build, comment, or review-status values for that branch
