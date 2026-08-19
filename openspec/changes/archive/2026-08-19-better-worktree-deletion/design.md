## Context

Removal today is one call: the TUI sends `ClientMsg::RemoveWorktree`, the daemon
takes `repo_ops`, `worktree::remove_worktree` runs every safety check and the
mutation together, and the reply is `Ok`, `Err`, or `ErrDirty`. Everything the
user learns about *why* a delete was refused arrives in that reply.

Three consequences follow from that shape.

**Nothing is visible before the reply.** `AppState::pending_delete` and the
"Deleting X…" status line are per-process. Other panes learn about the delete
only when the row disappears from a snapshot.

**The reply can be slow.** `repo_ops` is held by `refresh_all`, by
`create_worktree`, and by `fetch_and_refresh` — which allows a `git fetch --all`
up to 60 seconds. A delete request queued behind one of those sits silently, and
then the "this branch has commits that aren't elsewhere" prompt appears long
after the user pressed `y`.

**The reason is frequently wrong.** In `remove_worktree`:

```rust
if let Some(branch_name) = branch.as_deref()
    && let Ok(b) = repo.find_branch(branch_name, BranchType::Local)
    && b.upstream().is_err()
    && let Ok(tip) = b.get().peel_to_commit()
    && !reachable_from_other_branch(&repo, branch_name, tip.id())
```

`b.upstream()` returns `Err` in two very different situations: the branch never
had an upstream, and the branch had one whose remote-tracking ref was pruned.
`git_info` already distinguishes them via `upstream_gone`; this path does not.
So the normal agent workflow — push a branch, open a PR, squash-merge it, let
GitHub delete the remote branch, `git fetch --prune` — lands in the second case.
`reachable_from_other_branch` then correctly reports that the squashed commits
are on no other branch, and swamp refuses. The branch was deletable while its
remote ref existed and became undeletable the moment the merge cleaned it up,
which is exactly backwards.

`reachable_from_other_branch` is also unbounded: it peels every local and remote
branch and runs `graph_descendant_of` against each one. On a repo with a few
hundred remote branches that is a few hundred revwalks, inside the `repo_ops`
lock, per delete.

Separately, the daemon is spawned by `tokio::process::Command` from
`ensure_daemon` with no `current_dir`, so it inherits the cwd of whichever pane
first started it. When that pane is a worktree sidebar, `remove_worktree`'s
`path_contains(&wt_path, &cwd)` check makes that worktree permanently
undeletable with `RemoveRefusedReason::CurrentDirectory`.

Finally, deleting the worktree a pane lives in is not expressible today. The
sidebar TUI, lazygit, the agent, and the shell all hold a cwd inside the target,
and the TUI that would close the tab is inside that tab.

## Goals / Non-Goals

**Goals:**

- One shared, daemon-owned answer to "is this worktree being deleted?", visible
  in every TUI in the session.
- The refusal reason is on screen at the first confirmation, with no daemon
  round-trip and no `repo_ops` wait.
- A merged branch is deletable whether or not its remote ref still exists.
- `D` deletes the worktree the pane lives in and closes its tab.
- Blocked deletions show the work at risk in a floating pane, not a truncated
  one-line footer.

**Non-Goals:**

- No change to the wire protocol's message set. `WorktreeRow` gains two
  `#[serde(default)]` fields; no `ClientMsg`/`ServerMsg` variant is added,
  removed, or renamed.
- No undo, trash directory, or recovery of a deleted worktree.
- No deletion of remote branches. `delete_branch` still means the local branch.
- No change to worktree creation, the create picker, or the harness picker.
- The floating pane is a confirmation surface, not a general diff viewer.

## Decisions

### 1. Split the removal check from the removal

Extract the pre-mutation checks in `remove_worktree` into

```rust
pub fn removal_verdict(
    common_dir: &Path,
    name: &str,
    ctx: &VerdictContext,
) -> RemovalVerdict;
```

returning `RemovalVerdict::Removable` or
`RemovalVerdict::Blocked(RemoveRefusedReason)`. `remove_worktree` calls it and
refuses on `Blocked` unless `force`; `scan_worktrees` calls it to fill
`WorktreeRow::removal_block`.

**Why:** the check and the enforcement cannot drift, and the TUI gets the reason
from data it already receives. The alternative — a `ClientMsg::CheckRemoval`
request fired when the user presses `d` — was rejected: it still needs
`repo_ops`, so it reintroduces exactly the wait this change is meant to remove.

`VerdictContext` carries what the verdict needs but should not fetch itself: the
already-computed `GitInfo`, the default branch name and tip, and the branch's PR
state. Passing them in keeps the verdict a pure function of data the scan
already has.

### 2. Verdict rides the existing scan, not a new pass

`scan_worktrees` already fans per-worktree `git_info` out across scoped threads
and already runs under `spawn_blocking`. The verdict is computed in that same
per-worktree closure, reusing the `GitInfo` it just built.

**Why:** no new scan, no new lock, no new broadcast path. The added cost is one
lock-status read plus the bounded reachability probe per worktree.

