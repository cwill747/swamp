## 1. PR Summary Data

- [x] 1.1 Add a serde-defaulted comment count field to `PrSummary`.
- [x] 1.2 Extend the existing GitHub GraphQL PR query and parser to populate comment count.
- [x] 1.3 Update or add GitHub parser/model tests for comment count and default decoding behavior.

## 2. Worktrees Pane Rendering

- [x] 2.1 Add compact formatting helpers for failed-build count, comment count, and review status cells.
- [x] 2.2 Add a width-gated expanded worktrees table layout with separate failed-build, comment, and review-status columns.
- [x] 2.3 Preserve the current compact worktrees table layout when the pane is too narrow.
- [x] 2.4 Keep selection, scrolling, current-tab styling, and row hit regions unchanged across both layouts.

## 3. Validation

- [x] 3.1 Add TUI rendering tests or focused unit tests covering expanded and compact worktrees layouts.
- [x] 3.2 Run `cargo test` in the repo dev environment.
- [x] 3.3 Run `openspec status --change "show-ci-status-in-worktrees-pane"` and confirm the change is apply-ready.
