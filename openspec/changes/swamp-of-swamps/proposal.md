## Why

A user who runs many repo sessions must open each session to learn which
worktrees need attention. Each per-repo daemon already publishes agent, git, and
PR status on its socket, but no client reads all daemons at once. This change
adds a cross-repo view and cross-repo signals on top of the existing daemons.

## What Changes

- Add `swamp overview`: a TUI that lists only the worktrees that need the user,
  across all repos, in ranked order. `Enter` moves the Zellij client to the repo
  session and the worktree tab.
- Add a repo registry at `$XDG_STATE_HOME/swamp/repos.json`. `swamp launch`
  records each repo. The overview lists registered repos that have no live
  daemon, and `Enter` launches them.
- Add `swamp forget [DIR]` to remove a repo from the registry. `swamp kill`
  keeps the registry entry.
- Add `swamp register [DIR]` and `swamp register --all-live`. The second form
  records each repo that has a live daemon, which includes a daemon from an
  older swamp version. The user can then adopt open sessions without a restart.
- Add a `GetInfo` request to the daemon protocol. The reply identifies the
  repository and the Zellij session behind a socket.
- Add an "update all" action in the overview. It sends `UpdateDefault` to each
  live daemon. It does not start cold repos.
- Add tab badges. The daemon marks a worktree tab with `●` while its agent
  status is `waiting`. The `dashboard` tab of each live session shows the number
  of worktrees that wait in other repos (`dashboard ●2`).
- Add a hidden `swamp aggregate` process. It starts on demand, subscribes to
  each live daemon, and sends each daemon the cross-repo count.
- Change tab lookup so that a badge in a tab name does not break tab open,
  switch, relaunch, or close operations.

No breaking changes. An older daemon stays usable: it appears in the overview
with reduced function: it has no tab badges until the user restarts its session.

## Capabilities

### New Capabilities

- `repo-registry`: The persistent list of repos that swamp has launched, how
  swamp writes it, and how `swamp forget` removes entries.
- `cross-repo-overview`: The `swamp overview` TUI: repo discovery, the attention
  ranking, jump, cold-repo launch, and update-all.
- `attention-badges`: Waiting badges on worktree tabs, the cross-repo count
  badge on dashboard tabs, and the aggregator process that drives the count.

### Modified Capabilities

- `repo-session-launch`: The CLI command surface gains `overview`, `register`, and
  `forget` as public commands and `aggregate` as a hidden command. Launch records the
  repo in the registry and starts the aggregator.
- `daemon-tui-status`: The socket protocol gains a daemon identity request and
  an external attention count message. The dashboard TUI gains a key that opens
  the overview.

## Impact

- **Code**: `src/cli.rs`, `src/main.rs`, `src/launch.rs`, `src/kill.rs`,
  `src/zellij.rs`, `src/daemon/socket.rs`, `src/daemon/mod.rs`,
  `src/daemon/state.rs`, `src/tui/input.rs`. New modules for the registry, the
  daemon fan-in client, the overview TUI, and the aggregator.
- **Protocol**: Two new `ClientMsg` variants and one new `ServerMsg` variant.
  Existing messages do not change.
- **Files**: New `$XDG_STATE_HOME/swamp/repos.json`. New aggregator lock and PID
  files under the swamp runtime directory.
- **Zellij**: Uses `switch-session --tab-position`, `list-tabs --json`,
  `go-to-tab-by-id`, `close-tab-by-id`, and `rename-tab-by-id`. All exist in
  Zellij 0.45, which swamp already requires.
- **Delivery**: Four pull requests, in the order that `tasks.md` gives.
