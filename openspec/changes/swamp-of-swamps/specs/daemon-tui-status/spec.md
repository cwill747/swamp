## ADDED Requirements

### Requirement: Daemon Identity Request
The daemon SHALL answer an identity request with the git common directory of its repository, the name of its Zellij session, and its version. The request SHALL NOT require a subscription.

#### Scenario: Identity requested
- **WHEN** a client sends an identity request
- **THEN** the daemon replies with the git common directory, the session name, and the daemon version

#### Scenario: Identity before subscribe
- **WHEN** a client sends an identity request and then subscribes on the same connection
- **THEN** the client receives the identity reply and then the current snapshots

#### Scenario: Identity reply among broadcasts
- **WHEN** a subscribed client waits for an identity reply and receives unrelated broadcasts
- **THEN** the client skips the unrelated messages and continues to wait

### Requirement: External Attention Count
The daemon SHALL accept a message that carries the number of worktrees that wait in other repositories, and SHALL acknowledge it. The daemon SHALL treat the count as zero when it has received no such message for 60 seconds. The count SHALL NOT appear in worktree snapshots and SHALL NOT persist across a daemon restart.

#### Scenario: Count received
- **WHEN** a client sends an external attention count
- **THEN** the daemon acknowledges the message and holds that count

#### Scenario: Count not refreshed
- **WHEN** 60 seconds pass with no external attention count message
- **THEN** the daemon holds a count of zero

#### Scenario: Daemon restarts
- **WHEN** the daemon restarts
- **THEN** the daemon holds a count of zero until it receives a new message

### Requirement: Unknown Request Handling
A daemon that receives a request type that it does not know SHALL answer with an error message and SHALL keep serving other clients.

#### Scenario: Newer client, older daemon
- **WHEN** a client sends a request type that the daemon does not know
- **THEN** the client receives an error reply or a closed connection
- **AND** the daemon continues to serve its other clients

### Requirement: Open Overview From Dashboard
The dashboard TUI SHALL provide a key that moves the user to the cross-repo overview session. The key SHALL have no effect outside Zellij.

#### Scenario: Key pressed inside Zellij
- **WHEN** the user presses the overview key in the dashboard TUI inside Zellij
- **THEN** the client moves to the overview session, which swamp creates when necessary

#### Scenario: Key pressed outside Zellij
- **WHEN** the user presses the overview key while the TUI runs outside Zellij
- **THEN** the TUI stays open and reports that the overview requires Zellij
