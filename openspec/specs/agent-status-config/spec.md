# Agent Status Config Specification

## Purpose

Describe swamp's user configuration, initialization behavior, agent hook
integration, Codex notify bridge, and harness selection behavior.

## Requirements

### Requirement: Config Path Resolution
Swamp SHALL read user config from `$XDG_CONFIG_HOME/swamp/config.toml`, falling back to `$HOME/.config/swamp/config.toml` when `XDG_CONFIG_HOME` is unset.

#### Scenario: XDG config set
- **WHEN** `XDG_CONFIG_HOME` is set
- **THEN** swamp uses `$XDG_CONFIG_HOME/swamp/config.toml`

#### Scenario: XDG config unset
- **WHEN** `XDG_CONFIG_HOME` is unset
- **THEN** swamp uses `$HOME/.config/swamp/config.toml`

### Requirement: Config Defaults
Missing config values SHALL fall back to built-in defaults, while malformed config SHALL abort launch.

#### Scenario: Config missing
- **WHEN** the config file does not exist
- **THEN** swamp uses built-in default settings

#### Scenario: Config partially specified
- **WHEN** only some config keys are present
- **THEN** unspecified settings use defaults

#### Scenario: Config malformed
- **WHEN** the config file cannot be parsed
- **THEN** swamp aborts launch

#### Scenario: Dashboard config missing
- **WHEN** no dashboard table is configured
- **THEN** swamp uses 33, 34, and 33 for worktrees, AI, and shell columns

#### Scenario: Partial dashboard config
- **WHEN** one dashboard column is configured
- **THEN** that column uses the configured value and other columns use defaults

### Requirement: Harness Config
Harness config SHALL support `claude`, `codex`, and `choose`, with `claude` as the default.

#### Scenario: Harness pinned to Claude
- **WHEN** config sets the default harness to `claude`
- **THEN** worktree tabs use Claude regardless of per-worktree overrides

#### Scenario: Harness pinned to Codex
- **WHEN** config sets the default harness to `codex`
- **THEN** worktree tabs use Codex regardless of per-worktree overrides

#### Scenario: Harness choose mode
- **WHEN** config sets the default harness to `choose`
- **THEN** worktree tabs use a valid persisted per-worktree override or fall back to Claude

### Requirement: Initialization
`swamp init` SHALL write default config if missing, refresh managed config files, install or update Claude Code hooks, and configure Codex notify.

#### Scenario: Config file missing
- **WHEN** `swamp init` runs and the user config file does not exist
- **THEN** swamp writes the default config file

#### Scenario: Config file already exists
- **WHEN** `swamp init` runs and the user config file already exists
- **THEN** swamp preserves the existing config file

#### Scenario: Managed config refresh
- **WHEN** managed config files are missing or differ from embedded defaults
- **THEN** swamp writes the managed files

### Requirement: Read-only Config Protection
Initialization SHALL not modify read-only Claude or Codex config files and SHALL warn instead.

#### Scenario: Claude config read-only
- **WHEN** Claude settings cannot be written because the file is read-only
- **THEN** swamp leaves the file unchanged and warns

#### Scenario: Codex config read-only
- **WHEN** Codex config cannot be written because the file is read-only
- **THEN** swamp leaves the file unchanged and warns

### Requirement: Claude Hook Management
Initialization SHALL install or update swamp-managed Claude hooks while preserving unrelated user hooks.

#### Scenario: Existing foreign hooks
- **WHEN** Claude settings contain hooks not managed by swamp
- **THEN** `swamp init` preserves those hooks

#### Scenario: Stale swamp hooks
- **WHEN** Claude settings contain older swamp hook commands
- **THEN** `swamp init` updates them to the current commands

#### Scenario: Repeated init
- **WHEN** `swamp init` is run more than once
- **THEN** Claude hook configuration remains idempotent

### Requirement: Codex Notify Management
Initialization SHALL configure Codex `notify` to call `swamp codex-notify` while preserving unrelated TOML content where possible.

#### Scenario: Existing Codex config
- **WHEN** Codex config contains unrelated settings or comments
- **THEN** `swamp init` preserves them while setting notify

#### Scenario: Repeated config initialization
- **WHEN** `swamp init` is run more than once
- **THEN** managed config updates remain idempotent

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

### Requirement: Agent Record Preservation
Hook updates SHALL preserve previously recorded non-empty session name, session id, and harness override when later updates omit those fields.

#### Scenario: Status-only update
- **WHEN** an agent record already has session metadata and a later hook sends only status
- **THEN** the session metadata is preserved

#### Scenario: Harness override exists
- **WHEN** an agent record already has a harness override and a hook update arrives
- **THEN** the harness override is preserved

### Requirement: Codex Notify Events
The Codex notify bridge SHALL treat only `agent-turn-complete` events as idle status updates and SHALL ignore malformed or unknown payloads.

#### Scenario: Agent turn complete
- **WHEN** Codex invokes notify with an `agent-turn-complete` payload
- **THEN** swamp records an idle status update

#### Scenario: Unknown event
- **WHEN** Codex invokes notify with another event type
- **THEN** swamp exits without changing agent status

#### Scenario: Codex thread id
- **WHEN** a Codex payload contains a thread id
- **THEN** swamp does not persist that thread id as a Claude resume session id

### Requirement: Claude Session Resume
Claude panes SHALL resume safe persisted Claude session IDs, while unsafe IDs are ignored and Codex panes never resume sessions.

#### Scenario: Safe Claude session id
- **WHEN** the selected harness is Claude and a safe persisted session id exists
- **THEN** the generated agent command resumes that Claude session

#### Scenario: Unsafe session id
- **WHEN** a persisted session id contains unsafe characters
- **THEN** launch ignores the session id

#### Scenario: Codex harness
- **WHEN** the selected harness is Codex
- **THEN** the generated agent command starts Codex without session resume

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
