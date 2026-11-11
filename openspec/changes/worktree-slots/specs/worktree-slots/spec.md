## Purpose

Lets swamp keep a small fixed pool of reusable worktrees ("slots") so branches share warm build caches and bounded disk space, with dedicated overflow worktrees when the pool is full.

## ADDED Requirements

### Requirement: Slot Pool
Swamp SHALL manage a pool of slot worktrees named `work1` through `work<P>` under the repository worktree root, where `P` is the pool size. Swamp SHALL read the pool size from `[slots] pool_size` in the user config, and the per-repository config SHALL override it. The default pool size SHALL be 5. A pool size of 0 SHALL turn off the pool, so every allocation creates an overflow worktree. Swamp SHALL create a slot only when an allocation needs one and no existing slot is free. Swamp SHALL treat any worktree whose directory name is `work<N>` with `N` of 1 or more as a slot, and every other linked worktree except the default branch worktree as an overflow worktree. Allocation SHALL use only slots with `N` no larger than the pool size.

#### Scenario: First allocation in a repository
- **WHEN** no slot exists and the user allocates a branch
- **THEN** swamp creates `work1` and allocates the branch into it

#### Scenario: Slots exist but none is free
- **WHEN** slots `work1` and `work2` exist, both are busy, and the user allocates a branch
- **THEN** swamp creates `work3` and allocates the branch into it

#### Scenario: Pool is full
- **WHEN** the pool size is 5, and `work1` through `work5` exist and all are busy
- **THEN** swamp does not create `work6`

#### Scenario: Default pool size
- **WHEN** no config sets `pool_size`
- **THEN** the pool size is 5

#### Scenario: Per-repository override
- **WHEN** the user config sets `pool_size = 5` and the per-repository config sets `pool_size = 2`
- **THEN** the pool size for that repository is 2

#### Scenario: Pool turned off
- **WHEN** the pool size is 0
- **THEN** every allocation creates an overflow worktree

#### Scenario: Pool size lowered
- **WHEN** the pool size changes from 5 to 3 while `work4` and `work5` exist
- **THEN** swamp keeps `work4` and `work5` as slots, allocation does not choose them, and `swamp slot list` marks them as outside the pool

#### Scenario: Existing branch-named worktree
- **WHEN** a linked worktree named `feature-x` exists
- **THEN** swamp classifies it as an overflow worktree

### Requirement: Slot State Classification
Swamp SHALL classify each slot from git and pin state only, with no separate ownership registry. A slot SHALL be free only when its `HEAD` is detached, its working tree has no staged, unstaged, untracked, or conflicted changes, no rebase or merge is in progress, and no pin exists for it. A slot with a branch checked out SHALL be busy. A detached slot with changes SHALL be busy. A detached, clean slot with a pin SHALL be pinned.

#### Scenario: Detached and clean
- **WHEN** a slot has a detached `HEAD`, a clean tree, and no pin
- **THEN** swamp classifies it as free

#### Scenario: Branch checked out
- **WHEN** a slot has branch `fix-ci` checked out
- **THEN** swamp classifies it as busy on `fix-ci`

#### Scenario: Detached with untracked files
- **WHEN** a slot has a detached `HEAD` and one untracked file
- **THEN** swamp classifies it as busy

#### Scenario: Pinned
- **WHEN** a slot has a detached `HEAD`, a clean tree, and a pin with reason `review #172`
- **THEN** swamp classifies it as pinned and reports the reason

### Requirement: Slot Allocation
`swamp slot alloc <name>` SHALL give the caller a worktree for branch `<name>` and print only that worktree's absolute path on stdout. Swamp SHALL fetch the default remote before it allocates, and SHALL continue with cached refs when the fetch fails. When a worktree already has the branch checked out, swamp SHALL print that worktree's path and change nothing. When a local or remote-tracking branch `<name>` exists, swamp SHALL check out that branch. Otherwise swamp SHALL create branch `<name>` from `origin/<default>` with no upstream. Swamp SHALL choose the free slot with the lowest number. When no slot is free and the pool is full, swamp SHALL create an overflow worktree for the branch.

#### Scenario: New branch into a free slot
- **WHEN** `work2` is the lowest free slot and no branch `parser-fix` exists
- **THEN** swamp creates `parser-fix` from `origin/<default>`, checks it out in `work2`, and prints the `work2` path

#### Scenario: Existing pushed branch
- **WHEN** branch `fix-ci` exists on the remote only and a slot is free
- **THEN** swamp creates a local `fix-ci` that tracks the remote branch and checks it out in the free slot

#### Scenario: Branch already in a worktree
- **WHEN** `fix-ci` is already checked out in `work3`
- **THEN** swamp prints the `work3` path and changes no worktree

#### Scenario: Pool full
- **WHEN** every slot in the pool is busy or pinned, and the pool has reached its size
- **THEN** swamp creates a branch-named overflow worktree for the branch and prints its path

#### Scenario: Fetch fails
- **WHEN** the fetch of the default remote fails
- **THEN** swamp writes a warning to stderr and allocates from the cached refs

#### Scenario: Name with unsafe characters
- **WHEN** the name contains characters that are not valid in a branch name
- **THEN** swamp replaces each such character with `-` before it creates the branch

