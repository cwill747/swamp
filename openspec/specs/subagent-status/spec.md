# subagent-status Specification

## Purpose
Tracks the lifecycle of Claude Code subagents from hook events and shows each
subagent nested under its parent agent in the AI status view.

## Requirements

### Requirement: Subagent Lifecycle Tracking
The daemon SHALL keep a list of subagents for each worktree agent record. A `SubagentStart` event SHALL add a running subagent with its id, type, and start time. A `SubagentStop` event SHALL mark the matching subagent as done and record its finish time.

#### Scenario: Subagent starts
- **WHEN** the daemon receives a `SubagentStart` hook with an agent id and agent type for a worktree
- **THEN** that worktree's subagent list contains a running entry with that id and type

#### Scenario: Subagent stops
- **WHEN** the daemon receives a `SubagentStop` hook whose agent id matches a tracked subagent
- **THEN** that subagent's status becomes done and its finish time is recorded

#### Scenario: Stop for an unknown subagent
- **WHEN** the daemon receives a `SubagentStop` hook whose agent id is not tracked
- **THEN** the daemon adds the subagent as done so a short-lived subagent is still visible

#### Scenario: Subagent resumed
- **WHEN** the daemon receives a `SubagentStart` hook for an agent id that is already tracked
- **THEN** the existing entry becomes running again and keeps its original start time

#### Scenario: Internal agent with empty type
- **WHEN** a subagent hook arrives with an empty agent type
- **THEN** the daemon ignores the event for subagent tracking

### Requirement: Subagent Activity Attribution
A tool hook that carries a subagent id SHALL update only that subagent's last-activity time. It SHALL NOT change the parent agent's status or timestamp.

#### Scenario: Tool call inside a subagent
- **WHEN** a `PreToolUse` or `PostToolUse` hook carries an agent id that matches a tracked subagent
- **THEN** the subagent's last-activity time moves forward and the parent status is unchanged

#### Scenario: Background subagent after the parent stops
- **WHEN** the parent agent is idle and a background subagent sends a tool hook
- **THEN** the parent row stays idle and the subagent row shows as running

#### Scenario: Tool hook for an untracked subagent
- **WHEN** a tool hook carries an agent id that the daemon does not track
- **THEN** the daemon adds a running subagent entry with the reported agent type

### Requirement: Subagent Description
The daemon SHALL attach a short task description to a subagent when the parent's `Agent` tool call supplies one. When no description is available, the subagent SHALL show only its type.

#### Scenario: Description available
- **WHEN** the parent `Agent` tool call has a description and the daemon can match it to the subagent
- **THEN** the subagent entry carries that description

#### Scenario: Description unavailable
- **WHEN** the daemon cannot match a description to the subagent
- **THEN** the subagent entry has no description and the TUI shows the agent type

### Requirement: Subagent Expiry
The daemon SHALL drop a done subagent 60 seconds after it finishes. The daemon SHALL drop a running subagent that sends no event for 15 minutes. Parent hooks, including a new user prompt, SHALL NOT drop done subagents before their grace period ends.

#### Scenario: Done subagent grace period
- **WHEN** a subagent has been done for more than 60 seconds
- **THEN** the next snapshot does not include it

#### Scenario: Stale running subagent
- **WHEN** a running subagent has sent no event for more than 15 minutes
- **THEN** the next snapshot does not include it

#### Scenario: Parent turn after a background subagent finishes
- **WHEN** a background subagent finishes and the parent agent then receives a `UserPromptSubmit` hook
- **THEN** the done subagent stays visible until its grace period ends

#### Scenario: Worktree removed
- **WHEN** a worktree is removed
- **THEN** its subagent list is removed with its agent record

### Requirement: Subagent State Is Not Persisted
Subagent records SHALL live in daemon memory only. A daemon restart SHALL start with empty subagent lists, and the status file fallback SHALL NOT record subagents.

#### Scenario: Daemon restart
- **WHEN** the daemon restarts while subagents are running
- **THEN** the new daemon reports no subagents until new subagent hooks arrive

#### Scenario: Daemon unreachable
- **WHEN** `swamp hook` cannot reach the daemon and the payload is a subagent event
- **THEN** swamp does not write subagent data to the status file and does not change the parent status

### Requirement: Nested Subagent Rows
The AI status pane SHALL render each subagent of a shown worktree as an indented child row directly under the parent row. Each child row SHALL show a status icon, the agent type, the description when present, and the elapsed time since the subagent started. Parent rows SHALL take priority over child rows for pane space. Running subagents SHALL sort before done subagents, then by start time.

#### Scenario: Parent with running subagents
- **WHEN** a worktree agent is shown and it has two running subagents
- **THEN** two indented child rows appear under the parent row with the working icon

#### Scenario: Done subagent
- **WHEN** a subagent is done and still inside its grace period
- **THEN** its child row shows the done icon and the elapsed time stops at its finish time

#### Scenario: Idle parent with a running background subagent
- **WHEN** a worktree agent is idle but has a running subagent
- **THEN** the AI status pane shows the parent row even when the parent's idle time is older than the recent-idle window

#### Scenario: Child row selection
- **WHEN** the user clicks or selects a subagent child row
- **THEN** the TUI acts on the parent worktree, the same as for the parent row

#### Scenario: Pane height limit
- **WHEN** the pane has fewer lines than the parent and child rows need
- **THEN** parent rows keep their lines first, and a parent whose child rows do not all fit in the remaining lines shows a `+N more` summary line in place of its last child rows
