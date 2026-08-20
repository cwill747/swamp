## Why

Zellij 0.45 provides first-class nested-session controls, but swamp bypasses them by switching the outer client to the repo session. When you launch swamp from Zellij, the repo session should run inside the current pane so you can use Zellij's native zoom, focus, and breadcrumb navigation.

## What Changes

- Require Zellij 0.45 or later.
- When swamp runs inside Zellij, start a new repo session as a child in the current pane.
- When the repo session already exists, attach to it inside the current pane.
- Stop switching the outer client and closing the originating tab.
- Preserve launch and attach behavior outside Zellij.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `repo-session-launch`: Replace client switching and originating-tab cleanup with Zellij 0.45 native nested sessions.

## Impact

- Code: `src/launch.rs` and `src/zellij.rs`.
- Documentation: `README.md` and the repo session launch specification.
- Dependency: Zellij 0.45 or later is required for first-class nested-session controls.
