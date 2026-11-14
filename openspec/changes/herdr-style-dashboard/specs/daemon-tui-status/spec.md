## MODIFIED Requirements

### Requirement: Worktree Snapshot Rows
Daemon snapshots SHALL retain the union of existing git, harness, status, timestamp, default-branch, deleting, subagent, slot, pin, and resolved-title fields, with optional activity and completion identifiers. Each subagent SHALL retain its id, type, optional description, status, start, last-activity, and finish timestamps. Missing additive fields SHALL decode to inert defaults. The default branch SHALL come from the configured default remote HEAD, and all rows SHALL be unmarked when it is unknown. Routine snapshots SHALL NOT perform removal checks; the legacy removal-verdict field SHALL remain readable and empty in new snapshots.

#### Scenario: Snapshot requested
- **WHEN** a client requests or subscribes to a snapshot
- **THEN** git, agent, harness, timestamps, default branch, deletion, subagents, slot kind/state, pin, title/source, and optional activity/completion fields are preserved together

#### Scenario: Snapshot ordering
- **WHEN** rows are emitted
- **THEN** occupied work is sorted by newest head timestamp then name, followed by free/pinned slots in numeric slot order

#### Scenario: Default branch flagged
- **WHEN** the configured default branch is checked out
- **THEN** only its worktree row is marked as trunk

#### Scenario: Default branch unknown
- **WHEN** the default branch cannot be determined
- **THEN** no row is marked as trunk

#### Scenario: Removal verdict omitted from routine scan
- **WHEN** a worktree would refuse non-forced removal
- **THEN** routine scans do not compute or carry its blocking reason

#### Scenario: Older peer snapshot
- **WHEN** a peer omits additive metadata or includes a legacy removal verdict
- **THEN** it decodes with inert defaults without triggering removal checks or fabricating agents or completions

#### Scenario: Subagent change broadcast
- **WHEN** a subagent starts, stops, or expires
- **THEN** subscribers receive updated subagent rows

#### Scenario: Subagent activity ping
- **WHEN** only a subagent's last-activity timestamp changes
- **THEN** that ping alone does not require a snapshot broadcast

### Requirement: TUI Daemon Startup
Explicit legacy views SHALL continue probing and starting their current repository daemon on demand with the existing bounded startup timeout. Workspace, sidebar, and details views SHALL connect to the singleton aggregator broker with bounded reconnect attempts and visible unavailable state. Passive workspace discovery SHALL NOT launch stopped repositories or their sessions.

#### Scenario: Legacy view starts
- **WHEN** a legacy view has no responsive current-repository daemon
- **THEN** it starts and probes that daemon and fails if the existing startup timeout expires

#### Scenario: Daemon already running
- **WHEN** an explicit legacy TUI view starts with a responsive current-repository daemon
- **THEN** it connects to the existing daemon

#### Scenario: Daemon not running
- **WHEN** an explicit legacy TUI view starts without a responsive current-repository daemon
- **THEN** it starts that daemon and waits for its response

#### Scenario: Daemon unavailable
- **WHEN** no daemon answers a legacy view before the startup timeout
- **THEN** that view fails instead of presenting a successful empty state

#### Scenario: Workspace starts
- **WHEN** workspace navigation starts
- **THEN** it may start the singleton broker, displays connection state, and discovers running and registered stopped repositories without activating them

### Requirement: TUI Views
The TUI SHALL support workspace, sidebar, and details views in addition to all, worktrees, ai-status, resources, and pr-status. The default standalone view SHALL be workspace, presenting Spaces/Agents navigation and selected-destination details. Explicit legacy view names SHALL preserve their existing panel behavior.

#### Scenario: All view
- **WHEN** the TUI is run with the explicit all view
- **THEN** all legacy status panels are rendered together

#### Scenario: Single-panel view
- **WHEN** the TUI is run with a legacy single-panel view mode
- **THEN** only that panel's status view is rendered

#### Scenario: Default standalone view
- **WHEN** the TUI is run without an explicit view
- **THEN** it renders the workspace navigation/details presentation

#### Scenario: Generated sidebar and details views
- **WHEN** separate sidebar and details panes run in the same generated dashboard
- **THEN** the sidebar renders navigation only and the details pane follows its selected destination without rendering a second sidebar

#### Scenario: Standalone outside Zellij
- **WHEN** workspace view runs outside Zellij
- **THEN** it shows live status and destination paths with terminal-opening actions disabled and explained
- **AND** it does not automatically attach nested sessions

## ADDED Requirements

