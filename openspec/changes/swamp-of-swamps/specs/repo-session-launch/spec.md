## MODIFIED Requirements

### Requirement: CLI Command Surface
The CLI SHALL expose `launch`, `serve`, `tui`, `hook`, `kill`, `init`, `overview`, `register`, `forget`, and `completions` as public commands, while keeping `codex-notify`, `relaunch-tab`, `delete-tab`, `confirm-delete`, and `aggregate` hidden from normal help output.

#### Scenario: Help hides internal commands
- **WHEN** a user renders top-level help
- **THEN** public commands are listed
- **AND** internal commands are not shown

#### Scenario: Unknown hook status is rejected
- **WHEN** a user runs `swamp hook` with a status other than `working`, `waiting`, or `idle`
- **THEN** argument parsing fails before recording a status update

#### Scenario: Cross-repo commands are listed
- **WHEN** a user renders top-level help
- **THEN** `overview`, `register`, and `forget` are listed
- **AND** `aggregate` is not shown

## ADDED Requirements

### Requirement: Launch Registers The Repo
Launch SHALL record the target repository in the repo registry before it starts or attaches to the session. A registry failure SHALL NOT fail the launch.

#### Scenario: Successful launch
- **WHEN** launch resolves a repository with one or more worktrees
- **THEN** the repo registry contains that repository

#### Scenario: Launch attaches to an existing session
- **WHEN** launch attaches to a session that already exists
- **THEN** the registry entry for that repository has an updated last-launch time

#### Scenario: Registry write fails
- **WHEN** the registry cannot be written
- **THEN** launch continues and starts or attaches to the session

### Requirement: Launch Starts The Aggregator
Launch SHALL make sure that the aggregator process runs. Launch SHALL NOT wait for the aggregator, and an aggregator failure SHALL NOT fail the launch.

#### Scenario: No aggregator runs
- **WHEN** launch runs and no aggregator process is live
- **THEN** an aggregator process starts in the background

#### Scenario: Aggregator cannot start
- **WHEN** the aggregator process fails to start
- **THEN** launch continues and starts or attaches to the session
