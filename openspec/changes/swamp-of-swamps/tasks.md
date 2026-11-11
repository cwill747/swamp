Each numbered group from 2 through 5 is one pull request. Before each push, run
`nix build`, `nix develop --command cargo fmt --all --check`, and
`nix develop --command cargo clippy --all-targets --all-features -- -D warnings`.

## 1. Spikes (no code merged)

- [x] 1.1 In a scratch Zellij session, run `zellij action switch-session <S> --tab-position <N>` and confirm that the client lands on tab `N`; record whether the position is 0-based or 1-based in `design.md` D3
- [x] 1.2 From outside a session, run `zellij --session <S> action new-tab --layout <file> --name <n>` and `list-tabs --json`; confirm that the tab opens in `<S>` and record the JSON field names for ID, name, and position in `design.md` D3
- [x] 1.3 From outside a session, run `zellij --session <S> action rename-tab-by-id`, `go-to-tab-by-id`, and `close-tab-by-id`; confirm that each one acts on the correct tab and record the results in `design.md` D7
- [x] 1.4 Rename a tab, detach, reattach, then kill and resurrect the session; record in `design.md` D7 whether the tab name and the tab ID survive each case
- [x] 1.5 Send an unknown `ClientMsg` type to a daemon built from `main` and record the observed reply (error frame or closed connection) in `design.md` D4
- [x] 1.6 If a spike result contradicts a decision, update `design.md` and the affected spec before group 2 starts; verify with `openspec validate swamp-of-swamps --strict`

## 2. PR 1: Daemon identity and repo registry

- [x] 2.1 Add `ClientMsg::GetInfo` and `ServerMsg::Info { common_dir, session_name, version }`; verify with a serde round-trip test and a test that an existing variant still decodes
- [x] 2.2 Handle `GetInfo` in `handle_client` without a subscription; verify with a socket test that sends `GetInfo` then `Subscribe` on one connection and receives `Info` before the snapshots
- [x] 2.3 Make the daemon answer an undecodable request with `ServerMsg::Err` and keep the listener alive; verify with a socket test that sends an unknown type and then a `Ping` from a second client
- [x] 2.4 Add a registry module: path resolution with the `$XDG_STATE_HOME` fallback, load, upsert, remove, and stale-entry filter; verify with unit tests for both path cases, duplicate upsert, symlinked path, and a deleted directory
- [x] 2.5 Make registry writes atomic under an exclusive file lock, and quarantine a malformed file as `repos.json.corrupt`; verify with a test that runs two concurrent writers and a test that loads a malformed file
- [x] 2.6 Call the registry upsert from `swamp launch` on both the create path and the attach path, and log a warning on failure; verify with a test that launch continues when the registry directory is read-only
- [x] 2.7 Add the public `swamp forget [DIR]` command; verify with tests for a registered repo, an unregistered repo, and that no daemon or session is stopped
- [x] 2.8 Add a test that `swamp kill` leaves the registry entry in place
- [x] 2.9 Add daemon identification for one socket: try `GetInfo`, then resolve the git common directory from the `Snapshot` row paths and accept it only when `repo_id` equals the socket file stem; verify with tests against a fake daemon that rejects `GetInfo`, for usable row paths, missing row paths, and a hash mismatch
- [x] 2.10 Add the public `swamp register [DIR]` command and the `--all-live` flag, which identifies each socket in the runtime directory and upserts it; make `DIR` and `--all-live` mutually exclusive; verify with tests for one repo, a non-repo directory, two in-process daemons, one unidentifiable daemon that is skipped and reported, and that no daemon is stopped
- [x] 2.11 Verify by hand that `swamp register --all-live` registers each session that is open today under a daemon built from `main`, and record the output in the PR description
- [x] 2.12 Update the README commands and files tables for `register`, `forget`, and `repos.json`, and regenerate completions; verify that `swamp --help` lists `register` and `forget`

## 3. PR 2: Overview TUI with jump and cold launch

