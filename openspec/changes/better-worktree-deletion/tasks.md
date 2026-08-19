## 1. Fix the removal safety checks

- [x] 1.1 Derive `Serialize`/`Deserialize` on `RemoveRefusedReason` in `src/worktree/model.rs`, and add a serializable `RemovalVerdict` (`Removable` or `Blocked(RemoveRefusedReason)`) plus the `VerdictContext` struct carrying `GitInfo`, default branch name, default branch tip, and the branch's PR state.
- [x] 1.2 Rewrite `reachable_from_other_branch` in `src/worktree/remove.rs` to check the default branch tip first and return on the first match; keep the full branch walk as the fallback. Add a unit test proving the default-branch case short-circuits and the non-default case still resolves.
- [x] 1.3 Add an `is_merged` helper that returns true when the tip is reachable from the default branch, the branch's most recent PR is `MERGED` and its head matches the tip, or the tip is reachable from any other branch — evaluated in that order.
- [x] 1.4 Extract the pre-mutation checks out of `remove_worktree` into `removal_verdict(common_dir, name, ctx) -> RemovalVerdict`, branching on three upstream states: present (refuse on `ahead > 0`), pruned (`upstream_gone`, apply `is_merged`), never configured (apply `is_merged`).
- [x] 1.5 Reimplement `remove_worktree` on top of `removal_verdict`, refusing on `Blocked` unless `force`. Confirm every existing test in `src/worktree/remove.rs` still passes unchanged.
- [x] 1.6 Add tests: a squash-merged branch with a pruned upstream is removable without force; a branch with a merged PR is removable; a branch with genuinely unmerged commits is still refused; a branch with post-merge local commits is still refused.

## 2. Daemon: verdict on every row

- [x] 2.1 Add `#[serde(default)] pub removal_block: Option<RemoveRefusedReason>` to `WorktreeRow` in `src/daemon/state.rs`, and update every `WorktreeRow` literal in tests across `state.rs`, `socket.rs`, and `tui/state.rs`.
- [x] 2.2 Compute the verdict inside the per-worktree closure in `scan_worktrees`, reusing the `GitInfo` it already built and the default branch resolved in `DaemonState::load`.
- [x] 2.3 Pass PR state into the scan so `is_merged` can consult it, treating `PrSnapshot::loading` and a fetch error as "no signal".
- [x] 2.4 Test: a snapshot row for a dirty worktree reports the dirty reason; a clean merged worktree reports none; a row decoded from JSON lacking the field defaults to none.

## 3. Daemon: shared deleting state

- [x] 3.1 Add `deleting: HashSet<String>` to `DaemonState` with `mark_deleting` / `clear_deleting`, and add `#[serde(default)] pub deleting: bool` to `WorktreeRow`, filled in by `DaemonState::snapshot()`.
- [x] 3.2 Restructure `Daemon::remove_worktree` to mark the worktree deleting and broadcast **before** acquiring `repo_ops`, and to clear the mark and broadcast on success, refusal, and failure.
- [x] 3.3 Wrap the mark in a guard type so an early return or a panic in the `spawn_blocking` removal task cannot strand a row as permanently deleting.
- [x] 3.4 Test: the mark survives an `apply_scanned_rows` that replaces the rows; a refused removal clears the mark and restores the row; the guard clears the mark when the removal task fails.

## 4. Daemon working directory

- [x] 4.1 Call `std::env::set_current_dir(&common)` in `serve`'s foreground path, after the common dir is resolved and before the socket bind.
- [ ] 4.2 Verify manually that a worktree whose pane first started the daemon is now deletable, and that `RemoveRefusedReason::CurrentDirectory` still fires for a `swamp` process actually running inside the target. (deferred to the end-to-end manual pass in 9.3)
  - 2026-08-19: The daemon started from a target worktree, changed its directory to `.bare`, and removed that worktree. The direct `CurrentDirectory` case has no CLI surface.

## 5. TUI: shared deleting indicator

- [x] 5.1 Render rows with `deleting == true` distinctly in `src/tui/view/worktrees.rs`, with a spinner driven by the existing tick.
- [x] 5.2 Include a deleting row in the `AppEvent::Tick` `needs_tick` condition in `src/tui/event.rs` so the spinner animates.
- [x] 5.3 Reject `d` and `D` on a row already marked deleting, with a footer message. (`D` guarded when added in section 8)
- [x] 5.4 Drop the now-redundant local `pending_delete` status text in favour of the shared flag, keeping `pending_delete` only for the "close the tab once the row disappears" reconciliation.

