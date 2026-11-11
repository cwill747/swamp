## Purpose

Describe the persistent list of repositories that swamp has launched, so that
cross-repo features can find a repository that has no live daemon.

## ADDED Requirements

### Requirement: Registry Location
swamp SHALL keep the repo registry in `$XDG_STATE_HOME/swamp/repos.json`, and SHALL fall back to `$HOME/.local/state/swamp/repos.json` when `$XDG_STATE_HOME` is not set.

#### Scenario: State home is set
- **WHEN** `$XDG_STATE_HOME` is set
- **THEN** swamp reads and writes the registry at `$XDG_STATE_HOME/swamp/repos.json`

#### Scenario: State home is not set
- **WHEN** `$XDG_STATE_HOME` is not set
- **THEN** swamp reads and writes the registry at `$HOME/.local/state/swamp/repos.json`

### Requirement: Registry Entries
Each registry entry SHALL record the canonical git common directory, the Zellij session name, and the time of the last launch. The registry SHALL hold at most one entry for each canonical git common directory.

#### Scenario: Repo launched for the first time
- **WHEN** swamp launches a repository that has no registry entry
- **THEN** the registry gains one entry for that repository

#### Scenario: Repo launched again
- **WHEN** swamp launches a repository that already has a registry entry
- **THEN** the registry updates the last-launch time of that entry
- **AND** the registry does not gain a second entry

#### Scenario: Symlinked path
- **WHEN** swamp launches the same repository through a symlinked path
- **THEN** the registry resolves both paths to the same entry

### Requirement: Registry Write Safety
Registry writes SHALL be atomic and SHALL be serialized between concurrent swamp processes. A registry fault SHALL NOT prevent a launch.

#### Scenario: Concurrent launches
- **WHEN** two swamp processes register different repositories at the same time
- **THEN** the registry contains both entries afterward

#### Scenario: Malformed registry file
- **WHEN** the registry file cannot be parsed
- **THEN** swamp preserves the malformed file under a different name
- **AND** swamp treats the registry as empty and continues

#### Scenario: Registry is not writable
- **WHEN** swamp cannot write the registry during launch
- **THEN** swamp logs a warning and continues the launch

### Requirement: Stale Entry Handling
Registry readers SHALL omit an entry whose git common directory no longer exists. The next registry write SHALL remove such an entry.

#### Scenario: Repository directory deleted
- **WHEN** a registered repository directory is deleted from disk
- **THEN** registry readers do not return that repository

### Requirement: Session Kill Keeps Registration
`swamp kill` SHALL NOT remove the registry entry of the repository that it stops.

#### Scenario: Kill then list
- **WHEN** the user runs `swamp kill` for a registered repository
- **THEN** the repository remains in the registry

### Requirement: Forget Command
`swamp forget [DIR]` SHALL remove the registry entry for the repository at `DIR`, or at the current directory when `DIR` is omitted. The command SHALL NOT stop a daemon or a Zellij session.

#### Scenario: Forget a registered repo
- **WHEN** the user runs `swamp forget` inside a registered repository
- **THEN** the registry no longer contains that repository

#### Scenario: Forget an unregistered repo
- **WHEN** the user runs `swamp forget` for a repository that is not registered
- **THEN** the command reports that no entry exists and exits with success

#### Scenario: Forget a live repo
- **WHEN** the user runs `swamp forget` for a repository whose session is live
- **THEN** the daemon and the Zellij session continue to run

### Requirement: Register Command
`swamp register [DIR]` SHALL add or update the registry entry for the repository at `DIR`, or at the current directory when `DIR` is omitted. `swamp register --all-live` SHALL add or update an entry for each repository that has a live daemon, which includes a daemon from an older swamp version. Neither form SHALL start, stop, or restart a daemon or a Zellij session.

#### Scenario: Register one repo
- **WHEN** the user runs `swamp register` inside a git repository that is not registered
- **THEN** the registry contains that repository

#### Scenario: Register outside a repo
- **WHEN** the user runs `swamp register` in a directory that is not inside a git repository
- **THEN** the command fails with an error and the registry does not change

#### Scenario: Register all live repos
- **WHEN** the user runs `swamp register --all-live` while three repo daemons are live
- **THEN** the registry contains all three repositories
- **AND** all three daemons and their sessions continue to run without interruption

#### Scenario: Live daemon from an older version
- **WHEN** a live daemon does not support the identity request
- **THEN** `swamp register --all-live` still registers its repository

#### Scenario: Daemon cannot be identified
- **WHEN** swamp cannot determine the repository of a live daemon
- **THEN** the command registers the other repositories
- **AND** the command reports the daemon that it skipped

#### Scenario: Report
- **WHEN** `swamp register --all-live` completes
- **THEN** the command prints each repository that it registered

#### Scenario: Directory with all-live
- **WHEN** the user passes both `DIR` and `--all-live`
- **THEN** argument parsing fails
