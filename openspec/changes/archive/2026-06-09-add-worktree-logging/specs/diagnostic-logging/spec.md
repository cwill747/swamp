## ADDED Requirements

### Requirement: Persistent Daemon Log File
The daemon SHALL write its diagnostic logs to a per-repository log file located alongside its runtime socket and PID files (`$XDG_RUNTIME_DIR/swamp/{repo_id}.log`, with the same temp-directory fallback used for the socket), so logs from a detached daemon are inspectable after the fact. Foreground daemons SHALL continue to also emit logs to stderr.

#### Scenario: Detached daemon logs are recorded
- **WHEN** the daemon is spawned detached (the normal `launch` path with stdout/stderr suppressed)
- **THEN** its log events are written to the per-repository log file
- **AND** the file persists after the command that triggered the work returns

#### Scenario: Foreground daemon still logs to stderr
- **WHEN** the daemon runs with `--foreground`
- **THEN** log events are emitted to stderr
- **AND** also written to the per-repository log file

#### Scenario: Runtime dir fallback
- **WHEN** `$XDG_RUNTIME_DIR` is unavailable
- **THEN** the log file is placed under the same temp runtime fallback as the socket and PID files

### Requirement: Log File Lifecycle
The daemon SHALL bound log file growth so it does not accumulate indefinitely across daemon restarts.

#### Scenario: Startup does not grow unbounded
- **WHEN** a daemon starts and a log file from a previous run already exists
- **THEN** the daemon truncates or rotates the log file so a single file does not grow without bound across restarts

### Requirement: Configurable Log Level
The system SHALL read a `[logging]` section from `config.toml` with a `level` field accepting `error`, `warn`, `info`, `debug`, or `trace`, defaulting to `info` when the section or field is absent. An optional `filter` field SHALL accept full `EnvFilter` directive syntax (e.g. `swamp::zellij=debug,info`) for finer-grained, per-target control.

#### Scenario: Default level
- **WHEN** `config.toml` has no `[logging]` section
- **THEN** the daemon logs at `info` level
- **AND** existing behavior is otherwise unchanged

#### Scenario: Level override
- **WHEN** `[logging] level = "debug"` is set
- **THEN** the daemon emits `debug`-level events and above

#### Scenario: Per-target filter
- **WHEN** a `[logging] filter` directive string is set
- **THEN** the daemon applies it as its env-filter, taking precedence over the bare `level`

#### Scenario: Malformed logging config
- **WHEN** the `[logging]` section contains an invalid level or filter value
- **THEN** config loading fails with an error identifying the config file, consistent with how other malformed config is rejected

### Requirement: RUST_LOG Precedence
When the `RUST_LOG` environment variable is set, it SHALL override the `[logging]` configuration, preserving the existing environment-driven control.

#### Scenario: Environment overrides config
- **WHEN** `RUST_LOG` is set and `[logging]` is also configured
- **THEN** the `RUST_LOG` value determines the active filter
- **AND** the `[logging]` config is ignored for that run

### Requirement: Worktree-Tagged Events
Log events that concern a specific worktree SHALL carry that worktree's name (and, where available, its path) as structured fields or an enclosing span, so the repository log can be filtered down to a single worktree.

#### Scenario: Worktree-scoped event
- **WHEN** the daemon logs an event about a specific worktree (e.g. a refresh result, a tab spawn, a hook application)
- **THEN** the event includes the worktree name as a structured field or span

#### Scenario: Repo-wide event
- **WHEN** the daemon logs an event not tied to a single worktree (e.g. daemon startup, periodic fetch start)
- **THEN** the event is recorded without a worktree field

### Requirement: Tab-Addition Diagnostics
The system SHALL log the decisions that lead to (or suppress) opening a Zellij worktree tab, so a user can determine why a tab was added. Recorded events SHALL include detection of an externally-created worktree during dashboard reconciliation, duplicate-open cooldown suppression, layout file generation, and the Zellij `new-tab` invocation, each tagged with the target worktree.

