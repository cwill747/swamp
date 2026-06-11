## Why

On first launch the worktrees pane feels slow and the PR pane lies. The daemon's
initial scan runs a per-worktree `git status` / `git rev-list` sequentially
across every worktree before broadcasting, so the local wait grows with worktree
count. Separately, the PR pane shows "No PRs for any worktree branch" before any
fetch has run, so a still-loading (network) state is indistinguishable from a
genuinely empty one and looks like a wrong answer.

The dividing line we want: wait for **local** data (the per-worktree git status),
but never block the UI on the **network** (PR/CI status), which should stream in
as it arrives.

## What Changes

- The daemon's initial worktree scan gathers per-worktree git status
  concurrently instead of sequentially, cutting first-launch latency on repos
  with many worktrees. The worktree snapshot is still computed entirely from
  local git state.
- The worktree snapshot is broadcast as soon as the local scan completes and is
  never gated on network PR/CI data — the worktrees pane renders local rows
  before any PR fetch returns.
- PR snapshots gain an explicit loading/pending state distinct from "fetched and
  empty", so the PR pane and worktree PR columns can show a loading state on
  first launch instead of "No PRs".
- The PR pane renders a loading message while the first fetch is outstanding, and
  only shows "No PRs for any worktree branch" once a fetch has actually completed
  with no matching PRs.
- The daemon kicks off the first PR fetch as soon as a subscriber connects rather
  than only on the periodic poll cadence, so PR data arrives promptly.

## Capabilities

### New Capabilities
<!-- none: this extends existing daemon/TUI and PR status behavior -->

### Modified Capabilities
- `daemon-tui-status`: the initial worktree scan runs per-worktree git status
  concurrently, and the worktree snapshot is delivered from local git state as
  soon as the local scan completes, explicitly independent of (and never waiting
  for) network PR status.
- `github-pr-ci-status`: PR snapshots distinguish a loading/pending state from a
  completed-but-empty result, the PR pane shows loading vs. empty accordingly,
  and the first PR fetch is triggered on subscribe rather than waiting for the
  poll interval.

## Impact

- Code: `src/daemon/state.rs` (`scan_worktrees` concurrency, `PrSnapshot` shape),
  `src/daemon/mod.rs` (initial refresh, PR poller, on-subscribe fetch trigger),
  `src/daemon/socket.rs` (subscribe path), `src/tui/event.rs` (PR event
  handling), `src/tui/view/pr.rs` and `src/tui/view/worktrees.rs` (loading vs.
  empty rendering).
- Protocol: `PrSnapshot` JSON gains a pending/loading indication; daemon and TUI
  build from the same crate, and the new field uses a serde default so older
  snapshots decode cleanly.
- No new dependencies. The worktree snapshot remains local-only; the change to
  the scan is concurrency, not new data. Steady-state rendering after data loads
  is unchanged.
