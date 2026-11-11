## Purpose

Describe `swamp overview`, the cross-repo TUI that lists the worktrees that need
the user and moves the user to the correct repo session and worktree tab.

## ADDED Requirements

### Requirement: Overview Session
`swamp overview` SHALL open a dedicated Zellij session that contains the overview TUI. The command SHALL create the session when it does not exist and SHALL reuse it when it exists.

#### Scenario: Run outside Zellij
- **WHEN** the user runs `swamp overview` outside any Zellij session
- **THEN** swamp attaches to the overview session, and creates it first when necessary

#### Scenario: Run inside Zellij
- **WHEN** the user runs `swamp overview` inside a Zellij session
- **THEN** swamp switches the client to the overview session
- **AND** swamp does not nest the overview session in the current pane

#### Scenario: Overview session already exists
- **WHEN** the overview session exists and the user runs `swamp overview`
- **THEN** swamp does not start a second overview TUI

### Requirement: Repo Discovery
The overview SHALL discover each live per-repo daemon in the swamp runtime directory without configuration, and SHALL add and remove repositories as daemons start and stop while the overview runs.

#### Scenario: Daemon starts while the overview runs
- **WHEN** a repo daemon starts after the overview has started
- **THEN** the overview shows items from that repository without a restart

#### Scenario: Daemon stops while the overview runs
- **WHEN** a repo daemon stops
- **THEN** the overview removes the items of that repository
- **AND** the overview lists the repository as not running when it is registered

#### Scenario: Stale socket
- **WHEN** a socket file exists but no daemon responds
- **THEN** the overview ignores that socket and stays responsive

### Requirement: Attention Items
The overview SHALL list only worktrees that need the user. A worktree SHALL produce at most one item, at the highest tier that applies:

1. The agent status is `waiting`.
2. The PR checks have failed.
3. The PR review decision is changes requested.
4. The worktree has a conflict or a rebase in progress.
5. The agent status is `idle` and the worktree has staged, unstaged, or untracked changes.

A worktree that meets no tier SHALL NOT appear. The default-branch worktree SHALL NOT produce a tier 5 item. A worktree with a removal in flight SHALL NOT produce an item.

#### Scenario: Agent waits
- **WHEN** a worktree has agent status `waiting`
- **THEN** the overview lists it as a tier 1 item

#### Scenario: Worktree meets two tiers
- **WHEN** a worktree has agent status `waiting` and failed PR checks
- **THEN** the overview lists it one time, as a tier 1 item

#### Scenario: Worktree needs nothing
- **WHEN** a worktree has a working agent, no PR failure, and no conflict
- **THEN** the overview does not list it

#### Scenario: Dirty default branch
- **WHEN** the default-branch worktree is idle and has uncommitted changes
- **THEN** the overview does not list it as a tier 5 item

#### Scenario: Nothing needs the user
- **WHEN** no worktree in any repository meets a tier
- **THEN** the overview shows an empty state that says nothing needs attention

### Requirement: Attention Ordering
The overview SHALL order items by tier, lowest tier number first. Tier 1 items SHALL be ordered by the time the agent entered `waiting`, oldest first. Tier 5 items SHALL be ordered by agent timestamp, newest first. Items in other tiers SHALL be ordered by repository name and then worktree name.

#### Scenario: Two waiting agents
- **WHEN** two worktrees in different repositories have agent status `waiting`
- **THEN** the worktree that entered `waiting` first is listed first

#### Scenario: Waiting outranks CI failure
- **WHEN** one worktree waits and a different worktree has failed PR checks
- **THEN** the waiting worktree is listed first

### Requirement: Item Presentation
Each item SHALL show the repository, the worktree, the reason for attention, and the age of that state when the age is known.

#### Scenario: CI failure item
- **WHEN** an item is listed for failed PR checks
- **THEN** the item shows the repository name, the worktree name, and that checks failed

#### Scenario: Same worktree name in two repos
- **WHEN** two repositories each have a worktree with the same name
- **THEN** the user can tell the two items apart by the repository name

