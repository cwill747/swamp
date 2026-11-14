# Tasks

## 1. Compatibility Fixtures and Shared Projection

- [x] 1.1 Capture typed pane-list fixtures and exercise list-panes/focus-pane-id on an isolated Zellij 0.45.0 session, including renamed panes and two clients; verify pane ID/tab/role fields and record the supported partial-jump behavior without touching user sessions.
- [x] 1.2 Add the shared workspace destination model over fan-in/registry data; verify all live worktrees, stopped repos, duplicate basenames, unidentified peers, and disconnect/reconnect with deterministic unit fixtures.
- [x] 1.3 Implement stable selection, Spaces ordering, parent/subagent grouping, title fallbacks, trunk markers, and slot reuse invalidation; verify reordering/removal/free-slot/pinned-slot cases without regressions to overview behavior.
- [x] 1.4 Extend fan-in with resource events and structured ownership of scanner/follower tasks per design D3a; verify snapshots arrive when PR/resources never do, receiver drop interrupts a silent socket read, and shutdown cancels and joins every follower without changing legacy consumers.
- [x] 1.5 Add the singleton aggregator workspace broker, versioned framed Unix-socket protocol, per-repo cache/connection generations, and off-loop registry refresh per D3a; verify private endpoint ownership, bounded startup/reconnect, old-daemon compatibility, restart/version mismatch, and exactly one upstream subscription per daemon across multiple UI clients.
- [x] 1.6 Add the workspace client and per-client selected-repo resource interest with explicit loading/unavailable/disconnected state; verify independent selections, rapid selection changes, foreign-repo attribution, stale-generation rejection, no passive activation, and client exit closing only its own tasks/interest.
- [x] 1.7 Implement latest-state coalescing, independent deadline-bounded client writers, and a separate bounded external-attention worker; verify stalled readers, resource bursts, and slow/refused attention requests never block local inventory delivery, pending state stays bounded, and broker shutdown joins all owned tasks.

## 2. Completion and Seen State

- [x] 2.1 Add optional activity/completion identifiers to agent records and snapshot serialization while retaining the union of slot/title/subagent fields; verify old/new snapshot and persisted-record decoding fixtures and unchanged hook input vocabulary.
- [x] 2.2 Implement completion transitions and persistence through daemon restart and slot branch-session capture/restore; verify Working-Waiting-Working-Idle produces one completion, repeated Idle does not, and first-report Idle does not fabricate work.
- [x] 2.3 Add lock-protected atomic seen-ledger updates and bounded stale-entry retention; verify concurrent writers merge, malformed state reports an error without losing live status, and branches moving between slots retain correct acknowledgements.
- [x] 2.4 Derive Waiting/Working/Done/Idle/Unknown and revision-specific acknowledgements; verify successful visits and Mark Seen clear only the captured revision while browsing, native tab navigation, failed focus, and a later completion do not.

## 3. Herdr-style Rendering

- [x] 3.1 Add workspace/sidebar/details CLI modes while preserving explicit legacy modes; verify parser/default/help tests and that standalone use outside Zellij shows target paths without starting nested terminal sessions.
- [x] 3.2 Render the Spaces-over-Agents rail with two-line titles/context, state counts, distinct selection/active markers, subagent indentation, and a single divider; verify 32x30 rendered-buffer fixtures against design D1.
- [x] 3.3 Implement theme tokens, ASCII equivalents, display-width clipping, and control-sequence normalization; verify Unicode-width, long-title, control-character, no-truecolor, and SWAMP_ASCII fixtures.
- [x] 3.4 Render selected-destination details for git, PR/CI/review, harness/subagents, slot/title source, and repository resources; verify trunk PR suppression plus loading/empty/error/disconnected states.
- [x] 3.5 Synchronize sidebar selection and the dashboard details pane through private per-session runtime state with atomic writes and safe failure fallback; verify foreign-repo selection, removed destinations, multiple panes, and two sessions do not target the wrong record.
- [x] 3.6 Implement compact rail, short-height section switch, standalone detail drill-in, and extreme-small fallback; verify 24x16 and sub-16-column rails, 80x24 and 48x16 standalone screens, resize transitions, and no invalid geometry/panics.

## 4. Input, Search, and Safe Actions