### Requirement: Slot Release
`swamp slot release [<target>]` SHALL return a slot to the pool by detaching its `HEAD` at the branch tip and keeping its directory and untracked-ignored build output. The target SHALL be a slot name, a worktree path, or a branch name, and SHALL default to the worktree that contains the current directory. Without `--force`, swamp SHALL refuse release when the tree has changes, or when the branch has commits that its remote branch does not contain and the branch has no merged or closed PR. When the branch's PR is merged or closed, swamp SHALL delete the local branch and its remote-tracking ref after it detaches the slot. Release SHALL clear the slot's pin.

#### Scenario: Pushed branch
- **WHEN** slot `work2` is clean on `fix-ci` and `origin/fix-ci` contains `HEAD`
- **THEN** swamp detaches `work2` and keeps the `fix-ci` branch

#### Scenario: Merged PR
- **WHEN** slot `work2` is clean on `fix-ci` and the PR for `fix-ci` is merged
- **THEN** swamp detaches `work2` and deletes the local `fix-ci` branch

#### Scenario: Squash-merged PR
- **WHEN** the PR for the branch was squash-merged, so git does not see the branch as merged
- **THEN** swamp still deletes the local branch

#### Scenario: Dirty slot
- **WHEN** the slot has modified or untracked files
- **THEN** swamp refuses release, names the reason, and exits with a non-zero status

#### Scenario: Unpushed commits without a finished PR
- **WHEN** the branch has commits that are not on its remote branch and has no merged or closed PR
- **THEN** swamp refuses release and reports the number of unpushed commits

#### Scenario: Already free
- **WHEN** the slot is already detached
- **THEN** swamp clears any pin and exits with a zero status

#### Scenario: Overflow target
- **WHEN** the target is an overflow worktree
- **THEN** swamp keeps the worktree, writes a note to stderr, and exits with a zero status

### Requirement: Agent Safety On Release
Swamp SHALL refuse a non-forced release of a slot whose agent status is `working` when the caller does not run inside that slot.

#### Scenario: Another agent releases a working slot
- **WHEN** an agent in `work1` releases `work3` and the agent in `work3` is working
- **THEN** swamp refuses the release

#### Scenario: Agent releases its own slot
- **WHEN** an agent in `work3` releases `work3`
- **THEN** swamp applies only the normal release checks

### Requirement: Slot Pins
`swamp slot pin <slot> <reason>` SHALL mark a detached, clean slot as pinned so allocation skips it. `swamp slot unpin <slot>` SHALL remove the pin. Swamp SHALL store pins under the git common directory, outside every worktree.

#### Scenario: Pin a free slot
- **WHEN** the user pins free slot `work4` with reason `review #172`
- **THEN** allocation skips `work4` until the user unpins or releases it

#### Scenario: Pin a busy slot
- **WHEN** the user pins a slot that has a branch checked out
- **THEN** swamp refuses the pin and reports that the slot is busy

### Requirement: Slot Listing
`swamp slot list` SHALL print one line per slot and overflow worktree with its name, kind, state, branch, and pin reason. `--json` SHALL print the same data as JSON.

#### Scenario: Mixed pool
- **WHEN** `work1` is busy on `fix-ci`, `work2` is free, and overflow worktree `spike-x` exists
- **THEN** the listing shows `work1` busy with `fix-ci`, `work2` free, and `spike-x` as overflow

### Requirement: Serialized Slot Operations
Swamp SHALL serialize allocation, release, pin, and unpin across all processes for one repository with a lock under the git common directory. The daemon and the CLI SHALL take the same lock. Two concurrent allocations SHALL never receive the same slot.

#### Scenario: Concurrent allocations
- **WHEN** two agents allocate at the same time and one slot is free
- **THEN** one agent receives the free slot and the other receives a new slot or an overflow worktree

#### Scenario: Daemon not running
- **WHEN** the daemon is not running
- **THEN** the CLI still takes the lock and completes the operation

### Requirement: Slot Lifecycle Hooks
Swamp SHALL run configured `post-alloc` hooks in the worktree after allocation, and configured `pre-release` hooks in the slot after the release checks pass and before the detach. Swamp SHALL run each hook with `sh -c` and SHALL stop at the first failed hook. Swamp SHALL read hooks from the user config and from a per-repository config under the git common directory, and SHALL run user hooks first. Hooks SHALL receive `SWAMP_WORKTREE`, `SWAMP_NAME`, `SWAMP_KIND`, `SWAMP_BRANCH`, and `SWAMP_REPO_ROOT` in their environment. Hook output SHALL go to stderr so that stdout carries only the path. `--no-hooks` SHALL skip hooks.

#### Scenario: post-alloc hook succeeds
- **WHEN** a `post-alloc` hook `direnv allow` is configured
- **THEN** swamp runs it in the allocated worktree before it prints the path

#### Scenario: post-alloc hook fails
- **WHEN** a `post-alloc` hook exits with a non-zero status
- **THEN** the allocation stays in place, swamp writes a warning to stderr, prints the path, and exits with a zero status

#### Scenario: pre-release hook fails
- **WHEN** a `pre-release` hook exits with a non-zero status and the user did not pass `--force`
- **THEN** swamp does not detach the slot and exits with a non-zero status

#### Scenario: Hook output
- **WHEN** a hook prints text to stdout
- **THEN** swamp sends that text to stderr

#### Scenario: Release refused before hooks
- **WHEN** a release is refused because the slot is dirty
- **THEN** swamp does not run the `pre-release` hooks

#### Scenario: Hook timeout
- **WHEN** a hook runs longer than its time limit
- **THEN** swamp stops the hook and every process it started, and treats the hook as failed
