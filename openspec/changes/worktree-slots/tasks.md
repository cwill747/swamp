Each numbered group is one PR. Each PR passes `nix build`, `nix develop --command cargo fmt --all --check`, and `nix develop --command cargo clippy --all-targets --all-features -- -D warnings` before you push it.

## 1. Drop git-wt and add `swamp clone` (PR 1)

- [x] 1.1 Remove git-wt references from comments in `src/worktree/*`, `src/daemon/*`, and `src/tui/state.rs`; verify that `grep -rni "git.wt" src` returns nothing
- [x] 1.2 Add `swamp clone <url> [<dir>]` (bare `.bare`, `.git` pointer file, fetch refspec, default branch worktree); verify with a test that clones a local `file://` repo and asserts the layout and the stdout path
- [x] 1.3 Refuse `swamp clone` into a non-empty dir; verify with a test
- [x] 1.4 Update the README layout and Files sections to describe the swamp layout without git-wt; verify by review

## 2. Slot core (PR 2)

- [ ] 2.1 Add `[slots] pool_size` (default 5) to the user config and the per-repo `<common>/swamp/config.toml`, with the per-repo value taking precedence, and add a commented `[slots]` section to the default `src/config/config.toml`; verify config tests for the default, the override, and 0
- [ ] 2.2 Add a `worktree::slots` module with slot-name parsing and the `default`/`slot`/`overflow` kind; verify unit tests for `work1`, `work12`, `work3-1`, and `workx`
- [ ] 2.3 Add `classify()` (free, busy, pinned) from `HEAD`, `git_info()`, and pin files; verify tests for detached clean, checked-out branch, detached dirty, and pinned
- [ ] 2.4 Add the `flock` helper on `<common>/swamp/slots.lock`; verify a test where two threads allocate concurrently and receive different slots
- [ ] 2.5 Add the checkout into an existing slot dir (safe checkout, ignored files kept); verify a test that an ignored `target/` file survives a reallocation
- [ ] 2.6 Implement alloc (fetch best effort, existing branch reuse, new branch from `origin/<default>`, lowest free slot, lazy slot creation, overflow at the pool size, slots above the pool size skipped, pool size 0, name sanitizing); verify tests for each scenario in spec Slot Allocation
- [ ] 2.7 Add the `-1` suffix for overflow names that match `work<N>`; verify a test
- [ ] 2.8 Extract the shared "branch work is safe" check from `removal_verdict` and use it in removal; verify that the existing removal tests still pass
- [ ] 2.9 Implement release (dirty refusal, pushed check, PR merged or closed, force branch delete, pin clear, overflow no-op); verify tests for each scenario in spec Slot Release, with a stubbed PR state
- [ ] 2.10 Implement pin, unpin, and list (`--json`); verify tests for pin of a free slot, refusal for a busy slot, and the list output with a slot outside the pool
- [ ] 2.11 Add the `swamp slot {alloc,release,pin,unpin,list}` CLI with in-process execution; verify `swamp slot --help` output and an end-to-end test in a temp repo

## 3. Daemon integration and branch memory (PR 3)

- [ ] 3.1 Add slot operations to the daemon socket protocol, and take `flock` inside `repo_ops` for slot, create, and remove operations; verify a test that a CLI alloc and a daemon create do not get the same slot
- [ ] 3.2 Make the CLI forward to the daemon when it answers, and fall back to in-process otherwise; verify tests for both paths
- [ ] 3.3 Add the `branches` map to `.swamp-status.json` with serde defaults; verify that an old status file still decodes
- [ ] 3.4 Move the session fields to branch memory on release and restore them on alloc; verify tests for the scenarios in spec Branch Memory
- [ ] 3.5 Add agent safety on release (refuse when another slot's agent is working); verify a test
- [ ] 3.6 Add the kind, slot state, and pin reason to `WorktreeRow` with serde defaults, and the new sort order; verify snapshot tests
- [ ] 3.7 Route the TUI `c` picker through alloc; verify by a manual run in a scratch repo where `c` fills `work1`

## 4. Lifecycle hooks (PR 4)

- [ ] 4.1 Add `[slots.hooks]` config parsing to the same user and per-repo config files as `pool_size`; verify config tests
- [ ] 4.2 Run `post-alloc` and `pre-release` hooks with the `SWAMP_*` environment, stdout sent to stderr, a timeout, and `--no-hooks`; verify tests for success, failure semantics, and output routing

## 5. Tabs and Claude worktree hooks (PR 5)

- [ ] 5.1 Open or switch to the slot tab on alloc when the repo session exists; verify by a manual run inside Zellij
- [ ] 5.2 Close the slot tab on release, with a detached completion when the release runs inside that tab; verify by a manual run from inside the slot tab
- [ ] 5.3 Verify the `WorktreeCreate` and `WorktreeRemove` stdin JSON contract against the current Claude Code hooks docs, and record the fields in a code comment
- [ ] 5.4 Add `--hook-json` to alloc and release; verify tests that feed sample hook JSON and check stdout
- [ ] 5.5 Add `swamp init --claude-worktrees` with idempotent install and foreign-hook conflict detection; verify tests in `src/config/claude.rs`

## 6. Titles and work-item dashboard (PR 6)

- [ ] 6.1 Add the PR title to `PrSummary` if the query lacks it; verify a GraphQL fixture test
- [ ] 6.2 Implement title resolution (user, PR, session, openspec, commit subject, branch, `free`, pin reason); verify unit tests for each scenario in spec Title Resolution
- [ ] 6.3 Compute the openspec change on head change only; verify a test that it adds no work to a scan with no head change
- [ ] 6.4 Add `swamp title <text>` / `--clear` and a TUI key to set the title; verify tests and a manual TUI run
- [ ] 6.5 Show titles in place of the worktree name, add the label column, and dim free slots; verify view tests for compact and expanded layouts
- [ ] 6.6 Update the README commands and keys tables; verify by review

## 7. Rollout

- [ ] 7.1 Run the full flow in a real repo: `swamp clone`, allocations past the default pool size of 5 (5 slots and 1 overflow), then `pool_size = 2` in the per-repo config, release of a merged branch, and reopen of a pushed branch with session resume; verify each outcome by observation
