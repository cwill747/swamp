## Context

See `proposal.md` for the motivation.

Current state that shapes the design:

- Each repo has one daemon. The daemon binds
  `$XDG_RUNTIME_DIR/swamp/<repo_id>.sock`, where `repo_id` is an FNV-1a hash of
  the canonical git common directory (`src/util.rs`). The hash is one-way.
- A client that sends `Subscribe` receives `Snapshot`, `Resources`, and
  `PrStatus` frames, then all later broadcasts (`src/daemon/socket.rs`).
- No `ServerMsg` carries the repository path or the session name.
- A socket exists only while its daemon runs. swamp keeps no list of repos.
- The layout uses the stock Zellij `tab-bar` plugin, which shows tab names only.
- swamp finds tabs by exact name (`go_to_tab_name`, `list_tab_names`,
  `close_tab_by_name` in `src/zellij.rs`).
- Zellij 0.45 provides `switch-session <NAME> --tab-position <N>`,
  `list-tabs --json`, `go-to-tab-by-id`, `close-tab-by-id`, and
  `rename-tab-by-id`.

```
 swamp overview (TUI)        swamp aggregate (hidden, single instance)
   attention list              subscribes to each *.sock
   jump / cold launch          sends ExternalAttention{count} to each daemon
   update all
        │  fan-in client (shared module)  │
        ▼                                 ▼
 per-repo daemon  (existing)
   + GetInfo reply
   + single writer of tab names in its own session
        ▲
 repo registry  $XDG_STATE_HOME/swamp/repos.json  (written at launch)
```

## Goals / Non-Goals

**Goals:**

- One ranked list of the worktrees that need the user, across all repos.
- One keystroke from a list item to the correct session and tab.
- A waiting signal that is visible from each repo session without the overview.
- An older daemon and a newer client continue to work together.

**Non-Goals:**

- An outer Zellij session that nests the repo sessions.
- Full per-repo worktree tables in the overview.
- A replacement for the stock `tab-bar` plugin.
- Desktop notifications.
- Create, delete, or kill operations from the overview.
- Automatic start of cold repos for update-all.

## Decisions

### D1. Flat sessions with `switch-session`, not nested sessions

The overview moves the Zellij client between sibling sessions with
`zellij action switch-session`.

- **Alternative: an outer session with one nested attach for each repo.** This
  is the manual setup that the user has today. It adds a second layer of Zellij
  keybindings and one extra step to reach a worktree tab.

### D2. The overview lives in its own Zellij session

`swamp overview` creates or opens a session named `swamp-overview` that holds
only the overview TUI. The user then has a stable place to return to after a
jump. The dashboard TUI in each repo session gets a key (`O`) that switches to
that session.

- Inside Zellij, `swamp overview` runs `switch-session swamp-overview`.
- Outside Zellij, `swamp overview` attaches to the session, or creates it.
- **Alternative: a floating pane in each repo session.** A `switch-session`
  call from that pane leaves the pane open in the old session. Each session
  then accumulates overview panes.

### D3. Jump resolves a tab position in the target session

`switch-session` accepts a tab position, not a tab name. The jump sequence is:

1. Run `zellij --session <S> action list-tabs --json`.
2. Find the tab whose canonical name (see D7) equals the worktree name.
3. If no tab exists, open one in session `<S>` with the same layout code that
   the dashboard uses, then list the tabs again.
4. Run `zellij action switch-session <S> --tab-position <N>`.

Spike results (Zellij 0.45.0):

- `--tab-position` is 1-based. It equals the `position` field of
  `list-tabs --json` plus 1. Position 2 landed on the tab with `position: 1`.
- `list-tabs --json` returns objects with `tab_id`, `position`, and `name`.
- `zellij --session <S> action new-tab` from outside opens the tab in `<S>` and
  prints the new tab ID on stdout.
- `zellij --session <S> action switch-session <T>` from outside moves the
  client of `<S>` to `<T>`.
- An action against a session that does not exist prints an error and exits
  with status 0. swamp must check that the session exists before it trusts an
  action result.

If step 3 fails, the overview switches to the session without a tab position.
The user lands on the last active tab, which is an acceptable result.

Outside Zellij, the overview focuses the tab with
`zellij --session <S> action go-to-tab-by-id`, then attaches.

