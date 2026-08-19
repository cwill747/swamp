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
Swamp SHALL delete a worktree directory, prune git worktree metadata, and optionally delete the local branch when removal is allowed. Swamp SHALL refuse a non-forced removal when the process working directory is inside the target worktree, and this check SHALL apply only to the working directory of the process performing the removal.

#### Scenario: Clean worktree
- **WHEN** a clean worktree is removed
- **THEN** swamp removes its directory and prunes git metadata

#### Scenario: Delete branch option
- **WHEN** removal is requested with local branch deletion enabled
- **THEN** swamp deletes the associated local branch after removing the worktree

#### Scenario: Removal from inside the target worktree
- **WHEN** the removing process's working directory is inside the target worktree
- **THEN** swamp refuses the non-forced removal and reports the current-directory reason

### Requirement: Dirty Removal Protection
Swamp SHALL refuse non-forced worktree removal when staged, unstaged, untracked, or conflicted work exists, when the branch has commits the upstream never received, when the branch is not merged anywhere, when the worktree is locked, or when working-tree status cannot be read. Swamp SHALL surface each refusal as a distinct reason so callers can name it. Swamp SHALL distinguish a branch whose configured upstream ref was pruned from a branch that never had an upstream, and SHALL apply merged-branch handling to both.

#### Scenario: Dirty worktree without force
- **WHEN** removal is requested for a dirty worktree without force
- **THEN** swamp refuses removal and reports that force confirmation is required

#### Scenario: Dirty worktree with force
- **WHEN** removal is requested for a dirty worktree with force
- **THEN** swamp skips dirty protection and removes the worktree

#### Scenario: Status read failure
- **WHEN** working-tree status cannot be read during removal
- **THEN** swamp refuses the non-forced removal and reports an unreadable status
- **AND** the worktree directory is left in place

#### Scenario: Pruned upstream on a merged branch
- **WHEN** a branch's configured upstream ref no longer exists and the branch is merged
- **THEN** swamp allows non-forced removal instead of reporting commits on no other branch

#### Scenario: Locked worktree without force
- **WHEN** removal is requested for a locked worktree without force
- **THEN** swamp refuses removal, reports the locked reason, and leaves the directory in place

#### Scenario: All checks precede mutation
- **WHEN** any check refuses a non-forced removal
- **THEN** no directory has been removed, no metadata pruned, and no branch deleted

### Requirement: Removal Verdict
Swamp SHALL compute a removal verdict for a worktree without mutating anything. The verdict SHALL be either "removable" or a single blocking reason drawn from the same set the removal path enforces: uncommitted changes, unpushed commits, commits on no other branch, locked, or unreadable status. The verdict SHALL be derived from one git status read per worktree, so a caller does not pay a second scan to learn why removal is blocked.

#### Scenario: Clean worktree
- **WHEN** a verdict is requested for a clean worktree whose branch is fully merged
- **THEN** the verdict is removable

#### Scenario: Blocked worktree
- **WHEN** a verdict is requested for a worktree that a non-forced removal would refuse
- **THEN** the verdict names the same blocking reason that removal would report

#### Scenario: Verdict does not mutate
- **WHEN** a verdict is computed for any worktree
- **THEN** the worktree directory, its git metadata, and its branch are unchanged

### Requirement: Merged Branch Removal
Swamp SHALL treat a branch as safe to delete when its work is already present elsewhere, even when the branch's own commits were rewritten. A branch SHALL be treated as merged when its tip is reachable from the repository default branch, when its tip is reachable from any other local or remote branch, or when the most recent pull request for that branch is merged and its head commit matches the local branch tip.

#### Scenario: Squash-merged branch with a pruned remote
- **WHEN** a branch's pull request was squash-merged, the remote branch was deleted, and a fetch pruned the remote-tracking ref
- **THEN** swamp does not report commits on no other branch
- **AND** non-forced removal is allowed

#### Scenario: Branch merged into the default branch
- **WHEN** a branch's tip is reachable from the repository default branch
- **THEN** non-forced removal is allowed

#### Scenario: Branch with unmerged local work
- **WHEN** a branch has commits reachable from no other branch and no merged pull request
- **THEN** swamp refuses non-forced removal and reports commits on no other branch

#### Scenario: Local commit added after a merged pull request
- **WHEN** the local branch tip no longer matches the merged pull request head
- **THEN** the pull request does not prove that the new tip is merged
- **AND** swamp refuses non-forced removal when no other branch contains the tip

### Requirement: Bounded Reachability Probe
The reachability probe SHALL check the repository default branch first and SHALL stop as soon as any branch proves the tip reachable, instead of walking every local and remote branch in the repository on every check.

#### Scenario: Tip merged into the default branch
- **WHEN** the tip is reachable from the default branch
- **THEN** the probe returns reachable without examining other branches

#### Scenario: Tip merged into a non-default branch
- **WHEN** the tip is reachable only from some branch other than the default branch
- **THEN** the probe still returns reachable

#### Scenario: Default branch unknown
- **WHEN** the repository default branch cannot be determined
- **THEN** the probe falls back to scanning the remaining branches and stops at the first match
