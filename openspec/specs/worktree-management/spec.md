# Worktree Management Specification

## Purpose

Describe how swamp discovers, displays, creates, updates, and removes git
worktrees.

## Requirements

### Requirement: Worktree Discovery
Swamp SHALL list linked worktrees from the git repository and include the main worktree for non-bare repositories.

#### Scenario: Normal repository
- **WHEN** a non-bare repository is inspected
- **THEN** the main worktree is included in the discovered worktree list

#### Scenario: Linked worktrees
- **WHEN** linked worktrees exist
- **THEN** each existing linked worktree path is included

#### Scenario: Missing worktree path
- **WHEN** a linked worktree path no longer exists on disk
- **THEN** swamp skips that path instead of producing a row for it

### Requirement: Worktree Naming
Swamp SHALL derive user-facing worktree names from the worktree path basename and use branch basenames for git-wt-style worktree names when branch names contain slashes.

#### Scenario: Simple branch name
- **WHEN** the branch name is `feature`
- **THEN** the worktree name is `feature`

#### Scenario: Slash branch name
- **WHEN** the branch name is `users/alice/feature`
- **THEN** the git-wt-style worktree name is `feature`

### Requirement: Git Status Rows
Worktree status rows SHALL report branch, upstream, ahead count, behind count, staged count, unstaged count, untracked count, conflict state, rebase state, and head timestamp.

#### Scenario: Clean worktree
- **WHEN** a worktree has no file changes
- **THEN** its dirty counts are zero and conflict state is false

#### Scenario: Dirty worktree
- **WHEN** a worktree has staged, unstaged, untracked, or conflicted changes
- **THEN** the corresponding counts or conflict state are reflected in its row

#### Scenario: Rebase in progress
- **WHEN** a worktree has an in-progress rebase
- **THEN** the row marks rebase state as true

### Requirement: Detached Worktree Labels
Swamp SHALL label detached worktrees with a detached identifier when branch information is unavailable.

#### Scenario: Detached HEAD
- **WHEN** a worktree is detached at a commit
- **THEN** swamp reports a detached label based on that commit

#### Scenario: Unreadable HEAD
- **WHEN** branch resolution fails
- **THEN** swamp falls back to detached/default row data instead of failing the entire status scan

### Requirement: Branch Listing
The branch picker SHALL list local branches before remote branches, skip remote `HEAD`, hide remote branches shadowed by local branches, mark already checked-out branches, and mark the default branch.

#### Scenario: Local and remote branch share a short name
- **WHEN** both a local branch and a remote branch have the same short name
- **THEN** the branch picker shows the local branch and hides the shadowed remote branch

#### Scenario: Branch checked out elsewhere
- **WHEN** a local branch is already checked out in another worktree
- **THEN** the branch picker marks it as checked out

#### Scenario: Default branch present
- **WHEN** the repository default branch is known
- **THEN** the branch picker marks it as default

### Requirement: Worktree Creation
Swamp SHALL create worktrees under the repository worktree root using git-wt-style names.

#### Scenario: Existing local branch
- **WHEN** the user creates a worktree for an existing local branch
- **THEN** swamp creates a worktree checked out to that local branch

#### Scenario: Matching remote branch
- **WHEN** the requested branch exists only as a remote-tracking branch
- **THEN** swamp creates a local branch from that remote and checks it out in a new worktree

#### Scenario: New branch from base
- **WHEN** the user creates a new branch from a selected base branch, `origin/<base>`, tag, or SHA
- **THEN** swamp creates the new branch and checks it out in a new worktree

#### Scenario: Git LFS content
- **WHEN** worktree creation succeeds and LFS content inflation fails
- **THEN** swamp keeps the created worktree and treats LFS inflation as best effort

### Requirement: Default Branch Update
The update action SHALL fetch all remotes and fast-forward only the checked-out default branch worktree against `origin/<default>`.

#### Scenario: Default branch worktree exists
- **WHEN** update is requested and the default branch is checked out in a worktree
- **THEN** swamp fetches remotes and attempts a fast-forward update for that worktree

#### Scenario: No checked-out default branch
- **WHEN** update is requested and no worktree is checked out to the default branch
- **THEN** swamp skips the fast-forward step and refreshes worktree status

#### Scenario: Non-fast-forward update
- **WHEN** the default branch cannot be fast-forwarded
- **THEN** swamp returns an error to the TUI

### Requirement: Worktree Removal
Swamp SHALL delete a worktree directory, prune git worktree metadata, and optionally delete the local branch when removal is allowed.

#### Scenario: Clean worktree
- **WHEN** a clean worktree is removed
- **THEN** swamp removes its directory and prunes git metadata

#### Scenario: Delete branch option
- **WHEN** removal is requested with local branch deletion enabled
- **THEN** swamp deletes the associated local branch after removing the worktree

### Requirement: Dirty Removal Protection
Swamp SHALL refuse non-forced worktree removal when staged, unstaged, untracked, or conflicted work exists and SHALL surface dirty removal as a distinct condition.

#### Scenario: Dirty worktree without force
- **WHEN** removal is requested for a dirty worktree without force
- **THEN** swamp refuses removal and reports that force confirmation is required

#### Scenario: Dirty worktree with force
- **WHEN** removal is requested for a dirty worktree with force
- **THEN** swamp skips dirty protection and removes the worktree

#### Scenario: Status read failure
- **WHEN** dirty status cannot be read during removal
- **THEN** swamp treats the worktree as clean for removal purposes
