## ADDED Requirements

### Requirement: Zellij Version Compatibility
Swamp SHALL require Zellij 0.45 or later for repo session launch.

#### Scenario: Supported Zellij version
- **WHEN** the user launches swamp with Zellij 0.45 or later on `PATH`
- **THEN** swamp can use Zellij's native nested-session controls

## MODIFIED Requirements

### Requirement: Existing Session Attachment
When a matching Zellij session already exists, launch SHALL attach to it unless a stale daemon version is detected and an interactive restart is accepted. When launch is running inside an existing Zellij session, it SHALL attach to the matching repo session as a child in the current pane so Zellij can provide native nested-session controls.

#### Scenario: Current session exists
- **WHEN** a matching Zellij session exists, launch is not nested, and no accepted restart is required
- **THEN** swamp attaches to that session

#### Scenario: Current session exists while nested
- **WHEN** a matching Zellij session exists and launch is running inside another Zellij session
- **THEN** swamp attaches to the matching session inside the current pane
- **AND** Zellij offers its native nested-session controls

#### Scenario: Repo session is already active
- **WHEN** launch runs from inside the matching repo session
- **THEN** swamp leaves the current session active
- **AND** does not attach the session recursively

#### Scenario: Stale daemon in interactive terminal
- **WHEN** a matching session has a daemon version mismatch and the user accepts restart
- **THEN** swamp kills the old session before starting a fresh one

#### Scenario: Stale daemon in non-interactive terminal
- **WHEN** a matching session has a daemon version mismatch and no interactive prompt is available
- **THEN** swamp warns and attaches without restarting

### Requirement: Nested Session Launch
When launch is running inside an existing Zellij session and no matching repo session exists, swamp SHALL start the repo session from the generated layout as a child in the current pane. Swamp SHALL preserve the outer session and originating tab so Zellij 0.45 or later can provide native nested-session zoom, focus, and navigation controls.

#### Scenario: New session created while nested
- **WHEN** launch runs inside an existing Zellij session and no matching repo session exists
- **THEN** swamp creates the repo session using the generated layout inside the current pane
- **AND** Zellij offers its native nested-session controls

#### Scenario: Not nested
- **WHEN** launch runs outside any Zellij session and no matching repo session exists
- **THEN** swamp starts the new session in the foreground as before

## REMOVED Requirements

### Requirement: Originating Tab Cleanup
**Reason**: The originating tab contains the native nested repo session and must remain open.

**Migration**: Zellij 0.45 manages entry into and exit from the nested session through its native controls.
