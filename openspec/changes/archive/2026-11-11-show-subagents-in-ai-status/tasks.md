## 1. Hook payload and protocol

- [x] 1.1 Add `SubagentEvent` (`start`, `stop`, `activity`, `spawn`) and an optional `subagent` field on `ClientMsg::Hook` with `#[serde(default)]`; verify with a unit test that a message without the field decodes and that an old-shape `Hook` struct decodes a new message
- [x] 1.2 Extend `validate_hook` to bound the subagent strings with `MAX_SESSION_FIELD_LEN`; verify with tests for an over-long `agent_id` and description
- [x] 1.3 Add a payload parser in `src/hook.rs` that maps a Claude hook JSON value to `(hook_event_name, Option<SubagentEvent>)` per design Decision 1; verify with table tests for `SubagentStart`, `SubagentStop`, a subagent `PreToolUse`, a parent `Agent` `PreToolUse`, a background `Agent` `PostToolUse` with `agentId`, an empty `agent_type`, and garbage input
- [x] 1.4 Add a bounded stdin read (skip on TTY, 8 MiB cap, 100 ms timeout) and wire the parser into `hook::run`; verify `echo '{...}' | swamp hook working` forwards the event and `swamp hook working </dev/null` and a TTY run behave as before
- [x] 1.5 In the status file fallback, skip the write for `start`, `stop`, and `activity` events; verify with a test that a subagent event with no daemon leaves `.swamp-status.json` unchanged

## 2. Claude hook installation

- [x] 2.1 Add `SubagentStart` (`working`) and `SubagentStop` (`idle`) to `HOOK_EVENTS` in `src/config/claude.rs`; verify the existing `apply_swamp_hooks_adds_all_events` and idempotency tests pass with the new events
- [x] 2.2 Update the README "Claude Code hooks" section with the two new events; verify the documented JSON matches what `apply_swamp_hooks` writes (checked with a one-off `include_str!` test under `cargo test`; removed because the nix source filter excludes `README.md`)

## 3. Daemon state

- [x] 3.1 Add `SubagentStatus`, `SubagentRecord`, and the in-memory `subagents` map on `DaemonState`; drop the entry in `remove_row` and scan-time pruning; verify with a test that removing a worktree drops its subagents
- [x] 3.2 Implement the `apply_hook` dispatch from design Decision 3, including the "parent untouched" rule for subagent events; verify with tests for start, stop, revive, unknown stop, unknown activity, empty type, and an idle parent that stays idle after a subagent `activity`
- [x] 3.3 Implement the pending-description FIFO with the 30-second expiry and the `agentId` override from design Decision 4; verify with tests for one spawn, two spawns of different types, two of the same type, and a stale pending entry
- [x] 3.4 Keep done subagents through parent hooks, including `UserPromptSubmit`; verify with `parent_hook_keeps_done_subagents` (the first draft dropped them on a new prompt, which the live test showed to be wrong)
- [x] 3.5 Add `expire_subagents(now)` (60-second done grace, 15-minute stale running) and call it from the daemon's 30-second tick, broadcasting only on change; verify with unit tests at the boundary times
- [x] 3.6 Confirm `persist` never writes subagent data; verify with a test that serializes the status file after subagent events and finds no subagent keys

## 4. Snapshot

- [x] 4.1 Add `SubagentRow` and `WorktreeRow::subagents` (`serde(default)`, skip when empty), filled and sorted in `snapshot()`; verify an older snapshot JSON without the field decodes and a populated row round-trips
- [x] 4.2 Verify with a daemon test that start, stop, and expiry broadcast a snapshot and an `activity` ping for a known subagent does not

## 5. TUI

- [x] 5.1 Change the AI pane entry filter so a worktree with a running subagent qualifies even when its parent is idle past the recent window; verify with a render test
- [x] 5.2 Render child rows under each parent per design Decision 7 (tree glyph, type, description, elapsed, spinner or done icon); verify with a `TestBackend` render snapshot that has two running and one done subagent
- [x] 5.3 Map every child row to its parent's snapshot index in `regions.ai`; verify with an input test that a click on a child row selects the parent worktree
- [x] 5.4 Collapse overflow into a `+N more` line when rows exceed the pane height; verify with a render test at a small height

## 6. Verification

- [x] 6.1 Run `nix build`, `nix develop --command cargo fmt --all --check`, and `nix develop --command cargo clippy --all-targets --all-features -- -D warnings`; all must pass
- [x] 6.2 Run `swamp init`, start a Claude session in a worktree, spawn two parallel subagents plus one background subagent, and confirm in the AI pane that child rows appear, finish, expire after about 60 seconds, and that the parent stays idle while the background subagent runs

Live check for 6.2: a headless `claude -p` session (Claude Code 2.1.322) ran with the new build first on `PATH` and only swamp's default hooks loaded. It started two background `Explore` subagents. The daemon snapshots showed both descriptions, an idle parent while both subagents ran, both subagents done, and both dropped about 80 seconds after they finished.
