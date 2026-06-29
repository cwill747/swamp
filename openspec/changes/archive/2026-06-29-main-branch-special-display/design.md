## Context

The worktree dashboard is fed by a daemon snapshot (`Snapshot { rows: Vec<WorktreeRow> }`, `src/daemon/state.rs`). The daemon sorts rows by `head_ts` desc then name (`state.rs:251`). The TUI receives the snapshot and, in `AppState::pin_snapshot` (`src/tui/state.rs:241`), already moves the current/active tab's worktree to index 0 when in the worktrees view. Rendering happens in `build_row` (`src/tui/view/worktrees.rs:129`), which associates PR data by branch name (`app.pr_snapshot.prs.get(&r.branch)`), renders the branch in `Theme::BRANCH` (magenta), and draws PR/check/review/comment cells.

The repo already detects the default branch via `worktree::default_branch(dir)` (`src/worktree/branches.rs:112`), which reads the default remote's `HEAD` symbolic ref and returns `""` when undetectable. `BranchInfo` already carries an `is_default` flag used by the picker — we mirror that naming on `WorktreeRow`.

## Goals / Non-Goals

**Goals:**
- Pin the default-branch worktree to the second slot of the worktrees pane (first if it is the current worktree).
- Mark the default-branch row with a star and a dedicated accent color.
- Suppress all PR/CI/review/comment status for the default-branch row in both layouts.

**Non-Goals:**
- Changing the daemon's snapshot sort order (it stays `head_ts`/name; reordering for current+default is a TUI presentation concern).
- Skipping the network PR fetch for the default branch (we still fetch; we just don't render it — keeps the fetch path simple and the data available if ever needed).
- Any new configuration knobs (star glyph and color are fixed).

## Decisions

**1. Tag rows at the data layer with `is_default: bool`.**
Add `is_default` to `WorktreeRow` (`#[serde(default)]` for backward-compatible persistence). Set `is_default = !default.is_empty() && wt.branch == default` in `build_row` (`state.rs:361`). This keeps the flag out of the render hot path. The daemon's `snapshot()` sort is unchanged.

**1a. Resolve the default branch once per session, not per scan.**
The default branch is read from the default remote's `HEAD` and effectively never changes within a daemon's lifetime, so resolving it on every `scan_worktrees` is wasted work. Resolve it once in `DaemonState::load` (`state.rs:118`, which already takes `common_dir`) via `worktree::default_branch`, store it on `DaemonState` as a non-serialized `default_branch: String` (empty = undetectable). `refresh_all_unlocked` (`mod.rs:453`) already clones `agents` out under a read lock before the `spawn_blocking` scan; clone `default_branch` the same way and pass it into `scan_worktrees`/`build_row`. A default-branch change mid-session (rare: `git remote set-head`, `checkout.defaultRemote` edit) is picked up on the next daemon restart.

**2. Order in the TUI, not the daemon.** "Current first" is already TUI-only (`pin_snapshot`), and "default second" is relative to it, so both belong in `pin_snapshot`. After the existing current-to-front move, find the row with `is_default` and move it to index 1 (clamped to len). If the default row is already at index 0 (it is the current worktree), leave it. Because `selected_index`, scroll, and click regions all read `snapshot.rows` by position, reordering the vector once in `pin_snapshot` keeps every consumer consistent — no other call site changes.

**3. Render in `build_row` keyed off `r.is_default`.**
- Marker: emit a star glyph (new `icons::default_branch()` returning `★`/`*` per the existing unicode/ascii icon pattern in `src/tui/icons.rs`) in the agent-icon column position for the default row, or prefixed to the name — placed so it does not collide with the agent spinner. Decision: render it in the agent-status cell slot, since an agent status on `main` is not meaningful.
- Color: add `Theme::DEFAULT_BRANCH` (a gold/yellow, e.g. `Color::Yellow` or an indexed gold) in `src/tui/theme.rs`; use it for the name and branch cells of the default row instead of `Theme::BRANCH`.
- PR suppression: in the expanded branch, when `r.is_default`, push blank cells (mirroring the existing "no summary" arm) and never the `pr_loading` indicator; in the compact branch, pass `None`-equivalent so `compact_pr_cell` renders blank. Gate this purely on `r.is_default`, before the `prs.get`/`loading` checks.

**4. Default-branch detection uses the existing helper.** No new git logic; `default_branch` already handles `checkout.defaultRemote`, `origin` fallback, and the undetectable case. When it returns `""`, no row is flagged and behavior is exactly as today.

## Risks / Trade-offs

- **`pin_snapshot` only runs in the worktrees view** (`state.rs:243` early-returns otherwise). The second-slot ordering therefore applies in the worktrees pane; the star/color/PR-suppression render off `r.is_default` and apply wherever the table is drawn. This matches the requirement (ordering is a worktrees-pane concern) and is acceptable.
- **Once-per-session default resolution** means a mid-session default-branch change is not reflected until the daemon restarts. This is an extremely rare operation and a restart is a reasonable trigger; the alternative (per-scan resolution) is wasted work on every scan.
- **Star in the agent-icon slot** means the default row shows no agent spinner. Acceptable: agent activity on the trunk worktree is not a tracked workflow, and suppressing it reinforces "this is just main."
- **Serialized field addition** is backward compatible (`#[serde(default)]`); older `.swamp-status.json` files load with `is_default = false` and are corrected on the next scan.
