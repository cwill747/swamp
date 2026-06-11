## MODIFIED Requirements

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

## ADDED Requirements

### Requirement: Worktrees Pane PR Status Consumption
The worktrees pane SHALL consume existing PR summaries to display CI and review status for matching worktree branches.

#### Scenario: Matching worktree branch
- **WHEN** a worktree branch has a matching pull request summary
- **THEN** the worktrees pane can derive failed-build count, comment count, and review status from that summary

#### Scenario: PR refresh unavailable
- **WHEN** PR status refresh is unavailable or has not returned data for a branch
- **THEN** the worktrees pane does not fabricate failed-build, comment, or review-status values for that branch
