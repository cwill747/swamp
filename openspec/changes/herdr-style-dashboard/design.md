# Design

## Context

See `proposal.md` for motivation and scope. The user chose a Herdr-like experience on Zellij, not a replacement multiplexer.

Observed starting points:

- `src/launch/layout.rs` generates a dashboard with four independent status TUIs and a shell in three approximately equal columns. Worktree tabs split half the width between lazygit/status and agent/shell. Both paths emit stock Zellij tab/status bars.
- `src/tui/view/mod.rs` renders the standalone `all` view as a four-panel grid. `src/tui/` already has mouse hit regions, create dialogs, deletion confirmation, themes, and single-panel modes.
- `src/fanin.rs` merges snapshots/PR updates from all live repo daemons. `src/overview/` already supplies attention ranking, stopped-repo discovery, and safe session jumps. Its list intentionally excludes non-attention worktrees; the new sidebar must not inherit that restriction.
- `AgentStatus` is working/waiting/idle. Missing records currently look idle in snapshots; `agent_ts == 0` is the legacy absence signal. There is no parent-agent completion revision or seen state. Subagents have running/done and are pruned after their existing grace period.
- Slot/title fields and subagent fields are both implemented, but the unarchived `worktree-slots` snapshot delta omits subagents. Preserve the union when reconciling, not one delta's replacement text.
- Installed Zellij 0.45.0 exposes `list-panes --all --json`, `focus-pane-id`, `resize --pane-id`, `toggle-fullscreen`, and session/tab actions. The adapter currently knows tabs, not pane roles. CLI help establishes available actions, not their multi-client correctness.

Reference: Herdr's live TUI demo at https://herdr.dev/tui/ has Spaces above Agents, two-line sidebar entries, compact state marks, a thin tab row, and terminals dominating the right. Its concepts and keyboard docs describe explicit navigation versus terminal input. These are the reference, not the separate Herdr desktop app or a promise of feature parity. Do not copy Herdr assets or branding.

## Goals / Non-Goals

**Goals:**

- Recognizable Herdr composition in actual generated Zellij sessions, not only the standalone TUI.
- Reuse one cross-repo projection and action model for standalone workspace, sidebar, details, and Go To picker.
- Keep terminal output/input owned exclusively by Zellij. A Swamp pane never renders a fake terminal preview as though interactive.
- Retain reliable identity through sorting, attention badges, branch changes, slot reuse, and daemon reconnections.

**Non-Goals:**

- PTYs, terminal parsing, custom WASM plugins, a new global session, or changing flat repo-session ownership.
- Herdr's remote machines, screen-reading agent detection, complete pane orchestration API, notifications, or agent-to-agent messaging.
- Rebinding the user's global Zellij prefix, auto-starting suspended agents, or independent completion badges per attached client.
- Automatically acknowledging completion from every native Zellij navigation path. Explicit Swamp visits and Mark Seen are the supported acknowledgement paths in this change.

## Decisions

### D1. Visual contract: Spaces / Agents rail plus terminal canvas

Use a fixed 32-cell default rail, a single subtle vertical divider, two stacked scrollable sections, and dense two-line entries. Spaces takes about 45% of available body height; Agents takes the remainder, with minimum space for headings and one entry each. Empty sections do not reserve a large blank block. The rail appears in each generated dashboard and worktree tab; these are separate Swamp processes projecting shared data, not one pane moved between tabs.

Worktree tab, schematic at a wide terminal:

```text
 swamp   dashboard   [herdr-ify]   work2                              Zellij
 Spaces                    | claude / herdr-ify
 v swamp             !1    |------------------------------------------------
   * main                  | Real agent terminal (largest pane, suspended
 >   Herdr-style dashboard | until explicitly started, as today)
     herdr-ify   +3 ~2     |
 v papr              ~1    | No transcript copied into Swamp's renderer.
     Feed cleanup          |
     rss-fix               |
 Agents          waiting 1 |------------------------------------------------
 ! Fix flaky tests         | lazygit / review       | shell
   papr / claude waiting   |                        | ~/code/swamp/herdr-ify
 ~ Herdr-style dashboard   |                        | $ git status
   swamp / claude working  |                        |
 + Review parser           |                        |
   swamp / codex done      |                        |
 / search  g go to  ? help  |                                        Zellij
```

Dashboard tab:

