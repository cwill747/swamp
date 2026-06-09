## Why

Today swamp's daemon does the interesting work — opening Zellij tabs for new
worktrees, running periodic and watcher-driven git refreshes, fetching the
default branch, applying agent hooks — but it is spawned **detached with
stdout/stderr routed to `/dev/null`** (`src/daemon/mod.rs:57-70`). The
`tracing` subscriber is only initialized inside the `--foreground` path
(`src/daemon/mod.rs:72-77`), so in normal use **every log line is discarded**.
When a tab appears unexpectedly, or a refresh seems to fire "for no reason,"
there is no record to inspect after the fact. There is no way to raise or lower
verbosity per repository without editing source.

## What Changes

- Persist daemon logs to a per-repository **log file** under the swamp runtime
  directory (alongside the existing socket/PID files) instead of discarding
  them, so detached daemons leave an inspectable trail.
- Add a `[logging]` section to `config.toml` with a configurable **level**
  (`error|warn|info|debug|trace`, default `info`) and an optional free-form
  **filter** string (full `EnvFilter` syntax, e.g. `swamp::zellij=debug`).
  `RUST_LOG` continues to override config when set.
- **Tag log events with the worktree they concern** (worktree name + path as
  span/event fields) so a single repo log can be filtered down to "what
  happened to *this* worktree."
- Instrument the currently-silent debug points so the two motivating questions
  are answerable from the log:
  - **"Why was a tab added?"** — emit events at tab-reconciliation decisions and
    Zellij tab spawns (cooldown suppressions, externally-created worktree
    detection, layout written, `new-tab` invoked) in `src/launch.rs`,
    `src/launch/layout.rs`, `src/zellij.rs`, and the dashboard reconciliation
    path.
  - **"What did a git refresh do?"** — emit events naming the *trigger*
    (heartbeat, filesystem watcher, periodic fetch, on-demand TUI refresh,
    default-branch update, hook) and the resulting worktree-state delta in
    `src/daemon/mod.rs`, `src/daemon/watcher.rs`, and `src/daemon/socket.rs`.
- Add a `swamp logs` subcommand to print/follow the active repo's log file. It
  selects its target by **path** like every other swamp command (a positional
  `DIR`, default current directory): standing in a worktree scopes output to
  that worktree's tagged events, and `--all` prints the whole-repo log. Also
  rotate/truncate the file on daemon startup so it does not grow without bound.

## Capabilities

### New Capabilities
- `diagnostic-logging`: Per-repository, worktree-tagged, level-configurable
  diagnostic logging — where logs are written, how the level/filter is
  configured, which lifecycle events are recorded, and the `swamp logs`
  inspection command.

### Modified Capabilities
<!-- No existing spec's REQUIREMENTS change; instrumentation is additive
     behavior captured by the new diagnostic-logging capability. -->

## Impact

- **Code**: `src/daemon/mod.rs` (subscriber init for detached mode, log file
  path, instrumentation), `src/daemon/{watcher,socket,state}.rs`,
  `src/launch.rs` + `src/launch/layout.rs`, `src/zellij.rs`, `src/hook.rs`,
  `src/config/{types,paths}.rs` + `src/config/config.toml` (new `[logging]`
  section), `src/cli.rs` + `src/main.rs` (new `logs` subcommand).
- **Dependencies**: `tracing-subscriber` already present; may add
  `tracing-appender` for file output / rotation, or use a plain file writer.
- **Config / runtime**: new `[logging]` block in `config.toml` (optional,
  defaults preserve current behavior aside from logs now being written); new
  log file under `$XDG_RUNTIME_DIR/swamp/{repo_id}.log`.
- **Compatibility**: no breaking changes; `RUST_LOG` precedence and existing
  foreground stderr behavior are preserved.
