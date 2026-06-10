## Why

When `swamp` is run from inside an existing Zellij session and no matching repo
session exists yet, launch spawns the new session as a blocking child process
(`zellij --new-session-with-layout … `) and never hands the terminal over to it.
The result: a new session is created but the user's current ("parent") session
just sits there doing nothing, leaving the user stranded in the originating tab.
The user expects swamp to drop them into the new repo session instead.

## What Changes

- When launching from **inside** an existing Zellij session (nested), swamp SHALL
  switch the current client to the repo session instead of spawning a detached
  child that the client never attaches to.
- Use `zellij action switch-session <session> [--layout <layout>]` for the nested
  case so a single call both **creates** the session from the generated layout (when
  absent) and **switches** the live client to it.
- Best-effort close the originating tab in the host session after switching, so the
  user is not left with a dead/stale swamp tab — skipped when it is the host
  session's only tab (closing it would tear down the host session).
- Non-nested launch (run from a plain terminal, not inside Zellij) is unchanged.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `repo-session-launch`: the launch/attach behavior changes for the nested case
  (running swamp from inside an existing Zellij session). New session creation and
  existing-session attachment both switch the live client to the repo session and
  clean up the originating tab, rather than spawning a child that does nothing.

## Impact

- Code: `src/launch.rs` (`spawn_new_session`) and `src/zellij.rs`
  (`new_session_with_layout`, `attach`, new `switch_session` helper, originating-tab
  capture/close helpers).
- Behavior: nested launch UX only; no CLI surface, config, or daemon protocol
  changes.
- Dependency: relies on `zellij action switch-session` (Zellij ≥ 0.44, already the
  pinned toolchain version).
