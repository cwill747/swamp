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
  <a href="#configuration">Config</a>
</p>

---

swamp turns a git repo into a [Zellij](https://zellij.dev) session: a dashboard
tab listing every worktree, a live status TUI, and per-worktree agent status
reporting. It is built for parallel AI-agent workflows — open a worktree's tab
on demand from the dashboard and it comes up with
[lazygit](https://github.com/jesseduffield/lazygit), an agent pane, a shell, and
a status sidebar.

## Installation

### Nix

```bash
nix profile install "https://flakehub.com/f/cwill747/swamp/*.tar.gz"
```

From a clone:

```bash
nix build path:.
./result/bin/swamp --help
```

For a faster unoptimized local build:

```bash
nix build path:.#dev
```

For an optimized build matching release profile settings:

```bash
nix build path:.#release
```

### Static binaries

Static binaries are published on the
[GitHub Releases](https://github.com/cwill747/swamp/releases) page for Linux and
macOS targets. Download the asset for your platform, make it executable, and put
it somewhere on `PATH`:

```bash
chmod +x swamp-x86_64-linux
sudo install -m 0755 swamp-x86_64-linux /usr/local/bin/swamp
```

### Cargo

```bash
cargo install --path .
```

### Requirements

- [Zellij](https://zellij.dev) on `PATH`
- [lazygit](https://github.com/jesseduffield/lazygit) on `PATH`
- A login shell (`$SHELL`, falling back to bash)
- [Claude Code](https://github.com/anthropics/claude-code) or Codex if you want
  agent panes and status reporting

## Quick start

```bash
cd ~/code/myproject
swamp
```

swamp lists the repo's worktrees, generates a Zellij layout, and attaches to a
session named after the repo. The session name is `{repo-basename}-{4-hex}`
(e.g. `myrepo-3b8b`) so two repos with the same directory name but different
paths get distinct sessions. A new session opens to a single dashboard tab — for
bare clones and normal repos alike. The dashboard lists every worktree; open a
worktree's own tab on demand (`Enter` or double-click), and swamp switches to it
if it is already open. The tab count is no longer tied to the worktree count,
and tabs you open stay part of the Zellij session across detach/reattach.

If you are already inside Zellij, `swamp` adds tabs to the current session
instead of starting a new one.

Run the one-shot setup command to write the default config and wire agent status
hooks:

```bash
swamp init
```

When you are done with a repo session:

```bash
swamp kill
```

## Commands

Run `swamp --help` for the canonical list.

### `swamp` / `swamp launch`

Launch or attach to a Zellij session for a repo:

```bash
swamp
swamp launch
swamp launch ~/code/myproject
```

If the session already exists, swamp attaches to it. If the daemon version does
not match the current binary, swamp prompts to restart the session.

### `swamp init`

Writes `$XDG_CONFIG_HOME/swamp/config.toml`, refreshes managed configs, installs
or updates Claude Code hooks, and sets Codex `notify` to `["swamp",
"codex-notify"]`.

### `swamp tui`

Runs the status TUI. This is normally embedded in each Zellij tab, but it can be
started directly:

```bash
swamp tui
```

Useful keys:

| Key       | Action                              |
| --------- | ----------------------------------- |
| `j` / `↓` | Move selection down                 |
| `k` / `↑` | Move selection up                   |
| `Enter`   | Open or switch to the selected worktree tab |
| `c`       | Create a worktree                   |
| `d`       | Delete a worktree                   |
| `h`       | Choose Claude or Codex harness      |
| `r`       | Refresh status                      |
| `u`       | Update branches                     |
| `K`       | Kill the swamp session              |
| `q`       | Quit                                |

### `swamp hook`

Records an agent status update for the current worktree. `swamp init` installs
the recommended Claude Code hooks automatically.

```bash
swamp hook working
swamp hook waiting
swamp hook idle
swamp hook working --dir /path/to/worktree
swamp hook working --session-name "Fix auth bug"
swamp hook working --session-id "3f9c1e2a-7b40-4d8e-9a1f-2c3d4e5f6a7b"
```

### `swamp serve`

Runs the per-repo daemon explicitly:

```bash
swamp serve
swamp serve --foreground
```

Most users do not need this command; `swamp` and `swamp tui` start the daemon on
demand.

### `swamp kill`

Stops the daemon, kills the Zellij session, and removes runtime socket and PID
files:

```bash
swamp kill
swamp kill ~/code/myproject
```

### `swamp completions`

Prints shell completions:

```bash
swamp completions bash
swamp completions fish
swamp completions zsh
```

Nix installs bash, fish, and zsh completions automatically.

## Configuration

swamp reads optional settings from `$XDG_CONFIG_HOME/swamp/config.toml`
(usually `~/.config/swamp/config.toml`). `swamp init` writes a commented
default; missing values fall back to built-in defaults.

```toml
[dashboard]
worktrees_column = 33
ai_column        = 34
shell_column     = 33

[harness]
# "claude", "codex", or "choose"
default = "claude"
```

Set `[harness] default = "choose"` to pick Claude or Codex per worktree from the
TUI with `h`.

## Files

| Path                                  | Purpose                              |
| ------------------------------------- | ------------------------------------ |
| `$XDG_CONFIG_HOME/swamp/config.toml`  | User settings                        |
| `$XDG_CONFIG_HOME/swamp/lazygit.yml`  | Managed lazygit config               |
| `<git-common-dir>/.swamp-status.json` | Per-worktree agent status            |
| `$XDG_RUNTIME_DIR/swamp/<id>.sock`    | Daemon Unix socket                   |
| `$XDG_RUNTIME_DIR/swamp/<id>.pid`     | Daemon PID file                      |

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md).

## License

MIT