```text
 swamp   [dashboard]   herdr-ify   work2                              Zellij
 Spaces                    | Herdr-style dashboard
 v swamp             !1    | swamp / herdr-ify / claude     working  2m
   * main                  |
 >   Herdr-style dashboard | Branch   herdr-ify    Slot overflow
     herdr-ify   +3 ~2     | Git      3 staged / 2 unstaged
 v papr              !1    | PR       #42  checks pending  review requested
     Fix flaky tests       | Agents   claude working; reviewer running
     rss-fix               | Resources (selected repo) / loading or unavailable
 Agents          waiting 1 |
 ! Fix flaky tests         | [Open agent] [Go To] [Create] [More...]
   papr / claude waiting   |------------------------------------------------
 ~ Herdr-style dashboard   | Real shell in the repository default worktree
   swamp / claude working  | $ _
 / search  g go to  ? help  |                                        Zellij
```

Wireframes use ASCII state equivalents; normal rendering can use existing Swamp Unicode icons. Colors: terminal/default canvas; slightly differentiated rail; muted headings/secondary branch lines; cyan active-location accent; amber Waiting; green Working; blue Done; dim Idle/Unknown; red git/CI failures. Use theme tokens, never hard-code a font or force the entire terminal to black. Selected row background/caret differs from the active-tab marker. Labels/icons remain meaningful without color and with `SWAMP_ASCII`.

Titles are primary; repo, branch, harness, and slot are secondary. Preserve title-source fallbacks, visible trunk marker, no PR signals for trunk, free/pinned slots at the end, and subagent indentation. Do not show invented token counts, model names, elapsed task durations, or transcripts. Age labels describe known status timestamps only.

At rail width below 28 cells, use one-line compact entries; below 16, show state/short identity plus a search/help affordance. At body height below 10 rows, use a single selectable section with a Spaces/Agents switch. Standalone workspace at >=100 columns renders rail+details, at 60-99 columns renders a 24-cell rail+compact details, and below 60 renders one selected section with details as a drill-in. At very small sizes show an actionable resize/navigation message without panicking. Swamp can reflow only its own pane: it cannot automatically reclaim the rail's columns from sibling Zellij panes. Native pane fullscreen is the supported terminal-first escape hatch.

**Visual acceptance:** capture deterministic 120x36 and 160x48 composed layouts, 32x30 and 24x16 rails, 80x24 and 48x16 standalone views, and an extreme-small fixture. Confirm headings/row hierarchy, state/selection distinction, divider alignment, real terminal space, and no old status-panel grid in the new default. Unit snapshots cover Swamp cells; a real Zellij capture covers composition.

### D2. Layout generation and compatibility

Add `[dashboard] layout = "workspace" | "classic"` (default `workspace`) and `sidebar_width = 32` (validated range 16-60). Keep `worktrees_column`, `ai_column`, and `shell_column` parsed and used only by classic layout. `swamp init` documents this explicitly; existing percentages do not silently select classic.

New dashboard: left `swamp tui --view sidebar`, right a details pane above the existing real shell (roughly 65/35 height). New worktree tab: the same left sidebar, right a suspended agent occupying about 70% height, with lazygit and shell splitting the bottom. Keep managed lazygit config, fish/POSIX quoting, Nix entry, harness resolution, and Claude resume behavior. No extra worktree tabs are pre-created.

Use the stock one-line `compact-bar` as the tab/status strip, without also rendering a duplicate tab bar, rather than writing a Swamp plugin. Verify its exact rendering on minimum Zellij before visual sign-off. Each generated layout includes its own template as current on-demand layouts require. Default focus is the sidebar on dashboard and the agent pane on explicitly opened worktree tabs; focus does not start a suspended command. Classic preserves existing templates and proportions.

Existing sessions attach unchanged; do not override live layouts or restart agents on an upgrade. Layout config affects newly created/recreated sessions and tabs, so mixed old/new tabs are supported. Document intentional recreation, not an automatic migration. `swamp overview` remains its separate attention session; `O` still opens it.

### D3. Shared workspace projection, not an overview rewrite

Introduce a shared workspace model adjacent to `src/overview/`, fed by `fanin` and registry events. Reuse identity resolution, attention reason calculation, and jump code. Leave existing overview behavior intact; extract reusable pieces only as necessary.

Keys: repo identity is canonical common directory / repo ID, worktree destination is canonical path plus branch/session identity, and subagent identity includes parent plus agent ID. Never target by row number or display title. Slot branch reassignment invalidates old selection, acknowledgements, and pane associations. Retain selection by key through sorting; if removed select the closest surviving neighbor. Duplicate repository basenames include a short path disambiguator.