### D4. `GetInfo`, with identity derived from the snapshot for old daemons

New `ClientMsg::GetInfo` returns
`ServerMsg::Info { common_dir, session_name, version }`.

An old daemon does not know `GetInfo`. Each `WorktreeRow` in its `Snapshot`
carries the absolute worktree `path`. The fan-in client resolves the git common
directory from the first row path that still exists, with the same functions
that `swamp kill` uses (`resolve_git_dir`, `git_common_dir`). The client accepts
the result only when `repo_id(common_dir)` equals the socket file stem. The
session name then comes from `session_name_for(common_dir)`.

Spike result: a daemon built from `main` fails to decode an unknown request
type, returns an error from the connection handler, and closes that connection
without a reply. Other connections are not affected. The fan-in client
therefore treats a closed connection after `GetInfo` as "not supported" and
opens a new connection for `Subscribe`.

If that check fails, the client looks for a registry entry whose
`repo_id(common_dir)` equals the socket file stem. If no entry matches, the
overview shows the repo by socket ID, shows its items, and disables jump for
them with a "restart this session" hint. This last case needs an old daemon
whose worktree paths are all gone, so it is rare.

`swamp register --all-live` uses the same identification for each socket in the
runtime directory, then upserts each identified repo into the registry.

- **Alternative: the overview registers each live repo that it discovers.** A
  `swamp forget` for a live repo would then be undone at the next scan. An
  explicit command keeps `forget` meaningful.
- **Alternative: read `/proc/<pid>/cwd` from the PID file.** This is specific
  to Linux, and the daemon working directory is not always the repo.

### D5. Registry file

Path: `$XDG_STATE_HOME/swamp/repos.json`, with fallback
`$HOME/.local/state/swamp/repos.json`.

```json
{ "repos": [ { "common_dir": "/abs/path/.bare",
               "session_name": "myrepo-3b8b",
               "last_launched": 1790000000 } ] }
```

- `swamp launch` upserts the entry, keyed by canonical `common_dir`.
- A writer takes an exclusive `flock` on a sibling lock file, writes a
  temporary file, then renames it. This matches the pattern in `src/hook.rs`.
- A reader hides an entry whose `common_dir` no longer exists. The next writer
  removes it.
- A malformed file is renamed to `repos.json.corrupt` and treated as empty. A
  registry fault must not block launch.
- `swamp kill` does not change the registry. `swamp forget [DIR]` removes the
  entry. `swamp register [DIR]` and `swamp register --all-live` add entries
  without a launch (D4).

### D6. One fan-in module, two consumers, no aggregator socket

A shared module discovers sockets, connects, sends `GetInfo` and `Subscribe`,
and reconnects. The overview TUI and the aggregator both use it directly.

- Discovery scans the runtime directory for `*.sock` on a short interval
  (2 seconds). A scan is sufficient and avoids a file-watch dependency for a
  directory that changes rarely.
- A connection that fails or closes is dropped. The next scan retries it.
- **Alternative: the overview subscribes to the aggregator.** That needs a
  third protocol and makes the overview depend on a second process. The cost of
  the chosen design is two subscriptions per daemon, which is small.

### D7. The daemon is the only writer of tab names in its session

All tab operations resolve tabs with `list-tabs --json`, compare canonical
names, and then act by stable tab ID.

- Canonical name: remove a leading `● ` from a worktree tab name. Remove a
  trailing ` ●<digits>` from the dashboard tab name.
- The daemon reconciles tab names after each state change that affects a badge,
  and on a slow timer (30 seconds). The timer repairs names after a session
  resurrection or a manual rename.
- The daemon runs outside the session, so it uses `zellij --session <S>`.
- A failed rename is logged at debug level and ignored. A daemon can run
  without a Zellij session.
- IDs are resolved at each use and are not stored. A stored ID can go stale
  across a session resurrection.
- **Alternative: the aggregator renames dashboard tabs directly.** Two writers
  in one session can race on the same tab name.
- **Alternative: a swamp-owned tab bar.** This gives full control but replaces
  a stock component for a small visual gain.

Spike results (Zellij 0.45.0):

- `rename-tab-by-id`, `go-to-tab-by-id`, and `close-tab-by-id` all work from
  outside the session with `--session`.
