## 1. Tag snapshot rows with default-branch flag

- [x] 1.1 Add `#[serde(default)] pub is_default: bool` to `WorktreeRow` in `src/daemon/state.rs`.
- [x] 1.2 Resolve the default branch once in `DaemonState::load` via `worktree::default_branch(common_dir)` and store it on `DaemonState` as a non-serialized `default_branch: String` (empty = undetectable).
- [x] 1.3 In `refresh_all_unlocked` (`src/daemon/mod.rs`), clone `default_branch` out under the read lock (alongside `agents`) and thread it through `scan_worktrees` into `build_row`; set `is_default = !default.is_empty() && wt.branch == default`.
- [x] 1.4 Update the socket placeholder row construction (`src/daemon/socket.rs:388`) and any test row builders (`make_row`, `make_row_with_ts`) to initialize `is_default`.

## 2. Pin the default branch second in the TUI

- [x] 2.1 In `AppState::pin_snapshot` (`src/tui/state.rs`), after the existing current-to-front move, locate the row where `is_default` is true and move it to index 1 (clamped to `rows.len()`); leave it at index 0 if it is already there.
- [x] 2.2 Confirm `selected_index`, scroll clamping, and click-region mapping still behave correctly given the reordered vector (no separate index bookkeeping needed).

## 3. Style the default-branch row

- [x] 3.1 Add a star glyph helper `default_branch()` to `src/tui/icons.rs` following the existing unicode/ascii icon pattern.
- [x] 3.2 Add a `DEFAULT_BRANCH` accent color to `src/tui/theme.rs`, distinct from `BRANCH` (magenta).
- [x] 3.3 In `build_row` (`src/tui/view/worktrees.rs`), when `r.is_default`: render the star in the agent-icon cell, and render the name and branch cells in `Theme::DEFAULT_BRANCH`.

## 4. Suppress PR/CI/comment status for the default branch

- [x] 4.1 In `build_row` (`src/tui/view/worktrees.rs`), gate on `r.is_default` before the PR lookup: in the expanded layout push blank PR cells (no `pr_loading`); in the compact layout render a blank PR cell.
- [x] 4.2 Verify no PR-loading indicator ever appears on the default row even while `pr_snapshot.loading` is true.

## 5. Tests and verification

- [x] 5.1 Add a daemon unit test asserting `is_default` is set on the matching row and false elsewhere, including the undetectable-default case (no row flagged).
- [x] 5.2 Add a TUI ordering test asserting the default row lands at index 1 after `pin_snapshot` (and stays at index 0 when it is the current worktree).
- [x] 5.3 Run `nix develop path:. --command cargo fmt --all --check` and `nix develop path:. --command cargo clippy --all-targets --all-features -- -D warnings`.
- [x] 5.4 Run `nix build path:.` and manually confirm in the TUI that the default branch is second, starred, distinctly colored, and shows no PR/comment status. (`nix build` passes; visual TUI confirmation pending in a real terminal.)