Spaces contains all live identified repos, expandable worktrees, then registered stopped repos. Do not launch stopped repos on discovery. Within a repo keep current/trunk identity and established title/slot ordering. Agents contains reported parent agents and their subagents; do not manufacture an agent for a free slot or an unreported shell. Order groups by Waiting, Done, Working, Idle, Unknown, with stable repo/title/key tie-breakers. Parent/subagent blocks stay together. A subagent click targets its parent agent pane because existing subagents are not independently addressable terminals.

Reuse existing attention reasons, preserving the overview's priority (waiting, checks failed, changes requested, conflict/rebase, idle-dirty). In the new navigator insert unseen Done after urgent reasons and before idle-dirty. A worktree occupies one attention destination regardless of multiple reasons or subagents. Passive updates never open tabs, change sessions, or move focus. Network PR/resource failures do not gate local rows or input.

### D3a. Subscription ownership, delivery, and shutdown

The existing singleton `swamp aggregate` process becomes the workspace data broker as well as the external-attention count producer. Today it follows daemons but exposes no UI subscription endpoint; adding that endpoint is explicit implementation scope, not an existing capability. Keep existing overview and legacy single-repo TUI clients working unchanged. Workspace, sidebar, and details clients use a new `src/workspace/client.rs`; broker transport and cache live under `src/workspace/`, while daemon discovery and upstream subscriptions remain in `src/fanin.rs`.

**Ownership and transport:** expose a versioned, length-prefixed JSON protocol on `<runtime_base_dir>/workspace.sock`, inside the existing private runtime directory. Reuse frame limits and runtime ownership checks, and validate client messages before use. The aggregator's existing exclusive lock protects broker bind/stale-socket replacement; a contender never removes a live endpoint. Start the broker before waiting for any upstream data. UI clients may invoke the existing detached aggregator startup path, then connect with bounded retry. Version mismatch or unavailable broker produces a visible reconnecting/unavailable state, not an empty workspace or a second per-pane fan-in fallback. No new CLI mode is necessary.

**Upstream and cache:** the broker owns exactly one fan-in scanner and at most one subscription per live repo daemon, regardless of pane count. Extend fan-in events to forward `ServerMsg::Resources` alongside snapshots and PR status using the existing `Subscribe` vocabulary; no daemon protocol change or separate resource RPC is required. The daemon already samples resources locally and polls PRs independently. Subscribe never waits for a fresh sample/fetch. Cache identity, connection state, latest local snapshot, optional PR snapshot, and optional resource snapshot separately by repo ID. Missing resource/PR messages from an older daemon remain unavailable/loading rather than fabricated empty success. Preserve stale cached data on disconnect with a disconnected label; reconnect begins a new connection generation, and late events from an old generation cannot overwrite it. Registry refresh runs off the render/input path and supplies stopped entries without launching them.

**Client protocol and selection:** clients send `Subscribe` with the supported protocol version and an optional selected repo ID, then `SelectRepo` when that identity changes. These messages express data interest only and never activate, launch, or mutate repositories. Each reply carries a complete revisioned workspace inventory (identity, connection, local rows, and PR state for all repos) plus resource state for only that client's selected repository. Include selected repo ID and upstream generation with resource state. Selecting another repo immediately clears the previous resource association and returns the new cached value or an explicit loading/unavailable/disconnected state; late values cannot be displayed against the new selection. Sidebar clients need no resource payload. Shared sidebar/details selection remains the separate D5 runtime record: the details client reads it and updates its own broker interest. Multiple clients may select different repositories without sharing or overwriting resource interest.

**Backpressure and responsiveness:** merge events into the broker cache without awaiting a UI socket write or any network/resource fetch. Publish complete latest-state revisions through per-client coalescing/watch state, not an unbounded event queue: slow clients may skip intermediate snapshots but must receive the latest connection and inventory state when they resume. Use an independent writer task per client, a bounded frame size, and a write deadline; an oversized frame or timed-out writer disconnects only that client with an error. Resource ticks update only interested clients and cannot queue ahead of or indefinitely starve inventory changes. External-attention requests run in a separate bounded worker so a slow daemon acknowledgement cannot stall the broker's cache/event loop. Rendering reads cached client state only; it does not scan sockets, git, or Zellij. Coalescing applies to state snapshots, never to user actions or completion revision acknowledgements.

