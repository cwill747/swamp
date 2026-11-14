# Validation notes

## Configuration and generated layouts

- `nix develop --command cargo test dashboard_layout_config_keeps_classic_knobs_and_validates_sidebar_width`: passed. Covers workspace defaults, classic percentages, widths 16 and 60, and out-of-range signed/oversized values with the documented 16–60 message.
- `nix develop --command cargo test launch::layout::tests`: 19 passed. Covers workspace/classic KDL, one initial dashboard tab, fixed sidebar sizing, stable pane names, suspended agent, harness resolution, Claude resume, managed lazygit, fish/POSIX quoting, and Nix entry behavior.
- The workspace dashboard template uses the built-in `zellij:compact-bar` plugin after `children`. Zellij 0.45.0 renders this one-row bar at the bottom edge; the live transcript showed one combined `Zellij (<session>) NORMAL dashboard` strip at y=35/47 in 120x36/160x48 sessions.
- A real capture exposed a doubled vertical edge where the sidebar's divider met Zellij's split gutter. Setting `borderless=true` alone did not remove the gutter. Generated dashboard and worktree panes now pass `--embedded`; final captures confirm the workspace renderer leaves Zellij's gutter as the single divider.
- Existing-session reuse remains in `ensure_background_session`; explicit classic templates retain `tab-bar` and `status-bar`. Slot-tab open restores the prior tab after adding the tab.

## Fresh Zellij 0.45.0 composition

Sessions were generated from the built `result/bin/swamp` in isolated XDG/Zellij runtime directories using temporary repositories. Committed pane dumps and TTY transcripts are under `tests/fixtures/workspace/zellij-0.45-captures/`:

- `dashboard-{120,160}-panes.json`, sidebar/details/shell dumps, and `capture{120,160}-tty.txt`.
- The 120x36 session had a 32x35 sidebar at (0,0), 88x23 details at (32,0), 88x12 real shell at (32,23), and one compact bar row at (0,35).
- The 160x48 session had a 32x47 sidebar at (0,0), 128x31 details at (32,0), 128x16 real shell at (32,31), and one compact bar row at (0,47).
- Both captures show one initial dashboard tab, Spaces above Agents, the workspace hierarchy, one native gutter, and no legacy grid. Rail and standalone size snapshots are in `tests/fixtures/workspace/`.
- ANSI transcripts retain color. State glyphs remain distinct from `@` active and `*` trunk markers, so status remains readable without color.

## Isolated multi-repository acceptance fixture

The temporary fixture at `/tmp/swamp-herdr-e2e-completion` used private repo state, runtime, and Zellij sessions. `alpha` had Waiting/Working agent rows plus `alpha-free`; `beta` had a Done row and a separate idle row. Its registry also included a legacy peer serving a pre-extension Snapshot with no completion metadata. The beta seen ledger records exact completion IDs, and per-session selection files show sidebar/detail panes sharing selected destinations. Both live daemons logged unavailable GitHub CLI authentication and retained prior PR state. The agent report confirmed an attached-client cold jump from beta to alpha, creation/focus of canonical `alpha-waiting` and its generated terminal pane. Other reported checks covered Waiting and Done acknowledgement, safe disposable branch deletion, detach/reattach persistence, and no focus change from passive polling. Native `toggle-fullscreen` was exercised in the isolated alpha session and `list-panes --all --json` showed the agent terminal fullscreen, then restored to non-fullscreen. These temporary files are evidence, not committed golden output.

## Final verification

- `nix develop --command cargo test`: 459 passed, including legacy aggregator lock handoff coverage.
- `nix develop --command cargo fmt --all --check`: passed.
- `nix develop --command cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `nix build .#dev`: passed.
- `openspec validate herdr-style-dashboard --strict`: passed.
- `git diff --check`: passed.
- Fresh launch smoke test with a temporary repository and isolated Zellij stub: verified `swamp launch` started the repository daemon, waited for its Ping/Pong response, and generated the dashboard session.
- Regression tests verify that registered disconnected repositories reconcile to Stopped without stale worktree rows, both when disconnect and registry refresh arrive in either order.
- Workspace client reconnect coverage verifies that a lost broker connection requests aggregator restart, with retries throttled while the endpoint is unavailable. The stale-version relaunch path also starts and probes the replacement daemon after teardown.
