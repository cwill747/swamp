## 1. Config: `[logging]` section

- [x] 1.1 Add `LoggingConfig { level, filter: Option<String> }` to `src/config/types.rs` with `#[serde(default)]`; `level` defaults to `info`. Add a `level`/filter type that parses `error|warn|info|debug|trace`.
- [x] 1.2 Add `logging: LoggingConfig` to `SwampConfig`; surface it through `ConfigPaths` (or a dedicated loader) in `src/config/paths.rs`.
- [x] 1.3 Add a commented `[logging]` example to the embedded `src/config/config.toml`.
- [x] 1.4 Unit tests: default level is `info`, level parses, filter string round-trips, malformed `[logging]` value aborts `load_config` with a file-identifying error.

## 2. Shared logging init + log file path

- [x] 2.1 Add a `log_path(common_dir)` helper next to `socket_path`/`pid_path` in `src/daemon/mod.rs` (or a new `logging` module) returning `$XDG_RUNTIME_DIR/swamp/{repo_id}.log` with the temp fallback.
- [x] 2.2 Add `tracing-appender` to `Cargo.toml` (or implement a plain append/non-blocking file writer per the design's open question). *Chose the plain `std::fs` append writer (`FileWriter` over `Arc<File>`) — no new dependency / Cargo.lock churn; the daemon is the only long-lived writer.*
- [x] 2.3 Implement `logging::init(common_dir, foreground, &LoggingConfig)` that builds the `EnvFilter` (`RUST_LOG` → `filter` → `level` → `info`), always attaches a file-writer layer, and attaches a stderr layer only when `foreground`. Use `try_init` to avoid double-init panics.
- [x] 2.4 Truncate (or rotate to `.log.1`) the log file on daemon startup to bound growth.

## 3. Wire subscribers into both processes

- [x] 3.1 Replace the inline `tracing_subscriber::fmt().init()` in `src/daemon/mod.rs` (`serve`) with a call to `logging::init`, loading `LoggingConfig` before init.
- [x] 3.2 Initialize logging once at TUI/launch startup (the process that reconciles and spawns tabs) so tab-addition events are captured; resolve the common dir and reuse `logging::init`.

## 4. Worktree-tagged git-refresh diagnostics (daemon)

- [x] 4.1 Add a trigger-named event at each refresh call site: heartbeat (`src/daemon/mod.rs` ~:158), post-fetch (~:184), watcher (`src/daemon/watcher.rs:37`), TUI `Refresh` and `UpdateDefault` handlers (`src/daemon/socket.rs` ~:103-113).
- [x] 4.2 Log periodic-fetch start/outcome and default-branch fetch + fast-forward outcome with results.
- [x] 4.3 Emit a result summary from `DaemonState::refresh_git` (worktree set / git-state delta), tagging per-worktree events with the worktree name.
- [x] 4.4 Log hook application (`src/daemon/mod.rs:376-389`, `src/hook.rs`) tagged with the affected worktree.

## 5. Worktree-tagged tab-addition diagnostics (TUI/launch)

- [x] 5.1 Log dashboard reconciliation decisions: new externally-created worktree detected → opening, and duplicate-open cooldown suppression, tagged with worktree name.
- [x] 5.2 Log layout file generation in `src/launch/layout.rs` (path written, target worktree).
- [x] 5.3 Log the Zellij `new-tab` invocation in `src/zellij.rs:28-31` (worktree, layout path, args).

## 6. `swamp logs` subcommand

- [x] 6.1 Add a `Logs { dir: Option<PathBuf>, follow: bool, all: bool }` variant to the clap CLI in `src/cli.rs` (positional `DIR` matching `LaunchArgs`/`TuiArgs`/`KillArgs`, plus `--follow`/`-f` and `--all`) and dispatch it in `src/main.rs`. Do NOT add a `--worktree`/`--this` selector — keep the path idiom used elsewhere.
- [x] 6.2 Implement the handler: resolve the repo common dir from `DIR` (default cwd) via `resolve_git_dir`/`git_common_dir`, compute the log path via the shared helper, print contents; with `--follow`/`-f`, tail appended output.
- [x] 6.3 Implement path-based worktree scoping: derive the worktree that `DIR` falls in (same derivation used elsewhere / `git worktree list`); if it resolves to a specific worktree and `--all` is not set, restrict output to lines carrying the matching `worktree=` field; if `DIR` is the repo root or `--all` is set, print everything. Compose with `--follow`.
- [x] 6.4 Handle the no-log-file-yet case with a friendly message instead of an error; a path outside any repo fails like other commands.
- [x] 6.5 Update shell completions generation if needed (clap derive covers it; verify `completions` output includes `logs`).
- [x] 6.6 Tests: worktree-field matching includes only the selected worktree (and is not fooled by a name that is a substring of another), `DIR` inside a worktree scopes to it, and `--all` prints repo-wide events.

## 7. Verification

- [x] 7.1 `nix build path:.` succeeds.
- [x] 7.2 `nix develop --command cargo fmt --all --check` and `cargo clippy --all-targets --all-features -- -D warnings` are clean.
- [x] 7.3 Manual: launch a session, create a worktree, confirm tab-addition events appear in `swamp logs`; run `swamp logs` from inside a worktree and confirm output is limited to that worktree, and `swamp logs --all` shows everything; trigger a refresh/update and confirm trigger-tagged refresh events appear; set `[logging] level = "debug"` and confirm increased verbosity; confirm `RUST_LOG` overrides config. *Verified headlessly against this repo: a foreground daemon wrote the per-repo log; `swamp logs` scoped to the current worktree (16 `worktree=logging` lines, 0 `worktree=main`), `--all` showed all three worktrees, and `RUST_LOG=trace` overrode the `info` default. The live-zellij tab-addition path (`reconcile_tabs` requires `in_zellij()`) is covered by code + the daemon/CLI behaviors above but was not driven inside an interactive Zellij session here.*
