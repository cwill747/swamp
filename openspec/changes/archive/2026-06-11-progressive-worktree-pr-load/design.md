## Context

The per-repository daemon owns all state. On `swamp tui` startup the TUI spawns
the daemon if needed, subscribes over the Unix socket, and immediately receives
three messages: a worktree `Snapshot`, a `Resources` snapshot, and a
`PrSnapshot` (`src/daemon/socket.rs:96-106`). Future state changes arrive as
broadcasts.

Two first-launch problems:

1. **Worktree scan latency.** `bind_and_kickoff` spawns `refresh_all()` without
   awaiting, so the socket is reachable instantly, but the first `Snapshot` a
   subscriber sees is empty until the scan finishes. `scan_worktrees`
   (`src/daemon/state.rs:272-294`) loops over worktrees and calls
   `worktree::git_info()` **sequentially**, each spawning `git status` /
   `git rev-list`. The whole loop runs inside one `spawn_blocking`
   (`src/daemon/mod.rs:430-434`), so total latency scales with worktree count.
   This is all local git — no network.

2. **PR pane shows "No PRs" while loading.** `PrSnapshot` (`src/daemon/state.rs`)
   has `prs`, `fetched_at: Option<u64>`, and `error: Option<String>` — no way to
   say "a fetch is in flight / has never run". The PR view
   (`src/tui/view/pr.rs:47-54`) renders "No PRs for any worktree branch" whenever
   `prs` is empty and there's no error, which is exactly the never-fetched state.
   The poller also sleeps 2s, then only fetches if `pr_subscribers > 0`
   (`src/daemon/mod.rs:243-249`), so the first fetch is delayed.

The user's framing sets the boundary: **wait for local data (git status), never
block the UI on the network (PR/CI).**

## Goals / Non-Goals

**Goals:**
- Cut first-launch worktree-pane latency by scanning worktrees concurrently,
  while still delivering complete local git status (not partial rows).
- Guarantee the worktree snapshot is never gated on network PR data.
- Distinguish "PR status loading" from "PR status fetched and empty" end-to-end
  (daemon snapshot → PR pane → worktree PR columns).
- Trigger the first PR fetch when a subscriber connects, not on the poll cadence.

**Non-Goals:**
- Progressive/partial worktree rows (skeleton names first, git status later). Git
  status is local and we are willing to wait for it; the fix is to make that wait
  shorter, not to stream half-populated rows.
- Changing how PR/CI data is fetched (GraphQL/REST), aggregated, or its 60s
  steady-state poll cadence.
- Any new external dependency.

## Decisions

### 1. Scan worktrees concurrently instead of sequentially
`scan_worktrees` currently runs `git_info` per worktree in a sequential loop on a
single blocking thread. Run the per-worktree `git_info` calls concurrently so the
local wait is bounded by the slowest worktree rather than their sum.

- **Approach:** fan the per-worktree `git_info` calls out across blocking
  threads — e.g. spawn one `spawn_blocking` per worktree from the async caller
  and `join` them, or use a bounded blocking pool. Keep `scan_worktrees`'
  contract (returns the full `HashMap<String, WorktreeRow>`); only the internal
  gathering becomes concurrent. The write-lock swap via `apply_scanned_rows`
  stays unchanged.
- **Why not partial/streaming rows:** the user explicitly wants to wait for git
  status; partial rows add a `WorktreeRow` "pending" field, extra broadcasts, and
  TUI loading states for no benefit once the scan is fast.
- **Bounding:** cap concurrency (e.g. a semaphore or chunked joins) so a repo
  with dozens of worktrees doesn't spawn an unbounded number of `git`
  subprocesses at once.

### 2. Add a loading/pending state to `PrSnapshot`
Give `PrSnapshot` an explicit notion of "no fetch has completed yet". The
simplest encoding reuses existing fields: a snapshot is *loading* when
`fetched_at.is_none() && error.is_none()`. Make that explicit and intentional
rather than incidental:

