## Context

The worktrees pane is rendered by `src/tui/view/worktrees.rs` and already has access to `app.pr_snapshot.prs` keyed by branch. Today it uses that data for a single compact PR icon column. The dedicated PR status panel in `src/tui/view/pr.rs` renders richer PR and CI information, including check state and review status.

The requested display belongs in the worktrees pane as a responsive enhancement. Narrow dashboard layouts should keep the current compact table readable, while wider panes should surface the CI overview signals directly beside each worktree row.

## Goals / Non-Goals

**Goals:**

- Render separate worktrees pane columns for failed builds, comments, and review status when the pane is wide enough.
- Reuse existing daemon PR snapshot delivery and GitHub refresh flow.
- Add only the PR summary data needed for comments if it is not already available.
- Keep row ordering, selection, scrolling, mouse regions, and keyboard behavior unchanged.

**Non-Goals:**

- Changing the daemon socket framing or adding a new TUI view.
- Adding a new GitHub polling loop.
- Replacing the dedicated PR status pane.
- Showing verbose build names, PR titles, or detailed discussion threads in the worktrees pane.

## Decisions

1. Use a width gate in the worktrees renderer.

   The worktrees table will choose between the current compact column set and an expanded column set based on `area.width` or the bordered inner width. This keeps existing narrow layouts stable. The implementation should use named threshold constants near the renderer so tests can exercise both paths.

   Alternative considered: always add the columns and rely on `Constraint::Min` truncation. That would make narrow panes harder to scan and could squeeze worktree or branch names too aggressively.

2. Keep the expanded columns compact and numeric/icon based.

   The failed-builds column should show the failed count when check state is failure, show a pending/success glyph or blank for non-failure states, and stay short. The comments column should show a compact count when comments exist and blank otherwise. The review column should reuse the existing review icons/colors.

   Alternative considered: render the same combined check text used by the PR panel. That panel includes broader CI state, but the worktrees pane needs the specific three-column overview requested by the user.

3. Extend `PrSummary` with comment count through the existing GitHub query.

   The current model has review decision and latest review information, but no separate comments count. Add a serde-defaulted field such as `comment_count: u32` to keep compatibility with older serialized/test data. Populate it from the existing GraphQL PR query by requesting the relevant pull request comment count. The REST fallback calls the same GraphQL path per branch, so it should inherit the field automatically.

   Alternative considered: infer comments from `ReviewDecision::Commented` or `latestReviews.totalCount`. That conflates review state with discussion volume and would not support a numeric comments column.

4. Share small formatting helpers where practical.

   Existing icon/color behavior in `src/tui/view/pr.rs` can either be made `pub(super)` or mirrored in `worktrees.rs` if the coupling would be awkward. The goal is consistent visual language without introducing a new abstraction layer.

## Risks / Trade-offs

- Width thresholds may feel too conservative or too aggressive. Mitigation: make the cutoff explicit and cover both expanded and compact rendering in tests.
- GitHub comment count fields may differ between issue comments, review comments, and review submissions. Mitigation: document the chosen count in code and tests; prefer a single stable API field for the first implementation.
- Adding a field to `PrSummary` touches the daemon/client serialized data shape. Mitigation: use serde defaults so old or partial payloads decode cleanly.
