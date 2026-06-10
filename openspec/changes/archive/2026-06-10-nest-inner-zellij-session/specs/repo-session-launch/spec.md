## MODIFIED Requirements

### Requirement: Existing Session Attachment
When a matching Zellij session already exists, launch SHALL attach to it unless a stale daemon version is detected and an interactive restart is accepted. When launch is running **inside** an existing Zellij session (nested), it SHALL switch the live client to the matching session rather than spawning a process that leaves the originating client idle.

#### Scenario: Current session exists
- **WHEN** a matching Zellij session exists, launch is not nested, and no accepted restart is required
- **THEN** swamp attaches to that session

#### Scenario: Current session exists while nested
- **WHEN** a matching Zellij session exists and launch is running inside another Zellij session
- **THEN** swamp switches the current client to the matching session
- **AND** the originating client is not left idle in the host session

#### Scenario: Stale daemon in interactive terminal
- **WHEN** a matching session has a daemon version mismatch and the user accepts restart
- **THEN** swamp kills the old session before starting a fresh one

#### Scenario: Stale daemon in non-interactive terminal
- **WHEN** a matching session has a daemon version mismatch and no interactive prompt is available
- **THEN** swamp warns and attaches without restarting

## ADDED Requirements

### Requirement: Nested Session Launch
When launch is running inside an existing Zellij session and no matching repo session exists yet, swamp SHALL create the repo session from the generated layout AND switch the current client to it in a single operation, so the user is moved into the new session rather than being left in the host session. Launch SHALL NOT spawn the new session as a blocking child that the host client never attaches to.

#### Scenario: New session created while nested
- **WHEN** launch runs inside an existing Zellij session and no matching repo session exists
- **THEN** swamp creates the repo session using the generated layout
- **AND** switches the current client to that new session

#### Scenario: Not nested
- **WHEN** launch runs outside any Zellij session and no matching repo session exists
- **THEN** swamp starts the new session in the foreground as before, without switching an existing client

### Requirement: Originating Tab Cleanup
After a nested launch switches the client to the repo session, swamp SHALL make a best-effort attempt to close the originating tab in the host session, so the user is not left with a stale swamp tab. Swamp SHALL NOT close the originating tab when it is the host session's only tab, because doing so would tear down the host session.

#### Scenario: Host has multiple tabs
- **WHEN** a nested launch switches to the repo session and the host session has more than one tab
- **THEN** swamp closes the originating tab in the host session

#### Scenario: Host has a single tab
- **WHEN** a nested launch switches to the repo session and the originating tab is the host session's only tab
- **THEN** swamp leaves the originating tab in place and drops back to the shell it was in before

#### Scenario: Tab close fails
- **WHEN** the best-effort close of the originating tab fails
- **THEN** the switch to the repo session still succeeds and launch does not error