- Prefer an explicit `loading: bool` (serde `#[serde(default)]`) set true until
  the first fetch resolves (success or error), so the meaning is self-documenting
  and not coupled to the `fetched_at`/`error` combination. The default keeps old
  snapshots decoding cleanly.
- The daemon's first emitted `PrSnapshot` (sent on subscribe before any fetch)
  carries `loading = true`; the first completed fetch clears it.

### 3. PR pane and worktree columns render loading vs. empty
- **PR pane** (`src/tui/view/pr.rs`): when `prs` is empty, branch on the loading
  flag — show "Loading PRs…" while loading, "No PRs for any worktree branch" once
  fetched-and-empty, and keep the existing "github unreachable" path for the
  errored-and-never-succeeded case.
- **Worktrees pane** (`src/tui/view/worktrees.rs`): today the PR detail cells are
  only emitted when a branch has a `PrSummary`; while loading there is no summary,
  so the row would render blank and read as "no PR". Instead, when the snapshot is
  loading and a branch has no summary yet, render a dedicated **loading glyph** in
  the PR/checks area. This glyph MUST be visually distinct from the CI "build
  running" indicator: build-running is the **cyan animated braille spinner**
  (`SPINNER_FRAMES` via `check_state_icon_color`, `src/tui/view/pr.rs:235-237`),
  so the loading glyph is a **static, muted, non-spinner** mark — a new
  `icons::pr_loading()` returning `…` (ascii `...`) styled with `Theme::muted()`.
  Never fabricate failed-build/comment/review counts while loading. After a fetch
  completes, behavior is unchanged (blank for branches with no PR).

### 4. Trigger the first PR fetch on subscribe
Today the poller sleeps 2s then loops, skipping while there are no subscribers.
On first launch that adds latency. Signal the poller to fetch immediately when a
subscriber connects and no successful fetch has happened yet.

- **Approach:** on the subscribe path (where `pr_subscribers` is incremented),
  notify the poller (e.g. a `tokio::sync::Notify` or watch channel the poller
  selects on) to run a fetch now instead of waiting out its `sleep`. The existing
  60s cadence and error backoff remain for steady state.
- **Why not fetch inline on the socket handler:** that would re-block the
  subscribe response on the network — exactly what we're avoiding. The fetch must
  stay on the poller task and broadcast its result asynchronously.

## Risks / Trade-offs

- **Unbounded `git` subprocess fan-out** → cap scan concurrency (semaphore or
  chunked joins) so large repos don't spawn one `git` per worktree simultaneously
  and starve the machine.
- **Protocol field addition** → `PrSnapshot` gains a field; daemon and TUI build
  from one crate so they move together, and `#[serde(default)]` keeps a stale
  in-flight snapshot or older peer decodable. Low risk.
- **Loading state never clearing on persistent failure** → the first fetch
  *error* must also clear `loading` (loading means "no fetch has resolved yet"),
  so a repo with no `gh`/network settles into the existing "github unreachable"
  path rather than spinning "Loading…" forever.
- **On-subscribe fetch storms** → multiple panes subscribing at once could each
  poke the poller; coalesce so only one fetch runs (the poller already serializes
  fetches; the notify just wakes it once).

## Migration Plan

Single in-process change; no data migration. The daemon is per-repo and
short-lived — `swamp kill` / next `swamp tui` starts the new binary. Rollback is
reverting the change; the added `PrSnapshot` field is ignored by older readers
via serde defaults.

## Open Questions

- Exact concurrency bound for the scan (fixed cap vs. CPU-derived) — pick a small
  constant unless profiling on a large repo suggests otherwise.
- None outstanding. (Resolved: the worktrees pane shows a dedicated static muted
  loading glyph in the PR/checks area while loading, distinct from the cyan
  animated CI build-running spinner — see Decision 3.)
