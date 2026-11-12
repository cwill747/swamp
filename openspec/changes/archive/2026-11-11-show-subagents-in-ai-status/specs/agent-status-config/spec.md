## MODIFIED Requirements

### Requirement: Agent Status Hook
`swamp hook` SHALL record `working`, `waiting`, and `idle` status updates for a worktree, with optional session name and session id. When stdin is not a terminal and contains a Claude Code hook JSON payload, `swamp hook` SHALL read the hook event name, agent id, agent type, and `Agent` tool description from it and forward them with the status update.

#### Scenario: Daemon reachable
- **WHEN** `swamp hook` can reach the daemon
- **THEN** it sends the status update through the daemon socket

#### Scenario: Daemon unreachable
- **WHEN** `swamp hook` cannot reach the daemon within its short timeout
- **THEN** it atomically updates `<git-common-dir>/.swamp-status.json` directly

#### Scenario: Optional session fields
- **WHEN** session name or session id is provided
- **THEN** swamp stores the non-empty values with the agent record

#### Scenario: Hook payload on stdin
- **WHEN** Claude Code runs `swamp hook` with a JSON payload on stdin that contains `agent_id` and `agent_type`
- **THEN** swamp forwards the event name, agent id, and agent type to the daemon

#### Scenario: No payload or bad payload
- **WHEN** stdin is a terminal, is empty, or does not contain valid JSON
- **THEN** swamp records the positional status as it does without a payload and exits with success

#### Scenario: Slow stdin
- **WHEN** stdin stays open without data
- **THEN** swamp stops the read after a short timeout and continues without a payload

#### Scenario: Older daemon
- **WHEN** a newer `swamp hook` sends subagent fields to a daemon that does not know them
- **THEN** the daemon still applies the status update

## ADDED Requirements

### Requirement: Claude Subagent Hooks
`swamp init` SHALL install swamp-managed `SubagentStart` and `SubagentStop` hooks in Claude settings, with the same preservation and idempotency rules as the other swamp-managed Claude hooks.

#### Scenario: Fresh init
- **WHEN** `swamp init` runs and Claude settings have no swamp subagent hooks
- **THEN** swamp adds a `SubagentStart` hook and a `SubagentStop` hook that run `swamp hook`

#### Scenario: Repeated init with subagent hooks
- **WHEN** `swamp init` runs and the subagent hooks are already current
- **THEN** Claude settings do not change

#### Scenario: Read-only settings without subagent hooks
- **WHEN** Claude settings are read-only and lack the subagent hooks
- **THEN** swamp leaves the file unchanged and warns that hooks are out of date