**Trade-off:** the verdict is as fresh as the last scan, so a snapshot can be
stale — the user dirties a worktree between the scan and pressing `d`. Accepted,
because the daemon check stays authoritative: it re-runs the verdict inside
`repo_ops` and can still reply `ErrDirty`, which drives the existing
`DeleteNeedsForce` → force-prompt path. The snapshot verdict is a fast, usually
correct preview; the daemon is the guard.

### 3. Merged means "the work is somewhere else", by three signals

`is_merged` returns true when any of these hold, checked in this order:

1. The tip is reachable from the default branch tip.
2. The most recent PR for the branch is `MERGED`, and its head commit matches
   the local branch tip. `PrSummary` carries both values.
3. The tip is reachable from some other local or remote branch.

**Why this order:** (1) is one `graph_descendant_of` call and covers merge
commits and fast-forwards. (2) is a map lookup and is the *only* signal that
catches a squash or rebase merge, where the original commits exist nowhere. (3)
is the existing fallback and the only expensive one, so it runs last.

**On trusting PR state for a destructive action:** it is used only to *allow*, never
to block, and a merged PR is strong evidence the diff is on the default branch.
Local commits made after the merge change the local tip. The PR signal then
does not match and cannot allow removal.
`PrSnapshot::loading` and a fetch error both mean "no PR signal", which falls
through to (3) — the current behavior.

**Alternative considered:** `git cherry` / patch-id equivalence to detect squash
merges locally. Rejected — it needs a merge base and a full patch-id walk per
branch, which is far more expensive than the PR lookup for the same answer, and
swamp already polls PR state.

### 4. `upstream_gone` is a distinct state

The verdict branches on three upstream states rather than two:

| Upstream state | Meaning | Rule |
| --- | --- | --- |
| present | `branch.upstream()` is `Ok` | refuse when `ahead > 0` |
| pruned (`upstream_gone`) | config has `branch.<n>.merge`, ref is gone | apply the merged check |
| never configured | no upstream config at all | apply the merged check |

`git_info` already computes `upstream_gone`; the verdict reads it from the
`GitInfo` it is handed instead of re-deriving it. The pruned case previously
fell into "never configured" and produced the false `UnmergedCommits` refusal.

### 5. Bounded reachability probe

`reachable_from_other_branch` becomes:

1. If the default branch tip is known and `tip == default_tip ||
   graph_descendant_of(default_tip, tip)`, return true.
2. Otherwise iterate the remaining branches, returning on the first match.

Same worst case, dramatically better common case: the overwhelmingly common
answer is "yes, it is in main", found in one revwalk instead of an arbitrary
number.

**Alternative considered:** caching a reachability set per scan. Rejected as
premature — step 1 already collapses the common case, and a cache would need
invalidating on every ref change.

### 6. Deleting state lives in `DaemonState`, projected at `snapshot()`

Add `deleting: HashSet<String>` to `DaemonState`. `WorktreeRow` gains
`#[serde(default)] pub deleting: bool`, filled in by `DaemonState::snapshot()`
from that set.

**Why a separate set rather than a field on the stored row:**
`apply_scanned_rows` replaces the rows wholesale on every rescan, and a delete
races with exactly the rescan its own directory removal triggers via the fs
watcher. A field on the row would be silently dropped mid-delete. The set is
independent of the rows and survives.

`remove_worktree` becomes:

```
insert into deleting; broadcast snapshot   // BEFORE repo_ops
acquire repo_ops; run the removal
remove from deleting
on success: remove_row + broadcast
on failure: broadcast (row restored, mark cleared)
```

Marking before the lock is the point: the whole `repo_ops` wait is now visible
as "deleting" in every pane, which is what makes a slow delete legible instead
of mysterious.

Cleanup uses a guard type so an early return or a panic in the removal task
cannot strand a row as permanently deleting.

### 7. `D` runs through a detached `swamp delete-tab`

`D` resolves the pane's own worktree via the existing `AppState::current_tab`
(already maintained from `current_dir` / `ZELLIJ_TAB_NAME` for row pinning) and
spawns:

```
swamp delete-tab <name> <worktree-path> [--force]
```

detached, `process_group(0)`, cwd set to the git common directory — the same
pattern `spawn_relaunch_tab` already uses for the harness swap, which has the
identical "this pane is about to close its own tab" problem.

`delete-tab` closes the tab first, then sends `RemoveWorktree` to the daemon.

**Why close first:** `remove_dir_all` on a directory that is the cwd of live
processes succeeds on Linux but leaves lazygit and the agent writing into an
unlinked tree, and an agent mid-write can recreate paths under a directory being
removed. Closing the tab terminates those processes first. The cost is that a
removal refused *after* the close leaves the tab gone — which is why the verdict
and the confirmation both happen before `delete-tab` is ever spawned.

`delete-tab` initializes logging like `relaunch_worktree_tab` does and writes
failures to the repo log; it has no terminal to print to.

### 8. The floating pane is a `swamp confirm-delete` subcommand

Blocked deletion inside Zellij runs:

```
zellij action new-pane --floating --close-on-exit --cwd <common-dir> \
  --name "delete <worktree>" -- swamp confirm-delete <name> <path> [--close-tab]
```

