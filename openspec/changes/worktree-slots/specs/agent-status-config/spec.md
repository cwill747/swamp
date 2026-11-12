## MODIFIED Requirements

### Requirement: Claude Hook Management
Initialization SHALL install or update swamp-managed Claude hooks while preserving unrelated user hooks. When the user passes `--claude-worktrees`, initialization SHALL also install `WorktreeCreate` and `WorktreeRemove` hooks that call slot allocation and slot release. Without that flag, initialization SHALL leave existing worktree hooks unchanged.

#### Scenario: Existing foreign hooks
- **WHEN** Claude settings contain hooks not managed by swamp
- **THEN** `swamp init` preserves those hooks

#### Scenario: Stale swamp hooks
- **WHEN** Claude settings contain older swamp hook commands
- **THEN** `swamp init` updates them to the current commands

#### Scenario: Repeated init
- **WHEN** `swamp init` is run more than once
- **THEN** Claude hook configuration remains idempotent

#### Scenario: Worktree hooks opt-in
- **WHEN** the user runs `swamp init --claude-worktrees`
- **THEN** Claude settings contain swamp `WorktreeCreate` and `WorktreeRemove` hooks

#### Scenario: Foreign worktree hook exists
- **WHEN** Claude settings already contain a `WorktreeCreate` hook not managed by swamp and the user passes `--claude-worktrees`
- **THEN** `swamp init` does not replace it and reports the conflict

## ADDED Requirements

### Requirement: Claude Worktree Hook Protocol
The swamp `WorktreeCreate` hook SHALL read the hook JSON from stdin, allocate a worktree for the JSON `name` field, and print only the worktree path on stdout. The swamp `WorktreeRemove` hook SHALL read the JSON `worktree_path` field and release that worktree. A refused release SHALL leave the worktree in place and SHALL report the reason on stderr.

#### Scenario: Claude creates a worktree
- **WHEN** Claude Code runs the `WorktreeCreate` hook with name `parser-fix`
- **THEN** the hook prints the path of the slot or overflow worktree that now holds `parser-fix`

#### Scenario: Claude removes a dirty worktree
- **WHEN** Claude Code runs the `WorktreeRemove` hook for a slot with uncommitted changes
- **THEN** the slot stays on its branch and stderr names the reason

### Requirement: Branch Memory
On release, swamp SHALL move the slot's session name, session id, and harness override into a record keyed by the branch, and SHALL clear the slot's agent record. When swamp allocates a branch that has a branch record, swamp SHALL restore those fields to the new worktree's agent record. Swamp SHALL delete the branch record when it deletes the branch.

#### Scenario: Reopen a released branch
- **WHEN** `fix-ci` was released from `work1` with session id `abc-123` and later allocated into `work3`
- **THEN** the `work3` agent record has session id `abc-123`, and the `work3` tab resumes that Claude session

#### Scenario: New allocation into a reused slot
- **WHEN** `work1` is released and then allocated to a new branch
- **THEN** the `work1` agent record carries no session fields from the earlier branch

#### Scenario: Branch deleted on release
- **WHEN** release deletes branch `fix-ci` because its PR is merged
- **THEN** swamp deletes the `fix-ci` branch record
