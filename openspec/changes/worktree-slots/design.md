## Context

Swamp already manages worktrees without git-wt. `src/worktree/create.rs`, `remove.rs`, and the daemon `UpdateDefault` path use `git2` directly. Swamp shares only git-wt's layout (`.bare` plus sibling dirs) and its "dir name = sanitized branch" rule. Code comments and the `worktree-management` spec still describe that layout as "git-wt-style".

One string keys several things today: the worktree dir basename is the tab name, the `.swamp-status.json` key, and the hook key (`hook.rs` resolves the worktree that contains the cwd). With slots, the dir name stays fixed and the branch changes, so per-branch data (session id, session name, user title) must leave the per-worktree record.

The pool model comes from a teammate's `wt-pool` script, shared internally. Swamp keeps its rules: git state decides whether a slot is free, and release detaches instead of deleting. Swamp does not reuse that script's code.

## Goals / Non-Goals

**Goals:**
- One implementation of slot operations that the CLI, the daemon, the TUI, and the Claude hooks all call.
- Correct behavior when the daemon is not running. Claude hooks can run outside a swamp session.
- No new on-disk source of truth for slot ownership.

**Non-Goals:**
- Copy-on-write workspaces. Rift was evaluated and rejected: it does not support ext4, it rejects linked worktrees, and new paths give bazel a cold output base.
- Automatic eviction of busy slots, or moving overflow worktrees into slots.
- Zellij tab names that show titles. Tab names stay equal to the worktree name, because the TUI matches tabs by that name.
- Swap of a dirty branch out of a slot (auto WIP commits or stash).

## Decisions

### D1: Slot identity comes from the dir name
A worktree is a slot when its basename matches `work<N>` for any `N` of 1 or more, independent of the pool size. Then a change to `pool_size` never turns an existing slot into an overflow worktree. Alternatives: a registry file (can drift from git), or `git worktree lock` as a marker (`lock` already means "do not prune", and git-wt and others read it). A name check is free and visible in a shell.

Collision: a branch named `work3` would produce an overflow dir `work3`. Overflow creation adds a `-1` suffix to any name that matches `work<N>` (spec Worktree Naming).

Pool size: `[slots] pool_size` (default 5) in the user config, and the per-repo config (D2) can override it. Allocation considers only `work1`..`work<pool_size>`. When the user lowers the size, swamp keeps the slots above it, and they still classify and release normally. Swamp never deletes them automatically, because they can hold work. `swamp slot list` marks them as outside the pool, and the user removes them with the normal delete action. A size of 0 gives today's behavior: one worktree per branch.

### D2: State lives in git, with a small sidecar dir
- Free/busy: computed from `HEAD`, `git_info()` (already returns dirty counts, conflict, and rebase), and the pin file.
- Pins: `<git-common-dir>/swamp/pins/<slot>`, which holds the reason text.
- Lock: `<git-common-dir>/swamp/slots.lock`, an exclusive `flock`.
- Per-repo config: `<git-common-dir>/swamp/config.toml`.

The common dir is outside every worktree, so `git status` never shows these files, and they are not committed.

### D3: One lock for the CLI and the daemon
`flock` on `slots.lock` serializes slot operations. The daemon's existing `repo_ops` mutex keeps its role inside the daemon. The daemon takes `flock` inside `repo_ops` for slot operations and for `CreateWorktree*`/`RemoveWorktree`, so a CLI alloc cannot race a TUI create. The CLI sends the operation to the daemon when the socket answers, and otherwise runs it in-process under the same `flock`. This mirrors `swamp hook`, which falls back to a direct write when the daemon is unreachable.

Alternative: daemon-only. That fails for Claude hooks in repos with no running session.

### D4: Release reuses the removal checks
Release needs the same facts as `removal_verdict`: dirty state, upstream containment, and PR state. Swamp extracts the shared part ("is this branch's work safe?") into one function that both removal and release call. The PR lookup uses the existing GitHub client (`src/github/`) and does not call the `gh` CLI. When no GitHub token or network exists, the PR check returns "unknown". Release then accepts only the pushed condition.

Branch deletion uses a force delete, because a squash merge leaves the branch unmerged in git's view.