`confirm-delete` renders the block reason, `git status --short`, and
`git diff --stat HEAD` for the target, and waits for `f` (force) or `n`/`Esc`.
On `f` it spawns the same detached `delete-tab` (with `--force`) that `D` uses,
or sends `RemoveWorktree { force: true }` directly when no tab close is wanted;
then it exits and `--close-on-exit` disposes of the pane.

**Why a subcommand and not an in-TUI modal:** the sidebar is roughly a third of
a narrow column. A file list and a diffstat do not fit, and that is precisely
the information the user needs to decide. A separate process is also what makes
the confirmation survivable when the pane's own tab is the one being closed.

**Why `--cwd <common-dir>`:** a floating pane inheriting the target worktree's
cwd would be one more process pinning the directory.

Clean deletions do not go near this path: they keep the current footer `y/n`,
which is instant and already correct.

**Note:** a floating pane belongs to its tab, so `confirm-delete` must finish
its interaction and hand off to the detached helper *before* any tab close. The
ordering in decision 7 already guarantees this.

### 9. Daemon chdirs to the common directory

`serve --foreground` calls `std::env::set_current_dir(&common)` after resolving
the common dir, before spawning anything.

**Why not just delete the cwd check:** the check is correct for the CLI paths
and for a `swamp` invoked inside the worktree — a process should not delete the
ground it stands on. The bug is the daemon standing somewhere arbitrary. Fixing
the daemon's cwd is narrower than weakening a safety check.

The common directory always exists for the daemon's lifetime and is never a
worktree, so it is the natural anchor.

## Risks / Trade-offs

**Snapshot verdict is stale by up to one scan interval** → The daemon re-runs
the verdict authoritatively inside `repo_ops`; the existing `ErrDirty` →
`DeleteNeedsForce` path handles the disagreement. The user sees the force prompt
they would have seen before, just rarely instead of always.

**Verdict adds per-worktree work to every scan** → The added cost is a lock
check plus a probe that short-circuits on the default branch. It runs inside the
existing parallel scan. If it ever shows up in scan latency, the verdict can be
computed for the selected row only, on demand.

**A merged PR is trusted to allow deletion** → Only as an *allow* signal, only
for the reason that is currently a false positive, and only when a PR fetch has
actually resolved. Post-merge local commits still block through the `ahead` and
reachability checks. `force` remains available in the other direction.

**Closing the tab before removal can strand a user with no tab and no delete** →
Only reachable when the daemon refuses a removal the verdict approved and the
user has already confirmed. The worktree survives; the tab can be reopened from
the dashboard with `Enter`. Logged so it is diagnosable.

**A stranded `deleting` mark would make a row permanently undeletable** → The
mark is cleared by a guard on every exit path, including panics in the
`spawn_blocking` task. Worst case it clears on daemon restart.

**Floating-pane flags vary across Zellij versions** → Only `--floating`,
`--close-on-exit`, `--cwd`, and `--name` are used, all long-standing. Any spawn
failure falls back to the footer force-prompt, so an unsupported flag degrades
to today's behavior rather than blocking the delete.

**Two delete entry points (`d` and `D`) could diverge** → Both resolve to the
same verdict, the same confirmation surfaces, and the same `RemoveWorktree`
request. `D` only adds target resolution and the tab close.

## Migration Plan

Ships in one release, no data migration.

- `WorktreeRow`'s new fields are `#[serde(default)]`, so a snapshot written by
  an older peer decodes with `deleting: false` and no blocking reason — the
  pre-change behavior. `.swamp-status.json` persists `AgentRecord`, not rows, so
  it is untouched.
- The daemon already refuses to run against a mismatched binary version and
  prompts for a session restart, so a mixed-version session is already handled.
- Rollback is a binary revert; nothing on disk changes shape.

## Open Questions

None outstanding. Two questions raised during design are resolved below.

### Resolved: `D` no-ops on the default-branch worktree

`D` does nothing when the pane's resolved worktree is the default branch's, and
says so in the footer. The dashboard's cwd is the default worktree, so an
unguarded `D` there would target trunk — never what the user means, and the most
destructive possible misfire of a single keystroke.

The guard is on the *resolved worktree being the default branch*, not on "am I
the dashboard". A worktree tab genuinely open on the default branch is guarded
too, which is the right call for the same reason: swamp pins that row and treats
it specially everywhere else. Deleting the trunk worktree stays possible through
`d` on the dashboard, which is deliberate enough to not be an accident.

### Resolved: the floating pane does not re-check the verdict

The pane labels the block with the reason from the snapshot verdict it was
spawned with. It does not acquire `repo_ops` for a fresh check.

The pane already shows live `git status --short` and `git diff --stat HEAD`,
read directly from the worktree at render time — so the *evidence* the user
decides on is always current, and only the one-line label could be stale. Paying
a `repo_ops` acquisition to refresh a label, in the exact flow this change exists
to speed up, is the wrong trade. The daemon's authoritative check still runs when
the removal is actually requested, so a stale label cannot cause an unsafe
delete.

Consequence: the reason must be passed to `swamp confirm-delete` as an argument
rather than looked up by the subcommand.
