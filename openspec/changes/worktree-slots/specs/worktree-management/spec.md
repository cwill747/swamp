## MODIFIED Requirements

### Requirement: Worktree Naming
Swamp SHALL derive user-facing worktree names from the worktree path basename. For an overflow worktree, swamp SHALL name the directory after the full branch name with every `/` replaced by `-`, and SHALL add the suffix `-1` when that name has the slot form `work<N>`. For a slot, the name SHALL be the slot name `work<N>`, independent of the branch.

#### Scenario: Simple branch name
- **WHEN** an overflow worktree is created for branch `feature`
- **THEN** the worktree name is `feature`

#### Scenario: Slash branch name
- **WHEN** an overflow worktree is created for branch `users/alice/feature`
- **THEN** the worktree name is `users-alice-feature`

#### Scenario: Branch name matches a slot name
- **WHEN** an overflow worktree is created for branch `work3`
- **THEN** the worktree name is `work3-1`, so swamp does not classify it as a slot

#### Scenario: Branch name matches a slot name above the pool size
- **WHEN** the pool size is 5 and an overflow worktree is created for branch `work9`
- **THEN** the worktree name is `work9-1`

#### Scenario: Slot name
- **WHEN** slot `work2` holds branch `users/alice/feature`
- **THEN** the worktree name is `work2`

### Requirement: Worktree Creation
Swamp SHALL create worktrees under the repository worktree root. The TUI create action and a slot allocation SHALL both place a branch into the lowest free slot, and SHALL create an overflow worktree only when the slot pool is full.

#### Scenario: Existing local branch
- **WHEN** the user creates a worktree for an existing local branch
- **THEN** swamp checks out that local branch in a free slot, or in a new overflow worktree when the pool is full

#### Scenario: Matching remote branch
- **WHEN** the requested branch exists only as a remote-tracking branch
- **THEN** swamp creates a local branch from that remote and checks it out in a free slot or a new overflow worktree

#### Scenario: New branch from base
- **WHEN** the user creates a new branch from a selected base branch, `origin/<base>`, tag, or SHA
- **THEN** swamp creates the new branch and checks it out in a free slot or a new overflow worktree

#### Scenario: Git LFS content
- **WHEN** worktree creation succeeds and LFS content inflation fails
- **THEN** swamp keeps the created worktree and treats LFS inflation as best effort

## ADDED Requirements

### Requirement: Bare Repository Clone
`swamp clone <url> [<dir>]` SHALL create a repository in the bare layout that swamp manages: a container directory with the git data in `.bare`, a `.git` file that points to `.bare`, a fetch refspec for all remote branches, and a worktree for the default branch named after that branch. Swamp SHALL print the default branch worktree path on stdout.

#### Scenario: Clone a repository
- **WHEN** the user runs `swamp clone https://example.com/app.git`
- **THEN** swamp creates `app/.bare`, `app/.git`, and `app/main`, and prints the `app/main` path

#### Scenario: Remote branches visible
- **WHEN** the clone completes
- **THEN** remote-tracking refs exist for every branch on the remote

#### Scenario: Target exists
- **WHEN** the target directory already exists and is not empty
- **THEN** swamp refuses and changes nothing
