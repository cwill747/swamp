## Why

Swamp currently keeps the number of Zellij tabs equal to the number of git
worktrees: launch pre-creates one tab per worktree, and the dashboard TUI
auto-opens a tab whenever a new worktree appears. This "pinning" makes sessions
slow to start, clutters the tab bar for repos with many worktrees, and takes tab
management out of the user's hands. A session should open to the dashboard and
let the user open worktree tabs on demand.

## What Changes

- **BREAKING**: New sessions launch with a single focused dashboard tab and no
  longer pre-create one tab per worktree. The tab count is no longer tied to the
  worktree count.
- **BREAKING**: Remove live tab reconciliation — the dashboard TUI no longer
  auto-opens Zellij tabs for newly-appeared or externally-created worktrees.
- Worktree tabs become user-initiated: the user opens (or switches to) a
  worktree's tab on demand from the dashboard.
- Tabs the user has opened remain part of the live Zellij session state and are
  preserved across detach/reattach; swamp does not close or reconcile them away.
- Persisted per-worktree session/harness state (`.swamp-status.json`) is
  unchanged, so an opened worktree tab still resumes its recorded agent session.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `repo-session-launch`: Layout generation no longer emits one tab per worktree;
  a new session starts with a single focused dashboard tab and worktree tabs are
  opened on demand.
- `daemon-tui-status`: Remove the Tab Reconciliation requirement (auto-opening
  tabs for worktrees) and define on-demand, user-initiated worktree tab opening
  instead.

## Impact

- Code:
  - `src/launch/layout.rs` — `write_multi_tab_layout` stops looping over
    worktrees to emit tabs; always emits a single focused dashboard tab.
  - `src/tui/input.rs` — remove `reconcile_tabs` auto-open path; route worktree
    tab opening through explicit user activation.
  - `src/tui/state.rs` — drop reconciliation bookkeeping (recent-open cooldown /
    known-worktree tracking) no longer needed once auto-open is gone.
  - `src/launch.rs` — `open_worktree_tab` is retained for explicit user-initiated
    opens; `relaunch_worktree_tab` is unaffected.
- Behavior: faster session startup; uncluttered tab bar; user controls which
  worktrees have tabs.
- Docs: README sections describing per-worktree tab behavior need updating.
- No data/migration impact; `.swamp-status.json` format is unchanged.
