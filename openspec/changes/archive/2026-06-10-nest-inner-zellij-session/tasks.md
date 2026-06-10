## 1. Verify Zellij primitives

- [x] 1.1 Confirm `zellij action switch-session <name> --layout <layout>` creates the session from the layout when it does not exist and switches the calling client to it (Zellij 0.44.3).
- [x] 1.2 Confirm `zellij action current-tab-info` emits a stable tab id and determine its parse format.
- [x] 1.3 Confirm `zellij --session <host> action close-tab-by-id <id>` closes a tab in a *named* session from a process whose client has already switched away.

## 2. Zellij helpers (`src/zellij.rs`)

- [x] 2.1 Add `switch_session(session: &str, layout: Option<&Path>) -> Result<()>` that runs `zellij action switch-session <session>`, appending `--layout <layout>` when `layout` is `Some`.
- [x] 2.2 Add a helper to read the current tab's stable id (parse `zellij action current-tab-info`).
- [x] 2.3 Add `close_tab_by_id_in_session(host: &str, id: …)` that runs `zellij --session <host> action close-tab-by-id <id>` as best-effort (log on failure, never bail).
- [x] 2.4 Add unit tests for any new stdout parsing (tab id), following the `parse_tab_names` test style.

## 3. Nested launch flow (`src/launch.rs`)

- [x] 3.1 In `spawn_new_session`, capture the host session name (`ZELLIJ_SESSION_NAME`) and originating tab id up front when `nested` is true.
- [x] 3.2 Existing-session branch: when `nested`, call `zellij::switch_session(session, None)` instead of `zellij::attach`; keep `attach` for the non-nested case.
- [x] 3.3 New-session branch: when `nested`, call `zellij::switch_session(session, Some(&layout_path))` instead of `new_session_with_layout`; keep `new_session_with_layout` for the non-nested case.
- [x] 3.4 After a nested switch, best-effort close the originating tab via the new helper, guarded so it is skipped when the host session has only one tab (reuse `list_tab_names().len() > 1`, mirroring `relaunch_worktree_tab`).
- [x] 3.5 Update the nested-launch comment block in `run`/`spawn_new_session` to describe the switch-session behavior.

## 4. Verification

- [x] 4.1 `nix build path:.` succeeds.
- [x] 4.2 `nix develop --command cargo fmt --all --check` and `cargo clippy --all-targets --all-features -- -D warnings` are clean.
- [ ] 4.3 Manual: from inside an existing Zellij session with **no** matching repo session, run `swamp` → client switches into the new repo session and the originating tab is closed (host had >1 tab).
- [ ] 4.4 Manual: from inside an existing Zellij session with an **existing** matching repo session, run `swamp` → client switches to it and the originating tab is closed.
- [ ] 4.5 Manual: from a plain terminal (not nested), run `swamp` → unchanged foreground launch/attach behavior.
- [ ] 4.6 Manual: nested launch where the host session has a single tab → switch still happens, originating tab left intact.