#### Scenario: Tab opened for new worktree
- **WHEN** the dashboard TUI opens a tab for a worktree that appeared in a snapshot
- **THEN** a log event records the worktree and that reconciliation triggered the open

#### Scenario: Duplicate open suppressed
- **WHEN** tab reconciliation suppresses a duplicate open during the cooldown window
- **THEN** a log event records the suppressed worktree and the cooldown reason

#### Scenario: Tab spawn invoked
- **WHEN** swamp invokes Zellij to spawn a worktree tab
- **THEN** a log event records the worktree, the layout path, and the spawn invocation

### Requirement: Git-Refresh Diagnostics
The system SHALL log each git refresh with the trigger that caused it and a summary of what changed, so a user can determine why a refresh occurred and what it did. Triggers SHALL be distinguishable among at least: periodic heartbeat, filesystem watcher, periodic fetch, on-demand TUI refresh, default-branch update, and agent hook.

#### Scenario: Heartbeat refresh
- **WHEN** the periodic heartbeat triggers `refresh_all`
- **THEN** a log event records the heartbeat trigger

#### Scenario: Watcher refresh
- **WHEN** a filesystem change triggers a refresh
- **THEN** a log event records the watcher trigger

#### Scenario: Fetch refresh
- **WHEN** the periodic `git fetch` runs and refreshes state
- **THEN** log events record the fetch start, its outcome, and the subsequent refresh

#### Scenario: Default-branch update
- **WHEN** the user triggers a default-branch update from the TUI
- **THEN** log events record the fetch and fast-forward merge outcome

#### Scenario: Refresh result summary
- **WHEN** a refresh changes the set of worktrees or their git state
- **THEN** a log event summarizes the resulting worktree rows or the delta

### Requirement: Log Inspection Command
The CLI SHALL provide a `swamp logs` subcommand that prints the active repository's log file, with an option to follow (tail) new output, resolving the repository the same way other commands do.

#### Scenario: Print logs
- **WHEN** the user runs `swamp logs` inside a swamp-managed repository
- **THEN** the contents of that repository's log file are printed

#### Scenario: Follow logs
- **WHEN** the user runs `swamp logs` with the follow option
- **THEN** the command streams new log output as it is appended

#### Scenario: No log file yet
- **WHEN** the user runs `swamp logs` and no log file exists for the repository
- **THEN** the command reports that there are no logs rather than failing abnormally

### Requirement: Worktree-Scoped Log Inspection
The `swamp logs` subcommand SHALL select its target worktree by path, consistent with how other swamp commands resolve their target: an optional positional `DIR` argument (default: current directory), the same convention used by `swamp launch`, `swamp tui`, and `swamp kill`. When `DIR` resolves to a path inside a specific worktree, output SHALL be restricted to events tagged with that worktree (per the Worktree-Tagged Events requirement). An `--all` flag SHALL print the entire repository log regardless of the worktree `DIR` falls in. Worktree scoping SHALL compose with the follow option. The subcommand SHALL NOT introduce a name-string or boolean worktree selector that is not used elsewhere in the CLI.

#### Scenario: Scope to the current worktree
- **WHEN** the user runs `swamp logs` from inside a worktree with no `--all` flag
- **THEN** output is restricted to events tagged with the worktree containing the current directory

#### Scenario: Scope by explicit path
- **WHEN** the user runs `swamp logs DIR` where `DIR` is a path inside a worktree
- **THEN** output is restricted to events tagged with that worktree

#### Scenario: Whole repository log
- **WHEN** the user runs `swamp logs --all`
- **THEN** events for every worktree and repo-wide events are printed

#### Scenario: Filter while following
- **WHEN** the user runs `swamp logs` with the follow option and a worktree-scoped target
- **THEN** the command streams only newly-appended events tagged with that worktree

#### Scenario: Path outside a repo
- **WHEN** `DIR` (or the current directory) is not inside a swamp-managed repository
- **THEN** the command fails to resolve the repository, consistent with other swamp commands
