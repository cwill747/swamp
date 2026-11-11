## Why

Each new branch gets its own worktree today. Worktrees pile up on disk, and each new path starts with cold build caches (bazel output bases are keyed by path). A small fixed pool of reusable worktrees ("slots") keeps caches warm and disk use bounded. When dir names stop naming the branch, the dashboard must show what each worktree is doing, so rows need titles.

Swamp already creates, removes, and updates worktrees with `git2`. It only borrows git-wt's layout and naming conventions. Slots break those conventions, so swamp takes full ownership of worktree state and drops git-wt.

## What Changes

- Add a slot pool of reusable worktrees `work1`..`work<N>` under the repository worktree root. `[slots] pool_size` in the config sets `N`, with a default of 5. A value of 0 turns off the pool. Swamp creates slots lazily.
- Git state decides whether a slot is free: detached `HEAD`, a clean tree, and no pin. Swamp keeps no registry of slot ownership.
- Add `swamp slot alloc <name>`: check out a new branch from a fresh `origin/<default>`, or an existing branch, into the first free slot, and print the path. If a worktree already holds the branch, print that path. If all slots in the pool are busy, create a dedicated branch-named overflow worktree.
- Overflow worktrees persist until the user removes them.
- Add `swamp slot release`: detach the slot at the branch tip and keep the dir. Release refuses unless the tree is clean and the work is pushed or its PR is merged or closed. Release deletes the local branch when the PR is merged or closed.
- Add `swamp slot pin` / `unpin` to hold a detached slot busy, and `swamp slot list`.
- Add slot lifecycle hooks (`post-alloc`, `pre-release`) from user config and a per-repo config under the git common dir.
- `swamp init` can install Claude Code `WorktreeCreate` / `WorktreeRemove` hooks that call alloc and release. Agents can allocate and release slots.
- Alloc opens the slot's Zellij tab, and release closes it.
- Swamp saves session metadata and titles per branch on release, and restores them when a slot reopens that branch.
- The dashboard becomes work-item centric. Each row shows a title: user title > PR title > Claude session name > openspec change > last commit subject > branch.
- Add `swamp title` and a TUI key to set a user title.
- Add `swamp clone <url>` to create the bare-repo layout without git-wt.
- Remove git-wt references from code comments, specs, and docs. **BREAKING** for docs only: swamp no longer describes its layout as git-wt-compatible, and users stop using `git wt` against swamp-managed repos.

## Capabilities

### New Capabilities
- `worktree-slots`: The slot pool. Covers free/busy classification, alloc, release, pins, overflow worktrees, lifecycle hooks, locks, and the `swamp slot` CLI.
- `work-titles`: Title resolution for each worktree row from user, PR, session, openspec, and commit sources, and how the user sets a title.

### Modified Capabilities
- `worktree-management`: Naming and creation no longer use git-wt-style terms. Creation adds overflow worktrees. A new requirement adds `swamp clone` for the bare layout.
- `agent-status-config`: Init can install the Claude worktree hooks. Swamp moves agent records into per-branch memory on release, and restores them on alloc.
- `repo-session-launch`: The CLI surface adds `slot`, `title`, and `clone`.
- `daemon-tui-status`: Snapshot rows carry the worktree kind, slot state, and title. Alloc and release open and close tabs. The worktree pane shows titles and dims free slots.

## Impact

- Code: `src/worktree/*` (new slot module, overflow creation, clone), `src/daemon/*` (slot ops through the socket, a shared lock, snapshot fields), `src/hook.rs`, `src/config/*` (slot config, Claude worktree hooks), `src/tui/*` (titles, slot rows, tab lifecycle), `src/cli.rs`, `src/launch/layout.rs`.
- State files: new `<git-common-dir>/swamp/` holds branch memory (`branches.json`), pins, the lock file, and the per-repo config. `.swamp-status.json` keeps its format.
- Compatibility: Existing branch-named worktrees keep working and show as overflow worktrees. Older swamp versions see slots as ordinary worktrees named `work<N>`.
- External: The user's agent rules that tell agents to use `git wt` must change to `swamp slot`. That file is outside this repo.
- No new dependencies.
