## MODIFIED Requirements

### Requirement: Dashboard Panes
The dashboard tab SHALL use the workspace layout by default: a persistent Spaces/Agents sidebar on the left, a selected-destination details pane above a real shell on the right, and slim Zellij tab/status chrome. The dashboard SHALL retain worktree, agent, resource, and PR information through the sidebar and details rather than four independent status panes. An explicit classic layout SHALL retain the existing worktree/resource/AI/PR/shell pane arrangement and dashboard percentage sizing.

#### Scenario: Default dashboard
- **WHEN** a new session is generated without an explicit layout choice
- **THEN** it opens a focused dashboard tab with the Spaces/Agents sidebar, selected-destination details, and real shell
- **AND** it does not pre-create worktree tabs or restart any existing session

#### Scenario: Dashboard config present
- **WHEN** classic layout is selected and dashboard column percentages are configured
- **THEN** those percentages are used for the generated classic dashboard panes

#### Scenario: Dashboard config absent
- **WHEN** dashboard layout and width settings are missing
- **THEN** Swamp uses the workspace layout with its built-in sidebar width

#### Scenario: Sidebar width configured
- **WHEN** workspace layout is selected with a supported sidebar width
- **THEN** the generated sidebar uses that width instead of the classic percentage columns

### Requirement: Worktree Tab Panes
Each workspace-layout worktree tab SHALL include the persistent Spaces/Agents sidebar, a dominant suspended agent pane, and lazygit and shell panes in a smaller lower region. Lazygit SHALL use Swamp's managed configuration, and the sidebar SHALL identify the active worktree. Classic-layout worktree tabs SHALL retain lazygit, pinned worktree status, suspended agent, and shell panes in their existing arrangement.

#### Scenario: Worktree tab starts
- **WHEN** a worktree tab is opened in workspace layout
- **THEN** the sidebar, lazygit, agent, and shell panes are configured for that worktree
- **AND** the agent receives the largest individual terminal region

#### Scenario: Agent pane selected harness
- **WHEN** the worktree has an effective Claude or Codex harness
- **THEN** the generated agent pane uses that harness for the worktree
- **AND** it remains suspended until explicitly started

#### Scenario: Classic worktree tab
- **WHEN** a worktree tab is generated with classic layout selected
- **THEN** it retains the original lazygit, pinned worktree status, suspended agent, and shell arrangement

## ADDED Requirements

### Requirement: Workspace Layout Configuration Compatibility
Dashboard configuration SHALL accept layout workspace or classic and sidebar_width between 16 and 60 cells inclusive, defaulting to workspace and 32. Existing worktrees_column, ai_column, and shell_column values SHALL remain accepted and affect classic layout only. Invalid new layout or width values SHALL produce an actionable configuration error.

#### Scenario: Existing percentage configuration
- **WHEN** an existing config contains percentage column settings but no layout setting
- **THEN** new layouts use workspace without rejecting the config
- **AND** choosing classic restores the configured percentage sizing

#### Scenario: Invalid width
- **WHEN** sidebar_width is outside the supported range
- **THEN** configuration loading reports the permitted range instead of generating an invalid sidebar

### Requirement: Non-disruptive Layout Rollout
Layout changes SHALL apply to newly generated sessions and tabs without rewriting existing live layouts, restarting agents, or remapping the user's global Zellij prefix. Users SHALL have documented classic rollback and deliberate session recreation paths.

#### Scenario: Attach after upgrade
- **WHEN** a matching live repo session exists with the previous layout
- **THEN** Swamp attaches using existing startup/version-restart policy without performing an additional presentation-driven restart

#### Scenario: Mixed tabs
- **WHEN** a workspace-layout tab is generated in an existing session that has classic tabs
- **THEN** the old tabs keep their processes/layout and the new tab includes its required sidebar and chrome

#### Scenario: Classic rollback
- **WHEN** the user selects classic and deliberately recreates a session
- **THEN** its generated dashboard and worktree tabs use classic layout without requiring a terminal backend migration