### D5: The TUI create path uses alloc
The `c` picker keeps its UI. Its result goes to the slot allocator, not `create_worktree`. `create_worktree` and `create_worktree_from_base` become the overflow path and a "check out into an existing dir" variant for slots. The slot variant runs `checkout` in the existing worktree with `git2` (`set_head` plus `checkout_head`, with safe checkout), so ignored build output stays in place.

### D6: Branch memory in the status file
`.swamp-status.json` gains a top-level `branches` map: branch → `{session_id, session_name, harness, title}`. Release moves the fields from the slot record into this map. Alloc moves them back. The user title lives only in the branch map, because it always belongs to a branch. The hook keeps keying live status by worktree name, which stays correct: the live agent process belongs to the slot.

### D7: Lifecycle hooks are not in the repo
Hooks come from `[slots.hooks]` in the user config and from the per-repo config in the common dir. A checked-in `.swamp.toml` (rift's model) was rejected for v1: a clone would then run commands that the repo author chose, which needs a trust prompt like direnv. Hooks run through `$SHELL -c` with stdout redirected to stderr, and with a timeout (default 5 minutes, configurable).

```toml
[[slots.hooks.pre-release]]
run = "bazel shutdown"

[[slots.hooks.post-alloc]]
run = "direnv allow"
```

### D8: Claude worktree hooks are opt-in
A `WorktreeCreate` hook in `~/.claude/settings.json` changes `claude --worktree` for every repo on the machine. For that reason, `swamp init` installs it only with `--claude-worktrees`. Commands: `swamp slot alloc --hook-json` and `swamp slot release --hook-json`. Both read stdin.

### D9: Tab lifecycle goes through the existing Zellij helpers
Alloc calls the same open-or-switch logic that the TUI uses on `Enter`, when the repo session exists. Release reuses the detached-process pattern of `delete-tab`: close the tab first, then detach. A slot release from inside its own tab spawns the hidden `delete-tab`-style command with a release mode.

### D10: Title sources
- PR title: from the existing PR snapshot. This adds a `title` field to `PrSummary` if the GraphQL query does not fetch it yet.
- openspec change: the daemon lists `openspec/changes/*` in the worktree (excluding `archive`) and subtracts the names present on `origin/<default>`. Exactly one result gives a title. The daemon computes this only on head change, not on every scan.
- Commit subject: from `HEAD` during the existing scan.

### D11: Drop git-wt
No code calls `git wt`. Swamp removes the references in comments, the `worktree-management` spec, and the README. It adds `swamp clone` so a new repo does not need `git wt clone`. The layout stays the same, so existing git-wt clones keep working without migration.

## Risks / Trade-offs

- [Checkout over a slot fails when ignored build output conflicts with tracked files on the new branch] → Use a safe checkout. On conflict, abort the allocation, leave the slot free, and try the next slot or overflow.
- [A slot's agent keeps running after another process releases it] → Release refuses when the slot's agent is working (spec Agent Safety On Release). Release also closes the tab, which stops idle agents.
- [Claude's own history in `~/.claude/projects/<path>` mixes branches that used the same slot] → Accepted. `--resume <id>` from branch memory picks the correct session. The picker shows sessions from earlier branches.
- [The Claude `WorktreeCreate` / `WorktreeRemove` JSON contract changes] → The hook code reads only `name` and `worktree_path`, and logs the raw JSON to the diagnostic log. A task verifies the contract against the current Claude Code docs.
- [PR state is unknown offline, so release refuses an unpushed branch whose PR is merged] → The user passes `--force`, or retries when online.
- [Users keep running `git wt remove` against slots, which deletes the branch] → Docs state that swamp owns the layout. Optional: the slot's `.git` gitdir lock text can say `managed by swamp`.

## Migration Plan

1. Ship with no data migration. Existing branch-named worktrees become overflow worktrees.
2. Older status files decode, because the `branches` map and the new row fields use serde defaults.
3. Rollback: an older swamp sees `work<N>` dirs as normal worktrees. Swamp keeps pins and the lock under `<common>/swamp/`, and an older swamp ignores them.
4. After release, the user updates their agent rules to use `swamp slot alloc` / `release` instead of `git wt`.
