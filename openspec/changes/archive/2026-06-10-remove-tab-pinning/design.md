## Context

Swamp launches a Zellij session per repository. Today the tab set mirrors the
worktree set in two places:

1. **Launch fan-out** — `write_multi_tab_layout` (`src/launch/layout.rs`) emits a
   focused dashboard tab (bare repos only) plus one tab per discovered worktree.
2. **Live reconciliation** — `reconcile_tabs` (`src/tui/input.rs`) runs on every
   daemon snapshot and opens a tab for any worktree that doesn't already have
   one, debounced by a recent-open cooldown tracked in `src/tui/state.rs`.

The combination is the "tab pinning" behavior: tab count == worktree count,
maintained automatically. This change removes both mechanisms so a session opens
to the dashboard and worktree tabs are opened only when the user asks for one.

Per-worktree session/harness state in `<git-common-dir>/.swamp-status.json`
(`load_session_ids`, `load_harness_overrides`) and the worktree-tab pane layout
(`push_worktree_panes`) are orthogonal and stay as-is.

## Goals / Non-Goals

**Goals:**
- A newly launched session opens with exactly one focused dashboard tab.
- Tab count is decoupled from worktree count.
- Worktree tabs open only on explicit user activation, and activating a worktree
  that already has a tab switches to it instead of duplicating.
- Tabs the user opened during a session persist as live Zellij session state
  across detach/reattach with no extra work from swamp.

**Non-Goals:**
- No change to `.swamp-status.json` format or session-resume behavior.
- No change to the worktree-tab pane layout (lazygit / status / agent / shell).
- No new persistence of "which tabs were open" — Zellij already holds open tabs
  in the live session; we do not reconstruct them on a fresh launch.
- No change to `relaunch_worktree_tab` (harness-swap reopen of an existing tab).

## Decisions

### Always launch with a single dashboard tab (bare and normal repos)
`write_multi_tab_layout` emits one focused dashboard tab and stops looping over
worktrees. The previous bare/normal split (dashboard-first vs. focus-first-
worktree) collapses: both cases now start at the dashboard.

- *Why*: "just open the dashboard" is the stated intent; a single code path is
  simpler than preserving a normal-repo special case that focused a worktree
  tab (itself a pinning behavior).
- *Alternative considered*: keep focusing the single worktree for non-bare
  repos. Rejected — it reintroduces worktree-driven tab logic and contradicts
  the goal.

### Remove `reconcile_tabs` and its bookkeeping
Delete the snapshot-driven auto-open path in `src/tui/input.rs` and the
supporting state in `src/tui/state.rs` (recent-open cooldown, known-worktree
tracking). Snapshots still drive the dashboard's worktree list; they just no
longer open tabs.

- *Why*: this is the live half of pinning. The cooldown existed only to suppress
  duplicate reconcile opens; with reconcile gone it has no purpose.

### Worktree tabs open via explicit activation, with switch-or-open dedup
Activating a worktree row in the dashboard worktrees pane (the existing
selection/activation gesture) calls `open_worktree_tab`. Before opening, query
current tab names via `zellij::list_tab_names`; if a tab for that worktree
already exists, `go_to_tab_name` it instead of opening a duplicate. If tab names
can't be queried, treat tab state as unknown and do not open (matching the prior
reconciliation safety behavior).

- *Why*: a query-then-switch-or-open gives idempotent activation without the
  timer-based cooldown.

### Worktree creation still opens the new worktree's tab
Creating a worktree is itself an explicit user action, so the create flow keeps
opening (and focusing) the new worktree's tab. Only *automatic* opens for
worktrees the user did not act on are removed.

- *Alternative considered*: return to the dashboard after create and require a
  second activation. Rejected — creating a worktree strongly implies intent to
  work in it; opening its tab is the expected outcome.

## Risks / Trade-offs

- [Non-bare repos now show a dashboard tab they didn't before] → The dashboard is
  the intended hub and lists the worktree; document the change in the README.
- [Users relying on every worktree having a tab at launch lose that] → On-demand
  open is one activation away; call this out in release notes / README.
- [Externally created worktrees (git CLI elsewhere) no longer auto-tab] →
  Intended; they still appear in the dashboard worktree list and can be opened.
- [Repeated activation could race and open duplicates] → Mitigated by querying
  tab names and switching to an existing tab before opening.

## Migration Plan

Code-only change; no data migration. Steps:
1. Collapse `write_multi_tab_layout` to a single dashboard tab.
2. Remove `reconcile_tabs` and its `state.rs` bookkeeping; wire worktree
   activation to switch-or-open.
3. Update README sections describing per-worktree tab behavior.
4. Build with `nix build path:.`; run fmt + clippy.

Rollback: revert the change; `.swamp-status.json` is untouched, so existing
sessions are unaffected.

## Open Questions

- Should activation also be reachable via a dedicated key/click distinct from
  selection movement, or is the current activation gesture sufficient? (Lean:
  reuse the existing gesture; revisit if it feels overloaded.)
