## Purpose

Provide a Herdr-style cross-repository workspace dashboard that keeps projects, agents, and attention visible beside real Zellij terminals without replacing their session engine.

## ADDED Requirements

### Requirement: Herdr-style visual hierarchy
The workspace dashboard SHALL present a narrow left rail with Spaces above Agents, compact two-line entries at normal width, a clear active-location marker distinct from keyboard selection, and a compact contextual footer. Generated workspace layouts SHALL pair this rail with a slim tab strip and a larger terminal/detail canvas rather than a grid of independent status panels. Status SHALL remain understandable with ASCII icons and without color.

#### Scenario: Normal workspace rail
- **WHEN** the sidebar is rendered at 32 columns and sufficient height
- **THEN** Spaces appears above Agents with primary titles, muted secondary identity lines, and a single divider toward the canvas
- **AND** selected and currently active destinations are distinguishable

#### Scenario: Theme and ASCII compatibility
- **WHEN** the user enables SWAMP_ASCII or uses a terminal without truecolor
- **THEN** state, selection, headings, and actions remain identifiable without private-use glyphs or color alone
- **AND** the canvas respects the terminal theme rather than requiring a forced background

### Requirement: Complete Spaces inventory
Spaces SHALL show all live repositories and their worktrees, followed by registered repositories that are not running. It SHALL preserve work titles and fallbacks, branch/path identity, slot labels, free/pinned slot semantics, and a distinct default-branch marker. Repositories with the same basename SHALL be disambiguated. Stopped, disconnected, loading, and unidentified repositories SHALL be distinguished from repositories with no work.

#### Scenario: Non-attention work remains reachable
- **WHEN** a live repository has working or idle worktrees with no attention reason
- **THEN** those worktrees still appear under the repository in Spaces

#### Scenario: Slots and trunk
- **WHEN** a repository has an active worktree, a default-branch worktree, busy slots, free slots, and pinned slots
- **THEN** current and trunk identity are retained, idle slots follow occupied work, and free/pinned slots show no active branch or fabricated agent
- **AND** trunk displays no PR, CI, review, or comment badges

#### Scenario: Stopped repository
- **WHEN** a registered repository has no running session
- **THEN** it appears as Not running without starting a daemon or session until explicit activation

#### Scenario: Missing peer identity
- **WHEN** a live daemon cannot be identified safely
- **THEN** its data is labeled with its socket identity and unsafe jump/mutation actions are disabled with an explanatory hint

### Requirement: Cross-repository Agents inventory
Agents SHALL show reported parent agents and indented subagents across live repositories, identifying title, repository, harness when known, and semantic state. Parent groups SHALL be prioritized Waiting, Done, Working, Idle, Unknown with stable ordering within a priority; subagents SHALL stay with their parent. Missing agent reports SHALL NOT be presented as confirmed Idle or Done.

#### Scenario: Waiting agent in another project
- **WHEN** an agent in a different repository waits for input
- **THEN** it appears in the current sidebar's Agents section with a Waiting label and repository identity

#### Scenario: Subagent selection
- **WHEN** the user activates a reported subagent without an independent terminal destination
- **THEN** navigation targets the parent agent's worktree and pane
- **AND** the UI does not claim to open a separate subagent terminal

#### Scenario: Unreported terminal
- **WHEN** a worktree exists without a recorded agent status
- **THEN** it remains in Spaces and is not fabricated as an idle agent in Agents

### Requirement: Reliable Done acknowledgement
The dashboard SHALL display an unseen confirmed completion as Done and SHALL share explicit acknowledgements across local Swamp views. Acknowledgement SHALL apply only to the exact completion revision selected for an explicit successful agent visit or Mark Seen action. Browsing, passive updates, failed jumps, and tab-only partial jumps SHALL NOT acknowledge a completion. A new completion racing with acknowledgement SHALL remain unseen.

#### Scenario: Completion while user works elsewhere
- **WHEN** an active agent reaches a confirmed completed state
- **THEN** its unseen completion appears as Done in local Swamp workspace views until explicitly acknowledged or superseded by a new active turn

#### Scenario: Successful explicit visit
- **WHEN** the user activates a Done agent and the destination agent pane is confirmed focused
- **THEN** that completion becomes seen in other local Swamp views

#### Scenario: Revision changes during navigation
- **WHEN** an activation captures one completion and a later completion arrives before navigation finishes
- **THEN** acknowledgement of the captured revision does not clear the later Done signal

#### Scenario: Native Zellij navigation
- **WHEN** the user switches tabs outside Swamp's navigation controls
- **THEN** Swamp does not claim a completion was acknowledged by that action
- **AND** Mark Seen is available and documented

#### Scenario: Older daemon
- **WHEN** a peer has no completion revision metadata
- **THEN** the dashboard presents its reported working/waiting/idle state without fabricating a Done event