### Requirement: Live Updates
The overview SHALL update its list when any connected daemon broadcasts a change, without user action.

#### Scenario: Agent status changes
- **WHEN** an agent changes from `working` to `waiting`
- **THEN** the overview adds the item without a manual refresh

#### Scenario: Item resolved
- **WHEN** the state that caused an item no longer applies
- **THEN** the overview removes the item without a manual refresh

### Requirement: Jump To Worktree
Activation of an item SHALL move the user to the Zellij session of that repository with the tab of that worktree in focus. When the worktree has no open tab, swamp SHALL open one. Activation SHALL NOT nest the repo session in the overview session.

#### Scenario: Worktree tab is open
- **WHEN** the user activates an item whose worktree tab is open
- **THEN** the client moves to that repo session with that tab in focus

#### Scenario: Worktree tab is not open
- **WHEN** the user activates an item whose worktree has no open tab
- **THEN** swamp opens the worktree tab in the repo session
- **AND** the client moves to that repo session with that tab in focus

#### Scenario: Tab cannot be resolved
- **WHEN** swamp cannot find or open the worktree tab
- **THEN** the client still moves to the repo session
- **AND** the overview does not exit with an error

#### Scenario: Worktree tab has a badge
- **WHEN** the worktree tab name carries an attention badge
- **THEN** activation still focuses that tab

### Requirement: Cold Repos
The overview SHALL list each registered repository that has no live daemon, in a section separate from the attention items. Activation of such a repository SHALL launch its session and move the user to it.

#### Scenario: Registered repo is not running
- **WHEN** a registered repository has no live daemon
- **THEN** the overview lists it as not running

#### Scenario: Activate a cold repo
- **WHEN** the user activates a repository that is not running
- **THEN** swamp launches the session for that repository
- **AND** the client moves to that session

#### Scenario: Cold repo launch fails
- **WHEN** the launch of a cold repository fails
- **THEN** the overview shows the failure and continues to run

### Requirement: Update All
The overview SHALL provide an action that requests a default-branch update from each live daemon. The action SHALL NOT start a repository that is not running. A failure in one repository SHALL NOT stop the update of the others.

#### Scenario: Update all with live repos
- **WHEN** the user starts update-all while three repo daemons are live
- **THEN** each of the three daemons receives a default-branch update request

#### Scenario: One repo fails
- **WHEN** one repository fails its update during update-all
- **THEN** the other repositories still complete their updates
- **AND** the overview names the repository that failed

#### Scenario: Cold repo present
- **WHEN** the user starts update-all while a registered repository is not running
- **THEN** swamp does not start that repository

### Requirement: Older Daemon Compatibility
The overview SHALL show items from a daemon that does not support the identity request. The overview SHALL identify the repository of such a daemon from the worktree paths that the daemon reports, and SHALL use the registry when those paths do not identify it. When swamp cannot identify the repository, the overview SHALL still show the items, SHALL disable jump for them, and SHALL tell the user to restart that session.

#### Scenario: Old daemon, unregistered repo
- **WHEN** a daemon does not support the identity request and its repository is not registered
- **THEN** the overview shows its items with the repository name and jump works

#### Scenario: Old daemon, worktree paths unusable, registered repo
- **WHEN** the reported worktree paths do not identify the repository and the repository is registered
- **THEN** the overview shows its items with the repository name and jump works

#### Scenario: Old daemon cannot be identified
- **WHEN** neither the reported worktree paths nor the registry identify the repository
- **THEN** the overview shows its items without jump
- **AND** the overview shows a hint to restart that session

### Requirement: Overview Input
The overview SHALL support selection movement, item activation, update-all, manual refresh, and quit from the keyboard.

#### Scenario: Navigation
- **WHEN** the user presses the movement keys
- **THEN** the selection moves across attention items and cold repositories

#### Scenario: Quit
- **WHEN** the user quits the overview
- **THEN** the repo sessions and their daemons continue to run
