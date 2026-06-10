## Context

`swamp launch` resolves a repo, builds a Zellij layout, and either attaches to an
existing session or starts a new one. `src/launch.rs::run` detects whether it is
already running inside Zellij via `zellij::in_zellij()` (checks the `ZELLIJ` env
var) and passes a `nested` flag down to `spawn_new_session`.

Today the two terminal-handover paths behave asymmetrically:

- **Existing session** (`src/launch.rs:151`): `zellij::attach` does
  `execvp("zellij attach …")`, replacing the swamp process. With `ZELLIJ` stripped
  in nested mode, this hands the terminal to the session.
- **New session** (`src/launch.rs:155-156`): `zellij::new_session_with_layout`
  runs `zellij --new-session-with-layout … --session …` via `Command::status()` —
  a **blocking child**, not an `exec`. Inside an existing Zellij session the host
  client never attaches to the freshly created session: it is born detached while
  swamp blocks in the host pane. The user sees "a new session was created but the
  parent session does nothing."

Zellij 0.44 (the pinned version) exposes `zellij action switch-session <name>
[--layout <layout>]`, which switches the **calling client** to another session,
creating it from the layout when it does not exist. This is the primitive the
nested path is missing — it both creates and switches in one call, without the
nested-zellij-in-a-pane effect that a plain `exec` would produce.

## Goals / Non-Goals

**Goals:**
- When nested and no repo session exists, create it from the layout and move the
  current client into it (single `switch-session --layout` call).
- When nested and the repo session exists, switch the current client to it.
- Best-effort close the originating tab in the host session after switching, so no
  stale swamp tab is left behind.
- Leave non-nested launch behavior byte-for-byte unchanged.

**Non-Goals:**
- Changing the layout, dashboard, daemon protocol, or CLI surface.
- Supporting terminal multiplexers other than Zellij.
- Reworking the non-nested launch/attach flow.

## Decisions

### Use `zellij action switch-session` for the nested paths
For the nested case, replace the terminal-handover step with
`zellij action switch-session <session>`, adding `--layout <layout>` when the
session does not already exist so the same call creates it.

- *Why over `exec`-ing a new session*: an `exec` of `zellij --new-session-with-layout`
  inside a host pane produces a nested Zellij rendered inside that pane, not a real
  switch. `switch-session` moves the host client to the target session cleanly —
  matching the user's expectation of being dropped into the new session.
- *Why over keeping `Command::status()`*: `status()` blocks while a detached session
  is created; the client never moves. This is the bug.
- `switch-session` is a `zellij action` (the same family already wrapped by
  `zellij::action`), so it reuses the existing helper and error handling.

The non-nested paths keep using `new_session_with_layout` (foreground) and
`attach` (`exec`) unchanged.

### Branch on `nested` inside `spawn_new_session`
`spawn_new_session` already knows `nested`. Restructure both terminal-handover
points to branch:

- Existing session, nested → `zellij::switch_session(session, None)`
- Existing session, not nested → `zellij::attach(session, nested)` (unchanged)
- New session, nested → `zellij::switch_session(session, Some(&layout_path))`
- New session, not nested → `zellij::new_session_with_layout(…)` (unchanged)

Add one new `zellij::switch_session(session, layout: Option<&Path>)` helper that
shells out to `zellij action switch-session …`.

### Close the originating tab via captured stable tab id
Before switching, capture the host session's name (`ZELLIJ_SESSION_NAME`) and the
current tab's stable id (`zellij action current-tab-info`). `switch-session` moves
only the **client**; the swamp **process** keeps running in the (now-hidden) host
pane, so after switching swamp can issue
`zellij --session <host> action close-tab-by-id <id>` to close its own originating
tab, then exit.

- Guard like `relaunch_worktree_tab` does: only close when the host session has
  more than one tab (`list_tab_names().len() > 1`); closing the sole tab would tear
  the host session down.
- Entirely best-effort: any failure is logged and ignored — the switch itself is
  the success criterion.

### Alternatives considered
- *`exec` the new session in nested mode (symmetric with `attach`)*: simplest, but
  yields a Zellij-in-a-pane rather than a real switch, and can't clean up the host
  tab. Rejected — doesn't meet the "switch to the new session" goal.
- *Spawn a detached helper (`swamp` subcommand) to close the host tab*: mirrors
  `relaunch_worktree_tab`'s detached pattern. Unnecessary here because the swamp
  process survives `switch-session` and can close its own tab directly. Kept in
  reserve if direct close proves unreliable.

## Risks / Trade-offs

- [`switch-session --layout` may not create-or-switch exactly as assumed on every
  Zellij build] → Verify against Zellij 0.44 in the implementation (a tasks item);
  fall back to "create detached, then switch" if create-on-switch is unsupported.
- [Closing the originating tab races with the client leaving it] → Use the stable
  tab id captured *before* switching and target the host session explicitly with
  `--session`; never rely on "active tab" after the client has moved.
- [Closing the host session's only tab would kill unrelated user work] → Guarded by
  the `>1 tab` check, consistent with existing tab-relaunch logic.
- [`current-tab-info` / `close-tab-by-id` id format differs from expectations] →
  Parse defensively and treat the whole cleanup as best-effort; the switch does not
  depend on it.

## Open Questions

- Should the nested existing-session path also clean up the originating tab, or only
  the new-session path? (Proposed: both, since both leave a stale host tab.)
- Does `switch-session` preserve `--force-run-commands` semantics that `attach`
  relies on, or is that only relevant to fresh attaches? Confirm during
  implementation.
