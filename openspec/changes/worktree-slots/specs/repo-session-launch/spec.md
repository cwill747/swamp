## MODIFIED Requirements

### Requirement: CLI Command Surface
The CLI SHALL expose `launch`, `serve`, `tui`, `hook`, `kill`, `init`, `completions`, `slot`, `title`, and `clone` as public commands, while keeping `codex-notify`, `relaunch-tab`, `delete-tab`, and `confirm-delete` hidden from normal help output. The `slot` command SHALL expose the `alloc`, `release`, `pin`, `unpin`, and `list` subcommands.

#### Scenario: Help hides internal commands
- **WHEN** a user renders top-level help
- **THEN** public commands are listed
- **AND** internal commands are not shown

#### Scenario: Unknown hook status is rejected
- **WHEN** a user runs `swamp hook` with a status other than `working`, `waiting`, or `idle`
- **THEN** argument parsing fails before recording a status update

#### Scenario: Slot subcommands listed
- **WHEN** a user renders `swamp slot --help`
- **THEN** `alloc`, `release`, `pin`, `unpin`, and `list` are listed
