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
  <a href="#configuration">Config</a> ·
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
`swamp`. It writes its own lazygit config to `~/.config/swamp/`, generates a
Zellij layout on the fly, and attaches.

**Single binary, single daemon per repo.** A small Rust binary plus a per-repo
Unix-socket daemon — no plugins, no shell hooks beyond Claude's.

## Features

- One Zellij tab per git worktree, with a layout containing lazygit, Claude, a
  shell, and a swamp status sidebar
- Live status TUI ([`swamp tui`](#swamp-tui)) listing every worktree with
  branch, ahead/behind, dirty state, agent status, and conversation title
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

#### Binary cache

Prebuilt binaries for every commit on `main` and every release are pushed to the
[`cwill747-swamp`](https://app.cachix.org/cache/cwill747-swamp) Cachix cache, so
you can install without compiling from source.

The flake advertises the cache via `nixConfig`, but Nix **ignores** that unless
you are a trusted user. Either pass the flag per-invocation:

```bash
nix build github:cwill747/swamp --accept-flake-config
```

…or, to use the cache permanently, add it to your `nix.conf`
(`~/.config/nix/nix.conf` or `/etc/nix/nix.conf`):

```
extra-substituters = https://cwill747-swamp.cachix.org
extra-trusted-public-keys = cwill747-swamp.cachix.org-1:Oa1mwV26phjG8DrTS4nMuUhfq6VfCFE66ROte3qSSWU=
```

Or, with `cachix` installed: `cachix use cwill747-swamp`.

> If you still see source builds, you're likely evaluating a dirty tree or a
> commit that CI hasn't cached yet — only clean, pushed revisions have prebuilt
> binaries.

### Cargo

```bash
cargo install --path .
```

### Requirements

- [Zellij](https://zellij.dev) on `PATH`
- [lazygit](https://github.com/jesseduffield/lazygit) on `PATH`
- A login shell (`$SHELL`, falling back to bash); fish and POSIX shells are
  both supported — panes launch your normal shell with your own config
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

### `swamp init`

One-shot setup. Writes swamp's TOML config (if absent), refreshes the embedded
managed configs (lazygit), installs or updates swamp's
[Claude Code hooks](#claude-code-hooks) in your user `settings.json`, and wires
[Codex notify](#codex-notify) into your Codex `config.toml`.

```bash
swamp init
```

Behaviour:

- Writes `$XDG_CONFIG_HOME/swamp/config.toml` from a commented default. An
  existing config is left untouched — it's yours to edit.
- Merges swamp's hooks into `~/.claude/settings.json` (honors
  `CLAUDE_CONFIG_DIR`), preserving any unrelated hooks you've configured. An
  existing swamp hook is updated in place rather than duplicated.
- If `settings.json` is read-only (common under nix / home-manager, where it's
  a symlink into the store), swamp won't modify it. Instead it tells you the
  file is read-only and, if your hooks are missing or out of date, warns so you
  can update the source manually.
- Sets `notify = ["swamp", "codex-notify"]` in Codex's `config.toml` (honors
  `CODEX_HOME`), preserving your other Codex settings. A read-only config is
  left untouched with the line to add printed out.

Re-running is safe and idempotent. See [Configuration](#configuration) for the
config file.

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
swamp hook working --session-name "Fix auth bug"
swamp hook working --session-id "3f9c1e2a-7b40-4d8e-9a1f-2c3d4e5f6a7b"
```

`--dir` overrides the inferred worktree. Without it, the worktree name is
derived from `$PWD`'s basename.

`--session-name` sets the Claude Code conversation title for display in the AI
status panel. When omitted, the previously recorded session name (if any) is
preserved.

`--session-id` records the Claude Code session UUID for the worktree. It is
persisted to `.swamp-status.json` so that a later `swamp` launch can resume the
session in that worktree's Claude pane (`claude --resume <id>`). When omitted,
the previously recorded id (if any) is preserved.

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

## Configuration

swamp reads optional settings from `$XDG_CONFIG_HOME/swamp/config.toml`
(usually `~/.config/swamp/config.toml`). `swamp init` writes a commented
default; a missing file or any unset value falls back to the built-in defaults,
so the config is entirely optional.

```toml
[dashboard]
# The dashboard is three side-by-side columns. These set each column's width as
# a percentage of the terminal; they should sum to roughly 100.
worktrees_column = 33   # left column: worktrees + resources panes
ai_column        = 34   # middle column: ai-status + pr-status panes
shell_column     = 33   # right column: an interactive shell

[harness]
# Which AI coding agent runs in each worktree's agent pane:
#   "claude" — always Claude Code
#   "codex"  — always Codex
#   "choose" — pick per-worktree; press `h` in the worktrees pane to choose
default = "claude"
```

A malformed config doesn't block a launch — swamp warns and uses defaults.

### Swapping the agent harness (Claude Code ↔ Codex)

The `[harness] default` setting is tri-state. `claude` or `codex` pin every
worktree's agent pane to that agent. `choose` lets you pick per-worktree:
highlight a worktree in the worktrees pane and press **`h`**, then `c` (Claude)
or `x` (Codex). The choice is persisted in `.swamp-status.json` and applied
**live** — swamp closes and reopens that worktree's tab so its agent pane comes
back up as the chosen harness (it also still applies on the next launch). A small
`C`/`X` indicator in the worktrees table shows the recorded override.

The agent pane runs the harness as a child of an interactive shell rather than
replacing it, so when you quit the harness you land at a shell prompt in that
pane (inside the nix dev shell, if any) — handy for relaunching it or running the
other agent by hand.

Codex reports agent status through its [`notify`](#codex-notify) hook, which
[`swamp init`](#swamp-init) wires up. Because Codex only emits an
`agent-turn-complete` event (it has no "turn started" signal), a Codex pane is
reported **idle** when a turn finishes but never shows a live "working" state,
and Codex panes don't resume sessions. Claude panes keep full
working/waiting/idle status plus `--resume`.

## Claude Code hooks

swamp tracks agent status per worktree via `swamp hook <status>`. The quickest
way to wire this up is [`swamp init`](#swamp-init), which installs (or updates)
the recommended hooks in your `~/.claude/settings.json` automatically. To set
them up by hand instead, add the following to `~/.claude/settings.json` (or a
project-local `.claude/settings.json`).

The hooks parse Claude Code's JSON stdin to extract the conversation title
(`session_name`) and pass it to `swamp hook --session-name`, so the AI status
panel can show what each agent is working on. This requires `jq` on `PATH`.

### Basic (status only)

If you don't need session names in the dashboard, the minimal config is:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "swamp hook working >/dev/null 2>&1 || true" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          { "type": "command", "command": "swamp hook working >/dev/null 2>&1 || true" }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "permission_prompt|elicitation_dialog",
        "hooks": [
          { "type": "command", "command": "swamp hook waiting >/dev/null 2>&1 || true" }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "swamp hook idle >/dev/null 2>&1 || true" }
        ]
      }
    ]
  }
}
```

### Recommended (status + session name + resume)

This version extracts the Claude conversation title (`session_name`) and the
session id (`session_id`) from the hook's JSON stdin. The title shows in the AI
status panel; the id is recorded so that if you `swamp kill` and relaunch, each
still-existing worktree's Claude pane resumes its session with
`claude --resume <id>` instead of starting fresh.

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "input=$(cat); swamp hook working --session-name \"$(echo \"$input\" | jq -r '.session_name // empty')\" --session-id \"$(echo \"$input\" | jq -r '.session_id // empty')\" >/dev/null 2>&1 || true"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "input=$(cat); swamp hook working --session-name \"$(echo \"$input\" | jq -r '.session_name // empty')\" --session-id \"$(echo \"$input\" | jq -r '.session_id // empty')\" >/dev/null 2>&1 || true"
          }
        ],
        "matcher": ""
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "input=$(cat); swamp hook working --session-name \"$(echo \"$input\" | jq -r '.session_name // empty')\" --session-id \"$(echo \"$input\" | jq -r '.session_id // empty')\" >/dev/null 2>&1 || true"
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "permission_prompt|elicitation_dialog",
        "hooks": [
          {
            "type": "command",
            "command": "input=$(cat); swamp hook waiting --session-name \"$(echo \"$input\" | jq -r '.session_name // empty')\" --session-id \"$(echo \"$input\" | jq -r '.session_id // empty')\" >/dev/null 2>&1 || true"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "input=$(cat); swamp hook idle --session-name \"$(echo \"$input\" | jq -r '.session_name // empty')\" --session-id \"$(echo \"$input\" | jq -r '.session_id // empty')\" >/dev/null 2>&1 || true"
          }
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

## Codex notify

For Codex panes (see [Swapping the agent
harness](#swapping-the-agent-harness-claude-code--codex)), swamp reports status
through Codex's `notify` program. [`swamp init`](#swamp-init) sets this for you
in Codex's `config.toml` (honors `$CODEX_HOME`, default `~/.codex`), preserving
your other settings:

```toml
notify = ["swamp", "codex-notify"]
```

Codex invokes the program with a single JSON payload argument on each
`agent-turn-complete` event; `swamp codex-notify` parses it, resolves the
worktree from the payload's `cwd`, and records an `idle` status. Codex emits no
"turn started" event, so a Codex pane never shows a live `working` state. If your
Codex config is read-only (common under nix/home-manager), swamp won't modify it
and will print the line to add manually.

## How it works

### Layout

swamp generates a Zellij KDL layout on the fly for each repo. For a bare clone,
the layout has:

- A `dashboard` tab with lazygit + swamp TUI + a shell, rooted in the first
  real worktree (the bare container itself isn't a valid git working tree, so
  lazygit would otherwise fail).
- One tab per worktree, each with lazygit, a swamp TUI sidebar, an agent pane
  (Claude Code or Codex — see [Swapping the agent
  harness](#swapping-the-agent-harness-claude-code--codex)), and a shell.

A non-bare repo with a single worktree gets the same generated single-tab
worktree layout.

The Claude pane auto-detects a `flake.nix` / `shell.nix` / `default.nix` and
launches Claude inside `nix develop` if one is present.

If the worktree has a Claude session id recorded in `.swamp-status.json` (from
the [hooks](#claude-code-hooks)), the pane launches `claude --resume <id>` so
the conversation picks up where it left off after a `swamp kill` + relaunch. A
worktree with no recorded session — or one that's since been removed — just gets
a fresh `claude`.

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

On first launch, swamp writes its configs under `$XDG_CONFIG_HOME/swamp/`
(default `~/.config/swamp/`):

- `lazygit.yml` — used by every lazygit pane (managed: rewritten only when the
  embedded contents differ from disk, idempotent)
- `config.toml` — your settings (see [Configuration](#configuration)); written
  by `swamp init` and never overwritten once it exists

The generated Zellij layout points to the lazygit file directly and reads the
dashboard layout percentages from `config.toml`.

Shell panes launch your login shell (`$SHELL`) directly, so your own prompt and
shell config apply — swamp injects no prompt of its own.

### State files

| Path                                 | Purpose                                |
| ------------------------------------ | -------------------------------------- |
| `<git-common-dir>/.swamp-status.json` | Per-worktree agent status, persisted   |
| `$XDG_RUNTIME_DIR/swamp/<id>.sock`    | Daemon Unix socket                     |
| `$XDG_RUNTIME_DIR/swamp/<id>.pid`    | Daemon PID file                        |
| `$XDG_CONFIG_HOME/swamp/`             | Managed lazygit config + `config.toml` |

## Building

This repo uses Nix for reproducible builds. **Always verify changes with
`nix build`**, not raw `cargo build`:

```bash
nix build path:.
./result/bin/swamp --help
```

> **Why `path:.`?** In a bare-repo worktree layout, each worktree's `.git` is a
> relative file, not a directory. Nix can't resolve the flake through it, so
> `path:.` tells Nix to treat the current directory as the flake root.

For an interactive dev shell with `cargo`, `rustc`, `clippy`, `rustfmt`, and
`rust-analyzer`:

```bash
nix develop
cargo test
```

## License

MIT