**Lifetime:** use structured task ownership (`JoinSet` or an equivalent owned cancellation guard) for the fan-in scanner and every daemon follower, broker reader/writer, and attention worker. `tx.closed()` must cancel an idle upstream socket read as well as discovery/retry waits. Dropping or explicitly shutting down a client cancels and joins its local tasks and closes its connection; the broker then drops only that client's interest/writer. It does not stop the singleton or subscriptions needed by other panes or attention counts. Broker shutdown cancels and joins all owned tasks, closes sockets, and removes only its own endpoint under the aggregator lock. Retain the existing no-live-daemon idle exit policy; connected clients see broker disconnect and retry rather than keeping an idle broker alive forever.

**Verification:** use isolated Unix-socket fixtures with injected timing, not real user sessions. Cover a daemon that sends local snapshots but never PR/resources; delayed/failed PR updates; continuous resource bursts plus local row changes; a stalled UI reader; independent selected repos in two clients; selection changes racing resource delivery; disconnect/reconnect generations; broker version mismatch/restart; and receiver drop while a daemon is silent. Assert latest local state delivery within a bounded test deadline, bounded pending state, one upstream subscription per daemon across multiple clients, no cross-repo resource attribution, no actions from interest changes, and cancellation/join of every owned task. Benchmark the same path in task 7.3.

### D4. Completion metadata and shared local seen ledger

Keep hook/protocol `AgentStatus` unchanged. Add optional `activity_id` and `completion_id` metadata to agent records and worktree rows, preserving every existing git/slot/title/subagent field. Generate a new activity ID when Working begins from Idle/unknown or after a completed turn; retain it through Waiting-to-Working and repeated Working activity pings in that turn. A first Waiting report without an observed active turn does not fabricate prior work. Generate a unique opaque completion ID only when an observed active turn reaches Idle; repeated Idle hooks do not produce new completions. Persist metadata with existing status/branch-session records so daemon restarts and slot moves retain it. A new active turn supersedes the previous Done signal.

The UI derives Waiting from waiting, Working from working, Done from a completion ID absent from the seen ledger, otherwise Idle. No report is Unknown (legacy timestamp zero), not evidence of a completion. Old daemons lacking completion metadata still show their original statuses; do not infer Done from reconnection or timestamps. Subagent running/done remains its own reported state and expires under the existing daemon policy.

Store acknowledged completion IDs in `$XDG_STATE_HOME/swamp/seen.json`, keyed by repo and logical branch/session activity identity, with atomic replacement and a separate stable lock file. Acknowledgement is merge-under-lock, not last-writer-wins replacement. Prune entries for superseded activity IDs under a bounded retention policy. Malformed state is reported and treated as unacknowledged, never as loss of daemon status. No CLI status mutation or daemon roundtrip is needed for a UI acknowledgement.

A successful explicit Swamp activation of an agent/worktree acknowledges only the completion revision captured for that activation, after the target session/tab and agent pane are confirmed. A completion arriving during a jump remains unseen. Picker browsing, detail selection, failed navigation, and session-only fallback do not acknowledge. `v` / Mark Seen acknowledges the selected current revision without navigation. Native tab clicks do not automatically acknowledge; the UI/help explains `v`. Seen state is shared by local Swamp views and clients, intentionally simpler than Herdr's per-client seen badges.

### D5. Views, selection sharing, and navigation

Add `workspace`, `sidebar`, and `details` view modes. Make `swamp tui` default to standalone workspace (rail+details). Keep explicit `all`, `worktrees`, `ai-status`, `resources`, and `pr-status` behavior. Generated panes use `sidebar` and `details`, avoiding duplicate rails. Outside Zellij, standalone workspace shows live data and target paths but disables terminal-opening actions rather than recursively attaching a session. Existing `swamp overview --no-session` behavior stays unchanged.

Within a generated dashboard, sidebar and details processes share a small per-session selection record under `$XDG_RUNTIME_DIR/swamp/ui/<session-id>.json` (same private temp fallback policy as runtime sockets). Key by session identity, not caller cwd. Atomic writes and file-change/poll refresh synchronize selection without new daemon RPCs. Store canonical destination keys, section/collapse/filter state and selection, never arbitrary executable commands. The details view subscribes to data independently and renders the selected key; disappearing selection is handled by the projection. This state is shared within a repo session, including clients, and best-effort across restarts; failure must leave navigation usable. It does not steer Zellij focus.