### Requirement: Backward-compatible Agent Completion Revisions
Agent records and snapshot rows SHALL carry optional activity and completion identifiers in addition to all existing fields. A new active turn SHALL establish a new activity identity; Waiting and repeated Working pings within a turn SHALL retain it. An observed active turn reaching Idle SHALL create one completion identity, and repeated Idle updates SHALL NOT create further completions. These identifiers SHALL survive daemon restart and branch-session moves through slots. Existing working/waiting/idle hook inputs and legacy protocol decoding SHALL remain valid.

#### Scenario: Work completes once
- **WHEN** a Working agent reports Idle and then reports Idle repeatedly
- **THEN** it receives one completion identifier for that turn rather than one per hook

#### Scenario: Waiting within a turn
- **WHEN** an agent transitions Working to Waiting to Working to Idle
- **THEN** it keeps the same activity identity through that turn and records one completion at its end

#### Scenario: Unobserved idle
- **WHEN** an unreported worktree first reports Idle without an observed active turn
- **THEN** no completed turn is fabricated

#### Scenario: Reconnection and slot reuse
- **WHEN** a completed branch session is restored after daemon restart or moved into another slot
- **THEN** its completion identity remains stable
- **AND** a different branch later occupying the original slot does not inherit that completion

#### Scenario: Legacy peer and stored record
- **WHEN** a client or daemon reads data without activity or completion identifiers
- **THEN** missing fields decode to inert defaults and the original status remains usable

#### Scenario: Existing row metadata retained
- **WHEN** completion metadata is added to a snapshot row
- **THEN** git, harness, timestamps, trunk, deletion, subagents, slot state, pins, and resolved title/source fields retain their existing values and compatibility defaults

### Requirement: Non-blocking Workspace Data Updates
Workspace TUI clients SHALL merge live repository data without waiting for PR fetches or resource samples, preserve stable destination selection across broadcasts, and visibly distinguish loss of connection from successful empty results. A singleton workspace broker owned by the existing aggregator SHALL share one upstream subscription per live daemon across workspace panes. Clients SHALL receive revisioned inventory and resources scoped to their own selected repository over a versioned private local socket. Slow clients, resource updates, and external-attention requests SHALL NOT block the broker's inventory processing. Pending client state SHALL be bounded and coalesced to the latest complete state. Closing a workspace pane SHALL release its client tasks and resource interest without stopping unrelated subscriptions, repo daemons, or terminal processes.

#### Scenario: Slow PR response
- **WHEN** one repository's PR data is slow or fails
- **THEN** other repositories and local worktree navigation continue updating

#### Scenario: Daemon disconnects
- **WHEN** a subscribed daemon stops answering
- **THEN** its previously known state is marked disconnected/stale or replaced by a confirmed stopped state, not shown as a live empty repository

#### Scenario: Sidebar exits
- **WHEN** a sidebar client exits
- **THEN** its local subscription tasks are cancelled and joined, and its broker connection and resource interest are released without terminating the repo session or its agents
- **AND** subscriptions needed by other panes and external-attention counts remain active

#### Scenario: Missing optional streams
- **WHEN** a daemon sends local snapshots but sends no PR or resource messages
- **THEN** local worktrees become navigable without waiting for those messages
- **AND** missing optional data is labeled loading or unavailable rather than successful empty data

#### Scenario: Shared upstream subscriptions
- **WHEN** several workspace, sidebar, or details clients connect to the broker
- **THEN** the broker uses at most one upstream subscription per live daemon
- **AND** an unavailable or incompatible broker is reported visibly and retried without starting per-pane upstream fan-in

#### Scenario: Selected repository resources
- **WHEN** two clients select different repositories or one changes selection while a resource update is in flight
- **THEN** each receives only resource state associated with its own selected repository
- **AND** previous-selection or old-connection-generation data never appears as current data for the new selection
- **AND** resource interest changes do not activate or mutate any destination

#### Scenario: Slow consumer and background requests
- **WHEN** a client stops reading, resource messages arrive repeatedly, or an external-attention request stalls
- **THEN** the broker continues processing local inventory updates for responsive clients
- **AND** slow-client pending state remains bounded, intermediate state may be coalesced, and a timed-out writer disconnects only its client

#### Scenario: Reconnection generation
- **WHEN** a daemon disconnects and reconnects while old updates remain in flight
- **THEN** its new connection begins a distinct generation and old-generation updates cannot overwrite new state
- **AND** previously cached data is explicitly stale until replaced rather than presented as fresh

#### Scenario: Broker restart and shutdown
- **WHEN** the broker restarts or shuts down with an upstream daemon silent on its socket
- **THEN** shutdown cancels and joins discovery, follower, writer, and attention-worker tasks without requiring another daemon message
- **AND** clients visibly reconnect using bounded retries and do not remove a live broker endpoint or stop terminal processes
