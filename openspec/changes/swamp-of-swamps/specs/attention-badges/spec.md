## Purpose

Describe the badges that swamp shows in the Zellij tab bar when an agent waits
for the user, in the current repo session and in other repo sessions.

## ADDED Requirements

### Requirement: Worktree Waiting Badge
While the agent status of a worktree is `waiting` and that worktree has an open tab, the tab name SHALL carry a waiting badge before the worktree name. The tab name SHALL return to the plain worktree name when the status is no longer `waiting`.

#### Scenario: Agent enters waiting
- **WHEN** the agent status of a worktree with an open tab changes to `waiting`
- **THEN** the tab name becomes `● ` followed by the worktree name

#### Scenario: Agent leaves waiting
- **WHEN** the agent status of that worktree changes to `working` or `idle`
- **THEN** the tab name becomes the plain worktree name

#### Scenario: No open tab
- **WHEN** the agent status of a worktree without an open tab changes to `waiting`
- **THEN** swamp does not open a tab for it

#### Scenario: Tab opened while waiting
- **WHEN** the user opens the tab of a worktree whose agent already waits
- **THEN** the tab name carries the badge

### Requirement: Cross-Repo Count Badge
The `dashboard` tab of each live repo session SHALL show the number of worktrees with agent status `waiting` in all other live repositories. The tab name SHALL be plain `dashboard` when that number is zero. Worktrees of the session's own repository SHALL NOT count toward its dashboard badge.

#### Scenario: Other repo has waiting agents
- **WHEN** two worktrees in other repositories have agent status `waiting`
- **THEN** the dashboard tab name is `dashboard ●2`

#### Scenario: Only the own repo waits
- **WHEN** the only waiting worktrees belong to the session's own repository
- **THEN** the dashboard tab name is `dashboard`

#### Scenario: Count returns to zero
- **WHEN** the last waiting agent in other repositories leaves `waiting`
- **THEN** the dashboard tab name returns to `dashboard`

#### Scenario: Other repo session stops
- **WHEN** a repository with waiting agents stops its daemon
- **THEN** the other sessions no longer count those agents

### Requirement: Badge Scope
Only the `waiting` agent status SHALL produce a badge. PR check failures, review decisions, conflicts, and uncommitted changes SHALL NOT change a tab name.

#### Scenario: CI fails
- **WHEN** PR checks fail for a worktree whose agent does not wait
- **THEN** no tab name changes

### Requirement: Tab Operations Ignore Badges
A badge in a tab name SHALL NOT change the result of any swamp tab operation. Open, switch, relaunch, and close operations SHALL target the same tab with or without a badge, and SHALL NOT create a duplicate tab.

#### Scenario: Activate a badged worktree
- **WHEN** the user activates a worktree whose tab carries a badge
- **THEN** swamp switches to the existing tab and does not open a second tab

#### Scenario: Delete a badged worktree
- **WHEN** the user deletes a worktree whose tab carries a badge
- **THEN** swamp closes that tab

#### Scenario: Relaunch a badged tab
- **WHEN** the user relaunches a worktree tab that carries a badge
- **THEN** swamp replaces that tab and the new tab has the correct name

#### Scenario: Dashboard with count badge
- **WHEN** the dashboard tab carries a count badge
- **THEN** operations that return focus to the dashboard still find it

### Requirement: Badge Repair
swamp SHALL correct a tab name that does not match the current state within 30 seconds, without user action.

#### Scenario: Session resurrected with a stale badge
- **WHEN** a session is restored with a badge on a worktree whose agent no longer waits
- **THEN** swamp removes the badge within 30 seconds

#### Scenario: User renames a tab
- **WHEN** the user removes a badge by hand while the agent still waits
- **THEN** swamp restores the badge within 30 seconds

### Requirement: Stale Count Expiry
A dashboard count badge SHALL clear within 60 seconds when the process that supplies the cross-repo count stops.

#### Scenario: Aggregator stops
- **WHEN** the aggregator process stops while a dashboard shows `dashboard ●2`
- **THEN** the dashboard tab name returns to `dashboard` within 60 seconds

### Requirement: Aggregator Process
swamp SHALL run at most one aggregator process for each user. The aggregator SHALL start automatically when a repo session launches or a repo daemon starts, and SHALL exit when no repo daemon has been live for 60 seconds.

#### Scenario: First repo launched
- **WHEN** the user launches the first repo session
- **THEN** an aggregator process runs without a user command

#### Scenario: Second repo launched
- **WHEN** the user launches a second repo session while an aggregator runs
- **THEN** no second aggregator process stays running

#### Scenario: Last daemon stops
- **WHEN** the last repo daemon stops
- **THEN** the aggregator exits within 60 seconds and leaves no socket file

### Requirement: Badge Failure Tolerance
A failure to rename a tab SHALL NOT interrupt daemon operation or change any status that the daemon reports.

#### Scenario: Daemon without a Zellij session
- **WHEN** a daemon runs with no matching Zellij session
- **THEN** the daemon continues to serve clients and reports no error to them

### Requirement: Badge Compatibility With Older Daemons
A daemon that does not support the cross-repo count SHALL continue to operate when the aggregator runs. Its session SHALL show no count badge. Its waiting worktrees SHALL still count toward the badges of other sessions.

#### Scenario: Old daemon among new daemons
- **WHEN** one live daemon predates the count message and a worktree in its repository waits
- **THEN** the other sessions include that worktree in their count
- **AND** the old daemon's session shows a plain `dashboard` tab

### Requirement: Badge Opt-Out
The user SHALL be able to turn off all tab badges in `config.toml`. With badges off, swamp SHALL NOT rename any tab.

#### Scenario: Badges turned off
- **WHEN** the configuration turns badges off and an agent enters `waiting`
- **THEN** no tab name changes

#### Scenario: Default configuration
- **WHEN** the configuration does not mention badges
- **THEN** badges are on
