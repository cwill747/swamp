## ADDED Requirements

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

## MODIFIED Requirements

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