- [x] 4.1 Add section navigation, expand/collapse, keyboard selection, single-click activation, independent scroll areas, and contextual help; verify keyboard movement/chevrons/wheel never activate and click regions remain correct after scrolling/resizing.
- [x] 4.2 Add Go To/search across title/repo/branch/path/harness/subagent description and All/Waiting/Working/Done/Idle filters; verify zero results, clear/cancel, retained parent context, and selection stability during broadcasts.
- [x] 4.3 Route navigation and search through explicit input modes; verify typing d/D/K and filter shortcut letters into search cannot trigger mutations, while Esc and field editing remain predictable.
- [x] 4.4 Add unique next/previous attention destinations using existing reasons plus unseen Done; verify priority, multiple-reason deduplication, default-branch idle-dirty exclusion, and empty queues.
- [x] 4.5 Wire create/delete/title/harness/refresh/update/overview/kill through existing handlers with explicit target checks; verify foreign-repo local-only actions are disabled, free slots offer allocation, and force/delete/release confirmations remain authoritative.
- [x] 4.6 Implement navigation-pane return-to-terminal behavior and standalone quit semantics; verify q/closing a sidebar never kills agents or repo sessions and terminal panes receive ordinary keys unchanged.

## 5. Pane-aware Zellij Navigation

- [x] 5.1 Add typed pane discovery and generated-role resolution scoped to the destination tab in src/zellij.rs; verify recorded minimum-version fixtures, terminal/plugin ID distinction, ambiguous/renamed/exited panes, and session liveness checks.
- [x] 5.2 Extend existing jump handling to focus the destination agent pane after safe session/tab activation; verify already-open and newly opened tabs, clientless target attach-before-open, stale IDs, and tab-only fallback without duplicate agents or Done acknowledgement.
- [x] 5.3 Serialize repeated UI activation and reconcile canonical tab identity after creation races; verify rapid Enter/click and competing tab-open paths do not deliberately leave duplicate worktree tabs.
- [x] 5.4 Validate foreign-repo and cold-repo activation with isolated real sessions; verify flat session switching, canonical attention-badged tab lookup, two-client limitations, and no focus change from passive polling or snapshot updates.

## 6. Generated Layouts and Default Rollout

- [x] 6.1 Add dashboard layout/width configuration with workspace/classic choices, 32-cell default and 16-60 validation; verify defaults, partial configs, invalid values, and retained classic percentage settings.
- [x] 6.2 Generate workspace dashboard sidebar/details/real-shell layout with compact stock chrome, stable pane roles, and dashboard sidebar focus; verify KDL fixture shape, exactly one initial dashboard tab, no duplicate tab strip, and no worktree-count-dependent layout.
- [x] 6.3 Generate workspace worktree tabs with the rail, dominant suspended agent, and smaller lazygit/shell region; verify harness selection, Claude resume, managed lazygit config, fish/POSIX quoting, and optional Nix entry fixtures.
- [x] 6.4 Preserve classic layout generation and existing live-session attach policy; verify classic fixtures, mixed old/new tabs, slot no-focus allocation, and no presentation-driven agent/session restart.
- [x] 6.5 Switch newly generated layouts to workspace only after rendering/composition checks pass; verify standalone default, generated default, and explicit classic override agree with CLI/config documentation.

## 7. Acceptance, Documentation, and Spec Integration

- [x] 7.1 Produce deterministic 120x36 and 160x48 composed Zellij captures plus the rail/standalone buffer snapshots in design D1; verify Spaces above Agents, compact hierarchy, meaningful state colors/ASCII labels, agent-dominant terminal space, and absence of the old grid in workspace mode.
- [x] 7.2 Run an isolated end-to-end workflow with two repos, multiple agents, a subagent, busy/free slots, an older daemon fixture, and an unavailable remote; verify waiting/Done visits, selection-details synchronization, safe deletion, cold activation, native fullscreen, and detach/reattach persistence.
- [x] 7.3 Benchmark a fixture with 20 repositories and 200 worktrees across multiple sidebar panes; verify input/rendering stays responsive, idle polling never spawns per-frame git/Zellij work, and pane exit releases background tasks.
- [x] 7.4 Update README keys/screenshots, default config comments, help, and completions for workspace/classic, Go To, shared Done/Mark Seen semantics, native focus/fullscreen controls, subagent parent jumps, and deliberate recreation/rollback; verify documented commands and generated completions match the binary.
- [x] 7.5 Reconcile this change's deltas with the eventual archived swamp-of-swamps/worktree-slots baseline without discarding their snapshot or tab lifecycle requirements; verify all git/slot/title/subagent fields and existing safety scenarios remain represented, then run openspec validate herdr-style-dashboard --strict.
- [x] 7.6 Run nix develop --command cargo test, nix develop --command cargo fmt --all --check, nix develop --command cargo clippy --all-targets --all-features -- -D warnings, and nix build .#dev; verify all pass and record any environment-limited manual checks without claiming unrun tests passed.