- Each of them exits with status 0 for an ID that does not exist. The caller
  must resolve the ID from a fresh `list-tabs` result and cannot rely on the
  exit status.
- A renamed tab keeps its name across detach and across a session
  resurrection. A stale badge therefore does survive a resurrection, and the
  reconcile timer is necessary.
- Tab IDs are assigned again after a resurrection. This confirms that swamp
  must not store them.

`close_tab_by_name` today focuses the tab, closes the active tab, and restores
focus. `close-tab-by-id` removes that sequence and its failure mode.

### D8. The aggregator sends a count to each daemon

New `ClientMsg::ExternalAttention { count }`. The aggregator computes, for each
live daemon, the number of `waiting` worktrees in all other live repos, and
sends the value when it changes. The daemon stores the count in memory and
applies the dashboard badge (D7).

- A daemon sets its count to zero when it has received no
  `ExternalAttention` message for 60 seconds. The aggregator resends the count
  each 20 seconds. A dead aggregator therefore cannot leave a stale badge.
- An old daemon rejects the message. The aggregator marks that connection as
  "no badge support" and stops sending to it.
- Only `waiting` counts. CI failures and review states appear in the overview
  but do not change the badge. This keeps the badge meaning exact: an agent is
  blocked on the user.

### D9. Aggregator lifecycle

- `swamp aggregate` is a hidden command. `swamp launch` and daemon startup
  spawn it detached.
- A non-blocking exclusive `flock` on `aggregator.lock` in the runtime
  directory enforces one instance. A second instance exits with success.
- The aggregator exits after 60 seconds with no live daemon.
- The aggregator has no socket and holds no state on disk.

### D10. Attention ranking

Each worktree row maps to at most one item, at its highest tier:

| Tier | Condition | Order inside the tier |
|---|---|---|
| 1 | Agent status is `waiting` | Oldest `agent_ts` first |
| 2 | PR checks are `Failure` | Repo, then worktree name |
| 3 | PR review is `ChangesRequested` | Repo, then worktree name |
| 4 | `conflict` or `rebase` is set | Repo, then worktree name |
| 5 | Agent is `idle` and the tree has staged, unstaged, or untracked changes | Newest `agent_ts` first |

The default-branch row never produces a tier 5 item. A row with `deleting` set
produces no item. The ranking is a pure function of the snapshots, so unit
tests cover it without a daemon.

Cold repos appear in a separate section under the items.

### D11. Update all

`U` in the overview sends `UpdateDefault` to each live daemon concurrently, one
short connection per daemon. The footer shows a count of successes and the
names of repos that failed. One failure does not stop the others.

## Risks / Trade-offs

- **`zellij --session <S> action new-tab` from outside the session can behave
  differently from the same call inside** → Spike 1.2 verifies it. The fallback
  in D3 keeps jump usable.
- **A renamed tab can confuse a user script that matches tab names** → The
  badge is a fixed prefix or suffix. A config key `[badges] enabled = false`
  turns both badges off.
- **A session resurrection restores a stale badge** → The reconcile timer in D7
  corrects it within 30 seconds.
- **A worktree whose name starts with `● `** → The canonical form is ambiguous
  for that name. The badge code skips such a worktree and logs a warning.
- **Two daemon versions with one aggregator** → Each new message has a defined
  result against an old daemon (D4, D8). A version-skew test covers both.
- **Subscription load with many repos** → Each daemon gains at most two
  subscribers. Broadcast cost is linear in subscribers and is small.
- **The registry grows without bound** → Entries for deleted paths are pruned
  (D5). `swamp forget` removes the rest.

## Migration Plan

Four pull requests, each usable alone:

1. `GetInfo` and the registry, with `swamp forget`.
2. The fan-in module and `swamp overview` with jump and cold launch.
3. Update all.
4. Tab lookup by ID, worktree badges, the aggregator, and the count badge.

Rollback: revert the pull request. The registry file and the lock files are
inert without the code that reads them. A daemon from before a pull request
keeps its existing behavior until the user restarts the session.

## Open Questions

- The badge glyph in ASCII mode (`ascii_mode()` in `src/util.rs`). A `*` is the
  default candidate.
- The key for update-all (`U`) and for open-overview (`O`) can change after
  first use.