## 6. TUI: reason at the first prompt

- [x] 6.1 Populate `InputMode::ConfirmDelete::force_reason` from the selected row's `removal_block` when `d` is pressed, so the first prompt already names the reason.
- [x] 6.2 Keep the `AppEvent::DeleteNeedsForce` path intact as the stale-snapshot fallback, and confirm it still reopens the prompt when the daemon refuses a delete the verdict approved.
- [x] 6.3 Test: pressing `d` on a row carrying a blocking reason opens a force prompt with that reason and sends no daemon request until the user confirms.

## 7. Zellij floating pane

- [x] 7.1 Add `zellij::new_floating_pane(cwd, name, cmd)` to `src/zellij.rs` wrapping `zellij action new-pane --floating --close-on-exit --cwd … --name … -- …`, returning an error the caller can fall back from. Unit-test the argument construction.
- [x] 7.2 Add the hidden `confirm-delete` subcommand to `src/cli.rs` and `src/main.rs`, taking the worktree name, path, the blocking reason, and a `--close-tab` flag. The reason is passed in, not looked up — the pane never re-computes the verdict.
- [x] 7.3 Implement `confirm-delete`: render the block reason it was given, plus `git status --short` and `git diff --stat HEAD` read from the worktree at render time, then wait for `f` (force) or `n`/`Esc`. No daemon round-trip before the prompt appears.
- [x] 7.4 On `f`, spawn the detached `delete-tab` helper with `--force` when `--close-tab` is set, or send `RemoveWorktree { force: true }` directly when it is not; then exit so `--close-on-exit` disposes of the pane.
- [x] 7.5 Route a *blocked* delete confirmation to the floating pane when inside Zellij, and fall back to the existing footer force-prompt outside Zellij or when the spawn fails. A non-blocked delete must not open a pane.

## 8. Delete the current tab's worktree

- [x] 8.1 Add the hidden `delete-tab` subcommand to `src/cli.rs` and `src/main.rs`, taking the worktree name, path, and `--force`.
- [x] 8.2 Implement `delete_tab` in `src/launch.rs`: initialize logging like `relaunch_worktree_tab`, close the tab by name when inside Zellij, then send `RemoveWorktree` to the daemon and log any failure.
- [x] 8.3 Add a `spawn_delete_tab` helper mirroring `spawn_relaunch_tab` — detached, `process_group(0)`, cwd set to the git common directory. (Implemented in `src/launch.rs`, colocated with the `delete_tab` worker it spawns, and called from both `tui/input.rs`'s `D` handler and `confirm-delete`'s force path — one detached-spawn implementation instead of two copies.)
- [x] 8.4 Bind `D` in `src/tui/event.rs`: resolve the pane's own worktree from `AppState::current_tab`, do nothing when it resolves to nothing, and no-op with a footer message when the resolved row has `is_default` set. Guard on the row's default-branch flag, not on "is this the dashboard".
- [x] 8.5 Test that `D` on a default-branch worktree deletes nothing and closes no tab, and that `d` on that same row still runs the normal confirmation flow.
- [x] 8.6 Route `D` through the same verdict and confirmation surfaces as `d` — footer confirm when removable, floating pane when blocked — with the tab close attached in both cases.

## 9. Docs and verification

- [x] 9.1 Document `D` in the README key table and note that a blocked delete opens a floating confirmation pane.
- [x] 9.2 Run `nix build`, `nix develop --command cargo fmt --all --check`, and `nix develop --command cargo clippy --all-targets --all-features -- -D warnings`.
- [ ] 9.3 Manual end-to-end pass in a real session: delete a clean worktree from the dashboard and watch a second TUI show the deleting row; delete a dirty worktree and force through the floating pane; press `D` in a worktree tab and confirm the tab closes and the worktree is gone; delete a squash-merged branch whose remote ref was pruned and confirm no force prompt appears. (requires a live Zellij session — deferred to the user)
  - 2026-08-19: Two subscribers saw the delete mark and final removal. Clean and dirty `D` paths passed in an isolated Zellij session. The dirty path showed live status in a floating pane. The real GitHub squash-merge case remains open.
