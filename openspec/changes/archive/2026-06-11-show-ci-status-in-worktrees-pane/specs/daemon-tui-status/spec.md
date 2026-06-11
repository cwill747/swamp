## ADDED Requirements

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
