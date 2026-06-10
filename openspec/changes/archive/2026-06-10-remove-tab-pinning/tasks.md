## 1. Collapse launch layout to a single dashboard tab

- [x] 1.1 In `src/launch/layout.rs`, change `write_multi_tab_layout` to emit exactly one focused dashboard tab and remove the per-worktree tab loop
- [x] 1.2 Always generate the dashboard tab for both bare and normal repositories (drop the focus-first-worktree branch for non-bare repos)
- [x] 1.3 Keep `push_worktree_panes` / `push_dashboard_panes` and session-id resume wiring intact (still used by on-demand opens)
- [x] 1.4 Update layout tests to assert a single dashboard tab and no per-worktree tabs at launch (including a many-worktrees case)

## 2. Remove live tab reconciliation

- [x] 2.1 Delete `reconcile_tabs` and its call sites in `src/tui/input.rs`
- [x] 2.2 Remove reconciliation bookkeeping in `src/tui/state.rs` (recent-open cooldown map, known-worktree tracking) and any now-unused fields/imports
- [x] 2.3 Confirm daemon snapshots still update the dashboard worktree list without opening tabs

## 3. Open worktree tabs on explicit activation

- [x] 3.1 Wire the dashboard worktree-pane activation gesture to open the selected worktree's tab via `open_worktree_tab`
- [x] 3.2 Before opening, query `zellij::list_tab_names`; if a tab for the worktree exists, `go_to_tab_name` it instead of opening a duplicate
- [x] 3.3 If tab names cannot be queried, treat tab state as unknown and do not open a tab
- [x] 3.4 Do nothing when not running inside Zellij
- [x] 3.5 Confirm the worktree-creation flow still opens/focuses the newly created worktree's tab

## 4. Verification, docs, and cleanup

- [x] 4.1 Update README sections that describe per-worktree tab behavior to reflect dashboard-first launch and on-demand tab opening
- [x] 4.2 Build with `nix build path:.`
- [x] 4.3 Run `nix develop --command cargo fmt --all --check` and `nix develop --command cargo clippy --all-targets --all-features -- -D warnings`
- [ ] 4.4 Manually verify: fresh launch shows only the dashboard tab; activating a worktree opens its tab; re-activating switches to it; detach/reattach preserves opened tabs
