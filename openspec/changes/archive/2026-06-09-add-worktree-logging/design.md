## Context

swamp is a single binary structured as **CLI → detached daemon → TUI client**.
The daemon (`src/daemon/`) owns all the stateful, hard-to-observe work: git
state refreshes (heartbeat, filesystem watcher, periodic fetch, on-demand),
default-branch updates, agent-hook application, and resource sampling. The
dashboard TUI reconciles Zellij tabs for worktrees and spawns them via
`src/zellij.rs` / `src/launch/layout.rs`.

`tracing` + `tracing-subscriber` (with `env-filter`) are already
dependencies, and a handful of `tracing::{info,debug,warn,error}` calls exist
in the daemon. The fatal gap: the subscriber is only `.init()`-ed inside the
`--foreground` branch (`src/daemon/mod.rs:72-77`), but the normal `launch`
flow spawns the daemon **detached with stdout/stderr → `Stdio::null()`**
(`src/daemon/mod.rs:57-70`). Result: in real use, every log line is thrown
away. There is also no per-target or per-repo verbosity control beyond editing
the hard-coded `EnvFilter::new("info")` fallback, and the non-daemon code
paths (launch, TUI, worktree ops) use `eprintln!`/`println!` rather than
`tracing`.

Worktrees are identified by name (derived from `path.file_name()`), path, and
branch (`src/worktree/model.rs`). Runtime files live under
`$XDG_RUNTIME_DIR/swamp/{repo_id}.{sock,pid}` keyed by a hash of the git common
dir (`src/daemon/mod.rs:26-36`). Config is TOML at
`$XDG_CONFIG_HOME/swamp/config.toml`, loaded by `src/config/paths.rs`, with a
strict-parse policy (malformed config aborts).

## Goals / Non-Goals

**Goals:**
- Make detached-daemon logs land in an inspectable per-repository file.
- Let users set verbosity (level + optional per-target filter) from
  `config.toml`, with `RUST_LOG` still winning when set.
- Tag worktree-relevant events with the worktree name so a single repo log can
  answer "what happened to *this* worktree."
- Instrument the two motivating questions — "why was a tab added?" and "what
  did a git refresh do (and why)?" — at the existing decision points.
- Provide `swamp logs` to read/follow the active repo's log.

**Non-Goals:**
- Per-worktree *separate* log files (one repo log with worktree fields is
  sufficient and simpler to follow; see Decisions).
- Structured/JSON log output, log shipping, or remote aggregation.
- Instrumenting the TUI render loop or adding spans to every git call — only
  the decision points that answer the motivating questions.
- Changing the daemon's process/lifecycle model or the socket protocol.

## Decisions

### One repo-scoped log file with worktree fields, not per-worktree files
A single `{repo_id}.log` file written by the daemon, with each worktree-related
event carrying a `worktree = "<name>"` field (via `tracing` span or event
field). To view a single worktree, users filter the file (e.g.
`swamp logs | grep 'worktree="login"'`).
- *Why:* The daemon is one process that already holds the whole worktree set in
  `DaemonState`; routing events to N files by worktree would mean per-worktree
  subscribers/writers and racy file creation as worktrees come and go. Tabs are
  spawned by the TUI/launch process, not the daemon, so truly "per-worktree
  process" logging would fragment across processes anyway. A repo log keeps
  ordering and trigger→effect causality in one place.
- *Alternative considered:* a directory of per-worktree files. Rejected for
  complexity and loss of cross-worktree ordering; the field-tagging approach
  delivers the same "filter to one worktree" capability.

### File output via a non-blocking file writer + optional stderr layer
Build the subscriber from layers: a file-writing layer always on (path =
`{repo_id}.log`), plus a stderr layer only in `--foreground`. Use
`tracing_subscriber::fmt` with a file `MakeWriter`; consider
`tracing-appender` (non-blocking + rotation) to avoid blocking the async
runtime on disk writes.
- *Why:* The daemon is the only long-lived writer, so a single append writer is
  enough; non-blocking avoids stalling the tokio runtime. Keeping stderr in
  foreground preserves the current dev experience.
- *Alternative considered:* plain `std::fs::File` writer. Acceptable as a
  fallback if we want to avoid the extra dependency; decision recorded as an
  open question below.

### Level/filter resolution order: `RUST_LOG` → `[logging].filter` → `[logging].level` → `info`
Add `LoggingConfig { level: LevelString (default info), filter: Option<String> }`
to `SwampConfig` (`src/config/types.rs`), mirroring the existing
`#[serde(default)]` pattern so a missing section yields current behavior.
Resolve an `EnvFilter` as: if `RUST_LOG` set, use it (`try_from_default_env`);
else if `filter` set, parse it; else build from `level`. Invalid values abort
loading, consistent with the strict-parse policy already in `load_config`.
- *Why:* Matches the established config conventions and the existing `RUST_LOG`
  precedence in `src/daemon/mod.rs`. The bare `level` covers the common case;
  `filter` is the escape hatch for `swamp::zellij=debug` style tuning.

### Truncate-on-startup for lifecycle bounding
The daemon truncates (or rotates to `.log.1`) its log file when it starts,
since there is exactly one daemon per repo at a time. This bounds growth across
restarts without a size-watching background task.
- *Why:* Simplest bound that fits the one-daemon-per-repo invariant; a daemon's
  lifetime is the natural log window. If continuous-run growth becomes a
  problem, `tracing-appender` rolling is the follow-up.

