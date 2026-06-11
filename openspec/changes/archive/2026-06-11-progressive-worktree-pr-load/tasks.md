## 1. Concurrent worktree scan

- [x] 1.1 In `src/daemon/state.rs`, refactor `scan_worktrees` so the per-worktree `git_info` calls run concurrently instead of in a sequential loop, returning the same `HashMap<String, WorktreeRow>` contract.
- [x] 1.2 Bound the scan concurrency (semaphore or chunked joins) so a repo with many worktrees does not spawn an unbounded number of `git` subprocesses at once.
- [x] 1.3 Update the caller in `src/daemon/mod.rs` (`refresh_all_unlocked`) to drive the concurrent scan correctly off the async runtime without holding async locks during the git work.
- [x] 1.4 Confirm the worktree `Snapshot` is still computed purely from local git state and broadcast as soon as the local scan completes (no network dependency).

## 2. PrSnapshot loading state

- [x] 2.1 Add an explicit `loading: bool` (with `#[serde(default)]`) to `PrSnapshot` in `src/daemon/state.rs`, and the matching mirror state in `DaemonState`.
- [x] 2.2 Initialize `loading = true` until the first PR fetch resolves; clear it on the first fetch result whether it succeeds (`update_prs`) or errors (`record_pr_error`).
- [x] 2.3 Ensure `pr_snapshot()` carries the loading flag, and that the snapshot sent on subscribe before any fetch reports `loading = true`.

## 3. Eager PR fetch on subscribe

- [x] 3.1 Add a wake signal (e.g. `tokio::sync::Notify` or watch channel) the PR poller selects on in `src/daemon/mod.rs`, so it can run a fetch immediately instead of sleeping out its interval.
- [x] 3.2 On the subscribe path in `src/daemon/socket.rs` (where `pr_subscribers` is incremented), trigger that wake signal when no successful fetch has happened yet.
- [x] 3.3 Coalesce wakeups so concurrent subscriptions cause at most one in-flight fetch; keep the existing 60s cadence and error backoff for steady state.

## 4. TUI loading vs. empty rendering

- [x] 4.1 In `src/tui/event.rs`, propagate the new `PrSnapshot.loading` flag into `AppState` (PR snapshot already flows through `AppEvent::PrStatus`).
- [x] 4.2 In `src/tui/view/pr.rs`, when `prs` is empty, show "Loading PRs…" while `loading` is true, "No PRs for any worktree branch" once fetched-and-empty, and preserve the existing "github unreachable" error path.
- [x] 4.3 Add `icons::pr_loading()` returning a static, muted, non-spinner glyph (`…`, ascii `...`) — explicitly NOT the `SPINNER_FRAMES` braille spinner used for CI-pending in `src/tui/view/pr.rs:235-237`.
- [x] 4.4 In `src/tui/view/worktrees.rs`, when the PR snapshot is loading and a branch has no `PrSummary` yet, render `icons::pr_loading()` (styled `Theme::muted()`) in the PR/checks area instead of a blank cell; never fabricate counts. Once a fetch completes, restore the existing blank-for-no-PR behavior.

## 5. Verification

- [x] 5.1 Update/add unit tests in `src/daemon/state.rs` for the loading flag lifecycle (default loading, cleared on success, cleared on error, preserved map on error).
- [ ] 5.2 Manually verify on a multi-worktree repo: worktree rows appear quickly from local data; PR pane shows "Loading PRs…" then resolves to PRs or "No PRs"; no "No PRs" flash before the first fetch; worktree PR columns show the muted loading glyph (not the cyan CI spinner) until PR data lands.
- [x] 5.3 Run `nix develop path:. --command cargo fmt --all --check` and `nix develop path:. --command cargo clippy --all-targets --all-features -- -D warnings`.
- [x] 5.4 Run `nix build path:.` to confirm the fast build passes.