Sidebar/navigation keys: `j/k` or arrows select, Tab switches section, left/right collapse/expand Spaces, Enter activates, `/` opens search, `g` opens Go To, `]` / `[` visits next/previous attention target, `v` marks seen, `?` opens contextual help, Esc cancels overlays. Picker state filters are `a` all, `b` waiting/blocked, `w` working, `d` Done, `i` idle; printable keys type only when search has focus. Search matches title, repo, branch, path, harness and subagent description case-insensitively, preserving parent context. Zero results shows the query and Clear action. Search uses substring matching initially, not a new fuzzy-search dependency.

Single-click on a Space/agent row activates; chevrons only expand/collapse. Keyboard movement selects/details only. Wheel scroll never activates. Buttons and More menu expose create/delete/title/harness/update/refresh/kill through existing confirmations, with explicit repo/worktree identity. Reuse `c/d/D/t/h/r/u/O/K` where unambiguous in navigation mode; destructive keys never run while typing search. Local-only mutations on a foreign selection are disabled with a reason and an Open Repo action, rather than accidentally targeting cwd. Cold-repo activation uses the existing explicit background-launch-then-switch flow.

Do not steal keys from terminal panes. Users enter the rail via native Zellij pane focus or mouse, and use native tab/pane/split/zoom/detach controls. No compulsory `Ctrl+b` binding: Herdr's prefix is a concept reference, not a safe default for an existing Zellij config. In generated navigation panes, `q` returns focus to the active terminal when identifiable instead of killing the session; standalone `q` exits only the TUI. Closing a sidebar never kills terminal work.

### D6. Pane-aware activation and failure behavior

Extend `src/zellij.rs` with typed parsing for `list-panes --all --json`, matching generated role names within the resolved destination tab, and focusing the resolved agent pane ID. Resolve IDs afresh after tab creation/session resurrection. Check session liveness before commands (Zellij can report success for nonexistent targets). Reuse the overview's attach-before-open ordering for clientless destination sessions. Do not identify a pane solely from a harness process command or a global title match.

Validate fixture schema and real behavior against Zellij 0.45.0 before depending on it; do not infer JSON field shape from current online documentation. If pane lookup/focus is unavailable or ambiguous (including renamed/removed panes), switch to the existing worktree tab and report partial success without acknowledgement. Never create a duplicate agent to compensate. Serialize repeated activation per UI instance and reconcile duplicate tab-open races against canonical tab identity.

Zellij navigation can be session-global with multiple attached clients; this design does not promise independent pane focus. Automatic polling cannot move focus. Exercise two clients during validation and document native limitations. Do not send pane input or simulate keystrokes to perform navigation.

## Risks / Trade-offs

- One sidebar per tab multiplies subscriptions -> reuse the existing singleton aggregator; cap render/event work, clean tasks on pane exit, and benchmark 20 repos / 200 worktrees. Do not synchronously scan git or spawn Zellij queries on every render.
- Fixed rail costs terminal width -> compact rendering and native fullscreen; defer custom pane-hiding plugins.
- Shared seen/selection differs from Herdr multi-client semantics -> state this in help; revision-safe acknowledgements and lock-protected merges prevent lost completions.
- Pane rename or unsupported JSON fields break direct jumps -> typed compatibility fixtures, runtime errors, and tab-only fallback without losing work.
- Live sessions do not immediately look new -> clear migration docs and classic opt-out; never sacrifice running agents to cosmetic migration.
- Untrusted repo/title/status text -> strip terminal control sequences and clip by display width; never interpolate labels into shell commands, and route mutations using validated canonical identity.
- Existing delta overlap -> reconcile union of snapshot/title/slot/subagent fields before archive and retain original tab-opening and delete safety scenarios.

## Migration Plan

1. Land projection/completion/view support behind explicit view modes while preserving old defaults.
2. Land new layout configuration and generated templates, then switch the default for newly generated layouts after composed-screen acceptance passes.
3. Document `layout = "classic"`, retained column settings, native focus/fullscreen controls, shared seen semantics, and deliberate session recreation. Update default config, README keys, and completions.
4. Keep old sessions untouched. Rollback presentation by selecting classic and intentionally recreating sessions/tabs; additive record fields remain readable by old binaries. Do not promise rollback preserves new seen metadata if an old binary rewrites records.
5. Validate with OpenSpec and project-mandated Nix build/test/lint commands before applying or archiving the implementation.
