## MODIFIED Requirements

### Requirement: Zellij Layout Generation
Launch SHALL generate a layout consisting of a single focused dashboard tab and SHALL NOT pre-create worktree tabs. The number of tabs in the generated layout SHALL NOT depend on the number of discovered worktrees.

#### Scenario: Bare repository layout
- **WHEN** the target repository is bare or uses a bare worktree layout
- **THEN** the generated session starts with a single focused dashboard tab
- **AND** no worktree tabs are pre-created

#### Scenario: Normal repository layout
- **WHEN** the target repository is not bare
- **THEN** the generated session starts with a single focused dashboard tab
- **AND** no worktree tabs are pre-created

#### Scenario: Many worktrees
- **WHEN** the repository has many discovered worktrees
- **THEN** the generated layout still contains only the focused dashboard tab