### Requirement: Searchable Go To navigation
The dashboard SHALL provide a Go To picker and search over title, repository, branch, path, harness, and subagent description, with All, Waiting, Working, Done, and Idle filters. Search SHALL preserve parent context, distinguish text editing from navigation shortcuts, and retain selection by destination identity across updates.

#### Scenario: Search across projects
- **WHEN** the user searches for a branch or agent description from another repository
- **THEN** matching destinations appear with enough repository/worktree context to disambiguate them

#### Scenario: Live reorder during selection
- **WHEN** incoming status changes reorder matching rows
- **THEN** the selected destination stays selected if it still exists

#### Scenario: No results
- **WHEN** no destination matches the current query and state filter
- **THEN** the picker shows the active query/filter and an actionable way to clear them

#### Scenario: Typing an action letter
- **WHEN** search has text focus and the user types d, K, or another action key
- **THEN** it edits the query and does not delete a worktree or stop a session

### Requirement: Explicit activation and attention visits
Keyboard movement SHALL select a row without opening it; Enter and a single click on its activation area SHALL activate it. Chevron clicks and wheel scrolling SHALL NOT activate. Next/previous attention SHALL navigate among unique worktree destinations ordered by waiting, failed checks, requested changes, conflict/rebase, unseen Done, and idle-dirty, excluding default-branch idle-dirty. Snapshot updates SHALL never activate a destination or steal focus.

#### Scenario: Browse without switching
- **WHEN** the user moves selection with j/k or arrow keys
- **THEN** details follow the selection while the active Zellij session/tab remains unchanged

#### Scenario: Explicit cross-repository activation
- **WHEN** the user activates a worktree in another live repository
- **THEN** Swamp switches to that repository session, reuses or opens its worktree tab, and focuses its existing agent pane when safely identifiable
- **AND** it does not nest the target session in a new outer workspace

#### Scenario: Free slot activation
- **WHEN** the user activates a free slot
- **THEN** Swamp offers branch allocation without opening a detached-slot agent tab

#### Scenario: Attention destination has multiple reasons
- **WHEN** a waiting worktree also has failed checks and an unseen completion
- **THEN** attention navigation includes that destination once at its highest applicable priority

#### Scenario: Partial navigation
- **WHEN** the target tab opens but its agent pane cannot be resolved safely
- **THEN** Swamp reports tab-only navigation without spawning a duplicate agent or acknowledging Done

### Requirement: Context details and safe actions
The dashboard detail canvas SHALL show the selected destination's title, repo/path, branch, slot when applicable, agent/subagent state, local git summary, PR/CI/review summary, and selected-repository resources when available. Unknown/loading/failed reads SHALL be labeled, not rendered as successful empty data. Actions SHALL use explicit target identity and preserve existing creation, deletion, force-confirmation, release, and session-kill safeguards.

#### Scenario: Sidebar selection updates details
- **WHEN** the user selects another destination in a generated dashboard sidebar
- **THEN** its paired details pane displays that same destination without launching or mutating it

#### Scenario: Network unavailable
- **WHEN** PR or resource data is unavailable
- **THEN** local worktrees and navigation remain usable while the affected detail field reports loading, unavailable, or failure

#### Scenario: Foreign repository mutation
- **WHEN** an action only supports the current repository and a foreign repository row is selected
- **THEN** that action is disabled with a reason and an option to open the selected repository
- **AND** it never mutates the pane's cwd as an implicit substitute

#### Scenario: Untrusted display text
- **WHEN** a branch, title, or reported description contains terminal control characters or excessive width
- **THEN** the UI normalizes and clips display text without interpreting terminal controls or changing action targets

### Requirement: Responsive and terminal-safe interaction
Workspace views SHALL offer compact rows at narrow widths, a switchable single-section presentation at short heights, and a navigable small-terminal fallback without invalid layouts or panics. Swamp shortcuts SHALL apply only while a Swamp navigation surface has focus. Native Zellij terminal input, split, resize, zoom/fullscreen, and detach controls SHALL remain available without a mandatory global prefix remap.

#### Scenario: Narrow standalone workspace
- **WHEN** the standalone workspace is below 60 columns wide
- **THEN** it uses one selected section and details drill-in instead of squeezing an unreadable side-by-side layout

#### Scenario: Narrow rail
- **WHEN** a sidebar is less than 28 columns wide or has less than 10 body rows
- **THEN** it uses compact entries or a Spaces/Agents section switch and retains navigation/search/help access

#### Scenario: Terminal has focus
- **WHEN** the user types j, d, /, or other Swamp navigation keys in the real shell or agent pane
- **THEN** those keys go to that terminal application rather than triggering Swamp actions

#### Scenario: Detach
- **WHEN** the user detaches the Zellij client
- **THEN** terminal processes continue under Zellij without depending on the sidebar client remaining attached
