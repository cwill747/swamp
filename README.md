<p align="center">
  <strong>swamp</strong>
</p>

<p align="center">
  <strong>Zellij-integrated git worktree dashboard</strong>
</p>

<p align="center">
  <a href="#installation">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#commands">Commands</a> ·
  <a href="#claude-code-hooks">Claude hooks</a> ·
  <a href="#how-it-works">How it works</a>
</p>

---

swamp turns a git repo into a [Zellij](https://zellij.dev) session with one tab
per worktree, a live status TUI, and per-worktree agent status reporting. It's
built for parallel AI-agent workflows: every worktree gets its own tab with
[lazygit](https://github.com/jesseduffield/lazygit), a Claude pane, a shell, and
a status sidebar — and a daemon watches git state plus agent hooks so you can
see at a glance which worktrees are working, waiting on you, or idle.

**Philosophy**: Build on tools you already use. Zellij for windowing, git for
worktrees, lazygit for diffs, your agent for coding — swamp wires them together.

## Why swamp?

**Parallel agent workflows.** Run a different agent in every worktree. Each tab
has its own shell, lazygit, and Claude pane. Switching tasks is switching tabs.

**Status at a glance.** The embedded TUI shows every worktree's branch, ahead /
behind / dirty counts, and live agent status (working, waiting, idle). When
Claude needs you in one tab, you see it from the others.

**Zero per-repo setup.** Drop into any bare clone or repo with worktrees and run
`swamp`. It writes its own starship and lazygit configs to
`~/.config/swamp/`, generates a Zellij layout on the fly, and attaches.

**Single binary, single daemon per repo.** A small Rust binary plus a per-repo
Unix-socket daemon — no plugins, no shell hooks beyond Claude's.

## Features

- One Zellij tab per git worktree, with a layout containing lazygit, Claude, a
  shell, and a swamp status sidebar
- Live status TUI ([`swamp tui`](#swamp-tui)) listing every worktree with
  branch, ahead/behind, dirty state, and agent status
- Per-worktree agent status reporting via [`swamp hook`](#swamp-hook) +
  [Claude Code hooks](#claude-code-hooks)
- Auto-detection of bare clones (adds a "dashboard" tab with lazygit + swamp
  TUI as the entry point)
- Per-repo daemon that watches git state and broadcasts snapshots to the TUI
- Version-aware: if the running daemon was built from a different swamp version,
  swamp prompts to restart the session
- One-command cleanup ([`swamp kill`](#swamp-kill)) — tears down the Zellij
  session and the daemon

## Installation

swamp is distributed as a Nix flake. The recommended workflow is `nix build`
for local verification and `nix profile install` for system-wide use.

### Nix

```bash
nix profile install github:cwill747/swamp
```

Or, from a clone:

```bash
nix build
./result/bin/swamp --help
```

### Cargo

```bash
cargo install --path .
```

### Requirements

- [Zellij](https://zellij.dev) on `PATH`
- [lazygit](https://github.com/jesseduffield/lazygit) on `PATH`
- [starship](https://starship.rs) on `PATH` (used by the per-pane prompt)
- A `fish` shell (panes are launched as `fish -C ...`)
- [Claude Code](https://github.com/anthropics/claude-code) if you want agent
  status reporting

## Quick start

1. **Open a repo or bare clone**:

   ```bash
   cd ~/code/myproject
   swamp
   ```

   swamp lists the repo's worktrees, generates a Zellij layout, and attaches
   you to a new session named after the repo's directory. Each tab is a
   worktree; for bare clones, the first tab is a "dashboard" view (lazygit +
   status TUI) and the remaining tabs are the worktrees themselves.

2. **Spawn into an existing Zellij session**: if you're already inside Zellij,
   `swamp` adds a tab per worktree to the current session instead of starting
   a new one.

3. **Wire up Claude Code hooks** (optional but recommended): see
   [Claude Code hooks](#claude-code-hooks). Once configured, the swamp TUI in
   each tab shows live working / waiting / idle state for that worktree's
   Claude session.

4. **Tear down when done**:

   ```bash
   swamp kill
   ```

   Stops the daemon, kills the Zellij session, removes the socket and PID
   files.

## Commands

Run `swamp --help` for the canonical list.

### `swamp` / `swamp launch`

Launch (or attach to) a Zellij session for the repo. With no arguments, swamp
uses the current directory; pass a path to operate on a different repo or bare
clone.

```bash
swamp                  # current directory
swamp ~/code/myproject # specific repo
swamp launch           # explicit form
```

Behaviour:

- If a session named after the repo already exists, swamp attaches to it.
- If the running daemon's version differs from the binary's, swamp prompts to
  restart the session.
- Inside an existing Zellij session, swamp adds tabs to it instead of starting
  a new session.

### `swamp tui`

Run the long-running status TUI. This is what populates the swamp sidebar pane
in each tab, but you can also run it standalone.

```bash
swamp tui
```

Key bindings:

| Key       | Action                              |
| --------- | ----------------------------------- |
| `j` / `↓` | Move selection down                 |
| `k` / `↑` | Move selection up                   |
| `g`       | Jump to top                         |
| `G`       | Jump to bottom                      |
| `Enter`   | Switch to the selected worktree tab |
| `q`       | Quit                                |
| `Ctrl-C`  | Quit                                |

The TUI auto-starts the per-repo daemon if it isn't already running.

### `swamp serve`

Run the per-repo daemon explicitly. The daemon owns the worktree status
snapshot, watches `.git/` for changes, and accepts hook updates over a Unix
socket.

```bash
swamp serve              # detaches by default
swamp serve --foreground # stay attached, log to stderr
```

You usually don't need to call this directly — `swamp tui` and the per-pane
TUIs auto-spawn the daemon on demand.

### `swamp hook`

Record an agent status update for the current worktree. Designed to be called
from Claude Code's hook system; see [Claude Code hooks](#claude-code-hooks).

```bash
swamp hook working
swamp hook waiting
swamp hook idle
swamp hook working --dir /path/to/worktree
```

`--dir` overrides the inferred worktree. Without it, the worktree name is
derived from `$PWD`'s basename.

The hook prefers the daemon socket (200 ms timeout) and falls back to writing
`.swamp-status.json` in the git common dir if the daemon is unreachable.

### `swamp kill`

Tear down the Zellij session and the daemon for the current repo.

```bash
swamp kill
swamp kill ~/code/myproject
```

This kills the Zellij session, deletes the session entry, stops the daemon,
and removes the socket and PID files.

## Claude Code hooks

swamp tracks agent status per worktree via `swamp hook <status>`. To have
Claude Code report its status automatically, add the following to
`~/.claude/settings.json` (or a project-local `.claude/settings.json`):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "swamp hook working" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          { "type": "command", "command": "swamp hook working" }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "permission_prompt|elicitation_dialog",
        "hooks": [
          { "type": "command", "command": "swamp hook waiting" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "swamp hook idle" }
        ]
      }
    ]
  }
}
```

### Statuses

| Status    | When                                                      |
| --------- | --------------------------------------------------------- |
| `working` | Claude is actively processing a prompt or tool call       |
| `waiting` | Claude is blocked on user input or a permission prompt    |
| `idle`    | Claude finished its turn                                  |

The hook resolves the current worktree from `$PWD` (override with `--dir`) and
writes through the swamp daemon when running, falling back to
`.swamp-status.json` in the repo's git common dir.

## How it works

### Layout

swamp generates a Zellij KDL layout on the fly for each repo. For a bare clone,
the layout has:

- A `dashboard` tab with lazygit + swamp TUI + a shell, rooted in the first
  real worktree (the bare container itself isn't a valid git working tree, so
  lazygit would otherwise fail).
- One tab per worktree, each with lazygit, a swamp TUI sidebar, a Claude pane,
  and a shell.

For a non-bare repo with a single worktree, swamp uses the installed
`swamp` Zellij layout directly.

The Claude pane auto-detects a `flake.nix` / `shell.nix` / `default.nix` and
launches Claude inside `nix develop` if one is present.

### Daemon

Each repo gets a daemon at `$XDG_RUNTIME_DIR/swamp/<repo-id>.sock` (falls back
to `$TMPDIR/swamp/...`). The daemon:

- Loads `.swamp-status.json` from the git common dir on startup.
- Runs an initial git scan plus a 30-second heartbeat refresh.
- Watches the git common dir for changes via [`notify`](https://crates.io/crates/notify).
- Accepts `Subscribe`, `Hook`, `GetVersion`, and `Ping` messages over the
  socket; broadcasts `Snapshot` messages to all subscribers.
- Persists agent state back to `.swamp-status.json` after each hook.

The daemon is auto-started by `swamp tui` (and by `swamp` itself, indirectly,
via the per-pane TUIs). You can also start it explicitly with `swamp serve`.

### Managed configs

On first launch, swamp writes two configs under `$XDG_CONFIG_HOME/swamp/`
(default `~/.config/swamp/`):

- `starship.toml` — used by each pane's shell prompt
- `lazygit.yml` — used by every lazygit pane

These are rewritten only when the embedded contents differ from disk
(idempotent). The generated Zellij layout points to these files directly.

### State files

| Path                                 | Purpose                                |
| ------------------------------------ | -------------------------------------- |
| `<git-common-dir>/.swamp-status.json` | Per-worktree agent status, persisted   |
| `$XDG_RUNTIME_DIR/swamp/<id>.sock`    | Daemon Unix socket                     |
| `$XDG_RUNTIME_DIR/swamp/<id>.pid`    | Daemon PID file                        |
| `$XDG_CONFIG_HOME/swamp/`             | Managed starship + lazygit configs     |

## Building

This repo uses Nix for reproducible builds. **Always verify changes with
`nix build`**, not raw `cargo build`:

```bash
nix build
./result/bin/swamp --help
```

For an interactive dev shell with `cargo`, `rustc`, `clippy`, `rustfmt`, and
`rust-analyzer`:

```bash
nix develop
cargo test
```

## License

MIT
