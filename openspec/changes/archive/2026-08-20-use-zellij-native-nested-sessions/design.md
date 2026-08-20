## Context

Swamp currently detects `$ZELLIJ` and uses `zellij action switch-session` to move the outer client into a repo session. That workaround targets Zellij 0.44, which does not provide first-class nested-session controls. It also closes the tab that launched swamp when possible.

Zellij 0.45 detects a Zellij client running inside a pane and offers native zoom and focus modes. Swamp must launch or attach the repo client in that pane for Zellij to detect and manage the nesting relationship.

## Goals / Non-Goals

**Goals:**

- Run repo sessions as child clients when swamp starts inside Zellij.
- Support new and existing repo sessions.
- Preserve behavior in a plain terminal.
- Remove obsolete switching and tab-cleanup code.
- Document Zellij 0.45 as the minimum supported version.

**Non-Goals:**

- Select a Zellij nested-session mode on the user's behalf.
- Configure or rebind Zellij's nested-session controls.
- Support first-class nesting on Zellij 0.44 or earlier.

## Decisions

### Use the same foreground launch and attach paths in all terminals

`new_session_with_layout` already starts a child process and waits for it. `attach` replaces swamp with the Zellij client. Both keep the inherited `$ZELLIJ` metadata available, allowing Zellij 0.45 to discover the outer session.

This removes the `switch-session` branch instead of adding another nested-only command. Zellij owns the protocol and UI, while swamp only chooses the repo session and layout.

If `$ZELLIJ_SESSION_NAME` already matches the repo session, swamp returns without attaching. This preserves the prior no-op behavior and avoids recursively attaching a session inside itself.

### Preserve the originating tab

The child repo client runs in the originating pane. Closing its tab would destroy the nesting relationship, so swamp must remove the prior best-effort cleanup behavior.

### Require Zellij 0.45

The desired controls are unavailable in earlier releases. Documentation states the minimum version instead of adding version parsing and a behavior fallback that would preserve the old, non-nested interaction.

## Risks / Trade-offs

- [A user runs swamp with Zellij 0.44 or earlier] → Document Zellij 0.45 as the minimum supported version.
- [A user's Zellij configuration changes the native nested-session mode or bindings] → Defer to Zellij and do not override user configuration.
- [A nested client exits] → The foreground child returns to the shell in the original outer pane, which preserves normal terminal process behavior.
