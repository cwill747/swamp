## Context

Swamp's Claude hooks run `swamp hook <status>` and ignore stdin. The daemon
keeps one `AgentRecord` per worktree (`src/daemon/state.rs`), and
`WorktreeRow` copies the status into each snapshot. The AI pane
(`src/tui/view/ai.rs`) renders one table row per worktree and records a
visible-row-to-snapshot-index map in `app.regions.ai` for clicks.

Claude Code hook facts this design depends on (from the hooks reference at
https://code.claude.com/docs/en/hooks):

- `SubagentStart` stdin: common fields plus `agent_id` and `agent_type`.
- `SubagentStop` stdin: adds `agent_id`, `agent_type`,
  `agent_transcript_path`, and `last_assistant_message`.
- Hooks that fire inside a subagent (`PreToolUse`, `PostToolUse`) carry
  `agent_id` and `agent_type` as common fields.
- `Notification` does not document `agent_id`.
- The parent's `Agent` tool `PreToolUse` has `tool_input.description` and
  `tool_input.subagent_type`. For a background run, the `PostToolUse`
  `tool_response` has `agentId` and `description`.
- Some internal agents fire `SubagentStop` with an empty `agent_type`.

See `proposal.md` for motivation.

## Goals / Non-Goals

**Goals:**
- Show Claude subagents of each worktree agent, live, in the AI pane.
- Stop subagent tool calls from changing the parent status.
- Keep old clients, old daemons, and old status files compatible.

**Non-Goals:**
- Codex subagents. Codex has no subagent events.
- Nested subagents (a subagent that spawns a subagent) as a deeper tree. All
  subagents of a session render one level under the parent.
- Agent-team teammates as separate entities. They appear as subagents if
  Claude Code fires `SubagentStart` for them.
- Subagent "waiting" state. `Notification` has no `agent_id`, so a permission
  prompt from a subagent sets the parent to waiting, as it does today.
- Persisting subagents across daemon restarts.
- Selecting or opening a subagent transcript from the TUI.

## Decisions

### 1. Read the hook payload from stdin in `swamp hook`

`swamp hook` reads stdin only when stdin is not a TTY. It caps the read at
8 MiB and at 100 ms, then parses JSON with `serde_json`. A reader thread does
the read, so a stdin that never closes cannot hold the process open. The cap
is large because a subagent's `PostToolUse` can carry a large
`tool_response`. A cut payload does not parse, and the hook would then apply
its positional status to the parent. It extracts
`hook_event_name`, `agent_id`, `agent_type`, `tool_name`,
`tool_input.description`, `tool_input.subagent_type`, and
`tool_response.agentId`. Any failure yields "no payload".

The hook commands keep their positional status, so a payload-less invocation
behaves as today. The new events use new positional values only through the
event name. `SubagentStart` and `SubagentStop` install as
`swamp hook working` and `swamp hook idle`; the daemon reads the event name
to know the status applies to the subagent, not the parent.

Alternative: separate `swamp subagent start|stop` subcommands. Rejected,
because tool hooks inside subagents still go through `swamp hook` and need the
payload anyway. One parser is simpler.

Alternative: Read subagent state from transcript files under
`<session>/subagents/`. Rejected. It needs file watching, and the transcript
format is not a stable interface.

### 2. Extend `ClientMsg::Hook` with one optional struct

Add `#[serde(default)] subagent: Option<SubagentEvent>` to `ClientMsg::Hook`,
where `SubagentEvent { kind, agent_id, agent_type, description }`.
`kind` is `start`, `stop`, `activity`, or `spawn` (the parent `Agent` tool
call that supplies a description). `swamp hook` exits without a message when
a lifecycle event has no usable agent id or type, so such an event never
reaches the parent status. An old daemon ignores the unknown field
(serde ignores unknown fields on this enum today; a task confirms this with a
test). `validate_hook` bounds each string with the existing
`MAX_SESSION_FIELD_LEN`.

### 3. Store subagents in memory beside `AgentRecord`

`DaemonState` gains `subagents: HashMap<String, Vec<SubagentRecord>>`, keyed
by worktree name. It is not part of `AgentRecord`, so `persist` and the
status file never see it. `remove_row` and scan-time pruning drop the entry
with the agent record.

`apply_hook` dispatches on the subagent event:

- `start`: insert or revive the record (keep `started_ts`). Changed.
- `stop`: set `Done` and `finished_ts`; insert as done if unknown. Changed.
- `activity`: move `last_ts`; insert as running if unknown (changed);
  otherwise unchanged. The parent record is not touched.
- `spawn`: push the description onto a per-worktree pending queue keyed by
  `subagent_type`. Then apply the parent status as today.

Parent hooks never drop done subagents. A first draft dropped them on
`UserPromptSubmit`. The live test showed that a background subagent's finish
starts a new parent turn, which fires `UserPromptSubmit`. So that rule removed
each done subagent at once, and the grace period never showed.

The `changed` flag drives persist and broadcast as it does today.

### 4. Match descriptions with a FIFO queue per agent type

The parent `Agent` `PreToolUse` fires before `SubagentStart`, but neither
carries a shared id. On `start`, the daemon pops the oldest pending
description whose type equals `agent_type`. Pending entries older than 30
seconds are discarded. A background `PostToolUse` with `tool_response.agentId`
overwrites the description for that exact id, which corrects any mismatch.

Parallel spawns of the same type can swap descriptions in the foreground case.
This is acceptable, because the label is informational.

Alternative: no descriptions, type only. Rejected: "general-purpose" x3 tells
you little.

### 5. Expiry on the existing 30-second tick

The daemon already runs a 30-second interval task. Add a
`DaemonState::expire_subagents(now)` call to it. It drops done records older
than 60 seconds and running records with `last_ts` older than 15 minutes. It
broadcasts only if it dropped anything. The TUI hides done records past
60 seconds by itself, so the up-to-30-second tick lag is not visible.

A subagent that runs one tool call for more than 15 minutes is dropped, then
comes back on its next tool hook through the `activity` insert path.

### 6. Snapshot field

`WorktreeRow` gains
`#[serde(default, skip_serializing_if = "Vec::is_empty")] subagents: Vec<SubagentRow>`.
`snapshot()` fills it from `DaemonState::subagents`, sorted running first,
then by `started_ts`.

### 7. TUI rendering

`render_ai_status` builds a flat list of display rows: a parent row, then its
child rows. A worktree qualifies for display if it qualifies today or has any
running subagent. Child rows use the same table columns:

| column | child content |
|---|---|
| icon | `  ` + working spinner or done check |
| status | blank |
| name | `└ <agent_type>` (`├` for non-last) |
| branch + git | blank |
| elapsed | since `started_ts` (frozen at `finished_ts` when done) |
| session | `description`, in the column that holds the parent's session title |

Each display row maps to its parent's snapshot index in `regions.ai`, so
selection and clicks reuse the existing worktree actions. When rows exceed
`inner.height`, parent rows take priority, so one busy agent cannot push the
other worktrees out of the pane. Child rows use the space that is left, in
order. A parent whose child rows do not all fit shows `└ +N more` in place of
the last ones.

## Risks / Trade-offs

- [Claude Code changes hook payload fields] → Parsing is lenient and
  optional. Missing fields degrade to today's behavior.
- [Hook latency: stdin read on every tool call] → Bounded read (100 ms,
  8 MiB). Claude writes the payload before it runs the hook, so the read
  normally finishes at once.
- [Users who do not re-run `swamp init`] → Tool hooks inside subagents still
  carry `agent_id`, so subagents still appear through the `activity` path, but
  without the done state. `swamp init` adds the start and stop events.
- [Nix or home-manager read-only Claude settings] → `swamp init` warns, as it
  does today. The README hook section lists the two new events.
- [Description mismatch for parallel same-type spawns] → Accepted (see
  Decision 4).

## Migration Plan

No data migration. Ship the daemon and the hook together in one binary. The
first `swamp init` after upgrade adds the new hooks. Rollback is a downgrade;
old daemons ignore the new field, and old TUIs ignore the new snapshot field.