### Instrument existing decision points, tagging worktree where relevant
- **Tab additions** (TUI/launch process): dashboard reconciliation
  (new-worktree detected, cooldown suppression), layout write in
  `src/launch/layout.rs`, and the `new-tab` spawn in `src/zellij.rs:28-31`.
  Requires wiring a minimal subscriber into the TUI/launch process too (today
  only the daemon initializes one).
- **Git refreshes** (daemon): name the trigger at each call site —
  `src/daemon/mod.rs` heartbeat (~:158) and post-fetch (~:184),
  `src/daemon/watcher.rs:37`, `src/daemon/socket.rs` Refresh/UpdateDefault
  handlers (~:103-113), and hook application (`src/daemon/mod.rs:376-389`).
  Emit a result summary from `DaemonState::refresh_git`.
- *Why:* These are exactly the points the motivating questions map to; an
  Explore pass confirmed the file:line locations.

### `swamp logs` subcommand, path-based worktree scoping
Add a `Logs { dir: Option<PathBuf>, follow: bool, all: bool }` variant to the
clap CLI (`src/cli.rs`). This mirrors the flag vocabulary already used across
swamp: a positional `DIR` ("path inside the repo, default current directory")
exactly as `LaunchArgs`/`TuiArgs`/`KillArgs` declare it, plus `--follow`/`-f`.
There is **no** `--worktree <name>` string or `--this` boolean — swamp never
identifies a worktree that way; it always resolves a target from a path (the
positional `DIR` for `launch`/`serve`/`tui`/`kill`, `--dir` for `hook`). We
follow that idiom.

Resolution:
1. Resolve the repo common dir from `DIR` (default cwd) with the same
   `resolve_git_dir` / `git_common_dir` helpers other commands use, and compute
   the log path with the shared helper. Path not inside a repo → the same error
   other commands produce. Missing log file → friendly "no logs yet" message.
2. Determine the worktree `DIR` falls in via the same `path.file_name()`-style
   derivation used elsewhere (or by matching `DIR` against `git worktree list`).
   If `DIR` resolves to a specific worktree and `--all` is not set, scope output
   to that worktree by keeping only lines that carry the matching `worktree=`
   field. If `DIR` is the repo/bare root (no specific worktree) or `--all` is
   set, print everything.
- *Why:* "Use the same flags as everywhere else" — the path positional is the
  established worktree/repo selector, so standing in a worktree and running
  `swamp logs` naturally scopes to it, and `--all` is the escape hatch for the
  whole-repo view. No new selector vocabulary to learn.
- *Implementation note:* match on the structured field token (e.g.
  `worktree="login"`) rather than a bare substring, so a branch whose name
  contains another worktree's name does not leak across filters. If the log
  format makes field extraction awkward, emit an enclosing span so the field is
  stable and greppable.
- *Alternative considered:* a `--worktree <name>` flag / `--this`. Rejected:
  inconsistent with the rest of the CLI, which selects targets by path.

## Risks / Trade-offs

- **Two processes need subscribers (daemon + TUI/launch)** → Add a small shared
  helper (e.g. `logging::init(common_dir, foreground)`) used by both; the TUI
  initializes it once at startup. Avoids duplicated subscriber-building logic
  and double-init panics (guard with `try_init`).
- **Disk writes on the async runtime could block** → Use a non-blocking writer
  (`tracing-appender`) or a dedicated blocking writer; keep formatting cheap.
- **Truncate-on-startup loses the previous run's logs** → Acceptable since the
  motivating debugging happens within a daemon's lifetime; optionally keep one
  rotated `.log.1`. Documented behavior so users grab logs before restarting.
- **Log file may contain branch/worktree names (mildly sensitive paths)** →
  Same data already in the TUI and on disk under the repo; no new secrets.
  Worktree names, not file contents, are logged.
- **New `tracing-appender` dependency** → Small, first-party tokio-rs crate; if
  undesirable, fall back to a plain file writer (open question).
- **Added log calls in hot paths (watcher debounce, heartbeat every 30s)** →
  Keep these at `debug`; default `info` level keeps them off unless requested.

## Migration Plan

- Additive change: new optional `[logging]` config section (absent = `info`),
  new log file, new subcommand. No protocol or data-model changes.
- Update the embedded `src/config/config.toml` with a commented `[logging]`
  example so `swamp init` documents it; existing user configs keep working
  untouched.
- Rollback is trivial: the feature is self-contained; reverting restores the
  prior (discarded-logs) behavior without data migration.

## Open Questions

- Adopt `tracing-appender` for non-blocking writes + rotation, or use a plain
  `std::fs::File` append writer to avoid a new dependency? (Leaning
  `tracing-appender` for non-blocking behavior under tokio.)
- Truncate vs. keep one rotated `.log.1` on startup — is a single prior run
  worth retaining by default?
- Should `swamp logs` default to follow when run inside an attached session, or
  always require `--follow`? (Leaning: print by default, `--follow`/`-f` to
  tail.)
- A bare `swamp logs` from inside a worktree now scopes to that worktree (path
  idiom), with `--all` for the whole repo. Confirm this default is preferred
  over the inverse (whole-repo by default); the path-based scoping makes
  "scope to where I am" the natural, CLI-consistent default.