- [x] 3.1 Add the fan-in module: scan the runtime directory for `*.sock` each 2 seconds, connect, send `GetInfo` then `Subscribe`, and drop a connection that fails; verify with a test that starts two in-process daemons and observes both, then stops one and observes its removal
- [x] 3.2 Use the identification from task 2.9 in the fan-in module, and add the registry fallback that matches the socket file stem to `repo_id(common_dir)` of each entry; verify with a test against a fake daemon that rejects `GetInfo`, for usable row paths, for unusable paths with a registered repo, and for an unidentifiable repo
- [x] 3.3 Implement the attention ranking as a pure function over the snapshots of all repos; verify with unit tests for each tier, a worktree in two tiers, the default-branch exclusion, the `deleting` exclusion, and the order inside tier 1 and tier 5
- [x] 3.4 Add tab resolution in `src/zellij.rs`: parse `list-tabs --json` for a given session and return ID, position, and name; verify with a parser test on the JSON captured in task 1.2
- [x] 3.5 Implement jump: resolve the tab, open it in the target session when it is absent, then `switch-session --tab-position`; fall back to a switch without a position on failure; verify by hand in two live repo sessions and record the steps in the PR description
- [x] 3.6 Implement jump outside Zellij with `go-to-tab-by-id` and attach; verify by hand from a plain terminal
- [x] 3.7 Build the overview view: item rows with repo, worktree, reason, and age; a separate section for cold repos; an empty state; the old-daemon hint; verify with render tests on a fixed-size buffer for each state
- [x] 3.8 Add overview input: movement, `Enter`, `r`, and `q`; verify with input tests that selection crosses from items into cold repos and that `q` exits without a daemon request
- [x] 3.9 Implement cold-repo activation: run the launch path for the registered `common_dir`, then switch to the session; show a launch failure in the footer; verify by hand with a killed repo and with a registry entry that points to a broken repo
- [x] 3.10 Add the public `swamp overview` command with the dedicated `swamp-overview` session: create or attach outside Zellij, `switch-session` inside Zellij, and no second TUI when the session exists; verify by hand for all three cases
- [x] 3.11 Add the dashboard TUI key `O` that opens the overview, with a footer message outside Zellij; verify with an input test for the outside-Zellij case and by hand inside Zellij
- [x] 3.12 Update the README for `overview` and the `O` key, and regenerate completions; verify that `swamp --help` lists `overview`

## 4. PR 3: Update all

- [x] 4.1 Add a one-shot `UpdateDefault` client call that the fan-in module can run against each live daemon concurrently and that skips unrelated broadcasts; verify with a test against two in-process daemons where one returns an error
- [x] 4.2 Bind `U` in the overview to update-all and show the success count and the failed repo names in the footer; verify with an input test that cold repos receive no request and no launch
- [x] 4.3 Update the README key table for `U`

## 5. PR 4: Tab identity, badges, and the aggregator

- [x] 5.1 Add the canonical-name functions for worktree tabs and the dashboard tab; verify with unit tests for badged names, plain names, a count of two digits, and a worktree name that starts with the badge glyph
- [x] 5.2 Replace `go_to_tab_name`, `close_tab_by_name`, `list_tab_names`, and `current_tab_name` call sites with lookups by canonical name that act by tab ID; verify that the existing tab tests pass and add a test that a badged tab is found
- [x] 5.3 Verify by hand that open, switch, relaunch, delete, and current-tab delete each target the correct tab when the tab name carries a badge, and record the steps in the PR description
- [x] 5.4 Add `[badges] enabled` to `config.toml` with a default of `true`, and add it to the default config that `swamp init` writes; verify with a config parse test for present, absent, and `false`
- [x] 5.5 Add the daemon badge reconciler: compute the wanted name of each open tab from agent status and the external count, and rename by ID when the name differs; run it on each relevant state change and each 30 seconds; verify with a unit test on the wanted-name function and a test that a rename failure does not fail the daemon
- [x] 5.6 Add `ClientMsg::ExternalAttention { count }`, the in-memory count, and the 60-second expiry; verify with tests for acknowledge, expiry, and absence from `Snapshot`
- [x] 5.7 Add the hidden `swamp aggregate` command: single instance through a non-blocking lock on `aggregator.lock`, fan-in subscription, per-daemon count of waiting worktrees in other repos, send on change and each 20 seconds, stop sending to a daemon that rejects the message, exit after 60 seconds with no live daemon; verify with a test that starts two in-process daemons and checks the count that each receives, and a test that a second instance exits with success
- [x] 5.8 Spawn the aggregator detached from `swamp launch` and from daemon startup, and ignore a spawn failure; verify with a test that launch succeeds when the spawn fails
- [x] 5.9 Verify by hand with two live repo sessions: a `waiting` hook in repo A shows `● <worktree>` in session A and `dashboard ●1` in session B; a `working` hook clears both; killing the aggregator clears the count within 60 seconds; record the steps in the PR description
- [x] 5.10 Verify by hand that a daemon built from `main` stays usable while the new aggregator runs, and that its waiting worktree still counts in a new session
- [x] 5.11 Update the README: the badge description, the `[badges]` config key, and the aggregator lock file in the files table

## 6. Close out

- [x] 6.1 Run `openspec validate swamp-of-swamps --strict` and confirm that it passes
- [x] 6.2 Confirm that each scenario in the five spec deltas maps to an automated test or to a recorded manual check
