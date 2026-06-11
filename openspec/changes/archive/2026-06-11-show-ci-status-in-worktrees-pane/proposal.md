## Why

The worktrees pane is the primary dashboard list, but it currently compresses PR and CI state into a single icon. Users need the same quick CI overview signals there, when space permits, without switching to the dedicated PR status pane.

## What Changes

- Show three optional PR status columns in the worktrees pane when the pane is wide enough:
  - failed build count
  - comment/review discussion signal
  - review decision/status
- Keep the existing compact worktrees layout for narrow panes.
- Reuse the existing PR snapshot data that already feeds the CI overview; no new GitHub queries or external dependencies are introduced.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `daemon-tui-status`: worktrees pane rendering gains responsive PR/CI detail columns when there is room.
- `github-pr-ci-status`: existing PR summary status fields are consumed by the worktrees pane in addition to the PR status panel.

## Impact

- Affected code: `src/tui/view/worktrees.rs`, likely `src/tui/view/pr.rs`, `src/tui/icons.rs`, and related TUI tests.
- Affected specs: `daemon-tui-status`, `github-pr-ci-status`.
- No protocol, persistence, GitHub API, or dependency changes are expected.
