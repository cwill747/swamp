## Why

The AI status view shows one row per worktree agent. When a Claude Code session
delegates work to subagents (the `Agent`/`Task` tool), the view shows only
"working" for the parent. You cannot see how many subagents run, what each one
does, or which one is blocked. Parallel subagent workflows are now common, so
this hidden state makes the dashboard less useful.

## What Changes

- `swamp hook` reads the Claude Code hook JSON payload from stdin when one is
  present. Swamp extracts the hook event name, the subagent id, and the subagent
  type. Existing positional status arguments keep working.
- `swamp init` installs `SubagentStart` and `SubagentStop` hooks in addition to
  the current hooks.
- The daemon tracks subagent records under each worktree's agent record: id,
  type, description, status, start time, and last-update time.
- Worktree snapshot rows carry a list of subagents. Older peers that omit the
  list still decode.
- The AI status pane renders each subagent as an indented child row under its
  parent worktree row, with a status icon, the subagent type, the description,
  and the elapsed time.
- Hooks that fire inside a subagent update that subagent only. They no longer
  flip the parent row back to "working" after the parent stops, which today
  happens while background subagents run.
- Finished subagents stay visible for a short grace period, then the daemon
  drops them. The daemon also drops subagents that send no event for a long
  stale timeout.
- Codex sessions are unchanged. Codex has no subagent hook events.

## Capabilities

### New Capabilities
- `subagent-status`: Tracks Claude Code subagent lifecycle from hook events and
  shows subagents nested under their parent agent in the AI status view.

### Modified Capabilities
- `agent-status-config`: The Claude hook set gains `SubagentStart` and
  `SubagentStop`, and `swamp hook` accepts an optional hook payload on stdin.
- `daemon-tui-status`: Worktree snapshot rows gain a subagent list.

## Impact

- `src/config/claude.rs`: new hook events in `HOOK_EVENTS`.
- `src/hook.rs`, `src/cli.rs`: stdin payload parsing; status file fallback
  path.
- `src/daemon/socket.rs`: `ClientMsg::Hook` gains optional subagent fields
  (serde defaults keep older clients compatible).
- `src/daemon/state.rs`, `src/daemon/mod.rs`: subagent records, expiry, and
  snapshot fields.
- `src/tui/view/ai.rs`, `src/tui/input.rs`: child rows and the mapping from
  visible rows to worktree indices for selection and clicks.
- Users must re-run `swamp init` to get the new hooks. Without them, the view
  behaves as it does today.
