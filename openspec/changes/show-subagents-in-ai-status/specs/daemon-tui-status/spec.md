## MODIFIED Requirements

### Requirement: Worktree Snapshot Rows
Daemon snapshots SHALL include worktree rows with branch, upstream, ahead/behind, dirty counts, conflict/rebase state, agent status, agent timestamp, session name, head timestamp, harness override, a flag marking whether the row's branch is the repository default branch, a flag marking whether a removal is in flight for that worktree, and a list of the agent's subagents. Each subagent entry SHALL carry its id, type, optional description, status, start time, last-activity time, and optional finish time. The default-branch flag SHALL be derived from the repository's configured default branch (the default remote's `HEAD`), and SHALL be false for every row when no default branch can be determined. The deleting field and the subagent list SHALL decode to their inert defaults when a peer omits them. A legacy removal-verdict field MAY remain present for protocol compatibility, but routine scans SHALL leave it empty.

#### Scenario: Snapshot requested
- **WHEN** a client requests or subscribes to a snapshot
- **THEN** each row contains the git, agent, timestamp, harness, default-branch, deleting, and subagent fields needed by the TUI

#### Scenario: Snapshot ordering
- **WHEN** rows are emitted
- **THEN** they are sorted by newest head timestamp and then by name

#### Scenario: Default branch flagged
- **WHEN** the repository default branch is known and a worktree has that branch checked out
- **THEN** that worktree's row is marked as the default branch and every other row is not

#### Scenario: Default branch unknown
- **WHEN** the repository default branch cannot be determined
- **THEN** no row is marked as the default branch

#### Scenario: Removal verdict omitted from routine scan
- **WHEN** a worktree would be refused a non-forced removal
- **THEN** its routine snapshot does not compute or carry the blocking reason

#### Scenario: Older peer snapshot
- **WHEN** a snapshot omits the deleting flag, omits the subagent list, or includes the legacy removal-verdict field
- **THEN** it still decodes without requiring a background removal check

#### Scenario: Subagent change broadcast
- **WHEN** a subagent starts, stops, or expires
- **THEN** subscribed clients receive an updated snapshot

#### Scenario: Subagent activity ping
- **WHEN** a tool hook only moves a subagent's last-activity time
- **THEN** the daemon does not broadcast a new snapshot for that ping alone
