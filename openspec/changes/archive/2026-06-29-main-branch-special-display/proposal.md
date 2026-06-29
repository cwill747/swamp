## Why

The default branch (usually `main`) is the trunk every worktree branches from, not a unit of work in flight. Today it sorts into the worktree list by activity timestamp like any other row and shows PR/CI/comment status that is meaningless for it. Operators want it parked in a predictable slot and visually marked as "this is just main," so they can find it instantly and never mistake it for review-pending work.

## What Changes

- The default-branch worktree is pinned to the **second** row of the worktrees pane (directly below the current/active worktree, which is already pinned first). If the default branch *is* the current worktree, it stays first. All other worktrees keep their existing newest-activity ordering below it.
- The default-branch row gets a distinct visual treatment: a **star marker**, its name/branch rendered in a dedicated accent color (not the magenta used for other branches).
- The default-branch row **never displays PR state, PR number, checks, review status, or comment counts** — those columns stay blank for it in both compact and expanded layouts, and it is not subjected to PR loading indicators.
- The daemon tags each snapshot row with whether it is the default branch (`is_default`), computed from the repo's default-branch detection, so the TUI can order and style without re-deriving it.

## Capabilities

### New Capabilities
<!-- None — this extends existing snapshot/rendering behavior. -->

### Modified Capabilities
- `daemon-tui-status`: Snapshot rows carry a default-branch flag; worktrees-pane ordering pins the default branch to the second slot; the default-branch row is rendered with a special marker/color and suppresses all PR/CI/comment status.

## Impact

- **Code**: `src/daemon/state.rs` (add `is_default` to `WorktreeRow`, set it in `build_row`/`scan_rows` using `worktree::default_branch`), `src/tui/state.rs` (`pin_snapshot` slots the default row second), `src/tui/view/worktrees.rs` (`build_row` star/color + PR-cell suppression), `src/tui/icons.rs` + `src/tui/theme.rs` (star glyph + accent color).
- **APIs**: `WorktreeRow` gains a serialized field (`#[serde(default)]`, backward compatible with existing `.swamp-status.json`).
- **Dependencies**: none.
- **Behavior**: ordering of the worktrees pane changes; default-branch PR columns go blank.
