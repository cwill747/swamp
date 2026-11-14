# Proposal

## Why

Swamp already knows about repositories, worktrees, agents, and attention, but makes users leave their terminals for a dashboard assembled from separate status panels. Make the default experience look and navigate like the Herdr TUI: a persistent Spaces/Agents rail beside real terminals, with the agent that needs input always easy to find.

## What Changes

- Replace the default dashboard's three-column panel grid with a narrow, two-section left sidebar (Spaces above Agents), a slim tab strip, and a large context/detail area beside a real shell. Repeat the sidebar in generated worktree tabs beside an agent-dominant terminal layout.
- List all live repositories and their worktrees, plus registered stopped repositories. Keep waiting, working, newly completed, and idle agents visible across projects; preserve branch titles, slot labels, subagent rows, PR/CI cues, and default-branch identity.
- Add a searchable Go To picker, state filters, next-attention navigation, keyboard selection, and single-click activation. Browsing or receiving status updates never steals terminal focus.
- Display newly completed work as Done until explicitly visited; keep the existing working/waiting/idle hook vocabulary and distinguish missing status from confirmed idle.
- Keep Zellij as the terminal/session engine, existing repo sessions and on-demand tabs, lazygit, suspended agent startup, slot safety, and detach semantics. Cross-repo jumps switch sessions rather than nesting them.
- Make the Herdr-style workspace the default for new sessions, with explicit classic layout fallback and retained single-panel CLI views. Existing live sessions remain untouched until deliberately recreated.
- Include a concrete visual contract, ASCII wireframes, responsive behavior, and rendered-screen acceptance tests; this is a dashboard redesign, not just additional keybindings.

## Capabilities

### New Capabilities

- `workspace-dashboard`: Herdr-style Spaces/Agents presentation, cross-repo selection, searchable Go To navigation, attention workflow, and responsive visual contract.

### Modified Capabilities

- `repo-session-launch`: Default sidebar/terminal layouts, classic fallback, dashboard configuration compatibility, and sidebar placement in worktree tabs.
- `daemon-tui-status`: Workspace and sidebar TUI views plus backward-compatible completion revisions for reliable Done/seen presentation.

## Impact

- Layout generation and launch (`src/launch/layout.rs`, `src/launch.rs`); configuration and CLI (`src/config/`, `src/cli.rs`, `src/main.rs`).
- Shared cross-repo model built on `src/fanin.rs`, `src/discover.rs`, `src/registry.rs`, `src/overview/attention.rs`, and `src/overview/jump.rs`; terminal actions stay behind `src/zellij.rs`.
- TUI rendering, input, hit regions, theme, and client state (`src/tui/`); additive completion metadata in daemon state/socket serialization and branch-session persistence.
- No new terminal emulator, PTY server, WASM plugin, remote-machine layer, or Herdr runtime dependency. Herdr supplies the visual/interaction reference, not copied branding or code.
- Account for existing changes: `swamp-of-swamps` is implemented but unarchived; `worktree-slots` is implemented with one task outstanding. Reuse their behavior, do not rewrite their artifacts, and reconcile their deltas before archive, especially worktree snapshot fields and tab-opening rules.
