## Why

Worktree deletion is the roughest workflow in swamp. Three problems compound:

1. **The delete is invisible to every other TUI.** `pending_delete` and the
   "Deleting X…" status live only in the pane that pressed `y`. Every other
   sidebar and the dashboard show the row as normal until it vanishes.
2. **The refusal reason arrives late, and it is often wrong.** The user presses
   `d`, then `y`, then waits an unbounded time (the daemon must take
   `repo_ops`, which a `git fetch --all` can hold for up to 60s), then gets a
   *second* prompt saying "has commits on no other branch". For the common
   agent workflow — open a PR, squash-merge it, let GitHub delete the remote
   branch — that message is a false positive: the work **is** merged, but the
   squash rewrote the commits and the upstream ref was pruned, so
   `reachable_from_other_branch` says no. A branch is deletable while its
   remote ref exists and becomes undeletable the moment GitHub cleans it up.
3. **You cannot delete the worktree you are sitting in.** The sidebar TUI, its
   panes, and often the daemon itself have a cwd inside the target directory.
   Deleting it and then closing the tab needs a process that outlives the tab.

## What Changes

### Deletion state shared across every TUI

- The daemon tracks in-flight removals in `DaemonState` and projects a
  `deleting` flag onto every affected `WorktreeRow`.
- The flag is set **before** `repo_ops` is acquired and broadcast immediately,
  so the whole session shows the row as deleting during the lock wait, not just
  after the removal starts.
- The flag is cleared and re-broadcast on success, refusal, or failure.
- All TUIs render the deleting row with a spinner and reject a second `d` on a
  row already marked deleting.

### Refusal reasons computed up front, not after the request

- `scan_worktrees` computes a per-row removal verdict alongside `GitInfo` and
  carries it in the snapshot as `removal_block`.
- The first confirmation prompt already states the reason. The
  refuse → re-prompt → force round-trip disappears for the normal case; the
  daemon-side check stays as the authoritative guard against a stale snapshot.

### Fixes to the safety checks themselves

- **A pruned upstream is no longer treated as "never had one".**
  `upstream_gone` is now distinguished from "no upstream configured", matching
  what `git_info` already reports to the TUI.
- **Merge detection consults the default branch and PR state.** A branch whose
  tip is reachable from the default branch, or whose most recent PR is `MERGED`
  at the same head commit, is deletable without force. This unblocks the
  squash-merge case without ignoring later local commits.
- **The reachability probe is bounded.** It checks the default branch first and
  short-circuits, instead of running a `graph_descendant_of` revwalk against
  every local and remote branch in the repo.
- **The `CurrentDirectory` refusal stops firing on the daemon's own cwd.** The
  detached daemon inherits its cwd from whichever pane first started it, so a
  worktree tab that happened to launch the daemon could never be deleted. The
  daemon now chdirs to the git common directory at startup.
- The stale `Status read failure` scenario in `worktree-management` is
  corrected: swamp refuses removal on an unreadable status, it does not treat
  the worktree as clean.

### Delete the current tab's worktree and close its tab

- New key `D`: delete the worktree this pane lives in, then close its Zellij
  tab. `d` keeps deleting the selected row.
- The work runs in a detached `swamp delete-tab` helper, in its own process
  group with a cwd outside the target — the same pattern `relaunch-tab`
  already uses — so it survives the tab it is closing.
- The helper closes the tab first (terminating the panes holding the cwd), then
  removes the worktree, then reports failures to the daemon log.

### A Zellij floating pane for blocked deletions

- When the verdict says the worktree is blocked, swamp opens a floating pane
  (`zellij action new-pane --floating --close-on-exit`) instead of the
  one-line footer prompt. Narrow sidebars cannot show what is about to be lost;
  a floating pane can.
- The pane shows the block reason, `git status --short`, and a diffstat, and
  offers force-delete or cancel.
- Clean deletions keep the existing instant footer confirm — no pane spawn.
- Outside Zellij, and when the pane cannot be opened, the footer force-prompt
  remains the fallback.

## Capabilities

### New Capabilities

None. Every change extends behavior already owned by an existing spec.

### Modified Capabilities

- `worktree-management`: removal safety checks gain merged-branch and
  pruned-upstream handling, a bounded reachability probe, a precomputed
  per-row removal verdict, and a corrected status-read-failure scenario.
- `daemon-tui-status`: snapshot rows carry `deleting` and `removal_block`;
  the daemon broadcasts deletion start/end; the TUI gains the `D` key, the
  shared deleting indicator, the up-front refusal reason, and the floating
  confirmation pane.
- `repo-session-launch`: the daemon sets its working directory to the git
  common directory so it never pins a worktree it is asked to delete.

## Impact

**Code**

- `src/worktree/remove.rs` — split the safety checks into a reusable verdict
  function; fix pruned-upstream and merged-branch handling; bound the
  reachability probe.
- `src/worktree/model.rs` — `RemoveRefusedReason` becomes serializable;
  add the removal-verdict type.
- `src/worktree/status.rs` — surface the data the verdict needs from one scan.
- `src/daemon/state.rs` — `deleting` set, `removal_block` on `WorktreeRow`,
  verdict computed in `scan_worktrees`.
- `src/daemon/mod.rs` — broadcast deletion start/end; chdir to the common dir.
- `src/daemon/socket.rs` — no new message types required; `ErrDirty` keeps its
  role as the authoritative late refusal.
- `src/tui/state.rs`, `src/tui/event.rs`, `src/tui/input.rs`,
  `src/tui/view/worktrees.rs`, `src/tui/view/mod.rs` — `D` key, deleting
  indicator, reason in the first prompt, floating-pane dispatch.
- `src/zellij.rs` — floating-pane helper.
- `src/cli.rs`, `src/main.rs`, `src/launch.rs` — hidden `delete-tab` and
  `confirm-delete` subcommands.
- `README.md` — document `D`.

**Protocol**

`WorktreeRow` gains two `#[serde(default)]` fields, so a snapshot from an older
peer still decodes. No `ClientMsg` or `ServerMsg` variant changes.

**Dependencies**

None added. The floating pane uses `zellij action new-pane --floating
--close-on-exit`, already available in the Zellij versions swamp targets.
