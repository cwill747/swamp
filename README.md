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

#### Use the binary cache (skip rebuilding from source)

CI publishes prebuilt outputs to [FlakeHub Cache](https://flakehub.com/cache).
Without it configured, `nix profile install` only fetches the flake source and
rebuilds swamp locally. FlakeHub Cache is an authenticated substituter, so a
plain Nix install won't read from it until you log in once.

[Determinate Nix](https://docs.determinate.systems/) wires up the
`cache.flakehub.com` substituter, auth token, and trusted public keys
automatically:

```bash
# Install Determinate Nix (skip if already installed)
curl -fsSL https://install.determinate.systems/nix | sh -s -- install

# Authenticate to FlakeHub once
determinate-nixd login

nix profile install "https://flakehub.com/f/cwill747/swamp/*.tar.gz"
```

After logging in, the install substitutes the cached binary instead of
compiling.

From a clone:

```bash
nix build
./result/bin/swamp --help
```

For a faster unoptimized local build:

```bash
nix build .#dev
```

For an optimized build matching release profile settings:

```bash
nix build .#release
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

- [Zellij](https://zellij.dev) 0.45 or later on `PATH`
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
worktree's own tab on demand (`Enter` or single-click), and swamp switches to it
if it is already open. The tab count is no longer tied to the worktree count,
and tabs you open stay part of the Zellij session across detach/reattach.

If you run swamp inside an existing Zellij session, swamp opens the repo session
as a nested session in the current pane. Zellij then offers its native nested
session controls for zooming into the repo or controlling it when the pane has
focus. If you run swamp from the repo session itself, swamp leaves that session
active instead of attaching it recursively.

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

To send Claude Code worktrees (`claude --worktree` and subagents with
`isolation: worktree`) through the slot pool, add `--claude-worktrees`:

```bash
swamp init --claude-worktrees
```

This installs `WorktreeCreate` and `WorktreeRemove` hooks that call
`swamp slot alloc --hook-json` and `swamp slot release --hook-json`. The hooks
apply to every repository on the machine. If you already have a worktree hook
that swamp does not manage, `init` leaves it in place and prints a warning.
Claude Code does not run `WorktreeRemove` when a subagent finishes, so a slot
that a subagent took stays busy until you release it.

### `swamp tui`

Runs the workspace with Spaces above Agents, a searchable cross-repository
inventory, and selected-destination details. Generated Zellij tabs keep the
navigation rail beside real terminals; the agent pane stays suspended until
you start it. It can also be started directly:

```bash
swamp tui
swamp tui --view sidebar
swamp tui --view details
swamp tui --view all        # original four-panel view
```

Explicit `worktrees`, `ai-status`, `resources`, and `pr-status` views retain their
panel behavior. Outside Zellij, workspace displays paths and status; opening a
terminal is disabled. Discovery never starts stopped repositories. Enter on a
stopped repository explicitly starts it; cross-repository navigation switches
flat sessions. Selecting a subagent opens its parent's agent pane.

Useful keys:

| Key       | Action                              |
| --------- | ----------------------------------- |
| `j` / `↓` | Move selection down                 |
| `k` / `↑` | Move selection up                   |
| `Tab`     | Switch Spaces / Agents              |
| `←` / `→` | Collapse / expand a repository      |
| `Enter`   | Open selected agent (single-click also opens) |
| `/` / `g` | Go To: search titles, repo, branch, path, harness, subagents |
| `f`       | Filters: `a` All, `b` Waiting, `w` Working, `d` Done, `i` Idle |
| `[` / `]` | Visit previous / next attention destination |
| `v`       | Mark the selected completion seen   |
| `l`       | Details drill-in on narrow screens  |
| `Esc`     | Cancel overlay / clear search       |
| `?`       | Contextual help                     |
| `m`       | More actions                        |
| `c`       | Create a worktree                   |
| `d`       | Delete the selected worktree        |
| `D`       | Delete the worktree this pane lives in and close its tab |
| `t`       | Set the title of the selected branch |
| `h`       | Choose Claude or Codex harness      |
| `r`       | Refresh status                      |
| `u`       | Update branches                     |
| `O`       | Open the cross-repo overview        |
| `K`       | Confirm stopping the repo session and agents |
| `q`       | Exit standalone / return to terminal from a navigation pane |

In search, printable keys always edit text; Tab switches to the filter controls.
Keyboard movement and wheel scrolling only browse. Chevron clicks only expand
or collapse. Mutations support the current repository; foreign rows offer Open
Repo instead of applying an action to the pane's working directory.

Waiting (`!`), Working (`~`), unseen Done (`+`), Idle (`-`), and Unknown (`?`)
remain distinguishable with `SWAMP_ASCII=1` and without color. Done requires a
confirmed observed completion; older daemons keep their original status.
The selection caret (`>`) is separate from the active location (`@`) and trunk
marker (`*`); active rows keep their state symbol.
An explicit successful agent-pane visit or `v` acknowledges that exact completion
across local views. Native Zellij tab navigation and tab-only partial jumps do
not acknowledge Done. A later completion stays unseen.

Use your native Zellij pane focus, tab, resize, fullscreen, and detach controls;
Swamp does not change the global prefix. Ordinary keys in the shell or agent
go straight to that application. Fullscreen reclaims terminal space when the
sidebar is too narrow. Closing a sidebar or detaching leaves agents running.

The generated Zellij 0.45 dashboard composition is captured at
[`tests/fixtures/workspace/zellij-0.45-captures/`](tests/fixtures/workspace/zellij-0.45-captures/),
with pane dumps and 120x36 / 160x48 terminal transcripts.

A deletion that git refuses without `--force` (uncommitted changes, unpushed
commits, ...) opens a floating Zellij pane showing the reason plus the
worktree's status and diffstat, so you can review what's at risk before
forcing it through. Outside Zellij, or if the pane can't be opened, swamp
falls back to a one-line force-confirmation prompt instead.

Each row shows what the worktree is doing, not its directory name. The title
comes from the first of these that exists: your title (`t` or `swamp title`),
the PR title, the Claude session name, the openspec change that the branch
adds, the last commit subject, and the branch name. Slot rows also show the
slot name (`work1`, ...), and free slots appear dimmed at the end of the list.

### `swamp slot`

Manages a pool of reusable worktrees named `work1`, `work2`, and so on. A slot
keeps its directory between branches, so build caches (`target/`, bazel output
bases) stay warm and disk use stays bounded.

```bash
cd "$(swamp slot alloc parser-fix)"   # check a branch out into a free slot
swamp slot release                    # return the current slot to the pool
swamp slot list                       # show slots and overflow worktrees
swamp slot pin work3 review 172       # hold a free slot
swamp slot unpin work3
```

- `alloc` checks out an existing local or remote branch, or creates the branch
  from the default remote's default branch. It prints only the path on stdout.
  If a worktree already has the branch, it prints that path. When every slot is
  busy, it creates a dedicated branch-named worktree ("overflow"), which stays
  until you remove it.
- `release` detaches the slot at the branch tip. It refuses when the slot has
  uncommitted changes, or when the branch has commits that are not on the remote
  and its PR is not merged or closed. When the PR is merged or closed, it also
  deletes the local branch. Use `--force` to skip the checks.
- A slot is free when its `HEAD` is detached, its tree is clean, and no pin
  holds it. Git state is the only record of who owns a slot.
- In a running swamp session, `alloc` opens a tab for the slot without taking
  focus, and `release` closes it. Pass `--no-tab` to skip that.
- A release refuses to take a slot from another slot's working agent.
- The Claude session, session name, and harness follow the branch: release
  saves them, and a later `alloc` of the branch restores them in whichever slot
  it gets.

### `swamp title`

Sets the dashboard title of the branch in the current worktree. The title
follows the branch across slots.

```bash
swamp title "Bump zellij to 0.46"
swamp title --clear
```

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

When Claude Code runs `swamp hook`, it writes the hook payload as JSON to stdin.
Swamp reads the payload to track subagents. The AI status pane shows each
subagent as an indented row under its worktree. Tool calls inside a subagent
update only that subagent, so the parent row keeps its own status.

#### Claude Code hooks

`swamp init` adds these hooks to `~/.claude/settings.json` and keeps hooks that
swamp does not manage. If the file is read-only, for example under nix or
home-manager, add the hooks yourself:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "swamp hook working >/dev/null 2>&1 || true" }] }
    ],
    "PreToolUse": [
      { "matcher": "", "hooks": [{ "type": "command", "command": "swamp hook working >/dev/null 2>&1 || true" }] }
    ],
    "PostToolUse": [
      { "hooks": [{ "type": "command", "command": "swamp hook working >/dev/null 2>&1 || true" }] }
    ],
    "Notification": [
      { "matcher": "permission_prompt|elicitation_dialog", "hooks": [{ "type": "command", "command": "swamp hook waiting >/dev/null 2>&1 || true" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "swamp hook idle >/dev/null 2>&1 || true" }] }
    ],
    "SubagentStart": [
      { "hooks": [{ "type": "command", "command": "swamp hook working >/dev/null 2>&1 || true" }] }
    ],
    "SubagentStop": [
      { "hooks": [{ "type": "command", "command": "swamp hook idle >/dev/null 2>&1 || true" }] }
    ]
  }
}
```

### `swamp serve`

Runs the per-repo daemon explicitly:

```bash
swamp serve
swamp serve --foreground
```

Most users do not need this command; `swamp` and legacy single-repo TUI views
start the daemon on demand. Workspace views discover existing daemons through
the shared broker.

### `swamp kill`

Stops the daemon, kills the Zellij session, and removes runtime socket and PID
files:

```bash
swamp kill
swamp kill ~/code/myproject
```

### `swamp clone`

Clones a repository into swamp's bare layout and prints the path of the default
branch worktree:

```bash
swamp clone git@github.com:org/app.git
swamp clone https://github.com/org/app.git ~/code/app
cd "$(swamp clone git@github.com:org/app.git)"
```

The layout keeps the git data in `.bare` and puts each worktree in a sibling
directory:

```text
app/
├── .bare/   git data (bare repository)
├── .git     points to .bare
└── main/    default branch worktree
```

swamp creates and removes worktrees in this layout itself. You do not need
another worktree tool.

### `swamp overview`

Shows one list of the worktrees that need you, across every running repo
session:

```bash
swamp overview
```

The overview runs in its own Zellij session, `swamp-overview`. Outside Zellij,
the command attaches to that session. Inside Zellij, it switches your client to
that session. swamp creates the session when it does not exist. From a repo
dashboard, press `O` to do the same.

A worktree is listed one time, for the first reason that applies:

1. The agent waits for you.
2. The pull request checks failed.
3. The pull request review requests changes.
4. The worktree has a merge conflict or a rebase in progress.
5. The agent is idle and the worktree has uncommitted changes. The default
   branch is not listed for this reason.

Registered repos with no running session are listed under **Not running**.

| Key           | Action                                                     |
| ------------- | ---------------------------------------------------------- |
| `j` / `↓`     | Move selection down                                        |
| `k` / `↑`     | Move selection up                                          |
| `g` / `G`     | Select the first or the last row                           |
| `Enter`       | Go to the session and tab of the selected worktree, or launch the selected repo |
| `U`           | Update the default branch of each running repo             |
| `r`           | Reload the registry and refresh the status of each repo    |
| `q`           | Quit. This ends the `swamp-overview` session only          |

`U` does not start a repo that is not running. The footer shows how many repos
were updated and names each repo that failed.

`Enter` moves your Zellij client to the repo session. It does not nest that
session in the overview. If the worktree has no open tab, swamp opens one.

To run the overview in the current terminal, without the dedicated session, use
`swamp overview --no-session`.

A session that runs an older swamp version shows its worktrees in the overview.
To let the overview launch a repo that is not running, register the repo with
`swamp register`.

### `swamp register`

Records a repo in swamp's registry without a launch. `swamp launch` records the
repo automatically, so you need this command only for a repo that swamp has not
launched since the registry was added:

```bash
swamp register
swamp register ~/code/myproject
swamp register --all-live
```

`--all-live` records every repo that has a running daemon, including a daemon
from an older swamp version. The command does not start, stop, or restart a
daemon or a session.

### `swamp forget`

Removes a repo from the registry. The repo's daemon and Zellij session keep
running:

```bash
swamp forget
swamp forget ~/code/myproject
```

`swamp kill` stops a session but keeps the repo registered.

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
layout = "workspace"     # or "classic"
sidebar_width = 32        # cells; 16–60
# These percentages apply only to classic:
worktrees_column = 33
ai_column        = 34
shell_column     = 33

[harness]
# "claude", "codex", or "choose"
default = "claude"

[badges]
enabled = true
```

Set `[harness] default = "choose"` to pick Claude or Codex per worktree from the
TUI with `h`.

Workspace is the default for newly generated layouts, even when an existing
config contains only classic percentages. Existing live sessions attach with
their current layout and processes; changing presentation does not restart
agents. Newly opened tabs can coexist with older tabs.

To deliberately recreate a session, finish or preserve its terminal work, run
`swamp kill`, then `swamp launch`. To roll back the presentation, first set
`[dashboard] layout = "classic"`, then recreate it the same way. This is an
explicit session stop; attaching after an upgrade never performs a cosmetic
migration. Additive completion metadata is readable by older versions, though
an older version rewriting status may discard it.

Rendered workspace fixtures are in [tests/fixtures/workspace](tests/fixtures/workspace).
Real Zellij 0.45.0 captures show the generated dashboard at
[120×36](tests/fixtures/workspace/zellij-0.45-composed-120x36.txt) and
[160×48](tests/fixtures/workspace/zellij-0.45-composed-160x48.txt); pane records
and ANSI source captures are in
[zellij-0.45-captures](tests/fixtures/workspace/zellij-0.45-captures).

### Remote SSH hosts

Install a version of Swamp with SSH discovery on each host and add its
OpenSSH alias to your local Swamp config:

```toml
[ssh_hosts.dev]
target = "dev-box"  # an SSH config alias or user@host
# swamp_path = "/absolute/path/to/swamp"  # optional for noninteractive SSH
```

The workspace dashboard reloads this list automatically. Spaces groups
repositories by host and shows SSH connection status, including hosts with
no repositories. Agents includes reported agents from connected remote hosts.
Swamp discovers running Swamp daemons and repositories added on that host
with `swamp register`. An SSH disconnect marks cached rows stale and Swamp
retries automatically.

Use an SSH account that can connect without prompting. Swamp uses your usual
OpenSSH keys, agent, host keys, and jump hosts. Run `ssh dev-box swamp --version`
to check the remote binary. The SSH login needs the same Swamp runtime and
state directories as the remote sessions. If you upgraded with an old
`swamp aggregate` process still running, restart that broker process and
reopen the dashboard to pick up SSH support.

Inside local Zellij, press Enter on a remote repository or one of its rows
to open that repository's remote Swamp session in an SSH pane. From there
you can use the remote dashboard to select a worktree or agent and run
repository actions. Outside local Zellij, the workspace shows remote status
and paths.

### Attention badges

swamp marks Zellij tab names, so that you can see a waiting agent from any tab:

- A worktree tab is named `● <worktree>` while its agent waits for you.
- The dashboard tab is named `dashboard ●<n>` while `<n>` worktrees in other
  repos wait for you.

The daemon removes a badge when the agent works again. A tab that opens while
its agent already waits gets its badge within 30 seconds. With `SWAMP_ASCII`
set, the badge glyph is `*`.

The cross-repo count comes from a background process, `swamp aggregate`, that
`swamp launch` and the daemon start. One instance serves all repos, and it
exits when no repo session has run for one minute.

To keep tab names unchanged, set `[badges] enabled = false` and restart the
session.

### System notifications

Swamp sends desktop alerts when a parent agent starts waiting for input or
finishes a turn. It uses `terminal-notifier` on macOS and `notify-send` (libnotify)
on Linux, matching Herdr's system notification tools. Install the appropriate
tool through your system configuration and make it available on Swamp's PATH.
Missing tools, unavailable desktop services, and notification failures do not
interrupt agents. Delivery has a five-second time limit.

```toml
[notifications]
enabled = true
waiting = true
finished = true
# Optional executable path, using the platform's notifier argument format:
# command = "/path/to/terminal-notifier"
```

Set `enabled = false` to disable all alerts, or disable either event separately.
These settings are read for each alert. Alerts include the repository and
worktree name; they do not include conversation content. Refreshes, startup,
subagent hooks, and repeated status updates do not generate alerts. Codex turn
IDs deduplicate repeat completion events; older payloads without IDs alert on
each completion invocation. Existing daemons must be restarted to use the new
notification handling. Notifications have no click action.

The slot pool has its own section:

```toml
[slots]
pool_size = 5          # most slots swamp creates; 0 turns the pool off
autoclean = true       # clean up merged branches automatically; false disables

[[slots.hooks.pre-release]]
run = "bazel shutdown"

[[slots.hooks.post-alloc]]
run = "direnv allow"
timeout_secs = 60
```

Hooks run with `sh -c` in the worktree and get `SWAMP_WORKTREE`, `SWAMP_NAME`,
`SWAMP_KIND`, `SWAMP_BRANCH`, and `SWAMP_REPO_ROOT`. A failed `pre-release` hook
stops the release unless you pass `--force`; a failed `post-alloc` hook only
prints a warning. Each hook has a 300-second limit by default. Pass `--no-hooks`
to skip them. Captured output shares a 256 KiB budget across the hook chain;
excess output is drained and discarded, with an explicit truncation notice.

Release rechecks git safety after the hooks run. If a hook creates unpushed
commits or leaves the tree dirty, a normal release refuses and keeps those
changes in place. `--force` can override those checks, but never a hook that
switches or detaches the branch. A hook's new commits are not covered by stale
merged/closed PR facts and do not trigger deletion of their branch.

A repository can set its own `pool_size`, override `autoclean`, and add hooks in
`<git-common-dir>/swamp/config.toml` (for the bare layout,
`.bare/swamp/config.toml`). That file is not part of the repository, so a clone
never runs commands that someone else chose. User hooks run first.

Autoclean runs in the daemon on Git refreshes and the 30-second heartbeat.
It checks the **remote-tracking default branch**, not an unpushed local default
branch or the feature branch's own tracking upstream. Fetching updates this
evidence; autoclean itself never fetches. Merges to other destinations and
squash/rebase merges require a `MERGED` PR with an exact matching head commit.
PR polling continues without a dashboard subscriber while autoclean is enabled.

New branches that have not acquired commits are retained. Git-only cleanup
requires a previously observed unmerged tip (saved in branch memory across
restarts) or a branch creation reflog showing subsequent commits. If that
history is unavailable, cleanup waits for matching PR evidence. Closed,
unmerged PRs never trigger autoclean. Explicitly allocating an already merged
branch retains its current tip across refreshes and daemon restarts, giving the
caller time to open and use it. New commits can become eligible again; use an
explicit release to reclaim it without making new commits.

Working/waiting agents, running subagents, pinned or Git-locked worktrees,
dirty trees, and merge/rebase operations defer cleanup. Pre-release hooks run
with the usual failure checks, followed by another branch, commit, status, and
agent check. Swamp closes the idle agent's Zellij tab synchronously under the
slot lock before releasing the worktree. Failed tab closure leaves the worktree
available for retry; no detached closer can affect a reused slot. Pool slots
are detached for reuse; overflow worktrees are removed. Completed session
memory is cleared, while local branches without merged-PR evidence are kept.

## Files

| Path                                  | Purpose                              |
| ------------------------------------- | ------------------------------------ |
| `$XDG_CONFIG_HOME/swamp/config.toml`  | User settings                        |
| `$XDG_CONFIG_HOME/swamp/lazygit.yml`  | Managed lazygit config               |
| `<git-common-dir>/.swamp-status.json` | Per-worktree agent status            |
| `$XDG_STATE_HOME/swamp/repos.json`    | Registry of repos swamp has launched |
| `$XDG_STATE_HOME/swamp/seen.json`     | Shared exact-completion acknowledgements |
| `$XDG_RUNTIME_DIR/swamp/workspace.sock` | Versioned singleton workspace broker |
| `$XDG_RUNTIME_DIR/swamp/ui/<session>.json` | Shared sidebar/details selection |
| `<git-common-dir>/swamp/config.toml`  | Per-repository slot settings         |
| `<git-common-dir>/swamp/branches.json` | Per-branch sessions and titles      |
| `<git-common-dir>/swamp/pins/`        | Slot pins                            |
| `<git-common-dir>/swamp/slots.lock`   | Lock for slot operations             |
| `$XDG_RUNTIME_DIR/swamp/<id>.sock`    | Daemon Unix socket                   |
| `$XDG_RUNTIME_DIR/swamp/<id>.pid`     | Daemon PID file                      |
| `$XDG_RUNTIME_DIR/swamp/workspace.lock` | Single-instance workspace broker lock |
| `$XDG_RUNTIME_DIR/swamp/aggregator.lock` | Attention-count owner lock; retained for upgrade handoff |

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md).

## License

MIT
