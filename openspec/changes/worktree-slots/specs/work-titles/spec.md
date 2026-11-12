## Purpose

Gives each worktree row a human title that says what the work is, because slot directory names carry no meaning about the branch they hold.

## ADDED Requirements

### Requirement: Title Resolution
Swamp SHALL give each worktree row a title and the source of that title. Swamp SHALL use the first available source in this order: the user title for the branch, the title of the branch's open or most recent PR, the Claude session name, the active openspec change on the branch, the subject of the branch's last commit, and the branch name. A free slot SHALL have the title `free`. A pinned slot SHALL use its pin reason as its title.

#### Scenario: User title set
- **WHEN** the branch has a user title and a PR title
- **THEN** the row shows the user title

#### Scenario: PR title only
- **WHEN** the branch has no user title and has a PR titled `Fix flaky CI retry logic`
- **THEN** the row shows `Fix flaky CI retry logic`

#### Scenario: Session name fallback
- **WHEN** the branch has no user title and no PR, and the Claude session name is `Worktree slots exploration`
- **THEN** the row shows `Worktree slots exploration`

#### Scenario: openspec change fallback
- **WHEN** the higher sources are absent and the branch adds exactly one openspec change, `worktree-slots`, that the default branch does not have
- **THEN** the row shows `worktree-slots`

#### Scenario: Several openspec changes
- **WHEN** the branch adds more than one openspec change
- **THEN** swamp skips the openspec source and uses the next source

#### Scenario: Commit subject fallback
- **WHEN** only the commit and branch sources are available
- **THEN** the row shows the last commit subject

#### Scenario: Free slot
- **WHEN** a slot is free
- **THEN** its row title is `free`

### Requirement: User Titles
`swamp title <text>` SHALL set the user title for the branch of the worktree that contains the current directory, and `swamp title --clear` SHALL remove it. The TUI SHALL let the user set or clear the title of the selected row. Swamp SHALL store user titles per branch, so a title follows its branch across slots.

#### Scenario: Set from the CLI
- **WHEN** the user runs `swamp title "Bump zellij"` in slot `work2` on branch `zellij-bump`
- **THEN** every row for `zellij-bump` shows `Bump zellij`

#### Scenario: Title survives release and reallocation
- **WHEN** the user releases `zellij-bump` from `work2` and later allocates it into `work4`
- **THEN** the `work4` row shows `Bump zellij`

#### Scenario: Clear
- **WHEN** the user runs `swamp title --clear`
- **THEN** the row falls back to the next title source

#### Scenario: Detached worktree
- **WHEN** the user runs `swamp title` in a worktree with a detached `HEAD`
- **THEN** swamp refuses and reports that no branch is checked out
